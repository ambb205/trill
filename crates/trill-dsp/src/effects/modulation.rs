//! Chorus, auto filter and the tempo-synced "pump" volume shaper.

use crate::define_params;
use crate::params::{ParamValues, choice, float};
use crate::util::{DelayLine, Smooth, Svf, crossfade_gains};
use std::f32::consts::TAU;
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

// ---- Chorus ----------------------------------------------------------------

mod chorus {
    use super::*;

    define_params!(P, PARAMS, [
        Rate => float("rate", "Rate", 0.05, 8.0, 0.8, "Hz"),
        Depth => float("depth", "Depth", 0.0, 1.0, 0.5, ""),
        Feedback => float("feedback", "Feedback (higher = flanger-like)", 0.0, 0.7, 0.0, ""),
        Mix => float("mix", "Mix", 0.0, 1.0, 0.5, ""),
    ]);

    pub const DESCRIPTION: &str = "Stereo chorus: thickens and widens pads, keys, guitars and basses.";

    const BASE_SECONDS: f32 = 0.012;
    const DEPTH_SECONDS: f32 = 0.006;

    pub struct Chorus {
        params: ParamValues,
        sample_rate: f32,
        lines: [DelayLine; 2],
        phase: f32,
    }

    impl Chorus {
        pub fn new() -> Self {
            Self {
                params: ParamValues::new(PARAMS),
                sample_rate: 48_000.0,
                lines: [DelayLine::new(), DelayLine::new()],
                phase: 0.0,
            }
        }
    }

    impl Default for Chorus {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeviceProcessor for Chorus {
        fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
            self.sample_rate = sample_rate as f32;
            for line in &mut self.lines {
                line.resize(((BASE_SECONDS + DEPTH_SECONDS) * self.sample_rate) as usize + 8);
            }
            self.reset();
        }

        fn set_param(&mut self, index: usize, value: f64) {
            self.params.set(index, value);
        }

        fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
            let sr = self.sample_rate;
            let step = self.params.get(P::Rate as usize) / sr;
            let depth = self.params.get(P::Depth as usize) * DEPTH_SECONDS * sr;
            let base = BASE_SECONDS * sr;
            let feedback = self.params.get(P::Feedback as usize);
            let (dry, wet) = crossfade_gains(self.params.get(P::Mix as usize));

            for i in 0..left.len() {
                let (in_l, in_r) = (left[i], right[i]);
                let lfo_l = (self.phase * TAU).sin();
                let lfo_r = ((self.phase + 0.25) * TAU).sin();
                self.phase += step;
                self.phase -= self.phase.floor();

                let tap_l = self.lines[0].read(base + depth * lfo_l);
                let tap_r = self.lines[1].read(base + depth * lfo_r);
                self.lines[0].write(in_l + tap_l * feedback);
                self.lines[1].write(in_r + tap_r * feedback);
                left[i] = in_l * dry + tap_l * wet;
                right[i] = in_r * dry + tap_r * wet;
            }
        }

        fn reset(&mut self) {
            for line in &mut self.lines {
                line.clear();
            }
            self.phase = 0.0;
        }
    }
}

pub use chorus::{Chorus, DESCRIPTION as CHORUS_DESCRIPTION, PARAMS as CHORUS_PARAMS};

// ---- Auto filter -----------------------------------------------------------

mod filter {
    use super::*;

    const TYPES: &[&str] = &["lowpass", "highpass", "bandpass", "notch"];

    define_params!(P, PARAMS, [
        Type => choice("type", "Filter Type", TYPES, 0),
        Cutoff => float("cutoff", "Cutoff", 20.0, 20000.0, 1000.0, "Hz"),
        Resonance => float("resonance", "Resonance", 0.0, 1.0, 0.2, ""),
        LfoRate => float("lfo_rate", "LFO Rate", 0.01, 20.0, 1.0, "Hz"),
        LfoAmount => float("lfo_amount", "LFO Amount", 0.0, 4.0, 0.0, "oct"),
    ]);

    pub const DESCRIPTION: &str = "Resonant filter with an LFO. Automate `cutoff` for sweeps and build-ups.";

    const CONTROL_BLOCK: usize = 16;

    pub struct AutoFilter {
        params: ParamValues,
        sample_rate: f32,
        filters: [Svf; 2],
        /// Smoothed in the log domain so sweeps sound even.
        cutoff: Smooth,
        primed: bool,
        phase: f32,
    }

    impl AutoFilter {
        pub fn new() -> Self {
            Self {
                params: ParamValues::new(PARAMS),
                sample_rate: 48_000.0,
                filters: [Svf::default(); 2],
                cutoff: Smooth::new(0.0),
                primed: false,
                phase: 0.0,
            }
        }
    }

    impl Default for AutoFilter {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeviceProcessor for AutoFilter {
        fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
            self.sample_rate = sample_rate as f32;
            // The smoother runs once per control block.
            self.cutoff.set_time(0.02, self.sample_rate / CONTROL_BLOCK as f32);
            self.reset();
        }

        fn set_param(&mut self, index: usize, value: f64) {
            self.params.set(index, value);
        }

        fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
            let sr = self.sample_rate;
            let kind = self.params.index(P::Type as usize);
            let target = self.params.get(P::Cutoff as usize).log2();
            let q = 0.5 * 40f32.powf(self.params.get(P::Resonance as usize));
            let rate = self.params.get(P::LfoRate as usize);
            let amount = self.params.get(P::LfoAmount as usize);
            if !self.primed {
                self.cutoff.snap(target);
                self.primed = true;
            }

            let mut pos = 0;
            while pos < left.len() {
                let len = (left.len() - pos).min(CONTROL_BLOCK);
                let lfo = (self.phase * TAU).sin();
                self.phase = (self.phase + rate * len as f32 / sr).fract();
                let cutoff = 2f32.powf(self.cutoff.next(target) + lfo * amount);
                for filter in &mut self.filters {
                    filter.set(cutoff, q, sr);
                }
                for (samples, filter) in
                    [&mut left[pos..pos + len], &mut right[pos..pos + len]].into_iter().zip(self.filters.iter_mut())
                {
                    for sample in samples.iter_mut() {
                        let out = filter.tick(*sample);
                        *sample = match kind {
                            0 => out.low,
                            1 => out.high,
                            2 => out.band,
                            _ => out.low + out.high,
                        };
                    }
                }
                pos += len;
            }
        }

        fn reset(&mut self) {
            for filter in &mut self.filters {
                filter.reset();
            }
            self.primed = false;
            self.phase = 0.0;
        }
    }
}

pub use filter::{AutoFilter, DESCRIPTION as FILTER_DESCRIPTION, PARAMS as FILTER_PARAMS};

// ---- Pump ------------------------------------------------------------------

mod pump {
    use super::*;

    define_params!(P, PARAMS, [
        RateBeats => float("rate_beats", "Rate (1 = every beat, 0.5 = every 1/8)", 0.25, 4.0, 1.0, "beats"),
        Depth => float("depth", "Depth", 0.0, 1.0, 0.7, ""),
        Curve => float("curve", "Recovery Curve (higher = faster recovery)", 0.3, 6.0, 2.0, ""),
        Offset => float("offset", "Phase Offset", 0.0, 1.0, 0.0, ""),
    ]);

    pub const DESCRIPTION: &str = "Sidechain-style ducking locked to the song grid: the volume dips on every \
beat and swells back, as if compressed by a four-on-the-floor kick. Put it on pads and basses for a pumping groove.";

    pub struct Pump {
        params: ParamValues,
        sample_rate: f32,
        gain: Smooth,
    }

    impl Pump {
        pub fn new() -> Self {
            Self { params: ParamValues::new(PARAMS), sample_rate: 48_000.0, gain: Smooth::new(1.0) }
        }
    }

    impl Default for Pump {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeviceProcessor for Pump {
        fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
            self.sample_rate = sample_rate as f32;
            self.gain.set_time(0.003, self.sample_rate);
            self.reset();
        }

        fn set_param(&mut self, index: usize, value: f64) {
            self.params.set(index, value);
        }

        fn process(&mut self, info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
            let rate = self.params.get(P::RateBeats as usize) as f64;
            let depth = self.params.get(P::Depth as usize);
            let curve = self.params.get(P::Curve as usize);
            let offset = self.params.get(P::Offset as usize) as f64;
            let beats_per_sample = info.tempo / (60.0 * info.sample_rate);

            for i in 0..left.len() {
                let target = if info.playing {
                    let beat = info.position_beats + i as f64 * beats_per_sample;
                    let phase = (beat / rate + offset).rem_euclid(1.0) as f32;
                    1.0 - depth * (1.0 - phase).powf(curve * 2.0)
                } else {
                    1.0
                };
                let gain = self.gain.next(target);
                left[i] *= gain;
                right[i] *= gain;
            }
        }

        fn reset(&mut self) {
            self.gain.snap(1.0);
        }
    }
}

pub use pump::{DESCRIPTION as PUMP_DESCRIPTION, PARAMS as PUMP_PARAMS, Pump};
