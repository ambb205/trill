//! Where in the song the assistant left its mark, and how the window goes
//! there.
//!
//! A turn changes the song through Trill's tools, and each call says what it
//! touched: in its arguments (`update_clip {"clip": 7}`) or in its answer
//! (`create_track` answers `{"track_id": 9}`). [`of_call`] reads that into a
//! [`Place`]: a track, some clips, a device, an automation lane, a stretch
//! of the timeline. Nothing is asked of the server for it; where the calls
//! of a turn name nothing (a tool this table does not know), the song as it
//! was before the turn is compared with the song after ([`between`]).
//!
//! [`Route::to`] then works out, against the song as it is now, what to
//! select and what to bring into view for some places, and [`Route::take`]
//! does it: the clips selected, their track selected, the Clip tab open on
//! the last of them (or the Devices tab on a device, a lane unfolded for an
//! envelope), and the arrangement asked to scroll, and zoom if it must, to
//! where they are. A place whose thing is gone leads nowhere.
//!
//! When this is done is the panel's business (`super`): once, when a turn
//! has ended and the user is not at work elsewhere, and whenever a row of
//! the conversation is clicked.

use crate::ui::DetailTab;
use crate::ui::arrangement::Reveal;
use crate::ui::state::{LaneOwner, UiState, lane_owner};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use trill_core::{AutomationTarget, Beats, ChainRef, ClipId, DeviceId, Project, TrackId};

/// Something in the song a tool call made or changed. Things are known by
/// the ids the tools know them by, with the name each went by when the turn
/// ended: a conversation read back another day only leads to what is still
/// the same thing.
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
    /// An automation envelope. For a device's parameter, `param` is as the
    /// call gave it: its id, or what the tool took for it.
    Lane { target: AutomationTarget },
    /// A stretch of the timeline, in beats.
    Span { start: Beats, end: Beats },
}

impl Place {
    fn track(id: u64) -> Self {
        Place::Track { id, name: String::new() }
    }

    fn clips(ids: impl IntoIterator<Item = u64>) -> Option<Self> {
        let clips: Vec<(u64, String)> = ids.into_iter().map(|id| (id, String::new())).collect();
        (!clips.is_empty()).then_some(Place::Clips { clips })
    }

    fn device(id: u64) -> Self {
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

    /// The place as it stands in `project` when its turn ends: what is gone
    /// already is dropped from it, and what is there is given the name it
    /// goes by. `None` if nothing of it is left.
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

/// The envelope a call's target means: the one with exactly that target,
/// or, for a device's parameter the tool was given by another name than
/// its id, the device's envelope if it has just one.
fn lane(project: &Project, target: &AutomationTarget) -> Option<AutomationTarget> {
    let lanes = &project.automation;
    if lanes.iter().any(|lane| lane.target == *target) {
        return Some(target.clone());
    }
    let AutomationTarget::DeviceParam { device, param } = target else { return None };
    let of_device = |lane: &&trill_core::project::AutomationLane| matches!(&lane.target, AutomationTarget::DeviceParam { device: other, .. } if other == device);
    let named = lanes.iter().filter(of_device).find(|lane| {
        matches!(&lane.target, AutomationTarget::DeviceParam { param: other, .. } if other.eq_ignore_ascii_case(param))
    });
    let mut all = lanes.iter().filter(of_device);
    let only = all.next().filter(|_| all.next().is_none());
    named.or(only).map(|lane| lane.target.clone())
}

/// The automation target a call's `target` argument names.
fn target_of(target: &Value) -> Option<AutomationTarget> {
    let id = |key: &str| target.get(key).and_then(Value::as_u64);
    match target.get("type")?.as_str()? {
        "track_volume" => Some(AutomationTarget::TrackVolume { track: TrackId(id("track")?) }),
        "track_pan" => Some(AutomationTarget::TrackPan { track: TrackId(id("track")?) }),
        "master_volume" => Some(AutomationTarget::MasterVolume),
        "device_param" => Some(AutomationTarget::DeviceParam {
            device: DeviceId(id("device")?),
            param: target.get("param")?.as_str()?.to_owned(),
        }),
        _ => None,
    }
}

/// What a call to `tool` with `input` that answered `answer` made or
/// changed, if it says. A call that only read, that removed something, or
/// that this table does not know, names nothing.
pub fn of_call(tool: &str, input: &Value, answer: &str) -> Option<Place> {
    let answer: Value = serde_json::from_str(answer).unwrap_or(Value::Null);
    let given = |key: &str| input.get(key).and_then(Value::as_u64);
    let answered = |key: &str| answer.get(key).and_then(Value::as_u64);
    match tool {
        "create_track" => answered("track_id").map(Place::track),
        "update_track" | "set_instrument_profile" => given("track").map(Place::track),

        "add_device" => answered("device_id").map(Place::device),
        "update_device" | "set_sample" | "show_device_editor" | "save_preset" => given("device").map(Place::device),

        "create_clip" | "create_audio_clip" => Place::clips(answered("clip_id")),
        "update_clip" | "add_notes" | "set_notes" | "remove_notes" | "transform_notes" => Place::clips(given("clip")),
        "write_chords" | "write_drums" | "write_arpeggio" | "write_bass" | "write_melody" => {
            Place::clips(answered("clip_id"))
        }
        "split_clip" => {
            let half = |key: &str| answer.get(key)?.get("clip_id")?.as_u64();
            Place::clips(half("first").into_iter().chain(half("second")))
        }
        "duplicate_clip" => {
            let copies = answer.get("copies").and_then(Value::as_array);
            Place::clips(copies.into_iter().flatten().filter_map(|copy| copy.get("clip_id")?.as_u64()))
        }
        "import_midi" => {
            let parts = answer.get("tracks").and_then(Value::as_array);
            Place::clips(parts.into_iter().flatten().filter_map(|part| part.get("clip_id")?.as_u64()))
        }

        "set_automation" => {
            let cleared = answer.get("cleared").and_then(Value::as_bool) == Some(true);
            input.get("target").and_then(target_of).filter(|_| !cleared).map(|target| Place::Lane { target })
        }
        "set_markers" => {
            let beats = input
                .get("markers")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(|marker| marker.get("beat")?.as_f64());
            let (start, end) =
                beats.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), beat| (low.min(beat), high.max(beat)));
            (start <= end).then_some(Place::Span { start, end })
        }
        "edit_time" => {
            let number = |key: &str| input.get(key).and_then(Value::as_f64);
            let start = number("start")?;
            let length = match (input.get("action")?.as_str()?, number("end"), number("length")) {
                // What was there is gone: the cut is all there is to see.
                ("delete", ..) => 0.0,
                // The stretch and its repeat.
                ("duplicate", Some(end), _) => 2.0 * (end - start),
                ("insert", _, Some(length)) => length,
                // The copy, where it was put.
                ("copy", Some(end), _) => {
                    let to = number("to")?;
                    return Some(Place::Span { start: to, end: to + (end - start).max(0.0) });
                }
                _ => return None,
            };
            Some(Place::Span { start, end: start + length.max(0.0) })
        }
        "set_project" => {
            let region = input.get("loop")?;
            let (start, end) = (region.get("start")?.as_f64()?, region.get("end")?.as_f64()?);
            (end > start).then_some(Place::Span { start, end })
        }
        _ => None,
    }
}

/// What differs between the song as it was and as it is, for a turn whose
/// calls did not say: the clips, devices and tracks that are new or not as
/// they were, and envelopes likewise. It cannot tell the assistant's
/// changes from ones the user made meanwhile, which is why the calls are
/// asked first.
pub fn between(before: &Project, after: &Project) -> Vec<Place> {
    let mut places = Vec::new();
    let mut clips = Vec::new();
    for track in &after.tracks {
        let Ok(was) = before.track(track.id) else {
            places.push(Place::track(track.id.0));
            clips.extend(track.clips.iter().map(|clip| clip.id.0));
            continue;
        };
        let mixed = (was.volume_db, was.pan, was.mute, was.solo, &was.name, was.group, &was.sends)
            != (track.volume_db, track.pan, track.mute, track.solo, &track.name, track.group, &track.sends);
        if mixed {
            places.push(Place::track(track.id.0));
        }
        let changed = |device: &&trill_core::project::Device| !was.devices.contains(device);
        places.extend(track.devices.iter().filter(changed).map(|device| Place::device(device.id.0)));
        // A clip moved here from another track is found there, as it was.
        let same = |clip: &&trill_core::Clip| before.clip(clip.id).is_ok_and(|was| was == *clip);
        clips.extend(track.clips.iter().filter(|clip| !same(clip)).map(|clip| clip.id.0));
    }
    let changed = |device: &&trill_core::project::Device| !before.master.devices.contains(device);
    places.extend(after.master.devices.iter().filter(changed).map(|device| Place::device(device.id.0)));
    places.extend(Place::clips(clips));
    let lanes = after.automation.iter().filter(|lane| !before.automation.contains(lane));
    places.extend(lanes.map(|lane| Place::Lane { target: lane.target.clone() }));
    places
}

/// What to select and what to bring into view for some places.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Route {
    /// The track to select; `Some(None)` is the master.
    owner: Option<Option<TrackId>>,
    clips: Vec<ClipId>,
    device: Option<DeviceId>,
    lanes: Vec<(LaneOwner, AutomationTarget)>,
    tab: Option<DetailTab>,
    /// The tracks to scroll to and the time to show.
    reveal: Reveal,
}

impl Route {
    /// The way to `places` in `project`, in the order they were made.
    /// `None` if none of them is there any more.
    ///
    /// Everything they name is selected and asked into view. Which tab is
    /// open goes by the last clip or device among them, whichever came
    /// later, and the track that is selected is then that one's, so that
    /// the tab and the selection are about the same thing; with neither, it
    /// is the track of the last place.
    pub fn to<'a>(places: impl IntoIterator<Item = &'a Place>, project: &Project) -> Option<Self> {
        let mut route = Route::default();
        let mut found = false;
        // The track of the last clip, and of the last device (the master
        // is `None`).
        let (mut of_clip, mut of_device) = (None, None);
        for place in places {
            match place {
                Place::Track { id, .. } => {
                    let Ok(track) = project.track(TrackId(*id)) else { continue };
                    route.toward(Some(track.id));
                }
                Place::Clips { clips } => {
                    let mut any = false;
                    for (id, _) in clips {
                        let id = ClipId(*id);
                        let (Ok(clip), Ok(track)) = (project.clip(id), project.clip_track(id)) else { continue };
                        if !route.clips.contains(&id) {
                            route.clips.push(id);
                        }
                        route.toward(Some(track));
                        route.span(clip.start, clip.end());
                        of_clip = Some(Some(track));
                        any = true;
                    }
                    if !any {
                        continue;
                    }
                    route.tab = Some(DetailTab::Clip);
                }
                Place::Device { id, .. } => {
                    let id = DeviceId(*id);
                    let Ok(chain) = project.device_chain(id) else { continue };
                    let owner = match chain {
                        ChainRef::Track(track) => Some(track),
                        ChainRef::Master => None,
                    };
                    route.toward(owner);
                    of_device = Some(owner);
                    route.device = Some(id);
                    route.tab = Some(DetailTab::Devices);
                }
                Place::Lane { target } => {
                    let Some(target) = lane(project, target) else { continue };
                    let Some(owner) = lane_owner(project, &target) else { continue };
                    route.toward(match owner {
                        LaneOwner::Track(track) => Some(track),
                        LaneOwner::Master => None,
                    });
                    let points = project.automation.iter().find(|lane| lane.target == target).map(|lane| &lane.points);
                    if let Some((first, last)) = points.and_then(|points| Some((points.first()?, points.last()?))) {
                        route.span(first.beat, last.beat);
                    }
                    if !route.lanes.iter().any(|(_, open)| *open == target) {
                        route.lanes.push((owner, target));
                    }
                }
                Place::Span { start, end } => route.span(*start, *end),
            }
            found = true;
        }
        match route.tab {
            Some(DetailTab::Devices) => route.owner = of_device,
            Some(DetailTab::Clip) => {
                route.owner = of_clip;
                // The tab is not the devices': which of them is chosen
                // there is left as it is.
                route.device = None;
            }
            _ => {}
        }
        found.then_some(route)
    }

    fn toward(&mut self, owner: Option<TrackId>) {
        self.owner = Some(owner);
        if let Some(track) = owner
            && !self.reveal.tracks.contains(&track)
        {
            self.reveal.tracks.push(track);
        }
    }

    fn span(&mut self, start: Beats, end: Beats) {
        self.reveal.span = Some(match self.reveal.span {
            Some((low, high)) => (low.min(start), high.max(end)),
            None => (start, end),
        });
    }

    /// Goes there: selects what the route leads to, opens the tab it is
    /// shown in, and asks the arrangement to bring it into view.
    pub fn take(self, state: &mut UiState) {
        match self.owner {
            Some(Some(track)) => state.select_track(track),
            Some(None) => state.select_master(),
            None => {}
        }
        if !self.clips.is_empty() {
            state.select_clips(self.clips);
            state.arrangement.time_selection = None;
        }
        if let Some(device) = self.device {
            state.selected_device = Some(device);
            state.reveal_device = Some(device);
        }
        if let Some(tab) = self.tab {
            state.detail_tab = tab;
        }
        for (owner, target) in self.lanes {
            state.arrangement.open_lane(owner, target.clone());
            state.arrangement.reveal = Some(target);
        }
        state.arrangement.show = Some(self.reveal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trill_core::{AutomationPoint, Note};

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
    fn a_call_says_what_it_made_or_changed() {
        let of = |tool: &str, input: Value, answer: Value| of_call(tool, &input, &answer.to_string());
        // What was made is named in the answer.
        assert_eq!(
            of("create_track", json!({ "name": "Drums" }), json!({ "track_id": 9, "instrument_device_id": 10 })),
            Some(Place::track(9))
        );
        assert_eq!(
            of("add_device", json!({ "track": 2, "uri": "builtin:reverb" }), json!({ "device_id": 12 })),
            Some(Place::device(12))
        );
        assert_eq!(
            of("create_clip", json!({ "track": 2 }), json!({ "clip_id": 14, "note_count": 0 })),
            clips(&[ClipId(14)])
        );
        assert_eq!(of("create_audio_clip", json!({ "track": 2 }), json!({ "clip_id": 15 })), clips(&[ClipId(15)]));
        let written = json!({ "clip_id": 58, "track": 26, "created": true, "bars": 8 });
        for tool in ["write_chords", "write_drums", "write_arpeggio", "write_bass", "write_melody"] {
            assert_eq!(of(tool, json!({ "target": { "track": 26 } }), written.clone()), clips(&[ClipId(58)]), "{tool}");
        }
        assert_eq!(
            of(
                "split_clip",
                json!({ "clip": 3, "at": 8 }),
                json!({ "first": { "clip_id": 3 }, "second": { "clip_id": 20 } })
            ),
            clips(&[ClipId(3), ClipId(20)])
        );
        assert_eq!(
            of("duplicate_clip", json!({ "clip": 3 }), json!({ "copies": [{ "clip_id": 21 }, { "clip_id": 22 }] })),
            clips(&[ClipId(21), ClipId(22)])
        );
        assert_eq!(
            of("import_midi", json!({ "path": "a.mid" }), json!({ "tracks": [{ "track_id": 30, "clip_id": 31 }] })),
            clips(&[ClipId(31)])
        );

        // What was changed is named in the arguments.
        assert_eq!(of("update_track", json!({ "track": 4, "volume_db": -6 }), json!({})), Some(Place::track(4)));
        assert_eq!(of("update_device", json!({ "device": 5, "params": {} }), json!({})), Some(Place::device(5)));
        assert_eq!(of("set_sample", json!({ "device": 5, "path": "k.wav" }), json!({})), Some(Place::device(5)));
        for tool in ["update_clip", "add_notes", "set_notes", "remove_notes", "transform_notes"] {
            assert_eq!(of(tool, json!({ "clip": 7 }), json!({})), clips(&[ClipId(7)]), "{tool}");
        }
        let target = json!({ "type": "device_param", "device": 5, "param": "cutoff" });
        assert_eq!(
            of(
                "set_automation",
                json!({ "target": target, "points": [[0, 1]] }),
                json!({ "point_count": 1, "cleared": false })
            ),
            Some(Place::Lane { target: AutomationTarget::DeviceParam { device: DeviceId(5), param: "cutoff".into() } })
        );
        assert_eq!(
            of(
                "set_automation",
                json!({ "target": { "type": "track_volume", "track": 4 } }),
                json!({ "point_count": 2 })
            ),
            Some(Place::Lane { target: AutomationTarget::TrackVolume { track: TrackId(4) } })
        );
        assert_eq!(
            of(
                "set_automation",
                json!({ "target": { "type": "master_volume" }, "points": [] }),
                json!({ "cleared": true })
            ),
            None
        );

        // Time: the markers set, the stretch repeated or opened up, the loop.
        let markers = json!({ "markers": [{ "beat": 32, "name": "Verse" }, { "beat": 0, "name": "Intro" }] });
        assert_eq!(of("set_markers", markers, json!({})), Some(Place::Span { start: 0.0, end: 32.0 }));
        assert_eq!(of("set_markers", json!({ "markers": [] }), json!({})), None);
        let repeat = json!({ "action": "duplicate", "start": 16, "end": 32 });
        assert_eq!(of("edit_time", repeat, json!({})), Some(Place::Span { start: 16.0, end: 48.0 }));
        let open = json!({ "action": "insert", "start": 8, "length": 4 });
        assert_eq!(of("edit_time", open, json!({})), Some(Place::Span { start: 8.0, end: 12.0 }));
        let cut = json!({ "action": "delete", "start": 8, "end": 16 });
        assert_eq!(of("edit_time", cut, json!({})), Some(Place::Span { start: 8.0, end: 8.0 }));
        let copy = json!({ "action": "copy", "start": 16, "end": 32, "to": 64 });
        assert_eq!(of("edit_time", copy, json!({})), Some(Place::Span { start: 64.0, end: 80.0 }));
        let looped = json!({ "tempo": 96, "loop": { "enabled": true, "start": 16, "end": 32 } });
        assert_eq!(of("set_project", looped, json!({})), Some(Place::Span { start: 16.0, end: 32.0 }));
        assert_eq!(of("set_project", json!({ "tempo": 96 }), json!({})), None);

        // Reading, removing and anything unknown name nothing; nor does an
        // answer that is not what was expected.
        for tool in [
            "get_project",
            "get_notes",
            "delete_track",
            "delete_clip",
            "delete_device",
            "history",
            "render",
            "freeze_track",
        ] {
            assert_eq!(
                of(tool, json!({ "track": 1, "clip": 2, "device": 3 }), json!({ "track_id": 1, "clip_id": 2 })),
                None,
                "{tool}"
            );
        }
        assert_eq!(of_call("create_track", &json!({ "name": "x" }), "it went wrong"), None);
        assert_eq!(of_call("create_clip", &json!("nonsense"), "{\"clip_id\":\"seven\"}"), None);
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

        // An envelope the tool was told by the parameter's name is the one
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
    fn the_song_before_and_after_says_what_the_calls_did_not() {
        let (before, [lead, bass], [hook, _], [synth, limiter]) = song();
        assert!(between(&before, &before).is_empty());

        let mut after = before.clone();
        let drums = after.add_track("Drums");
        let beat = after.add_midi_clip(drums, "Beat", 0.0, 4.0).unwrap();
        after.set_track_volume(bass, -6.0).unwrap();
        after.clip_mut(hook).unwrap().add_notes(&[Note { pitch: 60, start: 0.0, length: 1.0, velocity: 100 }]).unwrap();
        after.device_mut(synth).unwrap().params.insert("cutoff".into(), 900.0);
        after.device_mut(limiter).unwrap().enabled = false;
        let volume = AutomationTarget::TrackVolume { track: lead };
        after.set_automation(volume.clone(), vec![AutomationPoint::new(0.0, -3.0)]).unwrap();

        let places = between(&before, &after);
        assert_eq!(
            places,
            [
                Place::device(synth.0),
                Place::track(bass.0),
                Place::track(drums.0),
                Place::device(limiter.0),
                clips(&[hook, beat]).unwrap(),
                Place::Lane { target: volume },
            ]
        );
    }

    #[test]
    fn the_way_selects_what_was_changed_and_shows_where_it_is() {
        let (mut project, [lead, bass], [hook, line], [synth, limiter]) = song();
        let mut state = UiState::new(DetailTab::Mixer);
        state.reconcile(&project);

        // Clips on two tracks: all selected, the last one's track and the
        // Clip tab, and the stretch of time they cover between them.
        let made = [clips(&[hook]).unwrap(), Place::track(bass.0), clips(&[line, ClipId(999)]).unwrap()];
        Route::to(&made, &project).expect("they are there").take(&mut state);
        assert_eq!(state.selected_clips, [hook, line]);
        assert_eq!((state.selected_clip, state.selected_track), (Some(line), Some(bass)));
        assert_eq!(state.detail_tab, DetailTab::Clip);
        assert_eq!(
            state.arrangement.show,
            Some(Reveal { tracks: vec![lead, bass], span: Some((16.0, 48.0)), waited: 0 })
        );

        // A device: its track, the Devices tab, the device chosen there.
        // The clips selected before stay selected.
        Route::to(&[Place::device(synth.0)], &project).unwrap().take(&mut state);
        assert_eq!((state.selected_track, state.selected_device), (Some(lead), Some(synth)));
        assert_eq!(state.reveal_device, Some(synth), "the rack is asked to scroll to it");
        assert_eq!(state.detail_tab, DetailTab::Devices);
        assert_eq!(state.selected_clips, [hook, line]);
        assert_eq!(state.arrangement.show, Some(Reveal { tracks: vec![lead], span: None, waited: 0 }));
        // One on the master selects the master.
        Route::to(&[Place::device(limiter.0)], &project).unwrap().take(&mut state);
        assert!(state.master_selected);
        assert_eq!(state.selected_device, Some(limiter));

        // An envelope: its lane unfolded and asked into view, over the
        // time its points cover.
        let pan = AutomationTarget::TrackPan { track: bass };
        project
            .set_automation(pan.clone(), vec![AutomationPoint::new(8.0, -1.0), AutomationPoint::new(24.0, 1.0)])
            .unwrap();
        Route::to(&[Place::Lane { target: pan.clone() }], &project).unwrap().take(&mut state);
        assert!(state.arrangement.lane_open(&pan) && state.arrangement.reveal == Some(pan));
        assert_eq!((state.selected_track, state.master_selected), (Some(bass), false));
        assert_eq!(state.detail_tab, DetailTab::Devices, "a lane is in the arrangement: the tab stays");
        assert_eq!(state.arrangement.show.as_ref().unwrap().span, Some((8.0, 24.0)));

        // A stretch of time alone moves nothing but the view.
        let selected = state.selected_clips.clone();
        Route::to(&[Place::Span { start: 64.0, end: 80.0 }], &project).unwrap().take(&mut state);
        assert_eq!(state.selected_clips, selected);
        assert_eq!(state.arrangement.show, Some(Reveal { tracks: Vec::new(), span: Some((64.0, 80.0)), waited: 0 }));

        // A turn that did several things: everything is selected, and the
        // tab and the track go by the clip or the device that came last.
        let mut state = UiState::new(DetailTab::Mixer);
        state.reconcile(&project);
        let mixed = [clips(&[line]).unwrap(), Place::device(synth.0), Place::track(bass.0)];
        Route::to(&mixed, &project).unwrap().take(&mut state);
        assert_eq!(state.selected_clips, [line]);
        assert_eq!(
            (state.detail_tab, state.selected_track, state.selected_device),
            (DetailTab::Devices, Some(lead), Some(synth))
        );
        let mixed = [Place::device(synth.0), clips(&[line]).unwrap(), Place::track(lead.0)];
        state.selected_device = None;
        Route::to(&mixed, &project).unwrap().take(&mut state);
        assert_eq!(
            (state.detail_tab, state.selected_track, state.selected_device),
            (DetailTab::Clip, Some(bass), None)
        );

        // What is gone leads nowhere.
        assert_eq!(Route::to(&[Place::track(999), clips(&[ClipId(999)]).unwrap(), Place::device(999)], &project), None);
        assert_eq!(Route::to(&[], &project), None);
    }
}
