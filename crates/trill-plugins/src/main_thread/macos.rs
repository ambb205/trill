//! The plugin main thread on macOS: served by a source of a thread's run
//! loop, and on the process's first thread wherever a program lends it.
//!
//! Plugins on macOS take the process's first thread to be their main
//! thread whatever their host calls one: their frameworks post their own
//! work to its run loop (JUCE's messages and timers do), and some do not
//! make a sound until that work has run. So a program lends its first
//! thread ([`lend`]), or says that its first thread runs a loop of its own
//! that plugins may be served from ([`serve_here`]); the process's plugin
//! thread is then that thread. Where neither was said (a test), the plugin
//! thread is a thread of its own, run the same way.
//!
//! Either way the thread's [`Home`] is served by a source of its run loop,
//! one job a turn: between two jobs the loop runs whatever else it has,
//! the plugins' own posted work among it, and a window that shares the
//! thread draws. A timer brings the idle round when it is due. A loop that
//! is entered from inside another (a menu, a plugin's dialog) is told
//! apart by an observer of the loop's comings and goings: from there no job
//! that takes something away from a plugin is run, as on Windows.

use super::cf::*;
use super::{Home, Message, SignalOnDrop, ThreadState};
use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

#[cfg(test)]
thread_local! {
    /// How many turns the home on this thread has been given, for tests.
    pub(super) static TURNS: Cell<u64> = const { Cell::new(0) };
}

/// Where the source that serves a home comes among a loop's sources.
const JOBS_AFTER: CFIndex = 1000;

/// The stack the program's own work gets on the thread it is moved to:
/// what the first thread has.
const BODY_STACK: usize = 8 * 1024 * 1024;

// ---- the source that serves a home --------------------------------------

/// Ends a plugin thread's wait, from any thread: signals its source and
/// wakes its loop, both of which may be done from anywhere.
pub(super) struct Waker {
    run_loop: usize,
    source: usize,
}

// SAFETY: the two are CoreFoundation objects retained for this waker, and
// signalling a source and waking a loop are calls made from any thread.
unsafe impl Send for Waker {}
unsafe impl Sync for Waker {}

impl Waker {
    fn new(run_loop: CFRunLoopRef, source: CFRunLoopSourceRef) -> Self {
        // SAFETY: both are live objects; this waker holds a reference each.
        unsafe {
            CFRetain(run_loop.cast_const());
            CFRetain(source.cast_const());
        }
        Self { run_loop: run_loop as usize, source: source as usize }
    }

    pub(super) fn wake(&self) {
        // SAFETY: the objects are retained for as long as this lives; a
        // source that was invalidated takes a signal and does nothing.
        unsafe {
            CFRunLoopSourceSignal(self.source as CFRunLoopSourceRef);
            CFRunLoopWakeUp(self.run_loop as CFRunLoopRef);
        }
    }
}

impl Drop for Waker {
    fn drop(&mut self) {
        // SAFETY: the references taken in `new`.
        unsafe {
            CFRelease(self.source as *const c_void);
            CFRelease(self.run_loop as *const c_void);
        }
    }
}

thread_local! {
    /// The source of the home that is served on this thread, if any.
    static OWN_SOURCE: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    /// How deep in run loops this thread is: 1 in its own, more inside a
    /// loop that was entered from inside another.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
    /// Whether this thread has its observer of loops.
    static OBSERVED: Cell<bool> = const { Cell::new(false) };
    /// What ends the home served on this thread, with what it is given.
    static OWN_END: Cell<Option<Ender>> = const { Cell::new(None) };
}

/// A home's `Driven`, and the function that ends it.
type Ender = (*mut c_void, fn(*mut c_void));

/// Ends the home served on the calling thread, which was told to end, where
/// its loop's turn will not come first (a thread that waits for the
/// session serves it meanwhile).
pub(super) fn end_current() {
    if let Some((info, end)) = OWN_END.take() {
        end(info);
    }
}

/// Whether the calling thread is the process's first.
pub(super) fn is_first_thread() -> bool {
    // SAFETY: asks of the calling thread only.
    unsafe { pthread_main_np() == 1 }
}

/// Whether the calling thread is inside a loop that was entered from
/// inside another (a menu, a plugin's dialog).
pub(super) fn nested() -> bool {
    DEPTH.get() > 1
}

/// Ends the calling thread's own next wait: its home is served at the
/// loop's next turn.
pub(super) fn wake_current() {
    if let Some((run_loop, source)) = OWN_SOURCE.get() {
        // SAFETY: set only while the home's source is live, on its thread.
        unsafe {
            CFRunLoopSourceSignal(source as CFRunLoopSourceRef);
            CFRunLoopWakeUp(run_loop as CFRunLoopRef);
        }
    }
}

/// How a caller waits for the plugin thread's reply: it is never the
/// plugin thread, which has its jobs refused as reentrant, so it just
/// waits.
pub(super) struct Arrival;

impl Arrival {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn announcer(&self) -> Announcer {
        Announcer
    }

    pub(super) fn await_reply<R>(&self, result: &Receiver<R>, timeout: Duration) -> Result<R, RecvTimeoutError> {
        result.recv_timeout(timeout)
    }
}

/// What a job takes with it; see the Windows counterpart.
pub(super) struct Announcer;

/// Counts the loops this thread is in, so that a turn can tell whether it
/// was taken inside someone else's.
fn observe_loops(run_loop: CFRunLoopRef) {
    if OBSERVED.replace(true) {
        return;
    }
    extern "C" fn seen(_observer: CFRunLoopObserverRef, activity: CFOptionFlags, _info: *mut c_void) {
        match activity {
            ENTRY => DEPTH.set(DEPTH.get() + 1),
            EXIT => DEPTH.set(DEPTH.get().saturating_sub(1)),
            _ => {}
        }
    }
    // SAFETY: an observer with no context, kept for as long as the thread
    // lives; the loop holds it from here.
    unsafe {
        let observer = CFRunLoopObserverCreate(
            std::ptr::null(),
            ENTRY | EXIT,
            1,
            // Before anything else that watches the loop come and go.
            isize::MIN,
            seen,
            std::ptr::null_mut(),
        );
        CFRunLoopAddObserver(run_loop, observer, kCFRunLoopCommonModes);
        CFRelease(observer.cast_const());
    }
}

/// A home served by a source of the run loop of the thread it is on.
struct Driven<S> {
    // Field order is drop order: the state goes first, the signal that the
    // thread is done with it last.
    home: Home<S>,
    run_loop: CFRunLoopRef,
    source: CFRunLoopSourceRef,
    timer: CFRunLoopTimerRef,
    /// How long the last turn kept the loop from everything else, to be
    /// told to the state at the next turn taken from the thread's own loop.
    away: Cell<Option<Duration>>,
    /// Whether the home has been let go of.
    ended: std::rc::Rc<Cell<bool>>,
    /// The frame the window had drawn when the last job was taken, and
    /// when that was: where plugins share the thread with a window, a job
    /// waits for the next frame (see [`frame_drawn`]).
    paced: Cell<(u64, Instant)>,
    _exit: SignalOnDrop,
}

impl<S: ThreadState> Driven<S> {
    /// Makes a home on the calling thread and has its loop serve it.
    /// Returns the waker and a flag that is set when the home has ended.
    fn start(receiver: Receiver<Message<S>>, exit: SignalOnDrop) -> (Waker, std::rc::Rc<Cell<bool>>) {
        let ended = std::rc::Rc::new(Cell::new(false));
        let driven = Box::new(Driven {
            home: Home::new(receiver),
            // SAFETY: the calling thread's loop, which outlives what is
            // added to it.
            run_loop: unsafe { CFRunLoopGetCurrent() },
            source: std::ptr::null_mut(),
            timer: std::ptr::null_mut(),
            away: Cell::new(None),
            ended: ended.clone(),
            paced: Cell::new((0, Instant::now())),
            _exit: exit,
        });
        let raw = Box::into_raw(driven);
        // SAFETY: `raw` lives until the home ends, when the source and the
        // timer that point to it are invalidated first. Both are made and
        // added on this thread.
        unsafe {
            let driven = &mut *raw;
            observe_loops(driven.run_loop);
            let mut context = SourceContext {
                version: 0,
                info: raw.cast(),
                retain: None,
                release: None,
                copy_description: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform: Some(perform::<S>),
            };
            // After the sources a plugin adds (theirs are of order 0): what a
            // plugin posted to itself while a job ran is served before the
            // next job, in the same turn of the loop.
            driven.source = CFRunLoopSourceCreate(std::ptr::null(), JOBS_AFTER, &mut context);
            CFRunLoopAddSource(driven.run_loop, driven.source, kCFRunLoopCommonModes);
            let mut context = Context::of(raw.cast());
            // Far off, and brought nearer when the state wants its idle.
            driven.timer = CFRunLoopTimerCreate(std::ptr::null(), f64::MAX / 2.0, 1.0e9, 0, 0, due::<S>, &mut context);
            CFRunLoopAddTimer(driven.run_loop, driven.timer, kCFRunLoopCommonModes);
            driven.home.register();
            OWN_SOURCE.set(Some((driven.run_loop as usize, driven.source as usize)));
            OWN_END.set(Some((raw.cast(), end::<S>)));
            let waker = Waker::new(driven.run_loop, driven.source);
            // Its first turn: whatever was sent before it existed.
            waker.wake();
            (waker, ended)
        }
    }
}

/// The idle timer fired: a turn is due.
extern "C" fn due<S: ThreadState>(_timer: CFRunLoopTimerRef, info: *mut c_void) {
    perform::<S>(info);
}

/// One turn of a home: what the state is owed from the thread's own loop,
/// one job, the idle round if it is due, and another turn asked for if
/// there may be more.
extern "C" fn perform<S: ThreadState>(info: *mut c_void) {
    // SAFETY: `info` is the `Driven` the source and timer were made for,
    // which lives until both are invalidated (below), on this thread.
    let driven = unsafe { &*info.cast::<Driven<S>>() };
    if driven.ended.get() {
        return;
    }
    #[cfg(test)]
    TURNS.set(TURNS.get() + 1);
    let nested = nested();
    if !nested && let Some(away) = driven.away.take() {
        driven.home.served(away);
    }
    let began = Instant::now();
    // Where a window draws on this thread, a frame between two jobs.
    let (frame, last) = driven.paced.get();
    let now_frame = FRAMES.load(Ordering::Relaxed);
    let waits = steps_a_turn() && now_frame == frame && last.elapsed() < FRAME_OR_AFTER;
    let ran = if waits { 0 } else { driven.home.service_some(nested, 1) };
    if ran > 0 {
        driven.paced.set((FRAMES.load(Ordering::Relaxed), Instant::now()));
    }
    if driven.home.shutdown.get() {
        end::<S>(info);
        return;
    }
    if ran > 0 {
        let away = driven.away.take().unwrap_or_default() + began.elapsed();
        driven.away.set(Some(away));
        // There may be more, and the loop turns in between.
        // SAFETY: the home's own source, live until it ends.
        unsafe {
            CFRunLoopSourceSignal(driven.source);
            CFRunLoopWakeUp(driven.run_loop);
        }
    }
    // A job is further up this thread's stack, and a loop runs inside it (a
    // plugin's dialog): nothing can be served until it is over, so the timer
    // looks again an idle's interval later and does not turn the loop at full
    // speed meanwhile.
    let in_use = driven.home.state.try_borrow_mut().is_err();
    let wait = match (driven.home.until_idle(), waits) {
        _ if in_use => Some(super::IDLE_INTERVAL),
        // Waiting for a frame takes no idle (nothing is served meanwhile):
        // what is left of the wait.
        (_, true) => Some(FRAME_OR_AFTER.saturating_sub(last.elapsed())),
        (idle, false) => idle,
    };
    let next = match wait {
        // SAFETY: a plain clock.
        Some(wait) => (unsafe { CFAbsoluteTimeGetCurrent() }) + wait.as_secs_f64(),
        None => f64::MAX / 2.0,
    };
    // SAFETY: the home's own timer, live until it ends.
    unsafe { CFRunLoopTimerSetNextFireDate(driven.timer, next) };
}

/// The home has been told to end: its state goes, on this thread, and the
/// loop no longer serves it.
fn end<S: ThreadState>(info: *mut c_void) {
    // SAFETY: as in `perform`; after this nothing points to the home.
    unsafe {
        let driven = &*info.cast::<Driven<S>>();
        driven.ended.set(true);
        CFRunLoopSourceInvalidate(driven.source);
        CFRunLoopTimerInvalidate(driven.timer);
        CFRelease(driven.source.cast_const());
        CFRelease(driven.timer.cast_const());
        OWN_SOURCE.set(None);
        OWN_END.set(None);
        super::unregister();
        drop(Box::from_raw(info.cast::<Driven<S>>()));
    }
}

/// Starts the home of a plugin thread: on the first thread where it is
/// lent and `first` is asked for, on a thread of its own otherwise.
/// Returns its waker, the thread's id and, for a thread of its own, its
/// handle.
pub(super) fn start<S: ThreadState>(
    name: &str,
    receiver: Receiver<Message<S>>,
    exit: SignalOnDrop,
    first: bool,
) -> io::Result<(Waker, ThreadId, Option<JoinHandle<()>>)> {
    let (ready, started) = mpsc::sync_channel(1);
    let begin = move || {
        let (waker, ended) = Driven::start(receiver, exit);
        let _ = ready.send((waker, thread::current().id()));
        ended
    };
    if first && let Some(host) = HOST.get() {
        // SAFETY: asks of the calling thread only.
        if unsafe { pthread_main_np() } == 1 {
            // Asked on the first thread itself (a window that starts before
            // its loop runs): the home is set up here, and served once the
            // loop runs.
            drop(begin());
            let (waker, id) = started.recv().map_err(|_| io::Error::other("the plugins' home was not set up"))?;
            return Ok((waker, id, None));
        }
        host.post(Box::new(move || drop(begin())));
        let (waker, id) = started.recv().map_err(|_| io::Error::other("the first thread did not take the plugins"))?;
        return Ok((waker, id, None));
    }
    let thread = thread::Builder::new().name(name.to_string()).stack_size(super::STACK_SIZE).spawn(move || {
        let ended = begin();
        while !ended.get() {
            // SAFETY: runs this thread's own loop until its home has ended.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 3600.0, 1) };
        }
    })?;
    let (waker, id) =
        started.recv().map_err(|_| io::Error::other("the plugin main thread ended while it was starting"))?;
    Ok((waker, id, Some(thread)))
}

// ---- the first thread -----------------------------------------------------

/// What runs work handed to the first thread: a source of its loop.
struct Host {
    run_loop: usize,
    source: usize,
    queue: Mutex<VecDeque<Box<dyn FnOnce() + Send>>>,
}

static HOST: OnceLock<Host> = OnceLock::new();

impl Host {
    fn post(&self, work: Box<dyn FnOnce() + Send>) {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner).push_back(work);
        // SAFETY: the host's source and loop live for the rest of the process.
        unsafe {
            CFRunLoopSourceSignal(self.source as CFRunLoopSourceRef);
            CFRunLoopWakeUp(self.run_loop as CFRunLoopRef);
        }
    }
}

extern "C" fn host_perform(_info: *mut c_void) {
    run_posted();
}

/// Runs what was posted to the first thread. Called on it.
pub(super) fn run_posted() {
    let Some(host) = HOST.get() else { return };
    loop {
        let work = host.queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
        match work {
            Some(work) => work(),
            None => break,
        }
    }
}

/// Makes the calling thread, which has to be the first, the one plugins
/// are served on. Whether it is.
fn install_host() -> bool {
    // SAFETY: asks of the calling thread only.
    if unsafe { pthread_main_np() } != 1 {
        return false;
    }
    if HOST.get().is_some() {
        return true;
    }
    // SAFETY: a source made and added on the first thread, kept for the
    // rest of the process.
    unsafe {
        let run_loop = CFRunLoopGetCurrent();
        let mut context = SourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(host_perform),
        };
        let source = CFRunLoopSourceCreate(std::ptr::null(), 0, &mut context);
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        observe_loops(run_loop);
        let _ =
            HOST.set(Host { run_loop: run_loop as usize, source: source as usize, queue: Mutex::new(VecDeque::new()) });
    }
    true
}

/// How many frames the window that shares the first thread has drawn.
static FRAMES: AtomicU64 = AtomicU64::new(0);
/// How long a job waits for the window's next frame at the most: a window
/// that is hidden or covered draws nothing, and plugin work goes on.
const FRAME_OR_AFTER: Duration = Duration::from_millis(50);

/// The window that shares the first thread has drawn a frame: the next
/// plugin job may be taken. Called on that thread.
pub(crate) fn frame_drawn() {
    FRAMES.fetch_add(1, Ordering::Relaxed);
    wake_current();
}

/// Whether the first thread runs a loop of the program's own (a window's)
/// that plugins are served from, a step a turn.
static STEPS_A_TURN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// See [`crate::first_thread::serve_here`].
pub(crate) fn serve_here() -> bool {
    let served = install_host();
    STEPS_A_TURN.store(served, std::sync::atomic::Ordering::Relaxed);
    served
}

/// Whether plugins are made a step a job: see [`STEPS_A_TURN`].
pub(crate) fn steps_a_turn() -> bool {
    STEPS_A_TURN.load(std::sync::atomic::Ordering::Relaxed)
}

/// See [`crate::first_thread::lend`].
pub(crate) fn lend(body: Box<dyn FnOnce() -> i32 + Send>) -> ! {
    if !install_host() {
        // Not the first thread: the plugins have a thread of their own.
        leave(body());
    }
    let code = std::sync::Arc::new(Mutex::new(None));
    let kept = code.clone();
    let done = move |value: i32| {
        *kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
        if let Some(host) = HOST.get() {
            // SAFETY: the first thread's loop, which is waiting for this.
            host.post(Box::new(|| unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) }));
        }
    };
    let started = thread::Builder::new().name("main".into()).stack_size(BODY_STACK).spawn(move || {
        // However the program's work ends, the first thread is told.
        struct Told<F: FnOnce(i32)>(Option<F>);
        impl<F: FnOnce(i32)> Drop for Told<F> {
            fn drop(&mut self) {
                if let Some(done) = self.0.take() {
                    done(101);
                    // The work panicked: the same bound as when it returns.
                    std::thread::sleep(LEAVE_WITHIN);
                    leave(101);
                }
            }
        }
        let mut told = Told(Some(done));
        let value = body();
        if let Some(done) = told.0.take() {
            done(value);
        }
        // The first thread leaves when its loop next turns, which lets what
        // it has in hand finish. A plugin that never comes back from a call
        // there would keep the process for good: after a while this thread
        // leaves by itself, as a job does.
        std::thread::sleep(LEAVE_WITHIN);
        leave(value);
    });
    if let Err(error) = started {
        eprintln!("the program's work could not be given a thread: {error}");
        leave(1);
    }
    loop {
        // SAFETY: the first thread's own loop, run until the program's work
        // is done and stops it. (A stop that lands in a loop a plugin runs
        // stops that one: this one looks again every quarter of a second.)
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0) };
        if let Some(value) = *code.lock().unwrap_or_else(PoisonError::into_inner) {
            leave(value);
        }
    }
}

/// How long the thread that did a program's work waits for the first thread
/// to end the process before it ends it itself.
const LEAVE_WITHIN: Duration = Duration::from_secs(2);

/// Ends the process where it stands: what it wrote is put out, and nothing
/// that is run at exit is (a plugin's library that is still loaded would
/// take itself down on a thread it does not expect).
fn leave(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().lock().flush();
    let _ = std::io::stderr().lock().flush();
    unsafe extern "C" {
        fn _exit(code: i32) -> !;
    }
    // SAFETY: ends the process.
    unsafe { _exit(code) }
}

/// Whether the plugins of this process are served on its first thread.
pub(crate) fn first_is_lent() -> bool {
    HOST.get().is_some()
}
