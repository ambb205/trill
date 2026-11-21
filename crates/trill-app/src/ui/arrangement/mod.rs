//! The arrangement: track headers on the left, a ruler on top, the clips
//! and automation of every track on a shared timeline, and the master
//! pinned along the bottom.
//!
//! Everything is placed by hand from a [`Geometry`] computed once per frame,
//! and painted from the snapshot. Edits go to the session and show up on the
//! next frame, when the snapshot has caught up.
//!
//! The parts: [`rows`] decides which rows there are, [`clips`] paints clips
//! and [`lanes`] handles what the pointer does to them, [`ops`] holds the
//! commands on the selection and on a stretch of time (the edits of time
//! themselves are the model's), [`envelopes`] is the automation lanes
//! (and what the piano roll's controller lane shares with them),
//! [`ruler`] the loop, markers and time selection, [`headers`] the track
//! headers, [`corner`] the grid and tool controls above them, [`master`]
//! the master row, and [`reveal`] brings tracks and a stretch of time into
//! view when something asks for them.

mod clips;
mod corner;
mod empty;
pub(super) mod envelopes;
mod headers;
mod lanes;
mod master;
pub(super) mod ops;
mod reveal;
mod rows;
mod ruler;

pub(super) use clips::{allowed_length, fade_level};
pub use envelopes::{delete_selected as delete_selected_points, select_all as select_all_points};
pub use headers::{add_group_track, add_return_track, add_track, add_track_with, group_selected_track};
pub(super) use headers::{pan_bar, send_bar};
pub use reveal::Reveal;
pub(super) use rows::display_order;
// Where things are, for the interaction tests that click on them.
#[cfg(test)]
pub(super) use headers::{
    HEADER_CHIP, HEADER_CONTROLS_TOP, HEADER_INDEX, HEADER_LEFT, HEADER_NAME_TOP, HEADER_PAN_WIDTH, HEADER_RIGHT,
};
#[cfg(test)]
pub(super) use master::MASTER_VOLUME_WIDTH;
pub(super) use ruler::RULER_HEIGHT;
#[cfg(test)]
pub(super) use ruler::{LOOP_STRIP, MARKER_STRIP};

use super::state::{ArrangeDrag, ArrangementState, Cx};
use super::timeline::{self, GridChoice, GridLevel, TimeView};
use eframe::egui::{self, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use rows::{Row, Rows};
use trill_core::{Beats, Clip, MeterMap, TimeSignature};
use trill_noir::paint;

pub(super) const HEADER_WIDTH: f32 = 232.0;
pub(super) const TRACK_HEIGHT: f32 = ArrangementState::DEFAULT_TRACK_HEIGHT;
pub(super) const MASTER_HEIGHT: f32 = 36.0;
/// Grid lines closer together than this are dropped.
const GRID_MIN_PIXELS: f64 = 18.0;
/// A fixed grid is drawn as chosen while its lines are at least this far
/// apart; closer, the lines thin out but snapping stays as chosen.
const FIXED_GRID_MIN_PIXELS: f64 = 9.0;
/// Bar numbers closer together than this are dropped.
const LABEL_MIN_PIXELS: f64 = 46.0;
/// The shortest clip an edit may leave behind when nothing is snapping.
const SHORTEST_CLIP: Beats = 1.0 / 16.0;

/// Where everything is this frame.
#[derive(Clone, Debug)]
pub(super) struct Geometry {
    corner: Rect,
    ruler: Rect,
    headers: Rect,
    lanes: Rect,
    /// The master's automation lanes, pinned above its row; empty when it
    /// has none open.
    master_lanes: Rect,
    /// The master row, across the whole width under the tracks.
    master: Rect,
    time: TimeView,
    scroll_y: f32,
    /// The song's bars, through every change of time signature, and the
    /// signature it begins in: the grid's steps are chosen for that one,
    /// and each other gets what they come to in it.
    meter: MeterMap,
    signature: TimeSignature,
    beats_per_bar: Beats,
    /// Spacing of the finest grid lines drawn, in beats.
    grid: Beats,
    /// What edits snap to, in beats; `None` while snapping is off.
    snap: Option<Beats>,
    rows: Rows,
}

impl Geometry {
    fn new(area: Rect, view: &ArrangementState, rows: Rows, meter: MeterMap) -> Self {
        let signature = meter.signature_at(0.0);
        let split_x = area.left() + HEADER_WIDTH;
        let split_y = area.top() + RULER_HEIGHT;
        let master_top = (area.bottom() - MASTER_HEIGHT).max(split_y);
        // The master's lanes give way before the tracks disappear.
        let room = (master_top - split_y - TRACK_HEIGHT).max(0.0);
        let lanes_bottom = master_top - rows.master_lanes_height(view).min(room);
        let mut geometry = Self {
            corner: Rect::from_min_max(area.min, pos2(split_x, split_y)),
            ruler: Rect::from_min_max(pos2(split_x, area.top()), pos2(area.right(), split_y)),
            headers: Rect::from_min_max(pos2(area.left(), split_y), pos2(split_x, lanes_bottom)),
            lanes: Rect::from_min_max(pos2(split_x, split_y), pos2(area.right(), lanes_bottom)),
            master_lanes: Rect::from_min_max(pos2(area.left(), lanes_bottom), pos2(area.right(), master_top)),
            master: Rect::from_min_max(pos2(area.left(), master_top), area.max),
            time: view.time,
            scroll_y: view.scroll_y,
            meter,
            signature,
            beats_per_bar: signature.beats_per_bar(),
            grid: 1.0,
            snap: None,
            rows,
        };
        geometry.set_time(view);
        geometry
    }

    /// Takes over the view's zoom, scroll and grid, after they changed.
    fn set_time(&mut self, view: &ArrangementState) {
        self.time = view.time;
        self.scroll_y = view.scroll_y;
        let pixels_per_beat = view.time.pixels_per_beat;
        let adaptive = timeline::grid_step(pixels_per_beat, self.signature, GRID_MIN_PIXELS);
        self.grid = match view.grid {
            GridChoice::Fixed(snap) => match snap.step() {
                Some(step) if step * pixels_per_beat >= FIXED_GRID_MIN_PIXELS => step,
                _ => adaptive,
            },
            GridChoice::Adaptive => adaptive,
        };
        // Zoomed far out, lines are several bars apart, but edits should
        // still land on any bar.
        self.snap = view.grid.step(adaptive.min(self.beats_per_bar)).filter(|_| view.snap);
    }

    fn x(&self, beat: Beats) -> f32 {
        self.time.beat_to_x(self.lanes.left(), beat)
    }

    fn beat(&self, x: f32) -> Beats {
        self.time.x_to_beat(self.lanes.left(), x)
    }

    /// Screen y of a height in the track list.
    fn screen_y(&self, content_y: f32) -> f32 {
        self.lanes.top() - self.scroll_y + content_y
    }

    /// Height in the track list of screen position `y`.
    fn content_y(&self, y: f32) -> f32 {
        y - self.lanes.top() + self.scroll_y
    }

    /// The row at screen position `y`, if there is one there.
    fn row_at(&self, y: f32) -> Option<&Row> {
        self.rows.at(self.content_y(y))
    }

    /// The part of the lanes `row` covers, before clipping.
    fn row_rect(&self, row: &Row) -> Rect {
        let top = self.screen_y(row.top);
        Rect::from_x_y_ranges(self.lanes.x_range(), top..=top + row.height)
    }

    /// The header of `row`, before clipping.
    fn header_rect(&self, row: &Row) -> Rect {
        let top = self.screen_y(row.top);
        Rect::from_x_y_ranges(self.headers.x_range(), top..=top + row.height)
    }

    fn is_visible(&self, row: &Row) -> bool {
        let top = self.screen_y(row.top);
        top < self.lanes.bottom() && top + row.height > self.lanes.top()
    }

    fn max_scroll(&self) -> f32 {
        (self.rows.height - self.lanes.height()).max(0.0)
    }

    /// Snaps a beat to the grid, unless `free` (Alt held) or snapping is
    /// off.
    fn snap(&self, beat: Beats, free: bool) -> Beats {
        match self.snap {
            Some(step) if !free => timeline::snap_in(beat, &self.meter, step),
            _ => beat.max(0.0),
        }
    }

    /// The step arrow keys nudge by and commands round to: the snap grid,
    /// or what the zoom shows when snapping is off.
    fn step(&self) -> Beats {
        self.snap.unwrap_or(self.grid.min(self.beats_per_bar))
    }

    /// The shortest clip a trim may leave behind.
    fn min_clip_length(&self, free: bool) -> Beats {
        match self.snap {
            Some(step) if !free => step,
            _ => SHORTEST_CLIP,
        }
    }

    /// Screen rectangle of a clip in `row`, before clipping: a pixel clear
    /// of the row's edges and of the clip that follows it.
    fn clip_rect(&self, row: &Row, clip: &Clip) -> Rect {
        let top = self.screen_y(row.top);
        let left = self.x(clip.start).round();
        let right = (self.x(clip.end()).round() - 1.0).max(left + 1.0);
        Rect::from_min_max(pos2(left, top + 1.0), pos2(right, top + row.height - 2.0))
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

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let area = ui.max_rect();
    ui.painter().rect_filled(area, 0.0, cx.theme.bg);

    let meter = cx.snap.project.meter_map();
    let signature = cx.snap.project.time_signature;
    let track_count = cx.snap.project.tracks.len();
    let now = ui.input(|input| input.time);
    let view = &mut cx.state.arrangement;
    if !view.framed && track_count > 0 {
        // Open a project showing all of it, or a few bars of a new one.
        view.framed = true;
        view.time = fit_song(cx.snap.project.length_shown(), signature, area.width() - HEADER_WIDTH);
    }
    if let Some(glide) = view.glide {
        let (time, arrived) = glide.at(now);
        view.time = time;
        if arrived {
            view.glide = None;
        }
        ui.ctx().request_repaint();
    }
    let rows = Rows::build(&cx.snap.project.tracks, view);
    let mut geometry = Geometry::new(area, view, rows, meter);
    if track_count == 0 {
        // Nothing to arrange yet: say how to begin, above the master.
        let welcome = Rect::from_min_max(area.min, pos2(area.right(), geometry.master.top()));
        empty::show(ui, cx, welcome);
        empty::sound_drop(ui, cx, welcome);
        empty::preset_drop(ui, cx, welcome);
        master::show(ui, cx, &geometry);
        return;
    }
    // A drag ends when the button comes up, wherever the pointer is by
    // then, even outside the window.
    if !ui.input(|input| input.pointer.primary_down())
        && let Some(drag) = cx.state.arrangement.drag.take()
    {
        finish_drag(cx, drag);
    }

    scroll_and_zoom(ui, cx, &mut geometry);
    follow_playhead(cx, &mut geometry);
    reveal_lane(cx, &mut geometry);
    reveal::carry_out(ui, cx, &mut geometry);
    // Commands run before the layout; they go by what it was last frame.
    cx.state.arrangement.step = geometry.step();
    cx.state.arrangement.lanes_width = geometry.lanes.width();

    lanes::show(ui, cx, &geometry);
    ruler::show(ui, cx, &geometry);
    cursors(ui, cx, &geometry);
    headers::show(ui, cx, &geometry);
    corner::show(ui, cx, &geometry);
    scroll_bar(ui, cx, &geometry);
    master::show(ui, cx, &geometry);
}

/// The view that shows a song `length` beats long across `width` pixels,
/// or eight bars of a song that is still empty.
fn fit_song(length: Beats, signature: TimeSignature, width: f32) -> TimeView {
    let mut time = TimeView::new(1.0);
    time.fit(0.0, length.max(8.0 * signature.beats_per_bar()), width);
    time.origin = 0.0;
    time
}

/// What a drag leaves to do once the button is up.
fn finish_drag(cx: &mut Cx, drag: ArrangeDrag) {
    if let ArrangeDrag::MoveTrack { track, drop: Some(drop) } = drag {
        headers::drop_track(cx, track, drop);
    }
}

/// Wheel pans along the timeline, Ctrl+wheel zooms around the pointer,
/// Alt+wheel (or the wheel over the headers) scrolls the track list, and
/// Ctrl+Alt+wheel (or Ctrl+wheel over the headers) zooms the tracks' height.
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
    if zoom != 1.0 && (over_headers || alt) {
        zoom_tracks(view, geometry, pointer.y, zoom);
    } else if zoom != 1.0 {
        view.glide = None;
        view.time.zoom_around(geometry.lanes.left(), pointer.x, zoom as f64);
    }
    if over_headers || alt {
        view.scroll_y -= scroll.y;
    } else if scroll != egui::Vec2::ZERO {
        // A plain wheel reports vertical motion; pads and Shift+wheel
        // report horizontal. Either pans the timeline.
        view.glide = None;
        view.time.pan_pixels(-(scroll.x + scroll.y));
    }
    let rows = Rows::build(&cx.snap.project.tracks, view);
    geometry.rows = rows;
    view.scroll_y = view.scroll_y.clamp(0.0, geometry.max_scroll());
    geometry.set_time(view);
}

/// Scales the height of every track, keeping what is under the pointer
/// where it is.
fn zoom_tracks(view: &mut ArrangementState, geometry: &Geometry, pointer_y: f32, factor: f32) {
    let (low, high) = (ArrangementState::MIN_TRACK_HEIGHT, ArrangementState::MAX_TRACK_HEIGHT);
    let before = view.track_height;
    let after = (before * factor).clamp(low, high).round();
    if after == before {
        return;
    }
    // Tracks with a height of their own scale along.
    for height in view.heights.values_mut() {
        *height = (*height * after / before).clamp(low, high).round();
    }
    view.track_height = after;
    let anchor = geometry.content_y(pointer_y);
    view.scroll_y += anchor * (after / before - 1.0);
}

/// Pages the view forward when playback runs off its right edge.
fn follow_playhead(cx: &mut Cx, geometry: &mut Geometry) {
    let view = &mut cx.state.arrangement;
    if !cx.live.playing || !view.follow || view.drag.is_some() || view.glide.is_some() {
        return;
    }
    let x = geometry.x(cx.live.position);
    if x > geometry.lanes.right() - 24.0 || x < geometry.lanes.left() {
        let lead_in = geometry.lanes.width() as f64 * 0.08 / geometry.time.pixels_per_beat;
        view.time.origin = (cx.live.position - lead_in).max(0.0);
        geometry.set_time(view);
    }
}

/// Scrolls the track list so a lane that was just asked for is in view.
fn reveal_lane(cx: &mut Cx, geometry: &mut Geometry) {
    let view = &mut cx.state.arrangement;
    let Some(target) = view.reveal.take() else { return };
    let Some(lane) = view.lanes.iter().position(|lane| lane.target == target) else { return };
    let Some(row) = geometry.rows.lane(lane) else { return };
    let visible = geometry.lanes.height();
    if row.top < view.scroll_y {
        // Keep the track the lane belongs to in view above it.
        view.scroll_y = (row.top - TRACK_HEIGHT).max(0.0);
    } else if row.bottom() > view.scroll_y + visible {
        view.scroll_y = row.bottom() - visible;
    }
    view.scroll_y = view.scroll_y.clamp(0.0, geometry.max_scroll());
    geometry.scroll_y = view.scroll_y;
}

/// The edit cursor and, over it, the playhead.
fn cursors(ui: &Ui, cx: &Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let area = geometry.ruler.union(geometry.lanes).union(geometry.master_lanes.with_min_x(geometry.lanes.left()));
    let painter = ui.painter_at(area);
    let on_screen = |x: f32| x >= area.left() - 6.0 && x <= area.right() + 6.0;

    // The edit cursor: a quiet line from the foot of the ruler down, with
    // a notch where it leaves the ruler. It marks a place, where the
    // playhead marks a moment.
    let x = geometry.x(cx.state.arrangement.cursor).round();
    if on_screen(x) {
        let top = geometry.ruler.bottom() - ruler::TIME_STRIP;
        paint::vline(&painter, x, top..=area.bottom(), theme.ink_faint);
        let tip = pos2(x + 0.5, geometry.ruler.bottom());
        let notch = vec![tip + vec2(-4.0, -5.0), tip + vec2(4.0, -5.0), tip];
        painter.add(Shape::convex_polygon(notch, theme.ink_dim, Stroke::NONE));
    }

    // Where the others' songs stand, each in its person's colour: under
    // one's own playhead, which is the one that is heard here.
    crate::ui::together::playheads(&painter, cx, area, |beat| geometry.x(beat));

    let x = geometry.x(cx.live.position).round();
    if on_screen(x) {
        paint::vline(&painter, x, area.y_range(), theme.signal);
        // A small flag where the line meets the top of the ruler.
        let tip = pos2(x + 0.5, area.top());
        let flag = vec![tip + vec2(-4.5, 0.0), tip + vec2(4.5, 0.0), tip + vec2(0.0, 6.0)];
        painter.add(Shape::convex_polygon(flag, theme.signal, Stroke::NONE));
    }
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
    let thumb_height = (track.height() * lanes.height() / geometry.rows.height).max(24.0);
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

/// Zooms to the time selection, or to the selected clips: `Z`. With
/// neither, to the whole song.
pub fn zoom_to_selection(ctx: &egui::Context, cx: &mut Cx) {
    let Some((start, end)) = ops::selected_span(cx) else { return zoom_to_song(ctx, cx) };
    let mut time = TimeView::new(1.0);
    time.fit(start, end, cx.state.arrangement.lanes_width);
    let now = ctx.input(|input| input.time);
    cx.state.arrangement.glide_to(time, now);
}

/// Zooms out to the whole song: `Shift+Z`.
pub fn zoom_to_song(ctx: &egui::Context, cx: &mut Cx) {
    let project = &cx.snap.project;
    let time = fit_song(project.length_shown(), project.time_signature, cx.state.arrangement.lanes_width);
    let now = ctx.input(|input| input.time);
    cx.state.arrangement.glide_to(time, now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::DetailTab;
    use crate::ui::arrangement::rows::RowKind;
    use crate::ui::state::{LaneOwner, UiState};
    use crate::ui::timeline::Snap;
    use trill_core::{AutomationTarget, Project};

    fn view(pixels_per_beat: f64) -> ArrangementState {
        let mut view = UiState::new(DetailTab::Clip).arrangement;
        view.time = TimeView { origin: 4.0, pixels_per_beat };
        view
    }

    fn geometry_of(project: &Project, view: &ArrangementState) -> Geometry {
        let area = Rect::from_min_size(pos2(0.0, 100.0), vec2(1000.0, 400.0));
        Geometry::new(area, view, Rows::build(&project.tracks, view), project.meter_map())
    }

    fn geometry(track_count: usize) -> Geometry {
        let mut project = Project::new("t");
        for index in 0..track_count {
            project.add_track(format!("Track {index}"));
        }
        geometry_of(&project, &view(20.0))
    }

    #[test]
    fn the_area_is_split_into_its_regions() {
        let g = geometry(3);
        assert_eq!(g.corner, Rect::from_min_max(pos2(0.0, 100.0), pos2(HEADER_WIDTH, 100.0 + RULER_HEIGHT)));
        assert_eq!(g.lanes.min, pos2(HEADER_WIDTH, 100.0 + RULER_HEIGHT));
        assert_eq!(g.lanes.max, pos2(1000.0, 500.0 - MASTER_HEIGHT));
        assert_eq!(g.ruler.bottom(), g.lanes.top());
        assert_eq!(g.headers.right(), g.lanes.left());
        // The master is pinned under both the headers and the lanes.
        assert_eq!(g.master, Rect::from_min_max(pos2(0.0, 500.0 - MASTER_HEIGHT), pos2(1000.0, 500.0)));
        assert_eq!(g.headers.bottom(), g.master.top());
        assert_eq!(g.master_lanes.height(), 0.0, "no master lane is open");
        // Beat 4 is at the left edge of the lanes; a bar later is 80 px on.
        assert_eq!(g.x(4.0), HEADER_WIDTH);
        assert_eq!(g.x(8.0), HEADER_WIDTH + 80.0);
        assert_eq!(g.beat(HEADER_WIDTH + 40.0), 6.0);
    }

    #[test]
    fn the_masters_lanes_are_pinned_above_its_row() {
        let mut project = Project::new("t");
        project.add_track("A");
        let mut view = view(20.0);
        view.open_lane(LaneOwner::Master, AutomationTarget::MasterVolume);
        let g = geometry_of(&project, &view);
        let lane = ArrangementState::DEFAULT_LANE_HEIGHT;
        assert_eq!(g.master_lanes.height(), lane);
        assert_eq!(g.master_lanes.bottom(), g.master.top());
        assert_eq!(g.lanes.bottom(), g.master_lanes.top());
        assert_eq!(g.master_lanes.left(), 0.0, "its header sits under the track headers");

        // In a window too short for both, the tracks keep a row.
        let short = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, RULER_HEIGHT + MASTER_HEIGHT + 90.0));
        let cramped = Geometry::new(short, &view, Rows::build(&project.tracks, &view), project.meter_map());
        assert_eq!(cramped.lanes.height(), TRACK_HEIGHT);
        assert_eq!(cramped.master_lanes.height(), 30.0);
    }

    #[test]
    fn rows_map_to_screen_positions() {
        let g = geometry(3);
        let top = g.lanes.top();
        assert!(g.row_at(top - 1.0).is_none());
        assert_eq!(g.row_at(top).unwrap().kind, RowKind::Track { index: 0, member: false });
        assert_eq!(g.row_at(top + TRACK_HEIGHT - 0.5).unwrap().kind, RowKind::Track { index: 0, member: false });
        assert_eq!(g.row_at(top + TRACK_HEIGHT).unwrap().kind, RowKind::Track { index: 1, member: false });
        assert!(g.row_at(top + 3.0 * TRACK_HEIGHT + 1.0).is_none(), "below the last track");
        assert_eq!(g.content_y(g.screen_y(77.0)), 77.0);

        let mut scrolled = g.clone();
        scrolled.scroll_y = TRACK_HEIGHT;
        assert_eq!(scrolled.row_at(top).unwrap().kind, RowKind::Track { index: 1, member: false });
        assert_eq!(scrolled.row_rect(&scrolled.rows.rows[1]).top(), top);
        assert!(!scrolled.is_visible(&scrolled.rows.rows[0]), "scrolled out above the lanes");
        assert!(scrolled.is_visible(&scrolled.rows.rows[1]));
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
        assert_eq!(g.min_clip_length(true), SHORTEST_CLIP);
        assert_eq!(g.step(), 1.0);

        let project = {
            let mut project = Project::new("t");
            project.add_track("A");
            project
        };
        let far = geometry_of(&project, &view(2.0));
        assert_eq!(far.grid, 16.0, "zoomed out, lines are four bars apart");
        assert_eq!(far.snap(9.0, false), 8.0, "but clips still snap to single bars");
        assert_eq!(far.snap(11.0, false), 12.0);
        assert_eq!(far.step(), 4.0);
    }

    #[test]
    fn a_fixed_grid_snaps_as_chosen_and_draws_what_fits() {
        let mut project = Project::new("t");
        project.add_track("A");
        let mut view = view(20.0);
        view.grid = GridChoice::Fixed(Snap::Sixteenth);
        let g = geometry_of(&project, &view);
        assert_eq!(g.snap, Some(0.25));
        assert_eq!(g.snap(5.4, false), 5.5);
        assert_eq!(g.grid, 1.0, "sixteenths 5 px apart would be noise, so the lines stay at beats");
        assert_eq!(g.step(), 0.25);

        view.time.pixels_per_beat = 80.0;
        assert_eq!(geometry_of(&project, &view).grid, 0.25);
        view.grid = GridChoice::Fixed(Snap::EighthTriplet);
        let triplets = geometry_of(&project, &view);
        assert!((triplets.snap(5.3, false) - (5.0 + 1.0 / 3.0)).abs() < 1e-9);

        // Off, or the snap toggle, lets everything land anywhere.
        view.grid = GridChoice::Fixed(Snap::Off);
        assert_eq!(geometry_of(&project, &view).snap(5.4, false), 5.4);
        view.grid = GridChoice::Adaptive;
        view.snap = false;
        let loose = geometry_of(&project, &view);
        assert_eq!(loose.snap(5.4, false), 5.4);
        assert_eq!(loose.step(), 0.25, "arrow keys still move by what the grid shows");
    }

    #[test]
    fn strong_grid_lines_are_bars_then_every_fourth_line() {
        use GridLevel::*;
        let near = geometry(1);
        assert!(near.is_strong_line(8.0, Bar));
        assert!(!near.is_strong_line(9.0, Beat), "between bar lines the grid is faint");

        // One line per bar, as when a whole song is in view: every fourth
        // bar carries the eye.
        let mut project = Project::new("t");
        project.add_track("A");
        let song = geometry_of(&project, &view(8.0));
        assert_eq!(song.grid, 4.0);
        assert!(song.is_strong_line(0.0, Bar) && song.is_strong_line(16.0, Bar));
        assert!(!song.is_strong_line(4.0, Bar) && !song.is_strong_line(12.0, Bar));
    }

    #[test]
    fn clips_keep_a_pixel_clear_of_their_row_and_of_each_other() {
        let mut project = Project::new("t");
        project.add_track("A");
        let second = project.add_track("B");
        let early = project.add_midi_clip(second, "early", 4.0, 4.0).unwrap();
        let g = geometry_of(&project, &view(20.0));
        let row = g.rows.track(1).unwrap();
        let rect = g.clip_rect(row, project.clip(early).unwrap());
        let row_top = g.screen_y(row.top);
        assert_eq!(rect.top(), row_top + 1.0);
        assert_eq!(rect.bottom(), row_top + TRACK_HEIGHT - 2.0, "one pixel above the row\u{2019}s hairline");
        assert_eq!((rect.left(), rect.right()), (g.x(4.0), g.x(8.0) - 1.0), "a gap before the clip that follows");
    }

    #[test]
    fn zooming_track_height_keeps_the_row_under_the_pointer() {
        let mut project = Project::new("t");
        let ids: Vec<_> = (0..12).map(|index| project.add_track(format!("T{index}"))).collect();
        let mut view = view(20.0);
        view.heights.insert(ids[0], 120.0);
        view.scroll_y = 200.0;
        let g = geometry_of(&project, &view);
        let pointer = g.lanes.top() + 100.0;
        let before = g.content_y(pointer) / g.rows.height;

        zoom_tracks(&mut view, &g, pointer, 1.5);
        assert_eq!(view.track_height, 90.0);
        assert_eq!(view.heights[&ids[0]], 180.0, "a track with its own height scales along");
        let zoomed = geometry_of(&project, &view);
        let after = zoomed.content_y(pointer) / zoomed.rows.height;
        assert!((after - before).abs() < 0.01, "{before} then {after}");

        zoom_tracks(&mut view, &zoomed, pointer, 100.0);
        assert_eq!(view.track_height, ArrangementState::MAX_TRACK_HEIGHT);
        zoom_tracks(&mut view, &zoomed, pointer, 0.0001);
        assert_eq!(view.track_height, ArrangementState::MIN_TRACK_HEIGHT);
    }
}
