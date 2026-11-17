//! Following the pitch of a single voice: a bass line, a sung melody.
//!
//! This is the YIN method: the pitch of a frame is the shortest lag at
//! which the frame, shifted, is nearly itself again. A signal holding one
//! instrument is the case it was made for, so nothing heavier is needed.

use crate::signal::{RATE, db, median, percentile, resample};
use serde::{Deserialize, Serialize};

/// Pitch over time, one value for each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PitchTrack {
    /// The middle of each frame, in seconds.
    pub times: Vec<f64>,
    /// The pitch as a MIDI note number with its fraction; it means
    /// something only where `voiced` is set.
    pub midi: Vec<f64>,
    pub voiced: Vec<bool>,
    /// The level of each frame, in dB.
    pub level: Vec<f64>,
}

/// Follows the pitch of `signal` (mono at [`RATE`]) between `low_hz` and
/// `high_hz`.
pub fn track(signal: &[f32], low_hz: f64, high_hz: f64) -> PitchTrack {
    // A low instrument needs long windows but not a high sample rate:
    // halving the rate for it does the same job in a quarter of the work.
    let (rate, hop) = if high_hz <= 500.0 { (11_025.0, 128usize) } else { (22_050.0, 256usize) };
    let signal = resample(signal, RATE, rate);
    let longest = (rate / low_hz) as usize + 2;
    let shortest = ((rate / high_hz) as usize).max(2);
    let window = ((longest as f64 * 2.5) as usize).next_power_of_two();
    if signal.len() < window + hop {
        return PitchTrack::default();
    }
    let count = 1 + (signal.len() - window) / hop;
    let half = window / 2;
    let mut out = PitchTrack {
        times: (0..count).map(|frame| (frame * hop + window / 4) as f64 / rate).collect(),
        midi: Vec::with_capacity(count),
        voiced: Vec::with_capacity(count),
        level: Vec::with_capacity(count),
    };
    let mut clarity = Vec::with_capacity(count);
    let mut normal = vec![1.0f64; longest + 1];
    for frame in 0..count {
        let frame = &signal[frame * hop..frame * hop + window];
        let head = &frame[..half];
        // How unlike itself the frame is at each lag: the energy of the two
        // stretches less twice what they have in common.
        let energy = |lag: usize| -> f64 { frame[lag..lag + half].iter().map(|&v| f64::from(v) * f64::from(v)).sum() };
        let own = energy(0);
        let mut running = 0.0;
        let mut shifted = own;
        for lag in 1..=longest {
            // The window's energy moves on by one sample.
            shifted += f64::from(frame[lag + half - 1]).powi(2) - f64::from(frame[lag - 1]).powi(2);
            let common: f32 = head.iter().zip(&frame[lag..lag + half]).map(|(a, b)| a * b).sum();
            let difference = (own + shifted - 2.0 * f64::from(common)).max(0.0);
            // Against its running mean, so that a dip stands out whatever the level.
            running += difference;
            normal[lag] = difference / (running / lag as f64).max(1e-12);
        }
        // The first lag that dips under the threshold and is a local
        // minimum; failing that, the deepest dip anywhere.
        let search = &normal[shortest..=longest];
        let dip = (1..search.len().saturating_sub(1))
            .find(|&index| {
                search[index] < 0.15 && search[index] <= search[index - 1] && search[index] < search[index + 1]
            })
            .unwrap_or_else(|| {
                (0..search.len()).min_by(|&a, &b| search[a].total_cmp(&search[b]).then(a.cmp(&b))).unwrap_or(0)
            });
        let lag = (dip + shortest).clamp(1, longest - 1);
        // A parabola through the dip and its neighbours finds the true lag
        // between samples.
        let (left, centre, right) = (normal[lag - 1], normal[lag], normal[lag + 1]);
        let curve = left - 2.0 * centre + right;
        let offset = if curve.abs() > 1e-12 { (0.5 * (left - right) / curve).clamp(-1.0, 1.0) } else { 0.0 };
        out.midi.push(69.0 + 12.0 * (rate / (lag as f64 + offset) / 440.0).log2());
        clarity.push(centre);
        out.level.push(db((own / half as f64).sqrt()));
    }
    let loud = percentile(&out.level, 95.0);
    out.voiced = (0..count).map(|frame| clarity[frame] < 0.3 && out.level[frame] > (loud - 40.0).max(-60.0)).collect();
    out
}

/// One note of a voice.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoiceNote {
    /// Where it starts and ends, in seconds.
    pub start: f64,
    pub end: f64,
    /// The MIDI note it settled on.
    pub midi: u8,
    /// How far from that note it sat, in cents.
    pub cents: f64,
    /// Its loudest frame, in dB.
    pub level: f64,
}

/// Groups a pitch track into notes: a note lasts while the pitch stays
/// within about half a semitone of where it settled, and counts when it
/// lasts `shortest` seconds.
pub fn notes(pitch: &PitchTrack, shortest: f64) -> Vec<VoiceNote> {
    let count = pitch.times.len();
    if count == 0 {
        return Vec::new();
    }
    let step = if count > 1 { pitch.times[1] - pitch.times[0] } else { 0.0116 };
    let mut found: Vec<VoiceNote> = Vec::new();
    let mut begin: Option<usize> = None;
    let mut values: Vec<f64> = Vec::new();
    let mut close = |begin: &mut Option<usize>, values: &mut Vec<f64>, end: usize| {
        if let Some(first) = begin.take()
            && values.len() as f64 * step >= shortest
        {
            let centre = median(values);
            let nearest = centre.round();
            if (0.0..=127.0).contains(&nearest) {
                found.push(VoiceNote {
                    start: pitch.times[first] - step / 2.0,
                    end: pitch.times[end - 1] + step / 2.0,
                    midi: nearest as u8,
                    cents: ((centre - nearest) * 1000.0).round() / 10.0,
                    level: pitch.level[first..end].iter().copied().fold(f64::NEG_INFINITY, f64::max),
                });
            }
        }
        values.clear();
    };
    for index in 0..count {
        if !pitch.voiced[index] {
            close(&mut begin, &mut values, index);
            continue;
        }
        if begin.is_none() {
            begin = Some(index);
            values.push(pitch.midi[index]);
            continue;
        }
        if (pitch.midi[index] - median(&values)).abs() > 0.6 {
            close(&mut begin, &mut values, index);
            begin = Some(index);
        }
        values.push(pitch.midi[index]);
    }
    close(&mut begin, &mut values, count);

    // A note joins the one before when the same pitch resumes after a
    // break too short to be a new attack.
    let mut merged: Vec<VoiceNote> = Vec::new();
    for note in found {
        match merged.last_mut() {
            Some(last) if last.midi == note.midi && note.start - last.end < 0.03 => {
                last.end = note.end;
                last.level = last.level.max(note.level);
            }
            _ => merged.push(note),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::testing::{hertz, silence, tone};

    #[test]
    fn a_bass_line_is_followed_note_by_note() {
        let mut signal = silence(3.0);
        let line = [(0.2, 0.5, 28.0), (0.8, 0.4, 31.0), (1.3, 0.6, 35.0), (2.1, 0.5, 40.0)];
        for &(at, seconds, note) in &line {
            tone(&mut signal, at, seconds, hertz(note, 0.0), 0.6);
        }
        let found = notes(&track(&signal, 30.0, 400.0), 0.09);
        assert_eq!(found.iter().map(|note| note.midi).collect::<Vec<_>>(), [28, 31, 35, 40], "{found:?}");
        for (note, &(at, seconds, _)) in found.iter().zip(&line) {
            assert!((note.start - at).abs() < 0.03, "{note:?} should start at {at}");
            assert!((note.end - (at + seconds)).abs() < 0.04, "{note:?} should end at {}", at + seconds);
            assert!(note.cents.abs() < 12.0);
        }
    }

    #[test]
    fn a_sung_note_that_sits_flat_says_by_how_much() {
        let mut signal = silence(1.5);
        tone(&mut signal, 0.2, 1.0, hertz(64.0, -30.0), 0.5);
        let found = notes(&track(&signal, 70.0, 1100.0), 0.07);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].midi, 64);
        assert!((found[0].cents + 30.0).abs() < 4.0, "{}", found[0].cents);
    }

    #[test]
    fn silence_noise_and_a_sound_too_short_hold_no_notes() {
        assert!(notes(&track(&silence(2.0), 30.0, 400.0), 0.09).is_empty());
        let mut noise = crate::signal::testing::Noise(9);
        let hiss: Vec<f32> = (0..44_100).map(|_| 0.3 * noise.next()).collect();
        assert!(notes(&track(&hiss, 70.0, 1100.0), 0.07).is_empty());
        assert_eq!(track(&[0.1; 200], 30.0, 400.0), PitchTrack::default());
    }

    #[test]
    fn a_note_that_breaks_for_an_instant_is_one_note() {
        let mut signal = silence(2.0);
        tone(&mut signal, 0.2, 0.6, hertz(45.0, 0.0), 0.6);
        tone(&mut signal, 0.805, 0.6, hertz(45.0, 0.0), 0.6);
        let found = notes(&track(&signal, 30.0, 400.0), 0.09);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].end - found[0].start > 1.1);
    }
}
