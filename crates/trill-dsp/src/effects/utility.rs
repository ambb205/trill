//! Utility (gain, pan, width) and saturator.

use crate::define_params;
use crate::params::{ParamValues, boolean, choice, float};
use crate::util::{SILENT_DB, Smooth, crossfade_gains, db_to_gain, level_gain, pan_gains};
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

// ---- Utility ---------------------------------------------------------------

mod gain {
    use super::*;

    define_params!(P, PARAMS, [
        Gain => float("gain", "Gain (silent at -70, as the bottom of a fader is)", SILENT_DB as f64, 24.0, 0.0, "dB"),
        Pan => float("pan", "Pan", -1.0, 1.0, 0.0, ""),
        Width => float("width", "Stereo Width (0 = mono, 1 = unchanged, 2 = extra wide)", 0.0, 2.0, 1.0, ""),
        Mono => boolean("mono", "Mono", false),
        Legacy => boolean(
            "legacy",
            "Legacy (the gain of songs saved before format 3: its lowest value is 70 dB down and not silence)",
            false
        ),
    ]);

    pub const DESCRIPTION: &str = "Gain, pan and stereo width. Automate `gain` for fades: its lowest value, \
-70 dB, is silence, so a fade written down to it ends in nothing.";

    pub struct Utility {
        params: ParamValues,
        left: Smooth,
        right: Smooth,
        /// False until the first block has been played: that one starts
        /// where the gain is set, instead of coming to it from somewhere.
        started: bool,
    }

    impl Utility {
        pub fn new() -> Self {
            Self { params: ParamValues::new(PARAMS), left: Smooth::new(1.0), right: Smooth::new(1.0), started: false }
        }

        /// The gain of each side, as the parameters have it.
        fn gains(&self) -> (f32, f32) {
            let db = self.params.get(P::Gain as usize);
            let gain = if self.params.on(P::Legacy as usize) { db_to_gain(db) } else { level_gain(db) };
            let (pan_l, pan_r) = pan_gains(self.params.get(P::Pan as usize));
            (gain * pan_l, gain * pan_r)
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
            self.started = false;
            self.reset();
        }

        fn set_param(&mut self, index: usize, value: f64) {
            self.params.set(index, value);
        }

        fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
            let (gain_l, gain_r) = self.gains();
            // A device is told its parameters after it is prepared. A song
            // saved before format 3 heard the gain come from unity to where
            // it was set over the first hundredth of a second, and still
            // does; since then the first block starts where the gain is.
            if !self.started {
                self.started = true;
                if !self.params.on(P::Legacy as usize) {
                    self.left.snap(gain_l);
                    self.right.snap(gain_r);
                }
            }
            let width = if self.params.on(P::Mono as usize) { 0.0 } else { self.params.get(P::Width as usize) };
            for i in 0..left.len() {
                let mid = (left[i] + right[i]) * 0.5;
                let side = (left[i] - right[i]) * 0.5 * width;
                left[i] = (mid + side) * self.left.next(gain_l);
                right[i] = (mid - side) * self.right.next(gain_r);
            }
            // A gain on its way to nothing gets there: 120 dB down it is
            // let go of, and what follows is silence itself.
            for (smooth, gain) in [(&mut self.left, gain_l), (&mut self.right, gain_r)] {
                if gain == 0.0 && smooth.next(0.0).abs() < 1e-6 {
                    smooth.snap(0.0);
                }
            }
        }

        fn reset(&mut self) {
            let (gain_l, gain_r) = self.gains();
            self.left.snap(gain_l);
            self.right.snap(gain_r);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::testing::*;

        fn utility(params: &[(&str, f64)]) -> Utility {
            let mut device = Utility::new();
            device.prepare(48_000.0, 512);
            for (id, value) in params {
                let index = PARAMS.iter().position(|def| def.id == *id).unwrap();
                device.set_param(index, *value);
            }
            device
        }

        /// What half a second of a steady signal comes out as.
        fn through(device: &mut Utility) -> Vec<f32> {
            let (mut left, mut right) = (vec![0.5f32; 24_000], vec![0.5f32; 24_000]);
            for (left, right) in left.chunks_mut(256).zip(right.chunks_mut(256)) {
                device.process(&info(48_000.0), &[], left, right);
            }
            left
        }

        #[test]
        fn the_lowest_gain_is_silence_and_the_first_block_starts_where_the_gain_is() {
            let out = through(&mut utility(&[("gain", -70.0)]));
            assert_eq!(peak(&out), 0.0, "silence from the first sample");
            let out = through(&mut utility(&[("gain", -69.0)]));
            assert!(
                (out[0] / 0.5 - db_to_gain(-69.0)).abs() < 1e-7 && (out[20_000] / 0.5 - db_to_gain(-69.0)).abs() < 1e-7
            );
            let out = through(&mut utility(&[("gain", -12.0)]));
            assert!(
                (out[0] - 0.5 * db_to_gain(-12.0)).abs() < 1e-6,
                "nothing of the signal at full level first: {}",
                out[0]
            );

            // A gain that is moved while it plays takes its hundredth of a
            // second, down to silence as anywhere else.
            let mut device = utility(&[]);
            through(&mut device);
            device.set_param(P::Gain as usize, -70.0);
            let out = through(&mut device);
            assert!(out[0] > 0.45 && out[240] < 0.35 && out[240] > 0.1, "{} then {}", out[0], out[240]);
            assert!(out[4_800] < 1e-4, "and is gone: {}", out[4_800]);
        }

        #[test]
        fn a_song_saved_before_keeps_a_gain_that_was_70_db_down() {
            let out = through(&mut utility(&[("gain", -70.0), ("legacy", 1.0)]));
            assert!((out[20_000] / 0.5 - db_to_gain(-70.0)).abs() < 1e-7, "{}", out[20_000]);
            assert!(out[0] > 0.45, "and came down to it from unity, as it did: {}", out[0]);
        }
    }
}

pub use gain::{DESCRIPTION as UTILITY_DESCRIPTION, PARAMS as UTILITY_PARAMS, Utility};

// ---- Saturator -------------------------------------------------------------

mod saturator {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    const SHAPES: &[&str] = &["soft", "hard", "tube", "sine"];

    define_params!(P, PARAMS, [
        Drive => float(
            "drive",
            "Drive (gain into the curve; a signal that peaks at 0 dB after it reaches the curve's ceiling)",
            0.0,
            36.0,
            6.0,
            "dB"
        ),
        Shape => choice("shape", "Shape", SHAPES, 0),
        Output => float(
            "output",
            "Output (gain after the curve, which never exceeds 0 dB before it)",
            -24.0,
            12.0,
            0.0,
            "dB"
        ),
        Mix => float("mix", "Mix", 0.0, 1.0, 1.0, ""),
        Legacy => boolean(
            "legacy",
            "Legacy (the level of songs saved before format 3: turned down by half the drive, in dB)",
            false
        ),
    ]);

    pub const DESCRIPTION: &str = "Waveshaping distortion: the signal is turned up by `drive`, bent by a curve \
that lets nothing over 0 dB, and turned by `output`. `soft` is warm tape-like saturation (a hyperbolic tangent: it \
squeezes from far below the ceiling and never quite reaches it), `sine` is a quarter of a sine wave up to the \
ceiling and flat above it (clean until near the ceiling, then strong odd overtones: what makes a sine bass heard on \
small speakers), `hard` clips, `tube` adds even overtones. The level is not compensated: more drive is louder until \
the curve holds it, so trim with `output`.";

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
            2 => {
                if x >= 0.0 {
                    x.tanh()
                } else {
                    (x * 0.7).tanh() / 0.7
                }
            }
            // A quarter of a sine up to the ceiling, where it is level, and
            // the ceiling from there on.
            _ => (x.clamp(-1.0, 1.0) * FRAC_PI_2).sin(),
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
            let mut trim = db_to_gain(self.params.get(P::Output as usize));
            if self.params.on(P::Legacy as usize) {
                // Before format 3: half the drive taken off again.
                trim /= drive.sqrt();
            }
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

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::testing::*;

        fn saturated(params: &[(&str, f64)], input: &[f32]) -> Vec<f32> {
            let mut device = Saturator::new();
            device.prepare(48_000.0, 512);
            for (id, value) in params {
                let index = PARAMS.iter().position(|def| def.id == *id).unwrap();
                device.set_param(index, *value);
            }
            let (mut left, mut right) = (input.to_vec(), input.to_vec());
            device.process(&info(48_000.0), &[], &mut left, &mut right);
            left
        }

        #[test]
        fn the_level_is_not_turned_down_as_the_drive_rises() {
            // A quiet signal is in the straight part of every curve: it
            // comes out turned up by the drive, and by nothing else.
            let quiet = vec![0.001f32; 16];
            for (shape, slope) in [(0.0, 1.0), (1.0, 1.0), (2.0, 1.0), (3.0, FRAC_PI_2)] {
                for drive in [0.0f32, 9.0, 24.0] {
                    let out = saturated(&[("shape", shape), ("drive", f64::from(drive))], &quiet);
                    let wanted = 0.001 * db_to_gain(drive) * slope;
                    assert!((out[0] / wanted - 1.0).abs() < 0.01, "shape {shape} at {drive} dB: {}", out[0]);
                }
            }
            // A loud one is held at the ceiling, which the output then moves.
            for shape in [0.0, 1.0, 3.0] {
                let out = saturated(&[("shape", shape), ("drive", 30.0), ("output", -8.0)], &[0.9, -0.9]);
                assert!(
                    (out[0] - db_to_gain(-8.0)).abs() < 1e-3 && (out[1] + db_to_gain(-8.0)).abs() < 1e-3,
                    "{out:?}"
                );
            }
            // A song saved before has half the drive taken off again.
            let old = saturated(&[("drive", 18.0), ("legacy", 1.0)], &quiet);
            assert!((old[0] / (0.001 * db_to_gain(9.0)) - 1.0).abs() < 0.01, "{}", old[0]);
        }

        #[test]
        fn the_sine_curve_is_clean_under_the_ceiling_and_flat_above_it() {
            // Exactly a quarter of a sine wave: level where it meets the ceiling.
            for (input, wanted) in
                [(0.0f32, 0.0f32), (0.5, 0.70711), (1.0, 1.0), (-1.0, -1.0), (1.7, 1.0), (-3.0, -1.0)]
            {
                let out = saturated(&[("shape", 3.0), ("drive", 0.0)], &[input]);
                assert!((out[0] - wanted).abs() < 1e-4, "{input} -> {}", out[0]);
            }
            // A sine driven over the ceiling grows odd overtones, and more
            // of them than the soft curve gives it at the same drive.
            let tone: Vec<f32> =
                (0..4_800).map(|n| 0.6 * (std::f32::consts::TAU * 100.0 * n as f32 / 48_000.0).sin()).collect();
            let overtone = |shape: f64, harmonic: f32| {
                let out = saturated(&[("shape", shape), ("drive", 9.0)], &tone);
                amplitude(&out, f64::from(100.0 * harmonic), 48_000.0) / amplitude(&out, 100.0, 48_000.0)
            };
            assert!(overtone(3.0, 2.0) < 1e-3, "no even overtones: {}", overtone(3.0, 2.0));
            assert!(
                overtone(3.0, 3.0) > 1.5 * overtone(0.0, 3.0),
                "{} against {}",
                overtone(3.0, 3.0),
                overtone(0.0, 3.0)
            );
            assert!(
                overtone(3.0, 5.0) > 2.0 * overtone(0.0, 5.0),
                "{} against {}",
                overtone(3.0, 5.0),
                overtone(0.0, 5.0)
            );
        }
    }
}

pub use saturator::{DESCRIPTION as SATURATOR_DESCRIPTION, PARAMS as SATURATOR_PARAMS, Saturator};
