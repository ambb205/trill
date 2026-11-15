//! Hairlines and other flat marks, placed on the device's pixel grid.
//!
//! The kit's strokes are one pixel: one *device* pixel at 100 % and 150 %,
//! two at 200 %. A line a point thick would be a pixel and a half at 150 %,
//! which a renderer shows as one pixel here and two there. So lines are
//! painted as filled rectangles a whole number of pixels thick, with the
//! edge they hug snapped to the grid: [`hline`] and [`vline`] start at the
//! coordinate given and grow down or right; [`hline_above`] and
//! [`vline_left_of`] end at it. Whatever is filled up to the same
//! coordinate meets the line exactly, at any scale.

use egui::{Color32, Painter, Pos2, Rangef, Rect, pos2};

/// Thickness of a hairline, in points.
pub fn hair(painter: &Painter) -> f32 {
    let scale = painter.pixels_per_point();
    scale.floor().max(1.0) / scale
}

/// `value`, in points, moved to the nearest pixel boundary.
pub fn snap(painter: &Painter, value: f32) -> f32 {
    let scale = painter.pixels_per_point();
    (value * scale).round() / scale
}

/// A horizontal hairline just below `y`.
pub fn hline(painter: &Painter, x: impl Into<Rangef>, y: f32, color: Color32) {
    let top = snap(painter, y);
    painter.rect_filled(Rect::from_x_y_ranges(x, Rangef::new(top, top + hair(painter))), 0.0, color);
}

/// A horizontal hairline just above `y`: along the bottom edge of
/// something that ends there.
pub fn hline_above(painter: &Painter, x: impl Into<Rangef>, y: f32, color: Color32) {
    let bottom = snap(painter, y);
    painter.rect_filled(Rect::from_x_y_ranges(x, Rangef::new(bottom - hair(painter), bottom)), 0.0, color);
}

/// A vertical hairline just right of `x`.
pub fn vline(painter: &Painter, x: f32, y: impl Into<Rangef>, color: Color32) {
    let left = snap(painter, x);
    painter.rect_filled(Rect::from_x_y_ranges(Rangef::new(left, left + hair(painter)), y), 0.0, color);
}

/// A vertical hairline just left of `x`: along the right edge of something
/// that ends there.
pub fn vline_left_of(painter: &Painter, x: f32, y: impl Into<Rangef>, color: Color32) {
    let right = snap(painter, x);
    painter.rect_filled(Rect::from_x_y_ranges(Rangef::new(right - hair(painter), right), y), 0.0, color);
}

/// A hairline frame just inside `rect`.
///
/// Drawn as four rules that do not overlap, so a translucent color keeps
/// one strength all the way around, corners included.
pub fn frame(painter: &Painter, rect: Rect, color: Color32) {
    let hair = hair(painter);
    if rect.width() < 2.0 * hair || rect.height() < 2.0 * hair {
        return;
    }
    hline(painter, rect.x_range(), rect.top(), color);
    hline_above(painter, rect.x_range(), rect.bottom(), color);
    let sides = Rangef::new(snap(painter, rect.top()) + hair, snap(painter, rect.bottom()) - hair);
    vline(painter, rect.left(), sides, color);
    vline_left_of(painter, rect.right(), sides, color);
}

/// The square of a status dot `side` points wide, vertically centered on
/// `left_center`: a whole number of pixels each way, so every dot in a
/// column is the same size.
fn dot_rect(painter: &Painter, left_center: Pos2, side: f32) -> Rect {
    let side = snap(painter, side);
    let left = snap(painter, left_center.x);
    let top = snap(painter, left_center.y - side / 2.0);
    Rect::from_min_max(pos2(left, top), pos2(left + side, top + side))
}

/// A status dot: a small square. Returns the x where whatever follows it
/// starts.
pub fn dot(painter: &Painter, left_center: Pos2, side: f32, color: Color32) -> f32 {
    let rect = dot_rect(painter, left_center, side);
    painter.rect_filled(rect, 0.0, color);
    rect.right()
}

/// The outline of a status dot, for "off".
pub fn hollow_dot(painter: &Painter, left_center: Pos2, side: f32, color: Color32) -> f32 {
    let rect = dot_rect(painter, left_center, side);
    frame(painter, rect, color);
    rect.right()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A painter in a context running at `scale` pixels per point.
    fn at_scale(scale: f32, check: impl FnOnce(&Painter)) {
        let ctx = egui::Context::default();
        let mut check = Some(check);
        let input = egui::RawInput {
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo { native_pixels_per_point: Some(scale), ..Default::default() },
            ))
            .collect(),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            if let Some(check) = check.take() {
                assert_eq!(ui.painter().pixels_per_point(), scale);
                check(ui.painter());
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn a_hairline_is_a_whole_number_of_device_pixels() {
        for (scale, pixels) in [(1.0, 1.0), (1.25, 1.0), (1.5, 1.0), (2.0, 2.0), (2.5, 2.0), (3.0, 3.0)] {
            at_scale(scale, |painter| {
                assert!((hair(painter) * scale - pixels).abs() < 1e-4, "at {scale}x: {} px", hair(painter) * scale);
            });
        }
    }

    #[test]
    fn edges_snap_to_the_pixel_grid() {
        at_scale(1.5, |painter| {
            // 101 pt is 151.5 px: not on the grid.
            let snapped = snap(painter, 101.0) * 1.5;
            assert!((snapped - snapped.round()).abs() < 1e-4, "{snapped}");
            assert!((snap(painter, 100.0) - 100.0).abs() < 1e-5, "150 px is already on it");
            // A dot is the same number of pixels wide wherever it lands.
            for x in [10.0, 10.3, 11.0, 11.7] {
                let rect = dot_rect(painter, pos2(x, 20.0), 5.0);
                assert!((rect.width() * 1.5 - 8.0).abs() < 1e-3, "at {x}: {} px", rect.width() * 1.5);
                assert!((rect.height() * 1.5 - 8.0).abs() < 1e-3);
            }
        });
        at_scale(1.0, |painter| {
            assert_eq!(
                dot_rect(painter, pos2(10.0, 20.0), 5.0),
                Rect::from_min_max(pos2(10.0, 18.0), pos2(15.0, 23.0))
            );
        });
    }
}
