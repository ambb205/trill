//! Asking an instrument for its articulations: turning "this note is
//! palm-muted" into the keys and velocities that make it so.
//!
//! Sampled instruments choose how a note is played by other notes: a key
//! under the range that sounds nothing and is pressed just before
//! ([`InstrumentProfile`] says which, and how long its choice lasts), and on
//! some by how hard the note is hit. A part written without knowing this
//! has silent low notes placed by hand, or none, and plays whatever the
//! part before it left chosen. [`articulate`] writes the presses:
//!
//! * A press goes [`SWITCH_LEAD`] beats before its note and is held for
//!   [`SWITCH_HOLD`], so that no two are down together.
//! * A part begins by choosing its articulation, the instrument's default
//!   if nothing was asked for: an earlier part may have left another.
//! * A choice that stays is pressed once and again only when it changes. A
//!   choice for one note is pressed before each such note. A joining one
//!   (hammer-ons, a slide along the string) is pressed, softly, before the
//!   first note of a run, and each note after it is made to start while the
//!   one before still sounds.
//! * Where velocity decides how a note is played, a note's velocity is
//!   fitted into the range that plays what was asked for.

use crate::error::{Error, Result, list};
use crate::expression::MIN_LENGTH;
use std::collections::BTreeMap;
use trill_core::{Articulation, InstrumentProfile, Lasts, Note};

/// How long before its note a keyswitch is pressed, in beats.
pub const SWITCH_LEAD: f64 = 0.03;
/// How long a keyswitch is held, in beats.
pub const SWITCH_HOLD: f64 = 0.01;
/// How far a note runs into the one joined to it, in beats.
pub const JOIN_OVERLAP: f64 = 0.04;
/// How hard a keyswitch is pressed.
const SWITCH_VELOCITY: u8 = 100;
/// How hard a joining keyswitch is pressed: softly, since some instruments
/// take a hard press of it to mean a move along the neck as well.
const JOIN_VELOCITY: u8 = 40;

/// A part with its keyswitches written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Articulated {
    /// The notes to write, the presses among them, by start.
    pub notes: Vec<Note>,
    /// How many presses were written, by articulation.
    pub presses: BTreeMap<String, usize>,
    /// How many notes had their velocity moved into the range that the
    /// instrument's default articulation needs: notes that would have been
    /// played another way than was meant.
    pub fitted: usize,
    /// The velocities the default articulation is played by, if velocity
    /// decides.
    pub default_range: Option<(u8, u8)>,
}

/// Writes the keyswitches for `notes`, each given with the name of the
/// articulation it asks for, or `None` for the instrument's default.
/// `room` is how many beats lie before the part's beat 0 in its clip: a
/// press that would fall before the clip begins moves its note late instead.
pub fn articulate(profile: &InstrumentProfile, notes: &[(Note, Option<String>)], room: f64) -> Result<Articulated> {
    let default = profile.default_articulation();
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by(|a, b| {
        notes[*a].0.start.total_cmp(&notes[*b].0.start).then(notes[*a].0.pitch.cmp(&notes[*b].0.pitch))
    });
    let named = |index: usize| -> Result<Option<&Articulation>> {
        match &notes[index].1 {
            None => Ok(None),
            Some(name) => profile.articulation(name).map(Some).ok_or_else(|| unknown(profile, name)),
        }
    };

    let mut out = Articulated { default_range: default.and_then(|a| a.velocity), ..Articulated::default() };
    // The key of the choice that stays, as far as this part has made one.
    let mut chosen: Option<u8> = None;
    // The last note of music written, which a joined note runs on from.
    let mut previous: Option<usize> = None;
    // Notes that begin together move together.
    let mut moved: Option<(f64, f64)> = None;
    let mut once: Option<(u8, f64)> = None;

    for (position, index) in order.iter().copied().enumerate() {
        let asked = named(index)?;
        let mut note = notes[index].0;
        let Some(articulation) = asked.or(default) else {
            // An instrument without articulations plays the note as it is.
            previous = Some(out.notes.len());
            out.notes.push(note);
            continue;
        };
        let joined = articulation.lasts == Lasts::Joins && previous.is_some();
        // The choice the note is played under: its own if that stays.
        let latch = if articulation.lasts == Lasts::Stays { Some(articulation) } else { default };

        // What goes down before the note: (key, beats before, velocity, name).
        let mut presses: Vec<(u8, f64, u8, &str)> = Vec::new();
        if joined {
            if let Some(before) = previous.and_then(|index| out.notes.get_mut(index)) {
                before.length = before.length.max(note.start + JOIN_OVERLAP - before.start);
            }
        } else {
            let once_key = articulation.key.filter(|_| articulation.lasts == Lasts::Once);
            if let Some(latch) = latch
                && let Some(key) = latch.key
                && chosen != Some(key)
            {
                // Before a press for one note, so that there is something to go back to.
                let lead = if once_key.is_some() { 2.0 * SWITCH_LEAD } else { SWITCH_LEAD };
                presses.push((key, lead, SWITCH_VELOCITY, &latch.name));
                chosen = Some(key);
            }
            if let Some(key) = once_key
                && once != Some((key, notes[index].0.start))
            {
                presses.push((key, SWITCH_LEAD, SWITCH_VELOCITY, &articulation.name));
                once = Some((key, notes[index].0.start));
            }
            // A run of joined notes begins here if the next note is joined to this one.
            let next = order.get(position + 1).map(|next| named(*next)).transpose()?.flatten();
            if let Some(next) = next
                && next.lasts == Lasts::Joins
                && let Some(key) = next.key
            {
                presses.push((key, SWITCH_LEAD / 2.0, JOIN_VELOCITY, &next.name));
            }
        }

        // How hard the note is hit is part of how it is played.
        let range =
            if articulation.lasts == Lasts::Stays { articulation.velocity } else { latch.and_then(|a| a.velocity) };
        if let Some((low, high)) = range {
            let chose_it = asked.is_some() && articulation.lasts == Lasts::Stays && !articulation.default;
            if chose_it {
                // Asked for by name: the whole of the dynamics, within what plays it.
                let span = (high - low) as f64;
                note.velocity = (low as f64 + span * (note.velocity.max(1) - 1) as f64 / 126.0).round() as u8;
            } else if !(low..=high).contains(&note.velocity) {
                note.velocity = note.velocity.clamp(low, high);
                out.fitted += 1;
            }
        }

        // No press may fall before the clip begins: the note waits instead.
        let original = notes[index].0.start;
        let lead = presses.iter().map(|(_, lead, ..)| *lead).fold(0.0, f64::max);
        let late = match moved {
            Some((start, late)) if start == original => late,
            _ => (lead - (original + room)).max(0.0),
        };
        if late > 0.0 {
            note.start += late;
            note.length = (note.length - late).max(MIN_LENGTH);
            moved = Some((original, late));
        }
        for (key, lead, velocity, name) in presses {
            out.notes.push(Note { pitch: key, start: note.start - lead, length: SWITCH_HOLD, velocity });
            *out.presses.entry(name.to_string()).or_default() += 1;
        }
        previous = Some(out.notes.len());
        out.notes.push(note);
    }
    out.notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    Ok(out)
}

/// The error for an articulation the instrument does not have.
pub fn unknown(profile: &InstrumentProfile, name: &str) -> Error {
    match profile.articulation_names().as_slice() {
        [] => Error::new(format!(
            "'{name}' is not an ornament, and this track's instrument has no articulations to ask for by name; \
             set_instrument_profile says which keys of it are keyswitches"
        )),
        names => Error::new(format!("'{name}' is not an articulation of this instrument; it has {}", list(names))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guitar laid out as the sampled ones are: under 64 a note is muted.
    fn guitar() -> InstrumentProfile {
        InstrumentProfile {
            articulations: vec![
                Articulation::key("open", 24).hit(64, 126).as_default(),
                Articulation::key("muted", 24).hit(32, 63),
                Articulation::key("palm_mute", 26),
                Articulation::key("slide", 27).lasting(Lasts::Once),
                Articulation::key("hammer", 29).lasting(Lasts::Joins),
            ],
            range: Some((36, 84)),
            ..InstrumentProfile::default()
        }
    }

    fn note(pitch: u8, start: f64, velocity: u8) -> Note {
        Note { pitch, start, length: 0.5, velocity }
    }

    fn part(notes: &[(u8, f64, u8, Option<&str>)]) -> Vec<(Note, Option<String>)> {
        notes
            .iter()
            .map(|(pitch, start, velocity, how)| (note(*pitch, *start, *velocity), how.map(str::to_string)))
            .collect()
    }

    /// `"24@0.97 40@1"`: every key with its start, rounded.
    fn written(articulated: &Articulated) -> String {
        let round = |value: f64| (value * 1e4).round() / 1e4;
        articulated
            .notes
            .iter()
            .map(|note| format!("{}@{}", note.pitch, round(note.start)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_part_begins_by_choosing_how_it_is_played() {
        // Nothing asked for: the default is pressed once, before the first note.
        let plain = articulate(&guitar(), &part(&[(40, 1.0, 90, None), (43, 1.5, 90, None), (45, 2.0, 90, None)]), 0.0);
        let plain = plain.unwrap();
        assert_eq!(written(&plain), "24@0.97 40@1 43@1.5 45@2");
        assert_eq!(plain.presses, [("open".to_string(), 1)].into());
        let press = plain.notes[0];
        assert_eq!((press.length, press.velocity), (SWITCH_HOLD, 100));

        // An articulation that stays is pressed when it changes, and not again.
        let riff = part(&[
            (40, 1.0, 90, Some("palm_mute")),
            (40, 1.5, 90, Some("palm_mute")),
            (47, 2.0, 90, None),
            (40, 2.5, 90, Some("Palm Mute")),
        ]);
        let riff = articulate(&guitar(), &riff, 0.0).unwrap();
        assert_eq!(written(&riff), "26@0.97 40@1 40@1.5 24@1.97 47@2 26@2.47 40@2.5");
        assert_eq!(riff.presses, [("open".to_string(), 1), ("palm_mute".to_string(), 2)].into());

        // A chord is one press.
        let chord = part(&[
            (40, 4.0, 90, Some("palm_mute")),
            (47, 4.0, 90, Some("palm_mute")),
            (52, 4.0, 90, Some("palm_mute")),
        ]);
        assert_eq!(written(&articulate(&guitar(), &chord, 0.0).unwrap()), "26@3.97 40@4 47@4 52@4");

        // An instrument without articulations is left alone.
        let notes = part(&[(60, 0.0, 30, None), (64, 1.0, 90, None)]);
        let plain = articulate(&InstrumentProfile::default(), &notes, 0.0).unwrap();
        assert_eq!((written(&plain), plain.fitted, plain.presses.len()), ("60@0 64@1".to_string(), 0, 0));
    }

    #[test]
    fn a_press_never_falls_before_the_clip_begins() {
        // The first note waits for its switch, and a chord waits together.
        let opening = part(&[(40, 0.0, 90, None), (47, 0.0, 90, None), (43, 1.0, 90, None)]);
        let opening = articulate(&guitar(), &opening, 0.0).unwrap();
        assert_eq!(written(&opening), "24@0 40@0.03 47@0.03 43@1");
        let first = opening.notes[1];
        assert!((first.start + first.length - 0.5).abs() < 1e-9, "it still ends where it would have");
        // With room before the part (it is written into the middle of a clip) nothing moves.
        let inside = articulate(&guitar(), &part(&[(40, 0.0, 90, None)]), 8.0).unwrap();
        assert_eq!(written(&inside), "24@-0.03 40@0");
    }

    #[test]
    fn a_choice_for_one_note_is_pressed_each_time_and_a_joining_one_before_the_run() {
        // Slides: the default first, so that there is something to go back to.
        let slides = part(&[(45, 1.0, 90, Some("slide")), (47, 2.0, 90, None), (50, 3.0, 90, Some("slide"))]);
        let slides = articulate(&guitar(), &slides, 0.0).unwrap();
        assert_eq!(written(&slides), "24@0.94 27@0.97 45@1 47@2 27@2.97 50@3");
        assert_eq!(slides.presses, [("open".to_string(), 1), ("slide".to_string(), 2)].into());

        // Hammer-ons: one soft press before the first note, and each note runs into the next.
        let run = part(&[
            (40, 1.0, 90, None),
            (42, 1.5, 90, Some("hammer")),
            (43, 2.0, 90, Some("hammer")),
            (40, 3.0, 90, None),
        ]);
        let run = articulate(&guitar(), &run, 0.0).unwrap();
        assert_eq!(written(&run), "24@0.97 29@0.985 40@1 42@1.5 43@2 40@3");
        let hammer = run.notes.iter().find(|note| note.pitch == 29).unwrap();
        assert_eq!(hammer.velocity, 40, "pressed softly");
        let lengths: Vec<f64> = run.notes.iter().filter(|note| note.pitch >= 36).map(|note| note.length).collect();
        assert!((lengths[0] - 0.54).abs() < 1e-9 && (lengths[1] - 0.54).abs() < 1e-9, "{lengths:?}");
        assert_eq!(lengths[2..], [0.5, 0.5], "the last of the run and the note after it are as written");
        // A joined note with nothing before it is an ordinary one.
        let alone = articulate(&guitar(), &part(&[(40, 1.0, 90, Some("hammer"))]), 0.0).unwrap();
        assert_eq!(written(&alone), "24@0.97 40@1");
    }

    #[test]
    fn velocity_is_fitted_to_what_plays_the_articulation() {
        // Unasked, a soft note would be muted: it is raised, and counted.
        let soft =
            part(&[(40, 1.0, 40, None), (40, 2.0, 100, None), (40, 3.0, 127, None), (40, 4.0, 20, Some("palm_mute"))]);
        let soft = articulate(&guitar(), &soft, 0.0).unwrap();
        let velocities: Vec<u8> = soft.notes.iter().filter(|note| note.pitch == 40).map(|note| note.velocity).collect();
        assert_eq!(velocities, [64, 100, 126, 20], "palm mutes may be as soft as they like");
        assert_eq!((soft.fitted, soft.default_range), (2, Some((64, 126))));

        // Asked for by name, the dynamics are kept within the range that plays it.
        let muted = part(&[(40, 1.0, 1, Some("muted")), (40, 2.0, 64, Some("muted")), (40, 3.0, 127, Some("muted"))]);
        let muted = articulate(&guitar(), &muted, 0.0).unwrap();
        let velocities: Vec<u8> =
            muted.notes.iter().filter(|note| note.pitch == 40).map(|note| note.velocity).collect();
        assert_eq!(velocities, [32, 48, 63]);
        assert_eq!(muted.fitted, 0, "nothing was changed that was not asked for");
        assert_eq!(written(&muted), "24@0.97 40@1 40@2 40@3", "muted notes are played under the open switch");
    }

    #[test]
    fn an_articulation_the_instrument_lacks_is_named_with_the_ones_it_has() {
        let error = articulate(&guitar(), &part(&[(40, 1.0, 90, Some("tremolo"))]), 0.0).unwrap_err();
        assert_eq!(
            error.message(),
            "'tremolo' is not an articulation of this instrument; it has open, muted, palm_mute, slide, hammer"
        );
        let error = articulate(&InstrumentProfile::default(), &part(&[(40, 1.0, 90, Some("palm_mute"))]), 0.0);
        assert!(error.unwrap_err().message().contains("has no articulations to ask for by name"));
    }
}
