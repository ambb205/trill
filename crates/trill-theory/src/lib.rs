//! Music theory for writing songs in musical terms.
//!
//! The rest of Trill deals in notes: a pitch, a start, a length and a
//! velocity. Musicians, and the assistants that compose through the MCP
//! server, think in chords, keys, grooves and phrases. This crate is the
//! translation between the two. It is pure: no audio, no I/O, no global
//! state, and everything that looks random is driven by a seed, so the same
//! request always writes the same music.

pub mod chord;
pub mod error;
pub mod expression;
pub mod pitch;
pub mod progression;
pub mod roman;
pub mod scale;
pub mod steps;
pub mod timing;
pub mod voicing;

pub use chord::{Chord, Quality, identify, is_no_chord};
pub use error::{Error, Result};
pub use expression::{Dynamics, Humanize, Level, Rng, Swing};
pub use pitch::{Interval, Letter, NoteName};
pub use progression::{Comping, Progression, Rhythm, Span, StrumDirection};
pub use scale::{Scale, ScaleKind, Snap};
pub use steps::StepPattern;
pub use voicing::{Bass, Voicing, VoicingSpec, VoicingStyle};
