//! What a track's notes mean to the instrument that plays them.
//!
//! A note is a key, and what a key does is the instrument's business. On a
//! drum kit it is a drum, and which drum differs from kit to kit. On a
//! sampled guitar the keys under the playing range make no sound at all:
//! they choose how the notes after them are played (palm-muted, as a
//! harmonic, hammered on). Some instruments sound an octave under the key,
//! and each bends by its own amount when the pitch wheel moves.
//!
//! An [`InstrumentProfile`] holds those facts for one track, in the song.
//! With it a drum name lands on the note this kit uses, an articulation is
//! asked for by name instead of by a silent low note, and whoever reads the
//! music back knows which notes are music.

use crate::device::PITCH_BEND_SEMITONES;
use crate::pitch::{drum_name, drum_stand_in, name_key, parse_drum, pitch_name};
use crate::project::EditError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The widest pitch-bend range a profile may claim, in semitones each way.
pub const MAX_BEND_SEMITONES: f64 = 96.0;
/// The most octaves an instrument may sound from the key that is played.
pub const MAX_OCTAVES: i8 = 4;
/// How many characters of free text a profile keeps.
pub const MAX_NOTES_TEXT: usize = 2400;

/// Whether a track's notes are pitches or drums.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Each key is a pitch: the notes have a key and make chords.
    Pitched,
    /// Each key is a drum: the notes are named by the track's drum map and
    /// take no part in the song's harmony.
    Drums,
}

/// How long the choice a keyswitch makes lasts.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lasts {
    /// Until another keyswitch is pressed: palm mute, harmonics.
    #[default]
    Stays,
    /// For the next note only, after which the instrument is back on what
    /// was chosen before: a slide into a note.
    Once,
    /// It joins notes: pressed before a note, every note that starts while
    /// the one before it still sounds is played without a new attack (a
    /// hammer-on, a pull-off, a slide along the string), until a note
    /// starts on its own again.
    Joins,
}

impl Lasts {
    fn is_stays(&self) -> bool {
        *self == Lasts::Stays
    }
}

/// One way of playing a note that the instrument is asked for by a key, by
/// how hard the note is hit, or by both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Articulation {
    /// What it is asked for by: lower case, words joined by underscores.
    pub name: String,
    /// The key that chooses it and sounds nothing itself, if a key does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<u8>,
    #[serde(default, skip_serializing_if = "Lasts::is_stays")]
    pub lasts: Lasts,
    /// The lowest and highest velocity that play it, if how hard a note is
    /// hit decides: on some guitars a soft note is a muted one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<(u8, u8)>,
    /// Whether this is how a note is played when nothing is said. A part
    /// begins by choosing it, since an earlier part may have left the
    /// instrument on another.
    #[serde(default, skip_serializing_if = "is_false")]
    pub default: bool,
    /// How many semitones above its key a note played this way is heard:
    /// 12 for a harmonic that rings an octave over the fretted note.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sounds: i8,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &i8) -> bool {
    *value == 0
}

impl Articulation {
    /// An articulation chosen by `key` that stays chosen.
    pub fn key(name: &str, key: u8) -> Self {
        Self { name: name_key(name), key: Some(key), lasts: Lasts::Stays, velocity: None, default: false, sounds: 0 }
    }

    /// The same, heard `semitones` above the key that is played.
    pub fn sounding(mut self, semitones: i8) -> Self {
        self.sounds = semitones;
        self
    }

    /// The same, lasting as `lasts` says.
    pub fn lasting(mut self, lasts: Lasts) -> Self {
        self.lasts = lasts;
        self
    }

    /// The same, played by velocities from `low` to `high`.
    pub fn hit(mut self, low: u8, high: u8) -> Self {
        self.velocity = Some((low, high));
        self
    }

    /// The same, as the way a note is played when nothing is said.
    pub fn as_default(mut self) -> Self {
        self.default = true;
        self
    }
}

/// What is known about the instrument a track plays through. Everything is
/// optional; an empty profile is an ordinary pitched instrument that plays
/// every key at its own pitch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InstrumentProfile {
    /// Drums or pitched. `None` leaves it to the instrument: a drum kit or
    /// a plugin that calls itself a drum instrument makes a drum track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Names for keys. On a drum track these are the drums, asked before
    /// the General MIDI names, so that "china" is the note this kit plays
    /// a china on. On a pitched track they are the keys that play a noise
    /// instead of a note (a pick scrape, a slap).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sounds: BTreeMap<String, u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub articulations: Vec<Articulation>,
    /// The lowest and highest key that plays a note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<(u8, u8)>,
    /// How many octaves from the key the instrument sounds: -1 for one
    /// that sounds an octave under what is written.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sounds_octaves: i8,
    /// How far the pitch wheel bends, in semitones each way, if it is not
    /// [`PITCH_BEND_SEMITONES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bend_range: Option<f64>,
    /// Whether a note that starts while the one before it still sounds is
    /// slurred into, with no new attack, as wind instruments and monophonic
    /// synths with legato do.
    #[serde(default, skip_serializing_if = "is_false")]
    pub slurs: bool,
    /// Anything else worth knowing when writing for it, in words.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Whether [`sounds`](Self::sounds) is the whole of what a drum track's
    /// instrument plays: its kit said what is on every key, so a drum it
    /// does not name is a drum this kit has not got, and General MIDI is
    /// not asked. Never in a song file: it is true of a profile that was
    /// made from the kit that is loaded ([`KeySound`]), while it is.
    #[serde(skip)]
    pub whole: bool,
}

/// What one key of an instrument plays, as the instrument's own state says
/// it: a drum sampler's kit says which drum hangs in each place, and which
/// keys strike it how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySound {
    pub key: u8,
    /// The place in the kit: "Snare", "Racktom 2", "Cymbal 5".
    pub place: String,
    /// What is in that place, as its maker describes it: "20\" Sehhar China".
    /// Empty where the kit only has the maker's own code for it.
    pub instrument: String,
    /// The way of striking it that the key plays: "Center", "Rimshot",
    /// "Closed Edge", "Mute Hit".
    pub stroke: String,
}

/// What a drum name comes to on a track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drum {
    /// It is played by this key.
    Key(DrumKey),
    /// It is a drum, and this track's kit has none: its map is the whole of
    /// what it plays ([`InstrumentProfile::whole`]) and does not name it.
    Lacking(String),
    /// It is not the name of a drum.
    Unknown,
}

/// What is known about one kind of instrument, as it is kept in a file of
/// its own: the device type, what it is called, and its profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KnownInstrument {
    /// The device type: `vst3:<class id>`, `clap:<plugin id>`.
    pub uri: String,
    /// What the device is called, for listing.
    #[serde(default)]
    pub device: String,
    pub profile: InstrumentProfile,
}

impl KnownInstrument {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("an instrument profile is always serializable")
    }

    /// Reads what [`to_json`](Self::to_json) wrote, with the profile
    /// checked as [`InstrumentProfile::validated`] checks it.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let known: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        let profile = known.profile.validated().map_err(|error| error.to_string())?;
        Ok(Self { profile, ..known })
    }
}

/// A drum name resolved to the key that plays it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrumKey {
    pub pitch: u8,
    /// The name the drum is listed under.
    pub name: String,
    /// The drum that was used in its place, when neither the track's map
    /// nor General MIDI has the one asked for.
    pub stand_in: Option<&'static str>,
}

impl InstrumentProfile {
    /// Whether the profile says nothing, which is what a song file leaves
    /// out.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The profile with its names in the form they are compared in, or
    /// what is wrong with it.
    pub fn validated(mut self) -> Result<Self, EditError> {
        let invalid = |message: String| Err(EditError::Invalid(message));
        let key = |pitch: u8, what: &str| match pitch {
            0..=127 => Ok(()),
            _ => Err(EditError::Invalid(format!("{what} is on key {pitch}, which is outside 0..=127"))),
        };
        let mut sounds = BTreeMap::new();
        for (name, pitch) in std::mem::take(&mut self.sounds) {
            let name = checked_name(&name)?;
            key(pitch, &format!("'{name}'"))?;
            sounds.insert(name, pitch);
        }
        self.sounds = sounds;

        let mut seen: Vec<String> = Vec::new();
        for articulation in &mut self.articulations {
            articulation.name = checked_name(&articulation.name)?;
            let name = &articulation.name;
            if seen.contains(name) {
                return invalid(format!("the articulation '{name}' is given twice"));
            }
            seen.push(name.clone());
            if let Some(pitch) = articulation.key {
                key(pitch, &format!("the keyswitch of '{name}'"))?;
            }
            match articulation.velocity {
                Some((low, high)) if low == 0 || high > 127 || low > high => {
                    return invalid(format!(
                        "the velocities {low}..={high} of '{name}' must lie within 1..=127, lowest first"
                    ));
                }
                None if articulation.key.is_none() => {
                    return invalid(format!(
                        "the articulation '{name}' has neither a keyswitch nor a velocity range, so nothing asks for it"
                    ));
                }
                _ => {}
            }
            if articulation.lasts != Lasts::Stays && articulation.key.is_none() {
                return invalid(format!("'{name}' lasts for one note or joins notes, which only a keyswitch can"));
            }
            if articulation.default && articulation.lasts != Lasts::Stays {
                return invalid(format!("'{name}' cannot be the default: it does not stay chosen"));
            }
            if !(-48..=48).contains(&articulation.sounds) {
                return invalid(format!(
                    "'{name}' sounding {} semitones from its key is outside -48..=48",
                    articulation.sounds
                ));
            }
        }
        if self.articulations.iter().filter(|articulation| articulation.default).count() > 1 {
            return invalid("only one articulation can be the default".to_string());
        }
        if let Some((low, high)) = self.range {
            key(low, "the bottom of the range")?;
            key(high, "the top of the range")?;
            if low > high {
                return invalid(format!("the range {}..{} is upside down", pitch_name(low), pitch_name(high)));
            }
            if let Some(inside) =
                self.articulations.iter().find(|a| a.key.is_some_and(|pitch| (low..=high).contains(&pitch)))
            {
                return invalid(format!(
                    "the keyswitch of '{}' ({}) is inside the range {}..{}; a key either switches or plays",
                    inside.name,
                    pitch_name(inside.key.unwrap_or_default()),
                    pitch_name(low),
                    pitch_name(high)
                ));
            }
        }
        if !(-MAX_OCTAVES..=MAX_OCTAVES).contains(&self.sounds_octaves) {
            return invalid(format!(
                "an instrument sounding {} octaves from its keys is outside -{MAX_OCTAVES}..={MAX_OCTAVES}",
                self.sounds_octaves
            ));
        }
        if let Some(range) = self.bend_range
            && !(range.is_finite() && range > 0.0 && range <= MAX_BEND_SEMITONES)
        {
            return invalid(format!("a bend range of {range} semitones is outside 0..={MAX_BEND_SEMITONES}"));
        }
        if self.notes.chars().count() > MAX_NOTES_TEXT {
            return invalid(format!("the notes are longer than {MAX_NOTES_TEXT} characters"));
        }
        self.notes = self.notes.trim().to_string();
        Ok(self)
    }

    /// How far the pitch wheel bends, in semitones each way.
    pub fn bend_semitones(&self) -> f64 {
        self.bend_range.unwrap_or(PITCH_BEND_SEMITONES as f64)
    }

    /// The articulation asked for by `name`.
    pub fn articulation(&self, name: &str) -> Option<&Articulation> {
        let wanted = name_key(name);
        self.articulations.iter().find(|articulation| articulation.name == wanted)
    }

    /// The first articulation that goes by one of `names`, and joins notes
    /// or not as `lasts` says: how an ornament finds the instrument's own
    /// way of playing it.
    pub fn articulation_like(&self, names: &[&str], lasts: Lasts) -> Option<&Articulation> {
        names.iter().find_map(|name| self.articulation(name)).filter(|articulation| articulation.lasts == lasts)
    }

    /// How a note is played when nothing is said, if the profile says.
    pub fn default_articulation(&self) -> Option<&Articulation> {
        self.articulations.iter().find(|articulation| articulation.default)
    }

    /// The names of the articulations, in the order they were given.
    pub fn articulation_names(&self) -> Vec<&str> {
        self.articulations.iter().map(|articulation| articulation.name.as_str()).collect()
    }

    /// Whether `pitch` is a key that switches how notes are played and
    /// sounds nothing itself.
    pub fn is_keyswitch(&self, pitch: u8) -> bool {
        self.articulations.iter().any(|articulation| articulation.key == Some(pitch))
    }

    /// The articulation whose keyswitch `pitch` is: the one listed first
    /// when several share a key.
    pub fn keyswitch_name(&self, pitch: u8) -> Option<&str> {
        self.articulations.iter().find(|a| a.key == Some(pitch)).map(|articulation| articulation.name.as_str())
    }

    /// The name a key has in [`sounds`](Self::sounds), if it has one: the
    /// shortest where it has several ("china" before "cymbal_5"), which is
    /// the one it is usually asked for by.
    pub fn sound_name(&self, pitch: u8) -> Option<&str> {
        let named = self.sounds.iter().filter(|(_, key)| **key == pitch).map(|(name, _)| name.as_str());
        named.min_by_key(|name| name.len())
    }

    /// Whether `pitch` plays a pitched note on this instrument: not a
    /// keyswitch, not a noise, and inside the range if there is one.
    pub fn is_music(&self, pitch: u8) -> bool {
        !self.is_keyswitch(pitch) && self.sound_name(pitch).is_none() && self.in_range(pitch)
    }

    /// Whether `pitch` is inside the playing range; true where none is
    /// known.
    pub fn in_range(&self, pitch: u8) -> bool {
        self.range.is_none_or(|(low, high)| (low..=high).contains(&pitch))
    }

    /// The pitch that is heard when `pitch` is played.
    pub fn sounding(&self, pitch: u8) -> u8 {
        (pitch as i32 + 12 * self.sounds_octaves as i32).clamp(0, 127) as u8
    }

    /// The key to play for `pitch` to be heard, if there is such a key.
    pub fn written(&self, pitch: u8) -> Option<u8> {
        let key = pitch as i32 - 12 * self.sounds_octaves as i32;
        (0..=127).contains(&key).then_some(key as u8)
    }

    /// The key that plays the drum `name`: by the track's own map, then
    /// by General MIDI, then by the General MIDI drum that stands in for
    /// one the map has no note for. `None` for a name that is no drum, and
    /// for a drum this track's kit has not got ([`find_drum`](Self::find_drum)
    /// tells the two apart).
    pub fn drum(&self, name: &str) -> Option<DrumKey> {
        match self.find_drum(name) {
            Drum::Key(key) => Some(key),
            Drum::Lacking(_) | Drum::Unknown => None,
        }
    }

    /// What the drum `name` comes to on this track. Where the track's map
    /// is the whole of what its kit plays, General MIDI is not asked: a
    /// drum the map does not name is one the kit has not got, and playing
    /// General MIDI's key for it would hit whatever the kit keeps there.
    pub fn find_drum(&self, name: &str) -> Drum {
        let wanted = name_key(name);
        if let Some(pitch) = self.sounds.get(&wanted) {
            return Drum::Key(DrumKey { pitch: *pitch, name: wanted, stand_in: None });
        }
        if let Some(general) = parse_drum(&wanted) {
            // Under the name the map has for that drum, so that two names
            // of one drum are one lane.
            let canonical = drum_name(general).unwrap_or("drum");
            return match (self.sounds.get(canonical).copied(), self.whole) {
                (Some(pitch), _) => Drum::Key(DrumKey { pitch, name: self.drum_name(pitch), stand_in: None }),
                (None, false) => Drum::Key(DrumKey { pitch: general, name: self.drum_name(general), stand_in: None }),
                (None, true) => Drum::Lacking(canonical.to_string()),
            };
        }
        let Some((stand_in, general)) = drum_stand_in(&wanted) else { return Drum::Unknown };
        match (self.sounds.get(stand_in).copied(), self.whole) {
            (Some(pitch), _) => Drum::Key(DrumKey { pitch, name: self.drum_name(pitch), stand_in: Some(stand_in) }),
            (None, false) => {
                Drum::Key(DrumKey { pitch: general, name: self.drum_name(general), stand_in: Some(stand_in) })
            }
            (None, true) => Drum::Lacking(wanted),
        }
    }

    /// What the drum on `pitch` is called: by the track's map, then by
    /// General MIDI (unless the map is the whole kit), else the key's name.
    pub fn drum_name(&self, pitch: u8) -> String {
        // A key the map gives to another drum is not the General MIDI one.
        let general = drum_name(pitch).filter(|name| !self.whole && !self.sounds.contains_key(*name));
        match (self.sound_name(pitch), general) {
            (Some(name), _) => name.to_string(),
            (None, Some(name)) => name.to_string(),
            (None, None) => pitch_name(pitch),
        }
    }

    /// The drums of the track by name, with their keys: the track's own
    /// first, then those of General MIDI that it does not replace (none,
    /// where its own are the whole kit).
    pub fn drum_map(&self) -> Vec<(String, u8)> {
        let mut map: Vec<(String, u8)> = self.sounds.iter().map(|(name, pitch)| (name.clone(), *pitch)).collect();
        if self.whole {
            return map;
        }
        let mut taken: Vec<u8> = map.iter().map(|(_, pitch)| *pitch).collect();
        for (name, pitch) in crate::pitch::DRUM_NAMES {
            // Under its first name only, and not a drum the track has moved.
            let first = drum_name(*pitch) == Some(*name);
            if first && !taken.contains(pitch) && !self.sounds.contains_key(*name) {
                taken.push(*pitch);
                map.push(((*name).to_string(), *pitch));
            }
        }
        map
    }

    /// The key a name stands for on this track: a drum on a drum track; on
    /// a pitched one a noise or a keyswitch.
    pub fn named_key(&self, name: &str, drums: bool) -> Option<u8> {
        if drums {
            return self.drum(name).map(|drum| drum.pitch);
        }
        let wanted = name_key(name);
        self.sounds.get(&wanted).copied().or_else(|| self.articulation(&wanted).and_then(|a| a.key))
    }
}

/// A name in the form names are compared in, if it can be one.
fn checked_name(name: &str) -> Result<String, EditError> {
    let key = name_key(name);
    let fine = !key.is_empty()
        && key.len() <= 40
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && key.chars().any(|c| c.is_ascii_alphabetic());
    if fine {
        Ok(key)
    } else {
        Err(EditError::Invalid(format!(
            "'{name}' cannot be a name: use letters, digits and underscores, with at least one letter, at most 40"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guitar as the sampled ones are laid out: switches under the
    /// range, noises over it, and soft notes muted.
    fn guitar() -> InstrumentProfile {
        InstrumentProfile {
            sounds: [("scratch".to_string(), 89), ("Pick Scrape".to_string(), 101)].into(),
            articulations: vec![
                Articulation::key("open", 24).hit(64, 126).as_default(),
                Articulation::key("muted", 24).hit(32, 63),
                Articulation::key("Palm Mute", 26),
                Articulation::key("slide", 27).lasting(Lasts::Once),
                Articulation::key("hammer", 29).lasting(Lasts::Joins),
            ],
            range: Some((36, 84)),
            ..InstrumentProfile::default()
        }
        .validated()
        .unwrap()
    }

    #[test]
    fn a_profile_tells_music_from_switches_and_noises() {
        let guitar = guitar();
        assert!(guitar.is_keyswitch(26) && guitar.is_keyswitch(24) && !guitar.is_keyswitch(40));
        assert_eq!(guitar.keyswitch_name(24), Some("open"), "two articulations share the key; the first names it");
        assert_eq!(guitar.sound_name(101), Some("pick_scrape"), "names are kept in the form they are compared in");
        assert!(guitar.is_music(40) && guitar.is_music(84));
        assert!(!guitar.is_music(26) && !guitar.is_music(89) && !guitar.is_music(30) && !guitar.is_music(90));
        assert_eq!(guitar.articulation("palm-mute").unwrap().key, Some(26));
        assert_eq!(guitar.articulation_names(), ["open", "muted", "palm_mute", "slide", "hammer"]);
        assert_eq!(guitar.default_articulation().unwrap().name, "open");
        assert_eq!(guitar.articulation_like(&["hammer_on", "hammer"], Lasts::Joins).unwrap().key, Some(29));
        assert!(guitar.articulation_like(&["slide"], Lasts::Joins).is_none(), "this slide does not join notes");
        assert_eq!(guitar.named_key("Palm Mute", false), Some(26));
        assert_eq!(guitar.named_key("scratch", false), Some(89));
        assert_eq!(guitar.named_key("kick", false), None, "a pitched track has no drums");
        // An empty profile is every key at its own pitch.
        let plain = InstrumentProfile::default();
        assert!(plain.is_empty() && plain.is_music(0) && plain.is_music(127) && !guitar.is_empty());
        assert_eq!((plain.bend_semitones(), plain.sounding(40), plain.written(40)), (2.0, 40, Some(40)));
    }

    #[test]
    fn an_instrument_can_sound_an_octave_from_its_keys() {
        let bass = InstrumentProfile { sounds_octaves: -1, bend_range: Some(12.0), ..Default::default() };
        assert_eq!((bass.sounding(45), bass.written(33)), (33, Some(45)));
        assert_eq!(bass.written(120), None, "there is no key an octave above the top of the keyboard");
        assert_eq!(bass.sounding(5), 0);
        assert_eq!(bass.bend_semitones(), 12.0);
    }

    #[test]
    fn a_drum_name_lands_on_the_key_this_kit_uses() {
        let kit = InstrumentProfile {
            role: Some(Role::Drums),
            sounds: [("china", 52), ("crash", 86), ("wide_open_hat", 26), ("high_tom", 48), ("cymbal_4", 57)]
                .map(|(name, pitch)| (name.to_string(), pitch))
                .into(),
            ..Default::default()
        };
        let key = |name: &str| kit.drum(name).map(|drum| (drum.pitch, drum.name, drum.stand_in));
        assert_eq!(key("crash"), Some((86, "crash".to_string(), None)), "the kit's own place for it");
        assert_eq!(key("Kick"), Some((36, "kick".to_string(), None)), "General MIDI where the kit says nothing");
        assert_eq!(key("bd"), Some((36, "kick".to_string(), None)), "another name of the same drum is the same lane");
        assert_eq!(key("tom_high"), Some((48, "high_tom".to_string(), None)), "an alias follows the kit's map too");
        assert_eq!(key("wide_open_hat"), Some((26, "wide_open_hat".to_string(), None)));
        assert_eq!(key("half_open_hat"), Some((46, "open_hat".to_string(), Some("open_hat"))), "the nearest it has");
        assert_eq!(key("gong"), None);
        // Reading back: the kit's names first, and a key it moved a drum off is no longer that drum.
        assert_eq!(kit.drum_name(86), "crash");
        assert_eq!(kit.drum_name(57), "cymbal_4", "not General MIDI's second crash");
        assert_eq!(kit.drum_name(49), "C#3", "the crash is elsewhere on this kit");
        assert_eq!(kit.drum_name(38), "snare");
        assert_eq!(kit.drum_name(100), "E7");
        let map = kit.drum_map();
        assert_eq!(map[0], ("china".to_string(), 52));
        assert!(map.contains(&("kick".to_string(), 36)) && map.contains(&("crash".to_string(), 86)));
        assert!(!map.iter().any(|(name, pitch)| name == "crash" && *pitch == 49));
        assert_eq!(InstrumentProfile::default().drum("china").unwrap().pitch, 52);
    }

    #[test]
    fn a_kit_that_said_what_is_on_every_key_has_no_general_midi_behind_it() {
        // A kit with its china where General MIDI has none, two toms, and no cowbell.
        let kit = InstrumentProfile {
            role: Some(Role::Drums),
            sounds: [("kick", 36), ("snare", 38), ("china", 86), ("cymbal_1", 86), ("high_tom", 48), ("open_hat", 25)]
                .map(|(name, pitch)| (name.to_string(), pitch))
                .into(),
            whole: true,
            ..Default::default()
        };
        let key = |name: &str| kit.find_drum(name);
        assert!(matches!(key("china"), Drum::Key(DrumKey { pitch: 86, .. })));
        assert!(matches!(key("bd"), Drum::Key(DrumKey { pitch: 36, .. })), "another name of a drum it has");
        // General MIDI's key for a drum the kit has not got is not played: something else is there.
        assert_eq!(key("cowbell"), Drum::Lacking("cowbell".to_string()));
        assert_eq!(key("tom_mid"), Drum::Lacking("mid_tom".to_string()), "by the name the drum is listed under");
        assert_eq!(key("closed_hat_edge"), Drum::Lacking("closed_hat_edge".to_string()), "nor a stand-in's");
        assert!(matches!(key("half_open_hat"), Drum::Key(DrumKey { pitch: 25, stand_in: Some("open_hat"), .. })));
        assert_eq!(key("gong"), Drum::Unknown);
        assert_eq!(kit.drum("cowbell"), None);
        // Read back by the kit's names alone: the shortest where a key has two.
        assert_eq!(
            (kit.drum_name(86), kit.drum_name(52), kit.drum_name(56)),
            ("china".into(), "E3".into(), "G#3".into())
        );
        assert_eq!(kit.drum_map().len(), 6, "no drums but its own");
        // The same map without the kit's word for it is filled out by General MIDI, as before.
        let partial = InstrumentProfile { whole: false, ..kit.clone() };
        assert!(matches!(partial.find_drum("cowbell"), Drum::Key(DrumKey { pitch: 56, .. })));
        assert!(serde_json::to_string(&kit).unwrap() == serde_json::to_string(&partial).unwrap(), "never in a file");
    }

    #[test]
    fn a_profile_that_contradicts_itself_is_refused_with_the_reason() {
        let with = |change: fn(&mut InstrumentProfile)| {
            let mut profile = guitar();
            change(&mut profile);
            profile.validated().unwrap_err().to_string()
        };
        assert!(with(|p| p.range = Some((60, 40))).contains("upside down"));
        assert!(with(|p| p.range = Some((20, 84))).contains("is inside the range"));
        assert!(with(|p| p.articulations.push(Articulation::key("open", 30))).contains("'open' is given twice"));
        assert!(with(|p| p.articulations[1].velocity = Some((70, 60))).contains("lowest first"));
        assert!(with(|p| p.articulations[2].default = true).contains("only one articulation can be the default"));
        assert!(with(|p| p.articulations[3].default = true).contains("does not stay chosen"));
        assert!(with(|p| p.articulations[2].key = None).contains("neither a keyswitch nor a velocity range"));
        assert!(with(|p| p.articulations[1].key = Some(200)).contains("outside 0..=127"));
        assert!(with(|p| p.sounds_octaves = 9).contains("octaves"));
        assert!(with(|p| p.articulations[2].sounds = 60).contains("semitones from its key"));
        assert!(with(|p| p.bend_range = Some(0.0)).contains("bend range"));
        assert!(with(|p| p.sounds = [("!!".to_string(), 3)].into()).contains("cannot be a name"));
        assert!(with(|p| p.sounds = [("42".to_string(), 3)].into()).contains("at least one letter"));
        assert!(with(|p| p.notes = "x".repeat(MAX_NOTES_TEXT + 1)).contains("longer than"));
    }

    #[test]
    fn a_song_file_says_nothing_of_an_ordinary_instrument() {
        let json = serde_json::to_string(&InstrumentProfile::default()).unwrap();
        assert_eq!(json, "{}");
        let guitar = guitar();
        let text = serde_json::to_string(&guitar).unwrap();
        assert!(text.contains("\"lasts\":\"joins\"") && !text.contains("\"lasts\":\"stays\""), "{text}");
        assert!(!text.contains("role") && !text.contains("bend_range") && !text.contains("slurs"), "{text}");
        assert_eq!(serde_json::from_str::<InstrumentProfile>(&text).unwrap(), guitar);
        assert_eq!(serde_json::from_str::<InstrumentProfile>("{}").unwrap(), InstrumentProfile::default());
    }
}
