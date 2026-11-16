//! The presets someone saved: a device's sound under a name, kept beside
//! the user's settings and listed with the presets the device type brings.
//!
//! A plugin that keeps its presets in its own window gives a host nothing
//! to list. What it does give is its state, and a state under a name is a
//! preset: saved once, it is there for every song. The same goes for a
//! built-in device, whose sound is its parameter values.
//!
//! Each preset is a small text file ([`SavedPreset`] as JSON) in a folder
//! per device type; a device's own state, which can be large and is only
//! needed when the preset is loaded, is in a file beside it. A store that
//! was given no folder keeps what is saved in memory, for as long as the
//! program runs: what a test or a scratch session wants.
//!
//! [`WithSaved`] puts the store in front of a device factory, so that
//! whoever asks for a device type's presets gets the saved ones too.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use trill_core::Device;
use trill_core::device::{DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceWarning};
use trill_core::presets::{PresetInfo, PresetList, SavedPreset, short_id};

/// The extension of a saved preset's file.
const EXTENSION: &str = "trillpreset";
/// What the name of the file with a preset's state adds to its own.
const STATE_SUFFIX: &str = "-state";

/// The presets that were saved, of every device type. See the module
/// documentation.
#[derive(Default)]
pub struct SavedPresets {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Where the presets are kept; `None` keeps them in memory.
    folder: Option<PathBuf>,
    entries: Vec<Entry>,
    /// Counts the changes, for those who keep something made of the list.
    revision: u64,
}

struct Entry {
    /// Without its state if that is in a file.
    preset: SavedPreset,
    info: PresetInfo,
    /// The preset's file; `None` for one that is only in memory.
    file: Option<PathBuf>,
}

/// How saving a preset came out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub info: PresetInfo,
    /// Whether there was a preset of that name, which this one replaced.
    pub replaced: bool,
    /// The file it was written to; `None` when nothing is kept on disk.
    pub file: Option<PathBuf>,
}

impl SavedPresets {
    /// Keeps the presets in `folder` from now on, and reads the ones that
    /// are there. What was saved in memory before is forgotten. Files that
    /// cannot be read are passed over, with a line in the log.
    pub fn open(&self, folder: PathBuf) {
        let entries = read_folder(&folder);
        let mut inner = self.inner.lock();
        inner.folder = Some(folder);
        inner.entries = entries;
        inner.revision += 1;
    }

    /// Looks at the folder again: for presets that were put there, or
    /// taken away, while the program ran.
    pub fn refresh(&self) {
        let Some(folder) = self.inner.lock().folder.clone() else { return };
        let entries = read_folder(&folder);
        let mut inner = self.inner.lock();
        let same = inner.entries.len() == entries.len()
            && inner.entries.iter().zip(&entries).all(|(old, new)| old.file == new.file && old.preset == new.preset);
        if !same {
            inner.entries = entries;
            inner.revision += 1;
        }
    }

    /// Where the presets are kept, if on disk.
    pub fn folder(&self) -> Option<PathBuf> {
        self.inner.lock().folder.clone()
    }

    /// A number that changes whenever the saved presets do.
    pub fn revision(&self) -> u64 {
        self.inner.lock().revision
    }

    /// The saved presets of the device type `uri`, by name.
    pub fn list(&self, uri: &str) -> Vec<PresetInfo> {
        let inner = self.inner.lock();
        inner.entries.iter().filter(|entry| entry.preset.uri == uri).map(|entry| entry.info.clone()).collect()
    }

    /// How many presets are saved, of all device types together.
    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Saves `preset`, in place of the one its device type has under that
    /// name (in either case) if there is one.
    pub fn save(&self, mut preset: SavedPreset) -> Result<Saved, String> {
        preset.name = SavedPreset::checked_name(&preset.name)?;
        let info = preset.info();
        let mut inner = self.inner.lock();
        let file = match &inner.folder {
            Some(folder) => Some(write(folder, &preset)?),
            None => None,
        };
        if file.is_some() {
            // The state is in its file, and is read from there.
            preset.state = None;
        }
        let same = |entry: &Entry| entry.preset.uri == preset.uri && same_name(&entry.preset.name, &preset.name);
        let replaced = inner.entries.iter().any(same);
        // A name in another case is another file on some systems, and the
        // same file on others.
        for old in inner.entries.iter().filter(|entry| same(entry)).filter_map(|entry| entry.file.as_ref()) {
            if !file.as_ref().is_some_and(|new| same_file(old, new)) {
                remove(old);
            }
        }
        inner.entries.retain(|entry| !same(entry));
        inner.entries.push(Entry { preset, info: info.clone(), file: file.clone() });
        sort(&mut inner.entries);
        inner.revision += 1;
        Ok(Saved { info, replaced, file })
    }

    /// The saved preset `info` lists, whole: with the state that is kept
    /// in a file of its own. `None` if `info` is not a preset saved for
    /// the device type `uri`.
    pub fn load(&self, uri: &str, info: &PresetInfo) -> Result<Option<SavedPreset>, String> {
        let Some(name) = SavedPreset::name_of(info) else { return Ok(None) };
        let (mut preset, file) = {
            let inner = self.inner.lock();
            let found = inner.entries.iter().find(|entry| entry.preset.uri == uri && entry.preset.name == name);
            let Some(entry) = found else {
                return Err(format!("the saved preset '{name}' is no longer there"));
            };
            (entry.preset.clone(), entry.file.clone())
        };
        if let Some(file) = file {
            let state = state_file(&file);
            match std::fs::read(&state) {
                Ok(bytes) => preset.state = Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("cannot read '{}': {error}", state.display())),
            }
        }
        Ok(Some(preset))
    }

    /// Removes the preset the device type `uri` has saved under `name`,
    /// and says whether there was one.
    pub fn delete(&self, uri: &str, name: &str) -> bool {
        let mut inner = self.inner.lock();
        let same = |entry: &Entry| entry.preset.uri == uri && same_name(&entry.preset.name, name.trim());
        let before = inner.entries.len();
        for file in inner.entries.iter().filter(|entry| same(entry)).filter_map(|entry| entry.file.as_ref()) {
            remove(file);
        }
        inner.entries.retain(|entry| !same(entry));
        let removed = inner.entries.len() != before;
        if removed {
            inner.revision += 1;
        }
        removed
    }
}

/// Whether two paths lead to one file.
fn same_file(one: &Path, other: &Path) -> bool {
    one == other
        || matches!((std::fs::canonicalize(one), std::fs::canonicalize(other)), (Ok(one), Ok(other)) if one == other)
}

fn same_name(one: &str, other: &str) -> bool {
    one.to_lowercase() == other.to_lowercase()
}

fn sort(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|entry| (entry.preset.uri.clone(), entry.preset.name.to_lowercase()));
}

/// The file a preset's state is kept in, beside the preset's own.
fn state_file(preset_file: &Path) -> PathBuf {
    let mut name = preset_file.as_os_str().to_owned();
    name.push(STATE_SUFFIX);
    PathBuf::from(name)
}

fn remove(preset_file: &Path) {
    for file in [preset_file.to_path_buf(), state_file(preset_file)] {
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("cannot remove '{}': {error}", file.display()),
        }
    }
}

/// A name as part of a path: without what a file system will not have in
/// one. Never empty.
fn file_name(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    // Names Windows keeps for devices, whatever follows the dot.
    let stem = cleaned.split('.').next().unwrap_or_default().to_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    match (cleaned.is_empty(), reserved) {
        (true, _) => "_".to_string(),
        (false, true) => format!("_{cleaned}"),
        (false, false) => cleaned,
    }
}

/// The folder the presets of one device type are kept in: named for a
/// person to find, with enough of the type's URI to keep two types of one
/// name apart.
fn device_folder(folder: &Path, preset: &SavedPreset) -> PathBuf {
    let scheme = preset.uri.split(':').next().unwrap_or_default();
    let named = if preset.device.trim().is_empty() { "device" } else { preset.device.as_str() };
    let mark = &short_id(&preset.uri)[..6];
    folder.join(file_name(&format!("{named} ({scheme} {mark})")))
}

/// Writes a preset's files, the state first: a preset that is listed has
/// its state.
fn write(folder: &Path, preset: &SavedPreset) -> Result<PathBuf, String> {
    let folder = device_folder(folder, preset);
    std::fs::create_dir_all(&folder).map_err(|error| format!("cannot create '{}': {error}", folder.display()))?;
    let file = folder.join(format!("{}.{EXTENSION}", file_name(&preset.name)));
    let state = state_file(&file);
    match &preset.state {
        Some(bytes) => replace(&state, bytes)?,
        None => match std::fs::remove_file(&state) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot remove '{}': {error}", state.display())),
        },
    }
    let text = SavedPreset { state: None, ..preset.clone() }.to_json();
    replace(&file, text.as_bytes())?;
    Ok(file)
}

/// Writes a file whole or not at all: beside it first, then in its place.
fn replace(file: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut partial = file.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let written = std::fs::write(&partial, bytes).and_then(|()| std::fs::rename(&partial, file));
    written.map_err(|error| {
        let _ = std::fs::remove_file(&partial);
        format!("cannot write '{}': {error}", file.display())
    })
}

/// The presets in the folders of `folder`, by device type and name.
fn read_folder(folder: &Path) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let Ok(devices) = std::fs::read_dir(folder) else { return entries };
    for device in devices.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()) {
        let Ok(files) = std::fs::read_dir(&device) else { continue };
        for file in files.flatten().map(|entry| entry.path()) {
            if file.extension().is_none_or(|extension| extension != EXTENSION) {
                continue;
            }
            let read = std::fs::read_to_string(&file).map_err(|error| error.to_string());
            match read.and_then(|text| SavedPreset::from_json(&text)) {
                Ok(preset) => {
                    // The same preset twice (a folder that was copied):
                    // the first one found stands.
                    let twice = entries
                        .iter()
                        .any(|entry| entry.preset.uri == preset.uri && same_name(&entry.preset.name, &preset.name));
                    if !twice {
                        entries.push(Entry { info: preset.info(), preset, file: Some(file) });
                    }
                }
                Err(reason) => log::warn!("the saved preset '{}' was passed over: {reason}", file.display()),
            }
        }
    }
    sort(&mut entries);
    entries
}

/// A device factory with the saved presets in front of its own: every
/// list of a device type's presets begins with the ones saved for it.
pub(crate) struct WithSaved {
    inner: Arc<dyn DeviceFactory>,
    saved: Arc<SavedPresets>,
    /// The last list made for each device type, with what it was made of,
    /// so that a view which asks on every frame is handed the same list.
    made: Mutex<HashMap<String, Made>>,
}

struct Made {
    of: Option<PresetList>,
    revision: u64,
    list: PresetList,
}

impl WithSaved {
    pub(crate) fn new(inner: Arc<dyn DeviceFactory>, saved: Arc<SavedPresets>) -> Self {
        Self { inner, saved, made: Mutex::new(HashMap::new()) }
    }

    /// `own`, the presets a device type brings, with the saved ones in
    /// front. `None` for a type that has neither.
    fn with_saved(&self, uri: &str, own: Option<PresetList>) -> Option<PresetList> {
        let revision = self.saved.revision();
        let mut made = self.made.lock();
        if let Some(made) = made.get(uri)
            && made.revision == revision
            && same_list(&made.of, &own)
        {
            return Some(made.list.clone());
        }
        let saved = self.saved.list(uri);
        if saved.is_empty() {
            made.remove(uri);
            return own;
        }
        let list = match &own {
            Some(own) => PresetList {
                status: own.status.clone(),
                presets: saved.into_iter().chain(own.presets.iter().cloned()).collect(),
            },
            None => PresetList::ready(saved),
        };
        made.insert(uri.to_string(), Made { of: own, revision, list: list.clone() });
        Some(list)
    }
}

/// Whether two lists are one and the same, without reading them through.
fn same_list(one: &Option<PresetList>, other: &Option<PresetList>) -> bool {
    match (one, other) {
        (None, None) => true,
        (Some(one), Some(other)) => {
            one.status == other.status
                && (Arc::ptr_eq(&one.presets, &other.presets) || (one.len() <= 64 && one.presets == other.presets))
        }
        _ => false,
    }
}

impl DeviceFactory for WithSaved {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.inner.descriptors()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        self.inner.create(device, sample_rate, max_block)
    }

    fn create_for(
        &self,
        device: &Device,
        sample_rate: f64,
        max_block: usize,
        purpose: trill_core::device::Purpose,
    ) -> Result<DeviceInstance, DeviceError> {
        self.inner.create_for(device, sample_rate, max_block, purpose)
    }

    fn warnings(&self, device: &Device) -> Vec<DeviceWarning> {
        self.inner.warnings(device)
    }

    fn presets(&self, uri: &str) -> Option<PresetList> {
        self.with_saved(uri, self.inner.presets(uri))
    }

    fn presets_within(&self, uri: &str, timeout: Duration) -> Option<PresetList> {
        self.with_saved(uri, self.inner.presets_within(uri, timeout))
    }

    fn refresh_presets(&self, uri: &str) {
        self.saved.refresh();
        self.inner.refresh_presets(uri);
    }

    fn known_presets(&self, uri: &str) -> Option<PresetList> {
        self.with_saved(uri, self.inner.known_presets(uri))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::presets::{PresetStatus, SAVED_CATEGORY};
    use trill_dsp::BuiltinFactory;

    const AMP: &str = "vst3:AMP";
    const SYNTH: &str = "builtin:polysynth";

    fn preset(uri: &str, name: &str, state: Option<&[u8]>) -> SavedPreset {
        SavedPreset {
            uri: uri.into(),
            device: "Amp: Lead/Rhythm".into(),
            name: name.into(),
            params: [("gain".to_string(), 0.5)].into(),
            state: state.map(<[u8]>::to_vec),
            ..SavedPreset::default()
        }
    }

    fn names(presets: &[PresetInfo]) -> Vec<&str> {
        presets.iter().map(|preset| preset.name.as_str()).collect()
    }

    #[test]
    fn a_preset_saved_to_a_folder_is_there_the_next_time() {
        let folder = tempfile::tempdir().unwrap();
        let store = SavedPresets::default();
        store.open(folder.path().join("presets"));
        assert!(store.is_empty());

        let saved = store.save(preset(AMP, " Lead Tone ", Some(&[1, 2, 3]))).unwrap();
        assert_eq!((saved.info.path().as_str(), saved.replaced), ("Saved/Lead Tone", false));
        let file = saved.file.clone().unwrap();
        assert_eq!(file.file_name().unwrap(), "Lead Tone.trillpreset");
        // The folder is named for the device, without what a path cannot
        // hold; the state is beside the preset, not in it.
        let device_folder = file.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        assert!(device_folder.starts_with("Amp_ Lead_Rhythm (vst3 "), "{device_folder}");
        assert_eq!(std::fs::read(state_file(&file)).unwrap(), [1, 2, 3]);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("state"));
        store.save(preset(AMP, "Clean", None)).unwrap();
        store.save(preset(SYNTH, "Glass", None)).unwrap();
        assert_eq!(names(&store.list(AMP)), ["Clean", "Lead Tone"]);
        assert_eq!(names(&store.list(SYNTH)), ["Glass"]);
        assert!(store.list("vst3:OTHER").is_empty());

        // Another run of the program.
        let again = SavedPresets::default();
        again.open(folder.path().join("presets"));
        assert_eq!(again.list(AMP), store.list(AMP));
        let lead = &again.list(AMP)[1];
        let loaded = again.load(AMP, lead).unwrap().unwrap();
        assert_eq!(loaded, preset(AMP, "Lead Tone", Some(&[1, 2, 3])));
        assert_eq!(again.load(AMP, &again.list(AMP)[0]).unwrap().unwrap().state, None);
        // It is a preset of its own device type only, and one the type
        // brings is not a saved one.
        assert!(again.load(SYNTH, lead).unwrap_err().contains("no longer there"));
        assert_eq!(again.load(AMP, &PresetInfo { locator: "file|x".into(), ..lead.clone() }), Ok(None));
    }

    #[test]
    fn saving_under_a_name_again_replaces_what_was_there() {
        let folder = tempfile::tempdir().unwrap();
        let store = SavedPresets::default();
        store.open(folder.path().to_path_buf());
        let first = store.save(preset(AMP, "Lead", Some(&[1]))).unwrap();
        let revision = store.revision();
        let second = store.save(preset(AMP, "LEAD", None)).unwrap();
        assert!(second.replaced && store.revision() > revision);
        assert_eq!(names(&store.list(AMP)), ["LEAD"]);
        // Nothing of the first is left: not its state, which the second
        // does not have.
        let loaded = store.load(AMP, &second.info).unwrap().unwrap();
        assert_eq!((loaded.name.as_str(), loaded.state), ("LEAD", None));
        assert!(!state_file(&first.file.unwrap()).exists());
        let again = SavedPresets::default();
        again.open(folder.path().to_path_buf());
        assert_eq!(names(&again.list(AMP)), ["LEAD"]);

        assert!(store.delete(AMP, "lead") && !store.delete(AMP, "lead"));
        assert!(store.list(AMP).is_empty() && !second.file.unwrap().exists());
        assert!(store.save(preset(AMP, "a/b", None)).unwrap_err().contains("cannot have '/'"));
    }

    #[test]
    fn without_a_folder_presets_are_kept_while_the_program_runs() {
        let store = SavedPresets::default();
        let saved = store.save(preset(AMP, "Lead", Some(&[9, 9]))).unwrap();
        assert_eq!((saved.file, store.folder()), (None, None));
        assert_eq!(store.load(AMP, &saved.info).unwrap().unwrap().state, Some(vec![9, 9]));
        store.refresh();
        assert_eq!(names(&store.list(AMP)), ["Lead"]);
    }

    #[test]
    fn files_that_are_not_presets_are_passed_over_and_new_ones_are_found() {
        let folder = tempfile::tempdir().unwrap();
        let store = SavedPresets::default();
        store.open(folder.path().to_path_buf());
        let saved = store.save(preset(AMP, "Lead", None)).unwrap();
        let device_folder = saved.file.unwrap().parent().unwrap().to_path_buf();
        std::fs::write(device_folder.join("Broken.trillpreset"), "{ not json").unwrap();
        std::fs::write(device_folder.join("notes.txt"), "hello").unwrap();
        // Someone copies a preset in while the program runs.
        std::fs::write(device_folder.join("Copied.trillpreset"), preset(AMP, "Copied", Some(&[4])).to_json()).unwrap();
        let revision = store.revision();
        store.refresh();
        assert_eq!(names(&store.list(AMP)), ["Copied", "Lead"]);
        assert!(store.revision() > revision);
        // A state written into the preset itself is read from there.
        assert_eq!(store.load(AMP, &store.list(AMP)[0]).unwrap().unwrap().state, Some(vec![4]));
        let revision = store.revision();
        store.refresh();
        assert_eq!(store.revision(), revision, "nothing changed");
    }

    #[test]
    fn names_a_file_system_refuses_are_made_into_ones_it_takes() {
        assert_eq!(file_name("Lead: Bright?"), "Lead_ Bright_");
        assert_eq!(file_name("ends with dots..."), "ends with dots");
        assert_eq!(file_name("con"), "_con");
        assert_eq!(file_name("COM1.old"), "_COM1.old");
        assert_eq!(file_name("Combo"), "Combo");
        assert_eq!(file_name("..."), "_");
    }

    #[test]
    fn saved_presets_are_listed_in_front_of_a_device_types_own() {
        let saved = Arc::new(SavedPresets::default());
        let factory = WithSaved::new(Arc::new(BuiltinFactory), saved.clone());
        let own = factory.presets(SYNTH).unwrap();
        assert!(!own.is_empty() && own.presets.iter().all(|preset| preset.category != SAVED_CATEGORY));
        assert_eq!(factory.presets("vst3:NOBODYS"), None);

        saved.save(preset(SYNTH, "Glass", None)).unwrap();
        let list = factory.presets(SYNTH).unwrap();
        assert_eq!(list.status, PresetStatus::Ready);
        assert_eq!(list.len(), own.len() + 1);
        assert_eq!(list.presets[0].path(), "Saved/Glass");
        assert_eq!(list.presets[1..], own.presets[..]);
        // The same list for as long as nothing changes.
        assert!(Arc::ptr_eq(&factory.presets(SYNTH).unwrap().presets, &list.presets));
        assert!(Arc::ptr_eq(&factory.known_presets(SYNTH).unwrap().presets, &list.presets));
        let waited = factory.presets_within(SYNTH, Duration::from_millis(1)).unwrap();
        assert_eq!(waited, list);

        // A type that brings no presets has the saved ones.
        saved.save(preset("vst3:NOBODYS", "Mine", None)).unwrap();
        assert_eq!(names(&factory.presets("vst3:NOBODYS").unwrap().presets), ["Mine"]);
        assert!(saved.delete(SYNTH, "Glass"));
        assert_eq!(factory.presets(SYNTH).unwrap().presets[..], own.presets[..]);
    }
}
