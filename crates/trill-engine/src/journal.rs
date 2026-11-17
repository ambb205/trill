//! Who is acting on a session, and where its journals are kept.
//!
//! The journal itself is `trill_core::journal`: what a step holds, how a
//! song is rebuilt from it, how a session is played again. The session
//! records into it (`session/story.rs`). This module is the two things
//! around that which are not the model's business:
//!
//! * **Who is acting.** A front end that does things on someone's behalf
//!   (the MCP server, for a client or for the assistant) opens a call in
//!   the journal and then says, for as long as it works on it, that what
//!   its thread does to the session is that call's doing ([`acting`]).
//!   Anything done to the session by a thread that says nothing is taken to
//!   be someone at the window. The session is shared and a call takes it
//!   and lets go of it many times; saying so per thread is what keeps an
//!   edit made at the window in the middle of a call from being counted as
//!   the call's.
//! * **Where journals are kept.** A journal holds a conversation, so it is
//!   never written into a project file or beside one: it lives in a folder
//!   of the program's own, under a name worked out from where the project
//!   file is ([`file_for`]). Handing someone a song hands them the song.

use std::cell::Cell;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use trill_core::journal::CallId;

thread_local! {
    static ACTING: Cell<Option<CallId>> = const { Cell::new(None) };
}

/// Says that what this thread does to a session, until the guard that is
/// returned is dropped, is the doing of `call`: a call opened with
/// [`Session::begin_call`](crate::Session::begin_call). Guards nest; each
/// puts back what was there before it.
///
/// A future that works on a call across awaits takes a guard each time it
/// is polled and lets go of it before it yields: the thread it is polled
/// on is the one that acts, and between polls that thread is someone
/// else's.
pub fn acting(call: CallId) -> Acting {
    Acting { before: ACTING.replace(Some(call)), _here: PhantomData }
}

/// While this lives, the thread it was made on acts for a call; see
/// [`acting`].
#[must_use = "the thread acts for the call only while the guard lives"]
pub struct Acting {
    before: Option<CallId>,
    /// Made and dropped on one thread.
    _here: PhantomData<*const ()>,
}

impl Drop for Acting {
    fn drop(&mut self) {
        ACTING.set(self.before);
    }
}

/// The call this thread is acting for, if it said so.
pub(crate) fn actor() -> Option<CallId> {
    ACTING.get()
}

/// The time by the system's clock, in milliseconds since 1970.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// The ending of a journal's file.
pub const FILE_ENDING: &str = "journal";
/// The ending of a journal that was written out to be handed on.
pub const EXPORT_ENDING: &str = "trillstory";
/// Journals are kept for this many projects, the most recently written.
pub const MOST_JOURNALS: usize = 200;

/// A project file as the key its journal is kept under: the whole path,
/// spelled one way.
fn key(project: &Path) -> String {
    let absolute = std::path::absolute(project).unwrap_or_else(|_| project.to_owned());
    let text = absolute.to_string_lossy().replace('\\', "/");
    // Windows does not tell `Song.trill` from `song.trill`.
    if cfg!(windows) { text.to_lowercase() } else { text }
}

/// The file in `folder` that holds the journal of the project at
/// `project`: sixteen hex digits worked out from where the project is, so
/// that the name says nothing of the song.
pub fn file_for(folder: &Path, project: &Path) -> PathBuf {
    // FNV-1a: small, and the same on every machine and in every version.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key(project).bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    folder.join(format!("{hash:016x}.{FILE_ENDING}"))
}

/// Lets go of the journals in `folder` beyond the [`MOST_JOURNALS`] most
/// recently written. Called once, when a program starts.
pub fn prune(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ending| ending == FILE_ENDING))
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    files.sort_by_key(|(written, _)| std::cmp::Reverse(*written));
    for (_, path) in files.into_iter().skip(MOST_JOURNALS) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journals_file_goes_by_where_its_project_is_and_says_nothing_of_it() {
        let folder = Path::new("journals");
        let song = file_for(folder, Path::new("C:/Songs/Night Drive.trill"));
        let name = song.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name.len(), 16 + 1 + FILE_ENDING.len());
        assert!(!name.to_lowercase().contains("night"), "{name}");
        assert_eq!(song.parent(), Some(folder));
        // The same project, however its path is written.
        assert_eq!(file_for(folder, Path::new("C:\\Songs\\Night Drive.trill")), song);
        if cfg!(windows) {
            assert_eq!(file_for(folder, Path::new("c:/songs/night drive.TRILL")), song);
        }
        assert_ne!(file_for(folder, Path::new("C:/Songs/Day Drive.trill")), song);
    }

    #[test]
    fn only_the_most_recently_written_journals_are_kept() {
        let folder = tempfile::tempdir().unwrap();
        for index in 0..MOST_JOURNALS + 5 {
            std::fs::write(folder.path().join(format!("{index:016x}.{FILE_ENDING}")), "x").unwrap();
        }
        std::fs::write(folder.path().join("notes.txt"), "not a journal").unwrap();
        prune(folder.path());
        let left = std::fs::read_dir(folder.path()).unwrap().count();
        assert_eq!(left, MOST_JOURNALS + 1, "the newest journals, and what is not one");
        prune(Path::new("no such folder"));
    }

    #[test]
    fn a_thread_acts_for_a_call_only_while_it_says_so() {
        let mut journal = trill_core::journal::Journal::begin(&trill_core::Project::new("x"), 0);
        let outer = journal.begin_call(trill_core::journal::Author::Assistant, "a", "A");
        let inner = journal.begin_call(trill_core::journal::Author::Assistant, "b", "B");
        assert_eq!(actor(), None);
        {
            let _outer = acting(outer);
            assert_eq!(actor(), Some(outer));
            {
                let _inner = acting(inner);
                assert_eq!(actor(), Some(inner));
            }
            assert_eq!(actor(), Some(outer));
            // Another thread says nothing, and acts for nobody.
            assert_eq!(std::thread::spawn(actor).join().unwrap(), None);
        }
        assert_eq!(actor(), None);
    }
}
