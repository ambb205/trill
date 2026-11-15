//! Pitch names. Middle C is C4 = MIDI note 60 (scientific pitch notation).

const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// `60` → `"C4"`.
pub fn pitch_name(pitch: u8) -> String {
    format!("{}{}", NAMES[(pitch % 12) as usize], pitch as i32 / 12 - 1)
}

/// Parses `"C4"`, `"F#3"`, `"Bb2"`, `"c-1"`. Returns `None` if the name is
/// malformed or outside the MIDI range 0..=127.
pub fn parse_pitch(name: &str) -> Option<u8> {
    let name = name.trim();
    let mut chars = name.chars();
    let base = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let accidentals = rest.chars().take_while(|c| matches!(c, '#' | 'b' | '♯' | '♭')).count();
    let mut offset = 0i32;
    for c in rest.chars().take(accidentals) {
        offset += if matches!(c, '#' | '♯') { 1 } else { -1 };
    }
    let octave_str: String = rest.chars().skip(accidentals).collect();
    let octave: i32 = octave_str.parse().ok()?;
    let midi = (octave + 1) * 12 + base + offset;
    (0..=127).contains(&midi).then_some(midi as u8)
}

/// Names for the General MIDI drum notes the built-in drum kit plays. The
/// first name listed for a pitch is its canonical one.
pub const DRUM_NAMES: &[(&str, u8)] = &[
    ("kick", 36),
    ("bd", 36),
    ("rim", 37),
    ("rimshot", 37),
    ("snare", 38),
    ("sd", 38),
    ("clap", 39),
    ("low_tom", 43),
    ("tom_low", 43),
    ("closed_hat", 42),
    ("hat", 42),
    ("hihat", 42),
    ("hh", 42),
    ("pedal_hat", 44),
    ("mid_tom", 47),
    ("tom_mid", 47),
    ("open_hat", 46),
    ("open_hihat", 46),
    ("ohh", 46),
    ("high_tom", 50),
    ("tom_high", 50),
    ("crash", 49),
    ("ride", 51),
    ("tambourine", 54),
    ("cowbell", 56),
    ("shaker", 70),
    ("claves", 75),
];

/// Resolves a drum name such as `"kick"` or `"Open Hat"` to its MIDI note.
pub fn parse_drum(name: &str) -> Option<u8> {
    let key: String =
        name.trim().chars().map(|c| if c == ' ' || c == '-' { '_' } else { c.to_ascii_lowercase() }).collect();
    DRUM_NAMES.iter().find(|(drum, _)| *drum == key).map(|(_, pitch)| *pitch)
}

/// The canonical drum name for a MIDI note, if it has one.
pub fn drum_name(pitch: u8) -> Option<&'static str> {
    DRUM_NAMES.iter().find(|(_, p)| *p == pitch).map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drum_names_resolve() {
        assert_eq!(parse_drum("kick"), Some(36));
        assert_eq!(parse_drum("Open Hat"), Some(46));
        assert_eq!(parse_drum("closed-hat"), Some(42));
        assert_eq!(parse_drum("gong"), None);
        assert_eq!(drum_name(38), Some("snare"));
        assert_eq!(drum_name(60), None);
    }

    #[test]
    fn names_round_trip() {
        assert_eq!(pitch_name(60), "C4");
        assert_eq!(pitch_name(0), "C-1");
        assert_eq!(pitch_name(127), "G9");
        for p in 0..=127u8 {
            assert_eq!(parse_pitch(&pitch_name(p)), Some(p));
        }
    }

    #[test]
    fn parses_flats_and_case() {
        assert_eq!(parse_pitch("Bb2"), Some(46));
        assert_eq!(parse_pitch("f#3"), Some(54));
        assert_eq!(parse_pitch("Cb4"), Some(59));
        assert_eq!(parse_pitch("A4"), Some(69));
        assert_eq!(parse_pitch("H2"), None);
        assert_eq!(parse_pitch("C"), None);
        assert_eq!(parse_pitch("G10"), None);
    }
}
