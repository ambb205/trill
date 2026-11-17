//! The sound a synth has before it is given one.

use super::*;

pub(super) const PATCHES: &[Patch] = &[Patch {
    name: "init",
    category: "",
    effects: DRY,
    range: (24, 96),
    description: "A plain sawtooth with nothing done to it: the place to start a sound from nothing",
    tags: &["plain", "saw"],
    macros: ["", "", "", ""],
    params: &[],
}];
