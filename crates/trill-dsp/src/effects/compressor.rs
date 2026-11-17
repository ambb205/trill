//! Compressor: two detectors (one for a track, one for a bus), automatic
//! release and makeup, and a key input with a filter of its own.

use super::dynamics::ReductionMeter;
use crate::define_params;
use crate::params::{ParamValues, boolean, choice, float};
use crate::util::{Biquad, crossfade_gains, db_to_gain, gain_to_db, smooth_coef};
use std::f32::consts::FRAC_1_SQRT_2;
use trill_core::device::{DeviceProcessor, Event, GainReduction, KeyInput, ProcessInfo};

/// How the compressor reads the level of what it hears.
pub const DETECTORS: &[&str] = &["peak", "bus"];

define_params!(P, PARAMS, [
    Threshold => float("threshold", "Threshold (the peak level above which it turns down)", -60.0, 0.0, -18.0, "dB"),
    Ratio => float("ratio", "Ratio", 1.0, 20.0, 4.0, ":1"),
    Attack => float("attack", "Attack", 0.1, 200.0, 10.0, "ms"),
    Release => float("release", "Release (not used with Auto Release)", 5.0, 2000.0, 120.0, "ms"),
    Knee => float("knee", "Knee", 0.0, 24.0, 6.0, "dB"),
    Makeup => float("makeup", "Makeup Gain", -12.0, 24.0, 0.0, "dB"),
    Mix => float("mix", "Mix (below 1 = parallel compression)", 0.0, 1.0, 1.0, ""),
    Detector => choice(
        "detector",
        "Detector (peak = follows every sample, for a single track; bus = holds the peaks, for a group or the master)",
        DETECTORS,
        0
    ),
    AutoRelease => boolean(
        "auto_release",
        "Auto Release (lets go of a single hit in 0.3 s and of sustained loudness over seconds)",
        false
    ),
    AutoMakeup => boolean(
        "auto_makeup",
        "Auto Makeup (adds 0.72 x -threshold x (1 - 1/ratio) dB on top of Makeup Gain)",
        false
    ),
    KeyLowCut => float(
        "key_low_cut",
        "Key Low Cut (filters what the detector hears, not the sound; 20 = off)",
        20.0,
        2000.0,
        20.0,
        "Hz"
    ),
    KeyHighCut => float(
        "key_high_cut",
        "Key High Cut (filters what the detector hears, not the sound; 20000 = off)",
        200.0,
        20000.0,
        20000.0,
        "Hz"
    ),
]);

pub const DESCRIPTION: &str = "Stereo-linked compressor with two detectors. \"peak\" follows every sample of what \
it hears: the usual track compressor (vocals, bass, guitars: attack 1-10 ms, release 80-200 ms); with a slow attack \
it only catches what stays loud, so on a full mix it does little. \"bus\" holds the peaks for as long as `release` \
and builds the reduction over `attack`, so a slow attack still works on the body of a mix: use it on a group or \
the master (threshold about 10 dB under the peaks, ratio 2, attack 10-30 ms, `auto_release` on: about 2 dB off). \
`auto_makeup` adds 0.72 x -threshold x (1 - 1/ratio) dB (threshold -24 at ratio 4: +13 dB; Ableton Live's Makeup \
comes to the same), so a track comes out louder the less of it is over the threshold; `makeup` comes on top. What \
it hears is its own input, \
or another track when the device is given a `sidechain` (a kick ducking a bass: sidechain = the kick's track, \
attack 1 ms, release 100-200 ms). `key_low_cut` and `key_high_cut` filter what it hears, never the sound \
(key_low_cut 100-150 Hz on the master keeps the kick from pumping the mix). `render` and `measure` report how many \
dB it took off.";

/// The share of what a full-scale signal would lose that automatic makeup
/// gives back. Ableton Live's Compressor with Makeup on comes to the same:
/// between 0.70 and 0.74 of it, measured on six tracks of one song.
pub const AUTO_MAKEUP: f32 = 0.72;

/// Automatic release: how fast the reduction comes back after a single hit,
/// and how slowly the level it comes back to follows the music, up and
/// down.
const AUTO_FAST_SECONDS: f32 = 0.3;
const AUTO_SLOW_UP_SECONDS: f32 = 2.0;
const AUTO_SLOW_DOWN_SECONDS: f32 = 8.0;

/// What automatic makeup adds, in dB, at `threshold` dB and `ratio`.
pub fn auto_makeup_db(threshold: f32, ratio: f32) -> f32 {
    AUTO_MAKEUP * -threshold * (1.0 - 1.0 / ratio.max(1.0))
}

pub struct Compressor {
    params: ParamValues,
    sample_rate: f32,
    /// What the detector asks for, in dB taken off (0 or more), before
    /// automatic release and, with the bus detector, before the attack:
    /// the peak detector's own reduction, or the bus detector's held peak.
    quick: f32,
    /// With automatic release, what the reduction falls back to: it
    /// follows `quick` over seconds.
    slow: f32,
    /// What is taken off, in dB, as it is applied.
    reduction: f32,
    key_low: [Biquad; 2],
    key_high: [Biquad; 2],
    taken_off: ReductionMeter,
    threshold: f32,
    /// The linear level under which nothing is asked for.
    quiet: f32,
    slope: f32,
    knee: f32,
    attack: f32,
    release: f32,
    bus: bool,
    auto_release: bool,
    slow_up: f32,
    slow_down: f32,
    filter_low: bool,
    filter_high: bool,
    makeup: f32,
    dry: f32,
    wet: f32,
}

impl Compressor {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            sample_rate: 48_000.0,
            quick: 0.0,
            slow: 0.0,
            reduction: 0.0,
            key_low: [Biquad::default(); 2],
            key_high: [Biquad::default(); 2],
            taken_off: ReductionMeter::default(),
            threshold: 0.0,
            quiet: 0.0,
            slope: 0.0,
            knee: 0.0,
            attack: 1.0,
            release: 1.0,
            bus: false,
            auto_release: false,
            slow_up: 1.0,
            slow_down: 1.0,
            filter_low: false,
            filter_high: false,
            makeup: 1.0,
            dry: 0.0,
            wet: 1.0,
        }
    }

    fn refresh(&mut self) {
        let p = &self.params;
        let sr = self.sample_rate;
        self.threshold = p.get(P::Threshold as usize);
        let ratio = p.get(P::Ratio as usize);
        self.slope = 1.0 / ratio - 1.0;
        self.knee = p.get(P::Knee as usize);
        self.quiet = db_to_gain(self.threshold - self.knee * 0.5);
        self.attack = smooth_coef(p.get(P::Attack as usize) * 0.001, sr);
        self.bus = p.index(P::Detector as usize) == 1;
        self.auto_release = p.on(P::AutoRelease as usize);
        let release = if self.auto_release { AUTO_FAST_SECONDS } else { p.get(P::Release as usize) * 0.001 };
        self.release = smooth_coef(release, sr);
        self.slow_up = smooth_coef(AUTO_SLOW_UP_SECONDS, sr);
        self.slow_down = smooth_coef(AUTO_SLOW_DOWN_SECONDS, sr);
        let mut makeup = p.get(P::Makeup as usize);
        if p.on(P::AutoMakeup as usize) {
            makeup += auto_makeup_db(self.threshold, ratio);
        }
        self.makeup = db_to_gain(makeup);
        let (low, high) = (p.get(P::KeyLowCut as usize), p.get(P::KeyHighCut as usize));
        self.filter_low = low > 20.5;
        self.filter_high = high < 19_900.0;
        for channel in 0..2 {
            self.key_low[channel].set_high_pass(low, FRAC_1_SQRT_2, sr);
            self.key_high[channel].set_low_pass(high, FRAC_1_SQRT_2, sr);
        }
        (self.dry, self.wet) = crossfade_gains(p.get(P::Mix as usize));
        if p.get(P::Mix as usize) >= 1.0 {
            (self.dry, self.wet) = (0.0, 1.0);
        }
        self.params.changed = false;
    }

    /// Static curve: gain change in dB for an input level in dB.
    #[inline]
    fn curve(&self, level_db: f32) -> f32 {
        let over = level_db - self.threshold;
        if 2.0 * over < -self.knee {
            0.0
        // A hard knee has no curve to be on; without this test a level
        // exactly at the threshold would divide zero by zero.
        } else if self.knee > 0.0 && 2.0 * over.abs() <= self.knee {
            let x = over + self.knee * 0.5;
            self.slope * x * x / (2.0 * self.knee)
        } else {
            self.slope * over
        }
    }

    /// The level of the key at one frame, through the key's filter.
    #[inline]
    fn heard(&mut self, mut left: f32, mut right: f32) -> f32 {
        if self.filter_low {
            left = self.key_low[0].tick(left);
            right = self.key_low[1].tick(right);
        }
        if self.filter_high {
            left = self.key_high[0].tick(left);
            right = self.key_high[1].tick(right);
        }
        left.abs().max(right.abs())
    }

    /// The peak detector: the reduction moves towards what each sample's
    /// own magnitude asks for, by the attack when that is more and by the
    /// release when it is less. It is the detector of the textbook, and
    /// the one songs of format 1 were made with: the arithmetic is kept to
    /// the letter, so that such a song renders to the same samples.
    #[inline]
    fn peak(&mut self, level: f32) -> f32 {
        let mut gain_db = -self.quick;
        let target = self.curve(gain_to_db(level));
        let coef = if target < gain_db { self.attack } else { self.release };
        gain_db += (target - gain_db) * coef;
        self.quick = -gain_db;
        self.quick
    }

    /// The first half of the bus detector: the reduction the level asks
    /// for (the static curve), its peaks held and let go of over the
    /// release. The attack comes after it. Holding first is what makes a
    /// slow attack work on a mix: between the peaks of a waveform, and
    /// between the hits of a drum, there is still something to build
    /// towards.
    #[inline]
    fn held(&mut self, level: f32) -> f32 {
        let asked = if level > self.quiet { -self.curve(gain_to_db(level)) } else { 0.0 };
        self.quick = asked.max(self.quick - self.quick * self.release);
        self.quick
    }

    /// Automatic release: `quick`, which is let go of in a third of a
    /// second, or what a follower of it that moves over seconds has got
    /// to, whichever is more. One hit is over at once; a loud passage has
    /// filled the slow follower, which lets go of it over seconds.
    #[inline]
    fn sustained(&mut self, quick: f32) -> f32 {
        let coef = if quick > self.slow { self.slow_up } else { self.slow_down };
        self.slow += (quick - self.slow) * coef;
        quick.max(self.slow)
    }

    fn run(&mut self, left: &mut [f32], right: &mut [f32], key: Option<KeyInput<'_>>) {
        if self.params.changed {
            self.refresh();
        }
        for i in 0..left.len() {
            let (l, r) = (left[i], right[i]);
            let level = match &key {
                Some(key) => self.heard(key.left[i], key.right[i]),
                None => self.heard(l, r),
            };
            let mut wanted = if self.bus { self.held(level) } else { self.peak(level) };
            if self.auto_release {
                wanted = self.sustained(wanted);
            }
            if self.bus {
                self.reduction += (wanted - self.reduction) * self.attack;
            } else {
                self.reduction = wanted;
            }
            self.taken_off.record(self.reduction);
            let gain = db_to_gain(-self.reduction) * self.makeup;
            left[i] = l * self.dry + l * gain * self.wet;
            right[i] = r * self.dry + r * gain * self.wet;
        }
    }
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProcessor for Compressor {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate as f32;
        self.params.changed = true;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
        self.run(left, right, None);
    }

    fn takes_key(&self) -> bool {
        true
    }

    fn process_keyed(
        &mut self,
        _info: &ProcessInfo,
        _events: &[Event],
        left: &mut [f32],
        right: &mut [f32],
        key: KeyInput<'_>,
    ) {
        self.run(left, right, Some(key));
    }

    fn reset(&mut self) {
        self.quick = 0.0;
        self.slow = 0.0;
        self.reduction = 0.0;
        for filter in self.key_low.iter_mut().chain(self.key_high.iter_mut()) {
            filter.reset();
        }
    }

    fn gain_reduction(&mut self) -> Option<GainReduction> {
        Some(self.taken_off.take())
    }
}
