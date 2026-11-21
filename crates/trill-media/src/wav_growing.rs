//! A WAV file that is written while what it holds is still being made: a
//! take, as it is recorded.
//!
//! Unlike [`WavWriter`](crate::WavWriter), which keeps its file under
//! another name until it is whole, this one is the file from its first
//! frame: a recording is nobody's to throw away because the program that
//! was making it did not live to finish it. Every second of audio it puts
//! what it has on disk and writes the length into the header again, so
//! that whatever becomes of the program the file is a WAV file that reads,
//! holding all but its last second or so. It is 32-bit float, one channel
//! or two, and nothing else: what a microphone gave, as it gave it.

use crate::MediaError;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Where in the header the three lengths are.
const RIFF_LENGTH_AT: u64 = 4;
const FRAMES_AT: u64 = 44;
const DATA_LENGTH_AT: u64 = 52;
const HEADER_BYTES: u64 = 56;
/// A WAV file's lengths are 32 bits: this is all one holds, header and all.
const MOST_BYTES: u64 = u32::MAX as u64 - 1;

/// What a file is held to. As it comes, to what a WAV file can be and to
/// the disk it is on; a test lowers either, to reach in a second what a
/// take reaches in hours or on a disk that fills.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes a file may be, header and all.
    pub most_bytes: u64,
    /// The byte of the file at which writing fails as it does on a disk
    /// that is full: what comes before it is written, and nothing after.
    pub fails_at: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Self { most_bytes: MOST_BYTES, fails_at: None }
    }
}

/// See the module documentation.
pub struct GrowingWav {
    path: PathBuf,
    file: File,
    limits: Limits,
    /// How many bytes of it are on disk, header and all.
    on_disk: u64,
    channels: u16,
    sample_rate: u32,
    /// Frames handed over, and how many of them the header on disk says.
    frames: u64,
    said: u64,
    /// What is handed over and not yet written.
    waiting: Vec<u8>,
}

impl GrowingWav {
    /// Begins a file at `path`, creating missing parent folders. It is
    /// there from now on: a file of no length that reads as one.
    pub fn create(path: &Path, sample_rate: f64, channels: u16) -> Result<Self, MediaError> {
        Self::create_within(path, sample_rate, channels, Limits::default())
    }

    /// [`GrowingWav::create`], held to `limits`.
    pub fn create_within(path: &Path, sample_rate: f64, channels: u16, limits: Limits) -> Result<Self, MediaError> {
        let rate = sample_rate.round();
        if !(1.0..=f64::from(u32::MAX / 8)).contains(&rate) {
            return Err(MediaError::write(path, format!("{sample_rate} is not a usable sample rate")));
        }
        if !(1..=2).contains(&channels) {
            return Err(MediaError::write(path, format!("a recording has one channel or two, not {channels}")));
        }
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| MediaError::write(path, error))?;
        }
        let begun = File::create(path).and_then(|mut file| {
            file.write_all(&header(rate as u32, channels, 0))?;
            Ok(file)
        });
        let file = begun.map_err(|error| MediaError::write(path, error))?;
        let per_second = rate as usize * channels as usize * 4;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            limits: Limits { most_bytes: limits.most_bytes.clamp(HEADER_BYTES, MOST_BYTES), ..limits },
            on_disk: HEADER_BYTES,
            channels,
            sample_rate: rate as u32,
            frames: 0,
            said: 0,
            waiting: Vec::with_capacity(per_second + per_second / 4),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many frames it has been handed.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// How many frames the file on disk says it holds, and does: all that
    /// was handed over up to the last time it was put there. What a
    /// reader finds, whatever became of the rest.
    pub fn kept(&self) -> u64 {
        self.said
    }

    /// How many frames it can still take before it is as long as a WAV
    /// file can be.
    pub fn room(&self) -> u64 {
        let block = u64::from(self.channels) * 4;
        ((self.limits.most_bytes - HEADER_BYTES) / block).saturating_sub(self.frames)
    }

    /// Appends frames: `samples` holds every frame's channels side by
    /// side. Frames there is no room for are left out, and the number that
    /// were taken is returned. A second's worth is put on disk as soon as
    /// there is one.
    pub fn write(&mut self, samples: &[f32]) -> Result<u64, MediaError> {
        let channels = usize::from(self.channels);
        let frames = ((samples.len() / channels) as u64).min(self.room());
        for sample in &samples[..frames as usize * channels] {
            let sample = if sample.is_finite() { *sample } else { 0.0 };
            self.waiting.extend_from_slice(&sample.to_le_bytes());
        }
        self.frames += frames;
        if self.frames - self.said >= u64::from(self.sample_rate) {
            self.checkpoint()?;
        }
        Ok(frames)
    }

    /// Appends `frames` frames of silence: what did not arrive, so that
    /// what comes after it is where it belongs.
    pub fn write_silence(&mut self, frames: u64) -> Result<u64, MediaError> {
        let channels = usize::from(self.channels);
        let mut left = frames.min(self.room());
        let taken = left;
        let silence = [0.0f32; 1024];
        while left > 0 {
            let now = (left as usize).min(silence.len() / channels);
            self.write(&silence[..now * channels])?;
            left -= now as u64;
        }
        Ok(taken)
    }

    /// Puts everything handed over so far on disk, and says so in the
    /// header: from here on the file reads as holding all of it. The
    /// audio first and the header after, so that the header never says
    /// more than is there.
    pub fn checkpoint(&mut self) -> Result<(), MediaError> {
        let written = (|| {
            if let Some(at) = self.limits.fails_at
                && self.on_disk + self.waiting.len() as u64 > at
            {
                // As a disk that fills does it: what there was room for
                // is written, and the rest is refused.
                let fits = at.saturating_sub(self.on_disk).min(self.waiting.len() as u64) as usize;
                self.file.write_all(&self.waiting[..fits])?;
                self.waiting.drain(..fits);
                self.on_disk += fits as u64;
                return Err(std::io::Error::from(std::io::ErrorKind::StorageFull));
            }
            self.file.write_all(&self.waiting)?;
            self.on_disk += self.waiting.len() as u64;
            self.waiting.clear();
            let data = self.frames * u64::from(self.channels) * 4;
            self.file.seek(SeekFrom::Start(RIFF_LENGTH_AT))?;
            self.file.write_all(&((HEADER_BYTES - 8 + data) as u32).to_le_bytes())?;
            self.file.seek(SeekFrom::Start(FRAMES_AT))?;
            self.file.write_all(&(self.frames as u32).to_le_bytes())?;
            self.file.seek(SeekFrom::Start(DATA_LENGTH_AT))?;
            self.file.write_all(&(data as u32).to_le_bytes())?;
            self.file.seek(SeekFrom::End(0))?;
            Ok(())
        })();
        written.map_err(|error: std::io::Error| match error.kind() {
            // In words: the system's own differ from one to the next.
            std::io::ErrorKind::StorageFull => MediaError::write(&self.path, "the disk is full"),
            _ => MediaError::write(&self.path, error),
        })?;
        self.said = self.frames;
        Ok(())
    }

    /// Ends the file: all of it is on disk, as far as the system will say.
    /// Returns how many frames it holds.
    pub fn finish(mut self) -> Result<u64, MediaError> {
        self.checkpoint()?;
        self.file.sync_all().map_err(|error| MediaError::write(&self.path, error))?;
        Ok(self.frames)
    }
}

/// The bytes before the samples of a 32-bit float file of `frames` frames.
fn header(sample_rate: u32, channels: u16, frames: u32) -> Vec<u8> {
    let block = u32::from(channels) * 4;
    let data = frames * block;
    let mut bytes = Vec::with_capacity(HEADER_BYTES as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(HEADER_BYTES as u32 - 8 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * block).to_le_bytes());
    bytes.extend_from_slice(&(block as u16).to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    // A file that is not plain integers says its length in frames too.
    bytes.extend_from_slice(b"fact");
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&frames.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    debug_assert_eq!(bytes.len() as u64, HEADER_BYTES);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_back(path: &Path) -> (hound::WavSpec, Vec<f32>) {
        let mut reader = hound::WavReader::open(path).unwrap();
        (reader.spec(), reader.samples::<f32>().map(Result::unwrap).collect())
    }

    fn ramp(frames: usize, channels: usize) -> Vec<f32> {
        (0..frames * channels).map(|n| (n % 2000) as f32 / 1000.0 - 1.0).collect()
    }

    #[test]
    fn a_file_reads_at_every_moment_of_its_making_and_whole_at_the_end() {
        let folder = tempfile::tempdir().unwrap();
        for channels in [1u16, 2] {
            let path = folder.path().join(format!("takes/take {channels}.wav"));
            let mut file = GrowingWav::create(&path, 48_000.0, channels).unwrap();
            let (spec, samples) = read_back(&path);
            assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (channels, 48_000, 32));
            assert!(samples.is_empty(), "there from the first, and of no length");

            let audio = ramp(48_000 * 3 + 777, usize::from(channels));
            let mut handed = 0;
            for size in [100usize, 4096, 48_000, 30_000, 61_000, 1581] {
                let from = handed * usize::from(channels);
                file.write(&audio[from..from + size * usize::from(channels)]).unwrap();
                handed += size;
                // As a program that died here would have left it.
                let copy = folder.path().join("as it was.wav");
                std::fs::copy(&path, &copy).unwrap();
                let (_, there) = read_back(&copy);
                let frames = there.len() / usize::from(channels);
                assert!(handed - frames < 48_000 + 1, "{handed} handed over and {frames} to be read");
                assert_eq!(there[..], audio[..there.len()], "what is there is what was handed over");
            }
            assert_eq!(handed, 48_000 * 3 + 777);
            assert_eq!(file.finish().unwrap(), handed as u64);
            let (_, whole) = read_back(&path);
            assert_eq!(whole, audio);
            // The file's own decoder reads it too, a mono file on both sides.
            let decoded = crate::decode(&path).unwrap();
            assert_eq!(decoded.frames(), handed);
            assert_eq!(decoded.left[1234], audio[1234 * usize::from(channels)]);
        }
    }

    #[test]
    fn silence_is_written_for_what_did_not_arrive_and_nothing_that_is_not_a_number() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("gap.wav");
        let mut file = GrowingWav::create(&path, 44_100.0, 1).unwrap();
        file.write(&[0.5, f32::NAN, f32::INFINITY, -0.5]).unwrap();
        assert_eq!(file.write_silence(3000).unwrap(), 3000);
        file.write(&[0.25]).unwrap();
        assert_eq!(file.finish().unwrap(), 3005);
        let (_, samples) = read_back(&path);
        assert_eq!(samples[..4], [0.5, 0.0, 0.0, -0.5]);
        assert!(samples[4..3004].iter().all(|sample| *sample == 0.0));
        assert_eq!(samples[3004], 0.25);
    }

    #[test]
    fn a_file_says_how_much_more_it_can_hold_and_what_it_cannot_be() {
        let folder = tempfile::tempdir().unwrap();
        let file = GrowingWav::create(&folder.path().join("room.wav"), 48_000.0, 2).unwrap();
        // A little over three hours of stereo at 48 kHz.
        let hours = file.room() as f64 / 48_000.0 / 3600.0;
        assert!((3.0..3.2).contains(&hours), "{hours} hours");
        assert!(GrowingWav::create(&folder.path().join("no.wav"), 0.0, 1).is_err());
        assert!(GrowingWav::create(&folder.path().join("no.wav"), 48_000.0, 3).is_err());
    }
}
