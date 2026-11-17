//! Safety copies: what a crash must not cost.
//!
//! While a project has changes that were not saved, a copy of it is kept in
//! a folder of the program's own and refreshed as the project changes.
//! Saving the project, or leaving the program in the ordinary way, removes
//! the copy. If the program dies instead (a plugin takes it down, the power
//! goes), the copy is still there the next time it starts, and is offered
//! back.
//!
//! How a copy is known to be orphaned: each running program holds a lock on
//! a file beside its copy for as long as it lives, and the system releases
//! that lock when the process ends, however it ends. A copy whose lock can
//! be taken belongs to nobody. Several programs can therefore run at once
//! (the window and the headless server, or two windows) without mistaking
//! each other's copies for wreckage, and a program that finds wreckage
//! keeps the lock it took, so that no second one offers the same project.
//!
//! Three files make a copy, named after the process that wrote them:
//! `<id>.lock`, `<id>.trill` (an ordinary project file) and `<id>.json`
//! (what the project was called, where it lived and when the copy was made).
//! Where a journal is kept, a fourth holds it as far as the copy goes
//! (`<id>.journal`), so that a project that comes back comes back with how
//! it was made. It is added to, not rewritten, as the project changes.

use serde_json::{Value, json};
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use trill_core::journal::{Writing, Written};
use trill_engine::SharedSession;

/// A project that was not saved before the program last stopped.
#[derive(Clone, Debug, PartialEq)]
pub struct SafetyCopy {
    /// The project's name.
    pub name: String,
    /// The file the project was opened from or last saved to, if any.
    pub original: Option<PathBuf>,
    /// When the copy was made.
    pub made: SystemTime,
    /// The copy itself: an ordinary project file.
    pub file: PathBuf,
}

/// This program's own lock, made the first time it has something to keep.
struct Own {
    /// The folder and the id: add an extension for each of the three files.
    stem: PathBuf,
    /// Held for the life of the process. Never read.
    _lock: File,
}

pub(crate) struct SafetyCopies {
    /// Where copies are kept, or `None` when nothing is kept on disk.
    folder: Option<PathBuf>,
    own: Option<Own>,
    /// The revision of the project the own copy was made of.
    copied: Option<u64>,
    /// What the journal beside the own copy holds.
    journal: Written,
    /// What other programs left behind, each with the lock that claims it.
    orphans: Vec<(SafetyCopy, File)>,
    /// Leave the own copy where it is when this goes: see [`Self::abandon`].
    abandoned: bool,
}

fn file_of(stem: &Path, extension: &str) -> PathBuf {
    // Not `with_extension`: an id is not a file name with one.
    let mut name = stem.as_os_str().to_owned();
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

fn remove_all(stem: &Path) {
    for extension in ["trill", "json", "tmp", "journal", "lock"] {
        let _ = std::fs::remove_file(file_of(stem, extension));
    }
}

impl SafetyCopies {
    /// Looks in `folder` for what earlier programs left behind. Nothing is
    /// created yet: a program that never has unsaved changes leaves no
    /// trace.
    pub fn open(folder: Option<PathBuf>) -> Self {
        let mut copies = Self {
            folder,
            own: None,
            copied: None,
            journal: Written::default(),
            orphans: Vec::new(),
            abandoned: false,
        };
        let Some(folder) = &copies.folder else { return copies };
        let Ok(entries) = std::fs::read_dir(folder) else { return copies };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.extension().is_none_or(|extension| extension != "lock") {
                continue;
            }
            let Ok(lock) = OpenOptions::new().read(true).write(true).open(&path) else { continue };
            // Held by a program that is still running: not ours to touch.
            if lock.try_lock().is_err() {
                continue;
            }
            let stem = path.with_extension("");
            let file = file_of(&stem, "trill");
            match described(&stem, &file) {
                Some(copy) => copies.orphans.push((copy, lock)),
                // A lock with no project beside it: its owner died with
                // nothing unsaved, or between two steps of writing.
                None => {
                    drop(lock);
                    remove_all(&stem);
                }
            }
        }
        copies.orphans.sort_by_key(|(copy, _)| std::cmp::Reverse(copy.made));
        copies
    }

    /// What earlier programs left behind, most recent first.
    pub fn orphans(&self) -> Vec<SafetyCopy> {
        self.orphans.iter().map(|(copy, _)| copy.clone()).collect()
    }

    /// The revision of the project this program's copy was made of.
    pub fn copied(&self) -> Option<u64> {
        self.copied
    }

    /// What the journal beside this program's copy holds, taken out to be
    /// brought up to date ([`Self::write`] takes it back).
    pub fn take_journal(&mut self) -> Written {
        std::mem::take(&mut self.journal)
    }

    /// Replaces this program's copy with `project`, the text of a project
    /// file. The old copy stays in place until the new one is complete.
    /// `journal` is what to write to bring the journal beside it up to
    /// date, after which it holds what `written` says.
    pub fn write(
        &mut self,
        revision: u64,
        project: &str,
        name: &str,
        original: Option<&Path>,
        (journal, written): (Writing, Written),
    ) -> io::Result<()> {
        let Some(folder) = self.folder.clone() else { return Ok(()) };
        if self.own.is_none() {
            std::fs::create_dir_all(&folder)?;
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos());
            let stem = folder.join(format!("{}-{nanos:x}", std::process::id()));
            let lock = OpenOptions::new().create_new(true).read(true).write(true).open(file_of(&stem, "lock"))?;
            match lock.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => return Err(io::Error::other("the new lock file is already locked")),
                Err(TryLockError::Error(error)) => return Err(error),
            }
            self.own = Some(Own { stem, _lock: lock });
        }
        let stem = &self.own.as_ref().expect("made above").stem;
        let temporary = file_of(stem, "tmp");
        std::fs::write(&temporary, project)?;
        std::fs::rename(&temporary, file_of(stem, "trill"))?;
        let made = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        let about = json!({ "name": name, "original": original.map(|path| path.display().to_string()), "made": made });
        std::fs::write(file_of(stem, "json"), about.to_string())?;
        self.copied = Some(revision);
        // The journal last: a copy without one is still the project. One
        // that could not be added to is written whole the next time.
        let kept = file_of(stem, "journal");
        let wrote = match journal {
            Writing::Nothing => Ok(()),
            Writing::Append(text) => {
                use std::io::Write as _;
                OpenOptions::new().append(true).open(&kept).and_then(|mut file| file.write_all(text.as_bytes()))
            }
            Writing::Whole(text) => std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, &kept)),
        };
        match wrote {
            Ok(()) => self.journal = written,
            Err(error) => log::warn!("could not keep the journal beside the safety copy: {error}"),
        }
        Ok(())
    }

    /// Puts back what [`Self::take_journal`] took, when nothing was
    /// written after all.
    pub fn give_journal(&mut self, written: Written) {
        self.journal = written;
    }

    /// Removes this program's copy: the project is saved, or there is
    /// nothing in it to lose.
    pub fn clear(&mut self) {
        self.journal = Written::default();
        if let (Some(own), Some(_)) = (&self.own, self.copied.take()) {
            for extension in ["trill", "json", "tmp", "journal"] {
                let _ = std::fs::remove_file(file_of(&own.stem, extension));
            }
        }
    }

    /// Deletes what an earlier program left behind. False if `copy` is not
    /// one of those.
    pub fn discard(&mut self, copy: &SafetyCopy) -> bool {
        let Some(index) = self.orphans.iter().position(|(orphan, _)| orphan.file == copy.file) else { return false };
        let (orphan, lock) = self.orphans.remove(index);
        drop(lock);
        remove_all(&orphan.file.with_extension(""));
        true
    }

    /// Leaves everything where it is from now on, as a process that dies
    /// does. For tests of what the next program finds.
    pub fn abandon(&mut self) {
        self.abandoned = true;
    }
}

impl Drop for SafetyCopies {
    /// Leaving in the ordinary way takes the copy along: the user chose not
    /// to save. Leaving by a panic does not, and neither, of course, does
    /// any ending that never gets here.
    fn drop(&mut self) {
        if self.abandoned || std::thread::panicking() {
            return;
        }
        if let Some(own) = self.own.take() {
            let Own { stem, _lock } = own;
            drop(_lock);
            remove_all(&stem);
        }
    }
}

/// What the copy at `file` is a copy of, or `None` if there is no copy.
fn described(stem: &Path, file: &Path) -> Option<SafetyCopy> {
    let written = std::fs::metadata(file).ok().filter(|metadata| metadata.is_file())?.modified().ok();
    // The description is written after the project. Without it the copy is
    // still worth offering, under what little is known.
    let about: Value = std::fs::read_to_string(file_of(stem, "json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let made = about["made"].as_u64().map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds)).or(written);
    Some(SafetyCopy {
        name: about["name"].as_str().unwrap_or("Untitled").to_owned(),
        original: about["original"].as_str().map(PathBuf::from),
        made: made.unwrap_or(UNIX_EPOCH),
        file: file.to_owned(),
    })
}

/// Brings this program's copy in step with the project: made anew if the
/// project has unsaved changes the copy does not have, removed if the
/// project has none. Cheap when nothing changed.
pub(crate) fn refresh(session: &SharedSession, copies: &parking_lot::Mutex<SafetyCopies>) {
    let (copied, mut written) = {
        let mut copies = copies.lock();
        (copies.copied(), copies.take_journal())
    };
    let project = {
        let mut session = session.lock();
        if !session.is_modified() {
            None
        } else if copied == Some(session.revision()) {
            drop(session);
            // Nothing to write: what the journal's file holds is as it was.
            let _ = copies.lock().give_journal(written);
            return;
        } else {
            let text = session.snapshot_json();
            // After the snapshot: taking it can itself move the revision
            // on, and enters what the plugins' states came to.
            let journal = session.journal_since(&mut written);
            let name = session.project().name.clone();
            Some((session.revision(), text, name, session.path().map(Path::to_owned), journal))
        }
    };
    // The file is written with the session free again.
    let mut copies = copies.lock();
    match project {
        None => copies.clear(),
        Some((revision, text, name, original, journal)) => {
            if let Err(error) = copies.write(revision, &text, &name, original.as_deref(), (journal, written)) {
                log::warn!("could not keep a safety copy of the project: {error}");
            }
        }
    }
}
