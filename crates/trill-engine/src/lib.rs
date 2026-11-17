//! The audio engine.
//!
//! * [`Session`] is the front door: one project, its undo history, transport
//!   and rendering. Front ends (GUI, MCP) only ever talk to a session.
//! * [`Processor`] is the audio-thread core, shared by realtime playback and
//!   offline rendering.
//! * [`Engine`] owns the audio thread and the lock-free queues to it.

pub mod analysis;
mod clock;
pub mod compile;
pub mod engine;
mod glide;
pub mod graph;
pub mod host;
pub mod instruments;
pub mod journal;
pub mod live;
mod monitor;
pub mod probe;
pub mod processor;
mod queue;
pub mod ready;
pub mod render;
pub mod saved;
pub mod scripted;
pub mod session;
pub mod sources;
mod stretcher;

pub use analysis::AudioStats;
pub use compile::Problem;
pub use engine::{AudioBackend, AudioSetup, Engine, EngineError, Patience, StreamInfo};
pub use graph::{MAX_BLOCK, Meter};
pub use host::{AudioConfig, AudioDevice, AudioDevices, AudioHost};
pub use instruments::InstrumentKnowledge;
pub use live::LiveSender;
pub use processor::Processor;
pub use render::{RenderError, RenderJob, RenderOptions, RenderReport, Rendered};
pub use saved::{Saved, SavedPresets};
pub use session::{
    MAX_COUNT_IN_BARS, ParamState, ProbeTarget, RecordState, RecordStatus, SampleFile, Session, SessionError,
    SilentInstrument, TakeClip, TransportStatus, file_facts,
};
pub use sources::AudioSources;

/// A session shared between front ends.
pub type SharedSession = std::sync::Arc<parking_lot::Mutex<Session>>;
