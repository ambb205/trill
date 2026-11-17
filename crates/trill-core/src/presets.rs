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

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
        crate::formats::keep_complete_format_1(&preset.uri, &mut preset.params);
        Ok(preset)
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
