//! Programs that share a song, each a runtime of its own, connected over
//! this machine's loopback and nothing else: no port is opened on any
//! network.

use std::path::Path;
use std::time::{Duration, Instant};
use trill_core::merge::lot::note;
use trill_core::merge::{Change, Floors, Op, Said, Stamp, TrackOp, TrackSet};
use trill_core::{ChainRef, ClipId, TrackId, TrackKind};
use trill_engine::AudioBackend;
use trill_runtime::{Mine, Reach, Runtime, RuntimeOptions, Storage};
use trill_together::{Invitation, Message, PROTOCOL, Person, secure};

const WAIT: Duration = Duration::from_secs(30);

/// A runtime that keeps its settings under `folder`: no audio device, no
/// plugins, nothing of the user's.
fn start(folder: &Path, name: &str) -> Runtime {
    let storage = Storage::At { settings: folder.join(name).join("settings.json"), library_index: None };
    let runtime = Runtime::start(RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage })
        .expect("the runtime starts");
    runtime.set_name_together(name);
    runtime
}

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The song a program holds, to the last id and without what is its own.
fn text(runtime: &Runtime) -> String {
    let mut song = runtime.session.lock().project().clone();
    song.count_ids_from(0);
    song.loop_region.enabled = false;
    for track in &mut song.tracks {
        track.solo = false;
    }
    song.to_json()
}

fn same(a: &Runtime, b: &Runtime) -> bool {
    text(a) == text(b)
}

/// A program with a small song in it.
fn with_a_song(folder: &Path, name: &str) -> (Runtime, TrackId, ClipId) {
    let runtime = start(folder, name);
    let (keys, verse) = {
        let mut session = runtime.session.lock();
        let keys = session.edit(|song| Ok(song.add_track("Keys"))).unwrap();
        session.edit(|song| Ok(song.add_track_of_kind("Room", TrackKind::Return))).unwrap();
        session.add_device(ChainRef::Track(keys), "builtin:polysynth", None, None).unwrap();
        let verse = session.edit(|song| song.add_midi_clip(keys, "Verse", 0.0, 8.0)).unwrap();
        session.edit(|song| song.clip_mut(verse)?.add_notes(&[note(60, 0.0), note(64, 1.0)])).unwrap();
        (keys, verse)
    };
    (runtime, keys, verse)
}

fn said(runtime: &Runtime) -> String {
    runtime.said_together(0).into_iter().map(|(_, sentence)| sentence).collect::<Vec<_>>().join(" | ")
}

#[test]
fn two_programs_share_a_song_see_each_other_and_part_and_meet_again_with_both_sets_of_work() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, keys, verse) = with_a_song(folder.path(), "Ada");
    let ben = start(folder.path(), "Ben");
    assert!(!ada.together().is_on() && ada.session.lock().shared_song().is_none());

    // Ada shares, on this machine only, and hands Ben the invitation.
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    assert!(invitation.starts_with("trill:127.0.0.1:"), "{invitation}");
    assert_eq!(ada.share(Reach::ThisMachine, 0).unwrap(), invitation, "sharing again is the same invitation");
    assert_eq!(ada.together().invitation.as_deref(), Some(invitation.as_str()));
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben));
    eventually("each sees the other", || ada.together().people.len() == 1 && ben.together().people.len() == 1);
    let (at_ada, at_ben) = (ada.together(), ben.together());
    assert_eq!((at_ada.people[0].person.name.as_str(), at_ben.people[0].person.name.as_str()), ("Ben", "Ada"));
    assert_eq!((at_ada.connected, at_ben.connected, at_ben.invitation.is_none()), (1, 1, true));
    // Everyone has a colour, the same in both programs, and no two the same.
    assert_eq!(at_ada.me.colour, at_ben.people[0].person.colour);
    assert_eq!(at_ben.me.colour, at_ada.people[0].person.colour);
    assert_ne!(at_ada.me.colour, at_ben.me.colour);
    assert!(said(&ada).contains("Ben is here.") && said(&ben).contains("You are in the song with Ada."), "{}", said(&ben));
    assert_eq!(ada.session.lock().shared_song(), ben.session.lock().shared_song());
    assert!(ben.session.lock().is_modified(), "a song that was handed over is one to save");

    // Both change it at once.
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    ben.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(48, 4.0)])).unwrap();
    ben.session.lock().edit(|song| song.set_tempo(98.0)).unwrap();
    let lead = ada.session.lock().edit(|song| Ok(song.add_track("Lead"))).unwrap();
    eventually("both have all of it", || {
        same(&ada, &ben) && ben.session.lock().project().clip(verse).is_ok_and(|clip| clip.notes().len() == 4)
    });
    assert!(ben.session.lock().project().track(lead).is_ok() && ada.session.lock().project().tempo == 98.0);

    // What one holds, the other sees: a selection, a hand on a fader,
    // where their song stands.
    ben.holding(Mine { tracks: vec![keys], clips: vec![verse], notes: vec![(verse, 7)], holding: Some("the volume of Keys".into()) });
    ben.session.lock().seek(12.0);
    eventually("Ada sees what Ben holds", || {
        let people = ada.together().people;
        people[0].holding.as_deref() == Some("the volume of Keys") && people[0].beat == 12.0
    });
    let seen = ada.together().people.remove(0);
    assert_eq!((seen.tracks, seen.clips, seen.notes, seen.playing), (vec![keys], vec![verse], vec![(verse, 7)], false));
    // None of that is in the song, or in anyone's journal.
    assert!(same(&ada, &ben));

    // A drag: what the other sees is where it got to, not every step.
    {
        let mut session = ben.session.lock();
        session.begin_gesture();
        for step in 1..=200 {
            session.edit(|song| song.set_track_volume(keys, -(step as f32) / 10.0)).unwrap();
        }
        session.end_gesture();
    }
    eventually("the fader is where Ben left it", || ada.session.lock().project().track(keys).unwrap().volume_db == -20.0);

    // Ben leaves, and both go on alone.
    ben.leave();
    eventually("Ben is gone", || ada.together().people.is_empty() && !ben.together().is_on());
    assert_eq!(ada.together().connected, 0);
    ada.session.lock().edit(|song| song.set_track_pan(keys, -0.5)).unwrap();
    ada.session.lock().edit(|song| song.clip_mut(verse)?.remove_notes(|note| note.pitch == 60).map(drop)).unwrap();
    let idea = ben.session.lock().edit(|song| song.add_midi_clip(lead, "Idea", 0.0, 4.0)).unwrap();
    ben.session.lock().edit(|song| song.set_track_volume(lead, -3.0)).unwrap();
    assert!(!same(&ada, &ben));

    // He comes back by the same invitation: it is the same song, and
    // both sets of work are in it.
    ben.join(&invitation, false).unwrap();
    eventually("both sets of work are merged", || {
        same(&ada, &ben) && ada.session.lock().project().clip(idea).is_ok() && ada.together().people.len() == 1
    });
    let song = ben.session.lock().project().clone();
    assert_eq!((song.track(keys).unwrap().pan, song.track(lead).unwrap().volume_db), (-0.5, -3.0));
    assert_eq!(song.clip(verse).unwrap().notes().len(), 3);

    // Ada stops sharing: nobody new can come, and Ben stays.
    ada.stop_sharing();
    eventually("the invitation is no longer one", || ada.together().invitation.is_none());
    let late = start(folder.path(), "Cy");
    late.join(&invitation, false).unwrap();
    eventually("the latecomer is told", || said(&late).contains("Not joined"));
    ada.session.lock().edit(|song| song.set_tempo(111.0)).unwrap();
    eventually("Ben still hears Ada", || ben.session.lock().project().tempo == 111.0);
}

#[test]
fn a_third_program_is_passed_on_what_the_other_two_do_and_sees_them_both() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, keys, verse) = with_a_song(folder.path(), "Ada");
    let (ben, cy) = (start(folder.path(), "Ben"), start(folder.path(), "Cy"));
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    ben.join(&invitation, false).unwrap();
    cy.join(&invitation, false).unwrap();
    eventually("all three have the song", || same(&ada, &ben) && same(&ada, &cy));
    // Ben and Cy are each connected to Ada alone, and know of each other.
    eventually("everyone sees the two others", || {
        [&ada, &ben, &cy].iter().all(|program| program.together().people.len() == 2)
    });
    assert_eq!((ben.together().connected, cy.together().connected, ada.together().connected), (1, 1, 2));
    let names = |program: &Runtime| {
        let mut names: Vec<String> = program.together().people.into_iter().map(|here| here.person.name).collect();
        names.sort();
        names
    };
    assert_eq!((names(&ben), names(&cy)), (vec!["Ada".to_owned(), "Cy".into()], vec!["Ada".to_owned(), "Ben".into()]));
    let colours: Vec<u8> = [&ada, &ben, &cy].iter().map(|program| program.together().me.colour).collect();
    assert!(colours[0] != colours[1] && colours[1] != colours[2] && colours[0] != colours[2], "{colours:?}");

    // What Ben does reaches Cy through Ada, and what Cy does, Ben.
    ben.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    cy.session.lock().edit(|song| song.set_track_volume(keys, -7.0)).unwrap();
    eventually("all three hold the same", || {
        same(&ada, &ben) && same(&ben, &cy) && cy.session.lock().project().clip(verse).unwrap().notes().len() == 3
    });
    assert_eq!(ben.session.lock().project().track(keys).unwrap().volume_db, -7.0);

    // Cy goes: both others know, and go on.
    drop(cy);
    eventually("Cy is gone for both", || ada.together().people.len() == 1 && ben.together().people.len() == 1);
    assert_eq!(names(&ben), ["Ada"]);
}

#[test]
fn joining_does_not_replace_a_song_with_unsaved_changes_unless_that_is_said() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, ..) = with_a_song(folder.path(), "Ada");
    let (ben, ..) = with_a_song(folder.path(), "Ben");
    ben.session.lock().edit(|song| Ok(song.add_track("Ben's own"))).unwrap();
    let own = text(&ben);
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    ben.join(&invitation, false).unwrap();
    eventually("Ben is told why not", || said(&ben).contains("has changes that are not saved"));
    assert_eq!(text(&ben), own, "his song is as it was");
    eventually("and nobody is connected", || ada.together().connected == 0 && !ben.together().is_on());
    // Said, it is replaced.
    ben.join(&invitation, true).unwrap();
    eventually("Ben has Ada's song", || same(&ada, &ben));
    // What is no invitation is refused where it is read.
    assert!(ben.join("come to my place", false).unwrap_err().contains("trill:HOST:PORT:SECRET"));
}

/// A program that has the invitation and speaks the protocol by hand.
fn by_hand(invitation: &str) -> (secure::Sayer, secure::Hearer) {
    let invitation = Invitation::parse(invitation).unwrap();
    let stream = std::net::TcpStream::connect(invitation.address()).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    secure::secure(stream, &invitation.key(), true).expect("the invitation is taken")
}

fn hello(protocol: u32) -> Message {
    Message::Hello {
        protocol,
        build: "9.9.9".into(),
        person: Person { id: 4242, name: "Mallory".into(), colour: 0 },
        song: None,
        floors: Floors::default(),
        replica: None,
    }
}

/// What the other side says until it ends the connection, and why it did.
fn until_bye(hear: &mut secure::Hearer) -> (Vec<Message>, String) {
    let mut heard = Vec::new();
    loop {
        match hear.hear(trill_together::MAX_MESSAGE).map(|bytes| Message::decode(&bytes)) {
            Ok(Ok(Message::Bye { why })) => return (heard, why),
            Ok(Ok(message)) => heard.push(message),
            Ok(Err(why)) => return (heard, why),
            Err(broken) => return (heard, broken.to_string()),
        }
    }
}

#[test]
fn a_program_that_speaks_another_version_is_refused_in_a_sentence() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, ..) = with_a_song(folder.path(), "Ada");
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    let before = text(&ada);
    let (mut say, mut hear) = by_hand(&invitation);
    say.say(&hello(PROTOCOL + 1).encode()).unwrap();
    let (heard, why) = until_bye(&mut hear);
    assert!(why.contains("do not speak the same version") && why.contains("9.9.9"), "{why}");
    assert!(heard.iter().all(|message| !matches!(message, Message::Song { .. })), "and is handed nothing of the song");
    eventually("Ada is told too", || said(&ada).contains("both need the same build of Trill"));
    assert_eq!(text(&ada), before);
    eventually("nobody is connected", || ada.together().connected == 0);
}

#[test]
fn a_program_that_says_what_no_edit_could_have_made_is_let_go_of_and_the_song_is_as_it_was() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, keys, _) = with_a_song(folder.path(), "Ada");
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    let (mut say, mut hear) = by_hand(&invitation);
    say.say(&hello(PROTOCOL).encode()).unwrap();
    // It is handed the song, as anyone with the invitation is.
    let handed = loop {
        match Message::decode(&hear.hear(trill_together::MAX_MESSAGE).unwrap()).unwrap() {
            Message::Song { text } => break text,
            _ => continue,
        }
    };
    assert!(handed.contains("\"shared\"") && handed.contains("Verse"));
    let before = text(&ada);
    let revision = ada.session.lock().revision();

    // A change nothing could have made: a pan that is no pan.
    let bad = Change { stamp: Stamp(50, 77), ops: vec![Op::Track(keys, TrackOp::Set(TrackSet::Pan(-9.0)))] };
    let changes = Message::Changes { said: vec![Said { change: bad, knew: None }], floors: Floors::default(), replica: Some(77) };
    say.say(&changes.encode()).unwrap();
    let (_, why) = until_bye(&mut hear);
    assert!(why.contains("no edit could have made"), "{why}");
    eventually("Ada is told who and why", || said(&ada).contains("Mallory is no longer connected"));
    assert_eq!((text(&ada), ada.session.lock().revision()), (before.clone(), revision), "and her song is as it was");

    // Bytes that are no message at all, and a flood of messages.
    let (mut say, mut hear) = by_hand(&invitation);
    say.say(b"\xff\xfe not json").unwrap();
    let _ = until_bye(&mut hear);
    let (mut say, mut hear) = by_hand(&invitation);
    say.say(&hello(PROTOCOL).encode()).unwrap();
    let gone = Message::Gone { person: 1 }.encode();
    for _ in 0..6000 {
        if say.say(&gone).is_err() {
            break;
        }
    }
    let _ = until_bye(&mut hear);
    eventually("nobody is left connected", || ada.together().connected == 0);
    assert_eq!(text(&ada), before);
    // And someone who is really there still gets in afterwards.
    let ben = start(folder.path(), "Ben");
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben));
}

#[test]
fn opening_another_song_leaves_the_shared_one_and_the_others_go_on() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, keys, _) = with_a_song(folder.path(), "Ada");
    let ben = start(folder.path(), "Ben");
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben) && ada.together().people.len() == 1);
    ben.session.lock().new_project();
    eventually("Ben has left", || !ben.together().is_on() && ada.together().people.is_empty());
    assert!(said(&ben).contains("another song was opened"), "{}", said(&ben));
    assert!(ben.session.lock().project().tracks.is_empty(), "and his new song is his own");
    ada.session.lock().edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(ben.session.lock().project().tracks.is_empty());
    assert!(ada.together().invitation.is_some(), "Ada is still sharing");
}
