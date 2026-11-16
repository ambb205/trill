//! Sampler: plays one audio file across the keyboard.
//!
//! The file is heard unchanged on the root note and repitched on every other
//! key, by reading it faster or slower through a band-limited interpolator
//! (see [`crate::sample`]). Around that sit the usual sampler controls: start
//! and end points, forward and ping-pong loops with a cross-fade, reverse,
//! one-shot or gated playback, an amplitude envelope and a multimode filter
//! with its own envelope.
//!
//! # State
//!
//! Which file to play is not a number, so it is not a parameter. It lives in
//! the device's `state` as a JSON document (see [`State`]):
//!
//! ```json
//! {
//!   "version": 1,
//!   "path": "C:/Samples/Keys/rhodes_c3.wav"
//! }
//! ```
//!
//! * `version`: the format of the document; this build writes and reads `1`.
//! * `path`: the audio file, handed as it is to the host's
//!   [`AudioLoader`](trill_core::audio::AudioLoader). Leave it out for a
//!   sampler with nothing loaded.
//!
//! Unknown fields are kept and written back. A device with no state, an
//! unreadable state or a file that cannot be loaded is created all the same
//! and plays silence; [`sample_problems`](crate::sample_problems) reports it.
//!
//! A sampler's file is fixed for the life of the instance. To change it,
//! write a new state into the project and create the device again.

use crate::define_params;
use crate::loading::{SampleRef, SampleSource, StateError, decode, encode};
use crate::params::{ParamValues, PresetDef, boolean, choice, float, int};
use crate::playhead::{LoopMode, LoopSpec, Playhead};
use crate::sample::Sample;
use crate::util::{db_to_gain, pan_gains};
use crate::voice::{EnvShape, Envelope, FadeOut, FilterMode, Stage, VoiceFilter, resonance_to_q};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use trill_core::device::{DeviceProcessor, Event, EventKind, ProcessInfo};

/// The state format this build writes, and the newest it reads.
pub const STATE_VERSION: u32 = 1;

/// The sampler's state document: which file it plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// The audio file; `None` for an empty sampler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Fields written by a build that knows more than this one.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for State {
    fn default() -> Self {
        Self { version: STATE_VERSION, path: None, extra: BTreeMap::new() }
    }
}

impl State {
    /// A sampler playing the file at `path`.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: Some(path.into()), ..Self::default() }
    }

    /// The document to store in [`Device::state`](trill_core::Device).
    pub fn to_bytes(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StateError> {
        decode(bytes, STATE_VERSION)
    }

    /// Reads a device's stored state; a device with none is an empty sampler.
    pub fn from_device_state(state: Option<&[u8]>) -> Result<Self, StateError> {
        state.map_or_else(|| Ok(Self::default()), Self::from_bytes)
    }
}

/// The samples a stored state refers to.
pub(crate) fn sample_refs(state: Option<&[u8]>) -> Result<Vec<SampleRef>, StateError> {
    let state = State::from_device_state(state)?;
    Ok(state.path.into_iter().map(|path| SampleRef { slot: "sample".to_string(), path }).collect())
}

/// Creates a sampler from a stored state, loading its file.
pub(crate) fn create(state: Option<&[u8]>, source: &SampleSource) -> Result<Box<dyn DeviceProcessor>, String> {
    let sample = match State::from_device_state(state) {
        Ok(State { path: Some(path), .. }) => source.load(&path)?,
        _ => None,
    };
    Ok(Box::new(Sampler::new(sample)))
}

/// The most notes that can sound at once.
pub const MAX_VOICES: usize = 32;
/// Extra voices that only ever hold notes fading out after being stolen, so
/// that a stolen note and the note that took its place can overlap briefly.
const SPARE_VOICES: usize = 16;
/// Samples between control-rate updates (pitch, filter cutoff).
const CONTROL_BLOCK: usize = 16;
/// How quickly a stolen voice gets out of the way.
const STEAL_SECONDS: f32 = 0.005;
/// Attack times below this start the envelope fully open, so that a drum
/// hit loaded into the sampler keeps its first sample.
const INSTANT_ATTACK: f32 = 0.0005;

const LOOP_MODES: &[&str] = &["off", "forward", "ping_pong"];
/// "off", then the modes of [`crate::voice::FILTER_MODES`] in order.
const FILTERS: &[&str] = &["off", "lowpass", "lowpass24", "highpass", "bandpass"];

define_params!(P, PARAMS, [
    Root => int("root", "Root Note (the key that plays the file unchanged; 60 = C4)", 0, 127, 60, ""),
    Transpose => int("transpose", "Transpose", -48, 48, 0, "st"),
    Fine => float("fine", "Fine Tune", -100.0, 100.0, 0.0, "cents"),
    Start => float("start", "Start Point (of the file)", 0.0, 100.0, 0.0, "%"),
    End => float("end", "End Point (of the file)", 0.0, 100.0, 100.0, "%"),
    Reverse => boolean("reverse", "Reverse", false),
    OneShot => boolean("one_shot", "One-Shot (play to the end whatever the note length; no looping)", false),
    LoopMode => choice("loop_mode", "Loop Mode", LOOP_MODES, 0),
    LoopStart => float("loop_start", "Loop Start (of the file)", 0.0, 100.0, 0.0, "%"),
    LoopEnd => float("loop_end", "Loop End (of the file)", 0.0, 100.0, 100.0, "%"),
    LoopCrossfade => float("loop_crossfade", "Loop Crossfade (forward loops)", 0.0, 2000.0, 0.0, "ms"),
    Attack => float("attack", "Amp Attack", 0.0, 10.0, 0.0, "s"),
    Decay => float("decay", "Amp Decay", 0.005, 20.0, 0.5, "s"),
    Sustain => float("sustain", "Amp Sustain", 0.0, 1.0, 1.0, ""),
    Release => float("release", "Amp Release", 0.005, 20.0, 0.05, "s"),
    FilterType => choice("filter_type", "Filter Type", FILTERS, 0),
    Cutoff => float("cutoff", "Filter Cutoff", 20.0, 20000.0, 20000.0, "Hz"),
    Resonance => float("resonance", "Filter Resonance", 0.0, 1.0, 0.1, ""),
    FilterEnv => float("filter_env", "Filter Envelope Amount", -8.0, 8.0, 0.0, "oct"),
    KeyTrack => float("key_track", "Filter Key Tracking", 0.0, 1.0, 0.0, ""),
    FilterAttack => float("filter_attack", "Filter Attack", 0.0, 10.0, 0.0, "s"),
    FilterDecay => float("filter_decay", "Filter Decay", 0.005, 20.0, 0.5, "s"),
    FilterSustain => float("filter_sustain", "Filter Sustain", 0.0, 1.0, 0.5, ""),
    FilterRelease => float("filter_release", "Filter Release", 0.005, 20.0, 0.3, "s"),
    VelocitySens => float("velocity_sens", "Velocity Sensitivity", 0.0, 1.0, 0.6, ""),
    Voices => int("voices", "Voices", 1, MAX_VOICES as i32, 16, ""),
    Volume => float("volume", "Volume", -48.0, 12.0, -6.0, "dB"),
    Pan => float("pan", "Pan", -1.0, 1.0, 0.0, ""),
]);

pub const DESCRIPTION: &str = "Plays one audio file across the keyboard: the root note plays it as recorded and \
other keys repitch it, with up to 32 voices. Start/end points, forward and ping-pong loops with crossfade, reverse, \
one-shot or gated playback, amp ADSR, multimode filter with envelope. For melodic one-shots, sustained instruments \
(set a loop), vocal chops and textures; for a kit of drum hits use builtin:drumrack. The audio file is not a \
parameter: it is the device's sample, stored in the device state. Without one the sampler is silent.";

pub const PRESETS: &[PresetDef] = &[
    PresetDef { name: "default", description: "Plays the whole file while the key is held", params: &[] },
    PresetDef {
        name: "one_shot",
        description: "Each key plays the file to its end: hits, stabs, chops",
        params: &[("one_shot", 1.0), ("release", 0.02), ("velocity_sens", 0.8)],
    },
    PresetDef {
        name: "sustained",
        description: "Loops the middle of the file while the key is held: a held note from a short recording",
        params: &[
            ("loop_mode", 1.0),
            ("loop_start", 30.0),
            ("loop_end", 90.0),
            ("loop_crossfade", 150.0),
            ("attack", 0.005),
            ("release", 0.4),
        ],
    },
    PresetDef {
        name: "pad",
        description: "Slow, soft, looped back and forth: turns any sound into a texture",
        params: &[
            ("loop_mode", 2.0),
            ("loop_start", 20.0),
            ("loop_end", 95.0),
            ("attack", 0.8),
            ("release", 2.0),
            ("filter_type", 2.0),
            ("cutoff", 3000.0),
            ("velocity_sens", 0.2),
        ],
    },
    PresetDef {
        name: "reverse",
        description: "The file backwards, played to its start",
        params: &[("reverse", 1.0), ("one_shot", 1.0), ("release", 0.02)],
    },
];

/// Everything derived from the parameters and the sample, refreshed when a
/// parameter changes.
#[derive(Clone, Default)]
struct Derived {
    /// The region new notes play, in frames of the file.
    start: f64,
    end: f64,
    reverse: bool,
    one_shot: bool,
    /// Already off in one-shot mode.
    looping: LoopSpec,
    /// Playback rate of MIDI note 0: every key's rate is this times two to
    /// the power of its note number over twelve.
    tune: f64,
    amp: EnvShape,
    instant_attack: bool,
    filt: EnvShape,
    instant_filter_attack: bool,
    filter: Option<FilterMode>,
    cutoff: f32,
    q: f32,
    filter_env: f32,
    key_track: f32,
    velocity_sens: f32,
    gain_left: f32,
    gain_right: f32,
    voices: usize,
}

#[derive(Clone)]
struct Voice {
    active: bool,
    /// The key is down. Never set for one-shot notes, which no key-up ends.
    held: bool,
    pitch: u8,
    /// Two to the power of the note number over twelve.
    pitch_ratio: f64,
    velocity_gain: f32,
    age: u64,
    amp: Envelope,
    filt: Envelope,
    filter: VoiceFilter,
    /// `None` until the voice has played its first note.
    head: Option<Playhead>,
    fade: FadeOut,
}

impl Voice {
    fn new() -> Self {
        Self {
            active: false,
            held: false,
            pitch: 0,
            pitch_ratio: 1.0,
            velocity_gain: 0.0,
            age: 0,
            amp: Envelope::idle(),
            filt: Envelope::idle(),
            filter: VoiceFilter::default(),
            head: None,
            fade: FadeOut::idle(),
        }
    }

    fn kill(&mut self) {
        self.active = false;
        self.held = false;
        self.amp = Envelope::idle();
        self.filt = Envelope::idle();
    }

    fn release(&mut self) {
        self.held = false;
        self.amp.release();
        self.filt.release();
    }

    /// Still sounding as a note in its own right, rather than on its way out
    /// after being stolen.
    fn playing(&self) -> bool {
        self.active && !self.fade.fading()
    }

    fn render(&mut self, d: &Derived, sample: &Sample, sample_rate: f32, out_left: &mut [f32], out_right: &mut [f32]) {
        let Some(head) = self.head.as_mut() else {
            self.active = false;
            return;
        };
        head.set_rate(sample, self.pitch_ratio * d.tune);
        if d.filter.is_some() {
            let octaves = d.filter_env * self.filt.level + d.key_track * (self.pitch as f32 - 60.0) / 12.0;
            self.filter.set(d.cutoff * 2f32.powf(octaves), d.q, sample_rate);
        }

        for (out_left, out_right) in out_left.iter_mut().zip(out_right.iter_mut()) {
            let amp = self.amp.tick(&d.amp);
            self.filt.tick(&d.filt);
            let (mut left, mut right) = head.next(sample, &d.looping);
            if let Some(mode) = d.filter {
                (left, right) = self.filter.tick(mode, left, right);
            }
            let level = amp * self.velocity_gain * self.fade.tick();
            *out_left += left * level * d.gain_left;
            *out_right += right * level * d.gain_right;
        }

        if self.amp.stage == Stage::Idle || head.finished() || self.fade.finished() {
            self.active = false;
        }
    }
}

pub struct Sampler {
    params: ParamValues,
    derived: Derived,
    sample: Option<Arc<Sample>>,
    sample_rate: f64,
    voices: Vec<Voice>,
    clock: u64,
}

impl Sampler {
    /// A sampler playing `sample`, or a silent one.
    pub fn new(sample: Option<Arc<Sample>>) -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            derived: Derived::default(),
            // A file with no frames has nothing to play or to loop.
            sample: sample.filter(|sample| sample.frames() > 0),
            sample_rate: 48_000.0,
            voices: vec![Voice::new(); MAX_VOICES + SPARE_VOICES],
            clock: 0,
        }
    }

    fn refresh(&mut self) {
        self.params.changed = false;
        let Some(sample) = self.sample.as_deref() else { return };
        let p = &self.params;
        let sr = self.sample_rate as f32;
        let d = &mut self.derived;
        let frames = sample.frames() as f64;
        let percent = |index: P| p.get(index as usize) as f64 * 0.01 * frames;

        d.start = percent(P::Start).min(frames - 1.0).floor();
        d.end = percent(P::End).max(d.start + 1.0).floor();
        d.reverse = p.on(P::Reverse as usize);
        d.one_shot = p.on(P::OneShot as usize);
        let mode = match p.index(P::LoopMode as usize) {
            _ if d.one_shot => LoopMode::Off,
            0 => LoopMode::Off,
            1 => LoopMode::Forward,
            _ => LoopMode::PingPong,
        };
        // The loop has to lie inside the region being played.
        let loop_start = percent(P::LoopStart).clamp(d.start, d.end);
        let loop_end = percent(P::LoopEnd).clamp(d.start, d.end);
        let crossfade = p.get(P::LoopCrossfade as usize) as f64 * 0.001 * sample.sample_rate();
        d.looping = LoopSpec::new(sample, mode, loop_start, loop_end, crossfade, d.reverse);

        let semitones = p.get(P::Transpose as usize) + p.get(P::Fine as usize) * 0.01 - p.get(P::Root as usize);
        d.tune = sample.sample_rate() / self.sample_rate * 2f64.powf(semitones as f64 / 12.0);

        let attack = p.get(P::Attack as usize);
        d.instant_attack = attack < INSTANT_ATTACK;
        d.amp =
            EnvShape::new(attack, p.get(P::Decay as usize), p.get(P::Sustain as usize), p.get(P::Release as usize), sr);
        let filter_attack = p.get(P::FilterAttack as usize);
        d.instant_filter_attack = filter_attack < INSTANT_ATTACK;
        d.filt = EnvShape::new(
            filter_attack,
            p.get(P::FilterDecay as usize),
            p.get(P::FilterSustain as usize),
            p.get(P::FilterRelease as usize),
            sr,
        );
        d.filter = p.index(P::FilterType as usize).checked_sub(1).map(FilterMode::from_index);
        d.cutoff = p.get(P::Cutoff as usize);
        d.q = resonance_to_q(p.get(P::Resonance as usize));
        d.filter_env = p.get(P::FilterEnv as usize);
        d.key_track = p.get(P::KeyTrack as usize);
        d.velocity_sens = p.get(P::VelocitySens as usize);
        let gain = db_to_gain(p.get(P::Volume as usize));
        let (pan_left, pan_right) = pan_gains(p.get(P::Pan as usize));
        d.gain_left = gain * pan_left;
        d.gain_right = gain * pan_right;
        d.voices = p.index(P::Voices as usize).clamp(1, MAX_VOICES);
    }

    fn note_on(&mut self, pitch: u8, velocity: f32) {
        let Some(sample) = self.sample.as_deref() else { return };
        let d = &self.derived;
        let sample_rate = self.sample_rate as f32;
        self.clock += 1;

        // A key struck again while it still sounds lets the earlier note go,
        // as lifting the key would. One-shot notes simply overlap.
        for voice in self.voices.iter_mut().filter(|voice| voice.active && voice.held && voice.pitch == pitch) {
            voice.release();
        }
        // Make room: voices over the limit fade out quickly rather than stop.
        while self.voices.iter().filter(|voice| voice.playing()).count() >= d.voices {
            let victim = steal(&self.voices);
            self.voices[victim].fade.start(STEAL_SECONDS, sample_rate);
        }

        let sens = d.velocity_sens;
        let opened = Envelope { stage: Stage::Decay, level: 1.0 };
        let attacking = Envelope { stage: Stage::Attack, level: 0.0 };
        let pitch_ratio = 2f64.powf(pitch as f64 / 12.0);
        let index = free_voice(&self.voices);
        self.voices[index] = Voice {
            active: true,
            held: !d.one_shot,
            pitch,
            pitch_ratio,
            velocity_gain: 1.0 - sens + sens * velocity * velocity,
            age: self.clock,
            amp: if d.instant_attack { opened } else { attacking },
            filt: if d.instant_filter_attack { opened } else { attacking },
            filter: VoiceFilter::default(),
            head: Some(Playhead::new(sample, d.start, d.end, d.reverse, pitch_ratio * d.tune, self.sample_rate)),
            fade: FadeOut::idle(),
        };
    }

    fn note_off(&mut self, pitch: u8) {
        for voice in self.voices.iter_mut().filter(|voice| voice.active && voice.held && voice.pitch == pitch) {
            voice.release();
        }
    }

    fn handle(&mut self, kind: EventKind) {
        match kind {
            EventKind::NoteOn { pitch, velocity } => self.note_on(pitch, velocity),
            EventKind::NoteOff { pitch } => self.note_off(pitch),
            // One-shot notes are released too: stopping the transport
            // should not leave a long sample playing to its end.
            EventKind::AllNotesOff => self.voices.iter_mut().filter(|voice| voice.active).for_each(Voice::release),
            EventKind::PitchBend { .. } | EventKind::Control { .. } => {}
        }
    }

    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let Some(sample) = self.sample.as_deref() else { return };
        let sample_rate = self.sample_rate as f32;
        let mut pos = 0;
        while pos < left.len() {
            let len = (left.len() - pos).min(CONTROL_BLOCK);
            for voice in self.voices.iter_mut().filter(|voice| voice.active) {
                voice.render(&self.derived, sample, sample_rate, &mut left[pos..pos + len], &mut right[pos..pos + len]);
            }
            pos += len;
        }
    }
}

/// Picks the voice to give up when the limit is reached: the quietest one
/// already released, or failing that the oldest.
fn steal(voices: &[Voice]) -> usize {
    let playing = || voices.iter().enumerate().filter(|(_, voice)| voice.playing());
    let released = playing()
        .filter(|(_, voice)| voice.amp.stage == Stage::Release)
        .min_by(|a, b| a.1.amp.level.total_cmp(&b.1.amp.level));
    released.or_else(|| playing().min_by_key(|(_, voice)| voice.age)).map_or(0, |(index, _)| index)
}

/// Picks the slot for a new note: a silent one, or failing that whichever
/// fading voice is nearest to silence.
fn free_voice(voices: &[Voice]) -> usize {
    if let Some(index) = voices.iter().position(|voice| !voice.active) {
        return index;
    }
    let fading = voices
        .iter()
        .enumerate()
        .filter(|(_, voice)| voice.fade.fading())
        .min_by(|a, b| a.1.fade.gain().total_cmp(&b.1.fade.gain()));
    // There are more slots than voices allowed to play, so one is always
    // free or fading; the fallback only keeps this total.
    fading.or_else(|| voices.iter().enumerate().min_by_key(|(_, voice)| voice.age)).map_or(0, |(index, _)| index)
}

impl DeviceProcessor for Sampler {
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
        for voice in &mut self.voices {
            voice.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use crate::voice::FILTER_MODES;
    use trill_core::audio::AudioBuffer;

    const SR: f64 = 48_000.0;

    fn sampler_at(left: Vec<f32>, file_rate: f64, engine_rate: f64, params: &[(&str, f64)]) -> Sampler {
        let buffer = AudioBuffer { sample_rate: file_rate, right: left.clone(), left };
        let mut sampler = Sampler::new(Some(Arc::new(Sample::new(&buffer))));
        sampler.prepare(engine_rate, 512);
        for (id, value) in params {
            let index = PARAMS.iter().position(|def| def.id == *id).unwrap_or_else(|| panic!("no parameter '{id}'"));
            sampler.set_param(index, *value);
        }
        sampler
    }

    fn sampler(left: Vec<f32>, params: &[(&str, f64)]) -> Sampler {
        sampler_at(left, SR, SR, params)
    }

    const ON: fn(u8) -> EventKind = |pitch| EventKind::NoteOn { pitch, velocity: 1.0 };
    const OFF: fn(u8) -> EventKind = |pitch| EventKind::NoteOff { pitch };
    /// Unity gain, centered, full velocity range off: output is the file.
    const PLAIN: &[(&str, f64)] = &[("volume", 0.0), ("velocity_sens", 0.0)];

    fn with(base: &[(&'static str, f64)], more: &[(&'static str, f64)]) -> Vec<(&'static str, f64)> {
        base.iter().chain(more).copied().collect()
    }

    #[test]
    fn filter_choices_match_the_shared_modes() {
        assert_eq!(&FILTERS[1..], FILTER_MODES);
    }

    #[test]
    fn state_round_trips_and_keeps_what_it_does_not_know() {
        let state = State::new("C:/Samples/a b.wav");
        let bytes = state.to_bytes();
        assert_eq!(State::from_bytes(&bytes).unwrap(), state);
        assert_eq!(State::from_device_state(None).unwrap(), State::default());
        assert_eq!(State::default().path, None);

        let newer = br#"{"version": 1, "path": "x.wav", "slices": [1, 2, 3], "note": "hello"}"#;
        let state = State::from_bytes(newer).unwrap();
        assert_eq!(state.path.as_deref(), Some("x.wav"));
        let rewritten: serde_json::Value = serde_json::from_slice(&state.to_bytes()).unwrap();
        assert_eq!(rewritten["slices"], serde_json::json!([1, 2, 3]));
        assert_eq!(rewritten["note"], "hello");

        assert!(matches!(State::from_bytes(br#"{"version": 2}"#), Err(StateError::TooNew { found: 2, supported: 1 })));
        assert!(matches!(State::from_bytes(b"\x00\x01"), Err(StateError::Invalid(_))));
        assert_eq!(
            sample_refs(Some(&bytes)).unwrap(),
            [SampleRef { slot: "sample".into(), path: "C:/Samples/a b.wav".into() }]
        );
        assert!(sample_refs(None).unwrap().is_empty());
    }

    #[test]
    fn root_note_plays_the_file_as_it_is() {
        let file: Vec<f32> = sine(440.0, SR, 24_000).iter().map(|s| s * 0.5).collect();
        let mut device = sampler(file.clone(), PLAIN);
        let (left, right) = run(&mut device, SR, 256, 30_000, &[(100, ON(60))]);
        assert_eq!(peak(&left[..100]), 0.0);
        for (i, want) in file.iter().enumerate() {
            assert!((left[100 + i] - want).abs() < 1e-5, "frame {i}");
        }
        assert!(left == right);
        assert_eq!(peak(&left[24_100..]), 0.0, "silent once the file has ended");
    }

    #[test]
    fn keys_play_the_right_pitches() {
        let file = sine(261.625_565, SR, 96_000);
        for (pitch, want) in
            [(36u8, 65.406_391), (48, 130.812_783), (60, 261.625_565), (67, 391.995_436), (84, 1_046.502_261)]
        {
            let mut device = sampler(file.clone(), PLAIN);
            let frames = if pitch >= 72 { 20_000 } else { 40_000 };
            let (left, _) = run(&mut device, SR, 256, frames, &[(0, ON(pitch))]);
            let hz = zero_crossing_hz(&left[1_000..frames - 1_000], SR);
            assert!((hz - want).abs() < want * 2e-4, "note {pitch}: {hz:.3} Hz, wanted {want:.3}");
        }
        // Root, transpose and fine tune all move the pitch.
        let cases: [(&[(&str, f64)], f64); 3] = [
            (&[("root", 72.0)], 130.812_783),
            (&[("transpose", 7.0)], 391.995_436),
            (&[("fine", 50.0), ("transpose", -1.0)], 254.177_638),
        ];
        for (params, want) in cases {
            let mut device = sampler(file.clone(), &with(PLAIN, params));
            let (left, _) = run(&mut device, SR, 256, 40_000, &[(0, ON(60))]);
            let hz = zero_crossing_hz(&left[1_000..39_000], SR);
            assert!((hz - want).abs() < want * 2e-4, "{params:?}: {hz:.3} Hz, wanted {want:.3}");
        }
    }

    #[test]
    fn pitch_and_length_are_right_at_every_engine_rate() {
        // A one-second 440 Hz file at each file rate, played at each engine
        // rate: always 440 Hz, always one second.
        for file_rate in [44_100.0, 48_000.0, 96_000.0] {
            let file = sine(440.0, file_rate, file_rate as usize);
            for engine_rate in [44_100.0, 48_000.0, 96_000.0] {
                let mut device = sampler_at(file.clone(), file_rate, engine_rate, PLAIN);
                let frames = (engine_rate * 1.2) as usize;
                let (left, _) = run(&mut device, engine_rate, 512, frames, &[(0, ON(60))]);
                let second = engine_rate as usize;
                let hz = zero_crossing_hz(&left[1_000..second - 1_000], engine_rate);
                assert!((hz - 440.0).abs() < 0.1, "file {file_rate} engine {engine_rate}: {hz:.3} Hz");
                let level = rms(&left[1_000..second - 1_000]);
                assert!((level - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01, "level {level}");
                let last = left.iter().rposition(|s| s.abs() > 1e-3).unwrap();
                assert!(
                    (last as i64 - second as i64).abs() < 64,
                    "file {file_rate} engine {engine_rate}: ended at {last}"
                );
            }
        }
    }

    #[test]
    fn gated_notes_release_and_one_shots_play_out() {
        let file = sine(440.0, SR, 48_000);
        let mut device = sampler(file.clone(), &with(PLAIN, &[("release", 0.01)]));
        let (left, _) = run(&mut device, SR, 256, 20_000, &[(0, ON(60)), (4_800, OFF(60))]);
        assert!(rms(&left[1_000..4_800]) > 0.6);
        assert_eq!(peak(&left[6_000..]), 0.0, "the release is over well within 25 ms");

        let mut device = sampler(file, &with(PLAIN, &[("one_shot", 1.0), ("loop_mode", 1.0)]));
        let (left, _) = run(&mut device, SR, 256, 60_000, &[(0, ON(60)), (4_800, OFF(60))]);
        assert!(rms(&left[40_000..47_000]) > 0.6, "a one-shot ignores the key coming up");
        assert_eq!(peak(&left[48_100..]), 0.0, "and ignores the loop");
    }

    #[test]
    fn start_end_and_reverse_select_what_is_played() {
        // A staircase: the value says which tenth of the file is playing.
        let file: Vec<f32> = (0..10_000).map(|i| (i / 1_000) as f32 * 0.1).collect();
        let params = with(PLAIN, &[("start", 30.0), ("end", 60.0), ("one_shot", 1.0)]);
        let mut device = sampler(file.clone(), &params);
        let (left, _) = run(&mut device, SR, 256, 4_000, &[(0, ON(60))]);
        assert!((left[500] - 0.3).abs() < 1e-3 && (left[2_500] - 0.5).abs() < 1e-3);
        assert_eq!(peak(&left[3_001..]), 0.0);

        let mut device = sampler(file, &with(&params, &[("reverse", 1.0)]));
        let (left, _) = run(&mut device, SR, 256, 4_000, &[(0, ON(60))]);
        assert!((left[500] - 0.5).abs() < 1e-3 && (left[2_500] - 0.3).abs() < 1e-3);
        assert_eq!(peak(&left[3_001..]), 0.0);
    }

    #[test]
    fn loops_are_continuous_at_the_loop_points() {
        // The loop holds exactly 30 periods of the tone, so a correct loop
        // is indistinguishable from the tone going on for ever.
        let period = 120.0;
        let file = sine(SR / period, SR, 12_000);
        let natural = (std::f64::consts::TAU / period) as f32;
        for mode in [1.0, 2.0] {
            for pitch in [48u8, 60, 67, 79] {
                // A ping-pong loop only joins up if it turns on the crests.
                let (start, end) = if mode == 2.0 { (3_030.0, 6_631.0) } else { (3_000.0, 6_600.0) };
                let params =
                    with(PLAIN, &[("loop_mode", mode), ("loop_start", start / 120.0), ("loop_end", end / 120.0)]);
                let mut device = sampler(file.clone(), &params);
                let (left, _) = run(&mut device, SR, 256, 96_000, &[(0, ON(pitch))]);
                let rate = 2f32.powf((pitch as f32 - 60.0) / 12.0);
                let step = largest_step(&left[100..]);
                assert!(step < natural * rate * 1.05 + 0.002, "mode {mode} note {pitch}: step {step}");
                assert!(rms(&left[90_000..]) > 0.69, "mode {mode} note {pitch}: the loop should sustain");
                let hz = zero_crossing_hz(&left[20_000..], SR);
                let want = SR / period * rate as f64;
                assert!((hz - want).abs() < want * 1e-3, "mode {mode} note {pitch}: {hz} Hz");
            }
        }
    }

    #[test]
    fn notes_start_stop_and_steal_without_clicks() {
        // A low, loud tone: any abrupt cut shows up as a step far larger
        // than the 0.0131 between its own samples.
        let file = sine(100.0, SR, 96_000);
        let natural = (std::f64::consts::TAU * 100.0 / SR) as f32;
        let limit = natural * 4.0;

        // Starting in the middle of the file, stopping by key-up mid-cycle.
        let params = with(PLAIN, &[("start", 10.3), ("release", 0.005), ("attack", 0.0)]);
        let mut device = sampler(file.clone(), &params);
        let (left, _) = run(&mut device, SR, 256, 20_000, &[(1_000, ON(60)), (9_123, OFF(60))]);
        assert!(largest_step(&left) < limit, "start/stop step {}", largest_step(&left));
        assert!(rms(&left[2_000..9_000]) > 0.6);

        // An end point in the middle of a cycle.
        let params = with(PLAIN, &[("end", 20.07), ("one_shot", 1.0)]);
        let mut device = sampler(file.clone(), &params);
        let (left, _) = run(&mut device, SR, 256, 30_000, &[(0, ON(60))]);
        assert!(largest_step(&left) < limit, "end point step {}", largest_step(&left));

        // Two voices allowed, a new note every 50 ms: every note steals.
        let params = with(PLAIN, &[("voices", 2.0), ("start", 5.0), ("volume", -12.0)]);
        let mut device = sampler(file.clone(), &params);
        let events: Vec<(usize, EventKind)> = (0..30).map(|i| (i * 2_400 + 77, ON(55 + (i % 7) as u8))).collect();
        let (left, _) = run(&mut device, SR, 256, 80_000, &events);
        // Up to three voices overlap during a steal, each at a quarter level
        // and up to a fifth above the root.
        let steal_limit = natural * 1.5 * 0.25 * 3.0 * 2.0;
        assert!(largest_step(&left) < steal_limit, "steal step {} (limit {steal_limit})", largest_step(&left));
        assert!(rms(&left[40_000..]) > 0.1);

        // The transport stopping, and a reset mid-note being survivable.
        let mut device = sampler(file, &with(PLAIN, &[("one_shot", 1.0), ("release", 0.01)]));
        let (left, _) = run(&mut device, SR, 256, 20_000, &[(0, ON(60)), (5_000, EventKind::AllNotesOff)]);
        assert!(largest_step(&left) < limit && peak(&left[6_000..]) == 0.0);
        device.reset();
        let (left, _) = run(&mut device, SR, 256, 1_000, &[]);
        assert_eq!(peak(&left), 0.0);
    }

    #[test]
    fn polyphony_is_limited_and_voices_are_reused() {
        let file = vec![0.25; 480_000];
        let mut device = sampler(file, &with(PLAIN, &[("voices", 4.0), ("attack", 0.01)]));
        let events: Vec<(usize, EventKind)> = (0..40).map(|i| (i * 1_000, ON(40 + i as u8))).collect();
        let (left, _) = run(&mut device, SR, 256, 60_000, &events);
        assert!(device.voices.iter().filter(|voice| voice.playing()).count() == 4);
        // Four voices of a constant 0.25 can never exceed 1; a fifth fading
        // out adds a little for 5 ms.
        assert!(peak(&left) < 1.26, "peak {}", peak(&left));
    }

    #[test]
    fn filter_and_envelopes_shape_the_sound() {
        let noise: Vec<f32> =
            (0..48_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 8) as f32 / 8_388_608.0 - 1.0).collect();
        let mut open = sampler(noise.clone(), PLAIN);
        let (bright, _) = run(&mut open, SR, 256, 24_000, &[(0, ON(60))]);
        let mut closed = sampler(noise.clone(), &with(PLAIN, &[("filter_type", 2.0), ("cutoff", 300.0)]));
        let (dark, _) = run(&mut closed, SR, 256, 24_000, &[(0, ON(60))]);
        assert!(rms(&dark[4_000..]) < rms(&bright[4_000..]) * 0.2);
        // The envelope opens the filter again.
        let params =
            with(PLAIN, &[("filter_type", 2.0), ("cutoff", 300.0), ("filter_env", 6.0), ("filter_sustain", 1.0)]);
        let mut swept = sampler(noise.clone(), &params);
        let (opened, _) = run(&mut swept, SR, 256, 24_000, &[(0, ON(60))]);
        assert!(rms(&opened[4_000..]) > rms(&dark[4_000..]) * 3.0);

        // Attack, velocity, volume and pan.
        let mut slow = sampler(vec![0.5; 48_000], &with(PLAIN, &[("attack", 0.1)]));
        let (left, _) = run(&mut slow, SR, 256, 9_600, &[(0, ON(60))]);
        assert!(left[0].abs() < 0.01 && (left[2_400] - 0.25).abs() < 0.01 && (left[9_000] - 0.5).abs() < 0.01);

        let mut device = sampler(vec![0.5; 48_000], &[("volume", -6.0), ("velocity_sens", 1.0), ("pan", 1.0)]);
        let (left, right) = run(&mut device, SR, 256, 1_000, &[(0, EventKind::NoteOn { pitch: 60, velocity: 0.5 })]);
        let want = 0.5 * db_to_gain(-6.0) * 0.25 * std::f32::consts::SQRT_2;
        assert!(peak(&left) < 1e-6 && (right[500] - want).abs() < 1e-4, "{} vs {want}", right[500]);
    }

    #[test]
    fn a_sampler_without_a_sample_is_silent() {
        let mut device = Sampler::new(None);
        device.prepare(SR, 512);
        let (left, _) = run(&mut device, SR, 256, 2_000, &[(0, ON(60)), (500, OFF(60)), (600, EventKind::AllNotesOff)]);
        assert_eq!(peak(&left), 0.0);
        let empty = AudioBuffer { sample_rate: SR, left: vec![], right: vec![] };
        let mut device = Sampler::new(Some(Arc::new(Sample::new(&empty))));
        device.prepare(SR, 512);
        let (left, _) = run(&mut device, SR, 256, 2_000, &[(0, ON(60))]);
        assert_eq!(peak(&left), 0.0);
    }
}
