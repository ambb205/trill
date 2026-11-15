//! Measurement helpers shared by the integration tests and the `stretch`
//! example: the audio cannot be listened to in a test, so it is measured.

#![allow(dead_code)]

use daw_dsp::fft::RealFft;
use std::f64::consts::PI;

pub fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |max, s| max.max(s.abs()))
}

pub fn db(ratio: f64) -> f64 {
    20.0 * ratio.max(1e-12).log10()
}

pub fn sine(hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f32> {
    (0..frames).map(|i| (amplitude * (2.0 * PI * hz * i as f64 / sample_rate).sin()) as f32).collect()
}

/// Deterministic white noise in -1..1.
pub fn noise(index: usize) -> f32 {
    let x = (index as u32).wrapping_mul(2_654_435_761).rotate_left(13).wrapping_mul(0x9E37_79B9);
    (x >> 8) as f32 / 8_388_608.0 - 1.0
}

/// Frequency of a sine, from the average spacing of its rising zero
/// crossings (each located by linear interpolation). Accurate to a small
/// fraction of a hertz over a second of audio.
pub fn zero_crossing_hz(samples: &[f32], sample_rate: f64) -> f64 {
    let crossings: Vec<f64> = samples
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0] <= 0.0 && pair[1] > 0.0)
        .map(|(i, pair)| i as f64 + (-pair[0] / (pair[1] - pair[0])) as f64)
        .collect();
    match (crossings.first(), crossings.last()) {
        (Some(first), Some(last)) if crossings.len() > 1 => (crossings.len() - 1) as f64 * sample_rate / (last - first),
        _ => 0.0,
    }
}

/// Power spectrum of the first `size` samples under a 4-term
/// Blackman-Harris window (side lobes below -92 dB), scaled so that a sine
/// of amplitude 1 sums to a power of 1 across its main lobe.
pub fn power_spectrum(samples: &[f32], size: usize) -> Vec<f64> {
    let window: Vec<f64> = (0..size)
        .map(|i| {
            let x = 2.0 * PI * i as f64 / size as f64;
            0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos()
        })
        .collect();
    let frame: Vec<f32> = samples[..size].iter().zip(&window).map(|(s, w)| (*s as f64 * w) as f32).collect();
    let mut fft = RealFft::new(size);
    let (mut re, mut im) = (vec![0.0; fft.bins()], vec![0.0; fft.bins()]);
    fft.forward(&frame, &mut re, &mut im);
    // A unit sine puts (size / 2)^2 * sum(w^2) / size... into its lobe;
    // normalizing by sum(w^2) * size / 4 makes that lobe sum to 1.
    let norm = window.iter().map(|w| w * w).sum::<f64>() * size as f64 / 4.0;
    re.iter().zip(&im).map(|(re, im)| ((*re as f64).powi(2) + (*im as f64).powi(2)) / norm).collect()
}

pub struct SineReport {
    pub hz: f64,
    /// Level of the fundamental relative to an amplitude of 1, in dB.
    pub level_db: f64,
    /// Everything that is not the fundamental, relative to it, in dB.
    pub spurious_db: f64,
    /// The single largest other component: its level relative to the
    /// fundamental, in dB, and its frequency.
    pub worst_db: f64,
    pub worst_hz: f64,
}

/// Analyses what should be a steady sine.
pub fn measure_sine(samples: &[f32], sample_rate: f64) -> SineReport {
    let size = (samples.len().next_power_of_two() / 2).min(1 << 17);
    let spectrum = power_spectrum(samples, size);
    let peak_bin = spectrum.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(bin, _)| bin);
    // The Blackman-Harris main lobe is 8 bins wide.
    let lobe = peak_bin.saturating_sub(5)..(peak_bin + 6).min(spectrum.len());
    let fundamental: f64 = spectrum[lobe.clone()].iter().sum();
    let (mut rest, mut worst, mut worst_bin) = (0.0, 0.0, 0);
    for (bin, &power) in spectrum.iter().enumerate() {
        if !lobe.contains(&bin) {
            rest += power;
            // A component's power is spread over its lobe; compare lobes.
            let around: f64 = spectrum[bin.saturating_sub(4)..(bin + 5).min(spectrum.len())].iter().sum();
            if around > worst && (bin < lobe.start.saturating_sub(4) || bin >= lobe.end + 4) {
                (worst, worst_bin) = (around, bin);
            }
        }
    }
    SineReport {
        hz: zero_crossing_hz(samples, sample_rate),
        level_db: 10.0 * fundamental.max(1e-24).log10(),
        spurious_db: 10.0 * (rest / fundamental).max(1e-24).log10(),
        worst_db: 10.0 * (worst / fundamental).max(1e-24).log10(),
        worst_hz: worst_bin as f64 * sample_rate / size as f64,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Click {
    /// Frame of the loudest sample.
    pub frame: usize,
    pub peak: f32,
    /// Frames from the first to the last sample above a tenth of the peak.
    pub width: usize,
}

/// Finds isolated clicks: groups of samples above `threshold` separated by
/// at least `gap` quieter frames.
pub fn find_clicks(samples: &[f32], threshold: f32, gap: usize) -> Vec<Click> {
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for (frame, sample) in samples.iter().enumerate() {
        if sample.abs() > threshold {
            match groups.last_mut() {
                Some(group) if frame - group.1 <= gap => group.1 = frame,
                _ => groups.push((frame, frame)),
            }
        }
    }
    groups
        .into_iter()
        .map(|(first, last)| {
            let (frame, peak) =
                (first..=last).map(|i| (i, samples[i].abs())).max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            let lo = first.saturating_sub(gap);
            let hi = (last + gap).min(samples.len() - 1);
            let above: Vec<usize> = (lo..=hi).filter(|&i| samples[i].abs() > peak * 0.1).collect();
            Click { frame, peak, width: above.last().unwrap() - above.first().unwrap() + 1 }
        })
        .collect()
}

/// Correlation coefficient of two channels: 1 for identical, 0 for
/// unrelated, -1 for inverted.
pub fn correlation(left: &[f32], right: &[f32]) -> f64 {
    let (mut dot, mut l2, mut r2) = (0.0, 0.0, 0.0);
    for (l, r) in left.iter().zip(right) {
        dot += *l as f64 * *r as f64;
        l2 += (*l as f64).powi(2);
        r2 += (*r as f64).powi(2);
    }
    dot / (l2 * r2).sqrt().max(1e-30)
}

/// Long-term average level in third-octave bands from 31.5 Hz up, in dB.
pub fn band_levels_db(samples: &[f32], sample_rate: f64) -> Vec<f64> {
    const SIZE: usize = 8192;
    let mut total = vec![0.0f64; SIZE / 2 + 1];
    let mut frames = 0;
    let mut start = 0;
    while start + SIZE <= samples.len() {
        for (sum, power) in total.iter_mut().zip(power_spectrum(&samples[start..], SIZE)) {
            *sum += power;
        }
        frames += 1;
        start += SIZE / 2;
    }
    let mut levels = Vec::new();
    let mut low = 28.0;
    while low * 1.26 < (sample_rate / 2.0).min(20_000.0) {
        let high = low * 1.26;
        let bins =
            (low * SIZE as f64 / sample_rate).ceil() as usize..(high * SIZE as f64 / sample_rate).ceil() as usize;
        if !bins.is_empty() {
            let power: f64 = total[bins].iter().sum::<f64>() / frames.max(1) as f64;
            levels.push(10.0 * power.max(1e-20).log10());
        }
        low = high;
    }
    levels
}

/// An [`AudioLoader`] that serves buffers from memory instead of from disk,
/// and counts how often it is asked.
#[derive(Default)]
pub struct MemoryLoader {
    files: std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<daw_core::audio::AudioBuffer>>>,
    loads: std::sync::atomic::AtomicUsize,
}

impl MemoryLoader {
    pub fn insert(&self, path: &str, samples: Vec<f32>, sample_rate: f64) {
        self.insert_stereo(path, samples.clone(), samples, sample_rate);
    }

    pub fn insert_stereo(&self, path: &str, left: Vec<f32>, right: Vec<f32>, sample_rate: f64) {
        let buffer = std::sync::Arc::new(daw_core::audio::AudioBuffer { sample_rate, left, right });
        self.files.lock().unwrap().insert(path.to_string(), buffer);
    }

    /// How many times [`AudioLoader::load`] has been called.
    pub fn loads(&self) -> usize {
        self.loads.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl daw_core::audio::AudioLoader for MemoryLoader {
    fn load(&self, path: &str) -> Result<std::sync::Arc<daw_core::audio::AudioBuffer>, String> {
        self.loads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.files.lock().unwrap().get(path).cloned().ok_or_else(|| format!("'{path}' does not exist"))
    }
}
