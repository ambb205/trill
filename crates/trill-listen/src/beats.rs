//! The beat grid: where the beats and bars fall, and the tempo.
//!
//! Without a model, beats are found by following the pulse of the hits:
//! the tempo is the lag at which the onsets most resemble themselves, and
//! the beats are the path through the onsets that keeps closest to that
//! tempo. The start of the bar is put where the chords change and the low
//! drums land. That assumes four beats to a bar and is less sure of the
//! "one" than of the beats, which is what the corrections are for
//! ([`Grid::corrected`]): half or double the tempo, move the bar one beat
//! later, or set the beats in a bar.

use crate::harmony::{beat_chroma, chroma_of};
use crate::signal::{RATE, median, resample};
use crate::spectrum::{onset_strength, pick_peaks, power};
use serde::{Deserialize, Serialize};
use trill_dsp::fft::{RealFft, hann};

/// The rate and hop the pulse is followed at.
const PULSE_RATE: f64 = 22_050.0;
const PULSE_HOP: usize = 256;
/// How strongly the beats are held to the tempo: a beat that comes at
/// twice or half its time costs this times the square of one octave.
const TIGHTNESS: f64 = 100.0;

/// Who found the beats.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tracker {
    /// The pulse of the onsets: no model.
    #[default]
    Onsets,
    /// A model that was trained to hear beats.
    Model,
}

/// One way a grid can be reread when the tracker got it wrong.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Correction {
    /// Every other beat is a beat: half the tempo.
    Half,
    /// There is a beat between every two: double the tempo.
    Double,
    /// The bar starts one beat later.
    Rotate,
    /// This many beats to a bar, 2 to 12.
    BeatsPerBar(u8),
}

impl Correction {
    /// Reads `half`, `double`, `rotate`, or `bar3` to `bar12`.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "half" => Some(Self::Half),
            "double" => Some(Self::Double),
            "rotate" => Some(Self::Rotate),
            other => {
                other.strip_prefix("bar")?.parse().ok().filter(|count| (2..=12).contains(count)).map(Self::BeatsPerBar)
            }
        }
    }

    /// The word [`Correction::parse`] reads it from.
    pub fn name(&self) -> String {
        match self {
            Self::Half => "half".to_owned(),
            Self::Double => "double".to_owned(),
            Self::Rotate => "rotate".to_owned(),
            Self::BeatsPerBar(count) => format!("bar{count}"),
        }
    }
}

/// Every beat of a recording, which of them start a bar, and the tempo.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Grid {
    /// The time of each beat, in seconds from the start of the recording.
    pub beats: Vec<f64>,
    /// Which beats start a bar, as numbers into `beats`.
    pub bars: Vec<usize>,
    /// Beats a minute, from the middle of the distribution of beat lengths.
    pub bpm: f64,
    pub beats_per_bar: usize,
    /// Whether the tempo holds: a loop cut from a steady recording is its
    /// bars long on any grid.
    pub steady: bool,
    /// How much the beat lengths vary, as a share of their mean.
    pub drift: f64,
    pub tracker: Tracker,
}

fn round_to(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

/// Tempo and how much it wanders, from the lengths of the beats: taken
/// from the middle of their distribution, so that a free introduction or a
/// dropped beat does not pull it.
fn tempo_of(beats: &[f64]) -> (f64, f64, bool) {
    let intervals: Vec<f64> = beats.windows(2).map(|pair| pair[1] - pair[0]).collect();
    if intervals.is_empty() {
        return (0.0, 0.0, false);
    }
    let period = median(&intervals);
    let centre: Vec<f64> =
        intervals.iter().copied().filter(|interval| (interval - period).abs() < 0.08 * period).collect();
    if centre.is_empty() {
        return (60.0 / period, 0.0, false);
    }
    let mean = centre.iter().sum::<f64>() / centre.len() as f64;
    let drift = if centre.len() > 1 {
        (centre.iter().map(|interval| (interval - mean).powi(2)).sum::<f64>() / centre.len() as f64).sqrt() / mean
    } else {
        0.0
    };
    (60.0 / mean, drift, centre.len() as f64 >= 0.85 * intervals.len() as f64 && drift < 0.02)
}

/// The tempo of beats that lie where they were heard. Beats on hits are
/// the tempo to the sample, and the few that found no hit (past the last
/// one, in a gap) are not: where most beat lengths agree to a fiftieth,
/// those are the tempo.
fn agreed_tempo(beats: &[f64]) -> f64 {
    let intervals: Vec<f64> = beats.windows(2).map(|pair| pair[1] - pair[0]).collect();
    let period = median(&intervals);
    let close: Vec<f64> =
        intervals.iter().copied().filter(|interval| (interval - period).abs() < 0.02 * period).collect();
    if close.len() * 2 >= intervals.len() && !close.is_empty() {
        60.0 * close.len() as f64 / close.iter().sum::<f64>()
    } else {
        tempo_of(beats).0
    }
}

impl Grid {
    /// The grid of a recording `seconds` long from the beats and downbeats
    /// a tracker found. It runs to both ends of the audio, continuing the
    /// nearest tempo over any lead-in or tail the tracker left out. Fewer
    /// than four beats are no grid.
    pub fn build(
        beats: &[f64],
        downbeats: &[f64],
        seconds: f64,
        beats_per_bar: Option<usize>,
        tracker: Tracker,
    ) -> Self {
        let mut beats: Vec<f64> = beats.iter().copied().filter(|&time| time >= 0.0 && time <= seconds).collect();
        beats.sort_by(f64::total_cmp);
        beats.dedup();
        if beats.len() < 4 {
            return Self { beats_per_bar: 4, tracker, ..Self::default() };
        }
        let intervals: Vec<f64> = beats.windows(2).map(|pair| pair[1] - pair[0]).collect();

        // Which beats are downbeats.
        let mut down: Vec<usize> = downbeats
            .iter()
            .filter(|&&time| time >= 0.0 && time <= seconds)
            .filter_map(|&time| {
                (0..beats.len()).min_by(|&a, &b| (beats[a] - time).abs().total_cmp(&(beats[b] - time).abs()))
            })
            .collect();
        down.sort_unstable();
        down.dedup();
        let beats_per_bar = beats_per_bar.unwrap_or_else(|| {
            // What most bars of the tracker hold.
            let mut counts = [0usize; 13];
            for pair in down.windows(2) {
                if (2..=12).contains(&(pair[1] - pair[0])) {
                    counts[pair[1] - pair[0]] += 1;
                }
            }
            (2..=12)
                .max_by(|&a, &b| counts[a].cmp(&counts[b]).then(b.cmp(&a)))
                .filter(|&count| counts[count] > 0)
                .unwrap_or(4)
        });
        if down.is_empty() {
            down.push(0);
        }

        // Continue the beats to the start and to the end.
        let head_period = median(&intervals[..intervals.len().min(8)]);
        let tail_period = median(&intervals[intervals.len().saturating_sub(8)..]);
        let mut head = Vec::new();
        let mut time = beats[0] - head_period;
        while time >= 0.0 {
            head.push(time);
            time -= head_period;
        }
        head.reverse();
        let mut all = head.clone();
        all.extend(&beats);
        let mut time = beats[beats.len() - 1] + tail_period;
        while time <= seconds - 0.05 {
            all.push(time);
            time += tail_period;
        }

        // Bars: the tracker's downbeats, then every bar's worth of beats
        // outward from the first and the last of them.
        let mut is_bar = vec![false; all.len()];
        for &index in &down {
            is_bar[index + head.len()] = true;
        }
        let (first, last) = (down[0] + head.len(), down[down.len() - 1] + head.len());
        let mut index = first;
        while index >= beats_per_bar {
            index -= beats_per_bar;
            is_bar[index] = true;
        }
        let mut index = last + beats_per_bar;
        while index < all.len() {
            is_bar[index] = true;
            index += beats_per_bar;
        }

        let (bpm, drift, steady) = tempo_of(&beats);
        Self {
            beats: all.iter().map(|&time| round_to(time, 5)).collect(),
            bars: (0..is_bar.len()).filter(|&index| is_bar[index]).collect(),
            bpm: round_to(bpm, 2),
            beats_per_bar,
            steady,
            drift: round_to(drift, 4),
            tracker,
        }
    }

    /// This grid with its tempo taken again from its beats as they now
    /// lie. A tracker says where the beats are to a hundredth of a second;
    /// once they have been moved onto the hits they belong to, their
    /// lengths say the tempo of a short loop far better.
    pub fn measured(mut self) -> Self {
        if self.beats.len() >= 4 {
            let (_, drift, steady) = tempo_of(&self.beats);
            (self.bpm, self.drift, self.steady) = (round_to(agreed_tempo(&self.beats), 2), round_to(drift, 4), steady);
        }
        self
    }

    /// This grid reread: see [`Correction`]. Loops, parts and chords follow
    /// from the grid and are to be worked out again from the result.
    pub fn corrected(&self, correction: Correction) -> Self {
        if self.beats.len() < 4 {
            return self.clone();
        }
        let mut beats = self.beats.clone();
        let mut per_bar = self.beats_per_bar.max(1);
        let mut first = self.bars.first().copied().unwrap_or(0);
        match correction {
            Correction::Double => {
                let mut doubled = Vec::with_capacity(beats.len() * 2);
                for pair in beats.windows(2) {
                    doubled.push(pair[0]);
                    doubled.push((pair[0] + pair[1]) / 2.0);
                }
                doubled.push(beats[beats.len() - 1]);
                beats = doubled;
                first *= 2;
            }
            Correction::Half => {
                let offset = first % 2;
                beats = beats.into_iter().skip(offset).step_by(2).collect();
                first = (first - offset) / 2;
            }
            Correction::Rotate => first += 1,
            Correction::BeatsPerBar(count) => per_bar = usize::from(count.clamp(2, 12)),
        }
        first %= per_bar;
        let bpm = agreed_tempo(&beats);
        Self {
            bars: (first..beats.len()).step_by(per_bar).collect(),
            beats: beats.iter().map(|&time| round_to(time, 5)).collect(),
            bpm: round_to(bpm, 2),
            beats_per_bar: per_bar,
            ..self.clone()
        }
    }

    /// The beat of the grid that bar `bar` begins on, bars counted from 0
    /// at the first bar line. A bar is as long as it was found: `bars`
    /// says where each begins, and a bar is not always `beats_per_bar`
    /// beats (a bar of two in a song of fours). Before the first line and
    /// past the last, bars are counted on at `beats_per_bar`.
    ///
    /// This, with [`Grid::bar_of_beat`], is the one numbering of a
    /// recording's bars: whatever names a bar of a recording, or says
    /// which bar a moment lies in, asks here.
    pub fn bar_beat(&self, bar: i64) -> f64 {
        let per_bar = self.beats_per_bar.max(1) as f64;
        let (Some(&first), Some(&last)) = (self.bars.first(), self.bars.last()) else {
            return bar as f64 * per_bar;
        };
        let lines = self.bars.len() as i64;
        if bar < 0 {
            first as f64 + bar as f64 * per_bar
        } else if bar >= lines {
            last as f64 + (bar - (lines - 1)) as f64 * per_bar
        } else {
            self.bars[bar as usize] as f64
        }
    }

    /// The bar a beat of the grid lies in, counted as [`Grid::bar_beat`]
    /// counts: negative before the first bar line.
    pub fn bar_of_beat(&self, beat: f64) -> i64 {
        let per_bar = self.beats_per_bar.max(1) as f64;
        let (Some(&first), Some(&last)) = (self.bars.first(), self.bars.last()) else {
            return (beat / per_bar).floor() as i64;
        };
        if beat < first as f64 {
            ((beat - first as f64) / per_bar).floor() as i64
        } else if beat >= last as f64 {
            self.bars.len() as i64 - 1 + ((beat - last as f64) / per_bar).floor() as i64
        } else {
            self.bars.partition_point(|&line| line as f64 <= beat) as i64 - 1
        }
    }

    /// How many beats long bar `bar` is.
    pub fn bar_length(&self, bar: i64) -> f64 {
        self.bar_beat(bar + 1) - self.bar_beat(bar)
    }

    /// When bar `bar` begins, in seconds of the recording.
    pub fn bar_start(&self, bar: i64) -> f64 {
        self.seconds_at(self.bar_beat(bar))
    }

    /// The bar a moment of the recording lies in, from 0, and how many
    /// beats into that bar it is. A hair before a line counts as on it.
    pub fn place(&self, seconds: f64) -> (i64, f64) {
        let beat = self.beat_at(seconds) + 1e-6;
        let bar = self.bar_of_beat(beat);
        (bar, beat - self.bar_beat(bar))
    }

    /// The start of every bar in seconds, closed off with the end of the
    /// last when the recording, `seconds` long, lasts that long.
    pub fn bar_times(&self, seconds: f64) -> Vec<f64> {
        let mut times: Vec<f64> = self.bars.iter().filter_map(|&index| self.beats.get(index).copied()).collect();
        let Some(&last) = self.bars.last() else { return times };
        if let Some(&end) = self.beats.get(last + self.beats_per_bar) {
            times.push(end);
        } else if times.len() > 1 {
            let end = times[times.len() - 1] + (times[times.len() - 1] - times[times.len() - 2]);
            if end <= seconds + 0.02 {
                times.push(end.min(seconds));
            }
        }
        times
    }

    /// Where `seconds` lies among the beats: beat 0 is the first beat of
    /// the grid, and a time before it or after the last is counted on at
    /// the nearest tempo. A recording without a grid is counted at two
    /// beats a second.
    pub fn beat_at(&self, seconds: f64) -> f64 {
        let beats = &self.beats;
        if beats.len() < 2 {
            return seconds * 2.0;
        }
        let last = beats.len() - 1;
        if seconds <= beats[0] {
            return (seconds - beats[0]) / (beats[1] - beats[0]);
        }
        if seconds >= beats[last] {
            return last as f64 + (seconds - beats[last]) / (beats[last] - beats[last - 1]);
        }
        let after = beats.partition_point(|&beat| beat <= seconds);
        (after - 1) as f64 + (seconds - beats[after - 1]) / (beats[after] - beats[after - 1])
    }

    /// The time of a beat counted as [`Grid::beat_at`] counts it.
    pub fn seconds_at(&self, beat: f64) -> f64 {
        let beats = &self.beats;
        if beats.len() < 2 {
            return beat / 2.0;
        }
        let last = beats.len() - 1;
        if beat <= 0.0 {
            return beats[0] + beat * (beats[1] - beats[0]);
        }
        if beat >= last as f64 {
            return beats[last] + (beat - last as f64) * (beats[last] - beats[last - 1]);
        }
        let index = beat.floor() as usize;
        beats[index] + (beat - index as f64) * (beats[index + 1] - beats[index])
    }
}

/// The tempo the onsets suggest, in beats a minute: the lag at which
/// windows of the envelope most resemble themselves, with tempos near 120
/// preferred over their halves and doubles.
fn pulse_tempo(envelope: &[f32], per_second: f64) -> f64 {
    // Eight seconds of envelope at a time.
    let window = ((8.0 * per_second) as usize).max(16);
    let size = (2 * window).next_power_of_two();
    let mut fft = RealFft::new(size);
    let shape = hann(window);
    let bins = size / 2 + 1;
    let (mut frame, mut re, mut im, mut alike) =
        (vec![0.0f32; size], vec![0.0f32; bins], vec![0.0f32; bins], vec![0.0f32; size]);
    let mut total = vec![0.0f64; window];
    let mut count = 0usize;
    for centre in (0..envelope.len()).step_by(8) {
        frame.fill(0.0);
        for offset in 0..window {
            let at = centre + offset;
            if at >= window / 2 && at - window / 2 < envelope.len() {
                frame[offset] = envelope[at - window / 2] * shape[offset];
            }
        }
        fft.forward(&frame, &mut re, &mut im);
        for (re, im) in re.iter_mut().zip(im.iter_mut()) {
            *re = *re * *re + *im * *im;
            *im = 0.0;
        }
        fft.inverse(&re, &im, &mut alike);
        // Each window counts alike, however loud.
        if alike[0] > 1e-12 {
            for (slot, &value) in total.iter_mut().zip(&alike) {
                *slot += f64::from(value / alike[0]);
            }
            count += 1;
        }
    }
    if count == 0 {
        return 120.0;
    }
    let mut best = (f64::NEG_INFINITY, 120.0);
    for (lag, sum) in total.iter().enumerate().skip(1) {
        let bpm = 60.0 * per_second / lag as f64;
        if bpm > 320.0 {
            continue;
        }
        let prior = -0.5 * (bpm / 120.0).log2().powi(2);
        let score = (1e6 * (sum / count as f64).max(0.0)).ln_1p() + prior;
        if score > best.0 {
            best = (score, bpm);
        }
    }
    best.1
}

/// The frames of `envelope` that are beats at about `bpm`: the path
/// through the onsets that scores best when every step off the tempo is
/// paid for.
fn pulse_beats(envelope: &[f32], bpm: f64, per_second: f64) -> Vec<usize> {
    let period = (60.0 * per_second / bpm).round().max(1.0);
    let count = envelope.len();
    if count == 0 {
        return Vec::new();
    }
    // Onsets in units of their own deviation, each spread over the frames
    // near it so that a beat a frame off its hit still scores.
    let mean = envelope.iter().map(|&v| f64::from(v)).sum::<f64>() / count as f64;
    let deviation =
        (envelope.iter().map(|&v| (f64::from(v) - mean).powi(2)).sum::<f64>() / (count.max(2) - 1) as f64).sqrt();
    let reach = period as isize;
    let spread: Vec<f64> =
        (-reach..=reach).map(|offset| (-0.5 * (offset as f64 * 32.0 / period).powi(2)).exp()).collect();
    let local: Vec<f64> = (0..count as isize)
        .map(|index| {
            let mut sum = 0.0;
            for (offset, weight) in (-reach..=reach).zip(&spread) {
                // Beyond three widths the weight is nothing.
                if *weight < 1e-9 {
                    continue;
                }
                if let Some(&value) = usize::try_from(index - offset).ok().and_then(|at| envelope.get(at)) {
                    sum += weight * f64::from(value) / (deviation + 1e-30);
                }
            }
            sum
        })
        .collect();
    let top = local.iter().copied().fold(0.0f64, f64::max);

    // A beat follows the one before by half a period to two.
    let (far, near) = ((2.0 * period) as isize, (period / 2.0).round() as isize);
    let costs: Vec<f64> = (near..=far).rev().map(|back| -TIGHTNESS * (back as f64 / period).ln().powi(2)).collect();
    let mut total = vec![0.0f64; count];
    let mut link = vec![-1isize; count];
    let mut started = false;
    for index in 0..count as isize {
        let mut best = (f64::NEG_INFINITY, -1isize);
        for (back, cost) in (near..=far).rev().zip(&costs) {
            let at = index - back;
            let score = cost + if at >= 0 { total[at as usize] } else { 0.0 };
            if score > best.0 {
                best = (score, at);
            }
        }
        total[index as usize] = local[index as usize] + best.0;
        // Nothing is a beat before the music starts.
        if !started && local[index as usize] < 0.01 * top {
            link[index as usize] = -1;
        } else {
            link[index as usize] = best.1;
            started = true;
        }
    }

    // The last beat: the last crest of the running score that stands at
    // least half as high as crests usually do.
    let crests: Vec<usize> =
        (1..count).filter(|&i| total[i] > total[i - 1] && (i + 1 == count || total[i] >= total[i + 1])).collect();
    if crests.is_empty() {
        return Vec::new();
    }
    let usual = median(&crests.iter().map(|&index| total[index]).collect::<Vec<f64>>());
    let Some(&last) = crests.iter().rev().find(|&&index| total[index] * 2.0 > usual) else { return Vec::new() };
    let mut beats = vec![last];
    let mut at = link[last];
    while at >= 0 {
        beats.push(at as usize);
        at = link[at as usize];
    }
    beats.reverse();
    beats
}

/// Beat and downbeat times of a recording, in seconds, by following the
/// pulse of `hits` (mono at [`RATE`]: the drums when they are had apart,
/// otherwise everything) and putting the start of the bar where the chords
/// of `pitched` change and the low drums land.
pub fn track(hits: &[f32], pitched: &[f32]) -> (Vec<f64>, Vec<f64>) {
    let signal = resample(hits, RATE, PULSE_RATE);
    if signal.len() < 4096 || signal.iter().all(|sample| sample.abs() < 1e-4) {
        return (Vec::new(), Vec::new());
    }
    let per_second = PULSE_RATE / PULSE_HOP as f64;
    let envelope = onset_strength(&signal, PULSE_RATE, 2048, PULSE_HOP, 128);
    // A pulse takes a few hits to be one: a single hit, or a sound that
    // only swells, has no tempo.
    let top = envelope.iter().copied().fold(0.0f32, f32::max);
    let scaled: Vec<f32> = envelope.iter().map(|value| value / top.max(1e-12)).collect();
    if pick_peaks(&scaled, 3, 9, 0.1, 5).len() < 4 {
        return (Vec::new(), Vec::new());
    }
    let frames = pulse_beats(&envelope, pulse_tempo(&envelope, per_second), per_second);
    let beats: Vec<f64> = frames.iter().map(|&frame| frame as f64 / per_second).collect();
    if beats.len() < 8 {
        return (beats.clone(), beats.into_iter().step_by(4).collect());
    }

    // Chords mostly change on the first beat of a bar.
    let (chroma, _) = chroma_of(pitched);
    let synced = beat_chroma(&chroma, &beats);
    let unit: Vec<[f64; 12]> = (0..synced.len())
        .map(|index| {
            let row = synced.row(index);
            let length = row.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>().sqrt().max(1e-9);
            std::array::from_fn(|class| f64::from(row[class]) / length)
        })
        .collect();
    let mut change = vec![0.0f64; beats.len()];
    for index in 1..unit.len() {
        change[index] = 1.0 - unit[index].iter().zip(&unit[index - 1]).map(|(a, b)| a * b).sum::<f64>();
    }

    // And the kick lands on it: energy under 150 Hz just after each beat.
    let spectrum = power(&signal, 2048, PULSE_HOP);
    let top_bin = (150.0 * 2048.0 / PULSE_RATE) as usize + 1;
    let low: Vec<f64> =
        (0..spectrum.len()).map(|frame| spectrum.row(frame)[..top_bin].iter().map(|&p| f64::from(p)).sum()).collect();
    let span = (0.09 * per_second) as usize;
    let thump: Vec<f64> = frames
        .iter()
        .map(|&frame| {
            let from = frame.min(low.len().saturating_sub(1));
            let to = (from + span).min(low.len()).max(from + 1);
            low[from..to].iter().sum::<f64>() / (to - from) as f64
        })
        .collect();
    let usual = thump.iter().sum::<f64>() / thump.len() as f64 + 1e-12;
    let thump: Vec<f64> = thump.iter().map(|value| (value / usual).ln_1p()).collect();

    let standard = |values: &[f64]| -> Vec<f64> {
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let deviation = (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt() + 1e-9;
        values.iter().map(|v| (v - mean) / deviation).collect()
    };
    let (change, thump) = (standard(&change), standard(&thump));
    let mean_from = |values: &[f64], phase: usize| -> f64 {
        let picked: Vec<f64> = values.iter().skip(phase).step_by(4).copied().collect();
        picked.iter().sum::<f64>() / picked.len().max(1) as f64
    };
    let phase = (0..4)
        .max_by(|&a, &b| {
            let score = |phase| mean_from(&change, phase) + 0.5 * mean_from(&thump, phase);
            score(a).total_cmp(&score(b)).then(b.cmp(&a))
        })
        .unwrap_or(0);
    let downbeats = beats.iter().skip(phase).step_by(4).copied().collect();
    (beats, downbeats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::testing::{Noise, drum, hertz, silence, tone};

    /// A drum part at `bpm`: kick on one and three, snare on two and four,
    /// hats on the eighths, starting `lead` seconds in, with a bass note
    /// that changes on the first beat of every bar.
    fn groove(bpm: f64, bars: usize, lead: f64) -> (Vec<f32>, Vec<f32>) {
        let beat = 60.0 / bpm;
        let seconds = lead + bars as f64 * 4.0 * beat + 0.5;
        let (mut drums, mut pitched) = (silence(seconds), silence(seconds));
        let mut noise = Noise(11);
        for bar in 0..bars {
            for step in 0..8 {
                let at = lead + (bar * 4) as f64 * beat + step as f64 * beat / 2.0;
                drum(&mut drums, at, "hat", 0.25, &mut noise);
                match step {
                    0 | 4 => drum(&mut drums, at, "kick", 0.9, &mut noise),
                    2 | 6 => drum(&mut drums, at, "snare", 0.7, &mut noise),
                    _ => {}
                }
            }
            let root = [36.0, 41.0, 43.0, 38.0][bar % 4];
            let at = lead + (bar * 4) as f64 * beat;
            tone(&mut pitched, at, 4.0 * beat - 0.02, hertz(root, 0.0), 0.5);
            tone(&mut pitched, at, 4.0 * beat - 0.02, hertz(root + 19.0, 0.0), 0.2);
        }
        (drums, pitched)
    }

    #[test]
    fn the_beats_and_bars_of_a_steady_groove_are_found() {
        let (drums, pitched) = groove(96.0, 12, 0.4);
        let mix: Vec<f32> = drums.iter().zip(&pitched).map(|(a, b)| a + b).collect();
        let (beats, downbeats) = track(&mix, &pitched);
        let seconds = mix.len() as f64 / RATE;
        let grid = Grid::build(&beats, &downbeats, seconds, None, Tracker::Onsets);
        assert!((grid.bpm - 96.0).abs() < 0.6, "{}", grid.bpm);
        assert_eq!(grid.beats_per_bar, 4);
        assert!(grid.steady, "drift {}", grid.drift);
        // Every beat of the part has a beat of the grid within 35 ms.
        let beat = 60.0 / 96.0;
        for index in 0..48 {
            let time = 0.4 + index as f64 * beat;
            let nearest = grid.beats.iter().map(|found| (found - time).abs()).fold(f64::INFINITY, f64::min);
            assert!(nearest < 0.035, "beat {index} at {time}: nearest is {nearest} away");
        }
        // The bars start where the bass changes and the kick lands.
        let first = grid.beats[grid.bars[0]];
        let into_bar = ((first - 0.4) / beat).round() as i64;
        assert_eq!(into_bar.rem_euclid(4), 0, "the first bar starts at {first}");
        assert!(grid.bar_times(seconds).len() >= 12);
    }

    #[test]
    fn a_grid_runs_to_both_ends_of_the_recording_and_counts_its_bars() {
        let beats: Vec<f64> = (0..16).map(|index| 2.0 + index as f64 * 0.5).collect();
        let downbeats = [3.0, 5.0, 7.0];
        let grid = Grid::build(&beats, &downbeats, 12.0, None, Tracker::Model);
        assert_eq!(grid.beats.first(), Some(&0.0));
        assert!((grid.beats.last().unwrap() - 11.5).abs() < 1e-9);
        assert_eq!(grid.bpm, 120.0);
        assert_eq!(grid.beats_per_bar, 4);
        assert!(grid.steady);
        // Bars at 1, 3, 5, 7, 9 and 11 seconds: every fourth beat, outward from the tracker's.
        let starts: Vec<f64> = grid.bars.iter().map(|&index| grid.beats[index]).collect();
        assert_eq!(starts, [1.0, 3.0, 5.0, 7.0, 9.0, 11.0]);
        // The last bar would end past the audio: the list ends where it starts.
        assert_eq!(grid.bar_times(12.0), [1.0, 3.0, 5.0, 7.0, 9.0, 11.0]);
        assert_eq!(grid.tracker, Tracker::Model);
        // Three beats are no grid.
        assert!(Grid::build(&[0.0, 0.5, 1.0], &[], 2.0, None, Tracker::Onsets).beats.is_empty());
    }

    #[test]
    fn a_bar_is_as_long_as_it_was_found_and_every_way_of_counting_bars_agrees() {
        // Beats every half second from 1 s on. The tracker found a bar of two among bars of four: lines at
        // 1, 3, 5, 6, 8 and 10 seconds.
        let beats: Vec<f64> = (0..20).map(|index| 1.0 + index as f64 * 0.5).collect();
        let grid = Grid::build(&beats, &[1.0, 3.0, 5.0, 6.0, 8.0, 10.0], 11.0, None, Tracker::Model);
        assert_eq!(grid.beats_per_bar, 4, "what most of its bars hold");
        let starts: Vec<f64> = (0..6).map(|bar| grid.bar_start(bar)).collect();
        assert_eq!(starts, [1.0, 3.0, 5.0, 6.0, 8.0, 10.0]);
        assert_eq!(grid.bar_times(11.0)[..6], starts[..], "the list of lines is the same lines");
        let lengths: Vec<f64> = (0..5).map(|bar| grid.bar_length(bar)).collect();
        assert_eq!(lengths, [4.0, 4.0, 2.0, 4.0, 4.0]);
        // A moment is in the bar whose line is the last at or before it, so many beats in.
        let (bar, beat) = grid.place(5.5);
        assert!(bar == 2 && (beat - 1.0).abs() < 1e-5, "{bar} {beat}");
        assert_eq!(grid.place(6.0).0, 3, "the bar after the short one begins where the short one ends");
        assert_eq!(grid.place(7.99).0, 3);
        assert_eq!(grid.place(2.999_999_9).0, 1, "a hair before a line is on it");
        // Counting there and back is the same bar, for every beat of the grid and past both its ends.
        for bar in -3..9 {
            assert_eq!(grid.bar_of_beat(grid.bar_beat(bar)), bar);
            assert_eq!(grid.bar_of_beat(grid.bar_beat(bar + 1) - 0.25), bar);
        }
        // Before the first line and past the last, bars are counted on at what most bars hold.
        assert_eq!((grid.bar_beat(-1), grid.bar_beat(0)), (-2.0, 2.0));
        assert_eq!(grid.bar_length(7), 4.0);
        // No grid at all is counted at four beats to a bar, two beats a second.
        let none = Grid { beats_per_bar: 4, ..Grid::default() };
        assert_eq!((none.bar_start(2), none.place(4.5).0), (4.0, 2));
    }

    #[test]
    fn a_grid_the_tracker_got_wrong_is_reread() {
        let beats: Vec<f64> = (0..17).map(|index| index as f64 * 0.5).collect();
        let grid = Grid::build(&beats, &[0.0, 2.0, 4.0], 8.2, None, Tracker::Onsets);
        assert_eq!(grid.bars, [0, 4, 8, 12, 16]);

        let double = grid.corrected(Correction::Double);
        assert_eq!(double.bpm, 240.0);
        assert_eq!(double.beats.len(), 33);
        assert_eq!(double.bars[..3], [0, 4, 8]);

        let half = grid.corrected(Correction::Half);
        assert_eq!(half.bpm, 60.0);
        assert_eq!(half.beats, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        assert_eq!(half.bars, [0, 4, 8]);

        let later = grid.corrected(Correction::Rotate);
        assert_eq!(later.bars[..3], [1, 5, 9]);
        // Four times round is where it began.
        let round = (0..4).fold(grid.clone(), |grid, _| grid.corrected(Correction::Rotate));
        assert_eq!(round.bars, grid.bars);

        let waltz = grid.corrected(Correction::BeatsPerBar(3));
        assert_eq!((waltz.beats_per_bar, &waltz.bars[..3]), (3, &[0, 3, 6][..]));

        assert_eq!(Correction::parse("Half"), Some(Correction::Half));
        assert_eq!(Correction::parse("bar6"), Some(Correction::BeatsPerBar(6)));
        assert_eq!(Correction::parse("bar1"), None);
        assert_eq!(Correction::parse("faster"), None);
    }

    #[test]
    fn seconds_are_counted_in_beats_along_a_tempo_that_wanders() {
        let grid =
            Grid { beats: vec![1.0, 1.5, 2.1, 2.6], bars: vec![0], bpm: 115.0, beats_per_bar: 4, ..Grid::default() };
        assert_eq!(grid.beat_at(1.0), 0.0);
        assert!((grid.beat_at(1.8) - 1.5).abs() < 1e-12);
        // Before the first beat and after the last, at the nearest tempo.
        assert!((grid.beat_at(0.5) + 1.0).abs() < 1e-12);
        assert!((grid.beat_at(3.1) - 4.0).abs() < 1e-12);
        for beat in [-1.0, 0.0, 0.25, 1.5, 2.9, 4.0] {
            assert!((grid.beat_at(grid.seconds_at(beat)) - beat).abs() < 1e-9);
        }
        assert_eq!(Grid::default().beat_at(3.0), 6.0);
    }

    #[test]
    fn silence_and_a_single_hit_have_no_beats() {
        assert_eq!(track(&silence(3.0), &silence(3.0)), (Vec::new(), Vec::new()));
        let mut one = silence(1.0);
        drum(&mut one, 0.1, "kick", 0.9, &mut Noise(5));
        let (beats, _) = track(&one, &one);
        assert!(Grid::build(&beats, &[], 1.0, None, Tracker::Onsets).beats.is_empty());
    }
}
