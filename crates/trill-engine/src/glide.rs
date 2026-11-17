//! A gain that is where its control is, frame by frame: a track's fader and
//! pan, a send, the master's fader.
//!
//! While the song plays, an envelope is read every [`STEP`] frames, counted
//! from where the song last started or jumped and not from the start of a
//! buffer, and between two readings the gain moves in a straight line. So
//! the gain at a frame depends on where the song is and on nothing else:
//! the same song makes the same samples whatever the buffer size, and a
//! render is what was played.
//!
//! What no envelope accounts for is smoothed here as well. When the gain is
//! suddenly somewhere else (a fader moved by hand, a track muted, the song
//! jumping to where the envelope is at another level), the difference is
//! kept and let go of evenly over a few milliseconds, so that nothing
//! clicks. A change the envelope itself makes from one reading to the next
//! is the envelope's: a step written into it is a step, a reading wide.
//!
//! Nothing in here allocates.

/// How often, in frames, an envelope is read for a gain: two thirds of a
/// millisecond at 48 kHz, as often as a clip's controllers are told.
pub(crate) const STEP: u64 = 32;
/// How long an abrupt change of a gain takes to be let go of, in seconds.
pub(crate) const DECLICK_SECONDS: f64 = 0.01;

/// Where a block lies in the song, for what is read from the song's
/// position.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Span {
    /// Whether the song moves through the block.
    pub playing: bool,
    /// The song's position at the block's first frame.
    pub start_beat: f64,
    /// Where the transport is anchored, and the frames played since, at
    /// the block's first frame.
    pub anchor_beat: f64,
    pub elapsed: u64,
    pub beats_per_frame: f64,
}

impl Span {
    /// The song's position `frames` frames after the anchor. Computed as
    /// the start of a block is, so that a reading that falls on a block's
    /// first frame is taken where the block begins.
    pub fn beat_at(&self, frames: u64) -> f64 {
        self.anchor_beat + frames as f64 * self.beats_per_frame
    }

    /// The position of the last frame at or before the block's first that
    /// is a multiple of `step` frames from the anchor; where the song
    /// stands, if it does not move.
    pub fn beat_on_grid(&self, step: u64) -> f64 {
        if self.playing { self.beat_at(self.elapsed - self.elapsed % step) } else { self.start_beat }
    }
}

/// `N` gains that move together (the two sides of a fader with its pan, or
/// one alone), and what it takes to carry them from one block to the next.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Glide<const N: usize> {
    /// Where the control has the gains at the frame after the last one
    /// played.
    expected: [f32; N],
    /// What is left from before the gains last changed abruptly: added on
    /// top of where the control has them, and fading to nothing over
    /// `left` more frames.
    offset: [f32; N],
    left: u32,
    /// False until the first block, which starts where the control is
    /// instead of coming up from somewhere else.
    primed: bool,
}

impl<const N: usize> Default for Glide<N> {
    fn default() -> Self {
        Self { expected: [0.0; N], offset: [0.0; N], left: 0, primed: false }
    }
}

impl<const N: usize> Glide<N> {
    /// Gains that have been at `gains`: the next block goes on from there.
    pub fn from(gains: [f32; N]) -> Self {
        Self { expected: gains, offset: [0.0; N], left: 0, primed: true }
    }

    /// Works out the gains of a block of `frames` frames.
    ///
    /// `target` says where the control has them at a position of the song.
    /// With `moving`, it is asked on the grid and the gains follow it
    /// through the block; without, once, where the block starts: for a
    /// control that nothing moves, or a song that stands still.
    ///
    /// Returns the gains if they are the same throughout the block, which
    /// is nearly always. Otherwise they are in `gains`, frame by frame.
    /// `declick` is how many frames an abrupt change takes.
    pub fn fill(
        &mut self,
        span: &Span,
        moving: bool,
        declick: u32,
        frames: usize,
        gains: &mut [&mut [f32]; N],
        mut target: impl FnMut(f64) -> [f32; N],
    ) -> Option<[f32; N]> {
        const PER_STEP: f32 = 1.0 / STEP as f32;
        let declick = declick.max(1);
        let between = |from: &[f32; N], to: &[f32; N], within: usize| {
            let share = within as f32 * PER_STEP;
            std::array::from_fn(|channel| from[channel] + (to[channel] - from[channel]) * share)
        };

        let mut flat = true;
        let first;
        if moving && span.playing {
            let mut cell = span.elapsed / STEP;
            let mut within = (span.elapsed % STEP) as usize;
            let mut from = target(span.beat_at(cell * STEP));
            let mut to = target(span.beat_at((cell + 1) * STEP));
            first = between(&from, &to, within);
            let mut done = 0;
            while done < frames {
                let run = (STEP as usize - within).min(frames - done);
                for ((out, low), high) in gains.iter_mut().zip(from).zip(to) {
                    let out = &mut out[done..done + run];
                    if low == high {
                        out.fill(low);
                    } else {
                        flat = false;
                        for (index, gain) in out.iter_mut().enumerate() {
                            *gain = low + (high - low) * ((within + index) as f32 * PER_STEP);
                        }
                    }
                }
                done += run;
                within += run;
                if within == STEP as usize {
                    cell += 1;
                    within = 0;
                    from = to;
                    to = target(span.beat_at((cell + 1) * STEP));
                }
            }
            flat &= from == first;
            self.settle(first, declick);
            self.expected = between(&from, &to, within);
        } else {
            first = target(span.start_beat);
            self.settle(first, declick);
            self.expected = first;
        }

        if self.left == 0 {
            return flat.then_some(first);
        }
        if flat {
            for (out, gain) in gains.iter_mut().zip(first) {
                out[..frames].fill(gain);
            }
        }
        // What is left of the last abrupt change, on top.
        let fading = (self.left as usize).min(frames);
        for (out, offset) in gains.iter_mut().zip(self.offset) {
            for (index, gain) in out[..fading].iter_mut().enumerate() {
                *gain += offset * ((self.left - index as u32) as f32 / declick as f32);
            }
        }
        self.left -= fading as u32;
        None
    }

    /// Takes note of where the control has the gains at a block's first
    /// frame. If that is not where they were headed, they have been moved
    /// by something other than the passing of the song: the difference is
    /// kept, to be let go of evenly.
    fn settle(&mut self, now: [f32; N], declick: u32) {
        if !self.primed {
            self.primed = true;
            return;
        }
        if now == self.expected {
            return;
        }
        let carried = self.left as f32 / declick as f32;
        for ((offset, expected), now) in self.offset.iter_mut().zip(self.expected).zip(now) {
            *offset = *offset * carried + (expected - now);
        }
        self.left = declick;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECLICK: u32 = 480;

    /// Plays `total` frames in blocks of `block` and returns every frame's
    /// gain. The song moves a hundredth of a beat a frame.
    fn run(
        glide: &mut Glide<1>,
        start: u64,
        total: usize,
        block: usize,
        moving: bool,
        target: &dyn Fn(f64) -> f32,
    ) -> Vec<f32> {
        let mut out = Vec::with_capacity(total);
        let mut buffer = vec![0.0f32; block];
        let mut elapsed = start;
        while out.len() < total {
            let frames = block.min(total - out.len());
            let span = Span {
                playing: true,
                start_beat: elapsed as f64 * 0.01,
                anchor_beat: 0.0,
                elapsed,
                beats_per_frame: 0.01,
            };
            let mut gains = [&mut buffer[..]];
            match glide.fill(&span, moving, DECLICK, frames, &mut gains, |beat| [target(beat)]) {
                Some([gain]) => out.extend(std::iter::repeat_n(gain, frames)),
                None => out.extend_from_slice(&buffer[..frames]),
            }
            elapsed += frames as u64;
        }
        out
    }

    #[test]
    fn a_gain_follows_its_envelope_the_same_however_the_blocks_fall() {
        // A slope, a step and a flat stretch.
        let envelope = |beat: f64| {
            if beat < 6.0 {
                (beat / 6.0) as f32
            } else if beat < 9.0 {
                0.25
            } else {
                0.9
            }
        };
        let reference = run(&mut Glide::default(), 0, 2_000, 512, true, &envelope);
        for block in [1, 7, 31, 32, 33, 480, 2_000] {
            let gains = run(&mut Glide::default(), 0, 2_000, block, true, &envelope);
            assert!(gains == reference, "blocks of {block} frames gave other gains");
        }
        // On the grid it is where the envelope is, and half way between
        // two readings half way between them.
        assert_eq!(reference[0], 0.0);
        assert_eq!(reference[320], envelope(3.2));
        assert!((reference[336] - (envelope(3.2) + envelope(3.52)) / 2.0).abs() < 1e-7);
        // The step is a reading wide: no more, and nothing before it.
        assert_eq!(reference[576], envelope(5.76));
        assert_eq!(reference[608], 0.25);
        assert!(reference[577..608].windows(2).all(|pair| pair[1] < pair[0]), "it falls evenly to the step");
        assert_eq!(&reference[1_000..1_100], &[0.9; 100]);
    }

    #[test]
    fn a_gain_that_nothing_moves_is_one_number_for_the_block() {
        let mut glide = Glide::<2>::default();
        let span = Span { playing: true, start_beat: 0.0, anchor_beat: 0.0, elapsed: 0, beats_per_frame: 0.01 };
        let (mut left, mut right) = (vec![9.0f32; 64], vec![9.0f32; 64]);
        let mut gains = [&mut left[..], &mut right[..]];
        assert_eq!(glide.fill(&span, false, DECLICK, 64, &mut gains, |_| [0.5, 0.25]), Some([0.5, 0.25]));
        // An envelope that is flat here is one number too.
        assert_eq!(glide.fill(&span, true, DECLICK, 64, &mut gains, |_| [0.5, 0.25]), Some([0.5, 0.25]));
        assert_eq!(left[0], 0.5, "which the buffers may hold, but need not be read for");
    }

    #[test]
    fn a_gain_moved_by_hand_gets_there_evenly_and_without_a_step() {
        let mut glide = Glide::default();
        let before = run(&mut glide, 0, 100, 64, false, &|_| 1.0);
        assert_eq!(before, [1.0; 100], "the first block starts where the control is");
        // The fader is pulled down between two blocks.
        let after = run(&mut glide, 100, 1_000, 64, false, &|_| 0.25);
        assert_eq!(after[0], 1.0, "nothing has moved yet at the first frame");
        assert!(after.windows(2).all(|pair| pair[1] <= pair[0]), "it only goes down");
        let largest = after.windows(2).map(|pair| pair[0] - pair[1]).fold(0.0, f32::max);
        assert!(largest < 0.75 / DECLICK as f32 * 1.01, "evenly: {largest} a frame");
        assert!((after[DECLICK as usize / 2] - 0.625).abs() < 1e-3, "half way there half way through");
        assert_eq!(&after[DECLICK as usize..], &vec![0.25; 1_000 - DECLICK as usize][..]);

        // Moved again before it has arrived, it goes on from where it is.
        let mut glide = Glide::from([1.0]);
        let first = run(&mut glide, 0, 240, 64, false, &|_| 0.0);
        let second = run(&mut glide, 240, 600, 64, false, &|_| 1.0);
        assert!((first[239] - second[0]).abs() < 3e-3, "{} then {}", first[239], second[0]);
        assert!(second.windows(2).all(|pair| pair[1] >= pair[0]));
        assert_eq!(second[599], 1.0);
    }

    #[test]
    fn a_song_that_jumps_takes_its_gain_along_evenly() {
        // The envelope is at 0.2 where the song was and 0.8 where it jumps to.
        let envelope = |beat: f64| if beat < 50.0 { 0.2 } else { 0.8 };
        let mut glide = Glide::default();
        let before = run(&mut glide, 0, 640, 64, true, &envelope);
        assert_eq!(before[639], 0.2);
        let after = run(&mut glide, 6_400, 640, 64, true, &envelope);
        assert!((after[0] - 0.2).abs() < 1e-6, "it is still where it was at the first frame: {}", after[0]);
        assert!(after.windows(2).all(|pair| pair[1] >= pair[0]));
        assert_eq!(after[DECLICK as usize], 0.8);
        // The same jump lands the same however the blocks fall.
        let mut other = Glide::default();
        run(&mut other, 0, 640, 37, true, &envelope);
        assert!(run(&mut other, 6_400, 640, 501, true, &envelope) == after);
    }

    #[test]
    fn a_new_control_can_come_up_from_nothing() {
        let mut glide = Glide::from([0.0]);
        let gains = run(&mut glide, 0, 600, 64, false, &|_| 0.5);
        assert_eq!((gains[0], gains[599]), (0.0, 0.5));
        assert!(gains.windows(2).all(|pair| pair[1] >= pair[0]));
    }

    #[test]
    fn positions_on_the_grid_do_not_depend_on_where_a_block_starts() {
        let span = |elapsed: u64, playing: bool| Span {
            playing,
            start_beat: 3.0 + elapsed as f64 * 0.01,
            anchor_beat: 3.0,
            elapsed,
            beats_per_frame: 0.01,
        };
        assert_eq!(span(700, true).beat_on_grid(256), span(512, true).beat_at(512));
        assert_eq!(span(512, true).beat_on_grid(256), span(767, true).beat_on_grid(256));
        assert_eq!(span(700, false).beat_on_grid(256), 10.0, "a song that stands still is where it stands");
    }
}
