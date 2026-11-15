//! Fields for typing: a number that can also be dragged, and plain text.
//!
//! Both wrap egui's text editor in the kit's hairline box, so typing,
//! selection and the clipboard behave as they do everywhere else.

use crate::format::parse_value;
use crate::theme::{TextStyle, Theme};
use crate::{paint, text};
use egui::{Align2, CursorIcon, FontId, Id, Key, Rect, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use std::ops::RangeInclusive;

/// What to show in a [`number_field`].
#[derive(Clone, Debug)]
pub struct NumberField<'a> {
    /// Caps label at the left edge; empty for none.
    pub label: &'a str,
    /// The value as displayed.
    pub text: &'a str,
    /// Micro caps after the value (a unit); empty for none.
    pub unit: &'a str,
    pub value: f64,
    pub range: RangeInclusive<f64>,
    /// Change per point of vertical drag.
    pub per_point: f64,
    /// The style of the value: `value` in a column of bars, `readout` in
    /// the transport.
    pub style: TextStyle,
    /// The name given to assistive technology.
    pub name: &'a str,
}

/// What the user did to a [`number_field`] this frame.
pub struct NumberOutput {
    pub response: Response,
    /// The value the user asked for, by dragging or by typing.
    pub value: Option<f64>,
}

/// A number in a hairline box. Drag up or down to change it (Ctrl or Shift
/// for a tenth of the speed); double-click to type, Enter to commit, Escape
/// to cancel.
pub fn number_field(ui: &mut Ui, theme: &Theme, rect: Rect, id: Id, field: &NumberField) -> NumberOutput {
    let (low, high) = (*field.range.start(), *field.range.end());
    let editing_id = id.with("typing");
    let typing: Option<String> = ui.data(|data| data.get_temp(editing_id));
    let inner = rect.shrink2(vec2(theme.metric.pad + 2.0, 0.0));

    if let Some(mut buffer) = typing {
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, theme.bg);
        paint::frame(&painter, rect, theme.ink);
        let response = edit_line(ui, inner, id, &mut buffer, field.style);
        let mut value = None;
        if response.lost_focus() {
            if ui.input(|input| input.key_pressed(Key::Enter)) {
                value = parse_value(&buffer, low, high);
            }
            ui.data_mut(|data| data.remove::<String>(editing_id));
        } else {
            response.request_focus();
            ui.data_mut(|data| data.insert_temp(editing_id, buffer));
        }
        return NumberOutput { response, value };
    }

    let response = ui.interact(rect, id, Sense::click_and_drag()).on_hover_cursor(CursorIcon::ResizeVertical);
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::SpinButton, enabled, field.name));

    // A drag is measured from where it started, so the result does not
    // depend on how many frames it took.
    let origin_id = id.with("origin");
    if response.drag_started() {
        ui.data_mut(|data| data.insert_temp(origin_id, field.value));
    }
    let mut value = None;
    if response.double_clicked() || response.triple_clicked() {
        ui.data_mut(|data| data.insert_temp(editing_id, field.text.to_owned()));
    } else if response.dragged()
        && let Some(travel) = response.total_drag_delta()
    {
        let origin: f64 = ui.data(|data| data.get_temp(origin_id)).unwrap_or(field.value);
        let fine = ui.input(|input| input.modifiers.command || input.modifiers.shift);
        let speed = if fine { field.per_point * 0.1 } else { field.per_point };
        value = Some((origin - travel.y as f64 * speed).clamp(low, high));
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        if enabled && (response.hovered() || response.dragged()) {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        paint::frame(&painter, rect, theme.hairline);
        if field.label.is_empty() {
            // Value then unit, read left to right like a sentence.
            let used = text::paint(&painter, inner, Align2::LEFT_CENTER, field.style, field.text, theme.ink);
            let unit = Rect::from_min_max(pos2(inner.left() + used + 7.0, inner.top()), inner.max);
            text::paint(&painter, unit, Align2::LEFT_CENTER, theme.text.micro, field.unit, theme.ink_faint);
        } else {
            let shown =
                if field.unit.is_empty() { field.text.to_owned() } else { format!("{} {}", field.text, field.unit) };
            let galley = text::layout(&painter, field.style, &shown, theme.ink, inner.width());
            let label =
                Rect::from_min_max(inner.min, pos2(inner.right() - galley.width() - theme.metric.pad, inner.bottom()));
            text::paint(&painter, label, Align2::LEFT_CENTER, theme.text.label, field.label, theme.ink_faint);
            text::place(&painter, inner, Align2::RIGHT_CENTER, galley);
        }
    }
    NumberOutput { response, value }
}

/// A single line of editable text in a hairline box, with `hint` shown
/// while it is empty.
pub fn text_field(ui: &mut Ui, theme: &Theme, rect: Rect, id: Id, buffer: &mut String, hint: &str) -> Response {
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, theme.bg);
    let inner = rect.shrink2(vec2(theme.metric.pad + 2.0, 0.0));
    let response = edit_line(ui, inner, id, buffer, theme.text.body);
    paint::frame(&painter, rect, if response.has_focus() { theme.ink_faint } else { theme.hairline });
    if buffer.is_empty() && !response.has_focus() {
        let style = TextStyle { size: theme.text.value.size, ..theme.text.body };
        text::paint(&painter, inner, Align2::LEFT_CENTER, style, hint, theme.ink_faint);
    }
    response
}

/// egui's single-line editor without its own frame, centered in `rect`.
fn edit_line(ui: &mut Ui, rect: Rect, id: Id, buffer: &mut String, style: TextStyle) -> Response {
    let font = FontId::proportional(style.size);
    let row = ui.fonts_mut(|fonts| fonts.row_height(&font));
    let line = Rect::from_center_size(rect.center(), vec2(rect.width(), row));
    let editor = egui::TextEdit::singleline(buffer)
        .id(id.with("editor"))
        .font(font)
        .frame(egui::Frame::NONE)
        .margin(egui::Margin::ZERO)
        .desired_width(line.width());
    ui.put(line, editor)
}
