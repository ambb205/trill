//! Bass lines written from a chord progression.
//!
//! A bass line is two decisions: which chord tone to play and when. The
//! [`BassStyle`] makes the first (always the root, root and fifth in turn,
//! octaves, or a walking line), a [`Rhythm`] makes the second, and the two
//! combine freely. The root of each chord is placed in one fixed low octave
//! so the line stays in the register where a bass sounds like a bass, and a
//! slash chord's bass note is honoured: over `G/B` the bass plays B.

use crate::chord::Chord;
use crate::error::{Error, Result, list};
use crate::expression::{Dynamics, MIN_LENGTH, clamp_velocity};
use crate::pitch::{at_or_above, pitch_class};
use crate::progression::{Progression, Rhythm, strikes};
use crate::timing::{EPSILON, pulses};
use trill_core::Note;

/// Which chord tones the bass plays.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BassStyle {
    /// The root (or slash bass) of each chord.
    Roots,
    /// Root and fifth in turn.
    RootFifth,
    /// The root, low and an octave up in turn.
    Octaves,
    /// A walking line: the root on each chord change, chord tones after it,
    /// and a chromatic approach into the next root.
    Walking,
}

impl BassStyle {
    pub const ALL: [BassStyle; 4] = [BassStyle::Roots, BassStyle::RootFifth, BassStyle::Octaves, BassStyle::Walking];

    pub fn name(self) -> &'static str {
        match self {
            BassStyle::Roots => "roots",
            BassStyle::RootFifth => "root_fifth",
            BassStyle::Octaves => "octaves",
            BassStyle::Walking => "walking",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            BassStyle::Roots => "the root of each chord; with rhythm sustain, pulse, eighths, offbeats or tresillo",
            BassStyle::RootFifth => "root and fifth in turn, by default on beats 1 and 3: country, pop, Latin",
            BassStyle::Octaves => "the root alternating with its octave, by default in eighths: disco and house",
            BassStyle::Walking => "one note per beat: root, chord tones, then a chromatic step into the next root",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let name = text.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        Self::ALL.into_iter().find(|style| style.name() == name || style.name().replace('_', "") == name).ok_or_else(
            || Error::new(format!("'{text}' is not a bass style; expected one of {}", list(Self::ALL.map(Self::name)))),
        )
    }

    /// The rhythm the style is played in when none is asked for.
    fn default_rhythm(self) -> &'static str {
        match self {
            BassStyle::Roots => "sustain",
            BassStyle::RootFifth => "half",
            BassStyle::Octaves => "eighths",
            BassStyle::Walking => "pulse",
        }
    }
}

/// How to write a bass line.
#[derive(Clone, Debug, PartialEq)]
pub struct BassLine {
    pub style: BassStyle,
    /// `None` uses the style's usual rhythm. A walking line ignores this
    /// and plays on every beat.
    pub rhythm: Option<Rhythm>,
    /// The lowest note. Each chord's root sits in the octave above it.
    pub low: u8,
    /// The highest note the line may reach.
    pub high: u8,
    /// Fraction of each note that sounds.
    pub gate: f64,
    /// Beats by which chord changes arrive early; see
    /// [`strikes`](crate::progression::strikes).
    pub anticipation: f64,
    pub dynamics: Dynamics,
}

impl Default for BassLine {
    fn default() -> Self {
        // E1 is the lowest string of a bass guitar; roots land in E1-D#2.
        Self {
            style: BassStyle::Roots,
            rhythm: None,
            low: 28,
            high: 55,
            gate: 0.9,
            anticipation: 0.0,
            dynamics: Dynamics { velocity: 96, accent: 10 },
        }
    }
}

/// Writes a bass line under a progression.
pub fn bass_line(progression: &Progression, line: &BassLine) -> Vec<Note> {
    if line.style == BassStyle::Walking {
        return walk(progression, line);
    }
    let rhythm = match &line.rhythm {
        Some(rhythm) => rhythm.clone(),
        None => Rhythm::parse(line.style.default_rhythm(), progression.signature).unwrap_or(Rhythm::Sustain),
    };
    let (low, high) = (line.low as i32, line.high.max(line.low) as i32);
    let mut notes = Vec::new();
    for strike in strikes(progression, &rhythm, line.anticipation) {
        let Some(chord) = &progression.spans[strike.span].chord else { continue };
        let root = at_or_above(chord.bass_name().pitch_class(), low);
        let other = match line.style {
            BassStyle::RootFifth => root + fifth_above_bass(chord),
            _ => root + 12,
        };
        // If the upper note is out of reach, take it below instead.
        let other = if other <= high {
            other
        } else if other - 12 >= low {
            other - 12
        } else {
            root
        };
        let pitch = if line.style != BassStyle::Roots && strike.index % 2 == 1 { other } else { root };
        if (0..=127).contains(&pitch) {
            let velocity = line.dynamics.at(strike.start, progression.signature);
            notes.push(Note {
                pitch: pitch as u8,
                start: strike.start,
                length: (strike.length * line.gate).max(MIN_LENGTH),
                velocity: strike.level.velocity(velocity),
            });
        }
    }
    notes
}

/// Semitones from a chord's bass note up to the note a bassist alternates
/// it with: the chord tone nearest a fifth above. For a chord in root
/// position that is its fifth, altered if the chord alters it; over a slash
/// bass it is usually the root (C/E alternates E with C).
fn fifth_above_bass(chord: &Chord) -> i32 {
    let bass = chord.bass_name().pitch_class() as i32;
    chord
        .pitch_classes()
        .into_iter()
        .map(|class| (class as i32 - bass).rem_euclid(12))
        .filter(|distance| *distance != 0)
        .min_by_key(|distance| (distance - 7).abs())
        .unwrap_or(12)
}

/// A walking line: one note on every beat.
fn walk(progression: &Progression, line: &BassLine) -> Vec<Note> {
    let (low, high) = (line.low as i32, line.high.max(line.low.saturating_add(12)) as i32);
    let bar_length = progression.signature.beats_per_bar();
    let beats = pulses(progression.signature);
    let chords: Vec<(f64, f64, &Chord)> = progression
        .spans
        .iter()
        .filter_map(|span| span.chord.as_ref().map(|chord| (span.start, span.end(), chord)))
        .collect();
    let Some((_, _, first)) = chords.first() else { return Vec::new() };

    let mut notes = Vec::new();
    let mut root = at_or_above(first.bass_name().pitch_class(), low);
    for (number, (start, end, chord)) in chords.iter().enumerate() {
        // The beats this chord covers; a chord that starts off the beat is
        // still played where it starts.
        let mut times: Vec<f64> = (0..progression.bars)
            .flat_map(|bar| beats.iter().map(move |beat| bar as f64 * bar_length + beat))
            .filter(|time| *time >= start - EPSILON && *time < end - EPSILON)
            .collect();
        if times.first().is_none_or(|first| first - start > EPSILON) {
            times.insert(0, *start);
        }

        // Wrapping to the first chord makes the line loop cleanly.
        let next = chords[(number + 1) % chords.len()].2;
        let next_root = nearest_root(next.bass_name().pitch_class(), root, (low, high));
        let pitches = walk_chord(chord, root, next_root, times.len(), (low, high), number);
        for (index, (time, pitch)) in times.iter().zip(pitches).enumerate() {
            let until = times.get(index + 1).copied().unwrap_or(*end);
            notes.push(Note {
                pitch: pitch.clamp(0, 127) as u8,
                start: *time,
                length: ((until - time) * line.gate).max(MIN_LENGTH),
                velocity: clamp_velocity(line.dynamics.at(*time, progression.signature)),
            });
        }
        root = next_root;
    }
    notes
}

/// Where the next chord's root is played: the instance nearest the current
/// root, kept within about an octave of the bottom of the range so the line
/// cannot climb out of the bass register.
fn nearest_root(class: u8, from: i32, (low, high): (i32, i32)) -> i32 {
    let lower = at_or_above(class, low);
    [lower, lower + 12]
        .into_iter()
        .filter(|pitch| *pitch <= (low + 16).min(high))
        .min_by_key(|pitch| ((pitch - from).abs(), *pitch))
        .unwrap_or(lower)
}

/// The notes of a walking bar over one chord: the root, chord tones moving
/// toward the next root, and a semitone approach to it on the last beat.
fn walk_chord(chord: &Chord, root: i32, next_root: i32, count: usize, range: (i32, i32), number: usize) -> Vec<i32> {
    let mut line = vec![root];
    if count <= 1 {
        return line;
    }
    let (low, high) = range;
    // What a bassist walks through: root, third, fifth and sixth or seventh.
    let chord_root = chord.root.pitch_class() as i32;
    let walkable: Vec<u8> = chord
        .quality
        .tones()
        .iter()
        .filter(|tone| tone.number <= 7)
        .map(|tone| (chord_root + tone.semitones()).rem_euclid(12) as u8)
        .collect();
    let mut ladder: Vec<i32> =
        (low..=high).filter(|pitch| walkable.contains(&pitch_class(*pitch as u8)) || *pitch == root).collect();
    ladder.dedup();

    // Head for the next root. When the chord stays, go up and back, and on
    // every other bar down and back if there is time and room for it.
    let mut position = ladder.iter().position(|pitch| *pitch == root).unwrap_or(0) as i32;
    let mut direction = match next_root.cmp(&root) {
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal if number % 2 == 1 && count >= 4 && position >= 2 => -1,
        std::cmp::Ordering::Equal => 1,
    };
    for _ in 0..count - 2 {
        if position + direction < 0 || position + direction >= ladder.len() as i32 {
            direction = -direction;
        }
        position = (position + direction).clamp(0, ladder.len() as i32 - 1);
        line.push(ladder[position as usize]);
    }

    let last = line[line.len() - 1];
    let mut approach = if next_root == root {
        // The chord stays: turn back through the note just left (1 3 5 3),
        // or with too few beats for that, end on the fifth.
        let fifth = root + fifth_above_bass(chord);
        let turn_back = line.get(line.len().wrapping_sub(2)).copied().filter(|pitch| *pitch != root);
        turn_back.or(Some(fifth).filter(|fifth| *fifth != last && *fifth <= high)).unwrap_or(root - 1)
    } else if last > next_root {
        next_root + 1
    } else {
        next_root - 1
    };
    // Never repeat the note before, and stay on the instrument.
    if approach == last {
        approach = 2 * next_root - approach;
    }
    if approach < low {
        approach = next_root + 1;
    }
    if approach > high {
        approach = next_root - 1;
    }
    line.push(approach);
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::TimeSignature;
    use trill_core::pitch::pitch_name;

    fn progression(text: &str, numerator: u8, denominator: u8) -> Progression {
        Progression::parse(text, TimeSignature::new(numerator, denominator).unwrap(), None).unwrap()
    }

    fn line(style: BassStyle) -> BassLine {
        BassLine { style, gate: 1.0, ..BassLine::default() }
    }

    /// `"A1@0 E2@2"`: each note with its start.
    fn written(notes: &[Note]) -> String {
        notes.iter().map(|note| format!("{}@{}", pitch_name(note.pitch), note.start)).collect::<Vec<_>>().join(" ")
    }

    fn names(notes: &[Note]) -> String {
        notes.iter().map(|note| pitch_name(note.pitch)).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn roots_sit_in_one_low_octave_and_follow_slash_chords() {
        let notes = bass_line(&progression("Am | F | C G/B | E7", 4, 4), &line(BassStyle::Roots));
        assert_eq!(written(&notes), "A1@0 F1@4 C2@8 B1@10 E1@12");
        assert_eq!(notes.iter().map(|note| note.length).collect::<Vec<_>>(), [4.0, 4.0, 2.0, 2.0, 4.0]);
        // Every root of every key stays in the octave above the lowest note.
        for tonic in ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"] {
            let notes =
                bass_line(&progression(&format!("{tonic} {tonic}m7 {tonic}7/E"), 4, 4), &line(BassStyle::Roots));
            assert!(notes.iter().all(|note| (28..40).contains(&note.pitch)), "{tonic}: {}", names(&notes));
        }
        // A silent bar is silent, and a different register is respected.
        let high = BassLine { low: 40, ..line(BassStyle::Roots) };
        assert_eq!(written(&bass_line(&progression("Am | N.C. | F", 4, 4), &high)), "A2@0 F2@8");
    }

    #[test]
    fn rhythms_and_styles_combine() {
        let chart = progression("Am | F", 4, 4);
        let with = |style: BassStyle, rhythm: &str| {
            let rhythm = Some(Rhythm::parse(rhythm, chart.signature).unwrap());
            bass_line(&chart, &BassLine { rhythm, ..line(style) })
        };
        assert_eq!(written(&with(BassStyle::Roots, "pulse")), "A1@0 A1@1 A1@2 A1@3 F1@4 F1@5 F1@6 F1@7");
        assert_eq!(
            written(&with(BassStyle::Roots, "offbeats")),
            "A1@0.5 A1@1.5 A1@2.5 A1@3.5 F1@4.5 F1@5.5 F1@6.5 F1@7.5"
        );
        assert_eq!(written(&with(BassStyle::Roots, "tresillo")), "A1@0 A1@1.5 A1@3 F1@4 F1@5.5 F1@7");
        assert_eq!(
            written(&with(BassStyle::Roots, "x..x..x.x.x..x..")),
            "A1@0 A1@0.75 A1@1.5 A1@2 A1@2.5 A1@3.25 F1@4 F1@4.75 F1@5.5 F1@6 F1@6.5 F1@7.25"
        );

        // Defaults: root-fifth on beats 1 and 3, octaves in eighths.
        assert_eq!(written(&bass_line(&chart, &line(BassStyle::RootFifth))), "A1@0 E2@2 F1@4 C2@6");
        assert_eq!(
            names(&bass_line(&chart, &line(BassStyle::Octaves))),
            "A1 A2 A1 A2 A1 A2 A1 A2 F1 F2 F1 F2 F1 F2 F1 F2"
        );
        assert_eq!(written(&with(BassStyle::RootFifth, "pulse")), "A1@0 E2@1 A1@2 E2@3 F1@4 C2@5 F1@6 C2@7");
        assert_eq!(names(&with(BassStyle::Octaves, "tresillo")), "A1 A2 A1 F1 F2 F1");
    }

    #[test]
    fn the_alternate_note_suits_the_chord() {
        let fifth = |symbol: &str| fifth_above_bass(&Chord::parse(symbol).unwrap());
        assert_eq!(fifth("C"), 7);
        assert_eq!(fifth("Cm7"), 7);
        assert_eq!(fifth("Cdim"), 6, "a diminished chord has a flat fifth");
        assert_eq!(fifth("Caug"), 8);
        assert_eq!(fifth("C7b5"), 6);
        assert_eq!(fifth("C/E"), 8, "over the third, the root is the note a sixth up");
        assert_eq!(fifth("G/B"), 8);
        assert_eq!(fifth("C/G"), 5, "over the fifth, the root a fourth up");
        assert_eq!(fifth("C5"), 7);
        // A fifth that will not fit above goes below.
        let tight = BassLine { low: 28, high: 34, ..line(BassStyle::RootFifth) };
        assert_eq!(names(&bass_line(&progression("A", 4, 4), &tight)), "A1 E1");
    }

    #[test]
    fn walking_lines_step_into_each_new_root() {
        let chart = progression("Dm7 | G7 | Cmaj7 | A7", 4, 4);
        let notes = bass_line(&chart, &line(BassStyle::Walking));
        assert_eq!(names(&notes), "D2 F2 A2 G#2 G2 F2 D2 C#2 C2 B1 G1 G#1 A1 C#2 E2 D#2");
        assert_eq!(notes.iter().map(|note| note.start).collect::<Vec<_>>(), (0..16).map(f64::from).collect::<Vec<_>>());

        for (text, numerator, denominator) in [
            ("Dm7 | G7 | Cmaj7 | A7", 4, 4),
            ("Cm7 | F7 | Bbmaj7 | Ebmaj7 | Am7b5 | D7 | Gm6 | Gm6", 4, 4),
            ("F7 | Bb7 | F7 | F7 | Bb7 | Bb7 | F7 | D7 | Gm7 | C7 | F7 D7 | Gm7 C7", 4, 4),
            ("Am | Dm | E7 | Am", 3, 4),
            ("C | C | C | C", 4, 4),
            ("Em | C D | Em", 6, 8),
            ("Abmaj7 Db7 | Gbmaj7 B7 | Emaj7 | Bbm7 Eb7", 4, 4),
        ] {
            let chart = progression(text, numerator, denominator);
            let notes = bass_line(&chart, &line(BassStyle::Walking));
            let label = format!("{text}: {}", names(&notes));
            assert!(notes.iter().all(|note| (28..=55).contains(&note.pitch)), "out of the bass register: {label}");
            assert!(notes.windows(2).all(|pair| pair[0].pitch != pair[1].pitch), "a repeated note: {label}");

            // Each chord starts on its bass note, and a new root is reached
            // by a semitone step.
            let chords: Vec<_> = chart.spans.iter().filter(|span| span.chord.is_some()).collect();
            for (index, span) in chords.iter().enumerate() {
                let bass = span.chord.as_ref().unwrap().bass_name().pitch_class();
                let first = notes.iter().position(|note| (note.start - span.start).abs() < 1e-9).unwrap();
                assert_eq!(pitch_class(notes[first].pitch), bass, "{label}");
                let Some(previous) = index.checked_sub(1).map(|previous| chords[previous]) else { continue };
                let walked =
                    notes.iter().filter(|note| note.start >= previous.start && note.start < span.start).count();
                if walked >= 2 && previous.chord.as_ref().unwrap().bass_name().pitch_class() != bass {
                    let step = (notes[first - 1].pitch as i32 - notes[first].pitch as i32).abs();
                    assert_eq!(step, 1, "no chromatic approach into bar at beat {}: {label}", span.start);
                }
            }
            // A walking line moves by step and small leap, not by octaves.
            let widest =
                notes.windows(2).map(|pair| (pair[0].pitch as i32 - pair[1].pitch as i32).abs()).max().unwrap();
            assert!(widest <= 9, "a leap of {widest}: {label}");
        }
    }

    #[test]
    fn walking_fits_the_beats_of_the_meter() {
        let waltz = bass_line(&progression("Am | Dm", 3, 4), &line(BassStyle::Walking));
        assert_eq!(waltz.iter().map(|note| note.start).collect::<Vec<_>>(), [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        let compound = bass_line(&progression("Em | C D", 6, 8), &line(BassStyle::Walking));
        assert_eq!(compound.iter().map(|note| note.start).collect::<Vec<_>>(), [0.0, 1.5, 3.0, 4.5]);
        assert_eq!(compound.iter().map(|note| note.length).collect::<Vec<_>>(), [1.5; 4]);
        // Two chords in a bar: root and approach each.
        let split = bass_line(&progression("Dm7 G7 | Cmaj7", 4, 4), &line(BassStyle::Walking));
        assert_eq!(names(&split), "D2 F#2 G2 C#2 C2 E2 G2 D#2");
        assert!(bass_line(&progression("N.C.", 4, 4), &line(BassStyle::Walking)).is_empty());
    }

    #[test]
    fn dynamics_and_gate_shape_the_notes() {
        let chart = progression("Am", 4, 4);
        let pulse = Some(Rhythm::parse("pulse", chart.signature).unwrap());
        let shaped = BassLine {
            rhythm: pulse,
            gate: 0.5,
            dynamics: Dynamics { velocity: 90, accent: 20 },
            ..BassLine::default()
        };
        let notes = bass_line(&chart, &shaped);
        assert_eq!(notes.iter().map(|note| note.velocity).collect::<Vec<_>>(), [110, 100, 100, 100]);
        assert!(notes.iter().all(|note| note.length == 0.5));
        let accented = BassLine { rhythm: Some(Rhythm::parse("X.g.", chart.signature).unwrap()), ..shaped };
        let notes = bass_line(&chart, &accented);
        assert_eq!(notes.iter().map(|note| note.velocity).collect::<Vec<_>>(), [127, 42]);
    }

    #[test]
    fn style_names_parse() {
        for style in BassStyle::ALL {
            assert_eq!(BassStyle::parse(style.name()).unwrap(), style);
            assert!(style.description().len() > 20);
        }
        assert_eq!(BassStyle::parse("Root-Fifth").unwrap(), BassStyle::RootFifth);
        assert_eq!(BassStyle::parse("rootfifth").unwrap(), BassStyle::RootFifth);
        let error = BassStyle::parse("slap").unwrap_err();
        assert!(error.message().contains("'slap'") && error.message().contains("walking"), "{error}");
    }
}
