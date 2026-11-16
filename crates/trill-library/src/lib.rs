//! The sound library and the program's settings.
//!
//! Point a [`Library`] at folders of samples and it indexes them in the
//! background: every audio file, MIDI file, project and preset, with what
//! its header says (length, sample rate, channels) and what its name and
//! folders say (tempo, key, and [tags](Tag) such as kick, pad or loop).
//! The index can then be [searched](Library::search) as fast as someone
//! types, and browsed as the folder tree it came from through
//! [`Library::index`].
//!
//! ```no_run
//! use trill_library::{Library, Query, Settings, Tag, Tags};
//! use std::time::Duration;
//!
//! let settings_path = Settings::default_path().expect("a home folder");
//! let mut settings = Settings::load(&settings_path).settings;
//!
//! let library = Library::open(&settings.library, Library::default_index_path());
//! library.add_root("D:/Samples".as_ref());
//! library.rescan(); // returns at once; the list fills in as the scan goes
//! library.wait_until_idle(Duration::from_secs(60));
//!
//! let query = Query { text: "deep kick".into(), tags: Tags::of(&[Tag::OneShot]), limit: Some(200), ..Query::default() };
//! for entry in library.search(&query).entries() {
//!     println!("{} ({:?} s)", entry.name, entry.seconds());
//! }
//!
//! settings.library = library.settings();
//! settings.save(&settings_path).expect("settings saved");
//! ```
//!
//! Nothing here touches the audio thread, and nothing panics on what it
//! finds on disk: folders, files and the saved index are all untrusted.

mod entry;
mod index;
mod infer;
mod key;
mod library;
mod locations;
mod natural;
#[cfg(test)]
mod scale;
mod scan;
mod search;
mod settings;
mod store;
mod tags;

pub use entry::{AudioFacts, Entry, EntryKind};
pub use index::{EntryId, Folder, FolderId, Index};
pub use infer::{Inferred, infer};
pub use key::Key;
pub use library::{Library, ScanStatus, SearchResults};
pub use locations::{SuggestedLocation, suggested_locations};
pub use natural::natural_cmp;
pub use scan::{ScanPhase, ScanSummary};
pub use search::Query;
pub use settings::{
    AudioSettings, LibrarySettings, Loaded, MidiSettings, SaveError, Settings, UiSettings, cache_dir, config_dir,
};
pub use tags::{Tag, TagRule, Tags, VOCABULARY};
