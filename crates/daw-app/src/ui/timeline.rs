//! Pure time ↔ pixel math shared by the arrangement and the piano roll:
//! zooming and panning, grid selection, snapping, position formatting and
//! hit-testing. Nothing here touches egui's context, so all of it is
//! unit-tested.

use daw_core::{Beats, Note, TimeSignature};

/// Horizontal mapping between beats and pixels: a scroll position and a zoom
/// level.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TimeView {
    /// Beat at the left edge of the view.
    pub origin: Beats,
    pub pixels_per_beat: f64,
}

impl TimeView {
    pub const MIN_PIXELS_PER_BEAT: f64 = 1.5;
    pub const MAX_PIXELS_PER_BEAT: f64 = 480.0;

    pub fn new(pixels_per_beat: f64) -> Self {
        Self { origin: 0.0, pixels_per_beat }
    }

    /// Screen x of `beat` in a view whose left edge is at `left`.
    pub fn beat_to_x(&self, left: f32, beat: Beats) -> f32 {
        left + ((beat - self.origin) * self.pixels_per_beat) as f32
    }

    pub fn x_to_beat(&self, left: f32, x: f32) -> Beats {
        self.origin + (x - left) as f64 / self.pixels_per_beat
    }

    /// Scrolls by `dx` pixels; positive moves the content left (later beats
    /// come into view). The view never scrolls before beat 0.
    pub fn pan_pixels(&mut self, dx: f32) {
        self.origin = (self.origin + dx as f64 / self.pixels_per_beat).max(0.0);
    }

    /// Zooms by `factor`, keeping the beat under screen position `x` fixed
    /// (unless that would scroll before beat 0).
    pub fn zoom_around(&mut self, left: f32, x: f32, factor: f64) {
        let anchor = self.x_to_beat(left, x);
        self.pixels_per_beat =
            (self.pixels_per_beat * factor).clamp(Self::MIN_PIXELS_PER_BEAT, Self::MAX_PIXELS_PER_BEAT);
        self.origin = (anchor - (x - left) as f64 / self.pixels_per_beat).max(0.0);
    }

    /// Shows `start..end` across `width` pixels, with a little margin.
    pub fn fit(&mut self, start: Beats, end: Beats, width: f32) {
        let span = (end - start).max(1.0);
        self.pixels_per_beat =
            ((width as f64 * 0.94) / span).clamp(Self::MIN_PIXELS_PER_BEAT, Self::MAX_PIXELS_PER_BEAT);
        self.origin = (start - span * 0.03).max(0.0);
    }

    /// The beat range visible in a view `width` pixels wide.
    pub fn visible(&self, width: f32) -> (Beats, Beats) {
        (self.origin, self.origin + width as f64 / self.pixels_per_beat)
    }
}

/// How prominent a grid line is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GridLevel {
    Bar,
    Beat,
    Subdivision,
}

/// The finest grid ever drawn: a thirty-second note.
const FINEST_STEP: Beats = 0.125;

/// Length of one beat of the meter in quarter notes: an eighth in 6/8.
fn beat_unit(signature: TimeSignature) -> Beats {
    4.0 / signature.denominator as f64
}

/// How the meter groups its beats within a bar, in quarter notes: three
/// eighths in compound meters such as 6/8, otherwise two beats. `None` when
/// such groups would not divide the bar evenly (3/4, 5/4, 7/8).
fn beat_group(signature: TimeSignature) -> Option<Beats> {
    let compound = signature.denominator == 8 && signature.numerator > 3 && signature.numerator.is_multiple_of(3);
    let beats = if compound { 3 } else { 2 };
    let divides_bar = signature.numerator.is_multiple_of(beats) && signature.numerator > beats;
    divides_bar.then(|| beats as f64 * beat_unit(signature))
}

/// The finest grid step, in quarter notes, whose lines are at least
/// `min_pixels` apart at this zoom.
///
/// Steps follow the meter: subdivisions of its beat, then the beat, then
/// its beat groups, then whole bars (1, 2, 4, 8, ...).
pub fn grid_step(pixels_per_beat: f64, signature: TimeSignature, min_pixels: f64) -> Beats {
    let fits = |step: Beats| step * pixels_per_beat >= min_pixels;
    let beats_per_bar = signature.beats_per_bar();
    let unit = beat_unit(signature);

    let mut step = FINEST_STEP;
    while step <= unit {
        if fits(step) && step < beats_per_bar {
            return step;
        }
        step *= 2.0;
    }
    if let Some(group) = beat_group(signature).filter(|group| fits(*group)) {
        return group;
    }
    let mut bars = 1.0;
    while !fits(bars * beats_per_bar) && bars < 4096.0 {
        bars *= 2.0;
    }
    bars * beats_per_bar
}

/// Calls `line(beat, level)` for every grid line in `start..=end`.
///
/// Lines are laid out bar by bar, so that when a bar is not a whole number
/// of `step`s (7/8 with a quarter-note snap) every bar still starts on a
/// line.
pub fn for_each_grid_line(
    start: Beats,
    end: Beats,
    signature: TimeSignature,
    step: Beats,
    mut line: impl FnMut(Beats, GridLevel),
) {
    let beats_per_bar = signature.beats_per_bar();
    let unit = beat_unit(signature);
    if !(step > 0.0 && end >= start) {
        return;
    }
    if step >= beats_per_bar {
        let bars_per_line = (step / beats_per_bar).round().max(1.0);
        let stride = bars_per_line * beats_per_bar;
        let mut beat = (start / stride).floor() * stride;
        while beat <= end {
            if beat >= start {
                line(beat, GridLevel::Bar);
            }
            beat += stride;
        }
        return;
    }
    let mut bar = (start / beats_per_bar).floor().max(0.0);
    loop {
        let bar_start = bar * beats_per_bar;
        if bar_start > end {
            break;
        }
        let mut index = 0u32;
        loop {
            let offset = index as f64 * step;
            if offset >= beats_per_bar - 1e-9 {
                break;
            }
            let beat = bar_start + offset;
            if beat > end {
                break;
            }
            if beat >= start {
                let beats = offset / unit;
                let level = if index == 0 {
                    GridLevel::Bar
                } else if (beats - beats.round()).abs() < 1e-9 {
                    GridLevel::Beat
                } else {
                    GridLevel::Subdivision
                };
                line(beat, level);
            }
            index += 1;
        }
        bar += 1.0;
    }
}

/// Note-value grids offered by the piano roll.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Snap {
    Off,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
    EighthTriplet,
    SixteenthTriplet,
}

impl Snap {
    pub const ALL: [Snap; 7] = [
        Snap::Quarter,
        Snap::Eighth,
        Snap::Sixteenth,
        Snap::ThirtySecond,
        Snap::EighthTriplet,
        Snap::SixteenthTriplet,
        Snap::Off,
    ];

    /// Grid size in beats; `None` when snapping is off.
    pub fn step(self) -> Option<Beats> {
        match self {
            Snap::Off => None,
            Snap::Quarter => Some(1.0),
            Snap::Eighth => Some(0.5),
            Snap::Sixteenth => Some(0.25),
            Snap::ThirtySecond => Some(0.125),
            Snap::EighthTriplet => Some(1.0 / 3.0),
            Snap::SixteenthTriplet => Some(1.0 / 6.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Snap::Off => "Off",
            Snap::Quarter => "1/4",
            Snap::Eighth => "1/8",
            Snap::Sixteenth => "1/16",
            Snap::ThirtySecond => "1/32",
            Snap::EighthTriplet => "1/8T",
            Snap::SixteenthTriplet => "1/16T",
        }
    }
}

/// Rounds `beat` to the nearest multiple of `step`, never below zero.
pub fn snap_round(beat: Beats, step: Option<Beats>) -> Beats {
    match step {
        Some(step) if step > 0.0 => ((beat / step).round() * step).max(0.0),
        _ => beat.max(0.0),
    }
}

/// Rounds `beat` down to a multiple of `step`: the grid cell that contains
/// it.
pub fn snap_floor(beat: Beats, step: Option<Beats>) -> Beats {
    match step {
        // The epsilon keeps a beat that sits exactly on a line in its own cell
        // despite floating-point error in the pixel conversion.
        Some(step) if step > 0.0 => (((beat + 1e-9) / step).floor() * step).max(0.0),
        _ => beat.max(0.0),
    }
}

/// Rounds `beat` to the nearest line of a bar-relative grid (see
/// [`for_each_grid_line`]).
pub fn snap_to_bar_grid(beat: Beats, beats_per_bar: Beats, step: Beats) -> Beats {
    if !(step > 0.0 && beats_per_bar > 0.0) {
        return beat.max(0.0);
    }
    if step >= beats_per_bar {
        return snap_round(beat, Some(step));
    }
    let bar_start = (beat / beats_per_bar).floor() * beats_per_bar;
    let offset = beat - bar_start;
    // The lines on either side; the one after the bar's last line is the
    // next bar, which may be closer than a full step away.
    let lower = ((offset + 1e-9) / step).floor() * step;
    let upper = (lower + step).min(beats_per_bar);
    let within = if offset - lower <= upper - offset { lower } else { upper };
    (bar_start + within).max(0.0)
}

/// A song position split into musical units, all 1-based as musicians count.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BarPosition {
    pub bar: u32,
    pub beat: u32,
    pub sixteenth: u32,
}

/// Splits a position in quarter notes into bar, beat and sixteenth. A "beat"
/// is the note value of the signature's denominator (an eighth in 6/8).
pub fn bar_position(beat: Beats, signature: TimeSignature) -> BarPosition {
    // The epsilon keeps positions that are a hair under a boundary (after
    // float arithmetic) from displaying as the previous unit.
    let beat = beat.max(0.0) + 1e-9;
    let per_bar = signature.beats_per_bar();
    let unit = 4.0 / signature.denominator as f64;
    let bar = (beat / per_bar).floor();
    let in_bar = beat - bar * per_bar;
    let beat_index = (in_bar / unit).floor();
    let in_beat = in_bar - beat_index * unit;
    BarPosition { bar: bar as u32 + 1, beat: beat_index as u32 + 1, sixteenth: (in_beat / 0.25).floor() as u32 + 1 }
}

/// `"5.3.2"`: bar, beat, sixteenth.
pub fn format_position(beat: Beats, signature: TimeSignature) -> String {
    let p = bar_position(beat, signature);
    format!("{}.{}.{}", p.bar, p.beat, p.sixteenth)
}

/// `"5.3"`, or just `"5"` on a bar line. Used for ruler labels.
pub fn format_bar_beat(beat: Beats, signature: TimeSignature) -> String {
    let p = bar_position(beat, signature);
    if p.beat == 1 && p.sixteenth == 1 { p.bar.to_string() } else { format!("{}.{}", p.bar, p.beat) }
}

/// A length such as `"2 bars"`, `"1 bar"`, `"1.5 beats"`.
pub fn format_length(length: Beats, signature: TimeSignature) -> String {
    let bars = length / signature.beats_per_bar();
    if (bars - bars.round()).abs() < 1e-9 && bars >= 1.0 {
        let bars = bars.round() as u64;
        return if bars == 1 { "1 bar".to_owned() } else { format!("{bars} bars") };
    }
    let rounded = (length * 100.0).round() / 100.0;
    if rounded == 1.0 { "1 beat".to_owned() } else { format!("{rounded} beats") }
}

/// Elapsed time at a fixed tempo, `"1:05.2"`.
pub fn format_clock(beat: Beats, tempo: f64) -> String {
    let seconds = (beat.max(0.0) * 60.0 / tempo.max(1.0) * 10.0).floor() / 10.0;
    let minutes = (seconds / 60.0).floor();
    format!("{}:{:04.1}", minutes as u64, seconds - minutes * 60.0)
}

/// Which part of a clip or note the pointer is over.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpanHit {
    Body,
    /// The right edge, which resizes.
    End,
}

/// Hit-tests a horizontal span `left..right` at `x`. The resize handle is
/// `handle` pixels wide, but never more than a third of the span so short
/// items can still be grabbed by the body.
pub fn span_hit(x: f32, left: f32, right: f32, handle: f32) -> Option<SpanHit> {
    if x < left || x > right {
        return None;
    }
    let handle = handle.min((right - left) / 3.0);
    Some(if x >= right - handle { SpanHit::End } else { SpanHit::Body })
}

/// Vertical mapping between pitches and pixels in the piano roll.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PitchView {
    /// Pitch boundary at the top edge: 128.0 puts the top of G9 there.
    pub top: f32,
    pub row_height: f32,
}

impl PitchView {
    pub const MIN_ROW_HEIGHT: f32 = 5.0;
    pub const MAX_ROW_HEIGHT: f32 = 32.0;

    /// Screen y of the top of `pitch`'s row in a view whose top edge is at
    /// `view_top`.
    pub fn pitch_to_y(&self, view_top: f32, pitch: u8) -> f32 {
        view_top + (self.top - (pitch as f32 + 1.0)) * self.row_height
    }

    pub fn y_to_pitch(&self, view_top: f32, y: f32) -> u8 {
        // A row covers the half-open range just below its upper boundary.
        ((self.top - (y - view_top) / self.row_height).ceil() - 1.0).clamp(0.0, 127.0) as u8
    }

    /// Keeps the view inside the 128-note range for a view `height` pixels
    /// tall.
    pub fn clamp(&mut self, height: f32) {
        let rows = height / self.row_height;
        self.top = self.top.clamp(rows.min(128.0), 128.0);
    }

    pub fn scroll_pixels(&mut self, dy: f32, height: f32) {
        self.top += dy / self.row_height;
        self.clamp(height);
    }

    /// Changes the row height, keeping the pitch under screen position `y`
    /// fixed.
    pub fn zoom_around(&mut self, view_top: f32, y: f32, factor: f32, height: f32) {
        let anchor = self.top - (y - view_top) / self.row_height;
        self.row_height = (self.row_height * factor).clamp(Self::MIN_ROW_HEIGHT, Self::MAX_ROW_HEIGHT);
        self.top = anchor + (y - view_top) / self.row_height;
        self.clamp(height);
    }

    /// Centers the view on the pitch range `low..=high`.
    pub fn center_on(&mut self, low: u8, high: u8, height: f32) {
        let middle = (low as f32 + high as f32 + 1.0) / 2.0;
        self.top = middle + height / self.row_height / 2.0;
        self.clamp(height);
    }
}

/// The topmost note under (`beat`, `pitch`), as an index into `notes`, and
/// which part of it was hit. `handle_beats` is the width of the resize zone
/// at the note's end. Later notes win, matching paint order.
pub fn note_hit(notes: &[Note], beat: Beats, pitch: u8, handle_beats: Beats) -> Option<(usize, SpanHit)> {
    notes.iter().enumerate().rev().find_map(|(index, note)| {
        if note.pitch != pitch || beat < note.start || beat > note.end() {
            return None;
        }
        let handle = handle_beats.min(note.length / 3.0);
        Some((index, if beat >= note.end() - handle { SpanHit::End } else { SpanHit::Body }))
    })
}

/// Lowest and highest pitch in `notes`.
pub fn pitch_range(notes: &[Note]) -> Option<(u8, u8)> {
    notes.iter().fold(None, |range, note| match range {
        None => Some((note.pitch, note.pitch)),
        Some((low, high)) => Some((low.min(note.pitch), high.max(note.pitch))),
    })
}

/// True for the black keys of a piano.
pub fn is_black_key(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(numerator: u8, denominator: u8) -> TimeSignature {
        TimeSignature::new(numerator, denominator).unwrap()
    }

    fn note(pitch: u8, start: Beats, length: Beats) -> Note {
        Note { pitch, start, length, velocity: 100 }
    }

    #[test]
    fn beats_and_pixels_round_trip() {
        let view = TimeView { origin: 8.0, pixels_per_beat: 24.0 };
        assert_eq!(view.beat_to_x(100.0, 8.0), 100.0);
        assert_eq!(view.beat_to_x(100.0, 9.5), 136.0);
        assert_eq!(view.x_to_beat(100.0, 136.0), 9.5);
        for beat in [0.0, 3.25, 8.0, 100.125] {
            let x = view.beat_to_x(40.0, beat);
            assert!((view.x_to_beat(40.0, x) - beat).abs() < 1e-4);
        }
        assert_eq!(view.visible(240.0), (8.0, 18.0));
    }

    #[test]
    fn zoom_keeps_the_beat_under_the_cursor() {
        let mut view = TimeView { origin: 16.0, pixels_per_beat: 20.0 };
        let cursor = 340.0;
        let before = view.x_to_beat(100.0, cursor);
        view.zoom_around(100.0, cursor, 1.5);
        assert_eq!(view.pixels_per_beat, 30.0);
        assert!((view.x_to_beat(100.0, cursor) - before).abs() < 1e-9);
        view.zoom_around(100.0, cursor, 0.5);
        assert!((view.x_to_beat(100.0, cursor) - before).abs() < 1e-9);
    }

    #[test]
    fn zoom_and_pan_stay_in_range() {
        let mut view = TimeView::new(20.0);
        view.zoom_around(0.0, 500.0, 1e-6);
        assert_eq!(view.pixels_per_beat, TimeView::MIN_PIXELS_PER_BEAT);
        assert_eq!(view.origin, 0.0, "zooming out near the start cannot scroll before beat 0");
        view.zoom_around(0.0, 0.0, 1e9);
        assert_eq!(view.pixels_per_beat, TimeView::MAX_PIXELS_PER_BEAT);
        view.pan_pixels(-10_000.0);
        assert_eq!(view.origin, 0.0);
        view.pan_pixels(960.0);
        assert_eq!(view.origin, 2.0);
    }

    #[test]
    fn fit_shows_the_whole_range() {
        let mut view = TimeView::new(10.0);
        view.fit(8.0, 24.0, 800.0);
        let (start, end) = view.visible(800.0);
        assert!(start <= 8.0 && end >= 24.0, "{start}..{end}");
        assert!(end - start < 20.0, "the range should nearly fill the view");
    }

    #[test]
    fn grid_gets_finer_as_you_zoom_in() {
        let four_four = TimeSignature::default();
        assert_eq!(grid_step(2.0, four_four, 16.0), 8.0, "two bars per line when a bar is 8 px wide");
        assert_eq!(grid_step(4.0, four_four, 16.0), 4.0);
        assert_eq!(grid_step(10.0, four_four, 16.0), 2.0, "half bars");
        assert_eq!(grid_step(20.0, four_four, 16.0), 1.0);
        assert_eq!(grid_step(70.0, four_four, 16.0), 0.25);
        assert_eq!(grid_step(400.0, four_four, 16.0), 0.125, "never finer than a 32nd");
        assert_eq!(grid_step(0.01, four_four, 16.0), 512.0 * 4.0);
    }

    #[test]
    fn the_grid_follows_the_meter() {
        // 6/8 is two groups of three eighths, never three quarters.
        let six_eight = signature(6, 8);
        assert_eq!(grid_step(8.0, six_eight, 16.0), 3.0, "whole bars");
        assert_eq!(grid_step(20.0, six_eight, 16.0), 1.5, "dotted quarters");
        assert_eq!(grid_step(40.0, six_eight, 16.0), 0.5, "eighths");
        assert_eq!(grid_step(80.0, six_eight, 16.0), 0.25);
        assert_eq!(grid_step(20.0, signature(12, 8), 16.0), 1.5);
        // Pairs of beats do not tile a 3/4 or 5/4 bar, so those go straight
        // from beats to bars.
        assert_eq!(grid_step(10.0, signature(3, 4), 16.0), 3.0);
        assert_eq!(grid_step(10.0, signature(5, 4), 16.0), 5.0);
        assert_eq!(grid_step(20.0, signature(7, 8), 16.0), 3.5);
        assert_eq!(grid_step(40.0, signature(7, 8), 16.0), 0.5);
        // A 2/4 bar is a single pair; a 2/2 beat is a half note.
        assert_eq!(grid_step(10.0, signature(2, 4), 16.0), 2.0);
        assert_eq!(grid_step(10.0, signature(2, 2), 16.0), 2.0);
        assert_eq!(grid_step(5.0, signature(4, 2), 16.0), 4.0, "pairs of half notes");
    }

    fn lines(start: Beats, end: Beats, signature: TimeSignature, step: Beats) -> Vec<(Beats, GridLevel)> {
        let mut out = Vec::new();
        for_each_grid_line(start, end, signature, step, |beat, level| out.push((beat, level)));
        out
    }

    #[test]
    fn grid_lines_mark_bars_beats_and_subdivisions() {
        use GridLevel::*;
        let four_four = TimeSignature::default();
        assert_eq!(
            lines(0.0, 4.0, four_four, 0.5),
            [
                (0.0, Bar),
                (0.5, Subdivision),
                (1.0, Beat),
                (1.5, Subdivision),
                (2.0, Beat),
                (2.5, Subdivision),
                (3.0, Beat),
                (3.5, Subdivision),
                (4.0, Bar)
            ]
        );
        assert_eq!(lines(3.0, 17.0, four_four, 8.0), [(8.0, Bar), (16.0, Bar)]);
        assert_eq!(lines(5.0, 9.0, four_four, 4.0), [(8.0, Bar)]);
        // 7/8: bars are 3.5 quarter notes long, and each one restarts the
        // grid.
        let seven_eight = signature(7, 8);
        assert_eq!(lines(0.0, 7.0, seven_eight, 1.0).iter().filter(|(_, level)| *level == Bar).count(), 3);
        assert_eq!(lines(3.4, 4.6, seven_eight, 1.0), [(3.5, Bar), (4.5, Beat)]);
        // In 6/8 the eighths are the beats; sixteenths subdivide them.
        assert_eq!(
            lines(0.0, 0.8, signature(6, 8), 0.25),
            [(0.0, Bar), (0.25, Subdivision), (0.5, Beat), (0.75, Subdivision)]
        );
        assert!(lines(4.0, 2.0, four_four, 1.0).is_empty());
    }

    #[test]
    fn snapping_rounds_to_the_grid() {
        assert_eq!(snap_round(1.1, Some(0.25)), 1.0);
        assert_eq!(snap_round(1.13, Some(0.25)), 1.25);
        assert_eq!(snap_round(-0.4, Some(0.25)), 0.0);
        assert_eq!(snap_round(1.13, None), 1.13);
        assert_eq!(snap_floor(1.24, Some(0.25)), 1.0);
        assert_eq!(snap_floor(1.25, Some(0.25)), 1.25);
        assert_eq!(snap_floor(0.999_999_999_9, Some(0.5)), 1.0, "float error does not drop a cell");
        let triplet = Snap::EighthTriplet.step();
        assert!((snap_round(0.7, triplet) - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(Snap::Sixteenth.step(), Some(0.25));
        assert_eq!(Snap::Off.step(), None);
        assert_eq!(Snap::ALL.len(), 7);
    }

    #[test]
    fn bar_grid_snapping_respects_odd_meters() {
        assert_eq!(snap_to_bar_grid(5.4, 4.0, 1.0), 5.0);
        assert_eq!(snap_to_bar_grid(5.6, 4.0, 1.0), 6.0);
        assert_eq!(snap_to_bar_grid(9.0, 4.0, 8.0), 8.0);
        assert_eq!(snap_to_bar_grid(13.0, 4.0, 8.0), 16.0);
        // 7/8 with a quarter grid: lines at 0, 1, 2, 3, then the bar at 3.5.
        assert_eq!(snap_to_bar_grid(3.2, 3.5, 1.0), 3.0);
        assert_eq!(snap_to_bar_grid(3.3, 3.5, 1.0), 3.5);
        assert_eq!(snap_to_bar_grid(3.6, 3.5, 1.0), 3.5);
        assert_eq!(snap_to_bar_grid(4.4, 3.5, 1.0), 4.5);
        assert_eq!(snap_to_bar_grid(-2.0, 4.0, 1.0), 0.0);
    }

    #[test]
    fn positions_count_from_one() {
        let four_four = TimeSignature::default();
        assert_eq!(format_position(0.0, four_four), "1.1.1");
        assert_eq!(format_position(4.0, four_four), "2.1.1");
        assert_eq!(format_position(18.75, four_four), "5.3.4");
        assert_eq!(format_position(-3.0, four_four), "1.1.1");
        // 0.1 * 3 is a hair under 0.3 in floating point, 3.9999999 a hair under 4.
        assert_eq!(format_position(3.999_999_999_9, four_four), "2.1.1");
        // 6/8: six eighth-note beats per bar, each two sixteenths long.
        assert_eq!(format_position(2.75, signature(6, 8)), "1.6.2");
        assert_eq!(format_position(3.0, signature(6, 8)), "2.1.1");
        assert_eq!(bar_position(7.0, signature(7, 8)), BarPosition { bar: 3, beat: 1, sixteenth: 1 });
        assert_eq!(format_bar_beat(8.0, four_four), "3");
        assert_eq!(format_bar_beat(9.0, four_four), "3.2");
    }

    #[test]
    fn lengths_and_clock_read_naturally() {
        let four_four = TimeSignature::default();
        assert_eq!(format_length(4.0, four_four), "1 bar");
        assert_eq!(format_length(16.0, four_four), "4 bars");
        assert_eq!(format_length(1.0, four_four), "1 beat");
        assert_eq!(format_length(6.0, four_four), "6 beats");
        assert_eq!(format_length(0.5, four_four), "0.5 beats");
        assert_eq!(format_length(3.0, signature(6, 8)), "1 bar");
        assert_eq!(format_clock(0.0, 120.0), "0:00.0");
        assert_eq!(format_clock(5.0, 120.0), "0:02.5");
        assert_eq!(format_clock(130.0, 120.0), "1:05.0");
    }

    #[test]
    fn spans_have_a_resize_handle_at_the_end() {
        assert_eq!(span_hit(9.0, 10.0, 110.0, 8.0), None);
        assert_eq!(span_hit(10.0, 10.0, 110.0, 8.0), Some(SpanHit::Body));
        assert_eq!(span_hit(101.0, 10.0, 110.0, 8.0), Some(SpanHit::Body));
        assert_eq!(span_hit(103.0, 10.0, 110.0, 8.0), Some(SpanHit::End));
        assert_eq!(span_hit(111.0, 10.0, 110.0, 8.0), None);
        // A 9 px item keeps two thirds of its width for moving.
        assert_eq!(span_hit(15.0, 10.0, 19.0, 8.0), Some(SpanHit::Body));
        assert_eq!(span_hit(17.0, 10.0, 19.0, 8.0), Some(SpanHit::End));
    }

    #[test]
    fn pitches_and_rows_round_trip() {
        let view = PitchView { top: 72.0, row_height: 10.0 };
        assert_eq!(view.pitch_to_y(50.0, 71), 50.0);
        assert_eq!(view.pitch_to_y(50.0, 60), 160.0);
        assert_eq!(view.y_to_pitch(50.0, 50.0), 71);
        assert_eq!(view.y_to_pitch(50.0, 59.9), 71);
        assert_eq!(view.y_to_pitch(50.0, 60.0), 70);
        assert_eq!(view.y_to_pitch(50.0, 9000.0), 0);
        assert_eq!(view.y_to_pitch(50.0, -9000.0), 127);
        for pitch in [0u8, 36, 60, 71] {
            assert_eq!(view.y_to_pitch(0.0, view.pitch_to_y(0.0, pitch) + 1.0), pitch);
        }
    }

    #[test]
    fn pitch_view_stays_on_the_keyboard() {
        let mut view = PitchView { top: 72.0, row_height: 10.0 };
        view.scroll_pixels(100_000.0, 200.0);
        assert_eq!(view.top, 128.0);
        view.scroll_pixels(-100_000.0, 200.0);
        assert_eq!(view.top, 20.0, "the bottom row is pitch 0");
        view.center_on(60, 72, 200.0);
        assert_eq!(view.top, 76.5);
        let anchor_before = view.top - 80.0 / view.row_height;
        view.zoom_around(0.0, 80.0, 2.0, 200.0);
        assert_eq!(view.row_height, 20.0);
        assert!((view.top - 80.0 / view.row_height - anchor_before).abs() < 1e-4);
        view.zoom_around(0.0, 80.0, 100.0, 200.0);
        assert_eq!(view.row_height, PitchView::MAX_ROW_HEIGHT);
    }

    #[test]
    fn note_hits_prefer_the_topmost_note() {
        let notes = [note(60, 0.0, 2.0), note(60, 1.0, 2.0), note(64, 0.0, 0.3)];
        assert_eq!(note_hit(&notes, 0.5, 60, 0.25), Some((0, SpanHit::Body)));
        assert_eq!(note_hit(&notes, 1.5, 60, 0.25), Some((1, SpanHit::Body)), "the later note is drawn on top");
        assert_eq!(note_hit(&notes, 2.9, 60, 0.25), Some((1, SpanHit::End)));
        assert_eq!(note_hit(&notes, 0.5, 61, 0.25), None);
        assert_eq!(note_hit(&notes, 3.5, 60, 0.25), None);
        // Short notes keep most of their length for moving.
        assert_eq!(note_hit(&notes, 0.1, 64, 0.25), Some((2, SpanHit::Body)));
        assert_eq!(note_hit(&notes, 0.25, 64, 0.25), Some((2, SpanHit::End)));
    }

    #[test]
    fn pitch_helpers() {
        assert_eq!(pitch_range(&[]), None);
        assert_eq!(pitch_range(&[note(64, 0.0, 1.0), note(48, 1.0, 1.0), note(72, 2.0, 1.0)]), Some((48, 72)));
        let black: Vec<u8> = (60..72).filter(|p| is_black_key(*p)).collect();
        assert_eq!(black, [61, 63, 66, 68, 70]);
    }
}
