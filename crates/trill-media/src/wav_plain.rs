//! Plain WAV files whose format chunk is longer than plain audio needs.
//!
//! A WAV file of plain samples (PCM or floats) describes them in sixteen
//! bytes. Some programs write that description into a longer chunk: twenty
//! bytes, say, the last four of them nothing. Players at large read the
//! sixteen and pass over the rest; the reader this crate uses refuses the
//! file. Whole sample libraries are written that way, so such a file is
//! shown to the reader as the file it stands for: the same sixteen bytes
//! of description, and the same audio, read from where it lies in the
//! file and not copied.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use symphonia::core::io::MediaSource;

/// Plain samples: whole numbers, or floats.
const PLAIN: [u16; 2] = [1, 3];
/// The lengths of a format chunk the reader takes as they are.
const USUAL: [u64; 3] = [16, 18, 40];
const MAX_CHUNKS: usize = 64;

/// A WAV file as it would be with its description in sixteen bytes: a
/// header made here, then the file's own audio.
pub(crate) struct Plain {
    head: Vec<u8>,
    file: File,
    /// Where the audio is in the file, and how much of it there is.
    start: u64,
    len: u64,
    at: u64,
}

/// Looks at the chunks of `file`. If it is a WAV of plain samples whose
/// format chunk has a length the reader would refuse, returns where its
/// description and its audio are; otherwise `None`, and the file goes to
/// the reader as it is. Leaves the file at its start.
pub(crate) fn odd(file: &mut File) -> io::Result<Option<([u8; 16], u64, u64)>> {
    let found = examine(file).unwrap_or(None);
    file.seek(SeekFrom::Start(0))?;
    Ok(found)
}

fn examine(file: &mut File) -> io::Result<Option<([u8; 16], u64, u64)>> {
    let mut header = [0u8; 12];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut header)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Ok(None);
    }
    let file_len = file.metadata()?.len();
    let mut position = 12u64;
    let mut format = None;
    for _ in 0..MAX_CHUNKS {
        let mut chunk = [0u8; 8];
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut chunk)?;
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        let body = position + 8;
        match &chunk[..4] {
            b"fmt " => {
                if size < 16 || USUAL.contains(&size) {
                    return Ok(None);
                }
                let mut described = [0u8; 16];
                file.read_exact(&mut described)?;
                if !PLAIN.contains(&u16::from_le_bytes([described[0], described[1]])) {
                    return Ok(None);
                }
                format = Some(described);
            }
            b"data" => {
                // A size that runs past the end of the file is a file cut
                // short; what is there is still worth reading.
                let len = size.min(file_len.saturating_sub(body));
                return Ok(format.map(|described| (described, body, len)));
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        position = body + size + (size & 1);
    }
    Ok(None)
}

impl Plain {
    /// `file`, whose description is `described` and whose audio is the
    /// `len` bytes from `start`.
    pub(crate) fn new(file: File, described: [u8; 16], start: u64, len: u64) -> Self {
        // A WAV file says its sizes in 32 bits.
        let len = len.min(u64::from(u32::MAX) - 36);
        let mut head = Vec::with_capacity(44);
        head.extend_from_slice(b"RIFF");
        head.extend_from_slice(&(36 + len as u32).to_le_bytes());
        head.extend_from_slice(b"WAVEfmt ");
        head.extend_from_slice(&16u32.to_le_bytes());
        head.extend_from_slice(&described);
        head.extend_from_slice(b"data");
        head.extend_from_slice(&(len as u32).to_le_bytes());
        Self { head, file, start, len, at: 0 }
    }

    fn whole(&self) -> u64 {
        self.head.len() as u64 + self.len
    }
}

impl Read for Plain {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let head = self.head.len() as u64;
        if self.at < head {
            let from = self.at as usize;
            let count = into.len().min(self.head.len() - from);
            into[..count].copy_from_slice(&self.head[from..from + count]);
            self.at += count as u64;
            return Ok(count);
        }
        let left = self.whole().saturating_sub(self.at);
        if left == 0 || into.is_empty() {
            return Ok(0);
        }
        let count = into.len().min(left.min(usize::MAX as u64) as usize);
        self.file.seek(SeekFrom::Start(self.start + (self.at - head)))?;
        let read = self.file.read(&mut into[..count])?;
        self.at += read as u64;
        Ok(read)
    }
}

impl Seek for Plain {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let wanted = match to {
            SeekFrom::Start(at) => i128::from(at),
            SeekFrom::End(from) => i128::from(self.whole()) + i128::from(from),
            SeekFrom::Current(from) => i128::from(self.at) + i128::from(from),
        };
        if wanted < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a place before the start of the file"));
        }
        self.at = (wanted as u64).min(self.whole());
        Ok(self.at)
    }
}

impl MediaSource for Plain {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.whole())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file of `samples` (16-bit, mono, 48 kHz) whose format chunk
    /// is `format_len` bytes long, with a chunk of notes before its audio.
    fn wav(samples: &[i16], format_len: u32) -> Vec<u8> {
        let mut described = Vec::new();
        described.extend_from_slice(&1u16.to_le_bytes());
        described.extend_from_slice(&1u16.to_le_bytes());
        described.extend_from_slice(&48_000u32.to_le_bytes());
        described.extend_from_slice(&96_000u32.to_le_bytes());
        described.extend_from_slice(&2u16.to_le_bytes());
        described.extend_from_slice(&16u16.to_le_bytes());
        described.resize(format_len as usize, 0);
        let audio: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&format_len.to_le_bytes());
        bytes.extend_from_slice(&described);
        bytes.extend_from_slice(b"note");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"abc\0");
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(audio.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&audio);
        let riff = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff.to_le_bytes());
        bytes
    }

    #[test]
    fn a_plain_wav_with_a_long_format_chunk_is_read_as_the_file_it_stands_for() {
        let folder = tempfile::tempdir().unwrap();
        let samples: Vec<i16> = (0..4_800).map(|n| ((n % 96) as i16 - 48) * 300).collect();
        let odd_file = folder.path().join("twenty.wav");
        std::fs::write(&odd_file, wav(&samples, 20)).unwrap();
        let usual_file = folder.path().join("sixteen.wav");
        std::fs::write(&usual_file, wav(&samples, 16)).unwrap();

        // The usual one goes to the reader as it is; the other is found out.
        assert_eq!(odd(&mut File::open(&usual_file).unwrap()).unwrap(), None);
        let (described, start, len) = odd(&mut File::open(&odd_file).unwrap()).unwrap().unwrap();
        assert_eq!((u16::from_le_bytes([described[2], described[3]]), start, len), (1, 60, 9_600));
        // What the reader is shown is a file of 44 bytes and the audio, wherever it is asked for it.
        let mut plain = Plain::new(File::open(&odd_file).unwrap(), described, start, len);
        assert_eq!(plain.byte_len(), Some(44 + 9_600));
        let mut whole = Vec::new();
        plain.read_to_end(&mut whole).unwrap();
        assert_eq!(&whole[..4], b"RIFF");
        assert_eq!(&whole[36..40], b"data");
        assert_eq!(i16::from_le_bytes([whole[44], whole[45]]), samples[0]);
        assert_eq!(i16::from_le_bytes([whole[44 + 200], whole[45 + 200]]), samples[100]);
        plain.seek(SeekFrom::End(-2)).unwrap();
        let mut last = [0u8; 4];
        assert_eq!(plain.read(&mut last).unwrap(), 2);
        assert_eq!(i16::from_le_bytes([last[0], last[1]]), samples[4_799]);
        assert_eq!(plain.read(&mut last).unwrap(), 0);
        assert!(plain.seek(SeekFrom::Current(-100_000)).is_err());

        // Both decode to the same audio, whole and a piece at a time.
        let (usual, long) = (crate::decode(&usual_file).unwrap(), crate::decode(&odd_file).unwrap());
        assert_eq!(long.left.len(), 4_800);
        assert!(long.left == usual.left && long.sample_rate == 48_000.0);
        use trill_core::audio::AudioStream;
        let mut stream = crate::open(&odd_file).unwrap();
        let (mut left, mut right) = (Vec::new(), Vec::new());
        assert_eq!(stream.read(&mut left, &mut right, 1_000).unwrap(), 1_000);
        assert_eq!((stream.frames(), stream.mono()), (Some(4_800), true));
        assert!(left[..] == usual.left[..1_000]);
        assert_eq!(crate::probe(&odd_file).unwrap().frames, Some(4_800));
    }
}
