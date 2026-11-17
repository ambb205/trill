//! The audio engine.
//!
//! * [`Session`] is the front door: one project, its undo history, transport
//!   and rendering. Front ends (GUI, MCP) only ever talk to a session.
//! * [`Processor`] is the audio-thread core, shared by realtime playback and
//!   offline rendering.
//! * [`Engine`] owns the audio thread and the lock-free queues to it.

pub mod analysis;
pub mod apart;
mod clock;
pub mod compile;
mod crew;
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
mod spread;
mod stretcher;

pub use analysis::AudioStats;
pub use compile::Problem;
pub use engine::{AudioBackend, AudioSetup, Engine, EngineError, Patience, StreamInfo};
pub use graph::{MAX_BLOCK, Meter};
pub use host::{AudioConfig, AudioDevice, AudioDevices, AudioHost};
pub use instruments::InstrumentKnowledge;
pub use live::LiveSender;
pub use processor::Processor;
pub use render::{RenderError, RenderJob, RenderOptions, RenderReport, Rendered, Watch};
pub use saved::{Saved, SavedPresets, SavedSound};
pub use session::{
    Added, Given, ListedSound, MAX_COUNT_IN_BARS, ParamState, ProbeTarget, RecordState, RecordStatus, SampleFile,
    SavedAsSound, Session, SessionError, SilentInstrument, TakeClip, TransportStatus, file_facts, listed_sounds,
    sound_after,
};
pub use sources::AudioSources;

/// A session shared between front ends.
pub type SharedSession = std::sync::Arc<parking_lot::Mutex<Session>>;
