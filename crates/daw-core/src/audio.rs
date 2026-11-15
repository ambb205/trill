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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_follows_the_shorter_channel() {
        let buffer = AudioBuffer { sample_rate: 48_000.0, left: vec![0.0; 48_000], right: vec![0.0; 24_000] };
        assert_eq!(buffer.frames(), 24_000);
        assert_eq!(buffer.seconds(), 0.5);
        assert_eq!(AudioBuffer::default().seconds(), 0.0);
    }
}
