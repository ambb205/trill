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
//! * The knob, keyboard, piano roll, step lane, scope and spectrum widgets
//!   were left behind; the app draws its own piano roll and response
//!   displays (on [`Plot`]) from its project snapshot.
//! * The [`waveform`] was written here: it draws pixel columns it is given
//!   into a mesh the caller owns, so that many can be painted as one.
//! * So was [`prose`]: running text that wraps, mixes faces (Inter SemiBold
//!   for strong text was added for it) and can be selected and copied.
//! * The [`mark`] is here so that the window and the stage draw the same
//!   one, and [`Theme::stored_hue`] so that both show a track in the same
//!   hue.
//!
//! Nothing here knows about audio or projects.

mod automation;
mod bar;
mod button;
mod chip;
mod fader;
mod field;
pub mod format;
mod icon;
pub mod mark;
mod menu;
mod meter;
pub mod paint;
mod plot;
pub mod prose;
mod style;
#[cfg(test)]
mod testing;
pub mod text;
mod theme;
mod waveform;

pub use automation::{Automation, automation_mark, automation_zone};
pub use bar::{Bar, BarOutput, bar};
pub use button::{Variant, bare_icon_button, button, button_width, icon_button, tab, tab_width};
pub use chip::{Chip, chip, chip_row, chip_row_width};
pub use fader::{Fader, fader};
pub use field::{NumberField, NumberOutput, number_field, text_field};
pub use icon::{Icon, paint_icon};
pub use menu::{MenuItem, dropdown, dropdown_width, menu_heading, menu_separator, menu_title};
pub use meter::{Levels, Zones, mini_meter, stereo_meter};
pub use plot::Plot;
pub use style::install;
pub use theme::{Metrics, POSITIONAL, TRACK_HUES, TextStyle, Theme, Type, fade};
pub use waveform::{Column, waveform, waveform_columns};
