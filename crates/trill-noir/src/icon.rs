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
    /// A favorite, not chosen: the outline of a star.
    Star,
    /// A favorite: the star filled in.
    StarOn,
    Folder,
    /// A sound that plays once: a hit and its decay.
    Hit,
    /// A window of its own: the editor a plugin brings.
    Window,
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
            Icon::Star => "Not a favorite",
            Icon::StarOn => "Favorite",
            Icon::Folder => "Folder",
            Icon::Hit => "One-shot",
            Icon::Window => "Editor",
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
        Icon::Star | Icon::StarOn => {
            let middle = center + vec2(0.5, 1.0);
            let points = star(middle, 5.5, 2.3);
            if icon == Icon::StarOn {
                // A star is not convex, and convex shapes are all that fill
                // cleanly: five kites from the middle out to the tips, and
                // over the seams between them the pentagon they surround.
                for tip in 0..5 {
                    let kite = vec![points[2 * tip], points[2 * tip + 1], middle, points[(2 * tip + 9) % 10]];
                    painter.add(Shape::convex_polygon(kite, color, Stroke::NONE));
                }
                let inner: Vec<Pos2> = points.iter().skip(1).step_by(2).copied().collect();
                painter.add(Shape::convex_polygon(inner, color, Stroke::NONE));
            }
            painter.add(Shape::closed_line(points, Stroke::new(1.0, color)));
        }
        Icon::Folder => {
            // The body, and the tab along the left of its top edge.
            let body = Rect::from_min_size(center + vec2(-5.0, -3.0), vec2(11.0, 8.0));
            paint::frame(painter, body, color);
            paint::hline(painter, Rangef::new(body.left(), body.left() + 5.0), body.top() - 2.0, color);
            paint::vline(painter, body.left(), Rangef::new(body.top() - 2.0, body.top()), color);
            paint::vline(painter, body.left() + 4.0, Rangef::new(body.top() - 2.0, body.top()), color);
        }
        Icon::Hit => {
            // Loud at once and dying away: the shape of a drum hit.
            let floor = center.y + 4.0;
            let points =
                vec![pos2(center.x - 4.0, floor), pos2(center.x - 4.0, center.y - 5.0), pos2(center.x + 5.0, floor)];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Icon::Window => {
            // A frame with its title bar filled in.
            let frame = Rect::from_min_size(center + vec2(-5.0, -4.0), vec2(11.0, 9.0));
            paint::frame(painter, frame, color);
            painter.rect_filled(Rect::from_min_size(frame.min, vec2(frame.width(), 3.0)), 0.0, color);
        }
    }
}

/// The ten corners of a five-pointed star around `center`, from the top
/// tip clockwise: tips `outer` away, the corners between them `inner`.
fn star(center: Pos2, outer: f32, inner: f32) -> Vec<Pos2> {
    (0..10)
        .map(|corner| {
            let angle = std::f32::consts::TAU * corner as f32 / 10.0 - std::f32::consts::FRAC_PI_2;
            let radius = if corner % 2 == 0 { outer } else { inner };
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_has_five_tips_and_stands_upright() {
        let points = star(pos2(0.0, 0.0), 5.0, 2.0);
        assert_eq!(points.len(), 10);
        assert!((points[0].x).abs() < 1e-5 && (points[0].y + 5.0).abs() < 1e-5, "the first tip points up");
        for (corner, point) in points.iter().enumerate() {
            let radius = point.to_vec2().length();
            let expected = if corner % 2 == 0 { 5.0 } else { 2.0 };
            assert!((radius - expected).abs() < 1e-4, "corner {corner} is {radius} out");
        }
        // Mirror images across the vertical through the top tip.
        for corner in 1..5 {
            let (right, left) = (points[corner], points[10 - corner]);
            assert!((right.x + left.x).abs() < 1e-4 && (right.y - left.y).abs() < 1e-4);
        }
    }

    #[test]
    fn every_icon_paints_and_has_a_name() {
        let all = [
            Icon::Play,
            Icon::Stop,
            Icon::Loop,
            Icon::Plus,
            Icon::Minus,
            Icon::Close,
            Icon::CaretDown,
            Icon::CaretRight,
            Icon::Metronome,
            Icon::Pencil,
            Icon::Split,
            Icon::Star,
            Icon::StarOn,
            Icon::Folder,
            Icon::Hit,
            Icon::Window,
        ];
        crate::testing::with_ui(|ui, theme| {
            for icon in all {
                assert!(!icon.name().is_empty());
                paint_icon(ui.painter(), pos2(20.0, 20.0), icon, theme.ink);
            }
        });
        let mut names: Vec<&str> = all.iter().map(|icon| icon.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len(), "no two icons share a name");
    }
}
