//! The rule that holds the microphone: **the input is opened for a track
//! only by the user's own hand, in this run of the program and in this
//! song.**
//!
//! A session is asked things by threads that say they are the user's hand
//! ([`trill_engine::hand::by_hand`]) and by threads that say nothing: the
//! door of an assistant that reads the web, where there is nobody to ask.
//! Here the test's own thread is the user (the rig says so for it), and
//! what is asked at the door is asked on another thread, as it is in the
//! program.
//!
//! What is held is not a flag of the session's: it is how often the
//! device was asked for its input, counted by the scripted host.

mod rig;

use rig::*;
use trill_core::{Monitoring, TrackId, TrackInput};
use trill_engine::host::AudioConfig;
use trill_engine::host::scripted::{ScriptedHost, ScriptedInput};
use trill_engine::{Patience, Session};

fn studio() -> (Rig, tempfile::TempDir) {
    let host = ScriptedHost::with_speakers();
    host.add_input(ScriptedInput::new("Microphone"));
    let folder = tempfile::tempdir().unwrap();
    let mut rig = Rig::with(host, AudioConfig::default(), Patience::default());
    rig.session.keep_unsaved_recordings_in(folder.path().join("unsaved"));
    rig.session.set_count_in(0);
    (rig, folder)
}

/// Does `act` to the session on a thread that says nothing of whose hand
/// it is: the assistant's door.
fn at_the_door<T: Send>(session: &mut Session, act: impl FnOnce(&mut Session) -> T + Send) -> T {
    std::thread::scope(|scope| scope.spawn(|| act(session)).join().unwrap())
}

/// A round of the session's chores and a few buffers: what was let has
/// opened the input by now, and what was not has not.
fn settle(rig: &mut Rig) {
    rig.session.housekeeping();
    rig.run(4);
    rig.session.housekeeping();
}

/// How often the device was asked for its input.
fn opens(rig: &Rig) -> usize {
    rig.host.input_requests().len()
}

fn input(monitor: Monitoring) -> Option<TrackInput> {
    Some(TrackInput { channel: 1, stereo: false, monitor })
}

fn track(session: &mut Session, name: &str, records: Option<TrackInput>) -> TrackId {
    let track = session.edit(|project| Ok(project.add_track(name))).unwrap();
    session.edit(|project| project.set_track_input(track, records)).unwrap();
    track
}

#[test]
fn nothing_asked_at_the_door_opens_the_input_by_any_route() {
    let (mut rig, folder) = studio();
    let (voice, keys) = at_the_door(&mut rig.session, |session| {
        (track(session, "Voice", input(Monitoring::Off)), track(session, "Keys", None))
    });

    // Arming a track that records an input.
    let refused = at_the_door(&mut rig.session, |session| session.set_armed(voice, true)).unwrap_err().to_string();
    assert!(refused.contains("arming it opens the microphone") && refused.contains("arm button"), "{refused}");
    assert!(rig.session.armed().is_empty());
    settle(&mut rig);

    // Arming one that records a keyboard, which is allowed, and then
    // giving it an input: it is disarmed, and that is said.
    at_the_door(&mut rig.session, |session| {
        session.set_armed(keys, true).unwrap();
        session.edit(|project| project.set_track_input(keys, input(Monitoring::Armed))).unwrap();
    });
    assert!(rig.session.armed().is_empty(), "an armed track that came to record an input is disarmed");
    let said = rig.session.take_input_note().expect("it is said");
    assert!(said.contains("'Keys' was disarmed") && said.contains("by hand"), "{said}");
    settle(&mut rig);

    // The same by way of undo and redo: an input given, taken back, the
    // track armed while it has none, and the input brought back.
    at_the_door(&mut rig.session, |session| {
        assert!(session.undo());
        assert!(session.project().track(keys).unwrap().input.is_none());
        session.set_armed(keys, true).unwrap();
        assert!(session.redo());
    });
    assert!(rig.session.project().track(keys).unwrap().input.is_some() && rig.session.armed().is_empty());
    assert!(rig.session.take_input_note().unwrap().contains("'Keys' was disarmed"));
    at_the_door(&mut rig.session, |session| {
        assert!(session.undo());
        session.set_armed(keys, true).unwrap();
        assert!(session.redo() && session.undo() && session.redo());
    });
    assert!(rig.session.armed().is_empty());
    settle(&mut rig);

    // Monitoring that is always on, on a track that is not armed: the
    // song says so, and the input is held.
    at_the_door(&mut rig.session, |session| {
        session.edit(|project| project.set_track_input(voice, input(Monitoring::Always))).unwrap();
    });
    assert!(rig.session.input_held(voice));
    assert!(!rig.session.input_status().needed, "what is held needs no input");
    let said = rig.session.take_input_note().expect("it is said");
    assert!(said.contains("the input of 'Voice' is held") && said.contains("monitor switch"), "{said}");
    // And the door cannot let it go.
    let refused = at_the_door(&mut rig.session, |session| session.let_input_be_heard(voice)).unwrap_err();
    assert!(refused.to_string().contains("the user does that themselves"), "{refused}");
    assert!(rig.session.input_held(voice));
    settle(&mut rig);

    // A track that is new: a duplicate of one that is always heard.
    let twin = at_the_door(&mut rig.session, |session| session.edit(|project| project.duplicate_track(voice)).unwrap());
    assert!(rig.session.input_held(twin) && rig.session.input_held(voice));
    settle(&mut rig);

    // Looking at the level, and the measurement.
    let refused = at_the_door(&mut rig.session, |session| session.watch_input(true)).unwrap_err().to_string();
    assert!(refused.contains("opens the microphone"), "{refused}");
    let refused = at_the_door(&mut rig.session, |session| session.measure_latency(1)).unwrap_err().to_string();
    assert!(refused.contains("opens the microphone") && refused.contains("the user runs it"), "{refused}");
    settle(&mut rig);

    // Record, with nothing armed: nothing is armed for it.
    let refused = at_the_door(&mut rig.session, |session| session.record()).unwrap_err().to_string();
    assert!(refused.contains("no track is armed"), "{refused}");
    assert!(rig.session.armed().is_empty());

    // A song that says all of it, opened: begun anew, read from a file,
    // and recovered from a copy.
    let song = folder.path().join("Song.trill");
    at_the_door(&mut rig.session, |session| {
        session.save(&song).unwrap();
        session.set_armed(keys, false).unwrap();
        session.new_project();
        session.load(&song).unwrap();
    });
    assert!(rig.session.input_held(voice) && rig.session.input_held(twin));
    settle(&mut rig);
    at_the_door(&mut rig.session, |session| session.load_recovered(&song, None).unwrap());
    assert!(rig.session.input_held(voice));
    settle(&mut rig);

    assert_eq!(opens(&rig), 0, "the device was asked for its input: {:?}", rig.host.input_requests());
    assert!(rig.host.capturing().is_none());
}

#[test]
fn the_users_hand_opens_the_input_and_what_no_hand_asked_for_is_held() {
    let (mut rig, folder) = studio();
    let voice = track(&mut rig.session, "Voice", input(Monitoring::Off));
    let keys = track(&mut rig.session, "Keys", None);
    settle(&mut rig);
    assert_eq!(opens(&rig), 0, "a track that records an input opens nothing by being there");

    // Arming a track as it then records an input.
    rig.session.set_armed(voice, true).unwrap();
    settle(&mut rig);
    assert_eq!(opens(&rig), 1);
    assert!(rig.host.capturing().is_some());
    rig.session.set_armed(voice, false).unwrap();
    settle(&mut rig);
    assert!(rig.host.capturing().is_none(), "disarmed, nothing needs it");

    // Giving an armed track its input; and bringing it back by the
    // hand's own undo and redo.
    rig.session.set_armed(keys, true).unwrap();
    rig.session.edit(|project| project.set_track_input(keys, input(Monitoring::Armed))).unwrap();
    assert!(rig.session.is_armed(keys) && !rig.session.input_held(keys));
    settle(&mut rig);
    assert_eq!(opens(&rig), 2);
    assert!(rig.session.undo());
    settle(&mut rig);
    assert!(rig.host.capturing().is_none());
    assert!(rig.session.redo());
    assert!(rig.session.is_armed(keys) && !rig.session.input_held(keys), "the hand's redo is the hand's");
    settle(&mut rig);
    assert_eq!(opens(&rig), 3);
    rig.session.set_armed(keys, false).unwrap();
    settle(&mut rig);

    // Switching monitoring on.
    rig.session.edit(|project| project.set_track_input(voice, input(Monitoring::Always))).unwrap();
    assert!(!rig.session.input_held(voice));
    settle(&mut rig);
    assert_eq!(opens(&rig), 4);
    assert!(rig.session.take_input_note().is_none(), "nothing was held, so nothing is said");

    // A track that is new is held, whoever made it; one click lets it go.
    let twin = rig.session.edit(|project| project.duplicate_track(voice)).unwrap();
    assert!(rig.session.input_held(twin) && !rig.session.input_held(voice));
    assert!(rig.session.take_input_note().unwrap().contains("is held, not heard"));
    rig.session.let_input_be_heard(twin).unwrap();
    assert!(!rig.session.input_held(twin));

    // What the door switches on is held, and stays so through what the
    // hand does next that has nothing to do with it; the click lets it go.
    rig.session.edit(|project| project.set_track_input(voice, input(Monitoring::Off))).unwrap();
    rig.session.edit(|project| project.set_track_input(twin, None)).unwrap();
    settle(&mut rig);
    assert!(rig.host.capturing().is_none());
    at_the_door(&mut rig.session, |session| {
        session.edit(|project| project.set_track_input(voice, input(Monitoring::Always))).unwrap();
    });
    rig.session.edit(|project| project.set_track_volume(voice, -6.0)).unwrap();
    assert!(rig.session.input_held(voice));
    settle(&mut rig);
    assert_eq!(opens(&rig), 4);
    rig.session.let_input_be_heard(voice).unwrap();
    settle(&mut rig);
    assert_eq!(opens(&rig), 5);

    // The door cannot make more of what the hand let: a track the hand
    // armed, heard while it is armed, turned at the door into one that is
    // always heard, is held; disarmed, nothing needs the input.
    rig.session.edit(|project| project.set_track_input(voice, input(Monitoring::Armed))).unwrap();
    rig.session.set_armed(voice, true).unwrap();
    assert!(!rig.session.input_held(voice), "arming by hand lets a track be heard while it is armed");
    at_the_door(&mut rig.session, |session| {
        session.edit(|project| project.set_track_input(voice, input(Monitoring::Always))).unwrap();
    });
    assert!(rig.session.input_held(voice) && rig.session.is_armed(voice));
    rig.session.set_armed(voice, false).unwrap();
    settle(&mut rig);
    assert!(rig.session.input_held(voice) && rig.host.capturing().is_none());

    // Another song: arming does not carry into it, whatever numbers its
    // tracks have, and what it says of always is held for the user too.
    rig.session.set_armed(voice, true).unwrap();
    let song = folder.path().join("Song.trill");
    rig.session.save(&song).unwrap();
    let before = opens(&rig);
    rig.session.load(&song).unwrap();
    assert!(rig.session.armed().is_empty(), "nothing is armed in a song that was just opened");
    assert!(rig.session.input_held(voice));
    assert!(rig.session.take_input_note().unwrap().contains("held"));
    settle(&mut rig);
    assert!(rig.host.capturing().is_none());
    assert_eq!(opens(&rig), before);
    // The one click.
    rig.session.let_input_be_heard(voice).unwrap();
    settle(&mut rig);
    assert!(rig.host.capturing().is_some());
    // And a song begun anew has nothing armed either.
    rig.session.new_project();
    assert!(rig.session.armed().is_empty());
    settle(&mut rig);
    assert!(rig.host.capturing().is_none());
}

#[test]
fn a_take_heard_in_place_and_a_level_looked_at_open_nothing_at_the_door() {
    let (mut rig, _folder) = studio();
    let voice = track(&mut rig.session, "Voice", input(Monitoring::Off));
    // The user records a take, and disarms.
    rig.session.set_armed(voice, true).unwrap();
    settle(&mut rig);
    rig.session.record().unwrap();
    for _ in 0..10 {
        rig.run(20);
        rig.session.housekeeping();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    rig.session.stop();
    rig.session.set_armed(voice, false).unwrap();
    settle(&mut rig);
    assert!(rig.host.capturing().is_none());
    let take = rig.session.project().track(voice).unwrap().takes[0].clip.id;
    let before = opens(&rig);

    // The door hears it in place, looks at the input as it stands, and
    // asks how long sound takes to go round: none of it opens anything.
    at_the_door(&mut rig.session, |session| {
        session.hear_take(Some(take)).unwrap();
        assert!(session.input_status().open.is_none());
        assert!(session.input_levels().is_empty());
        assert!(session.recording_latency().is_none());
        session.hear_take(None).unwrap();
        session.play();
    });
    settle(&mut rig);
    assert_eq!(opens(&rig), before);
}
