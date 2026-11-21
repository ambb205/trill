//! Songs and edits drawn by lot, for the tests of everything that merges:
//! here, in the session, over the wire and in the window. A lot is the
//! same every time it is drawn from the same seed, so a test that fails
//! fails again.

use crate::controllers::ControllerKind;
use crate::project::{AudioContent, AutomationPoint, AutomationTarget, ChainRef, EditError, Note, Project, TrackKind};
use crate::{ClipId, DeviceId, Reference, TrackId};

/// A lot that is the same every time it is drawn from the same seed.
pub struct Lot(u64);

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

/// Holds a project to everything an edit method would have refused to
/// make: whatever the properties were, the song worked out from them is
/// one the model could have been edited into.
pub fn check(project: &Project) {
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

pub fn note(pitch: u8, start: f64) -> Note {
    Note { pitch, start, length: 0.5, velocity: 100, ..Default::default() }
}

/// A small song with one of everything.
pub fn song() -> Project {
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

pub fn ids(project: &Project) -> (Vec<TrackId>, Vec<ClipId>, Vec<DeviceId>) {
    let tracks = project.tracks.iter().map(|track| track.id).collect();
    let clips = project.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.id).collect();
    let devices = project.devices().map(|device| device.id).collect();
    (tracks, clips, devices)
}

/// An edit drawn by lot from the model's edit methods, on whatever the
/// song holds: some are refused, as edits are.
pub fn any_edit(lot: &mut Lot, project: &Project) -> Box<dyn FnOnce(&mut Project) -> Result<(), EditError>> {
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
        0 => Box::new(move |song| {
            song.add_track_of_kind(format!("Track {a}"), kinds[b % 4]);
            Ok(())
        }),
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

