//! Beat This: a network that hears where the beats and the downbeats are.
//!
//! It is fed what a recording's spectrum looks like fifty times a second,
//! in 128 bands spaced as the ear spaces them, thirty seconds at a time,
//! and answers for every frame how likely a beat and a downbeat are
//! there. A beat is a peak of that likelihood; a downbeat is moved onto
//! the beat nearest it.

use super::tensor;
use crate::models::Progress;
use ort::session::Session;
use trill_dsp::fft::{RealFft, hann};
use trill_listen::signal::resample;
use trill_listen::{RATE, TrackedBeats};

/// The rate the model listens at, and its frames a second.
const MODEL_RATE: f64 = 22_050.0;
const HOP: usize = 441;
const SIZE: usize = 1024;
const BANDS: usize = 128;
const FRAMES_A_SECOND: f64 = 50.0;
/// The frames the model takes at once, and how many at each end of them
/// are only there for the ones between.
const CHUNK: usize = 1500;
const BORDER: usize = 6;

fn to_mel(hertz: f64) -> f64 {
    const STEP: f64 = 200.0 / 3.0;
    if hertz < 1000.0 { hertz / STEP } else { 1000.0 / STEP + (hertz / 1000.0).ln() / (6.4f64.ln() / 27.0) }
}

fn from_mel(mel: f64) -> f64 {
    const STEP: f64 = 200.0 / 3.0;
    let knee = 1000.0 / STEP;
    if mel < knee { mel * STEP } else { 1000.0 * ((mel - knee) * (6.4f64.ln() / 27.0)).exp() }
}

/// The bands the model was trained on: triangles from 30 Hz to 11 kHz,
/// evenly spaced on the mel scale, each reaching one at its peak. For each
/// band, the first bin it draws on and its weights from there.
fn bands() -> Vec<(usize, Vec<f32>)> {
    let (bottom, top) = (to_mel(30.0), to_mel(11_000.0));
    let edges: Vec<f64> =
        (0..BANDS + 2).map(|index| from_mel(bottom + (top - bottom) * index as f64 / (BANDS + 1) as f64)).collect();
    let bins = SIZE / 2 + 1;
    (0..BANDS)
        .map(|band| {
            let (left, centre, right) = (edges[band], edges[band + 1], edges[band + 2]);
            let mut first = None;
            let mut weights = Vec::new();
            for bin in 0..bins {
                // The bins are spread evenly from nothing to half the rate, as whole hertz have it.
                let hertz = bin as f64 * (MODEL_RATE / 2.0).floor() / (bins - 1) as f64;
                let weight = ((hertz - left) / (centre - left)).min((right - hertz) / (right - centre)).max(0.0);
                if weight > 0.0 {
                    first.get_or_insert(bin);
                    weights.push(weight as f32);
                } else if first.is_some() {
                    break;
                }
            }
            (first.unwrap_or(0), weights)
        })
        .collect()
}

/// What the model is fed: for each frame, fifty a second, the level of
/// each band, compressed.
pub fn spectrogram(mono: &[f32]) -> Vec<f32> {
    let signal = resample(mono, RATE, MODEL_RATE);
    let frames = 1 + signal.len() / HOP;
    let bands = bands();
    let mut fft = RealFft::new(SIZE);
    let window = hann(SIZE);
    let (mut frame, mut re, mut im) = (vec![0.0f32; SIZE], vec![0.0f32; SIZE / 2 + 1], vec![0.0f32; SIZE / 2 + 1]);
    let scale = 1.0 / (SIZE as f32).sqrt();
    let mut out = vec![0.0f32; frames * BANDS];
    if signal.len() < 2 {
        return out;
    }
    let last = signal.len() as isize - 1;
    for index in 0..frames {
        let centre = (index * HOP) as isize;
        for (offset, slot) in frame.iter_mut().enumerate() {
            // What lies beyond either end is the signal folded back.
            let mut at = centre - (SIZE / 2) as isize + offset as isize;
            while at < 0 || at > last {
                at = if at < 0 { -at } else { 2 * last - at };
            }
            *slot = signal[at as usize] * window[offset];
        }
        fft.forward(&frame, &mut re, &mut im);
        let row = &mut out[index * BANDS..(index + 1) * BANDS];
        for ((first, weights), slot) in bands.iter().zip(row) {
            let level: f32 = weights
                .iter()
                .enumerate()
                .map(|(offset, weight)| weight * (re[first + offset].powi(2) + im[first + offset].powi(2)).sqrt())
                .sum();
            *slot = (1000.0 * level * scale).ln_1p();
        }
    }
    out
}

/// The peaks of a likelihood that is given as logits: above nothing, and
/// the largest within three frames either side. Neighbours are one peak,
/// at their middle. In seconds.
fn peaks(logits: &[f32]) -> Vec<f64> {
    let mut times = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let mut close = |run: &mut Vec<usize>| {
        if !run.is_empty() {
            times.push(run.iter().sum::<usize>() as f64 / run.len() as f64 / FRAMES_A_SECOND);
            run.clear();
        }
    };
    for (index, &value) in logits.iter().enumerate() {
        let (low, high) = (index.saturating_sub(3), (index + 4).min(logits.len()));
        if value > 0.0 && logits[low..high].iter().all(|&other| other <= value) {
            if run.last().is_some_and(|&last| index - last > 1) {
                close(&mut run);
            }
            run.push(index);
        }
    }
    close(&mut run);
    times
}

/// The beats and downbeats of a recording heard as one channel.
pub fn track(session: &mut Session, mono: &[f32], progress: Progress) -> Result<TrackedBeats, String> {
    let mel = spectrogram(mono);
    let frames = mel.len() / BANDS;
    let step = CHUNK - 2 * BORDER;
    let mut starts: Vec<isize> = (0..)
        .map(|index| (index * step) as isize - BORDER as isize)
        .take_while(|&start| start < frames as isize - BORDER as isize)
        .collect();
    if starts.is_empty() {
        return Ok(TrackedBeats::default());
    }
    // The last chunk ends where the recording does, so that it is a whole one.
    if frames > step {
        *starts.last_mut().expect("not empty") = frames as isize - (CHUNK - BORDER) as isize;
    }
    let (mut beat, mut downbeat) = (vec![-1000.0f32; frames], vec![-1000.0f32; frames]);
    // Later chunks first, so that where two overlap the earlier one stays.
    for (done, &start) in starts.iter().rev().enumerate() {
        let mut chunk = vec![0.0f32; CHUNK * BANDS];
        for row in 0..CHUNK {
            if let Some(frame) = usize::try_from(start + row as isize).ok().filter(|&frame| frame < frames) {
                chunk[row * BANDS..(row + 1) * BANDS].copy_from_slice(&mel[frame * BANDS..(frame + 1) * BANDS]);
            }
        }
        let outputs = session
            .run(ort::inputs!["spectrogram" => tensor(&[1, CHUNK, BANDS], chunk, CHUNK * BANDS)?])
            .map_err(|error| format!("the model that finds the beat could not be run: {error}"))?;
        let (_, beats) = outputs["beat"].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        let (_, downbeats) = outputs["downbeat"].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        if beats.len() != CHUNK || downbeats.len() != CHUNK {
            return Err("the model that finds the beat answered in a shape that is not its own: is its file the one the setup fetched?".to_owned());
        }
        for row in BORDER..CHUNK - BORDER {
            if let Some(frame) = usize::try_from(start + row as isize).ok().filter(|&frame| frame < frames) {
                (beat[frame], downbeat[frame]) = (beats[row], downbeats[row]);
            }
        }
        progress((done + 1) as f32 / starts.len() as f32);
    }
    let beats = peaks(&beat);
    let mut downbeats: Vec<f64> = peaks(&downbeat)
        .into_iter()
        .filter_map(|time| beats.iter().copied().min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs())))
        .collect();
    downbeats.dedup();
    Ok(TrackedBeats { beats, downbeats })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_likelihood_is_read_at_its_peaks() {
        let mut logits = vec![-5.0f32; 200];
        logits[50] = 2.0;
        // Two frames as high as one another are one beat, between them.
        logits[100] = 1.0;
        logits[101] = 1.0;
        // Under nothing is no beat, and a lesser peak beside a greater is none.
        logits[150] = -0.5;
        logits[52] = 1.0;
        assert_eq!(peaks(&logits), [1.0, 2.01]);
        assert!(peaks(&[]).is_empty());
    }

    #[test]
    fn a_tone_lies_in_the_band_that_holds_it_fifty_times_a_second() {
        let tone: Vec<f32> =
            (0..44_100).map(|i| (i as f64 * std::f64::consts::TAU * 440.0 / 44_100.0).sin() as f32 * 0.5).collect();
        let mel = spectrogram(&tone);
        assert_eq!(mel.len() / BANDS, 51);
        let row = &mel[25 * BANDS..26 * BANDS];
        let loudest = (0..BANDS).max_by(|&a, &b| row[a].total_cmp(&row[b])).unwrap();
        let bands = bands();
        let centre = (bands[loudest].0 as f64 + bands[loudest].1.len() as f64 / 2.0) * 11_025.0 / 512.0;
        assert!((centre - 440.0).abs() < 40.0, "the loudest band is around {centre} Hz");
        assert_eq!(bands.len(), BANDS);
        assert!(bands[0].0 >= 1 && bands[BANDS - 1].0 + bands[BANDS - 1].1.len() <= 513);
    }
}
