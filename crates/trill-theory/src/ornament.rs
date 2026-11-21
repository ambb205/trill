//! Ornaments: what a bend, a slide, vibrato or a hammer-on becomes on the
//! instrument that is to play it.
//!
//! A phrase says *that* a note is bent or hammered on
//! ([`Mark`](crate::melody::Mark)); how depends on the instrument:
//!
//! * Where the instrument has an articulation of its own for it (a sampled
//!   guitar's hammer-on, slide and glide keyswitches), the note asks for
//!   that by name, and [`articulate`](crate::articulation::articulate)
//!   writes the keys.
//! * Otherwise it is the pitch wheel. A bend, a pre-bend, vibrato and a
//!   fall are curves on the note itself. A hammer-on or a slide between
//!   notes needs there to be no new attack, so the note before is held
//!   through and the wheel steps or glides to the new pitch, as far as the
//!   instrument's bend range reaches; beyond it the note is struck anew.
//! * An instrument that slurs notes that overlap (a wind model, a
//!   monophonic synth) is given a hammer-on as an overlap.
//!
//! The wheel moves every note of a track at once, which is what a guitar's
//! whammy bar does and not what its fingers do: ornaments are for single
//! lines.

use crate::articulation::{JOIN_OVERLAP, unknown};
use crate::error::Result;
use crate::expression::{Dynamics, MIN_LENGTH, clamp_velocity};
use crate::melody::{Mark, PhraseBar, Tone};
use crate::timing::EPSILON;
use std::collections::BTreeMap;
use trill_core::project::{AutomationPoint, Segment};
use trill_core::{InstrumentProfile, Lasts, Note, TimeSignature};

/// How wide vibrato is when the phrase does not say, in semitones each way.
pub const VIBRATO_DEPTH: f64 = 0.3;
/// How fast vibrato is, in cycles a second.
pub const VIBRATO_HZ: f64 = 5.5;
/// How far below a note a slide with nothing before it begins, and how far
/// a fall drops, in semitones.
const SCOOP: f64 = 2.0;
/// The longest a slide takes, in beats.
const SLIDE_BEATS: f64 = 0.25;
/// How long after the last note the wheel is let go, in beats.
const LET_GO: f64 = 0.25;

/// The names an instrument's own articulation for each ornament goes by.
const HAMMER_NAMES: [&str; 4] = ["hammer", "hammer_on", "legato", "slur"];
const GLIDE_NAMES: [&str; 3] = ["glide", "legato_slide", "portamento"];
const SLIDE_IN_NAMES: [&str; 2] = ["slide", "slide_in"];

/// How a line is to be played.
#[derive(Clone, Debug)]
pub struct Playing<'a> {
    /// Fraction of each note that sounds.
    pub gate: f64,
    pub dynamics: Dynamics,
    pub signature: TimeSignature,
    /// The bars the line is laid through, from its beat 0, where they are
    /// not all bars of `signature`: the last goes on repeating. Empty for a
    /// line in one time signature.
    pub bars: &'a [PhraseBar],
    /// Beats a minute: vibrato has a speed in seconds.
    pub tempo: f64,
    /// What is known about the instrument.
    pub profile: &'a InstrumentProfile,
    /// The articulation every note without one of its own asks for.
    pub articulation: Option<&'a str>,
}

/// A line as notes and the wheel's movement.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Performance {
    /// The notes, each with the articulation it asks of the instrument.
    pub notes: Vec<(Note, Option<String>)>,
    /// The pitch wheel's course, from -1 to 1 of the instrument's bend
    /// range, in beats like the notes. Empty if nothing bends.
    pub wheel: Vec<AutomationPoint>,
    /// What each ornament was written as, with how many times:
    /// `("hammer", "the instrument's 'hammer' keyswitch", 3)`.
    pub written: Vec<(String, String, usize)>,
    /// What could not be written as asked, in words.
    pub remarks: Vec<String>,
}

/// How a note begins.
#[derive(Copy, Clone, PartialEq)]
enum Entry {
    /// Struck.
    Attack,
    /// Not struck: the note at this index of the output is held through,
    /// and the wheel steps to the new pitch.
    Step(usize),
    /// The same, with the wheel gliding there.
    Glide(usize),
}

/// The velocity each tone gets: the dynamics, lifted a little toward the
/// top of the line's range, the way a singer or player leans on the high
/// notes.
pub fn velocities(tones: &[Tone], dynamics: &Dynamics, signature: TimeSignature) -> Vec<u8> {
    velocities_through(tones, dynamics, &[PhraseBar::of(signature)])
}

/// [`velocities`] for a line laid through `bars`, each stressed on its own
/// beats.
pub fn velocities_through(tones: &[Tone], dynamics: &Dynamics, bars: &[PhraseBar]) -> Vec<u8> {
    let lowest = tones.iter().map(|tone| tone.pitch).min().unwrap_or(0) as f64;
    let highest = tones.iter().map(|tone| tone.pitch).max().unwrap_or(0) as f64;
    let span = highest - lowest;
    tones
        .iter()
        .map(|tone| {
            let lift = if span > 0.0 { ((tone.pitch as f64 - lowest) / span - 0.5) * 8.0 } else { 0.0 };
            let (start, bar) = PhraseBar::at(bars, tone.start);
            clamp_velocity(dynamics.at(tone.start - start, bar.signature) + lift)
        })
        .collect()
}

/// The wheel's course while it is being drawn, in semitones.
#[derive(Default)]
struct Wheel {
    points: Vec<AutomationPoint>,
    value: f64,
    widest: f64,
}

impl Wheel {
    fn push(&mut self, beat: f64, value: f64, shape: Segment) {
        let beat = self.points.last().map_or(beat, |last| beat.max(last.beat));
        match self.points.last_mut() {
            Some(last) if (last.beat - beat).abs() < 1e-9 && last.value == value => last.shape = shape,
            _ => self.points.push(AutomationPoint { beat, value, shape, id: 0 }),
        }
        self.value = value;
        self.widest = self.widest.max(value.abs());
    }

    /// Jumps to `value` at `beat` and stays.
    fn step(&mut self, beat: f64, value: f64) {
        if self.points.is_empty() && value != 0.0 {
            // The course begins at rest, whoever reads it.
            self.points.push(AutomationPoint { beat, value: 0.0, shape: Segment::Hold, id: 0 });
        }
        self.push(beat, value, Segment::Hold);
    }

    /// Travels from where it is at `from` to `value` at `to`, and stays.
    fn ramp(&mut self, from: f64, to: f64, value: f64) {
        if to <= from + 1e-9 {
            return self.step(from, value);
        }
        self.push(from, self.value, Segment::Linear);
        self.push(to, value, Segment::Hold);
    }

    /// Back to rest at `beat`, if it is not there.
    fn rest(&mut self, beat: f64) {
        if self.value != 0.0 {
            self.step(beat, 0.0);
        }
    }
}

/// Works out how `tones` are played on the instrument `playing` describes.
/// Fails only for a mark that names an articulation the instrument does
/// not have.
pub fn perform(tones: &[Tone], playing: &Playing) -> Result<Performance> {
    let profile = playing.profile;
    let range = profile.bend_semitones();
    let velocity = match playing.bars {
        [] => velocities(tones, &playing.dynamics, playing.signature),
        bars => velocities_through(tones, &playing.dynamics, bars),
    };
    let gate = playing.gate.max(0.0);
    let mut out = Performance::default();
    let mut wheel = Wheel::default();
    let mut counts: BTreeMap<(&'static str, String), usize> = BTreeMap::new();
    let mut count = |ornament: &'static str, how: &str| *counts.entry((ornament, how.to_string())).or_default() += 1;
    let mut struck_anew = 0usize;

    // The note in the output that sounds through the tone before, and the
    // pitch the wheel is measured from while it does.
    let mut sounding: Option<(usize, u8)> = None;
    let mut last_end = 0.0f64;
    for (index, tone) in tones.iter().enumerate() {
        let (start, pitch) = (tone.start, tone.pitch);
        let end = start + (tone.length * gate).max(MIN_LENGTH);
        let follows = index > 0 && tones[index - 1].start + tones[index - 1].length >= start - EPSILON;
        let own = tone.marks.iter().find_map(|mark| match mark {
            Mark::Named(name) => Some(name.as_str()),
            _ => None,
        });
        if let Some(name) = own
            && profile.articulation(name).is_none()
        {
            return Err(unknown(profile, name));
        }
        let mut how: Option<String> = own.or(playing.articulation).map(str::to_string);
        let has = |wanted: fn(&Mark) -> bool| tone.marks.iter().find(|mark| wanted(mark));

        // How the note begins: struck, or carried on from the one before.
        let mut entry = Entry::Attack;
        let mut slide_from: Option<f64> = None;
        let within = |anchor: u8| (pitch as f64 - anchor as f64).abs() <= range + 1e-9;
        if has(|mark| matches!(mark, Mark::Join)).is_some() {
            if let Some(hammer) = profile.articulation_like(&HAMMER_NAMES, Lasts::Joins) {
                how = Some(hammer.name.clone());
                count("hammer", &format!("the instrument's '{}' keyswitch", hammer.name));
            } else if profile.slurs {
                if let Some((before, _)) = sounding {
                    let note = &mut out.notes[before].0;
                    note.length = note.length.max(start + JOIN_OVERLAP - note.start);
                }
                count("hammer", "a note overlapping the one before, which this instrument slurs");
            } else if let Some((before, anchor)) = sounding.filter(|(_, anchor)| within(*anchor)) {
                entry = Entry::Step(before);
                let _ = anchor;
                count("hammer", "a pitch-bend step on the note before, held through");
            } else {
                struck_anew += 1;
                count("hammer", "an ordinary note");
            }
        } else if has(|mark| matches!(mark, Mark::Scoop)).is_some() {
            if let Some(slide) = profile.articulation_like(&SLIDE_IN_NAMES, Lasts::Once) {
                how = Some(slide.name.clone());
                count("scoop", &format!("the instrument's '{}' keyswitch", slide.name));
            } else {
                slide_from = Some(-SCOOP);
                count("scoop", "a pitch bend into the note, from below");
            }
        } else if let Some(Mark::Slide(from)) = has(|mark| matches!(mark, Mark::Slide(_))) {
            match (from, follows) {
                (Some(from), _) => {
                    slide_from = Some(*from as f64 - pitch as f64);
                    count("slide", "a pitch bend into the note");
                }
                (None, true) => {
                    if let Some(glide) = profile.articulation_like(&GLIDE_NAMES, Lasts::Joins) {
                        how = Some(glide.name.clone());
                        count("slide", &format!("the instrument's '{}' keyswitch", glide.name));
                    } else if let Some((before, _)) = sounding.filter(|(_, anchor)| within(*anchor)) {
                        entry = Entry::Glide(before);
                        count("slide", "a pitch-bend glide from the note before, held through");
                    } else {
                        slide_from = Some(tones[index - 1].pitch as f64 - pitch as f64);
                        count("slide", "a pitch bend into the note");
                    }
                }
                (None, false) => {
                    if let Some(slide) = profile.articulation_like(&SLIDE_IN_NAMES, Lasts::Once) {
                        how = Some(slide.name.clone());
                        count("slide", &format!("the instrument's '{}' keyswitch", slide.name));
                    } else {
                        slide_from = Some(-SCOOP);
                        count("slide", "a pitch bend into the note, from below");
                    }
                }
            }
        }

        // Where the wheel sits under this note before its own ornaments.
        let slide_time = SLIDE_BEATS.min((end - start) / 2.0);
        let base = match entry {
            Entry::Attack => {
                wheel.rest(start);
                out.notes.push((
                    Note { pitch, start, length: end - start, velocity: velocity[index], ..Default::default() },
                    how.clone(),
                ));
                sounding = Some((out.notes.len() - 1, pitch));
                0.0
            }
            Entry::Step(before) | Entry::Glide(before) => {
                let anchor = sounding.map_or(pitch, |(_, anchor)| anchor);
                let base = pitch as f64 - anchor as f64;
                let note = &mut out.notes[before].0;
                note.length = end - note.start;
                match entry {
                    Entry::Glide(_) => wheel.ramp(start, start + slide_time, base),
                    _ => wheel.step(start, base),
                }
                base
            }
        };
        if let Some(offset) = slide_from {
            wheel.step(start, base + offset);
            wheel.ramp(start, start + slide_time, base);
        }

        // The note's own ornaments, around `centre`.
        let span = end - start;
        let mut centre = base;
        let mut vibrato_from = start + (0.3 * span).min(0.2 * playing.tempo / 60.0);
        let mut vibrato_to = end;
        if let Some(Mark::Prebend(target)) = has(|mark| matches!(mark, Mark::Prebend(_))) {
            wheel.step(start, base + (*target as f64 - pitch as f64));
            wheel.ramp(start + span / 3.0, start + 2.0 * span / 3.0, base);
            vibrato_from = start + 2.0 * span / 3.0;
            count("prebend", "a pitch bend");
        }
        if let Some(Mark::Bend(target)) = has(|mark| matches!(mark, Mark::Bend(_))) {
            let rise = (span / 3.0).min(1.0);
            centre = base + (*target as f64 - pitch as f64);
            wheel.ramp(start, start + rise, centre);
            vibrato_from = start + rise;
            if has(|mark| matches!(mark, Mark::Release)).is_some() {
                vibrato_to = end - rise;
                count("bend", "a pitch bend, let back down");
            } else {
                count("bend", "a pitch bend");
            }
        }
        let fall = has(|mark| matches!(mark, Mark::Fall)).map(|_| (0.4 * span).min(0.5));
        if let Some(fall) = fall {
            vibrato_to = vibrato_to.min(end - fall);
        }
        if let Some(Mark::Vibrato(depth)) = has(|mark| matches!(mark, Mark::Vibrato(_))) {
            let depth = depth.unwrap_or(VIBRATO_DEPTH);
            let quarter = playing.tempo / 60.0 / VIBRATO_HZ / 4.0;
            let (mut at, mut swing, mut grown) = (vibrato_from, 1.0, 0.5);
            while at + 2.0 * quarter <= vibrato_to + 1e-9 {
                // Out to one side over a quarter of a cycle the first time, then side to side.
                let reach = if grown < 1.0 { quarter } else { 2.0 * quarter };
                wheel.ramp(at, at + reach, centre + swing * depth * f64::min(grown, 1.0));
                at += reach;
                swing = -swing;
                grown += 0.25;
            }
            if at > vibrato_from {
                wheel.ramp(at, (at + quarter).min(vibrato_to), centre);
                count("vib", "a pitch-bend vibrato");
            }
        }
        if has(|mark| matches!(mark, Mark::Release)).is_some() {
            wheel.ramp(vibrato_to, end, base);
        }
        if let Some(fall) = fall {
            wheel.ramp(end - fall, end, wheel.value - SCOOP);
            count("fall", "a pitch bend down at the end");
        }
        last_end = end;
    }
    // Let go of the wheel once the last note has had a moment to die away.
    wheel.rest(last_end + LET_GO);

    if wheel.widest > range + 1e-9 {
        out.remarks.push(format!(
            "A bend of {} semitones was asked for, and this instrument bends {} each way; it was written as far as \
             the wheel goes. If the instrument can be set to bend further, set it and tell the track its bend_range.",
            round(wheel.widest),
            round(range)
        ));
    }
    if struck_anew > 0 {
        out.remarks.push(format!(
            "{struck_anew} hammered note(s) are further from the note before than this instrument bends ({} \
             semitones) and were struck as ordinary notes.",
            round(range)
        ));
    }
    out.wheel = wheel
        .points
        .iter()
        .map(|point| AutomationPoint { value: (point.value / range).clamp(-1.0, 1.0), ..*point })
        .collect();
    out.written = counts.into_iter().map(|((ornament, how), count)| (ornament.to_string(), how, count)).collect();
    Ok(out)
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::melody::Phrase;
    use crate::scale::Scale;
    use trill_core::Articulation;
    use trill_core::project::automation_value;

    fn four() -> TimeSignature {
        TimeSignature::default()
    }

    fn tones(text: &str) -> Vec<Tone> {
        let key = Scale::parse("A minor").unwrap();
        Phrase::parse(text, Some(&key), 4, four()).unwrap_or_else(|error| panic!("{error}")).tones
    }

    fn playing(profile: &InstrumentProfile) -> Playing<'_> {
        Playing {
            gate: 1.0,
            dynamics: Dynamics { velocity: 90, accent: 0 },
            signature: four(),
            bars: &[],
            tempo: 120.0,
            profile,
            articulation: None,
        }
    }

    /// The wheel in semitones at each of `beats`, for a bend range of two.
    fn bent(performance: &Performance, beats: &[f64]) -> Vec<f64> {
        beats.iter().map(|beat| (automation_value(&performance.wheel, *beat) * 2.0 * 1e3).round() / 1e3).collect()
    }

    fn pitches(performance: &Performance) -> Vec<(u8, f64, f64)> {
        performance.notes.iter().map(|(note, _)| (note.pitch, note.start, note.length)).collect()
    }

    fn how(performance: &Performance, ornament: &str) -> Vec<(String, usize)> {
        performance
            .written
            .iter()
            .filter(|(name, ..)| name == ornament)
            .map(|(_, how, count)| (how.clone(), *count))
            .collect()
    }

    #[test]
    fn a_line_without_ornaments_leaves_the_wheel_alone() {
        let plain = InstrumentProfile::default();
        let line = perform(&tones(":0.5 5 6 5 4 3:2"), &playing(&plain)).unwrap();
        assert!(line.wheel.is_empty() && line.written.is_empty() && line.remarks.is_empty());
        assert_eq!(line.notes.len(), 5);
        assert!(line.notes.iter().all(|(_, how)| how.is_none()));
        // The same notes as the phrase gives by itself, velocities and all.
        let key = Scale::parse("A minor").unwrap();
        let phrase = Phrase::parse(":0.5 5 6 5 4 3:2", Some(&key), 4, four()).unwrap();
        let notes: Vec<Note> = line.notes.iter().map(|(note, _)| *note).collect();
        assert_eq!(notes, phrase.notes(1.0, &Dynamics { velocity: 90, accent: 0 }, four()));
    }

    #[test]
    fn a_bend_rises_to_its_pitch_and_a_release_lets_it_down() {
        let plain = InstrumentProfile::default();
        // The fourth degree of A minor (D) bent to the fifth (E): two semitones, over a third of the note.
        let held = perform(&tones("4:3!bend=5 1:1"), &playing(&plain)).unwrap();
        assert_eq!(bent(&held, &[0.0, 0.5, 1.0, 2.9]), [0.0, 1.0, 2.0, 2.0]);
        assert_eq!(bent(&held, &[3.0, 3.9]), [0.0, 0.0], "the next note is struck in tune");
        assert_eq!(pitches(&held), [(74, 0.0, 3.0), (69, 3.0, 1.0)]);
        assert_eq!(how(&held, "bend"), [("a pitch bend".to_string(), 1)]);

        let released = perform(&tones("4:3!bend=5!release"), &playing(&plain)).unwrap();
        assert_eq!(bent(&released, &[1.0, 2.0, 2.5, 3.0]), [2.0, 2.0, 1.0, 0.0]);
        // Struck already bent, then let down through the middle of the note.
        let prebent = perform(&tones("4:3!prebend=5"), &playing(&plain)).unwrap();
        assert_eq!(bent(&prebent, &[0.0, 0.99, 1.5, 2.0, 3.0]), [2.0, 2.0, 1.0, 0.0, 0.0]);
        // A semitone bend is half the wheel.
        let half = perform(&tones("2:3!bend=3"), &playing(&plain)).unwrap();
        assert_eq!(automation_value(&half.wheel, 2.0), 0.5);
    }

    #[test]
    fn the_wheel_is_scaled_by_the_instruments_bend_range_and_says_when_it_runs_out() {
        // A wide wheel: a bend of two semitones is a sixth of it.
        let wide = InstrumentProfile { bend_range: Some(12.0), ..Default::default() };
        let bend = perform(&tones("4:3!bend=5"), &playing(&wide)).unwrap();
        assert!((automation_value(&bend.wheel, 2.0) - 2.0 / 12.0).abs() < 1e-12);
        assert!(bend.remarks.is_empty());
        // A minor third on a wheel of two semitones goes as far as it can, and says so.
        let plain = InstrumentProfile::default();
        let far = perform(&tones("1:3!bend=3"), &playing(&plain)).unwrap();
        assert_eq!(automation_value(&far.wheel, 2.0), 1.0);
        assert!(far.remarks[0].starts_with("A bend of 3 semitones was asked for, and this instrument bends 2"));
    }

    #[test]
    fn vibrato_swings_about_the_note_and_ends_with_it() {
        let plain = InstrumentProfile::default();
        let line = perform(&tones("5:4!vib 1:2"), &playing(&plain)).unwrap();
        let during: Vec<f64> = (0..400).map(|step| automation_value(&line.wheel, step as f64 / 100.0) * 2.0).collect();
        let turns: Vec<f64> = line.wheel.iter().map(|point| point.value * 2.0).collect();
        let (lowest, highest) = turns.iter().fold((0.0f64, 0.0f64), |(low, high), v| (low.min(*v), high.max(*v)));
        assert!((highest - VIBRATO_DEPTH).abs() < 1e-9 && (lowest + VIBRATO_DEPTH).abs() < 1e-9, "{lowest}..{highest}");
        assert_eq!(during[0], 0.0, "it comes in after the note has begun");
        // At 120 BPM a cycle of 5.5 Hz is 4/11 of a beat: about nine of them fit.
        let crossings = during.windows(2).filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0).count();
        assert!((8..=11).contains(&crossings), "{crossings} cycles");
        assert_eq!(bent(&line, &[4.0, 5.0]), [0.0, 0.0], "the note after it is steady");
        // A wider one, on top of a bend: it swings about the bent pitch.
        let both = perform(&tones("4:4!bend=5!vib=0.5"), &playing(&plain)).unwrap();
        let late: Vec<f64> = (150..400).map(|step| automation_value(&both.wheel, step as f64 / 100.0) * 2.0).collect();
        assert!(late.iter().all(|value| (1.5 - 1e-9..=2.0 + 1e-9).contains(value)), "clipped at the top of the wheel");
        assert!(how(&both, "vib") == [("a pitch-bend vibrato".to_string(), 1)]);
    }

    #[test]
    fn a_hammer_on_holds_the_note_before_and_steps_the_wheel() {
        let plain = InstrumentProfile::default();
        // E, F hammered on, E pulled off: one note three halves long, the wheel a semitone up in the middle.
        let run = perform(&tones(":0.5 5 6!hammer 5!pull 3"), &playing(&plain)).unwrap();
        assert_eq!(pitches(&run), [(76, 0.0, 1.5), (72, 1.5, 0.5)]);
        assert_eq!(bent(&run, &[0.0, 0.49, 0.5, 0.99, 1.0, 1.49, 1.5]), [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0]);
        assert_eq!(how(&run, "hammer"), [("a pitch-bend step on the note before, held through".to_string(), 2)]);
        // Further than the wheel reaches: struck as an ordinary note, and said.
        let leap = perform(&tones(":0.5 1 5!hammer"), &playing(&plain)).unwrap();
        assert_eq!(pitches(&leap), [(69, 0.0, 0.5), (76, 0.5, 0.5)]);
        assert!(leap.wheel.is_empty());
        assert!(leap.remarks[0].contains("1 hammered note(s) are further from the note before"), "{:?}", leap.remarks);

        // An instrument that slurs is given an overlap and no bend.
        let wind = InstrumentProfile { slurs: true, ..Default::default() };
        let slurred = perform(&tones(":0.5 5 6!hammer 1!legato"), &playing(&wind)).unwrap();
        assert_eq!(pitches(&slurred), [(76, 0.0, 0.54), (77, 0.5, 0.54), (69, 1.0, 0.5)]);
        assert!(slurred.wheel.is_empty());
    }

    #[test]
    fn a_slide_comes_from_the_note_before_or_from_below() {
        let plain = InstrumentProfile::default();
        // After a note within reach: that note is held and the wheel glides.
        let joined = perform(&tones("5:1 6:1!slide"), &playing(&plain)).unwrap();
        assert_eq!(pitches(&joined), [(76, 0.0, 2.0)]);
        assert_eq!(bent(&joined, &[0.99, 1.0, 1.125, 1.25, 1.9]), [0.0, 0.0, 0.5, 1.0, 1.0]);
        // With nothing before it: struck, from two semitones under.
        let scooped = perform(&tones("r 5:1!slide"), &playing(&plain)).unwrap();
        assert_eq!(pitches(&scooped), [(76, 1.0, 1.0)]);
        assert_eq!(bent(&scooped, &[1.0, 1.125, 1.25, 1.9]), [-2.0, -1.0, 0.0, 0.0]);
        // A scoop is from below whatever came before it, and the note is struck.
        let scoop = perform(&tones("4:1 5:1!scoop"), &playing(&plain)).unwrap();
        assert_eq!(pitches(&scoop), [(74, 0.0, 1.0), (76, 1.0, 1.0)]);
        assert_eq!(bent(&scoop, &[0.5, 1.0, 1.125, 1.25]), [0.0, -2.0, -1.0, 0.0]);
        assert_eq!(how(&scoop, "scoop"), [("a pitch bend into the note, from below".to_string(), 1)]);
        // From a pitch that is named.
        let named = perform(&tones("5:1!slide=4"), &playing(&plain)).unwrap();
        assert_eq!(bent(&named, &[0.0, 0.25]), [-2.0, 0.0]);
        // A fall drops away at the end, and the wheel is let go afterwards.
        let fall = perform(&tones("5:2!fall"), &playing(&plain)).unwrap();
        assert_eq!(bent(&fall, &[1.5, 1.75, 2.0, 2.2, 2.25]), [0.0, -1.0, -2.0, -2.0, 0.0]);
    }

    /// A guitar with keyswitches for the three ornaments it has samples of.
    fn guitar() -> InstrumentProfile {
        InstrumentProfile {
            articulations: vec![
                Articulation::key("open", 24).as_default(),
                Articulation::key("palm_mute", 26),
                Articulation::key("slide", 27).lasting(Lasts::Once),
                Articulation::key("glide", 28).lasting(Lasts::Joins),
                Articulation::key("hammer", 29).lasting(Lasts::Joins),
            ],
            range: Some((36, 88)),
            ..InstrumentProfile::default()
        }
    }

    #[test]
    fn an_instrument_with_its_own_articulation_is_asked_for_that() {
        let guitar = guitar();
        let line =
            perform(&tones(":0.5 5 6!hammer 5!pull r 3!slide 4!slide 1!palm_mute 4:2!bend=5"), &playing(&guitar));
        let line = line.unwrap();
        let asked: Vec<Option<&str>> = line.notes.iter().map(|(_, how)| how.as_deref()).collect();
        assert_eq!(
            asked,
            [None, Some("hammer"), Some("hammer"), Some("slide"), Some("glide"), Some("palm_mute"), None]
        );
        assert_eq!(line.notes.len(), 7, "every note is written: the keyswitches do the joining");
        assert_eq!(how(&line, "hammer"), [("the instrument's 'hammer' keyswitch".to_string(), 2)]);
        assert_eq!(
            how(&line, "slide"),
            [
                ("the instrument's 'glide' keyswitch".to_string(), 1),
                ("the instrument's 'slide' keyswitch".to_string(), 1)
            ]
        );
        // Only the bend, which it has no articulation for, moves the wheel.
        assert_eq!(bent(&line, &[0.0, 2.9, 3.5, 5.0]), [0.0, 0.0, 0.0, 2.0]);

        // A scoop is the instrument's slide into a note, also where a note comes right before.
        let scooped = perform(&tones(":0.5 5 6!scoop 3!slide"), &playing(&guitar)).unwrap();
        let asked: Vec<Option<&str>> = scooped.notes.iter().map(|(_, how)| how.as_deref()).collect();
        assert_eq!(asked, [None, Some("slide"), Some("glide")]);
        assert!(scooped.wheel.is_empty());

        // Every note can be given one articulation, which a mark overrides.
        let muted = Playing { articulation: Some("palm_mute"), ..playing(&guitar) };
        let line = perform(&tones("1 1 5!open"), &muted).unwrap();
        let asked: Vec<Option<&str>> = line.notes.iter().map(|(_, how)| how.as_deref()).collect();
        assert_eq!(asked, [Some("palm_mute"), Some("palm_mute"), Some("open")]);
        // One it does not have is refused, with the ones it has.
        let error = perform(&tones("1!tremolo"), &playing(&guitar)).unwrap_err();
        assert!(
            error.message().contains("'tremolo' is not an articulation of this instrument; it has open, palm_mute")
        );
    }

    #[test]
    fn the_gate_shortens_what_sounds_and_ornaments_follow_it() {
        let plain = InstrumentProfile::default();
        let short = Playing { gate: 0.5, ..playing(&plain) };
        let line = perform(&tones("4:2!bend=5!release 1:2!fall"), &short).unwrap();
        assert_eq!(pitches(&line), [(74, 0.0, 1.0), (69, 2.0, 1.0)]);
        assert_eq!(bent(&line, &[1.0 / 3.0, 2.0 / 3.0, 1.0]), [2.0, 2.0, 0.0], "let down by the time the note stops");
        assert_eq!(bent(&line, &[2.6, 3.0]), [0.0, -2.0]);
    }
}
