//! Which thread a process drives its plugins from.
//!
//! Plugin formats speak of a "main thread", and every call of that kind
//! comes from one thread of this crate's ([`crate::main_thread`]). On
//! Windows and Linux it is a thread of its own. On macOS plugins take the
//! process's first thread for their main thread whatever their host says:
//! their frameworks post their own work to its run loop, and some make no
//! sound until it has run. So there a program says which way it goes:
//!
//! * [`lend`]: the program's work moves to another thread, and the first
//!   thread runs its loop and drives the plugins (the program without a
//!   window, and every process that does a job);
//! * [`serve_here`]: the first thread runs a loop of the program's own (the
//!   window's), and the plugins are driven from a source of that loop, a
//!   step a turn;
//! * neither (a test): the plugins have a thread of their own, which runs
//!   its loop the same way.
//!
//! Whichever it is, the plugin thread refuses a job from itself
//! ([`crate::main_thread::CallError::Reentrant`]): code that runs on the
//! first thread where it drives plugins asks them nothing and waits for
//! nothing of theirs.

/// Moves the program's work (`body`) to a thread of its own, has the
/// process's first thread drive the plugins until that work is done, and
/// then ends the process with the code `body` gave, where it stands: what
/// is run at exit is not, so that a plugin library that is still loaded is
/// not taken down on a thread it does not expect. Called first thing in
/// `main`, on the first thread; called anywhere else it runs `body` there
/// and leaves the same way, the plugins having a thread of their own.
#[cfg(target_os = "macos")]
pub fn lend(body: impl FnOnce() -> i32 + Send + 'static) -> ! {
    crate::main_thread::lend(Box::new(body))
}

/// Has the plugins of this process driven from the first thread's own run
/// loop, which the program runs (a window's). Called on the first thread
/// before any plugin is made; returns whether it was. Plugin work is then
/// done a step a turn of that loop.
#[cfg(target_os = "macos")]
pub fn serve_here() -> bool {
    crate::main_thread::serve_here()
}

/// Says that the window which shares the first thread with the plugins has
/// drawn a frame: plugin work goes on a step a frame, so that the window
/// is drawn between two steps (or a step every 50 ms where it draws
/// nothing). Called on the first thread, at the end of every frame.
#[cfg(target_os = "macos")]
pub fn frame_drawn() {
    crate::main_thread::frame_drawn()
}

/// What a thread that waits for something does meanwhile, where the thing
/// waited for may be held by a thread that waits for a plugin: on the
/// first thread of a window that serves its plugins, one plugin job, and
/// `true`; anywhere else nothing, and `false` (it waits as anyone does).
/// For the session's lock (`trill_engine::SessionLock::serve_while_waiting`).
#[cfg(target_os = "macos")]
pub fn serve_while_waiting() -> bool {
    crate::main_thread::serve_one::<crate::plugin_thread::PluginThread>()
}

/// Whether this process's plugins are driven from its first thread.
pub fn is_first() -> bool {
    #[cfg(target_os = "macos")]
    return crate::main_thread::first_is_lent();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Has a job count as one that hangs after `after` in place of ten seconds,
/// where plugins share their thread with a window: for tests.
#[doc(hidden)]
pub fn refuse_after(after: std::time::Duration) {
    crate::main_thread::hung_after(after)
}

/// Runs `job` on the plugin thread and waits for what it gives, for at most
/// `timeout`. For tests and measurements of where plugin work runs.
#[doc(hidden)]
pub fn on_the_plugin_thread<R: Send + 'static>(
    timeout: std::time::Duration,
    job: impl FnOnce() -> R + Send + 'static,
) -> Option<R> {
    crate::plugin_thread::plugin_thread().ok()?.call(timeout, move |_| job()).ok()
}

/// [`on_the_plugin_thread`] for a job that takes something away from a
/// plugin, which waits until the thread is back in its own loop: for tests
/// of which loops are someone else's.
#[doc(hidden)]
pub fn at_rest_on_the_plugin_thread<R: Send + 'static>(
    timeout: std::time::Duration,
    job: impl FnOnce() -> R + Send + 'static,
) -> Option<R> {
    crate::plugin_thread::plugin_thread().ok()?.call_at_rest(timeout, move |_| job()).ok()
}
