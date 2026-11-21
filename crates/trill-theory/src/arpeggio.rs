//! Arpeggios: chords played one note at a time.

use crate::chord::Chord;
use crate::error::{Error, Result, list};
use crate::expression::{Dynamics, MIN_LENGTH, Rng, clamp_velocity};
use crate::pitch::pitch_class;
use crate::progression::Progression;
use crate::timing::EPSILON;
use crate::voicing::Voicing;
use trill_core::Note;

/// The order an arpeggio runs through the notes of a chord.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArpOrder {
    Up,
    Down,
    /// Up then down, turning round without repeating the top and bottom
    /// notes (C E G E C E G E).
    UpDown,
    /// Up then down, playing the top and bottom notes twice (C E G G E C).
    UpDownInclusive,
    DownUp,
    /// In the order the chord is named: root, third, fifth, seventh,
    /// whichever octave each is voiced in.
    Played,
    /// A shuffled order that never repeats a note straight away; the same
    /// seed gives the same line.
    Random,
    /// Chord tones by number, counted from the lowest note: 1 is the bottom
    /// note, numbers past the top carry on into the next octave, 0 rests.
    /// `[1, 3, 2, 3]` over a triad is the Alberti figure.
    Pattern(Vec<usize>),
}

/// The named orders, with what each one is.
pub const ORDERS: &[(&str, &str)] = &[
    ("up", "lowest to highest, then again"),
    ("down", "highest to lowest"),
    ("up_down", "up then down without repeating the top and bottom notes"),
    ("up_down_inclusive", "up then down, top and bottom notes played twice"),
    ("down_up", "down then up without repeating the end notes"),
    ("played", "root, third, fifth, seventh... in the order the chord is named"),
    ("random", "shuffled, never the same note twice in a row; set by the seed"),
];

impl ArpOrder {
    pub fn parse(text: &str) -> Result<Self> {
        let name: String = text.trim().to_ascii_lowercase().chars().filter(|c| !matches!(c, '_' | '-' | ' ')).collect();
        Ok(match name.as_str() {
            "up" => ArpOrder::Up,
            "down" => ArpOrder::Down,
            "updown" => ArpOrder::UpDown,
            "updowninclusive" => ArpOrder::UpDownInclusive,
            "downup" => ArpOrder::DownUp,
            "played" | "asplayed" => ArpOrder::Played,
            "random" => ArpOrder::Random,
            _ => {
                return Err(Error::new(format!(
                    "'{text}' is not an arpeggio order; expected one of {}",
                    list(ORDERS.iter().map(|(name, _)| *name))
                )));
            }
        })
    }
}

/// How to arpeggiate.
#[derive(Clone, Debug, PartialEq)]
pub struct Arpeggio {
    pub order: ArpOrder,
    /// How many octaves the figure climbs through; 1 stays on the chord as
    /// voiced.
    pub octaves: u8,
    /// Beats per note: 0.25 is sixteenths, 0.5 eighths.
    pub rate: f64,
    /// Fraction of each step that sounds; above 1 the notes overlap.
    pub gate: f64,
    /// Drives [`ArpOrder::Random`].
    pub seed: u64,
    pub dynamics: Dynamics,
}

impl Default for Arpeggio {
    fn default() -> Self {
        Self { order: ArpOrder::Up, octaves: 1, rate: 0.25, gate: 0.8, seed: 1, dynamics: Dynamics::default() }
    }
}

impl Arpeggio {
    /// The first `count` steps over a chord whose notes are `played`, given
    /// in played order (root first). `None` is a rest. `salt` varies the
    /// random order from chord to chord.
    pub fn sequence(&self, played: &[u8], count: usize, salt: u64) -> Vec<Option<u8>> {
        if played.is_empty() {
            return vec![None; count];
        }
        let octaves = self.octaves.clamp(1, 6) as u16;
        let ladder = |base: &[u8]| -> Vec<u8> {
            (0..octaves)
                .flat_map(|octave| base.iter().map(move |pitch| *pitch as u16 + 12 * octave))
                .filter(|pitch| *pitch <= 127)
                .map(|pitch| pitch as u8)
                .collect()
        };
        let mut lowest_first = played.to_vec();
        lowest_first.sort_unstable();
        lowest_first.dedup();
        let mut up = ladder(&lowest_first);
        up.sort_unstable();
        up.dedup();
        let down: Vec<u8> = up.iter().rev().copied().collect();
        // There and back, leaving out the turning points on the way back.
        let bounce = |there: &[u8]| -> Vec<u8> {
            let back = there.iter().rev().skip(1).take(there.len().saturating_sub(2));
            there.iter().chain(back).copied().collect()
        };

        let cycle: Vec<Option<u8>> = match &self.order {
            ArpOrder::Up => up.into_iter().map(Some).collect(),
            ArpOrder::Down => down.into_iter().map(Some).collect(),
            ArpOrder::UpDown => bounce(&up).into_iter().map(Some).collect(),
            ArpOrder::DownUp => bounce(&down).into_iter().map(Some).collect(),
            ArpOrder::UpDownInclusive => up.iter().chain(&down).copied().map(Some).collect(),
            ArpOrder::Played => ladder(played).into_iter().map(Some).collect(),
            ArpOrder::Random => {
                let mut rng = Rng::new(self.seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15));
                let mut previous = None;
                return (0..count)
                    .map(|_| {
                        let mut pick = rng.below(up.len());
                        if Some(pick) == previous && up.len() > 1 {
                            pick = (pick + 1 + rng.below(up.len() - 1)) % up.len();
                        }
                        previous = Some(pick);
                        Some(up[pick])
                    })
                    .collect();
            }
            ArpOrder::Pattern(numbers) => numbers
                .iter()
                .map(|number| {
                    let index = number.checked_sub(1)?;
                    let pitch = lowest_first[index % lowest_first.len()] as usize + 12 * (index / lowest_first.len());
                    (pitch <= 127).then_some(pitch as u8)
                })
                .collect(),
        };
        if cycle.is_empty() {
            return vec![None; count];
        }
        cycle.iter().cycle().take(count).copied().collect()
    }

    /// Arpeggiates a progression. Each chord starts the figure afresh, so
    /// the pattern lines up with the harmony. `voicings` is what
    /// [`Progression::voice`] returned.
    pub fn notes(&self, progression: &Progression, voicings: &[Option<Voicing>]) -> Vec<Note> {
        let rate = self.rate.max(MIN_LENGTH);
        let mut notes = Vec::new();
        for (index, span) in progression.spans.iter().enumerate() {
            let (Some(chord), Some(Some(voicing))) = (&span.chord, voicings.get(index)) else { continue };
            let steps = ((span.length - EPSILON) / rate).floor() as usize + 1;
            let played = played_order(chord, voicing);
            for (step, pitch) in self.sequence(&played, steps, index as u64).into_iter().enumerate() {
                let Some(pitch) = pitch else { continue };
                let start = span.start + step as f64 * rate;
                notes.push(Note {
                    pitch,
                    start,
                    length: (rate * self.gate).max(MIN_LENGTH),
                    velocity: clamp_velocity(self.dynamics.at(start, progression.signature)),
                    ..Default::default()
                });
            }
        }
        notes
    }
}

/// The notes of a voicing in the order the chord is named: root, third,
/// fifth, seventh, then the extensions; any other voiced note (a slash
/// bass) last.
pub fn played_order(chord: &Chord, voicing: &Voicing) -> Vec<u8> {
    let pitches = voicing.pitches();
    let mut ordered: Vec<u8> = Vec::with_capacity(pitches.len());
    let classes = chord.pitch_classes();
    for class in &classes {
        ordered.extend(pitches.iter().filter(|pitch| pitch_class(**pitch) == *class));
    }
    ordered.dedup();
    ordered.extend(pitches.iter().filter(|pitch| !classes.contains(&pitch_class(**pitch))));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voicing::VoicingSpec;
    use trill_core::TimeSignature;

    const TRIAD: [u8; 3] = [60, 64, 67];

    fn run(order: ArpOrder, octaves: u8, played: &[u8], count: usize) -> Vec<u8> {
        let arpeggio = Arpeggio { order, octaves, ..Arpeggio::default() };
        arpeggio.sequence(played, count, 0).into_iter().map(|pitch| pitch.unwrap_or(0)).collect()
    }

    #[test]
    fn orders_run_through_the_chord() {
        assert_eq!(run(ArpOrder::Up, 1, &TRIAD, 7), [60, 64, 67, 60, 64, 67, 60]);
        assert_eq!(run(ArpOrder::Down, 1, &TRIAD, 4), [67, 64, 60, 67]);
        assert_eq!(run(ArpOrder::UpDown, 1, &TRIAD, 9), [60, 64, 67, 64, 60, 64, 67, 64, 60]);
        assert_eq!(run(ArpOrder::UpDownInclusive, 1, &TRIAD, 7), [60, 64, 67, 67, 64, 60, 60]);
        assert_eq!(run(ArpOrder::DownUp, 1, &TRIAD, 5), [67, 64, 60, 64, 67]);
        assert_eq!(run(ArpOrder::Up, 2, &TRIAD, 7), [60, 64, 67, 72, 76, 79, 60]);
        assert_eq!(run(ArpOrder::UpDown, 2, &TRIAD, 11), [60, 64, 67, 72, 76, 79, 76, 72, 67, 64, 60]);
        assert_eq!(run(ArpOrder::Down, 2, &TRIAD, 3), [79, 76, 72]);
        // One and two notes have nothing to turn round on.
        assert_eq!(run(ArpOrder::UpDown, 1, &[60], 3), [60, 60, 60]);
        assert_eq!(run(ArpOrder::UpDown, 1, &[60, 67], 4), [60, 67, 60, 67]);
        assert_eq!(run(ArpOrder::Up, 1, &[], 2), [0, 0], "no chord, no notes");
    }

    #[test]
    fn played_order_follows_the_chord_not_the_keyboard() {
        // C major voiced in first inversion: E G C.
        let chord = Chord::parse("C").unwrap();
        let voicing = Voicing { bass: vec![], upper: vec![64, 67, 72] };
        let played = played_order(&chord, &voicing);
        assert_eq!(played, [72, 64, 67], "root, third, fifth");
        assert_eq!(run(ArpOrder::Played, 1, &played, 4), [72, 64, 67, 72]);
        assert_eq!(run(ArpOrder::Up, 1, &played, 4), [64, 67, 72, 64]);
        assert_eq!(run(ArpOrder::Played, 2, &played, 6), [72, 64, 67, 84, 76, 79]);
        // A slash bass that is not a chord tone comes last.
        let slash = Chord::parse("C/Bb").unwrap();
        let voicing = Voicing { bass: vec![46], upper: vec![60, 64, 67] };
        assert_eq!(played_order(&slash, &voicing), [60, 64, 67, 46]);
    }

    #[test]
    fn patterns_pick_tones_by_number() {
        assert_eq!(run(ArpOrder::Pattern(vec![1, 3, 2, 3]), 1, &TRIAD, 8), [60, 67, 64, 67, 60, 67, 64, 67]);
        assert_eq!(run(ArpOrder::Pattern(vec![1, 0, 4, 5]), 1, &TRIAD, 4), [60, 0, 72, 76], "0 rests, 4 is the octave");
        assert_eq!(run(ArpOrder::Pattern(vec![]), 1, &TRIAD, 2), [0, 0]);
        assert_eq!(
            run(ArpOrder::Pattern(vec![1, 40]), 1, &TRIAD, 2),
            [60, 0],
            "past the top of the keyboard is a rest"
        );
    }

    #[test]
    fn random_order_is_seeded_and_never_stutters() {
        let arpeggio = |seed| Arpeggio { order: ArpOrder::Random, octaves: 2, seed, ..Arpeggio::default() };
        let first = arpeggio(5).sequence(&TRIAD, 200, 0);
        assert_eq!(first, arpeggio(5).sequence(&TRIAD, 200, 0));
        assert_ne!(first, arpeggio(6).sequence(&TRIAD, 200, 0));
        assert_ne!(first, arpeggio(5).sequence(&TRIAD, 200, 1), "each chord gets its own shuffle");
        assert!(first.windows(2).all(|pair| pair[0] != pair[1]));
        let allowed = [60, 64, 67, 72, 76, 79];
        assert!(first.iter().all(|pitch| allowed.contains(&pitch.unwrap())));
        for pitch in allowed {
            assert!(first.contains(&Some(pitch)), "{pitch} never came up");
        }
    }

    #[test]
    fn arpeggios_follow_the_progression_in_time() {
        let progression = Progression::parse("Am | F C", TimeSignature::default(), None).unwrap();
        let voicings = progression.voice(&VoicingSpec::default());
        let arpeggio =
            Arpeggio { rate: 0.5, gate: 0.5, dynamics: Dynamics { velocity: 80, accent: 20 }, ..Arpeggio::default() };
        let notes = arpeggio.notes(&progression, &voicings);
        assert_eq!(notes.len(), 16);
        assert!(notes.iter().all(|note| note.length == 0.25));
        assert_eq!(
            notes.iter().map(|note| note.start).collect::<Vec<_>>(),
            (0..16).map(|i| i as f64 * 0.5).collect::<Vec<_>>()
        );
        // Each chord starts again from its lowest note.
        for (start, index) in [(0.0, 0), (4.0, 1), (6.0, 2)] {
            let first = notes.iter().find(|note| note.start == start).unwrap();
            assert_eq!(first.pitch, voicings[index].as_ref().unwrap().upper[0]);
        }
        assert_eq!(notes[0].velocity, 100, "the downbeat is accented");
        assert_eq!(notes[2].velocity, 90, "other beats a little");
        assert_eq!(notes[1].velocity, 80);

        // Triplets fit a beat exactly, and a silent bar stays silent.
        let progression = Progression::parse("Am | N.C.", TimeSignature::default(), None).unwrap();
        let voicings = progression.voice(&VoicingSpec::default());
        let triplets = Arpeggio { rate: 1.0 / 3.0, ..Arpeggio::default() };
        let notes = triplets.notes(&progression, &voicings);
        assert_eq!(notes.len(), 12);
        assert!(notes.iter().all(|note| note.start < 4.0));
    }

    #[test]
    fn order_names_parse() {
        for (name, description) in ORDERS {
            assert!(ArpOrder::parse(name).is_ok(), "{name}");
            assert!(description.len() > 10);
        }
        assert_eq!(ArpOrder::parse("Up-Down").unwrap(), ArpOrder::UpDown);
        assert_eq!(ArpOrder::parse("updown inclusive").unwrap(), ArpOrder::UpDownInclusive);
        let error = ArpOrder::parse("sideways").unwrap_err();
        assert!(error.message().contains("'sideways'") && error.message().contains("up_down"), "{error}");
    }
}
