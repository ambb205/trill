//! Musical keys, and reading them from file names.

/// A musical key: a root note and whether it is minor.
///
/// Enharmonic spellings are one key: `Gb` and `F#` compare equal.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    /// Pitch class of the root, C = 0 up to B = 11.
    root: u8,
    minor: bool,
}

const MAJOR_NAMES: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
const MINOR_NAMES: [&str; 12] = ["Cm", "C#m", "Dm", "Ebm", "Em", "Fm", "F#m", "Gm", "G#m", "Am", "Bbm", "Bm"];

impl Key {
    /// The key with root `pitch_class` (C = 0; taken modulo 12).
    pub fn new(pitch_class: u8, minor: bool) -> Self {
        Self { root: pitch_class % 12, minor }
    }

    /// Pitch class of the root, C = 0 up to B = 11.
    pub fn root(self) -> u8 {
        self.root
    }

    pub fn is_minor(self) -> bool {
        self.minor
    }

    /// The usual spelling: `F#m`, `Bb`, `C`.
    pub fn name(self) -> &'static str {
        let names = if self.minor { &MINOR_NAMES } else { &MAJOR_NAMES };
        names[usize::from(self.root)]
    }

    /// Reads a key written on its own, as a user would type it into a
    /// filter: `Am`, `f#min`, `C major`, `Gb`, or a bare `C` for C major.
    pub fn parse(text: &str) -> Option<Key> {
        let mut words = text.split_whitespace();
        let note = Note::parse(words.next()?)?;
        let mode = match (note.mode, words.next()) {
            (None, Some(word)) => Some(mode_word(word)?),
            (mode, None) => mode,
            (Some(_), Some(_)) => return None,
        };
        words.next().is_none().then_some(Key::new(note.root, mode == Some(Mode::Minor)))
    }

    /// One number for the key, for compact storage: 0..12 major, 12..24 minor.
    pub(crate) fn code(self) -> u8 {
        self.root + if self.minor { 12 } else { 0 }
    }

    pub(crate) fn from_code(code: u8) -> Option<Key> {
        (code < 24).then(|| Key::new(code % 12, code >= 12))
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    Major,
    Minor,
}

/// `min`, `minor`, `maj` or `major`, in any letter case.
fn mode_word(word: &str) -> Option<Mode> {
    match word.to_ascii_lowercase().as_str() {
        "min" | "minor" => Some(Mode::Minor),
        "maj" | "major" => Some(Mode::Major),
        _ => None,
    }
}

/// A note name as it appears in one token of a file name.
struct Note {
    root: u8,
    /// A sharp or flat sign follows the letter.
    accidental: bool,
    /// The letter is lower case.
    lower: bool,
    /// The mode written onto the note, if any.
    mode: Option<Mode>,
    /// The mode is the single letter `m` or `M` rather than a word.
    short_mode: bool,
    /// The mode letter is a capital `M`.
    capital_mode: bool,
}

impl Note {
    /// Parses a whole token: a letter A to G, an optional accidental and an
    /// optional mode. Anything else after the note (`Cmaj7`, `C#4`, `Am7`)
    /// makes it a chord or a pitch, not a key.
    fn parse(token: &str) -> Option<Note> {
        let mut chars = token.chars();
        let letter = chars.next()?;
        let semitones: i8 = match letter.to_ascii_uppercase() {
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
        let (shift, rest) = match rest.chars().next() {
            Some(sign @ ('#' | '♯')) => (1, &rest[sign.len_utf8()..]),
            Some(sign @ ('b' | '♭')) => (-1, &rest[sign.len_utf8()..]),
            _ => (0, rest),
        };
        let (mode, short_mode) = match rest {
            "" => (None, false),
            "m" | "M" => (Some(Mode::Minor), true),
            word => (Some(mode_word(word)?), false),
        };
        Some(Note {
            root: (semitones + shift).rem_euclid(12) as u8,
            accidental: shift != 0,
            lower: letter.is_ascii_lowercase(),
            mode,
            short_mode,
            capital_mode: rest == "M",
        })
    }
}

/// Characters that separate the parts of a file name. `#` is not one of
/// them: it belongs to the note in front of it.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || "_-.,()[]{}+&".contains(c)
}

/// Whether `token` is a tempo: a number in the range tempos come in, with
/// or without `bpm` attached. Keys are written beside tempos, so a tempo
/// next to an ambiguous token settles what it is.
fn is_tempo(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let digits = lower.strip_suffix("bpm").unwrap_or(&lower);
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && digits.parse::<u32>().is_ok_and(|bpm| (40..=300).contains(&bpm))
}

/// The key a file name states, if it states one.
///
/// Read strictly, because a wrong key misleads while a missing one only
/// fails to help:
///
/// - A note with its mode is a key: `F#min`, `Ebmaj`, `C#m`, `A minor`,
///   `Bb_major`.
/// - A note with an accidental and no mode is that major key: `Gb`, `F#`.
/// - A bare letter is a key only right after a tempo (`128_F`) or after the
///   word `key`; anywhere else it is far more often a take or a variation
///   (`Loop_A`, `Stereoid E 128bpm`).
/// - `Am` and `Fm` in capitals are words (`AM`, `FM`), and names written
///   wholly in capitals spell minor with a capital `M` (`C#M`, `DM`). Both
///   count only beside a tempo, or with an accidental.
/// - Lower-case notes need more than one letter of evidence: `f#m` and
///   `amin` are keys, `am` and `a` are words.
/// - Chords and pitches are not keys: `Cmaj7`, `Am7`, `C#4`.
///
/// When a name holds several, the last one wins: keys are written at the
/// end.
pub(crate) fn key_from_name(stem: &str) -> Option<Key> {
    let all_capitals = !stem.chars().any(char::is_lowercase);
    let tokens: Vec<&str> = stem.split(is_separator).filter(|token| !token.is_empty()).collect();
    let after_tempo = |index: usize| index > 0 && is_tempo(tokens[index - 1]);
    let beside_tempo = |index: usize| after_tempo(index) || tokens.get(index + 1).is_some_and(|next| is_tempo(next));

    (0..tokens.len()).rev().find_map(|index| {
        let note = Note::parse(tokens[index])?;
        let following = tokens.get(index + 1).and_then(|next| mode_word(next));
        let mode = note.mode.or(following);
        let after_key = index > 0 && tokens[index - 1].eq_ignore_ascii_case("key");

        let accepted = match mode {
            // A capital M means minor only where everything is capitals;
            // elsewhere it is as likely to mean major, so it is no evidence.
            Some(_) if note.capital_mode && !all_capitals => false,
            // "AM", "FM", "DM": words and abbreviations as often as keys.
            Some(_) if note.capital_mode && !note.accidental => beside_tempo(index),
            // "am", "em": words. "f#m" and "amin" are not.
            Some(_) if note.lower && note.short_mode && !note.accidental => false,
            Some(_) => true,
            None if note.lower => false,
            None => note.accidental || after_key || after_tempo(index),
        };
        accepted.then(|| Key::new(note.root, mode == Some(Mode::Minor)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Option<&'static str> {
        key_from_name(name).map(Key::name)
    }

    #[test]
    fn keys_in_realistic_file_names() {
        let cases = [
            // Splice-style names.
            ("BISHU_synth_loop_coolin_100_Fm", Some("Fm")),
            ("BISHU_vocal_one_shot_huh_119_Am", Some("Am")),
            ("KITTY_vocal_boom_boom_boom_fx_140_Emaj", Some("E")),
            ("KRYSTA_YOUNGS_vocal_fx_loop_screwed_prayer_166_Cmin", Some("Cm")),
            ("KRYSTA_YOUNGS_vocal_syllable_phrase_lalalala_stacked_wet_122_Ebmaj", Some("Eb")),
            ("VOX_UGV_155_vocal_hook_gang_yeah_falsetto_dry_F#min", Some("F#m")),
            ("VOX_UGV_160_vocal_hook_gang_hey_power_post_lead_dry_A#min", Some("Bbm")),
            ("VOX_UGV_130_vocal_hook_gang_ohh_soul_lead_wet_G#min", Some("G#m")),
            ("KRYSTA_YOUNGS_vocal_syllable_one_shot_no_dry_128_F", Some("F")),
            ("NOIZU_riser_one_shot_09_124_F", Some("F")),
            ("KRYSTA_YOUNGS_vocal_syllable_one_shot_adlib_mania_dry_Amin", Some("Am")),
            ("KITTY_vocal_ooh_chord_Cm", Some("Cm")),
            // Hand-named packs.
            ("MELODY 7 - 140bpm - Dm", Some("Dm")),
            ("Melody Loop 140bpm C#m", Some("C#m")),
            ("Pad Ebmaj", Some("Eb")),
            ("Lead F Minor", Some("Fm")),
            ("Chords_A_min_120", Some("Am")),
            ("Bass Key C", Some("C")),
            ("Pluck Bb", Some("Bb")),
            ("Bbm Loop", Some("Bbm")),
            ("Dark Pad Am", Some("Am")),
            ("Air Space Pad C Major 114 bpm", Some("C")),
            ("147 F# major tiff vibes", Some("F#")),
            ("dark pad f#m 128", Some("F#m")),
            ("it just has a vibe idk 140 c# major", Some("Db")),
            ("Bowed Semi Hollow Baritone Guitar G#", Some("Ab")),
            ("Keys Loop 90 Gb", Some("F#")),
            // Packs named wholly in capitals write minor as M.
            ("BASSPOP MK1 - BEAUTY SINE W NOISE - 95 BPM F#M", Some("F#m")),
            ("BASSPOP MK1 - BASS 140 C#M", Some("C#m")),
            ("BASSPOP MK1 - FUZEY BASS 145 DM", Some("Dm")),
            ("BASSPOP MK1 - SAW BUZZ 140 FM", Some("Fm")),
            ("BASSPOP MK1 - BISH SINE BAZZ D# 132", Some("Eb")),
            // Not keys.
            ("A Kick", None),
            ("Drum Loop C", None),
            ("Loop_A", None),
            ("Snare_B", None),
            ("NOIZU_bass_one_shot_moog_07_F", None),
            ("Stab Cmaj7", None),
            ("Guitar Chord_C9 Jazz", None),
            ("Pad Am7", None),
            ("Zither Hit D#1 v1 rr2", None),
            ("KITTY_music_box_notes_B4", None),
            ("Bansuri Sustained A3 02", None),
            ("Forest Fear Train High F#0", None),
            ("Bass Clap Drum", None),
            ("Pad CM", None),
            ("FM BASS", None),
            ("WAKE UP AM", None),
            ("this is a test", None),
            ("i am here 140", None),
            ("Em drums", Some("Em")),
            ("em drums", None),
            ("Grv Kick 19", None),
            ("Mousehouse Stereoid E 128bpm", None),
            ("", None),
            ("###", None),
        ];
        for (name, expected) in cases {
            assert_eq!(key(name), expected, "{name}");
        }
    }

    #[test]
    fn typed_keys_parse() {
        let parsed = |text: &str| Key::parse(text).map(Key::name);
        assert_eq!(parsed("Am"), Some("Am"));
        assert_eq!(parsed("f#min"), Some("F#m"));
        assert_eq!(parsed("C major"), Some("C"));
        assert_eq!(parsed("  gb "), Some("F#"));
        assert_eq!(parsed("c"), Some("C"));
        assert_eq!(parsed("B♭ minor"), Some("Bbm"));
        assert_eq!(parsed("H"), None);
        assert_eq!(parsed("Am minor"), None);
        assert_eq!(parsed("C major scale"), None);
        assert_eq!(parsed(""), None);
    }

    #[test]
    fn keys_compare_by_pitch_and_store_compactly() {
        assert_eq!(Key::parse("Gb"), Key::parse("F#"));
        assert_ne!(Key::parse("A"), Key::parse("Am"));
        assert_eq!(Key::new(14, true), Key::new(2, true));
        for code in 0..24 {
            let key = Key::from_code(code).unwrap();
            assert_eq!(key.code(), code);
            assert_eq!(Key::parse(key.name()), Some(key), "{key}");
        }
        assert_eq!(Key::from_code(24), None);
        assert_eq!((Key::new(9, true).root(), Key::new(9, true).is_minor()), (9, true));
        assert_eq!(Key::new(10, false).to_string(), "Bb");
    }
}
