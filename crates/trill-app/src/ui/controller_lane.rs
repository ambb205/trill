//! The lane under the piano roll's notes when it shows a MIDI controller:
//! the clip's curve for expression, a wheel, a pedal or any other
//! controller, drawn in the clip's own time under the notes it shapes, and
//! the pointer editing it.
//!
//! A strip across the top of the lane says what it shows and offers the
//! rest: velocity, the controllers every keyboard has, whatever else the
//! clip holds, and a chooser for any other. A dot on a chip says the clip
//! has a curve for that controller, so what a clip holds is seen without
//! opening each one.
//!
//! The curve is edited with the gestures of the automation lanes, and
//! with their code (`arrangement::envelopes`): a click adds a point,
//! points and lines drag, Alt bends a line, a double-click removes a
//! point, a drag over nothing selects, and the pencil writes steps on the
//! grid. One drag is one undo step. Points are followed by value, as
//! there; every step of a drag swaps the previous step's points for the
//! next in the clip as it is at that moment.
//!
//! The lane is in the clip's time: beat 0 is where the clip's content
//! begins, as it is for the notes above, and what lies past the loop or
//! the clip's end is shaded as the notes there are.

use super::arrangement::envelopes::{self, LaneMap};
use super::automation::Scale;
use super::state::{Cx, FocusArea, LaneDrag, LaneShown, Tool};
use super::timeline::{self, TimeView};
use eframe::egui::{self, Align2, Color32, CursorIcon, Id, PointerButton, Pos2, Rect, Response, Sense, Ui, pos2, vec2};
use trill_core::device::ControllerSupport;
use trill_core::project::automation_value;
use trill_core::{AutomationPoint, Beats, Clip, ClipId, ControllerKind, Segment, TimeSignature, Track};
use trill_noir::{Chip, Icon, MenuItem, fade, paint, text};

/// Height of the strip of choices above the lane.
pub(super) const HEADER: f32 = 20.0;
/// The controllers every lane offers, clip or no clip: the ones a keyboard
/// has under the hands and the feet.
const STANDARD: [ControllerKind; 4] =
    [ControllerKind::EXPRESSION, ControllerKind::MOD_WHEEL, ControllerKind::PitchBend, ControllerKind::SUSTAIN];

/// Where the lane is and how its time runs, as the piano roll lays it out.
pub(super) struct Frame {
    /// The strip of choices, across the keys and the grid.
    pub header: Rect,
    /// The lane itself, across the keys and the grid.
    pub body: Rect,
    /// Where the grid starts: the screen x of the view's first beat.
    pub grid_left: f32,
    pub time: TimeView,
    /// The grid points snap to; `None` when snapping is off.
    pub snap: Option<Beats>,
    /// The grid the pencil writes on.
    pub cell: Beats,
    pub signature: TimeSignature,
    /// The track's hue.
    pub color: Color32,
    /// The part of the clip's content that plays, from and to: its loop,
    /// or what a trimmed clip keeps.
    pub sounding: (Beats, Beats),
    /// Where the song's playhead is in the lane, while it is in the clip.
    pub playhead: Option<f32>,
}

impl Frame {
    /// The part of the lane the curve is drawn in: under the grid.
    fn curve_rect(&self) -> Rect {
        self.body.with_min_x(self.grid_left)
    }

    fn map(&self, kind: ControllerKind) -> LaneMap {
        LaneMap {
            rect: self.curve_rect(),
            left: self.grid_left,
            time: self.time,
            scale: Scale::Controller(kind),
            // A controller's curve is straight in its own steps.
            lines: trill_core::Travel::Linear,
        }
    }

    fn snapped(&self, beat: Beats, free: bool) -> Beats {
        timeline::snap_round(beat, if free { None } else { self.snap }).max(0.0)
    }
}

/// What the accessibility tree, and so a test, calls the chip of a lane.
pub(super) fn chip_name(shown: LaneShown) -> String {
    match shown {
        LaneShown::Velocity => "Velocity lane".to_owned(),
        LaneShown::Controller(kind) => format!("{} lane", kind.label()),
    }
}

/// The controllers the lane's strip offers for `clip`, in order: the
/// standard ones, then whatever else the clip holds, then the one being
/// shown if it is none of those.
fn offered(clip: &Clip, shown: LaneShown) -> Vec<ControllerKind> {
    let mut kinds = STANDARD.to_vec();
    let held = clip.controllers().iter().map(|curve| curve.controller);
    let showing = match shown {
        LaneShown::Controller(kind) => Some(kind),
        LaneShown::Velocity => None,
    };
    for kind in held.chain(showing) {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

/// The strip across the top of the lane: what it shows, as chips, and a
/// chooser for every other controller. Painted for every lane, velocity
/// included.
pub(super) fn header(ui: &mut Ui, cx: &mut Cx, frame: &Frame, track: &Track, clip: &Clip) {
    let theme = cx.theme;
    let strip = frame.header;
    let painter = ui.painter_at(strip);
    painter.rect_filled(strip, 0.0, theme.panel);
    paint::hline(&painter, strip.x_range(), strip.top(), theme.hairline);
    let id = ui.scope_id().with("lane-header");
    let shown = cx.state.piano.lane;
    let support = cx.snap.controller_support(track.id);
    let chip_height = theme.metric.chip.min(strip.height() - 3.0);
    let top = (strip.center().y - chip_height / 2.0).round();
    let gap = theme.metric.gap - 1.0;

    // What the instrument has no use for is offered all the same, stepped
    // back: the curve can be written, and another instrument may want it.
    let unused = |kind: ControllerKind| support.responds_to(kind) == Some(false);
    let mut x = frame.grid_left + 4.0;
    let mut place = |ui: &Ui, spec: &Chip| {
        let rect = Rect::from_min_size(pos2(x, top), vec2(spec.width(ui, theme), chip_height));
        x = rect.right() + gap;
        rect
    };
    let name = chip_name(LaneShown::Velocity);
    let spec = Chip::new("Velocity", shown == LaneShown::Velocity).named(&name);
    let rect = place(ui, &spec);
    let response = trill_noir::chip(ui, theme, rect, id.with("velocity"), &spec);
    cx.hint(&response, "Show how hard each note is hit");
    if response.clicked() {
        show_lane(cx, LaneShown::Velocity);
    }

    let mut chosen = None;
    for kind in offered(clip, shown) {
        let label = kind.label();
        let name = chip_name(LaneShown::Controller(kind));
        let held = clip.controller(kind).is_some();
        let mut spec = Chip::new(&label, shown == LaneShown::Controller(kind)).named(&name).dimmed(unused(kind));
        if held {
            spec = spec.dot(frame.color);
        }
        let rect = place(ui, &spec);
        if rect.right() > strip.right() - 30.0 {
            // No room: the chooser still lists it.
            break;
        }
        let response = trill_noir::chip(ui, theme, rect, id.with(("kind", kind.index())), &spec);
        let about = match (held, unused(kind)) {
            (_, true) => format!("{label}: this track\u{2019}s instrument has no use for it \u{00B7} click to show it"),
            (true, false) => format!("{label}: the clip has a curve for it \u{00B7} click to show it"),
            (false, false) => format!("{label}: nothing written yet \u{00B7} click to show it and draw a curve"),
        };
        cx.hint(&response, about);
        if response.clicked() {
            chosen = Some(kind);
        }
    }
    let more = Rect::from_min_size(pos2(x, top), vec2(20.0, chip_height));
    let response = trill_noir::bare_icon_button(ui, theme, more, id.with("more"), Icon::Plus, "Other controllers");
    cx.hint(&response, "Show another controller: breath, pressure, volume, or any by its number");
    chosen = chosen.or(choose(cx, &response, clip, shown, &support));
    if let Some(kind) = chosen {
        show_lane(cx, LaneShown::Controller(kind));
    }
    // Under the keys: what the lane's numbers are.
    let gutter = Rect::from_min_max(strip.min + vec2(6.0, 0.0), pos2(frame.grid_left - 4.0, strip.bottom()));
    let unit = match shown {
        LaneShown::Velocity => "Vel",
        LaneShown::Controller(ControllerKind::PitchBend) => "Bend",
        LaneShown::Controller(kind) if kind.is_switch() => "Pedal",
        LaneShown::Controller(_) => "0\u{2013}127",
    };
    text::paint(&painter, gutter, Align2::LEFT_CENTER, theme.text.micro, unit, theme.ink_faint);
    paint::vline_left_of(&painter, frame.grid_left, strip.y_range(), theme.hairline);
    silent_notice(ui, cx, frame, track, x + 26.0);
}

/// Makes the lane show `shown`, with nothing selected in it yet.
fn show_lane(cx: &mut Cx, shown: LaneShown) {
    if cx.state.piano.lane != shown {
        cx.state.piano.lane = shown;
        cx.state.piano.lane_points.clear();
        cx.state.piano.lane_drag = None;
    }
}

/// The menu behind the strip's plus: every controller there is, by name
/// and number, with a search. Returns the one chosen.
fn choose(
    cx: &mut Cx,
    button: &Response,
    clip: &Clip,
    shown: LaneShown,
    support: &ControllerSupport,
) -> Option<ControllerKind> {
    let theme = cx.theme;
    let mut chosen = None;
    egui::Popup::menu(button).gap(1.0).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(230.0);
        let (field, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
        let search = &mut cx.state.piano.lane_search;
        let id = Id::unique("controller-search");
        let hint = "Search by name or number";
        let response = trill_noir::text_field(ui, theme, field.shrink2(vec2(6.0, 2.0)), id, search, hint);
        // Typing starts right away, without a click on the field.
        if !response.has_focus() && !ui.memory(|memory| memory.focused().is_some()) {
            response.request_focus();
        }
        let query = search.trim().to_lowercase();
        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            let all = [ControllerKind::PitchBend, ControllerKind::Pressure]
                .into_iter()
                .chain((0..=trill_core::controllers::MAX_CONTROLLER).filter_map(ControllerKind::numbered));
            let mut listed = 0;
            for kind in all {
                let label = kind.label();
                let number = kind.number().map(|number| number.to_string()).unwrap_or_default();
                let named = kind.name().is_some();
                let found = match query.as_str() {
                    // Unasked, the ones with a name: the rest are numbers.
                    "" => named,
                    query => label.to_lowercase().contains(query) || number == query || format!("cc{number}") == query,
                };
                if !found {
                    continue;
                }
                listed += 1;
                let held = clip.controller(kind).is_some();
                let detail = match (held, support.responds_to(kind)) {
                    (true, _) => "In this clip".to_owned(),
                    (false, Some(false)) => "Not used by the instrument".to_owned(),
                    (false, _) => number,
                };
                let item = MenuItem::new(&label).checked(shown == LaneShown::Controller(kind)).shortcut(&detail);
                if item.show(ui, theme).clicked() {
                    chosen = Some(kind);
                }
            }
            if listed == 0 {
                trill_noir::menu_heading(ui, theme, "No such controller \u{00B7} try a number from 0 to 119");
            }
        });
    });
    if chosen.is_some() {
        cx.state.piano.lane_search.clear();
    }
    chosen
}

/// What to say about an instrument that is given notes and gives no sound:
/// who it is, in a few words, and what to do about it. A click on either
/// place that says it shows the Expression lane.
pub(super) fn silent_advice(name: &str, plugin: bool) -> (String, String) {
    let short = format!("{name} is given notes and stays silent");
    let long = if plugin {
        format!(
            "{name} is given notes and makes no sound. Instruments modelled on breath or a bow wait for \
             expression: draw an Expression curve under the clip\u{2019}s notes, or switch on the computer keyboard, \
             which gives expression as you play \u{00B7} click to show the Expression lane"
        )
    } else {
        format!(
            "{name} is given notes and makes no sound: it has no sound loaded, or its expression or volume is at \
             nothing \u{00B7} click to show the Expression lane"
        )
    };
    (short, long)
}

/// The mark of a track whose instrument is given notes and stays silent: a
/// hollow dot in `rect` that says who and what to do, where the track is
/// looked at. A click selects the track and shows the Expression lane.
/// Returns whether there was anything to mark.
pub(super) fn silent_mark(ui: &Ui, cx: &mut Cx, rect: Rect, id: Id, track: &Track) -> bool {
    let Some(silent) = cx.live.silent_instrument(track.id) else { return false };
    let (short, long) = silent_advice(&silent.name, silent.plugin);
    let theme = cx.theme;
    let response = ui.interact(rect, id, Sense::click());
    let name = format!("{}: {short}", track.name);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    let color = if response.hovered() { theme.ink } else { theme.warn };
    let left = pos2(rect.center().x - theme.metric.dot / 2.0, rect.center().y);
    paint::hollow_dot(ui.painter(), left, theme.metric.dot, color);
    cx.hint(&response, long);
    if response.clicked() {
        cx.state.select_track(track.id);
        show_lane(cx, LaneShown::Controller(ControllerKind::EXPRESSION));
    }
    true
}

/// At the far end of the strip, when this track's instrument has been
/// given notes and has put nothing out: who, and on a click the lane where
/// it is put right.
fn silent_notice(ui: &Ui, cx: &mut Cx, frame: &Frame, track: &Track, from: f32) {
    let theme = cx.theme;
    let Some(silent) = cx.live.silent_instrument(track.id) else { return };
    let (short, long) = silent_advice(&silent.name, silent.plugin);
    let strip = frame.header;
    let width = text::width(ui.painter(), theme.text.micro, &short).ceil() + theme.metric.dot + 12.0;
    let left = (strip.right() - width - 8.0).max(from);
    let rect = Rect::from_min_max(pos2(left, strip.top() + 1.0), pos2(strip.right() - 4.0, strip.bottom()));
    if rect.width() < 60.0 {
        return;
    }
    let response = ui.interact(rect, ui.scope_id().with("silent-instrument"), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &short));
    let painter = ui.painter_at(rect);
    let ink = if response.hovered() { theme.ink } else { theme.warn };
    let after = paint::hollow_dot(&painter, rect.left_center() + vec2(2.0, 0.0), theme.metric.dot, theme.warn) + 6.0;
    text::paint(&painter, rect.with_min_x(after), Align2::LEFT_CENTER, theme.text.micro, &short, ink);
    cx.hint(&response, long);
    if response.clicked() {
        show_lane(cx, LaneShown::Controller(ControllerKind::EXPRESSION));
    }
}

// ---- the curve ---------------------------------------------------------------

/// Replaces `remove` with `add` in the clip's curve for `kind`, as one edit
/// against the curve as it is right now.
fn swap(cx: &mut Cx, clip: ClipId, kind: ControllerKind, remove: &[AutomationPoint], add: &[AutomationPoint]) -> bool {
    cx.edit(|project| {
        let clip = project.clip_mut(clip)?;
        let now = clip.controller(kind).map(|curve| curve.points.clone()).unwrap_or_default();
        clip.set_controller(kind, envelopes::replaced(&now, remove, add))
    })
    .is_some()
}

/// Writes a step of `value` over the grid cells from `first` to `last`.
fn write_steps(cx: &mut Cx, clip: ClipId, kind: ControllerKind, cells: (Beats, Beats), grid: Beats, value: f64) {
    cx.edit(|project| {
        let clip = project.clip_mut(clip)?;
        let now = clip.controller(kind).map(|curve| curve.points.clone()).unwrap_or_default();
        clip.set_controller(kind, envelopes::drawn(&now, trill_core::Travel::Linear, cells.0, cells.1, grid, value))
    });
}

/// The clip the Clip tab shows and the controller its lane shows, if it
/// shows one.
fn shown_curve(cx: &Cx) -> Option<(ClipId, ControllerKind)> {
    match (cx.state.selected_clip, cx.state.piano.lane) {
        (Some(clip), LaneShown::Controller(kind)) => Some((clip, kind)),
        _ => None,
    }
}

/// Removes the selected points: Delete, with the lane in focus.
pub fn delete_selected(cx: &mut Cx) {
    let Some((clip, kind)) = shown_curve(cx) else { return };
    let doomed = std::mem::take(&mut cx.state.piano.lane_points);
    if !doomed.is_empty() {
        swap(cx, clip, kind, &doomed, &[]);
    }
}

/// Selects every point of the curve the lane shows: Ctrl+A.
pub fn select_all(cx: &mut Cx) {
    let Some((clip, kind)) = shown_curve(cx) else { return };
    let curve = cx.snap.clip(clip).and_then(|(_, clip)| clip.controller(kind));
    cx.state.piano.lane_points = curve.map(|curve| curve.points.clone()).unwrap_or_default();
}

/// Paints the lane for `kind` and lets the pointer edit its curve.
pub(super) fn show(ui: &mut Ui, cx: &mut Cx, frame: &Frame, clip: &Clip, kind: ControllerKind) {
    let theme = cx.theme;
    let lane = frame.body;
    let rect = frame.curve_rect();
    let id = ui.scope_id().with("controller-lane");
    let response = ui.interact(rect, id, Sense::click_and_drag());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, format!("{} curve", kind.label())));

    let painter = ui.painter_at(lane);
    painter.rect_filled(lane, 0.0, theme.panel);
    paint::vline_left_of(&painter, frame.grid_left, lane.y_range(), theme.hairline);
    let map = frame.map(kind);
    let painter = painter.with_clip_rect(rect);
    // Where the controller rests, or its floor: the line a curve is read
    // against.
    paint::hline(&painter, rect.x_range(), map.y_at(map.scale.default_position()).round(), theme.grid);

    let curve = clip.controller(kind);
    let points: &[AutomationPoint] = curve.map(|curve| curve.points.as_slice()).unwrap_or_default();
    match curve {
        Some(curve) => {
            // As it is played: a pedal or a wheel is at rest until its
            // first point, and that is drawn too.
            let played = curve.played();
            envelopes::paint_curve(&painter, &map, &played, frame.color, Some(fade(frame.color, 0.10)));
            envelopes::paint_points(&painter, &map, points, &cx.state.piano.lane_points, frame.color, theme.ink);
        }
        None => {
            let label = format!("No {} in this clip \u{00B7} click to add a point", kind.label().to_lowercase());
            let room = rect.shrink2(vec2(10.0, 0.0));
            let style = trill_noir::TextStyle { upper: false, tracking: 0.2, ..theme.text.micro };
            text::paint(&painter, room, Align2::LEFT_CENTER, style, &label, theme.ink_faint);
        }
    }
    // Past the end of the clip (or of its loop) nothing of it plays, nor
    // what a trimmed clip hides before its start: shaded as the notes are.
    let (from, to) = frame.sounding;
    super::piano_roll::shade_outside(&painter, rect, map.x(from).round(), map.x(to).round(), theme.shade);
    if let Some(x) = frame.playhead {
        paint::vline(&painter, x, rect.y_range(), theme.signal);
    }
    band(ui, &painter, cx, &map);

    interact(ui, cx, frame, &response, &map, clip.id, kind, points);
    context_menu(ui, cx, frame, &response, &map, clip.id, kind, points);
    // Also ends a drag whose button was released outside the window.
    if cx.state.piano.lane_drag.is_some() && !ui.input(|input| input.pointer.primary_down()) {
        cx.state.piano.lane_drag = None;
    }
}

/// The rubber band of a selection being dragged out.
fn band(ui: &Ui, painter: &egui::Painter, cx: &Cx, map: &LaneMap) {
    if let Some(LaneDrag::Band { origin, .. }) = &cx.state.piano.lane_drag
        && let Some(pointer) = ui.input(|input| input.pointer.latest_pos())
    {
        let band = Rect::from_two_pos(pos2(map.x(origin.0), map.y_at(origin.1)), pointer).intersect(map.rect);
        painter.rect_filled(band, 0.0, cx.theme.selected);
        paint::frame(painter, band, cx.theme.ink_dim);
    }
}

#[expect(clippy::too_many_arguments, reason = "everything a gesture in the lane reads, gathered by the caller")]
fn interact(
    ui: &Ui,
    cx: &mut Cx,
    frame: &Frame,
    response: &Response,
    map: &LaneMap,
    clip: ClipId,
    kind: ControllerKind,
    points: &[AutomationPoint],
) {
    let modifiers = ui.input(|input| input.modifiers);
    let pencil = cx.state.arrangement.tool == Tool::Draw;
    let inside = |position: &Pos2| map.rect.contains(*position);
    let signature = frame.signature;
    let at = |beat: Beats| timeline::format_bar_beat(beat, signature);

    if cx.state.piano.lane_drag.is_none()
        && let Some(pointer) = response.hover_pos().filter(inside)
    {
        let on_point = envelopes::point_at(points, map, pointer);
        let on_line = envelopes::segment_at(points, map, pointer).filter(|_| on_point.is_none());
        let here = at(map.beat(pointer.x));
        let hint = match (pencil, on_point, on_line) {
            (true, ..) => {
                format!("{} at {here} \u{00B7} drag to draw steps on the grid", map.scale.format(map.value(pointer.y)))
            }
            (false, Some(point), _) => {
                ui.set_cursor_icon(CursorIcon::Grab);
                format!(
                    "{} at {} \u{00B7} drag to move, Shift for fine, Alt for no snapping \u{00B7} double-click removes",
                    map.scale.format(point.value),
                    at(point.beat)
                )
            }
            (false, None, Some(_)) => {
                ui.set_cursor_icon(CursorIcon::ResizeVertical);
                let value = automation_value(points, map.beat(pointer.x));
                format!(
                    "{} at {here} \u{00B7} drag moves the line, Alt-drag bends it \u{00B7} click adds a point",
                    map.scale.format(value)
                )
            }
            (false, None, None) => {
                format!(
                    "{} at {here} \u{00B7} click adds a point \u{00B7} drag selects",
                    map.scale.format(map.value(pointer.y))
                )
            }
        };
        cx.state.hint.set(hint);
    }

    if let Some(pointer) = response.interact_pointer_pos().filter(inside) {
        let double = response.double_clicked() || response.triple_clicked();
        match envelopes::point_at(points, map, pointer) {
            Some(point) if double => {
                cx.state.focus = FocusArea::Controllers;
                swap(cx, clip, kind, &[point], &[]);
                cx.state.piano.lane_points.clear();
            }
            Some(point) if response.clicked() => {
                cx.state.focus = FocusArea::Controllers;
                let mut chosen = if modifiers.shift { cx.state.piano.lane_points.clone() } else { Vec::new() };
                match chosen.iter().position(|selected| *selected == point) {
                    Some(index) => drop(chosen.remove(index)),
                    None => chosen.push(point),
                }
                cx.state.piano.lane_points = chosen;
            }
            None if response.clicked() && !pencil => {
                cx.state.focus = FocusArea::Controllers;
                // On the line the new point sits on it; elsewhere it goes
                // where the pointer is.
                let beat = frame.snapped(map.beat(pointer.x), modifiers.alt);
                let value = match envelopes::segment_at(points, map, pointer) {
                    Some(_) => automation_value(points, beat),
                    None => map.value(pointer.y),
                };
                let shape = if map.scale.stepped() { Segment::Hold } else { Segment::Linear };
                let point = AutomationPoint { beat, value, shape };
                if swap(cx, clip, kind, &[], &[point]) {
                    cx.state.piano.lane_points = vec![point];
                }
            }
            // The pencil's click is its shortest stroke: one grid cell.
            _ if response.clicked() && pencil && !double => {
                cx.state.focus = FocusArea::Controllers;
                let cell = timeline::snap_floor(map.beat(pointer.x), Some(frame.cell));
                write_steps(cx, clip, kind, (cell, cell), frame.cell, map.value(pointer.y));
            }
            _ => {}
        }
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|input| input.pointer.press_origin()).filter(inside)
    {
        cx.state.focus = FocusArea::Controllers;
        let drag = if pencil {
            cx.begin_gesture();
            // The cell under the press is written by the first step below.
            LaneDrag::Draw { last: timeline::snap_floor(map.beat(press.x), Some(frame.cell)) }
        } else if let Some(grabbed) = envelopes::point_at(points, map, press) {
            let mut chosen = cx.state.piano.lane_points.clone();
            if !chosen.contains(&grabbed) {
                chosen = if modifiers.shift { [chosen, vec![grabbed]].concat() } else { vec![grabbed] };
                cx.state.piano.lane_points = chosen.clone();
            }
            cx.begin_gesture();
            LaneDrag::Points { originals: chosen.clone(), current: chosen, grabbed, press }
        } else if let Some(ends) = envelopes::segment_at(points, map, press) {
            cx.begin_gesture();
            LaneDrag::Segment { originals: ends, current: ends, press, bend: modifiers.alt }
        } else {
            let kept = if modifiers.shift { cx.state.piano.lane_points.clone() } else { Vec::new() };
            LaneDrag::Band { origin: (map.beat(press.x), map.position(press.y)), kept }
        };
        cx.state.piano.lane_drag = Some(drag);
    }

    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some(drag) = cx.state.piano.lane_drag.clone()
    {
        let next = step(ui, cx, frame, map, (clip, kind), points, drag, pointer);
        cx.state.piano.lane_drag = Some(next);
    }
}

/// Carries a drag one frame further and returns how it stands.
#[expect(clippy::too_many_arguments, reason = "everything one step of a drag reads, gathered by the caller")]
fn step(
    ui: &Ui,
    cx: &mut Cx,
    frame: &Frame,
    map: &LaneMap,
    (clip, kind): (ClipId, ControllerKind),
    points: &[AutomationPoint],
    drag: LaneDrag,
    pointer: Pos2,
) -> LaneDrag {
    let modifiers = ui.input(|input| input.modifiers);
    let at = |beat: Beats| timeline::format_bar_beat(beat, frame.signature);
    match drag {
        LaneDrag::Points { originals, current, grabbed, press } => {
            ui.set_cursor_icon(CursorIcon::Grabbing);
            // The grabbed point lands on the grid and the others keep
            // their distance from it.
            let wanted = grabbed.beat + map.beat(pointer.x) - map.beat(press.x);
            let beats = frame.snapped(wanted, modifiers.alt) - grabbed.beat;
            let fine = if modifiers.shift { 0.1 } else { 1.0 };
            let height = (press.y - pointer.y) / map.travel() * fine;
            let others = envelopes::replaced(points, &current, &[]);
            let next = envelopes::moved(&originals, &others, beats, height, &map.scale);
            if let Some(shown) = next.iter().zip(&originals).find(|(_, original)| **original == grabbed) {
                cx.state.hint.set(format!("{} at {}", map.scale.format(shown.0.value), at(shown.0.beat)));
            }
            if next != current && swap(cx, clip, kind, &current, &next) {
                cx.state.piano.lane_points = next.clone();
                return LaneDrag::Points { originals, current: next, grabbed, press };
            }
            LaneDrag::Points { originals, current, grabbed, press }
        }
        LaneDrag::Segment { originals, current, press, bend: bending } => {
            ui.set_cursor_icon(CursorIcon::ResizeVertical);
            let up = press.y - pointer.y;
            let next = if bending {
                let rising = originals[1].value > originals[0].value;
                let shape = Segment::Curve(envelopes::bend(up, map.travel(), rising));
                [AutomationPoint { shape, ..originals[0] }, originals[1]]
            } else {
                let fine = if modifiers.shift { 0.1 } else { 1.0 };
                let lifted = envelopes::moved(&originals, &[], 0.0, up / map.travel() * fine, &map.scale);
                [lifted[0], lifted[1]]
            };
            cx.state.hint.set(match next[0].shape {
                Segment::Curve(amount) if bending => format!("Bend {:.0} %", amount * 100.0),
                _ => format!("{} \u{2013} {}", map.scale.format(next[0].value), map.scale.format(next[1].value)),
            });
            if next != current && swap(cx, clip, kind, &current, &next) {
                return LaneDrag::Segment { originals, current: next, press, bend: bending };
            }
            LaneDrag::Segment { originals, current, press, bend: bending }
        }
        LaneDrag::Band { origin, kept } => {
            let corner = (map.beat(pointer.x), map.position(pointer.y));
            let mut chosen = kept.clone();
            for point in envelopes::points_in_box(points, &map.scale, origin, corner) {
                if !chosen.contains(&point) {
                    chosen.push(point);
                }
            }
            cx.state.piano.lane_points = chosen;
            LaneDrag::Band { origin, kept }
        }
        LaneDrag::Draw { last } => {
            let cell = timeline::snap_floor(map.beat(pointer.x), Some(frame.cell));
            let value = map.value(pointer.y);
            cx.state.hint.set(format!("{} at {}", map.scale.format(value), at(cell)));
            write_steps(cx, clip, kind, (last, cell), frame.cell, value);
            LaneDrag::Draw { last: cell }
        }
    }
}

// ---- the context menu --------------------------------------------------------

/// What a right-click in the lane was on.
#[derive(Clone, Debug, PartialEq)]
struct MenuSubject {
    /// The point under the pointer.
    point: Option<AutomationPoint>,
    /// The line under the pointer, by the point it starts at.
    line: Option<AutomationPoint>,
    /// Where a new point would go.
    place: AutomationPoint,
}

#[expect(clippy::too_many_arguments, reason = "the same reading of the lane as its other gestures take")]
fn context_menu(
    ui: &Ui,
    cx: &mut Cx,
    frame: &Frame,
    response: &Response,
    map: &LaneMap,
    clip: ClipId,
    kind: ControllerKind,
    points: &[AutomationPoint],
) {
    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        cx.state.focus = FocusArea::Controllers;
        let point = envelopes::point_at(points, map, pointer);
        let line = envelopes::segment_at(points, map, pointer).map(|ends| ends[0]);
        let shape = if map.scale.stepped() { Segment::Hold } else { Segment::Linear };
        let place =
            AutomationPoint { beat: frame.snapped(map.beat(pointer.x), false), value: map.value(pointer.y), shape };
        // What was right-clicked is selected, so the menu's subject shows.
        if let Some(point) = point
            && !cx.state.piano.lane_points.contains(&point)
        {
            cx.state.piano.lane_points = vec![point];
        }
        ui.data_mut(|data| data.insert_temp(response.id, MenuSubject { point, line, place }));
    }
    let Some(subject) = ui.data(|data| data.get_temp::<MenuSubject>(response.id)) else { return };
    let theme = cx.theme;
    let has_curve = !points.is_empty();
    response.context_menu(|ui| {
        ui.set_min_width(210.0);
        // The line a shape applies to: the one under the pointer, or the
        // one leaving the point under it.
        if let Some(start) = subject.line.or(subject.point) {
            for (label, shape) in [("Straight line", Segment::Linear), ("Step: hold, then jump", Segment::Hold)] {
                if MenuItem::new(label).checked(start.shape == shape).show(ui, theme).clicked() {
                    let reshaped = AutomationPoint { shape, ..start };
                    if swap(cx, clip, kind, &[start], &[reshaped]) {
                        cx.state.piano.lane_points = vec![reshaped];
                    }
                }
            }
            trill_noir::menu_separator(ui, theme);
        }
        if subject.point.is_some() {
            let count = cx.state.piano.lane_points.len();
            let label = if count > 1 { format!("Delete {count} points") } else { "Delete point".to_owned() };
            if MenuItem::new(&label).shortcut("Del").show(ui, theme).clicked() {
                delete_selected(cx);
            }
        } else if MenuItem::new("Add point here").show(ui, theme).clicked()
            && swap(cx, clip, kind, &[], &[subject.place])
        {
            cx.state.piano.lane_points = vec![subject.place];
        }
        trill_noir::menu_separator(ui, theme);
        let label = format!("Clear the {} curve", kind.label().to_lowercase());
        if MenuItem::new(&label).enabled(has_curve).show(ui, theme).clicked() {
            cx.edit(|project| project.clip_mut(clip)?.set_controller(kind, Vec::new()));
            cx.state.piano.lane_points.clear();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::Project;

    fn clip_with(kinds: &[ControllerKind]) -> Clip {
        let mut project = Project::new("t");
        let track = project.add_track("A");
        let id = project.add_midi_clip(track, "c", 0.0, 4.0).unwrap();
        for kind in kinds {
            project.clip_mut(id).unwrap().set_controller(*kind, vec![AutomationPoint::new(0.0, 0.5)]).unwrap();
        }
        project.clip(id).unwrap().clone()
    }

    #[test]
    fn the_strip_offers_the_standard_controllers_and_whatever_else_the_clip_holds() {
        let labels = |clip: &Clip, shown: LaneShown| -> Vec<String> {
            offered(clip, shown).into_iter().map(ControllerKind::label).collect()
        };
        let standard = ["Expression", "Mod wheel", "Pitch wheel", "Sustain"];
        assert_eq!(labels(&clip_with(&[]), LaneShown::Velocity), standard);
        // What the clip holds beside them follows, each once.
        let held = clip_with(&[ControllerKind::BREATH, ControllerKind::EXPRESSION, ControllerKind::Controller(23)]);
        assert_eq!(labels(&held, LaneShown::Velocity), [&standard[..], &["Breath", "CC 23"]].concat());
        // And one that is being shown, though nothing is written for it yet.
        let shown = LaneShown::Controller(ControllerKind::Pressure);
        assert_eq!(labels(&clip_with(&[]), shown), [&standard[..], &["Pressure"]].concat());
        assert_eq!(labels(&held, LaneShown::Controller(ControllerKind::BREATH)).len(), 6);
    }

    #[test]
    fn a_lanes_chip_is_named_for_what_it_shows() {
        assert_eq!(chip_name(LaneShown::Velocity), "Velocity lane");
        assert_eq!(chip_name(LaneShown::Controller(ControllerKind::EXPRESSION)), "Expression lane");
        assert_eq!(chip_name(LaneShown::Controller(ControllerKind::Controller(23))), "CC 23 lane");
    }

    #[test]
    fn a_controllers_scale_is_in_the_steps_and_the_words_of_the_controller() {
        let expression = Scale::Controller(ControllerKind::EXPRESSION);
        assert_eq!((expression.position(0.0), expression.position(1.0)), (0.0, 1.0));
        assert_eq!(expression.value(0.5), 64.0 / 127.0, "one of the 128 steps");
        assert_eq!(expression.format(100.0 / 127.0), "100");
        assert!(!expression.stepped() && expression.straight(trill_core::Travel::Linear));

        let wheel = Scale::Controller(ControllerKind::PitchBend);
        assert_eq!((wheel.position(-1.0), wheel.position(0.0), wheel.position(1.0)), (0.0, 0.5, 1.0));
        assert_eq!((wheel.value(0.75), wheel.value(0.505)), (0.5, 0.0), "at rest in a dead zone about the middle");
        assert_eq!((wheel.format(0.5), wheel.format(-1.0)), ("+50 %".to_owned(), "-100 %".to_owned()));
        assert_eq!(wheel.default_position(), 0.5, "the line it is read against is its rest");

        let pedal = Scale::Controller(ControllerKind::SUSTAIN);
        assert_eq!((pedal.value(0.4), pedal.value(0.6)), (0.0, 1.0));
        assert_eq!((pedal.format(1.0), pedal.format(0.0)), ("Down".to_owned(), "Up".to_owned()));
        assert!(pedal.stepped());
    }

    #[test]
    fn what_to_do_about_a_silent_instrument_is_said_for_plugins_and_built_ins() {
        let (short, long) = silent_advice("SWAM Alto Sax 3", true);
        assert_eq!(short, "SWAM Alto Sax 3 is given notes and stays silent");
        assert!(long.contains("Expression curve") && long.contains("computer keyboard"), "{long}");
        let (short, long) = silent_advice("Sampler", false);
        assert_eq!(short, "Sampler is given notes and stays silent");
        assert!(long.contains("no sound loaded") && !long.contains("breath"), "{long}");
    }
}
