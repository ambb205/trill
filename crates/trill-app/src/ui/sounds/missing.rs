//! The song's missing files, as the window keeps up with them.
//!
//! Whether a file is there is a question for the disk, and looking for
//! one that is not reads folders: neither is done while drawing. A worker
//! looks ([`Runtime::missing_files`]) whenever what could change the
//! answer has changed: the files the song names, what the engine says it
//! could not play, the library, where the user has pointed. What it found
//! is kept here for the views: the status bar's line, the list
//! (`ui/missing.rs`), the folder tree of the browser.
//!
//! Putting the song right is the runtime's too ([`Runtime::relink`],
//! [`Runtime::move_library_folder`]), from a worker, as one undo step.

use super::{Done, Sounds};
use crate::ui::snapshot::Snapshot;
use crate::ui::work::Priority;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::files::{FolderMove, Relink};
use trill_runtime::{Confidence, Look, Missing, Runtime};

/// What a look was a look at. Another is due when this is no longer how
/// things stand.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    /// The files the song names and what the engine could not play.
    song: u64,
    /// The library's generation, while it matters.
    library: u64,
    look: Look,
    /// Raised to look again when nothing else has changed.
    again: u64,
}

/// What the window knows of the song's missing files.
pub struct MissingFiles {
    /// What the last look found.
    pub found: Arc<Missing>,
    /// A look is out.
    pub looking: bool,
    /// The list is on screen.
    pub open: bool,
    /// A relink, or the move of a library folder, is under way.
    pub busy: bool,
    /// Whether the library is told where its folder went along with a
    /// relink, when the same move explains both.
    pub with_library: bool,
    /// Where the user has pointed, for as long as the song is open.
    look: Look,
    seen: Option<Seen>,
    /// The snapshot `song` was worked out from, and what it came to.
    revision: Option<u64>,
    song: u64,
    again: u64,
    /// Counts looks sent; only the latest one's answer is kept.
    tickets: u64,
}

impl Default for MissingFiles {
    fn default() -> Self {
        Self {
            found: Arc::default(),
            looking: false,
            open: false,
            busy: false,
            with_library: true,
            look: Look::default(),
            seen: None,
            revision: None,
            song: 0,
            again: 0,
            tickets: 0,
        }
    }
}

impl MissingFiles {
    /// Has the next frame look again, whatever has or has not changed.
    pub fn look_again(&mut self) {
        self.again += 1;
    }

    /// Whether the user has pointed anywhere.
    pub fn has_pointed(&self) -> bool {
        self.look != Look::default()
    }

    /// Takes it that the folder went as `moved` says: the missing files
    /// are looked for there from now on.
    pub fn point_at(&mut self, moved: FolderMove) {
        if !self.look.moves.contains(&moved) {
            self.look.moves.push(moved);
        }
    }
}

/// What a relink did, for telling the user.
pub struct Relinked {
    pub files: usize,
    pub places: usize,
    /// The library folders that went along.
    pub library: Vec<(PathBuf, PathBuf)>,
}

/// A number that changes when the files the song names do, or what the
/// engine says of them.
fn song_signature(snap: &Snapshot) -> u64 {
    let mut hasher = DefaultHasher::new();
    snap.path.hash(&mut hasher);
    for track in &snap.project.tracks {
        for clip in &track.clips {
            if let Some(audio) = clip.audio() {
                audio.path.hash(&mut hasher);
            }
        }
    }
    for device in snap.project.devices() {
        // Only the sample players name files, and only in their state.
        if device.state.is_some() {
            for file in trill_dsp::sample_refs(device) {
                file.path.hash(&mut hasher);
            }
        }
    }
    snap.problems.hash(&mut hasher);
    hasher.finish()
}

impl Sounds {
    /// Keeps what is known of the song's missing files in step with the
    /// song, the library and where the user pointed. Call once per frame,
    /// after the snapshot has caught up. Cheap unless something changed,
    /// and then the looking is a worker's.
    pub fn watch_missing(&mut self, snap: &Snapshot) {
        if self.missing.revision != Some(snap.revision) {
            self.missing.revision = Some(snap.revision);
            self.missing.song = song_signature(snap);
        }
        // The library can only change the answer while something is
        // missing, in the song or of its own folders.
        let library = &self.runtime.library;
        let matters = !self.missing.found.is_empty() || !library.missing_roots().is_empty();
        let now = Seen {
            song: self.missing.song,
            library: if matters { library.generation() } else { 0 },
            look: self.missing.look.clone(),
            again: self.missing.again,
        };
        if self.missing.looking || self.missing.seen.as_ref() == Some(&now) {
            return;
        }
        self.missing.tickets += 1;
        self.missing.looking = true;
        let (ticket, runtime, look) = (self.missing.tickets, self.runtime.clone(), now.look.clone());
        self.missing.seen = Some(now);
        self.work
            .spawn(Priority::Urgent, move || Done::Looked { ticket, missing: Box::new(runtime.missing_files(&look)) });
    }

    /// Takes in what a look found. Returns what to tell the user: that
    /// files are missing, the first time it is seen of each.
    pub(super) fn looked(&mut self, ticket: u64, missing: Missing) -> Option<String> {
        if ticket != self.missing.tickets {
            return None;
        }
        self.missing.looking = false;
        let known = |path: &str| self.missing.found.files.iter().any(|file| file.path == path);
        let news = missing.files.iter().filter(|file| !known(&file.path)).count();
        let told = (news > 0 && !self.missing.open).then(|| announce(&missing));
        self.missing.found = Arc::new(missing);
        told
    }

    /// Relinks every missing file to what was found for it, where that is
    /// at least as sure as `sure`: one undo step. With `with_library`, the
    /// library is told where its own folders went too.
    pub fn relink_found(&mut self, sure: Confidence) {
        let (look, with_library) = (self.missing.look.clone(), self.missing.with_library);
        self.missing.busy = true;
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || {
            // Looked for afresh: what is relinked is what is there now.
            let missing = runtime.missing_files(&look);
            let relinks = missing.relinks(sure);
            Done::FilesRelinked(relink(
                &runtime,
                &relinks,
                if with_library { missing.library_moves() } else { Vec::new() },
            ))
        });
    }

    /// Points every place that names the missing file `old` at `new`: a
    /// file found for it that the user accepted, or one they chose. Where
    /// the two say a folder went is where the other missing files are
    /// looked for from now on.
    pub fn relink_file(&mut self, old: String, new: PathBuf) {
        let new = super::stored_path(&new);
        if let Some((moved, _)) = FolderMove::between(&old, &new) {
            self.missing.point_at(moved);
        }
        self.missing.busy = true;
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || {
            let places = runtime.session.lock().files().into_iter().filter(|file| file.path == old);
            let relinks: Vec<Relink> = places.map(|file| Relink { place: file.place, path: new.clone() }).collect();
            Done::FilesRelinked(relink(&runtime, &relinks, Vec::new()))
        });
    }

    /// Asks where the missing file `old` is now and relinks every place
    /// that names it.
    pub fn locate_file_dialog(&mut self, old: String) {
        if std::mem::replace(&mut self.dialog_open, true) {
            return;
        }
        let start_in = nearest_folder(Path::new(&old));
        let title = format!("Locate {}", super::file_name(Path::new(&old)));
        self.work.ask(move || {
            let file = pollster::block_on(super::audio_dialog(&title, start_in).pick_file());
            Done::FileLocated(file.map(|file| (old, file.path().to_path_buf())))
        });
    }

    /// Asks for a folder to look through for the missing files.
    pub fn look_in_folder_dialog(&mut self) {
        if std::mem::replace(&mut self.dialog_open, true) {
            return;
        }
        let start_in = self.missing.found.files.first().and_then(|file| nearest_folder(Path::new(&file.path)));
        self.work.ask(move || {
            let mut dialog = rfd::AsyncFileDialog::new().set_title("Look for the missing files in a folder");
            if let Some(folder) = start_in {
                dialog = dialog.set_directory(folder);
            }
            Done::LookIn(pollster::block_on(dialog.pick_folder()).map(|folder| folder.path().to_path_buf()))
        });
    }

    /// Has `folder` looked through for the missing files, from now on.
    pub fn look_in(&mut self, folder: PathBuf) {
        if !self.missing.look.folders.contains(&folder) {
            self.missing.look.folders.push(folder);
        }
    }

    /// Tells the library that its folder `from` is at `to`.
    pub fn move_library_folder(&mut self, from: PathBuf, to: PathBuf) {
        self.missing.busy = true;
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || {
            let moved = runtime.move_library_folder(&from, &to).map(|_| (from, to));
            Done::LibraryMoved(moved)
        });
    }

    /// Asks where the library's folder `from` went, and tells the library.
    pub fn locate_library_folder_dialog(&mut self, from: PathBuf) {
        if std::mem::replace(&mut self.dialog_open, true) {
            return;
        }
        let start_in = nearest_folder(&from);
        let name = super::file_name(&from);
        self.work.ask(move || {
            let mut dialog = rfd::AsyncFileDialog::new().set_title(format!("Where is {name} now?"));
            if let Some(folder) = start_in {
                dialog = dialog.set_directory(folder);
            }
            let picked = pollster::block_on(dialog.pick_folder()).map(|folder| folder.path().to_path_buf());
            Done::LibraryLocated(picked.map(|to| (from, to)))
        });
    }
}

/// The nearest folder above `path` that is there: where a dialog about a
/// file that is gone opens.
fn nearest_folder(path: &Path) -> Option<PathBuf> {
    path.ancestors().skip(1).find(|above| above.is_dir()).map(Path::to_path_buf)
}

/// Relinks, and then moves the library's folders: the body of every way
/// of putting the song right.
fn relink(runtime: &Runtime, relinks: &[Relink], library: Vec<(PathBuf, PathBuf)>) -> Result<Relinked, String> {
    let places = runtime.relink(relinks).map_err(|error| error.to_string())?;
    let mut files: Vec<&str> = relinks.iter().map(|relink| relink.path.as_str()).collect();
    files.sort_unstable();
    files.dedup();
    let mut moved = Vec::new();
    // The library goes along with the song's files, not by itself.
    for (from, to) in library.into_iter().filter(|_| places > 0) {
        match runtime.move_library_folder(&from, &to) {
            Ok(_) => moved.push((from, to)),
            Err(error) => log::warn!("the library's folder was not moved: {error}"),
        }
    }
    Ok(Relinked { files: if places == 0 { 0 } else { files.len() }, places, library: moved })
}

fn count(number: usize, one: &str, many: &str) -> String {
    if number == 1 { format!("1 {one}") } else { format!("{number} {many}") }
}

/// What is said, once, when a song turns out to have files missing.
fn announce(missing: &Missing) -> String {
    let files = missing.files.len();
    let found = missing.found(Confidence::Sure);
    let what = format!(
        "{} this song plays {} not on disk",
        count(files, "audio file", "audio files"),
        if files == 1 { "is" } else { "are" }
    );
    match found {
        0 => format!("{what}. Missing files, in the bar below, lists them."),
        found if found == files && files == 1 => {
            format!("{what}; it was found. Missing files, in the bar below, relinks it.")
        }
        found if found == files => format!("{what}; all were found. Missing files, in the bar below, relinks them."),
        found => format!("{what}; {found} were found. Missing files, in the bar below, lists them."),
    }
}

/// What a relink did, in a sentence.
pub fn relinked_words(relinked: &Relinked) -> String {
    let mut words = match (relinked.files, relinked.places) {
        (0, _) => "Nothing was relinked: every place already plays that file".to_owned(),
        (files, places) if files == places => format!("Relinked {}", count(files, "file", "files")),
        (files, places) => {
            format!("Relinked {} in {}", count(files, "file", "files"), count(places, "place", "places"))
        }
    };
    if let [(_, to)] = relinked.library.as_slice() {
        words.push_str(&format!(", and the library now has {}", super::file_name(to)));
    } else if !relinked.library.is_empty() {
        words.push_str(&format!(", and {} library folders with them", relinked.library.len()));
    }
    words
}
