//! The arrangement: track headers on the left, a ruler on top, the clips of
//! every track on a shared timeline, and the master pinned along the bottom.
//!
//! Everything is placed by hand from a [`Geometry`] computed once per frame,
//! and painted from the snapshot. Edits go to the session and show up on the
//! next frame, when the snapshot has caught up.

use super::state::{ArrangeDrag, Cx, DetailTab, FocusArea, TextEditState};
use super::timeline::{self, GridLevel, SpanHit, TimeView};
use super::{colors, units, widgets};
use daw_core::project::{ClipContent, LoopRegion};
use daw_core::{Beats, ChainRef, Clip, ClipId, TimeSignature, Track, TrackId, TrackKind};
use daw_noir::{Bar, Chip, MenuItem, Theme, Variant, fade, paint, text};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Mesh, PointerButton, Pos2, Rect, Sense, Shape, Stroke, Ui, UiBuilder, pos2, vec2,
};

pub(super) const HEADER_WIDTH: f32 = 232.0;
pub(super) const RULER_HEIGHT: f32 = 22.0;
pub(super) const TRACK_HEIGHT: f32 = 60.0;
pub(super) const MASTER_HEIGHT: f32 = 36.0;
/// The top of the ruler, where the loop bar is grabbed. The bar itself is
/// drawn thinner than the zone that catches the pointer.
const LOOP_STRIP: f32 = 9.0;
const LOOP_BAR: f32 = 4.0;
/// How far a grip hangs below the ruler's top edge.
const LOOP_GRIP: f32 = 8.0;
/// Width of the color strip down a header's left edge.
const HEADER_STRIP: f32 = 3.0;
/// Where a header's content starts, and how far from its right edge it ends.
pub(super) const HEADER_LEFT: f32 = 12.0;
pub(super) const HEADER_RIGHT: f32 = 14.0;
/// Width of the track number before the name.
pub(super) const HEADER_INDEX: f32 = 14.0;
/// Top of the name row and of the row of controls under it.
pub(super) const HEADER_NAME_TOP: f32 = 6.0;
pub(super) const HEADER_CONTROLS_TOP: f32 = 31.0;
/// Size of the mute and solo chips.
pub(super) const HEADER_CHIP: egui::Vec2 = egui::Vec2::new(20.0, 17.0);
pub(super) const HEADER_PAN_WIDTH: f32 = 46.0;
/// The meter at a header's right edge.
const HEADER_METER_WIDTH: f32 = 7.0;
const HEADER_METER_INSET: f32 = 4.0;
/// Width of the master's volume bar.
pub(super) const MASTER_VOLUME_WIDTH: f32 = 112.0;
const CLIP_TITLE: f32 = 14.0;
/// Width of the zone at a clip's end that resizes it.
const RESIZE_HANDLE: f32 = 8.0;
/// How close, in pixels, the pointer must be to an end of the loop bar to
/// grab it.
const LOOP_END_GRAB: f32 = 6.0;
/// Grid lines closer together than this are dropped.
const GRID_MIN_PIXELS: f64 = 18.0;
/// Bar numbers closer together than this are dropped.
const LABEL_MIN_PIXELS: f64 = 46.0;
/// The instrument new tracks get, so they make sound immediately.
const DEFAULT_INSTRUMENT: &str = "builtin:polysynth";

/// Where everything is this frame.
#[derive(Copy, Clone, Debug)]
struct Geometry {
    corner: Rect,
    ruler: Rect,
    headers: Rect,
    lanes: Rect,
    /// The master row, across the whole width under the tracks.
    master: Rect,
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
        let master_top = (area.bottom() - MASTER_HEIGHT).max(split_y);
        Self {
            corner: Rect::from_min_max(area.min, pos2(split_x, split_y)),
            ruler: Rect::from_min_max(pos2(split_x, area.top()), pos2(area.right(), split_y)),
            headers: Rect::from_min_max(pos2(area.left(), split_y), pos2(split_x, master_top)),
            lanes: Rect::from_min_max(pos2(split_x, split_y), pos2(area.right(), master_top)),
            master: Rect::from_min_max(pos2(area.left(), master_top), area.max),
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

    /// Height of everything that scrolls: the tracks.
    fn content_height(&self) -> f32 {
        self.track_count as f32 * TRACK_HEIGHT
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

    /// Screen rectangle of a clip on track row `row`, before clipping: a
    /// pixel clear of the row's edges and of the clip that follows it.
    fn clip_rect(&self, row: usize, clip: &Clip) -> Rect {
        let top = self.row_top(row);
        let left = self.x(clip.start).round();
        let right = (self.x(clip.end()).round() - 1.0).max(left + 1.0);
        Rect::from_min_max(pos2(left, top + 1.0), pos2(right, top + TRACK_HEIGHT - 2.0))
    }

    /// Whether the grid line at `beat` is one of the strong ones: a bar
    /// line while beats are drawn, every fourth line once only bars are.
    fn is_strong_line(&self, beat: Beats, level: GridLevel) -> bool {
        if self.grid < self.beats_per_bar {
            return level == GridLevel::Bar;
        }
        let group = self.grid * 4.0;
        (beat / group - (beat / group).round()).abs() < 1e-9
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

/// Which part of the loop bar is at `position`, if any.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LoopHit {
    Start,
    End,
    Body,
}

fn loop_hit(region: LoopRegion, geometry: &Geometry, position: Pos2) -> Option<LoopHit> {
    if position.y > geometry.ruler.top() + LOOP_STRIP {
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
    ui.painter().rect_filled(area, 0.0, cx.theme.bg);

    let signature = cx.snap.project.time_signature;
    let track_count = cx.snap.project.tracks.len();
    let view = &mut cx.state.arrangement;
    if !view.framed && track_count > 0 {
        // Open a project showing all of it, or a few bars of a new one.
        view.framed = true;
        let end = cx.snap.project.length_beats().max(8.0 * signature.beats_per_bar());
        view.time.fit(0.0, end, area.width() - HEADER_WIDTH);
        view.time.origin = 0.0;
    }
    let mut geometry = Geometry::new(area, view.time, view.scroll_y, track_count, signature);
    if track_count == 0 {
        // Nothing to arrange yet: say how to begin, above the master.
        empty_state(ui, cx, Rect::from_min_max(area.min, pos2(area.right(), geometry.master.top())));
        master(ui, cx, &geometry);
        return;
    }
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
    master(ui, cx, &geometry);
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
    let theme = cx.theme;
    let painter = ui.painter_at(geometry.lanes);
    let lanes = geometry.lanes;
    let (first_beat, last_beat) = geometry.time.visible(lanes.width());

    timeline::for_each_grid_line(first_beat, last_beat, geometry.signature, geometry.grid, |beat, level| {
        let color = if geometry.is_strong_line(beat, level) { theme.grid } else { theme.grid_fine };
        paint::vline(&painter, geometry.x(beat).round(), lanes.y_range(), color);
    });

    // All note miniatures go into one mesh: thousands of tiny rectangles
    // cost one draw call instead of one shape each.
    let mut notes = Mesh::default();
    for (row, track) in cx.snap.project.tracks.iter().enumerate() {
        let top = geometry.row_top(row);
        if top > lanes.bottom() || top + TRACK_HEIGHT < lanes.top() {
            continue;
        }
        if track.kind != TrackKind::Normal {
            // A group or return track mixes other tracks; its lane holds
            // no clips, and says so by stepping back.
            let lane = Rect::from_x_y_ranges(lanes.x_range(), top..=top + TRACK_HEIGHT);
            painter.rect_filled(lane, 0.0, theme.shade);
        }
        paint::hline_above(&painter, lanes.x_range(), top + TRACK_HEIGHT, theme.hairline);
        let color = colors::track_color(theme, track.color);
        for (index, clip) in track.clips.iter().enumerate() {
            let rect = geometry.clip_rect(row, clip);
            if rect.right() < lanes.left() || rect.left() > lanes.right() {
                continue;
            }
            // A clip that starts at the same place is painted on top and
            // hides this one's start; the name moves to where it shows.
            let later = track.clips[index + 1..].iter().filter(|later| later.start <= clip.start);
            let covered_until = later.map(Clip::end).fold(clip.start, f64::max);
            let look =
                ClipLook { color, ghosted: clip.muted || track.mute, name_from: geometry.x(covered_until).round() };
            paint_clip(&painter, theme, &mut notes, rect, lanes, geometry, clip, look);
        }
    }
    painter.add(Shape::mesh(notes));

    // The selection's outline goes on top of the notes of neighboring clips.
    if let Some((track, clip)) = cx.state.selected_clip.and_then(|id| cx.snap.clip(id))
        && let Some(row) = cx.snap.track_index(track.id)
    {
        paint::frame(&painter, geometry.clip_rect(row, clip), theme.ink);
    }
}

/// How a clip is colored.
#[derive(Copy, Clone)]
struct ClipLook {
    /// The hue of its track.
    color: Color32,
    /// Muted, by itself or with its track: it keeps its hue but steps back.
    ghosted: bool,
    /// Screen x from which the clip is not covered by another: where its
    /// name may start.
    name_from: f32,
}

#[expect(clippy::too_many_arguments, reason = "a private paint routine; a struct would only rename the arguments")]
fn paint_clip(
    painter: &egui::Painter,
    theme: &Theme,
    notes: &mut Mesh,
    rect: Rect,
    lanes: Rect,
    geometry: &Geometry,
    clip: &Clip,
    look: ClipLook,
) {
    let ClipLook { color, ghosted, name_from } = look;
    painter.rect_filled(rect, 0.0, fade(color, if ghosted { 0.06 } else { 0.15 }));

    // The name strip: the one solid block of the track's color.
    let title = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + CLIP_TITLE));
    let (strip, name) = if ghosted { (fade(color, 0.22), theme.ink_dim) } else { (color, theme.on_accent) };
    painter.rect_filled(title, 0.0, strip);
    if rect.width() > 18.0 {
        // Keep the name on screen while the clip's start is scrolled away.
        let text_left = (rect.left().max(lanes.left()).max(name_from) + 4.0).min(rect.right() - 14.0);
        let room = Rect::from_min_max(pos2(text_left, title.top()), pos2(title.right() - 3.0, title.bottom()));
        text::paint(painter, room, Align2::LEFT_CENTER, theme.text.micro, &clip.name, name);
    }

    let content = Rect::from_min_max(pos2(rect.left(), title.bottom()), rect.max).intersect(lanes);
    if content.width() <= 0.0 || content.height() <= 0.0 {
        return;
    }
    let ink = if ghosted { fade(color, 0.4) } else { color };
    match &clip.content {
        ClipContent::Midi(midi) => {
            paint_note_miniature(painter, theme, notes, content, geometry, clip, &midi.notes, midi.loop_length, ink)
        }
        ClipContent::Audio(audio) => paint_audio_body(painter, theme, content, rect, &audio.path, ink),
    }
}

/// Draws a clip's notes as thin lines, repeated across the clip if it loops.
#[expect(clippy::too_many_arguments, reason = "a private paint routine; a struct would only rename the arguments")]
fn paint_note_miniature(
    painter: &egui::Painter,
    theme: &Theme,
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
    let area = content.shrink2(vec2(0.0, 4.0));
    let line = (area.height() / span).clamp(1.0, 2.0);
    let y_of = |pitch: u8| (area.bottom() - line - (pitch - low) as f32 / span * (area.height() - line)).round();

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
            // Where the phrase starts over.
            paint::vline(painter, geometry.x(offset).round(), content.y_range(), theme.hairline);
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
            let bar = Rect::from_min_max(pos2(left, top), pos2(right - 0.5, top + line)).intersect(content);
            if bar.width() > 0.0 {
                mesh.add_colored_rect(bar, ink);
            }
        }
    }
}

/// Audio clips show their file instead of notes: a hatched body with a
/// center line, so they are told apart at a glance, and the file's name.
fn paint_audio_body(painter: &egui::Painter, theme: &Theme, content: Rect, clip_rect: Rect, path: &str, ink: Color32) {
    const HATCH: f32 = 8.0;
    let clipped = painter.with_clip_rect(content);
    let hatch = Stroke::new(1.0, fade(ink, 0.3));
    let slant = content.height();
    // The hatching is anchored to the clip, not the view, so it does not
    // crawl while scrolling.
    let skipped = ((content.left() - clip_rect.left()) / HATCH).floor().max(0.0);
    let mut x = clip_rect.left() - slant + skipped * HATCH;
    while x < content.right() {
        clipped.line_segment([pos2(x, content.bottom()), pos2(x + slant, content.top())], hatch);
        x += HATCH;
    }
    let middle = content.center().y.round();
    paint::hline(&clipped, content.x_range(), middle, fade(ink, 0.8));

    if content.width() > 48.0 {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let room = Rect::from_min_max(pos2(content.left() + 4.0, middle + 2.0), content.max - vec2(4.0, 2.0));
        if room.height() >= theme.text.micro.size {
            // A plate under the name keeps the hatching from running
            // through the letters.
            // A file name keeps its case: it is a name, not a label.
            let file_style = daw_noir::TextStyle { upper: false, tracking: 0.2, ..theme.text.micro };
            let width = text::width(&clipped, file_style, name).min(room.width());
            let plate = Rect::from_min_size(room.min - vec2(2.0, 0.0), vec2(width + 4.0, room.height()));
            clipped.rect_filled(plate, 0.0, theme.bg);
            text::paint(&clipped, room, Align2::LEFT_CENTER, file_style, name, theme.ink_dim);
        }
    }
}

fn lane_interaction(ui: &Ui, cx: &mut Cx, geometry: &Geometry, response: &egui::Response) {
    let tracks = &cx.snap.project.tracks;
    let free = ui.input(|input| input.modifiers.alt);

    if let Some(pointer) = response.hover_pos()
        && cx.state.arrangement.drag.is_none()
    {
        match clip_at(tracks, geometry, pointer) {
            Some(hit) if hit.part == SpanHit::End => {
                ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
                cx.state.hint.set("Drag to change the clip\u{2019}s length \u{00B7} Alt for no snapping");
            }
            Some(_) => cx.state.hint.set(
                "Drag to move \u{00B7} double-click to open in the piano roll \u{00B7} Ctrl+D duplicates \u{00B7} right-click for more",
            ),
            None => {}
        }
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
                    match kind_note(track.kind) {
                        Some(note) => cx.state.toasts.info(note),
                        None => {
                            let bar = geometry.beats_per_bar;
                            let start = (geometry.beat(pointer.x) / bar).floor().max(0.0) * bar;
                            add_clip(cx, track.id, start);
                        }
                    }
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
    let theme = cx.theme;
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
        ui.set_min_width(190.0);
        match cx.state.selected_clip.and_then(|id| cx.snap.clip(id)) {
            Some((_, clip)) => {
                let (id, muted) = (clip.id, clip.muted);
                if MenuItem::new("Duplicate").shortcut("Ctrl+D").show(ui, theme).clicked() {
                    duplicate_selected_clip(cx);
                }
                if MenuItem::new(if muted { "Unmute clip" } else { "Mute clip" }).show(ui, theme).clicked() {
                    cx.edit(|project| {
                        project.clip_mut(id)?.muted = !muted;
                        Ok(())
                    });
                }
                daw_noir::menu_separator(ui, theme);
                if MenuItem::new("Delete").shortcut("Del").show(ui, theme).clicked() {
                    delete_selected_clip(cx);
                }
            }
            None => {
                let holds_clips = |id| cx.snap.track(id).is_some_and(|track| track.kind == TrackKind::Normal);
                let track = cx.state.selected_track.filter(|id| holds_clips(*id));
                if MenuItem::new("Add MIDI clip here").enabled(track.is_some()).show(ui, theme).clicked()
                    && let Some(track) = track
                {
                    let bar = geometry.beats_per_bar;
                    add_clip(cx, track, (menu_beat / bar).floor() * bar);
                }
            }
        }
    });
}

/// Why a track of this kind takes no clips; `None` for one that does.
fn kind_note(kind: TrackKind) -> Option<&'static str> {
    match kind {
        TrackKind::Normal => None,
        TrackKind::Group => Some("A group track mixes the tracks that belong to it and holds no clips"),
        TrackKind::Return => Some("A return track receives what other tracks send to it and holds no clips"),
    }
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
        cx.state.select_clip(target, id);
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
            Some(LoopHit::Start | LoopHit::End) => {
                ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
                cx.state.hint.set("Drag to move this end of the loop \u{00B7} L switches looping on and off");
            }
            Some(LoopHit::Body) => {
                ui.set_cursor_icon(CursorIcon::Grab);
                cx.state.hint.set("Drag to move the loop \u{00B7} L switches looping on and off");
            }
            None => cx.state.hint.set("Click or drag to move the playhead \u{00B7} Ctrl+scroll zooms"),
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
    let theme = cx.theme;
    let ruler = geometry.ruler;
    let painter = ui.painter_at(ruler);
    painter.rect_filled(ruler, 0.0, theme.panel);
    paint::hline_above(&painter, ruler.x_range(), ruler.bottom(), theme.hairline);

    let signature = geometry.signature;
    let (first_beat, last_beat) = geometry.time.visible(ruler.width());
    // Ticks continue the lanes' grid up through the ruler, bars and coarser
    // only; numbers are spaced for legibility, independently of the grid.
    let tick_step = geometry.grid.max(geometry.beats_per_bar);
    timeline::for_each_grid_line(first_beat, last_beat, signature, tick_step, |beat, _| {
        paint::vline(&painter, geometry.x(beat).round(), ruler.y_range(), theme.grid);
    });
    let label_step = timeline::grid_step(geometry.time.pixels_per_beat, signature, LABEL_MIN_PIXELS).max(tick_step);
    let style = daw_noir::TextStyle { tracking: 0.4, ..theme.text.micro };
    timeline::for_each_grid_line(first_beat, last_beat, signature, label_step, |beat, _| {
        let room = Rect::from_min_max(pos2(geometry.x(beat).round() + 4.0, ruler.top() + 3.0), ruler.max);
        text::paint(
            &painter,
            room,
            Align2::LEFT_CENTER,
            style,
            &timeline::format_bar_beat(beat, signature),
            theme.ink_faint,
        );
    });

    // The loop region: a thin bar along the top edge with a grip at each
    // end, lit while looping is on.
    let region = cx.snap.project.loop_region;
    let (left, right) = (geometry.x(region.start).round(), geometry.x(region.end).round());
    let color = if region.enabled { theme.ink_dim } else { theme.ghost };
    painter.rect_filled(Rect::from_min_max(pos2(left, ruler.top()), pos2(right, ruler.top() + LOOP_BAR)), 0.0, color);
    if right - left > 8.0 {
        for grip in [left, right - 2.0] {
            painter.rect_filled(Rect::from_min_size(pos2(grip, ruler.top()), vec2(2.0, LOOP_GRIP)), 0.0, color);
        }
    }
}

fn playhead(ui: &Ui, cx: &Cx, geometry: &Geometry) {
    let area = geometry.ruler.union(geometry.lanes);
    let x = geometry.x(cx.live.position).round();
    if x < area.left() - 6.0 || x > area.right() + 6.0 {
        return;
    }
    let painter = ui.painter_at(area);
    let signal = cx.theme.signal;
    paint::vline(&painter, x, area.y_range(), signal);
    // A small flag where the line meets the top of the ruler.
    let tip = pos2(x + 0.5, area.top());
    let flag = vec![tip + vec2(-4.5, 0.0), tip + vec2(4.5, 0.0), tip + vec2(0.0, 6.0)];
    painter.add(Shape::convex_polygon(flag, signal, Stroke::NONE));
}

// ---- track headers ---------------------------------------------------------

fn headers(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    // A child clipped to the header column, so rows scrolled under the
    // ruler can neither be seen nor clicked.
    let mut column = ui.new_child(UiBuilder::new().max_rect(geometry.headers));
    column.set_clip_rect(geometry.headers.intersect(ui.clip_rect()));
    column.painter().rect_filled(geometry.headers, 0.0, theme.panel);

    let tracks = &cx.snap.project.tracks;
    for (row, track) in tracks.iter().enumerate() {
        let top = geometry.row_top(row);
        if top > geometry.headers.bottom() || top + TRACK_HEIGHT < geometry.headers.top() {
            continue;
        }
        let rect = Rect::from_min_size(pos2(geometry.headers.left(), top), vec2(HEADER_WIDTH, TRACK_HEIGHT));
        header(&mut column, cx, track, row, rect);
    }
    paint::vline_left_of(column.painter(), geometry.headers.right(), geometry.headers.y_range(), theme.hairline);
}

/// Where the parts of a track header are, relative to its rectangle. Shared
/// with the master row, whose controls line up with the tracks'.
struct HeaderParts {
    /// Left edge of the content, right of the color strip.
    left: f32,
    /// Right edge of the content, left of the meter.
    right: f32,
    meter: Rect,
}

impl HeaderParts {
    fn of(rect: Rect, meter_inset: f32) -> Self {
        let meter_right = rect.right() - HEADER_METER_INSET;
        Self {
            left: rect.left() + HEADER_LEFT,
            right: rect.right() - HEADER_RIGHT,
            meter: Rect::from_min_max(
                pos2(meter_right - HEADER_METER_WIDTH, rect.top() + meter_inset),
                pos2(meter_right, rect.bottom() - meter_inset - 1.0),
            ),
        }
    }
}

fn header(ui: &mut Ui, cx: &mut Cx, track: &Track, row: usize, rect: Rect) {
    let theme = cx.theme;
    let id = ui.scope_id().with(("track", track.id));
    let selected = cx.state.selected_track == Some(track.id) && !cx.state.master_selected;
    let color = colors::track_color(theme, track.color);
    let surface = if selected { theme.raised } else { theme.panel };
    let quiet = theme.quiet_on(surface);
    let parts = HeaderParts::of(rect, 5.0);

    // Registered first, so the controls placed over it take the pointer.
    let background = ui.interact(rect, id.with("background"), Sense::click());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, surface);
    paint::hline_above(&painter, rect.x_range(), rect.bottom(), theme.hairline);
    let strip = Rect::from_min_max(rect.min, pos2(rect.left() + HEADER_STRIP, rect.bottom() - 1.0));
    painter.rect_filled(strip, 0.0, if track.mute { fade(color, 0.35) } else { color });

    cx.hint(&background, "Click to select \u{00B7} double-click to rename \u{00B7} right-click for more");
    if background.clicked() {
        cx.state.focus = FocusArea::Arrangement;
        cx.state.select_track(track.id);
    }
    if widgets::double_clicked(&background) {
        start_rename(cx, track);
    }
    background.context_menu(|ui| track_menu(ui, cx, track, row));

    // Name row: number, name, mute and solo.
    let name_row = Rect::from_min_size(pos2(parts.left, rect.top() + HEADER_NAME_TOP), vec2(0.0, HEADER_CHIP.y));
    let chip = |index: f32| {
        let left = parts.right - (2.0 - index) * HEADER_CHIP.x - (1.0 - index) * 3.0;
        Rect::from_min_size(pos2(left, name_row.top()), HEADER_CHIP)
    };
    let index_rect = name_row.with_max_x(parts.left + HEADER_INDEX);
    let number_style = daw_noir::TextStyle { size: 11.0, ..theme.text.value };
    text::paint(&painter, index_rect, Align2::LEFT_CENTER, number_style, &(row + 1).to_string(), quiet);
    let mut name_rect = Rect::from_min_max(index_rect.right_top(), pos2(chip(0.0).left() - 6.0, name_row.bottom()));
    let badge = match track.kind {
        TrackKind::Normal => None,
        TrackKind::Group => Some("Group"),
        TrackKind::Return => Some("Return"),
    };
    if let Some(badge) = badge {
        // What kind of track this is, where the name leaves room.
        let width = text::paint(&painter, name_rect, Align2::RIGHT_CENTER, theme.text.micro, badge, quiet);
        name_rect.max.x -= width + 6.0;
    }
    if cx.state.renaming_track.as_ref().is_some_and(|edit| edit.target == track.id) {
        let field = name_rect.expand2(vec2(5.0, 2.0)).with_min_x(name_rect.left() - 7.0);
        let committed = widgets::name_field(ui, theme, field, track.id, &track.name, &mut cx.state.renaming_track);
        if let Some(name) = committed {
            let track_id = track.id;
            cx.edit(|project| {
                project.track_mut(track_id)?.name = name;
                Ok(())
            });
        }
    } else {
        let ink = if track.mute { theme.ink_dim } else { theme.ink };
        text::paint(&painter, name_rect, Align2::LEFT_CENTER, theme.text.body, &track.name, ink);
    }

    // Named for the track, so each is told apart by assistive technology.
    let (mute_name, solo_name) = (format!("Mute {}", track.name), format!("Solo {}", track.name));
    let mute = Chip::new("M", track.mute).accent(theme.warn).quiet(quiet).named(&mute_name);
    let response = daw_noir::chip(ui, theme, chip(0.0), id.with("mute"), &mute);
    cx.hint(&response, "Mute this track");
    if response.clicked() {
        let (track_id, mute) = (track.id, !track.mute);
        cx.edit(|project| {
            project.track_mut(track_id)?.mute = mute;
            Ok(())
        });
    }
    let solo = Chip::new("S", track.solo).accent(theme.info).quiet(quiet).named(&solo_name);
    let response = daw_noir::chip(ui, theme, chip(1.0), id.with("solo"), &solo);
    cx.hint(&response, "Solo: hear only the soloed tracks");
    if response.clicked() {
        let (track_id, solo) = (track.id, !track.solo);
        cx.edit(|project| {
            project.track_mut(track_id)?.solo = solo;
            Ok(())
        });
    }

    // Controls row: volume and pan, indented to line up with the name.
    let controls_top = rect.top() + HEADER_CONTROLS_TOP;
    let pan_rect = Rect::from_min_size(
        pos2(parts.right - HEADER_PAN_WIDTH, controls_top),
        vec2(HEADER_PAN_WIDTH, theme.metric.bar),
    );
    let volume_rect = Rect::from_min_max(
        pos2(parts.left + HEADER_INDEX, controls_top),
        pos2(pan_rect.left() - 3.0, controls_top + theme.metric.bar),
    );
    let shown = units::format_db(track.volume_db);
    let bar = Bar::new("Vol", &shown, units::db_to_fader(track.volume_db)).default(units::db_to_fader(0.0));
    let output = daw_noir::bar(ui, theme, volume_rect, id.with("volume"), &bar);
    cx.hint(
        &output.response,
        format!("Volume \u{00B7} {shown} \u{00B7} drag to set, Ctrl-drag for fine, double-click for 0 dB"),
    );
    if output.pressed {
        cx.begin_gesture();
    }
    if let Some(position) = output.frac {
        let volume = if output.reset { 0.0 } else { units::fader_to_db(position) };
        let track_id = track.id;
        if volume != track.volume_db {
            cx.edit(|project| project.set_track_volume(track_id, volume));
        }
    }

    let shown = units::format_pan(track.pan);
    let bar = Bar::compact("Pan", &shown, (track.pan + 1.0) / 2.0).default(0.5);
    let output = daw_noir::bar(ui, theme, pan_rect, id.with("pan"), &bar);
    cx.hint(
        &output.response,
        format!("Pan \u{00B7} {shown} \u{00B7} drag to set, Ctrl-drag for fine, double-click to center"),
    );
    if output.pressed {
        cx.begin_gesture();
    }
    if let Some(position) = output.frac {
        let pan = position * 2.0 - 1.0;
        // A small dead zone makes center easy to find again.
        let (track_id, pan) = (track.id, if output.reset || pan.abs() < 0.03 { 0.0 } else { pan });
        if pan != track.pan {
            cx.edit(|project| project.set_track_pan(track_id, pan));
        }
    }

    let levels = widgets::levels(cx.live.track_meter(track.id));
    daw_noir::stereo_meter(&painter, theme, parts.meter, levels, widgets::meter_zones());
}

fn start_rename(cx: &mut Cx, track: &Track) {
    cx.state.renaming_track = Some(TextEditState { target: track.id, text: track.name.clone(), focused: false });
}

fn track_menu(ui: &mut Ui, cx: &mut Cx, track: &Track, row: usize) {
    let theme = cx.theme;
    ui.set_min_width(170.0);
    let id = track.id;
    let last = cx.snap.project.tracks.len().saturating_sub(1);
    if MenuItem::new("Rename").show(ui, theme).clicked() {
        start_rename(cx, track);
    }
    if MenuItem::new("Move up").enabled(row > 0).show(ui, theme).clicked() {
        cx.edit(|project| project.move_track(id, row - 1));
    }
    if MenuItem::new("Move down").enabled(row < last).show(ui, theme).clicked() {
        cx.edit(|project| project.move_track(id, row + 1));
    }
    daw_noir::menu_separator(ui, theme);
    if MenuItem::new("Delete track").show(ui, theme).clicked() {
        cx.edit(|project| project.remove_track(id).map(drop));
    }
}

/// Adds a track with the default instrument, as one undo step, and selects
/// it.
pub fn add_track(cx: &mut Cx) {
    add_track_with(cx, DEFAULT_INSTRUMENT);
}

/// Adds a track holding the instrument at `uri`, in the next hue of the
/// palette, as one undo step, and selects it.
pub fn add_track_with(cx: &mut Cx, uri: &str) {
    let index = cx.snap.project.tracks.len();
    let name = format!("Track {}", index + 1);
    let color = colors::new_track_color(index);
    let added = cx.try_session(|session| {
        session.begin_gesture();
        let track = session.edit(|project| {
            let track = project.add_track(name);
            project.track_mut(track)?.color = color;
            Ok(track)
        });
        let result = track.and_then(|track| session.add_device(ChainRef::Track(track), uri, None, None).map(|_| track));
        session.end_gesture();
        result
    });
    if let Some(track) = added {
        cx.state.select_track(track);
        cx.state.clear_clip_selection();
    }
}

fn corner(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let corner = geometry.corner;
    let painter = ui.painter_at(corner);
    painter.rect_filled(corner, 0.0, theme.panel);
    paint::hline_above(&painter, corner.x_range(), corner.bottom(), theme.hairline);
    paint::vline_left_of(&painter, corner.right(), corner.y_range(), theme.hairline);
    let count = geometry.track_count;
    let label = if count == 1 { "1 track".to_owned() } else { format!("{count} tracks") };
    let room = Rect::from_min_max(pos2(corner.left() + HEADER_LEFT, corner.top()), corner.max);
    text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, &label, theme.ink_faint);

    let add = add_track_button(ui, theme, corner);
    let response = ui.interact(add, ui.scope_id().with("corner-add"), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "New track"));
    if response.hovered() {
        painter.rect_filled(add, 0.0, theme.hover);
    }
    let ink = if response.hovered() { theme.ink } else { theme.ink_faint };
    text::paint(&painter, add, Align2::CENTER_CENTER, theme.text.micro, "+ Track", ink);
    cx.hint(&response, "Add a track with a Poly Synth \u{00B7} Ctrl+T");
    if response.clicked() {
        add_track(cx);
    }
}

/// Where the "+ Track" action sits in the corner above the headers.
fn add_track_button(ui: &Ui, theme: &Theme, corner: Rect) -> Rect {
    let width = text::width(ui.painter(), theme.text.micro, "+ Track").ceil() + 12.0;
    let right = corner.right() - HEADER_RIGHT + 6.0;
    Rect::from_min_max(pos2(right - width, corner.top() + 2.0), pos2(right, corner.bottom() - 3.0))
}

/// A slim vertical scroll bar at the right edge, shown when the tracks do
/// not fit.
fn scroll_bar(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let max_scroll = geometry.max_scroll();
    if max_scroll <= 0.0 {
        return;
    }
    let lanes = geometry.lanes;
    let width = theme.metric.scroll_bar;
    let track = Rect::from_min_max(pos2(lanes.right() - width, lanes.top()), lanes.max);
    let thumb_height = (track.height() * lanes.height() / geometry.content_height()).max(24.0);
    let travel = track.height() - thumb_height;
    let thumb_top = track.top() + travel * (geometry.scroll_y / max_scroll);
    let thumb = Rect::from_min_size(pos2(track.left() + 2.0, thumb_top), vec2(width - 4.0, thumb_height));

    let response = ui.interact(thumb.expand2(vec2(3.0, 0.0)), ui.scope_id().with("track-scroll"), Sense::drag());
    if response.dragged() && travel > 0.0 {
        let view = &mut cx.state.arrangement;
        view.scroll_y = (view.scroll_y + response.drag_delta().y / travel * max_scroll).clamp(0.0, max_scroll);
    }
    let active = response.hovered() || response.dragged();
    ui.painter().rect_filled(thumb, 0.0, if active { theme.ink_faint } else { theme.selected });
}

// ---- master ----------------------------------------------------------------

/// The master row: volume and level where the track headers are, and the
/// peak and the master's devices where the lanes are.
fn master(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let row = geometry.master;
    if row.height() < MASTER_HEIGHT {
        return;
    }
    let id = ui.scope_id().with("master");
    let head = row.with_max_x(row.left() + HEADER_WIDTH);
    let lane = row.with_min_x(head.right());
    let selected = cx.state.master_selected;
    let surface = if selected { theme.raised } else { theme.panel };
    let parts = HeaderParts::of(head, 4.0);

    let background = ui.interact(row, id.with("background"), Sense::click());
    let painter = ui.painter().clone();
    painter.rect_filled(lane, 0.0, theme.panel);
    painter.rect_filled(head, 0.0, surface);
    paint::hline(&painter, row.x_range(), row.top(), theme.hairline);
    paint::vline_left_of(&painter, head.right(), head.y_range(), theme.hairline);
    painter.rect_filled(Rect::from_min_max(head.min, pos2(head.left() + HEADER_STRIP, head.bottom())), 0.0, theme.ink);
    cx.hint(&background, "The master: everything passes through it \u{00B7} click to show its devices");
    if background.clicked() {
        cx.state.select_master();
    }

    let volume = cx.snap.project.master.volume_db;
    let bar_rect = Rect::from_min_size(
        pos2(parts.right - MASTER_VOLUME_WIDTH, (head.center().y - theme.metric.bar / 2.0).round()),
        vec2(MASTER_VOLUME_WIDTH, theme.metric.bar),
    );
    let title = Rect::from_min_max(pos2(parts.left, head.top()), pos2(bar_rect.left() - 6.0, head.bottom()));
    text::paint(&painter, title, Align2::LEFT_CENTER, theme.text.caps, "Master", theme.ink);
    let shown = units::format_db(volume);
    let bar = Bar::new("Vol", &shown, units::db_to_fader(volume)).default(units::db_to_fader(0.0));
    let output = daw_noir::bar(ui, theme, bar_rect, id.with("volume"), &bar);
    cx.hint(&output.response, format!("Master volume \u{00B7} {shown} \u{00B7} drag to set, double-click for 0 dB"));
    if output.pressed {
        cx.begin_gesture();
    }
    if let Some(position) = output.frac {
        let next = if output.reset { 0.0 } else { units::fader_to_db(position) };
        if next != volume {
            cx.edit(|project| project.set_master_volume(next));
        }
    }

    let meter = cx.live.master;
    daw_noir::stereo_meter(&painter, theme, parts.meter, widgets::levels(meter), widgets::meter_zones());

    // Peak readout. It takes the signal color once the output has clipped
    // and keeps it until clicked, so an over is not missed while looking
    // elsewhere.
    let mut x = lane.left() + 10.0;
    let rest = |x: f32| Rect::from_min_max(pos2(x, lane.top()), lane.max);
    let start = x;
    x += text::paint(&painter, rest(x), Align2::LEFT_CENTER, theme.text.micro, "Peak", theme.ink_faint) + 5.0;
    let value_style = daw_noir::TextStyle { size: 12.0, ..theme.text.value };
    // Wide enough for any level, so the chips after it never move.
    let width = text::width(&painter, value_style, "\u{2212}88.8 dB").ceil();
    let color = if meter.clipped { theme.signal } else { theme.ink };
    let value = Rect::from_min_size(pos2(x, lane.top()), vec2(width, lane.height()));
    text::paint(&painter, value, Align2::LEFT_CENTER, value_style, &units::format_level(meter.peak_db), color);
    x += width;
    let peak = Rect::from_min_max(pos2(start - 4.0, lane.top() + 6.0), pos2(x + 4.0, lane.bottom() - 6.0));
    let response = ui.interact(peak, id.with("peak"), Sense::click());
    cx.hint(&response, if meter.clipped { "The output clipped \u{00B7} click to reset" } else { "Output peak, dBFS" });
    if response.clicked() {
        cx.state.clear_clip_indicator = true;
    }
    x += 14.0;

    // The master's devices, in signal order. A click shows them.
    let chip_top = (lane.center().y - theme.metric.chip / 2.0).round();
    for device in &cx.snap.project.master.devices {
        let lit = selected && cx.state.selected_device == Some(device.id);
        let name = format!("{} on the master", device.name);
        let dot = if device.enabled { theme.ok } else { theme.ghost };
        let chip = Chip::new(&device.name, lit).dot(dot).named(&name);
        let rect = Rect::from_min_size(pos2(x, chip_top), vec2(chip.width(ui, theme), theme.metric.chip));
        if rect.right() > lane.right() - 8.0 {
            break;
        }
        let response = daw_noir::chip(ui, theme, rect, id.with(("device", device.id)), &chip);
        let state = if device.enabled { "on" } else { "bypassed" };
        cx.hint(&response, format!("{}, {state} \u{00B7} click to show the master\u{2019}s devices", device.name));
        if response.clicked() {
            cx.state.select_master();
            cx.state.selected_device = Some(device.id);
            cx.state.detail_tab = DetailTab::Devices;
        }
        x = rect.right() + 6.0;
    }
}

// ---- empty project ---------------------------------------------------------

/// What a project with no tracks shows instead of an empty grid: how to
/// begin, by hand or with an assistant.
fn empty_state(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    const WIDTH: f32 = 520.0;
    const STEPS: [(&str, &str); 3] = [
        ("Add a track", "It comes with a Poly Synth, so it makes sound right away."),
        ("Double-click a lane to create a clip", "Then click in the piano roll below to write notes."),
        ("Press Space to play", "Drag the bar at the top of the ruler to loop a section while you work."),
    ];
    let theme = cx.theme;
    let height: f32 = if cx.mcp_url.is_some() { 318.0 } else { 296.0 };
    let block = Rect::from_center_size(area.center(), vec2(WIDTH.min(area.width() - 32.0), height.min(area.height())));
    let block = block.translate(vec2(0.0, (area.top() + 12.0 - block.top()).max(0.0)));
    widgets::place(ui, block, "welcome", egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_clip_rect(area.intersect(ui.clip_rect()));
        ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
        let line = |ui: &mut Ui, style: daw_noir::TextStyle, text: &str, color: Color32, height: f32| {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
            text::paint(ui.painter(), rect, Align2::LEFT_CENTER, style, text, color);
        };
        line(ui, theme.text.caps, "New project", theme.ink_faint, 16.0);
        ui.add_space(6.0);
        let title = daw_noir::TextStyle { size: 20.0, ..theme.text.body };
        line(ui, title, "Start your song", theme.ink, 28.0);
        line(
            ui,
            theme.text.body,
            "This project is empty. A track, a clip and a few notes are all it takes.",
            theme.ink_dim,
            20.0,
        );
        ui.add_space(14.0);

        for (number, (title, detail)) in STEPS.iter().enumerate() {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
            let painter = ui.painter();
            let badge = Rect::from_min_size(rect.min + vec2(0.0, 1.0), vec2(18.0, 18.0));
            paint::frame(painter, badge, theme.hairline);
            text::paint(
                painter,
                badge,
                Align2::CENTER_CENTER,
                theme.text.value,
                &(number + 1).to_string(),
                theme.ink_dim,
            );
            let lines = rect.with_min_x(badge.right() + 10.0);
            let (upper, lower) = lines.split_top_bottom_at_y(rect.top() + 19.0);
            text::paint(painter, upper, Align2::LEFT_CENTER, theme.text.body, title, theme.ink);
            let quiet = daw_noir::TextStyle { size: 12.0, ..theme.text.body };
            text::paint(
                painter,
                lower.with_max_y(lower.top() + 16.0),
                Align2::LEFT_CENTER,
                quiet,
                detail,
                theme.ink_dim,
            );
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme.metric.gap;
            if daw_noir::button(ui, theme, "Add first track", Variant::Primary).clicked() {
                add_track(cx);
            }
            if daw_noir::button(ui, theme, "Open demo song", Variant::Default).clicked() {
                load_demo(cx);
            }
            if daw_noir::button(ui, theme, "Open project\u{2026}", Variant::Default).clicked() {
                super::files::open(ui.ctx(), cx);
            }
        });

        ui.add_space(18.0);
        let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
        ui.add_space(10.0);
        line(ui, theme.text.caps, "Working with an AI assistant", theme.ink_faint, 16.0);
        ui.add_space(4.0);
        let quiet = daw_noir::TextStyle { size: 12.0, ..theme.text.body };
        for sentence in [
            "An assistant connected through MCP can add tracks, write parts and mix while you watch.",
            "Every change appears here as it happens, and Ctrl+Z undoes it like any other edit.",
        ] {
            line(ui, quiet, sentence, theme.ink_dim, 18.0);
        }
        if let Some(url) = cx.mcp_url {
            ui.add_space(4.0);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            let used = text::paint(
                ui.painter(),
                rect,
                Align2::LEFT_CENTER,
                theme.text.micro,
                "Connect it to",
                theme.ink_faint,
            );
            let address = rect.with_min_x(rect.left() + used + 6.0);
            text::paint(ui.painter(), address, Align2::LEFT_CENTER, theme.text.value, url, theme.ink);
        }
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
    fn the_area_is_split_into_five_regions() {
        let g = geometry(3);
        assert_eq!(g.corner, Rect::from_min_max(pos2(0.0, 100.0), pos2(HEADER_WIDTH, 100.0 + RULER_HEIGHT)));
        assert_eq!(g.lanes.min, pos2(HEADER_WIDTH, 100.0 + RULER_HEIGHT));
        assert_eq!(g.lanes.max, pos2(1000.0, 500.0 - MASTER_HEIGHT));
        assert_eq!(g.ruler.bottom(), g.lanes.top());
        assert_eq!(g.headers.right(), g.lanes.left());
        // The master is pinned under both the headers and the lanes.
        assert_eq!(g.master, Rect::from_min_max(pos2(0.0, 500.0 - MASTER_HEIGHT), pos2(1000.0, 500.0)));
        assert_eq!(g.headers.bottom(), g.master.top());
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
        assert_eq!(tall.max_scroll(), 10.0 * TRACK_HEIGHT - tall.lanes.height());
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

    #[test]
    fn strong_grid_lines_are_bars_then_every_fourth_line() {
        use GridLevel::*;
        let near = geometry(1);
        assert!(near.is_strong_line(8.0, Bar));
        assert!(!near.is_strong_line(9.0, Beat), "between bar lines the grid is faint");

        // One line per bar, as when a whole song is in view: every fourth
        // bar carries the eye.
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 400.0));
        let song = Geometry::new(area, TimeView::new(8.0), 0.0, 1, TimeSignature::default());
        assert_eq!(song.grid, 4.0);
        assert!(song.is_strong_line(0.0, Bar) && song.is_strong_line(16.0, Bar));
        assert!(!song.is_strong_line(4.0, Bar) && !song.is_strong_line(12.0, Bar));
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
    fn clips_keep_a_pixel_clear_of_their_row_and_of_each_other() {
        let (project, _, early, _) = project_with_clips();
        let g = geometry(2);
        let rect = g.clip_rect(1, project.clip(early).unwrap());
        let row_top = g.row_top(1);
        assert_eq!(rect.top(), row_top + 1.0);
        assert_eq!(rect.bottom(), row_top + TRACK_HEIGHT - 2.0, "one pixel above the row\u{2019}s hairline");
        assert_eq!((rect.left(), rect.right()), (g.x(4.0), g.x(8.0) - 1.0), "a gap before the clip that follows");
    }

    #[test]
    fn the_loop_bar_has_two_ends_and_a_body() {
        let g = geometry(1);
        let region = LoopRegion { enabled: true, start: 8.0, end: 16.0 };
        let y = g.ruler.top() + 3.0;
        assert_eq!(loop_hit(region, &g, pos2(g.x(8.0) + 2.0, y)), Some(LoopHit::Start));
        assert_eq!(loop_hit(region, &g, pos2(g.x(16.0) - 2.0, y)), Some(LoopHit::End));
        assert_eq!(loop_hit(region, &g, pos2(g.x(12.0), y)), Some(LoopHit::Body));
        assert_eq!(loop_hit(region, &g, pos2(g.x(20.0), y)), None);
        assert_eq!(loop_hit(region, &g, pos2(g.x(12.0), g.ruler.bottom() - 2.0)), None, "below the bar is for seeking");
        // A very short region can still be stretched: its end wins.
        let tiny = LoopRegion { enabled: true, start: 8.0, end: 8.25 };
        assert_eq!(loop_hit(tiny, &g, pos2(g.x(8.1), y)), Some(LoopHit::End));
    }
}
