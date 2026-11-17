//! Tuning, key and chords.
//!
//! All of it is read from what is pitched: drums smear energy across every
//! pitch class and make every key look alike, so a caller that has the
//! drums apart leaves them out.
//!
//! The energy of each of the twelve pitch classes over time (a *chroma*) is
//! read from three transforms of the same signal, each at a rate that gives
//! its register enough resolution to tell neighbouring semitones apart: a
//! window of a fifth of a second does that from middle C up, and the bottom
//! octave of a bass needs one of a second and a half.

use crate::signal::{RATE, median, percentile, resample};
use crate::spectrum::{Frames, frames, transform};
use serde::{Deserialize, Serialize};

/// The rate the chroma is read at, and the samples between its frames.
pub const CHROMA_RATE: f64 = 22_050.0;
pub const CHROMA_HOP: usize = 512;
const SIZE: usize = 4096;
/// The lowest note read: C1.
const LOWEST_HZ: f64 = 32.703_195_662_574_83;

const SHARP: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
const FLAT: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B"];
// The usual spelling of each key, which is also how the sound library spells them.
const MAJOR_KEYS: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
const MINOR_KEYS: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "G#", "A", "Bb", "B"];

/// How strongly each scale degree is heard in a key, from listening
/// experiments (Krumhansl and Kessler) and from a large set of labelled
/// dance and pop tracks (Sha'ath). The two disagree usefully, so both vote.
const PROFILES: [([f64; 12], [f64; 12]); 2] = [
    (
        [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88],
        [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17],
    ),
    (
        [6.6, 2.0, 3.5, 2.3, 4.6, 4.0, 2.5, 5.2, 2.4, 3.7, 2.3, 3.4],
        [6.5, 2.7, 3.5, 5.4, 2.6, 3.5, 2.5, 5.2, 4.0, 2.7, 4.3, 3.2],
    ),
];

/// A key: its tonic as a pitch class (C = 0) and whether it is minor.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key {
    pub tonic: u8,
    pub minor: bool,
}

impl Key {
    /// `F#m`, `Eb`.
    pub fn name(self) -> String {
        let tonic = usize::from(self.tonic % 12);
        if self.minor { format!("{}m", MINOR_KEYS[tonic]) } else { MAJOR_KEYS[tonic].to_owned() }
    }

    /// The key as file names carry it: `F#min`, `Ebmaj`.
    pub fn tag(self) -> String {
        let tonic = usize::from(self.tonic % 12);
        if self.minor { format!("{}min", MINOR_KEYS[tonic]) } else { format!("{}maj", MAJOR_KEYS[tonic]) }
    }

    /// The key's place on the wheel disc jockeys mix by: `8A`, `11B`.
    pub fn camelot(self) -> String {
        let major = if self.minor { (self.tonic + 3) % 12 } else { self.tonic % 12 };
        let number = ((u32::from(major) * 7) % 12 + 7) % 12 + 1;
        format!("{number}{}", if self.minor { 'A' } else { 'B' })
    }

    /// Whether its notes are spelled with flats: F, Bb, Eb, Ab and Db major
    /// and their relatives.
    pub fn uses_flats(self) -> bool {
        let major = if self.minor { (self.tonic + 3) % 12 } else { self.tonic % 12 };
        matches!(major, 5 | 10 | 3 | 8 | 1)
    }

    /// The relative key: the one with the same notes.
    pub fn relative(self) -> Key {
        if self.minor {
            Key { tonic: (self.tonic + 3) % 12, minor: false }
        } else {
            Key { tonic: (self.tonic + 9) % 12, minor: true }
        }
    }
}

/// A note by name: `F#2`, or `Gb2` in a key spelled with flats.
pub fn note_name(pitch: u8, flats: bool) -> String {
    let names = if flats { &FLAT } else { &SHARP };
    format!("{}{}", names[usize::from(pitch % 12)], i32::from(pitch / 12) - 1)
}

/// One key that was weighed, and how well it fits (a correlation, at most 1).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyScore {
    pub key: Key,
    pub score: f64,
}

/// The key a piece is most likely in, with the runners-up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyReading {
    pub key: Key,
    pub score: f64,
    /// How far ahead the key is of the next that is not merely its
    /// relative, from 0 (a toss-up) to 1.
    pub confidence: f64,
    pub alternates: Vec<KeyScore>,
}

/// What a chord is made of, as far as it is read.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Major,
    Minor,
    Seventh,
    MajorSeventh,
    MinorSeventh,
}

impl Quality {
    const ALL: [Quality; 5] =
        [Quality::Major, Quality::Minor, Quality::Seventh, Quality::MajorSeventh, Quality::MinorSeventh];

    /// What follows the root in the chord's name.
    pub fn suffix(self) -> &'static str {
        match self {
            Quality::Major => "",
            Quality::Minor => "m",
            Quality::Seventh => "7",
            Quality::MajorSeventh => "maj7",
            Quality::MinorSeventh => "m7",
        }
    }

    /// Its notes, in semitones above the root.
    pub fn steps(self) -> &'static [u8] {
        match self {
            Quality::Major => &[0, 4, 7],
            Quality::Minor => &[0, 3, 7],
            Quality::Seventh => &[0, 4, 7, 10],
            Quality::MajorSeventh => &[0, 4, 7, 11],
            Quality::MinorSeventh => &[0, 3, 7, 10],
        }
    }

    /// What naming it costs: a seventh is only named when its extra note
    /// is really there.
    fn price(self) -> f64 {
        if self.steps().len() > 3 { 0.035 } else { 0.0 }
    }
}

/// A chord over a stretch of the recording, in seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chord {
    pub start: f64,
    pub end: f64,
    /// Pitch class of the root, C = 0.
    pub root: u8,
    pub quality: Quality,
}

impl Chord {
    /// `Bbm7`, or `A#m7` when `flats` is not set.
    pub fn name(&self, flats: bool) -> String {
        let names = if flats { &FLAT } else { &SHARP };
        format!("{}{}", names[usize::from(self.root % 12)], self.quality.suffix())
    }
}

/// One of the three transforms a chroma is read from: the signal at a
/// rate, and for each bin that counts, the semitone it feeds and how much.
struct Register {
    signal: Vec<f32>,
    hop: usize,
    /// (bin, semitone above C1, weight).
    feeds: Vec<(usize, usize, f32)>,
}

impl Register {
    /// `signal` (at [`CHROMA_RATE`]) slowed `slower` times, reading the
    /// semitones from `from` hertz up to `to`.
    fn new(signal: &[f32], slower: usize, from: f64, to: f64, tuning: f64, semitones: usize) -> Self {
        let rate = CHROMA_RATE / slower as f64;
        let signal = if slower == 1 { signal.to_vec() } else { resample(signal, CHROMA_RATE, rate) };
        let lowest = LOWEST_HZ * 2f64.powf(tuning / 12.0);
        let mut feeds = Vec::new();
        for bin in 1..SIZE / 2 {
            let hertz = bin as f64 * rate / SIZE as f64;
            if hertz < from || hertz >= to {
                continue;
            }
            let pitch = 12.0 * (hertz / lowest).log2();
            let nearest = pitch.round();
            if nearest < 0.0 || nearest as usize >= semitones {
                continue;
            }
            // Full weight on the note itself, none half-way to the next.
            let weight = (std::f64::consts::PI * (pitch - nearest)).cos().powi(2);
            // A long window gathers a low note's energy for longer: weigh
            // each register as a transform whose windows grow with the
            // wavelength would, the lows a little over the highs.
            let tilt = 261.63 / hertz;
            feeds.push((bin, nearest as usize, (weight * tilt) as f32));
        }
        Self { signal, hop: CHROMA_HOP / slower, feeds }
    }

    /// Adds this register's energy per semitone to `out` (frames x semitones).
    fn read(&self, out: &mut Frames) {
        let count = out.len();
        transform(&self.signal, SIZE, self.hop, |frame, re, im| {
            if frame >= count {
                return;
            }
            let row = out.row_mut(frame);
            for &(bin, semitone, weight) in &self.feeds {
                row[semitone] += weight * (re[bin] * re[bin] + im[bin] * im[bin]);
            }
        });
    }
}

/// The level of each semitone above C1 over time, `octaves` octaves of
/// them, read at `tuning` semitones from A = 440 Hz.
fn semitone_levels(signal: &[f32], tuning: f64, octaves: usize) -> Frames {
    let semitones = 12 * octaves;
    let top = LOWEST_HZ * 2f64.powf(tuning / 12.0) * 2f64.powf((semitones as f64 - 0.5) / 12.0);
    let bottom = LOWEST_HZ * 2f64.powf((tuning - 0.5) / 12.0);
    let count = frames(signal.len(), CHROMA_HOP);
    let mut energy = Frames::new(semitones, count);
    // The bottom octave, the two above it, and the rest.
    for (slower, from, to) in [(8, bottom, 62.0), (4, 62.0, 250.0), (1, 250.0, top)] {
        if from < top {
            Register::new(signal, slower, from, to.min(top), tuning, semitones).read(&mut energy);
        }
    }
    energy.data.iter_mut().for_each(|value| *value = value.sqrt());
    energy
}

fn fold(levels: &Frames) -> Frames {
    let mut chroma = Frames::new(12, levels.len());
    for frame in 0..levels.len() {
        let row = levels.row(frame);
        let out = chroma.row_mut(frame);
        for (semitone, &level) in row.iter().enumerate() {
            out[semitone % 12] += level;
        }
    }
    chroma
}

/// How far a recording sits from A = 440 Hz, in semitones (-0.5 to 0.5):
/// where the peaks of its spectrum lie between the notes of the tempered
/// scale, most of them.
pub fn tuning_of(signal: &[f32]) -> f64 {
    let mut peaks: Vec<(f64, f32)> = Vec::new();
    let mut magnitude = vec![0.0f32; SIZE / 2 + 1];
    let (low, high) = ((150.0 * SIZE as f64 / CHROMA_RATE) as usize, (4000.0 * SIZE as f64 / CHROMA_RATE) as usize);
    // Every fourth frame says as much as every frame.
    transform(signal, SIZE, CHROMA_HOP * 4, |_, re, im| {
        for ((slot, re), im) in magnitude.iter_mut().zip(re).zip(im) {
            *slot = (re * re + im * im).sqrt();
        }
        for bin in low.max(1)..high {
            let (before, here, after) = (magnitude[bin - 1], magnitude[bin], magnitude[bin + 1]);
            if here > before && here >= after && here > 0.0 {
                // A parabola through the peak and its neighbours.
                let curve = before - 2.0 * here + after;
                let shift = if curve.abs() > 1e-12 { 0.5 * (before - after) / curve } else { 0.0 };
                let hertz = (bin as f64 + f64::from(shift)) * CHROMA_RATE / SIZE as f64;
                peaks.push((hertz, here - 0.25 * (before - after) * shift));
            }
        }
    });
    if peaks.is_empty() {
        return 0.0;
    }
    let levels: Vec<f64> = peaks.iter().map(|peak| f64::from(peak.1)).collect();
    let threshold = median(&levels);
    let mut counts = [0u32; 100];
    for &(hertz, level) in &peaks {
        if f64::from(level) < threshold {
            continue;
        }
        let pitch = 12.0 * (hertz / 440.0).log2();
        let residual = pitch - pitch.round();
        counts[(((residual + 0.5) * 100.0) as usize).min(99)] += 1;
    }
    let best = (0..100).max_by(|&a, &b| counts[a].cmp(&counts[b]).then(b.cmp(&a))).unwrap_or(50);
    best as f64 / 100.0 - 0.5
}

/// Pitch-class energy over time (frames x 12, C first) of a mono signal at
/// [`RATE`], and the tuning it was read at, in cents away from A = 440 Hz.
pub fn chroma_of(signal: &[f32]) -> (Frames, f64) {
    let low = resample(signal, RATE, CHROMA_RATE);
    let peak = low.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if (low.len() as f64) < CHROMA_RATE || peak < 1e-4 {
        return (Frames::new(12, (low.len() / CHROMA_HOP).max(1)), 0.0);
    }
    let tuning = tuning_of(&low);
    (fold(&semitone_levels(&low, tuning, 6)), (tuning * 1000.0).round() / 10.0)
}

/// The pitch classes of a bass alone: the three octaves from C1, read at
/// `cents` from A = 440 Hz.
pub fn bass_chroma_of(signal: &[f32], cents: f64) -> Option<Frames> {
    let low = resample(signal, RATE, CHROMA_RATE);
    let peak = low.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if (low.len() as f64) < CHROMA_RATE || peak <= 1e-3 {
        return None;
    }
    Some(fold(&semitone_levels(&low, cents / 100.0, 3)))
}

fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let (mean_a, mean_b) = (a.iter().sum::<f64>() / 12.0, b.iter().sum::<f64>() / 12.0);
    let (mut both, mut only_a, mut only_b) = (0.0, 0.0, 0.0);
    for index in 0..12 {
        let (da, db) = (a[index] - mean_a, b[index] - mean_b);
        both += da * db;
        only_a += da * da;
        only_b += db * db;
    }
    if only_a <= 0.0 || only_b <= 0.0 { 0.0 } else { both / (only_a * only_b).sqrt() }
}

/// The key a chroma is most likely in, with the three runners-up.
pub fn key_of(chroma: &Frames) -> KeyReading {
    // Loud passages say more about the key than quiet ones, but not in
    // proportion: the square root keeps one loud chorus from deciding it.
    let mut profile = [0.0f64; 12];
    for frame in 0..chroma.len() {
        let row = chroma.row(frame);
        let weight = f64::from(row.iter().sum::<f32>()).sqrt();
        for (slot, &value) in profile.iter_mut().zip(row) {
            *slot += f64::from(value) * weight;
        }
    }
    if profile.iter().sum::<f64>() <= 0.0 {
        return KeyReading { key: Key { tonic: 0, minor: false }, score: 0.0, confidence: 0.0, alternates: Vec::new() };
    }
    let mut scores = [0.0f64; 24];
    for (major, minor) in &PROFILES {
        for tonic in 0..12 {
            let turned =
                |shape: &[f64; 12]| -> [f64; 12] { std::array::from_fn(|index| shape[(index + 12 - tonic) % 12]) };
            scores[tonic] += correlation(&profile, &turned(major)) / PROFILES.len() as f64;
            scores[12 + tonic] += correlation(&profile, &turned(minor)) / PROFILES.len() as f64;
        }
    }
    let mut order: Vec<usize> = (0..24).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then(a.cmp(&b)));
    let key_at = |code: usize| Key { tonic: (code % 12) as u8, minor: code >= 12 };
    let round = |value: f64| (value * 1000.0).round() / 1000.0;
    let key = key_at(order[0]);
    // A whole scale in common makes a key and its relative a near-tie in
    // most music, and both are useful answers: the margin is to the next.
    let relative = key.relative();
    let rival = order[1..].iter().copied().find(|&code| key_at(code) != relative).unwrap_or(order[1]);
    let margin = scores[order[0]] - scores[rival];
    KeyReading {
        key,
        score: round(scores[order[0]]),
        confidence: ((margin / 0.15).clamp(0.0, 1.0) * 100.0).round() / 100.0,
        alternates: order[1..4]
            .iter()
            .map(|&code| KeyScore { key: key_at(code), score: round(scores[code]) })
            .collect(),
    }
}

/// One chroma for each stretch between two beats (stretches x 12), each
/// the median of its frames: robust to the passing notes between chord
/// changes.
pub fn beat_chroma(chroma: &Frames, beats: &[f64]) -> Frames {
    let total = chroma.len();
    let at: Vec<usize> =
        beats.iter().map(|&time| ((time * CHROMA_RATE / CHROMA_HOP as f64).max(0.0) as usize).min(total)).collect();
    let mut synced = Frames::new(12, at.len().saturating_sub(1));
    let mut column = Vec::new();
    for index in 0..at.len().saturating_sub(1) {
        let (start, stop) = (at[index], at[index + 1].max(at[index] + 1).min(total));
        if start >= total {
            continue;
        }
        for class in 0..12 {
            column.clear();
            column.extend((start..stop).map(|frame| f64::from(chroma.row(frame)[class])));
            synced.row_mut(index)[class] = median(&column) as f32;
        }
    }
    synced
}

/// A chord for each stretch of beats.
///
/// Each beat is scored against every chord shape; the bass, when it is had
/// apart, votes for the root; then the most likely path is taken through
/// the whole piece, with a price on every change so that it does not
/// flicker. Stretches with little to hear have no chord and are left out.
pub fn chords_of(chroma: &Frames, bass: Option<&Frames>, beats: &[f64]) -> Vec<Chord> {
    if beats.len() < 2 {
        return Vec::new();
    }
    let synced = beat_chroma(chroma, beats);
    let count = synced.len();
    let energy: Vec<f64> = (0..count).map(|beat| f64::from(synced.row(beat).iter().sum::<f32>())).collect();
    let floor = 0.02 * percentile(&energy, 95.0);
    let bass = bass.filter(|bass| !bass.is_empty()).map(|bass| beat_chroma(bass, beats));

    let labels: Vec<(u8, Quality)> =
        Quality::ALL.iter().flat_map(|&quality| (0..12).map(move |root| (root, quality))).collect();
    let templates: Vec<[f64; 12]> = labels
        .iter()
        .map(|&(root, quality)| {
            let mut shape = [0.0f64; 12];
            let steps = quality.steps();
            for step in steps {
                shape[usize::from((root + step) % 12)] = 1.0 / (steps.len() as f64).sqrt();
            }
            shape
        })
        .collect();
    let states = labels.len() + 1;
    let score_at = |beat: usize| -> Vec<f64> {
        let row = synced.row(beat);
        let length = row.iter().map(|&value| f64::from(value).powi(2)).sum::<f64>().sqrt().max(1e-9);
        let lows = bass.as_ref().map(|bass| {
            let row = bass.row(beat);
            let total = f64::from(row.iter().sum::<f32>()).max(1e-9);
            row.iter().map(|&value| f64::from(value) / total).collect::<Vec<f64>>()
        });
        let mut scores: Vec<f64> = labels
            .iter()
            .zip(&templates)
            .map(|(&(root, quality), template)| {
                let fit: f64 = template.iter().zip(row).map(|(shape, &value)| shape * f64::from(value) / length).sum();
                fit - quality.price() + lows.as_ref().map_or(0.0, |lows| 0.12 * lows[usize::from(root)])
            })
            .collect();
        // A last state for "no chord": chosen where there is little to hear.
        scores.push(if energy[beat] <= floor { 2.0 } else { 0.35 });
        scores
    };

    // The most likely path, paying `CHANGE` for each change of chord.
    const CHANGE: f64 = 0.22;
    let mut best = score_at(0);
    let mut back = vec![0usize; states * count];
    for beat in 1..count {
        let leader = (0..states).max_by(|&a, &b| best[a].total_cmp(&best[b]).then(b.cmp(&a))).unwrap_or(0);
        let switch = best[leader] - CHANGE;
        let scores = score_at(beat);
        for state in 0..states {
            let from = if switch > best[state] { leader } else { state };
            back[beat * states + state] = from;
            let carried = if switch > best[state] { switch } else { best[state] };
            best[state] = carried + scores[state];
        }
    }
    let mut path = vec![0usize; count];
    if count > 0 {
        path[count - 1] = (0..states).max_by(|&a, &b| best[a].total_cmp(&best[b]).then(b.cmp(&a))).unwrap_or(0);
        for beat in (1..count).rev() {
            path[beat - 1] = back[beat * states + path[beat]];
        }
    }

    let round = |value: f64| (value * 10_000.0).round() / 10_000.0;
    // What a run of beats was read as, from where to where.
    type Stretch = (Option<(u8, Quality)>, f64, f64);
    let mut chords: Vec<Stretch> = Vec::new();
    for (beat, &state) in path.iter().enumerate() {
        let label = labels.get(state).copied();
        match chords.last_mut() {
            Some(last) if last.0 == label => last.2 = round(beats[beat + 1]),
            _ => chords.push((label, round(beats[beat]), round(beats[beat + 1]))),
        }
    }
    chords
        .into_iter()
        .filter_map(|(label, start, end)| label.map(|(root, quality)| Chord { start, end, root, quality }))
        .collect()
}

/// Tuning, key and chords of one recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Harmony {
    pub key: KeyReading,
    /// Cents from A = 440 Hz.
    pub tuning_cents: f64,
    pub chords: Vec<Chord>,
}

/// Reads the harmony of `pitched` (mono, at [`RATE`]: everything but the
/// drums, where they are had apart), with the bass alone voting for chord
/// roots when it is given, over the beats at `beats` seconds.
pub fn analyse(pitched: &[f32], bass: Option<&[f32]>, beats: &[f64]) -> Harmony {
    let (chroma, cents) = chroma_of(pitched);
    let key = key_of(&chroma);
    let bass = bass.and_then(|bass| bass_chroma_of(bass, cents));
    Harmony { chords: chords_of(&chroma, bass.as_ref(), beats), key, tuning_cents: cents }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::testing::{hertz, silence, tone};

    /// Chords of four beats each at 120 beats a minute, as held notes, with
    /// the root an octave and two below as a bass.
    fn progression(chords: &[&[u8]], cents: f64) -> (Vec<f32>, Vec<f32>, Vec<f64>) {
        let seconds = 2.0 * chords.len() as f64;
        let (mut upper, mut bass) = (silence(seconds), silence(seconds));
        for (index, notes) in chords.iter().enumerate() {
            let at = 2.0 * index as f64;
            for &note in *notes {
                tone(&mut upper, at, 1.95, hertz(f64::from(note), cents), 0.2);
            }
            tone(&mut bass, at, 1.95, hertz(f64::from(notes[0]) - 24.0, cents), 0.4);
        }
        let beats = (0..=4 * chords.len()).map(|beat| beat as f64 * 0.5).collect();
        (upper, bass, beats)
    }

    #[test]
    fn the_chords_of_a_progression_are_read_with_their_sevenths() {
        // C, Am, F, G7 and back, in C major. The bass is had apart and
        // votes for the roots; what is read is the chords above it.
        let shapes: [&[u8]; 6] =
            [&[60, 64, 67], &[57, 60, 64], &[65, 69, 72], &[55, 59, 62, 65], &[60, 64, 67], &[62, 65, 69, 72]];
        let (upper, bass, beats) = progression(&shapes, 0.0);
        let harmony = analyse(&upper, Some(&bass), &beats);
        let names: Vec<String> = harmony.chords.iter().map(|chord| chord.name(false)).collect();
        assert_eq!(names, ["C", "Am", "F", "G7", "C", "Dm7"]);
        assert!((harmony.chords[1].start - 2.0).abs() < 1e-9 && (harmony.chords[1].end - 4.0).abs() < 1e-9);
        assert_eq!(harmony.key.key, Key { tonic: 0, minor: false });
        assert!(harmony.tuning_cents.abs() <= 3.0, "{}", harmony.tuning_cents);
        // The relative minor is among the runners-up.
        assert!(harmony.key.alternates.iter().any(|other| other.key == Key { tonic: 9, minor: true }));
    }

    #[test]
    fn a_seventh_is_not_named_over_a_root_that_drowns_it() {
        // The same G7 with a loud bass under it in the same signal: the
        // root is most of what is heard, and the chord is called by its triad.
        let shapes: [&[u8]; 2] = [&[60, 64, 67], &[55, 59, 62, 65]];
        let (upper, bass, beats) = progression(&shapes, 0.0);
        let mix: Vec<f32> = upper.iter().zip(&bass).map(|(a, b)| a + b).collect();
        let names: Vec<String> = analyse(&mix, None, &beats).chords.iter().map(|chord| chord.name(false)).collect();
        assert_eq!(names, ["C", "G"]);
    }

    #[test]
    fn chords_are_read_without_a_bass_and_a_minor_key_is_told_from_its_relative() {
        // Am, Dm, E, Am: A minor, with the leading note that C major has not.
        let shapes: [&[u8]; 4] = [&[57, 60, 64], &[62, 65, 69], &[52, 56, 59], &[57, 60, 64]];
        let (upper, _, beats) = progression(&shapes, 0.0);
        let harmony = analyse(&upper, None, &beats);
        let names: Vec<String> = harmony.chords.iter().map(|chord| chord.name(false)).collect();
        assert_eq!(names, ["Am", "Dm", "E", "Am"]);
        assert_eq!(harmony.key.key.name(), "Am");
    }

    #[test]
    fn a_recording_that_sits_between_the_notes_is_read_at_its_own_tuning() {
        let shapes: [&[u8]; 3] = [&[63, 67, 70], &[68, 72, 75], &[70, 74, 77]];
        let (upper, bass, beats) = progression(&shapes, -32.0);
        let mix: Vec<f32> = upper.iter().zip(&bass).map(|(a, b)| a + b).collect();
        let harmony = analyse(&mix, Some(&bass), &beats);
        assert!((harmony.tuning_cents + 32.0).abs() <= 4.0, "{}", harmony.tuning_cents);
        let names: Vec<String> = harmony.chords.iter().map(|chord| chord.name(true)).collect();
        assert_eq!(names, ["Eb", "Ab", "Bb"]);
        assert_eq!(harmony.key.key.name(), "Eb");
        assert!(harmony.key.key.uses_flats());
    }

    #[test]
    fn silence_has_no_chords_and_no_key_to_speak_of() {
        let harmony = analyse(&silence(4.0), None, &[0.0, 1.0, 2.0, 3.0, 4.0]);
        assert!(harmony.chords.is_empty());
        assert_eq!(harmony.key.confidence, 0.0);
        assert_eq!(harmony.tuning_cents, 0.0);
        let short = analyse(&silence(0.2), None, &[]);
        assert!(short.chords.is_empty());
    }

    #[test]
    fn keys_are_named_as_file_names_and_disc_jockeys_name_them() {
        let key = Key { tonic: 6, minor: true };
        assert_eq!((key.name().as_str(), key.tag().as_str(), key.camelot().as_str()), ("F#m", "F#min", "11A"));
        let key = Key { tonic: 8, minor: false };
        assert_eq!((key.name().as_str(), key.tag().as_str(), key.camelot().as_str()), ("Ab", "Abmaj", "4B"));
        assert!(key.uses_flats());
        assert_eq!(key.relative(), Key { tonic: 5, minor: true });
        assert_eq!(note_name(41, false), "F2");
        assert_eq!(note_name(42, true), "Gb2");
    }
}
