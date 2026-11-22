//! A song as its properties, each with when it was last set: what every
//! program that holds the song holds, and what changes are taken into.

use super::props::{AudioSet, ClipSet, DeviceSet, LaneSet, MarkerSet, MidiSet, NoteSet, PointSet, SongSet, TrackSet};
use super::{
    ALIVE, Beside, Change, ClipOp, DeviceOp, FIRST_AUDIO, FIRST_MIDI, Floors, Home, KIND, Key, LANE, LISTENING,
    MarkerOp, NoteOp, Op, PLACE, Place, PointOp, REFERENCE, SENDS, SongId, Stamp, Thing, TrackOp,
};
use crate::channel::Channel;
use crate::controllers::{ControllerCurve, ControllerKind};
use crate::project::{
    AutomationPoint, AutomationTarget, ChainRef, Clip, ClipContent, Device, FORMAT_VERSION, Marker, Master,
    MidiContent, Note, Project, Reference, Track,
};
use crate::time::TimeSignature;
use crate::travel::Travel;
use crate::{ClipId, DeviceId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// A thing as it is held: what it is, whether it is there, and when it was
/// made. A property of it that has not been set since is as old as that.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Held<T> {
    pub it: T,
    pub alive: bool,
    pub made: Stamp,
}

impl<T> Held<T> {
    fn new(it: T, made: Stamp) -> Self {
        Self { it, alive: true, made }
    }
}

pub(super) type Points = BTreeMap<u64, Held<AutomationPoint>>;

/// A track without what it holds, which is held apart.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TrackHeld {
    pub held: Held<Track>,
    pub place: Place,
    /// What it sends, by the return.
    pub sends: BTreeMap<TrackId, f32>,
    /// The order its sends are held in ([`TrackOp::Sends`]).
    pub sends_order: Vec<TrackId>,
}

/// A clip without its notes and curves, and those beside it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClipHeld {
    pub held: Held<Clip>,
    pub track: TrackId,
    /// The lane it is kept in beside its track, for a take; `None` for a
    /// clip the track plays.
    pub lane: Option<u32>,
    pub notes: BTreeMap<u64, Held<Note>>,
    pub curves: BTreeMap<(ControllerKind, Channel), Points>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DeviceHeld {
    pub held: Held<Device>,
    pub chain: ChainRef,
    pub place: Place,
}

/// An envelope: it is there while it has a point that is.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LaneHeld {
    pub travel: Travel,
    pub enabled: bool,
    pub points: Points,
}

impl Default for LaneHeld {
    fn default() -> Self {
        Self { travel: Travel::Linear, enabled: true, points: Points::new() }
    }
}

/// What taking a part of a change in did: the part, and what the property
/// it set was before. For a part that made things whole, what says that
/// they were not there.
#[derive(Clone, Debug, PartialEq)]
pub struct Won {
    pub op: Op,
    /// When the change it is a part of was made.
    pub stamp: Stamp,
    /// The same property as it was, for a part that sets one.
    pub was: Option<Op>,
    /// When it had last been set.
    pub was_stamp: Stamp,
}

/// What taking a change in did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Taken {
    /// The parts that set something. Among them may be parts of earlier
    /// changes that waited for a thing this one made: each says when its
    /// own change was made.
    pub won: Vec<Won>,
    /// The parts of this change that are held until the thing they are
    /// about is known: news to whoever has not heard them, like the parts
    /// that set something.
    pub waiting: Vec<Op>,
}

impl Taken {
    /// Whether the change was news here: something of it was set or is
    /// held. One that was heard before, or that later changes had already
    /// had their way over, is not.
    pub fn is_news(&self) -> bool {
        !self.won.is_empty() || !self.waiting.is_empty()
    }
}

/// What a shared song remembers beside the song itself: what a project
/// file of one carries, and what a program that joins is handed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    pub song: SongId,
    /// The highest count seen.
    pub clock: u64,
    pub floors: Floors,
    /// What is held and is not in the song as it stands: things that were
    /// removed, things in them, and what was set aside. Each as the change
    /// that puts it on top of the song.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<Op>,
    /// When things were made, where that is not before everything. A note
    /// or a point of a curve that is not named was made with its clip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub made: Vec<(Stamp, Vec<Thing>)>,
    /// When properties were last set, where that is later than their
    /// thing was made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stamps: Vec<(Stamp, Vec<Key>)>,
    /// Changes to things that are not known yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orphans: Vec<(Stamp, Op)>,
}

impl Kept {
    /// What is kept, as it is handed to another program: without what is
    /// one's own listening.
    pub fn impersonal(mut self) -> Kept {
        for (_, keys) in &mut self.stamps {
            keys.retain(|key| !key.is_personal());
        }
        self.stamps.retain(|(_, keys)| !keys.is_empty());
        for op in &mut self.hidden {
            if let Op::Track(_, TrackOp::Whole(track, _)) = op {
                **track = impersonal((**track).clone());
            }
        }
        self.hidden.retain(|op| !op.is_personal());
        self
    }
}

/// A song as every program that holds it holds it: see the
/// [module](super).
#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    /// The song's own settings: a project with nothing in it.
    pub(super) song: Project,
    pub(super) tempo: BTreeMap<(u64, bool), f64>,
    pub(super) meter: BTreeMap<u64, TimeSignature>,
    pub(super) markers: BTreeMap<u64, Held<Marker>>,
    pub(super) tracks: BTreeMap<TrackId, TrackHeld>,
    pub(super) clips: BTreeMap<ClipId, ClipHeld>,
    pub(super) devices: BTreeMap<DeviceId, DeviceHeld>,
    pub(super) lanes: BTreeMap<AutomationTarget, LaneHeld>,
    /// The order the envelopes are held in ([`Op::Envelopes`]).
    pub(super) envelopes: Vec<AutomationTarget>,
    /// When each property was last set, where that is later than its thing
    /// was made.
    pub(super) stamps: HashMap<Key, Stamp>,
    /// Parts of changes to things that are not known yet: taken in when
    /// the thing is.
    pub(super) orphans: Vec<(Stamp, Op)>,
    clock: u64,
    floors: Floors,
}

/// A track with nothing on it.
fn track_shell(track: &Track) -> Track {
    Track { clips: Vec::new(), devices: Vec::new(), sends: Vec::new(), takes: Vec::new(), ..track.clone() }
}

/// A clip without its notes and curves.
fn clip_shell(clip: &Clip) -> Clip {
    let content = match &clip.content {
        ClipContent::Midi(MidiContent { notes: _, loop_length, start_offset, controllers: _ }) => {
            ClipContent::Midi(MidiContent {
                notes: Vec::new(),
                loop_length: *loop_length,
                start_offset: *start_offset,
                controllers: Vec::new(),
            })
        }
        audio @ ClipContent::Audio(_) => audio.clone(),
    };
    Clip { id: clip.id, name: clip.name.clone(), start: clip.start, length: clip.length, muted: clip.muted, content }
}

fn points_of(points: &[AutomationPoint], made: Stamp) -> Points {
    points.iter().map(|point| (point.id, Held::new(*point, made))).collect()
}

impl ClipHeld {
    fn of(clip: &Clip, track: TrackId, lane: Option<u32>, made: Stamp) -> Self {
        let notes = clip.notes().iter().map(|note| (note.id, Held::new(*note, made))).collect();
        let curves = (clip.controllers().iter())
            .map(|curve| ((curve.controller, curve.channel), points_of(&curve.points, made)))
            .collect();
        Self { held: Held::new(clip_shell(clip), made), track, lane, notes, curves }
    }

    /// The clip with everything that is held in it, there or not.
    fn whole(&self) -> Clip {
        let mut clip = self.held.it.clone();
        if let ClipContent::Midi(midi) = &mut clip.content {
            midi.notes = self.notes.values().map(|note| note.it).collect();
            midi.controllers = (self.curves.iter())
                .filter(|(_, points)| !points.is_empty())
                .map(|((controller, channel), points)| ControllerCurve {
                    controller: *controller,
                    channel: *channel,
                    points: points.values().map(|point| point.it).collect(),
                })
                .collect();
        }
        clip
    }
}

impl TrackHeld {
    fn of(track: &Track, place: Place, made: Stamp) -> Self {
        let sends = track.sends.iter().map(|send| (send.to, send.level_db)).collect();
        let sends_order = track.sends.iter().map(|send| send.to).collect();
        Self { held: Held::new(track_shell(track), made), place, sends, sends_order }
    }

    /// The track as a change that makes it says it: with what it sends,
    /// in the order its sends are held in.
    fn whole(&self) -> Track {
        let mut sends: Vec<crate::Send> =
            self.sends.iter().map(|(to, level_db)| crate::Send { to: *to, level_db: *level_db }).collect();
        sends.sort_by(|a, b| self.send_order(a.to, b.to));
        Track { sends, ..self.held.it.clone() }
    }

    /// The order two of its sends are held in: the ones the order names
    /// as it names them, then the others by the return they go to.
    pub(super) fn send_order(&self, a: TrackId, b: TrackId) -> std::cmp::Ordering {
        let place = |to: TrackId| self.sends_order.iter().position(|named| *named == to);
        let (at_a, at_b) = (place(a), place(b));
        at_a.is_none().cmp(&at_b.is_none()).then(at_a.cmp(&at_b)).then(a.cmp(&b))
    }
}

impl Doc {
    /// The song `project` is, as something changes can be taken into. The
    /// project is one that was settled ([`Project::settle`]): every note,
    /// point and marker of it has an id.
    pub fn from_project(project: &Project) -> Doc {
        let made = Stamp::ZERO;
        let Project {
            format_version: _,
            name,
            tempo,
            time_signature,
            key,
            tempo_changes,
            meter_changes,
            loop_region,
            tracks,
            master,
            automation,
            markers,
            next_id: _,
        } = project;
        let Master { volume_db, devices: master_devices } = master;
        let song = Project {
            format_version: FORMAT_VERSION,
            name: name.clone(),
            tempo: *tempo,
            time_signature: *time_signature,
            key: *key,
            tempo_changes: Vec::new(),
            meter_changes: Vec::new(),
            loop_region: *loop_region,
            tracks: Vec::new(),
            master: Master { volume_db: *volume_db, devices: Vec::new() },
            automation: Vec::new(),
            markers: Vec::new(),
            next_id: 0,
        };
        let mut doc = Doc {
            song,
            tempo: tempo_changes.iter().map(|change| ((change.beat.to_bits(), change.ramp), change.tempo)).collect(),
            meter: meter_changes.iter().map(|change| (change.beat.to_bits(), change.signature)).collect(),
            markers: markers.iter().map(|marker| (marker.id, Held::new(marker.clone(), made))).collect(),
            tracks: BTreeMap::new(),
            clips: BTreeMap::new(),
            devices: BTreeMap::new(),
            lanes: BTreeMap::new(),
            envelopes: automation.iter().map(|lane| lane.target.clone()).collect(),
            stamps: HashMap::new(),
            orphans: Vec::new(),
            clock: 0,
            floors: Floors::default(),
        };
        let chain = |chain: ChainRef, devices: &[Device], doc: &mut Doc| {
            for (index, device) in devices.iter().enumerate() {
                let held = DeviceHeld { held: Held::new(device.clone(), made), chain, place: (index + 1) as Place };
                doc.devices.insert(device.id, held);
            }
        };
        for (index, track) in tracks.iter().enumerate() {
            doc.tracks.insert(track.id, TrackHeld::of(track, (index + 1) as Place, made));
            for clip in &track.clips {
                doc.clips.insert(clip.id, ClipHeld::of(clip, track.id, None, made));
            }
            for take in &track.takes {
                doc.clips.insert(take.clip.id, ClipHeld::of(&take.clip, track.id, Some(take.lane), made));
            }
            chain(ChainRef::Track(track.id), &track.devices, &mut doc);
        }
        chain(ChainRef::Master, master_devices, &mut doc);
        for lane in automation {
            let held = LaneHeld { travel: lane.travel, enabled: lane.enabled, points: points_of(&lane.points, made) };
            doc.lanes.insert(lane.target.clone(), held);
        }
        doc
    }

    // ---- when things were set --------------------------------------------

    /// The highest count seen: a change made here takes one above it.
    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// How much of what each program did is held here for certain.
    pub fn floors(&self) -> &Floors {
        &self.floors
    }

    pub fn floors_mut(&mut self) -> &mut Floors {
        &mut self.floors
    }

    /// When `thing` was made; `None` for one that is not known.
    fn made(&self, thing: &Thing) -> Option<Stamp> {
        Some(match thing {
            Thing::Marker(id) => self.markers.get(id)?.made,
            Thing::Track(id) => self.tracks.get(id)?.held.made,
            Thing::Clip(id) => self.clips.get(id)?.held.made,
            Thing::Note(clip, id) => self.clips.get(clip)?.notes.get(id)?.made,
            Thing::Device(id) => self.devices.get(id)?.held.made,
            Thing::Point(home, id) => self.points(home)?.get(id)?.made,
        })
    }

    /// When the thing a property belongs to was made, for a property that
    /// is as old as that until it is set: a thing's own, a track's sends
    /// and a device's parameters.
    fn made_for(&self, key: &Key) -> Stamp {
        let made = match key {
            // Whether a clip is a take, and in which lane, is not said
            // when it is made: a clip is made a clip, and what says of it
            // that it is a take is as old as itself.
            Key::Clip(_, LANE) => None,
            Key::Send(track, _) => self.tracks.get(track).map(|track| track.held.made),
            Key::Param(device, _) => self.devices.get(device).map(|device| device.held.made),
            key => key.thing().and_then(|thing| self.made(&thing)),
        };
        made.unwrap_or(Stamp::ZERO)
    }

    /// When a property was last set.
    pub fn stamp(&self, key: &Key) -> Stamp {
        let own = self.stamps.get(key).copied().unwrap_or(Stamp::ZERO);
        own.max(self.made_for(key))
    }

    /// Whether `thing` is held and has not been removed. (It may still be
    /// out of the song: on a track that was.)
    pub fn is_alive(&self, thing: &Thing) -> bool {
        match thing {
            Thing::Marker(id) => self.markers.get(id).is_some_and(|held| held.alive),
            Thing::Track(id) => self.tracks.get(id).is_some_and(|held| held.held.alive),
            Thing::Clip(id) => self.clips.get(id).is_some_and(|held| held.held.alive),
            Thing::Note(clip, id) => {
                self.clips.get(clip).and_then(|clip| clip.notes.get(id)).is_some_and(|held| held.alive)
            }
            Thing::Device(id) => self.devices.get(id).is_some_and(|held| held.held.alive),
            Thing::Point(home, id) => {
                self.points(home).and_then(|points| points.get(id)).is_some_and(|held| held.alive)
            }
        }
    }

    /// The order envelopes are held in, as something to sort by: the ones
    /// the order names as it names them, then the others by what they
    /// move.
    pub fn envelope_order(&self) -> impl Fn(&AutomationTarget, &AutomationTarget) -> std::cmp::Ordering + '_ {
        let mut named: HashMap<&AutomationTarget, usize> = HashMap::new();
        for (at, target) in self.envelopes.iter().enumerate() {
            named.entry(target).or_insert(at);
        }
        move |a, b| {
            let (at_a, at_b) = (named.get(a).copied(), named.get(b).copied());
            at_a.is_none().cmp(&at_b.is_none()).then(at_a.cmp(&at_b)).then_with(|| a.cmp(b))
        }
    }

    /// A track's sends as they are held in order: `sends`, sorted as the
    /// song worked out from the properties has them.
    pub fn in_send_order(&self, track: TrackId, mut sends: Vec<TrackId>) -> Vec<TrackId> {
        match self.tracks.get(&track) {
            Some(held) => sends.sort_by(|a, b| held.send_order(*a, *b)),
            None => sends.sort(),
        }
        sends
    }

    /// The lane a clip is kept in as a take, for one that is held as one.
    pub fn lane_of(&self, clip: ClipId) -> Option<u32> {
        self.clips.get(&clip)?.lane
    }

    /// A clip as it is held, there or removed, with what is alive in it,
    /// and the track it is on.
    pub fn held_clip(&self, clip: ClipId) -> Option<(TrackId, Clip)> {
        let held = self.clips.get(&clip)?;
        Some((held.track, super::derive::clip_of(held)))
    }

    /// A device as it is held, there or removed.
    pub fn held_device(&self, device: DeviceId) -> Option<Device> {
        Some(self.devices.get(&device)?.held.it.clone())
    }

    /// The song with everything in it that is held, removed or not: no
    /// state of the song that anyone can come back to holds anything this
    /// has not got.
    pub fn with_everything(&self) -> Project {
        let mut all = self.clone();
        all.markers.values_mut().for_each(|held| held.alive = true);
        all.tracks.values_mut().for_each(|track| track.held.alive = true);
        all.clips.values_mut().for_each(|clip| clip.held.alive = true);
        all.devices.values_mut().for_each(|device| device.held.alive = true);
        all.derive().project
    }

    /// The song with `things` in it too, where they are held and were
    /// removed: what it would hold if each were brought back.
    pub fn with_also<'a>(&self, things: impl Iterator<Item = &'a Thing>) -> Project {
        let mut with = self.clone();
        for thing in things {
            match thing {
                Thing::Marker(id) => with.markers.get_mut(id).into_iter().for_each(|held| held.alive = true),
                Thing::Track(id) => with.tracks.get_mut(id).into_iter().for_each(|track| track.held.alive = true),
                Thing::Clip(id) => with.clips.get_mut(id).into_iter().for_each(|clip| clip.held.alive = true),
                Thing::Device(id) => with.devices.get_mut(id).into_iter().for_each(|held| held.held.alive = true),
                // (A note or a point names no file.)
                Thing::Note(..) | Thing::Point(..) => {}
            }
        }
        with.derive().project
    }

    /// Whether `thing` is held at all, there or removed.
    pub fn knows(&self, thing: &Thing) -> bool {
        self.made(thing).is_some()
    }

    /// The thing `thing` is in: a note's clip, a clip's track, a device's
    /// track, a curve's point's clip. `None` for what is in the song
    /// itself.
    pub fn holder(&self, thing: &Thing) -> Option<Thing> {
        match thing {
            Thing::Note(clip, _) | Thing::Point(Home::Curve(clip, ..), _) => Some(Thing::Clip(*clip)),
            Thing::Clip(id) => self.clips.get(id).map(|clip| Thing::Track(clip.track)),
            Thing::Device(id) => match self.devices.get(id)?.chain {
                ChainRef::Track(track) => Some(Thing::Track(track)),
                ChainRef::Master => None,
            },
            Thing::Point(Home::Lane(target), _) => target_holder(target),
            Thing::Marker(_) | Thing::Track(_) => None,
        }
    }

    /// Whether the property `key` is of `thing` or of something in it.
    pub fn is_within(&self, key: &Key, thing: &Thing) -> bool {
        let mut at = match key {
            Key::Send(track, _) => Some(Thing::Track(*track)),
            Key::Param(device, _) => Some(Thing::Device(*device)),
            Key::Lane(target, _) => target_holder(target),
            key => key.thing(),
        };
        // No deeper than a point in a clip on a track.
        for _ in 0..4 {
            match at {
                Some(found) if found == *thing => return true,
                Some(found) => at = self.holder(&found),
                None => return false,
            }
        }
        false
    }

    /// What `thing` is called, for a sentence: a track's, a clip's or a
    /// device's name.
    pub fn name_of(&self, thing: &Thing) -> Option<String> {
        match thing {
            Thing::Track(id) => self.tracks.get(id).map(|held| held.held.it.name.clone()),
            Thing::Clip(id) => self.clips.get(id).map(|held| held.held.it.name.clone()),
            Thing::Device(id) => self.devices.get(id).map(|held| held.held.it.name.clone()),
            Thing::Marker(id) => self.markers.get(id).map(|held| held.it.name.clone()),
            Thing::Note(..) | Thing::Point(..) => None,
        }
    }

    pub(super) fn points(&self, home: &Home) -> Option<&Points> {
        match home {
            Home::Lane(target) => self.lanes.get(target).map(|lane| &lane.points),
            Home::Curve(clip, channel, kind) => self.clips.get(clip)?.curves.get(&(*kind, *channel)),
        }
    }

    /// The points of `home`, made room for where the home is known.
    fn points_mut(&mut self, home: &Home) -> Option<&mut Points> {
        match home {
            Home::Lane(target) => Some(&mut self.lanes.entry(target.clone()).or_default().points),
            Home::Curve(clip, channel, kind) => {
                let clip = self.clips.get_mut(clip)?;
                clip.held.it.midi()?;
                Some(clip.curves.entry((*kind, *channel)).or_default())
            }
        }
    }

    /// A track's place among the tracks, and a device's in its chain.
    pub fn place_of_track(&self, id: TrackId) -> Option<Place> {
        self.tracks.get(&id).map(|held| held.place)
    }

    pub fn place_of_device(&self, id: DeviceId) -> Option<(ChainRef, Place)> {
        self.devices.get(&id).map(|held| (held.chain, held.place))
    }

    // ---- reading a property ----------------------------------------------

    /// The property `key` as it is held, as the change that would set it
    /// so. `None` for a property of something that is not known, and for
    /// what is said only when a thing is made.
    pub fn read(&self, key: &Key) -> Option<Op> {
        Some(match key {
            Key::Song(prop) => Op::Song(SongSet::read(&self.song, *prop)?),
            Key::MasterVolume => Op::MasterVolume(self.song.master.volume_db),
            Key::Tempo(beat, ramp) => Op::Tempo(f64::from_bits(*beat), *ramp, self.tempo.get(&(*beat, *ramp)).copied()),
            Key::Meter(beat) => Op::Meter(f64::from_bits(*beat), self.meter.get(beat).copied()),
            Key::Marker(id, prop) => {
                let held = self.markers.get(id)?;
                Op::Marker(
                    *id,
                    match *prop {
                        ALIVE => MarkerOp::Alive(held.alive),
                        prop => MarkerOp::Set(MarkerSet::read(&held.it, prop)?),
                    },
                )
            }
            Key::Track(id, prop) => {
                let track = self.tracks.get(id)?;
                let it = &track.held.it;
                Op::Track(
                    *id,
                    match *prop {
                        ALIVE => TrackOp::Alive(track.held.alive),
                        PLACE => TrackOp::Place(track.place),
                        REFERENCE => {
                            TrackOp::Reference(it.reference.as_ref().map(|found| Beside { study: found.study.clone() }))
                        }
                        LISTENING => TrackOp::Listening(it.reference.as_ref().is_some_and(|found| found.listening)),
                        SENDS => TrackOp::Sends(track.sends_order.clone()),
                        prop => TrackOp::Set(TrackSet::read(it, prop)?),
                    },
                )
            }
            Key::Send(track, to) => Op::Send(*track, *to, self.tracks.get(track)?.sends.get(to).copied()),
            Key::Clip(id, prop) => {
                let clip = self.clips.get(id)?;
                let it = &clip.held.it;
                Op::Clip(
                    *id,
                    match (*prop, &it.content) {
                        (ALIVE, _) => ClipOp::Alive(clip.held.alive),
                        (PLACE, _) => ClipOp::Track(clip.track),
                        (LANE, _) => ClipOp::Lane(clip.lane),
                        (prop, ClipContent::Audio(audio)) if prop >= FIRST_AUDIO => {
                            ClipOp::Audio(AudioSet::read(audio, prop)?)
                        }
                        (prop, ClipContent::Midi(midi)) if (FIRST_MIDI..FIRST_AUDIO).contains(&prop) => {
                            ClipOp::Midi(MidiSet::read(midi, prop)?)
                        }
                        (prop, _) => ClipOp::Set(ClipSet::read(it, prop)?),
                    },
                )
            }
            Key::Note(clip, id, prop) => {
                let held = self.clips.get(clip)?.notes.get(id)?;
                Op::Note(
                    *clip,
                    *id,
                    match *prop {
                        ALIVE => NoteOp::Alive(held.alive),
                        prop => NoteOp::Set(NoteSet::read(&held.it, prop)?),
                    },
                )
            }
            Key::Device(id, prop) => {
                let device = self.devices.get(id)?;
                Op::Device(
                    *id,
                    match *prop {
                        ALIVE => DeviceOp::Alive(device.held.alive),
                        PLACE => DeviceOp::At(device.chain, device.place),
                        prop => DeviceOp::Set(DeviceSet::read(&device.held.it, prop)?),
                    },
                )
            }
            Key::Param(id, name) => {
                Op::Param(*id, name.clone(), self.devices.get(id)?.held.it.params.get(name).copied())
            }
            Key::Lane(target, prop) => {
                let default = LaneHeld::default();
                let lane = self.lanes.get(target).unwrap_or(&default);
                let shown = crate::AutomationLane {
                    target: target.clone(),
                    points: Vec::new(),
                    travel: lane.travel,
                    enabled: lane.enabled,
                };
                Op::Lane(target.clone(), LaneSet::read(&shown, *prop)?)
            }
            Key::Envelopes => Op::Envelopes(self.envelopes.clone()),
            Key::Point(home, id, prop) => {
                let held = self.points(home)?.get(id)?;
                Op::Point(
                    home.clone(),
                    *id,
                    match *prop {
                        ALIVE => PointOp::Alive(held.alive),
                        prop => PointOp::Set(PointSet::read(&held.it, prop)?),
                    },
                )
            }
        })
    }

    // ---- taking a change in ----------------------------------------------

    /// Takes a change in: each property it sets is set where the change is
    /// later than what set it last. Returns the parts that set something,
    /// with what was there before; a part that an equal or later change
    /// had already had its way over is not among them. The same changes
    /// leave the same properties in whatever order and however often they
    /// are taken in.
    pub fn apply(&mut self, change: &Change) -> Taken {
        let stamp = change.stamp;
        self.clock = self.clock.max(stamp.0);
        let waited: Vec<Op> =
            self.orphans.iter().filter(|(held, _)| *held == stamp).map(|(_, op)| op.clone()).collect();
        let mut won = Vec::new();
        let mut made = false;
        for op in &change.ops {
            made |= op.key().is_none();
            self.take(stamp, op.clone(), &mut won);
        }
        // Something that was waited for may be there now.
        while made && !self.orphans.is_empty() {
            let waiting = std::mem::take(&mut self.orphans);
            let before = waiting.len();
            let mut again = Vec::new();
            for (stamp, op) in waiting {
                self.take(stamp, op, &mut again);
            }
            won.extend(again);
            made = self.orphans.len() < before;
        }
        self.tidy_lanes();
        let waiting = (self.orphans.iter())
            .filter(|(held, op)| *held == stamp && !waited.contains(op))
            .map(|(_, op)| op.clone())
            .collect();
        Taken { won, waiting }
    }

    /// An envelope that has nothing held in it and is as one that was
    /// never there, is not there.
    fn tidy_lanes(&mut self) {
        self.lanes.retain(|_, lane| *lane != LaneHeld::default());
    }

    /// Whether a change at `stamp` that sets `key`, the property of a
    /// thing made at `made`, as `op` says has its way over what set the
    /// property last: it is the later. Two changes have one stamp only if
    /// they are the same change heard twice, which sets nothing again; a
    /// program that says two things under one stamp all the same is not
    /// allowed to leave two programs with two songs, so of two values set
    /// at one stamp one is taken by a rule that does not depend on which
    /// arrived first (the one that is written the greater).
    fn outranks(&self, key: &Key, made: Stamp, stamp: Stamp, op: &Op) -> bool {
        let held = self.stamps.get(key).copied().unwrap_or(Stamp::ZERO).max(made);
        match held.cmp(&stamp) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => {
                let was = self.read(key);
                let written = |op: &Op| serde_json::to_string(op).unwrap_or_default();
                was.as_ref() != Some(op) && was.is_none_or(|was| written(op) > written(&was))
            }
        }
    }

    /// Sets one property if the change is the later, and says so.
    fn set(&mut self, stamp: Stamp, op: Op, key: Key, won: &mut Vec<Won>, put: impl FnOnce(&mut Doc, Op)) {
        if !self.outranks(&key, self.made_for(&key), stamp, &op) {
            return;
        }
        let was_stamp = self.stamp(&key);
        let was = self.read(&key);
        put(self, op.clone());
        self.stamps.insert(key, stamp);
        won.push(Won { op, stamp, was, was_stamp });
    }

    /// [`Doc::outranks`], for what a thing is for good: said when it is
    /// made, and of two said at one stamp the greater (`greater`).
    fn remakes(&self, key: &Key, made: Stamp, stamp: Stamp, greater: bool) -> bool {
        let held = self.stamps.get(key).copied().unwrap_or(Stamp::ZERO).max(made);
        held < stamp || (held == stamp && greater)
    }

    fn take(&mut self, stamp: Stamp, op: Op, won: &mut Vec<Won>) {
        // A property of a thing that is not known waits for the thing.
        let known = match &op {
            Op::Song(_) | Op::MasterVolume(_) | Op::Tempo(..) | Op::Meter(..) | Op::Lane(..) | Op::Envelopes(_) => true,
            Op::Marker(_, MarkerOp::Whole(_))
            | Op::Track(_, TrackOp::Whole(..))
            | Op::Clip(_, ClipOp::Whole(..))
            | Op::Device(_, DeviceOp::Whole(..)) => true,
            Op::Marker(id, _) => self.markers.contains_key(id),
            Op::Track(id, _) | Op::Send(id, ..) => self.tracks.contains_key(id),
            Op::Clip(id, _) | Op::Notes(id, _) => self.clips.contains_key(id),
            Op::Note(clip, id, _) => self.clips.get(clip).is_some_and(|clip| clip.notes.contains_key(id)),
            Op::Device(id, _) | Op::Param(id, ..) => self.devices.contains_key(id),
            Op::Points(home, _) => match home {
                Home::Lane(_) => true,
                Home::Curve(clip, ..) => self.clips.contains_key(clip),
            },
            Op::Point(home, id, _) => self.points(home).is_some_and(|points| points.contains_key(id)),
        };
        if !known {
            if !self.orphans.iter().any(|(held, waiting)| *held == stamp && *waiting == op) {
                self.orphans.push((stamp, op));
            }
            return;
        }
        match op.key() {
            Some(key) => self.set(stamp, op, key, won, |doc, op| doc.put(op)),
            None => self.make(stamp, op, won),
        }
    }

    /// Sets the property a part of a change names, whatever set it last.
    fn put(&mut self, op: Op) {
        match op {
            Op::Song(set) => set.put(&mut self.song),
            Op::MasterVolume(volume) => self.song.master.volume_db = volume,
            Op::Tempo(beat, ramp, tempo) => {
                match tempo {
                    Some(tempo) => self.tempo.insert((beat.to_bits(), ramp), tempo),
                    None => self.tempo.remove(&(beat.to_bits(), ramp)),
                };
            }
            Op::Meter(beat, signature) => {
                match signature {
                    Some(signature) => self.meter.insert(beat.to_bits(), signature),
                    None => self.meter.remove(&beat.to_bits()),
                };
            }
            Op::Marker(id, part) => {
                let Some(held) = self.markers.get_mut(&id) else { return };
                match part {
                    MarkerOp::Alive(alive) => held.alive = alive,
                    MarkerOp::Set(set) => set.put(&mut held.it),
                    MarkerOp::Whole(_) => {}
                }
            }
            Op::Track(id, part) => {
                let Some(track) = self.tracks.get_mut(&id) else { return };
                let it = &mut track.held.it;
                match part {
                    TrackOp::Alive(alive) => track.held.alive = alive,
                    TrackOp::Place(place) => track.place = place,
                    TrackOp::Reference(beside) => {
                        let listening = it.reference.as_ref().is_some_and(|reference| reference.listening);
                        it.reference = beside.map(|beside| Reference { study: beside.study, listening });
                    }
                    TrackOp::Listening(listening) => {
                        if let Some(reference) = &mut it.reference {
                            reference.listening = listening;
                        }
                    }
                    TrackOp::Set(set) => set.put(it),
                    TrackOp::Sends(order) => track.sends_order = order,
                    TrackOp::Whole(..) => {}
                }
            }
            Op::Send(track, to, level) => {
                let Some(track) = self.tracks.get_mut(&track) else { return };
                match level {
                    Some(level) => track.sends.insert(to, level),
                    None => track.sends.remove(&to),
                };
            }
            Op::Clip(id, part) => {
                let Some(clip) = self.clips.get_mut(&id) else { return };
                match (part, &mut clip.held.it.content) {
                    (ClipOp::Alive(alive), _) => clip.held.alive = alive,
                    (ClipOp::Track(track), _) => clip.track = track,
                    (ClipOp::Lane(lane), _) => clip.lane = lane,
                    (ClipOp::Set(set), _) => set.put(&mut clip.held.it),
                    (ClipOp::Midi(set), ClipContent::Midi(midi)) => set.put(midi),
                    (ClipOp::Audio(set), ClipContent::Audio(audio)) => set.put(audio),
                    // What is said of a clip of the other kind is said of nothing.
                    (ClipOp::Midi(_) | ClipOp::Audio(_) | ClipOp::Whole(..), _) => {}
                }
            }
            Op::Note(clip, id, part) => {
                let Some(held) = self.clips.get_mut(&clip).and_then(|clip| clip.notes.get_mut(&id)) else { return };
                match part {
                    NoteOp::Alive(alive) => held.alive = alive,
                    NoteOp::Set(set) => set.put(&mut held.it),
                }
            }
            Op::Device(id, part) => {
                let Some(device) = self.devices.get_mut(&id) else { return };
                match part {
                    DeviceOp::Alive(alive) => device.held.alive = alive,
                    DeviceOp::At(chain, place) => (device.chain, device.place) = (chain, place),
                    DeviceOp::Set(set) => set.put(&mut device.held.it),
                    DeviceOp::Whole(..) => {}
                }
            }
            Op::Param(id, name, value) => {
                let Some(device) = self.devices.get_mut(&id) else { return };
                match value {
                    Some(value) => device.held.it.params.insert(name, value),
                    None => device.held.it.params.remove(&name),
                };
            }
            Op::Lane(target, set) => {
                let lane = self.lanes.entry(target.clone()).or_default();
                let mut shown =
                    crate::AutomationLane { target, points: Vec::new(), travel: lane.travel, enabled: lane.enabled };
                set.put(&mut shown);
                (lane.travel, lane.enabled) = (shown.travel, shown.enabled);
            }
            Op::Envelopes(order) => self.envelopes = order,
            Op::Point(home, id, part) => {
                let Some(held) = self.points_mut(&home).and_then(|points| points.get_mut(&id)) else { return };
                match part {
                    PointOp::Alive(alive) => held.alive = alive,
                    PointOp::Set(set) => set.put(&mut held.it),
                }
            }
            Op::Notes(..) | Op::Points(..) => {}
        }
    }

    /// Takes in a part that makes things whole: every property of each is
    /// set where the change is later than what set it last. For a thing
    /// that is not known, which is nearly every time, that is all of them.
    fn make(&mut self, stamp: Stamp, op: Op, won: &mut Vec<Won>) {
        let mut any = false;
        let mut absent = Vec::new();
        match &op {
            Op::Marker(id, MarkerOp::Whole(marker)) => {
                absent.push(Thing::Marker(*id).set_alive(false));
                any = match self.markers.get(id).map(|held| held.made) {
                    None => {
                        self.markers.insert(*id, Held::new(Marker { id: *id, ..marker.clone() }, stamp));
                        true
                    }
                    Some(made) => {
                        let mut sets = vec![Op::Marker(*id, MarkerOp::Alive(true))];
                        sets.extend(MarkerSet::all(marker).into_iter().map(|set| Op::Marker(*id, MarkerOp::Set(set))));
                        let any = self.remake(stamp, made, sets);
                        if let Some(held) = self.markers.get_mut(id) {
                            held.made = held.made.max(stamp);
                        }
                        any
                    }
                };
            }
            Op::Track(id, TrackOp::Whole(track, place)) => {
                absent.push(Thing::Track(*id).set_alive(false));
                let track = Track { id: *id, ..(**track).clone() };
                any = match self.tracks.get(id).map(|held| held.held.made) {
                    None => {
                        self.tracks.insert(*id, TrackHeld::of(&track, *place, stamp));
                        true
                    }
                    Some(made) => {
                        let beside = track.reference.as_ref().map(|found| Beside { study: found.study.clone() });
                        let mut sets = vec![
                            Op::Track(*id, TrackOp::Alive(true)),
                            Op::Track(*id, TrackOp::Place(*place)),
                            Op::Track(*id, TrackOp::Reference(beside)),
                        ];
                        sets.extend(TrackSet::all(&track).into_iter().map(|set| Op::Track(*id, TrackOp::Set(set))));
                        // What it sends, and what it no longer does.
                        let held: Vec<TrackId> = self.tracks[id].sends.keys().copied().collect();
                        let level =
                            |to: TrackId| track.sends.iter().find(|send| send.to == to).map(|send| send.level_db);
                        let named = track.sends.iter().map(|send| send.to).chain(held);
                        sets.extend(named.map(|to| Op::Send(*id, to, level(to))));
                        sets.push(Op::Track(*id, TrackOp::Sends(track.sends.iter().map(|send| send.to).collect())));
                        let mut any = self.remake(stamp, made, sets);
                        let rank = super::props::kind_rank;
                        let greater = rank(track.kind) > rank(self.tracks[id].held.it.kind);
                        if self.remakes(&Key::Track(*id, KIND), made, stamp, greater)
                            && let Some(held) = self.tracks.get_mut(id)
                            && held.held.it.kind != track.kind
                        {
                            held.held.it.kind = track.kind;
                            any = true;
                        }
                        if let Some(held) = self.tracks.get_mut(id) {
                            held.held.made = held.held.made.max(stamp);
                        }
                        any
                    }
                };
            }
            Op::Clip(id, ClipOp::Whole(clip, track)) => {
                absent.push(Thing::Clip(*id).set_alive(false));
                let clip = Clip { id: *id, ..(**clip).clone() };
                match self.clips.get(id).map(|held| held.held.made) {
                    None => {
                        self.clips.insert(*id, ClipHeld::of(&clip, *track, None, stamp));
                        any = true;
                    }
                    Some(made) => {
                        let mut sets = vec![Op::Clip(*id, ClipOp::Alive(true)), Op::Clip(*id, ClipOp::Track(*track))];
                        sets.extend(ClipSet::all(&clip).into_iter().map(|set| Op::Clip(*id, ClipOp::Set(set))));
                        // What a clip holds, notes or audio, is said when it is made.
                        let audio = |clip: &Clip| clip.audio().is_some();
                        let greater = audio(&clip) && !audio(&self.clips[id].held.it);
                        if self.remakes(&Key::Clip(*id, KIND), made, stamp, greater)
                            && let Some(held) = self.clips.get_mut(id)
                            && audio(&held.held.it) != audio(&clip)
                        {
                            held.held.it.content = clip_shell(&clip).content;
                            any = true;
                        }
                        match &clip.content {
                            ClipContent::Midi(midi) => {
                                sets.extend(MidiSet::all(midi).into_iter().map(|set| Op::Clip(*id, ClipOp::Midi(set))))
                            }
                            ClipContent::Audio(audio) => sets
                                .extend(AudioSet::all(audio).into_iter().map(|set| Op::Clip(*id, ClipOp::Audio(set)))),
                        }
                        any |= self.remake(stamp, made, sets);
                        if let Some(held) = self.clips.get_mut(id) {
                            held.held.made = held.held.made.max(stamp);
                        }
                        // What came in it is made in it.
                        let mut inner = Vec::new();
                        if !clip.notes().is_empty() {
                            self.make(stamp, Op::Notes(*id, clip.notes().to_vec()), &mut inner);
                        }
                        for curve in clip.controllers() {
                            let home = Home::Curve(*id, curve.channel, curve.controller);
                            self.make(stamp, Op::Points(home, curve.points.clone()), &mut inner);
                        }
                        any |= !inner.is_empty();
                    }
                }
            }
            Op::Device(id, DeviceOp::Whole(device, chain, place)) => {
                absent.push(Thing::Device(*id).set_alive(false));
                let device = Device { id: *id, ..(**device).clone() };
                any = match self.devices.get(id).map(|held| held.held.made) {
                    None => {
                        let held = DeviceHeld { held: Held::new(device, stamp), chain: *chain, place: *place };
                        self.devices.insert(*id, held);
                        true
                    }
                    Some(made) => {
                        let mut sets =
                            vec![Op::Device(*id, DeviceOp::Alive(true)), Op::Device(*id, DeviceOp::At(*chain, *place))];
                        sets.extend(DeviceSet::all(&device).into_iter().map(|set| Op::Device(*id, DeviceOp::Set(set))));
                        let held: Vec<String> = self.devices[id].held.it.params.keys().cloned().collect();
                        let named = device.params.keys().cloned().chain(held);
                        sets.extend(named.map(|name| {
                            let value = device.params.get(&name).copied();
                            Op::Param(*id, name, value)
                        }));
                        let mut any = self.remake(stamp, made, sets);
                        let greater = device.uri > self.devices[id].held.it.uri;
                        if self.remakes(&Key::Device(*id, KIND), made, stamp, greater)
                            && let Some(held) = self.devices.get_mut(id)
                            && held.held.it.uri != device.uri
                        {
                            held.held.it.uri.clone_from(&device.uri);
                            any = true;
                        }
                        if let Some(held) = self.devices.get_mut(id) {
                            held.held.made = held.held.made.max(stamp);
                        }
                        any
                    }
                };
            }
            Op::Notes(clip, notes) => {
                for note in notes {
                    absent.push(Thing::Note(*clip, note.id).set_alive(false));
                    let made = self.clips.get(clip).and_then(|held| held.notes.get(&note.id)).map(|held| held.made);
                    match made {
                        None => {
                            if let Some(held) = self.clips.get_mut(clip) {
                                held.notes.insert(note.id, Held::new(*note, stamp));
                                any = true;
                            }
                        }
                        Some(made) => {
                            let mut sets = vec![Op::Note(*clip, note.id, NoteOp::Alive(true))];
                            sets.extend(
                                NoteSet::all(note).into_iter().map(|set| Op::Note(*clip, note.id, NoteOp::Set(set))),
                            );
                            any |= self.remake(stamp, made, sets);
                            if let Some(held) = self.clips.get_mut(clip).and_then(|held| held.notes.get_mut(&note.id)) {
                                held.made = held.made.max(stamp);
                            }
                        }
                    }
                }
            }
            Op::Points(home, points) => {
                for point in points {
                    absent.push(Thing::Point(home.clone(), point.id).set_alive(false));
                    let made = self.points(home).and_then(|held| held.get(&point.id)).map(|held| held.made);
                    match made {
                        None => {
                            if let Some(held) = self.points_mut(home) {
                                held.insert(point.id, Held::new(*point, stamp));
                                any = true;
                            }
                        }
                        Some(made) => {
                            let mut sets = vec![Op::Point(home.clone(), point.id, PointOp::Alive(true))];
                            let all = PointSet::all(point).into_iter();
                            sets.extend(all.map(|set| Op::Point(home.clone(), point.id, PointOp::Set(set))));
                            any |= self.remake(stamp, made, sets);
                            if let Some(held) = self.points_mut(home).and_then(|held| held.get_mut(&point.id)) {
                                held.made = held.made.max(stamp);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if any {
            // What says that the things were not there: all of them for a
            // part that made several.
            let was = match absent.len() {
                1 => absent.pop(),
                _ => None,
            };
            won.push(Won { op, stamp, was, was_stamp: Stamp::ZERO });
        }
    }

    /// Sets every property of a thing that is held already and is made
    /// again at `stamp`: each where that is later than what set it last. A
    /// property that is set so is as old as the thing from then on.
    fn remake(&mut self, stamp: Stamp, made: Stamp, sets: Vec<Op>) -> bool {
        let mut any = false;
        for op in sets {
            let Some(key) = op.key() else { continue };
            if self.outranks(&key, made, stamp, &op) {
                self.put(op);
                // (As old as the thing, which is as old as this from here on.)
                self.stamps.remove(&key);
                any = true;
            }
        }
        any
    }

    // ---- what another program lacks ---------------------------------------

    /// The changes that bring a program that holds what `floors` says up
    /// to what is held here: every property that was set above them, as it
    /// is now, each with the stamp it was set at. What is one's own is not
    /// among them.
    pub fn since(&self, floors: &Floors) -> Vec<Change> {
        let mut changes: BTreeMap<Stamp, Vec<Op>> = BTreeMap::new();
        let lacks = |stamp: Stamp| !floors.has(stamp);
        // Things that were made: first, so that a property of one finds it.
        for (id, held) in self.markers.iter().filter(|(_, held)| lacks(held.made)) {
            changes.entry(held.made).or_default().push(Op::Marker(*id, MarkerOp::Whole(held.it.clone())));
        }
        for (id, track) in self.tracks.iter().filter(|(_, track)| lacks(track.held.made)) {
            let whole = Box::new(impersonal(track.whole()));
            changes.entry(track.held.made).or_default().push(Op::Track(*id, TrackOp::Whole(whole, track.place)));
        }
        for (id, clip) in &self.clips {
            if lacks(clip.held.made) {
                let whole = Box::new(clip.held.it.clone());
                let made = changes.entry(clip.held.made).or_default();
                made.push(Op::Clip(*id, ClipOp::Whole(whole, clip.track)));
                // A take that was made one, and is as old as that.
                if clip.lane.is_some() && !self.stamps.contains_key(&Key::Clip(*id, LANE)) {
                    made.push(Op::Clip(*id, ClipOp::Lane(clip.lane)));
                }
            }
            let mut notes: BTreeMap<Stamp, Vec<Note>> = BTreeMap::new();
            for held in clip.notes.values().filter(|held| lacks(held.made)) {
                notes.entry(held.made).or_default().push(held.it);
            }
            for (made, notes) in notes {
                changes.entry(made).or_default().push(Op::Notes(*id, notes));
            }
            for ((kind, channel), points) in &clip.curves {
                points_since(&Home::Curve(*id, *channel, *kind), points, &lacks, &mut changes);
            }
        }
        for (id, device) in self.devices.iter().filter(|(_, device)| lacks(device.held.made)) {
            let whole = DeviceOp::Whole(Box::new(device.held.it.clone()), device.chain, device.place);
            changes.entry(device.held.made).or_default().push(Op::Device(*id, whole));
        }
        for (target, lane) in &self.lanes {
            points_since(&Home::Lane(target.clone()), &lane.points, &lacks, &mut changes);
        }
        // Then the properties that were set since.
        let mut set: Vec<(&Key, &Stamp)> =
            self.stamps.iter().filter(|(key, stamp)| lacks(**stamp) && !key.is_personal()).collect();
        set.sort();
        for (key, stamp) in set {
            if let Some(op) = self.read(key) {
                changes.entry(*stamp).or_default().push(op);
            }
        }
        for (stamp, op) in self.orphans.iter().filter(|(stamp, _)| lacks(*stamp)) {
            changes.entry(*stamp).or_default().push(op.clone());
        }
        changes.into_iter().map(|(stamp, ops)| Change { stamp, ops }).collect()
    }

    // ---- what a file keeps -------------------------------------------------

    /// What is held beside `song`, the song as it stands
    /// ([`Doc::derive`]): what a project file of a shared song carries,
    /// and what a program that joins is handed with the song.
    pub fn kept(&self, id: SongId, song: &Project) -> Kept {
        let base = Doc::from_project(song);
        let mut hidden = Vec::new();
        let removed = |alive: bool, thing: Thing, hidden: &mut Vec<Op>| {
            if !alive {
                hidden.push(thing.set_alive(false));
            }
        };
        SongSet::differing(&base.song, &self.song, |set| hidden.push(Op::Song(set)));
        for ((beat, ramp), tempo) in self.tempo.iter().filter(|(slot, _)| !base.tempo.contains_key(slot)) {
            hidden.push(Op::Tempo(f64::from_bits(*beat), *ramp, Some(*tempo)));
        }
        for (beat, signature) in self.meter.iter().filter(|(slot, _)| !base.meter.contains_key(slot)) {
            hidden.push(Op::Meter(f64::from_bits(*beat), Some(*signature)));
        }
        for (id, held) in &self.markers {
            if base.markers.get(id).map(|base| (&base.it, base.it.id)) != Some((&held.it, held.it.id)) {
                hidden.push(Op::Marker(*id, MarkerOp::Whole(held.it.clone())));
            }
            removed(held.alive, Thing::Marker(*id), &mut hidden);
        }
        for (id, track) in &self.tracks {
            let same = base.tracks.get(id).is_some_and(|base| {
                base.held.it == track.held.it && base.place == track.place && base.sends == track.sends
            });
            if !same {
                hidden.push(Op::Track(*id, TrackOp::Whole(Box::new(track.whole()), track.place)));
            }
            // The order its sends are held in, where it names more or
            // less than the song as written shows, or than the track as
            // it is made again from here would.
            if !same || base.tracks.get(id).map(|base| &base.sends_order) != Some(&track.sends_order) {
                hidden.push(Op::Track(*id, TrackOp::Sends(track.sends_order.clone())));
            }
            removed(track.held.alive, Thing::Track(*id), &mut hidden);
        }
        for (id, clip) in &self.clips {
            match base.clips.get(id) {
                None => {
                    hidden.push(Op::Clip(*id, ClipOp::Whole(Box::new(clip.whole()), clip.track)));
                    if clip.lane.is_some() {
                        hidden.push(Op::Clip(*id, ClipOp::Lane(clip.lane)));
                    }
                }
                Some(base) => {
                    if base.held.it != clip.held.it || base.track != clip.track {
                        hidden.push(Op::Clip(*id, ClipOp::Whole(Box::new(clip.held.it.clone()), clip.track)));
                    }
                    // (The lane a take asked for, where it lies in another
                    // because two asked for one.)
                    if base.lane != clip.lane {
                        hidden.push(Op::Clip(*id, ClipOp::Lane(clip.lane)));
                    }
                    let as_held =
                        |note: &&Held<Note>| base.notes.get(&note.it.id).is_none_or(|base| base.it != note.it);
                    let notes: Vec<Note> = clip.notes.values().filter(as_held).map(|note| note.it).collect();
                    if !notes.is_empty() {
                        hidden.push(Op::Notes(*id, notes));
                    }
                    for ((kind, channel), points) in &clip.curves {
                        let home = Home::Curve(*id, *channel, *kind);
                        points_beside(&home, points, base.curves.get(&(*kind, *channel)), &mut hidden);
                    }
                }
            }
            removed(clip.held.alive, Thing::Clip(*id), &mut hidden);
            for note in clip.notes.values() {
                removed(note.alive, Thing::Note(*id, note.it.id), &mut hidden);
            }
            for ((kind, channel), points) in &clip.curves {
                for point in points.values() {
                    removed(point.alive, Thing::Point(Home::Curve(*id, *channel, *kind), point.it.id), &mut hidden);
                }
            }
        }
        for (id, device) in &self.devices {
            let same = base.devices.get(id).is_some_and(|base| {
                base.held.it == device.held.it && base.chain == device.chain && base.place == device.place
            });
            if !same {
                let whole = DeviceOp::Whole(Box::new(device.held.it.clone()), device.chain, device.place);
                hidden.push(Op::Device(*id, whole));
            }
            removed(device.held.alive, Thing::Device(*id), &mut hidden);
        }
        if base.envelopes != self.envelopes {
            hidden.push(Op::Envelopes(self.envelopes.clone()));
        }
        for (target, lane) in &self.lanes {
            let default = LaneHeld::default();
            let base = base.lanes.get(target).unwrap_or(&default);
            if base.travel != lane.travel {
                hidden.push(Op::Lane(target.clone(), LaneSet::Travel(lane.travel)));
            }
            if base.enabled != lane.enabled {
                hidden.push(Op::Lane(target.clone(), LaneSet::Enabled(lane.enabled)));
            }
            let home = Home::Lane(target.clone());
            points_beside(&home, &lane.points, Some(&base.points), &mut hidden);
            for point in lane.points.values() {
                removed(point.alive, Thing::Point(home.clone(), point.it.id), &mut hidden);
            }
        }

        // When things were made, where that is not before everything.
        let mut made: BTreeMap<Stamp, Vec<Thing>> = BTreeMap::new();
        let mut say = |stamp: Stamp, usual: Stamp, thing: Thing| {
            if stamp != usual {
                made.entry(stamp).or_default().push(thing);
            }
        };
        for (id, held) in &self.markers {
            say(held.made, Stamp::ZERO, Thing::Marker(*id));
        }
        for (id, track) in &self.tracks {
            say(track.held.made, Stamp::ZERO, Thing::Track(*id));
        }
        for (id, clip) in &self.clips {
            say(clip.held.made, Stamp::ZERO, Thing::Clip(*id));
            for note in clip.notes.values() {
                say(note.made, clip.held.made, Thing::Note(*id, note.it.id));
            }
            for ((kind, channel), points) in &clip.curves {
                for point in points.values() {
                    say(point.made, clip.held.made, Thing::Point(Home::Curve(*id, *channel, *kind), point.it.id));
                }
            }
        }
        for (id, device) in &self.devices {
            say(device.held.made, Stamp::ZERO, Thing::Device(*id));
        }
        for (target, lane) in &self.lanes {
            for point in lane.points.values() {
                say(point.made, Stamp::ZERO, Thing::Point(Home::Lane(target.clone()), point.it.id));
            }
        }
        let mut stamps: BTreeMap<Stamp, Vec<Key>> = BTreeMap::new();
        for (key, stamp) in &self.stamps {
            stamps.entry(*stamp).or_default().push(key.clone());
        }
        for keys in stamps.values_mut() {
            keys.sort();
        }
        Kept {
            song: id,
            clock: self.clock,
            floors: self.floors.clone(),
            hidden,
            made: made.into_iter().collect(),
            stamps: stamps.into_iter().collect(),
            orphans: self.orphans.clone(),
        }
    }

    /// A shared song as it was kept: `song` with what was held beside it.
    pub fn from_kept(song: &Project, kept: &Kept) -> Doc {
        let mut doc = Doc::from_project(song);
        // Laid over the song one after the other, whatever anything says
        // of when: the stamps are put in afterwards.
        for (index, op) in kept.hidden.iter().enumerate() {
            doc.apply(&Change { stamp: Stamp(index as u64 + 1, u64::MAX), ops: vec![op.clone()] });
        }
        doc.stamps.clear();
        for held in doc.markers.values_mut() {
            held.made = Stamp::ZERO;
        }
        for track in doc.tracks.values_mut() {
            track.held.made = Stamp::ZERO;
        }
        for device in doc.devices.values_mut() {
            device.held.made = Stamp::ZERO;
        }
        for point in doc.lanes.values_mut().flat_map(|lane| lane.points.values_mut()) {
            point.made = Stamp::ZERO;
        }
        // Clips before what is in them, which is as old as they are
        // unless it says otherwise.
        for clip in doc.clips.values_mut() {
            clip.held.made = Stamp::ZERO;
        }
        for (stamp, things) in &kept.made {
            for thing in things {
                if let Thing::Clip(id) = thing
                    && let Some(clip) = doc.clips.get_mut(id)
                {
                    clip.held.made = *stamp;
                }
            }
        }
        for clip in doc.clips.values_mut() {
            let made = clip.held.made;
            let points = clip.curves.values_mut().flat_map(|points| points.values_mut());
            for held in points {
                held.made = made;
            }
            for held in clip.notes.values_mut() {
                held.made = made;
            }
        }
        for (stamp, things) in &kept.made {
            for thing in things {
                match thing {
                    Thing::Marker(id) => {
                        if let Some(held) = doc.markers.get_mut(id) {
                            held.made = *stamp;
                        }
                    }
                    Thing::Track(id) => {
                        if let Some(track) = doc.tracks.get_mut(id) {
                            track.held.made = *stamp;
                        }
                    }
                    Thing::Clip(_) => {}
                    Thing::Note(clip, id) => {
                        if let Some(held) = doc.clips.get_mut(clip).and_then(|clip| clip.notes.get_mut(id)) {
                            held.made = *stamp;
                        }
                    }
                    Thing::Device(id) => {
                        if let Some(device) = doc.devices.get_mut(id) {
                            device.held.made = *stamp;
                        }
                    }
                    Thing::Point(home, id) => {
                        if let Some(held) = doc.points_mut(home).and_then(|points| points.get_mut(id)) {
                            held.made = *stamp;
                        }
                    }
                }
            }
        }
        for (stamp, keys) in &kept.stamps {
            for key in keys {
                doc.stamps.insert(key.clone(), *stamp);
            }
        }
        doc.orphans.clone_from(&kept.orphans);
        doc.clock = kept.clock;
        doc.floors = kept.floors.clone();
        doc.tidy_lanes();
        doc
    }
}

/// The thing an envelope goes with: the track it moves, or the device.
fn target_holder(target: &AutomationTarget) -> Option<Thing> {
    match target {
        AutomationTarget::TrackVolume { track }
        | AutomationTarget::TrackPan { track }
        | AutomationTarget::Send { track, .. } => Some(Thing::Track(*track)),
        AutomationTarget::DeviceParam { device, .. } => Some(Thing::Device(*device)),
        AutomationTarget::MasterVolume => None,
    }
}

/// A track as it is said to anyone else: without what is one's own.
pub(super) fn impersonal(mut track: Track) -> Track {
    track.solo = false;
    track.input = None;
    if let Some(reference) = &mut track.reference {
        reference.listening = false;
    }
    track
}

fn points_since(home: &Home, points: &Points, lacks: &impl Fn(Stamp) -> bool, changes: &mut BTreeMap<Stamp, Vec<Op>>) {
    let mut made: BTreeMap<Stamp, Vec<AutomationPoint>> = BTreeMap::new();
    for held in points.values().filter(|held| lacks(held.made)) {
        made.entry(held.made).or_default().push(held.it);
    }
    for (made, points) in made {
        changes.entry(made).or_default().push(Op::Points(home.clone(), points));
    }
}

/// The points of `points` that `base` has not got as they are.
fn points_beside(home: &Home, points: &Points, base: Option<&Points>, hidden: &mut Vec<Op>) {
    let beside: Vec<AutomationPoint> = (points.values())
        .filter(|point| base.and_then(|base| base.get(&point.it.id)).is_none_or(|base| base.it != point.it))
        .map(|point| point.it)
        .collect();
    if !beside.is_empty() {
        hidden.push(Op::Points(home.clone(), beside));
    }
}
