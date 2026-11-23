//! Writing WAV files.

use crate::MediaError;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use trill_core::audio::AudioBuffer;

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
        encode(format, left, right, &mut bytes);
        file.write_all(&bytes)?;
    }
    // Flush explicitly: dropping a `BufWriter` discards write errors.
    file.into_inner().map_err(std::io::IntoInnerError::into_error)?.sync_all()
}

/// Appends the frames of `left` and `right` to `bytes` as `format` has
/// them in a file.
fn encode(format: WavFormat, left: &[f32], right: &[f32], bytes: &mut Vec<u8>) {
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
}

/// A stereo WAV file that is written a piece at a time, for audio that is
/// not kept in memory: a stem of a render, made as the render goes.
///
/// As with [`write_wav`], the file is written under a temporary name and
/// moved into place by [`WavWriter::finish`]; one that is let go of
/// without being finished leaves nothing behind.
pub struct WavWriter {
    path: PathBuf,
    temporary: PathBuf,
    /// `None` once it is finished.
    file: Option<BufWriter<File>>,
    format: WavFormat,
    sample_rate: u32,
    frames: u64,
    /// The first write that failed. Nothing is written after it.
    failed: Option<String>,
    bytes: Vec<u8>,
}

impl WavWriter {
    /// Begins a file at `path`, creating missing parent folders.
    pub fn create(path: &Path, sample_rate: f64, format: WavFormat) -> Result<Self, MediaError> {
        let rate = sample_rate.round();
        if !(1.0..=f64::from(u32::MAX / 8)).contains(&rate) {
            return Err(MediaError::write(path, format!("{sample_rate} is not a usable sample rate")));
        }
        let block_align = CHANNELS * format.bytes_per_sample();
        // Room for the header, which is written again when the length is
        // known.
        let header = header(format, rate as u32, 0, block_align).expect("an empty file fits");
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| MediaError::write(path, error))?;
        }
        let temporary = temporary_path(path);
        let begun = File::create(&temporary).and_then(|file| {
            let mut file = BufWriter::new(file);
            file.write_all(&header)?;
            Ok(file)
        });
        let file = begun.map_err(|error| MediaError::write(path, error))?;
        Ok(Self {
            path: path.to_path_buf(),
            temporary,
            file: Some(file),
            format,
            sample_rate: rate as u32,
            frames: 0,
            failed: None,
            bytes: Vec::with_capacity(CHUNK_FRAMES * block_align as usize),
        })
    }

    /// The rate the file is written at, in frames a second.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Where the file will be.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends frames. A failure is kept and reported by
    /// [`WavWriter::finish`]: whoever makes the audio need not stop for it.
    pub fn write(&mut self, left: &[f32], right: &[f32]) {
        let Some(file) = self.file.as_mut().filter(|_| self.failed.is_none()) else { return };
        for (left, right) in left.chunks(CHUNK_FRAMES).zip(right.chunks(CHUNK_FRAMES)) {
            self.bytes.clear();
            encode(self.format, left, right, &mut self.bytes);
            if let Err(error) = file.write_all(&self.bytes) {
                self.failed = Some(error.to_string());
                return;
            }
            self.frames += left.len().min(right.len()) as u64;
        }
    }

    /// Ends the file and moves it into place, keeping its first `frames`
    /// frames if it has more than that (what a render wrote after its
    /// last sound). Returns how many frames the file holds.
    pub fn finish(mut self, frames: Option<u64>) -> Result<u64, MediaError> {
        let path = self.path.clone();
        if let Some(reason) = self.failed.take() {
            return Err(MediaError::write(&path, reason));
        }
        let frames = frames.map_or(self.frames, |frames| frames.min(self.frames));
        let block_align = CHANNELS * self.format.bytes_per_sample();
        let length = usize::try_from(frames).ok();
        let header = length
            .and_then(|frames| header(self.format, self.sample_rate, frames, block_align))
            .ok_or_else(|| MediaError::write(&path, "the audio is longer than a WAV file can hold (4 GiB)"))?;
        let file = self.file.take().expect("finished once");
        let written = (|| {
            let mut file = file.into_inner().map_err(std::io::IntoInnerError::into_error)?;
            file.set_len(header.len() as u64 + frames * u64::from(block_align))?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&header)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&self.temporary, &self.path)
        })();
        written.map_err(|error: std::io::Error| MediaError::write(&path, error))?;
        Ok(frames)
    }
}

impl Drop for WavWriter {
    fn drop(&mut self) {
        // Never finished, or finishing it failed: the part that was
        // written is no use to anyone. (After a finish that worked, the
        // file has moved and there is nothing under this name.)
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.temporary);
    }
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
    fn a_file_written_a_piece_at_a_time_is_the_file_written_at_once_and_can_be_cut_short() {
        let folder = tempfile::tempdir().unwrap();
        let buffer = ramp(10_001);
        for format in [WavFormat::Int16, WavFormat::Int24, WavFormat::Float32] {
            let whole = folder.path().join("whole.wav");
            write_wav(&whole, &buffer, format).unwrap();
            // In pieces of every size, the last of them a single frame.
            let pieces = folder.path().join("made").join(format!("{format:?}.wav"));
            let mut writer = WavWriter::create(&pieces, buffer.sample_rate, format).unwrap();
            assert!(!pieces.exists(), "nothing is there until it is finished");
            let mut at = 0;
            for size in [1, 4096, 5000, 903, 1] {
                writer.write(&buffer.left[at..at + size], &buffer.right[at..at + size]);
                at += size;
            }
            assert_eq!(writer.finish(None).unwrap(), 10_001);
            assert_eq!(std::fs::read(&pieces).unwrap(), std::fs::read(&whole).unwrap(), "{format:?}");

            // Cut short: the first frames only, as the header then says.
            let mut writer = WavWriter::create(&pieces, buffer.sample_rate, format).unwrap();
            writer.write(&buffer.left, &buffer.right);
            assert_eq!(writer.finish(Some(700)).unwrap(), 700);
            let short = AudioBuffer {
                sample_rate: buffer.sample_rate,
                left: buffer.left[..700].to_vec(),
                right: buffer.right[..700].to_vec(),
            };
            write_wav(&whole, &short, format).unwrap();
            assert_eq!(std::fs::read(&pieces).unwrap(), std::fs::read(&whole).unwrap(), "{format:?}");
            // Asked to keep more than there is, it keeps what there is.
            let mut writer = WavWriter::create(&pieces, buffer.sample_rate, format).unwrap();
            writer.write(&buffer.left[..10], &buffer.right[..10]);
            assert_eq!(writer.finish(Some(700)).unwrap(), 10);
        }
        // One that is let go of leaves nothing behind.
        let dropped = folder.path().join("made").join("dropped.wav");
        let mut writer = WavWriter::create(&dropped, 48_000.0, WavFormat::Float32).unwrap();
        writer.write(&buffer.left, &buffer.right);
        drop(writer);
        let made = std::fs::read_dir(folder.path().join("made")).unwrap();
        let left: Vec<_> = made.flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(left.len(), 3, "the three that were finished: {left:?}");
        assert!(WavWriter::create(&dropped, 0.0, WavFormat::Float32).is_err());
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
