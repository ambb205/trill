//! Rows for popups: menu bars, context menus and dropdowns.
//!
//! The popup itself is egui's (`egui::Popup::menu`, `Response::context_menu`),
//! framed by the style [`crate::install`] sets. These are the rows inside.

use crate::icon::{Icon, paint_icon};
use crate::theme::{Theme, fade};
use crate::{paint, text};
use egui::{Align2, Id, Rect, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};

/// One row of a menu.
#[derive(Clone, Debug)]
pub struct MenuItem<'a> {
    label: &'a str,
    shortcut: &'a str,
    checked: Option<bool>,
    enabled: bool,
    detail: &'a str,
}

impl<'a> MenuItem<'a> {
    pub fn new(label: &'a str) -> Self {
        Self { label, shortcut: "", checked: None, enabled: true, detail: "" }
    }

    /// The keys that do the same, shown at the right edge.
    pub fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = shortcut;
        self
    }

    /// Makes the row a choice: a mark before the label shows whether it is
    /// the current one.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// A longer explanation, shown when the pointer rests on the row.
    pub fn detail(mut self, detail: &'a str) -> Self {
        self.detail = detail;
        self
    }

    /// Adds the row to a popup. A click closes the popup and is reported as
    /// `clicked()`.
    pub fn show(self, ui: &mut Ui, theme: &Theme) -> Response {
        let pad = 2.0 * theme.metric.pad;
        let mark = if self.checked.is_some() { 14.0 } else { 0.0 };
        let shortcut = if self.shortcut.is_empty() {
            0.0
        } else {
            text::width(ui.painter(), theme.text.value, self.shortcut) + 24.0
        };
        let wanted = text::width(ui.painter(), theme.text.body, self.label) + mark + shortcut + 2.0 * pad;
        let width = wanted.max(ui.available_width()).ceil();
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, response) = ui.allocate_exact_size(vec2(width, theme.metric.control), sense);
        let (enabled, label, checked) = (self.enabled, self.label, self.checked);
        response.widget_info(|| match checked {
            Some(checked) => WidgetInfo::selected(Role::Button, enabled, checked, label),
            None => WidgetInfo::labeled(Role::Button, enabled, label),
        });

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let lit = self.enabled && response.hovered();
            if lit {
                painter.rect_filled(rect, 0.0, theme.hover);
            }
            let mut content = rect.shrink2(vec2(pad, 0.0));
            if let Some(checked) = self.checked {
                if checked {
                    paint::dot(painter, content.left_center(), theme.metric.dot, theme.ink);
                }
                content.min.x += mark;
            }
            // A disabled row keeps its ink at the disabled opacity, so it
            // is dimmer than any enabled row and still says what it is.
            let unavailable = fade(theme.ink_dim, theme.disabled);
            let color = match (self.enabled, lit || self.checked == Some(true)) {
                (false, _) => unavailable,
                (true, true) => theme.ink,
                (true, false) => theme.ink_dim,
            };
            text::paint(painter, content, Align2::LEFT_CENTER, theme.text.body, self.label, color);
            let keys = if self.enabled { theme.ink_dim } else { unavailable };
            text::paint(painter, content, Align2::RIGHT_CENTER, theme.text.value, self.shortcut, keys);
        }
        let response = if self.detail.is_empty() { response } else { response.on_hover_text(self.detail) };
        if response.clicked() {
            ui.close();
        }
        response
    }
}

/// A caps heading above a group of rows.
pub fn menu_heading(ui: &mut Ui, theme: &Theme, title: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let content = Rect::from_min_max(pos2(rect.left() + 2.0 * theme.metric.pad, rect.top() + 4.0), rect.max);
    text::paint(ui.painter(), content, Align2::LEFT_CENTER, theme.text.micro, title, theme.ink_faint);
}

/// A hairline between groups of rows.
pub fn menu_separator(ui: &mut Ui, theme: &Theme) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 5.0), Sense::hover());
    paint::hline(ui.painter(), rect.x_range(), rect.top() + 2.0, theme.hairline);
}

/// The caps title of a menu in a menu bar, at an exact place. Open its
/// popup with `egui::Popup::menu(&response)`.
pub fn menu_title(ui: &Ui, theme: &Theme, rect: Rect, id: Id, title: &str) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, title));
    if ui.is_rect_visible(rect) {
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        if open {
            ui.painter().rect_filled(rect, 0.0, theme.selected);
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, theme.hover);
        }
        let color = if open || response.hovered() { theme.ink } else { theme.ink_dim };
        text::paint(ui.painter(), rect, Align2::CENTER_CENTER, theme.text.caps, title, color);
    }
    response
}

/// Width a [`dropdown`] needs to show `value`.
pub fn dropdown_width(ui: &Ui, theme: &Theme, value: &str) -> f32 {
    (text::width(ui.painter(), theme.text.micro, value) + 2.0 * theme.metric.chip_pad + 14.0).ceil()
}

/// A chip showing the current choice and a caret. Open its popup with
/// `egui::Popup::menu(&response)`.
///
/// With a `label`, the chip reads like a Bar: the label on the left in the
/// label style and the choice on the right, for a column of parameters.
pub fn dropdown(ui: &Ui, theme: &Theme, rect: Rect, id: Id, label: &str, value: &str, dimmed: bool) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let name = if label.is_empty() { value.to_owned() } else { format!("{label}: {value}") };
    response.widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, &name));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        if open {
            painter.rect_filled(rect, 0.0, theme.selected);
        } else if response.hovered() {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        paint::frame(&painter, rect, theme.hairline);
        let lit = open || response.hovered();
        let color = match (lit, dimmed) {
            (true, _) => theme.ink,
            (false, false) => theme.ink_dim,
            (false, true) => fade(theme.ink_dim, theme.disabled),
        };
        let content = Rect::from_min_max(
            pos2(rect.left() + theme.metric.chip_pad + 1.0, rect.top()),
            pos2(rect.right() - 13.0, rect.bottom()),
        );
        if label.is_empty() {
            text::paint(&painter, content, Align2::LEFT_CENTER, theme.text.micro, value, color);
        } else {
            let used = text::paint(&painter, content, Align2::RIGHT_CENTER, theme.text.micro, value, color);
            let room = content.with_max_x(content.right() - used - theme.metric.pad);
            let quiet = if lit { theme.ink_dim } else { theme.ink_faint };
            text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.label, label, quiet);
        }
        paint_icon(&painter, pos2(rect.right() - 8.0, rect.center().y), Icon::CaretDown, color);
    }
    response
}
