//! Two windows in one song: sharing and joining from the File menu, who
//! is here and what they hold, and a clash as a notice with its way back.
//! Each window is on a runtime of its own, and the two meet over this
//! machine's loopback as two programs do.

use super::*;
use crate::ui::TogetherScene;
use trill_core::merge::{Change, ClipOp, ClipSet, Floors, Op, Said, Stamp, TrackOp, TrackSet};
use trill_runtime::{Meeting, MeetingPointSet};

/// A window on a runtime of its own whose person goes by `name`.
fn window(name: &str, setup: impl FnOnce(&mut Session)) -> Harness {
    let harness = Harness::new(UiOptions::default(), setup);
    harness.runtime.set_name_together(name);
    harness
}

/// The song a window holds, to the last id and without what is its own.
fn song(harness: &Harness) -> String {
    let mut song = harness.project();
    song.count_ids_from(0);
    song.loop_region.enabled = false;
    for track in &mut song.tracks {
        (track.solo, track.input) = (false, None);
    }
    song.to_json()
}

/// Runs both windows until `done`.
fn both_until(a: &mut Harness, b: &mut Harness, what: &str, done: impl Fn(&Harness, &Harness) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(a, b) {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
        a.idle(1);
        b.idle(1);
    }
}

#[test]
fn two_windows_joined_in_one_song_change_it_at_once_and_each_says_who_is_here_and_what_they_hold() {
    let mut ids = None;
    let mut ada = window("Ada", |session| ids = Some(two_tracks(session)));
    let (one, _, clip) = ids.unwrap();
    let mut ben = window("Ben", |_| {});
    assert!(!ada.is_on_screen("Shared song"), "a song of one's own says nothing of anyone");

    // Ada shares from the File menu: on this computer only, by default.
    ada.click_label("File");
    ada.click_label("Share this song");
    ada.idle(2);
    assert!(ada.is_on("This computer only") && !ada.is_on("My network"));
    ada.click_label("Share");
    ada.wait_until("the invitation is there", |ada| ada.app.live().together.invitation.is_some());
    let invitation = ada.app.live().together.invitation.clone().unwrap();
    assert!(invitation.starts_with("trill:127.0.0.1:"), "{invitation}");
    assert!(ada.is_on_screen(&format!("Invitation: {invitation}")));
    ada.click_label("Copy the invitation");
    assert_eq!(ada.clipboard.as_deref(), Some(invitation.as_str()));
    assert!(ada.is_on_screen("Shared song: Shared \u{00B7} nobody else is here"));
    ada.key(Modifiers::NONE, Key::Escape);
    assert!(ada.app.state().together.panel.is_none(), "Escape closes the panel, and the song stays shared");

    // Ben joins from his File menu with what he was handed.
    ben.click_label("File");
    ben.click_label("Join a song");
    ben.idle(2);
    ben.clipboard = Some(invitation);
    ben.paste();
    ben.click_label("Join");
    both_until(&mut ada, &mut ben, "Ben has the song and each sees the other", |ada, ben| {
        song(ada) == song(ben) && ada.app.live().together.people.len() == 1 && ben.app.live().together.people.len() == 1
    });
    assert!(ada.is_on_screen("Shared song: Here: Ben") && ben.is_on_screen("Shared song: Here: Ada"));
    assert_eq!(ben.app.snapshot().project.tracks.len(), 2);
    let said = |harness: &Harness| harness.app.state().toasts.texts().join(" | ");
    assert!(
        said(&ada).contains("Ben is here.") && said(&ben).contains("You are in the song with Ada."),
        "{}",
        said(&ben)
    );
    ben.key(Modifiers::NONE, Key::Escape);

    // Both change it at once: Ada in her window, Ben in his.
    ada.session.lock().edit(|project| project.set_track_volume(one, -6.0)).unwrap();
    let lane = ben.lane(1.0, 1);
    ben.double_click(lane);
    both_until(&mut ada, &mut ben, "both have both changes", |ada, ben| {
        song(ada) == song(ben) && ada.project().tracks[1].clips.len() == 1 && ben.project().tracks[0].volume_db == -6.0
    });

    // What Ben has selected, Ada sees: in the panel by name, and in his
    // colour where it is.
    let over = ben.lane(9.0, 0);
    ben.click(over);
    assert_eq!(ben.selected_clips(), [clip]);
    both_until(&mut ada, &mut ben, "Ada sees what Ben holds", |ada, _| ada.app.live().together.on_clip(clip).is_some());
    let seen = ada.app.live().together.clone();
    assert_eq!(seen.on_clip(clip).map(|here| here.person.name.as_str()), Some("Ben"));
    assert_ne!(seen.me.as_ref().unwrap().colour, seen.people[0].person.colour, "each in a colour of their own");
    ada.click_label("Shared song: Here: Ben");
    ada.idle(2);
    assert!(ada.is_on_screen("Ada: you") && ada.is_on_screen("Ben: on the clip \u{201C}Clip\u{201D}"));
    // None of that is in the song.
    assert_eq!(song(&ada), song(&ben));

    // Each undoes their own: Ben's clip goes, Ada's fader stays.
    ben.key(Modifiers::COMMAND, Key::Z);
    both_until(&mut ada, &mut ben, "Ben's clip is gone for both", |ada, ben| {
        song(ada) == song(ben) && ada.project().tracks[1].clips.is_empty()
    });
    assert_eq!(ada.project().tracks[0].volume_db, -6.0);

    // Ada leaves from the panel; Ben is told, and goes on with the song.
    ada.click_label("Leave");
    both_until(&mut ada, &mut ben, "they are apart", |ada, ben| {
        !ada.app.live().together.is_on() && ben.app.live().together.people.is_empty()
    });
    assert!(said(&ben).contains("Ada left."), "{}", said(&ben));
    assert!(!ada.is_on_screen("Shared song"));
    assert_eq!(ben.project().tracks.len(), 2);
}

/// A window whose song is shared, with a change in it that someone else
/// then makes without having seen it.
fn clash() -> (Harness, TrackId, ClipId) {
    let mut ids = None;
    let harness = window("Ada", |session| {
        ids = Some(two_tracks(session));
        session.share();
        session.hear_changes(true);
    });
    let (one, _, clip) = ids.unwrap();
    (harness, one, clip)
}

/// What someone else's program would say: a change made at `clock` that
/// knows nothing of what was done here.
fn from_ben(harness: &Harness, ops: Vec<Op>) {
    let mut session = harness.session.lock();
    let clock = session.floors().0.values().copied().max().unwrap_or(0) + 10;
    let change = Change { stamp: Stamp(clock, 77_777), ops };
    session.merge("Ben", vec![Said { change, knew: Some(Floors::default()) }], None, None).unwrap();
}

#[test]
fn a_clash_is_a_notice_that_stays_until_it_is_answered_and_puts_ones_own_back() {
    let (mut ada, _, clip) = clash();
    // Ada moves the clip; Ben, who has not seen that, moves it elsewhere.
    ada.session.lock().edit(|project| project.move_clip(clip, None, 16.0)).unwrap();
    from_ben(&ada, vec![Op::Clip(clip, ClipOp::Set(ClipSet::Start(32.0)))]);
    ada.idle(3);
    assert_eq!(ada.project().clip(clip).unwrap().start, 32.0, "the later move holds");
    let sentence = "Ben set where the clip \u{201C}Clip\u{201D} starts at the same moment as you, and theirs is what the \
                    song has.";
    assert!(ada.is_on_screen(sentence), "the notice says what happened");
    // It does not fade as a passing message does.
    ada.idle(900);
    assert!(ada.is_on_screen(sentence));
    ada.click_label("Put mine back");
    ada.idle(3);
    assert_eq!(ada.project().clip(clip).unwrap().start, 16.0);
    assert!(!ada.is_on_screen(sentence), "answered, it is gone");
    // Putting it back was an edit like any other.
    ada.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(ada.project().clip(clip).unwrap().start, 32.0);

    // A notice can also be left as it is.
    ada.session.lock().edit(|project| project.move_clip(clip, None, 4.0)).unwrap();
    from_ben(&ada, vec![Op::Clip(clip, ClipOp::Set(ClipSet::Start(40.0)))]);
    ada.idle(3);
    ada.click_label("Leave it");
    ada.idle(3);
    assert!(!ada.is_on_screen(sentence));
    assert_eq!(ada.project().clip(clip).unwrap().start, 40.0);
}

#[test]
fn taking_a_step_back_says_what_stays_as_someone_else_set_it() {
    let (mut ada, one, _) = clash();
    {
        let mut session = ada.session.lock();
        session.begin_gesture();
        session.edit(|project| project.set_track_volume(one, -6.0)).unwrap();
        session.edit(|project| project.set_track_pan(one, -0.5)).unwrap();
        session.end_gesture();
    }
    // Ben sets the volume, knowing of Ada's.
    {
        let mut session = ada.session.lock();
        let knew = session.floors();
        let clock = knew.0.values().copied().max().unwrap_or(0) + 1;
        let change =
            Change { stamp: Stamp(clock, 77_777), ops: vec![Op::Track(one, TrackOp::Set(TrackSet::Volume(-20.0)))] };
        session.merge("Ben", vec![Said { change, knew: Some(knew) }], None, None).unwrap();
    }
    ada.idle(2);
    assert!(ada.app.live().together.notices.is_empty(), "set knowing of hers, it is no clash");
    ada.key(Modifiers::COMMAND, Key::Z);
    let track = ada.project().tracks[0].clone();
    assert_eq!((track.volume_db, track.pan), (-20.0, 0.0), "her pan is back and his volume stays");
    let said = ada.app.state().toasts.texts().join(" | ");
    assert!(said.contains("One thing stays as someone else has set it since."), "{said}");
}

#[test]
fn company_that_is_made_up_is_shown_as_company_is() {
    let options = UiOptions { together_scene: Some(TogetherScene::Two), ..Default::default() };
    let (mut harness, ..) = with_three_tracks(options);
    harness.idle(3);
    assert!(harness.is_on_screen("Shared song: Here: Jo, Sam"));
    harness.click_label("Shared song: Here: Jo, Sam");
    harness.idle(2);
    assert!(harness.is_on_screen("Jo: has hold of the volume of the track \u{201C}Bass\u{201D}"));
    assert!(harness.is_on_screen("Kit: you"));
    // The invitation is there to hand to a third.
    assert!(harness.labels.iter().any(|(label, _)| label.starts_with("Invitation: trill:192.168.1.20:47474:")));
    let seen = &harness.app.live().together;
    let jo = &seen.people[0];
    assert!(seen.on_track(jo.tracks[0]).is_some() && seen.on_clip(jo.clips[0]).is_some());
}

#[test]
fn audio_that_is_on_its_way_and_files_that_are_held_are_said_in_the_status_bar_and_the_panel() {
    let options = UiOptions { together_scene: Some(TogetherScene::Arriving), ..Default::default() };
    let (mut harness, ..) = with_three_tracks(options);
    harness.idle(3);
    let bar = "Shared song: Here: Jo \u{00B7} 2 audio files wait for your word \u{00B7} 1 file held here";
    assert!(harness.is_on_screen(bar), "{:?}", harness.labels);
    harness.click_label(bar);
    harness.idle(2);
    // What is on its way: from whom, how far, and why one waits.
    assert!(harness.is_on_screen("Vocal take 3.wav \u{00B7} 20 MB of 48 MB \u{00B7} from Jo"), "{:?}", harness.labels);
    assert!(harness.is_on_screen("Room mics.flac \u{00B7} 0.0 MB of 612 MB \u{00B7} waits for your word"));
    assert!(harness.is_on_screen("Let them arrive"));
    // And what has not gone from here, by its path, with the word that lets it.
    assert!(harness.is_on_screen(r"D:\Sessions\Elsewhere\guide vocal.wav"));
    assert!(harness.is_on_screen("Let them go"));
}

#[test]
fn before_a_song_is_shared_the_panel_says_what_goes_with_it_and_its_audio_arrives_at_whoever_joins() {
    let folder = tempfile::tempdir().unwrap();
    let vocal = super::sounds::tone(folder.path().join("Elsewhere").join("vocal take.wav"), 1.0);
    let named = vocal.to_string_lossy().into_owned();
    let mut ada = window("Ada", |session| {
        let track = session.edit(|song| Ok(song.add_track("Vocal"))).unwrap();
        let recording = trill_core::project::AudioContent::new(named.clone());
        session.edit(|song| song.add_audio_clip(track, "Verse vocal", 0.0, 2.0, recording)).unwrap();
    });
    let mut ben = window("Ben", |_| {});
    ada.idle(2);
    ada.click_label("File");
    ada.click_label("Share this song");
    // What goes is looked at by a worker, and said before anything is shared.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ada.is_on_screen("From outside the song's folder, Trill's recordings and your sample library:") {
        assert!(Instant::now() < deadline, "the panel never said what goes: {:?}", ada.labels);
        std::thread::sleep(Duration::from_millis(5));
        ada.idle(1);
    }
    assert!(ada.labels.iter().any(|(label, _)| label.starts_with("1 audio file, ")), "{:?}", ada.labels);
    assert!(ada.is_on_screen(&vocal.to_string_lossy()), "the file outside the song's folder, by its path");
    assert!(!ada.runtime.together().is_on(), "nothing is shared by looking");
    ada.click_label("Share");
    ada.idle(2);
    let invitation = ada.runtime.together().invitation.expect("the song is shared");

    ben.runtime.join(&invitation, false).unwrap();
    let clip = ada.project().tracks.last().unwrap().clips[0].id;
    let here = |window: &Harness| {
        let files = window.runtime.session.lock().files_here();
        let file = files.into_iter().find(|file| file.place == trill_core::FilePlace::Clip(clip));
        file.is_some_and(|file| std::fs::read(&file.path).ok() == std::fs::read(&vocal).ok())
    };
    both_until(&mut ada, &mut ben, "the recording is at Ben's", |_, ben| here(ben));
    both_until(&mut ada, &mut ben, "nothing is on its way", |_, ben| ben.app.live().together.files.is_empty());
    // It was at no time a missing file there, and its clip is not marked.
    ben.idle(3);
    assert!(ben.app.snapshot().clip_problems.is_empty() && ben.app.snapshot().arriving.is_empty());
    assert!(!ben.is_on_screen("File missing"));
    assert!(!ben.project().to_json().contains("Elsewhere"), "and nothing Ben holds names where Ada keeps it");
}

/// A meeting point on this machine, as its keeper would hand it over.
fn meeting_point_on(listener: std::net::TcpListener, key: &trill_meet::Key) -> trill_meet::Server {
    let keys = trill_meet::Keys::of(vec![trill_meet::keys::Given::of(key, "Ada")]);
    let log: trill_meet::Log = std::sync::Arc::new(|_: &str| {});
    trill_meet::Server::start(vec![listener], keys, trill_meet::Limits::default(), log).unwrap()
}

/// About 8 connections.
#[test]
fn a_song_is_shared_through_a_meeting_point_set_in_the_panel_and_a_lost_link_is_said_until_it_is_back() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (at, key) = (format!("127.0.0.1:{port}"), trill_meet::Key::new().unwrap());
    let point = meeting_point_on(listener, &key);
    let mut ada = window("Ada", |session| {
        two_tracks(session);
    });
    let mut ben = window("Ben", |_| {});

    // Ada shares from the File menu, through a meeting point. None is
    // set: the panel asks for what its keeper gave, once.
    ada.click_label("File");
    ada.click_label("Share this song");
    ada.idle(2);
    ada.click_label("A meeting point");
    ada.idle(2);
    assert!(ada.is_on("A meeting point") && !ada.is_on("This computer only"));
    // What is no meeting point is said, and nothing is kept of it.
    ada.click_label("Meeting point's address");
    ada.type_text("https://meet.example.org");
    ada.click_label("Meeting point's key");
    ada.clipboard = Some(key.to_string());
    ada.paste();
    let key_on_screen = |ada: &Harness| ada.labels.iter().any(|(label, _)| label.contains(&key.to_string()[10..]));
    assert!(!key_on_screen(&ada), "the key is shown to nobody, as it is pasted either");
    ada.click_label("Keep this meeting point");
    ada.idle(2);
    assert!(ada.app.state().together.refusal.as_deref().is_some_and(|why| why.contains("meet.example.org:443")));
    assert!(ada.app.state().together.point_key.is_empty(), "and the key has left the window with the click");
    assert_eq!(ada.runtime.meeting_point(), None);
    // The address its keeper wrote, and the key again.
    ada.app.state_mut().together.point_address.clear();
    ada.click_label("Meeting point's address");
    ada.type_text(&at);
    ada.click_label("Meeting point's key");
    ada.paste();
    ada.click_label("Keep this meeting point");
    ada.wait_until("it is kept", |ada| ada.runtime.meeting_point().is_some());
    assert_eq!(ada.runtime.meeting_point(), Some(MeetingPointSet { address: at.clone(), has_key: true }));
    ada.idle(2);
    assert!(ada.is_on_screen(&format!("Meeting point: {at}")) && !key_on_screen(&ada));

    // Shared through it: the invitation carries the meeting point's
    // address, and a moment on the meeting is open there.
    ada.click_label("Share");
    ada.wait_until("the meeting is open", |ada| ada.app.live().together.meeting == Some((at.clone(), Meeting::Held)));
    let invitation = ada.app.live().together.invitation.clone().unwrap();
    assert!(invitation.starts_with(&format!("trill:meet:{at}:")), "{invitation}");
    assert!(ada.is_on_screen(&format!("Invitation: {invitation}")) && !key_on_screen(&ada));
    ada.key(Modifiers::NONE, Key::Escape);

    // Ben joins with the invitation and nothing else.
    ben.click_label("File");
    ben.click_label("Join a song");
    ben.idle(2);
    ben.clipboard = Some(invitation.clone());
    ben.paste();
    ben.click_label("Join");
    both_until(&mut ada, &mut ben, "Ben has the song and each sees the other", |ada, ben| {
        song(ada) == song(ben) && ada.app.live().together.people.len() == 1 && ben.app.live().together.people.len() == 1
    });
    assert!(ada.is_on_screen("Shared song: Here: Ben") && ben.is_on_screen("Shared song: Here: Ada"));
    ben.key(Modifiers::NONE, Key::Escape);

    // The meeting point goes down. Nobody left: each status bar says what
    // is away and that it is being tried again, for as long as it is.
    point.stop();
    both_until(&mut ada, &mut ben, "each says what is away", |ada, ben| {
        ada.is_on_screen("Shared song: Shared \u{00B7} nobody else is here \u{00B7} meeting point away, trying again")
            && ben.is_on_screen("Shared song: Connection to Ada lost \u{00B7} trying again")
    });
    let said = |harness: &Harness| harness.app.state().toasts.texts().join(" | ");
    assert!(said(&ben).contains("The connection to Ada was lost"), "{}", said(&ben));
    assert!(said(&ada).contains(&format!("The meeting point {at} is away")), "{}", said(&ada));
    // Each goes on meanwhile.
    ada.session.lock().edit(|project| project.set_tempo(98.0)).unwrap();
    ben.session.lock().edit(|project| Ok(project.add_track("Ben's"))).unwrap();

    // It is back, where it was: nobody does anything, and both sets of
    // work are in the song.
    let _point = meeting_point_on(std::net::TcpListener::bind(("127.0.0.1", port)).unwrap(), &key);
    both_until(&mut ada, &mut ben, "they are in the song together again", |ada, ben| {
        song(ada) == song(ben)
            && ada.is_on_screen("Shared song: Here: Ben")
            && ben.is_on_screen("Shared song: Here: Ada")
    });
    assert_eq!((ada.project().tracks.len(), ben.project().tempo), (3, 98.0));
    assert!(said(&ben).contains("The connection is back: you are in the song with Ada."), "{}", said(&ben));
    assert_eq!(ada.app.live().together.invitation.as_deref(), Some(invitation.as_str()));
}

#[test]
fn company_through_a_meeting_point_and_a_connection_that_was_lost_are_shown_as_they_are() {
    let options = UiOptions { together_scene: Some(TogetherScene::Meeting), ..Default::default() };
    let (mut harness, ..) = with_three_tracks(options);
    harness.idle(3);
    harness.click_label("Shared song: Here: Jo");
    harness.idle(2);
    assert!(harness.labels.iter().any(|(label, _)| label.starts_with("Invitation: trill:meet:meet.example.org:443:")));

    let options = UiOptions { together_scene: Some(TogetherScene::Lost), ..Default::default() };
    let (mut harness, ..) = with_three_tracks(options);
    harness.idle(3);
    harness.click_label("Shared song: Connection to Jo lost \u{00B7} trying again");
    harness.idle(2);
    assert!(harness.is_on_screen("Jo: the connection was lost; Trill has asked for it 3 times and goes on"));
}
