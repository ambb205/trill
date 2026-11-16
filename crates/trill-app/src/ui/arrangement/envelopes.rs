//! Automation lanes: an envelope drawn in its parameter's units under the
//! track it belongs to, and the pointer editing it. Click adds a point,
//! points and lines drag, Alt bends a line, the pencil writes steps.
//!
//! A lane can be switched off, which is what taking hold of an automated
//! control does: its envelope stays, drawn stepped back, and drives
//! nothing. The broken line across it is then the value in force, the one
//! the control was given by hand, and the switch in the lane's header
//! turns the envelope back on. An envelope that is off can still be edited.
//!
//! Points have no ids, so like notes they are followed by value. Every
//! step of a drag is derived from the points as they were when it began
//! and swaps them for the previous step's in the project, inside one undo
//! gesture; the swap is done against the project as it is at that moment,
//! so an edit made elsewhere meanwhile is kept.
//!
//! The first part of this file, the envelope as geometry, is shared with
//! the controller lane of the piano roll (`controller_lane`), which edits a
//! clip's controller curves with the same gestures.

use super::Geometry;
use crate::ui::automation::{self, Choice, Scale};
use crate::ui::state::{ArrangeDrag, Cx, FocusArea, LaneDrag, LaneOwner, LaneView, PointSelection, Tool};
use crate::ui::{colors, timeline};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, Id, Mesh, PointerButton, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui,
    WidgetInfo, pos2, vec2,
};
use trill_core::project::automation_value;
use trill_core::{AutomationPoint, AutomationTarget, Beats, Segment};
use trill_noir::{Icon, MenuItem, fade, paint, text};

/// Space kept clear above the highest and below the lowest value, so a
/// point at either extreme is whole.
pub(in crate::ui) const PAD: f32 = 6.0;
/// Side of a point's square.
pub(in crate::ui) const POINT: f32 = 5.0;
/// How close, in pixels, the pointer must be to a point or a line to grab
/// it.
const POINT_GRAB: f32 = 6.0;
const LINE_GRAB: f32 = 5.0;
/// Distance between samples of a curved segment, in pixels.
const CURVE_STEP: f32 = 3.0;
pub(in crate::ui) const LINE_WIDTH: f32 = 1.25;
/// Where a lane header's content starts: indented under its track's name.
const HEADER_INDENT: f32 = 26.0;
/// Where the switch of a lane's envelope is, from the header's left edge:
/// under its track's number.
const SWITCH_LEFT: f32 = super::headers::HEADER_LEFT + 1.0;
/// How long the dashes of a value nothing automates are, and the gaps
/// between them.
const DASH: f32 = 4.0;
/// A chooser listing more parameters than this gets a search field.
const SEARCH_ABOVE: usize = 14;

// ---- the envelope as geometry, pure ------------------------------------------

/// Where a lane's beats and values are on screen.
#[derive(Clone, Debug)]
pub(in crate::ui) struct LaneMap {
    pub rect: Rect,
    /// Screen x of the view's origin beat.
    pub left: f32,
    pub time: timeline::TimeView,
    pub scale: Scale,
}

impl LaneMap {
    fn new(geometry: &Geometry, rect: Rect, scale: Scale) -> Self {
        Self { rect, left: geometry.lanes.left(), time: geometry.time, scale }
    }

    pub fn x(&self, beat: Beats) -> f32 {
        self.time.beat_to_x(self.left, beat)
    }

    pub fn beat(&self, x: f32) -> Beats {
        self.time.x_to_beat(self.left, x).max(0.0)
    }

    /// Pixels between the lowest and the highest value.
    pub fn travel(&self) -> f32 {
        (self.rect.height() - 2.0 * PAD).max(1.0)
    }

    /// Screen y of height `position` (0 at the bottom, 1 at the top).
    pub fn y_at(&self, position: f32) -> f32 {
        self.rect.bottom() - PAD - position.clamp(0.0, 1.0) * self.travel()
    }

    pub fn y(&self, value: f64) -> f32 {
        self.y_at(self.scale.position(value))
    }

    /// Height in the lane of screen position `y`.
    pub fn position(&self, y: f32) -> f32 {
        ((self.rect.bottom() - PAD - y) / self.travel()).clamp(0.0, 1.0)
    }

    pub fn value(&self, y: f32) -> f64 {
        self.scale.value(self.position(y))
    }

    pub fn point(&self, point: &AutomationPoint) -> Pos2 {
        pos2(self.x(point.beat), self.y(point.value))
    }
}

/// The point nearest to `position`, if one is within reach.
pub(in crate::ui) fn point_at(points: &[AutomationPoint], map: &LaneMap, position: Pos2) -> Option<AutomationPoint> {
    let distance = |point: &AutomationPoint| {
        let at = map.point(point);
        (at.x - position.x).abs().max((at.y - position.y).abs())
    };
    // Only the points near the pointer in time are worth measuring.
    let reach = POINT_GRAB as f64 / map.time.pixels_per_beat;
    let beat = map.time.x_to_beat(map.left, position.x);
    let from = points.partition_point(|point| point.beat < beat - reach);
    let near = points[from..].iter().take_while(|point| point.beat <= beat + reach);
    // Of two points at one place the later one is on top.
    near.filter(|point| distance(point) <= POINT_GRAB)
        .min_by(|a, b| distance(a).total_cmp(&distance(b)).then(std::cmp::Ordering::Greater))
        .copied()
}

/// The line under `position`: the two points it joins. `None` over a step
/// (which is moved by its points) and off the ends of the envelope.
pub(in crate::ui) fn segment_at(
    points: &[AutomationPoint],
    map: &LaneMap,
    position: Pos2,
) -> Option<[AutomationPoint; 2]> {
    let beat = map.time.x_to_beat(map.left, position.x);
    let next = points.partition_point(|point| point.beat <= beat);
    if next == 0 || next == points.len() {
        return None;
    }
    let on_line = (map.y(automation_value(points, beat)) - position.y).abs() <= LINE_GRAB;
    on_line.then(|| [points[next - 1], points[next]])
}

/// `points` without one instance of each point in `remove`, plus `add`,
/// in time order.
pub(in crate::ui) fn replaced(
    points: &[AutomationPoint],
    remove: &[AutomationPoint],
    add: &[AutomationPoint],
) -> Vec<AutomationPoint> {
    let mut result = points.to_vec();
    for point in remove {
        if let Some(index) = result.iter().position(|candidate| candidate == point) {
            result.remove(index);
        }
    }
    result.extend_from_slice(add);
    result.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    result
}

/// `originals` moved by `beats` in time and `height` up the lane, held back
/// as a group so none passes a point of `others` (the rest of the envelope)
/// or the start of the song, and each kept inside the scale.
pub(in crate::ui) fn moved(
    originals: &[AutomationPoint],
    others: &[AutomationPoint],
    beats: Beats,
    height: f32,
    scale: &Scale,
) -> Vec<AutomationPoint> {
    let mut earliest = f64::NEG_INFINITY;
    let mut latest = f64::INFINITY;
    for point in originals {
        let before = others.iter().rev().find(|other| other.beat <= point.beat).map_or(0.0, |other| other.beat);
        let after = others.iter().find(|other| other.beat >= point.beat).map_or(f64::INFINITY, |other| other.beat);
        earliest = earliest.max(before - point.beat);
        latest = latest.min(after - point.beat);
    }
    let beats = beats.clamp(earliest.min(0.0), latest.max(0.0));
    originals
        .iter()
        .map(|point| {
            let value = if height == 0.0 { point.value } else { scale.value(scale.position(point.value) + height) };
            AutomationPoint { beat: (point.beat + beats).max(0.0), value, ..*point }
        })
        .collect()
}

/// The bend a line takes when it is dragged `up` pixels in a lane whose
/// values span `travel` pixels. Dragging up bows the line upward whichever
/// way it runs, so the sign depends on whether it rises or falls.
pub(in crate::ui) fn bend(up: f32, travel: f32, rising: bool) -> f32 {
    let amount = (up / (travel * 0.5).max(1.0)).clamp(-1.0, 1.0);
    // A positive bend starts slowly: on a rising line that bows downward.
    if rising { -amount } else { amount }
}

/// `points` with a step of `value` written over the grid cells from
/// `first` to `last` (their starts, in either order), each `step` beats
/// long. What the stroke passes over is replaced; where it ends the
/// envelope carries on as it was.
pub(in crate::ui) fn drawn(
    points: &[AutomationPoint],
    first: Beats,
    last: Beats,
    step: Beats,
    value: f64,
) -> Vec<AutomationPoint> {
    let (from, to) = (first.min(last), first.max(last) + step);
    let within = |point: &&AutomationPoint| point.beat >= from - 1e-9 && point.beat < to - 1e-9;
    let mut result: Vec<AutomationPoint> = points.iter().filter(|point| !within(point)).copied().collect();
    result.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    let at = result.partition_point(|point| point.beat < from);
    let mut written = Vec::new();
    // A line arriving at the step keeps the slope it had, then jumps.
    if at > 0 && result[at - 1].shape != Segment::Hold {
        written.push(AutomationPoint::new(from, arriving(points, from)));
    }
    written.push(AutomationPoint { beat: from, value, shape: Segment::Hold });
    let resumes = result.get(at).is_some_and(|next| (next.beat - to).abs() < 1e-9);
    if !resumes && !points.is_empty() {
        written.push(AutomationPoint::new(to, automation_value(points, to)));
    }
    result.splice(at..at, written);
    result
}

/// The value the envelope reaches `beat` with, coming from before it. Where
/// the envelope jumps at `beat` this is the value before the jump, which
/// [`automation_value`] has already left behind.
fn arriving(points: &[AutomationPoint], beat: Beats) -> f64 {
    let next = points.partition_point(|point| point.beat < beat);
    match (next.checked_sub(1).map(|before| points[before]), points.get(next)) {
        // The line from the point before to the first one at or after
        // `beat`, evaluated on its own so a later point there is not seen.
        (Some(before), Some(after)) => automation_value(&[before, *after], beat),
        (Some(only), None) | (None, Some(&only)) => only.value,
        (None, None) => 0.0,
    }
}

/// The points of `points` inside the box spanned by two corners, each a
/// beat and a height in the lane.
pub(in crate::ui) fn points_in_box(
    points: &[AutomationPoint],
    scale: &Scale,
    a: (Beats, f32),
    b: (Beats, f32),
) -> Vec<AutomationPoint> {
    let (first, last) = (a.0.min(b.0), a.0.max(b.0));
    let (low, high) = (a.1.min(b.1), a.1.max(b.1));
    let inside = |point: &&AutomationPoint| {
        let height = scale.position(point.value);
        point.beat >= first && point.beat <= last && height >= low && height <= high
    };
    points.iter().filter(inside).copied().collect()
}

// ---- the lane ----------------------------------------------------------------

/// Paints the lane at `lane` of the view's list into `rect` and lets the
/// pointer edit it. `response` is the lanes' shared one; `clip` is what the
/// lane may paint in.
pub(super) fn show(
    ui: &Ui,
    cx: &mut Cx,
    geometry: &Geometry,
    response: &Response,
    lane: usize,
    rect: Rect,
    clip: Rect,
) {
    let theme = cx.theme;
    let snapshot = cx.snap;
    let Some(view) = cx.state.arrangement.lanes.get(lane).cloned() else { return };
    let painter = ui.painter_at(rect.intersect(clip));
    // A lane steps back from the clips above it.
    painter.rect_filled(rect, 0.0, theme.shade);
    paint::hline_above(&painter, rect.x_range(), rect.bottom(), theme.hairline);

    let Some(scale) = Scale::of(snapshot, &view.target) else {
        // The parameter cannot be described: its device did not load.
        let room = rect.shrink2(vec2(10.0, 0.0));
        text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, "Unavailable", theme.ink_faint);
        return;
    };
    let map = LaneMap::new(geometry, rect, scale);
    let points = snapshot.envelope(&view.target).unwrap_or_default();
    let color = lane_color(cx, &view);
    let off = snapshot.lane(&view.target).is_some_and(|lane| !lane.enabled);
    paint_envelope(&painter, cx, &map, &view.target, points, color, off);
    interact(ui, cx, geometry, response, &map, &view.target, points);
    paint_band(ui, &painter, cx, &map, &view.target);
}

/// The hue of the track a lane belongs to; ink for the master's.
fn lane_color(cx: &Cx, view: &LaneView) -> Color32 {
    match view.owner {
        LaneOwner::Track(track) => {
            cx.snap.track(track).map_or(cx.theme.ink, |track| colors::track_color(cx.theme, track.color))
        }
        LaneOwner::Master => cx.theme.ink,
    }
}

/// A broken line across the lane at `value`: where a control stands that
/// nothing automates. The dashes keep their place on the screen.
fn paint_resting(painter: &egui::Painter, map: &LaneMap, value: f64, color: Color32) {
    let rect = map.rect;
    let y = map.y(value).round();
    let mut x = (rect.left() / (2.0 * DASH)).ceil() * 2.0 * DASH;
    while x < rect.right() {
        paint::hline(painter, x..=(x + DASH).min(rect.right()), y, color);
        x += 2.0 * DASH;
    }
}

/// Paints the envelope of `target` in `color`. `off` says its lane is
/// switched off: the envelope steps back, and the value in force is drawn
/// over it.
fn paint_envelope(
    painter: &egui::Painter,
    cx: &Cx,
    map: &LaneMap,
    target: &AutomationTarget,
    points: &[AutomationPoint],
    color: Color32,
    off: bool,
) {
    let theme = cx.theme;
    let rect = map.rect;
    // Where the control rests: 0 dB, center, a parameter's default.
    paint::hline(painter, rect.x_range(), map.y_at(map.scale.default_position()).round(), theme.grid);

    let resting = automation::resting_value(cx.snap, target);
    if points.is_empty() {
        // Not automated yet: a broken line at the value the control has.
        if let Some(value) = resting {
            paint_resting(painter, map, value, theme.ghost);
        }
        return;
    }
    // Switched off, the envelope is what could be and not what is: it
    // gives up its hue, which goes to the value the user holds.
    let (held, color) = if off { (Some(color), fade(theme.ink_faint, 0.55)) } else { (None, color) };

    let selected = cx.state.arrangement.points.as_ref().filter(|selection| selection.target == *target);
    let chosen = selected.map(|selection| selection.points.as_slice()).unwrap_or_default();
    // A tint between the envelope and the lane's floor, as the displays
    // of the devices have under their curves. An envelope that is off
    // carries none: nothing is filled by it.
    paint_curve(painter, map, points, color, (!off).then(|| fade(color, 0.10)));
    paint_points(painter, map, points, chosen, color, cx.theme.ink);
    if let (Some(hue), Some(value)) = (held, resting) {
        paint_resting(painter, map, value, hue);
    }
}

/// The line an envelope draws across the lane of `map`, through every
/// value it takes there: held before its first point and after its last,
/// stepped where a point holds, and bent where a segment is.
pub(in crate::ui) fn curve_path(map: &LaneMap, points: &[AutomationPoint]) -> Vec<Pos2> {
    let rect = map.rect;
    let (first_beat, last_beat) = (map.beat(rect.left()), map.time.x_to_beat(map.left, rect.right()));
    // The points on screen, and one on either side for the lines that
    // enter and leave the view.
    let from = points.partition_point(|point| point.beat < first_beat).saturating_sub(1);
    let to = (points.partition_point(|point| point.beat <= last_beat) + 1).min(points.len());
    let visible = &points[from..to];

    let mut path = vec![pos2(rect.left(), map.y(automation_value(points, first_beat)))];
    if visible[0].beat > first_beat {
        path.push(map.point(&visible[0]));
    }
    for pair in visible.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let end = map.point(&b);
        match a.shape {
            Segment::Hold => path.push(pos2(end.x, map.y(a.value))),
            Segment::Linear if map.scale.linear() => {}
            // A bent line, or a straight one on a scale that is not: drawn
            // through the values it really takes.
            _ => {
                let mut x = map.x(a.beat).max(rect.left()) + CURVE_STEP;
                while x < end.x.min(rect.right()) {
                    path.push(pos2(x, map.y(automation_value(pair, map.time.x_to_beat(map.left, x)))));
                    x += CURVE_STEP;
                }
            }
        }
        path.push(end);
    }
    let last = visible[visible.len() - 1];
    if last.beat < last_beat {
        path.push(pos2(rect.right(), map.y(last.value)));
    }
    path
}

/// Paints the line of an envelope (`points` must not be empty) in `color`,
/// over a `tint` between it and the lane's floor if one is given.
pub(in crate::ui) fn paint_curve(
    painter: &egui::Painter,
    map: &LaneMap,
    points: &[AutomationPoint],
    color: Color32,
    tint: Option<Color32>,
) {
    let path = curve_path(map, points);
    if let Some(tint) = tint {
        let mut fill = Mesh::default();
        for (index, point) in path.iter().enumerate() {
            fill.colored_vertex(*point, tint);
            fill.colored_vertex(pos2(point.x, map.rect.bottom()), tint);
            if index > 0 {
                let first = 2 * index as u32;
                fill.add_triangle(first - 2, first - 1, first);
                fill.add_triangle(first - 1, first + 1, first);
            }
        }
        painter.add(Shape::mesh(fill));
    }
    painter.add(Shape::line(path, Stroke::new(LINE_WIDTH, color)));
}

/// Paints the squares of the points on screen: in `color`, and those of
/// `chosen` (the selected ones) larger and in `ink`.
pub(in crate::ui) fn paint_points(
    painter: &egui::Painter,
    map: &LaneMap,
    points: &[AutomationPoint],
    chosen: &[AutomationPoint],
    color: Color32,
    ink: Color32,
) {
    let rect = map.rect;
    let (first_beat, last_beat) = (map.beat(rect.left()), map.time.x_to_beat(map.left, rect.right()));
    let from = points.partition_point(|point| point.beat < first_beat);
    let mut squares = Mesh::default();
    let mut outlined = Vec::new();
    for point in points[from..].iter().take_while(|point| point.beat <= last_beat) {
        let center = map.point(point);
        let corner = pos2((center.x - POINT / 2.0).round(), (center.y - POINT / 2.0).round());
        let square = Rect::from_min_size(corner, vec2(POINT, POINT));
        if chosen.contains(point) {
            outlined.push(square);
        } else {
            squares.add_colored_rect(square, color);
        }
    }
    painter.add(Shape::mesh(squares));
    for square in outlined {
        painter.rect_filled(square.expand(1.0), 0.0, ink);
    }
}

/// The rubber band of a selection being dragged out in this lane.
fn paint_band(ui: &Ui, painter: &egui::Painter, cx: &Cx, map: &LaneMap, target: &AutomationTarget) {
    if let Some(ArrangeDrag::Lane { target: dragged, drag: LaneDrag::Band { origin, .. } }) = &cx.state.arrangement.drag
        && dragged == target
        && let Some(pointer) = ui.input(|input| input.pointer.latest_pos())
    {
        let band = Rect::from_two_pos(pos2(map.x(origin.0), map.y_at(origin.1)), pointer).intersect(map.rect);
        painter.rect_filled(band, 0.0, cx.theme.selected);
        paint::frame(painter, band, cx.theme.ink_dim);
    }
}

// ---- interaction -----------------------------------------------------------

/// Replaces `remove` with `add` in the envelope of `target`, as one edit
/// against the envelope as it is right now.
fn swap(cx: &mut Cx, target: &AutomationTarget, remove: &[AutomationPoint], add: &[AutomationPoint]) -> bool {
    let target = target.clone();
    cx.edit(|project| {
        let lane = project.automation.iter().find(|lane| lane.target == target);
        let now = lane.map(|lane| lane.points.clone()).unwrap_or_default();
        project.set_automation(target, replaced(&now, remove, add))
    })
    .is_some()
}

fn select(cx: &mut Cx, target: &AutomationTarget, points: Vec<AutomationPoint>) {
    cx.state.arrangement.points = Some(PointSelection { target: target.clone(), points });
}

/// The selected points of `target`.
fn selection(cx: &Cx, target: &AutomationTarget) -> Vec<AutomationPoint> {
    let selected = cx.state.arrangement.points.as_ref().filter(|selection| selection.target == *target);
    selected.map(|selection| selection.points.clone()).unwrap_or_default()
}

fn interact(
    ui: &Ui,
    cx: &mut Cx,
    geometry: &Geometry,
    response: &Response,
    map: &LaneMap,
    target: &AutomationTarget,
    points: &[AutomationPoint],
) {
    let modifiers = ui.input(|input| input.modifiers);
    let pencil = cx.state.arrangement.tool == Tool::Draw;
    let inside = |position: &Pos2| map.rect.contains(*position);
    let signature = geometry.signature;

    if cx.state.arrangement.drag.is_none()
        && let Some(pointer) = response.hover_pos().filter(inside)
    {
        let on_point = point_at(points, map, pointer);
        let on_line = segment_at(points, map, pointer).filter(|_| on_point.is_none());
        let at = timeline::format_position(map.beat(pointer.x), signature);
        let hint = match (pencil, on_point, on_line) {
            (true, ..) => {
                format!("{} at {at} \u{00B7} drag to draw steps on the grid", map.scale.format(map.value(pointer.y)))
            }
            (false, Some(point), _) => {
                ui.set_cursor_icon(CursorIcon::Grab);
                let at = timeline::format_position(point.beat, signature);
                format!(
                    "{} at {at} \u{00B7} drag to move, Shift for fine, Alt for no snapping \u{00B7} double-click removes",
                    map.scale.format(point.value)
                )
            }
            (false, None, Some(_)) => {
                ui.set_cursor_icon(CursorIcon::ResizeVertical);
                let value = automation_value(points, map.beat(pointer.x));
                format!(
                    "{} at {at} \u{00B7} drag moves the line, Alt-drag bends it \u{00B7} click adds a point",
                    map.scale.format(value)
                )
            }
            (false, None, None) => {
                format!(
                    "{} at {at} \u{00B7} click adds a point \u{00B7} drag selects",
                    map.scale.format(map.value(pointer.y))
                )
            }
        };
        cx.state.hint.set(hint);
    }

    if let Some(pointer) = response.interact_pointer_pos().filter(inside) {
        let double = response.double_clicked() || response.triple_clicked();
        match point_at(points, map, pointer) {
            Some(point) if double => {
                cx.state.focus = FocusArea::Automation;
                swap(cx, target, &[point], &[]);
                select(cx, target, Vec::new());
            }
            Some(point) if response.clicked() => {
                cx.state.focus = FocusArea::Automation;
                let mut chosen = if modifiers.shift { selection(cx, target) } else { Vec::new() };
                match chosen.iter().position(|selected| *selected == point) {
                    Some(index) => drop(chosen.remove(index)),
                    None => chosen.push(point),
                }
                select(cx, target, chosen);
            }
            None if response.clicked() && !pencil => {
                cx.state.focus = FocusArea::Automation;
                // On the line the new point sits on it; elsewhere it goes
                // where the pointer is.
                let beat = geometry.snap(map.beat(pointer.x), modifiers.alt);
                let value = match segment_at(points, map, pointer) {
                    Some(_) => automation_value(points, beat),
                    None => map.value(pointer.y),
                };
                let shape = if map.scale.stepped() { Segment::Hold } else { Segment::Linear };
                let point = AutomationPoint { beat, value, shape };
                if swap(cx, target, &[], &[point]) {
                    select(cx, target, vec![point]);
                }
            }
            // The pencil's click is its shortest stroke: one grid cell.
            _ if response.clicked() && pencil && !double => {
                cx.state.focus = FocusArea::Automation;
                let grid = geometry.step();
                let cell = timeline::snap_floor(map.beat(pointer.x), Some(grid));
                write_steps(cx, target, cell, cell, grid, map.value(pointer.y));
            }
            _ => {}
        }
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|input| input.pointer.press_origin()).filter(inside)
    {
        cx.state.focus = FocusArea::Automation;
        let drag = if pencil {
            cx.begin_gesture();
            let cell = timeline::snap_floor(map.beat(press.x), Some(geometry.step()));
            // The cell under the press is written by the first step below.
            LaneDrag::Draw { last: cell }
        } else if let Some(grabbed) = point_at(points, map, press) {
            let mut chosen = selection(cx, target);
            if !chosen.contains(&grabbed) {
                chosen = if modifiers.shift { [chosen, vec![grabbed]].concat() } else { vec![grabbed] };
                select(cx, target, chosen.clone());
            }
            cx.begin_gesture();
            LaneDrag::Points { originals: chosen.clone(), current: chosen, grabbed, press }
        } else if let Some(ends) = segment_at(points, map, press) {
            cx.begin_gesture();
            LaneDrag::Segment { originals: ends, current: ends, press, bend: modifiers.alt }
        } else {
            let kept = if modifiers.shift { selection(cx, target) } else { Vec::new() };
            LaneDrag::Band { origin: (map.beat(press.x), map.position(press.y)), kept }
        };
        cx.state.arrangement.drag = Some(ArrangeDrag::Lane { target: target.clone(), drag });
    }

    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(ArrangeDrag::Lane { target: dragged, drag }) = cx.state.arrangement.drag.clone()
        && dragged == *target
    {
        let next = step(ui, cx, geometry, map, target, points, drag, pointer);
        cx.state.arrangement.drag = Some(ArrangeDrag::Lane { target: dragged, drag: next });
    }
}

/// Carries a drag one frame further and returns how it stands.
#[expect(clippy::too_many_arguments, reason = "everything one step of a drag reads, gathered by the caller")]
fn step(
    ui: &Ui,
    cx: &mut Cx,
    geometry: &Geometry,
    map: &LaneMap,
    target: &AutomationTarget,
    points: &[AutomationPoint],
    drag: LaneDrag,
    pointer: Pos2,
) -> LaneDrag {
    let modifiers = ui.input(|input| input.modifiers);
    let signature = geometry.signature;
    match drag {
        LaneDrag::Points { originals, current, grabbed, press } => {
            ui.set_cursor_icon(CursorIcon::Grabbing);
            // The grabbed point lands on the grid and the others keep
            // their distance from it.
            let wanted = grabbed.beat + map.beat(pointer.x) - map.beat(press.x);
            let beats = geometry.snap(wanted, modifiers.alt) - grabbed.beat;
            let fine = if modifiers.shift { 0.1 } else { 1.0 };
            let height = (press.y - pointer.y) / map.travel() * fine;
            let others = replaced(points, &current, &[]);
            let next = moved(&originals, &others, beats, height, &map.scale);
            if let Some(shown) = next.iter().zip(&originals).find(|(_, original)| **original == grabbed) {
                let at = timeline::format_position(shown.0.beat, signature);
                cx.state.hint.set(format!("{} at {at}", map.scale.format(shown.0.value)));
            }
            if next != current && swap(cx, target, &current, &next) {
                select(cx, target, next.clone());
                return LaneDrag::Points { originals, current: next, grabbed, press };
            }
            LaneDrag::Points { originals, current, grabbed, press }
        }
        LaneDrag::Segment { originals, current, press, bend: bending } => {
            ui.set_cursor_icon(CursorIcon::ResizeVertical);
            let up = press.y - pointer.y;
            let next = if bending {
                let rising = originals[1].value > originals[0].value;
                let shape = Segment::Curve(bend(up, map.travel(), rising));
                [AutomationPoint { shape, ..originals[0] }, originals[1]]
            } else {
                let fine = if modifiers.shift { 0.1 } else { 1.0 };
                let lifted = moved(&originals, &[], 0.0, up / map.travel() * fine, &map.scale);
                [lifted[0], lifted[1]]
            };
            cx.state.hint.set(match next[0].shape {
                Segment::Curve(amount) if bending => format!("Bend {:.0} %", amount * 100.0),
                _ => format!("{} \u{2013} {}", map.scale.format(next[0].value), map.scale.format(next[1].value)),
            });
            if next != current && swap(cx, target, &current, &next) {
                return LaneDrag::Segment { originals, current: next, press, bend: bending };
            }
            LaneDrag::Segment { originals, current, press, bend: bending }
        }
        LaneDrag::Band { origin, kept } => {
            let corner = (map.beat(pointer.x), map.position(pointer.y));
            let mut chosen = kept.clone();
            for point in points_in_box(points, &map.scale, origin, corner) {
                if !chosen.contains(&point) {
                    chosen.push(point);
                }
            }
            select(cx, target, chosen);
            LaneDrag::Band { origin, kept }
        }
        LaneDrag::Draw { last } => {
            let grid = geometry.step();
            let cell = timeline::snap_floor(map.beat(pointer.x), Some(grid));
            let value = map.value(pointer.y);
            cx.state.hint.set(format!("{} at {}", map.scale.format(value), timeline::format_position(cell, signature)));
            write_steps(cx, target, last, cell, grid, value);
            LaneDrag::Draw { last: cell }
        }
    }
}

/// Writes a step of `value` over the grid cells from `first` to `last`
/// into the envelope of `target`, as it is right now.
fn write_steps(cx: &mut Cx, target: &AutomationTarget, first: Beats, last: Beats, grid: Beats, value: f64) {
    let target = target.clone();
    cx.edit(|project| {
        let lane = project.automation.iter().find(|lane| lane.target == target);
        let now = lane.map(|lane| lane.points.clone()).unwrap_or_default();
        project.set_automation(target, drawn(&now, first, last, grid, value))
    });
}

/// Removes the selected points: Delete, with an automation lane in focus.
pub fn delete_selected(cx: &mut Cx) {
    let Some(selection) = cx.state.arrangement.points.take() else { return };
    if !selection.points.is_empty() {
        swap(cx, &selection.target, &selection.points, &[]);
    }
    select(cx, &selection.target, Vec::new());
}

/// Selects every point of the lane last worked in: Ctrl+A.
pub fn select_all(cx: &mut Cx) {
    if let Some(target) = cx.state.arrangement.points.as_ref().map(|selection| selection.target.clone()) {
        let all = cx.snap.envelope(&target).unwrap_or_default().to_vec();
        select(cx, &target, all);
    }
}

// ---- the context menu ------------------------------------------------------

/// What a right-click in a lane was on.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct MenuSubject {
    target: AutomationTarget,
    /// The point under the pointer.
    point: Option<AutomationPoint>,
    /// The line under the pointer, by the point it starts at.
    line: Option<AutomationPoint>,
    /// Where a new point would go.
    place: AutomationPoint,
}

/// Works out what the pointer is on, for the menu about to open.
pub(super) fn menu_subject(cx: &mut Cx, geometry: &Geometry, pointer: Pos2) -> Option<MenuSubject> {
    let row = geometry.row_at(pointer.y)?;
    let super::rows::RowKind::Lane { lane, .. } = row.kind else { return None };
    let target = cx.state.arrangement.lanes.get(lane)?.target.clone();
    let scale = Scale::of(cx.snap, &target)?;
    let map = LaneMap::new(geometry, geometry.row_rect(row), scale);
    let points = cx.snap.envelope(&target).unwrap_or_default();
    let point = point_at(points, &map, pointer);
    let line = segment_at(points, &map, pointer).map(|ends| ends[0]);
    let beat = geometry.snap(map.beat(pointer.x), false);
    let place = AutomationPoint::new(beat, map.value(pointer.y));
    cx.state.focus = FocusArea::Automation;
    if let Some(point) = point
        && !selection(cx, &target).contains(&point)
    {
        select(cx, &target, vec![point]);
    }
    Some(MenuSubject { target, point, line, place })
}

pub(super) fn menu(ui: &mut Ui, cx: &mut Cx, subject: &MenuSubject) {
    let theme = cx.theme;
    let target = &subject.target;
    // The line a shape applies to: the one under the pointer, or the one
    // leaving the point under it.
    let shaped = subject.line.or(subject.point);
    if let Some(start) = shaped {
        let options = [("Straight line", Segment::Linear), ("Step: hold, then jump", Segment::Hold)];
        for (label, shape) in options {
            if MenuItem::new(label).checked(start.shape == shape).show(ui, theme).clicked() {
                let reshaped = AutomationPoint { shape, ..start };
                if swap(cx, target, &[start], &[reshaped]) {
                    select(cx, target, vec![reshaped]);
                }
            }
        }
        trill_noir::menu_separator(ui, theme);
    }
    if subject.point.is_some() {
        let count = selection(cx, target).len();
        let label = if count > 1 { format!("Delete {count} points") } else { "Delete point".to_owned() };
        if MenuItem::new(&label).shortcut("Del").show(ui, theme).clicked() {
            delete_selected(cx);
        }
    } else if MenuItem::new("Add point here").show(ui, theme).clicked() && swap(cx, target, &[], &[subject.place]) {
        select(cx, target, vec![subject.place]);
    }
    trill_noir::menu_separator(ui, theme);
    let on = cx.snap.lane(target).map(|lane| lane.enabled);
    let label = if on == Some(false) { "Switch the envelope back on" } else { "Switch the envelope off" };
    if MenuItem::new(label).enabled(on.is_some()).show(ui, theme).clicked() {
        automation::set_enabled(cx, target, on == Some(false));
    }
    if MenuItem::new("Clear the envelope").enabled(on.is_some()).show(ui, theme).clicked() {
        let target = target.clone();
        cx.edit(|project| project.set_automation(target, Vec::new()));
    }
    if MenuItem::new("Hide this lane").show(ui, theme).clicked() {
        cx.state.arrangement.close_lane(target);
    }
}

// ---- the lane's header -----------------------------------------------------

/// The header of a lane: the switch of its envelope, what it shows (a menu
/// of everything the track can automate), its value at the playhead, and
/// buttons to add a lane and to close this one.
pub(super) fn header(ui: &mut Ui, cx: &mut Cx, lane: usize, rect: Rect, strip: f32) {
    let theme = cx.theme;
    let snapshot = cx.snap;
    let Some(view) = cx.state.arrangement.lanes.get(lane).cloned() else { return };
    let id = ui.scope_id().with(("lane", lane));
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, theme.panel);
    painter.rect_filled(rect, 0.0, theme.shade);
    paint::hline_above(&painter, rect.x_range(), rect.bottom(), theme.hairline);
    // The track's color strip runs on down its lanes, stepped back.
    let color = lane_color(cx, &view);
    let edge = Rect::from_min_max(rect.min, pos2(rect.left() + strip, rect.bottom() - 1.0));
    painter.rect_filled(edge, 0.0, fade(color, 0.45));

    let row = Rect::from_min_size(
        pos2(rect.left() + HEADER_INDENT, rect.top() + 5.0),
        vec2(rect.width() - HEADER_INDENT - super::headers::HEADER_RIGHT, theme.metric.chip),
    );
    let button = |index: f32| {
        Rect::from_min_size(pos2(row.right() - (index + 1.0) * 19.0 + 1.0, row.top()), vec2(18.0, theme.metric.chip))
    };

    let close = trill_noir::bare_icon_button(ui, theme, button(0.0), id.with("close"), Icon::Close, "Hide lane");
    cx.hint(&close, "Hide this lane \u{00B7} its envelope stays");
    if close.clicked() {
        cx.state.arrangement.close_lane(&view.target);
        return;
    }
    let all = automation::choices(snapshot, view.owner);
    let add = trill_noir::bare_icon_button(ui, theme, button(1.0), id.with("add"), Icon::Plus, "Add lane");
    cx.hint(&add, "Show another lane for this track");
    if add.clicked() {
        // The next thing not shown yet: envelopes first, then the rest.
        let hidden = |choice: &&Choice| !cx.state.arrangement.lane_open(&choice.target);
        let automated = all.iter().filter(hidden).find(|choice| snapshot.envelope(&choice.target).is_some());
        if let Some(choice) = automated.or_else(|| all.iter().find(hidden)) {
            automation::show_lane(cx, choice.target.clone());
        }
    }

    let (name, device) = automation::names(snapshot, &view.target);
    let on = snapshot.lane(&view.target).map(|lane| lane.enabled);
    if let Some(on) = on {
        switch(ui, cx, &view.target, on, pos2(rect.left() + SWITCH_LEFT, row.center().y), id.with("switch"), &name);
    }
    let chooser = row.with_max_x(button(1.0).left() - 3.0);
    let dropdown = trill_noir::dropdown(ui, theme, chooser, id.with("target"), "", &name, false);
    cx.hint(&dropdown, "What this lane automates \u{00B7} click to choose");
    choose(cx, &dropdown, lane, &view, &all);

    // Under it, what the envelope says at the playhead.
    let readout =
        Rect::from_min_max(pos2(row.left() + 1.0, row.bottom() + 3.0), pos2(row.right(), rect.bottom() - 3.0));
    if readout.height() >= theme.text.value.size + 2.0 {
        let scale = Scale::of(snapshot, &view.target);
        let driven = snapshot.automated(&view.target, cx.live.position);
        let value = driven.or_else(|| automation::resting_value(snapshot, &view.target));
        let line = readout.with_max_y(readout.top() + 16.0);
        let quiet = theme.quiet_on(theme.panel);
        match (scale, value) {
            (Some(scale), Some(value)) => {
                let ink = if driven.is_some() { theme.ink } else { quiet };
                let used =
                    text::paint(&painter, line, Align2::LEFT_CENTER, theme.text.value, &scale.format(value), ink);
                // Beside the value, whose parameter it is: the device, or
                // that nothing is written yet, or that what is written is
                // switched off and the value is the one set by hand.
                let (note, note_ink) = match on {
                    None => (Some("No envelope"), quiet),
                    Some(false) => (Some("Envelope off"), theme.ink),
                    Some(true) => (device.as_deref(), quiet),
                };
                if let Some(note) = note {
                    let rest = line.with_min_x(line.left() + used + 8.0);
                    text::paint(&painter, rest, Align2::RIGHT_CENTER, theme.text.micro, note, note_ink);
                }
            }
            _ => drop(text::paint(&painter, line, Align2::LEFT_CENTER, theme.text.micro, "Unavailable", quiet)),
        }
    }
}

/// The switch of a lane's envelope, at `left_center`: a dot like the one
/// that bypasses a device. Filled, the envelope drives its control; an
/// outline, it is off and the control stays where it is set by hand. A
/// click switches, as one undo step.
fn switch(ui: &Ui, cx: &mut Cx, target: &AutomationTarget, on: bool, left_center: Pos2, id: Id, name: &str) {
    let theme = cx.theme;
    let side = theme.metric.dot;
    let zone = Rect::from_center_size(left_center + vec2(side / 2.0, 0.0), vec2(side + 10.0, theme.metric.chip));
    let response = ui.interact(zone, id, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, format!("Envelope of {name} on")));
    let painter = ui.painter();
    if on {
        paint::dot(painter, left_center, side, if response.hovered() { theme.ink } else { theme.ok });
    } else {
        // In the signal color: this is the one thing in the lane that is
        // not as it was written, and the way back.
        paint::hollow_dot(painter, left_center, side, if response.hovered() { theme.ink } else { theme.signal });
    }
    cx.hint(
        &response,
        if on {
            "On: the envelope sets this value \u{00B7} click to switch it off and set the value by hand"
        } else {
            "Off: the value stays where it is set by hand \u{00B7} click to let the envelope set it again"
        },
    );
    if response.clicked() {
        automation::set_enabled(cx, target, !on);
    }
}

/// The menu behind a lane's chooser: the mixer's controls, then each
/// device's parameters under its name, with a search field when there are
/// many. Choosing one points the lane at it.
fn choose(cx: &mut Cx, dropdown: &Response, lane: usize, view: &LaneView, all: &[Choice]) {
    let theme = cx.theme;
    let snapshot = cx.snap;
    let mut chosen = None;
    egui::Popup::menu(dropdown).gap(1.0).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(230.0);
        let mut query = String::new();
        if all.len() > SEARCH_ABOVE {
            let (field, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
            let search = &mut cx.state.arrangement.lane_search;
            let hint = format!("Search {} parameters", all.len());
            let id = Id::unique("lane-search");
            let response = trill_noir::text_field(ui, theme, field.shrink2(vec2(6.0, 2.0)), id, search, &hint);
            // Typing starts right away, without a click on the field.
            if !response.has_focus() && !ui.memory(|memory| memory.focused().is_some()) {
                response.request_focus();
            }
            query = search.trim().to_lowercase();
        }
        egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
            let mut heading: Option<&str> = None;
            let mut shown = 0;
            for choice in all {
                let matches = |text: &str| text.to_lowercase().contains(&query);
                let found = query.is_empty() || matches(&choice.name) || choice.device.as_deref().is_some_and(matches);
                if !found {
                    continue;
                }
                shown += 1;
                if choice.device.as_deref() != heading
                    && let Some(device) = choice.device.as_deref()
                {
                    trill_noir::menu_heading(ui, theme, device);
                }
                heading = choice.device.as_deref();
                // The mark says which this lane shows; the detail, which
                // already have an envelope.
                let automated = snapshot.envelope(&choice.target).is_some();
                let item = MenuItem::new(&choice.name).checked(choice.target == view.target);
                let item = item.shortcut(if automated { "Automated" } else { "" });
                if item.show(ui, theme).clicked() {
                    chosen = Some(choice.target.clone());
                }
            }
            if shown == 0 {
                trill_noir::menu_heading(ui, theme, "Nothing matches");
            }
        });
    });
    let Some(target) = chosen else { return };
    cx.state.arrangement.lane_search.clear();
    let lanes = &mut cx.state.arrangement.lanes;
    if lanes.iter().any(|open| open.target == target) {
        // Already shown in another lane: go to that one.
        cx.state.arrangement.reveal = Some(target);
    } else if let Some(open) = lanes.get_mut(lane) {
        open.target = target;
        cx.state.arrangement.points = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::timeline::TimeView;

    fn point(beat: Beats, value: f64) -> AutomationPoint {
        AutomationPoint::new(beat, value)
    }

    /// A lane 100 px of travel tall at 20 px per beat, showing pan.
    fn map() -> LaneMap {
        let rect = Rect::from_min_size(pos2(200.0, 300.0), vec2(800.0, 100.0 + 2.0 * PAD));
        LaneMap { rect, left: 200.0, time: TimeView { origin: 0.0, pixels_per_beat: 20.0 }, scale: Scale::Pan }
    }

    #[test]
    fn values_map_to_heights_and_back() {
        let map = map();
        assert_eq!(map.y(-1.0), map.rect.bottom() - PAD, "the lowest value clears the floor");
        assert_eq!(map.y(1.0), map.rect.top() + PAD);
        assert_eq!(map.y(0.0), map.rect.center().y);
        assert_eq!(map.value(map.rect.top() + PAD + 25.0), 0.5);
        assert_eq!(map.value(map.rect.top() - 50.0), 1.0, "above the lane is the top of the scale");
        assert_eq!(map.value(map.rect.bottom() + 50.0), -1.0);
        assert_eq!(map.x(4.0), 280.0);
        assert_eq!(map.beat(280.0), 4.0);
        assert_eq!(map.beat(0.0), 0.0, "never before the start of the song");
        for value in [-1.0, -0.5, 0.0, 0.25, 1.0] {
            assert!((map.value(map.y(value)) - value).abs() < 0.011, "{value}");
        }

        let volume = LaneMap { scale: Scale::Volume, ..map };
        assert!(volume.y(0.0) < volume.y(-12.0), "louder is higher");
        assert_eq!(volume.value(volume.y(-6.0)), -6.0);
    }

    #[test]
    fn points_and_lines_are_grabbed_where_they_are_drawn() {
        let map = map();
        let points = [point(0.0, -1.0), point(4.0, 1.0), point(8.0, 1.0)];
        let on = |beat: Beats, value: f64| pos2(map.x(beat), map.y(value));
        assert_eq!(point_at(&points, &map, on(4.0, 1.0) + vec2(4.0, 3.0)), Some(points[1]));
        assert_eq!(point_at(&points, &map, on(4.0, 1.0) + vec2(9.0, 0.0)), None);
        assert_eq!(point_at(&points, &map, on(2.0, 0.0)), None, "the line is not a point");
        assert_eq!(point_at(&[], &map, on(2.0, 0.0)), None);

        assert_eq!(segment_at(&points, &map, on(2.0, 0.0)), Some([points[0], points[1]]));
        assert_eq!(segment_at(&points, &map, on(2.0, 0.0) + vec2(0.0, 12.0)), None, "too far below the line");
        assert_eq!(segment_at(&points, &map, on(6.0, 1.0)), Some([points[1], points[2]]));
        assert_eq!(segment_at(&points, &map, on(10.0, 1.0)), None, "past the last point there is no line to move");

        // Where a step puts two points at one beat, each is grabbed at its
        // own height.
        let step = [point(2.0, -1.0), point(2.0, 1.0)];
        assert_eq!(point_at(&step, &map, on(2.0, 1.0)), Some(step[1]));
        assert_eq!(point_at(&step, &map, on(2.0, -1.0)), Some(step[0]));
    }

    #[test]
    fn moved_points_stay_between_their_neighbors_and_on_the_scale() {
        let scale = Scale::Pan;
        let others = [point(0.0, 0.0), point(8.0, 0.0)];
        let chosen = [point(2.0, -0.5), point(4.0, 0.5)];
        assert_eq!(moved(&chosen, &others, 1.0, 0.0, &scale), [point(3.0, -0.5), point(5.0, 0.5)]);
        // The later point stops at the neighbor after it; both stop.
        assert_eq!(moved(&chosen, &others, 10.0, 0.0, &scale), [point(6.0, -0.5), point(8.0, 0.5)]);
        assert_eq!(moved(&chosen, &others, -10.0, 0.0, &scale), [point(0.0, -0.5), point(2.0, 0.5)]);
        // A quarter of the lane up is half the pan range; the top is the top.
        let lifted = moved(&chosen, &others, 0.0, 0.25, &scale);
        assert_eq!((lifted[0].value, lifted[1].value), (0.0, 1.0));
        assert_eq!(moved(&chosen, &others, 0.0, 5.0, &scale)[0].value, 1.0);
        // With nothing else in the envelope only the start of the song stops them.
        assert_eq!(moved(&chosen, &[], -10.0, 0.0, &scale)[0].beat, 0.0);
        assert_eq!(moved(&chosen, &[], 100.0, 0.0, &scale)[1].beat, 104.0);
    }

    #[test]
    fn replacing_swaps_points_and_keeps_time_order() {
        let points = [point(0.0, 0.0), point(4.0, 1.0), point(8.0, 0.0)];
        let result = replaced(&points, &[points[1]], &[point(9.0, 0.5), point(2.0, 0.5)]);
        assert_eq!(result, [point(0.0, 0.0), point(2.0, 0.5), point(8.0, 0.0), point(9.0, 0.5)]);
        // A point that is no longer there (removed elsewhere) is not an error.
        assert_eq!(replaced(&points, &[point(5.0, 5.0)], &[]), points);
        assert!(replaced(&points, &points, &[]).is_empty());
    }

    #[test]
    fn dragging_up_bows_a_line_upward_whichever_way_it_runs() {
        assert_eq!(bend(25.0, 100.0, true), -0.5, "a rising line that starts fast bows upward");
        assert_eq!(bend(25.0, 100.0, false), 0.5, "a falling line that starts slowly does");
        assert_eq!(bend(-25.0, 100.0, true), 0.5);
        assert_eq!(bend(500.0, 100.0, false), 1.0, "no further than the model allows");
        assert_eq!(bend(0.0, 100.0, true), 0.0);
    }

    fn held(beat: Beats, value: f64) -> AutomationPoint {
        AutomationPoint { beat, value, shape: Segment::Hold }
    }

    #[test]
    fn the_pencil_writes_steps_and_lets_the_envelope_resume() {
        // On an empty lane a stroke is a single held value.
        assert_eq!(drawn(&[], 4.0, 4.0, 1.0, 0.5), [held(4.0, 0.5)]);

        // Over a ramp from 0 to 8: the ramp runs up to the step, the step
        // holds, and the ramp picks up where the stroke ended.
        let ramp = [point(0.0, 0.0), point(8.0, 8.0)];
        let stepped = drawn(&ramp, 2.0, 3.0, 1.0, 9.0);
        assert_eq!(stepped, [point(0.0, 0.0), point(2.0, 2.0), held(2.0, 9.0), point(4.0, 4.0), point(8.0, 8.0)]);
        let value = |beat: Beats| automation_value(&stepped, beat);
        assert_eq!((value(1.0), value(2.0), value(3.9), value(4.0), value(6.0)), (1.0, 9.0, 9.0, 4.0, 6.0));

        // The next cell of the same stroke, at another value, joins on.
        let next = drawn(&stepped, 4.0, 4.0, 1.0, 5.0);
        assert_eq!(
            next,
            [point(0.0, 0.0), point(2.0, 2.0), held(2.0, 9.0), held(4.0, 5.0), point(5.0, 5.0), point(8.0, 8.0)]
        );
        // Drawing back over a cell replaces what was there, in either direction.
        let again = drawn(&next, 4.0, 2.0, 1.0, 1.0);
        assert_eq!(again, [point(0.0, 0.0), point(2.0, 2.0), held(2.0, 1.0), point(5.0, 5.0), point(8.0, 8.0)]);
    }

    #[test]
    fn the_value_a_line_arrives_with_is_the_one_before_a_jump() {
        let jump = [point(0.0, 0.0), point(2.0, 2.0), held(2.0, 9.0), point(4.0, 4.0)];
        assert_eq!(arriving(&jump, 2.0), 2.0);
        assert_eq!(automation_value(&jump, 2.0), 9.0, "the envelope itself has already jumped");
        assert_eq!(arriving(&jump, 1.0), 1.0, "between points it is simply the value");
        assert_eq!(arriving(&jump, 9.0), 4.0);
        assert_eq!(arriving(&jump, 0.0), 0.0);
        assert_eq!(arriving(&[], 3.0), 0.0);
    }

    #[test]
    fn the_band_selects_the_points_inside_it() {
        let points = [point(1.0, -1.0), point(2.0, 0.0), point(3.0, 1.0)];
        assert_eq!(points_in_box(&points, &Scale::Pan, (0.0, 0.0), (2.5, 0.6)), [points[0], points[1]]);
        assert_eq!(points_in_box(&points, &Scale::Pan, (2.5, 0.6), (0.0, 0.4)), [points[1]], "corners in any order");
        assert!(points_in_box(&points, &Scale::Pan, (5.0, 0.0), (9.0, 1.0)).is_empty());
    }
}
