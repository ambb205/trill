//! The Windows side of the plugin main thread: its message queue.
//!
//! Plugin frameworks deliver their timers and deferred main-thread work
//! through the message queue of the thread they were created on, and a
//! plugin's editor is a window of that thread. So the thread's loop is a
//! Windows message loop: it sleeps until a message, a job or its next idle
//! is due, and dispatches what arrived.
//!
//! Windows also runs message loops of its own on the thread, inside the
//! window procedure of whatever started them: while a window is dragged or
//! resized, while a menu is open, while a dialog is up. The thread's loop
//! does not turn during those, but messages are still dispatched, to
//! windows. So the thread has a hidden window to which every wake-up is
//! posted, and whose procedure serves the job queue when one of those other
//! loops delivers the message there.

use std::cell::Cell;
use std::io;
use std::panic::catch_unwind;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW, INFINITE, SetEvent, SetWaitableTimer,
    TIMER_ALL_ACCESS,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, IsGUIThread, KillTimer, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_NOREMOVE, PM_QS_SENDMESSAGE, PM_REMOVE, PeekMessageW,
    PostMessageW, QS_ALLINPUT, QS_SENDMESSAGE, RegisterClassExW, SetTimer, TranslateMessage, WM_APP, WM_TIMER,
    WNDCLASSEXW,
};

/// Bounds one round of dispatching so a plugin flooding its queue cannot
/// starve jobs.
const MAX_MESSAGES: usize = 256;
/// Posted to the service window to end the thread's wait.
const WAKE: u32 = WM_APP + 0x3A1;
/// The service window's timer, which stands in for the loop's idle
/// interval while the loop is not the one running.
const TICK_TIMER: usize = 1;
const TICK_MILLISECONDS: u32 = super::IDLE_INTERVAL.as_millis() as u32;
/// NUL-terminated UTF-16 for "TrillPluginThread".
const CLASS_NAME: &[u16] = &{
    let ascii = b"TrillPluginThread\0";
    let mut wide = [0u16; 18];
    let mut index = 0;
    while index < ascii.len() {
        wide[index] = ascii[index] as u16;
        index += 1;
    }
    wide
};

thread_local! {
    /// The service window of this thread, if it is a plugin thread.
    static SERVICE_WINDOW: Cell<HWND> = const { Cell::new(null_mut()) };
}

/// The plugin thread's place in the operating system: its hidden service
/// window and what else the thread sets up for the plugins it hosts.
pub(super) struct Pump {
    window: HWND,
    /// A timer that can end a wait more exactly than the system's clock
    /// tick of 15.6 ms; null where Windows is too old to have one.
    alarm: HANDLE,
    /// Whether `OleInitialize` succeeded and is to be undone.
    ole: bool,
    /// Whether the service window's tick timer is running.
    ticking: Cell<bool>,
}

impl Pump {
    /// Must be called on the thread that is to pump.
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: plain calls into user32, ole32 and kernel32 with valid
        // arguments: the class is registered before a window of it is
        // made, strings are NUL-terminated statics, and every handle that
        // is kept is released in `drop`, on this same thread.
        unsafe {
            // Windows made on this thread, the plugins' included, are told
            // the real resolution of their monitor, whatever the rest of
            // the process has chosen. Without this a plugin's editor is
            // drawn small and stretched by the system: blurred.
            if SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_null() {
                SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE);
            }
            // Plugins use the clipboard and drag and drop from their main
            // thread, and expect the host to have set that thread up for
            // them. `S_FALSE` (already initialized) counts as success and
            // is balanced like one.
            let ole = OleInitialize(null()) >= 0;

            let window = CreateWindowExW(
                0,
                service_class(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            if window.is_null() {
                let error = io::Error::last_os_error();
                if ole {
                    OleUninitialize();
                }
                return Err(error);
            }
            SERVICE_WINDOW.set(window);
            let alarm = CreateWaitableTimerExW(null(), null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
            Ok(Self { window, alarm, ole, ticking: Cell::new(false) })
        }
    }

    pub(super) fn waker(&self) -> Waker {
        Waker(self.window as isize)
    }

    /// Sleeps until a message arrives for one of the thread's windows, the
    /// thread is woken, or `idle_in` has passed; then dispatches what there
    /// is. `None` means that nothing is due and only the first two count.
    pub(super) fn wait(&self, idle_in: Option<Duration>) {
        self.tick_while_away(idle_in.is_some());
        // SAFETY: `alarm` is null or a timer handle this pump owns, the
        // due time is a local, and the handle array has the one element
        // the count says. `MWMO_INPUTAVAILABLE` makes the wait see
        // messages that were already queued before it began, which the
        // bounded dispatch below can leave behind.
        unsafe {
            match idle_in {
                None => MsgWaitForMultipleObjectsEx(0, null(), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE),
                Some(Duration::ZERO) => WAIT_TIMEOUT,
                Some(after) => {
                    // Negative: relative, in units of 100 ns.
                    let due = -i64::try_from(after.as_nanos() / 100).unwrap_or(i64::MAX).max(1);
                    if !self.alarm.is_null() && SetWaitableTimer(self.alarm, &due, 0, None, null(), 0) != 0 {
                        MsgWaitForMultipleObjectsEx(1, &self.alarm, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
                    } else {
                        let milliseconds = u32::try_from(after.as_millis()).unwrap_or(INFINITE - 1).max(1);
                        MsgWaitForMultipleObjectsEx(0, null(), milliseconds, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
                    }
                }
            };
        }
        self.dispatch();
    }

    fn dispatch(&self) {
        // SAFETY: `MSG` is a plain C struct for which all-zero bytes are a
        // valid value, and `PeekMessageW` only writes to it. A null window
        // handle selects every message of the calling thread, so nothing
        // owned by another thread is touched. Dispatching runs the window
        // procedures registered on this thread, plugins' among them, which
        // is exactly the main-thread context they expect.
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            for _ in 0..MAX_MESSAGES {
                if PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) == 0 {
                    break;
                }
                // Wake-ups and ticks only had to end the wait: the loop
                // that called this serves the queue itself. Kept from the
                // service window's procedure, they leave it one meaning:
                // "someone else's loop is running".
                if message.hwnd == self.window {
                    continue;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    /// Starts or stops the timer that keeps idle processing going while
    /// the thread is inside a message loop other than its own.
    fn tick_while_away(&self, wanted: bool) {
        if self.ticking.replace(wanted) == wanted {
            return;
        }
        // SAFETY: the window is this pump's, and the timer id its own.
        unsafe {
            if wanted {
                SetTimer(self.window, TICK_TIMER, TICK_MILLISECONDS, None);
            } else {
                KillTimer(self.window, TICK_TIMER);
            }
        }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        SERVICE_WINDOW.set(null_mut());
        // SAFETY: each handle was made by `new` on this thread, is released
        // exactly once, and nothing uses it afterwards: a `Waker` that is
        // still around posts to a window that no longer exists, which
        // fails harmlessly.
        unsafe {
            DestroyWindow(self.window);
            if !self.alarm.is_null() {
                CloseHandle(self.alarm);
            }
            if self.ole {
                OleUninitialize();
            }
        }
    }
}

/// The window class of service windows, registered on first use. A failed
/// registration yields a class that does not exist, and with it a failed
/// window creation, which is reported.
fn service_class() -> *const u16 {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        // SAFETY: the class structure is fully initialized, its strings
        // and its window procedure live for the whole process, and all-zero
        // is valid for the fields left alone.
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(service_procedure),
                hInstance: GetModuleHandleW(null()),
                lpszClassName: CLASS_NAME.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&class);
        }
    });
    CLASS_NAME.as_ptr()
}

/// Only a message loop this module did not start delivers a wake-up or a
/// tick here (see [`Pump::dispatch`]), so arriving here is how the thread
/// learns that it is inside one.
unsafe extern "system" fn service_procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WAKE | WM_TIMER => {
            // Jobs catch their own panics; this keeps anything else from
            // unwinding into the system's code.
            let _ = catch_unwind(super::service_nested);
            0
        }
        // SAFETY: the arguments are passed on exactly as they were given.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// Ends the plugin thread's wait, from any thread.
pub(super) struct Waker(isize);

impl Waker {
    pub(super) fn wake(&self) {
        // SAFETY: posting to a window of another thread is what
        // `PostMessageW` is for; if the window is gone the call fails and
        // nothing happens.
        unsafe { PostMessageW(self.0 as HWND, WAKE, 0, 0) };
    }
}

/// Ends the calling thread's own wait, if it is a plugin thread.
pub(super) fn wake_current() {
    let window = SERVICE_WINDOW.get();
    if !window.is_null() {
        Waker(window as isize).wake();
    }
}

/// An event object, closed when the last of its users lets go.
struct Event(HANDLE);

// SAFETY: an event handle may be used from any thread; signaling it and
// waiting on it are the thread-safe operations the object exists for.
unsafe impl Send for Event {}
// SAFETY: as above.
unsafe impl Sync for Event {}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by `CreateEventW` and is closed
        // once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// How a caller waits for the plugin thread's reply.
///
/// A thread that owns windows must not simply block. Windows delivers some
/// messages by having the sender wait for the receiving thread to take
/// them, and the plugin thread sends such messages to the caller's windows
/// when an editor belongs to one of them (activation and focus pass
/// between the two). A caller deaf to those while it waits for the plugin
/// thread would deadlock with it. So such a caller waits for "the reply is
/// there or a message was sent to me", and lets the messages through.
pub(super) struct Arrival(Option<Arc<Event>>);

impl Arrival {
    pub(super) fn new() -> Self {
        // SAFETY: `IsGUIThread(FALSE)` only asks; the event is created
        // unnamed, manually reset and unsignaled, with default security.
        let event = unsafe {
            // A thread without a message queue can be sent nothing.
            if IsGUIThread(0) == 0 {
                return Self(None);
            }
            CreateEventW(null(), 1, 0, null())
        };
        Self((!event.is_null()).then(|| Arc::new(Event(event))))
    }

    /// What the job takes with it, to say when its reply is on its way.
    pub(super) fn announcer(&self) -> Announcer {
        Announcer(self.0.clone())
    }

    pub(super) fn await_reply<R>(&self, result: &Receiver<R>, timeout: Duration) -> Result<R, RecvTimeoutError> {
        let Some(event) = &self.0 else { return result.recv_timeout(timeout) };
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let milliseconds = u32::try_from(remaining.as_millis()).unwrap_or(INFINITE - 1);
            // SAFETY: the array is the one live handle the count says.
            let woken = unsafe { MsgWaitForMultipleObjectsEx(1, &event.0, milliseconds, QS_SENDMESSAGE, 0) };
            match woken {
                // Announced: the reply follows at once, or never will.
                WAIT_OBJECT_0 | WAIT_FAILED => {
                    return result.recv_timeout(deadline.saturating_duration_since(Instant::now()));
                }
                WAIT_TIMEOUT => return Err(RecvTimeoutError::Timeout),
                _ => {
                    // SAFETY: as in `Pump::dispatch`. Looking for a message
                    // without removing any is what lets the ones other
                    // threads sent run; posted messages stay queued for
                    // the caller's own loop.
                    unsafe {
                        let mut message: MSG = std::mem::zeroed();
                        PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
                    }
                }
            }
        }
    }
}

/// Tells the waiting caller that its job is over when dropped: finished,
/// panicked, or thrown away unrun.
pub(super) struct Announcer(Option<Arc<Event>>);

impl Drop for Announcer {
    fn drop(&mut self) {
        if let Some(event) = &self.0 {
            // SAFETY: the handle is alive for as long as the `Arc` is.
            unsafe { SetEvent(event.0) };
        }
    }
}
