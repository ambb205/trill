//! Utility (gain, pan, width) and saturator.

use crate::define_params;
use crate::params::{ParamValues, boolean, choice, float};
use crate::util::{Smooth, crossfade_gains, db_to_gain, pan_gains};
use daw_core::device::{DeviceProcessor, Event, ProcessInfo};

// ---- Utility ---------------------------------------------------------------

mod gain {
    use super::*;

    define_params!(P, PARAMS, [
        Gain => float("gain", "Gain", -70.0, 24.0, 0.0, "dB"),
        Pan => float("pan", "Pan", -1.0, 1.0, 0.0, ""),
        Width => float("width", "Stereo Width (0 = mono, 1 = unchanged, 2 = extra wide)", 0.0, 2.0, 1.0, ""),
        Mono => boolean("mono", "Mono", false),
    ]);

    pub const DESCRIPTION: &str = "Gain, pan and stereo width. Automate `gain` for fades.";

    pub struct Utility {
        params: ParamValues,
        left: Smooth,
        right: Smooth,
    }

    impl Utility {
        pub fn new() -> Self {
            Self { params: ParamValues::new(PARAMS), left: Smooth::new(1.0), right: Smooth::new(1.0) }
        }
    }

    impl Default for Utility {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeviceProcessor for Utility {
        fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
            self.left.set_time(0.01, sample_rate as f32);
            self.right.set_time(0.01, sample_rate as f32);
            self.reset();
        }

        fn set_param(&mut self, index: usize, value: f64) {
            self.params.set(index, value);
        }

        fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
            let gain = db_to_gain(self.params.get(P::Gain as usize));
            let (pan_l, pan_r) = pan_gains(self.params.get(P::Pan as usize));
            let width = if self.params.on(P::Mono as usize) { 0.0 } else { self.params.get(P::Width as usize) };
            for i in 0..left.len() {
                let mid = (left[i] + right[i]) * 0.5;
                let side = (left[i] - right[i]) * 0.5 * width;
                left[i] = (mid + side) * self.left.next(gain * pan_l);
                right[i] = (mid - side) * self.right.next(gain * pan_r);
            }
        }

        fn reset(&mut self) {
            let gain = db_to_gain(self.params.get(P::Gain as usize));
            let (pan_l, pan_r) = pan_gains(self.params.get(P::Pan as usize));
            self.left.snap(gain * pan_l);
            self.right.snap(gain * pan_r);
        }
    }
}

pub use gain::{DESCRIPTION as UTILITY_DESCRIPTION, PARAMS as UTILITY_PARAMS, Utility};

// ---- Saturator -------------------------------------------------------------

mod saturator {
    use super::*;

    const SHAPES: &[&str] = &["soft", "hard", "tube"];

    define_params!(P, PARAMS, [
        Drive => float("drive", "Drive", 0.0, 36.0, 6.0, "dB"),
        Shape => choice("shape", "Shape", SHAPES, 0),
        Output => float("output", "Output", -24.0, 12.0, 0.0, "dB"),
        Mix => float("mix", "Mix", 0.0, 1.0, 1.0, ""),
    ]);

    pub const DESCRIPTION: &str = "Waveshaping distortion: `soft` is warm tape-like saturation, `hard` clips, \
`tube` adds even harmonics. Level is compensated as drive rises.";

    pub struct Saturator {
        params: ParamValues,
    }

    impl Saturator {
        pub fn new() -> Self {
            Self { params: ParamValues::new(PARAMS) }
        }
    }

    #[inline]
    fn shape(kind: usize, x: f32) -> f32 {
        match kind {
            0 => x.tanh(),
            1 => x.clamp(-1.0, 1.0),
            // Asymmetric: the positive half saturates sooner than the negative.
            _ => {
                if x >= 0.0 {
                    x.tanh()
                } else {
                    (x * 0.7).tanh() / 0.7
                }
            }
        }
    }

    impl Default for Saturator {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeviceProcessor for Saturator {
        fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

        fn set_param(&mut self, index: usize, value: f64) {
            self.params.set(index, value);
        }

        fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
            let drive = db_to_gain(self.params.get(P::Drive as usize));
            let kind = self.params.index(P::Shape as usize);
            // Partial compensation keeps perceived loudness roughly steady.
            let trim = db_to_gain(self.params.get(P::Output as usize)) / drive.sqrt();
            let mix = self.params.get(P::Mix as usize);
            let (dry, wet) = if mix >= 1.0 { (0.0, 1.0) } else { crossfade_gains(mix) };
            for samples in [left, right] {
                for sample in samples.iter_mut() {
                    *sample = *sample * dry + shape(kind, *sample * drive) * trim * wet;
                }
            }
        }

        fn reset(&mut self) {}
    }
}

pub use saturator::{DESCRIPTION as SATURATOR_DESCRIPTION, PARAMS as SATURATOR_PARAMS, Saturator};
