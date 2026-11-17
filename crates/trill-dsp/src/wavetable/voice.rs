//! One note of the wavetable synth: its oscillators, its filters, and what
//! moves them.
//!
//! A voice is told where everything is to be once every [`TICK`] frames
//! ([`Voice::control`]), counted on the synth's own clock, and goes there
//! in a line, frame by frame ([`Glide`]). So nothing a note does depends
//! on where a buffer or an event divides the frames, and nothing steps.

// The loops here run over a few frames of several buffers at once, by
// their number.
#![allow(clippy::needless_range_loop, clippy::manual_memcpy)]

use super::Shared;
use super::params::{MAX_UNISON, SLOTS};
use super::table::{Reader, Table};
use crate::util::{Rng, midi_to_hz, pan_gains};
use crate::voice::{Envelope, FadeOut, Stage};
use std::f32::consts::{PI, TAU};

/// Frames between two readings of what moves a voice: a third of a
/// millisecond at 48 kHz.
pub const TICK: usize = 16;
/// How quickly a voice that has to make room gets out of the way.
const STEAL_SECONDS: f32 = 0.005;
/// How far oscillator 2 bends the phase of oscillator 1 at an FM of 1, in
/// cycles each way.
const FM_CYCLES: f32 = 1.5;
/// The level of the noise at a noise level of 1, against the peak of an
/// oscillator: as the poly synth's.
const NOISE_GAIN: f32 = 0.223_87;
/// The lowest note a comb filter can be tuned to, in Hz: its delay is as
/// long as one cycle of it.
pub const COMB_LOWEST_HZ: f32 = 20.0;

/// What a slot of the matrix reads, in the order of
/// [`SOURCES`](super::params::SOURCES).
pub mod source {
    pub const ENV: usize = 1;
    pub const LFO: usize = 4;
    pub const VELOCITY: usize = 7;
    pub const KEY: usize = 8;
    pub const MOD_WHEEL: usize = 9;
    pub const PITCH_BEND: usize = 10;
    pub const PRESSURE: usize = 11;
    pub const BREATH: usize = 12;
    pub const MACRO: usize = 13;
    pub const RANDOM: usize = 17;
    pub const COUNT: usize = 18;
}

/// What a slot of the matrix moves, in the order of
/// [`DESTINATIONS`](super::params::DESTINATIONS).
pub mod dest {
    pub const PITCH: usize = 1;
    pub const FINE_PITCH: usize = 2;
    pub const OSC_PITCH: usize = 3;
    pub const OSC_POSITION: usize = 5;
    pub const OSC_WARP: usize = 7;
    pub const OSC1_FM: usize = 9;
    pub const OSC_LEVEL: usize = 10;
    pub const OSC_PAN: usize = 12;
    pub const SUB_LEVEL: usize = 14;
    pub const NOISE_LEVEL: usize = 15;
    pub const CUTOFF: usize = 16;
    pub const RESONANCE: usize = 18;
    pub const DRIVE: usize = 20;
    pub const BALANCE: usize = 22;
    pub const LEVEL: usize = 23;
    pub const PAN: usize = 24;
    pub const LFO_RATE: usize = 25;
    pub const LFO_DEPTH: usize = 28;
    /// Of envelopes 2 and 3.
    pub const ENV_DEPTH: usize = 31;
    pub const COUNT: usize = 33;
}

/// A value on its way to where it was last told to be, a frame at a time.
#[derive(Copy, Clone, Default)]
pub struct Glide {
    now: f32,
    step: f32,
}

impl Glide {
    /// Puts the value there, now.
    #[inline]
    pub fn snap(&mut self, value: f32) {
        self.now = value;
        self.step = 0.0;
    }

    /// Sets off for `target`, to be there a tick from now.
    #[inline]
    pub fn aim(&mut self, target: f32) {
        self.step = (target - self.now) * (1.0 / TICK as f32);
    }

    /// The value for the next frame.
    #[inline(always)]
    pub fn next(&mut self) -> f32 {
        let value = self.now;
        self.now += self.step;
        value
    }

    #[inline]
    pub fn now(&self) -> f32 {
        self.now
    }

    /// Whether it stands at `value` and is going nowhere.
    #[inline]
    fn rests_at(&self, value: f32) -> bool {
        self.now == value && self.step == 0.0
    }
}

/// How an oscillator's cycle is bent before it is read.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Warp {
    #[default]
    Off,
    Sync,
    Bend,
    Squeeze,
}

impl Warp {
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Sync,
            2 => Self::Bend,
            3 => Self::Squeeze,
            _ => Self::Off,
        }
    }

    /// How many times as fast as the note the cycle is read at its
    /// fastest, at `amount`: what the brightness an oscillator may read
    /// is chosen by.
    #[inline]
    pub fn speed(self, amount: f32) -> f32 {
        match self {
            Warp::Off => 1.0,
            Warp::Sync => sync_ratio(amount),
            Warp::Bend => 1.0 + bend_lean(amount),
            Warp::Squeeze => 1.0 / squeeze_width(amount),
        }
    }
}

/// How many times a cycle is restarted at a sync of `amount`: 1 to 16.
#[inline(always)]
fn sync_ratio(amount: f32) -> f32 {
    1.0 + 15.0 * amount * amount
}

/// How much faster than the note the middle of the cycle is read at a
/// bend of `amount`, and how much slower its ends: not at all with none,
/// eight times at the most.
#[inline(always)]
fn bend_lean(amount: f32) -> f32 {
    8.0 * amount * amount
}

/// Where in the cycle a period that is `phase` of the way through reads
/// at a bend that leans by `lean`: slowly at its start, fast through its
/// middle and slowly to its end, along a curve with no corner in it (each
/// half is the mirror of the other).
#[inline(always)]
fn bent(phase: f32, lean: f32) -> f32 {
    if phase < 0.5 {
        phase / (lean * (1.0 - 2.0 * phase) + 1.0)
    } else {
        1.0 - (1.0 - phase) / (lean * (2.0 * phase - 1.0) + 1.0)
    }
}

/// The share of the period the whole cycle is read in at a squeeze of
/// `amount`: all of it with none, a tenth at the most.
#[inline(always)]
fn squeeze_width(amount: f32) -> f32 {
    1.0 - 0.9 * amount
}

/// What an oscillator is set to that a note does not change.
#[derive(Clone)]
pub struct OscSetting {
    /// The table as it is chosen: its number among the names.
    pub table: usize,
    pub warp: Warp,
    /// Octave, semitones and cents, in semitones.
    pub tune: f32,
    pub unison: usize,
    /// Each copy's pitch against the note's, as a ratio, and the highest.
    pub ratio: [f32; MAX_UNISON],
    pub highest: f32,
    pub left: [f32; MAX_UNISON],
    pub right: [f32; MAX_UNISON],
    pub phase: f32,
    pub phase_random: f32,
}

impl Default for OscSetting {
    fn default() -> Self {
        Self {
            table: 1,
            warp: Warp::Off,
            tune: 0.0,
            unison: 1,
            ratio: [1.0; MAX_UNISON],
            highest: 1.0,
            left: [1.0; MAX_UNISON],
            right: [1.0; MAX_UNISON],
            phase: 0.0,
            phase_random: 1.0,
        }
    }
}

impl OscSetting {
    /// Lays out `unison` copies: how far each is from the note (`detune`
    /// cents at the outermost), how loud against the one in tune
    /// (`blend`) and where between the sides (`spread`).
    pub fn lay_out(&mut self, unison: usize, detune: f32, blend: f32, spread: f32) {
        let unison = unison.clamp(1, MAX_UNISON);
        self.unison = unison;
        let pairs = unison / 2;
        let odd = unison % 2 == 1;
        let mut weights = [0.0f32; MAX_UNISON];
        let mut place = [0.0f32; MAX_UNISON];
        let mut side = [0.0f32; MAX_UNISON];
        let mut next = 0;
        if odd {
            weights[0] = 1.0;
            next = 1;
        }
        for pair in 0..pairs {
            // Closer together near the note than far from it, and not
            // quite the same on either side, so that the copies do not
            // beat in step with one another.
            let reach = if odd {
                (pair + 1) as f32 / pairs as f32
            } else if pairs == 1 {
                1.0
            } else {
                (pair as f32 + 0.5) / (pairs as f32 - 0.5)
            };
            let reach = reach.powf(1.5);
            let innermost = !odd && pair == 0;
            let weight = if innermost { 1.0 } else { blend };
            // Which side the higher of the two goes to changes with every
            // pair, so that neither side is the sharp one.
            let lean = if pair % 2 == 0 { 1.0 } else { -1.0 };
            let across = if pairs == 1 && !odd { 1.0 } else { (pair + 1) as f32 / pairs as f32 };
            for (sign, skew) in [(1.0f32, 1.0f32), (-1.0, 0.97)] {
                place[next] = sign * skew * reach;
                weights[next] = weight;
                side[next] = sign * lean * across;
                next += 1;
            }
        }
        let power: f32 = weights[..unison].iter().map(|weight| weight * weight).sum();
        let norm = 1.0 / power.max(1e-9).sqrt();
        self.highest = 1.0;
        for copy in 0..unison {
            self.ratio[copy] = 2f32.powf(place[copy] * detune / 1200.0);
            self.highest = self.highest.max(self.ratio[copy]);
            let (left, right) = pan_gains(side[copy] * spread);
            self.left[copy] = left * weights[copy] * norm;
            self.right[copy] = right * weights[copy] * norm;
        }
    }
}

/// An oscillator of one voice.
#[derive(Copy, Clone, Default)]
pub struct OscVoice {
    /// Where in its cycle each copy is, the cycle being the whole range of
    /// the number: counted so, a note keeps its pitch to the last bit and
    /// the counting adds no noise of its own.
    phase: [u32; MAX_UNISON],
    /// Cycles a frame, of the copy in tune.
    increment: f32,
    /// The brightness of the table it reads.
    level_of_table: usize,
    position: Glide,
    warp: Glide,
    level: Glide,
    left: Glide,
    right: Glide,
}

/// What kind of filter a filter is.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FilterKind {
    #[default]
    Off,
    LowPass12,
    LowPass24,
    BandPass12,
    BandPass24,
    HighPass12,
    HighPass24,
    Notch12,
    Notch24,
    Comb,
    CombMinus,
}

impl FilterKind {
    pub fn from_index(index: usize) -> Self {
        use FilterKind::*;
        [Off, LowPass12, LowPass24, BandPass12, BandPass24, HighPass12, HighPass24, Notch12, Notch24, Comb, CombMinus]
            .get(index)
            .copied()
            .unwrap_or(Off)
    }

    pub fn is_comb(self) -> bool {
        matches!(self, FilterKind::Comb | FilterKind::CombMinus)
    }
}

/// One stage of a state-variable filter, on one channel.
#[derive(Copy, Clone, Default)]
struct Stage2 {
    ic1: f32,
    ic2: f32,
}

impl Stage2 {
    /// Low, band and high for `input`, with the coefficients of this
    /// frame.
    #[inline(always)]
    fn tick(&mut self, input: f32, k: f32, a1: f32, a2: f32, a3: f32) -> (f32, f32, f32) {
        let v3 = input - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, v1, input - k * v1 - v2)
    }
}

/// A filter of one voice.
pub struct FilterVoice {
    /// The cutoff as the filter's own coefficient; for a comb, its delay
    /// in frames.
    tuning: Glide,
    /// The damping, one over the Q; for a comb, its feedback.
    k: f32,
    /// How hard the sound is pushed into the saturation, and how much of
    /// what comes out is the saturated sound.
    drive: f32,
    driven: f32,
    stages: [[Stage2; 2]; 2],
    /// A comb's delay lines, one a side, and where they are written next.
    lines: [Vec<f32>; 2],
    write: usize,
    /// Whether the lines hold anything: they are cleared for a new note
    /// only if they do.
    rings: bool,
    /// The kind of filter it was last tuned as: one that becomes another
    /// kind starts as that kind, with nothing left in it of the last.
    kind: FilterKind,
}

impl FilterVoice {
    fn new(line: usize) -> Self {
        Self {
            tuning: Glide::default(),
            k: 2.0,
            drive: 1.0,
            driven: 0.0,
            stages: [[Stage2::default(); 2]; 2],
            lines: [vec![0.0; line], vec![0.0; line]],
            write: 0,
            rings: false,
            kind: FilterKind::Off,
        }
    }

    fn clear(&mut self) {
        self.stages = [[Stage2::default(); 2]; 2];
        if self.rings {
            self.lines[0].fill(0.0);
            self.lines[1].fill(0.0);
            self.rings = false;
        }
    }

    /// Runs `left` and `right` through the filter, in place.
    fn run(&mut self, kind: FilterKind, left: &mut [f32], right: &mut [f32]) {
        if kind == FilterKind::Off {
            return;
        }
        if self.driven > 0.0 {
            let (drive, driven) = (self.drive, self.driven);
            // Louder going in, and a little quieter coming out: a sound
            // that fills the range comes out about as loud as it was, and
            // a quiet one louder.
            let makeup = driven / drive.powf(0.25);
            for sample in left.iter_mut().chain(right.iter_mut()) {
                *sample = *sample * (1.0 - driven) + saturate(*sample * drive) * makeup;
            }
        }
        let k = self.k;
        match kind {
            FilterKind::Off => {}
            FilterKind::Comb | FilterKind::CombMinus => self.comb(kind == FilterKind::CombMinus, left, right),
            FilterKind::LowPass12 => self.one(k, left, right, |low, _, _| low),
            FilterKind::BandPass12 => self.one(k, left, right, |_, band, _| band * k.sqrt()),
            FilterKind::HighPass12 => self.one(k, left, right, |_, _, high| high),
            FilterKind::Notch12 => self.one(k, left, right, |low, _, high| low + high),
            // The second stage of a low-pass or a high-pass is a gentle
            // one, so that the resonance is the first stage's alone.
            FilterKind::LowPass24 => self.two(k, std::f32::consts::SQRT_2, left, right, |low, _, _| low),
            FilterKind::HighPass24 => self.two(k, std::f32::consts::SQRT_2, left, right, |_, _, high| high),
            FilterKind::BandPass24 => self.two(k, k, left, right, |_, band, _| band * k.sqrt()),
            FilterKind::Notch24 => self.two(k, k, left, right, |low, _, high| low + high),
        }
    }

    #[inline(always)]
    fn one(&mut self, k: f32, left: &mut [f32], right: &mut [f32], pick: impl Fn(f32, f32, f32) -> f32) {
        let [first, _] = &mut self.stages;
        for (left, right) in left.iter_mut().zip(right.iter_mut()) {
            let g = self.tuning.next();
            let a1 = 1.0 / (1.0 + g * (g + k));
            let (a2, a3) = (g * a1, g * g * a1);
            let (low, band, high) = first[0].tick(*left, k, a1, a2, a3);
            *left = pick(low, band, high);
            let (low, band, high) = first[1].tick(*right, k, a1, a2, a3);
            *right = pick(low, band, high);
        }
    }

    #[inline(always)]
    fn two(&mut self, k: f32, k2: f32, left: &mut [f32], right: &mut [f32], pick: impl Fn(f32, f32, f32) -> f32) {
        let [first, second] = &mut self.stages;
        for (left, right) in left.iter_mut().zip(right.iter_mut()) {
            let g = self.tuning.next();
            let a1 = 1.0 / (1.0 + g * (g + k));
            let (a2, a3) = (g * a1, g * g * a1);
            let b1 = 1.0 / (1.0 + g * (g + k2));
            let (b2, b3) = (g * b1, g * g * b1);
            let (low, band, high) = first[0].tick(*left, k, a1, a2, a3);
            let (low, band, high) = second[0].tick(pick(low, band, high), k2, b1, b2, b3);
            *left = pick(low, band, high);
            let (low, band, high) = first[1].tick(*right, k, a1, a2, a3);
            let (low, band, high) = second[1].tick(pick(low, band, high), k2, b1, b2, b3);
            *right = pick(low, band, high);
        }
    }

    fn comb(&mut self, minus: bool, left: &mut [f32], right: &mut [f32]) {
        let feedback = if minus { -self.k } else { self.k };
        // As loud as it was for a sound with something everywhere.
        let level = (1.0 - feedback * feedback).max(0.0).sqrt();
        let mask = self.lines[0].len() - 1;
        let [line_left, line_right] = &mut self.lines;
        self.rings = true;
        for (left, right) in left.iter_mut().zip(right.iter_mut()) {
            let delay = self.tuning.next().clamp(1.0, mask as f32 - 1.0);
            let whole = delay as usize;
            let between = delay - whole as f32;
            let (near, far) = (self.write.wrapping_sub(whole) & mask, self.write.wrapping_sub(whole + 1) & mask);
            let rung = line_left[near] + (line_left[far] - line_left[near]) * between;
            let out = *left + feedback * rung;
            line_left[self.write] = out;
            *left = out * level;
            let rung = line_right[near] + (line_right[far] - line_right[near]) * between;
            let out = *right + feedback * rung;
            line_right[self.write] = out;
            *right = out * level;
            self.write = (self.write + 1) & mask;
        }
    }
}

/// A saturation that is the sound itself while it is quiet and never
/// passes full level.
#[inline(always)]
fn saturate(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

/// A number drawn from two others, the same every time: what is drawn by
/// lot for a note is drawn from the note.
#[inline]
pub fn lot(a: u32, b: u32) -> u32 {
    let mut x = a.wrapping_mul(0x9E37_79B9) ^ b.wrapping_add(0x7F4A_7C15).wrapping_mul(0x85EB_CA6B);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^ (x >> 16)
}

/// The same, as a value from -1 to 1.
#[inline]
fn lot_bipolar(a: u32, b: u32) -> f32 {
    (lot(a, b) >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
}

/// What an LFO puts out at `phase` of its cycle `step`, from -1 to 1.
#[inline]
pub fn lfo_shape(shape: usize, phase: f32, seed: u32, step: u32) -> f32 {
    match shape {
        0 => (TAU * phase).sin(),
        1 => {
            if phase < 0.25 {
                4.0 * phase
            } else if phase < 0.75 {
                2.0 - 4.0 * phase
            } else {
                4.0 * phase - 4.0
            }
        }
        2 => 2.0 * phase - 1.0,
        3 => 1.0 - 2.0 * phase,
        4 => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        5 => lot_bipolar(seed, step),
        _ => {
            let (from, to) = (lot_bipolar(seed, step), lot_bipolar(seed, step.wrapping_add(1)));
            let eased = phase * phase * (3.0 - 2.0 * phase);
            from + (to - from) * eased
        }
    }
}

/// One note.
pub struct Voice {
    pub active: bool,
    /// The key is down, or the pedal holds it.
    pub held: bool,
    pub pitch: u8,
    /// Where the pitch is, in semitones, on its way to `target`.
    pub note: f32,
    pub target: f32,
    velocity: f32,
    velocity_gain: f32,
    /// When it was last struck, among the notes of this synth.
    pub age: u64,
    /// Frames since it was struck, for the LFOs' fades.
    frames: u32,
    seed: u32,
    random: f32,
    noise: Rng,
    env: [Envelope; 3],
    lfo_phase: [f32; 3],
    lfo_step: [u32; 3],
    /// What the matrix last made of each LFO's rate and depth: an LFO is
    /// worked out before the matrix that moves it, so it hears of it a
    /// tick late.
    lfo_rate: [f32; 3],
    lfo_depth: [f32; 3],
    /// The same for how much envelopes 2 and 3 send.
    env_depth: [f32; 2],
    osc: [OscVoice; 2],
    fm: Glide,
    sub_phase: f32,
    sub_increment: f32,
    sub_level: Glide,
    noise_level: Glide,
    noise_state: [f32; 2],
    filter: [FilterVoice; 2],
    balance: [Glide; 2],
    gain: Glide,
    left: Glide,
    right: Glide,
    pub fade: FadeOut,
}

impl Voice {
    /// A voice whose comb filters have room for a cycle of
    /// [`COMB_LOWEST_HZ`] at `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        let line = ((sample_rate / COMB_LOWEST_HZ) as usize + 4).next_power_of_two();
        Self {
            active: false,
            held: false,
            pitch: 0,
            note: 60.0,
            target: 60.0,
            velocity: 0.0,
            velocity_gain: 0.0,
            age: 0,
            frames: 0,
            seed: 1,
            random: 0.0,
            noise: Rng::new(1),
            env: [Envelope::idle(); 3],
            lfo_phase: [0.0; 3],
            lfo_step: [0; 3],
            lfo_rate: [0.0; 3],
            lfo_depth: [0.0; 3],
            env_depth: [0.0; 2],
            osc: [OscVoice::default(); 2],
            fm: Glide::default(),
            sub_phase: 0.0,
            sub_increment: 0.0,
            sub_level: Glide::default(),
            noise_level: Glide::default(),
            noise_state: [0.0; 2],
            filter: [FilterVoice::new(line), FilterVoice::new(line)],
            balance: [Glide::default(); 2],
            gain: Glide::default(),
            left: Glide::default(),
            right: Glide::default(),
            fade: FadeOut::idle(),
        }
    }

    pub fn kill(&mut self) {
        self.active = false;
        self.held = false;
        self.env = [Envelope::idle(); 3];
        self.fade = FadeOut::idle();
    }

    /// Sounding as a note in its own right, not on its way out to make
    /// room for another.
    pub fn playing(&self) -> bool {
        self.active && !self.fade.fading()
    }

    /// How loud the note's own envelope has it.
    pub fn level(&self) -> f32 {
        self.env[0].level
    }

    pub fn release(&mut self) {
        self.held = false;
        for env in &mut self.env {
            env.release();
        }
    }

    /// Gets out of the way of another note, in a few milliseconds.
    pub fn make_room(&mut self, sample_rate: f32) {
        self.held = false;
        self.fade.start(STEAL_SECONDS, sample_rate);
    }

    /// Strikes the note: `number` is which note of the synth's it is, by
    /// which everything about it that is drawn by lot is drawn. A voice
    /// that is sounding carries on from where it is (`legato` leaves its
    /// envelopes alone as well); one that is not starts from nothing, at
    /// `from` if the pitch is to glide.
    pub fn strike(&mut self, shared: &Shared, pitch: u8, velocity: f32, number: u32, from: Option<f32>, legato: bool) {
        let fresh = !self.active || self.fade.fading();
        self.pitch = pitch;
        self.target = f32::from(pitch);
        self.velocity = velocity;
        let sens = shared.d.velocity_sens;
        self.velocity_gain = 1.0 - sens + sens * velocity * velocity;
        self.held = true;
        self.age = u64::from(number);
        if fresh {
            self.seed = lot(u32::from(pitch), number);
            self.random = lot_bipolar(self.seed, 1);
            self.noise = Rng::new(lot(self.seed, 2));
            self.note = from.unwrap_or(self.target);
            self.env = [Envelope::idle(); 3];
            self.fade = FadeOut::idle();
            for (which, osc) in self.osc.iter_mut().enumerate() {
                let setting = &shared.d.osc[which];
                for copy in 0..MAX_UNISON {
                    let drawn = (lot(self.seed, 16 + (which * MAX_UNISON + copy) as u32) >> 8) as f32 / 16_777_216.0;
                    let phase = setting.phase + drawn * setting.phase_random;
                    osc.phase[copy] = counted(phase - phase.floor());
                }
            }
            self.sub_phase = 0.0;
            self.noise_state = [0.0; 2];
            for filter in &mut self.filter {
                filter.clear();
            }
            self.lfo_rate = [0.0; 3];
            self.lfo_depth = [0.0; 3];
            self.env_depth = [0.0; 2];
        } else if from.is_none() && !shared.d.glides {
            self.note = self.target;
        }
        self.active = true;
        if fresh || !legato {
            for env in &mut self.env {
                env.stage = Stage::Attack;
            }
            self.frames = 0;
            for which in 0..3 {
                if shared.d.lfo[which].retrigger {
                    self.lfo_phase[which] = shared.d.lfo[which].phase;
                    self.lfo_step[which] = 0;
                }
            }
        }
        if fresh {
            // Everything stands where it belongs from the first frame; a
            // matrix that moves an LFO needs a second look to know of it.
            self.control(shared, 0, true);
            self.control(shared, 0, true);
        }
    }

    /// Works out where everything is to be a tick from now, `frames`
    /// frames after the last time: the envelopes and LFOs are moved on by
    /// that much, the matrix is read, and every value that glides is sent
    /// on its way (or put there, with `snap`).
    pub fn control(&mut self, shared: &Shared, frames: usize, snap: bool) {
        let d = &shared.d;
        let sample_rate = shared.sample_rate;
        // The envelopes that move things are read where they are now; the
        // note's own is ticked with every frame it plays.
        for _ in 0..frames {
            self.env[1].tick(&d.env[1]);
            self.env[2].tick(&d.env[2]);
        }
        self.frames = self.frames.saturating_add(frames as u32);

        let mut sources = [0.0f32; source::COUNT];
        sources[..source::COUNT].copy_from_slice(&shared.sources);
        sources[source::ENV] = self.env[0].level;
        for which in 1..3 {
            sources[source::ENV + which] = self.env[which].level * (1.0 + self.env_depth[which - 1]).clamp(0.0, 2.0);
        }
        sources[source::VELOCITY] = self.velocity;
        sources[source::KEY] = ((self.note - 60.0) / 48.0).clamp(-1.0, 1.0);
        sources[source::RANDOM] = self.random;
        for which in 0..3 {
            let lfo = &d.lfo[which];
            let value = if lfo.retrigger {
                let rate = shared.lfo_hz[which] * 2f32.powf(5.0 * self.lfo_rate[which]);
                let phase = self.lfo_phase[which] + rate * frames as f32 / sample_rate;
                let wraps = phase.floor();
                self.lfo_phase[which] = phase - wraps;
                self.lfo_step[which] = self.lfo_step[which].wrapping_add(wraps as u32);
                lfo_shape(lfo.shape, self.lfo_phase[which], lot(self.seed, 3 + which as u32), self.lfo_step[which])
            } else {
                shared.lfo_value[which]
            };
            let fade = if lfo.fade_frames > 0.0 { (self.frames as f32 / lfo.fade_frames).min(1.0) } else { 1.0 };
            let depth = (lfo.depth + self.lfo_depth[which]).clamp(0.0, 1.0);
            sources[source::LFO + which] = value * depth * fade;
        }

        let mut moved = [0.0f32; dest::COUNT];
        for slot in 0..d.slots {
            let (from, to) = d.slot[slot];
            moved[to] += shared.smooth[super::smoothed::AMOUNT + d.slot_param[slot]].now * sources[from];
        }
        for which in 0..3 {
            self.lfo_rate[which] = moved[dest::LFO_RATE + which];
            self.lfo_depth[which] = moved[dest::LFO_DEPTH + which];
        }
        self.env_depth = [moved[dest::ENV_DEPTH], moved[dest::ENV_DEPTH + 1]];

        // The pitch slides to where it is going, by a share of the way
        // that is the same however the frames are counted.
        if frames > 0 {
            self.note = self.target + (self.note - self.target) * d.glide_keep.powi(frames as i32);
        }
        let bend = shared.wheel * d.bend_range + 48.0 * moved[dest::PITCH] + moved[dest::FINE_PITCH];
        let value = |index: usize| shared.smooth[index].now;
        let aim = |glide: &mut Glide, target: f32| if snap { glide.snap(target) } else { glide.aim(target) };

        let fm = (value(super::smoothed::FM) + moved[dest::OSC1_FM]).clamp(0.0, 1.0) * FM_CYCLES;
        aim(&mut self.fm, fm);
        let mut increments = [0.0f32; 2];
        for which in 0..2 {
            let setting = &d.osc[which];
            let base = super::smoothed::OSC + which * super::smoothed::OSC_VALUES;
            let osc = &mut self.osc[which];
            let semitones = self.note + setting.tune + bend + 48.0 * moved[dest::OSC_PITCH + which];
            osc.increment = (midi_to_hz(semitones) / sample_rate).clamp(1e-7, 0.49);
            increments[which] = osc.increment;
            aim(&mut osc.level, (value(base) + moved[dest::OSC_LEVEL + which]).clamp(0.0, 1.0));
            aim(&mut osc.position, (value(base + 1) + moved[dest::OSC_POSITION + which]).clamp(0.0, 1.0));
            let warp = (value(base + 2) + moved[dest::OSC_WARP + which]).clamp(0.0, 1.0);
            let fastest = setting.warp.speed(warp.max(osc.warp.now()));
            aim(&mut osc.warp, warp);
            let (left, right) = pan_gains((value(base + 3) + moved[dest::OSC_PAN + which]).clamp(-1.0, 1.0));
            aim(&mut osc.left, left);
            aim(&mut osc.right, right);
            osc.level_of_table = Table::level_for(osc.increment * setting.highest * fastest);
        }
        // What bends oscillator 1's phase makes it faster in places: by
        // as much as a sine at oscillator 2's pitch would.
        let bent = fm.max(self.fm.now()) * TAU * increments[1] * d.osc[1].highest;
        if bent > 0.0 {
            let osc = &mut self.osc[0];
            let fastest = d.osc[0].warp.speed(osc.warp.now().max(osc.warp.now() + osc.warp.step * TICK as f32));
            osc.level_of_table = Table::level_for(osc.increment * d.osc[0].highest * fastest + bent);
        }

        self.sub_increment = (midi_to_hz(self.note + bend + d.sub_tune) / sample_rate).clamp(1e-7, 0.49);
        aim(&mut self.sub_level, (value(super::smoothed::SUB) + moved[dest::SUB_LEVEL]).clamp(0.0, 1.0));
        let noise = (value(super::smoothed::NOISE) + moved[dest::NOISE_LEVEL]).clamp(0.0, 1.0) * NOISE_GAIN;
        aim(&mut self.noise_level, noise);

        for which in 0..2 {
            let base = super::smoothed::FILTER + which * super::smoothed::FILTER_VALUES;
            let filter = &mut self.filter[which];
            let octaves =
                value(base) + 10.0 * moved[dest::CUTOFF + which] + d.key_track[which] * (self.note - 60.0) / 12.0;
            let hz = 2f32.powf(octaves).clamp(COMB_LOWEST_HZ, (sample_rate * 0.45).min(20_000.0));
            let resonance = (value(base + 1) + moved[dest::RESONANCE + which]).clamp(0.0, 1.0);
            let changed = filter.kind != d.filter[which];
            if changed {
                filter.kind = d.filter[which];
                filter.clear();
            }
            let (tuning, k) = if d.filter[which].is_comb() {
                (sample_rate / hz, 0.98 * resonance)
            } else {
                ((PI * hz / sample_rate).tan(), 1.0 / (0.5 * 40f32.powf(resonance)))
            };
            // A filter that has just become another kind is tuned as that
            // kind at once: a comb's delay is no cutoff to glide from.
            if changed {
                filter.tuning.snap(tuning)
            } else {
                aim(&mut filter.tuning, tuning)
            }
            filter.k = k;
            let drive = (value(base + 2) + 24.0 * moved[dest::DRIVE + which]).clamp(0.0, 24.0);
            filter.drive = 10f32.powf(drive * 0.05);
            filter.driven = (drive / 6.0).min(1.0);
        }
        let balance = (value(super::smoothed::BALANCE) + moved[dest::BALANCE]).clamp(0.0, 1.0);
        aim(&mut self.balance[0], (2.0 * (1.0 - balance)).min(1.0));
        aim(&mut self.balance[1], (2.0 * balance).min(1.0));

        let level = (1.0 + moved[dest::LEVEL]).max(0.0);
        // The gate is shut for a moment while something is changed that
        // cannot be changed gradually.
        let gain = self.velocity_gain * level * value(super::smoothed::VOLUME) * super::VOICE_GAIN * shared.gate;
        aim(&mut self.gain, gain);
        let (left, right) = pan_gains(moved[dest::PAN].clamp(-1.0, 1.0));
        aim(&mut self.left, left);
        aim(&mut self.right, right);
    }

    /// Plays `out_left.len()` frames, at most a tick of them, on top of
    /// what is there. `levels` is the channel's level at each.
    pub fn render(&mut self, shared: &Shared, levels: &[f32], out_left: &mut [f32], out_right: &mut [f32]) {
        let frames = out_left.len().min(TICK);
        let d = &shared.d;
        // What goes to the first filter and to the second (which is the
        // same sound, unless the two are split), and past both.
        let mut first = [[0.0f32; TICK]; 2];
        let mut second = [[0.0f32; TICK]; 2];
        let mut past = [0.0f32; TICK];
        let mut bends = [0.0f32; TICK];

        // Oscillator 2 goes first: oscillator 1 may be bent by it.
        let bending = !self.fm.rests_at(0.0);
        let sounds = |osc: &OscVoice| !osc.level.rests_at(0.0);
        if sounds(&self.osc[1]) || bending {
            let mut raw = [[0.0f32; TICK]; 2];
            let reader = shared.table(1).reader(self.osc[1].level_of_table);
            self.osc[1].run(&d.osc[1], &shared.copies[1], reader, None, frames, &mut raw);
            let into = if d.split { &mut second } else { &mut first };
            let osc = &mut self.osc[1];
            for frame in 0..frames {
                bends[frame] = 0.5 * (raw[0][frame] + raw[1][frame]);
                let level = osc.level.next();
                into[0][frame] += raw[0][frame] * level * osc.left.next();
                into[1][frame] += raw[1][frame] * level * osc.right.next();
            }
        } else {
            self.osc[1].stand(&d.osc[1], frames);
        }
        if sounds(&self.osc[0]) {
            let mut raw = [[0.0f32; TICK]; 2];
            let reader = shared.table(0).reader(self.osc[0].level_of_table);
            let bent = bending.then_some((&bends, &mut self.fm));
            self.osc[0].run(&d.osc[0], &shared.copies[0], reader, bent, frames, &mut raw);
            let osc = &mut self.osc[0];
            for frame in 0..frames {
                let level = osc.level.next();
                first[0][frame] += raw[0][frame] * level * osc.left.next();
                first[1][frame] += raw[1][frame] * level * osc.right.next();
            }
        } else {
            self.osc[0].stand(&d.osc[0], frames);
            for _ in 0..frames {
                self.fm.next();
            }
        }

        // The sub: one plain wave, in the middle.
        if !self.sub_level.rests_at(0.0) {
            let step = self.sub_increment;
            for frame in 0..frames {
                let phase = self.sub_phase;
                let wave = match d.sub_shape {
                    0 => (TAU * phase).sin(),
                    1 => {
                        if phase < 0.25 {
                            4.0 * phase
                        } else if phase < 0.75 {
                            2.0 - 4.0 * phase
                        } else {
                            4.0 * phase - 4.0
                        }
                    }
                    2 => {
                        let across = if phase < 0.5 { phase + 0.5 } else { phase - 0.5 };
                        2.0 * across - 1.0 - edge(across, step)
                    }
                    _ => {
                        let half = if phase < 0.5 { phase + 0.5 } else { phase - 0.5 };
                        (if phase < 0.5 { 1.0 } else { -1.0 }) + edge(phase, step) - edge(half, step)
                    }
                } * self.sub_level.next();
                if d.sub_direct {
                    past[frame] += wave;
                } else {
                    first[0][frame] += wave;
                    first[1][frame] += wave;
                }
                self.sub_phase += step;
                self.sub_phase -= self.sub_phase.floor();
            }
        }

        if !self.noise_level.rests_at(0.0) {
            let (same, own) = (d.noise_same, d.noise_own);
            for frame in 0..frames {
                let level = self.noise_level.next();
                let left = self.noise.bipolar();
                let right = if own > 0.0 { left * same + self.noise.bipolar() * own } else { left };
                // Dark is what a low-pass leaves of it, thin what it takes.
                let mut colour = |side: usize, white: f32| {
                    self.noise_state[side] += (white - self.noise_state[side]) * d.noise_pole;
                    if d.noise_dark { self.noise_state[side] * d.noise_makeup } else { white - self.noise_state[side] }
                };
                let (left, right) =
                    if d.noise_pole < 1.0 { (colour(0, left), colour(1, right)) } else { (left, right) };
                first[0][frame] += left * level;
                first[1][frame] += right * level;
            }
        }

        let [first_left, first_right] = &mut first;
        let [second_left, second_right] = &mut second;
        let (first_left, first_right) = (&mut first_left[..frames], &mut first_right[..frames]);
        let (second_left, second_right) = (&mut second_left[..frames], &mut second_right[..frames]);
        let [one, two] = &mut self.filter;
        if d.serial {
            one.run(d.filter[0], first_left, first_right);
            two.run(d.filter[1], first_left, first_right);
            for _ in 0..frames {
                self.balance[0].next();
                self.balance[1].next();
            }
        } else {
            if !d.split {
                second_left.copy_from_slice(first_left);
                second_right.copy_from_slice(first_right);
            }
            one.run(d.filter[0], first_left, first_right);
            two.run(d.filter[1], second_left, second_right);
            for frame in 0..frames {
                let (a, b) = (self.balance[0].next(), self.balance[1].next());
                first_left[frame] = first_left[frame] * a + second_left[frame] * b;
                first_right[frame] = first_right[frame] * a + second_right[frame] * b;
            }
        }
        // A filter that is off, or was not run, still has to be where it
        // is told to be when it is next needed.
        for (filter, kind) in self.filter.iter_mut().zip(d.filter) {
            if kind == FilterKind::Off {
                for _ in 0..frames {
                    filter.tuning.next();
                }
            }
        }

        for frame in 0..frames {
            let level = self.env[0].tick(&d.env[0]) * self.gain.next() * self.fade.tick() * levels[frame];
            out_left[frame] += (first_left[frame] + past[frame]) * level * self.left.next();
            out_right[frame] += (first_right[frame] + past[frame]) * level * self.right.next();
        }
        if self.env[0].stage == Stage::Idle || self.fade.finished() {
            self.active = false;
            self.held = false;
        }
    }
}

/// The correction that rounds the edge of a plain sawtooth or square, at
/// `phase` of a cycle that moves on by `step`.
#[inline]
fn edge(phase: f32, step: f32) -> f32 {
    if phase < step {
        let x = phase / step;
        x + x - x * x - 1.0
    } else if phase > 1.0 - step {
        let x = (phase - 1.0) / step;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// A phase from 0 to 1 as an oscillator counts it.
#[inline]
fn counted(phase: f32) -> u32 {
    (f64::from(phase) * 4_294_967_296.0) as u32
}

/// How far a copy moves on in a frame, as an oscillator counts it.
#[inline]
fn counted_step(increment: f32, ratio: f32) -> u32 {
    (f64::from(increment) * f64::from(ratio) * 4_294_967_296.0).min(4_294_967_295.0) as u32
}

/// What counting to a 24th of the range is of a cycle.
const COUNTED: f32 = 1.0 / 16_777_216.0;

impl OscVoice {
    /// Moves on by `frames` frames without sounding: the phases keep
    /// their place and what glides keeps going.
    fn stand(&mut self, setting: &OscSetting, frames: usize) {
        for copy in 0..setting.unison {
            let step = counted_step(self.increment, setting.ratio[copy]);
            self.phase[copy] = self.phase[copy].wrapping_add(step.wrapping_mul(frames as u32));
        }
        for _ in 0..frames {
            self.position.next();
            self.warp.next();
            self.level.next();
            self.left.next();
            self.right.next();
        }
    }

    /// Plays `frames` frames of every copy into `out` (left, right), at
    /// the copies' own levels and places (`copies`: left, right): the
    /// oscillator's level and pan are the caller's. `bent_by` is what bends
    /// the phase, frame by frame, and by how many cycles.
    fn run(
        &mut self,
        setting: &OscSetting,
        copies: &[[f32; MAX_UNISON]; 2],
        reader: Reader<'_>,
        bent_by: Option<(&[f32; TICK], &mut Glide)>,
        frames: usize,
        out: &mut [[f32; TICK]; 2],
    ) {
        // Where in the table each frame is read, and how far the cycle is
        // bent: the same for every copy.
        let mut pair = [0usize; TICK];
        let mut mix = [0.0f32; TICK];
        let mut warp = [0.0f32; TICK];
        let mut bend = [0.0f32; TICK];
        for frame in 0..frames {
            (pair[frame], mix[frame]) = reader.pair(self.position.next());
            warp[frame] = self.warp.next();
        }
        let bending = bent_by.is_some();
        if let Some((by, depth)) = bent_by {
            for frame in 0..frames {
                bend[frame] = by[frame] * depth.next();
            }
        }
        let [out_left, out_right] = out;
        for copy in 0..setting.unison {
            let step = counted_step(self.increment, setting.ratio[copy]);
            let (left, right) = (copies[0][copy], copies[1][copy]);
            let mut phase = self.phase[copy];
            match setting.warp {
                Warp::Off if !bending => {
                    for frame in 0..frames {
                        let sample = reader.read_counted(pair[frame], mix[frame], phase);
                        out_left[frame] += sample * left;
                        out_right[frame] += sample * right;
                        phase = phase.wrapping_add(step);
                    }
                }
                Warp::Sync => {
                    // The cycle is restarted with the period. Over the last
                    // few frames of a period what is read goes over into
                    // what leads up to the restart, so that the restart is
                    // not a step.
                    let over = (6.0 * step as f32 * (1.0 / 4_294_967_296.0)).clamp(1.0 / 256.0, 0.5);
                    for frame in 0..frames {
                        let period = (phase >> 8) as f32 * COUNTED;
                        let ratio = sync_ratio(warp[frame]);
                        let at = period * ratio + bend[frame];
                        let mut sample = reader.read(pair[frame], mix[frame], at - at.floor());
                        if period > 1.0 - over {
                            let lead = (period - 1.0) * ratio + bend[frame];
                            let next = reader.read(pair[frame], mix[frame], lead - lead.floor());
                            sample += (next - sample) * (period - (1.0 - over)) / over;
                        }
                        out_left[frame] += sample * left;
                        out_right[frame] += sample * right;
                        phase = phase.wrapping_add(step);
                    }
                }
                Warp::Bend | Warp::Squeeze | Warp::Off => {
                    for frame in 0..frames {
                        let period = (phase >> 8) as f32 * COUNTED;
                        let at = match setting.warp {
                            Warp::Bend => bent(period, bend_lean(warp[frame])),
                            // A cycle squeezed into the start of its period
                            // rests at its end, which is its beginning.
                            Warp::Squeeze => (period / squeeze_width(warp[frame])).min(1.0),
                            _ => period,
                        } + bend[frame];
                        let at = if bending { at - at.floor() } else { at };
                        let sample = reader.read(pair[frame], mix[frame], at);
                        out_left[frame] += sample * left;
                        out_right[frame] += sample * right;
                        phase = phase.wrapping_add(step);
                    }
                }
            }
            self.phase[copy] = phase;
        }
    }
}

/// Which parameter's amount each slot of the matrix that is in use reads,
/// with what it reads and what it moves.
pub type SlotList = [(usize, usize); SLOTS];
