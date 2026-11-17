//! Sampled instruments from the catalogue (`trill-catalogue`): what
//! happens on Trill's side of a fetch, and what a song owes for the ones
//! it plays.
//!
//! Trill fetches nothing here. A set comes into a folder some other way
//! (the program's fetching, or someone who had the files), and then:
//!
//! * [`Runtime::set_arrived`] is told the folder. It checks that what is
//!   there is the set that was read, all of it, and only then makes the
//!   folder it lies in a folder of the library, so that its instruments
//!   are found like any others. It is what the fetching calls when a set
//!   has arrived.
//! * [`Runtime::take_set`] is handed the files in a folder of their own
//!   and brings them into the folder the user chose: copied under another
//!   name, checked, and only then given the set's name. A copy that was
//!   stopped is gone on with; a set that is not what was read is not
//!   kept, and says so.
//! * [`Runtime::owed`] reads the song: for the sampled instruments it
//!   plays, the lines of the makers who ask to be named.
//!
//! An instrument on a disk is known for one of the catalogue's by the
//! checksum of its file, wherever it was put ([`Runtime::catalogue_here`]).

use crate::Runtime;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use trill_catalogue::check::{below, file_sum};
use trill_catalogue::{Catalogue, Fetch, Owed, Set};
use trill_library::{EntryKind, Query};

/// What a folder a set is being copied into is called until it is whole.
const ARRIVING: &str = ".arriving";

/// A set that has arrived and is in the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrived {
    /// The set's id.
    pub set: String,
    /// Where it is.
    pub folder: PathBuf,
    pub files: u32,
    pub bytes: u64,
    /// The files of its instruments.
    pub instruments: Vec<PathBuf>,
    /// The folder that was made a folder of the library for it; `None`
    /// when it lay in one already.
    pub library_folder: Option<PathBuf>,
}

/// The checksum of a file as it was when it was read: of that size, last
/// written then.
struct Summed {
    size: u64,
    modified: Option<SystemTime>,
    sum: String,
}

/// The checksums of instrument files that were read, each for as long as
/// its file is the one that was read.
#[derive(Default)]
pub(crate) struct Known {
    sums: Mutex<HashMap<PathBuf, Summed>>,
}

impl Known {
    /// The checksum of the file at `path`, read now unless the file is the
    /// one that was read before.
    fn sum(&self, path: &Path) -> Option<String> {
        let found = std::fs::metadata(path).ok()?;
        let (size, modified) = (found.len(), found.modified().ok());
        if let Some(known) = self.sums.lock().get(path)
            && (known.size, known.modified) == (size, modified)
        {
            return Some(known.sum.clone());
        }
        let sum = file_sum(path).ok()?;
        self.sums.lock().insert(path.to_path_buf(), Summed { size, modified, sum: sum.clone() });
        Some(sum)
    }
}

/// Every file below `folder` that may be a set's, as paths with forward
/// slashes: not the record a fetch keeps there, nor what was left of a
/// file when one was stopped.
fn files_below(folder: &Path) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut waiting = vec![(folder.to_path_buf(), String::new())];
    while let Some((at, under)) = waiting.pop() {
        let said = |error: std::io::Error| format!("{}: {error}", at.display());
        for entry in std::fs::read_dir(&at).map_err(said)? {
            let entry = entry.map_err(said)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if under.is_empty() { name.clone() } else { format!("{under}/{name}") };
            if entry.file_type().map_err(said)?.is_dir() {
                waiting.push((entry.path(), path));
            } else if path != "part.json" && !name.ends_with(".partial") {
                found.push(path);
            }
        }
    }
    Ok(found)
}

impl Runtime {
    /// The catalogue that is built into the program.
    pub fn catalogue(&self) -> &'static Catalogue {
        Catalogue::shipped()
    }

    /// A set has arrived in `folder`: checks that it is the set that was
    /// read, all of it, and makes the folder it lies in a folder of the
    /// library (unless it lies in one already), which is then looked
    /// through. An error is a sentence, and the library is as it was.
    ///
    /// This reads every file of the set: seconds for a large one. Not on
    /// the thread that draws.
    pub fn set_arrived(&self, set: &Set, folder: &Path) -> Result<Arrived, String> {
        let whole = set.check(folder)?;
        let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
        let inside = self.library.roots().iter().any(|root| folder.starts_with(root));
        let library_folder = match (inside, folder.parent()) {
            (true, _) => {
                self.library.rescan();
                None
            }
            (false, Some(parent)) => {
                // The folder the sets are kept in, so that the next set to
                // arrive there is in the library too.
                if !self.library.add_root(parent) {
                    self.library.rescan();
                }
                Some(parent.to_path_buf())
            }
            (false, None) => return Err(format!("{} is not in a folder", folder.display())),
        };
        if let Err(error) = self.save_library_changes() {
            log::warn!("the library's folders could not be saved: {error}");
        }
        let instruments = set.instruments.iter().map(|instrument| below(&folder, &instrument.file)).collect();
        Ok(Arrived { set: set.id.clone(), folder, files: whole.files, bytes: whole.bytes, instruments, library_folder })
    }

    /// A set was removed from the folder it was kept in: the library
    /// looks again, and no longer lists it.
    pub fn set_removed(&self) {
        self.library.rescan();
    }

    /// Brings a set whose files are in `from` (as its maker lays them
    /// out) into `into`, the folder the user keeps such sets in, as
    /// `into/<the set's id>`, and has it arrive ([`Self::set_arrived`]).
    ///
    /// The files are copied into a folder of another name, which is given
    /// the set's name only when all of it is there and is what was read.
    /// Stopped (`stop` is set, or the program ends), what was copied is
    /// kept, and taking the set again goes on from there. A set that is
    /// not what was read is not kept. A set that is at `into` already,
    /// whole, is only made known to the library.
    pub fn take_set(&self, set: &Set, from: &Path, into: &Path, stop: &AtomicBool) -> Result<Arrived, String> {
        let target = into.join(&set.id);
        if target.is_dir() {
            return match set.check(&target) {
                Ok(_) => self.set_arrived(set, &target),
                Err(why) => Err(format!(
                    "{why}. (It was not taken from {}: move what is at {} away first.)",
                    from.display(),
                    target.display()
                )),
            };
        }
        if !from.is_dir() {
            return Err(format!("{} was to be taken from {}, and there is no such folder", set.name, from.display()));
        }
        let wanted: Vec<String> = match &set.fetch {
            Fetch::Files { files, .. } => files.iter().map(|file| file.path.clone()).collect(),
            Fetch::Archive { .. } => files_below(from)?,
        };
        let staging = into.join(format!("{}{ARRIVING}", set.id));
        std::fs::create_dir_all(&staging).map_err(|error| format!("{}: {error}", staging.display()))?;
        for path in &wanted {
            if stop.load(Ordering::Relaxed) {
                return Err(format!(
                    "Taking {} was stopped. What was copied is kept at {}, and taking it again goes on from there.",
                    set.name,
                    staging.display()
                ));
            }
            let (source, copy) = (below(from, path), below(&staging, path));
            let Ok(found) = std::fs::metadata(&source) else { continue };
            // What a copy that was stopped left whole is not copied again;
            // the check below reads it either way.
            if std::fs::metadata(&copy).is_ok_and(|there| there.is_file() && there.len() == found.len()) {
                continue;
            }
            let said = |error: std::io::Error| format!("{}: {error}", copy.display());
            if let Some(parent) = copy.parent() {
                std::fs::create_dir_all(parent).map_err(said)?;
            }
            // Under another name until all of it is written: a copy that
            // is cut short leaves no file that looks whole.
            let partial = copy.with_file_name(format!(
                "{}.partial",
                copy.file_name().map(|name| name.to_string_lossy()).unwrap_or_default()
            ));
            std::fs::copy(&source, &partial).map_err(said)?;
            std::fs::rename(&partial, &copy).map_err(said)?;
        }
        if let Err(why) = set.check(&staging) {
            // What is not the set that was read is not kept; what is only
            // not all there yet is.
            let kept = if why.contains("are not there") || why.contains("not unpacked to the end") {
                "What was copied is kept, and taking it again from a folder that has all of it goes on from there"
                    .to_string()
            } else {
                let _ = std::fs::remove_dir_all(&staging);
                "Nothing of it was kept".to_string()
            };
            return Err(format!(
                "{}. {kept}.",
                why.replace(&staging.display().to_string(), &from.display().to_string())
            ));
        }
        std::fs::rename(&staging, &target).map_err(|error| format!("{}: {error}", target.display()))?;
        self.set_arrived(set, &target)
    }

    /// The instruments of the catalogue that are in the library, by the
    /// checksum of their file: where each is now. Reads the instrument
    /// files it has not read before; not on the thread that draws.
    pub fn catalogue_here(&self) -> HashMap<String, PathBuf> {
        self.here_of(self.catalogue())
    }

    /// [`Self::catalogue_here`] for a catalogue that is not the program's own.
    pub fn here_of(&self, catalogue: &Catalogue) -> HashMap<String, PathBuf> {
        let found = self.library.search(&Query { kind: Some(EntryKind::Instrument), ..Query::default() });
        let mut here = HashMap::new();
        for entry in found.entries() {
            if let Some(sum) = self.instruments.sum(&entry.path)
                && catalogue.by_text(&sum).is_some()
            {
                here.entry(sum).or_insert_with(|| entry.path.clone());
            }
        }
        here
    }

    /// What the song that is open owes for the sampled instruments it
    /// plays: the lines of the makers who ask to be named, each once with
    /// the tracks it is owed for; the instruments nothing is owed for; and
    /// the instrument files the catalogue does not know, whose terms are
    /// their own.
    pub fn owed(&self) -> Owed {
        self.owed_by(self.catalogue())
    }

    /// [`Self::owed`] by a catalogue that is not the program's own.
    pub fn owed_by(&self, catalogue: &Catalogue) -> Owed {
        // What plays what, read with the session; the files, without it.
        let played: Vec<(String, String)> = {
            let session = self.session.lock();
            let project = session.project();
            let mut played = Vec::new();
            for track in &project.tracks {
                for device in track.devices.iter().filter(|device| device.uri == trill_dsp::sampled::URI) {
                    let state = trill_dsp::sampled::State::from_device_state(device.state.as_deref()).ok();
                    if let Some(file) = state.and_then(|state| state.path) {
                        played.push((track.name.clone(), file));
                    }
                }
            }
            played
        };
        let summed: Vec<(String, String)> = played
            .into_iter()
            .map(|(track, file)| {
                let path = Path::new(&file);
                let sum = self.instruments.sum(path).unwrap_or_default();
                // One the catalogue knows is called by its track. Any other
                // (a file of the user's own, or one that is not there) by
                // its file too: whatever it owes is in its own terms.
                if catalogue.by_text(&sum).is_some() {
                    (track, sum)
                } else {
                    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or(file);
                    (format!("{name}, on {track}"), sum)
                }
            })
            .collect();
        catalogue.owed(summed.iter().map(|(called, sum)| (called.as_str(), sum.as_str())))
    }
}
