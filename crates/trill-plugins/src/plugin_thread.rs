//! What lives on the plugin main thread: the main-thread half of every
//! plugin format's host, side by side.
//!
//! "The main thread" is one thread per process, not one per format. A
//! plugin framework adopts the thread it is first called on as the UI
//! thread of everything built with it, and the same framework sits under a
//! vendor's CLAP and VST3 plugins alike. So the formats share one thread,
//! and its state is the sum of theirs.

use crate::clap::ClapThread;
use crate::main_thread::{CallError, MainThread, SharedMainThread, ThreadState};
use crate::vst3::Vst3Thread;
use std::time::Duration;

/// The state of the plugin main thread.
pub(crate) struct PluginThread {
    pub clap: ClapThread,
    pub vst3: Vst3Thread,
}

impl ThreadState for PluginThread {
    fn new() -> Self {
        Self { clap: ClapThread::new(), vst3: Vst3Thread::new() }
    }

    fn wants_idle(&self) -> bool {
        self.clap.wants_idle() || self.vst3.wants_idle()
    }

    fn idle(&mut self, nested: bool) {
        self.clap.idle(nested);
        self.vst3.idle(nested);
    }
}

/// Every factory in the process drives its plugins from the same thread.
static PLUGIN_THREAD: SharedMainThread<PluginThread> = SharedMainThread::new();

/// A handle to the process's plugin main thread, which is started if it is
/// not running. Fails only if the operating system cannot start a thread.
pub(crate) fn plugin_thread() -> std::io::Result<MainThread<PluginThread>> {
    PLUGIN_THREAD.get("plugin-main")
}

/// Longest a plugin may take to load, restore its state and activate.
/// Generous: sample-based instruments read their content while loading.
pub(crate) const CREATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Creates a plugin instance on the plugin main thread with `create`,
/// giving up after `timeout`.
///
/// The job cannot be cancelled, so a plugin that takes longer still gets
/// instantiated. Such an instance is not orphaned: the plugin thread hands
/// it over only while this thread is still waiting, and gives it to
/// `destroy` otherwise.
pub(crate) fn instantiate<T: Send + 'static>(
    main: &MainThread<PluginThread>,
    timeout: Duration,
    create: impl FnOnce(&mut PluginThread) -> Result<T, String> + Send + 'static,
    destroy: impl FnOnce(&mut PluginThread, T) + Send + 'static,
) -> Result<T, String> {
    let unclaimed = |thread: &mut PluginThread, created: Result<T, String>| {
        if let Ok(created) = created {
            destroy(thread, created);
        }
    };
    match main.call_claimed(timeout, create, unclaimed) {
        Ok(created) => created,
        Err(CallError::TimedOut) => Err("the plugin took too long to load".to_string()),
        Err(CallError::Failed) => Err("the plugin host failed unexpectedly while loading".to_string()),
        Err(error @ CallError::Reentrant) => Err(error.to_string()),
    }
}
