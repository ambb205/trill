//! What the keyboard shows: its switch in the top bar, and while it is on a
//! strip between the song and the detail panel that says what the keys
//! play, in which octave and how hard, and draws the keys themselves with
//! the letter that plays each.
//!
//! The strip is where the player's eye goes for what the top bar has no
//! room for. Its picture of the keys can be folded away by someone who
//! knows them; the readouts stay.

use super::layout::{self, NOTES, Role};
use super::{Keyboard, range_name};
use crate::ui::colors;
use crate::ui::shortcuts::{self, Command};
use crate::ui::state::Cx;
use eframe::egui::{Align2, Color32, Id, Painter, Rect, Response, Sense, Ui, pos2, vec2};
use trill_core::pitch::pitch_name;
use trill_noir::{Chip, Icon, TextStyle, Theme, fade, paint, text};

/// Height of the strip with the picture of the keys, and without.
const TALL: f32 = 46.0;
const SHORT: f32 = 24.0;
const EDGE: f32 = 10.0;
/// Space between the strip's readouts.
const GAP: f32 = 18.0;
/// Side of a key cap: the chips that stand for `Z`, `X`, `C` and `V`.
const CAP: f32 = 18.0;
/// Width of the number between two key caps.
const VALUE: f32 = 36.0;
const STEPPER: f32 = CAP + VALUE + CAP;
/// The least and the most room the name of what plays is given: the most
/// is what the longest reason for silence needs.
const PLAYS_MIN: f32 = 150.0;
const PLAYS_MAX: f32 = 280.0;
/// The rule for letters and shortcuts, as the strip states it.
const RULE: &str = "Shortcuts on these letters: hold Alt";
const HELD_WIDTH: f32 = 112.0;
const WHITE_KEY: f32 = 24.0;
const BLACK_KEY: f32 = 16.0;
/// How far down a white key a black one reaches.
const BLACK_KEY_REACH: f32 = 0.58;

/// The switch in the top bar.
pub fn switch(ui: &Ui, cx: &mut Cx, rect: Rect) -> Response {
    let theme = cx.theme;
    let on = cx.keyboard.is_on();
    let color = if on { theme.ink } else { theme.ink_dim };
    let id = Id::unique("computer-keyboard-switch");
    let response = trill_noir::icon_button(ui, theme, rect, id, Icon::Keyboard, on, color);
    let keys = shortcuts::keys(Command::ToggleKeyboard);
    cx.hint(&response, format!("Play the selected track from the computer keyboard \u{00B7} {keys}"));
    if response.clicked() {
        cx.keyboard.toggle();
    }
    response
}

/// How tall the strip is, when there is one: while the keyboard is on.
pub fn height(cx: &Cx) -> Option<f32> {
    cx.keyboard.is_on().then(|| if cx.keyboard.shows_key_map() { TALL } else { SHORT })
}

/// The strip.
pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let area = ui.max_rect();
    ui.expand_to_include_rect(area);
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme.panel);
    paint::hline(&painter, area.x_range(), area.top(), theme.hairline);

    let tall = cx.keyboard.shows_key_map();
    let id = ui.scope_id().with("computer-keyboard");

    // The fold comes first, where a disclosure triangle is looked for.
    let fold = Rect::from_center_size(pos2(area.left() + EDGE - 4.0 + CAP / 2.0, area.center().y), vec2(CAP, CAP));
    let (icon, name, about) = if tall {
        (Icon::CaretDown, "Hide the keys", "Fold the picture of the keys away")
    } else {
        (Icon::CaretRight, "Show the keys", "Show which key plays what")
    };
    let response = trill_noir::bare_icon_button(ui, theme, fold, id.with("fold"), icon, name);
    cx.hint(&response, about);
    if response.clicked() {
        cx.keyboard.set_key_map(!tall);
    }

    // What fits: the readouts always, the sustain key and the picture of
    // the keys (or, folded, the names of the held notes) as the strip is
    // wide enough for them. What plays takes the room that is left, up to
    // what its longest line needs.
    let mut cells = Cells { painter: &painter, theme, area, x: fold.right() + 6.0, tall, first: true };
    let room = area.right() - EDGE - cells.x;
    let sustain_cap = text::width(&painter, theme.text.micro, "Tab").ceil() + 12.0;
    let piano_width = white_keys() as f32 * WHITE_KEY;
    let steppers = cells.width("Octave", STEPPER) + cells.width("Velocity", STEPPER) + 2.0 * GAP;
    let sustain = GAP + cells.width("Sustain", sustain_cap);
    let held = GAP + cells.width("Held", HELD_WIDTH);
    let least = cells.width("Plays", PLAYS_MIN) + steppers;
    let with_sustain = room >= least + sustain;
    let with_piano = tall && room >= least + sustain + GAP + piano_width;
    let with_held = !with_piano && room >= least + sustain + held;
    let taken = steppers
        + if with_sustain { sustain } else { 0.0 }
        + if with_piano { GAP + piano_width } else { 0.0 }
        + if with_held { held } else { 0.0 };
    let plays_width = (room - taken - cells.width("Plays", 0.0)).clamp(60.0, PLAYS_MAX);

    plays(ui, cx, &mut cells, id, plays_width);
    octave(ui, cx, &mut cells, id);
    velocity(ui, cx, &mut cells, id);
    if with_sustain {
        let (value, whole) = cells.claim("Sustain", sustain_cap);
        key_cap(&painter, theme, value.with_max_x(value.left() + sustain_cap), "Tab", cx.keyboard.sustaining());
        let response = ui.interact(whole, id.with("sustain"), Sense::hover());
        cx.hint(&response, "Hold Tab and what is played rings on until Tab comes up");
    }
    if with_piano {
        let top = area.top() + 6.0;
        let keys = Rect::from_min_size(pos2(cells.x + GAP, top), vec2(piano_width, area.bottom() - 6.0 - top));
        piano_keys(ui, cx, keys, id);
        cells.x = keys.right();
    } else if with_held {
        let (value, whole) = cells.claim("Held", HELD_WIDTH);
        let names: Vec<String> = cx.keyboard.pitches().into_iter().map(pitch_name).collect();
        let (shown, color) =
            if names.is_empty() { ("\u{2013}".to_owned(), theme.ink_faint) } else { (names.join(" "), theme.ink) };
        text::paint(&painter, value, Align2::LEFT_CENTER, theme.text.value, &shown, color);
        let response = ui.interact(whole, id.with("held"), Sense::hover());
        cx.hint(&response, "The notes the keyboard is holding");
    }

    // The rule for the letters, where there is room left to say it.
    let rest = Rect::from_min_max(pos2(cells.x + GAP, area.top()), pos2(area.right() - EDGE, area.bottom()));
    if rest.width() >= text::width(&painter, theme.text.micro, RULE).ceil() + 2.0 {
        text::paint(&painter, rest, Align2::RIGHT_CENTER, theme.text.micro, RULE, theme.ink_faint);
    }
}

/// Lays the strip's readouts out from the left: a micro label over its
/// value in the tall strip, before it in the short one.
struct Cells<'a> {
    painter: &'a Painter,
    theme: &'a Theme,
    area: Rect,
    x: f32,
    tall: bool,
    /// Nothing has been laid out yet: the next cell needs no gap before it.
    first: bool,
}

impl Cells<'_> {
    /// The width of the label as the strip sets it.
    fn label_width(&self, label: &str) -> f32 {
        text::width(self.painter, self.theme.text.micro, label).ceil()
    }

    /// The width a cell takes for a value `value` wide: no narrower than
    /// its label where that is above the value, both side by side where
    /// it is before it.
    fn width(&self, label: &str, value: f32) -> f32 {
        if self.tall { value.max(self.label_width(label)) } else { self.label_width(label) + 6.0 + value }
    }

    /// The place of a value `width` wide under (or after) `label`, and the
    /// place of the two together, for the hint.
    fn claim(&mut self, label: &str, width: f32) -> (Rect, Rect) {
        let (theme, area) = (self.theme, self.area);
        if !std::mem::take(&mut self.first) {
            self.x += GAP;
        }
        let left = self.x;
        let whole = Rect::from_min_max(pos2(left, area.top()), pos2(left + self.width(label, width), area.bottom()));
        let value = if self.tall {
            let title = Rect::from_min_size(pos2(left, area.top() + 6.0), vec2(whole.width(), 12.0));
            text::paint(self.painter, title, Align2::LEFT_CENTER, theme.text.micro, label, theme.ink_faint);
            Rect::from_min_size(pos2(left, area.top() + 21.0), vec2(whole.width(), CAP))
        } else {
            text::paint(self.painter, whole, Align2::LEFT_CENTER, theme.text.micro, label, theme.ink_faint);
            Rect::from_min_size(pos2(whole.right() - width, area.center().y - CAP / 2.0), vec2(width, CAP))
        };
        self.x = whole.right();
        (value, whole)
    }
}

/// What the keys play: the track and its instrument, or why nothing sounds.
fn plays(ui: &Ui, cx: &mut Cx, cells: &mut Cells, id: Id, width: f32) {
    let theme = cx.theme;
    let (value, whole) = cells.claim("Plays", width);
    let playing = cx.keyboard.playing().clone();
    let style = TextStyle { size: 12.0, ..theme.text.body };
    match &playing.silence {
        Some(silence) => {
            let left = paint::hollow_dot(cells.painter, value.left_center(), theme.metric.dot, theme.warn) + 6.0;
            let mut room = value.with_min_x(left);
            // A long name is cut short before what is the matter is.
            let (name, matter) = silence.parts();
            if let Some(name) = name {
                let matter_width = text::width(cells.painter, style, &matter).ceil();
                let name_room = room.with_max_x((room.right() - matter_width).max(room.left() + 40.0));
                room.min.x += text::paint(cells.painter, name_room, Align2::LEFT_CENTER, style, name, theme.warn);
            }
            text::paint(cells.painter, room, Align2::LEFT_CENTER, style, &matter, theme.warn);
        }
        None => {
            let stored = playing.tracks.first().and_then(|track| cx.snap.track(*track)).map_or(0, |track| track.color);
            // Armed tracks are marked as their arm control is: these are
            // played whatever is selected.
            let color = if playing.armed { theme.signal } else { colors::track_color(theme, stored) };
            let left = paint::dot(cells.painter, value.left_center(), theme.metric.dot, color) + 6.0;
            let room = value.with_min_x(left);
            let used = text::paint(cells.painter, room, Align2::LEFT_CENTER, style, &playing.name, theme.ink);
            let rest = room.with_min_x(room.left() + used + 8.0);
            text::paint(cells.painter, rest, Align2::LEFT_CENTER, style, &playing.instrument, theme.ink_dim);
        }
    }
    let response = ui.interact(whole, id.with("plays"), Sense::hover());
    let about = match &playing.silence {
        Some(silence) => match silence.advice() {
            Some(advice) => format!("{} \u{00B7} {advice}", silence.text()),
            None => silence.text(),
        },
        None if playing.armed => {
            format!("The keys play the armed tracks ({}), as a MIDI keyboard does \u{00B7} disarm to play the selected one", playing.name)
        }
        None => {
            format!("The keys play {} on {} \u{00B7} select another track to play it", playing.instrument, playing.name)
        }
    };
    cx.hint(&response, about);
}

/// A number between the two keys that step it, which can be clicked as
/// well as pressed. Returns which of the two was clicked, if one was.
fn stepper(ui: &Ui, cx: &mut Cx, value: Rect, id: Id, shown: &str, keys: [(&str, &str); 2]) -> Option<usize> {
    let theme = cx.theme;
    let mut clicked = None;
    for (index, (label, name)) in keys.into_iter().enumerate() {
        let left = if index == 0 { value.left() } else { value.left() + CAP + VALUE };
        let cap = Rect::from_min_size(pos2(left, value.top()), vec2(CAP, CAP));
        let chip = Chip::action(label).named(name);
        if trill_noir::chip(ui, theme, cap, id.with(index), &chip).clicked() {
            clicked = Some(index);
        }
    }
    let number = Rect::from_min_size(pos2(value.left() + CAP, value.top()), vec2(VALUE, CAP));
    text::paint(ui.painter(), number, Align2::CENTER_CENTER, theme.text.value, shown, theme.ink);
    clicked
}

fn octave(ui: &Ui, cx: &mut Cx, cells: &mut Cells, id: Id) {
    let (value, whole) = cells.claim("Octave", STEPPER);
    let shown = pitch_name(cx.keyboard.lowest());
    let keys = [(cap_of(Role::Octave(-1)), "Octave down"), (cap_of(Role::Octave(1)), "Octave up")];
    let clicked = stepper(ui, cx, value, id.with("octave"), &shown, keys);
    if let Some(index) = clicked {
        cx.keyboard.shift_octave(if index == 0 { -1 } else { 1 });
    }
    let response = ui.interact(whole, id.with("octave-cell"), Sense::hover());
    let range = range_name(cx.keyboard);
    cx.hint(&response, format!("The keys play {range} \u{00B7} Z and X move them an octave down and up"));
}

fn velocity(ui: &Ui, cx: &mut Cx, cells: &mut Cells, id: Id) {
    let (value, whole) = cells.claim("Velocity", STEPPER);
    let shown = cx.keyboard.velocity().to_string();
    let keys = [(cap_of(Role::Velocity(-1)), "Softer"), (cap_of(Role::Velocity(1)), "Louder")];
    let clicked = stepper(ui, cx, value, id.with("velocity"), &shown, keys);
    if let Some(index) = clicked {
        cx.keyboard.step_velocity(if index == 0 { -1 } else { 1 });
    }
    let response = ui.interact(whole, id.with("velocity-cell"), Sense::hover());
    cx.hint(&response, "How hard the keys strike, 1 to 127 \u{00B7} C plays softer and V louder");
}

/// The letter on the key with `role`.
fn cap_of(role: Role) -> &'static str {
    layout::CONTROLS.iter().find(|control| control.role == role).map_or("", |control| control.label)
}

/// A key drawn as the chips are, for one that is only shown: lit while it
/// is held down.
fn key_cap(painter: &Painter, theme: &Theme, rect: Rect, label: &str, lit: bool) {
    if lit {
        painter.rect_filled(rect, 0.0, theme.selected);
    }
    paint::frame(painter, rect, theme.hairline);
    let color = if lit { theme.ink } else { theme.ink_dim };
    text::paint(painter, rect.shrink(1.0), Align2::CENTER_CENTER, theme.text.micro, label, color);
}

/// How many of the keys are white ones.
fn white_keys() -> usize {
    NOTES.iter().filter(|key| matches!(key.role, Role::Note(semitone) if !layout::is_black(semitone))).count()
}

/// Where each key of the picture is: white keys side by side, the black
/// ones over the joins between them. In the order of [`NOTES`].
fn key_rects(keys: Rect) -> Vec<(u8, Rect, bool)> {
    let mut rects = Vec::with_capacity(NOTES.len());
    let mut whites = 0.0;
    for semitone in 0..NOTES.len() as u8 {
        if layout::is_black(semitone) {
            let center = keys.left() + whites * WHITE_KEY;
            let size = vec2(BLACK_KEY, (keys.height() * BLACK_KEY_REACH).round());
            rects.push((semitone, Rect::from_min_size(pos2(center - BLACK_KEY / 2.0, keys.top()), size), true));
        } else {
            let left = keys.left() + whites * WHITE_KEY;
            rects.push((semitone, Rect::from_min_size(pos2(left, keys.top()), vec2(WHITE_KEY, keys.height())), false));
            whites += 1.0;
        }
    }
    rects
}

/// The key of the picture at `position`: a black key wins over the white
/// ones it lies across.
fn key_at(keys: Rect, position: eframe::egui::Pos2) -> Option<u8> {
    let rects = key_rects(keys);
    let hit = |black: bool| {
        rects.iter().find(|(_, rect, is_black)| *is_black == black && rect.contains(position)).map(|(key, ..)| *key)
    };
    hit(true).or_else(|| hit(false))
}

/// The keys as a piano, each with the letter that plays it; the ones
/// sounding are lit in the track's color. They can be played with the
/// mouse too: press one, or slide along them.
fn piano_keys(ui: &Ui, cx: &mut Cx, keys: Rect, id: Id) {
    let theme = cx.theme;
    let response = ui.interact(keys, id.with("keys"), Sense::click_and_drag());
    response
        .widget_info(|| eframe::egui::WidgetInfo::labeled(eframe::egui::Role::Button, true, "Computer keyboard keys"));
    let playing = cx.keyboard.playing().clone();
    let lowest = cx.keyboard.lowest();
    // The mouse plays the first of the tracks the keys play.
    let first = playing.tracks.first().copied();
    let stored = first.and_then(|track| cx.snap.track(track)).map_or(0, |track| track.color);
    let hue = colors::track_color(theme, stored);
    let hovered = response.hover_pos().and_then(|position| key_at(keys, position));
    let auditioned = cx.state.audition.filter(|audition| Some(audition.track) == first);

    let painter = ui.painter_at(keys);
    let look = |semitone: u8| {
        let pitch = lowest as u16 + semitone as u16;
        let playable = pitch <= 127;
        let pitch = pitch.min(127) as u8;
        let lit = playable
            && (playing.tracks.iter().any(|track| cx.keyboard.is_sounding(*track, pitch))
                || auditioned.is_some_and(|audition| audition.pitch == pitch));
        (playable, lit)
    };
    let rects = key_rects(keys);
    painter.rect_filled(keys, 0.0, theme.key_white);
    for black in [false, true] {
        for (semitone, rect, _) in rects.iter().filter(|(.., is_black)| *is_black == black) {
            let (playable, lit) = look(*semitone);
            let fill = match (lit, black) {
                (true, _) => Some(hue),
                (false, true) => Some(theme.key_black),
                (false, false) => None,
            };
            if let Some(fill) = fill {
                painter.rect_filled(*rect, 0.0, fill);
            }
            if hovered == Some(*semitone) && !lit {
                painter.rect_filled(*rect, 0.0, fade(if black { theme.key_white } else { theme.key_black }, 0.16));
            }
            if !black {
                // The gap between two keys of a real keyboard.
                paint::vline(&painter, rect.left(), rect.y_range(), fade(theme.key_ink, 0.35));
            }
            let ink = key_ink(theme, black, lit);
            let ink = if playable { ink } else { fade(ink, 0.35) };
            let letter = layout::label_for(*semitone).unwrap_or_default();
            let style = TextStyle { tracking: 0.0, ..theme.text.micro };
            // On a white key the letter sits below the black keys.
            let place = if black { *rect } else { rect.with_min_y(rect.bottom() - 14.0) };
            text::paint(&painter, place, Align2::CENTER_CENTER, style, letter, ink);
        }
    }

    if let Some(semitone) = hovered {
        let pitch = lowest as u16 + semitone as u16;
        let letter = layout::label_for(semitone).unwrap_or_default();
        if pitch <= 127 {
            cx.state.hint.set(format!("{letter} plays {} \u{00B7} or press it here", pitch_name(pitch as u8)));
        }
    }
    // Pressed or slid along with the mouse, as the piano roll's keys are.
    if response.is_pointer_button_down_on()
        && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
        && let Some(semitone) = key_at(keys, pointer)
        && let Some(track) = first
        && lowest as u16 + semitone as u16 <= 127
    {
        let pitch = lowest + semitone;
        if auditioned.is_none_or(|audition| audition.pitch != pitch) {
            let velocity = cx.keyboard.velocity();
            cx.audition(track, pitch, velocity, None);
        }
    }
}

/// The ink of the letter on a key.
fn key_ink(theme: &Theme, black: bool, lit: bool) -> Color32 {
    match (lit, black) {
        (true, _) => theme.on_accent,
        (false, true) => theme.key_white,
        (false, false) => theme.key_ink,
    }
}

/// The letter of the computer key that plays `pitch`, for the piano
/// roll's own keyboard, and the ink it is written in there.
pub fn letter_on(
    keyboard: &Keyboard,
    theme: &Theme,
    pitch: u8,
    black: bool,
    lit: bool,
) -> Option<(&'static str, Color32)> {
    keyboard.letter(pitch).map(|letter| (letter, key_ink(theme, black, lit)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_is_a_piano_of_eleven_white_keys() {
        assert_eq!(white_keys(), 11);
        let keys = Rect::from_min_size(pos2(100.0, 10.0), vec2(11.0 * WHITE_KEY, 34.0));
        let rects = key_rects(keys);
        assert_eq!(rects.len(), NOTES.len());
        let whites: Vec<&(u8, Rect, bool)> = rects.iter().filter(|(.., black)| !black).collect();
        assert_eq!(whites.first().unwrap().1.left(), keys.left());
        assert_eq!(whites.last().unwrap().1.right(), keys.right(), "the white keys fill the picture");
        for pair in whites.windows(2) {
            assert_eq!(pair[0].1.right(), pair[1].1.left(), "side by side");
        }
        // C sharp lies over the join of C and D, and reaches part way down.
        let (c, c_sharp, d) = (rects[0].1, rects[1].1, rects[2].1);
        assert_eq!(c_sharp.center().x, c.right());
        assert_eq!(c.right(), d.left());
        assert!(c_sharp.bottom() < c.bottom() - 10.0, "the letter of a white key shows below the black ones");
    }

    #[test]
    fn a_point_on_the_picture_is_the_key_under_it() {
        let keys = Rect::from_min_size(pos2(100.0, 10.0), vec2(11.0 * WHITE_KEY, 34.0));
        // Low on the first white key: C. High on the join: C sharp.
        assert_eq!(key_at(keys, pos2(104.0, 40.0)), Some(0));
        assert_eq!(key_at(keys, pos2(100.0 + WHITE_KEY, 14.0)), Some(1), "a black key wins where it lies");
        assert_eq!(key_at(keys, pos2(101.0 + WHITE_KEY, 40.0)), Some(2), "below it, the white key to the right");
        // E and F have no black key between them.
        assert_eq!(key_at(keys, pos2(100.0 + 3.0 * WHITE_KEY - 1.0, 14.0)), Some(4));
        assert_eq!(key_at(keys, pos2(100.0 + 3.0 * WHITE_KEY + 1.0, 14.0)), Some(5));
        assert_eq!(key_at(keys, pos2(keys.right() - 2.0, 40.0)), Some(17), "the last key is the F above");
        assert_eq!(key_at(keys, pos2(90.0, 20.0)), None);
    }
}
