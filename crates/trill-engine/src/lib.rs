//! The audio engine.
//!
//! * [`Session`] is the front door: one project, its undo history, transport
//!   and rendering. Front ends (GUI, MCP) only ever talk to a session.
//! * [`Processor`] is the audio-thread core, shared by realtime playback and
//!   offline rendering.
//! * [`Engine`] owns the audio thread and the lock-free queues to it.

pub mod analysis;
pub mod compile;
pub mod engine;
pub mod graph;
mod monitor;
pub mod processor;
pub mod render;
pub mod session;
pub mod sources;
mod stretcher;

pub use analysis::AudioStats;
pub use compile::Problem;
pub use engine::{AudioBackend, Engine, EngineError, StreamInfo};
pub use graph::{MAX_BLOCK, Meter};
pub use processor::Processor;
pub use render::{RenderError, RenderJob, RenderOptions, RenderReport, Rendered};
pub use session::{ParamState, SampleFile, Session, SessionError, TransportStatus};
pub use sources::AudioSources;

/// A session shared between front ends.
pub type SharedSession = std::sync::Arc<parking_lot::Mutex<Session>>;
