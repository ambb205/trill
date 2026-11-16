//! Time-stretches an audio file and measures what that did to it.
//!
//! ```text
//! cargo run -p trill-dsp --release --example stretch -- <in.wav> <out.wav> <ratio> [beats|tonal|repitch] [semitones]
//! ```
//!
//! `ratio` is output length over input length: `0.7667` fits a 92 BPM loop
//! into a 120 BPM song. The measurements are what the tests check on
//! synthetic signals, taken on real material: where the hits landed, how
//! sharp they stayed, and how the spectrum and level changed.

use std::path::Path;
use std::time::Instant;
use trill_core::audio::AudioBuffer;
use trill_dsp::fft::{RealFft, hann};
use trill_dsp::stretch::{Onset, StretchMode, StretchOptions, detect_onsets, stretch};
use trill_media::{WavFormat, decode, write_wav};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: stretch <in.wav> <out.wav> <ratio> [beats|tonal|repitch] [semitones]");
        std::process::exit(2);
    }
    let mode = match args.get(3).map(String::as_str) {
        Some("beats") => StretchMode::Beats,
        Some("repitch") => StretchMode::Repitch,
        _ => StretchMode::Tonal,
    };
    let options = StretchOptions {
        time_ratio: args[2].parse().expect("ratio must be a number"),
        pitch_semitones: args.get(4).map_or(0.0, |s| s.parse().expect("semitones must be a number")),
        mode,
    };

    let input = decode(Path::new(&args[0])).unwrap_or_else(|error| panic!("{error}"));
    // Best of three, so a busy machine does not pass for a slow algorithm.
    let mut elapsed = f64::MAX;
    let mut output = AudioBuffer::default();
    for _ in 0..3 {
        let start = Instant::now();
        output = stretch(&input, &options);
        elapsed = elapsed.min(start.elapsed().as_secs_f64());
    }
    write_wav(Path::new(&args[1]), &output, WavFormat::Int24).unwrap_or_else(|error| panic!("{error}"));

    let ratio = output.frames() as f64 / input.frames().max(1) as f64;
    println!("input    {:.3} s at {} Hz, {} frames", input.seconds(), input.sample_rate, input.frames());
    println!("output   {:.3} s, {} frames (ratio {ratio:.6}), {mode:?}", output.seconds(), output.frames());
    println!(
        "speed    {:.3} s: {:.0}x real time by input, {:.0}x by output",
        elapsed,
        input.seconds() / elapsed,
        output.seconds() / elapsed
    );
    println!(
        "level    rms {:+.2} dB, peak {:+.2} dB",
        db(rms(&output) / rms(&input)),
        db(peak(&output) / peak(&input))
    );
    println!(
        "stereo   correlation {:.4} -> {:.4}, side/mid {:+.2} dB -> {:+.2} dB",
        correlation(&input),
        correlation(&output),
        side_over_mid_db(&input),
        side_over_mid_db(&output)
    );
    if options.pitch_semitones == 0.0 && mode != StretchMode::Repitch {
        report_onsets(&input, &output, ratio);
        report_spectrum(&input, &output);
    }
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.max(1e-12).log10()
}

fn rms(buffer: &AudioBuffer) -> f64 {
    let sum: f64 = buffer.left.iter().chain(&buffer.right).map(|s| (*s as f64).powi(2)).sum();
    (sum / (buffer.frames().max(1) * 2) as f64).sqrt()
}

fn peak(buffer: &AudioBuffer) -> f64 {
    buffer.left.iter().chain(&buffer.right).fold(0.0f64, |max, s| max.max(s.abs() as f64))
}

fn correlation(buffer: &AudioBuffer) -> f64 {
    let (mut dot, mut left, mut right) = (0.0, 0.0, 0.0);
    for (l, r) in buffer.left.iter().zip(&buffer.right) {
        dot += *l as f64 * *r as f64;
        left += (*l as f64).powi(2);
        right += (*r as f64).powi(2);
    }
    dot / (left * right).sqrt().max(1e-30)
}

fn side_over_mid_db(buffer: &AudioBuffer) -> f64 {
    let (mut mid, mut side) = (0.0, 0.0);
    for (l, r) in buffer.left.iter().zip(&buffer.right) {
        mid += ((l + r) as f64).powi(2);
        side += ((l - r) as f64).powi(2);
    }
    10.0 * (side.max(1e-30) / mid.max(1e-30)).log10()
}

/// Power of both channels, smoothed over a millisecond: the shape of a hit
/// without the waveform under it, which a stretch is allowed to change.
fn envelope(buffer: &AudioBuffer) -> Vec<f64> {
    let radius = (buffer.sample_rate * 0.0005) as usize;
    let power: Vec<f64> =
        buffer.left.iter().zip(&buffer.right).map(|(l, r)| (*l as f64).powi(2) + (*r as f64).powi(2)).collect();
    let mut sums = vec![0.0; power.len() + 1];
    for (index, value) in power.iter().enumerate() {
        sums[index + 1] = sums[index] + value;
    }
    (0..power.len())
        .map(|index| {
            let (low, high) = (index.saturating_sub(radius), (index + radius + 1).min(power.len()));
            (sums[high] - sums[low]) / (high - low) as f64
        })
        .collect()
}

/// Finds where each pronounced hit of the input ended up, by sliding the
/// shape of its attack (5 ms before to 15 ms after) along the output around
/// where it should be, and compares the hit there with the original.
fn report_onsets(input: &AudioBuffer, output: &AudioBuffer, ratio: f64) {
    const HIT: f32 = 0.6;
    let rate = input.sample_rate;
    let onsets = detect_onsets(input);
    let hits: Vec<&Onset> = onsets.iter().filter(|onset| onset.strength >= HIT).collect();
    let after = detect_onsets(output);
    let hits_after = after.iter().filter(|onset| onset.strength >= HIT).count();
    let (before, length) = ((0.005 * rate) as usize, (0.020 * rate) as usize);
    let search = (0.020 * rate) as i64;
    let (shape_in, shape_out) = (envelope(input), envelope(output));

    let mut errors_ms = Vec::new();
    let mut likeness = Vec::new();
    let mut attack_db = Vec::new();
    for onset in &hits {
        let Some(start) = onset.frame.checked_sub(before).filter(|start| start + length <= input.frames()) else {
            continue;
        };
        let template = &shape_in[start..start + length];
        let energy: f64 = template.iter().map(|value| value * value).sum();
        let want = (onset.frame as f64 * ratio).round() as i64 - before as i64;
        // Normalized correlation of the two shapes at every lag within 20 ms.
        let mut best = (f64::MIN, 0i64);
        for lag in -search..=search {
            let at = want + lag;
            if at < 0 || at as usize + length > shape_out.len() {
                continue;
            }
            let candidate = &shape_out[at as usize..at as usize + length];
            let dot: f64 = template.iter().zip(candidate).map(|(a, b)| a * b).sum();
            let other: f64 = candidate.iter().map(|value| value * value).sum();
            let score = dot / (energy * other).sqrt().max(1e-30);
            // Ties (a hit copied exactly scores 1 only at its own lag, but
            // rounding can equal it nearby) go to the smallest lag.
            if score > best.0 + 1e-12 || (score > best.0 - 1e-12 && lag.abs() < best.1.abs()) {
                best = (score.max(best.0), lag);
            }
        }
        errors_ms.push(best.1.abs() as f64 / rate * 1000.0);
        likeness.push(best.0);
        // Level of the first 5 ms of the hit, at the place it should be.
        let at = ((want + before as i64).max(0) as usize).min(shape_out.len().saturating_sub(before + 1));
        let level = |shape: &[f64], at: usize| shape[at..at + before].iter().sum::<f64>().max(1e-30);
        attack_db.push(
            10.0 * (level(&shape_out, at) / level(&shape_in, onset.frame.min(shape_in.len() - before - 1))).log10(),
        );
    }
    errors_ms.sort_by(f64::total_cmp);
    likeness.sort_by(f64::total_cmp);
    let mean = errors_ms.iter().sum::<f64>() / errors_ms.len().max(1) as f64;
    let at =
        |values: &[f64], p: f64| values.get(((values.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0);
    let attack_rms = (attack_db.iter().map(|d| d * d).sum::<f64>() / attack_db.len().max(1) as f64).sqrt();
    let attack_worst = attack_db.iter().fold(0.0f64, |worst, d| if d.abs() > worst.abs() { *d } else { worst });
    println!(
        "onsets   {} hits detected in the input, {} in the output ({} weaker onsets in, {} out)",
        hits.len(),
        hits_after,
        onsets.len() - hits.len(),
        after.len() - hits_after
    );
    println!(
        "timing   each hit's attack located in the output: mean error {mean:.3} ms, median {:.3} ms, 95% {:.3} ms, worst {:.3} ms",
        at(&errors_ms, 0.5),
        at(&errors_ms, 0.95),
        at(&errors_ms, 1.0)
    );
    println!(
        "attacks  shape likeness (1 = identical): median {:.4}, worst {:.4}; first 5 ms level: rms change {attack_rms:.2} dB, worst {attack_worst:+.2} dB",
        at(&likeness, 0.5),
        at(&likeness, 0.0)
    );
}

/// Long-term spectra in third-octave bands, before and after.
fn report_spectrum(input: &AudioBuffer, output: &AudioBuffer) {
    let (before, after) = (band_levels(input), band_levels(output));
    let differences: Vec<f64> = before
        .iter()
        .zip(&after)
        // Bands with nothing in them say nothing about the stretch.
        .filter(|(before, _)| before.1 > -70.0)
        .map(|(before, after)| after.1 - before.1)
        .collect();
    let rms = (differences.iter().map(|d| d * d).sum::<f64>() / differences.len().max(1) as f64).sqrt();
    let worst = differences.iter().fold(0.0f64, |worst, d| if d.abs() > worst.abs() { *d } else { worst });
    println!("spectrum third-octave bands: rms difference {rms:.2} dB, worst {worst:+.2} dB");
    let line: Vec<String> = before
        .iter()
        .zip(&after)
        .filter(|(before, _)| before.1 > -70.0)
        .map(|(before, after)| format!("{:.0}Hz {:+.1}", before.0, after.1 - before.1))
        .collect();
    println!("         {}", line.join("  "));
}

/// `(center frequency, level in dB)` of each third-octave band from 31.5 Hz.
fn band_levels(buffer: &AudioBuffer) -> Vec<(f64, f64)> {
    const SIZE: usize = 8192;
    let window = hann(SIZE);
    let mut fft = RealFft::new(SIZE);
    let (mut re, mut im, mut frame) = (vec![0.0; fft.bins()], vec![0.0; fft.bins()], vec![0.0; SIZE]);
    let mut power = vec![0.0f64; fft.bins()];
    let mut frames = 0usize;
    for channel in [&buffer.left, &buffer.right] {
        let mut start = 0;
        while start + SIZE <= channel.len() {
            for (slot, (sample, window)) in frame.iter_mut().zip(channel[start..].iter().zip(&window)) {
                *slot = sample * window;
            }
            fft.forward(&frame, &mut re, &mut im);
            for (sum, (re, im)) in power.iter_mut().zip(re.iter().zip(&im)) {
                *sum += (*re as f64).powi(2) + (*im as f64).powi(2);
            }
            frames += 1;
            start += SIZE / 2;
        }
    }
    let mut bands = Vec::new();
    let mut low = 28.0;
    while low * 1.26 < (buffer.sample_rate / 2.0).min(20_000.0) {
        let high = low * 1.26;
        let bins = (low * SIZE as f64 / buffer.sample_rate).ceil() as usize
            ..(high * SIZE as f64 / buffer.sample_rate).ceil() as usize;
        if !bins.is_empty() {
            let sum: f64 = power[bins].iter().sum::<f64>() / frames.max(1) as f64;
            bands.push(((low * high).sqrt(), 10.0 * (sum / (SIZE as f64).powi(2)).max(1e-20).log10()));
        }
        low = high;
    }
    bands
}
