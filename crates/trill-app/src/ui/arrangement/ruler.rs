//! The ruler: three strips over the lanes. Along the top edge the loop
//! region, which is dragged by its ends and body and drawn anew by dragging
//! where it is not. Under it the markers. At the foot the bar numbers,
//! where a click moves the edit cursor and a drag selects time.
//!
//! Where the tempo or the time signature changes, the foot says so beside
//! the bar's number: "7/8", "140 BPM", an arrow before a tempo the song
//! glides to. Such a change is typed over with a double-click, dragged
//! along the song, and set, removed or made a glide from the right-click
//! menu of the foot.

use super::{Geometry, LABEL_MIN_PIXELS, ops};
use crate::ui::shortcuts::{self, Command};
use crate::ui::state::{ArrangeDrag, ChangeTarget, Cx, FocusArea, TextEditState, TimeSelection};
use crate::ui::{timeline, widgets};
use eframe::egui::{Align2, CursorIcon, PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, pos2, vec2};
use trill_core::project::LoopRegion;
use trill_core::{Beats, Marker, Project, TimeSignature};
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

/// What the tempo and the time signature do at one position of the song.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(in crate::ui) struct Change {
    pub beat: Beats,
    pub signature: Option<TimeSignature>,
    /// The tempo the song glides to, arriving here.
    pub glide: Option<f64>,
    /// The tempo that takes over here.
    pub tempo: Option<f64>,
}

impl Change {
    /// What the foot of the ruler says of it: a time signature, a tempo
    /// the song glides to behind an arrow, a tempo, with a dot between.
    pub fn said(&self) -> String {
        let bpm = |tempo: f64| format!("{}", (tempo * 100.0).round() / 100.0);
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.signature.map(|signature| format!("{}/{}", signature.numerator, signature.denominator)));
        parts.extend(self.glide.map(|tempo| format!("\u{2192}{}", bpm(tempo))));
        parts.extend(self.tempo.map(bpm));
        let unit = if self.glide.is_some() || self.tempo.is_some() { " BPM" } else { "" };
        format!("{}{unit}", parts.join(" \u{00B7} "))
    }
}

/// Every place where the song's tempo or time signature changes, in order.
pub(in crate::ui) fn changes(project: &Project) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    // The one at `beat`, made if there is none yet.
    fn at(changes: &mut Vec<Change>, beat: Beats) -> &mut Change {
        let index = match changes.iter().position(|change| (change.beat - beat).abs() < 1e-6) {
            Some(index) => index,
            None => {
                changes.push(Change { beat, signature: None, glide: None, tempo: None });
                changes.len() - 1
            }
        };
        &mut changes[index]
    }
    for change in &project.meter_changes {
        at(&mut changes, change.beat).signature = Some(change.signature);
    }
    for change in &project.tempo_changes {
        let there = at(&mut changes, change.beat);
        match change.ramp {
            true => there.glide = Some(change.tempo),
            false => there.tempo = Some(change.tempo),
        }
    }
    changes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    changes
}

/// Where the foot of the ruler says `change`: from its bar line on, the
/// bar's number and then what changes. Returns the whole of it and where
/// what changes begins.
fn change_rect(ui: &Ui, cx: &Cx, geometry: &Geometry, change: &Change) -> (Rect, f32) {
    let style = trill_noir::TextStyle { tracking: 0.4, ..cx.theme.text.micro };
    let number = text::width(ui.painter(), style, &timeline::format_bar_beat(change.beat, &geometry.meter));
    let said = text::width(ui.painter(), style, &change.said());
    let left = geometry.x(change.beat).round() + 1.0;
    let top = geometry.ruler.bottom() - TIME_STRIP + 1.0;
    let rect = Rect::from_min_size(pos2(left, top), vec2(3.0 + number + 6.0 + said + 5.0, TIME_STRIP - 2.0));
    (rect, left + 3.0 + number + 6.0)
}

/// The change whose words at the foot of the ruler are at `position`.
fn change_at(ui: &Ui, cx: &Cx, geometry: &Geometry, position: Pos2) -> Option<Change> {
    if strip_at(geometry, position.y) != Strip::Time {
        return None;
    }
    let found = changes(&cx.snap.project).into_iter().rev();
    found.into_iter().find(|change| change_rect(ui, cx, geometry, change).0.contains(position))
}

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
    change_field(ui, cx, geometry);
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
            (Strip::Time, _) if change_at(ui, cx, geometry, pointer).is_some() => {
                "Double-click to type another \u{00B7} drag to move it \u{00B7} right-click to remove it or make the tempo glide"
            }
            (Strip::Time, _) => {
                "Click to move the edit cursor \u{00B7} drag to select time \u{00B7} right-click to set the tempo or the time signature here"
            }
        };
        cx.state.hint.set(hint);
    }

    if let Some(pointer) = response.interact_pointer_pos() {
        let beat = geometry.snap(geometry.beat(pointer.x), free);
        let on_marker = marker_at(ui, cx, geometry, pointer);
        let strip = strip_at(geometry, pointer.y);
        let on_change = change_at(ui, cx, geometry, pointer);
        if widgets::double_clicked(response) && strip == Strip::Markers {
            match on_marker {
                Some(index) => start_rename(cx, index),
                None => ops::add_marker_at(cx, beat),
            }
        } else if let (true, Some(change)) = (widgets::double_clicked(response), on_change) {
            // The tempo, if the tempo changes there; else the time signature.
            start_change(cx, change.beat, change.tempo.is_some() || change.glide.is_some());
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
            (Strip::Time, _) => match change_at(ui, cx, geometry, press) {
                Some(change) => ArrangeDrag::MoveChange { beat: change.beat, grab_offset: beat - change.beat },
                None => ArrangeDrag::RulerSelect { anchor: beat },
            },
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
            Some(ArrangeDrag::MoveChange { beat: from, grab_offset }) => {
                ui.set_cursor_icon(CursorIcon::Grabbing);
                let beat = geometry.snap(geometry.beat(pointer.x) - grab_offset, free);
                // What the tempo does there and what the time signature
                // does there go together, as they are shown together.
                if beat > 1e-6 && (beat - from).abs() >= 1e-9 {
                    let there = changes(&cx.snap.project).into_iter().find(|change| (change.beat - from).abs() < 1e-6);
                    let moved = cx.edit(|project| {
                        let Some(change) = there else { return Ok(false) };
                        if change.tempo.is_some() || change.glide.is_some() {
                            project.move_tempo_at(from, beat)?;
                        }
                        if change.signature.is_some() {
                            project.move_time_signature_at(from, beat)?;
                        }
                        Ok(true)
                    });
                    if moved == Some(true) {
                        cx.state.arrangement.drag = Some(ArrangeDrag::MoveChange { beat, grab_offset });
                    }
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

/// Opens the field that types the tempo (`tempo`) or the time signature
/// at `beat`.
fn start_change(cx: &mut Cx, beat: Beats, tempo: bool) {
    let target = ChangeTarget::at(beat, tempo);
    cx.state.arrangement.editing_change = Some(TextEditState { target, text: change_text(cx, target), focused: false });
}

/// What the song has where `target` points, as it is typed: "140", "7/8".
/// Where nothing changes there yet, what is in force there.
fn change_text(cx: &Cx, target: ChangeTarget) -> String {
    let project = &cx.snap.project;
    let beat = target.beat();
    if target.tempo {
        let tempo = (project.tempo_map().tempo_at(beat) * 100.0).round() / 100.0;
        format!("{tempo}")
    } else {
        let signature = project.meter_map().signature_at(beat);
        format!("{}/{}", signature.numerator, signature.denominator)
    }
}

/// The field at the foot of the ruler while a tempo or a time signature is
/// being typed there.
fn change_field(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let Some(target) = cx.state.arrangement.editing_change.as_ref().map(|edit| edit.target) else { return };
    let ruler = geometry.ruler;
    let beat = target.beat();
    let left = (geometry.x(beat).round() + 1.0).clamp(ruler.left(), (ruler.right() - 70.0).max(ruler.left()));
    let field = Rect::from_min_size(pos2(left, ruler.bottom() - TIME_STRIP - 2.0), vec2(70.0, 20.0));
    // Typed over what is there: so that the same number typed again is
    // still an answer, the field starts from nothing to compare with.
    let edit = &mut cx.state.arrangement.editing_change;
    let Some(typed) = widgets::name_field(ui, cx.theme, field, target, "", edit) else { return };
    let typed = typed.trim().to_ascii_lowercase();
    if target.tempo {
        let number = typed.trim_end_matches("bpm").trim().replace(',', ".");
        match number.parse::<f64>() {
            Ok(tempo) => {
                // A glide that arrives there stays a glide, to its new tempo.
                let glides = changes(&cx.snap.project).iter().any(|change| {
                    (change.beat - beat).abs() < 1e-6 && change.glide.is_some() && change.tempo.is_none()
                });
                cx.edit(|project| project.set_tempo_at(beat, tempo, glides));
            }
            Err(_) => cx
                .state
                .toasts
                .error(format!("\u{201C}{typed}\u{201D} is not a tempo: type beats per minute, such as 140")),
        }
    } else {
        let parts = typed
            .split_once('/')
            .and_then(|(beats, note)| Some((beats.trim().parse().ok()?, note.trim().parse().ok()?)));
        match parts {
            Some((beats, note)) => {
                cx.edit(|project| project.set_time_signature_at(beat, beats, note));
            }
            None => {
                cx.state.toasts.error(format!("\u{201C}{typed}\u{201D} is not a time signature: type one such as 7/8"))
            }
        }
    }
}

/// What a right-click on the ruler was on.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Subject {
    marker: Option<usize>,
    beat: Beats,
    /// At the foot of the ruler, where the tempo and the time signature are set.
    foot: bool,
    change: Option<Change>,
}

/// Right-click menu of the ruler: the markers', and at its foot the
/// tempo's and the time signature's.
fn marker_menu(ui: &Ui, cx: &mut Cx, geometry: &Geometry, response: &Response) {
    let theme = cx.theme;
    if response.secondary_clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let change = change_at(ui, cx, geometry, pointer);
        let subject = Subject {
            marker: marker_at(ui, cx, geometry, pointer),
            // What changes there is spoken of where it is, not where the grid is.
            beat: change.map_or(geometry.snap(geometry.beat(pointer.x), false), |change| change.beat),
            foot: strip_at(geometry, pointer.y) == Strip::Time,
            change,
        };
        ui.data_mut(|data| data.insert_temp(response.id, subject));
    }
    let Some(Subject { marker, beat, foot, change }) = ui.data(|data| data.get_temp::<Subject>(response.id)) else {
        return;
    };
    response.context_menu(|ui| {
        ui.set_min_width(190.0);
        if foot {
            let place = timeline::format_bar_beat(beat, &geometry.meter);
            if MenuItem::new(&format!("Set the tempo at {place}\u{2026}")).show(ui, theme).clicked() {
                start_change(cx, beat, true);
            }
            if MenuItem::new(&format!("Set the time signature at {place}\u{2026}")).show(ui, theme).clicked() {
                start_change(cx, beat, false);
            }
            if let Some(change) = change {
                trill_noir::menu_separator(ui, theme);
                if let Some(tempo) = change.glide.or(change.tempo) {
                    let glides = change.glide.is_some();
                    if MenuItem::new("Glide to this tempo").checked(glides).show(ui, theme).clicked() {
                        cx.edit(|project| {
                            project.remove_tempo_at(beat)?;
                            project.set_tempo_at(beat, tempo, !glides)
                        });
                    }
                    if MenuItem::new("Remove the change of tempo").show(ui, theme).clicked() {
                        cx.edit(|project| project.remove_tempo_at(beat));
                    }
                }
                if change.signature.is_some()
                    && MenuItem::new("Remove the change of time signature").show(ui, theme).clicked()
                {
                    cx.edit(|project| project.remove_time_signature_at(beat));
                }
            }
            trill_noir::menu_separator(ui, theme);
            let selected = cx.state.arrangement.time_selection.is_some();
            let keys = shortcuts::keys(Command::LoopSelection);
            if MenuItem::new("Loop the selection").shortcut(&keys).enabled(selected).show(ui, theme).clicked() {
                ops::loop_selection(cx);
            }
            return;
        }
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
    let bars = &geometry.meter;
    let (first_beat, last_beat) = geometry.time.visible(ruler.width());
    // Ticks continue the lanes' grid up through the time strip, bars and
    // coarser only; numbers are spaced for legibility, independently of
    // the grid.
    let tick_step = geometry.grid.max(geometry.beats_per_bar);
    timeline::for_each_grid_line(first_beat, last_beat, bars, tick_step, |beat, _| {
        paint::vline(&painter, geometry.x(beat).round(), time_top..=ruler.bottom(), theme.grid);
    });
    // Where the tempo or the time signature changes, the bar's number
    // and what changes are said together, and no other number is put
    // under them.
    let editing = cx.state.arrangement.editing_change.as_ref().map(|edit| edit.target.beat());
    let changing: Vec<(Change, Rect, f32)> = changes(&cx.snap.project)
        .into_iter()
        .filter(|change| change.beat <= last_beat)
        .map(|change| {
            let (rect, said_at) = change_rect(ui, cx, geometry, &change);
            (change, rect, said_at)
        })
        .filter(|(_, rect, _)| rect.right() >= ruler.left())
        .collect();
    let label_step = timeline::grid_step(geometry.time.pixels_per_beat, signature, LABEL_MIN_PIXELS).max(tick_step);
    let style = trill_noir::TextStyle { tracking: 0.4, ..theme.text.micro };
    timeline::for_each_grid_line(first_beat, last_beat, bars, label_step, |beat, _| {
        let x = geometry.x(beat).round();
        if changing.iter().any(|(_, rect, _)| x + 34.0 > rect.left() && x < rect.right()) {
            return;
        }
        let room = Rect::from_min_max(pos2(x + 4.0, time_top + 1.0), ruler.max);
        text::paint(
            &painter,
            room,
            Align2::LEFT_CENTER,
            style,
            &timeline::format_bar_beat(beat, bars),
            theme.ink_faint,
        );
    });
    for (index, (change, rect, said_at)) in changing.iter().enumerate() {
        let x = geometry.x(change.beat).round();
        paint::vline(&painter, x, time_top..=ruler.bottom(), theme.signal);
        // Zoomed far out, one change's words give way where the next begin.
        let room = changing.get(index + 1).map_or(ruler.right(), |(_, next, _)| next.left() - 4.0);
        let painter = painter.with_clip_rect(Rect::from_min_max(pos2(x, time_top), pos2(room.max(x), ruler.bottom())));
        let number = Rect::from_min_max(pos2(x + 4.0, time_top + 1.0), ruler.max);
        let place = timeline::format_bar_beat(change.beat, bars);
        text::paint(&painter, number, Align2::LEFT_CENTER, style, &place, theme.ink_faint);
        if editing.is_none_or(|editing| (editing - change.beat).abs() > 1e-6) {
            let said = Rect::from_min_max(pos2(*said_at, time_top + 1.0), ruler.max);
            text::paint(&painter, said, Align2::LEFT_CENTER, style, &change.said(), theme.ink);
        }
        // Said to a screen reader, and to a test, as it is shown.
        let response =
            ui.interact(*rect, ui.scope_id().with(("change", ChangeTarget::at(change.beat, true))), Sense::hover());
        let label = format!("At bar {place}: {}", change.said());
        response.widget_info(|| eframe::egui::WidgetInfo::labeled(eframe::egui::Role::Label, true, &label));
    }

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
        Geometry::new(area, &view, Rows::build(&project.tracks, &view), project.meter_map())
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
