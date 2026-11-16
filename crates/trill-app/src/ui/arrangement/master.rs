//! The master row, pinned along the bottom: volume and level where the
//! track headers are, the peak and the master's devices where the lanes
//! are, and above it the master's automation lanes when they are open.

use super::headers::{HEADER_STRIP, HeaderParts};
use super::{Geometry, HEADER_WIDTH, MASTER_HEIGHT, envelopes};
use crate::ui::automation::{self, Hold};
use crate::ui::state::{Cx, DetailTab, LaneOwner};
use crate::ui::{units, widgets};
use eframe::egui::{Align2, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use trill_core::AutomationTarget;
use trill_noir::{Bar, Chip, paint, text};

/// Width of the master's volume bar.
pub(in crate::ui) const MASTER_VOLUME_WIDTH: f32 = 112.0;
const LANES_CHIP: f32 = 20.0;

pub(super) fn show(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    lanes(ui, cx, geometry);
    row(ui, cx, geometry);
}

/// The master's open lanes, stacked above its row.
fn lanes(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
    let area = geometry.master_lanes;
    if area.height() <= 0.0 {
        return;
    }
    let theme = cx.theme;
    let body = area.with_min_x(geometry.lanes.left());
    let response = ui.interact(body, ui.scope_id().with("master-lanes"), Sense::click_and_drag());
    ui.painter().rect_filled(area, 0.0, theme.bg);
    paint::hline(ui.painter(), area.x_range(), area.top(), theme.hairline);

    // Clipped to the area, so a lane cut short by a small window does not
    // spill over the master row.
    let mut child = ui.new_child(UiBuilder::new().max_rect(area));
    child.set_clip_rect(area.intersect(ui.clip_rect()));
    let mut top = area.top();
    for lane in geometry.rows.master_lanes.clone() {
        let Some(height) = cx.state.arrangement.lanes.get(lane).map(|open| open.height) else { continue };
        let head = Rect::from_min_size(pos2(area.left(), top), vec2(HEADER_WIDTH, height));
        let lane_body = Rect::from_min_max(pos2(body.left(), top), pos2(body.right(), top + height));
        envelopes::header(&mut child, cx, lane, head, HEADER_STRIP);
        envelopes::show(&child, cx, geometry, &response, lane, lane_body, body);
        top += height;
    }
    paint::vline_left_of(ui.painter(), body.left(), area.y_range(), theme.hairline);
}

fn row(ui: &mut Ui, cx: &mut Cx, geometry: &Geometry) {
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

    let target = AutomationTarget::MasterVolume;
    let hold = Hold::of(cx, &target);
    let resting = cx.snap.project.master.volume_db;
    let volume = hold.driven().map_or(resting, |value| value as f32);
    let bar_rect = Rect::from_min_size(
        pos2(parts.right - MASTER_VOLUME_WIDTH, (head.center().y - theme.metric.bar / 2.0).round()),
        vec2(MASTER_VOLUME_WIDTH, theme.metric.bar),
    );

    // The chip that unfolds the master's lanes, left of the volume.
    let quiet = theme.quiet_on(surface);
    let chip_rect = Rect::from_min_size(
        pos2(bar_rect.left() - 3.0 - LANES_CHIP, bar_rect.top()),
        vec2(LANES_CHIP, theme.metric.bar),
    );
    let lanes_open = cx.state.arrangement.lanes.iter().any(|open| open.owner == LaneOwner::Master);
    let chip = Chip::new("A", lanes_open).quiet(quiet).named("Automation of the master");
    let response = trill_noir::chip(ui, theme, chip_rect, id.with("automation"), &chip);
    automation::paint_owner_mark(&painter, cx, LaneOwner::Master, chip_rect);
    cx.hint(&response, "Show or hide the master\u{2019}s automation lanes");
    if response.clicked() {
        automation::toggle_lanes(cx, LaneOwner::Master);
    }

    let title = Rect::from_min_max(pos2(parts.left, head.top()), pos2(chip_rect.left() - 6.0, head.bottom()));
    text::paint(&painter, title, Align2::LEFT_CENTER, theme.text.caps, "Master", theme.ink);
    let shown = units::format_db(volume);
    let bar = Bar::new("Vol", &shown, units::db_to_fader(volume)).default(units::db_to_fader(0.0));
    let output = trill_noir::bar(ui, theme, bar_rect, id.with("volume"), &bar.automation(hold.mark()));
    let (what, gestures) = (format!("Master volume \u{00B7} {shown}"), "drag to set, double-click for 0 dB");
    if let Some(position) = automation::respond(cx, &target, hold, &output, &what, gestures) {
        let next = if output.reset { 0.0 } else { units::fader_to_db(position) };
        if next != resting {
            cx.edit(|project| project.set_master_volume(next));
        }
    }

    let meter = cx.live.master;
    trill_noir::stereo_meter(&painter, theme, parts.meter, widgets::levels(meter), widgets::meter_zones());

    // Peak readout. It takes the signal color once the output has clipped
    // and keeps it until clicked, so an over is not missed while looking
    // elsewhere.
    let mut x = lane.left() + 10.0;
    let rest = |x: f32| Rect::from_min_max(pos2(x, lane.top()), lane.max);
    let start = x;
    x += text::paint(&painter, rest(x), Align2::LEFT_CENTER, theme.text.micro, "Peak", theme.ink_faint) + 5.0;
    let value_style = trill_noir::TextStyle { size: 12.0, ..theme.text.value };
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
        let response = trill_noir::chip(ui, theme, rect, id.with(("device", device.id)), &chip);
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
