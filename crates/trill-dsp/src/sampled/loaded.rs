//! An instrument as a device has it: read from its file, with its
//! recordings in the pool, and with what there is to say about it.
//!
//! # State
//!
//! Which instrument a device plays is the device's `state`, a JSON
//! document like the sampler's:
//!
//! ```json
//! {
//!   "version": 1,
//!   "path": "D:/Instruments/Small Grand/Small Grand.sfz",
//!   "recordings": { "samples/C4_ff.wav": "E:/Elsewhere/C4_ff.wav" }
//! }
//! ```
//!
//! * `path`: the instrument's file. Its recordings are found from there,
//!   as the file names them, so an instrument that is moved whole needs
//!   only this path put right.
//! * `recordings`: the recordings that are somewhere else than the file
//!   says, by the name the file has for them. Left out when there are
//!   none, which is nearly always.
//!
//! Fields this build does not know are kept. A device without a state, or
//! whose file cannot be read, is made all the same and is silent.

use super::model::{Instrument, LoopMode, Summary, Trigger};
use super::pool::{Pool, Recording};
use crate::loading::{SampleRef, StateError, decode, encode};
use crate::sfz;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use trill_core::device::DeviceWarning;
use trill_core::files::{folder_of, resolve};
use trill_core::{Articulation, InstrumentProfile, KeySound, Role, SampledInstrument, SampledRegion};

/// The state format this build writes, and the newest it reads.
pub const STATE_VERSION: u32 = 1;
/// The slot of the instrument's own file.
pub const INSTRUMENT_SLOT: &str = "instrument";
/// What the slot of a recording begins with; the name the instrument's
/// file has for it follows.
pub const RECORDING_SLOT: &str = "recording ";
/// The most recordings an instrument may name. The start of every one is
/// held in memory, a third of a second each: this many are two gigabytes.
pub const MAX_RECORDINGS: usize = 16_384;
/// How many of the things an instrument asks for and does not get are
/// named for each kind of thing.
const NAMED: usize = 4;
/// How many of an instrument's missing recordings are looked for again
/// each time the instrument is asked for.
const LOOKED_FOR_AGAIN: usize = 16;

/// The state document: which instrument a device plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// The instrument's file; `None` for a device with nothing loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Recordings that are elsewhere than the file says: the name the
    /// file has for each, and where it is.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recordings: BTreeMap<String, String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for State {
    fn default() -> Self {
        Self { version: STATE_VERSION, path: None, recordings: BTreeMap::new(), extra: BTreeMap::new() }
    }
}

impl State {
    /// A device playing the instrument in the file at `path`.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: Some(path.into()), ..Self::default() }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StateError> {
        decode(bytes, STATE_VERSION)
    }

    pub fn from_device_state(state: Option<&[u8]>) -> Result<Self, StateError> {
        state.map_or_else(|| Ok(Self::default()), Self::from_bytes)
    }
}

/// The files a stored state names: the instrument's, and the recordings
/// it has elsewhere than the instrument says.
pub(crate) fn sample_refs(state: Option<&[u8]>) -> Result<Vec<SampleRef>, StateError> {
    let state = State::from_device_state(state)?;
    let mut refs: Vec<SampleRef> =
        state.path.into_iter().map(|path| SampleRef { slot: INSTRUMENT_SLOT.to_string(), path }).collect();
    refs.extend(
        state.recordings.into_iter().map(|(name, path)| SampleRef { slot: format!("{RECORDING_SLOT}{name}"), path }),
    );
    Ok(refs)
}

/// The stored state with other files in the given slots: the instrument's
/// own, or a recording by the name the instrument has for it.
pub(crate) fn relinked(state: Option<&[u8]>, slots: &[(String, String)]) -> Result<Vec<u8>, StateError> {
    let mut state = State::from_device_state(state)?;
    for (slot, path) in slots {
        match slot.strip_prefix(RECORDING_SLOT) {
            _ if slot == INSTRUMENT_SLOT && state.path.is_some() => state.path = Some(path.clone()),
            Some(name) if !name.is_empty() && state.path.is_some() => {
                state.recordings.insert(name.to_string(), path.clone());
            }
            _ => return Err(StateError::Invalid(format!("a sampled instrument has no file in '{slot}'"))),
        }
    }
    Ok(state.to_bytes())
}

/// Where the text of instrument files comes from.
pub trait Texts: Send + Sync {
    fn read(&self, path: &Path) -> Result<String, String>;

    /// Something that is another number when the file at `path` has been
    /// changed; `None` if there is no file.
    fn stamp(&self, path: &Path) -> Option<u64>;
}

/// The files of this machine.
pub struct Disk;

impl Texts for Disk {
    fn read(&self, path: &Path) -> Result<String, String> {
        match std::fs::read(path) {
            // Instrument files are not always Unicode; what is not is
            // somebody's name in a comment.
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err("there is no such file".to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn stamp(&self, path: &Path) -> Option<u64> {
        let found = std::fs::metadata(path).ok()?;
        let modified = found.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(found.len() ^ (modified.as_nanos() as u64).rotate_left(17))
    }
}

/// An instrument that was read, with its recordings.
pub struct Loaded {
    /// The instrument's file.
    pub file: String,
    pub instrument: Instrument,
    pub summary: Summary,
    /// Where each of [`Instrument::recordings`] is.
    pub paths: Vec<String>,
    pub recordings: Vec<Arc<Recording>>,
    /// The recordings there is no file for, by their place in the lists.
    pub missing: Vec<u32>,
    /// What its recordings take on the disk, the ones that are there.
    pub bytes: u64,
    /// The regions a key going down may sound, for each key, and the ones
    /// its coming up may.
    pub(crate) down: Vec<Vec<u32>>,
    pub(crate) up: Vec<Vec<u32>>,
    /// Every file its text was read from, with what each was then.
    texts: Vec<(PathBuf, Option<u64>)>,
    overrides: BTreeMap<String, String>,
}

/// The instruments that are loaded, so that every device playing one, and
/// every render of it, has the one copy.
#[derive(Default)]
pub struct Instruments {
    loaded: Mutex<Vec<Weak<Loaded>>>,
}

impl Instruments {
    /// The instrument `state` names, read now unless it is loaded already
    /// and its file has not changed since. An error is a sentence about
    /// the file.
    pub fn load(&self, state: &State, pool: &Pool, texts: &dyn Texts) -> Result<Option<Arc<Loaded>>, String> {
        let Some(file) = &state.path else { return Ok(None) };
        let mut loaded = self.loaded.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loaded.retain(|known| known.strong_count() > 0);
        let known = loaded.iter().filter_map(Weak::upgrade).find(|known| {
            known.file == *file
                && known.overrides == state.recordings
                && known.texts.iter().all(|(path, stamp)| texts.stamp(path) == *stamp)
                // A recording that was missing and is back is a reason to
                // look again; the first few say whether a folder is.
                && !known.missing.iter().take(LOOKED_FOR_AGAIN).any(|index| {
                    pool.loader().size(&known.paths[*index as usize]).is_some()
                })
        });
        if let Some(known) = known {
            return Ok(Some(known));
        }
        let fresh = Arc::new(Loaded::read(file, &state.recordings, pool, texts)?);
        loaded.push(Arc::downgrade(&fresh));
        Ok(Some(fresh))
    }
}

impl Loaded {
    fn read(file: &str, overrides: &BTreeMap<String, String>, pool: &Pool, texts: &dyn Texts) -> Result<Self, String> {
        let path = Path::new(file);
        if !sfz::is_sfz(path) {
            return Err(format!("'{file}' is not an instrument this reads: it reads SFZ files (.sfz)"));
        }
        let read_files = Mutex::new(Vec::new());
        let source = |path: &Path| {
            let text = texts.read(path);
            if text.is_ok() {
                read_files.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(path.to_path_buf());
            }
            text
        };
        let instrument = sfz::read(path, &source).map_err(|reason| format!("cannot read '{file}': {reason}"))?;
        if instrument.recordings.len() > MAX_RECORDINGS {
            return Err(format!(
                "'{file}' names {} recordings, and an instrument may name {MAX_RECORDINGS} at most; none of it is \
                 loaded",
                instrument.recordings.len()
            ));
        }
        let folder = folder_of(file);
        let paths: Vec<String> = instrument
            .recordings
            .iter()
            .map(|name| match overrides.get(name) {
                Some(elsewhere) => elsewhere.clone(),
                None if is_rooted(name) => name.clone(),
                None => resolve(folder, name).unwrap_or_else(|| format!("{folder}/{name}")),
            })
            .collect();
        let (mut missing, mut bytes) = (Vec::new(), 0);
        for (index, path) in paths.iter().enumerate() {
            match pool.loader().size(path) {
                Some(size) => bytes += size,
                None => missing.push(index as u32),
            }
        }
        let recordings = paths.iter().map(|path| pool.recording(path)).collect();
        let (mut down, mut up) = (vec![Vec::new(); 128], vec![Vec::new(); 128]);
        for (index, region) in instrument.regions.iter().enumerate() {
            let list = if region.on_key_down() { &mut down } else { &mut up };
            for key in region.keys.0.min(127)..=region.keys.1.min(127) {
                list[usize::from(key)].push(index as u32);
            }
        }
        let files = read_files.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(Self {
            file: file.to_string(),
            summary: instrument.summary(),
            instrument,
            paths,
            recordings,
            missing,
            bytes,
            down,
            up,
            texts: files
                .into_iter()
                .map(|path| {
                    let stamp = texts.stamp(&path);
                    (path, stamp)
                })
                .collect(),
            overrides: overrides.clone(),
        })
    }

    /// How many recordings have their start in memory or are known to be
    /// unreadable, and how many there are.
    pub fn settled(&self) -> (usize, usize) {
        (self.recordings.iter().filter(|recording| recording.is_settled()).count(), self.recordings.len())
    }

    /// The recordings there is no file for: the slot each is in, and where
    /// it was looked for.
    pub fn missing_files(&self) -> Vec<(String, String)> {
        self.missing
            .iter()
            .map(|index| {
                let index = *index as usize;
                (format!("{RECORDING_SLOT}{}", self.instrument.recordings[index]), self.paths[index].clone())
            })
            .collect()
    }

    /// What is wrong with the instrument that it plays in spite of.
    pub fn warnings(&self) -> Vec<DeviceWarning> {
        let mut warnings = Vec::new();
        if let Some(first) = self.missing.first() {
            let (count, all) = (self.missing.len(), self.recordings.len());
            let first = &self.paths[*first as usize];
            let message = match count {
                1 => format!("{}: its recording '{first}' is missing; the notes it plays are silent", self.summary.name),
                _ => format!(
                    "{}: {count} of its {all} recordings are missing, the first '{first}'; the notes they play are \
                     silent",
                    self.summary.name
                ),
            };
            warnings.push(DeviceWarning { slot: Some("recordings".to_string()), message });
        }
        for problem in &self.instrument.problems {
            let message = format!("{}: {problem}", self.summary.name);
            warnings.push(DeviceWarning { slot: Some(INSTRUMENT_SLOT.to_string()), message });
        }
        warnings
    }

    /// What the instrument asks for that this player does not do, in a
    /// sentence that names it and says what a listener misses. `None` for
    /// an instrument that is played as its maker meant.
    pub fn unsupported(&self) -> Option<String> {
        let unsupported = &self.instrument.unsupported;
        if unsupported.is_empty() {
            return None;
        }
        // By what is missed, in the order it was first met.
        let mut kinds: Vec<(&str, Vec<&str>)> = Vec::new();
        for un in unsupported {
            match kinds.iter_mut().find(|(miss, _)| *miss == un.miss) {
                Some((_, names)) => names.push(&un.name),
                None => kinds.push((un.miss, vec![&un.name])),
            }
        }
        let parts: Vec<String> = kinds
            .iter()
            .map(|(miss, names)| {
                let shown: Vec<&str> = names.iter().take(NAMED).copied().collect();
                let more = match names.len().saturating_sub(NAMED) {
                    0 => String::new(),
                    more => format!(" and {more} more"),
                };
                format!("{miss} ({}{more})", shown.join(", "))
            })
            .collect();
        let (count, plural) = (unsupported.len(), if unsupported.len() == 1 { "" } else { "s" });
        Some(format!(
            "It asks for {count} thing{plural} this player does not do, so it will not sound quite as in its \
             maker's player. Not heard: {}. Everything else plays.",
            parts.join("; ")
        ))
    }

    /// What a track that plays through the instrument is told of its
    /// notes: the range, whether the keys are drums, the ways of playing
    /// chosen by a key, and how far the wheel bends.
    pub fn profile(&self) -> InstrumentProfile {
        let summary = &self.summary;
        let mut profile = InstrumentProfile::default();
        if summary.drums {
            profile.role = Some(Role::Drums);
        }
        let switch_keys: Vec<u8> = summary.switches.iter().map(|(key, _)| *key).collect();
        let default = summary.default_switch.or_else(|| switch_keys.first().copied());
        let mut used: Vec<String> = Vec::new();
        for (key, label) in &summary.switches {
            // A switch without a name is called by its key; one whose name
            // another has too takes its key along.
            let named = trill_core::pitch::name_key(label);
            let plain = named.chars().any(|c| c.is_ascii_alphabetic())
                && named.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && named.len() <= 40;
            let mut name = if plain { named } else { format!("switch_{key}") };
            if used.contains(&name) {
                name = format!("{name}_{key}");
            }
            used.push(name.clone());
            let articulation = Articulation::key(&name, *key);
            profile.articulations.push(if default == Some(*key) { articulation.as_default() } else { articulation });
        }
        // A range that the switches lie inside says nothing a track can
        // keep: a key either switches or plays.
        profile.range = summary.range.filter(|(low, high)| !switch_keys.iter().any(|key| (low..=high).contains(&key)));
        profile.bend_range = summary.bend.map(f64::from).filter(|bend| *bend > 0.0 && *bend != 2.0);
        profile
    }

    /// What is on each key, for an instrument whose keys are drums: the
    /// name the instrument gives the key, or failing that the name of the
    /// recording it plays.
    pub fn key_sounds(&self) -> Option<Vec<KeySound>> {
        if !self.summary.drums {
            return None;
        }
        let mut sounds: Vec<KeySound> = Vec::new();
        for key in 0..128u8 {
            let Some(region) = self.down[usize::from(key)].first().map(|index| &self.instrument.regions[*index as usize])
            else {
                continue;
            };
            let place = match self.instrument.key_labels.get(&key) {
                Some(label) => label.clone(),
                None => match region.recording {
                    Some(index) => recording_words(&self.instrument.recordings[index as usize]),
                    None => continue,
                },
            };
            sounds.push(KeySound { key, place, instrument: String::new(), stroke: String::new() });
        }
        Some(sounds)
    }

    /// What the device says of the instrument: see [`SampledInstrument`].
    pub fn said(&self) -> SampledInstrument {
        SampledInstrument {
            name: self.summary.name.clone(),
            file: self.file.clone(),
            about: self.about(),
            unsupported: self.unsupported(),
            bytes: self.bytes,
            recordings: self.recordings.len(),
            layers: self.summary.layers,
            takes: self.summary.takes,
            regions: self.boxes(),
            profile: self.profile(),
        }
    }

    /// The regions as a picture shows them.
    pub fn boxes(&self) -> Vec<SampledRegion> {
        self.instrument
            .regions
            .iter()
            .filter(|region| region.recording.is_some())
            .map(|region| SampledRegion {
                keys: region.keys,
                velocities: region.velocities,
                root: region.root,
                switch: region.switched.last.map(|(key, _)| key),
                release: matches!(region.trigger, Trigger::Release | Trigger::ReleaseKey),
            })
            .collect()
    }

    /// What the instrument is, in a few sentences for whoever writes for
    /// it: what `describe` and the reply to loading it say.
    pub fn about(&self) -> String {
        let summary = &self.summary;
        let mut text = format!("{}: ", summary.name);
        let megabytes = self.bytes as f64 / (1024.0 * 1024.0);
        let size = if megabytes >= 1024.0 {
            format!("{:.1} GB", megabytes / 1024.0)
        } else if megabytes >= 10.0 {
            format!("{megabytes:.0} MB")
        } else {
            format!("{megabytes:.1} MB")
        };
        text.push_str(&format!("{} recordings ({size}) in {} regions", summary.recordings, summary.regions));
        if let Some((low, high)) = summary.range {
            let name = trill_core::pitch::pitch_name;
            let what = if summary.drums { "drums on keys" } else { "range" };
            text.push_str(&format!(", {what} {}..{} ({low}..{high})", name(low), name(high)));
        }
        text.push_str(&match (summary.layers, summary.takes) {
            (0 | 1, 0 | 1) => ", one recording a note".to_string(),
            (layers, 0 | 1) => format!(", {layers} layers of loudness"),
            (0 | 1, takes) => format!(", {takes} takes of a note, taken in turn or at random"),
            (layers, takes) => format!(", up to {layers} layers of loudness and {takes} takes of a note"),
        });
        text.push('.');
        if summary.drums {
            text.push_str(" Its keys are drums: the track's drum names are the kit's.");
        }
        if !summary.switches.is_empty() {
            let profile = self.profile();
            let ways: Vec<String> = profile
                .articulations
                .iter()
                .map(|way| {
                    let key = way.key.map(trill_core::pitch::pitch_name).unwrap_or_default();
                    format!("{}{} on {key}", way.name, if way.default { " (default)" } else { "" })
                })
                .collect();
            text.push_str(&format!(" Ways of playing, chosen by a key: {}.", ways.join(", ")));
        }
        if !summary.controllers.is_empty() {
            let named: Vec<String> = summary
                .controllers
                .iter()
                .map(|(number, label)| if label.is_empty() { format!("{number}") } else { format!("{number} ({label})") })
                .collect();
            text.push_str(&format!(" It listens to controllers {}.", named.join(", ")));
        }
        let mut has: Vec<&str> = Vec::new();
        if self.instrument.pedal {
            has.push("the sustain pedal holds its notes");
        }
        if summary.releases {
            has.push("it has sounds of its own for a key coming up");
        }
        let looped = self.instrument.regions.iter().any(|region| {
            matches!(region.loop_mode, Some(LoopMode::Continuous | LoopMode::Sustain) | None) && !summary.drums
        });
        if summary.loops {
            has.push("held notes loop");
        } else if looped {
            has.push("a held note lasts as long as its recording, unless the recording says where it loops");
        }
        if !has.is_empty() {
            let mut sentence = has.join("; ");
            if let Some(first) = sentence.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            text.push_str(&format!(" {sentence}."));
        }
        if let Some(unsupported) = self.unsupported() {
            text.push(' ');
            text.push_str(&unsupported);
        }
        text
    }
}

/// Whether a recording's name is a whole path already.
fn is_rooted(name: &str) -> bool {
    name.starts_with('/') || name.as_bytes().get(1) == Some(&b':')
}

/// The name of a recording's file as words: `Drums/Kick_01.wav` is
/// "Kick 01".
fn recording_words(name: &str) -> String {
    let file = name.rsplit('/').next().unwrap_or(name);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    stem.replace(['_', '-'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{MemoryLoader, MemoryTexts};

    pub(crate) const PIANO: &str = "<control> default_path=samples/ label_cc64=Pedal\n\
        <global> ampeg_release=0.4 eq1_gain=2 eq1_freq=100 <effect> reverb=1\n\
        <group> lovel=1 hivel=80\n<region> sample=c3_p.wav lokey=46 hikey=50 pitch_keycenter=48\n\
        <region> sample=g3_p.wav lokey=51 hikey=57 pitch_keycenter=55\n\
        <group> lovel=81 hivel=127\n<region> sample=c3_f.wav lokey=46 hikey=50 pitch_keycenter=48\n\
        <region> sample=g3_f.wav lokey=51 hikey=57 pitch_keycenter=55 locc64=0 hicc64=63\n\
        <group> trigger=release <region> sample=../noise/damper.wav lokey=46 hikey=57";

    /// A pool whose loader has `files`, each a ramp of that many frames,
    /// and texts that have `texts`.
    pub(crate) fn sources(files: &[(&str, usize)], texts: &[(&str, &str)]) -> (Pool, MemoryTexts) {
        let loader = MemoryLoader::default();
        for (path, frames) in files {
            loader.insert(path, (0..*frames).map(|frame| (frame % 100) as f32 / 100.0).collect(), 48_000.0);
        }
        let memory = MemoryTexts::default();
        for (path, text) in texts {
            memory.insert(path, text);
        }
        (Pool::new(Arc::new(loader)), memory)
    }

    fn piano_files() -> Vec<(&'static str, usize)> {
        vec![
            ("D:/Inst/Grand/samples/c3_p.wav", 40_000),
            ("D:/Inst/Grand/samples/g3_p.wav", 40_000),
            ("D:/Inst/Grand/samples/c3_f.wav", 40_000),
            ("D:/Inst/Grand/noise/damper.wav", 2_000),
        ]
    }

    #[test]
    fn a_state_names_the_instrument_and_the_recordings_that_are_elsewhere() {
        let state = State::new("D:/Inst/Grand/Grand.sfz");
        let bytes = state.to_bytes();
        assert_eq!(State::from_bytes(&bytes).unwrap(), state);
        assert!(!String::from_utf8(bytes.clone()).unwrap().contains("recordings"), "nothing is said of none");
        assert_eq!(State::from_device_state(None).unwrap(), State::default());
        assert_eq!(
            sample_refs(Some(&bytes)).unwrap(),
            [SampleRef { slot: "instrument".into(), path: "D:/Inst/Grand/Grand.sfz".into() }]
        );
        assert!(sample_refs(None).unwrap().is_empty());

        // The instrument is somewhere else; and one of its recordings is.
        let slots = [("instrument".to_string(), "E:/Grand/Grand.sfz".to_string())];
        let moved = relinked(Some(&bytes), &slots).unwrap();
        let slots = [("recording samples/c3_f.wav".to_string(), "F:/c3_f.wav".to_string())];
        let moved = relinked(Some(&moved), &slots).unwrap();
        assert_eq!(
            sample_refs(Some(&moved)).unwrap(),
            [
                SampleRef { slot: "instrument".into(), path: "E:/Grand/Grand.sfz".into() },
                SampleRef { slot: "recording samples/c3_f.wav".into(), path: "F:/c3_f.wav".into() }
            ]
        );
        for slot in ["sample", "recording ", "pad 36"] {
            let error = relinked(Some(&bytes), &[(slot.to_string(), "x".to_string())]).unwrap_err();
            assert!(error.to_string().contains(&format!("no file in '{slot}'")), "{error}");
        }
        assert!(relinked(None, &[("instrument".to_string(), "x.sfz".to_string())]).is_err(), "nothing is loaded");
        // What this build does not know is kept, and a newer document is left alone.
        let newer = br#"{"version": 1, "path": "a.sfz", "tuning": "stretched"}"#;
        let rewritten: serde_json::Value =
            serde_json::from_slice(&relinked(Some(newer), &[("instrument".into(), "b.sfz".into())]).unwrap()).unwrap();
        assert_eq!((&rewritten["tuning"], &rewritten["path"]), (&"stretched".into(), &"b.sfz".into()));
        assert!(matches!(State::from_bytes(br#"{"version": 9}"#), Err(StateError::TooNew { .. })));
    }

    #[test]
    fn an_instrument_is_read_once_and_says_what_it_is_and_what_is_missing() {
        let (pool, texts) = sources(&piano_files(), &[("D:/Inst/Grand/Grand.sfz", PIANO)]);
        let instruments = Instruments::default();
        let state = State::new("D:/Inst/Grand/Grand.sfz");
        let loaded = instruments.load(&state, &pool, &texts).unwrap().unwrap();
        assert_eq!(loaded.summary.name, "Grand");
        assert_eq!(loaded.paths[0], "D:/Inst/Grand/samples/c3_p.wav");
        assert_eq!(loaded.paths[4], "D:/Inst/Grand/noise/damper.wav", "a path that steps up is followed");
        assert_eq!(loaded.missing, [3], "g3_f.wav is not there");
        assert_eq!(loaded.bytes, (3 * 40_000 + 2_000) * 8);
        assert_eq!(
            loaded.missing_files(),
            [("recording samples/g3_f.wav".to_string(), "D:/Inst/Grand/samples/g3_f.wav".to_string())]
        );
        let warnings = loaded.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("its recording 'D:/Inst/Grand/samples/g3_f.wav' is missing"));
        assert_eq!(loaded.down[48], [0, 2]);
        assert_eq!((loaded.down[60].len(), loaded.up[48].as_slice()), (0, &[4u32][..]));

        // The same state is the same instrument, for a second device and for a render.
        let again = instruments.load(&state, &pool, &texts).unwrap().unwrap();
        assert!(Arc::ptr_eq(&loaded, &again));
        // Until its file changes, or a recording is said to be elsewhere.
        texts.insert("D:/Inst/Grand/Grand.sfz", &PIANO.replace("ampeg_release=0.4", "ampeg_release=0.9"));
        let changed = instruments.load(&state, &pool, &texts).unwrap().unwrap();
        assert!(!Arc::ptr_eq(&loaded, &changed));
        assert_eq!(changed.instrument.regions[0].envelope.release, 0.9);
        let mut elsewhere = state.clone();
        elsewhere.recordings.insert("samples/g3_f.wav".into(), "D:/Inst/Grand/samples/c3_f.wav".into());
        let found = instruments.load(&elsewhere, &pool, &texts).unwrap().unwrap();
        assert!(found.missing.is_empty() && found.warnings().is_empty());
        assert!(Arc::ptr_eq(&found.recordings[3], &found.recordings[2]), "the one file is one recording");

        // What cannot be loaded says why, and nothing loaded is nothing.
        assert!(instruments.load(&State::default(), &pool, &texts).unwrap().is_none());
        let error = instruments.load(&State::new("D:/Inst/none.sfz"), &pool, &texts).err().unwrap();
        assert_eq!(error, "cannot read 'D:/Inst/none.sfz': there is no such file");
        let error = instruments.load(&State::new("D:/Inst/Grand.nki"), &pool, &texts).err().unwrap();
        assert!(error.contains("it reads SFZ files"), "{error}");
    }

    #[test]
    fn what_an_instrument_asks_for_and_does_not_get_is_said_in_a_sentence() {
        let (pool, texts) = sources(&piano_files(), &[("D:/Inst/Grand/Grand.sfz", PIANO)]);
        let loaded = Instruments::default().load(&State::new("D:/Inst/Grand/Grand.sfz"), &pool, &texts).unwrap().unwrap();
        let sentence = loaded.unsupported().unwrap();
        assert_eq!(
            sentence,
            "It asks for 3 things this player does not do, so it will not sound quite as in its maker's player. \
             Not heard: effects of its own (a reverb, a chorus) (<effect>); equalisers of its own (eq1_freq, \
             eq1_gain). Everything else plays."
        );
        let about = loaded.about();
        assert!(about.starts_with("Grand: 5 recordings (0.9 MB) in 5 regions, range A#2..A3 (46..57), 2 layers of loudness."), "{about}");
        assert!(about.contains("It listens to controllers 64 (Pedal)."), "{about}");
        assert!(about.contains("The sustain pedal holds its notes; it has sounds of its own for a key coming up"), "{about}");
        assert!(about.ends_with("Everything else plays."), "{about}");
        let profile = loaded.profile();
        assert_eq!((profile.range, profile.role, profile.bend_range), (Some((46, 57)), None, None));
        assert!(loaded.key_sounds().is_none(), "a piano's keys are not drums");
        assert_eq!(loaded.boxes().len(), 5);
        assert!(loaded.boxes()[4].release && loaded.boxes()[0].keys == (46, 50));
    }

    #[test]
    fn a_kit_names_its_keys_and_an_instrument_with_switches_its_ways_of_playing() {
        let kit = "<control> label_key38=Snare Center\n<global> loop_mode=one_shot\n\
            <region> key=36 sample=Kick_01.wav\n<region> key=38 sample=sn-a.wav\n<region> key=42 sample=hats/HH closed.wav";
        let guitar = "<global> sw_lokey=24 sw_hikey=26 sw_default=24 bend_up=1200 bend_down=-1200\n\
            <group> sw_last=24 sw_label=Open <region> sample=o.wav lokey=40 hikey=80\n\
            <group> sw_last=25 sw_label=Palm Mute <region> sample=m.wav lokey=40 hikey=80\n\
            <group> sw_last=26 <region> sample=h.wav lokey=40 hikey=80";
        let (pool, texts) = sources(&[], &[("/i/Kit.sfz", kit), ("/i/Guitar.sfz", guitar)]);
        let instruments = Instruments::default();
        let kit = instruments.load(&State::new("/i/Kit.sfz"), &pool, &texts).unwrap().unwrap();
        assert_eq!(kit.profile().role, Some(Role::Drums));
        let sounds: Vec<(u8, String)> = kit.key_sounds().unwrap().into_iter().map(|sound| (sound.key, sound.place)).collect();
        assert_eq!(sounds, [(36, "Kick 01".to_string()), (38, "Snare Center".to_string()), (42, "HH closed".to_string())]);
        assert!(kit.about().contains("drums on keys C2..F#2 (36..42)"), "{}", kit.about());
        assert_eq!(kit.missing.len(), 3);
        assert!(kit.warnings()[0].message.contains("3 of its 3 recordings are missing, the first '/i/Kick_01.wav'"));

        let guitar = instruments.load(&State::new("/i/Guitar.sfz"), &pool, &texts).unwrap().unwrap();
        let profile = guitar.profile();
        let ways: Vec<(&str, Option<u8>, bool)> =
            profile.articulations.iter().map(|way| (way.name.as_str(), way.key, way.default)).collect();
        assert_eq!(ways, [("open", Some(24), true), ("palm_mute", Some(25), false), ("switch_26", Some(26), false)]);
        assert_eq!((profile.range, profile.bend_range), (Some((40, 80)), Some(12.0)));
        assert!(profile.clone().validated().is_ok(), "a track can keep it");
        assert!(guitar.about().contains("Ways of playing, chosen by a key: open (default) on C1, palm_mute on C#1, switch_26 on D1."), "{}", guitar.about());
    }
}
