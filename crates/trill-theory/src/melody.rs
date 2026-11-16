//! A compact notation for single melodic lines.
//!
//! ```text
//! :0.5 5 6 5 4 3:2 | :1 1:2 r 7, | 1:4            scale degrees, here in A minor
//! :0.5 E5 F5 E5 D5 C5:2 | :1 A4:2 r G4 | A4:4    the same line as note names
//! 3:1.5 ~:0.5 b3 #4 1':2                         a tie, a flat third, the tonic an octave up
//! ```
//!
//! * A pitch is a **scale degree** (`1`-`7`, counting up from the tonic in
//!   the given octave; `8` and beyond carry on upward) or a **note name
//!   with its octave** (`C4`, `F#5`, `Bb3`).
//! * `b` or `#` before a degree lowers or raises it a semitone. After a
//!   degree, `'` moves it up an octave and `,` down.
//! * `:` and a number after a note give that note's length in beats. A note
//!   without one is a beat long, or whatever the last bare `:beats` set: so
//!   `:0.5` on its own makes the notes after it eighths, and `:1` goes back.
//! * `r` is a rest and `~` ties: it lengthens the note before it.
//! * `|` is an optional bar line. When bar lines are written, every bar is
//!   checked to be the right length, which catches slips in counting. The
//!   first bar may be shorter: a pickup leading into the first full bar.

use crate::error::{Error, Result};
use crate::expression::{Dynamics, MIN_LENGTH, clamp_velocity};
use crate::pitch::NoteName;
use crate::scale::Scale;
use crate::timing::{EPSILON, parse_beats};
use trill_core::{Note, TimeSignature};

/// One note of a phrase, before dynamics.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tone {
    pub pitch: u8,
    pub start: f64,
    pub length: f64,
}

/// A parsed melodic phrase.
#[derive(Clone, Debug, PartialEq)]
pub struct Phrase {
    pub tones: Vec<Tone>,
    /// Total length in beats, including rests at either end.
    pub length: f64,
    /// Length of the pickup before the first bar line, or 0 when the
    /// phrase starts on a downbeat. A phrase with a pickup belongs this
    /// many beats before a bar line.
    pub pickup: f64,
}

const EXPECTED: &str = "expected a scale degree (1-7, b3, #4, 5' for the octave above, 7, for the octave below), a note \
                        name with its octave (C4, F#5), r for a rest or ~ for a tie, each optionally followed by \
                        :beats; a bare :beats sets the length of the notes after it";

impl Phrase {
    /// Parses a phrase. Degrees are read in `key`, with degree 1 in
    /// `octave`; a phrase of note names needs no key.
    pub fn parse(text: &str, key: Option<&Scale>, octave: i32, signature: TimeSignature) -> Result<Self> {
        let bar_length = signature.beats_per_bar();
        let spaced = text.replace('|', " | ");
        let mut tones: Vec<Tone> = Vec::new();
        let (mut position, mut default, mut bar_start, mut bar) = (0.0f64, 1.0f64, 0.0f64, 1usize);
        let mut notes_and_rests = 0;
        let mut pickup = 0.0;
        // Whether the last thing written was a note, which is what a tie extends.
        let mut tieable = false;
        let mut count = 0;

        for token in spaced.split_whitespace() {
            if token == "|" {
                let written = position - bar_start;
                // Only the first bar may be short: it is then a pickup.
                let short_first = bar == 1 && bar_start == 0.0 && written < bar_length - EPSILON;
                if short_first {
                    pickup = written;
                }
                if (written - bar_length).abs() > EPSILON && !short_first {
                    return Err(Error::new(format!(
                        "bar {bar} of the phrase is {} beats long, but a bar of {}/{} has {bar_length}; check the \
                         lengths: a note without :beats is as long as the last bare :beats said (1 to begin with)",
                        round(written),
                        signature.numerator,
                        signature.denominator
                    )));
                }
                if written.abs() > EPSILON {
                    bar += 1;
                }
                bar_start = position;
                continue;
            }
            count += 1;
            let fail = |problem: &str| Error::new(format!("phrase token {count} '{token}': {problem}"));
            // "5:0.5" is one note's length; a bare ":0.5" is the default from here on.
            let (body, own) = match token.split_once(':') {
                Some((body, beats)) => {
                    let Some(beats) = parse_beats(beats) else {
                        return Err(fail(&format!(
                            "'{beats}' is not a length in beats; expected a number, as in 5:0.5"
                        )));
                    };
                    (body, Some(beats))
                }
                None => (token, None),
            };
            if body.is_empty() {
                default = own.unwrap_or(default);
                continue;
            }
            notes_and_rests += 1;
            let length = own.unwrap_or(default);
            match body {
                "r" | "R" => tieable = false,
                "~" => {
                    let Some(tone) = tones.last_mut().filter(|_| tieable) else {
                        return Err(fail("a tie lengthens the note before it, and there is no note there"));
                    };
                    tone.length += length;
                }
                _ => {
                    let pitch = parse_pitch(body, key, octave).map_err(|problem| fail(&problem))?;
                    tones.push(Tone { pitch, start: position, length });
                    tieable = true;
                }
            }
            position += length;
        }
        if notes_and_rests == 0 {
            return Err(Error::new(format!("'{text}' has no notes; {EXPECTED}")));
        }
        Ok(Self { tones, length: position, pickup })
    }

    /// The phrase as notes. `gate` is the fraction of each note that
    /// sounds. Velocities follow `dynamics`, lifted a little toward the top
    /// of the phrase's range, the way a singer or player leans on the high
    /// notes.
    pub fn notes(&self, gate: f64, dynamics: &Dynamics, signature: TimeSignature) -> Vec<Note> {
        let lowest = self.tones.iter().map(|tone| tone.pitch).min().unwrap_or(0) as f64;
        let highest = self.tones.iter().map(|tone| tone.pitch).max().unwrap_or(0) as f64;
        let span = highest - lowest;
        self.tones
            .iter()
            .map(|tone| {
                let lift = if span > 0.0 { ((tone.pitch as f64 - lowest) / span - 0.5) * 8.0 } else { 0.0 };
                Note {
                    pitch: tone.pitch,
                    start: tone.start,
                    length: (tone.length * gate).max(MIN_LENGTH),
                    velocity: clamp_velocity(dynamics.at(tone.start, signature) + lift),
                }
            })
            .collect()
    }
}

fn round(value: f64) -> f64 {
    (value * 1e4).round() / 1e4
}

fn parse_pitch(body: &str, key: Option<&Scale>, octave: i32) -> std::result::Result<u8, String> {
    // A capital letter starts a note name; degrees start with a digit or an accidental.
    if body.starts_with(|c: char| c.is_ascii_uppercase()) {
        let named = NoteName::split_prefix(body)
            .and_then(|(name, rest)| rest.parse::<i32>().ok().map(|octave| (name, octave)))
            .ok_or_else(|| format!("not a note name with an octave; {EXPECTED}"))?;
        return named.0.pitch(named.1).ok_or_else(|| "that note is outside the MIDI range C-1 to G9".to_string());
    }
    let after_accidentals = body.trim_start_matches(['b', '#', '♭', '♯']);
    let alter: i32 =
        body[..body.len() - after_accidentals.len()].chars().map(|c| if matches!(c, '#' | '♯') { 1 } else { -1 }).sum();
    let digits = after_accidentals.chars().take_while(char::is_ascii_digit).count();
    let (number, marks) = after_accidentals.split_at(digits);
    let degree: i32 =
        number.parse().ok().filter(|degree| (1..=21).contains(degree)).ok_or_else(|| EXPECTED.to_string())?;
    let mut shift = 0;
    for mark in marks.chars() {
        match mark {
            '\'' | '’' => shift += 1,
            ',' => shift -= 1,
            _ => return Err(EXPECTED.to_string()),
        }
    }
    let key = key.ok_or_else(|| "a scale degree needs a key; give one, or write note names like C4".to_string())?;
    key.degree_pitch(degree, alter, octave + shift)
        .ok_or_else(|| "that degree is outside the MIDI range in this octave".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn four() -> TimeSignature {
        TimeSignature::default()
    }

    fn phrase(text: &str, key: &str) -> Phrase {
        let key = Scale::parse(key).unwrap();
        Phrase::parse(text, Some(&key), 4, four()).unwrap_or_else(|error| panic!("{error}"))
    }

    /// `"69@0+1 72@1+0.5"`: pitch, start and length of every note.
    fn written(phrase: &Phrase) -> String {
        phrase
            .tones
            .iter()
            .map(|tone| format!("{}@{}+{}", tone.pitch, tone.start, tone.length))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn degrees_are_read_in_the_key() {
        assert_eq!(
            written(&phrase("1 2 3 4 5 6 7 8", "A minor")),
            "69@0+1 71@1+1 72@2+1 74@3+1 76@4+1 77@5+1 79@6+1 81@7+1"
        );
        assert_eq!(written(&phrase("1 3 5", "C major")), "60@0+1 64@1+1 67@2+1");
        assert_eq!(written(&phrase("1 b3 #4 b7 #7", "C major")), "60@0+1 63@1+1 66@2+1 70@3+1 72@4+1");
        assert_eq!(written(&phrase("7 #7 1'", "A minor")), "79@0+1 80@1+1 81@2+1", "the leading tone of a minor key");
        assert_eq!(written(&phrase("1' 1 1, 5,, 9", "C major")), "72@0+1 60@1+1 48@2+1 43@3+1 74@4+1");
        assert_eq!(written(&phrase("1 2 3 4 5 6", "A minor pentatonic")), "69@0+1 72@1+1 74@2+1 76@3+1 79@4+1 81@5+1");
        let low = Phrase::parse("1 5", Some(&Scale::parse("E minor").unwrap()), 2, four()).unwrap();
        assert_eq!(written(&low), "40@0+1 47@1+1");
    }

    #[test]
    fn note_names_need_no_key() {
        let named = Phrase::parse(":0.5 E5 F5 E5 D5 C5:2 | :1 A4:2 r G4 | A4:4", None, 4, four()).unwrap();
        assert_eq!(written(&named), "76@0+0.5 77@0.5+0.5 76@1+0.5 74@1.5+0.5 72@2+2 69@4+2 67@7+1 69@8+4");
        // The same line in degrees.
        assert_eq!(phrase(":0.5 5 6 5 4 3:2 | :1 1:2 r 7, | 1:4", "A minor"), named);
        assert_eq!(written(&phrase("Bb3 F#4 C-1 Cb4", "C major")), "58@0+1 66@1+1 0@2+1 59@3+1");
    }

    #[test]
    fn lengths_defaults_ties_and_rests() {
        // A note's own length is its alone; a bare length sets the default.
        assert_eq!(written(&phrase("1:0.5 2 3:2 4", "C major")), "60@0+0.5 62@0.5+1 64@1.5+2 65@3.5+1");
        let line = phrase(":0.5 1 2 3 4:2 5 :1 6", "C major");
        assert_eq!(written(&line), "60@0+0.5 62@0.5+0.5 64@1+0.5 65@1.5+2 67@3.5+0.5 69@4+1");
        assert_eq!(line.length, 5.0);
        assert_eq!(written(&phrase("3:1.5 ~:0.5 5:2 ~ ~:4", "C major")), "64@0+2 67@2+7");
        let rests = phrase("r:2 1:1 r 3 r:4", "C major");
        assert_eq!(written(&rests), "60@2+1 64@4+1");
        assert_eq!(rests.length, 9.0);
        // Triplets written as 0.33 are exact.
        let triplets = phrase(":0.33 1 2 3 4:1", "C major");
        assert!((triplets.tones[3].start - 1.0).abs() < 1e-12);
        assert!((triplets.length - 2.0).abs() < 1e-12);
    }

    #[test]
    fn bar_lines_check_the_counting() {
        assert!(Phrase::parse("| 1:1 2 3 4 | 5:4 |", Some(&Scale::parse("C").unwrap()), 4, four()).is_ok());
        assert_eq!(phrase("1:1 2 3 4|5:2 6", "C major").length, 7.0, "the last bar may be left open");
        let error = Phrase::parse("1:1 2 3 4 5 | 6", Some(&Scale::parse("C").unwrap()), 4, four()).unwrap_err();
        assert!(error.message().starts_with("bar 1 of the phrase is 5 beats long, but a bar of 4/4 has 4"), "{error}");
        // A short first bar is a pickup, not a mistake.
        let pickup = phrase(":0.5 5 5 | :1 1':2 7 6 | 5:4", "C major");
        assert_eq!((pickup.pickup, pickup.length), (1.0, 9.0));
        assert_eq!(phrase("1:1 2 3 4 | 5:4", "C major").pickup, 0.0);
        let error =
            Phrase::parse("1:1 2 3 4 | 5:0.5 6 7 | 1", Some(&Scale::parse("C").unwrap()), 4, four()).unwrap_err();
        assert!(error.message().starts_with("bar 2 of the phrase is 2.5 beats long"), "{error}");
        let waltz = TimeSignature::new(3, 4).unwrap();
        assert!(Phrase::parse("1:1 2 3 | 4:3 | 5:1.5 6", Some(&Scale::parse("C").unwrap()), 4, waltz).is_ok());
        let compound = TimeSignature::new(6, 8).unwrap();
        assert!(Phrase::parse(":0.5 1 2 3 4 5 6 | :1.5 1 5", Some(&Scale::parse("C").unwrap()), 4, compound).is_ok());
    }

    #[test]
    fn mistakes_name_the_token() {
        let key = Scale::parse("C major").unwrap();
        for (bad, reason) in [
            ("", "has no notes"),
            ("| |", "has no notes"),
            ("1 2 H4", "phrase token 3 'H4': not a note name with an octave"),
            ("1 x", "phrase token 2 'x': expected a scale degree"),
            ("C", "phrase token 1 'C': not a note name with an octave"),
            ("C4 Dx", "phrase token 2 'Dx': not a note name with an octave"),
            ("1:abc", "phrase token 1 '1:abc': 'abc' is not a length in beats"),
            ("1 :x 2", "phrase token 2 ':x': 'x' is not a length in beats"),
            (":0.5", "has no notes"),
            ("1:0", "is not a length in beats"),
            ("1:-1", "is not a length in beats"),
            ("~ 1", "phrase token 1 '~': a tie lengthens the note before it"),
            ("1 r ~", "phrase token 3 '~': a tie lengthens the note before it"),
            ("0", "phrase token 1 '0': expected a scale degree"),
            ("1 5!", "phrase token 2 '5!': expected a scale degree"),
            ("C12", "outside the MIDI range"),
            ("1''''''", "outside the MIDI range"),
        ] {
            let error = Phrase::parse(bad, Some(&key), 4, four()).unwrap_err();
            assert!(error.message().contains(reason), "{bad:?}: {error}");
        }
        let error = Phrase::parse("1 2 3", None, 4, four()).unwrap_err();
        assert!(error.message().contains("phrase token 1 '1': a scale degree needs a key"), "{error}");
    }

    #[test]
    fn notes_get_dynamics_and_gate() {
        let line = phrase("1:1 5 1' 5 | 1:4", "C major");
        let notes = line.notes(0.9, &Dynamics { velocity: 80, accent: 10 }, four());
        assert_eq!(notes.len(), 5);
        assert!((notes[0].length - 0.9).abs() < 1e-9);
        assert!((notes[4].length - 3.6).abs() < 1e-9);
        // Downbeat low note: 80 + 10 - 4. High note on beat three: 80 + 5 + 4.
        assert_eq!(notes[0].velocity, 86);
        assert_eq!(notes[2].velocity, 89);
        assert_eq!(notes[1].velocity, 86, "beat two, just above the middle of the range");
        // A one-note phrase has no range to lean across.
        let single = phrase("1:4", "C major").notes(1.0, &Dynamics { velocity: 80, accent: 0 }, four());
        assert_eq!(single[0].velocity, 80);
    }
}
