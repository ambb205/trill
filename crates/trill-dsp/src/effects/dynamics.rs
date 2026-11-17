//! Look-ahead limiter, and the count of what a dynamics device takes off.

use crate::define_params;
use crate::params::{ParamValues, float};
use crate::util::{DelayLine, db_to_gain, gain_to_db, smooth_coef};
use trill_core::device::{DeviceProcessor, Event, GainReduction, ProcessInfo};

/// Keeps count of what a dynamics device takes off, a few additions a
/// sample, for [`DeviceProcessor::gain_reduction`].
#[derive(Default)]
pub(super) struct ReductionMeter {
    /// The most taken off, in dB.
    most: f32,
    /// The dB taken off, summed over the frames it was half a dB or more.
    sum: f64,
    active: u64,
    frames: u64,
}

impl ReductionMeter {
    /// Below this, in dB, a device is not said to be doing anything.
    const WORTH_THE_NAME: f32 = 0.5;

    /// One frame on which `taken_off` dB (0 or more) was taken off.
    #[inline]
    pub(super) fn record(&mut self, taken_off: f32) {
        self.frames += 1;
        if taken_off >= Self::WORTH_THE_NAME {
            self.active += 1;
            self.sum += f64::from(taken_off);
            self.most = self.most.max(taken_off);
        }
    }

    /// What was counted since the last call, which starts the count anew.
    pub(super) fn take(&mut self) -> GainReduction {
        let counted = std::mem::take(self);
        if counted.active == 0 {
            return GainReduction::default();
        }
        GainReduction {
            most_db: counted.most,
            typical_db: (counted.sum / counted.active as f64) as f32,
            active: (counted.active as f64 / counted.frames.max(1) as f64) as f32,
        }
    }
}

// ---- Limiter ---------------------------------------------------------------

mod limiter {
    use super::*;

    define_params!(P, PARAMS, [
        Gain => float("gain", "Input Gain", 0.0, 24.0, 0.0, "dB"),
        Ceiling => float("ceiling", "Ceiling", -12.0, 0.0, -0.3, "dB"),
        Release => float("release", "Release", 10.0, 1000.0, 100.0, "ms"),
    ]);

    pub const DESCRIPTION: &str = "Look-ahead brickwall limiter: output never exceeds `ceiling`. Put it last on \
the master; raise `gain` to make the mix louder.";

    const LOOKAHEAD_SECONDS: f32 = 0.0015;

    /// The gain is the minimum required gain over the look-ahead window,
    /// smoothed by a moving average of the same length. Every window that
    /// contributes to the average contains the sample being output, so the
    /// applied gain can never exceed what that sample needs.
    pub struct Limiter {
        params: ParamValues,
        sample_rate: f32,
        window: usize,
        delay: [DelayLine; 2],
        required: Vec<f32>,
        averaged: Vec<f32>,
        index: usize,
        minimum: f32,
        minimum_age: usize,
        released: f32,
        sum: f32,
        gain: f32,
        ceiling: f32,
        release: f32,
        taken_off: ReductionMeter,
        /// The gain last looked at for the count, and what it is in dB:
        /// while the limiter is not limiting the gain stays at 1, and the
        /// count costs no logarithm.
        counted: (f32, f32),
    }

    impl Limiter {
        pub fn new() -> Self {
            Self {
                params: ParamValues::new(PARAMS),
                sample_rate: 48_000.0,
                window: 1,
                delay: [DelayLine::new(), DelayLine::new()],
                required: vec![1.0],
                averaged: vec![1.0],
                index: 0,
                minimum: 1.0,
                minimum_age: 0,
                released: 1.0,
                sum: 1.0,
                gain: 1.0,
                ceiling: 1.0,
                release: 1.0,
                taken_off: ReductionMeter::default(),
                counted: (1.0, 0.0),
            }
        }

        fn refresh(&mut self) {
            let p = &self.params;
            self.gain = db_to_gain(p.get(P::Gain as usize));
            self.ceiling = db_to_gain(p.get(P::Ceiling as usize));
            self.release = smooth_coef(p.get(P::Release as usize) * 0.001, self.sample_rate);
            self.params.changed = false;
        }
    }

    impl Default for Limiter {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeviceProcessor for Limiter {
        fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
            self.sample_rate = sample_rate as f32;
            self.window = ((LOOKAHEAD_SECONDS * self.sample_rate) as usize).max(2);
            for line in &mut self.delay {
                line.resize(self.window + 4);
            }
            self.required = vec![1.0; self.window];
            self.averaged = vec![1.0; self.window];
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
            let window = self.window;
            for i in 0..left.len() {
                let l = left[i] * self.gain;
                let r = right[i] * self.gain;
                let peak = l.abs().max(r.abs());
                let required = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };

                // Sliding-window minimum of the required gain.
                self.required[self.index] = required;
                if required <= self.minimum {
                    self.minimum = required;
                    self.minimum_age = 0;
                } else {
                    self.minimum_age += 1;
                    if self.minimum_age >= window {
                        // The old minimum left the window; find the new one
                        // and how long ago it was written.
                        self.minimum = f32::MAX;
                        for back in 0..window {
                            let value = self.required[(self.index + window - back) % window];
                            if value < self.minimum {
                                self.minimum = value;
                                self.minimum_age = back;
                            }
                        }
                    }
                }

                // Recover slowly, never rising above the windowed minimum.
                self.released = (self.released + (1.0 - self.released) * self.release).min(self.minimum);

                self.sum += self.released - self.averaged[self.index];
                self.averaged[self.index] = self.released;
                self.index += 1;
                if self.index == window {
                    self.index = 0;
                    // Re-sum once per window so rounding error cannot build up.
                    self.sum = self.averaged.iter().sum();
                }
                let gain = (self.sum / window as f32).min(1.0);
                if gain != self.counted.0 {
                    self.counted = (gain, -gain_to_db(gain));
                }
                self.taken_off.record(self.counted.1);

                self.delay[0].write(l);
                self.delay[1].write(r);
                left[i] = self.delay[0].tap(window) * gain;
                right[i] = self.delay[1].tap(window) * gain;
            }
        }

        fn reset(&mut self) {
            for line in &mut self.delay {
                line.clear();
            }
            self.required.fill(1.0);
            self.averaged.fill(1.0);
            self.index = 0;
            self.minimum = 1.0;
            self.minimum_age = 0;
            self.released = 1.0;
            self.sum = self.window as f32;
        }

        fn latency_samples(&self) -> u32 {
            (self.window - 1) as u32
        }

        fn gain_reduction(&mut self) -> Option<GainReduction> {
            Some(self.taken_off.take())
        }
    }
}

pub use limiter::{DESCRIPTION as LIMITER_DESCRIPTION, Limiter, PARAMS as LIMITER_PARAMS};
