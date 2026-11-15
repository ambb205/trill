//! Glue between the views and the widget kit: the few helpers that know
//! about this app's state (meters, names being typed) or about how egui
//! lays things out.

use super::snapshot::MeterState;
use super::state::TextEditState;
use super::units::db_to_fader;
use daw_noir::{Levels, Theme, Zones};
use eframe::egui::{self, Id, Layout, Rect, Response, Ui, UiBuilder};
use std::hash::Hash;

/// Runs `add` in a child `Ui` that covers `rect`.
///
/// Views here position things by hand inside areas that egui also lays out
/// (a scroll area's row of panels, say). `Ui::scope_builder` would move the
/// parent's layout cursor to the end of the child; this leaves it alone.
/// `salt` keeps the ids of the child's widgets stable from frame to frame.
pub fn place<R>(
    ui: &mut Ui,
    rect: Rect,
    salt: impl egui::AsIdSalt,
    layout: Layout,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let mut child = ui.new_child(UiBuilder::new().id_salt(salt).max_rect(rect).layout(layout));
    add(&mut child)
}

/// True on the second click of a double-click.
///
/// A double-click that follows another click within about half a second is
/// reported by egui as a triple-click instead, and it must still count.
pub fn double_clicked(response: &Response) -> bool {
    response.double_clicked() || response.triple_clicked()
}

/// Where the colors of a level meter change. Meters share the faders'
/// scale, so these are positions on it: amber above -6 dBFS, the signal
/// color over full scale.
pub fn meter_zones() -> Zones {
    Zones { hot: db_to_fader(-6.0), over: db_to_fader(0.0) }
}

/// A meter's state as positions on the fader scale.
pub fn levels(meter: MeterState) -> Levels {
    let peak = db_to_fader(meter.peak_db);
    Levels { left: db_to_fader(meter.left_db), right: db_to_fader(meter.right_db), peak: (peak > 0.0).then_some(peak) }
}

/// A name field that edits a copy of `current` and returns the new text
/// only when the user commits it (Enter, or clicking elsewhere). Escape
/// cancels. `edit` holds the text between frames while the field has focus;
/// set it before calling to open the field for editing straight away.
pub fn name_field<Target: Copy + PartialEq + Hash>(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    target: Target,
    current: &str,
    edit: &mut Option<TextEditState<Target>>,
) -> Option<String> {
    let editing = edit.as_ref().filter(|state| state.target == target);
    let mut text = editing.map_or_else(|| current.to_owned(), |state| state.text.clone());
    let awaiting_focus = editing.is_some_and(|state| !state.focused);
    let had_focus = editing.is_some_and(|state| state.focused);

    let id = Id::unique(("name-field", target));
    let response = daw_noir::text_field(ui, theme, rect, id, &mut text, "");
    if awaiting_focus {
        response.request_focus();
    }

    if response.lost_focus() {
        *edit = None;
        let cancelled = ui.input(|input| input.key_pressed(egui::Key::Escape));
        let text = text.trim();
        return (!cancelled && !text.is_empty() && text != current).then(|| text.to_owned());
    }
    if response.has_focus() || awaiting_focus {
        *edit = Some(TextEditState { target, text, focused: true });
    } else if had_focus {
        // Focus went elsewhere without the field noticing (the window lost
        // it, say): close without committing.
        *edit = None;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_positions_share_the_fader_scale() {
        let zones = meter_zones();
        assert!(zones.hot < zones.over && zones.over < 1.0, "{zones:?}");
        assert_eq!(zones.over, db_to_fader(0.0));

        let silent = levels(MeterState::default());
        assert_eq!((silent.left, silent.right, silent.peak), (0.0, 0.0, None));

        let mut meter = MeterState::default();
        meter.feed(1.0, 0.5, 0.01);
        let loud = levels(meter);
        assert_eq!(loud.left, zones.over, "full scale sits where the over zone begins");
        assert!(loud.right < loud.left);
        assert_eq!(loud.peak, Some(zones.over));
    }
}
