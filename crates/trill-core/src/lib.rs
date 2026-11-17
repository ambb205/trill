//! Shared vocabulary of the DAW: the project model, the edit operations on it,
//! and the device API that instruments, effects and plugin hosts implement.
//!
//! This crate has no audio or UI dependencies. Everything that changes a song
//! goes through [`project::Project`]; everything that makes sound implements
//! [`device::DeviceProcessor`].

pub mod audio;
pub mod controllers;
pub mod device;
pub mod ids;
pub mod instrument;
pub mod midi_file;
pub mod pitch;
pub mod presets;
pub mod project;
pub mod time;
mod time_ops;
pub mod travel;

pub use controllers::{ControllerCurve, ControllerKind, ControllerLane};
pub use ids::{ClipId, DeviceId, TrackId};
pub use instrument::{Articulation, InstrumentProfile, KnownInstrument, Lasts, Role};
pub use project::{
    AutomationLane, AutomationPoint, AutomationTarget, ChainRef, Clip, Device, EditError, LoopRegion, Marker, Master,
    Note, Project, Segment, Send, Track, TrackKind, WarpMode,
};
pub use time::{Beats, TimeSignature};
pub use time_ops::{PasteMode, TimeExcerpt};
pub use travel::Travel;
