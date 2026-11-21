//! Dynamics for a line, from its notes: where it breathes, where it swells
//! and where it is going.
//!
//! A wind or a string player does not hold one level through a line. The
//! sound starts with the breath or the bow, grows towards the high point of
//! the phrase, blooms through a long note and falls away into the pause
//! before the next phrase. Instruments modelled on that take it from a
//! controller (expression, breath), and are silent without one. This
//! module writes that controller's curve for a line, in a few breakpoints
//! a person can then move by hand.
//!
//! # How a line is read
//!
//! * **Phrases.** Notes belong to one phrase until there is a rest of at
//!   least [`Phrasing::phrase_gap`] beats. Between phrases the level is
//!   [`Phrasing::rest`]: the breath.
//! * **The peak.** A phrase is going to its highest note (the longer of
//!   two as high, the later of two as long); a phrase on one pitch is
//!   going to a point three fifths of the way through. The level rises
//!   into the peak, slowly at first, and falls away after it. The phrase
//!   with the highest peak of the line reaches [`Phrasing::high`]; the
//!   others stop short of it by as much as their peaks are lower.
//! * **Long notes.** A note of [`Phrasing::long_note`] beats or more swells
//!   through its first half and eases off again, by [`Phrasing::swell`].
//! * **Edges.** A phrase starts below where it is going and with a quick
//!   rise through its first note (the attack), and ends in a short fall to
//!   the level of the rest (the release).
//!
//! Chords are read as one line: what counts is when something sounds and
//! which is the highest note.

use trill_core::Note;
use trill_core::project::{AutomationPoint, Segment};

/// Times closer together than this are one time.
const EPSILON: f64 = 1e-6;
/// How much of the way from [`Phrasing::low`] to its peak a phrase starts
/// at, and ends at before its release.
const START_SHARE: f64 = 0.4;
const END_SHARE: f64 = 0.25;
/// How much of its level the attack begins at.
const ONSET_SHARE: f64 = 0.7;
/// The longest attack and release, in beats, and the most of the first and
/// last note each may take.
const ATTACK_BEATS: f64 = 0.125;
const RELEASE_BEATS: f64 = 0.25;
const EDGE_SHARE: f64 = 0.3;
/// Where in a long note its swell is fullest, and how much a full swell
/// adds, as a share of the range from low to high.
const BLOOM_AT: f64 = 0.55;
const BLOOM_SHARE: f64 = 0.3;
/// How far a peak sits into its note, as a share of the note and in beats
/// at most.
const PEAK_INTO_NOTE: f64 = 0.35;
const PEAK_INTO_BEATS: f64 = 1.0;
/// How much of the range from low to high the phrase with the lowest peak
/// of a line reaches.
const QUIETEST_PEAK: f64 = 0.6;
/// How the line bends on its way up and on its way down.
const RISE_BEND: f32 = 0.3;
const FALL_BEND: f32 = -0.3;

/// How a line is to be shaped. Levels are positions of the controller, 0
/// to 1.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Phrasing {
    /// The level between phrases: 0 for an instrument that falls silent as
    /// the player breathes.
    pub rest: f64,
    /// The quietest a sounding note gets.
    pub low: f64,
    /// The level of the line's highest point.
    pub high: f64,
    /// How much long notes swell, 0 (not at all) to 1.
    pub swell: f64,
    /// A rest of at least this many beats ends a phrase.
    pub phrase_gap: f64,
    /// A note of at least this many beats swells.
    pub long_note: f64,
}

impl Default for Phrasing {
    fn default() -> Self {
        Self { rest: 0.0, low: 0.45, high: 0.9, swell: 0.5, phrase_gap: 0.5, long_note: 2.0 }
    }
}

impl Phrasing {
    /// The same shaping with every level kept to 0..=1 and in order
    /// (`rest` at most `low`, `low` at most `high`), and lengths that are
    /// lengths.
    fn checked(self) -> Self {
        let level = |value: f64, fallback: f64| if value.is_finite() { value.clamp(0.0, 1.0) } else { fallback };
        let length = |value: f64, fallback: f64| if value.is_finite() && value > 0.0 { value } else { fallback };
        let defaults = Self::default();
        let high = level(self.high, defaults.high);
        let low = level(self.low, defaults.low).min(high);
        Self {
            rest: level(self.rest, defaults.rest).min(low),
            low,
            high,
            swell: level(self.swell, defaults.swell),
            phrase_gap: length(self.phrase_gap, defaults.phrase_gap),
            long_note: length(self.long_note, defaults.long_note),
        }
    }
}

/// One phrase of a shaped line.
#[derive(Clone, Debug, PartialEq)]
pub struct PhraseShape {
    /// Where its first note starts and its last note ends, in the beats
    /// the notes were given in.
    pub start: f64,
    pub end: f64,
    /// How many notes it has.
    pub notes: usize,
    /// Where the level is highest, the pitch it is going to there, and the
    /// level.
    pub peak_beat: f64,
    pub peak_pitch: u8,
    pub peak_level: f64,
}

/// A line's dynamics: the curve and how the line was read.
#[derive(Clone, Debug, PartialEq)]
pub struct Shaped {
    /// The curve, in the beats the notes were given in; values 0..=1.
    pub points: Vec<AutomationPoint>,
    pub phrases: Vec<PhraseShape>,
}

/// The notes of one phrase, by start.
struct Phrase<'a> {
    notes: &'a [Note],
    end: f64,
}

/// Divides `notes` (sorted by start) into phrases.
fn phrases(notes: &[Note], gap: f64) -> Vec<Phrase<'_>> {
    let mut found = Vec::new();
    let mut first = 0;
    let mut end = f64::NEG_INFINITY;
    for (index, note) in notes.iter().enumerate() {
        if index > first && note.start - end >= gap - EPSILON {
            found.push(Phrase { notes: &notes[first..index], end });
            first = index;
            end = f64::NEG_INFINITY;
        }
        end = end.max(note.end());
    }
    if first < notes.len() {
        found.push(Phrase { notes: &notes[first..], end });
    }
    found
}

/// The note a phrase is going to: its highest, the longer of two as high,
/// the later of two as long. `None` for a phrase on one pitch.
fn climax(notes: &[Note]) -> Option<&Note> {
    let lowest = notes.iter().map(|note| note.pitch).min()?;
    let peak = notes
        .iter()
        .max_by(|a, b| a.pitch.cmp(&b.pitch).then(a.length.total_cmp(&b.length)).then(a.start.total_cmp(&b.start)))?;
    (peak.pitch > lowest).then_some(peak)
}

/// Writes the dynamics of the line `notes` make: see the module
/// documentation. The notes may be in any order; ones that are not finite
/// or have no length are left out. An empty line has no curve.
pub fn shape(notes: &[Note], phrasing: &Phrasing) -> Shaped {
    let phrasing = phrasing.checked();
    let mut line: Vec<Note> = notes
        .iter()
        .filter(|note| note.start.is_finite() && note.length.is_finite() && note.length > 0.0)
        .copied()
        .collect();
    line.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    let found = phrases(&line, phrasing.phrase_gap);

    // How high each phrase's peak is among the line's: the highest reaches
    // `high`, the lowest stops well short of it.
    let peaks: Vec<u8> =
        found.iter().map(|phrase| phrase.notes.iter().map(|note| note.pitch).max().unwrap_or(0)).collect();
    let (lowest, highest) = (peaks.iter().copied().min().unwrap_or(0), peaks.iter().copied().max().unwrap_or(0));
    let span = phrasing.high - phrasing.low;

    let mut points: Vec<AutomationPoint> = Vec::new();
    let mut shapes = Vec::with_capacity(found.len());
    for (phrase, peak_pitch) in found.iter().zip(&peaks) {
        let (first, last) = (phrase.notes[0], phrase.notes[phrase.notes.len() - 1]);
        let (start, end) = (first.start, phrase.end);
        let standing =
            if highest > lowest { f64::from(peak_pitch - lowest) / f64::from(highest - lowest) } else { 1.0 };
        let peak_level = phrasing.low + span * (QUIETEST_PEAK + (1.0 - QUIETEST_PEAK) * standing);
        let reach = peak_level - phrasing.low;

        // The four corners of the arch: after the attack, the peak, before
        // the release, and the end.
        let attack = start + ATTACK_BEATS.min(first.length * EDGE_SHARE);
        let release = (end - RELEASE_BEATS.min(last.length * EDGE_SHARE)).max(attack);
        let peak_beat = match climax(phrase.notes) {
            Some(note) => note.start + (note.length * PEAK_INTO_NOTE).min(PEAK_INTO_BEATS),
            None => start + (end - start) * 0.6,
        }
        .clamp(attack, release);
        let (start_level, end_level) = (phrasing.low + reach * START_SHARE, phrasing.low + reach * END_SHARE);
        let arch = |beat: f64| {
            if beat <= peak_beat {
                let way =
                    if peak_beat > attack { ((beat - attack) / (peak_beat - attack)).clamp(0.0, 1.0) } else { 1.0 };
                start_level + (peak_level - start_level) * way
            } else {
                let way = if release > peak_beat {
                    ((beat - peak_beat) / (release - peak_beat)).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                peak_level + (end_level - peak_level) * way
            }
        };
        // What the long notes add: a rise to a point past their middle and
        // an easing off again.
        let long: Vec<&Note> = phrase.notes.iter().filter(|note| note.length >= phrasing.long_note - EPSILON).collect();
        let bloom = |beat: f64| -> f64 {
            long.iter()
                .map(|note| {
                    let apex = note.start + note.length * BLOOM_AT;
                    let way = if beat <= apex {
                        (beat - note.start) / (apex - note.start)
                    } else {
                        (note.end() - beat) / (note.end() - apex)
                    };
                    way.clamp(0.0, 1.0) * span * BLOOM_SHARE * phrasing.swell
                })
                .fold(0.0, f64::max)
        };
        let level = |beat: f64| (arch(beat) + bloom(beat)).clamp(phrasing.low.min(start_level), 1.0);

        // The moments at which the line turns.
        let mut turns = vec![attack, peak_beat, release];
        for note in &long {
            turns.extend([note.start, note.start + note.length * BLOOM_AT, note.end()]);
        }
        turns.retain(|beat| *beat >= attack - EPSILON && *beat <= release + EPSILON);
        turns.sort_by(f64::total_cmp);
        turns.dedup_by(|later, earlier| *later - *earlier < EPSILON);

        let from = points.len();
        if attack > start + EPSILON {
            points.push(AutomationPoint::new(start, level(attack) * ONSET_SHARE));
        }
        points.extend(turns.iter().map(|beat| AutomationPoint::new(*beat, level(*beat))));
        if end > release + EPSILON {
            points.push(AutomationPoint::new(end, phrasing.rest));
        }
        // Each stretch bends the way it is going; the last point holds the
        // level of the rest until the next phrase starts.
        let count = points.len();
        for index in from..count {
            points[index].shape = match points.get(index + 1) {
                Some(next) if next.value > points[index].value + EPSILON => Segment::Curve(RISE_BEND),
                Some(next) if next.value < points[index].value - EPSILON => Segment::Curve(FALL_BEND),
                Some(_) => Segment::Linear,
                None => Segment::Hold,
            };
        }
        if let Some(last) = points.last_mut() {
            // A phrase that ends on its release is at rest from there; one
            // too short for a release is put there.
            if end <= release + EPSILON {
                let beat = last.beat.max(end);
                points.push(AutomationPoint { beat, value: phrasing.rest, shape: Segment::Hold, id: 0 });
            } else {
                last.shape = Segment::Hold;
            }
        }
        // The peak as it came out, long notes and all.
        let top = points[from..].iter().max_by(|a, b| a.value.total_cmp(&b.value)).copied();
        shapes.push(PhraseShape {
            start,
            end,
            notes: phrase.notes.len(),
            peak_beat: top.map_or(peak_beat, |point| point.beat),
            peak_pitch: *peak_pitch,
            peak_level: top.map_or(peak_level, |point| point.value),
        });
    }
    // Before the first phrase the line is at rest.
    if let Some(first) = points.first().copied()
        && first.beat > EPSILON
        && first.value != phrasing.rest
    {
        points.insert(0, AutomationPoint { beat: 0.0, value: phrasing.rest, shape: Segment::Hold, id: 0 });
    }
    Shaped { points, phrases: shapes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::project::automation_value;

    fn note(pitch: u8, start: f64, length: f64) -> Note {
        Note { pitch, start, length, velocity: 100, ..Default::default() }
    }

    fn at(shaped: &Shaped, beat: f64) -> f64 {
        automation_value(&shaped.points, beat)
    }

    /// Two phrases: a rise to a long high note, a breath, and a lower
    /// answer.
    fn line() -> Vec<Note> {
        vec![
            note(60, 0.0, 1.0),
            note(62, 1.0, 1.0),
            note(64, 2.0, 1.0),
            note(67, 3.0, 4.0),
            note(65, 7.0, 1.0),
            // A beat of rest.
            note(62, 9.0, 1.0),
            note(64, 10.0, 1.0),
            note(60, 11.0, 2.0),
        ]
    }

    #[test]
    fn a_line_breathes_between_its_phrases_and_rises_into_its_peaks() {
        let shaped = shape(&line(), &Phrasing::default());
        assert_eq!(shaped.phrases.len(), 2);
        let (first, second) = (&shaped.phrases[0], &shaped.phrases[1]);
        assert_eq!((first.start, first.end, first.notes), (0.0, 8.0, 5));
        assert_eq!((second.start, second.end, second.notes), (9.0, 13.0, 3));
        assert_eq!((first.peak_pitch, second.peak_pitch), (67, 64));

        // The level is going to the long high note, and gets there in it.
        assert!((3.0..7.0).contains(&first.peak_beat), "{}", first.peak_beat);
        assert!(first.peak_level > 0.9, "the long note swells past the line's level: {}", first.peak_level);
        assert!(at(&shaped, 1.0) < at(&shaped, 2.0) && at(&shaped, 2.0) < at(&shaped, 3.0), "rising into it");
        assert!(at(&shaped, 7.5) < at(&shaped, 5.0), "and falling away after it");
        // The lower phrase stops short of the higher one's level.
        assert!(second.peak_level < 0.75 && second.peak_level > 0.45, "{}", second.peak_level);

        // The breath: nothing between the phrases, from the last note's end
        // to the next note's first frame, where the attack starts at once.
        assert_eq!((at(&shaped, 8.0), at(&shaped, 8.5), at(&shaped, 8.99)), (0.0, 0.0, 0.0));
        assert!(at(&shaped, 9.0) > 0.3, "the next phrase starts on its note: {}", at(&shaped, 9.0));
        assert!(at(&shaped, 9.125) > at(&shaped, 9.0), "with a quick rise through the attack");
        assert_eq!(at(&shaped, 13.0), 0.0);
        assert_eq!(at(&shaped, 40.0), 0.0, "and it stays at rest after the line");

        // A few points a person can move, each within the controller's
        // range, in order.
        assert!(shaped.points.len() <= 20, "{} points", shaped.points.len());
        assert!(shaped.points.iter().all(|point| (0.0..=1.0).contains(&point.value)));
        assert!(shaped.points.windows(2).all(|pair| pair[0].beat <= pair[1].beat));
        // Every sounding moment is above the quietest level.
        for note in line() {
            let middle = note.start + note.length / 2.0;
            assert!(at(&shaped, middle) >= 0.45, "at beat {middle}: {}", at(&shaped, middle));
        }
    }

    #[test]
    fn a_long_note_swells_and_eases_off() {
        let one = [note(60, 0.0, 8.0)];
        let shaped = shape(&one, &Phrasing::default());
        let (early, apex, late) = (at(&shaped, 0.5), at(&shaped, 4.4), at(&shaped, 7.5));
        assert!(apex > early + 0.1 && apex > late + 0.1, "{early} {apex} {late}");
        // Without swell it is the plain arch of a phrase on one pitch.
        let flat = shape(&one, &Phrasing { swell: 0.0, ..Phrasing::default() });
        assert!(at(&flat, 4.4) < apex - 0.05);
        // A short note is not long enough to swell.
        let short = shape(&[note(60, 0.0, 1.0)], &Phrasing::default());
        assert!(short.phrases[0].peak_level <= 0.9 + 1e-9);
        assert_eq!(at(&short, 1.0), 0.0);
        assert!(at(&short, 0.5) >= 0.45);
    }

    #[test]
    fn the_levels_and_the_phrasing_are_as_asked() {
        let quiet = Phrasing { rest: 0.2, low: 0.3, high: 0.5, ..Phrasing::default() };
        let shaped = shape(&line(), &quiet);
        assert_eq!(at(&shaped, 8.5), 0.2, "the level between phrases");
        let sounding: Vec<f64> = line().iter().map(|note| at(&shaped, note.start + note.length / 2.0)).collect();
        assert!(sounding.iter().all(|level| (0.3..=0.65).contains(level)), "{sounding:?}");

        // A longer gap makes one phrase of the two; a shorter one makes
        // every detached note a phrase.
        assert_eq!(shape(&line(), &Phrasing { phrase_gap: 2.0, ..Phrasing::default() }).phrases.len(), 1);
        let detached = [note(60, 0.0, 0.5), note(62, 1.0, 0.5), note(64, 2.0, 0.5)];
        assert_eq!(shape(&detached, &Phrasing::default()).phrases.len(), 3);
        assert_eq!(shape(&detached, &Phrasing { phrase_gap: 1.0, ..Phrasing::default() }).phrases.len(), 1);

        // Nonsense is read as the nearest sense.
        let odd = Phrasing { rest: 2.0, low: f64::NAN, high: 0.3, swell: -1.0, phrase_gap: 0.0, long_note: -3.0 };
        let shaped = shape(&line(), &odd);
        assert!(shaped.points.iter().all(|point| (0.0..=1.0).contains(&point.value)));
        assert_eq!(shaped.phrases.len(), 2);
    }

    #[test]
    fn chords_overlaps_and_odd_input_are_read_as_one_line() {
        // A chord under a melody note, notes out of order, and one that
        // overlaps the next: one phrase, going to the top note.
        let notes = [note(67, 2.0, 2.0), note(48, 0.0, 4.0), note(55, 0.0, 4.0), note(64, 0.0, 2.5)];
        let shaped = shape(&notes, &Phrasing::default());
        assert_eq!(shaped.phrases.len(), 1);
        assert_eq!((shaped.phrases[0].start, shaped.phrases[0].end, shaped.phrases[0].peak_pitch), (0.0, 4.0, 67));
        // Nothing in, nothing out; notes that are not notes are left out.
        assert_eq!(shape(&[], &Phrasing::default()), Shaped { points: vec![], phrases: vec![] });
        let broken = [note(60, f64::NAN, 1.0), note(60, 0.0, 0.0), note(62, 4.0, 1.0)];
        let shaped = shape(&broken, &Phrasing::default());
        assert_eq!(shaped.phrases.len(), 1);
        assert_eq!(
            shaped.points[0],
            AutomationPoint { beat: 0.0, value: 0.0, shape: Segment::Hold, id: 0 },
            "at rest until it starts"
        );
        // The same notes give the same curve.
        assert_eq!(shape(&line(), &Phrasing::default()), shape(&line(), &Phrasing::default()));
    }
}
