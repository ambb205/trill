//! The master strip down the right edge: output level and volume, always
//! in view whatever else the window shows.

use super::state::Cx;
use super::{theme, units, widgets};
use eframe::egui::{Align2, Rect, Sense, StrokeKind, Ui, pos2, vec2};

pub const WIDTH: f32 = 86.0;
/// Scale marks shared by the fader and the meter, in dB.
const MARKS: [f32; 7] = [6.0, 0.0, -6.0, -12.0, -24.0, -36.0, -48.0];

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let area = ui.max_rect();
    let painter = ui.painter().clone();
    painter.vline(area.left() + 0.5, area.y_range(), theme::hairline(theme::BORDER));
    painter.text(
        pos2(area.center().x, area.top() + 15.0),
        Align2::CENTER_CENTER,
        "MASTER",
        theme::strong(10.0),
        theme::TEXT_FAINT,
    );

    // Peak readout. It turns red once the output has clipped and stays red
    // until clicked, so an over is not missed while looking elsewhere.
    let meter = cx.live.master;
    let readout = Rect::from_center_size(pos2(area.center().x, area.top() + 40.0), vec2(area.width() - 24.0, 22.0));
    let response = ui.interact(readout, ui.scope_id().with("master-peak"), Sense::click());
    let (fill, text) = if meter.clipped {
        (theme::fade(theme::ERROR, 0.9), theme::TEXT_ON_COLOR)
    } else {
        (theme::BG_INSET, theme::TEXT_DIM)
    };
    painter.rect_filled(readout, theme::RADIUS_SMALL, fill);
    let peak = if meter.peak_db > units::METER_FLOOR_DB { format!("{:.1}", meter.peak_db) } else { "-inf".to_owned() };
    painter.text(readout.center(), Align2::CENTER_CENTER, peak, theme::font(11.0), text);
    let hint = if meter.clipped { "The output clipped. Click to reset." } else { "Output peak, dBFS" };
    if response.on_hover_text(hint).clicked() {
        cx.state.clear_clip_indicator = true;
    }

    // Fader on the left, scale in the middle, meter on the right; all three
    // share one vertical scale.
    let column = Rect::from_min_max(
        pos2(area.left() + 12.0, readout.bottom() + 14.0),
        pos2(area.right() - 12.0, area.bottom() - 40.0),
    );
    if column.height() < 40.0 {
        return;
    }
    let fader = Rect::from_min_max(column.min, pos2(column.left() + 24.0, column.bottom()));
    // The fader's handle stops this far from each end of its rectangle.
    let travel = fader.shrink2(vec2(0.0, 8.0));
    let level = Rect::from_min_max(pos2(column.right() - 14.0, travel.top()), pos2(column.right(), travel.bottom()));

    for mark in MARKS {
        let y = travel.bottom() - travel.height() * units::db_to_fader(mark);
        painter.text(
            pos2((fader.right() + level.left()) / 2.0, y),
            Align2::CENTER_CENTER,
            if mark > 0.0 { format!("+{mark:.0}") } else { format!("{:.0}", mark.abs()) },
            theme::font(9.0),
            if mark == 0.0 { theme::TEXT_DIM } else { theme::TEXT_FAINT },
        );
    }

    widgets::paint_meter_vertical(&painter, level, meter);
    painter.rect_stroke(level, 2.0, theme::hairline(theme::BORDER), StrokeKind::Outside);

    let volume = cx.snap.project.master.volume_db;
    let drag = widgets::volume_fader_vertical(ui, fader, ui.scope_id().with("master-volume"), volume);
    if drag.response.drag_started() {
        cx.begin_gesture();
    }
    if let Some(volume) = drag.value {
        cx.edit(|project| project.set_master_volume(volume));
    }
    drag.response.on_hover_text("Master volume. Double-click for 0 dB.");

    painter.text(
        pos2(area.center().x, area.bottom() - 20.0),
        Align2::CENTER_CENTER,
        units::format_db(drag.value.unwrap_or(volume)),
        theme::font(11.0),
        theme::TEXT,
    );
}
