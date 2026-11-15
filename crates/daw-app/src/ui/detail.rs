//! The detail panel along the bottom: a tab bar and the tab it selects.

use super::state::{Cx, DetailTab};
use super::{devices, piano_roll, theme, widgets};
use eframe::egui::{Align, Align2, Layout, Rect, Sense, Ui, pos2, vec2};

pub const DEFAULT_HEIGHT: f32 = 400.0;
pub const MIN_HEIGHT: f32 = 190.0;
pub const MAX_HEIGHT: f32 = 640.0;
pub(super) const TAB_BAR: f32 = 32.0;

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let area = ui.max_rect();
    // Everything below is placed by hand, which allocates nothing; a
    // resizable panel shrinks to what its content claims, so claim it all.
    ui.expand_to_include_rect(area);
    let bar = Rect::from_min_max(area.min, pos2(area.right(), area.top() + TAB_BAR));
    let body = Rect::from_min_max(pos2(area.left(), bar.bottom()), area.max);

    let painter = ui.painter().clone();
    painter.hline(bar.x_range(), bar.top() + 0.5, theme::hairline(theme::BORDER));
    painter.hline(bar.x_range(), bar.bottom() - 0.5, theme::hairline(theme::BORDER));

    let mut left = bar.left() + 14.0;
    for (tab, title) in [(DetailTab::Clip, "Clip"), (DetailTab::Devices, "Devices")] {
        let galley = painter.layout_no_wrap(title.to_owned(), theme::strong(12.0), theme::TEXT);
        let rect = Rect::from_min_size(pos2(left, bar.top()), vec2(galley.size().x + 20.0, TAB_BAR));
        let response = ui.interact(rect, ui.scope_id().with(("tab", title)), Sense::click());
        let selected = cx.state.detail_tab == tab;
        let color = if selected || response.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        painter.text(rect.center(), Align2::CENTER_CENTER, title, theme::strong(12.0), color);
        if selected {
            let underline = Rect::from_min_max(
                pos2(rect.left() + 8.0, rect.bottom() - 2.5),
                pos2(rect.right() - 8.0, rect.bottom() - 0.5),
            );
            painter.rect_filled(underline, 1.0, theme::ACCENT);
        }
        if response.clicked() {
            cx.state.detail_tab = tab;
        }
        left = rect.right() + 2.0;
    }

    // What the tab is showing, so its subject is never in doubt.
    let subject = match cx.state.detail_tab {
        DetailTab::Clip => cx
            .state
            .selected_clip
            .and_then(|id| cx.snap.clip(id))
            .map(|(track, clip)| format!("{}  /  {}", track.name, clip.name)),
        DetailTab::Devices => cx.state.selected_track.and_then(|id| cx.snap.track(id)).map(|track| track.name.clone()),
    };
    if let Some(subject) = subject {
        let space = Rect::from_min_max(pos2(left + 16.0, bar.top()), pos2(bar.right() - 14.0, bar.bottom()));
        widgets::text_in(&painter, space, Align2::RIGHT_CENTER, &subject, theme::font(12.0), theme::TEXT_FAINT);
    }

    widgets::place(ui, body, "tab", Layout::top_down(Align::Min), |ui| {
        ui.set_clip_rect(body.intersect(ui.clip_rect()));
        match cx.state.detail_tab {
            DetailTab::Clip => piano_roll::show(ui, cx),
            DetailTab::Devices => devices::show(ui, cx),
        }
    });
}
