//! The Chip: a small toggle drawn as a hairline box with micro caps text,
//! and rows of chips for choosing one of a few things.

use crate::theme::{Theme, fade};
use crate::{paint, text};
use egui::{Align2, Color32, Id, Pos2, Rect, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};

/// What to show in a [`chip`].
#[derive(Clone, Debug)]
pub struct Chip<'a> {
    label: &'a str,
    on: bool,
    dot: Option<Color32>,
    accent: Option<Color32>,
    action: bool,
    quiet: Option<Color32>,
    dimmed: bool,
    name: Option<&'a str>,
}

impl<'a> Chip<'a> {
    /// A toggle that reads `label`. On: the selected wash and primary ink.
    pub fn new(label: &'a str, on: bool) -> Self {
        Self { label, on, dot: None, accent: None, action: false, quiet: None, dimmed: false, name: None }
    }

    /// A momentary chip: it never latches, so it reads as a button rather
    /// than as a toggle that is off.
    pub fn action(label: &'a str) -> Self {
        Self { action: true, ..Self::new(label, false) }
    }

    /// A status dot before the label.
    pub fn dot(mut self, color: Color32) -> Self {
        self.dot = Some(color);
        self
    }

    /// Fills the chip with `color` while on, for the few toggles whose
    /// state must be seen across the window (mute, solo).
    pub fn accent(mut self, color: Color32) -> Self {
        self.accent = Some(color);
        self
    }

    /// The label's color while off, when the default would be too quiet for
    /// what the chip sits on (see [`Theme::quiet_on`]).
    pub fn quiet(mut self, color: Color32) -> Self {
        self.quiet = Some(color);
        self
    }

    /// Draws the chip stepped back; it still works.
    pub fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = dimmed;
        self
    }

    /// The name given to assistive technology, when the label alone (a
    /// single letter, a note value) would not say what the chip is.
    pub fn named(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }

    /// The width the chip needs for its label.
    pub fn width(&self, ui: &Ui, theme: &Theme) -> f32 {
        let dot = if self.dot.is_some() { theme.metric.dot + 5.0 } else { 0.0 };
        (text::width(ui.painter(), theme.text.micro, self.label) + dot + 2.0 * theme.metric.chip_pad + 2.0).ceil()
    }
}

/// Places a chip in `rect`. `clicked()` on the response is the toggle.
pub fn chip(ui: &Ui, theme: &Theme, rect: Rect, id: Id, chip: &Chip) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let enabled = ui.is_enabled();
    let name = chip.name.unwrap_or(chip.label);
    response.widget_info(|| {
        if chip.action {
            WidgetInfo::labeled(Role::Button, enabled, name)
        } else {
            WidgetInfo::selected(Role::CheckBox, enabled, chip.on, name)
        }
    });
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter_at(rect);
    let hovered = enabled && response.hovered();
    let lit = chip.on.then_some(chip.accent).flatten();
    match lit {
        Some(accent) => {
            painter.rect_filled(rect, 0.0, accent);
        }
        None => {
            if chip.on || response.is_pointer_button_down_on() {
                painter.rect_filled(rect, 0.0, theme.selected);
            } else if hovered {
                painter.rect_filled(rect, 0.0, theme.hover);
            }
            paint::frame(&painter, rect, theme.hairline);
        }
    }

    let color = match (lit, chip.on, hovered) {
        (Some(_), ..) => theme.on_accent,
        (None, true, _) | (None, false, true) => theme.ink,
        (None, false, false) => chip.quiet.unwrap_or(theme.ink_dim),
    };
    let color = if chip.dimmed { fade(color, theme.disabled) } else { color };
    let mut content = rect.shrink2(vec2(theme.metric.chip_pad + 1.0, 0.0));
    if let Some(dot) = chip.dot {
        let dot = if chip.dimmed { fade(dot, theme.disabled) } else { dot };
        content.min.x = paint::dot(&painter, content.left_center(), theme.metric.dot, dot) + 5.0;
        text::paint(&painter, content, Align2::LEFT_CENTER, theme.text.micro, chip.label, color);
    } else {
        // Centered, and allowed to use the padding when the chip was made
        // narrower than its label would like.
        text::paint(&painter, rect.shrink(1.0), Align2::CENTER_CENTER, theme.text.micro, chip.label, color);
    }
    response
}

/// Lays out one chip per label from `origin`, wrapping at `max_width`, with
/// the chip at `selected` on. Returns the label index that was clicked, if
/// one was, and the height the rows took.
#[expect(clippy::too_many_arguments, reason = "a placement call: where, which chips, and their state")]
pub fn chip_row(
    ui: &Ui,
    theme: &Theme,
    origin: Pos2,
    max_width: f32,
    id: Id,
    labels: &[&str],
    selected: Option<usize>,
    dimmed: bool,
) -> (Option<usize>, f32) {
    let (height, gap) = (theme.metric.chip, theme.metric.gap - 1.0);
    let mut clicked = None;
    let mut cursor = origin;
    for (index, label) in labels.iter().enumerate() {
        let spec = Chip::new(label, selected == Some(index)).dimmed(dimmed);
        let width = spec.width(ui, theme);
        if cursor.x > origin.x && cursor.x + width > origin.x + max_width {
            cursor = pos2(origin.x, cursor.y + height + gap);
        }
        let rect = Rect::from_min_size(cursor, vec2(width, height));
        if chip(ui, theme, rect, id.with(index), &spec).clicked() && selected != Some(index) {
            clicked = Some(index);
        }
        cursor.x += width + gap;
    }
    (clicked, cursor.y + height - origin.y)
}

/// The width of these chips side by side on one line.
pub fn chip_row_width(ui: &Ui, theme: &Theme, labels: &[&str]) -> f32 {
    let gap = theme.metric.gap - 1.0;
    let chips: f32 = labels.iter().map(|label| Chip::new(label, false).width(ui, theme)).sum();
    chips + gap * labels.len().saturating_sub(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{self, frame};
    use egui::{Event, Modifiers, PointerButton};

    fn click(position: Pos2) -> [Vec<Event>; 4] {
        let button = |pressed| Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        [vec![Event::PointerMoved(position)], vec![button(true)], vec![button(false)], Vec::new()]
    }

    #[test]
    fn a_row_selects_the_chip_that_is_clicked_and_wraps() {
        let ctx = testing::context();
        let labels = ["1/4", "1/8", "1/16", "1/32", "Triplet"];
        let origin = pos2(20.0, 20.0);
        let mut selected = Some(2);
        let mut rects = Vec::new();
        let mut height = 0.0;
        let mut time = 0.0;
        let mut run = |events: Vec<Event>, selected: &mut Option<usize>, rects: &mut Vec<Rect>, height: &mut f32| {
            time += 1.0 / 60.0;
            frame(&ctx, time, events, |ui, theme| {
                let (clicked, rows) = chip_row(ui, theme, origin, 150.0, Id::unique("row"), &labels, *selected, false);
                *height = rows;
                rects.clear();
                rects.extend((0..labels.len()).map(|index| {
                    let id = Id::unique("row").with(index);
                    ui.ctx().read_response(id).map(|response| response.rect).unwrap_or(Rect::NOTHING)
                }));
                if let Some(index) = clicked {
                    *selected = Some(index);
                }
            });
        };
        run(Vec::new(), &mut selected, &mut rects, &mut height);
        assert!(rects[4].top() > rects[0].top(), "the last chip wrapped to a second row: {rects:?}");
        assert_eq!(rects[0].height(), 18.0);
        assert_eq!(height, 18.0 * 2.0 + 2.0);

        let target = rects[0].center();
        for events in click(target) {
            run(events, &mut selected, &mut rects, &mut height);
        }
        assert_eq!(selected, Some(0));

        // Clicking the chip that is already on changes nothing.
        for events in click(target) {
            run(events, &mut selected, &mut rects, &mut height);
        }
        assert_eq!(selected, Some(0));
    }

    #[test]
    fn a_chip_is_as_wide_as_its_label_needs() {
        testing::with_ui(|ui, theme| {
            let short = Chip::new("M", false).width(ui, theme);
            let long = Chip::new("Ping Pong", false).width(ui, theme);
            let dotted = Chip::new("M", false).dot(theme.ok).width(ui, theme);
            assert!(long > short + 30.0);
            assert_eq!(dotted, short + 10.0);
            assert_eq!(short.fract(), 0.0, "widths are whole pixels");
            assert_eq!(chip_row_width(ui, theme, &["M", "Ping Pong"]), short + long + 2.0);
            assert_eq!(chip_row_width(ui, theme, &[]), 0.0);
        });
    }
}
