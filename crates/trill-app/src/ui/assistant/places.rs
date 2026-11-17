//! How the window goes to where the assistant left its mark.
//!
//! A turn changes the song through Trill's tools, and each call says what it
//! touched: in its arguments (`update_clip {"clip": 7}`) or in its answer
//! (`create_track` answers `{"track_id": 9}`). Reading that into a [`Place`]
//! (a track, some clips, a device, an automation lane, a stretch of the
//! timeline) is the server's (`trill_mcp::words::of_call`), and what a place
//! is the journal's (`trill_core::journal::Place`), which keeps one for every
//! step of a session. Where the calls of a turn name nothing (a tool that
//! table does not know), the song as it was before the turn is compared with
//! the song after ([`between`]).
//!
//! [`Route::to`] works out, against the song as it is now, what to select
//! and what to bring into view for some places, and [`Route::take`] does it:
//! the clips selected, their track selected, the Clip tab open on the last
//! of them (or the Devices tab on a device, a lane unfolded for an
//! envelope), and the arrangement asked to scroll, and zoom if it must, to
//! where they are. A place whose thing is gone leads nowhere.
//!
//! When this is done is the panel's business (`super`): once, when a turn
//! has ended and the user is not at work elsewhere, and whenever a row of
//! the conversation is clicked.

use crate::ui::DetailTab;
use crate::ui::arrangement::Reveal;
use crate::ui::state::{LaneOwner, UiState, lane_owner};
pub use trill_core::journal::Place;
use trill_core::journal::place::{lane, of_change};
use trill_core::journal::{Blobs, Change};
use trill_core::{AutomationTarget, Beats, ChainRef, ClipId, DeviceId, Project, TrackId};
pub use trill_mcp::words::of_call;

/// What differs between the song as it was and as it is, for a turn whose
/// calls did not say: the clips, devices and tracks that are new or not as
/// they were, and envelopes likewise, each under the name it goes by now.
/// It cannot tell the assistant's changes from ones the user made
/// meanwhile, which is why the calls are asked first.
pub fn between(before: &Project, after: &Project) -> Vec<Place> {
    of_change(&Change::between(before, after, &mut Blobs::default()), after)
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
    use trill_core::AutomationPoint;

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
    fn the_song_before_and_after_says_what_the_calls_did_not() {
        let (before, [_, bass], [hook, _], [synth, _]) = song();
        assert!(between(&before, &before).is_empty());
        let mut after = before.clone();
        after.set_track_volume(bass, -6.0).unwrap();
        after.clip_mut(hook).unwrap().muted = true;
        after.device_mut(synth).unwrap().params.insert("cutoff".into(), 900.0);
        assert_eq!(
            between(&before, &after),
            [
                Place::Track { id: bass.0, name: "Bass".into() },
                Place::Device { id: synth.0, name: "Poly Synth".into() },
                Place::Clips { clips: vec![(hook.0, "Hook".into())] },
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
