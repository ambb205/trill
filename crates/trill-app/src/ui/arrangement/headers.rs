//! The track headers: name, mute and solo, volume and pan, the sends when
//! there is room, and the gestures on the track as a whole: dragging it
//! to another place or into a group, dragging its lower edge to resize
//! it, folding a group, and the menu.

use super::rows::{self, RowKind};
use super::{Geometry, envelopes};
use crate::ui::automation::{self, Hold};
use crate::ui::shortcuts::{self, Command};
use crate::ui::sounds::DropTarget;
use crate::ui::state::{ArrangeDrag, ArrangementState, Cx, FocusArea, LaneOwner, TextEditState, TrackDrop};
use crate::ui::{colors, recording, units, widgets};
use eframe::egui::{self, Align2, CursorIcon, PointerButton, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use trill_core::{AutomationTarget, ChainRef, Role, Track, TrackId, TrackKind};
use trill_noir::{Bar, Chip, Icon, MenuItem, TRACK_HUES, fade, paint, text};

/// Width of the color strip down a header's left edge.
pub(super) const HEADER_STRIP: f32 = 3.0;
/// Where a header's content starts, and how far from its right edge it ends.
pub(in crate::ui) const HEADER_LEFT: f32 = 12.0;
pub(in crate::ui) const HEADER_RIGHT: f32 = 14.0;
/// Width of the track number before the name.
pub(in crate::ui) const HEADER_INDEX: f32 = 14.0;
/// Top of the name row and of the row of controls under it.
pub(in crate::ui) const HEADER_NAME_TOP: f32 = 6.0;
pub(in crate::ui) const HEADER_CONTROLS_TOP: f32 = 31.0;
/// Size of the mute and solo chips.
pub(in crate::ui) const HEADER_CHIP: egui::Vec2 = egui::Vec2::new(20.0, 17.0);
pub(in crate::ui) const HEADER_PAN_WIDTH: f32 = 46.0;
/// The meter at a header's right edge.
const HEADER_METER_WIDTH: f32 = 7.0;
const HEADER_METER_INSET: f32 = 4.0;
/// How far the members of a group are set in under it.
const MEMBER_INDENT: f32 = 10.0;
/// A header shorter than this has room for its name row only.
const TWO_ROWS: f32 = 52.0;
/// A send bar narrower than this leaves the unit off its value.
const NARROW_SEND: f32 = 130.0;
/// Height of the zone along a header's lower edge that resizes it.
const RESIZE_ZONE: f32 = 4.0;
/// The instrument new tracks get, so they make sound immediately.
const DEFAULT_INSTRUMENT: &str = "builtin:polysynth";
/// What a new return track holds, fully wet: the usual shared reverb.
const RETURN_EFFECT: &str = "builtin:reverb";

/// Where the parts of a track header are, relative to its rectangle. Shared
/// with the master row, whose controls line up with the tracks'.
pub(super) struct HeaderParts {
    /// Left edge of the content, right of the color strip.
    pub left: f32,
    /// Right edge of the content, left of the meter.
    pub right: f32,
    pub meter: Rect,
}

impl HeaderParts {
    pub fn of(rect: Rect, meter_inset: f32) -> Self {
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

pub(super) fn show(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let theme = cx.theme;
    // A child clipped to the header column, so rows scrolled under the
    // ruler can neither be seen nor clicked.
    let mut column = ui.new_child(UiBuilder::new().max_rect(geometry.headers));
    column.set_clip_rect(geometry.headers.intersect(ui.clip_rect()));
    column.painter().rect_filled(geometry.headers, 0.0, theme.panel);

    let tracks = &cx.snap.project.tracks;
    let mut number = 0;
    for row in &geometry.rows.rows {
        if matches!(row.kind, RowKind::Track { .. }) {
            number += 1;
        }
        if !geometry.is_visible(row) {
            continue;
        }
        let rect = geometry.header_rect(row);
        match row.kind {
            RowKind::Track { index, member } => header(&mut column, cx, geometry, &tracks[index], number, member, rect),
            RowKind::Lane { lane, .. } => {
                envelopes::header(&mut column, cx, lane, rect, HEADER_STRIP);
                resize_lane(&column, cx, lane, rect);
            }
        }
    }
    drop_indicator(&column, cx, geometry);
    paint::vline_left_of(column.painter(), geometry.headers.right(), geometry.headers.y_range(), theme.hairline);
}

/// A line where a dragged header would land, set in when it would join a
/// group there.
fn drop_indicator(ui: &Ui, cx: &Cx, geometry: &Geometry) {
    let Some(ArrangeDrag::MoveTrack { drop: Some(drop), .. }) = &cx.state.arrangement.drag else { return };
    let before = drop.before.and_then(|id| cx.snap.track_index(id)).and_then(|index| geometry.rows.track(index));
    let y = geometry.screen_y(before.map_or(geometry.rows.height, |row| row.top));
    let left = geometry.headers.left() + if drop.group.is_some() { MEMBER_INDENT + HEADER_STRIP } else { 0.0 };
    let line = Rect::from_min_max(pos2(left, y - 1.0), pos2(geometry.headers.right(), y + 1.0));
    ui.painter().rect_filled(line, 0.0, cx.theme.ink);
}

fn header(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry, track: &Track, number: usize, member: bool, rect: Rect) {
    let theme = cx.theme;
    let snapshot = cx.snap;
    let id = ui.scope_id().with(("track", track.id));
    let selected = cx.state.selected_track == Some(track.id) && !cx.state.master_selected;
    let color = colors::track_color(theme, track.color);
    let surface = if selected { theme.raised } else { theme.panel };
    let quiet = theme.quiet_on(surface);
    let mut parts = HeaderParts::of(rect, 5.0);
    let two_rows = rect.height() >= TWO_ROWS;

    // Registered first, so the controls placed over it take the pointer.
    let background = ui.interact(rect, id.with("background"), Sense::click_and_drag());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, surface);
    paint::hline_above(&painter, rect.x_range(), rect.bottom(), theme.hairline);
    let strip = Rect::from_min_max(rect.min, pos2(rect.left() + HEADER_STRIP, rect.bottom() - 1.0));
    painter.rect_filled(strip, 0.0, if track.mute { fade(color, 0.35) } else { color });
    if member {
        // A bar in the group's color ties the members to their group.
        let group = track.group.and_then(|group| snapshot.track(group));
        let hue = group.map_or(theme.ghost, |group| colors::track_color(theme, group.color));
        let left = rect.left() + HEADER_STRIP + 3.0;
        painter.rect_filled(
            Rect::from_min_max(pos2(left, rect.top()), pos2(left + 2.0, rect.bottom())),
            0.0,
            fade(hue, 0.6),
        );
        parts.left += MEMBER_INDENT;
    }

    cx.hint(
        &background,
        "Click to select \u{00B7} drag to reorder or into a group \u{00B7} double-click to rename \u{00B7} right-click for more",
    );
    if background.clicked() {
        cx.state.focus = FocusArea::Arrangement;
        cx.state.select_track(track.id);
    }
    if widgets::double_clicked(&background) {
        start_rename(cx, track);
    }
    reorder(ui, cx, geometry, &background, track);
    background.context_menu(|ui| track_menu(ui, cx, track));

    // Name row: number (or a group's fold caret), name, badge, automation,
    // mute, solo and arm. A short header centers it. The arm control is
    // last, beside the meter, and its place is kept on the tracks that
    // have none, so that mute and solo stand in one column.
    let name_top = if two_rows { rect.top() + HEADER_NAME_TOP } else { rect.center().y - HEADER_CHIP.y / 2.0 };
    let name_row = Rect::from_min_size(pos2(parts.left, name_top.round()), vec2(0.0, HEADER_CHIP.y));
    let chip = |index: f32| {
        let left = parts.right - (2.0 - index) * HEADER_CHIP.x - (1.0 - index) * 3.0;
        Rect::from_min_size(pos2(left, name_row.top()), HEADER_CHIP)
    };
    let index_rect = name_row.with_max_x(parts.left + HEADER_INDEX);
    if track.kind == TrackKind::Group {
        let folded = cx.state.arrangement.folded.contains(&track.id);
        let icon = if folded { Icon::CaretRight } else { Icon::CaretDown };
        let caret = Rect::from_center_size(pos2(index_rect.left() + 5.0, index_rect.center().y), vec2(16.0, 16.0));
        let name = if folded { format!("Unfold {}", track.name) } else { format!("Fold {}", track.name) };
        let response = trill_noir::bare_icon_button(ui, theme, caret, id.with("fold"), icon, &name);
        cx.hint(&response, if folded { "Show the tracks of this group" } else { "Fold the tracks of this group away" });
        if response.clicked() {
            toggle_fold(cx, track.id);
        }
    } else {
        let number_style = trill_noir::TextStyle { size: 11.0, ..theme.text.value };
        text::paint(&painter, index_rect, Align2::LEFT_CENTER, number_style, &number.to_string(), quiet);
    }
    let mut name_rect = Rect::from_min_max(index_rect.right_top(), pos2(chip(-2.0).left() - 6.0, name_row.bottom()));
    let badge = match track.kind {
        TrackKind::Normal if track.reference.is_some() => Some("Reference"),
        TrackKind::Normal => None,
        TrackKind::Group => Some("Group"),
        TrackKind::Return => Some("Return"),
    };
    if let Some(badge) = badge {
        // What kind of track this is, where the name leaves room.
        let width = text::paint(&painter, name_rect, Align2::RIGHT_CENTER, theme.text.micro, badge, quiet);
        name_rect.max.x -= width + 6.0;
    }
    // An instrument that is given notes and stays silent is marked here,
    // where the track is looked at when its clips play and nothing is
    // heard.
    let mark = Rect::from_min_max(pos2(name_rect.right() - 14.0, name_rect.top()), name_rect.max);
    if track.kind == TrackKind::Normal
        && crate::ui::controller_lane::silent_mark(ui, cx, mark, id.with("silent-instrument"), track)
    {
        name_rect.max.x = mark.left() - 2.0;
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
    let lanes_open = cx.state.arrangement.lanes.iter().any(|lane| lane.owner == LaneOwner::Track(track.id));
    let lanes_name = format!("Automation of {}", track.name);
    let lanes_chip = Chip::new("A", lanes_open).quiet(quiet).named(&lanes_name);
    let response = trill_noir::chip(ui, theme, chip(-2.0), id.with("automation"), &lanes_chip);
    automation::paint_owner_mark(&painter, cx, LaneOwner::Track(track.id), chip(-2.0));
    let about = "Show or hide this track\u{2019}s automation lanes";
    cx.hint(&response, shortcuts::with_keys(about, Command::ToggleAutomation, cx.keyboard.is_on()));
    if response.clicked() {
        cx.state.select_track(track.id);
        automation::toggle_lanes(cx, LaneOwner::Track(track.id));
    }

    if let Some(reference) = &track.reference {
        // A reference is beside the song: it is not muted or soloed, it is listened to.
        let name = format!("Listen to {}", track.name);
        let listen = Chip::new("L", reference.listening).accent(theme.signal).quiet(quiet).named(&name);
        let response = trill_noir::chip(ui, theme, chip(0.0), id.with("listen"), &listen);
        cx.hint(&response, "Listen to the reference in place of the song; it is silent otherwise, and in no render");
        if response.clicked() {
            let (track_id, listening) = (track.id, !reference.listening);
            cx.edit(|project| project.listen_to_reference(track_id, listening));
        }
    }
    if track.reference.is_none() {
        let (mute_name, solo_name) = (format!("Mute {}", track.name), format!("Solo {}", track.name));
        let mute = Chip::new("M", track.mute).accent(theme.warn).quiet(quiet).named(&mute_name);
        let response = trill_noir::chip(ui, theme, chip(-1.0), id.with("mute"), &mute);
        cx.hint(&response, "Mute this track");
        if response.clicked() {
            let (track_id, mute) = (track.id, !track.mute);
            cx.edit(|project| {
                project.track_mut(track_id)?.mute = mute;
                Ok(())
            });
        }
        let solo = Chip::new("S", track.solo).accent(theme.info).quiet(quiet).named(&solo_name);
        let response = trill_noir::chip(ui, theme, chip(0.0), id.with("solo"), &solo);
        cx.hint(&response, "Solo: hear only the soloed tracks");
        if response.clicked() {
            let (track_id, solo) = (track.id, !track.solo);
            cx.edit(|project| {
                project.track_mut(track_id)?.solo = solo;
                Ok(())
            });
        }
        if recording::armable(cx, track) {
            let name = format!("Arm {}", track.name);
            recording::arm_chip(ui, cx, track, chip(1.0), id.with("arm"), quiet, &name);
        }
    }

    if two_rows {
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
        volume_bar(ui, cx, track, volume_rect, id.with("volume"));
        pan_bar(ui, cx, track, pan_rect, id.with("pan"));

        // Sends, one bar per return track, as far as the header's height
        // allows. The mixer always shows them all.
        let step = theme.metric.bar + theme.metric.gap;
        let returns = snapshot.project.tracks.iter().filter(|other| other.kind == TrackKind::Return);
        // A reference sends nowhere: it is beside the song.
        let sends = track.kind != TrackKind::Return && track.reference.is_none();
        for (index, target) in returns.filter(|_| sends).enumerate() {
            let top = controls_top + (index + 1) as f32 * step;
            if top + theme.metric.bar > rect.bottom() - 5.0 {
                break;
            }
            let send =
                Rect::from_min_max(pos2(volume_rect.left(), top), pos2(pan_rect.right(), top + theme.metric.bar));
            send_bar(ui, cx, track, target, send, id.with(("send", target.id)));
        }
    }

    let levels = widgets::levels(cx.live.track_meter(track.id));
    trill_noir::stereo_meter(&painter, theme, parts.meter, levels, widgets::meter_zones());
    resize_track(ui, cx, track, rect, id);
    sound_drop(ui, cx, geometry, track, rect);
    preset_drop(ui, cx, geometry, track, rect);
}

/// A sound of an instrument and its effects, dragged over a header,
/// becomes the sound of the track.
fn preset_drop(ui: &Ui, cx: &mut Cx, geometry: &Geometry, track: &Track, rect: Rect) {
    let visible = rect.intersect(geometry.headers);
    if crate::ui::sound_presets::over(cx, visible).is_none() {
        return;
    }
    if crate::ui::sound_presets::offer_track(cx, track.id) {
        paint::frame(ui.painter(), rect.with_max_y(rect.bottom() - 1.0), cx.theme.ink);
    }
}

/// A sound dragged over a header goes to the track as a whole: into its
/// sampler, onto its drum rack, or as a clip at the edit cursor.
fn sound_drop(ui: &Ui, cx: &mut Cx, geometry: &Geometry, track: &Track, rect: Rect) {
    let visible = rect.intersect(geometry.headers);
    let over =
        cx.state.sound_drag.as_ref().and_then(|drag| drag.pointer).is_some_and(|pointer| visible.contains(pointer));
    if !over {
        return;
    }
    let instrument = crate::ui::sample_panels::instrument(cx, track);
    let beat = cx.state.arrangement.cursor;
    let Some(drag) = &mut cx.state.sound_drag else { return };
    match instrument {
        // An instrument takes the place of whatever the track plays
        // through, a sampler or a rack included.
        _ if drag.is_instrument() && track.kind == TrackKind::Normal => {
            drag.offer(DropTarget::TrackInstrument { track: track.id });
        }
        _ if drag.is_instrument() => {
            drag.refusal = Some("An instrument goes on a track that holds clips, not on a group or a return");
            return;
        }
        Some(device) if device.uri == crate::ui::sample_panels::SAMPLER => {
            drag.offer(DropTarget::Sampler { device: device.id });
        }
        Some(device) if device.uri == crate::ui::sample_panels::DRUM_RACK => {
            drag.offer(DropTarget::Rack { device: device.id });
        }
        _ if track.kind == TrackKind::Normal => drag.offer(DropTarget::Lane { track: track.id, beat }),
        _ => {
            drag.refusal = Some("Audio goes on a track that holds clips, not on a group or a return");
            return;
        }
    }
    paint::frame(ui.painter(), rect.with_max_y(rect.bottom() - 1.0), cx.theme.ink);
}

/// The volume bar of a track. While an envelope drives the volume the bar
/// shows what the envelope says; dragging it takes hold of the volume (see
/// [`automation::respond`]).
pub(in crate::ui) fn volume_bar(ui: &Ui, cx: &mut Cx, track: &Track, rect: Rect, id: egui::Id) {
    let target = AutomationTarget::TrackVolume { track: track.id };
    let hold = Hold::of(cx, &target);
    let volume = hold.driven().map_or(track.volume_db, |value| value as f32);
    let shown = units::format_db(volume);
    let bar = Bar::new("Vol", &shown, units::db_to_fader(volume)).default(units::db_to_fader(0.0));
    let output = trill_noir::bar(ui, cx.theme, rect, id, &bar.automation(hold.mark()));
    let (what, gestures) =
        (format!("Volume \u{00B7} {shown}"), "drag to set, Ctrl-drag for fine, double-click for 0 dB");
    if let Some(position) = automation::respond(cx, &target, hold, &output, &what, gestures) {
        let volume = if output.reset { 0.0 } else { units::fader_to_db(position) };
        let track_id = track.id;
        if volume != track.volume_db {
            cx.edit(|project| project.set_track_volume(track_id, volume));
        }
    }
}

/// The pan cell of a track; see [`volume_bar`] for how automation shows.
pub(in crate::ui) fn pan_bar(ui: &Ui, cx: &mut Cx, track: &Track, rect: Rect, id: egui::Id) {
    let target = AutomationTarget::TrackPan { track: track.id };
    let hold = Hold::of(cx, &target);
    let pan = hold.driven().map_or(track.pan, |value| value as f32);
    let shown = units::format_pan(pan);
    let bar = Bar::compact("Pan", &shown, (pan + 1.0) / 2.0).default(0.5);
    let output = trill_noir::bar(ui, cx.theme, rect, id, &bar.automation(hold.mark()));
    let (what, gestures) = (format!("Pan \u{00B7} {shown}"), "drag to set, Ctrl-drag for fine, double-click to center");
    if let Some(position) = automation::respond(cx, &target, hold, &output, &what, gestures) {
        let pan = position * 2.0 - 1.0;
        // A small dead zone makes center easy to find again.
        let (track_id, pan) = (track.id, if output.reset || pan.abs() < 0.03 { 0.0 } else { pan });
        if pan != track.pan {
            cx.edit(|project| project.set_track_pan(track_id, pan));
        }
    }
}

/// How much of `track` goes to the return track `to`. Dragged to the
/// bottom, or double-clicked, the send is removed. While an envelope
/// drives the send the bar shows what the envelope says; dragging it takes
/// hold of the send (see [`automation::respond`]).
pub(in crate::ui) fn send_bar(ui: &Ui, cx: &mut Cx, track: &Track, to: &Track, rect: Rect, id: egui::Id) {
    let target = AutomationTarget::Send { track: track.id, to: to.id };
    let hold = Hold::of(cx, &target);
    let set = track.sends.iter().find(|send| send.to == to.id).map(|send| send.level_db);
    // What is sent now: where the envelope has the send, at the bottom of
    // which nothing is, or what was set by hand.
    let level = match hold.driven() {
        Some(value) => Some(value as f32).filter(|value| *value > trill_core::project::MIN_VOLUME_DB),
        None => set,
    };
    let shown = level.map_or_else(|| "Off".to_owned(), units::format_db);
    // In a mixer strip there is no room for the return's name and a unit:
    // sends are in decibels throughout, and the hint spells it out.
    let in_bar = if rect.width() < NARROW_SEND { shown.trim_end_matches(" dB") } else { &shown };
    let bar = Bar::new(&to.name, in_bar, level.map_or(0.0, units::db_to_send));
    let output = trill_noir::bar(ui, cx.theme, rect, id, &bar.automation(hold.mark()));
    let what = format!("{} \u{00B7} {shown}", automation::send_name(&to.name));
    let gestures = "drag to set, double-click to switch off, right-click to automate";
    if let Some(position) = automation::respond(cx, &target, hold, &output, &what, gestures) {
        let next = (!output.reset && position > 0.005).then(|| units::send_to_db(position));
        if next != set {
            let (from, to) = (track.id, to.id);
            cx.edit(|project| project.set_send(from, to, next));
        }
    }
}

/// Dragging a header to another place in the list.
fn reorder(ui: &Ui, cx: &mut Cx, geometry: &Geometry, background: &egui::Response, track: &Track) {
    if background.drag_started_by(PointerButton::Primary) {
        cx.state.select_track(track.id);
        cx.state.arrangement.drag = Some(ArrangeDrag::MoveTrack { track: track.id, drop: None });
    }
    if background.dragged_by(PointerButton::Primary)
        && let Some(pointer) = background.interact_pointer_pos()
        && matches!(cx.state.arrangement.drag, Some(ArrangeDrag::MoveTrack { .. }))
    {
        ui.set_cursor_icon(CursorIcon::Grabbing);
        let tracks = &cx.snap.project.tracks;
        let drop = rows::drop_at(tracks, &geometry.rows, track.id, geometry.content_y(pointer.y));
        cx.state.arrangement.drag = Some(ArrangeDrag::MoveTrack { track: track.id, drop });
        cx.state.hint.set(match drop.and_then(|drop| drop.group).and_then(|group| cx.snap.track(group)) {
            Some(group) => format!("Release to put {} into {}", track.name, group.name),
            None => format!("Release to move {} here", track.name),
        });
    }
}

/// Puts a dragged track where it was dropped: its new place in the list
/// and its group, as one undo step.
pub(super) fn drop_track(cx: &mut Cx, track: TrackId, drop: TrackDrop) {
    let tracks = &cx.snap.project.tracks;
    let order = rows::reordered(tracks, track, drop);
    let joins = tracks.iter().any(|candidate| candidate.id == track && candidate.kind == TrackKind::Normal);
    cx.edit(|project| {
        if joins {
            project.set_track_group(track, drop.group)?;
        }
        for (index, id) in order.iter().enumerate() {
            project.move_track(*id, index)?;
        }
        Ok(())
    });
}

/// The zone along a header's lower edge: drag to set the track's height,
/// double-click to return it to the height of the others.
fn resize_track(ui: &Ui, cx: &mut Cx, track: &Track, rect: Rect, id: egui::Id) {
    let zone = Rect::from_min_max(pos2(rect.left(), rect.bottom() - RESIZE_ZONE), rect.max);
    let response = ui.interact(zone, id.with("resize"), Sense::click_and_drag());
    if response.hovered() || response.dragged() {
        ui.set_cursor_icon(CursorIcon::ResizeVertical);
        cx.state.hint.set("Drag to resize this track \u{00B7} double-click for the default height");
    }
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let (low, high) = (ArrangementState::MIN_TRACK_HEIGHT, ArrangementState::MAX_TRACK_HEIGHT);
        cx.state.arrangement.heights.insert(track.id, (pointer.y - rect.top()).clamp(low, high).round());
    }
    if widgets::double_clicked(&response) {
        cx.state.arrangement.heights.remove(&track.id);
    }
}

/// The same for an automation lane.
fn resize_lane(ui: &Ui, cx: &mut Cx, lane: usize, rect: Rect) {
    let zone = Rect::from_min_max(pos2(rect.left(), rect.bottom() - RESIZE_ZONE), rect.max);
    let response = ui.interact(zone, ui.scope_id().with(("resize-lane", lane)), Sense::click_and_drag());
    if response.hovered() || response.dragged() {
        ui.set_cursor_icon(CursorIcon::ResizeVertical);
        cx.state.hint.set("Drag to resize this lane");
    }
    let Some(open) = cx.state.arrangement.lanes.get_mut(lane) else { return };
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let (low, high) = (ArrangementState::MIN_LANE_HEIGHT, ArrangementState::MAX_TRACK_HEIGHT);
        open.height = (pointer.y - rect.top()).clamp(low, high).round();
    }
    if widgets::double_clicked(&response) {
        open.height = ArrangementState::DEFAULT_LANE_HEIGHT;
    }
}

fn toggle_fold(cx: &mut Cx, group: TrackId) {
    let folded = &mut cx.state.arrangement.folded;
    if !folded.remove(&group) {
        folded.insert(group);
    }
}

fn start_rename(cx: &mut Cx, track: &Track) {
    cx.state.renaming_track = Some(TextEditState { target: track.id, text: track.name.clone(), focused: false });
}

/// Says whether a track's notes are drums or pitches. A track that says
/// what its instrument says anyway is left saying nothing, so that it
/// follows the instrument if that is changed, and the song's file stays as
/// it was.
fn set_plays_drums(cx: &mut Cx, track: TrackId, drums: bool) {
    let by_instrument = cx.snap.has_drum_instrument(track);
    let role = (drums != by_instrument).then_some(if drums { Role::Drums } else { Role::Pitched });
    cx.edit(|project| {
        let mut profile = project.track(track)?.profile.clone();
        profile.role = role;
        project.set_track_profile(track, profile)
    });
}

fn track_menu(ui: &mut Ui, cx: &mut Cx, track: &Track) {
    let theme = cx.theme;
    let snapshot = cx.snap;
    ui.set_min_width(200.0);
    let id = track.id;
    if MenuItem::new("Rename").show(ui, theme).clicked() {
        start_rename(cx, track);
    }
    if MenuItem::new("Duplicate").show(ui, theme).clicked()
        && let Some(copy) = cx.edit(|project| project.duplicate_track(id))
    {
        cx.state.select_track(copy);
    }
    palette(ui, cx, track);
    trill_noir::menu_separator(ui, theme);

    let lanes_open = cx.state.arrangement.lanes.iter().any(|lane| lane.owner == LaneOwner::Track(id));
    let label = if lanes_open { "Hide automation" } else { "Show automation" };
    if MenuItem::new(label).shortcut(&shortcuts::keys(Command::ToggleAutomation)).show(ui, theme).clicked() {
        cx.state.select_track(id);
        automation::toggle_lanes(cx, LaneOwner::Track(id));
    }
    if track.kind == TrackKind::Normal {
        // A recording to hold the song against, and a track of the song again.
        let is = track.reference.is_some();
        let detail = "A recording beside the song: silent unless listened to, and in no render";
        if MenuItem::new("A reference").checked(is).detail(detail).show(ui, theme).clicked() {
            let reference = (!is).then(trill_core::Reference::default);
            cx.edit(|project| project.set_reference(id, reference));
        }
        let drums = snapshot.plays_drums(id);
        let detail = "Its notes are drums, named in the piano roll and written by name, not pitches";
        if MenuItem::new("Plays drums").checked(drums).detail(detail).show(ui, theme).clicked() {
            set_plays_drums(cx, id, !drums);
        }
    }
    trill_noir::menu_separator(ui, theme);

    match track.kind {
        TrackKind::Normal => {
            if MenuItem::new("Group this track")
                .shortcut(&shortcuts::keys(Command::GroupTrack))
                .show(ui, theme)
                .clicked()
            {
                group_track(cx, id);
            }
            let groups = snapshot.project.tracks.iter().filter(|other| other.kind == TrackKind::Group);
            for group in groups.filter(|group| track.group != Some(group.id)) {
                if MenuItem::new(&format!("Move into {}", group.name)).show(ui, theme).clicked() {
                    // To the end of the group, where a newcomer is expected.
                    let last = snapshot.project.tracks.iter().rev().find(|other| other.group == Some(group.id));
                    let after = last.map_or(group.id, |last| last.id);
                    let tracks = &snapshot.project.tracks;
                    let position = tracks.iter().position(|other| other.id == after).map_or(0, |index| index + 1);
                    let before = tracks.get(position).map(|next| next.id).filter(|next| *next != id);
                    drop_track(cx, id, TrackDrop { before, group: Some(group.id) });
                }
            }
            if MenuItem::new("Take out of its group").enabled(track.group.is_some()).show(ui, theme).clicked() {
                cx.edit(|project| project.set_track_group(id, None));
            }
        }
        TrackKind::Group => {
            let folded = cx.state.arrangement.folded.contains(&id);
            if MenuItem::new(if folded { "Unfold" } else { "Fold" }).show(ui, theme).clicked() {
                toggle_fold(cx, id);
            }
        }
        TrackKind::Return => {}
    }
    trill_noir::menu_separator(ui, theme);
    if MenuItem::new("Delete track").show(ui, theme).clicked() {
        cx.edit(|project| project.remove_track(id).map(drop));
    }
}

/// A row of the palette's hues in a track's menu; a click gives the track
/// that color.
fn palette(ui: &mut Ui, cx: &mut Cx, track: &Track) {
    const SWATCH: f32 = 14.0;
    const GAP: f32 = 4.0;
    let theme = cx.theme;
    let pad = 2.0 * theme.metric.pad;
    let width = (TRACK_HUES as f32 * (SWATCH + GAP) - GAP + 2.0 * pad).max(ui.available_width());
    let (row, _) = ui.allocate_exact_size(vec2(width, theme.metric.control), Sense::hover());
    let current = colors::track_color(theme, track.color);
    for index in 0..TRACK_HUES {
        let hue = theme.track_hue(index);
        let left = row.left() + pad + index as f32 * (SWATCH + GAP);
        let swatch = Rect::from_min_size(pos2(left, row.center().y - SWATCH / 2.0), vec2(SWATCH, SWATCH));
        let response = ui.interact(swatch.expand(GAP / 2.0), ui.scope_id().with(("hue", index)), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Color {}", index + 1)));
        ui.painter().rect_filled(swatch, 0.0, hue);
        if hue == current || response.hovered() {
            paint::frame(ui.painter(), swatch.expand(2.0), theme.ink);
        }
        if response.clicked() {
            let (track_id, color) = (track.id, colors::new_track_color(index));
            cx.edit(|project| {
                project.track_mut(track_id)?.color = color;
                Ok(())
            });
            ui.close();
        }
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

/// How many tracks of `kind` the project has.
fn count(cx: &Cx, kind: TrackKind) -> usize {
    cx.snap.project.tracks.iter().filter(|track| track.kind == kind).count()
}

/// Adds an empty group track and selects it. Tracks are dragged into it.
pub fn add_group_track(cx: &mut Cx) {
    let name = format!("Group {}", count(cx, TrackKind::Group) + 1);
    let color = colors::new_track_color(cx.snap.project.tracks.len());
    let added = cx.edit(|project| {
        let track = project.add_track_of_kind(name, TrackKind::Group);
        project.track_mut(track)?.color = color;
        Ok(track)
    });
    if let Some(track) = added {
        cx.state.select_track(track);
    }
}

/// Adds a return track holding a reverb with nothing dry in it, as one
/// undo step, and selects it. Tracks send to it from their send bars.
pub fn add_return_track(cx: &mut Cx) {
    let letter = (b'A' + (count(cx, TrackKind::Return) % 26) as u8) as char;
    let name = format!("Return {letter}");
    let color = colors::new_track_color(cx.snap.project.tracks.len());
    let added = cx.try_session(|session| {
        session.begin_gesture();
        let track = session.edit(|project| {
            let track = project.add_track_of_kind(name, TrackKind::Return);
            project.track_mut(track)?.color = color;
            Ok(track)
        });
        let result = track.and_then(|track| {
            let device = session.add_device(ChainRef::Track(track), RETURN_EFFECT, None, None)?;
            session.set_device_params(device, &[("mix".to_owned(), 1.0)])?;
            Ok(track)
        });
        session.end_gesture();
        result
    });
    if let Some(track) = added {
        cx.state.select_track(track);
    }
}

/// Puts the selected track into a new group of its own: `Ctrl+G`.
pub fn group_selected_track(cx: &mut Cx) {
    match cx.state.selected_track.and_then(|id| cx.snap.track(id)) {
        Some(track) if track.kind == TrackKind::Normal => group_track(cx, track.id),
        Some(_) => cx.state.toasts.info("Only a track that holds clips can be put into a group"),
        None => {}
    }
}

/// Creates a group track where `track` is and puts `track` into it, as one
/// undo step.
fn group_track(cx: &mut Cx, track: TrackId) {
    let name = format!("Group {}", count(cx, TrackKind::Group) + 1);
    let color = cx.snap.track(track).map_or(0, |track| track.color);
    cx.edit(|project| {
        let index = project.track_index(track)?;
        let group = project.add_track_of_kind(name, TrackKind::Group);
        project.track_mut(group)?.color = color;
        project.move_track(group, index)?;
        project.set_track_group(track, Some(group))
    });
}
