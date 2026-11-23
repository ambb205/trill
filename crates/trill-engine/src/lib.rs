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
pub type SharedSession = std::sync::Arc<parking_lot::Mutex<Session>>;
