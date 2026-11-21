//! A song's recordings coming beside it.
//!
//! What is recorded for a song that has no place yet is written into a
//! folder of the program's own. When the song is saved, its recordings
//! come beside it, in `<song> files/takes`, and the song names them there.
//! Two things must hold while that happens: nobody waits for a copy with
//! the session held (an hour of stereo is 1.4 GB), and at every moment
//! whichever file of the song is read finds its recordings.
//!
//! **On one volume a recording is given a second name.** A hard link costs
//! nothing and takes no time: both names are the same file, the song is
//! written naming the one beside it, and then the first name is taken
//! away. Nothing was copied.
//!
//! **Where that is refused** (another volume, a file system without
//! links), the copy is made by a thread of its own. The song goes on
//! naming each recording where it is until its copy is whole and as long
//! as the original; then the song, every state undo can reach, and the
//! song's file as it was saved are all given the new name, by the same
//! means a relink renames a file ([`Project::relink`]: clips, takes, and
//! what the sampler and the drum rack play), and only then is the first
//! copy removed. [`Session::recordings_coming`] says how far it is.
//!
//! What counts as having no place is where the file lies: in a folder of
//! songs that were never saved, of this run of the program or of one
//! before it (a song that was recovered names recordings of an earlier
//! run). Not only at the first save: a take that was still being recorded
//! when the song was saved is brought when it ends.

use super::{Session, SessionError};
use parking_lot::Mutex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use trill_core::Project;
use trill_core::files::Relink;
use trill_dsp::SampleFiles;

/// A path as two spellings of it are told to be one: Windows minds
/// neither the case of a name nor which way its strokes lean, and a song
/// that was opened by a path spelled otherwise names the same files.
pub fn spelled(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) { text.replace('/', "\\").to_lowercase() } else { text.into_owned() }
}

/// Every file `project` names, as the one place that knows says them: its
/// clips, its takes, and what its devices play.
pub(super) fn named_by(project: &Project) -> Vec<String> {
    project.files(&SampleFiles).into_iter().map(|used| used.path).collect()
}

/// Has `project` name the new path wherever it names an old one of
/// `moved` (old, new), by the means a relink does: clips, takes, and what
/// the sampler and the drum rack play.
pub(super) fn rename(project: &mut Project, moved: &[(String, String)]) {
    let new_name = |path: &str| {
        let spelling = spelled(Path::new(path));
        moved.iter().find(|(old, _)| spelled(Path::new(old)) == spelling).map(|(_, new)| new.clone())
    };
    let relinks: Vec<Relink> = project
        .files(&SampleFiles)
        .into_iter()
        .filter_map(|used| Some(Relink { place: used.place, path: new_name(&used.path)? }))
        .collect();
    if relinks.is_empty() {
        return;
    }
    if let Err(error) = project.relink(&relinks, &SampleFiles) {
        log::error!("a recording could not be named where it now is: {error}");
    }
}

/// How far the recordings of a song are with coming beside it; see
/// [`Session::recordings_coming`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ComingBeside {
    /// How many recordings are being copied.
    pub files: usize,
    /// How many bytes of them are copied, and how many there are.
    pub done: u64,
    pub total: u64,
}

impl ComingBeside {
    /// In words, for a status line: "Bringing 2 recordings beside the
    /// song · 40 %".
    pub fn said(&self) -> String {
        let what = if self.files == 1 { "1 recording".to_string() } else { format!("{} recordings", self.files) };
        let share = if self.total == 0 { 100 } else { (self.done * 100 / self.total).min(100) };
        format!("Bringing {what} beside the song \u{00B7} {share} %")
    }
}

/// What the thread that copies says of itself.
#[derive(Default)]
struct Progress {
    done: AtomicU64,
    finished: AtomicBool,
    /// The recordings whose copy is whole, and what went wrong with one
    /// that is not.
    whole: Mutex<Vec<(String, String)>>,
    failed: Mutex<Option<String>>,
}

/// Recordings on their way beside a song, copied by a thread of their own.
pub(super) struct Bringing {
    /// The song they come beside, whose file names them where they were.
    song: PathBuf,
    files: usize,
    total: u64,
    progress: Arc<Progress>,
}

impl Bringing {
    /// Starts copying each of `files` (from, to) for `song`.
    fn start(song: &Path, files: Vec<(String, String)>) -> Self {
        let total = files.iter().filter_map(|(from, _)| std::fs::metadata(from).ok()).map(|file| file.len()).sum();
        let progress = Arc::new(Progress::default());
        let (said, count) = (progress.clone(), files.len());
        let copying = std::thread::Builder::new().name("recordings".into()).spawn(move || {
            for (from, to) in files {
                match copy_whole(Path::new(&from), Path::new(&to), &said.done) {
                    Ok(()) => said.whole.lock().push((from, to)),
                    Err(why) => {
                        *said.failed.lock() = Some(why);
                        break;
                    }
                }
            }
            said.finished.store(true, Ordering::Release);
        });
        if let Err(error) = copying {
            *progress.failed.lock() = Some(format!("the thread that copies recordings could not be started: {error}"));
            progress.finished.store(true, Ordering::Release);
        }
        Self { song: song.to_path_buf(), files: count, total, progress }
    }
}

/// Copies `from` to `to` under another name first, a piece at a time, and
/// gives it its name only when it is whole: as long as the original, and
/// on disk. Counts what it copies in `done`.
fn copy_whole(from: &Path, to: &Path, done: &AtomicU64) -> Result<(), String> {
    use std::io::{Read, Write};
    let said = |error: std::io::Error, path: &Path| format!("'{}': {error}", path.display());
    let temporary = to.with_extension("coming");
    let copied = (|| -> Result<u64, String> {
        let mut source = std::fs::File::open(from).map_err(|error| said(error, from))?;
        let mut target = std::fs::File::create(&temporary).map_err(|error| said(error, &temporary))?;
        let mut piece = vec![0u8; 1 << 20];
        let mut copied = 0u64;
        loop {
            let read = source.read(&mut piece).map_err(|error| said(error, from))?;
            if read == 0 {
                break;
            }
            target.write_all(&piece[..read]).map_err(|error| said(error, &temporary))?;
            copied += read as u64;
            done.fetch_add(read as u64, Ordering::Relaxed);
        }
        target.sync_all().map_err(|error| said(error, &temporary))?;
        Ok(copied)
    })();
    let whole = std::fs::metadata(from).map(|file| file.len()).unwrap_or(u64::MAX);
    match copied {
        Ok(copied) if copied == whole => std::fs::rename(&temporary, to).map_err(|error| said(error, to)),
        Ok(copied) => {
            let _ = std::fs::remove_file(&temporary);
            Err(format!("'{}' was not copied whole: {copied} of {whole} bytes", from.display()))
        }
        Err(why) => {
            let _ = std::fs::remove_file(&temporary);
            Err(why)
        }
    }
}

/// What a save has to do once the song's file is written.
#[derive(Default)]
pub(super) struct Brought {
    /// First names of recordings that have a second one beside the song,
    /// which the song now names: taken away once its file says so.
    pub first_names: Vec<PathBuf>,
    /// Recordings that could be given no second name, with where their
    /// copies go: copied once the song's file is written.
    pub to_copy: Vec<(String, String)>,
}

impl Session {
    /// Whether `path` is a recording of a song that had no place when it
    /// was recorded: it lies in a folder of such a song, of this run of
    /// the program or of one before it.
    fn has_no_place(&self, path: &Path) -> bool {
        let base = self.unsaved_base();
        let Some(root) = base.parent() else { return false };
        // `<root>/<a run>/song N/<the recording>`
        let folder = path.parent();
        let of_a_song =
            folder.and_then(Path::file_name).is_some_and(|name| name.to_string_lossy().starts_with("song "));
        let runs = folder.and_then(Path::parent).and_then(Path::parent);
        of_a_song && runs.is_some_and(|runs| spelled(runs) == spelled(root))
    }

    /// The recordings the song, or a state undo can reach, names that
    /// have no place beside a song yet.
    fn recordings_without_a_place(&self) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        self.each_state(|project| {
            found.extend(named_by(project).into_iter().filter(|path| self.has_no_place(Path::new(path))));
        });
        found
    }

    /// Has the song and every state undo can reach name the new path
    /// wherever they name an old one of `moved`.
    fn rename_everywhere(&mut self, moved: &[(String, String)]) {
        if moved.is_empty() {
            return;
        }
        rename(&mut self.project, moved);
        for step in self.undo.iter_mut().chain(&mut self.redo) {
            rename(&mut step.project, moved);
        }
        for (old, new) in moved {
            self.audio.media().forget(Path::new(old));
            self.audio.media().forget(Path::new(new));
        }
        // The same sound from another name: the graph is made again, and
        // whoever shows the song reads it again. Whether the song has
        // unsaved changes is as it was: this is no edit.
        let saved = self.saved_revision == self.revision;
        self.compile_and_swap();
        self.revision += 1;
        if saved {
            self.saved_revision = self.revision;
        }
    }

    /// Brings the recordings that have no place yet beside `song`, which
    /// is about to be written: each is given a second name there where
    /// the volume allows, and the song and every state undo can reach
    /// name it by that from here on. The others are named where they are,
    /// and are returned to be copied once the song's file is written.
    pub(super) fn bring_recordings_beside(&mut self, song: &Path) -> Result<Brought, SessionError> {
        let waiting = self.recordings_without_a_place();
        let mut brought = Brought::default();
        if waiting.is_empty() {
            return Ok(brought);
        }
        let folder = Self::beside_song(song);
        std::fs::create_dir_all(&folder).map_err(|error| super::io_error(&folder, error))?;
        let mut linked: Vec<(String, String)> = Vec::new();
        let mut names = BTreeSet::new();
        for old in &waiting {
            let from = Path::new(old);
            if !from.is_file() {
                // Gone already: the song goes on naming where it was.
                continue;
            }
            let stem = from.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
            let to = Self::free_recording_name(&folder, &stem, &mut names);
            let new = to.to_string_lossy().into_owned();
            // A second name for the same file, where the volume gives one.
            if !self.copies_recordings() && std::fs::hard_link(from, &to).is_ok() {
                brought.first_names.push(from.to_path_buf());
                linked.push((old.clone(), new));
            } else {
                brought.to_copy.push((old.clone(), new));
            }
        }
        self.rename_everywhere(&linked);
        Ok(brought)
    }

    /// The song's file is written: the first names of the recordings it
    /// now names beside it are taken away, and the ones that could be
    /// given no second name begin to be copied.
    pub(super) fn after_song_was_written(&mut self, song: &Path, brought: Brought) {
        for old in brought.first_names {
            // (One that cannot be taken away stays, and costs a name.)
            if let Err(error) = std::fs::remove_file(&old) {
                log::warn!("'{}' has a name beside the song and could not be taken away: {error}", old.display());
            }
        }
        if !brought.to_copy.is_empty() {
            self.set_bringing(Some(Bringing::start(song, brought.to_copy)));
        }
    }

    /// How far the recordings of the song are with coming beside it,
    /// while any are being copied: a front end says so. `None` when none
    /// are, which is nearly always: on one volume nothing is copied.
    pub fn recordings_coming(&self) -> Option<ComingBeside> {
        let bringing = self.bringing()?;
        let done = bringing.progress.done.load(Ordering::Relaxed);
        Some(ComingBeside { files: bringing.files, done, total: bringing.total })
    }

    /// Takes in what the thread that copies recordings has finished: the
    /// song, every state undo can reach and the song's file as it was
    /// saved name each recording whose copy is whole where it now is, and
    /// the first copy is removed. Part of [`Session::housekeeping`].
    pub(super) fn tend_bringing(&mut self) {
        let Some(bringing) = self.bringing() else { return };
        if !bringing.progress.finished.load(Ordering::Acquire) {
            return;
        }
        let Some(bringing) = self.take_bringing() else { return };
        let whole = std::mem::take(&mut *bringing.progress.whole.lock());
        if let Some(why) = bringing.progress.failed.lock().take() {
            // What was not copied is where it was, and the song names it
            // there: nothing is lost, and it is tried again at the next save.
            self.say_of_recordings(format!(
                "a recording could not be copied beside the song ({why}): it is where it was, and the song plays \
                 it from there. Saving again tries again"
            ));
        }
        if whole.is_empty() {
            return;
        }
        // The song's file first, as it was saved: nothing but the names.
        let written = rename_in_file(&bringing.song, &whole);
        if let Err(why) = &written {
            log::warn!("the song's file could not be given the recordings' new names: {why}");
        }
        self.rename_everywhere(&whole);
        if written.is_ok() {
            for (old, _) in &whole {
                let _ = std::fs::remove_file(old);
            }
        }
    }

    /// A take has landed. If the song has a place and the take was begun
    /// before it had one, its recording comes beside the song now.
    pub(super) fn bring_what_landed(&mut self) {
        let Some(song) = self.path.clone() else { return };
        if self.recordings_without_a_place().is_empty() {
            return;
        }
        match self.bring_recordings_beside(&song) {
            Ok(brought) => {
                // The song's file never named these: there is nothing in
                // it to wait for. A first name is kept until the next
                // save says where the recording is, in case the program
                // does not live to make it.
                self.keep_first_names(brought.first_names);
                if !brought.to_copy.is_empty() {
                    self.set_bringing(Some(Bringing::start(&song, brought.to_copy)));
                }
            }
            Err(error) => log::warn!("a take could not be brought beside the song: {error}"),
        }
    }
}

/// Gives the song in the file at `song` the new names of `moved`, and
/// nothing else: the file is what was saved, with its recordings named
/// where they now are. Written beside the file and renamed, as a save is.
fn rename_in_file(song: &Path, moved: &[(String, String)]) -> Result<(), String> {
    let text = std::fs::read_to_string(song).map_err(|error| format!("'{}': {error}", song.display()))?;
    let (mut project, table) = trill_core::files::read(&text).map_err(|error| error.to_string())?;
    rename(&mut project, moved);
    // What was known of each file is known of it under its new name.
    let mut table = table;
    for (old, new) in moved {
        table.found_at(old, new);
    }
    let temporary = song.with_extension("tmp");
    std::fs::write(&temporary, trill_core::files::write(&project, &table))
        .and_then(|()| std::fs::rename(&temporary, song))
        .map_err(|error| format!("'{}': {error}", song.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_spellings_of_a_path_are_one_where_the_file_system_has_them_so() {
        let (one, other) = (
            Path::new("C:/Songs/My Song files/takes/Voice.wav"),
            Path::new("c:\\songs\\my song files\\takes\\voice.WAV"),
        );
        assert_eq!(spelled(one) == spelled(other), cfg!(windows));
        assert_eq!(spelled(one), spelled(one));
    }

    #[test]
    fn a_copy_has_its_name_only_when_it_is_whole() {
        let folder = tempfile::tempdir().unwrap();
        let (from, to) = (folder.path().join("from.wav"), folder.path().join("to.wav"));
        std::fs::write(&from, vec![3u8; 3_000_000]).unwrap();
        let done = AtomicU64::new(0);
        copy_whole(&from, &to, &done).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), std::fs::read(&from).unwrap());
        assert_eq!(done.load(Ordering::Relaxed), 3_000_000);
        assert!(!to.with_extension("coming").exists());
        // One that cannot be read leaves nothing under the name.
        let nowhere = folder.path().join("nowhere.wav");
        assert!(copy_whole(&folder.path().join("missing.wav"), &nowhere, &done).is_err());
        assert!(!nowhere.exists() && !nowhere.with_extension("coming").exists());
        assert_eq!(
            ComingBeside { files: 2, done: 40, total: 100 }.said(),
            "Bringing 2 recordings beside the song \u{00B7} 40 %"
        );
    }
}
