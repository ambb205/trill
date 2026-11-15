//! The host side of the CLAP ABI: what a plugin may ask of the DAW.
//!
//! The host is deliberately static. A device's parameter list, ports and
//! latency are read once when it is created and reported to the engine
//! then; the device API has no way to change them afterwards. So requests
//! that amount to "my shape changed, look again" are declined or noted, and
//! take effect the next time the device is created.

use clack_extensions::audio_ports::{AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl};
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortRescanFlags};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_host::prelude::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, ThreadId};

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
pub(crate) struct DawHost;

impl HostHandlers for DawHost {
    type Shared<'a> = HostShared;
    type MainThread<'a> = HostMainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostThreadCheck>()
            .register::<HostParams>()
            .register::<HostState>()
            .register::<HostAudioPorts>()
            .register::<HostNotePorts>()
            .register::<HostLatency>();
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
}

impl HostShared {
    /// Must be called on the plugin main thread.
    pub(crate) fn new(plugin_id: &str) -> Self {
        Self {
            plugin_id: plugin_id.to_string(),
            main_thread: thread::current().id(),
            callback_requested: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
        }
    }

    pub(crate) fn plugin_id(&self) -> &str {
        &self.plugin_id
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
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread
    }

    fn is_audio_thread(&self) -> bool {
        in_audio_context()
    }
}

impl HostParamsImplShared for HostShared {
    fn request_flush(&self) {
        // Parameter events travel with `process`, which runs on every block.
    }
}

/// Callbacks a plugin makes on its main thread. Stateless: see the module
/// documentation for why none of these requests changes anything.
pub(crate) struct HostMainThread;

impl MainThreadHandler<'_> for HostMainThread {}

impl HostParamsImplMainThread for HostMainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {
        // Values are read from the plugin whenever they are asked for, and
        // the parameter list is fixed for the life of the device.
    }

    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for HostMainThread {
    fn mark_dirty(&self) {
        // The session captures plugin state whenever it saves or snapshots
        // the project; it does not track per-plugin dirtiness.
    }
}

impl HostAudioPortsImpl for HostMainThread {
    fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
        false
    }

    fn rescan(&self, _flags: AudioPortRescanFlags) {}
}

impl HostNotePortsImpl for HostMainThread {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }

    fn rescan(&self, _flags: NotePortRescanFlags) {}
}

impl HostLatencyImpl for HostMainThread {
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
}
