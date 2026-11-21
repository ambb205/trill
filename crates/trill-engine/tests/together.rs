//! Several sessions that hold one song: what each changes reaches the
//! others, each undoes its own work, a clash is a sentence with a way
//! back, and a session that goes away and comes back merges.
//!
//! No wire here: what one session says is handed to another by the test,
//! which is what the wire does (`trill-together`, `trill-runtime`).

use std::sync::Arc;
use trill_core::journal::Author;
use trill_core::merge::lot::{Lot, any_edit, check, note};
use trill_core::merge::{Change, Op, Said, Stamp, TrackOp, TrackSet};
use trill_core::project::{AutomationPoint, AutomationTarget};
use trill_core::{ChainRef, ClipId, DeviceId, Project, TrackId, TrackKind};
use trill_dsp::BuiltinFactory;
use trill_engine::{AudioBackend, Session};

fn session() -> Session {
    let mut session = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    session.hear_changes(true);
    session
}

/// A session with a song in it, shared, and another that was handed it.
fn two() -> (Session, Session) {
    let mut a = session();
    let keys = a.edit(|song| Ok(song.add_track("Keys"))).unwrap();
    a.edit(|song| Ok(song.add_track("Bass"))).unwrap();
    a.edit(|song| Ok(song.add_track_of_kind("Room", TrackKind::Return))).unwrap();
    a.add_device(ChainRef::Track(keys), "builtin:polysynth", None, None).unwrap();
    let verse = a.edit(|song| song.add_midi_clip(keys, "Verse", 0.0, 8.0)).unwrap();
    a.edit(|song| song.clip_mut(verse)?.add_notes(&[note(60, 0.0), note(64, 1.0), note(67, 2.0)])).unwrap();
    a.share();
    let mut b = session();
    b.take_song(&a.whole_song()).unwrap();
    a.take_said();
    assert_eq!(text(&a), text(&b), "the one who joined has the song");
    assert_eq!(a.shared_song(), b.shared_song());
    (a, b)
}

/// The song a session holds, as one who compares two sessions' reads it:
/// to the last id, and without what is each one's own.
fn text(session: &Session) -> String {
    let mut song = session.project().clone();
    song.count_ids_from(0);
    song.loop_region.enabled = false;
    for track in &mut song.tracks {
        track.solo = false;
    }
    song.to_json()
}

/// What `from` changed since it was last asked reaches `to`, under the
/// name `by`. Returns how many changes that was.
fn pass(from: &mut Session, to: &mut Session, by: &str) -> usize {
    let said = from.take_said();
    let count = said.len();
    let floors = from.floors();
    to.merge(by, said, Some(&floors), from.replica()).expect("what a session says is taken");
    count
}

fn meet(a: &mut Session, b: &mut Session) {
    pass(a, b, "Ada");
    pass(b, a, "Ben");
    assert_eq!(text(a), text(b), "both hold the same song");
    check(a.project());
}

fn track(session: &Session, name: &str) -> TrackId {
    session.project().tracks.iter().find(|track| track.name == name).map(|track| track.id).expect("the track is there")
}

fn clip(session: &Session, name: &str) -> ClipId {
    let clips = session.project().tracks.iter().flat_map(|track| &track.clips);
    clips.into_iter().find(|clip| clip.name == name).map(|clip| clip.id).expect("the clip is there")
}

#[test]
fn what_either_of_two_sessions_changes_the_other_has() {
    let (mut a, mut b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    let synth = a.project().track(keys).unwrap().devices[0].id;
    let revision = b.revision();

    // Notes, clips, tracks, devices and their values, automation, tempo.
    a.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    b.edit(|song| song.add_midi_clip(keys, "Chorus", 8.0, 8.0)).unwrap();
    let lead = a.edit(|song| Ok(song.add_track("Lead"))).unwrap();
    b.add_device(ChainRef::Track(keys), "builtin:reverb", None, None).unwrap();
    a.set_device_params(synth, &[("cutoff".to_owned(), 1234.0)]).unwrap();
    let fade = vec![AutomationPoint::new(0.0, -12.0), AutomationPoint::new(8.0, 0.0)];
    b.edit(|song| song.set_automation(AutomationTarget::TrackVolume { track: keys }, fade)).unwrap();
    a.edit(|song| song.set_tempo(96.0)).unwrap();
    b.edit(|song| song.set_tempo_at(16.0, 120.0, true)).unwrap();
    meet(&mut a, &mut b);

    let song = b.project();
    assert_eq!(song.clip(verse).unwrap().notes().len(), 4);
    assert!(song.track(lead).is_ok() && song.tempo == 96.0 && song.tempo_changes.len() == 1);
    assert_eq!(song.device(synth).unwrap().params["cutoff"], 1234.0);
    assert_eq!(song.track(keys).unwrap().devices.len(), 2);
    assert!(b.revision() > revision, "whoever draws the song looks again");
    assert!(b.is_modified(), "and it is a song with changes to save");

    // What someone else did is no step of one's own: taking back takes
    // back one's own last edit and leaves theirs.
    assert!(b.undo());
    assert!(b.project().tempo_changes.is_empty() && b.project().tempo == 96.0);
    // It is in the journal, under their name.
    let journal = b.journal().expect("a journal is kept");
    let theirs: Vec<String> = journal
        .entries()
        .filter(|entry| entry.by == Author::Other("Ada".into()))
        .map(|entry| entry.line())
        .collect();
    assert!(theirs.iter().any(|line| line.contains("Lead")), "{theirs:?}");

    // Ids made in a shared song are each program's own, and exact as
    // numbers in JSON.
    let chorus = clip(&a, "Chorus");
    assert!(chorus.0 > 1 << 20 && chorus.0 < 1 << 53 && lead.0 > 1 << 20);
    assert_ne!(chorus.0 >> 20, lead.0 >> 20);
}

#[test]
fn of_two_moves_of_one_clip_the_later_holds_and_the_other_is_told_once_and_can_put_theirs_back() {
    let (mut a, mut b) = two();
    let verse = clip(&a, "Verse");
    a.edit(|song| song.move_clip(verse, None, 8.0)).unwrap();
    b.edit(|song| song.move_clip(verse, None, 16.0)).unwrap();
    let (said_a, said_b) = (a.take_said(), b.take_said());
    let later = said_a[0].change.stamp.max(said_b[0].change.stamp);
    let (floors_a, floors_b) = (a.floors(), b.floors());
    a.merge("Ben", said_b, Some(&floors_b), b.replica()).unwrap();
    b.merge("Ada", said_a, Some(&floors_a), a.replica()).unwrap();
    assert_eq!(text(&a), text(&b));
    // The one whose move gave way is told, the other is not.
    let (loser, winner, name) = match later.replica() == a.replica().unwrap() {
        true => (&mut b, &mut a, "Ada"),
        false => (&mut a, &mut b, "Ben"),
    };
    assert!(winner.notices().is_empty());
    let [notice] = loser.notices() else { panic!("one sentence: {:?}", loser.notices()) };
    assert!(notice.text.contains(name) && notice.text.contains("where the clip \u{201C}Verse\u{201D} starts"), "{}", notice.text);
    assert_eq!(notice.remedy.as_ref().unwrap().label, "Put mine back");
    let mine = if name == "Ada" { 16.0 } else { 8.0 };
    // Putting one's own back is an ordinary edit: it travels, and it is a
    // step that can be taken back.
    let id = notice.id;
    loser.remedy(id).unwrap();
    assert!(loser.notices().is_empty());
    assert_eq!(loser.project().clip(verse).unwrap().start, mine);
    let (said, floors, number) = (loser.take_said(), loser.floors(), loser.replica());
    winner.merge("them", said, Some(&floors), number).unwrap();
    assert_eq!(winner.project().clip(verse).unwrap().start, mine);
    assert!(winner.notices().is_empty(), "made knowing of the other's move, it is no clash");
    assert!(loser.undo());
    assert_ne!(loser.project().clip(verse).unwrap().start, mine);
}

#[test]
fn two_people_on_one_fader_is_one_notice_and_not_a_list() {
    let (mut a, mut b) = two();
    let keys = track(&a, "Keys");
    a.begin_gesture();
    b.begin_gesture();
    for step in 1..=20 {
        a.edit(|song| song.set_track_volume(keys, -(step as f32))).unwrap();
        b.edit(|song| song.set_track_volume(keys, -(step as f32) - 0.5)).unwrap();
        pass(&mut a, &mut b, "Ada");
        pass(&mut b, &mut a, "Ben");
    }
    a.end_gesture();
    b.end_gesture();
    assert!(a.notices().len() + b.notices().len() <= 2, "{:?} {:?}", a.notices(), b.notices());
    assert!(a.notices().len() <= 1 && b.notices().len() <= 1);
    assert_eq!(text(&a), text(&b));
    // A drag is one step for whoever made it, whatever arrived meanwhile.
    assert!(a.undo());
    assert!(!a.project().track(keys).unwrap().volume_db.is_nan());
}

#[test]
fn a_track_that_is_deleted_while_another_writes_in_it_is_gone_said_and_brought_back_whole() {
    let (mut a, mut b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    a.edit(|song| song.remove_track(keys).map(drop)).unwrap();
    b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0), note(74, 5.0)])).unwrap();
    meet(&mut a, &mut b);
    assert!(a.project().track(keys).is_err() && b.project().track(keys).is_err(), "the track is gone");
    assert!(a.notices().is_empty());
    let [notice] = b.notices() else { panic!("the one who wrote in it is told: {:?}", b.notices()) };
    assert!(notice.text.contains("Ada removed the track \u{201C}Keys\u{201D}"), "{}", notice.text);
    assert_eq!(notice.remedy.as_ref().unwrap().label, "Bring it back");
    let id = notice.id;
    b.remedy(id).unwrap();
    meet(&mut a, &mut b);
    let song = a.project();
    assert_eq!(song.clip(verse).unwrap().notes().len(), 5, "as it was, with what was written meanwhile");
    assert_eq!(song.track(keys).unwrap().devices.len(), 1);
}

#[test]
fn of_two_tracks_made_to_listen_to_each_other_one_does_and_the_other_is_told_how_to_have_theirs() {
    let (mut a, mut b) = two();
    let (keys, bass) = (track(&a, "Keys"), track(&a, "Bass"));
    let on_keys = a.add_device(ChainRef::Track(keys), "builtin:compressor", None, None).unwrap();
    let on_bass = a.add_device(ChainRef::Track(bass), "builtin:compressor", None, None).unwrap();
    meet(&mut a, &mut b);
    a.set_sidechain(on_bass, Some(keys)).unwrap();
    b.set_sidechain(on_keys, Some(bass)).unwrap();
    meet(&mut a, &mut b);
    let listens = |session: &Session, id: DeviceId| session.project().device(id).unwrap().sidechain;
    let (first, second) = (listens(&a, on_bass), listens(&a, on_keys));
    assert!(first.is_some() != second.is_some(), "one of the two: {first:?} {second:?}");
    assert!(a.project().playing_order().is_some());
    let (told, other) = if first.is_some() { (&mut b, &mut a) } else { (&mut a, &mut b) };
    assert!(other.notices().is_empty());
    let [notice] = told.notices() else { panic!("the one whose edit waits is told: {:?}", told.notices()) };
    assert!(notice.text.contains("each wait for the other"), "{}", notice.text);
    assert_eq!(notice.remedy.as_ref().unwrap().label, "Have mine instead");
    let id = notice.id;
    told.remedy(id).unwrap();
    let (said, floors, number) = (told.take_said(), told.floors(), told.replica());
    other.merge("them", said, Some(&floors), number).unwrap();
    assert_eq!(text(told), text(other));
    assert!(first.is_some() != listens(other, on_bass).is_some(), "now the other way round");
    assert!(other.project().playing_order().is_some());
}

// ---- undo is one's own -------------------------------------------------------

#[test]
fn taking_a_step_back_leaves_what_someone_else_has_set_since_and_says_so() {
    let (mut a, mut b) = two();
    let keys = track(&a, "Keys");
    // One step of a's that sets two things.
    a.begin_gesture();
    a.edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    a.edit(|song| song.set_track_pan(keys, -0.5)).unwrap();
    a.end_gesture();
    meet(&mut a, &mut b);
    // b sets one of them, knowing of a's.
    b.edit(|song| song.set_track_volume(keys, -20.0)).unwrap();
    meet(&mut a, &mut b);
    assert!(a.notices().is_empty(), "set knowing of a's, it is no clash");

    let turned = a.take_back();
    assert!(turned.done);
    assert_eq!(turned.theirs, 1, "the volume stays theirs");
    assert_eq!(turned.sentence().as_deref(), Some("One thing stays as someone else has set it since."));
    meet(&mut a, &mut b);
    let found = b.project().track(keys).unwrap();
    assert_eq!((found.volume_db, found.pan), (-20.0, 0.0), "the pan is put back and the volume is theirs");
    // Bringing the step back brings back what it took back, and no more.
    assert_eq!(a.bring_again().theirs, 1);
    meet(&mut a, &mut b);
    let found = b.project().track(keys).unwrap();
    assert_eq!((found.volume_db, found.pan), (-20.0, -0.5));
}

#[test]
fn a_thing_ones_step_made_goes_with_anothers_work_in_it_and_they_can_bring_it_back() {
    let (mut a, mut b) = two();
    let keys = track(&a, "Keys");
    let idea = a.edit(|song| song.add_midi_clip(keys, "Idea", 16.0, 4.0)).unwrap();
    meet(&mut a, &mut b);
    b.edit(|song| song.clip_mut(idea)?.add_notes(&[note(60, 0.0), note(62, 1.0)])).unwrap();
    meet(&mut a, &mut b);

    assert_eq!(a.take_back(), trill_engine::Turned { done: true, theirs: 0 });
    meet(&mut a, &mut b);
    assert!(b.project().clip(idea).is_err(), "the clip a made is gone, though b worked in it");
    let [notice] = b.notices() else { panic!("b is told: {:?}", b.notices()) };
    assert!(notice.text.contains("the clip \u{201C}Idea\u{201D}"), "{}", notice.text);
    let id = notice.id;
    b.remedy(id).unwrap();
    meet(&mut a, &mut b);
    assert_eq!(a.project().clip(idea).unwrap().notes().len(), 2, "with what b wrote in it");
}

#[test]
fn a_thing_ones_step_removed_comes_back_with_what_others_did_to_it_meanwhile() {
    let (mut a, mut b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    a.edit(|song| song.remove_track(keys).map(drop)).unwrap();
    // b has not heard of it yet, and works on.
    b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(79, 6.0)])).unwrap();
    b.edit(|song| song.set_track_volume(keys, -9.0)).unwrap();
    meet(&mut a, &mut b);
    assert!(a.project().track(keys).is_err());
    assert!(a.undo());
    meet(&mut a, &mut b);
    let song = b.project();
    assert_eq!(song.track(keys).unwrap().volume_db, -9.0);
    assert_eq!(song.clip(verse).unwrap().notes().len(), 4);
}

#[test]
fn someone_elses_change_never_clears_ones_redo_and_each_undoes_their_own() {
    let (mut a, mut b) = two();
    let (keys, bass) = (track(&a, "Keys"), track(&a, "Bass"));
    a.edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    assert!(a.undo());
    assert!(a.can_redo());
    b.edit(|song| song.set_track_volume(bass, -3.0)).unwrap();
    b.edit(|song| song.set_track_pan(bass, 0.25)).unwrap();
    meet(&mut a, &mut b);
    assert!(a.can_redo(), "what b did is no edit of a's");
    assert!(a.redo());
    meet(&mut a, &mut b);
    assert_eq!(b.project().track(keys).unwrap().volume_db, -6.0);
    // Each takes back their own last step, whoever acted last.
    assert!(a.undo());
    assert!(b.undo());
    meet(&mut a, &mut b);
    let song = a.project();
    assert_eq!(song.track(keys).unwrap().volume_db, 0.0);
    assert_eq!((song.track(bass).unwrap().volume_db, song.track(bass).unwrap().pan), (-3.0, 0.0));
}

// ---- going away and coming back ----------------------------------------------

#[test]
fn a_shared_song_that_is_saved_closed_and_opened_again_merges_with_what_the_others_did_meanwhile() {
    let (mut a, b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    let mut b = b;
    b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    meet(&mut a, &mut b);

    // b saves and closes.
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("ours.trill");
    b.save(&path).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("\"shared\""), "a shared song is saved with what it takes to merge it again");
    assert!(Project::from_json(&written).is_ok(), "and is a song to whoever knows nothing of that");
    let floors_when_closed = b.floors();
    drop(b);

    // a goes on alone.
    a.edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    a.edit(|song| song.remove_clip(verse).map(drop)).unwrap();
    let lead = a.edit(|song| Ok(song.add_track("Lead"))).unwrap();
    a.take_said();

    // b opens the file again, another day, and works alone too.
    let mut b = session();
    b.load(&path).unwrap();
    assert_eq!(b.shared_song(), a.shared_song(), "it is the same shared song");
    assert_eq!(b.floors(), floors_when_closed);
    assert!(!b.is_modified());
    b.edit(|song| song.set_track_pan(keys, 0.5)).unwrap();
    b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(76, 5.0)])).unwrap();
    let chorus = b.edit(|song| song.add_midi_clip(keys, "Chorus", 8.0, 8.0)).unwrap();
    assert_ne!(b.replica(), a.replica(), "an opening is a program of its own");
    b.take_said();

    // They meet: each says how much it holds, and is handed the rest.
    let (for_b, for_a) = (a.since(&b.floors()), b.since(&a.floors()));
    assert!(for_b.iter().all(|said| !floors_when_closed.has(said.change.stamp)), "no more than b lacks");
    let (floors_a, floors_b) = (a.floors(), b.floors());
    a.merge("Ben", for_a, Some(&floors_b), None).unwrap();
    b.merge("Ada", for_b, Some(&floors_a), None).unwrap();
    assert_eq!(text(&a), text(&b), "both sets of work, merged");
    let song = a.project();
    assert!(song.track(lead).is_ok() && song.clip(chorus).is_ok());
    assert_eq!((song.track(keys).unwrap().volume_db, song.track(keys).unwrap().pan), (-6.0, 0.5));
    assert!(song.clip(verse).is_err(), "the clip a removed is gone");
    // b wrote a note in it while apart, and is told with the way back.
    assert!(b.notices().iter().any(|notice| notice.text.contains("the clip \u{201C}Verse\u{201D}")), "{:?}", b.notices());
    assert_eq!(a.floors(), b.floors());
    // And from here on they are together again.
    b.edit(|song| song.set_tempo(101.0)).unwrap();
    meet(&mut a, &mut b);
}

#[test]
fn a_song_nobody_shares_is_saved_as_it_always_was_and_keeps_small_ids() {
    let mut alone = session();
    let keys = alone.edit(|song| Ok(song.add_track("Keys"))).unwrap();
    let verse = alone.edit(|song| song.add_midi_clip(keys, "Verse", 0.0, 4.0)).unwrap();
    alone.edit(|song| song.clip_mut(verse)?.add_notes(&[note(60, 0.0)])).unwrap();
    alone.edit(|song| song.remove_clip(verse).map(drop)).unwrap();
    assert!(alone.undo());
    assert!(keys.0 < 10 && verse.0 < 10 && alone.project().clip(verse).unwrap().notes()[0].id < 10);
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mine.trill");
    alone.save(&path).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("\"shared\""), "nothing is kept for others who are not there");
    assert!(alone.shared_song().is_none() && alone.replica().is_none());
    assert!(alone.take_said().len() >= 4, "what is changed is kept to be said only while someone is there to hear");
    alone.hear_changes(false);
    alone.edit(|song| song.set_tempo(90.0)).unwrap();
    assert!(alone.take_said().is_empty());
}

// ---- what another program says is not trusted --------------------------------

#[test]
fn what_no_edit_could_have_made_is_refused_whole_and_the_song_is_as_it_was() {
    let (mut a, mut b) = two();
    let keys = track(&a, "Keys");
    let before = text(&b);
    let revision = b.revision();
    a.edit(|song| song.set_track_volume(keys, -6.0)).unwrap();
    let mut said = a.take_said();
    // A batch with one good change and one that no edit could have made.
    let bad = Change { stamp: Stamp(said[0].change.stamp.0 + 1, 99_999), ops: vec![Op::Track(keys, TrackOp::Set(TrackSet::Pan(7.0)))] };
    said.push(Said { change: bad, knew: None });
    let refused = b.merge("Mallory", said, None, None).unwrap_err();
    assert!(refused.to_string().contains("pan"), "{refused}");
    assert_eq!((text(&b), b.revision()), (before, revision), "nothing of the batch was taken");
    // Someone else's listening is not theirs to set here.
    let solo = Change { stamp: Stamp(1_000, 99_999), ops: vec![Op::Track(keys, TrackOp::Set(TrackSet::Solo(true)))] };
    assert!(b.merge("Mallory", vec![Said { change: solo, knew: None }], None, None).is_err());
    // A change that is larger than anything an edit makes.
    let many = (0..300_000).map(|_| Op::MasterVolume(0.0)).collect();
    let flood = Change { stamp: Stamp(1_001, 99_999), ops: many };
    assert!(b.merge("Mallory", vec![Said { change: flood, knew: None }], None, None).is_err());
    check(b.project());
    assert!(b.project().tracks.iter().all(|track| !track.solo));
}

#[test]
fn someone_who_goes_away_in_the_middle_of_a_drag_leaves_a_song_that_goes_on() {
    let (mut a, mut b) = two();
    let (keys, verse) = (track(&a, "Keys"), clip(&a, "Verse"));
    a.begin_gesture();
    for step in 1..=7 {
        a.edit(|song| song.move_clip(verse, None, step as f64)).unwrap();
        a.edit(|song| song.set_track_volume(keys, -(step as f32))).unwrap();
        if step <= 4 {
            pass(&mut a, &mut b, "Ada");
        }
    }
    // a is gone: no end to the drag, and what it said last is all there is.
    drop(a);
    let song = b.project();
    assert_eq!((song.clip(verse).unwrap().start, song.track(keys).unwrap().volume_db), (4.0, -4.0));
    check(song);
    // b goes on: nothing is held, and nothing waits for anyone.
    b.edit(|song| song.move_clip(verse, None, 2.0)).unwrap();
    assert!(b.undo());
    assert_eq!(b.project().clip(verse).unwrap().start, 4.0);
    b.play();
    b.housekeeping();
    b.stop();
}

// ---- three sessions, real edits ----------------------------------------------

/// Three sessions make real edits drawn by lot, and what each says
/// reaches the others shuffled, late and more than once, with a stretch
/// in which one of them is cut off. In the end all three hold the same
/// song.
#[test]
fn three_sessions_that_took_the_same_changes_in_any_order_hold_the_same_song() {
    for seed in 1..=6u64 {
        let mut lot = Lot::new(seed);
        let (a, b) = two();
        let mut c = session();
        let mut a = a;
        c.take_song(&a.whole_song()).unwrap();
        a.take_said();
        let mut sessions = [a, b, c];
        let mut on_the_way: Vec<Vec<Said>> = vec![Vec::new(); 3];
        let mut made = 0;
        for round in 0..60 {
            let cut = (20..40).contains(&round).then_some(seed as usize % 3);
            let who = lot.below(3);
            let edit = any_edit(&mut lot, sessions[who].project());
            if sessions[who].edit(edit).is_ok() {
                made += 1;
            }
            // Now and then someone takes their last step back.
            if lot.chance(8) {
                sessions[who].undo();
            }
            for said in sessions[who].take_said() {
                for (to, waiting) in on_the_way.iter_mut().enumerate().filter(|(to, _)| *to != who) {
                    waiting.push(said.clone());
                    if lot.chance(5) {
                        waiting.push(said.clone());
                    }
                    let _ = to;
                }
            }
            for to in (0..3).filter(|to| Some(*to) != cut) {
                while !on_the_way[to].is_empty() && !lot.chance(3) {
                    let at = lot.below(on_the_way[to].len());
                    let said = on_the_way[to].swap_remove(at);
                    let from_cut = cut.is_some_and(|cut| sessions[cut].replica() == Some(said.change.stamp.replica()));
                    if from_cut {
                        on_the_way[to].push(said);
                        break;
                    }
                    sessions[to].merge("someone", vec![said], None, None).unwrap();
                    assert!(sessions[to].take_said().is_empty(), "taking a change in says nothing new");
                }
            }
        }
        assert!(made > 15, "seed {seed}: enough of the edits were allowed ({made})");
        for (to, waiting) in on_the_way.iter_mut().enumerate() {
            while !waiting.is_empty() {
                let said = waiting.swap_remove(lot.below(waiting.len()));
                sessions[to].merge("someone", vec![said], None, None).unwrap();
            }
        }
        let texts: Vec<String> = sessions.iter().map(text).collect();
        assert_eq!(texts[0], texts[1], "seed {seed}");
        assert_eq!(texts[1], texts[2], "seed {seed}");
        check(sessions[0].project());
        // Each still takes back and brings back its own.
        for session in &mut sessions {
            while session.undo() {}
            while session.redo() {}
        }
    }
}
