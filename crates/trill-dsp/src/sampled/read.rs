//! Reading a recording at another pitch than its own.
//!
//! A note between two recorded ones plays the nearest recording faster or
//! slower. Reading between the stored frames is done as the sampler does
//! it ([`crate::sample`]): each output frame is a weighted sum of 32 or
//! more stored ones, a Kaiser-windowed sinc. Reading *faster* than the
//! recording was made pushes its top past what the output can hold, where
//! it would fold back as tones that are not in the instrument, so the
//! weights are widened with the speed: their cutoff comes down to what
//! survives.
//!
//! The sampler keeps half-rate copies of its one file for that. An
//! instrument is a thousand recordings, each played within a few keys of
//! its own, so here there are no copies, which would double the memory:
//! the weights alone are widened, in quarter-octave steps up to two
//! octaves. Beyond two octaves up the widest is used, and what is above it
//! folds.
//!
//! What this comes to is held in numbers by the tests below and by
//! `trill-engine/tests/instruments.rs`.

use super::pool::{Audio, GUARD};
use crate::resample::{kaiser, sinc};
use std::sync::OnceLock;

/// Zero crossings of the weights on each side of the middle before they
/// are widened: 32 points, which keeps the response flat to 20 kHz in a
/// 48 kHz recording.
const ZERO_CROSSINGS: usize = 16;
/// Positions between two frames the weights are tabulated at; in between,
/// two rows are blended.
const PHASES: usize = 256;
const KAISER_BETA: f64 = 7.0;
/// How many widths there are: quarter octaves from 1 to 4.
const WIDTHS: usize = 9;
/// How much wider than the speed the weights are made, so that the end of
/// their roll-off and not the middle of it lies on the frequency that
/// folds.
const ALIAS_MARGIN: f64 = 1.1;
/// The most weights on either side of the middle.
pub const MAX_HALF: usize = 64;

/// The weights for one width.
pub struct Kernel {
    /// Weights on each side of where is read; a multiple of four.
    half: usize,
    /// `PHASES + 1` rows of `2 * half` weights.
    rows: Vec<f32>,
}

impl Kernel {
    fn new(width: f64) -> Self {
        let half = ((ZERO_CROSSINGS as f64 * width).ceil() as usize).next_multiple_of(4);
        let taps = half * 2;
        let mut rows = Vec::with_capacity((PHASES + 1) * taps);
        for phase in 0..=PHASES {
            let fraction = phase as f64 / PHASES as f64;
            let row: Vec<f64> = (0..taps)
                .map(|tap| {
                    let distance = (tap as f64 - (half - 1) as f64 - fraction) / width;
                    sinc(distance) * kaiser(distance / ZERO_CROSSINGS as f64, KAISER_BETA)
                })
                .collect();
            // Every row sums to one: a steady level reads back as itself
            // at every position.
            let sum: f64 = row.iter().sum();
            rows.extend(row.iter().map(|weight| (weight / sum) as f32));
        }
        Self { half, rows }
    }

    /// How many frames either side of where it reads this reaches.
    pub fn half(&self) -> usize {
        self.half
    }

    #[inline]
    fn rows(&self, fraction: f64) -> (&[f32], &[f32], f32) {
        let phase = fraction * PHASES as f64;
        let row = (phase as usize).min(PHASES - 1);
        let taps = self.half * 2;
        let (below, above) = self.rows[row * taps..(row + 2) * taps].split_at(taps);
        (below, above, (phase - row as f64) as f32)
    }
}

fn kernels() -> &'static [Kernel] {
    static KERNELS: OnceLock<Vec<Kernel>> = OnceLock::new();
    KERNELS.get_or_init(|| (0..WIDTHS).map(|step| Kernel::new(2f64.powf(step as f64 / 4.0))).collect())
}

/// Builds the tables, so that no voice has to: for whoever makes a device.
pub fn prepare() {
    kernels();
}

/// The weights to read with at `rate` stored frames for each one played.
pub fn kernel_for(rate: f64) -> &'static Kernel {
    let kernels = kernels();
    let rate = rate.abs();
    if rate <= 1.000_001 {
        return &kernels[0];
    }
    let step = ((rate * ALIAS_MARGIN).log2() * 4.0).ceil().max(0.0) as usize;
    &kernels[step.min(WIDTHS - 1)]
}

/// The sum of `samples` each times its weight, where the weight is a
/// blend of two rows. Added up in eight lanes, always the same eight, so
/// that the result is the same on every machine.
#[inline]
fn weighted(samples: &[f32], weights: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    for (samples, weights) in samples.chunks_exact(8).zip(weights.chunks_exact(8)) {
        for lane in 0..8 {
            lanes[lane] += samples[lane] * weights[lane];
        }
    }
    ((lanes[0] + lanes[4]) + (lanes[1] + lanes[5])) + ((lanes[2] + lanes[6]) + (lanes[3] + lanes[7]))
}

/// The recording at `position` frames from its start. `position` must not
/// be negative, and what the kernel reaches must be held
/// ([`Audio::reach`]): otherwise this is silence.
#[inline]
pub fn read(audio: &Audio, kernel: &Kernel, position: f64) -> (f32, f32) {
    let half = kernel.half;
    let whole = position.floor();
    if whole < 0.0 || whole as usize + half >= audio.reach() {
        return (0.0, 0.0);
    }
    let index = whole as usize;
    let (below, above, blend) = kernel.rows(position - whole);
    let taps = half * 2;
    let mut weights = [0.0f32; MAX_HALF * 2];
    for ((weight, below), above) in weights[..taps].iter_mut().zip(below).zip(above) {
        *weight = below + (above - below) * blend;
    }
    let first = GUARD + index - (half - 1);
    let (left, right) = audio.channels();
    let sum_left = weighted(&left[first..first + taps], &weights[..taps]);
    if audio.is_mono() {
        return (sum_left, sum_left);
    }
    (sum_left, weighted(&right[first..first + taps], &weights[..taps]))
}

/// [`read`], with every frame the kernel reaches for first passed through
/// `map`: how a read beside the end of a loop sees round the corner.
/// Frames `map` sends outside what is held are silence.
pub fn read_mapped(audio: &Audio, kernel: &Kernel, position: f64, map: impl Fn(i64) -> i64) -> (f32, f32) {
    let half = kernel.half as i64;
    let whole = position.floor();
    let first = whole as i64 - (half - 1);
    let (below, above, blend) = kernel.rows(position - whole);
    let (left, right) = audio.channels();
    let frames = audio.frames() as i64;
    let (mut sum_left, mut sum_right) = (0.0f32, 0.0f32);
    for (tap, (below, above)) in below.iter().zip(above).enumerate() {
        let index = map(first + tap as i64);
        if index >= 0 && index < frames {
            let weight = below + (above - below) * blend;
            let at = GUARD + index as usize;
            sum_left += left[at] * weight;
            sum_right += right[at] * weight;
        }
    }
    (sum_left, sum_right)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sampled::pool::Pool;
    use crate::testing::{MemoryLoader, spectrum_db};
    use std::f64::consts::TAU;
    use std::sync::Arc;

    const SR: f64 = 48_000.0;

    /// A recording in memory, whole.
    pub(crate) fn audio(left: Vec<f32>, right: Option<Vec<f32>>) -> Arc<Audio> {
        let loader = MemoryLoader::default();
        match right {
            Some(right) => loader.insert_stereo("a.wav", left, right, SR),
            None => loader.insert("a.wav", left, SR),
        }
        let pool = Pool::new(Arc::new(loader));
        pool.whole_now(&pool.recording("a.wav")).unwrap()
    }

    fn sine(hz: f64, frames: usize) -> Vec<f32> {
        (0..frames).map(|i| (TAU * hz * i as f64 / SR).sin() as f32).collect()
    }

    fn play(audio: &Audio, rate: f64, frames: usize) -> Vec<f32> {
        let kernel = kernel_for(rate);
        (0..frames).map(|i| read(audio, kernel, i as f64 * rate).0).collect()
    }

    fn rms_db(samples: &[f32]) -> f64 {
        let power = samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64;
        10.0 * power.max(1e-30).log10()
    }

    #[test]
    fn at_its_own_pitch_a_recording_reads_back_as_it_is() {
        let stored = sine(1_234.0, 4_800);
        let audio = audio(stored.clone(), None);
        let played = play(&audio, 1.0, 4_800);
        for (i, (played, stored)) in played.iter().zip(&stored).enumerate() {
            assert!((played - stored).abs() < 1e-6, "frame {i}: {played} vs {stored}");
        }
        // Outside what is held there is silence, never a panic.
        let kernel = kernel_for(1.0);
        assert_eq!(read(&audio, kernel, -1.0), (0.0, 0.0));
        assert_eq!(read(&audio, kernel, 4_800.0 + GUARD as f64), (0.0, 0.0));
        assert!(read(&audio, kernel, 4_799.5).0.abs() < 1.0);
    }

    #[test]
    fn between_the_frames_the_error_is_eighty_decibels_down() {
        // A 1 kHz tone read at awkward speeds below its own: against the
        // tone itself at that pitch.
        let audio = audio(sine(1_000.0, 48_000), None);
        for rate in [0.5, 0.749_153_5, 0.943_874_3, 0.999] {
            let played = play(&audio, rate, 20_000);
            let error: Vec<f32> = played
                .iter()
                .enumerate()
                .skip(100)
                .map(|(i, s)| s - (TAU * 1_000.0 * rate * i as f64 / SR).sin() as f32)
                .collect();
            assert!(rms_db(&error) < -80.0, "rate {rate}: error {:.1} dB", rms_db(&error));
        }
    }

    #[test]
    fn a_recording_played_up_to_two_octaves_above_itself_does_not_fold() {
        // 9 kHz played an octave up would be 18 kHz, which a 48 kHz output
        // holds; two octaves up it would be 36 kHz, which it does not: it
        // has to be gone, not back at 12 kHz. And a tone that stays in
        // band has to come through as loud as it went in.
        let high = audio(sine(9_000.0, 96_000), None);
        let low = audio(sine(1_000.0, 96_000), None);
        for rate in [1.059_463, 1.189_207, 1.5, 2.0, 2.378, 2.7, 3.3, 4.0] {
            let frames = (90_000.0 / rate) as usize;
            let played = play(&high, rate, frames);
            let level = rms_db(&played[64..frames - 64]) + 3.01;
            if 9_000.0 * rate > 24_000.0 {
                assert!(level < -60.0, "rate {rate}: {level:.1} dB of a tone that should have gone");
            } else if 9_000.0 * rate < 19_000.0 {
                assert!(level.abs() < 0.5, "rate {rate}: {level:.2} dB of a tone that stays in band");
            }
            let played = play(&low, rate, frames);
            let level = rms_db(&played[64..frames - 64]) + 3.01;
            assert!(level.abs() < 0.1, "rate {rate}: a 1 kHz tone came through at {level:.2} dB");
        }
    }

    #[test]
    fn an_octave_up_what_folds_is_seventy_decibels_under_what_is_played() {
        // A recording as bright as a recording can be: equal tones every
        // 500 Hz up to 23.5 kHz. An octave up, the ones above 12 kHz
        // would land above 24 kHz. What is left between the tones that
        // belong (now 1 kHz apart) is what folded.
        let frames = 1 << 17;
        let tones: Vec<f64> = (1..=47).map(|k| k as f64 * 500.0).collect();
        let recording: Vec<f32> = (0..frames)
            .map(|i| tones.iter().map(|hz| (TAU * hz * i as f64 / SR + hz).sin()).sum::<f64>() as f32 / 47.0)
            .collect();
        let audio = audio(recording, None);
        let played = play(&audio, 2.0, 1 << 15);
        let spectrum = spectrum_db(&played[1_024..1_024 + (1 << 14)]);
        let bin = SR / (1 << 14) as f64;
        let (mut wanted, mut folded) = (f64::MIN, f64::MIN);
        for (index, level) in spectrum.iter().enumerate() {
            let hz = index as f64 * bin;
            let nearest = (hz / 1_000.0).round() * 1_000.0;
            if (hz - nearest).abs() < 6.0 * bin && (1_000.0..=20_000.0).contains(&nearest) {
                wanted = wanted.max(f64::from(*level));
            } else if (hz - nearest).abs() > 200.0 && hz > 200.0 {
                folded = folded.max(f64::from(*level));
            }
        }
        assert!(folded < wanted - 70.0, "what folded is {:.1} dB under what belongs", wanted - folded);
    }

    #[test]
    fn two_sides_stay_two_and_one_side_is_both() {
        let left = sine(500.0, 8_000);
        let right: Vec<f32> = left.iter().map(|s| s * -0.5).collect();
        let stereo = audio(left.clone(), Some(right));
        for rate in [0.7, 1.0, 1.3, 3.0] {
            let (l, r) = read(&stereo, kernel_for(rate), 1_234.56);
            assert!((r + 0.5 * l).abs() < 1e-5 && l.abs() > 0.01, "rate {rate}: {l} {r}");
        }
        let mono = audio(left, None);
        let (l, r) = read(&mono, kernel_for(0.7), 1_234.56);
        assert_eq!(l, r);
    }

    #[test]
    fn a_mapped_read_is_a_plain_read_where_nothing_is_mapped() {
        let audio = audio(sine(700.0, 8_000), None);
        for rate in [0.6, 1.0, 1.3, 2.5] {
            let kernel = kernel_for(rate);
            for position in [100.0, 217.25, 3_999.9, 7_800.5] {
                let (plain, mapped) = (read(&audio, kernel, position), read_mapped(&audio, kernel, position, |i| i));
                assert!((plain.0 - mapped.0).abs() < 1e-5, "{rate} at {position}: {} vs {}", plain.0, mapped.0);
            }
        }
    }

    #[test]
    fn the_weights_widen_with_the_speed_and_never_past_what_is_guarded() {
        let half = |rate: f64| kernel_for(rate).half();
        assert_eq!((half(0.25), half(1.0)), (16, 16));
        assert_eq!(half(1.059_463), 20, "a semitone up");
        assert_eq!(half(2.0), 40, "an octave up");
        assert_eq!((half(4.0), half(100.0)), (MAX_HALF, MAX_HALF));
        assert!(GUARD >= MAX_HALF, "the guard has to cover the widest reach");
        for kernel in kernels() {
            assert_eq!(kernel.half % 4, 0);
        }
    }
}
