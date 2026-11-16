//! Scales and keys: which notes belong, what they are called, and how to
//! get from a scale degree to a pitch and back.

use crate::error::{Error, Result, list};
use crate::pitch::{CHROMATIC_DEGREES, Interval, Letter, NoteName, pitch_class};

const fn i(number: u8, alter: i8) -> Interval {
    Interval::new(number, alter)
}

/// The shape of a scale, independent of its tonic.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ScaleKind {
    Major,
    /// Natural minor.
    Minor,
    HarmonicMinor,
    /// The ascending (jazz) form: minor with a major sixth and seventh.
    MelodicMinor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    WholeTone,
    /// Whole-half diminished: the scale of a diminished seventh chord.
    Diminished,
    /// Half-whole diminished: the scale of a dominant 7b9 chord.
    DominantDiminished,
}

impl ScaleKind {
    pub const ALL: [ScaleKind; 15] = [
        ScaleKind::Major,
        ScaleKind::Minor,
        ScaleKind::HarmonicMinor,
        ScaleKind::MelodicMinor,
        ScaleKind::Dorian,
        ScaleKind::Phrygian,
        ScaleKind::Lydian,
        ScaleKind::Mixolydian,
        ScaleKind::Locrian,
        ScaleKind::MajorPentatonic,
        ScaleKind::MinorPentatonic,
        ScaleKind::Blues,
        ScaleKind::WholeTone,
        ScaleKind::Diminished,
        ScaleKind::DominantDiminished,
    ];

    /// The name used in replies and accepted first when parsing.
    pub fn name(self) -> &'static str {
        match self {
            ScaleKind::Major => "major",
            ScaleKind::Minor => "minor",
            ScaleKind::HarmonicMinor => "harmonic minor",
            ScaleKind::MelodicMinor => "melodic minor",
            ScaleKind::Dorian => "dorian",
            ScaleKind::Phrygian => "phrygian",
            ScaleKind::Lydian => "lydian",
            ScaleKind::Mixolydian => "mixolydian",
            ScaleKind::Locrian => "locrian",
            ScaleKind::MajorPentatonic => "major pentatonic",
            ScaleKind::MinorPentatonic => "minor pentatonic",
            ScaleKind::Blues => "blues",
            ScaleKind::WholeTone => "whole tone",
            ScaleKind::Diminished => "diminished",
            ScaleKind::DominantDiminished => "dominant diminished",
        }
    }

    /// Other spellings people use for the same scale.
    fn aliases(self) -> &'static [&'static str] {
        match self {
            ScaleKind::Major => &["maj", "ionian"],
            ScaleKind::Minor => &["min", "natural minor", "aeolian"],
            ScaleKind::HarmonicMinor => &["harmonic"],
            ScaleKind::MelodicMinor => &["melodic", "jazz minor"],
            ScaleKind::MajorPentatonic => &["pentatonic", "pentatonic major", "major pent"],
            ScaleKind::MinorPentatonic => &["pentatonic minor", "minor pent"],
            ScaleKind::Blues => &["minor blues"],
            ScaleKind::WholeTone => &["wholetone"],
            ScaleKind::Diminished => &["whole half", "whole half diminished", "octatonic"],
            ScaleKind::DominantDiminished => &["half whole", "half whole diminished"],
            _ => &[],
        }
    }

    /// The scale as degrees of the major scale, which is also how each note
    /// gets its letter.
    pub fn intervals(self) -> &'static [Interval] {
        const MAJOR: &[Interval] = &[i(1, 0), i(2, 0), i(3, 0), i(4, 0), i(5, 0), i(6, 0), i(7, 0)];
        const MINOR: &[Interval] = &[i(1, 0), i(2, 0), i(3, -1), i(4, 0), i(5, 0), i(6, -1), i(7, -1)];
        const HARMONIC_MINOR: &[Interval] = &[i(1, 0), i(2, 0), i(3, -1), i(4, 0), i(5, 0), i(6, -1), i(7, 0)];
        const MELODIC_MINOR: &[Interval] = &[i(1, 0), i(2, 0), i(3, -1), i(4, 0), i(5, 0), i(6, 0), i(7, 0)];
        const DORIAN: &[Interval] = &[i(1, 0), i(2, 0), i(3, -1), i(4, 0), i(5, 0), i(6, 0), i(7, -1)];
        const PHRYGIAN: &[Interval] = &[i(1, 0), i(2, -1), i(3, -1), i(4, 0), i(5, 0), i(6, -1), i(7, -1)];
        const LYDIAN: &[Interval] = &[i(1, 0), i(2, 0), i(3, 0), i(4, 1), i(5, 0), i(6, 0), i(7, 0)];
        const MIXOLYDIAN: &[Interval] = &[i(1, 0), i(2, 0), i(3, 0), i(4, 0), i(5, 0), i(6, 0), i(7, -1)];
        const LOCRIAN: &[Interval] = &[i(1, 0), i(2, -1), i(3, -1), i(4, 0), i(5, -1), i(6, -1), i(7, -1)];
        const MAJOR_PENTATONIC: &[Interval] = &[i(1, 0), i(2, 0), i(3, 0), i(5, 0), i(6, 0)];
        const MINOR_PENTATONIC: &[Interval] = &[i(1, 0), i(3, -1), i(4, 0), i(5, 0), i(7, -1)];
        const BLUES: &[Interval] = &[i(1, 0), i(3, -1), i(4, 0), i(5, -1), i(5, 0), i(7, -1)];
        const WHOLE_TONE: &[Interval] = &[i(1, 0), i(2, 0), i(3, 0), i(4, 1), i(5, 1), i(7, -1)];
        const DIMINISHED: &[Interval] = &[i(1, 0), i(2, 0), i(3, -1), i(4, 0), i(5, -1), i(6, -1), i(6, 0), i(7, 0)];
        const DOMINANT_DIMINISHED: &[Interval] =
            &[i(1, 0), i(2, -1), i(3, -1), i(3, 0), i(4, 1), i(5, 0), i(6, 0), i(7, -1)];
        match self {
            ScaleKind::Major => MAJOR,
            ScaleKind::Minor => MINOR,
            ScaleKind::HarmonicMinor => HARMONIC_MINOR,
            ScaleKind::MelodicMinor => MELODIC_MINOR,
            ScaleKind::Dorian => DORIAN,
            ScaleKind::Phrygian => PHRYGIAN,
            ScaleKind::Lydian => LYDIAN,
            ScaleKind::Mixolydian => MIXOLYDIAN,
            ScaleKind::Locrian => LOCRIAN,
            ScaleKind::MajorPentatonic => MAJOR_PENTATONIC,
            ScaleKind::MinorPentatonic => MINOR_PENTATONIC,
            ScaleKind::Blues => BLUES,
            ScaleKind::WholeTone => WHOLE_TONE,
            ScaleKind::Diminished => DIMINISHED,
            ScaleKind::DominantDiminished => DOMINANT_DIMINISHED,
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == text || kind.aliases().contains(&text))
    }
}

/// Which way [`Scale::snap`] moves a pitch that is not in the scale.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Snap {
    /// To the closest scale note; halfway cases go down.
    Nearest,
    Up,
    Down,
}

/// A scale on a tonic. A key is simply a major or minor scale.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scale {
    pub tonic: NoteName,
    pub kind: ScaleKind,
}

impl Scale {
    pub const fn new(tonic: NoteName, kind: ScaleKind) -> Self {
        Self { tonic, kind }
    }

    /// Parses `"A minor"`, `"F# dorian"`, `"Bb major pentatonic"`, and the
    /// short key names `"C"`, `"Am"`, `"Ebm"`. A key that could only be
    /// written with double sharps or flats (G# major) becomes its everyday
    /// twin (Ab major).
    pub fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let mut chars = trimmed.chars();
        let letter = chars.next().and_then(|c| Letter::from_char(c.to_ascii_uppercase())).ok_or_else(|| {
            Error::new(format!(
                "'{text}' is not a key or scale; expected a tonic and a scale name, like \"A minor\" or \"F# dorian\""
            ))
        })?;
        let after_letter = chars.as_str();
        let accidentals = after_letter.chars().take_while(|c| matches!(c, '#' | 'b' | '♯' | '♭')).count();
        if accidentals > 2 {
            return Err(Error::new(format!("'{text}': a tonic takes at most two sharps or flats")));
        }
        let accidental: i32 =
            after_letter.chars().take(accidentals).map(|c| if matches!(c, '#' | '♯') { 1 } else { -1 }).sum();
        let rest: String = after_letter.chars().skip(accidentals).collect();
        let tonic = NoteName::new(letter, accidental as i8);

        // "Am" is A minor but "AM" is not: case matters only for the one-letter form.
        let kind = match rest.trim() {
            "" | "M" => ScaleKind::Major,
            "m" => ScaleKind::Minor,
            other => {
                let wanted = normalize(other);
                ScaleKind::parse(&wanted).ok_or_else(|| {
                    Error::new(format!(
                        "'{text}': unknown scale '{}'; known scales: {}",
                        other.trim(),
                        list(ScaleKind::ALL.map(ScaleKind::name))
                    ))
                })?
            }
        };
        Ok(Self { tonic, kind }.practical())
    }

    /// The same scale under the tonic's other name when this one needs
    /// double accidentals and that one does not.
    fn practical(self) -> Self {
        let doubles = |scale: &Scale| scale.names().iter().any(|name| name.accidental.abs() >= 2);
        let other = Self { tonic: self.tonic.enharmonic(), kind: self.kind };
        if doubles(&self) && !doubles(&other) { other } else { self }
    }

    /// `"F# minor"`.
    pub fn name(&self) -> String {
        format!("{} {}", self.tonic, self.kind.name())
    }

    pub fn intervals(&self) -> &'static [Interval] {
        self.kind.intervals()
    }

    /// How many notes the scale has in an octave.
    pub fn size(&self) -> usize {
        self.intervals().len()
    }

    /// The notes of the scale, spelled, from the tonic up.
    pub fn names(&self) -> Vec<NoteName> {
        self.intervals().iter().map(|interval| self.tonic.up(*interval)).collect()
    }

    pub fn pitch_classes(&self) -> Vec<u8> {
        let tonic = self.tonic.pitch_class() as i32;
        self.intervals().iter().map(|interval| (tonic + interval.semitones()).rem_euclid(12) as u8).collect()
    }

    pub fn contains(&self, pitch: u8) -> bool {
        self.pitch_classes().contains(&pitch_class(pitch))
    }

    /// The pitch of a scale degree. Degree 1 is the tonic in `octave` (the
    /// octave of its letter, so A minor in octave 4 starts on A4); degrees
    /// count on past the top of the scale into the next octave, and 0 or
    /// below go down from the tonic. `alter` raises or lowers the result by
    /// semitones. `None` when the pitch is outside the MIDI range.
    pub fn degree_pitch(&self, degree: i32, alter: i32, octave: i32) -> Option<u8> {
        let size = self.size() as i32;
        let index = degree - 1;
        let tonic = (octave + 1) * 12 + self.tonic.letter.semitones() + self.tonic.accidental as i32;
        let pitch =
            tonic + 12 * index.div_euclid(size) + self.intervals()[index.rem_euclid(size) as usize].semitones() + alter;
        (0..=127).contains(&pitch).then_some(pitch as u8)
    }

    /// The 1-based degree of a pitch that belongs to the scale.
    pub fn degree_of(&self, pitch: u8) -> Option<usize> {
        self.pitch_classes().iter().position(|pc| *pc == pitch_class(pitch)).map(|index| index + 1)
    }

    /// Moves a pitch into the scale. Pitches already in it are unchanged.
    pub fn snap(&self, pitch: u8, toward: Snap) -> u8 {
        let fits = |candidate: i32| (0..=127).contains(&candidate) && self.contains(candidate as u8);
        let pitch = pitch as i32;
        for distance in 0..12 {
            let (below, above) = (pitch - distance, pitch + distance);
            let ordered = match toward {
                Snap::Nearest => [Some(below), Some(above)],
                Snap::Down => [Some(below), None],
                Snap::Up => [Some(above), None],
            };
            if let Some(found) = ordered.into_iter().flatten().find(|candidate| fits(*candidate)) {
                return found as u8;
            }
        }
        // Only reachable at the very edge of the MIDI range, where the
        // asked-for direction runs out of keys.
        (0..=127).rev().find(|candidate| fits(*candidate)).unwrap_or(pitch) as u8
    }

    /// The name of a pitch class in this key: the scale's own spelling for
    /// its notes, and the usual chromatic degree (b3, #4, b7...) for the rest.
    pub fn spell_pitch_class(&self, pitch_class: u8) -> NoteName {
        let pitch_class = pitch_class % 12;
        if let Some(index) = self.pitch_classes().iter().position(|pc| *pc == pitch_class) {
            return self.tonic.up(self.intervals()[index]);
        }
        let above_tonic = (pitch_class as i32 - self.tonic.pitch_class() as i32).rem_euclid(12);
        self.tonic.up(CHROMATIC_DEGREES[above_tonic as usize])
    }

    /// The name of a pitch in this key, with its octave: `"Bb3"`.
    pub fn spell(&self, pitch: u8) -> String {
        self.spell_pitch_class(pitch_class(pitch)).spell(pitch)
    }

    /// The relative minor of a major key and the relative major of a minor
    /// key: the same notes around another tonic.
    pub fn relative(&self) -> Option<Self> {
        match self.kind {
            ScaleKind::Major => Some(Self::new(self.tonic.up(Interval::MAJOR_SIXTH), ScaleKind::Minor)),
            ScaleKind::Minor => Some(Self::new(self.tonic.up(Interval::MINOR_THIRD), ScaleKind::Major)),
            _ => None,
        }
    }
}

/// Lowercases and reduces separators to single spaces, so `"Whole-Tone"`,
/// `"whole_tone"` and `"whole  tone"` are the same name.
fn normalize(text: &str) -> String {
    let spaced: String =
        text.chars().map(|c| if c == '_' || c == '-' { ' ' } else { c.to_ascii_lowercase() }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale(text: &str) -> Scale {
        Scale::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn spelled(text: &str) -> String {
        scale(text).names().iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn scales_are_spelled_the_way_they_are_written() {
        let cases = [
            ("C major", "C D E F G A B"),
            ("F major", "F G A Bb C D E"),
            ("Bb major", "Bb C D Eb F G A"),
            ("Eb major", "Eb F G Ab Bb C D"),
            ("Gb major", "Gb Ab Bb Cb Db Eb F"),
            ("F# major", "F# G# A# B C# D# E#"),
            ("B major", "B C# D# E F# G# A#"),
            ("A minor", "A B C D E F G"),
            ("C minor", "C D Eb F G Ab Bb"),
            ("F# minor", "F# G# A B C# D E"),
            ("Bb minor", "Bb C Db Eb F Gb Ab"),
            ("D# minor", "D# E# F# G# A# B C#"),
            ("A harmonic minor", "A B C D E F G#"),
            ("C harmonic minor", "C D Eb F G Ab B"),
            ("A melodic minor", "A B C D E F# G#"),
            ("D dorian", "D E F G A B C"),
            ("E phrygian", "E F G A B C D"),
            ("F lydian", "F G A B C D E"),
            ("G mixolydian", "G A B C D E F"),
            ("B locrian", "B C D E F G A"),
            ("Eb lydian", "Eb F G A Bb C D"),
            ("C major pentatonic", "C D E G A"),
            ("A minor pentatonic", "A C D E G"),
            ("A blues", "A C D Eb E G"),
            ("C whole tone", "C D E F# G# Bb"),
            ("C diminished", "C D Eb F Gb Ab A B"),
            ("C dominant diminished", "C Db Eb E F# G A Bb"),
        ];
        for (name, expected) in cases {
            assert_eq!(spelled(name), expected, "{name}");
        }
    }

    #[test]
    fn key_names_parse_in_their_common_forms() {
        assert_eq!(scale("Am"), scale("A minor"));
        assert_eq!(scale("a minor"), scale("A minor"));
        assert_eq!(scale("C"), scale("C major"));
        assert_eq!(scale("Ebm"), scale("Eb minor"));
        assert_eq!(scale("bb minor"), scale("Bb minor"));
        assert_eq!(scale("F# Minor"), scale("F# min"));
        assert_eq!(scale("D aeolian"), scale("D minor"));
        assert_eq!(scale("G ionian"), scale("G major"));
        assert_eq!(scale("E whole-tone"), scale("E whole_tone"));
        assert_eq!(scale("A pentatonic minor").kind, ScaleKind::MinorPentatonic);
        assert_eq!(scale("C half whole diminished").kind, ScaleKind::DominantDiminished);
        assert_eq!(scale("B blues").name(), "B blues");
        assert_eq!(scale("F#m").name(), "F# minor");
    }

    #[test]
    fn theoretical_keys_become_their_everyday_twins() {
        assert_eq!(scale("G# major").name(), "Ab major");
        assert_eq!(scale("D# major").name(), "Eb major");
        assert_eq!(scale("Db minor").name(), "C# minor");
        // Seven sharps is awkward but real.
        assert_eq!(scale("C# major").name(), "C# major");
        assert_eq!(scale("A# minor").name(), "A# minor");
    }

    #[test]
    fn unreadable_keys_say_what_was_expected() {
        for bad in ["", "H minor", "C foo", "A  super locrian", "7", "Cbbb major"] {
            let error = Scale::parse(bad).unwrap_err();
            assert!(error.message().contains(&format!("'{bad}'")), "{bad}: {error}");
        }
        assert!(Scale::parse("C foo").unwrap_err().message().contains("mixolydian"));
    }

    #[test]
    fn degrees_map_to_pitches_and_back() {
        let a_minor = scale("A minor");
        let pitches: Vec<u8> = (1..=8).map(|degree| a_minor.degree_pitch(degree, 0, 4).unwrap()).collect();
        assert_eq!(pitches, [69, 71, 72, 74, 76, 77, 79, 81]);
        assert_eq!(a_minor.degree_pitch(0, 0, 4), Some(67), "degree 0 is the note below the tonic");
        assert_eq!(a_minor.degree_pitch(-6, 0, 4), Some(57), "seven steps down is the tonic an octave lower");
        assert_eq!(a_minor.degree_pitch(7, 1, 4), Some(80), "#7 is the leading tone");
        assert_eq!(a_minor.degree_pitch(1, 0, 10), None);
        for (degree, pitch) in pitches.iter().take(7).enumerate() {
            assert_eq!(a_minor.degree_of(*pitch), Some(degree + 1));
        }
        assert_eq!(a_minor.degree_of(70), None);

        let pentatonic = scale("C major pentatonic");
        let run: Vec<u8> = (1..=6).map(|degree| pentatonic.degree_pitch(degree, 0, 4).unwrap()).collect();
        assert_eq!(run, [60, 62, 64, 67, 69, 72]);
        assert_eq!(scale("Cb major").degree_pitch(1, 0, 4), Some(59));
    }

    #[test]
    fn snapping_moves_only_what_is_outside() {
        let c_major = scale("C major");
        assert_eq!(c_major.snap(60, Snap::Nearest), 60);
        assert_eq!(c_major.snap(61, Snap::Nearest), 60, "halfway cases go down");
        assert_eq!(c_major.snap(61, Snap::Up), 62);
        assert_eq!(c_major.snap(66, Snap::Down), 65);
        assert_eq!(c_major.snap(70, Snap::Up), 71);
        let pentatonic = scale("A minor pentatonic");
        assert_eq!(pentatonic.snap(71, Snap::Nearest), 72, "B is a semitone from C and a tone from A");
        assert_eq!(pentatonic.snap(66, Snap::Nearest), 67);
        assert_eq!(scale("F# major").snap(127, Snap::Up), 126, "at the top of the range it comes back down");
        for pitch in 0..=127u8 {
            assert!(c_major.contains(c_major.snap(pitch, Snap::Nearest)));
        }
    }

    #[test]
    fn pitches_are_named_in_the_key() {
        assert_eq!(scale("F major").spell(58), "Bb3");
        assert_eq!(scale("B major").spell(58), "A#3");
        assert_eq!(scale("A minor").spell(68), "G#4", "the leading tone of a minor key is a sharp");
        assert_eq!(scale("C major").spell(66), "F#4");
        assert_eq!(scale("C major").spell(70), "Bb4");
        assert_eq!(scale("Gb major").spell(71), "Cb5");
        assert_eq!(scale("C major").relative(), Some(scale("A minor")));
        assert_eq!(scale("F# minor").relative(), Some(scale("A major")));
        assert_eq!(scale("D dorian").relative(), None);
    }
}
