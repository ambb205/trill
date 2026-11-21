//! What both front ends do the same way with recorded sound, and what the
//! program remembers of it from one run to the next: how long sound takes
//! to go round a pair of devices, as a measurement found it or as it was
//! put right by hand; and which recordings a song no longer uses, which is
//! a question about every song Trill knows of and not only the open one.
//!
//! That is the machine's and not a song's, and it is true of two devices
//! as they were opened: another buffer, another rate, or taken exclusively
//! is another figure. So it is kept in the settings under all of that
//! ([`RoundTrip`]), and the session is given the one that fits whenever
//! its devices change: by the housekeeping thread, within a moment. A
//! front end that sets one goes through here
//! ([`Runtime::keep_measured_latency`], [`Runtime::keep_latency_adjustment`]),
//! so that it is remembered.

use crate::Runtime;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use trill_engine::{Removed, Session};
use trill_library::{RoundTrip, SaveError, Settings};

/// The folder beside the settings that holds the takes of songs that have
/// not been saved yet.
pub(crate) const RECORDINGS_FOLDER: &str = "recordings";

/// The devices that are open, as a round trip is kept for them: `None`
/// while no input is open, when there is nothing to go round.
fn pair(session: &Session) -> Option<RoundTrip> {
    let input = session.input_status().open?;
    let output = session.audio_info();
    Some(RoundTrip {
        output: output.device.clone().unwrap_or_default(),
        input: input.device,
        exclusive: output.exclusive,
        sample_rate: output.sample_rate.round() as u32,
        buffer_frames: output.buffer_frames,
        measured_ms: None,
        adjust_ms: 0.0,
    })
}

fn same_pair(one: &RoundTrip, other: &RoundTrip) -> bool {
    one.output == other.output
        && one.input == other.input
        && one.exclusive == other.exclusive
        && one.sample_rate == other.sample_rate
        && one.buffer_frames == other.buffer_frames
}

/// Gives the session what is kept for the devices it has open: what was
/// measured for them and what was added by hand, or neither.
pub(crate) fn apply_kept(session: &mut Session, kept: &[RoundTrip]) {
    let Some(now) = pair(session) else { return };
    let found = kept.iter().find(|kept| same_pair(kept, &now));
    let (measured, adjust) = found.map_or((None, 0.0), |kept| (kept.measured_ms, kept.adjust_ms));
    if session.latency_correction() != (measured, adjust) {
        session.set_measured_latency(measured);
        session.set_latency_adjustment(adjust);
    }
}

/// Puts `change` on what is kept for the devices `now`, and lets go of an
/// entry that says nothing any more.
fn keep(settings: &mut Settings, now: RoundTrip, change: impl FnOnce(&mut RoundTrip)) {
    let kept = &mut settings.audio.round_trips;
    let index = kept.iter().position(|kept| same_pair(kept, &now)).unwrap_or_else(|| {
        kept.push(now);
        kept.len() - 1
    });
    change(&mut kept[index]);
    if kept[index].measured_ms.is_none() && kept[index].adjust_ms == 0.0 {
        kept.remove(index);
    }
}

impl Runtime {
    /// Takes what a measurement of the round trip found
    /// ([`trill_engine::Measured::beyond_ms`]) for the devices that are
    /// open, in place of what they say, and remembers it for them; `None`
    /// goes back to what they say. Nothing is kept while no input is open.
    pub fn keep_measured_latency(&self, measured_ms: Option<f64>) -> Result<(), SaveError> {
        let now = {
            let mut session = self.session.lock();
            session.set_measured_latency(measured_ms);
            pair(&session)
        };
        match now {
            Some(now) => self.update_settings(|settings| keep(settings, now, |kept| kept.measured_ms = measured_ms)),
            None => Ok(()),
        }
    }

    /// Adds `adjust_ms` by hand to how long sound is taken to go round the
    /// devices that are open, and remembers it for them.
    pub fn keep_latency_adjustment(&self, adjust_ms: f64) -> Result<(), SaveError> {
        let now = {
            let mut session = self.session.lock();
            session.set_latency_adjustment(adjust_ms);
            pair(&session)
        };
        match now {
            Some(now) => self.update_settings(|settings| keep(settings, now, |kept| kept.adjust_ms = adjust_ms)),
            None => Ok(()),
        }
    }
}

// ---- the recordings a song no longer uses -----------------------------------

/// The recordings in the open song's own folder that it no longer uses,
/// and what there is to say of each before one goes; see
/// [`Runtime::unused_recordings`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnusedRecordings {
    /// The folder that was looked in.
    pub folder: PathBuf,
    /// The ones nothing plays: not the open song or any state undo can
    /// reach, not another song Trill knows of, not unsaved work that can
    /// still be recovered. Each with its size in bytes.
    pub unused: Vec<(PathBuf, u64)>,
    /// The ones the open song has stopped using and another plays: they
    /// stay, and each says who plays it.
    pub played_elsewhere: Vec<PlayedElsewhere>,
    /// Where one that is removed goes: "to the Recycle Bin", "to C:\…\removed".
    pub goes: String,
}

/// A recording the open song no longer uses and another song plays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayedElsewhere {
    pub path: PathBuf,
    pub bytes: u64,
    /// Who plays it, each as a sentence can name it: a song by its name
    /// and its file, unsaved work by its name.
    pub by: Vec<String>,
}

/// A path as two spellings of it are told to be one: Windows minds neither
/// the case of a name nor which way its strokes lean.
fn spelled(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) { text.replace('/', "\\").to_lowercase() } else { text.into_owned() }
}

/// Every file the song in `text`, a project file at `file`, names: as it
/// spells them, and where its table says they lie from the folder the
/// song is in now (a song that was moved with its files plays those).
fn named_by(text: &str, file: Option<&Path>) -> Option<(String, HashSet<String>)> {
    let (project, table) = trill_core::files::read(text).ok()?;
    let mut named: HashSet<String> =
        project.files(&trill_dsp::SampleFiles).iter().map(|used| spelled(Path::new(&used.path))).collect();
    if let Some(folder) = file.and_then(Path::parent) {
        let beside = table.files().iter().filter_map(|known| known.beside.as_ref());
        named.extend(beside.map(|beside| spelled(&folder.join(beside))));
    }
    Some((project.name, named))
}

impl Runtime {
    /// Who else plays each of `paths`: the songs Trill knows of beside the
    /// open one (the ones it opened or saved lately), and the unsaved work
    /// of a program that did not live to save it, which can still be
    /// recovered. Reads those songs' files: not for the thread that draws.
    fn played_elsewhere(&self, paths: &[PathBuf]) -> HashMap<PathBuf, Vec<String>> {
        let mut by: HashMap<PathBuf, Vec<String>> = HashMap::new();
        if paths.is_empty() {
            return by;
        }
        let open = self.session.lock().path().map(Path::to_path_buf);
        let mut songs: Vec<(String, HashSet<String>)> = Vec::new();
        for path in self.settings().recent_projects {
            if open.as_deref().is_some_and(|open| spelled(open) == spelled(&path)) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some((name, named)) = named_by(&text, Some(&path)) else { continue };
            songs.push((format!("'{name}' ({})", path.display()), named));
        }
        for copy in self.recoverable() {
            let Ok(text) = std::fs::read_to_string(&copy.file) else { continue };
            let Some((name, named)) = named_by(&text, None) else { continue };
            songs.push((format!("unsaved work on '{name}' that can still be recovered"), named));
        }
        for path in paths {
            let spelling = spelled(path);
            let players: Vec<String> =
                songs.iter().filter(|(_, named)| named.contains(&spelling)).map(|(who, _)| who.clone()).collect();
            if !players.is_empty() {
                by.insert(path.clone(), players);
            }
        }
        by
    }

    /// The recordings the open song no longer uses, with their sizes: the
    /// ones that can go, and the ones another song plays, which stay.
    /// Trill never removes a recording by itself; this is the list a front
    /// end shows before it asks. It reads the files of the songs Trill
    /// knows of, so it is not for the thread that draws.
    pub fn unused_recordings(&self) -> UnusedRecordings {
        let (folder, candidates, goes) = {
            let session = self.session.lock();
            (session.recordings_folder(), session.unused_recordings(), session.removed_recordings_go())
        };
        let paths: Vec<PathBuf> = candidates.iter().map(|(path, _)| path.clone()).collect();
        let mut elsewhere = self.played_elsewhere(&paths);
        let mut listed = UnusedRecordings { folder, goes, ..UnusedRecordings::default() };
        for (path, bytes) in candidates {
            match elsewhere.remove(&path) {
                Some(by) => listed.played_elsewhere.push(PlayedElsewhere { path, bytes, by }),
                None => listed.unused.push((path, bytes)),
            }
        }
        listed
    }

    /// Removes the recordings among `paths` that nothing plays
    /// ([`Runtime::unused_recordings`]) and says where they went: the
    /// system's Recycle Bin, or a folder beside the takes. None is
    /// unlinked. One that any song plays (the open one, another Trill
    /// knows of, unsaved work that can be recovered) is refused with that
    /// song's name, and then none is removed. A front end asks first; the
    /// assistant's door, which cannot ask, refuses.
    pub fn delete_unused_recordings(&self, paths: &[PathBuf]) -> Result<Removed, String> {
        let elsewhere = self.played_elsewhere(paths);
        let by = |path: &Path| elsewhere.get(path).map(|players| players.join(" and "));
        self.session.lock().delete_unused_recordings(paths, &by).map_err(|error| error.to_string())
    }
}
