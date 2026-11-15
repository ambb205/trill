//! Onset detection: where the drum hits and note attacks are.
//!
//! Both stretch modes need this, because a transient is the one thing that
//! must not be stretched: it has to be moved to its new place in time and
//! played back exactly as it was.
//!
//! Detection runs in two passes. A coarse pass follows the energy in a dozen
//! or so frequency bands, each on a logarithmic scale relative to its own
//! loudest moment so that a quiet hi-hat counts as much as a loud kick, and
//! looks for moments where bands rise together ("spectral flux"). That finds
//! onsets to within a few milliseconds.
//! A fine pass then walks the waveform around each one to find the sample
//! where the level jumps, because stretching by `r` multiplies whatever error
//! is left by `|1 - r|`.

use crate::fft::{RealFft, hann};

/// A detected onset.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Onset {
    /// Where the attack starts, in frames from the start of the audio.
    pub frame: usize,
    /// How pronounced it is: the average rise across the frequency bands, in
    /// nepers (1.0 is about 4.3 dB of power). Drum hits score from about 0.5
    /// (a ghost note under other sounds) to 9 (a hit out of silence).
    pub strength: f32,
}

/// The flux is measured between these frequencies, in bands a third of an
/// octave wide but never narrower than [`MIN_BAND_BINS`] bins: the energy of
/// a band made of one or two bins flickers too much to compare.
const LOWEST_HZ: f64 = 40.0;
const HIGHEST_HZ: f64 = 16_000.0;
const BAND_RATIO: f64 = 1.26;
const MIN_BAND_BINS: usize = 4;
/// Band energies are compared on a log scale that bottoms out this far
/// (40 dB) below the band's loudest moment, so that a noise floor does not
/// register however much it flickers.
const FLOOR: f32 = 1e-4;
/// A band that never gets within this (30 dB) of the loudest band is judged
/// against that level instead of its own, or a band holding nothing but
/// hiss would count for as much as the ones with the music in them.
const QUIET_BAND: f32 = 1e-3;
/// Each frame is compared with the one this many hops earlier: half a window,
/// the time a transient takes to move from the edge of the analysis window to
/// its middle.
const LAG: usize = 4;
/// Smallest flux that counts as an onset.
const MIN_STRENGTH: f32 = 0.12;
/// An onset must also stand out from its surroundings by this factor.
const LOCAL_FACTOR: f32 = 1.6;

/// Seconds either side of a flux peak in which nothing may be larger.
const PEAK_RADIUS: f64 = 0.030;
/// Seconds either side over which the surrounding flux is averaged.
const LOCAL_RADIUS: f64 = 0.100;
/// Onsets closer together than this are treated as one event (a flam).
const MIN_GAP: f64 = 0.025;
/// Lengths of the windows, in seconds, whose energies the fine pass compares:
/// just after a candidate sample against just before it.
const AFTER: f64 = 0.0015;
const BEFORE: f64 = 0.004;

/// Finds the onsets in a stereo signal. Both channels are measured and their
/// energies added, so a hit that is out of phase between the channels is
/// found as surely as a centered one.
pub(super) fn detect(left: &[f32], right: &[f32], sample_rate: f64) -> Vec<Onset> {
    let frames = left.len().min(right.len());
    let size = ((sample_rate * 0.016) as usize).next_power_of_two().max(64);
    if frames < size {
        return Vec::new();
    }
    let hop = size / 8;
    let mono = left[..frames] == right[..frames];
    let flux = spectral_flux(&left[..frames], (!mono).then_some(&right[..frames]), sample_rate, size);

    let peak_radius = ((PEAK_RADIUS * sample_rate) as usize / hop).max(1);
    let local_radius = ((LOCAL_RADIUS * sample_rate) as usize / hop).max(1);
    let mut onsets: Vec<Onset> = Vec::new();
    // Windows that hang over the end of the audio are left out: the cut-off
    // itself looks like a burst of new frequencies.
    let complete = (frames - size / 2) / hop + 1;
    for (index, &value) in flux.iter().enumerate().take(complete) {
        if value < MIN_STRENGTH {
            continue;
        }
        let near = &flux[index.saturating_sub(peak_radius)..(index + peak_radius + 1).min(flux.len())];
        let before = &flux[index.saturating_sub(peak_radius)..index];
        if near.iter().any(|&other| other > value) || before.contains(&value) {
            continue;
        }
        let around = &flux[index.saturating_sub(local_radius)..(index + local_radius + 1).min(flux.len())];
        let local_mean = around.iter().sum::<f32>() / around.len() as f32;
        if value < local_mean * LOCAL_FACTOR {
            continue;
        }

        // The flux peaks while the transient is still entering the analysis
        // window, so the attack lies between here and half a window ahead.
        let center = index * hop;
        let search = center.saturating_sub(hop)..(center + size / 2 + hop).min(frames);
        let frame = refine(&left[..frames], &right[..frames], search, sample_rate);
        match onsets.last_mut() {
            Some(last) if frame < last.frame + (MIN_GAP * sample_rate) as usize => {
                last.strength = last.strength.max(value);
            }
            _ => onsets.push(Onset { frame, strength: value }),
        }
    }
    onsets
}

/// The onset detection function: for each hop, the average rise in log band
/// energy since [`LAG`] hops earlier.
fn spectral_flux(left: &[f32], right: Option<&[f32]>, sample_rate: f64, size: usize) -> Vec<f32> {
    let hop = size / 8;
    let window = hann(size);
    let mut fft = RealFft::new(size);
    let bins = fft.bins();
    let (mut re, mut im, mut frame) = (vec![0.0; bins], vec![0.0; bins], vec![0.0; size]);

    // Band edges as bin indices. Bins below the first band (rumble, DC) and
    // above the last are ignored.
    let bin_hz = sample_rate / size as f64;
    let top = ((HIGHEST_HZ.min(sample_rate * 0.5) / bin_hz) as usize).min(bins);
    let mut edges = vec![((LOWEST_HZ / bin_hz).ceil() as usize).max(1)];
    while let Some(&low) = edges.last()
        && low < top
    {
        edges.push(((low as f64 * BAND_RATIO) as usize).max(low + MIN_BAND_BINS).min(top));
    }
    let band_count = edges.len() - 1;
    if band_count == 0 {
        return Vec::new();
    }

    let count = left.len() / hop + 1;
    let mut energy = vec![0.0f32; count * band_count];
    for (index, bands) in energy.chunks_exact_mut(band_count).enumerate() {
        // Frames are centered on multiples of the hop, reading silence
        // before the start, so a hit on the first sample is seen arriving.
        let start = (index * hop) as i64 - (size / 2) as i64;
        for channel in std::iter::once(left).chain(right) {
            for (i, slot) in frame.iter_mut().enumerate() {
                let position = start + i as i64;
                let sample = if position >= 0 { channel.get(position as usize).copied().unwrap_or(0.0) } else { 0.0 };
                *slot = sample * window[i];
            }
            fft.forward(&frame, &mut re, &mut im);
            for (band, range) in bands.iter_mut().zip(edges.windows(2)) {
                *band += (range[0]..range[1]).map(|bin| re[bin] * re[bin] + im[bin] * im[bin]).sum::<f32>();
            }
        }
    }

    let mut loudest = vec![0.0f32; band_count];
    for bands in energy.chunks_exact(band_count) {
        for (max, &value) in loudest.iter_mut().zip(bands) {
            *max = max.max(value);
        }
    }
    let overall = loudest.iter().fold(0.0f32, |max, &value| max.max(value));
    if overall <= 0.0 {
        return vec![0.0; count];
    }
    let scale: Vec<f32> = loudest.iter().map(|&max| 1.0 / (max.max(overall * QUIET_BAND) * FLOOR)).collect();
    for bands in energy.chunks_exact_mut(band_count) {
        for (value, scale) in bands.iter_mut().zip(&scale) {
            *value = (1.0 + *value * scale).ln();
        }
    }

    (0..count)
        .map(|index| {
            let now = &energy[index * band_count..(index + 1) * band_count];
            let rise: f32 = match index.checked_sub(LAG) {
                Some(earlier) => {
                    let then = &energy[earlier * band_count..(earlier + 1) * band_count];
                    now.iter().zip(then).map(|(a, b)| (a - b).max(0.0)).sum()
                }
                // Before the start there was silence.
                None => now.iter().sum(),
            };
            rise / band_count as f32
        })
        .collect()
}

/// Finds the sample in `search` where the attack starts: the point where the
/// energy just after most exceeds the energy just before.
fn refine(left: &[f32], right: &[f32], search: std::ops::Range<usize>, sample_rate: f64) -> usize {
    let after = ((AFTER * sample_rate) as usize).max(1);
    let before = ((BEFORE * sample_rate) as usize).max(1);
    let power = |i: usize| -> f64 {
        match (left.get(i), right.get(i)) {
            (Some(l), Some(r)) => (*l as f64).powi(2) + (*r as f64).powi(2),
            _ => 0.0,
        }
    };

    // Sliding sums of the two windows around each candidate.
    let first = search.start;
    let mut sum_after: f64 = (first..first + after).map(power).sum();
    let mut sum_before: f64 = (first.saturating_sub(before)..first).map(power).sum();
    let mut ratios = Vec::with_capacity(search.len());
    let mut loudest = 0.0f64;
    for i in search.clone() {
        ratios.push((sum_after / after as f64, sum_before / before as f64));
        loudest = loudest.max(sum_after / after as f64);
        sum_after += power(i + after) - power(i);
        sum_before += power(i) - if i >= before { power(i - before) } else { 0.0 };
    }
    // A floor 30 dB under the loudest moment keeps the ratio from exploding
    // in digital silence, where it would latch on to the first stray bit.
    let floor = loudest * 1e-3 + 1e-30;
    let ratio = |&(after, before): &(f64, f64)| after / (before + floor);

    let Some((best, peak)) =
        ratios.iter().map(ratio).enumerate().max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
    else {
        return search.start;
    };
    // While the window after the candidate is still sliding on to a sharp
    // attack the ratio sits on a plateau; the attack is at its far end.
    let plateau = ratios[best..].iter().map(ratio).take_while(|&value| value >= peak * 0.9).count();
    search.start + best + plateau - 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    const SR: f64 = 48_000.0;

    #[test]
    fn finds_clicks_to_the_sample() {
        let mut signal = vec![0.0f32; 96_000];
        let clicks = [0usize, 12_000, 30_001, 47_999, 72_345];
        for &click in &clicks {
            signal[click] = 0.8;
        }
        let found: Vec<usize> = detect(&signal, &signal, SR).iter().map(|onset| onset.frame).collect();
        assert_eq!(found, clicks);
    }

    #[test]
    fn finds_drum_like_hits_over_noise_and_ignores_steady_sound() {
        // Decaying noise bursts over a steady tone and a noise floor.
        let mut rng = Rng::new(3);
        let hits = [4_800usize, 19_200, 28_800, 43_200, 60_000];
        let mut signal: Vec<f32> = (0..72_000).map(|i| 0.2 * (i as f32 * 0.03).sin() + 0.003 * rng.bipolar()).collect();
        for &hit in &hits {
            for i in 0..4_000 {
                signal[hit + i] += 0.5 * rng.bipolar() * (-(i as f32) / 600.0).exp();
            }
        }
        // The tone and the hiss start abruptly on the first frame: that is an
        // onset too.
        let found: Vec<usize> = detect(&signal, &signal, SR).iter().map(|onset| onset.frame).collect();
        assert!(found[0] < 48, "found {found:?}");
        let found = &found[1..];
        assert_eq!(found.len(), hits.len(), "found {found:?}");
        for (found, hit) in found.iter().zip(hits) {
            assert!((*found as i64 - hit as i64).abs() <= 24, "hit at {hit} found at {found}");
        }

        let steady: Vec<f32> = (0..72_000).map(|i| 0.5 * (i as f32 * 0.03).sin()).collect();
        let found = detect(&steady, &steady, SR);
        // The tone switching on at the first sample is an onset; nothing after is.
        assert!(found.iter().all(|onset| onset.frame < 480), "found {found:?}");
    }

    #[test]
    fn out_of_phase_hits_are_found() {
        let mut left = vec![0.0f32; 48_000];
        left[24_000] = 0.9;
        let right: Vec<f32> = left.iter().map(|s| -s).collect();
        let found = detect(&left, &right, SR);
        assert_eq!(found.iter().map(|onset| onset.frame).collect::<Vec<_>>(), [24_000]);
    }
}
