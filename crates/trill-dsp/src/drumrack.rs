//! Drum Rack: sixteen pads, each playing its own audio file.
//!
//! The pads sit on MIDI notes 36 to 51, the bottom of the General MIDI drum
//! map that `builtin:drumkit` also follows (36 kick, 38 snare, 42 closed
//! hat, 46 open hat...), so a pattern written for one plays on the other. A
//! note starts its pad's sample and the sample plays out: note lengths do
//! not matter. Every pad has its own level, pan, tuning, start point, decay
//! and choke group, all of them ordinary parameters named after the pad's
//! note (`pad36_level`), so they can be automated like anything else.
//!
//! # State
//!
//! Which file each pad plays lives in the device's `state` as a JSON
//! document (see [`State`]):
//!
//! ```json
//! {
//!   "version": 1,
//!   "pads": [
//!     { "note": 36, "path": "C:/Samples/Kit/kick.wav" },
//!     { "note": 38, "path": "C:/Samples/Kit/snare.wav" }
//!   ]
//! }
//! ```
//!
//! * `version`: the format of the document; this build writes and reads `1`.
//! * `pads`: one entry per pad that has a sample. `note` is the pad
//!   (36..=51) and `path` its audio file, handed as it is to the host's
//!   [`AudioLoader`](trill_core::audio::AudioLoader). Pads without an entry
//!   are empty and silent.
//!
//! Unknown fields are kept and written back, both at the top and in each
//! pad. A device with no state, an unreadable state or files that cannot be
//! loaded is created all the same, with those pads silent;
//! [`sample_problems`](crate::sample_problems) reports them.
//!
//! A rack's files are fixed for the life of the instance. To change one,
//! write a new state into the project and create the device again.

use crate::level::ChannelLevel;
use crate::loading::{SampleRef, SampleSource, StateError, decode, encode};
use crate::params::{ParamDef, ParamValues, boolean, float, int};
use crate::playhead::{LoopSpec, Playhead};
use crate::sample::Sample;
use crate::util::{SILENT_DB, decay_coef, level_gain, pan_gains};
use crate::voice::{FadeOut, VELOCITY_SENS_NAME, velocity_gain};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use trill_core::device::{DeviceProcessor, Event, EventKind, ProcessInfo};

/// The state format this build writes, and the newest it reads.
pub const STATE_VERSION: u32 = 1;
/// MIDI note of the first pad.
pub const FIRST_NOTE: u8 = 36;
pub const PAD_COUNT: usize = 16;
/// The notes that have a pad.
pub const PAD_NOTES: RangeInclusive<u8> = FIRST_NOTE..=FIRST_NOTE + PAD_COUNT as u8 - 1;

/// One pad's entry in the state document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pad {
    /// The pad, by the MIDI note that plays it.
    pub note: u8,
    pub path: String,
    /// Fields written by a build that knows more than this one.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// The drum rack's state document: which file each pad plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// The pads that have a sample, in note order.
    #[serde(default)]
    pub pads: Vec<Pad>,
    /// Fields written by a build that knows more than this one.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for State {
    fn default() -> Self {
        Self { version: STATE_VERSION, pads: Vec::new(), extra: BTreeMap::new() }
    }
}

impl State {
    /// An empty rack.
    pub fn new() -> Self {
        Self::default()
    }

    /// A rack with the given `(note, path)` pads.
    pub fn with_pads<P: Into<String>>(pads: impl IntoIterator<Item = (u8, P)>) -> Result<Self, StateError> {
        let mut state = Self::default();
        for (note, path) in pads {
            state.set_pad(note, path)?;
        }
        Ok(state)
    }

    /// The file on the pad for `note`, if it has one.
    pub fn path(&self, note: u8) -> Option<&str> {
        self.pads.iter().find(|pad| pad.note == note).map(|pad| pad.path.as_str())
    }

    /// Puts the file at `path` on the pad for `note`, replacing what was
    /// there. Fails if no pad plays that note.
    pub fn set_pad(&mut self, note: u8, path: impl Into<String>) -> Result<(), StateError> {
        if !PAD_NOTES.contains(&note) {
            return Err(StateError::Invalid(format!(
                "there is no pad for note {note}: the pads are notes {}..={}",
                PAD_NOTES.start(),
                PAD_NOTES.end()
            )));
        }
        let path = path.into();
        match self.pads.iter_mut().find(|pad| pad.note == note) {
            Some(pad) => pad.path = path,
            None => {
                self.pads.push(Pad { note, path, extra: BTreeMap::new() });
                self.pads.sort_by_key(|pad| pad.note);
            }
        }
        Ok(())
    }

    /// Empties the pad for `note`. Returns whether it held anything.
    pub fn clear_pad(&mut self, note: u8) -> bool {
        let before = self.pads.len();
        self.pads.retain(|pad| pad.note != note);
        self.pads.len() != before
    }

    /// The document to store in [`Device::state`](trill_core::Device).
    pub fn to_bytes(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StateError> {
        decode(bytes, STATE_VERSION)
    }

    /// Reads a device's stored state; a device with none is an empty rack.
    pub fn from_device_state(state: Option<&[u8]>) -> Result<Self, StateError> {
        state.map_or_else(|| Ok(Self::default()), Self::from_bytes)
    }
}

/// The samples a stored state refers to. Entries for notes without a pad
/// are included: the caller reports them as files that are never played.
pub(crate) fn sample_refs(state: Option<&[u8]>) -> Result<Vec<SampleRef>, StateError> {
    let state = State::from_device_state(state)?;
    Ok(state.pads.into_iter().map(|pad| SampleRef { slot: format!("pad {}", pad.note), path: pad.path }).collect())
}

/// The stored state with the files of the pads named in `slots` (`pad 36`)
/// replaced by the ones given; the other pads, and whatever else the
/// document holds of each pad, as they are.
pub(crate) fn relinked(state: Option<&[u8]>, slots: &[(String, String)]) -> Result<Vec<u8>, StateError> {
    let mut state = State::from_device_state(state)?;
    for (slot, path) in slots {
        let note = slot.strip_prefix("pad ").and_then(|note| note.parse::<u8>().ok());
        // A document written by hand may name a pad twice: both entries
        // are the pad's.
        let mut pads = state.pads.iter_mut().filter(|pad| Some(pad.note) == note).peekable();
        if pads.peek().is_none() {
            return Err(StateError::Invalid(format!("the drum rack has no file in '{slot}'")));
        }
        pads.for_each(|pad| pad.path = path.clone());
    }
    Ok(state.to_bytes())
}

/// Creates a drum rack from a stored state, loading its files.
pub(crate) fn create(state: Option<&[u8]>, source: &SampleSource) -> Result<Box<dyn DeviceProcessor>, String> {
    let mut samples = vec![None; PAD_COUNT];
    for pad in State::from_device_state(state).map(|state| state.pads).unwrap_or_default() {
        // If a hand-edited document lists a pad twice, the last entry wins.
        if PAD_NOTES.contains(&pad.note) {
            samples[(pad.note - FIRST_NOTE) as usize] = source.load(&pad.path)?;
        }
    }
    Ok(Box::new(DrumRack::new(samples)))
}

/// Parameters of each pad, in the order they appear in the table.
const PAD_PARAMS: usize = 6;
const LEVEL: usize = 0;
const PAN: usize = 1;
const TUNE: usize = 2;
const START: usize = 3;
const DECAY: usize = 4;
const CHOKE: usize = 5;
/// Parameters of the whole rack, which come first.
const GLOBAL_PARAMS: usize = 2;
const VOLUME: usize = 0;
const VELOCITY_SENS: usize = 1;
/// A parameter of the whole rack that came later, after the pads', so that
/// theirs keep the places they had.
const LEGACY: usize = GLOBAL_PARAMS + PAD_COUNT * PAD_PARAMS;

/// The longest decay; at this setting the envelope is off and the sample
/// plays at its own length.
const FULL_LENGTH: f64 = 10.0;
const CHOKE_GROUPS: i32 = 8;

/// Builds the parameter table: the rack's own parameters, then one block
/// per pad with the pad's note in every id, so ids stay put for good.
macro_rules! rack_params {
    ($( ($note:literal, $name:literal, $choke:literal) ),* $(,)?) => {
        pub const PARAMS: &[ParamDef] = &[
            float(
                "volume",
                "Volume (of a hit at velocity 75 on a pad at level 1: the file as it was recorded at 0; silent at -70)",
                SILENT_DB as f64, 12.0, -8.0, "dB",
            ),
            float("velocity_sens", VELOCITY_SENS_NAME, 0.0, 1.0, 0.5, ""),
            $(
                float(
                    concat!("pad", stringify!($note), "_level"),
                    concat!("Pad ", stringify!($note), " ", $name, " Level"),
                    0.0, 2.0, 1.0, "",
                ),
                float(
                    concat!("pad", stringify!($note), "_pan"),
                    concat!("Pad ", stringify!($note), " ", $name, " Pan"),
                    -1.0, 1.0, 0.0, "",
                ),
                float(
                    concat!("pad", stringify!($note), "_tune"),
                    concat!("Pad ", stringify!($note), " ", $name, " Tune"),
                    -24.0, 24.0, 0.0, "st",
                ),
                float(
                    concat!("pad", stringify!($note), "_start"),
                    concat!("Pad ", stringify!($note), " ", $name, " Start Point (of the file)"),
                    0.0, 100.0, 0.0, "%",
                ),
                float(
                    concat!("pad", stringify!($note), "_decay"),
                    concat!("Pad ", stringify!($note), " ", $name, " Decay (10 = the sample's own length)"),
                    0.01, FULL_LENGTH, FULL_LENGTH, "s",
                ),
                int(
                    concat!("pad", stringify!($note), "_choke"),
                    concat!("Pad ", stringify!($note), " ", $name, " Choke Group (0 = none; pads in a group cut each other)"),
                    0, CHOKE_GROUPS, $choke, "",
                ),
            )*
            boolean(
                "legacy",
                "Legacy (the velocity law of songs saved before format 4: sensitivity only ever turns a hit down \
                 from the volume)",
                false,
            ),
        ];

        /// The General MIDI name of each pad's note, for labelling pads.
        pub const PAD_NAMES: [&str; PAD_COUNT] = [ $( $name ),* ];
    };
}

rack_params![
    (36, "Kick", 0),
    (37, "Rim", 0),
    (38, "Snare", 0),
    (39, "Clap", 0),
    (40, "Snare 2", 0),
    (41, "Low Tom", 0),
    (42, "Closed Hat", 1),
    (43, "Low Tom 2", 0),
    (44, "Pedal Hat", 1),
    (45, "Mid Tom", 0),
    (46, "Open Hat", 1),
    (47, "Mid Tom 2", 0),
    (48, "High Tom", 0),
    (49, "Crash", 0),
    (50, "High Tom 2", 0),
    (51, "Ride", 0),
];

pub const DESCRIPTION: &str = "Drum sampler: 16 pads on MIDI notes 36-51, each playing its own audio file as a \
one-shot (note length is ignored, velocity sets loudness: `volume` is the level of a hit at velocity 75, and \
`velocity_sens` how far harder and softer hits move from it in dB, so a hard hit is louder than the volume). Pads \
follow the General MIDI drum map, like \
builtin:drumkit: 36 kick, 37 rim, 38 snare, 39 clap, 40 snare 2, 41/43 low toms, 42 closed hat, 44 pedal hat, \
45/47 mid toms, 46 open hat, 48/50 high toms, 49 crash, 51 ride - but any sound can go on any pad. Each pad has \
level, pan, tune, start point, decay and a choke group (by default the hats 42/44/46 cut each other), as \
parameters named padNN_level, padNN_pan and so on. The audio files are not parameters: they are the device's \
samples, one per pad, stored in the device state. Pads without one are silent.";

/// Notes that can sound at once: one per pad, plus as many again that are
/// fading out after their pad was struck again or choked.
const VOICES: usize = PAD_COUNT * 2;
/// Samples between control-rate updates (pad parameters).
const CONTROL_BLOCK: usize = 16;
/// How fast a pad's previous hit makes way when the pad is struck again.
/// Long enough not to click on a low kick, short enough not to flam.
const RETRIGGER_SECONDS: f32 = 0.012;
/// How fast a choked pad is silenced: a hand closing on a cymbal.
const CHOKE_SECONDS: f32 = 0.008;
/// A decaying voice this far down (-80 dB) is finished.
const SILENT: f32 = 1e-4;

#[derive(Copy, Clone, Default)]
struct PadSettings {
    level: f32,
    pan_left: f32,
    pan_right: f32,
    /// Frames of the file per output frame.
    rate: f64,
    /// Fraction of the file to skip.
    start: f64,
    /// Per-frame multiplier of the decay envelope; 1 when it is off.
    decay: f32,
    choke: u8,
}

#[derive(Copy, Clone)]
struct Voice {
    active: bool,
    pad: usize,
    /// `None` until the voice has played its first hit.
    head: Option<Playhead>,
    envelope: f32,
    velocity_gain: f32,
    age: u64,
    fade: FadeOut,
}

impl Voice {
    const fn idle() -> Self {
        Self { active: false, pad: 0, head: None, envelope: 0.0, velocity_gain: 0.0, age: 0, fade: FadeOut::idle() }
    }

    /// `levels` is the channel's level at each frame.
    fn render(
        &mut self,
        pad: &PadSettings,
        sample: &Sample,
        (volume, levels): (f32, &[f32]),
        out_left: &mut [f32],
        out_right: &mut [f32],
    ) {
        let Some(head) = self.head.as_mut() else {
            self.active = false;
            return;
        };
        head.set_rate(sample, pad.rate);
        let gain = self.velocity_gain * pad.level * volume;
        let (gain_left, gain_right) = (gain * pad.pan_left, gain * pad.pan_right);
        for (frame, (out_left, out_right)) in out_left.iter_mut().zip(out_right.iter_mut()).enumerate() {
            let (left, right) = head.next(sample, &LoopSpec::OFF);
            let level = self.envelope * self.fade.tick() * levels[frame];
            self.envelope *= pad.decay;
            *out_left += left * level * gain_left;
            *out_right += right * level * gain_right;
        }
        if head.finished() || self.fade.finished() || self.envelope < SILENT {
            self.active = false;
        }
    }
}

pub struct DrumRack {
    params: ParamValues,
    /// One per pad; `None` for an empty pad.
    samples: Vec<Option<Arc<Sample>>>,
    pads: [PadSettings; PAD_COUNT],
    volume: f32,
    velocity_sens: f32,
    /// The velocity law of songs saved before format 4.
    legacy: bool,
    sample_rate: f64,
    voices: [Voice; VOICES],
    clock: u64,
    /// Channel volume and expression.
    level: ChannelLevel,
}

impl DrumRack {
    /// A rack with `samples` on its pads, in note order from
    /// [`FIRST_NOTE`]. Missing and `None` entries are empty pads.
    pub fn new(mut samples: Vec<Option<Arc<Sample>>>) -> Self {
        samples.resize(PAD_COUNT, None);
        for sample in &mut samples {
            // A file with no frames has nothing to play.
            *sample = sample.take().filter(|sample| sample.frames() > 0);
        }
        Self {
            params: ParamValues::new(PARAMS),
            samples,
            pads: [PadSettings::default(); PAD_COUNT],
            volume: 1.0,
            velocity_sens: 1.0,
            legacy: false,
            sample_rate: 48_000.0,
            voices: [Voice::idle(); VOICES],
            clock: 0,
            level: ChannelLevel::new(),
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        self.volume = level_gain(p.get(VOLUME));
        self.velocity_sens = p.get(VELOCITY_SENS);
        self.legacy = p.on(LEGACY);
        for (index, (pad, sample)) in self.pads.iter_mut().zip(&self.samples).enumerate() {
            let base = GLOBAL_PARAMS + index * PAD_PARAMS;
            pad.level = p.get(base + LEVEL);
            (pad.pan_left, pad.pan_right) = pan_gains(p.get(base + PAN));
            let file_rate = sample.as_ref().map_or(self.sample_rate, |sample| sample.sample_rate());
            pad.rate = file_rate / self.sample_rate * 2f64.powf(p.get(base + TUNE) as f64 / 12.0);
            pad.start = p.get(base + START) as f64 * 0.01;
            let decay = p.get(base + DECAY);
            pad.decay = if decay as f64 >= FULL_LENGTH { 1.0 } else { decay_coef(decay, self.sample_rate as f32) };
            pad.choke = p.index(base + CHOKE) as u8;
        }
        self.params.changed = false;
    }

    fn trigger(&mut self, pitch: u8, velocity: f32) {
        let Some(index) = pitch.checked_sub(FIRST_NOTE).map(usize::from).filter(|index| *index < PAD_COUNT) else {
            return;
        };
        let Some(sample) = self.samples[index].as_deref() else { return };
        let pad = self.pads[index];
        let sample_rate = self.sample_rate as f32;
        self.clock += 1;

        for voice in self.voices.iter_mut().filter(|voice| voice.active) {
            if voice.pad == index {
                voice.fade.start(RETRIGGER_SECONDS, sample_rate);
            } else if pad.choke != 0 && self.pads[voice.pad].choke == pad.choke {
                voice.fade.start(CHOKE_SECONDS, sample_rate);
            }
        }

        // A free voice, or whichever fading one is nearest to silence. With
        // twice as many voices as pads and one sounding hit per pad, one of
        // the two always exists; the oldest voice is only a last resort.
        let voices = &self.voices;
        let free = voices.iter().position(|voice| !voice.active);
        let fading = || {
            let fading = voices.iter().enumerate().filter(|(_, voice)| voice.fade.fading());
            fading.min_by(|a, b| a.1.fade.gain().total_cmp(&b.1.fade.gain())).map(|(index, _)| index)
        };
        let oldest = || voices.iter().enumerate().min_by_key(|(_, voice)| voice.age).map_or(0, |(index, _)| index);
        let slot = free.or_else(fading).unwrap_or_else(oldest);

        let frames = sample.frames() as f64;
        let sens = self.velocity_sens;
        self.voices[slot] = Voice {
            active: true,
            pad: index,
            head: Some(Playhead::new(sample, pad.start * frames, frames, false, pad.rate, self.sample_rate)),
            envelope: 1.0,
            velocity_gain: if self.legacy {
                // The loudness curve of the synthesized kit.
                1.0 - sens + sens * velocity * velocity.sqrt()
            } else {
                velocity_gain(sens, velocity)
            },
            age: self.clock,
            fade: FadeOut::idle(),
        };
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let mut pos = 0;
        while pos < left.len() {
            let len = (left.len() - pos).min(CONTROL_BLOCK);
            let mut levels = [1.0f32; CONTROL_BLOCK];
            self.level.fill(&mut levels[..len], self.sample_rate as f32);
            for voice in self.voices.iter_mut().filter(|voice| voice.active) {
                match self.samples[voice.pad].as_deref() {
                    Some(sample) => voice.render(
                        &self.pads[voice.pad],
                        sample,
                        (self.volume, &levels[..len]),
                        &mut left[pos..pos + len],
                        &mut right[pos..pos + len],
                    ),
                    None => voice.active = false,
                }
            }
            pos += len;
        }
    }

    /// Of the notes only note-ons matter: a drum rings for as long as its
    /// sample does, whatever the note length, and through a stop of the
    /// transport. Volume and expression set the level of the whole rack.
    fn handle(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn { pitch, velocity } => self.trigger(pitch, velocity),
            EventKind::Control { controller, value } => {
                let sounding = self.voices.iter().any(|voice| voice.active);
                self.level.control(controller, value, sounding);
            }
            _ => {}
        }
    }
}

impl DeviceProcessor for DrumRack {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate;
        self.params.changed = true;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, _info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        if self.params.changed {
            self.refresh();
        }
        let len = left.len();
        let mut pos = 0;
        let mut next_event = 0;
        while pos < len {
            while next_event < events.len() && events[next_event].offset as usize <= pos {
                self.handle(events[next_event].kind);
                next_event += 1;
            }
            let end = events.get(next_event).map_or(len, |e| (e.offset as usize).min(len));
            self.render(&mut left[pos..end], &mut right[pos..end]);
            pos = end;
        }
        for event in &events[next_event..] {
            self.handle(event.kind);
        }
    }

    fn reset(&mut self) {
        self.level.settle();
        for voice in &mut self.voices {
            voice.active = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use crate::util::db_to_gain;
    use trill_core::audio::AudioBuffer;

    const SR: f64 = 48_000.0;

    fn sample(left: Vec<f32>, sample_rate: f64) -> Option<Arc<Sample>> {
        Some(Arc::new(Sample::new(&AudioBuffer { sample_rate, right: left.clone(), left })))
    }

    /// A rack whose pads each hold a constant of a different value (pad 36
    /// is 0.10, pad 37 is 0.11...), so the output says which pads sound.
    fn constant_rack(frames: usize, params: &[(&str, f64)]) -> DrumRack {
        let samples = (0..PAD_COUNT).map(|pad| sample(vec![0.10 + pad as f32 * 0.01; frames], SR)).collect();
        rack(samples, params)
    }

    /// A rack that plays every hit as its file is, whatever the velocity,
    /// but for what `params` say.
    fn rack(samples: Vec<Option<Arc<Sample>>>, params: &[(&str, f64)]) -> DrumRack {
        let mut device = DrumRack::new(samples);
        device.prepare(SR, 512);
        for (id, value) in [("volume", 0.0), ("velocity_sens", 0.0)].iter().chain(params) {
            let index = PARAMS.iter().position(|def| def.id == *id).unwrap_or_else(|| panic!("no parameter '{id}'"));
            device.set_param(index, *value);
        }
        device
    }

    const HIT: fn(u8) -> EventKind = |pitch| EventKind::NoteOn { pitch, velocity: 1.0 };

    #[test]
    fn the_parameter_table_has_a_block_per_pad_with_stable_ids() {
        assert_eq!(PARAMS.len(), GLOBAL_PARAMS + PAD_COUNT * PAD_PARAMS + 1);
        assert_eq!(PARAMS[LEGACY].id, "legacy");
        assert_eq!(PARAMS[VOLUME].id, "volume");
        assert_eq!(PARAMS[VELOCITY_SENS].id, "velocity_sens");
        for (pad, note) in PAD_NOTES.enumerate() {
            let base = GLOBAL_PARAMS + pad * PAD_PARAMS;
            for (offset, suffix) in
                [(LEVEL, "level"), (PAN, "pan"), (TUNE, "tune"), (START, "start"), (DECAY, "decay"), (CHOKE, "choke")]
            {
                assert_eq!(PARAMS[base + offset].id, format!("pad{note}_{suffix}"));
            }
            assert!(PARAMS[base + LEVEL].name.contains(PAD_NAMES[pad]));
        }
        assert_eq!(
            (PAD_NAMES[0], PAD_NAMES[2], PAD_NAMES[6], PAD_NAMES[10]),
            ("Kick", "Snare", "Closed Hat", "Open Hat")
        );
    }

    #[test]
    fn state_round_trips_and_edits() {
        let mut state = State::with_pads([(38, "snare.wav"), (36, "kick.wav")]).unwrap();
        assert_eq!(state.pads.iter().map(|pad| pad.note).collect::<Vec<_>>(), [36, 38], "kept in note order");
        assert_eq!(state.path(36), Some("kick.wav"));
        assert_eq!(state.path(37), None);
        state.set_pad(36, "other kick.wav").unwrap();
        assert_eq!((state.path(36), state.pads.len()), (Some("other kick.wav"), 2));
        assert!(state.clear_pad(38) && !state.clear_pad(38));
        assert!(matches!(state.set_pad(35, "x.wav"), Err(StateError::Invalid(_))));
        assert!(matches!(state.set_pad(52, "x.wav"), Err(StateError::Invalid(_))));

        let bytes = state.to_bytes();
        assert_eq!(State::from_bytes(&bytes).unwrap(), state);
        assert_eq!(State::from_device_state(None).unwrap(), State::new());
        assert_eq!(
            sample_refs(Some(&bytes)).unwrap(),
            [SampleRef { slot: "pad 36".into(), path: "other kick.wav".into() }]
        );

        let newer = br#"{"version": 1, "kit": "909", "pads": [{"note": 40, "path": "s.wav", "color": 3}]}"#;
        let state = State::from_bytes(newer).unwrap();
        let rewritten: serde_json::Value = serde_json::from_slice(&state.to_bytes()).unwrap();
        assert_eq!(rewritten["kit"], "909");
        assert_eq!(rewritten["pads"][0]["color"], 3);
        assert!(matches!(State::from_bytes(br#"{"version": 7}"#), Err(StateError::TooNew { found: 7, supported: 1 })));
        assert!(State::from_bytes(br#"{"version": 1}"#).unwrap().pads.is_empty());
    }

    #[test]
    fn each_note_plays_its_own_pad_and_no_other() {
        for (pad, note) in PAD_NOTES.enumerate() {
            let mut device = constant_rack(4_800, &[]);
            let (left, right) = run(&mut device, SR, 256, 6_000, &[(100, HIT(note))]);
            let want = 0.10 + pad as f32 * 0.01;
            assert_eq!(peak(&left[..100]), 0.0);
            assert!((left[1_000] - want).abs() < 1e-4, "note {note}: {} (wanted {want})", left[1_000]);
            assert!(left == right || PARAMS[GLOBAL_PARAMS + pad * PAD_PARAMS + PAN].default != 0.0);
            assert_eq!(peak(&left[4_901..]), 0.0, "note {note}: one-shot ends with the file");
        }
        // Notes without a pad, and pads without a sample, do nothing.
        let mut samples = vec![None; PAD_COUNT];
        samples[0] = sample(vec![0.5; 1_000], SR);
        let mut device = rack(samples, &[]);
        let events: Vec<(usize, EventKind)> = [0u8, 35, 37, 52, 127].iter().map(|note| (10, HIT(*note))).collect();
        let (left, _) = run(&mut device, SR, 256, 2_000, &events);
        assert_eq!(peak(&left), 0.0);
    }

    #[test]
    fn note_length_is_ignored_and_velocity_sets_the_level() {
        let mut device = constant_rack(9_600, &[]);
        let events = [(0, HIT(36)), (480, EventKind::NoteOff { pitch: 36 }), (960, EventKind::AllNotesOff)];
        let (left, _) = run(&mut device, SR, 256, 12_000, &events);
        assert!((left[9_000] - 0.10).abs() < 1e-4, "the hit rings on after the key and the transport stop");

        // What a hit of a velocity plays at, against one at velocity 75.
        let level = |params: &[(&str, f64)], velocity: u8| {
            let hit = |velocity: u8| {
                let mut device = constant_rack(9_600, params);
                let hit = EventKind::NoteOn { pitch: 36, velocity: f32::from(velocity) / 127.0 };
                run(&mut device, SR, 256, 2_000, &[(0, hit)]).0[500]
            };
            20.0 * (hit(velocity) / hit(75)).log10()
        };
        // Half of 0.4 dB a step where nothing is said: a ghost note 9 dB
        // under a hit at 75, an accent 10 dB over it.
        assert_eq!((PARAMS[VELOCITY_SENS].default, PARAMS[VOLUME].default), (0.5, -8.0));
        let half = [("velocity_sens", 0.5)];
        for (velocity, wanted) in [(30, -9.0), (46, -5.8), (75, 0.0), (100, 5.0), (127, 10.4)] {
            assert!((level(&half, velocity) - wanted).abs() < 0.02, "velocity {velocity}: {}", level(&half, velocity));
        }
        assert!((level(&[("velocity_sens", 1.0)], 100) - 10.0).abs() < 0.02);
        assert!(level(&[("velocity_sens", 0.0)], 20).abs() < 0.01, "no sensitivity: every hit at the volume");
        // At velocity 75 and the volume at 0 a pad plays its file as it is.
        let mut device = constant_rack(9_600, &half);
        let (plain, _) =
            run(&mut device, SR, 256, 2_000, &[(0, EventKind::NoteOn { pitch: 36, velocity: 75.0 / 127.0 })]);
        assert!((plain[500] - 0.10).abs() < 1e-5, "{}", plain[500]);
        let mut device = constant_rack(9_600, &[("volume", -70.0)]);
        assert_eq!(peak(&run(&mut device, SR, 256, 2_000, &[(0, HIT(36))]).0), 0.0, "the lowest volume is silence");

        // A song saved before keeps its law: velocity 0.25 at 0.25^1.5.
        let old = [("legacy", 1.0), ("volume", 0.0), ("velocity_sens", 1.0)];
        let mut device = constant_rack(9_600, &old);
        let (soft, _) = run(&mut device, SR, 256, 2_000, &[(0, EventKind::NoteOn { pitch: 36, velocity: 0.25 })]);
        assert!((soft[500] - 0.10 * 0.125).abs() < 1e-4, "velocity 0.25 plays at 0.25^1.5: {}", soft[500]);
        let mut device = constant_rack(9_600, &[("legacy", 1.0), ("volume", 0.0), ("velocity_sens", 0.0)]);
        let (flat, _) = run(&mut device, SR, 256, 2_000, &[(0, EventKind::NoteOn { pitch: 36, velocity: 0.25 })]);
        assert!((flat[500] - 0.10).abs() < 1e-4);
    }

    #[test]
    fn closed_hat_chokes_open_hat_and_groups_can_be_changed() {
        let hits = [(0, HIT(46)), (2_400, HIT(42))];
        let mut device = constant_rack(48_000, &[]);
        let (left, _) = run(&mut device, SR, 256, 9_600, &hits);
        let (open, closed) = (0.20, 0.16);
        assert!((left[2_000] - open).abs() < 1e-4);
        assert!((left[4_800] - closed).abs() < 1e-4, "only the closed hat is left: {}", left[4_800]);
        // The choke is a fade of a few milliseconds, not a cut.
        assert!(largest_step(&left[2_300..3_400]) < closed + 0.01, "the open hat was cut, not faded");
        assert!(left[2_500] > closed + 0.05, "it is still fading 2 ms later");

        // Out of the group, the open hat rings on underneath.
        let mut device = constant_rack(48_000, &[("pad46_choke", 0.0)]);
        let (left, _) = run(&mut device, SR, 256, 9_600, &hits);
        assert!((left[4_800] - (open + closed)).abs() < 1e-4);

        // Any pads can be put in a group of their own.
        let mut device = constant_rack(48_000, &[("pad36_choke", 5.0), ("pad49_choke", 5.0)]);
        let (left, _) = run(&mut device, SR, 256, 9_600, &[(0, HIT(49)), (2_400, HIT(36))]);
        assert!((left[4_800] - 0.10).abs() < 1e-4, "the kick choked the crash: {}", left[4_800]);
    }

    #[test]
    fn per_pad_parameters_apply_to_their_pad_only() {
        let both = [(0, HIT(36)), (0, HIT(38))];
        let play = |params: &[(&str, f64)]| {
            let mut device = constant_rack(48_000, params);
            run(&mut device, SR, 256, 24_000, &both)
        };
        let (kick, snare) = (0.10f32, 0.12f32);
        let (left, right) = play(&[]);
        assert!((left[1_000] - (kick + snare)).abs() < 1e-4 && left == right);

        let (left, _) = play(&[("pad36_level", 0.5)]);
        assert!((left[1_000] - (kick * 0.5 + snare)).abs() < 1e-4);

        let (left, right) = play(&[("pad38_pan", -1.0)]);
        assert!((left[1_000] - (kick + snare * std::f32::consts::SQRT_2)).abs() < 1e-4);
        assert!((right[1_000] - kick).abs() < 1e-4);

        let (left, _) = play(&[("pad36_decay", 0.1)]);
        assert!((left[10] - (kick + snare)).abs() < 2e-3, "the decay starts from full level");
        // 60 dB down after 0.1 s: half way there after 0.05 s.
        assert!((left[2_400] - (kick * 10f32.powf(-1.5) + snare)).abs() < 1e-3, "{}", left[2_400]);
        assert!((left[9_600] - snare).abs() < 1e-4);

        let (left, _) = play(&[("volume", -6.0)]);
        assert!((left[1_000] - (kick + snare) * db_to_gain(-6.0)).abs() < 1e-4);
    }

    #[test]
    fn tune_start_and_sample_rate_are_respected() {
        // A 440 Hz file recorded at 44.1 kHz, on a 48 kHz engine.
        let file = sine(440.0, 44_100.0, 44_100);
        let pitch_of = |params: &[(&str, f64)]| {
            let mut samples = vec![None; PAD_COUNT];
            samples[2] = sample(file.clone(), 44_100.0);
            let mut device = rack(samples, params);
            let (left, _) = run(&mut device, SR, 256, 12_000, &[(0, HIT(38))]);
            zero_crossing_hz(&left[500..11_500], SR)
        };
        assert!((pitch_of(&[]) - 440.0).abs() < 0.2, "{}", pitch_of(&[]));
        assert!((pitch_of(&[("pad38_tune", 12.0)]) - 880.0).abs() < 0.4);
        assert!((pitch_of(&[("pad38_tune", -7.0)]) - 293.665).abs() < 0.2);

        // Start point: skip the first half of a file that is silent there.
        let mut half: Vec<f32> = vec![0.0; 4_800];
        half.extend(vec![0.5; 4_800]);
        let mut samples = vec![None; PAD_COUNT];
        samples[0] = sample(half, SR);
        let mut device = rack(samples, &[("pad36_start", 50.0)]);
        let (left, _) = run(&mut device, SR, 256, 6_000, &[(0, HIT(36))]);
        assert!((left[200] - 0.5).abs() < 1e-3 && peak(&left[4_801..]) == 0.0);
    }

    #[test]
    fn hammering_a_pad_never_clicks_or_runs_out_of_voices() {
        // A loud low tone on every pad, struck far faster than it decays.
        let tone: Vec<f32> = sine(80.0, SR, 96_000).iter().map(|s| s * 0.5).collect();
        let samples = (0..PAD_COUNT).map(|_| sample(tone.clone(), SR)).collect();
        let mut device = rack(samples, &[]);
        let events: Vec<(usize, EventKind)> =
            (0..400).map(|i| (i * 331 + 7, HIT(FIRST_NOTE + ((i * 7) % PAD_COUNT) as u8))).collect();
        let (left, _) = run(&mut device, SR, 256, 140_000, &events);
        assert!(left.iter().all(|s| s.is_finite()));
        // Sixteen pads at half scale, starting at a zero crossing: the
        // steepest the sum can honestly be is 16 tones' worth of slope, and
        // a retrigger adds a 12 ms fade of one of them.
        let natural = (std::f64::consts::TAU * 80.0 / SR) as f32 * 0.5;
        assert!(largest_step(&left) < natural * 16.0 + 0.05, "step {}", largest_step(&left));
        assert!(device.voices.iter().filter(|voice| voice.active && !voice.fade.fading()).count() <= PAD_COUNT);
        device.reset();
        let (left, _) = run(&mut device, SR, 256, 500, &[]);
        assert_eq!(peak(&left), 0.0);
    }

    #[test]
    fn expression_and_volume_set_the_level_of_the_rack() {
        let level = |events: &[(usize, EventKind)]| {
            let mut rack = constant_rack(48_000, &[]);
            peak(&run(&mut rack, SR, 256, 4_800, events).0)
        };
        let control = |controller: u8, value: f32| EventKind::Control { controller, value };
        let full = level(&[(1_000, HIT(36))]);
        assert!(full > 0.05);
        assert!((level(&[(0, control(11, 0.5)), (1_000, HIT(36))]) / full - 0.25).abs() < 1e-3);
        assert!((level(&[(0, control(7, 0.5)), (1_000, HIT(36))]) / full - 0.25).abs() < 1e-3);
        assert_eq!(level(&[(0, control(11, 0.0)), (1_000, HIT(36))]), 0.0);
        assert_eq!(level(&[(0, control(11, 1.0)), (1_000, HIT(36))]), full);
    }
}
