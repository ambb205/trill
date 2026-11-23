//! VST3's run loop on Linux (`Steinberg::Linux::IRunLoop`), which a view
//! finds on the frame the host gives it: the connections a view wants to
//! hear of and the timers it wants run, served on the plugin main thread.
//!
//! A view on Linux has no loop of the system's to hang its X connection or
//! its timers off; one built on Steinberg's own interface library draws only
//! through these. A connection is watched by the plugin thread's `poll()`
//! ([`crate::main_thread::watch`]), and its handler is called when it is
//! ready; a timer is called as the thread idles, which it does every 15 ms
//! while a VST3 instance exists. What a view leaves registered when its
//! editor closes is let go of then ([`let_go_of`]). Every call comes from
//! the plugin thread, so what is registered is that thread's.

use super::instance::InstanceId;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use vst3::ComPtr;
use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, ITimerHandler, ITimerHandlerTrait, TimerInterval,
};
use vst3::Steinberg::{kInvalidArgument, kResultFalse, kResultOk, tresult};

/// The most of either a view may register: real ones use one or two.
const MOST: usize = 64;

struct Handler {
    owner: InstanceId,
    fd: FileDescriptor,
    handler: ComPtr<IEventHandler>,
}

struct Timer {
    owner: InstanceId,
    period: Duration,
    due: Instant,
    handler: ComPtr<ITimerHandler>,
}

#[derive(Default)]
struct Registered {
    handlers: Vec<Handler>,
    timers: Vec<Timer>,
}

thread_local! {
    static REGISTERED: RefCell<Registered> = RefCell::default();
}

/// Takes a reference of its own to the object at `raw`.
///
/// # Safety
/// `raw` is null or a live interface pointer of the type.
unsafe fn held<I: vst3::Interface>(raw: *mut I) -> Option<ComPtr<I>> {
    // SAFETY: as the caller says.
    unsafe { vst3::ComRef::from_raw(raw) }.map(|reference| reference.to_com_ptr())
}

/// `registerEventHandler`: `handler` is called on the plugin thread when
/// `fd` has something to read.
///
/// # Safety
/// `handler` is null or a live `IEventHandler`.
pub(crate) unsafe fn register_handler(owner: InstanceId, handler: *mut IEventHandler, fd: FileDescriptor) -> tresult {
    // SAFETY: as the caller says.
    let Some(handler) = (unsafe { held(handler) }) else { return kInvalidArgument };
    let full = REGISTERED.with_borrow_mut(|registered| {
        if registered.handlers.len() >= MOST {
            return true;
        }
        registered.handlers.push(Handler { owner, fd, handler });
        false
    });
    if full {
        log::warn!("a VST3 view registered too many connections; refusing another");
        return kResultFalse;
    }
    crate::main_thread::watch(fd, Rc::new(move || ready(fd)));
    kResultOk
}

/// Calls every handler registered for `fd`. Not news for a window of the
/// host's: the view's own.
fn ready(fd: FileDescriptor) -> bool {
    // Taken out first: a handler may register or unregister as it runs.
    let handlers: Vec<ComPtr<IEventHandler>> = REGISTERED.with_borrow(|registered| {
        registered.handlers.iter().filter(|handler| handler.fd == fd).map(|handler| handler.handler.clone()).collect()
    });
    for handler in handlers {
        // SAFETY: a live handler, held by a reference of the host's own.
        unsafe { handler.onFDIsSet(fd) };
    }
    false
}

/// Stops watching the descriptors no handler is registered for any more.
fn unwatch_unheld(fds: Vec<FileDescriptor>) {
    for fd in fds {
        let held = REGISTERED.with_borrow(|registered| registered.handlers.iter().any(|handler| handler.fd == fd));
        if !held {
            crate::main_thread::unwatch(fd);
        }
    }
}

/// `unregisterEventHandler`: every registration of `handler` goes.
pub(crate) fn unregister_handler(handler: *mut IEventHandler) -> tresult {
    let gone: Vec<Handler> = REGISTERED.with_borrow_mut(|registered| {
        let (gone, kept) = registered.handlers.drain(..).partition(|held| held.handler.as_ptr() == handler);
        registered.handlers = kept;
        gone
    });
    if gone.is_empty() {
        return kInvalidArgument;
    }
    unwatch_unheld(gone.iter().map(|handler| handler.fd).collect());
    kResultOk
}

/// `registerTimer`: `handler` is called on the plugin thread every
/// `milliseconds`, as often as the thread idles.
///
/// # Safety
/// `handler` is null or a live `ITimerHandler`.
pub(crate) unsafe fn register_timer(
    owner: InstanceId,
    handler: *mut ITimerHandler,
    milliseconds: TimerInterval,
) -> tresult {
    // SAFETY: as the caller says.
    let Some(handler) = (unsafe { held(handler) }) else { return kInvalidArgument };
    let period = Duration::from_millis(milliseconds.max(1));
    let full = REGISTERED.with_borrow_mut(|registered| {
        if registered.timers.len() >= MOST {
            return true;
        }
        registered.timers.push(Timer { owner, period, due: Instant::now() + period, handler });
        false
    });
    if full {
        log::warn!("a VST3 view registered too many timers; refusing another");
        return kResultFalse;
    }
    kResultOk
}

/// `unregisterTimer`: every registration of `handler` goes.
pub(crate) fn unregister_timer(handler: *mut ITimerHandler) -> tresult {
    let found = REGISTERED.with_borrow_mut(|registered| {
        let before = registered.timers.len();
        registered.timers.retain(|timer| timer.handler.as_ptr() != handler);
        registered.timers.len() != before
    });
    if found { kResultOk } else { kInvalidArgument }
}

/// Calls the timers that are due. On the plugin thread, as it idles.
pub(crate) fn tick() {
    let now = Instant::now();
    let due: Vec<ComPtr<ITimerHandler>> = REGISTERED.with_borrow_mut(|registered| {
        let mut due = Vec::new();
        for timer in registered.timers.iter_mut().filter(|timer| timer.due <= now) {
            // The next one a period after this, or now where it fell behind.
            timer.due = (timer.due + timer.period).max(now);
            due.push(timer.handler.clone());
        }
        due
    });
    for handler in due {
        // SAFETY: a live handler, held by a reference of the host's own.
        unsafe { handler.onTimer() };
    }
}

/// Lets go of whatever the view of `owner` left registered: when its
/// editor has closed, and the view was released.
pub(crate) fn let_go_of(owner: InstanceId) {
    let fds = REGISTERED.with_borrow_mut(|registered| {
        registered.timers.retain(|timer| timer.owner != owner);
        let (gone, kept): (Vec<Handler>, Vec<Handler>) =
            registered.handlers.drain(..).partition(|handler| handler.owner == owner);
        registered.handlers = kept;
        gone.into_iter().map(|handler| handler.fd).collect::<Vec<_>>()
    });
    if !fds.is_empty() {
        log::debug!("a VST3 view left {} connection(s) registered; let go of", fds.len());
    }
    unwatch_unheld(fds);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_thread::plugin_thread;
    use crate::test_support::eventually;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vst3::{Class, ComWrapper};

    const WAIT: Duration = Duration::from_secs(10);

    /// Reads what is there when it is told, as a view reads its connection.
    struct Reader {
        heard: Arc<AtomicUsize>,
    }

    impl Class for Reader {
        type Interfaces = (IEventHandler,);
    }

    impl IEventHandlerTrait for Reader {
        unsafe fn onFDIsSet(&self, fd: FileDescriptor) {
            let mut byte = [0u8; 16];
            // SAFETY: reads into a local of that many bytes.
            unsafe { libc::read(fd, byte.as_mut_ptr().cast(), byte.len()) };
            self.heard.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut ends = [0; 2];
        // SAFETY: fills in two descriptors, then owned.
        assert_eq!(unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) }, 0);
        // SAFETY: both were just made and nothing else owns them.
        unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) }
    }

    #[test]
    fn a_view_hears_of_its_connection_on_the_plugin_thread_until_it_lets_go() {
        let main = plugin_thread().unwrap();
        let (read, write) = pipe();
        let fd = read.as_raw_fd();
        let heard = Arc::new(AtomicUsize::new(0));
        let owner = InstanceId::for_tests(u64::MAX - 3);
        let reader = heard.clone();
        let registered = main
            .call(WAIT, move |_| {
                let handler = ComWrapper::new(Reader { heard: reader }).to_com_ptr::<IEventHandler>().unwrap();
                // SAFETY: a live handler; the run loop takes a reference of its own.
                unsafe { register_handler(owner, handler.as_ptr(), fd) }
            })
            .unwrap();
        assert_eq!(registered, kResultOk);
        let say = || {
            // SAFETY: writes one byte from a local.
            unsafe { libc::write(write.as_raw_fd(), [7u8].as_ptr().cast(), 1) };
        };
        say();
        assert!(eventually(WAIT, || heard.load(Ordering::Relaxed) == 1), "the handler was called on the plugin thread");
        say();
        assert!(eventually(WAIT, || heard.load(Ordering::Relaxed) == 2));
        // Its editor closed: what it left registered goes, and nothing is heard of it any more.
        main.call(WAIT, move |_| let_go_of(owner)).unwrap();
        say();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(heard.load(Ordering::Relaxed), 2);
        drop(read);
    }
}
