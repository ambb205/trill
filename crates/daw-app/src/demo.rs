//! A short built-in song, for trying the app out (`--demo`, or the button
//! on an empty project). Sixteen bars in A minor over Am - F - C - G: pad and
//! hats, then drums and bass, then an arpeggio, then a bell melody on top.

use daw_core::{Beats, ChainRef, ClipId, DeviceId, Note, TrackId};
use daw_engine::{Session, SessionError};

const BAR: Beats = 4.0;
/// Bars in one pass through the chord progression.
const PROGRESSION_BARS: usize = 4;
const PROGRESSION: Beats = PROGRESSION_BARS as f64 * BAR;

/// One chord of the progression.
struct Chord {
    /// Bass note.
    root: u8,
    /// Close-voiced triad around middle C, for the pad.
    triad: [u8; 3],
    /// Four chord tones climbing from the fourth octave, for the arpeggio.
    arpeggio: [u8; 4],
}

const CHORDS: [Chord; PROGRESSION_BARS] = [
    Chord { root: 33, triad: [57, 60, 64], arpeggio: [69, 72, 76, 81] }, // A minor
    Chord { root: 29, triad: [57, 60, 65], arpeggio: [65, 69, 72, 77] }, // F major
    Chord { root: 36, triad: [55, 60, 64], arpeggio: [67, 72, 76, 79] }, // C major
    Chord { root: 31, triad: [55, 59, 62], arpeggio: [67, 71, 74, 79] }, // G major
];

fn note(pitch: u8, start: Beats, length: Beats, velocity: u8) -> Note {
    Note { pitch, start, length, velocity }
}

/// Fills the project with the demo song, as a single undo step, and returns
/// the clip that is the most interesting to look at.
pub fn load(session: &mut Session) -> Result<ClipId, SessionError> {
    session.begin_gesture();
    let built = build(session);
    session.end_gesture();
    built
}

fn build(session: &mut Session) -> Result<ClipId, SessionError> {
    session.edit(|project| {
        project.name = "Night Drive".to_owned();
        project.set_tempo(112.0)?;
        project.set_loop(true, 8.0 * BAR, 12.0 * BAR)
    })?;

    let drums = track(session, "Drums", "builtin:drumkit", "909", 0.0, 0.0)?;
    effect(session, drums, "builtin:compressor", &[("threshold", -16.0), ("ratio", 3.0), ("makeup", 2.0)])?;
    clip(session, drums, "Hats", 0.0, 4.0 * BAR, Some(BAR), intro_hats())?;
    clip(session, drums, "Beat", 4.0 * BAR, 12.0 * BAR, Some(2.0 * BAR), beat())?;

    let bass = track(session, "Bass", "builtin:polysynth", "bass", -4.0, 0.0)?;
    effect(session, bass, "builtin:saturator", &[("drive", 9.0), ("mix", 0.6)])?;
    clip(session, bass, "Bassline", 4.0 * BAR, 4.0 * BAR, None, bassline())?;
    clip(session, bass, "Bassline", 8.0 * BAR, 8.0 * BAR, Some(PROGRESSION), bassline())?;

    let pad = track(session, "Pad", "builtin:polysynth", "warm_pad", -10.0, 0.0)?;
    effect(session, pad, "builtin:chorus", &[("mix", 0.4)])?;
    effect(session, pad, "builtin:reverb", &[("decay", 4.5), ("mix", 0.35)])?;
    clip(session, pad, "Chords", 0.0, 8.0 * BAR, Some(PROGRESSION), pad_chords())?;
    clip(session, pad, "Chords", 8.0 * BAR, 8.0 * BAR, Some(PROGRESSION), pad_chords())?;

    let arp = track(session, "Arp", "builtin:polysynth", "pluck", -9.0, 0.2)?;
    effect(session, arp, "builtin:delay", &[("time_beats", 0.75), ("feedback", 0.45), ("mix", 0.28)])?;
    effect(session, arp, "builtin:reverb", &[("mix", 0.2)])?;
    let featured = clip(session, arp, "Arp A", 8.0 * BAR, PROGRESSION, None, arpeggio(false))?;
    clip(session, arp, "Arp B", 12.0 * BAR, PROGRESSION, None, arpeggio(true))?;

    let bell = track(session, "Bell", "builtin:polysynth", "bell", -12.0, -0.25)?;
    effect(session, bell, "builtin:delay", &[("time_beats", 1.0), ("mix", 0.25)])?;
    clip(session, bell, "Melody", 12.0 * BAR, PROGRESSION, None, melody())?;

    Ok(featured)
}

/// Adds a track with an instrument set to one of its presets.
fn track(
    session: &mut Session,
    name: &str,
    instrument: &str,
    preset: &str,
    volume_db: f32,
    pan: f32,
) -> Result<TrackId, SessionError> {
    let track = session.edit(|project| {
        let track = project.add_track(name);
        project.set_track_volume(track, volume_db)?;
        project.set_track_pan(track, pan)?;
        Ok(track)
    })?;
    session.add_device(ChainRef::Track(track), instrument, Some(preset), None)?;
    Ok(track)
}

fn effect(session: &mut Session, track: TrackId, uri: &str, params: &[(&str, f64)]) -> Result<DeviceId, SessionError> {
    let device = session.add_device(ChainRef::Track(track), uri, None, None)?;
    let values: Vec<(String, f64)> = params.iter().map(|(id, value)| (id.to_string(), *value)).collect();
    session.set_device_params(device, &values)?;
    Ok(device)
}

fn clip(
    session: &mut Session,
    track: TrackId,
    name: &str,
    start: Beats,
    length: Beats,
    loop_length: Option<Beats>,
    notes: Vec<Note>,
) -> Result<ClipId, SessionError> {
    session.edit(|project| {
        let id = project.add_midi_clip(track, name, start, length)?;
        let clip = project.clip_mut(id)?;
        clip.set_notes(notes)?;
        clip.set_loop_length(loop_length)?;
        Ok(id)
    })
}

// General MIDI drum notes.
const KICK: u8 = 36;
const SNARE: u8 = 38;
const CLAP: u8 = 39;
const CLOSED_HAT: u8 = 42;
const OPEN_HAT: u8 = 46;
const CRASH: u8 = 49;

/// One bar of offbeat hats with ghost sixteenths.
fn intro_hats() -> Vec<Note> {
    (0..16)
        .filter(|step| step % 4 != 0)
        .map(|step| {
            let velocity = if step % 4 == 2 { 96 } else { 52 };
            note(CLOSED_HAT, step as f64 * 0.25, 0.25, velocity)
        })
        .collect()
}

/// Two bars: four on the floor, backbeat, hats, and a push into the repeat.
fn beat() -> Vec<Note> {
    let mut notes = vec![note(CRASH, 0.0, 1.0, 84)];
    for bar in 0..2 {
        let at = |beat: Beats| bar as f64 * BAR + beat;
        for beat in 0..4 {
            notes.push(note(KICK, at(beat as f64), 0.25, if beat == 0 { 120 } else { 108 }));
        }
        for backbeat in [1.0, 3.0] {
            notes.push(note(SNARE, at(backbeat), 0.25, 104));
            notes.push(note(CLAP, at(backbeat), 0.25, 76));
        }
        for step in (0..16).filter(|step| step % 4 != 0) {
            let last_offbeat = bar == 1 && step == 14;
            let (pitch, velocity) = match step % 4 {
                2 if last_offbeat => (OPEN_HAT, 100),
                2 => (CLOSED_HAT, 98),
                _ => (CLOSED_HAT, 54),
            };
            notes.push(note(pitch, at(step as f64 * 0.25), 0.25, velocity));
        }
    }
    notes.push(note(KICK, BAR + 3.5, 0.25, 96));
    notes
}

/// Four bars: the root, an octave jump and a fifth leading into the next
/// chord.
fn bassline() -> Vec<Note> {
    let mut notes = Vec::new();
    for (bar, chord) in CHORDS.iter().enumerate() {
        let at = |beat: Beats| bar as f64 * BAR + beat;
        notes.extend([
            note(chord.root, at(0.0), 0.75, 112),
            note(chord.root, at(1.0), 0.5, 96),
            note(chord.root + 12, at(1.75), 0.25, 84),
            note(chord.root, at(2.5), 0.5, 104),
            note(chord.root, at(3.0), 0.5, 92),
            note(chord.root + 7, at(3.5), 0.5, 88),
        ]);
    }
    notes
}

/// Four bars of held triads.
fn pad_chords() -> Vec<Note> {
    let mut notes = Vec::new();
    for (bar, chord) in CHORDS.iter().enumerate() {
        notes.push(note(chord.root + 12, bar as f64 * BAR, BAR, 70));
        notes.extend(chord.triad.iter().map(|pitch| note(*pitch, bar as f64 * BAR, BAR, 78)));
    }
    notes
}

/// Four bars of sixteenths through the chord tones, accented in threes
/// against the beat. The variation climbs in its last bar.
fn arpeggio(variation: bool) -> Vec<Note> {
    const SHAPE: [usize; 16] = [0, 1, 2, 3, 2, 1, 2, 3, 0, 1, 2, 3, 2, 1, 0, 1];
    const CLIMB: [usize; 16] = [0, 1, 2, 3, 0, 1, 2, 3, 1, 2, 3, 2, 3, 3, 3, 3];
    let mut notes = Vec::new();
    for (bar, chord) in CHORDS.iter().enumerate() {
        let shape = if variation && bar == PROGRESSION_BARS - 1 { CLIMB } else { SHAPE };
        for (step, tone) in shape.iter().enumerate() {
            let accent = matches!(step, 0 | 3 | 6 | 8 | 11 | 14);
            // The octave above on the last three steps of the climb.
            let lift = if variation && bar == PROGRESSION_BARS - 1 && step >= 13 { 12 } else { 0 };
            notes.push(note(
                chord.arpeggio[*tone] + lift,
                bar as f64 * BAR + step as f64 * 0.25,
                0.25,
                if accent { 112 } else { 74 },
            ));
        }
    }
    notes
}

/// Four bars of long notes over the chords.
fn melody() -> Vec<Note> {
    vec![
        note(76, 0.0, 3.0, 100),
        note(74, 3.0, 1.0, 84),
        note(72, 4.0, 2.0, 96),
        note(69, 6.0, 2.0, 88),
        note(67, 8.0, 1.0, 90),
        note(72, 9.0, 1.0, 94),
        note(76, 10.0, 2.0, 104),
        note(74, 12.0, 3.0, 100),
        note(71, 15.0, 1.0, 86),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::device::CompositeFactory;
    use daw_dsp::BuiltinFactory;
    use daw_engine::AudioBackend;
    use std::sync::Arc;

    fn session() -> Session {
        let factory = Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory)]));
        Session::new(factory, AudioBackend::Null { sample_rate: 48_000 }).unwrap()
    }

    #[test]
    fn the_demo_loads_cleanly_as_one_undo_step() {
        let mut session = session();
        let featured = load(&mut session).unwrap();
        assert!(session.problems().is_empty(), "{:?}", session.problems());

        let project = session.project();
        assert_eq!(project.name, "Night Drive");
        assert_eq!(project.tracks.len(), 5);
        assert_eq!(project.length_beats(), 16.0 * BAR);
        assert!(project.loop_region.enabled);
        assert_eq!(project.clip(featured).unwrap().name, "Arp A");
        for track in &project.tracks {
            assert!(track.devices.len() >= 2, "{} has an instrument and at least one effect", track.name);
            assert!(!track.clips.is_empty(), "{} has clips", track.name);
            for clip in &track.clips {
                assert!(!clip.notes().is_empty(), "{} / {} has notes", track.name, clip.name);
            }
        }

        assert!(session.undo());
        assert!(session.project().tracks.is_empty(), "one undo removes the whole demo");
        assert!(!session.can_undo());
    }

    #[test]
    fn patterns_stay_inside_their_loops() {
        let inside = |notes: &[Note], length: Beats| notes.iter().all(|note| note.start >= 0.0 && note.end() <= length);
        assert!(inside(&intro_hats(), BAR));
        assert!(inside(&beat(), 2.0 * BAR));
        assert!(inside(&bassline(), PROGRESSION));
        assert!(inside(&pad_chords(), PROGRESSION));
        assert!(inside(&arpeggio(false), PROGRESSION));
        assert!(inside(&arpeggio(true), PROGRESSION));
        assert!(inside(&melody(), PROGRESSION));
        assert_eq!(arpeggio(false).len(), 64);
        assert_ne!(arpeggio(false), arpeggio(true));
    }
}
