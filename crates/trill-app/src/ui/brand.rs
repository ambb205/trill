//! The mark: a T whose right arm is a second bar raised a step, like two
//! notes a step apart in a piano roll.
//!
//! It is drawn from the same three rectangles everywhere (the top bar, the
//! window icon), on a 14 x 11 grid, so a size that is a multiple of the
//! grid lands every edge on a whole pixel. `assets/trill-mark.svg` is the
//! same drawing for use outside the program.

use eframe::egui::{Color32, IconData, Painter, Pos2, Rect, pos2, vec2};

pub const WIDTH: u32 = 14;
pub const HEIGHT: u32 = 11;
/// The mark's own colours. They do not follow the theme.
const VIOLET: Color32 = Color32::from_rgb(0xA8, 0x7F, 0xD1);
const TEAL: Color32 = Color32::from_rgb(0x4E, 0xC9, 0xB0);
/// `(x, y, width, height, colour)` in grid units, back to front.
const BARS: [(u32, u32, u32, u32, Color32); 3] = [(0, 2, 9, 3, VIOLET), (5, 2, 4, 9, VIOLET), (7, 0, 7, 3, TEAL)];
/// Side of the square the mark sits in as an icon, in grid units.
const ICON_GRID: u32 = 16;

/// Paints the mark with its top-left corner at `origin`, `unit` points per
/// grid unit, and returns the rectangle it covers.
pub fn paint(painter: &Painter, origin: Pos2, unit: f32) -> Rect {
    for (x, y, width, height, color) in BARS {
        let min = origin + vec2(x as f32, y as f32) * unit;
        painter.rect_filled(Rect::from_min_size(min, vec2(width as f32, height as f32) * unit), 0.0, color);
    }
    Rect::from_min_size(origin, vec2(WIDTH as f32, HEIGHT as f32) * unit)
}

/// The mark centered at `center`, for a row of the given height.
pub fn paint_centered(painter: &Painter, left: f32, center_y: f32, unit: f32) -> Rect {
    paint(painter, pos2(left, (center_y - HEIGHT as f32 * unit / 2.0).round()), unit)
}

/// The window's icon: the mark on a transparent square of `size` pixels,
/// which should be a multiple of 16.
pub fn icon(size: u32) -> IconData {
    let unit = (size / ICON_GRID).max(1);
    let left = (size - WIDTH * unit) / 2;
    // An odd leftover goes above the mark: its weight sits in the top half.
    let top = (size - HEIGHT * unit).div_ceil(2);
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for (x, y, width, height, color) in BARS {
        for row in top + y * unit..top + (y + height) * unit {
            for column in left + x * unit..left + (x + width) * unit {
                let at = ((row * size + column) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&[color.r(), color.g(), color.b(), 255]);
            }
        }
    }
    IconData { rgba, width: size, height: size }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 16-pixel icon, one character per pixel.
    fn rows(icon: &IconData) -> Vec<String> {
        icon.rgba
            .chunks(4 * icon.width as usize)
            .map(|row| {
                row.chunks(4)
                    .map(|p| match (p[3], Color32::from_rgb(p[0], p[1], p[2])) {
                        (0, _) => '.',
                        (_, color) if color == TEAL => 'T',
                        _ => 'V',
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_icon_is_the_mark_on_whole_pixels() {
        let small = icon(16);
        assert_eq!((small.width, small.height, small.rgba.len()), (16, 16, 16 * 16 * 4));
        // The same picture as assets/trill.ico at this size.
        let expected = [
            "................",
            "................",
            "................",
            "........TTTTTTT.",
            "........TTTTTTT.",
            ".VVVVVVVTTTTTTT.",
            ".VVVVVVVVV......",
            ".VVVVVVVVV......",
            "......VVVV......",
            "......VVVV......",
            "......VVVV......",
            "......VVVV......",
            "......VVVV......",
            "......VVVV......",
            "................",
            "................",
        ];
        assert_eq!(rows(&small), expected);

        // Larger sizes scale the grid by whole pixels and sit closer to the
        // true center: 56 x 44 pixels with 4 beside and 10 above and below.
        let large = rows(&icon(64));
        let filled: Vec<usize> = (0..64).filter(|&y| large[y].contains(['V', 'T'])).collect();
        assert_eq!((filled[0], filled[filled.len() - 1]), (10, 53));
        assert_eq!(large[10], format!("{}{}{}", ".".repeat(32), "T".repeat(28), ".".repeat(4)), "the raised bar");
        assert_eq!(
            large[53],
            format!("{}{}{}", ".".repeat(24), "V".repeat(16), ".".repeat(24)),
            "the foot of the stem"
        );
    }
}
