//! The files the open song plays: which they are, what is known of them,
//! and giving the song other ones.
//!
//! * [`Session::files`] is every place the song names a file, and
//!   [`Session::relink`] gives places other files as one undo step. Looking
//!   for a file that is gone is not done here: that takes the library and
//!   the disk, and belongs to whoever has them (`trill-runtime`).
//! * [`Session::known_files`] is what was written down about the files when
//!   the song was last saved or opened (`trill_core::files`): their sizes
//!   and lengths, which is what tells a file from one that merely has its
//!   name once it is gone.
//! * A song is saved with that table, and with where each file is from the
//!   song's own folder. When it is opened, **the file beside the song
//!   wins**: one found where that leads from the folder the song is in
//!   now, and that is the file the song knew, is the one it plays. That is
//!   what lets a folder holding a song and its samples be moved or sent
//!   whole.

use super::{Session, SessionError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use trill_core::files::{self, FileTable, FileUse, KnownFile, Relink};
use trill_core::journal::words::quoted;
use trill_core::{EditError, Project};
use trill_dsp::SampleFiles;

/// What the file at `path` is, as far as its size and its header say:
/// `None` if there is no file there. `before`, what was known of it,
/// saves reading the header of a file whose size has not changed.
pub fn file_facts(path: &Path, before: Option<&KnownFile>) -> Option<KnownFile> {
    let found = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file)?;
    let bytes = Some(found.len());
    let named = path.to_string_lossy().into_owned();
    if let Some(before) = before
        && before.bytes == bytes
        && before.sample_rate.is_some()
    {
        return Some(KnownFile { path: named, beside: None, ..before.clone() });
    }
    // A file that is no audio is still a file of that size.
    let header = trill_media::probe(path).ok();
    Some(KnownFile {
        path: named,
        beside: None,
        bytes,
        frames: header.as_ref().and_then(|header| header.frames),
        sample_rate: header.as_ref().map(|header| header.sample_rate),
        channels: header.as_ref().map(|header| header.channels),
    })
}

/// The folder the project file at `song` is in, as text.
fn folder_of_song(song: &Path) -> Option<String> {
    let whole = std::path::absolute(song).ok()?;
    Some(whole.parent()?.to_string_lossy().into_owned())
}

/// Brings a song that has just been read in line with where its files are
/// from `folder`, the folder its project file is in now: a file found
/// where the table says it is from there, and that is the file the song
/// knew, is the one the song plays. Returns how many files that was.
fn take_files_beside(project: &mut Project, table: &mut FileTable, folder: &str) -> usize {
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    for known in table.files() {
        let Some(there) = known.beside.as_deref().and_then(|beside| files::resolve(folder, beside)) else { continue };
        if files::same_path(&there, &known.path) {
            continue;
        }
        // It has to be the file: one of that name that is something else
        // (a newer take, another kick) is not what the song was made with.
        let is_it = file_facts(Path::new(&there), None).and_then(|file| known.is(&file)) == Some(true);
        if is_it {
            found.insert(known.path.clone(), there);
        }
    }
    if found.is_empty() {
        return 0;
    }
    let relinks: Vec<Relink> = project
        .files(&SampleFiles)
        .into_iter()
        .filter_map(|file| found.get(&file.path).map(|there| Relink { place: file.place, path: there.clone() }))
        .collect();
    if let Err(error) = project.relink(&relinks, &SampleFiles) {
        log::warn!("the files beside the song were not taken up: {error}");
        return 0;
    }
    for (was, is) in &found {
        table.found_at(was, is);
    }
    found.len()
}

impl Session {
    /// Every place the song names a file: its audio clips, and the samples
    /// of its sampler and drum racks. Whether the files are there is for
    /// the caller to find out.
    pub fn files(&self) -> Vec<FileUse> {
        self.project.files(&SampleFiles)
    }

    /// What was written down about the song's files when it was last saved
    /// or opened. Empty for a song that has never been saved by a build
    /// that keeps it.
    pub fn known_files(&self) -> &FileTable {
        &self.files
    }

    /// Gives places of the song other files to play: one undo step,
    /// however many places, and one step of the journal ("Relinked 4
    /// files"). Each clip goes on playing the stretch it played and each
    /// device keeps its settings; see [`Project::relink`].
    ///
    /// All of them or none. A place the song does not have, and a file
    /// that cannot be read, are errors that say which, and then nothing is
    /// changed. Returns how many places now play another file.
    ///
    /// The new files are decoded here if nothing has read them yet, which
    /// takes as long as it takes: a front end reads them through
    /// [`Session::media`] first, without the session.
    pub fn relink(&mut self, relinks: &[Relink]) -> Result<usize, SessionError> {
        // Tried on a copy first: what is wrong with the request is said
        // before any file is opened for it.
        let before = self.files();
        let mut trial = self.project.clone();
        if trial.relink(relinks, &SampleFiles)? == 0 {
            return Ok(0);
        }
        let after = trial.files(&SampleFiles);
        drop(trial);
        let moved: Vec<(&FileUse, &FileUse)> =
            before.iter().zip(&after).filter(|(was, is)| was.path != is.path).collect();
        let news: BTreeSet<&str> = moved.iter().map(|(_, is)| is.path.as_str()).collect();
        for path in &news {
            if let Err(error) = self.audio.media().audio(Path::new(path)) {
                return Err(SessionError::Io(format!("{error}; nothing was relinked")));
            }
        }

        let olds: BTreeSet<&str> = moved.iter().map(|(was, _)| was.path.as_str()).collect();
        let label = match (olds.len(), moved.as_slice()) {
            (1, [(_, is), ..]) => format!("Relinked {}", quoted(files::file_name(&is.path))),
            (count, places) if places.len() == count => format!("Relinked {count} files"),
            (count, places) => format!("Relinked {count} files in {} places", places.len()),
        };
        let pairs: Vec<(String, String)> = moved.iter().map(|(was, is)| (was.path.clone(), is.path.clone())).collect();
        let changed = self.named(&label, |session| session.edit(|project| project.relink(relinks, &SampleFiles)))?;
        // What was known of each file is known of it where it is now.
        for (was, is) in &pairs {
            self.files.found_at(was, is);
        }
        Ok(changed)
    }

    /// The text of the project file as it would be written to `song`: the
    /// song, and what is known of its files, each with where it is from
    /// the song's folder. `None` for `song` when the file has no place
    /// yet: then nothing is said of where the files are from it.
    pub(super) fn project_text(&mut self, song: Option<&Path>) -> String {
        let folder = song.and_then(folder_of_song);
        let mut seen = BTreeSet::new();
        let mut table = Vec::new();
        for file in self.project.files(&SampleFiles) {
            if !seen.insert(file.path.clone()) {
                continue;
            }
            let known = self.files.get(&file.path);
            // A file that is gone keeps what was known of it: that is
            // what it will be recognised by.
            let facts = file_facts(Path::new(&file.path), known).or_else(|| known.cloned()).unwrap_or_default();
            let beside = folder.as_deref().and_then(|folder| files::relative_to(folder, &file.path));
            table.push(KnownFile { path: file.path, beside, ..facts });
        }
        let table = FileTable::new(table);
        let text = files::write(&self.project, &table);
        self.files = table;
        text
    }

    /// Reads the text of a project file that lies at `song`: the song, with
    /// the files beside it taken up, and its table. `None` for `song` when
    /// the text is not where the song lives (a safety copy): then the
    /// paths are taken as they stand.
    pub(super) fn read_project(&self, json: &str, song: Option<&Path>) -> Result<(Project, FileTable), EditError> {
        let (mut project, mut table) = files::read(json)?;
        if let Some(folder) = song.and_then(folder_of_song) {
            let taken = take_files_beside(&mut project, &mut table, &folder);
            if taken > 0 {
                log::info!("{taken} file(s) of the song were found beside it, and are played from there");
            }
        }
        Ok((project, table))
    }
}
