//! The small displays in device panels: each reads a few parameter values
//! and draws what they do, with the math of [`super::response`].

use super::device_layouts::{Display, Shape};
use super::response::{self, EqBands, FilterShape};
use eframe::egui::{Color32, Painter, Rect};
use trill_noir::{Plot, Theme};

/// The audible range a frequency axis covers.
const LOW_HZ: f64 = 20.0;
const HIGH_HZ: f64 = 20_000.0;
/// Decibels from the middle of a filter display to its top or bottom.
const FILTER_SPAN_DB: f64 = 24.0;
const EQ_SPAN_DB: f64 = 18.0;
/// The level at the left and bottom edges of the compressor's curve.
const COMPRESSOR_FLOOR_DB: f64 = -60.0;

/// Frequency at a position across a display, on a logarithmic axis.
fn frequency_at(x: f32) -> f64 {
    LOW_HZ * (HIGH_HZ / LOW_HZ).powf(x as f64)
}

/// Position across a display of a frequency.
fn position_of(hz: f64) -> f32 {
    ((hz.clamp(LOW_HZ, HIGH_HZ) / LOW_HZ).ln() / (HIGH_HZ / LOW_HZ).ln()) as f32
}

/// Height in a display of a gain in dB, with 0 dB in the middle.
fn height_of(db: f64, span_db: f64) -> f32 {
    (0.5 + db / (2.0 * span_db)) as f32
}

/// Paints `display` into `rect`. `value` looks a parameter up by id;
/// `accent` colors the curves that belong to the signal (the track's hue).
pub fn paint(
    painter: &Painter,
    theme: &Theme,
    rect: Rect,
    display: Display,
    accent: Color32,
    value: &dyn Fn(&str) -> f64,
) {
    let plot = Plot::new(painter, theme, rect);
    let decades = [position_of(100.0), position_of(1000.0), position_of(10_000.0)];
    match display {
        Display::Filter { kind, shapes, cutoff, resonance, slope } => {
            let shape = match shapes.get(value(kind).round().max(0.0) as usize) {
                Some(Shape::LowPass) | None => Some(FilterShape::LowPass),
                Some(Shape::LowPass24) => Some(FilterShape::LowPass24),
                Some(Shape::HighPass) => Some(FilterShape::HighPass),
                Some(Shape::BandPass) => Some(FilterShape::BandPass),
                Some(Shape::Notch) => Some(FilterShape::Notch),
                Some(Shape::Off) => None,
            };
            let (cutoff, resonance) = (value(cutoff), value(resonance));
            let steep = slope.is_some_and(|slope| value(slope) >= 0.5);
            plot.verticals(&decades);
            plot.horizontals(&[0.5]);
            plot.curve(accent, 0.0, |x| match shape {
                Some(shape) => {
                    let gain = match steep {
                        true => response::steep_filter_gain(shape, cutoff, resonance, frequency_at(x)),
                        false => response::filter_gain(shape, cutoff, resonance, frequency_at(x)),
                    };
                    height_of(response::to_db(gain), FILTER_SPAN_DB)
                }
                // Switched off, everything passes unchanged.
                None => 0.5,
            });
        }
        Display::Envelope { attack, decay, sustain, release } => {
            let points = response::envelope_points(value(attack), value(decay), value(sustain), value(release));
            // Headroom, so the peak does not touch the frame.
            let scaled = points.map(|(x, y)| (x, y * 0.88));
            plot.drops(&scaled[1..4]);
            plot.polyline(theme.ink, &scaled);
        }
        Display::Eq => {
            let bands = EqBands {
                low_cut: value("low_cut"),
                low_cut_stages: response::cut_stages(value("low_cut_slope")),
                low_shelf: (value("low_freq"), value("low_gain"), value("low_q")),
                mid1: (value("mid1_freq"), value("mid1_gain"), value("mid1_q")),
                mid2: (value("mid2_freq"), value("mid2_gain"), value("mid2_q")),
                high_shelf: (value("high_freq"), value("high_gain"), value("high_q")),
                high_cut: value("high_cut"),
                high_cut_stages: response::cut_stages(value("high_cut_slope")),
            };
            plot.verticals(&decades);
            plot.horizontals(&[0.25, 0.5, 0.75]);
            plot.curve(accent, 0.0, |x| height_of(response::eq_gain_db(&bands, frequency_at(x)), EQ_SPAN_DB));
        }
        Display::Compressor => {
            let (threshold, ratio, knee) = (value("threshold"), value("ratio"), value("knee"));
            let level = |position: f32| COMPRESSOR_FLOOR_DB * (1.0 - position as f64);
            let height = |db: f64| (1.0 - db / COMPRESSOR_FLOOR_DB) as f32;
            plot.verticals(&[0.25, 0.5, 0.75]);
            plot.horizontals(&[0.25, 0.5, 0.75]);
            // Unity: what comes out when nothing is compressed.
            plot.reference((0.0, 0.0), (1.0, 1.0));
            plot.curve(theme.ink, 0.0, |x| height(response::compressor_curve(threshold, ratio, knee, level(x))));
            let at_threshold = height(threshold);
            plot.mark(accent, (at_threshold, height(response::compressor_curve(threshold, ratio, knee, threshold))));
        }
        Display::Saturator => {
            let shape = value("shape").round().max(0.0) as usize;
            // Drive in dB, as the device applies it; the display normalizes
            // the output so the shape is what shows, not the level.
            let drive = 10f64.powf(value("drive") / 20.0);
            let ceiling = response::saturate(shape, drive).abs().max(response::saturate(shape, -drive).abs()).max(1e-6);
            plot.verticals(&[0.5]);
            plot.horizontals(&[0.5]);
            plot.reference((0.0, 0.0), (1.0, 1.0));
            plot.curve(accent, 0.5, |x| {
                let input = (x as f64 * 2.0 - 1.0) * drive;
                (0.5 + 0.44 * response::saturate(shape, input) / ceiling) as f32
            });
        }
        Display::Tremolo => {
            let (depth, spread) = (value("depth"), value("spread") / 360.0);
            let shape = value("shape").round().max(0.0) as usize;
            let level = |phase: f64| (0.06 + 0.84 * response::tremolo_level(shape, depth, phase)) as f32;
            plot.verticals(&[0.5]);
            // The right side behind the left, where the two are apart.
            if spread > 0.0 {
                plot.curve(theme.ink_faint, 0.0, |x| level(x as f64 * 2.0 + spread));
            }
            plot.curve(accent, 0.0, |x| level(x as f64 * 2.0));
        }
        // Drawn by the panel of the device it belongs to, which knows its
        // tables: here it is an empty well.
        Display::Table { .. } => plot.horizontals(&[0.5]),
        Display::Lfo { shape, phase, depth } => {
            let (shape, start, depth) = (value(shape).round().max(0.0) as usize, value(phase), value(depth));
            plot.verticals(&[0.5]);
            plot.horizontals(&[0.5]);
            // Two cycles from where a note starts it, as deep as it is set.
            plot.curve(accent, 0.5, |x| {
                let at = x as f64 * 2.0 + start;
                (0.5 + 0.42 * depth * response::lfo_level(shape, at)) as f32
            });
        }
        Display::TableFilter { kind, cutoff, resonance } => {
            let (kind, cutoff, resonance) = (value(kind).round().max(0.0) as usize, value(cutoff), value(resonance));
            plot.verticals(&decades);
            plot.horizontals(&[0.5]);
            plot.curve(accent, 0.0, |x| {
                let gain = response::table_filter_gain(kind, cutoff, resonance, frequency_at(x));
                height_of(response::to_db(gain), FILTER_SPAN_DB)
            });
        }
        Display::Pump => {
            let (depth, curve) = (value("depth"), value("curve"));
            // Two cycles, so the duck on the beat is seen to repeat.
            plot.verticals(&[0.5]);
            plot.curve(accent, 0.0, |x| {
                let phase = (x as f64 * 2.0).fract();
                (0.06 + 0.84 * response::pump_gain(depth, curve, phase)) as f32
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frequency_axis_is_logarithmic_and_invertible() {
        assert_eq!(position_of(20.0), 0.0);
        assert!((position_of(20_000.0) - 1.0).abs() < 1e-6);
        assert!((position_of(632.456) - 0.5).abs() < 1e-4, "the geometric middle is halfway across");
        for hz in [20.0, 100.0, 1234.0, 19_000.0] {
            assert!((frequency_at(position_of(hz)) / hz - 1.0).abs() < 1e-4);
        }
        assert_eq!(position_of(5.0), 0.0, "frequencies below the axis pin to its edge");
    }

    #[test]
    fn unity_gain_sits_in_the_middle() {
        assert_eq!(height_of(0.0, EQ_SPAN_DB), 0.5);
        assert_eq!(height_of(EQ_SPAN_DB, EQ_SPAN_DB), 1.0);
        assert_eq!(height_of(-FILTER_SPAN_DB, FILTER_SPAN_DB), 0.0);
    }
}
