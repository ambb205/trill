//! The browser down the left side. Its two tabs list what can be brought
//! into the song: [`devices`] (instruments, effects, plugins and their
//! presets) and [`sounds`] (the sample library). What it was showing is
//! [`remembered`] from one session to the next.

mod devices;
mod remembered;
mod sounds;

use super::state::{BrowserTab, Cx, FocusArea};
use eframe::egui::{self, Rect, Ui, pos2, vec2};
use trill_noir::paint;

pub(super) use remembered::Remembered;
#[cfg(test)]
pub(in crate::ui) use sounds::query;
pub(super) use sounds::{choose_file, facts, keys, reveal};

/// Height of the row of tabs.
pub(in crate::ui) const HEADER: f32 = 24.0;
pub(in crate::ui) const EDGE: f32 = 10.0;
pub(in crate::ui) const SEARCH_HEIGHT: f32 = 22.0;
/// Height of the search field with the space around it.
pub(in crate::ui) const SEARCH_BLOCK: f32 = 8.0 + SEARCH_HEIGHT + 4.0;
/// Height of one row of a list.
pub(in crate::ui) const ROW: f32 = 22.0;

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let area = ui.max_rect();
    // Everything below is placed by hand, which allocates nothing; a
    // resizable panel shrinks to what its content claims, so claim it all.
    ui.expand_to_include_rect(area);
    cx.state.browser.width = area.width();
    cx.state.browser.rect = area;
    let painter = ui.painter().clone();
    painter.rect_filled(area, 0.0, theme.panel);
    paint::vline_left_of(&painter, area.right(), area.y_range(), theme.hairline);

    let header = Rect::from_min_size(area.min, vec2(area.width(), HEADER));
    paint::hline_above(&painter, header.x_range(), header.bottom(), theme.hairline);
    let mut left = header.left();
    for (tab, title, about) in [
        (BrowserTab::Devices, "Devices", "Instruments, effects and plugins to add, and their presets"),
        (BrowserTab::Sounds, "Sounds", "Your sample library: search it, hear it, drag it into the song"),
        (BrowserTab::Studies, "Studies", "Recordings taken apart: their stems, bars, chords and words"),
    ] {
        let rect = Rect::from_min_size(pos2(left, header.top()), vec2(trill_noir::tab_width(ui, theme, title), HEADER));
        let active = cx.state.browser.tab == tab;
        let response = trill_noir::tab(ui, theme, rect, ui.scope_id().with(("browser-tab", title)), title, active);
        cx.hint(&response, about);
        if response.clicked() {
            cx.state.browser.tab = tab;
            if tab != BrowserTab::Sounds && cx.state.focus == FocusArea::Browser {
                cx.state.focus = FocusArea::Arrangement;
            }
        }
        left = rect.right();
    }

    // The keyboard is the sound list's only while it is being worked in:
    // a press anywhere else in the window gives it back.
    let pressed_outside = ui.input(|input| {
        input.pointer.any_pressed() && input.pointer.interact_pos().is_some_and(|position| !area.contains(position))
    });
    if pressed_outside && cx.state.focus == FocusArea::Browser {
        cx.state.focus = FocusArea::Arrangement;
    }

    let body = Rect::from_min_max(pos2(area.left(), header.bottom()), area.max);
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt("browser-body").max_rect(body));
    child.set_clip_rect(body.intersect(ui.clip_rect()));
    match cx.state.browser.tab {
        BrowserTab::Devices => devices::show(&mut child, cx, body),
        BrowserTab::Sounds => sounds::show(&mut child, cx, body),
        BrowserTab::Studies => crate::ui::study::place::show(&mut child, cx, body),
    }
}
