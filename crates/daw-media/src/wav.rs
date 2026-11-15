//! Writing WAV files.

use crate::MediaError;
use daw_core::audio::AudioBuffer;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// The sample format of a WAV file to write.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WavFormat {
    /// 16-bit integer: the CD format, read by everything.
    Int16,
    /// 24-bit integer: the usual format for masters and stems.
    Int24,
    /// 32-bit float: keeps levels above full scale; for files that will be
    /// processed further.
    Float32,
}

impl WavFormat {
    fn bytes_per_sample(self) -> u32 {
        match self {
            Self::Int16 => 2,
            Self::Int24 => 3,
            Self::Float32 => 4,
        }
    }
}

const CHANNELS: u32 = 2;
/// Frames converted per write, to keep the conversion buffer small.
const CHUNK_FRAMES: usize = 4096;

/// Writes `buffer` as a stereo WAV file, creating missing parent folders.
///
/// Integer formats are rounded to the nearest step and clipped at full
/// scale, without dither: whether and how to dither is a mastering choice
/// that belongs to the caller. Samples that are not finite numbers are
/// written as silence.
///
/// The file is written under a temporary name and moved into place when
/// complete, so a failed export never leaves a truncated file where a good
/// one was.
pub fn write_wav(path: &Path, buffer: &AudioBuffer, format: WavFormat) -> Result<(), MediaError> {
    let rate = buffer.sample_rate.round();
    // The header also stores bytes per second, which must fit 32 bits too.
    if !(1.0..=f64::from(u32::MAX / 8)).contains(&rate) {
        return Err(MediaError::write(path, format!("{} is not a usable sample rate", buffer.sample_rate)));
    }
    let frames = buffer.frames();
    let block_align = CHANNELS * format.bytes_per_sample();
    let header = header(format, rate as u32, frames, block_align)
        .ok_or_else(|| MediaError::write(path, "the audio is longer than a WAV file can hold (4 GiB)"))?;

    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| MediaError::write(path, error))?;
    }
    let temporary = temporary_path(path);
    let written = write_file(&temporary, &header, buffer, frames, format)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| MediaError::write(path, error));
    if written.is_err() {
        // Best effort: the error being reported is the one that matters.
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// The bytes before the samples, or `None` if the sizes overflow the
/// format's 32-bit fields.
fn header(format: WavFormat, sample_rate: u32, frames: usize, block_align: u32) -> Option<Vec<u8>> {
    let data_len = u32::try_from(frames).ok()?.checked_mul(block_align)?;
    let float = format == WavFormat::Float32;
    // Non-PCM formats carry a `fact` chunk stating the length in frames.
    let fact_len: u32 = if float { 12 } else { 0 };
    let riff_len = 4u32.checked_add(8 + 16)?.checked_add(fact_len)?.checked_add(8)?.checked_add(data_len)?;
    // A data chunk of odd length is followed by a pad byte; stereo frames
    // are always even, so there is none to account for.

    let mut bytes = Vec::with_capacity(64);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    let format_tag: u16 = if float { 3 } else { 1 };
    bytes.extend_from_slice(&format_tag.to_le_bytes());
    bytes.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.checked_mul(block_align)?.to_le_bytes());
    bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
    bytes.extend_from_slice(&((format.bytes_per_sample() * 8) as u16).to_le_bytes());
    if float {
        bytes.extend_from_slice(b"fact");
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&(frames as u32).to_le_bytes());
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    Some(bytes)
}

fn write_file(
    path: &Path,
    header: &[u8],
    buffer: &AudioBuffer,
    frames: usize,
    format: WavFormat,
) -> std::io::Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    file.write_all(header)?;
    let mut bytes = Vec::with_capacity(CHUNK_FRAMES * (CHANNELS * format.bytes_per_sample()) as usize);
    let (left, right) = (&buffer.left[..frames], &buffer.right[..frames]);
    for (left, right) in left.chunks(CHUNK_FRAMES).zip(right.chunks(CHUNK_FRAMES)) {
        bytes.clear();
        for (&l, &r) in left.iter().zip(right) {
            for sample in [l, r] {
                let sample = if sample.is_finite() { sample } else { 0.0 };
                match format {
                    WavFormat::Int16 => bytes.extend_from_slice(&(quantise(sample, 32_767.0) as i16).to_le_bytes()),
                    WavFormat::Int24 => bytes.extend_from_slice(&quantise(sample, 8_388_607.0).to_le_bytes()[..3]),
                    WavFormat::Float32 => bytes.extend_from_slice(&sample.to_le_bytes()),
                }
            }
        }
        file.write_all(&bytes)?;
    }
    // Flush explicitly: dropping a `BufWriter` discards write errors.
    file.into_inner().map_err(std::io::IntoInnerError::into_error)?.sync_all()
}

/// `sample` as an integer where ±1.0 maps to ±`full_scale`.
fn quantise(sample: f32, full_scale: f32) -> i32 {
    (sample.clamp(-1.0, 1.0) * full_scale).round() as i32
}

/// A sibling of `path` to write to before moving into place. It must be in
/// the same folder for the move to be a rename rather than a copy.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.part", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize) -> AudioBuffer {
        let left: Vec<f32> = (0..frames).map(|n| n as f32 / frames as f32 * 2.0 - 1.0).collect();
        let right: Vec<f32> = left.iter().map(|sample| -sample * 0.5).collect();
        AudioBuffer { sample_rate: 44_100.0, left, right }
    }

    fn read_back(path: &Path) -> (hound::WavSpec, Vec<f32>) {
        let mut reader = hound::WavReader::open(path).unwrap();
        let spec = reader.spec();
        let samples = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().map(Result::unwrap).collect(),
            hound::SampleFormat::Int => {
                let scale = ((1i64 << (spec.bits_per_sample - 1)) - 1) as f32;
                reader.samples::<i32>().map(|sample| sample.unwrap() as f32 / scale).collect()
            }
        };
        (spec, samples)
    }

    #[test]
    fn every_format_reads_back_through_an_independent_reader() {
        let folder = tempfile::tempdir().unwrap();
        let buffer = ramp(10_001);
        for (format, bits, float, tolerance) in [
            (WavFormat::Int16, 16, false, 1.0 / 32_767.0),
            (WavFormat::Int24, 24, false, 1.0 / 8_388_607.0),
            (WavFormat::Float32, 32, true, 0.0),
        ] {
            let path = folder.path().join(format!("nested/out {bits}.wav"));
            write_wav(&path, &buffer, format).unwrap();
            let (spec, samples) = read_back(&path);
            assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (2, 44_100, bits));
            assert_eq!(spec.sample_format == hound::SampleFormat::Float, float);
            assert_eq!(samples.len(), 2 * buffer.frames());
            for (frame, pair) in samples.chunks_exact(2).enumerate() {
                assert!((pair[0] - buffer.left[frame]).abs() <= tolerance, "{format:?} left {frame}");
                assert!((pair[1] - buffer.right[frame]).abs() <= tolerance, "{format:?} right {frame}");
            }
            let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name.to_string_lossy().ends_with(".part"))
                .collect();
            assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
        }
    }

    #[test]
    fn integers_clip_and_odd_samples_are_silenced() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("clip.wav");
        let buffer = AudioBuffer {
            sample_rate: 48_000.0,
            left: vec![2.0, -2.0, f32::NAN, 1.0],
            right: vec![f32::INFINITY, 0.0, -1.0, 0.5],
        };
        write_wav(&path, &buffer, WavFormat::Int16).unwrap();
        let samples: Vec<i16> = hound::WavReader::open(&path).unwrap().samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(samples, [32_767, 0, -32_767, 0, 0, -32_767, 32_767, 16_384]);
    }

    #[test]
    fn the_shorter_channel_sets_the_length_and_empty_audio_is_a_valid_file() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("short.wav");
        let buffer = AudioBuffer { sample_rate: 48_000.0, left: vec![0.25; 10], right: vec![0.25; 4] };
        write_wav(&path, &buffer, WavFormat::Int24).unwrap();
        assert_eq!(hound::WavReader::open(&path).unwrap().duration(), 4);

        let empty = AudioBuffer { sample_rate: 48_000.0, ..AudioBuffer::default() };
        write_wav(&path, &empty, WavFormat::Float32).unwrap();
        assert_eq!(hound::WavReader::open(&path).unwrap().duration(), 0);
    }

    #[test]
    fn failures_are_reported_and_leave_the_existing_file_alone() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("keep.wav");
        write_wav(&path, &ramp(100), WavFormat::Int16).unwrap();
        let before = std::fs::read(&path).unwrap();

        let no_rate = AudioBuffer { sample_rate: 0.0, ..ramp(100) };
        let error = write_wav(&path, &no_rate, WavFormat::Int16).unwrap_err();
        assert!(matches!(error, MediaError::Write { .. }), "{error}");
        assert!(error.to_string().contains("sample rate"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // A folder where the file should go cannot be replaced by a file.
        let blocked = folder.path().join("blocked.wav");
        std::fs::create_dir(&blocked).unwrap();
        let error = write_wav(&blocked, &ramp(100), WavFormat::Int16).unwrap_err();
        assert!(matches!(error, MediaError::Write { .. }), "{error}");
        assert!(blocked.is_dir());
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 2, "no temporary file left behind");
    }

    #[test]
    fn sizes_beyond_the_format_are_refused() {
        assert!(header(WavFormat::Float32, 48_000, 1 << 29, 8).is_none());
        assert!(header(WavFormat::Int16, 48_000, 1 << 29, 4).is_some());
        assert!(header(WavFormat::Int16, u32::MAX, 10, 4).is_none());
    }
}
