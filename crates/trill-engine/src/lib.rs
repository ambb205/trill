//! The audio engine.
//!
//! * [`Session`] is the front door: one project, its undo history, transport
//!   and rendering. Front ends (GUI, MCP) only ever talk to a session.
//! * [`Processor`] is the audio-thread core, shared by realtime playback and
//!   offline rendering.
//! * [`Engine`] owns the audio thread and the lock-free queues to it.

pub mod analysis;
pub mod apart;
pub mod capture;
pub mod character;
mod clock;
pub mod compile;
mod crew;
pub mod engine;
mod glide;
pub mod graph;
pub mod hand;
pub mod host;
pub mod instruments;
pub mod journal;
pub mod live;
mod monitor;
pub mod probe;
pub mod processor;
mod queue;
pub mod ready;
pub mod recorder;
pub mod render;
pub mod saved;
pub mod scripted;
pub mod session;
pub mod sources;
mod spread;
mod stretcher;
pub mod team;

pub use analysis::AudioStats;
pub use compile::Problem;
pub use engine::{
    AudioBackend, AudioSetup, Engine, EngineError, InputInfo, Load, Patience, StreamInfo, usual_play_threads,
};
pub use graph::{Cost, MAX_BLOCK, Meter};
pub use host::{AudioConfig, AudioDevice, AudioDevices, AudioHost};
pub use instruments::InstrumentKnowledge;
pub use live::LiveSender;
pub use processor::Processor;
pub use recorder::{COLUMN_FRAMES, Growing};
pub use render::{After, RenderError, RenderJob, RenderOptions, RenderReport, Rendered, Watch};
pub use saved::{Change as PresetChange, Saved, SavedPresets, SavedSound};
pub use session::{
    Added, Arriving, Bin, ComingBeside, Correction, Costs, FileHere, Given, GrowingTake, InputClock, InputStatus,
    KeepingUp, ListedSound, MAX_COUNT_IN_BARS, Made, Measured, Measurement, Notice, ParamState, ProbeTarget,
    RecordState, RecordStatus, Recorded, RecordedTake, RecordingLatency, Remedy, Removed, RenditionHere,
    RenditionWanted, SampleFile, SavedAsSound, Session, SessionError, Shortfall, SilentInstrument, TakeClip, TrackCost,
    TransportStatus, Turned, file_facts, listed_sounds, sound_after, spelled_path,
};
pub use sources::AudioSources;

/// A session shared between front ends.
pub type SharedSession = std::sync::Arc<SessionLock>;

/// The lock a shared session is held under: a mutex, whose waiters may
/// have something else to do meanwhile.
///
/// On macOS the thread a window draws on is the thread plugins are served
/// on, and another thread may hold the session while it waits for a plugin's
/// answer: a waiter on that thread who only waited would wait for itself.
/// So where a program says what a waiting thread is to do
/// ([`SessionLock::serve_while_waiting`]), [`SessionLock::lock`] does that
/// between short waits. Nowhere else does it differ from the mutex.
pub struct SessionLock(parking_lot::Mutex<Session>);

/// What a thread that waits for the session does meanwhile: one piece of
/// work, and whether it is a thread that has such work (when it is not, it
/// waits as anyone does).
static WHILE_WAITING: std::sync::OnceLock<fn() -> bool> = std::sync::OnceLock::new();

impl SessionLock {
    pub fn new(session: Session) -> Self {
        Self(parking_lot::Mutex::new(session))
    }

    /// Has a thread that waits for a session do `serve` between short
    /// waits, for as long as `serve` says that the thread is one that has
    /// work of that kind. Set once, by a program, before anything waits.
    pub fn serve_while_waiting(serve: fn() -> bool) {
        let _ = WHILE_WAITING.set(serve);
    }

    pub fn lock(&self) -> parking_lot::MutexGuard<'_, Session> {
        if let Some(guard) = self.0.try_lock() {
            return guard;
        }
        if let Some(serve) = WHILE_WAITING.get() {
            loop {
                if let Some(guard) = self.0.try_lock_for(std::time::Duration::from_millis(2)) {
                    return guard;
                }
                if !serve() {
                    break;
                }
            }
        }
        self.0.lock()
    }

    pub fn try_lock(&self) -> Option<parking_lot::MutexGuard<'_, Session>> {
        self.0.try_lock()
    }
}
