//! Hairlines and other flat marks.
//!
//! Lines are painted as one-pixel-thick filled rectangles rather than
//! stroked segments: egui snaps rectangles to the pixel grid, so a rule is
//! exactly one device pixel of the intended color at any scale, never two
//! half-covered rows.

use egui::{Color32, Painter, Pos2, Rangef, Rect, pos2, vec2};

/// A horizontal hairline whose top edge is at `y`.
pub fn hline(painter: &Painter, x: impl Into<Rangef>, y: f32, color: Color32) {
    let x = x.into();
    painter.rect_filled(Rect::from_min_max(pos2(x.min, y), pos2(x.max, y + 1.0)), 0.0, color);
}

/// A vertical hairline whose left edge is at `x`.
pub fn vline(painter: &Painter, x: f32, y: impl Into<Rangef>, color: Color32) {
    let y = y.into();
    painter.rect_filled(Rect::from_min_max(pos2(x, y.min), pos2(x + 1.0, y.max)), 0.0, color);
}

/// A hairline frame just inside `rect`.
///
/// Drawn as four rules that do not overlap, so a translucent color keeps
/// one strength all the way around, corners included.
pub fn frame(painter: &Painter, rect: Rect, color: Color32) {
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    hline(painter, rect.x_range(), rect.top(), color);
    hline(painter, rect.x_range(), rect.bottom() - 1.0, color);
    let sides = Rangef::new(rect.top() + 1.0, rect.bottom() - 1.0);
    vline(painter, rect.left(), sides, color);
    vline(painter, rect.right() - 1.0, sides, color);
}

/// A status dot: a small square, vertically centered on `left_center`.
/// Returns the x where whatever follows it starts.
pub fn dot(painter: &Painter, left_center: Pos2, side: f32, color: Color32) -> f32 {
    let top = (left_center.y - side / 2.0).round();
    painter.rect_filled(Rect::from_min_size(pos2(left_center.x, top), vec2(side, side)), 0.0, color);
    left_center.x + side
}

/// The outline of a status dot, for "off".
pub fn hollow_dot(painter: &Painter, left_center: Pos2, side: f32, color: Color32) -> f32 {
    let top = (left_center.y - side / 2.0).round();
    frame(painter, Rect::from_min_size(pos2(left_center.x, top), vec2(side, side)), color);
    left_center.x + side
}
