//! The sounds the wavetable synth ships with.
//!
//! A patch is a set of parameter values and the names of its macros. Its
//! description says what it sounds like and what it is for, which is what
//! a model that cannot hear chooses it by.

use super::State;
use trill_core::device::Preset;

/// One sound.
pub struct Patch {
    pub name: &'static str,
    /// Where it is filed: `Bass`, `Lead`, `Pad`, `Pluck`, `Keys`.
    pub category: &'static str,
    pub description: &'static str,
    pub tags: &'static [&'static str],
    /// What the four macros do in this sound; empty for one it does not
    /// use.
    pub macros: [&'static str; 4],
    pub params: &'static [(&'static str, f64)],
}

impl Patch {
    pub fn preset(&self) -> Preset {
        Preset {
            name: self.name.to_string(),
            description: self.description.to_string(),
            params: self.params.iter().map(|(id, value)| (id.to_string(), *value)).collect(),
            category: self.category.to_string(),
            tags: self.tags.iter().map(|tag| tag.to_string()).collect(),
            state: Some(State::with_macros(&self.macros).to_bytes()),
        }
    }
}

pub static PATCHES: &[Patch] = &[Patch {
    name: "init",
    category: "",
    description: "A plain sawtooth, filter open: the place to start a sound from nothing",
    tags: &[],
    macros: ["", "", "", ""],
    params: &[],
}];

/// The presets, as the device lists them.
pub fn presets() -> Vec<Preset> {
    PATCHES.iter().map(Patch::preset).collect()
}
