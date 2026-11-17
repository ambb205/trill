//! Reading an audio file a piece at a time: [`open`].
//!
//! [`decode`](crate::decode) reads a whole file, which is right for a clip
//! of a song. An instrument made of a thousand recordings wants the first
//! third of a second of each and the rest of one only when its note is
//! played, so this reads as far as it is asked and no further. It goes
//! through the same readers, and hands out the same samples `decode` does.
//!
//! A WAV file may carry a sustain loop in a `smpl` chunk, which is where
//! most sampled instruments keep theirs: [`wav_loop`] reads it.

use crate::MediaError;
use crate::decode::{AudioTrack, audio_track, describe, guarded, open as open_format, silence_non_finite};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatReader;
use trill_core::audio::{AudioStream, FileLoop};

/// Chunks of a WAV file looked at for its loop before giving up.
const MAX_CHUNKS: usize = 64;

/// An audio file that is being read from its start.
pub struct AudioFile {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    sample_rate: f64,
    channels: usize,
    frames: Option<u64>,
    file_loop: Option<FileLoop>,
    /// What was decoded and not yet handed out.
    planes: Vec<Vec<f32>>,
    at: usize,
    ended: bool,
}

/// Opens the audio file at `path` to be read a piece at a time. Reads its
/// header, and for a WAV file the chunk that says where it loops.
pub fn open(path: &Path) -> Result<AudioFile, MediaError> {
    guarded(path, || {
        let format = open_format(path)?;
        let AudioTrack { id, frames, params } = audio_track(path, format.as_ref())?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|error| describe(path, &error))?;
        let sample_rate = params
            .sample_rate
            .filter(|rate| *rate > 0)
            .ok_or_else(|| MediaError::unsupported(path, "it does not state a sample rate"))?;
        let channels = params.channels.as_ref().map_or(0, |channels| channels.count());
        Ok(AudioFile {
            path: path.to_path_buf(),
            format,
            decoder,
            track: id,
            sample_rate: f64::from(sample_rate),
            channels,
            frames,
            file_loop: wav_loop(path),
            planes: Vec::new(),
            at: 0,
            ended: false,
        })
    })
}

impl AudioFile {
    /// Decodes the next packet into `planes`. False at the end of the file.
    fn refill(&mut self) -> Result<bool, MediaError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) | Err(Error::ResetRequired) => return Ok(false),
                // A file that is cut short is read as far as it goes.
                Err(_) => return Ok(false),
            };
            if packet.track_id != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    decoded.copy_to_vecs_planar(&mut self.planes);
                    self.at = 0;
                    if self.planes.first().is_some_and(|first| !first.is_empty()) {
                        for plane in &mut self.planes {
                            silence_non_finite(plane);
                        }
                        return Ok(true);
                    }
                }
                Err(Error::DecodeError(_) | Error::IoError(_)) => {}
                Err(Error::ResetRequired) => return Ok(false),
                Err(error) => return Err(describe(&self.path, &error)),
            }
        }
    }

    fn read_some(&mut self, left: &mut Vec<f32>, right: &mut Vec<f32>, frames: usize) -> Result<usize, MediaError> {
        let mut read = 0;
        while read < frames && !self.ended {
            let have = self.planes.first().map_or(0, Vec::len);
            if self.at >= have {
                if !self.refill()? {
                    self.ended = true;
                }
                continue;
            }
            let take = (frames - read).min(have - self.at);
            let span = self.at..self.at + take;
            left.extend_from_slice(&self.planes[0][span.clone()]);
            if self.channels > 1 {
                let second = self.planes.get(1).unwrap_or(&self.planes[0]);
                right.extend_from_slice(&second[span]);
            }
            self.at += take;
            read += take;
        }
        Ok(read)
    }
}

impl AudioStream for AudioFile {
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn mono(&self) -> bool {
        self.channels <= 1
    }

    fn frames(&self) -> Option<u64> {
        self.frames
    }

    fn file_loop(&self) -> Option<FileLoop> {
        self.file_loop
    }

    fn read(&mut self, left: &mut Vec<f32>, right: &mut Vec<f32>, frames: usize) -> Result<usize, String> {
        let path = self.path.clone();
        let (mut out_left, mut out_right) = (std::mem::take(left), std::mem::take(right));
        let read = guarded(&path, || self.read_some(&mut out_left, &mut out_right, frames));
        (*left, *right) = (out_left, out_right);
        read.map_err(|error| error.to_string())
    }
}

/// The sustain loop the WAV file at `path` carries in its `smpl` chunk:
/// the first of its loops that plays forward. `None` for a file without
/// one, for a file that is no WAV, and for a chunk that makes no sense.
pub fn wav_loop(path: &Path) -> Option<FileLoop> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).ok()?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return None;
    }
    let mut position = 12u64;
    for _ in 0..MAX_CHUNKS {
        let mut chunk = [0u8; 8];
        file.seek(SeekFrom::Start(position)).ok()?;
        file.read_exact(&mut chunk).ok()?;
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        let body = position + 8;
        if &chunk[..4] == b"smpl" {
            // Nine fields of the sampler itself, then its loops: each an
            // id, a type, a start and an end, a fraction and a count.
            let mut smpl = vec![0u8; usize::try_from(size.min(36 + 24 * 8)).ok()?];
            file.read_exact(&mut smpl).ok()?;
            let word = |at: usize| smpl.get(at..at + 4).map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()));
            let loops = word(28)? as usize;
            return (0..loops.min(8)).find_map(|index| {
                let at = 36 + index * 24;
                let (kind, start, end) = (word(at + 4)?, word(at + 8)?, word(at + 12)?);
                (kind == 0 && end > start).then_some(FileLoop { start: u64::from(start), end: u64::from(end) })
            });
        }
        position = body + size + (size & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WavFormat, decode, write_wav};
    use trill_core::audio::AudioBuffer;

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).map(|i| ((i % 2_000) as f32 / 2_000.0) - 0.5).collect()
    }

    #[test]
    fn a_file_read_in_pieces_is_the_file_read_whole() {
        let dir = tempfile::tempdir().unwrap();
        let left = ramp(50_000);
        let right: Vec<f32> = left.iter().map(|sample| -sample * 0.5).collect();
        let stereo = dir.path().join("stereo.wav");
        write_wav(&stereo, &AudioBuffer { sample_rate: 44_100.0, left: left.clone(), right }, WavFormat::Int24)
            .unwrap();
        let whole = decode(&stereo).unwrap();

        let mut file = open(&stereo).unwrap();
        assert_eq!((file.sample_rate(), file.mono(), file.frames()), (44_100.0, false, Some(50_000)));
        let (mut got_left, mut got_right) = (Vec::new(), Vec::new());
        // An awkward size, so that pieces end inside the reader's own.
        let mut pieces = 0;
        while file.read(&mut got_left, &mut got_right, 7_001).unwrap() > 0 {
            pieces += 1;
        }
        assert_eq!(pieces, 8);
        assert_eq!((got_left, got_right), (whole.left, whole.right));
        assert_eq!(file.read(&mut Vec::new(), &mut Vec::new(), 10), Ok(0), "the end stays the end");

        // A file of one channel hands out one.
        let mono = dir.path().join("mono.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&mono, spec).unwrap();
        for sample in &left[..1_000] {
            writer.write_sample((sample * 32_767.0) as i16).unwrap();
        }
        writer.finalize().unwrap();
        let mut file = open(&mono).unwrap();
        let (mut got_left, mut got_right) = (Vec::new(), Vec::new());
        assert!(file.mono());
        assert_eq!(file.read(&mut got_left, &mut got_right, 400), Ok(400));
        assert_eq!((got_left.len(), got_right.len()), (400, 0));
        assert_eq!(got_left, decode(&mono).unwrap().left[..400]);

        assert!(open(&dir.path().join("none.wav")).is_err());
    }

    /// A 16-bit mono WAV of `frames` frames with a `smpl` chunk after its
    /// audio that names `loops` (type, start, end).
    fn wav_with_loops(path: &Path, frames: u32, loops: &[(u32, u32, u32)]) {
        let data: Vec<u8> = (0..frames).flat_map(|i| ((i % 100) as i16 * 100).to_le_bytes()).collect();
        let mut smpl: Vec<u8> = Vec::new();
        for field in [0u32, 0, 20_833, 60, 0, 0, 0, loops.len() as u32, 0] {
            smpl.extend(field.to_le_bytes());
        }
        for (index, (kind, start, end)) in loops.iter().enumerate() {
            for field in [index as u32, *kind, *start, *end, 0, 0] {
                smpl.extend(field.to_le_bytes());
            }
        }
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((4 + 24 + 8 + data.len() as u32 + 8 + smpl.len() as u32).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        for field in [1u16, 1] {
            bytes.extend(field.to_le_bytes());
        }
        bytes.extend(48_000u32.to_le_bytes());
        bytes.extend(96_000u32.to_le_bytes());
        for field in [2u16, 16] {
            bytes.extend(field.to_le_bytes());
        }
        bytes.extend(b"data");
        bytes.extend((data.len() as u32).to_le_bytes());
        bytes.extend(&data);
        bytes.extend(b"smpl");
        bytes.extend((smpl.len() as u32).to_le_bytes());
        bytes.extend(&smpl);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn the_loop_a_wav_file_carries_is_read_from_its_sampler_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let looped = dir.path().join("looped.wav");
        wav_with_loops(&looped, 4_000, &[(0, 1_200, 3_599)]);
        assert_eq!(wav_loop(&looped), Some(FileLoop { start: 1_200, end: 3_599 }));
        // The file still reads as audio, and says its loop when opened.
        let file = open(&looped).unwrap();
        assert_eq!((file.frames(), file.file_loop()), (Some(4_000), Some(FileLoop { start: 1_200, end: 3_599 })));

        // A loop that plays back and forth is not one this is for; the
        // forward one after it is.
        let two = dir.path().join("two.wav");
        wav_with_loops(&two, 4_000, &[(1, 100, 200), (0, 300, 400)]);
        assert_eq!(wav_loop(&two), Some(FileLoop { start: 300, end: 400 }));

        let none = dir.path().join("none.wav");
        wav_with_loops(&none, 4_000, &[]);
        assert_eq!(wav_loop(&none), None);
        let backwards = dir.path().join("backwards.wav");
        wav_with_loops(&backwards, 4_000, &[(0, 500, 500)]);
        assert_eq!(wav_loop(&backwards), None);
        std::fs::write(dir.path().join("text.wav"), b"not a wave file at all").unwrap();
        assert_eq!(wav_loop(&dir.path().join("text.wav")), None);
        assert_eq!(wav_loop(&dir.path().join("absent.wav")), None);
    }
}
