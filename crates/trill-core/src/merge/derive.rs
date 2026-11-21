//! The song worked out from its properties: the one function of them that
//! every program that holds them comes to the same [`Project`] by.
//!
//! Properties alone can say what no song may be, because each was set by
//! an edit that was allowed where it was made. What is done about each
//! such case is decided here, once, and is the same everywhere:
//!
//! * **A thing that was removed is not in the song**, and neither is what
//!   is in it or goes with it: the clips and devices of a track, the notes
//!   of a clip, an envelope of something that is gone, a send to a return
//!   that is gone, a group that is gone (its members play to the master), a
//!   key that is gone (the device listens to its own input). All of it is
//!   held, and is in the song again when the thing is.
//! * **An envelope without a point is no envelope**, and a curve without
//!   one no curve: two people who each remove one of its last two points
//!   have removed it.
//! * **What the tempo does at one beat is one thing** (and the time
//!   signature): of two set a hair apart, the later.
//! * **Routing is taken in the order it was made.** What plays through
//!   what (a group, a send set by hand or by its envelope, a device that
//!   listens to a track, a track that is a reference and feeds nothing) is
//!   gone through from the earliest, and each is taken if the song as taken
//!   so far would have allowed that edit. One that it would have refused
//!   (it closes a loop, or it has a reference feed something) is set aside
//!   ([`Aside`]) and is in the song as soon as what stands in its way is
//!   not. That is what would have happened had the two edits been made one
//!   after the other, and it is the edit method's own rule that decides.

use super::doc::{ClipHeld, Doc, Held};
use super::props::{group_prop, sidechain_prop, travel_prop};
use super::{Home, Key, REFERENCE, Stamp, Thing};
use crate::controllers::ControllerCurve;
use crate::project::{
    AutomationLane, AutomationPoint, AutomationTarget, ChainRef, Clip, ClipContent, Device, FORMAT_VERSION, Marker,
    Project, Send, Track, TrackKind, sort_clips, sort_notes, sort_points, sort_tempo_changes, valid_signature,
    valid_tempo,
};
use crate::tempo::{MeterChange, SAME_BEAT, TempoChange};
use crate::{DeviceId, InstrumentProfile, TrackId};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Why an edit that was allowed where it was made is not in the song.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// It would have two tracks each wait for the other.
    Loop,
    /// It would have a reference feed something in the song, or make a
    /// reference of a track that does.
    Reference,
}

/// An edit that was set aside: the property it set, which is held as it
/// was set and is not in the song.
#[derive(Clone, Debug, PartialEq)]
pub struct Aside {
    pub key: Key,
    pub stamp: Stamp,
    pub why: Why,
    /// The property that stands in its way: the one made before it.
    pub by: Option<Key>,
}

/// The song as it stands, and what was set aside to make it one.
#[derive(Clone, Debug, PartialEq)]
pub struct Derived {
    /// (The number the next thing is given is whoever asks' own to say.)
    pub project: Project,
    pub aside: Vec<Aside>,
}

/// Something that says what plays through what.
#[derive(Clone, Debug)]
enum Wish {
    Group { track: TrackId, group: TrackId },
    Send { track: TrackId, to: TrackId },
    /// A send that is its envelope.
    SendLane { track: TrackId, to: TrackId },
    Listen { device: DeviceId, chain: ChainRef, to: TrackId },
    Reference { track: TrackId },
}

impl Wish {
    /// The track that has to be played first, and the one after it, where
    /// the wish has one wait for another.
    fn edge(&self) -> Option<(TrackId, TrackId)> {
        match self {
            Wish::Group { track, group } => Some((*track, *group)),
            Wish::Send { track, to } | Wish::SendLane { track, to } => Some((*track, *to)),
            Wish::Listen { chain: ChainRef::Track(on), to, .. } => Some((*to, *on)),
            Wish::Listen { chain: ChainRef::Master, .. } | Wish::Reference { .. } => None,
        }
    }
}

/// Whether what the wishes say can all hold at once.
fn all_hold(wishes: &[(Stamp, Key, Wish)]) -> bool {
    let references: HashSet<TrackId> = (wishes.iter())
        .filter_map(|(_, _, wish)| match wish {
            Wish::Reference { track } => Some(*track),
            _ => None,
        })
        .collect();
    let feeds = |wish: &Wish| match wish {
        Wish::Group { track, .. } | Wish::Send { track, .. } | Wish::SendLane { track, .. } => {
            references.contains(track)
        }
        Wish::Listen { to, .. } => references.contains(to),
        Wish::Reference { .. } => false,
    };
    if !references.is_empty() && wishes.iter().any(|(_, _, wish)| feeds(wish)) {
        return false;
    }
    // No loop: tracks that wait for nothing are taken away until none is
    // left, or none of those left waits for nothing.
    let mut waits: HashMap<TrackId, usize> = HashMap::new();
    let mut after: HashMap<TrackId, Vec<TrackId>> = HashMap::new();
    for (first, then) in wishes.iter().filter_map(|(_, _, wish)| wish.edge()) {
        *waits.entry(then).or_default() += 1;
        waits.entry(first).or_default();
        after.entry(first).or_default().push(then);
    }
    let mut ready: Vec<TrackId> = waits.iter().filter(|(_, count)| **count == 0).map(|(track, _)| *track).collect();
    let mut played = 0;
    while let Some(track) = ready.pop() {
        played += 1;
        for then in after.get(&track).into_iter().flatten() {
            let count = waits.get_mut(then).expect("every track of an edge is counted");
            *count -= 1;
            if *count == 0 {
                ready.push(*then);
            }
        }
    }
    played == waits.len()
}

/// The wishes that hold, taken in the order they were made, and the ones
/// that were set aside.
fn admit(mut wishes: Vec<(Stamp, Key, Wish)>) -> (Vec<Wish>, Vec<Aside>) {
    if all_hold(&wishes) {
        return (wishes.into_iter().map(|(_, _, wish)| wish).collect(), Vec::new());
    }
    wishes.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let mut taken: Vec<(Stamp, Key, Wish)> = Vec::new();
    let mut aside = Vec::new();
    for (stamp, key, wish) in wishes {
        // The way from one track to another along what was taken, as the
        // wishes it goes by.
        let way = |from: TrackId, to: TrackId| -> Option<Vec<usize>> {
            let mut seen = HashSet::from([from]);
            let mut ways: Vec<(TrackId, Vec<usize>)> = vec![(from, Vec::new())];
            while let Some((at, by)) = ways.pop() {
                if at == to {
                    return Some(by);
                }
                for (index, (_, _, other)) in taken.iter().enumerate() {
                    if let Some((first, then)) = other.edge()
                        && first == at
                        && seen.insert(then)
                    {
                        ways.push((then, by.iter().copied().chain([index]).collect()));
                    }
                }
            }
            None
        };
        let feeder = |of: TrackId| {
            taken.iter().position(|(_, _, other)| match other {
                Wish::Group { track, .. } | Wish::Send { track, .. } | Wish::SendLane { track, .. } => *track == of,
                Wish::Listen { to, .. } => *to == of,
                Wish::Reference { .. } => false,
            })
        };
        let reference =
            |of: TrackId| taken.iter().position(|(_, _, other)| matches!(other, Wish::Reference { track } if *track == of));
        let refused: Option<(Why, Option<usize>)> = match &wish {
            Wish::Reference { track } => feeder(*track).map(|by| (Why::Reference, Some(by))),
            Wish::Group { track, .. } | Wish::Send { track, .. } | Wish::SendLane { track, .. }
                if reference(*track).is_some() =>
            {
                Some((Why::Reference, reference(*track)))
            }
            Wish::Listen { to, .. } if reference(*to).is_some() => Some((Why::Reference, reference(*to))),
            wish => wish.edge().and_then(|(first, then)| {
                // It closes a loop if the one that is to wait is already
                // waited for by the other.
                let back = if first == then { Some(Vec::new()) } else { way(then, first) };
                back.map(|by| (Why::Loop, by.into_iter().max_by_key(|index| taken[*index].0)))
            }),
        };
        match refused {
            Some((why, by)) => aside.push(Aside { key, stamp, why, by: by.map(|index| taken[index].1.clone()) }),
            None => taken.push((stamp, key, wish)),
        }
    }
    (taken.into_iter().map(|(_, _, wish)| wish).collect(), aside)
}

fn alive_points(points: &BTreeMap<u64, Held<AutomationPoint>>) -> Vec<AutomationPoint> {
    let mut alive: Vec<AutomationPoint> = points.values().filter(|held| held.alive).map(|held| held.it).collect();
    sort_points(&mut alive);
    alive
}

/// A clip with the notes and curves it has.
fn clip_of(held: &ClipHeld) -> Clip {
    let mut clip = held.held.it.clone();
    if let ClipContent::Midi(midi) = &mut clip.content {
        midi.notes = held.notes.values().filter(|note| note.alive).map(|note| note.it).collect();
        sort_notes(&mut midi.notes);
        midi.controllers = (held.curves.iter())
            .map(|((controller, channel), points)| ControllerCurve {
                controller: *controller,
                channel: *channel,
                points: alive_points(points),
            })
            .filter(|curve| !curve.points.is_empty())
            .collect();
    }
    clip
}

impl Doc {
    /// The song as it stands: see the [module](self).
    pub fn derive(&self) -> Derived {
        // The tracks that are there, in their order.
        let mut order: Vec<_> = self.tracks.values().filter(|track| track.held.alive).collect();
        order.sort_by(|a, b| a.place.total_cmp(&b.place).then(a.held.it.id.cmp(&b.held.it.id)));
        let index: HashMap<TrackId, usize> =
            order.iter().enumerate().map(|(index, track)| (track.held.it.id, index)).collect();
        let kind = |id: TrackId| index.get(&id).map(|index| order[*index].held.it.kind);
        let mut tracks: Vec<Track> = (order.iter())
            .map(|held| {
                let mut track = held.held.it.clone();
                // What plays through what is taken below.
                (track.group, track.reference) = (None, None);
                if track.kind != TrackKind::Normal {
                    track.profile = InstrumentProfile::default();
                }
                track
            })
            .collect();

        for held in self.clips.values().filter(|clip| clip.held.alive) {
            if kind(held.track) == Some(TrackKind::Normal) {
                tracks[index[&held.track]].clips.push(clip_of(held));
            }
        }
        for track in &mut tracks {
            sort_clips(&mut track.clips);
        }

        // The devices that are there, each listening to its own input
        // until what it listens to is taken.
        let mut devices: BTreeMap<DeviceId, (ChainRef, f64, Device)> = BTreeMap::new();
        for (id, held) in self.devices.iter().filter(|(_, device)| device.held.alive) {
            let there = match held.chain {
                ChainRef::Master => true,
                ChainRef::Track(track) => index.contains_key(&track),
            };
            if there {
                devices.insert(*id, (held.chain, held.place, Device { sidechain: None, ..held.held.it.clone() }));
            }
        }

        // What plays through what.
        let mut wishes: Vec<(Stamp, Key, Wish)> = Vec::new();
        let sends = |track: TrackId, to: TrackId| {
            kind(track).is_some_and(|kind| kind != TrackKind::Return) && kind(to) == Some(TrackKind::Return)
        };
        for held in &order {
            let (track, it) = (held.held.it.id, &held.held.it);
            if let Some(group) = it.group
                && it.kind == TrackKind::Normal
                && kind(group) == Some(TrackKind::Group)
            {
                let key = Key::Track(track, group_prop());
                wishes.push((self.stamp(&key), key, Wish::Group { track, group }));
            }
            for to in held.sends.keys().filter(|to| sends(track, **to)) {
                let key = Key::Send(track, *to);
                wishes.push((self.stamp(&key), key, Wish::Send { track, to: *to }));
            }
            if it.reference.is_some() && it.kind == TrackKind::Normal {
                let key = Key::Track(track, REFERENCE);
                wishes.push((self.stamp(&key), key, Wish::Reference { track }));
            }
        }
        for (target, lane) in &self.lanes {
            if let AutomationTarget::Send { track, to } = target
                && sends(*track, *to)
                && lane.points.values().any(|point| point.alive)
            {
                // An envelope is as old as the saying of how it travels,
                // which is said when it is made.
                let key = Key::Lane(target.clone(), travel_prop());
                wishes.push((self.stamp(&key), key, Wish::SendLane { track: *track, to: *to }));
            }
        }
        for (id, held) in &self.devices {
            if let Some(to) = held.held.it.sidechain
                && let Some((chain, ..)) = devices.get(id)
                && kind(to).is_some()
                && *chain != ChainRef::Track(to)
            {
                let key = Key::Device(*id, sidechain_prop());
                wishes.push((self.stamp(&key), key, Wish::Listen { device: *id, chain: *chain, to }));
            }
        }
        let (taken, aside) = admit(wishes);
        let mut send_lanes: HashSet<(TrackId, TrackId)> = HashSet::new();
        for wish in taken {
            match wish {
                Wish::Group { track, group } => tracks[index[&track]].group = Some(group),
                Wish::Send { track, to } => {
                    let level_db = order[index[&track]].sends[&to];
                    tracks[index[&track]].sends.push(Send { to, level_db });
                }
                Wish::SendLane { track, to } => {
                    send_lanes.insert((track, to));
                }
                Wish::Listen { device, to, .. } => {
                    if let Some((.., device)) = devices.get_mut(&device) {
                        device.sidechain = Some(to);
                    }
                }
                Wish::Reference { track } => {
                    tracks[index[&track]].reference.clone_from(&order[index[&track]].held.it.reference);
                }
            }
        }
        for track in &mut tracks {
            track.sends.sort_by_key(|send| send.to);
        }

        // Envelopes of what is there, that have a point.
        let mut automation = Vec::new();
        for (target, lane) in &self.lanes {
            let there = match target {
                AutomationTarget::TrackVolume { track } | AutomationTarget::TrackPan { track } => {
                    index.contains_key(track)
                }
                AutomationTarget::DeviceParam { device, .. } => devices.contains_key(device),
                AutomationTarget::MasterVolume => true,
                AutomationTarget::Send { track, to } => send_lanes.contains(&(*track, *to)),
            };
            let points = alive_points(&lane.points);
            if there && !points.is_empty() {
                automation.push(AutomationLane {
                    target: target.clone(),
                    points,
                    travel: lane.travel,
                    enabled: lane.enabled,
                });
            }
        }

        // The devices into their chains, in their order.
        let mut master_devices: Vec<(f64, Device)> = Vec::new();
        let mut chains: Vec<Vec<(f64, Device)>> = tracks.iter().map(|_| Vec::new()).collect();
        for (chain, place, device) in devices.into_values() {
            match chain {
                ChainRef::Master => master_devices.push((place, device)),
                ChainRef::Track(track) => chains[index[&track]].push((place, device)),
            }
        }
        let in_order = |mut chain: Vec<(f64, Device)>| -> Vec<Device> {
            chain.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.id.cmp(&b.1.id)));
            chain.into_iter().map(|(_, device)| device).collect()
        };
        for (track, chain) in tracks.iter_mut().zip(chains) {
            track.devices = in_order(chain);
        }

        // What the tempo and the time signature do: one thing a beat.
        let mut tempo: Vec<(TempoChange, Stamp)> = (self.tempo.iter())
            .map(|((beat, ramp), tempo)| {
                let change = TempoChange { beat: f64::from_bits(*beat), tempo: *tempo, ramp: *ramp };
                (change, self.stamp(&Key::Tempo(*beat, *ramp)))
            })
            .filter(|(change, _)| change.beat.is_finite() && change.beat > SAME_BEAT && valid_tempo(change.tempo).is_ok())
            .collect();
        tempo.sort_by(|a, b| a.0.ramp.cmp(&b.0.ramp).then(a.0.beat.total_cmp(&b.0.beat)));
        let mut tempo_changes: Vec<(TempoChange, Stamp)> = Vec::new();
        for found in tempo {
            match tempo_changes.last_mut() {
                Some(last) if last.0.ramp == found.0.ramp && found.0.beat - last.0.beat <= SAME_BEAT => {
                    if found.1 > last.1 {
                        *last = found;
                    }
                }
                _ => tempo_changes.push(found),
            }
        }
        let mut tempo_changes: Vec<TempoChange> = tempo_changes.into_iter().map(|(change, _)| change).collect();
        sort_tempo_changes(&mut tempo_changes);
        let mut meter_changes: Vec<(MeterChange, Stamp)> = Vec::new();
        for (beat, signature) in &self.meter {
            let found = (MeterChange { beat: f64::from_bits(*beat), signature: *signature }, self.stamp(&Key::Meter(*beat)));
            let valid = found.0.beat.is_finite()
                && found.0.beat > SAME_BEAT
                && valid_signature(signature.numerator, signature.denominator).is_ok();
            match meter_changes.last_mut() {
                _ if !valid => {}
                Some(last) if found.0.beat - last.0.beat <= SAME_BEAT => {
                    if found.1 > last.1 {
                        *last = found;
                    }
                }
                _ => meter_changes.push(found),
            }
        }

        let mut markers: Vec<Marker> =
            self.markers.values().filter(|marker| marker.alive).map(|marker| marker.it.clone()).collect();
        markers.sort_by(|a, b| a.beat.total_cmp(&b.beat).then(a.id.cmp(&b.id)));

        let mut project = self.song.clone();
        project.format_version = FORMAT_VERSION;
        project.tempo_changes = tempo_changes;
        project.meter_changes = meter_changes.into_iter().map(|(change, _)| change).collect();
        project.tracks = tracks;
        project.master.devices = in_order(master_devices);
        project.automation = automation;
        project.markers = markers;
        Derived { project, aside }
    }

    /// What is held alive and is not in the song because the thing it is
    /// in was removed, among `things`: for telling someone whose work
    /// landed there.
    pub fn out_with(&self, thing: &Thing) -> Option<Thing> {
        let mut at = self.holder(thing);
        for _ in 0..4 {
            match at {
                Some(holder) if self.knows(&holder) && !self.is_alive(&holder) => return Some(holder),
                Some(holder) => at = self.holder(&holder),
                None => return None,
            }
        }
        None
    }

    /// The points of an envelope or a curve that are there.
    pub fn points_there(&self, home: &Home) -> usize {
        self.points(home).map_or(0, |points| points.values().filter(|point| point.alive).count())
    }
}
