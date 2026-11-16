//! Chord symbols: reading the ones musicians write, naming chords back, and
//! recognising a chord from the notes that are sounding.

use crate::error::{Error, Result};
use crate::pitch::{Interval, NoteName, pitch_class};
use crate::scale::Scale;
use std::fmt;

/// The tones of a chord above its root, without the root itself being
/// fixed: what `m7b5` or `maj9` means on any note.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Quality {
    /// Sorted by degree; always starts with the unison.
    tones: Vec<Interval>,
}

const NINTH: u8 = 9;
const ELEVENTH: u8 = 11;
const THIRTEENTH: u8 = 13;

impl Quality {
    /// Parses the part of a chord symbol after the root: `""`, `"m7"`,
    /// `"maj7#11"`, `"7sus4"`, `"6/9"`.
    pub fn parse(text: &str) -> Result<Self> {
        Self::parse_as(text, false, text)
    }

    /// `minor` starts from a minor triad, which is how a lowercase Roman
    /// numeral reads. `whole` is the complete symbol, quoted in errors.
    pub(crate) fn parse_as(text: &str, minor: bool, whole: &str) -> Result<Self> {
        let mut parser = Parser::new(text, whole);
        if minor {
            parser.third = Some(-1);
        }
        parser.run(minor)?;
        Ok(parser.finish())
    }

    /// The chord's tones as intervals above the root, lowest degree first.
    pub fn tones(&self) -> &[Interval] {
        &self.tones
    }

    pub fn has(&self, interval: Interval) -> bool {
        self.tones.contains(&interval)
    }

    /// How the tone on a degree is altered, if the chord has that degree.
    pub fn alter_of(&self, number: u8) -> Option<i8> {
        self.tones.iter().find(|tone| tone.number == number).map(|tone| tone.alter)
    }

    /// The chord's third, or the second or fourth that replaces it in a
    /// suspended chord.
    pub fn third_or_sus(&self) -> Option<Interval> {
        [3, 4, 2].into_iter().find_map(|number| self.tones.iter().copied().find(|tone| tone.number == number))
    }

    /// The canonical spelling of the quality, such as `"m7b5"`. Parsing it
    /// again gives the same tones.
    pub fn symbol(&self) -> String {
        let alters = |number: u8| -> Vec<i8> {
            self.tones.iter().filter(|tone| tone.number == number).map(|tone| tone.alter).collect()
        };
        let third = self.alter_of(3);
        let fifth = self.alter_of(5);
        let seventh = self.alter_of(7);
        let sixth = self.alter_of(6).is_some();
        let (sus2, sus4) = (self.alter_of(2).is_some(), self.alter_of(4).is_some());
        let (ninths, elevenths, thirteenths) = (alters(NINTH), alters(ELEVENTH), alters(THIRTEENTH));
        let natural = |alters: &[i8]| alters.contains(&0);
        let tensions = ninths.len() + elevenths.len() + thirteenths.len();

        // The altered dominant has a name of its own.
        if third == Some(0)
            && seventh == Some(-1)
            && fifth.is_none()
            && ninths == [-1, 1]
            && elevenths == [1]
            && thirteenths == [-1]
        {
            return "7alt".to_string();
        }

        let mut out = String::new();
        // Tones already said by the part of the name written so far.
        let (mut fifth_named, mut ninth_named, mut eleventh_named, mut thirteenth_named) = (false, false, false, false);
        let diminished_triad = third == Some(-1) && fifth == Some(-1);

        if diminished_triad && seventh == Some(-2) {
            out.push_str("dim7");
            fifth_named = true;
        } else if diminished_triad && seventh.is_none() && !sixth {
            out.push_str("dim");
            fifth_named = true;
        } else if third == Some(0) && fifth == Some(1) && seventh.is_none() && !sixth && tensions == 0 {
            out.push_str("aug");
            fifth_named = true;
        } else {
            if third == Some(-1) {
                out.push('m');
            }
            if let Some(seventh) = seventh {
                // A 13 says there is a ninth under it and an 11 likewise; an
                // altered ninth still fills that place ("13b9").
                let top = if natural(&thirteenths) && !ninths.is_empty() && (third != Some(-1) || natural(&elevenths)) {
                    13
                } else if natural(&elevenths) && !ninths.is_empty() {
                    11
                } else if natural(&ninths) {
                    9
                } else {
                    7
                };
                match (seventh, third) {
                    (0, Some(-1)) => out.push_str("Maj"),
                    (0, _) => out.push_str("maj"),
                    _ => {}
                }
                if seventh == -2 {
                    out.push_str("dim7");
                } else {
                    out.push_str(&top.to_string());
                    ninth_named = top >= 9;
                    eleventh_named = top == 11 || (top == 13 && third == Some(-1));
                    thirteenth_named = top == 13;
                }
            } else if sixth {
                out.push('6');
                if natural(&ninths) {
                    out.push_str("/9");
                    ninth_named = true;
                }
            }
            if third.is_none() {
                match (sus2, sus4) {
                    (true, true) => out.push_str("sus24"),
                    (true, false) => out.push_str("sus2"),
                    (false, true) => out.push_str("sus4"),
                    (false, false) if out.is_empty() && fifth == Some(0) && tensions == 0 => {
                        out.push('5');
                        fifth_named = true;
                    }
                    (false, false) => {}
                }
            }
        }

        if !fifth_named {
            match fifth {
                Some(-1) => out.push_str("b5"),
                Some(1) => out.push_str("#5"),
                _ => {}
            }
        }
        if seventh.is_some() && sixth {
            out.push_str("add6");
        }
        for (number, alters, named) in [
            (NINTH, &ninths, ninth_named),
            (ELEVENTH, &elevenths, eleventh_named),
            (THIRTEENTH, &thirteenths, thirteenth_named),
        ] {
            for alter in alters {
                match alter {
                    0 if named => {}
                    0 => out.push_str(&format!("add{number}")),
                    _ => out.push_str(&Interval::new(number, *alter).label()),
                }
            }
        }
        if third.is_none() && !sus2 && !sus4 && !out.starts_with('5') {
            out.push_str("no3");
        }
        if fifth.is_none() && !fifth_named {
            out.push_str("no5");
        }
        out
    }
}

/// Reads a quality left to right: the family (minor, diminished...), the
/// extension number, then any number of alterations and additions.
struct Parser<'a> {
    whole: &'a str,
    rest: &'a str,
    third: Option<i8>,
    fifth: Option<i8>,
    seventh: Option<i8>,
    sixth: bool,
    sus: Vec<u8>,
    tensions: Vec<Interval>,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str, whole: &'a str) -> Self {
        Self {
            whole,
            rest: text,
            third: Some(0),
            fifth: Some(0),
            seventh: None,
            sixth: false,
            sus: Vec::new(),
            tensions: Vec::new(),
        }
    }

    fn fail<T>(&self, problem: impl fmt::Display) -> Result<T> {
        Err(Error::new(format!("'{}': {problem}", self.whole)))
    }

    /// Brackets, commas and spaces only group a symbol visually: `C7(b9,#11)`.
    fn skip_separators(&mut self) {
        self.rest = self.rest.trim_start_matches(['(', ')', ',', ' ']);
    }

    fn eat(&mut self, token: &str) -> bool {
        match self.rest.strip_prefix(token) {
            Some(rest) => {
                self.rest = rest;
                true
            }
            None => false,
        }
    }

    fn eat_any(&mut self, tokens: &[&str]) -> bool {
        tokens.iter().any(|token| self.eat(token))
    }

    fn peek_digit(&self) -> bool {
        self.rest.starts_with(|c: char| c.is_ascii_digit())
    }

    fn number(&mut self) -> Option<u32> {
        let digits = self.rest.chars().take_while(char::is_ascii_digit).count();
        let (number, rest) = self.rest.split_at(digits);
        let value = number.parse().ok()?;
        self.rest = rest;
        Some(value)
    }

    /// "maj", "M", a triangle: the seventh, if there is one, is major.
    fn major_marker(&mut self) -> Option<MajorMarker> {
        if self.eat_any(&["Δ", "^"]) {
            return Some(MajorMarker::Triangle);
        }
        if self.eat_any(&["maj", "Maj", "MAJ", "M"]) {
            return Some(MajorMarker::Word);
        }
        // "ma7" is a major seventh, but a bare "ma" is not a word.
        if self.rest.starts_with("ma") && self.rest[2..].starts_with(|c: char| c.is_ascii_digit()) {
            self.rest = &self.rest[2..];
            return Some(MajorMarker::Word);
        }
        None
    }

    fn set_tension(&mut self, number: u8, alter: i8) {
        // An altered tension replaces the natural one the extension implied
        // (the 9 in C13b9), and the other way round.
        if alter == 0 {
            self.tensions.retain(|tone| tone.number != number);
        } else {
            self.tensions.retain(|tone| *tone != Interval::new(number, 0));
        }
        let tone = Interval::new(number, alter);
        if !self.tensions.contains(&tone) {
            self.tensions.push(tone);
        }
    }

    fn run(&mut self, minor: bool) -> Result<()> {
        self.skip_separators();
        let mut major = self.major_marker();
        let mut diminished = false;
        if major.is_none() {
            if self.eat_any(&["min", "mi", "m", "-"]) {
                self.third = Some(-1);
                self.skip_separators();
                major = self.major_marker();
            } else if self.eat_any(&["dim", "°"]) || self.eat_circle() {
                (self.third, self.fifth, diminished) = (Some(-1), Some(-1), true);
            } else if self.eat_any(&["ø", "Ø"]) {
                (self.third, self.fifth, self.seventh) = (Some(-1), Some(-1), Some(-1));
                if self.peek_digit() && !self.eat("7") {
                    return self.fail("a half-diminished chord (ø) is a seventh chord; expected ø or ø7");
                }
            } else if self.eat_any(&["aug", "+"]) {
                self.fifth = Some(1);
                major = self.major_marker();
            }
        }

        self.skip_separators();
        let minor_family = self.third == Some(-1);
        match self.number() {
            None => match major {
                // A triangle alone means a major seventh; "maj" alone is just a major triad.
                Some(MajorMarker::Triangle) => self.seventh = Some(0),
                Some(MajorMarker::Word) if minor_family => {
                    return self.fail("a minor chord with 'maj' needs its seventh; expected mMaj7 or m(maj7)");
                }
                _ => {}
            },
            Some(5) if major.is_none() && !minor && !minor_family && self.fifth == Some(0) && self.rest.is_empty() => {
                self.third = None;
            }
            Some(number @ (6 | 69)) if !diminished => {
                self.sixth = true;
                self.skip_separators();
                if number == 69 || self.eat_any(&["/9", "9"]) {
                    self.set_tension(NINTH, 0);
                }
            }
            Some(7) if diminished => self.seventh = Some(-2),
            Some(number @ (7 | 9 | 11 | 13)) if !diminished => {
                if self.seventh.is_none() {
                    self.seventh = Some(if major.is_some() { 0 } else { -1 });
                }
                if number >= 9 {
                    self.set_tension(NINTH, 0);
                }
                // A major third and a natural eleventh clash, so a 13 chord
                // leaves the eleventh out; a minor 13 keeps it.
                if number == 11 || (number == 13 && minor_family) {
                    self.set_tension(ELEVENTH, 0);
                }
                if number == 13 {
                    self.set_tension(THIRTEENTH, 0);
                }
            }
            Some(number @ (2 | 4)) => {
                let (sus, add) = if number == 2 { ("sus2", "add9") } else { ("sus4", "add11") };
                return self.fail(format!(
                    "'{number}' alone is ambiguous; write {sus} (replacing the third) or {add} (keeping it)"
                ));
            }
            Some(number) if diminished => {
                return self.fail(format!("a diminished chord takes no '{number}'; expected dim or dim7"));
            }
            Some(number) => {
                return self.fail(format!("'{number}' is not a chord extension; expected 6, 7, 9, 11 or 13"));
            }
        }

        loop {
            self.skip_separators();
            if self.rest.is_empty() {
                return Ok(());
            }
            if self.eat("sus") {
                let kinds: &[u8] = match self.number() {
                    None | Some(4) => &[4],
                    Some(2) => &[2],
                    Some(24) => &[2, 4],
                    Some(other) => {
                        return self.fail(format!("'sus{other}' is not a suspension; expected sus2 or sus4"));
                    }
                };
                self.third = None;
                for kind in kinds {
                    if !self.sus.contains(kind) {
                        self.sus.push(*kind);
                    }
                }
            } else if self.eat("add") {
                let alter = self.accidental();
                match (self.number(), alter) {
                    (Some(2 | 9), alter) => self.set_tension(NINTH, alter),
                    (Some(4 | 11), alter @ (0 | 1)) => self.set_tension(ELEVENTH, alter),
                    (Some(13), alter @ (0 | -1)) => self.set_tension(THIRTEENTH, alter),
                    (Some(6), 0) => self.sixth = true,
                    _ => return self.fail("'add' needs a tone to add; expected add9, add11, add13 or add6"),
                }
            } else if self.eat_any(&["no", "omit"]) {
                match self.number() {
                    Some(3) => self.third = None,
                    Some(5) => self.fifth = None,
                    _ => return self.fail("only the third or fifth can be left out; expected no3 or no5"),
                }
            } else if self.eat("alt") {
                if self.third != Some(0) || self.seventh == Some(0) {
                    return self.fail("'alt' belongs on a dominant seventh chord; expected a symbol like G7alt");
                }
                (self.seventh, self.fifth) = (Some(-1), None);
                self.tensions =
                    vec![Interval::new(9, -1), Interval::new(9, 1), Interval::new(11, 1), Interval::new(13, -1)];
            } else if self.rest.starts_with(['b', '#', '♭', '♯', '+', '-']) {
                self.alteration()?;
            } else {
                return self.fail(format!(
                    "'{}' is not something a chord symbol can end with; expected an alteration (b5, #5, b9, #9, #11, \
                     b13), sus2, sus4, add9, no3, no5 or a slash bass like /E",
                    self.rest
                ));
            }
        }
    }

    /// A lowercase "o" is a diminished sign only where it cannot be the
    /// start of a word: at the end or before the 7.
    fn eat_circle(&mut self) -> bool {
        let found = self.rest == "o" || self.rest.starts_with("o7");
        if found {
            self.rest = &self.rest[1..];
        }
        found
    }

    fn accidental(&mut self) -> i8 {
        let mut alter = 0;
        loop {
            if self.eat_any(&["b", "♭"]) {
                alter -= 1;
            } else if self.eat_any(&["#", "♯"]) {
                alter += 1;
            } else {
                return alter;
            }
        }
    }

    /// `b5`, `#9`, `b13`, and the older `+5`, `-9` and trailing `+`.
    fn alteration(&mut self) -> Result<()> {
        let written = self.rest;
        let alter = match self.accidental() {
            0 if self.eat("+") => 1,
            0 if self.eat("-") => -1,
            alter => alter,
        };
        let number = self.number();
        match (number, alter) {
            // "C7+": an augmented fifth.
            (None, 1) if written.starts_with('+') && self.rest.is_empty() => self.alter_fifth(1),
            (Some(5), -1 | 1) => self.alter_fifth(alter),
            (Some(9), -1 | 1) => self.set_tension(NINTH, alter),
            (Some(11), 1) => self.set_tension(ELEVENTH, 1),
            (Some(13), -1) => self.set_tension(THIRTEENTH, -1),
            (None, _) => {
                return self.fail(format!(
                    "'{written}' has an accidental with no tone after it; expected b5, #5, b9, #9, #11 or b13"
                ));
            }
            (Some(_), _) => {
                let unread = &written[..written.len() - self.rest.len()];
                return self.fail(format!("'{unread}' is not an alteration; expected b5, #5, b9, #9, #11 or b13"));
            }
        }
        Ok(())
    }

    /// A chord with both altered fifths (`7b5#5`) keeps the flat one as its
    /// fifth and the sharp one, the same note, as a b13, whichever came first.
    fn alter_fifth(&mut self, alter: i8) {
        match self.fifth {
            Some(existing) if existing != 0 && existing != alter => {
                self.fifth = Some(-1);
                self.set_tension(THIRTEENTH, -1);
            }
            _ => self.fifth = Some(alter),
        }
    }

    fn finish(mut self) -> Quality {
        let mut tones = vec![Interval::UNISON];
        if self.sus.is_empty() {
            tones.extend(self.third.map(|alter| Interval::new(3, alter)));
        }
        self.sus.sort_unstable();
        tones.extend(self.sus.iter().map(|number| Interval::new(*number, 0)));
        tones.extend(self.fifth.map(|alter| Interval::new(5, alter)));
        if self.sixth {
            tones.push(Interval::MAJOR_SIXTH);
        }
        tones.extend(self.seventh.map(|alter| Interval::new(7, alter)));
        self.tensions.sort();
        tones.append(&mut self.tensions);
        // Two names for one key (sus4 with an added eleventh) keep the first.
        let mut seen = Vec::new();
        tones.retain(|tone| {
            let fresh = !seen.contains(&tone.pitch_class());
            seen.push(tone.pitch_class());
            fresh
        });
        tones.sort();
        Quality { tones }
    }
}

#[derive(Copy, Clone, PartialEq)]
enum MajorMarker {
    /// "maj" or "M": major, and a major seventh if a number follows.
    Word,
    /// Δ or ^: a major seventh chord even with no number.
    Triangle,
}

/// A chord: a root, a quality, and optionally a different note in the bass.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub root: NoteName,
    pub quality: Quality,
    /// The bass note of a slash chord (`C/E`, `Dm7/G`). It need not be a
    /// chord tone.
    pub bass: Option<NoteName>,
}

impl Chord {
    pub fn new(root: NoteName, quality: Quality) -> Self {
        Self { root, quality, bass: None }
    }

    /// Parses a chord symbol such as `Am9`, `Fmaj7/A`, `C#m7b5`, `G7sus4`,
    /// `Bb6/9`, `E7#9b13` or `Cdim7`.
    pub fn parse(text: &str) -> Result<Self> {
        let symbol = text.trim();
        let Some((root, rest)) = NoteName::split_prefix(symbol) else {
            let hint = match symbol.chars().next() {
                Some(c) if c.is_ascii_lowercase() && Self::parse(&uppercase_first(symbol)).is_ok() => {
                    format!("; chord roots are capital letters, did you mean '{}'?", uppercase_first(symbol))
                }
                _ => String::new(),
            };
            return Err(Error::new(format!(
                "'{text}' is not a chord symbol; expected a root A-G followed by a quality, like Am7 or F#maj7{hint}"
            )));
        };
        // The last slash starts a bass note unless it belongs to "6/9".
        let (quality, bass) = match rest.rfind('/') {
            Some(slash) if !rest[slash..].starts_with("/9") => {
                let bass = NoteName::parse(&rest[slash + 1..]).map_err(|_| {
                    Error::new(format!(
                        "'{text}': '{}' after the slash is not a bass note; expected a note name, as in C/E",
                        &rest[slash + 1..]
                    ))
                })?;
                (&rest[..slash], Some(bass))
            }
            _ => (rest, None),
        };
        let quality = Quality::parse_as(quality, false, symbol)?;
        Ok(Self { root, quality, bass: bass.filter(|bass| *bass != root) })
    }

    /// The canonical symbol, e.g. `"C#m7b5"` or `"Fmaj7/A"`.
    pub fn name(&self) -> String {
        match self.bass {
            Some(bass) => format!("{}{}/{bass}", self.root, self.quality.symbol()),
            None => format!("{}{}", self.root, self.quality.symbol()),
        }
    }

    /// The chord tones, spelled, in the order of [`Quality::tones`].
    pub fn tone_names(&self) -> Vec<NoteName> {
        self.quality.tones().iter().map(|tone| self.root.up(*tone)).collect()
    }

    /// The pitch classes of the chord tones, root first. A slash bass that
    /// is not a chord tone is not included.
    pub fn pitch_classes(&self) -> Vec<u8> {
        self.tone_names().into_iter().map(NoteName::pitch_class).collect()
    }

    /// The note in the bass: the slash bass if there is one, else the root.
    pub fn bass_name(&self) -> NoteName {
        self.bass.unwrap_or(self.root)
    }

    /// The name of a pitch class as this chord spells it; pitch classes
    /// outside the chord get their usual name.
    pub fn spell_pitch_class(&self, pitch_class: u8) -> NoteName {
        self.tone_names()
            .into_iter()
            .chain(self.bass)
            .find(|name| name.pitch_class() == pitch_class % 12)
            .unwrap_or_else(|| NoteName::of_pitch_class(pitch_class))
    }

    /// The name of a pitch as this chord spells it, with its octave.
    pub fn spell(&self, pitch: u8) -> String {
        self.spell_pitch_class(pitch_class(pitch)).spell(pitch)
    }

    /// The same chord with its root named as `key` names it: G#m in E
    /// major, Abm in Eb major. A slash bass that is a chord tone follows the
    /// chord (D7/F#, even in F major); any other bass follows the key.
    pub fn spelled_in(&self, key: &Scale) -> Self {
        let mut chord =
            Self { root: key.spell_pitch_class(self.root.pitch_class()), quality: self.quality.clone(), bass: None };
        chord.bass = self.bass.map(|bass| {
            let class = bass.pitch_class();
            let as_tone = chord.tone_names().into_iter().find(|name| name.pitch_class() == class);
            as_tone.unwrap_or_else(|| key.spell_pitch_class(class))
        });
        chord
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

fn uppercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
}

/// Whether `text` marks a stretch with no chord: `N.C.`, `NC`, `n.c.`.
pub fn is_no_chord(text: &str) -> bool {
    let compact: String = text.trim().chars().filter(|c| *c != '.').collect();
    compact.eq_ignore_ascii_case("nc")
}

/// The chord qualities [`identify`] can name, most common first: a list of
/// the everyday ones, for reference listings too. When two
/// readings fit equally well the earlier one wins, so a plain triad is
/// preferred to an exotic chord on another root.
pub const QUALITIES: &[&str] = &[
    "", "m", "7", "maj7", "m7", "sus4", "sus2", "5", "6", "m6", "dim", "aug", "dim7", "m7b5", "mMaj7", "add9", "madd9",
    "9", "maj9", "m9", "6/9", "m6/9", "7sus4", "9sus4", "11", "m11", "13", "maj13", "m13", "7b9", "7#9", "7#11",
    "7b13", "7b5", "7#5", "maj7#11", "maj7#5", "maj9#11", "9#11", "13#11", "13b9", "7#9b13", "7b9b13", "7b9#11",
    "13sus4", "mMaj9", "m9b5", "add11", "madd11", "7alt",
];

/// Names the chord a set of pitches makes, reading the lowest pitch as the
/// bass. Root position is preferred, so C E G A is C6 but A C E G is Am7; an
/// inversion comes back as a slash chord (E G C is C/E). The tones players
/// routinely leave out may be missing: the perfect fifth, and the ninth
/// under an eleventh or thirteenth. Returns `None` for fewer than two pitch
/// classes or for a cluster that is not a recognisable chord.
pub fn identify(pitches: &[u8]) -> Option<Chord> {
    let bass = pitch_class(*pitches.iter().min()?);
    let mut weights = [0.0; 12];
    for pitch in pitches {
        weights[pitch_class(*pitch) as usize] = 1.0;
    }
    identify_weighted(&weights, Some(bass))
}

/// Names the chord in a passage, given how much each pitch class is heard
/// (index 0 is C; any unit, such as beats sounding) and, if known, the
/// pitch class in the bass.
///
/// Every chord on every sounding root is scored: the weight of its tones,
/// less the weight of the notes it does not explain, less a charge for each
/// tone it claims that is not there and for each tone beyond a triad. So a
/// passing note in a melody does not turn C into Cadd11, while a ninth held
/// with the chord does make it a ninth chord. `None` when no chord accounts
/// for at least four fifths of what sounds.
pub fn identify_weighted(weights: &[f64; 12], bass: Option<u8>) -> Option<Chord> {
    let total: f64 = weights.iter().sum();
    if weights.iter().filter(|weight| **weight > 0.0).count() < 2 {
        return None;
    }
    let bass = bass.map(|bass| bass % 12).filter(|bass| weights[*bass as usize] > 0.0);
    let templates: Vec<Quality> =
        QUALITIES.iter().map(|symbol| Quality::parse(symbol).expect("built-in qualities parse")).collect();

    let mut best: Option<(f64, Chord)> = None;
    for root in (0..12u8).filter(|root| weights[*root as usize] > 0.0) {
        for (rank, quality) in templates.iter().enumerate() {
            // The tones a player may leave out without changing the chord's name.
            let ninth_under_extension = quality.has(Interval::new(NINTH, 0))
                && (quality.has(Interval::new(ELEVENTH, 0)) || quality.has(Interval::new(THIRTEENTH, 0)));
            let optional = |tone: &Interval| {
                *tone == Interval::PERFECT_FIFTH || (ninth_under_extension && *tone == Interval::new(NINTH, 0))
            };
            let (mut inside, mut present, mut missing) = (0.0, 0, 0);
            for tone in quality.tones() {
                let weight = weights[((root + tone.pitch_class()) % 12) as usize];
                inside += weight;
                if weight > 0.0 {
                    present += 1;
                } else if !optional(tone) {
                    missing += 1;
                }
            }
            if present < 2 || inside < 0.8 * total {
                continue;
            }
            let extra_tones = quality.tones().len().saturating_sub(3) as f64;
            let score =
                inside - (total - inside) - total * (0.2 * missing as f64 + 0.12 * extra_tones + 0.002 * rank as f64)
                    + if Some(root) == bass { 0.1 * total } else { 0.0 };
            if best.as_ref().is_none_or(|(top, _)| score > *top) {
                let mut chord = Chord::new(NoteName::of_pitch_class(root), quality.clone());
                if let Some(bass) = bass.filter(|bass| *bass != root) {
                    chord.bass = Some(chord.spell_pitch_class(bass));
                }
                best = Some((score, chord));
            }
        }
    }
    best.map(|(_, chord)| chord)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        Chord::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn spelled(text: &str) -> String {
        chord(text).tone_names().iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
    }

    fn mask(pitch_classes: impl IntoIterator<Item = u8>) -> u16 {
        pitch_classes.into_iter().fold(0, |mask, pc| mask | 1 << (pc % 12))
    }

    #[test]
    fn symbols_spell_the_chords_musicians_mean() {
        let cases = [
            ("C", "C E G"),
            ("Cm", "C Eb G"),
            ("Cdim", "C Eb Gb"),
            ("F#aug", "F# A# C##"),
            ("C+", "C E G#"),
            ("C5", "C G"),
            ("Csus2", "C D G"),
            ("Csus4", "C F G"),
            ("Csus", "C F G"),
            ("C6", "C E G A"),
            ("Cm6", "C Eb G A"),
            ("C7", "C E G Bb"),
            ("Cmaj7", "C E G B"),
            ("Cm7", "C Eb G Bb"),
            ("CmMaj7", "C Eb G B"),
            ("Cdim7", "C Eb Gb Bbb"),
            ("C#m7b5", "C# E G B"),
            ("Cø7", "C Eb Gb Bb"),
            ("Am9", "A C E G B"),
            ("Fmaj7/A", "F A C E"),
            ("G7sus4", "G C D F"),
            ("Bb6/9", "Bb D F G C"),
            ("Dadd9", "D F# A E"),
            ("E7#9b13", "E G# B D F## C"),
            ("C9", "C E G Bb D"),
            ("Cmaj9", "C E G B D"),
            ("C11", "C E G Bb D F"),
            ("Cm11", "C Eb G Bb D F"),
            ("C13", "C E G Bb D A"),
            ("Cm13", "C Eb G Bb D F A"),
            ("Cmaj13", "C E G B D A"),
            ("C7b9", "C E G Bb Db"),
            ("C7#11", "C E G Bb F#"),
            ("C13b9", "C E G Bb Db A"),
            ("Cmaj7#11", "C E G B F#"),
            ("C7b5", "C E Gb Bb"),
            ("C7#5", "C E G# Bb"),
            ("C+7", "C E G# Bb"),
            ("Caug7", "C E G# Bb"),
            ("C9sus4", "C F G Bb D"),
            ("C7alt", "C E Bb Db D# F# Ab"),
            ("Cm7b5", "C Eb Gb Bb"),
            ("Ebm7", "Eb Gb Bb Db"),
            ("F#m7", "F# A C# E"),
            ("Bbmaj7", "Bb D F A"),
            ("Abmaj7", "Ab C Eb G"),
            ("Db7", "Db F Ab Cb"),
            ("Cmadd9", "C Eb G D"),
            ("C7no5", "C E Bb"),
            ("C7b9#9", "C E G Bb Db D#"),
        ];
        for (symbol, expected) in cases {
            assert_eq!(spelled(symbol), expected, "{symbol}");
        }
    }

    #[test]
    fn the_many_ways_to_write_one_chord_agree() {
        let groups: &[&[&str]] = &[
            &["Cmaj7", "CM7", "CΔ", "CΔ7", "C^7", "Cma7", "Cmaj7", "C(maj7)"],
            &["Cm7", "Cmin7", "C-7", "Cmi7", "Cm(7)"],
            &["Cm", "Cmin", "C-", "Cmi"],
            &["C", "Cmaj", "CM"],
            &["CmMaj7", "Cm(maj7)", "CmM7", "CminMaj7", "C-Δ7", "C-Δ"],
            &["Cm7b5", "Cø", "Cø7", "Cmin7b5", "Cm7(b5)", "C-7b5", "Cm7-5"],
            &["Cdim7", "C°7", "Co7"],
            &["Cdim", "C°", "Co"],
            &["C7#5", "C+7", "Caug7", "C7+", "C7+5", "C7(#5)"],
            &["C7b9", "C7-9", "C7(b9)"],
            &["C7#9", "C7+9"],
            &["C6/9", "C69", "C6(9)", "C6add9"],
            &["Csus4", "Csus"],
            &["C7sus4", "C7sus"],
            &["Cadd9", "Cadd2", "C(add9)"],
            &["C7b9#11", "C7(b9,#11)", "C7(b9, #11)", "C7#11b9"],
            &["C7b5#5", "C7#5b5"],
        ];
        for group in groups {
            let first = chord(group[0]);
            for other in &group[1..] {
                assert_eq!(chord(other), first, "{other} should equal {}", group[0]);
            }
        }
    }

    #[test]
    fn names_are_canonical_and_parse_back() {
        let canonical = [
            "C",
            "Cm",
            "Cdim",
            "Caug",
            "C5",
            "Csus2",
            "Csus4",
            "C6",
            "Cm6",
            "C6/9",
            "Cm6/9",
            "C7",
            "Cmaj7",
            "Cm7",
            "CmMaj7",
            "Cdim7",
            "Cm7b5",
            "C9",
            "Cmaj9",
            "Cm9",
            "C11",
            "Cm11",
            "C13",
            "Cm13",
            "Cmaj13",
            "Cadd9",
            "Cmadd9",
            "Cadd11",
            "C7sus4",
            "C9sus4",
            "C13sus4",
            "C7b9",
            "C7#9",
            "C7#11",
            "C7b13",
            "C7b5",
            "C7#5",
            "Cmaj7#5",
            "Cmaj7#11",
            "Cmaj9#11",
            "C13b9",
            "C13#11",
            "C7#9b13",
            "C7b9#9",
            "C7alt",
            "C7no5",
            "Cm9b5",
            "CmMaj9",
            "C7add13",
            "Csus24",
            "F#m7b5",
            "Bb13",
            "Ebmaj7#11",
            "Fmaj7/A",
            "C/E",
            "Dm7/G",
            "Am/F#",
            "C7b5b13",
            "Cm7no5",
            "Cm7add6",
        ];
        for symbol in canonical {
            assert_eq!(chord(symbol).name(), symbol);
        }
        let respelled = [
            ("CM7", "Cmaj7"),
            ("C-7", "Cm7"),
            ("Cø", "Cm7b5"),
            ("C+", "Caug"),
            ("C°7", "Cdim7"),
            ("Csus", "Csus4"),
            ("C69", "C6/9"),
            ("Cadd2", "Cadd9"),
            ("Cadd4", "Cadd11"),
            ("C7+", "C7#5"),
            ("C/C", "C"),
            ("Cno3", "C5"),
            ("Cmaj11", "Cmaj11"),
            ("Cm(maj7)", "CmMaj7"),
            ("C7b5#5", "C7b5b13"),
        ];
        for (written, name) in respelled {
            assert_eq!(chord(written).name(), name, "{written}");
        }
    }

    #[test]
    fn slash_chords_keep_their_bass() {
        assert_eq!(chord("G/B").bass_name().to_string(), "B");
        assert_eq!(chord("G/B").root.to_string(), "G");
        assert_eq!(chord("C/Bb").bass, Some(NoteName::parse("Bb").unwrap()), "the bass need not be a chord tone");
        assert_eq!(chord("C").bass_name().to_string(), "C");
        assert_eq!(chord("Bb6/9").bass, None, "the slash of a six-nine chord is not a bass");
        assert_eq!(chord("Bb6/9/D").bass_name().to_string(), "D");
        assert_eq!(chord("Fmaj7/A").spell(57), "A3");
        assert_eq!(chord("Db7").spell(59), "Cb4");
        assert_eq!(chord("C").spell(61), "Db4");

        let f_major = Scale::parse("F major").unwrap();
        assert_eq!(chord("D7/Gb").spelled_in(&f_major).name(), "D7/F#", "a chord tone in the bass follows the chord");
        assert_eq!(chord("A#m").spelled_in(&f_major).name(), "Bbm");
        assert_eq!(chord("C/A#").spelled_in(&f_major).name(), "C/Bb", "any other bass follows the key");
        assert_eq!(chord("Abm").spelled_in(&Scale::parse("E major").unwrap()).name(), "G#m");
    }

    #[test]
    fn malformed_symbols_are_rejected_with_the_reason() {
        let cases = [
            ("", "expected a root"),
            ("H7", "expected a root"),
            ("am7", "did you mean 'Am7'"),
            ("7", "expected a root"),
            ("Cm7b", "accidental with no tone"),
            ("C7#", "accidental with no tone"),
            ("C/", "not a bass note"),
            ("C/x", "not a bass note"),
            ("C/9", "is not something a chord symbol can end with"),
            ("C2", "ambiguous"),
            ("C4", "ambiguous"),
            ("C8", "not a chord extension"),
            ("Cm7b7", "not an alteration"),
            ("C7b11", "not an alteration"),
            ("C7#13", "not an alteration"),
            ("Csus3", "not a suspension"),
            ("Cadd", "needs a tone"),
            ("Cno7", "only the third or fifth"),
            ("Cdim9", "diminished chord takes no"),
            ("Cmmaj", "needs its seventh"),
            ("Cmajor7", "is not something a chord symbol can end with"),
            ("Chello", "is not something a chord symbol can end with"),
            ("Cmalt", "'alt' belongs on a dominant"),
            ("C7 G7", "is not something a chord symbol can end with"),
            ("Cø9", "half-diminished"),
            ("C###", "expected a root"),
        ];
        for (symbol, reason) in cases {
            let error = Chord::parse(symbol).unwrap_err();
            assert!(error.message().contains(&format!("'{symbol}'")), "{symbol}: {error}");
            assert!(error.message().contains(reason), "{symbol}: {error}");
        }
    }

    #[test]
    fn no_chord_marks_are_recognised() {
        for text in ["N.C.", "NC", "n.c.", "N.C", " nc "] {
            assert!(is_no_chord(text), "{text}");
        }
        assert!(!is_no_chord("C"));
        assert!(!is_no_chord("N"));
    }

    #[test]
    fn pitches_are_named_back_as_chords() {
        let cases: &[(&[u8], &str)] = &[
            (&[60, 64, 67], "C"),
            (&[64, 67, 72], "C/E"),
            (&[67, 72, 76], "C/G"),
            (&[57, 60, 64], "Am"),
            (&[60, 64, 67, 69], "C6"),
            (&[57, 60, 64, 67], "Am7"),
            (&[59, 62, 65, 69], "Bm7b5"),
            (&[62, 65, 69, 71], "Dm6"),
            (&[60, 63, 66, 69], "Cdim7"),
            (&[60, 64, 68], "Caug"),
            (&[60, 65, 67], "Csus4"),
            (&[60, 62, 67], "Csus2"),
            (&[60, 67], "C5"),
            (&[60, 64], "C"),
            (&[57, 60], "Am"),
            (&[50, 54, 60], "D7"),
            (&[48, 52, 59], "Cmaj7"),
            (&[45, 52, 55, 59, 60], "Am9"),
            (&[53, 57, 60, 64], "Fmaj7"),
            (&[57, 60, 64, 65], "Fmaj7/A"),
            (&[55, 59, 62, 65], "G7"),
            (&[59, 62, 65, 67], "G7/B"),
            (&[52, 56, 62, 67, 72], "E7#9b13"),
            (&[48, 55, 58, 64, 69], "C13"),
            (&[58, 62, 65, 67, 72], "Bb6/9"),
            (&[50, 54, 57, 64], "Dadd9"),
            (&[43, 55, 60, 62, 65], "G7sus4"),
            (&[61, 64, 67, 71], "Dbm7b5"),
            (&[36, 48, 60, 64, 67, 72, 76], "C"),
        ];
        for (pitches, expected) in cases {
            let named = identify(pitches).map(|chord| chord.name());
            assert_eq!(named.as_deref(), Some(*expected), "{pitches:?}");
        }
        assert_eq!(identify(&[]), None);
        assert_eq!(identify(&[60, 72]), None, "one pitch class is a note, not a chord");
        assert_eq!(identify(&[60, 61, 62, 63]), None, "a cluster has no name");
    }

    #[test]
    fn every_identifiable_quality_is_found_on_every_root() {
        for symbol in QUALITIES {
            let quality = Quality::parse(symbol).unwrap();
            assert_eq!(&quality.symbol(), symbol, "the list should use canonical names");
            for root in 0..12u8 {
                let pitches: Vec<u8> = quality.tones().iter().map(|tone| 48 + root + tone.semitones() as u8).collect();
                let found = identify(&pitches).unwrap_or_else(|| panic!("{symbol} on {root} was not identified"));
                assert_eq!(found.root.pitch_class(), root, "{symbol} on {root} read as {found}");
                assert_eq!(
                    mask(found.pitch_classes()),
                    mask(pitches.iter().map(|p| p % 12)),
                    "{symbol} read as {found}"
                );
            }
        }
    }
}
