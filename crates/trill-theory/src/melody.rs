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
//!   In a song whose time signature changes the bars are the song's own,
//!   each as long as it is there ([`Phrase::parse_through`]).
//!
//! # Ornaments
//!
//! A note can say how it is played, in marks after it, each begun by `!`:
//!
//! ```text
//! 4:2!bend=5              bent up to the fifth degree and held there
//! 4:2!bend=5!release      bent up, then let back down
//! 4:2!prebend=5           struck already bent, then let down
//! 5:2!vib                 with vibrato (5:2!vib=0.5 says how wide, in semitones)
//! :0.5 5 6!hammer 5!pull  hammered on and pulled off: no new attack
//! 3:1!slide               slid into from the note before, or from below if there is none
//! 3:1!slide=1             slid into from the first degree
//! 3:1!scoop               slid up into from just below, whatever came before
//! 5:2!fall                falling away at its end
//! 1:0.5!palm_mute         by one of the instrument's own articulations, by name
//! ```
//!
//! Marks combine (`4:2!bend=5!vib`). What a mark becomes depends on the
//! instrument, which [`ornament`](crate::ornament) decides: a keyswitch
//! where the instrument has one for it, a pitch-bend curve where not.

use crate::error::{Error, Result};
use crate::expression::{Dynamics, MIN_LENGTH};
use crate::pitch::NoteName;
use crate::scale::Scale;
use crate::timing::{EPSILON, parse_beats};
use trill_core::{Note, TimeSignature};

/// How a note is played, beyond its pitch and length.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    /// Bent from its own pitch to this one and held there.
    Bend(u8),
    /// After a bend: let back to its own pitch before it ends.
    Release,
    /// Struck already bent to this pitch, then let down to its own.
    Prebend(u8),
    /// Slid into: from this pitch, or with `None` from the note before it,
    /// or from below where no note comes right before.
    Slide(Option<u8>),
    /// Slid up into from just below, whatever came before it.
    Scoop,
    /// Falling away in pitch at its end.
    Fall,
    /// With vibrato, this many semitones each way if given.
    Vibrato(Option<f64>),
    /// Joined to the note before it with no new attack: a hammer-on, a
    /// pull-off, a slur.
    Join,
    /// By an articulation of the instrument, named as the instrument's
    /// profile names it.
    Named(String),
}

impl Mark {
    /// What the mark is written as, for messages.
    pub fn word(&self) -> &str {
        match self {
            Mark::Bend(_) => "bend",
            Mark::Release => "release",
            Mark::Prebend(_) => "prebend",
            Mark::Slide(_) => "slide",
            Mark::Scoop => "scoop",
            Mark::Fall => "fall",
            Mark::Vibrato(_) => "vib",
            Mark::Join => "hammer",
            Mark::Named(name) => name,
        }
    }
}

/// The ornaments a note can be marked with, for listings and messages.
pub const ORNAMENTS: &[(&str, &str)] = &[
    ("bend=P", "bent up (or down) from its own pitch to the pitch P and held there"),
    ("release", "after bend=P: let back to its own pitch before the note ends"),
    ("prebend=P", "struck already bent to P, then let down to its own pitch"),
    ("slide", "slid into from the note before it, or from below if no note comes right before; slide=P slides from P"),
    ("scoop", "slid up into from just below, whatever came before it: a guitar's slide into a note"),
    ("hammer", "joined to the note before it with no new attack: a hammer-on or pull-off (also pull, legato, h, p)"),
    ("vib", "vibrato, after a moment; vib=0.5 sets how wide in semitones (default 0.3)"),
    ("fall", "falls away in pitch at its end"),
];

/// One note of a phrase, before dynamics.
#[derive(Clone, Debug, PartialEq)]
pub struct Tone {
    pub pitch: u8,
    pub start: f64,
    pub length: f64,
    /// The note's name as the phrase spelled it: a sharpened degree is a
    /// sharp and a flattened one a flat, whatever the key would call it.
    pub name: NoteName,
    /// How it is played, in the order the marks were written.
    pub marks: Vec<Mark>,
}

impl Tone {
    /// The name with its octave, such as `"F#5"`.
    pub fn spelled(&self) -> String {
        self.name.spell(self.pitch)
    }
}

/// A bar a phrase is written through: how long it is, and its time
/// signature, which says where its beats fall.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PhraseBar {
    pub length: f64,
    pub signature: TimeSignature,
}

impl PhraseBar {
    /// A whole bar of `signature`.
    pub fn of(signature: TimeSignature) -> Self {
        Self { length: signature.beats_per_bar(), signature }
    }

    /// The bar of `bars` that `beat` falls in, with where it begins: the
    /// last one goes on repeating past the end of the list. `bars` has at
    /// least one.
    pub fn at(bars: &[PhraseBar], beat: f64) -> (f64, PhraseBar) {
        let mut start = 0.0;
        for bar in bars {
            if beat < start + bar.length - EPSILON {
                return (start, *bar);
            }
            start += bar.length;
        }
        let last = *bars.last().expect("a phrase is written through at least one bar");
        let past = ((beat - start + EPSILON) / last.length).floor().max(0.0);
        (start + past * last.length, last)
    }
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
                        :beats; a bare :beats sets the length of the notes after it; marks such as !bend=5, !vib, \
                        !hammer or !slide follow a note";

impl Phrase {
    /// Parses a phrase. Degrees are read in `key`, with degree 1 in
    /// `octave`; a phrase of note names needs no key.
    pub fn parse(text: &str, key: Option<&Scale>, octave: i32, signature: TimeSignature) -> Result<Self> {
        Self::parse_through(text, key, octave, &[PhraseBar::of(signature)])
    }

    /// [`parse`](Self::parse) for a phrase written through bars that are
    /// not all alike: `bars` are the bars from the one the phrase's first
    /// bar line closes, in order, the last going on for as long as the
    /// phrase does. Each bar that is written is checked against its own.
    pub fn parse_through(text: &str, key: Option<&Scale>, octave: i32, bars: &[PhraseBar]) -> Result<Self> {
        let bar_at = |number: usize| bars[(number - 1).min(bars.len() - 1)];
        let alike = bars.iter().all(|bar| *bar == bars[0]);
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
                let PhraseBar { length: bar_length, signature } = bar_at(bar);
                // Only the first bar may be short: it is then a pickup.
                let short_first = bar == 1 && bar_start == 0.0 && written < bar_length - EPSILON;
                if short_first {
                    pickup = written;
                }
                if (written - bar_length).abs() > EPSILON && !short_first {
                    let whole = (bar_length - signature.beats_per_bar()).abs() < EPSILON;
                    return Err(Error::new(format!(
                        "bar {bar} of the phrase is {} beats long, but {} has {}; check the lengths: a note without \
                         :beats is as long as the last bare :beats said (1 to begin with)",
                        round(written),
                        match (whole, alike) {
                            (true, true) => format!("a bar of {}/{}", signature.numerator, signature.denominator),
                            (true, _) => format!(
                                "the song's bar there, one of {}/{},",
                                signature.numerator, signature.denominator
                            ),
                            (false, _) => "the song's bar there, cut short by a change of time signature,".to_string(),
                        },
                        round(bar_length),
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
            // Marks come last: "4:2!bend=5!vib".
            let (token_head, marks_text) = match token.split_once('!') {
                Some((head, marks)) => (head, Some(marks)),
                None => (token, None),
            };
            // "5:0.5" is one note's length; a bare ":0.5" is the default from here on.
            let (body, own) = match token_head.split_once(':') {
                Some((body, beats)) => {
                    let Some(beats) = parse_beats(beats) else {
                        return Err(fail(&format!(
                            "'{beats}' is not a length in beats; expected a number, as in 5:0.5"
                        )));
                    };
                    (body, Some(beats))
                }
                None => (token_head, None),
            };
            if body.is_empty() {
                if marks_text.is_some() {
                    return Err(fail("marks follow a note; this has none"));
                }
                default = own.unwrap_or(default);
                continue;
            }
            notes_and_rests += 1;
            let length = own.unwrap_or(default);
            match body {
                "r" | "R" | "~" if marks_text.is_some() => {
                    return Err(fail("only a note can be marked; a rest or a tie has nothing to bend or slide"));
                }
                "r" | "R" => tieable = false,
                "~" => {
                    let Some(tone) = tones.last_mut().filter(|_| tieable) else {
                        return Err(fail("a tie lengthens the note before it, and there is no note there"));
                    };
                    tone.length += length;
                }
                _ => {
                    let (pitch, name) = parse_pitch(body, key, octave).map_err(|problem| fail(&problem))?;
                    let marks = match marks_text {
                        Some(text) => {
                            parse_marks(text, pitch, tieable, key, octave).map_err(|problem| fail(&problem))?
                        }
                        None => Vec::new(),
                    };
                    tones.push(Tone { pitch, start: position, length, name, marks });
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
        let velocities = crate::ornament::velocities(&self.tones, dynamics, signature);
        self.tones
            .iter()
            .zip(velocities)
            .map(|(tone, velocity)| Note {
                pitch: tone.pitch,
                start: tone.start,
                length: (tone.length * gate).max(MIN_LENGTH),
                velocity,
                ..Default::default()
            })
            .collect()
    }
}

fn round(value: f64) -> f64 {
    (value * 1e4).round() / 1e4
}

/// Reads the marks after a note: `bend=5!vib`. `follows` says whether a
/// note comes right before this one, which joining needs.
fn parse_marks(
    text: &str,
    pitch: u8,
    follows: bool,
    key: Option<&Scale>,
    octave: i32,
) -> std::result::Result<Vec<Mark>, String> {
    let mut marks: Vec<Mark> = Vec::new();
    for written in text.split('!') {
        let (word, value) = match written.split_once('=') {
            Some((word, value)) => (word, Some(value)),
            None => (written, None),
        };
        let name = trill_core::pitch::name_key(word);
        let target = |what: &str| -> std::result::Result<u8, String> {
            let value =
                value.ok_or_else(|| format!("'{what}' needs the pitch to reach, as in {what}=5 or {what}=E5"))?;
            let (target, _) =
                parse_pitch(value, key, octave).map_err(|problem| format!("{what}={value}: {problem}"))?;
            if target == pitch {
                return Err(format!("{what}={value} is the note's own pitch, so there is nothing to bend"));
            }
            Ok(target)
        };
        let plain = |what: &str| match value {
            Some(value) => Err(format!("'{what}' takes no value, but was given '{value}'")),
            None => Ok(()),
        };
        let mark = match name.as_str() {
            "" => return Err("a '!' with no mark after it; write for example !vib or !bend=5".to_string()),
            "bend" => Mark::Bend(target("bend")?),
            "prebend" | "pre_bend" => Mark::Prebend(target("prebend")?),
            "release" => plain("release").map(|()| Mark::Release)?,
            "fall" => plain("fall").map(|()| Mark::Fall)?,
            "slide" => match value {
                Some(_) => Mark::Slide(Some(target("slide")?)),
                None => Mark::Slide(None),
            },
            "scoop" => plain("scoop").map(|()| Mark::Scoop)?,
            "vib" | "vibrato" => match value {
                None => Mark::Vibrato(None),
                Some(depth) => match depth.parse::<f64>() {
                    Ok(depth) if depth > 0.0 && depth <= 2.0 => Mark::Vibrato(Some(depth)),
                    _ => return Err(format!("vib={depth}: the width is in semitones, above 0 and at most 2")),
                },
            },
            "hammer" | "hammer_on" | "pull" | "pull_off" | "legato" | "slur" | "h" | "p" => {
                plain(word)?;
                if !follows {
                    return Err(format!(
                        "'{word}' joins a note to the one before it, and no note comes right before this one"
                    ));
                }
                Mark::Join
            }
            _ => plain(word).map(|()| Mark::Named(name.clone()))?,
        };
        if !marks.iter().any(|other| std::mem::discriminant(other) == std::mem::discriminant(&mark)) {
            marks.push(mark);
        }
    }
    let has = |wanted: fn(&Mark) -> bool| marks.iter().any(wanted);
    if has(|mark| matches!(mark, Mark::Release)) && !has(|mark| matches!(mark, Mark::Bend(_))) {
        return Err("'release' lets a bend back down, and this note has no bend=P".to_string());
    }
    if has(|mark| matches!(mark, Mark::Bend(_))) && has(|mark| matches!(mark, Mark::Prebend(_))) {
        return Err("a note is bent (bend=P) or struck already bent (prebend=P), not both".to_string());
    }
    let slid = has(|mark| matches!(mark, Mark::Slide(_))) || has(|mark| matches!(mark, Mark::Scoop));
    if has(|mark| matches!(mark, Mark::Join)) && slid {
        return Err("a note is hammered on or slid into, not both".to_string());
    }
    if has(|mark| matches!(mark, Mark::Slide(_))) && has(|mark| matches!(mark, Mark::Scoop)) {
        return Err("a note is slid into (slide) or scooped up into from below (scoop), not both".to_string());
    }
    Ok(marks)
}

fn parse_pitch(body: &str, key: Option<&Scale>, octave: i32) -> std::result::Result<(u8, NoteName), String> {
    // A capital letter starts a note name; degrees start with a digit or an accidental.
    if body.starts_with(|c: char| c.is_ascii_uppercase()) {
        let named = NoteName::split_prefix(body)
            .and_then(|(name, rest)| rest.parse::<i32>().ok().map(|octave| (name, octave)))
            .ok_or_else(|| format!("not a note name with an octave; {EXPECTED}"))?;
        let pitch = named.0.pitch(named.1).ok_or("that note is outside the MIDI range C-1 to G9")?;
        return Ok((pitch, named.0));
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
    let pitch = key
        .degree_pitch(degree, alter, octave + shift)
        .ok_or("that degree is outside the MIDI range in this octave")?;
    // The degree's own letter, altered as written.
    let own = key.names()[(degree as usize - 1) % key.size()];
    let name = NoteName::new(own.letter, (own.accidental as i32 + alter).clamp(-3, 3) as i8).simplified();
    Ok((pitch, name))
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
    fn notes_keep_the_spelling_they_were_written_with() {
        let spelled = |text: &str, key: &str| -> String {
            phrase(text, key).tones.iter().map(Tone::spelled).collect::<Vec<_>>().join(" ")
        };
        assert_eq!(spelled("1 #1 2 b3 3 #4 5 b6 6 b7 7", "F major"), "F4 F#4 G4 Ab4 A4 B4 C5 Db5 D5 Eb5 E5");
        assert_eq!(spelled("7 #7 1' 7, #7,", "A minor"), "G5 G#5 A5 G4 G#4");
        assert_eq!(spelled("Gb4 F#4 Cb5 B#3", "C major"), "Gb4 F#4 Cb5 B#3");
        assert_eq!(spelled("1 2 3 4", "Bb minor"), "Bb4 C5 Db5 Eb5");
    }

    fn marks(text: &str, key: &str) -> Vec<Vec<Mark>> {
        phrase(text, key).tones.into_iter().map(|tone| tone.marks).collect()
    }

    #[test]
    fn notes_carry_the_marks_written_after_them() {
        assert_eq!(marks("4:2!bend=5 5!vib 1", "A minor"), [vec![Mark::Bend(76)], vec![Mark::Vibrato(None)], vec![]]);
        assert_eq!(
            marks("4:2!bend=5!release!vib=0.5", "A minor"),
            [vec![Mark::Bend(76), Mark::Release, Mark::Vibrato(Some(0.5))]]
        );
        assert_eq!(marks("E5!prebend=F#5 E5:2!fall", "C"), [vec![Mark::Prebend(78)], vec![Mark::Fall]]);
        assert_eq!(
            marks(":0.5 5 6!hammer 5!pull 3!h 1!p 2!legato", "C")[1..],
            [vec![Mark::Join], vec![Mark::Join], vec![Mark::Join], vec![Mark::Join], vec![Mark::Join]]
        );
        assert_eq!(
            marks("3!slide 5!slide=3 5!Slide", "C"),
            [vec![Mark::Slide(None)], vec![Mark::Slide(Some(64))], vec![Mark::Slide(None)]]
        );
        // Any other word is one of the instrument's own articulations.
        assert_eq!(
            marks("1:0.5!palm_mute 1!Palm-Mute!vib", "E minor"),
            [vec![Mark::Named("palm_mute".into())], vec![Mark::Named("palm_mute".into()), Mark::Vibrato(None)]]
        );
        // Marks change nothing about where and how long the notes are.
        let plain = phrase(":0.5 5 6 5 4 3:2 | :1 1:2 r 7, | 1:4", "A minor");
        let marked = phrase(":0.5 5 6!hammer 5!pull 4 3:2!vib | :1 1:2!bend=2 r 7,!slide | 1:4!fall", "A minor");
        assert_eq!(written(&marked), written(&plain));
        assert_eq!(marked.length, plain.length);
        for (word, mark) in [("bend", Mark::Bend(1)), ("vib", Mark::Vibrato(None)), ("hammer", Mark::Join)] {
            assert_eq!(mark.word(), word);
        }
        assert!(ORNAMENTS.iter().all(|(name, what)| !name.is_empty() && what.len() > 10));
    }

    #[test]
    fn a_mark_that_cannot_be_played_says_why() {
        let key = Scale::parse("C major").unwrap();
        for (bad, reason) in [
            ("4!bend", "'bend' needs the pitch to reach, as in bend=5 or bend=E5"),
            ("4!bend=4", "bend=4 is the note's own pitch"),
            ("4!bend=x", "bend=x: expected a scale degree"),
            ("4!release", "'release' lets a bend back down, and this note has no bend=P"),
            ("4!bend=5!prebend=6", "not both"),
            ("4!vib=9", "vib=9: the width is in semitones"),
            ("4!vib=wide", "the width is in semitones"),
            ("4!fall=2", "'fall' takes no value"),
            ("4!", "a '!' with no mark after it"),
            ("4!!vib", "a '!' with no mark after it"),
            ("4!hammer", "phrase token 1 '4!hammer': 'hammer' joins a note to the one before it"),
            ("4 r 5!pull", "phrase token 3 '5!pull': 'pull' joins a note to the one before it"),
            ("4 5!hammer!slide", "hammered on or slid into, not both"),
            ("r!vib", "only a note can be marked"),
            ("4 ~!vib", "only a note can be marked"),
            (":0.5!vib 4", "marks follow a note"),
            ("4!palm_mute=3", "'palm_mute' takes no value"),
        ] {
            let error = Phrase::parse(bad, Some(&key), 4, four()).unwrap_err();
            assert!(error.message().contains(reason), "{bad:?}: {error}");
        }
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
    fn a_phrase_is_checked_against_the_songs_own_bars_where_they_change() {
        // Two bars of 4/4, one of 7/8, then 3/4 for as long as it lasts.
        let (four, seven, three) = (four(), TimeSignature::new(7, 8).unwrap(), TimeSignature::new(3, 4).unwrap());
        let bars = [PhraseBar::of(four), PhraseBar::of(four), PhraseBar::of(seven), PhraseBar::of(three)];
        let key = Scale::parse("C").unwrap();
        let through = |text: &str| Phrase::parse_through(text, Some(&key), 4, &bars);
        let line = through("1:4 | 2:2 3:2 | :0.5 4 4 4 4 4 4 4 | 5:3 | :1 6 7 1' | 2':3").unwrap();
        assert_eq!(line.length, 20.5);
        assert_eq!(line.tones.last().unwrap().start, 17.5, "the bars after the list are the last one's again");
        let error = through("1:4 | 2:4 | 3:4 | 4:3").unwrap_err();
        assert!(
            error
                .message()
                .starts_with("bar 3 of the phrase is 4 beats long, but the song's bar there, one of 7/8, has 3.5"),
            "{error}"
        );
        // A bar that a change of time signature cuts short is as long as it is cut to.
        let cut = [PhraseBar::of(four), PhraseBar { length: 2.0, signature: four }, PhraseBar::of(three)];
        assert!(Phrase::parse_through("1:4 | 2:2 | 3:3", Some(&key), 4, &cut).is_ok());
        let error = Phrase::parse_through("1:4 | 2:4 | 3:3", Some(&key), 4, &cut).unwrap_err();
        assert!(error.message().contains("cut short by a change of time signature, has 2"), "{error}");
        // Where a beat falls, and in which bar: past the end the last bar repeats.
        let at = |beat: f64| PhraseBar::at(&bars, beat);
        assert_eq!(
            (at(0.0).0, at(7.9).0, at(8.0), at(11.5).0, at(14.5).0, at(20.0).0),
            (0.0, 4.0, (8.0, bars[2]), 11.5, 14.5, 17.5)
        );
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
            ("1 5?", "phrase token 2 '5?': expected a scale degree"),
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
