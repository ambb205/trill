//! Reverb: an eight-line feedback delay network with input diffusion,
//! frequency-dependent decay and gentle modulation.

// The network is a set of parallel arrays indexed by delay-line number;
// index loops say that more plainly than zipped iterators would.
#![allow(clippy::needless_range_loop, clippy::manual_memcpy)]

use crate::define_params;
use crate::params::{ParamValues, float};
use crate::util::{DelayLine, OnePole, mix_gains};
use daw_core::device::{DeviceProcessor, Event, ProcessInfo};

define_params!(P, PARAMS, [
    Size => float("size", "Room Size", 0.0, 1.0, 0.6, ""),
    Decay => float("decay", "Decay Time", 0.2, 20.0, 2.5, "s"),
    Damping => float("damping", "High Damping", 0.0, 1.0, 0.5, ""),
    Predelay => float("predelay", "Pre-delay", 0.0, 200.0, 15.0, "ms"),
    LowCut => float("low_cut", "Low Cut", 20.0, 1000.0, 150.0, "Hz"),
    Modulation => float("modulation", "Modulation", 0.0, 1.0, 0.3, ""),
    Width => float("width", "Stereo Width", 0.0, 1.0, 1.0, ""),
    Mix => float("mix", "Mix (0.5 = equal dry and wet, 1 = wet only)", 0.0, 1.0, 0.25, ""),
]);

pub const DESCRIPTION: &str = "Algorithmic reverb. `decay` is the tail length in seconds, `size` the room \
scale, `damping` darkens the tail. Up to mix 0.5 the dry signal stays at full level.";

const LINES: usize = 8;
const LINE_MS: [f32; LINES] = [29.7, 34.1, 38.9, 43.3, 47.9, 53.1, 59.3, 67.7];
const DIFFUSER_MS: [[f32; 4]; 2] = [[4.77, 3.59, 12.73, 9.31], [5.03, 3.97, 12.11, 9.89]];
const DIFFUSER_GAIN: [f32; 4] = [0.75, 0.75, 0.625, 0.625];
const MAX_SCALE: f32 = 1.6;
const MOD_SAMPLES: f32 = 12.0;

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
    low_cut: [OnePole; 2],
    lfo: (f32, f32),
    // Derived from the parameters.
    lengths: [f32; LINES],
    gains: [f32; LINES],
    damp_coef: f32,
    predelay_samples: usize,
    lfo_step: (f32, f32),
    mod_depth: f32,
    width: f32,
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
            low_cut: [OnePole::default(); 2],
            lfo: (0.0, 1.0),
            lengths: [1.0; LINES],
            gains: [0.0; LINES],
            damp_coef: 1.0,
            predelay_samples: 1,
            lfo_step: (0.0, 1.0),
            mod_depth: 0.0,
            width: 1.0,
            dry: 1.0,
            wet: 0.0,
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        let scale = 0.5 + p.get(P::Size as usize) * (MAX_SCALE - 0.5);
        let decay = p.get(P::Decay as usize);
        for k in 0..LINES {
            self.lengths[k] = LINE_MS[k] * 0.001 * sr * scale;
            // Each pass through a line loses just enough for a 60 dB decay
            // over `decay` seconds.
            self.gains[k] = 10f32.powf(-3.0 * (self.lengths[k] / sr) / decay);
        }
        let damping_hz = 16_000.0 * (1500.0f32 / 16_000.0).powf(p.get(P::Damping as usize));
        self.damp_coef = 1.0 - (-std::f32::consts::TAU * damping_hz / sr).exp();
        self.predelay_samples = ((p.get(P::Predelay as usize) * 0.001 * sr) as usize).max(1);
        for filter in &mut self.low_cut {
            filter.set_cutoff(p.get(P::LowCut as usize), sr);
        }
        let angle = std::f32::consts::TAU * 0.7 / sr;
        self.lfo_step = (angle.sin(), angle.cos());
        self.mod_depth = p.get(P::Modulation as usize) * MOD_SAMPLES * (sr / 48_000.0);
        self.width = p.get(P::Width as usize);
        (self.dry, self.wet) = mix_gains(p.get(P::Mix as usize));
        self.params.changed = false;
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
        for i in 0..left.len() {
            let (dry_l, dry_r) = (left[i], right[i]);

            self.predelay[0].write(dry_l);
            self.predelay[1].write(dry_r);
            let mut in_l = self.predelay[0].tap(self.predelay_samples);
            let mut in_r = self.predelay[1].tap(self.predelay_samples);
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

            let mut wet_l = (taps[0] - taps[2] + taps[4] - taps[6]) * 0.4;
            let mut wet_r = (taps[1] - taps[3] + taps[5] - taps[7]) * 0.4;

            hadamard(&mut taps);
            for k in 0..LINES {
                let input = if k % 2 == 0 { in_l } else { in_r };
                self.lines[k].write(input + taps[k] * self.gains[k]);
            }

            wet_l = self.low_cut[0].high(wet_l);
            wet_r = self.low_cut[1].high(wet_r);
            let mid = (wet_l + wet_r) * 0.5;
            let side = (wet_l - wet_r) * 0.5 * self.width;

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
        for filter in &mut self.low_cut {
            filter.reset();
        }
        self.lfo = (0.0, 1.0);
    }
}
