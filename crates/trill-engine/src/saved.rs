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
//!
//! A preset's file is a preset wherever it lies: one that was sent by
//! someone else, or copied from another machine, is read from where it is
//! ([`SavedPresets::read_file`]) and given to a device without becoming
//! one of the user's own.

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
/// The largest preset file, and the largest file of a preset's state, that
/// is read from somewhere other than the store's own folder: a preset's
/// text is a few kilobytes, a plugin's state at most some megabytes.
const MAX_PRESET_TEXT: u64 = 64 * 1024 * 1024;
const MAX_PRESET_STATE: u64 = 256 * 1024 * 1024;
/// The most devices a sound may bring after its first: far more than a
/// chain has, and what bounds the looking for their state files.
pub const MAX_AFTER: usize = 32;

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

/// What to change about a saved preset: each part that is `Some`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub name: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
}

/// A saved preset that is a sound, as a browser lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedSound {
    /// The device type it is a preset of: its first device.
    pub uri: String,
    /// What that type was called when the sound was saved.
    pub device: String,
    pub info: PresetInfo,
    /// What it brings after its first device, by name.
    pub after: Vec<String>,
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
        if preset.after.as_ref().is_some_and(|after| after.len() > MAX_AFTER) {
            return Err(format!("a sound can bring {MAX_AFTER} devices after its first at most"));
        }
        let info = preset.info();
        let mut inner = self.inner.lock();
        let file = match &inner.folder {
            Some(folder) => Some(write(folder, &preset)?),
            None => None,
        };
        if file.is_some() {
            // The states are in their files, and are read from there.
            preset.state = None;
            for device in preset.after.iter_mut().flatten() {
                device.state = None;
            }
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
            let read = |state: PathBuf| match std::fs::read(&state) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(format!("cannot read '{}': {error}", state.display())),
            };
            // A state written into the preset itself stays where there
            // is no file of it.
            if let Some(bytes) = read(state_file(&file))? {
                preset.state = Some(bytes);
            }
            for (place, device) in preset.after.iter_mut().flatten().enumerate() {
                if let Some(bytes) = read(state_file_after(&file, place))? {
                    device.state = Some(bytes);
                }
            }
        }
        Ok(Some(preset))
    }

    /// What the saved preset `info` lists brings after its device, by
    /// name, if it is a sound: `Some` and empty for one that is dry.
    /// `None` for a preset of one device, and for one that is not saved
    /// for the device type `uri`.
    pub fn after(&self, uri: &str, info: &PresetInfo) -> Option<Vec<String>> {
        let name = SavedPreset::name_of(info)?;
        let inner = self.inner.lock();
        let entry = inner.entries.iter().find(|entry| entry.preset.uri == uri && entry.preset.name == name)?;
        entry.preset.after.as_deref().map(trill_core::presets::names_after)
    }

    /// Every saved preset that is a sound, of all device types: the type
    /// it is a preset of, what that was called, the preset, and what it
    /// brings after its device by name.
    pub fn sounds(&self) -> Vec<SavedSound> {
        let inner = self.inner.lock();
        let sounds = inner.entries.iter().filter_map(|entry| {
            let after = entry.preset.after.as_deref()?;
            Some(SavedSound {
                uri: entry.preset.uri.clone(),
                device: entry.preset.device.clone(),
                info: entry.info.clone(),
                after: trill_core::presets::names_after(after),
            })
        });
        sounds.collect()
    }

    /// Whether `file` is named as a saved preset's file is.
    pub fn is_preset_file(file: &Path) -> bool {
        file.extension().is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case(EXTENSION))
    }

    /// The preset in the file at `file`, wherever that lies: whole, with
    /// the state that is kept in a file beside it. The file is one this
    /// program wrote when a preset was saved, here or on another machine;
    /// it is only read. An error is the end of a sentence about the file.
    pub fn read_file(file: &Path) -> Result<SavedPreset, String> {
        let text = read_bounded(file, MAX_PRESET_TEXT)?;
        let text = String::from_utf8(text).map_err(|_| "it is not a saved preset (it is not text)".to_string())?;
        let mut preset =
            SavedPreset::from_json(&text).map_err(|reason| format!("it is not a saved preset ({reason})"))?;
        let read = |state: PathBuf| -> Result<Option<Vec<u8>>, String> {
            if !state.is_file() {
                return Ok(None);
            }
            let bytes = read_bounded(&state, MAX_PRESET_STATE)
                .map_err(|reason| format!("the file with its state, '{}', is no good: {reason}", state.display()))?;
            Ok(Some(bytes))
        };
        if let Some(bytes) = read(state_file(file))? {
            preset.state = Some(bytes);
        }
        for (place, device) in preset.after.iter_mut().flatten().enumerate() {
            if let Some(bytes) = read(state_file_after(file, place))? {
                device.state = Some(bytes);
            }
        }
        Ok(preset)
    }

    /// Gives the preset the device type `uri` has saved under `name`
    /// another name, description or tags, whichever of them `change` has,
    /// and leaves the rest of it as it is: its devices and their states.
    /// A new name that another saved preset of the type has is refused
    /// (one that differs from its own in case alone is a new name for it).
    pub fn change(&self, uri: &str, name: &str, change: &Change) -> Result<Saved, String> {
        let found = {
            let inner = self.inner.lock();
            let same = |entry: &&Entry| entry.preset.uri == uri && same_name(&entry.preset.name, name.trim());
            inner.entries.iter().find(same).map(|entry| entry.info.clone())
        };
        let Some(info) = found else {
            return Err(format!("there is no saved preset '{}' of '{uri}'", name.trim()));
        };
        let mut preset = self.load(uri, &info)?.ok_or_else(|| format!("'{}' is no saved preset", info.name))?;
        let old = preset.name.clone();
        if let Some(wanted) = &change.name {
            let wanted = SavedPreset::checked_name(wanted)?;
            let taken = self.list(uri).iter().any(|other| same_name(&other.name, &wanted));
            if taken && !same_name(&wanted, &old) {
                return Err(format!(
                    "'{uri}' has a saved preset called '{wanted}' already; delete that one first, or choose another name"
                ));
            }
            preset.name = wanted;
        }
        if let Some(description) = &change.description {
            preset.description = description.trim().to_string();
        }
        if let Some(tags) = &change.tags {
            preset.tags = tags.iter().map(|tag| tag.trim().to_string()).filter(|tag| !tag.is_empty()).collect();
        }
        let renamed = !same_name(&preset.name, &old);
        // Under its new name first: there is no moment without it.
        let saved = self.save(preset)?;
        if renamed {
            self.delete(uri, &old);
        }
        Ok(Saved { replaced: false, ..saved })
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

/// The file at `file`, whole, if it is no larger than `most`; or what to
/// say of it.
fn read_bounded(file: &Path, most: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let cannot = |error: std::io::Error| match error.kind() {
        std::io::ErrorKind::NotFound => "there is no such file".to_string(),
        _ => format!("it cannot be read ({error})"),
    };
    let found = std::fs::metadata(file).map_err(cannot)?;
    if !found.is_file() {
        return Err("it is a folder, not a file".to_string());
    }
    if found.len() > most {
        return Err(format!("it is too large to be a preset ({} MB)", found.len() / (1024 * 1024)));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file).map_err(cannot)?.take(most).read_to_end(&mut bytes).map_err(cannot)?;
    Ok(bytes)
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

/// The file the state of a sound's device is kept in that comes `place`
/// places after its first (from 0): beside the sound's own, numbered.
fn state_file_after(preset_file: &Path, place: usize) -> PathBuf {
    let mut name = preset_file.as_os_str().to_owned();
    name.push(format!("{STATE_SUFFIX}-{}", place + 1));
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
    remove_states_after(preset_file, 0);
}

/// Removes the state files of a sound's devices from `place` on: every
/// one there is, since they are numbered without a gap that matters.
fn remove_states_after(preset_file: &Path, place: usize) {
    for place in place..MAX_AFTER {
        let file = state_file_after(preset_file, place);
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
    // A sound's other devices, each in a file of its own; and none left
    // of a sound that had more of them.
    let after = preset.after.as_deref().unwrap_or_default();
    for (place, device) in after.iter().enumerate() {
        let state = state_file_after(&file, place);
        match &device.state {
            Some(bytes) => replace(&state, bytes)?,
            None => match std::fs::remove_file(&state) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("cannot remove '{}': {error}", state.display())),
            },
        }
    }
    remove_states_after(&file, after.len());
    // What is written says the format it is written in: a preset made
    // here without one is of this build.
    let format = preset.format.or(Some(trill_core::project::FORMAT_VERSION));
    let mut written = SavedPreset { state: None, format, ..preset.clone() };
    for device in written.after.iter_mut().flatten() {
        device.state = None;
    }
    replace(&file, written.to_json().as_bytes())?;
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

    fn busy(&self) -> bool {
        self.inner.busy()
    }

    fn takes_time(&self, uri: &str) -> bool {
        self.inner.takes_time(uri)
    }

    fn beside(&self, uri: &str) -> trill_core::device::Beside {
        self.inner.beside(uri)
    }

    fn module(&self, uri: &str) -> Option<std::path::PathBuf> {
        self.inner.module(uri)
    }

    fn repeating(&self, uri: &str) -> Option<trill_core::device::Repeating> {
        self.inner.repeating(uri)
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

    fn plays_drums(&self, uri: &str) -> bool {
        self.inner.plays_drums(uri)
    }

    fn preset_file(&self, uri: &str, file: &Path, within: Option<&str>) -> Option<Result<PresetInfo, String>> {
        self.inner.preset_file(uri, file, within)
    }

    fn preset_file_type(&self, file: &Path) -> Option<String> {
        self.inner.preset_file_type(file)
    }

    fn preset_state(&self, uri: &str, file: &Path) -> Option<Result<(PresetInfo, Vec<u8>), String>> {
        self.inner.preset_state(uri, file)
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
            format: Some(trill_core::project::FORMAT_VERSION),
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

    /// A guitar through an amplifier and a reverb: three devices, two of
    /// them with a state.
    fn tone(name: &str) -> SavedPreset {
        use trill_core::presets::SoundDevice;
        let amp = SoundDevice { state: Some(vec![7, 7]), ..SoundDevice::new(AMP, "Amp", &[("gain", 0.8)]) };
        let reverb = SoundDevice::new("builtin:reverb", "Reverb", &[("decay", 1.5)]);
        SavedPreset { after: Some(vec![amp, reverb]), ..preset("vst3:GUITAR", name, Some(&[1])) }
    }

    #[test]
    fn a_sound_is_kept_with_the_state_of_each_of_its_devices() {
        let folder = tempfile::tempdir().unwrap();
        let store = SavedPresets::default();
        store.open(folder.path().to_path_buf());
        let saved = store.save(tone("Tone")).unwrap();
        let file = saved.file.clone().unwrap();
        // One text file that says what the sound is made of, and beside it
        // a file for each state: none of them in the text.
        assert_eq!(std::fs::read(state_file(&file)).unwrap(), [1]);
        assert_eq!(std::fs::read(state_file_after(&file, 0)).unwrap(), [7, 7]);
        assert!(!state_file_after(&file, 1).exists(), "the reverb has no state");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(r#""after""#) && !text.contains("state"), "{text}");

        // It is a preset of its first device's type, and says what follows.
        assert_eq!(names(&store.list("vst3:GUITAR")), ["Tone"]);
        assert_eq!(store.after("vst3:GUITAR", &saved.info), Some(vec!["Amp".to_string(), "Reverb".to_string()]));
        let plain = store.save(preset("vst3:GUITAR", "Plain", None)).unwrap();
        assert_eq!(store.after("vst3:GUITAR", &plain.info), None, "a preset of one device says nothing");
        let sounds = store.sounds();
        assert_eq!(sounds.len(), 1);
        assert_eq!(
            (sounds[0].uri.as_str(), sounds[0].info.name.as_str(), sounds[0].after.len()),
            ("vst3:GUITAR", "Tone", 2)
        );

        // Whole again in another run of the program, and from its file
        // wherever that lies.
        let again = SavedPresets::default();
        again.open(folder.path().to_path_buf());
        assert_eq!(again.load("vst3:GUITAR", &saved.info).unwrap().unwrap(), tone("Tone"));
        assert_eq!(SavedPresets::read_file(&file).unwrap(), tone("Tone"));

        // Saved again with less after it, nothing of the longer sound is
        // left; and nothing at all once it is deleted.
        let shorter = SavedPreset { after: Some(Vec::new()), ..tone("Tone") };
        store.save(shorter.clone()).unwrap();
        assert!(!state_file_after(&file, 0).exists());
        assert_eq!(store.load("vst3:GUITAR", &saved.info).unwrap().unwrap(), shorter);
        assert_eq!(store.after("vst3:GUITAR", &saved.info), Some(Vec::new()), "dry, and it says so");
        store.save(tone("Tone")).unwrap();
        assert!(store.delete("vst3:GUITAR", "Tone"));
        assert!(!file.exists() && !state_file(&file).exists() && !state_file_after(&file, 0).exists());

        // In memory the states stay where they are.
        let memory = SavedPresets::default();
        let saved = memory.save(tone("Tone")).unwrap();
        assert_eq!(memory.load("vst3:GUITAR", &saved.info).unwrap().unwrap(), tone("Tone"));
        let long = SavedPreset { after: Some(vec![Default::default(); MAX_AFTER + 1]), ..tone("Long") };
        assert!(memory.save(long).unwrap_err().contains("at most"));
    }

    #[test]
    fn a_saved_preset_is_renamed_and_described_with_everything_it_holds() {
        let folder = tempfile::tempdir().unwrap();
        let store = SavedPresets::default();
        store.open(folder.path().to_path_buf());
        let first = store.save(tone("Tone")).unwrap();
        let old_file = first.file.clone().unwrap();
        store.save(preset("vst3:GUITAR", "Clean", None)).unwrap();

        // Another name: the same sound, state for state, under it; and
        // nothing left under the old one, on disk or in the list.
        let rename = Change { name: Some("  Djent Rhythm ".into()), ..Change::default() };
        let renamed = store.change("vst3:GUITAR", "tone", &rename).unwrap();
        assert_eq!(renamed.info.path(), "Saved/Djent Rhythm");
        assert_eq!(names(&store.list("vst3:GUITAR")), ["Clean", "Djent Rhythm"]);
        let whole = store.load("vst3:GUITAR", &renamed.info).unwrap().unwrap();
        assert_eq!(whole, SavedPreset { name: "Djent Rhythm".into(), ..tone("Tone") });
        assert!(!old_file.exists() && !state_file(&old_file).exists() && !state_file_after(&old_file, 0).exists());
        let new_file = renamed.file.clone().unwrap();
        assert_eq!(std::fs::read(state_file_after(&new_file, 0)).unwrap(), [7, 7]);

        // What it says of itself, with its name and its devices as they are.
        let said = Change {
            description: Some(" tight and dry ".into()),
            tags: Some(vec!["guitar".into(), " ".into(), "rhythm".into()]),
            ..Change::default()
        };
        let described = store.change("vst3:GUITAR", "Djent Rhythm", &said).unwrap();
        assert_eq!((described.info.description.as_str(), described.info.tags.len()), ("tight and dry", 2));
        assert_eq!(store.load("vst3:GUITAR", &described.info).unwrap().unwrap().after, tone("Tone").after);
        assert_eq!(store.sounds()[0].info.description, "tight and dry");
        // Its case alone is a new name for it, and no clash with itself.
        let cased = Change { name: Some("DJENT rhythm".into()), ..Change::default() };
        assert_eq!(store.change("vst3:GUITAR", "Djent Rhythm", &cased).unwrap().info.name, "DJENT rhythm");
        assert_eq!(names(&store.list("vst3:GUITAR")), ["Clean", "DJENT rhythm"]);
        let again = SavedPresets::default();
        again.open(folder.path().to_path_buf());
        assert_eq!(names(&again.list("vst3:GUITAR")), ["Clean", "DJENT rhythm"]);

        // A name that is taken, a name that will not do, and a preset
        // there is none of: each is said, and nothing is changed.
        let taken = Change { name: Some("clean".into()), ..Change::default() };
        assert!(store.change("vst3:GUITAR", "DJENT rhythm", &taken).unwrap_err().contains("called 'clean' already"));
        let bad = Change { name: Some("a/b".into()), ..Change::default() };
        assert!(store.change("vst3:GUITAR", "DJENT rhythm", &bad).unwrap_err().contains("cannot have '/'"));
        assert!(
            store.change("vst3:GUITAR", "Gone", &Change::default()).unwrap_err().contains("no saved preset 'Gone'")
        );
        assert_eq!(names(&store.list("vst3:GUITAR")), ["Clean", "DJENT rhythm"]);
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
