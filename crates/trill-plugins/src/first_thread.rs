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

/// Whether this process's plugins are driven from its first thread.
pub fn is_first() -> bool {
    #[cfg(target_os = "macos")]
    return crate::main_thread::first_is_lent();
    #[cfg(not(target_os = "macos"))]
    false
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
