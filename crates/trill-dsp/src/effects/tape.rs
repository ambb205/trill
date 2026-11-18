//! Tape stop: the sound slowed down as a tape or a record is when it is
//! stopped.
//!
//! What comes in is kept in a memory and read from it once, at the speed
//! that is set. Read slower than it came in, the reader falls behind, and
//! pitch and speed fall together: no moment of the sound is played twice
//! and none is left out, which is what tells a tape from a pitch shifter
//! that keeps a sound's length by playing it in overlapping pieces.
//!
//! At a speed of one the device is out of the sound altogether. When the
//! speed leaves one the reader comes in beside what comes in, a few frames
//! behind it, and takes over within a few milliseconds; when the speed is
//! back at one what the reader had not yet played is let go of, and the
//! sound is what comes in again, joined the same way. A reader brought to
//! a full stop is silent, and one set going again from a stop begins from
//! what comes in at that moment.

use crate::define_params;
use crate::params::{ParamValues, float};
use crate::util::{Biquad, Ramp};
use std::f32::consts::{FRAC_1_SQRT_2, PI};
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

define_params!(P, PARAMS, [
    Speed => float(
        "speed",
        "Speed (1 = the sound as it comes in, the device out of the way; 0.5 = half speed and an octave down; 0 = \
         stopped, which is silence. Made to be automated: a line from 1 down to 0 is a tape stop, a step back to 1 \
         brings the sound back)",
        0.0,
        1.0,
        1.0,
        ""
    ),
]);

pub const DESCRIPTION: &str = "Tape stop: slows the sound down as a tape or a record slows when it is stopped. Pitch \
and speed fall together and every moment of the sound is played once, so it is one sound sinking, without the grain \
a pitch shifter gives. This is the device for \"the song slows to a stop\", a tape stop, a record winding down, a \
power-down into a break: put it on the track that is to slow, or on the master for the whole song. Its one control, \
`speed`, is made to be automated. At 1 the device is out of the sound (what comes out is what went in, to the \
sample); 0.5 is half speed and an octave down; 0 is a full stop, which is silence. For a stop: hold 1 until it \
begins, draw a line down to 0 over one or two beats (a straight line is the classic sound: the pitch sinks slowly \
at first and plunges at the end), hold 0 for as long as the silence lasts, and step back to 1 where the sound is to \
return: it is back within a hundredth of a second and in time with the song, what was not played being let go of. \
A line from 0 up to 1 is a tape starting: it plays the sound as it then comes in at the rising speed and joins the \
song when it reaches 1, skipping what it fell behind by (half the length of a straight start, so make a start a \
whole number of beats and the skip lands on the beat). While it is slowed it falls behind the song by what it has \
not played, 20 s at the most (a straight stop drawn over 40 s, or half speed held for 40 s); further than that it \
fades to silence until `speed` is at 0 or back at 1. Under a tenth of full speed its level falls with the speed, \
and what sinks under 30 Hz is let go of, as a tape machine's playback does. It is not for making a part slower and \
keeping its pitch (change the song's tempo: `set_tempo` with `ramp` is a ritardando), nor for changing a pitch and \
keeping the length (that is `builtin:shifter`).";

/// How far behind what comes in the reader can fall, in seconds: what the
/// memory holds.
const BEHIND_SECONDS: f32 = 20.0;
/// Frames a change of speed takes: the stretch an envelope holds a value
/// for, so that a speed that is drawn moves in a line from one of its
/// values to the next and the pitch falls without steps.
const GLIDE_FRAMES: u32 = 256;
/// How long the reader takes to come into the sound when a slow-down
/// begins, and what comes in to come back when it ends, in seconds.
const JOIN_SECONDS: f32 = 0.005;
/// A speed this near to one is one.
const AT_SPEED: f32 = 0.999_999;
/// Under this speed the level falls with the speed, to nothing at a stop:
/// what a playback head gives is as strong as the tape is fast.
const FADES_UNDER: f32 = 0.1;
/// What a slowed sound sinks under is let go of: a low cut of 12 dB an
/// octave here, which is whole once the speed is this far under one. At a
/// speed of one there is none of it, so that the reader comes in as the
/// sound it comes in beside.
const RUMBLE_HZ: f32 = 30.0;
const RUMBLE_WHOLE_BY: f32 = 0.25;

/// Reading between frames: a windowed sinc over this many frames on each
/// side of the place read. With the window below, what a frame's worth of
/// sound is turned into between the frames is 80 dB under it for anything
/// below a third of the sample rate.
const HALF: usize = 8;
const TAPS: usize = 2 * HALF;
/// The places between two frames the sinc is worked out for; between
/// those, a line.
const PHASES: usize = 256;
/// The Kaiser window's shape.
const BETA: f64 = 8.0;

/// The modified Bessel function of the first kind, of order zero.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..64 {
        term *= (x / 2.0 / f64::from(k)).powi(2);
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

/// The weights of the [`TAPS`] frames around a place read, for each of
/// `PHASES + 1` places from one frame to the next: row `p` is the place
/// `p / PHASES` of the way from the frame at tap `HALF - 1` to the one
/// after it. Each row adds up to one, and the first and the last are the
/// frame itself.
fn kernel() -> Box<[f32]> {
    let mut table = vec![0.0f32; (PHASES + 1) * TAPS];
    let top = bessel_i0(BETA);
    for (phase, row) in table.chunks_mut(TAPS).enumerate() {
        let place = phase as f64 / PHASES as f64;
        let mut weights = [0.0f64; TAPS];
        for (tap, weight) in weights.iter_mut().enumerate() {
            // How far this frame is from the place read, in frames.
            let x = tap as f64 - (HALF as f64 - 1.0) - place;
            let reach = x / HALF as f64;
            let window = if reach.abs() >= 1.0 { 0.0 } else { bessel_i0(BETA * (1.0 - reach * reach).sqrt()) / top };
            let angle = std::f64::consts::PI * x;
            let sinc = if angle.abs() < 1e-9 { 1.0 } else { angle.sin() / angle };
            *weight = window * sinc;
        }
        let sum: f64 = weights.iter().sum();
        for (out, weight) in row.iter_mut().zip(weights) {
            *out = (weight / sum) as f32;
        }
    }
    table.into_boxed_slice()
}

pub struct TapeStop {
    params: ParamValues,
    sample_rate: f32,
    /// What came in, the last [`BEHIND_SECONDS`] of it and a little more.
    left: Vec<f32>,
    right: Vec<f32>,
    /// Where the next frame is written.
    write: usize,
    kernel: Box<[f32]>,
    /// The speed, on its way to where it is set.
    speed: Ramp,
    /// False until the first block: that one starts where the speed is.
    started: bool,
    /// A slow-down is on: the reader is in the sound.
    engaged: bool,
    /// The speed is back at one: the sound is being handed back to what
    /// comes in, and will be whatever the speed does meanwhile.
    returning: bool,
    /// The reader has been brought to a full stop.
    stopped: bool,
    /// How far behind the frame written last the reader is, in frames.
    behind: f64,
    /// The most that may be.
    most_behind: f64,
    /// How far the reader has come into the sound, 0..=1, along the join.
    join: f32,
    join_step: f32,
    /// The level of a reader that has run out of memory: 1 until then.
    held: f32,
    rumble: [Biquad; 2],
}

impl TapeStop {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            sample_rate: 48_000.0,
            left: vec![0.0; 4 * TAPS],
            right: vec![0.0; 4 * TAPS],
            write: 0,
            kernel: kernel(),
            speed: Ramp::new(1.0),
            started: false,
            engaged: false,
            returning: false,
            stopped: false,
            behind: HALF as f64,
            most_behind: TAPS as f64,
            join: 0.0,
            join_step: 1.0,
            held: 1.0,
            rumble: [Biquad::default(); 2],
        }
    }

    /// Brings the reader in, as near to what comes in as it can read.
    fn engage(&mut self) {
        self.engaged = true;
        self.returning = false;
        self.join = 0.0;
        self.seat();
    }

    /// Puts the reader at what comes in now, with all its memory before
    /// it again.
    fn seat(&mut self) {
        self.stopped = false;
        self.behind = HALF as f64;
        self.held = 1.0;
        for filter in &mut self.rumble {
            filter.reset();
        }
    }

    /// The sound `behind` frames before the frame at `newest`, between
    /// frames. `behind` is [`HALF`] at the least, so that there are frames
    /// on both sides of the place.
    #[inline]
    fn read(&self, newest: usize, behind: f64) -> (f32, f32) {
        let whole = behind as usize;
        let part = (behind - whole as f64) as f32;
        // The place is `place` of the way from the frame `back` frames
        // ago to the one after it.
        let (back, place) = if part > 0.0 { (whole + 1, 1.0 - part) } else { (whole, 0.0) };
        let place = place * PHASES as f32;
        let row = (place as usize).min(PHASES - 1);
        let between = place - row as f32;
        let (near, far) = self.kernel[row * TAPS..(row + 2) * TAPS].split_at(TAPS);
        let len = self.left.len();
        // The oldest frame the weights reach.
        let mut index = (newest + 2 * len - back - (HALF - 1)) % len;
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for (near, far) in near.iter().zip(far) {
            let weight = near + between * (far - near);
            left += weight * self.left[index];
            right += weight * self.right[index];
            index += 1;
            if index == len {
                index = 0;
            }
        }
        (left, right)
    }
}

impl Default for TapeStop {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProcessor for TapeStop {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate as f32;
        let sr = self.sample_rate;
        let join = (JOIN_SECONDS * sr).round().max(1.0);
        self.join_step = 1.0 / join;
        self.most_behind = f64::from(BEHIND_SECONDS * sr);
        // The memory, and room for the frames the weights reach on either
        // side of the furthest place.
        let frames = self.most_behind as usize + 4 * TAPS;
        self.left = vec![0.0; frames];
        self.right = vec![0.0; frames];
        self.write = 0;
        let (low_cut, q) = (RUMBLE_HZ, FRAC_1_SQRT_2);
        for filter in &mut self.rumble {
            filter.set_high_pass(low_cut, q, sr);
        }
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
        let target = self.params.get(P::Speed as usize);
        if !self.started {
            // A song that begins slowed begins so, with nothing to join.
            self.started = true;
            self.speed.snap(target);
            if target < AT_SPEED {
                self.engage();
                self.join = 1.0;
                self.stopped = target == 0.0;
            }
        }
        self.speed.aim(target, GLIDE_FRAMES);
        let len = self.left.len();
        // Where the reader runs out of memory, less the frames it takes
        // to let its sound go.
        let nearly_out = self.most_behind - f64::from(1.0 / self.join_step);

        for (left, right) in left.iter_mut().zip(right.iter_mut()) {
            let (in_left, in_right) = (*left, *right);
            let newest = self.write;
            self.left[newest] = in_left;
            self.right[newest] = in_right;
            self.write += 1;
            if self.write == len {
                self.write = 0;
            }
            let speed = self.speed.tick();
            if !self.engaged {
                if target >= AT_SPEED {
                    // Out of the sound: what came in goes out as it is.
                    continue;
                }
                self.engage();
            }
            if target >= AT_SPEED {
                self.returning = true;
            }

            if speed == 0.0 {
                self.stopped = true;
            } else if self.stopped {
                // Set going again from a stop, where nothing is heard: it
                // begins from what comes in now.
                self.seat();
            }
            let (mut out_left, mut out_right) = (0.0, 0.0);
            if !self.stopped && self.held > 0.0 {
                // What comes in moved on by a frame, the reader by `speed`
                // of one.
                self.behind += f64::from(1.0 - speed);
                if self.behind > nearly_out {
                    self.held = (self.held - self.join_step).max(0.0);
                }
                let (read_left, read_right) = self.read(newest, self.behind);
                let cut = ((1.0 - speed) / RUMBLE_WHOLE_BY).min(1.0);
                let level = (speed / FADES_UNDER).min(1.0) * self.held;
                out_left = (read_left + cut * (self.rumble[0].tick(read_left) - read_left)) * level;
                out_right = (read_right + cut * (self.rumble[1].tick(read_right) - read_right)) * level;
            }

            // The reader's share of what goes out: a raised cosine along
            // the join, so that neither end of it is a corner.
            let towards = if self.returning { 0.0 } else { 1.0 };
            if self.join < towards {
                self.join = (self.join + self.join_step).min(1.0);
            } else if self.join > towards {
                self.join = (self.join - self.join_step).max(0.0);
            }
            let share = 0.5 - 0.5 * (PI * self.join).cos();
            *left = in_left * (1.0 - share) + out_left * share;
            *right = in_right * (1.0 - share) + out_right * share;
            if self.returning && self.join <= 0.0 {
                self.engaged = false;
                self.returning = false;
            }
        }
    }

    fn reset(&mut self) {
        // A reader that comes in after this reads no further back than
        // the frames its weights reach behind where it is seated: those
        // are all that has to be forgotten.
        let len = self.left.len();
        for back in 1..=(2 * TAPS).min(len) {
            let index = (self.write + len - back) % len;
            self.left[index] = 0.0;
            self.right[index] = 0.0;
        }
        self.engaged = false;
        self.returning = false;
        self.join = 0.0;
        self.started = false;
        self.seat();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_weights_between_frames_are_a_frame_at_either_end_and_add_up_to_one() {
        let table = kernel();
        let row = |phase: usize| &table[phase * TAPS..(phase + 1) * TAPS];
        for (phase, at) in [(0, HALF - 1), (PHASES, HALF)] {
            for (tap, weight) in row(phase).iter().enumerate() {
                let wanted = if tap == at { 1.0 } else { 0.0 };
                assert!((weight - wanted).abs() < 1e-6, "phase {phase}, tap {tap}: {weight}");
            }
        }
        for phase in 0..=PHASES {
            let sum: f32 = row(phase).iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "phase {phase}: {sum}");
        }
        // Half way between two frames the two nearest weigh the same, and
        // the rest fall away on both sides alike.
        let middle = row(PHASES / 2);
        for tap in 0..HALF {
            assert!((middle[tap] - middle[TAPS - 1 - tap]).abs() < 1e-6);
        }
        assert!(middle[HALF - 1] > 0.6 && middle[0].abs() < 1e-3);
    }
}
