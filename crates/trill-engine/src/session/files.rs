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
use trill_core::files::{self, FilePlace, FileTable, FileUse, KnownFile, Relink};
use trill_core::journal::words::quoted;
use trill_core::{Device, DeviceFiles, DeviceId, EditError, Project};
use trill_dsp::SampleFiles;
use trill_dsp::sampled::loaded::INSTRUMENT_SLOT;

/// What the devices of the open song say of files: the built-in sample
/// players by their saved states ([`SampleFiles`]), and with them the files
/// a device looked for and did not find that its state does not name
/// itself: the recordings a sampled instrument's file names. Those are in
/// the instrument's file, not in the song, for as long as they are where
/// that says; one that is missing is the song's to find, and giving it
/// another place writes that into the device's state.
pub(super) struct SongFiles {
    missing: BTreeMap<DeviceId, Vec<(String, String)>>,
}

impl DeviceFiles for SongFiles {
    fn files(&self, device: &Device) -> Vec<(String, String)> {
        let mut files = SampleFiles.files(device);
        for (slot, path) in self.missing.get(&device.id).into_iter().flatten() {
            if !files.iter().any(|(known, _)| known == slot) {
                files.push((slot.clone(), path.clone()));
            }
        }
        files
    }

    fn relinked(&self, device: &Device, slots: &[(String, String)]) -> Result<Vec<u8>, String> {
        SampleFiles.relinked(device, slots)
    }
}

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
    /// What the devices of the song say of files: see [`SongFiles`].
    pub(super) fn song_files(&self) -> SongFiles {
        let mut missing = BTreeMap::new();
        for (id, meta) in &self.bank.devices {
            let Some(controller) = &meta.controller else { continue };
            let files = controller.lock().missing_files();
            if !files.is_empty() {
                missing.insert(*id, files);
            }
        }
        SongFiles { missing }
    }

    /// Every place the song names a file: its audio clips, the samples of
    /// its sampler and drum racks, the files of its sampled instruments,
    /// and the recordings of those that are not where the instrument's
    /// file says. Whether the files are there is for the caller to find
    /// out.
    pub fn files(&self) -> Vec<FileUse> {
        self.project.files(&self.song_files())
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
        let devices = self.song_files();
        let before = self.files();
        let mut trial = self.project.clone();
        if trial.relink(relinks, &devices)? == 0 {
            return Ok(0);
        }
        // A recording that was missing is in the device's state once it is
        // given a place, so the places are the same before and after.
        let after = trial.files(&devices);
        drop(trial);
        let moved: Vec<(&FileUse, &FileUse)> =
            before.iter().zip(&after).filter(|(was, is)| was.place == is.place && was.path != is.path).collect();
        let mut news: BTreeSet<&str> = BTreeSet::new();
        for (_, is) in &moved {
            // An instrument's own file is text, and is read when the
            // device is made from it; that it is there is all that is
            // asked of it here.
            if matches!(&is.place, FilePlace::Sample { slot, .. } if slot == INSTRUMENT_SLOT) {
                if !Path::new(&is.path).is_file() {
                    return Err(SessionError::Io(format!("there is no file '{}'; nothing was relinked", is.path)));
                }
            } else {
                news.insert(is.path.as_str());
            }
        }
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
        let changed = self.named(&label, |session| session.edit(|project| project.relink(relinks, &devices)))?;
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
        // The files the song itself names: a recording that an
        // instrument's file names is found from that file.
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
