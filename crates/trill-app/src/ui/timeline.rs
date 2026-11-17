//! Pure time ↔ pixel math shared by the arrangement and the piano roll:
//! zooming and panning, grid selection, snapping, position formatting and
//! hit-testing. Nothing here touches egui's context, so all of it is
//! unit-tested.

use trill_core::{Beats, MeterMap, Note, TempoMap, TimeSignature};

/// What a grid is laid in and a position counted in: one time signature
/// all through (a [`TimeSignature`]), or a song's bars through every change
/// of it (a [`MeterMap`]).
pub trait Bars {
    /// The stretch in one time signature that `beat` lies in.
    fn stretch(&self, beat: Beats) -> Stretch;
}

/// A stretch of time in one time signature.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Stretch {
    /// Where it begins, which is a bar line, and where it ends: infinite
    /// if nothing follows it.
    pub start: Beats,
    pub end: Beats,
    /// How many bars lie before it.
    pub bars_before: u32,
    pub signature: TimeSignature,
}

impl Bars for TimeSignature {
    fn stretch(&self, _: Beats) -> Stretch {
        Stretch { start: 0.0, end: f64::INFINITY, bars_before: 0, signature: *self }
    }
}

impl Bars for MeterMap {
    fn stretch(&self, beat: Beats) -> Stretch {
        let start = self.since(beat);
        let first = self.bar_at(start);
        let end = self.next_change(beat.max(start)).unwrap_or(f64::INFINITY);
        Stretch { start, end, bars_before: first.index, signature: first.signature }
    }
}

/// The first stretches of a song's bars, as something that can be copied
/// about: what the view of one clip keeps of the time signatures under it.
/// A clip that lies under more changes than it holds is counted in the
/// last one it holds from there on.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FewBars {
    stretches: [Stretch; FewBars::MOST],
    count: usize,
}

impl FewBars {
    const MOST: usize = 8;

    /// One time signature all through.
    pub fn constant(signature: TimeSignature) -> Self {
        Self { stretches: [signature.stretch(0.0); Self::MOST], count: 1 }
    }

    pub fn of(bars: &MeterMap) -> Self {
        let mut few = Self::constant(bars.signature_at(0.0));
        few.count = 0;
        for bar in bars.sections().take(Self::MOST) {
            if let Some(before) = few.count.checked_sub(1) {
                few.stretches[before].end = bar.start;
            }
            few.stretches[few.count] =
                Stretch { start: bar.start, end: f64::INFINITY, bars_before: bar.index, signature: bar.signature };
            few.count += 1;
        }
        few
    }
}

impl Bars for FewBars {
    fn stretch(&self, beat: Beats) -> Stretch {
        let held = &self.stretches[..self.count.max(1)];
        let after = held.partition_point(|stretch| stretch.start <= beat + 1e-9);
        held[after.saturating_sub(1)]
    }
}

impl<T: Bars + ?Sized> Bars for &T {
    fn stretch(&self, beat: Beats) -> Stretch {
        (**self).stretch(beat)
    }
}

/// A grid step chosen for bars of `first`, as it is in bars of
/// `signature`: so many whole bars stay so many whole bars, and a step
/// within the bar is never longer than a bar is.
fn step_in(step: Beats, first: TimeSignature, signature: TimeSignature) -> Beats {
    let (own, other) = (first.beats_per_bar(), signature.beats_per_bar());
    if step >= own - 1e-9 { (step / own).round().max(1.0) * other } else { step.min(other) }
}

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
/// line. Where the time signature changes a bar begins, and the lines go
/// on from there in the new one: `step` is the step of the first time
/// signature, and each other one gets what it comes to there.
pub fn for_each_grid_line(
    start: Beats,
    end: Beats,
    bars: impl Bars,
    step: Beats,
    mut line: impl FnMut(Beats, GridLevel),
) {
    if !(step > 0.0 && end >= start) {
        return;
    }
    let first = bars.stretch(0.0).signature;
    let mut from = start;
    loop {
        let stretch = bars.stretch(from.max(0.0));
        let local = step_in(step, first, stretch.signature);
        let lines = |beat: Beats, level: GridLevel| {
            // The line a change lies on is the first of what follows it.
            let beat = beat + stretch.start;
            if beat < stretch.end - 1e-9 {
                line(beat, level);
            }
        };
        grid_lines_of(from - stretch.start, end.min(stretch.end) - stretch.start, stretch.signature, local, lines);
        if stretch.end > end {
            return;
        }
        from = stretch.end;
    }
}

/// [`for_each_grid_line`] in one time signature, counted from beat 0.
fn grid_lines_of(
    start: Beats,
    end: Beats,
    signature: TimeSignature,
    step: Beats,
    mut line: impl FnMut(Beats, GridLevel),
) {
    let beats_per_bar = signature.beats_per_bar();
    let unit = beat_unit(signature);
    if end < start {
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

/// The grid of the arrangement: the one that follows the zoom, or a fixed
/// note value.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum GridChoice {
    /// As fine as the zoom shows, never coarser than a bar.
    #[default]
    Adaptive,
    Fixed(Snap),
}

impl GridChoice {
    /// The choices in menu order.
    pub const ALL: [GridChoice; 8] = [
        GridChoice::Adaptive,
        GridChoice::Fixed(Snap::Quarter),
        GridChoice::Fixed(Snap::Eighth),
        GridChoice::Fixed(Snap::Sixteenth),
        GridChoice::Fixed(Snap::ThirtySecond),
        GridChoice::Fixed(Snap::EighthTriplet),
        GridChoice::Fixed(Snap::SixteenthTriplet),
        GridChoice::Fixed(Snap::Off),
    ];

    pub fn label(self) -> &'static str {
        match self {
            GridChoice::Adaptive => "Auto",
            GridChoice::Fixed(snap) => snap.label(),
        }
    }

    /// The spacing things snap to, given the spacing `adaptive` that the
    /// zoom suggests. `None` when the grid is off.
    pub fn step(self, adaptive: Beats) -> Option<Beats> {
        match self {
            GridChoice::Adaptive => Some(adaptive),
            GridChoice::Fixed(snap) => snap.step(),
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

/// Rounds `beat` to the nearest line of the grid that [`for_each_grid_line`]
/// draws through `bars` with `step`: bar by bar, and through every change
/// of time signature, where a bar line is.
pub fn snap_in(beat: Beats, bars: impl Bars, step: Beats) -> Beats {
    let first = bars.stretch(0.0).signature;
    let stretch = bars.stretch(beat.max(0.0));
    let local = step_in(step, first, stretch.signature);
    let within = snap_to_bar_grid(beat - stretch.start, stretch.signature.beats_per_bar(), local);
    (stretch.start + within).min(stretch.end).max(0.0)
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
/// The bar is counted through every change of time signature, and the beat
/// in the signature the bar has.
pub fn bar_position(beat: Beats, bars: impl Bars) -> BarPosition {
    // The epsilon keeps positions that are a hair under a boundary (after
    // float arithmetic) from displaying as the previous unit.
    let beat = beat.max(0.0) + 1e-9;
    let stretch = bars.stretch(beat);
    let signature = stretch.signature;
    let beat = beat - stretch.start;
    let per_bar = signature.beats_per_bar();
    let unit = 4.0 / signature.denominator as f64;
    let bar = (beat / per_bar).floor();
    let in_bar = beat - bar * per_bar;
    let beat_index = (in_bar / unit).floor();
    let in_beat = in_bar - beat_index * unit;
    BarPosition {
        bar: stretch.bars_before + bar as u32 + 1,
        beat: beat_index as u32 + 1,
        sixteenth: (in_beat / 0.25).floor() as u32 + 1,
    }
}

/// `"5.3.2"`: bar, beat, sixteenth.
pub fn format_position(beat: Beats, bars: impl Bars) -> String {
    let p = bar_position(beat, bars);
    format!("{}.{}.{}", p.bar, p.beat, p.sixteenth)
}

/// `"5.3"`, or just `"5"` on a bar line. Used for ruler labels.
pub fn format_bar_beat(beat: Beats, bars: impl Bars) -> String {
    let p = bar_position(beat, bars);
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
    format_seconds(beat.max(0.0) * 60.0 / tempo.max(1.0))
}

/// The time a song takes to get to `beat`, through every tempo it has on
/// the way, `"1:05.2"`.
pub fn format_clock_through(beat: Beats, tempo: &TempoMap) -> String {
    format_seconds(tempo.seconds_at(beat.max(0.0)))
}

fn format_seconds(seconds: f64) -> String {
    let seconds = (seconds * 10.0).floor() / 10.0;
    let minutes = (seconds / 60.0).floor();
    format!("{}:{:04.1}", minutes as u64, seconds - minutes * 60.0)
}

/// Which part of a note the pointer is over.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpanHit {
    Body,
    /// The right edge, which resizes.
    End,
}

/// Which part of a clip the pointer is over, when both of its edges trim.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EdgeHit {
    Start,
    Body,
    End,
}

/// Hit-tests a span `left..right` whose two edges are handles `handle`
/// pixels wide. The handles never take more than a quarter of the span
/// each, so half of even the shortest item is left for moving it.
pub fn edge_hit(x: f32, left: f32, right: f32, handle: f32) -> Option<EdgeHit> {
    if x < left || x > right {
        return None;
    }
    let handle = handle.min((right - left) / 4.0);
    Some(if x >= right - handle {
        EdgeHit::End
    } else if x < left + handle {
        EdgeHit::Start
    } else {
        EdgeHit::Body
    })
}

/// A view on its way from one zoom and scroll position to another: what
/// makes "zoom to fit" glide instead of jump.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Glide {
    pub from: TimeView,
    pub to: TimeView,
    /// When it began, in seconds of the UI's clock.
    pub started: f64,
}

impl Glide {
    pub const SECONDS: f64 = 0.16;

    /// The view at time `now`, and whether the glide has arrived. Zoom is
    /// interpolated in ratios, so every frame scales by the same factor.
    pub fn at(&self, now: f64) -> (TimeView, bool) {
        let progress = ((now - self.started) / Self::SECONDS).clamp(0.0, 1.0);
        if progress >= 1.0 {
            return (self.to, true);
        }
        // Ease out: fast away from where it was, gentle on arrival.
        let eased = 1.0 - (1.0 - progress).powi(3);
        let ratio = self.to.pixels_per_beat / self.from.pixels_per_beat;
        let view = TimeView {
            origin: self.from.origin + (self.to.origin - self.from.origin) * eased,
            pixels_per_beat: self.from.pixels_per_beat * ratio.powf(eased),
        };
        (view, false)
    }
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

    /// Two bars of 4/4, two of 7/8 from beat 8, and 3/4 from beat 15.
    fn changing() -> MeterMap {
        let mut song = trill_core::Project::new("t");
        song.set_time_signature_at(8.0, 7, 8).unwrap();
        song.set_time_signature_at(15.0, 3, 4).unwrap();
        song.meter_map()
    }

    #[test]
    fn a_grid_goes_on_from_every_change_of_time_signature() {
        let bars = changing();
        let lines = |start: Beats, end: Beats, step: Beats| {
            let mut lines = Vec::new();
            for_each_grid_line(start, end, &bars, step, |beat, level| lines.push((beat, level)));
            lines
        };
        // Bar lines only: the bars of each signature, each as long as it is.
        let bar_lines: Vec<Beats> = lines(0.0, 22.0, 4.0).into_iter().map(|(beat, _)| beat).collect();
        assert_eq!(bar_lines, [0.0, 4.0, 8.0, 11.5, 15.0, 18.0, 21.0]);
        assert!(lines(0.0, 22.0, 4.0).iter().all(|(_, level)| *level == GridLevel::Bar));
        // Every other bar, counted in each stretch from where it begins.
        let wide: Vec<Beats> = lines(0.0, 30.0, 8.0).into_iter().map(|(beat, _)| beat).collect();
        assert_eq!(wide, [0.0, 8.0, 15.0, 21.0, 27.0]);
        // Quarter notes: a bar of 7/8 has three and a half of them, and
        // the next bar begins on a line all the same.
        let quarters = lines(7.0, 16.0, 1.0);
        let at: Vec<Beats> = quarters.iter().map(|(beat, _)| *beat).collect();
        assert_eq!(at, [7.0, 8.0, 9.0, 10.0, 11.0, 11.5, 12.5, 13.5, 14.5, 15.0, 16.0]);
        let strong: Vec<Beats> =
            quarters.iter().filter(|(_, level)| *level == GridLevel::Bar).map(|(beat, _)| *beat).collect();
        assert_eq!(strong, [8.0, 11.5, 15.0]);
        // One signature all through is what it always was.
        let mut plain = Vec::new();
        for_each_grid_line(0.0, 8.0, TimeSignature::default(), 1.0, |beat, _| plain.push(beat));
        assert_eq!(plain, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn positions_and_snapping_are_counted_through_the_changes() {
        let bars = changing();
        for (beat, said) in [(0.0, "1.1.1"), (7.75, "2.4.4"), (8.0, "3.1.1"), (11.0, "3.7.1"), (11.5, "4.1.1")] {
            assert_eq!(format_position(beat, &bars), said, "at beat {beat}");
        }
        assert_eq!((format_position(15.0, &bars), format_position(19.25, &bars)), ("5.1.1".into(), "6.2.2".into()));
        assert_eq!((format_bar_beat(11.5, &bars), format_bar_beat(12.0, &bars)), ("4".into(), "4.2".into()));
        assert_eq!(bar_position(100.0, &bars).bar, 5 + 28);

        // To the bar: the bars of the song, not multiples of four beats.
        for (beat, snapped) in [(9.0, 8.0), (10.0, 11.5), (13.0, 11.5), (13.5, 15.0), (16.4, 15.0), (16.6, 18.0)] {
            assert_eq!(snap_in(beat, &bars, 4.0), snapped, "beat {beat} to the bar");
        }
        // To the quarter note: from each bar line on, and to the next bar
        // line where a bar ends between two quarters.
        for (beat, snapped) in [(11.2, 11.0), (11.3, 11.5), (11.9, 11.5), (12.1, 12.5), (14.8, 15.0)] {
            assert_eq!(snap_in(beat, &bars, 1.0), snapped, "beat {beat} to the quarter");
        }
        assert_eq!(snap_in(5.4, TimeSignature::default(), 1.0), snap_to_bar_grid(5.4, 4.0, 1.0));
        // What a clip's view keeps of the bars says the same.
        let few = FewBars::of(&bars);
        for beat in [0.0, 7.75, 8.0, 11.5, 14.9, 15.0, 40.0] {
            assert_eq!(few.stretch(beat), bars.stretch(beat), "at beat {beat}");
        }
        assert_eq!(FewBars::constant(TimeSignature::default()).stretch(9.0), TimeSignature::default().stretch(9.0));
    }

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
    fn both_edges_of_a_clip_are_handles() {
        assert_eq!(edge_hit(9.0, 10.0, 110.0, 6.0), None);
        assert_eq!(edge_hit(10.0, 10.0, 110.0, 6.0), Some(EdgeHit::Start));
        assert_eq!(edge_hit(15.9, 10.0, 110.0, 6.0), Some(EdgeHit::Start));
        assert_eq!(edge_hit(16.0, 10.0, 110.0, 6.0), Some(EdgeHit::Body));
        assert_eq!(edge_hit(103.9, 10.0, 110.0, 6.0), Some(EdgeHit::Body));
        assert_eq!(edge_hit(104.0, 10.0, 110.0, 6.0), Some(EdgeHit::End));
        assert_eq!(edge_hit(110.5, 10.0, 110.0, 6.0), None);
        // An 8 px clip keeps its middle half for moving.
        assert_eq!(edge_hit(11.0, 10.0, 18.0, 6.0), Some(EdgeHit::Start));
        assert_eq!(edge_hit(14.0, 10.0, 18.0, 6.0), Some(EdgeHit::Body));
        assert_eq!(edge_hit(17.0, 10.0, 18.0, 6.0), Some(EdgeHit::End));
    }

    #[test]
    fn the_grid_choice_is_adaptive_fixed_or_off() {
        assert_eq!(GridChoice::default(), GridChoice::Adaptive);
        assert_eq!(GridChoice::Adaptive.step(2.0), Some(2.0), "whatever the zoom shows");
        assert_eq!(GridChoice::Fixed(Snap::Sixteenth).step(2.0), Some(0.25));
        assert_eq!(GridChoice::Fixed(Snap::Off).step(2.0), None);
        let labels: Vec<&str> = GridChoice::ALL.iter().map(|choice| choice.label()).collect();
        assert_eq!(labels, ["Auto", "1/4", "1/8", "1/16", "1/32", "1/8T", "1/16T", "Off"]);
    }

    #[test]
    fn a_glide_arrives_exactly_and_passes_through_the_middle() {
        let from = TimeView { origin: 0.0, pixels_per_beat: 10.0 };
        let to = TimeView { origin: 32.0, pixels_per_beat: 40.0 };
        let glide = Glide { from, to, started: 5.0 };
        assert_eq!(glide.at(5.0), (from, false));
        let (middle, arrived) = glide.at(5.0 + Glide::SECONDS / 2.0);
        assert!(!arrived);
        assert!(middle.origin > 16.0 && middle.origin < 32.0, "eased out, so past halfway: {}", middle.origin);
        assert!(middle.pixels_per_beat > 10.0 && middle.pixels_per_beat < 40.0);
        assert_eq!(glide.at(5.0 + Glide::SECONDS), (to, true));
        assert_eq!(glide.at(99.0), (to, true));
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
