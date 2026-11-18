//! The lanes: the clips of every track on the timeline, and what the
//! pointer does to them. Selecting, moving, copying, trimming, fading,
//! splitting and the rubber band all start here; the commands they share
//! with the keyboard are in [`super::ops`].
//!
//! A drag keeps the clips' positions as they were when it began and derives
//! every step from those, inside one undo gesture. Clips are followed by
//! id, so an edit made elsewhere in the middle of a drag at worst removes a
//! clip from it.
//!
//! The lanes are also where sounds land: while one is dragged over them
//! (see [`crate::ui::dragging`]) they offer the track and the place under
//! the pointer, or a new track below the last one, and draw the clip that
//! would be made.

use super::clips::{self, ClipHit, ClipLook, ClipPart};
use super::rows::RowKind;
use super::{Geometry, envelopes, ops};
use crate::ui::shortcuts::{self, Command};
use crate::ui::sounds::DropTarget;
use crate::ui::state::{
    ArrangeDrag, ClipOrigin, Clipboard, Cx, DetailTab, FadeEnd, FocusArea, InlineClip, TimeSelection, Tool,
};
use crate::ui::{colors, dragging, timeline, widgets};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Mesh, PointerButton, Pos2, Rect, Response, Sense, Shape, Ui, pos2, vec2,
};
use trill_core::{Beats, Clip, ClipId, TrackId, TrackKind};
use trill_noir::{MenuItem, fade, paint, text};

/// What a right-click in the lanes was on, kept while its menu is open.
#[derive(Clone, Debug, PartialEq)]
enum MenuSubject {
    /// The selected clips; `beat` is where the pointer was, for "split here".
    Clips {
        clip: ClipId,
        beat: Beats,
    },
    /// Empty lane space on `track`.
    Lane {
        track: Option<TrackId>,
        beat: Beats,
    },
    Envelope(envelopes::MenuSubject),
}

pub(super) fn show(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let response = ui.interact(geometry.lanes, ui.scope_id().with("lanes"), Sense::click_and_drag());
    let tracks = &cx.snap.project.tracks;
    let on_clip = |pointer: Pos2| clips::clip_at(tracks, geometry, pointer).map(|hit| hit.clip);
    let hovered = response.hover_pos().filter(|_| cx.state.arrangement.drag.is_none()).and_then(on_clip);

    paint_rows(ui, cx, geometry, hovered);
    for row in &geometry.rows.rows {
        if let RowKind::Lane { lane, .. } = row.kind
            && geometry.is_visible(row)
        {
            envelopes::show(ui, cx, geometry, &response, lane, geometry.row_rect(row), geometry.lanes);
        }
    }
    paint_overlays(ui, cx, geometry, &response);
    interact(ui, cx, geometry, &response);
    sound_drop(ui, cx, geometry);
    preset_drop(ui, cx, geometry);
    context_menu(ui, cx, geometry, &response);
    rename_field(ui, cx, geometry);
}

/// True if `position` is on an automation lane rather than a track's row.
fn on_envelope(geometry: &Geometry, position: Pos2) -> bool {
    geometry.row_at(position.y).is_some_and(|row| matches!(row.kind, RowKind::Lane { .. }))
}

/// The track whose row (or whose automation lane) is at `position`.
fn track_at(cx: &Cx, geometry: &Geometry, position: Pos2) -> Option<(TrackId, TrackKind)> {
    let index = match geometry.row_at(position.y)?.kind {
        RowKind::Track { index, .. } => index,
        RowKind::Lane { track, .. } => track,
    };
    let track = &cx.snap.project.tracks[index];
    Some((track.id, track.kind))
}

// ---- painting --------------------------------------------------------------

fn paint_rows(ui: &Ui, cx: &Cx, geometry: &Geometry, hovered: Option<ClipId>) {
    let theme = cx.theme;
    let painter = ui.painter_at(geometry.lanes);
    let lanes = geometry.lanes;
    let (first_beat, last_beat) = geometry.time.visible(lanes.width());

    timeline::for_each_grid_line(first_beat, last_beat, &geometry.meter, geometry.grid, |beat, level| {
        let color = if geometry.is_strong_line(beat, level) { theme.grid } else { theme.grid_fine };
        paint::vline(&painter, geometry.x(beat).round(), lanes.y_range(), color);
    });

    let renaming = cx.state.renaming_clip_inline.as_ref().map(|edit| edit.target.0);
    let mut notes = Mesh::default();
    for row in geometry.rows.rows.iter().filter(|row| geometry.is_visible(row)) {
        let RowKind::Track { index, .. } = row.kind else { continue };
        let track = &cx.snap.project.tracks[index];
        let lane = geometry.row_rect(row);
        if track.kind != TrackKind::Normal {
            // A group or return track mixes other tracks; its lane holds
            // no clips, and says so by stepping back.
            painter.rect_filled(lane, 0.0, theme.shade);
        }
        paint::hline_above(&painter, lanes.x_range(), lane.bottom(), theme.hairline);
        let color = colors::track_color(theme, track.color);
        for (index, clip) in track.clips.iter().enumerate() {
            if clip.end() < first_beat || clip.start > last_beat {
                continue;
            }
            let rect = geometry.clip_rect(row, clip);
            // A clip that starts at the same place is painted on top and
            // hides this one's start; the name moves to where it shows.
            // Clips are sorted by start, so those follow directly.
            let later = track.clips[index + 1..].iter().take_while(|later| later.start <= clip.start);
            let covered_until = later.map(Clip::end).fold(clip.start, f64::max);
            let look = ClipLook {
                color,
                ghosted: clip.muted || track.mute,
                name_from: geometry.x(covered_until).round(),
                handles: hovered == Some(clip.id) || cx.state.is_clip_selected(clip.id),
                renaming: renaming == Some(clip.id),
            };
            clips::paint_clip(&painter, cx, &mut notes, rect, geometry, clip, look);
        }
    }
    painter.add(Shape::mesh(notes));
}

/// What lies over the clips: the time selection, the outlines of the
/// selected clips, the markers' lines and the blade.
fn paint_overlays(ui: &Ui, cx: &Cx, geometry: &Geometry, response: &Response) {
    let theme = cx.theme;
    let lanes = geometry.lanes;
    let painter = ui.painter_at(lanes);
    let view = &cx.state.arrangement;
    let tracks = &cx.snap.project.tracks;

    for marker in &cx.snap.project.markers {
        let x = geometry.x(marker.beat).round();
        if x >= lanes.left() && x <= lanes.right() {
            paint::vline(&painter, x, lanes.y_range(), theme.hairline);
        }
    }

    if let Some(selection) = view.time_selection.as_ref().filter(|selection| selection.end > selection.start) {
        let (left, right) = (geometry.x(selection.start).round(), geometry.x(selection.end).round());
        let wash = |rect: Rect| {
            painter.rect_filled(rect, 0.0, theme.selected);
            paint::vline(&painter, rect.left(), rect.y_range(), theme.ink_dim);
            paint::vline_left_of(&painter, rect.right(), rect.y_range(), theme.ink_dim);
        };
        if selection.tracks.is_empty() {
            wash(Rect::from_x_y_ranges(left..=right, lanes.y_range()));
        }
        for row in geometry.rows.rows.iter().filter(|row| geometry.is_visible(row)) {
            if let RowKind::Track { index, .. } = row.kind
                && selection.tracks.contains(&tracks[index].id)
            {
                let rect = geometry.row_rect(row);
                wash(Rect::from_x_y_ranges(left..=right, rect.top()..=rect.bottom() - 1.0));
            }
        }
    }

    // On top of the notes of neighboring clips.
    for (track, clip) in cx.state.selected_clips.iter().filter_map(|id| cx.snap.clip(*id)) {
        if let Some(row) = cx.snap.track_index(track.id).and_then(|index| geometry.rows.track(index)) {
            paint::frame(&painter, geometry.clip_rect(row, clip), theme.ink);
        }
    }

    // The blade shows where it would cut.
    if view.tool == Tool::Blade
        && let Some(pointer) = response.hover_pos()
        && let Some(hit) = clips::clip_at(tracks, geometry, pointer)
        && let Some((track, clip)) = cx.snap.clip(hit.clip)
        && let Some(row) = cx.snap.track_index(track.id).and_then(|index| geometry.rows.track(index))
    {
        let free = ui.input(|input| input.modifiers.alt);
        let at = blade_position(geometry, clip, pointer.x, free);
        paint::vline(&painter, geometry.x(at).round(), geometry.clip_rect(row, clip).y_range(), theme.ink);
    }
}

/// Where the blade cuts `clip` with the pointer at `x`: on the grid, unless
/// that would be at or past one of the clip's ends.
fn blade_position(geometry: &Geometry, clip: &Clip, x: f32, free: bool) -> Beats {
    let exact = geometry.beat(x);
    let snapped = geometry.snap(exact, free);
    if snapped > clip.start && snapped < clip.end() { snapped } else { exact }
}

// ---- interaction -----------------------------------------------------------

fn interact(ui: &Ui, cx: &mut Cx, geometry: &Geometry, response: &Response) {
    let modifiers = ui.input(|input| input.modifiers);
    let blade = cx.state.arrangement.tool == Tool::Blade;

    if cx.state.arrangement.drag.is_none()
        && let Some(pointer) = response.hover_pos()
        && !on_envelope(geometry, pointer)
    {
        hover(ui, cx, geometry, pointer, blade);
    }

    if let Some(pointer) = response.interact_pointer_pos().filter(|pointer| !on_envelope(geometry, *pointer)) {
        if widgets::double_clicked(response) && !blade {
            double_click(cx, geometry, pointer);
        } else if response.clicked() {
            click(cx, geometry, pointer, modifiers, blade);
        }
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|input| input.pointer.press_origin())
        && !on_envelope(geometry, press)
        && !blade
    {
        start_drag(cx, geometry, press, modifiers);
    }
    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        drag(ui, cx, geometry, pointer, modifiers.alt);
    }
}

fn hover(ui: &Ui, cx: &mut Cx, geometry: &Geometry, pointer: Pos2, blade: bool) {
    let hit = clips::clip_at(&cx.snap.project.tracks, geometry, pointer);
    let hint = match hit.map(|hit| hit.part) {
        Some(_) if blade => {
            ui.set_cursor_icon(CursorIcon::Crosshair);
            "Click to split the clip here \u{00B7} Alt for no snapping \u{00B7} C or Esc puts the blade away"
        }
        Some(ClipPart::Start | ClipPart::End) => {
            ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
            "Drag to trim the clip \u{00B7} Alt for no snapping"
        }
        Some(ClipPart::FadeIn | ClipPart::FadeOut) => {
            ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
            "Drag to set the fade"
        }
        Some(ClipPart::Title) => "Drag to move \u{00B7} Ctrl-drag copies \u{00B7} double-click to rename",
        Some(ClipPart::Body) => {
            "Drag to move \u{00B7} Ctrl-drag copies \u{00B7} double-click opens the piano roll \u{00B7} right-click for more"
        }
        None => "Click sets the edit cursor \u{00B7} drag selects \u{00B7} double-click creates a clip",
    };
    cx.state.hint.set(hint);
}

fn click(cx: &mut Cx, geometry: &Geometry, pointer: Pos2, modifiers: egui::Modifiers, blade: bool) {
    cx.state.focus = FocusArea::Arrangement;
    let adding = modifiers.shift || modifiers.command;
    match clips::clip_at(&cx.snap.project.tracks, geometry, pointer) {
        Some(hit) if blade => {
            if let Some((_, clip)) = cx.snap.clip(hit.clip) {
                let at = blade_position(geometry, clip, pointer.x, modifiers.alt);
                ops::split_clip(cx, hit.clip, at);
            }
        }
        Some(hit) => {
            if adding {
                cx.state.toggle_clip(hit.track, hit.clip);
            } else {
                cx.state.select_clip(hit.track, hit.clip);
                // The name strip stands for the clip as a whole; a click
                // in its body also points at a place in it, which is
                // where Ctrl+E then splits.
                let at = match hit.part {
                    ClipPart::Title => hit.start,
                    _ => geometry.snap(geometry.beat(pointer.x), modifiers.alt),
                };
                ops::set_cursor(cx, at);
            }
            cx.state.arrangement.time_selection = None;
        }
        None => {
            if !adding {
                cx.state.clear_clip_selection();
            }
            cx.state.arrangement.time_selection = None;
            if let Some((track, _)) = track_at(cx, geometry, pointer) {
                cx.state.select_track(track);
            }
            ops::set_cursor(cx, geometry.snap(geometry.beat(pointer.x), modifiers.alt));
        }
    }
}

fn double_click(cx: &mut Cx, geometry: &Geometry, pointer: Pos2) {
    match clips::clip_at(&cx.snap.project.tracks, geometry, pointer) {
        Some(hit) => {
            cx.state.select_clip(hit.track, hit.clip);
            if hit.part == ClipPart::Title {
                ops::rename_selected(cx);
            } else {
                cx.state.detail_tab = DetailTab::Clip;
            }
        }
        None => {
            if let Some((track, kind)) = track_at(cx, geometry, pointer) {
                match ops::kind_note(kind) {
                    Some(note) => cx.state.toasts.info(note),
                    None => {
                        let bar = geometry.beats_per_bar;
                        let start = (geometry.beat(pointer.x) / bar).floor().max(0.0) * bar;
                        ops::add_clip(cx, track, start);
                    }
                }
            }
        }
    }
}

fn start_drag(cx: &mut Cx, geometry: &Geometry, press: Pos2, modifiers: egui::Modifiers) {
    cx.state.focus = FocusArea::Arrangement;
    let adding = modifiers.shift || modifiers.command;
    let Some(hit) = clips::clip_at(&cx.snap.project.tracks, geometry, press) else {
        // Empty space: a rubber band.
        let kept = if adding { cx.state.selected_clips.clone() } else { Vec::new() };
        if !adding {
            cx.state.clear_clip_selection();
        }
        let band = ArrangeDrag::Band { anchor: geometry.beat(press.x), anchor_y: geometry.content_y(press.y), kept };
        cx.state.arrangement.drag = Some(band);
        return;
    };
    cx.state.arrangement.time_selection = None;
    let edge = |cx: &mut Cx, drag: ArrangeDrag| {
        cx.state.select_clip(hit.track, hit.clip);
        drag
    };
    let drag = match hit.part {
        ClipPart::Start => edge(cx, ArrangeDrag::TrimStart { clip: hit.clip }),
        ClipPart::End => edge(cx, ArrangeDrag::TrimEnd { clip: hit.clip }),
        ClipPart::FadeIn => edge(cx, ArrangeDrag::Fade { clip: hit.clip, end: FadeEnd::In }),
        ClipPart::FadeOut => edge(cx, ArrangeDrag::Fade { clip: hit.clip, end: FadeEnd::Out }),
        ClipPart::Body | ClipPart::Title => {
            if !cx.state.is_clip_selected(hit.clip) {
                if modifiers.shift {
                    cx.state.toggle_clip(hit.track, hit.clip);
                } else {
                    cx.state.select_clip(hit.track, hit.clip);
                }
            }
            return start_move(cx, geometry, press, hit, modifiers.command);
        }
    };
    cx.begin_gesture();
    cx.state.arrangement.drag = Some(drag);
}

/// Begins moving the selected clips, or copies of them when `copy`.
fn start_move(cx: &mut Cx, geometry: &Geometry, press: Pos2, hit: ClipHit, copy: bool) {
    let snapshot = cx.snap;
    let chosen = cx.state.selected_clips.iter().filter_map(|id| snapshot.clip(*id));
    let mut origins: Vec<ClipOrigin> =
        chosen.map(|(track, clip)| ClipOrigin { clip: clip.id, track: track.id, start: clip.start }).collect();
    let mut grabbed = hit.clip;
    cx.begin_gesture();
    if copy {
        // The copies are made where the originals are and then dragged
        // away from them, all in the one gesture.
        let sources = origins.clone();
        let copies = cx.edit(|project| {
            sources
                .iter()
                .map(|origin| project.duplicate_clip(origin.clip, origin.start, None))
                .collect::<Result<Vec<_>, _>>()
        });
        let Some(copies) = copies else { return };
        for (origin, copy) in origins.iter_mut().zip(&copies) {
            if origin.clip == hit.clip {
                grabbed = *copy;
            }
            origin.clip = *copy;
        }
        cx.state.select_clips(copies);
    }
    let grab_offset = geometry.beat(press.x) - hit.start;
    cx.state.arrangement.drag = Some(ArrangeDrag::MoveClips { grabbed, grab_offset, origins, applied: (0.0, 0) });
}

fn drag(ui: &Ui, cx: &mut Cx, geometry: &Geometry, pointer: Pos2, free: bool) {
    let snapshot = cx.snap;
    match cx.state.arrangement.drag.clone() {
        Some(ArrangeDrag::MoveClips { grabbed, grab_offset, origins, applied }) => {
            ui.set_cursor_icon(CursorIcon::Grabbing);
            move_clips(cx, geometry, pointer, free, grabbed, grab_offset, origins, applied);
        }
        Some(ArrangeDrag::TrimStart { clip }) => {
            ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
            let Some((_, current)) = snapshot.clip(clip) else { return };
            let latest = current.end() - geometry.min_clip_length(free);
            let start = geometry.snap(geometry.beat(pointer.x), free).min(latest).max(0.0);
            if (start - current.start).abs() >= 1e-9 {
                cx.edit(|project| project.trim_clip_start(clip, start).map(drop));
            }
        }
        Some(ArrangeDrag::TrimEnd { clip }) => {
            ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
            let Some((_, current)) = snapshot.clip(clip) else { return };
            let end = geometry.snap(geometry.beat(pointer.x), free);
            let wanted = (end - current.start).max(geometry.min_clip_length(free));
            // An audio clip stops growing where its file ends.
            let (start, length) = (current.start, clips::allowed_length(cx, current, wanted));
            if (current.length - length).abs() >= 1e-9 {
                cx.edit(|project| project.set_clip_span(clip, start, length));
            }
        }
        Some(ArrangeDrag::Fade { clip, end }) => {
            ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
            let Some((_, current)) = snapshot.clip(clip) else { return };
            let Some(audio) = current.audio() else { return };
            // Fades are short and set by ear, so they never snap.
            let beat = geometry.beat(pointer.x);
            let (fade_in, fade_out) = match end {
                FadeEnd::In => ((beat - current.start).clamp(0.0, current.length - audio.fade_out), audio.fade_out),
                FadeEnd::Out => (audio.fade_in, (current.end() - beat).clamp(0.0, current.length - audio.fade_in)),
            };
            if (fade_in, fade_out) != (audio.fade_in, audio.fade_out) {
                cx.edit(|project| project.set_clip_fades(clip, fade_in, fade_out));
            }
        }
        Some(ArrangeDrag::Band { anchor, anchor_y, kept }) => band(cx, geometry, pointer, free, anchor, anchor_y, kept),
        _ => {}
    }
}

/// How many lanes a group of clips on `lanes` (positions among the `count`
/// tracks that hold clips) can move when the pointer asks for `wanted`:
/// held back as a group, so the clips keep their lanes relative to one
/// another and none leaves the list.
fn lane_shift(lanes: &[usize], count: usize, wanted: i32) -> i32 {
    let (Some(top), Some(bottom)) = (lanes.iter().min(), lanes.iter().max()) else { return 0 };
    let last = count.saturating_sub(1) as i32;
    wanted.clamp(-(*top as i32), (last - *bottom as i32).max(0))
}

#[expect(clippy::too_many_arguments, reason = "the fields of one drag, taken apart by the caller")]
fn move_clips(
    cx: &mut Cx,
    geometry: &Geometry,
    pointer: Pos2,
    free: bool,
    grabbed: ClipId,
    grab_offset: Beats,
    origins: Vec<ClipOrigin>,
    applied: (Beats, i32),
) {
    let tracks = &cx.snap.project.tracks;
    let Some(anchor) = origins.iter().find(|origin| origin.clip == grabbed) else { return };
    // The grabbed clip lands on the grid and the others keep their distance
    // from it; together they stop at the start of the song.
    let earliest = origins.iter().map(|origin| origin.start).fold(f64::INFINITY, f64::min);
    let beats = (geometry.snap(geometry.beat(pointer.x) - grab_offset, free) - anchor.start).max(-earliest);

    let clip_tracks = geometry.rows.clip_tracks();
    let lane_of = |track: TrackId| clip_tracks.iter().position(|index| tracks[*index].id == track);
    let lanes: Vec<usize> = origins.iter().filter_map(|origin| lane_of(origin.track)).collect();
    let under_pointer = geometry.rows.nearest_clip_track(geometry.content_y(pointer.y));
    let rows = match (under_pointer, lane_of(anchor.track)) {
        (Some(to), Some(from)) => lane_shift(&lanes, clip_tracks.len(), to as i32 - from as i32),
        _ => 0,
    };
    if (beats, rows) == applied {
        return;
    }

    let target = |origin: &ClipOrigin| {
        let lane = lane_of(origin.track)? as i32 + rows;
        Some((origin.clip, tracks[*clip_tracks.get(lane as usize)?].id, origin.start + beats))
    };
    let moves: Vec<(ClipId, TrackId, Beats)> = origins.iter().filter_map(target).collect();
    let moved = cx.edit(|project| {
        // A clip removed elsewhere since the drag began drops out of it.
        for (clip, track, start) in &moves {
            if project.clip(*clip).is_ok() {
                project.move_clip(*clip, Some(*track), *start)?;
            }
        }
        Ok(())
    });
    if moved.is_some() {
        if let Some((_, track, _)) = moves.iter().find(|(clip, ..)| *clip == grabbed) {
            cx.state.select_track(*track);
        }
        let applied = (beats, rows);
        cx.state.arrangement.drag = Some(ArrangeDrag::MoveClips { grabbed, grab_offset, origins, applied });
    }
}

/// The rubber band: selects the stretch of time it spans on the tracks it
/// crosses, and the clips that lie in it.
fn band(cx: &mut Cx, geometry: &Geometry, pointer: Pos2, free: bool, anchor: Beats, anchor_y: f32, kept: Vec<ClipId>) {
    let tracks = &cx.snap.project.tracks;
    let now = geometry.beat(pointer.x);
    let (start, end) = (geometry.snap(anchor.min(now), free), geometry.snap(anchor.max(now), free));
    // A band narrower than a grid step still picks up what it touches.
    let (from, to) = if end > start { (start, end) } else { (anchor.min(now), anchor.max(now)) };
    let pointer_y = geometry.content_y(pointer.y);
    let (top, bottom) = (anchor_y.min(pointer_y), anchor_y.max(pointer_y));

    let crossed = geometry.rows.rows.iter().filter(|row| row.bottom() > top && row.top <= bottom);
    let crossed: Vec<usize> = crossed
        .filter_map(|row| match row.kind {
            RowKind::Track { index, .. } => Some(index),
            RowKind::Lane { .. } => None,
        })
        .collect();
    let mut selection = kept;
    for index in &crossed {
        let touched = tracks[*index].clips.iter().filter(|clip| clip.start < to && clip.end() > from);
        for clip in touched {
            if !selection.contains(&clip.id) {
                selection.push(clip.id);
            }
        }
    }
    if let Some(first) = crossed.first() {
        cx.state.select_track(tracks[*first].id);
    }
    cx.state.select_clips(selection);
    let view = &mut cx.state.arrangement;
    view.cursor = start;
    let tracks = crossed.iter().map(|index| tracks[*index].id).collect();
    view.time_selection = (end > start).then_some(TimeSelection { start, end, tracks });
}

// ---- sounds being dropped --------------------------------------------------

/// Height of the band below the last track that stands for the track a
/// drop there would make.
const NEW_TRACK_BAND: f32 = super::TRACK_HEIGHT;

/// While a sound is dragged over the lanes: offers the place under the
/// pointer and draws the clip that would be made there. Drops still on
/// their way (the file is being read) keep their ghost until they land.
fn sound_drop(ui: &Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let lanes = geometry.lanes;
    let painter = ui.painter_at(lanes);
    let tracks = &cx.snap.project.tracks;
    let project_tempo = cx.snap.project.tempo;
    let bar = geometry.beats_per_bar;
    // The row a new track would get: right under the last one.
    let new_row = |beat: Beats, length: Beats| {
        let top = geometry.screen_y(geometry.rows.height);
        let left = geometry.x(beat).round();
        let right = (geometry.x(beat + length).round() - 1.0).max(left + 12.0);
        Rect::from_min_max(pos2(left, top + 1.0), pos2(right, top + NEW_TRACK_BAND - 2.0))
    };
    let on_track = |track: TrackId, beat: Beats, length: Beats| {
        let index = cx.snap.track_index(track)?;
        let row = geometry.rows.track(index)?;
        let top = geometry.screen_y(row.top);
        let left = geometry.x(beat).round();
        let right = (geometry.x(beat + length).round() - 1.0).max(left + 12.0);
        let color = colors::track_color(theme, tracks[index].color);
        Some((Rect::from_min_max(pos2(left, top + 1.0), pos2(right, top + row.height - 2.0)), color))
    };

    for pending in &cx.sounds.pending {
        let length = if pending.length > 0.0 { pending.length } else { bar };
        match &pending.target {
            DropTarget::Lane { track, beat } => {
                if let Some((rect, color)) = on_track(*track, *beat, length) {
                    ghost(&painter, theme, rect, color, &pending.label, false);
                }
            }
            DropTarget::NewTrack { beat, colors } => {
                let color = colors.first().map_or(theme.ink_dim, |stored| colors::track_color(theme, *stored));
                ghost(&painter, theme, new_row(*beat, length), color, &pending.label, false);
            }
            _ => {}
        }
    }

    let free = ui.input(|input| input.modifiers.alt);
    let Some(drag) = &mut cx.state.sound_drag else { return };
    let Some(pointer) = drag.pointer.filter(|pointer| lanes.contains(*pointer)) else { return };
    if drag.is_instrument() {
        // An instrument is the track's as a whole: its row is lit, and
        // below the tracks the band says a track will appear.
        match geometry.row_at(pointer.y) {
            Some(row) => {
                let index = match row.kind {
                    RowKind::Track { index, .. } => index,
                    RowKind::Lane { track, .. } => track,
                };
                let track = &tracks[index];
                if track.kind != TrackKind::Normal {
                    drag.refusal = Some("An instrument goes on a track that holds clips, not on a group or a return");
                    return;
                }
                drag.offer(DropTarget::TrackInstrument { track: track.id });
                let top = geometry.screen_y(row.top);
                let lit = Rect::from_x_y_ranges(lanes.x_range(), top..=top + row.height - 1.0);
                paint::frame(&painter, lit, theme.ink);
            }
            None => {
                let color = dragging::new_track_colors(cx, 1).first().copied();
                let top = geometry.screen_y(geometry.rows.height);
                let band = Rect::from_x_y_ranges(lanes.x_range(), top..=top + NEW_TRACK_BAND);
                painter.rect_filled(band, 0.0, theme.hover);
                paint::hline_above(&painter, band.x_range(), band.bottom(), theme.hairline);
                let caption = Rect::from_min_max(band.min + vec2(8.0, 0.0), band.max - vec2(8.0, 0.0));
                text::paint(&painter, caption, Align2::RIGHT_CENTER, theme.text.micro, "New track", theme.ink_faint);
                if let Some(drag) = &mut cx.state.sound_drag {
                    drag.offer(DropTarget::NewInstrument { color });
                }
            }
        }
        return;
    }
    let beat = geometry.snap(geometry.beat(pointer.x), free);
    let length = drag.beats(project_tempo, bar);
    let label = drag.label.clone();
    match geometry.row_at(pointer.y) {
        Some(row) => {
            let index = match row.kind {
                RowKind::Track { index, .. } => index,
                RowKind::Lane { track, .. } => track,
            };
            let track = &tracks[index];
            if track.kind != TrackKind::Normal {
                drag.refusal = Some("Audio goes on a track that holds clips, not on a group or a return");
                return;
            }
            drag.offer(DropTarget::Lane { track: track.id, beat });
            if let Some((rect, color)) = on_track(track.id, beat, length) {
                ghost(&painter, theme, rect, color, &label, true);
            }
        }
        None => {
            // What comes from a study is no file, and makes one track.
            let count = drag.paths.len().max(1);
            let colors = dragging::new_track_colors(cx, count);
            let color = colors::track_color(theme, colors[0]);
            // The band says a track will appear; the ghost, where on it.
            let top = geometry.screen_y(geometry.rows.height);
            let band = Rect::from_x_y_ranges(lanes.x_range(), top..=top + NEW_TRACK_BAND);
            painter.rect_filled(band, 0.0, theme.hover);
            paint::hline_above(&painter, band.x_range(), band.bottom(), theme.hairline);
            let caption = Rect::from_min_max(band.min + vec2(8.0, 0.0), band.max - vec2(8.0, 0.0));
            text::paint(&painter, caption, Align2::RIGHT_CENTER, theme.text.micro, "New audio track", theme.ink_faint);
            ghost(&painter, theme, new_row(beat, length), color, &label, true);
            if let Some(drag) = &mut cx.state.sound_drag {
                drag.offer(DropTarget::NewTrack { beat, colors });
            }
        }
    }
}

/// While a sound of an instrument and its effects is dragged over the
/// lanes: the track under the pointer is offered, and below the tracks a
/// track of its own.
fn preset_drop(ui: &Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let lanes = geometry.lanes;
    let Some(pointer) = crate::ui::sound_presets::over(cx, lanes) else { return };
    let painter = ui.painter_at(lanes);
    match geometry.row_at(pointer.y) {
        Some(row) => {
            let index = match row.kind {
                RowKind::Track { index, .. } => index,
                RowKind::Lane { track, .. } => track,
            };
            let Some(track) = cx.snap.project.tracks.get(index).map(|track| track.id) else { return };
            if crate::ui::sound_presets::offer_track(cx, track)
                && let Some(row) = geometry.rows.track(index)
            {
                let top = geometry.screen_y(row.top);
                let band = Rect::from_x_y_ranges(lanes.x_range(), top..=top + row.height - 1.0);
                paint::frame(&painter, band, theme.ink);
            }
        }
        None => {
            // The band says a track will appear.
            let top = geometry.screen_y(geometry.rows.height);
            let band = Rect::from_x_y_ranges(lanes.x_range(), top..=top + NEW_TRACK_BAND);
            painter.rect_filled(band, 0.0, theme.hover);
            paint::hline_above(&painter, band.x_range(), band.bottom(), theme.hairline);
            let caption = Rect::from_min_max(band.min + vec2(8.0, 0.0), band.max - vec2(8.0, 0.0));
            text::paint(&painter, caption, Align2::RIGHT_CENTER, theme.text.micro, "New track", theme.ink_faint);
            crate::ui::sound_presets::offer_new_track(cx);
        }
    }
}

/// The outline of a clip that does not exist yet. `live` while it follows
/// the pointer; stepped back while its file is still being read.
fn ghost(painter: &egui::Painter, theme: &trill_noir::Theme, rect: Rect, color: Color32, label: &str, live: bool) {
    painter.rect_filled(rect, 0.0, fade(color, if live { 0.22 } else { 0.10 }));
    let title = clips::title_rect(rect);
    painter.rect_filled(title, 0.0, fade(color, if live { 0.75 } else { 0.35 }));
    paint::frame(painter, rect, if live { theme.ink } else { theme.ink_faint });
    if rect.width() > 18.0 {
        let room = Rect::from_min_max(pos2(title.left() + 4.0, title.top()), pos2(title.right() - 3.0, title.bottom()));
        let ink = if live { theme.on_accent } else { theme.ink_dim };
        text::paint(painter, room, Align2::LEFT_CENTER, theme.text.micro, label, ink);
    }
}

// ---- menus and renaming ----------------------------------------------------

/// Right-click menu for the clips, the empty lane or the envelope under
/// the pointer.
fn context_menu(ui: &Ui, cx: &mut Cx, geometry: &Geometry, response: &Response) {
    // Selecting what was right-clicked makes the menu's target visible and
    // lets the menu simply act on the selection.
    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        cx.state.focus = FocusArea::Arrangement;
        let beat = geometry.beat(pointer.x);
        let subject = if on_envelope(geometry, pointer) {
            envelopes::menu_subject(cx, geometry, pointer).map(MenuSubject::Envelope)
        } else {
            match clips::clip_at(&cx.snap.project.tracks, geometry, pointer) {
                Some(hit) => {
                    if !cx.state.is_clip_selected(hit.clip) {
                        cx.state.select_clip(hit.track, hit.clip);
                    }
                    Some(MenuSubject::Clips { clip: hit.clip, beat })
                }
                None => {
                    cx.state.clear_clip_selection();
                    let track = track_at(cx, geometry, pointer);
                    if let Some((track, _)) = track {
                        cx.state.select_track(track);
                    }
                    // Pasting and new markers go where the menu was opened.
                    cx.state.arrangement.cursor = geometry.snap(beat, false);
                    let holds_clips = track.filter(|(_, kind)| *kind == TrackKind::Normal).map(|(track, _)| track);
                    Some(MenuSubject::Lane { track: holds_clips, beat })
                }
            }
        };
        ui.data_mut(|data| match subject {
            Some(subject) => drop(data.insert_temp(response.id, subject)),
            None => data.remove::<MenuSubject>(response.id),
        });
    }
    let Some(subject) = ui.data(|data| data.get_temp::<MenuSubject>(response.id)) else { return };

    response.context_menu(|ui| {
        ui.set_min_width(210.0);
        match &subject {
            MenuSubject::Clips { clip, beat } => clip_menu(ui, cx, geometry, *clip, *beat),
            MenuSubject::Lane { track, beat } => lane_menu(ui, cx, geometry, *track, *beat),
            MenuSubject::Envelope(subject) => envelopes::menu(ui, cx, subject),
        }
    });
}

fn clip_menu(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry, clip: ClipId, beat: Beats) {
    let theme = cx.theme;
    let item = |ui: &mut Ui, label: &str, command: Command| {
        MenuItem::new(label).shortcut(&shortcuts::keys(command)).show(ui, theme).clicked()
    };
    if item(ui, "Cut", Command::Cut) {
        ops::cut(cx);
    }
    if item(ui, "Copy", Command::Copy) {
        ops::copy(cx);
    }
    if item(ui, "Duplicate", Command::Duplicate) {
        ops::duplicate_selection(cx);
    }
    if item(ui, "Delete", Command::Delete) {
        ops::delete_selection(cx);
    }
    trill_noir::menu_separator(ui, theme);
    let inside = cx.snap.clip(clip).filter(|(_, clip)| clip.start < beat && beat < clip.end());
    if MenuItem::new("Split here").enabled(inside.is_some()).show(ui, theme).clicked()
        && let Some((_, target)) = inside
    {
        let at = blade_position(geometry, target, geometry.x(beat), false);
        ops::split_clip(cx, clip, at);
    }
    if item(ui, "Split at the edit cursor", Command::Split) {
        ops::split_at_cursor(cx);
    }
    let muted = cx.state.selected_clips.iter().filter_map(|id| cx.snap.clip(*id)).all(|(_, clip)| clip.muted);
    if item(ui, if muted { "Unmute" } else { "Mute" }, Command::MuteClips) {
        ops::toggle_mute(cx);
    }
    if item(ui, "Rename", Command::RenameClip) {
        ops::rename_selected(cx);
    }
    trill_noir::menu_separator(ui, theme);
    if item(ui, "Loop the selection", Command::LoopSelection) {
        ops::loop_selection(cx);
    }
    if item(ui, "Zoom to the selection", Command::ZoomToSelection) {
        super::zoom_to_selection(ui.ctx(), cx);
    }
    // An audio clip's file: to hear it on its own, or to carry it from the
    // browser onto a pad or into a sampler.
    let file = cx.snap.clip(clip).and_then(|(_, clip)| clip.audio()).map(|audio| std::path::PathBuf::from(&audio.path));
    if let Some(file) = file {
        trill_noir::menu_separator(ui, theme);
        if MenuItem::new("Show its file in the browser").show(ui, theme).clicked() {
            crate::ui::browser::choose_file(cx, file.clone());
        }
        if MenuItem::new("Show its file in the file manager").show(ui, theme).clicked() {
            crate::ui::browser::reveal(cx, file);
        }
    }
}

fn lane_menu(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry, track: Option<TrackId>, beat: Beats) {
    let theme = cx.theme;
    let item = |ui: &mut Ui, label: &str, command: Command, enabled: bool| {
        MenuItem::new(label).shortcut(&shortcuts::keys(command)).enabled(enabled).show(ui, theme).clicked()
    };
    // A copied stretch of time goes back on the tracks it came from, whichever lane this is.
    let holds_time = matches!(cx.state.clipboard, Some(Clipboard::Time(_)));
    let can_paste = holds_time || matches!(cx.state.clipboard, Some(Clipboard::Clips(_))) && track.is_some();
    if item(ui, if holds_time { "Paste over" } else { "Paste" }, Command::Paste, can_paste) {
        ops::paste(cx);
    }
    if MenuItem::new("Add MIDI clip here").enabled(track.is_some()).show(ui, theme).clicked()
        && let Some(track) = track
    {
        let bar = geometry.beats_per_bar;
        ops::add_clip(cx, track, (beat / bar).floor().max(0.0) * bar);
    }
    if item(ui, "Add marker here", Command::AddMarker, true) {
        ops::add_marker(cx);
    }
    if item(ui, "Select all clips", Command::SelectAll, true) {
        ops::select_all(cx);
    }
    trill_noir::menu_separator(ui, theme);
    let has_time = cx.state.arrangement.time_selection.is_some();
    if item(ui, "Loop the selection", Command::LoopSelection, has_time) {
        ops::loop_selection(cx);
    }
    if item(ui, "Delete time", Command::DeleteTime, has_time) {
        ops::delete_time(cx);
    }
    if item(ui, "Duplicate time", Command::DuplicateTime, has_time) {
        ops::duplicate_time(cx);
    }
    if item(ui, "Insert time", Command::InsertTime, has_time) {
        ops::insert_time(cx);
    }
    if item(ui, "Copy time", Command::CopyTime, has_time) {
        ops::copy_time(cx);
    }
    if item(ui, "Paste time", Command::PasteTime, holds_time) {
        ops::paste_time(cx, trill_core::PasteMode::Insert);
    }
}

/// The field over a clip's name strip while it is being renamed there.
fn rename_field(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let Some(InlineClip(id)) = cx.state.renaming_clip_inline.as_ref().map(|edit| edit.target) else { return };
    let placed = cx.snap.clip(id).and_then(|(track, clip)| {
        let row = geometry.rows.track(cx.snap.track_index(track.id)?)?;
        Some((clip, geometry.clip_rect(row, clip)))
    });
    // The clip is gone, or folded away with its group: nothing to type on.
    let Some((clip, rect)) = placed.filter(|(_, rect)| rect.intersects(geometry.lanes)) else {
        cx.state.renaming_clip_inline = None;
        return;
    };
    // Wide enough to type in even on a short clip, and never off screen.
    let lanes = geometry.lanes;
    let width = rect.width().clamp(140.0, 320.0).min(lanes.width());
    let left = rect.left().clamp(lanes.left(), lanes.right() - width);
    let top = rect.top().clamp(lanes.top(), lanes.bottom() - 20.0);
    let field = Rect::from_min_size(pos2(left, top), vec2(width, 20.0));
    let target = InlineClip(id);
    if let Some(name) = widgets::name_field(ui, cx.theme, field, target, &clip.name, &mut cx.state.renaming_clip_inline)
    {
        cx.edit(|project| {
            project.clip_mut(id)?.name = name;
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_group_of_clips_keeps_its_lanes_and_stays_in_the_list() {
        // Clips on lanes 1 and 3 of five.
        assert_eq!(lane_shift(&[1, 3], 5, 1), 1);
        assert_eq!(lane_shift(&[1, 3], 5, 4), 1, "the lower clip stops at the last lane");
        assert_eq!(lane_shift(&[1, 3], 5, -4), -1, "the upper clip stops at the first");
        assert_eq!(lane_shift(&[1, 3], 5, 0), 0);
        assert_eq!(lane_shift(&[0], 1, 3), 0, "one lane: nowhere to go");
        assert_eq!(lane_shift(&[], 5, 3), 0);
        // A clip whose track was folded away since the drag began counts as
        // being past the end.
        assert_eq!(lane_shift(&[2, 7], 4, 1), 0);
    }
}
