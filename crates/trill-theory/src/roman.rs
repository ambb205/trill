//! Roman numerals: chords named by their place in a key, so one progression
//! (`ii7 V7 Imaj7`) works in every key.
//!
//! The conventions are the ones lead sheets and pop analysis use:
//!
//! * Case sets the triad (`IV` major, `iv` minor) and everything after the
//!   numeral is an ordinary chord quality: `V7`, `ii7`, `Imaj7`, `iiø7`,
//!   `vii°7`, `V7b9`, `Isus4`.
//! * A plain numeral is the key's own scale degree. In A minor `VI` is F.
//! * Accidentals move that degree, except that a flat on a degree the key
//!   already lowers is taken as the familiar "counted from major" spelling:
//!   `bVI` in A minor is also F, not Fb.
//! * A diminished chord on the sixth or seventh of a minor key sits on the
//!   raised degree, as it does in practice: `vii°` in A minor is G#dim.
//! * `V/V` is a secondary chord: the first numeral in the key of the second.
//! * `64`, `65`, `43` and `42` are inversions (`V65` is a dominant seventh
//!   over its third).

use crate::chord::{Chord, Quality};
use crate::error::{Error, Result};
use crate::pitch::Interval;
use crate::scale::{Scale, ScaleKind};

/// A Roman numeral worked out in a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Realized {
    pub chord: Chord,
    /// Set when the numeral was read in a way the writer may not expect.
    pub note: Option<String>,
}

const ACCIDENTALS: [char; 4] = ['b', '#', '♭', '♯'];
const NUMERALS: [&str; 7] = ["I", "II", "III", "IV", "V", "VI", "VII"];

/// Whether `text` is written as a Roman numeral rather than a chord symbol.
/// The two cannot be confused: chord roots are the letters A-G.
pub fn looks_roman(text: &str) -> bool {
    text.trim().trim_start_matches(ACCIDENTALS).starts_with(['I', 'V', 'i', 'v'])
}

/// Works out a Roman numeral such as `ii7`, `bVII`, `V7/V` or `iv` in `key`.
pub fn realize(text: &str, key: &Scale) -> Result<Realized> {
    let whole = text.trim();
    if key.size() != 7 {
        return Err(Error::new(format!(
            "'{whole}' is a Roman numeral, which needs a seven-note key; {} has {} notes",
            key.name(),
            key.size()
        )));
    }
    // "V7/V": everything after a slash that starts a numeral is the key to borrow from.
    let secondary = whole.match_indices('/').find(|(index, _)| looks_roman(&whole[index + 1..]));
    match secondary {
        None => realize_in(whole, key, whole),
        Some((slash, _)) => {
            let target = realize(&whole[slash + 1..], key)?.chord;
            let kind = if target.quality.alter_of(3) == Some(-1) { ScaleKind::Minor } else { ScaleKind::Major };
            let realized = realize_in(&whole[..slash], &Scale::new(target.root, kind), whole)?;
            Ok(Realized { chord: realized.chord, note: None })
        }
    }
}

fn realize_in(numeral: &str, key: &Scale, whole: &str) -> Result<Realized> {
    let fail = |problem: &str| Err(Error::new(format!("'{whole}': {problem}")));
    let after_accidentals = numeral.trim_start_matches(ACCIDENTALS);
    let written: i32 = numeral[..numeral.len() - after_accidentals.len()]
        .chars()
        .map(|c| if matches!(c, '#' | '♯') { 1 } else { -1 })
        .sum();
    let letters = after_accidentals.chars().take_while(|c| matches!(c, 'I' | 'V' | 'i' | 'v')).count();
    let (digits, rest) = after_accidentals.split_at(letters);
    let minor = digits.chars().all(|c| c.is_ascii_lowercase());
    if !minor && !digits.chars().all(|c| c.is_ascii_uppercase()) {
        return fail("a Roman numeral is all capitals (major) or all lowercase (minor); expected for example IV or iv");
    }
    let Some(degree) = NUMERALS.iter().position(|known| known.eq_ignore_ascii_case(digits)).map(|index| index + 1)
    else {
        return fail("expected a Roman numeral from I to VII, optionally with b or # in front");
    };

    // Figured-bass inversions; the three for seventh chords imply the seventh.
    let (quality_text, figure) = ["64", "65", "43", "42"]
        .into_iter()
        .find_map(|figure| rest.strip_suffix(figure).map(|quality| (quality, Some(figure))))
        .unwrap_or((rest, None));
    let quality = match figure {
        Some("65" | "43" | "42") => Quality::parse_as(&format!("{quality_text}7"), minor, whole)?,
        _ => Quality::parse_as(quality_text, minor, whole)?,
    };

    let own = key.intervals()[degree - 1].alter as i32;
    let diminished = quality.alter_of(3) == Some(-1) && quality.alter_of(5) == Some(-1);
    let mut note = None;
    let alter = if written < 0 && own < 0 {
        let alter = (written - own).min(0);
        if alter != written {
            note = Some("the flat is counted from the major scale, and this key already has it");
        }
        alter
    } else if written > 0 && own > 0 {
        (written - own).max(0)
    } else if written == 0 && own == -1 && diminished && matches!(degree, 6 | 7) {
        note = Some("a diminished chord here sits on the raised degree");
        1
    } else {
        written
    };
    let root = key.tonic.up(Interval::new(degree as u8, (own + alter) as i8));

    let mut chord = Chord::new(root, quality);
    if let Some(figure) = figure {
        let bass_degree = match figure {
            "65" => 3,
            "64" | "43" => 5,
            _ => 7,
        };
        let Some(tone) = chord.quality.tones().iter().copied().find(|tone| tone.number == bass_degree) else {
            return fail("this inversion figure needs a chord tone the chord does not have");
        };
        chord.bass = Some(root.up(tone));
    }
    if quality_text == "6" && figure.is_none() {
        note = Some("read as an added-sixth chord; write a first inversion as a chord symbol with a slash bass");
    }
    let note = note.map(|reason| format!("{whole} in {} is {}: {reason}", key.name(), chord.name()));
    Ok(Realized { chord, note })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(key: &str, numerals: &str) -> String {
        let key = Scale::parse(key).unwrap();
        numerals
            .split_whitespace()
            .map(|numeral| realize(numeral, &key).unwrap_or_else(|error| panic!("{numeral}: {error}")).chord.name())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn numerals_in_major_keys() {
        let cases = [
            ("C major", "I ii iii IV V vi vii°", "C Dm Em F G Am Bdim"),
            ("C major", "ii7 V7 Imaj7", "Dm7 G7 Cmaj7"),
            ("Eb major", "ii7 V7 Imaj7", "Fm7 Bb7 Ebmaj7"),
            ("F# major", "I IV V vi", "F# B C# D#m"),
            ("C major", "bVII bVI bIII bII", "Bb Ab Eb Db"),
            ("C major", "iv iiø7 #iv° IVmaj7", "Fm Dm7b5 F#dim Fmaj7"),
            ("C major", "V/V V7/ii V7/vi vii°7/V V7/IV", "D A7 E7 F#dim7 C7"),
            ("G major", "V/V V7/V/V", "A E7"),
            ("C major", "V7sus4 V7b9 I6 Iadd9 vi9 IM7", "G7sus4 G7b9 C6 Cadd9 Am9 Cmaj7"),
            ("C major", "I64 V65 V43 V42 ii65 viiø65", "C/G G7/B G7/D G7/F Dm7/F Bm7b5/D"),
            ("Bb major", "I vi IV V", "Bb Gm Eb F"),
            ("A major", "bVII bIII", "G C"),
        ];
        for (key, numerals, expected) in cases {
            assert_eq!(names(key, numerals), expected, "{numerals} in {key}");
        }
    }

    #[test]
    fn numerals_in_minor_keys_and_modes() {
        let cases = [
            ("A minor", "i iv v V V7", "Am Dm Em E E7"),
            ("A minor", "i bVI bIII bVII", "Am F C G"),
            ("A minor", "i VI III VII", "Am F C G"),
            ("F# minor", "i bVI bIII bVII", "F#m D A E"),
            ("C minor", "i iv V7 bII", "Cm Fm G7 Db"),
            ("A minor", "ii° iiø7 vii° vii°7 vi° #vi° #vii°", "Bdim Bm7b5 G#dim G#dim7 F#dim F#dim G#dim"),
            ("A minor", "IV i6 imaj7 V/III V7/iv", "D Am6 AmMaj7 G A7"),
            ("D dorian", "i IV bVII", "Dm G C"),
            ("E phrygian", "i II bII", "Em F F"),
            ("G mixolydian", "I VII bVII v", "G F F Dm"),
            ("F lydian", "I II #IV°", "F G Bdim"),
            ("A harmonic minor", "i V vii°", "Am E G#dim"),
        ];
        for (key, numerals, expected) in cases {
            assert_eq!(names(key, numerals), expected, "{numerals} in {key}");
        }
    }

    #[test]
    fn surprising_readings_are_explained() {
        let a_minor = Scale::parse("A minor").unwrap();
        let note = |numeral: &str| realize(numeral, &a_minor).unwrap().note;
        assert_eq!(note("i"), None);
        assert_eq!(note("VI"), None);
        assert!(note("bVI").unwrap().starts_with("bVI in A minor is F: the flat is counted from the major scale"));
        assert!(note("vii°").unwrap().starts_with("vii° in A minor is G#dim"));
        assert!(note("i6").unwrap().contains("added-sixth"));
        let c_major = Scale::parse("C major").unwrap();
        assert_eq!(realize("bVI", &c_major).unwrap().note, None, "in major the flat really lowers the degree");
    }

    #[test]
    fn malformed_numerals_are_rejected() {
        let c_major = Scale::parse("C major").unwrap();
        for (bad, reason) in [
            ("IIII", "from I to VII"),
            ("VIII", "from I to VII"),
            ("Iv", "all capitals"),
            ("V/", "is not something a chord symbol can end with"),
            ("Vfoo", "is not something a chord symbol can end with"),
            ("V7/X", "is not something a chord symbol can end with"),
        ] {
            let error = realize(bad, &c_major).unwrap_err();
            assert!(error.message().contains(&format!("'{bad}'")), "{bad}: {error}");
            assert!(error.message().contains(reason), "{bad}: {error}");
        }
        let pentatonic = Scale::parse("A minor pentatonic").unwrap();
        let error = realize("ii7", &pentatonic).unwrap_err();
        assert!(error.message().contains("seven-note key") && error.message().contains("'ii7'"), "{error}");
    }

    #[test]
    fn numerals_are_told_apart_from_chord_symbols() {
        for roman in ["I", "ii7", "bVII", "#iv°", "V7/V", "vi"] {
            assert!(looks_roman(roman), "{roman}");
        }
        for symbol in ["C", "Bb7", "Dm", "F#m7b5", "N.C.", "%", "."] {
            assert!(!looks_roman(symbol), "{symbol}");
        }
    }
}
