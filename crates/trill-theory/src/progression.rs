//! Chord progressions in time: a compact chart notation, and the rhythms
//! chords are played in.
//!
//! ```text
//! Am9 | Fmaj7 | C G/B | E7          one bar each; C and G/B share a bar
//! Am F C G                          no bar lines: one chord per bar
//! Dm7 . . G7 | Cmaj7                dots hold: Dm7 for three beats, G7 for one
//! Am | . | F | %                    Am for two bars; % plays the bar before again
//! C:3 G:1 | F:1.5 G:2.5             explicit lengths in beats
//! ii7 V7 | Imaj7 | N.C.             Roman numerals (with a key), and a silent bar
//! ```
//!
//! Chords in a bar share it evenly unless lengths are given. A dot holds
//! the previous chord for one more share, also across a bar line.

use crate::chord::{Chord, is_no_chord};
use crate::error::{Error, Result, list};
use crate::expression::{Dynamics, Level, MIN_LENGTH};
use crate::roman;
use crate::scale::Scale;
use crate::steps::{Cell, StepPattern};
use crate::timing::{EPSILON, parse_beats, pulses};
use crate::voicing::{Voicing, VoicingSpec, voice};
use trill_core::{Note, TimeSignature};

/// One chord, or one silence, and when it sounds.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// `None` for `N.C.`.
    pub chord: Option<Chord>,
    /// Beats from the start of the progression.
    pub start: f64,
    pub length: f64,
    /// The symbol as it was written (`"ii7"`, `"Am9"`).
    pub written: String,
}

impl Span {
    pub fn end(&self) -> f64 {
        self.start + self.length
    }
}

/// A parsed progression.
#[derive(Clone, Debug, PartialEq)]
pub struct Progression {
    pub spans: Vec<Span>,
    pub bars: usize,
    pub signature: TimeSignature,
    /// Readings the writer may not have expected, such as how a Roman
    /// numeral was resolved or that three chords split a bar into thirds.
    pub notes: Vec<String>,
}

/// A slot of a bar before lengths are worked out.
#[derive(Clone)]
struct Slot {
    /// `None` holds the previous chord.
    chord: Option<(Option<Chord>, String)>,
    beats: Option<f64>,
}

impl Progression {
    /// Parses a progression. `key` is needed only for Roman numerals.
    pub fn parse(text: &str, signature: TimeSignature, key: Option<&Scale>) -> Result<Self> {
        let bar_length = signature.beats_per_bar();
        let meter = format!("{}/{}", signature.numerator, signature.denominator);
        let segments: Vec<String> = if text.contains('|') {
            let parts: Vec<&str> = text.split('|').map(str::trim).collect();
            let last = parts.len() - 1;
            let mut segments = Vec::new();
            for (index, part) in parts.iter().enumerate() {
                if part.is_empty() && index != 0 && index != last {
                    return Err(Error::new(format!(
                        "'{text}': bar {} is empty; write N.C. for a silent bar or . to hold the chord",
                        segments.len() + 1
                    )));
                }
                if !part.is_empty() {
                    segments.push(part.to_string());
                }
            }
            segments
        } else {
            text.split_whitespace().map(str::to_string).collect()
        };
        if segments.is_empty() {
            return Err(Error::new(format!(
                "'{text}' has no chords; expected a progression like \"Am9 | Fmaj7 | C G/B\""
            )));
        }

        // Lengths that do not fill a bar are most often chords meant to share one.
        let hint = if text.contains('|') {
            ""
        } else {
            "; without bar lines every chord gets a bar of its own, so end the bar with | to put them in one"
        };
        let mut notes: Vec<String> = Vec::new();
        let mut spans: Vec<Span> = Vec::new();
        let mut previous_bar: Option<Vec<Slot>> = None;
        for (index, segment) in segments.iter().enumerate() {
            let bar = index + 1;
            let located = |error: Error| error.within(format!("bar {bar}"));
            let tokens: Vec<&str> = segment.split_whitespace().collect();
            let slots: Vec<Slot> = if tokens == ["%"] {
                previous_bar.clone().ok_or_else(|| {
                    located(Error::new("'%' repeats the bar before it, and this is the first bar".to_string()))
                })?
            } else {
                tokens
                    .iter()
                    .map(|token| parse_slot(token, key, &mut notes))
                    .collect::<Result<Vec<Slot>>>()
                    .map_err(located)?
            };

            let fixed: f64 = slots.iter().filter_map(|slot| slot.beats).sum();
            let free = slots.iter().filter(|slot| slot.beats.is_none()).count();
            let remaining = bar_length - fixed;
            if free == 0 && remaining.abs() > EPSILON {
                return Err(located(Error::new(format!(
                    "'{segment}' adds up to {fixed} beats, but a bar of {meter} has {bar_length}{hint}"
                ))));
            }
            if free > 0 && remaining < EPSILON {
                return Err(located(Error::new(format!(
                    "'{segment}': the lengths given take {fixed} of the bar's {bar_length} beats and leave nothing \
                     for the other chords"
                ))));
            }
            let share = if free > 0 { remaining / free as f64 } else { 0.0 };
            // A share off the sixteenth grid is legal but rarely what was meant.
            if free > 0 && ((share * 4.0).round() - share * 4.0).abs() > 1e-6 {
                notes.push(format!(
                    "bar {bar}: {free} chords share the bar evenly, {:.3} beats each; use dots (C . G Am) or \
                     lengths (C:2 G:1 Am:1) for another split",
                    share
                ));
            }

            let mut position = (bar - 1) as f64 * bar_length;
            for slot in &slots {
                let length = slot.beats.unwrap_or(share);
                match &slot.chord {
                    Some((chord, written)) => {
                        spans.push(Span { chord: chord.clone(), start: position, length, written: written.clone() });
                    }
                    None => match spans.last_mut() {
                        Some(span) => span.length += length,
                        None => {
                            return Err(located(Error::new(
                                "'.' holds the chord before it, and nothing comes before this one".to_string(),
                            )));
                        }
                    },
                }
                position += length;
            }
            previous_bar = Some(slots);
        }
        Ok(Self { spans, bars: segments.len(), signature, notes })
    }

    /// Length in beats.
    pub fn length(&self) -> f64 {
        self.bars as f64 * self.signature.beats_per_bar()
    }

    /// The progression repeated, or cut short, to last exactly `bars` bars.
    pub fn cycled(&self, bars: usize) -> Self {
        let (period, total) = (self.length(), bars as f64 * self.signature.beats_per_bar());
        let mut spans = Vec::new();
        let mut offset = 0.0;
        while offset < total - EPSILON {
            for span in &self.spans {
                let start = offset + span.start;
                if start < total - EPSILON {
                    spans.push(Span { start, length: span.length.min(total - start), ..span.clone() });
                }
            }
            offset += period;
        }
        Self { spans, bars, signature: self.signature, notes: self.notes.clone() }
    }

    /// The chords that start in each bar, by name, for showing what was
    /// read: `["Am9", "Fmaj7", "C G/B"]`. A bar that only holds an earlier
    /// chord shows it in brackets.
    pub fn bar_names(&self) -> Vec<String> {
        let bar_length = self.signature.beats_per_bar();
        let name = |span: &Span| span.chord.as_ref().map_or_else(|| "N.C.".to_string(), Chord::name);
        (0..self.bars)
            .map(|bar| {
                let (start, end) = (bar as f64 * bar_length, (bar + 1) as f64 * bar_length);
                let starting: Vec<String> = self
                    .spans
                    .iter()
                    .filter(|span| span.start >= start - EPSILON && span.start < end - EPSILON)
                    .map(name)
                    .collect();
                if !starting.is_empty() {
                    return starting.join(" ");
                }
                let held = self.spans.iter().rev().find(|span| span.start < start);
                held.map_or_else(String::new, |span| format!("({})", name(span)))
            })
            .collect()
    }

    /// Voices every chord, leading each from the one before. The result has
    /// one entry per span; silences get `None`.
    pub fn voice(&self, spec: &VoicingSpec) -> Vec<Option<Voicing>> {
        let mut previous: Option<Voicing> = None;
        self.spans
            .iter()
            .map(|span| {
                let chord = span.chord.as_ref()?;
                let voicing = voice(chord, spec, previous.as_ref());
                previous = Some(voicing.clone());
                Some(voicing)
            })
            .collect()
    }
}

fn parse_slot(token: &str, key: Option<&Scale>, notes: &mut Vec<String>) -> Result<Slot> {
    let (symbol, beats) = match token.rsplit_once(':') {
        Some((symbol, beats)) => {
            let beats = parse_beats(beats).ok_or_else(|| {
                Error::new(format!("'{token}': '{beats}' is not a length in beats; expected a number, as in C:1.5"))
            })?;
            (symbol, Some(beats))
        }
        None => (token, None),
    };
    if symbol == "." {
        return Ok(Slot { chord: None, beats });
    }
    if symbol == "%" {
        return Err(Error::new("'%' repeats a whole bar and must be alone in it".to_string()));
    }
    let chord = if is_no_chord(symbol) {
        None
    } else if roman::looks_roman(symbol) {
        let key = key.ok_or_else(|| {
            Error::new(format!("'{symbol}' is a Roman numeral, which needs a key; give one, or write chord names"))
        })?;
        let realized = roman::realize(symbol, key)?;
        // A numeral used in every bar is explained once.
        notes.extend(realized.note.filter(|note| !notes.contains(note)));
        Some(realized.chord)
    } else {
        Some(Chord::parse(symbol)?)
    };
    Ok(Slot { chord: Some((chord, symbol.to_string())), beats })
}

/// The rhythm chords (or bass notes) are played in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rhythm {
    /// One strike per chord, held until the next chord.
    Sustain,
    /// One short strike per chord.
    Stabs,
    /// A step pattern, repeated bar after bar.
    Pattern(StepPattern),
}

/// The named rhythms, with what each one is.
pub const RHYTHMS: &[(&str, &str)] = &[
    ("sustain", "each chord struck once and held until the next"),
    ("stabs", "each chord struck once, short"),
    ("pulse", "a held strike on every beat (quarter notes in 4/4, dotted quarters in 6/8)"),
    ("half", "a held strike on the downbeat and the middle of the bar"),
    ("eighths", "straight eighth notes"),
    ("offbeats", "short stabs between the beats: reggae skank, house organ"),
    ("charleston", "beat 1 and the and of 2 (4/4)"),
    ("tresillo", "three held strikes grouped 3+3+2 (4/4)"),
    ("clave", "son clave, 3-2, short stabs over two bars (4/4)"),
];

impl Rhythm {
    /// Parses a named rhythm (see [`RHYTHMS`]) or a step pattern such as
    /// `"x__x__x_"`.
    pub fn parse(text: &str, signature: TimeSignature) -> Result<Self> {
        let name = text.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        let steps = (signature.beats_per_bar() * 4.0).round() as usize;
        let beats = pulses(signature);
        let step_of = |beat: f64| (beat * 4.0).round() as usize;
        // Hits as (step, steps held); everything else rests.
        let bar = |hits: &[(usize, usize)]| {
            let mut cells = vec![Cell::Rest; steps];
            for (start, held) in hits {
                let end = (*start + *held).min(steps);
                if *start < end {
                    cells[*start] = Cell::Hit { level: Level::Normal, roll: 1 };
                    cells[*start + 1..end].fill(Cell::Hold);
                }
            }
            cells
        };
        let until_next = |starts: &[usize]| -> Vec<(usize, usize)> {
            starts
                .iter()
                .enumerate()
                .map(|(index, start)| (*start, starts.get(index + 1).copied().unwrap_or(steps) - start))
                .collect()
        };
        let common_time = signature.numerator == 4 && signature.denominator == 4;
        let pattern = match name.as_str() {
            "sustain" | "sustained" | "hold" => return Ok(Rhythm::Sustain),
            "stabs" | "stab" => return Ok(Rhythm::Stabs),
            "pulse" => bar(&until_next(&beats.iter().map(|beat| step_of(*beat)).collect::<Vec<_>>())),
            "half" => {
                let mut starts = vec![0];
                if beats.len().is_multiple_of(2) {
                    starts.push(step_of(beats[beats.len() / 2]));
                }
                bar(&until_next(&starts))
            }
            "eighths" => bar(&until_next(&(0..steps).step_by(2).collect::<Vec<_>>())),
            "offbeats" | "offbeat" => {
                let hits: Vec<(usize, usize)> = beats
                    .iter()
                    .enumerate()
                    .map(|(index, beat)| {
                        let end = beats.get(index + 1).copied().unwrap_or(signature.beats_per_bar());
                        // Halfway in eighths, rounding late: the "and" of a
                        // simple beat, the third eighth of a compound one.
                        let eighths = ((end - beat) * 2.0).round() as usize;
                        (step_of(*beat) + eighths.div_ceil(2) * 2, 2)
                    })
                    .collect();
                bar(&hits)
            }
            "charleston" | "tresillo" | "clave" if !common_time => {
                let general: Vec<&str> =
                    RHYTHMS.iter().map(|(name, _)| *name).filter(|name| !FOUR_FOUR_ONLY.contains(name)).collect();
                return Err(Error::new(format!(
                    "the '{name}' rhythm is written for 4/4, not {}/{}; use one of {} or a step pattern",
                    signature.numerator,
                    signature.denominator,
                    list(general)
                )));
            }
            "charleston" => bar(&[(0, 4), (6, 6)]),
            "tresillo" => bar(&until_next(&[0, 6, 12])),
            "clave" => {
                return Ok(Rhythm::Pattern(StepPattern::from_bars(vec![
                    bar(&[(0, 2), (6, 2), (12, 2)]),
                    bar(&[(4, 2), (8, 2)]),
                ])));
            }
            _ => {
                let looks_like_steps = !text.trim().is_empty()
                    && text.chars().all(|c| "xXoOg._-|".contains(c) || c.is_ascii_digit() || c.is_whitespace());
                if looks_like_steps {
                    return Ok(Rhythm::Pattern(StepPattern::parse(text)?));
                }
                return Err(Error::new(format!(
                    "'{text}' is not a rhythm; expected one of {} or a step pattern like \"x__x__x_\"",
                    list(RHYTHMS.iter().map(|(name, _)| *name))
                )));
            }
        };
        Ok(Rhythm::Pattern(StepPattern::from_cells(pattern)))
    }
}

const FOUR_FOUR_ONLY: [&str; 3] = ["charleston", "tresillo", "clave"];

/// One strike of a chord: when, for how long, and which chord.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Strike {
    pub start: f64,
    pub length: f64,
    /// Index into [`Progression::spans`].
    pub span: usize,
    pub level: Level,
    /// How many strikes this chord has had before this one.
    pub index: usize,
}

/// Works out when each chord of a progression is struck in a rhythm.
///
/// `anticipation` moves every chord change that many beats early, so the
/// chord arrives ahead of the beat and is held over it: the "push" of pop,
/// funk and Latin comping. A strike of a pattern that falls inside that
/// early window already plays the next chord.
pub fn strikes(progression: &Progression, rhythm: &Rhythm, anticipation: f64) -> Vec<Strike> {
    let spans = &progression.spans;
    let total = progression.length();
    // Each span sounds from its own (possibly early) start to the next one's.
    let early: Vec<f64> = spans
        .iter()
        .enumerate()
        .map(|(index, span)| {
            let room = index > 0 && spans[index - 1].length > anticipation + EPSILON;
            if span.chord.is_some() && room { anticipation.max(0.0) } else { 0.0 }
        })
        .collect();
    let window = |index: usize| {
        let end = if index + 1 < spans.len() { spans[index + 1].start - early[index + 1] } else { total };
        (spans[index].start - early[index], end)
    };

    let mut out: Vec<Strike> = Vec::new();
    let mut counts = vec![0usize; spans.len()];
    let mut push = |start: f64, length: f64, span: usize, level: Level| {
        out.push(Strike { start, length, span, level, index: counts[span] });
        counts[span] += 1;
    };
    match rhythm {
        Rhythm::Sustain | Rhythm::Stabs => {
            let beat = pulses(progression.signature).get(1).copied().unwrap_or(progression.signature.beats_per_bar());
            for (index, span) in spans.iter().enumerate() {
                if span.chord.is_some() {
                    let (start, end) = window(index);
                    let length = if *rhythm == Rhythm::Stabs { (beat / 2.0).min(end - start) } else { end - start };
                    push(start, length, index, Level::Normal);
                }
            }
        }
        Rhythm::Pattern(pattern) => {
            let bar_length = progression.signature.beats_per_bar();
            for hit in pattern.hits_for(bar_length, total) {
                // A held strike that runs into the next chord strikes that chord too.
                let (mut start, end) = (hit.start, (hit.start + hit.length).min(total));
                while start < end - EPSILON {
                    let Some(index) = (0..spans.len()).find(|index| {
                        let (from, to) = window(*index);
                        start >= from - EPSILON && start < to - EPSILON
                    }) else {
                        break;
                    };
                    let stop = end.min(window(index).1);
                    if spans[index].chord.is_some() {
                        push(start, stop - start, index, hit.level);
                    }
                    start = stop;
                }
            }
        }
    }
    out
}

/// Which way a strummed chord is rolled.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum StrumDirection {
    /// Lowest note first.
    #[default]
    Up,
    /// Highest note first.
    Down,
    /// Up and down in turn, like a guitarist's hand.
    Alternate,
}

/// How a chord part is played.
#[derive(Clone, Debug, PartialEq)]
pub struct Comping {
    pub rhythm: Rhythm,
    /// Beats by which chord changes arrive early; see [`strikes`].
    pub anticipation: f64,
    /// Fraction of each strike that sounds: 1 is legato, 0.5 detached.
    pub gate: f64,
    /// Beats between the onsets of neighbouring voices; 0 strikes them
    /// together, 0.02-0.06 sounds strummed or rolled.
    pub strum: f64,
    pub strum_direction: StrumDirection,
    pub dynamics: Dynamics,
}

impl Default for Comping {
    fn default() -> Self {
        Self {
            rhythm: Rhythm::Sustain,
            anticipation: 0.0,
            gate: 1.0,
            strum: 0.0,
            strum_direction: StrumDirection::Up,
            dynamics: Dynamics::default(),
        }
    }
}

/// Writes out a chord part: every strike of every chord as notes.
/// `voicings` is what [`Progression::voice`] returned.
pub fn comp(progression: &Progression, voicings: &[Option<Voicing>], comping: &Comping) -> Vec<Note> {
    let mut notes = Vec::new();
    for (count, strike) in strikes(progression, &comping.rhythm, comping.anticipation).into_iter().enumerate() {
        let Some(Some(voicing)) = voicings.get(strike.span) else { continue };
        let mut pitches = voicing.pitches();
        let voices = pitches.len();
        let downward = match comping.strum_direction {
            StrumDirection::Up => false,
            StrumDirection::Down => true,
            StrumDirection::Alternate => count % 2 == 1,
        };
        if downward {
            pitches.reverse();
        }
        let (lowest, highest) = (pitches.iter().min().copied(), pitches.iter().max().copied());
        let base = comping.dynamics.at(strike.start, progression.signature);
        let sounding = strike.length * comping.gate.max(0.0);
        for (order, pitch) in pitches.into_iter().enumerate() {
            let delay = order as f64 * comping.strum.max(0.0);
            // The outer voices carry the line; the middle of the chord sits back.
            let balance = match Some(pitch) {
                _ if voices < 3 => 0.0,
                top if top == highest => 3.0,
                bottom if bottom == lowest => 0.0,
                _ => -4.0,
            };
            notes.push(Note {
                pitch,
                start: strike.start + delay,
                length: (sounding - delay).max(MIN_LENGTH),
                velocity: strike.level.velocity(base + balance),
            });
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voicing::VoicingStyle;

    fn signature(numerator: u8, denominator: u8) -> TimeSignature {
        TimeSignature::new(numerator, denominator).unwrap()
    }

    fn parse(text: &str, numerator: u8, denominator: u8) -> Progression {
        Progression::parse(text, signature(numerator, denominator), None).unwrap_or_else(|error| panic!("{error}"))
    }

    /// `"Am9@0+4 Fmaj7@4+4"`: each chord with its start and length.
    fn timeline(progression: &Progression) -> String {
        progression
            .spans
            .iter()
            .map(|span| {
                let name = span.chord.as_ref().map_or("N.C.".to_string(), Chord::name);
                format!("{name}@{}+{}", span.start, span.length)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn bars_split_evenly_in_four_four() {
        let progression = parse("Am9 | Fmaj7 | C G/B |", 4, 4);
        assert_eq!(progression.bars, 3);
        assert_eq!(progression.length(), 12.0);
        assert_eq!(timeline(&progression), "Am9@0+4 Fmaj7@4+4 C@8+2 G/B@10+2");
        assert_eq!(progression.bar_names(), ["Am9", "Fmaj7", "C G/B"]);
        assert!(progression.notes.is_empty());

        assert_eq!(timeline(&parse("Am F C G", 4, 4)), "Am@0+4 F@4+4 C@8+4 G@12+4", "no bar lines: a bar each");
        assert_eq!(timeline(&parse("C Am F G | C", 4, 4)), "C@0+1 Am@1+1 F@2+1 G@3+1 C@4+4");
    }

    #[test]
    fn dots_hold_and_percent_repeats() {
        assert_eq!(timeline(&parse("Dm7 . . G7 | Cmaj7", 4, 4)), "Dm7@0+3 G7@3+1 Cmaj7@4+4");
        let held = parse("Am | . | F | %", 4, 4);
        assert_eq!(timeline(&held), "Am@0+8 F@8+4 F@12+4");
        assert_eq!(held.bar_names(), ["Am", "(Am)", "F", "F"]);
        assert_eq!(timeline(&parse("C G | . Am", 4, 4)), "C@0+2 G@2+4 Am@6+2", "a dot holds across the bar line");
        assert_eq!(timeline(&parse("C G | % | F", 4, 4)), "C@0+2 G@2+2 C@4+2 G@6+2 F@8+4");
        assert_eq!(timeline(&parse("Am . F .", 4, 4)), "Am@0+8 F@8+8");
    }

    #[test]
    fn explicit_lengths_and_rests() {
        assert_eq!(timeline(&parse("C:3 G:1 | F:1.5 G:2.5", 4, 4)), "C@0+3 G@3+1 F@4+1.5 G@5.5+2.5");
        assert_eq!(
            timeline(&parse("C:3 G | Am F:1", 4, 4)),
            "C@0+3 G@3+1 Am@4+3 F@7+1",
            "the rest of the bar is shared"
        );
        assert_eq!(timeline(&parse("Am | N.C. | F NC", 4, 4)), "Am@0+4 N.C.@4+4 F@8+2 N.C.@10+2");
        assert_eq!(parse("Am | N.C.", 4, 4).bar_names(), ["Am", "N.C."]);
    }

    #[test]
    fn other_meters() {
        assert_eq!(timeline(&parse("Am | F C | G", 3, 4)), "Am@0+3 F@3+1.5 C@4.5+1.5 G@6+3");
        assert_eq!(timeline(&parse("Am . F | C G Em", 3, 4)), "Am@0+2 F@2+1 C@3+1 G@4+1 Em@5+1");
        assert_eq!(timeline(&parse("Em | C D | Em", 6, 8)), "Em@0+3 C@3+1.5 D@4.5+1.5 Em@6+3");
        assert_eq!(timeline(&parse("Dm | G:2 A:1.5 | Dm", 7, 8)), "Dm@0+3.5 G@3.5+2 A@5.5+1.5 Dm@7+3.5");
        let uneven = parse("Dm G | Dm", 7, 8);
        assert_eq!(timeline(&uneven), "Dm@0+1.75 G@1.75+1.75 Dm@3.5+3.5");
        assert!(uneven.notes.is_empty(), "1.75 beats is on the sixteenth grid");
        let thirds = parse("C G Am | F", 4, 4);
        assert!((thirds.spans[1].start - 4.0 / 3.0).abs() < 1e-9);
        assert!(
            thirds.notes[0].contains("bar 1: 3 chords share the bar evenly, 1.333 beats each"),
            "{:?}",
            thirds.notes
        );
    }

    #[test]
    fn roman_numerals_need_and_use_the_key() {
        let key = Scale::parse("A minor").unwrap();
        let progression = Progression::parse("i | bVI | bIII V7/III | V7", signature(4, 4), Some(&key)).unwrap();
        assert_eq!(progression.bar_names(), ["Am", "F", "C G7", "E7"]);
        assert_eq!(progression.spans[1].written, "bVI");
        assert_eq!(progression.notes.len(), 2, "the two flats are explained once each: {:?}", progression.notes);
        // Symbols and numerals mix.
        let mixed = Progression::parse("Am | iv | Fmaj7", signature(4, 4), Some(&key)).unwrap();
        assert_eq!(mixed.bar_names(), ["Am", "Dm", "Fmaj7"]);
        let error = Progression::parse("ii7 V7", signature(4, 4), None).unwrap_err();
        assert!(error.message().contains("bar 1") && error.message().contains("needs a key"), "{error}");
    }

    #[test]
    fn mistakes_name_the_bar() {
        let four = signature(4, 4);
        for (bad, reason) in [
            ("", "has no chords"),
            (" | ", "has no chords"),
            ("Am | | F", "bar 2 is empty"),
            ("% | Am", "bar 1: '%' repeats the bar before it"),
            (". | Am", "bar 1: '.' holds the chord before it"),
            ("Am | Hm7", "bar 2: 'Hm7' is not a chord symbol"),
            ("Am | C:3 G:2", "bar 2: 'C:3 G:2' adds up to 5 beats, but a bar of 4/4 has 4"),
            ("Am | C:2 G:1", "bar 2: 'C:2 G:1' adds up to 3 beats"),
            ("C:4 G |", "bar 1: 'C:4 G': the lengths given take 4 of the bar's 4 beats"),
            ("C:3 G:1", "bar 1: 'C:3' adds up to 3 beats, but a bar of 4/4 has 4; without bar lines every chord"),
            ("C:x", "bar 1: 'C:x': 'x' is not a length in beats"),
            ("C:0", "is not a length in beats"),
            ("Am | C %", "bar 2: '%' repeats a whole bar"),
            ("Am | Cm7b", "bar 2: 'Cm7b'"),
        ] {
            let error = Progression::parse(bad, four, None).unwrap_err();
            assert!(error.message().contains(reason), "{bad:?}: {error}");
        }
    }

    #[test]
    fn progressions_cycle_to_a_length() {
        let progression = parse("Am | F C", 4, 4);
        let longer = progression.cycled(5);
        assert_eq!(timeline(&longer), "Am@0+4 F@4+2 C@6+2 Am@8+4 F@12+2 C@14+2 Am@16+4");
        assert_eq!(longer.length(), 20.0);
        assert_eq!(timeline(&progression.cycled(1)), "Am@0+4");
        // A chord cut by the end is shortened, not dropped.
        assert_eq!(timeline(&parse("Am | .", 4, 4).cycled(3)), "Am@0+8 Am@8+4");
    }

    fn strike_times(text: &str, rhythm: &str, sig: (u8, u8), anticipation: f64) -> Vec<(f64, f64, usize)> {
        let progression = parse(text, sig.0, sig.1);
        let rhythm = Rhythm::parse(rhythm, progression.signature).unwrap_or_else(|error| panic!("{error}"));
        strikes(&progression, &rhythm, anticipation).iter().map(|s| (s.start, s.length, s.span)).collect()
    }

    #[test]
    fn sustain_and_stabs_follow_the_chords() {
        assert_eq!(strike_times("Am | F C", "sustain", (4, 4), 0.0), [(0.0, 4.0, 0), (4.0, 2.0, 1), (6.0, 2.0, 2)]);
        assert_eq!(strike_times("Am | F C", "stabs", (4, 4), 0.0), [(0.0, 0.5, 0), (4.0, 0.5, 1), (6.0, 0.5, 2)]);
        assert_eq!(strike_times("Am | N.C. | F", "sustain", (4, 4), 0.0), [(0.0, 4.0, 0), (8.0, 4.0, 2)]);
        assert_eq!(strike_times("Em | C D", "stabs", (6, 8), 0.0), [(0.0, 0.75, 0), (3.0, 0.75, 1), (4.5, 0.75, 2)]);
    }

    #[test]
    fn named_rhythms_fit_the_meter() {
        let starts = |text: &str, rhythm: &str, sig: (u8, u8)| -> Vec<f64> {
            strike_times(text, rhythm, sig, 0.0).iter().map(|strike| strike.0).collect()
        };
        assert_eq!(starts("C", "pulse", (4, 4)), [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(starts("C", "pulse", (3, 4)), [0.0, 1.0, 2.0]);
        assert_eq!(starts("C", "pulse", (6, 8)), [0.0, 1.5]);
        assert_eq!(starts("C", "pulse", (7, 8)), [0.0, 1.0, 2.0]);
        assert_eq!(starts("C", "half", (4, 4)), [0.0, 2.0]);
        assert_eq!(starts("C", "half", (3, 4)), [0.0]);
        assert_eq!(starts("C", "half", (6, 8)), [0.0, 1.5]);
        assert_eq!(starts("C", "eighths", (4, 4)), [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
        assert_eq!(starts("C", "eighths", (7, 8)).len(), 7);
        assert_eq!(starts("C", "eighths", (6, 8)).len(), 6);
        assert_eq!(starts("C", "offbeats", (4, 4)), [0.5, 1.5, 2.5, 3.5]);
        assert_eq!(starts("C", "offbeats", (6, 8)), [1.0, 2.5], "the last eighth of each dotted beat");
        assert_eq!(starts("C", "tresillo", (4, 4)), [0.0, 1.5, 3.0]);
        assert_eq!(starts("C", "charleston", (4, 4)), [0.0, 1.5]);
        assert_eq!(starts("C | C", "clave", (4, 4)), [0.0, 1.5, 3.0, 5.0, 6.0]);
        // Pulses are held to the next one; a 7/8 bar ends on a longer beat.
        assert_eq!(strike_times("C", "pulse", (7, 8), 0.0), [(0.0, 1.0, 0), (1.0, 1.0, 0), (2.0, 1.5, 0)]);

        let error = Rhythm::parse("tresillo", signature(6, 8)).unwrap_err();
        assert!(
            error.message().contains("written for 4/4, not 6/8") && error.message().contains("offbeats"),
            "{error}"
        );
        let error = Rhythm::parse("bossa", signature(4, 4)).unwrap_err();
        assert!(error.message().contains("'bossa' is not a rhythm") && error.message().contains("tresillo"), "{error}");
        for (name, description) in RHYTHMS {
            assert!(Rhythm::parse(name, signature(4, 4)).is_ok(), "{name}");
            assert!(description.len() > 10);
        }
    }

    #[test]
    fn patterns_strike_whatever_chord_is_sounding() {
        // A custom pattern over a bar with two chords.
        assert_eq!(
            strike_times("C G |", "x__x__x_", (4, 4), 0.0),
            [(0.0, 1.5, 0), (1.5, 0.5, 0), (2.0, 1.0, 1), (3.0, 1.0, 1)]
        );
        // The second strike was held into G, so G is struck where it arrives.
        let over_rest = strike_times("C N.C. |", "xxxx", (4, 4), 0.0);
        assert_eq!(over_rest, [(0.0, 1.0, 0), (1.0, 1.0, 0)], "nothing is struck during N.C.");
        // A two-bar pattern keeps going over a longer progression.
        let long = strike_times("C | F | G | C", "x...|..x.", (4, 4), 0.0);
        assert_eq!(long.iter().map(|strike| strike.0).collect::<Vec<_>>(), [0.0, 6.0, 8.0, 14.0]);
        let indexes: Vec<usize> = {
            let progression = parse("C | F", 4, 4);
            let rhythm = Rhythm::parse("pulse", progression.signature).unwrap();
            strikes(&progression, &rhythm, 0.0).iter().map(|strike| strike.index).collect()
        };
        assert_eq!(indexes, [0, 1, 2, 3, 0, 1, 2, 3], "strikes are counted per chord");
    }

    #[test]
    fn anticipation_pushes_chord_changes_early() {
        assert_eq!(strike_times("Am | F | C", "sustain", (4, 4), 0.5), [(0.0, 3.5, 0), (3.5, 4.0, 1), (7.5, 4.5, 2)]);
        // In a pattern, the strike on the and of four already plays the next chord.
        let pushed = strike_times("Am | F", "x_____.x", (4, 4), 0.5);
        assert_eq!(pushed, [(0.0, 3.0, 0), (3.5, 0.5, 1), (4.0, 3.0, 1), (7.5, 0.5, 1)]);
        // A chord too short to borrow from is left alone.
        assert_eq!(strike_times("C:0.5 G:3.5 |", "sustain", (4, 4), 0.5), [(0.0, 0.5, 0), (0.5, 3.5, 1)]);
    }

    #[test]
    fn comping_writes_voiced_notes() {
        let progression = parse("Am9 | Fmaj7", 4, 4);
        let spec = VoicingSpec { style: VoicingStyle::Pad, ..VoicingSpec::default() };
        let voicings = progression.voice(&spec);
        let comping = Comping { dynamics: Dynamics { velocity: 80, accent: 10 }, ..Comping::default() };
        let notes = comp(&progression, &voicings, &comping);
        let first_chord: Vec<&Note> = notes.iter().filter(|note| note.start == 0.0).collect();
        assert_eq!(first_chord.len(), voicings[0].as_ref().unwrap().pitches().len());
        assert!(first_chord.iter().all(|note| note.length == 4.0));
        let top = first_chord.iter().max_by_key(|note| note.pitch).unwrap();
        let inner = first_chord.iter().find(|note| note.pitch != top.pitch && note.pitch > 52).unwrap();
        assert_eq!((top.velocity, inner.velocity), (93, 86), "the top voice sings over the inner ones");
        assert_eq!(
            notes.iter().filter(|note| note.start == 4.0).count(),
            voicings[1].as_ref().unwrap().pitches().len()
        );

        // Stabs with a short gate, strummed downward.
        let stabs = Comping {
            rhythm: Rhythm::parse("x.x.", progression.signature).unwrap(),
            gate: 0.5,
            strum: 0.03,
            strum_direction: StrumDirection::Down,
            ..Comping::default()
        };
        let notes = comp(&progression, &voicings, &stabs);
        let strike: Vec<&Note> = notes.iter().filter(|note| note.start < 1.0).collect();
        let highest = strike.iter().max_by_key(|note| note.pitch).unwrap();
        let lowest = strike.iter().min_by_key(|note| note.pitch).unwrap();
        assert_eq!(highest.start, 0.0, "a down strum starts from the top");
        assert!((lowest.start - 0.03 * (strike.len() - 1) as f64).abs() < 1e-9);
        assert!((highest.length - 0.5).abs() < 1e-9, "one step of a beat, half of it sounding");
        assert!((lowest.end() - highest.end()).abs() < 1e-9, "strummed notes still end together");

        // Alternate strumming flips every strike.
        let alternate = Comping { strum: 0.05, strum_direction: StrumDirection::Alternate, ..stabs.clone() };
        let notes = comp(&progression, &voicings, &alternate);
        let first_of = |from: f64| {
            notes
                .iter()
                .filter(|note| note.start >= from && note.start < from + 1.0)
                .min_by(|a, b| a.start.total_cmp(&b.start))
                .unwrap()
                .pitch
        };
        assert!(first_of(0.0) < first_of(2.0), "up on the first strike, down on the second");
    }
}
