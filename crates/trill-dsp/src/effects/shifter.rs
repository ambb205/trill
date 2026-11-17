//! Pitch shifter: changes the pitch of whatever is played into it, by an
//! amount that can be moved while it plays.
//!
//! The sound is kept in a short memory and read back from it faster or
//! slower than it came in, by two readers half a window apart. A reader
//! that reads slower falls behind, and one that reads faster catches up
//! with the present: before either happens it fades out, jumps back to
//! where it started and fades in again, while the other, half a window
//! on, is at its loudest. Their two fades always add up to one.
//!
//! Where a reader lands when it jumps is chosen: within a fiftieth of a
//! second of where it would land, it looks for the place where the sound
//! is most like what the other reader is playing at that moment, so that
//! the two are in step while they overlap. For a single note that means
//! the jumps are whole periods and nothing cancels; for a chord it is the
//! best there is.

use crate::define_params;
use crate::params::{ParamValues, float};
use crate::util::{Biquad, Smooth, butterworth_qs, crossfade_gains};
use std::f32::consts::TAU;
use trill_core::device::{DeviceProcessor, Event, ProcessInfo};

define_params!(P, PARAMS, [
    Pitch => float(
        "pitch",
        "Pitch (semitones up or down. It can be moved while it plays: a line from 0 to -24 over a beat is a dive)",
        -24.0,
        24.0,
        0.0,
        "st"
    ),
    Fine => float("fine", "Fine Tune", -100.0, 100.0, 0.0, "cents"),
    Window => float(
        "window",
        "Window (the length of the pieces the sound is read in: short keeps hits tight, long keeps low and held \
         notes smooth. The shifted sound is half of it late)",
        10.0,
        MAX_WINDOW_MS as f64,
        60.0,
        "ms"
    ),
    Mix => float("mix", "Mix (0 = the sound as it came in, 1 = only the shifted sound)", 0.0, 1.0, 1.0, ""),
]);

pub const DESCRIPTION: &str = "Pitch shifter: moves the pitch of any sound, a recorded or sampled part as well as a \
synth, up or down by as much as two octaves, keeping its length. `pitch` and `mix` can be automated: with `mix` \
opened at a stop and `pitch` drawn from 0 down to -24 over a beat, a held chord melts away. At a pitch of 0 it is \
its input, half a `window` late. Shifted far, a sound takes on a grain of its own; for a part that is to stay in \
tune by a few semitones, transposing its notes or its clip is cleaner.";

const MAX_WINDOW_MS: f32 = 200.0;
/// How far from where it would land a reader may jump to, to land where
/// the sound is in step with the other reader: a period of 50 Hz.
const SEARCH_SECONDS: f32 = 0.02;
/// How much sound is compared to decide that.
const COMPARE_SECONDS: f32 = 0.008;
/// The rate the comparison is made at, whatever the device runs at: the
/// places are then found exactly, around the best of these.
const COMPARE_RATE: f32 = 24_000.0;
/// The least a reader is behind the present, so that there is sound on
/// both sides of where it reads.
const GUARD: f32 = 3.0;
/// How long a change of pitch or of mix takes, in seconds.
const GLIDE_SECONDS: f32 = 0.008;
/// Frames between two readings of the pitch.
const STEP: usize = 16;
/// A ratio this near to one is no shift: the readers then come to rest
/// with one of them alone, at this share of the speed of sound in the
/// memory, which is a few cents of detuning for a moment.
const AT_REST: f32 = 1e-4;
const SETTLE: f32 = 0.05;
/// Reading faster than the sound came in folds what is above half the
/// sample rate back into it: from this ratio on, the sound is filtered
/// before it is kept.
const FOLDS_FROM: f32 = 1.02;
/// Biquads in that filter: 48 dB an octave.
const FILTER_STAGES: usize = 4;

/// A memory of the last stretch of a signal, read anywhere in it.
struct Memory {
    buffer: Vec<f32>,
    mask: usize,
    write: usize,
}

impl Memory {
    fn new() -> Self {
        Self { buffer: vec![0.0; 8], mask: 7, write: 0 }
    }

    /// Room for `frames` frames. Not for the audio thread.
    fn resize(&mut self, frames: usize) {
        let size = (frames + 8).next_power_of_two();
        self.buffer = vec![0.0; size];
        self.mask = size - 1;
        self.write = 0;
    }

    #[inline]
    fn push(&mut self, sample: f32) {
        self.buffer[self.write] = sample;
        self.write = (self.write + 1) & self.mask;
    }

    /// The sample `delay` frames before the one pushed last (0 is that
    /// one).
    #[inline]
    fn at(&self, delay: usize) -> f32 {
        self.buffer[self.write.wrapping_sub(1 + delay) & self.mask]
    }

    /// The signal `delay` frames before the sample pushed last, between
    /// samples: a cubic through the four around it.
    #[inline]
    fn read(&self, delay: f32) -> f32 {
        let whole = delay as usize;
        let t = 1.0 - (delay - whole as f32);
        // Older to newer: the place read is `t` of the way from `b` to `c`.
        let a = self.at(whole + 2);
        let b = self.at(whole + 1);
        let c = self.at(whole);
        let d = self.at(whole.saturating_sub(1));
        let slope_b = 0.5 * (c - a);
        let slope_c = 0.5 * (d - b);
        let (t2, t3) = (t * t, t * t * t);
        b * (2.0 * t3 - 3.0 * t2 + 1.0)
            + slope_b * (t3 - 2.0 * t2 + t)
            + c * (3.0 * t2 - 2.0 * t3)
            + slope_c * (t3 - t2)
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
    }
}

pub struct Shifter {
    params: ParamValues,
    sample_rate: f32,
    /// Left, right, and the two together, which is what the readers are
    /// put in step by: one place for both sides, so that a sound stays
    /// where it was between them.
    left: Memory,
    right: Memory,
    both: Memory,
    /// Where the first reader is in its window, 0..=1. The second is half
    /// a window on.
    phase: f32,
    /// How far behind its window each reader landed, in frames.
    offsets: [f32; 2],
    semitones: Smooth,
    dry: Smooth,
    wet: Smooth,
    /// False until the first block: that one starts where the pitch and
    /// the mix are set.
    started: bool,
    /// The pitch as a ratio of frequencies, and what is left of the frames
    /// it stands for.
    ratio: f32,
    ratio_frames: usize,
    /// The filters on what is kept, and the ratio they are set for.
    filters: [[Biquad; FILTER_STAGES]; 2],
    filtered_for: f32,
    search: usize,
    compare: usize,
    stride: usize,
}

impl Shifter {
    pub fn new() -> Self {
        Self {
            params: ParamValues::new(PARAMS),
            sample_rate: 48_000.0,
            left: Memory::new(),
            right: Memory::new(),
            both: Memory::new(),
            phase: 0.5,
            offsets: [0.0; 2],
            semitones: Smooth::new(0.0),
            dry: Smooth::new(0.0),
            wet: Smooth::new(1.0),
            started: false,
            ratio: 1.0,
            ratio_frames: 0,
            filters: Default::default(),
            filtered_for: 1.0,
            search: 1,
            compare: 1,
            stride: 1,
        }
    }

    fn pitch(&self) -> f32 {
        self.params.get(P::Pitch as usize) + self.params.get(P::Fine as usize) * 0.01
    }

    fn window(&self) -> f32 {
        self.params.get(P::Window as usize) * 0.001 * self.sample_rate
    }

    /// Where each reader is in its window.
    fn phases(&self) -> [f32; 2] {
        [self.phase, (self.phase + 0.5).fract()]
    }

    /// Sets the filters on what is kept for a pitch ratio: out of the way
    /// unless the sound is read faster than it came in.
    fn filter_for(&mut self, ratio: f32) {
        self.filtered_for = ratio;
        let qs = butterworth_qs(FILTER_STAGES);
        for side in &mut self.filters {
            for (stage, filter) in side.iter_mut().enumerate() {
                if ratio > FOLDS_FROM {
                    filter.set_low_pass(0.45 * self.sample_rate / ratio, qs[stage], self.sample_rate);
                } else {
                    filter.set_bypass();
                }
            }
        }
    }

    /// Lands reader `index`, which has just jumped to `phase`, where the
    /// sound is most in step with what the other reader is playing.
    fn land(&mut self, index: usize, phase: f32, window: f32) {
        let other = 1 - index;
        let playing = (GUARD + self.phases()[other] * window + self.offsets[other]) as usize;
        let landing = (GUARD + phase * window) as usize;
        let points = self.compare / self.stride;
        let heard = |delay: usize, point: usize| self.both.at(delay + point * self.stride);
        let loud: f32 = (0..points).map(|point| heard(playing, point).powi(2)).sum();
        // In silence there is nothing to be in step with.
        if loud < 1e-9 {
            self.offsets[index] = 0.0;
            return;
        }
        let alike = |offset: usize| {
            let (mut with, mut own) = (0.0f32, 1e-9f32);
            for point in 0..points {
                let (a, b) = (heard(playing, point), heard(landing + offset, point));
                with += a * b;
                own += b * b;
            }
            with / own.sqrt()
        };
        let mut best = (f32::MIN, 0);
        for offset in (0..=self.search).step_by(self.stride) {
            let score = alike(offset);
            if score > best.0 {
                best = (score, offset);
            }
        }
        // To the frame, around the best of the coarse places.
        let around = best.1.saturating_sub(self.stride - 1)..=(best.1 + self.stride - 1).min(self.search);
        for offset in around {
            let score = alike(offset);
            if score > best.0 {
                best = (score, offset);
            }
        }
        self.offsets[index] = best.1 as f32;
    }
}

impl Default for Shifter {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProcessor for Shifter {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.sample_rate = sample_rate as f32;
        self.search = ((SEARCH_SECONDS * self.sample_rate) as usize).max(1);
        self.compare = ((COMPARE_SECONDS * self.sample_rate) as usize).max(1);
        self.stride = ((self.sample_rate / COMPARE_RATE).round() as usize).max(1);
        let longest = (MAX_WINDOW_MS * 0.001 * self.sample_rate) as usize + self.search + self.compare + 16;
        for memory in [&mut self.left, &mut self.right, &mut self.both] {
            memory.resize(longest);
        }
        for smooth in [&mut self.semitones, &mut self.dry, &mut self.wet] {
            smooth.set_time(GLIDE_SECONDS, self.sample_rate);
        }
        self.started = false;
        self.reset();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        self.params.set(index, value);
    }

    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], right: &mut [f32]) {
        let pitch = self.pitch();
        let window = self.window();
        let mix = self.params.get(P::Mix as usize);
        let (dry, wet) = if mix >= 1.0 { (0.0, 1.0) } else { crossfade_gains(mix) };
        if !self.started {
            self.started = true;
            self.semitones.snap(pitch);
            self.dry.snap(dry);
            self.wet.snap(wet);
        }

        for (left, right) in left.iter_mut().zip(right.iter_mut()) {
            let semitones = self.semitones.next(pitch);
            if self.ratio_frames == 0 {
                self.ratio_frames = STEP;
                self.ratio = 2f32.powf(semitones / 12.0);
                let folds = self.ratio > FOLDS_FROM;
                let filtering = self.filtered_for > FOLDS_FROM;
                if folds != filtering || (folds && (self.ratio / self.filtered_for - 1.0).abs() > 0.01) {
                    self.filter_for(self.ratio);
                }
            }
            self.ratio_frames -= 1;

            let (in_left, in_right) = (*left, *right);
            let mut kept = [in_left, in_right];
            for (sample, filters) in kept.iter_mut().zip(&mut self.filters) {
                for filter in filters {
                    *sample = filter.tick(*sample);
                }
            }
            self.left.push(kept[0]);
            self.right.push(kept[1]);
            self.both.push(kept[0] + kept[1]);

            // A reader that reads slower than the sound comes in falls
            // behind by the difference; a faster one catches up.
            let shifting = (self.ratio - 1.0).abs() > AT_REST;
            let was = self.phase;
            if shifting {
                let phase = was + (1.0 - self.ratio) / window;
                // A reader that comes to the end of its window jumps to
                // the other end, silent at both: the first at 0 and 1, the
                // second, half a window on, as the first passes the middle.
                if phase >= 1.0 {
                    self.phase = phase - 1.0;
                    self.land(0, 0.0, window);
                } else if phase < 0.0 {
                    self.phase = phase + 1.0;
                    self.land(0, 1.0, window);
                } else {
                    self.phase = phase;
                    if was < 0.5 && phase >= 0.5 {
                        self.land(1, 0.0, window);
                    } else if was >= 0.5 && phase < 0.5 {
                        self.land(1, 1.0, window);
                    }
                }
            } else {
                // At rest: on to where one reader plays alone.
                let nearest = (was * 2.0).round() * 0.5;
                self.phase = was + (nearest - was).clamp(-SETTLE / window, SETTLE / window);
            }

            let dry = self.dry.next(dry);
            let wet = self.wet.next(wet);
            // A mix that is on its way to nothing gets there.
            let wet = if wet < 1e-6 { 0.0 } else { wet };
            let (mut out_left, mut out_right) = (0.0, 0.0);
            if wet > 0.0 {
                for (phase, offset) in self.phases().into_iter().zip(self.offsets) {
                    // A raised cosine over the window: nothing at its ends,
                    // where the reader jumps.
                    let gain = 0.5 - 0.5 * (TAU * phase).cos();
                    if gain > 0.0 {
                        let delay = GUARD + phase * window + offset;
                        out_left += gain * self.left.read(delay);
                        out_right += gain * self.right.read(delay);
                    }
                }
            }
            *left = in_left * dry + out_left * wet;
            *right = in_right * dry + out_right * wet;
        }
    }

    fn reset(&mut self) {
        for memory in [&mut self.left, &mut self.right, &mut self.both] {
            memory.clear();
        }
        for filter in self.filters.iter_mut().flatten() {
            filter.reset();
        }
        self.phase = 0.5;
        self.offsets = [0.0; 2];
        self.ratio_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    const SR: f64 = 48_000.0;

    fn shifter(params: &[(&str, f64)]) -> Shifter {
        let mut device = Shifter::new();
        device.prepare(SR, 512);
        for (id, value) in params {
            let index = PARAMS.iter().position(|def| def.id == *id).unwrap_or_else(|| panic!("no parameter '{id}'"));
            device.set_param(index, *value);
        }
        device
    }

    /// `left` and `right` through the device, in blocks.
    fn through(device: &mut Shifter, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (left.to_vec(), right.to_vec());
        for (left, right) in left.chunks_mut(256).zip(right.chunks_mut(256)) {
            device.process(&info(SR), &[], left, right);
        }
        (left, right)
    }

    fn tone(hz: f64, seconds: f64, level: f32) -> Vec<f32> {
        sine(hz, SR, (seconds * SR) as usize).into_iter().map(|sample| sample * level).collect()
    }

    /// The loudest and the quietest stretch of `frames` frames of
    /// `samples`, in dB against `level`.
    fn steadiness(samples: &[f32], frames: usize, level: f32) -> (f32, f32) {
        let levels: Vec<f32> = samples.chunks_exact(frames).map(|piece| 20.0 * (rms(piece) / level).log10()).collect();
        (levels.iter().copied().fold(f32::MIN, f32::max), levels.iter().copied().fold(f32::MAX, f32::min))
    }

    #[test]
    fn a_note_comes_out_at_the_pitch_asked_for_as_loud_and_as_steady_as_it_went_in() {
        for hz in [82.4, 220.0, 440.0, 1_000.0, 3_000.0] {
            for semitones in [-24.0f64, -12.0, -5.0, 3.0, 7.0, 12.0, 24.0] {
                let input = tone(hz, 2.0, 0.5);
                let (out, _) = through(&mut shifter(&[("pitch", semitones)]), &input, &input);
                let heard = &out[24_000..];
                let wanted = hz * 2f64.powf(semitones / 12.0);
                let cents = 1200.0 * (zero_crossing_hz(heard, SR) / wanted).log2();
                assert!(cents.abs() < 15.0, "{hz} Hz by {semitones}: {cents} cents out");
                // A sine has an rms of 0.707 of its peak. The readers are
                // put in step where they overlap, so nothing cancels: the
                // level stays within a dB and a half of the note's own.
                // Measured over three periods of what comes out, a hundredth
                // of a second at the least.
                let stretch = ((3.0 * SR / wanted) as usize).max(480);
                let (most, least) = steadiness(heard, stretch, 0.5 * std::f32::consts::FRAC_1_SQRT_2);
                assert!(most < 1.0 && least > -1.5, "{hz} Hz by {semitones}: between {least} and {most} dB");
            }
        }
    }

    #[test]
    fn a_pitch_that_is_drawn_downwards_is_followed_down() {
        // Two octaves down in half a second, as a song draws a dive: the
        // device is told a new pitch every block.
        let input = tone(880.0, 1.5, 0.5);
        let mut device = shifter(&[]);
        let (mut left, mut right) = (input.clone(), input.clone());
        for (block, (left, right)) in left.chunks_mut(256).zip(right.chunks_mut(256)).enumerate() {
            let seconds = block as f64 * 256.0 / SR;
            let fallen = ((seconds - 0.5) / 0.5).clamp(0.0, 1.0);
            device.set_param(P::Pitch as usize, -24.0 * fallen);
            device.process(&info(SR), &[], left, right);
        }
        // The pitch a twentieth of a second around a moment, in semitones
        // under where it started. The pitch is the speed of the reading,
        // so it is heard as soon as it is set, but for its short glide.
        let late = 0.008;
        let at = |seconds: f64| {
            let from = ((seconds + late) * SR) as usize;
            12.0 * (zero_crossing_hz(&left[from - 1_200..from + 1_200], SR) / 880.0).log2()
        };
        assert!(at(0.3).abs() < 0.1, "at rest: {}", at(0.3));
        for (seconds, wanted) in [(0.625, -6.0), (0.75, -12.0), (0.875, -18.0)] {
            assert!((at(seconds) - wanted).abs() < 1.5, "at {seconds} s: {} semitones", at(seconds));
        }
        assert!((at(1.3) + 24.0).abs() < 0.1, "and stays down: {}", at(1.3));
        // Nothing clicks on the way: no step larger than the loudest the
        // tone itself makes.
        assert!(largest_step(&left[12_000..]) < 0.5 * 0.2, "{}", largest_step(&left[12_000..]));
    }

    #[test]
    fn at_no_shift_it_is_its_input_half_a_window_late_and_at_no_mix_its_input() {
        let input: Vec<f32> = tone(330.0, 1.0, 0.4).iter().zip(tone(1_234.0, 1.0, 0.3)).map(|(a, b)| a + b).collect();
        let (out, _) = through(&mut shifter(&[("window", 40.0)]), &input, &input);
        // 20 ms and the three frames a reader keeps from the present.
        let late = 960 + 3;
        for frame in (10_000..40_000).step_by(997) {
            assert!((out[frame] - input[frame - late]).abs() < 1e-3, "frame {frame}: {}", out[frame]);
        }
        // Back from a shift, it comes to rest as its input again.
        let mut device = shifter(&[("pitch", -7.0)]);
        through(&mut device, &input, &input);
        device.set_param(P::Pitch as usize, 0.0);
        through(&mut device, &input, &input);
        let (out, _) = through(&mut device, &input, &input);
        let cents = 1200.0 * (zero_crossing_hz(&tone_of(&out[24_000..], 330.0), SR) / 330.0).log2();
        assert!(cents.abs() < 1.0, "{cents} cents");
        let (most, least) = steadiness(&out[24_000..], 4_800, rms(&input));
        assert!(most < 0.2 && least > -0.2, "between {least} and {most} dB");

        let (dry, _) = through(&mut shifter(&[("pitch", -12.0), ("mix", 0.0)]), &input, &input);
        assert_eq!(dry, input, "with no mix it is the input to the bit");
    }

    /// What is in `samples` around `hz`, for a pitch to be read from.
    fn tone_of(samples: &[f32], hz: f32) -> Vec<f32> {
        let mut filter = crate::util::Svf::default();
        filter.set(hz, 8.0, SR as f32);
        samples.iter().map(|sample| filter.tick(*sample).band).collect()
    }

    #[test]
    fn a_sound_stays_on_the_side_it_was_on() {
        let input = tone(440.0, 1.0, 0.5);
        let silence = vec![0.0f32; input.len()];
        let (left, right) = through(&mut shifter(&[("pitch", -12.0)]), &input, &silence);
        assert!(rms(&left[24_000..]) > 0.3 && peak(&right) == 0.0);
        // And a sound in the middle stays the same on both sides.
        let (left, right) = through(&mut shifter(&[("pitch", 5.0)]), &input, &input);
        assert_eq!(left, right);
    }

    #[test]
    fn shifting_up_does_not_fold_the_top_of_the_sound_back_into_it() {
        // 9 kHz an octave up is 18 kHz; 15 kHz would be 30 kHz, which
        // 48 kHz cannot hold: most of it is taken out before it can fold
        // down to 18 kHz.
        let high = tone(15_000.0, 1.0, 0.5);
        let (out, _) = through(&mut shifter(&[("pitch", 12.0)]), &high, &high);
        assert!(rms(&out[24_000..]) < 0.5 * 0.1, "{}", rms(&out[24_000..]));
        let below = tone(9_000.0, 1.0, 0.5);
        let (out, _) = through(&mut shifter(&[("pitch", 12.0)]), &below, &below);
        assert!(rms(&out[24_000..]) > 0.5 * 0.5, "{}", rms(&out[24_000..]));
    }
}
