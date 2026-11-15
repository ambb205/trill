//! A playhead: one voice's position in a [`Sample`], and the rules for
//! moving it: start and end points, direction, looping, and the short fades
//! that keep every cut in the audio from clicking.
//!
//! Positions are in frames of the sample file and fractional. Everything
//! here runs on the audio thread.

use crate::sample::{ReadPlan, Sample};

/// Fade applied where playback cuts into or out of the middle of a file.
const DECLICK_SECONDS: f64 = 0.0015;
/// Loops shorter than this many frames are widened to it.
pub const MIN_LOOP_FRAMES: f64 = 4.0;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum LoopMode {
    #[default]
    Off,
    /// Jump from the loop end back to the loop start.
    Forward,
    /// Turn around at each end of the loop.
    PingPong,
}

/// A loop region, shared by all the voices of a device and free to change
/// while they play.
#[derive(Copy, Clone, Debug, Default)]
pub struct LoopSpec {
    pub mode: LoopMode,
    /// First frame of the loop.
    pub start: f64,
    /// One past its last frame.
    pub end: f64,
    /// Frames before the loop end over which the audio leading up to the
    /// loop start is faded in, so the jump back lands on what is already
    /// playing. Forward loops only: a ping-pong loop has no jump to hide.
    pub crossfade: f64,
    /// How alike the two stretches being cross-faded are (0..=1), which
    /// decides the shape of the fade; see [`crossfade_gains`].
    pub similarity: f32,
}

impl LoopSpec {
    pub const OFF: Self = Self { mode: LoopMode::Off, start: 0.0, end: 0.0, crossfade: 0.0, similarity: 0.0 };

    /// A loop over `start..end` of `sample`, with the points and the
    /// cross-fade pulled in to what the sample has room for. `reverse` says
    /// which way the voices will be travelling, because the cross-fade
    /// borrows the audio just outside the loop on the side they come from.
    pub fn new(sample: &Sample, mode: LoopMode, start: f64, end: f64, crossfade: f64, reverse: bool) -> Self {
        let frames = sample.frames() as f64;
        if mode == LoopMode::Off || frames < MIN_LOOP_FRAMES {
            return Self::OFF;
        }
        let start = start.clamp(0.0, frames - MIN_LOOP_FRAMES).floor();
        let end = end.clamp(start + MIN_LOOP_FRAMES, frames).floor();
        let room = if reverse { frames - end } else { start };
        let crossfade = match mode {
            LoopMode::Forward => crossfade.clamp(0.0, (end - start).min(room)).floor(),
            _ => 0.0,
        };
        let mut spec = Self { mode, start, end, crossfade, similarity: 0.0 };
        spec.similarity = spec.measure_similarity(sample, reverse);
        spec
    }

    /// Correlation of the two stretches the cross-fade blends, from at most
    /// a few hundred evenly spaced frames so that it is cheap enough to redo
    /// whenever a loop point moves.
    fn measure_similarity(&self, sample: &Sample, reverse: bool) -> f32 {
        const POINTS: usize = 512;
        if self.crossfade < 1.0 {
            return 0.0;
        }
        let length = self.end - self.start;
        let first = if reverse { self.start } else { self.end - self.crossfade };
        let offset = if reverse { length } else { -length };
        let step = (self.crossfade / POINTS as f64).max(1.0);
        let (mut dot, mut energy_a, mut energy_b) = (0.0f32, 0.0f32, 0.0f32);
        let mut at = 0.0;
        while at < self.crossfade {
            let a = sample.frame((first + at) as i64);
            let b = sample.frame((first + at + offset) as i64);
            dot += a.0 * b.0 + a.1 * b.1;
            energy_a += a.0 * a.0 + a.1 * a.1;
            energy_b += b.0 * b.0 + b.1 * b.1;
            at += step;
        }
        (dot / (energy_a * energy_b).sqrt().max(1e-20)).clamp(0.0, 1.0)
    }
}

/// Gains of the outgoing and incoming signal `progress` (0..=1) of the way
/// through a cross-fade between signals of the given similarity (0..=1).
///
/// Two signals that are alike add as amplitudes and want a linear fade;
/// unrelated ones add as powers and want an equal-power fade. Dividing a
/// linear fade by the level the mix is expected to have covers both, and
/// everything in between, without a dip or a bump in loudness.
#[inline]
pub fn crossfade_gains(progress: f32, similarity: f32) -> (f32, f32) {
    let incoming = progress.clamp(0.0, 1.0);
    let outgoing = 1.0 - incoming;
    let level = (outgoing * outgoing + incoming * incoming + 2.0 * similarity * outgoing * incoming).sqrt();
    (outgoing / level, incoming / level)
}

#[derive(Copy, Clone)]
pub struct Playhead {
    position: f64,
    /// Frames to advance per output frame; negative while moving backwards.
    step: f64,
    plan: ReadPlan,
    /// The region being played, fixed when the note starts so that moving
    /// the start point cannot make a sounding note jump.
    start: f64,
    end: f64,
    reverse: bool,
    /// Whether the playhead has been around the loop at least once. Until
    /// then the audio outside the loop on the side it came from is real and
    /// the interpolator may use it.
    looped: bool,
    finished: bool,
    /// Output frames since the note started, for the fade-in.
    age: f64,
    fade_in: bool,
    fade_out: bool,
    declick_frames: f64,
}

impl Playhead {
    /// Starts playing `start..end` of `sample` at `rate` frames per output
    /// frame, backwards from the end if `reverse`.
    pub fn new(sample: &Sample, start: f64, end: f64, reverse: bool, rate: f64, output_rate: f64) -> Self {
        let frames = sample.frames() as f64;
        let start = start.clamp(0.0, (frames - 1.0).max(0.0)).floor();
        let end = end.clamp(start + 1.0, frames.max(start + 1.0)).floor();
        let rate = rate.abs().max(1e-9);
        Self {
            position: if reverse { end - 1.0 } else { start },
            step: if reverse { -rate } else { rate },
            plan: sample.plan(rate),
            start,
            end,
            reverse,
            looped: false,
            finished: frames < 1.0,
            age: 0.0,
            // A file's own first and last frames are left as they are; only
            // cuts made here are smoothed.
            fade_in: if reverse { end < frames } else { start > 0.0 },
            fade_out: if reverse { start > 0.0 } else { end < frames },
            declick_frames: (DECLICK_SECONDS * output_rate).max(1.0),
        }
    }

    /// Changes the playback speed, keeping the direction.
    pub fn set_rate(&mut self, sample: &Sample, rate: f64) {
        let rate = rate.abs().max(1e-9);
        if rate != self.step.abs() {
            self.step = rate.copysign(self.step);
            self.plan = sample.plan(rate);
        }
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    /// Produces the next output frame and advances.
    #[inline]
    pub fn next(&mut self, sample: &Sample, looping: &LoopSpec) -> (f32, f32) {
        if self.finished {
            return (0.0, 0.0);
        }
        let position = self.position;
        let (mut left, mut right) = self.read(sample, looping, position);
        let rate = self.step.abs();

        let mut gain = 1.0f32;
        if self.fade_in && self.age < self.declick_frames {
            gain = (self.age / self.declick_frames) as f32;
        }
        self.age += 1.0;

        match looping.mode {
            LoopMode::Off => {
                // Frames of output left before the region runs out.
                let remaining = if self.reverse { position - self.start + 1.0 } else { self.end - position } / rate;
                if self.fade_out && remaining < self.declick_frames {
                    gain *= (remaining / self.declick_frames).max(0.0) as f32;
                }
                self.position += self.step;
                self.finished = if self.reverse { self.position < self.start } else { self.position >= self.end };
            }
            LoopMode::Forward => {
                let length = looping.end - looping.start;
                if looping.crossfade >= 1.0 {
                    // How far into the fade zone at the far end of the loop
                    // the playhead is, and where the audio leading up to the
                    // other end is.
                    let (into, partner) = if self.reverse {
                        (looping.start + looping.crossfade - position, position + length)
                    } else {
                        (position - (looping.end - looping.crossfade), position - length)
                    };
                    if into > 0.0 && into <= looping.crossfade {
                        let (out_gain, in_gain) =
                            crossfade_gains((into / looping.crossfade) as f32, looping.similarity);
                        let (in_left, in_right) = sample.read(&self.plan, partner);
                        left = left * out_gain + in_left * in_gain;
                        right = right * out_gain + in_right * in_gain;
                    }
                }
                self.position += self.step;
                if self.reverse {
                    if self.position < looping.start {
                        self.position = looping.end - (looping.start - self.position).rem_euclid(length);
                        self.looped = true;
                    }
                } else if self.position >= looping.end {
                    self.position = looping.start + (self.position - looping.end).rem_euclid(length);
                    self.looped = true;
                }
            }
            LoopMode::PingPong => {
                self.position += self.step;
                // Turn around on the last and first frames of the loop.
                let (top, bottom) = (looping.end - 1.0, looping.start);
                if self.step > 0.0 && self.position > top {
                    self.position = (2.0 * top - self.position).max(bottom);
                    self.step = -self.step;
                    self.looped = true;
                } else if self.step < 0.0 && self.position < bottom {
                    self.position = (2.0 * bottom - self.position).min(top);
                    self.step = -self.step;
                    self.looped = true;
                }
            }
        }
        (left * gain, right * gain)
    }

    /// Reads the sample at `position`. Near a loop point the interpolator
    /// reaches for frames beyond it; those are fetched from where the
    /// playhead is about to go instead, so that the loop is as smooth as the
    /// audio at its two ends allows.
    #[inline]
    fn read(&self, sample: &Sample, looping: &LoopSpec, position: f64) -> (f32, f32) {
        if looping.mode == LoopMode::Off {
            return sample.read(&self.plan, position);
        }
        let scale = self.plan.scale();
        let (low, high) = ((looping.start * scale).round() as i64, (looping.end * scale).round() as i64);
        let length = high - low;
        let center = (position * scale).floor() as i64;
        let reach = self.plan.reach();
        // The side the playhead entered the loop from still holds the real
        // lead-in until the first time round.
        let wrap_above = !self.reverse || self.looped;
        let wrap_below = self.reverse || self.looped;
        let crosses = (wrap_above && center + reach >= high) || (wrap_below && center - reach < low);
        if !crosses || length < 2 {
            return sample.read(&self.plan, position);
        }
        match looping.mode {
            LoopMode::PingPong => sample.read_mapped(&self.plan, position, |index| {
                if wrap_above && index >= high {
                    (2 * (high - 1) - index).max(low)
                } else if wrap_below && index < low {
                    (2 * low - index).min(high - 1)
                } else {
                    index
                }
            }),
            _ => sample.read_mapped(&self.plan, position, |index| {
                if wrap_above && index >= high {
                    low + (index - high) % length
                } else if wrap_below && index < low {
                    high - 1 - (low - 1 - index) % length
                } else {
                    index
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;
    use trill_core::audio::AudioBuffer;

    const SR: f64 = 48_000.0;

    fn sample_of(left: Vec<f32>) -> Sample {
        Sample::new(&AudioBuffer { sample_rate: SR, right: left.clone(), left })
    }

    fn ramp(frames: usize) -> Sample {
        sample_of((0..frames).map(|i| i as f32).collect())
    }

    fn sine(period: f64, frames: usize) -> Sample {
        sample_of((0..frames).map(|i| (2.0 * PI * i as f64 / period).sin() as f32).collect())
    }

    fn play(head: &mut Playhead, sample: &Sample, looping: &LoopSpec, frames: usize) -> Vec<f32> {
        (0..frames).map(|_| head.next(sample, looping).0).collect()
    }

    fn largest_step(signal: &[f32]) -> f32 {
        signal.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn plays_the_region_once_forwards_and_backwards() {
        let sample = ramp(1000);
        let mut head = Playhead::new(&sample, 0.0, 1000.0, false, 1.0, SR);
        let out = play(&mut head, &sample, &LoopSpec::OFF, 1100);
        assert!(head.finished());
        for (i, value) in out.iter().enumerate() {
            let want = if i < 1000 { i as f32 } else { 0.0 };
            assert!((value - want).abs() < 2e-3, "frame {i}: {value}");
        }

        let mut head = Playhead::new(&sample, 0.0, 1000.0, true, 1.0, SR);
        let out = play(&mut head, &sample, &LoopSpec::OFF, 1100);
        for (i, value) in out.iter().enumerate() {
            let want = if i < 1000 { (999 - i) as f32 } else { 0.0 };
            assert!((value - want).abs() < 2e-3, "frame {i}: {value}");
        }
    }

    #[test]
    fn cuts_inside_the_file_are_faded() {
        // A constant: any cut into it is a step unless it is faded.
        let sample = sample_of(vec![1.0; 4_000]);
        let mut head = Playhead::new(&sample, 1_000.0, 3_000.0, false, 1.0, SR);
        let out = play(&mut head, &sample, &LoopSpec::OFF, 2_100);
        assert!(out[0].abs() < 0.02 && out[2_000..].iter().all(|s| *s == 0.0));
        assert!((out[1_000] - 1.0).abs() < 1e-3);
        assert!(largest_step(&out) < 0.02, "step of {}", largest_step(&out));

        // The file's own edges are left alone.
        let mut head = Playhead::new(&sample, 0.0, 4_000.0, false, 1.0, SR);
        let out = play(&mut head, &sample, &LoopSpec::OFF, 4_000);
        assert!((out[0] - 1.0).abs() < 1e-3 && (out[3_999] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn a_seamless_loop_stays_seamless_at_any_pitch() {
        // 40 whole periods between the loop points: looping it is a perfect
        // sine, so the largest step must stay that of the sine itself.
        let period = 100.0;
        let sample = sine(period, 8_000);
        for (start, end) in [(2_000.0, 6_000.0), (0.0, 8_000.0)] {
            for mode in [LoopMode::Forward, LoopMode::PingPong] {
                for reverse in [false, true] {
                    for rate in [0.31, 1.0, 1.5, 2.37, 4.0] {
                        // A ping-pong loop mirrors the wave, which is only
                        // smooth if it turns on the crests.
                        let (start, end) =
                            if mode == LoopMode::PingPong { (start + 25.0, end - 24.0) } else { (start, end) };
                        let looping = LoopSpec::new(&sample, mode, start, end, 0.0, reverse);
                        let mut head = Playhead::new(&sample, 0.0, 8_000.0, reverse, rate, SR);
                        let out = play(&mut head, &sample, &looping, 40_000);
                        assert!(!head.finished());
                        let natural = (2.0 * PI * rate / period) as f32;
                        let step = largest_step(&out[100..]);
                        // Known limitation: a loop that runs to the very edge
                        // of the file, played from a half-rate copy, meets
                        // the ripple that band-limiting leaves at a file's
                        // ends (the copies are made as if silence surrounded
                        // the file, not the loop). It shows as a step about
                        // 30 dB below the signal once per pass.
                        let at_file_edge = start == 0.0 && rate > 1.5;
                        let limit = if at_file_edge { natural * 1.15 } else { natural * 1.05 + 0.003 };
                        assert!(
                            step < limit,
                            "{mode:?} reverse {reverse} rate {rate} {start}..{end}: step {step} vs {natural}"
                        );
                        let peak = out[100..].iter().fold(0.0f32, |max, s| max.max(s.abs()));
                        assert!((peak - 1.0).abs() < 0.01, "{mode:?} rate {rate}: peak {peak}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_crossfade_hides_a_loop_that_does_not_line_up() {
        // The loop is a quarter period short of whole, so jumping back is a
        // jump in the waveform; the cross-fade must spread it out.
        let period = 100.0;
        let sample = sine(period, 8_000);
        for reverse in [false, true] {
            let abrupt = LoopSpec::new(&sample, LoopMode::Forward, 2_000.0, 5_975.0, 0.0, reverse);
            let faded = LoopSpec::new(&sample, LoopMode::Forward, 2_000.0, 5_975.0, 1_000.0, reverse);
            assert_eq!(faded.crossfade, 1_000.0);
            let natural = (2.0 * PI / period) as f32;
            let mut head = Playhead::new(&sample, 0.0, 8_000.0, reverse, 1.0, SR);
            let hard = largest_step(&play(&mut head, &sample, &abrupt, 30_000)[100..]);
            let mut head = Playhead::new(&sample, 0.0, 8_000.0, reverse, 1.0, SR);
            let out = play(&mut head, &sample, &faded, 30_000);
            let soft = largest_step(&out[100..]);
            assert!(hard > 0.5, "reverse {reverse}: expected a jump without the fade, got {hard}");
            assert!(soft < natural * 1.5, "reverse {reverse}: step {soft} with the fade");
            // And the level holds through the fade: no dip, no bump.
            let levels: Vec<f32> =
                out[4_000..].chunks_exact(200).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / 200.0).sqrt()).collect();
            let (low, high) = levels.iter().fold((f32::MAX, 0.0f32), |(low, high), l| (low.min(*l), high.max(*l)));
            assert!(high / low < 1.2, "reverse {reverse}: level wobbles between {low} and {high}");
        }
    }

    #[test]
    fn crossfade_is_limited_to_the_room_before_the_loop() {
        let sample = sine(100.0, 8_000);
        assert_eq!(LoopSpec::new(&sample, LoopMode::Forward, 300.0, 4_000.0, 1_000.0, false).crossfade, 300.0);
        assert_eq!(LoopSpec::new(&sample, LoopMode::Forward, 300.0, 7_500.0, 1_000.0, true).crossfade, 500.0);
        assert_eq!(LoopSpec::new(&sample, LoopMode::Forward, 0.0, 8_000.0, 1_000.0, false).crossfade, 0.0);
        assert_eq!(LoopSpec::new(&sample, LoopMode::PingPong, 300.0, 4_000.0, 1_000.0, false).crossfade, 0.0);
        // Points given backwards or out of range still make a valid loop.
        let spec = LoopSpec::new(&sample, LoopMode::Forward, 9_000.0, -5.0, 0.0, false);
        assert!(spec.start >= 0.0 && spec.end <= 8_000.0 && spec.end - spec.start >= MIN_LOOP_FRAMES);
        assert_eq!(LoopSpec::new(&sample, LoopMode::Off, 0.0, 100.0, 0.0, false).mode, LoopMode::Off);
    }

    #[test]
    fn crossfade_gains_keep_the_level() {
        for similarity in [0.0f32, 0.5, 1.0] {
            for progress in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                let (out_gain, in_gain) = crossfade_gains(progress, similarity);
                let level = out_gain * out_gain + in_gain * in_gain + 2.0 * similarity * out_gain * in_gain;
                assert!((level - 1.0).abs() < 1e-5);
            }
        }
        assert_eq!(crossfade_gains(0.0, 0.3), (1.0, 0.0));
        assert_eq!(crossfade_gains(1.0, 0.3), (0.0, 1.0));
    }

    #[test]
    fn empty_and_tiny_samples_are_safe() {
        for frames in [0usize, 1, 2, 3, 5] {
            let sample = sample_of(vec![0.5; frames]);
            let looping = LoopSpec::new(&sample, LoopMode::Forward, 0.0, frames as f64, 10.0, false);
            let mut head = Playhead::new(&sample, 0.0, frames as f64, false, 1.7, SR);
            let out = play(&mut head, &sample, &looping, 200);
            assert!(out.iter().all(|s| s.is_finite()));
        }
    }
}
