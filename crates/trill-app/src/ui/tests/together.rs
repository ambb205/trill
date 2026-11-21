//! Two windows in one song: sharing and joining from the File menu, who
//! is here and what they hold, and a clash as a notice with its way back.
//! Each window is on a runtime of its own, and the two meet over this
//! machine's loopback as two programs do.

use super::*;
use crate::ui::TogetherScene;
use trill_core::merge::{Change, ClipOp, ClipSet, Floors, Op, Said, Stamp, TrackOp, TrackSet};

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
        track.solo = false;
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
