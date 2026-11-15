//! Waveform overviews: the minimum and maximum of a buffer at several
//! resolutions, so a waveform can be drawn at any zoom by reading a few
//! values per pixel column instead of every sample under it.

use daw_core::audio::AudioBuffer;
use std::ops::Range;

/// The lowest and highest sample in a span of audio.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MinMax {
    pub min: f32,
    pub max: f32,
}

impl MinMax {
    /// A span with no signal in it.
    pub const SILENT: Self = Self { min: 0.0, max: 0.0 };

    /// The identity for [`MinMax::include`]: wider than nothing.
    const NOTHING: Self = Self { min: f32::INFINITY, max: f32::NEG_INFINITY };

    fn include(&mut self, other: Self) {
        if other.min < self.min {
            self.min = other.min;
        }
        if other.max > self.max {
            self.max = other.max;
        }
    }

    fn include_sample(&mut self, sample: f32) {
        if sample < self.min {
            self.min = sample;
        }
        if sample > self.max {
            self.max = sample;
        }
    }

    /// `NOTHING` survives only when no comparable sample was seen (an empty
    /// span, or one made of NaNs); that draws as silence.
    fn or_silent(self) -> Self {
        if self.min <= self.max { self } else { Self::SILENT }
    }
}

/// One column of a waveform drawing: the range each channel covers, and the
/// range of the mono mix `(left + right) / 2`, which is what a single-lane
/// waveform shows.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PeakColumn {
    pub left: MinMax,
    pub right: MinMax,
    pub mixed: MinMax,
}

impl PeakColumn {
    pub const SILENT: Self = Self { left: MinMax::SILENT, right: MinMax::SILENT, mixed: MinMax::SILENT };

    const NOTHING: Self = Self { left: MinMax::NOTHING, right: MinMax::NOTHING, mixed: MinMax::NOTHING };

    fn include(&mut self, other: &Self) {
        self.left.include(other.left);
        self.right.include(other.right);
        self.mixed.include(other.mixed);
    }

    fn or_silent(self) -> Self {
        Self { left: self.left.or_silent(), right: self.right.or_silent(), mixed: self.mixed.or_silent() }
    }
}

/// A multi-resolution min/max overview of one audio buffer.
///
/// The finest level summarises [`Peaks::BLOCK`] frames per entry and each
/// level above it four entries of the one below, so the whole pyramid costs
/// about 1.6 % of the audio it describes and a query touches a handful of
/// entries per column whatever the zoom.
#[derive(Clone, Debug, PartialEq)]
pub struct Peaks {
    frames: usize,
    sample_rate: f64,
    /// `levels[n]` holds one entry per `BLOCK * FANOUT^n` frames.
    levels: Vec<Vec<PeakColumn>>,
}

impl Peaks {
    /// Frames summarised by one entry of the finest level. Columns narrower
    /// than this are still bounded correctly by [`Peaks::columns`], only
    /// less tightly; [`scan_columns`] reads the samples for those.
    pub const BLOCK: usize = 256;

    const FANOUT: usize = 4;

    /// Summarises `buffer`. One pass over the samples; a five-minute stereo
    /// file takes a few tens of milliseconds.
    pub fn build(buffer: &AudioBuffer) -> Self {
        let frames = buffer.frames();
        let (left, right) = (&buffer.left[..frames], &buffer.right[..frames]);
        let finest: Vec<PeakColumn> = left
            .chunks(Self::BLOCK)
            .zip(right.chunks(Self::BLOCK))
            .map(|(left, right)| summarise(left, right).or_silent())
            .collect();

        let mut levels = vec![finest];
        loop {
            let below = &levels[levels.len() - 1];
            if below.len() <= 1 {
                break;
            }
            let above = below
                .chunks(Self::FANOUT)
                .map(|group| {
                    let mut merged = PeakColumn::NOTHING;
                    group.iter().for_each(|entry| merged.include(entry));
                    merged
                })
                .collect();
            levels.push(above);
        }
        Self { frames, sample_rate: buffer.sample_rate, levels }
    }

    /// Frames in the buffer this was built from.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Sample rate of the buffer this was built from.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Bytes of memory the overview occupies.
    pub fn memory_bytes(&self) -> usize {
        self.levels.iter().map(|level| level.len() * size_of::<PeakColumn>()).sum()
    }

    /// Divides `frames` into `columns` equal columns and returns the range
    /// of the signal in each.
    ///
    /// Each answer bounds the true minimum and maximum of its column. It is
    /// read from whole pyramid entries, so it may also take in up to one
    /// entry's worth of neighbouring frames on either side: less than a
    /// column's width whenever a column spans at least [`Peaks::BLOCK`]
    /// frames. The part of `frames` beyond the end of the audio, and columns
    /// of an empty range, are silent.
    pub fn columns(&self, frames: Range<usize>, columns: usize) -> Vec<PeakColumn> {
        let frames = clamp(frames, self.frames);
        let per_column = frames.len() / columns.max(1);
        // The coarsest level whose entries are no wider than a column.
        let mut level = 0;
        let mut block = Self::BLOCK;
        while level + 1 < self.levels.len() && block * Self::FANOUT <= per_column {
            level += 1;
            block *= Self::FANOUT;
        }
        let entries = &self.levels[level];
        if entries.is_empty() {
            return vec![PeakColumn::SILENT; columns];
        }

        (0..columns)
            .map(|column| {
                let span = column_span(&frames, columns, column);
                if span.is_empty() {
                    return PeakColumn::SILENT;
                }
                let mut merged = PeakColumn::NOTHING;
                let last = ((span.end - 1) / block).min(entries.len() - 1);
                entries[(span.start / block).min(last)..=last].iter().for_each(|entry| merged.include(entry));
                merged.or_silent()
            })
            .collect()
    }
}

/// Like [`Peaks::columns`], but read from the samples themselves, so every
/// column is exact. For the deep zooms where a column covers fewer than
/// [`Peaks::BLOCK`] frames and touching each sample is cheap.
pub fn scan_columns(buffer: &AudioBuffer, frames: Range<usize>, columns: usize) -> Vec<PeakColumn> {
    let frames = clamp(frames, buffer.frames());
    (0..columns)
        .map(|column| {
            let span = column_span(&frames, columns, column);
            summarise(&buffer.left[span.clone()], &buffer.right[span]).or_silent()
        })
        .collect()
}

/// Samples handled together by [`summarise`]. Keeping a separate running
/// minimum and maximum per lane lets the compiler use vector instructions,
/// which it will not do for a single running value because that would
/// reorder floating-point comparisons.
const LANES: usize = 8;

/// The range of each channel and of their mix over two equally long slices.
fn summarise(left: &[f32], right: &[f32]) -> PeakColumn {
    let (left_lanes, left_rest) = left.as_chunks::<LANES>();
    let (right_lanes, right_rest) = right.as_chunks::<LANES>();
    // Index 0 is the left channel, 1 the right, 2 the mix.
    let mut low = [[f32::INFINITY; LANES]; 3];
    let mut high = [[f32::NEG_INFINITY; LANES]; 3];
    for (l, r) in left_lanes.iter().zip(right_lanes) {
        let mut mixed = [0.0; LANES];
        for ((mixed, l), r) in mixed.iter_mut().zip(l).zip(r) {
            *mixed = (l + r) * 0.5;
        }
        widen(&mut low[0], &mut high[0], l);
        widen(&mut low[1], &mut high[1], r);
        widen(&mut low[2], &mut high[2], &mixed);
    }

    let finish = |channel: usize, rest: &mut dyn Iterator<Item = f32>| {
        let mut range = MinMax::NOTHING;
        for (&min, &max) in low[channel].iter().zip(&high[channel]) {
            range.include(MinMax { min, max });
        }
        rest.for_each(|sample| range.include_sample(sample));
        range
    };
    PeakColumn {
        left: finish(0, &mut left_rest.iter().copied()),
        right: finish(1, &mut right_rest.iter().copied()),
        mixed: finish(2, &mut left_rest.iter().zip(right_rest).map(|(l, r)| (l + r) * 0.5)),
    }
}

fn widen(low: &mut [f32; LANES], high: &mut [f32; LANES], samples: &[f32; LANES]) {
    for ((low, high), &sample) in low.iter_mut().zip(high.iter_mut()).zip(samples) {
        *low = if sample < *low { sample } else { *low };
        *high = if sample > *high { sample } else { *high };
    }
}

/// `range` cut down to the part that lies inside `0..frames`.
fn clamp(range: Range<usize>, frames: usize) -> Range<usize> {
    let end = range.end.min(frames);
    range.start.min(end)..end
}

/// The frames column `column` of `columns` covers when `frames` is divided
/// evenly. When there are more columns than frames, each column still shows
/// the one frame it falls on, so a deep zoom draws steps rather than gaps.
fn column_span(frames: &Range<usize>, columns: usize, column: usize) -> Range<usize> {
    let edge = |index: usize| frames.start + (index as u128 * frames.len() as u128 / columns as u128) as usize;
    let start = edge(column);
    start..edge(column + 1).max(start + 1).min(frames.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise in -1..1 (a linear congruential generator).
    fn noise(frames: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..frames)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn noisy_buffer(frames: usize) -> AudioBuffer {
        AudioBuffer { sample_rate: 48_000.0, left: noise(frames, 1), right: noise(frames, 2) }
    }

    fn true_range(samples: impl Iterator<Item = f32>) -> MinMax {
        let mut range = MinMax::NOTHING;
        samples.for_each(|sample| range.include_sample(sample));
        range
    }

    fn bounds(outer: MinMax, inner: MinMax) -> bool {
        outer.min <= inner.min && outer.max >= inner.max
    }

    #[test]
    fn columns_bound_the_true_range_at_every_zoom() {
        let frames = 200_003;
        let buffer = noisy_buffer(frames);
        let peaks = Peaks::build(&buffer);
        assert_eq!(peaks.frames(), frames);

        let views = [
            (0..frames, 1),
            (0..frames, 7),
            (0..frames, 800),
            (0..frames, 3_000),
            (1_234..150_000, 640),
            (99_000..101_000, 500),
            (5_000..5_300, 300),
            (17..18, 4),
            (frames - 10..frames + 500, 16),
        ];
        for (range, columns) in views {
            let found = peaks.columns(range.clone(), columns);
            assert_eq!(found.len(), columns);
            let clamped = clamp(range.clone(), frames);
            for (index, column) in found.iter().enumerate() {
                let span = column_span(&clamped, columns, index);
                if span.is_empty() {
                    assert_eq!(*column, PeakColumn::SILENT);
                    continue;
                }
                let (left, right) = (&buffer.left[span.clone()], &buffer.right[span.clone()]);
                let mixed = left.iter().zip(right).map(|(l, r)| (l + r) * 0.5);
                let context = format!("{range:?} in {columns} columns, column {index}");
                assert!(bounds(column.left, true_range(left.iter().copied())), "left of {context}");
                assert!(bounds(column.right, true_range(right.iter().copied())), "right of {context}");
                assert!(bounds(column.mixed, true_range(mixed)), "mix of {context}");
            }
        }
    }

    #[test]
    fn columns_wider_than_a_block_are_tight_on_block_boundaries() {
        // A spike in one block must not leak into columns that end before it.
        let mut buffer = AudioBuffer { sample_rate: 48_000.0, left: vec![0.0; 40_960], right: vec![0.0; 40_960] };
        buffer.left[20_480] = 1.0;
        let peaks = Peaks::build(&buffer);
        let columns = peaks.columns(0..40_960, 40);
        let loud: Vec<usize> = columns.iter().enumerate().filter(|(_, c)| c.left.max > 0.0).map(|(i, _)| i).collect();
        assert_eq!(loud, [20]);
        assert_eq!(columns[20].mixed, MinMax { min: 0.0, max: 0.5 });
        assert_eq!(columns[20].right, MinMax::SILENT);
    }

    #[test]
    fn scanning_the_samples_is_exact() {
        let buffer = noisy_buffer(10_000);
        let columns = scan_columns(&buffer, 100..612, 64);
        for (index, column) in columns.iter().enumerate() {
            let span = 100 + index * 8..108 + index * 8;
            assert_eq!(column.left, true_range(buffer.left[span.clone()].iter().copied()));
            assert_eq!(column.right, true_range(buffer.right[span].iter().copied()));
        }
        // More columns than frames: each column shows the frame it falls on.
        let zoomed = scan_columns(&buffer, 10..12, 4);
        assert_eq!(zoomed[0].left, MinMax { min: buffer.left[10], max: buffer.left[10] });
        assert_eq!(zoomed[3].left, MinMax { min: buffer.left[11], max: buffer.left[11] });
    }

    #[test]
    fn empty_and_odd_inputs_are_silent_not_panics() {
        let empty = Peaks::build(&AudioBuffer::default());
        assert_eq!(empty.frames(), 0);
        assert_eq!(empty.columns(0..100, 3), vec![PeakColumn::SILENT; 3]);
        assert!(scan_columns(&AudioBuffer::default(), 0..100, 2).iter().all(|c| *c == PeakColumn::SILENT));

        let peaks = Peaks::build(&noisy_buffer(1_000));
        assert!(peaks.columns(0..1_000, 0).is_empty());
        assert_eq!(peaks.columns(5_000..9_000, 2), vec![PeakColumn::SILENT; 2]);
        #[allow(clippy::reversed_empty_ranges)]
        let backwards = 900..100;
        assert_eq!(peaks.columns(backwards, 2), vec![PeakColumn::SILENT; 2]);

        // NaNs are skipped rather than spreading.
        let buffer = AudioBuffer { sample_rate: 48_000.0, left: vec![f32::NAN, 0.25], right: vec![f32::NAN; 2] };
        let column = Peaks::build(&buffer).columns(0..2, 1)[0];
        assert_eq!(column.left, MinMax { min: 0.25, max: 0.25 });
        assert_eq!(column.right, MinMax::SILENT);
    }

    #[test]
    fn a_five_minute_file_builds_well_under_a_tenth_of_a_second() {
        let frames = 48_000 * 300;
        let buffer = noisy_buffer(frames);
        // The best of a few runs: what the build costs, not what else the
        // machine happened to be doing during one of them.
        let (elapsed, peaks) = (0..5)
            .map(|_| {
                let started = std::time::Instant::now();
                let peaks = Peaks::build(&buffer);
                (started.elapsed(), peaks)
            })
            .min_by_key(|(elapsed, _)| *elapsed)
            .unwrap();
        println!(
            "peaks for {frames} stereo frames: {elapsed:?}, {} bytes ({:.2} % of the audio)",
            peaks.memory_bytes(),
            100.0 * peaks.memory_bytes() as f64 / (frames * 8) as f64
        );
        assert!(elapsed.as_millis() < 100, "took {elapsed:?}");

        let started = std::time::Instant::now();
        let columns = peaks.columns(0..frames, 2_000);
        println!("2000 columns over the whole file: {:?}", started.elapsed());
        assert_eq!(columns.len(), 2_000);
    }
}
