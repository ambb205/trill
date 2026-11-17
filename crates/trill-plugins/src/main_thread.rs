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
//!
//! # The thread is also a UI thread
//!
//! Plugin editors are windows, and their windows live on this thread. So
//! between jobs the thread runs the operating system's event loop, and that
//! has a consequence a plain job queue does not have: the thread can be
//! inside an event loop this module did not start. Dragging a window by
//! its title bar, a plugin's popup menu and a plugin's file dialog all run
//! one, for as long as the user takes. The queue is served from inside
//! those too (the `os_events` module arranges for that), or every caller
//! would time out while a menu is open. Two things are held back until the
//! thread is back in its own loop:
//!
//! * a job that is already running, which simply finishes first, and
//! * jobs sent with [`MainThread::call_at_rest`]: the ones that take
//!   something away from a plugin (its instance, its editor) that the code
//!   further up the stack may still be using.

use std::any::TypeId;
use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SendError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

#[cfg(windows)]
#[path = "main_thread/windows.rs"]
mod os_events;

#[cfg(not(windows))]
#[path = "main_thread/portable.rs"]
mod os_events;

/// How often an idling thread services plugin callbacks.
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

    /// Whether [`idle`](Self::idle) needs to run periodically. While this
    /// is false the thread sleeps until a job or an OS event arrives, so a
    /// DAW with no plugins loaded pays nothing for it.
    fn wants_idle(&self) -> bool;

    /// Called after every job, and every [`IDLE_INTERVAL`] in between while
    /// [`wants_idle`](Self::wants_idle) is true.
    ///
    /// `nested` says that the thread is inside an event loop it did not
    /// start (see the module documentation), with unknown code further up
    /// its stack. Whatever takes something away from a plugin waits for a
    /// call without it.
    fn idle(&mut self, nested: bool);

    /// Called from the thread's own loop, and never from inside a loop of
    /// someone else's, each time it has just handed the operating system's
    /// events to the thread's windows. `away` is how long the thread had
    /// been kept from them before that: by its jobs and its idling.
    ///
    /// What a plugin posted to itself while the thread was away has
    /// reached it when this is called, and nothing else has run since.
    /// That makes this the place to take from a plugin what such a
    /// message may be about, and `away` the time a plugin that is waiting
    /// to hear from its own work has not had.
    fn served(&mut self, away: Duration) {
        let _ = away;
    }
}

type Job<S> = Box<dyn FnOnce(&mut S) + Send>;

enum Message<S> {
    Run {
        job: Job<S>,
        /// Whether the job has to wait for the thread to be back in its
        /// own loop.
        at_rest: bool,
    },
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

impl From<RecvTimeoutError> for CallError {
    fn from(error: RecvTimeoutError) -> Self {
        match error {
            RecvTimeoutError::Timeout => CallError::TimedOut,
            // The job panicked, or was dropped without being run, and with
            // it its end of the channel.
            RecvTimeoutError::Disconnected => CallError::Failed,
        }
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

/// How long the thread must have had work without a break to count as
/// behind: longer than anything a plugin is asked for in passing.
const BEHIND_AFTER: Duration = Duration::from_millis(250);

/// The work the thread has been given and has not finished: how many jobs,
/// and since when there has been at least one.
#[derive(Default)]
struct Backlog(Mutex<(usize, Option<Instant>)>);

impl Backlog {
    fn began(&self) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 += 1;
        state.1.get_or_insert_with(Instant::now);
    }

    fn ended(&self) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = state.0.saturating_sub(1);
        if state.0 == 0 {
            state.1 = None;
        }
    }

    fn behind(&self) -> bool {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.1.is_some_and(|since| since.elapsed() >= BEHIND_AFTER)
    }
}

/// Takes a job off the backlog when it is done, or was never run.
struct Finished(Arc<Backlog>);

impl Drop for Finished {
    fn drop(&mut self) {
        self.0.ended();
    }
}

struct Inner<S> {
    /// What the thread has yet to do.
    backlog: Arc<Backlog>,
    sender: Sender<Message<S>>,
    /// Ends the thread's wait for OS events when a message was sent.
    waker: os_events::Waker,
    thread_id: ThreadId,
    thread: Option<JoinHandle<()>>,
    exit: Arc<Exit>,
}

impl<S> Drop for Inner<S> {
    fn drop(&mut self) {
        // Every job sent before this one still runs, so plugins are torn
        // down in order before their libraries are unloaded.
        if self.sender.send(Message::Shutdown).is_ok() {
            self.waker.wake();
        }
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
    /// Starts a thread. The process's plugins all share the one that
    /// [`SharedMainThread`] hands out; a thread beside it is for tests of
    /// what a thread does.
    pub(crate) fn spawn(name: &str) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let (ready, started) = mpsc::sync_channel(1);
        let exit = Arc::new(Exit::default());
        let signal = SignalOnDrop(exit.clone());
        let thread = thread::Builder::new().name(name.to_string()).stack_size(STACK_SIZE).spawn(move || {
            // Declared first so that it drops last, after the state.
            let _exit = signal;
            let pump = match os_events::Pump::new() {
                Ok(pump) => pump,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    return;
                }
            };
            let _ = ready.send(Ok(pump.waker()));
            run::<S>(receiver, &pump);
        })?;
        let waker = started
            .recv()
            .map_err(|_| std::io::Error::other("the plugin main thread ended while it was starting"))??;
        let backlog = Arc::new(Backlog::default());
        let inner = Inner { backlog, sender, waker, thread_id: thread.thread().id(), thread: Some(thread), exit };
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Whether the thread is behind: it has had work to do for a while
    /// without a break. A plugin that takes long to be made or let go of
    /// keeps every other plugin waiting for its turn, and one of them may
    /// be waiting to begin loading its samples.
    pub(crate) fn behind(&self) -> bool {
        self.inner.backlog.behind()
    }

    /// Queues `job` without waiting for it.
    fn send(&self, at_rest: bool, job: impl FnOnce(&mut S) + Send + 'static) -> Result<(), CallError> {
        self.inner.backlog.began();
        let finished = Finished(self.inner.backlog.clone());
        let job = move |state: &mut S| {
            // Let go of when the job has run, and if it never does.
            let _finished = finished;
            job(state);
        };
        self.inner.sender.send(Message::Run { job: Box::new(job), at_rest }).map_err(|_| CallError::Failed)?;
        self.inner.waker.wake();
        Ok(())
    }

    /// Runs `job` on the plugin thread and waits up to `timeout` for its
    /// result. A job that times out is not cancelled; its result is dropped
    /// on the plugin thread when it eventually finishes.
    pub(crate) fn call<R: Send + 'static>(
        &self,
        timeout: Duration,
        job: impl FnOnce(&mut S) -> R + Send + 'static,
    ) -> Result<R, CallError> {
        self.call_with(false, timeout, job)
    }

    /// Like [`call`](Self::call), for a job that takes something away from
    /// a plugin: destroying it, deactivating it, closing its editor.
    ///
    /// Such a job only runs once the thread is back in its own loop. While
    /// it is inside someone else's (a plugin's menu or dialog, a window
    /// being dragged), the plugin's own code may be further up the stack,
    /// in the middle of using what the job would remove. The job, and
    /// everything queued behind it, waits for that loop to end.
    pub(crate) fn call_at_rest<R: Send + 'static>(
        &self,
        timeout: Duration,
        job: impl FnOnce(&mut S) -> R + Send + 'static,
    ) -> Result<R, CallError> {
        self.call_with(true, timeout, job)
    }

    fn call_with<R: Send + 'static>(
        &self,
        at_rest: bool,
        timeout: Duration,
        job: impl FnOnce(&mut S) -> R + Send + 'static,
    ) -> Result<R, CallError> {
        if thread::current().id() == self.inner.thread_id {
            return Err(CallError::Reentrant);
        }
        let (reply, result) = mpsc::sync_channel(1);
        let arrival = os_events::Arrival::new();
        let announcer = arrival.announcer();
        self.send(at_rest, move |state| {
            // Dropped, and with that announcing, however this closure ends.
            let announcer = announcer;
            let value = job(state);
            drop(announcer);
            let _ = reply.send(value);
        })?;
        Ok(arrival.await_reply(&result, timeout)?)
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
        let arrival = os_events::Arrival::new();
        let announcer = arrival.announcer();
        self.send(false, move |state| {
            let announcer = announcer;
            let value = job(state);
            drop(announcer);
            if let Err(SendError(value)) = reply.send(value) {
                unclaimed(state, value);
            }
        })?;
        Ok(arrival.await_reply(&result, timeout)?)
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

/// What a plugin thread keeps for as long as it runs.
struct Home<S> {
    /// Behind a `RefCell` because the thread's own stack is the one thing
    /// that can get in its way: a job that is running when an event loop
    /// further down calls back in must find the state taken.
    state: RefCell<S>,
    receiver: Receiver<Message<S>>,
    /// A message that was taken from the queue where it could not be
    /// acted on. It is still the next one.
    held: Cell<Option<Message<S>>>,
    shutdown: Cell<bool>,
    last_idle: Cell<Instant>,
}

impl<S: ThreadState> Home<S> {
    /// Runs the jobs that are waiting, then [`ThreadState::idle`] if it is
    /// due. `nested` is whether this is a call from inside an event loop
    /// the thread did not start.
    fn service(&self, nested: bool) {
        // In use further up this thread's stack. Whoever has it finishes
        // first, and the thread's loop comes back here afterwards.
        let Ok(mut state) = self.state.try_borrow_mut() else { return };
        while let Some(message) = self.next_message() {
            match message {
                Message::Run { at_rest: true, .. } | Message::Shutdown if nested => {
                    self.held.set(Some(message));
                    break;
                }
                Message::Run { job, .. } => {
                    // One bad job must not take every plugin's main thread
                    // away.
                    if catch_unwind(AssertUnwindSafe(|| job(&mut state))).is_err() {
                        log::error!("a job on the plugin main thread panicked");
                    }
                    self.idle(&mut state, nested);
                }
                Message::Shutdown => {
                    self.shutdown.set(true);
                    return;
                }
            }
        }
        let due = self.last_idle.get().elapsed() >= IDLE_INTERVAL;
        if IDLE_REQUESTED.take() || (due && state.wants_idle()) {
            self.idle(&mut state, nested);
        }
    }

    fn next_message(&self) -> Option<Message<S>> {
        self.held.take().or_else(|| match self.receiver.try_recv() {
            Ok(message) => Some(message),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Message::Shutdown),
        })
    }

    fn idle(&self, state: &mut S, nested: bool) {
        self.last_idle.set(Instant::now());
        if catch_unwind(AssertUnwindSafe(|| state.idle(nested))).is_err() {
            log::error!("plugin idle processing panicked");
        }
    }

    /// Tells the state that the thread's windows have just been served.
    fn served(&self, away: Duration) {
        // Nothing is running here: this is the thread's own loop.
        let Ok(mut state) = self.state.try_borrow_mut() else { return };
        if catch_unwind(AssertUnwindSafe(|| state.served(away))).is_err() {
            log::error!("the plugin main thread panicked on its way back from its windows");
        }
    }

    /// How long the thread may sleep before `idle` is due, or `None` if
    /// nothing needs it to wake by itself.
    fn until_idle(&self) -> Option<Duration> {
        let wanted = self.state.try_borrow().map_or(true, |state| state.wants_idle());
        wanted.then(|| IDLE_INTERVAL.saturating_sub(self.last_idle.get().elapsed()))
    }
}

fn run<S: ThreadState>(receiver: Receiver<Message<S>>, pump: &os_events::Pump) {
    let home = Home {
        state: RefCell::new(S::new()),
        receiver,
        held: Cell::new(None),
        shutdown: Cell::new(false),
        last_idle: Cell::new(Instant::now()),
    };
    let _registered = Registered::new(&home);
    let mut left = Instant::now();
    loop {
        home.service(false);
        if home.shutdown.get() {
            break;
        }
        let away = left.elapsed();
        // Dispatches what the operating system has for this thread's
        // windows. That is where an event loop of someone else's may start,
        // and call back into `service`.
        pump.wait(home.until_idle());
        left = Instant::now();
        home.served(away);
    }
    // `home` drops here, and the state with it: on the thread that created
    // it, and while the thread's OS resources (`pump`) still exist.
}

thread_local! {
    /// The home of this thread, while it is a plugin thread in its loop.
    static HOME: Cell<Option<Registration>> = const { Cell::new(None) };
    /// Set by [`request_idle`] until the idle it asks for has run.
    static IDLE_REQUESTED: Cell<bool> = const { Cell::new(false) };
}

/// A [`Home`] with its type erased, for the thread-local it is found by.
#[derive(Copy, Clone)]
struct Registration {
    home: *const (),
    /// The type of state the home holds.
    state: TypeId,
    // Only the Windows layer has event loops of others to notice.
    #[cfg_attr(not(windows), allow(dead_code))]
    service_nested: unsafe fn(*const ()),
}

/// Keeps a home registered as its thread's, and for no longer than it
/// lives.
struct Registered<'a>(PhantomData<&'a ()>);

impl<'a> Registered<'a> {
    fn new<S: ThreadState>(home: &'a Home<S>) -> Self {
        /// # Safety
        ///
        /// `home` must point to a live `Home<S>` of the calling thread.
        unsafe fn service_nested<S: ThreadState>(home: *const ()) {
            // SAFETY: per this function's contract.
            unsafe { &*home.cast::<Home<S>>() }.service(true);
        }
        HOME.set(Some(Registration {
            home: std::ptr::from_ref(home).cast(),
            state: TypeId::of::<S>(),
            service_nested: service_nested::<S>,
        }));
        Self(PhantomData)
    }
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        HOME.set(None);
    }
}

/// Serves the calling thread's queue from inside an event loop that is not
/// the thread's own. For the OS layer, which is what notices being there;
/// so far only the Windows one has such loops to notice.
#[cfg_attr(not(windows), allow(dead_code))]
fn service_nested() {
    if let Some(registration) = HOME.get() {
        // SAFETY: a registration exists only while the home it points to
        // does (`Registered` borrows the home and unregisters when dropped),
        // and it is thread-local, so this is that home's thread. The
        // function stored with the pointer is the one for the home's type.
        unsafe { (registration.service_nested)(registration.home) };
    }
}

/// Runs `body` with the state of the plugin thread, right now, if the
/// caller is on that thread and nothing further up its stack is using the
/// state. Otherwise returns `None` without running it.
///
/// This is for code the operating system calls on the plugin thread, which
/// has no job to hand its work to: a window procedure that needs an answer
/// from a plugin before it returns.
pub(crate) fn with_current<S: ThreadState, R>(body: impl FnOnce(&mut S) -> R) -> Option<R> {
    let registration = HOME.get().filter(|registration| registration.state == TypeId::of::<S>())?;
    // SAFETY: as in `service_nested`; the state's type was just checked.
    let home = unsafe { &*registration.home.cast::<Home<S>>() };
    let mut state = home.state.try_borrow_mut().ok()?;
    Some(body(&mut state))
}

/// Asks for [`ThreadState::idle`] to run as soon as the plugin thread is
/// free, instead of at its next interval. For code the operating system
/// calls on the plugin thread with news that should not wait: a window
/// that was resized or closed.
pub(crate) fn request_idle() {
    IDLE_REQUESTED.set(true);
    os_events::wake_current();
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
        /// The idles that were told they run inside a loop of someone
        /// else's.
        nested_idles: usize,
        busy: bool,
        /// How often the thread said it had served its windows, and how
        /// long it said it had been kept from them, in all.
        served: usize,
        away: Duration,
    }

    impl ThreadState for Counter {
        fn new() -> Self {
            Self {
                thread: thread::current().id(),
                jobs: 0,
                idles: 0,
                nested_idles: 0,
                busy: false,
                served: 0,
                away: Duration::ZERO,
            }
        }

        fn served(&mut self, away: Duration) {
            self.served += 1;
            self.away += away;
        }

        fn wants_idle(&self) -> bool {
            self.busy
        }

        fn idle(&mut self, nested: bool) {
            self.idles += 1;
            self.nested_idles += usize::from(nested);
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
    fn the_state_is_told_when_the_windows_were_served_and_how_long_jobs_kept_the_thread_from_them() {
        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        let before = main.call(WAIT, |state| (state.served, state.away)).unwrap();
        let long = Duration::from_millis(120);
        main.call(WAIT, move |_| thread::sleep(long)).unwrap();
        // The thread goes back to its windows after the job, and says so
        // before it takes the next one.
        let after = main.call(WAIT, |state| (state.served, state.away)).unwrap();
        assert!(after.0 > before.0, "{before:?}, then {after:?}");
        assert!(after.1 - before.1 >= long, "{before:?}, then {after:?}");
        // Waiting is not being kept away.
        thread::sleep(long);
        let rested = main.call(WAIT, |state| state.away).unwrap();
        assert!(rested - after.1 < long, "{:?} more while it was waiting", rested - after.1);
    }

    #[test]
    fn the_thread_is_behind_while_it_has_had_work_for_a_while_and_not_for_work_done_in_passing() {
        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        assert!(!main.behind());
        // Short jobs, one after the other with a rest between: never behind.
        for _ in 0..5 {
            main.call(WAIT, |state| state.jobs += 1).unwrap();
            assert!(!main.behind());
            thread::sleep(Duration::from_millis(20));
        }
        // One that takes its time (a plugin being let go of), and two more
        // behind it: behind until the last of them is done.
        let slow = {
            let main = main.clone();
            thread::spawn(move || main.call(WAIT, |_| thread::sleep(Duration::from_millis(700))).unwrap())
        };
        thread::sleep(Duration::from_millis(100));
        assert!(!main.behind(), "not yet: anything takes a moment");
        let queued: Vec<_> = (0..2)
            .map(|_| {
                let main = main.clone();
                thread::spawn(move || main.call(WAIT, |_| thread::sleep(Duration::from_millis(150))).unwrap())
            })
            .collect();
        thread::sleep(Duration::from_millis(300));
        assert!(main.behind(), "work for 400 ms without a break");
        slow.join().unwrap();
        assert!(main.behind(), "the two behind it are still to do");
        queued.into_iter().for_each(|job| job.join().unwrap());
        assert!(!main.behind(), "and no longer when there is nothing left");
        // A job that is given up on still counts until it has run.
        let given_up = main.call(Duration::from_millis(20), |_| thread::sleep(Duration::from_millis(500)));
        assert!(given_up.is_err());
        thread::sleep(Duration::from_millis(330));
        assert!(main.behind());
        main.call(WAIT, |_| ()).unwrap();
        assert!(!main.behind());
    }

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

        fn idle(&mut self, _nested: bool) {}
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

    #[test]
    fn the_state_is_lent_only_on_its_own_thread_and_only_while_free() {
        assert_eq!(with_current(|state: &mut Counter| state.jobs), None, "this is not a plugin thread");

        let main = MainThread::<Counter>::spawn("test-main").unwrap();
        // A job has the state, so nothing it calls can have it too.
        assert_eq!(main.call(WAIT, |_| with_current(|state: &mut Counter| state.jobs)), Ok(None));
        // Nor is it lent as another type than it is.
        assert_eq!(main.call(WAIT, |_| with_current(|_: &mut Tenant| ()).is_some()), Ok(false));
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use crate::main_thread::os_events::testing;
        use crate::test_support::eventually;
        use std::sync::atomic::AtomicBool;

        /// A flag that says whether a test's foreign loop is running, and
        /// a way to read it that a job can take along.
        fn loop_flag() -> (Arc<AtomicBool>, impl Fn() -> bool + Clone + Send + 'static) {
            let running = Arc::new(AtomicBool::new(false));
            let reader = running.clone();
            (running, move || reader.load(Ordering::SeqCst))
        }

        #[test]
        fn message_handling_is_lent_the_state_and_can_hurry_the_idle() {
            static LENT: AtomicBool = AtomicBool::new(false);
            let main = MainThread::<Counter>::spawn("test-main").unwrap();
            let idles = main.call(WAIT, |state| state.idles).unwrap();
            // What a window procedure does: it runs between jobs, on the
            // thread, and asks for the state.
            main.call(WAIT, |_| {
                testing::from_message_handling(|| {
                    LENT.store(with_current(|state: &mut Counter| state.jobs += 100).is_some(), Ordering::SeqCst);
                    request_idle();
                });
            })
            .unwrap();
            assert!(eventually(WAIT, || LENT.load(Ordering::SeqCst)));
            assert!(main.call(WAIT, |state| state.jobs).unwrap() >= 100);
            // The thread does not idle by itself (`busy` is off): the
            // jobs so far account for three idles, the request for one
            // more, and reading the count comes before its own.
            assert!(eventually(WAIT, || main.call(WAIT, |state| state.idles).unwrap() >= idles + 4));
        }

        #[test]
        fn the_queue_is_served_inside_a_loop_the_thread_did_not_start() {
            let main = MainThread::<Counter>::spawn("test-main").unwrap();
            let (running, in_loop) = loop_flag();
            // A loop of someone else's, started from message handling the
            // way dragging a window starts one.
            main.call(WAIT, move |_| {
                testing::from_message_handling(move || testing::foreign_loop(Duration::from_millis(600), &running));
            })
            .unwrap();
            assert!(eventually(WAIT, &in_loop));

            // An ordinary job does not wait for the loop to end, and the
            // idle after it is told where it is.
            let asked = in_loop.clone();
            let served_inside = main.call(WAIT, move |state| {
                state.jobs += 1;
                asked()
            });
            assert_eq!(served_inside, Ok(true));
            assert!(main.call(WAIT, |state| state.nested_idles).unwrap() >= 1);

            // A job that must find the thread at rest is held back until
            // the loop is over, and what was queued behind it keeps its
            // place.
            let behind = {
                let (main, asked) = (main.clone(), in_loop.clone());
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(50));
                    main.call(WAIT, move |_| asked())
                })
            };
            assert_eq!(main.call_at_rest(WAIT, move |_| in_loop()), Ok(false));
            assert_eq!(behind.join().unwrap(), Ok(false), "a later job overtook one that was held");
        }

        #[test]
        fn a_job_that_runs_a_message_loop_keeps_the_queue_waiting() {
            let main = MainThread::<Counter>::spawn("test-main").unwrap();
            // A plugin that puts up a dialog from inside a call: the job
            // is not over, so nothing else may touch the state.
            let (running, in_loop) = loop_flag();
            let looping = {
                let main = main.clone();
                thread::spawn(move || {
                    main.call(WAIT, move |_| testing::foreign_loop(Duration::from_millis(300), &running))
                })
            };
            assert!(eventually(WAIT, &in_loop));
            assert_eq!(main.call(WAIT, move |_| in_loop()), Ok(false), "a job ran inside another");
            assert_eq!(looping.join().unwrap(), Ok(()));
        }

        #[test]
        fn a_caller_that_owns_windows_answers_them_while_it_waits() {
            let main = MainThread::<Counter>::spawn("test-main").unwrap();
            // The plugin thread sends a message to a window of the thread
            // that is waiting for it, as it does when an editor belongs to
            // that window. A caller that just blocked would never answer.
            let window = testing::echo_window();
            let before = testing::pings();
            let answer = main.call(Duration::from_secs(5), move |_| testing::send(window));
            assert_eq!(answer, Ok(testing::ECHO));
            assert_eq!(testing::pings(), before + 1);

            // The same through the rendezvous of `call_claimed`.
            let claimed = main.call_claimed(Duration::from_secs(5), move |_| testing::send(window), |_, _| ());
            assert_eq!(claimed, Ok(testing::ECHO));
            // A panicking job still fails its caller at once, not at the
            // timeout.
            let started = Instant::now();
            assert_eq!(main.call(WAIT, |_| -> u32 { panic!("deliberate") }), Err(CallError::Failed));
            assert!(started.elapsed() < WAIT / 2);
            testing::destroy(window);
        }
    }
}
