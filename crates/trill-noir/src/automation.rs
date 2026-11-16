//! The mark a control carries when an envelope can drive it.
//!
//! A control that is automated shows a small triangle in its top right
//! corner, in the signal color: the envelope, not the user, sets its value.
//! When the user takes hold of such a control the envelope is switched off
//! and the mark becomes an outline. The outline is a button: one click
//! switches the envelope back on. It is registered after the control it
//! sits on, so a press on it never reaches the control underneath.

use crate::paint;
use crate::theme::Theme;
use egui::{Id, Rect, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};

/// How an envelope stands to a control.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Automation {
    /// There is none: the control is the user's alone.
    #[default]
    None,
    /// An envelope sets the control's value.
    On,
    /// There is an envelope, switched off: the control stays where it is
    /// set by hand until the envelope is switched on again.
    Off,
}

impl Automation {
    /// On if `automated`, else none: for a control that only ever shows
    /// whether something drives it.
    pub fn when(automated: bool) -> Self {
        if automated { Self::On } else { Self::None }
    }
}

/// Side of the square in a control's corner that takes the click.
const ZONE: f32 = 11.0;

/// The part of `rect` a click on the mark of a switched-off envelope lands
/// in: its top right corner.
pub fn automation_zone(rect: Rect) -> Rect {
    let corner = rect.right_top();
    Rect::from_min_max(pos2(corner.x - ZONE, corner.y), corner + vec2(0.0, ZONE)).intersect(rect)
}

/// Paints the mark of `automation` in the corner of `rect`. For an
/// envelope that is switched off, also places the button that switches it
/// back on and returns its response: `clicked()` is the request. `name` is
/// what the control is called, for assistive technology.
///
/// Call it after the control itself has been placed, so the button lies on
/// top of it.
pub fn automation_mark(
    ui: &Ui,
    theme: &Theme,
    rect: Rect,
    id: Id,
    automation: Automation,
    name: &str,
) -> Option<Response> {
    match automation {
        Automation::None => None,
        Automation::On => {
            if ui.is_rect_visible(rect) {
                paint::corner_mark(&ui.painter_at(rect), rect, theme.signal);
            }
            None
        }
        Automation::Off => {
            // Drags too: a press here must not start one on what is under it.
            let response = ui.interact(automation_zone(rect), id.with("automation"), Sense::click_and_drag());
            let enabled = ui.is_enabled();
            response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, format!("Automation of {name} is off")));
            if ui.is_rect_visible(rect) {
                let painter = ui.painter_at(rect);
                // Under the pointer the mark fills in: what a click gives.
                if response.hovered() || response.is_pointer_button_down_on() {
                    paint::corner_mark(&painter, rect, theme.signal);
                }
                paint::hollow_corner_mark(&painter, rect, theme.signal);
            }
            Some(response)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use egui::{Event, Modifiers, PointerButton, Pos2};

    const RECT: Rect = Rect::from_min_max(pos2(100.0, 50.0), pos2(300.0, 68.0));

    /// Frames in which a mark of `automation` sits on `RECT` and is clicked
    /// at `at`; counts the clicks it reports.
    fn clicks(automation: Automation, at: Pos2) -> usize {
        let ctx = testing::context();
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let frames = [Vec::new(), vec![Event::PointerMoved(at)], vec![button(true)], vec![button(false)], Vec::new()];
        let mut clicks = 0;
        for (index, events) in frames.into_iter().enumerate() {
            testing::frame(&ctx, index as f64 / 60.0, events, |ui, theme| {
                let mark = automation_mark(ui, theme, RECT, Id::unique("control"), automation, "Cutoff");
                assert_eq!(mark.is_some(), automation == Automation::Off, "only a switched-off envelope has a button");
                clicks += mark.is_some_and(|mark| mark.clicked()) as usize;
            });
        }
        clicks
    }

    #[test]
    fn the_mark_of_an_envelope_that_is_off_takes_a_click_in_the_corner() {
        let corner = RECT.right_top() + vec2(-4.0, 4.0);
        assert_eq!(clicks(Automation::Off, corner), 1);
        // (With a control under it, as there always is, the mark takes
        // only what lands on it; see the Bar's own test.)
        assert_eq!(clicks(Automation::Off, RECT.center()), 0, "the rest of the control is the control's");
        assert_eq!(clicks(Automation::Off, RECT.right_top() + vec2(-30.0, 4.0)), 0);
        assert_eq!(clicks(Automation::On, corner), 0, "an envelope that is on shows it and takes no click");
        assert_eq!(clicks(Automation::None, corner), 0);
    }

    #[test]
    fn the_zone_stays_inside_small_controls() {
        assert_eq!(automation_zone(RECT), Rect::from_min_max(pos2(289.0, 50.0), pos2(300.0, 61.0)));
        let small = Rect::from_min_max(pos2(10.0, 10.0), pos2(16.0, 18.0));
        assert_eq!(automation_zone(small), small);
        assert_eq!(Automation::when(true), Automation::On);
        assert_eq!(Automation::when(false), Automation::None);
    }
}
