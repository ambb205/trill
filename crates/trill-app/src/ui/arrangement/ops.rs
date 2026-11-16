//! Commands on the arrangement's selection: what the keyboard, the Edit
//! menu and the context menus all call. Each is one undo step.

use super::rows::Rows;
use super::time_ops;
use crate::ui::snapshot::Snapshot;
use crate::ui::state::{Clipboard, CopiedClip, Cx, InlineClip, TextEditState, TimeSelection, Tool, UiState};
use trill_core::{Beats, Clip, ClipId, Track, TrackId, TrackKind};

/// The selected clips that still exist, each with its track.
fn selected<'a>(snapshot: &'a Snapshot, state: &UiState) -> Vec<(&'a Track, &'a Clip)> {
    state.selected_clips.iter().filter_map(|id| snapshot.clip(*id)).collect()
}

/// From the earliest start to the latest end of `clips`.
fn span(clips: &[(&Track, &Clip)]) -> Option<(Beats, Beats)> {
    let start = clips.iter().map(|(_, clip)| clip.start).fold(f64::INFINITY, f64::min);
    let end = clips.iter().map(|(_, clip)| clip.end()).fold(f64::NEG_INFINITY, f64::max);
    (end > start).then_some((start, end))
}

/// The stretch of time commands act on: the time selection if there is
/// one, else what the selected clips cover.
pub fn selected_span(cx: &Cx) -> Option<(Beats, Beats)> {
    match &cx.state.arrangement.time_selection {
        Some(selection) if selection.end > selection.start => Some((selection.start, selection.end)),
        _ => span(&selected(cx.snap, cx.state)),
    }
}

/// The tracks that hold clips, top to bottom as shown.
fn clip_tracks(cx: &Cx) -> Vec<TrackId> {
    let tracks = &cx.snap.project.tracks;
    let rows = Rows::build(tracks, &cx.state.arrangement);
    rows.clip_tracks().iter().map(|index| tracks[*index].id).collect()
}

pub fn delete_selection(cx: &mut Cx) {
    let doomed = cx.state.selected_clips.clone();
    if doomed.is_empty() {
        return;
    }
    let removed = cx.edit(|project| {
        // A clip another front end already removed is not an error.
        for id in &doomed {
            if project.clip(*id).is_ok() {
                project.remove_clip(*id)?;
            }
        }
        Ok(())
    });
    if removed.is_some() {
        cx.state.clear_clip_selection();
    }
}

/// Copies the selected clips to right after the selection and selects the
/// copies, so repeating the command lays out a row of repeats.
pub fn duplicate_selection(cx: &mut Cx) {
    let clips = selected(cx.snap, cx.state);
    let Some((start, end)) = span(&clips) else { return };
    let sources: Vec<(ClipId, Beats)> = clips.iter().map(|(_, clip)| (clip.id, clip.start)).collect();
    let length = end - start;
    let copies = cx.edit(|project| {
        sources
            .iter()
            .map(|(id, start)| project.duplicate_clip(*id, start + length, None))
            .collect::<Result<Vec<_>, _>>()
    });
    if let Some(copies) = copies {
        cx.state.select_clips(copies);
        cx.state.arrangement.time_selection = None;
        cx.state.arrangement.cursor = end + length;
    }
}

/// Puts the selected clips on the clipboard, with how they lie relative to
/// one another.
pub fn copy(cx: &mut Cx) {
    let clips = selected(cx.snap, cx.state);
    let Some((start, _)) = span(&clips) else { return };
    let lanes = clip_tracks(cx);
    let lane_of = |track: &Track| lanes.iter().position(|id| *id == track.id).unwrap_or(0);
    let top = clips.iter().map(|(track, _)| lane_of(track)).min().unwrap_or(0);
    let copied: Vec<CopiedClip> = clips
        .iter()
        .map(|(track, clip)| CopiedClip {
            clip: (*clip).clone(),
            offset: clip.start - start,
            lane: lane_of(track) - top,
        })
        .collect();
    let count = copied.len();
    cx.state.copied_text =
        Some(if count == 1 { "1 clip from Trill".to_owned() } else { format!("{count} clips from Trill") });
    cx.state.clipboard = Some(Clipboard::Clips(copied));
}

pub fn cut(cx: &mut Cx) {
    copy(cx);
    delete_selection(cx);
}

/// Pastes the copied clips at the edit cursor, the topmost of them on the
/// selected track, and moves the cursor past them so pasting again follows
/// on.
pub fn paste(cx: &mut Cx) {
    let Some(Clipboard::Clips(copied)) = cx.state.clipboard.clone() else { return };
    let lanes = clip_tracks(cx);
    let base = cx.state.selected_track.and_then(|track| lanes.iter().position(|id| *id == track));
    let Some(base) = base else {
        cx.state.toasts.info("Select a track that holds clips to paste onto");
        return;
    };
    let cursor = cx.state.arrangement.cursor;
    let placed: Vec<(TrackId, &CopiedClip)> =
        copied.iter().filter_map(|item| Some((*lanes.get(base + item.lane)?, item))).collect();
    if placed.len() < copied.len() {
        let missing = copied.len() - placed.len();
        cx.state.toasts.info(format!("{missing} of the copied clips had no track below to go on"));
    }
    let pasted = cx.edit(|project| {
        placed
            .iter()
            .map(|(track, item)| project.paste_clip(*track, &item.clip, cursor + item.offset))
            .collect::<Result<Vec<_>, _>>()
    });
    if let Some(pasted) = pasted.filter(|pasted| !pasted.is_empty()) {
        let length = placed.iter().map(|(_, item)| item.offset + item.clip.length).fold(0.0, f64::max);
        cx.state.select_clips(pasted);
        cx.state.arrangement.time_selection = None;
        cx.state.arrangement.cursor = cursor + length;
    }
}

/// Splits at the edit cursor, or at the playhead while the song plays: the
/// selected clips it falls inside, or failing that the clips of the
/// selected track.
pub fn split_at_cursor(cx: &mut Cx) {
    let at = if cx.live.playing { cx.live.position } else { cx.state.arrangement.cursor };
    let inside = |clip: &Clip| clip.start < at && at < clip.end();
    let chosen = selected(cx.snap, cx.state);
    let mut targets: Vec<ClipId> = chosen.iter().filter(|(_, clip)| inside(clip)).map(|(_, clip)| clip.id).collect();
    if targets.is_empty()
        && let Some(track) = cx.state.selected_track.and_then(|id| cx.snap.track(id))
    {
        targets = track.clips.iter().filter(|clip| inside(clip)).map(|clip| clip.id).collect();
    }
    if targets.is_empty() {
        cx.state.toasts.info("No clip under the edit cursor to split");
        return;
    }
    cx.edit(|project| targets.iter().try_for_each(|id| project.split_clip(*id, at).map(drop)));
}

/// Splits one clip at `at`, as the blade does.
pub fn split_clip(cx: &mut Cx, clip: ClipId, at: Beats) {
    cx.edit(|project| project.split_clip(clip, at).map(drop));
}

/// Mutes the selected clips, or unmutes them if they all are muted.
pub fn toggle_mute(cx: &mut Cx) {
    let clips = selected(cx.snap, cx.state);
    if clips.is_empty() {
        return;
    }
    let mute = clips.iter().any(|(_, clip)| !clip.muted);
    let ids: Vec<ClipId> = clips.iter().map(|(_, clip)| clip.id).collect();
    cx.edit(|project| {
        for id in &ids {
            project.clip_mut(*id)?.muted = mute;
        }
        Ok(())
    });
}

/// Moves the selected clips by `steps` grid steps, stopping together at
/// the start of the song.
pub fn nudge(cx: &mut Cx, steps: i32) {
    let clips = selected(cx.snap, cx.state);
    let Some((start, _)) = span(&clips) else { return };
    let delta = (steps as f64 * cx.state.arrangement.step).max(-start);
    if delta == 0.0 {
        return;
    }
    let moves: Vec<(ClipId, Beats)> = clips.iter().map(|(_, clip)| (clip.id, clip.start + delta)).collect();
    cx.edit(|project| moves.iter().try_for_each(|(id, start)| project.move_clip(*id, None, *start)));
}

/// Selects every clip of the song.
pub fn select_all(cx: &mut Cx) {
    let all = cx.snap.project.tracks.iter().flat_map(|track| track.clips.iter().map(|clip| clip.id)).collect();
    cx.state.select_clips(all);
    cx.state.arrangement.time_selection = None;
}

/// Escape: puts a tool away, or failing that drops the selections.
pub fn escape(cx: &mut Cx) {
    let view = &mut cx.state.arrangement;
    if view.tool != Tool::Pointer {
        view.tool = Tool::Pointer;
        return;
    }
    view.time_selection = None;
    view.points = None;
    cx.state.clear_clip_selection();
}

/// Makes the selected time the loop region and switches looping on.
pub fn loop_selection(cx: &mut Cx) {
    match selected_span(cx) {
        Some((start, end)) => drop(cx.edit(|project| project.set_loop(true, start, end))),
        None => cx.state.toasts.info("Select some time or a clip to loop it"),
    }
}

/// Removes the selected time from every track and closes the gap.
pub fn delete_time(cx: &mut Cx) {
    let Some((start, end)) = selected_span(cx) else {
        cx.state.toasts.info("Select the time to delete first: drag in an empty lane or on the ruler");
        return;
    };
    if cx.edit(|project| time_ops::delete_time(project, start, end)).is_some() {
        cx.state.arrangement.time_selection = None;
        cx.state.arrangement.cursor = start;
    }
}

/// Repeats the selected time right after itself, moving what follows.
pub fn duplicate_time(cx: &mut Cx) {
    let Some((start, end)) = selected_span(cx) else {
        cx.state.toasts.info("Select the time to duplicate first: drag in an empty lane or on the ruler");
        return;
    };
    if cx.edit(|project| time_ops::duplicate_time(project, start, end)).is_some() {
        // The copy becomes the selection, so the command can be repeated.
        let tracks = cx.state.arrangement.time_selection.take().map(|selection| selection.tracks).unwrap_or_default();
        let length = end - start;
        cx.state.arrangement.time_selection = Some(TimeSelection { start: end, end: end + length, tracks });
        cx.state.arrangement.cursor = end;
    }
}

/// Adds a marker at the edit cursor.
pub fn add_marker(cx: &mut Cx) {
    add_marker_at(cx, cx.state.arrangement.cursor);
}

pub fn add_marker_at(cx: &mut Cx, beat: Beats) {
    let name = format!("Marker {}", cx.snap.project.markers.len() + 1);
    cx.edit(|project| project.add_marker(beat, name));
}

/// Moves the edit cursor, and the playhead with it unless the song is
/// playing: Space then plays from where the user is working.
pub fn set_cursor(cx: &mut Cx, beat: Beats) {
    cx.state.arrangement.cursor = beat;
    if !cx.live.playing {
        cx.with_session(|session| session.seek(beat));
    }
}

/// Opens the name of the clip the Clip tab shows for typing, on the clip.
pub fn rename_selected(cx: &mut Cx) {
    if let Some((_, clip)) = cx.state.selected_clip.and_then(|id| cx.snap.clip(id)) {
        cx.state.renaming_clip_inline =
            Some(TextEditState { target: InlineClip(clip.id), text: clip.name.clone(), focused: false });
    }
}

/// Creates a one-bar MIDI clip and opens it in the piano roll.
pub fn add_clip(cx: &mut Cx, track: TrackId, start: Beats) {
    let length = cx.snap.project.time_signature.beats_per_bar();
    let created = cx.edit(|project| {
        let name = format!("Clip {}", project.track(track)?.clips.len() + 1);
        project.add_midi_clip(track, name, start, length)
    });
    if let Some(clip) = created {
        cx.state.select_clip(track, clip);
        cx.state.detail_tab = crate::ui::DetailTab::Clip;
    }
}

/// Why a track of this kind takes no clips; `None` for one that does.
pub fn kind_note(kind: TrackKind) -> Option<&'static str> {
    match kind {
        TrackKind::Normal => None,
        TrackKind::Group => Some("A group track mixes the tracks that belong to it and holds no clips"),
        TrackKind::Return => Some("A return track receives what other tracks send to it and holds no clips"),
    }
}
