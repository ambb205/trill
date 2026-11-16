//! The Waveform: the outline of a sound, drawn as the band between the
//! lowest and highest value of the signal in each pixel column.
//!
//! The kit does not know about audio files. It is handed the columns, one
//! pair of values per pixel, and draws them; where they come from (an
//! overview of the file, at whatever zoom) is the caller's business. The
//! band goes into a [`Mesh`] the caller owns, so a view with fifty clips
//! on screen can put all their waveforms into one and paint it once.

use egui::{Color32, Mesh, Painter, Rect, pos2};

/// The lowest and highest value of the signal in one column, each in
/// -1..=1.
pub type Column = (f32, f32);

/// How many columns a waveform `width` points wide should be given: one
/// for every pixel of the device, so it is as sharp as the screen allows.
pub fn waveform_columns(painter: &Painter, width: f32) -> usize {
    (width * painter.pixels_per_point()).round().max(0.0) as usize
}

/// Adds the band of `columns` to `mesh`, spread evenly across `rect`, with
/// zero on its horizontal middle and full scale at its top and bottom.
///
/// `gain` scales the values, which are then held to full scale: a clip
/// turned up shows a taller waveform that flattens at the edges, as its
/// sound would. A column is never thinner than `hair` (the thickness of a
/// hairline, see [`crate::paint::hair`]), so silence is a line rather than
/// nothing.
pub fn waveform(mesh: &mut Mesh, rect: Rect, columns: &[Column], gain: f32, color: Color32, hair: f32) {
    if columns.is_empty() || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let middle = rect.center().y;
    let half = rect.height() / 2.0;
    let step = rect.width() / columns.len() as f32;
    let base = mesh.vertices.len() as u32;
    // Two vertices per column, at its center; the first and the last
    // column reach to the edges of the rectangle.
    let last = columns.len() - 1;
    for (index, &(low, high)) in columns.iter().enumerate() {
        let x = if index == 0 {
            rect.left()
        } else if index == last {
            rect.right()
        } else {
            rect.left() + (index as f32 + 0.5) * step
        };
        let (top, bottom) = band(middle, half, low * gain, high * gain, hair);
        mesh.colored_vertex(pos2(x, top), color);
        mesh.colored_vertex(pos2(x, bottom), color);
    }
    if last == 0 {
        // One column: a bar across the whole width.
        let (top, bottom) = band(middle, half, columns[0].0 * gain, columns[0].1 * gain, hair);
        mesh.colored_vertex(pos2(rect.right(), top), color);
        mesh.colored_vertex(pos2(rect.right(), bottom), color);
    }
    let pairs = (mesh.vertices.len() as u32 - base) / 2;
    for pair in 0..pairs.saturating_sub(1) {
        let first = base + 2 * pair;
        mesh.add_triangle(first, first + 1, first + 2);
        mesh.add_triangle(first + 1, first + 3, first + 2);
    }
}

/// The top and bottom of a column whose signal spans `low..=high`, at
/// least `hair` apart.
fn band(middle: f32, half: f32, low: f32, high: f32, hair: f32) -> (f32, f32) {
    // A value that is not a number draws as silence.
    let clean = |value: f32| if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 };
    let (low, high) = (clean(low).min(clean(high)), clean(high).max(clean(low)));
    let (mut top, mut bottom) = (middle - high * half, middle - low * half);
    if bottom - top < hair {
        let center = (top + bottom) / 2.0;
        top = center - hair / 2.0;
        bottom = center + hair / 2.0;
    }
    (top, bottom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn rect() -> Rect {
        Rect::from_min_size(pos2(10.0, 100.0), vec2(40.0, 20.0))
    }

    /// Top and bottom of every column drawn.
    fn bands(mesh: &Mesh) -> Vec<(f32, f32)> {
        mesh.vertices.chunks(2).map(|pair| (pair[0].pos.y, pair[1].pos.y)).collect()
    }

    #[test]
    fn the_band_spans_each_columns_range_around_the_middle() {
        let mut mesh = Mesh::default();
        waveform(&mut mesh, rect(), &[(-1.0, 1.0), (-0.5, 0.5), (0.0, 0.5), (-0.25, 0.0)], 1.0, Color32::WHITE, 1.0);
        assert_eq!(bands(&mesh), [(100.0, 120.0), (105.0, 115.0), (105.0, 110.0), (110.0, 112.5)]);
        // Two triangles between each pair of neighboring columns.
        assert_eq!(mesh.indices.len(), 3 * 2 * 3);
        let xs: Vec<f32> = mesh.vertices.iter().step_by(2).map(|vertex| vertex.pos.x).collect();
        assert_eq!(xs, [10.0, 25.0, 35.0, 50.0], "from edge to edge, the inner columns at their centers");
    }

    #[test]
    fn silence_is_a_line_and_loud_signals_stop_at_full_scale() {
        let mut mesh = Mesh::default();
        waveform(&mut mesh, rect(), &[(0.0, 0.0), (-3.0, 3.0), (f32::NAN, 0.5)], 1.0, Color32::WHITE, 1.0);
        let bands = bands(&mesh);
        assert_eq!(bands[0], (109.5, 110.5), "a hairline on the middle");
        assert_eq!(bands[1], (100.0, 120.0));
        assert_eq!(bands[2], (105.0, 110.0), "what is not a number counts as zero");

        // Gain scales the picture as it scales the sound.
        let mut quiet = Mesh::default();
        waveform(&mut quiet, rect(), &[(-1.0, 1.0), (-1.0, 1.0)], 0.5, Color32::WHITE, 1.0);
        assert_eq!(self::bands(&quiet), [(105.0, 115.0), (105.0, 115.0)]);
    }

    #[test]
    fn meshes_are_appended_to_and_empty_input_adds_nothing() {
        let mut mesh = Mesh::default();
        waveform(&mut mesh, rect(), &[], 1.0, Color32::WHITE, 1.0);
        waveform(&mut mesh, Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 10.0)), &[(0.0, 1.0)], 1.0, Color32::WHITE, 1.0);
        assert!(mesh.is_empty());

        waveform(&mut mesh, rect(), &[(-1.0, 1.0)], 1.0, Color32::WHITE, 1.0);
        assert_eq!((mesh.vertices.len(), mesh.indices.len()), (4, 6), "one column is a bar across the width");
        waveform(&mut mesh, rect(), &[(-1.0, 1.0), (0.0, 0.0)], 1.0, Color32::WHITE, 1.0);
        assert_eq!((mesh.vertices.len(), mesh.indices.len()), (8, 12));
        assert!(mesh.indices.iter().all(|index| (*index as usize) < mesh.vertices.len()));
        assert!(mesh.indices[6..].iter().all(|index| *index >= 4), "the second band uses its own vertices");
    }

    #[test]
    fn a_column_per_device_pixel() {
        crate::testing::with_ui(|ui, _| {
            let scale = ui.painter().pixels_per_point();
            assert_eq!(waveform_columns(ui.painter(), 100.0), (100.0 * scale).round() as usize);
            assert_eq!(waveform_columns(ui.painter(), -5.0), 0);
        });
    }
}
