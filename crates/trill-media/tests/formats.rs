//! Decoding and probing checked against files with known content.
//!
//! WAV and AIFF are generated here, so they run everywhere. FLAC, MP3 and
//! Ogg Vorbis need an encoder: when `ffmpeg` is on the path they are made
//! from a known signal and checked the same way, and otherwise those tests
//! say so and pass. A last test decodes real files found in well-known
//! sample folders of the machine, if there are any.

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use std::process::Command;
use trill_core::audio::AudioBuffer;
use trill_media::{MediaError, decode, is_audio_file, probe};

const LEFT_HZ: f64 = 440.0;
const LEFT_LEVEL: f64 = 0.5;
const RIGHT_HZ: f64 = 880.0;
const RIGHT_LEVEL: f64 = 0.25;

/// Two different tones, so that swapped or merged channels show up.
fn signal(rate: u32, frames: usize) -> (Vec<f64>, Vec<f64>) {
    let tone = |hz: f64, level: f64| {
        (0..frames).map(|n| level * (TAU * hz * n as f64 / f64::from(rate)).sin()).collect::<Vec<f64>>()
    };
    (tone(LEFT_HZ, LEFT_LEVEL), tone(RIGHT_HZ, RIGHT_LEVEL))
}

/// The amplitude of the `hz` component of `samples` (by least squares) and
/// the level of everything else relative to it, in decibels.
fn tone_and_rest(samples: &[f32], rate: f64, hz: f64) -> (f64, f64) {
    let phase = |n: usize| TAU * hz * n as f64 / rate;
    let (mut ss, mut cc, mut sc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (n, &x) in samples.iter().enumerate() {
        let (s, c) = phase(n).sin_cos();
        ss += s * s;
        cc += c * c;
        sc += s * c;
        xs += f64::from(x) * s;
        xc += f64::from(x) * c;
    }
    let determinant = ss * cc - sc * sc;
    let (a, b) = ((xs * cc - xc * sc) / determinant, (xc * ss - xs * sc) / determinant);
    let rest: f64 = samples
        .iter()
        .enumerate()
        .map(|(n, &x)| {
            let (s, c) = phase(n).sin_cos();
            (f64::from(x) - a * s - b * c).powi(2)
        })
        .sum();
    let amplitude = (a * a + b * b).sqrt();
    let rest_rms = (rest / samples.len() as f64).sqrt();
    (amplitude, 20.0 * (rest_rms / (amplitude / 2f64.sqrt())).max(1.0e-12).log10())
}

/// Asserts that `audio` is the two-tone test signal: the right tones at the
/// right levels in the right channels, and nothing else above `rest_db`.
fn assert_is_the_signal(audio: &AudioBuffer, level_tolerance: f64, rest_db: f64, context: &str) {
    for (channel, hz, level) in [(&audio.left, LEFT_HZ, LEFT_LEVEL), (&audio.right, RIGHT_HZ, RIGHT_LEVEL)] {
        // Lossy codecs smear the very start and end; judge the middle.
        let middle = &channel[channel.len() / 10..channel.len() * 9 / 10];
        let (amplitude, rest) = tone_and_rest(middle, audio.sample_rate, hz);
        assert!(
            (amplitude / level - 1.0).abs() < level_tolerance,
            "{context}: the {hz} Hz tone came back at {amplitude:.5}, not {level}"
        );
        assert!(rest < rest_db, "{context}: {rest:.1} dB of something other than the {hz} Hz tone");
    }
}

fn write_wav(path: &Path, rate: u32, channels: u16, bits: u16, float: bool, frames: usize) -> Vec<Vec<f32>> {
    let (left, right) = signal(rate, frames);
    let format = if float { hound::SampleFormat::Float } else { hound::SampleFormat::Int };
    let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: bits, sample_format: format };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    let full_scale = ((1i64 << (bits - 1)) - 1) as f64;
    // What a decoder should return for each channel: the integer that was
    // written, over the format's full range.
    let mut expected = vec![Vec::with_capacity(frames); channels as usize];
    for frame in 0..frames {
        for (channel, expected) in expected.iter_mut().enumerate() {
            // Channels beyond the second carry a constant, which must not
            // leak into the stereo result.
            let value = match channel {
                0 => left[frame],
                1 => right[frame],
                _ => 0.9,
            };
            if float {
                writer.write_sample(value as f32).unwrap();
                expected.push(value as f32);
            } else {
                let integer = (value * full_scale).round() as i32;
                writer.write_sample(integer).unwrap();
                expected.push((f64::from(integer) / (full_scale + 1.0)) as f32);
            }
        }
    }
    writer.finalize().unwrap();
    expected
}

/// A minimal AIFF writer: big-endian integer PCM.
fn write_aiff(path: &Path, rate: u32, channels: u16, bits: u16, frames: usize) -> Vec<Vec<f32>> {
    let (left, right) = signal(rate, frames);
    let bytes_per_sample = usize::from(bits / 8);
    let full_scale = ((1i64 << (bits - 1)) - 1) as f64;
    let mut expected = vec![Vec::with_capacity(frames); channels as usize];
    let mut sound = Vec::with_capacity(frames * channels as usize * bytes_per_sample);
    for frame in 0..frames {
        for (channel, expected) in expected.iter_mut().enumerate() {
            let value = if channel == 0 { left[frame] } else { right[frame] };
            let integer = (value * full_scale).round() as i32;
            sound.extend_from_slice(&integer.to_be_bytes()[4 - bytes_per_sample..]);
            expected.push((f64::from(integer) / (full_scale + 1.0)) as f32);
        }
    }
    // The sample rate is an 80-bit extended float.
    let exponent = 31 - rate.leading_zeros();
    let mut extended = [0u8; 10];
    extended[..2].copy_from_slice(&((16_383 + exponent) as u16).to_be_bytes());
    extended[2..].copy_from_slice(&(u64::from(rate) << (63 - exponent)).to_be_bytes());

    let mut file = Vec::new();
    file.extend_from_slice(b"FORM");
    file.extend_from_slice(&((4 + 8 + 18 + 8 + 8 + sound.len()) as u32).to_be_bytes());
    file.extend_from_slice(b"AIFFCOMM");
    file.extend_from_slice(&18u32.to_be_bytes());
    file.extend_from_slice(&channels.to_be_bytes());
    file.extend_from_slice(&(frames as u32).to_be_bytes());
    file.extend_from_slice(&bits.to_be_bytes());
    file.extend_from_slice(&extended);
    file.extend_from_slice(b"SSND");
    file.extend_from_slice(&((8 + sound.len()) as u32).to_be_bytes());
    file.extend_from_slice(&[0; 8]);
    file.extend_from_slice(&sound);
    std::fs::write(path, file).unwrap();
    expected
}

/// Checks a decoded file sample by sample against what was written.
fn assert_exact(path: &Path, rate: u32, expected: &[Vec<f32>], tolerance: f32) -> AudioBuffer {
    let context = path.file_name().unwrap().to_string_lossy().into_owned();
    let audio = decode(path).unwrap_or_else(|error| panic!("{context}: {error}"));
    let frames = expected[0].len();
    assert_eq!(audio.sample_rate, f64::from(rate), "{context}: sample rate");
    assert_eq!((audio.left.len(), audio.right.len()), (frames, frames), "{context}: length");
    let second = expected.get(1).unwrap_or(&expected[0]);
    for frame in 0..frames {
        assert!((audio.left[frame] - expected[0][frame]).abs() <= tolerance, "{context}: left frame {frame}");
        assert!((audio.right[frame] - second[frame]).abs() <= tolerance, "{context}: right frame {frame}");
    }

    let info = probe(path).unwrap_or_else(|error| panic!("{context}: {error}"));
    assert_eq!(info.sample_rate, rate, "{context}: probed rate");
    assert_eq!(usize::from(info.channels), expected.len(), "{context}: probed channels");
    assert_eq!(info.frames, Some(frames as u64), "{context}: probed length");
    assert!((info.seconds().unwrap() - audio.seconds()).abs() < 1.0e-9, "{context}: probed duration");
    audio
}

#[test]
fn wav_in_every_sample_format_decodes_to_the_samples_written() {
    let folder = tempfile::tempdir().unwrap();
    // (rate, channels, bits, float)
    let formats = [
        (44_100, 2, 16, false),
        (48_000, 2, 24, false),
        (96_000, 2, 32, false),
        (44_100, 2, 32, true),
        (22_050, 2, 8, false),
        (8_000, 1, 16, false),
        (192_000, 1, 24, false),
        (48_000, 1, 32, true),
        (44_100, 6, 16, false),
        (48_000, 4, 32, true),
    ];
    for (rate, channels, bits, float) in formats {
        let name = format!("{rate}-{channels}ch-{bits}{}.wav", if float { "f" } else { "i" });
        let path = folder.path().join(name);
        let frames = rate as usize / 2;
        let expected = write_wav(&path, rate, channels, bits, float, frames);
        let audio = assert_exact(&path, rate, &expected, 1.0e-7);

        let info = probe(&path).unwrap();
        assert_eq!((info.container, info.bits_per_sample), ("wave", Some(u32::from(bits))));
        assert!(info.codec.starts_with("pcm_"), "{}", info.codec);
        if channels == 1 {
            assert_eq!(audio.left, audio.right, "mono is duplicated to both channels");
        }
        // Not just the right numbers: the right sound. Eight bits leave
        // audible quantisation noise; the others must be clean.
        if channels >= 2 {
            assert_is_the_signal(&audio, 0.01, if bits == 8 { -35.0 } else { -85.0 }, &format!("{rate} Hz {bits}-bit"));
        } else {
            let (amplitude, _) = tone_and_rest(&audio.left, f64::from(rate), LEFT_HZ);
            assert!((amplitude / LEFT_LEVEL - 1.0).abs() < 0.01);
        }
    }
}

#[test]
fn aiff_decodes_to_the_samples_written() {
    let folder = tempfile::tempdir().unwrap();
    for (rate, channels, bits) in [(44_100, 2, 16), (48_000, 2, 24), (44_100, 1, 16), (96_000, 2, 32), (22_050, 1, 8)] {
        for extension in ["aif", "aiff"] {
            let path = folder.path().join(format!("{rate}-{channels}ch-{bits}.{extension}"));
            let expected = write_aiff(&path, rate, channels, bits, rate as usize / 4);
            let audio = assert_exact(&path, rate, &expected, 1.0e-7);
            assert_eq!(probe(&path).unwrap().container, "aiff");
            if channels == 2 && bits > 8 {
                assert_is_the_signal(&audio, 0.01, -85.0, &format!("AIFF {rate} Hz {bits}-bit"));
            }
        }
    }
}

/// Converts `source` to `target` with ffmpeg; `false` if ffmpeg is missing
/// or cannot produce that format.
fn ffmpeg(source: &Path, target: &Path, options: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(source)
        .args(options)
        .arg(target)
        .output()
        .is_ok_and(|output| output.status.success() && target.exists())
}

/// What ffmpeg's own decoder makes of `encoded`: an independent reference
/// for the lossy formats, where the original samples are not what a correct
/// decoder returns.
fn reference_decode(encoded: &Path) -> Option<(Vec<f32>, Vec<f32>)> {
    let target = encoded.with_extension("reference.wav");
    if !ffmpeg(encoded, &target, &["-c:a", "pcm_f32le"]) {
        return None;
    }
    let mut reader = hound::WavReader::open(&target).ok()?;
    let channels = usize::from(reader.spec().channels);
    let samples: Vec<f32> = reader.samples::<f32>().collect::<Result<_, _>>().ok()?;
    let channel = |index: usize| samples.iter().skip(index.min(channels - 1)).step_by(channels).copied().collect();
    Some((channel(0), channel(1)))
}

/// How far `ours` is from `reference`, in decibels relative to the level of
/// `reference`.
fn difference_db(ours: &[f32], reference: &[f32]) -> f64 {
    let power = |samples: &mut dyn Iterator<Item = f64>| samples.map(|x| x * x).sum::<f64>();
    let difference = power(&mut ours.iter().zip(reference).map(|(a, b)| f64::from(a - b)));
    let signal = power(&mut reference.iter().map(|&x| f64::from(x)));
    10.0 * (difference / signal).max(1.0e-20).log10()
}

#[test]
fn flac_mp3_and_ogg_decode_to_the_signal_that_was_encoded() {
    let folder = tempfile::tempdir().unwrap();
    let rate = 44_100;
    let frames = rate as usize * 2;
    let source = folder.path().join("source.wav");
    let expected = write_wav(&source, rate, 2, 16, false, frames);
    let source_24 = folder.path().join("source24.wav");
    let expected_24 = write_wav(&source_24, 48_000, 2, 24, false, 48_000);

    let mut verified = Vec::new();
    let mut skipped = Vec::new();

    // Lossless: every sample must match. `true` encodes the 24-bit source.
    let lossless: [(&str, bool, &[&str], &str); 4] = [
        ("flac16.flac", false, &["-c:a", "flac"], "flac"),
        ("flac24.flac", true, &["-c:a", "flac", "-sample_fmt", "s32"], "flac"),
        ("pcm24.aiff", true, &["-c:a", "pcm_s24be"], "aiff"),
        ("pcm16.aif", false, &["-c:a", "pcm_s16be"], "aiff"),
    ];
    for (name, deep, options, container) in lossless {
        let (source, rate, expected) =
            if deep { (&source_24, 48_000, &expected_24) } else { (&source, rate, &expected) };
        let path = folder.path().join(name);
        if !ffmpeg(source, &path, options) {
            skipped.push(name);
            continue;
        }
        assert_exact(&path, rate, expected, 1.0e-7);
        assert_eq!(probe(&path).unwrap().container, container);
        verified.push(name);
    }

    // Lossy: the same tones in the same channels with only codec noise
    // beside them, exactly the length that was encoded (the encoder's
    // padding trimmed), and sample for sample what ffmpeg's decoder makes
    // of the same file. The level is allowed 5 %: encoders rescale (LAME
    // writes constant-bitrate files at 0.97, which ffmpeg's decoder shows
    // too), so the reference decode is the exact check and this the sanity
    // check.
    let lossy: [(&str, &[&str], &str); 4] = [
        ("cbr.mp3", &["-c:a", "libmp3lame", "-b:a", "192k"], "mp3"),
        ("vbr.mp3", &["-c:a", "libmp3lame", "-q:a", "2"], "mp3"),
        ("vorbis.ogg", &["-c:a", "libvorbis", "-q:a", "6"], "ogg"),
        ("vorbis-low.ogg", &["-c:a", "libvorbis", "-q:a", "2"], "ogg"),
    ];
    for (name, options, container) in lossy {
        let path = folder.path().join(name);
        if !ffmpeg(&source, &path, options) {
            skipped.push(name);
            continue;
        }
        let audio = decode(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(audio.sample_rate, f64::from(rate), "{name}");
        assert_eq!(audio.frames(), frames, "{name}: length");
        assert_is_the_signal(&audio, 0.05, -30.0, name);

        let (left, right) = reference_decode(&path).unwrap_or_else(|| panic!("{name}: ffmpeg could not decode it"));
        assert_eq!(left.len(), frames, "{name}: the reference decoder's length");
        let (left_db, right_db) = (difference_db(&audio.left, &left), difference_db(&audio.right, &right));
        println!("{name}: differs from ffmpeg's decode by {left_db:.1} dB (left), {right_db:.1} dB (right)");
        assert!(left_db < -60.0 && right_db < -60.0, "{name}: {left_db:.1} / {right_db:.1} dB from the reference");

        let info = probe(&path).unwrap();
        assert_eq!((info.container, info.sample_rate, info.channels), (container, rate, 2), "{name}");
        assert_eq!(info.frames, Some(frames as u64), "{name}: probed length");
        verified.push(name);
    }

    // Mono, another rate, and a tag block in front of the audio.
    let mono = folder.path().join("mono.mp3");
    let tagged = ["-ac", "1", "-ar", "48000", "-c:a", "libmp3lame", "-b:a", "128k", "-metadata", "title=Test Tone"];
    if ffmpeg(&source, &mono, &tagged) {
        let audio = decode(&mono).unwrap();
        assert_eq!(audio.sample_rate, 48_000.0);
        assert_eq!(audio.left, audio.right, "mono is duplicated to both channels");
        assert_eq!(probe(&mono).unwrap().channels, 1);
        verified.push("mono.mp3");
    } else {
        skipped.push("mono.mp3");
    }

    // FL Studio's kind of WAV: an Ogg Vorbis stream as the data chunk. It
    // must decode to exactly what the stream decodes to on its own.
    let ogg = folder.path().join("vorbis.ogg");
    if ogg.exists() {
        let stream = std::fs::read(&ogg).unwrap();
        let wrapped = folder.path().join("vorbis in a.wav");
        std::fs::write(&wrapped, wav_around_vorbis(0x674F, &stream, rate)).unwrap();
        assert_eq!(decode(&wrapped).unwrap(), decode(&ogg).unwrap());
        let (info, plain) = (probe(&wrapped).unwrap(), probe(&ogg).unwrap());
        assert_eq!(info, plain);
        assert_eq!((info.frames, info.codec), (Some(frames as u64), "vorbis"));
        verified.push("vorbis in a.wav");

        // FL Studio puts an empty page of another stream in front of the
        // last page of many samples. The sound must not end there.
        let last_page = stream.windows(4).rposition(|window| window == b"OggS").unwrap();
        let mut with_stray = stream[..last_page].to_vec();
        with_stray.extend_from_slice(&stray_page());
        with_stray.extend_from_slice(&stream[last_page..]);
        std::fs::write(&wrapped, wav_around_vorbis(0x674F, &with_stray, rate)).unwrap();
        assert_eq!(decode(&wrapped).unwrap().frames(), frames, "the tail after the stray page");
        assert_eq!(decode(&wrapped).unwrap(), decode(&ogg).unwrap());
        assert_eq!(probe(&wrapped).unwrap().frames, Some(frames as u64));
        verified.push("vorbis in a.wav, with a stray page");

        // The layouts that keep the Vorbis headers elsewhere are refused in
        // so many words rather than misread.
        std::fs::write(&wrapped, wav_around_vorbis(0x6750, &stream, rate)).unwrap();
        let error = decode(&wrapped).unwrap_err();
        assert!(matches!(error, MediaError::Unsupported { .. }), "{error}");
        assert!(error.to_string().contains("holding Ogg Vorbis audio in a layout"), "{error}");
        assert_eq!(probe(&wrapped).unwrap_err(), error);
    } else {
        skipped.push("vorbis in a.wav");
    }

    println!("verified against ffmpeg-encoded files: {verified:?}");
    if !skipped.is_empty() {
        println!("SKIPPED (ffmpeg missing or lacking the encoder): {skipped:?}");
    }
}

/// An empty Ogg page of a stream numbered FFFFFFFF, marked as both the
/// first and the last page of it, with a correct checksum: what FL Studio
/// writes into its samples.
fn stray_page() -> Vec<u8> {
    let mut page = b"OggS\x00\x06".to_vec();
    page.extend_from_slice(&[0; 8]);
    page.extend_from_slice(&[0xFF; 4]);
    page.extend_from_slice(&[0; 9]);
    // Ogg's checksum: CRC-32 with polynomial 04C11DB7, no reflection,
    // computed with the checksum field itself zero.
    let mut crc = 0u32;
    for &byte in &page {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

/// A WAV file whose data chunk is `stream`, laid out as FL Studio writes
/// them: format, data, then a `fact` chunk and a `LIST` after the audio.
fn wav_around_vorbis(format_tag: u16, stream: &[u8], rate: u32) -> Vec<u8> {
    let mut chunks = Vec::new();
    chunks.extend_from_slice(b"fmt ");
    chunks.extend_from_slice(&18u32.to_le_bytes());
    chunks.extend_from_slice(&format_tag.to_le_bytes());
    chunks.extend_from_slice(&2u16.to_le_bytes());
    chunks.extend_from_slice(&rate.to_le_bytes());
    chunks.extend_from_slice(&16_000u32.to_le_bytes());
    chunks.extend_from_slice(&[1, 0, 16, 0, 0, 0]);
    chunks.extend_from_slice(b"data");
    chunks.extend_from_slice(&(stream.len() as u32).to_le_bytes());
    chunks.extend_from_slice(stream);
    if stream.len() % 2 == 1 {
        chunks.push(0);
    }
    chunks.extend_from_slice(b"fact\x04\0\0\0\0\0\0\0LIST\x04\0\0\0INFO");
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(4 + chunks.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVE");
    file.extend_from_slice(&chunks);
    file
}

#[test]
fn problems_are_reported_in_plain_words() {
    let folder = tempfile::tempdir().unwrap();

    let missing = folder.path().join("missing.wav");
    assert_eq!(decode(&missing).unwrap_err(), MediaError::NotFound { path: missing.clone() });
    assert_eq!(probe(&missing).unwrap_err(), MediaError::NotFound { path: missing.clone() });
    assert!(decode(&missing).unwrap_err().to_string().ends_with("missing.wav' does not exist"));

    // A folder is not a file.
    assert!(matches!(decode(folder.path()).unwrap_err(), MediaError::Io { .. } | MediaError::Unsupported { .. }));

    let empty = folder.path().join("empty.wav");
    std::fs::write(&empty, b"").unwrap();
    let error = decode(&empty).unwrap_err();
    assert!(matches!(error, MediaError::Corrupt { .. }) && error.to_string().contains("empty"), "{error}");

    let text = folder.path().join("notes.wav");
    std::fs::write(&text, "This is not audio at all, just a text file with the wrong name.\n".repeat(200)).unwrap();
    for error in [decode(&text).unwrap_err(), probe(&text).unwrap_err()] {
        assert!(matches!(error, MediaError::Unsupported { .. }), "{error}");
        assert!(error.to_string().contains("is not a WAV, AIFF, FLAC, MP3 or Ogg Vorbis file"), "{error}");
        assert_eq!(error.path(), text);
    }

    // The shape of the samples in Ableton's factory packs: an AIFF-C whose
    // audio is encoded in a way only Live understands.
    let protected = folder.path().join("pack sample.aif");
    let mut bytes = b"FORM\0\0\0\x40AIFCFVER\0\0\0\x04\xa2\x80\x51\x40able\0\0\0\x08\0\0\0\0\0\0\0\0".to_vec();
    bytes.extend_from_slice(b"COMM\0\0\0\x18\0\x02\0\0\x10\0\0\x10\x40\x0e\xac\x44\0\0\0\0\0\0able\0\0");
    std::fs::write(&protected, bytes).unwrap();
    let error = decode(&protected).unwrap_err();
    assert!(matches!(error, MediaError::Unsupported { .. }), "{error}");
    assert!(error.to_string().contains("Ableton's protected pack format"), "{error}");

    // A WAV with a header and no samples.
    let silent = folder.path().join("no-samples.wav");
    write_wav(&silent, 44_100, 2, 16, false, 0);
    assert_eq!(decode(&silent).unwrap_err(), MediaError::Empty { path: silent.clone() });
    assert_eq!(probe(&silent).unwrap().frames, Some(0));
}

#[test]
fn a_file_cut_short_decodes_as_far_as_it_goes() {
    let folder = tempfile::tempdir().unwrap();
    let whole = folder.path().join("whole.wav");
    let expected = write_wav(&whole, 44_100, 2, 16, false, 20_000);
    let bytes = std::fs::read(&whole).unwrap();

    // Half the samples are gone but the header still promises all of them.
    let cut = folder.path().join("cut.wav");
    std::fs::write(&cut, &bytes[..44 + 10_000 * 4]).unwrap();
    let audio = decode(&cut).unwrap();
    assert!((9_000..=10_000).contains(&audio.frames()), "{} frames", audio.frames());
    assert_eq!(audio.left[..audio.frames()], expected[0][..audio.frames()]);

    // Cut inside the header: an error, not a panic.
    let stub = folder.path().join("stub.wav");
    std::fs::write(&stub, &bytes[..20]).unwrap();
    assert!(decode(&stub).is_err());
    assert!(probe(&stub).is_err());
}

/// A small deterministic generator for the damage test.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: usize) -> usize {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % sides.max(1)
    }
}

#[test]
fn damaged_files_never_panic() {
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("source.wav");
    write_wav(&source, 44_100, 2, 16, false, 8_000);
    let aiff = folder.path().join("source.aiff");
    write_aiff(&aiff, 44_100, 2, 16, 8_000);
    let mut originals = vec![source.clone(), aiff];
    for (name, options) in [
        ("damage.flac", &["-c:a", "flac"][..]),
        ("damage.mp3", &["-c:a", "libmp3lame", "-b:a", "128k"][..]),
        ("damage.ogg", &["-c:a", "libvorbis"][..]),
    ] {
        let path = folder.path().join(name);
        if ffmpeg(&source, &path, options) {
            originals.push(path);
        }
    }

    let mut dice = Dice(0x5eed);
    let (mut decoded, mut refused, mut decoder_faults) = (0, 0, 0);
    for original in &originals {
        let bytes = std::fs::read(original).unwrap();
        let extension = original.extension().unwrap().to_str().unwrap();
        for round in 0..120 {
            let mut damaged = bytes.clone();
            match round % 4 {
                // Scattered wrong bytes, concentrated on the header in every other round.
                0 | 1 => {
                    let reach = if round % 8 < 2 { damaged.len().min(256) } else { damaged.len() };
                    for _ in 0..1 + dice.roll(12) {
                        let at = dice.roll(reach);
                        damaged[at] = dice.roll(256) as u8;
                    }
                }
                // Cut short.
                2 => damaged.truncate(dice.roll(damaged.len())),
                // A stretch overwritten with one value.
                _ => {
                    let at = dice.roll(damaged.len());
                    let end = (at + 1 + dice.roll(2_000)).min(damaged.len());
                    let value = dice.roll(256) as u8;
                    damaged[at..end].fill(value);
                }
            }
            let path = folder.path().join(format!("damaged.{extension}"));
            std::fs::write(&path, &damaged).unwrap();
            // Either outcome is fine; reaching the next line is the test.
            let _ = probe(&path);
            match decode(&path) {
                Ok(audio) => {
                    assert!(audio.left.iter().chain(&audio.right).all(|sample| sample.is_finite()));
                    assert_eq!(audio.left.len(), audio.right.len());
                    decoded += 1;
                }
                Err(MediaError::Corrupt { reason, .. }) if reason.contains("could not make sense") => {
                    decoder_faults += 1;
                }
                Err(_) => refused += 1,
            }
        }
    }
    println!(
        "{} damaged files from {} originals: {decoded} still decoded, {refused} refused, {decoder_faults} decoder panics contained",
        decoded + refused + decoder_faults,
        originals.len()
    );
}

#[test]
fn probing_reads_only_the_header() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("long.wav");
    // Four minutes: far too much to decode a thousand times in a test.
    write_wav(&path, 44_100, 2, 16, false, 44_100 * 240);
    let rounds = 1_000;
    let started = std::time::Instant::now();
    for _ in 0..rounds {
        assert_eq!(probe(&path).unwrap().frames, Some(44_100 * 240));
    }
    let each = started.elapsed() / rounds;
    println!("probe of a 4-minute WAV: {each:?} per call");
    assert!(each.as_millis() < 5, "probing took {each:?} per file");
}

/// Folders on this machine that usually hold audio in the formats worth
/// checking against files made by other software. Add more with the
/// `TRILL_TEST_MEDIA` variable (separated like `PATH`).
fn real_media_folders() -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> =
        std::env::var_os("TRILL_TEST_MEDIA").map(|value| std::env::split_paths(&value).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from) {
        for relative in [
            "Documents/Splice",
            "Splice",
            "Documents/Ableton/Factory Packs",
            "Music/Ableton/Factory Packs",
            "Documents/Ableton/User Library",
            "Music",
        ] {
            folders.push(home.join(relative));
        }
    }
    folders.push(PathBuf::from(r"C:\Windows\Media"));
    folders.push(PathBuf::from(r"C:\Program Files\Image-Line"));
    folders.push(PathBuf::from(r"C:\ProgramData\Ableton"));
    folders.push(PathBuf::from("/usr/share/sounds"));
    folders.push(PathBuf::from("/System/Library/Sounds"));
    folders
}

/// Up to `per_folder` files of each format from each of `folders`, spread
/// evenly over what a bounded walk of the folder finds, so that one large
/// pack does not supply every example.
fn find_real_files(folders: &[PathBuf], per_folder: usize) -> Vec<(&'static str, Vec<PathBuf>)> {
    let mut found: Vec<(&'static str, Vec<PathBuf>)> =
        ["wav", "aif", "flac", "mp3", "ogg"].into_iter().map(|extension| (extension, Vec::new())).collect();
    for folder in folders {
        let mut candidates: Vec<Vec<PathBuf>> = vec![Vec::new(); found.len()];
        let mut pending = vec![folder.clone()];
        let mut visited = 0;
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else { continue };
            for entry in entries.flatten() {
                visited += 1;
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if is_audio_file(&path) {
                    let extension = path.extension().unwrap().to_string_lossy().to_lowercase();
                    let extension = if extension == "aiff" { "aif" } else { extension.as_str() };
                    if let Some(index) = found.iter().position(|(known, _)| *known == extension) {
                        candidates[index].push(path);
                    }
                }
            }
            if visited > 150_000 {
                break;
            }
        }
        for ((_, files), mut candidates) in found.iter_mut().zip(candidates) {
            candidates.sort();
            let stride = candidates.len().div_ceil(per_folder).max(1);
            files.extend(candidates.into_iter().step_by(stride));
        }
    }
    found
}

#[test]
#[ignore = "reads audio files from this machine's folders; run with `cargo test -p trill-media -- --include-ignored`"]
fn real_files_from_this_machine_decode() {
    let found = find_real_files(&real_media_folders(), 25);
    for (extension, files) in &found {
        if files.is_empty() {
            println!("SKIPPED {extension}: no real files found on this machine");
            continue;
        }
        let mut seconds = 0.0;
        let mut decoded = 0;
        let mut unreadable: Vec<String> = Vec::new();
        for path in files {
            // Other software's files are not all readable (Ableton ships
            // its packs in a protected AIFF variant, for one). Those must
            // be refused by both functions alike; everything that does
            // decode must be sane.
            let (info, audio) = match (probe(path), decode(path)) {
                (Ok(info), Ok(audio)) => (info, audio),
                (Err(probe_error), Err(decode_error)) => {
                    assert!(
                        matches!(decode_error, MediaError::Unsupported { .. } | MediaError::Corrupt { .. }),
                        "{decode_error}"
                    );
                    assert_eq!(probe_error.path(), path.as_path());
                    unreadable.push(decode_error.to_string());
                    continue;
                }
                (probed, decoded) => panic!(
                    "{}: probe and decode disagree: {:?} / {:?}",
                    path.display(),
                    probed.map(|info| info.codec),
                    decoded.map(|audio| audio.frames())
                ),
            };
            decoded += 1;
            let context = path.display();
            assert!(audio.frames() > 0, "{context}");
            assert_eq!(audio.left.len(), audio.right.len(), "{context}");
            assert_eq!(audio.sample_rate, f64::from(info.sample_rate), "{context}");
            assert!(audio.left.iter().chain(&audio.right).all(|sample| sample.is_finite()), "{context}");
            let peak = audio.left.iter().chain(&audio.right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            assert!(peak > 0.0 && peak < 8.0, "{context}: peak {peak}");
            // The header's length matches what decoding produced. MP3
            // headers may count the encoder's padding.
            let slack = if *extension == "mp3" { 3 * 1152 } else { 0 };
            let probed = info.frames.unwrap_or_else(|| panic!("{context}: no length in the header"));
            assert!(
                probed.abs_diff(audio.frames() as u64) <= slack,
                "{context}: header says {probed} frames, decoded {}",
                audio.frames()
            );
            seconds += audio.seconds();
        }
        println!("verified {decoded} of {} real .{extension} files ({seconds:.1} s of audio)", files.len());
        if let Some(example) = unreadable.first() {
            println!("  {} refused, e.g. {example}", unreadable.len());
        }
    }
}

/// For a WAV holding Ogg Vorbis as FL Studio writes them: the number of
/// frames in the stream, read from the last page of the stream the data
/// starts with, and the number the file's `fact` chunk states. `None` for
/// any other file.
fn lengths_of_vorbis_wav(path: &Path) -> Option<(u64, Option<u64>)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..16)? != b"WAVEfmt " || bytes.get(20..22)? != [0x4F, 0x67] {
        return None;
    }
    let (mut stream, mut fact) = (None, None);
    let mut at = 12;
    while let Some(header) = bytes.get(at..at + 8) {
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let body = bytes.get(at + 8..(at + 8 + size).min(bytes.len()))?;
        match &header[..4] {
            b"fact" => fact = body.get(..4).map(|samples| u64::from(u32::from_le_bytes(samples.try_into().unwrap()))),
            b"data" => stream = Some(body),
            _ => {}
        }
        at += 8 + size + (size & 1);
    }

    // Walk the pages: 27 bytes of header, a table of segment sizes, the body.
    let stream = stream?;
    let own_serial = stream.get(14..18)?;
    let (mut page, mut frames) = (0, None);
    while let Some(header) = stream.get(page..page + 27).filter(|header| &header[..4] == b"OggS") {
        let segments = stream.get(page + 27..page + 27 + usize::from(header[26]))?;
        if &header[14..18] == own_serial {
            frames = Some(u64::from_le_bytes(header[6..14].try_into().unwrap()));
        }
        page += 27 + segments.len() + segments.iter().map(|size| usize::from(*size)).sum::<usize>();
    }
    Some((frames?, fact))
}

/// FL Studio's factory samples are the reason WAV-wrapped Vorbis is read at
/// all, and where its oddities were found, so they are the test of it: every
/// one must come out as long as its stream is, to the frame.
#[test]
#[ignore = "reads audio files from this machine's folders; run with `cargo test -p trill-media -- --include-ignored`"]
fn fl_studio_samples_decode_to_their_full_length() {
    let mut samples: Vec<(PathBuf, u64, Option<u64>)> = Vec::new();
    for folder in real_media_folders() {
        let mut pending = vec![folder];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
                    && let Some((frames, fact)) = lengths_of_vorbis_wav(&path)
                {
                    samples.push((path, frames, fact));
                }
            }
            if samples.len() > 20_000 {
                break;
            }
        }
    }
    if samples.is_empty() {
        println!("SKIPPED: no WAV files holding Ogg Vorbis (FL Studio's factory samples) on this machine");
        return;
    }
    samples.sort();
    let mut seconds = 0.0;
    for (path, frames, _) in &samples {
        let context = path.display();
        let info = probe(path).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(info.frames, Some(*frames), "{context}: the length in the header");
        assert_eq!(info.codec, "vorbis", "{context}");
        let audio = decode(path).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(audio.frames() as u64, *frames, "{context}: the decoded length");
        assert!(audio.left.iter().chain(&audio.right).all(|sample| sample.is_finite()), "{context}");
        seconds += audio.seconds();
    }
    println!("verified {} real WAV files holding Ogg Vorbis ({seconds:.1} s of audio)", samples.len());
    // The `fact` chunk is not what is checked against: it is not always the
    // length of the stream.
    let disagreeing = samples.iter().filter(|(_, frames, fact)| fact.is_some_and(|fact| fact != *frames)).count();
    println!("  in {disagreeing} of them the fact chunk states a different length than the stream holds");
}
