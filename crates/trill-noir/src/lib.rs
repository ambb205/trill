//! Instrument Noir for egui: the DAW's design tokens and widget kit.
//!
//! Ported from loopycat's `noir` crate (egui 0.36) to egui 0.37, trimmed to
//! what this app uses and extended where it needed more. The look is the
//! Instrument Noir kit's: near-black surfaces, ink text, one-pixel
//! hairlines, zero rounding, flat washes, caps labels and tabular figures
//! with a real minus sign. `core/tokens.json` and `core/widgets.md` of the
//! kit are the reference for the palette and for how the widgets behave.
//!
//! What changed in the port:
//!
//! * Tokens are a [`Theme`] value handed to every widget instead of
//!   constants, so the palette can be swapped ([`Theme::graphite`]).
//! * Text goes through [`text`], which adds tabular figures (egui cannot
//!   switch on Inter's `tnum`) on top of letter spacing.
//! * Widgets take an exact rectangle and an id rather than allocating from
//!   a layout, because the app places everything by hand, and they report
//!   what the user did instead of editing a value in place: every edit in
//!   the app goes through its session.
//! * The Bar follows the app's gestures: a double-click restores the
//!   default, and Ctrl as well as Shift makes a drag fine. The [`Fader`] is
//!   the same control stood on end, for mixer strips.
//! * The knob, keyboard, piano roll, step lane, scope, spectrum and
//!   waveform widgets were left behind; the app draws its own piano roll
//!   and response displays (on [`Plot`]) from its project snapshot.
//!
//! Nothing here knows about audio or projects.

mod bar;
mod button;
mod chip;
mod fader;
mod field;
pub mod format;
mod icon;
mod menu;
mod meter;
pub mod paint;
mod plot;
mod style;
#[cfg(test)]
mod testing;
pub mod text;
mod theme;

pub use bar::{Bar, BarOutput, bar};
pub use button::{Variant, bare_icon_button, button, icon_button, tab, tab_width};
pub use chip::{Chip, chip, chip_row, chip_row_width};
pub use fader::{Fader, fader};
pub use field::{NumberField, NumberOutput, number_field, text_field};
pub use icon::Icon;
pub use menu::{MenuItem, dropdown, dropdown_width, menu_heading, menu_separator, menu_title};
pub use meter::{Levels, Zones, mini_meter, stereo_meter};
pub use plot::Plot;
pub use style::install;
pub use theme::{Metrics, TRACK_HUES, TextStyle, Theme, Type, fade};
