//! The mark as the window uses it: in the top bar, and as its icon.
//!
//! The drawing itself is the kit's ([`trill_noir::mark`]): a T whose right
//! arm is a second bar raised a step, three rectangles on a 14 x 11 grid.
//! `assets/trill-mark.svg` is the same drawing for use outside the program.

use eframe::egui::IconData;
use trill_noir::mark::BARS;
pub use trill_noir::mark::{HEIGHT, WIDTH, paint_centered};
#[cfg(test)]
use {eframe::egui::Color32, trill_noir::mark::TEAL};

/// Side of the square the mark sits in as an icon, in grid units.
const ICON_GRID: u32 = 16;

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
