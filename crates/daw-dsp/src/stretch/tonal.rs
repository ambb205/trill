//! Tonal mode: a phase vocoder, for sustained and harmonic material.
//!
//! The audio is cut into long overlapping windows, each is taken to the
//! frequency domain, and the windows are laid back down at a different
//! spacing. Laid down unchanged they would no longer line up: a partial that
//! advanced by some phase between two windows now has more (or less) time to
//! cover. So each frequency bin is rotated by the phase its partial would
//! have gained in the extra time, which is what keeps a held note a held
//! note instead of a flutter.
//!
//! Which bins belong to which partial is the hard part, and where the
//! quality comes from. Rotating every bin by its own estimate gives the
//! familiar smeared, "phasey" vocoder sound, because the bins around a peak
//! must keep their phase relationship to it to describe one clean sinusoid.
//! Here bins are visited from loudest to quietest (after Prusa and Holighaus,
//! "Phase Vocoder Done Right", 2017): a bin that was louder in the previous
//! window continues its own partial; otherwise it takes the rotation of the
//! louder neighbour it leans on, provided the two measure the same frequency
//! and so really are the same partial. Steady partials are thereby locked
//! around their peaks, and two notes close enough to share bins (a minor
//! third in the bass) are not locked to each other.
//!
//! Attacks get two further protections. A bin whose level has just jumped is
//! carrying something new, with no past to stay coherent with, so it is not
//! rotated at all: the attack keeps the waveform it had. And the time map
//! locks the stretch around each strong onset, so that all the windows that
//! see the attack agree on where it is instead of spreading it out. Notes
//! that are held through an attack keep their rotation and carry on
//! unbroken underneath it.
//!
//! Stereo is handled by working out the rotations once, on a combination of
//! both channels, and applying them to each. The phase and level relations
//! between the channels pass through untouched, so the image cannot wander
//! and a mono-compatible file stays mono-compatible.

use super::onsets::Onset;
use super::timemap::{Anchor, TimeMap};
use crate::fft::{RealFft, hann};
use std::f32::consts::TAU;

/// Only pronounced onsets are locked in this mode. Every lock makes the
/// material around it stretch harder, which costs more in a held chord than
/// a slightly softened attack does. Weaker onsets are still pinned to their
/// place in time, just not protected.
const LOCK_STRENGTH: f32 = 1.0;
/// A bin whose power has grown by this factor since the previous window is
/// treated as the start of something new (10 dB within a hop is an attack,
/// not a swell).
const ONSET_RISE: f32 = 10.0;
/// The same for a window that sits on a detected attack (inside the lock
/// the time map puts around one). There any bin that is gaining energy is
/// taken to be part of the attack, even over something that was already
/// sounding in it: by 1 dB within a hop where attacks are heard as
/// sharpness, by 3 dB down where bass notes ring on underneath and a break
/// in them would be heard instead.
const ATTACK_RISE: f32 = 1.25;
const ATTACK_RISE_BASS: f32 = 2.0;
const BASS_HZ: f64 = 500.0;
/// Bins this far below the loudest keep the rotation they had: there is
/// nothing in them to estimate a frequency from.
const SILENCE: f32 = 1e-10;
/// Neighbouring bins are taken to hold the same partial when the
/// frequencies measured in them agree to within this many bins...
const SAME_PARTIAL: f32 = 0.25;
/// ...or when the neighbour is this far (30 dB) below the peak it would be
/// locked to. That deep in a peak's skirt the measurement is unreliable, and
/// being wrong costs nothing that could be heard next to the peak.
const SKIRT: f32 = 1e-3;
/// The closest two windows are ever laid down, however hard the audio is
/// squeezed. Reached only at ratios beyond the supported range.
const MIN_HOP: i64 = 32;

/// About 85 ms at 44.1 and 48 kHz: long enough to tell apart the partials of
/// a bass note, short enough that chord changes are not blurred.
fn window_size(sample_rate: f64) -> usize {
    ((sample_rate * 0.06) as usize).next_power_of_two().max(256)
}

pub(super) fn stretch(
    left: &[f32],
    right: &[f32],
    sample_rate: f64,
    out_len: usize,
    onsets: &[Onset],
) -> (Vec<f32>, Vec<f32>) {
    let size = window_size(sample_rate);
    // Lock half a window either side of each strong attack: every window
    // that contains the attack is then read and written at the same spacing,
    // so their copies of it coincide. Every other onset gets a lock of no
    // length, which only pins it: without that, the locks around it would
    // push it off the beat.
    let lock = size / 2;
    let anchors: Vec<Anchor> = onsets
        .iter()
        .map(|onset| {
            let span = if onset.strength >= LOCK_STRENGTH { lock } else { 0 };
            Anchor { input: onset.frame, pre: span, post: span }
        })
        .collect();
    // The two ends are locked for a shorter stretch: enough for the output
    // to begin and end as the input does, without holding whatever happens
    // to be near an end off the beat.
    let map = TimeMap::new(left.len(), out_len, &anchors, size / 8);

    let both = [left, right];
    let channels = if left == right { &both[..1] } else { &both[..] };
    let mut vocoder = Vocoder::new(size, channels.len(), sample_rate);
    let mut outputs: Vec<Vec<f32>> = channels.iter().map(|_| vec![0.0; out_len]).collect();
    let mut weight = vec![0.0f32; out_len];

    // Windows are laid down a quarter of their length apart. The frequency
    // measurement needs them to be read no further apart than that either,
    // so wherever the audio is squeezed the next window is laid down closer:
    // the spacing halves until the read spacing is legal. The windows are
    // normalized by what was actually overlapped, so the spacing is free to
    // change from one to the next.
    let quarter = (size / 4) as i64;
    let read_at = |center: i64| map.input_at(center as f64).round() as i64;
    // Start with the first window that reaches into the output, so every
    // output frame is covered by the full set of overlapping windows.
    let mut center = quarter - (size / 2) as i64;
    let mut position = read_at(center);
    vocoder.analyze(channels, position);
    vocoder.synthesize(center, &mut outputs, &mut weight);
    while center - (size as i64 / 2) < out_len as i64 {
        let mut hop = quarter;
        while hop > MIN_HOP && read_at(center + hop) - position > quarter {
            hop /= 2;
        }
        center += hop;
        let next = read_at(center);
        vocoder.analyze(channels, next);
        vocoder.rotate((next - position).max(0) as usize, hop as usize, map.locked_at(center as f64));
        vocoder.synthesize(center, &mut outputs, &mut weight);
        position = next;
    }

    for output in &mut outputs {
        for (sample, weight) in output.iter_mut().zip(&weight) {
            *sample = if *weight > 1e-6 { *sample / weight } else { 0.0 };
        }
    }
    let right_out = if outputs.len() > 1 { outputs.pop() } else { None };
    let left_out = outputs.swap_remove(0);
    let right_out = right_out.unwrap_or_else(|| left_out.clone());
    (left_out, right_out)
}

/// Bins waiting to pass their rotation on, loudest first.
///
/// The loudness order only has to be roughly right, so instead of a heap
/// this sorts into buckets a quarter of an octave of power (0.75 dB) wide,
/// which makes both adding and taking constant-time. Within a bucket, bins
/// leave in the order they arrived, so the result is fully determined.
struct Queue {
    /// Bucket of each queued entry's successor, forming one list per bucket.
    next: Vec<u32>,
    heads: [u32; BUCKETS],
    tails: [u32; BUCKETS],
    /// No bucket above this one has anything in it.
    top: usize,
    /// Loudness key of the top bucket's upper edge.
    ceiling: i32,
}

/// 160 buckets of 0.75 dB reach 120 dB down; anything quieter shares the
/// last bucket.
const BUCKETS: usize = 160;
const NONE: u32 = u32::MAX;

impl Queue {
    fn new(bins: usize) -> Self {
        // An entry is a bin in the previous window (index `bin`) or in the
        // current one (index `bins + bin`).
        Self { next: vec![NONE; bins * 2], heads: [NONE; BUCKETS], tails: [NONE; BUCKETS], top: 0, ceiling: 0 }
    }

    /// The top two bits of the mantissa below the exponent: a logarithmic
    /// scale with four steps per doubling, read straight off the float.
    fn key(power: f32) -> i32 {
        (power.to_bits() >> 21) as i32
    }

    /// Empties the queue and sets the loudest power it will be given.
    fn reset(&mut self, loudest: f32) {
        self.heads = [NONE; BUCKETS];
        self.tails = [NONE; BUCKETS];
        self.top = 0;
        self.ceiling = Self::key(loudest);
    }

    fn push(&mut self, entry: usize, power: f32) {
        let depth = (self.ceiling - Self::key(power)).clamp(0, BUCKETS as i32 - 1) as usize;
        let bucket = BUCKETS - 1 - depth;
        self.next[entry] = NONE;
        match self.tails[bucket] {
            NONE => self.heads[bucket] = entry as u32,
            tail => self.next[tail as usize] = entry as u32,
        }
        self.tails[bucket] = entry as u32;
        self.top = self.top.max(bucket);
    }

    fn pop(&mut self) -> Option<usize> {
        loop {
            let entry = self.heads[self.top];
            if entry != NONE {
                self.heads[self.top] = self.next[entry as usize];
                if self.heads[self.top] == NONE {
                    self.tails[self.top] = NONE;
                }
                return Some(entry as usize);
            }
            if self.top == 0 {
                return None;
            }
            self.top -= 1;
        }
    }
}

struct Spectrum {
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Spectrum {
    fn new(bins: usize) -> Self {
        Self { re: vec![0.0; bins], im: vec![0.0; bins] }
    }
}

struct Vocoder {
    size: usize,
    /// Bins below [`BASS_HZ`].
    bass_bins: usize,
    fft: RealFft,
    window: Vec<f32>,
    frame: Vec<f32>,
    /// Spectrum of each channel for the current window.
    channels: Vec<Spectrum>,
    /// The spectrum the rotations are worked out on, and its power, for the
    /// current and the previous window.
    reference: Spectrum,
    power: Vec<f32>,
    last_reference: Spectrum,
    last_power: Vec<f32>,
    /// Per bin, the unit complex number the input spectrum is multiplied by.
    rotation: Spectrum,
    /// Per bin, how far the phase gained since the previous window is from
    /// what the bin's center frequency would have gained, in radians.
    deviation: Vec<f32>,
    /// Per bin, the power of the peak whose rotation it was given.
    peak_power: Vec<f32>,
    pending: Vec<bool>,
    queue: Queue,
}

impl Vocoder {
    fn new(size: usize, channels: usize, sample_rate: f64) -> Self {
        let fft = RealFft::new(size);
        let bins = fft.bins();
        Self {
            size,
            bass_bins: (BASS_HZ * size as f64 / sample_rate) as usize,
            fft,
            window: hann(size),
            frame: vec![0.0; size],
            channels: (0..channels).map(|_| Spectrum::new(bins)).collect(),
            reference: Spectrum::new(bins),
            power: vec![0.0; bins],
            last_reference: Spectrum::new(bins),
            last_power: vec![0.0; bins],
            // No rotation: until `rotate` says otherwise the output is the input.
            rotation: Spectrum { re: vec![1.0; bins], im: vec![0.0; bins] },
            deviation: vec![0.0; bins],
            peak_power: vec![0.0; bins],
            pending: vec![false; bins],
            queue: Queue::new(bins),
        }
    }

    /// Transforms the window centered on input frame `position`.
    fn analyze(&mut self, inputs: &[&[f32]], position: i64) {
        std::mem::swap(&mut self.reference, &mut self.last_reference);
        std::mem::swap(&mut self.power, &mut self.last_power);

        let start = position - (self.size / 2) as i64;
        for (input, spectrum) in inputs.iter().zip(&mut self.channels) {
            for (i, slot) in self.frame.iter_mut().enumerate() {
                let frame = start + i as i64;
                let sample = if frame >= 0 { input.get(frame as usize).copied().unwrap_or(0.0) } else { 0.0 };
                *slot = sample * self.window[i];
            }
            self.fft.forward(&self.frame, &mut spectrum.re, &mut spectrum.im);
        }

        // The reference is left + i * right. Unlike the sum of the channels
        // it does not vanish for out-of-phase material, and a partial present
        // in either channel advances its phase at that partial's frequency.
        let left = &self.channels[0];
        match self.channels.get(1) {
            Some(right) => {
                for bin in 0..self.power.len() {
                    self.reference.re[bin] = left.re[bin] - right.im[bin];
                    self.reference.im[bin] = left.im[bin] + right.re[bin];
                }
            }
            None => {
                self.reference.re.copy_from_slice(&left.re);
                self.reference.im.copy_from_slice(&left.im);
            }
        }
        for (bin, power) in self.power.iter_mut().enumerate() {
            *power = self.reference.re[bin].powi(2) + self.reference.im[bin].powi(2);
        }
    }

    /// Updates the rotations for a window read `analysis_hop` frames after
    /// the previous one and written `synthesis_hop` frames after it.
    /// `on_attack` says the window is centered on a detected attack.
    fn rotate(&mut self, analysis_hop: usize, synthesis_hop: usize, on_attack: bool) {
        let bins = self.power.len();
        let loudest = self.power.iter().chain(&self.last_power).fold(0.0f32, |max, &power| max.max(power));
        let threshold = loudest * SILENCE;

        // How much longer (or shorter) the partials have to advance for,
        // relative to the time between the two analysis windows. A hop of
        // zero (only at ratios far beyond the supported range) leaves
        // nothing to measure a frequency from, so the rotations are held.
        let extra = if analysis_hop == 0 { 0 } else { synthesis_hop as i64 - analysis_hop as i64 };
        let stretch = extra as f32 / analysis_hop.max(1) as f32;
        let size = self.size as i64;
        // Turns of phase per hop to bins of frequency.
        let bins_per_radian = self.size as f32 / (TAU * analysis_hop.max(1) as f32);

        self.queue.reset(loudest);
        for bin in 0..bins {
            self.pending[bin] = self.power[bin] > threshold;
            if self.pending[bin] {
                self.queue.push(bin, self.last_power[bin]);
                // Phase the bin actually gained between the two windows,
                // compared with what its center frequency would have gained:
                // the difference says how far off center whatever is
                // sounding in the bin is.
                let (re, im) = (self.reference.re[bin], self.reference.im[bin]);
                let (last_re, last_im) = (self.last_reference.re[bin], self.last_reference.im[bin]);
                let gained = atan2(im * last_re - re * last_im, re * last_re + im * last_im);
                let expected = TAU * ((bin * analysis_hop) % self.size) as f32 / self.size as f32;
                self.deviation[bin] = wrap(gained - expected);
            }
        }

        while let Some(entry) = self.queue.pop() {
            // Entries past `bins` are bins of the current window.
            let bin = entry % bins;
            if entry >= bins {
                // Loudest unassigned thing around: the neighbours that are
                // part of the same partial lean on it. Being part of it
                // shows in measuring the same frequency. A neighbour that
                // measures another belongs to another partial, perhaps one
                // too close for the window to show as a separate peak, and
                // is left to carry on by itself: locked to this one it would
                // be dragged off its pitch.
                let peak_power = self.peak_power[bin];
                for neighbour in [bin.wrapping_sub(1), bin + 1] {
                    if neighbour < bins && self.pending[neighbour] {
                        let apart = neighbour as f32 - bin as f32
                            + (self.deviation[neighbour] - self.deviation[bin]) * bins_per_radian;
                        if apart.abs() <= SAME_PARTIAL || self.power[neighbour] < peak_power * SKIRT {
                            self.pending[neighbour] = false;
                            self.rotation.re[neighbour] = self.rotation.re[bin];
                            self.rotation.im[neighbour] = self.rotation.im[bin];
                            self.peak_power[neighbour] = peak_power;
                            self.queue.push(bins + neighbour, self.power[neighbour]);
                        }
                    }
                }
            } else if self.pending[bin] {
                self.pending[bin] = false;
                let rise = match (on_attack, bin < self.bass_bins) {
                    (false, _) => ONSET_RISE,
                    (true, true) => ATTACK_RISE_BASS,
                    (true, false) => ATTACK_RISE,
                };
                if self.power[bin] > self.last_power[bin] * rise {
                    // Something new has arrived in this bin: play it as it is.
                    self.rotation.re[bin] = 1.0;
                    self.rotation.im[bin] = 0.0;
                } else if extra != 0 {
                    // The partial's frequency times the extra time. The
                    // center frequency's share is reduced modulo a turn in
                    // integers, so high bins lose no precision.
                    let center = TAU * (bin as i64 * extra).rem_euclid(size) as f32 / self.size as f32;
                    let (sin, cos) = (center + self.deviation[bin] * stretch).sin_cos();
                    let (old_re, old_im) = (self.rotation.re[bin], self.rotation.im[bin]);
                    let (new_re, new_im) = (old_re * cos - old_im * sin, old_re * sin + old_im * cos);
                    // Renormalized so thousands of products cannot drift off
                    // the unit circle.
                    let norm = 1.0 / (new_re * new_re + new_im * new_im).sqrt();
                    self.rotation.re[bin] = new_re * norm;
                    self.rotation.im[bin] = new_im * norm;
                }
                self.peak_power[bin] = self.power[bin];
                self.queue.push(bins + bin, self.power[bin]);
            }
        }
    }

    /// Rotates the current window's spectra, transforms them back and adds
    /// them to the output centered on output frame `center`.
    fn synthesize(&mut self, center: i64, outputs: &mut [Vec<f32>], weight: &mut [f32]) {
        let start = center - (self.size / 2) as i64;
        let out_len = weight.len() as i64;
        let first = (-start).clamp(0, self.size as i64) as usize;
        let last = (out_len - start).clamp(0, self.size as i64) as usize;

        for (spectrum, output) in self.channels.iter_mut().zip(outputs.iter_mut()) {
            for bin in 0..spectrum.re.len() {
                let (re, im) = (spectrum.re[bin], spectrum.im[bin]);
                let (rot_re, rot_im) = (self.rotation.re[bin], self.rotation.im[bin]);
                spectrum.re[bin] = re * rot_re - im * rot_im;
                spectrum.im[bin] = re * rot_im + im * rot_re;
            }
            self.fft.inverse(&spectrum.re, &spectrum.im, &mut self.frame);
            for i in first..last {
                output[(start + i as i64) as usize] += self.frame[i] * self.window[i];
            }
        }
        for i in first..last {
            weight[(start + i as i64) as usize] += self.window[i] * self.window[i];
        }
    }
}

/// Wraps a phase to -pi..=pi.
#[inline]
fn wrap(phase: f32) -> f32 {
    phase - TAU * (phase / TAU).round()
}

/// Arctangent of `y / x` in the right quadrant, to within 0.00001 radians
/// (Abramowitz and Stegun 4.4.49). An error that small is a few thousandths
/// of a hertz in the frequency it is used to measure; the exact function is
/// several times slower and is called for every bin of every window.
#[inline]
fn atan2(y: f32, x: f32) -> f32 {
    use std::f32::consts::{FRAC_PI_2, PI};
    let (ax, ay) = (x.abs(), y.abs());
    let (small, large) = if ax < ay { (ax, ay) } else { (ay, ax) };
    if large == 0.0 {
        return 0.0;
    }
    let t = small / large;
    let t2 = t * t;
    let angle = t * (0.999_866 + t2 * (-0.330_299_5 + t2 * (0.180_141 + t2 * (-0.085_133 + t2 * 0.020_835_1))));
    let angle = if ax < ay { FRAC_PI_2 - angle } else { angle };
    let angle = if x < 0.0 { PI - angle } else { angle };
    angle.copysign(y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    #[test]
    fn identical_lengths_reproduce_the_input() {
        let mut rng = Rng::new(5);
        let left: Vec<f32> = (0..40_000).map(|_| rng.bipolar() * 0.5).collect();
        let right: Vec<f32> = (0..40_000).map(|_| rng.bipolar() * 0.5).collect();
        let onsets = [Onset { frame: 9_000, strength: 3.0 }];
        let (out_left, out_right) = stretch(&left, &right, 48_000.0, 40_000, &onsets);
        for (output, input) in [(&out_left, &left), (&out_right, &right)] {
            let error = output.iter().zip(input.iter()).fold(0.0f32, |max, (a, b)| max.max((a - b).abs()));
            assert!(error < 1e-4, "error {error}");
        }
    }

    #[test]
    fn fast_arctangent_is_accurate_in_every_quadrant() {
        for step in 0..3_600 {
            let angle = (step as f32 - 1_800.0) * (TAU / 3_600.0) + 1e-3;
            for radius in [1e-6f32, 1.0, 1e6] {
                let (y, x) = (radius * angle.sin(), radius * angle.cos());
                assert!((atan2(y, x) - y.atan2(x)).abs() < 2e-5, "angle {angle}: {} vs {}", atan2(y, x), y.atan2(x));
            }
        }
        assert_eq!(atan2(0.0, 0.0), 0.0);
    }

    #[test]
    fn wrap_stays_within_half_a_turn() {
        use std::f32::consts::PI;
        for phase in [-20.0f32, -PI, -1.0, 0.0, 1.0, PI, 7.0, 100.0] {
            let wrapped = wrap(phase);
            assert!(wrapped.abs() <= PI + 1e-5);
            assert!(((wrapped - phase) / TAU).round() * TAU - (wrapped - phase) < 1e-3);
        }
    }
}
