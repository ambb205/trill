//! The program's preferences, kept as a JSON file in the user's
//! configuration folder.
//!
//! Losing settings is worse than most bugs: it is the list of folders
//! someone spent an evening collecting. So the file is written atomically,
//! read leniently (one unreadable value costs that value, not the file),
//! and never overwritten without a copy being kept when something in it
//! could not be understood.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The name of the folder this program keeps its files in, under the
/// per-user configuration and cache directories.
const APP_FOLDER: &str = "trill";

/// How many recent projects are remembered.
const RECENT_PROJECTS: usize = 20;

/// Everything the program remembers between runs.
///
/// Every field has a default, so a settings file written by an older
/// version (missing fields) or a newer one (fields this version does not
/// know, which are ignored) still loads.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Settings {
    pub library: LibrarySettings,
    /// Projects opened lately, latest first.
    #[serde(serialize_with = "paths::serialize")]
    pub recent_projects: Vec<PathBuf>,
    pub audio: AudioSettings,
    pub midi: MidiSettings,
    pub journal: JournalSettings,
    pub studies: StudySettings,
    /// Where what was fetched is kept, by the name of its list (`models`,
    /// `instruments`), where that is not the program's own place for it.
    #[serde(serialize_with = "paths::serialize_map")]
    pub fetched: BTreeMap<String, PathBuf>,
    pub ui: UiSettings,
    pub together: TogetherSettings,
}

/// What is remembered for working in a song with others.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TogetherSettings {
    /// The name this person goes by among the others. `None` is the name
    /// they are signed in to the machine by.
    pub name: Option<String>,
    /// The meeting point this person shares songs through, where they
    /// have one: what its keeper handed them.
    pub meeting_point: Option<MeetingPointSettings>,
}

/// A meeting point as its keeper handed it over: where it is, and the
/// key that lets this person open meetings there.
///
/// The key is a secret of this person's, and the settings are the one
/// place it is kept: it is in no song, no journal and no log, and no
/// reply of a tool says it. So it is not in what this shows of itself
/// either (`Debug`).
#[derive(Clone, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct MeetingPointSettings {
    /// Its address as the keeper wrote it: `meet.example.org:443`.
    pub address: String,
    pub key: String,
}

impl std::fmt::Debug for MeetingPointSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key = if self.key.is_empty() { "no key" } else { "a key" };
        write!(f, "MeetingPointSettings({}, {key})", self.address)
    }
}

/// The sound library's part of the settings; see
/// [`Library::open`](crate::Library::open) and
/// [`Library::settings`](crate::Library::settings).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LibrarySettings {
    /// The folders the library covers.
    #[serde(serialize_with = "paths::serialize")]
    pub roots: Vec<PathBuf>,
    /// Files marked as favorites, in the order they were marked.
    #[serde(serialize_with = "paths::serialize")]
    pub favorites: Vec<PathBuf>,
    /// Files used lately, latest first.
    #[serde(serialize_with = "paths::serialize")]
    pub recent: Vec<PathBuf>,
}

/// Audio device preferences. `None` means "whatever the system offers".
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AudioSettings {
    /// Name of the output device.
    pub output_device: Option<String>,
    /// Buffer size in frames.
    pub buffer_size: Option<u32>,
    /// Sample rate in hertz.
    pub sample_rate: Option<u32>,
    /// Whether the output device is taken for this program alone, for the
    /// shortest latency.
    pub exclusive: bool,
    /// Name of the device audio is recorded from.
    pub input_device: Option<String>,
    /// How long sound takes to go round each pair of devices it was found
    /// out for, as they were opened. A file written before there were any
    /// has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub round_trips: Vec<RoundTrip>,
}

/// How long sound takes to go round an output and an input as they were
/// opened: what a measurement found, and what was added by hand. Another
/// buffer, another rate, or a device taken for the program alone is
/// another round trip.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub struct RoundTrip {
    pub output: String,
    pub input: String,
    pub exclusive: bool,
    pub sample_rate: u32,
    pub buffer_frames: u32,
    /// What lies between the engine and the microphone on the way round
    /// beside the engine's own part, in milliseconds, where it was
    /// measured: it stands in place of what the devices say.
    #[serde(default)]
    pub measured_ms: Option<f64>,
    /// What was added to that by hand, in milliseconds.
    #[serde(default)]
    pub adjust_ms: f64,
}

/// MIDI device preferences.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MidiSettings {
    /// Names of the MIDI inputs that are switched off. Every other input
    /// is listened to, one plugged in for the first time included: a
    /// keyboard should play without a visit to the settings.
    pub disabled_inputs: Vec<String>,
}

/// Whether a journal is kept of how each song is made.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JournalSettings {
    /// Keep one. On unless it was switched off: a journal stays on this
    /// machine, in the program's own folder, and is what a session is
    /// played again from.
    pub keep: bool,
}

impl Default for JournalSettings {
    fn default() -> Self {
        Self { keep: true }
    }
}

/// Where studies of recordings are kept. `None` is the program's own
/// data folder ([`data_dir`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StudySettings {
    /// The folder studies are kept in, one folder each: a few hundred
    /// megabytes for a song, most of it its stems.
    pub folder: Option<PathBuf>,
}

/// Interface preferences.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UiSettings {
    /// Name of the colour theme.
    pub theme: String,
    /// Width of the browser panel, in points.
    pub browser_width: f32,
    /// Width and height of the main window, in points; `None` until the
    /// window has been sized once.
    pub window_size: Option<[f32; 2]>,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self { theme: "noir".into(), browser_width: 280.0, window_size: None }
    }
}

/// Settings as read from disk, and anything the reader should be told.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub settings: Settings,
    /// What could not be read and what was done about it, in words fit to
    /// show the user. `None` when the file was read whole or does not
    /// exist yet.
    pub warning: Option<String>,
}

/// A settings file could not be written.
#[derive(Debug, thiserror::Error)]
#[error("cannot save settings to '{}': {reason}", .path.display())]
pub struct SaveError {
    pub path: PathBuf,
    pub reason: String,
}

impl Settings {
    /// Where the settings live: `settings.json` in [`config_dir`].
    pub fn default_path() -> Option<PathBuf> {
        config_dir().map(|folder| folder.join("settings.json"))
    }

    /// Reads the settings at `path`. Never fails:
    ///
    /// - no file: the defaults;
    /// - a value of the wrong type, or one this version cannot make sense
    ///   of: the default for that value, everything else as written, and a
    ///   copy of the file kept beside it (`.backup`) since saving will drop
    ///   what was not understood;
    /// - a file that is not JSON at all: the defaults, with the file moved
    ///   aside (`.corrupt-<time>`) so that it can be recovered by hand.
    ///
    /// Names this version does not know are ignored.
    pub fn load(path: &Path) -> Loaded {
        let text = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Loaded { settings: Settings::default(), warning: None };
            }
            Err(error) => {
                let warning =
                    format!("The settings at '{}' could not be read ({error}); using defaults.", path.display());
                return Loaded { settings: Settings::default(), warning: Some(warning) };
            }
        };
        let Ok(Value::Object(document)) = serde_json::from_slice::<Value>(&text) else {
            let moved = set_aside(path);
            let warning = match moved {
                Ok(backup) => format!(
                    "The settings file was damaged and has been moved to '{}'; starting from defaults.",
                    backup.display()
                ),
                Err(error) => format!(
                    "The settings file '{}' is damaged and could not be moved aside ({error}); starting from defaults.",
                    path.display()
                ),
            };
            return Loaded { settings: Settings::default(), warning: Some(warning) };
        };

        let mut reader = Reader { rejected: Vec::new() };
        let settings = reader.settings(&document);
        if reader.rejected.is_empty() {
            return Loaded { settings, warning: None };
        }
        let backup = with_suffix(path, ".backup");
        let kept = std::fs::copy(path, &backup);
        let mut warning =
            format!("Some settings could not be understood and were reset: {}.", reader.rejected.join(", "));
        if kept.is_ok() {
            warning.push_str(&format!(" The file as it was is kept at '{}'.", backup.display()));
        }
        Loaded { settings, warning: Some(warning) }
    }

    /// Writes the settings to `path`, creating its folder if need be. The
    /// file is replaced in one step, so a crash or a full disk leaves the
    /// old settings, never half of the new.
    pub fn save(&self, path: &Path) -> Result<(), SaveError> {
        let fail = |reason: &dyn std::fmt::Display| SaveError { path: path.to_path_buf(), reason: reason.to_string() };
        let mut text = serde_json::to_vec_pretty(self).map_err(|error| fail(&error))?;
        text.push(b'\n');
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| fail(&error))?;
        }
        let temporary = with_suffix(path, &format!(".{}.part", std::process::id()));
        let written = std::fs::File::create(&temporary)
            .and_then(|mut file| {
                file.write_all(&text)?;
                // On disk before it takes the old file's place.
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&temporary, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written.map_err(|error| fail(&error))
    }

    /// Puts `project` at the front of the recent projects.
    pub fn add_recent_project(&mut self, project: &Path) {
        self.recent_projects.retain(|known| known != project);
        self.recent_projects.insert(0, project.to_path_buf());
        self.recent_projects.truncate(RECENT_PROJECTS);
    }
}

/// Reads settings out of a JSON document value by value, so that one bad
/// value cannot take the rest with it.
struct Reader {
    /// Names of the values that were present but unusable.
    rejected: Vec<String>,
}

impl Reader {
    fn settings(&mut self, document: &Map<String, Value>) -> Settings {
        let mut settings = Settings::default();
        if let Some(library) = self.section(document, "library") {
            self.paths(library, "library.roots", &mut settings.library.roots);
            self.paths(library, "library.favorites", &mut settings.library.favorites);
            self.paths(library, "library.recent", &mut settings.library.recent);
        }
        self.paths(document, "recent_projects", &mut settings.recent_projects);
        if let Some(audio) = self.section(document, "audio") {
            self.value(audio, "audio.output_device", &mut settings.audio.output_device);
            self.value(audio, "audio.buffer_size", &mut settings.audio.buffer_size);
            self.value(audio, "audio.sample_rate", &mut settings.audio.sample_rate);
            self.value(audio, "audio.exclusive", &mut settings.audio.exclusive);
            self.value(audio, "audio.input_device", &mut settings.audio.input_device);
            self.value(audio, "audio.round_trips", &mut settings.audio.round_trips);
        }
        if let Some(midi) = self.section(document, "midi") {
            self.value(midi, "midi.disabled_inputs", &mut settings.midi.disabled_inputs);
        }
        if let Some(journal) = self.section(document, "journal") {
            self.value(journal, "journal.keep", &mut settings.journal.keep);
        }
        if let Some(studies) = self.section(document, "studies") {
            self.value(studies, "studies.folder", &mut settings.studies.folder);
        }
        self.value(document, "fetched", &mut settings.fetched);
        if let Some(together) = self.section(document, "together") {
            self.value(together, "together.name", &mut settings.together.name);
            self.value(together, "together.meeting_point", &mut settings.together.meeting_point);
        }
        if let Some(ui) = self.section(document, "ui") {
            self.value(ui, "ui.theme", &mut settings.ui.theme);
            self.value(ui, "ui.browser_width", &mut settings.ui.browser_width);
            self.value(ui, "ui.window_size", &mut settings.ui.window_size);
        }
        settings
    }

    fn section<'a>(&mut self, document: &'a Map<String, Value>, name: &str) -> Option<&'a Map<String, Value>> {
        match document.get(name)? {
            Value::Object(section) => Some(section),
            _ => {
                self.rejected.push(name.to_string());
                None
            }
        }
    }

    /// Reads the value called `name` (the part after the last dot is its
    /// key in `object`) into `target`, leaving `target` alone if the value
    /// is absent or unusable.
    fn value<T: DeserializeOwned>(&mut self, object: &Map<String, Value>, name: &str, target: &mut T) {
        let key = name.rsplit('.').next().unwrap_or(name);
        if let Some(value) = object.get(key) {
            match T::deserialize(value) {
                Ok(read) => *target = read,
                Err(_) => self.rejected.push(name.to_string()),
            }
        }
    }

    /// Reads a list of paths, keeping the items that are paths.
    fn paths(&mut self, object: &Map<String, Value>, name: &str, target: &mut Vec<PathBuf>) {
        let key = name.rsplit('.').next().unwrap_or(name);
        match object.get(key) {
            None => {}
            Some(Value::Array(items)) => {
                *target =
                    items.iter().filter_map(Value::as_str).filter(|path| !path.is_empty()).map(PathBuf::from).collect();
                if target.len() != items.len() {
                    self.rejected.push(format!("part of {name}"));
                }
            }
            Some(_) => self.rejected.push(name.to_string()),
        }
    }
}

/// Paths are written as text. One that is not valid Unicode is written as
/// near as text can get, rather than making the whole file unwritable.
mod paths {
    use serde::Serializer;
    use std::path::PathBuf;

    pub fn serialize<S: Serializer>(paths: &[PathBuf], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(paths.iter().map(|path| path.to_string_lossy()))
    }

    /// A map of names to paths, each path as [`serialize`] writes one.
    pub fn serialize_map<S: Serializer>(
        paths: &std::collections::BTreeMap<String, PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_map(paths.iter().map(|(name, path)| (name, path.to_string_lossy())))
    }
}

/// `path` with `suffix` appended to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Moves a damaged file out of the way, under a name that says when.
fn set_aside(path: &Path) -> std::io::Result<PathBuf> {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs());
    let backup = with_suffix(path, &format!(".corrupt-{seconds}"));
    std::fs::rename(path, &backup)?;
    Ok(backup)
}

/// The folder for this program's settings: `%APPDATA%\trill` on Windows,
/// `~/Library/Application Support/trill` on macOS, and `$XDG_CONFIG_HOME/trill`
/// or `~/.config/trill` elsewhere. `None` if the environment names no home.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_in(&|name| std::env::var_os(name))
}

/// The folder for files this program can rebuild, such as the library
/// index: `%LOCALAPPDATA%\trill` on Windows, `~/Library/Caches/trill` on macOS,
/// and `$XDG_CACHE_HOME/trill` or `~/.cache/trill` elsewhere.
pub fn cache_dir() -> Option<PathBuf> {
    cache_dir_in(&|name| std::env::var_os(name))
}

/// The folder for what this program keeps that is large and stays on this
/// machine (studies of recordings, the models that make them):
/// `%LOCALAPPDATA%\trill` on Windows, `~/Library/Application Support/trill`
/// on macOS, and `$XDG_DATA_HOME/trill` or `~/.local/share/trill` elsewhere.
pub fn data_dir() -> Option<PathBuf> {
    data_dir_in(&|name| std::env::var_os(name))
}

type Environment<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// The value of an environment variable naming a folder, if it is set to
/// something usable. The XDG specification says relative values are to be
/// ignored, and an empty one is as good as unset anywhere.
fn folder(environment: Environment, name: &str) -> Option<PathBuf> {
    environment(name).map(PathBuf::from).filter(|path| path.is_absolute())
}

fn config_dir_in(environment: Environment) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        folder(environment, "APPDATA")
    } else if cfg!(target_os = "macos") {
        folder(environment, "HOME").map(|home| home.join("Library").join("Application Support"))
    } else {
        folder(environment, "XDG_CONFIG_HOME").or_else(|| folder(environment, "HOME").map(|home| home.join(".config")))
    };
    base.map(|base| base.join(APP_FOLDER))
}

fn data_dir_in(environment: Environment) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        folder(environment, "LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        folder(environment, "HOME").map(|home| home.join("Library").join("Application Support"))
    } else {
        folder(environment, "XDG_DATA_HOME")
            .or_else(|| folder(environment, "HOME").map(|home| home.join(".local").join("share")))
    };
    base.map(|base| base.join(APP_FOLDER))
}

fn cache_dir_in(environment: Environment) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        folder(environment, "LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        folder(environment, "HOME").map(|home| home.join("Library").join("Caches"))
    } else {
        folder(environment, "XDG_CACHE_HOME").or_else(|| folder(environment, "HOME").map(|home| home.join(".cache")))
    };
    base.map(|base| base.join(APP_FOLDER))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            library: LibrarySettings {
                roots: vec!["/samples/Splice".into(), "/samples/My Packs".into()],
                favorites: vec!["/samples/My Packs/Kick 01.wav".into()],
                recent: vec!["/samples/Splice/loop 140.wav".into(), "/samples/My Packs/Kick 01.wav".into()],
            },
            recent_projects: vec!["/songs/night drive.trill".into()],
            audio: AudioSettings {
                output_device: Some("Speakers (USB Audio)".into()),
                buffer_size: Some(256),
                sample_rate: Some(48_000),
                exclusive: true,
                input_device: Some("Line In (USB Audio)".into()),
                // What a measurement found for one pair of devices, and
                // what was added by hand for another.
                round_trips: vec![
                    RoundTrip {
                        output: "Speakers (USB Audio)".into(),
                        input: "Line In (USB Audio)".into(),
                        exclusive: true,
                        sample_rate: 48_000,
                        buffer_frames: 256,
                        measured_ms: Some(11.25),
                        adjust_ms: 0.0,
                    },
                    RoundTrip {
                        output: "Speakers".into(),
                        input: "Microphone".into(),
                        exclusive: false,
                        sample_rate: 44_100,
                        buffer_frames: 441,
                        measured_ms: None,
                        adjust_ms: -1.5,
                    },
                ],
            },
            midi: MidiSettings { disabled_inputs: vec!["Loopback".into()] },
            journal: JournalSettings { keep: false },
            studies: StudySettings { folder: Some("/big disk/studies".into()) },
            fetched: [("models".to_owned(), PathBuf::from("/big disk/models"))].into_iter().collect(),
            ui: UiSettings { theme: "noir".into(), browser_width: 312.5, window_size: Some([1600.0, 900.0]) },
            together: TogetherSettings {
                name: Some("Jo".into()),
                meeting_point: Some(MeetingPointSettings {
                    address: "meet.example.org:443".into(),
                    key: "0000-1111-2222-3333-4444-5555-6666-7777-8888-9999".into(),
                }),
            },
        }
    }

    #[test]
    fn settings_round_trip_and_saving_is_tidy() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("trill").join("settings.json");
        let settings = sample();
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), Loaded { settings: settings.clone(), warning: None });

        // Saving over an existing file replaces it and leaves nothing else.
        let mut changed = settings;
        changed.ui.browser_width = 200.0;
        changed.save(&path).unwrap();
        assert_eq!(Settings::load(&path).settings.ui.browser_width, 200.0);
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        // It is a file a person can read and edit.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\n  \"library\": {\n    \"roots\": ["), "{text}");
    }

    #[test]
    fn a_missing_file_means_defaults() {
        let folder = tempfile::tempdir().unwrap();
        let loaded = Settings::load(&folder.path().join("settings.json"));
        assert_eq!(loaded, Loaded { settings: Settings::default(), warning: None });
        assert_eq!(loaded.settings.ui.theme, "noir");
        assert!(loaded.settings.library.roots.is_empty() && loaded.settings.audio.output_device.is_none());
    }

    #[test]
    fn missing_and_unknown_names_are_fine() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        // An older version's file: only some names. A newer one's: extras.
        std::fs::write(
            &path,
            r#"{ "library": { "roots": ["/a"], "smart_folders": [1, 2] }, "ui": { "browser_width": 300 }, "telemetry": false }"#,
        )
        .unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.warning, None);
        assert_eq!(loaded.settings.library.roots, [PathBuf::from("/a")]);
        assert_eq!(loaded.settings.ui, UiSettings { browser_width: 300.0, ..UiSettings::default() });
        assert_eq!(loaded.settings.audio, AudioSettings::default());
        assert_eq!(loaded.settings.midi, MidiSettings::default());
        assert!(!with_suffix(&path, ".backup").exists(), "nothing was lost, so nothing is backed up");
    }

    #[test]
    fn one_bad_value_costs_only_that_value() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        let text = r#"{
            "library": { "roots": ["/a", 7, "/b", ""], "favorites": "none" },
            "recent_projects": ["/songs/x.trill"],
            "audio": { "output_device": "Speakers", "buffer_size": "big", "sample_rate": 44100, "exclusive": "yes" },
            "midi": { "disabled_inputs": ["Loopback"] },
            "ui": []
        }"#;
        std::fs::write(&path, text).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.settings.library.roots, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(loaded.settings.library.favorites.is_empty());
        assert_eq!(loaded.settings.recent_projects, [PathBuf::from("/songs/x.trill")]);
        assert_eq!(
            loaded.settings.audio,
            AudioSettings {
                output_device: Some("Speakers".into()),
                sample_rate: Some(44_100),
                ..AudioSettings::default()
            }
        );
        assert_eq!(loaded.settings.midi.disabled_inputs, ["Loopback"]);
        assert_eq!(loaded.settings.ui, UiSettings::default());
        let warning = loaded.warning.unwrap();
        for name in ["part of library.roots", "library.favorites", "audio.buffer_size", "audio.exclusive", "ui"] {
            assert!(warning.contains(name), "{warning}");
        }
        // The file as it was is kept, because saving will drop those values.
        assert_eq!(std::fs::read_to_string(with_suffix(&path, ".backup")).unwrap(), text);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "loading changes nothing");
    }

    #[test]
    fn a_damaged_file_is_moved_aside_and_defaults_are_used() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.json");
        for damaged in ["{ \"library\": { \"roots\": [\"/a\"", "", "[1, 2, 3]", "\u{0}\u{1}garbage"] {
            std::fs::write(&path, damaged).unwrap();
            let loaded = Settings::load(&path);
            assert_eq!(loaded.settings, Settings::default());
            assert!(loaded.warning.unwrap().contains("moved to"));
            assert!(!path.exists(), "the damaged file is out of the way");
            let backups: Vec<PathBuf> =
                std::fs::read_dir(folder.path()).unwrap().map(|entry| entry.unwrap().path()).collect();
            assert_eq!(backups.len(), 1);
            assert!(backups[0].to_string_lossy().contains("settings.json.corrupt-"));
            assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), damaged, "and nothing of it is lost");
            std::fs::remove_file(&backups[0]).unwrap();
            // The program then saves its defaults in the usual place.
            loaded.settings.save(&path).unwrap();
            assert_eq!(Settings::load(&path).warning, None);
        }
    }

    #[test]
    fn saving_reports_failure_and_keeps_the_old_file() {
        let folder = tempfile::tempdir().unwrap();
        // A folder where the file should be.
        let blocked = folder.path().join("settings.json");
        std::fs::create_dir(&blocked).unwrap();
        let error = sample().save(&blocked).unwrap_err();
        assert_eq!(error.path, blocked);
        assert!(error.to_string().starts_with("cannot save settings to"), "{error}");
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1, "no temporary file left behind");
        // Reading a folder as settings is a warning, not a crash.
        assert!(Settings::load(&blocked).warning.is_some());
    }

    #[test]
    fn recent_projects_are_latest_first_without_repeats() {
        let mut settings = Settings::default();
        for name in ["a", "b", "a", "c"] {
            settings.add_recent_project(Path::new(name));
        }
        assert_eq!(settings.recent_projects, [PathBuf::from("c"), PathBuf::from("a"), PathBuf::from("b")]);
        for number in 0..100 {
            settings.add_recent_project(Path::new(&format!("song {number}")));
        }
        assert_eq!(settings.recent_projects.len(), RECENT_PROJECTS);
        assert_eq!(settings.recent_projects[0], PathBuf::from("song 99"));
    }

    #[test]
    fn folders_follow_each_platform() {
        let home = if cfg!(windows) { r"C:\Users\someone" } else { "/home/someone" };
        let variables = |set: &'static [(&'static str, &'static str)]| {
            move |name: &str| set.iter().find(|(known, _)| *known == name).map(|(_, value)| OsString::from(value))
        };
        if cfg!(windows) {
            let environment = variables(&[
                ("APPDATA", r"C:\Users\someone\AppData\Roaming"),
                ("LOCALAPPDATA", r"C:\Users\someone\AppData\Local"),
            ]);
            assert_eq!(config_dir_in(&environment), Some(PathBuf::from(r"C:\Users\someone\AppData\Roaming\trill")));
            assert_eq!(cache_dir_in(&environment), Some(PathBuf::from(r"C:\Users\someone\AppData\Local\trill")));
        } else if cfg!(target_os = "macos") {
            let environment = variables(&[("HOME", "/Users/someone")]);
            assert_eq!(
                config_dir_in(&environment),
                Some(PathBuf::from("/Users/someone/Library/Application Support/trill"))
            );
            assert_eq!(cache_dir_in(&environment), Some(PathBuf::from("/Users/someone/Library/Caches/trill")));
        } else {
            let plain = variables(&[("HOME", "/home/someone")]);
            assert_eq!(config_dir_in(&plain), Some(PathBuf::from("/home/someone/.config/trill")));
            assert_eq!(cache_dir_in(&plain), Some(PathBuf::from("/home/someone/.cache/trill")));
            let xdg = variables(&[
                ("HOME", "/home/someone"),
                ("XDG_CONFIG_HOME", "/cfg"),
                ("XDG_CACHE_HOME", "relative/ignored"),
            ]);
            assert_eq!(config_dir_in(&xdg), Some(PathBuf::from("/cfg/trill")));
            assert_eq!(cache_dir_in(&xdg), Some(PathBuf::from("/home/someone/.cache/trill")));
        }
        // Nothing to go on: no folder, rather than one relative to wherever
        // the program happened to be started.
        assert_eq!(config_dir_in(&|_| None), None);
        assert_eq!(cache_dir_in(&|name| (name != "nothing").then(|| OsString::from(""))), None);
        assert!(Path::new(home).is_absolute());
        // On a real machine there is somewhere.
        assert!(Settings::default_path().is_some_and(|path| path.ends_with("trill/settings.json")));
    }
}
