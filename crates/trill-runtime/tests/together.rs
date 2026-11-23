//! Programs that share a song, each a runtime of its own, connected over
//! this machine's loopback and nothing else: no port is opened on any
//! network.

use std::path::Path;
use std::time::{Duration, Instant};
use trill_core::merge::lot::note;
use trill_core::merge::{Change, Floors, Op, Said, Stamp, TrackOp, TrackSet};
use trill_core::{ChainRef, ClipId, TrackId, TrackKind};
use trill_engine::AudioBackend;
use trill_runtime::{Meeting, MeetingPointSet, Mine, Reach, Runtime, RuntimeOptions, Storage};
use trill_together::line::{Line, Pace};
use trill_together::{Invitation, Message, PROTOCOL, Person, meet, secure};

const WAIT: Duration = Duration::from_secs(30);

/// A runtime that keeps its settings under `folder`: no audio device, no
/// plugins, nothing of the user's.
fn start(folder: &Path, name: &str) -> Runtime {
    let storage = Storage::At { settings: folder.join(name).join("settings.json"), library_index: None };
    let runtime =
        Runtime::start(RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage })
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
        (track.solo, track.input) = (false, None);
    }
    song.to_json()
}

fn same(a: &Runtime, b: &Runtime) -> bool {
    text(a) == text(b)
}

/// [`start`], for where that name is taken.
fn start_named(folder: &Path, name: &str) -> Runtime {
    start(folder, name)
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
    assert!(
        said(&ada).contains("Ben is here.") && said(&ben).contains("You are in the song with Ada."),
        "{}",
        said(&ben)
    );
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
    ben.holding(Mine {
        tracks: vec![keys],
        clips: vec![verse],
        notes: vec![(verse, 7)],
        holding: Some("the volume of Keys".into()),
    });
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
    eventually("the fader is where Ben left it", || {
        ada.session.lock().project().track(keys).unwrap().volume_db == -20.0
    });

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
    let changes =
        Message::Changes { said: vec![Said { change: bad, knew: None }], floors: Floors::default(), replica: Some(77) };
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
fn whoever_connects_is_told_who_shares_the_song_before_anything_else_is_said_to_them() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, keys, _) = with_a_song(folder.path(), "Ada");
    let ben = start(folder.path(), "Ben");
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    ben.join(&invitation, false).unwrap();
    eventually("Ben is in the song", || same(&ada, &ben) && ada.together().people.len() == 1);

    // Ada's program is kept from its song for a moment, as a save or a
    // long edit keeps it, and has something new to say of where she is.
    // Someone connects meanwhile: the connection is there before the
    // program has come round to greeting it.
    let held = ada.session.lock();
    std::thread::sleep(Duration::from_millis(150));
    let (_say, mut hear) = by_hand(&invitation);
    std::thread::sleep(Duration::from_millis(150));
    ada.holding(Mine { tracks: vec![keys], holding: Some("the volume of Keys".into()), ..Mine::default() });
    drop(held);

    // Whoever hears something else first takes the other for no Trill and
    // lets go, which is how a third person once never got into a song.
    let first = Message::decode(&hear.hear(trill_together::MAX_MESSAGE).unwrap()).unwrap();
    assert!(matches!(first, Message::Hello { .. }), "the first thing said is who is there, not {first:?}");
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

#[test]
fn a_join_whose_connection_ends_before_the_two_have_greeted_each_other_says_so() {
    let folder = tempfile::tempdir().unwrap();
    let ben = start(folder.path(), "Ben");
    // Something that has the invitation's secret and lets go without a
    // word, as a program that goes down while it is joined does.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let invitation = Invitation::new("127.0.0.1", listener.local_addr().unwrap().port()).unwrap();
    let key = invitation.key();
    let taking = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        drop(secure::secure(stream, &key, false).unwrap());
    });
    ben.join(&invitation.to_string(), false).unwrap();
    taking.join().unwrap();
    eventually("Ben is told that the join came to nothing", || {
        let said = said(&ben);
        said.contains("Not joined: the connection ended before the other program had said who it is")
    });
    eventually("and is in no song with anyone", || {
        let together = ben.together();
        !together.joining && together.connected == 0 && together.people.is_empty()
    });
    assert!(!said(&ben).contains("You are in the song"), "{}", said(&ben));
}

/// A meeting point on this machine, on a port the system picks, at which
/// one key is given: as its keeper would hand it over, and its log.
struct MeetingPoint {
    server: Option<meet::Server>,
    port: u16,
    /// Its address, as a keeper writes it.
    address: String,
    key: meet::Key,
    limits: meet::Limits,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl MeetingPoint {
    fn start(limits: meet::Limits) -> MeetingPoint {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut point = MeetingPoint {
            server: None,
            port,
            address: format!("127.0.0.1:{port}"),
            key: meet::Key::new().unwrap(),
            limits,
            log: Default::default(),
        };
        point.serve(listener);
        point
    }

    fn serve(&mut self, listener: std::net::TcpListener) {
        let lines = self.log.clone();
        let said: meet::Log = std::sync::Arc::new(move |line: &str| lines.lock().unwrap().push(line.to_owned()));
        let keys = meet::Keys::of(vec![meet::keys::Given::of(&self.key, "Ada")]);
        self.server = Some(meet::Server::start(vec![listener], keys, self.limits.clone(), said).unwrap());
    }

    /// It goes down: every connection through it is lost on the way.
    fn stop(&mut self) {
        self.server.take().expect("it is running").stop();
    }

    /// Its keeper starts it again, where it was.
    fn start_again(&mut self) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", self.port)).unwrap();
        self.serve(listener);
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    /// `sharer` shares its song through this meeting point, with the key
    /// its keeper gave: the invitation, once the meeting is open here.
    fn shared_by(&self, sharer: &Runtime) -> String {
        sharer.set_meeting_point(&self.address, Some(&self.key.to_string())).unwrap();
        let invitation = sharer.share_through_meeting_point().unwrap();
        eventually("the meeting is open", || sharer.together().meeting == Some((self.address.clone(), Meeting::Held)));
        invitation
    }
}

/// 3 connections.
#[test]
fn two_programs_share_a_song_through_a_meeting_point_whose_key_is_in_the_settings_and_nowhere_else() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, _, verse) = with_a_song(folder.path(), "Ada");
    let ben = start(folder.path(), "Ben");
    let point = MeetingPoint::start(meet::Limits::default());
    let key = point.key.to_string();

    // Nothing is shared through a meeting point until one is set: its
    // address and the key its keeper gave, once.
    assert!(ada.share_through_meeting_point().unwrap_err().contains("no meeting point is set"));
    assert_eq!(ada.meeting_point(), None);
    assert!(
        ada.set_meeting_point("https://meet.example.org", Some(&key)).unwrap_err().contains("meet.example.org:443")
    );
    assert!(ada.set_meeting_point(&point.address, Some("not a key")).unwrap_err().contains("ten groups of four"));
    assert!(ada.set_meeting_point(&point.address, None).unwrap_err().contains("takes the key"));
    assert_eq!(ada.meeting_point(), None, "what is no meeting point is not kept");
    ada.set_meeting_point(&format!("  {} ", point.address), Some(&key.to_lowercase().replace('-', " "))).unwrap();
    let set = MeetingPointSet { address: point.address.clone(), has_key: true };
    assert_eq!(ada.meeting_point(), Some(set.clone()));
    // (The address alone is changed without the key being given again.)
    ada.set_meeting_point(&point.address, None).unwrap();
    assert_eq!(ada.meeting_point(), Some(set));

    // Shared through it: the invitation is there at once and carries the
    // meeting point's address, and the meeting is open there a moment on.
    let invitation = ada.share_through_meeting_point().unwrap();
    assert!(invitation.starts_with(&format!("trill:meet:{}:", point.address)), "{invitation}");
    assert_eq!(ada.share_through_meeting_point().unwrap(), invitation, "sharing again is the same invitation");
    eventually("the meeting is open", || ada.together().meeting == Some((point.address.clone(), Meeting::Held)));

    // Whoever is handed the invitation sets nothing up.
    assert_eq!(ben.meeting_point(), None);
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben));
    eventually("each sees the other", || ada.together().people.len() == 1 && ben.together().people.len() == 1);
    assert!(said(&ben).contains("You are in the song with Ada."), "{}", said(&ben));
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    ben.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(48, 4.0)])).unwrap();
    ben.session.lock().edit(|song| song.set_tempo(98.0)).unwrap();
    eventually("both have all of it", || {
        same(&ada, &ben) && ada.session.lock().project().clip(verse).is_ok_and(|clip| clip.notes().len() == 4)
    });
    assert_eq!(ada.session.lock().project().tempo, 98.0);
    assert_eq!(ben.together().meeting, None, "whoever joined holds no meeting");

    // The key is in the settings, and in nothing else that Ada's program
    // writes or says: not the song's file, not the journal, not how
    // things stand, not a sentence. The meeting point's log has neither
    // it nor the invitation's secret.
    let settings = std::fs::read_to_string(folder.path().join("Ada").join("settings.json")).unwrap();
    assert!(settings.contains(&key) && settings.contains(&point.address), "{settings}");
    let (song, journal) = (folder.path().join("song.trill"), folder.path().join("journal.json"));
    ada.session.lock().save(&song).unwrap();
    ada.session.lock().export_journal(&journal, true).unwrap();
    let written = [std::fs::read_to_string(&song).unwrap(), std::fs::read_to_string(&journal).unwrap()];
    let shown = [format!("{:?}", ada.together()), said(&ada), format!("{:?}", ada.meeting_point())];
    let secret = invitation.rsplit(':').next().unwrap();
    let log = point.log();
    for text in written.iter().chain(&shown).chain([&log]) {
        assert!(!text.contains(&key) && !text.contains(&key[10..]) && !text.contains(&key.replace('-', "")));
    }
    for text in &written {
        assert!(!text.contains(&point.address) && !text.contains(secret), "nor is the meeting point in the song");
    }
    assert!(log.contains("Ada holds a meeting") && !log.contains(secret), "{log}");
}

/// About 8 connections.
#[test]
fn a_link_that_is_lost_comes_back_by_itself_and_what_each_did_meanwhile_is_merged() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, keys, verse) = with_a_song(folder.path(), "Ada");
    let ben = start(folder.path(), "Ben");
    let mut point = MeetingPoint::start(meet::Limits::default());
    let invitation = point.shared_by(&ada);
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben) && ben.together().people.len() == 1);

    // The meeting point goes down. Neither of them left and neither said
    // a word: the connections were lost on the way.
    point.stop();
    eventually("each knows the other is away", || {
        ada.together().people.is_empty() && ben.together().people.is_empty() && ben.together().away.len() == 1
    });
    eventually("and Ada that the meeting point is", || matches!(ada.together().meeting, Some((_, Meeting::Away(_)))));
    assert!(ben.together().is_on() && ben.together().away[0].name == "Ada", "{:?}", ben.together().away);
    let (at_ben, at_ada) = (said(&ben), said(&ada));
    assert!(at_ben.contains("The connection to Ada was lost") && at_ben.contains("asks for it again by itself"));
    assert!(at_ada.contains("Ben is away: the connection was lost"), "{at_ada}");
    assert!(at_ada.contains(&format!("The meeting point {} is away", point.address)), "{at_ada}");
    assert!(!at_ben.contains("invitation again"), "nobody is asked to paste anything: {at_ben}");
    // What the system says of a connection that was closed is about
    // buffers: neither is told that.
    assert!(!at_ada.contains("buffer") && !at_ben.contains("buffer"), "{at_ada}\n{at_ben}");

    // Each goes on alone meanwhile.
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    ada.session.lock().edit(|song| song.set_track_pan(keys, -0.5)).unwrap();
    ben.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(48, 4.0)])).unwrap();
    ben.session.lock().edit(|song| song.set_tempo(98.0)).unwrap();
    assert!(!same(&ada, &ben));

    // Its keeper starts it again. Nobody does anything: the meeting is
    // held again under the invitation it had, Ben's program gets in
    // again, and both sets of work are in the song.
    point.start_again();
    eventually("they are in the song together again", || {
        same(&ada, &ben)
            && ada.together().people.len() == 1
            && ben.together().people.len() == 1
            && ben.together().away.is_empty()
    });
    let song = ada.session.lock().project().clone();
    assert_eq!((song.clip(verse).unwrap().notes().len(), song.tempo, song.track(keys).unwrap().pan), (4, 98.0, -0.5));
    assert_eq!(ada.together().invitation.as_deref(), Some(invitation.as_str()), "the invitation is the one it was");
    assert_eq!(ada.together().meeting, Some((point.address.clone(), Meeting::Held)));
    assert!(said(&ben).contains("The connection is back: you are in the song with Ada."), "{}", said(&ben));
    assert!(said(&ada).contains("is back: whoever has the invitation gets in again"), "{}", said(&ada));

    // Leaving is not losing: said, and nothing is asked for again.
    ben.leave();
    eventually("Ben is gone", || ada.together().people.is_empty() && !ben.together().is_on());
    assert!(said(&ada).ends_with("Ben left."), "{}", said(&ada));
}

/// Two programs in a song through a meeting point, each on a line of its
/// own home to it: Ada, who shares, with her line, and Ben with his.
fn each_on_a_line(folder: &Path, point: &MeetingPoint) -> (Runtime, Runtime, Line, Line, ClipId) {
    let (ada, _, verse) = with_a_song(folder, "Ada");
    let ben = start(folder, "Ben");
    let pace = Pace::of(20_000, 20_000, 5);
    let (hers, his) = (Line::to(&point.address, pace).unwrap(), Line::to(&point.address, pace).unwrap());
    ada.set_meeting_point(&hers.address(), Some(&point.key.to_string())).unwrap();
    let invitation = ada.share_through_meeting_point().unwrap();
    eventually("the meeting is open", || matches!(ada.together().meeting, Some((_, Meeting::Held))));
    ben.join(&his.carries(&Invitation::parse(&invitation).unwrap()).to_string(), false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben) && ben.together().people.len() == 1);
    eventually("Ada sees him", || ada.together().people.len() == 1);
    (ada, ben, hers, his, verse)
}

/// About 9 connections.
#[test]
fn whoever_shares_and_loses_their_line_without_a_word_is_missed_within_seconds_and_met_again() {
    let folder = tempfile::tempdir().unwrap();
    let point = MeetingPoint::start(meet::Limits::default());
    let (ada, ben, hers, _his, verse) = each_on_a_line(folder.path(), &point);

    // Ada's line goes down: her connections end where she is, and no word
    // of it reaches the meeting point or Ben. His own connection is as
    // good as it was, and what he says goes into it as before.
    hers.lose();
    eventually("Ada knows", || ada.together().people.is_empty());
    assert!(said(&ada).contains("Ben is away: the connection was lost"), "{}", said(&ada));
    // Each goes on meanwhile.
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    ben.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(48, 4.0)])).unwrap();

    // Ben's program hears nothing from hers, and takes that for what it
    // is within seconds: it lets go and asks again by itself. Her program
    // holds the meeting again by then (her line is back), and they are in
    // the song together with what each did.
    let began = Instant::now();
    eventually("Ben's program has missed her", || said(&ben).contains("The connection to Ada was lost"));
    assert!(said(&ben).contains("nothing was heard over it for 10 seconds"), "{}", said(&ben));
    assert!(began.elapsed() < Duration::from_secs(20), "it took {:?} to miss her", began.elapsed());
    eventually("they are in the song together again", || {
        same(&ada, &ben)
            && ada.together().people.len() == 1
            && ben.together().people.len() == 1
            && ben.together().away.is_empty()
    });
    assert_eq!(ada.session.lock().project().clip(verse).unwrap().notes().len(), 4);
    assert!(said(&ben).contains("The connection is back: you are in the song with Ada."), "{}", said(&ben));
    assert!(said(&ada).ends_with("Ben is back."), "{}", said(&ada));
    assert_eq!(ada.together().connected, 1, "one connection to him, and none that is no longer one");
}

/// About 9 connections.
#[test]
fn whoever_joined_and_loses_their_line_is_back_before_the_other_was_told_anything() {
    let folder = tempfile::tempdir().unwrap();
    let point = MeetingPoint::start(meet::Limits::default());
    let (ada, ben, _hers, his, verse) = each_on_a_line(folder.path(), &point);
    let before = said(&ada);

    // Ben's line goes down and is there again: his program sees its
    // connection end and asks again at once. Ada's program is sent no word
    // of the end, and the connection it had to him is open as far as it
    // can tell.
    his.lose();
    eventually("Ben's program knows", || said(&ben).contains("The connection to Ada was lost"));
    ben.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(48, 4.0)])).unwrap();
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    eventually("they are in the song together again", || {
        same(&ada, &ben) && ben.together().people.len() == 1 && ben.together().away.is_empty()
    });
    assert_eq!(ada.session.lock().project().clip(verse).unwrap().notes().len(), 4);
    assert!(said(&ben).contains("The connection is back: you are in the song with Ada."), "{}", said(&ben));

    // Ada's program has let go of the connection that was no longer one,
    // without a word: he was never away as far as she was told, so nothing
    // is said of his return either, and nothing later says he is away.
    eventually("the older connection is let go of", || ada.together().connected == 1);
    assert_eq!(ada.together().people.len(), 1);
    std::thread::sleep(Duration::from_secs(12));
    assert_eq!(said(&ada), before, "nothing was said to her of any of it");
    assert_eq!((ada.together().connected, ada.together().people.len()), (1, 1));
}

/// What stands between two programs on a way that can be cut: it passes
/// what each says to the other, and drops every connection when told to.
struct Way {
    port: u16,
    passing: std::sync::Arc<std::sync::Mutex<Vec<std::sync::Arc<std::net::TcpStream>>>>,
}

impl Way {
    fn to(port: u16) -> Way {
        use std::net::{Shutdown, TcpListener, TcpStream};
        use std::sync::Arc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let way = Way { port: listener.local_addr().unwrap().port(), passing: Default::default() };
        let passing = way.passing.clone();
        std::thread::spawn(move || {
            for near in listener.incoming().flatten() {
                let Ok(far) = TcpStream::connect(("127.0.0.1", port)) else { continue };
                let (near, far) = (Arc::new(near), Arc::new(far));
                passing.lock().unwrap().extend([near.clone(), far.clone()]);
                for (from, to) in [(near.clone(), far.clone()), (far, near)] {
                    std::thread::spawn(move || {
                        let _ = std::io::copy(&mut &*from, &mut &*to);
                        let _ = (from.shutdown(Shutdown::Both), to.shutdown(Shutdown::Both));
                    });
                }
            }
        });
        way
    }

    fn cut(&self) {
        for socket in self.passing.lock().unwrap().drain(..) {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// About 6 connections.
#[test]
fn a_lost_link_to_a_program_that_listens_itself_comes_back_too_until_its_invitation_is_no_longer_one() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, _, verse) = with_a_song(folder.path(), "Ada");
    let ben = start(folder.path(), "Ben");
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    let port = Invitation::parse(&invitation).unwrap().port;
    // Ben reaches Ada by a way that can be cut.
    let way = Way::to(port);
    let by_the_way = invitation.replace(&format!(":{port}:"), &format!(":{}:", way.port));
    ben.join(&by_the_way, false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben) && ben.together().people.len() == 1);

    // The way is cut, and is there again a moment later.
    way.cut();
    eventually("Ben's program knows", || ben.together().away.len() == 1);
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    ben.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(48, 4.0)])).unwrap();
    eventually("they are in the song together again", || {
        same(&ada, &ben) && ben.together().people.len() == 1 && ben.together().away.is_empty()
    });
    assert_eq!(ada.session.lock().project().clip(verse).unwrap().notes().len(), 4);
    assert!(said(&ben).contains("The connection is back: you are in the song with Ada."), "{}", said(&ben));

    // Ada stops sharing and shares again, where she listened before: that
    // is another invitation. When the way is cut now, Ben's program asks
    // again, is told that the invitation it has is no longer one, and
    // stops asking: that is when someone has to ask for the invitation.
    ada.stop_sharing();
    eventually("the invitation is no longer one", || ada.together().invitation.is_none());
    let mut another = None;
    eventually("she shares again on the port she had", || {
        another = ada.share(Reach::ThisMachine, port).ok();
        another.is_some()
    });
    assert_ne!(another.as_deref(), Some(invitation.as_str()));
    way.cut();
    eventually("Ben is told that asking again is no use", || said(&ben).contains("Ask for the invitation again."));
    assert!(said(&ben).contains("No longer in the song with Ada: the program at"), "{}", said(&ben));
    assert!(ben.together().away.is_empty() && !ben.together().is_on(), "{:?}", ben.together());
}

/// About 6 connections.
#[test]
fn a_meeting_point_that_does_not_take_the_key_or_passes_no_more_today_is_said_in_a_sentence() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, _, verse) = with_a_song(folder.path(), "Ada");
    let ben = start(folder.path(), "Ben");
    let point = MeetingPoint::start(meet::Limits { bytes_a_day: 200_000, ..meet::Limits::default() });

    // A key its keeper did not give: the invitation is there at once, as
    // always, and a moment on sharing ends in the sentence that says why.
    ada.set_meeting_point(&point.address, Some(&meet::Key::new().unwrap().to_string())).unwrap();
    ada.share_through_meeting_point().unwrap();
    eventually("it is said", || said(&ada).contains("Not shared through the meeting point"));
    let told = said(&ada);
    assert!(told.contains(&point.address) && told.contains("did not take the key"), "{told}");
    assert!(told.contains("The song is not shared, and is yours as it was."), "{told}");
    eventually("and nothing is shared", || ada.together().invitation.is_none() && ada.together().meeting.is_none());

    // With the key it is shared, and Ben is in the song.
    let invitation = point.shared_by(&ada);
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben) && ben.together().people.len() == 1);

    // More is said through the meeting point than its keeper lets a key
    // pass in a day: a clip of thousands of notes.
    let many: Vec<_> = (0..4000u32).map(|n| note(36 + (n / 800) as u8, f64::from(n % 800) * 0.01)).collect();
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&many)).unwrap();
    eventually("the meeting point passes no more", || matches!(ada.together().meeting, Some((_, Meeting::Away(_)))));
    let told = said(&ada);
    let at = &point.address;
    assert!(told.contains(&format!("The meeting point {at} passes no more for your key today")), "{told}");
    assert!(told.contains("The song is yours to go on with alone meanwhile"), "{told}");
    assert!(told.contains("is merged when you meet again"), "{told}");
    // Ben's link was lost on the way, and his program goes on asking: it
    // is tomorrow's to let in. The song is his to go on with as well.
    eventually("Ben's program asks again", || ben.together().away.len() == 1 && ben.together().away[0].tries >= 1);
    assert!(!said(&ben).contains("invitation again"), "{}", said(&ben));
    ben.leave();
    eventually("until he leaves", || !ben.together().is_on());
}

// ---- the Linux program, under WSL --------------------------------------------

/// What the test has WSL run: the meeting point as a keeper would start
/// it, in a folder of the test's own under `/tmp` there. `first` makes a
/// key and says it; `again` starts the program where it was; `end` clears
/// up. The number of the process is said first, to stop it by.
const IN_LINUX: &str = "#!/bin/sh
set -eu
run=/tmp/trill-meet-test-$1
case \"$4\" in
    first) rm -rf \"$run\" && mkdir -p \"$run\" && cp \"$3\" \"$run/trill-meet\" && chmod +x \"$run/trill-meet\"
           cd \"$run\" && ./trill-meet key Ada ;;
    again) cd \"$run\" ;;
    end) rm -rf \"$run\"; exit 0 ;;
esac
echo \"PID $$\"
exec ./trill-meet serve --port \"$2\"
";

/// A path of this machine as WSL has it: `C:\x\y` is `/mnt/c/x/y`.
fn in_wsl(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let path = path.strip_prefix("//?/").unwrap_or(&path);
    match path.split_once(":/") {
        Some((drive, rest)) => format!("/mnt/{}/{rest}", drive.to_lowercase()),
        None => path.to_owned(),
    }
}

/// The meeting point running under WSL, and what it has written.
struct InLinux {
    wsl: std::process::Child,
    distribution: String,
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl InLinux {
    fn wsl(distribution: &str) -> std::process::Command {
        let mut wsl = std::process::Command::new("wsl");
        wsl.args(["-d", distribution, "--"]);
        wsl
    }

    fn start(distribution: &str, script: &Path, arguments: &[&str]) -> InLinux {
        use std::io::BufRead;
        let mut wsl = (Self::wsl(distribution).arg("sh").arg(in_wsl(script)).args(arguments))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("WSL is installed");
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        for from in [
            Box::new(wsl.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
            Box::new(wsl.stderr.take().unwrap()),
        ] {
            let lines = lines.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(from).lines().map_while(Result::ok) {
                    lines.lock().unwrap().push(line);
                }
            });
        }
        InLinux { wsl, distribution: distribution.to_owned(), lines }
    }

    fn said(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }

    /// Waits for a line that has `words` in it, and returns it.
    fn line_with(&self, words: &str) -> String {
        let mut found = None;
        eventually(&format!("the meeting point under WSL says \"{words}\""), || {
            found = self.lines.lock().unwrap().iter().find(|line| line.contains(words)).cloned();
            found.is_some()
        });
        found.unwrap()
    }

    /// Ends the program there, by its number.
    fn stop(mut self) {
        let pid = self.line_with("PID ").trim_start_matches("PID ").trim().to_owned();
        let _ = Self::wsl(&self.distribution).args(["kill", &pid]).status();
        let _ = self.wsl.wait();
    }
}

/// The real thing, as far as one machine has it: the meeting point as the
/// static Linux program, run under WSL, and two programs on Windows that
/// share a song through it. It is reached on this machine's loopback,
/// which WSL hands on to Linux; nothing leaves the machine.
#[test]
#[ignore = "needs WSL with a Linux distribution, and the Linux program built: \
            `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld cargo build -p trill-meet --release --target \
            x86_64-unknown-linux-musl`, then `cargo test -p trill-runtime --test together under_wsl -- --ignored \
            --nocapture` (TRILL_WSL_DISTRIBUTION names another distribution than Ubuntu)"]
fn two_programs_on_windows_share_a_song_through_the_linux_program_under_wsl() {
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/x86_64-unknown-linux-musl/release/trill-meet");
    let program = std::env::var_os("TRILL_MEET_LINUX").map_or(built, std::path::PathBuf::from);
    assert!(program.is_file(), "the Linux program is not built at {}: see how above", program.display());
    let distribution = std::env::var("TRILL_WSL_DISTRIBUTION").unwrap_or_else(|_| "Ubuntu".to_owned());
    let folder = tempfile::tempdir().unwrap();
    let script = folder.path().join("in-linux.sh");
    std::fs::write(&script, IN_LINUX).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (id, port_said, program) = (std::process::id().to_string(), port.to_string(), in_wsl(&program));
    let arguments = |how: &'static str| [id.clone(), port_said.clone(), program.clone(), how.to_owned()];
    let start = |how: &'static str| {
        let arguments = arguments(how);
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        InLinux::start(&distribution, &script, &arguments)
    };

    // Its keeper makes a key and starts it.
    let point = start("first");
    let listening = point.line_with("a meeting point on");
    println!("under WSL ({distribution}): {listening}");
    let said = point.said();
    let key = said.lines().map(str::trim).find(|line| meet::Key::parse(line).is_ok()).expect("the key is shown, once");
    let at = format!("127.0.0.1:{port}");

    // Two programs on Windows. One shares through it, with the key; the
    // other joins with the invitation and nothing else.
    let (ada, _, verse) = with_a_song(folder.path(), "Ada");
    let ben = start_named(folder.path(), "Ben");
    ada.set_meeting_point(&at, Some(key)).unwrap();
    let began = Instant::now();
    let invitation = ada.share_through_meeting_point().unwrap();
    eventually("the meeting is open there", || ada.together().meeting == Some((at.clone(), Meeting::Held)));
    println!("the meeting was open at the meeting point {:.0} ms after sharing", began.elapsed().as_secs_f64() * 1e3);
    assert!(invitation.starts_with(&format!("trill:meet:{at}:")), "{invitation}");
    let began = Instant::now();
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the song", || same(&ada, &ben) && ben.together().people.len() == 1);
    println!("the one who joined had the song after {:.0} ms", began.elapsed().as_secs_f64() * 1e3);

    // Both change it at once, and how long an edit takes to the other.
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    ben.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(48, 4.0)])).unwrap();
    eventually("both have both", || {
        same(&ada, &ben) && ada.session.lock().project().clip(verse).is_ok_and(|clip| clip.notes().len() == 4)
    });
    let mut took: Vec<Duration> = (0..40)
        .map(|round| {
            let before = ben.session.lock().revision();
            let began = Instant::now();
            ada.session.lock().edit(|song| song.set_tempo(100.0 + f64::from(round))).unwrap();
            while ben.session.lock().revision() == before {
                assert!(began.elapsed() < WAIT, "an edit never arrived");
                std::thread::sleep(Duration::from_micros(200));
            }
            let took = began.elapsed();
            std::thread::sleep(Duration::from_millis(7 + (round as u64 * 13) % 31));
            took
        })
        .collect();
    took.sort();
    let ms = |at: usize| took[at].as_secs_f64() * 1e3;
    println!("an edit, to the other's graph: {:.0} ms in the middle, {:.0} ms for nine in ten", ms(20), ms(36));

    // The meeting point is stopped, as when its keeper restarts the
    // server. Neither of them left: each goes on, and says what is away.
    point.stop();
    eventually("each knows", || {
        ben.together().away.len() == 1 && matches!(ada.together().meeting, Some((_, Meeting::Away(_))))
    });
    ada.session.lock().edit(|song| song.clip_mut(verse)?.add_notes(&[note(74, 5.0)])).unwrap();
    ben.session.lock().edit(|song| song.set_tempo(98.0)).unwrap();
    assert!(!same(&ada, &ben));
    // Started again where it was, with the keys it had: nobody does
    // anything, and both sets of work are in the song.
    let began = Instant::now();
    let point = start("again");
    point.line_with("a meeting point on");
    eventually("they are in the song together again", || {
        same(&ada, &ben) && ben.together().people.len() == 1 && ben.together().away.is_empty()
    });
    println!("both were in the song again {:.1} s after it was started again", began.elapsed().as_secs_f64());
    let song = ada.session.lock().project().clone();
    assert_eq!((song.clip(verse).unwrap().notes().len(), song.tempo), (5, 98.0));
    assert_eq!(ada.together().invitation.as_deref(), Some(invitation.as_str()));

    // What the meeting point wrote: who held a meeting, and nothing of
    // the key or of the invitation's secret.
    point.line_with("Ada holds a meeting");
    let log = point.said();
    println!("its log since it was started again:\n{log}");
    let secret = invitation.rsplit(':').next().unwrap();
    assert!(!log.contains(&key[10..]) && !log.contains(secret), "{log}");
    drop((ada, ben));
    point.stop();
    let _ = start("end").wsl.wait();
}
