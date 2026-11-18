//! What differs between two states of a song, small enough to keep for
//! every step of a session.
//!
//! A song with plugins is megabytes: one device's saved state can be
//! thousands of lines. A [`Change`] therefore holds only the parts of the
//! song that are not as they were, each whole: a track's own settings, a
//! clip, a device without its state, an envelope. Notes, which are most of
//! what a song is and are edited a few at a time, are kept as the stretch of
//! the clip's list that was replaced. A device's state is kept once, by what
//! is in it ([`Blobs`]), and named from every change that sets it: a hundred
//! steps on a song with a large state cost what they changed and no more.
//!
//! [`Change::between`] reads two projects, and [`Change::apply`] turns the
//! first into the second. The parts are compared field by field through
//! patterns that name every field of the model's structs, so that a field
//! added to the model does not compile here until it has been given a place.

use crate::formats;
use crate::project::{ClipContent, FORMAT_VERSION, Master, MidiContent};
use crate::{
    AutomationLane, AutomationTarget, ChainRef, Clip, ClipId, Device, DeviceId, Marker, Note, Project, Track, TrackId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// The name of a device state the journal holds: worked out from its bytes,
/// so that the same state is the same name wherever it turns up.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobId(u64);

impl Serialize for BlobId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:016x}", self.0))
    }
}

impl<'de> Deserialize<'de> for BlobId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        u64::from_str_radix(&text, 16).map(BlobId).map_err(serde::de::Error::custom)
    }
}

/// A number for some bytes that is the same on every machine and in every
/// version: eight bytes at a time through a multiply and a fold. Nothing
/// relies on two states never sharing one ([`Blobs::keep`] compares).
fn fingerprint(bytes: &[u8]) -> u64 {
    const PRIME: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut hash = 0xcbf2_9ce4_8422_2325 ^ (bytes.len() as u64).wrapping_mul(PRIME);
    let mut words = bytes.chunks_exact(8);
    for word in &mut words {
        let word = u64::from_le_bytes(word.try_into().expect("eight bytes"));
        hash = (hash ^ word).wrapping_mul(PRIME);
        hash ^= hash >> 29;
    }
    for byte in words.remainder() {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(PRIME);
    }
    hash ^ (hash >> 32)
}

/// The device states a journal holds, each once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Blobs {
    held: BTreeMap<BlobId, Arc<[u8]>>,
    bytes: usize,
}

impl Blobs {
    /// Holds `bytes`, if they are not held already, and names them.
    pub fn keep(&mut self, bytes: &[u8]) -> BlobId {
        let mut id = BlobId(fingerprint(bytes));
        loop {
            match self.held.get(&id) {
                Some(held) if **held == *bytes => return id,
                // Two states with one fingerprint: the next free name.
                Some(_) => id = BlobId(id.0.wrapping_add(1)),
                None => {
                    self.held.insert(id, Arc::from(bytes));
                    self.bytes += bytes.len();
                    return id;
                }
            }
        }
    }

    /// Holds `bytes` under the name a journal file gives them.
    pub(super) fn put(&mut self, id: BlobId, bytes: Vec<u8>) {
        self.bytes += bytes.len();
        if let Some(replaced) = self.held.insert(id, Arc::from(bytes)) {
            self.bytes -= replaced.len();
        }
    }

    pub fn get(&self, id: BlobId) -> Option<&Arc<[u8]>> {
        self.held.get(&id)
    }

    /// How many bytes are held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn ids(&self) -> impl Iterator<Item = BlobId> + '_ {
        self.held.keys().copied()
    }

    /// Lets go of every state `used` does not name.
    pub(super) fn retain(&mut self, used: &BTreeSet<BlobId>) {
        self.held.retain(|id, _| used.contains(id));
        self.bytes = self.held.values().map(|held| held.len()).sum();
    }
}

/// What a change does to a device's state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateChange {
    /// The device keeps the state it has.
    Keep,
    /// The device has no state from here on.
    Clear,
    /// The device takes the state held under this name.
    Set(BlobId),
}

/// Whether the songs a journal rebuilds carry their devices' states.
#[derive(Copy, Clone)]
pub enum States<'a> {
    /// They do: the song comes out as it was, byte for byte.
    With(&'a Blobs),
    /// They do not: every device comes out without a state. For looking at
    /// a song as it was, which needs no megabytes copied.
    Without,
}

/// A note as a journal writes it: pitch, start, length, velocity.
type NoteCode = (u8, f64, f64, u8);

fn code(note: &Note) -> NoteCode {
    (note.pitch, note.start, note.length, note.velocity)
}

fn note((pitch, start, length, velocity): NoteCode) -> Note {
    Note { pitch, start, length, velocity }
}

/// One part of a change. The parts of a change are applied in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// The song's own settings: a project without its tracks, master
    /// devices, envelopes and markers, which stay as they are.
    Song(Box<Project>),
    /// The number the next track, clip or device is given.
    Ids(u64),
    /// The markers, all of them.
    Markers(Vec<Marker>),
    /// A track's own settings: the track without its clips and devices,
    /// which stay as they are. A track the song does not have is added,
    /// empty, after the others.
    Track(Box<Track>),
    /// A clip, whole. One the song does not have is added to `track`.
    Clip { track: TrackId, clip: Box<Clip> },
    /// Everything about a MIDI clip but its notes, which stay as they are.
    ClipSettings(Box<Clip>),
    /// A stretch of a clip's notes replaced: `cut` notes from the one at
    /// `at` on make way for `put`.
    Notes { clip: ClipId, at: usize, cut: usize, put: Vec<NoteCode> },
    /// Which clips some tracks hold, in order. A clip named here is taken
    /// from wherever it is; one a listed track held and no list names is
    /// gone.
    Clips(Vec<(TrackId, Vec<ClipId>)>),
    /// A device without its state. One the song does not have is added to
    /// the end of `chain`.
    Device { chain: ChainRef, device: Box<Device>, state: StateChange },
    /// Which devices some chains hold, in order; as [`Op::Clips`].
    Devices(Vec<(ChainRef, Vec<DeviceId>)>),
    /// The tracks, in order. One that is not named is gone, with what it
    /// held.
    Tracks(Vec<TrackId>),
    /// An envelope, whole. One the song does not have is added.
    Lane(Box<AutomationLane>),
    /// The envelopes, in order, by what they automate. One that is not
    /// named is gone.
    Lanes(Vec<AutomationTarget>),
}

/// A journal that does not add up: a change names something the song it is
/// applied to does not have. The text says what.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Broken(pub String);

fn broken<T>(what: impl Into<String>) -> Result<T, Broken> {
    Err(Broken(what.into()))
}

/// What turns one state of a song into another.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Change {
    ops: Vec<Op>,
}

// ---- the parts of the model, by every field ------------------------------

/// Whether two songs have the same settings of their own.
fn same_song(a: &Project, b: &Project) -> bool {
    let Project {
        format_version,
        name,
        tempo,
        time_signature,
        key,
        tempo_changes,
        meter_changes,
        loop_region,
        tracks: _,
        master,
        automation: _,
        markers: _,
        next_id: _,
    } = a;
    let Master { volume_db, devices: _ } = master;
    *format_version == b.format_version
        && *name == b.name
        && *tempo == b.tempo
        && *time_signature == b.time_signature
        && *key == b.key
        && *tempo_changes == b.tempo_changes
        && *meter_changes == b.meter_changes
        && *loop_region == b.loop_region
        && *volume_db == b.master.volume_db
}

/// A song's own settings: the project with nothing in it.
fn song_of(project: &Project) -> Project {
    let Project {
        format_version,
        name,
        tempo,
        time_signature,
        key,
        tempo_changes,
        meter_changes,
        loop_region,
        tracks: _,
        master,
        automation: _,
        markers: _,
        next_id,
    } = project;
    let Master { volume_db, devices: _ } = master;
    Project {
        format_version: *format_version,
        name: name.clone(),
        tempo: *tempo,
        time_signature: *time_signature,
        key: *key,
        tempo_changes: tempo_changes.clone(),
        meter_changes: meter_changes.clone(),
        loop_region: *loop_region,
        tracks: Vec::new(),
        master: Master { volume_db: *volume_db, devices: Vec::new() },
        automation: Vec::new(),
        markers: Vec::new(),
        next_id: *next_id,
    }
}

/// Gives `project` the settings of `song`, and leaves what it holds.
fn set_song(project: &mut Project, song: &Project) {
    let Project {
        format_version,
        name,
        tempo,
        time_signature,
        key,
        tempo_changes,
        meter_changes,
        loop_region,
        tracks: _,
        master,
        automation: _,
        markers: _,
        next_id: _,
    } = song;
    let Master { volume_db, devices: _ } = master;
    project.format_version = *format_version;
    project.name.clone_from(name);
    project.tempo = *tempo;
    project.time_signature = *time_signature;
    project.key = *key;
    project.tempo_changes.clone_from(tempo_changes);
    project.meter_changes.clone_from(meter_changes);
    project.loop_region = *loop_region;
    project.master.volume_db = *volume_db;
}

/// Whether two tracks have the same settings of their own.
fn same_track(a: &Track, b: &Track) -> bool {
    let Track {
        id,
        name,
        kind,
        group,
        sends,
        color,
        volume_db,
        pan,
        mute,
        solo,
        devices: _,
        clips: _,
        profile,
        reference,
    } = a;
    *id == b.id
        && *reference == b.reference
        && *name == b.name
        && *kind == b.kind
        && *group == b.group
        && *sends == b.sends
        && *color == b.color
        && *volume_db == b.volume_db
        && *pan == b.pan
        && *mute == b.mute
        && *solo == b.solo
        && *profile == b.profile
}

/// A track's own settings: the track with nothing on it.
fn track_of(track: &Track) -> Track {
    let Track {
        id,
        name,
        kind,
        group,
        sends,
        color,
        volume_db,
        pan,
        mute,
        solo,
        devices: _,
        clips: _,
        profile,
        reference,
    } = track;
    Track {
        id: *id,
        name: name.clone(),
        kind: *kind,
        group: *group,
        sends: sends.clone(),
        color: *color,
        volume_db: *volume_db,
        pan: *pan,
        mute: *mute,
        solo: *solo,
        devices: Vec::new(),
        clips: Vec::new(),
        profile: profile.clone(),
        reference: reference.clone(),
    }
}

/// Whether two devices are the same but for their state.
fn same_device(a: &Device, b: &Device) -> bool {
    let Device { id, uri, name, enabled, params, state: _, preset, from_preset, sidechain } = a;
    *id == b.id
        && *uri == b.uri
        && *name == b.name
        && *enabled == b.enabled
        && *params == b.params
        && *preset == b.preset
        && *from_preset == b.from_preset
        && *sidechain == b.sidechain
}

/// A device without its state.
fn device_of(device: &Device) -> Device {
    let Device { id, uri, name, enabled, params, state: _, preset, from_preset, sidechain } = device;
    Device {
        id: *id,
        uri: uri.clone(),
        name: name.clone(),
        enabled: *enabled,
        params: params.clone(),
        state: None,
        preset: preset.clone(),
        from_preset: from_preset.clone(),
        sidechain: *sidechain,
    }
}

/// How two versions of one clip differ: `None` if in more than the
/// settings and notes of a MIDI clip, in which case it is kept whole.
fn midi_difference(old: &Clip, new: &Clip) -> Option<(bool, bool)> {
    let Clip { id, name, start, length, muted, content } = old;
    let (ClipContent::Midi(was), ClipContent::Midi(now)) = (content, &new.content) else { return None };
    let MidiContent { notes, loop_length, start_offset, controllers } = was;
    let settings = *id != new.id
        || *name != new.name
        || *start != new.start
        || *length != new.length
        || *muted != new.muted
        || *loop_length != now.loop_length
        || *start_offset != now.start_offset
        || *controllers != now.controllers;
    Some((settings, *notes != now.notes))
}

/// Everything about a MIDI clip but its notes.
fn clip_settings_of(clip: &Clip) -> Clip {
    let Clip { id, name, start, length, muted, content } = clip;
    let content = match content {
        ClipContent::Midi(MidiContent { notes: _, loop_length, start_offset, controllers }) => {
            ClipContent::Midi(MidiContent {
                notes: Vec::new(),
                loop_length: *loop_length,
                start_offset: *start_offset,
                controllers: controllers.clone(),
            })
        }
        audio @ ClipContent::Audio(_) => audio.clone(),
    };
    Clip { id: *id, name: name.clone(), start: *start, length: *length, muted: *muted, content }
}

/// The stretch of `old` that has to be replaced to make it `new`.
fn splice(clip: ClipId, old: &[Note], new: &[Note]) -> Op {
    let front = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let rest = (old.len() - front).min(new.len() - front);
    let back = old.iter().rev().zip(new.iter().rev()).take(rest).take_while(|(a, b)| a == b).count();
    Op::Notes {
        clip,
        at: front,
        cut: old.len() - front - back,
        put: new[front..new.len() - back].iter().map(code).collect(),
    }
}

impl Change {
    /// What turns `before` into `after`. States that are new to the journal
    /// are put into `blobs`.
    pub fn between(before: &Project, after: &Project, blobs: &mut Blobs) -> Change {
        let mut ops = Vec::new();
        if !same_song(before, after) {
            ops.push(Op::Song(Box::new(song_of(after))));
        }
        if before.next_id != after.next_id {
            ops.push(Op::Ids(after.next_id));
        }
        if before.markers != after.markers {
            ops.push(Op::Markers(after.markers.clone()));
        }

        // A track is nearly always where it was; otherwise it is looked for.
        let was_track = |index: usize, id: TrackId| -> Option<&Track> {
            let there = before.tracks.get(index).filter(|track| track.id == id);
            there.or_else(|| before.tracks.iter().find(|track| track.id == id))
        };
        for (index, track) in after.tracks.iter().enumerate() {
            if !was_track(index, track.id).is_some_and(|was| same_track(was, track)) {
                ops.push(Op::Track(Box::new(track_of(track))));
            }
        }

        // Clips. One that is not where it was on its track may have come
        // from another, which is looked up among all of them, once.
        let mut everywhere: Option<HashMap<ClipId, &Clip>> = None;
        let mut orders = Vec::new();
        for (index, track) in after.tracks.iter().enumerate() {
            let was_clips: &[Clip] = was_track(index, track.id).map_or(&[], |was| &was.clips);
            for (place, clip) in track.clips.iter().enumerate() {
                let there = was_clips.get(place).filter(|was| was.id == clip.id);
                let old = there.or_else(|| was_clips.iter().find(|was| was.id == clip.id)).or_else(|| {
                    let all = everywhere.get_or_insert_with(|| {
                        before.tracks.iter().flat_map(|track| &track.clips).map(|clip| (clip.id, clip)).collect()
                    });
                    all.get(&clip.id).copied()
                });
                match old {
                    Some(old) if old == clip => {}
                    Some(old) => match midi_difference(old, clip) {
                        Some((settings, notes)) => {
                            if settings {
                                ops.push(Op::ClipSettings(Box::new(clip_settings_of(clip))));
                            }
                            if notes {
                                ops.push(splice(clip.id, old.notes(), clip.notes()));
                            }
                        }
                        None => ops.push(Op::Clip { track: track.id, clip: Box::new(clip.clone()) }),
                    },
                    None => ops.push(Op::Clip { track: track.id, clip: Box::new(clip.clone()) }),
                }
            }
            if !was_clips.iter().map(|clip| clip.id).eq(track.clips.iter().map(|clip| clip.id)) {
                orders.push((track.id, track.clips.iter().map(|clip| clip.id).collect()));
            }
        }
        if !orders.is_empty() {
            ops.push(Op::Clips(orders));
        }

        // Devices, chain by chain: each track's, then the master's.
        let mut orders = Vec::new();
        let chains = after.tracks.iter().enumerate().map(|(index, track)| {
            let was: &[Device] = was_track(index, track.id).map_or(&[], |was| &was.devices);
            (ChainRef::Track(track.id), &track.devices, was)
        });
        for (chain, devices, was) in
            chains.chain([(ChainRef::Master, &after.master.devices, &before.master.devices[..])])
        {
            for (place, device) in devices.iter().enumerate() {
                let there = was.get(place).filter(|old| old.id == device.id);
                let old = there
                    .or_else(|| was.iter().find(|old| old.id == device.id))
                    .or_else(|| before.device(device.id).ok());
                let state = match old {
                    Some(old) if old.state == device.state => {
                        if same_device(old, device) {
                            continue;
                        }
                        StateChange::Keep
                    }
                    _ => match &device.state {
                        Some(bytes) => StateChange::Set(blobs.keep(bytes)),
                        None => StateChange::Clear,
                    },
                };
                ops.push(Op::Device { chain, device: Box::new(device_of(device)), state });
            }
            if !was.iter().map(|device| device.id).eq(devices.iter().map(|device| device.id)) {
                orders.push((chain, devices.iter().map(|device| device.id).collect()));
            }
        }
        if !orders.is_empty() {
            ops.push(Op::Devices(orders));
        }

        // After what the tracks hold: a track that is gone takes with it
        // only what was not moved elsewhere first.
        if !before.tracks.iter().map(|track| track.id).eq(after.tracks.iter().map(|track| track.id)) {
            ops.push(Op::Tracks(after.tracks.iter().map(|track| track.id).collect()));
        }

        for (index, lane) in after.automation.iter().enumerate() {
            let there = before.automation.get(index).filter(|was| was.target == lane.target);
            let old = there.or_else(|| before.automation.iter().find(|was| was.target == lane.target));
            if old != Some(lane) {
                ops.push(Op::Lane(Box::new(lane.clone())));
            }
        }
        if !before.automation.iter().map(|lane| &lane.target).eq(after.automation.iter().map(|lane| &lane.target)) {
            ops.push(Op::Lanes(after.automation.iter().map(|lane| lane.target.clone()).collect()));
        }
        Change { ops }
    }

    /// The change that makes a device what `device` is, in `chain`: the
    /// device without its state, and its state too if `state_changed`.
    pub(super) fn of_device(chain: ChainRef, device: &Device, state_changed: bool, blobs: &mut Blobs) -> Change {
        let state = match (&device.state, state_changed) {
            (_, false) => StateChange::Keep,
            (Some(bytes), true) => StateChange::Set(blobs.keep(bytes)),
            (None, true) => StateChange::Clear,
        };
        Change { ops: vec![Op::Device { chain, device: Box::new(device_of(device)), state }] }
    }

    /// Whether this is a change to the device `id` and to nothing else.
    pub(super) fn is_of_device(&self, id: DeviceId) -> bool {
        matches!(self.ops.as_slice(), [Op::Device { device, .. }] if device.id == id)
    }

    /// Makes this change to one device the one that also does what
    /// `newer`, a change to the same device, does: the device as the newer
    /// has it, with the state the newer sets or, if it sets none, this one.
    pub(super) fn follow_device(&mut self, newer: Change) {
        let (Some(Op::Device { state: older, .. }), Some(Op::Device { chain, device, state })) =
            (self.ops.first(), newer.ops.into_iter().next())
        else {
            return;
        };
        let state = if state == StateChange::Keep { *older } else { state };
        self.ops = vec![Op::Device { chain, device, state }];
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Makes what the change kept of devices and envelopes say in this
    /// build what it meant in the build that made it (see
    /// [`formats`]), as a project file of an older format is made to when
    /// it is opened: a journal that was begun before a device changed then
    /// leads to the song as it sounded.
    ///
    /// `format` is the song's format where the change begins, and where it
    /// ends afterwards. One build makes all of a change, and a change that
    /// carries the song's own settings says which format that build wrote:
    /// the step that takes a journal up again under a newer build is such
    /// a one, and is already in that build's terms. `types` is the type of
    /// every device met so far, which an envelope is read by.
    pub(super) fn keep_format(&mut self, format: &mut u32, types: &mut BTreeMap<DeviceId, String>) {
        let says = self.ops.iter().find_map(|op| match op {
            Op::Song(song) => Some(song.format_version),
            _ => None,
        });
        *format = says.unwrap_or(*format);
        for op in &mut self.ops {
            match op {
                Op::Device { device, .. } => {
                    types.insert(device.id, device.uri.clone());
                    formats::keep_device(*format, device);
                }
                // The songs a journal rebuilds are this build's.
                Op::Song(song) if song.format_version < FORMAT_VERSION => song.format_version = FORMAT_VERSION,
                _ => {}
            }
        }
        // An envelope, once every device of the change is known.
        for op in &mut self.ops {
            if let Op::Lane(lane) = op
                && let AutomationTarget::DeviceParam { device, .. } = &lane.target
                && let Some(uri) = types.get(device)
            {
                formats::keep_lane(*format, uri, lane);
            }
        }
    }

    /// Its parts, in the order they are applied.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// How many parts it has.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Makes this change the two, one after the other.
    pub(super) fn then(&mut self, next: Change) {
        self.ops.extend(next.ops);
    }

    /// Forgets its parts from the one numbered `len` on.
    pub(super) fn truncate(&mut self, len: usize) {
        self.ops.truncate(len);
    }

    /// The states it names.
    pub fn blobs(&self) -> impl Iterator<Item = BlobId> + '_ {
        self.ops.iter().filter_map(|op| match op {
            Op::Device { state: StateChange::Set(id), .. } => Some(*id),
            _ => None,
        })
    }

    /// Turns `project` into what it was after this change. A change that
    /// names something `project` does not have leaves it half applied and
    /// says what was missing: the journal it came from is damaged.
    pub fn apply(&self, project: &mut Project, states: States<'_>) -> Result<(), Broken> {
        self.ops.iter().try_for_each(|op| apply(op, project, states))
    }
}

/// The devices of a chain.
fn chain_mut(project: &mut Project, chain: ChainRef) -> Result<&mut Vec<Device>, Broken> {
    match chain {
        ChainRef::Master => Ok(&mut project.master.devices),
        ChainRef::Track(id) => match project.tracks.iter_mut().find(|track| track.id == id) {
            Some(track) => Ok(&mut track.devices),
            None => broken(format!("a device belongs on {id}, which the song does not have")),
        },
    }
}

fn every_chain(project: &mut Project) -> impl Iterator<Item = &mut Vec<Device>> {
    project.tracks.iter_mut().map(|track| &mut track.devices).chain([&mut project.master.devices])
}

fn apply(op: &Op, project: &mut Project, states: States<'_>) -> Result<(), Broken> {
    match op {
        Op::Song(song) => set_song(project, song),
        Op::Ids(next) => project.next_id = *next,
        Op::Markers(markers) => project.markers.clone_from(markers),
        Op::Track(settings) => match project.tracks.iter_mut().find(|track| track.id == settings.id) {
            Some(track) => {
                let (clips, devices) = (std::mem::take(&mut track.clips), std::mem::take(&mut track.devices));
                *track = Track { clips, devices, ..(**settings).clone() };
            }
            None => project.tracks.push((**settings).clone()),
        },
        Op::Clip { track, clip } => {
            let held = project.tracks.iter_mut().flat_map(|track| &mut track.clips).find(|held| held.id == clip.id);
            match held {
                Some(held) => *held = (**clip).clone(),
                None => match project.tracks.iter_mut().find(|held| held.id == *track) {
                    Some(track) => track.clips.push((**clip).clone()),
                    None => return broken(format!("{} belongs on {track}, which the song does not have", clip.id)),
                },
            }
        }
        Op::ClipSettings(settings) => {
            let held = project.tracks.iter_mut().flat_map(|track| &mut track.clips).find(|held| held.id == settings.id);
            let Some(held) = held else {
                return broken(format!("{} is changed, and the song does not have it", settings.id));
            };
            let notes = match &mut held.content {
                ClipContent::Midi(midi) => std::mem::take(&mut midi.notes),
                ClipContent::Audio(_) => Vec::new(),
            };
            *held = (**settings).clone();
            if let ClipContent::Midi(midi) = &mut held.content {
                midi.notes = notes;
            }
        }
        Op::Notes { clip, at, cut, put } => {
            let held = project.tracks.iter_mut().flat_map(|track| &mut track.clips).find(|held| held.id == *clip);
            let Some(Clip { content: ClipContent::Midi(midi), .. }) = held else {
                return broken(format!("notes are written to {clip}, and the song has no such MIDI clip"));
            };
            let end = at.checked_add(*cut).filter(|end| *end <= midi.notes.len());
            let Some(end) = end else {
                return broken(format!(
                    "notes {at} to {} of {clip} are replaced, and it has {}",
                    at + cut,
                    midi.notes.len()
                ));
            };
            midi.notes.splice(*at..end, put.iter().copied().map(note));
        }
        Op::Clips(orders) => {
            // Everything the listed tracks hold, to be dealt out again.
            let mut pool: HashMap<ClipId, Clip> = HashMap::new();
            for (id, _) in orders {
                let Some(track) = project.tracks.iter_mut().find(|track| track.id == *id) else {
                    return broken(format!("clips are put on {id}, which the song does not have"));
                };
                pool.extend(track.clips.drain(..).map(|clip| (clip.id, clip)));
            }
            for (id, order) in orders {
                let mut clips = Vec::with_capacity(order.len());
                for wanted in order {
                    // From the listed tracks, or from one that is about to
                    // go with everything else it holds.
                    let clip = pool.remove(wanted).or_else(|| {
                        project.tracks.iter_mut().find_map(|track| {
                            let index = track.clips.iter().position(|clip| clip.id == *wanted)?;
                            Some(track.clips.remove(index))
                        })
                    });
                    match clip {
                        Some(clip) => clips.push(clip),
                        None => return broken(format!("{wanted} is put on {id}, and the song does not have it")),
                    }
                }
                if let Some(track) = project.tracks.iter_mut().find(|track| track.id == *id) {
                    track.clips = clips;
                }
            }
        }
        Op::Device { chain, device, state } => {
            let new_state = |held: Option<Vec<u8>>| -> Result<Option<Vec<u8>>, Broken> {
                match (state, states) {
                    (_, States::Without) | (StateChange::Clear, _) => Ok(None),
                    (StateChange::Keep, States::With(_)) => Ok(held),
                    (StateChange::Set(id), States::With(blobs)) => match blobs.get(*id) {
                        Some(bytes) => Ok(Some(bytes.to_vec())),
                        None => broken(format!("the state of {} is not in the journal", device.id)),
                    },
                }
            };
            let held = every_chain(project).flatten().find(|held| held.id == device.id);
            if let Some(held) = held {
                let state = new_state(held.state.take())?;
                *held = Device { state, ..(**device).clone() };
            } else {
                let state = new_state(None)?;
                chain_mut(project, *chain)?.push(Device { state, ..(**device).clone() });
            }
        }
        Op::Devices(orders) => {
            let mut pool: HashMap<DeviceId, Device> = HashMap::new();
            for (chain, _) in orders {
                pool.extend(chain_mut(project, *chain)?.drain(..).map(|device| (device.id, device)));
            }
            for (chain, order) in orders {
                let mut devices = Vec::with_capacity(order.len());
                for wanted in order {
                    let device = pool.remove(wanted).or_else(|| {
                        every_chain(project).find_map(|devices| {
                            let index = devices.iter().position(|device| device.id == *wanted)?;
                            Some(devices.remove(index))
                        })
                    });
                    match device {
                        Some(device) => devices.push(device),
                        None => return broken(format!("{wanted} is put in a chain, and the song does not have it")),
                    }
                }
                *chain_mut(project, *chain)? = devices;
            }
        }
        Op::Tracks(order) => {
            let mut pool: HashMap<TrackId, Track> = project.tracks.drain(..).map(|track| (track.id, track)).collect();
            for wanted in order {
                match pool.remove(wanted) {
                    Some(track) => project.tracks.push(track),
                    None => return broken(format!("{wanted} is among the tracks, and the song does not have it")),
                }
            }
        }
        Op::Lane(lane) => match project.automation.iter_mut().find(|held| held.target == lane.target) {
            Some(held) => *held = (**lane).clone(),
            None => project.automation.push((**lane).clone()),
        },
        Op::Lanes(order) => {
            let mut held = std::mem::take(&mut project.automation);
            for wanted in order {
                match held.iter().position(|lane| lane.target == *wanted) {
                    Some(index) => project.automation.push(held.remove(index)),
                    None => return broken("an envelope is among the lanes, and the song does not have it"),
                }
            }
        }
    }
    Ok(())
}

/// `project` without its devices' states, and the states: each held in
/// `blobs` and named by its device.
pub(super) fn without_states(project: &Project, blobs: &mut Blobs) -> (Project, BTreeMap<DeviceId, BlobId>) {
    let mut bare = song_of(project);
    let mut states = BTreeMap::new();
    let mut strip = |devices: &[Device]| -> Vec<Device> {
        let bare = devices.iter().map(|device| {
            if let Some(state) = &device.state {
                states.insert(device.id, blobs.keep(state));
            }
            device_of(device)
        });
        bare.collect()
    };
    bare.master.devices = strip(&project.master.devices);
    let tracks = project.tracks.iter().map(|track| {
        let devices = strip(&track.devices);
        Track { devices, clips: track.clips.clone(), ..track_of(track) }
    });
    bare.tracks = tracks.collect();
    bare.automation.clone_from(&project.automation);
    bare.markers.clone_from(&project.markers);
    (bare, states)
}

/// Gives the devices of `project` the states `named` for them.
pub(super) fn with_states(
    project: &mut Project,
    named: &BTreeMap<DeviceId, BlobId>,
    blobs: &Blobs,
) -> Result<(), Broken> {
    for device in every_chain(project).flatten() {
        device.state = match named.get(&device.id) {
            None => None,
            Some(id) => match blobs.get(*id) {
                Some(bytes) => Some(bytes.to_vec()),
                None => return broken(format!("the state of {} is not in the journal", device.id)),
            },
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::AutomationPoint;
    use crate::{ControllerKind, InstrumentProfile, Role};

    fn note_at(start: f64, pitch: u8) -> Note {
        Note { pitch, start, length: 0.5, velocity: 96 }
    }

    /// Two tracks with a device and a clip each, one device with a state,
    /// an envelope and a marker.
    fn song() -> (Project, [TrackId; 2], [ClipId; 2], [DeviceId; 2]) {
        let mut project = Project::new("Song");
        let lead = project.add_track("Lead");
        let bass = project.add_track("Bass");
        let synth = project.add_device(ChainRef::Track(lead), "builtin:polysynth", "Poly Synth", None).unwrap();
        let sampler = project.add_device(ChainRef::Track(bass), "clap:big", "Big Sampler", None).unwrap();
        project.device_mut(sampler).unwrap().state = Some(vec![7; 4096]);
        let hook = project.add_midi_clip(lead, "Hook", 0.0, 8.0).unwrap();
        let line = project.add_midi_clip(bass, "Line", 8.0, 8.0).unwrap();
        let notes: Vec<Note> = (0..16).map(|step| note_at(step as f64 * 0.5, 48 + step)).collect();
        project.clip_mut(hook).unwrap().add_notes(&notes).unwrap();
        project
            .set_automation(AutomationTarget::TrackVolume { track: lead }, vec![AutomationPoint::new(0.0, -6.0)])
            .unwrap();
        project.add_marker(8.0, "Verse").unwrap();
        (project, [lead, bass], [hook, line], [synth, sampler])
    }

    /// Checks that what is kept of an edit is enough to make it again, and
    /// returns what was kept.
    fn kept<R>(before: &Project, blobs: &mut Blobs, edit: impl FnOnce(&mut Project) -> R) -> (Project, Change) {
        let mut after = before.clone();
        edit(&mut after);
        let change = Change::between(before, &after, blobs);
        let mut again = before.clone();
        change.apply(&mut again, States::With(blobs)).expect("the change applies to what it was read from");
        assert_eq!(again, after, "the change makes the song what it became: {change:?}");
        (after, change)
    }

    fn size(change: &Change) -> usize {
        serde_json::to_string(change).unwrap().len()
    }

    #[test]
    fn every_kind_of_edit_is_kept_as_what_it_changed_and_made_again_from_it() {
        let (project, [lead, bass], [hook, line], [synth, sampler]) = song();
        let mut blobs = Blobs::default();
        let blobs = &mut blobs;
        assert!(Change::between(&project, &project, blobs).is_empty());

        // The song's own settings.
        let (project, change) = kept(&project, blobs, |p| {
            p.set_tempo(96.0).unwrap();
            p.set_loop(true, 8.0, 16.0).unwrap();
            p.set_master_volume(-3.0).unwrap();
            p.name = "Night Drive".into();
        });
        assert!(matches!(change.ops(), [Op::Song(song)] if song.tracks.is_empty() && song.tempo == 96.0));
        let (project, change) = kept(&project, blobs, |p| p.add_marker(16.0, "Chorus").unwrap());
        assert!(matches!(change.ops(), [Op::Markers(markers)] if markers.len() == 2));

        // A track's settings: the track goes along without what it holds.
        let (project, change) = kept(&project, blobs, |p| {
            p.set_track_volume(lead, -4.5).unwrap();
            p.set_track_pan(lead, 0.25).unwrap();
            p.track_mut(lead).unwrap().mute = true;
            let profile = InstrumentProfile { role: Some(Role::Pitched), ..InstrumentProfile::default() };
            p.set_track_profile(lead, profile).unwrap();
        });
        assert!(matches!(change.ops(), [Op::Track(track)] if track.clips.is_empty() && track.devices.is_empty()));

        // Tracks added, moved and removed, with what they hold.
        let (project, change) = kept(&project, blobs, |p| {
            let drums = p.add_track("Drums");
            p.add_device(ChainRef::Track(drums), "builtin:drumkit", "Drum Kit", None).unwrap();
            let beat = p.add_midi_clip(drums, "Beat", 0.0, 4.0).unwrap();
            p.clip_mut(beat).unwrap().add_notes(&[note_at(0.0, 36), note_at(1.0, 38)]).unwrap();
            p.move_track(drums, 0).unwrap();
        });
        assert!(change.ops().iter().any(|op| matches!(op, Op::Tracks(order) if order.len() == 3)));
        let drums = project.tracks[0].id;
        let (project, _) = kept(&project, blobs, |p| p.duplicate_track(bass).unwrap());
        let (project, change) = kept(&project, blobs, |p| p.remove_track(drums).unwrap());
        assert!(matches!(change.ops(), [Op::Tracks(order)] if !order.contains(&drums)), "{change:?}");

        // Clips: moved, to another track, resized, split, looped, removed.
        let (project, change) = kept(&project, blobs, |p| p.set_clip_span(hook, 4.0, 8.0).unwrap());
        assert!(matches!(change.ops(), [Op::ClipSettings(clip)] if clip.notes().is_empty() && clip.start == 4.0));
        let (project, change) = kept(&project, blobs, |p| p.move_clip(hook, Some(bass), 16.0).unwrap());
        assert!(matches!(change.ops(), [Op::ClipSettings(_), Op::Clips(orders)] if orders.len() == 2), "{change:?}");
        let (project, _) = kept(&project, blobs, |p| p.move_clip(hook, Some(lead), 0.0).unwrap());
        let (project, _) = kept(&project, blobs, |p| p.split_clip(hook, 4.0).unwrap());
        let (project, _) = kept(&project, blobs, |p| p.duplicate_clip(line, 32.0, Some(lead)).unwrap());
        let (project, _) = kept(&project, blobs, |p| {
            let clip = p.clip_mut(line).unwrap();
            clip.set_loop_length(Some(2.0)).unwrap();
            clip.set_controller(ControllerKind::MOD_WHEEL, vec![AutomationPoint::new(0.0, 0.5)]).unwrap();
            clip.muted = true;
        });
        let (project, _) = kept(&project, blobs, |p| {
            p.add_audio_clip(bass, "Loop", 40.0, 8.0, crate::project::AudioContent::new("D:/loop.wav"))
        });
        let (project, change) = kept(&project, blobs, |p| p.remove_clip(line).unwrap());
        assert!(matches!(change.ops(), [Op::Clips(orders)] if orders.len() == 1), "{change:?}");

        // Devices: a parameter, a track to listen to, a state, bypassed,
        // moved off, removed.
        let (project, change) = kept(&project, blobs, |p| {
            p.device_mut(sampler).unwrap().params.insert("gain".into(), 0.5);
        });
        assert!(matches!(change.ops(), [Op::Device { state: StateChange::Keep, .. }]), "{change:?}");
        let (project, change) = kept(&project, blobs, |p| p.set_sidechain(sampler, Some(lead)).unwrap());
        assert!(
            matches!(change.ops(), [Op::Device { device, state: StateChange::Keep, .. }] if device.sidechain == Some(lead)),
            "{change:?}"
        );
        let (project, change) = kept(&project, blobs, |p| p.device_mut(sampler).unwrap().state = Some(vec![9; 4096]));
        assert!(matches!(change.ops(), [Op::Device { state: StateChange::Set(_), .. }]));
        let (project, change) = kept(&project, blobs, |p| p.device_mut(sampler).unwrap().state = None);
        assert!(matches!(change.ops(), [Op::Device { state: StateChange::Clear, .. }]));
        let (project, _) = kept(&project, blobs, |p| {
            p.device_mut(synth).unwrap().enabled = false;
            p.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
            p.add_device(ChainRef::Track(lead), "builtin:reverb", "Reverb", Some(0)).unwrap();
        });
        let (project, _) = kept(&project, blobs, |p| p.remove_device(synth).unwrap());

        // Envelopes: changed, added, switched off, removed.
        let volume = AutomationTarget::TrackVolume { track: lead };
        let pan = AutomationTarget::TrackPan { track: bass };
        let (project, _) = kept(&project, blobs, |p| {
            let points = vec![AutomationPoint::new(0.0, -6.0), AutomationPoint::new(8.0, 0.0)];
            p.set_automation(volume.clone(), points).unwrap();
            p.set_automation(pan.clone(), vec![AutomationPoint::new(0.0, -1.0)]).unwrap();
        });
        let (project, _) = kept(&project, blobs, |p| p.set_automation_enabled(&pan, false).unwrap());
        let (project, change) = kept(&project, blobs, |p| p.set_automation(volume.clone(), Vec::new()).unwrap());
        assert!(matches!(change.ops(), [Op::Lanes(order)] if *order == [pan.clone()]), "{change:?}");

        // Time edited on every track at once.
        let (project, _) = kept(&project, blobs, |p| p.insert_time(4.0, 8.0).unwrap());
        let (_, _) = kept(&project, blobs, |p| p.delete_time(0.0, 16.0).unwrap());
    }

    #[test]
    fn a_few_notes_changed_in_a_long_clip_are_kept_as_those_notes() {
        let (mut project, _, [hook, _], _) = song();
        let many: Vec<Note> = (0..2000).map(|step| note_at(8.0 + step as f64 * 0.25, 40 + (step % 40) as u8)).collect();
        project.clip_mut(hook).unwrap().add_notes(&many).unwrap();
        let whole = serde_json::to_string(project.clip(hook).unwrap()).unwrap().len();
        let mut blobs = Blobs::default();

        // One added in the middle, one removed, one moved, all replaced.
        let (project, change) =
            kept(&project, &mut blobs, |p| p.clip_mut(hook).unwrap().add_notes(&[note_at(100.1, 60)]).unwrap());
        assert!(matches!(change.ops(), [Op::Notes { cut: 0, put, .. }] if put.len() == 1), "{change:?}");
        assert!(size(&change) < 80, "{} bytes for one note, where the clip is {whole}", size(&change));
        let (project, change) = kept(&project, &mut blobs, |p| {
            p.clip_mut(hook).unwrap().remove_notes(|note| note.start == 100.1).unwrap();
        });
        assert!(matches!(change.ops(), [Op::Notes { cut: 1, put, .. }] if put.is_empty()), "{change:?}");
        let (project, change) = kept(&project, &mut blobs, |p| {
            let mut notes = p.clip(hook).unwrap().notes().to_vec();
            notes[1000].velocity = 40;
            p.clip_mut(hook).unwrap().set_notes(notes).unwrap();
        });
        assert!(matches!(change.ops(), [Op::Notes { at: 1000, cut: 1, put, .. }] if put.len() == 1), "{change:?}");
        let (project, _) = kept(&project, &mut blobs, |p| p.clip_mut(hook).unwrap().set_notes(Vec::new()).unwrap());
        // The same note twice in a row is still told apart from one.
        let twice = [note_at(0.0, 60), note_at(0.0, 60)];
        let (project, _) = kept(&project, &mut blobs, |p| p.clip_mut(hook).unwrap().add_notes(&twice).unwrap());
        let (_, change) = kept(&project, &mut blobs, |p| {
            p.clip_mut(hook).unwrap().set_notes(vec![note_at(0.0, 60)]).unwrap();
        });
        assert!(matches!(change.ops(), [Op::Notes { cut: 1, put, .. }] if put.is_empty()), "{change:?}");
    }

    #[test]
    fn a_state_is_held_once_however_often_it_comes_back() {
        let mut blobs = Blobs::default();
        let (large, other) = (vec![3u8; 300_000], vec![4u8; 300_000]);
        let first = blobs.keep(&large);
        assert_eq!(blobs.keep(&large), first);
        let second = blobs.keep(&other);
        assert_ne!(first, second);
        assert_eq!((blobs.len(), blobs.bytes()), (2, 600_000));
        assert_eq!(&**blobs.get(first).unwrap(), &large[..]);
        assert_eq!(blobs.keep(&[]), blobs.keep(&[]));

        // Two states that share a fingerprint are still two states.
        let mut blobs = Blobs::default();
        let taken = BlobId(fingerprint(b"one"));
        blobs.put(taken, b"another".to_vec());
        let id = blobs.keep(b"one");
        assert_ne!(id, taken);
        assert_eq!(&**blobs.get(id).unwrap(), b"one");
        assert_eq!(blobs.keep(b"one"), id);

        // A name reads back as it was written.
        let text = serde_json::to_string(&first).unwrap();
        assert_eq!(text.len(), 18);
        assert_eq!(serde_json::from_str::<BlobId>(&text).unwrap(), first);

        blobs.retain(&BTreeSet::from([id]));
        assert_eq!((blobs.len(), blobs.bytes()), (1, 3));
    }

    #[test]
    fn a_song_can_be_rebuilt_without_its_states_and_a_damaged_change_says_what_is_missing() {
        let (project, _, [hook, _], [_, sampler]) = song();
        let mut blobs = Blobs::default();
        let mut after = project.clone();
        after.device_mut(sampler).unwrap().state = Some(vec![1; 64]);
        after.device_mut(sampler).unwrap().name = "Renamed".into();
        let change = Change::between(&project, &after, &mut blobs);

        let mut light = project.clone();
        change.apply(&mut light, States::Without).unwrap();
        assert_eq!(light.device(sampler).unwrap().name, "Renamed");
        assert_eq!(light.device(sampler).unwrap().state, None);

        // The state it names is gone from the journal.
        let error = change.apply(&mut project.clone(), States::With(&Blobs::default())).unwrap_err();
        assert!(error.0.contains("is not in the journal"), "{error}");
        // A clip that is not there.
        let notes = Change { ops: vec![Op::Notes { clip: ClipId(999), at: 0, cut: 0, put: Vec::new() }] };
        assert!(notes.apply(&mut project.clone(), States::Without).is_err());
        let beyond = Change { ops: vec![Op::Notes { clip: hook, at: 10, cut: 100, put: Vec::new() }] };
        assert!(beyond.apply(&mut project.clone(), States::Without).is_err());
        let order = Change { ops: vec![Op::Tracks(vec![TrackId(999)])] };
        assert!(order.apply(&mut project.clone(), States::Without).is_err());

        // A song parts with its states and takes them back.
        let (bare, named) = without_states(&project, &mut blobs);
        assert!(bare.devices().all(|device| device.state.is_none()));
        let mut whole = bare.clone();
        with_states(&mut whole, &named, &blobs).unwrap();
        assert_eq!(whole, project);
    }
}
