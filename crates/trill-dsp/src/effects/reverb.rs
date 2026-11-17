//! Reverb: an eight-line feedback delay network with input diffusion,
//! frequency-dependent decay and gentle modulation, behind a filter on
//! what goes into it. It sounds from its pre-delay on: what has gone
//! through half of the diffusion comes out at once, and the network's
//! tail arrives under it.

// The network is a set of parallel arrays indexed by delay-line number;
// index loops say that more plainly than zipped iterators would.
#![allow(clippy::needless_range_loop, clippy::manual_memcpy)]

use crate::define_params;
use crate::params::{ParamValues, choice, float};
use crate::util::{Biquad, DelayLine, OnePole, mix_gains};
use std::f32::consts::FRAC_1_SQRT_2;
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

define_params!(P, PARAMS, [
    Size => float("size", "Room Size", 0.0, 1.0, 0.6, ""),
    Decay => float("decay", "Decay Time", 0.2, 20.0, 2.5, "s"),
    Damping => float(
        "damping",
        "High Damping (what is over 4.5 kHz rings for a shorter time and comes back quieter: at 0.5, two thirds \
         as long and 5.4 dB down)",
        0.0,
        1.0,
        0.5,
        ""
    ),
    Predelay => float("predelay", "Pre-delay (the reverb begins this long after the sound)", 0.0, 200.0, 15.0, "ms"),
    LowCut => float("low_cut", "Input Low Cut (keeps lows out of the reverb)", 20.0, 1000.0, 150.0, "Hz"),
    Modulation => float("modulation", "Modulation", 0.0, 1.0, 0.3, ""),
    Width => float(
        "width",
        "Stereo Width (0 = mono, 1 = as the reverb is: its sides 2.3 dB under its middle, 2 = sides doubled)",
        0.0,
        2.0,
        1.0,
        ""
    ),
    Mix => float("mix", "Mix (0.5 = equal dry and wet, 1 = wet only)", 0.0, 1.0, 0.25, ""),
    HighCut => float(
        "high_cut",
        "Input High Cut (keeps highs out of the reverb; 20000 = off)",
        1000.0,
        20000.0,
        8000.0,
        "Hz"
    ),
    Legacy => choice(
        "legacy",
        "Legacy (the reverb of older songs. `format 1`: some 10 dB louder, the cuts 6 dB an octave on what comes \
         out. `formats 2-6`: begins 30 ms or more after its pre-delay, as wide as it can be, darker in the tail, \
         6 dB more for each doubling of the decay)",
        LEGACY,
        0
    ),
]);

/// What `legacy` can say: the reverb as it is, as songs of format 1 have
/// it, and as songs of formats 2 to 6 have it.
const LEGACY: &[&str] = &["off", "format 1", "formats 2-6"];
const FIRST: usize = 1;

pub const DESCRIPTION: &str = "Algorithmic reverb. `decay` is the tail length in seconds, `size` the room scale, \
`predelay` how long after the sound it begins. `damping` shortens and quietens what is over 4.5 kHz. `low_cut` \
and `high_cut` (12 dB an octave each) filter what goes in: the usual send keeps out what is under \
150-300 Hz and over 6-10 kHz; whatever the cuts, it returns about 4 dB less of what is under 300 Hz than of the \
middle. A longer decay returns more, 3 to 4 dB for each doubling of `decay` (of steady sound going in, a decay of \
0.7 s returns about 11 dB less than went in, 1 s 9.5 dB less, 2 s 6 dB less and 2.8 s 4 dB less, between the \
cuts); `size` changes the level by a dB at most. Its two sides are 2.3 dB under its middle at a `width` of 1. On a \
return track set `mix` to 1 and set the amount with the sends (-10 to -20 dB for most \
tracks); as an insert, the dry signal stays at full level up to mix 0.5.";

const LINES: usize = 8;
const LINE_MS: [f32; LINES] = [29.7, 34.1, 38.9, 43.3, 47.9, 53.1, 59.3, 67.7];
const DIFFUSER_MS: [[f32; 4]; 2] = [[4.77, 3.59, 12.73, 9.31], [5.03, 3.97, 12.11, 9.89]];
const DIFFUSER_GAIN: [f32; 4] = [0.75, 0.75, 0.625, 0.625];
const MAX_SCALE: f32 = 1.6;
const MOD_SAMPLES: f32 = 12.0;
/// What the taps of the network are brought to in songs of format 1.
const FIRST_GAIN: f32 = 0.4;
/// And in songs of formats 2 to 6, for a room of scale 1 and a decay of one
/// second: the root of the room scale went in (longer lines are read less
/// often, which made a big room quieter than a small one), and the root of
/// the decay up to [`SECOND_LOUDEST_DECAY`], which made what came back grow
/// by 6 dB for each doubling of the decay.
const SECOND_GAIN: f32 = 0.095;
const SECOND_LOUDEST_DECAY: f32 = 5.0;

/// What the taps are brought to now, for a room of scale 1 and a decay of
/// one second. The root of the room scale goes in as it did; of the decay
/// only its power [`GAIN_DECAY`], on top of the 3 dB for each doubling
/// that a network gives by itself. The two are fitted to what Ableton
/// Live's Reverb returns of the middle of what goes in, with its own
/// recordings of a song's sends as that: 10.7 dB less from a room of 0.7 s
/// and 3.8 dB less from a hall of 2.8 s.
const GAIN: f32 = 0.127;
const GAIN_DECAY: f32 = 0.24;
/// How much of what has gone through the first two diffusers comes out at
/// once: the same whatever the size and the decay, as a room's first
/// reflections are. Of what goes in, Live's room and hall both return
/// about 21 dB less in their first 20 ms.
const EARLY: f32 = 0.145;
/// The sides of the reverb against its middle, at a width of 1: Live's are
/// 2.2 to 2.4 dB under (its two sides are 0.27 alike).
const OWN_WIDTH: f32 = 0.767;
/// Where the highs begin that `damping` shortens and quietens, and what it
/// does to them: at a damping of d they ring (1 - d) to the power of
/// [`DAMPED_TIME`] as long and come back (1 - d) to the power of
/// [`DAMPED_LEVEL`] as loud. Live's room, its high shelf at 0.7, rang
/// 0.59 s over 5 kHz where it rang 0.67 s in the middle, and this one
/// rings 0.62 s and 0.69 s there; with these two numbers both of Live's
/// returns have their share over 5 kHz within a dB.
const DAMPED_HZ: f32 = 4500.0;
const DAMPED_TIME: f32 = 0.6;
const DAMPED_LEVEL: f32 = 0.9;
/// Where a line's own filter divides the highs it shortens from the rest:
/// over the corner, so that the middle rings as long as the decay says.
const LINE_HZ: f32 = 6000.0;
/// The reverb returns less of what is under this than of the middle, by
/// this much: as Live's does, whose room and hall both step down by 3 to
/// 5 dB between 500 and 250 Hz whatever their low cut.
const BODY_HZ: f32 = 400.0;
const BODY_DB: f32 = -4.3;

struct Allpass {
    line: DelayLine,
    delay: usize,
    gain: f32,
}

impl Allpass {
    #[inline]
    fn tick(&mut self, input: f32) -> f32 {
        let delayed = self.line.tap(self.delay);
        let fed = input + delayed * self.gain;
        self.line.write(fed);
        delayed - fed * self.gain
    }
}

pub struct Reverb {
    params: ParamValues,
    sample_rate: f32,
    predelay: [DelayLine; 2],
    diffusers: [Vec<Allpass>; 2],
    lines: Vec<DelayLine>,
    damp_state: [f32; LINES],
    /// The cuts on what goes in, 12 dB an octave.
    input_low: [Biquad; 2],
    input_high: [Biquad; 2],
    /// What the reverb takes of the lows and, by its damping, of the highs
    /// of what goes in.
    body: [Biquad; 2],
    air: [Biquad; 2],
    /// The cuts of format 1: 6 dB an octave, on what comes out.
    low_cut: [OnePole; 2],
    high_cut: [OnePole; 2],
    lfo: (f32, f32),
    // Derived from the parameters.
    lengths: [f32; LINES],
    /// What a pass through each line keeps: of everything, and of the highs.
    gains: [f32; LINES],
    high_gains: [f32; LINES],
    damp_coef: f32,
    predelay_samples: usize,
    lfo_step: (f32, f32),
    mod_depth: f32,
    width: f32,
    /// Which of [`LEGACY`].
    legacy: usize,
    cut_highs: bool,
    gain: f32,
    dry: f32,
    wet: f32,
}

impl Reverb {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            sample_rate: 48_000.0,
            predelay: [DelayLine::new(), DelayLine::new()],
            diffusers: [Vec::new(), Vec::new()],
            lines: (0..LINES).map(|_| DelayLine::new()).collect(),
            damp_state: [0.0; LINES],
            input_low: [Biquad::default(); 2],
            input_high: [Biquad::default(); 2],
            body: [Biquad::default(); 2],
            air: [Biquad::default(); 2],
            low_cut: [OnePole::default(); 2],
            high_cut: [OnePole::default(); 2],
            lfo: (0.0, 1.0),
            lengths: [1.0; LINES],
            gains: [0.0; LINES],
            high_gains: [0.0; LINES],
            damp_coef: 1.0,
            predelay_samples: 1,
            lfo_step: (0.0, 1.0),
            mod_depth: 0.0,
            width: 1.0,
            legacy: 0,
            cut_highs: false,
            gain: GAIN,
            dry: 1.0,
            wet: 0.0,
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        let scale = 0.5 + p.get(P::Size as usize) * (MAX_SCALE - 0.5);
        let decay = p.get(P::Decay as usize);
        let damping = p.get(P::Damping as usize);
        self.legacy = p.index(P::Legacy as usize);
        // What is left of the highs: all of them at a damping of 0.
        let highs = (1.0 - damping).max(0.03);
        for k in 0..LINES {
            self.lengths[k] = LINE_MS[k] * 0.001 * sr * scale;
            // Each pass through a line loses just enough for a 60 dB decay
            // over `decay` seconds.
            self.gains[k] = 10f32.powf(-3.0 * (self.lengths[k] / sr) / decay);
            self.high_gains[k] = 10f32.powf(-3.0 * (self.lengths[k] / sr) / (decay * highs.powf(DAMPED_TIME)));
        }
        self.damp_coef = if self.legacy == 0 {
            1.0 - (-std::f32::consts::TAU * LINE_HZ / sr).exp()
        } else {
            let damping_hz = 16_000.0 * (1500.0f32 / 16_000.0).powf(damping);
            1.0 - (-std::f32::consts::TAU * damping_hz / sr).exp()
        };
        self.predelay_samples = ((p.get(P::Predelay as usize) * 0.001 * sr) as usize).max(1);
        let (low_cut, high_cut) = (p.get(P::LowCut as usize), p.get(P::HighCut as usize));
        self.cut_highs = high_cut < 19_900.0;
        for channel in 0..2 {
            self.low_cut[channel].set_cutoff(low_cut, sr);
            self.high_cut[channel].set_cutoff(high_cut, sr);
            self.input_low[channel].set_high_pass(low_cut, FRAC_1_SQRT_2, sr);
            self.input_high[channel].set_low_pass(high_cut, FRAC_1_SQRT_2, sr);
            self.body[channel].set_low_shelf(BODY_HZ, BODY_DB, 1.0, sr);
            self.air[channel].set_high_shelf(DAMPED_HZ, 20.0 * DAMPED_LEVEL * highs.log10(), FRAC_1_SQRT_2, sr);
        }
        self.gain = match self.legacy {
            0 => GAIN * scale.sqrt() * decay.powf(GAIN_DECAY),
            FIRST => FIRST_GAIN,
            _ => SECOND_GAIN * (scale * decay.min(SECOND_LOUDEST_DECAY)).sqrt(),
        };
        let angle = std::f32::consts::TAU * 0.7 / sr;
        self.lfo_step = (angle.sin(), angle.cos());
        self.mod_depth = p.get(P::Modulation as usize) * MOD_SAMPLES * (sr / 48_000.0);
        self.width = p.get(P::Width as usize);
        (self.dry, self.wet) = mix_gains(p.get(P::Mix as usize));
        self.params.changed = false;
    }

    /// The reverb of songs of formats up to 6, to the letter.
    fn process_as_it_was(&mut self, left: &mut [f32], right: &mut [f32]) {
        let filter_input = self.legacy != FIRST;
        for i in 0..left.len() {
            let (dry_l, dry_r) = (left[i], right[i]);

            self.predelay[0].write(dry_l);
            self.predelay[1].write(dry_r);
            let mut in_l = self.predelay[0].tap(self.predelay_samples);
            let mut in_r = self.predelay[1].tap(self.predelay_samples);
            if filter_input {
                in_l = self.input_low[0].tick(in_l);
                in_r = self.input_low[1].tick(in_r);
                if self.cut_highs {
                    in_l = self.input_high[0].tick(in_l);
                    in_r = self.input_high[1].tick(in_r);
                }
            }
            for allpass in &mut self.diffusers[0] {
                in_l = allpass.tick(in_l);
            }
            for allpass in &mut self.diffusers[1] {
                in_r = allpass.tick(in_r);
            }

            // Quadrature LFO by rotation; half the lines are modulated, each
            // at a different phase, to break up metallic resonances.
            let (sin, cos) = self.lfo;
            self.lfo = (sin * self.lfo_step.1 + cos * self.lfo_step.0, cos * self.lfo_step.1 - sin * self.lfo_step.0);
            let phases = [sin, cos, -sin, -cos];

            let mut taps = [0.0f32; LINES];
            for k in 0..LINES {
                let modulation = if k % 2 == 0 { phases[k / 2] * self.mod_depth } else { 0.0 };
                let tap = self.lines[k].read(self.lengths[k] + modulation);
                self.damp_state[k] += (tap - self.damp_state[k]) * self.damp_coef;
                taps[k] = self.damp_state[k];
            }

            let mut wet_l = (taps[0] - taps[2] + taps[4] - taps[6]) * self.gain;
            let mut wet_r = (taps[1] - taps[3] + taps[5] - taps[7]) * self.gain;

            hadamard(&mut taps);
            for k in 0..LINES {
                let input = if k % 2 == 0 { in_l } else { in_r };
                self.lines[k].write(input + taps[k] * self.gains[k]);
            }

            if !filter_input {
                wet_l = self.low_cut[0].high(wet_l);
                wet_r = self.low_cut[1].high(wet_r);
                if self.cut_highs {
                    wet_l = self.high_cut[0].low(wet_l);
                    wet_r = self.high_cut[1].low(wet_r);
                }
            }
            let mid = (wet_l + wet_r) * 0.5;
            let side = (wet_l - wet_r) * 0.5 * self.width;

            left[i] = dry_l * self.dry + (mid + side) * self.wet;
            right[i] = dry_r * self.dry + (mid - side) * self.wet;
        }
    }

    /// What goes into the network of one side: the cuts, and the reverb's
    /// own voice.
    #[inline]
    fn voiced(&mut self, side: usize, mut input: f32) -> f32 {
        input = self.input_low[side].tick(input);
        if self.cut_highs {
            input = self.input_high[side].tick(input);
        }
        self.air[side].tick(self.body[side].tick(input))
    }
}

/// In-place fast Walsh-Hadamard transform, scaled to be orthonormal.
#[inline]
fn hadamard(v: &mut [f32; LINES]) {
    let mut h = 1;
    while h < LINES {
        let mut i = 0;
        while i < LINES {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
            i += h * 2;
        }
        h *= 2;
    }
    let norm = 1.0 / (LINES as f32).sqrt();
    for x in v {
        *x *= norm;
    }
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProcessor for Reverb {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate as f32;
        let sr = self.sample_rate;
        for line in &mut self.predelay {
            line.resize((0.2 * sr) as usize + 8);
        }
        for (channel, diffusers) in self.diffusers.iter_mut().enumerate() {
            *diffusers = (0..4)
                .map(|stage| {
                    let delay = ((DIFFUSER_MS[channel][stage] * 0.001 * sr) as usize).max(1);
                    let mut line = DelayLine::new();
                    line.resize(delay + 4);
                    Allpass { line, delay, gain: DIFFUSER_GAIN[stage] }
                })
                .collect();
        }
        for (line, ms) in self.lines.iter_mut().zip(LINE_MS) {
            line.resize((ms * 0.001 * sr * MAX_SCALE + MOD_SAMPLES * sr / 48_000.0) as usize + 16);
        }
        self.params.changed = true;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
        if self.params.changed {
            self.refresh();
        }
        if self.legacy != 0 {
            self.process_as_it_was(left, right);
            return;
        }
        let side_gain = self.width * OWN_WIDTH;
        for i in 0..left.len() {
            let (dry_l, dry_r) = (left[i], right[i]);

            self.predelay[0].write(dry_l);
            self.predelay[1].write(dry_r);
            let in_l = self.predelay[0].tap(self.predelay_samples);
            let in_r = self.predelay[1].tap(self.predelay_samples);
            let mut in_l = self.voiced(0, in_l);
            let mut in_r = self.voiced(1, in_r);
            // What has gone through two of the four diffusers is heard at
            // once: a close cluster of reflections, where the network's
            // own first sound is a line's length away.
            let (mut early_l, mut early_r) = (0.0, 0.0);
            for (stage, allpass) in self.diffusers[0].iter_mut().enumerate() {
                in_l = allpass.tick(in_l);
                if stage == 1 {
                    early_l = in_l;
                }
            }
            for (stage, allpass) in self.diffusers[1].iter_mut().enumerate() {
                in_r = allpass.tick(in_r);
                if stage == 1 {
                    early_r = in_r;
                }
            }

            // Quadrature LFO by rotation; half the lines are modulated, each
            // at a different phase, to break up metallic resonances.
            let (sin, cos) = self.lfo;
            self.lfo = (sin * self.lfo_step.1 + cos * self.lfo_step.0, cos * self.lfo_step.1 - sin * self.lfo_step.0);
            let phases = [sin, cos, -sin, -cos];

            let mut taps = [0.0f32; LINES];
            let mut kept = [0.0f32; LINES];
            for k in 0..LINES {
                let modulation = if k % 2 == 0 { phases[k / 2] * self.mod_depth } else { 0.0 };
                let tap = self.lines[k].read(self.lengths[k] + modulation);
                taps[k] = tap;
                // What the line keeps for its next pass: a shelf, the highs
                // by their own share and the rest by the line's.
                self.damp_state[k] += (tap - self.damp_state[k]) * self.damp_coef;
                kept[k] = tap * self.high_gains[k] + self.damp_state[k] * (self.gains[k] - self.high_gains[k]);
            }

            let wet_l = early_l * EARLY + (taps[0] - taps[2] + taps[4] - taps[6]) * self.gain;
            let wet_r = early_r * EARLY + (taps[1] - taps[3] + taps[5] - taps[7]) * self.gain;

            hadamard(&mut kept);
            for k in 0..LINES {
                let input = if k % 2 == 0 { in_l } else { in_r };
                self.lines[k].write(input + kept[k]);
            }

            let mid = (wet_l + wet_r) * 0.5;
            let side = (wet_l - wet_r) * 0.5 * side_gain;

            left[i] = dry_l * self.dry + (mid + side) * self.wet;
            right[i] = dry_r * self.dry + (mid - side) * self.wet;
        }
    }

    fn reset(&mut self) {
        for line in self.predelay.iter_mut().chain(self.lines.iter_mut()) {
            line.clear();
        }
        for allpass in self.diffusers.iter_mut().flatten() {
            allpass.line.clear();
        }
        self.damp_state = [0.0; LINES];
        for filter in self.low_cut.iter_mut().chain(self.high_cut.iter_mut()) {
            filter.reset();
        }
        let biquads = self.input_low.iter_mut().chain(self.input_high.iter_mut());
        for filter in biquads.chain(self.body.iter_mut()).chain(self.air.iter_mut()) {
            filter.reset();
        }
        self.lfo = (0.0, 1.0);
    }
}
