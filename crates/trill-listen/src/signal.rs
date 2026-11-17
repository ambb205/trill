//! Audio as the analyses take it: one channel of samples at a known rate,
//! and the small measurements all of them share.

use trill_core::audio::AudioBuffer;

/// The rate everything is analysed at. It is what most recordings and
/// sample packs are in, and every length below that is given in seconds was
/// tuned at it.
pub const RATE: f64 = 44_100.0;

/// Stereo audio at [`RATE`], which is what a cut is taken from and what a
/// stem is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sound {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Sound {
    /// `buffer` brought to [`RATE`]. A buffer that is there already is
    /// copied as it is.
    pub fn of(buffer: &AudioBuffer) -> Self {
        let frames = buffer.frames();
        if (buffer.sample_rate - RATE).abs() < 0.5 || buffer.sample_rate <= 0.0 {
            return Self { left: buffer.left[..frames].to_vec(), right: buffer.right[..frames].to_vec() };
        }
        let converted = trill_media::resample(buffer, RATE);
        let frames = converted.frames();
        Self { left: converted.left[..frames].to_vec(), right: converted.right[..frames].to_vec() }
    }

    /// A sound of one channel, heard the same in both.
    pub fn from_mono(samples: Vec<f32>) -> Self {
        Self { right: samples.clone(), left: samples }
    }

    pub fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / RATE
    }

    /// Both channels as one: their mean.
    pub fn mono(&self) -> Vec<f32> {
        self.left.iter().zip(&self.right).map(|(left, right)| 0.5 * (left + right)).collect()
    }

    /// The loudest sample of either channel.
    pub fn peak(&self) -> f32 {
        self.left.iter().chain(&self.right).fold(0.0f32, |peak, sample| peak.max(sample.abs()))
    }

    /// Whether there is nothing here to analyse.
    pub fn is_silent(&self) -> bool {
        self.peak() < 1e-4
    }

    /// This sound and `other` added up, as long as the shorter of them.
    pub fn plus(&self, other: &Sound) -> Sound {
        let frames = self.frames().min(other.frames());
        Sound {
            left: (0..frames).map(|i| self.left[i] + other.left[i]).collect(),
            right: (0..frames).map(|i| self.right[i] + other.right[i]).collect(),
        }
    }
}

/// `signal`, which is at `from` hertz, at `to` hertz.
pub fn resample(signal: &[f32], from: f64, to: f64) -> Vec<f32> {
    if (from - to).abs() < 1e-9 || signal.is_empty() {
        return signal.to_vec();
    }
    // The converter takes two channels and converts one when they are the same.
    let buffer = AudioBuffer { sample_rate: from, left: signal.to_vec(), right: signal.to_vec() };
    trill_media::resample(&buffer, to).left
}

/// A level as decibels, with a floor at -180 dB so that silence is a number.
pub fn db(value: f64) -> f64 {
    20.0 * value.max(1e-9).log10()
}

/// The root of the mean of the squares.
pub fn rms(signal: &[f32]) -> f64 {
    if signal.is_empty() {
        return 0.0;
    }
    (signal.iter().map(|&sample| f64::from(sample) * f64::from(sample)).sum::<f64>() / signal.len() as f64).sqrt()
}

/// The mean of `width` samples around each one: a moving average the length
/// of the signal, with silence taken to lie beyond its ends.
pub fn smooth(values: &[f32], width: usize) -> Vec<f32> {
    let width = width.max(1);
    let mut sums = Vec::with_capacity(values.len() + 1);
    let mut total = 0.0f64;
    sums.push(0.0);
    for &value in values {
        total += f64::from(value);
        sums.push(total);
    }
    // The window reaches half its width back and one less forward, which
    // is how a convolution that keeps the signal's length lies.
    let (back, forward) = (width / 2, (width - 1) / 2);
    (0..values.len())
        .map(|index| {
            let low = index.saturating_sub(back);
            let high = (index + forward + 1).min(values.len());
            ((sums[high] - sums[low]) / width as f64) as f32
        })
        .collect()
}

/// The absolute value of each sample.
pub fn magnitude(signal: &[f32]) -> Vec<f32> {
    signal.iter().map(|sample| sample.abs()).collect()
}

/// The value below which `share` (0 to 100) of `values` lie, read between
/// the two nearest when it falls between them.
pub fn percentile(values: &[f64], share: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let position = (share / 100.0).clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let low = position.floor() as usize;
    let high = (low + 1).min(sorted.len() - 1);
    sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
}

/// [`percentile`] of single-precision values.
pub fn percentile_of(values: &[f32], share: f64) -> f64 {
    let wide: Vec<f64> = values.iter().map(|&value| f64::from(value)).collect();
    percentile(&wide, share)
}

/// The middle value.
pub fn median(values: &[f64]) -> f64 {
    percentile(values, 50.0)
}

/// The level of `signal` in frames of `frame` samples every `hop`: frame
/// `i` covers samples `i * hop .. i * hop + frame`.
pub fn frame_rms(signal: &[f32], frame: usize, hop: usize) -> Vec<f32> {
    let mut squares = Vec::with_capacity(signal.len().max(frame) + 1);
    let mut total = 0.0f64;
    squares.push(0.0);
    for index in 0..signal.len().max(frame) {
        let sample = f64::from(signal.get(index).copied().unwrap_or(0.0));
        total += sample * sample;
        squares.push(total);
    }
    let length = squares.len() - 1;
    (0..=(length - frame) / hop.max(1))
        .map(|index| ((squares[index * hop + frame] - squares[index * hop]) / frame as f64).sqrt() as f32)
        .collect()
}

#[cfg(test)]
pub(crate) mod testing {
    //! Sounds the tests build for themselves.

    use super::RATE;
    use std::f64::consts::TAU;

    /// Noise that is the same on every run.
    pub struct Noise(pub u32);

    impl Noise {
        /// Uniform in -1..1.
        pub fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            (self.0 >> 8) as f32 / 8_388_608.0 - 1.0
        }
    }

    pub fn silence(seconds: f64) -> Vec<f32> {
        vec![0.0; (seconds * RATE) as usize]
    }

    /// Adds a drum into `out` at `at` seconds: a kick is a low tone that
    /// falls in pitch and dies in a fifth of a second, a snare a tone with
    /// noise over it, a hat a tick of bright noise.
    pub fn drum(out: &mut [f32], at: f64, kind: &str, level: f32, noise: &mut Noise) {
        let start = (at * RATE) as usize;
        let (length, mut previous) = (
            match kind {
                "kick" => 0.22,
                "snare" => 0.16,
                _ => 0.045,
            },
            0.0f32,
        );
        let mut phase = 0.0f64;
        for index in 0..(length * RATE) as usize {
            let Some(slot) = out.get_mut(start + index) else { break };
            let time = index as f64 / RATE;
            let value = match kind {
                "kick" => {
                    phase += TAU * (48.0 + 70.0 * (-time * 35.0).exp()) / RATE;
                    (phase.sin() * (-time * 16.0).exp()) as f32
                }
                "snare" => {
                    phase += TAU * 230.0 / RATE;
                    let hiss = noise.next();
                    (0.45 * phase.sin() * (-time * 28.0).exp()) as f32 + 0.75 * hiss * (-time * 26.0).exp() as f32
                }
                _ => {
                    // Noise with its lows taken out: the difference of neighbours.
                    let hiss = noise.next();
                    let bright = hiss - previous;
                    previous = hiss;
                    0.6 * bright * (-time * 110.0).exp() as f32
                }
            };
            *slot += level * value;
        }
    }

    /// Adds a note of `hertz` into `out`: a few harmonics with a soft start
    /// and end, as a plucked or blown note has.
    pub fn tone(out: &mut [f32], at: f64, seconds: f64, hertz: f64, level: f32) {
        let start = (at * RATE) as usize;
        let length = (seconds * RATE) as usize;
        for index in 0..length {
            let Some(slot) = out.get_mut(start + index) else { break };
            let time = index as f64 / RATE;
            let fade = (time / 0.004).min(1.0) * ((seconds - time) / 0.012).clamp(0.0, 1.0);
            let wave: f64 = (1..=5).map(|k| (TAU * hertz * k as f64 * time).sin() / (k * k) as f64).sum();
            *slot += level * (wave * fade) as f32;
        }
    }

    /// The frequency of a MIDI note, A4 = 440 Hz moved by `cents`.
    pub fn hertz(note: f64, cents: f64) -> f64 {
        440.0 * 2f64.powf((note - 69.0 + cents / 100.0) / 12.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_at_another_rate_is_brought_to_the_rate_of_the_analyses() {
        let buffer = AudioBuffer { sample_rate: 48_000.0, left: vec![0.25; 48_000], right: vec![0.25; 48_000] };
        let sound = Sound::of(&buffer);
        assert_eq!(sound.frames(), 44_100);
        assert!((sound.seconds() - 1.0).abs() < 1e-9);
        assert!((sound.mono()[22_050] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn smoothing_keeps_the_length_and_the_level_of_a_steady_signal() {
        let smoothed = smooth(&[1.0; 100], 9);
        assert_eq!(smoothed.len(), 100);
        assert!((smoothed[50] - 1.0).abs() < 1e-6);
        // At the ends the window hangs over into silence.
        assert!(smoothed[0] < 1.0 && smoothed[99] < 1.0);
    }

    #[test]
    fn a_percentile_is_read_between_neighbours() {
        let values = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&values, 50.0), 3.0);
        assert_eq!(percentile(&values, 0.0), 1.0);
        assert_eq!(percentile(&values, 100.0), 5.0);
        assert!((percentile(&values, 90.0) - 4.6).abs() < 1e-12);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn levels_are_measured_frame_by_frame() {
        let mut signal = vec![0.0f32; 400];
        signal[200..].fill(0.5);
        let levels = frame_rms(&signal, 100, 100);
        assert_eq!(levels.len(), 4);
        assert!(levels[0] < 1e-6 && (levels[3] - 0.5).abs() < 1e-6);
        assert!((db(0.5) + 6.0206).abs() < 1e-3);
    }
}
