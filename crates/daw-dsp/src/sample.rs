//! Samples prepared for playback at any pitch, and the interpolator that
//! reads them.
//!
//! Playing a sample at a different pitch means reading it at a different
//! speed, which has two ways to go wrong. Reading between the stored frames
//! with a crude interpolator dulls the highs and adds a harsh mirror image of
//! them; here each output frame is a weighted sum of 32 or more stored frames
//! (a Kaiser-windowed sinc). And reading *faster* than the sample was
//! recorded pushes its highs past what the output can represent, where they
//! fold back down as inharmonic noise (aliasing). So a [`Sample`] also holds
//! copies of the audio at half, a quarter, an eighth... of the rate, each
//! with the top octave filtered out beforehand, and the reader picks the copy
//! and a slightly widened interpolator that together remove exactly the part
//! of the spectrum that would not survive.
//!
//! Preparing a sample allocates and takes a moment, so it happens when a
//! device is created. Reading is realtime-safe.

use crate::resample::{kaiser, resample, sinc};
use daw_core::audio::AudioBuffer;
use std::sync::OnceLock;

/// Zero crossings of the interpolation kernel on each side of its center,
/// before widening: 32 points for plain interpolation. That many keep the
/// response flat to 20 kHz in a 48 kHz file; with half as many, the top
/// three kilohertz are audibly mirrored when a bright sample is played a
/// little below its root.
const ZERO_CROSSINGS: usize = 16;
/// Fractional positions the kernels are tabulated at; in-between positions
/// are interpolated linearly, which is accurate to about -100 dB.
const PHASES: usize = 256;
/// Trades the kernel's stop-band depth (about -70 dB) against the width of
/// its transition band.
const KAISER_BETA: f64 = 7.0;
/// The kernel widths available, as factors by which the cutoff is lowered:
/// quarter-octave steps up to where the next half-rate copy takes over.
const WIDTHS: [f64; 4] = [1.0, 1.189_207_115, std::f64::consts::SQRT_2, 1.681_792_831];
/// How much wider than the playback speed the kernel is made, which moves
/// the end of its roll-off, rather than the middle, on to the fold-over
/// frequency.
const ALIAS_MARGIN: f64 = 1.1;
/// Zero frames stored before and after each copy, so the interpolator can
/// read past either end without checking every tap. Twice the reach of the
/// widest kernel, rounded up.
const GUARD: usize = 64;
/// Half-rate copies stop after this many, or when they get this short.
const MAX_LEVELS: usize = 10;
const MIN_LEVEL_FRAMES: usize = 64;

/// The interpolation weights for one kernel width.
struct Kernel {
    /// Taps on each side of the read position.
    half: usize,
    /// `PHASES + 1` rows of `2 * half` weights; row `p` is for a read
    /// position `p / PHASES` of the way from one frame to the next.
    rows: Vec<f32>,
}

impl Kernel {
    fn new(width: f64) -> Self {
        let half = (ZERO_CROSSINGS as f64 * width).ceil() as usize;
        let taps = half * 2;
        let mut rows = Vec::with_capacity((PHASES + 1) * taps);
        for phase in 0..=PHASES {
            let fraction = phase as f64 / PHASES as f64;
            let row: Vec<f64> = (0..taps)
                .map(|tap| {
                    // Tap `half - 1` is the frame at or just before the
                    // read position.
                    let distance = (tap as f64 - (half - 1) as f64 - fraction) / width;
                    sinc(distance) * kaiser(distance / ZERO_CROSSINGS as f64, KAISER_BETA)
                })
                .collect();
            // Every row sums to one, so a constant input reads back as the
            // same constant at every position: no ripple with pitch.
            let sum: f64 = row.iter().sum();
            rows.extend(row.iter().map(|weight| (weight / sum) as f32));
        }
        Self { half, rows }
    }

    /// The weights for the two tabulated positions around `fraction`
    /// (0..1), and how far between them it lies.
    #[inline]
    fn rows(&self, fraction: f64) -> (&[f32], &[f32], f32) {
        let phase = fraction * PHASES as f64;
        let row = (phase as usize).min(PHASES - 1);
        let taps = self.half * 2;
        let (below, above) = self.rows[row * taps..(row + 2) * taps].split_at(taps);
        (below, above, (phase - row as f64) as f32)
    }
}

fn kernels() -> &'static [Kernel; 4] {
    static KERNELS: OnceLock<[Kernel; 4]> = OnceLock::new();
    KERNELS.get_or_init(|| WIDTHS.map(Kernel::new))
}

/// One stored copy of the audio, with [`GUARD`] zero frames at each end.
struct Level {
    frames: usize,
    left: Vec<f32>,
    /// `None` when the right channel is identical to the left.
    right: Option<Vec<f32>>,
}

impl Level {
    fn new(left: &[f32], right: Option<&[f32]>) -> Self {
        let pad = |channel: &[f32]| {
            let mut padded = vec![0.0; channel.len() + 2 * GUARD];
            padded[GUARD..GUARD + channel.len()].copy_from_slice(channel);
            padded
        };
        Self { frames: left.len(), left: pad(left), right: right.map(pad) }
    }

    fn right(&self) -> &[f32] {
        self.right.as_deref().unwrap_or(&self.left)
    }
}

/// How to read a sample at a particular speed: which copy, and with which
/// kernel. Worked out once per change of pitch rather than per frame.
#[derive(Copy, Clone)]
pub struct ReadPlan {
    level: usize,
    /// Positions in original frames times this are positions in the copy.
    scale: f64,
    kernel: &'static Kernel,
}

/// An audio file prepared for pitched playback. Immutable and shared: voices
/// read it, nothing writes it.
pub struct Sample {
    sample_rate: f64,
    levels: Vec<Level>,
}

impl Sample {
    /// Prepares `buffer`. Allocates and filters; not for the audio thread.
    pub fn new(buffer: &AudioBuffer) -> Self {
        // Build the kernel tables now, so that no voice ever has to.
        kernels();
        let frames = buffer.frames();
        let stereo = buffer.left[..frames] != buffer.right[..frames];
        let mut left = buffer.left[..frames].to_vec();
        let mut right = stereo.then(|| buffer.right[..frames].to_vec());

        let mut levels = vec![Level::new(&left, right.as_deref())];
        while levels.len() < MAX_LEVELS && left.len() >= 2 * MIN_LEVEL_FRAMES {
            // Each copy is made from the one before it, so the whole set
            // costs about as much as the first.
            let half = left.len().div_ceil(2);
            left = resample(&left, half, 2.0);
            right = right.map(|right| resample(&right, half, 2.0));
            levels.push(Level::new(&left, right.as_deref()));
        }
        Self { sample_rate: buffer.sample_rate, levels }
    }

    /// Length in frames at the file's own sample rate.
    pub fn frames(&self) -> usize {
        self.levels[0].frames
    }

    /// The file's sample rate.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// One stored frame, untouched; silence outside the sample.
    #[inline]
    pub fn frame(&self, index: i64) -> (f32, f32) {
        let level = &self.levels[0];
        if index < 0 || index >= level.frames as i64 {
            return (0.0, 0.0);
        }
        let at = GUARD + index as usize;
        (level.left[at], level.right()[at])
    }

    /// Chooses how to read at `rate` original frames per output frame.
    pub fn plan(&self, rate: f64) -> ReadPlan {
        let kernels = kernels();
        let last = self.levels.len() - 1;
        let rate = rate.abs().max(1e-9);
        // Each copy halves the rate; what is left over (1 to 2) is made up
        // by widening the kernel. A kernel exactly as wide as that would
        // put the middle of its roll-off on the highest frequency that
        // survives, and let through half of what lies just above it, so it
        // is made a little wider still. Exactly on a copy's own rate there
        // is nothing to remove and the plain kernel returns the stored
        // frames untouched.
        let mut level = (rate.log2().floor().max(0.0) as usize).min(last);
        let speed = rate / (1u64 << level) as f64;
        let mut width = if speed > 1.000_001 { speed * ALIAS_MARGIN } else { 1.0 };
        // Past the widest kernel, the next copy read slightly slowly is both
        // cheaper and as good.
        if width > WIDTHS[WIDTHS.len() - 1] && level < last {
            level += 1;
            width = 1.0;
        }
        // Beyond the last copy (eight or more octaves up) the widest kernel
        // is all there is.
        let kernel = WIDTHS.iter().position(|available| width <= *available).unwrap_or(WIDTHS.len() - 1);
        ReadPlan { level, scale: 1.0 / (1u64 << level) as f64, kernel: &kernels[kernel] }
    }

    /// The audio at `position` (in original frames, fractional), silence
    /// outside the sample.
    #[inline]
    pub fn read(&self, plan: &ReadPlan, position: f64) -> (f32, f32) {
        let level = &self.levels[plan.level];
        let half = plan.kernel.half as i64;
        let scaled = position * plan.scale;
        let whole = scaled.floor();
        let index = whole as i64;
        if index < -half || index >= level.frames as i64 + half {
            return (0.0, 0.0);
        }
        let (below, above, blend) = plan.kernel.rows(scaled - whole);
        let first = (GUARD as i64 + index - (half - 1)) as usize;
        let taps = below.len();
        let left = &level.left[first..first + taps];
        let weights = below.iter().zip(above).map(|(below, above)| below + (above - below) * blend);
        match &level.right {
            Some(right) => {
                let right = &right[first..first + taps];
                let (mut sum_left, mut sum_right) = (0.0, 0.0);
                for ((left, right), weight) in left.iter().zip(right).zip(weights) {
                    sum_left += left * weight;
                    sum_right += right * weight;
                }
                (sum_left, sum_right)
            }
            None => {
                let sum: f32 = left.iter().zip(weights).map(|(sample, weight)| sample * weight).sum();
                (sum, sum)
            }
        }
    }

    /// Like [`Sample::read`], but each frame the kernel reaches for is first
    /// passed through `map`, which is how a read near a loop point sees
    /// around the corner. `map` takes and returns frame indices in the copy
    /// being read (original frames times [`ReadPlan::scale`]).
    pub fn read_mapped(&self, plan: &ReadPlan, position: f64, map: impl Fn(i64) -> i64) -> (f32, f32) {
        let level = &self.levels[plan.level];
        let half = plan.kernel.half as i64;
        let scaled = position * plan.scale;
        let whole = scaled.floor();
        let first = whole as i64 - (half - 1);
        let (below, above, blend) = plan.kernel.rows(scaled - whole);
        let right = level.right();
        let (mut sum_left, mut sum_right) = (0.0, 0.0);
        for (tap, (below, above)) in below.iter().zip(above).enumerate() {
            let index = map(first + tap as i64);
            if index >= 0 && index < level.frames as i64 {
                let weight = below + (above - below) * blend;
                let at = GUARD + index as usize;
                sum_left += level.left[at] * weight;
                sum_right += right[at] * weight;
            }
        }
        (sum_left, sum_right)
    }
}

impl ReadPlan {
    /// Factor from original frames to frames of the copy this plan reads.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// How many frames either side of the read position the kernel uses, in
    /// frames of the copy this plan reads.
    pub fn reach(&self) -> i64 {
        self.kernel.half as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const SR: f64 = 48_000.0;

    fn sine_buffer(hz: f64, frames: usize) -> AudioBuffer {
        let left: Vec<f32> = (0..frames).map(|i| (2.0 * PI * hz * i as f64 / SR).sin() as f32).collect();
        AudioBuffer { sample_rate: SR, right: left.clone(), left }
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    /// Plays `sample` at `rate` and returns the left channel.
    fn play(sample: &Sample, rate: f64, frames: usize) -> Vec<f32> {
        let plan = sample.plan(rate);
        (0..frames).map(|i| sample.read(&plan, i as f64 * rate).0).collect()
    }

    #[test]
    fn reading_at_the_original_rate_returns_the_stored_frames() {
        let buffer = sine_buffer(1234.0, 4_800);
        let sample = Sample::new(&buffer);
        assert_eq!(sample.frames(), 4_800);
        let played = play(&sample, 1.0, 4_800);
        for (i, (played, stored)) in played.iter().zip(&buffer.left).enumerate() {
            assert!((played - stored).abs() < 1e-6, "frame {i}: {played} vs {stored}");
            assert_eq!(sample.frame(i as i64).0, *stored);
        }
        assert_eq!(sample.read(&sample.plan(1.0), -100.0), (0.0, 0.0));
        assert_eq!(sample.read(&sample.plan(1.0), 4_900.0), (0.0, 0.0));
        assert_eq!(sample.frame(-1), (0.0, 0.0));
    }

    #[test]
    fn interpolation_is_accurate_between_frames() {
        // An octave down, and an awkward ratio: compare with the sine itself.
        let sample = Sample::new(&sine_buffer(1000.0, 48_000));
        for rate in [0.5, 0.749_153_5, 0.943_874_3] {
            let played = play(&sample, rate, 20_000);
            let error: Vec<f32> = played
                .iter()
                .enumerate()
                .skip(100)
                .map(|(i, s)| s - (2.0 * PI * 1000.0 * rate * i as f64 / SR).sin() as f32)
                .collect();
            let error_db = 20.0 * rms(&error).log10();
            assert!(error_db < -80.0, "rate {rate}: error {error_db:.1} dB");
        }
    }

    #[test]
    fn stereo_channels_stay_separate_and_mono_is_shared() {
        let mut buffer = sine_buffer(500.0, 8_000);
        let mono = Sample::new(&buffer);
        assert!(mono.levels.iter().all(|level| level.right.is_none()));
        let plan = mono.plan(0.7);
        let (l, r) = mono.read(&plan, 1234.56);
        assert_eq!(l, r);

        buffer.right.iter_mut().for_each(|s| *s *= -0.5);
        let stereo = Sample::new(&buffer);
        for rate in [0.7, 1.0, 1.3, 3.0] {
            let plan = stereo.plan(rate);
            let (l, r) = stereo.read(&plan, 1234.56);
            assert!((r + 0.5 * l).abs() < 1e-5 && l.abs() > 0.01, "rate {rate}: {l} {r}");
        }
    }

    #[test]
    fn playing_fast_does_not_alias() {
        // 9 kHz played two octaves up would be 36 kHz: it must be gone, not
        // folded back to 12 kHz. Likewise for rates between the octaves.
        let high = Sample::new(&sine_buffer(9_000.0, 96_000));
        let low = Sample::new(&sine_buffer(1_000.0, 96_000));
        for rate in [1.1, 1.5, 1.9, 2.0, 2.7, 3.3, 4.0, 5.2, 8.0] {
            // The high tone lands above Nyquist for rates over 2.67.
            let played = play(&high, rate, (90_000.0 / rate) as usize);
            let level_db = 20.0 * (rms(&played[64..]) * std::f64::consts::SQRT_2).log10();
            if 9_000.0 * rate > 24_000.0 {
                assert!(level_db < -60.0, "rate {rate}: aliased level {level_db:.1} dB");
            }
            // The low tone is in band at every one of these rates.
            let played = play(&low, rate, (90_000.0 / rate) as usize);
            let level_db = 20.0 * (rms(&played[64..played.len() - 64]) * std::f64::consts::SQRT_2).log10();
            assert!(level_db.abs() < 0.1, "rate {rate}: pass-band level {level_db:.2} dB");
        }
    }

    #[test]
    fn plans_pick_the_copy_and_kernel_for_the_rate() {
        let sample = Sample::new(&sine_buffer(100.0, 48_000));
        let describe = |rate: f64| {
            let plan = sample.plan(rate);
            (plan.level, plan.kernel.half)
        };
        let half = |width: f64| (ZERO_CROSSINGS as f64 * width).ceil() as usize;
        assert_eq!(describe(0.25), (0, half(1.0)));
        assert_eq!(describe(1.0), (0, half(1.0)));
        assert_eq!(describe(1.05), (0, half(WIDTHS[1])));
        assert_eq!(describe(1.2), (0, half(WIDTHS[2])));
        assert_eq!(describe(1.5), (0, half(WIDTHS[3])));
        // Past the widest kernel, the next copy is read slightly slowly.
        assert_eq!(describe(1.6), (1, half(1.0)));
        assert_eq!(describe(2.0), (1, half(1.0)));
        assert_eq!(describe(2.1), (1, half(WIDTHS[1])));
        assert_eq!(describe(4.0), (2, half(1.0)));
        assert!(GUARD >= 2 * half(WIDTHS[3]), "the guard must cover the widest kernel's reach");
        // Far beyond the last copy it still returns something sensible.
        let plan = sample.plan(1e6);
        assert_eq!(plan.level, sample.levels.len() - 1);
    }

    #[test]
    fn mapped_reads_match_plain_reads_when_nothing_is_remapped() {
        let sample = Sample::new(&sine_buffer(700.0, 8_000));
        for rate in [0.6, 1.0, 1.3, 2.5] {
            let plan = sample.plan(rate);
            for position in [0.0, 17.25, 3_999.9, 7_990.5] {
                let (plain, mapped) =
                    (sample.read(&plan, position), sample.read_mapped(&plan, position, |index| index));
                assert!((plain.0 - mapped.0).abs() < 1e-6 && (plain.1 - mapped.1).abs() < 1e-6);
            }
        }
    }
}
