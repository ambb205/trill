//! The shape of a recording: what each bar sounds like, where it changes,
//! and which stretches are the same part come round again.

use crate::signal::{RATE, db, percentile, resample};
use crate::spectrum::mel_power;
use serde::{Deserialize, Serialize};

const PICTURE_RATE: f64 = 22_050.0;
const PICTURE_HOP: usize = 256;
const BANDS: usize = 24;
const STEPS_PER_BEAT: usize = 4;

/// Every bar of one signal: a picture of its rhythm and colour, and its
/// level.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarFeatures {
    /// For each bar, its picture: unit length, with what all bars share
    /// taken out.
    pub pattern: Vec<Vec<f32>>,
    /// The level of each bar, in dB.
    pub level: Vec<f64>,
    /// The mean square of each bar.
    pub energy: Vec<f64>,
    /// The share of each bar, 0 to 1, with something sounding.
    pub fill: Vec<f64>,
}

impl BarFeatures {
    pub fn bars(&self) -> usize {
        self.pattern.len()
    }

    /// How alike two bars are: about 0 for unrelated bars, 0.5 to 0.8 for
    /// a pattern played again by hand, 1 for the same audio.
    pub fn alike(&self, a: usize, b: usize) -> f64 {
        self.pattern[a].iter().zip(&self.pattern[b]).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum()
    }

    /// [`BarFeatures::alike`] for every pair of bars, row by row.
    pub fn similarity(&self) -> Vec<Vec<f64>> {
        let bars = self.bars();
        (0..bars).map(|a| (0..bars).map(|b| self.alike(a, b)).collect()).collect()
    }
}

/// Describes every bar of a mono signal at [`RATE`], the bars starting at
/// `bar_times` seconds (one more time than there are bars).
///
/// Each bar is cut into sixteenth-note steps (four to a beat) and each step
/// into frequency bands, which captures both the rhythm and roughly what is
/// playing. Two bars holding the same pattern score close to 1 against each
/// other however the tempo wanders, because the steps follow the bar.
pub fn bar_features(signal: &[f32], bar_times: &[f64], beats_per_bar: usize) -> BarFeatures {
    let mut low = resample(signal, RATE, PICTURE_RATE);
    if low.len() < 2048 {
        low.resize(2048, 0.0);
    }
    let mel = mel_power(&low, PICTURE_RATE, 1024, PICTURE_HOP, BANDS, 30.0, PICTURE_RATE / 2.0);
    let frames = mel.len();
    let steps = beats_per_bar.max(1) * STEPS_PER_BEAT;
    let bars = bar_times.len().saturating_sub(1);

    // Running sums down every band, so that a step's energy is a difference.
    let mut running = vec![0.0f64; (frames + 1) * BANDS];
    for frame in 0..frames {
        for band in 0..BANDS {
            running[(frame + 1) * BANDS + band] = running[frame * BANDS + band] + f64::from(mel.row(frame)[band]);
        }
    }

    let mut out = BarFeatures::default();
    let mut step_energy = vec![0.0f64; bars * steps];
    for bar in 0..bars {
        let (start, end) = (bar_times[bar], bar_times[bar + 1]);
        let edges: Vec<usize> = (0..=steps)
            .map(|step| {
                let time = start + (end - start) * step as f64 / steps as f64;
                ((time * PICTURE_RATE / PICTURE_HOP as f64).round().max(0.0) as usize).min(frames)
            })
            .collect();
        let mut picture = vec![0.0f32; BANDS * steps];
        let mut total = 0.0;
        for step in 0..steps {
            let from = edges[step];
            let width = edges[step + 1].saturating_sub(from).max(1);
            let to = edges[step + 1].max(from + 1).min(frames);
            let mut across = 0.0;
            for band in 0..BANDS {
                let cell = (running[to * BANDS + band] - running[from.min(to) * BANDS + band]) / width as f64;
                across += cell;
                picture[band * steps + step] = (1000.0 * cell).ln_1p() as f32;
            }
            step_energy[bar * steps + step] = across / BANDS as f64;
            total += across;
        }
        let energy = total / (BANDS * steps) as f64;
        out.energy.push(energy);
        out.level.push(db(energy.max(1e-18).sqrt()));
        out.pattern.push(picture);
    }

    // Every bar of a song shares most of its picture: the same instruments
    // in the same registers. Taking that common part away leaves what makes
    // a bar itself, and only then does comparing two bars say much.
    if bars > 0 {
        let floor = percentile(&out.level, 90.0) - 24.0;
        let playing: Vec<usize> = (0..bars).filter(|&bar| out.level[bar] > floor).collect();
        if !playing.is_empty() {
            let mut common = vec![0.0f64; BANDS * steps];
            for &bar in &playing {
                for (slot, &value) in common.iter_mut().zip(&out.pattern[bar]) {
                    *slot += f64::from(value) / playing.len() as f64;
                }
            }
            for picture in &mut out.pattern {
                for (value, common) in picture.iter_mut().zip(&common) {
                    *value -= *common as f32;
                }
            }
        }
    }
    for picture in &mut out.pattern {
        let length = picture.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt().max(1e-9);
        picture.iter_mut().for_each(|value| *value = (f64::from(*value) / length) as f32);
    }
    // A step counts as sounding within 30 dB of the signal's loud steps.
    let loud = percentile(&step_energy, 95.0);
    out.fill = (0..bars)
        .map(|bar| {
            if loud <= 0.0 {
                return 0.0;
            }
            step_energy[bar * steps..(bar + 1) * steps].iter().filter(|&&energy| energy > loud * 1e-3).count() as f64
                / steps as f64
        })
        .collect();
    out
}

/// One part of a recording: a run of bars that belong together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Part {
    /// Where it starts and ends, in seconds.
    pub start: f64,
    pub end: f64,
    /// Its first bar, counted from 0, and how many bars it lasts.
    pub bar: usize,
    pub bars: usize,
    /// A letter: parts that sound alike share one, so that `A B A B C B`
    /// reads as a song.
    pub label: String,
    /// The stems that are not in it, by name.
    pub without: Vec<String>,
}

fn letter(index: usize) -> String {
    if index < 26 { char::from(b'A' + index as u8).to_string() } else { format!("A{}", index - 25) }
}

fn mean_of(values: &[Vec<f64>], rows: std::ops::Range<usize>, columns: std::ops::Range<usize>) -> f64 {
    let count = rows.len() * columns.len();
    if count == 0 {
        return 0.0;
    }
    rows.map(|row| values[row][columns.clone()].iter().sum::<f64>()).sum::<f64>() / count as f64
}

/// Divides a recording into parts and letters them.
///
/// Boundaries go where the music on one side stops resembling the music on
/// the other; a stem coming in or dropping out is the surest sign of one.
/// `mix` describes the whole, `stems` each stem that is had apart (none,
/// for a recording that was not taken apart).
pub fn parts(mix: &BarFeatures, stems: &[(String, &BarFeatures)], bar_times: &[f64]) -> Vec<Part> {
    let bars = bar_times.len().saturating_sub(1).min(mix.bars());
    if bars < 1 {
        return Vec::new();
    }
    // What a bar is, for telling parts apart: its pattern in the mix, and
    // how much of each stem is playing.
    let usual: Vec<f64> = stems.iter().map(|(_, features)| percentile(&features.level, 90.0)).collect();
    let presence: Vec<Vec<f64>> = (0..bars)
        .map(|bar| {
            stems
                .iter()
                .zip(&usual)
                .map(|((_, features), loud)| ((features.level[bar] - (loud - 30.0)) / 30.0).clamp(0.0, 1.0))
                .collect()
        })
        .collect();
    let cast = |range: std::ops::Range<usize>| -> Vec<f64> {
        (0..stems.len())
            .map(|stem| range.clone().map(|bar| presence[bar][stem]).sum::<f64>() / range.len().max(1) as f64)
            .collect()
    };
    let similarity = mix.similarity();

    let mut boundaries = vec![0, bars];
    if bars >= 8 {
        let half = 4;
        let mut novelty = vec![0.0f64; bars];
        for (bar, slot) in novelty.iter_mut().enumerate().take(bars - 1).skip(2) {
            let (before, after) = (bar.saturating_sub(half)..bar, bar..(bar + half).min(bars));
            let within = (mean_of(&similarity, before.clone(), before.clone())
                + mean_of(&similarity, after.clone(), after.clone()))
                / 2.0;
            let across = mean_of(&similarity, before.clone(), after.clone());
            let entering = cast(before)
                .iter()
                .zip(cast(after))
                .map(|(before, after)| (before - after).abs())
                .fold(0.0f64, f64::max);
            *slot = (within - across) + entering;
        }
        let threshold = (crate::signal::median(&novelty) + 0.3).max(0.6);
        let mut order: Vec<usize> = (0..bars).collect();
        order.sort_by(|&a, &b| novelty[b].total_cmp(&novelty[a]).then(a.cmp(&b)));
        let mut chosen: Vec<usize> = Vec::new();
        for bar in order {
            if novelty[bar] < threshold {
                break;
            }
            if (2..=bars - 2).contains(&bar) && chosen.iter().all(|&other| bar.abs_diff(other) >= 4) {
                chosen.push(bar);
            }
        }
        boundaries.extend(chosen);
        boundaries.sort_unstable();
        boundaries.dedup();
    }

    // Which parts are the same part: the same kind of bars, played by the
    // same stems.
    let spans: Vec<(usize, usize)> = boundaries.windows(2).map(|pair| (pair[0], pair[1])).collect();
    let width = mix.pattern.first().map_or(0, Vec::len);
    let centres: Vec<Vec<f64>> = spans
        .iter()
        .map(|&(start, end)| {
            let mut centre = vec![0.0f64; width];
            for bar in start..end {
                for (slot, &value) in centre.iter_mut().zip(&mix.pattern[bar]) {
                    *slot += f64::from(value);
                }
            }
            let length = centre.iter().map(|v| v * v).sum::<f64>().sqrt().max(1e-9);
            centre.iter().map(|v| v / length).collect()
        })
        .collect();
    let casts: Vec<Vec<f64>> = spans.iter().map(|&(start, end)| cast(start..end)).collect();
    let mut exemplars: Vec<usize> = Vec::new();
    let mut letters: Vec<usize> = Vec::new();
    for index in 0..spans.len() {
        let mut found = None;
        let mut best = 0.5;
        for (number, &exemplar) in exemplars.iter().enumerate() {
            let alike: f64 = centres[index].iter().zip(&centres[exemplar]).map(|(a, b)| a * b).sum();
            let same_cast = casts[index].iter().zip(&casts[exemplar]).all(|(a, b)| (a - b).abs() < 0.35);
            if same_cast && alike > best {
                found = Some(number);
                best = alike;
            }
        }
        letters.push(found.unwrap_or_else(|| {
            exemplars.push(index);
            exemplars.len() - 1
        }));
    }

    let round = |value: f64| (value * 10_000.0).round() / 10_000.0;
    spans
        .iter()
        .zip(&letters)
        .zip(&casts)
        .map(|((&(start, end), &number), cast)| Part {
            start: round(bar_times[start]),
            end: round(bar_times[end]),
            bar: start,
            bars: end - start,
            label: letter(number),
            without: stems
                .iter()
                .zip(cast)
                .filter(|(_, share)| **share < 0.15)
                .map(|((name, _), _)| name.clone())
                .collect(),
        })
        .collect()
}

/// How loud a signal is over time, 0 to 1, every tenth of a second:
/// enough to dim the stretches where a stem has nothing in it.
pub fn activity(signal: &[f32]) -> Vec<f32> {
    let hop = (0.1 * RATE) as usize;
    let count = (signal.len() / hop).max(1);
    let levels: Vec<f64> = (0..count)
        .map(|index| {
            let piece = &signal[(index * hop).min(signal.len())..((index + 1) * hop).min(signal.len())];
            db(crate::signal::rms(piece))
        })
        .collect();
    let top = percentile(&levels, 98.0);
    levels
        .iter()
        .map(|level| (((level - (top - 45.0)) / 45.0).clamp(0.0, 1.0) * 100.0).round() as f32 / 100.0)
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::signal::testing::{Noise, drum, hertz, silence, tone};

    /// A song in bars of two seconds (120 beats a minute, four to the bar):
    /// each letter of `form` is four bars of one of three patterns.
    pub(crate) fn song(form: &str) -> (Vec<f32>, Vec<f64>) {
        let bars = form.len() * 4;
        let mut signal = silence(bars as f64 * 2.0 + 0.5);
        let mut noise = Noise(21);
        for (index, part) in form.chars().enumerate() {
            for bar in 0..4 {
                let start = ((index * 4 + bar) * 2) as f64;
                for step in 0..16 {
                    let at = start + step as f64 * 0.125;
                    match part {
                        'A' => {
                            if step % 4 == 0 {
                                drum(&mut signal, at, "kick", 0.8, &mut noise);
                            }
                            if step % 8 == 4 {
                                drum(&mut signal, at, "snare", 0.6, &mut noise);
                            }
                        }
                        'B' => {
                            if step % 2 == 0 {
                                drum(&mut signal, at, "hat", 0.5, &mut noise);
                            }
                            if step == 0 || step == 6 || step == 10 {
                                drum(&mut signal, at, "kick", 0.8, &mut noise);
                            }
                            if step % 4 == 0 {
                                tone(&mut signal, at, 0.4, hertz(48.0 + (step / 4) as f64 * 3.0, 0.0), 0.4);
                            }
                        }
                        _ => {
                            if step % 8 == 0 {
                                tone(&mut signal, at, 0.9, hertz(72.0 + (bar % 2) as f64 * 5.0, 0.0), 0.5);
                            }
                            if step % 3 == 0 {
                                drum(&mut signal, at, "snare", 0.4, &mut noise);
                            }
                        }
                    }
                }
            }
        }
        (signal, (0..=bars).map(|bar| bar as f64 * 2.0).collect())
    }

    #[test]
    fn bars_that_hold_the_same_pattern_are_alike_and_others_are_not() {
        let (signal, bar_times) = song("ABA");
        let features = bar_features(&signal, &bar_times, 4);
        assert_eq!(features.bars(), 12);
        assert!(features.alike(0, 1) > 0.8, "{}", features.alike(0, 1));
        assert!(features.alike(1, 9) > 0.8, "{}", features.alike(1, 9));
        assert!(features.alike(1, 5) < 0.2, "{}", features.alike(1, 5));
        assert!(features.fill.iter().all(|&fill| fill > 0.2));
        assert!((features.alike(3, 3) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_song_is_divided_where_it_changes_and_parts_that_return_share_a_letter() {
        let (signal, bar_times) = song("ABABCB");
        let features = bar_features(&signal, &bar_times, 4);
        let parts = parts(&features, &[], &bar_times);
        let letters: String = parts.iter().map(|part| part.label.as_str()).collect();
        assert_eq!(letters, "ABABCB");
        assert!(parts.iter().all(|part| part.bars == 4), "{parts:?}");
        assert_eq!((parts[2].bar, parts[2].start, parts[2].end), (8, 16.0, 24.0));
    }

    #[test]
    fn a_part_says_which_stems_are_not_in_it() {
        let (drums, bar_times) = song("AAAA");
        let (mut keys, _) = song("CCCC");
        // The keys rest for the second half.
        let half = keys.len() / 2;
        keys[half..].fill(0.0);
        let mix: Vec<f32> = drums.iter().zip(&keys).map(|(a, b)| a + b).collect();
        let (whole, drums, keys) = (
            bar_features(&mix, &bar_times, 4),
            bar_features(&drums, &bar_times, 4),
            bar_features(&keys, &bar_times, 4),
        );
        let parts = parts(&whole, &[("drums".to_owned(), &drums), ("keys".to_owned(), &keys)], &bar_times);
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert_eq!(parts[0].without, Vec::<String>::new());
        assert_eq!(parts[1].without, ["keys"]);
        assert_ne!(parts[0].label, parts[1].label);
    }

    #[test]
    fn a_recording_too_short_for_bars_has_no_parts_and_a_quiet_stretch_is_dim() {
        let features = bar_features(&silence(0.1), &[], 4);
        assert_eq!(features.bars(), 0);
        assert!(parts(&features, &[], &[]).is_empty());
        let mut signal = silence(2.0);
        tone(&mut signal, 1.0, 0.9, 220.0, 0.5);
        let curve = activity(&signal);
        assert_eq!(curve.len(), 20);
        assert!(curve[2] < 0.05 && curve[14] > 0.9, "{curve:?}");
    }
}
