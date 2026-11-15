//! The arrangement: track headers on the left, a ruler on top and the clips
//! of every track on a shared timeline.
//!
//! Everything is placed by hand from a [`Geometry`] computed once per frame,
//! and painted from the snapshot. Edits go to the session and show up on the
//! next frame, when the snapshot has caught up.

use super::state::{ArrangeDrag, Cx, DetailTab, FocusArea, TextEditState};
use super::timeline::{self, GridLevel, SpanHit, TimeView};
use super::widgets::{self, Icon};
use super::{theme, units};
use daw_core::project::{ClipContent, LoopRegion};
use daw_core::{Beats, ChainRef, Clip, ClipId, TimeSignature, Track, TrackId};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Mesh, PointerButton, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui,
    UiBuilder, pos2, vec2,
};

pub(super) const HEADER_WIDTH: f32 = 272.0;
pub(super) const RULER_HEIGHT: f32 = 30.0;
/// The top of the ruler, which holds the loop brace.
const LOOP_STRIP: f32 = 11.0;
pub(super) const TRACK_HEIGHT: f32 = 72.0;
/// Where a track header's content starts, right of the color strip.
pub(super) const HEADER_PADDING: f32 = 16.0;
/// Top of the fader and pan row inside a track header.
pub(super) const HEADER_CONTROLS_TOP: f32 = 40.0;
pub(super) const HEADER_FADER_WIDTH: f32 = 134.0;
/// Room below the last track for the "add track" button.
const ADD_ROW_HEIGHT: f32 = 48.0;
const CLIP_TITLE: f32 = 16.0;
/// Width of the zone at a clip's end that resizes it.
const RESIZE_HANDLE: f32 = 8.0;
/// How close, in pixels, the pointer must be to an end of the loop brace to
/// grab it.
const LOOP_END_GRAB: f32 = 6.0;
/// Grid lines closer together than this are dropped.
const GRID_MIN_PIXELS: f64 = 18.0;
/// The instrument new tracks get, so they make sound immediately.
const DEFAULT_INSTRUMENT: &str = "builtin:polysynth";

/// Where everything is this frame.
#[derive(Copy, Clone, Debug)]
struct Geometry {
    corner: Rect,
    ruler: Rect,
    headers: Rect,
    lanes: Rect,
    time: TimeView,
    scroll_y: f32,
    track_count: usize,
    signature: TimeSignature,
    beats_per_bar: Beats,
    /// Spacing of the finest grid lines drawn, in beats.
    grid: Beats,
}

impl Geometry {
    fn new(area: Rect, time: TimeView, scroll_y: f32, track_count: usize, signature: TimeSignature) -> Self {
        let split_x = area.left() + HEADER_WIDTH;
        let split_y = area.top() + RULER_HEIGHT;
        Self {
            corner: Rect::from_min_max(area.min, pos2(split_x, split_y)),
            ruler: Rect::from_min_max(pos2(split_x, area.top()), pos2(area.right(), split_y)),
            headers: Rect::from_min_max(pos2(area.left(), split_y), pos2(split_x, area.bottom())),
            lanes: Rect::from_min_max(pos2(split_x, split_y), area.max),
            time,
            scroll_y,
            track_count,
            signature,
            beats_per_bar: signature.beats_per_bar(),
            grid: timeline::grid_step(time.pixels_per_beat, signature, GRID_MIN_PIXELS),
        }
    }

    fn x(&self, beat: Beats) -> f32 {
        self.time.beat_to_x(self.lanes.left(), beat)
    }

    fn beat(&self, x: f32) -> Beats {
        self.time.x_to_beat(self.lanes.left(), x)
    }

    fn row_top(&self, index: usize) -> f32 {
        self.lanes.top() - self.scroll_y + index as f32 * TRACK_HEIGHT
    }

    /// The track row at screen position `y`, if there is one there.
    fn row_at(&self, y: f32) -> Option<usize> {
        let offset = y - self.lanes.top() + self.scroll_y;
        let index = (offset / TRACK_HEIGHT).floor();
        (offset >= 0.0 && (index as usize) < self.track_count).then_some(index as usize)
    }

    /// Like [`Self::row_at`], but positions above or below the tracks map
    /// to the first or last one. For drags, which should not let go of a
    /// clip just because the pointer left the lanes.
    fn nearest_row(&self, y: f32) -> usize {
        let offset = y - self.lanes.top() + self.scroll_y;
        ((offset / TRACK_HEIGHT).floor().max(0.0) as usize).min(self.track_count.saturating_sub(1))
    }

    /// Height of everything that scrolls: the tracks and the add button.
    fn content_height(&self) -> f32 {
        self.track_count as f32 * TRACK_HEIGHT + ADD_ROW_HEIGHT
    }

    fn max_scroll(&self) -> f32 {
        (self.content_height() - self.lanes.height()).max(0.0)
    }

    /// Snaps a beat to the grid the user sees, at most a bar coarse. Zoomed
    /// far out, grid lines are several bars apart, but clips should still
    /// land on any bar.
    fn snap(&self, beat: Beats, free: bool) -> Beats {
        if free {
            return beat.max(0.0);
        }
        timeline::snap_to_bar_grid(beat, self.beats_per_bar, self.grid.min(self.beats_per_bar))
    }

    /// The shortest clip a resize may leave behind.
    fn min_clip_length(&self, free: bool) -> Beats {
        if free { 1.0 / 16.0 } else { self.grid.min(self.beats_per_bar) }
    }

    /// Screen rectangle of a clip on track row `row`, before clipping.
    fn clip_rect(&self, row: usize, clip: &Clip) -> Rect {
        let top = self.row_top(row);
        Rect::from_min_max(pos2(self.x(clip.start), top + 3.0), pos2(self.x(clip.end()), top + TRACK_HEIGHT - 4.0))
    }
}

/// What the pointer is over in the lanes.
#[derive(Copy, Clone, Debug, PartialEq)]
struct ClipHit {
    track: TrackId,
    clip: ClipId,
    part: SpanHit,
    start: Beats,
}

fn clip_at(tracks: &[Track], geometry: &Geometry, position: Pos2) -> Option<ClipHit> {
    let track = tracks.get(geometry.row_at(position.y)?)?;
    // Later clips are painted over earlier ones, so they are hit first.
    track.clips.iter().rev().find_map(|clip| {
        let part = timeline::span_hit(position.x, geometry.x(clip.start), geometry.x(clip.end()), RESIZE_HANDLE)?;
        Some(ClipHit { track: track.id, clip: clip.id, part, start: clip.start })
    })
}

/// Which part of the loop brace is at `position`, if any.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LoopHit {
    Start,
    End,
    Body,
}

fn loop_hit(region: LoopRegion, geometry: &Geometry, position: Pos2) -> Option<LoopHit> {
    if position.y > geometry.ruler.top() + LOOP_STRIP + 2.0 {
        return None;
    }
    let (start, end) = (geometry.x(region.start), geometry.x(region.end));
    if (position.x - end).abs() <= LOOP_END_GRAB {
        Some(LoopHit::End)
    } else if (position.x - start).abs() <= LOOP_END_GRAB {
        Some(LoopHit::Start)
    } else if position.x > start && position.x < end {
        Some(LoopHit::Body)
    } else {
        None
    }
}

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let area = ui.max_rect();
    ui.painter().rect_filled(area, 0.0, theme::BG_INSET);
    if cx.snap.project.tracks.is_empty() {
        empty_state(ui, cx, area);
        return;
    }

    let signature = cx.snap.project.time_signature;
    let track_count = cx.snap.project.tracks.len();
    let view = &mut cx.state.arrangement;
    if !view.framed {
        // Open a project showing all of it, or a few bars of a new one.
        view.framed = true;
        let end = cx.snap.project.length_beats().max(8.0 * signature.beats_per_bar());
        view.time.fit(0.0, end, area.width() - HEADER_WIDTH);
        view.time.origin = 0.0;
    }
    let mut geometry = Geometry::new(area, view.time, view.scroll_y, track_count, signature);
    // A drag whose button was released outside the window never reports
    // that it stopped.
    if view.drag.is_some() && !ui.input(|input| input.pointer.primary_down()) {
        view.drag = None;
    }

    scroll_and_zoom(ui, cx, &mut geometry);
    follow_playhead(cx, &mut geometry);

    lanes(ui, cx, &geometry);
    ruler(ui, cx, &geometry);
    playhead(ui, cx, &geometry);
    headers(ui, cx, &geometry);
    corner(ui, cx, &geometry);
    scroll_bar(ui, cx, &geometry);
}

/// Wheel pans along the timeline, Ctrl+wheel zooms around the pointer, and
/// Alt+wheel (or the wheel over the headers) scrolls the track list.
fn scroll_and_zoom(ui: &Ui, cx: &mut Cx, geometry: &mut Geometry) {
    let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) else { return };
    let over_timeline = geometry.lanes.contains(pointer) || geometry.ruler.contains(pointer);
    let over_headers = geometry.headers.contains(pointer);
    // The second test fails when a menu or dialog covers the arrangement.
    if !(over_timeline || over_headers) || !ui.rect_contains_pointer(geometry.corner.union(geometry.lanes)) {
        return;
    }
    let (scroll, zoom, alt) = ui.input(|input| (input.smooth_scroll_delta, input.zoom_delta(), input.modifiers.alt));
    let view = &mut cx.state.arrangement;
    if over_timeline && zoom != 1.0 {
        view.time.zoom_around(geometry.lanes.left(), pointer.x, zoom as f64);
    }
    if over_headers || alt {
        view.scroll_y -= scroll.y;
    } else {
        // A plain wheel reports vertical motion; pads and Shift+wheel
        // report horizontal. Either pans the timeline.
        view.time.pan_pixels(-(scroll.x + scroll.y));
    }
    view.scroll_y = view.scroll_y.clamp(0.0, geometry.max_scroll());
    geometry.time = view.time;
    geometry.scroll_y = view.scroll_y;
    geometry.grid = timeline::grid_step(view.time.pixels_per_beat, geometry.signature, GRID_MIN_PIXELS);
}

/// Pages the view forward when playback runs off its right edge.
fn follow_playhead(cx: &mut Cx, geometry: &mut Geometry) {
    if !cx.live.playing || cx.state.arrangement.drag.is_some() {
        return;
    }
    let x = geometry.x(cx.live.position);
    if x > geometry.lanes.right() - 24.0 || x < geometry.lanes.left() {
        let lead_in = geometry.lanes.width() as f64 * 0.08 / geometry.time.pixels_per_beat;
        cx.state.arrangement.time.origin = (cx.live.position - lead_in).max(0.0);
        geometry.time = cx.state.arrangement.time;
    }
}

// ---- lanes -----------------------------------------------------------------

fn lanes(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let response = ui.interact(geometry.lanes, ui.scope_id().with("lanes"), Sense::click_and_drag());
    paint_lanes(ui, cx, geometry);
    lane_interaction(ui, cx, geometry, &response);
}

fn paint_lanes(ui: &Ui, cx: &Cx, geometry: &Geometry) {
    let painter = ui.painter_at(geometry.lanes);
    let lanes = geometry.lanes;
    let (first_beat, last_beat) = geometry.time.visible(lanes.width());

    // Alternate bars (or groups of bars, zoomed out) are tinted.
    let group = geometry.grid.max(geometry.beats_per_bar);
    let mut index = (first_beat / group).floor();
    while index * group < last_beat {
        if index as i64 % 2 == 1 {
            let band =
                Rect::from_x_y_ranges(geometry.x(index * group)..=geometry.x((index + 1.0) * group), lanes.y_range());
            painter.rect_filled(band, 0.0, theme::LANE_ALTERNATE);
        }
        index += 1.0;
    }

    for (row, track) in cx.snap.project.tracks.iter().enumerate() {
        let top = geometry.row_top(row);
        if cx.state.selected_track == Some(track.id) {
            let band = Rect::from_x_y_ranges(lanes.x_range(), top..=top + TRACK_HEIGHT);
            painter.rect_filled(band, 0.0, theme::fade(Color32::WHITE, 0.022));
        }
    }

    timeline::for_each_grid_line(first_beat, last_beat, geometry.signature, geometry.grid, |beat, level| {
        let color = match level {
            GridLevel::Bar => theme::GRID_BAR,
            GridLevel::Beat => theme::GRID_BEAT,
            GridLevel::Subdivision => theme::GRID_SUBDIVISION,
        };
        painter.vline(geometry.x(beat).round() + 0.5, lanes.y_range(), theme::hairline(color));
    });

    let region = cx.snap.project.loop_region;
    if region.enabled {
        let band = Rect::from_x_y_ranges(geometry.x(region.start)..=geometry.x(region.end), lanes.y_range());
        painter.rect_filled(band, 0.0, theme::fade(theme::ACCENT, 0.05));
        for x in [band.left(), band.right()] {
            painter.vline(x.round() + 0.5, lanes.y_range(), theme::hairline(theme::fade(theme::ACCENT, 0.45)));
        }
    }

    // All note miniatures go into one mesh: thousands of tiny rectangles
    // cost one draw call instead of one shape each.
    let mut notes = Mesh::default();
    for (row, track) in cx.snap.project.tracks.iter().enumerate() {
        let top = geometry.row_top(row);
        if top > lanes.bottom() || top + TRACK_HEIGHT < lanes.top() {
            continue;
        }
        painter.hline(lanes.x_range(), top + TRACK_HEIGHT - 0.5, theme::hairline(theme::BORDER));
        let color = theme::track_color(track.color);
        for clip in &track.clips {
            let rect = geometry.clip_rect(row, clip);
            if rect.right() < lanes.left() || rect.left() > lanes.right() {
                continue;
            }
            let selected = cx.state.selected_clip == Some(clip.id);
            paint_clip(&painter, &mut notes, rect, lanes, geometry, clip, color, selected, track.mute);
        }
    }
    painter.add(Shape::mesh(notes));

    // Selection outlines go on top of the notes of neighboring clips.
    if let Some((track, clip)) = cx.state.selected_clip.and_then(|id| cx.snap.clip(id))
        && let Some(row) = cx.snap.track_index(track.id)
    {
        let rect = geometry.clip_rect(row, clip);
        painter.rect_stroke(rect, theme::RADIUS_SMALL, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
    }
}

#[expect(clippy::too_many_arguments, reason = "a private paint routine; a struct would only rename the arguments")]
fn paint_clip(
    painter: &egui::Painter,
    notes: &mut Mesh,
    rect: Rect,
    lanes: Rect,
    geometry: &Geometry,
    clip: &Clip,
    track_color: Color32,
    selected: bool,
    track_muted: bool,
) {
    // Muted material keeps its hue but steps back.
    let color =
        if clip.muted || track_muted { theme::mix(track_color, theme::BG_CONTROL_HOVER, 0.62) } else { track_color };
    let body = theme::mix(theme::BG_INSET, color, if selected { 0.44 } else { 0.30 });
    painter.rect_filled(rect, theme::RADIUS_SMALL, body);

    let title = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + CLIP_TITLE));
    let title_color = if selected { theme::mix(color, Color32::WHITE, 0.22) } else { color };
    let corners = egui::CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 };
    painter.rect_filled(title, corners, title_color);
    if rect.width() > 26.0 {
        // Keep the name on screen while the clip's start is scrolled away.
        let text_left = (rect.left().max(lanes.left()) + 6.0).min(rect.right() - 20.0);
        let text_area = Rect::from_min_max(pos2(text_left, title.top()), pos2(title.right() - 4.0, title.bottom()));
        widgets::text_in(
            painter,
            text_area,
            Align2::LEFT_CENTER,
            &clip.name,
            theme::strong(11.0),
            theme::TEXT_ON_COLOR,
        );
    }

    let content = Rect::from_min_max(pos2(rect.left(), title.bottom()), rect.max).intersect(lanes);
    if content.width() <= 0.0 || content.height() <= 0.0 {
        return;
    }
    match &clip.content {
        ClipContent::Midi(midi) => {
            let ink = theme::mix(color, Color32::WHITE, if selected { 0.5 } else { 0.3 });
            paint_note_miniature(painter, notes, content, geometry, clip, &midi.notes, midi.loop_length, ink);
        }
        ClipContent::Audio(audio) => paint_audio_body(painter, content, rect, &audio.path, color),
    }
}

/// Draws a clip's notes as small bars, repeated across the clip if it loops.
#[expect(clippy::too_many_arguments, reason = "a private paint routine; a struct would only rename the arguments")]
fn paint_note_miniature(
    painter: &egui::Painter,
    mesh: &mut Mesh,
    content: Rect,
    geometry: &Geometry,
    clip: &Clip,
    notes: &[daw_core::Note],
    loop_length: Option<Beats>,
    ink: Color32,
) {
    let Some((low, high)) = timeline::pitch_range(notes) else { return };
    // At least an octave of range, so a single note is not drawn as a slab.
    let span = (high - low).max(11) as f32 + 1.0;
    let area = content.shrink2(vec2(0.0, 3.0));
    let bar_height = (area.height() / span).clamp(1.5, 4.0);
    let y_of = |pitch: u8| area.bottom() - bar_height - (pitch - low) as f32 / span * (area.height() - bar_height);

    let cycle = loop_length.filter(|length| *length > 0.0 && *length < clip.length);
    // A loop only a few pixels wide would repeat thousands of times across
    // the clip without showing anything recognizable.
    if cycle.is_some_and(|length| length * geometry.time.pixels_per_beat < 3.0) {
        return;
    }
    let repeats = cycle.map_or(1, |length| (clip.length / length).ceil() as usize);
    for repeat in 0..repeats {
        let offset = clip.start + repeat as f64 * cycle.unwrap_or(0.0);
        // Each repeat plays only the first `cycle` beats, and the last one
        // is cut off by the end of the clip.
        let cut = cycle.map_or(clip.end(), |length| (offset + length).min(clip.end()));
        if geometry.x(cut) < content.left() {
            continue;
        }
        if geometry.x(offset) > content.right() {
            break;
        }
        if repeat > 0 {
            let x = geometry.x(offset).round() + 0.5;
            painter.vline(x, content.y_range(), theme::hairline(theme::fade(Color32::BLACK, 0.35)));
        }
        for note in notes {
            let start = offset + note.start;
            if start >= cut {
                // Notes are sorted by start, so the rest are cut off too.
                break;
            }
            let left = geometry.x(start);
            let right = geometry.x((start + note.length).min(cut)).max(left + 1.5);
            if right < content.left() || left > content.right() {
                continue;
            }
            let top = y_of(note.pitch);
            let bar = Rect::from_min_max(pos2(left, top), pos2(right - 0.5, top + bar_height)).intersect(content);
            if bar.width() > 0.0 {
                mesh.add_colored_rect(bar, ink);
            }
        }
    }
}

/// Audio clips show their file instead of notes: a striped body, so they
/// are told apart at a glance, and the file name.
fn paint_audio_body(painter: &egui::Painter, content: Rect, clip_rect: Rect, path: &str, color: Color32) {
    let clipped = painter.with_clip_rect(content);
    let stripe = theme::hairline(theme::fade(color, 0.28));
    let slant = content.height();
    // Stripes are anchored to the clip, not the view, so they do not crawl
    // while scrolling.
    let mut x = clip_rect.left() - slant + ((content.left() - clip_rect.left()) / 9.0).floor().max(0.0) * 9.0;
    while x < content.right() {
        clipped.line_segment([pos2(x, content.bottom()), pos2(x + slant, content.top())], stripe);
        x += 9.0;
    }
    let middle = content.center().y;
    clipped.hline(content.x_range(), middle, theme::hairline(theme::fade(color, 0.7)));

    if content.width() > 40.0 {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let font = theme::font(11.0);
        let galley = clipped.layout_no_wrap(name.to_owned(), font, theme::TEXT);
        let label = Rect::from_min_size(
            pos2(content.left() + 6.0, middle - galley.size().y / 2.0 - 2.0),
            galley.size() + vec2(10.0, 4.0),
        );
        clipped.rect_filled(label, theme::RADIUS_SMALL, theme::fade(theme::BG_INSET, 0.85));
        clipped.galley(label.min + vec2(5.0, 2.0), galley, theme::TEXT);
    }
}

fn lane_interaction(ui: &Ui, cx: &mut Cx, geometry: &Geometry, response: &egui::Response) {
    let tracks = &cx.snap.project.tracks;
    let free = ui.input(|input| input.modifiers.alt);

    if let Some(pointer) = response.hover_pos()
        && cx.state.arrangement.drag.is_none()
        && let Some(hit) = clip_at(tracks, geometry, pointer)
        && hit.part == SpanHit::End
    {
        ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
    }

    if widgets::double_clicked(response)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        match clip_at(tracks, geometry, pointer) {
            Some(hit) => {
                cx.state.select_clip(hit.track, hit.clip);
                cx.state.detail_tab = DetailTab::Clip;
            }
            None => {
                if let Some(track) = geometry.row_at(pointer.y).and_then(|row| tracks.get(row)) {
                    let bar = geometry.beats_per_bar;
                    let start = (geometry.beat(pointer.x) / bar).floor().max(0.0) * bar;
                    add_clip(cx, track.id, start);
                }
            }
        }
    } else if response.clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        cx.state.focus = FocusArea::Arrangement;
        match clip_at(tracks, geometry, pointer) {
            Some(hit) => cx.state.select_clip(hit.track, hit.clip),
            None => {
                cx.state.clear_clip_selection();
                if let Some(track) = geometry.row_at(pointer.y).and_then(|row| tracks.get(row)) {
                    cx.state.select_track(track.id);
                }
            }
        }
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|input| input.pointer.press_origin())
        && let Some(hit) = clip_at(tracks, geometry, press)
    {
        cx.state.focus = FocusArea::Arrangement;
        cx.state.select_clip(hit.track, hit.clip);
        cx.state.arrangement.drag = Some(match hit.part {
            SpanHit::Body => ArrangeDrag::MoveClip { clip: hit.clip, grab_offset: geometry.beat(press.x) - hit.start },
            SpanHit::End => ArrangeDrag::ResizeClip { clip: hit.clip },
        });
        cx.begin_gesture();
    }

    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        match cx.state.arrangement.drag {
            Some(ArrangeDrag::MoveClip { clip, grab_offset }) => {
                ui.set_cursor_icon(CursorIcon::Grabbing);
                let start = geometry.snap(geometry.beat(pointer.x) - grab_offset, free);
                let target = tracks[geometry.nearest_row(pointer.y)].id;
                move_clip(cx, clip, start, target);
            }
            Some(ArrangeDrag::ResizeClip { clip }) => {
                ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
                resize_clip(cx, clip, geometry.snap(geometry.beat(pointer.x), free), geometry.min_clip_length(free));
            }
            _ => {}
        }
    }

    clip_context_menu(ui, cx, geometry, response);
}

/// Right-click menu for a clip, or for the empty lane under the pointer.
fn clip_context_menu(ui: &Ui, cx: &mut Cx, geometry: &Geometry, response: &egui::Response) {
    let tracks = &cx.snap.project.tracks;
    // Selecting what was right-clicked makes the menu's target visible and
    // lets the menu simply act on the selection.
    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        match clip_at(tracks, geometry, pointer) {
            Some(hit) => cx.state.select_clip(hit.track, hit.clip),
            None => {
                cx.state.clear_clip_selection();
                if let Some(track) = geometry.row_at(pointer.y).and_then(|row| tracks.get(row)) {
                    cx.state.select_track(track.id);
                }
            }
        }
        let menu_beat = geometry.snap(geometry.beat(pointer.x), false);
        ui.data_mut(|data| data.insert_temp(response.id, menu_beat));
    }
    let menu_beat: Beats = ui.data(|data| data.get_temp(response.id)).unwrap_or(0.0);

    response.context_menu(|ui| {
        ui.set_min_width(170.0);
        match cx.state.selected_clip.and_then(|id| cx.snap.clip(id)) {
            Some((_, clip)) => {
                let (id, muted) = (clip.id, clip.muted);
                if ui.add(egui::Button::new("Duplicate").shortcut_text("Ctrl+D")).clicked() {
                    duplicate_selected_clip(cx);
                }
                if ui.button(if muted { "Unmute clip" } else { "Mute clip" }).clicked() {
                    cx.edit(|project| {
                        project.clip_mut(id)?.muted = !muted;
                        Ok(())
                    });
                }
                ui.separator();
                if ui.add(egui::Button::new("Delete").shortcut_text("Del")).clicked() {
                    delete_selected_clip(cx);
                }
            }
            None => {
                let track = cx.state.selected_track;
                if ui.add_enabled(track.is_some(), egui::Button::new("Add MIDI clip here")).clicked()
                    && let Some(track) = track
                {
                    let bar = geometry.beats_per_bar;
                    add_clip(cx, track, (menu_beat / bar).floor() * bar);
                }
            }
        }
    });
}

/// Creates a one-bar MIDI clip and opens it in the piano roll.
fn add_clip(cx: &mut Cx, track: TrackId, start: Beats) {
    let length = cx.snap.project.time_signature.beats_per_bar();
    let created = cx.edit(|project| {
        let name = format!("Clip {}", project.track(track)?.clips.len() + 1);
        project.add_midi_clip(track, name, start, length)
    });
    if let Some(clip) = created {
        cx.state.select_clip(track, clip);
        cx.state.detail_tab = DetailTab::Clip;
    }
}

/// Moves a clip to `start` on track `target`, if that is not where it is.
fn move_clip(cx: &mut Cx, id: ClipId, start: Beats, target: TrackId) {
    let Some((track, clip)) = cx.snap.clip(id) else { return };
    if (clip.start - start).abs() < 1e-9 && track.id == target {
        return;
    }
    let moved = cx.edit(|project| {
        let length = project.clip(id)?.length;
        if project.clip_track(id)? != target {
            project.track(target)?;
            let clip = project.remove_clip(id)?;
            project.track_mut(target)?.clips.push(clip);
        }
        // Also restores the clip order on the track it landed on.
        project.set_clip_span(id, start, length)
    });
    if moved.is_some() {
        cx.state.selected_track = Some(target);
    }
}

fn resize_clip(cx: &mut Cx, id: ClipId, end: Beats, min_length: Beats) {
    let Some((_, clip)) = cx.snap.clip(id) else { return };
    let (start, length) = (clip.start, (end - clip.start).max(min_length));
    if (clip.length - length).abs() >= 1e-9 {
        cx.edit(|project| project.set_clip_span(id, start, length));
    }
}

pub fn delete_selected_clip(cx: &mut Cx) {
    if let Some(id) = cx.state.selected_clip
        && cx.edit(|project| project.remove_clip(id).map(drop)).is_some()
    {
        cx.state.clear_clip_selection();
    }
}

/// Copies the selected clip to right after itself and selects the copy, so
/// repeating the command lays out a row of repeats.
pub fn duplicate_selected_clip(cx: &mut Cx) {
    let Some((track, clip)) = cx.state.selected_clip.and_then(|id| cx.snap.clip(id)) else { return };
    let (track, id, end) = (track.id, clip.id, clip.end());
    if let Some(copy) = cx.edit(|project| project.duplicate_clip(id, end, None)) {
        cx.state.select_clip(track, copy);
    }
}

// ---- ruler -----------------------------------------------------------------

fn ruler(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let response = ui.interact(geometry.ruler, ui.scope_id().with("ruler"), Sense::click_and_drag());
    paint_ruler(ui, cx, geometry);

    let region = cx.snap.project.loop_region;
    let free = ui.input(|input| input.modifiers.alt);
    if cx.state.arrangement.drag.is_none()
        && let Some(pointer) = response.hover_pos()
    {
        match loop_hit(region, geometry, pointer) {
            Some(LoopHit::Start | LoopHit::End) => ui.set_cursor_icon(CursorIcon::ResizeHorizontal),
            Some(LoopHit::Body) => ui.set_cursor_icon(CursorIcon::Grab),
            None => {}
        }
    }

    if response.clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let beat = geometry.snap(geometry.beat(pointer.x), free);
        cx.with_session(|session| session.seek(beat));
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|input| input.pointer.press_origin())
    {
        cx.state.arrangement.drag = Some(match loop_hit(region, geometry, press) {
            Some(LoopHit::Start) => ArrangeDrag::StretchLoop { anchor: region.end },
            Some(LoopHit::End) => ArrangeDrag::StretchLoop { anchor: region.start },
            Some(LoopHit::Body) => ArrangeDrag::MoveLoop { grab_offset: geometry.beat(press.x) - region.start },
            None => ArrangeDrag::Seek,
        });
        if cx.state.arrangement.drag != Some(ArrangeDrag::Seek) {
            cx.begin_gesture();
        }
    }

    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let beat = geometry.snap(geometry.beat(pointer.x), free);
        match cx.state.arrangement.drag {
            Some(ArrangeDrag::Seek) => {
                if (beat - cx.live.position).abs() > 1e-6 {
                    cx.with_session(|session| session.seek(beat));
                }
            }
            Some(ArrangeDrag::StretchLoop { anchor }) => {
                ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
                // Dragging an end past the other one flips the region
                // rather than collapsing it.
                let (start, end) = (anchor.min(beat), anchor.max(beat));
                if end - start >= geometry.min_clip_length(free) {
                    set_loop(cx, start, end);
                }
            }
            Some(ArrangeDrag::MoveLoop { grab_offset }) => {
                ui.set_cursor_icon(CursorIcon::Grabbing);
                let start = geometry.snap(geometry.beat(pointer.x) - grab_offset, free);
                set_loop(cx, start, start + (region.end - region.start));
            }
            _ => {}
        }
    }
}

fn set_loop(cx: &mut Cx, start: Beats, end: Beats) {
    let region = cx.snap.project.loop_region;
    if (region.start - start).abs() >= 1e-9 || (region.end - end).abs() >= 1e-9 {
        cx.edit(|project| project.set_loop(region.enabled, start, end));
    }
}

fn paint_ruler(ui: &Ui, cx: &Cx, geometry: &Geometry) {
    let ruler = geometry.ruler;
    let painter = ui.painter_at(ruler);
    painter.rect_filled(ruler, 0.0, theme::BG_PANEL);
    painter.hline(ruler.x_range(), ruler.bottom() - 0.5, theme::hairline(theme::BORDER));

    let signature = geometry.signature;
    let (first_beat, last_beat) = geometry.time.visible(ruler.width());
    timeline::for_each_grid_line(first_beat, last_beat, signature, geometry.grid, |beat, level| {
        let height = if level == GridLevel::Bar { 7.0 } else { 3.5 };
        let x = geometry.x(beat).round() + 0.5;
        painter.vline(x, ruler.bottom() - height..=ruler.bottom(), theme::hairline(theme::BORDER_STRONG));
    });
    // Labels are spaced for legibility, independently of the grid.
    let label_step = timeline::grid_step(geometry.time.pixels_per_beat, signature, 64.0);
    timeline::for_each_grid_line(first_beat, last_beat, signature, label_step, |beat, level| {
        let color = if level == GridLevel::Bar { theme::TEXT_DIM } else { theme::TEXT_FAINT };
        painter.text(
            pos2(geometry.x(beat) + 5.0, ruler.bottom() - 3.0),
            Align2::LEFT_BOTTOM,
            timeline::format_bar_beat(beat, signature),
            theme::font(11.0),
            color,
        );
    });

    // The loop brace.
    let region = cx.snap.project.loop_region;
    let brace = Rect::from_min_max(
        pos2(geometry.x(region.start), ruler.top() + 2.0),
        pos2(geometry.x(region.end), ruler.top() + LOOP_STRIP),
    );
    let color = if region.enabled { theme::ACCENT } else { theme::BG_CONTROL_ACTIVE };
    painter.rect_filled(brace, 2.0, theme::fade(color, if region.enabled { 0.9 } else { 1.0 }));
    if brace.width() > 12.0 {
        // Grips at both ends hint that they can be dragged.
        let grip = theme::hairline(if region.enabled { theme::TEXT_ON_COLOR } else { theme::TEXT_FAINT });
        for x in [brace.left() + 3.5, brace.right() - 3.5] {
            painter.vline(x, brace.top() + 2.5..=brace.bottom() - 2.5, grip);
        }
    }
}

fn playhead(ui: &Ui, cx: &Cx, geometry: &Geometry) {
    let area = geometry.ruler.union(geometry.lanes);
    let x = geometry.x(cx.live.position);
    if x < area.left() - 8.0 || x > area.right() + 8.0 {
        return;
    }
    let painter = ui.painter_at(area);
    let x = x.round() + 0.5;
    painter.vline(x, geometry.ruler.bottom()..=area.bottom(), theme::hairline(theme::PLAYHEAD));
    let tip = pos2(x, geometry.ruler.bottom());
    let head = vec![tip + vec2(-5.5, -9.0), tip + vec2(5.5, -9.0), tip];
    painter.add(Shape::convex_polygon(head, theme::PLAYHEAD, Stroke::NONE));
}

// ---- track headers ---------------------------------------------------------

fn headers(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    // A child clipped to the header column, so rows scrolled under the
    // ruler can neither be seen nor clicked.
    let mut column = ui.new_child(UiBuilder::new().max_rect(geometry.headers));
    column.set_clip_rect(geometry.headers.intersect(ui.clip_rect()));
    column.painter().rect_filled(geometry.headers, 0.0, theme::BG_PANEL);

    let tracks = &cx.snap.project.tracks;
    for (row, track) in tracks.iter().enumerate() {
        let top = geometry.row_top(row);
        if top > geometry.headers.bottom() || top + TRACK_HEIGHT < geometry.headers.top() {
            continue;
        }
        let rect = Rect::from_min_size(pos2(geometry.headers.left(), top), vec2(HEADER_WIDTH, TRACK_HEIGHT));
        header(&mut column, cx, track, row, rect);
    }

    let button = Rect::from_min_size(
        pos2(geometry.headers.left() + 14.0, geometry.row_top(tracks.len()) + 10.0),
        vec2(HEADER_WIDTH - 28.0, 28.0),
    );
    let response = column.interact(button, column.scope_id().with("add-track"), Sense::click());
    let (stroke, text) =
        if response.hovered() { (theme::BORDER_STRONG, theme::TEXT) } else { (theme::BORDER, theme::TEXT_DIM) };
    column.painter().rect_stroke(button, theme::RADIUS, theme::hairline(stroke), StrokeKind::Inside);
    column.painter().text(button.center(), Align2::CENTER_CENTER, "+  Add track", theme::font(12.0), text);
    if response.on_hover_text("New tracks start with a Poly Synth (Ctrl+T)").clicked() {
        add_track(cx);
    }

    column.painter().vline(geometry.headers.right() - 0.5, geometry.headers.y_range(), theme::hairline(theme::BORDER));
}

fn header(ui: &mut Ui, cx: &mut Cx, track: &Track, row: usize, rect: Rect) {
    let id = ui.scope_id().with(("track", track.id));
    let selected = cx.state.selected_track == Some(track.id);
    let color = theme::track_color(track.color);

    // Registered first, so the controls placed over it take the pointer.
    let background = ui.interact(rect, id.with("background"), Sense::click());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, if selected { theme::BG_RAISED } else { theme::BG_PANEL });
    painter.hline(rect.x_range(), rect.bottom() - 0.5, theme::hairline(theme::BORDER));
    let strip = Rect::from_min_max(rect.min, pos2(rect.left() + 5.0, rect.bottom() - 1.0));
    painter.rect_filled(strip, 0.0, if track.mute { theme::fade(color, 0.35) } else { color });

    if background.clicked() {
        cx.state.focus = FocusArea::Arrangement;
        cx.state.select_track(track.id);
    }
    if widgets::double_clicked(&background) {
        start_rename(cx, track);
    }
    background.context_menu(|ui| track_menu(ui, cx, track, row));

    let left = rect.left() + HEADER_PADDING;
    let name_row = Rect::from_min_size(pos2(left, rect.top() + 8.0), vec2(rect.width() - 16.0 - 72.0, 24.0));
    if cx.state.renaming_track.as_ref().is_some_and(|edit| edit.target == track.id) {
        let layout = egui::Layout::left_to_right(egui::Align::Center);
        let committed = widgets::place(ui, name_row, ("rename", track.id), layout, |ui| {
            widgets::name_field(
                ui,
                track.id,
                &track.name,
                &mut cx.state.renaming_track,
                theme::strong(13.0),
                name_row.width() - 12.0,
            )
        });
        if let Some(name) = committed {
            let track_id = track.id;
            cx.edit(|project| {
                project.track_mut(track_id)?.name = name;
                Ok(())
            });
        }
    } else {
        let text = if track.mute { theme::TEXT_DIM } else { theme::TEXT };
        widgets::text_in(&painter, name_row, Align2::LEFT_CENTER, &track.name, theme::strong(13.0), text);
    }

    let chip =
        |index: f32| Rect::from_min_size(pos2(rect.right() - 62.0 + index * 28.0, rect.top() + 10.0), vec2(24.0, 20.0));
    if widgets::chip_at(ui, chip(0.0), id.with("mute"), "M", track.mute, theme::MUTE).on_hover_text("Mute").clicked() {
        let (track_id, mute) = (track.id, !track.mute);
        cx.edit(|project| {
            project.track_mut(track_id)?.mute = mute;
            Ok(())
        });
    }
    if widgets::chip_at(ui, chip(1.0), id.with("solo"), "S", track.solo, theme::SOLO).on_hover_text("Solo").clicked() {
        let (track_id, solo) = (track.id, !track.solo);
        cx.edit(|project| {
            project.track_mut(track_id)?.solo = solo;
            Ok(())
        });
    }

    let controls_top = rect.top() + HEADER_CONTROLS_TOP;
    let fader = Rect::from_min_size(pos2(left, controls_top), vec2(HEADER_FADER_WIDTH, 22.0));
    let drag = widgets::volume_fader(ui, fader, id.with("volume"), track.volume_db, cx.live.track_meter(track.id));
    if drag.response.drag_started() {
        cx.begin_gesture();
    }
    if let Some(volume) = drag.value {
        let track_id = track.id;
        cx.edit(|project| project.set_track_volume(track_id, volume));
    }
    let readout = Rect::from_min_size(pos2(fader.right() + 6.0, controls_top), vec2(56.0, 22.0));
    widgets::text_in(
        &painter,
        readout,
        Align2::RIGHT_CENTER,
        &units::format_db(track.volume_db),
        theme::font(11.0),
        theme::TEXT_DIM,
    );

    let knob = Rect::from_min_size(pos2(rect.right() - 34.0, controls_top - 1.0), vec2(24.0, 24.0));
    let drag = widgets::pan_knob(ui, knob, id.with("pan"), track.pan, color);
    if drag.response.drag_started() {
        cx.begin_gesture();
    }
    if let Some(pan) = drag.value {
        let track_id = track.id;
        cx.edit(|project| project.set_track_pan(track_id, pan));
    }
    drag.response.on_hover_text(format!("Pan: {}", units::format_pan(track.pan)));
}

fn start_rename(cx: &mut Cx, track: &Track) {
    cx.state.renaming_track = Some(TextEditState { target: track.id, text: track.name.clone(), focused: false });
}

fn track_menu(ui: &mut Ui, cx: &mut Cx, track: &Track, row: usize) {
    ui.set_min_width(160.0);
    let id = track.id;
    let last = cx.snap.project.tracks.len().saturating_sub(1);
    if ui.button("Rename").clicked() {
        start_rename(cx, track);
    }
    if ui.add_enabled(row > 0, egui::Button::new("Move up")).clicked() {
        cx.edit(|project| project.move_track(id, row - 1));
    }
    if ui.add_enabled(row < last, egui::Button::new("Move down")).clicked() {
        cx.edit(|project| project.move_track(id, row + 1));
    }
    ui.separator();
    if ui.button("Delete track").clicked() {
        cx.edit(|project| project.remove_track(id).map(drop));
    }
}

/// Adds a track with the default instrument, as one undo step, and selects
/// it.
pub fn add_track(cx: &mut Cx) {
    let name = format!("Track {}", cx.snap.project.tracks.len() + 1);
    let added = cx.try_session(|session| {
        session.begin_gesture();
        let track = session.edit(|project| Ok(project.add_track(name)));
        let result = track.and_then(|track| {
            session.add_device(ChainRef::Track(track), DEFAULT_INSTRUMENT, None, None).map(|_| track)
        });
        session.end_gesture();
        result
    });
    if let Some(track) = added {
        cx.state.select_track(track);
        cx.state.clear_clip_selection();
    }
}

fn corner(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let corner = geometry.corner;
    let painter = ui.painter_at(corner);
    painter.rect_filled(corner, 0.0, theme::BG_PANEL);
    painter.hline(corner.x_range(), corner.bottom() - 0.5, theme::hairline(theme::BORDER));
    painter.vline(corner.right() - 0.5, corner.y_range(), theme::hairline(theme::BORDER));
    let count = geometry.track_count;
    painter.text(
        corner.left_center() + vec2(16.0, 0.0),
        Align2::LEFT_CENTER,
        if count == 1 { "1 TRACK".to_owned() } else { format!("{count} TRACKS") },
        theme::strong(10.0),
        theme::TEXT_FAINT,
    );
    let button = Rect::from_center_size(corner.right_center() - vec2(22.0, 0.0), vec2(26.0, 22.0));
    if widgets::icon_button_at(ui, button, ui.scope_id().with("corner-add"), Icon::Plus, None)
        .on_hover_text("Add track (Ctrl+T)")
        .clicked()
    {
        add_track(cx);
    }
}

/// A slim vertical scroll bar at the right edge, shown when the tracks do
/// not fit.
fn scroll_bar(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let max_scroll = geometry.max_scroll();
    if max_scroll <= 0.0 {
        return;
    }
    let lanes = geometry.lanes;
    let track = Rect::from_min_max(
        pos2(lanes.right() - 9.0, lanes.top() + 2.0),
        pos2(lanes.right() - 3.0, lanes.bottom() - 2.0),
    );
    let thumb_height = (track.height() * lanes.height() / geometry.content_height()).max(24.0);
    let travel = track.height() - thumb_height;
    let thumb_top = track.top() + travel * (geometry.scroll_y / max_scroll);
    let thumb = Rect::from_min_size(pos2(track.left(), thumb_top), vec2(track.width(), thumb_height));

    let response = ui.interact(thumb.expand2(vec2(4.0, 0.0)), ui.scope_id().with("track-scroll"), Sense::drag());
    if response.dragged() && travel > 0.0 {
        let view = &mut cx.state.arrangement;
        view.scroll_y = (view.scroll_y + response.drag_delta().y / travel * max_scroll).clamp(0.0, max_scroll);
    }
    let active = response.hovered() || response.dragged();
    let color = if active { theme::BG_CONTROL_ACTIVE } else { theme::BG_CONTROL_HOVER };
    ui.painter().rect_filled(thumb, 3.0, color);
}

// ---- empty project ---------------------------------------------------------

/// What a project with no tracks shows instead of an empty grid: how to
/// begin, by hand or with an assistant.
fn empty_state(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    let card = Rect::from_center_size(area.center(), vec2(560.0_f32.min(area.width() - 32.0), 392.0));
    widgets::place(ui, card, "welcome", egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(egui::RichText::new("Start your song").font(theme::strong(24.0)).color(theme::TEXT));
        ui.label(
            egui::RichText::new("This project is empty. A track, a clip and a few notes are all it takes.")
                .color(theme::TEXT_DIM),
        );
        ui.add_space(14.0);

        let steps = [
            ("Add a track", "It comes with a Poly Synth, so it makes sound right away."),
            ("Double-click a lane to create a clip", "Then click in the piano roll below to write notes."),
            ("Press Space to play", "Drag the brace on the ruler to loop a section while you work."),
        ];
        for (number, (title, detail)) in steps.iter().enumerate() {
            ui.horizontal(|ui| {
                let (badge, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                ui.painter().circle_filled(badge.center(), 12.0, theme::BG_CONTROL);
                ui.painter().text(
                    badge.center(),
                    Align2::CENTER_CENTER,
                    number + 1,
                    theme::strong(12.0),
                    theme::ACCENT,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(egui::RichText::new(*title).font(theme::strong(13.0)));
                    ui.label(egui::RichText::new(*detail).font(theme::font(12.0)).color(theme::TEXT_DIM));
                });
            });
            ui.add_space(8.0);
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let primary = egui::Button::new(
                egui::RichText::new("Add first track").color(theme::TEXT_ON_COLOR).font(theme::strong(13.0)),
            )
            .fill(theme::ACCENT)
            .min_size(vec2(132.0, 32.0));
            if ui.add(primary).clicked() {
                add_track(cx);
            }
            if ui.add(egui::Button::new("Open demo song").min_size(vec2(124.0, 32.0))).clicked() {
                load_demo(cx);
            }
            if ui.add(egui::Button::new("Open project\u{2026}").min_size(vec2(124.0, 32.0))).clicked() {
                super::files::open(ui.ctx(), cx);
            }
        });

        ui.add_space(18.0);
        egui::Frame::NONE
            .fill(theme::BG_PANEL)
            .stroke(theme::hairline(theme::BORDER))
            .corner_radius(theme::RADIUS)
            .inner_margin(egui::Margin::symmetric(14, 11))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(egui::RichText::new("Working with an AI assistant?").font(theme::strong(13.0)));
                ui.label(
                    egui::RichText::new(
                        "This DAW can be driven through MCP. A connected assistant can add tracks, write parts and \
                         mix while you watch: every change appears here as it happens, and Ctrl+Z undoes it like \
                         any other edit.",
                    )
                    .font(theme::font(12.0))
                    .color(theme::TEXT_DIM),
                );
                if let Some(url) = cx.mcp_url {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Connect it to").font(theme::font(12.0)).color(theme::TEXT_DIM));
                        ui.label(egui::RichText::new(url).font(theme::strong(12.0)).color(theme::ACCENT));
                    });
                }
            });
    });
}

fn load_demo(cx: &mut Cx) {
    if let Some(clip) = cx.try_session(crate::demo::load)
        && let Ok(track) = cx.session.lock().project().clip_track(clip)
    {
        cx.state.select_clip(track, clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daw_core::Project;

    fn geometry(track_count: usize) -> Geometry {
        let area = Rect::from_min_size(pos2(0.0, 100.0), vec2(1000.0, 400.0));
        Geometry::new(area, TimeView { origin: 4.0, pixels_per_beat: 20.0 }, 0.0, track_count, TimeSignature::default())
    }

    #[test]
    fn the_area_is_split_into_four_regions() {
        let g = geometry(3);
        assert_eq!(g.corner, Rect::from_min_max(pos2(0.0, 100.0), pos2(HEADER_WIDTH, 100.0 + RULER_HEIGHT)));
        assert_eq!(g.lanes.min, pos2(HEADER_WIDTH, 100.0 + RULER_HEIGHT));
        assert_eq!(g.lanes.max, pos2(1000.0, 500.0));
        assert_eq!(g.ruler.bottom(), g.lanes.top());
        assert_eq!(g.headers.right(), g.lanes.left());
        // Beat 4 is at the left edge of the lanes; a bar later is 80 px on.
        assert_eq!(g.x(4.0), HEADER_WIDTH);
        assert_eq!(g.x(8.0), HEADER_WIDTH + 80.0);
        assert_eq!(g.beat(HEADER_WIDTH + 40.0), 6.0);
    }

    #[test]
    fn rows_map_to_tracks() {
        let mut g = geometry(3);
        let top = g.lanes.top();
        assert_eq!(g.row_at(top - 1.0), None);
        assert_eq!(g.row_at(top), Some(0));
        assert_eq!(g.row_at(top + TRACK_HEIGHT - 0.5), Some(0));
        assert_eq!(g.row_at(top + TRACK_HEIGHT), Some(1));
        assert_eq!(g.row_at(top + 3.0 * TRACK_HEIGHT + 1.0), None, "below the last track");
        assert_eq!(g.nearest_row(top - 500.0), 0);
        assert_eq!(g.nearest_row(top + 3.0 * TRACK_HEIGHT + 500.0), 2);

        g.scroll_y = TRACK_HEIGHT;
        assert_eq!(g.row_at(top), Some(1), "scrolled down by one track");
        assert_eq!(g.row_top(1), top);
    }

    #[test]
    fn scrolling_is_only_possible_when_tracks_overflow() {
        assert_eq!(geometry(2).max_scroll(), 0.0);
        let tall = geometry(10);
        assert_eq!(tall.max_scroll(), 10.0 * TRACK_HEIGHT + ADD_ROW_HEIGHT - tall.lanes.height());
    }

    #[test]
    fn snapping_follows_the_visible_grid_but_never_skips_bars() {
        let g = geometry(1);
        assert_eq!(g.grid, 1.0, "20 px per beat shows quarter notes");
        assert_eq!(g.snap(5.4, false), 5.0);
        assert_eq!(g.snap(5.4, true), 5.4);
        assert_eq!(g.snap(-3.0, true), 0.0);
        assert_eq!(g.min_clip_length(false), 1.0);

        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 400.0));
        let far = Geometry::new(area, TimeView::new(2.0), 0.0, 1, TimeSignature::default());
        assert_eq!(far.grid, 16.0, "zoomed out, lines are four bars apart");
        assert_eq!(far.snap(9.0, false), 8.0, "but clips still snap to single bars");
        assert_eq!(far.snap(11.0, false), 12.0);
    }

    fn project_with_clips() -> (Project, TrackId, ClipId, ClipId) {
        let mut project = Project::new("t");
        let _first = project.add_track("A");
        let second = project.add_track("B");
        let early = project.add_midi_clip(second, "early", 4.0, 4.0).unwrap();
        let late = project.add_midi_clip(second, "late", 6.0, 8.0).unwrap();
        (project, second, early, late)
    }

    #[test]
    fn clips_are_hit_on_their_own_track_topmost_first() {
        let (project, track, early, late) = project_with_clips();
        let g = geometry(2);
        let row = g.lanes.top() + TRACK_HEIGHT + 20.0;

        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(5.0), g.lanes.top() + 20.0)), None, "track A is empty");
        let hit = clip_at(&project.tracks, &g, pos2(g.x(5.0), row)).unwrap();
        assert_eq!((hit.track, hit.clip, hit.part, hit.start), (track, early, SpanHit::Body, 4.0));
        // Where the two overlap, the later clip is on top.
        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(7.0), row)).unwrap().clip, late);
        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(14.0) - 2.0, row)).unwrap().part, SpanHit::End);
        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(15.0), row)), None);
        assert_eq!(clip_at(&project.tracks, &g, pos2(g.x(5.0), g.lanes.top() + 5.0 * TRACK_HEIGHT)), None);
    }

    #[test]
    fn the_loop_brace_has_two_ends_and_a_body() {
        let g = geometry(1);
        let region = LoopRegion { enabled: true, start: 8.0, end: 16.0 };
        let y = g.ruler.top() + 5.0;
        assert_eq!(loop_hit(region, &g, pos2(g.x(8.0) + 2.0, y)), Some(LoopHit::Start));
        assert_eq!(loop_hit(region, &g, pos2(g.x(16.0) - 2.0, y)), Some(LoopHit::End));
        assert_eq!(loop_hit(region, &g, pos2(g.x(12.0), y)), Some(LoopHit::Body));
        assert_eq!(loop_hit(region, &g, pos2(g.x(20.0), y)), None);
        assert_eq!(
            loop_hit(region, &g, pos2(g.x(12.0), g.ruler.bottom() - 2.0)),
            None,
            "below the brace is for seeking"
        );
        // A very short region can still be stretched: its end wins.
        let tiny = LoopRegion { enabled: true, start: 8.0, end: 8.25 };
        assert_eq!(loop_hit(tiny, &g, pos2(g.x(8.1), y)), Some(LoopHit::End));
    }
}
