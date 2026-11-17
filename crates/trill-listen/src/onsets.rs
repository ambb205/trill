//! Where notes and hits begin.
//!
//! An onset is found coarsely, from the rise in the spectrum, and is then
//! walked back to the foot of its attack in the waveform itself: a slice
//! cut a few milliseconds late has lost the front of the kick.

use crate::signal::{RATE, resample};
use crate::spectrum::{onset_strength, pick_peaks};

/// The rate and the hop the coarse pass works at.
const COARSE_RATE: f64 = 22_050.0;
const COARSE_HOP: usize = 128;

/// How far a beat may be moved to land on a hit, in seconds.
pub const SNAP_WINDOW: f64 = 0.045;
/// A slice starts this long before the hit, so that its very front is kept.
pub const PRE_ROLL: f64 = 0.002;

/// The samples (at [`RATE`]) where notes or hits begin in `signal`.
/// `sensitivity` is how far a rise must stand over its surroundings, as a
/// share of the largest rise in the signal: 0.07 finds every hit of a drum
/// part.
pub fn onsets(signal: &[f32], sensitivity: f32) -> Vec<usize> {
    let peak = signal.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let low = resample(signal, RATE, COARSE_RATE);
    if low.len() < 2048 || peak < 1e-4 {
        return Vec::new();
    }
    let mut envelope = onset_strength(&low, COARSE_RATE, 1024, COARSE_HOP, 64);
    // Between nothing and one, so that `sensitivity` means the same in a
    // loud signal and a quiet one.
    let floor = envelope.iter().copied().fold(f32::INFINITY, f32::min);
    let top = envelope.iter().copied().fold(0.0f32, f32::max) - floor;
    envelope.iter_mut().for_each(|value| *value = (*value - floor) / top.max(1e-12));

    let per_second = COARSE_RATE / COARSE_HOP as f64;
    let frames = pick_peaks(
        &envelope,
        (0.03 * per_second) as usize,
        (0.10 * per_second) as usize,
        sensitivity,
        (0.045 * per_second) as usize,
    );
    let coarse: Vec<usize> =
        frames.iter().map(|&frame| (frame as f64 * COARSE_HOP as f64 * RATE / COARSE_RATE) as usize).collect();
    let mut found = refine(signal, &coarse);
    // A rise in the spectrum needs something before it to rise from, so a
    // hit on the very first sample goes unseen. Audio that opens loud opens
    // with an onset.
    let opening = signal[..signal.len().min((0.010 * RATE) as usize)].iter().fold(0.0f32, |top, s| top.max(s.abs()));
    if opening > 0.2 * peak && found.first().is_none_or(|&first| first > (0.030 * RATE) as usize) {
        found.insert(0, 0);
    }
    found
}

/// The loudest sample of the `hold` samples up to and including each one.
fn peak_hold(signal: &[f32], hold: usize) -> Vec<f32> {
    let mut held = Vec::with_capacity(signal.len());
    // Positions whose values fall from front to back: the front is the
    // loudest still inside the window.
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    for (index, sample) in signal.iter().enumerate() {
        let level = sample.abs();
        while queue.back().is_some_and(|&back| signal[back].abs() <= level) {
            queue.pop_back();
        }
        queue.push_back(index);
        if queue.front().is_some_and(|&front| front + hold <= index) {
            queue.pop_front();
        }
        held.push(signal[queue[0]].abs());
    }
    held
}

/// Moves each rough onset to where its attack begins.
///
/// Loudness is followed with a peak-hold: the loudest sample of the last
/// few milliseconds. That stays level through the cycles of a low note,
/// where the waveform itself swings to zero and back, and steps up at once
/// when something new strikes. The attack is the biggest such step near
/// the rough position. A hit often lands on the tail of the one before, so
/// it is measured against whatever was still sounding, not against
/// silence; and a rough onset with no step near it (a ripple in the body
/// of a long note, a hat buried under a kick) is dropped.
pub fn refine(signal: &[f32], coarse: &[usize]) -> Vec<usize> {
    // Half a cycle of 40 Hz, and odd.
    let hold = (0.013 * RATE) as usize | 1;
    let held = peak_hold(signal, hold);
    let reach = (0.030 * RATE) as usize;
    // A step is measured over two milliseconds.
    let span = (0.002 * RATE) as usize;
    let mut refined = Vec::new();
    for &position in coarse {
        let (start, stop) = (position.saturating_sub(reach), (position + reach).min(held.len()));
        if stop <= start + span * 2 {
            continue;
        }
        let window = &held[start..stop];
        if window.iter().copied().fold(0.0f32, f32::max) < 1e-4 {
            continue;
        }
        let attack = (0..window.len() - span)
            .max_by(|&a, &b| (window[a + span] - window[a]).total_cmp(&(window[b + span] - window[b])).then(b.cmp(&a)))
            .unwrap_or(0);
        // Back to where the step starts.
        let mut foot = attack;
        while foot > 0 && window[foot - 1] < window[foot] {
            foot -= 1;
        }
        while foot + 1 < window.len() && window[foot + 1] <= window[foot] && foot < attack + span {
            foot += 1;
        }
        let crest =
            window[attack..(attack + (0.010 * RATE) as usize).min(window.len())].iter().copied().fold(0.0f32, f32::max);
        if crest <= 0.0 || (crest - window[foot]) / crest < 0.4 {
            continue;
        }
        refined.push(start + foot);
    }
    refined.sort_unstable();
    refined.dedup();
    // Two that landed on the same attack are one onset.
    let gap = (0.020 * RATE) as usize;
    let mut kept: Vec<usize> = Vec::with_capacity(refined.len());
    for (index, &onset) in refined.iter().enumerate() {
        if index == 0 || onset - refined[index - 1] > gap {
            kept.push(onset);
        }
    }
    kept
}

/// Moves each time onto the nearest onset within [`SNAP_WINDOW`], a
/// [`PRE_ROLL`] before it. Times keep their order and stay apart.
pub fn snap(times: &[f64], onsets: &[usize]) -> Vec<f64> {
    if onsets.is_empty() || times.is_empty() {
        return times.to_vec();
    }
    let onset_times: Vec<f64> = onsets.iter().map(|&onset| onset as f64 / RATE).collect();
    let mut snapped = times.to_vec();
    for (slot, &time) in snapped.iter_mut().zip(times) {
        let index = onset_times.partition_point(|&onset| onset < time);
        let nearest = [index.checked_sub(1), Some(index)]
            .into_iter()
            .flatten()
            .filter_map(|index| onset_times.get(index).copied())
            .min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()));
        if let Some(nearest) = nearest.filter(|nearest| (nearest - time).abs() <= SNAP_WINDOW) {
            *slot = (nearest - PRE_ROLL).max(0.0);
        }
    }
    // Snapping must never reorder or merge beats.
    for index in 1..snapped.len() {
        if snapped[index] <= snapped[index - 1] + 0.05 {
            snapped[index] = times[index].max(snapped[index - 1] + 0.05);
        }
    }
    snapped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::testing::{Noise, drum, silence, tone};

    #[test]
    fn every_hit_of_a_drum_part_is_found_at_the_foot_of_its_attack() {
        let mut signal = silence(3.0);
        let mut noise = Noise(7);
        let times = [0.25, 0.75, 1.0, 1.25, 1.75, 2.25, 2.5];
        for (index, &time) in times.iter().enumerate() {
            drum(&mut signal, time, ["kick", "hat", "snare"][index % 3], 0.8, &mut noise);
        }
        let found = onsets(&signal, 0.07);
        assert_eq!(found.len(), times.len(), "{found:?}");
        for (onset, time) in found.iter().zip(times) {
            let seconds = *onset as f64 / RATE;
            // Never late, and at most three milliseconds early.
            assert!(seconds <= time + 0.0005 && seconds > time - 0.003, "{seconds} for {time}");
        }
    }

    #[test]
    fn audio_that_opens_with_a_hit_opens_with_an_onset() {
        let mut signal = silence(1.0);
        let mut noise = Noise(3);
        drum(&mut signal, 0.0, "kick", 0.9, &mut noise);
        drum(&mut signal, 0.5, "snare", 0.9, &mut noise);
        let found = onsets(&signal, 0.07);
        assert_eq!(found.first(), Some(&0));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn the_body_of_a_long_note_holds_no_onset_and_silence_holds_none() {
        let mut signal = silence(2.0);
        tone(&mut signal, 0.3, 1.5, 55.0, 0.7);
        let found = onsets(&signal, 0.07);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!((found[0] as f64 / RATE - 0.3).abs() < 0.005);
        assert!(onsets(&silence(1.0), 0.07).is_empty());
        assert!(onsets(&[0.5; 100], 0.07).is_empty());
    }

    #[test]
    fn a_beat_is_moved_onto_the_hit_it_belongs_to_and_no_further() {
        let onsets = [(1.0 * RATE) as usize, (2.03 * RATE) as usize];
        let snapped = snap(&[0.98, 2.0, 3.0], &onsets);
        assert!((snapped[0] - (1.0 - PRE_ROLL)).abs() < 1e-4);
        assert!((snapped[1] - (2.03 - PRE_ROLL)).abs() < 1e-4);
        // Nothing within reach: left where it was.
        assert_eq!(snapped[2], 3.0);
        // Two beats are never moved onto one hit.
        let crowded = snap(&[0.97, 1.03], &onsets[..1]);
        assert!(crowded[1] >= crowded[0] + 0.05);
    }
}
