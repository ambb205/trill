//! The host side of the CLAP ABI: what a plugin may ask of the DAW.
//!
//! The host is deliberately static. A device's parameter list, ports and
//! latency are read once when it is created and reported to the engine
//! then; the device API has no way to change them afterwards. So requests
//! that amount to "my shape changed, look again" are declined or noted, and
//! take effect the next time the device is created.
//!
//! What a plugin asks for while its editor is open is different: timers,
//! a window of another size, a flush of the parameter changes its editor
//! made. Those are served, by the plugin main thread when it next idles.
//! A request can come from any thread, so making one only sets a flag.

use super::timers::Timers;
use crate::editor::Size;
use crate::main_thread::request_idle;
use clack_extensions::audio_ports::{AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl};
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortRescanFlags};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, TimerId};
use clack_host::prelude::*;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

thread_local! {
    /// True while this thread is inside one of a plugin's audio-thread
    /// functions. CLAP's "audio thread" is a role, not a fixed thread: the
    /// engine's realtime thread plays it, and so does whichever thread runs
    /// an offline render.
    static IN_AUDIO_CONTEXT: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as a plugin's audio thread until dropped.
pub(crate) struct AudioContext {
    previous: bool,
}

impl AudioContext {
    pub(crate) fn enter() -> Self {
        Self { previous: IN_AUDIO_CONTEXT.replace(true) }
    }
}

impl Drop for AudioContext {
    fn drop(&mut self) {
        IN_AUDIO_CONTEXT.set(self.previous);
    }
}

fn in_audio_context() -> bool {
    IN_AUDIO_CONTEXT.get()
}

/// Ties the handler types together for clack. Never instantiated.
pub(crate) struct TrillHost;

impl HostHandlers for TrillHost {
    type Shared<'a> = HostShared;
    type MainThread<'a> = HostMainThread<'a>;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostThreadCheck>()
            .register::<HostParams>()
            .register::<HostState>()
            .register::<HostAudioPorts>()
            .register::<HostNotePorts>()
            .register::<HostLatency>()
            .register::<HostGui>()
            .register::<HostTimer>();
    }
}

/// Callbacks a plugin may make from any thread, the audio thread included.
/// Nothing here may block or allocate; requests are recorded in flags that
/// the plugin main thread picks up when it next idles.
pub(crate) struct HostShared {
    /// For log messages.
    plugin_id: String,
    main_thread: ThreadId,
    callback_requested: AtomicBool,
    restart_requested: AtomicBool,
    /// Set while the plugin waits for parameter changes to be exchanged
    /// with it: by its next block, or by a flush if none is coming.
    flush_requested: AtomicBool,
    /// Whether the plugin's editor is open, which is when its requests
    /// about the editor's window mean something.
    editor_open: AtomicBool,
    /// The size the plugin asked for its editor's window to be given, as
    /// `GuiSize` packs it. Zero: none.
    editor_resize: AtomicU64,
    /// The plugin reported that its editor is gone.
    editor_lost: AtomicBool,
    /// The plugin changed its mind about how its editor may be resized.
    editor_hints_changed: AtomicBool,
}

/// What a plugin asked about its editor since it was last looked at.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditorRequests {
    /// The size it wants its window to be.
    pub resize: Option<Size>,
    /// Its editor is gone and must be closed on the host's side too.
    pub lost: bool,
    /// Whether the user may resize the editor has changed.
    pub hints_changed: bool,
}

impl HostShared {
    /// Must be called on the plugin main thread.
    pub(crate) fn new(plugin_id: &str) -> Self {
        Self {
            plugin_id: plugin_id.to_string(),
            main_thread: thread::current().id(),
            callback_requested: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            flush_requested: AtomicBool::new(false),
            editor_open: AtomicBool::new(false),
            editor_resize: AtomicU64::new(0),
            editor_lost: AtomicBool::new(false),
            editor_hints_changed: AtomicBool::new(false),
        }
    }

    /// Whether the plugin asked for its `on_main_thread` callback since the
    /// last time this was called.
    pub(crate) fn take_callback_request(&self) -> bool {
        self.callback_requested.swap(false, Ordering::AcqRel)
    }

    /// Whether the plugin asked to be restarted since the last call.
    pub(crate) fn take_restart_request(&self) -> bool {
        self.restart_requested.swap(false, Ordering::AcqRel)
    }

    /// Whether the plugin is waiting for a flush.
    pub(crate) fn flush_requested(&self) -> bool {
        self.flush_requested.load(Ordering::Acquire)
    }

    /// Records that parameter changes were just exchanged with the plugin,
    /// which is all a flush is. Realtime-safe: every block does this.
    pub(crate) fn flush_served(&self) {
        self.flush_requested.store(false, Ordering::Release);
    }

    /// Whether the calling thread is the plugin main thread, whatever it
    /// is doing there. This is about which thread may touch what belongs
    /// to that thread; what CLAP calls "the main thread" is a role, which
    /// [`HostThreadCheckImpl::is_main_thread`] answers for.
    fn on_host_thread(&self) -> bool {
        thread::current().id() == self.main_thread
    }

    /// Has the plugin main thread act on a request without waiting for its
    /// next interval, if this is that thread; from any other, the interval
    /// has to do.
    fn hurry(&self) {
        if self.on_host_thread() {
            request_idle();
        }
    }

    /// Opens or closes the books on the plugin's editor. Closing them
    /// drops what the plugin asked about an editor that no longer exists.
    pub(crate) fn set_editor_open(&self, open: bool) {
        self.editor_open.store(open, Ordering::Release);
        if !open {
            self.take_editor_requests();
        }
    }

    /// What the plugin asked about its editor since the last call.
    pub(crate) fn take_editor_requests(&self) -> EditorRequests {
        let size = GuiSize::unpack_from_u64(self.editor_resize.swap(0, Ordering::AcqRel));
        EditorRequests {
            resize: (size.width != 0 && size.height != 0).then(|| Size::new(size.width, size.height)),
            lost: self.editor_lost.swap(false, Ordering::AcqRel),
            hints_changed: self.editor_hints_changed.swap(false, Ordering::AcqRel),
        }
    }
}

impl SharedHandler<'_> for HostShared {
    fn request_restart(&self) {
        self.restart_requested.store(true, Ordering::Release);
    }

    fn request_process(&self) {
        // The engine processes every device on every block; plugins are
        // never put to sleep, so there is nothing to wake.
    }

    fn request_callback(&self) {
        self.callback_requested.store(true, Ordering::Release);
    }
}

impl HostLogImpl for HostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        // A logger may allocate, lock or write to a file, none of which is
        // allowed on the audio thread. A message a plugin sends from there
        // is dropped rather than risking a dropout.
        if in_audio_context() {
            return;
        }
        let level = match severity {
            LogSeverity::Debug => log::Level::Debug,
            LogSeverity::Info => log::Level::Info,
            LogSeverity::Warning | LogSeverity::HostMisbehaving | LogSeverity::PluginMisbehaving => log::Level::Warn,
            LogSeverity::Error | LogSeverity::Fatal => log::Level::Error,
        };
        log::log!(level, "[{}] {message}", self.plugin_id);
    }
}

impl HostThreadCheckImpl for HostShared {
    /// Like the audio thread, the main thread is a role: the plugin main
    /// thread plays the audio thread while it flushes parameter changes
    /// for a plugin nobody is processing, and is not "main" meanwhile.
    fn is_main_thread(&self) -> bool {
        self.on_host_thread() && !in_audio_context()
    }

    fn is_audio_thread(&self) -> bool {
        in_audio_context()
    }
}

impl HostParamsImplShared for HostShared {
    /// Parameter events travel with `process`, which normally runs on
    /// every block and answers this by itself. The request is kept for the
    /// plugin main thread to notice when no block comes: when the device
    /// is switched off, or nothing is driving the engine.
    fn request_flush(&self) {
        self.flush_requested.store(true, Ordering::Release);
    }
}

impl HostGuiImpl for HostShared {
    fn resize_hints_changed(&self) {
        self.editor_hints_changed.store(true, Ordering::Release);
        self.hurry();
    }

    /// Accepted whenever the editor is open: the window is resized when
    /// the plugin main thread next idles. Should the system not allow the
    /// size, the plugin is told the one it got instead.
    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        if !self.editor_open.load(Ordering::Acquire) {
            return Err(HostError::Message("the editor is not open"));
        }
        if new_size.width == 0 || new_size.height == 0 {
            return Err(HostError::Message("an editor cannot be empty"));
        }
        self.editor_resize.store(new_size.pack_to_u64(), Ordering::Release);
        self.hurry();
        Ok(())
    }

    /// Declined: an editor opens when the user asks the DAW for it, and
    /// the device API has no way for a device to ask.
    fn request_show(&self) -> Result<(), HostError> {
        if !in_audio_context() {
            log::info!("[{}] asked for its editor to be shown, which this host leaves to the user", self.plugin_id);
        }
        Err(HostError::Message("editors are opened by the user"))
    }

    /// Declined, like [`request_show`](Self::request_show): the user
    /// closes the window.
    fn request_hide(&self) -> Result<(), HostError> {
        if !in_audio_context() {
            log::info!("[{}] asked for its editor to be hidden, which this host leaves to the user", self.plugin_id);
        }
        Err(HostError::Message("editors are closed by the user"))
    }

    fn closed(&self, _was_destroyed: bool) {
        self.editor_lost.store(true, Ordering::Release);
        self.hurry();
    }
}

/// Callbacks a plugin makes on its main thread, and what they need kept:
/// the plugin's timers. See the module documentation for why most of the
/// other requests change nothing.
pub(crate) struct HostMainThread<'a> {
    shared: &'a HostShared,
    timers: RefCell<Timers>,
}

impl<'a> HostMainThread<'a> {
    pub(crate) fn new(shared: &'a HostShared) -> Self {
        Self { shared, timers: RefCell::new(Timers::default()) }
    }

    /// Appends the plugin's timers that are due at `now` to `due`.
    pub(crate) fn take_due_timers(&self, now: Instant, due: &mut Vec<TimerId>) {
        if let Ok(mut timers) = self.timers.try_borrow_mut() {
            timers.take_due(now, due);
        }
    }

    /// Whether the plugin still has the timer `id`.
    pub(crate) fn has_timer(&self, id: TimerId) -> bool {
        self.timers.try_borrow().is_ok_and(|timers| timers.contains(id))
    }

    /// The timers, for a request the plugin made on the thread that owns
    /// them. clack hands the plugin's main-thread calls to this handler
    /// without checking where they come from, and the timers are not for
    /// sharing between threads; a plugin that calls from elsewhere is
    /// turned down.
    fn timers(&self) -> Result<std::cell::RefMut<'_, Timers>, HostError> {
        if !self.shared.on_host_thread() {
            return Err(HostError::Message("timers are managed on the main thread"));
        }
        self.timers.try_borrow_mut().map_err(|_| HostError::Message("the timers are in use"))
    }
}

impl<'a> MainThreadHandler<'a> for HostMainThread<'a> {}

impl HostTimerImpl for HostMainThread<'_> {
    /// The period is taken as asked. How often a timer really ticks is
    /// bounded by how often the plugin main thread idles, about 60 times a
    /// second.
    fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
        let period = Duration::from_millis(period_ms.into());
        let id = self.timers()?.register(period, Instant::now());
        if id.is_none() {
            log::warn!("[{}] has registered too many timers; refusing another", self.shared.plugin_id);
        }
        id.ok_or(HostError::Message("too many timers"))
    }

    fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
        if self.timers()?.unregister(timer_id) { Ok(()) } else { Err(HostError::Message("no such timer")) }
    }
}

impl HostParamsImplMainThread for HostMainThread<'_> {
    fn rescan(&self, _flags: ParamRescanFlags) {
        // Values are read from the plugin whenever they are asked for, and
        // the parameter list is fixed for the life of the device.
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for HostMainThread<'_> {
    fn mark_dirty(&self) {
        // The session captures plugin state whenever it saves or snapshots
        // the project; it does not track per-plugin dirtiness.
    }
}

impl HostAudioPortsImpl for HostMainThread<'_> {
    fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
        false
    }

    fn rescan(&self, _flags: AudioPortRescanFlags) {}
}

impl HostNotePortsImpl for HostMainThread<'_> {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }

    fn rescan(&self, _flags: NotePortRescanFlags) {}
}

impl HostLatencyImpl for HostMainThread<'_> {
    fn changed(&self) {
        // CLAP only allows latency to change during activation, and it is
        // read right after activating.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_context_nests_and_is_per_thread() {
        assert!(!in_audio_context());
        {
            let _outer = AudioContext::enter();
            assert!(in_audio_context());
            {
                let _inner = AudioContext::enter();
                assert!(in_audio_context());
            }
            assert!(in_audio_context(), "leaving a nested context keeps the outer one");
            assert!(!thread::spawn(in_audio_context).join().unwrap(), "other threads are unaffected");
        }
        assert!(!in_audio_context());
    }

    #[test]
    fn requests_are_recorded_until_taken() {
        let shared = HostShared::new("com.acme.synth");
        assert!(shared.is_main_thread());
        assert!(!shared.take_callback_request());

        shared.request_callback();
        shared.request_callback();
        assert!(shared.take_callback_request());
        assert!(!shared.take_callback_request(), "repeated requests collapse into one");

        shared.request_restart();
        assert!(shared.take_restart_request());
        assert!(!shared.take_restart_request());

        let shared = std::sync::Arc::new(shared);
        let other = shared.clone();
        assert!(!thread::spawn(move || other.is_main_thread()).join().unwrap());
    }

    #[test]
    fn the_main_thread_is_a_role_like_the_audio_thread() {
        let shared = HostShared::new("com.acme.synth");
        assert!(shared.is_main_thread() && !shared.is_audio_thread());
        let _flushing = AudioContext::enter();
        assert!(!shared.is_main_thread() && shared.is_audio_thread());
    }

    #[test]
    fn a_flush_request_stands_until_parameters_were_exchanged() {
        let shared = HostShared::new("com.acme.synth");
        assert!(!shared.flush_requested());
        shared.request_flush();
        assert!(shared.flush_requested());
        assert!(shared.flush_requested(), "asking does not answer it");
        shared.flush_served();
        assert!(!shared.flush_requested());
    }

    #[test]
    fn editor_requests_only_count_while_the_editor_is_open() {
        let shared = HostShared::new("com.acme.synth");
        let size = |width, height| GuiSize { width, height };
        assert!(shared.request_resize(size(640, 480)).is_err(), "there is no window to resize");
        assert_eq!(shared.take_editor_requests(), EditorRequests::default());

        shared.set_editor_open(true);
        assert!(shared.request_resize(size(0, 480)).is_err());
        assert!(shared.request_resize(size(640, 480)).is_ok());
        assert!(shared.request_resize(size(800, 600)).is_ok());
        shared.resize_hints_changed();
        let requests = shared.take_editor_requests();
        assert_eq!(requests.resize, Some(Size::new(800, 600)), "the latest size is the one that counts");
        assert!(requests.hints_changed && !requests.lost);
        assert_eq!(shared.take_editor_requests(), EditorRequests::default(), "requests are served once");

        // The host does not show or hide editors at a plugin's request.
        assert!(shared.request_show().is_err() && shared.request_hide().is_err());

        // What was asked about an editor that has since closed is dropped.
        shared.request_resize(size(640, 480)).unwrap();
        shared.closed(true);
        shared.set_editor_open(false);
        assert_eq!(shared.take_editor_requests(), EditorRequests::default());
    }

    #[test]
    fn timers_are_registered_on_the_host_thread_only() {
        let shared = HostShared::new("com.acme.synth");
        let handler = HostMainThread::new(&shared);
        let id = handler.register_timer(0).unwrap();
        assert!(handler.has_timer(id));
        let mut due = Vec::new();
        handler.take_due_timers(Instant::now(), &mut due);
        assert_eq!(due, [id]);

        assert!(handler.unregister_timer(id).is_ok());
        assert!(handler.unregister_timer(id).is_err());
        assert!(!handler.has_timer(id));

        // A plugin calling from a thread of its own is turned down rather
        // than let at state that belongs to the plugin thread.
        let shared = std::sync::Arc::new(HostShared::new("com.acme.synth"));
        let elsewhere = shared.clone();
        let refused = thread::spawn(move || HostMainThread::new(&elsewhere).register_timer(10).is_err());
        assert!(refused.join().unwrap());
    }
}
