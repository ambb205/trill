//! Vector icons, drawn rather than taken from a font so they are the same
//! on every platform and sit on the pixel grid.

use crate::paint;
use egui::{Color32, Painter, Pos2, Rangef, Rect, Shape, Stroke, pos2, vec2};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Play,
    Stop,
    Loop,
    Plus,
    Minus,
    Close,
    /// The caret of a dropdown.
    CaretDown,
}

impl Icon {
    /// What the icon means, for assistive technology and tests.
    pub fn name(self) -> &'static str {
        match self {
            Icon::Play => "Play",
            Icon::Stop => "Stop",
            Icon::Loop => "Loop",
            Icon::Plus => "Add",
            Icon::Minus => "Less",
            Icon::Close => "Remove",
            Icon::CaretDown => "Open",
        }
    }
}

/// Paints `icon` centered on `center`. Icons have a fixed size (about ten
/// points) so their strokes land on whole pixels.
pub fn paint_icon(painter: &Painter, center: Pos2, icon: Icon, color: Color32) {
    let center = pos2(center.x.round(), center.y.round());
    match icon {
        Icon::Play => {
            let points = vec![center + vec2(-4.0, -5.0), center + vec2(5.0, 0.0), center + vec2(-4.0, 5.0)];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Stop => {
            painter.rect_filled(Rect::from_min_size(center - vec2(4.0, 4.0), vec2(9.0, 9.0)), 0.0, color);
        }
        Icon::Loop => {
            // A track with an arrowhead where it turns back.
            let track = Rect::from_min_size(center - vec2(6.0, 4.0), vec2(12.0, 8.0));
            paint::frame(painter, track, color);
            let tip = pos2(track.right() - 5.0, track.bottom() - 0.5);
            let head = vec![tip + vec2(3.0, -3.0), tip, tip + vec2(3.0, 3.0)];
            painter.add(Shape::line(head, Stroke::new(1.0, color)));
        }
        Icon::Plus => {
            paint::hline(painter, Rangef::new(center.x - 4.0, center.x + 5.0), center.y, color);
            paint::vline(painter, center.x, Rangef::new(center.y - 4.0, center.y + 5.0), color);
        }
        Icon::Minus => {
            paint::hline(painter, Rangef::new(center.x - 4.0, center.x + 5.0), center.y, color);
        }
        Icon::Close => {
            let stroke = Stroke::new(1.0, color);
            let middle = center + vec2(0.5, 0.5);
            painter.line_segment([middle + vec2(-3.5, -3.5), middle + vec2(3.5, 3.5)], stroke);
            painter.line_segment([middle + vec2(-3.5, 3.5), middle + vec2(3.5, -3.5)], stroke);
        }
        Icon::CaretDown => {
            let points = vec![center + vec2(-3.5, -2.0), center + vec2(3.5, -2.0), center + vec2(0.0, 2.0)];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
    }
}
