//! The takes of a track, unfolded under it, and the take that is being
//! recorded.
//!
//! A take is a recording kept beside a track and never played from there
//! ([`trill_core::Take`]). Unfolded, each lane of a track's takes is a row
//! under it, and every take is drawn as the clip it is: its name and the
//! waveform of the stretch of its recording it holds. What the track plays
//! of a take stands out in the track's color; the rest steps back.
//!
//! What the pointer does here is the comp:
//!
//! * dragging across a take marks a stretch of it, and letting go puts
//!   that stretch on the track, in place of what the track had there;
//! * a double-click puts the whole take there;
//! * the menu hears a take in place, names it, and lets go of it.
//!
//! Each is one edit of the song ([`Project::comp_take`] and its
//! neighbors), so each is one undo step, and nothing that was on the track
//! is lost by it: what a comp covers is kept as a take first. Letting go
//! of a take leaves its recording on disk.
//!
//! While a take is recorded its clip grows on the track itself, with the
//! waveform the writer has seen of it: nothing reads a file that is being
//! written.

use super::clips::{self, ClipLook};
use super::rows::{Row, RowKind};
use super::{Geometry, headers};
use crate::ui::snapshot::Grown;
use crate::ui::state::{Cx, FocusArea, TakeDrag, TextEditState};
use crate::ui::{colors, widgets};
use eframe::egui::{Align2, Mesh, PointerButton, Rect, Sense, Shape, Ui, pos2, vec2};
use std::sync::Arc;
use trill_core::{Beats, Clip, ClipId, Take, TempoMap, Track, TrackId};
use trill_engine::COLUMN_FRAMES;
use trill_noir::{Chip, MenuItem, fade, paint, text};

/// Height of a lane of takes: enough for a name and a waveform.
pub(in crate::ui) const LANE_HEIGHT: f32 = 40.0;
/// A stretch shorter than this, in pixels, is a click and not a drag.
const SHORTEST_DRAG: f32 = 3.0;

/// Whether the takes of `track` are unfolded.
pub(in crate::ui) fn open(cx: &Cx, track: &Track) -> bool {
    !track.takes.is_empty() && cx.state.arrangement.takes_open.contains(&track.id)
}

/// Unfolds the takes of `track`, or folds them away.
pub(in crate::ui) fn toggle(cx: &mut Cx, track: TrackId) {
    let open = &mut cx.state.arrangement.takes_open;
    if !open.remove(&track) {
        open.insert(track);
    }
}

/// Unfolds or folds the takes of the selected track, and says so where it
/// has none.
pub fn toggle_selected(cx: &mut Cx) {
    let Some(track) = cx.state.selected_track.and_then(|id| cx.snap.track(id)) else { return };
    if track.takes.is_empty() {
        cx.state
            .toasts
            .info(format!("{} has no takes: they are kept here once an input is recorded on it", track.name));
        return;
    }
    toggle(cx, track.id);
}

/// How many takes `track` keeps, in words.
pub(in crate::ui) fn counted(track: &Track) -> String {
    match track.takes.len() {
        1 => "1 take".to_owned(),
        count => format!("{count} takes"),
    }
}

/// The stretches of `take` that `track` plays: where a clip of the track
/// plays the same recording at the same place.
pub(in crate::ui) fn on_track(track: &Track, take: &Clip, tempo: &TempoMap) -> Vec<(Beats, Beats)> {
    let Some(recording) = take.audio() else { return Vec::new() };
    let plays = |clip: &Clip| {
        let audio = clip.audio().filter(|audio| audio.path == recording.path)?;
        let (from, to) = (clip.start.max(take.start), clip.end().min(take.end()));
        if to <= from {
            return None;
        }
        // The same moment of the recording at the same moment of the song.
        let here = audio.second_at(tempo, clip.start, from);
        let there = recording.second_at(tempo, take.start, from);
        ((here - there).abs() < 0.002).then_some((from, to))
    };
    track.clips.iter().filter_map(plays).collect()
}

/// The take of `lane` that lies over `beat`.
fn take_at(track: &Track, lane: u32, beat: Beats) -> Option<&Take> {
    track.takes.iter().find(|take| take.lane == lane && take.clip.start <= beat && beat < take.clip.end())
}

/// The take a lane's header speaks for: the one under the edit cursor, or
/// the lane's first.
fn spoken_for(track: &Track, lane: u32, cursor: Beats) -> Option<&Take> {
    take_at(track, lane, cursor).or_else(|| track.takes.iter().find(|take| take.lane == lane))
}

fn choose(cx: &mut Cx, track: TrackId, take: ClipId) {
    if let Some(clip) = cx.edit(|project| project.choose_take(take)) {
        cx.state.select_clip(track, clip);
    }
}

fn hear(cx: &mut Cx, take: Option<ClipId>) {
    cx.try_session(|session| session.hear_take(take));
}

fn remove(cx: &mut Cx, take: ClipId) {
    if cx.live.input.heard_take == Some(take) {
        hear(cx, None);
    }
    if cx.edit(|project| project.remove_take(take).map(drop)).is_some() {
        cx.state.toasts.info("The take is gone from the song; its recording stays on disk");
    }
}

/// The header of a lane of takes: which take it holds, and the two things
/// done most with it.
pub(super) fn header(ui: &mut Ui, cx: &mut Cx, track: &Track, lane: u32, rect: Rect) {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, theme.shade);
    paint::hline_above(&painter, rect.x_range(), rect.bottom(), theme.hairline);
    let color = colors::track_color(theme, track.color);
    let strip = Rect::from_min_max(rect.min, pos2(rect.left() + headers::HEADER_STRIP, rect.bottom() - 1.0));
    painter.rect_filled(strip, 0.0, fade(color, 0.35));
    let id = ui.scope_id().with(("take-lane", track.id, lane));
    let quiet = theme.quiet_on(theme.shade);
    let Some(take) = spoken_for(track, lane, cx.state.arrangement.cursor) else { return };
    let (take_id, name) = (take.clip.id, take.clip.name.clone());
    let heard = cx.live.input.heard_take == Some(take_id);

    let chip_size = headers::HEADER_CHIP;
    let right = rect.right() - headers::HEADER_RIGHT;
    let top = rect.center().y - chip_size.y / 2.0;
    let use_chip = Chip::action("Use").quiet(quiet).named("Put this take on the track");
    let use_width = use_chip.width(ui, theme);
    let use_rect = Rect::from_min_size(pos2(right - use_width, top), vec2(use_width, chip_size.y));
    let hear_name = format!("Hear {name} in place");
    let hear_chip = Chip::new("Hear", heard).accent(theme.signal).quiet(quiet).named(&hear_name);
    let hear_width = hear_chip.width(ui, theme);
    let hear_rect = Rect::from_min_size(pos2(use_rect.left() - 3.0 - hear_width, top), vec2(hear_width, chip_size.y));
    let room = Rect::from_min_max(
        pos2(rect.left() + headers::HEADER_LEFT + headers::HEADER_INDEX, rect.top()),
        pos2(hear_rect.left() - 6.0, rect.bottom()),
    );
    text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, &name, theme.ink_dim);

    let response = trill_noir::chip(ui, theme, hear_rect, id.with("hear"), &hear_chip);
    let about = match heard {
        true => "The track plays this take in place of what it has \u{00B7} click to go back to the song",
        false => "Hear this take in place of what the track plays, without changing the song",
    };
    cx.hint(&response, about);
    if response.clicked() {
        hear(cx, (!heard).then_some(take_id));
    }
    let response = trill_noir::chip(ui, theme, use_rect, id.with("use"), &use_chip);
    cx.hint(&response, "Put the whole take on the track \u{00B7} what it covers is kept as a take");
    if response.clicked() {
        choose(cx, track.id, take_id);
    }
}

/// The lanes of takes that are in view, and what the pointer does there.
pub(super) fn show(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let snapshot = cx.snap;
    for row in geometry.rows.rows.iter().filter(|row| geometry.is_visible(row)) {
        if let RowKind::Take { track, lane } = row.kind {
            lane_row(ui, cx, geometry, row, &snapshot.project.tracks[track], lane);
        }
    }
    paint_growing(ui, cx, geometry);
    rename_field(ui, cx, geometry);
}

fn lane_row(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry, row: &Row, track: &Track, lane: u32) {
    let theme = cx.theme;
    let whole = geometry.row_rect(row);
    let rect = whole.intersect(geometry.lanes);
    if rect.height() <= 0.0 {
        return;
    }
    let id = ui.scope_id().with(("takes", track.id, lane));
    // Registered after the lanes' own response, so it takes the pointer.
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let painter = ui.painter_at(geometry.lanes);
    painter.rect_filled(rect, 0.0, theme.shade);
    paint::hline_above(&painter, geometry.lanes.x_range(), whole.bottom(), theme.hairline);

    let color = colors::track_color(theme, track.color);
    let tempo = cx.snap.project.tempo_map();
    let heard = cx.live.input.heard_take;
    let renaming = cx.state.arrangement.renaming_take.as_ref().map(|edit| edit.target);
    let mut notes = Mesh::default();
    for take in track.takes.iter().filter(|take| take.lane == lane) {
        let clip = &take.clip;
        let clip_rect = geometry.clip_rect(row, clip);
        if !clip_rect.intersects(geometry.lanes) {
            continue;
        }
        let is_heard = heard == Some(clip.id);
        let look = ClipLook {
            color,
            ghosted: !is_heard,
            name_from: clip_rect.left(),
            handles: false,
            renaming: renaming == Some(clip.id),
        };
        clips::paint_clip(&painter, cx, &mut notes, clip_rect, geometry, clip, look);
        // What the track plays of it, in the track's color.
        for (from, to) in on_track(track, clip, &tempo) {
            let (left, right) =
                (geometry.x(from).round(), (geometry.x(to).round() - 1.0).max(geometry.x(from).round()));
            let part = Rect::from_x_y_ranges(left..=right, clip_rect.y_range()).intersect(geometry.lanes);
            if part.width() > 0.0 {
                let lit = ClipLook { ghosted: false, ..look };
                clips::paint_clip(&painter.with_clip_rect(part), cx, &mut notes, clip_rect, geometry, clip, lit);
            }
        }
        if is_heard {
            paint::frame(&painter, clip_rect.intersect(geometry.lanes), theme.signal);
        }
    }

    // A stretch being marked: here, and where it will land on the track.
    if let Some(drag) =
        cx.state.arrangement.take_drag.filter(|drag| track.take(drag.take).is_some_and(|t| t.lane == lane))
    {
        let (from, to) = drag.span();
        let (left, right) = (geometry.x(from).round(), geometry.x(to).round());
        let marked = Rect::from_x_y_ranges(left..=right, rect.y_range());
        painter.rect_filled(marked, 0.0, theme.selected);
        paint::vline(&painter, left, rect.y_range(), theme.ink);
        paint::vline_left_of(&painter, right, rect.y_range(), theme.ink);
        if let Some(above) = cx.snap.track_index(track.id).and_then(|index| geometry.rows.track(index)) {
            let landing = geometry.row_rect(above);
            let landing = Rect::from_x_y_ranges(left..=right, landing.top() + 1.0..=landing.bottom() - 2.0);
            painter.rect_filled(landing, 0.0, fade(color, 0.25));
            paint::frame(&painter, landing, theme.ink);
        }
    }

    cx.hint(
        &response,
        "Drag across a take to put that stretch on the track \u{00B7} double-click puts the whole take there \u{00B7} \
         right-click for more",
    );
    let free = ui.input(|input| input.modifiers.alt);
    let pointer = response.interact_pointer_pos();
    let within = |take: &Take, beat: Beats| geometry.snap(beat, free).clamp(take.clip.start, take.clip.end());
    if response.drag_started_by(PointerButton::Primary)
        && let Some(pointer) = pointer
    {
        cx.state.focus = FocusArea::Arrangement;
        cx.state.select_track(track.id);
        // Where the button went down, not where the drag was noticed.
        let origin = ui.input(|input| input.pointer.press_origin()).unwrap_or(pointer);
        let beat = geometry.beat(origin.x);
        cx.state.arrangement.take_drag = take_at(track, lane, beat).map(|take| {
            let anchor = within(take, beat);
            TakeDrag { take: take.clip.id, anchor, to: anchor }
        });
    }
    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = pointer
        && let Some(drag) = cx.state.arrangement.take_drag
        && let Some(take) = track.take(drag.take)
    {
        let to = within(take, geometry.beat(pointer.x));
        cx.state.arrangement.take_drag = Some(TakeDrag { to, ..drag });
    }
    if response.drag_stopped()
        && let Some(drag) = cx.state.arrangement.take_drag.take()
    {
        let (from, to) = drag.span();
        if (geometry.x(to) - geometry.x(from)).abs() >= SHORTEST_DRAG
            && let Some(clip) = cx.edit(|project| project.comp_take(drag.take, from, to))
        {
            cx.state.select_clip(track.id, clip);
        }
    }
    if response.clicked() {
        cx.state.focus = FocusArea::Arrangement;
        cx.state.select_track(track.id);
        if let Some(pointer) = pointer {
            cx.state.arrangement.cursor = geometry.snap(geometry.beat(pointer.x), free);
        }
    }
    if widgets::double_clicked(&response)
        && let Some(pointer) = pointer
        && let Some(take) = take_at(track, lane, geometry.beat(pointer.x))
    {
        choose(cx, track.id, take.clip.id);
    }

    // The menu of the take that was right-clicked.
    if response.secondary_clicked()
        && let Some(pointer) = pointer
    {
        cx.state.select_track(track.id);
        let subject = take_at(track, lane, geometry.beat(pointer.x)).map(|take| take.clip.id);
        ui.data_mut(|data| match subject {
            Some(subject) => drop(data.insert_temp(response.id, subject)),
            None => data.remove::<ClipId>(response.id),
        });
    }
    let subject = ui.data(|data| data.get_temp::<ClipId>(response.id));
    let (track_id, name) = (track.id, track.name.clone());
    response.context_menu(|ui| {
        ui.set_min_width(230.0);
        if let Some(take) = subject {
            let is_heard = heard == Some(take);
            let detail = "In place of what the track has there, which is kept as a take";
            if MenuItem::new("Put the whole take on the track").detail(detail).show(ui, theme).clicked() {
                choose(cx, track_id, take);
            }
            let label = if is_heard { "Go back to the song" } else { "Hear it in place" };
            let detail = "Without changing the song";
            if MenuItem::new(label).checked(is_heard).detail(detail).show(ui, theme).clicked() {
                hear(cx, (!is_heard).then_some(take));
            }
            if MenuItem::new("Rename").show(ui, theme).clicked() {
                let text = cx.snap.project.take(take).map(|(_, take)| take.clip.name.clone()).unwrap_or_default();
                cx.state.arrangement.renaming_take = Some(TextEditState { target: take, text, focused: false });
            }
            trill_noir::menu_separator(ui, theme);
            let detail = "Its recording stays on disk";
            if MenuItem::new("Let go of the take").detail(detail).show(ui, theme).clicked() {
                remove(cx, take);
            }
            trill_noir::menu_separator(ui, theme);
        }
        if MenuItem::new(&format!("Fold the takes of {name} away")).show(ui, theme).clicked() {
            toggle(cx, track_id);
        }
    });
}

/// The take whose name is being typed, over its strip.
fn rename_field(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let Some(id) = cx.state.arrangement.renaming_take.as_ref().map(|edit| edit.target) else { return };
    let snapshot = cx.snap;
    let placed = snapshot.project.take(id).ok().and_then(|(track, take)| {
        let index = snapshot.track_index(track.id)?;
        let lane = take.lane;
        let row = geometry.rows.rows.iter().find(|row| row.kind == RowKind::Take { track: index, lane })?;
        Some((&take.clip, geometry.clip_rect(row, &take.clip)))
    });
    // The take is gone, or its lanes were folded away: nothing to type on.
    let Some((clip, rect)) = placed.filter(|(_, rect)| rect.intersects(geometry.lanes)) else {
        cx.state.arrangement.renaming_take = None;
        return;
    };
    let lanes = geometry.lanes;
    let width = rect.width().clamp(140.0, 320.0).min(lanes.width());
    let left = rect.left().clamp(lanes.left(), lanes.right() - width);
    let top = rect.top().clamp(lanes.top(), lanes.bottom() - 20.0);
    let field = Rect::from_min_size(pos2(left, top), vec2(width, 20.0));
    if let Some(name) =
        widgets::name_field(ui, cx.theme, field, id, &clip.name, &mut cx.state.arrangement.renaming_take)
    {
        cx.edit(|project| project.rename_take(id, name));
    }
}

/// The lowest and the highest sample `grown` has between two moments of
/// its recording, in seconds from the frame that was sung to the first
/// beat of the pass; `None` where nothing was written yet.
fn band(grown: &Grown, from: f64, to: f64) -> Option<(f32, f32)> {
    let column = |seconds: f64| (grown.first_frame as f64 + seconds * grown.sample_rate) / COLUMN_FRAMES as f64;
    let first = column(from).floor().max(0.0) as usize;
    let last = (column(to).ceil().max(0.0) as usize).max(first + 1).min(grown.columns.len());
    let columns = grown.columns.get(first..last)?;
    columns.iter().fold(None, |band: Option<(f32, f32)>, [low, high]| {
        Some(band.map_or((*low, *high), |(lowest, highest)| (lowest.min(*low), highest.max(*high))))
    })
}

/// The takes that are being recorded, each on its track: a clip that
/// grows with the song, with the waveform the writer has seen so far.
fn paint_growing(ui: &Ui, cx: &Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let lanes = geometry.lanes;
    let painter = ui.painter_at(lanes);
    let tempo = cx.snap.project.tempo_map();
    for grown in &cx.live.input.growing {
        let Some(start) = grown.start_beat else { continue };
        let Some(row) = cx.snap.track_index(grown.track).and_then(|index| geometry.rows.track(index)) else {
            continue;
        };
        let end = cx.live.position.max(start);
        let row_rect = geometry.row_rect(row);
        let (left, right) = (geometry.x(start).round(), geometry.x(end).round().max(geometry.x(start).round() + 1.0));
        let rect = Rect::from_min_max(pos2(left, row_rect.top() + 1.0), pos2(right, row_rect.bottom() - 2.0));
        if !rect.intersects(lanes) {
            continue;
        }
        // Over whatever the track has there: that is not heard meanwhile.
        painter.rect_filled(rect, 0.0, theme.bg);
        painter.rect_filled(rect, 0.0, fade(theme.signal, 0.14));
        let title = clips::title_rect(rect);
        painter.rect_filled(title, 0.0, theme.signal);
        if rect.width() > 60.0 {
            let room = Rect::from_min_max(pos2(rect.left().max(lanes.left()) + 4.0, title.top()), title.max);
            let words = if grown.failed.is_some() { "Recording stopped" } else { "Recording" };
            text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, words, theme.on_accent);
        }
        let content = Rect::from_min_max(pos2(rect.left(), title.bottom()), rect.max).intersect(lanes);
        if content.width() <= 0.0 || content.height() <= 2.0 {
            continue;
        }
        let area = content.shrink2(vec2(0.0, 2.0f32.min(content.height() / 6.0)));
        let count = trill_noir::waveform_columns(&painter, area.width()).max(1);
        let step = area.width() / count as f32;
        let seconds = |x: f32| tempo.seconds_between(start, geometry.beat(x));
        let bands: Vec<(f32, f32)> = (0..count)
            .map(|index| {
                let x = area.left() + index as f32 * step;
                band(grown, seconds(x), seconds(x + step)).unwrap_or((0.0, 0.0))
            })
            .collect();
        let mut mesh = Mesh::default();
        trill_noir::waveform(&mut mesh, area, &bands, 1.0, fade(theme.signal, 0.9), paint::hair(&painter));
        painter.with_clip_rect(content).add(Shape::mesh(Arc::new(mesh)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::Project;
    use trill_core::project::AudioContent;

    fn song() -> (Project, TrackId, ClipId, ClipId) {
        let mut project = Project::new("t");
        let voice = project.add_track("Voice");
        let first = project.add_take(voice, "Take 1", 4.0, 8.0, AudioContent::new("a.wav")).unwrap();
        let second = project.add_take(voice, "Take 2", 4.0, 8.0, AudioContent::new("b.wav")).unwrap();
        (project, voice, first, second)
    }

    #[test]
    fn what_a_track_plays_of_a_take_is_found_by_its_recording_and_its_place() {
        let (mut project, voice, first, second) = song();
        project.choose_take(second).unwrap();
        project.comp_take(first, 6.0, 8.0).unwrap();
        let tempo = project.tempo_map();
        let track = project.track(voice).unwrap();
        let of = |take: ClipId| on_track(track, &track.take(take).unwrap().clip, &tempo);
        assert_eq!(of(first), [(6.0, 8.0)]);
        assert_eq!(of(second), [(4.0, 6.0), (8.0, 12.0)]);

        // A clip of the same recording that was moved plays another moment
        // of it there: that is not the take's stretch.
        let mut moved = project.clone();
        let clip = moved.track(voice).unwrap().clips[0].id;
        moved.move_clip(clip, None, 4.5).unwrap();
        let track = moved.track(voice).unwrap();
        let found = on_track(track, &track.take(second).unwrap().clip, &tempo);
        assert_eq!(found, [(8.0, 12.0)], "{found:?}");
    }

    #[test]
    fn a_lane_names_the_take_under_the_cursor_or_its_first() {
        let (mut project, voice, first, _) = song();
        let later = project.add_take(voice, "Later", 20.0, 4.0, AudioContent::new("c.wav")).unwrap();
        let track = project.track(voice).unwrap();
        assert_eq!(track.take(later).unwrap().lane, 0, "it has room in the first lane");
        assert_eq!(take_at(track, 0, 5.0).map(|take| take.clip.id), Some(first));
        assert_eq!(take_at(track, 0, 12.0).map(|take| take.clip.id), None, "a take ends before its last beat");
        assert_eq!(spoken_for(track, 0, 21.0).map(|take| take.clip.id), Some(later));
        assert_eq!(spoken_for(track, 0, 15.0).map(|take| take.clip.id), Some(first));
        assert_eq!(counted(track), "3 takes");
    }

    #[test]
    fn a_growing_take_is_drawn_from_what_the_writer_has_seen() {
        let columns: Vec<[f32; 2]> = (0..100).map(|index| [-(index as f32) / 100.0, index as f32 / 100.0]).collect();
        let grown = Grown { sample_rate: 48_000.0, first_frame: 2_560, columns, ..Grown::default() };
        // Column 10 is where the pass begins; a column is 256 frames.
        let column = COLUMN_FRAMES as f64 / 48_000.0;
        assert_eq!(band(&grown, 0.0, column), Some((-0.10, 0.10)));
        assert_eq!(band(&grown, 10.0 * column, 13.0 * column), Some((-0.22, 0.22)));
        assert_eq!(band(&grown, 95.0 * column, 96.0 * column), None, "nothing was written that far yet");
    }
}
