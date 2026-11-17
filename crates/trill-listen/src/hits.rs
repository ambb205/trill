//! Cutting single hits and notes out of a recording.
//!
//! Every hit in a drum part is found, cut from the foot of its attack to
//! where it dies away, and told apart by where its energy sits. Of the many
//! kicks in a song only the cleanest few differing ones are kept. Bass
//! notes get the same treatment, named by pitch. And the drum part is read
//! as notes: the groove.

use crate::harmony::note_name;
use crate::onsets::{onsets, refine};
use crate::pitch::{self, PitchTrack};
use crate::signal::{RATE, db, magnitude, percentile_of, smooth};
use serde::{Deserialize, Serialize};
use trill_dsp::fft::RealFft;

/// What a single hit is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Kick,
    Snare,
    Hat,
    OpenHat,
    Cymbal,
    Tom,
    Perc,
    /// One note of a bass.
    Bass,
}

impl HitKind {
    /// The drums, in the order they are listed.
    pub const DRUMS: [HitKind; 7] =
        [HitKind::Kick, HitKind::Snare, HitKind::Hat, HitKind::OpenHat, HitKind::Cymbal, HitKind::Tom, HitKind::Perc];

    /// `kick`, `openhat`: what ids are made of.
    pub fn key(self) -> &'static str {
        match self {
            HitKind::Kick => "kick",
            HitKind::Snare => "snare",
            HitKind::Hat => "hat",
            HitKind::OpenHat => "openhat",
            HitKind::Cymbal => "cymbal",
            HitKind::Tom => "tom",
            HitKind::Perc => "perc",
            HitKind::Bass => "bass",
        }
    }

    /// What a person calls it.
    pub fn label(self) -> &'static str {
        match self {
            HitKind::Kick => "Kick",
            HitKind::Snare => "Snare",
            HitKind::Hat => "Hat",
            HitKind::OpenHat => "Open hat",
            HitKind::Cymbal => "Cymbal",
            HitKind::Tom => "Tom",
            HitKind::Perc => "Perc",
            HitKind::Bass => "Bass note",
        }
    }

    /// The General MIDI drum note it is written as.
    pub fn drum_note(self) -> u8 {
        match self {
            HitKind::Kick => 36,
            HitKind::Snare => 38,
            HitKind::Hat => 42,
            HitKind::OpenHat => 46,
            HitKind::Cymbal => 49,
            HitKind::Tom => 45,
            HitKind::Perc | HitKind::Bass => 39,
        }
    }

    /// How many of a kind are kept.
    fn keep(self) -> usize {
        match self {
            HitKind::Kick | HitKind::Snare | HitKind::Hat | HitKind::Perc => 8,
            _ => 4,
        }
    }
}

/// One hit of a drum part, measured.
#[derive(Clone, Debug, PartialEq)]
pub struct DrumHit {
    pub kind: HitKind,
    /// Where it starts and ends, in seconds.
    pub start: f64,
    pub end: f64,
    pub peak: f64,
    /// How far down the sound is where it ends, in dB under its peak.
    pub tail_db: f64,
    /// How far down whatever sounded just before it is, likewise.
    pub pre_db: f64,
    /// Whether it dies away before the next hit cuts it off.
    pub complete: bool,
    /// A description for telling two hits of a kind apart: unit length.
    pub shape: Vec<f32>,
}

/// A single hit or note worth keeping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OneShot {
    /// `kick-2`, `bass-41`.
    pub id: String,
    pub kind: HitKind,
    /// "Kick 2", "Bass F2".
    pub label: String,
    /// Where it starts and ends, in seconds.
    pub start: f64,
    pub end: f64,
    /// How clean it is, 0 to 1.
    pub score: f64,
    /// For a pitched one: the MIDI note it plays, and how far from it, in cents.
    pub note: Option<u8>,
    pub cents: Option<f64>,
}

/// One note of a drum part.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GrooveNote {
    /// When it is struck, in seconds.
    pub time: f64,
    pub kind: HitKind,
    /// The General MIDI drum note.
    pub note: u8,
    pub velocity: u8,
}

/// The loudest of `values`.
fn top(values: &[f32]) -> f32 {
    values.iter().copied().fold(0.0f32, f32::max)
}

/// Every hit in a drum part (mono at [`RATE`]), classified and measured,
/// in time order.
pub fn drum_hits(drums: &[f32]) -> Vec<DrumHit> {
    let starts = onsets(drums, 0.06);
    if starts.is_empty() {
        return Vec::new();
    }
    let level = smooth(&magnitude(drums), (RATE / 500.0) as usize);
    let overall = percentile_of(&level, 99.9);
    let mut hits = Vec::new();
    for (index, &onset) in starts.iter().enumerate() {
        let following = starts.get(index + 1).copied().unwrap_or(drums.len());
        let start = onset.saturating_sub((0.0015 * RATE) as usize);
        let limit =
            drums.len().min(start + (1.5 * RATE) as usize).min(following.saturating_sub((0.002 * RATE) as usize));
        if limit < start + (0.045 * RATE) as usize {
            // Another hit lands on top of this one: not usable alone.
            continue;
        }
        let window = &level[start..limit];
        let peak = f64::from(top(window));
        if peak < 0.02 * overall || peak < 1e-4 {
            continue;
        }
        let after_peak =
            (0..window.len()).max_by(|&a, &b| window[a].total_cmp(&window[b]).then(b.cmp(&a))).unwrap_or(0);
        if after_peak > (0.04 * RATE) as usize {
            // It swells rather than strikes: not a clean hit.
            continue;
        }
        // The hit ends when it has fallen 46 dB, or at the next hit.
        let quiet = window[after_peak..].iter().position(|&value| f64::from(value) < peak * 0.005);
        let end = quiet.map_or(limit, |quiet| (start + after_peak + quiet + (0.004 * RATE) as usize).min(limit));
        // Ringing: how far down the sound is when the next hit cuts it off.
        let tail = &level[start.max(end.saturating_sub(64))..end];
        let tail_db = db(f64::from(tail.iter().sum::<f32>()) / tail.len().max(1) as f64 / peak);
        let before = &level[start.saturating_sub((0.012 * RATE) as usize)
            ..start.saturating_sub((0.001 * RATE) as usize).max(1).min(level.len())];
        let pre_db = if before.is_empty() {
            -60.0
        } else {
            db(f64::from(before.iter().sum::<f32>()) / before.len() as f64 / peak)
        };
        let (kind, shape) = classify(&drums[start..end]);
        hits.push(DrumHit {
            kind,
            start: start as f64 / RATE,
            end: end as f64 / RATE,
            peak,
            tail_db,
            pre_db,
            complete: quiet.is_some() || tail_db < -30.0,
            shape,
        });
    }
    hits
}

/// The power spectrum of `piece` under a window that falls to nothing at
/// both ends, in 8192 points.
fn spectrum(fft: &mut RealFft, piece: &[f32]) -> Vec<f32> {
    let size = fft.size();
    let mut frame = vec![0.0f32; size];
    let count = piece.len().min(size);
    for (index, slot) in frame.iter_mut().enumerate().take(count) {
        let shape =
            if count > 1 { 0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / (count - 1) as f64).cos() } else { 1.0 };
        *slot = piece[index] * shape as f32;
    }
    let (mut re, mut im) = (vec![0.0f32; size / 2 + 1], vec![0.0f32; size / 2 + 1]);
    fft.forward(&frame, &mut re, &mut im);
    re.iter().zip(&im).map(|(re, im)| re * re + im * im).collect()
}

/// What kind of drum a hit is, and a description for comparing hits.
///
/// Drums are played together: most kicks have a hat on top of them. So the
/// question is asked of the body of the sound, under 2.5 kHz, where a hat
/// has almost nothing: is that body low (a kick), or higher with noise
/// above it (a snare)? Only a hit with no body to speak of is a cymbal.
pub fn classify(hit: &[f32]) -> (HitKind, Vec<f32>) {
    const SIZE: usize = 8192;
    let mut fft = RealFft::new(SIZE);
    let head = &hit[..hit.len().min((0.12 * RATE) as usize)];
    let power = spectrum(&mut fft, head);
    let bin = |hertz: f64| ((hertz * SIZE as f64 / RATE).ceil() as usize).min(power.len());
    let total = f64::from(power.iter().sum::<f32>()) + 1e-20;
    let share = |low: f64, high: f64| -> f64 { f64::from(power[bin(low)..bin(high)].iter().sum::<f32>()) / total };
    let (deep, punch, middle, bright) =
        (share(20.0, 200.0), share(200.0, 500.0), share(500.0, 2500.0), share(2500.0, 22_050.0));
    let body = deep + punch + middle;

    // How long it takes to fall 20 dB from its peak.
    let envelope = smooth(&magnitude(hit), (RATE / 400.0) as usize);
    let peak_at = (0..envelope.len()).max_by(|&a, &b| envelope[a].total_cmp(&envelope[b]).then(b.cmp(&a))).unwrap_or(0);
    let fallen = envelope[peak_at..].iter().position(|&value| value < envelope[peak_at] * 0.1);
    let decay = fallen.unwrap_or(envelope.len() - peak_at) as f64 / RATE;

    let kind = if body < 0.25 {
        if decay > 0.45 {
            HitKind::Cymbal
        } else if decay > 0.12 {
            HitKind::OpenHat
        } else {
            HitKind::Hat
        }
    } else if deep / body > 0.6 {
        HitKind::Kick
    } else if bright > 0.12 && (punch + middle) / body >= 0.4 {
        HitKind::Snare
    } else if punch / body > 0.5 && decay > 0.1 {
        HitKind::Tom
    } else {
        HitKind::Perc
    };

    // For telling two hits of a kind apart: the spectrum in twelve broad
    // bands from 40 Hz to 16 kHz, early and late, and how long it rings.
    let edges: Vec<f64> = (0..=12).map(|index| 40.0 * 400f64.powf(index as f64 / 12.0)).collect();
    let cut = hit.len().min((0.03 * RATE) as usize);
    let late_end = hit.len().min((0.12 * RATE) as usize);
    let mut bands = Vec::with_capacity(24);
    for piece in [&hit[..cut], &hit[cut..late_end.max(cut)]] {
        if piece.len() < 64 {
            bands.extend([0.0f64; 12]);
            continue;
        }
        let power = spectrum(&mut fft, piece);
        bands.extend(edges.windows(2).map(|pair| f64::from(power[bin(pair[0])..bin(pair[1])].iter().sum::<f32>())));
    }
    let sum = bands.iter().sum::<f64>() + 1e-20;
    let mut shape: Vec<f64> = bands.iter().map(|band| (1e6 * band / sum).ln_1p()).collect();
    shape.push(decay.min(1.0) * 10.0);
    let length = shape.iter().map(|v| v * v).sum::<f64>().sqrt().max(1e-9);
    (kind, shape.iter().map(|v| (v / length) as f32).collect())
}

/// The hits worth keeping: for each kind, clean ones that sound different.
pub fn pick_drums(hits: &[DrumHit]) -> Vec<OneShot> {
    let mut chosen = Vec::new();
    for kind in HitKind::DRUMS {
        let group: Vec<&DrumHit> = hits.iter().filter(|hit| hit.kind == kind).collect();
        let Some(loudest) = group.iter().map(|hit| hit.peak).max_by(f64::total_cmp) else { continue };
        // Ghost notes and bleed are not worth a pad.
        let mut scored: Vec<(f64, &DrumHit)> = group
            .into_iter()
            .filter(|hit| hit.peak >= 0.25 * loudest)
            .map(|hit| {
                // Clean: nothing ringing on underneath, allowed to die
                // away, and struck firmly.
                let isolation = ((-hit.pre_db - 12.0) / 30.0).clamp(0.0, 1.0);
                let ending = if hit.complete { 1.0 } else { (-hit.tail_db / 30.0).clamp(0.0, 1.0) };
                let strength = (hit.peak / loudest).clamp(0.0, 1.0).sqrt();
                (((0.45 * isolation + 0.35 * ending + 0.20 * strength) * 1000.0).round() / 1000.0, hit)
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut kept: Vec<(f64, &DrumHit)> = Vec::new();
        // A first pass keeps only hits unlike those already kept; a second,
        // looser pass fills any places left.
        for threshold in [0.975, 0.995] {
            for &(score, hit) in &scored {
                if kept.len() >= kind.keep() {
                    break;
                }
                if score < 0.35 || kept.iter().any(|(_, other)| std::ptr::eq(*other, hit)) {
                    continue;
                }
                let alike = |other: &DrumHit| -> f64 {
                    hit.shape.iter().zip(&other.shape).map(|(a, b)| f64::from(*a) * f64::from(*b)).sum()
                };
                if kept.iter().all(|(_, other)| alike(other) < threshold) {
                    kept.push((score, hit));
                }
            }
        }
        for (number, (score, hit)) in kept.into_iter().enumerate() {
            chosen.push(OneShot {
                id: format!("{}-{}", kind.key(), number + 1),
                kind,
                label: format!("{} {}", kind.label(), number + 1),
                start: (hit.start * 1e5).round() / 1e5,
                end: (hit.end * 1e5).round() / 1e5,
                score,
                note: None,
                cents: None,
            });
        }
    }
    chosen
}

/// One clean, sustained note for each pitch a bass (mono at [`RATE`])
/// plays, lowest first: at most `keep` of them. `track` is the bass's
/// pitch, if it has been followed already; `flats` says how notes are
/// spelled.
pub fn bass_notes(bass: &[f32], flats: bool, track: Option<&PitchTrack>, keep: usize) -> Vec<OneShot> {
    if top(&magnitude(bass)) < 1e-3 {
        return Vec::new();
    }
    let own;
    let track = match track {
        Some(track) => track,
        None => {
            own = pitch::track(bass, 30.0, 400.0);
            &own
        }
    };
    let notes: Vec<pitch::VoiceNote> =
        pitch::notes(track, 0.14).into_iter().filter(|note| (24..=67).contains(&note.midi)).collect();
    let Some(loudest) = notes.iter().map(|note| note.level).max_by(f64::total_cmp) else { return Vec::new() };
    let mut best: Vec<(f64, pitch::VoiceNote)> = Vec::new();
    for (index, note) in notes.iter().enumerate() {
        let length = note.end - note.start;
        let gap = if index > 0 { note.start - notes[index - 1].end } else { 1.0 };
        let score = 0.5 * length.min(0.8) / 0.8
            + 0.3 * ((note.level - (loudest - 20.0)) / 20.0).clamp(0.0, 1.0)
            + 0.2 * if gap > 0.03 { 1.0 } else { 0.4 };
        match best.iter_mut().find(|(_, other)| other.midi == note.midi) {
            Some(slot) if score > slot.0 => *slot = (score, *note),
            Some(_) => {}
            None => best.push((score, *note)),
        }
    }
    best.sort_by(|a, b| b.0.total_cmp(&a.0));
    best.truncate(keep);
    let mut out: Vec<OneShot> = best
        .into_iter()
        .map(|(score, note)| {
            // The pitch tracker's frames are coarse: start at the attack itself.
            let rough = (note.start.max(0.0) * RATE) as usize;
            let start = refine(bass, &[rough]).first().copied().unwrap_or(rough) as f64 / RATE;
            OneShot {
                id: format!("bass-{}", note.midi),
                kind: HitKind::Bass,
                label: format!("Bass {}", note_name(note.midi, flats)),
                start: ((start - 0.0015).max(0.0) * 1e5).round() / 1e5,
                end: ((note.end + 0.02).min(start + 3.0) * 1e5).round() / 1e5,
                score: (score * 1000.0).round() / 1000.0,
                note: Some(note.midi),
                cents: Some(note.cents),
            }
        })
        .collect();
    out.sort_by_key(|shot| shot.note);
    out
}

/// One stage of a filter: two poles and two zeros.
#[derive(Copy, Clone)]
struct Stage {
    b: [f64; 3],
    a: [f64; 2],
}

impl Stage {
    /// A low-pass or high-pass stage at `hertz` with resonance `q`.
    fn new(high: bool, hertz: f64, q: f64) -> Self {
        let turn = std::f64::consts::TAU * hertz / RATE;
        let (sin, cos) = turn.sin_cos();
        let alpha = sin / (2.0 * q);
        let scale = 1.0 / (1.0 + alpha);
        let b = if high {
            [(1.0 + cos) / 2.0 * scale, -(1.0 + cos) * scale, (1.0 + cos) / 2.0 * scale]
        } else {
            [(1.0 - cos) / 2.0 * scale, (1.0 - cos) * scale, (1.0 - cos) / 2.0 * scale]
        };
        Self { b, a: [-2.0 * cos * scale, (1.0 - alpha) * scale] }
    }

    fn run(&self, signal: &mut [f64]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for sample in signal {
            let x = *sample;
            let y = self.b[0] * x + self.b[1] * x1 + self.b[2] * x2 - self.a[0] * y1 - self.a[1] * y2;
            (x2, x1, y2, y1) = (x1, x, y1, y);
            *sample = y;
        }
    }
}

/// `signal` with everything below `low` and above `high` hertz taken out
/// (either may be left open), steeply and without moving anything in time:
/// a fourth-order filter with a flat pass band, run forwards and then
/// backwards.
pub fn band(signal: &[f32], low: Option<f64>, high: Option<f64>) -> Vec<f32> {
    // The two stages of a fourth-order filter with the flattest pass band.
    const Q: [f64; 2] = [0.541_196_100_146_197, 1.306_562_964_876_376_4];
    let mut stages = Vec::new();
    for (edge, is_high) in [(low, true), (high, false)] {
        if let Some(hertz) = edge {
            stages.extend(Q.iter().map(|&q| Stage::new(is_high, hertz, q)));
        }
    }
    let mut work: Vec<f64> = signal.iter().map(|&sample| f64::from(sample)).collect();
    for _ in 0..2 {
        for stage in &stages {
            stage.run(&mut work);
        }
        work.reverse();
    }
    work.iter().map(|&sample| sample as f32).collect()
}

/// The drum part (mono at [`RATE`]) as notes.
///
/// The kit is heard in three bands, each on its own: the bottom for the
/// kick, the middle for the snare, the top for the hats. Hits struck
/// together, which the waveform shows as one, come out as the separate
/// notes they were played as. Toms and cymbals are left out.
pub fn groove(drums: &[f32]) -> Vec<GrooveNote> {
    if top(&magnitude(drums)) < 1e-3 {
        return Vec::new();
    }
    let stream = |low: Option<f64>, high: Option<f64>| -> (Vec<f64>, Vec<f64>) {
        let band = band(drums, low, high);
        let starts = onsets(&band, 0.06);
        let level = smooth(&magnitude(&band), (RATE / 500.0) as usize);
        let reach = (0.03 * RATE) as usize;
        let peaks =
            starts.iter().map(|&start| f64::from(top(&level[start..(start + reach).min(level.len())]))).collect();
        (starts.iter().map(|&start| start as f64 / RATE).collect(), peaks)
    };
    let (kick_times, kick_peaks) = stream(None, Some(110.0));
    let (snare_times, snare_peaks) = stream(Some(250.0), Some(3000.0));
    let (hat_times, hat_peaks) = stream(Some(7000.0), None);

    let mut notes = Vec::new();
    // Writes the hits of one band that are loud enough for it to be that
    // drum and not the others leaking in, and returns their times.
    let mut add = |kind: HitKind, times: &[f64], peaks: &[f64], floor: f64, skip: Option<&[f64]>| -> Vec<f64> {
        if times.is_empty() {
            return Vec::new();
        }
        let usual = crate::signal::percentile(peaks, 90.0);
        let mut kept = Vec::new();
        for (&time, &peak) in times.iter().zip(peaks) {
            if peak < floor * usual {
                continue;
            }
            if skip.is_some_and(|skip| skip.iter().any(|other| (other - time).abs() < 0.03)) {
                continue;
            }
            kept.push(time);
            let velocity = (30.0 + 97.0 * (peak / usual).min(1.0).powf(0.6)).clamp(1.0, 127.0) as u8;
            notes.push(GrooveNote { time: (time * 1e5).round() / 1e5, kind, note: kind.drum_note(), velocity });
        }
        kept
    };
    let kicks = add(HitKind::Kick, &kick_times, &kick_peaks, 0.3, None);
    // A kick's beater is heard in the middle band too: that is not a snare.
    let snares = add(HitKind::Snare, &snare_times, &snare_peaks, 0.4, Some(&kicks));
    // A snare's wires reach the top band, where they look like a hat. If
    // the top band is busy between the snares, there is a hat part and it
    // most likely runs through them; if it only ever sounds with the snare,
    // it is the snare.
    let usual = crate::signal::percentile(&hat_peaks, 90.0);
    let (mut with_snare, mut without) = (0usize, 0usize);
    for (&time, &peak) in hat_times.iter().zip(&hat_peaks) {
        if peak >= 0.2 * usual {
            if snares.iter().any(|snare| (snare - time).abs() < 0.03) { with_snare += 1 } else { without += 1 }
        }
    }
    let only_the_snare = with_snare > without;
    add(HitKind::Hat, &hat_times, &hat_peaks, 0.2, only_the_snare.then_some(&snares[..]));
    notes.sort_by(|a, b| a.time.total_cmp(&b.time));
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::testing::{Noise, drum, hertz, silence, tone};

    /// Two bars at 120: kick on one and three, snare on two and four, hats
    /// on the off-beats in between.
    fn part() -> Vec<f32> {
        let mut signal = silence(4.4);
        let mut noise = Noise(31);
        for beat in 0..8 {
            let at = 0.2 + beat as f64 * 0.5;
            drum(&mut signal, at, if beat % 2 == 0 { "kick" } else { "snare" }, 0.8, &mut noise);
            drum(&mut signal, at + 0.25, "hat", 0.5, &mut noise);
        }
        signal
    }

    #[test]
    fn the_hits_of_a_drum_part_are_told_apart() {
        let hits = drum_hits(&part());
        assert_eq!(hits.len(), 16, "{:?}", hits.iter().map(|hit| (hit.kind, hit.start)).collect::<Vec<_>>());
        for (index, hit) in hits.iter().enumerate() {
            let expected = match index % 4 {
                0 => HitKind::Kick,
                2 => HitKind::Snare,
                _ => HitKind::Hat,
            };
            assert_eq!(hit.kind, expected, "hit {index} at {}", hit.start);
            assert!(hit.complete);
        }
        // A hit is cut from just before its attack to where it has died away.
        assert!((hits[0].start - 0.2).abs() < 0.004 && hits[0].end < 0.45, "{:?}", (hits[0].start, hits[0].end));
    }

    #[test]
    fn only_the_hits_that_differ_are_kept_and_they_are_numbered_by_kind() {
        let picked = pick_drums(&drum_hits(&part()));
        let of = |kind: HitKind| picked.iter().filter(|shot| shot.kind == kind).count();
        // The same kick eight times over is one kick worth having, or two
        // where the looser pass lets a near-twin in.
        assert!((1..=2).contains(&of(HitKind::Kick)), "{picked:?}");
        assert!((1..=2).contains(&of(HitKind::Snare)));
        assert!(of(HitKind::Hat) >= 1);
        assert_eq!(picked[0].id, "kick-1");
        assert_eq!(picked[0].label, "Kick 1");
        assert!(picked.iter().all(|shot| shot.score >= 0.35 && shot.note.is_none()));
        assert!(pick_drums(&[]).is_empty());
    }

    #[test]
    fn the_groove_is_written_as_kick_snare_and_hat_with_what_was_struck_together_apart() {
        let mut signal = part();
        // A hat on top of every kick, as most drummers play it.
        let mut noise = Noise(5);
        for beat in (0..8).step_by(2) {
            drum(&mut signal, 0.2 + beat as f64 * 0.5, "hat", 0.5, &mut noise);
        }
        let notes = groove(&signal);
        let times = |kind: HitKind| -> Vec<f64> {
            notes.iter().filter(|note| note.kind == kind).map(|note| note.time).collect()
        };
        let near = |found: &[f64], expected: &[f64]| {
            found.len() == expected.len() && found.iter().zip(expected).all(|(a, b)| (a - b).abs() < 0.012)
        };
        assert!(near(&times(HitKind::Kick), &[0.2, 1.2, 2.2, 3.2]), "kicks {:?}", times(HitKind::Kick));
        assert!(near(&times(HitKind::Snare), &[0.7, 1.7, 2.7, 3.7]), "snares {:?}", times(HitKind::Snare));
        let hats = times(HitKind::Hat);
        // The eight off-beat hats and the four on the kicks.
        assert!(hats.len() >= 12, "hats {hats:?}");
        assert!(notes.iter().all(|note| (1..=127).contains(&note.velocity)));
        assert_eq!(notes.iter().find(|note| note.kind == HitKind::Kick).map(|note| note.note), Some(36));
        assert!(groove(&silence(1.0)).is_empty());
    }

    #[test]
    fn a_bass_gives_one_note_for_each_pitch_named_and_cut_at_its_attack() {
        let mut signal = silence(4.0);
        for (index, note) in [29.0, 29.0, 33.0, 36.0, 33.0].into_iter().enumerate() {
            tone(&mut signal, 0.3 + index as f64 * 0.7, 0.3 + 0.08 * index as f64, hertz(note, 0.0), 0.6);
        }
        let shots = bass_notes(&signal, false, None, 12);
        assert_eq!(shots.iter().map(|shot| shot.note.unwrap()).collect::<Vec<_>>(), [29, 33, 36]);
        assert_eq!(shots[0].label, "Bass F1");
        assert_eq!(shots[0].id, "bass-29");
        // Of the two F's, the longer one was kept: the second.
        assert!((shots[0].start - 1.0).abs() < 0.01, "{}", shots[0].start);
        // And of the two A's the last, which is the longest of all.
        assert!((shots[1].start - 3.1).abs() < 0.01, "{}", shots[1].start);
        assert!(bass_notes(&silence(1.0), false, None, 12).is_empty());
    }

    #[test]
    fn a_band_keeps_what_is_inside_it_where_it_was() {
        let mut signal = silence(1.0);
        tone(&mut signal, 0.1, 0.8, 30.0, 0.5);
        let mut high = silence(1.0);
        tone(&mut high, 0.1, 0.8, 9000.0, 0.5);
        let both: Vec<f32> = signal.iter().zip(&high).map(|(a, b)| a + b).collect();
        let lows = band(&both, None, Some(110.0));
        let highs = band(&both, Some(7000.0), None);
        let level = |values: &[f32]| crate::signal::rms(&values[10_000..30_000]);
        // The low tone passes the low band nearly whole and the high one not at all.
        assert!((level(&lows) / level(&signal) - 1.0).abs() < 0.15, "{}", level(&lows) / level(&signal));
        assert!(level(&highs) / level(&high) > 0.8);
        let leak: Vec<f32> = lows.iter().zip(&signal).map(|(a, b)| a - b).collect();
        assert!(level(&leak) < 0.2 * level(&signal));
    }
}
