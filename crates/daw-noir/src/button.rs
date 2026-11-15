//! Buttons, icon buttons and tabs.

use crate::icon::{Icon, paint_icon};
use crate::theme::Theme;
use crate::{paint, text};
use egui::{Align2, Color32, Id, Rect, Response, Role, Sense, Ui, WidgetInfo, vec2};

/// How a [`button`] presents itself.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Variant {
    /// A hairline box with primary ink.
    Default,
    /// The action a dialog or an empty view suggests: the selected wash.
    Primary,
    /// Something that loses work: the label takes the signal color.
    Danger,
}

/// Width a [`button`] needs for `label`.
pub fn button_width(ui: &Ui, theme: &Theme, label: &str) -> f32 {
    (text::width(ui.painter(), theme.text.caps, label) + 4.0 * theme.metric.pad + 2.0).ceil()
}

/// A text button in the current layout, sized to its label.
pub fn button(ui: &mut Ui, theme: &Theme, label: &str, variant: Variant) -> Response {
    let size = vec2(button_width(ui, theme, label), theme.metric.control);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let pressed = response.is_pointer_button_down_on();
        if variant == Variant::Primary || pressed {
            painter.rect_filled(rect, 0.0, theme.selected);
        }
        if enabled && response.hovered() {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        paint::frame(painter, rect, theme.hairline);
        if response.has_focus() {
            paint::frame(painter, rect, theme.ink);
        }
        let color = match variant {
            _ if !enabled => theme.ink_dim,
            Variant::Danger => theme.signal,
            Variant::Default | Variant::Primary => theme.ink,
        };
        text::paint(painter, rect, Align2::CENTER_CENTER, theme.text.caps, label, color);
    }
    response
}

/// A square button holding an icon, at an exact place. `on` gives it the
/// selected wash (playing, loop on); `color` is the icon's.
pub fn icon_button(ui: &Ui, theme: &Theme, rect: Rect, id: Id, icon: Icon, on: bool, color: Color32) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, on, icon.name()));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if on || response.is_pointer_button_down_on() {
            painter.rect_filled(rect, 0.0, theme.selected);
        }
        if enabled && response.hovered() {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        paint::frame(painter, rect, theme.hairline);
        let color = if enabled && response.hovered() && !on { theme.ink } else { color };
        paint_icon(painter, rect.center(), icon, color);
    }
    response
}

/// An icon with no box around it, for the quiet corners of a header.
pub fn bare_icon_button(ui: &Ui, theme: &Theme, rect: Rect, id: Id, icon: Icon, label: &str) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter().rect_filled(rect, 0.0, theme.hover);
        }
        let color = if response.hovered() { theme.ink } else { theme.ink_faint };
        paint_icon(ui.painter(), rect.center(), icon, color);
    }
    response
}

/// Width a [`tab`] needs for `label`.
pub fn tab_width(ui: &Ui, theme: &Theme, label: &str) -> f32 {
    (text::width(ui.painter(), theme.text.caps, label) + 28.0).ceil()
}

/// One tab of a tab bar: a cell with a hairline on its right. The active
/// tab takes the color of the content below it and an ink underline.
pub fn tab(ui: &Ui, theme: &Theme, rect: Rect, id: Id, label: &str, active: bool) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, active, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if active {
            painter.rect_filled(rect, 0.0, theme.bg);
            paint::hline(painter, rect.x_range(), rect.bottom() - 1.0, theme.ink);
        } else if response.hovered() {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        paint::vline(painter, rect.right() - 1.0, rect.y_range(), theme.hairline);
        let color = if active || response.hovered() { theme.ink } else { theme.ink_dim };
        text::paint(painter, rect, Align2::CENTER_CENTER, theme.text.caps, label, color);
    }
    response
}
