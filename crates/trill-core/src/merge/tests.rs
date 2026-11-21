//! The merge model held to what it promises, without a session or a wire:
//! programs here are a project, the properties it is derived from and a
//! number, and changes are handed from one to another by the test.

use super::*;
use crate::controllers::ControllerKind;
use crate::project::{AudioContent, AutomationPoint, AutomationTarget, ChainRef, EditError, Note, Project, TrackKind};
use crate::{Channel, ClipId, DeviceId, InstrumentProfile, Reference, TrackId};

/// A lot that is the same every time it is drawn from the same seed.
pub(super) struct Lot(u64);

impl Lot {
    pub fn new(seed: u64) -> Self {
        Lot(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, of: &'a [T]) -> Option<&'a T> {
        if of.is_empty() { None } else { of.get(self.below(of.len())) }
    }

    pub fn chance(&mut self, of: usize) -> bool {
        self.below(of) == 0
    }
}

/// A song as one who compares two programs' songs reads it: to the last
/// id, and without what is each program's own.
fn text(project: &Project) -> String {
    let mut project = project.clone();
    project.count_ids_from(0);
    project.loop_region = crate::LoopRegion { enabled: false, start: 0.0, end: 16.0 };
    for track in &mut project.tracks {
        track.solo = false;
        if let Some(reference) = &mut track.reference {
            reference.listening = false;
        }
    }
    project.to_json()
}

/// One program that holds the song.
pub(super) struct Peer {
    pub project: Project,
    pub doc: Doc,
    replica: Replica,
    next_id: u64,
    pub aside: Vec<Aside>,
}

impl Peer {
    /// A program that opens `song` and is given the number `replica`.
    pub fn new(song: &Project, replica: Replica) -> Self {
        let mut project = Project::from_json(&song.to_json()).expect("a song that was written is read");
        project.settle(None);
        let doc = Doc::from_project(&project);
        Peer { project, doc, replica, next_id: first_id(replica), aside: Vec::new() }
    }

    /// Makes an edit as a session does: the method on the project, then
    /// what it changed as a change, taken into its own properties like
    /// anyone's. `None` for an edit that was refused or changed nothing.
    pub fn edit<T>(&mut self, edit: impl FnOnce(&mut Project) -> Result<T, EditError>) -> Option<Change> {
        let before = self.project.clone();
        self.project.count_ids_from(self.next_id);
        if edit(&mut self.project).is_err() {
            self.project = before;
            return None;
        }
        self.project.settle(Some(&before));
        self.next_id = self.project.next_id();
        let ops = diff(&before, &self.project, &self.doc);
        if ops.is_empty() {
            self.project = before;
            return None;
        }
        let change = Change { stamp: Stamp(self.doc.clock() + 1, self.replica), ops };
        let won = self.doc.apply(&change);
        assert_eq!(won.len(), change.ops.len(), "every part of one's own change sets something: {:?}", change.ops);
        let derived = self.doc.derive();
        if self.aside.is_empty() && derived.aside.is_empty() {
            assert_eq!(
                text(&derived.project),
                text(&self.project),
                "the song worked out from the properties is the song the edit made; the change was {:?}",
                change.ops
            );
        }
        self.settle_on(derived);
        Some(change)
    }

    /// Takes in a change another program made.
    pub fn take(&mut self, change: &Change) {
        let change = change.clone().checked(self.doc.clock()).expect("a change a program made is one the model takes");
        self.doc.apply(&change);
        let derived = self.doc.derive();
        self.settle_on(derived);
    }

    fn settle_on(&mut self, derived: Derived) {
        let personal = self.project.clone();
        self.project = derived.project;
        self.project.count_ids_from(personal.next_id());
        self.aside = derived.aside;
        check(&self.project);
    }

    /// Writes one property, as an undo or "bring it back" does.
    pub fn write(&mut self, ops: Vec<Op>) -> Change {
        let change = Change { stamp: Stamp(self.doc.clock() + 1, self.replica), ops };
        self.doc.apply(&change);
        let derived = self.doc.derive();
        self.settle_on(derived);
        change
    }

    pub fn text(&self) -> String {
        text(&self.project)
    }
}

/// Holds a project to everything an edit method would have refused to
/// make: whatever the properties were, the song worked out from them is
/// one the model could have been edited into.
pub(super) fn check(project: &Project) {
    use std::collections::HashSet;
    let json = project.to_json();
    let kinds: std::collections::HashMap<TrackId, TrackKind> =
        project.tracks.iter().map(|track| (track.id, track.kind)).collect();
    assert_eq!(kinds.len(), project.tracks.len(), "no track twice");
    assert!(project.playing_order().is_some(), "the tracks can be played in an order: {json}");
    let mut clips = HashSet::new();
    let mut devices = HashSet::new();
    for track in &project.tracks {
        if track.kind != TrackKind::Normal {
            assert!(track.clips.is_empty() && track.group.is_none() && track.reference.is_none());
            assert!(track.profile.is_empty());
        }
        if let Some(group) = track.group {
            assert_eq!(kinds.get(&group), Some(&TrackKind::Group), "a group is a group track that is there");
        }
        for send in &track.sends {
            assert_ne!(track.kind, TrackKind::Return, "a return does not send");
            assert_eq!(kinds.get(&send.to), Some(&TrackKind::Return), "a send goes to a return that is there");
            assert!(send.level_db.is_finite() && send.level_db <= crate::project::MAX_VOLUME_DB);
        }
        assert!(track.sends.windows(2).all(|pair| pair[0].to < pair[1].to), "a send to a return once, in order");
        if track.reference.is_some() {
            let sent = |lane: &crate::AutomationLane| matches!(lane.target, AutomationTarget::Send { track: from, .. } if from == track.id);
            assert!(track.group.is_none() && track.sends.is_empty() && !project.automation.iter().any(sent));
            assert!(project.devices().all(|device| device.sidechain != Some(track.id)), "nothing listens to a reference");
        }
        assert!(track.volume_db.is_finite() && (-1.0..=1.0).contains(&track.pan));
        let mut last = (f64::NEG_INFINITY, ClipId(0));
        for clip in &track.clips {
            assert!(clips.insert(clip.id), "no clip twice");
            assert!(clip.start >= 0.0 && clip.length > 0.0 && clip.start.is_finite() && clip.length.is_finite());
            assert!((clip.start, clip.id) > last, "clips in order");
            last = (clip.start, clip.id);
            let mut notes = HashSet::new();
            for note in clip.notes() {
                assert!(note.id != 0 && notes.insert(note.id), "every note its own id");
                note.validate().expect("a note the model takes");
            }
            for curve in clip.controllers() {
                assert!(!curve.points.is_empty(), "a curve has a point");
                assert!(curve.points.windows(2).all(|pair| (pair[0].beat, pair[0].id) < (pair[1].beat, pair[1].id)));
            }
        }
        for device in &track.devices {
            assert!(devices.insert(device.id), "no device twice");
            if let Some(to) = device.sidechain {
                assert!(kinds.contains_key(&to) && to != track.id, "a device listens to another track that is there");
            }
        }
    }
    for device in &project.master.devices {
        assert!(devices.insert(device.id), "no device twice");
    }
    let mut targets = HashSet::new();
    for lane in &project.automation {
        assert!(targets.insert(lane.target.clone()), "one envelope a target");
        assert!(!lane.points.is_empty(), "an envelope has a point");
        assert!(lane.points.windows(2).all(|pair| (pair[0].beat, pair[0].id) < (pair[1].beat, pair[1].id)));
        match &lane.target {
            AutomationTarget::TrackVolume { track } | AutomationTarget::TrackPan { track } => {
                assert!(kinds.contains_key(track));
            }
            AutomationTarget::DeviceParam { device, .. } => assert!(devices.contains(device)),
            AutomationTarget::MasterVolume => {}
            AutomationTarget::Send { track, to } => {
                assert!(kinds.get(track).is_some_and(|kind| *kind != TrackKind::Return));
                assert_eq!(kinds.get(to), Some(&TrackKind::Return));
            }
        }
    }
    for pair in project.tempo_changes.windows(2) {
        let same = (pair[1].beat - pair[0].beat).abs() <= crate::tempo::SAME_BEAT;
        assert!(!same || (pair[0].ramp && !pair[1].ramp), "one beat holds a glide that arrives and a tempo that takes over");
    }
    for pair in project.meter_changes.windows(2) {
        assert!(pair[1].beat - pair[0].beat > crate::tempo::SAME_BEAT);
    }
    // And it is a song a file holds and gives back.
    let read = Project::from_json(&json).expect("the song is read back");
    assert_eq!(read.to_json(), json, "as it was written");
}

fn note(pitch: u8, start: f64) -> Note {
    Note { pitch, start, length: 0.5, velocity: 100, ..Default::default() }
}

/// A small song with one of everything.
pub(super) fn song() -> Project {
    let mut song = Project::new("Together");
    let keys = song.add_track("Keys");
    let bass = song.add_track("Bass");
    let drums = song.add_track_of_kind("Drums", TrackKind::Group);
    let room = song.add_track_of_kind("Room", TrackKind::Return);
    song.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
    let synth = song.add_device(ChainRef::Track(keys), "builtin:polysynth", "Synth", None).unwrap();
    song.add_device(ChainRef::Track(keys), "builtin:eq", "EQ", None).unwrap();
    song.add_device(ChainRef::Track(bass), "builtin:compressor", "Duck", None).unwrap();
    song.add_device(ChainRef::Track(room), "builtin:reverb", "Hall", None).unwrap();
    song.device_mut(synth).unwrap().params.insert("cutoff".into(), 900.0);
    let verse = song.add_midi_clip(keys, "Verse", 0.0, 8.0).unwrap();
    song.clip_mut(verse).unwrap().add_notes(&[note(60, 0.0), note(64, 1.0), note(67, 2.0)]).unwrap();
    let points = vec![AutomationPoint::new(0.0, 0.2), AutomationPoint::new(4.0, 0.9)];
    song.clip_mut(verse).unwrap().set_controller(ControllerKind::Controller(1), points).unwrap();
    song.add_midi_clip(bass, "Line", 0.0, 4.0).unwrap();
    song.add_audio_clip(bass, "Loop", 8.0, 4.0, AudioContent::new("loop.wav")).unwrap();
    song.set_send(keys, room, Some(-12.0)).unwrap();
    song.set_track_group(bass, Some(drums)).unwrap();
    let fade = vec![AutomationPoint::new(0.0, -6.0), AutomationPoint::new(8.0, 0.0)];
    song.set_automation(AutomationTarget::TrackVolume { track: keys }, fade).unwrap();
    song.set_tempo_at(16.0, 140.0, false).unwrap();
    song.add_marker(8.0, "Chorus").unwrap();
    song.settle(None);
    song
}

fn ids(project: &Project) -> (Vec<TrackId>, Vec<ClipId>, Vec<DeviceId>) {
    let tracks = project.tracks.iter().map(|track| track.id).collect();
    let clips = project.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.id).collect();
    let devices = project.devices().map(|device| device.id).collect();
    (tracks, clips, devices)
}

fn find_track(project: &Project, name: &str) -> TrackId {
    project.tracks.iter().find(|track| track.name == name).map(|track| track.id).expect("the track is there")
}

fn find_clip(project: &Project, name: &str) -> ClipId {
    let clips = project.tracks.iter().flat_map(|track| &track.clips);
    clips.into_iter().find(|clip| clip.name == name).map(|clip| clip.id).expect("the clip is there")
}

fn find_device(project: &Project, name: &str) -> DeviceId {
    project.devices().find(|device| device.name == name).map(|device| device.id).expect("the device is there")
}

/// Two programs that hold the song.
fn two() -> (Peer, Peer) {
    let song = song();
    (Peer::new(&song, 70_001), Peer::new(&song, 70_002))
}

/// Both take what the other did, and hold the same song.
fn meet(a: &mut Peer, b: &mut Peer, from_a: &[Change], from_b: &[Change]) {
    for change in from_b {
        a.take(change);
    }
    for change in from_a {
        b.take(change);
    }
    assert_eq!(a.text(), b.text(), "the same changes are the same song");
}

#[test]
fn a_song_is_the_song_its_properties_come_to() {
    let song = song();
    let derived = Doc::from_project(&song).derive();
    assert_eq!(text(&derived.project), text(&song));
    assert!(derived.aside.is_empty());
    check(&song);
}

// ---- the table ---------------------------------------------------------------

#[test]
fn two_people_who_write_notes_in_one_clip_at_once_both_have_them() {
    let (mut a, mut b) = two();
    let verse = find_clip(&a.project, "Verse");
    let from_a = a.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0), note(74, 5.0)])).unwrap();
    let from_b = b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(48, 4.0)])).unwrap();
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    let pitches: Vec<u8> = a.project.clip(verse).unwrap().notes().iter().map(|note| note.pitch).collect();
    assert_eq!(pitches, [60, 64, 67, 48, 72, 74], "all of them, in the clip's order");
}

#[test]
fn a_note_that_one_moves_while_another_changes_its_velocity_is_moved_and_has_the_velocity() {
    let (mut a, mut b) = two();
    let verse = find_clip(&a.project, "Verse");
    let moved = |song: &mut Project| {
        let mut notes = song.clip(verse)?.notes().to_vec();
        notes[1].start = 3.5;
        notes[1].pitch = 65;
        song.clip_mut(verse)?.set_notes(notes)
    };
    let louder = |song: &mut Project| {
        let mut notes = song.clip(verse)?.notes().to_vec();
        notes[1].velocity = 30;
        song.clip_mut(verse)?.set_notes(notes)
    };
    let from_a = a.edit(moved).unwrap();
    let from_b = b.edit(louder).unwrap();
    assert_eq!((from_a.ops.len(), from_b.ops.len()), (2, 1), "a change says the properties that were set");
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    let found = a.project.clip(verse).unwrap().notes().iter().find(|note| note.pitch == 65).copied().unwrap();
    assert_eq!((found.start, found.velocity), (3.5, 30), "both changes");
    assert_eq!(a.project.clip(verse).unwrap().notes().len(), 3, "and it is the one note still");
}

#[test]
fn of_two_moves_of_one_clip_the_later_holds_and_the_other_can_be_put_back() {
    let (mut a, mut b) = two();
    let verse = find_clip(&a.project, "Verse");
    let from_a = a.edit(|song| song.move_clip(verse, None, 8.0)).unwrap();
    let from_b = b.edit(|song| song.move_clip(verse, None, 16.0)).unwrap();
    assert_eq!(from_a.stamp.clock(), from_b.stamp.clock(), "made at the same moment");
    meet(&mut a, &mut b, &[from_a.clone()], &[from_b]);
    assert_eq!(a.project.clip(verse).unwrap().start, 16.0, "the later: of two at one count, the higher number's");
    // The one whose move gave way puts it back with an ordinary edit,
    // which is later than both.
    let back = a.edit(|song| song.move_clip(verse, None, 8.0)).unwrap();
    b.take(&back);
    assert_eq!((a.project.clip(verse).unwrap().start, b.project.clip(verse).unwrap().start), (8.0, 8.0));
    // What was said once is not said again by being heard again.
    b.take(&from_a);
    b.take(&back);
    assert_eq!(a.text(), b.text());
}

#[test]
fn of_two_turns_of_one_knob_the_later_holds() {
    let (mut a, mut b) = two();
    let synth = find_device(&a.project, "Synth");
    let turn = |to: f64| {
        move |song: &mut Project| {
            song.device_mut(synth)?.params.insert("cutoff".into(), to);
            Ok(())
        }
    };
    let first = a.edit(turn(1200.0)).unwrap();
    let second = a.edit(turn(1500.0)).unwrap();
    let theirs = b.edit(turn(400.0)).unwrap();
    // Theirs was made knowing neither of the two, so it is the earlier
    // of the three whatever order they arrive in.
    meet(&mut a, &mut b, &[second, first], &[theirs]);
    assert_eq!(a.project.device(synth).unwrap().params["cutoff"], 1500.0);
    // And another knob of the same device is another property.
    let resonance = b.edit(|song| {
        song.device_mut(synth)?.params.insert("resonance".into(), 0.7);
        Ok(())
    });
    let cutoff = a.edit(turn(300.0)).unwrap();
    meet(&mut a, &mut b, &[cutoff], &[resonance.unwrap()]);
    let params = &a.project.device(synth).unwrap().params;
    assert_eq!((params["cutoff"], params["resonance"]), (300.0, 0.7));
}

#[test]
fn a_device_added_to_a_chain_while_another_reorders_it_is_there_in_the_new_order() {
    let (mut a, mut b) = two();
    let keys = find_track(&a.project, "Keys");
    let from_a = a.edit(|song| song.add_device(ChainRef::Track(keys), "builtin:delay", "Echo", None)).unwrap();
    // The other puts the EQ in front of the synth.
    let from_b = b
        .edit(|song| {
            let track = song.track_mut(keys)?;
            track.devices.swap(0, 1);
            Ok(())
        })
        .unwrap();
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    let names: Vec<&str> = a.project.track(keys).unwrap().devices.iter().map(|device| device.name.as_str()).collect();
    assert_eq!(names, ["EQ", "Synth", "Echo"], "both");
}

#[test]
fn a_track_deleted_while_another_writes_notes_in_it_is_gone_and_comes_back_whole() {
    let (mut a, mut b) = two();
    let (keys, room) = (find_track(&a.project, "Keys"), find_track(&a.project, "Room"));
    let verse = find_clip(&a.project, "Verse");
    let before = a.text();
    let from_a = a.edit(|song| song.remove_track(keys).map(drop)).unwrap();
    assert_eq!(from_a.ops, [Thing::Track(keys).set_alive(false)], "what went with it is not said: it goes with it");
    let from_b = b.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    meet(&mut a, &mut b, &[from_a], &[from_b.clone()]);
    assert!(a.project.track(keys).is_err() && a.project.clip(verse).is_err(), "the track is gone, for both");
    assert!(a.project.automation.is_empty(), "and its envelope with it");
    // The one who wrote in it is the one who can tell: their work is
    // held in something that was removed.
    let written = from_b.ops[0].makes().remove(0);
    assert_eq!(b.doc.out_with(&written), Some(Thing::Track(keys)));
    assert!(b.doc.is_within(&Key::Note(verse, 1, ALIVE), &Thing::Track(keys)));
    // Brought back, it is whole: as it was, with the notes written
    // meanwhile, its send and its envelope.
    let back = b.write(vec![Thing::Track(keys).set_alive(true)]);
    a.take(&back);
    assert_eq!(a.text(), b.text());
    assert_eq!(a.project.clip(verse).unwrap().notes().len(), 4);
    assert_eq!(a.project.track(keys).unwrap().sends, [crate::Send { to: room, level_db: -12.0 }]);
    let mut as_it_was = Peer::new(&song(), 1);
    as_it_was.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])).unwrap();
    let _ = before;
    let same_but_ids = |text: String| text.lines().filter(|line| !line.contains("\"id\"")).collect::<Vec<_>>().join("\n");
    assert_eq!(same_but_ids(a.text()), same_but_ids(as_it_was.text()), "nothing of it was lost");
}

#[test]
fn of_two_tracks_made_to_listen_to_each_other_the_first_listens_and_the_other_is_set_aside() {
    let (mut a, mut b) = two();
    let (keys, bass) = (find_track(&a.project, "Keys"), find_track(&a.project, "Bass"));
    let (eq, duck) = (find_device(&a.project, "EQ"), find_device(&a.project, "Duck"));
    // Each is allowed where it is made.
    let from_a = a.edit(|song| song.set_sidechain(duck, Some(keys))).unwrap();
    let from_b = b.edit(|song| song.set_sidechain(eq, Some(bass))).unwrap();
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    // One of the two by a fixed rule: the one made first, and of two at
    // one count the lower number's. The other is held, and said.
    assert_eq!(a.project.device(duck).unwrap().sidechain, Some(keys));
    assert_eq!(a.project.device(eq).unwrap().sidechain, None);
    for peer in [&a, &b] {
        let [aside] = &peer.aside[..] else { panic!("one edit was set aside: {:?}", peer.aside) };
        assert_eq!(aside.key, Key::Device(eq, super::props::sidechain_prop()));
        assert_eq!((aside.why, aside.stamp.replica()), (derive::Why::Loop, 70_002));
        assert_eq!(aside.by, Some(Key::Device(duck, super::props::sidechain_prop())), "and what stood in its way");
    }
    // Whoever gave way has theirs by taking the other's away: an ordinary
    // edit, after which nothing is set aside.
    let theirs = b
        .edit(|song| {
            song.set_sidechain(duck, None)?;
            song.set_sidechain(eq, Some(bass))
        })
        .unwrap();
    a.take(&theirs);
    assert_eq!(a.text(), b.text());
    assert_eq!((a.project.device(eq).unwrap().sidechain, a.aside.len()), (Some(bass), 0));
}

#[test]
fn a_plugin_changed_in_two_windows_at_once_has_one_whole_state() {
    let (mut a, mut b) = two();
    let synth = find_device(&a.project, "Synth");
    let state = |bytes: &'static [u8]| {
        move |song: &mut Project| {
            song.device_mut(synth)?.state = Some(bytes.to_vec());
            Ok(())
        }
    };
    let from_a = a.edit(state(b"filter open, slow attack")).unwrap();
    let from_b = b.edit(state(b"filter shut, fast attack")).unwrap();
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    assert_eq!(a.project.device(synth).unwrap().state.as_deref(), Some(&b"filter shut, fast attack"[..]));
}

// ---- rules that span more than one thing ------------------------------------

#[test]
fn a_reference_that_is_given_a_send_at_the_same_moment_stays_a_reference_and_the_send_waits() {
    let (mut a, mut b) = two();
    let room = find_track(&a.project, "Room");
    let solo = a.edit(|song| Ok(song.add_track("Demo"))).unwrap();
    b.take(&solo);
    let demo = find_track(&a.project, "Demo");
    let from_a = a.edit(|song| song.set_reference(demo, Some(Reference::default()))).unwrap();
    let from_b = b.edit(|song| song.set_send(demo, room, Some(-6.0))).unwrap();
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    let track = a.project.track(demo).unwrap();
    assert!(track.reference.is_some() && track.sends.is_empty(), "a reference feeds nothing in the song");
    assert_eq!(a.aside[0].why, derive::Why::Reference);
    // When it is a reference no longer, the send that was set is there.
    let plain = a.edit(|song| song.set_reference(demo, None)).unwrap();
    b.take(&plain);
    assert_eq!(b.project.track(demo).unwrap().sends.len(), 1);
    assert!(b.aside.is_empty());
}

#[test]
fn an_envelope_whose_last_two_points_two_people_remove_is_gone() {
    let (mut a, mut b) = two();
    let keys = find_track(&a.project, "Keys");
    let target = AutomationTarget::TrackVolume { track: keys };
    let keep = |index: usize| {
        let target = target.clone();
        move |song: &mut Project| {
            let points = song.automation[0].points.clone();
            song.set_automation(target, vec![points[index]])
        }
    };
    let from_a = a.edit(keep(0)).unwrap();
    let from_b = b.edit(keep(1)).unwrap();
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    assert!(a.project.automation.is_empty(), "each removed one of two, and none is left: no envelope without a point");
    // A point written again makes it an envelope again.
    let again = a.edit(|song| song.set_automation(target.clone(), vec![AutomationPoint::new(2.0, -3.0)])).unwrap();
    b.take(&again);
    assert_eq!(b.project.automation.len(), 1);
}

#[test]
fn of_two_tempos_set_at_one_beat_the_later_is_the_tempo_there() {
    let (mut a, mut b) = two();
    let from_a = a.edit(|song| song.set_tempo_at(32.0, 90.0, false)).unwrap();
    let from_b = b.edit(|song| song.set_tempo_at(32.0, 150.0, false)).unwrap();
    let glide = a.edit(|song| song.set_tempo_at(32.0, 100.0, true)).unwrap();
    meet(&mut a, &mut b, &[from_a, glide], &[from_b]);
    let at: Vec<(f64, bool)> = (a.project.tempo_changes.iter())
        .filter(|change| change.beat == 32.0)
        .map(|change| (change.tempo, change.ramp))
        .collect();
    assert_eq!(at, [(100.0, true), (150.0, false)], "a glide that arrives and one tempo that takes over");
}

#[test]
fn a_clip_moved_to_a_track_that_is_deleted_at_the_same_moment_is_held_until_the_track_is_back() {
    let (mut a, mut b) = two();
    let (keys, bass) = (find_track(&a.project, "Keys"), find_track(&a.project, "Bass"));
    let line = find_clip(&a.project, "Line");
    let from_a = a.edit(|song| song.move_clip(line, Some(keys), 16.0)).unwrap();
    let from_b = b.edit(|song| song.remove_track(keys).map(drop)).unwrap();
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    assert!(a.project.clip(line).is_err(), "it is on a track that is gone");
    assert_eq!(a.doc.out_with(&Thing::Clip(line)), Some(Thing::Track(keys)));
    assert!(a.project.track(bass).unwrap().clips.iter().all(|clip| clip.id != line), "and not where it was");
    let back = a.write(vec![Thing::Track(keys).set_alive(true)]);
    b.take(&back);
    assert_eq!(b.project.clip_track(line).unwrap(), keys);
}

#[test]
fn a_group_that_is_deleted_lets_its_members_play_to_the_master_until_it_is_back() {
    let (mut a, mut b) = two();
    let (bass, drums) = (find_track(&a.project, "Bass"), find_track(&a.project, "Drums"));
    let gone = a.edit(|song| song.remove_track(drums).map(drop)).unwrap();
    assert_eq!(gone.ops.len(), 1, "that its member is no longer grouped follows, and is not said");
    b.take(&gone);
    assert_eq!(b.project.track(bass).unwrap().group, None);
    let back = a.write(vec![Thing::Track(drums).set_alive(true)]);
    b.take(&back);
    assert_eq!(b.project.track(bass).unwrap().group, Some(drums), "as it was");
    assert_eq!(a.text(), b.text());
}

// ---- what is one's own -------------------------------------------------------

#[test]
fn what_is_ones_own_listening_is_a_change_that_is_never_anyone_elses() {
    let (mut a, mut b) = two();
    let keys = find_track(&a.project, "Keys");
    let solo = a
        .edit(|song| {
            song.track_mut(keys)?.solo = true;
            song.set_loop(true, 4.0, 8.0)
        })
        .unwrap();
    assert!(solo.ops.iter().all(Op::is_personal), "{:?}", solo.ops);
    // A program that hears it from someone refuses it.
    assert!(solo.clone().checked(0).is_err());
    // And what is handed to a program that lacks everything has none of it.
    let all = a.doc.since(&Floors::default());
    assert!(all.iter().flat_map(|change| &change.ops).all(|op| !op.is_personal()));
    for change in &all {
        b.take(change);
    }
    assert!(!b.project.track(keys).unwrap().solo && !b.project.loop_region.enabled);
    assert!(a.project.track(keys).unwrap().solo && a.project.loop_region.enabled, "and one's own stays one's own");
}

// ---- ids ---------------------------------------------------------------------

#[test]
fn everything_a_change_names_has_an_id_that_needed_nobodys_leave() {
    let (mut a, mut b) = two();
    let (keys, verse) = (find_track(&a.project, "Keys"), find_clip(&a.project, "Verse"));
    let edit = |song: &mut Project| {
        let track = song.add_track("New");
        let clip = song.add_midi_clip(track, "Idea", 0.0, 4.0)?;
        song.clip_mut(clip)?.add_notes(&[note(60, 0.0), note(60, 0.0)])?;
        song.clip_mut(verse)?.add_notes(&[note(80, 6.0)])?;
        song.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None)?;
        song.add_marker(4.0, "Here")?;
        let points = vec![AutomationPoint::new(0.0, 0.0), AutomationPoint::new(0.0, 1.0)];
        song.set_automation(AutomationTarget::TrackPan { track: keys }, points)
    };
    let from_a = a.edit(edit).unwrap();
    let from_b = b.edit(edit).unwrap();
    // The same edit in two programs makes different things.
    let made = |change: &Change| -> Vec<Thing> { change.ops.iter().flat_map(Op::makes).collect() };
    let (of_a, of_b) = (made(&from_a), made(&from_b));
    assert_eq!(of_a.len(), 7, "a track, a clip with its notes, a note, a device, a marker and two points: {of_a:?}");
    assert!(of_a.iter().all(|thing| !of_b.contains(thing)));
    let number = |thing: &Thing| match thing {
        Thing::Marker(id) | Thing::Note(_, id) | Thing::Point(_, id) => *id,
        Thing::Track(id) => id.0,
        Thing::Clip(id) => id.0,
        Thing::Device(id) => id.0,
    };
    for thing in of_a.iter().chain(&of_b) {
        let id = number(thing);
        assert!(id > 0 && id <= MAX_ID, "{id} is exact as a number in JSON");
        assert!([70_001, 70_002].contains(&(id >> ID_BITS)), "and says whose it is");
    }
    meet(&mut a, &mut b, &[from_a], &[from_b]);
    assert_eq!(a.project.tracks.len(), 6);
    assert_eq!(a.project.clip(verse).unwrap().notes().len(), 5);
    // Two points at one beat are a step: which is arrived at and which is
    // left from is the same for everyone.
    let pan = |peer: &Peer| -> Vec<f64> {
        let lane = peer.project.automation.iter().find(|lane| matches!(lane.target, AutomationTarget::TrackPan { .. }));
        lane.unwrap().points.iter().map(|point| point.value).collect()
    };
    assert_eq!(pan(&a), pan(&b));
    // A song nobody shares counts from one.
    let mut alone = Peer::new(&Project::new("Alone"), 0);
    alone.next_id = alone.project.next_id();
    alone.edit(|song| Ok(song.add_track("First"))).unwrap();
    assert_eq!(alone.project.tracks[0].id, TrackId(1));
}

#[test]
fn a_note_that_is_copied_in_its_clip_is_another_note_and_the_one_that_stayed_keeps_its_id() {
    let mut peer = Peer::new(&song(), 70_001);
    let verse = find_clip(&peer.project, "Verse");
    let first = peer.project.clip(verse).unwrap().notes()[0];
    let change = peer
        .edit(|song| {
            // As a duplicate does: the copy is the note, moved.
            let copy = Note { start: 6.0, ..first };
            song.clip_mut(verse)?.add_notes(&[copy])
        })
        .unwrap();
    let notes = peer.project.clip(verse).unwrap().notes();
    assert_eq!(notes[0].id, first.id, "the one that is as it was is the one it was");
    assert!(notes[3].id != first.id && notes[3].start == 6.0);
    assert!(matches!(&change.ops[..], [Op::Notes(_, made)] if made.len() == 1), "one note was made: {:?}", change.ops);
}

// ---- a file, and a program that was away ------------------------------------

#[test]
fn a_shared_song_that_is_kept_and_read_again_holds_what_it_held() {
    let (mut a, mut b) = two();
    let (keys, verse) = (find_track(&a.project, "Keys"), find_clip(&a.project, "Verse"));
    let duck = find_device(&a.project, "Duck");
    let mut said = Vec::new();
    said.extend(a.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0)])));
    said.extend(a.edit(|song| song.clip_mut(verse)?.remove_notes(|note| note.pitch == 64).map(drop)));
    said.extend(a.edit(|song| song.set_sidechain(duck, Some(keys))));
    said.extend(a.edit(|song| song.move_track(keys, 2)));
    said.extend(a.edit(|song| song.remove_track(keys).map(drop)));
    let id = SongId(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
    let kept = a.doc.kept(id, &a.project);
    let written = serde_json::to_string(&kept).unwrap();
    let read: Kept = serde_json::from_str(&written).unwrap();
    assert_eq!(read, kept, "what is kept is written and read as it was");
    let again = Doc::from_kept(&a.project, &read);
    assert_eq!(again, a.doc, "and with the song it is everything that was held: {written}");
    // A program that opens it brings back what was removed before it was
    // closed, and the other, who was there all along, has it too.
    let mut c = Peer::new(&a.project, 70_003);
    c.doc = again;
    for change in &said {
        b.take(change);
    }
    let back = c.write(vec![Thing::Track(keys).set_alive(true)]);
    b.take(&back);
    assert_eq!(b.text(), c.text());
    assert_eq!(b.project.clip(verse).unwrap().notes().len(), 3);
}

#[test]
fn a_program_that_was_away_is_handed_what_it_lacks_and_no_more() {
    let (mut a, mut b) = two();
    let (keys, verse) = (find_track(&a.project, "Keys"), find_clip(&a.project, "Verse"));
    // Together for a while: each has what the other did.
    let early = a.edit(|song| song.set_track_volume(keys, -3.0)).unwrap();
    b.take(&early);
    a.doc.floors_mut().raise(70_001, early.stamp.clock());
    b.doc.floors_mut().raise(70_001, early.stamp.clock());
    // Then apart, each working on.
    let mut apart = Vec::new();
    apart.extend(a.edit(|song| song.clip_mut(verse)?.add_notes(&[note(72, 4.0), note(76, 5.0)])));
    apart.extend(a.edit(|song| song.set_track_volume(keys, -9.0)));
    apart.extend(a.edit(|song| song.set_track_volume(keys, -12.0)));
    apart.extend(a.edit(|song| Ok(song.add_track("Lead"))));
    let theirs = b.edit(|song| song.set_track_pan(keys, 0.5)).unwrap();
    // They meet: each says how much of everyone's it holds, and is handed
    // what lies above that.
    let for_b = a.doc.since(b.doc.floors());
    let for_a = b.doc.since(a.doc.floors());
    let parts = |changes: &[Change]| changes.iter().map(|change| change.ops.len()).sum::<usize>();
    assert_eq!(parts(&for_a), 1, "the pan");
    assert_eq!(parts(&for_b), 3, "the notes, the volume as it is now and the track: not every step on the way");
    assert!(for_b.iter().all(|change| change.stamp > early.stamp));
    meet(&mut a, &mut b, &for_b, &for_a);
    assert_eq!(b.project.track(keys).unwrap().volume_db, -12.0);
    let _ = (apart, theirs);
}

// ---- what another program says is not trusted --------------------------------

#[test]
fn a_change_no_edit_could_have_made_is_refused_in_a_sentence() {
    let song = song();
    let (keys, verse) = (find_track(&song, "Keys"), find_clip(&song, "Verse"));
    let synth = find_device(&song, "Synth");
    let refused = |op: Op| {
        let change = Change { stamp: Stamp(5, 9), ops: vec![op.clone()] };
        change.checked(0).expect_err(&format!("{op:?} is not taken")).to_string()
    };
    let bad_note = Note { pitch: 200, id: 7, ..Default::default() };
    refused(Op::Notes(verse, vec![bad_note]));
    refused(Op::Notes(verse, vec![Note { id: 0, ..Default::default() }]));
    refused(Op::Note(verse, 7, NoteOp::Set(NoteSet::Velocity(0))));
    refused(Op::Note(verse, 7, NoteOp::Set(NoteSet::Start(f64::NAN))));
    refused(Op::Note(verse, 1 << 60, NoteOp::Alive(false)));
    refused(Op::Track(keys, TrackOp::Set(TrackSet::Pan(3.0))));
    refused(Op::Track(keys, TrackOp::Set(TrackSet::Volume(40.0))));
    refused(Op::Track(keys, TrackOp::Set(TrackSet::Name("n".repeat(MAX_TEXT + 1)))));
    refused(Op::Track(keys, TrackOp::Set(TrackSet::Solo(true))));
    refused(Op::Track(keys, TrackOp::Place(f64::INFINITY)));
    refused(Op::Song(SongSet::Tempo(5.0)));
    refused(Op::Song(SongSet::Loop(crate::LoopRegion { enabled: true, start: 0.0, end: 4.0 })));
    refused(Op::Tempo(0.0, false, Some(120.0)));
    refused(Op::Tempo(8.0, false, Some(-1.0)));
    refused(Op::Clip(verse, ClipOp::Set(ClipSet::Length(0.0))));
    refused(Op::Clip(verse, ClipOp::Audio(AudioSet::Offset(-2.0))));
    refused(Op::Param(synth, "cutoff".into(), Some(f64::NAN)));
    refused(Op::Device(synth, DeviceOp::Set(DeviceSet::State(Blob(Some(vec![0; MAX_STATE_BYTES + 1]))))));
    let curve = Home::Curve(verse, Channel::FIRST, ControllerKind::Controller(1));
    refused(Op::Points(curve, vec![AutomationPoint { id: 3, ..AutomationPoint::new(0.0, 7.0) }]));
    // A change from nowhere, and one from far in the future.
    assert!(Change { stamp: Stamp::ZERO, ops: Vec::new() }.checked(0).is_err());
    assert!(Change { stamp: Stamp(u64::MAX, 3), ops: Vec::new() }.checked(10).is_err());
    // What the model would have clamped is clamped, not refused.
    let quiet = Change { stamp: Stamp(5, 9), ops: vec![Op::MasterVolume(-300.0)] }.checked(0).unwrap();
    assert_eq!(quiet.ops, [Op::MasterVolume(crate::project::MIN_VOLUME_DB)]);
    // And a track that is made brings nothing of anyone's own with it.
    let mut loud = song.track(keys).unwrap().clone();
    loud.solo = true;
    let made = Op::Track(TrackId(99), TrackOp::Whole(Box::new(loud), 9.0)).checked().unwrap();
    assert!(matches!(made, Op::Track(_, TrackOp::Whole(track, _)) if !track.solo && track.clips.is_empty()));
}

/// A part of a change that is allowed one by one and names whatever it
/// likes: things that are there, things that are not, kinds that do not
/// fit.
fn any_op(lot: &mut Lot, project: &Project) -> Op {
    let (tracks, clips, devices) = ids(project);
    let wild = |lot: &mut Lot| 1 + lot.below(40) as u64;
    let track = |lot: &mut Lot| match lot.chance(4) {
        true => TrackId(wild(lot)),
        false => lot.pick(&tracks).copied().unwrap_or(TrackId(1)),
    };
    let clip = |lot: &mut Lot| match lot.chance(4) {
        true => ClipId(wild(lot)),
        false => lot.pick(&clips).copied().unwrap_or(ClipId(1)),
    };
    let device = |lot: &mut Lot| match lot.chance(4) {
        true => DeviceId(wild(lot)),
        false => lot.pick(&devices).copied().unwrap_or(DeviceId(1)),
    };
    let chain = |lot: &mut Lot| match lot.chance(5) {
        true => ChainRef::Master,
        false => ChainRef::Track(track(lot)),
    };
    let beat = |lot: &mut Lot| lot.below(64) as f64 / 2.0;
    let target = |lot: &mut Lot| match lot.below(5) {
        0 => AutomationTarget::TrackVolume { track: track(lot) },
        1 => AutomationTarget::TrackPan { track: track(lot) },
        2 => AutomationTarget::DeviceParam { device: device(lot), param: "cutoff".into() },
        3 => AutomationTarget::MasterVolume,
        _ => AutomationTarget::Send { track: track(lot), to: track(lot) },
    };
    let kinds = [TrackKind::Normal, TrackKind::Group, TrackKind::Return];
    match lot.below(22) {
        0 => {
            let mut made = Project::new("made");
            let id = made.add_track_of_kind("Wild", kinds[lot.below(3)]);
            let mut whole = made.track(id).unwrap().clone();
            whole.group = lot.chance(2).then(|| track(lot));
            whole.reference = lot.chance(4).then(Reference::default);
            Op::Track(track(lot), TrackOp::Whole(Box::new(whole), beat(lot)))
        }
        1 => Op::Track(track(lot), TrackOp::Alive(lot.chance(2))),
        2 => Op::Track(track(lot), TrackOp::Place(beat(lot))),
        3 => Op::Track(track(lot), TrackOp::Set(TrackSet::Group(lot.chance(3).then(|| track(lot)).or(Some(track(lot)))))),
        4 => Op::Track(track(lot), TrackOp::Reference(lot.chance(2).then(Beside::default))),
        5 => Op::Send(track(lot), track(lot), lot.chance(4).then_some(-6.0).or(Some(-3.0))),
        6 => {
            let mut made = Project::new("made");
            let on = made.add_track("t");
            let id = match lot.chance(3) {
                true => made.add_audio_clip(on, "Wild", beat(lot), 4.0, AudioContent::new("wild.wav")).unwrap(),
                false => made.add_midi_clip(on, "Wild", beat(lot), 4.0).unwrap(),
            };
            let mut whole = made.clip(id).unwrap().clone();
            if whole.midi().is_some() {
                whole.add_notes(&[Note { id: wild(lot), ..note(60, 0.0) }]).unwrap();
            }
            Op::Clip(clip(lot), ClipOp::Whole(Box::new(whole), track(lot)))
        }
        7 => Op::Clip(clip(lot), ClipOp::Alive(lot.chance(2))),
        8 => Op::Clip(clip(lot), ClipOp::Track(track(lot))),
        9 => Op::Clip(clip(lot), ClipOp::Set(ClipSet::Start(beat(lot)))),
        10 => Op::Notes(clip(lot), vec![Note { id: wild(lot), ..note(40 + lot.below(40) as u8, beat(lot)) }]),
        11 => Op::Note(clip(lot), wild(lot), NoteOp::Alive(lot.chance(2))),
        12 => {
            let mut made = Project::new("made");
            let id = made.add_device(ChainRef::Master, "builtin:compressor", "Wild", None).unwrap();
            let mut whole = made.device(id).unwrap().clone();
            whole.sidechain = lot.chance(2).then(|| track(lot));
            Op::Device(device(lot), DeviceOp::Whole(Box::new(whole), chain(lot), beat(lot)))
        }
        13 => Op::Device(device(lot), DeviceOp::Alive(lot.chance(2))),
        14 => Op::Device(device(lot), DeviceOp::At(chain(lot), beat(lot))),
        15 => Op::Device(device(lot), DeviceOp::Set(DeviceSet::Sidechain(Some(track(lot))))),
        16 => Op::Points(Home::Lane(target(lot)), vec![AutomationPoint { id: wild(lot), ..AutomationPoint::new(beat(lot), -6.0) }]),
        17 => Op::Point(Home::Lane(target(lot)), wild(lot), PointOp::Alive(lot.chance(2))),
        18 => Op::Tempo(1.0 + beat(lot), lot.chance(2), lot.chance(5).then_some(90.0).or(Some(130.0))),
        19 => Op::Meter(1.0 + beat(lot), Some(crate::TimeSignature { numerator: 3, denominator: 4 })),
        20 => Op::Marker(wild(lot), MarkerOp::Whole(crate::Marker { beat: beat(lot), name: "Wild".into(), id: 0 })),
        _ => Op::Track(track(lot), TrackOp::Set(TrackSet::Profile(InstrumentProfile::default()))),
    }
}

#[test]
fn whatever_another_program_says_the_song_is_one_an_edit_could_have_made() {
    for seed in 1..=40 {
        let mut lot = Lot::new(seed);
        let mut peer = Peer::new(&song(), 70_001);
        let mut other = Peer::new(&song(), 70_002);
        let mut said = Vec::new();
        for count in 1..=120u64 {
            let ops: Vec<Op> = (0..1 + lot.below(3)).map(|_| any_op(&mut lot, &peer.project)).collect();
            let change = Change { stamp: Stamp(1 + lot.below(60) as u64, 3 + count % 5), ops };
            // (`take` holds the song it comes to to the model's rules.)
            if let Ok(change) = change.checked(peer.doc.clock()) {
                peer.take(&change);
                said.push(change);
            }
        }
        // And in another order it is the same song.
        while !said.is_empty() {
            let change = said.swap_remove(lot.below(said.len()));
            other.take(&change);
        }
        let (one, two) = (peer.text(), other.text());
        let differ = one.lines().zip(two.lines()).position(|(a, b)| a != b);
        let around = |text: &str| {
            let from = differ.unwrap_or(0).saturating_sub(12);
            text.lines().skip(from).take(24).collect::<Vec<_>>().join("
")
        };
        assert!(one == two, "seed {seed}:
{}
---- and in another order ----
{}", around(&one), around(&two));
    }
}

// ---- three programs, real edits ----------------------------------------------

/// An edit drawn by lot from the model's edit methods, on whatever the
/// song holds: some are refused, as edits are.
fn any_edit(lot: &mut Lot, project: &Project) -> Box<dyn FnOnce(&mut Project) -> Result<(), EditError>> {
    let (tracks, clips, devices) = ids(project);
    let track = lot.pick(&tracks).copied().unwrap_or(TrackId(1));
    let other = lot.pick(&tracks).copied().unwrap_or(TrackId(1));
    let clip = lot.pick(&clips).copied().unwrap_or(ClipId(1));
    let device = lot.pick(&devices).copied().unwrap_or(DeviceId(1));
    let beat = lot.below(64) as f64 / 2.0;
    let (a, b, c) = (lot.below(1000), lot.below(1000), lot.below(1000));
    let level = -(lot.below(40) as f32);
    let target = match a % 5 {
        0 => AutomationTarget::TrackVolume { track },
        1 => AutomationTarget::TrackPan { track },
        2 => AutomationTarget::DeviceParam { device, param: "cutoff".into() },
        3 => AutomationTarget::MasterVolume,
        _ => AutomationTarget::Send { track, to: other },
    };
    let kinds = [TrackKind::Normal, TrackKind::Normal, TrackKind::Group, TrackKind::Return];
    match lot.below(46) {
        0 => Box::new(move |song| Ok(drop(song.add_track_of_kind(format!("Track {a}"), kinds[b % 4])))),
        1 => Box::new(move |song| song.remove_track(track).map(drop)),
        2 => Box::new(move |song| song.move_track(track, b % 6)),
        3 => Box::new(move |song| song.duplicate_track(track).map(drop)),
        4 => Box::new(move |song| song.set_track_volume(track, level)),
        5 => Box::new(move |song| song.set_track_pan(track, (a % 21) as f32 / 10.0 - 1.0)),
        6 => Box::new(move |song| {
            song.track_mut(track)?.name = format!("Renamed {a}");
            song.track_mut(track)?.mute = b % 2 == 0;
            Ok(())
        }),
        7 => Box::new(move |song| song.set_track_group(track, (a % 3 != 0).then_some(other))),
        8 => Box::new(move |song| song.set_send(track, other, (a % 4 != 0).then_some(level))),
        9 => Box::new(move |song| song.set_reference(track, (a % 2 == 0).then(Reference::default))),
        10 | 11 => Box::new(move |song| song.add_midi_clip(track, format!("Clip {a}"), beat, 4.0).map(drop)),
        12 => Box::new(move |song| song.add_audio_clip(track, "Audio", beat, 2.0, AudioContent::new("a.wav")).map(drop)),
        13 => Box::new(move |song| song.remove_clip(clip).map(drop)),
        14 => Box::new(move |song| song.move_clip(clip, (a % 2 == 0).then_some(track), beat)),
        15 => Box::new(move |song| song.set_clip_span(clip, beat, 1.0 + (a % 8) as f64)),
        16 => Box::new(move |song| song.split_clip(clip, beat).map(drop)),
        17 => Box::new(move |song| song.duplicate_clip(clip, beat, (a % 2 == 0).then_some(track)).map(drop)),
        18 => Box::new(move |song| song.trim_clip_start(clip, beat).map(drop)),
        19 => Box::new(move |song| song.loop_clip(clip, a % 2 == 0)),
        20 => Box::new(move |song| {
            song.clip_mut(clip)?.name = format!("Named {a}");
            song.clip_mut(clip)?.muted = b % 3 == 0;
            Ok(())
        }),
        21..=24 => Box::new(move |song| {
            let notes: Vec<Note> = (0..1 + a % 4).map(|n| note(48 + ((b + n * 7) % 36) as u8, (c % 16 + n) as f64 / 2.0)).collect();
            song.clip_mut(clip)?.add_notes(&notes)
        }),
        25 => Box::new(move |song| song.clip_mut(clip)?.remove_notes(|note| usize::from(note.pitch) % 3 == a % 3).map(drop)),
        26 | 27 => Box::new(move |song| {
            // Some notes moved, some made louder: as a drag does.
            let mut notes = song.clip(clip)?.notes().to_vec();
            for (index, note) in notes.iter_mut().enumerate() {
                match (index + a) % 3 {
                    0 => note.start += 0.5,
                    1 => note.velocity = 1 + (b % 127) as u8,
                    _ => {}
                }
            }
            song.clip_mut(clip)?.set_notes(notes)
        }),
        28 => Box::new(move |song| {
            let points = vec![AutomationPoint::new(0.0, (a % 100) as f64 / 100.0), AutomationPoint::new(beat, 0.5)];
            song.clip_mut(clip)?.set_controller(ControllerKind::Controller(1 + (b % 2) as u8), points)
        }),
        29 => Box::new(move |song| {
            let uri = ["builtin:eq", "builtin:compressor", "builtin:delay"][a % 3];
            let chain = if b % 6 == 0 { ChainRef::Master } else { ChainRef::Track(track) };
            song.add_device(chain, uri, format!("Device {a}"), (c % 2 == 0).then_some(c % 3)).map(drop)
        }),
        30 => Box::new(move |song| song.remove_device(device).map(drop)),
        31 => Box::new(move |song| {
            let chain = song.device_chain(device)?;
            let ChainRef::Track(on) = chain else { return Ok(()) };
            song.track_mut(on)?.devices.rotate_left(1);
            Ok(())
        }),
        32 | 33 => Box::new(move |song| {
            song.device_mut(device)?.params.insert(["cutoff", "resonance", "mix"][a % 3].into(), b as f64);
            Ok(())
        }),
        34 => Box::new(move |song| {
            let found = song.device_mut(device)?;
            found.enabled = a % 2 == 0;
            found.state = (b % 3 != 0).then(|| vec![a as u8; 1 + b % 5]);
            Ok(())
        }),
        35 => Box::new(move |song| song.set_sidechain(device, (a % 3 != 0).then_some(track))),
        36 | 37 => Box::new(move |song| {
            // An envelope written anew, or one of its points moved.
            let held = song.automation.iter().find(|lane| lane.target == target).map(|lane| lane.points.clone());
            let points = match held.filter(|_| a % 2 == 0) {
                Some(mut points) => {
                    let at = b % points.len();
                    points[at].value -= 1.0;
                    points[at].beat += 0.25;
                    if c % 4 == 0 {
                        points.remove(at);
                    }
                    points
                }
                None => vec![AutomationPoint::new(beat, -6.0), AutomationPoint::new(beat + 4.0, -1.0)],
            };
            song.set_automation(target, points)
        }),
        38 => Box::new(move |song| song.set_automation_enabled(&target, a % 2 == 0)),
        39 => Box::new(move |song| song.set_tempo_at(beat, 80.0 + (a % 90) as f64, b % 3 == 0)),
        40 => Box::new(move |song| song.remove_tempo_at(beat)),
        41 => Box::new(move |song| song.set_time_signature_at(beat, 3 + (a % 4) as u8, 4)),
        42 => Box::new(move |song| song.add_marker(beat, format!("Marker {a}")).map(drop)),
        43 => Box::new(move |song| song.move_marker(a % 3, beat).map(drop)),
        44 => Box::new(move |song| song.remove_marker(a % 3).map(drop)),
        _ => Box::new(move |song| {
            song.set_key((a % 2 == 0).then(|| crate::SongKey::new((b % 12) as u8, c % 2 == 0)));
            song.name = format!("Song {a}");
            song.set_tempo(90.0 + (c % 60) as f64)
        }),
    }
}

/// Three programs make real edits, and what each made reaches the others
/// shuffled, late and more than once, with a stretch in which one of them
/// hears nothing and is not heard. In the end all three hold the same
/// song, the song is one the model could have been edited into, and what
/// each holds can be kept in a file and read again as it was.
#[test]
fn three_programs_that_took_the_same_changes_in_any_order_hold_the_same_song() {
    for seed in 1..=60u64 {
        let mut lot = Lot::new(seed);
        let song = song();
        let mut peers: Vec<Peer> = (0..3).map(|index| Peer::new(&song, 70_001 + index)).collect();
        // What is on its way to each program.
        let mut on_the_way: Vec<Vec<Change>> = vec![Vec::new(); 3];
        let mut made = 0;
        for round in 0..90 {
            // One of them is cut off for a third of the time: what it
            // makes waits, and so does what is made for it.
            let cut = (30..60).contains(&round).then_some(seed as usize % 3);
            let who = lot.below(3);
            let edit = any_edit(&mut lot, &peers[who].project);
            if let Some(change) = peers[who].edit(edit) {
                made += 1;
                for (to, waiting) in on_the_way.iter_mut().enumerate().filter(|(to, _)| *to != who) {
                    waiting.push(change.clone());
                    // Said twice, now and then.
                    if lot.chance(5) {
                        waiting.push(change.clone());
                    }
                    let _ = to;
                }
            }
            // Some of what is on its way arrives, in no order.
            for to in (0..3).filter(|to| Some(*to) != cut) {
                while !on_the_way[to].is_empty() && !lot.chance(3) {
                    let at = lot.below(on_the_way[to].len());
                    let change = on_the_way[to].swap_remove(at);
                    // (Not from the one that is cut off.)
                    if cut.is_some_and(|cut| change.stamp.replica() == 70_001 + cut as u64) {
                        on_the_way[to].push(change);
                        break;
                    }
                    peers[to].take(&change);
                }
            }
            if round % 15 == 14 {
                for peer in &peers {
                    let kept = peer.doc.kept(SongId(seed as u128), &peer.project);
                    let read: Kept = serde_json::from_str(&serde_json::to_string(&kept).unwrap()).unwrap();
                    assert_eq!(Doc::from_kept(&peer.project, &read), peer.doc, "seed {seed}, round {round}");
                }
            }
        }
        assert!(made > 20, "seed {seed}: enough of the edits were allowed ({made})");
        // Everything arrives.
        for (to, waiting) in on_the_way.iter_mut().enumerate() {
            while !waiting.is_empty() {
                let change = waiting.swap_remove(lot.below(waiting.len()));
                peers[to].take(&change);
            }
        }
        let texts: Vec<String> = peers.iter().map(Peer::text).collect();
        assert_eq!(texts[0], texts[1], "seed {seed}");
        assert_eq!(texts[1], texts[2], "seed {seed}");
        // And what each holds says the same to a program that has nothing.
        let all: Vec<Vec<Change>> = peers.iter().map(|peer| peer.doc.since(&Floors::default())).collect();
        let mut late = Peer::new(&song, 70_009);
        for change in &all[seed as usize % 3] {
            late.take(change);
        }
        assert_eq!(late.text(), texts[0], "seed {seed}: a program handed everything at once holds the same song");
    }
}
