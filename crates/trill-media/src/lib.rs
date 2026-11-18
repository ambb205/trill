//! Audio files in and out: decoding, caching, waveform overviews,
//! sample-rate conversion and WAV writing.
//!
//! - [`decode`] reads a whole file (WAV, AIFF, FLAC, MP3, Ogg Vorbis) into
//!   the shared [`AudioBuffer`](trill_core::audio::AudioBuffer); [`probe`]
//!   reads only its header.
//! - [`open`] reads a file a piece at a time, for what wants the start of
//!   a thousand recordings and the rest of only some.
//! - [`MediaCache`] keeps decoded files and their [`Peaks`] in memory within
//!   a budget, and is the [`AudioLoader`](trill_core::audio::AudioLoader) the
//!   engine and devices load through.
//! - [`Peaks`] says what a stretch of audio looks like in a given number of
//!   pixel columns, at any zoom.
//! - [`resample`] converts a buffer to another sample rate, offline and
//!   band-limited.
//! - [`write_wav`] writes a buffer to disk.
//!
//! Everything here runs off the audio thread. Files are untrusted, so no
//! function panics on bad input; each returns a [`MediaError`] that says in
//! plain words what is wrong.

mod cache;
mod decode;
mod error;
mod peaks;
mod resample;
mod stream;
mod wav;
mod wav_plain;
mod wav_vorbis;

pub use cache::{CacheStats, FileStamp, MediaCache};
pub use decode::{AUDIO_EXTENSIONS, AudioInfo, decode, is_audio_file, probe};
pub use error::MediaError;
pub use peaks::{MinMax, PeakColumn, Peaks, scan_columns};
pub use resample::{Resampler, resample};
pub use stream::{AudioFile, open, wav_loop};
pub use wav::{WavFormat, WavWriter, write_wav};
