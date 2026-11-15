//! Small displays: a well with a faint grid and a curve in it. Used for
//! filter and equalizer responses, envelopes and transfer curves.
//!
//! Everything is given in unit coordinates, x left to right and y bottom to
//! top, so the caller's math stays free of pixels.

use crate::paint;
use crate::theme::{Theme, fade};
use egui::{Color32, Mesh, Painter, Pos2, Rangef, Rect, Shape, Stroke, pos2};

/// Opacity of the tint under a curve.
const TINT: f32 = 0.10;
const LINE_WIDTH: f32 = 1.25;

/// A display being painted.
pub struct Plot<'a> {
    painter: Painter,
    theme: &'a Theme,
    /// The plotting area, inside the frame.
    area: Rect,
}

impl<'a> Plot<'a> {
    /// Paints the well and its frame in `rect` and returns the plot to draw
    /// into.
    pub fn new(painter: &Painter, theme: &'a Theme, rect: Rect) -> Self {
        let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
        painter.rect_filled(rect, 0.0, theme.bg);
        paint::frame(&painter, rect, theme.hairline);
        Self { painter, theme, area: rect.shrink(1.0) }
    }

    fn point(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.area.left() + self.area.width() * x, self.area.bottom() - self.area.height() * y)
    }

    /// Vertical grid lines at these positions across the plot.
    pub fn verticals(&self, positions: &[f32]) {
        for x in positions {
            paint::vline(&self.painter, self.point(*x, 0.0).x.round(), self.area.y_range(), self.theme.grid);
        }
    }

    /// Horizontal grid lines at these heights.
    pub fn horizontals(&self, heights: &[f32]) {
        for y in heights {
            paint::hline(&self.painter, self.area.x_range(), self.point(0.0, *y).y.round(), self.theme.grid);
        }
    }

    /// A function of x, sampled once per pixel, with a tint between it and
    /// `baseline` (a height). Values outside 0..=1 run off the plot.
    pub fn curve(&self, color: Color32, baseline: f32, value: impl Fn(f32) -> f32) {
        let columns = self.area.width().round().max(1.0) as usize;
        let points: Vec<Pos2> = (0..=columns)
            .map(|column| {
                let x = column as f32 / columns as f32;
                // A little overshoot keeps a clipped curve's line off the frame.
                self.point(x, value(x).clamp(-0.2, 1.2))
            })
            .collect();
        self.tint(&points, baseline, color);
        self.painter.add(Shape::line(points, Stroke::new(LINE_WIDTH, color)));
    }

    /// A shape through `points` (unit coordinates), tinted down to the
    /// bottom of the plot: an envelope.
    pub fn polyline(&self, color: Color32, points: &[(f32, f32)]) {
        let points: Vec<Pos2> = points.iter().map(|(x, y)| self.point(*x, *y)).collect();
        self.tint(&points, 0.0, color);
        self.painter.add(Shape::line(points, Stroke::new(LINE_WIDTH, color)));
    }

    /// Hairline drops from the bottom of the plot up to these points: where
    /// one stage of an envelope hands over to the next.
    pub fn drops(&self, points: &[(f32, f32)]) {
        for (x, y) in points {
            let top = self.point(*x, *y);
            paint::vline(&self.painter, top.x.round(), Rangef::new(top.y, self.area.bottom()), self.theme.hairline);
        }
    }

    /// A straight reference line between two points, in the grid color.
    pub fn reference(&self, from: (f32, f32), to: (f32, f32)) {
        let stroke = Stroke::new(1.0, self.theme.grid);
        self.painter.line_segment([self.point(from.0, from.1), self.point(to.0, to.1)], stroke);
    }

    /// A small square mark at a point, in `color`.
    pub fn mark(&self, color: Color32, at: (f32, f32)) {
        let center = self.point(at.0, at.1);
        let corner = pos2(center.x.round() - 2.0, center.y.round() - 2.0);
        self.painter.rect_filled(Rect::from_min_size(corner, egui::vec2(4.0, 4.0)), 0.0, color);
    }

    /// Fills between a line and a baseline. The line is a function of x, so
    /// the area is a strip of quads, one per segment.
    fn tint(&self, points: &[Pos2], baseline: f32, color: Color32) {
        let base = self.point(0.0, baseline.clamp(0.0, 1.0)).y;
        let fill = fade(color, TINT);
        let mut mesh = Mesh::default();
        for (index, point) in points.iter().enumerate() {
            mesh.colored_vertex(*point, fill);
            mesh.colored_vertex(pos2(point.x, base), fill);
            if index > 0 {
                let first = 2 * index as u32;
                mesh.add_triangle(first - 2, first - 1, first);
                mesh.add_triangle(first - 1, first + 1, first);
            }
        }
        self.painter.add(Shape::mesh(mesh));
    }
}
