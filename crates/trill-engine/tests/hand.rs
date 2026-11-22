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
//!
//! A song that others hold too brings routes of its own: what another
//! program changes arrives on a thread that is nobody's hand, a song is
//! handed over when one joins, and a clash is answered by an edit that
//! anyone may ask for, the assistant's door too. The last test here
//! tries each of them.

mod rig;

use rig::*;
use trill_core::merge::{Change, Op, Said, Stamp, TrackOp, TrackSet};
use trill_core::project::AudioContent;
use trill_core::{ChainRef, Monitoring, TrackId, TrackInput};
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

    // The user's own hand takes the door's step back and brings it back.
    // The step was not the hand's: what it brings stays held, as it was
    // when the door made it, and that is said again.
    assert!(rig.session.undo(), "the door's step is taken back by hand");
    assert!(!rig.session.input_held(voice), "there is nothing to hold: the song no longer says it is heard");
    assert!(rig.session.redo(), "and brought back by hand");
    assert!(rig.session.input_held(voice), "what the step brings is held all the same");
    assert!(!rig.session.input_status().needed);
    let said = rig.session.take_input_note().expect("it is said");
    assert!(said.contains("the input of 'Voice' is held"), "{said}");
    settle(&mut rig);
    // The same with a track the hand arms in between: the door gives a
    // track an input, the hand takes that back, arms the track while it
    // records a keyboard, and brings the door's step back. The track is
    // disarmed.
    let lead = at_the_door(&mut rig.session, |session| {
        let lead = track(session, "Lead", None);
        session.edit(|project| project.set_track_input(lead, input(Monitoring::Armed))).unwrap();
        lead
    });
    assert!(rig.session.undo());
    rig.session.set_armed(lead, true).unwrap();
    assert!(rig.session.redo());
    assert!(rig.session.project().track(lead).unwrap().input.is_some() && rig.session.armed().is_empty());
    assert!(rig.session.take_input_note().unwrap().contains("'Lead' was disarmed"));
    settle(&mut rig);
    assert_eq!(opens(&rig), 0, "a hand's redo of the door's step opened the input");

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

    // The device that is recorded from, with nothing live: the door may
    // change it, and nothing is opened by that.
    rig.host.add_input(ScriptedInput::new("Other"));
    at_the_door(&mut rig.session, |session| {
        session.set_audio_config(AudioConfig { input: Some("Other".into()), ..AudioConfig::default() });
        assert_eq!(session.audio_config().input.as_deref(), Some("Other"));
        session.set_audio_config(AudioConfig::default());
    });
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

    // The device that is recorded from, while the input is live: the door
    // cannot put another microphone in its place; the hand can, and what
    // was live stays live on the device it chose.
    rig.host.add_input(ScriptedInput::new("Other"));
    rig.session.let_input_be_heard(voice).unwrap();
    settle(&mut rig);
    assert_eq!(rig.host.capturing().map(|format| format.device), Some("Microphone".to_string()));
    at_the_door(&mut rig.session, |session| {
        session.set_audio_config(AudioConfig { input: Some("Other".into()), ..AudioConfig::default() });
    });
    settle(&mut rig);
    assert_eq!(rig.session.audio_config().input, None, "the device is left as it was");
    assert_eq!(rig.host.capturing().map(|format| format.device), Some("Microphone".to_string()));
    assert!(rig.session.take_input_note().unwrap().contains("the device that is recorded from was left as it is"));
    rig.session.set_audio_config(AudioConfig { input: Some("Other".into()), ..AudioConfig::default() });
    settle(&mut rig);
    assert_eq!(rig.host.capturing().map(|format| format.device), Some("Other".to_string()), "what is live stays live");
    assert!(!rig.session.input_held(voice));
    rig.session.set_audio_config(AudioConfig::default());
    settle(&mut rig);

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

/// Another program in the same song: a session of its own, with no audio
/// device.
fn someone_else() -> Session {
    let factory = std::sync::Arc::new(trill_dsp::BuiltinFactory);
    let mut session = Session::new(factory, trill_engine::AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    session.hear_changes(true);
    session
}

/// What `from` changed since it was last asked reaches the user's
/// session as the wire hands it over: on a thread that is nobody's hand.
fn arrives(from: &mut Session, rig: &mut Rig, by: &str) {
    let (said, floors, replica) = (from.take_said(), from.floors(), from.replica());
    at_the_door(&mut rig.session, |session| {
        session.merge(by, said, Some(&floors), replica).expect("what a session says is taken");
    });
    settle(rig);
}

/// What the user's session changed reaches the other program.
fn leaves(rig: &mut Rig, to: &mut Session) {
    let (said, floors, replica) = (rig.session.take_said(), rig.session.floors(), rig.session.replica());
    to.merge("Ada", said, Some(&floors), replica).expect("what a session says is taken");
}

/// A change as a program that is no Trill might say it: with a stamp
/// later than anything this session knows of.
fn planted(session: &Session, ops: Vec<Op>) -> Vec<Said> {
    let clock = session.floors().0.values().copied().max().unwrap_or(0) + 1;
    vec![Said { change: Change { stamp: Stamp(clock, 77_777), ops }, knew: None }]
}

#[test]
fn what_another_program_does_in_a_shared_song_opens_no_input_by_any_route() {
    let (mut rig, _folder) = studio();
    // The user's song: a track that records the microphone, and one that
    // keeps takes. Shared, and someone joins: a program of their own that
    // is handed the song.
    let voice = track(&mut rig.session, "Voice", input(Monitoring::Armed));
    let bass = track(&mut rig.session, "Bass", None);
    let verse = rig.session.edit(|song| song.add_midi_clip(voice, "Verse", 0.0, 8.0)).unwrap();
    rig.session.hear_changes(true);
    rig.session.share();
    let mut ben = someone_else();
    ben.take_song(&rig.session.whole_song()).unwrap();
    rig.session.take_said();
    assert!(ben.project().track(voice).unwrap().input.is_none(), "which input a track records is not handed over");
    settle(&mut rig);
    assert_eq!(opens(&rig), 0, "sharing a song opens nothing");

    // The user arms the track by hand: the one opening there has been.
    rig.session.set_armed(voice, true).unwrap();
    settle(&mut rig);
    let mut by_hand = 1;
    assert_eq!(opens(&rig), by_hand);
    assert!(rig.host.capturing().is_some());

    // 1. A change from the other program, to that track and beside it:
    // what the hand opened stays open, and nothing is opened again.
    ben.edit(|song| song.set_track_volume(voice, -6.0)).unwrap();
    let theirs = ben.edit(|song| Ok(song.add_track("Theirs"))).unwrap();
    arrives(&mut ben, &mut rig, "Ben");
    assert_eq!(rig.session.project().track(voice).unwrap().volume_db, -6.0);
    assert_eq!(opens(&rig), by_hand);
    assert!(rig.session.is_armed(voice) && rig.host.capturing().is_some());

    // 2. The other program removes the armed track, and brings it back.
    // With the track gone nothing records the input; with it back the
    // input is not opened for it again: no hand armed the track that is
    // there now.
    ben.edit(|song| song.remove_track(voice).map(drop)).unwrap();
    arrives(&mut ben, &mut rig, "Ben");
    assert!(rig.session.project().track(voice).is_err());
    assert!(rig.host.capturing().is_none(), "nothing records the input any more");
    assert!(ben.undo());
    arrives(&mut ben, &mut rig, "Ben");
    assert!(rig.session.project().track(voice).is_ok(), "the track is here again");
    assert_eq!(opens(&rig), by_hand, "a track that someone else brought back opened the input");
    assert!(rig.host.capturing().is_none() && !rig.session.is_armed(voice));
    let _ = rig.session.take_input_note();

    // 3. What no Trill says: a track given an input that is always heard,
    // one that is there and one that is made with it. Refused or taken,
    // neither opens anything.
    let always = input(Monitoring::Always);
    let given = planted(&rig.session, vec![Op::Track(theirs, TrackOp::Set(TrackSet::Input(always)))]);
    let came = at_the_door(&mut rig.session, |session| session.merge("Mallory", given, None, None).is_ok());
    settle(&mut rig);
    let heard = |rig: &Rig, id: TrackId| rig.session.project().track(id).is_ok_and(|track| track.input.is_some());
    assert!(!heard(&rig, theirs) || rig.session.input_held(theirs), "taken: {came}");
    let mut made = rig.session.project().track(theirs).unwrap().clone();
    (made.id, made.name, made.input) = (TrackId(77_777_000_001), "Planted".to_owned(), always);
    let whole = planted(&rig.session, vec![Op::Track(made.id, TrackOp::Whole(Box::new(made.clone()), 9.5))]);
    let came = at_the_door(&mut rig.session, |session| session.merge("Mallory", whole, None, None).is_ok());
    settle(&mut rig);
    assert!(!heard(&rig, made.id) || rig.session.input_held(made.id), "taken: {came}");
    assert_eq!(opens(&rig), by_hand, "what another program said opened the input");
    assert!(rig.host.capturing().is_none());
    let _ = rig.session.take_input_note();
    // (Ben hears of what was planted like anyone.)
    leaves(&mut rig, &mut ben);

    // 4. They work apart and meet again: each is handed what it lacks of
    // the other's, as when a connection that was lost is back. The user
    // has armed the track by hand meanwhile, and that stays theirs.
    rig.session.set_armed(voice, true).unwrap();
    settle(&mut rig);
    by_hand += 1;
    assert_eq!(opens(&rig), by_hand);
    rig.session.edit(|song| song.set_tempo(98.0)).unwrap();
    ben.edit(|song| song.set_track_pan(voice, 0.3)).unwrap();
    ben.edit(|song| song.set_track_volume(bass, -3.0)).unwrap();
    let (from_ben, from_ada) = (ben.since(&rig.session.floors()), rig.session.since(&ben.floors()));
    let (floors, replica) = (ben.floors(), ben.replica());
    at_the_door(&mut rig.session, |session| session.merge("Ben", from_ben, Some(&floors), replica).unwrap());
    ben.merge("Ada", from_ada, Some(&rig.session.floors()), rig.session.replica()).unwrap();
    settle(&mut rig);
    assert_eq!((rig.session.project().track(voice).unwrap().pan, ben.project().tempo), (0.3, 98.0));
    assert_eq!(opens(&rig), by_hand);
    assert!(rig.session.is_armed(voice) && rig.host.capturing().is_some());
    rig.session.take_said();
    ben.take_said();

    // 5. "Bring it back", asked at the assistant's door. The user has
    // switched the track's monitoring on by hand, so that it is always
    // heard, and writes in it while the other removes it. The track is
    // gone and nothing is heard; the door brings it back, and what the
    // song then says of its input is held: no hand said so of the track
    // that is there now.
    rig.session.set_armed(voice, false).unwrap();
    rig.session.edit(|song| song.set_track_input(voice, always)).unwrap();
    settle(&mut rig);
    assert!(!rig.session.input_held(voice) && rig.host.capturing().is_some(), "the hand switched it on");
    by_hand = opens(&rig);
    rig.session.take_said();
    rig.session.edit(|song| song.clip_mut(verse)?.add_notes(&[trill_core::merge::lot::note(72, 4.0)])).unwrap();
    ben.edit(|song| song.remove_track(voice).map(drop)).unwrap();
    arrives(&mut ben, &mut rig, "Ben");
    leaves(&mut rig, &mut ben);
    assert!(rig.session.project().track(voice).is_err() && rig.host.capturing().is_none());
    let [notice] = rig.session.notices() else { panic!("the user is told: {:?}", rig.session.notices()) };
    assert_eq!(notice.remedy.as_ref().map(|remedy| remedy.label.as_str()), Some("Bring it back"));
    let id = notice.id;
    at_the_door(&mut rig.session, |session| session.remedy(id)).unwrap();
    settle(&mut rig);
    assert!(rig.session.project().track(voice).is_ok(), "the track is back, with what was written in it");
    assert!(rig.session.input_held(voice), "and its input is held");
    assert!(rig.session.take_input_note().is_some_and(|said| said.contains("the input of 'Voice' is held")));
    assert_eq!(opens(&rig), by_hand, "an answer given at the door opened the input");
    assert!(rig.host.capturing().is_none());
    leaves(&mut rig, &mut ben);

    // 6. "Put mine back", asked at the door: of two recordings put over
    // one stretch at once the user's was the earlier and gave way. The
    // track records the microphone and was armed by hand, which stays as
    // it is: the door's answer opens nothing more.
    let recording = |name: &str| AudioContent::new(format!("{name}.wav"));
    rig.session.edit(|song| song.set_track_input(bass, input(Monitoring::Off))).unwrap();
    rig.session.edit(|song| song.add_audio_clip(bass, "Bed", 0.0, 16.0, recording("bed")).map(drop)).unwrap();
    let one = rig.session.edit(|song| song.add_take(bass, "Take 1", 0.0, 16.0, recording("one"))).unwrap();
    let two = rig.session.edit(|song| song.add_take(bass, "Take 2", 0.0, 16.0, recording("two"))).unwrap();
    leaves(&mut rig, &mut ben);
    rig.session.set_armed(bass, true).unwrap();
    settle(&mut rig);
    by_hand = opens(&rig);
    assert!(rig.host.capturing().is_some());
    rig.session.edit(|song| song.comp_take(one, 4.0, 8.0).map(drop)).unwrap();
    ben.edit(|song| song.set_track_pan(bass, 0.1)).unwrap();
    ben.edit(|song| song.comp_take(two, 4.0, 8.0).map(drop)).unwrap();
    leaves(&mut rig, &mut ben);
    arrives(&mut ben, &mut rig, "Ben");
    let [notice] = rig.session.notices() else { panic!("the user is told: {:?}", rig.session.notices()) };
    assert_eq!(notice.remedy.as_ref().map(|remedy| remedy.label.as_str()), Some("Put mine back"));
    let id = notice.id;
    at_the_door(&mut rig.session, |session| session.remedy(id)).unwrap();
    settle(&mut rig);
    assert!(rig.session.notices().is_empty());
    assert_eq!(opens(&rig), by_hand, "an answer given at the door opened the input");
    assert!(rig.session.is_armed(bass) && rig.host.capturing().is_some(), "what the hand armed stays armed");
    leaves(&mut rig, &mut ben);
    arrives(&mut ben, &mut rig, "Ben");

    // 7. "Have mine instead", asked at the door: of two devices made to
    // listen to each other's tracks the user's was the later, and waits.
    let on_voice = rig.session.add_device(ChainRef::Track(voice), "builtin:compressor", None, None).unwrap();
    let on_bass = rig.session.add_device(ChainRef::Track(bass), "builtin:compressor", None, None).unwrap();
    leaves(&mut rig, &mut ben);
    ben.set_sidechain(on_bass, Some(voice)).unwrap();
    rig.session.edit(|song| song.set_tempo(99.0)).unwrap();
    rig.session.set_sidechain(on_voice, Some(bass)).unwrap();
    leaves(&mut rig, &mut ben);
    arrives(&mut ben, &mut rig, "Ben");
    let [notice] = rig.session.notices() else { panic!("the user is told: {:?}", rig.session.notices()) };
    assert_eq!(notice.remedy.as_ref().map(|remedy| remedy.label.as_str()), Some("Have mine instead"));
    let id = notice.id;
    at_the_door(&mut rig.session, |session| session.remedy(id)).unwrap();
    settle(&mut rig);
    assert_eq!(rig.session.project().device(on_voice).unwrap().sidechain, Some(bass), "the user's is what holds now");
    assert_eq!(opens(&rig), by_hand, "an answer given at the door opened the input");
    assert!(rig.session.is_armed(bass) && rig.session.input_held(voice));

    // 8. The user joins someone else's song, by hand, and what is handed
    // over says of every track that it records the microphone and is
    // always heard. That is the song's to say and nobody's hand: each is
    // held, nothing that was armed in the song before is armed in this
    // one, and the input that was open is let go of.
    let mut mallory = someone_else();
    for name in ["One", "Two"] {
        mallory.edit(|song| Ok(song.add_track(name))).unwrap();
    }
    let mut handed: serde_json::Value = serde_json::from_str(&mallory.whole_song()).unwrap();
    for track in handed["tracks"].as_array_mut().expect("a song has its tracks") {
        track["input"] = serde_json::to_value(always.unwrap()).unwrap();
    }
    rig.session.take_song(&handed.to_string()).unwrap();
    settle(&mut rig);
    let tracks: Vec<TrackId> = rig.session.project().tracks.iter().map(|track| track.id).collect();
    assert_eq!(tracks.len(), 2);
    for track in tracks {
        let said = rig.session.project().track(track).unwrap().input;
        assert!(said.is_none() || rig.session.input_held(track), "{said:?}");
    }
    assert!(rig.session.armed().is_empty());
    assert_eq!(opens(&rig), by_hand, "a song that was handed over opened the input");
    assert!(rig.host.capturing().is_none());
    assert!(!rig.session.input_status().needed);
}
