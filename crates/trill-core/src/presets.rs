//! The presets a device type offers, as something to browse and choose
//! from.
//!
//! A built-in device has a dozen presets that are known the moment the
//! program starts. A plugin may have thousands, in banks and folders, and
//! finding them means running the plugin's own code or walking its preset
//! folders, which takes seconds and happens in the background. Both look
//! the same from here: a [`PresetList`] that says how far along it is and
//! can be searched, paged and looked things up in.
//!
//! How a preset is *loaded* is the device's business: see
//! [`DeviceController::load_preset`](crate::device::DeviceController::load_preset).
//!
//! A preset need not be one of the listed ones. People keep presets they
//! bought or were sent wherever they keep their sounds, so a preset may
//! also be asked for as the path of its file ([`PresetFile`]); what kind
//! of file a device type takes is the type's business
//! ([`DeviceFactory::preset_file`](crate::device::DeviceFactory::preset_file)).

use crate::ids::DeviceId;
use crate::project::{Device, EditError, Project};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What separates the parts of a [`PresetInfo::category`].
pub const CATEGORY_SEPARATOR: char = '/';

/// The category the presets someone saved are filed under, beside the
/// ones a device type brings.
pub const SAVED_CATEGORY: &str = "Saved";
/// What the locator of a saved preset begins with; its name follows.
const SAVED_LOCATOR: &str = "saved|";
/// The longest name a saved preset may have.
const SAVED_NAME_LENGTH: usize = 80;

/// A device's sound, kept under a name: what a plugin that offers a host
/// no presets is given instead of them, and what anyone who has found a
/// sound keeps it as. It belongs to the device type it was saved from and
/// is listed with that type's own presets, under [`SAVED_CATEGORY`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedPreset {
    /// The device type it is a preset of.
    pub uri: String,
    /// What that type was called when the preset was saved, for someone
    /// reading the file.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Parameter values by parameter id: all of them for a device whose
    /// sound is its parameters, and for a plugin the ones that go on top
    /// of its state.
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    /// The device's own state, for one that has any: a plugin's patch, a
    /// sample player's files.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::project::state_base64")]
    pub state: Option<Vec<u8>>,
    /// The project format of the build that saved it
    /// ([`FORMAT_VERSION`](crate::project::FORMAT_VERSION)), which is what
    /// its values are read by. A preset saved before format 7 does not
    /// say, and is read by what it holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<u32>,
    /// The devices that come after it, for a preset that is a *sound*: an
    /// instrument and the effects behind it, kept as one thing. In the
    /// order the signal goes through them; empty for a sound that is its
    /// first device alone and says so. `None` for a preset of one device,
    /// which says nothing of what comes after it. See [`SoundDevice`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Vec<SoundDevice>>,
}

/// One of the devices a sound brings after its first: an effect as it was
/// when the sound was saved, with everything a project would keep of it.
///
/// # What a sound is, and what it is not
///
/// A sound is a preset of its first device (the instrument, on a track
/// that has one) that says what follows: [`SavedPreset::after`] for one
/// that was saved, [`Preset::after`](crate::device::Preset::after) for one
/// a built-in device brings. It is listed, searched and asked for as that
/// device type's preset, and given to a device or a new track like any
/// other; the effects are made with it ([`Project::bring_after`]).
///
/// It holds each device's type, parameter values, state and whether it is
/// switched on. It does not hold what places a sound in one song:
///
/// * the track a device listens to ([`Device::sidechain`]): that is a
///   track of one song, by an id that means nothing in another;
/// * sends, the fader, the pan, mute and solo, the group: the mix, which
///   is made anew for every song the sound is put in;
/// * automation: that is a stretch of one song's time;
/// * what the track's notes mean (its profile): known by the instrument's
///   type, and for a kit read from the instrument itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoundDevice {
    /// The device type.
    pub uri: String,
    /// What that type is called, for the sentence that says a machine has
    /// not got it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    /// Off for an effect that was bypassed: it is there to be switched on.
    #[serde(default = "switched_on", skip_serializing_if = "is_switched_on")]
    pub enabled: bool,
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::project::state_base64")]
    pub state: Option<Vec<u8>>,
}

fn switched_on() -> bool {
    true
}

fn is_switched_on(enabled: &bool) -> bool {
    *enabled
}

impl Default for SoundDevice {
    fn default() -> Self {
        Self { uri: String::new(), device: String::new(), enabled: true, params: BTreeMap::new(), state: None }
    }
}

impl SoundDevice {
    /// A device of the type `uri`, called `device`, with these parameter
    /// values and everything else as the type comes.
    pub fn new(uri: &str, device: &str, params: &[(&str, f64)]) -> Self {
        Self {
            uri: uri.to_string(),
            device: device.to_string(),
            params: params.iter().map(|(id, value)| ((*id).to_string(), *value)).collect(),
            ..Self::default()
        }
    }

    /// What to call the device: its type's name, or its URI where the
    /// sound does not say.
    pub fn called(&self) -> &str {
        if self.device.trim().is_empty() { &self.uri } else { &self.device }
    }
}

/// The names of the devices a sound brings after its first, for saying in
/// a few words what it is made of.
pub fn names_after(after: &[SoundDevice]) -> Vec<String> {
    after.iter().map(|device| device.called().to_string()).collect()
}

impl SavedPreset {
    /// The preset as a preset list has it.
    pub fn info(&self) -> PresetInfo {
        let locator = format!("{SAVED_LOCATOR}{}", self.name);
        PresetInfo {
            id: short_id(&locator),
            name: self.name.clone(),
            category: SAVED_CATEGORY.to_string(),
            author: String::new(),
            tags: self.tags.clone(),
            description: self.description.clone(),
            locator,
        }
    }

    /// The preset as the text of a file.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a saved preset is always serializable")
    }

    /// The preset a file's text holds, or what is wrong with the text.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let preset: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        if preset.uri.is_empty() {
            return Err("it does not say which device it is a preset of".to_string());
        }
        Self::checked_name(&preset.name)?;
        // A preset of a built-in device holds every value the device had
        // when it was saved: one from before a device was changed is made
        // to sound as it did.
        let mut preset = preset;
        crate::formats::keep_complete(preset.format, &preset.uri, &mut preset.params);
        // And so is every device a sound brings after its first.
        for (index, device) in preset.after.iter_mut().flatten().enumerate() {
            if device.uri.is_empty() {
                return Err(format!("the device in place {} after its first does not say which it is", index + 1));
            }
            crate::formats::keep_complete(preset.format, &device.uri, &mut device.params);
        }
        preset.format = Some(crate::project::FORMAT_VERSION);
        Ok(preset)
    }

    /// Whether the preset is a sound: one that says what comes after its
    /// device, if only that nothing does.
    pub fn is_sound(&self) -> bool {
        self.after.is_some()
    }

    /// The name `preset` was saved under, if it is a saved preset and not
    /// one a device type brings.
    pub fn name_of(preset: &PresetInfo) -> Option<&str> {
        preset.locator.strip_prefix(SAVED_LOCATOR)
    }

    /// A name as a preset may be saved under: without the space around
    /// it; or why it will not do.
    pub fn checked_name(name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a preset needs a name".to_string());
        }
        if name.chars().count() > SAVED_NAME_LENGTH {
            return Err(format!("a preset's name can be {SAVED_NAME_LENGTH} characters at most"));
        }
        if let Some(bad) = name.chars().find(|c| c.is_control() || matches!(c, '/' | '\\' | '|')) {
            return Err(format!("a preset's name cannot have {bad:?} in it"));
        }
        Ok(name.to_string())
    }
}

/// One preset a device type offers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetInfo {
    /// What the preset is asked for by. It is unique among the presets of
    /// its device type, short, and the same from one run of the program
    /// to the next for as long as the preset stays where it is.
    pub id: String,
    pub name: String,
    /// Where a browser files the preset: its bank, its folder, or both as
    /// a path such as `Factory/Pads`. Empty for a preset that is filed
    /// nowhere.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    /// Who made it, where the format says.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// Words the format files the preset under: `pad`, `warm`, `factory`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Where the device finds the preset when it is asked to load it. It
    /// means something only to the device type the preset belongs to, and
    /// is not for showing: an [`id`](Self::id) is the short form of it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub locator: String,
}

impl PresetInfo {
    /// The preset's name with where it is filed in front: `Factory/Pads/Warm`.
    pub fn path(&self) -> String {
        if self.category.is_empty() {
            self.name.clone()
        } else {
            format!("{}{CATEGORY_SEPARATOR}{}", self.category, self.name)
        }
    }

    /// The reference to keep in a project once the preset is loaded.
    pub fn reference(&self) -> PresetRef {
        PresetRef { id: self.id.clone(), name: self.name.clone() }
    }
}

/// Which preset a device in a project was last given: what it was started
/// from, not a promise that nothing has been changed since.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetRef {
    pub id: String,
    pub name: String,
}

/// What the id of a reference begins with on a device that a sound
/// brought: no preset of the device's own type has such an id.
const BROUGHT: &str = "sound|";

impl PresetRef {
    /// The reference an effect carries that the sound `sound` brought: it
    /// started from that sound, and goes when its track is given another.
    pub fn brought_by(sound: &PresetRef) -> Self {
        Self { id: format!("{BROUGHT}{}", sound.id), name: sound.name.clone() }
    }

    /// The id of the sound that brought the device this is the reference
    /// of; `None` for a device that was put on its chain otherwise, or
    /// given a preset of its own since.
    pub fn bringer(&self) -> Option<&str> {
        self.id.strip_prefix(BROUGHT)
    }
}

/// What [`Project::bring_after`] did to a chain.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Brought {
    /// The devices that were made, in the order they are in.
    pub made: Vec<DeviceId>,
    /// The devices an earlier sound had brought, which went.
    pub removed: Vec<Device>,
}

impl Project {
    /// Makes what follows the device `first` on its chain the effects of
    /// the sound it was just given: `after`, directly behind it, in their
    /// order, each marked as brought by `sound` ([`PresetRef::brought_by`]).
    ///
    /// What it replaces: the devices behind `first` that an earlier sound
    /// brought, whichever sound that was, and their automation with them.
    /// A device that was put on the chain otherwise, or was given a preset
    /// of its own since it came, is not a sound's and stays, behind the
    /// new ones. Nothing in front of `first` is touched.
    ///
    /// The devices are made with what the sound holds and with nothing
    /// else: none of them listens to another track.
    pub fn bring_after(
        &mut self,
        first: DeviceId,
        sound: &PresetRef,
        after: &[SoundDevice],
    ) -> Result<Brought, EditError> {
        let chain = self.device_chain(first)?;
        let devices = self.chain(chain)?;
        let at = devices.iter().position(|device| device.id == first).ok_or(EditError::DeviceNotFound(first))?;
        let going: Vec<DeviceId> = devices[at + 1..]
            .iter()
            .filter(|device| device.preset.as_ref().is_some_and(|preset| preset.bringer().is_some()))
            .map(|device| device.id)
            .collect();
        let mut brought = Brought::default();
        for id in going {
            brought.removed.push(self.remove_device(id)?);
        }
        for (place, device) in after.iter().enumerate() {
            let id = self.add_device(chain, device.uri.clone(), device.called(), Some(at + 1 + place))?;
            let made = self.device_mut(id)?;
            made.enabled = device.enabled;
            made.params = device.params.clone();
            made.state = device.state.clone();
            made.preset = Some(PresetRef::brought_by(sound));
            brought.made.push(id);
        }
        Ok(brought)
    }
}

/// How much of a device type's presets is known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PresetStatus {
    /// The list is complete.
    #[default]
    Ready,
    /// The presets are being read. The list holds what is known so far,
    /// which may be what an earlier run of the program found.
    Reading,
    /// The presets could not be read, and why. The list holds what could
    /// be read all the same, or what an earlier run found.
    Failed(String),
}

/// The presets of one device type, as far as they are known right now.
///
/// A snapshot: cheap to clone and to hand around, and never changed by
/// the reading that may still be going on. Ask again for a newer one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PresetList {
    pub status: PresetStatus,
    /// In the order a browser lists them: as the device gives them.
    pub presets: Arc<[PresetInfo]>,
}

/// What to show of a [`PresetList`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresetQuery<'a> {
    /// Words to look for, in any order and in either case. Each must be
    /// found somewhere in a preset: its name, category, author, tags or
    /// description. Empty: every preset.
    pub text: &'a str,
    /// Only the presets filed under this category or below it. Any
    /// stretch of the path will do, and of each part of it any piece:
    /// `pads` finds `Factory/Pads/Soft`, and `factory/pad` finds
    /// `Big Synth Factory Bank/Pads`.
    pub category: Option<&'a str>,
    /// How many of the matching presets to skip, and the most to return
    /// after them.
    pub offset: usize,
    pub limit: usize,
}

/// One page of the presets that match a [`PresetQuery`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetPage<'a> {
    /// How many presets match, on every page together.
    pub total: usize,
    /// How many of them come before this page.
    pub offset: usize,
    /// Best match first; where nothing was searched for, in the list's
    /// own order.
    pub presets: Vec<&'a PresetInfo>,
}

/// What looking a preset up by name or id came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetLookup<'a> {
    Found(&'a PresetInfo),
    /// Several presets have that name. They are told apart by their
    /// category (`Pads/Warm`) or their id.
    Ambiguous(Vec<&'a PresetInfo>),
    /// There is none. `similar` are the nearest things there are.
    Missing {
        similar: Vec<&'a PresetInfo>,
    },
}

/// How many near misses a failed lookup comes back with.
const SUGGESTIONS: usize = 8;

impl PresetList {
    /// A complete list.
    pub fn ready(presets: impl Into<Arc<[PresetInfo]>>) -> Self {
        Self { status: PresetStatus::Ready, presets: presets.into() }
    }

    /// A list that is still being read, with what is known so far.
    pub fn reading(presets: impl Into<Arc<[PresetInfo]>>) -> Self {
        Self { status: PresetStatus::Reading, presets: presets.into() }
    }

    pub fn len(&self) -> usize {
        self.presets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.presets.is_empty()
    }

    /// Whether more presets may still turn up without anyone asking for
    /// the list to be read again.
    pub fn is_reading(&self) -> bool {
        self.status == PresetStatus::Reading
    }

    /// The preset with this id.
    pub fn get(&self, id: &str) -> Option<&PresetInfo> {
        self.presets.iter().find(|preset| preset.id == id)
    }

    /// Finds the preset someone asked for by `wanted`: its id, its name,
    /// or its name with as much of its category in front as tells it
    /// apart (`Pads/Warm`). Names are compared without regard to case.
    pub fn find(&self, wanted: &str) -> PresetLookup<'_> {
        let wanted = wanted.trim();
        if let Some(preset) = self.get(wanted) {
            return PresetLookup::Found(preset);
        }
        let lowered = wanted.to_lowercase();
        let named: Vec<&PresetInfo> =
            self.presets.iter().filter(|preset| preset.name.to_lowercase() == lowered).collect();
        let found = if named.is_empty() {
            // `Pads/Warm`: the end of a preset's path, from the start of
            // one of its parts.
            self.presets
                .iter()
                .filter(|preset| {
                    let path = preset.path().to_lowercase();
                    path.strip_suffix(lowered.as_str())
                        .is_some_and(|before| before.is_empty() || before.ends_with(CATEGORY_SEPARATOR))
                })
                .collect()
        } else {
            named
        };
        match found.as_slice() {
            [] => {
                let query = PresetQuery { text: wanted, category: None, offset: 0, limit: SUGGESTIONS };
                let mut similar = self.search(&query).presets;
                if similar.is_empty() {
                    // Nothing has all of the words; any of them will do.
                    let words = words(wanted);
                    similar = self
                        .presets
                        .iter()
                        .filter(|preset| {
                            let name = preset.name.to_lowercase();
                            words.iter().any(|word| word.len() > 2 && name.contains(word.as_str()))
                        })
                        .take(SUGGESTIONS)
                        .collect();
                }
                PresetLookup::Missing { similar }
            }
            [one] => PresetLookup::Found(one),
            _ => PresetLookup::Ambiguous(found),
        }
    }

    /// The presets that match `query`, a page of them.
    pub fn search(&self, query: &PresetQuery<'_>) -> PresetPage<'_> {
        let mut matching = self.matches(query.text, query.category);
        // Stable: within a rank the list's own order stands.
        matching.sort_by_key(|(rank, _)| *rank);
        PresetPage {
            total: matching.len(),
            offset: query.offset.min(matching.len()),
            presets: matching.into_iter().skip(query.offset).take(query.limit).map(|(_, preset)| preset).collect(),
        }
    }

    /// Every preset that has the words of `text` (and is filed under
    /// `category`, if one is given), in the list's own order, each with
    /// how near a match it is: 0 for a name that is the text, 1 for a name
    /// that begins with it, 2 for a name that has every word, 3 where the
    /// category and tags are needed to find them all, 4 where the author
    /// or the description is. For those who put the matches of several
    /// lists together; [`search`](Self::search) pages those of one.
    pub fn matches(&self, text: &str, category: Option<&str>) -> Vec<(u8, &PresetInfo)> {
        let words = words(text);
        let category = category.map(parts).filter(|parts| !parts.is_empty());
        let whole = text.trim().to_lowercase();

        let mut matching: Vec<(u8, &PresetInfo)> = Vec::new();
        for preset in self.presets.iter() {
            if let Some(wanted) = &category
                && !filed_under(&preset.category, wanted)
            {
                continue;
            }
            if words.is_empty() {
                matching.push((0, preset));
                continue;
            }
            let name = preset.name.to_lowercase();
            let filed = format!("{} {}", preset.category, preset.tags.join(" ")).to_lowercase();
            let rest = format!("{} {}", preset.author, preset.description).to_lowercase();
            let everywhere = |word: &String| name.contains(word.as_str()) || filed.contains(word.as_str());
            // The nearer a match is to the name, the better it is.
            let rank = if name == whole {
                0
            } else if name.starts_with(whole.as_str()) {
                1
            } else if words.iter().all(|word| name.contains(word.as_str())) {
                2
            } else if words.iter().all(everywhere) {
                3
            } else if words.iter().all(|word| everywhere(word) || rest.contains(word.as_str())) {
                4
            } else {
                continue;
            };
            matching.push((rank, preset));
        }
        matching
    }

    /// Every category that has presets, with how many, by name. Presets
    /// filed nowhere count under the empty name.
    pub fn categories(&self) -> Vec<(String, usize)> {
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for preset in self.presets.iter() {
            *counts.entry(preset.category.as_str()).or_default() += 1;
        }
        counts.into_iter().map(|(category, count)| (category.to_string(), count)).collect()
    }
}

/// The words of a search, lowered.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_lowercase).collect()
}

/// The parts of a category path, lowered.
fn parts(category: &str) -> Vec<String> {
    category.split(CATEGORY_SEPARATOR).map(|part| part.trim().to_lowercase()).filter(|part| !part.is_empty()).collect()
}

/// Whether `category` has parts in a row, anywhere along it, that hold
/// the parts `wanted`, each in its place.
fn filed_under(category: &str, wanted: &[String]) -> bool {
    let parts = parts(category);
    parts
        .windows(wanted.len())
        .any(|window| window.iter().zip(wanted).all(|(part, wanted)| part.contains(wanted.as_str())))
}

/// What separates the path of a file that holds several presets from the
/// one that is meant: `C:\Kits\presets#Black Metal`.
pub const WITHIN_FILE: char = '#';

/// A preset asked for as a file rather than by its name: the file, and for
/// a file that holds several presets which of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetFile {
    pub path: PathBuf,
    /// The preset in the file that is meant: its name, its category and
    /// name, or its id, as [`PresetList::find`] takes them.
    pub within: Option<String>,
}

impl PresetFile {
    /// `wanted`, what was asked for as a preset, as a file; `None` if it
    /// is a preset's name or id.
    ///
    /// A file is asked for by its whole path, from the drive or the root
    /// down: that is what tells it from a name, which may have slashes of
    /// its own (`Pads/Warm`). A path may be followed by [`WITHIN_FILE`] and
    /// the preset that is meant. Since a `#` may be part of a path too,
    /// the file is the longest stretch before one that is a file on the
    /// disk; a path that leads to no file is taken whole, and whoever goes
    /// on to read it says that there is none.
    pub fn asked(wanted: &str) -> Option<Self> {
        let wanted = wanted.trim();
        if !Path::new(wanted).is_absolute() {
            return None;
        }
        let whole = Self { path: PathBuf::from(wanted), within: None };
        if !wanted.contains(WITHIN_FILE) || whole.path.is_file() {
            return Some(whole);
        }
        let split = wanted.rmatch_indices(WITHIN_FILE).find_map(|(at, _)| {
            let (path, within) = (Path::new(&wanted[..at]), wanted[at + 1..].trim());
            let found = !within.is_empty() && path.is_file();
            found.then(|| Self { path: path.to_path_buf(), within: Some(within.to_string()) })
        });
        Some(split.unwrap_or(whole))
    }

    /// The name to show for a file's preset where the file itself gives
    /// none: the file's name without its extension.
    pub fn name_of(path: &Path) -> String {
        path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// A short id for something that is found by `locator`: the same for the
/// same locator every time, and for practical purposes different for
/// different ones. Ten digits of a 64-bit FNV-1a hash.
pub fn short_id(locator: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in locator.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:010x}", hash >> 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, category: &str, author: &str, tags: &[&str]) -> PresetInfo {
        let locator = format!("{category}|{name}");
        PresetInfo {
            id: short_id(&locator),
            name: name.into(),
            category: category.into(),
            author: author.into(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            description: String::new(),
            locator,
        }
    }

    fn list() -> PresetList {
        PresetList::ready(vec![
            preset("Warm Pad", "Factory/Pads", "Ada", &["pad", "warm"]),
            preset("Glass", "Factory/Pads", "Ada", &["pad", "bright"]),
            preset("Warm Bass", "Factory/Basses", "Bo", &["bass"]),
            preset("Init", "Factory/Templates", "", &[]),
            preset("Warm Pad", "User/Pads", "Me", &["pad"]),
            preset("Padlock", "User/Leads", "Me", &["lead"]),
            preset("Loose", "", "", &[]),
        ])
    }

    fn names(page: &PresetPage<'_>) -> Vec<String> {
        page.presets.iter().map(|preset| preset.path()).collect()
    }

    #[test]
    fn a_saved_preset_is_listed_under_saved_and_keeps_its_state_as_text() {
        let saved = SavedPreset {
            uri: "vst3:ABC".into(),
            device: "Amp".into(),
            name: "Lead Tone".into(),
            description: "bright, for solos".into(),
            tags: vec!["lead".into()],
            params: [("101".to_string(), 0.25)].into(),
            state: Some(vec![0, 255, 7]),
            format: Some(crate::project::FORMAT_VERSION),
            after: None,
        };
        let info = saved.info();
        assert_eq!((info.path().as_str(), info.tags.as_slice()), ("Saved/Lead Tone", &["lead".to_string()][..]));
        assert_eq!(SavedPreset::name_of(&info), Some("Lead Tone"));
        assert_eq!(info.id, saved.info().id, "the same every time");
        assert_ne!(info.id, SavedPreset { name: "Lead Tone 2".into(), ..saved.clone() }.info().id);
        assert_eq!(SavedPreset::name_of(&preset("Lead Tone", "Factory", "", &[])), None);

        let written = serde_json::to_string(&saved).unwrap();
        assert!(written.contains(r#""state":"AP8H""#), "{written}");
        assert_eq!(serde_json::from_str::<SavedPreset>(&written).unwrap(), saved);
        assert_eq!(SavedPreset::from_json(&saved.to_json()), Ok(saved.clone()));
        assert!(SavedPreset::from_json("{").is_err() && SavedPreset::from_json(r#"{"uri":"","name":"x"}"#).is_err());
        assert!(SavedPreset::from_json(r#"{"uri":"builtin:reverb","name":"a/b"}"#).is_err());
        // What is not there is left out, and is not missed when read.
        let plain = SavedPreset { uri: "builtin:reverb".into(), name: "Hall".into(), ..SavedPreset::default() };
        let written = serde_json::to_string(&plain).unwrap();
        assert_eq!(written, r#"{"uri":"builtin:reverb","name":"Hall","params":{}}"#);
        assert_eq!(serde_json::from_str::<SavedPreset>(r#"{"uri":"builtin:reverb","name":"Hall"}"#).unwrap(), plain);

        // A preset of a built-in device is read by the format it was saved
        // in: one that does not say was saved before format 7, when a
        // reverb with its legacy switch off was the reverb of formats 2 to
        // 6; one that says 7 means the reverb as it is.
        let hall = |format: &str| {
            let text =
                format!(r#"{{"uri":"builtin:reverb","name":"Hall","params":{{"decay":3.0,"legacy":0.0}}{format}}}"#);
            SavedPreset::from_json(&text).unwrap()
        };
        assert_eq!(hall("").params.get("legacy"), Some(&2.0));
        assert_eq!(hall(r#","format":6"#).params.get("legacy"), Some(&2.0));
        assert_eq!(hall(r#","format":7"#).params.get("legacy"), Some(&0.0));
        // Read once, it is of this format, and stays what it was made.
        let again = SavedPreset::from_json(&hall("").to_json()).unwrap();
        assert_eq!((again.format, again.params.get("legacy")), (Some(crate::project::FORMAT_VERSION), Some(&2.0)));
    }

    #[test]
    fn a_name_to_save_under_is_trimmed_and_checked() {
        assert_eq!(SavedPreset::checked_name("  Lead Tone  ").as_deref(), Ok("Lead Tone"));
        assert_eq!(SavedPreset::checked_name(" ").unwrap_err(), "a preset needs a name");
        assert!(SavedPreset::checked_name("Pads/Warm").unwrap_err().contains("cannot have '/'"));
        assert!(SavedPreset::checked_name("a|b").is_err() && SavedPreset::checked_name("a\nb").is_err());
        assert!(SavedPreset::checked_name("a\\b").is_err());
        assert!(SavedPreset::checked_name(&"x".repeat(81)).unwrap_err().contains("80 characters"));
        assert!(SavedPreset::checked_name(&"é".repeat(80)).is_ok());
    }

    #[test]
    fn a_search_wants_every_word_and_puts_names_first() {
        let list = list();
        let search = |text| list.search(&PresetQuery { text, category: None, offset: 0, limit: 10 });

        let warm = search("warm");
        assert_eq!(names(&warm), ["Factory/Pads/Warm Pad", "Factory/Basses/Warm Bass", "User/Pads/Warm Pad"]);
        assert_eq!(warm.total, 3);

        // Both words, in any order and case; the bass has only one.
        assert_eq!(names(&search("PAD warm")), ["Factory/Pads/Warm Pad", "User/Pads/Warm Pad"]);
        // A name that begins with the text comes before one that has it
        // somewhere, and both before a match by tag or category alone.
        assert_eq!(
            names(&search("pad")),
            ["User/Leads/Padlock", "Factory/Pads/Warm Pad", "User/Pads/Warm Pad", "Factory/Pads/Glass"]
        );
        // The author is searched too, last.
        assert_eq!(names(&search("bo")), ["Factory/Basses/Warm Bass"]);
        assert_eq!(search("nothing like it").total, 0);
        assert_eq!(search("   ").total, 7, "no words: everything, in the list's order");
        assert_eq!(names(&search(""))[0], "Factory/Pads/Warm Pad");
    }

    #[test]
    fn a_category_narrows_a_search_by_any_stretch_of_its_path() {
        // What plugins call their banks is long; a piece of it will do.
        let long = PresetList::ready(vec![
            preset("Air", "Big Synth Factory Bank/Pads", "", &[]),
            preset("Sub", "Big Synth Factory Bank/Basses", "", &[]),
            preset("Mine", "Big Synth User Bank/Pads", "", &[]),
        ]);
        let found = |category| {
            let query = PresetQuery { text: "", category: Some(category), offset: 0, limit: 10 };
            names(&long.search(&query))
        };
        assert_eq!(found("factory/pads"), ["Big Synth Factory Bank/Pads/Air"]);
        assert_eq!(found("Factory Bank"), ["Big Synth Factory Bank/Pads/Air", "Big Synth Factory Bank/Basses/Sub"]);
        assert_eq!(found("pad").len(), 2);
        assert_eq!(found("pads/factory"), [] as [&str; 0]);

        let list = list();
        let filed = |category, text| {
            let query = PresetQuery { text, category: Some(category), offset: 0, limit: 10 };
            names(&list.search(&query))
        };
        assert_eq!(filed("pads", ""), ["Factory/Pads/Warm Pad", "Factory/Pads/Glass", "User/Pads/Warm Pad"]);
        assert_eq!(filed("User", ""), ["User/Pads/Warm Pad", "User/Leads/Padlock"]);
        assert_eq!(filed("factory/pads", "glass"), ["Factory/Pads/Glass"]);
        assert_eq!(filed("Pads/Factory", ""), [] as [&str; 0], "the parts are wanted in their order");
        assert_eq!(filed("pad", "").len(), 3, "a piece of a part will do");
        assert_eq!(filed(" / ", "").len(), 7, "an empty category is no category");
    }

    #[test]
    fn pages_say_how_many_there_are_in_all() {
        let list = list();
        let page = |offset, limit| list.search(&PresetQuery { text: "", category: None, offset, limit });
        let first = page(0, 3);
        assert_eq!((first.total, first.offset, first.presets.len()), (7, 0, 3));
        let last = page(6, 3);
        assert_eq!((last.total, last.offset, names(&last)), (7, 6, vec!["Loose".to_string()]));
        let beyond = page(40, 3);
        assert_eq!((beyond.total, beyond.offset, beyond.presets.len()), (7, 7, 0));
        assert_eq!(page(0, 0).presets.len(), 0);
    }

    #[test]
    fn a_preset_is_found_by_id_name_or_the_end_of_its_path() {
        let list = list();
        let glass = &list.presets[1];
        assert_eq!(list.find(&glass.id), PresetLookup::Found(glass));
        assert_eq!(list.find("glass"), PresetLookup::Found(glass));
        assert_eq!(list.find("  Pads/GLASS "), PresetLookup::Found(glass));
        assert_eq!(list.find("factory/pads/glass"), PresetLookup::Found(glass));
        assert_eq!(list.find("Loose"), PresetLookup::Found(&list.presets[6]));

        // Two presets share a name: the category tells them apart.
        match list.find("warm pad") {
            PresetLookup::Ambiguous(found) => assert_eq!(found.len(), 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(list.find("User/Pads/Warm Pad"), PresetLookup::Found(&list.presets[4]));
        assert_eq!(list.find("factory/pads/warm pad"), PresetLookup::Found(&list.presets[0]));
        match list.find("Pads/Warm Pad") {
            PresetLookup::Ambiguous(found) => assert_eq!(found.len(), 2),
            other => panic!("{other:?}"),
        }
        // Only whole parts of the path count.
        assert!(matches!(list.find("ads/Glass"), PresetLookup::Missing { .. }));
    }

    #[test]
    fn a_miss_comes_back_with_what_is_near() {
        let list = list();
        let near = |wanted| match list.find(wanted) {
            PresetLookup::Missing { similar } => similar.iter().map(|preset| preset.path()).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };
        assert_eq!(near("warm"), ["Factory/Pads/Warm Pad", "Factory/Basses/Warm Bass", "User/Pads/Warm Pad"]);
        // No preset has both words; the ones with either are offered.
        assert_eq!(near("glass organ"), ["Factory/Pads/Glass"]);
        assert_eq!(near("zzz"), [] as [&str; 0]);
        assert!(matches!(PresetList::default().find("x"), PresetLookup::Missing { similar } if similar.is_empty()));
    }

    #[test]
    fn categories_are_counted() {
        assert_eq!(
            list().categories(),
            [
                (String::new(), 1),
                ("Factory/Basses".to_string(), 1),
                ("Factory/Pads".to_string(), 2),
                ("Factory/Templates".to_string(), 1),
                ("User/Leads".to_string(), 1),
                ("User/Pads".to_string(), 1),
            ]
        );
    }

    #[test]
    fn short_ids_are_stable_and_tell_locators_apart() {
        assert_eq!(short_id("a"), short_id("a"));
        assert_eq!(short_id("").len(), 10);
        let ids: std::collections::HashSet<String> = (0..20_000).map(|n| short_id(&format!("preset {n}"))).collect();
        assert_eq!(ids.len(), 20_000);
    }

    #[test]
    fn a_list_knows_how_far_along_it_is() {
        let reading = PresetList::reading(Vec::new());
        assert!(reading.is_reading() && reading.is_empty());
        let ready = list();
        assert!(!ready.is_reading());
        assert_eq!(ready.len(), 7);
        assert_eq!(
            ready.presets[0].reference(),
            PresetRef { id: ready.presets[0].id.clone(), name: "Warm Pad".into() }
        );
        assert_eq!(PresetList::default().status, PresetStatus::Ready);
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    /// A folder of the test's own in the temporary folder, gone with it.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let folder = std::env::temp_dir().join(format!("trill-core-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&folder).unwrap();
            Self(folder)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_whole_path_is_a_file_and_anything_else_is_a_name() {
        let folder = Folder::new("preset-path");
        let file = folder.path().join("Lead #1.vstpreset");
        std::fs::write(&file, b"x").unwrap();
        let text = file.to_string_lossy().into_owned();

        assert_eq!(PresetFile::asked(&text), Some(PresetFile { path: file.clone(), within: None }));
        assert_eq!(PresetFile::asked(&format!("  {text} ")).map(|asked| asked.path), Some(file.clone()));
        // Names, with a category in front or without, and ids.
        for name in ["Warm Pad", "Factory/Pads/Warm Pad", "Saved/My Glass", "3ba773e32c", "Lead.vstpreset", ""] {
            assert_eq!(PresetFile::asked(name), None, "{name}");
        }
        // A path that leads nowhere is still a path: the answer to it is
        // that there is no such file, not that there is no such preset.
        let gone = folder.path().join("Gone.vstpreset");
        assert_eq!(PresetFile::asked(&gone.to_string_lossy()).map(|asked| asked.path), Some(gone));
        assert_eq!(PresetFile::name_of(&file), "Lead #1");
    }

    #[test]
    fn a_preset_in_a_file_of_several_is_named_after_the_path() {
        let folder = Folder::new("preset-within");
        let kits = folder.path().join("#1 Kits").join("presets");
        std::fs::create_dir_all(kits.parent().unwrap()).unwrap();
        std::fs::write(&kits, b"x").unwrap();
        let text = kits.to_string_lossy().into_owned();

        let asked = PresetFile::asked(&format!("{text}#Metal/Kit #2")).unwrap();
        assert_eq!(asked, PresetFile { path: kits.clone(), within: Some("Metal/Kit #2".into()) });
        // A `#` with nothing after it, and one in the path of a file that
        // is not there, name nothing.
        assert_eq!(PresetFile::asked(&format!("{text}#")).unwrap().within, None);
        let gone = format!("{}#Kit", folder.path().join("gone").display());
        assert_eq!(PresetFile::asked(&gone), Some(PresetFile { path: PathBuf::from(&gone), within: None }));
    }
}

#[cfg(test)]
mod sound_tests {
    use super::*;
    use crate::project::{AutomationTarget, ChainRef};

    const SYNTH: &str = "builtin:polysynth";
    const CHORUS: &str = "builtin:chorus";
    const REVERB: &str = "builtin:reverb";
    const DELAY: &str = "builtin:delay";
    const EQ: &str = "builtin:eq";

    fn sound(name: &str) -> PresetRef {
        PresetRef { id: name.to_lowercase(), name: name.to_string() }
    }

    /// The chain of the first track, as the types of its devices.
    fn chain(project: &Project) -> Vec<&str> {
        project.tracks[0].devices.iter().map(|device| device.uri.as_str()).collect()
    }

    #[test]
    fn a_sound_is_a_preset_that_says_what_follows_its_device() {
        let amp = SoundDevice {
            uri: "vst3:AMP".into(),
            device: "Amp".into(),
            enabled: false,
            params: [("7".to_string(), 0.5)].into(),
            state: Some(vec![1, 2, 3]),
        };
        let tone = SavedPreset {
            uri: "vst3:GUITAR".into(),
            device: "Guitar".into(),
            name: "Tone".into(),
            state: Some(vec![9]),
            after: Some(vec![amp.clone(), SoundDevice::new(REVERB, "Reverb", &[("decay", 1.5)])]),
            ..SavedPreset::default()
        };
        assert!(tone.is_sound());
        let read = SavedPreset::from_json(&tone.to_json()).unwrap();
        let after = read.after.as_deref().unwrap();
        // The plugin as it was, its state as text and still bypassed; the
        // reverb switched on, which is not written down.
        assert_eq!(after[0], amp);
        assert!(tone.to_json().contains(r#""enabled": false"#) && tone.to_json().matches("enabled").count() == 1);
        assert_eq!((after[1].uri.as_str(), after[1].enabled, after[1].params.get("decay")), (REVERB, true, Some(&1.5)));
        assert_eq!(names_after(after), ["Amp", "Reverb"]);
        assert_eq!(SoundDevice { device: " ".into(), ..amp.clone() }.called(), "vst3:AMP");

        // A sound that is dry says so, which a preset of one device does
        // not: the two are told apart when they are read back.
        let dry = SavedPreset { after: Some(Vec::new()), ..tone.clone() };
        assert_eq!(SavedPreset::from_json(&dry.to_json()).unwrap().after, Some(Vec::new()));
        let alone = SavedPreset { after: None, ..tone.clone() };
        assert!(!alone.to_json().contains("after") && !SavedPreset::from_json(&alone.to_json()).unwrap().is_sound());

        // Every device of a sound is read by the format the sound was
        // saved in, as a preset of one device is: a reverb from before
        // format 7 is the reverb of its day.
        let old = r#"{"uri":"builtin:polysynth","name":"Old","after":[
            {"uri":"builtin:reverb","params":{"decay":3.0,"legacy":0.0}}]}"#;
        let read = SavedPreset::from_json(old).unwrap();
        assert_eq!(read.after.unwrap()[0].params.get("legacy"), Some(&2.0));
        let new = old.replace(r#""name":"Old""#, r#""name":"New","format":7"#);
        assert_eq!(SavedPreset::from_json(&new).unwrap().after.unwrap()[0].params.get("legacy"), Some(&0.0));
        // A device that does not say which it is makes no sound.
        let nameless = r#"{"uri":"builtin:polysynth","name":"X","after":[{"uri":""}]}"#;
        assert!(SavedPreset::from_json(nameless).unwrap_err().contains("place 1"));
    }

    #[test]
    fn a_sounds_effects_go_behind_its_device_and_replace_what_a_sound_brought() {
        let mut project = Project::new("Song");
        let track = project.add_track("Keys");
        let on = ChainRef::Track(track);
        let gate = project.add_device(on, "builtin:autofilter", "Auto Filter", None).unwrap();
        let synth = project.add_device(on, SYNTH, "Poly Synth", None).unwrap();
        let eq = project.add_device(on, EQ, "EQ", None).unwrap();

        let pad = sound("Pad");
        let after = [
            SoundDevice { enabled: false, ..SoundDevice::new(CHORUS, "Chorus", &[("mix", 0.4)]) },
            SoundDevice { state: Some(vec![5]), ..SoundDevice::new(REVERB, "Reverb", &[("decay", 4.0)]) },
        ];
        let brought = project.bring_after(synth, &pad, &after).unwrap();
        assert_eq!(chain(&project), ["builtin:autofilter", SYNTH, CHORUS, REVERB, EQ], "behind it, in their order");
        assert!(brought.removed.is_empty());
        let [chorus, reverb] = brought.made[..] else { panic!("{brought:?}") };
        let made = project.device(chorus).unwrap();
        assert_eq!((made.name.as_str(), made.enabled, made.params.get("mix")), ("Chorus", false, Some(&0.4)));
        assert_eq!(made.preset.as_ref().unwrap().bringer(), Some("pad"));
        assert_eq!(made.preset.as_ref().unwrap().name, "Pad", "what it started from, by name");
        let made = project.device(reverb).unwrap();
        assert_eq!((made.state.as_deref(), made.sidechain), (Some(&[5u8][..]), None));
        // The devices that were there are nobody's sound.
        assert!(project.device(eq).unwrap().preset.is_none() && project.device(gate).unwrap().preset.is_none());

        // The reverb is given a preset of its own, and its mix an envelope
        // as the chorus's has one.
        project.device_mut(reverb).unwrap().preset = Some(PresetRef { id: "hall".into(), name: "hall".into() });
        for device in [chorus, reverb] {
            let target = AutomationTarget::DeviceParam { device, param: "mix".into() };
            project.set_automation(target, vec![crate::project::AutomationPoint::new(0.0, 0.2)]).unwrap();
        }
        assert_eq!(project.automation.len(), 2);

        // Another sound: what the first one brought goes, with its
        // envelope; the reverb is the user's by now and stays, behind the
        // new effect, as the EQ does.
        let pluck = sound("Pluck");
        let brought = project.bring_after(synth, &pluck, &[SoundDevice::new(DELAY, "", &[])]).unwrap();
        assert_eq!(chain(&project), ["builtin:autofilter", SYNTH, DELAY, REVERB, EQ]);
        assert_eq!(brought.removed.iter().map(|device| device.id).collect::<Vec<_>>(), [chorus]);
        assert_eq!(project.automation.len(), 1, "the envelope of the chorus went with it");
        let delay = project.device(brought.made[0]).unwrap();
        assert_eq!((delay.name.as_str(), delay.preset.as_ref().unwrap().bringer()), (DELAY, Some("pluck")));

        // A sound that is dry takes away what a sound brought and adds
        // nothing; given twice, it changes nothing the second time.
        let dry = project.bring_after(synth, &sound("Bass"), &[]).unwrap();
        assert_eq!((dry.made.len(), dry.removed.len()), (0, 1));
        assert_eq!(chain(&project), ["builtin:autofilter", SYNTH, REVERB, EQ]);
        let before = project.clone();
        assert_eq!(project.bring_after(synth, &sound("Bass"), &[]).unwrap(), Brought::default());
        assert_eq!(project, before);

        // A device that is not there is an error, and nothing is changed.
        assert!(project.bring_after(DeviceId(9_999), &pad, &after).is_err());
        assert_eq!(project, before);
    }

    #[test]
    fn a_reference_says_whether_a_sound_brought_its_device() {
        let pad = sound("Pad");
        let brought = PresetRef::brought_by(&pad);
        assert_eq!((brought.bringer(), brought.name.as_str()), (Some("pad"), "Pad"));
        assert_eq!(pad.bringer(), None);
        // No preset a device type lists has such an id: a saved one's is
        // ten hexadecimal digits, a built-in one's its name.
        assert_ne!(brought.id, SavedPreset { name: "Pad".into(), ..SavedPreset::default() }.info().id);
    }
}
