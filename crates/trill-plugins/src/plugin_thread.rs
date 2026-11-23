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
use std::sync::OnceLock;
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

    fn served(&mut self, away: Duration) {
        self.clap.served(away);
        self.vst3.served(away);
    }
}

/// Every factory in the process drives its plugins from the same thread.
static PLUGIN_THREAD: SharedMainThread<PluginThread> = SharedMainThread::new();

/// A handle to the process's plugin main thread, which is started if it is
/// not running. Fails only if the operating system cannot start a thread.
pub(crate) fn plugin_thread() -> std::io::Result<MainThread<PluginThread>> {
    PLUGIN_THREAD.get("plugin-main")
}

/// The handle that keeps the plugin main thread running once it has been
/// decided that it must not end; see [`keep_for_good`].
static KEPT: OnceLock<MainThread<PluginThread>> = OnceLock::new();

/// Keeps the plugin main thread running, and with it every plugin library
/// it has loaded, for the rest of the process. Called when a plugin first
/// shows an editor.
///
/// Until then the thread ends with its last user, shuts its plugins'
/// libraries down and unloads them, and a later user gets a fresh thread
/// and freshly loaded libraries. A plugin that has shown an editor does
/// not survive either half of that. It has started threads of its own
/// (timers, graphics, an embedded web view) which it does not reliably
/// stop before it reports being shut down, so unloading takes the code
/// away from under them: seen as a crash a moment after the last plugin
/// was let go, about every second time. And its framework has adopted
/// this thread as its user-interface thread for good, so it cannot be
/// handed a new one. A process that is done with its plugins simply
/// exits.
pub(crate) fn keep_for_good(main: &MainThread<PluginThread>) {
    let _ = KEPT.set(main.clone());
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

/// Makes a plugin a step a job, where the plugin thread shares its loop
/// with a window that is to draw between two steps ([`a_step_a_turn`]):
/// `begin` makes what the making keeps, `advance` takes it a step further
/// until the plugin is made, and in between it is kept on the plugin thread
/// (`park`, `unpark`). The whole of it gives up after `timeout`; what was
/// half made then is let go of there, and a plugin made after it is given
/// to `destroy`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_in_steps<M: 'static, T: Send + 'static>(
    main: &MainThread<PluginThread>,
    timeout: Duration,
    path: &std::path::Path,
    begin: impl FnOnce(&mut PluginThread) -> M + Send + 'static,
    park: fn(&mut PluginThread, u64, M),
    unpark: fn(&mut PluginThread, u64) -> Option<M>,
    advance: fn(&mut PluginThread, &mut M) -> Result<Option<T>, String>,
    destroy: fn(&mut PluginThread, T),
) -> Result<T, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let token = NEXT.fetch_add(1, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + timeout;
    // Its file is opened here first, off the thread that draws.
    let opened = beforehand::open(path);
    let forget = |main: &MainThread<PluginThread>| {
        let _ = main.call(Duration::from_secs(1), move |thread| drop(unpark(thread, token)));
    };
    let left = || deadline.saturating_duration_since(std::time::Instant::now());
    if let Err(error) = main.call(left(), move |thread| {
        let making = begin(thread);
        park(thread, token, making)
    }) {
        return Err(error.to_string());
    }
    loop {
        let step = instantiate(
            main,
            left(),
            move |thread| {
                let mut making = unpark(thread, token).ok_or("the plugin's making was let go of")?;
                let made = advance(thread, &mut making)?;
                if made.is_none() {
                    park(thread, token, making);
                }
                Ok(made)
            },
            move |thread, made: Option<T>| {
                if let Some(made) = made {
                    destroy(thread, made);
                }
            },
        );
        match step {
            Ok(Some(made)) => {
                opened.made();
                return Ok(made);
            }
            Ok(None) => {}
            Err(error) => {
                forget(main);
                return Err(error);
            }
        }
    }
}

/// Whether plugins are made a step a job: on macOS where the plugin thread
/// shares its loop with a window ([`crate::first_thread::serve_here`]).
pub(crate) fn a_step_a_turn() -> bool {
    #[cfg(test)]
    if STEPS_IN_THIS_TEST.get() {
        return true;
    }
    #[cfg(target_os = "macos")]
    return crate::main_thread::steps_a_turn();
    #[cfg(not(target_os = "macos"))]
    false
}

#[cfg(test)]
thread_local! {
    /// Has the plugins this test thread asks for made a step a job, as the
    /// window on macOS has them, wherever the test runs.
    pub(crate) static STEPS_IN_THIS_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A plugin's file opened before it is made, on the thread that asks for
/// it: on macOS what a library runs as it is loaded (its static
/// initializers) runs there and not on the thread that draws, and the
/// making finds it loaded. Nothing elsewhere.
mod beforehand {
    use std::path::Path;

    pub(super) struct Opened {
        #[cfg(target_os = "macos")]
        library: Option<libloading::os::unix::Library>,
    }

    impl Opened {
        /// The plugin was made: the library is the making's from here, and
        /// this hold on it is let go of. (Otherwise the hold is kept for
        /// good: a library let go of here would be taken down on a thread
        /// its plugin does not expect.)
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        pub(super) fn made(mut self) {
            #[cfg(target_os = "macos")]
            drop(self.library.take());
        }
    }

    impl Drop for Opened {
        fn drop(&mut self) {
            #[cfg(target_os = "macos")]
            if let Some(library) = self.library.take() {
                std::mem::forget(library);
            }
        }
    }

    /// The library of the plugin at `path`, which on macOS is a bundle.
    #[cfg(target_os = "macos")]
    fn binary(path: &Path) -> Option<std::path::PathBuf> {
        if !path.is_dir() {
            return Some(path.to_path_buf());
        }
        let folder = path.join("Contents").join("MacOS");
        let named = folder.join(path.file_stem()?);
        if named.is_file() {
            return Some(named);
        }
        std::fs::read_dir(&folder).ok()?.flatten().map(|entry| entry.path()).find(|path| path.is_file())
    }

    pub(super) fn open(path: &Path) -> Opened {
        #[cfg(target_os = "macos")]
        {
            use libloading::os::unix::{Library, RTLD_LAZY, RTLD_LOCAL};
            // SAFETY: loading a plugin runs code this crate has no control
            // over, as making it would; it is a library the scanner read.
            let library =
                binary(path).and_then(|binary| unsafe { Library::open(Some(binary), RTLD_LAZY | RTLD_LOCAL) }.ok());
            Opened { library }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = path;
            Opened {}
        }
    }
}
