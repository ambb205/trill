//! The mark: a T whose right arm is a second bar raised a step, like two
//! notes a step apart in a piano roll.
//!
//! It is drawn from the same three rectangles everywhere (the top bar, the
//! window icon, the stage), on a 14 x 11 grid, so a size that is a multiple
//! of the grid lands every edge on a whole pixel.

use egui::{Color32, Painter, Pos2, Rect, pos2, vec2};

pub const WIDTH: u32 = 14;
pub const HEIGHT: u32 = 11;
/// The mark's own colours. They do not follow the theme.
pub const VIOLET: Color32 = Color32::from_rgb(0xA8, 0x7F, 0xD1);
pub const TEAL: Color32 = Color32::from_rgb(0x4E, 0xC9, 0xB0);
/// `(x, y, width, height, colour)` in grid units, back to front.
pub const BARS: [(u32, u32, u32, u32, Color32); 3] = [(0, 2, 9, 3, VIOLET), (5, 2, 4, 9, VIOLET), (7, 0, 7, 3, TEAL)];

/// Paints the mark with its top-left corner at `origin`, `unit` points per
/// grid unit, and returns the rectangle it covers.
pub fn paint(painter: &Painter, origin: Pos2, unit: f32) -> Rect {
    for (x, y, width, height, color) in BARS {
        let min = origin + vec2(x as f32, y as f32) * unit;
        painter.rect_filled(Rect::from_min_size(min, vec2(width as f32, height as f32) * unit), 0.0, color);
    }
    Rect::from_min_size(origin, vec2(WIDTH as f32, HEIGHT as f32) * unit)
}

/// The mark with its left edge at `left`, centered on `center_y`: for a
/// row of that height.
pub fn paint_centered(painter: &Painter, left: f32, center_y: f32, unit: f32) -> Rect {
    paint(painter, pos2(left, (center_y - HEIGHT as f32 * unit / 2.0).round()), unit)
}
