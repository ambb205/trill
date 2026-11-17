//! The sounds the wavetable synth ships with: its bank.
//!
//! A patch is a set of parameter values and the names of its macros. Its
//! description says what it sounds like and what it is for, which is what
//! a model that cannot hear chooses it by; the macros are the handles
//! worth reaching for first; its tags are the words it is searched by.
//!
//! The synth has no effects inside. A patch that wants an echo or a room
//! says so ([`Patch::effects`]) and is a *sound*: given to a track, the
//! effects are made behind the synth as devices of their own, to be seen,
//! changed and taken away like any other.
//!
//! The first sixteen were heard by the owner and passed as they were. The
//! rest were written by someone who cannot hear, from how such sounds are
//! made, and are held to what can be measured instead: each kind at one
//! level through its effects, with room for a chord; every key of a
//! patch's range sounding; every macro moving the sound from end to end
//! without a gap, a click or a jump; a bass in the middle under 120 Hz;
//! and each within what the sounds of its kind in a synth known to be
//! good measure (`trill-engine/tests/bank.rs` has the numbers). A file for
//! each kind.

mod bass;
mod bell;
mod ensemble;
mod fx;
mod init;
mod keys;
mod lead;
mod pad;
mod pluck;
mod texture;

use super::State;
use super::params::PARAMS;
use crate::params::Kind;
use trill_core::device::Preset;
use trill_core::presets::SoundDevice;

/// A value a patch sets: a number, or one of a parameter's choices by
/// its name.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Value {
    N(f64),
    C(&'static str),
}

use Value::{C, N};

/// The kinds a patch is filed under, in the order a browser lists them.
pub const KINDS: &[&str] = &["Bass", "Lead", "Pluck", "Pad", "Keys", "Bell", "Strings", "Brass", "Texture", "FX"];

/// One sound.
pub struct Patch {
    pub name: &'static str,
    /// Where it is filed: one of [`KINDS`].
    pub category: &'static str,
    pub description: &'static str,
    pub tags: &'static [&'static str],
    /// What the four macros do in this sound; empty for one it does not
    /// use.
    pub macros: [&'static str; 4],
    pub params: &'static [(&'static str, Value)],
    /// The effects it brings behind the synth, which make it a sound
    /// (`trill_core::presets::SoundDevice`): in the order the signal goes
    /// through them. Empty for a patch that is dry, which is still a
    /// sound: given to a track, it takes the last sound's effects away.
    pub effects: &'static [Effect],
    /// The lowest and the highest key it is for (60 is middle C, C4):
    /// every key between them sounds as the sound is meant, which a test
    /// holds. Its description ends with them.
    pub range: (u8, u8),
}

/// An effect a patch brings: one of the built-in devices by its key, with
/// the values it is given. Every other parameter is as the device comes.
pub struct Effect {
    pub key: &'static str,
    pub params: &'static [(&'static str, f64)],
}

/// The chains the first sixteen patches share, one for each kind of
/// sound. They are the chains the first listening files were rendered
/// through.
const DRY: &[Effect] = &[];
/// An echo on the dotted eighth and a short room.
const LEAD: &[Effect] = &[
    Effect { key: "delay", params: &[("time_beats", 0.75), ("feedback", 0.35), ("mix", 0.18)] },
    Effect { key: "reverb", params: &[("decay", 1.8), ("mix", 0.18)] },
];
/// A chorus as it comes and a long hall.
const PAD: &[Effect] =
    &[Effect { key: "chorus", params: &[] }, Effect { key: "reverb", params: &[("decay", 4.0), ("mix", 0.3)] }];
/// A longer echo on the dotted eighth and a room.
const PLUCK: &[Effect] = &[
    Effect { key: "delay", params: &[("time_beats", 0.75), ("feedback", 0.4), ("mix", 0.22)] },
    Effect { key: "reverb", params: &[("decay", 2.2), ("mix", 0.2)] },
];
/// A chorus as it comes and a small room.
const KEYS: &[Effect] =
    &[Effect { key: "chorus", params: &[] }, Effect { key: "reverb", params: &[("decay", 1.6), ("mix", 0.16)] }];

/// What a key is called: middle C, 60, is C4.
pub fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[usize::from(key % 12)], i32::from(key / 12) - 1)
}

impl Patch {
    /// The patch's values as numbers, by parameter id. A choice that the
    /// parameter does not have is left out (a test holds that there is
    /// none).
    pub fn values(&self) -> Vec<(String, f64)> {
        self.params
            .iter()
            .filter_map(|(id, value)| {
                let number = match value {
                    N(number) => *number,
                    C(name) => {
                        let def = PARAMS.iter().find(|def| def.id == *id)?;
                        let Kind::Enum(choices) = def.kind else { return None };
                        choices.iter().position(|choice| choice == name)? as f64
                    }
                };
                Some((id.to_string(), number))
            })
            .collect()
    }

    /// What a parameter is set to, as the number it is kept as; `None`
    /// for one the patch leaves as the synth has it.
    pub fn value(&self, id: &str) -> Option<f64> {
        self.values().into_iter().find(|(set, _)| set == id).map(|(_, value)| value)
    }

    /// Whether it plays one note at a time.
    pub fn one_at_a_time(&self) -> bool {
        self.params.iter().any(|(id, value)| *id == "mode" && *value != C("poly"))
    }

    /// What it says of itself, ending with the keys it is for.
    pub fn says(&self) -> String {
        let (low, high) = self.range;
        format!("{}. Range {} to {}", self.description.trim_end_matches('.'), key_name(low), key_name(high))
    }

    pub fn preset(&self) -> Preset {
        Preset {
            name: self.name.to_string(),
            description: self.says(),
            params: self.values(),
            category: self.category.to_string(),
            tags: self.tags.iter().map(|tag| tag.to_string()).collect(),
            state: Some(State::with_macros(&self.macros).to_bytes()),
            after: Some(self.after()),
        }
    }

    /// The effects the patch brings, as devices to make behind the synth.
    pub fn after(&self) -> Vec<SoundDevice> {
        let device = |effect: &Effect| {
            let uri = format!("{}:{}", trill_core::device::BUILTIN_SCHEME, effect.key);
            SoundDevice::new(&uri, crate::name_of(effect.key).unwrap_or(effect.key), effect.params)
        };
        self.effects.iter().map(device).collect()
    }
}

/// Every patch, in the order a browser lists them: the plain one, then
/// kind by kind as [`KINDS`] has them.
const FILES: &[&[Patch]] = &[
    init::PATCHES,
    bass::PATCHES,
    lead::PATCHES,
    pluck::PATCHES,
    pad::PATCHES,
    keys::PATCHES,
    bell::PATCHES,
    ensemble::PATCHES,
    texture::PATCHES,
    fx::PATCHES,
];

/// Every patch of the bank.
pub fn all() -> impl Iterator<Item = &'static Patch> {
    FILES.iter().flat_map(|file| file.iter())
}

/// The bank as the presets of the device.
pub fn presets() -> Vec<Preset> {
    all().map(Patch::preset).collect()
}

/// The patch called `name`.
pub fn named(name: &str) -> Option<&'static Patch> {
    all().find(|patch| patch.name.eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_patch_sets_parameters_the_synth_has_to_values_they_take() {
        for patch in all() {
            let values = patch.values();
            assert_eq!(values.len(), patch.params.len(), "{}: a choice some parameter does not have", patch.name);
            for (id, value) in &values {
                let def =
                    PARAMS.iter().find(|def| def.id == *id).unwrap_or_else(|| panic!("{}: no '{id}'", patch.name));
                assert!((def.min..=def.max).contains(value), "{}: {id} = {value}", patch.name);
            }
            let mut ids: Vec<&str> = patch.params.iter().map(|(id, _)| *id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), patch.params.len(), "{}: a parameter set twice", patch.name);
            // A slot is whole or not there, and a macro that has a name
            // moves something.
            for slot in 1..=super::super::params::SLOTS {
                let has = |part: &str| patch.params.iter().any(|(id, _)| *id == format!("mod{slot}_{part}"));
                assert!(has("source") == has("dest") && has("dest") == has("amount"), "{}: slot {slot}", patch.name);
            }
            for (which, name) in patch.macros.iter().enumerate() {
                let source = C(["macro1", "macro2", "macro3", "macro4"][which]);
                let moves = patch.params.iter().any(|(id, value)| id.ends_with("_source") && *value == source);
                assert_eq!(moves, !name.is_empty(), "{}: macro {} ({name:?})", patch.name, which + 1);
            }
            // A macro starts where the patch leaves it: at nothing.
            for id in ["macro1", "macro2", "macro3", "macro4"] {
                assert_eq!(patch.value(id), None, "{}: {id} is the player's to move", patch.name);
            }
        }
    }

    #[test]
    fn every_patch_says_what_it_is_and_carries_the_names_of_its_macros() {
        let mut names: Vec<String> = all().map(|patch| patch.name.to_lowercase()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "two patches of one name");
        for patch in all().skip(1) {
            assert!(KINDS.contains(&patch.category), "{}", patch.name);
            assert!(patch.description.len() > 120, "{} says too little of itself", patch.name);
            assert!(patch.tags.len() >= 4, "{}", patch.name);
            // Its kind is one of the words it is found by.
            let kind = patch.category.to_lowercase();
            assert!(patch.tags.iter().any(|tag| *tag == kind), "{} is not tagged {kind}", patch.name);
            // What a macro does is said where a model reads it.
            for name in patch.macros.iter().filter(|name| !name.is_empty()) {
                assert!(
                    patch.description.contains(name),
                    "{}: its description does not say what {name} does",
                    patch.name
                );
            }
            // The keys it is for: an octave at the least, and said.
            let (low, high) = patch.range;
            assert!(low + 12 <= high && high <= 108 && low >= 12, "{}: {low} to {high}", patch.name);
            let preset = patch.preset();
            assert!(preset.description.ends_with(&format!("Range {} to {}", key_name(low), key_name(high))));
            let state = State::from_bytes(preset.state.as_deref().unwrap()).unwrap();
            assert_eq!(state.macro_name(0), patch.macros[0]);
            assert_eq!((preset.category.as_str(), preset.tags.len()), (patch.category, patch.tags.len()));
        }
        assert_eq!((key_name(60), key_name(28), key_name(61)), ("C4".into(), "E1".into(), "C#4".into()));
        assert_eq!(named(" tine piano ").map(|patch| patch.name), Some("Tine Piano"));
    }

    #[test]
    fn the_bank_has_what_a_song_of_each_style_needs_and_no_sound_twice() {
        // Near eighty sounds, weighted to the bottom of a song.
        let of = |kind: &str| all().filter(|patch| patch.category == kind).count();
        let total = all().count() - 1;
        assert!((76..=84).contains(&total), "{total} sounds");
        for (kind, fewest) in [
            ("Bass", 16),
            ("Lead", 8),
            ("Pluck", 8),
            ("Pad", 9),
            ("Keys", 7),
            ("Bell", 5),
            ("Strings", 3),
            ("Brass", 2),
            ("Texture", 4),
            ("FX", 7),
        ] {
            assert!(of(kind) >= fewest, "{kind}: {} of at least {fewest}", of(kind));
        }
        assert!(of("Bass") > of("Pad"), "basses first");
        // A sound that is another with one knob turned is not a sound: no
        // two patches of a kind differ in fewer than six of their values.
        let all: Vec<&Patch> = all().skip(1).collect();
        for (index, one) in all.iter().enumerate() {
            for other in &all[index + 1..] {
                if one.category != other.category {
                    continue;
                }
                let (a, b) = (one.values(), other.values());
                let voice = |id: &str| !id.starts_with("mod") && id != "volume";
                let differ = |from: &[(String, f64)], to: &[(String, f64)]| {
                    from.iter()
                        .filter(|(id, _)| voice(id))
                        .filter(|(id, value)| to.iter().find(|(other, _)| other == id).map(|(_, v)| v) != Some(value))
                        .count()
                };
                let apart = differ(&a, &b).max(differ(&b, &a));
                assert!(apart >= 6, "{} and {} differ in {apart} values", one.name, other.name);
            }
        }
    }
}
