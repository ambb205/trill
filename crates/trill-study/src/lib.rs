//! A recording taken apart: a *study*.
//!
//! Given a recording, a study's run finds what only models can (its
//! stems, its beats, the words sung in it and where each is sung) and
//! then everything `trill-listen` hears in the stems over those beats:
//! key, chords, parts, loops, single hits, the groove, the notes of the
//! bass and of the melody. What it finds is kept in a folder of its own
//! ([`store`]), which is all that anything showing or reading a study
//! needs: no model is run again to draw it.
//!
//! - [`run`] is the stages in order; a stage that is done is not done again.
//! - [`desk`] is what a program holds of studies: it gives each its turn,
//!   refuses one the machine has no room for, and says how each is.
//! - [`helper`] is the process the run is done in: the program started a
//!   second time, as a song's renders are, so that a model's runtime that
//!   faults or takes too much memory costs a sentence.
//! - [`models`] is what a run asks of models, the real ones (ONNX Runtime,
//!   loaded when a study is run, never linked) and scripted ones for tests.
//! - [`lyrics`] puts words where they are sung, by the loudness of the vocal.
//! - [`room`] says how much memory there is and how much a process took.
//! - [`link`] finds the programs that are not Trill's own.
//! - [`setup`] is what a study needs that Trill does not ship: the parts a
//!   user fetches, each pinned, and the timed test of what runs the models.

pub mod desk;
pub mod helper;
pub mod link;
pub mod lyrics;
pub mod models;
pub mod room;
pub mod run;
pub mod setup;
pub mod store;

pub use desk::{Desk, DeskOptions, Report};
pub use lyrics::{Line, Lyrics, Word};
pub use setup::Setup;
pub use store::{Source, Stage, StageReport, StageState, State, StemChoice, Studies, Study, StudyFolder, StudyOptions};
