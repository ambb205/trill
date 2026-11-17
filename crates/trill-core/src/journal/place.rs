//! Where in a song something was made or changed: a track, some clips, a
//! device, an envelope, a stretch of the timeline.
//!
//! A [`Place`] is what a step of the journal says about where it landed,
//! which is where a window goes to show it and where a camera looks while a
//! session is played again. Things are known by their ids, with the name
//! each went by at the time: a place read back another day only leads to
//! what is still the same thing.

use super::change::{Change, Op};
use crate::{AutomationTarget, Beats, ChainRef, ClipId, DeviceId, Project, TrackId};
use serde::{Deserialize, Serialize};

/// Something in the song that was made or changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "what", rename_all = "snake_case")]
pub enum Place {
    Track {
        id: u64,
        #[serde(default)]
        name: String,
    },
    Clips {
        /// Each with its name.
        clips: Vec<(u64, String)>,
    },
    Device {
        id: u64,
        #[serde(default)]
        name: String,
    },
    /// An automation envelope. For a device's parameter, `param` is as
    /// whoever named it gave it: its id, or what a tool took for it.
    Lane { target: AutomationTarget },
    /// A stretch of the timeline, in beats.
    Span { start: Beats, end: Beats },
}

impl Place {
    pub fn track(id: u64) -> Self {
        Place::Track { id, name: String::new() }
    }

    /// The clips `ids` name; `None` for none.
    pub fn clips(ids: impl IntoIterator<Item = u64>) -> Option<Self> {
        let clips: Vec<(u64, String)> = ids.into_iter().map(|id| (id, String::new())).collect();
        (!clips.is_empty()).then_some(Place::Clips { clips })
    }

    pub fn device(id: u64) -> Self {
        Place::Device { id, name: String::new() }
    }

    /// Whether what the place names is in `project`.
    pub fn is_there(&self, project: &Project) -> bool {
        match self {
            Place::Track { id, .. } => project.track(TrackId(*id)).is_ok(),
            Place::Clips { clips } => clips.iter().any(|(id, _)| project.clip(ClipId(*id)).is_ok()),
            Place::Device { id, .. } => project.device(DeviceId(*id)).is_ok(),
            Place::Lane { target } => lane(project, target).is_some(),
            Place::Span { .. } => true,
        }
    }

    /// The place as it stands in `project`: what is gone already is dropped
    /// from it, and what is there is given the name it goes by. `None` if
    /// nothing of it is left.
    pub fn settled(self, project: &Project) -> Option<Self> {
        match self {
            Place::Track { id, .. } => {
                project.track(TrackId(id)).ok().map(|track| Place::Track { id, name: track.name.clone() })
            }
            Place::Clips { clips } => {
                let there =
                    clips.into_iter().filter_map(|(id, _)| Some((id, project.clip(ClipId(id)).ok()?.name.clone())));
                let clips: Vec<(u64, String)> = there.collect();
                (!clips.is_empty()).then_some(Place::Clips { clips })
            }
            Place::Device { id, .. } => {
                project.device(DeviceId(id)).ok().map(|device| Place::Device { id, name: device.name.clone() })
            }
            Place::Lane { target } => lane(project, &target).map(|target| Place::Lane { target }),
            span @ Place::Span { .. } => Some(span),
        }
    }

    /// Whether the place still names the same things in `project`, which
    /// was opened another day: they are there, under the names they had.
    /// An id alone would not do: a project opened without what a turn made
    /// gives the same id to the next thing made in it.
    pub fn is_the_same_in(&self, project: &Project) -> bool {
        match self {
            Place::Track { id, name } => project.track(TrackId(*id)).is_ok_and(|track| track.name == *name),
            Place::Clips { clips } => {
                clips.iter().all(|(id, name)| project.clip(ClipId(*id)).is_ok_and(|clip| clip.name == *name))
            }
            Place::Device { id, name } => project.device(DeviceId(*id)).is_ok_and(|device| device.name == *name),
            Place::Lane { target } => lane(project, target).is_some(),
            Place::Span { .. } => true,
        }
    }
}

/// The envelope a target means: the one with exactly that target, or, for a
/// device's parameter that was given by another name than its id, the
/// device's envelope if it has just one.
pub fn lane(project: &Project, target: &AutomationTarget) -> Option<AutomationTarget> {
    let lanes = &project.automation;
    if lanes.iter().any(|lane| lane.target == *target) {
        return Some(target.clone());
    }
    let AutomationTarget::DeviceParam { device, param } = target else { return None };
    let of_device = |lane: &&crate::AutomationLane| matches!(&lane.target, AutomationTarget::DeviceParam { device: other, .. } if other == device);
    let named = lanes.iter().filter(of_device).find(|lane| {
        matches!(&lane.target, AutomationTarget::DeviceParam { param: other, .. } if other.eq_ignore_ascii_case(param))
    });
    let mut all = lanes.iter().filter(of_device);
    let only = all.next().filter(|_| all.next().is_none());
    named.or(only).map(|lane| lane.target.clone())
}

/// The track an envelope belongs to; `None` for the master's, and for one
/// whose track or device is gone.
fn lane_track(project: &Project, target: &AutomationTarget) -> Option<TrackId> {
    match target {
        AutomationTarget::TrackVolume { track }
        | AutomationTarget::TrackPan { track }
        | AutomationTarget::Send { track, .. } => Some(*track),
        AutomationTarget::DeviceParam { device, .. } => match project.device_chain(*device).ok()? {
            ChainRef::Track(track) => Some(track),
            ChainRef::Master => None,
        },
        AutomationTarget::MasterVolume => None,
    }
}

/// The most places a step is given when they are read from what it changed:
/// a step that touched everything is not about any one thing.
const MOST_PLACES: usize = 24;

/// Where a change landed in `after`, the song it made: the tracks whose
/// settings it changed, the clips and devices it made or changed, the
/// envelopes it wrote, each under the name it goes by there. What it
/// removed is nowhere.
pub fn of_change(change: &Change, after: &Project) -> Vec<Place> {
    let mut places: Vec<Place> = Vec::new();
    let mut clips: Vec<u64> = Vec::new();
    let add = |place: Place, places: &mut Vec<Place>| {
        if !places.contains(&place) {
            places.push(place);
        }
    };
    for op in change.ops() {
        match op {
            Op::Track(track) => add(Place::track(track.id.0), &mut places),
            Op::Clip { clip, .. } | Op::ClipSettings(clip) => clips.push(clip.id.0),
            Op::Notes { clip, .. } => clips.push(clip.0),
            Op::Device { device, .. } => add(Place::device(device.id.0), &mut places),
            Op::Lane(lane) => add(Place::Lane { target: lane.target.clone() }, &mut places),
            Op::Markers(markers) => {
                let beats = markers.iter().map(|marker| marker.beat);
                let (start, end) =
                    beats.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), beat| (low.min(beat), high.max(beat)));
                if start <= end {
                    add(Place::Span { start, end }, &mut places);
                }
            }
            Op::Song(_) | Op::Ids(_) | Op::Clips(_) | Op::Devices(_) | Op::Tracks(_) | Op::Lanes(_) => {}
        }
    }
    clips.dedup();
    let mut seen = std::collections::HashSet::new();
    clips.retain(|id| seen.insert(*id));
    places.extend(Place::clips(clips));
    places.truncate(MOST_PLACES);
    places.into_iter().filter_map(|place| place.settled(after)).collect()
}

/// Where the action is: the tracks and the stretch of time some places
/// cover in a song, which is what a view brings into sight and a camera
/// frames.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Focus {
    /// The tracks, in the order the places name them, each once.
    pub tracks: Vec<TrackId>,
    /// The stretch of time, in beats, if the places have one.
    pub span: Option<(Beats, Beats)>,
    /// The device among them that came last, if any.
    pub device: Option<DeviceId>,
    /// Whether any of it is on the master.
    pub master: bool,
}

impl Focus {
    /// What `places` cover in `project`. What is not there covers nothing.
    pub fn of<'a>(places: impl IntoIterator<Item = &'a Place>, project: &Project) -> Focus {
        let mut focus = Focus::default();
        for place in places {
            match place {
                Place::Track { id, .. } => {
                    if let Ok(track) = project.track(TrackId(*id)) {
                        focus.toward(track.id);
                    }
                }
                Place::Clips { clips } => {
                    for (id, _) in clips {
                        let id = ClipId(*id);
                        let (Ok(clip), Ok(track)) = (project.clip(id), project.clip_track(id)) else { continue };
                        focus.toward(track);
                        focus.cover(clip.start, clip.end());
                    }
                }
                Place::Device { id, .. } => match project.device_chain(DeviceId(*id)) {
                    Ok(ChainRef::Track(track)) => {
                        focus.toward(track);
                        focus.device = Some(DeviceId(*id));
                    }
                    Ok(ChainRef::Master) => {
                        focus.master = true;
                        focus.device = Some(DeviceId(*id));
                    }
                    Err(_) => {}
                },
                Place::Lane { target } => {
                    let Some(target) = lane(project, target) else { continue };
                    match lane_track(project, &target) {
                        Some(track) => focus.toward(track),
                        None => focus.master = true,
                    }
                    let points = project.automation.iter().find(|lane| lane.target == target).map(|lane| &lane.points);
                    if let Some((first, last)) = points.and_then(|points| Some((points.first()?, points.last()?))) {
                        focus.cover(first.beat, last.beat);
                    }
                }
                Place::Span { start, end } => focus.cover(*start, *end),
            }
        }
        focus
    }

    /// Whether the places led anywhere.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.span.is_none() && self.device.is_none() && !self.master
    }

    fn toward(&mut self, track: TrackId) {
        if !self.tracks.contains(&track) {
            self.tracks.push(track);
        }
    }

    fn cover(&mut self, start: Beats, end: Beats) {
        self.span = Some(match self.span {
            Some((low, high)) => (low.min(start), high.max(end)),
            None => (start, end),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Blobs;
    use crate::{AutomationPoint, Note};

    /// Two tracks with a clip each, a synth on the first, a limiter on the
    /// master.
    fn song() -> (Project, [TrackId; 2], [ClipId; 2], [DeviceId; 2]) {
        let mut project = Project::new("t");
        let lead = project.add_track("Lead");
        let bass = project.add_track("Bass");
        let synth = project.add_device(ChainRef::Track(lead), "builtin:polysynth", "Poly Synth", None).unwrap();
        let limiter = project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
        let hook = project.add_midi_clip(lead, "Hook", 16.0, 8.0).unwrap();
        let line = project.add_midi_clip(bass, "Line", 32.0, 16.0).unwrap();
        (project, [lead, bass], [hook, line], [synth, limiter])
    }

    fn clips(ids: &[ClipId]) -> Option<Place> {
        Place::clips(ids.iter().map(|id| id.0))
    }

    #[test]
    fn a_place_stands_for_what_is_there_under_its_name() {
        let (mut project, [lead, bass], [hook, line], [synth, _]) = song();
        let made = clips(&[hook, ClipId(999), line]).unwrap().settled(&project).unwrap();
        assert_eq!(
            made,
            Place::Clips { clips: vec![(hook.0, "Hook".into()), (line.0, "Line".into())] },
            "what is gone is dropped"
        );
        assert_eq!(clips(&[ClipId(999)]).unwrap().settled(&project), None);
        assert_eq!(Place::track(lead.0).settled(&project), Some(Place::Track { id: lead.0, name: "Lead".into() }));
        assert_eq!(Place::track(999).settled(&project), None);
        let device = Place::device(synth.0).settled(&project).unwrap();
        assert_eq!(device, Place::Device { id: synth.0, name: "Poly Synth".into() });

        // An envelope that was named by the parameter's name is the one
        // the device has.
        let cutoff = AutomationTarget::DeviceParam { device: synth, param: "cutoff".into() };
        project.set_automation(cutoff.clone(), vec![AutomationPoint::new(0.0, 500.0)]).unwrap();
        let by_name = AutomationTarget::DeviceParam { device: synth, param: "Cutoff".into() };
        assert_eq!(Place::Lane { target: by_name }.settled(&project), Some(Place::Lane { target: cutoff.clone() }));
        let none = AutomationTarget::TrackPan { track: bass };
        assert!(!Place::Lane { target: none }.is_there(&project));

        // Read back another day, it leads only to what still goes by the
        // name: an id alone could be anything made since.
        assert!(made.is_the_same_in(&project) && device.is_the_same_in(&project));
        project.clip_mut(line).unwrap().name = "Another".into();
        assert!(!made.is_the_same_in(&project));
        assert!(made.is_there(&project), "in the session it was made in, the id is enough");
        project.remove_track(lead).unwrap();
        assert!(!device.is_the_same_in(&project) && !device.is_there(&project));

        // And it is kept between runs as it is.
        let kept = serde_json::to_string(&made).unwrap();
        assert_eq!(serde_json::from_str::<Place>(&kept).unwrap(), made);
        let lane = Place::Lane { target: cutoff };
        assert_eq!(serde_json::from_str::<Place>(&serde_json::to_string(&lane).unwrap()).unwrap(), lane);
    }

    #[test]
    fn what_a_change_made_or_changed_is_where_it_landed() {
        let (before, [lead, bass], [hook, _], [synth, limiter]) = song();
        let mut blobs = Blobs::default();
        assert!(of_change(&Change::between(&before, &before, &mut blobs), &before).is_empty());

        let mut after = before.clone();
        let drums = after.add_track("Drums");
        let beat = after.add_midi_clip(drums, "Beat", 0.0, 4.0).unwrap();
        after.set_track_volume(bass, -6.0).unwrap();
        after.clip_mut(hook).unwrap().add_notes(&[Note { pitch: 60, start: 0.0, length: 1.0, velocity: 100 }]).unwrap();
        after.device_mut(synth).unwrap().params.insert("cutoff".into(), 900.0);
        after.device_mut(limiter).unwrap().enabled = false;
        let volume = AutomationTarget::TrackVolume { track: lead };
        after.set_automation(volume.clone(), vec![AutomationPoint::new(0.0, -3.0)]).unwrap();

        let places = of_change(&Change::between(&before, &after, &mut blobs), &after);
        assert_eq!(
            places,
            [
                Place::Track { id: bass.0, name: "Bass".into() },
                Place::Track { id: drums.0, name: "Drums".into() },
                Place::Device { id: synth.0, name: "Poly Synth".into() },
                Place::Device { id: limiter.0, name: "Limiter".into() },
                Place::Lane { target: volume },
                Place::Clips { clips: vec![(hook.0, "Hook".into()), (beat.0, "Beat".into())] },
            ]
        );

        // What was removed is nowhere.
        let mut gone = after.clone();
        gone.remove_clip(beat).unwrap();
        gone.remove_track(lead).unwrap();
        assert!(of_change(&Change::between(&after, &gone, &mut blobs), &gone).is_empty());
    }

    #[test]
    fn places_cover_tracks_and_a_stretch_of_time() {
        let (mut project, [lead, bass], [hook, line], [synth, limiter]) = song();
        let made = [clips(&[hook]).unwrap(), Place::track(bass.0), clips(&[line, ClipId(999)]).unwrap()];
        let focus = Focus::of(&made, &project);
        assert_eq!((focus.tracks.as_slice(), focus.span), (&[lead, bass][..], Some((16.0, 48.0))));
        assert!(!focus.master && focus.device.is_none());

        let focus = Focus::of(&[Place::device(synth.0), Place::device(limiter.0)], &project);
        assert_eq!((focus.tracks.as_slice(), focus.device, focus.master), (&[lead][..], Some(limiter), true));

        let pan = AutomationTarget::TrackPan { track: bass };
        project
            .set_automation(pan.clone(), vec![AutomationPoint::new(8.0, -1.0), AutomationPoint::new(24.0, 1.0)])
            .unwrap();
        let focus = Focus::of(&[Place::Lane { target: pan }, Place::Span { start: 64.0, end: 80.0 }], &project);
        assert_eq!((focus.tracks.as_slice(), focus.span), (&[bass][..], Some((8.0, 80.0))));

        let nowhere = Focus::of(&[Place::track(999), Place::device(999), clips(&[ClipId(999)]).unwrap()], &project);
        assert!(nowhere.is_empty());
    }
}
