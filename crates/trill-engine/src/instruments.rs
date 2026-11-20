//! What is known about instruments, by device type: which keys are drums
//! or keyswitches, where the range ends, which octave a key sounds at.
//!
//! Finding such a fact about a plugin takes listening: playing a key and
//! measuring what comes out. Once found it holds for every song, so it is
//! kept here and given to a track ([`InstrumentProfile`]) when the track is
//! given that instrument.
//!
//! The knowledge comes from two places. Some of it ships with the program
//! ([`known`]): entries for plugins that were measured, matched by the
//! plugin's id. The rest is what was learned on this machine, one small
//! text file per device type in a folder beside the user's settings. What
//! was learned wins. A store that was given no folder keeps what it learns
//! in memory, for as long as the program runs: what a test or a scratch
//! session wants.
//!
//! One thing is not known by device type at all: which drum is on which
//! key of a drum plugin, since that is the kit's doing and kits differ.
//! Where the plugin says what its kit has on its keys, the names come
//! from that ([`kit`]), each time they are asked for.

pub mod kit;
mod known;

use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use trill_core::{Channel, InstrumentProfile, KnownInstrument as Entry};

/// The extension of a learned profile's file.
const EXTENSION: &str = "json";

/// Where a profile comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// It ships with the program.
    BuiltIn,
    /// It was learned on this machine.
    Learned,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::BuiltIn => "built_in",
            Source::Learned => "learned",
        }
    }
}

/// What is known about one device type.
#[derive(Clone, Debug, PartialEq)]
pub struct Known {
    pub uri: String,
    /// What the device is called, for listing.
    pub device: String,
    pub profile: InstrumentProfile,
    pub source: Source,
}

/// How remembering a profile came out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remembered {
    /// Whether something had been learned about the device type before,
    /// which this replaced.
    pub replaced: bool,
    /// The file it was written to; `None` when nothing is kept on disk.
    pub file: Option<PathBuf>,
}

/// What is known about instruments. See the module documentation.
#[derive(Default)]
pub struct InstrumentKnowledge {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Where what is learned is kept; `None` keeps it in memory.
    folder: Option<PathBuf>,
    /// By device type.
    learned: BTreeMap<String, Entry>,
}

impl InstrumentKnowledge {
    /// Keeps what is learned in `folder` from now on, and reads what is
    /// there. What was learned in memory before is forgotten. Files that
    /// cannot be read are passed over, with a line in the log.
    pub fn open(&self, folder: PathBuf) {
        let learned = read_folder(&folder);
        let mut inner = self.inner.lock();
        inner.folder = Some(folder);
        inner.learned = learned;
    }

    /// Where what is learned is kept, if on disk.
    pub fn folder(&self) -> Option<PathBuf> {
        self.inner.lock().folder.clone()
    }

    /// What is known about the device type `uri`: what was learned here,
    /// or else what ships with the program.
    pub fn of(&self, uri: &str) -> Option<Known> {
        if let Some(entry) = self.inner.lock().learned.get(uri) {
            return Some(Known {
                uri: uri.to_string(),
                device: entry.device.clone(),
                profile: entry.profile.clone(),
                source: Source::Learned,
            });
        }
        Self::built_in(uri)
    }

    /// Whether what the channels of the device type `uri` mean follows a
    /// setting of the instrument itself: a guitar that has a string to a
    /// channel while a switch of its is on. What they are is then read
    /// from the instrument ([`channels_now`](Self::channels_now)), and is
    /// no part of the profile a track is given.
    pub fn channels_follow_a_setting(uri: &str) -> bool {
        known::channels_now(uri, &|_| None).is_some()
    }

    /// What the device type `uri` calls its MIDI channels as it is set
    /// now, `value` being what one of its parameters is at, by the name
    /// the instrument gives the parameter. Empty for an instrument that
    /// is set to play every channel alike, and for a device type whose
    /// channels follow no setting.
    pub fn channels_now(uri: &str, value: &dyn Fn(&str) -> Option<f64>) -> BTreeMap<Channel, String> {
        known::channels_now(uri, value).unwrap_or_default()
    }

    /// What ships with the program about the device type `uri`, whatever
    /// was learned since.
    pub fn built_in(uri: &str) -> Option<Known> {
        known::entry(uri).map(|(device, profile)| Known {
            uri: uri.to_string(),
            device: device.to_string(),
            profile,
            source: Source::BuiltIn,
        })
    }

    /// Every device type something is known about, by name: the learned
    /// ones, and the built-in ones nothing was learned over.
    pub fn all(&self) -> Vec<Known> {
        let inner = self.inner.lock();
        let mut all: Vec<Known> = inner
            .learned
            .values()
            .map(|entry| Known {
                uri: entry.uri.clone(),
                device: entry.device.clone(),
                profile: entry.profile.clone(),
                source: Source::Learned,
            })
            .collect();
        all.extend(known::uris().filter(|uri| !inner.learned.contains_key(*uri)).filter_map(Self::built_in));
        all.sort_by(|a, b| a.device.to_lowercase().cmp(&b.device.to_lowercase()).then_with(|| a.uri.cmp(&b.uri)));
        all
    }

    /// Remembers `profile` as what is known about the device type `uri`,
    /// called `device`, in place of what was learned about it before.
    pub fn remember(&self, uri: &str, device: &str, profile: InstrumentProfile) -> Result<Remembered, String> {
        let profile = profile.validated().map_err(|error| error.to_string())?;
        if uri.trim().is_empty() {
            return Err("a profile is remembered for a device type, which needs a URI".to_string());
        }
        let entry = Entry { uri: uri.to_string(), device: device.to_string(), profile };
        let mut inner = self.inner.lock();
        let file = match &inner.folder {
            Some(folder) => Some(write(folder, &entry)?),
            None => None,
        };
        let replaced = inner.learned.insert(uri.to_string(), entry).is_some();
        Ok(Remembered { replaced, file })
    }

    /// Forgets what was learned about the device type `uri`, so that what
    /// ships with the program, if anything, is what is known again.
    /// Returns whether there was something to forget.
    pub fn forget(&self, uri: &str) -> Result<bool, String> {
        let mut inner = self.inner.lock();
        if !inner.learned.contains_key(uri) {
            return Ok(false);
        }
        if let Some(folder) = &inner.folder {
            let file = file_of(folder, uri);
            if file.exists() {
                std::fs::remove_file(&file).map_err(|error| format!("{}: {error}", file.display()))?;
            }
        }
        inner.learned.remove(uri);
        Ok(true)
    }
}

/// The file a device type's learned profile is kept in: its URI with
/// everything a file name cannot hold replaced.
fn file_of(folder: &Path, uri: &str) -> PathBuf {
    let name: String = uri.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    folder.join(format!("{name}.{EXTENSION}"))
}

fn write(folder: &Path, entry: &Entry) -> Result<PathBuf, String> {
    let failed = |path: &Path, error: std::io::Error| format!("{}: {error}", path.display());
    std::fs::create_dir_all(folder).map_err(|error| failed(folder, error))?;
    let file = file_of(folder, &entry.uri);
    let text = entry.to_json();
    // Written beside and moved into place, so that a failure half way
    // leaves the old file.
    let partial = file.with_extension("partial");
    std::fs::write(&partial, text).map_err(|error| failed(&partial, error))?;
    std::fs::rename(&partial, &file).map_err(|error| failed(&file, error))?;
    Ok(file)
}

fn read_folder(folder: &Path) -> BTreeMap<String, Entry> {
    let mut learned = BTreeMap::new();
    let Ok(files) = std::fs::read_dir(folder) else { return learned };
    for file in files.flatten().map(|file| file.path()) {
        if file.extension().and_then(|extension| extension.to_str()) != Some(EXTENSION) {
            continue;
        }
        let read =
            std::fs::read_to_string(&file).map_err(|error| error.to_string()).and_then(|text| Entry::from_json(&text));
        match read {
            Ok(entry) => drop(learned.insert(entry.uri.clone(), entry)),
            Err(error) => log::warn!("what was learned in {} was not read: {error}", file.display()),
        }
    }
    learned
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::{Articulation, Role};

    const SYNTH: &str = "vst3:SOMESYNTH";
    const ECLIPSE: &str = "vst3:ABCDEF019182FAEB41707364416D6574";

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("trill-instruments-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&folder).ok();
        folder
    }

    fn found() -> InstrumentProfile {
        InstrumentProfile {
            articulations: vec![Articulation::key("staccato", 12)],
            range: Some((36, 96)),
            bend_range: Some(12.0),
            notes: "Bends an octave.".into(),
            ..InstrumentProfile::default()
        }
    }

    #[test]
    fn what_is_learned_is_kept_in_the_folder_and_read_by_the_next_run() {
        let folder = folder("kept");
        let knowledge = InstrumentKnowledge::default();
        knowledge.open(folder.clone());
        assert_eq!(knowledge.of(SYNTH), None);

        let remembered = knowledge.remember(SYNTH, "Some Synth", found()).unwrap();
        assert!(!remembered.replaced);
        let file = remembered.file.unwrap();
        assert_eq!(file, folder.join("vst3_SOMESYNTH.json"));
        let known = knowledge.of(SYNTH).unwrap();
        assert_eq!((known.source, known.device.as_str(), &known.profile), (Source::Learned, "Some Synth", &found()));

        // Another run of the program finds it.
        let next = InstrumentKnowledge::default();
        next.open(folder.clone());
        assert_eq!(next.of(SYNTH), Some(known));
        // Learning again replaces it; forgetting takes the file away.
        let changed = InstrumentProfile { sounds_octaves: -1, ..found() };
        assert!(next.remember(SYNTH, "Some Synth", changed.clone()).unwrap().replaced);
        assert_eq!(next.of(SYNTH).unwrap().profile, changed);
        assert_eq!(next.forget(SYNTH), Ok(true));
        assert_eq!((next.of(SYNTH), file.exists(), next.forget(SYNTH)), (None, false, Ok(false)));
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn without_a_folder_what_is_learned_lasts_as_long_as_the_program() {
        let knowledge = InstrumentKnowledge::default();
        let remembered = knowledge.remember(SYNTH, "Some Synth", found()).unwrap();
        assert_eq!((remembered.file, knowledge.folder()), (None, None));
        assert_eq!(knowledge.of(SYNTH).unwrap().profile, found());
        assert_eq!(InstrumentKnowledge::default().of(SYNTH), None, "another store knows nothing of it");
        // A profile that contradicts itself is not remembered.
        let upside_down = InstrumentProfile { range: Some((90, 30)), ..found() };
        assert!(knowledge.remember(SYNTH, "Some Synth", upside_down).unwrap_err().contains("upside down"));
        assert!(knowledge.remember(" ", "Nameless", found()).unwrap_err().contains("needs a URI"));
        assert_eq!(knowledge.of(SYNTH).unwrap().profile, found());
    }

    #[test]
    fn what_was_learned_here_wins_over_what_ships_and_forgetting_brings_that_back() {
        let knowledge = InstrumentKnowledge::default();
        let shipped = knowledge.of(ECLIPSE).unwrap();
        assert_eq!((shipped.source, shipped.device.as_str()), (Source::BuiltIn, "Ample Metal Eclipse"));
        assert_eq!(shipped.profile.articulation("palm_mute").unwrap().key, Some(26));

        let own = InstrumentProfile { role: Some(Role::Pitched), range: Some((36, 80)), ..Default::default() };
        knowledge.remember(ECLIPSE, "Ample Metal Eclipse", own.clone()).unwrap();
        let known = knowledge.of(ECLIPSE).unwrap();
        assert_eq!((known.source, &known.profile), (Source::Learned, &own));
        assert_eq!(InstrumentKnowledge::built_in(ECLIPSE), Some(shipped.clone()), "what ships is still there to read");
        let all = knowledge.all();
        assert_eq!(all.iter().filter(|known| known.uri == ECLIPSE).count(), 1, "listed once, as learned");
        assert!(all.iter().any(|known| known.source == Source::BuiltIn));

        knowledge.forget(ECLIPSE).unwrap();
        assert_eq!(knowledge.of(ECLIPSE), Some(shipped));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_passed_over() {
        let folder = folder("broken");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("broken.json"), "{ not json").unwrap();
        std::fs::write(folder.join("notes.txt"), "not a profile").unwrap();
        let upside_down = r#"{"uri": "vst3:X", "device": "X", "profile": {"range": [90, 30]}}"#;
        std::fs::write(folder.join("vst3_X.json"), upside_down).unwrap();
        let good = r#"{"uri": "vst3:Y", "profile": {"sounds_octaves": -1}}"#;
        std::fs::write(folder.join("vst3_Y.json"), good).unwrap();
        let knowledge = InstrumentKnowledge::default();
        knowledge.open(folder.clone());
        assert_eq!(knowledge.of("vst3:X"), None);
        assert_eq!(knowledge.of("vst3:Y").unwrap().profile.sounds_octaves, -1);
        std::fs::remove_dir_all(&folder).ok();
    }
}
