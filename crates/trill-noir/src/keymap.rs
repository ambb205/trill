//! The Key Map: what plays where on a keyboard, as boxes over the keys
//! (across) and how hard they are struck (up), with the keys under it.
//!
//! It is how an instrument made of recordings is seen at a glance: where
//! its recordings lie, how many layers of loudness a key has, where it
//! ends, which keys choose a way of playing instead of sounding. The kit
//! does not know about instruments: it is handed boxes.

use crate::paint;
use crate::text;
use crate::theme::{Theme, fade};
use egui::{Align2, Color32, Painter, Pos2, Rect, pos2, vec2};

/// Height of the strip of keys under the boxes.
const KEYS: f32 = 9.0;
/// Height of the line of octave names under the keys.
const NAMES: f32 = 12.0;
/// The least room an octave's name wants.
const NAME_WIDTH: f32 = 22.0;
/// How strongly a box is filled with the accent, and one that stands back.
const FILL: f32 = 0.34;
const FILL_BACK: f32 = 0.14;

/// One box of the map: the keys it covers and how hard they are struck
/// for it, both ends included.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeyBox {
    /// The lowest and highest key, 0..=127.
    pub keys: (u8, u8),
    /// The softest and hardest strike, 0..=127.
    pub levels: (u8, u8),
    /// It stands back: something that is not the main thing (a sound for
    /// a key coming up).
    pub back: bool,
}

/// A map to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyMap<'a> {
    pub boxes: &'a [KeyBox],
    /// The lowest and highest key shown. Widened to whole octaves.
    pub range: (u8, u8),
    /// Keys that are marked on the strip: the ones that switch.
    pub marks: &'a [u8],
    pub accent: Color32,
}

impl KeyMap<'_> {
    /// The keys shown: `range` widened to whole octaves, from a C to a B.
    pub fn shown(&self) -> (u8, u8) {
        let low = self.range.0.min(self.range.1).min(127) / 12 * 12;
        let high = (self.range.1.max(self.range.0).min(127) / 12 * 12 + 11).min(127);
        (low, high)
    }

    /// Where the boxes go in `rect`: above the keys and their names.
    pub fn boxes_area(&self, rect: Rect) -> Rect {
        rect.with_max_y((rect.bottom() - KEYS - NAMES).max(rect.top())).shrink(1.0)
    }

    /// The left and right edge of `key` in `area`.
    fn key_span(&self, area: Rect, key: u8) -> (f32, f32) {
        let (low, high) = self.shown();
        let count = f32::from(high - low) + 1.0;
        let at = |key: f32| area.left() + area.width() * (key - f32::from(low)) / count;
        (at(f32::from(key)), at(f32::from(key) + 1.0))
    }

    /// The key and the strike under `pointer`, if it is over the boxes.
    pub fn at(&self, rect: Rect, pointer: Pos2) -> Option<(u8, u8)> {
        let area = self.boxes_area(rect);
        if !area.contains(pointer) {
            return None;
        }
        let (low, high) = self.shown();
        let count = f32::from(high - low) + 1.0;
        let key = f32::from(low) + ((pointer.x - area.left()) / area.width() * count).floor();
        let level = ((area.bottom() - pointer.y) / area.height() * 127.0).round();
        Some((key.clamp(f32::from(low), f32::from(high)) as u8, level.clamp(1.0, 127.0) as u8))
    }
}

/// Whether `key` is a black key.
fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// Paints `map` in `rect`: a well with the boxes, the strip of keys under
/// it, and the names of the octaves under that.
pub fn key_map(painter: &Painter, theme: &Theme, rect: Rect, map: &KeyMap) {
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let well = rect.with_max_y((rect.bottom() - KEYS - NAMES).max(rect.top()));
    painter.rect_filled(well, 0.0, theme.bg);
    let area = map.boxes_area(rect);
    let (low, high) = map.shown();

    // The octaves, as faint lines up the well.
    for key in (low..=high).filter(|key| key % 12 == 0 && *key != low) {
        paint::vline(&painter, map.key_span(area, key).0.round(), area.y_range(), theme.grid_fine);
    }
    // The boxes, each a pixel apart from its neighbours.
    let hair = paint::hair(&painter);
    for item in map.boxes {
        let (first, last) = (item.keys.0.max(low), item.keys.1.min(high));
        if first > last {
            continue;
        }
        let (left, right) = (map.key_span(area, first).0, map.key_span(area, last).1);
        let up = |level: u8| area.bottom() - area.height() * f32::from(level.min(127)) / 127.0;
        // A strike of nothing is no strike: a box from 0 or from 1 starts at the bottom.
        let bottom = if item.levels.0 <= 1 { area.bottom() } else { up(item.levels.0 - 1) };
        let top = up(item.levels.1);
        let drawn = Rect::from_min_max(
            pos2(paint::snap(&painter, left) + hair, paint::snap(&painter, top) + hair),
            pos2(paint::snap(&painter, right), paint::snap(&painter, bottom)),
        );
        if drawn.width() > 0.0 && drawn.height() > 0.0 {
            painter.rect_filled(drawn, 0.0, fade(map.accent, if item.back { FILL_BACK } else { FILL }));
        }
    }
    paint::frame(&painter, well, theme.hairline);

    // The keys.
    let strip = Rect::from_min_max(pos2(area.left(), well.bottom()), pos2(area.right(), well.bottom() + KEYS));
    painter.rect_filled(strip, 0.0, theme.key_white);
    for key in low..=high {
        let (left, right) = map.key_span(strip, key);
        let rect = Rect::from_min_max(pos2(left, strip.top()), pos2(right, strip.bottom()));
        if map.marks.contains(&key) {
            painter.rect_filled(rect, 0.0, map.accent);
        } else if is_black(key) {
            painter.rect_filled(rect.with_max_y(strip.top() + KEYS * 0.62), 0.0, theme.key_black);
        }
        if key % 12 == 0 && key != low {
            paint::vline(&painter, left.round(), strip.y_range(), theme.key_ink);
        }
    }

    // The octaves by name, as many as there is room for.
    let octave = map.key_span(strip, low.saturating_add(12).min(127)).0 - strip.left();
    let every = (NAME_WIDTH / octave.max(1.0)).ceil().max(1.0) as u8;
    for (index, key) in (low..=high).filter(|key| key % 12 == 0).enumerate() {
        if !(index as u8).is_multiple_of(every) {
            continue;
        }
        let left = map.key_span(strip, key).0;
        let room = Rect::from_min_size(
            pos2(left + 1.0, strip.bottom()),
            vec2(NAME_WIDTH.max(octave * f32::from(every)), NAMES),
        );
        // Middle C is C4.
        let name = format!("C{}", i32::from(key / 12) - 1);
        text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, &name, theme.ink_faint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::with_ui;

    fn map<'a>(boxes: &'a [KeyBox], range: (u8, u8)) -> KeyMap<'a> {
        KeyMap { boxes, range, marks: &[], accent: Color32::WHITE }
    }

    #[test]
    fn the_keys_shown_are_whole_octaves_and_the_pointer_reads_a_key_and_a_strike() {
        let boxes = [KeyBox { keys: (40, 44), levels: (1, 127), back: false }];
        let piano = map(&boxes, (21, 108));
        assert_eq!(piano.shown(), (12, 119), "from the C under the lowest key to the B over the highest");
        assert_eq!(map(&boxes, (60, 60)).shown(), (60, 71));
        assert_eq!(map(&boxes, (127, 0)).shown(), (0, 127), "an upside-down range is put right");

        let rect = Rect::from_min_size(pos2(10.0, 10.0), vec2(218.0, 100.0));
        let area = piano.boxes_area(rect);
        assert_eq!((area.left(), area.width()), (11.0, 216.0), "two pixels a key over nine octaves");
        // The first key shown is at the left, soft strikes at the bottom.
        assert_eq!(piano.at(rect, pos2(area.left() + 0.5, area.bottom() - 0.5)), Some((12, 1)));
        assert_eq!(piano.at(rect, pos2(area.right() - 0.5, area.top() + 0.1)), Some((119, 127)));
        let middle_c = area.left() + 2.0 * (60.0 - 12.0) + 1.0;
        assert_eq!(piano.at(rect, pos2(middle_c, area.center().y)).map(|(key, _)| key), Some(60));
        assert_eq!(piano.at(rect, pos2(rect.left() + 5.0, rect.bottom() - 2.0)), None, "the keys under it are not it");
        assert_eq!(piano.at(rect, pos2(0.0, 0.0)), None);
    }

    #[test]
    fn a_map_is_painted_whatever_it_is_handed() {
        with_ui(|ui, theme| {
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 90.0));
            let boxes = [
                KeyBox { keys: (0, 127), levels: (0, 127), back: true },
                KeyBox { keys: (60, 59), levels: (90, 20), back: false },
                KeyBox { keys: (36, 36), levels: (64, 127), back: false },
            ];
            key_map(
                ui.painter(),
                theme,
                rect,
                &KeyMap { boxes: &boxes, range: (36, 84), marks: &[24, 25], accent: theme.signal },
            );
            key_map(ui.painter(), theme, rect, &map(&[], (60, 60)));
            // No room at all is no reason to fall over.
            key_map(ui.painter(), theme, Rect::from_min_size(pos2(0.0, 0.0), vec2(3.0, 2.0)), &map(&boxes, (0, 127)));
        });
        assert!(is_black(61) && is_black(70) && !is_black(60) && !is_black(71));
    }
}
