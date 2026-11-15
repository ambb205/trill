//! The time map: which input frame each output frame should sound like.
//!
//! A plain stretch would map time linearly. Here the map is bent around every
//! transient: a short span around each one is *locked*, meaning it advances
//! through the input at exactly the output rate, so the transient is copied
//! rather than stretched and lands where a linear map would have put it. The
//! *elastic* spans in between make up the difference.

/// A transient to lock, with how much around it to protect.
#[derive(Copy, Clone, Debug)]
pub(super) struct Anchor {
    /// Input frame of the transient.
    pub input: usize,
    /// Frames to lock before and after it. Shortened where neighbours are
    /// too close for both to fit.
    pub pre: usize,
    pub post: usize,
}

/// A run of output frames and the input frames they correspond to. Segments
/// tile both the input and the output without gaps.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Segment {
    pub out_start: usize,
    pub out_end: usize,
    pub in_start: usize,
    pub in_end: usize,
    /// Locked segments have equal input and output lengths.
    pub locked: bool,
}

impl Segment {
    /// Input frames consumed per output frame.
    pub fn slope(&self) -> f64 {
        (self.in_end - self.in_start) as f64 / (self.out_end - self.out_start) as f64
    }

    pub fn input_at(&self, out: f64) -> f64 {
        self.in_start as f64 + (out - self.out_start as f64) * self.slope()
    }
}

pub(super) struct TimeMap {
    pub segments: Vec<Segment>,
    in_len: usize,
    out_len: usize,
}

/// Locked spans may take up at most this share of the gap between two
/// anchors (measured on whichever of input and output is shorter), so the
/// elastic span in between never has to stretch or squeeze by more than
/// about three times the overall ratio.
const LOCKED_SHARE: f64 = 0.5;

impl TimeMap {
    /// Builds the map from `in_len` input frames to `out_len` output frames.
    /// `anchors` must be sorted by input position. The first and last
    /// `edge` frames are locked as well, so that the result starts and ends
    /// on exactly the samples the input does and a loop still loops; an
    /// anchor that close to an end is covered by the end's lock instead of
    /// getting its own.
    pub fn new(in_len: usize, out_len: usize, anchors: &[Anchor], edge: usize) -> Self {
        let ratio = out_len as f64 / in_len as f64;
        let output_of = |input: usize| ((input as f64 * ratio).round() as usize).min(out_len);

        // The two ends are anchors too. An onset closer to an end than
        // `edge` takes the end's place.
        // Anchors that rounding has pushed on top of a neighbour are dropped.
        let follows = |earlier: &Anchor, later: &Anchor| {
            later.input > earlier.input && output_of(later.input) > output_of(earlier.input)
        };
        let mut kept = vec![Anchor { input: 0, pre: 0, post: edge }];
        let mut end = Anchor { input: in_len, pre: edge, post: 0 };
        for anchor in anchors.iter().filter(|anchor| anchor.input < in_len) {
            let last = kept.len() - 1;
            if anchor.input + edge >= in_len {
                // Inside the lock at the end: that lock grows to cover it.
                end.pre = end.pre.max(in_len - anchor.input + anchor.pre);
            } else if anchor.input > edge && follows(&kept[last], anchor) {
                kept.push(*anchor);
            } else {
                // Inside the lock at the start (most often a hit on the very
                // first frame), or too close to the anchor before it to tell
                // apart: the earlier lock grows to cover this one too.
                kept[last].post = kept[last].post.max(anchor.input.saturating_sub(kept[last].input) + anchor.post);
            }
        }
        while kept.len() > 1 && !follows(&kept[kept.len() - 1], &end) {
            kept.pop();
        }
        kept.push(end);

        // Fit each pair of neighbouring locks into the gap between them.
        for index in 0..kept.len().saturating_sub(1) {
            let gap_in = kept[index + 1].input - kept[index].input;
            let gap_out = output_of(kept[index + 1].input) - output_of(kept[index].input);
            let budget = (gap_in.min(gap_out) as f64 * LOCKED_SHARE) as usize;
            let wanted = kept[index].post + kept[index + 1].pre;
            if wanted > budget {
                let scale = budget as f64 / wanted as f64;
                kept[index].post = (kept[index].post as f64 * scale) as usize;
                kept[index + 1].pre = (kept[index + 1].pre as f64 * scale) as usize;
            }
        }

        let mut segments = Vec::with_capacity(kept.len() * 2);
        let (mut in_pos, mut out_pos) = (0, 0);
        for point in &kept {
            let output = output_of(point.input);
            let (lock_in, lock_out) = (point.input - point.pre, output - point.pre);
            if lock_out > out_pos {
                segments.push(Segment {
                    out_start: out_pos,
                    out_end: lock_out,
                    in_start: in_pos,
                    in_end: lock_in,
                    locked: false,
                });
            }
            let length = point.pre + point.post;
            if length > 0 {
                segments.push(Segment {
                    out_start: lock_out,
                    out_end: lock_out + length,
                    in_start: lock_in,
                    in_end: lock_in + length,
                    locked: true,
                });
            }
            (in_pos, out_pos) = (lock_in + length, lock_out + length);
        }
        Self { segments, in_len, out_len }
    }

    /// Whether output position `out` lies in a locked segment.
    pub fn locked_at(&self, out: f64) -> bool {
        let index = self.segments.partition_point(|segment| (segment.out_end as f64) <= out);
        self.segments.get(index).is_some_and(|segment| segment.locked && out >= segment.out_start as f64)
    }

    /// The input position that output position `out` corresponds to. Outside
    /// the audio the map continues at the output rate.
    pub fn input_at(&self, out: f64) -> f64 {
        if out <= 0.0 {
            return out;
        }
        if out >= self.out_len as f64 {
            return self.in_len as f64 + (out - self.out_len as f64);
        }
        let index = self.segments.partition_point(|segment| (segment.out_end as f64) <= out);
        self.segments[index].input_at(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_tiling(map: &TimeMap, in_len: usize, out_len: usize) {
        let (mut in_pos, mut out_pos) = (0, 0);
        for segment in &map.segments {
            assert_eq!((segment.in_start, segment.out_start), (in_pos, out_pos));
            assert!(segment.out_end > segment.out_start && segment.in_end > segment.in_start, "{segment:?}");
            if segment.locked {
                assert_eq!(segment.in_end - segment.in_start, segment.out_end - segment.out_start);
            }
            (in_pos, out_pos) = (segment.in_end, segment.out_end);
        }
        assert_eq!((in_pos, out_pos), (in_len, out_len));
    }

    #[test]
    fn anchors_land_where_a_linear_map_puts_them() {
        let anchors: Vec<Anchor> =
            [10_000, 22_000, 22_900, 40_000].iter().map(|&input| Anchor { input, pre: 100, post: 1000 }).collect();
        for out_len in [24_000, 36_000, 48_000, 72_000, 96_000] {
            let map = TimeMap::new(48_000, out_len, &anchors, 150);
            check_tiling(&map, 48_000, out_len);
            let ratio = out_len as f64 / 48_000.0;
            for anchor in &anchors {
                let output = (anchor.input as f64 * ratio).round();
                assert_eq!(map.input_at(output), anchor.input as f64, "out_len {out_len}");
            }
            assert_eq!(map.input_at(0.0), 0.0);
            assert_eq!(map.input_at(out_len as f64), 48_000.0);
            // Around each anchor, and at both ends, the map is locked.
            assert!(map.locked_at(0.0) && map.locked_at(out_len as f64 - 1.0) && !map.locked_at(out_len as f64));
            assert!(anchors.iter().all(|anchor| map.locked_at((anchor.input as f64 * ratio).round())));
            assert!(!map.locked_at((31_000.0 * ratio).round()));
            // Locks leave the elastic parts no more than three times as
            // squeezed or stretched as the whole.
            for segment in map.segments.iter().filter(|segment| !segment.locked) {
                let local = 1.0 / segment.slope();
                assert!(local > ratio / 3.0 && local < ratio * 3.0, "out_len {out_len}: {segment:?}");
            }
            // Monotonic.
            let mut last = -1.0;
            for out in (0..out_len).step_by(97) {
                let input = map.input_at(out as f64);
                assert!(input >= last);
                last = input;
            }
        }
    }

    #[test]
    fn degenerate_inputs_still_tile() {
        // More anchors than output frames, anchors on the edges, no anchors.
        let dense: Vec<Anchor> = (0..200).map(|i| Anchor { input: i * 5, pre: 2, post: 4 }).collect();
        for (in_len, out_len) in [(1000, 30), (1000, 4000), (10, 20), (1, 1), (5, 2)] {
            check_tiling(&TimeMap::new(in_len, out_len, &dense, 3), in_len, out_len);
            check_tiling(&TimeMap::new(in_len, out_len, &[], 3), in_len, out_len);
        }
    }
}
