//! Finding the stretches worth looping.
//!
//! A good loop is whole bars of something that is actually playing, comes
//! back round to where it started, and is not just a shorter loop played
//! twice. The best of all is a part that plays alone in the original,
//! because that comes out with no trace of having been taken apart.

use crate::signal::percentile;
use crate::structure::{BarFeatures, Part};
use serde::{Deserialize, Serialize};

/// The lengths looked for, in bars.
pub const LENGTHS: [usize; 4] = [1, 2, 4, 8];

/// How many loops of each length are kept from each source.
fn keep(bars: usize) -> usize {
    match bars {
        1 => 2,
        2 => 3,
        4 => 4,
        _ => 2,
    }
}

/// Two stretches whose bars are this alike are the same pattern. Bar
/// pictures have what is common to the whole song taken out, so unrelated
/// bars score around zero and a pattern played again by hand scores 0.5 to
/// 0.8.
const SAME: f64 = 0.55;

/// Below this a loop is not worth showing.
const WORTH_KEEPING: f64 = 0.45;

/// The name of the source that is everything together.
pub const MIX: &str = "mix";

/// One signal loops are looked for in: a stem, or several together.
pub struct Source<'a> {
    /// `drums`, `bass`, or `mix`, `instrumental`, `melodic` for a blend.
    pub name: String,
    /// What a person calls it: "Drums", "No drums".
    pub label: String,
    /// The stems it is made of, by name; none for a recording that was
    /// not taken apart.
    pub stems: Vec<String>,
    /// Whether it is one stem alone.
    pub single: bool,
    pub features: &'a BarFeatures,
}

/// A stretch of whole bars worth looping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loop {
    /// `drums-2-17`: the source, the bars it lasts and the bar it starts on.
    pub id: String,
    pub source: String,
    pub label: String,
    /// The stems to cut it from. A loop that plays alone in the original
    /// names every stem: it is taken from the whole, untouched.
    pub stems: Vec<String>,
    /// Where it starts and ends, in seconds.
    pub start: f64,
    pub end: f64,
    /// Its first bar, counted from 0, and how many bars it lasts.
    pub bar: usize,
    pub bars: usize,
    /// Its own tempo: its beats over its length.
    pub bpm: f64,
    /// How good a loop it is, 0 to 1; loops come best first.
    pub score: f64,
    /// How well its end leads back into its start, 0 to 1.
    pub seam: f64,
    /// Whether it plays alone here in the original.
    pub solo: bool,
    /// How many times the pattern comes round in the recording, this one
    /// counted.
    pub rounds: usize,
    /// The letter of the part it lies in.
    pub part: String,
}

/// The loops worth having in every source, best first within each, the
/// sources in the order given.
///
/// `whole` describes everything together, which is what a source's share
/// of the sound is measured against; `all_stems` names every stem of the
/// recording.
pub fn find(
    sources: &[Source<'_>],
    whole: &BarFeatures,
    all_stems: &[String],
    bar_times: &[f64],
    beats_per_bar: usize,
    parts: &[Part],
) -> Vec<Loop> {
    let bars = bar_times.len().saturating_sub(1).min(whole.bars());
    if bars < 1 {
        return Vec::new();
    }
    let mut part_starts: Vec<usize> = parts.iter().map(|part| part.bar).collect();
    part_starts.sort_unstable();
    part_starts.dedup();
    let mut part_of = vec![0usize; bars];
    for (index, part) in parts.iter().enumerate() {
        for slot in part_of.iter_mut().skip(part.bar).take(part.bars) {
            *slot = index;
        }
    }

    let mut found = Vec::new();
    for source in sources {
        let feature = source.features;
        if feature.bars() < bars {
            continue;
        }
        let top = percentile(&feature.level, 90.0);
        if top < -70.0 {
            continue;
        }
        // Quieter than this against the source's usual level is bleed from
        // the other stems, not the part itself playing.
        let active: Vec<bool> =
            (0..bars).map(|bar| feature.level[bar] > top - 18.0 && feature.fill[bar] >= 0.3).collect();
        let similarity = feature.similarity();
        for length in LENGTHS {
            if bars < length {
                continue;
            }
            let starts = bars - length + 1;
            // How alike the `length` bars from one bar are to those from another.
            let alike = |a: usize, b: usize| -> f64 {
                (0..length).map(|k| similarity[a + k][b + k]).sum::<f64>() / length as f64
            };
            let mut candidates: Vec<(f64, usize, f64, usize, f64)> = Vec::new();
            for start in 0..starts {
                if !active[start..start + length].iter().all(|&playing| playing) {
                    continue;
                }
                if length >= 2 {
                    let half = length / 2;
                    let halves = (0..half).map(|k| similarity[start + k][start + half + k]).sum::<f64>() / half as f64;
                    if halves >= SAME {
                        // A shorter loop, twice.
                        continue;
                    }
                }
                let mut seams = Vec::new();
                if start + length < bars {
                    seams.push(similarity[start][start + length]);
                }
                if start > 0 {
                    seams.push(similarity[start - 1][start + length - 1]);
                }
                // Bars with the common part taken out compare from about
                // -0.3 (unrelated) to 1.
                let seam = if seams.is_empty() {
                    0.4
                } else {
                    (seams.iter().sum::<f64>() / seams.len() as f64 + 0.2).clamp(0.0, 1.0)
                };
                let repeats = (start % length..starts)
                    .step_by(length)
                    .filter(|&other| other != start && alike(start, other) >= SAME)
                    .count();
                let part_start = part_starts.iter().copied().filter(|&bar| bar <= start).max().unwrap_or(0);
                let aligned = if part_starts.contains(&start) {
                    1.0
                } else if (start - part_start) % length.max(2) == 0 {
                    0.6
                } else {
                    0.0
                };
                let level = ((feature.level[start..start + length].iter().sum::<f64>() / length as f64 - (top - 18.0))
                    / 18.0)
                    .clamp(0.0, 1.0);
                let mix_energy: f64 = whole.energy[start..start + length].iter().sum();
                let own: f64 = feature.energy[start..start + length].iter().sum();
                let purity = if source.name == MIX {
                    0.5
                } else if mix_energy > 0.0 {
                    (own / mix_energy).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let score =
                    0.38 * seam + 0.17 * repeats.min(6) as f64 / 6.0 + 0.20 * purity + 0.15 * aligned + 0.10 * level;
                candidates.push((score, start, seam, repeats, purity));
            }
            candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
            let mut kept: Vec<usize> = Vec::new();
            for (score, start, seam, repeats, purity) in candidates {
                if kept.len() >= keep(length) || score < WORTH_KEEPING {
                    break;
                }
                if kept.iter().any(|&other| start.abs_diff(other) < length || alike(start, other) >= SAME - 0.1) {
                    continue;
                }
                kept.push(start);
                let seconds = bar_times[start + length] - bar_times[start];
                let solo = source.single && purity >= 0.8;
                found.push(Loop {
                    id: format!("{}-{length}-{start}", source.name),
                    source: source.name.clone(),
                    label: source.label.clone(),
                    stems: if solo { all_stems.to_vec() } else { source.stems.clone() },
                    start: (bar_times[start] * 1e5).round() / 1e5,
                    end: (bar_times[start + length] * 1e5).round() / 1e5,
                    bar: start,
                    bars: length,
                    bpm: if seconds > 0.0 {
                        (60.0 * (length * beats_per_bar) as f64 / seconds * 100.0).round() / 100.0
                    } else {
                        0.0
                    },
                    score: (score * 1000.0).round() / 1000.0,
                    seam: (seam * 1000.0).round() / 1000.0,
                    solo,
                    rounds: repeats + 1,
                    part: parts.get(part_of[start]).map(|part| part.label.clone()).unwrap_or_default(),
                });
            }
        }
    }
    // Best first within each source; the sort keeps the sources' order.
    let order = |name: &str| sources.iter().position(|source| source.name == name).unwrap_or(usize::MAX);
    found.sort_by(|a, b| order(&a.source).cmp(&order(&b.source)).then(b.score.total_cmp(&a.score)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::tests::song;
    use crate::structure::{bar_features, parts};

    #[test]
    fn a_pattern_that_comes_round_is_found_once_and_ranked_by_how_it_closes() {
        let (signal, bar_times) = song("AABBAA");
        let features = bar_features(&signal, &bar_times, 4);
        let found_parts = parts(&features, &[], &bar_times);
        let sources = [Source {
            name: MIX.into(),
            label: "Full mix".into(),
            stems: Vec::new(),
            single: false,
            features: &features,
        }];
        let loops = find(&sources, &features, &[], &bar_times, 4, &found_parts);
        assert!(!loops.is_empty());
        // Each pattern repeats every bar, so nothing longer than a bar is a
        // loop of its own: two bars would be the one bar twice.
        assert!(loops.iter().all(|found| found.bars == 1), "{loops:?}");
        // One loop for each pattern, not one for each bar that holds it.
        assert_eq!(loops.len(), 2, "{loops:?}");
        let first = &loops[0];
        assert!(first.score >= loops[1].score);
        assert!(first.rounds >= 8, "{first:?}");
        assert_eq!(first.bpm, 120.0);
        assert!((first.end - first.start - 2.0).abs() < 1e-9);
        assert_eq!(first.id, format!("mix-1-{}", first.bar));
        assert!(!first.solo);
    }

    #[test]
    fn a_stem_that_plays_alone_is_cut_from_the_whole_and_a_silent_one_has_no_loops() {
        let (drums, bar_times) = song("AAAA");
        let silent = vec![0.0f32; drums.len()];
        let (whole, drum_features, silent_features) = (
            bar_features(&drums, &bar_times, 4),
            bar_features(&drums, &bar_times, 4),
            bar_features(&silent, &bar_times, 4),
        );
        let all = ["drums".to_owned(), "bass".to_owned()];
        let sources = [
            Source {
                name: "drums".into(),
                label: "Drums".into(),
                stems: vec!["drums".into()],
                single: true,
                features: &drum_features,
            },
            Source {
                name: "bass".into(),
                label: "Bass".into(),
                stems: vec!["bass".into()],
                single: true,
                features: &silent_features,
            },
        ];
        let loops = find(&sources, &whole, &all, &bar_times, 4, &[]);
        assert!(!loops.is_empty());
        assert!(loops.iter().all(|found| found.source == "drums"));
        assert!(loops[0].solo);
        assert_eq!(loops[0].stems, all);
    }

    #[test]
    fn a_recording_shorter_than_a_bar_has_no_loops() {
        let features = BarFeatures::default();
        assert!(find(&[], &features, &[], &[], 4, &[]).is_empty());
        assert!(find(&[], &features, &[], &[0.0], 4, &[]).is_empty());
    }
}
