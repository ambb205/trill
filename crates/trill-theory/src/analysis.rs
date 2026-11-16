//! Reading music back: which key a passage is in and which chords it
//! spells, so a writer who cannot hear can still check the result.
//!
//! Both are estimates. The key comes from how long each pitch class sounds,
//! compared against the profile of every major and minor key. A key and its
//! relative (C major and A minor) share all their notes, so between those
//! two the music's first and last chords decide, and the runner-up is
//! always reported with the winner.

use crate::chord::{Chord, identify, identify_weighted};
use crate::pitch::{NoteName, pitch_class};
use crate::scale::{Scale, ScaleKind};
use trill_core::Note;

/// The most likely key of a passage.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyEstimate {
    pub key: Scale,
    /// How clearly the key beats the runner-up: 0.5 is a coin toss, above
    /// 0.9 there is little doubt.
    pub confidence: f64,
    pub runner_up: Scale,
}

/// How much each scale degree belongs to a major and a minor key; index 0
/// is the tonic. These follow Temperley's revision of the Krumhansl-Kessler
/// probe-tone profiles, which tells a key from its dominant far better than
/// the originals, with the two sevenths of the minor profile evened out so
/// that natural minor, the minor of most pop, fits as well as harmonic minor.
const MAJOR_PROFILE: [f64; 12] = [5.0, 2.0, 3.5, 2.0, 4.5, 4.0, 2.0, 4.5, 2.0, 3.5, 1.5, 4.0];
const MINOR_PROFILE: [f64; 12] = [5.0, 2.0, 3.5, 4.5, 2.0, 4.0, 2.0, 4.5, 3.5, 2.0, 2.5, 3.0];

/// The names keys are normally given: Db major rather than C# major, but
/// C# minor rather than Db minor.
const MAJOR_TONICS: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
const MINOR_TONICS: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "G#", "A", "Bb", "B"];

/// What starting or ending on a key's tonic is worth when choosing between
/// that key and its relative, on the scale of the profile correlation
/// (-1..1). A progression is heard in the key of the chord it sets out
/// from unless the notes clearly say otherwise; a single opening note says
/// much less, since tunes often start on the third or fifth.
const OPENING_CHORD: f64 = 0.25;
const OPENING_NOTE: f64 = 0.05;
const CLOSING: f64 = 0.1;

/// How far apart two keys' scores are when the better one is three to one
/// on; sets how quickly confidence rises with the margin.
const CONFIDENCE_SCALE: f64 = 0.08;

fn correlation(a: &[f64; 12], b: &[f64; 12]) -> f64 {
    let mean = |values: &[f64; 12]| values.iter().sum::<f64>() / 12.0;
    let (mean_a, mean_b) = (mean(a), mean(b));
    let (mut product, mut square_a, mut square_b) = (0.0, 0.0, 0.0);
    for index in 0..12 {
        let (da, db) = (a[index] - mean_a, b[index] - mean_b);
        product += da * db;
        square_a += da * da;
        square_b += db * db;
    }
    if square_a == 0.0 || square_b == 0.0 { 0.0 } else { product / (square_a * square_b).sqrt() }
}

/// Estimates the key of some notes. `None` when there are no notes.
pub fn estimate_key(notes: &[Note]) -> Option<KeyEstimate> {
    let scores = key_scores(notes);
    let (best, second) = (scores.first()?, scores.get(1)?);
    let confidence = 1.0 / (1.0 + (-(best.0 - second.0) / CONFIDENCE_SCALE).exp());
    Some(KeyEstimate { key: best.1, confidence, runner_up: second.1 })
}

/// Every major and minor key with how well it fits, best first.
fn key_scores(notes: &[Note]) -> Vec<(f64, Scale)> {
    let (Some(first_start), Some(last_end)) = (
        notes.iter().map(|note| note.start).min_by(f64::total_cmp),
        notes.iter().map(Note::end).max_by(f64::total_cmp),
    ) else {
        return Vec::new();
    };
    // Very long notes are capped so one drone cannot outvote a whole tune,
    // and very short ones still count as having been played.
    let mut weights = [0.0f64; 12];
    for note in notes {
        weights[pitch_class(note.pitch) as usize] += note.length.clamp(0.125, 8.0);
    }
    let mut scores: Vec<(f64, Scale)> = Vec::with_capacity(24);
    for tonic in 0..12u8 {
        // The passage as seen from this tonic.
        let mut rotated = [0.0f64; 12];
        for (degree, weight) in rotated.iter_mut().enumerate() {
            *weight = weights[(tonic as usize + degree) % 12];
        }
        for (profile, kind, names) in
            [(&MAJOR_PROFILE, ScaleKind::Major, &MAJOR_TONICS), (&MINOR_PROFILE, ScaleKind::Minor, &MINOR_TONICS)]
        {
            let name = NoteName::parse(names[tonic as usize]).expect("built-in tonic names parse");
            scores.push((correlation(&rotated, profile), Scale::new(name, kind)));
        }
    }
    scores.sort_by(|a, b| b.0.total_cmp(&a.0));

    // The notes alone cannot separate the best key from its relative; where
    // the music starts and ends can. Only that pair is touched, so the
    // evidence never promotes a key the notes do not support.
    let best = scores[0].1;
    if let Some(relative) = best.relative() {
        let opening = home_note(notes.iter().filter(|note| note.start - first_start < 1e-6));
        let closing = home_note(notes.iter().filter(|note| last_end - note.end() < 1e-6));
        let evidence = |key: &Scale| {
            let tonic = key.tonic.pitch_class();
            let opens = match opening {
                Some((home, true)) if home == tonic => OPENING_CHORD,
                Some((home, false)) if home == tonic => OPENING_NOTE,
                _ => 0.0,
            };
            opens + if closing.is_some_and(|(home, _)| home == tonic) { CLOSING } else { 0.0 }
        };
        let advantage = evidence(&relative) - evidence(&best);
        let is_relative =
            |key: &Scale| key.kind == relative.kind && key.tonic.pitch_class() == relative.tonic.pitch_class();
        if let Some((score, _)) = scores.iter_mut().find(|(_, key)| is_relative(key)) {
            *score += advantage;
        }
        scores.sort_by(|a, b| b.0.total_cmp(&a.0));
    }
    scores
}

/// The note a sonority points to as home, and whether it is a chord: the
/// root if the notes make a chord, otherwise the lowest of them.
fn home_note<'a>(notes: impl Iterator<Item = &'a Note>) -> Option<(u8, bool)> {
    let pitches: Vec<u8> = notes.map(|note| note.pitch).collect();
    match identify(&pitches) {
        Some(chord) => Some((chord.root.pitch_class(), true)),
        None => pitches.iter().min().map(|pitch| (pitch_class(*pitch), false)),
    }
}

/// How much a note that enters at the very end of a window counts toward
/// that window's chord.
const LEAD_IN_WEIGHT: f64 = 0.4;

/// The most windows [`read_chords`] reads in one call.
const MAX_WINDOWS: usize = 4096;

/// What sounds in one stretch of time.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Start of the stretch, in the notes' own time.
    pub start: f64,
    pub length: f64,
    /// The chord, when the notes make one.
    pub chord: Option<Chord>,
    /// The pitch classes the reading is based on, the bass first.
    pub pitch_classes: Vec<u8>,
}

/// Names the chord sounding in each `window` beats from `start` to `end`
/// (a bar, or half a bar, at a time).
///
/// Pitch classes are weighed by how long they sound within the window (see
/// [`identify_weighted`]), so a melody over the chords or an arpeggio
/// spelling them out reads as the chord underneath. The bass is the lowest
/// note sounding as the window opens, and notes that only enter at its very
/// end count for little, so a walking bass or a pushed chord change does not
/// rename the chord. With a `key`, roots are spelled as that key spells them.
pub fn read_chords(notes: &[Note], window: f64, start: f64, end: f64, key: Option<&Scale>) -> Vec<Reading> {
    let mut readings = Vec::new();
    // Written so that a NaN anywhere reads nothing rather than looping.
    let sensible = window.is_finite() && window > 0.0 && end.is_finite() && start.is_finite() && end > start;
    if !sensible {
        return readings;
    }
    // A window far smaller than any note value would only produce noise, and a great deal of it.
    let count = (((end - start) / window - 1e-9).ceil().max(1.0) as usize).min(MAX_WINDOWS);
    for index in 0..count {
        let from = start + index as f64 * window;
        let to = (from + window).min(end);
        // How long each pitch class sounds here, and its lowest pitch.
        let mut weights = [0.0f64; 12];
        let mut lowest = [u8::MAX; 12];
        // The lowest note sounding as the window opens, allowing for a
        // strummed or humanized entry.
        let opening = from + (window * 0.125).min(0.25);
        let mut downbeat_bass: Option<u8> = None;
        for note in notes {
            let overlap = note.end().min(to) - note.start.max(from);
            if overlap > 1e-9 {
                let class = pitch_class(note.pitch) as usize;
                // What enters in the last quarter of a window leads into
                // the next one: an approach note, a pickup, a pushed chord.
                let leading = note.start > from + (to - from) * 0.75 - 1e-9;
                weights[class] += overlap * if leading { LEAD_IN_WEIGHT } else { 1.0 };
                lowest[class] = lowest[class].min(note.pitch);
                if note.start <= opening {
                    downbeat_bass = Some(downbeat_bass.map_or(note.pitch, |bass| bass.min(note.pitch)));
                }
            }
        }
        // A note counts toward the reading if it is heard at least a tenth
        // as long as the most-heard one.
        let strongest = weights.iter().copied().fold(0.0, f64::max);
        let mut sounding: Vec<usize> =
            (0..12).filter(|class| weights[*class] > 0.0 && weights[*class] >= strongest * 0.1).collect();
        sounding.sort_by_key(|class| lowest[*class]);
        // The bass of a chord is what is underneath when it arrives; where
        // a bass line walks to afterwards does not rename the chord. If
        // nothing sounds at the start, the lowest note heard stands in.
        let bass = downbeat_bass.map(pitch_class).or(sounding.first().map(|class| *class as u8));
        let chord = identify_weighted(&weights, bass).map(|chord| match key {
            Some(key) => chord.spelled_in(key),
            None => chord,
        });
        readings.push(Reading {
            start: from,
            length: to - from,
            chord,
            pitch_classes: sounding.into_iter().map(|class| class as u8).collect(),
        });
    }
    readings
}

/// The overall shape of a part.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Summary {
    pub note_count: usize,
    pub lowest: u8,
    pub highest: u8,
    /// The most notes sounding at once.
    pub polyphony: usize,
    /// Where the first note starts and the last one ends.
    pub start: f64,
    pub end: f64,
}

/// Summarises some notes. `None` when there are none.
pub fn summarize(notes: &[Note]) -> Option<Summary> {
    let lowest = notes.iter().map(|note| note.pitch).min()?;
    let highest = notes.iter().map(|note| note.pitch).max()?;
    let start = notes.iter().map(|note| note.start).min_by(f64::total_cmp)?;
    let end = notes.iter().map(Note::end).max_by(f64::total_cmp)?;
    // Sweep the note boundaries; an end at the same instant as a start comes first.
    let mut edges: Vec<(f64, i32)> = notes.iter().flat_map(|note| [(note.start, 1), (note.end(), -1)]).collect();
    edges.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let (mut sounding, mut polyphony) = (0i32, 0i32);
    for (_, change) in edges {
        sounding += change;
        polyphony = polyphony.max(sounding);
    }
    Some(Summary { note_count: notes.len(), lowest, highest, polyphony: polyphony as usize, start, end })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arpeggio::Arpeggio;
    use crate::bass::{BassLine, BassStyle, bass_line};
    use crate::melody::Phrase;
    use crate::progression::{Comping, Progression, comp};
    use crate::voicing::{VoicingSpec, VoicingStyle};
    use trill_core::TimeSignature;

    fn four() -> TimeSignature {
        TimeSignature::default()
    }

    fn chords(text: &str, style: VoicingStyle) -> Vec<Note> {
        let progression = Progression::parse(text, four(), None).unwrap();
        let voicings = progression.voice(&VoicingSpec { style, ..VoicingSpec::default() });
        comp(&progression, &voicings, &Comping::default())
    }

    fn melody(text: &str, key: &str) -> Vec<Note> {
        let key = Scale::parse(key).unwrap();
        Phrase::parse(text, Some(&key), 4, four()).unwrap().notes(1.0, &Default::default(), four())
    }

    fn key_of(notes: &[Note]) -> String {
        estimate_key(notes).unwrap().key.name()
    }

    #[test]
    fn keys_of_common_progressions() {
        let cases = [
            ("C | F | G | C", "C major"),
            ("Dm7 | G7 | Cmaj7 | Cmaj7", "C major"),
            ("C | Am | F | G", "C major"),
            ("C | G | Am | F", "C major"),
            ("G | D | Em | C", "G major"),
            ("Eb | Ab | Bb | Eb", "Eb major"),
            ("A | D | E | A", "A major"),
            ("F | Bb | C7 | F", "F major"),
            ("Bbmaj7 | Gm7 | Cm7 | F7", "Bb major"),
            ("Am | Dm | E7 | Am", "A minor"),
            ("Am | F | C | G", "A minor"),
            ("Am | G | F | E", "A minor"),
            ("Dm | Bb | C | Dm", "D minor"),
            ("F#m | D | A | E", "F# minor"),
            ("Cm | Ab | Eb | Bb", "C minor"),
            ("Em | C | G | D", "E minor"),
            ("C#m | A | E | B", "C# minor"),
            ("Gm | Cm | D7 | Gm", "G minor"),
        ];
        for style in [VoicingStyle::Close, VoicingStyle::Pad] {
            for (progression, key) in cases {
                assert_eq!(key_of(&chords(progression, style)), key, "{progression} voiced {}", style.name());
            }
        }
    }

    #[test]
    fn keys_of_melodies() {
        // Ode to Joy, in D.
        let ode = melody(
            "3 3 4 5 | 5 4 3 2 | 1 1 2 3 | 3:1.5 2:0.5 2:2 | 3 3 4 5 | 5 4 3 2 | 1 1 2 3 | 2:1.5 1:0.5 1:2",
            "D major",
        );
        assert_eq!(key_of(&ode), "D major");
        // Frere Jacques, in F.
        let jacques = melody(
            "1 2 3 1 | 1 2 3 1 | 3 4 5:2 | 3 4 5:2 | :0.5 5 6 5 4 3:1 1:1 | 5 6 5 4 3:1 1:1 | :1 1 5, 1:2 | 1 5, 1:2",
            "F major",
        );
        assert_eq!(key_of(&jacques), "F major");
        // Greensleeves, in A minor, with its raised sixth and seventh.
        let greensleeves = Phrase::parse(
            "A4:1 | C5:2 D5:1 | E5:1.5 F#5:0.5 E5:1 | D5:2 B4:1 | G4:1.5 A4:0.5 B4:1 | C5:2 A4:1 | A4:1.5 G#4:0.5 A4:1 | \
             B4:2 G#4:1 | E4:2 A4:1 | C5:2 D5:1 | E5:1.5 F#5:0.5 E5:1 | D5:2 B4:1 | G4:1.5 A4:0.5 B4:1 | C5:1.5 B4:0.5 A4:1 | \
             G#4:1.5 F#4:0.5 G#4:1 | A4:3",
            None,
            4,
            TimeSignature::new(3, 4).unwrap(),
        );
        // The pickup makes the first "bar" short; the phrase is checked from the first full bar.
        let greensleeves = greensleeves.unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(key_of(&greensleeves.notes(1.0, &Default::default(), four())), "A minor");
        // A natural-minor line that never touches a leading tone.
        let aeolian =
            melody("1 3 5 4 | 3:2 1 7, | 6, 7, 1 3 | 2:2 1:2 | 5 4 3 2 | 3:2 1 7, | 6, 1 7, 5, | 1:4", "E minor");
        assert_eq!(key_of(&aeolian), "E minor");
        // A bass line is enough.
        let progression = Progression::parse("Dm7 | G7 | Cmaj7 | A7 | Dm7 | G7 | Cmaj7 | Cmaj7", four(), None).unwrap();
        let walking = bass_line(&progression, &BassLine { style: BassStyle::Walking, ..BassLine::default() });
        assert_eq!(key_of(&walking), "C major");
    }

    #[test]
    fn confidence_and_the_runner_up() {
        // An unmistakable cadence.
        let clear = estimate_key(&chords("C | F | G7 | C", VoicingStyle::Close)).unwrap();
        assert!(clear.confidence > 0.9, "{clear:?}");
        // The four chords shared by C major and A minor: the answer leans on where it starts, and says so.
        let shared = estimate_key(&chords("Am | F | C | G", VoicingStyle::Close)).unwrap();
        assert_eq!(shared.runner_up.name(), "C major", "{shared:?}");
        assert!(shared.confidence < clear.confidence, "{shared:?}");
        let other_way = estimate_key(&chords("C | G | Am | F", VoicingStyle::Close)).unwrap();
        assert_eq!(other_way.key.name(), "C major", "the same chords starting from C");
        assert!(other_way.confidence > shared.confidence, "{other_way:?}");
        // One note: a guess, and it knows it.
        let single = estimate_key(&melody("1:4", "G major")).unwrap();
        assert!((0.5..0.7).contains(&single.confidence), "{single:?}");
        assert_eq!(estimate_key(&[]), None);
        // Keys are named the usual way.
        assert_eq!(key_of(&chords("Db | Gb | Ab | Db", VoicingStyle::Close)), "Db major");
        assert_eq!(key_of(&chords("G#m | E | B | F#", VoicingStyle::Close)), "G# minor");
        assert_eq!(key_of(&chords("Bbm | Gb | Db | Ab", VoicingStyle::Close)), "Bb minor");
    }

    fn names(readings: &[Reading]) -> Vec<String> {
        readings.iter().map(|reading| reading.chord.as_ref().map_or("-".to_string(), Chord::name)).collect()
    }

    #[test]
    fn written_chords_read_back_by_name() {
        let charts = [
            ("Am9 | Fmaj7 | C | E7", vec!["Am9", "Fmaj7", "C", "E7"]),
            ("Dm7 | G7 | Cmaj7 | A7", vec!["Dm7", "G7", "Cmaj7", "A7"]),
            ("Cm7 | Fm7 | Bb7 | Ebmaj7", vec!["Cm7", "Fm7", "Bb7", "Ebmaj7"]),
            ("C | Csus4 | G7sus4 | Cadd9", vec!["C", "Csus4", "G7sus4", "Cadd9"]),
            ("Bm7b5 | E7 | Am6 | N.C.", vec!["Bm7b5", "E7", "Am6", "-"]),
        ];
        for style in [VoicingStyle::Close, VoicingStyle::Pad, VoicingStyle::Open] {
            for (chart, expected) in &charts {
                // A bass note under every voicing, so each chord is heard in root position.
                let progression = Progression::parse(chart, four(), None).unwrap();
                let spec = VoicingSpec { style, bass: crate::voicing::Bass::Root, voices: 5, ..VoicingSpec::default() };
                let notes = comp(&progression, &progression.voice(&spec), &Comping::default());
                assert_eq!(
                    names(&read_chords(&notes, 4.0, 0.0, 16.0, None)),
                    *expected,
                    "{chart} voiced {}",
                    style.name()
                );
            }
        }
    }

    #[test]
    fn half_bars_inversions_and_keys() {
        let notes = chords("C G/B | Am F", VoicingStyle::Pad);
        assert_eq!(names(&read_chords(&notes, 2.0, 0.0, 8.0, None)), ["C", "G/B", "Am", "F"]);
        // A whole-bar window hears both chords at once and may find no name; half bars are the remedy.
        assert_eq!(read_chords(&notes, 4.0, 0.0, 8.0, None).len(), 2);
        // In a sharp key, roots are sharps.
        let key = Scale::parse("E major").unwrap();
        let notes = chords("E | G#m | C#m | B", VoicingStyle::Pad);
        assert_eq!(names(&read_chords(&notes, 4.0, 0.0, 16.0, Some(&key))), ["E", "G#m", "C#m", "B"]);
        assert_eq!(names(&read_chords(&notes, 4.0, 0.0, 16.0, None)), ["E", "Abm", "Dbm", "B"]);
        // The window can stop short, and an empty range reads nothing.
        let partial = read_chords(&notes, 4.0, 4.0, 10.0, Some(&key));
        assert_eq!(names(&partial), ["G#m", "C#m"]);
        assert_eq!((partial[1].start, partial[1].length), (8.0, 2.0));
        assert!(read_chords(&notes, 4.0, 4.0, 4.0, None).is_empty());
        assert!(read_chords(&notes, 0.0, 0.0, 4.0, None).is_empty());
    }

    #[test]
    fn arpeggios_and_melodies_read_as_the_chord_underneath() {
        let progression = Progression::parse("Dm7 | G7 | Cmaj7 | Am7", four(), None).unwrap();
        let voicings = progression.voice(&VoicingSpec::default());
        let arpeggio = Arpeggio { octaves: 2, ..Arpeggio::default() }.notes(&progression, &voicings);
        let bass = bass_line(&progression, &BassLine::default());
        let both: Vec<Note> = arpeggio.iter().chain(&bass).copied().collect();
        assert_eq!(names(&read_chords(&both, 4.0, 0.0, 16.0, None)), ["Dm7", "G7", "Cmaj7", "Am7"]);

        // Chords with a busy melody on top: the passing notes do not rename them.
        let mut song = chords("C | F | G | C", VoicingStyle::Pad);
        song.extend(melody(":0.5 5 6 5 4 3 4 5 3 | 6 5 6 7 1':1 6:1 | 7 1' 2' 1' 7 6 5:1 | 5 4 3 2 1:2", "C major"));
        assert_eq!(names(&read_chords(&song, 4.0, 0.0, 16.0, None)), ["C", "F", "G", "C"]);
        assert_eq!(key_of(&song), "C major");
    }

    #[test]
    fn a_walking_bass_does_not_rename_the_chords() {
        // Comping in short stabs over a bass that moves on every beat: the
        // approach notes are as long as the chord tones and lower than the roots.
        let progression = Progression::parse("Dm7 | G7 | Cmaj7 | A7 | Dm7 | G7 | Cmaj7 | Cmaj7", four(), None).unwrap();
        let spec = VoicingSpec { style: VoicingStyle::Drop2, ..VoicingSpec::default() };
        let comping = Comping {
            rhythm: crate::progression::Rhythm::parse("charleston", four()).unwrap(),
            gate: 0.7,
            ..Comping::default()
        };
        let mut band = comp(&progression, &progression.voice(&spec), &comping);
        band.extend(bass_line(&progression, &BassLine { style: BassStyle::Walking, ..BassLine::default() }));
        assert_eq!(
            names(&read_chords(&band, 4.0, 0.0, 32.0, None)),
            ["Dm7", "G7", "Cmaj7", "A7", "Dm7", "G7", "Cmaj7", "Cmaj7"]
        );
        assert_eq!(key_of(&band), "C major");
    }

    #[test]
    fn single_notes_and_silence_have_no_chord() {
        let line = melody("1:4 | r:4 | 5:4", "C major");
        let readings = read_chords(&line, 4.0, 0.0, 12.0, None);
        assert_eq!(names(&readings), ["-", "-", "-"]);
        assert_eq!(readings[0].pitch_classes, [0]);
        assert!(readings[1].pitch_classes.is_empty());
        assert_eq!(readings[2].pitch_classes, [7]);
    }

    #[test]
    fn summaries_measure_range_and_density() {
        let notes = chords("C | F", VoicingStyle::Close);
        let summary = summarize(&notes).unwrap();
        assert_eq!(summary.note_count, 6);
        assert_eq!(summary.polyphony, 3, "the second chord starts as the first ends");
        assert_eq!((summary.start, summary.end), (0.0, 8.0));
        assert!(summary.lowest >= 52 && summary.highest <= 76);
        let line = melody("1:1 r 5 1'", "C major");
        let summary = summarize(&line).unwrap();
        assert_eq!((summary.lowest, summary.highest, summary.polyphony), (60, 72, 1));
        assert_eq!((summary.start, summary.end), (0.0, 4.0));
        assert_eq!(summarize(&[]), None);
    }
}
