//! Helpers for this crate's unit tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use trill_core::audio::{AudioBuffer, AudioLoader};
use trill_core::device::{DeviceProcessor, Event, EventKind, ProcessInfo};

/// An [`AudioLoader`] that serves buffers from memory instead of from disk.
#[derive(Default)]
pub struct MemoryLoader {
    files: Mutex<HashMap<String, Arc<AudioBuffer>>>,
}

impl MemoryLoader {
    /// Adds (or replaces) a mono file.
    pub fn insert(&self, path: &str, samples: Vec<f32>, sample_rate: f64) {
        self.insert_stereo(path, samples.clone(), samples, sample_rate);
    }

    pub fn insert_stereo(&self, path: &str, left: Vec<f32>, right: Vec<f32>, sample_rate: f64) {
        let buffer = Arc::new(AudioBuffer { sample_rate, left, right });
        self.files.lock().unwrap().insert(path.to_string(), buffer);
    }
}

impl AudioLoader for MemoryLoader {
    fn load(&self, path: &str) -> Result<Arc<AudioBuffer>, String> {
        self.files.lock().unwrap().get(path).cloned().ok_or_else(|| "no such file".to_string())
    }
}

pub fn sine(hz: f64, sample_rate: f64, frames: usize) -> Vec<f32> {
    (0..frames).map(|i| (std::f64::consts::TAU * hz * i as f64 / sample_rate).sin() as f32).collect()
}

pub fn info(sample_rate: f64) -> ProcessInfo {
    ProcessInfo {
        sample_rate,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    }
}

/// Runs `device` for `frames` frames in blocks of `block`, delivering each
/// `(frame, event)` at its frame. Returns the left and right outputs.
pub fn run(
    device: &mut dyn DeviceProcessor,
    sample_rate: f64,
    block: usize,
    frames: usize,
    events: &[(usize, EventKind)],
) -> (Vec<f32>, Vec<f32>) {
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let info = info(sample_rate);
    let mut start = 0;
    while start < frames {
        let end = (start + block).min(frames);
        let block_events: Vec<Event> = events
            .iter()
            .filter(|(frame, _)| (start..end).contains(frame))
            .map(|(frame, kind)| Event { offset: (frame - start) as u32, kind: *kind })
            .collect();
        device.process(&info, &block_events, &mut left[start..end], &mut right[start..end]);
        start = end;
    }
    (left, right)
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |max, sample| max.max(sample.abs()))
}

pub fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// The largest jump between neighbouring samples: what a click looks like.
pub fn largest_step(samples: &[f32]) -> f32 {
    samples.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max)
}

/// Frequency from the spacing of rising zero crossings, each located by
/// linear interpolation.
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

/// How much of the tone at `hz` is in `samples`: the amplitude of the sine
/// of that frequency that accounts for it, whatever its phase.
pub fn amplitude(samples: &[f32], hz: f64, sample_rate: f64) -> f32 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, sample) in samples.iter().enumerate() {
        let phase = std::f64::consts::TAU * hz * n as f64 / sample_rate;
        re += f64::from(*sample) * phase.cos();
        im += f64::from(*sample) * phase.sin();
    }
    (2.0 * (re * re + im * im).sqrt() / samples.len().max(1) as f64) as f32
}

/// The level of each frequency in `samples`, in dB where a sine of full
/// scale is 0: one figure for each bin from 0 Hz up to half the sample
/// rate. `samples` has to be a power of two long.
pub fn spectrum_db(samples: &[f32]) -> Vec<f32> {
    let mut fft = crate::fft::RealFft::new(samples.len());
    let window = crate::fft::hann(samples.len());
    let windowed: Vec<f32> = samples.iter().zip(&window).map(|(sample, weight)| sample * weight).collect();
    let (mut re, mut im) = (vec![0.0f32; fft.bins()], vec![0.0f32; fft.bins()]);
    fft.forward(&windowed, &mut re, &mut im);
    let scale = 2.0 / window.iter().sum::<f32>();
    re.iter().zip(&im).map(|(re, im)| 20.0 * ((re * re + im * im).sqrt() * scale).max(1e-12).log10()).collect()
}

/// Instrument files that are in memory instead of on a disk.
#[derive(Default)]
pub struct MemoryTexts {
    files: Mutex<HashMap<std::path::PathBuf, (String, u64)>>,
}

impl MemoryTexts {
    /// Adds a file, or changes it.
    pub fn insert(&self, path: &str, text: &str) {
        let mut files = self.files.lock().unwrap();
        let version = files.get(std::path::Path::new(path)).map_or(0, |(_, version)| version + 1);
        files.insert(path.into(), (text.to_string(), version));
    }
}

impl crate::sampled::loaded::Texts for MemoryTexts {
    fn read(&self, path: &std::path::Path) -> Result<String, String> {
        let files = self.files.lock().unwrap();
        files.get(path).map(|(text, _)| text.clone()).ok_or_else(|| "there is no such file".to_string())
    }

    fn stamp(&self, path: &std::path::Path) -> Option<u64> {
        self.files.lock().unwrap().get(path).map(|(_, version)| *version)
    }
}
