//! Offline band-limited resampling: sample-rate conversion, "turntable"
//! repitching and the octave-reduced copies the sample players read from.
//!
//! This is the textbook windowed-sinc interpolator: every output sample is a
//! weighted sum of the input samples around its position, the weights being a
//! sinc (the ideal low-pass) shaped by a Kaiser window. When the input is read
//! faster than one sample per output sample the sinc is widened, which
//! lowers its cutoff to just under the new Nyquist frequency so that nothing
//! aliases. Allocates freely; not for the audio thread.

use daw_core::audio::AudioBuffer;
use std::f64::consts::PI;
use std::sync::OnceLock;

/// Zero crossings of the sinc on each side of its center. Sets how steep the
/// low-pass is: with 32 it goes from flat to fully off within 9% either side
/// of its cutoff.
const ZERO_CROSSINGS: usize = 32;
/// Table entries per zero crossing; values in between are interpolated
/// linearly, which at this density is accurate to better than -100 dB.
const TABLE_RESOLUTION: usize = 256;
/// Kaiser window shape, chosen for about -90 dB of stop-band rejection.
const KAISER_BETA: f64 = 9.0;
/// When reading faster than 1:1 the cutoff is put this far below the new
/// Nyquist frequency, so that the roll-off ends there instead of straddling
/// it: nothing folds back, at the price of the top sixth of the band.
const DOWNSAMPLE_MARGIN: f64 = 1.08;

/// The right half of the windowed sinc, sampled [`TABLE_RESOLUTION`] times
/// per zero crossing, with one guard entry for the interpolation.
fn kernel() -> &'static [f32] {
    static KERNEL: OnceLock<Vec<f32>> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let length = ZERO_CROSSINGS * TABLE_RESOLUTION;
        let mut table: Vec<f32> = (0..=length)
            .map(|i| {
                let x = i as f64 / TABLE_RESOLUTION as f64;
                (sinc(x) * kaiser(x / ZERO_CROSSINGS as f64, KAISER_BETA)) as f32
            })
            .collect();
        table.push(0.0);
        table
    })
}

/// `sin(pi x) / (pi x)`.
pub(crate) fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 { 1.0 } else { (PI * x).sin() / (PI * x) }
}

/// Kaiser window at `x` in -1..=1 (zero outside).
pub(crate) fn kaiser(x: f64, beta: f64) -> f64 {
    if x.abs() >= 1.0 { 0.0 } else { bessel_i0(beta * (1.0 - x * x).sqrt()) / bessel_i0(beta) }
}

/// Modified Bessel function of the first kind, order zero, by its power
/// series (converges quickly for the arguments a Kaiser window uses).
fn bessel_i0(x: f64) -> f64 {
    let quarter = x * x / 4.0;
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..64 {
        term *= quarter / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Reads `input` at `step` input samples per output sample, producing
/// `out_len` samples: output `j` is the band-limited value of the input at
/// position `j * step`. Input beyond either end counts as silence.
pub fn resample(input: &[f32], out_len: usize, step: f64) -> Vec<f32> {
    if step == 1.0 {
        let mut output = input[..out_len.min(input.len())].to_vec();
        output.resize(out_len, 0.0);
        return output;
    }
    let table = kernel();
    // Reading faster than 1:1 widens the kernel, which lowers its cutoff.
    let widen = if step > 1.0 { step * DOWNSAMPLE_MARGIN } else { 1.0 };
    let reach = ZERO_CROSSINGS as f64 * widen;
    let table_step = TABLE_RESOLUTION as f64 / widen;
    let gain = (1.0 / widen) as f32;
    let last = input.len() as i64 - 1;

    (0..out_len)
        .map(|j| {
            let center = j as f64 * step;
            let first = ((center - reach).ceil() as i64).max(0);
            let end = ((center + reach).floor() as i64).min(last);
            let middle = (center.floor() as i64).clamp(first - 1, end);
            let mut sum = 0.0f32;
            // Left wing: samples at or before the center, walking outwards
            // would index the table forwards; walking inwards is the same
            // values in input order, which keeps the read sequential.
            if middle >= first {
                let mut position = (center - first as f64) * table_step;
                for &sample in &input[first as usize..=middle as usize] {
                    sum += sample * lookup(table, position);
                    position -= table_step;
                }
            }
            if end > middle {
                let mut position = ((middle + 1) as f64 - center) * table_step;
                for &sample in &input[(middle + 1) as usize..=end as usize] {
                    sum += sample * lookup(table, position);
                    position += table_step;
                }
            }
            sum * gain
        })
        .collect()
}

/// The kernel at `position` table steps from its center. Positions are kept
/// in `f64`: stepping an `f32` across a whole wing accumulates enough
/// rounding to cost 20 dB of accuracy.
#[inline]
fn lookup(table: &[f32], position: f64) -> f32 {
    let position = position.max(0.0);
    let index = position as usize;
    if index + 1 >= table.len() {
        return 0.0;
    }
    let fraction = (position - index as f64) as f32;
    table[index] + (table[index + 1] - table[index]) * fraction
}

/// Stretches or squeezes both channels to `out_len` frames by resampling, so
/// pitch and duration change together.
pub fn resample_to_length(input: &AudioBuffer, out_len: usize) -> AudioBuffer {
    let frames = input.frames();
    if frames == 0 || out_len == 0 {
        return AudioBuffer { sample_rate: input.sample_rate, left: vec![0.0; out_len], right: vec![0.0; out_len] };
    }
    let step = frames as f64 / out_len as f64;
    let left = resample(&input.left[..frames], out_len, step);
    let right = if input.left[..frames] == input.right[..frames] {
        left.clone()
    } else {
        resample(&input.right[..frames], out_len, step)
    };
    AudioBuffer { sample_rate: input.sample_rate, left, right }
}

/// Converts `input` to `sample_rate`, keeping its duration and pitch.
pub fn convert_sample_rate(input: &AudioBuffer, sample_rate: f64) -> AudioBuffer {
    if input.sample_rate <= 0.0 || sample_rate <= 0.0 || input.sample_rate == sample_rate {
        return AudioBuffer { sample_rate, ..input.clone() };
    }
    let out_len = (input.frames() as f64 * sample_rate / input.sample_rate).round() as usize;
    AudioBuffer { sample_rate, ..resample_to_length(input, out_len) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, sample_rate: f64, frames: usize) -> Vec<f32> {
        (0..frames).map(|i| (2.0 * PI * hz * i as f64 / sample_rate).sin() as f32).collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn interpolates_a_sine_accurately() {
        // 48 kHz -> 44.1 kHz: compare with the sine computed directly.
        let input = sine(1000.0, 48_000.0, 48_000);
        let out_len = 44_100;
        let output = resample(&input, out_len, 48_000.0 / 44_100.0);
        let want = sine(1000.0, 44_100.0, out_len);
        let error: Vec<f32> = output.iter().zip(&want).map(|(a, b)| a - b).collect();
        // Away from the ends, where the kernel hangs over the edge.
        let error_db = 20.0 * rms(&error[200..out_len - 200]).log10();
        assert!(error_db < -85.0, "interpolation error {error_db:.1} dB");
    }

    #[test]
    fn removes_what_would_alias_when_reading_fast() {
        // Read at 4x: 9 kHz would land on 36 kHz, above the 24 kHz Nyquist,
        // and without band-limiting would fold back to 12 kHz.
        let input = sine(9000.0, 48_000.0, 96_000);
        let output = resample(&input, 24_000, 4.0);
        let level_db = 20.0 * rms(&output[200..23_800]).log10();
        assert!(level_db < -85.0, "aliased level {level_db:.1} dB");

        // The hardest case: a tone just above the new Nyquist frequency
        // (6.1 kHz against 6 kHz), which would fold back to just below it.
        let input = sine(6_100.0, 48_000.0, 96_000);
        let output = resample(&input, 24_000, 4.0);
        let level_db = 20.0 * rms(&output[200..23_800]).log10();
        assert!(level_db < -80.0, "level just past Nyquist {level_db:.1} dB");

        // ...while 2 kHz (8 kHz after the speed-up) passes at full level.
        let input = sine(2000.0, 48_000.0, 96_000);
        let output = resample(&input, 24_000, 4.0);
        let level_db = 20.0 * (rms(&output[200..23_800]) * std::f64::consts::SQRT_2).log10();
        assert!(level_db.abs() < 0.01, "pass-band level {level_db:.3} dB");
    }

    #[test]
    fn sample_rate_conversion_keeps_duration() {
        let input =
            AudioBuffer { sample_rate: 44_100.0, left: sine(440.0, 44_100.0, 44_100), right: vec![0.0; 44_100] };
        let output = convert_sample_rate(&input, 48_000.0);
        assert_eq!(output.frames(), 48_000);
        assert_eq!(output.sample_rate, 48_000.0);
        assert!((output.seconds() - input.seconds()).abs() < 1e-9);
        let crossings = output.left[1000..47_000].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((crossings as f64 / (46_000.0 / 48_000.0) - 440.0).abs() < 1.5);
        assert!(output.right.iter().all(|s| *s == 0.0));
    }
}
