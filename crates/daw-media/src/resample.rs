//! Offline sample-rate conversion.
//!
//! Converting an asset to the engine's rate once, when it is loaded, keeps
//! interpolation off the audio thread and lets the conversion be far better
//! than anything affordable per sample in real time: a Kaiser-windowed sinc
//! long enough that aliasing and imaging stay below the noise floor of
//! 24-bit audio.

use daw_core::audio::AudioBuffer;

/// Attenuation of everything beyond the lower of the two Nyquist
/// frequencies, in decibels. Aliases and images land there.
const STOPBAND_DB: f64 = 120.0;

/// Fraction of the lower Nyquist frequency that passes untouched; the filter
/// rolls off between here and Nyquist. At 44.1 kHz that keeps everything
/// below 20 kHz flat.
const PASSBAND: f64 = 0.91;

/// Filter taps on each side of the output position when not downsampling,
/// from Kaiser's estimate for the attenuation and transition width above:
/// `(STOPBAND_DB - 7.95) / (2.285 * transition in radians)` is 174 taps in
/// all. Downsampling stretches the filter by the rate ratio.
const HALF_TAPS: usize = 88;

/// The most filter phases stored. Rate pairs whose reduced ratio needs no
/// more than this (every pair of standard rates) get one exact filter per
/// phase; anything else interpolates between this many.
const MAX_PHASES: u64 = 2048;

/// Ceiling on the filter table, in coefficients (16 MiB). Reached only by
/// absurd rate ratios, which are refused instead of spending seconds and
/// memory on a filter nobody needs.
const MAX_COEFFICIENTS: usize = 1 << 22;

/// The most frames a conversion may produce, matching what the decoder will
/// load: four gibibytes of stereo float.
const MAX_FRAMES: u128 = 1 << 29;

/// Outputs below this count are computed on the calling thread; spawning
/// threads costs more than converting a short sample.
const PARALLEL_FRAMES: usize = 1 << 17;

/// Converts `buffer` to `target_rate`, band-limited, in one call.
///
/// The result has `round(frames * target_rate / source_rate)` frames. If no
/// conversion is needed or possible (the rates are equal, one of them is
/// not a positive number, or the ratio is beyond what [`Resampler`]
/// accepts) the audio is returned unchanged, and its `sample_rate` still
/// says what it is.
///
/// To convert many buffers between the same two rates, build one
/// [`Resampler`] and reuse it; the filter design is the fixed cost.
pub fn resample(buffer: &AudioBuffer, target_rate: f64) -> AudioBuffer {
    match Resampler::new(buffer.sample_rate, target_rate) {
        Some(resampler) => resampler.process(buffer),
        None => buffer.clone(),
    }
}

/// A sample-rate converter for one pair of rates.
#[derive(Debug)]
pub struct Resampler {
    source_rate: f64,
    target_rate: f64,
    /// The rate ratio `source / target` in lowest terms: every output frame
    /// advances the input position by `step / phases_per_frame` frames.
    step: u64,
    phases_per_frame: u64,
    /// Coefficients, one row of `2 * half` per stored phase.
    bank: Vec<f32>,
    half: usize,
    /// Whether rows are a fixed grid to interpolate between (ratios that
    /// would need more than [`MAX_PHASES`] exact rows) or one per phase.
    interpolated: bool,
}

impl Resampler {
    /// Designs the filter for converting `source_rate` to `target_rate`.
    ///
    /// Returns `None` when there is nothing to design: the rates are equal,
    /// either is not a positive finite number, or they are so far apart
    /// that the filter would exceed a sane size (downsampling more than
    /// about tenfold between rates with no simple ratio, or thousandsfold
    /// otherwise).
    pub fn new(source_rate: f64, target_rate: f64) -> Option<Self> {
        // Rates are compared in millihertz, so that every position is exact
        // integer arithmetic and no rounding accumulates over a long file.
        let millihertz = |rate: f64| {
            let scaled = (rate * 1000.0).round();
            (scaled.is_finite() && (1.0..=1.0e12).contains(&scaled)).then_some(scaled as u64)
        };
        let (source, target) = (millihertz(source_rate)?, millihertz(target_rate)?);
        if source == target {
            return None;
        }
        let divisor = gcd(source, target);
        let (step, phases_per_frame) = (source / divisor, target / divisor);

        // Everything below is in units of input samples. When downsampling
        // the cutoff drops to the output's Nyquist and the filter widens to
        // keep the same transition width relative to it.
        let ratio = (target as f64 / source as f64).min(1.0);
        let half = ((HALF_TAPS as f64 / ratio).ceil() as usize).checked_next_multiple_of(4)?;
        let taps = 2 * half;
        let interpolated = phases_per_frame > MAX_PHASES;
        // The interpolated table carries one extra row for the far edge.
        let rows = if interpolated { MAX_PHASES as usize + 1 } else { phases_per_frame as usize };
        if rows.checked_mul(taps)? > MAX_COEFFICIENTS {
            return None;
        }

        let cutoff = 0.5 * ratio * (1.0 + PASSBAND) / 2.0;
        let beta = 0.1102 * (STOPBAND_DB - 8.7);
        let window_scale = 1.0 / bessel_i0(beta);
        let phase_count = if interpolated { MAX_PHASES as f64 } else { phases_per_frame as f64 };
        let mut bank = Vec::with_capacity(rows * taps);
        let mut row = vec![0.0f64; taps];
        for phase in 0..rows {
            let offset = phase as f64 / phase_count;
            for (tap, coefficient) in row.iter_mut().enumerate() {
                // Distance from the output position to this input sample.
                let distance = (tap as f64 - (half as f64 - 1.0)) - offset;
                let across = distance / half as f64;
                let window = if across.abs() < 1.0 {
                    bessel_i0(beta * (1.0 - across * across).sqrt()) * window_scale
                } else {
                    0.0
                };
                *coefficient = 2.0 * cutoff * sinc(2.0 * cutoff * distance) * window;
            }
            // Unity gain at DC for every phase, so a constant stays exactly
            // constant whatever the position between samples.
            let sum: f64 = row.iter().sum();
            bank.extend(row.iter().map(|coefficient| (coefficient / sum) as f32));
        }
        Some(Self { source_rate, target_rate, step, phases_per_frame, bank, half, interpolated })
    }

    /// The rate this converter expects its input at.
    pub fn source_rate(&self) -> f64 {
        self.source_rate
    }

    /// The rate of what [`Resampler::process`] returns.
    pub fn target_rate(&self) -> f64 {
        self.target_rate
    }

    /// Frames produced for `input_frames` of input: the exact length ratio,
    /// rounded to the nearest frame.
    pub fn output_frames(&self, input_frames: usize) -> usize {
        self.exact_frames(input_frames).min(MAX_FRAMES) as usize
    }

    fn exact_frames(&self, input_frames: usize) -> u128 {
        let (step, phases) = (u128::from(self.step), u128::from(self.phases_per_frame));
        (input_frames as u128 * phases + step / 2) / step
    }

    /// Converts `buffer`, which is taken to be at [`Resampler::source_rate`]
    /// whatever its `sample_rate` field says. A conversion that would
    /// produce more audio than fits in memory returns the buffer unchanged.
    pub fn process(&self, buffer: &AudioBuffer) -> AudioBuffer {
        let frames = buffer.frames();
        if self.exact_frames(frames) > MAX_FRAMES {
            return buffer.clone();
        }
        let (left, right) = (&buffer.left[..frames], &buffer.right[..frames]);
        let converted_left = self.convert(left);
        // Mono sources are stored as two identical channels; convert once.
        let converted_right = if left == right { converted_left.clone() } else { self.convert(right) };
        AudioBuffer { sample_rate: self.target_rate, left: converted_left, right: converted_right }
    }

    fn convert(&self, input: &[f32]) -> Vec<f32> {
        // Silence on both sides, so that the filter can hang over the ends.
        let mut padded = vec![0.0f32; input.len() + 2 * self.half];
        padded[self.half..self.half + input.len()].copy_from_slice(input);

        let mut output = vec![0.0f32; self.output_frames(input.len())];
        let threads = if output.len() < PARALLEL_FRAMES {
            1
        } else {
            std::thread::available_parallelism().map_or(1, |count| count.get())
        };
        if threads == 1 {
            self.convolve(&padded, &mut output, 0);
        } else {
            let share = output.len().div_ceil(threads);
            std::thread::scope(|scope| {
                for (index, part) in output.chunks_mut(share).enumerate() {
                    let padded = &padded;
                    scope.spawn(move || self.convolve(padded, part, index * share));
                }
            });
        }
        output
    }

    /// Computes output frames `first..first + output.len()`.
    fn convolve(&self, padded: &[f32], output: &mut [f32], first: usize) {
        let taps = 2 * self.half;
        let phases = self.phases_per_frame;
        let start = first as u128 * u128::from(self.step);
        // The input position of the current output frame, as a whole frame
        // and a remainder out of `phases`.
        let mut whole = (start / u128::from(phases)) as usize;
        let mut remainder = (start % u128::from(phases)) as u64;
        let (whole_step, remainder_step) = ((self.step / phases) as usize, self.step % phases);

        for sample in output {
            // `padded` is offset by `half`, so this is the window of input
            // frames `whole - half + 1 ..= whole + half`.
            let window = padded.get(whole + 1..whole + 1 + taps).unwrap_or(&[]);
            *sample = if self.interpolated {
                let scaled = u128::from(remainder) * u128::from(MAX_PHASES);
                let row = (scaled / u128::from(phases)) as usize;
                let between = ((scaled % u128::from(phases)) as f64 / phases as f64) as f32;
                let here = dot(self.row(row), window);
                here + (dot(self.row(row + 1), window) - here) * between
            } else {
                dot(self.row(remainder as usize), window)
            };
            whole += whole_step;
            remainder += remainder_step;
            if remainder >= phases {
                remainder -= phases;
                whole += 1;
            }
        }
    }

    fn row(&self, index: usize) -> &[f32] {
        let taps = 2 * self.half;
        &self.bank[index * taps..(index + 1) * taps]
    }
}

/// The inner product of two equally long slices, in eight independent
/// partial sums so the compiler can use vector instructions.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let (a_lanes, a_rest) = a.as_chunks::<8>();
    let (b_lanes, b_rest) = b.as_chunks::<8>();
    let mut sums = [0.0f32; 8];
    for (a, b) in a_lanes.iter().zip(b_lanes) {
        for ((sum, a), b) in sums.iter_mut().zip(a).zip(b) {
            *sum += a * b;
        }
    }
    sums.iter().sum::<f32>() + a_rest.iter().zip(b_rest).map(|(a, b)| a * b).sum::<f32>()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1.0e-9 {
        1.0
    } else {
        let scaled = std::f64::consts::PI * x;
        scaled.sin() / scaled
    }
}

/// The zeroth-order modified Bessel function of the first kind, by its
/// power series; the Kaiser window is defined in terms of it.
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..200 {
        term *= quarter_square / f64::from(k * k);
        sum += term;
        if term < sum * 1.0e-17 {
            break;
        }
    }
    sum
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn sine(rate: f64, frequency: f64, amplitude: f64, frames: usize) -> AudioBuffer {
        let channel: Vec<f32> =
            (0..frames).map(|n| (amplitude * (TAU * frequency * n as f64 / rate).sin()) as f32).collect();
        AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }
    }

    /// The amplitude of the `frequency` component of `samples`, and the
    /// level in decibels (relative to `reference`) of everything else in
    /// them. The component is found by least squares, so "everything else"
    /// is exactly the distortion, aliasing and imaging.
    fn analyse(samples: &[f32], rate: f64, frequency: f64, reference: f64) -> (f64, f64) {
        let phase = |n: usize| TAU * frequency * n as f64 / rate;
        let (mut ss, mut cc, mut sc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (n, &x) in samples.iter().enumerate() {
            let (s, c) = phase(n).sin_cos();
            ss += s * s;
            cc += c * c;
            sc += s * c;
            xs += f64::from(x) * s;
            xc += f64::from(x) * c;
        }
        let determinant = ss * cc - sc * sc;
        let a = (xs * cc - xc * sc) / determinant;
        let b = (xc * ss - xs * sc) / determinant;
        let residual: f64 = samples
            .iter()
            .enumerate()
            .map(|(n, &x)| {
                let (s, c) = phase(n).sin_cos();
                (f64::from(x) - a * s - b * c).powi(2)
            })
            .sum();
        let residual_rms = (residual / samples.len() as f64).sqrt();
        ((a * a + b * b).sqrt(), 20.0 * (residual_rms / (reference / 2f64.sqrt())).max(1.0e-12).log10())
    }

    fn level_db(samples: &[f32], reference: f64) -> f64 {
        let rms = (samples.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>() / samples.len() as f64).sqrt();
        20.0 * (rms / (reference / 2f64.sqrt())).max(1.0e-12).log10()
    }

    /// The part of a converted signal clear of the filter's edge transients.
    fn settled(samples: &[f32]) -> &[f32] {
        &samples[4_000..samples.len() - 4_000]
    }

    #[test]
    fn a_sine_keeps_its_frequency_and_level() {
        let cases = [
            (44_100.0, 48_000.0, 1_000.0),
            (44_100.0, 48_000.0, 18_000.0),
            (48_000.0, 44_100.0, 1_000.0),
            (48_000.0, 44_100.0, 19_000.0),
            (96_000.0, 44_100.0, 440.0),
            (22_050.0, 48_000.0, 5_000.0),
            (48_000.0, 96_000.0, 20_000.0),
            // No small ratio: the interpolated filter table.
            (44_100.0, 47_999.0, 1_000.0),
            (48_000.0, 44_056.0, 10_000.0),
        ];
        for (source, target, frequency) in cases {
            let input = sine(source, frequency, 0.5, source as usize);
            let output = resample(&input, target);
            assert_eq!(output.sample_rate, target);
            assert_eq!(output.left, output.right);
            let (amplitude, residual) = analyse(settled(&output.left), target, frequency, 0.5);
            println!(
                "{source} -> {target} Hz, {frequency} Hz tone: level {:+.5} dB, everything else {residual:.1} dB",
                20.0 * (amplitude / 0.5).log10()
            );
            assert!((amplitude / 0.5 - 1.0).abs() < 1.0e-3, "{source}->{target} at {frequency} Hz: {amplitude}");
            assert!(residual < -100.0, "{source}->{target} at {frequency} Hz: residual {residual:.1} dB");
        }
    }

    #[test]
    fn images_of_a_tone_near_nyquist_stay_below_minus_80_db() {
        // Upsampling mirrors a 21 kHz tone to 23.1 kHz unless the filter
        // removes the image. The tone itself sits in the roll-off, so only
        // what is left after removing it is judged.
        for (source, target, frequency) in
            [(44_100.0, 48_000.0, 21_000.0), (44_100.0, 96_000.0, 21_900.0), (44_100.0, 47_999.0, 21_000.0)]
        {
            let output = resample(&sine(source, frequency, 0.5, source as usize), target);
            let (amplitude, residual) = analyse(settled(&output.left), target, frequency, 0.5);
            println!(
                "{source} -> {target} Hz, {frequency} Hz tone: level {:+.2} dB, images {residual:.1} dB",
                20.0 * (amplitude / 0.5).log10()
            );
            assert!(residual < -80.0, "{source}->{target}: images at {residual:.1} dB");
        }
    }

    #[test]
    fn aliases_of_a_tone_above_the_new_nyquist_stay_below_minus_80_db() {
        // Nothing above the output's Nyquist can be represented; whatever
        // survives the filter folds back as an alias.
        for (source, target, frequency) in [
            (48_000.0, 44_100.0, 22_100.0),
            (48_000.0, 44_100.0, 23_500.0),
            (96_000.0, 44_100.0, 30_000.0),
            (96_000.0, 48_000.0, 24_500.0),
            (48_000.0, 44_056.0, 23_000.0),
        ] {
            let output = resample(&sine(source, frequency, 0.5, source as usize), target);
            let level = level_db(settled(&output.left), 0.5);
            println!("{source} -> {target} Hz, {frequency} Hz tone: aliases {level:.1} dB");
            assert!(level < -80.0, "{source}->{target} at {frequency} Hz: aliases at {level:.1} dB");
        }
    }

    #[test]
    fn length_is_the_exact_ratio() {
        for (source, target, frames, expected) in [
            (44_100.0, 48_000.0, 44_100, 48_000),
            (48_000.0, 44_100.0, 48_000, 44_100),
            (44_100.0, 48_000.0, 1, 1),
            (44_100.0, 48_000.0, 0, 0),
            (48_000.0, 44_100.0, 7, 6),
            (44_100.0, 96_000.0, 1_000, 2_177),
            (96_000.0, 8_000.0, 100_001, 8_333),
            (44_100.0, 47_999.0, 44_100, 47_999),
        ] {
            let output = resample(&sine(source, 100.0, 0.5, frames), target);
            assert_eq!(output.frames(), expected, "{frames} frames {source}->{target}");
            assert_eq!(output.left.len(), output.right.len());
        }
    }

    #[test]
    fn dc_and_stereo_are_preserved() {
        let frames = 20_000;
        let input = AudioBuffer { sample_rate: 44_100.0, left: vec![0.25; frames], right: vec![-0.5; frames] };
        let output = resample(&input, 48_000.0);
        for (&l, &r) in settled(&output.left).iter().zip(settled(&output.right)) {
            assert!((l - 0.25).abs() < 1.0e-6 && (r + 0.5).abs() < 1.0e-6, "{l} {r}");
        }
    }

    #[test]
    fn timing_is_preserved() {
        // An impulse comes out where it went in: the filter adds no delay.
        let mut input = AudioBuffer { sample_rate: 44_100.0, left: vec![0.0; 44_100], right: vec![0.0; 44_100] };
        input.left[22_050] = 1.0;
        input.right[22_050] = 1.0;
        let output = resample(&input, 48_000.0);
        let peak = output.left.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map(|(n, _)| n);
        assert_eq!(peak, Some(24_000));
    }

    #[test]
    fn nothing_to_do_or_impossible_returns_the_audio_unchanged() {
        let input = sine(48_000.0, 440.0, 0.5, 1_000);
        for target in [48_000.0, 0.0, -44_100.0, f64::NAN, f64::INFINITY, 1.0e-9, 2.0] {
            assert_eq!(resample(&input, target), input, "target {target}");
        }
        let unknown = AudioBuffer { sample_rate: 0.0, ..input.clone() };
        assert_eq!(resample(&unknown, 48_000.0), unknown);
        // A header that claims 8 Hz must not turn a short file into hours.
        let slow = AudioBuffer { sample_rate: 8.0, left: vec![0.0; 1_000_000], right: vec![0.0; 1_000_000] };
        assert_eq!(resample(&slow, 48_000.0).frames(), 1_000_000);
        assert!(resample(&AudioBuffer::default(), 48_000.0).left.is_empty());
    }

    #[test]
    fn a_resampler_is_reusable_and_reports_its_rates() {
        let resampler = Resampler::new(44_100.0, 48_000.0).unwrap();
        assert_eq!((resampler.source_rate(), resampler.target_rate()), (44_100.0, 48_000.0));
        assert_eq!(resampler.output_frames(441), 480);
        let once = resampler.process(&sine(44_100.0, 1_000.0, 0.5, 4_410));
        let again = resampler.process(&sine(44_100.0, 1_000.0, 0.5, 4_410));
        assert_eq!(once, again);
        assert_eq!(once, resample(&sine(44_100.0, 1_000.0, 0.5, 4_410), 48_000.0));
        assert!(Resampler::new(48_000.0, 48_000.0).is_none());
    }

    #[test]
    fn long_files_convert_in_parallel_to_the_same_result() {
        // Long enough to be split across threads; every split must agree
        // with the single-threaded path to the last bit.
        let input = sine(44_100.0, 997.0, 0.5, 44_100 * 6);
        let resampler = Resampler::new(44_100.0, 48_000.0).unwrap();
        let started = std::time::Instant::now();
        let output = resampler.process(&input);
        println!("6 s of 44.1 kHz stereo to 48 kHz: {:?}", started.elapsed());

        let mut padded = vec![0.0f32; input.left.len() + 2 * resampler.half];
        padded[resampler.half..resampler.half + input.left.len()].copy_from_slice(&input.left);
        let mut single = vec![0.0f32; output.left.len()];
        resampler.convolve(&padded, &mut single, 0);
        assert!(output.left == single);
    }
}
