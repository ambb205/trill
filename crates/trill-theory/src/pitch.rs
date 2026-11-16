//! Spelled note names and intervals.
//!
//! A MIDI pitch says which key to press; a [`NoteName`] says what a musician
//! would call it. Bb and A# are the same key but different names, and chords
//! and scales are only readable when each tone sits on the letter its role
//! calls for (the third of F# minor is A, not Bbb or G##). Everything here
//! therefore carries a letter and an accidental, and an [`Interval`] carries
//! the number of letters it spans as well as its size.

use crate::error::{Error, Result};
use std::fmt;

/// One of the seven note letters.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Letter {
    C,
    D,
    E,
    F,
    G,
    A,
    B,
}

impl Letter {
    pub const ALL: [Letter; 7] = [Letter::C, Letter::D, Letter::E, Letter::F, Letter::G, Letter::A, Letter::B];

    /// Semitones above C of each natural letter: the C major scale.
    const SEMITONES: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];

    pub fn from_char(c: char) -> Option<Self> {
        Some(match c {
            'C' => Letter::C,
            'D' => Letter::D,
            'E' => Letter::E,
            'F' => Letter::F,
            'G' => Letter::G,
            'A' => Letter::A,
            'B' => Letter::B,
            _ => return None,
        })
    }

    pub fn as_char(self) -> char {
        ['C', 'D', 'E', 'F', 'G', 'A', 'B'][self as usize]
    }

    /// Semitones above C.
    pub fn semitones(self) -> i32 {
        Self::SEMITONES[self as usize]
    }

    /// The letter `steps` letters up (down when negative), wrapping at B.
    pub fn stepped(self, steps: i32) -> Self {
        Self::ALL[(self as i32 + steps).rem_euclid(7) as usize]
    }
}

/// A spelled pitch class: a letter with sharps (positive) or flats (negative).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct NoteName {
    pub letter: Letter,
    pub accidental: i8,
}

impl NoteName {
    pub const fn new(letter: Letter, accidental: i8) -> Self {
        Self { letter, accidental }
    }

    /// Splits a note name off the front of `text`: an uppercase letter and
    /// any accidentals that follow it. Returns the name and what is left.
    pub(crate) fn split_prefix(text: &str) -> Option<(Self, &str)> {
        let mut chars = text.chars();
        let letter = Letter::from_char(chars.next()?)?;
        let rest = chars.as_str();
        let mut accidental = 0i8;
        let mut used = 0;
        for c in rest.chars() {
            match c {
                '#' | '♯' => accidental += 1,
                'b' | '♭' => accidental -= 1,
                _ => break,
            }
            used += c.len_utf8();
            if accidental.abs() > 2 {
                return None;
            }
        }
        Some((Self { letter, accidental }, &rest[used..]))
    }

    /// Parses `"C"`, `"F#"`, `"Bb"`. The letter must be uppercase, so that a
    /// lowercase `b` can only ever mean a flat.
    pub fn parse(text: &str) -> Result<Self> {
        match Self::split_prefix(text.trim()) {
            Some((name, "")) => Ok(name),
            _ => Err(Error::new(format!("'{text}' is not a note name; expected a letter A-G with optional # or b"))),
        }
    }

    /// 0 for C up to 11 for B.
    pub fn pitch_class(self) -> u8 {
        (self.letter.semitones() + self.accidental as i32).rem_euclid(12) as u8
    }

    /// The usual name for a pitch class when nothing says how to spell it:
    /// C, Db, D, Eb, E, F, F#, G, Ab, A, Bb, B.
    pub fn of_pitch_class(pitch_class: u8) -> Self {
        NoteName::new(Letter::C, 0).up(CHROMATIC_DEGREES[(pitch_class % 12) as usize])
    }

    /// The name an interval above this one, on the letter the interval calls
    /// for: a third above C is always some kind of E.
    pub fn up(self, interval: Interval) -> Self {
        let letter = self.letter.stepped(interval.number as i32 - 1);
        let target = self.letter.semitones() + self.accidental as i32 + interval.semitones();
        let mut accidental = (target - letter.semitones()).rem_euclid(12);
        if accidental > 6 {
            accidental -= 12;
        }
        Self { letter, accidental: accidental as i8 }.simplified()
    }

    /// Names that would need three or more accidentals are respelled the
    /// plain way; nobody reads Ebbb.
    pub fn simplified(self) -> Self {
        if self.accidental.abs() <= 2 { self } else { Self::of_pitch_class(self.pitch_class()) }
    }

    /// The other common name of a black key (C# for Db, Db for C#). Natural
    /// names are returned unchanged.
    pub fn enharmonic(self) -> Self {
        match self.accidental {
            0 => self,
            accidental => {
                let letter = self.letter.stepped(accidental.signum() as i32);
                let mut other = (self.pitch_class() as i32 - letter.semitones()).rem_euclid(12);
                if other > 6 {
                    other -= 12;
                }
                Self { letter, accidental: other as i8 }
            }
        }
    }

    /// This name with the octave number that `pitch` has under it, such as
    /// `"Bb3"`. The octave follows the letter, so Cb4 is the key below C4.
    pub fn spell(self, pitch: u8) -> String {
        let octave = (pitch as i32 - self.accidental as i32).div_euclid(12) - 1;
        format!("{self}{octave}")
    }

    /// The MIDI pitch of this name in `octave` (C4 = 60), if it is in the
    /// MIDI range.
    pub fn pitch(self, octave: i32) -> Option<u8> {
        let pitch = (octave + 1) * 12 + self.letter.semitones() + self.accidental as i32;
        (0..=127).contains(&pitch).then_some(pitch as u8)
    }
}

impl fmt::Display for NoteName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.accidental < 0 { 'b' } else { '#' };
        write!(f, "{}", self.letter.as_char())?;
        (0..self.accidental.unsigned_abs()).try_for_each(|_| write!(f, "{sign}"))
    }
}

/// An interval above a root, described the way chord symbols and scale
/// formulas do: a degree number and how far it is altered from the major
/// scale. `b3` is `Interval::new(3, -1)`, `#11` is `Interval::new(11, 1)`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Interval {
    /// 1 = unison, 3 = third, 9 = ninth...
    pub number: u8,
    /// Semitones above (positive) or below the major-scale degree.
    pub alter: i8,
}

impl Interval {
    pub const UNISON: Self = Self::new(1, 0);
    pub const MAJOR_SECOND: Self = Self::new(2, 0);
    pub const MINOR_THIRD: Self = Self::new(3, -1);
    pub const MAJOR_THIRD: Self = Self::new(3, 0);
    pub const PERFECT_FOURTH: Self = Self::new(4, 0);
    pub const DIMINISHED_FIFTH: Self = Self::new(5, -1);
    pub const PERFECT_FIFTH: Self = Self::new(5, 0);
    pub const AUGMENTED_FIFTH: Self = Self::new(5, 1);
    pub const MAJOR_SIXTH: Self = Self::new(6, 0);
    pub const DIMINISHED_SEVENTH: Self = Self::new(7, -2);
    pub const MINOR_SEVENTH: Self = Self::new(7, -1);
    pub const MAJOR_SEVENTH: Self = Self::new(7, 0);

    pub const fn new(number: u8, alter: i8) -> Self {
        Self { number, alter }
    }

    /// Size in semitones.
    pub fn semitones(self) -> i32 {
        let steps = self.number.max(1) as i32 - 1;
        12 * (steps / 7) + Letter::SEMITONES[(steps % 7) as usize] + self.alter as i32
    }

    /// Pitch class of this interval above a root on C.
    pub fn pitch_class(self) -> u8 {
        self.semitones().rem_euclid(12) as u8
    }

    /// `"b3"`, `"5"`, `"#11"`.
    pub fn label(self) -> String {
        let sign = if self.alter < 0 { "b" } else { "#" };
        format!("{}{}", sign.repeat(self.alter.unsigned_abs() as usize), self.number)
    }
}

/// How each of the twelve pitch classes above a tonic is usually spelled:
/// 1 b2 2 b3 3 4 #4 5 b6 6 b7 7.
pub(crate) const CHROMATIC_DEGREES: [Interval; 12] = [
    Interval::new(1, 0),
    Interval::new(2, -1),
    Interval::new(2, 0),
    Interval::new(3, -1),
    Interval::new(3, 0),
    Interval::new(4, 0),
    Interval::new(4, 1),
    Interval::new(5, 0),
    Interval::new(6, -1),
    Interval::new(6, 0),
    Interval::new(7, -1),
    Interval::new(7, 0),
];

/// 0 for C up to 11 for B.
pub fn pitch_class(pitch: u8) -> u8 {
    pitch % 12
}

/// The lowest pitch at or above `floor` with the given pitch class. The
/// result can exceed the MIDI range; callers that build voicings check it.
pub(crate) fn at_or_above(pitch_class: u8, floor: i32) -> i32 {
    floor + (pitch_class as i32 - floor).rem_euclid(12)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> NoteName {
        NoteName::parse(text).unwrap()
    }

    #[test]
    fn names_parse_and_print() {
        for text in ["C", "F#", "Bb", "Ebb", "G##", "Cb", "E#"] {
            assert_eq!(name(text).to_string(), text);
        }
        assert_eq!(name("B♭"), name("Bb"));
        assert_eq!(name("Cb").pitch_class(), 11);
        assert_eq!(name("B#").pitch_class(), 0);
        for bad in ["", "H", "c", "C4", "Cbbb", "#", "Bb b"] {
            let error = NoteName::parse(bad).unwrap_err();
            assert!(error.message().contains(&format!("'{bad}'")), "{error}");
        }
    }

    #[test]
    fn intervals_keep_their_letter() {
        let cases = [
            ("C", Interval::MAJOR_THIRD, "E"),
            ("C", Interval::MINOR_THIRD, "Eb"),
            ("F#", Interval::MINOR_THIRD, "A"),
            ("Bb", Interval::MINOR_SEVENTH, "Ab"),
            ("C#", Interval::DIMINISHED_FIFTH, "G"),
            ("Eb", Interval::PERFECT_FIFTH, "Bb"),
            ("B", Interval::MAJOR_THIRD, "D#"),
            ("C", Interval::DIMINISHED_SEVENTH, "Bbb"),
            ("E", Interval::new(9, 1), "F##"),
            ("E", Interval::new(13, -1), "C"),
            ("Gb", Interval::PERFECT_FOURTH, "Cb"),
            ("F", Interval::new(11, 1), "B"),
        ];
        for (root, interval, expected) in cases {
            assert_eq!(name(root).up(interval).to_string(), expected, "{root} + {}", interval.label());
        }
    }

    #[test]
    fn interval_sizes() {
        let sizes = [
            (Interval::UNISON, 0),
            (Interval::MINOR_THIRD, 3),
            (Interval::PERFECT_FIFTH, 7),
            (Interval::MINOR_SEVENTH, 10),
            (Interval::new(9, -1), 13),
            (Interval::new(9, 0), 14),
            (Interval::new(11, 1), 18),
            (Interval::new(13, 0), 21),
        ];
        for (interval, semitones) in sizes {
            assert_eq!(interval.semitones(), semitones, "{}", interval.label());
        }
        assert_eq!(Interval::new(13, -1).label(), "b13");
        assert_eq!(Interval::new(7, -2).label(), "bb7");
    }

    #[test]
    fn spelling_puts_the_octave_on_the_letter() {
        assert_eq!(name("Bb").spell(58), "Bb3");
        assert_eq!(name("Cb").spell(59), "Cb4");
        assert_eq!(name("B#").spell(60), "B#3");
        assert_eq!(name("C").pitch(4), Some(60));
        assert_eq!(name("Cb").pitch(4), Some(59));
        assert_eq!(name("G").pitch(9), Some(127));
        assert_eq!(name("A").pitch(9), None);
    }

    #[test]
    fn default_and_enharmonic_spellings() {
        let default: Vec<String> = (0..12).map(|pc| NoteName::of_pitch_class(pc).to_string()).collect();
        assert_eq!(default.join(" "), "C Db D Eb E F F# G Ab A Bb B");
        assert_eq!(name("C#").enharmonic(), name("Db"));
        assert_eq!(name("Gb").enharmonic(), name("F#"));
        assert_eq!(name("D").enharmonic(), name("D"));
        assert_eq!(at_or_above(9, 48), 57);
        assert_eq!(at_or_above(0, 48), 48);
    }
}
