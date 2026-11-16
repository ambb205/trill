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
    /// The caret of a dropdown, and of something unfolded.
    CaretDown,
    /// The caret of something folded shut.
    CaretRight,
    Metronome,
    /// The pencil that draws automation.
    Pencil,
    /// The blade that splits clips.
    Split,
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
            Icon::CaretRight => "Closed",
            Icon::Metronome => "Metronome",
            Icon::Pencil => "Draw",
            Icon::Split => "Split",
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
        Icon::CaretRight => {
            let points = vec![center + vec2(-2.0, -3.5), center + vec2(2.0, 0.0), center + vec2(-2.0, 3.5)];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Metronome => {
            // The case, narrower at the top, and the pendulum leaning in it.
            let stroke = Stroke::new(1.0, color);
            let middle = center + vec2(0.5, 0.5);
            let case = vec![
                middle + vec2(-4.0, 5.0),
                middle + vec2(-1.5, -5.0),
                middle + vec2(1.5, -5.0),
                middle + vec2(4.0, 5.0),
            ];
            painter.add(Shape::closed_line(case, stroke));
            painter.line_segment([middle + vec2(-0.5, 3.0), middle + vec2(3.5, -4.0)], stroke);
        }
        Icon::Pencil => {
            // A shaft from the top right down to a point at the bottom left.
            let middle = center + vec2(0.5, 0.5);
            painter.line_segment([middle + vec2(-2.0, 2.0), middle + vec2(4.0, -4.0)], Stroke::new(2.5, color));
            let tip = vec![middle + vec2(-5.0, 5.0), middle + vec2(-4.0, 1.5), middle + vec2(-1.5, 4.0)];
            painter.add(Shape::convex_polygon(tip, color, Stroke::NONE));
        }
        Icon::Split => {
            // A cut line with the two halves it leaves pointing at it.
            paint::vline(painter, center.x, Rangef::new(center.y - 5.0, center.y + 6.0), color);
            let left = vec![center + vec2(-6.0, -3.0), center + vec2(-2.0, 0.5), center + vec2(-6.0, 4.0)];
            let right = vec![center + vec2(7.0, -3.0), center + vec2(3.0, 0.5), center + vec2(7.0, 4.0)];
            painter.add(Shape::convex_polygon(left, color, Stroke::NONE));
            painter.add(Shape::convex_polygon(right, color, Stroke::NONE));
        }
    }
}
