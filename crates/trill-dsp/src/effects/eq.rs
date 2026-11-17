//! EQ: low cut, low shelf, two peaking bands, high shelf, high cut. The
//! cuts are Butterworth filters of 12, 24 or 48 dB an octave.

use crate::define_params;
use crate::params::{ParamValues, choice, float};
use crate::util::{Biquad, butterworth_qs};
use std::f64::consts::FRAC_1_SQRT_2;
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

/// How steep a cut is, per octave: one, two or four biquads.
pub const SLOPES: &[&str] = &["12 dB", "24 dB", "48 dB"];

define_params!(P, PARAMS, [
    LowCut => float("low_cut", "Low Cut (10 = off)", 10.0, 20000.0, 10.0, "Hz"),
    LowFreq => float("low_freq", "Low Shelf Frequency", 20.0, 20000.0, 100.0, "Hz"),
    LowGain => float("low_gain", "Low Shelf Gain", -18.0, 18.0, 0.0, "dB"),
    Mid1Freq => float("mid1_freq", "Mid 1 Frequency", 20.0, 20000.0, 500.0, "Hz"),
    Mid1Gain => float("mid1_gain", "Mid 1 Gain", -18.0, 18.0, 0.0, "dB"),
    Mid1Q => float("mid1_q", "Mid 1 Q", 0.2, 10.0, 1.0, ""),
    Mid2Freq => float("mid2_freq", "Mid 2 Frequency", 20.0, 20000.0, 3000.0, "Hz"),
    Mid2Gain => float("mid2_gain", "Mid 2 Gain", -18.0, 18.0, 0.0, "dB"),
    Mid2Q => float("mid2_q", "Mid 2 Q", 0.2, 10.0, 1.0, ""),
    HighFreq => float("high_freq", "High Shelf Frequency", 20.0, 20000.0, 8000.0, "Hz"),
    HighGain => float("high_gain", "High Shelf Gain", -18.0, 18.0, 0.0, "dB"),
    HighCut => float("high_cut", "High Cut (20000 = off)", 20.0, 20000.0, 20000.0, "Hz"),
    LowCutSlope => choice("low_cut_slope", "Low Cut Slope (per octave)", SLOPES, 0),
    HighCutSlope => choice("high_cut_slope", "High Cut Slope (per octave)", SLOPES, 0),
    LowQ => float("low_q", "Low Shelf Q (0.71 = a plain shelf; higher dips before it rises)", 0.3, 2.0, FRAC_1_SQRT_2, ""),
    HighQ => float("high_q", "High Shelf Q (0.71 = a plain shelf; higher dips before it rises)", 0.3, 2.0, FRAC_1_SQRT_2, ""),
]);

pub const DESCRIPTION: &str = "Six-band equalizer. `low_cut` clears rumble from anything that is not bass or kick \
and `high_cut` takes the top off; each is 3 dB down at its frequency and falls by its slope: \"12 dB\", \"24 dB\" or \
\"48 dB\" an octave (48 leaves the sound above the corner alone and removes what is under it: 39 dB down at 0.55 of \
the corner). The shelves are for broad tone, the two mid bands (any frequency from 20 Hz to 20 kHz) cut mud \
(200-400 Hz), add presence (2-5 kHz) or shape a kick or a bass under 100 Hz.";

/// The most biquads a cut is made of: 48 dB an octave.
const CUT_STAGES: usize = 4;

/// One channel's filters, in the order the signal passes them.
#[derive(Copy, Clone, Default)]
struct Bands {
    low_cut: [Biquad; CUT_STAGES],
    low_shelf: Biquad,
    mid: [Biquad; 2],
    high_shelf: Biquad,
    high_cut: [Biquad; CUT_STAGES],
}

pub struct Eq {
    params: ParamValues,
    sample_rate: f32,
    bands: [Bands; 2],
    /// How many biquads of each cut are in use; none for a cut that is off.
    low_stages: usize,
    high_stages: usize,
}

/// How many biquads the slope choice `index` is made of.
fn stages(index: usize) -> usize {
    [1, 2, 4][index.min(2)]
}

/// The Q of each biquad of a cut made of `stages` of them: a Butterworth
/// filter. The single stage keeps the 0.707 it has had since format 1, a
/// hair under the textbook's 0.70711 (a thousandth of a dB at the corner),
/// so that a song saved with it renders to the same samples.
fn cut_qs(stages: usize) -> [f32; CUT_STAGES] {
    if stages <= 1 { [0.707; CUT_STAGES] } else { butterworth_qs(stages) }
}

impl Eq {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            sample_rate: 48_000.0,
            bands: [Bands::default(); 2],
            low_stages: 0,
            high_stages: 0,
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        let low_cut = p.get(P::LowCut as usize);
        let high_cut = p.get(P::HighCut as usize);
        self.low_stages = if low_cut > 12.0 { stages(p.index(P::LowCutSlope as usize)) } else { 0 };
        self.high_stages = if high_cut < 19_900.0 { stages(p.index(P::HighCutSlope as usize)) } else { 0 };
        let low_qs = cut_qs(self.low_stages);
        let high_qs = cut_qs(self.high_stages);
        for channel in &mut self.bands {
            for (stage, q) in channel.low_cut.iter_mut().zip(low_qs).take(self.low_stages) {
                stage.set_high_pass(low_cut, q, sr);
            }
            channel.low_shelf.set_low_shelf(
                p.get(P::LowFreq as usize),
                p.get(P::LowGain as usize),
                p.get(P::LowQ as usize),
                sr,
            );
            channel.mid[0].set_peak(
                p.get(P::Mid1Freq as usize),
                p.get(P::Mid1Gain as usize),
                p.get(P::Mid1Q as usize),
                sr,
            );
            channel.mid[1].set_peak(
                p.get(P::Mid2Freq as usize),
                p.get(P::Mid2Gain as usize),
                p.get(P::Mid2Q as usize),
                sr,
            );
            channel.high_shelf.set_high_shelf(
                p.get(P::HighFreq as usize),
                p.get(P::HighGain as usize),
                p.get(P::HighQ as usize),
                sr,
            );
            for (stage, q) in channel.high_cut.iter_mut().zip(high_qs).take(self.high_stages) {
                stage.set_low_pass(high_cut, q, sr);
            }
        }
        self.params.changed = false;
    }
}

impl Default for Eq {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProcessor for Eq {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate as f32;
        self.params.changed = true;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
        if self.params.changed {
            // A cut that is switched in, or made steeper, starts from rest.
            let (low, high) = (self.low_stages, self.high_stages);
            self.refresh();
            for bands in &mut self.bands {
                bands.low_cut.iter_mut().skip(low).for_each(Biquad::reset);
                bands.high_cut.iter_mut().skip(high).for_each(Biquad::reset);
            }
        }
        let (low, high) = (self.low_stages, self.high_stages);
        for (samples, bands) in [left, right].into_iter().zip(self.bands.iter_mut()) {
            for sample in samples.iter_mut() {
                let mut x = *sample;
                for stage in &mut bands.low_cut[..low] {
                    x = stage.tick(x);
                }
                x = bands.low_shelf.tick(x);
                x = bands.mid[0].tick(x);
                x = bands.mid[1].tick(x);
                x = bands.high_shelf.tick(x);
                for stage in &mut bands.high_cut[..high] {
                    x = stage.tick(x);
                }
                *sample = x;
            }
        }
    }

    fn reset(&mut self) {
        for bands in &mut self.bands {
            let Bands { low_cut, low_shelf, mid, high_shelf, high_cut } = bands;
            let all = low_cut.iter_mut().chain(mid).chain(high_cut).chain([low_shelf, high_shelf]);
            all.for_each(Biquad::reset);
        }
    }
}
