//! The Bar: a horizontal value cell that stands in for sliders and spin
//! boxes. Label on the left, value on the right, a wash filled up to the
//! value and a one-pixel line at it.
//!
//! The pointer is the value: pressing or dragging sets the position under
//! the pointer. Holding Ctrl (or Shift) moves a tenth as far for fine
//! adjustment, and a double-click restores the default. The wheel is left
//! alone, so scrolling a panel never edits a value.

use crate::theme::{Theme, fade};
use crate::{paint, text};
use egui::{Align2, Id, Rangef, Rect, Response, Sense, Ui, WidgetInfo, pos2};

/// How much slower a fine drag moves.
const FINE: f32 = 0.1;

/// What to show in a [`bar`].
#[derive(Clone, Debug)]
pub struct Bar<'a> {
    label: &'a str,
    value: &'a str,
    frac: f32,
    origin: Option<f32>,
    default: f32,
    dimmed: bool,
    compact: bool,
    automated: bool,
}

impl<'a> Bar<'a> {
    /// A bar reading `value`, filled from the left up to `frac` (0..=1).
    pub fn new(label: &'a str, value: &'a str, frac: f32) -> Self {
        Self { label, value, frac, origin: None, default: 0.0, dimmed: false, compact: false, automated: false }
    }

    /// A bar for a cell too narrow for a label: the value is centered and
    /// the position is a tick on the bottom edge instead of a fill. `name`
    /// is what assistive technology calls it.
    pub fn compact(name: &'a str, value: &'a str, frac: f32) -> Self {
        Self { compact: true, ..Self::new(name, value, frac) }
    }

    /// Fills outward from `origin` (0..=1) instead of from the left, and
    /// marks it: for ranges with a zero point inside them.
    pub fn bipolar(mut self, origin: f32) -> Self {
        self.origin = Some(origin);
        self
    }

    /// The position a double-click restores.
    pub fn default(mut self, frac: f32) -> Self {
        self.default = frac;
        self
    }

    /// Draws the bar stepped back, for a control whose device is bypassed.
    /// It still works.
    pub fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = dimmed;
        self
    }

    /// Marks the bar's corner in the signal color: an envelope, not the
    /// user, is setting this value.
    pub fn automated(mut self, automated: bool) -> Self {
        self.automated = automated;
        self
    }
}

/// What the user did to a [`bar`] this frame.
pub struct BarOutput {
    pub response: Response,
    /// The position the user asked for, when they did.
    pub frac: Option<f32>,
    /// True when that position is the default, asked for by a double-click.
    /// The caller can then restore the default value itself, exactly,
    /// rather than whatever the position converts back to.
    pub reset: bool,
    /// True on the frame the pointer went down on the bar: the moment to
    /// open an undo gesture, since the press itself already sets a value.
    pub pressed: bool,
}

/// Places a bar in `rect`.
pub fn bar(ui: &Ui, theme: &Theme, rect: Rect, id: Id, bar: &Bar) -> BarOutput {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::slider(enabled, bar.frac as f64, bar.label));

    let primary = |ui: &Ui| ui.input(|input| (input.pointer.primary_pressed(), input.pointer.primary_down()));
    let (just_pressed, held) = primary(ui);
    let pressed = just_pressed && response.is_pointer_button_down_on();
    let fine = ui.input(|input| input.modifiers.command || input.modifiers.shift);

    // The position while the button is down lives here rather than being
    // read back from the caller, whose value may lag a frame behind.
    let mut frac = None;
    let reset = response.double_clicked() || response.triple_clicked();
    if reset {
        frac = Some(bar.default);
    } else if held
        && response.is_pointer_button_down_on()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let previous: f32 = ui.data(|data| data.get_temp(id)).unwrap_or(bar.frac);
        let under_pointer = (pointer.x - rect.left()) / rect.width().max(1.0);
        let next = if fine {
            let start = if pressed { bar.frac } else { previous };
            start + response.drag_delta().x / rect.width().max(1.0) * FINE
        } else {
            under_pointer
        };
        let next = next.clamp(0.0, 1.0);
        ui.data_mut(|data| data.insert_temp(id, next));
        frac = Some(next);
    }

    if ui.is_rect_visible(rect) {
        let active = enabled && (response.hovered() || response.is_pointer_button_down_on());
        paint_bar(ui, theme, rect, bar, frac.unwrap_or(bar.frac), active);
    }
    BarOutput { response, frac, pressed, reset }
}

fn paint_bar(ui: &Ui, theme: &Theme, rect: Rect, bar: &Bar, frac: f32, active: bool) {
    let painter = ui.painter_at(rect);
    let frac = frac.clamp(0.0, 1.0);
    let hair = paint::hair(&painter);
    let x_at = |frac: f32| paint::snap(&painter, rect.left() + rect.width() * frac);
    // The edge stays inside the frame at both ends of the range.
    let edge = x_at(frac).clamp(rect.left(), rect.right() - hair);

    let from = bar.origin.map_or(rect.left(), |origin| x_at(origin.clamp(0.0, 1.0)));
    if !bar.compact {
        let fill = Rect::from_x_y_ranges(Rangef::new(from.min(edge), from.max(edge)), rect.y_range());
        painter.rect_filled(fill, 0.0, theme.fill);
        if bar.origin.is_some() {
            paint::vline(&painter, from.min(rect.right() - hair), rect.y_range(), theme.selected);
        }
    }
    if active {
        painter.rect_filled(rect, 0.0, theme.hover);
    }
    paint::frame(&painter, rect, theme.hairline);
    if bar.automated {
        paint::corner_mark(&painter, rect, theme.signal);
    }

    let opacity = if bar.dimmed { theme.disabled } else { 1.0 };
    if bar.compact {
        let inner = rect.shrink(1.0);
        text::paint(&painter, inner, Align2::CENTER_CENTER, theme.text.value, bar.value, fade(theme.ink, opacity));
        // Short enough to stay clear of the text's baseline.
        let tick = paint::snap(&painter, inner.left() + (inner.width() - 2.0) * frac);
        let mark = Rect::from_min_max(pos2(tick, inner.bottom() - 3.0), pos2(tick + 2.0, inner.bottom()));
        painter.rect_filled(mark, 0.0, fade(theme.ink, opacity));
        return;
    }
    // Secondary ink, not the faint one: the label sits on the fill wash,
    // where faint text would drop below what is legible.
    let label_color = fade(theme.ink_dim, opacity);
    let value_color = fade(theme.ink, opacity);
    let inner = rect.shrink2(egui::vec2(theme.metric.pad, 0.0));
    // The value is what matters; the label gets the room that is left.
    let value = text::layout(&painter, theme.text.value, bar.value, value_color, inner.width());
    let value_left = inner.right() - value.width();
    let label_room = Rect::from_min_max(inner.min, pos2(value_left - theme.metric.pad, inner.bottom()));
    let label_width = text::paint(&painter, label_room, Align2::LEFT_CENTER, theme.text.label, bar.label, label_color);
    text::place(&painter, inner, Align2::RIGHT_CENTER, value);

    // Where the edge would run through a glyph it is broken into stubs
    // above and below the text.
    let gap = theme.metric.gap;
    let near = |left: f32, right: f32| right > left && edge + hair > left - gap && edge < right + gap;
    let through_text = near(inner.left(), inner.left() + label_width) || near(value_left, inner.right());
    let line = fade(theme.ink, opacity);
    if through_text {
        let band = theme.text.value.size / 2.0 + 1.0;
        let middle = rect.center().y;
        paint::vline(&painter, edge, Rangef::new(rect.top(), (middle - band).round()), line);
        paint::vline(&painter, edge, Rangef::new((middle + band).round(), rect.bottom()), line);
    } else {
        paint::vline(&painter, edge, rect.y_range(), line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use egui::{Event, Modifiers, PointerButton, Pos2, vec2};

    const RECT: Rect = Rect::from_min_max(pos2(100.0, 50.0), pos2(300.0, 68.0));

    /// A bar on its own in a window, driven by pointer events.
    struct Rig {
        ctx: egui::Context,
        frac: f32,
        /// How many frames reported a press.
        presses: usize,
        time: f64,
    }

    impl Rig {
        fn new(frac: f32) -> Self {
            let mut rig = Self { ctx: testing::context(), frac, presses: 0, time: 0.0 };
            rig.frame(Modifiers::NONE, Vec::new());
            rig
        }

        fn frame(&mut self, modifiers: Modifiers, mut events: Vec<Event>) {
            self.time += 1.0 / 60.0;
            events.insert(0, Event::ModifiersChanged(modifiers));
            let (mut frac, mut presses) = (self.frac, self.presses);
            testing::frame(&self.ctx, self.time, events, |ui, theme| {
                let spec = Bar::new("Cutoff", "1.00 kHz", frac).default(0.6);
                let output = bar(ui, theme, RECT, Id::unique("bar"), &spec);
                presses += output.pressed as usize;
                if let Some(next) = output.frac {
                    frac = next;
                }
            });
            (self.frac, self.presses) = (frac, presses);
        }

        fn at(fraction: f32) -> Pos2 {
            pos2(RECT.left() + RECT.width() * fraction, RECT.center().y)
        }

        fn button(&mut self, modifiers: Modifiers, position: Pos2, pressed: bool) {
            let event = Event::PointerButton { pos: position, button: PointerButton::Primary, pressed, modifiers };
            self.frame(modifiers, vec![event]);
        }

        fn drag(&mut self, modifiers: Modifiers, from: f32, to: f32) {
            self.frame(modifiers, vec![Event::PointerMoved(Self::at(from))]);
            self.button(modifiers, Self::at(from), true);
            for step in 1..=5 {
                let fraction = from + (to - from) * step as f32 / 5.0;
                self.frame(modifiers, vec![Event::PointerMoved(Self::at(fraction))]);
            }
            self.button(modifiers, Self::at(to), false);
            self.frame(Modifiers::NONE, Vec::new());
        }
    }

    #[test]
    fn a_press_sets_the_position_under_the_pointer() {
        let mut rig = Rig::new(0.9);
        rig.frame(Modifiers::NONE, vec![Event::PointerMoved(Rig::at(0.25))]);
        rig.button(Modifiers::NONE, Rig::at(0.25), true);
        assert!((rig.frac - 0.25).abs() < 0.005, "{}", rig.frac);
        assert_eq!(rig.presses, 1, "the press is reported once, for the undo gesture");
        rig.button(Modifiers::NONE, Rig::at(0.25), false);
        assert!((rig.frac - 0.25).abs() < 0.005);
        assert_eq!(rig.presses, 1);
    }

    #[test]
    fn a_drag_follows_the_pointer_and_stops_at_the_ends() {
        let mut rig = Rig::new(0.1);
        rig.drag(Modifiers::NONE, 0.2, 0.8);
        assert!((rig.frac - 0.8).abs() < 0.005, "{}", rig.frac);
        rig.drag(Modifiers::NONE, 0.8, 1.7);
        assert_eq!(rig.frac, 1.0, "dragging past the end pins the value there");
        rig.drag(Modifiers::NONE, 0.5, -2.0);
        assert_eq!(rig.frac, 0.0);
        assert_eq!(rig.presses, 3);
    }

    #[test]
    fn ctrl_and_shift_drags_are_fine_and_relative() {
        for modifiers in [Modifiers::COMMAND, Modifiers::SHIFT] {
            let mut rig = Rig::new(0.5);
            // Pressing far from the value must not jump to the pointer.
            rig.drag(modifiers, 0.1, 0.6);
            assert!((rig.frac - 0.55).abs() < 0.005, "half the bar's width moves a twentieth: {}", rig.frac);
        }
    }

    #[test]
    fn a_double_click_restores_the_default() {
        let mut rig = Rig::new(0.2);
        rig.frame(Modifiers::NONE, vec![Event::PointerMoved(Rig::at(0.9))]);
        for _ in 0..2 {
            rig.button(Modifiers::NONE, Rig::at(0.9), true);
            rig.button(Modifiers::NONE, Rig::at(0.9), false);
        }
        assert_eq!(rig.frac, 0.6);
    }

    #[test]
    fn the_wheel_does_not_edit() {
        let mut rig = Rig::new(0.4);
        rig.frame(Modifiers::NONE, vec![Event::PointerMoved(Rig::at(0.5))]);
        let wheel = Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 3.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: Modifiers::NONE,
        };
        rig.frame(Modifiers::NONE, vec![wheel]);
        rig.frame(Modifiers::NONE, Vec::new());
        assert_eq!(rig.frac, 0.4);
        assert_eq!(rig.presses, 0);
    }
}
