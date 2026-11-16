//! The Fader: a Bar stood on end, for the channel strips of a mixer, where
//! level reads bottom to top beside a meter.
//!
//! It behaves as the [`crate::bar`] does: the pointer is the value, Ctrl or
//! Shift makes a drag fine, a double-click restores the default and the
//! wheel is left alone.

use crate::automation::{Automation, automation_mark};
use crate::bar::BarOutput;
use crate::paint;
use crate::theme::Theme;
use egui::{Id, Rangef, Rect, Sense, Ui, WidgetInfo, pos2};

/// How much slower a fine drag moves.
const FINE: f32 = 0.1;
/// Height of the cap that marks the position.
const CAP: f32 = 3.0;

/// What to show in a [`fader`].
#[derive(Clone, Debug)]
pub struct Fader<'a> {
    name: &'a str,
    frac: f32,
    default: f32,
    automation: Automation,
}

impl<'a> Fader<'a> {
    /// A fader at `frac` (0 at the bottom, 1 at the top). `name` is what
    /// assistive technology calls it.
    pub fn new(name: &'a str, frac: f32) -> Self {
        Self { name, frac, default: 0.0, automation: Automation::None }
    }

    /// The position a double-click restores. It is marked with a notch.
    pub fn default(mut self, frac: f32) -> Self {
        self.default = frac;
        self
    }

    /// Marks the fader's corner for the envelope that can drive it: filled
    /// while it sets this value, an outline that takes a click while it is
    /// switched off.
    pub fn automation(mut self, automation: Automation) -> Self {
        self.automation = automation;
        self
    }
}

/// Places a fader in `rect`.
pub fn fader(ui: &Ui, theme: &Theme, rect: Rect, id: Id, fader: &Fader) -> BarOutput {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::slider(enabled, fader.frac as f64, fader.name));

    let (just_pressed, held) = ui.input(|input| (input.pointer.primary_pressed(), input.pointer.primary_down()));
    let pressed = just_pressed && response.is_pointer_button_down_on();
    let fine = ui.input(|input| input.modifiers.command || input.modifiers.shift);
    // The cap's travel stops half its height short of each end, so it is
    // whole at both extremes.
    let travel = (rect.height() - CAP).max(1.0);

    let mut frac = None;
    let reset = response.double_clicked() || response.triple_clicked();
    if reset {
        frac = Some(fader.default);
    } else if held
        && response.is_pointer_button_down_on()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let previous: f32 = ui.data(|data| data.get_temp(id)).unwrap_or(fader.frac);
        let under_pointer = (rect.bottom() - CAP / 2.0 - pointer.y) / travel;
        let next = if fine {
            let start = if pressed { fader.frac } else { previous };
            start - response.drag_delta().y / travel * FINE
        } else {
            under_pointer
        };
        let next = next.clamp(0.0, 1.0);
        ui.data_mut(|data| data.insert_temp(id, next));
        frac = Some(next);
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let shown = frac.unwrap_or(fader.frac).clamp(0.0, 1.0);
        let y_at = |frac: f32| paint::snap(&painter, rect.bottom() - CAP - travel * frac);
        let cap_top = y_at(shown);
        painter.rect_filled(
            Rect::from_x_y_ranges(rect.x_range(), Rangef::new(cap_top, rect.bottom())),
            0.0,
            theme.fill,
        );
        if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        paint::frame(&painter, rect, theme.hairline);
        // A notch at each edge where the default sits.
        let notch = y_at(fader.default.clamp(0.0, 1.0)) + (CAP / 2.0).floor();
        for x in [Rangef::new(rect.left(), rect.left() + 4.0), Rangef::new(rect.right() - 4.0, rect.right())] {
            paint::hline(&painter, x, notch, theme.ghost);
        }
        let cap = Rect::from_min_max(pos2(rect.left(), cap_top), pos2(rect.right(), cap_top + CAP));
        painter.rect_filled(cap, 0.0, theme.ink);
    }
    let mark = automation_mark(ui, theme, rect, id, fader.automation, fader.name);
    BarOutput { response, frac, pressed, reset, mark }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use egui::{Event, Modifiers, PointerButton, Pos2};

    const RECT: Rect = Rect::from_min_max(pos2(100.0, 20.0), pos2(118.0, 20.0 + 100.0 + CAP));

    struct Rig {
        ctx: egui::Context,
        frac: f32,
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
                let spec = Fader::new("Volume", frac).default(0.85);
                let output = fader(ui, theme, RECT, Id::unique("fader"), &spec);
                presses += output.pressed as usize;
                if let Some(next) = output.frac {
                    frac = next;
                }
            });
            (self.frac, self.presses) = (frac, presses);
        }

        /// The point where the cap's middle sits at `fraction`.
        fn at(fraction: f32) -> Pos2 {
            pos2(RECT.center().x, RECT.bottom() - CAP / 2.0 - 100.0 * fraction)
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
    fn the_pointer_is_the_value_bottom_to_top() {
        let mut rig = Rig::new(0.2);
        rig.drag(Modifiers::NONE, 0.3, 0.75);
        assert!((rig.frac - 0.75).abs() < 0.005, "{}", rig.frac);
        assert_eq!(rig.presses, 1, "the press is reported once, for the undo gesture");
        rig.drag(Modifiers::NONE, 0.75, 1.6);
        assert_eq!(rig.frac, 1.0, "dragging past the top pins the value there");
        rig.drag(Modifiers::NONE, 0.5, -1.0);
        assert_eq!(rig.frac, 0.0);
    }

    #[test]
    fn fine_drags_are_relative_and_a_double_click_restores_the_default() {
        let mut rig = Rig::new(0.5);
        rig.drag(Modifiers::SHIFT, 0.1, 0.6);
        assert!((rig.frac - 0.55).abs() < 0.005, "half the travel moves a twentieth: {}", rig.frac);

        rig.frame(Modifiers::NONE, vec![Event::PointerMoved(Rig::at(0.1))]);
        for _ in 0..2 {
            rig.button(Modifiers::NONE, Rig::at(0.1), true);
            rig.button(Modifiers::NONE, Rig::at(0.1), false);
        }
        assert_eq!(rig.frac, 0.85);
    }
}
