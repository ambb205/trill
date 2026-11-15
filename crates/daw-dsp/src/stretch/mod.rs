//! Time-stretching and pitch-shifting of whole audio buffers.
//!
//! This is what fits a loop recorded at one tempo into a song at another.
//! It runs when the engine prepares a clip, never on the audio thread: it
//! allocates freely and is tuned for quality, at well over twenty times real
//! time. The same input and options always give the same output.
//!
//! No single algorithm stretches everything well, so the caller picks a
//! [`StretchMode`]:
//!
//! * [`StretchMode::Beats`] keeps transients intact and in time by copying
//!   them, and stretches only the decay between them (see [`beats`]). For
//!   drums, percussion and other rhythmic material.
//! * [`StretchMode::Tonal`] is a phase vocoder that keeps sustained notes
//!   smooth and in tune (see [`tonal`]). For chords, pads, bass, vocals and
//!   full mixes.
//! * [`StretchMode::Repitch`] resamples, so pitch follows speed like a
//!   record on a turntable. Nothing is lost or invented, at the price of the
//!   pitch change.
//!
//! Pitch-shifting is stretching by the pitch ratio followed by resampling
//! back, so it shares the modes' strengths. Formants move with the pitch.

mod beats;
mod onsets;
mod timemap;
mod tonal;

pub use onsets::Onset;

use crate::resample::resample_to_length;
use daw_core::audio::AudioBuffer;
use serde::{Deserialize, Serialize};

/// Time ratios outside this range are clamped to it. Quality is tuned for
/// 0.5..=2.0; beyond that the result is usable but increasingly synthetic.
pub const MIN_TIME_RATIO: f64 = 0.25;
pub const MAX_TIME_RATIO: f64 = 4.0;
/// Pitch shifts are clamped to this many semitones either way.
pub const MAX_PITCH_SEMITONES: f64 = 24.0;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StretchMode {
    /// Transient-preserving, for drums and rhythmic material.
    Beats,
    /// Phase vocoder, for sustained and harmonic material.
    #[default]
    Tonal,
    /// Plain resampling: duration and pitch change together and
    /// [`StretchOptions::pitch_semitones`] is ignored.
    Repitch,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StretchOptions {
    /// Output duration divided by input duration: 2.0 plays at half speed.
    /// To fit a loop to a song this is `loop tempo / song tempo` (see
    /// [`tempo_ratio`]).
    pub time_ratio: f64,
    /// Pitch shift in semitones, independent of the duration.
    pub pitch_semitones: f64,
    pub mode: StretchMode,
}

impl Default for StretchOptions {
    fn default() -> Self {
        Self { time_ratio: 1.0, pitch_semitones: 0.0, mode: StretchMode::default() }
    }
}

/// The time ratio that makes audio recorded at `source_bpm` play in time at
/// `target_bpm`: a 92 BPM loop in a 120 BPM song is stretched by 92/120.
pub fn tempo_ratio(source_bpm: f64, target_bpm: f64) -> f64 {
    if source_bpm > 0.0 && target_bpm > 0.0 { source_bpm / target_bpm } else { 1.0 }
}

/// Changes the duration and/or pitch of `input`.
///
/// The result has exactly `round(input.frames() * time_ratio)` frames at the
/// input's sample rate, and begins and ends on the input's first and last
/// frames, so a loop that was seamless stays seamless. Options that are out
/// of range or not finite are clamped; a ratio of 1 with no pitch shift
/// returns the input unchanged.
pub fn stretch(input: &AudioBuffer, options: &StretchOptions) -> AudioBuffer {
    let frames = input.frames();
    let time_ratio = finite_or(options.time_ratio, 1.0).clamp(MIN_TIME_RATIO, MAX_TIME_RATIO);
    let out_len = (frames as f64 * time_ratio).round() as usize;
    let (left, right) = (&input.left[..frames], &input.right[..frames]);
    let finish = |left: Vec<f32>, right: Vec<f32>| AudioBuffer { sample_rate: input.sample_rate, left, right };
    if frames == 0 || out_len == 0 {
        return finish(vec![0.0; out_len], vec![0.0; out_len]);
    }

    let algorithm = match options.mode {
        StretchMode::Repitch => return resample_to_length(input, out_len),
        StretchMode::Beats => beats::stretch,
        StretchMode::Tonal => tonal::stretch,
    };
    // Shifting pitch up by a ratio p is stretching by p and then playing the
    // result p times faster, which the resampler does.
    let semitones = finite_or(options.pitch_semitones, 0.0).clamp(-MAX_PITCH_SEMITONES, MAX_PITCH_SEMITONES);
    let stretched_len = ((out_len as f64 * 2f64.powf(semitones / 12.0)).round() as usize).max(1);
    if stretched_len == frames {
        return resample_to_length(&finish(left.to_vec(), right.to_vec()), out_len);
    }
    // Sample rates below 8 kHz would make the analysis windows degenerate.
    let sample_rate = finite_or(input.sample_rate, 48_000.0).max(8_000.0);
    let detected = onsets::detect(left, right, sample_rate);
    let (left, right) = algorithm(left, right, sample_rate, stretched_len, &detected);
    resample_to_length(&finish(left, right), out_len)
}

/// Changes the duration of `input` by resampling, so that the pitch changes
/// with it "like a turntable": a ratio of 0.5 plays twice as fast and an
/// octave higher. Shorthand for [`stretch`] in [`StretchMode::Repitch`].
pub fn repitch(input: &AudioBuffer, time_ratio: f64) -> AudioBuffer {
    stretch(input, &StretchOptions { time_ratio, pitch_semitones: 0.0, mode: StretchMode::Repitch })
}

/// Finds the transients in `input`: drum hits and note attacks, in order.
/// This is the analysis the stretch modes use to decide what to protect;
/// it is also what slicing a loop at its hits needs.
pub fn detect_onsets(input: &AudioBuffer) -> Vec<Onset> {
    let frames = input.frames();
    let sample_rate = finite_or(input.sample_rate, 48_000.0).max(8_000.0);
    onsets::detect(&input.left[..frames], &input.right[..frames], sample_rate)
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}
