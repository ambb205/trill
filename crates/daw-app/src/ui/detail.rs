//! The detail panel along the bottom: a row of tabs and the tab it selects.

use super::state::{Cx, DetailTab};
use super::{colors, devices, piano_roll, widgets};
use daw_core::ChainRef;
use daw_noir::{paint, text};
use eframe::egui::{Align, Align2, Layout, Rect, Ui, pos2, vec2};

pub const DEFAULT_HEIGHT: f32 = 340.0;
pub const MIN_HEIGHT: f32 = 190.0;
pub const MAX_HEIGHT: f32 = 640.0;
pub(super) const TAB_BAR: f32 = 24.0;

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let area = ui.max_rect();
    // Everything below is placed by hand, which allocates nothing; a
    // resizable panel shrinks to what its content claims, so claim it all.
    ui.expand_to_include_rect(area);
    let bar = Rect::from_min_max(area.min, pos2(area.right(), area.top() + TAB_BAR));
    let body = Rect::from_min_max(pos2(area.left(), bar.bottom()), area.max);

    let painter = ui.painter().clone();
    painter.rect_filled(body, 0.0, theme.bg);
    painter.rect_filled(bar, 0.0, theme.panel);
    paint::hline(&painter, bar.x_range(), bar.top(), theme.hairline);
    paint::hline_above(&painter, bar.x_range(), bar.bottom(), theme.hairline);

    // The tabs sit between the two rules, so the active one's underline
    // replaces the lower rule under it.
    let mut left = bar.left();
    for (tab, title) in [(DetailTab::Clip, "Clip"), (DetailTab::Devices, "Devices")] {
        let rect = Rect::from_min_size(
            pos2(left, bar.top() + 1.0),
            vec2(daw_noir::tab_width(ui, theme, title), TAB_BAR - 1.0),
        );
        let active = cx.state.detail_tab == tab;
        let response = daw_noir::tab(ui, theme, rect, ui.scope_id().with(("tab", title)), title, active);
        let about = match tab {
            DetailTab::Clip => "The selected clip and its notes",
            DetailTab::Devices => "The devices of the selected track, or of the master",
        };
        cx.hint(&response, about);
        if response.clicked() {
            cx.state.detail_tab = tab;
        }
        left = rect.right();
    }

    // What the tab is showing, so its subject is never in doubt.
    let track = cx.state.selected_track.and_then(|id| cx.snap.track(id));
    let subject =
        match cx.state.detail_tab {
            DetailTab::Clip => cx.state.selected_clip.and_then(|id| cx.snap.clip(id)).map(|(track, clip)| {
                (colors::track_color(theme, track.color), format!("{} / {}", track.name, clip.name))
            }),
            DetailTab::Devices => match cx.state.shown_chain() {
                ChainRef::Master => Some((theme.ink, "Master".to_owned())),
                ChainRef::Track(_) => track.map(|track| (colors::track_color(theme, track.color), track.name.clone())),
            },
        };
    if let Some((color, subject)) = subject {
        // A point to spare, so a width that rounds up does not cut the name.
        let wanted = text::width(&painter, theme.text.caps, &subject).ceil() + 1.0;
        let width = wanted.min(bar.right() - left - 60.0).max(0.0);
        let cell = Rect::from_min_max(
            pos2(bar.right() - width - theme.metric.dot - 5.0 - 28.0, bar.top() + 1.0),
            pos2(bar.right(), bar.bottom() - 1.0),
        );
        paint::vline(&painter, cell.left(), cell.y_range(), theme.hairline);
        let content = cell.shrink2(vec2(14.0, 0.0));
        let label_left = paint::dot(&painter, content.left_center(), theme.metric.dot, color) + 5.0;
        text::paint(
            &painter,
            content.with_min_x(label_left),
            Align2::LEFT_CENTER,
            theme.text.caps,
            &subject,
            theme.ink_dim,
        );
    }

    widgets::place(ui, body, "tab", Layout::top_down(Align::Min), |ui| {
        ui.set_clip_rect(body.intersect(ui.clip_rect()));
        match cx.state.detail_tab {
            DetailTab::Clip => piano_roll::show(ui, cx),
            DetailTab::Devices => devices::show(ui, cx),
        }
    });
}
