//! Helpers for this crate's unit tests.

use daw_core::audio::{AudioBuffer, AudioLoader};
use daw_core::device::{DeviceProcessor, Event, EventKind, ProcessInfo};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
