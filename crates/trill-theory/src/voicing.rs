//! Voicings: which chord tones to play and in which octave, and how to move
//! from one chord to the next without the hands jumping about.
//!
//! A voicing has two parts. The *upper voices* are the chord proper; they
//! are chosen among every inversion and octave that fits the register, and
//! the one that moves least from the previous chord wins. That is all voice
//! leading is: common tones stay, the rest step to the nearest chord tone,
//! and because voices are kept in order from low to high they never cross.
//! The *bass* is handled separately, as a bassist or a left hand would: it
//! plays the chord's bass note in a fixed low octave and is free to leap.
//!
//! What gets doubled depends on the instrument. On a piano, low notes are
//! thick and every doubled third or seventh sticks out, so the bass is at
//! most doubled at the octave and the upper voices never repeat a tone. A
//! synth pad sustains evenly and wants a foundation, so the pad voicing
//! puts root and fifth at the bottom, doubles them freely, and keeps the
//! tones that give the chord its colour (third, seventh, tensions) once
//! each in the middle.

use crate::chord::Chord;
use crate::error::{Error, Result, list};
use crate::pitch::{Interval, at_or_above, pitch_class};

/// How the upper voices are laid out.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VoicingStyle {
    /// All tones within an octave.
    Close,
    /// Spread wide: every other tone of the close position dropped an
    /// octave (the middle note for a triad).
    Open,
    /// Close position with the second voice from the top dropped an octave.
    Drop2,
    /// Close position with the third voice from the top dropped an octave.
    Drop3,
    /// Root plus the two tones that define the chord: third and seventh.
    Shell,
    /// Root and fifth low, colour tones in close position above.
    Pad,
}

impl VoicingStyle {
    pub const ALL: [VoicingStyle; 6] = [
        VoicingStyle::Close,
        VoicingStyle::Open,
        VoicingStyle::Drop2,
        VoicingStyle::Drop3,
        VoicingStyle::Shell,
        VoicingStyle::Pad,
    ];

    pub fn name(self) -> &'static str {
        match self {
            VoicingStyle::Close => "close",
            VoicingStyle::Open => "open",
            VoicingStyle::Drop2 => "drop2",
            VoicingStyle::Drop3 => "drop3",
            VoicingStyle::Shell => "shell",
            VoicingStyle::Pad => "pad",
        }
    }

    /// One line for a reference listing.
    pub fn description(self) -> &'static str {
        match self {
            VoicingStyle::Close => "all chord tones within an octave; compact, the default for keys and plucks",
            VoicingStyle::Open => "close position with alternate tones dropped an octave; wide and airy, for strings",
            VoicingStyle::Drop2 => {
                "second voice from the top dropped an octave; the standard jazz piano and guitar spread"
            }
            VoicingStyle::Drop3 => "third voice from the top dropped an octave; wider, with a strong bass-melody frame",
            VoicingStyle::Shell => "root, third and seventh only; lean comping that leaves room for other parts",
            VoicingStyle::Pad => {
                "root and fifth low with the colour tones stacked above; full and warm, for synth pads"
            }
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let wanted: String = text.trim().chars().filter(|c| !matches!(c, '-' | '_' | ' ')).collect();
        let wanted = match wanted.to_ascii_lowercase().as_str() {
            "spread" => "open".to_string(),
            other => other.to_string(),
        };
        Self::ALL.into_iter().find(|style| style.name() == wanted).ok_or_else(|| {
            Error::new(format!("'{text}' is not a voicing; expected one of {}", list(Self::ALL.map(Self::name))))
        })
    }
}

/// What is played under the upper voices.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Bass {
    /// Nothing: another part has the bass.
    #[default]
    None,
    /// The chord's bass note (its slash bass, or the root).
    Root,
    /// The bass note doubled at the octave, like a pianist's left hand.
    Octaves,
}

/// How to voice a chord.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoicingSpec {
    pub style: VoicingStyle,
    /// Lowest pitch of the upper voices.
    pub low: u8,
    /// Highest pitch of the upper voices.
    pub high: u8,
    /// The most upper voices to use. Chords with more tones than this lose
    /// the least important first: the fifth, then the root if a bass note
    /// has it, then the lower tensions.
    pub voices: usize,
    pub bass: Bass,
    /// The bass note sits in the octave starting here.
    pub bass_low: u8,
    /// Use this inversion (0 = root position) for every chord instead of
    /// leading each from the last.
    pub inversion: Option<usize>,
}

impl Default for VoicingSpec {
    fn default() -> Self {
        // E3-E5 keeps thirds clear of the mud below C3 and the top voice
        // under the register where a melody would sit.
        Self {
            style: VoicingStyle::Close,
            low: 52,
            high: 76,
            voices: 4,
            bass: Bass::None,
            bass_low: 36,
            inversion: None,
        }
    }
}

/// A chord placed on the keyboard.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Voicing {
    /// Notes under the chord, ascending. Empty when no bass was asked for.
    pub bass: Vec<u8>,
    /// The chord proper, ascending.
    pub upper: Vec<u8>,
}

impl Voicing {
    /// Every pitch, ascending.
    pub fn pitches(&self) -> Vec<u8> {
        let mut all: Vec<u8> = self.bass.iter().chain(&self.upper).copied().collect();
        all.sort_unstable();
        all.dedup();
        all
    }
}

/// Voices one chord. With `previous`, the upper voices move as little as
/// possible from it; without, they sit in the middle of the register.
pub fn voice(chord: &Chord, spec: &VoicingSpec, previous: Option<&Voicing>) -> Voicing {
    let previous_upper = previous.map(|voicing| voicing.upper.as_slice()).filter(|upper| !upper.is_empty());
    let has_bass = spec.bass != Bass::None || spec.style == VoicingStyle::Pad;
    let candidates = match spec.style {
        VoicingStyle::Shell => shell_candidates(chord, spec),
        style => {
            let tones = match style {
                VoicingStyle::Pad => pad_tones(chord, spec.voices),
                _ => select_tones(chord, spec.voices.max(1), has_bass),
            };
            let forced = spec.inversion.map(|inversion| inversion % tones.len().max(1));
            close_positions(chord, &tones, spec, forced).into_iter().map(|close| spread(style, close)).collect()
        }
    };
    let leading = if spec.inversion.is_some() { None } else { previous_upper };
    let upper = candidates
        .into_iter()
        .filter(|candidate| candidate.iter().all(|pitch| (0..=127).contains(pitch)))
        .min_by_key(|candidate| cost(candidate, spec, leading))
        .map(|candidate| candidate.into_iter().map(|pitch| pitch as u8).collect::<Vec<u8>>())
        .unwrap_or_default();

    let bass = bass_notes(chord, spec, &upper);
    Voicing { bass, upper }
}

/// Voices a progression, leading each chord from the one before.
pub fn voice_all<'a>(chords: impl IntoIterator<Item = &'a Chord>, spec: &VoicingSpec) -> Vec<Voicing> {
    let mut voicings: Vec<Voicing> = Vec::new();
    for chord in chords {
        let voicing = voice(chord, spec, voicings.last());
        voicings.push(voicing);
    }
    voicings
}

/// The chord in close position with its `inversion`th tone lowest (0 = root
/// position), starting at or above `floor`.
pub fn inversion(chord: &Chord, inversion: usize, floor: u8) -> Vec<u8> {
    let classes = chord.pitch_classes();
    let lowest = classes[inversion % classes.len()];
    let bottom = at_or_above(lowest, floor as i32);
    let mut pitches: Vec<i32> = classes.iter().map(|pc| at_or_above(*pc, bottom)).collect();
    pitches.sort_unstable();
    pitches.into_iter().filter(|pitch| *pitch <= 127).map(|pitch| pitch as u8).collect()
}

/// Total semitones the voices travel between two chords, voice by voice
/// from the bottom, so voices never cross. When the chords have different
/// numbers of voices, neighbouring voices of the fuller chord share a voice
/// of the thinner one.
pub fn movement(from: &[u8], to: &[u8]) -> u32 {
    let (long, short) = if from.len() >= to.len() { (from, to) } else { (to, from) };
    if short.is_empty() {
        return 0;
    }
    let distance = |i: usize, j: usize| (long[i] as i32 - short[j] as i32).unsigned_abs();
    // best[j]: least movement with the voices of `long` so far ending on voice j of `short`.
    let mut best = vec![u32::MAX; short.len()];
    best[0] = distance(0, 0);
    for i in 1..long.len() {
        for j in (0..short.len()).rev() {
            let stay = best[j];
            let step = if j > 0 { best[j - 1] } else { u32::MAX };
            let reached = stay.min(step);
            best[j] = if reached == u32::MAX { u32::MAX } else { reached + distance(i, j) };
        }
    }
    best[short.len() - 1]
}

/// Pairs of neighbouring pitches that sit too low for the interval between
/// them and will sound muddy (the arranger's "low interval limits": a third
/// needs to be above about C3, a second above E3, while fifths and octaves
/// stay clear much lower).
pub fn muddy_intervals(pitches: &[u8]) -> Vec<(u8, u8)> {
    /// Lowest allowed bottom note for each interval of 1-11 semitones.
    const LIMITS: [u8; 12] = [0, 52, 51, 48, 46, 36, 47, 31, 43, 41, 41, 41];
    let mut sorted = pitches.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted
        .windows(2)
        .filter(|pair| pair[1] - pair[0] < 12 && pair[0] < LIMITS[(pair[1] - pair[0]) as usize])
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

/// The tones the upper voices use when there are more than `limit`.
fn select_tones(chord: &Chord, limit: usize, bass_below: bool) -> Vec<Interval> {
    let quality = &chord.quality;
    let mut tones: Vec<Interval> = quality.tones().to_vec();
    // A natural eleventh a ninth above a major third is the one clash chord
    // players always avoid; the eleventh wins because the symbol asked for it.
    if quality.has(Interval::new(11, 0)) && quality.has(Interval::MAJOR_THIRD) {
        tones.retain(|tone| *tone != Interval::MAJOR_THIRD);
    }
    let importance = |tone: &Interval| match (tone.number, tone.alter) {
        (2..=4, _) => 100,
        (6 | 7, _) => 90,
        (5, alter) if alter != 0 => 80,
        (9.., alter) if alter != 0 => 70,
        // With a bass note underneath, the upper voices can go rootless.
        (1, _) => {
            if bass_below {
                5
            } else {
                60
            }
        }
        (13, _) => 50,
        (11, _) => 45,
        (9, _) => 40,
        _ => 10,
    };
    let mut kept = tones.clone();
    kept.sort_by_key(|tone| std::cmp::Reverse(importance(tone)));
    kept.truncate(limit);
    tones.retain(|tone| kept.contains(tone));
    tones
}

/// The upper voices of a pad: the colour tones once each, filled out with
/// the fifth and then the root when the chord has few of them.
fn pad_tones(chord: &Chord, limit: usize) -> Vec<Interval> {
    let is_frame = |tone: &Interval| tone.number == 1 || *tone == Interval::PERFECT_FIFTH;
    let all = select_tones(chord, usize::MAX, false);
    let mut tones: Vec<Interval> = select_tones(chord, usize::MAX, true).into_iter().filter(|t| !is_frame(t)).collect();
    if tones.len() > limit.max(1) {
        let fewer = select_tones(chord, limit.max(1) + 2, true);
        tones.retain(|tone| fewer.contains(tone));
        tones.truncate(limit.max(1));
    }
    // Fifth before root: the bass already has the root, and third-fifth-
    // seventh is a triad of its own that leads smoothly to the next chord.
    for frame in all.iter().rev().filter(|tone| is_frame(tone)) {
        if tones.len() < 3.min(limit.max(1)) {
            tones.push(*frame);
        }
    }
    tones.sort();
    tones
}

/// Every close-position stack of `tones` whose bottom note lies in or just
/// under the register. With `forced`, only stacks built on that tone.
fn close_positions(chord: &Chord, tones: &[Interval], spec: &VoicingSpec, forced: Option<usize>) -> Vec<Vec<i32>> {
    let root = chord.root.pitch_class() as i32;
    let classes: Vec<u8> = tones.iter().map(|tone| (root + tone.semitones()).rem_euclid(12) as u8).collect();
    let mut out = Vec::new();
    for (index, bottom_class) in classes.iter().enumerate() {
        if forced.is_some_and(|forced| forced != index) {
            continue;
        }
        let mut bottom = at_or_above(*bottom_class, spec.low as i32 - 12);
        while bottom <= spec.high as i32 {
            let mut stack: Vec<i32> = classes.iter().map(|pc| at_or_above(*pc, bottom)).collect();
            stack.sort_unstable();
            out.push(stack);
            bottom += 12;
        }
    }
    out
}

/// Turns a close position into the asked-for spread by dropping voices an
/// octave.
fn spread(style: VoicingStyle, mut close: Vec<i32>) -> Vec<i32> {
    let count = close.len();
    // Voices counted from the top, as "drop 2" does.
    let from_top: &[usize] = match style {
        VoicingStyle::Drop2 => &[2],
        VoicingStyle::Drop3 => &[3],
        VoicingStyle::Open if count == 3 => &[2],
        VoicingStyle::Open => &[2, 4],
        _ => &[],
    };
    for position in from_top {
        if *position <= count && count > 1 {
            close[count - position] -= 12;
        }
    }
    close.sort_unstable();
    close
}

/// Root at the bottom and the two guide tones above it, in either order.
fn shell_candidates(chord: &Chord, spec: &VoicingSpec) -> Vec<Vec<i32>> {
    let quality = &chord.quality;
    let tone_of = |number: u8| quality.tones().iter().copied().find(|tone| tone.number == number);
    let first = quality.third_or_sus().or_else(|| tone_of(5));
    let second = tone_of(7).or_else(|| tone_of(6)).or_else(|| tone_of(5)).filter(|tone| Some(*tone) != first);
    let root_class = chord.root.pitch_class();
    let class_of = |tone: Interval| ((root_class as i32 + tone.semitones()).rem_euclid(12)) as u8;

    let mut out = Vec::new();
    let mut root = at_or_above(root_class, spec.low as i32 - 12);
    while root <= spec.high as i32 {
        match (first, second) {
            (Some(first), Some(second)) => {
                for (lower, upper) in [(first, second), (second, first)] {
                    let lower = at_or_above(class_of(lower), root + 1);
                    out.push(vec![root, lower, at_or_above(class_of(upper), lower + 1)]);
                }
            }
            (Some(only), None) | (None, Some(only)) => out.push(vec![root, at_or_above(class_of(only), root + 1)]),
            (None, None) => out.push(vec![root]),
        }
        root += 12;
    }
    out
}

/// How good a candidate is; lower is better. Staying in the register comes
/// first, then moving little, then sounding clear.
fn cost(candidate: &[i32], spec: &VoicingSpec, previous: Option<&[u8]>) -> i64 {
    let (low, high) = (spec.low as i32, spec.high as i32);
    let outside: i32 = candidate.iter().map(|pitch| (low - pitch).max(0) + (pitch - high).max(0)).sum();
    let centre = (low + high) / 2;
    let mean = candidate.iter().sum::<i32>() / candidate.len().max(1) as i32;
    let mut cost = outside as i64 * 10_000 + (mean - centre).abs() as i64;

    if let Some(previous) = previous {
        let now: Vec<u8> = candidate.iter().map(|pitch| *pitch as u8).collect();
        cost += movement(previous, &now) as i64 * 10;
        // The top voice is the one heard as a melody; keep it smoothest.
        if let (Some(before), Some(after)) = (previous.last(), candidate.last()) {
            cost += (*before as i32 - after).abs() as i64 * 3;
        }
    } else {
        cost += (mean - centre).abs() as i64 * 3;
    }

    let last_pair = candidate.len().saturating_sub(2);
    for (index, pair) in candidate.windows(2).enumerate() {
        // A semitone rub is colour inside a chord but blurs the top note.
        if pair[1] - pair[0] == 1 {
            cost += if index == last_pair { 25 } else { 6 };
        }
    }
    let as_pitches: Vec<u8> = candidate.iter().map(|pitch| *pitch as u8).collect();
    cost + muddy_intervals(&as_pitches).len() as i64 * 30
}

fn bass_notes(chord: &Chord, spec: &VoicingSpec, upper: &[u8]) -> Vec<u8> {
    let wanted = if spec.style == VoicingStyle::Pad && spec.bass == Bass::None { Bass::Root } else { spec.bass };
    if wanted == Bass::None {
        return Vec::new();
    }
    let ceiling = upper.first().map_or(127, |lowest| *lowest as i32);
    let mut bass = at_or_above(chord.bass_name().pitch_class(), spec.bass_low as i32);
    while bass >= ceiling && bass >= 12 {
        bass -= 12;
    }
    let mut notes = vec![bass];
    if wanted == Bass::Octaves {
        notes.push(if bass + 12 < ceiling { bass + 12 } else { bass - 12 });
    }
    // The pad's foundation: a fifth over the root, when the chord has one
    // and it stays clear of the voices above.
    let fifth = bass + 7;
    if spec.style == VoicingStyle::Pad
        && chord.bass.is_none()
        && chord.quality.has(Interval::PERFECT_FIFTH)
        && fifth < ceiling
        && muddy_intervals(&[bass as u8, fifth as u8]).is_empty()
    {
        notes.push(fifth);
    }
    let mut notes: Vec<u8> = notes.into_iter().filter(|pitch| (0..=127).contains(pitch)).map(|p| p as u8).collect();
    notes.sort_unstable();
    notes.dedup();
    notes
}

/// Whether every pitch of `voicing` is a tone of `chord` or its bass.
pub fn belongs_to(voicing: &Voicing, chord: &Chord) -> bool {
    let mut classes = chord.pitch_classes();
    classes.push(chord.bass_name().pitch_class());
    voicing.pitches().iter().all(|pitch| classes.contains(&pitch_class(*pitch)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roman;
    use crate::scale::Scale;

    fn chord(text: &str) -> Chord {
        Chord::parse(text).unwrap()
    }

    fn chords(text: &str) -> Vec<Chord> {
        text.split_whitespace().map(chord).collect()
    }

    fn spec(style: VoicingStyle) -> VoicingSpec {
        VoicingSpec { style, ..VoicingSpec::default() }
    }

    fn total_movement(voicings: &[Vec<u8>]) -> u32 {
        voicings.windows(2).map(|pair| movement(&pair[0], &pair[1])).sum()
    }

    const SYMBOLS: &[&str] = &[
        "C",
        "Am",
        "F#m",
        "Bb",
        "Ddim",
        "Eaug",
        "Gsus4",
        "A5",
        "C6",
        "Dm7",
        "G7",
        "Cmaj7",
        "F#m7b5",
        "Bdim7",
        "Am9",
        "Fmaj9",
        "E7#9b13",
        "Bb13",
        "Ebmaj7#11",
        "D7alt",
        "G11",
        "Cm11",
        "Fmaj7/A",
        "C/E",
        "Dm7/G",
        "Ab6/9",
    ];

    #[test]
    fn every_style_stays_in_its_register_and_in_the_chord() {
        for style in VoicingStyle::ALL {
            for (low, high) in [(52, 76), (48, 72), (55, 84), (43, 67)] {
                let spec = VoicingSpec { style, low, high, bass: Bass::Root, ..VoicingSpec::default() };
                let mut previous: Option<Voicing> = None;
                for symbol in SYMBOLS {
                    let chord = chord(symbol);
                    let voicing = voice(&chord, &spec, previous.as_ref());
                    let label = format!("{symbol} {} in {low}-{high}: {voicing:?}", style.name());
                    assert!(!voicing.upper.is_empty(), "{label}");
                    assert!(voicing.upper.iter().all(|pitch| (low..=high).contains(pitch)), "{label}");
                    assert!(voicing.upper.windows(2).all(|pair| pair[0] < pair[1]), "{label}");
                    assert!(belongs_to(&voicing, &chord), "{label}");
                    let bass = voicing.bass[0];
                    assert_eq!(pitch_class(bass), chord.bass_name().pitch_class(), "{label}");
                    assert!(bass < voicing.upper[0], "the bass stays under the chord: {label}");
                    previous = Some(voicing);
                }
            }
        }
    }

    #[test]
    fn the_tones_that_define_a_chord_are_always_kept() {
        for style in VoicingStyle::ALL {
            for symbol in ["Dm7", "G7", "Cmaj7", "Am9", "Bb13", "E7#9b13", "F#m7b5", "Cm11", "Fmaj9"] {
                let chord = chord(symbol);
                let voicing = voice(&chord, &VoicingSpec { bass: Bass::Root, ..spec(style) }, None);
                let classes: Vec<u8> = voicing.pitches().iter().map(|pitch| pitch_class(*pitch)).collect();
                let root = chord.root.pitch_class() as i32;
                for number in [3, 7] {
                    let tone = chord.quality.tones().iter().find(|tone| tone.number == number).unwrap();
                    let class = (root + tone.semitones()).rem_euclid(12) as u8;
                    assert!(
                        classes.contains(&class),
                        "{symbol} {} lost its {}: {voicing:?}",
                        style.name(),
                        tone.label()
                    );
                }
            }
        }
    }

    #[test]
    fn styles_have_their_shapes() {
        let close = voice(&chord("Cmaj7"), &VoicingSpec { inversion: Some(0), ..spec(VoicingStyle::Close) }, None);
        assert_eq!(close.upper, [60, 64, 67, 71]);
        assert!(close.bass.is_empty());

        let drop2 = voice(&chord("Cmaj7"), &VoicingSpec { inversion: Some(0), ..spec(VoicingStyle::Drop2) }, None);
        assert_eq!(drop2.upper, [55, 60, 64, 71], "the fifth, second from the top, drops an octave");

        let drop3 = voice(&chord("Cmaj7"), &VoicingSpec { inversion: Some(0), ..spec(VoicingStyle::Drop3) }, None);
        assert_eq!(drop3.upper, [52, 60, 67, 71]);

        let open = voice(&chord("C"), &VoicingSpec { inversion: Some(0), ..spec(VoicingStyle::Open) }, None);
        assert_eq!(open.upper, [52, 60, 67], "an open triad: the third moves under the root");
        let open = voice(&chord("Cmaj7"), &VoicingSpec { inversion: Some(1), ..spec(VoicingStyle::Open) }, None);
        assert_eq!(open.upper, [52, 59, 67, 72], "drop 2 and 4");

        let shell = voice(&chord("G7"), &spec(VoicingStyle::Shell), None);
        assert_eq!(shell.upper.len(), 3);
        assert_eq!(pitch_class(shell.upper[0]), 7, "a shell has the root at the bottom");
        let above: Vec<u8> = shell.upper[1..].iter().map(|pitch| pitch_class(*pitch)).collect();
        assert!(above.contains(&11) && above.contains(&5), "then the third and seventh: {shell:?}");
        assert_eq!(voice(&chord("A5"), &spec(VoicingStyle::Shell), None).upper.len(), 2);

        let pad = voice(&chord("Am9"), &spec(VoicingStyle::Pad), None);
        assert_eq!(pad.bass, [45, 52], "root and fifth underneath");
        let colours: Vec<u8> = pad.upper.iter().map(|pitch| pitch_class(*pitch)).collect();
        assert!([0, 7, 11].iter().all(|class| colours.contains(class)), "third, seventh and ninth above: {pad:?}");
        assert!(!colours.contains(&9), "the root is not repeated among the colour tones: {pad:?}");

        // A plain triad has one colour tone, so root and fifth fill the pad out.
        let triad = voice(&chord("F"), &spec(VoicingStyle::Pad), None);
        assert_eq!(triad.bass, [41, 48]);
        assert_eq!(triad.upper.len(), 3);
        // A slash chord keeps its bass and gets no fifth stacked on it.
        assert_eq!(voice(&chord("C/E"), &spec(VoicingStyle::Pad), None).bass, [40]);
    }

    #[test]
    fn large_chords_shed_the_right_tones() {
        let names = |chord: &Chord, tones: Vec<Interval>| -> String {
            tones.iter().map(|tone| chord.root.up(*tone).to_string()).collect::<Vec<_>>().join(" ")
        };
        let c13 = chord("C13");
        assert_eq!(names(&c13, select_tones(&c13, 4, false)), "C E Bb A", "without a bass the root stays");
        assert_eq!(names(&c13, select_tones(&c13, 4, true)), "E Bb D A", "over a bass the voicing goes rootless");
        let am9 = chord("Am9");
        assert_eq!(names(&am9, select_tones(&am9, 4, true)), "C E G B");
        let g11 = chord("G11");
        assert!(!select_tones(&g11, 6, false).contains(&Interval::MAJOR_THIRD), "the third gives way to the eleventh");
        let alt = chord("E7#9b13");
        assert_eq!(names(&alt, select_tones(&alt, 4, true)), "G# D F## C");
    }

    #[test]
    fn voice_leading_moves_less_than_block_chords() {
        let c_major = Scale::parse("C major").unwrap();
        let mut progressions: Vec<Vec<Chord>> = [
            "Dm7 G7 Cmaj7 A7 Dm7 G7 Cmaj7 Cmaj7",
            "C Am F G C Am F G",
            "Am F C G Am F C G",
            "Cmaj7 Fmaj7 Bm7b5 E7 Am7 Dm7 G7 Cmaj7",
            "Am9 Fmaj7 C G/B Am9 Fmaj7 C G/B",
            "Ebmaj7 Cm7 Fm7 Bb7 Gm7 C7 Fm7 Bb7",
            "F#m7 B7 Emaj7 C#m7 F#m7 B7 Emaj7 E6",
        ]
        .iter()
        .map(|text| chords(text))
        .collect();
        // ii-V-I in every key.
        for tonic in ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"] {
            let key = Scale::parse(&format!("{tonic} major")).unwrap();
            progressions.push(
                ["ii7", "V7", "Imaj7", "vi7", "ii7", "V7", "Imaj7"]
                    .iter()
                    .map(|numeral| roman::realize(numeral, &key).unwrap().chord)
                    .collect(),
            );
        }
        progressions.push(
            ["I", "vi", "ii", "V", "iii", "vi", "IV", "V"]
                .iter()
                .map(|n| roman::realize(n, &c_major).unwrap().chord)
                .collect(),
        );

        for style in [VoicingStyle::Close, VoicingStyle::Drop2, VoicingStyle::Open, VoicingStyle::Pad] {
            for progression in &progressions {
                let names = progression.iter().map(Chord::name).collect::<Vec<_>>().join(" ");
                let led: Vec<Vec<u8>> =
                    voice_all(progression, &spec(style)).into_iter().map(|voicing| voicing.upper).collect();
                // The baseline: the same layout with every chord in root position.
                let block: Vec<Vec<u8>> = progression
                    .iter()
                    .map(|chord| voice(chord, &VoicingSpec { inversion: Some(0), ..spec(style) }, None).upper)
                    .collect();
                let (led_total, block_total) = (total_movement(&led), total_movement(&block));
                assert!(
                    led_total * 2 <= block_total,
                    "{} {names}: led {led_total} should be under half of block {block_total}",
                    style.name()
                );
                let voices = led.iter().map(Vec::len).max().unwrap() as u32;
                let changes = (led.len() - 1) as u32;
                assert!(
                    led_total <= 2 * voices * changes,
                    "{} {names}: {led_total} semitones is more than a tone per voice per change",
                    style.name()
                );
                // The melody on top never leaps.
                for pair in led.windows(2) {
                    let leap = (*pair[0].last().unwrap() as i32 - *pair[1].last().unwrap() as i32).abs();
                    assert!(leap <= 5, "{} {names}: the top voice leaps {leap}", style.name());
                }
            }
        }
    }

    #[test]
    fn common_tones_are_held() {
        let voicings = voice_all(&chords("C Am F"), &spec(VoicingStyle::Close));
        let shared = |a: &Voicing, b: &Voicing| a.upper.iter().filter(|pitch| b.upper.contains(pitch)).count();
        assert_eq!(shared(&voicings[0], &voicings[1]), 2, "C and Am share C and E: {voicings:?}");
        assert_eq!(shared(&voicings[1], &voicings[2]), 2, "Am and F share A and C: {voicings:?}");
        assert_eq!(movement(&voicings[0].upper, &voicings[1].upper), 2);
    }

    #[test]
    fn movement_counts_semitones_without_crossing() {
        assert_eq!(movement(&[60, 64, 67], &[60, 64, 67]), 0);
        assert_eq!(movement(&[60, 64, 67], &[60, 65, 69]), 3);
        assert_eq!(movement(&[60, 64, 67], &[59, 62, 65, 67]), 4);
        assert_eq!(movement(&[59, 62, 65, 67], &[60, 64, 67]), 4);
        assert_eq!(movement(&[60, 64, 67], &[72, 76, 79]), 36);
        assert_eq!(movement(&[60], &[60, 64, 67]), 11);
        assert_eq!(movement(&[], &[60]), 0);
    }

    #[test]
    fn inversions_stack_from_the_named_tone() {
        assert_eq!(inversion(&chord("C"), 0, 60), [60, 64, 67]);
        assert_eq!(inversion(&chord("C"), 1, 60), [64, 67, 72]);
        assert_eq!(inversion(&chord("C"), 2, 60), [67, 72, 76]);
        assert_eq!(inversion(&chord("Dm7"), 3, 48), [48, 50, 53, 57]);
        assert_eq!(inversion(&chord("C"), 3, 60), [60, 64, 67], "inversions wrap round");
    }

    #[test]
    fn low_close_intervals_are_flagged() {
        assert_eq!(muddy_intervals(&[36, 40]), [(36, 40)], "a third at C2 is mud");
        assert_eq!(muddy_intervals(&[36, 43, 48]), [], "fifths and fourths are fine down there");
        assert_eq!(muddy_intervals(&[48, 52, 55]), [], "a triad from C3 is clear");
        assert_eq!(muddy_intervals(&[47, 50]), [(47, 50)], "a minor third below C3 is not");
        assert_eq!(muddy_intervals(&[45, 52, 55, 59, 60]), [], "a pad voicing of Am9");
        assert_eq!(muddy_intervals(&[50, 51]), [(50, 51)]);
        assert_eq!(muddy_intervals(&[24, 36]), [], "octaves are never muddy");
    }

    #[test]
    fn the_bass_follows_slash_chords_and_can_be_doubled() {
        let with_bass = VoicingSpec { bass: Bass::Root, ..spec(VoicingStyle::Close) };
        assert_eq!(voice(&chord("Fmaj7/A"), &with_bass, None).bass, [45]);
        assert_eq!(voice(&chord("Fmaj7"), &with_bass, None).bass, [41]);
        assert_eq!(voice(&chord("C/Bb"), &with_bass, None).bass, [46], "even when it is not a chord tone");
        let octaves = VoicingSpec { bass: Bass::Octaves, ..spec(VoicingStyle::Close) };
        assert_eq!(voice(&chord("C"), &octaves, None).bass, [36, 48]);
        let low_chord = VoicingSpec { low: 40, high: 64, ..octaves };
        let voicing = voice(&chord("G"), &low_chord, None);
        assert!(voicing.bass.iter().all(|bass| *bass < voicing.upper[0]), "{voicing:?}");
    }

    #[test]
    fn voicing_names_parse() {
        for style in VoicingStyle::ALL {
            assert_eq!(VoicingStyle::parse(style.name()).unwrap(), style);
            assert!(style.description().len() > 20);
        }
        assert_eq!(VoicingStyle::parse("Drop-2").unwrap(), VoicingStyle::Drop2);
        assert_eq!(VoicingStyle::parse("spread").unwrap(), VoicingStyle::Open);
        let error = VoicingStyle::parse("jazzy").unwrap_err();
        assert!(error.message().contains("'jazzy'") && error.message().contains("drop2"), "{error}");
    }
}
