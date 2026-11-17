//! What a sound is like, in numbers: how loud, where its energy lies, how
//! wide it is, how fast a note of it begins and ends.
//!
//! Nobody who writes a sound for Trill's own instruments can hear it, so
//! a bank of them is held to what can be measured, and to what the sounds
//! of a synth that is known to be good measure by the same rule. This is
//! that rule: pure functions of the samples of a render, and the notes a
//! kind of sound is played to be measured ([`playing`]), so that two
//! sounds are compared on the same playing whoever made them.

use trill_core::Note;
use trill_dsp::fft::RealFft;

/// What is as good as silence, in dB.
const FLOOR_DB: f32 = -90.0;
/// The frames of a transform, and how far apart they are taken.
const FRAME: usize = 8192;
const HOP: usize = FRAME / 2;

/// What a render of a sound measures over all of it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Character {
    /// Its level over its loudest 400 ms, in dBFS, the two sides averaged.
    pub level_db: f32,
    /// Its highest sample, in dBFS.
    pub peak_db: f32,
    /// Where the weight of its energy lies: the frequency that has as much
    /// of it below as above, counted in octaves.
    pub centre_hz: f32,
    /// The share of its energy under 250 Hz, and over 4 kHz, in dB against
    /// the whole.
    pub low_db: f32,
    pub high_db: f32,
    /// What differs between its sides against what they share, in dB: far
    /// under 0 for a sound in the middle, near 0 for a wide one.
    pub width_db: f32,
    /// The same under 120 Hz, where a bass belongs in the middle.
    pub low_width_db: f32,
}

/// How a note of a sound begins and ends.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Onset {
    /// From the key going down to the sound reaching half of the level it
    /// comes to while the key is held, in seconds.
    pub attack_s: f32,
    /// From the key coming up to the last moment the sound is within 30 dB
    /// of that level, in seconds: 0 for a sound that has rung out by then.
    pub release_s: f32,
}

fn db(power: f64) -> f32 {
    (10.0 * power.max(1e-30).log10()).max(f64::from(FLOOR_DB)) as f32
}

/// What `left` and `right`, a render at `sample_rate`, measure.
pub fn character(left: &[f32], right: &[f32], sample_rate: f64) -> Character {
    let frames = left.len().min(right.len());
    let power: Vec<f32> = left.iter().zip(right).map(|(l, r)| 0.5 * (l * l + r * r)).collect();
    let window = ((0.4 * sample_rate) as usize).min(frames.max(1));
    let step = (window / 8).max(1);
    let loudest = (0..=frames.saturating_sub(window))
        .step_by(step)
        .map(|start| power[start..start + window].iter().map(|p| f64::from(*p)).sum::<f64>() / window as f64)
        .fold(0.0f64, f64::max);
    let peak = left.iter().chain(right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));

    // The spectrum of what the sides share and of what differs between
    // them, averaged over the render.
    let bins = FRAME / 2 + 1;
    let (mut shared, mut apart) = (vec![0.0f64; bins], vec![0.0f64; bins]);
    let mut plan = RealFft::new(FRAME);
    let hann: Vec<f32> =
        (0..FRAME).map(|n| 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / FRAME as f32).cos()).collect();
    let (mut re, mut im) = (vec![0.0f32; bins], vec![0.0f32; bins]);
    let mut taken = vec![0.0f32; FRAME];
    let mut start = 0;
    while start < frames {
        for (into, to) in [(&mut shared, 1.0f32), (&mut apart, -1.0)] {
            for (n, sample) in taken.iter_mut().enumerate() {
                let at = start + n;
                *sample = if at < frames { 0.5 * (left[at] + to * right[at]) * hann[n] } else { 0.0 };
            }
            plan.forward(&taken, &mut re, &mut im);
            for (bin, sum) in into.iter_mut().enumerate() {
                *sum += f64::from(re[bin]).powi(2) + f64::from(im[bin]).powi(2);
            }
        }
        start += HOP;
    }
    let hz = |bin: usize| bin as f64 * sample_rate / FRAME as f64;
    let heard = |bin: usize| hz(bin) >= 20.0;
    let sum = |of: &[f64], which: &dyn Fn(f64) -> bool| -> f64 {
        of.iter().enumerate().filter(|(bin, _)| heard(*bin) && which(hz(*bin))).map(|(_, power)| *power).sum()
    };
    let whole = sum(&shared, &|_| true) + sum(&apart, &|_| true);
    let weighted: f64 = (0..bins).filter(|bin| heard(*bin)).map(|bin| (shared[bin] + apart[bin]) * hz(bin).ln()).sum();
    let under = |limit: f64| sum(&shared, &|at| at < limit) + sum(&apart, &|at| at < limit);
    Character {
        level_db: db(loudest),
        peak_db: db(f64::from(peak).powi(2)),
        centre_hz: if whole > 0.0 { (weighted / whole).exp() as f32 } else { 0.0 },
        low_db: db(under(250.0) / whole.max(1e-30)),
        high_db: db((whole - under(4_000.0)).max(0.0) / whole.max(1e-30)),
        width_db: db(sum(&apart, &|_| true) / sum(&shared, &|_| true).max(1e-30)),
        low_width_db: db(sum(&apart, &|at| at < 120.0) / sum(&shared, &|at| at < 120.0).max(1e-30)),
    }
}

/// How the one note in `left` and `right` begins and ends: its key went
/// down at the frame `on` and came up at `off`.
pub fn onset(left: &[f32], right: &[f32], sample_rate: f64, on: usize, off: usize) -> Onset {
    let frames = left.len().min(right.len());
    // The level every five milliseconds.
    let hop = ((0.005 * sample_rate) as usize).max(1);
    let level: Vec<f64> = (0..frames / hop)
        .map(|window| {
            let from = window * hop;
            let sum: f64 = (from..from + hop).map(|n| f64::from(left[n]).powi(2) + f64::from(right[n]).powi(2)).sum();
            (sum / (2 * hop) as f64).sqrt()
        })
        .collect();
    let (on, off) = ((on / hop).min(level.len()), (off / hop).min(level.len()));
    let held = level[on..off].iter().copied().fold(0.0f64, f64::max);
    if held <= 0.0 {
        return Onset { attack_s: 0.0, release_s: 0.0 };
    }
    let reached = level[on..off].iter().position(|now| *now >= 0.5 * held).unwrap_or(0);
    let floor = held * 10f64.powf(-30.0 / 20.0);
    let last = level[off..].iter().rposition(|now| *now >= floor).map_or(0, |window| window + 1);
    let seconds = |windows: usize| (windows * hop) as f64 / sample_rate;
    Onset { attack_s: seconds(reached) as f32, release_s: seconds(last) as f32 }
}

/// What a kind of sound is played to be measured: the same notes for
/// every sound of the kind, whoever made it.
#[derive(Clone, Debug, PartialEq)]
pub struct Playing {
    pub tempo: f64,
    /// A phrase as the kind is played, from the start of the song, at a
    /// velocity of 100; and how many beats it lasts.
    pub notes: Vec<Note>,
    pub beats: f64,
    /// As full a chord as the kind is given, for the room it must leave:
    /// two hands of a pad, keys or strings, a seventh chord of plucks or
    /// bells, a triad of a lead; one key for a sound that plays one note
    /// at a time.
    pub chord: Vec<u8>,
    /// One key and how many beats it is held, for how a note begins and
    /// ends.
    pub single: u8,
    pub held: f64,
    /// How many beats of the song follow the phrase, and the one note,
    /// for what was played to ring out in: they are part of what is
    /// measured, and they lie before the song stops, since a hosted
    /// instrument may be silenced when it does.
    pub ring: f64,
}

/// The beats a sound is given to ring out in: eight seconds at the tempo
/// it is played at.
const RING: f64 = 16.0;

/// The kinds [`playing`] knows, as a bank files its sounds.
pub const KINDS: &[&str] = &["Bass", "Lead", "Pluck", "Pad", "Keys", "Bell", "Strings", "Brass", "Texture", "FX"];

/// What a sound of `kind` is played: a line for one that plays
/// `one_at_a_time`, and for a bass or a lead whatever it does.
pub fn playing(kind: &str, one_at_a_time: bool) -> Playing {
    let note = |pitch: u8, start: f64, length: f64| Note { pitch, start, length, velocity: 100 };
    let line = |pitches: &[u8], each: f64, length: f64| -> Vec<Note> {
        pitches.iter().enumerate().map(|(index, pitch)| note(*pitch, index as f64 * each, length)).collect()
    };
    let chord = |pitches: &[u8], start: f64, length: f64| -> Vec<Note> {
        pitches.iter().map(|pitch| note(*pitch, start, length)).collect()
    };
    let tempo = 120.0;
    let (notes, beats, full, single, held): (Vec<Note>, f64, &[u8], u8, f64) = match kind {
        // Eighths in the octave every bass is written in.
        "Bass" => (line(&[40, 40, 43, 40, 36, 36, 41, 43], 0.5, 0.45), 4.0, &[40], 40, 2.0),
        "Lead" => (line(&[72, 74, 76, 79, 76, 74, 72, 67], 0.5, 0.45), 4.0, &[72, 76, 79], 72, 2.0),
        "Pluck" => (
            line(&[57, 60, 64, 69, 72, 69, 64, 60, 57, 60, 64, 69, 72, 69, 64, 60], 0.25, 0.2),
            4.0,
            &[57, 60, 64, 69],
            64,
            1.0,
        ),
        "Pad" => (chord(&[48, 55, 60, 64], 0.0, 8.0), 8.0, &[43, 48, 55, 60, 64, 67], 60, 6.0),
        "Keys" => {
            let mut notes = chord(&[48, 55, 60, 64], 0.0, 1.8);
            notes.extend(chord(&[45, 52, 57, 60], 2.0, 1.8));
            notes.extend(line(&[72, 76, 79], 0.5, 0.45).into_iter().map(|n| Note { start: n.start + 0.5, ..n }));
            (notes, 4.0, &[43, 48, 55, 60, 64, 67], 60, 2.0)
        }
        "Bell" => (line(&[72, 76, 79, 84, 79, 76], 1.0, 0.9), 6.0, &[60, 64, 67, 72], 76, 2.0),
        "Strings" => (chord(&[48, 55, 60, 64, 67], 0.0, 8.0), 8.0, &[43, 48, 55, 60, 64, 67], 60, 6.0),
        "Brass" => {
            let mut notes = chord(&[53, 57, 60, 65], 0.0, 0.4);
            notes.extend(chord(&[53, 57, 60, 65], 1.0, 0.4));
            notes.extend(chord(&[55, 58, 62, 67], 2.0, 2.0));
            (notes, 4.0, &[48, 53, 57, 60, 65], 60, 2.0)
        }
        "Texture" => (chord(&[36, 43, 48], 0.0, 12.0), 12.0, &[36, 43, 48, 55], 48, 6.0),
        // An effect is one key held for four bars.
        _ => (chord(&[48], 0.0, 16.0), 16.0, &[48], 48, 16.0),
    };
    if !one_at_a_time {
        return Playing { tempo, notes, beats, chord: full.to_vec(), single, held, ring: RING };
    }
    // One note at a time: the top of whatever sounds together.
    let mut alone: Vec<Note> = Vec::new();
    for note in notes {
        match alone.iter_mut().find(|kept| kept.start == note.start) {
            Some(kept) if note.pitch > kept.pitch => *kept = note,
            Some(_) => {}
            None => alone.push(note),
        }
    }
    Playing { tempo, notes: alone, beats, chord: vec![single], single, held, ring: RING }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn tone(hz: f64, seconds: f64, level: f32) -> Vec<f32> {
        (0..(seconds * RATE) as usize)
            .map(|n| level * (std::f64::consts::TAU * hz * n as f64 / RATE).sin() as f32)
            .collect()
    }

    #[test]
    fn a_tone_measures_what_it_is() {
        // A sine at a tenth of full scale in the middle: 23 dB under it
        // on average, at its frequency, all of it under 250 Hz.
        let low = tone(100.0, 2.0, 0.1);
        let found = character(&low, &low, RATE);
        assert!((found.level_db + 23.0).abs() < 0.2 && (found.peak_db + 20.0).abs() < 0.1, "{found:?}");
        assert!((found.centre_hz - 100.0).abs() < 3.0, "{found:?}");
        assert!(found.low_db > -0.1 && found.high_db < -60.0, "{found:?}");
        assert!(found.width_db <= -80.0 && found.low_width_db <= -80.0, "in the middle: {found:?}");

        // On one side only it is as wide as it can be; with a high tone
        // in the middle beside a low one out of phase, wide below alone.
        let nothing = vec![0.0; low.len()];
        let one_side = character(&low, &nothing, RATE);
        assert!(one_side.width_db.abs() < 0.1 && one_side.low_width_db.abs() < 0.1, "{one_side:?}");
        let high = tone(6_000.0, 2.0, 0.1);
        let left: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        let right: Vec<f32> = low.iter().zip(&high).map(|(a, b)| b - a).collect();
        let split = character(&left, &right, RATE);
        assert!(split.low_width_db > 20.0, "{split:?}");
        assert!((split.width_db).abs() < 0.2, "as much apart as shared: {split:?}");
        assert!((split.high_db + 3.0).abs() < 0.2 && (split.low_db + 3.0).abs() < 0.2, "{split:?}");
        // Half the energy at 100 Hz and half at 6 kHz: the weight lies
        // half way between them in octaves.
        assert!((split.centre_hz / (100.0f32 * 6_000.0).sqrt() - 1.0).abs() < 0.03, "{split:?}");
        // Nothing is nothing, not a number that is none.
        let silent = character(&nothing, &nothing, RATE);
        assert_eq!((silent.level_db, silent.centre_hz), (FLOOR_DB, 0.0));
    }

    #[test]
    fn a_note_is_timed_from_its_key_to_half_its_level_and_to_thirty_db_under_it() {
        // A note that rises over 200 ms, is held, and after its key comes
        // up at two seconds dies away at 60 dB a second.
        let (on, off) = (4_800usize, 96_000usize);
        let note: Vec<f32> = (0..3 * 48_000)
            .map(|n| {
                let level = if n < on {
                    0.0
                } else if n < off {
                    ((n - on) as f32 / 9_600.0).min(1.0)
                } else {
                    10f32.powf(-3.0 * (n - off) as f32 / 48_000.0)
                };
                level * (std::f32::consts::TAU * 440.0 * n as f32 / 48_000.0).sin()
            })
            .collect();
        let timed = onset(&note, &note, RATE, on, off);
        assert!((timed.attack_s - 0.1).abs() < 0.015, "half way up the rise: {timed:?}");
        assert!((timed.release_s - 0.5).abs() < 0.02, "30 dB at 60 dB a second: {timed:?}");
        // A pluck that has rung out before its key comes up has no end to
        // time, and begins at once.
        let pluck: Vec<f32> =
            (0..96_000).map(|n| 10f32.powf(-6.0 * n as f32 / 48_000.0) * (0.06 * n as f32).sin()).collect();
        let timed = onset(&pluck, &pluck, RATE, 0, 72_000);
        assert!(timed.attack_s < 0.011 && timed.release_s == 0.0, "{timed:?}");
        let silent = vec![0.0; 48_000];
        assert_eq!(onset(&silent, &silent, RATE, 0, 24_000), Onset { attack_s: 0.0, release_s: 0.0 });
    }

    #[test]
    fn a_kind_is_played_the_same_notes_and_one_at_a_time_its_top_line() {
        for kind in KINDS {
            let played = playing(kind, false);
            assert!(
                !played.notes.is_empty() && played.notes.iter().all(|note| note.start + note.length <= played.beats)
            );
            assert!(!played.chord.is_empty() && played.held > 0.0, "{kind}");
            let alone = playing(kind, true);
            let mut starts: Vec<f64> = alone.notes.iter().map(|note| note.start).collect();
            starts.dedup();
            assert_eq!(starts.len(), alone.notes.len(), "{kind}: two notes at once");
            assert_eq!(alone.chord, [alone.single]);
        }
        // A pad's chord is a keys' top note for a sound that plays one.
        assert_eq!(playing("Pad", true).notes.iter().map(|note| note.pitch).collect::<Vec<_>>(), [64]);
        assert_eq!(playing("Bass", false), playing("Bass", true), "a bass is a line either way");
    }
}
