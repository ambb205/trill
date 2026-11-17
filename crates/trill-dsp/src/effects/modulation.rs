//! Chorus, auto filter, tremolo and the tempo-synced "pump" volume shaper.

use crate::define_params;
use crate::params::{ParamValues, boolean, choice, float};
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
        Width => float(
            "width",
            "Stereo Width of the chorused sound (0 = mono, 1 = as it is, 2 = sides doubled)",
            0.0,
            2.0,
            1.0,
            ""
        ),
    ]);

    pub const DESCRIPTION: &str = "Stereo chorus: thickens and widens pads, keys, guitars and basses. The two sides \
are modulated a quarter of a cycle apart, which is where the width comes from; `width` scales it (0 keeps a mono \
source mono, 1.4 is a wide ensemble).";

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
            let width = self.params.get(P::Width as usize);

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
                // At a width of 1 the taps go out untouched, to the bit.
                let (wet_l, wet_r) = if width == 1.0 {
                    (tap_l, tap_r)
                } else {
                    let mid = (tap_l + tap_r) * 0.5;
                    let side = (tap_l - tap_r) * 0.5 * width;
                    (mid + side, mid - side)
                };
                left[i] = in_l * dry + wet_l * wet;
                right[i] = in_r * dry + wet_r * wet;
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
    const SLOPES: &[&str] = &["12 dB", "24 dB"];

    define_params!(P, PARAMS, [
        Type => choice("type", "Filter Type", TYPES, 0),
        Cutoff => float("cutoff", "Cutoff", 20.0, 20000.0, 1000.0, "Hz"),
        Resonance => float("resonance", "Resonance", 0.0, 1.0, 0.2, ""),
        LfoRate => float("lfo_rate", "LFO Rate", 0.01, 20.0, 1.0, "Hz"),
        LfoAmount => float("lfo_amount", "LFO Amount", 0.0, 4.0, 0.0, "oct"),
        Slope => choice("slope", "Slope (per octave)", SLOPES, 0),
    ]);

    pub const DESCRIPTION: &str = "Resonant filter with an LFO. Automate `cutoff` for sweeps and build-ups. `slope` \
is \"12 dB\" or \"24 dB\" an octave: at 24 a shut low-pass is properly muffled (24 dB down an octave above the cutoff \
where 12 leaves half of it), with the same peak at the cutoff.";

    const CONTROL_BLOCK: usize = 16;
    /// What the root of the Q is brought to for each of the two stages of
    /// the 24 dB slope.
    const STEEP_Q: f32 = 0.8;

    pub struct AutoFilter {
        params: ParamValues,
        sample_rate: f32,
        /// `[stage][channel]`: one stage for 12 dB an octave, both for 24.
        filters: [[Svf; 2]; 2],
        steep: bool,
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
                filters: [[Svf::default(); 2]; 2],
                steep: false,
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

    /// What a filter of type `kind` takes from one stage.
    #[inline]
    fn pick(kind: usize, out: crate::util::SvfOut) -> f32 {
        match kind {
            0 => out.low,
            1 => out.high,
            2 => out.band,
            _ => out.low + out.high,
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
            let steep = self.params.index(P::Slope as usize) == 1;
            let q = 0.5 * 40f32.powf(self.params.get(P::Resonance as usize));
            // Two stages in a row multiply their peaks, so each gets the
            // root of the Q, and a little less: at a resonance of 0.12 the
            // pair is then 6 dB down at the cutoff with no peak at all,
            // and at the default 0.2 close to a fourth-order Butterworth.
            // Ableton Live's Auto Filter at 24 dB was measured to do the
            // first.
            let q = if steep { STEEP_Q * q.sqrt() } else { q };
            let rate = self.params.get(P::LfoRate as usize);
            let amount = self.params.get(P::LfoAmount as usize);
            if !self.primed {
                self.cutoff.snap(target);
                self.primed = true;
            }
            if steep && !self.steep {
                // The second stage comes in from rest.
                self.filters[1].iter_mut().for_each(Svf::reset);
            }
            self.steep = steep;

            let mut pos = 0;
            while pos < left.len() {
                let len = (left.len() - pos).min(CONTROL_BLOCK);
                let lfo = (self.phase * TAU).sin();
                self.phase = (self.phase + rate * len as f32 / sr).fract();
                let cutoff = 2f32.powf(self.cutoff.next(target) + lfo * amount);
                for filter in self.filters.iter_mut().flatten() {
                    filter.set(cutoff, q, sr);
                }
                let [first, second] = &mut self.filters;
                for (channel, samples) in
                    [&mut left[pos..pos + len], &mut right[pos..pos + len]].into_iter().enumerate()
                {
                    for sample in samples.iter_mut() {
                        let mut out = pick(kind, first[channel].tick(*sample));
                        if steep {
                            out = pick(kind, second[channel].tick(out));
                        }
                        *sample = out;
                    }
                }
                pos += len;
            }
        }

        fn reset(&mut self) {
            for filter in self.filters.iter_mut().flatten() {
                filter.reset();
            }
            self.primed = false;
            self.phase = 0.0;
        }
    }
}

pub use filter::{AutoFilter, DESCRIPTION as FILTER_DESCRIPTION, PARAMS as FILTER_PARAMS};

// ---- Tremolo ---------------------------------------------------------------

mod tremolo {
    use super::*;

    const SHAPES: &[&str] = &["sine", "triangle", "square"];

    define_params!(P, PARAMS, [
        Sync => boolean("sync", "Tempo Sync (the cycle is `rate_beats` long and starts on the beat)", false),
        Rate => float("rate", "Rate (when sync is off)", 0.05, 20.0, 5.0, "Hz"),
        RateBeats => float("rate_beats", "Synced Cycle (1 = a quarter note, 0.5 = an eighth)", 0.0625, 16.0, 0.5, "beats"),
        Depth => float("depth", "Depth (how far the level dips: 0.35 = down to 65 %)", 0.0, 1.0, 0.5, ""),
        Shape => choice("shape", "Shape", SHAPES, 0),
        Spread => float(
            "spread",
            "Stereo Spread (0 = tremolo: both sides together; 180 = auto-pan: the sides take turns)",
            0.0,
            180.0,
            0.0,
            "deg"
        ),
    ]);

    pub const DESCRIPTION: &str = "Tremolo and auto-pan: the level swells and dips `rate` times a second (or once \
every `rate_beats` with `sync`), down to 1 - `depth` of what it was. With `spread` at 0 both sides move together \
(a guitar amp's tremolo); at 180 the right side is loud while the left is quiet, so the sound travels between the \
speakers at an even level (an electric piano's stereo vibrato: depth 0.35, rate 4-6 Hz, spread 180).";

    pub struct Tremolo {
        params: ParamValues,
        sample_rate: f32,
        phase: f32,
        gains: [Smooth; 2],
    }

    impl Tremolo {
        pub fn new() -> Self {
            Self { params: ParamValues::new(PARAMS), sample_rate: 48_000.0, phase: 0.0, gains: [Smooth::new(1.0); 2] }
        }
    }

    impl Default for Tremolo {
        fn default() -> Self {
            Self::new()
        }
    }

    /// The level at `phase` (in cycles) of a cycle that starts at full level
    /// and dips to `1 - depth` half way.
    #[inline]
    fn level(shape: usize, phase: f32, depth: f32) -> f32 {
        let phase = phase - phase.floor();
        // How far into the dip, 0..=1.
        let dip = match shape {
            0 => 0.5 - 0.5 * (phase * TAU).cos(),
            1 => 1.0 - (2.0 * phase - 1.0).abs(),
            _ => {
                if phase < 0.5 {
                    0.0
                } else {
                    1.0
                }
            }
        };
        1.0 - depth * dip
    }

    impl DeviceProcessor for Tremolo {
        fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
            self.sample_rate = sample_rate as f32;
            // Short enough to leave a square its edge, long enough that the
            // edge is not a click.
            for gain in &mut self.gains {
                gain.set_time(0.002, self.sample_rate);
            }
            self.reset();
        }

        fn set_param(&mut self, index: usize, value: f64) {
            self.params.set(index, value);
        }

        fn process(&mut self, info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
            let depth = self.params.get(P::Depth as usize);
            let shape = self.params.index(P::Shape as usize);
            let spread = self.params.get(P::Spread as usize) / 360.0;
            let synced = self.params.on(P::Sync as usize);
            let cycle_beats = self.params.get(P::RateBeats as usize) as f64;
            let step = if synced {
                (info.tempo / (60.0 * info.sample_rate) / cycle_beats) as f32
            } else {
                self.params.get(P::Rate as usize) / self.sample_rate
            };
            // While the song plays, a synced cycle is where the song is: it
            // starts on the beat, and the same bar sounds the same every
            // time. Standing still, it runs on from where it was.
            if synced && info.playing {
                self.phase = (info.position_beats / cycle_beats).rem_euclid(1.0) as f32;
            }
            for i in 0..left.len() {
                let gain_l = self.gains[0].next(level(shape, self.phase, depth));
                let gain_r = self.gains[1].next(level(shape, self.phase + spread, depth));
                left[i] *= gain_l;
                right[i] *= gain_r;
                self.phase += step;
                self.phase -= self.phase.floor();
            }
        }

        fn reset(&mut self) {
            self.phase = 0.0;
            for gain in &mut self.gains {
                gain.snap(1.0);
            }
        }
    }
}

pub use tremolo::{DESCRIPTION as TREMOLO_DESCRIPTION, PARAMS as TREMOLO_PARAMS, Tremolo};

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
beat and swells back, as if compressed by a four-on-the-floor kick. Put it on pads and basses for a pumping groove. \
To duck to the kick that is really played, give a compressor the kick's track as its `sidechain` instead.";

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
