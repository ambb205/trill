//! Which rows the arrangement shows and in what order: tracks with their
//! group members under them, folded groups hidden, and the automation
//! lanes unfolded under each track. Pure layout, no painting.
//!
//! The project keeps tracks in one flat list and says which group a track
//! plays through. Here that becomes what a musician sees: a group followed
//! by its members, wherever in the list they happen to be.

use crate::ui::state::{ArrangementState, LaneOwner, TrackDrop};
use trill_core::{Track, TrackId, TrackKind};

/// What a row of the arrangement holds.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A track, by its index in the project. `member` is true for a track
    /// shown indented under its group.
    Track { index: usize, member: bool },
    /// An automation lane, by its index in the view's list of open lanes,
    /// under the track at `track`.
    Lane { lane: usize, track: usize },
    /// A lane of the takes kept beside the track at `track`, unfolded
    /// under it: directly under the track, above its automation.
    Take { track: usize, lane: u32 },
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Row {
    pub kind: RowKind,
    /// Distance from the top of the first row.
    pub top: f32,
    pub height: f32,
}

impl Row {
    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

/// The rows of one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    pub rows: Vec<Row>,
    /// Height of all rows together.
    pub height: f32,
    /// The master's open lanes (indices into the view's list). They are
    /// pinned above the master row instead of scrolling with the tracks.
    pub master_lanes: Vec<usize>,
    /// The visible tracks that hold clips, top to bottom, by project index.
    clip_tracks: Vec<usize>,
}

/// The group a track is shown under: the one it names, if that is a group
/// track that exists.
fn shown_group(tracks: &[Track], track: &Track) -> Option<TrackId> {
    let group = track.group.filter(|_| track.kind == TrackKind::Normal)?;
    tracks.iter().any(|other| other.id == group && other.kind == TrackKind::Group).then_some(group)
}

/// The tracks in the order they are shown, by project index: each group
/// followed by its members, everything else where the project has it.
/// The flag is true for a member.
pub fn display_order(tracks: &[Track]) -> Vec<(usize, bool)> {
    let mut order = Vec::with_capacity(tracks.len());
    for (index, track) in tracks.iter().enumerate() {
        if shown_group(tracks, track).is_some() {
            continue;
        }
        order.push((index, false));
        if track.kind == TrackKind::Group {
            let members = tracks.iter().enumerate().filter(|(_, other)| shown_group(tracks, other) == Some(track.id));
            order.extend(members.map(|(member, _)| (member, true)));
        }
    }
    order
}

impl Rows {
    pub fn build(tracks: &[Track], view: &ArrangementState) -> Self {
        let mut rows = Vec::new();
        let mut clip_tracks = Vec::new();
        let mut top = 0.0;
        for (index, member) in display_order(tracks) {
            let track = &tracks[index];
            if member && track.group.is_some_and(|group| view.folded.contains(&group)) {
                continue;
            }
            let height = view.height_of(track.id);
            rows.push(Row { kind: RowKind::Track { index, member }, top, height });
            top += height;
            if track.kind == TrackKind::Normal {
                clip_tracks.push(index);
            }
            if view.takes_open.contains(&track.id) {
                for lane in 0..track.lanes() {
                    let height = super::takes::LANE_HEIGHT;
                    rows.push(Row { kind: RowKind::Take { track: index, lane }, top, height });
                    top += height;
                }
            }
            for (lane, open) in view.lanes.iter().enumerate() {
                if open.owner == LaneOwner::Track(track.id) {
                    rows.push(Row { kind: RowKind::Lane { lane, track: index }, top, height: open.height });
                    top += open.height;
                }
            }
        }
        let master = view.lanes.iter().enumerate().filter(|(_, lane)| lane.owner == LaneOwner::Master);
        Self { rows, height: top, master_lanes: master.map(|(lane, _)| lane).collect(), clip_tracks }
    }

    /// The row at `y`, measured from the top of the first row.
    pub fn at(&self, y: f32) -> Option<&Row> {
        if y < 0.0 {
            return None;
        }
        let index = self.rows.partition_point(|row| row.bottom() <= y);
        self.rows.get(index)
    }

    /// The row of the track at project index `index`; `None` while it is
    /// folded away.
    pub fn track(&self, index: usize) -> Option<&Row> {
        self.rows.iter().find(|row| matches!(row.kind, RowKind::Track { index: found, .. } if found == index))
    }

    /// The row of the open lane `lane`, if it scrolls with the tracks.
    pub fn lane(&self, lane: usize) -> Option<&Row> {
        self.rows.iter().find(|row| matches!(row.kind, RowKind::Lane { lane: found, .. } if found == lane))
    }

    /// The visible tracks that hold clips, top to bottom, by project index.
    pub fn clip_tracks(&self) -> &[usize] {
        &self.clip_tracks
    }

    /// Where among [`Self::clip_tracks`] the clip-holding track nearest to
    /// `y` is: the one under it, or the closest above or below. For drags,
    /// which should not let go of a clip because the pointer crossed a
    /// group header or left the lanes.
    pub fn nearest_clip_track(&self, y: f32) -> Option<usize> {
        let distance = |index: &usize| {
            let row = self.track(*index).expect("clip tracks are visible");
            if y < row.top {
                row.top - y
            } else if y >= row.bottom() {
                y - row.bottom() + 1.0
            } else {
                0.0
            }
        };
        let nearest = self.clip_tracks.iter().enumerate().min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)));
        nearest.map(|(position, _)| position)
    }

    /// Total height of the master's lanes.
    pub fn master_lanes_height(&self, view: &ArrangementState) -> f32 {
        self.master_lanes.iter().map(|lane| view.lanes[*lane].height).sum()
    }
}

/// Where a header dragged to `y` (measured like [`Rows::at`]) would land.
/// `None` when that is where it already is, or nowhere.
pub fn drop_at(tracks: &[Track], rows: &Rows, dragged: TrackId, y: f32) -> Option<TrackDrop> {
    let moving = tracks.iter().find(|track| track.id == dragged)?;
    // Only an ordinary track can go inside a group.
    let joins = moving.kind == TrackKind::Normal;
    let track_rows: Vec<&Row> = rows.rows.iter().filter(|row| matches!(row.kind, RowKind::Track { .. })).collect();
    let track_of = |row: &Row| match row.kind {
        RowKind::Track { index, .. } => &tracks[index],
        RowKind::Lane { track, .. } | RowKind::Take { track, .. } => &tracks[track],
    };
    // The track row the pointer is on, or whose lanes it is on; past the
    // last row counts as the last row's lower half.
    let last = *track_rows.last()?;
    let (position, over) = match rows.at(y.max(0.0)) {
        Some(row) => {
            let owner = track_of(row).id;
            let position = track_rows.iter().position(|candidate| track_of(candidate).id == owner)?;
            (position, track_rows[position])
        }
        None => (track_rows.len() - 1, last),
    };
    let target = track_of(over);
    let upper = y < over.top + over.height / 2.0;
    let next = |position: usize| track_rows.get(position + 1).map(|row| track_of(row));

    let drop = if target.kind == TrackKind::Group {
        let members_follow = next(position).is_some_and(|after| shown_group(tracks, after) == Some(target.id));
        if upper && y < over.top + over.height / 4.0 || !joins {
            // Above the group; a group or return dropped on its lower half
            // goes below the whole block.
            let before = if upper { Some(target.id) } else { after_block(tracks, &track_rows, position) };
            TrackDrop { before, group: None }
        } else {
            // Onto the group: in as its first member.
            let before = if members_follow { next(position).map(|track| track.id) } else { None };
            let before = before.or_else(|| after_block(tracks, &track_rows, position));
            TrackDrop { before, group: Some(target.id) }
        }
    } else if let Some(group) = shown_group(tracks, target) {
        if joins {
            let before = if upper { Some(target.id) } else { next(position).map(|track| track.id) };
            TrackDrop { before, group: Some(group) }
        } else {
            // Not allowed inside: above or below the whole group.
            let head = track_rows.iter().position(|row| track_of(row).id == group).unwrap_or(position);
            let before = if upper { Some(group) } else { after_block(tracks, &track_rows, head) };
            TrackDrop { before, group: None }
        }
    } else {
        let before = if upper { Some(target.id) } else { next(position).map(|track| track.id) };
        // Below a group's last member is outside the group.
        TrackDrop { before, group: None }
    };

    // Dropping a track onto its own place changes nothing.
    let unchanged =
        reordered(tracks, dragged, drop) == display_ids(tracks) && shown_group(tracks, moving) == drop.group;
    (!unchanged && drop.before != Some(dragged)).then_some(drop)
}

/// The first track after the group whose header is at `head` and all of its
/// visible members.
fn after_block(tracks: &[Track], track_rows: &[&Row], head: usize) -> Option<TrackId> {
    let track_at = |row: &Row| match row.kind {
        RowKind::Track { index, .. } => &tracks[index],
        RowKind::Lane { track, .. } | RowKind::Take { track, .. } => &tracks[track],
    };
    let group = track_at(track_rows[head]).id;
    let rest = track_rows[head + 1..].iter().map(|row| track_at(row));
    rest.skip_while(|track| shown_group(tracks, track) == Some(group)).map(|track| track.id).next()
}

fn display_ids(tracks: &[Track]) -> Vec<TrackId> {
    display_order(tracks).into_iter().map(|(index, _)| tracks[index].id).collect()
}

/// The order of all tracks after `dragged` (with its members, if it is a
/// group) has been dropped at `drop`: the display order, which is the order
/// the project is then given.
pub fn reordered(tracks: &[Track], dragged: TrackId, drop: TrackDrop) -> Vec<TrackId> {
    let order = display_ids(tracks);
    let in_block = |id: &TrackId| {
        *id == dragged || tracks.iter().any(|track| track.id == *id && shown_group(tracks, track) == Some(dragged))
    };
    let block: Vec<TrackId> = order.iter().copied().filter(in_block).collect();
    let mut rest: Vec<TrackId> = order.into_iter().filter(|id| !in_block(id)).collect();
    let at = drop.before.and_then(|before| rest.iter().position(|id| *id == before)).unwrap_or(rest.len());
    rest.splice(at..at, block);
    rest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::DetailTab;
    use crate::ui::state::UiState;
    use trill_core::{AutomationTarget, Project};

    /// Drums, a group "Synths" declared last with Pad and Lead in it, Bass
    /// between them, and a return.
    fn project() -> (Project, [TrackId; 6]) {
        let mut project = Project::new("t");
        let drums = project.add_track("Drums");
        let pad = project.add_track("Pad");
        let bass = project.add_track("Bass");
        let lead = project.add_track("Lead");
        let reverb = project.add_track_of_kind("Reverb", TrackKind::Return);
        let synths = project.add_track_of_kind("Synths", TrackKind::Group);
        project.set_track_group(pad, Some(synths)).unwrap();
        project.set_track_group(lead, Some(synths)).unwrap();
        (project, [drums, pad, bass, lead, reverb, synths])
    }

    fn view() -> ArrangementState {
        UiState::new(DetailTab::Clip).arrangement
    }

    fn names(project: &Project, ids: &[TrackId]) -> Vec<String> {
        ids.iter().map(|id| project.track(*id).unwrap().name.clone()).collect()
    }

    #[test]
    fn members_are_shown_under_their_group() {
        let (project, _) = project();
        let order = display_order(&project.tracks);
        let shown: Vec<(&str, bool)> =
            order.iter().map(|(index, member)| (project.tracks[*index].name.as_str(), *member)).collect();
        assert_eq!(
            shown,
            [("Drums", false), ("Bass", false), ("Reverb", false), ("Synths", false), ("Pad", true), ("Lead", true)]
        );

        // A track naming a group that is gone, or that is no group, stands
        // on its own.
        let mut loose = project.clone();
        loose.tracks[1].group = Some(TrackId(999));
        loose.tracks[3].group = Some(loose.tracks[0].id);
        assert!(display_order(&loose.tracks).iter().all(|(_, member)| !member));
        assert_eq!(display_order(&loose.tracks).len(), 6);
    }

    #[test]
    fn rows_stack_with_lanes_under_their_track_and_folds_hide_members() {
        let (project, [drums, pad, _, _, _, synths]) = project();
        let mut view = view();
        view.heights.insert(drums, 80.0);
        view.open_lane(LaneOwner::Track(drums), AutomationTarget::TrackVolume { track: drums });
        view.open_lane(LaneOwner::Master, AutomationTarget::MasterVolume);
        view.open_lane(LaneOwner::Track(pad), AutomationTarget::TrackPan { track: pad });
        let rows = Rows::build(&project.tracks, &view);
        let lane = ArrangementState::DEFAULT_LANE_HEIGHT;

        assert_eq!(rows.rows[0], Row { kind: RowKind::Track { index: 0, member: false }, top: 0.0, height: 80.0 });
        assert_eq!(rows.rows[1], Row { kind: RowKind::Lane { lane: 0, track: 0 }, top: 80.0, height: lane });
        assert_eq!(rows.rows[2].top, 80.0 + lane);
        assert_eq!(rows.rows.len(), 8, "six tracks and two lanes that scroll");
        assert_eq!(rows.height, 80.0 + 5.0 * 60.0 + 2.0 * lane);
        assert_eq!(rows.master_lanes, [1], "the master's lane is pinned, not a row");
        assert_eq!(rows.master_lanes_height(&view), lane);
        assert_eq!(rows.clip_tracks(), [0, 2, 1, 3], "groups and returns hold no clips");

        assert_eq!(rows.at(-1.0), None);
        assert_eq!(rows.at(0.0).unwrap().kind, RowKind::Track { index: 0, member: false });
        assert_eq!(rows.at(79.9).unwrap().kind, RowKind::Track { index: 0, member: false });
        assert_eq!(rows.at(80.0).unwrap().kind, RowKind::Lane { lane: 0, track: 0 });
        assert_eq!(rows.at(rows.height), None);
        assert_eq!(rows.track(1).unwrap().kind, RowKind::Track { index: 1, member: true });
        assert_eq!(rows.lane(2).unwrap().kind, RowKind::Lane { lane: 2, track: 1 });
        assert_eq!(rows.lane(1), None);

        view.folded.insert(synths);
        let folded = Rows::build(&project.tracks, &view);
        assert_eq!(folded.rows.len(), 5, "the members and the lane under one of them are folded away");
        assert_eq!(folded.track(1), None);
        assert_eq!(folded.clip_tracks(), [0, 2]);
    }

    #[test]
    fn takes_unfold_under_their_track_above_its_automation() {
        use trill_core::project::AudioContent;
        let (mut project, [drums, ..]) = project();
        for name in ["Take 1", "Take 2"] {
            project.add_take(drums, name, 0.0, 4.0, AudioContent::new("take.wav")).unwrap();
        }
        let mut view = view();
        view.open_lane(LaneOwner::Track(drums), AutomationTarget::TrackVolume { track: drums });
        let folded = Rows::build(&project.tracks, &view);
        assert_eq!(folded.rows.len(), 7, "takes are kept out of sight until they are unfolded");
        view.takes_open.insert(drums);
        let rows = Rows::build(&project.tracks, &view);
        let height = crate::ui::arrangement::takes::LANE_HEIGHT;
        assert_eq!(rows.rows[1], Row { kind: RowKind::Take { track: 0, lane: 0 }, top: 60.0, height });
        assert_eq!(rows.rows[2].kind, RowKind::Take { track: 0, lane: 1 });
        assert_eq!(rows.rows[3].kind, RowKind::Lane { lane: 0, track: 0 }, "automation comes after the takes");
        assert_eq!(rows.rows[4].top, 60.0 + 2.0 * height + ArrangementState::DEFAULT_LANE_HEIGHT);
        assert_eq!(rows.clip_tracks(), [0, 2, 1, 3], "a lane of takes holds no clips");
        // A header dragged onto a lane of takes lands by the track above it.
        let dragged = project.tracks[2].id;
        assert_eq!(drop_at(&project.tracks, &rows, dragged, 70.0), drop_at(&project.tracks, &folded, dragged, 40.0));
    }

    #[test]
    fn a_drag_stays_with_the_nearest_track_that_holds_clips() {
        let (project, _) = project();
        let rows = Rows::build(&project.tracks, &view());
        // Rows: Drums 0, Bass 60, Reverb 120, Synths 180, Pad 240, Lead 300.
        assert_eq!(rows.nearest_clip_track(-500.0), Some(0));
        assert_eq!(rows.nearest_clip_track(70.0), Some(1), "Bass");
        assert_eq!(rows.nearest_clip_track(130.0), Some(1), "over the return: the track above is nearer");
        assert_eq!(rows.nearest_clip_track(230.0), Some(2), "over the group header: its first member");
        assert_eq!(rows.nearest_clip_track(9000.0), Some(3));
        assert_eq!(Rows::default().nearest_clip_track(10.0), None);
    }

    #[test]
    fn a_dropped_track_takes_its_place_and_its_group() {
        let (project, [drums, pad, bass, lead, reverb, synths]) = project();
        let tracks = &project.tracks;
        let rows = Rows::build(tracks, &view());
        let order = |dragged, drop| names(&project, &reordered(tracks, dragged, drop));

        // Bass onto the upper half of Pad: into the group, ahead of Pad.
        let drop = drop_at(tracks, &rows, bass, 250.0).unwrap();
        assert_eq!(drop, TrackDrop { before: Some(pad), group: Some(synths) });
        assert_eq!(order(bass, drop), ["Drums", "Reverb", "Synths", "Bass", "Pad", "Lead"]);

        // Onto the group header itself: its first member.
        assert_eq!(drop_at(tracks, &rows, bass, 220.0).unwrap(), TrackDrop { before: Some(pad), group: Some(synths) });
        // The top edge of the header: above the group, outside it.
        assert_eq!(drop_at(tracks, &rows, lead, 185.0).unwrap(), TrackDrop { before: Some(synths), group: None });
        assert_eq!(order(lead, TrackDrop { before: Some(synths), group: None })[3..5], ["Lead", "Synths"]);

        // Below the last member, past the end of the list: still in the
        // group if dropped on a member's lower half.
        assert_eq!(drop_at(tracks, &rows, bass, 350.0).unwrap(), TrackDrop { before: None, group: Some(synths) });
        assert_eq!(order(bass, TrackDrop { before: None, group: Some(synths) })[3..], ["Pad", "Lead", "Bass"]);

        // A group moves with its members and cannot go inside itself or
        // another track's place within a group.
        let drop = drop_at(tracks, &rows, synths, 10.0).unwrap();
        assert_eq!(drop, TrackDrop { before: Some(drums), group: None });
        assert_eq!(order(synths, drop), ["Synths", "Pad", "Lead", "Drums", "Bass", "Reverb"]);
        // A return dropped on a member goes above or below the whole group.
        assert_eq!(drop_at(tracks, &rows, reverb, 250.0), None, "above the group is where it already is");
        assert_eq!(drop_at(tracks, &rows, reverb, 330.0).unwrap(), TrackDrop { before: None, group: None });

        // Dropping a track where it is does nothing.
        assert_eq!(drop_at(tracks, &rows, drums, 10.0), None);
        assert_eq!(drop_at(tracks, &rows, drums, 50.0), None);
        assert_eq!(drop_at(tracks, &rows, pad, 250.0), None);
        // Lead out of the group by dropping it on Drums.
        assert_eq!(drop_at(tracks, &rows, lead, 50.0).unwrap(), TrackDrop { before: Some(bass), group: None });
    }
}
