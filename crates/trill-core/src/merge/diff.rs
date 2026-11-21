//! What an edit did, as the properties it set: found by comparing the song
//! before the edit and after, so that no edit is written a second time as
//! a message.
//!
//! Things are matched by their ids, never by where they lie in a list: a
//! note that was moved is that note with another start, not one note gone
//! and another come. What follows from something else that the edit did is
//! not said: when a track is removed, its clips and devices, the sends to
//! it and its envelopes go with it by themselves ([`Doc::derive`]), and
//! come back with it. Saying that they were removed would be saying that
//! they should stay gone.

use super::doc::Doc;
use super::props::{AudioSet, ClipSet, DeviceSet, LaneSet, MarkerSet, MidiSet, NoteSet, PointSet, SongSet, TrackSet};
use super::{Beside, ClipOp, DeviceOp, Home, MarkerOp, NoteOp, Op, Place, PointOp, Thing, TrackOp};
use crate::project::{
    AutomationPoint, AutomationTarget, ChainRef, Clip, ClipContent, Device, Note, Project, Track, same_clip, same_lane,
};
use crate::{ClipId, DeviceId, TrackId};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The places to give the things of a row so that they lie in `order`:
/// each thing with the place it has, or `None` for one that is new to the
/// row. Things that are in order among themselves keep their places, and
/// the others take places between their neighbours'. Returns the place of
/// every thing, in order; the ones that differ from what was given are the
/// ones to say.
fn places(order: &[(u64, Option<Place>)]) -> Vec<Place> {
    // The longest run of things that are in order by what they have.
    let before = |a: (u64, Place), b: (u64, Place)| a.1 < b.1 || (a.1 == b.1 && a.0 < b.0);
    let placed: Vec<(usize, (u64, Place))> = (order.iter().enumerate())
        .filter_map(|(index, (id, place))| place.filter(|place| place.is_finite()).map(|place| (index, (*id, place))))
        .collect();
    let mut keeps = vec![false; order.len()];
    if placed.windows(2).all(|pair| before(pair[0].1, pair[1].1)) {
        for (index, _) in &placed {
            keeps[*index] = true;
        }
    } else {
        // Few enough things in a row for the plain way.
        let mut longest: Vec<(usize, Option<usize>)> = Vec::with_capacity(placed.len());
        for (at, (_, item)) in placed.iter().enumerate() {
            let best =
                (0..at).filter(|earlier| before(placed[*earlier].1, *item)).max_by_key(|earlier| longest[*earlier].0);
            longest.push((best.map_or(1, |best| longest[best].0 + 1), best));
        }
        let mut at = (0..placed.len()).max_by_key(|at| longest[*at].0);
        while let Some(found) = at {
            keeps[placed[found].0] = true;
            at = longest[found].1;
        }
    }
    let mut given: Vec<Place> = Vec::with_capacity(order.len());
    for index in 0..order.len() {
        if keeps[index] {
            given.push(order[index].1.expect("a thing that keeps its place has one"));
            continue;
        }
        let after = given.last().copied();
        let ahead = (index + 1..order.len()).find(|later| keeps[*later]).and_then(|later| order[later].1);
        let place = match (after, ahead) {
            (None, None) => 1.0,
            (Some(after), None) => after.floor() + 1.0,
            (None, Some(ahead)) => ahead.ceil() - 1.0,
            (Some(after), Some(ahead)) => (after + ahead) / 2.0,
        };
        let fits = after.is_none_or(|after| after < place) && ahead.is_none_or(|ahead| place < ahead);
        if !fits || !place.is_finite() {
            // No room between two neighbours: the whole row is laid out anew.
            return (1..=order.len()).map(|place| place as Place).collect();
        }
        given.push(place);
    }
    given
}

fn points_differ(home: &Home, was: &[AutomationPoint], now: &[AutomationPoint], ops: &mut Vec<Op>) {
    if was == now && was.iter().map(|point| point.id).eq(now.iter().map(|point| point.id)) {
        return;
    }
    let old: HashMap<u64, &AutomationPoint> = was.iter().map(|point| (point.id, point)).collect();
    let mut made = Vec::new();
    for point in now {
        match old.get(&point.id) {
            Some(old) => {
                PointSet::differing(old, point, |set| ops.push(Op::Point(home.clone(), point.id, PointOp::Set(set))))
            }
            None => made.push(*point),
        }
    }
    if !made.is_empty() {
        ops.push(Op::Points(home.clone(), made));
    }
    let kept: HashSet<u64> = now.iter().map(|point| point.id).collect();
    for point in was.iter().filter(|point| !kept.contains(&point.id)) {
        ops.push(Op::Point(home.clone(), point.id, PointOp::Alive(false)));
    }
}

fn notes_differ(clip: ClipId, was: &[Note], now: &[Note], ops: &mut Vec<Op>) {
    if was == now && was.iter().map(|note| note.id).eq(now.iter().map(|note| note.id)) {
        return;
    }
    let old: HashMap<u64, &Note> = was.iter().map(|note| (note.id, note)).collect();
    let mut made = Vec::new();
    for note in now {
        match old.get(&note.id) {
            Some(old) => NoteSet::differing(old, note, |set| ops.push(Op::Note(clip, note.id, NoteOp::Set(set)))),
            None => made.push(*note),
        }
    }
    if !made.is_empty() {
        ops.push(Op::Notes(clip, made));
    }
    let kept: HashSet<u64> = now.iter().map(|note| note.id).collect();
    for note in was.iter().filter(|note| !kept.contains(&note.id)) {
        ops.push(Op::Note(clip, note.id, NoteOp::Alive(false)));
    }
}

fn clips_differ(was: &Clip, now: &Clip, ops: &mut Vec<Op>) {
    let id = now.id;
    ClipSet::differing(was, now, |set| ops.push(Op::Clip(id, ClipOp::Set(set))));
    match (&was.content, &now.content) {
        (ClipContent::Midi(old), ClipContent::Midi(new)) => {
            MidiSet::differing(old, new, |set| ops.push(Op::Clip(id, ClipOp::Midi(set))));
            notes_differ(id, &old.notes, &new.notes, ops);
            for curve in &new.controllers {
                let home = Home::Curve(id, curve.channel, curve.controller);
                let before = old.controller_on(curve.channel, curve.controller).map_or(&[][..], |old| &old.points);
                points_differ(&home, before, &curve.points, ops);
            }
            for curve in &old.controllers {
                if new.controller_on(curve.channel, curve.controller).is_none() {
                    points_differ(&Home::Curve(id, curve.channel, curve.controller), &curve.points, &[], ops);
                }
            }
        }
        (ClipContent::Audio(old), ClipContent::Audio(new)) => {
            AudioSet::differing(old, new, |set| ops.push(Op::Clip(id, ClipOp::Audio(set))));
        }
        // Never by an edit: a clip that came to hold the other kind of
        // thing is made again.
        _ => {}
    }
}

/// What turned `before` into `after`, as the properties to set. `doc` is
/// the song as it is held, which says where the things of a row lie.
/// Both projects are settled ([`Project::settle`]).
pub fn diff(before: &Project, after: &Project, doc: &Doc) -> Vec<Op> {
    let mut ops = Vec::new();

    // ---- the song itself ---------------------------------------------------
    SongSet::differing(before, after, |set| ops.push(Op::Song(set)));
    if before.master.volume_db != after.master.volume_db {
        ops.push(Op::MasterVolume(after.master.volume_db));
    }
    if before.tempo_changes != after.tempo_changes {
        let slot = |change: &crate::TempoChange| (change.beat.to_bits(), change.ramp);
        let now: BTreeMap<(u64, bool), f64> =
            after.tempo_changes.iter().map(|change| (slot(change), change.tempo)).collect();
        let was: BTreeMap<(u64, bool), f64> =
            before.tempo_changes.iter().map(|change| (slot(change), change.tempo)).collect();
        for ((beat, ramp), _) in was.iter().filter(|(slot, _)| !now.contains_key(slot)) {
            ops.push(Op::Tempo(f64::from_bits(*beat), *ramp, None));
        }
        for ((beat, ramp), tempo) in now.iter().filter(|(slot, tempo)| was.get(slot) != Some(tempo)) {
            ops.push(Op::Tempo(f64::from_bits(*beat), *ramp, Some(*tempo)));
        }
    }
    if before.meter_changes != after.meter_changes {
        let now: BTreeMap<u64, _> =
            after.meter_changes.iter().map(|change| (change.beat.to_bits(), change.signature)).collect();
        let was: BTreeMap<u64, _> =
            before.meter_changes.iter().map(|change| (change.beat.to_bits(), change.signature)).collect();
        for beat in was.keys().filter(|beat| !now.contains_key(beat)) {
            ops.push(Op::Meter(f64::from_bits(*beat), None));
        }
        for (beat, signature) in now.iter().filter(|(beat, signature)| was.get(beat) != Some(signature)) {
            ops.push(Op::Meter(f64::from_bits(*beat), Some(*signature)));
        }
    }
    let same_markers = before.markers == after.markers
        && before.markers.iter().map(|marker| marker.id).eq(after.markers.iter().map(|marker| marker.id));
    if !same_markers {
        let was: HashMap<u64, &crate::Marker> = before.markers.iter().map(|marker| (marker.id, marker)).collect();
        for marker in &after.markers {
            match was.get(&marker.id) {
                Some(old) => {
                    MarkerSet::differing(old, marker, |set| ops.push(Op::Marker(marker.id, MarkerOp::Set(set))));
                }
                None => ops.push(Op::Marker(marker.id, MarkerOp::Whole(marker.clone()))),
            }
        }
        let now: HashSet<u64> = after.markers.iter().map(|marker| marker.id).collect();
        for marker in before.markers.iter().filter(|marker| !now.contains(&marker.id)) {
            ops.push(Op::Marker(marker.id, MarkerOp::Alive(false)));
        }
    }

    // ---- tracks ------------------------------------------------------------
    let tracks_now: HashSet<TrackId> = after.tracks.iter().map(|track| track.id).collect();
    let was_track = |index: usize, id: TrackId| -> Option<&Track> {
        let there = before.tracks.get(index).filter(|track| track.id == id);
        there.or_else(|| before.tracks.iter().find(|track| track.id == id))
    };
    let same_order = before.tracks.iter().map(|track| track.id).eq(after.tracks.iter().map(|track| track.id));
    let track_places = match same_order {
        true => Vec::new(),
        false => {
            let order: Vec<(u64, Option<Place>)> = (after.tracks.iter().enumerate())
                .map(|(index, track)| {
                    let had = was_track(index, track.id).and_then(|_| doc.place_of_track(track.id));
                    (track.id.0, had)
                })
                .collect();
            places(&order).into_iter().zip(order).map(|(place, (_, had))| (place, had)).collect()
        }
    };
    for (index, track) in after.tracks.iter().enumerate() {
        let id = track.id;
        let placed = track_places.get(index).copied();
        let Some(old) = was_track(index, id) else {
            // New: made whole, without what it holds, which follows.
            let place =
                placed.map_or_else(|| doc.place_of_track(id).unwrap_or((index + 1) as Place), |(place, _)| place);
            let whole = Track { clips: Vec::new(), devices: Vec::new(), ..track.clone() };
            ops.push(Op::Track(id, TrackOp::Whole(Box::new(whole), place)));
            continue;
        };
        if let Some((place, had)) = placed
            && had != Some(place)
        {
            ops.push(Op::Track(id, TrackOp::Place(place)));
        }
        TrackSet::differing(old, track, |set| {
            // A group that went takes its members' grouping with it.
            let implied =
                matches!((&set, old.group), (TrackSet::Group(None), Some(group)) if !tracks_now.contains(&group));
            if !implied {
                ops.push(Op::Track(id, TrackOp::Set(set)));
            }
        });
        let study = |track: &Track| track.reference.as_ref().map(|reference| Beside { study: reference.study.clone() });
        if study(old) != study(track) {
            ops.push(Op::Track(id, TrackOp::Reference(study(track))));
        }
        let listening = |track: &Track| track.reference.as_ref().is_some_and(|reference| reference.listening);
        if listening(old) != listening(track) {
            ops.push(Op::Track(id, TrackOp::Listening(listening(track))));
        }
        if old.sends != track.sends {
            for send in &old.sends {
                // A return that went takes the sends to it with it.
                if !track.sends.iter().any(|now| now.to == send.to) && tracks_now.contains(&send.to) {
                    ops.push(Op::Send(id, send.to, None));
                }
            }
            for send in &track.sends {
                if !old.sends.contains(send) {
                    ops.push(Op::Send(id, send.to, Some(send.level_db)));
                }
            }
        }
    }
    for track in before.tracks.iter().filter(|track| !tracks_now.contains(&track.id)) {
        ops.push(Thing::Track(track.id).set_alive(false));
    }

    // ---- clips -------------------------------------------------------------
    // One that is not where it was on its track may have come from
    // another, which is looked up among all of them, once.
    let mut everywhere: Option<HashMap<ClipId, (TrackId, &Clip)>> = None;
    let mut clips_now: Option<HashSet<ClipId>> = None;
    for (index, track) in after.tracks.iter().enumerate() {
        let old_track = was_track(index, track.id);
        let was_clips: &[Clip] = old_track.map_or(&[], |was| &was.clips);
        if was_clips.len() == track.clips.len() && was_clips.iter().zip(&track.clips).all(|(a, b)| same_clip(a, b)) {
            continue;
        }
        for (place, clip) in track.clips.iter().enumerate() {
            let there = was_clips.get(place).filter(|was| was.id == clip.id);
            let here = there.or_else(|| was_clips.iter().find(|was| was.id == clip.id));
            let old = here.map(|old| (track.id, old)).or_else(|| {
                let all = everywhere.get_or_insert_with(|| {
                    let mut all = HashMap::new();
                    for track in &before.tracks {
                        for clip in &track.clips {
                            all.insert(clip.id, (track.id, clip));
                        }
                    }
                    all
                });
                all.get(&clip.id).copied()
            });
            let same_kind = |old: &Clip| {
                matches!(
                    (&old.content, &clip.content),
                    (ClipContent::Midi(_), ClipContent::Midi(_)) | (ClipContent::Audio(_), ClipContent::Audio(_))
                )
            };
            match old {
                Some((on, old)) if same_kind(old) => {
                    if on != track.id {
                        ops.push(Op::Clip(clip.id, ClipOp::Track(track.id)));
                    }
                    if !same_clip(old, clip) {
                        clips_differ(old, clip, &mut ops);
                    }
                }
                _ => ops.push(Op::Clip(clip.id, ClipOp::Whole(Box::new(clip.clone()), track.id))),
            }
        }
        // The ones that are gone from this track, if they are gone from
        // the song.
        for old in was_clips {
            if track.clips.iter().any(|clip| clip.id == old.id) {
                continue;
            }
            let now = clips_now.get_or_insert_with(|| {
                after.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.id).collect()
            });
            if !now.contains(&old.id) {
                ops.push(Thing::Clip(old.id).set_alive(false));
            }
        }
    }
    // (The clips of a track that went, went with it.)

    // ---- devices -----------------------------------------------------------
    let devices_now: HashSet<DeviceId> = after.devices().map(|device| device.id).collect();
    let was_device: HashMap<DeviceId, (ChainRef, &Device)> = (before.tracks.iter())
        .flat_map(|track| track.devices.iter().map(|device| (device.id, (ChainRef::Track(track.id), device))))
        .chain(before.master.devices.iter().map(|device| (device.id, (ChainRef::Master, device))))
        .collect();
    let chains = (after.tracks.iter().enumerate()).map(|(index, track)| {
        let was: &[Device] = was_track(index, track.id).map_or(&[], |was| &was.devices);
        (ChainRef::Track(track.id), &track.devices[..], was)
    });
    for (chain, devices, was) in
        chains.chain([(ChainRef::Master, &after.master.devices[..], &before.master.devices[..])])
    {
        if was == devices {
            continue;
        }
        let same_order = was.iter().map(|device| device.id).eq(devices.iter().map(|device| device.id));
        let chain_places = match same_order {
            true => Vec::new(),
            false => {
                let order: Vec<(u64, Option<Place>)> = (devices.iter())
                    .map(|device| {
                        let had = was.iter().any(|old| old.id == device.id).then(|| doc.place_of_device(device.id));
                        (device.id.0, had.flatten().filter(|(held, _)| *held == chain).map(|(_, place)| place))
                    })
                    .collect();
                places(&order).into_iter().zip(order).map(|(place, (_, had))| (place, had)).collect()
            }
        };
        for (index, device) in devices.iter().enumerate() {
            let id = device.id;
            let placed: Option<(Place, Option<Place>)> = chain_places.get(index).copied();
            let Some((_, old)) = was_device.get(&id).filter(|(_, old)| old.uri == device.uri) else {
                let place = placed.map_or((index + 1) as Place, |(place, _)| place);
                ops.push(Op::Device(id, DeviceOp::Whole(Box::new(device.clone()), chain, place)));
                continue;
            };
            if let Some((place, had)) = placed
                && had != Some(place)
            {
                ops.push(Op::Device(id, DeviceOp::At(chain, place)));
            }
            if *old == device {
                continue;
            }
            DeviceSet::differing(old, device, |set| {
                // A track that went takes the listening to it with it.
                let implied = matches!((&set, old.sidechain), (DeviceSet::Sidechain(None), Some(to)) if !tracks_now.contains(&to));
                if !implied {
                    ops.push(Op::Device(id, DeviceOp::Set(set)));
                }
            });
            if old.params != device.params {
                for name in old.params.keys().filter(|name| !device.params.contains_key(*name)) {
                    ops.push(Op::Param(id, name.clone(), None));
                }
                for (name, value) in device.params.iter().filter(|(name, value)| old.params.get(*name) != Some(value)) {
                    ops.push(Op::Param(id, name.clone(), Some(*value)));
                }
            }
        }
        // The ones that are gone from this chain, if they are gone from
        // the song.
        for old in was.iter().filter(|old| !devices_now.contains(&old.id)) {
            ops.push(Thing::Device(old.id).set_alive(false));
        }
    }

    // ---- envelopes ---------------------------------------------------------
    let same_lanes = before.automation.len() == after.automation.len()
        && before.automation.iter().zip(&after.automation).all(|(a, b)| same_lane(a, b));
    if !same_lanes {
        for lane in &after.automation {
            let home = Home::Lane(lane.target.clone());
            match before.automation.iter().find(|was| was.target == lane.target) {
                Some(old) => {
                    LaneSet::differing(old, lane, |set| ops.push(Op::Lane(lane.target.clone(), set)));
                    points_differ(&home, &old.points, &lane.points, &mut ops);
                }
                None => {
                    // New: how it travels is said even where that is as
                    // an envelope does by itself, because it says when
                    // the envelope was made.
                    ops.push(Op::Lane(lane.target.clone(), LaneSet::Travel(lane.travel)));
                    ops.push(Op::Lane(lane.target.clone(), LaneSet::Enabled(lane.enabled)));
                    // One that was there once may have points held that
                    // were removed: these are others.
                    ops.push(Op::Points(home, lane.points.clone()));
                }
            }
        }
        for old in &before.automation {
            if after.automation.iter().any(|lane| lane.target == old.target) {
                continue;
            }
            // An envelope goes with what it moves.
            let there = match &old.target {
                AutomationTarget::TrackVolume { track } | AutomationTarget::TrackPan { track } => {
                    tracks_now.contains(track)
                }
                AutomationTarget::DeviceParam { device, .. } => devices_now.contains(device),
                AutomationTarget::MasterVolume => true,
                AutomationTarget::Send { track, to } => tracks_now.contains(track) && tracks_now.contains(to),
            };
            if there {
                points_differ(&Home::Lane(old.target.clone()), &old.points, &[], &mut ops);
            }
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::places;

    #[test]
    fn a_thing_that_is_moved_takes_a_place_between_its_neighbours_and_the_others_keep_theirs() {
        // Nothing moved.
        assert_eq!(places(&[(1, Some(1.0)), (2, Some(2.0)), (3, Some(3.0))]), [1.0, 2.0, 3.0]);
        // The last to the front.
        assert_eq!(places(&[(3, Some(3.0)), (1, Some(1.0)), (2, Some(2.0))]), [0.0, 1.0, 2.0]);
        // The first to the end, and a new one in the middle.
        assert_eq!(places(&[(2, Some(2.0)), (9, None), (3, Some(3.0)), (1, Some(1.0))]), [2.0, 2.5, 3.0, 4.0]);
        // New ones at either end and into nothing.
        assert_eq!(places(&[(9, None), (1, Some(1.0)), (8, None)]), [0.0, 1.0, 2.0]);
        assert_eq!(places(&[(7, None), (8, None)]), [1.0, 2.0]);
        // Two that have one place are in order by which is which.
        assert_eq!(places(&[(1, Some(1.0)), (2, Some(1.0))]), [1.0, 1.0]);
        // And where there is no room between two, the row is laid out anew.
        assert_eq!(places(&[(1, Some(1.0)), (9, None), (2, Some(1.0))]), [1.0, 2.0, 3.0]);
        let mut tight = 1.0f64;
        let mut order = vec![(1, Some(0.0)), (2, Some(tight))];
        for id in 3..2000 {
            let given = places(&[order[0], (id, None), order[1]]);
            if given == [1.0, 2.0, 3.0] {
                return;
            }
            tight = given[1];
            order[1] = (id, Some(tight));
        }
        panic!("a place was found between two neighbours two thousand times over");
    }
}
