//! Beats mode: stretching in the time domain, for drums and anything else
//! whose character is in its transients.
//!
//! The output is assembled from *runs*: stretches of the input copied at
//! their original speed, each cross-faded into the next. Nothing is ever
//! resynthesized, so a drum hit that sits inside a run comes out sample for
//! sample as it went in. Three things make that work:
//!
//! * Every detected transient is *locked* by the time map: a run is forced to
//!   start just ahead of it at exactly the right moment, so hits stay sharp
//!   and on the grid, and are never played twice or skipped.
//! * Between transients, where the stretching actually happens, each new run
//!   starts wherever the waveform best continues the previous one (WSOLA,
//!   waveform-similarity overlap-add). A decaying kick is extended by
//!   repeating whole cycles of it, in phase.
//! * Repeating part of a decaying tail would make its level step up at every
//!   splice. Each run is therefore scaled so that the output follows the
//!   input's own envelope, stretched.

use super::onsets::Onset;
use super::timemap::{Anchor, Segment, TimeMap};

/// All lengths are in frames at the audio's sample rate.
struct Settings {
    /// Locked before and after each transient: enough to cover the attack's
    /// run-up, and the part of the hit the ear judges it by.
    pre: usize,
    post: usize,
    /// Locked at each end of the file.
    edge: usize,
    /// Cross-fade between runs, which is also the length over which
    /// candidate runs are compared.
    fade: usize,
    /// Cross-fade into a locked run. Short, because it has to fit in the
    /// few milliseconds before a transient that the transient masks.
    forced_fade: usize,
    /// How far ahead of a transient that cross-fade may be moved to find a
    /// quiet moment for it.
    forced_search: usize,
    /// How far from where the time map says it should be a run may start.
    /// Must cover a full cycle of the lowest pitch to be extended in phase:
    /// 12 ms reaches down to about 42 Hz.
    tolerance: usize,
    /// How far a run is allowed to drift from the time map before the next
    /// one takes over; sets how often there is a splice.
    jump: usize,
    min_spacing: usize,
    max_spacing: usize,
    /// The search for a matching run is done first on a copy of the audio
    /// reduced by this factor, then refined at full rate.
    decimation: usize,
    /// Half-width of the smoothing applied to the power envelope.
    envelope_radius: usize,
}

impl Settings {
    fn new(sample_rate: f64) -> Self {
        let frames = |seconds: f64| ((seconds * sample_rate).round() as usize).max(1);
        Self {
            pre: frames(0.003),
            post: frames(0.030),
            edge: frames(0.003),
            fade: frames(0.008),
            forced_fade: frames(0.003),
            forced_search: frames(0.010),
            tolerance: frames(0.012),
            jump: frames(0.010),
            min_spacing: frames(0.016),
            max_spacing: frames(0.060),
            decimation: ((sample_rate / 12_000.0).round() as usize).max(1),
            envelope_radius: frames(0.005),
        }
    }
}

/// How much a candidate run is marked down for starting a full tolerance
/// away from the ideal position. Among equally good matches (every cycle of
/// a steady tone) this picks the one that follows the time map most closely.
const DISTANCE_PENALTY: f32 = 0.1;
/// The envelope correction may change a run's level by at most 6 dB.
const MAX_ENVELOPE_GAIN: f32 = 2.0;

/// A stretch of the input copied to the output at its original speed.
struct Run {
    /// Output frame where the run starts fading in (and the previous run
    /// starts fading out).
    start: usize,
    /// Input frame minus output frame; `None` plays silence.
    delta: Option<i64>,
    fade: usize,
    /// How alike this run and the previous one are over the cross-fade,
    /// 0..=1, which decides the shape of the fade.
    similarity: f32,
}

pub(super) fn stretch(
    left: &[f32],
    right: &[f32],
    sample_rate: f64,
    out_len: usize,
    onsets: &[Onset],
) -> (Vec<f32>, Vec<f32>) {
    let settings = Settings::new(sample_rate);
    let anchors: Vec<Anchor> =
        onsets.iter().map(|onset| Anchor { input: onset.frame, pre: settings.pre, post: settings.post }).collect();
    let map = TimeMap::new(left.len(), out_len, &anchors, settings.edge);

    // Everything is decided once, on both channels together, and then
    // applied to each: the channels cannot drift apart.
    let both = [left, right];
    let channels = if left == right { &both[..1] } else { &both[..] };
    let source = Source::new(channels, &settings);
    let runs = plan(&source, &map, &settings);

    let mut outputs: Vec<Vec<f32>> = channels.iter().map(|_| vec![0.0; out_len]).collect();
    render(&source, &map, &runs, &mut outputs);
    let right_out = if outputs.len() > 1 { outputs.pop() } else { None };
    let left_out = outputs.swap_remove(0);
    let right_out = right_out.unwrap_or_else(|| left_out.clone());
    (left_out, right_out)
}

/// The input, with what the planner needs to know about it.
struct Source<'a> {
    channels: &'a [&'a [f32]],
    /// Low-rate copies of the channels for the coarse search.
    reduced: Vec<Vec<f32>>,
    decimation: usize,
    /// Smoothed power of all channels together, per frame.
    envelope: Vec<f32>,
    /// Added to envelope values before they are divided, so that the
    /// correction fades out towards the noise floor.
    envelope_floor: f32,
}

impl<'a> Source<'a> {
    fn new(channels: &'a [&'a [f32]], settings: &Settings) -> Self {
        let frames = channels[0].len();
        let decimation = settings.decimation;
        let reduced = channels
            .iter()
            .map(|channel| {
                channel.chunks(decimation).map(|chunk| chunk.iter().sum::<f32>() / decimation as f32).collect()
            })
            .collect::<Vec<Vec<f32>>>();

        let mut envelope: Vec<f32> =
            (0..frames).map(|i| channels.iter().map(|channel| channel[i] * channel[i]).sum()).collect();
        // Two box filters make a triangular one, which is smooth enough not
        // to follow the individual cycles of a bass note.
        let mut scratch = vec![0.0; frames];
        box_filter(&envelope, &mut scratch, settings.envelope_radius);
        box_filter(&scratch, &mut envelope, settings.envelope_radius);
        let loudest = envelope.iter().fold(0.0f32, |max, &value| max.max(value));
        Self { channels, reduced, decimation, envelope, envelope_floor: loudest * 1e-6 + 1e-20 }
    }

    fn frames(&self) -> usize {
        self.channels[0].len()
    }

    /// Finds the input frame in `lo..=hi` from which the audio best continues
    /// what follows `reference`, comparing `length` frames; `ideal` is where
    /// the time map would start the run. Returns the frame and how similar
    /// the two stretches are (0..=1).
    fn best_match(&self, reference: i64, lo: i64, hi: i64, ideal: f64, length: usize, tolerance: f64) -> (i64, f32) {
        let frames = self.frames() as i64;
        let (lo, hi) = (lo.max(0), hi.min(frames - 1));
        let length = (length as i64).min(frames - reference).min(frames - hi);
        if hi < lo || reference < 0 || length < 4 {
            return ((ideal.round() as i64).clamp(lo.min(hi), hi.max(lo)), 0.0);
        }
        let penalty = |position: i64| DISTANCE_PENALTY * ((position as f64 - ideal).abs() / tolerance.max(1.0)) as f32;

        // Pick the neighbourhood on a reduced-rate copy of the audio (over
        // candidates that share the reference's position within a reduced
        // sample), which is where the distance penalty applies...
        let step = self.decimation as i64;
        let mut position = (ideal.round() as i64).clamp(lo, hi);
        if hi - lo > 4 * step {
            let coarse_len = (length / step) as usize;
            let reference_index = (reference / step) as usize;
            let offset = reference % step;
            let first = (lo - offset + step - 1).div_euclid(step);
            let last = (hi - offset).div_euclid(step);
            let mut best = f32::NEG_INFINITY;
            for candidate in first..=last {
                let frame = candidate * step + offset;
                let score =
                    self.similarity(&self.reduced, reference_index, candidate as usize, coarse_len) - penalty(frame);
                if score > best {
                    (best, position) = (score, frame);
                }
            }
        } else {
            let mut best = f32::NEG_INFINITY;
            for candidate in lo..=hi {
                let score = self.similarity(self.channels, reference as usize, candidate as usize, length as usize)
                    - penalty(candidate);
                if score > best {
                    (best, position) = (score, candidate);
                }
            }
        }

        // ...then climb to the top of that peak at full rate, on similarity
        // alone. The penalty must not pull the result off the peak: an
        // offset of a fraction of a sample per splice, always in the same
        // direction, would detune a steady tone.
        let similarity =
            |candidate: i64| self.similarity(self.channels, reference as usize, candidate as usize, length as usize);
        let mut best = similarity(position);
        for _ in 0..2 * step {
            let (below, above) = (position - 1, position + 1);
            let down = if below >= lo { similarity(below) } else { f32::NEG_INFINITY };
            let up = if above <= hi { similarity(above) } else { f32::NEG_INFINITY };
            if down <= best && up <= best {
                break;
            }
            (best, position) = if up > down { (up, above) } else { (down, below) };
        }
        (position, best.clamp(0.0, 1.0))
    }

    /// Normalized cross-correlation of `length` frames starting at `a` and
    /// at `b`, over all channels. Frames past the end count as silence.
    fn similarity<T: AsRef<[f32]>>(&self, channels: &[T], a: usize, b: usize, length: usize) -> f32 {
        let (mut dot, mut energy_a, mut energy_b) = (0.0f32, 0.0f32, 0.0f32);
        for channel in channels {
            let channel = channel.as_ref();
            let length = length.min(channel.len().saturating_sub(a.max(b)));
            let (x, y) = (&channel[a.min(channel.len())..], &channel[b.min(channel.len())..]);
            for (x, y) in x[..length].iter().zip(&y[..length]) {
                dot += x * y;
                energy_a += x * x;
                energy_b += y * y;
            }
        }
        dot / (energy_a * energy_b).sqrt().max(1e-20)
    }

    /// Finds where in `lo..=hi` (output frames) to cross-fade from a run at
    /// `old_delta` to one at `new_delta`: the place where the two differ
    /// least over `length` frames. Returns the frame and the similarity there.
    fn quietest_splice(&self, old_delta: i64, new_delta: i64, lo: usize, hi: usize, length: usize) -> (usize, f32) {
        let difference = |frame: usize| -> f64 {
            let (a, b) = (frame as i64 + old_delta, frame as i64 + new_delta);
            self.channels.iter().map(|channel| (sample(channel, a) as f64 - sample(channel, b) as f64).powi(2)).sum()
        };
        let mut error: f64 = (lo..lo + length).map(difference).sum();
        let mut best = (error, lo);
        for frame in lo..hi {
            error += difference(frame + length) - difference(frame);
            if error < best.0 {
                best = (error, frame + 1);
            }
        }

        let at = best.1;
        let (mut dot, mut energy_a, mut energy_b) = (0.0f64, 0.0f64, 0.0f64);
        for channel in self.channels {
            for frame in at..at + length {
                let x = sample(channel, frame as i64 + old_delta) as f64;
                let y = sample(channel, frame as i64 + new_delta) as f64;
                dot += x * y;
                energy_a += x * x;
                energy_b += y * y;
            }
        }
        (at, (dot / (energy_a * energy_b).sqrt().max(1e-20)).clamp(0.0, 1.0) as f32)
    }

    /// The gain that makes a run reading input frame `actual` as loud as the
    /// input is at `wanted`, where the time map says the output should be.
    #[inline]
    fn envelope_gain(&self, wanted: f64, actual: i64) -> f32 {
        let last = self.envelope.len() - 1;
        let wanted = (wanted.round().max(0.0) as usize).min(last);
        let actual = (actual.max(0) as usize).min(last);
        if wanted == actual {
            return 1.0;
        }
        let ratio = (self.envelope[wanted] + self.envelope_floor) / (self.envelope[actual] + self.envelope_floor);
        ratio.sqrt().clamp(1.0 / MAX_ENVELOPE_GAIN, MAX_ENVELOPE_GAIN)
    }
}

#[inline]
fn sample(channel: &[f32], frame: i64) -> f32 {
    if frame >= 0 { channel.get(frame as usize).copied().unwrap_or(0.0) } else { 0.0 }
}

/// Moving average over `radius` frames either side, shrinking at the ends.
fn box_filter(input: &[f32], output: &mut [f32], radius: usize) {
    let mut sum = 0.0f64;
    let (mut lo, mut hi) = (0, 0);
    for (index, out) in output.iter_mut().enumerate() {
        let (want_lo, want_hi) = (index.saturating_sub(radius), (index + radius + 1).min(input.len()));
        while hi < want_hi {
            sum += input[hi] as f64;
            hi += 1;
        }
        while lo < want_lo {
            sum -= input[lo] as f64;
            lo += 1;
        }
        *out = (sum / (hi - lo) as f64).max(0.0) as f32;
    }
}

/// Decides where every run starts.
fn plan(source: &Source, map: &TimeMap, settings: &Settings) -> Vec<Run> {
    let mut runs = vec![Run { start: 0, delta: Some(0), fade: 0, similarity: 1.0 }];
    // The run now playing, and the output frame from which it plays alone.
    let mut delta = 0i64;
    let mut cursor = 0usize;

    for (index, segment) in map.segments.iter().enumerate() {
        if segment.locked {
            // The cross-fade into a locked segment is planned at the end of
            // the elastic one before it; this only catches the case of two
            // locked segments back to back.
            let locked = segment.in_start as i64 - segment.out_start as i64;
            if delta != locked {
                cursor = cursor.max(segment.out_start);
                runs.push(Run { start: cursor, delta: Some(locked), fade: 0, similarity: 1.0 });
                delta = locked;
            }
            continue;
        }
        let next_lock = map
            .segments
            .get(index + 1)
            .filter(|next| next.locked)
            .map(|next| next.in_start as i64 - next.out_start as i64);
        plan_elastic(source, segment, next_lock, settings, &mut runs, &mut delta, &mut cursor);
    }
    runs
}

/// Plans the runs of one elastic segment, ending on the run the following
/// locked segment needs (`next_lock`), if there is one.
fn plan_elastic(
    source: &Source,
    segment: &Segment,
    next_lock: Option<i64>,
    settings: &Settings,
    runs: &mut Vec<Run>,
    delta: &mut i64,
    cursor: &mut usize,
) {
    let in_len = segment.in_end - segment.in_start;
    // Short segments (fast rolls) get proportionally shorter fades and
    // searches, so that the constraints below can always be met.
    let fade = settings.fade.min(in_len / 4).max(1);
    let tolerance = settings.tolerance.min(in_len / 4) as f64;
    let forced_fade = settings.forced_fade.min(in_len / 8).max(1);
    let forced_search = settings.forced_search.min(in_len / 4);
    let reserve = if next_lock.is_some() { forced_fade + forced_search } else { 0 };
    let limit = segment.out_end.saturating_sub(reserve);

    // A run advances one input frame per output frame, the time map `slope`
    // of them: this is how fast a run pulls ahead of where it should be.
    let drift = 1.0 - segment.slope();
    if drift.abs() > 1e-9 {
        let spacing = ((settings.jump as f64 / drift.abs()) as usize)
            .clamp(settings.min_spacing, settings.max_spacing)
            .min(in_len / 2)
            .max(fade);
        // Start each run half a jump behind the map so it ends half a jump
        // ahead, rather than a whole jump off at one end.
        let lead = drift * spacing as f64 * 0.5;
        let mut at = (segment.out_start + spacing / 2).max(*cursor);
        while at + fade <= limit {
            let ideal = segment.input_at(at as f64) - lead;
            // A run must not reach back into the transient before this
            // segment (it would sound twice) and must be able to play until
            // the next splice without reaching the transient after it.
            let lo = ((ideal - tolerance).ceil() as i64).max(segment.in_start as i64);
            let hi = ((ideal + tolerance).floor() as i64).min(segment.in_end as i64 - (spacing + fade) as i64);
            if hi < lo {
                break;
            }
            let (position, similarity) = source.best_match(at as i64 + *delta, lo, hi, ideal, fade, tolerance);
            let chosen = position - at as i64;
            if chosen != *delta {
                runs.push(Run { start: at, delta: Some(chosen), fade, similarity });
                *delta = chosen;
                *cursor = at + fade;
            }
            at += spacing;
        }
    }

    let Some(target) = next_lock else { return };
    if target == *delta {
        return;
    }
    // The current run has to be gone before it reaches the next transient,
    // and the locked run must not start so early that it reaches back past
    // this segment's input.
    let ahead = (*delta - target).max(0) as usize;
    let latest = (segment.out_end as i64 - forced_fade as i64 - ahead as i64).max(0) as usize;
    let earliest = (*cursor).max(segment.out_end.saturating_sub(in_len));
    if latest >= earliest {
        let lo = earliest.max(latest.saturating_sub(forced_search));
        let (at, similarity) = source.quietest_splice(*delta, target, lo, latest, forced_fade);
        runs.push(Run { start: at, delta: Some(target), fade: forced_fade, similarity });
        *cursor = at + forced_fade;
    } else {
        // Too little input to fill the gap without repeating a transient
        // (hits packed closer than the stretch can fill): let the current
        // run end and leave a moment of silence rather than double a hit.
        let stop = latest.max(*cursor);
        runs.push(Run { start: stop, delta: None, fade: forced_fade, similarity: 1.0 });
        let resume = earliest.max(stop + forced_fade).min(segment.out_end);
        runs.push(Run {
            start: resume,
            delta: Some(target),
            fade: forced_fade.min(segment.out_end - resume),
            similarity: 1.0,
        });
        *cursor = resume + forced_fade.min(segment.out_end - resume);
    }
    *delta = target;
}

/// Copies the runs to the output, cross-fading between them.
fn render(source: &Source, map: &TimeMap, runs: &[Run], outputs: &mut [Vec<f32>]) {
    let out_len = outputs[0].len();
    for (index, run) in runs.iter().enumerate() {
        let next = runs.get(index + 1);
        let end = next.map_or(out_len, |next| next.start + next.fade).min(out_len);
        let Some(delta) = run.delta else { continue };

        let mut segment_index = map.segments.partition_point(|segment| segment.out_end <= run.start);
        for frame in run.start..end {
            while map.segments[segment_index].out_end <= frame {
                segment_index += 1;
            }
            let fade = if frame < run.start + run.fade {
                fade_gains(frame - run.start, run.fade, run.similarity).1
            } else {
                match next {
                    Some(next) if frame >= next.start => fade_gains(frame - next.start, next.fade, next.similarity).0,
                    _ => 1.0,
                }
            };
            let from = frame as i64 + delta;
            let gain = fade * source.envelope_gain(map.segments[segment_index].input_at(frame as f64), from);
            for (output, channel) in outputs.iter_mut().zip(source.channels) {
                output[frame] += sample(channel, from) * gain;
            }
        }
    }
}

/// Gains of the outgoing and incoming run at `position` of a `length`-frame
/// cross-fade. Two signals that are alike add as amplitudes and want a
/// linear fade; unrelated ones add as powers and want an equal-power fade.
/// Normalizing by the level the mix is expected to have covers both, and
/// everything in between, without a dip or a bump.
#[inline]
fn fade_gains(position: usize, length: usize, similarity: f32) -> (f32, f32) {
    let incoming = (position as f32 + 0.5) / length as f32;
    let outgoing = 1.0 - incoming;
    let level = (outgoing * outgoing + incoming * incoming + 2.0 * similarity * outgoing * incoming).sqrt();
    (outgoing / level, incoming / level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    const SR: f64 = 48_000.0;

    fn kick(frames: usize, at: usize, signal: &mut [f32]) {
        for i in 0..frames {
            let t = i as f32 / SR as f32;
            let hz = 50.0 + 120.0 * (-t * 40.0).exp();
            signal[at + i] += 0.8 * (std::f32::consts::TAU * hz * t).sin() * (-t * 9.0).exp();
        }
    }

    #[test]
    fn identical_lengths_copy_the_input() {
        let mut rng = Rng::new(11);
        let input: Vec<f32> = (0..30_000).map(|_| rng.bipolar() * 0.5).collect();
        let onsets = [Onset { frame: 7_000, strength: 1.0 }, Onset { frame: 19_000, strength: 1.0 }];
        let (left, right) = stretch(&input, &input, SR, input.len(), &onsets);
        assert!(left == input && right == input);
    }

    #[test]
    fn transients_are_copied_exactly_to_their_new_place() {
        let mut input = vec![0.0f32; 96_000];
        let hits = [0usize, 24_000, 48_000, 72_000];
        for &hit in &hits {
            kick(20_000, hit, &mut input);
        }
        let onsets: Vec<Onset> = hits.iter().map(|&frame| Onset { frame, strength: 1.0 }).collect();
        for ratio in [0.5, 0.77, 1.3, 2.0] {
            let out_len = (input.len() as f64 * ratio).round() as usize;
            let (output, _) = stretch(&input, &input, SR, out_len, &onsets);
            for &hit in &hits {
                let at = (hit as f64 * ratio).round() as usize;
                // The first 10 ms of every hit are the input's, bit for bit.
                assert_eq!(output[at..at + 480], input[hit..hit + 480], "ratio {ratio}, hit {hit}");
            }
        }
    }

    #[test]
    fn decaying_tail_stretches_smoothly() {
        // One kick, stretched to double length: the tail's envelope must keep
        // falling, without the level stepping back up at each splice.
        let mut input = vec![0.0f32; 48_000];
        kick(40_000, 0, &mut input);
        let (output, _) = stretch(&input, &input, SR, 96_000, &[Onset { frame: 0, strength: 1.0 }]);
        let level = |signal: &[f32], at: usize| {
            (signal[at..at + 1920].iter().map(|s| s * s).sum::<f32>() / 1920.0).sqrt().max(1e-9)
        };
        let mut last = f32::INFINITY;
        for step in 1..40 {
            let at = step * 2_000;
            let (got, want) = (level(&output, at), level(&input, at / 2));
            assert!((20.0 * (got / want).log10()).abs() < 1.5, "at {at}: {got} vs {want}");
            assert!(got < last * 1.02, "level rose at {at}");
            last = got;
        }
    }

    #[test]
    fn dense_hits_are_never_doubled() {
        // Clicks 20 ms apart, stretched far beyond what the gaps can fill.
        let mut input = vec![0.0f32; 48_000];
        let hits: Vec<usize> = (1..40).map(|i| i * 960 + 100).collect();
        for &hit in &hits {
            input[hit] = 0.9;
        }
        let onsets: Vec<Onset> = hits.iter().map(|&frame| Onset { frame, strength: 1.0 }).collect();
        for ratio in [0.3, 0.5, 2.0, 4.0] {
            let out_len = (input.len() as f64 * ratio).round() as usize;
            let (output, _) = stretch(&input, &input, SR, out_len, &onsets);
            let clicks = output.iter().filter(|s| s.abs() > 0.2).count();
            assert_eq!(clicks, hits.len(), "ratio {ratio}");
        }
    }
}
