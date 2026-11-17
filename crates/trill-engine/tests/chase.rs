//! A song that starts in its middle begins as it would have had it played
//! up to there, for what an instrument is told.
//!
//! What a part sets and leaves set is in force wherever the song starts,
//! seeks or loops round: the controllers (`controllers.rs` holds those)
//! and the keys that choose how notes are played. Here the keys: live,
//! through an instrument that writes down what it is told, and in renders,
//! where a stretch rendered alone is held to the same stretch cut from a
//! render of the whole song. And the notes that are already sounding where
//! a song starts: a song that plays does not begin them; a render begins
//! to play where they began and keeps nothing until its start.

mod rig;

use rig::*;
use std::sync::atomic::Ordering;
use trill_core::device::EventKind;
use trill_core::{Articulation, ChainRef, InstrumentProfile, Lasts, Note, Project, TrackId};
use trill_engine::render::{LEAD_IN_BEATS, RenderOptions, Rendered, render};
use trill_engine::scripted::{BASS, Loading, MUTE_KEY, OPEN_KEY, ScriptedFactory};

/// The keys of the guitar the probe stands for: one that plays open (how
/// a note is played when nothing is said), one that mutes and stays, and
/// one that slides into the next note only.
const OPEN: u8 = 24;
const MUTED: u8 = 25;
const SLIDE: u8 = 27;

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 100 }
}

/// A probe's track that knows its keys, with one clip of `notes`.
fn guitar(rig: &mut Rig, notes: &[Note]) -> TrackId {
    let track = rig.probe_track("Guitar");
    let profile = InstrumentProfile {
        articulations: vec![
            Articulation::key("open", OPEN).as_default(),
            Articulation::key("muted", MUTED),
            Articulation::key("slide", SLIDE).lasting(Lasts::Once),
        ],
        ..InstrumentProfile::default()
    };
    rig.session
        .edit(|project| {
            project.set_track_profile(track, profile)?;
            let clip = project.add_midi_clip(track, "part", 0.0, 16.0)?;
            project.clip_mut(clip)?.add_notes(notes)
        })
        .unwrap();
    rig.run(2);
    rig.probes.take();
    track
}

/// Puts the song at `beat`, starts it, runs `buffers` buffers and returns
/// what the probe was told, with frames counted from the song's first.
fn started_at(rig: &mut Rig, beat: f64, buffers: usize) -> Vec<(u64, EventKind)> {
    rig.session.stop();
    rig.run(2);
    rig.session.seek(beat);
    rig.run(2);
    rig.probes.take();
    let start = rig.probes.frames.load(Ordering::Relaxed);
    rig.session.play();
    rig.run(buffers);
    rig.probes.events().into_iter().map(|(frame, kind)| (frame - start, kind)).collect()
}

fn at(events: &[(u64, EventKind)], frame: u64) -> Vec<EventKind> {
    events.iter().filter(|(when, _)| *when == frame).map(|(_, kind)| *kind).collect()
}

#[test]
fn a_song_started_after_a_key_that_switches_presses_it_again_ahead_of_its_first_note() {
    let mut rig = Rig::new();
    // The mute is chosen at the top and never undone; the slide is pressed
    // just before the note at beat 8, for that note.
    let part = [
        note(MUTED, 0.0, 0.03),
        note(60, 0.5, 0.5),
        note(62, 4.0, 0.5),
        note(SLIDE, 7.97, 0.03),
        note(64, 8.0, 0.5),
        note(65, 9.0, 0.5),
    ];
    guitar(&mut rig, &part);

    // From the top nothing is pressed that the part does not press.
    let events = started_at(&mut rig, 0.0, 2);
    assert_eq!(at(&events, 0), [on(MUTED)]);

    // Started at beat 4, where the mute is chosen: it is pressed on the
    // song's first frame, ahead of the note that begins there, and let go
    // of 0.03 beats on, as the part itself holds it.
    let events = started_at(&mut rig, 4.0, 4);
    assert_eq!(at(&events, 0), [on(MUTED), on(62)]);
    assert_eq!(at(&events, 720), [off(MUTED)]);

    // Started on the note the slide was pressed for: the mute, which
    // stays, then the slide, which was not used yet, then the note.
    let events = started_at(&mut rig, 8.0, 2);
    assert_eq!(at(&events, 0), [on(MUTED), on(SLIDE), on(64)]);

    // A beat later the slide has had its note: the mute alone.
    let events = started_at(&mut rig, 9.0, 2);
    assert_eq!(at(&events, 0), [on(MUTED), on(65)]);

    // Put there while it stands still, the song presses the key and lets
    // go of it at once: a note played by hand is then played as the song
    // would play it there.
    rig.session.stop();
    rig.run(2);
    rig.probes.take();
    let before = rig.probes.frames.load(Ordering::Relaxed);
    rig.session.seek(6.0);
    rig.run(1);
    let events: Vec<(u64, EventKind)> =
        rig.probes.events().into_iter().map(|(frame, kind)| (frame - before, kind)).collect();
    assert_eq!(events, [(0, EventKind::AllNotesOff), (0, on(MUTED)), (BUFFER as u64 - 1, off(MUTED))], "{events:?}");

    // And where a loop comes round to a place after the key, it is
    // pressed again there.
    rig.session.edit(|project| project.set_loop(true, 4.0, 5.0)).unwrap();
    let events = started_at(&mut rig, 4.0, 60);
    let wrap = FRAMES_PER_BEAT as u64;
    assert_eq!(at(&events, wrap), [EventKind::AllNotesOff, on(MUTED), on(62)], "{:?}", at(&events, wrap));
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn a_song_that_plays_does_not_begin_a_note_that_was_sounding_where_it_starts_and_holds_a_key_that_was() {
    let mut rig = Rig::new();
    // A note that sounds from beat 1 to beat 7, and the mute held under
    // all of it, as an instrument needs it whose switch only lasts while
    // it is down.
    let part = [note(MUTED, 0.0, 8.0), note(60, 1.0, 6.0), note(67, 5.0, 0.5)];
    guitar(&mut rig, &part);

    let events = started_at(&mut rig, 4.0, 220);
    // The key goes down at the start and comes up where the part lets go
    // of it, four beats on. The long note is not begun: only the note
    // that starts after the song does.
    assert_eq!(at(&events, 0), [on(MUTED)]);
    let ons: Vec<EventKind> =
        events.iter().map(|(_, kind)| *kind).filter(|kind| matches!(kind, EventKind::NoteOn { .. })).collect();
    assert_eq!(ons, [on(MUTED), on(67)]);
    assert_eq!(at(&events, FRAMES_PER_BEAT as u64), [on(67)]);
    assert_eq!(at(&events, 4 * FRAMES_PER_BEAT as u64), [off(MUTED)]);
    assert_eq!(rig.session.audio_faults(), 0);
}

// ---- renders -------------------------------------------------------------------------

const RATE: u32 = 48_000;
/// Frames in a beat at 120 to the minute.
const BEAT: usize = 24_000;

/// A scripted bass whose mute stays once it is chosen, on a track that
/// knows its keys, with one clip of `notes` (pitch, start, length).
fn bass_part(notes: &[(u8, f64, f64)]) -> Project {
    let mut project = Project::new("chase");
    let track = project.add_track("Bass");
    project.add_device(ChainRef::Track(track), BASS, "Bass", None).unwrap();
    let clip = project.add_midi_clip(track, "part", 0.0, 64.0).unwrap();
    let notes: Vec<Note> = notes.iter().map(|(pitch, start, length)| note(*pitch, *start, *length)).collect();
    project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    let profile = InstrumentProfile {
        articulations: vec![Articulation::key("open", OPEN_KEY).as_default(), Articulation::key("muted", MUTE_KEY)],
        ..InstrumentProfile::default()
    };
    project.set_track_profile(track, profile).unwrap();
    project
}

fn rendered(project: &Project, loading: Loading, (start, end): (f64, f64)) -> Rendered {
    let options = RenderOptions {
        sample_rate: RATE,
        start_beat: start,
        end_beat: Some(end),
        max_tail_seconds: 0.0,
        ..RenderOptions::default()
    };
    render(project, &ScriptedFactory::loading(loading), &options).unwrap()
}

/// The most two stretches of samples differ by.
fn apart(one: &[f32], other: &[f32]) -> f32 {
    assert_eq!(one.len(), other.len());
    one.iter().zip(other).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max)
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn a_stretch_rendered_alone_is_that_stretch_of_the_whole_song_where_a_key_was_chosen_before_it() {
    // The mute is chosen at the top and stays. The second half plays
    // other notes than the first, on a bass with five recordings of each
    // note, muted and open apart.
    let part = bass_part(&[
        (MUTE_KEY, 0.0, 0.03),
        (40, 0.5, 0.5),
        (43, 1.5, 0.5),
        (40, 2.5, 0.5),
        (45, 4.0, 0.5),
        (47, 5.0, 0.5),
        (45, 6.0, 0.5),
        (48, 7.0, 0.5),
    ]);
    let bass = Loading { latches: true, takes: 5, ..Loading::default() };
    let whole = rendered(&part, bass, (0.0, 8.0));
    let alone = rendered(&part, bass, (4.0, 8.0));
    assert_eq!((whole.left.len(), alone.left.len()), (8 * BEAT, 4 * BEAT));
    let cut = &whole.left[4 * BEAT..];
    // Its notes are the muted ones: a fifth of a second after each, they
    // are gone. (Open, they would ring for seconds.)
    for beat in [0usize, 1, 2, 3] {
        let (onset, later) = (beat * BEAT, beat * BEAT + 9_600);
        assert!(peak(&alone.left[onset..onset + 480]) > 0.1, "the note at beat {} of the stretch", beat);
        assert!(peak(&alone.left[later..later + 480]) < 0.02, "the note at beat {} of the stretch rings open", beat);
    }
    // And they are the samples the whole song has there: the same notes,
    // on the same frames, on the same recordings.
    assert!(apart(&alone.left, cut) < 1e-6, "the stretch alone is {} from the song's", apart(&alone.left, cut));
    assert!(apart(&alone.right, &whole.right[4 * BEAT..]) < 1e-6);
    assert_eq!(alone.measured.led_in_beats, 0.0, "nothing was sounding where it starts");

    // A part that undoes the choice before the stretch is played open
    // there, alone as in the song.
    let part = bass_part(&[(MUTE_KEY, 0.0, 0.03), (40, 0.5, 0.5), (OPEN_KEY, 3.5, 0.03), (45, 4.0, 0.5)]);
    let (whole, alone) = (rendered(&part, bass, (0.0, 8.0)), rendered(&part, bass, (4.0, 8.0)));
    assert!(peak(&alone.left[4_800..5_280]) > 0.1, "the open note is muted");
    assert!(apart(&alone.left, &whole.left[4 * BEAT..]) < 1e-6);

    // One that undoes it inside the stretch begins the stretch muted all
    // the same: the wait before the render tries the stretch's last notes
    // open and leaves the bass so, and the render's first frame presses
    // what the song has chosen there.
    let part =
        bass_part(&[(MUTE_KEY, 0.0, 0.03), (40, 0.5, 0.5), (45, 4.0, 0.5), (OPEN_KEY, 5.5, 0.03), (47, 6.0, 0.5)]);
    let (whole, alone) = (rendered(&part, bass, (0.0, 8.0)), rendered(&part, bass, (4.0, 8.0)));
    assert!(peak(&alone.left[9_600..10_080]) < 0.02, "the stretch begins on what the wait left chosen");
    assert!(peak(&alone.left[2 * BEAT + 9_600..2 * BEAT + 10_080]) > 0.1, "the note after the open key is muted");
    assert!(apart(&alone.left, &whole.left[4 * BEAT..]) < 1e-6);
}

#[test]
fn a_note_that_is_sounding_where_a_render_starts_is_heard_from_where_it_began() {
    // An open note that begins a beat before the stretch and is held for
    // three, then two more in the stretch.
    let part = bass_part(&[(40, 3.0, 3.0), (45, 5.0, 0.5), (47, 6.0, 0.5)]);
    let bass = Loading { latches: true, ..Loading::default() };
    let whole = rendered(&part, bass, (0.0, 8.0));
    let alone = rendered(&part, bass, (4.0, 8.0));
    // The render began to play a beat ahead and kept nothing of that
    // beat: what it kept is the stretch.
    assert_eq!(alone.measured.led_in_beats, 1.0);
    assert_eq!(alone.left.len(), 4 * BEAT);
    assert!(alone.measured.not_begun.is_empty());
    // The long note is ringing as the stretch begins, where the song has
    // it by then, and the stretch is the song's.
    assert!(peak(&alone.left[..480]) > 0.05, "the note that was sounding is not there");
    let cut = &whole.left[4 * BEAT..];
    assert!(apart(&alone.left, cut) < 1e-6, "the stretch alone is {} from the song's", apart(&alone.left, cut));
    // What was measured is the stretch's own: the track's level is what
    // the whole song has over those four beats, not over five.
    let level = |rendered: &Rendered| rendered.measured.tracks[0].peak_db;
    let stretch_of_whole = 20.0 * peak(cut).log10();
    assert!((level(&alone) - stretch_of_whole).abs() < 0.1, "{} against {stretch_of_whole}", level(&alone));

    // One that began longer ago than a render goes back is not played,
    // and the render says how many such notes there were on which track.
    let far = LEAD_IN_BEATS + 8.0;
    let part = bass_part(&[(40, 1.0, far), (45, far + 1.0, 0.5)]);
    let alone = rendered(&part, bass, (far, far + 4.0));
    assert_eq!(alone.measured.led_in_beats, 0.0);
    let [left_out] = &alone.measured.not_begun[..] else { panic!("{:?}", alone.measured.not_begun) };
    assert_eq!((left_out.track_name.as_str(), left_out.notes), ("Bass", 1));
    assert!(peak(&alone.left[..BEAT / 2]) < 1e-4, "it was begun at the start");
    assert!(peak(&alone.left[BEAT..2 * BEAT]) > 0.1, "the note in the stretch is there");
}
