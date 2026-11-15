//! Reading and writing audio files.

use crate::graph::AudioData;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum AudioFileError {
    #[error("cannot read '{path}': {reason}")]
    Read { path: String, reason: String },
    #[error("cannot write '{path}': {reason}")]
    Write { path: String, reason: String },
}

/// Decodes a WAV file to stereo float. Mono files are duplicated to both
/// channels; extra channels are ignored.
pub fn read_wav(path: &Path) -> Result<AudioData, AudioFileError> {
    let fail = |reason: String| AudioFileError::Read { path: path.display().to_string(), reason };
    let mut reader = hound::WavReader::open(path).map_err(|e| fail(e.to_string()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => {
            reader.samples::<f32>().collect::<Result<_, _>>().map_err(|e| fail(e.to_string()))?
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| fail(e.to_string()))?
        }
    };

    let frames = interleaved.len() / channels;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for frame in interleaved.chunks_exact(channels) {
        left.push(frame[0]);
        right.push(frame[if channels > 1 { 1 } else { 0 }]);
    }
    Ok(AudioData { sample_rate: spec.sample_rate as f64, left, right })
}

/// Writes a 24-bit stereo WAV file.
pub fn write_wav(path: &Path, sample_rate: u32, left: &[f32], right: &[f32]) -> Result<(), AudioFileError> {
    let fail = |reason: String| AudioFileError::Write { path: path.display().to_string(), reason };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| fail(e.to_string()))?;
    }
    let spec =
        hound::WavSpec { channels: 2, sample_rate, bits_per_sample: 24, sample_format: hound::SampleFormat::Int };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| fail(e.to_string()))?;
    const SCALE: f32 = 8_388_607.0;
    for (l, r) in left.iter().zip(right) {
        for sample in [l, r] {
            let value = (sample.clamp(-1.0, 1.0) * SCALE).round() as i32;
            writer.write_sample(value).map_err(|e| fail(e.to_string()))?;
        }
    }
    writer.finalize().map_err(|e| fail(e.to_string()))
}

/// Decoded audio files, keyed by path, so a file used by many clips is
/// loaded once. Failures are remembered too, so a missing file is not
/// re-read on every edit.
#[derive(Default)]
pub struct AudioCache {
    files: HashMap<String, Result<Arc<AudioData>, String>>,
}

impl AudioCache {
    pub fn load(&mut self, path: &str) -> Result<Arc<AudioData>, String> {
        self.files
            .entry(path.to_string())
            .or_insert_with(|| read_wav(Path::new(path)).map(Arc::new).map_err(|e| e.to_string()))
            .clone()
    }

    /// Forgets failures so that files that have since appeared are retried.
    pub fn forget_failures(&mut self) {
        self.files.retain(|_, result| result.is_ok());
    }
}
