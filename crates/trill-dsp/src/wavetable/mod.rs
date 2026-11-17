//! Wavetable: a synthesizer whose oscillators read tables of waves, built
//! to be shaped by someone who cannot hear it.
//!
//! Two oscillators each read a [`Table`]: a row of single cycles that a
//! position moves through. Around them are a sub oscillator, noise, two
//! filters, three envelopes, three LFOs, four macros and a matrix of
//! sixteen slots that says what moves what. Everything that shapes the
//! sound is a parameter with a name that says what it does
//! ([`params`]), so that a sound can be read and written as data.
//!
//! # Tables
//!
//! The tables that ship are made in code ([`factory`]) and chosen by name
//! through `osc1_table` and `osc2_table`. An oscillator can also be given
//! a table of its own, which lives in the device's state:
//!
//! ```json
//! {
//!   "version": 1,
//!   "macros": ["Brightness", "Motion", "", ""],
//!   "osc1": { "name": "Thin Reed", "size": 32,
//!             "frames": [ { "shape": "pulse", "width": 0.5 }, { "shape": "pulse", "width": 0.04 } ] },
//!   "osc2": { "file": "C:/Sounds/Tables/glass.wav" }
//! }
//! ```
//!
//! * `osc1`, `osc2`: the oscillator's own table, which it plays when its
//!   table parameter is `custom`: a recipe ([`recipe::Recipe`]) or an
//!   audio file ([`recipe::FileSpec`]). A file is named like a sampler's:
//!   it is listed with the song's files and can be found again.
//! * `macros`: what the four macros are called in this sound.
//!
//! Tables are made when the device is created, never on the audio thread.
//!
//! # Nothing steps
//!
//! A value that is moved (a level, a cutoff, a macro) takes 256 frames to
//! get there. What cannot be moved gradually (another table, another kind
//! of filter, another warp, the number of unison copies) is changed behind
//! a gate: the synth dips to nothing in a two-hundredth of a second,
//! changes, and comes back as quickly.
//!
//! # The same song is the same samples
//!
//! Everything that moves a voice is worked out every [`TICK`] frames of
//! the synth's own clock and reached in a line between, so the sound does
//! not depend on how the frames are handed over. What is drawn by lot (the
//! phases a note starts at, its noise, an LFO's steps) is drawn from the
//! note: its pitch and how many notes came before it since the synth was
//! last reset.

pub mod factory;
pub mod params;
pub mod patches;
pub mod recipe;
pub mod table;
mod voice;

use crate::level::ChannelLevel;
use crate::loading::{SampleRef, SampleSource, StateError, decode, encode};
use crate::params::ParamValues;
use crate::util::level_gain;
use crate::voice::EnvShape;
use factory::Tables;
use params::{ENV_PARAMS, FILTER_PARAMS, LFO_PARAMS, MAX_UNISON, MAX_VOICES, OSC_PARAMS, P, PARAMS, SLOTS, of};
use recipe::{FileSpec, Reading, Recipe};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};
use table::Table;
use trill_core::Device;
use trill_core::audio::{AudioBuffer, AudioLoader};
use trill_core::controllers::CONTROLLER_BREATH;
use trill_core::device::{CONTROLLER_MOD_WHEEL, DeviceProcessor, DeviceWarning, Event, EventKind, Pedal, ProcessInfo};
use voice::{FilterKind, OscSetting, SlotList, TICK, Voice, Warp, dest, lfo_shape, source};

/// The device's URI.
pub const URI: &str = "builtin:wavetable";

/// The state format this build writes, and the newest it reads.
pub const STATE_VERSION: u32 = 1;
/// The level of one voice of one oscillator at a volume of 0 dB and full
/// velocity, for a wave that reaches full level: the poly synth's, so
/// that the same volume setting is the same level on either. At the
/// default volume a voice peaks 13 dB under full scale, which is the room
/// a chord needs.
pub(crate) const VOICE_GAIN: f32 = 2.468_44;
/// Voices that only ever hold notes on their way out after making room,
/// so that a note and the one that took its place overlap for a moment.
const SPARE_VOICES: usize = 8;
/// How many ticks a parameter that was moved takes to get there: 256
/// frames, the stretch an envelope holds a value for.
const SMOOTH_TICKS: u32 = 16;
/// The most keys remembered as held, for one-note-at-a-time playing.
const MAX_HELD: usize = 16;

/// Where each value that glides when its parameter is moved is kept.
pub(crate) mod smoothed {
    /// Per oscillator: level, position, warp amount, pan.
    pub const OSC: usize = 0;
    pub const OSC_VALUES: usize = 4;
    pub const FM: usize = 8;
    pub const SUB: usize = 9;
    pub const NOISE: usize = 10;
    /// Per filter: cutoff (as octaves: the logarithm of its Hz),
    /// resonance, drive.
    pub const FILTER: usize = 11;
    pub const FILTER_VALUES: usize = 3;
    pub const BALANCE: usize = 17;
    pub const MACRO: usize = 18;
    pub const VOLUME: usize = 22;
    /// The amount of each slot of the matrix.
    pub const AMOUNT: usize = 23;
    pub const COUNT: usize = AMOUNT + super::SLOTS;
}

/// A parameter's value on its way to where it was set, a tick at a time.
#[derive(Copy, Clone, Default)]
pub(crate) struct Smooth {
    pub now: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Smooth {
    fn snap(&mut self, value: f32) {
        *self = Self { now: value, target: value, step: 0.0, left: 0 };
    }

    fn aim(&mut self, target: f32) {
        if target != self.target {
            self.target = target;
            self.left = SMOOTH_TICKS;
            self.step = (target - self.now) / SMOOTH_TICKS as f32;
        }
    }

    fn tick(&mut self) {
        if self.left > 0 {
            self.left -= 1;
            self.now = if self.left == 0 { self.target } else { self.now + self.step };
        }
    }
}

/// What an LFO is set to.
#[derive(Copy, Clone, Default)]
pub(crate) struct LfoSetting {
    pub shape: usize,
    rate: f32,
    /// The length of a cycle in the song's time: beats, or bars.
    sync: Option<Span>,
    pub retrigger: bool,
    pub fade_frames: f32,
    pub depth: f32,
    pub phase: f32,
}

/// The length of a synced LFO's cycle.
#[derive(Copy, Clone, PartialEq)]
enum Span {
    Beats(f64),
    Bars(f64),
}

fn span_of(sync: usize) -> Option<Span> {
    Some(match sync {
        1 => Span::Bars(8.0),
        2 => Span::Bars(4.0),
        3 => Span::Bars(2.0),
        4 => Span::Bars(1.0),
        5 => Span::Beats(2.0),
        6 => Span::Beats(1.0),
        7 => Span::Beats(0.5),
        8 => Span::Beats(0.25),
        9 => Span::Beats(0.125),
        10 => Span::Beats(3.0),
        11 => Span::Beats(1.5),
        12 => Span::Beats(0.75),
        13 => Span::Beats(2.0 / 3.0),
        14 => Span::Beats(1.0 / 3.0),
        15 => Span::Beats(1.0 / 6.0),
        _ => return None,
    })
}

/// Everything derived from the parameters that a note does not change,
/// refreshed when one is set.
#[derive(Clone, Default)]
pub(crate) struct Derived {
    // (See `same_structure` for what of this cannot be changed gradually.)
    pub osc: [OscSetting; 2],
    pub sub_shape: usize,
    /// The sub's pitch against the note's, in semitones.
    pub sub_tune: f32,
    pub sub_direct: bool,
    /// What the right side takes of the noise the left side has, and of a
    /// noise of its own.
    pub noise_same: f32,
    pub noise_own: f32,
    /// The noise's colour: how much of the way a low-pass goes each
    /// frame (1 = none), whether what it leaves or what it takes is
    /// heard, and what makes up for what a dark noise loses.
    pub noise_pole: f32,
    pub noise_dark: bool,
    pub noise_makeup: f32,
    pub filter: [FilterKind; 2],
    pub key_track: [f32; 2],
    pub serial: bool,
    pub split: bool,
    pub env: [EnvShape; 3],
    pub lfo: [LfoSetting; 3],
    /// The slots of the matrix that do something: what each reads and
    /// moves, and which slot it is.
    pub slots: usize,
    pub slot: SlotList,
    pub slot_param: [usize; SLOTS],
    mode: usize,
    voices: usize,
    /// The share of the way to its note a pitch keeps for each frame.
    pub glide_keep: f32,
    pub glides: bool,
    pub bend_range: f32,
    pub velocity_sens: f32,
}

impl Derived {
    /// Whether going from `other` to this changes nothing that cannot be
    /// changed gradually: the tables, the warps, how many copies there
    /// are, the sub's shape and way, the filters' kinds and their order.
    fn same_structure(&self, other: &Derived) -> bool {
        let osc = |which: usize| {
            let (a, b) = (&self.osc[which], &other.osc[which]);
            a.table == b.table && a.warp == b.warp && a.unison == b.unison
        };
        osc(0)
            && osc(1)
            && self.sub_shape == other.sub_shape
            && self.sub_direct == other.sub_direct
            && self.filter == other.filter
            && self.serial == other.serial
            && self.split == other.split
    }
}

/// What every voice of a synth reads: its settings, where the wheels
/// are, and the tables.
pub(crate) struct Shared {
    pub d: Derived,
    pub sample_rate: f32,
    pub smooth: [Smooth; smoothed::COUNT],
    /// What the matrix reads that is the same for every note; the places
    /// of what a note has for itself are left at nothing.
    pub sources: [f32; source::COUNT],
    /// The pitch wheel, -1..=1.
    pub wheel: f32,
    /// Each LFO's rate in Hz as it is set, and what one that runs on for
    /// all notes puts out.
    pub lfo_hz: [f32; 3],
    pub lfo_value: [f32; 3],
    /// How far open the gate is behind which what cannot be changed
    /// gradually is changed: 1, but for the moment of such a change.
    pub gate: f32,
    /// The level of each unison copy of each oscillator on either side
    /// (oscillator, side, copy), on its way to where the settings have it.
    pub copies: [[[f32; MAX_UNISON]; 2]; 2],
    tables: Arc<Tables>,
    custom: [Option<Arc<Table>>; 2],
}

impl Shared {
    /// The table oscillator `which` reads.
    #[inline]
    pub fn table(&self, which: usize) -> &Table {
        match self.tables.chosen(self.d.osc[which].table) {
            Some(table) => table,
            None => self.custom[which].as_deref().unwrap_or(self.tables.fallback()),
        }
    }
}

/// Where the song is, for the LFOs that keep time with it.
#[derive(Copy, Clone)]
struct Song {
    playing: bool,
    tempo: f64,
    per_bar: f64,
    /// The beat at the frame `clock`, and the bar that beat is in.
    beat: f64,
    clock: u64,
    bar: f64,
    bar_start: f64,
}

pub struct Wavetable {
    params: ParamValues,
    shared: Shared,
    voices: Vec<Voice>,
    /// Frames played since the synth was made or last reset, and how many
    /// are left of the tick it is in.
    clock: u64,
    until_tick: usize,
    /// Notes struck since then.
    notes: u32,
    song: Song,
    /// The LFOs that run on for all notes: where in their cycle, and which
    /// cycle it is.
    lfo_phase: [f32; 3],
    lfo_step: [u32; 3],
    /// Held keys in the order they went down, for one note at a time.
    held: [u8; MAX_HELD],
    held_len: usize,
    last_note: Option<f32>,
    pedal: Pedal,
    level: ChannelLevel,
    /// Whether the values that glide have been told where they start.
    started: bool,
    /// Settings that wait for the gate to shut before they are taken on.
    pending: Option<Derived>,
}

impl Wavetable {
    /// A synth whose oscillators have `custom` as their own tables.
    pub fn new(custom: [Option<Arc<Table>>; 2]) -> Self {
        let sample_rate = 48_000.0;
        Self {
            params: ParamValues::new(PARAMS),
            shared: Shared {
                d: Derived::default(),
                sample_rate,
                smooth: [Smooth::default(); smoothed::COUNT],
                sources: [0.0; source::COUNT],
                wheel: 0.0,
                lfo_hz: [1.0; 3],
                lfo_value: [0.0; 3],
                gate: 1.0,
                copies: [[[0.0; MAX_UNISON]; 2]; 2],
                tables: Tables::get(),
                custom,
            },
            voices: Vec::new(),
            clock: 0,
            until_tick: 0,
            notes: 0,
            song: Song { playing: false, tempo: 120.0, per_bar: 4.0, beat: 0.0, clock: 0, bar: 0.0, bar_start: 0.0 },
            lfo_phase: [0.0; 3],
            lfo_step: [0; 3],
            held: [0; MAX_HELD],
            held_len: 0,
            last_note: None,
            pedal: Pedal::default(),
            level: ChannelLevel::new(),
            started: false,
            pending: None,
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.shared.sample_rate;
        let mut next = self.shared.d.clone();
        let d = &mut next;
        let mut targets = [0.0f32; smoothed::COUNT];

        for which in 0..2 {
            let at = |first: P| p.get(of(first, which, OSC_PARAMS));
            let setting = &mut d.osc[which];
            setting.table = p.index(of(P::Osc1Table, which, OSC_PARAMS));
            setting.warp = Warp::from_index(p.index(of(P::Osc1Warp, which, OSC_PARAMS)));
            setting.tune = 12.0 * at(P::Osc1Octave) + at(P::Osc1Semitones) + at(P::Osc1Fine) / 100.0;
            setting.lay_out(
                p.index(of(P::Osc1Unison, which, OSC_PARAMS)),
                at(P::Osc1Detune),
                at(P::Osc1Blend),
                at(P::Osc1Spread),
            );
            setting.phase = at(P::Osc1Phase);
            setting.phase_random = at(P::Osc1PhaseRandom);
            let base = smoothed::OSC + which * smoothed::OSC_VALUES;
            targets[base] = at(P::Osc1Level);
            targets[base + 1] = at(P::Osc1Position);
            targets[base + 2] = at(P::Osc1WarpAmount);
            targets[base + 3] = at(P::Osc1Pan);
        }
        targets[smoothed::FM] = p.get(P::Osc1Fm as usize);

        d.sub_shape = p.index(P::SubShape as usize);
        d.sub_tune = 12.0 * p.get(P::SubOctave as usize);
        d.sub_direct = p.on(P::SubDirect as usize);
        targets[smoothed::SUB] = p.get(P::SubLevel as usize);
        targets[smoothed::NOISE] = p.get(P::NoiseLevel as usize);
        let width = p.get(P::NoiseWidth as usize) * std::f32::consts::FRAC_PI_2;
        (d.noise_same, d.noise_own) = (width.cos(), width.sin());
        // Dark: a low-pass that comes down from the top of hearing to
        // 150 Hz. Thin: what a low-pass that goes up to 9 kHz takes away.
        let colour = p.get(P::NoiseColor as usize);
        let pole = |hz: f32| 1.0 - (-std::f32::consts::TAU * hz.min(sr * 0.45) / sr).exp();
        if colour < 0.0 {
            let hz = 20_000.0 * (150.0f32 / 20_000.0).powf(-colour);
            d.noise_pole = pole(hz);
            d.noise_dark = true;
            // As loud as white noise is, for what is left of it.
            d.noise_makeup = (2.0 / d.noise_pole - 1.0).sqrt().min(12.0);
        } else if colour > 0.0 {
            d.noise_pole = pole(20.0 * (9_000.0f32 / 20.0).powf(colour));
            d.noise_dark = false;
            d.noise_makeup = 1.0;
        } else {
            (d.noise_pole, d.noise_dark, d.noise_makeup) = (1.0, true, 1.0);
        }

        for which in 0..2 {
            let at = |first: P| p.get(of(first, which, FILTER_PARAMS));
            d.filter[which] = FilterKind::from_index(p.index(of(P::Filter1Type, which, FILTER_PARAMS)));
            d.key_track[which] = at(P::Filter1KeyTrack);
            let base = smoothed::FILTER + which * smoothed::FILTER_VALUES;
            targets[base] = at(P::Filter1Cutoff).log2();
            targets[base + 1] = at(P::Filter1Resonance);
            targets[base + 2] = at(P::Filter1Drive);
        }
        let routing = p.index(P::FilterRouting as usize);
        d.serial = routing == 0;
        d.split = routing == 2;
        targets[smoothed::BALANCE] = p.get(P::FilterBalance as usize);

        for which in 0..3 {
            let at = |first: P| p.get(of(first, which, ENV_PARAMS));
            d.env[which] =
                EnvShape::new(at(P::Env1Attack), at(P::Env1Decay), at(P::Env1Sustain), at(P::Env1Release), sr);
            let at = |first: P| p.get(of(first, which, LFO_PARAMS));
            d.lfo[which] = LfoSetting {
                shape: p.index(of(P::Lfo1Shape, which, LFO_PARAMS)),
                rate: at(P::Lfo1Rate),
                sync: span_of(p.index(of(P::Lfo1Sync, which, LFO_PARAMS))),
                retrigger: p.on(of(P::Lfo1Retrigger, which, LFO_PARAMS)),
                fade_frames: at(P::Lfo1Fade) * sr,
                depth: at(P::Lfo1Depth),
                phase: at(P::Lfo1Phase),
            };
        }

        d.slots = 0;
        for slot in 0..SLOTS {
            let from = p.index(of(P::Mod1Source, slot, 3));
            let to = p.index(of(P::Mod1Dest, slot, 3));
            let amount = p.get(of(P::Mod1Amount, slot, 3));
            targets[smoothed::AMOUNT + slot] = amount;
            if from != 0 && to != 0 && from < source::COUNT && to < dest::COUNT {
                d.slot[d.slots] = (from, to);
                d.slot_param[d.slots] = slot;
                d.slots += 1;
            }
        }
        for which in 0..4 {
            targets[smoothed::MACRO + which] = p.get(P::Macro1 as usize + which);
        }

        d.mode = p.index(P::Mode as usize);
        d.voices = p.index(P::Voices as usize).clamp(1, MAX_VOICES);
        let glide = p.get(P::Glide as usize);
        d.glides = glide >= 0.001;
        d.glide_keep = if d.glides { (-3.0 / (glide * sr)).exp() } else { 0.0 };
        d.bend_range = p.get(P::BendRange as usize);
        d.velocity_sens = p.get(P::VelocitySens as usize);
        targets[smoothed::VOLUME] = level_gain(p.get(P::Volume as usize));

        // The first block starts where everything is set; after that a
        // value that is moved takes its 256 frames to get there, and what
        // cannot be moved gradually waits for the gate.
        let started = self.started;
        self.started = true;
        for (smooth, target) in self.shared.smooth.iter_mut().zip(targets) {
            if started {
                smooth.aim(target);
            } else {
                smooth.snap(target);
            }
        }
        let sounding = self.voices.iter().any(|voice| voice.active);
        if started && sounding && (self.pending.is_some() || !next.same_structure(&self.shared.d)) {
            self.pending = Some(next);
        } else {
            self.pending = None;
            self.take_on(next, !started);
        }
        self.params.changed = false;
    }

    /// Takes on new settings. The unison copies go to their new levels at
    /// once if their number changed (`snap`, or a gate that is shut).
    fn take_on(&mut self, next: Derived, snap: bool) {
        for which in 0..2 {
            if snap || next.osc[which].unison != self.shared.d.osc[which].unison {
                self.shared.copies[which] = [next.osc[which].left, next.osc[which].right];
            }
        }
        self.shared.d = next;
    }

    /// Takes note of where the song is at the start of a block. The beat
    /// at a later frame is worked out from the last block at which the
    /// song was somewhere it had not been heading, which is the same
    /// block however the frames are handed over.
    fn follow(&mut self, info: &ProcessInfo) {
        let per_frame = info.tempo / (60.0 * info.sample_rate);
        let expected = self.song.beat + (self.clock - self.song.clock) as f64 * per_frame;
        let per_bar = info.time_signature.beats_per_bar();
        let moved = info.playing != self.song.playing
            || info.tempo != self.song.tempo
            || per_bar != self.song.per_bar
            || (expected - info.position_beats).abs() > 1e-6;
        if moved {
            self.song = Song {
                playing: info.playing,
                tempo: info.tempo,
                per_bar,
                beat: info.position_beats,
                clock: self.clock,
                bar: f64::from(info.song.bar),
                bar_start: info.song.bar_start,
            };
        }
    }

    /// Works out what the next [`TICK`] frames are played with.
    fn tick(&mut self) {
        let sr = self.shared.sample_rate;
        for smooth in &mut self.shared.smooth {
            smooth.tick();
        }
        for which in 0..4 {
            self.shared.sources[source::MACRO + which] = self.shared.smooth[smoothed::MACRO + which].now;
        }

        // What cannot be changed gradually: the gate shuts, the change is
        // made a tick after it has shut, and it opens again.
        let sounding = self.voices.iter().any(|voice| voice.active);
        let step = 1.0 / SMOOTH_TICKS as f32;
        if self.pending.is_some() {
            if !sounding || self.shared.gate <= 0.0 {
                if let Some(next) = self.pending.take() {
                    self.take_on(next, false);
                }
                if !sounding {
                    self.shared.gate = 1.0;
                }
            } else {
                self.shared.gate = (self.shared.gate - step).max(0.0);
            }
        } else if self.shared.gate < 1.0 {
            self.shared.gate = (self.shared.gate + step).min(1.0);
        }
        // The unison copies, on their way to where a blend or a spread
        // that was moved has them.
        for which in 0..2 {
            let setting = &self.shared.d.osc[which];
            for (side, target) in [&setting.left, &setting.right].into_iter().enumerate() {
                for (now, target) in self.shared.copies[which][side].iter_mut().zip(target) {
                    *now += (target - *now) * 0.25;
                }
            }
        }

        // The LFOs: how fast each is, and for one that runs on for all
        // notes, where it is.
        let beat = self.song.beat + (self.clock - self.song.clock) as f64 * self.song.tempo / (60.0 * f64::from(sr));
        for which in 0..3 {
            let lfo = self.shared.d.lfo[which];
            let cycle_beats = lfo.sync.map(|span| match span {
                Span::Beats(beats) => beats,
                Span::Bars(bars) => bars * self.song.per_bar,
            });
            let hz = match cycle_beats {
                Some(beats) => (self.song.tempo / (60.0 * beats)) as f32,
                None => lfo.rate,
            };
            self.shared.lfo_hz[which] = hz;
            if lfo.retrigger {
                continue;
            }
            let locked = cycle_beats.filter(|_| self.song.playing);
            if let (Some(cycle), Some(span)) = (locked, lfo.sync) {
                // In time with the song: where the song is says where the
                // LFO is, so it is the same wherever playing started.
                let cycles = match span {
                    Span::Beats(_) => beat / cycle,
                    Span::Bars(bars) => (self.song.bar + (beat - self.song.bar_start) / self.song.per_bar) / bars,
                } + f64::from(lfo.phase);
                self.lfo_phase[which] = (cycles - cycles.floor()) as f32;
                self.lfo_step[which] = cycles.floor() as i64 as u32;
            } else {
                // What moves its rate is what is the same for all notes.
                let d = &self.shared.d;
                let moved: f32 = (0..d.slots)
                    .filter(|slot| d.slot[*slot].1 == dest::LFO_RATE + which)
                    .map(|slot| {
                        self.shared.smooth[smoothed::AMOUNT + d.slot_param[slot]].now
                            * self.shared.sources[d.slot[slot].0]
                    })
                    .sum();
                let phase = self.lfo_phase[which] + hz * 2f32.powf(5.0 * moved) * TICK as f32 / sr;
                let wraps = phase.floor();
                self.lfo_phase[which] = phase - wraps;
                self.lfo_step[which] = self.lfo_step[which].wrapping_add(wraps as u32);
            }
            self.shared.lfo_value[which] =
                lfo_shape(lfo.shape, self.lfo_phase[which], 0x5EED + which as u32, self.lfo_step[which]);
        }

        let shared = &self.shared;
        for voice in self.voices.iter_mut().filter(|voice| voice.active) {
            voice.control(shared, TICK, false);
        }
    }

    /// Plays the frames up to the next tick, or to the end of `left`.
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len();
        let mut levels = [1.0f32; TICK];
        self.level.fill(&mut levels[..frames], self.shared.sample_rate);
        let shared = &self.shared;
        for voice in self.voices.iter_mut().filter(|voice| voice.active) {
            voice.render(shared, &levels[..frames], left, right);
        }
        self.until_tick -= frames;
        self.clock += frames as u64;
    }

    fn note_on(&mut self, pitch: u8, velocity: f32) {
        self.pedal.key_down(pitch);
        self.notes = self.notes.wrapping_add(1);
        let number = self.notes;
        let (mode, limit, glides) = (self.shared.d.mode, self.shared.d.voices, self.shared.d.glides);
        let sr = self.shared.sample_rate;
        let came_from = self.last_note.filter(|_| glides);
        self.last_note = Some(f32::from(pitch));
        if mode == 0 {
            // A key struck again strikes its own note again.
            let again = self.voices.iter().position(|voice| voice.playing() && voice.pitch == pitch);
            let index = match again {
                Some(index) => index,
                None => {
                    if self.voices.iter().filter(|voice| voice.playing()).count() >= limit
                        && let Some(victim) = self.victim()
                    {
                        self.voices[victim].make_room(sr);
                    }
                    self.free().unwrap_or(0)
                }
            };
            let from = if again.is_some() { None } else { came_from };
            self.voices[index].strike(&self.shared, pitch, velocity, number, from, false);
            return;
        }
        // One note at a time.
        let legato = mode == 2;
        let overlapped = self.held_len > 0;
        self.hold(pitch);
        let sounding = self.voices.iter().position(Voice::playing);
        let index = sounding.or_else(|| self.free()).unwrap_or(0);
        let voice = &mut self.voices[index];
        let carried = legato && overlapped && sounding.is_some() && voice.held;
        let from = match (sounding.is_some(), legato) {
            // Legato slides only between notes that overlap.
            (true, true) => carried.then_some(voice.note).filter(|_| glides),
            (true, false) => Some(voice.note).filter(|_| glides),
            (false, true) => None,
            (false, false) => came_from,
        };
        voice.strike(&self.shared, pitch, velocity, number, from, carried);
    }

    /// The voice to make room: the quietest whose key is up, or the one
    /// struck longest ago.
    fn victim(&self) -> Option<usize> {
        let playing = || self.voices.iter().enumerate().filter(|(_, voice)| voice.playing());
        playing()
            .filter(|(_, voice)| !voice.held)
            .min_by(|a, b| a.1.level().total_cmp(&b.1.level()))
            .or_else(|| playing().min_by_key(|(_, voice)| voice.age))
            .map(|(index, _)| index)
    }

    /// A voice for a new note: one that is silent, or the one furthest on
    /// its way out.
    fn free(&self) -> Option<usize> {
        self.voices.iter().position(|voice| !voice.active).or_else(|| {
            self.voices
                .iter()
                .enumerate()
                .filter(|(_, voice)| voice.fade.fading())
                .min_by(|a, b| a.1.fade.gain().total_cmp(&b.1.fade.gain()))
                .map(|(index, _)| index)
        })
    }

    fn hold(&mut self, pitch: u8) {
        self.unhold(pitch);
        if self.held_len == MAX_HELD {
            self.held.copy_within(1.., 0);
            self.held_len -= 1;
        }
        self.held[self.held_len] = pitch;
        self.held_len += 1;
    }

    fn unhold(&mut self, pitch: u8) {
        if let Some(index) = self.held[..self.held_len].iter().position(|held| *held == pitch) {
            self.held.copy_within(index + 1..self.held_len, index);
            self.held_len -= 1;
        }
    }

    /// A key came up: its note ends, unless the pedal holds it.
    fn note_off(&mut self, pitch: u8) {
        if self.pedal.key_up(pitch) {
            self.release(pitch);
        }
    }

    fn release(&mut self, pitch: u8) {
        if self.shared.d.mode == 0 {
            for voice in &mut self.voices {
                if voice.playing() && voice.held && voice.pitch == pitch {
                    voice.release();
                }
            }
            return;
        }
        self.unhold(pitch);
        let glides = self.shared.d.glides;
        let previous = self.held[..self.held_len].last().copied();
        let Some(voice) = self.voices.iter_mut().find(|voice| voice.playing() && voice.held && voice.pitch == pitch)
        else {
            return;
        };
        match previous {
            // Back to the last key that is still down.
            Some(previous) => {
                voice.pitch = previous;
                voice.target = f32::from(previous);
                if !glides {
                    voice.note = voice.target;
                }
                self.last_note = Some(voice.target);
            }
            None => voice.release(),
        }
    }

    fn all_notes_off(&mut self) {
        self.held_len = 0;
        self.pedal.clear();
        for voice in &mut self.voices {
            voice.release();
        }
    }

    fn handle(&mut self, kind: EventKind) {
        let position = |value: f32, low: f32| if value.is_finite() { value.clamp(low, 1.0) } else { 0.0 };
        match kind {
            EventKind::NoteOn { pitch, velocity } => self.note_on(pitch.min(127), position(velocity, 0.0)),
            EventKind::NoteOff { pitch } => self.note_off(pitch.min(127)),
            EventKind::AllNotesOff => self.all_notes_off(),
            EventKind::PitchBend { value } => {
                self.shared.wheel = position(value, -1.0);
                self.shared.sources[source::PITCH_BEND] = self.shared.wheel;
            }
            EventKind::Control { controller: CONTROLLER_MOD_WHEEL, value } => {
                self.shared.sources[source::MOD_WHEEL] = position(value, 0.0);
            }
            EventKind::Control { controller: CONTROLLER_BREATH, value } => {
                self.shared.sources[source::BREATH] = position(value, 0.0);
            }
            EventKind::Control { controller, value } => {
                if let Some(down) = kind.sustain() {
                    for pitch in self.pedal.set(down) {
                        self.release(pitch);
                    }
                } else {
                    let sounding = self.voices.iter().any(|voice| voice.active);
                    self.level.control(controller, value, sounding);
                }
            }
            EventKind::ChannelPressure { value } => self.shared.sources[source::PRESSURE] = position(value, 0.0),
        }
    }
}

impl DeviceProcessor for Wavetable {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.shared.sample_rate = sample_rate as f32;
        self.voices = (0..MAX_VOICES + SPARE_VOICES).map(|_| Voice::new(sample_rate as f32)).collect();
        self.params.changed = true;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        if self.params.changed {
            self.refresh();
        }
        self.follow(info);
        let len = left.len();
        let mut pos = 0;
        let mut next_event = 0;
        while pos < len {
            // In this order at every frame, wherever a block begins: what
            // the next tick is played with, then what happens at the
            // frame, then the frames.
            if self.until_tick == 0 {
                self.tick();
                self.until_tick = TICK;
            }
            while next_event < events.len() && events[next_event].offset as usize <= pos {
                self.handle(events[next_event].kind);
                next_event += 1;
            }
            let until_event = events.get(next_event).map_or(len, |event| (event.offset as usize).min(len));
            let end = until_event.min(pos + self.until_tick);
            self.render(&mut left[pos..end], &mut right[pos..end]);
            pos = end;
        }
        for event in &events[next_event..] {
            self.handle(event.kind);
        }
    }

    fn reset(&mut self) {
        self.held_len = 0;
        self.pedal.clear();
        self.level.settle();
        for voice in &mut self.voices {
            voice.kill();
        }
        // As a synth that was just made: what is drawn by lot for a note
        // is drawn as it is in a render, which starts from one.
        self.clock = 0;
        self.until_tick = 0;
        self.notes = 0;
        self.last_note = None;
        self.song.clock = 0;
        self.song.playing = false;
        self.lfo_phase = [0.0; 3];
        self.lfo_step = [0; 3];
        self.shared.gate = 1.0;
        self.pending = None;
        self.started = false;
        self.params.changed = true;
    }
}

/// Where an oscillator's own table comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TableSource {
    File(FileSpec),
    Recipe(Recipe),
}

impl TableSource {
    /// What the table is called: its own name, or its file's.
    pub fn name(&self) -> String {
        match self {
            TableSource::Recipe(recipe) if !recipe.name.is_empty() => recipe.name.clone(),
            TableSource::Recipe(_) => "Custom".to_string(),
            TableSource::File(spec) if !spec.name.is_empty() => spec.name.clone(),
            TableSource::File(spec) => {
                let name = trill_core::files::file_name(&spec.file);
                name.rsplit_once('.').map_or(name, |(stem, _)| stem).to_string()
            }
        }
    }

    pub fn check(&self) -> Result<(), String> {
        match self {
            TableSource::Recipe(recipe) => recipe.check(),
            TableSource::File(spec) => spec.check(),
        }
    }

    /// The file the table is read from, if it is read from one.
    pub fn file(&self) -> Option<&str> {
        match self {
            TableSource::File(spec) => Some(&spec.file),
            TableSource::Recipe(_) => None,
        }
    }

    /// The table this describes, and for one from a file how the file was
    /// read; or why there is none, in a sentence. Not for the audio
    /// thread: a table of a few hundred frames takes a moment.
    pub fn make(&self, loader: Option<&dyn AudioLoader>) -> Result<(Table, Option<Reading>), String> {
        self.check()?;
        match self {
            TableSource::Recipe(recipe) => Ok((Table::new(self.name(), &recipe.harmonics()), None)),
            TableSource::File(spec) => {
                let loader = loader.ok_or_else(|| format!("there is nothing to read '{}' with", spec.file))?;
                // As it was recorded: its cycles are so many samples each.
                let audio = loader
                    .load_as_recorded(&spec.file)
                    .map_err(|reason| format!("cannot load '{}': {reason}", spec.file))?;
                let (frames, reading) = spec.harmonics(&audio).map_err(|reason| format!("'{}' {reason}", spec.file))?;
                Ok((Table::new(self.name(), &frames), Some(reading)))
            }
        }
    }
}

/// The synth's state document. See the module documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// What the four macros are called; a name left empty is "Macro 1"...
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macros: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc1: Option<TableSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc2: Option<TableSource>,
    /// Fields written by a build that knows more than this one.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for State {
    fn default() -> Self {
        Self { version: STATE_VERSION, macros: Vec::new(), osc1: None, osc2: None, extra: BTreeMap::new() }
    }
}

/// The slot an oscillator's table file is listed under.
pub const SLOT_NAMES: [&str; 2] = ["osc1 table", "osc2 table"];

impl State {
    /// A state that only names the macros.
    pub fn with_macros(names: &[&str]) -> Self {
        Self { macros: names.iter().map(|name| name.to_string()).collect(), ..Self::default() }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StateError> {
        decode(bytes, STATE_VERSION)
    }

    /// Reads a device's stored state; a device with none has no tables of
    /// its own and macros without names.
    pub fn from_device_state(state: Option<&[u8]>) -> Result<Self, StateError> {
        state.map_or_else(|| Ok(Self::default()), Self::from_bytes)
    }

    /// Oscillator `which`'s own table (0 or 1).
    pub fn table(&self, which: usize) -> Option<&TableSource> {
        if which == 0 { self.osc1.as_ref() } else { self.osc2.as_ref() }
    }

    pub fn table_mut(&mut self, which: usize) -> &mut Option<TableSource> {
        if which == 0 { &mut self.osc1 } else { &mut self.osc2 }
    }

    /// What macro `which` (0..4) is called.
    pub fn macro_name(&self, which: usize) -> String {
        match self.macros.get(which).map(|name| name.trim()) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => format!("Macro {}", which + 1),
        }
    }

    /// Nothing in it: a device with this state is one with none.
    pub fn is_empty(&self) -> bool {
        self.macros.iter().all(|name| name.trim().is_empty())
            && self.osc1.is_none()
            && self.osc2.is_none()
            && self.extra.is_empty()
    }
}

/// The files a stored state refers to: the tables that are read from one.
pub(crate) fn sample_refs(state: Option<&[u8]>) -> Result<Vec<SampleRef>, StateError> {
    let state = State::from_device_state(state)?;
    let files = (0..2).filter_map(|which| match state.table(which) {
        Some(TableSource::File(spec)) => {
            Some(SampleRef { slot: SLOT_NAMES[which].to_string(), path: spec.file.clone() })
        }
        _ => None,
    });
    Ok(files.collect())
}

/// The stored state with the files of the given slots replaced; everything
/// else in the document as it is.
pub(crate) fn relinked(state: Option<&[u8]>, slots: &[(String, String)]) -> Result<Vec<u8>, StateError> {
    let mut state = State::from_device_state(state)?;
    for (slot, path) in slots {
        let which = SLOT_NAMES.iter().position(|name| name == slot);
        match which.and_then(|which| state.table_mut(which).as_mut()) {
            Some(TableSource::File(spec)) => spec.file = path.clone(),
            _ => return Err(StateError::Invalid(format!("a wavetable synth has no file in '{slot}'"))),
        }
    }
    Ok(state.to_bytes())
}

/// Tables made for devices, shared between the devices that have the same
/// one: every render creates each device again, and a table of a few
/// hundred frames takes a moment to make. Held weakly: a table is freed
/// with the last device that plays it.
#[derive(Default)]
pub(crate) struct TableStore {
    made: Mutex<HashMap<String, Made>>,
}

/// The audio a table was made from, if it was made from any, and the
/// table.
type Made = (Option<Weak<AudioBuffer>>, Weak<Table>);

impl TableStore {
    fn get(&self, key: &str, audio: Option<&Arc<AudioBuffer>>) -> Option<Arc<Table>> {
        let made = self.made.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (from, table) = made.get(key)?;
        // A file that changed on disk is another buffer, and another table.
        let same = match (from, audio) {
            (None, None) => true,
            (Some(from), Some(audio)) => from.upgrade().is_some_and(|from| Arc::ptr_eq(&from, audio)),
            _ => false,
        };
        table.upgrade().filter(|_| same)
    }

    fn keep(&self, key: String, audio: Option<&Arc<AudioBuffer>>, table: &Arc<Table>) {
        let mut made = self.made.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        made.retain(|_, (_, table)| table.strong_count() > 0);
        made.insert(key, (audio.map(Arc::downgrade), Arc::downgrade(table)));
    }
}

/// The table `source` describes, with what is wrong with it if it could
/// not be made: `Ok(None)` is a table that is not to be had (its file is
/// gone, or is no table), which the device plays without.
fn make_table(table: &TableSource, source: &SampleSource) -> Result<(Option<Arc<Table>>, Option<String>), String> {
    let key = serde_json::to_string(table).unwrap_or_default();
    match table {
        TableSource::Recipe(recipe) => {
            if let Some(found) = source.tables.and_then(|tables| tables.get(&key, None)) {
                return Ok((Some(found), None));
            }
            let made = Arc::new(Table::new(table.name(), &recipe.harmonics()));
            if let Some(tables) = source.tables {
                tables.keep(key, None, &made);
            }
            Ok((Some(made), recipe.check().err()))
        }
        TableSource::File(spec) => {
            let loader = source.loader.ok_or_else(|| {
                format!(
                    "it reads a wavetable from '{}', but this device factory was created without an audio loader \
                     (use trill_dsp::Builtins::with_loader)",
                    spec.file
                )
            })?;
            let audio = match loader.load_as_recorded(&spec.file) {
                Ok(audio) => audio,
                Err(reason) => return Ok((None, Some(format!("cannot load '{}': {reason}", spec.file)))),
            };
            if let Some(found) = source.tables.and_then(|tables| tables.get(&key, Some(&audio))) {
                return Ok((Some(found), None));
            }
            match spec.harmonics(&audio) {
                Ok((frames, _)) => {
                    let made = Arc::new(Table::new(table.name(), &frames));
                    if let Some(tables) = source.tables {
                        tables.keep(key, Some(&audio), &made);
                    }
                    Ok((Some(made), None))
                }
                Err(reason) => Ok((None, Some(format!("'{}' {reason}", spec.file)))),
            }
        }
    }
}

/// Creates a synth from a stored state, making its own tables.
pub(crate) fn create(state: Option<&[u8]>, source: &SampleSource) -> Result<Box<dyn DeviceProcessor>, String> {
    let state = State::from_device_state(state).unwrap_or_default();
    let mut custom = [None, None];
    for (which, slot) in custom.iter_mut().enumerate() {
        if let Some(table) = state.table(which) {
            *slot = make_table(table, source)?.0;
        }
    }
    Ok(Box::new(Wavetable::new(custom)))
}

/// What is wrong with the tables `device` has of its own: one that cannot
/// be made, and an oscillator set to a table of its own that has none.
pub(crate) fn warnings(device: &Device, source: &SampleSource) -> Vec<DeviceWarning> {
    let state = match State::from_device_state(device.state.as_deref()) {
        Ok(state) => state,
        Err(error) => {
            let message = format!("{error}; the synth plays without tables of its own until they are set again");
            return vec![DeviceWarning { slot: None, message }];
        }
    };
    let mut warnings = Vec::new();
    for which in 0..2 {
        let slot = SLOT_NAMES[which];
        let chosen = device.params.get(PARAMS[of(P::Osc1Table, which, OSC_PARAMS)].id).copied();
        match state.table(which) {
            Some(table) => {
                if let Ok((_, Some(problem))) = make_table(table, source) {
                    warnings
                        .push(DeviceWarning { slot: Some(slot.to_string()), message: format!("{slot}: {problem}") });
                }
            }
            None if chosen == Some(0.0) => warnings.push(DeviceWarning {
                slot: Some(slot.to_string()),
                message: format!(
                    "{slot}: oscillator {} is set to a table of its own and the device has none, so it plays \
                     '{}' until one is made for it",
                    which + 1,
                    factory::TABLES[0].name
                ),
            }),
            None => {}
        }
    }
    warnings
}

pub const DESCRIPTION: &str = "Wavetable synth, and the built-in instrument to shape a sound with: every part of \
it is a parameter that says what it does. Two oscillators each read a table of waves (`osc1_table`: 22 ship, see \
describe_wavetable; set_wavetable makes one from a recipe of shapes or harmonics, or from an audio file) at a \
`position` that moves through the table, which is where the movement in a wavetable sound comes from: send an \
envelope or an LFO to `osc1_position`. Each has unison up to 16, a warp (sync, bend, squeeze) and its own tuning; \
oscillator 2 can bend oscillator 1's phase (`osc1_fm`). Sub oscillator, noise, two filters (12/24 dB low, band, \
high, notch, comb; serial, parallel or split), three envelopes (envelope 1 is the note's level), three LFOs (free \
or in time with the song). Nothing is routed that the matrix does not say: sixteen slots, each `modN_source` x \
`modN_amount` -> `modN_dest`. A filter sweep is env2 -> filter1_cutoff at 0.4 (four octaves), vibrato lfo1 -> \
fine_pitch at 0.2, brightness by touch velocity -> osc1_position. Four macros (`macro1`..`macro4`) are handles a \
preset names: get_device shows the names, and moving one moves what the matrix sends it to. Poly, mono or legato \
with glide. Start from a preset (search_presets) and change what you need; a preset's description says what its \
macros do. Tables are band-limited at every pitch and the same song renders to the same samples every time. It \
has no effects inside: a preset that wants them brings them as devices behind it (its leads, pads, plucks and keys \
do), and to a sound of your own you add builtin:chorus, builtin:delay or builtin:reverb.";

#[cfg(test)]
mod tests;
