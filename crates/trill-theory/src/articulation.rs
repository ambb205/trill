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
//! * A key that plays a noise instead of a note (a muted strum, a pick
//!   scrape) is not played any way at all: it is left as it is.
//!
//! The instrument is in one state at a time, so the presses of a clip are
//! one thing: notes added to a clip that has some change what the notes
//! after them are played as. [`read_back`] says how each note of a clip is
//! played by the presses it holds, so that old notes and new can be given
//! to [`articulate_asked`] together and the clip's presses written anew.

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

/// A note with how it is to be played.
#[derive(Clone, Debug, PartialEq)]
pub struct Asked {
    pub note: Note,
    /// The articulation, by the name the instrument's profile gives it;
    /// `None` for the way the instrument plays a note nothing is said of.
    pub how: Option<String>,
    /// The note is already as the instrument is to be given it: its key
    /// and its velocity were fitted when it was written, and are kept. A
    /// note that [`read_back`] found in a clip is.
    pub settled: bool,
}

impl Asked {
    /// A note to be played as `how` says, its velocity fitted to that.
    pub fn new(note: Note, how: Option<String>) -> Self {
        Self { note, how, settled: false }
    }

    /// A note that is to stay exactly as it is, played as `how` says.
    pub fn settled(note: Note, how: Option<String>) -> Self {
        Self { note, how, settled: true }
    }
}

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
    let asked: Vec<Asked> = notes.iter().map(|(note, how)| Asked::new(*note, how.clone())).collect();
    articulate_asked(profile, &asked, room)
}

/// The keys an instrument is given for notes that are to be *heard* at
/// their pitches: an articulation that sounds away from its key (a
/// harmonic rings an octave above the fretted note) is played that far the
/// other way. Returns, for each such articulation that was asked for, how
/// far it sounds from its key and how many notes were moved. Notes that
/// are settled are keys already.
pub fn keys_for(profile: &InstrumentProfile, asked: &mut [Asked]) -> Result<BTreeMap<String, (i8, usize)>> {
    let mut moved: BTreeMap<String, (i8, usize)> = BTreeMap::new();
    for entry in asked.iter_mut().filter(|entry| !entry.settled) {
        let Some(articulation) = entry.how.as_deref().and_then(|name| profile.articulation(name)) else { continue };
        if articulation.sounds == 0 {
            continue;
        }
        let key = i32::from(entry.note.pitch) - i32::from(articulation.sounds);
        if !(0..=127).contains(&key) {
            return Err(Error::new(format!(
                "a note cannot be heard at {} as '{}': that sounds {} semitones above its key, and there is no key \
                 that far down",
                trill_core::pitch::pitch_name(entry.note.pitch),
                articulation.name,
                articulation.sounds
            )));
        }
        entry.note.pitch = key as u8;
        moved.entry(articulation.name.clone()).or_insert((articulation.sounds, 0)).1 += 1;
    }
    Ok(moved)
}

/// [`articulate`] for notes that say more of themselves: some as they are
/// to stay ([`Asked::settled`]), which is how the notes a clip already
/// holds are written again beside new ones.
pub fn articulate_asked(profile: &InstrumentProfile, notes: &[Asked], room: f64) -> Result<Articulated> {
    let default = profile.default_articulation();
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by(|a, b| {
        notes[*a].note.start.total_cmp(&notes[*b].note.start).then(notes[*a].note.pitch.cmp(&notes[*b].note.pitch))
    });
    let named = |index: usize| -> Result<Option<&Articulation>> {
        match &notes[index].how {
            None => Ok(None),
            Some(name) => profile.articulation(name).map(Some).ok_or_else(|| unknown(profile, name)),
        }
    };
    // A key that plays a noise is not a note of music: nothing is pressed
    // for it and it joins nothing.
    let noise = |index: usize| profile.sound_name(notes[index].note.pitch).is_some();

    let mut out = Articulated { default_range: default.and_then(|a| a.velocity), ..Articulated::default() };
    // The key of the choice that stays, as far as this part has made one.
    let mut chosen: Option<u8> = None;
    // The last note of music written, which a joined note runs on from.
    let mut previous: Option<usize> = None;
    // Notes that begin together move together, and none starts before a
    // note that was moved late to make room for its press.
    let mut moved: Option<(f64, f64)> = None;
    let mut earliest = f64::NEG_INFINITY;
    let mut once: Option<(u8, f64)> = None;
    let music: Vec<usize> = order.iter().copied().filter(|index| !noise(*index)).collect();

    for index in order.iter().copied() {
        let mut note = notes[index].note;
        if noise(index) {
            out.notes.push(note);
            continue;
        }
        let position = music.iter().position(|other| *other == index).expect("a note of music");
        let asked = named(index)?;
        let settled = notes[index].settled;
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
                && once != Some((key, notes[index].note.start))
            {
                presses.push((key, SWITCH_LEAD, SWITCH_VELOCITY, &articulation.name));
                once = Some((key, notes[index].note.start));
            }
            // A run of joined notes begins here if the next note is joined to this one.
            let next = music.get(position + 1).map(|next| named(*next)).transpose()?.flatten();
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
        if let (Some((low, high)), false) = (range, settled) {
            let chose_it = asked.is_some() && articulation.lasts == Lasts::Stays && !articulation.default;
            if chose_it {
                // Asked for by name: the whole of the dynamics, within what plays it.
                let span = u32::from(high - low);
                note.velocity = low + (span * u32::from(note.velocity.max(1) - 1) / 126) as u8;
            } else if !(low..=high).contains(&note.velocity) {
                note.velocity = note.velocity.clamp(low, high);
                out.fitted += 1;
            }
        }

        // No press may fall before the clip begins: the note waits instead.
        let original = notes[index].note.start;
        let lead = presses.iter().map(|(_, lead, ..)| *lead).fold(0.0, f64::max);
        let late = match moved {
            Some((start, late)) if start == original => late,
            _ => (lead - (original + room)).max(0.0),
        };
        if late > 0.0 {
            note.start += late;
            note.length = (note.length - late).max(MIN_LENGTH);
            moved = Some((original, late));
            earliest = earliest.max(note.start);
        } else if note.start < earliest {
            // The strings of a strum that begins the clip do not come in ahead of its first.
            note.length = (note.length - (earliest - note.start)).max(MIN_LENGTH);
            note.start = earliest;
        }
        for (key, lead, velocity, name) in presses {
            // Never before the clip begins, by rounding either: a note a hair before beat 0 is refused.
            let start = (note.start - lead).max(0.0 - room);
            out.notes.push(Note { pitch: key, start, length: SWITCH_HOLD, velocity });
            *out.presses.entry(name.to_string()).or_default() += 1;
        }
        previous = Some(out.notes.len());
        out.notes.push(note);
    }
    out.notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    Ok(out)
}

/// How each note of a clip is played, by the presses the clip holds: the
/// notes of music and the noises, each as it is, with the articulation it
/// is played by where that is not the instrument's default. The presses
/// themselves are left out; [`articulate_asked`] writes them again.
///
/// It reads what [`articulate`] writes: a key that stays chosen until
/// another is, a key for the next note, and a key that joins the notes of
/// the run that follows it, each of which starts while the one before
/// still sounds.
pub fn read_back(profile: &InstrumentProfile, notes: &[Note]) -> Vec<Asked> {
    let mut order: Vec<&Note> = notes.iter().collect();
    order.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    let default = profile.default_articulation();
    let mut out = Vec::with_capacity(notes.len());
    // The key of the choice that stays.
    let mut latched: Option<u8> = None;
    // A choice for the next note, and once it has been taken, the start of
    // the note that took it: the notes of a chord take it together.
    let mut once: Option<&Articulation> = None;
    let mut took: Option<(&Articulation, f64)> = None;
    // A joining key that was pressed, and whether its run has begun.
    let mut joining: Option<(&Articulation, bool)> = None;
    let mut before: Option<&Note> = None;
    for note in order {
        if let Some(switch) = profile.articulations.iter().find(|a| a.key == Some(note.pitch)) {
            match switch.lasts {
                Lasts::Stays => latched = Some(note.pitch),
                Lasts::Once => once = Some(switch),
                Lasts::Joins => joining = Some((switch, false)),
            }
            continue;
        }
        if profile.sound_name(note.pitch).is_some() {
            out.push(Asked::settled(*note, None));
            continue;
        }
        let overlaps = before.is_some_and(|before| note.start < before.end() - 1e-9);
        let how = if let (Some((join, true)), true) = (joining, overlaps) {
            Some(join)
        } else if let Some(slide) = once.take() {
            took = Some((slide, note.start));
            Some(slide)
        } else if let Some((slide, _)) = took.filter(|(_, start)| (note.start - start).abs() < 1e-9) {
            Some(slide)
        } else {
            // By the choice that stays and how hard the note is hit.
            let key = latched.or(default.and_then(|default| default.key));
            let among = || profile.articulations.iter().filter(|a| a.lasts == Lasts::Stays && a.key == key);
            let hit = |a: &&Articulation| a.velocity.is_some_and(|(low, high)| (low..=high).contains(&note.velocity));
            among().find(hit).or_else(|| among().find(|a| a.velocity.is_none()))
        };
        // A run goes on while each note starts in the one before it.
        joining = match joining {
            Some((join, false)) => Some((join, true)),
            Some((join, true)) if overlaps => Some((join, true)),
            _ => None,
        };
        let name = how.filter(|articulation| !articulation.default).map(|articulation| articulation.name.clone());
        out.push(Asked::settled(*note, name));
        before = Some(note);
    }
    out
}

/// Whether the presses in `notes` are the ones [`articulate`] writes for
/// the notes among them: a clip that these tools wrote, and can write
/// again. Presses that someone placed by hand, their own way, are not.
pub fn is_our_writing(profile: &InstrumentProfile, notes: &[Note]) -> bool {
    let Ok(again) = articulate_asked(profile, &read_back(profile, notes), 0.0) else { return false };
    let sorted = |notes: &[Note]| {
        let mut notes: Vec<Note> = notes.to_vec();
        notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
        notes
    };
    let (was, is) = (sorted(notes), sorted(&again.notes));
    was.len() == is.len()
        && was.iter().zip(&is).all(|(was, is)| {
            was.pitch == is.pitch
                && was.velocity == is.velocity
                && (was.start - is.start).abs() < 1e-3
                && (was.length - is.length).abs() < 1e-3
        })
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
        assert_eq!(velocities, [32, 47, 63]);
        assert_eq!(muted.fitted, 0, "nothing was changed that was not asked for");
        assert_eq!(written(&muted), "24@0.97 40@1 40@2 40@3", "muted notes are played under the open switch");
    }

    #[test]
    fn a_strum_that_begins_the_clip_keeps_its_order_and_a_noise_is_left_alone() {
        // Three strings a hair apart from beat 0: the first waits for its switch, and the
        // others do not come in ahead of it.
        let strum = part(&[(40, 0.0, 90, None), (47, 0.015, 90, None), (52, 0.03, 90, None), (55, 0.045, 90, None)]);
        let strum = articulate(&guitar(), &strum, 0.0).unwrap();
        assert_eq!(written(&strum), "24@0 40@0.03 47@0.03 52@0.03 55@0.045");

        // A key that plays a noise: no switch is pressed for it, it is not fitted, and it
        // neither joins a run nor breaks one.
        let mut profile = guitar();
        profile.sounds.insert("chk".to_string(), 92);
        let riff = part(&[
            (92, 1.0, 40, None),
            (40, 2.0, 90, None),
            (92, 2.25, 30, None),
            (42, 2.5, 90, Some("hammer")),
            (92, 3.0, 20, None),
        ]);
        let riff = articulate(&profile, &riff, 0.0).unwrap();
        assert_eq!(written(&riff), "92@1 24@1.97 29@1.985 40@2 92@2.25 42@2.5 92@3");
        let noises: Vec<u8> = riff.notes.iter().filter(|note| note.pitch == 92).map(|note| note.velocity).collect();
        assert_eq!((noises, riff.fitted), (vec![40, 30, 20], 0));
        let picked = riff.notes.iter().find(|note| note.pitch == 40).unwrap();
        assert!((picked.length - 0.54).abs() < 1e-9, "the hammered note is joined to the note, not to the noise");
    }

    #[test]
    fn an_articulation_that_sounds_away_from_its_key_is_given_the_key_that_is_heard_right() {
        let mut profile = guitar();
        profile.articulations.push(Articulation::key("harmonic", 25).sounding(12));
        let mut asked = vec![
            Asked::new(note(64, 1.0, 90), Some("harmonic".to_string())),
            Asked::new(note(64, 2.0, 90), None),
            Asked::settled(note(52, 3.0, 90), Some("harmonic".to_string())),
        ];
        let moved = keys_for(&profile, &mut asked).unwrap();
        let keys: Vec<u8> = asked.iter().map(|asked| asked.note.pitch).collect();
        assert_eq!(keys, [52, 64, 52], "the octave under for the harmonic; a note that is settled is a key already");
        assert_eq!(moved, [("harmonic".to_string(), (12, 1))].into());
        let mut low = vec![Asked::new(note(5, 1.0, 90), Some("harmonic".to_string()))];
        assert!(keys_for(&profile, &mut low).unwrap_err().message().contains("no key that far down"));
    }

    #[test]
    fn a_clip_is_read_back_as_it_was_asked_for_and_written_again_the_same() {
        let asked = part(&[
            (40, 0.0, 90, None),
            (47, 0.0, 90, None),
            (40, 1.0, 100, Some("palm_mute")),
            (40, 1.5, 20, Some("palm_mute")),
            (45, 2.0, 90, Some("slide")),
            (47, 3.0, 90, None),
            (49, 3.5, 90, Some("hammer")),
            (51, 4.0, 90, Some("hammer")),
            (40, 5.0, 127, Some("muted")),
            (52, 6.0, 90, None),
        ]);
        let first = articulate(&guitar(), &asked, 0.0).unwrap();
        let read = read_back(&guitar(), &first.notes);
        let hows: Vec<Option<&str>> = read.iter().map(|asked| asked.how.as_deref()).collect();
        assert_eq!(
            hows,
            [
                None,
                None,
                Some("palm_mute"),
                Some("palm_mute"),
                Some("slide"),
                None,
                Some("hammer"),
                Some("hammer"),
                Some("muted"),
                None
            ]
        );
        assert!(read.iter().all(|asked| asked.settled && asked.note.pitch >= 36), "the presses are left out");
        let again = articulate_asked(&guitar(), &read, 0.0).unwrap();
        assert_eq!(again.notes, first.notes, "written again, it is the same clip");
        assert_eq!(again.fitted, 0);
        assert!(is_our_writing(&guitar(), &first.notes));

        // Presses that were placed by hand, another way, are not ours to write again.
        let mut by_hand = first.notes.clone();
        by_hand.iter_mut().find(|note| note.pitch == 26).unwrap().start -= 0.2;
        assert!(!is_our_writing(&guitar(), &by_hand));
        // Nor is a clip of notes alone on an instrument that would have been given its switch.
        assert!(!is_our_writing(&guitar(), &[note(40, 1.0, 90)]));
        assert!(is_our_writing(&InstrumentProfile::default(), &[note(40, 1.0, 90)]));
    }

    #[test]
    fn notes_added_among_others_are_played_their_way_and_leave_the_others_theirs() {
        // Open chords, written first.
        let chords = part(&[(40, 0.0, 90, None), (40, 2.0, 90, None), (40, 4.0, 90, None)]);
        let clip = articulate(&guitar(), &chords, 0.0).unwrap().notes;
        // Palm-muted notes between them, in a second call: the clip as it is, read back, with the new notes.
        let mut all = read_back(&guitar(), &clip);
        all.extend([
            Asked::new(note(43, 1.0, 90), Some("palm_mute".to_string())),
            Asked::new(note(43, 3.0, 90), Some("palm_mute".to_string())),
        ]);
        let both = articulate_asked(&guitar(), &all, 0.0).unwrap();
        assert_eq!(
            written(&both),
            "24@0 40@0.03 26@0.97 43@1 24@1.97 40@2 26@2.97 43@3 24@3.97 40@4",
            "each chord is given its open switch back after the palm mute before it"
        );
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
