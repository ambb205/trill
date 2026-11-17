//! Decoded audio and the interface for loading it.
//!
//! The types live here, in the dependency-free core, so that devices that
//! play samples, the engine and the file decoders can share them without
//! depending on each other.

use std::sync::Arc;

/// Decoded stereo audio. Mono sources are duplicated to both channels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioBuffer {
    pub sample_rate: f64,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl AudioBuffer {
    /// Number of frames both channels hold.
    pub fn frames(&self) -> usize {
        self.left.len().min(self.right.len())
    }

    pub fn seconds(&self) -> f64 {
        if self.sample_rate > 0.0 { self.frames() as f64 / self.sample_rate } else { 0.0 }
    }
}

/// Loads audio files by path. Implemented by the media layer (which decodes
/// and caches) and handed to whatever needs samples, so a device never does
/// file I/O or decoding itself.
pub trait AudioLoader: Send + Sync {
    /// Returns the decoded file, or a message saying why it could not be
    /// read. Loading the same path twice should return the same buffer.
    fn load(&self, path: &str) -> Result<Arc<AudioBuffer>, String>;

    /// Opens the file to be read a piece at a time, from its start: for
    /// what plays an instrument of thousands of recordings, which keeps
    /// the beginning of each and reads the rest of one when it is played.
    /// Nothing that is read this way is kept by the loader.
    ///
    /// Without an implementation of its own this loads the whole file and
    /// hands it out in pieces, which is right for a loader that has its
    /// files in memory anyway.
    fn open(&self, path: &str) -> Result<Box<dyn AudioStream>, String> {
        Ok(Box::new(Loaded::new(self.load(path)?)))
    }

    /// How large the file at `path` is, in bytes; `None` if there is no
    /// such file. Costs what asking the file system does: an instrument
    /// asks it of every recording it names, to say which are missing.
    ///
    /// Without an implementation of its own this loads the file to see.
    fn size(&self, path: &str) -> Option<u64> {
        self.load(path).ok().map(|buffer| buffer.frames() as u64 * 8)
    }
}

/// A sustain loop a file carries in itself (the `smpl` chunk of a WAV
/// file): the first and the last frame of the stretch that repeats.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FileLoop {
    pub start: u64,
    /// The last frame of the loop, itself played.
    pub end: u64,
}

/// An audio file being read from its start, a piece at a time.
pub trait AudioStream: Send {
    /// The file's own sample rate.
    fn sample_rate(&self) -> f64;

    /// Whether the file has one channel, or two that are the same: its
    /// right side is then not handed out at all.
    fn mono(&self) -> bool;

    /// How many frames the whole file has, where its header says.
    fn frames(&self) -> Option<u64>;

    /// The loop the file itself carries, if it has one.
    fn file_loop(&self) -> Option<FileLoop> {
        None
    }

    /// Reads up to `frames` more frames on to the end of `left` and, for a
    /// file that is not [`mono`](Self::mono), of `right`. Returns how many
    /// were read: fewer than asked for only at the end of the file, and 0
    /// from then on. An error is a sentence about the file.
    fn read(&mut self, left: &mut Vec<f32>, right: &mut Vec<f32>, frames: usize) -> Result<usize, String>;
}

/// A file that was loaded whole, handed out in pieces.
struct Loaded {
    buffer: Arc<AudioBuffer>,
    mono: bool,
    at: usize,
}

impl Loaded {
    fn new(buffer: Arc<AudioBuffer>) -> Self {
        let frames = buffer.frames();
        let mono = buffer.left[..frames] == buffer.right[..frames];
        Self { buffer, mono, at: 0 }
    }
}

impl AudioStream for Loaded {
    fn sample_rate(&self) -> f64 {
        self.buffer.sample_rate
    }

    fn mono(&self) -> bool {
        self.mono
    }

    fn frames(&self) -> Option<u64> {
        Some(self.buffer.frames() as u64)
    }

    fn read(&mut self, left: &mut Vec<f32>, right: &mut Vec<f32>, frames: usize) -> Result<usize, String> {
        let end = (self.at + frames).min(self.buffer.frames());
        left.extend_from_slice(&self.buffer.left[self.at..end]);
        if !self.mono {
            right.extend_from_slice(&self.buffer.right[self.at..end]);
        }
        let read = end - self.at;
        self.at = end;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loader_that_only_loads_whole_files_still_hands_one_out_in_pieces() {
        struct One(Arc<AudioBuffer>);
        impl AudioLoader for One {
            fn load(&self, path: &str) -> Result<Arc<AudioBuffer>, String> {
                if path == "a.wav" { Ok(self.0.clone()) } else { Err("no such file".into()) }
            }
        }
        let ramp: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let stereo = One(Arc::new(AudioBuffer { sample_rate: 44_100.0, left: ramp.clone(), right: vec![0.5; 10] }));
        let mut stream = stereo.open("a.wav").unwrap();
        assert_eq!((stream.sample_rate(), stream.mono(), stream.frames()), (44_100.0, false, Some(10)));
        assert_eq!(stream.file_loop(), None);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        assert_eq!(stream.read(&mut left, &mut right, 4), Ok(4));
        assert_eq!(stream.read(&mut left, &mut right, 100), Ok(6));
        assert_eq!(stream.read(&mut left, &mut right, 100), Ok(0));
        assert_eq!((left, right), (ramp.clone(), vec![0.5; 10]));
        assert!(stereo.open("b.wav").is_err());
        // Two sides that are the same are one side.
        let mono = One(Arc::new(AudioBuffer { sample_rate: 48_000.0, left: ramp.clone(), right: ramp.clone() }));
        let mut stream = mono.open("a.wav").unwrap();
        let (mut left, mut right) = (Vec::new(), Vec::new());
        assert!(stream.mono());
        assert_eq!(stream.read(&mut left, &mut right, 100), Ok(10));
        assert_eq!((left, right), (ramp, Vec::new()));
    }

    #[test]
    fn length_follows_the_shorter_channel() {
        let buffer = AudioBuffer { sample_rate: 48_000.0, left: vec![0.0; 48_000], right: vec![0.0; 24_000] };
        assert_eq!(buffer.frames(), 24_000);
        assert_eq!(buffer.seconds(), 0.5);
        assert_eq!(AudioBuffer::default().seconds(), 0.0);
    }
}
