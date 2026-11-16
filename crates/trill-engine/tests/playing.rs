//! Playing by hand and recording it, end to end: a session, an audio
//! device that exists only here, and the test as both the player and the
//! device's audio thread. See `rig` for how time is kept exact.

mod common;
mod rig;

use common::click_times;
use rig::*;
use std::sync::atomic::Ordering;
use trill_core::device::EventKind;
use trill_core::{DeviceId, TrackId};
use trill_engine::{RecordState, Session, TransportStatus};

fn instrument(session: &Session, track: TrackId) -> DeviceId {
    session.project().track(track).unwrap().devices[0].id
}

/// How far the one probe's clock is behind the host's, in frames.
fn probe_lag(rig: &Rig) -> f64 {
    rig.now() - rig.probes.frames.load(Ordering::Relaxed) as f64
}

#[test]
fn a_key_sounds_one_buffer_after_it_goes_down_wherever_in_the_buffer_that_was() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Keys");
    rig.session.set_selected_track(Some(track));
    rig.run(2);
    let lag = probe_lag(&rig);
    rig.probes.take();

    // Keys pressed at awkward moments: early and late in a buffer's time,
    // between frames, several in one buffer.
    let start = rig.now();
    let presses = [7.0, 481.0, 959.0, 1_203.4, 1_210.0, 4_000.25, 4_799.0];
    for (index, press) in presses.iter().enumerate() {
        rig.key(start + press, on(60 + index as u8));
    }
    rig.run(2);

    let seen = rig.probes.events();
    assert_eq!(seen.len(), presses.len(), "{seen:?}");
    for (index, (frame, kind)) in seen.iter().enumerate() {
        assert_eq!(*kind, on(60 + index as u8), "in the order they were played");
        let delay = *frame as f64 + lag - (start + presses[index]);
        assert!((delay - BUFFER as f64).abs() <= 0.5, "key {index} sounded {delay} frames after it went down");
    }

    // A key that goes down at the very moment a buffer is asked for is in
    // that buffer's last frame, a frame sooner than the rule has it.
    let boundary = rig.now();
    rig.key(boundary, off(60));
    rig.run(2);
    let seen = rig.probes.events();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0 as f64 + lag, boundary + BUFFER as f64 - 1.0);
    assert_eq!(rig.session.audio_info().latency_frames, LATENCY as u32);
}

#[test]
fn what_is_played_goes_to_the_armed_tracks_and_nothing_is_left_sounding() {
    let mut rig = Rig::new();
    let keys = rig.probe_track("Keys");
    let bass = rig.probe_track("Bass");
    let (on_keys, on_bass) = (instrument(&rig.session, keys), instrument(&rig.session, bass));
    rig.run(2);
    rig.probes.take();

    // What the probes were told after `act`, as (device, event).
    fn after(rig: &mut Rig, act: impl FnOnce(&mut Session)) -> Vec<(DeviceId, EventKind)> {
        act(&mut rig.session);
        rig.run(2);
        rig.probes.take().into_iter().map(|seen| (seen.device, seen.kind)).collect()
    }
    let play = |kind: EventKind| move |session: &mut Session| session.play_live(kind);

    // Nothing armed, nothing selected: nobody plays it.
    assert_eq!(after(&mut rig, play(on(59))), []);
    assert_eq!(after(&mut rig, play(off(59))), []);

    // The selected track, while none is armed.
    rig.session.set_selected_track(Some(keys));
    assert_eq!(after(&mut rig, play(on(60))), [(on_keys, on(60))]);

    // Arming another track moves the keyboard there. The key that is down
    // is let go of where it sounds, and when it comes up nobody is told:
    // the bass never had it.
    assert_eq!(after(&mut rig, |session| session.set_armed(bass, true).unwrap()), [(on_keys, off(60))]);
    assert_eq!(after(&mut rig, play(off(60))), []);
    assert_eq!(after(&mut rig, play(on(62))), [(on_bass, on(62))]);

    // Both armed: both play. The key that was down on the bass stays down.
    assert_eq!(after(&mut rig, |session| session.set_armed(keys, true).unwrap()), []);
    assert_eq!(after(&mut rig, play(on(64))), [(on_keys, on(64)), (on_bass, on(64))]);
    assert_eq!(after(&mut rig, play(off(62))), [(on_bass, off(62))]);

    // Disarming with a key, the pedal and the wheel held lets go of all of
    // them on that track, and only there.
    let bend = EventKind::PitchBend { value: 0.5 };
    assert_eq!(after(&mut rig, play(pedal(true))), [(on_keys, pedal(true)), (on_bass, pedal(true))]);
    assert_eq!(after(&mut rig, play(bend)), [(on_keys, bend), (on_bass, bend)]);
    assert_eq!(
        after(&mut rig, |session| session.set_armed(bass, false).unwrap()),
        [(on_bass, off(64)), (on_bass, pedal(false)), (on_bass, EventKind::PitchBend { value: 0.0 })]
    );

    // The keyboard's port disappears: everything it held is let go of.
    assert_eq!(
        after(&mut rig, |session| session.release_live()),
        [(on_keys, off(64)), (on_keys, pedal(false)), (on_keys, EventKind::PitchBend { value: 0.0 })]
    );
    assert_eq!(after(&mut rig, |session| session.release_live()), [], "once");

    // A note played through the session names its track and plays there
    // whatever is armed, and arming and disarming do not end it.
    assert_eq!(after(&mut rig, |session| session.note_on(bass, 40, 100)), [(on_bass, on(40))]);
    assert_eq!(after(&mut rig, |session| session.set_armed(keys, false).unwrap()), []);
    assert_eq!(after(&mut rig, |session| session.set_selected_track(None)), []);
    assert_eq!(after(&mut rig, |session| session.note_off(bass, 40)), [(on_bass, off(40))]);

    // Stop silences what is held by hand, even with the song not playing.
    rig.session.set_selected_track(Some(keys));
    assert_eq!(after(&mut rig, play(on(65))), [(on_keys, on(65))]);
    let stopped = after(&mut rig, |session| session.stop());
    assert!(stopped.contains(&(on_keys, EventKind::AllNotesOff)), "{stopped:?}");
    assert_eq!(after(&mut rig, play(off(65))), [], "the key coming up later has nothing left to end");

    // A track that is deleted is no longer played, and the session does
    // not mind a key held on it.
    assert_eq!(after(&mut rig, play(on(66))), [(on_keys, on(66))]);
    rig.session.edit(|project| project.remove_track(keys).map(|_| ())).unwrap();
    assert_eq!(after(&mut rig, play(off(66))), []);
    assert!(rig.session.live_tracks().is_empty());
    assert_eq!(rig.session.audio_faults(), 0);
}

#[test]
fn a_take_is_written_where_it_was_heard_and_is_one_undo_step() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Keys");
    rig.session.set_armed(track, true).unwrap();
    rig.session.set_count_in(0);
    rig.run(2);
    rig.probes.take();
    let before = rig.session.project().clone();
    let undo_before = rig.session.undo_step();
    assert_eq!(rig.session.record_status().state, RecordState::Idle);

    // With no count-in the song starts rolling in the next buffer.
    let rolling = rig.now();
    rig.session.record().unwrap();
    assert!(rig.session.is_recording());
    // The engine is LATENCY frames ahead of the speakers: this is when the
    // player hears `beat`, and plays to it.
    let heard = |beat: f64| rolling + beat * FRAMES_PER_BEAT + LATENCY;
    rig.key(heard(1.0), on(60));
    rig.key(heard(1.5), on(64));
    rig.key(heard(2.0), off(60));
    rig.key(heard(2.75) + 133.7, off(64));
    // The pedal holds a note past its key.
    rig.key(heard(3.0), on(72));
    rig.key(heard(3.25), pedal(true));
    rig.key(heard(3.5), off(72));
    rig.key(heard(4.5), pedal(false));
    // And one key is still down when recording stops.
    rig.key(heard(5.0), on(67));

    rig.session.housekeeping();
    let status = rig.session.record_status();
    assert_eq!(status.state, RecordState::Recording);
    assert_eq!((status.tracks, status.start_beat, status.pass), (vec![track], Some(0.0), 0));
    assert_eq!(status.notes, 3, "the notes finished so far");
    assert_eq!(status.clips.len(), 1, "the clip is there while it is being recorded");
    assert!(rig.session.transport().playing);

    rig.run_until(heard(6.0));
    rig.session.stop();
    assert!(!rig.session.is_recording());
    let take = rig.session.last_take().to_vec();
    assert_eq!(take.len(), 1);
    assert_eq!((take[0].track, take[0].notes, take[0].created), (track, 4, true));
    let off_grid = 133.7 / FRAMES_PER_BEAT;
    assert_notes(
        &timeline(&rig.session, take[0].clip),
        &[(60, 1.0, 1.0), (64, 1.5, 1.25 + off_grid), (72, 3.0, 1.5), (67, 5.0, 1.0)],
    );
    let clip = rig.session.project().clip(take[0].clip).unwrap();
    assert_eq!((clip.start, clip.length), (0.0, 8.0), "whole bars around what was played");
    assert_eq!(clip.notes()[0].velocity, 100);

    // The instrument was told to let go of the key that was still down.
    rig.run(2);
    let told = rig.probes.events();
    assert!(told.iter().any(|(_, kind)| *kind == EventKind::AllNotesOff), "{told:?}");
    assert!(!rig.session.transport().playing);

    // One step back and the take never happened; one forward and it did.
    assert!(rig.session.undo());
    assert_eq!(rig.session.project(), &before);
    assert_eq!(rig.session.undo_step(), undo_before);
    assert!(rig.session.redo());
    assert_eq!(timeline(&rig.session, take[0].clip).len(), 4);
}

#[test]
fn a_count_in_clicks_a_bar_and_the_song_starts_on_the_sample() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Keys");
    rig.session.set_armed(track, true).unwrap();
    rig.session.seek(8.0);
    rig.run(2);
    assert_eq!(rig.session.count_in(), 1, "a bar, unless changed");
    assert!(!rig.session.metronome());
    rig.host.keep_output();

    let asked = rig.now();
    rig.session.record().unwrap();
    let rolling = asked + 4.0 * FRAMES_PER_BEAT;
    let heard = |beat: f64| rolling + (beat - 8.0) * FRAMES_PER_BEAT + LATENCY;

    // Warming up during the count-in is not part of the take.
    rig.key(asked + 10_000.0, on(48));
    rig.key(asked + 20_000.0, off(48));
    rig.run_until(asked + 2.0 * FRAMES_PER_BEAT);
    assert_eq!(rig.session.record_status().state, RecordState::CountIn);
    assert_eq!(rig.session.transport(), TransportStatus { playing: false, position_beats: 8.0 });
    // The first note a tenth of a beat early is.
    rig.key(heard(7.9), on(60));
    rig.key(heard(9.0), off(60));
    rig.key(heard(9.0), on(62));
    rig.key(heard(9.5), off(62));
    rig.run_until(heard(10.0));
    assert_eq!(rig.session.record_status().state, RecordState::Recording);
    assert!(rig.session.transport().playing);
    rig.session.stop();

    let take = rig.session.last_take().to_vec();
    assert_notes(&timeline(&rig.session, take[0].clip), &[(60, 8.0, 1.0), (62, 9.0, 0.5)]);
    let clip = rig.session.project().clip(take[0].clip).unwrap();
    assert_eq!((clip.start, clip.length), (8.0, 4.0));

    // Four clicks, on the song's own beats, and with the metronome off
    // none once the song is rolling.
    let (left, _) = rig.host.take_output();
    let clicks = click_times(&left, SR, 0.1);
    assert_eq!(clicks.len(), 4, "{clicks:?}");
    for (index, click) in clicks.iter().enumerate() {
        assert!((click - index as f64 * 0.5).abs() < 0.002, "click {index} at {click} s");
    }
    assert!(left.len() as f64 > 4.5 * FRAMES_PER_BEAT);

    // No count-in when the song is already playing, or when it is set to
    // none; and asking to play ends a count-in early.
    rig.session.seek(0.0);
    rig.run(2);
    let asked = rig.now();
    rig.session.record().unwrap();
    rig.run_until(asked + 1_000.0);
    rig.session.play();
    rig.run(2);
    rig.session.housekeeping();
    let status = rig.session.record_status();
    assert_eq!((status.state, status.start_beat), (RecordState::Recording, Some(0.0)));
    rig.session.stop();
    assert!(rig.session.last_take().is_empty(), "nothing was played");
}

#[test]
fn looping_layers_each_pass_onto_the_last_and_the_earlier_ones_are_heard() {
    let mut rig = Rig::new();
    let track = rig.probe_track("Keys");
    rig.session.edit(|project| project.set_loop(true, 0.0, 4.0)).unwrap();
    rig.session.set_armed(track, true).unwrap();
    rig.session.set_count_in(0);
    rig.run(2);
    let lag = probe_lag(&rig);
    rig.probes.take();
    let depth_before = rig.session.undo_step();

    let rolling = rig.now();
    rig.session.record().unwrap();
    let pass = 4.0 * FRAMES_PER_BEAT;
    let heard = |round: f64, beat: f64| rolling + round * pass + beat * FRAMES_PER_BEAT + LATENCY;
    rig.key(heard(0.0, 1.0), on(60));
    rig.key(heard(0.0, 1.5), off(60));
    // Held across the loop point.
    rig.key(heard(0.0, 3.5), on(48));
    rig.key(heard(1.0, 0.5), off(48));
    rig.key(heard(1.0, 2.0), on(64));
    rig.key(heard(1.0, 2.5), off(64));
    rig.run_until(heard(2.0, 2.5));
    rig.session.housekeeping();
    assert_eq!(rig.session.record_status().pass, 2);
    rig.session.stop();

    let take = rig.session.last_take().to_vec();
    assert_eq!(take.len(), 1);
    assert_notes(&timeline(&rig.session, take[0].clip), &[(60, 1.0, 0.5), (64, 2.0, 0.5), (48, 3.5, 0.5)]);
    let clip = rig.session.project().clip(take[0].clip).unwrap();
    assert_eq!((clip.start, clip.length), (0.0, 4.0), "the clip is the loop");

    // What the instrument was told: the keys as they were played, and from
    // the second pass on the notes of the passes before, from the clip.
    let told = rig.probes.events();
    let at = |round: f64, beat: f64| (rolling + round * pass + beat * FRAMES_PER_BEAT - lag).round() as u64;
    let times = |kind: EventKind| -> Vec<u64> {
        told.iter().filter(|(_, seen)| *seen == kind).map(|(frame, _)| *frame).collect()
    };
    // A key sounds a buffer after it goes down (give or take the frame a
    // key on a buffer's edge gains).
    let sounds = |pressed: f64| (pressed - lag + BUFFER as f64).round() as u64;
    let first = times(on(60));
    assert_eq!(first.len(), 3, "{told:?}");
    assert!(first[0].abs_diff(sounds(heard(0.0, 1.0))) <= 1, "{told:?}");
    assert_eq!(first[1..], [at(1.0, 1.0), at(2.0, 1.0)], "played back from the clip in each later pass");
    let second = times(on(64));
    assert_eq!(second.len(), 2, "played in the second pass, played back in the third: {told:?}");
    assert_eq!(second[1], at(2.0, 2.0));
    // The key held across the loop point was not cut off by the loop
    // coming round: it ended when it was let go.
    let released = times(off(48));
    assert!(released[0].abs_diff(sounds(heard(1.0, 0.5))) <= 1, "{told:?}");
    let wraps = times(EventKind::AllNotesOff);
    assert!(!wraps.contains(&at(1.0, 0.0)), "nothing was cut as the loop came round under a held key: {told:?}");

    // All of it is one undo step.
    assert!(rig.session.undo());
    assert!(rig.session.project().track(track).unwrap().clips.is_empty());
    assert_eq!(rig.session.undo_step(), depth_before);
}

#[test]
fn notes_played_through_the_session_are_recorded_like_a_keyboards() {
    let mut rig = Rig::new();
    let keys = rig.probe_track("Keys");
    let bass = rig.probe_track("Bass");
    rig.session.set_armed(keys, true).unwrap();
    rig.session.set_armed(bass, true).unwrap();
    rig.run(2);

    // Punching in while the song plays: no count-in, from where it is.
    let playing = rig.now();
    rig.session.play();
    rig.run_until(playing + 5.5 * FRAMES_PER_BEAT);
    rig.session.record().unwrap();
    let heard = |beat: f64| playing + beat * FRAMES_PER_BEAT + LATENCY;

    // `note_on` names its track: only the bass, though both are armed.
    rig.run_until(heard(6.0));
    rig.session.note_on(bass, 36, 90);
    rig.run_until(heard(6.5));
    rig.session.note_off(bass, 36);
    // A keyboard plays both.
    rig.key(heard(7.0), on(60));
    rig.key(heard(7.25), off(60));
    rig.run_until(heard(9.0));
    rig.session.stop();

    let take = rig.session.last_take().to_vec();
    assert_eq!(take.iter().map(|clip| clip.track).collect::<Vec<_>>(), [keys, bass]);
    assert_notes(&timeline(&rig.session, take[0].clip), &[(60, 7.0, 0.25)]);
    assert_notes(&timeline(&rig.session, take[1].clip), &[(36, 6.0, 0.5), (60, 7.0, 0.25)]);
    for clip in &take {
        let clip = rig.session.project().clip(clip.clip).unwrap();
        assert_eq!((clip.start, clip.length), (4.0, 8.0), "from the bar line before the punch-in");
    }
    assert_eq!(rig.session.project().clip(take[1].clip).unwrap().notes()[0].velocity, 90);

    // One step for the take, both clips.
    assert!(rig.session.undo());
    assert!(rig.session.project().tracks.iter().all(|track| track.clips.is_empty()));
}

#[test]
fn a_take_survives_what_happens_around_it() {
    let mut rig = Rig::new();
    let keys = rig.probe_track("Keys");
    rig.session.set_armed(keys, true).unwrap();
    rig.session.set_count_in(0);
    rig.run(2);

    // Recording twice is recording once; nothing armed is an error that
    // says what to do.
    rig.session.record().unwrap();
    rig.session.record().unwrap();
    rig.run(4);

    // Another project is opened in the middle of the take.
    rig.session.new_project();
    assert!(!rig.session.is_recording());
    assert!(rig.session.last_take().is_empty());
    rig.run(4);
    rig.session.housekeeping();
    assert!(!rig.session.transport().playing);
    assert!(rig.session.armed().is_empty(), "the armed track was the other project's");
    let error = rig.session.record().unwrap_err().to_string();
    assert!(error.contains("arm a track first"), "{error}");
    // What is edited now is a step of its own: the take's undo group did
    // not stay open.
    let track = rig.probe_track("Again");
    let depth = rig.session.undo_step();
    rig.session.edit(|project| project.set_track_volume(track, -6.0)).unwrap();
    assert_ne!(rig.session.undo_step(), depth);
    assert!(rig.session.undo());
    assert_eq!(rig.session.undo_step(), depth);

    // The audio device is unplugged in the middle of a take: the take ends
    // with what was played, and the key that was down ends with it.
    rig.session.set_armed(track, true).unwrap();
    rig.run(2);
    let rolling = rig.now();
    rig.session.record().unwrap();
    let heard = |beat: f64| rolling + beat * FRAMES_PER_BEAT + LATENCY;
    rig.key(heard(1.0), on(60));
    rig.key(heard(1.5), off(60));
    rig.key(heard(2.0), on(62));
    rig.run_until(heard(3.0));
    rig.host.remove_device("Speakers");
    rig.session.housekeeping();
    assert!(!rig.session.is_recording());
    let take = rig.session.last_take().to_vec();
    assert_eq!(take.len(), 1);
    let notes = timeline(&rig.session, take[0].clip);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert_eq!((notes[0].0, notes[1].0), (60, 62));
    assert!((notes[1].1 - 2.0).abs() < 1e-6 && notes[1].2 > 0.5, "{notes:?}");
    assert_eq!(rig.session.audio_info().device, None, "no device is left");
}
