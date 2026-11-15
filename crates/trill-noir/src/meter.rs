//! Level displays: a slim stereo meter with peak hold, and a small
//! horizontal meter for single measurements.
//!
//! Meters take positions on their scale (0..=1), not decibels, so the
//! caller decides the scale and a meter lines up with the fader beside it.

use crate::paint;
use crate::theme::Theme;
use egui::{Color32, Painter, Rangef, Rect, pos2};

/// Where a level meter changes color, as positions on its scale.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Zones {
    /// Above this the level is close to the top: amber.
    pub hot: f32,
    /// Above this the signal is over full scale: the signal color.
    pub over: f32,
}

/// What a [`stereo_meter`] shows.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Levels {
    pub left: f32,
    pub right: f32,
    /// The held peak, drawn as a line across both channels.
    pub peak: Option<f32>,
}

/// Paints a vertical stereo meter: two columns a pixel apart, each filled
/// from the bottom in the colors of the zones the level passes through.
pub fn stereo_meter(painter: &Painter, theme: &Theme, rect: Rect, levels: Levels, zones: Zones) {
    // Columns and the gap between them are whole pixels, so both channels
    // are the same width at any scale.
    let scale = painter.pixels_per_point();
    let gap = paint::hair(painter);
    let column = (((rect.width() - gap) / 2.0 * scale).floor().max(1.0)) / scale;
    let left = paint::snap(painter, rect.left());
    let columns = [(left, levels.left), (left + column + gap, levels.right)];
    let y_at = |position: f32| paint::snap(painter, rect.bottom() - rect.height() * position.clamp(0.0, 1.0));
    let bands = [(0.0, zones.hot, theme.ok), (zones.hot, zones.over, theme.warn), (zones.over, 1.0, theme.signal)];

    for (left, level) in columns {
        let x = Rangef::new(left, left + column);
        painter.rect_filled(Rect::from_x_y_ranges(x, rect.y_range()), 0.0, theme.well);
        for (from, to, color) in bands {
            if level > from {
                let band = Rect::from_x_y_ranges(x, Rangef::new(y_at(level.min(to)), y_at(from)));
                painter.rect_filled(band, 0.0, color);
            }
        }
    }
    if let Some(peak) = levels.peak.filter(|peak| *peak > 0.0) {
        let y = y_at(peak).min(rect.bottom() - gap);
        let color = if peak > zones.over { theme.signal } else { theme.ink };
        paint::hline(painter, Rangef::new(left, left + 2.0 * column + gap), y, color);
    }
}

/// Paints a small horizontal meter: a hairline box filled from the left.
pub fn mini_meter(painter: &Painter, theme: &Theme, rect: Rect, position: f32, color: Color32) {
    paint::frame(painter, rect, theme.hairline);
    let inner = rect.shrink(1.0);
    let filled = (inner.width() * position.clamp(0.0, 1.0)).round();
    if filled > 0.0 {
        painter.rect_filled(Rect::from_min_max(inner.min, pos2(inner.left() + filled, inner.bottom())), 0.0, color);
    }
}
