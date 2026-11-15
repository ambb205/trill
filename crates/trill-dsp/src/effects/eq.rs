//! EQ: low cut, low shelf, two peaking bands, high shelf, high cut.

use crate::define_params;
use crate::params::{ParamValues, float};
use crate::util::Biquad;
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

define_params!(P, PARAMS, [
    LowCut => float("low_cut", "Low Cut (10 = off)", 10.0, 1000.0, 10.0, "Hz"),
    LowFreq => float("low_freq", "Low Shelf Frequency", 20.0, 1000.0, 100.0, "Hz"),
    LowGain => float("low_gain", "Low Shelf Gain", -18.0, 18.0, 0.0, "dB"),
    Mid1Freq => float("mid1_freq", "Mid 1 Frequency", 100.0, 10000.0, 500.0, "Hz"),
    Mid1Gain => float("mid1_gain", "Mid 1 Gain", -18.0, 18.0, 0.0, "dB"),
    Mid1Q => float("mid1_q", "Mid 1 Q", 0.2, 10.0, 1.0, ""),
    Mid2Freq => float("mid2_freq", "Mid 2 Frequency", 200.0, 16000.0, 3000.0, "Hz"),
    Mid2Gain => float("mid2_gain", "Mid 2 Gain", -18.0, 18.0, 0.0, "dB"),
    Mid2Q => float("mid2_q", "Mid 2 Q", 0.2, 10.0, 1.0, ""),
    HighFreq => float("high_freq", "High Shelf Frequency", 1000.0, 20000.0, 8000.0, "Hz"),
    HighGain => float("high_gain", "High Shelf Gain", -18.0, 18.0, 0.0, "dB"),
    HighCut => float("high_cut", "High Cut (20000 = off)", 1000.0, 20000.0, 20000.0, "Hz"),
]);

pub const DESCRIPTION: &str = "Six-band equalizer. Use `low_cut` to clear rumble from anything that is not \
bass or kick, the shelves for broad tone, and the two mid bands to cut mud (200-400 Hz) or add presence (2-5 kHz).";

const BANDS: usize = 6;

pub struct Eq {
    params: ParamValues,
    sample_rate: f32,
    /// `[channel][band]`.
    bands: [[Biquad; BANDS]; 2],
}

impl Eq {
    pub fn new() -> Self {
        Self { params: ParamValues::new(PARAMS), sample_rate: 48_000.0, bands: [[Biquad::default(); BANDS]; 2] }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        for channel in &mut self.bands {
            let low_cut = p.get(P::LowCut as usize);
            if low_cut > 12.0 {
                channel[0].set_high_pass(low_cut, 0.707, sr);
            } else {
                channel[0].set_bypass();
            }
            channel[1].set_low_shelf(p.get(P::LowFreq as usize), p.get(P::LowGain as usize), sr);
            channel[2].set_peak(p.get(P::Mid1Freq as usize), p.get(P::Mid1Gain as usize), p.get(P::Mid1Q as usize), sr);
            channel[3].set_peak(p.get(P::Mid2Freq as usize), p.get(P::Mid2Gain as usize), p.get(P::Mid2Q as usize), sr);
            channel[4].set_high_shelf(p.get(P::HighFreq as usize), p.get(P::HighGain as usize), sr);
            let high_cut = p.get(P::HighCut as usize);
            if high_cut < 19_900.0 {
                channel[5].set_low_pass(high_cut, 0.707, sr);
            } else {
                channel[5].set_bypass();
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
            self.refresh();
        }
        for (samples, bands) in [left, right].into_iter().zip(self.bands.iter_mut()) {
            for sample in samples.iter_mut() {
                let mut x = *sample;
                for band in bands.iter_mut() {
                    x = band.tick(x);
                }
                *sample = x;
            }
        }
    }

    fn reset(&mut self) {
        for band in self.bands.iter_mut().flatten() {
            band.reset();
        }
    }
}
