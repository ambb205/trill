//! A song that several programs hold and change at once.
//!
//! Nobody is the host. Every program that has the song has all of it, can
//! go on alone, and takes what the others did when it meets them again.
//! That works because of how the song is held ([`Doc`]): not as the
//! [`Project`] an edit reads and writes, but as the *properties* the
//! project is made of, each with the moment it was last set.
//!
//! # Properties, and the later one
//!
//! Everything a song holds is a thing with an id that needs nobody's leave
//! (a track, a clip, a device, a note, a point of an envelope, a marker),
//! or has a name that is its own (a device's parameter by its name, a send
//! by its two tracks, what the tempo does at a beat, an envelope by what it
//! moves). Whether a thing is there, where it is, and each of its values
//! are properties of their own ([`Key`]). A change ([`Change`]) sets some
//! properties, and carries a [`Stamp`]: a count that is higher than every
//! count its maker had seen, with the maker's number to tell two of the
//! same count apart.
//!
//! A property is what the change with the highest stamp set it to. That is
//! the whole rule for taking changes in ([`Doc::apply`]), and it is why
//! the same changes are the same song in whatever order, however late and
//! however often they arrive: the highest of a set of stamps does not
//! depend on the order they were looked at in. Two people on two
//! properties of one thing both have their way; two on one property, the
//! later.
//!
//! # The song is worked out from the properties
//!
//! Properties alone can say things no song may be: a clip on a track that
//! is gone, two tracks that each listen to the other. The project is
//! therefore *derived* ([`Doc::derive`]): a function of the properties and
//! of nothing else, which is the same in every program that holds them.
//! What it leaves out is still held:
//!
//! * A thing that was removed is kept, with whatever is done in it
//!   meanwhile, and comes back whole when someone brings it back. What was
//!   in it or pointed at it (its clips, the sends to it, its envelopes)
//!   goes and comes with it, without anyone having said so.
//! * Of two edits that are each allowed and cannot both hold (two tracks
//!   listening to each other, a track made a reference while it is given a
//!   send), the one that was made first stands, as it would have if the
//!   two had been made one after the other, and the later is set aside
//!   ([`Aside`]) until it can hold.
//!
//! # Edits stay edits
//!
//! An edit is still a method on [`Project`]. What travels is found by
//! comparing the song before and after ([`diff`]), property by property,
//! and nothing is written twice as a message. A session applies what it
//! found to its own [`Doc`] like anyone else's change, which is also what
//! its undo history is made of: an undo sets back the properties its step
//! set, where they are still as the step left them.
//!
//! # What is one's own
//!
//! Some of what a project holds is one person's listening and not the
//! song: the loop, which tracks are soloed, which reference is listened
//! to. They are properties like the others, so that they are undone like
//! the others, and they never leave the program ([`Op::is_personal`]).

mod check;
mod derive;
mod diff;
mod doc;
#[doc(hidden)]
pub mod lot;
mod props;
#[cfg(test)]
mod tests;

pub use check::{MAX_OPS, MAX_STATE_BYTES, MAX_TEXT, Refused};
pub use derive::{Aside, Derived, Why};
pub use diff::diff;
pub use doc::{Doc, Kept, Taken, Won};
pub use props::{
    AudioSet, Blob, ClipSet, DeviceSet, FIRST_AUDIO, FIRST_MIDI, FIRST_SET, LaneSet, MarkerSet, MidiSet, NoteSet,
    PointSet, SongSet, TrackSet, is_prop, path_prop,
};

use crate::channel::Channel;
use crate::controllers::ControllerKind;
use crate::project::{AutomationPoint, AutomationTarget, ChainRef, Clip, Device, Marker, Note, Project, Track};
use crate::time::{Beats, TimeSignature};
use crate::{ClipId, DeviceId, TrackId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The number of one opening of a song in one program: drawn by lot when
/// that program first changes the song, so that a copied file, a safety
/// copy or a second window is never the same one twice. 0 is whoever
/// changed the song before it was shared.
pub type Replica = u64;

/// How many bits of an id count things; the bits above them are the
/// number of the program that made the thing.
pub const ID_BITS: u32 = 20;
/// One more than the highest number a program draws for itself: with the
/// bits that count, an id stays below 2^53, which is as far as a number
/// in JSON is exact for whoever reads it.
pub const REPLICA_LIMIT: u64 = 1 << 33;
/// The lowest number a program draws for itself, above every id a song
/// that was never shared is likely to have counted to.
pub const REPLICA_FLOOR: u64 = 1 << 16;
/// The highest id there is.
pub const MAX_ID: u64 = (1 << 53) - 1;

/// When a property was set: a count that is above every count its maker
/// had seen, and the maker's number. The later of two is the higher count,
/// and of two of one count the higher number.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Stamp(pub u64, pub Replica);

impl Stamp {
    /// Before anything was set: what a song that is opened starts from.
    pub const ZERO: Stamp = Stamp(0, 0);

    pub fn clock(self) -> u64 {
        self.0
    }

    pub fn replica(self) -> Replica {
        self.1
    }
}

/// Where a thing lies among the others of its row (the tracks, the devices
/// of a chain): they are in the order of these, and a thing that is moved
/// takes a number between its new neighbours'.
pub type Place = f64;

/// Where a point belongs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Home {
    /// In the envelope that moves this.
    Lane(AutomationTarget),
    /// In a clip's curve for a controller on a channel.
    Curve(ClipId, Channel, ControllerKind),
}

/// What makes a track a reference, as a change says it (which reference
/// is listened to is one's own, and said apart).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beside {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub study: Option<String>,
}

/// What a change does to a marker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerOp {
    /// The marker is made, as this.
    Whole(Marker),
    /// It is there, or it is removed.
    Alive(bool),
    Set(MarkerSet),
}

/// What a change does to a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackOp {
    /// The track is made, as this (without what it holds, which is made
    /// apart), at this place among the tracks.
    Whole(Box<Track>, Place),
    Alive(bool),
    Place(Place),
    /// It is a reference, or with `None` an ordinary track.
    Reference(Option<Beside>),
    /// Whether it is the reference being listened to. One's own.
    Listening(bool),
    Set(TrackSet),
    /// The order its sends are held in, by the returns they go to: one
    /// property, the whole list. A send it does not name comes after the
    /// ones it names.
    Sends(Vec<TrackId>),
}

/// What a change does to a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipOp {
    /// The clip is made, as this and with the notes and curves in it, on
    /// this track.
    Whole(Box<Clip>, TrackId),
    Alive(bool),
    /// The track it is on.
    Track(TrackId),
    Set(ClipSet),
    Midi(MidiSet),
    Audio(AudioSet),
    /// Where on its track it is: in a lane beside the track's clips, as a
    /// take that is kept and not played from there, or with `None` among
    /// the clips the track plays. A clip that is made is among the clips
    /// until this says otherwise.
    Lane(Option<u32>),
}

/// What a change does to a note.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteOp {
    Alive(bool),
    Set(NoteSet),
}

/// What a change does to a device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceOp {
    /// The device is made, as this, in this chain at this place.
    Whole(Box<Device>, ChainRef, Place),
    Alive(bool),
    /// The chain it is in and its place there: one property, so that a
    /// place is never taken for another chain's.
    At(ChainRef, Place),
    Set(DeviceSet),
}

/// What a change does to a point.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointOp {
    Alive(bool),
    Set(PointSet),
}

/// One part of a change: a property set, or a thing made with all of its
/// properties at once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Song(SongSet),
    MasterVolume(f32),
    /// What the tempo does at a beat: the tempo that takes over there, or
    /// with `true` the glide that arrives there; `None` for nothing.
    Tempo(Beats, bool, Option<f64>),
    /// The time signature that takes over at a beat, or none.
    Meter(Beats, Option<TimeSignature>),
    Marker(u64, MarkerOp),
    Track(TrackId, TrackOp),
    /// How much of the first track goes to the second, or nothing.
    Send(TrackId, TrackId, Option<f32>),
    Clip(ClipId, ClipOp),
    /// Notes made in a clip.
    Notes(ClipId, Vec<Note>),
    Note(ClipId, u64, NoteOp),
    Device(DeviceId, DeviceOp),
    /// A parameter's value by its name, or with `None` whatever the device
    /// has it at by itself.
    Param(DeviceId, String, Option<f64>),
    Lane(AutomationTarget, LaneSet),
    /// Points made in an envelope or a curve.
    Points(Home, Vec<AutomationPoint>),
    Point(Home, u64, PointOp),
    /// The order the song's envelopes are held in: one property, the
    /// whole list. An envelope it does not name comes after the ones it
    /// names.
    Envelopes(Vec<AutomationTarget>),
}

/// The property that says whether a thing is there.
pub const ALIVE: u8 = 0;
/// The property that says where a thing is: a track's place, the track a
/// clip is on, a device's chain and its place in it.
pub const PLACE: u8 = 1;
/// The property that says whether a track is a reference.
pub const REFERENCE: u8 = 2;
/// The property that says whether a reference is listened to.
pub const LISTENING: u8 = 3;
/// What a thing is for good: a track's kind, a device's type, whether a
/// clip holds notes or audio. Said when the thing is made.
pub const KIND: u8 = 4;
/// The property that says whether a clip is a take kept beside its track,
/// and in which lane.
pub const LANE: u8 = 5;
/// The property that says in which order a track's sends are held.
pub const SENDS: u8 = 6;

/// One property: what a change sets, and what the later of two changes
/// wins.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Song(u8),
    MasterVolume,
    /// By the bits of the beat, and whether it is the glide that arrives.
    Tempo(u64, bool),
    Meter(u64),
    Marker(u64, u8),
    Track(TrackId, u8),
    Send(TrackId, TrackId),
    Clip(ClipId, u8),
    Note(ClipId, u64, u8),
    Device(DeviceId, u8),
    Param(DeviceId, String),
    Lane(AutomationTarget, u8),
    Point(Home, u64, u8),
    /// The order of the song's envelopes.
    Envelopes,
}

/// Something a song holds that has an id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Thing {
    Marker(u64),
    Track(TrackId),
    Clip(ClipId),
    Note(ClipId, u64),
    Device(DeviceId),
    Point(Home, u64),
}

impl Key {
    /// The thing this is a property of, for one that is a thing's.
    pub fn thing(&self) -> Option<Thing> {
        Some(match self {
            Key::Marker(id, _) => Thing::Marker(*id),
            Key::Track(id, _) => Thing::Track(*id),
            Key::Clip(id, _) => Thing::Clip(*id),
            Key::Note(clip, id, _) => Thing::Note(*clip, *id),
            Key::Device(id, _) => Thing::Device(*id),
            Key::Point(home, id, _) => Thing::Point(home.clone(), *id),
            Key::Song(_)
            | Key::MasterVolume
            | Key::Tempo(..)
            | Key::Meter(_)
            | Key::Send(..)
            | Key::Param(..)
            | Key::Lane(..)
            | Key::Envelopes => return None,
        })
    }

    /// Whether this is one's own, and never anyone else's.
    pub fn is_personal(&self) -> bool {
        match self {
            Key::Song(prop) => {
                *prop == SongSet::Loop(crate::LoopRegion { enabled: false, start: 0.0, end: 1.0 }).prop()
            }
            Key::Track(_, prop) => {
                *prop == LISTENING || *prop == TrackSet::Solo(false).prop() || *prop == TrackSet::Input(None).prop()
            }
            // Where a clip's file lies is each machine's own: the song
            // says what the file is.
            Key::Clip(_, prop) => *prop == props::path_prop(),
            _ => false,
        }
    }
}

impl Change {
    /// The change as it is said to another program: without what is one's
    /// own listening and this machine's, and without where any file lies.
    /// `None` where nothing of it is anyone else's.
    pub fn impersonal(mut self) -> Option<Change> {
        self.ops.retain(|op| !op.is_personal());
        for op in &mut self.ops {
            op.forget_what_is_ones_own();
        }
        (!self.ops.is_empty()).then_some(self)
    }
}

impl Op {
    /// A part that makes a thing whole, without what of the thing is
    /// one's own: a track's listening and its input, and where a clip's
    /// file lies. Whatever is said to another program goes through this,
    /// and whatever arrives from one too ([`Op::checked`]): a path is
    /// never said, and one that is said is never taken.
    pub(crate) fn forget_what_is_ones_own(&mut self) {
        match self {
            Op::Track(_, TrackOp::Whole(track, _)) => **track = doc::impersonal((**track).clone()),
            Op::Clip(_, ClipOp::Whole(clip, _)) => doc::forget_place(clip),
            _ => {}
        }
    }
}

/// The song as it is said to another program, and as two programs that
/// hold the same song both have it: without what is each one's own (the
/// loop, what is soloed, which input a track records, which reference is
/// listened to) and without where any file lies.
pub fn said_song(song: &Project) -> Project {
    let mut song = song.clone();
    song.loop_region.enabled = false;
    song.tracks = song.tracks.into_iter().map(doc::impersonal).collect();
    song
}

impl Thing {
    /// The property that says whether the thing is there.
    pub fn alive(&self) -> Key {
        match self {
            Thing::Marker(id) => Key::Marker(*id, ALIVE),
            Thing::Track(id) => Key::Track(*id, ALIVE),
            Thing::Clip(id) => Key::Clip(*id, ALIVE),
            Thing::Note(clip, id) => Key::Note(*clip, *id, ALIVE),
            Thing::Device(id) => Key::Device(*id, ALIVE),
            Thing::Point(home, id) => Key::Point(home.clone(), *id, ALIVE),
        }
    }

    /// The change that removes the thing, or brings it back.
    pub fn set_alive(&self, alive: bool) -> Op {
        match self {
            Thing::Marker(id) => Op::Marker(*id, MarkerOp::Alive(alive)),
            Thing::Track(id) => Op::Track(*id, TrackOp::Alive(alive)),
            Thing::Clip(id) => Op::Clip(*id, ClipOp::Alive(alive)),
            Thing::Note(clip, id) => Op::Note(*clip, *id, NoteOp::Alive(alive)),
            Thing::Device(id) => Op::Device(*id, DeviceOp::Alive(alive)),
            Thing::Point(home, id) => Op::Point(home.clone(), *id, PointOp::Alive(alive)),
        }
    }
}

impl Op {
    /// The one property this sets; `None` for a part that makes things
    /// whole, which sets every property they have.
    pub fn key(&self) -> Option<Key> {
        Some(match self {
            Op::Song(set) => Key::Song(set.prop()),
            Op::MasterVolume(_) => Key::MasterVolume,
            Op::Tempo(beat, ramp, _) => Key::Tempo(beat.to_bits(), *ramp),
            Op::Meter(beat, _) => Key::Meter(beat.to_bits()),
            Op::Marker(id, MarkerOp::Alive(_)) => Key::Marker(*id, ALIVE),
            Op::Marker(id, MarkerOp::Set(set)) => Key::Marker(*id, set.prop()),
            Op::Track(id, TrackOp::Alive(_)) => Key::Track(*id, ALIVE),
            Op::Track(id, TrackOp::Place(_)) => Key::Track(*id, PLACE),
            Op::Track(id, TrackOp::Reference(_)) => Key::Track(*id, REFERENCE),
            Op::Track(id, TrackOp::Listening(_)) => Key::Track(*id, LISTENING),
            Op::Track(id, TrackOp::Sends(_)) => Key::Track(*id, SENDS),
            Op::Track(id, TrackOp::Set(set)) => Key::Track(*id, set.prop()),
            Op::Send(track, to, _) => Key::Send(*track, *to),
            Op::Clip(id, ClipOp::Alive(_)) => Key::Clip(*id, ALIVE),
            Op::Clip(id, ClipOp::Track(_)) => Key::Clip(*id, PLACE),
            Op::Clip(id, ClipOp::Set(set)) => Key::Clip(*id, set.prop()),
            Op::Clip(id, ClipOp::Midi(set)) => Key::Clip(*id, set.prop()),
            Op::Clip(id, ClipOp::Audio(set)) => Key::Clip(*id, set.prop()),
            Op::Clip(id, ClipOp::Lane(_)) => Key::Clip(*id, LANE),
            Op::Envelopes(_) => Key::Envelopes,
            Op::Note(clip, id, NoteOp::Alive(_)) => Key::Note(*clip, *id, ALIVE),
            Op::Note(clip, id, NoteOp::Set(set)) => Key::Note(*clip, *id, set.prop()),
            Op::Device(id, DeviceOp::Alive(_)) => Key::Device(*id, ALIVE),
            Op::Device(id, DeviceOp::At(..)) => Key::Device(*id, PLACE),
            Op::Device(id, DeviceOp::Set(set)) => Key::Device(*id, set.prop()),
            Op::Param(device, name, _) => Key::Param(*device, name.clone()),
            Op::Lane(target, set) => Key::Lane(target.clone(), set.prop()),
            Op::Point(home, id, PointOp::Alive(_)) => Key::Point(home.clone(), *id, ALIVE),
            Op::Point(home, id, PointOp::Set(set)) => Key::Point(home.clone(), *id, set.prop()),
            Op::Marker(_, MarkerOp::Whole(_))
            | Op::Track(_, TrackOp::Whole(..))
            | Op::Clip(_, ClipOp::Whole(..))
            | Op::Device(_, DeviceOp::Whole(..))
            | Op::Notes(..)
            | Op::Points(..) => return None,
        })
    }

    /// The things this makes whole, for a part that does.
    pub fn makes(&self) -> Vec<Thing> {
        match self {
            Op::Marker(id, MarkerOp::Whole(_)) => vec![Thing::Marker(*id)],
            Op::Track(id, TrackOp::Whole(..)) => vec![Thing::Track(*id)],
            Op::Clip(id, ClipOp::Whole(..)) => vec![Thing::Clip(*id)],
            Op::Device(id, DeviceOp::Whole(..)) => vec![Thing::Device(*id)],
            Op::Notes(clip, notes) => notes.iter().map(|note| Thing::Note(*clip, note.id)).collect(),
            Op::Points(home, points) => points.iter().map(|point| Thing::Point(home.clone(), point.id)).collect(),
            _ => Vec::new(),
        }
    }

    /// Whether this is one's own listening and not the song: never sent,
    /// and never taken from anyone.
    pub fn is_personal(&self) -> bool {
        self.key().is_some_and(|key| key.is_personal())
    }

    /// Whether this sets nothing but a parameter's value, which a running
    /// device takes as it is.
    pub fn is_param(&self) -> bool {
        matches!(self, Op::Param(..))
    }
}

/// What one edit did to the song: the properties it set, and when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub stamp: Stamp,
    pub ops: Vec<Op>,
}

/// A change as one program says it to another: with how much of everyone's
/// its maker held when it was made, which is what tells a change that was
/// made knowing of another from one made at the same moment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Said {
    pub change: Change,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knew: Option<Floors>,
}

/// How much of what each program did another has: for each program by its
/// number, the count up to which every change of its that still matters
/// is held. Two programs that meet tell each other theirs, and each sends
/// what lies above the other's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Floors(pub BTreeMap<Replica, u64>);

impl Floors {
    /// Whether a change with this stamp is held for certain.
    pub fn has(&self, stamp: Stamp) -> bool {
        stamp == Stamp::ZERO || self.0.get(&stamp.1).is_some_and(|floor| *floor >= stamp.0)
    }

    /// Everything of `replica` up to `clock` is held.
    pub fn raise(&mut self, replica: Replica, clock: u64) {
        let floor = self.0.entry(replica).or_insert(0);
        *floor = (*floor).max(clock);
    }

    /// Everything `other` says is held, is held here too.
    pub fn take(&mut self, other: &Floors) {
        for (replica, clock) in &other.0 {
            self.raise(*replica, *clock);
        }
    }

    /// The count up to which `replica`'s changes are held.
    pub fn of(&self, replica: Replica) -> u64 {
        self.0.get(&replica).copied().unwrap_or(0)
    }
}

/// Which shared song a song is: drawn by lot when it is first shared, and
/// kept with it, so that two programs know whether they hold the same.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SongId(pub u128);

impl Serialize for SongId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:032x}", self.0))
    }
}

impl<'de> Deserialize<'de> for SongId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        u128::from_str_radix(&text, 16).map(SongId).map_err(serde::de::Error::custom)
    }
}

/// The first id a program whose number is `replica` gives.
pub fn first_id(replica: Replica) -> u64 {
    (replica << ID_BITS) | 1
}

/// Whether `next` is still an id of the program that counts from
/// [`first_id`]: when it is not, the program has given all of its ids and
/// draws another number.
pub fn ids_left(next: u64) -> bool {
    next & ((1 << ID_BITS) - 1) != 0 && next <= MAX_ID
}
