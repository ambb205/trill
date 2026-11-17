//! Spectra over time: the short-time transform every analysis starts from,
//! energies in bands spaced as the ear spaces them, and how strongly
//! something new begins in each frame.

use trill_dsp::fft::{RealFft, hann};

/// Runs `each` over the frames of a short-time transform of `signal`:
/// windows of `size` samples every `hop`, frame `t` centered on sample
/// `t * hop`, with silence taken to lie beyond both ends. `each` is given
/// the frame's number and the real and imaginary parts of its `size / 2 + 1`
/// bins, unscaled.
pub fn transform(signal: &[f32], size: usize, hop: usize, mut each: impl FnMut(usize, &[f32], &[f32])) {
    let mut fft = RealFft::new(size);
    let window = hann(size);
    let bins = size / 2 + 1;
    let (mut frame, mut re, mut im) = (vec![0.0f32; size], vec![0.0f32; bins], vec![0.0f32; bins]);
    let half = size / 2;
    for index in 0..frames(signal.len(), hop) {
        let centre = index * hop;
        for (offset, slot) in frame.iter_mut().enumerate() {
            let at = centre + offset;
            *slot = if at >= half && at - half < signal.len() { signal[at - half] * window[offset] } else { 0.0 };
        }
        fft.forward(&frame, &mut re, &mut im);
        each(index, &re, &im);
    }
}

/// How many frames [`transform`] makes of `samples` samples.
pub fn frames(samples: usize, hop: usize) -> usize {
    1 + samples / hop.max(1)
}

/// Values over time in rows: `width` of them for each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frames {
    pub width: usize,
    pub data: Vec<f32>,
}

impl Frames {
    pub fn new(width: usize, frames: usize) -> Self {
        Self { width, data: vec![0.0; width * frames] }
    }

    pub fn len(&self) -> usize {
        self.data.len().checked_div(self.width).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn row(&self, frame: usize) -> &[f32] {
        &self.data[frame * self.width..(frame + 1) * self.width]
    }

    pub fn row_mut(&mut self, frame: usize) -> &mut [f32] {
        &mut self.data[frame * self.width..(frame + 1) * self.width]
    }
}

/// The power in every bin of every frame.
pub fn power(signal: &[f32], size: usize, hop: usize) -> Frames {
    let mut out = Frames::new(size / 2 + 1, frames(signal.len(), hop));
    transform(signal, size, hop, |index, re, im| {
        for ((slot, re), im) in out.row_mut(index).iter_mut().zip(re).zip(im) {
            *slot = re * re + im * im;
        }
    });
    out
}

/// Hertz on the scale the ear hears pitch along: even steps up to a
/// kilohertz, even ratios above.
fn to_mel(hertz: f64) -> f64 {
    const STEP: f64 = 200.0 / 3.0;
    if hertz < 1000.0 { hertz / STEP } else { 1000.0 / STEP + (hertz / 1000.0).ln() / (6.4f64.ln() / 27.0) }
}

fn from_mel(mel: f64) -> f64 {
    const STEP: f64 = 200.0 / 3.0;
    let knee = 1000.0 / STEP;
    if mel < knee { mel * STEP } else { 1000.0 * ((mel - knee) * (6.4f64.ln() / 27.0)).exp() }
}

/// Triangular bands evenly spaced on the mel scale between two frequencies,
/// each weighted so that a flat spectrum fills every band alike.
pub struct MelBands {
    /// For each band, the first bin it draws on and its weight on that bin
    /// and the ones after.
    bands: Vec<(usize, Vec<f32>)>,
}

impl MelBands {
    pub fn new(rate: f64, size: usize, count: usize, low: f64, high: f64) -> Self {
        let (bottom, top) = (to_mel(low), to_mel(high));
        let edges: Vec<f64> =
            (0..count + 2).map(|index| from_mel(bottom + (top - bottom) * index as f64 / (count + 1) as f64)).collect();
        let bins = size / 2 + 1;
        let bands = (0..count)
            .map(|band| {
                let (left, centre, right) = (edges[band], edges[band + 1], edges[band + 2]);
                let scale = 2.0 / (right - left);
                let mut first = None;
                let mut weights = Vec::new();
                for bin in 0..bins {
                    let hertz = bin as f64 * rate / size as f64;
                    let weight = ((hertz - left) / (centre - left)).min((right - hertz) / (right - centre)).max(0.0);
                    if weight > 0.0 {
                        first.get_or_insert(bin);
                        weights.push((weight * scale) as f32);
                    } else if first.is_some() {
                        break;
                    }
                }
                (first.unwrap_or(0), weights)
            })
            .collect();
        Self { bands }
    }

    pub fn count(&self) -> usize {
        self.bands.len()
    }

    /// The energy of each band in one frame's power spectrum.
    pub fn apply(&self, power: &[f32], out: &mut [f32]) {
        for ((first, weights), slot) in self.bands.iter().zip(out) {
            *slot = weights.iter().zip(&power[*first..]).map(|(weight, power)| weight * power).sum();
        }
    }
}

/// The energy in `count` mel bands of every frame.
pub fn mel_power(signal: &[f32], rate: f64, size: usize, hop: usize, count: usize, low: f64, high: f64) -> Frames {
    let bands = MelBands::new(rate, size, count, low, high);
    let mut out = Frames::new(count, frames(signal.len(), hop));
    let mut power = vec![0.0f32; size / 2 + 1];
    transform(signal, size, hop, |index, re, im| {
        for ((slot, re), im) in power.iter_mut().zip(re).zip(im) {
            *slot = re * re + im * im;
        }
        bands.apply(&power, out.row_mut(index));
    });
    out
}

/// How strongly something begins in each frame: how much the mel bands,
/// in decibels, rise from one frame to the next, averaged over the bands.
/// A frame's value is put where the rise is heard, half a window after the
/// frame in which the window first reaches it.
pub fn onset_strength(signal: &[f32], rate: f64, size: usize, hop: usize, count: usize) -> Vec<f32> {
    let mel = mel_power(signal, rate, size, hop, count, 0.0, rate / 2.0);
    let total = mel.len();
    // Decibels, with everything more than 80 dB under the loudest band of
    // the whole signal held at that floor.
    let mut levels: Vec<f32> = mel.data.iter().map(|&power| 10.0 * power.max(1e-10).log10()).collect();
    let top = levels.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    levels.iter_mut().for_each(|level| *level = level.max(top - 80.0));

    let shift = 1 + size / (2 * hop);
    let mut out = vec![0.0f32; total];
    for frame in 1..total {
        let at = frame - 1 + shift;
        if at >= total {
            break;
        }
        let (now, before) = (&levels[frame * count..(frame + 1) * count], &levels[(frame - 1) * count..frame * count]);
        out[at] = now.iter().zip(before).map(|(now, before)| (now - before).max(0.0)).sum::<f32>() / count as f32;
    }
    out
}

/// The frames of `envelope` that stand out: the largest for `before_max`
/// frames back, at least `delta` over the mean of the `around` frames either
/// side, and more than `wait` frames after the one before.
pub fn pick_peaks(envelope: &[f32], before_max: usize, around: usize, delta: f32, wait: usize) -> Vec<usize> {
    let mut sums = Vec::with_capacity(envelope.len() + 1);
    let mut total = 0.0f64;
    sums.push(0.0);
    for &value in envelope {
        total += f64::from(value);
        sums.push(total);
    }
    let mut peaks: Vec<usize> = Vec::new();
    for (index, &value) in envelope.iter().enumerate() {
        let low = index.saturating_sub(before_max);
        if envelope[low..=index].iter().any(|&other| other > value) {
            continue;
        }
        let (from, to) = (index.saturating_sub(around), (index + around + 1).min(envelope.len()));
        let mean = (sums[to] - sums[from]) / (to - from) as f64;
        if f64::from(value) < mean + f64::from(delta) {
            continue;
        }
        if peaks.last().is_some_and(|&last| index <= last + wait) {
            continue;
        }
        peaks.push(index);
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    #[test]
    fn a_tone_is_found_in_the_bin_and_the_band_that_hold_its_frequency() {
        let rate = 22_050.0;
        let signal: Vec<f32> = (0..22_050).map(|i| (TAU * 1000.0 * i as f64 / rate).sin() as f32).collect();
        let spectrum = power(&signal, 1024, 256);
        assert_eq!(spectrum.len(), 1 + 22_050 / 256);
        let middle = spectrum.row(40);
        let loudest = (0..middle.len()).max_by(|&a, &b| middle[a].total_cmp(&middle[b])).unwrap();
        assert_eq!(loudest, (1000.0 * 1024.0 / rate).round() as usize);

        let mel = mel_power(&signal, rate, 1024, 256, 40, 0.0, rate / 2.0);
        let row = mel.row(40);
        let band = (0..40).max_by(|&a, &b| row[a].total_cmp(&row[b])).unwrap();
        // 1 kHz is where the scale turns from even steps to even ratios: 15
        // of the 50 steps that reach 11 kHz, so about the twelfth of 40 bands.
        assert!((11..=12).contains(&band), "{band}");
    }

    #[test]
    fn the_mel_scale_goes_there_and_back() {
        for hertz in [30.0, 440.0, 1000.0, 4000.0, 11_025.0] {
            assert!((from_mel(to_mel(hertz)) - hertz).abs() < 1e-6);
        }
    }

    #[test]
    fn a_click_is_an_onset_where_it_is_and_nowhere_else() {
        let rate = 22_050.0;
        let mut signal = vec![0.0f32; 22_050];
        for (index, slot) in signal[11_025..11_025 + 400].iter_mut().enumerate() {
            *slot = (TAU * 2000.0 * index as f64 / rate).sin() as f32 * (1.0 - index as f32 / 400.0);
        }
        let strength = onset_strength(&signal, rate, 1024, 128, 64);
        let peak = (0..strength.len()).max_by(|&a, &b| strength[a].total_cmp(&strength[b])).unwrap();
        let seconds = peak as f64 * 128.0 / rate;
        assert!((seconds - 0.5).abs() < 0.03, "{seconds}");
        let peaks = pick_peaks(&strength, 5, 17, 0.5, 7);
        assert_eq!(peaks, vec![peak]);
    }
}
