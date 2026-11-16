//! Music theory for writing songs in musical terms.
//!
//! The rest of Trill deals in notes: a pitch, a start, a length and a
//! velocity. Musicians, and the assistants that compose through the MCP
//! server, think in chords, keys, grooves and phrases. This crate is the
//! translation between the two. It is pure: no audio, no I/O, no global
//! state, and everything that looks random is driven by a seed, so the same
//! request always writes the same music.
//!
//! | To...                                   | use                          |
//! |-----------------------------------------|------------------------------|
//! | name notes, build scales, snap to a key | [`pitch`], [`scale`]         |
//! | read `Am9`, `F#m7b5/A` or `bVII`        | [`chord`], [`roman`]         |
//! | place a chord on the keyboard           | [`voicing`]                  |
//! | lay a progression out in time and comp  | [`progression`]              |
//! | break chords up or put a bass under them | [`arpeggio`], [`bass`]      |
//! | write a line as degrees or note names   | [`melody`]                   |
//! | write drums as a grid, or take a groove | [`steps`], [`drums`], [`grooves`] |
//! | swing, humanize and shape velocity      | [`expression`]               |
//! | find the key and the chords of a passage | [`analysis`]                |
//!
//! Every notation is documented at the top of its module, and every parser
//! answers bad input with an [`Error`] that quotes it and says what was
//! expected.

pub mod analysis;
pub mod arpeggio;
pub mod bass;
pub mod chord;
pub mod drums;
pub mod error;
pub mod expression;
pub mod grooves;
pub mod melody;
pub mod pitch;
pub mod progression;
pub mod roman;
pub mod scale;
pub mod steps;
pub mod timing;
pub mod voicing;

pub use analysis::{KeyEstimate, estimate_key, read_chords, summarize};
pub use arpeggio::{ArpOrder, Arpeggio};
pub use bass::{BassLine, BassStyle, bass_line};
pub use chord::{Chord, Quality, identify, is_no_chord};
pub use drums::{DrumPattern, FillPlan};
pub use error::{Error, Result};
pub use expression::{Dynamics, Humanize, Level, Rng, Swing};
pub use grooves::{FILLS, Fill, GROOVES, Groove};
pub use melody::Phrase;
pub use pitch::{Interval, Letter, NoteName};
pub use progression::{Comping, Progression, Rhythm, Span, StrumDirection};
pub use scale::{Scale, ScaleKind, Snap};
pub use steps::StepPattern;
pub use voicing::{Bass, Voicing, VoicingSpec, VoicingStyle};
