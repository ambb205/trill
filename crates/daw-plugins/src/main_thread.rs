//! The plugin main thread.
//!
//! Plugin APIs (CLAP, VST3) divide their calls into "audio thread" and "main
//! thread" ones, and require every main-thread call to come from one and the
//! same thread for the life of the process. The DAW has no such thread to
//! offer: a session is driven from the GUI thread, from MCP server workers
//! and from render jobs. So this crate owns one, and everything that must
//! happen on a plugin's main thread is sent to it as a job.
//!
//! The thread's state (loaded libraries, plugin instances) is created on the
//! thread and never leaves it, so it does not need to be `Send`; only the
//! jobs do.
//!
//! "One and the same thread" is a property of the process, not of whoever
//! asks: a plugin library loaded twice is still one library with one idea
//! of its main thread. [`SharedMainThread`] therefore hands every user the
//! same thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SendError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

/// How often an idling thread services plugin callbacks and OS messages.
const IDLE_INTERVAL: Duration = Duration::from_millis(15);
/// Plugins assume a main thread's stack, not the smaller default of a
/// spawned thread (some parse deeply nested state recursively).
const STACK_SIZE: usize = 8 * 1024 * 1024;
/// How long to wait for the thread to unload its plugins and exit before
/// carrying on without it.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// What lives on the plugin thread.
pub(crate) trait ThreadState: 'static {
    /// Called once, on the plugin thread.
    fn new() -> Self;

    /// Whether [`idle`](Self::idle) and the OS message pump need to run
    /// periodically. While this is false the thread sleeps until a job
    /// arrives, so a DAW with no plugins loaded pays nothing for it.
    fn wants_idle(&self) -> bool;

    /// Called after every job, and every [`IDLE_INTERVAL`] in between while
    /// [`wants_idle`](Self::wants_idle) is true.
    fn idle(&mut self);
}

type Job<S> = Box<dyn FnOnce(&mut S) + Send>;

enum Message<S> {
    Run(Job<S>),
    Shutdown,
}

/// Why a [`MainThread::call`] produced no result.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum CallError {
    /// The job did not finish in time. It is still queued or running.
    TimedOut,
    /// The job panicked, or the plugin thread is gone.
    Failed,
    /// Called from the plugin thread itself, which would wait on itself.
    Reentrant,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CallError::TimedOut => "the plugin main thread did not respond in time",
            CallError::Failed => "the plugin host failed unexpectedly",
            CallError::Reentrant => "called from the plugin main thread itself",
        })
    }
}

/// Set once a plugin thread has dropped its state and is about to end.
#[derive(Default)]
struct Exit {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Exit {
    fn signal(&self) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    /// Waits for the signal; returns whether it came within `timeout`.
    fn wait(&self, timeout: Duration) -> bool {
        let done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        let (done, _) =
            self.changed.wait_timeout_while(done, timeout, |done| !*done).unwrap_or_else(PoisonError::into_inner);
        *done
    }
}

/// Signals an [`Exit`] when dropped, so the signal is sent however the
/// thread's function ends.
struct SignalOnDrop(Arc<Exit>);

impl Drop for SignalOnDrop {
    fn drop(&mut self) {
        self.0.signal();
    }
}

struct Inner<S> {
    sender: Sender<Message<S>>,
    thread_id: ThreadId,
    thread: Option<JoinHandle<()>>,
    exit: Arc<Exit>,
}

impl<S> Drop for Inner<S> {
    fn drop(&mut self) {
        // Every job sent before this one still runs, so plugins are torn
        // down in order before their libraries are unloaded.
        let _ = self.sender.send(Message::Shutdown);
        let Some(thread) = self.thread.take() else { return };
        if thread::current().id() == self.thread_id {
            return;
        }
        // Wait for the libraries to unload: a process that exits while a
        // plugin is half-way through its teardown can crash in the plugin's
        // static destructors. But a plugin that hangs must not hang the DAW.
        if self.exit.wait(SHUTDOWN_TIMEOUT) {
            let _ = thread.join();
        } else {
            log::warn!("the plugin main thread did not shut down in time; leaving it behind");
        }
    }
}

/// Handle to the plugin main thread. Cloning is cheap; the thread shuts down
/// when the last handle is dropped.
pub(crate) struct MainThread<S> {
    inner: Arc<Inner<S>>,
}

impl<S> Clone for MainThread<S> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<S: ThreadState> MainThread<S> {
    fn spawn(name: &str) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let exit = Arc::new(Exit::default());
        let signal = SignalOnDrop(exit.clone());
        let thread = thread::Builder::new()
            .name(name.to_string())
            .stack_size(STACK_SIZE)
            .spawn(move || run::<S>(receiver, signal))?;
        let inner = Inner { sender, thread_id: thread.thread().id(), thread: Some(thread), exit };
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Queues `job` without waiting for it.
    pub(crate) fn post(&self, job: impl FnOnce(&mut S) + Send + 'static) -> Result<(), CallError> {
        self.inner.sender.send(Message::Run(Box::new(job))).map_err(|_| CallError::Failed)
    }

    /// Runs `job` on the plugin thread and waits up to `timeout` for its
    /// result. A job that times out is not cancelled; its result is dropped
    /// on the plugin thread when it eventually finishes.
    pub(crate) fn call<R: Send + 'static>(
        &self,
        timeout: Duration,
        job: impl FnOnce(&mut S) -> R + Send + 'static,
    ) -> Result<R, CallError> {
        if thread::current().id() == self.inner.thread_id {
            return Err(CallError::Reentrant);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.post(move |state| {
            let _ = reply.send(job(state));
        })?;
        result.recv_timeout(timeout).map_err(|error| match error {
            RecvTimeoutError::Timeout => CallError::TimedOut,
            // The job panicked and dropped its end of the channel.
            RecvTimeoutError::Disconnected => CallError::Failed,
        })
    }

    /// Like [`call`](Self::call), for a job whose result must not be lost:
    /// something that exists on the plugin thread once the job has made
    /// it, such as a plugin instance.
    ///
    /// A job cannot be cancelled, so one that outlasts `timeout` still
    /// completes. Its result is handed over only while the caller is still
    /// waiting for it; otherwise the plugin thread gives it to `unclaimed`
    /// to dispose of.
    pub(crate) fn call_claimed<R: Send + 'static>(
        &self,
        timeout: Duration,
        job: impl FnOnce(&mut S) -> R + Send + 'static,
        unclaimed: impl FnOnce(&mut S, R) + Send + 'static,
    ) -> Result<R, CallError> {
        if thread::current().id() == self.inner.thread_id {
            return Err(CallError::Reentrant);
        }
        // A rendezvous: sending succeeds only into the hands of a caller
        // that is still there.
        let (reply, result) = mpsc::sync_channel(0);
        self.post(move |state| {
            if let Err(SendError(value)) = reply.send(job(state)) {
                unclaimed(state, value);
            }
        })?;
        result.recv_timeout(timeout).map_err(|error| match error {
            RecvTimeoutError::Timeout => CallError::TimedOut,
            RecvTimeoutError::Disconnected => CallError::Failed,
        })
    }
}

/// The process's plugin main thread: started on first use, shared by every
/// user, and shut down when the last of them lets go. A later user gets a
/// new thread, but never while the previous one is still unloading.
pub(crate) struct SharedMainThread<S> {
    /// The thread most recently started, if it may still be running.
    latest: Mutex<Option<Started<S>>>,
}

/// What is remembered of a thread once its handles are out of our hands.
struct Started<S> {
    /// Dead once the last handle has been dropped.
    thread: Weak<Inner<S>>,
    /// Signals that the thread has really ended, which is a little later.
    exit: Arc<Exit>,
}

impl<S: ThreadState> SharedMainThread<S> {
    pub(crate) const fn new() -> Self {
        Self { latest: Mutex::new(None) }
    }

    /// A handle to the running thread, starting one named `name` if there
    /// is none.
    pub(crate) fn get(&self, name: &str) -> std::io::Result<MainThread<S>> {
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(Started { thread, exit }) = latest.as_ref() {
            if let Some(inner) = thread.upgrade() {
                return Ok(MainThread { inner });
            }
            // Its last handle is gone but it may not have finished yet. Two
            // main threads must never be alive at once, so wait it out.
            if !exit.wait(SHUTDOWN_TIMEOUT) {
                log::warn!("the previous plugin main thread is still shutting down; starting another");
            }
        }
        let thread = MainThread::spawn(name)?;
        *latest = Some(Started { thread: Arc::downgrade(&thread.inner), exit: thread.inner.exit.clone() });
        Ok(thread)
    }
}

/// `_exit` is declared first so that it drops last, after the state.
fn run<S: ThreadState>(receiver: Receiver<Message<S>>, _exit: SignalOnDrop) {
    let mut state = S::new();
    loop {
        let message = if state.wants_idle() {
            match receiver.recv_timeout(IDLE_INTERVAL) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => Some(Message::Shutdown),
            }
        } else {
            Some(receiver.recv().unwrap_or(Message::Shutdown))
        };
        match message {
            Some(Message::Run(job)) => {
                // One bad job must not take every plugin's main thread away.
                if catch_unwind(AssertUnwindSafe(|| job(&mut state))).is_err() {
                    log::error!("a job on the plugin main thread panicked");
                }
            }
            Some(Message::Shutdown) => break,
            None => {}
        }
        if catch_unwind(AssertUnwindSafe(|| state.idle())).is_err() {
            log::error!("plugin idle processing panicked");
        }
        os_events::pump();
    }
    // `state` drops here, on the thread that created it.
}

/// Plugin frameworks deliver their timers and deferred main-thread work
/// through the operating system's event queue of the thread they were
/// created on. A host main thread is expected to drain it.
#[cfg(windows)]
mod os_events {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    /// Bounds one pump so a plugin flooding its queue cannot starve jobs.
    const MAX_MESSAGES: usize = 256;

    pub(super) fn pump() {
        // SAFETY: `MSG` is a plain C struct for which all-zero bytes are a
        // valid value, and `PeekMessageW` only writes to it. A null window
        // handle selects every message of the calling thread, so nothing
        // owned by another thread is touched. Dispatching runs the window
        // procedures plugins registered on this thread, which is exactly the
        // main-thread context they expect.
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            for _ in 0..MAX_MESSAGES {
                if PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) == 0 {
                    break;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

/// On macOS a run loop only works on the process's first thread, and on
/// Linux plugins bring their own event loop or use CLAP's timer and fd
/// extensions. Neither is needed until plugin editors are hosted.
#[cfg(not(windows))]
mod os_events {
    pub(super) fn pump() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

    static DROPS: AtomicUsize = AtomicUsize::new(0);

    struct Counter {
        thread: ThreadId,
        jobs: usize,
        idles: usize,
        busy: bool,
    }

    impl ThreadState for Counter {
        fn new() -> Self {
            Self { thread: thread::current().id(), jobs: 0, idles: 0, busy: false }
        }

        fn wants_idle(&self) -> bool {
            self.busy
        }

        fn idle(&mut self) {
            self.idles += 1;
        }
    }

    impl Drop for Counter {
        fn drop(&mut self) {
            assert_eq!(thread::current().id(), self.thread, "state must drop on its own thread");
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    const WAIT: Duration = Duration::from_secs(10);

    #[test]
    fn jobs_run_in_order_on_one_thread_and_state_drops_there() {
        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        let home = main.call(WAIT, |state| state.thread).unwrap();
        assert_ne!(home, thread::current().id());

        let workers: Vec<_> = (0..4)
            .map(|_| {
                let main = main.clone();
                thread::spawn(move || {
                    for _ in 0..50 {
                        let thread = main
                            .call(WAIT, |state| {
                                state.jobs += 1;
                                thread::current().id()
                            })
                            .unwrap();
                        assert_eq!(thread, home);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(main.call(WAIT, |state| state.jobs).unwrap(), 200);

        let before = DROPS.load(Ordering::SeqCst);
        drop(main);
        assert!(DROPS.load(Ordering::SeqCst) > before, "dropping the last handle joins the thread");
    }

    /// State that counts how many of itself are alive.
    struct Tenant;

    static TENANTS: AtomicIsize = AtomicIsize::new(0);
    static MOST_TENANTS: AtomicIsize = AtomicIsize::new(0);

    impl ThreadState for Tenant {
        fn new() -> Self {
            let alive = TENANTS.fetch_add(1, Ordering::SeqCst) + 1;
            MOST_TENANTS.fetch_max(alive, Ordering::SeqCst);
            Self
        }

        fn wants_idle(&self) -> bool {
            false
        }

        fn idle(&mut self) {}
    }

    impl Drop for Tenant {
        fn drop(&mut self) {
            // Unloading plugins takes a while; a successor must wait for it.
            thread::sleep(Duration::from_millis(30));
            TENANTS.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn the_shared_thread_is_one_thread_at_a_time() {
        static SHARED: SharedMainThread<Tenant> = SharedMainThread::new();
        let id = |main: &MainThread<Tenant>| main.call(WAIT, |_| thread::current().id()).unwrap();

        let first = SHARED.get("test-shared").unwrap();
        let second = SHARED.get("test-shared").unwrap();
        assert_eq!(id(&first), id(&second), "users share the running thread");
        drop(first);
        assert_eq!(TENANTS.load(Ordering::SeqCst), 1, "the thread lives while any handle does");

        // Let go on another thread while this one already asks again: the
        // new thread must not start until the old one has unloaded.
        let releasing = thread::spawn(move || drop(second));
        for _ in 0..5 {
            let again = SHARED.get("test-shared").unwrap();
            assert_eq!(main_call_count(&again), 1);
        }
        releasing.join().unwrap();
        assert_eq!(MOST_TENANTS.load(Ordering::SeqCst), 1, "two main threads were alive at once");
        assert_eq!(TENANTS.load(Ordering::SeqCst), 0);
    }

    /// Runs one job and reports how many states were alive during it.
    fn main_call_count(main: &MainThread<Tenant>) -> isize {
        main.call(WAIT, |_| TENANTS.load(Ordering::SeqCst)).unwrap()
    }

    #[test]
    fn a_panicking_job_fails_its_caller_but_not_the_thread() {
        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        let result = main.call(WAIT, |_| -> u32 { panic!("deliberate") });
        assert_eq!(result, Err(CallError::Failed));
        assert_eq!(main.call(WAIT, |_| 7), Ok(7));
    }

    #[test]
    fn a_slow_job_times_out_without_blocking_the_caller() {
        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        let result = main.call(Duration::from_millis(20), |_| thread::sleep(Duration::from_millis(300)));
        assert_eq!(result, Err(CallError::TimedOut));
        // The thread recovers once the slow job is done.
        assert_eq!(main.call(WAIT, |_| 1), Ok(1));
    }

    #[test]
    fn calling_from_the_plugin_thread_is_refused() {
        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        let inner = main.clone();
        let nested = main.call(WAIT, move |_| inner.call(WAIT, |_| ())).unwrap();
        assert_eq!(nested, Err(CallError::Reentrant));
        let inner = main.clone();
        let nested = main.call(WAIT, move |_| inner.call_claimed(WAIT, |_| (), |_, ()| ())).unwrap();
        assert_eq!(nested, Err(CallError::Reentrant));
    }

    #[test]
    fn a_result_nobody_waited_for_is_disposed_of_on_the_plugin_thread() {
        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        // Claimed: the caller gets the result and nothing is disposed of.
        let claimed = main.call_claimed(WAIT, |state| state.jobs + 40, |state, _| state.idles = usize::MAX);
        assert_eq!(claimed, Ok(40));

        // Unclaimed: the caller has given up by the time the job is done.
        let slow = main.call_claimed(
            Duration::from_millis(20),
            |_| {
                thread::sleep(Duration::from_millis(300));
                7
            },
            |state, made| state.jobs = made,
        );
        assert_eq!(slow, Err(CallError::TimedOut));
        // Queued behind the slow job, this sees what became of its result.
        assert_eq!(main.call(WAIT, |state| state.jobs), Ok(7));
        assert_ne!(main.call(WAIT, |state| state.idles), Ok(usize::MAX), "a claimed result is not disposed of");

        let panicked = main.call_claimed(WAIT, |_| -> u32 { panic!("deliberate") }, |_, _| ());
        assert_eq!(panicked, Err(CallError::Failed));
    }

    #[test]
    fn idle_runs_periodically_only_while_wanted() {
        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        thread::sleep(IDLE_INTERVAL * 4);
        // Once after the job that reads it, and never in between.
        assert_eq!(main.call(WAIT, |state| state.idles).unwrap(), 0);

        main.call(WAIT, |state| state.busy = true).unwrap();
        thread::sleep(IDLE_INTERVAL * 8);
        let idles = main.call(WAIT, |state| state.idles).unwrap();
        assert!(idles >= 4, "expected periodic idling, got {idles}");
    }
}
