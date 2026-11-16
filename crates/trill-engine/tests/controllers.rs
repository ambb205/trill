//! MIDI controllers in clips, end to end: a session playing through a
//! device that exists only here, with an instrument that writes down what
//! it is told, and renders whose audio is measured.
//!
//! What is checked is what the rules in `Session`'s controller module
//! promise: a curve is played as a slope and in time; it is in force
//! wherever the song starts, seeks or loops round; nothing is left held or
//! bent; a controller moved by hand is the hand's until the song next
//! starts or jumps; and what is played on wheels and pedals is recorded.

mod common;
mod rig;

use common::{largest_step, rms};
use rig::*;
use std::sync::atomic::Ordering;
use trill_core::controllers::THIN_TOLERANCE;
use trill_core::device::{ControllerSupport, EventKind};
use trill_core::project::{AutomationPoint, Segment};
use trill_core::{ChainRef, ClipId, ControllerKind, Note, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::render::{RenderOptions, render};
use trill_engine::{AudioSources, Processor, Session};

const EXPRESSION: ControllerKind = ControllerKind::EXPRESSION;
const PEDAL: ControllerKind = ControllerKind::SUSTAIN;
const BEND: ControllerKind = ControllerKind::PitchBend;
const MOD: ControllerKind = ControllerKind::MOD_WHEEL;

fn point(beat: f64, value: f64) -> AutomationPoint {
    AutomationPoint::new(beat, value)
}

fn held(beat: f64, value: f64) -> AutomationPoint {
    AutomationPoint { beat, value, shape: Segment::Hold }
}

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 100 }
}

/// A clip on `track` with `notes` and a curve for each of `curves`.
fn clip(
    session: &mut Session,
    track: TrackId,
    (start, length): (f64, f64),
    notes: &[Note],
    curves: &[(ControllerKind, Vec<AutomationPoint>)],
) -> ClipId {
    session
        .edit(|project| {
            let id = project.add_midi_clip(track, "clip", start, length)?;
            let clip = project.clip_mut(id)?;
            clip.add_notes(notes)?;
            for (kind, points) in curves {
                clip.set_controller(*kind, points.clone())?;
            }
            Ok(id)
        })
        .unwrap()
}

/// The one probe's clock: how many frames it has played.
fn probe_frames(rig: &Rig) -> u64 {
    rig.probes.frames.load(Ordering::Relaxed)
}

/// What the probe was told since the last look that moves `kind`, as
/// `(frame, value)`.
fn moves(events: &[(u64, EventKind)], kind: ControllerKind) -> Vec<(u64, f32)> {
    events
        .iter()
        .filter_map(|(frame, event)| match ControllerKind::of_event(*event) {
            Some((moved, value)) if moved == kind => Some((*frame, value)),
            _ => None,
        })
        .collect()
}

/// Starts the song and returns the probe's frame at which it starts: the
/// first frame of the next buffer.
fn play(rig: &mut Rig) -> u64 {
    let start = probe_frames(rig);
    rig.session.play();
    start
}

#[test]
fn a_curve_is_played_as_a_slope_every_few_frames_and_in_time() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Sax");
    // Expression rising from nothing to full over four beats.
    clip(
        &mut rig.session,
        track,
        (0.0, 8.0),
        &[note(60, 0.0, 4.0)],
        &[(EXPRESSION, vec![point(0.0, 0.0), point(4.0, 1.0)])],
    );
    rig.run(2);
    // Standing at the start, the instrument is where the song has it there.
    assert_eq!(moves(&rig.probes.events(), EXPRESSION).last().map(|(_, value)| *value), Some(0.0));

    let start = play(&mut rig);
    rig.run(100);
    let events = rig.probes.events();
    assert_eq!(events[0], (start, on(60)), "the note starts with the song");
    let swell = moves(&events, EXPRESSION);
    // Two beats are 48000 frames: a value every 32 of them after the first.
    assert_eq!(swell.len(), 1_499, "one value per step of 32 frames");
    for (index, (frame, value)) in swell.iter().enumerate() {
        let elapsed = frame - start;
        assert_eq!(elapsed, 32 * (index as u64 + 1), "on the grid, counted from where the song started");
        let wanted = elapsed as f64 / FRAMES_PER_BEAT / 4.0;
        assert!((f64::from(*value) - wanted).abs() < 1e-6, "at frame {elapsed}: {value} for {wanted}");
    }
    // No step of it is bigger than the curve's own slope over 32 frames.
    assert!(swell.windows(2).all(|pair| pair[1].1 - pair[0].1 < 0.0004));
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn a_song_started_in_the_middle_has_every_controller_where_it_is_there_before_the_first_note() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Sax");
    let curves = [
        (EXPRESSION, vec![point(0.0, 0.0), point(8.0, 1.0)]),
        (BEND, vec![point(0.0, 0.5)]),
        (PEDAL, vec![held(1.0, 1.0), held(7.0, 0.0)]),
    ];
    clip(&mut rig.session, track, (0.0, 8.0), &[note(60, 0.0, 1.0), note(62, 4.0, 1.0)], &curves);
    rig.run(2);
    rig.probes.take();

    // Moved to the middle while stopped: what stays put follows, and what
    // springs back stays at rest until something plays.
    rig.session.seek(4.0);
    rig.run(2);
    let events = rig.probes.events();
    assert_eq!(moves(&events, EXPRESSION), [(probe_frames(&rig) - 2 * BUFFER as u64, 0.5)]);
    assert!(moves(&events, BEND).is_empty() && moves(&events, PEDAL).is_empty(), "{events:?}");

    // Started there: the wheel and the pedal are where the song has them,
    // on the song's first frame and ahead of the note that starts on it.
    let start = play(&mut rig);
    rig.run(2);
    let events: Vec<(u64, EventKind)> =
        rig.probes.events().into_iter().filter(|(frame, _)| *frame == start).map(|(_, kind)| (0, kind)).collect();
    let at_start: Vec<EventKind> = events.iter().map(|(_, kind)| *kind).collect();
    assert_eq!(at_start, [pedal(true), EventKind::PitchBend { value: 0.5 }, on(62)]);
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn the_loop_coming_round_puts_every_controller_back_where_the_loop_starts() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Sax");
    let curves = [(EXPRESSION, vec![point(0.0, 0.25), point(2.0, 0.75)]), (PEDAL, vec![held(1.0, 1.0)])];
    clip(&mut rig.session, track, (0.0, 4.0), &[note(60, 1.5, 1.0)], &curves);
    rig.session.edit(|project| project.set_loop(true, 0.0, 2.0)).unwrap();
    rig.run(2);
    rig.probes.take();

    let start = play(&mut rig);
    // Two beats and a little: the loop comes round at frame 48000.
    rig.run(104);
    let events = rig.probes.events();
    let wrap = start + 48_000;
    let before: Vec<(u64, EventKind)> = events.iter().copied().filter(|(frame, _)| *frame < wrap).collect();
    assert!((moves(&before, EXPRESSION).last().unwrap().1 - 0.75).abs() < 1e-3, "it had nearly reached the top");
    assert_eq!(moves(&before, PEDAL), [(start + 24_000, 1.0)], "the pedal went down half way");

    // On the very frame of the wrap: the song's notes are let go of, the
    // pedal is up again and the swell starts over, mid-gesture or not.
    let at_wrap: Vec<EventKind> = events.iter().filter(|(frame, _)| *frame == wrap).map(|(_, kind)| *kind).collect();
    assert_eq!(at_wrap, [EventKind::AllNotesOff, EXPRESSION.event(0.25), pedal(false)]);
    let after: Vec<(u64, EventKind)> = events.iter().copied().filter(|(frame, _)| *frame > wrap).collect();
    let swell = moves(&after, EXPRESSION);
    assert!(swell[0].0 == wrap + 32 && swell[0].1 > 0.25 && swell[0].1 < 0.2504, "{:?}", swell[0]);
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn at_a_clips_end_the_pedal_comes_up_and_the_wheel_centres_and_the_rest_stays() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Keys");
    let curves = [
        (MOD, vec![point(0.0, 0.5)]),
        (EXPRESSION, vec![point(0.0, 0.75)]),
        (PEDAL, vec![held(0.5, 1.0)]),
        (ControllerKind::Pressure, vec![point(0.0, 0.25)]),
        (BEND, vec![point(0.0, -0.5)]),
    ];
    clip(&mut rig.session, track, (0.0, 2.0), &[note(60, 0.0, 2.0)], &curves);
    // A second clip, later, that says nothing about any controller.
    clip(&mut rig.session, track, (3.0, 1.0), &[note(64, 0.0, 1.0)], &[]);
    rig.run(2);
    rig.probes.take();

    let start = play(&mut rig);
    rig.run(210);
    let events = rig.probes.events();
    let end = start + 48_000;
    let at_end: Vec<EventKind> = events.iter().filter(|(frame, _)| *frame == end).map(|(_, kind)| *kind).collect();
    assert_eq!(
        at_end,
        [pedal(false), ControllerKind::Pressure.event(0.0), EventKind::PitchBend { value: 0.0 }, off(60)],
        "what springs back is let go of where the clip ends"
    );
    // (The probe never makes a sound, so it is told where its controllers
    // are again while it has a note to play: the same places, which is
    // nothing moving.)
    let at_rest = [MOD.event(0.5), EXPRESSION.event(0.75), pedal(false), ControllerKind::Pressure.event(0.0)];
    let moved = |kind: &EventKind| !at_rest.contains(kind) && *kind != EventKind::PitchBend { value: 0.0 };
    let later: Vec<(u64, EventKind)> =
        events.iter().copied().filter(|(frame, kind)| *frame > end && moved(kind)).collect();
    assert_eq!(later, [(start + 72_000, on(64)), (start + 96_000, off(64))], "and nothing else moves: {later:?}");
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn stopping_lets_go_of_the_songs_pedal_and_wheel_and_leaves_the_level_where_it_was() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Keys");
    let curves = [
        (EXPRESSION, vec![point(0.0, 0.5), point(8.0, 1.0)]),
        (PEDAL, vec![held(0.0, 1.0)]),
        (BEND, vec![point(0.0, 0.25)]),
    ];
    clip(&mut rig.session, track, (0.0, 8.0), &[note(60, 0.0, 8.0)], &curves);
    rig.run(2);
    rig.probes.take();
    play(&mut rig);
    rig.run(50);
    rig.probes.take();

    // Stopped with the pedal down and the wheel bent.
    rig.session.stop();
    rig.run(3);
    let stopped = rig.probes.events();
    let level = moves(&stopped, EXPRESSION);
    let others: Vec<EventKind> = stopped
        .iter()
        .map(|(_, kind)| *kind)
        .filter(|kind| !matches!(kind, EventKind::Control { controller: 11, .. }))
        .collect();
    assert_eq!(others, [EventKind::AllNotesOff, pedal(false), EventKind::PitchBend { value: 0.0 }]);
    // The level is where the song has it at the place it stopped: at most
    // the last few frames' worth further on than it was last told.
    assert!(level.len() <= 1 && level.iter().all(|(_, value)| (value - 0.5625).abs() < 1e-4), "{level:?}");
    // Nothing more while it stands, and a key played by hand now sounds at
    // the level the song has there.
    rig.run(20);
    assert_eq!(rig.probes.events(), []);

    // Deleting a curve while its pedal is down lets the pedal up: no curve
    // can leave one held.
    play(&mut rig);
    rig.run(4);
    assert_eq!(moves(&rig.probes.events(), PEDAL).last().map(|(_, value)| *value), Some(1.0));
    let clip = rig.session.project().track(track).unwrap().clips[0].id;
    rig.session.set_controller_curve(clip, PEDAL, Vec::new()).unwrap();
    rig.session.set_controller_curve(clip, BEND, Vec::new()).unwrap();
    rig.run(2);
    let events = rig.probes.events();
    assert_eq!((moves(&events, PEDAL), moves(&events, BEND).len()), (vec![(events[0].0, 0.0)], 1), "{events:?}");
    // Undoing brings the curves, and the pedal, back.
    assert!(rig.session.undo() && rig.session.undo());
    rig.run(2);
    assert_eq!(moves(&rig.probes.events(), PEDAL).last().map(|(_, value)| *value), Some(1.0));
    // And a muted clip moves nothing.
    rig.session
        .edit(|project| {
            project.clip_mut(clip)?.muted = true;
            Ok(())
        })
        .unwrap();
    rig.run(2);
    assert_eq!(moves(&rig.probes.events(), PEDAL), [(probe_frames(&rig) - 2 * BUFFER as u64, 0.0)]);
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn a_controller_moved_by_hand_is_the_hands_until_the_song_next_starts_or_jumps() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Sax");
    let other = rig.probe_track("Other");
    rig.session.edit(|project| project.remove_track(other).map(|_| ())).unwrap();
    clip(&mut rig.session, track, (0.0, 4.0), &[], &[(EXPRESSION, vec![point(0.0, 0.0), point(4.0, 1.0)])]);
    rig.session.edit(|project| project.set_loop(true, 0.0, 2.0)).unwrap();
    rig.run(2);
    rig.probes.take();

    let start = play(&mut rig);
    rig.run(10);
    assert!(moves(&rig.probes.events(), EXPRESSION).len() > 100, "the song is moving it");

    // The hand takes it: from here on the curve is not played.
    rig.session.send_controller(track, EXPRESSION, 0.875);
    rig.run(2);
    let events = rig.probes.events();
    let taken = moves(&events, EXPRESSION).iter().position(|(_, value)| *value == 0.875).expect("the hand's value");
    assert_eq!(moves(&events, EXPRESSION).len(), taken + 1, "nothing from the song after it");
    while probe_frames(&rig) < start + 48_000 - BUFFER as u64 {
        rig.run(1);
    }
    assert_eq!(rig.probes.events(), [], "nor for the rest of the pass");

    // The loop comes round: the song has it again, from the wrap's frame.
    rig.run(4);
    let swell = moves(&rig.probes.events(), EXPRESSION);
    assert_eq!(swell[0], (start + 48_000, 0.0));
    assert!(swell.len() > 1);

    // Stopped, the hand's value stays through a seek: a keyboard played
    // while the song stands still keeps the level it was given...
    rig.session.stop();
    rig.session.send_controller(track, EXPRESSION, 0.625);
    rig.run(2);
    rig.probes.take();
    rig.session.seek(1.0);
    rig.run(4);
    assert_eq!(moves(&rig.probes.events(), EXPRESSION), []);
    // ...and the song takes it back as it starts.
    let start = play(&mut rig);
    rig.run(2);
    let swell = moves(&rig.probes.events(), EXPRESSION);
    assert_eq!(swell[0], (start, 0.25), "the level at the first beat, on the song's first frame");

    // A controller the song has no curve for stays the hand's for good.
    rig.session.send_controller(track, MOD, 0.5);
    rig.session.send_controller(track, MOD, 7.0);
    rig.run(110);
    let wheel = moves(&rig.probes.events(), MOD);
    assert_eq!(
        wheel.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
        [0.5, 1.0],
        "kept within its range, never taken back"
    );
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn an_instrument_that_is_made_anew_is_told_where_its_controllers_are() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Sax");
    clip(&mut rig.session, track, (0.0, 4.0), &[], &[(EXPRESSION, vec![point(0.0, 0.75)])]);
    rig.session.send_controller(track, MOD, 0.5);
    rig.run(2);
    rig.probes.take();

    // The device is created again from another state, as an undo or a
    // sampler given another file does.
    let device = rig.session.instrument(track).unwrap();
    rig.session.set_device_state(device, Some(vec![1, 2, 3])).unwrap();
    assert_eq!(rig.probes.created.load(Ordering::Relaxed), 2);
    rig.run(2);
    let told: Vec<EventKind> = rig.probes.events().into_iter().map(|(_, kind)| kind).collect();
    assert_eq!(told, [MOD.event(0.5), EXPRESSION.event(0.75)], "what the hand set, then what the song says");

    // So is one that was reset.
    rig.session.panic();
    rig.run(2);
    let told: Vec<EventKind> = rig.probes.events().into_iter().map(|(_, kind)| kind).collect();
    assert_eq!(told, [EventKind::AllNotesOff, MOD.event(0.5), EXPRESSION.event(0.75)]);
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn an_instrument_that_has_not_answered_its_notes_is_told_its_controllers_again() {
    // A plugin that is still waking up when the song starts does not hear
    // what it is told. A level that never moves would not be said twice.
    let mut rig = Rig::new();
    let silent = rig.probe_track("Sax");
    let level = vec![point(0.0, 0.75)];
    clip(&mut rig.session, silent, (0.0, 8.0), &[note(60, 0.5, 6.0)], &[(EXPRESSION, level)]);
    rig.session.send_controller(silent, MOD, 0.5);
    rig.run(2);
    rig.probes.take();
    let start = play(&mut rig);
    // A second and a half: a quarter of a second before the note, and
    // then the note, which the probe never answers.
    rig.run(150);
    let events = rig.probes.events();
    let told = moves(&events, EXPRESSION);
    // (It was told once before the song started, where the playhead stood.)
    assert!(told.len() >= 3, "said again and again while nothing sounded: {told:?}");
    assert!(told.iter().all(|(_, value)| *value == 0.75), "the same thing every time: {told:?}");
    let note_on = start + RATE as u64 / 4;
    assert!(told[0].0 > note_on, "not before there was a note to answer: {told:?}");
    let apart: Vec<u64> = told.windows(2).map(|pair| pair[1].0 - pair[0].0).collect();
    assert!(apart.iter().all(|frames| (15_000..=18_000).contains(frames)), "about three times a second: {apart:?}");
    // What the hand set is said again with it.
    assert_eq!(moves(&events, MOD).len(), told.len(), "{events:?}");

    // An instrument that sounds is told once.
    let mut rig = Rig::new();
    let sine = rig.sine_track("Keys");
    let level = vec![point(0.0, 0.75)];
    clip(&mut rig.session, sine, (0.0, 8.0), &[note(60, 0.5, 6.0)], &[(EXPRESSION, level)]);
    let probe = rig.probe_track("Listener");
    clip(&mut rig.session, probe, (0.0, 8.0), &[], &[(EXPRESSION, vec![point(0.0, 0.5)])]);
    rig.run(2);
    rig.probes.take();
    play(&mut rig);
    rig.run(150);
    // (The probe has no notes, so it has nothing to answer either.)
    assert_eq!(moves(&rig.probes.events(), EXPRESSION).len(), 0, "told before the song started, and not again");
    assert_eq!(rig.session.silent_instrument(sine), None);
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn a_keyboard_that_stops_playing_a_track_leaves_nothing_held_or_bent_there() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Keys");
    rig.session.set_selected_track(Some(track));
    rig.run(2);
    rig.probes.take();
    for kind in
        [pedal(true), BEND.event(0.5), ControllerKind::Pressure.event(0.75), MOD.event(0.5), EXPRESSION.event(0.25)]
    {
        rig.session.play_live(kind);
    }
    rig.run(2);
    assert_eq!(rig.probes.events().len(), 5);

    // The keyboard goes elsewhere: the pedal, the pressure and the wheel
    // are let go of. What does not spring back is left where it was put.
    rig.session.set_selected_track(None);
    rig.run(2);
    let released: Vec<EventKind> = rig.probes.events().into_iter().map(|(_, kind)| kind).collect();
    assert_eq!(released, [pedal(false), ControllerKind::Pressure.event(0.0), BEND.event(0.0)]);
    rig.session.release_live();
    rig.run(2);
    assert_eq!(rig.probes.events(), [], "once");
}

#[test]
fn what_is_played_on_wheels_and_pedals_is_recorded_and_reproduces_the_gesture() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Sax");
    rig.session.set_armed(track, true).unwrap();
    rig.session.set_count_in(0);
    // The expression was set before the take, as a keyboard's strip does.
    rig.session.play_live(EXPRESSION.event(0.75));
    rig.run(4);

    rig.session.record().unwrap();
    rig.run(2);
    rig.session.housekeeping();
    let status = rig.session.record_status();
    let started = rig.now() - 2.0 * BUFFER as f64;
    assert_eq!(status.start_beat, Some(0.0));

    // What is heard at a frame of the host's clock is the song as it was
    // rendered a latency earlier.
    let heard = |frame: f64| (frame - started - LATENCY) / FRAMES_PER_BEAT;
    // A note, the mod wheel pushed up and let back over a beat (reported
    // every 96 frames in a controller's 128 steps), and the pedal.
    let key_at = started + LATENCY + 12_000.0;
    rig.key(key_at, on(60));
    let wheel_from = started + LATENCY + 24_000.0;
    let gesture: Vec<(f64, f32)> = (0..=250)
        .map(|n| {
            let frame = wheel_from + n as f64 * 96.0;
            let value = ((std::f64::consts::PI * n as f64 / 250.0).sin() * 127.0).round() / 127.0;
            (frame, value as f32)
        })
        .collect();
    for (frame, value) in &gesture {
        rig.key(*frame, MOD.event(*value));
    }
    let (pedal_down, pedal_up) = (started + LATENCY + 60_000.0, started + LATENCY + 72_000.0);
    rig.key(pedal_down, pedal(true));
    rig.key(key_at + 54_000.0, off(60));
    // The lane shows the gesture while the take still runs.
    rig.session.housekeeping();
    let recording = rig.session.record_status();
    let clip = recording.clips[0].clip;
    assert!(rig.session.clip_controllers(clip).unwrap().contains(&MOD), "{recording:?}");
    rig.key(pedal_up, pedal(false));
    rig.run_until(started + LATENCY + 80_000.0);
    rig.session.stop();

    let take = rig.session.last_take().to_vec();
    assert_eq!(take.len(), 1);
    assert_eq!(take[0].controllers, [MOD, EXPRESSION, PEDAL]);
    assert_eq!(rig.session.clip_controllers(clip).unwrap(), [MOD, EXPRESSION, PEDAL]);
    assert_notes(&timeline(&rig.session, clip), &[(60, 0.5, 2.25)]);

    // The wheel: a handful of points for 251 reports, that pass within a
    // step of a MIDI controller of every one of them, where it was heard.
    let lanes = rig.session.project().track(track).unwrap().controller_lanes();
    let lane = |kind: ControllerKind| lanes.iter().find(|lane| lane.kind == kind).unwrap();
    let points = rig.session.controller_curve(clip, MOD).unwrap();
    assert!(points.len() < 24, "{} points", points.len());
    for (frame, value) in &gesture {
        let got = lane(MOD).value_at(heard(*frame));
        assert!((got - f64::from(*value)).abs() <= THIN_TOLERANCE + 1e-6, "at frame {frame}: {got} for {value}");
    }
    // In time to the frame: the gesture's first and last reports are points.
    let frame = 1.0 / FRAMES_PER_BEAT;
    assert!((points[0].beat - heard(wheel_from)).abs() < 0.01 * frame, "{:?}", points[0]);
    assert!((points.last().unwrap().beat - heard(gesture[250].0)).abs() < 0.01 * frame);

    // The pedal, as steps where it was heard to move; and the expression
    // the take began with, from the take's first beat.
    let steps = rig.session.controller_curve(clip, PEDAL).unwrap();
    assert_eq!(steps.len(), 2);
    assert!((steps[0].beat - heard(pedal_down)).abs() < 0.01 * frame && steps[0].value == 1.0, "{steps:?}");
    assert!((steps[1].beat - heard(pedal_up)).abs() < 0.01 * frame && steps[1].value == 0.0, "{steps:?}");
    assert_eq!(rig.session.controller_curve(clip, EXPRESSION).unwrap(), [point(0.0, 0.75)]);

    // One take, one undo.
    assert!(rig.session.undo());
    assert!(rig.session.project().track(track).unwrap().clips.is_empty());
    assert!(!rig.session.can_undo() || rig.session.project().tracks.len() == 1);
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn an_instrument_that_is_given_notes_and_stays_silent_is_noticed() {
    let mut rig = Rig::new();
    let probe = rig.probe_track("Sax");
    let sine = rig.sine_track("Keys");
    clip(&mut rig.session, probe, (0.0, 8.0), &[note(60, 0.0, 6.0)], &[]);
    // A note that is long after the sound has died away: the sine is
    // given a short decay to nothing.
    let synth = rig.session.instrument(sine).unwrap();
    let decay = [("decay".to_string(), 0.05), ("sustain".to_string(), 0.0)];
    rig.session.set_device_params(synth, &decay).unwrap();
    clip(&mut rig.session, sine, (0.0, 8.0), &[note(60, 0.0, 6.0)], &[]);
    rig.run(2);
    assert_eq!(rig.session.silent_instrument(probe), None, "nothing has been played");

    rig.session.play();
    rig.run(80);
    assert_eq!(rig.session.silent_instrument(probe), None, "not within the first second");
    rig.run(40);
    let silent = rig.session.silent_instrument(probe).expect("a second of notes and no sound");
    assert_eq!(
        (silent.name.as_str(), silent.plugin, silent.device),
        ("Probe", true, rig.session.instrument(probe).unwrap())
    );
    assert!((1.0..1.3).contains(&silent.seconds), "{}", silent.seconds);
    assert_eq!(rig.session.silent_instrument(sine), None, "an instrument that rang out under its note has answered");

    // It is still so after the notes have ended, until something sounds.
    rig.session.stop();
    rig.run(4);
    assert!(rig.session.silent_instrument(probe).is_some());
    assert_eq!(rig.session.silent_instrument(TrackId(999)), None);

    // A built-in instrument held at no expression is silent too, and says
    // so until its expression is raised.
    let quiet = rig.sine_track("Quiet");
    rig.session.note_on(quiet, 69, 100);
    rig.session.send_controller(quiet, EXPRESSION, 0.0);
    rig.run(4);
    rig.session.note_off(quiet, 69);
    rig.session.note_on(quiet, 69, 100);
    rig.run(110);
    let silent = rig.session.silent_instrument(quiet).expect("expression at nothing");
    assert!(!silent.plugin);
    rig.session.send_controller(quiet, EXPRESSION, 1.0);
    rig.run(2);
    assert_eq!(rig.session.silent_instrument(quiet), None, "heard: nothing to say any more");
    rig.session.note_off(quiet, 69);
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn what_an_instrument_responds_to_is_known_for_the_built_in_ones() {
    let mut rig = Rig::new();
    let sine = rig.sine_track("Keys");
    let probe = rig.probe_track("Probe");
    let empty = rig.session.edit(|project| Ok(project.add_track("Empty"))).unwrap();
    assert_eq!(rig.session.controller_support(sine).responds_to(EXPRESSION), Some(true));
    assert_eq!(rig.session.controller_support(sine).responds_to(ControllerKind::BREATH), Some(false));
    assert_eq!(rig.session.controller_support(probe), ControllerSupport::Unknown, "a device that does not say");
    assert_eq!(rig.session.controller_support(empty), ControllerSupport::Unknown);
    assert_eq!(rig.session.instrument(empty), None);
    // An effect ahead of the instrument is not the instrument.
    rig.session.add_device(ChainRef::Track(sine), "builtin:eq", None, Some(0)).unwrap();
    let devices = &rig.session.project().track(sine).unwrap().devices;
    assert_eq!(rig.session.instrument(sine), Some(devices[1].id));
}

// ---- the audio --------------------------------------------------------------

const RATE: u32 = 48_000;

/// A track with a sine that starts and stops at once.
fn sine_track(project: &mut Project) -> TrackId {
    let track = project.add_track("Sine");
    let device = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    let params = &mut project.device_mut(device).unwrap().params;
    for (key, value) in [("osc1_wave", 0.0), ("attack", 0.001), ("release", 0.01), ("sustain", 1.0)] {
        params.insert(key.to_string(), value);
    }
    track
}

/// Eight beats of A4 in one long note, with `curves` in the clip.
fn swelling(curves: &[(ControllerKind, Vec<AutomationPoint>)], notes: &[Note]) -> Project {
    let mut project = Project::new("t");
    let track = sine_track(&mut project);
    let id = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
    let clip = project.clip_mut(id).unwrap();
    clip.add_notes(notes).unwrap();
    for (kind, points) in curves {
        clip.set_controller(*kind, points.clone()).unwrap();
    }
    project
}

fn rendered(project: &Project, start_beat: f64) -> Vec<f32> {
    let options = RenderOptions { sample_rate: RATE, start_beat, max_tail_seconds: 0.5, ..RenderOptions::default() };
    render(project, &BuiltinFactory, &options).unwrap().left
}

#[test]
fn a_swell_in_a_clip_is_heard_as_a_swell_without_steps() {
    let long = [note(69, 0.0, 8.0)];
    let plain = rendered(&swelling(&[], &long), 0.0);
    let swell = rendered(&swelling(&[(EXPRESSION, vec![point(0.0, 0.0), point(8.0, 1.0)])], &long), 0.0);
    // What the swell made of the plain note, frame by frame, wherever the
    // wave is far enough from zero to say.
    let loud = 0.5 * common::peak(&plain);
    let gains: Vec<(usize, f32)> = (4_800..190_000)
        .filter(|&frame| plain[frame].abs() > loud)
        .map(|frame| (frame, swell[frame] / plain[frame]))
        .collect();
    assert!(gains.len() > 100_000, "{}", gains.len());
    for &(frame, gain) in &gains {
        // Louder where the curve is higher: the square of the expression,
        // a few milliseconds behind.
        let expression = frame as f32 / 192_000.0;
        assert!((gain - expression * expression).abs() < 0.004, "at frame {frame}: {gain}");
    }
    for pair in gains.windows(2).filter(|pair| pair[1].0 == pair[0].0 + 1) {
        let step = pair[1].1 - pair[0].1;
        assert!((-1e-6..2e-5).contains(&step), "the level steps by {step} at frame {}", pair[1].0);
    }
    assert!(largest_step(&swell) <= largest_step(&plain));
}

#[test]
fn a_render_started_in_the_middle_of_a_crescendo_is_at_its_level_from_the_first_note() {
    // A note on every beat, short enough that none runs into the next.
    let notes: Vec<Note> = (0..8).map(|beat| note(69, beat as f64, 0.5)).collect();
    let project = swelling(&[(EXPRESSION, vec![point(0.0, 0.1), point(8.0, 1.0)])], &notes);
    let whole = rendered(&project, 0.0);
    for start in [2.0, 4.0, 7.0] {
        let from_there = rendered(&project, start);
        let offset = (start * 24_000.0) as usize;
        // The first note heard: the same, to the sample, as in the render
        // from the top. Nothing of it is louder or softer on the way to
        // the level the song has there.
        assert!(rms(&from_there[..12_000]) > 0.001, "something sounds from beat {start}");
        assert!(from_there[..12_000] == whole[offset..offset + 12_000], "from beat {start} it differs from the whole");
    }
}

#[test]
fn controllers_play_the_same_at_any_buffer_size_and_in_a_render() {
    let notes: Vec<Note> = (0..16).map(|step| note(57 + (step % 7) as u8, step as f64 * 0.5, 0.45)).collect();
    let curves = [
        (
            EXPRESSION,
            vec![
                point(0.0, 0.2),
                AutomationPoint { beat: 3.0, value: 1.0, shape: Segment::Curve(0.6) },
                point(8.0, 0.3),
            ],
        ),
        (BEND, vec![point(1.0, 0.0), point(1.5, 1.0), held(2.0, -0.5), point(5.0, 0.0)]),
        (PEDAL, vec![held(2.25, 1.0), held(3.75, 0.0), held(6.1, 1.0)]),
        (MOD, vec![point(4.0, 0.0), point(6.0, 1.0)]),
    ];
    let mut project = swelling(&curves, &notes);
    let clip = project.tracks[0].clips[0].id;
    // Looped and entered off the beat, so the curves wrap inside the clip.
    project.clip_mut(clip).unwrap().set_loop_length(Some(3.0)).unwrap();
    project.trim_clip_start(clip, 0.25).unwrap();
    project.set_loop(true, 1.0, 6.5).unwrap();

    let run = |block: usize, looping: bool| {
        let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
        let (graph, _) = Compiler {
            factory: &BuiltinFactory,
            bank: &mut bank,
            meters: &mut meters,
            audio: &audio,
            stand_ins: None,
            sample_rate: RATE as f64,
            purpose: trill_core::device::Purpose::Playing,
        }
        .compile(&project, looping);
        let mut processor = Processor::new(RATE as f64);
        drop(processor.swap_graph(graph));
        processor.play();
        let mut left = vec![0.0f32; 8 * RATE as usize];
        let mut right = left.clone();
        for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
            processor.process(l, r);
        }
        left
    };

    // Round the transport's loop, wrapping mid-gesture, whatever the buffer.
    let reference = run(512, true);
    assert!(rms(&reference) > 0.01);
    for block in [480, 37, 1, 4096] {
        assert!(run(block, true) == reference, "looping, block size {block} produced different audio");
    }
    // Straight through: a render is what playback is, to the bit.
    let played = run(441, false);
    let offline = rendered(&project, 0.0);
    assert!(played[..192_000] == offline[..192_000], "the render differs from playback");
    assert!(run(1, false)[..96_000] == played[..96_000]);
}
