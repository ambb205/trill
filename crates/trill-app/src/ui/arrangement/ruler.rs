//! The ruler: three strips over the lanes. Along the top edge the loop
//! region, which is dragged by its ends and body and drawn anew by dragging
//! where it is not. Under it the markers. At the foot the bar numbers,
//! where a click moves the edit cursor and a drag selects time.

use super::{Geometry, LABEL_MIN_PIXELS, ops};
use crate::ui::shortcuts::{self, Command};
use crate::ui::state::{ArrangeDrag, Cx, FocusArea, TextEditState, TimeSelection};
use crate::ui::{timeline, widgets};
use eframe::egui::{Align2, CursorIcon, PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};
use trill_core::project::LoopRegion;
use trill_core::{Beats, Marker};
use trill_noir::{MenuItem, paint, text};

/// The top strip, where the loop bar is grabbed. The bar itself is drawn
/// thinner than the zone that catches the pointer.
pub(in crate::ui) const LOOP_STRIP: f32 = 9.0;
/// The strip of markers under it.
pub(in crate::ui) const MARKER_STRIP: f32 = 14.0;
/// The strip of bar numbers at the foot.
pub(super) const TIME_STRIP: f32 = 17.0;
pub(in crate::ui) const RULER_HEIGHT: f32 = LOOP_STRIP + MARKER_STRIP + TIME_STRIP;
const LOOP_BAR: f32 = 4.0;
/// How far a grip hangs below the ruler's top edge.
const LOOP_GRIP: f32 = 8.0;
/// How close, in pixels, the pointer must be to an end of the loop bar to
/// grab it.
const LOOP_END_GRAB: f32 = 6.0;
/// Width of a marker's flag, and of the space kept after its name.
const MARKER_FLAG: f32 = 7.0;

/// Which strip of the ruler a position is in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Strip {
    Loop,
    Markers,
    Time,
}

fn strip_at(geometry: &Geometry, y: f32) -> Strip {
    let offset = y - geometry.ruler.top();
    if offset <= LOOP_STRIP {
        Strip::Loop
    } else if offset <= LOOP_STRIP + MARKER_STRIP {
        Strip::Markers
    } else {
        Strip::Time
    }
}

/// Which part of the loop bar is at `position`, if any.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LoopHit {
    Start,
    End,
    Body,
}

fn loop_hit(region: LoopRegion, geometry: &Geometry, position: Pos2) -> Option<LoopHit> {
    if strip_at(geometry, position.y) != Strip::Loop {
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

/// Where a marker's flag and name are, given the width of its name.
fn marker_rect(geometry: &Geometry, marker: &Marker, name_width: f32) -> Rect {
    let top = geometry.ruler.top() + LOOP_STRIP;
    let left = geometry.x(marker.beat).round();
    Rect::from_min_size(pos2(left, top), vec2(MARKER_FLAG + 3.0 + name_width + 4.0, MARKER_STRIP))
}

/// The marker whose flag or name is at `position`, by index. Later markers
/// are drawn over earlier ones, so they are found first.
fn marker_at(ui: &Ui, cx: &Cx, geometry: &Geometry, position: Pos2) -> Option<usize> {
    if strip_at(geometry, position.y) != Strip::Markers {
        return None;
    }
    let style = marker_style(cx);
    let markers = cx.snap.project.markers.iter().enumerate().rev();
    let hit = |(_, marker): &(usize, &Marker)| {
        marker_rect(geometry, marker, text::width(ui.painter(), style, &marker.name)).contains(position)
    };
    markers.into_iter().find(hit).map(|(index, _)| index)
}

fn marker_style(cx: &Cx) -> trill_noir::TextStyle {
    // A marker's name keeps its case: it is the user's word, not a label.
    trill_noir::TextStyle { upper: false, tracking: 0.2, ..cx.theme.text.micro }
}

pub(super) fn show(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let response = ui.interact(geometry.ruler, ui.scope_id().with("ruler"), Sense::click_and_drag());
    paint(ui, cx, geometry);
    interact(ui, cx, geometry, &response);
    marker_menu(ui, cx, geometry, &response);
    rename_field(ui, cx, geometry);
}

fn interact(ui: &Ui, cx: &mut Cx, geometry: &Geometry, response: &Response) {
    let region = cx.snap.project.loop_region;
    let free = ui.input(|input| input.modifiers.alt);

    if cx.state.arrangement.drag.is_none()
        && let Some(pointer) = response.hover_pos()
    {
        let hint = match (strip_at(geometry, pointer.y), loop_hit(region, geometry, pointer)) {
            (_, Some(LoopHit::Start | LoopHit::End)) => {
                ui.set_cursor_icon(CursorIcon::ResizeHorizontal);
                "Drag to move this end of the loop \u{00B7} L switches looping on and off"
            }
            (_, Some(LoopHit::Body)) => {
                ui.set_cursor_icon(CursorIcon::Grab);
                "Drag to move the loop \u{00B7} L switches looping on and off"
            }
            (Strip::Loop, None) => "Drag to draw the loop region here",
            (Strip::Markers, _) if marker_at(ui, cx, geometry, pointer).is_some() => {
                "Click to go to this marker \u{00B7} drag to move it \u{00B7} double-click to rename \u{00B7} right-click for more"
            }
            (Strip::Markers, _) => "Double-click to add a marker \u{00B7} M adds one at the edit cursor",
            (Strip::Time, _) => "Click to move the edit cursor \u{00B7} drag to select time \u{00B7} Ctrl+scroll zooms",
        };
        cx.state.hint.set(hint);
    }

    if let Some(pointer) = response.interact_pointer_pos() {
        let beat = geometry.snap(geometry.beat(pointer.x), free);
        let on_marker = marker_at(ui, cx, geometry, pointer);
        let strip = strip_at(geometry, pointer.y);
        if widgets::double_clicked(response) && strip == Strip::Markers {
            match on_marker {
                Some(index) => start_rename(cx, index),
                None => ops::add_marker_at(cx, beat),
            }
        } else if response.clicked() {
            cx.state.focus = FocusArea::Arrangement;
            match on_marker.and_then(|index| cx.snap.project.markers.get(index)) {
                Some(marker) => go_to(cx, marker.beat),
                None if strip != Strip::Loop => {
                    cx.state.arrangement.time_selection = None;
                    go_to(cx, beat);
                }
                None => {}
            }
        }
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(press) = ui.input(|input| input.pointer.press_origin())
    {
        let beat = geometry.beat(press.x);
        let drag = match (strip_at(geometry, press.y), loop_hit(region, geometry, press)) {
            (_, Some(LoopHit::Start)) => ArrangeDrag::StretchLoop { anchor: region.end },
            (_, Some(LoopHit::End)) => ArrangeDrag::StretchLoop { anchor: region.start },
            (_, Some(LoopHit::Body)) => ArrangeDrag::MoveLoop { grab_offset: beat - region.start },
            // Outside the bar, the strip draws a new region from here.
            (Strip::Loop, None) => ArrangeDrag::StretchLoop { anchor: geometry.snap(beat, free) },
            (Strip::Markers, _) => match marker_at(ui, cx, geometry, press) {
                Some(index) => ArrangeDrag::MoveMarker { index },
                None => ArrangeDrag::RulerSelect { anchor: beat },
            },
            (Strip::Time, _) => ArrangeDrag::RulerSelect { anchor: beat },
        };
        if !matches!(drag, ArrangeDrag::RulerSelect { .. }) {
            cx.begin_gesture();
        }
        cx.state.arrangement.drag = Some(drag);
    }

    if response.dragged_by(PointerButton::Primary)
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let beat = geometry.snap(geometry.beat(pointer.x), free);
        match cx.state.arrangement.drag.clone() {
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
            Some(ArrangeDrag::MoveMarker { index }) => {
                ui.set_cursor_icon(CursorIcon::Grabbing);
                let moved = cx.snap.project.markers.get(index).is_some_and(|marker| (marker.beat - beat).abs() >= 1e-9);
                // Passing another marker changes its place in the list.
                if moved && let Some(index) = cx.edit(|project| project.move_marker(index, beat)) {
                    cx.state.arrangement.drag = Some(ArrangeDrag::MoveMarker { index });
                }
            }
            Some(ArrangeDrag::RulerSelect { anchor }) => {
                let now = geometry.beat(pointer.x);
                let (start, end) = (geometry.snap(anchor.min(now), free), geometry.snap(anchor.max(now), free));
                let view = &mut cx.state.arrangement;
                view.time_selection = (end > start).then_some(TimeSelection { start, end, tracks: Vec::new() });
                view.cursor = start;
            }
            _ => {}
        }
    }
}

/// Moves the edit cursor and the playhead to `beat`. A click on the ruler
/// seeks even while the song plays: it is how one jumps around in it.
fn go_to(cx: &mut Cx, beat: Beats) {
    cx.state.arrangement.cursor = beat;
    cx.with_session(|session| session.seek(beat));
}

fn set_loop(cx: &mut Cx, start: Beats, end: Beats) {
    let region = cx.snap.project.loop_region;
    if (region.start - start).abs() >= 1e-9 || (region.end - end).abs() >= 1e-9 {
        cx.edit(|project| project.set_loop(region.enabled, start, end));
    }
}

fn start_rename(cx: &mut Cx, index: usize) {
    if let Some(marker) = cx.snap.project.markers.get(index) {
        cx.state.arrangement.renaming_marker =
            Some(TextEditState { target: index, text: marker.name.clone(), focused: false });
    }
}

/// Right-click menu of the marker strip.
fn marker_menu(ui: &Ui, cx: &mut Cx, geometry: &Geometry, response: &Response) {
    let theme = cx.theme;
    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let subject = (marker_at(ui, cx, geometry, pointer), geometry.snap(geometry.beat(pointer.x), false));
        ui.data_mut(|data| data.insert_temp(response.id, subject));
    }
    let Some((marker, beat)) = ui.data(|data| data.get_temp::<(Option<usize>, Beats)>(response.id)) else { return };
    response.context_menu(|ui| {
        ui.set_min_width(190.0);
        match marker.filter(|index| *index < cx.snap.project.markers.len()) {
            Some(index) => {
                if MenuItem::new("Rename marker").show(ui, theme).clicked() {
                    start_rename(cx, index);
                }
                if MenuItem::new("Delete marker").show(ui, theme).clicked() {
                    cx.edit(|project| project.remove_marker(index).map(drop));
                }
            }
            None => {
                if MenuItem::new("Add marker here").show(ui, theme).clicked() {
                    ops::add_marker_at(cx, beat);
                }
            }
        }
        trill_noir::menu_separator(ui, theme);
        let selected = cx.state.arrangement.time_selection.is_some();
        let keys = shortcuts::keys(Command::LoopSelection);
        if MenuItem::new("Loop the selection").shortcut(&keys).enabled(selected).show(ui, theme).clicked() {
            ops::loop_selection(cx);
        }
    });
}

/// The field over a marker's name while it is being renamed.
fn rename_field(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let Some(index) = cx.state.arrangement.renaming_marker.as_ref().map(|edit| edit.target) else { return };
    let Some(marker) = cx.snap.project.markers.get(index) else {
        cx.state.arrangement.renaming_marker = None;
        return;
    };
    let ruler = geometry.ruler;
    let left = (geometry.x(marker.beat).round() + MARKER_FLAG).clamp(ruler.left(), ruler.right() - 150.0);
    let field = Rect::from_min_size(pos2(left, ruler.top() + LOOP_STRIP - 3.0), vec2(150.0, 20.0));
    let edit = &mut cx.state.arrangement.renaming_marker;
    if let Some(name) = widgets::name_field(ui, cx.theme, field, index, &marker.name, edit) {
        cx.edit(|project| project.rename_marker(index, name));
    }
}

fn paint(ui: &Ui, cx: &Cx, geometry: &Geometry) {
    let theme = cx.theme;
    let ruler = geometry.ruler;
    let painter = ui.painter_at(ruler);
    painter.rect_filled(ruler, 0.0, theme.panel);
    paint::hline_above(&painter, ruler.x_range(), ruler.bottom(), theme.hairline);
    let time_top = ruler.bottom() - TIME_STRIP;
    paint::hline(&painter, ruler.x_range(), time_top, theme.grid);

    // The time selection shows on the strip it was drawn on.
    let view = &cx.state.arrangement;
    if let Some(selection) = view.time_selection.as_ref().filter(|selection| selection.end > selection.start) {
        let x = geometry.x(selection.start).round()..=geometry.x(selection.end).round();
        painter.rect_filled(Rect::from_x_y_ranges(x, time_top..=ruler.bottom()), 0.0, theme.selected);
    }

    let signature = geometry.signature;
    let (first_beat, last_beat) = geometry.time.visible(ruler.width());
    // Ticks continue the lanes' grid up through the time strip, bars and
    // coarser only; numbers are spaced for legibility, independently of
    // the grid.
    let tick_step = geometry.grid.max(geometry.beats_per_bar);
    timeline::for_each_grid_line(first_beat, last_beat, signature, tick_step, |beat, _| {
        paint::vline(&painter, geometry.x(beat).round(), time_top..=ruler.bottom(), theme.grid);
    });
    let label_step = timeline::grid_step(geometry.time.pixels_per_beat, signature, LABEL_MIN_PIXELS).max(tick_step);
    let style = trill_noir::TextStyle { tracking: 0.4, ..theme.text.micro };
    timeline::for_each_grid_line(first_beat, last_beat, signature, label_step, |beat, _| {
        let room = Rect::from_min_max(pos2(geometry.x(beat).round() + 4.0, time_top + 1.0), ruler.max);
        text::paint(
            &painter,
            room,
            Align2::LEFT_CENTER,
            style,
            &timeline::format_bar_beat(beat, signature),
            theme.ink_faint,
        );
    });

    // Markers: a flag on a short staff, and the name beside it.
    let renaming = view.renaming_marker.as_ref().map(|edit| edit.target);
    let marker_style = marker_style(cx);
    for (index, marker) in cx.snap.project.markers.iter().enumerate() {
        if marker.beat < first_beat - 64.0 || marker.beat > last_beat {
            continue;
        }
        let x = geometry.x(marker.beat).round();
        let top = ruler.top() + LOOP_STRIP;
        paint::vline(&painter, x, top..=ruler.bottom(), theme.ink_dim);
        let flag = vec![pos2(x, top + 2.0), pos2(x + MARKER_FLAG, top + 5.5), pos2(x, top + 9.0)];
        painter.add(Shape::convex_polygon(flag, theme.ink_dim, Stroke::NONE));
        if renaming != Some(index) {
            let room = Rect::from_min_max(pos2(x + MARKER_FLAG + 3.0, top), pos2(ruler.right(), top + MARKER_STRIP));
            text::paint(&painter, room, Align2::LEFT_CENTER, marker_style, &marker.name, theme.ink_dim);
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::DetailTab;
    use crate::ui::arrangement::rows::Rows;
    use crate::ui::state::UiState;
    use crate::ui::timeline::TimeView;
    use trill_core::Project;

    fn geometry() -> Geometry {
        let mut project = Project::new("t");
        project.add_track("A");
        let mut view = UiState::new(DetailTab::Clip).arrangement;
        view.time = TimeView { origin: 4.0, pixels_per_beat: 20.0 };
        let area = Rect::from_min_size(pos2(0.0, 100.0), vec2(1000.0, 400.0));
        Geometry::new(area, &view, Rows::build(&project.tracks, &view), project.time_signature)
    }

    #[test]
    fn the_ruler_has_three_strips() {
        let g = geometry();
        let top = g.ruler.top();
        assert_eq!(g.ruler.height(), RULER_HEIGHT);
        assert_eq!(strip_at(&g, top + 3.0), Strip::Loop);
        assert_eq!(strip_at(&g, top + LOOP_STRIP + 1.0), Strip::Markers);
        assert_eq!(strip_at(&g, top + LOOP_STRIP + MARKER_STRIP + 1.0), Strip::Time);
        assert_eq!(strip_at(&g, g.ruler.bottom() - 1.0), Strip::Time);
    }

    #[test]
    fn the_loop_bar_has_two_ends_and_a_body() {
        let g = geometry();
        let region = LoopRegion { enabled: true, start: 8.0, end: 16.0 };
        let y = g.ruler.top() + 3.0;
        assert_eq!(loop_hit(region, &g, pos2(g.x(8.0) + 2.0, y)), Some(LoopHit::Start));
        assert_eq!(loop_hit(region, &g, pos2(g.x(16.0) - 2.0, y)), Some(LoopHit::End));
        assert_eq!(loop_hit(region, &g, pos2(g.x(12.0), y)), Some(LoopHit::Body));
        assert_eq!(loop_hit(region, &g, pos2(g.x(20.0), y)), None, "beside the bar a new region can be drawn");
        assert_eq!(loop_hit(region, &g, pos2(g.x(12.0), g.ruler.bottom() - 2.0)), None, "the foot is for the cursor");
        // A very short region can still be stretched: its end wins.
        let tiny = LoopRegion { enabled: true, start: 8.0, end: 8.25 };
        assert_eq!(loop_hit(tiny, &g, pos2(g.x(8.1), y)), Some(LoopHit::End));
    }

    #[test]
    fn a_marker_is_grabbed_by_its_flag_and_its_name() {
        let g = geometry();
        let marker = Marker { beat: 12.0, name: "Chorus".into() };
        let rect = marker_rect(&g, &marker, 40.0);
        assert_eq!(rect.left(), g.x(12.0));
        assert_eq!((rect.top(), rect.height()), (g.ruler.top() + LOOP_STRIP, MARKER_STRIP));
        assert_eq!(rect.width(), MARKER_FLAG + 3.0 + 40.0 + 4.0);
    }
}
