//! Reading audio files: [`decode`] for the samples, [`probe`] for the facts
//! in the header.
//!
//! Both go through symphonia, so every container and codec is handled by one
//! code path and a file that probes is a file that decodes.

use crate::MediaError;
use crate::wav_vorbis::{self, Embedded};
use std::fs::File;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use trill_core::audio::AudioBuffer;

/// File extensions (lower case, without the dot) of the formats [`decode`]
/// reads: WAV (PCM, float, ADPCM, and the Ogg Vorbis kind FL Studio ships),
/// AIFF, FLAC, MP3 and Ogg Vorbis.
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "wave", "aif", "aiff", "aifc", "flac", "mp3", "ogg", "oga"];

/// The most frames one file may decode to: four gibibytes of stereo float,
/// a little over three hours at 48 kHz. Compressed files are small on disk
/// and huge in memory; without a ceiling one mislabelled podcast could
/// exhaust memory, which aborts the process instead of returning an error.
const MAX_FRAMES: usize = 1 << 29;

/// Whether `path` has one of the [`AUDIO_EXTENSIONS`], in any letter case.
/// Looks at the name only; the file is not opened.
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| AUDIO_EXTENSIONS.iter().any(|known| known.eq_ignore_ascii_case(extension)))
}

/// What a file's header says about its audio, read without decoding it.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioInfo {
    pub sample_rate: u32,
    /// Channels in the file. [`decode`] always returns two: mono is
    /// duplicated and anything wider keeps its first two.
    pub channels: u16,
    /// Length in frames, when the container states it. Exact for WAV, AIFF,
    /// FLAC and Ogg; for MP3 without a length header it is an estimate from
    /// the file size.
    pub frames: Option<u64>,
    /// Short name of the container: `wave`, `aiff`, `flac`, `mp3`, `ogg`.
    /// A WAV file wrapped around an Ogg Vorbis stream reports `ogg`.
    pub container: &'static str,
    /// Short name of the codec inside it, such as `pcm_s24le` or `vorbis`.
    pub codec: &'static str,
    /// Bits per sample, for formats that have a fixed sample width.
    pub bits_per_sample: Option<u32>,
}

impl AudioInfo {
    /// Length in seconds, when the container states the length.
    pub fn seconds(&self) -> Option<f64> {
        self.frames.map(|frames| frames as f64 / f64::from(self.sample_rate))
    }
}

/// Reads the header of an audio file: length, sample rate, channel count
/// and format. Only the start of the file is read (and its end, for formats
/// that keep the length or tags there), so this is cheap enough to call on
/// every file of a large sample library.
///
/// A file that probes successfully has a codec [`decode`] can read.
pub fn probe(path: &Path) -> Result<AudioInfo, MediaError> {
    guarded(path, || {
        let format = open(path)?;
        let AudioTrack { frames, params, .. } = audio_track(path, format.as_ref())?;
        let sample_rate = params
            .sample_rate
            .filter(|rate| *rate > 0)
            .ok_or_else(|| MediaError::unsupported(path, "it does not state a sample rate"))?;
        let codec = symphonia::default::get_codecs()
            .get_audio_decoder(params.codec)
            .map(|decoder| decoder.codec.info.short_name)
            .ok_or_else(|| MediaError::unsupported(path, "its audio codec is not supported"))?;
        let channels = params.channels.as_ref().map_or(0, |channels| channels.count());
        Ok(AudioInfo {
            sample_rate,
            channels: u16::try_from(channels).unwrap_or(u16::MAX),
            frames,
            container: format.format_info().short_name,
            codec,
            bits_per_sample: params.bits_per_sample.or(params.bits_per_coded_sample),
        })
    })
}

/// Decodes a whole audio file to stereo float at the file's own sample rate.
///
/// WAV, AIFF, FLAC, MP3 and Ogg Vorbis are read, in any sample format and
/// rate. Mono is duplicated to both channels; files with more than two
/// channels keep the first two. Samples that are not finite numbers (which a
/// float file can contain) become silence, because one of them would poison
/// every filter downstream.
///
/// A file that is cut short still decodes as far as it goes: an interrupted
/// recording is worth more than an error message.
pub fn decode(path: &Path) -> Result<AudioBuffer, MediaError> {
    guarded(path, || {
        let mut format = open(path)?;
        let AudioTrack { id: track_id, frames, params } = audio_track(path, format.as_ref())?;
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|error| describe(path, &error))?;

        // The header's length is only a hint: trust it for small files, where
        // reserving saves reallocation, and let large ones grow as they decode.
        let expected = frames.map_or(0, |frames| usize::try_from(frames).unwrap_or(0).min(1 << 24));
        let mut left: Vec<f32> = Vec::with_capacity(expected);
        let mut right: Vec<f32> = Vec::with_capacity(expected);
        let mut planes: Vec<Vec<f32>> = Vec::new();
        let mut sample_rate = params.sample_rate.unwrap_or(0);
        let mut damaged = false;

        loop {
            let packet = match format.next_packet() {
                Ok(Some(packet)) => packet,
                // The end of the file, the end of the first chained stream,
                // or the point where a truncated file stops.
                Ok(None) | Err(Error::ResetRequired) => break,
                Err(error) => {
                    if left.is_empty() {
                        return Err(describe(path, &error));
                    }
                    break;
                }
            };
            if packet.track_id != track_id {
                continue;
            }
            match decoder.decode(&packet) {
                Ok(decoded) => {
                    sample_rate = decoded.spec().rate();
                    decoded.copy_to_vecs_planar(&mut planes);
                    let Some(first) = planes.first() else { continue };
                    let second = planes.get(1).unwrap_or(first);
                    if left.len() + first.len() > MAX_FRAMES {
                        return Err(MediaError::unsupported(path, "it is too long to load into memory"));
                    }
                    left.extend_from_slice(first);
                    right.extend_from_slice(second);
                }
                // One damaged packet is skipped, as symphonia documents; the
                // audio around it is still good.
                Err(Error::DecodeError(_) | Error::IoError(_)) => damaged = true,
                Err(Error::ResetRequired) => break,
                Err(error) => return Err(describe(path, &error)),
            }
        }

        if left.is_empty() {
            return Err(if damaged {
                MediaError::corrupt(path, "none of its audio could be decoded")
            } else {
                MediaError::Empty { path: path.to_path_buf() }
            });
        }
        if sample_rate == 0 {
            return Err(MediaError::unsupported(path, "it does not state a sample rate"));
        }
        silence_non_finite(&mut left);
        silence_non_finite(&mut right);
        // A buffer lives as long as the project uses the file; do not carry
        // the slack that growing left behind.
        left.shrink_to_fit();
        right.shrink_to_fit();
        Ok(AudioBuffer { sample_rate: f64::from(sample_rate), left, right })
    })
}

/// Opens `path` and finds the reader for its container.
fn open(path: &Path) -> Result<Box<dyn FormatReader>, MediaError> {
    let io_error = |error: std::io::Error| MediaError::from_io(path, &error);
    let mut file = File::open(path).map_err(io_error)?;

    // A look at the first bytes settles the cases the readers below would
    // only get wrong more slowly. A library can hold tens of thousands of
    // files that end here, so it is one small read and no more.
    let mut head = [0u8; 64];
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]).map_err(io_error)? {
            0 => break,
            read => filled += read,
        }
    }
    let head = &head[..filled];
    if head.is_empty() {
        return Err(MediaError::corrupt(path, "the file is empty"));
    }
    if is_ableton_protected(head) {
        let reason = "it is in Ableton's protected pack format, which only Ableton Live can open";
        return Err(MediaError::unsupported(path, reason));
    }

    let mut hint = Hint::new();
    let source: Box<dyn MediaSource> = match wav_vorbis::find(&mut file).map_err(io_error)? {
        Embedded::None => {
            if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
                hint.with_extension(extension);
            }
            Box::new(file)
        }
        // A WAV wrapped around an Ogg Vorbis stream: read the stream.
        Embedded::Stream { start, len } => {
            if len > wav_vorbis::MAX_STREAM_BYTES {
                return Err(MediaError::unsupported(path, "it is too long to load into memory"));
            }
            hint.with_extension("ogg");
            Box::new(std::io::Cursor::new(wav_vorbis::read_stream(&mut file, start, len).map_err(io_error)?))
        }
        Embedded::Unsupported => {
            let reason = "it is a WAV file holding Ogg Vorbis audio in a layout this program does not read";
            return Err(MediaError::unsupported(path, reason));
        }
    };
    let stream = MediaSourceStream::new(source, Default::default());
    symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|error| describe(path, &error))
}

/// The track of a file that gets decoded.
struct AudioTrack {
    id: u32,
    /// Length in frames, when the container states it.
    frames: Option<u64>,
    params: AudioCodecParameters,
}

fn audio_track(path: &Path, format: &dyn FormatReader) -> Result<AudioTrack, MediaError> {
    let track =
        format.default_track(TrackType::Audio).ok_or_else(|| MediaError::unsupported(path, "it has no audio track"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(CodecParameters::audio)
        .ok_or_else(|| MediaError::unsupported(path, "its audio track cannot be identified"))?;
    Ok(AudioTrack { id: track.id, frames: track.num_frames, params: params.clone() })
}

/// Turns a symphonia error into ours, in words that do not need a codec
/// engineer to read.
fn describe(path: &Path, error: &Error) -> MediaError {
    match error {
        Error::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            MediaError::corrupt(path, "the file ends before its audio does")
        }
        Error::IoError(io) => MediaError::from_io(path, io),
        Error::DecodeError(detail) => MediaError::corrupt(path, *detail),
        Error::LimitError(detail) => MediaError::corrupt(path, format!("a size in it is implausibly large ({detail})")),
        Error::Unsupported(detail) => MediaError::unsupported(path, unsupported_reason(detail)),
        other => MediaError::unsupported(path, other.to_string()),
    }
}

/// Why a file symphonia would not open, in better words than the library's
/// for the case every sample folder has.
fn unsupported_reason(detail: &str) -> &str {
    if detail.contains("no suitable format reader") {
        // Typically a file with an audio extension that holds something
        // else, such as the `._name.wav` files macOS leaves in archives.
        "it is not a WAV, AIFF, FLAC, MP3 or Ogg Vorbis file, whatever its name says"
    } else {
        detail
    }
}

/// Whether a file that starts with `head` is one of the encoded AIFF-C
/// files Ableton's factory packs are made of. They carry an `able` chunk
/// right after the header, and use `able` as their compression type.
fn is_ableton_protected(head: &[u8]) -> bool {
    head.get(8..12) == Some(b"AIFC") && head.windows(4).any(|window| window == b"able")
}

/// Runs `work`, turning a panic inside the decoder into an error.
///
/// Decoders are large and fed arbitrary bytes. A bug in one must cost the
/// user that file, not the session, so a panic is reported like any other
/// damaged file.
fn guarded<T>(path: &Path, work: impl FnOnce() -> Result<T, MediaError>) -> Result<T, MediaError> {
    catch_unwind(AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err(MediaError::corrupt(path, "the decoder could not make sense of it")))
}

fn silence_non_finite(samples: &mut [f32]) {
    for sample in samples {
        if !sample.is_finite() {
            *sample = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_files_are_recognised_by_extension_in_any_case() {
        assert!(is_audio_file(Path::new("Kick 01.WAV")));
        assert!(is_audio_file(Path::new("pads/Warm.Aiff")));
        assert!(is_audio_file(Path::new("a.b.flac")));
        assert!(!is_audio_file(Path::new("notes.txt")));
        assert!(!is_audio_file(Path::new("wav")));
        assert!(!is_audio_file(Path::new("Kick.wav.asd")));
    }

    #[test]
    fn ableton_pack_samples_are_known_by_their_first_bytes() {
        // The start of a sample from a Live factory pack.
        let protected =
            b"FORM\x00\x05\x1f\x57AIFCFVER\x00\x00\x00\x04\xa2\x80\x51\x40able\x00\x00\x01\x54\x00\x00\x00\x00";
        assert!(is_ableton_protected(protected));
        // Ordinary AIFF and AIFF-C, a word that merely occurs later, and
        // files too short to say.
        assert!(!is_ableton_protected(b"FORM\x00\x00\x10\x00AIFFCOMM\x00\x00\x00\x12able"));
        assert!(!is_ableton_protected(
            b"FORM\x00\x00\x10\x00AIFCFVER\x00\x00\x00\x04\xa2\x80\x51\x40COMM\x00\x00\x00\x18"
        ));
        assert!(!is_ableton_protected(b"RIFF\x00\x00\x10\x00WAVEfmt able"));
        assert!(!is_ableton_protected(b"FORM"));
        assert!(!is_ableton_protected(b""));
    }

    #[test]
    fn non_finite_samples_become_silence() {
        let mut samples = [0.5, f32::NAN, -0.25, f32::INFINITY, f32::NEG_INFINITY];
        silence_non_finite(&mut samples);
        assert_eq!(samples, [0.5, 0.0, -0.25, 0.0, 0.0]);
    }
}
