//! The sounds the wavetable synth ships with.
//!
//! A patch is a set of parameter values and the names of its macros. Its
//! description says what it sounds like and what it is for, which is what
//! a model that cannot hear chooses it by; the macros are the handles
//! worth reaching for first.
//!
//! None of these has been heard by whoever wrote them: they are built
//! from how such sounds are made, and matched in level by measurement
//! (the tests hold that). The bank is to be shaped by ear.
//!
//! The synth has no effects inside. A patch that wants an echo or a room
//! says so ([`Patch::effects`]) and is a *sound*: given to a track, the
//! effects are made behind the synth as devices of their own, to be seen,
//! changed and taken away like any other.

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
    pub params: &'static [(&'static str, Value)],
    /// The effects it brings behind the synth, which make it a sound
    /// (`trill_core::presets::SoundDevice`): in the order the signal goes
    /// through them. Empty for a patch that is dry, which is still a
    /// sound: given to a track, it takes the last sound's effects away.
    pub effects: &'static [Effect],
}

/// An effect a patch brings: one of the built-in devices by its key, with
/// the values it is given. Every other parameter is as the device comes.
pub struct Effect {
    pub key: &'static str,
    pub params: &'static [(&'static str, f64)],
}

/// The chains the patches share, one for each kind of sound. They are the
/// chains the first listening files were rendered through.
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

    pub fn preset(&self) -> Preset {
        Preset {
            name: self.name.to_string(),
            description: self.description.to_string(),
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

pub static PATCHES: &[Patch] = &[
    Patch {
        name: "init",
        category: "",
        effects: DRY,
        description: "A plain sawtooth with nothing done to it: the place to start a sound from nothing",
        tags: &["plain", "saw"],
        macros: ["", "", "", ""],
        params: &[],
    },
    Patch {
        name: "Solid Bass",
        category: "Bass",
        effects: DRY,
        description: "A firm, round analog bass with a short filter pluck on every note: saw and square over a \
                      sine at the note itself that goes past the filter. For bass lines of any style; write it between E1 and G2. \
                      Macros: Cutoff opens it, Drive roughens it, Sub adds weight, Resonance makes the pluck ring",
        tags: &["bass", "analog", "mono", "punchy", "warm"],
        macros: ["Cutoff", "Drive", "Sub", "Resonance"],
        params: &[
            ("osc1_level", N(0.8)),
            ("osc1_phase_random", N(0.0)),
            ("osc2_position", N(0.75)),
            ("osc2_level", N(0.5)),
            ("osc2_fine", N(6.0)),
            ("osc2_phase_random", N(0.0)),
            ("sub_octave", N(0.0)),
            ("sub_level", N(0.6)),
            ("sub_direct", N(1.0)),
            ("filter1_type", C("lowpass24")),
            ("filter1_cutoff", N(160.0)),
            ("filter1_resonance", N(0.22)),
            ("filter1_drive", N(6.0)),
            ("filter1_key_track", N(0.5)),
            ("env1_attack", N(0.003)),
            ("env1_decay", N(0.9)),
            ("env1_sustain", N(0.75)),
            ("env1_release", N(0.12)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(0.22)),
            ("env2_sustain", N(0.15)),
            ("env2_release", N(0.12)),
            ("mod1_source", C("env2")),
            ("mod1_dest", C("filter1_cutoff")),
            ("mod1_amount", N(0.32)),
            ("mod2_source", C("velocity")),
            ("mod2_dest", C("filter1_cutoff")),
            ("mod2_amount", N(0.1)),
            ("mod3_source", C("macro1")),
            ("mod3_dest", C("filter1_cutoff")),
            ("mod3_amount", N(0.35)),
            ("mod4_source", C("macro2")),
            ("mod4_dest", C("filter1_drive")),
            ("mod4_amount", N(0.6)),
            ("mod5_source", C("macro3")),
            ("mod5_dest", C("sub_level")),
            ("mod5_amount", N(0.4)),
            ("mod6_source", C("macro4")),
            ("mod6_dest", C("filter1_resonance")),
            ("mod6_amount", N(0.5)),
            ("mode", C("mono")),
            ("glide", N(0.02)),
            ("velocity_sens", N(0.4)),
            ("volume", N(-22.2)),
        ],
    },
    Patch {
        name: "Reese",
        category: "Bass",
        effects: DRY,
        description: "Two detuned sawtooth stacks grinding slowly against each other through a low-pass: the dark, \
                      moving bass of drum and bass and garage. Long notes show its movement, and notes that overlap \
                      slide; write it between E1 and E3. Macros: Cutoff, Movement (how far the filter drifts), \
                      Grit, Sub",
        tags: &["bass", "reese", "dark", "detuned", "dnb", "wide"],
        macros: ["Cutoff", "Movement", "Grit", "Sub"],
        params: &[
            ("osc1_unison", N(3.0)),
            ("osc1_detune", N(22.0)),
            ("osc1_spread", N(0.6)),
            ("osc2_level", N(0.9)),
            ("osc2_unison", N(3.0)),
            ("osc2_detune", N(30.0)),
            ("osc2_fine", N(-11.0)),
            ("osc2_spread", N(0.8)),
            ("sub_octave", N(0.0)),
            ("sub_level", N(0.45)),
            ("sub_direct", N(1.0)),
            ("filter1_type", C("lowpass24")),
            ("filter1_cutoff", N(520.0)),
            ("filter1_resonance", N(0.3)),
            ("filter1_drive", N(4.0)),
            ("filter1_key_track", N(0.3)),
            ("env1_attack", N(0.01)),
            ("env1_decay", N(1.0)),
            ("env1_sustain", N(0.9)),
            ("env1_release", N(0.2)),
            ("lfo3_rate", N(0.13)),
            ("lfo3_retrigger", N(0.0)),
            ("lfo3_depth", N(0.5)),
            ("lfo2_rate", N(0.21)),
            ("lfo2_retrigger", N(0.0)),
            ("mod1_source", C("lfo3")),
            ("mod1_dest", C("filter1_cutoff")),
            ("mod1_amount", N(0.12)),
            ("mod2_source", C("lfo2")),
            ("mod2_dest", C("osc2_pitch")),
            ("mod2_amount", N(0.003)),
            ("mod3_source", C("macro1")),
            ("mod3_dest", C("filter1_cutoff")),
            ("mod3_amount", N(0.4)),
            ("mod4_source", C("macro2")),
            ("mod4_dest", C("lfo3_depth")),
            ("mod4_amount", N(0.5)),
            ("mod5_source", C("macro3")),
            ("mod5_dest", C("filter1_drive")),
            ("mod5_amount", N(0.5)),
            ("mod6_source", C("macro4")),
            ("mod6_dest", C("sub_level")),
            ("mod6_amount", N(0.5)),
            ("mode", C("legato")),
            ("glide", N(0.07)),
            ("velocity_sens", N(0.3)),
            ("volume", N(-20.2)),
        ],
    },
    Patch {
        name: "Acid Line",
        category: "Bass",
        effects: DRY,
        description: "A squelching resonant bass in the manner of a 303: every note opens the resonance and lets it \
                      fall, harder notes further, and notes that overlap slide into each other. For acid lines and \
                      rubbery plucked basses between C1 and C4. Macros: Squelch (how far open it sits), Drive, \
                      Bright, Snap (how far each note opens it)",
        tags: &["bass", "acid", "303", "resonant", "squelch", "mono"],
        macros: ["Squelch", "Drive", "Bright", "Snap"],
        params: &[
            ("osc1_table", C("Acid")),
            ("osc1_position", N(0.12)),
            ("osc1_phase_random", N(0.0)),
            ("filter1_type", C("lowpass12")),
            ("filter1_cutoff", N(4500.0)),
            ("filter1_drive", N(8.0)),
            ("env1_attack", N(0.002)),
            ("env1_decay", N(0.6)),
            ("env1_sustain", N(0.6)),
            ("env1_release", N(0.08)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(0.2)),
            ("env2_release", N(0.1)),
            ("mod1_source", C("env2")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.5)),
            ("mod2_source", C("velocity")),
            ("mod2_dest", C("osc1_position")),
            ("mod2_amount", N(0.25)),
            ("mod3_source", C("macro1")),
            ("mod3_dest", C("osc1_position")),
            ("mod3_amount", N(0.5)),
            ("mod4_source", C("macro2")),
            ("mod4_dest", C("filter1_drive")),
            ("mod4_amount", N(0.5)),
            ("mod5_source", C("macro3")),
            ("mod5_dest", C("filter1_cutoff")),
            ("mod5_amount", N(0.2)),
            ("mod6_source", C("macro4")),
            ("mod6_dest", C("env2_depth")),
            ("mod6_amount", N(0.8)),
            ("mode", C("legato")),
            ("glide", N(0.07)),
            ("velocity_sens", N(0.3)),
            ("volume", N(-13.8)),
        ],
    },
    Patch {
        name: "Growl Bass",
        category: "Bass",
        effects: DRY,
        description: "A snarling bass whose tone wobbles in time with the song, one cycle to the quarter note \
                      (`lfo1_sync` sets another length): the talking bass of dubstep and electro. Hold long notes \
                      between C1 and C3. Macros: Wobble (how far it moves), Tone, Dirt, Vowel (where the wobble \
                      sits)",
        tags: &["bass", "growl", "wobble", "dubstep", "aggressive", "synced"],
        macros: ["Wobble", "Tone", "Dirt", "Vowel"],
        params: &[
            ("osc1_table", C("Growl")),
            ("osc1_position", N(0.3)),
            ("osc1_unison", N(2.0)),
            ("osc1_detune", N(10.0)),
            ("osc1_spread", N(0.3)),
            ("sub_octave", N(0.0)),
            ("sub_level", N(0.55)),
            ("sub_direct", N(1.0)),
            ("filter1_type", C("lowpass24")),
            ("filter1_cutoff", N(900.0)),
            ("filter1_resonance", N(0.3)),
            ("filter1_drive", N(9.0)),
            ("filter1_key_track", N(0.3)),
            ("env1_attack", N(0.005)),
            ("env1_sustain", N(0.9)),
            ("env1_release", N(0.12)),
            ("lfo1_shape", C("triangle")),
            ("lfo1_sync", C("1/4")),
            ("lfo1_retrigger", N(0.0)),
            ("lfo1_depth", N(0.6)),
            ("mod1_source", C("lfo1")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.45)),
            ("mod2_source", C("lfo1")),
            ("mod2_dest", C("filter1_cutoff")),
            ("mod2_amount", N(0.15)),
            ("mod3_source", C("macro1")),
            ("mod3_dest", C("lfo1_depth")),
            ("mod3_amount", N(0.4)),
            ("mod4_source", C("macro2")),
            ("mod4_dest", C("filter1_cutoff")),
            ("mod4_amount", N(0.35)),
            ("mod5_source", C("macro3")),
            ("mod5_dest", C("filter1_drive")),
            ("mod5_amount", N(0.5)),
            ("mod6_source", C("macro4")),
            ("mod6_dest", C("osc1_position")),
            ("mod6_amount", N(0.3)),
            ("mode", C("mono")),
            ("glide", N(0.04)),
            ("velocity_sens", N(0.3)),
            ("volume", N(-18.5)),
        ],
    },
    Patch {
        name: "Deep Sub",
        category: "Bass",
        effects: DRY,
        description: "A clean, heavy sub with a short knock at the front of each note, as an 808 has: a sine that \
                      falls an octave onto its pitch in a few hundredths of a second, then rings. For the bottom \
                      of a track; write it between C1 and C3. Macros: Tone (adds harmonics, so that it is heard on \
                      small speakers), Drive, Knock (how far the note falls)",
        tags: &["bass", "sub", "808", "sine", "deep", "clean"],
        macros: ["Tone", "Drive", "Knock", ""],
        params: &[
            ("osc1_position", N(0.0)),
            ("osc1_phase_random", N(0.0)),
            ("filter1_type", C("lowpass12")),
            ("filter1_cutoff", N(700.0)),
            ("filter1_key_track", N(0.5)),
            ("env1_attack", N(0.002)),
            ("env1_decay", N(2.5)),
            ("env1_sustain", N(0.6)),
            ("env1_release", N(0.25)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(0.04)),
            ("mod1_source", C("env2")),
            ("mod1_dest", C("pitch")),
            ("mod1_amount", N(0.25)),
            ("mod2_source", C("macro1")),
            ("mod2_dest", C("osc1_position")),
            ("mod2_amount", N(0.4)),
            ("mod3_source", C("macro2")),
            ("mod3_dest", C("filter1_drive")),
            ("mod3_amount", N(0.7)),
            ("mod4_source", C("macro3")),
            ("mod4_dest", C("env2_depth")),
            ("mod4_amount", N(1.0)),
            ("mode", C("mono")),
            ("glide", N(0.03)),
            ("velocity_sens", N(0.3)),
            ("volume", N(-18.8)),
        ],
    },
    Patch {
        name: "Sync Lead",
        category: "Lead",
        effects: LEAD,
        description: "A tearing, vocal lead: each note rips upward through oscillator sync and settles, with \
                      vibrato that comes in on held notes and more of it on the mod wheel. One note at a time, \
                      sliding between notes that overlap; for solos and hooks from C3 to C6. Macros: Rip (how \
                      high the tear sits), Vibrato, Bright, Body",
        tags: &["lead", "sync", "mono", "aggressive", "solo", "bright"],
        macros: ["Rip", "Vibrato", "Bright", "Body"],
        params: &[
            ("osc1_table", C("Sync Sweep")),
            ("osc1_position", N(0.25)),
            ("osc1_unison", N(2.0)),
            ("osc1_detune", N(8.0)),
            ("osc1_spread", N(0.3)),
            ("osc2_level", N(0.3)),
            ("osc2_fine", N(5.0)),
            ("filter1_type", C("lowpass12")),
            ("filter1_cutoff", N(3500.0)),
            ("filter1_resonance", N(0.1)),
            ("filter1_key_track", N(0.4)),
            ("env1_attack", N(0.004)),
            ("env1_decay", N(0.8)),
            ("env1_sustain", N(0.85)),
            ("env1_release", N(0.18)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(0.35)),
            ("env2_sustain", N(0.25)),
            ("env2_release", N(0.2)),
            ("lfo1_rate", N(5.6)),
            ("lfo1_fade", N(0.35)),
            ("lfo1_depth", N(0.35)),
            ("mod1_source", C("env2")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.45)),
            ("mod2_source", C("lfo1")),
            ("mod2_dest", C("fine_pitch")),
            ("mod2_amount", N(0.3)),
            ("mod3_source", C("mod_wheel")),
            ("mod3_dest", C("lfo1_depth")),
            ("mod3_amount", N(0.65)),
            ("mod4_source", C("macro1")),
            ("mod4_dest", C("osc1_position")),
            ("mod4_amount", N(0.4)),
            ("mod5_source", C("macro2")),
            ("mod5_dest", C("lfo1_depth")),
            ("mod5_amount", N(0.6)),
            ("mod6_source", C("macro3")),
            ("mod6_dest", C("filter1_cutoff")),
            ("mod6_amount", N(0.25)),
            ("mod7_source", C("macro4")),
            ("mod7_dest", C("osc2_level")),
            ("mod7_amount", N(0.5)),
            ("mode", C("legato")),
            ("glide", N(0.05)),
            ("velocity_sens", N(0.4)),
            ("volume", N(-18.4)),
        ],
    },
    Patch {
        name: "Anthem Saws",
        category: "Lead",
        effects: LEAD,
        description: "Seven detuned sawtooths with an octave on top: the wide, bright wall of trance and big-room \
                      chords and leads. Play chords in the middle of the keyboard or a single line up high. \
                      Macros: Bright, Octave (the upper stack), Air (a breath of noise on top)",
        tags: &["lead", "supersaw", "chords", "wide", "bright", "trance", "pad"],
        macros: ["Bright", "Octave", "Air", ""],
        params: &[
            ("osc1_unison", N(7.0)),
            ("osc1_detune", N(26.0)),
            ("osc1_blend", N(0.85)),
            ("osc1_spread", N(0.9)),
            ("osc2_level", N(0.4)),
            ("osc2_octave", N(1.0)),
            ("osc2_unison", N(5.0)),
            ("osc2_detune", N(20.0)),
            ("osc2_spread", N(1.0)),
            ("noise_color", N(0.8)),
            ("noise_width", N(1.0)),
            ("filter1_type", C("lowpass12")),
            ("filter1_cutoff", N(4200.0)),
            ("filter1_resonance", N(0.05)),
            ("filter1_key_track", N(0.4)),
            ("env1_attack", N(0.006)),
            ("env1_decay", N(1.5)),
            ("env1_sustain", N(0.85)),
            ("env1_release", N(0.4)),
            ("mod1_source", C("macro1")),
            ("mod1_dest", C("filter1_cutoff")),
            ("mod1_amount", N(0.22)),
            ("mod2_source", C("macro2")),
            ("mod2_dest", C("osc2_level")),
            ("mod2_amount", N(0.5)),
            ("mod3_source", C("macro3")),
            ("mod3_dest", C("noise_level")),
            ("mod3_amount", N(0.3)),
            ("velocity_sens", N(0.4)),
            ("volume", N(-23.1)),
        ],
    },
    Patch {
        name: "Talk Lead",
        category: "Lead",
        effects: LEAD,
        description: "A lead that speaks: one vocal resonance that the mod wheel and macro 1 move from 'oo' \
                      through 'ah' to 'ee', drifting a little on its own. One note at a time; for hooks and bass \
                      lines that should sound like a voice, from C2 to C5. Macros: Vowel, Drift, Vibrato, Body",
        tags: &["lead", "vocal", "formant", "talk", "mono", "vowel"],
        macros: ["Vowel", "Drift", "Vibrato", "Body"],
        params: &[
            ("osc1_table", C("Formant Sweep")),
            ("osc1_position", N(0.3)),
            ("osc2_level", N(0.2)),
            ("filter1_type", C("lowpass24")),
            ("filter1_cutoff", N(4500.0)),
            ("filter1_resonance", N(0.2)),
            ("env1_attack", N(0.01)),
            ("env1_sustain", N(0.9)),
            ("env1_release", N(0.15)),
            ("lfo1_rate", N(5.3)),
            ("lfo1_fade", N(0.4)),
            ("lfo1_depth", N(0.4)),
            ("lfo2_shape", C("smooth")),
            ("lfo2_rate", N(0.7)),
            ("lfo2_retrigger", N(0.0)),
            ("lfo2_depth", N(0.5)),
            ("mod1_source", C("lfo2")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.2)),
            ("mod2_source", C("mod_wheel")),
            ("mod2_dest", C("osc1_position")),
            ("mod2_amount", N(0.6)),
            ("mod3_source", C("lfo1")),
            ("mod3_dest", C("fine_pitch")),
            ("mod3_amount", N(0.25)),
            ("mod4_source", C("macro1")),
            ("mod4_dest", C("osc1_position")),
            ("mod4_amount", N(0.6)),
            ("mod5_source", C("macro2")),
            ("mod5_dest", C("lfo2_depth")),
            ("mod5_amount", N(0.5)),
            ("mod6_source", C("macro3")),
            ("mod6_dest", C("lfo1_depth")),
            ("mod6_amount", N(0.6)),
            ("mod7_source", C("macro4")),
            ("mod7_dest", C("osc2_level")),
            ("mod7_amount", N(0.5)),
            ("mode", C("legato")),
            ("glide", N(0.06)),
            ("velocity_sens", N(0.4)),
            ("volume", N(-13.0)),
        ],
    },
    Patch {
        name: "Glass Pad",
        category: "Pad",
        effects: PAD,
        description: "A slow, glittering pad: thin high partials that shift over a soft warm bed, each note \
                      drifting at its own pace. For held chords behind a track; it takes a second to come in and \
                      three to go. Macros: Shimmer (moves the glitter), Warmth (the bed under it), Close (shuts \
                      the filter), Motion",
        tags: &["pad", "glass", "airy", "slow", "evolving", "ambient", "digital"],
        macros: ["Shimmer", "Warmth", "Close", "Motion"],
        params: &[
            ("osc1_table", C("Glass")),
            ("osc1_position", N(0.3)),
            ("osc1_level", N(0.8)),
            ("osc1_unison", N(4.0)),
            ("osc1_detune", N(12.0)),
            ("osc1_spread", N(0.9)),
            ("osc2_table", C("Sine to Saw")),
            ("osc2_position", N(0.3)),
            ("osc2_level", N(0.6)),
            ("osc2_unison", N(3.0)),
            ("osc2_detune", N(9.0)),
            ("osc2_spread", N(0.6)),
            ("filter1_type", C("lowpass12")),
            ("filter1_cutoff", N(3800.0)),
            ("env1_attack", N(0.9)),
            ("env1_decay", N(2.0)),
            ("env1_sustain", N(0.85)),
            ("env1_release", N(2.8)),
            ("env2_attack", N(1.5)),
            ("env2_decay", N(3.0)),
            ("env2_sustain", N(0.6)),
            ("env2_release", N(2.5)),
            ("lfo3_shape", C("triangle")),
            ("lfo3_rate", N(0.07)),
            ("lfo3_phase", N(0.25)),
            ("lfo3_depth", N(0.5)),
            ("lfo2_rate", N(0.11)),
            ("mod1_source", C("lfo3")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.5)),
            ("mod2_source", C("random")),
            ("mod2_dest", C("lfo3_rate")),
            ("mod2_amount", N(0.2)),
            ("mod3_source", C("lfo2")),
            ("mod3_dest", C("osc2_position")),
            ("mod3_amount", N(0.1)),
            ("mod4_source", C("env2")),
            ("mod4_dest", C("filter1_cutoff")),
            ("mod4_amount", N(0.1)),
            ("mod5_source", C("macro1")),
            ("mod5_dest", C("osc1_position")),
            ("mod5_amount", N(0.4)),
            ("mod6_source", C("macro2")),
            ("mod6_dest", C("osc2_level")),
            ("mod6_amount", N(0.4)),
            ("mod7_source", C("macro3")),
            ("mod7_dest", C("filter1_cutoff")),
            ("mod7_amount", N(-0.3)),
            ("mod8_source", C("macro4")),
            ("mod8_dest", C("lfo3_depth")),
            ("mod8_amount", N(0.5)),
            ("velocity_sens", N(0.25)),
            ("volume", N(-24.0)),
        ],
    },
    Patch {
        name: "PWM Strings",
        category: "Pad",
        effects: PAD,
        description: "Warm string-machine strings: pulse waves whose width drifts slowly, each note at its own \
                      pace, over a bowed-string tone, with a gentle vibrato on held notes. For chords and slow \
                      lines. Macros: Bright, Movement (how far the pulse width drifts), Vibrato",
        tags: &["pad", "strings", "analog", "warm", "pwm", "ensemble", "chords"],
        macros: ["Bright", "Movement", "Vibrato", ""],
        params: &[
            ("osc1_table", C("Pulse Width")),
            ("osc1_position", N(0.3)),
            ("osc1_unison", N(4.0)),
            ("osc1_detune", N(16.0)),
            ("osc1_spread", N(0.85)),
            ("osc2_table", C("Strings")),
            ("osc2_position", N(0.45)),
            ("osc2_level", N(0.7)),
            ("osc2_unison", N(3.0)),
            ("osc2_detune", N(12.0)),
            ("osc2_spread", N(0.7)),
            ("filter1_type", C("lowpass24")),
            ("filter1_cutoff", N(2200.0)),
            ("filter1_resonance", N(0.12)),
            ("filter1_key_track", N(0.35)),
            ("env1_attack", N(0.35)),
            ("env1_decay", N(1.5)),
            ("env1_sustain", N(0.9)),
            ("env1_release", N(1.4)),
            ("env2_attack", N(0.6)),
            ("env2_decay", N(2.0)),
            ("env2_sustain", N(0.7)),
            ("env2_release", N(1.5)),
            ("lfo1_shape", C("triangle")),
            ("lfo1_rate", N(0.4)),
            ("lfo1_depth", N(0.6)),
            ("lfo2_rate", N(5.2)),
            ("lfo2_fade", N(1.0)),
            ("lfo2_depth", N(0.5)),
            ("mod1_source", C("lfo1")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.35)),
            ("mod2_source", C("random")),
            ("mod2_dest", C("lfo1_rate")),
            ("mod2_amount", N(0.15)),
            ("mod3_source", C("env2")),
            ("mod3_dest", C("filter1_cutoff")),
            ("mod3_amount", N(0.12)),
            ("mod4_source", C("lfo2")),
            ("mod4_dest", C("fine_pitch")),
            ("mod4_amount", N(0.12)),
            ("mod5_source", C("macro1")),
            ("mod5_dest", C("filter1_cutoff")),
            ("mod5_amount", N(0.3)),
            ("mod6_source", C("macro2")),
            ("mod6_dest", C("lfo1_depth")),
            ("mod6_amount", N(0.4)),
            ("mod7_source", C("macro3")),
            ("mod7_dest", C("lfo2_depth")),
            ("mod7_amount", N(0.5)),
            ("velocity_sens", N(0.35)),
            ("volume", N(-27.4)),
        ],
    },
    Patch {
        name: "Choir",
        category: "Pad",
        effects: PAD,
        description: "Voices holding a vowel that the mod wheel and macro 1 change, from 'ah' through 'ee' and \
                      'oh' to 'oo', with a breath of air on top. For choir pads and slow lines between C2 and C5, \
                      where the vowels are truest. Macros: Vowel, Breath, Vibrato, Dark",
        tags: &["pad", "choir", "vocal", "voices", "vowel", "soft"],
        macros: ["Vowel", "Breath", "Vibrato", "Dark"],
        params: &[
            ("osc1_table", C("Vowels")),
            ("osc1_position", N(0.0)),
            ("osc1_unison", N(5.0)),
            ("osc1_detune", N(18.0)),
            ("osc1_blend", N(0.9)),
            ("osc1_spread", N(0.9)),
            ("noise_level", N(0.05)),
            ("noise_color", N(0.6)),
            ("noise_width", N(1.0)),
            ("filter1_type", C("lowpass12")),
            ("filter1_cutoff", N(6000.0)),
            ("env1_attack", N(0.3)),
            ("env1_decay", N(1.0)),
            ("env1_sustain", N(0.9)),
            ("env1_release", N(1.2)),
            ("lfo1_rate", N(5.0)),
            ("lfo1_fade", N(0.8)),
            ("lfo1_depth", N(0.5)),
            ("lfo3_rate", N(0.09)),
            ("mod1_source", C("lfo3")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.06)),
            ("mod2_source", C("random")),
            ("mod2_dest", C("lfo3_rate")),
            ("mod2_amount", N(0.2)),
            ("mod3_source", C("mod_wheel")),
            ("mod3_dest", C("osc1_position")),
            ("mod3_amount", N(0.9)),
            ("mod4_source", C("lfo1")),
            ("mod4_dest", C("fine_pitch")),
            ("mod4_amount", N(0.15)),
            ("mod5_source", C("macro1")),
            ("mod5_dest", C("osc1_position")),
            ("mod5_amount", N(1.0)),
            ("mod6_source", C("macro2")),
            ("mod6_dest", C("noise_level")),
            ("mod6_amount", N(0.25)),
            ("mod7_source", C("macro3")),
            ("mod7_dest", C("lfo1_depth")),
            ("mod7_amount", N(0.5)),
            ("mod8_source", C("macro4")),
            ("mod8_dest", C("filter1_cutoff")),
            ("mod8_amount", N(-0.25)),
            ("velocity_sens", N(0.3)),
            ("volume", N(-18.3)),
        ],
    },
    Patch {
        name: "Glass Pluck",
        category: "Pluck",
        effects: PLUCK,
        description: "A short, glassy pluck that starts bright and rings down to a soft tone: bell-like, clean \
                      and digital, brighter the harder it is played. For arpeggios and melodies from C3 up. \
                      Macros: Bright, Octave (a pure tone an octave up), Snap (how far each note opens)",
        tags: &["pluck", "glass", "bell", "fm", "arp", "digital", "clean"],
        macros: ["Bright", "Octave", "Snap", ""],
        params: &[
            ("osc1_table", C("FM Glass")),
            ("osc1_position", N(0.08)),
            ("osc1_unison", N(2.0)),
            ("osc1_detune", N(7.0)),
            ("osc1_spread", N(0.5)),
            ("osc1_phase_random", N(0.0)),
            ("osc2_position", N(0.0)),
            ("osc2_level", N(0.2)),
            ("osc2_octave", N(1.0)),
            ("osc2_phase_random", N(0.0)),
            ("filter1_type", C("lowpass12")),
            ("filter1_cutoff", N(8000.0)),
            ("filter1_key_track", N(0.5)),
            ("env1_attack", N(0.001)),
            ("env1_decay", N(0.9)),
            ("env1_sustain", N(0.0)),
            ("env1_release", N(0.6)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(0.28)),
            ("env2_release", N(0.25)),
            ("mod1_source", C("env2")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.5)),
            ("mod2_source", C("velocity")),
            ("mod2_dest", C("osc1_position")),
            ("mod2_amount", N(0.25)),
            ("mod3_source", C("macro1")),
            ("mod3_dest", C("osc1_position")),
            ("mod3_amount", N(0.3)),
            ("mod4_source", C("macro2")),
            ("mod4_dest", C("osc2_level")),
            ("mod4_amount", N(0.5)),
            ("mod5_source", C("macro3")),
            ("mod5_dest", C("env2_depth")),
            ("mod5_amount", N(0.8)),
            ("velocity_sens", N(0.7)),
            ("volume", N(-21.7)),
        ],
    },
    Patch {
        name: "String Pluck",
        category: "Pluck",
        effects: PLUCK,
        description: "A plucked string made by a comb filter that rings at the note: a burst of noise and \
                      sawtooth goes in, a string comes out. Between a harp, a koto and a clean guitar; for \
                      arpeggios and picked lines from C2 to C6. Macros: Damp (a shorter ring), Bright, Pick (a \
                      harder burst)",
        tags: &["pluck", "string", "harp", "koto", "guitar", "comb", "physical", "acoustic"],
        macros: ["Damp", "Bright", "Pick", ""],
        params: &[
            ("osc1_level", N(0.0)),
            ("osc1_phase_random", N(0.0)),
            ("filter1_type", C("comb")),
            ("filter1_cutoff", N(261.63)),
            ("filter1_resonance", N(0.955)),
            ("filter1_key_track", N(1.0)),
            ("filter2_type", C("lowpass12")),
            ("filter2_cutoff", N(4200.0)),
            ("filter2_key_track", N(0.5)),
            ("env1_attack", N(0.001)),
            ("env1_decay", N(3.0)),
            ("env1_sustain", N(0.0)),
            ("env1_release", N(0.45)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(0.03)),
            ("env2_release", N(0.03)),
            ("mod1_source", C("env2")),
            ("mod1_dest", C("noise_level")),
            ("mod1_amount", N(0.9)),
            ("mod2_source", C("env2")),
            ("mod2_dest", C("osc1_level")),
            ("mod2_amount", N(0.5)),
            ("mod3_source", C("velocity")),
            ("mod3_dest", C("filter2_cutoff")),
            ("mod3_amount", N(0.15)),
            ("mod4_source", C("macro1")),
            ("mod4_dest", C("filter1_resonance")),
            ("mod4_amount", N(-0.25)),
            ("mod5_source", C("macro2")),
            ("mod5_dest", C("filter2_cutoff")),
            ("mod5_amount", N(0.2)),
            ("mod6_source", C("macro3")),
            ("mod6_dest", C("env2_depth")),
            ("mod6_amount", N(0.8)),
            ("velocity_sens", N(0.6)),
            ("volume", N(-3.7)),
        ],
    },
    Patch {
        name: "Trance Pluck",
        category: "Pluck",
        effects: PLUCK,
        description: "The classic short synth pluck: bright sawtooths through a low-pass that snaps shut on \
                      every note. For arpeggios, stabs and chord rhythms anywhere on the keyboard. Macros: Cutoff, \
                      Snap (how far each note opens it), Resonance",
        tags: &["pluck", "analog", "arp", "stab", "trance", "bright", "short"],
        macros: ["Cutoff", "Snap", "Resonance", ""],
        params: &[
            ("osc1_unison", N(3.0)),
            ("osc1_detune", N(14.0)),
            ("osc1_spread", N(0.7)),
            ("osc2_position", N(0.75)),
            ("osc2_level", N(0.5)),
            ("osc2_fine", N(-4.0)),
            ("filter1_type", C("lowpass24")),
            ("filter1_cutoff", N(320.0)),
            ("filter1_resonance", N(0.22)),
            ("filter1_key_track", N(0.5)),
            ("env1_attack", N(0.001)),
            ("env1_decay", N(0.45)),
            ("env1_sustain", N(0.0)),
            ("env1_release", N(0.35)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(0.2)),
            ("env2_release", N(0.2)),
            ("mod1_source", C("env2")),
            ("mod1_dest", C("filter1_cutoff")),
            ("mod1_amount", N(0.5)),
            ("mod2_source", C("velocity")),
            ("mod2_dest", C("filter1_cutoff")),
            ("mod2_amount", N(0.1)),
            ("mod3_source", C("macro1")),
            ("mod3_dest", C("filter1_cutoff")),
            ("mod3_amount", N(0.3)),
            ("mod4_source", C("macro2")),
            ("mod4_dest", C("env2_depth")),
            ("mod4_amount", N(0.8)),
            ("mod5_source", C("macro3")),
            ("mod5_dest", C("filter1_resonance")),
            ("mod5_amount", N(0.5)),
            ("velocity_sens", N(0.5)),
            ("volume", N(-19.7)),
        ],
    },
    Patch {
        name: "Tine Piano",
        category: "Keys",
        effects: KEYS,
        description: "An electric piano with a struck metal tine: a round body with a bell on top that harder \
                      notes bring out and that fades as the note rings. For chords, comping and ballads. Macros: \
                      Bell, Tremolo (pans the sound slowly from side to side), Dark",
        tags: &["keys", "electric piano", "rhodes", "tine", "warm", "chords", "soft"],
        macros: ["Bell", "Tremolo", "Dark", ""],
        params: &[
            ("osc1_table", C("Tine")),
            ("osc1_position", N(0.12)),
            ("osc1_unison", N(2.0)),
            ("osc1_detune", N(4.0)),
            ("osc1_spread", N(0.25)),
            ("osc1_phase_random", N(0.0)),
            ("osc2_position", N(0.0)),
            ("osc2_level", N(0.35)),
            ("osc2_phase_random", N(0.0)),
            ("filter1_type", C("lowpass12")),
            ("filter1_cutoff", N(5500.0)),
            ("filter1_key_track", N(0.4)),
            ("env1_attack", N(0.002)),
            ("env1_decay", N(3.2)),
            ("env1_sustain", N(0.1)),
            ("env1_release", N(0.45)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(1.4)),
            ("env2_release", N(0.4)),
            ("lfo2_rate", N(4.5)),
            ("lfo2_retrigger", N(0.0)),
            ("lfo2_depth", N(0.0)),
            ("mod1_source", C("velocity")),
            ("mod1_dest", C("osc1_position")),
            ("mod1_amount", N(0.55)),
            ("mod2_source", C("env2")),
            ("mod2_dest", C("osc1_position")),
            ("mod2_amount", N(0.25)),
            ("mod3_source", C("lfo2")),
            ("mod3_dest", C("pan")),
            ("mod3_amount", N(0.7)),
            ("mod4_source", C("macro1")),
            ("mod4_dest", C("osc1_position")),
            ("mod4_amount", N(0.3)),
            ("mod5_source", C("macro2")),
            ("mod5_dest", C("lfo2_depth")),
            ("mod5_amount", N(1.0)),
            ("mod6_source", C("macro3")),
            ("mod6_dest", C("filter1_cutoff")),
            ("mod6_amount", N(-0.25)),
            ("velocity_sens", N(0.85)),
            ("volume", N(-23.6)),
        ],
    },
    Patch {
        name: "FM Keys",
        category: "Keys",
        effects: KEYS,
        description: "A glassy FM electric piano: a sine bent by another, more the harder the key is struck, \
                      dying away to a pure tone. For ballads, city-pop chords and bell-like comping. Macros: Bite \
                      (how hard the tone is bent), Hollow (the bending sine goes an octave up), Tremolo",
        tags: &["keys", "electric piano", "fm", "dx", "glassy", "bell", "chords"],
        macros: ["Bite", "Hollow", "Tremolo", ""],
        params: &[
            ("osc1_position", N(0.0)),
            ("osc1_unison", N(3.0)),
            ("osc1_detune", N(4.0)),
            ("osc1_spread", N(0.2)),
            ("osc1_phase_random", N(0.0)),
            ("osc2_position", N(0.0)),
            ("osc2_phase_random", N(0.0)),
            ("osc1_fm", N(0.03)),
            ("env1_attack", N(0.002)),
            ("env1_decay", N(3.5)),
            ("env1_sustain", N(0.15)),
            ("env1_release", N(0.5)),
            ("env2_attack", N(0.001)),
            ("env2_decay", N(0.9)),
            ("env2_sustain", N(0.1)),
            ("env2_release", N(0.4)),
            ("lfo2_rate", N(4.5)),
            ("lfo2_retrigger", N(0.0)),
            ("lfo2_depth", N(0.0)),
            ("mod1_source", C("env2")),
            ("mod1_dest", C("osc1_fm")),
            ("mod1_amount", N(0.2)),
            ("mod2_source", C("velocity")),
            ("mod2_dest", C("osc1_fm")),
            ("mod2_amount", N(0.16)),
            ("mod3_source", C("lfo2")),
            ("mod3_dest", C("pan")),
            ("mod3_amount", N(0.7)),
            ("mod4_source", C("macro1")),
            ("mod4_dest", C("osc1_fm")),
            ("mod4_amount", N(0.25)),
            ("mod5_source", C("macro2")),
            ("mod5_dest", C("osc2_pitch")),
            ("mod5_amount", N(0.25)),
            ("mod6_source", C("macro3")),
            ("mod6_dest", C("lfo2_depth")),
            ("mod6_amount", N(1.0)),
            ("velocity_sens", N(0.85)),
            ("volume", N(-25.1)),
        ],
    },
];

/// The presets, as the device lists them.
pub fn presets() -> Vec<Preset> {
    PATCHES.iter().map(Patch::preset).collect()
}

/// The patch called `name`.
pub fn named(name: &str) -> Option<&'static Patch> {
    PATCHES.iter().find(|patch| patch.name.eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_patch_sets_parameters_the_synth_has_to_values_they_take() {
        for patch in PATCHES {
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
        }
    }

    #[test]
    fn every_patch_says_what_it_is_and_carries_the_names_of_its_macros() {
        let mut names: Vec<&str> = PATCHES.iter().map(|patch| patch.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), PATCHES.len());
        for patch in PATCHES.iter().skip(1) {
            assert!(["Bass", "Lead", "Pad", "Pluck", "Keys"].contains(&patch.category), "{}", patch.name);
            assert!(patch.description.len() > 120, "{} says too little of itself", patch.name);
            assert!(patch.tags.len() >= 4, "{}", patch.name);
            // What a macro does is said where a model reads it.
            for name in patch.macros.iter().filter(|name| !name.is_empty()) {
                assert!(
                    patch.description.contains(name),
                    "{}: its description does not say what {name} does",
                    patch.name
                );
            }
            let preset = patch.preset();
            let state = State::from_bytes(preset.state.as_deref().unwrap()).unwrap();
            assert_eq!(state.macro_name(0), patch.macros[0]);
            assert_eq!((preset.category.as_str(), preset.tags.len()), (patch.category, patch.tags.len()));
        }
        // Every kind of sound is there.
        for category in ["Bass", "Lead", "Pad", "Pluck", "Keys"] {
            assert!(PATCHES.iter().filter(|patch| patch.category == category).count() >= 2, "{category}");
        }
        assert_eq!(named(" tine piano ").map(|patch| patch.name), Some("Tine Piano"));
    }
}
