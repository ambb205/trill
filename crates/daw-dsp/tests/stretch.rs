//! Measures what the time-stretcher does to audio. Run with `--nocapture`
//! to see the numbers behind the assertions.

mod common;

use common::*;
use daw_core::audio::AudioBuffer;
use daw_dsp::stretch::{StretchMode, StretchOptions, detect_onsets, repitch, stretch};
use std::time::Instant;

const SR: f64 = 48_000.0;
const RATIOS: [f64; 4] = [0.5, 0.75, 1.5, 2.0];
const MODES: [StretchMode; 2] = [StretchMode::Beats, StretchMode::Tonal];

fn mono(samples: Vec<f32>) -> AudioBuffer {
    AudioBuffer { sample_rate: SR, left: samples.clone(), right: samples }
}

fn options(time_ratio: f64, mode: StretchMode) -> StretchOptions {
    StretchOptions { time_ratio, pitch_semitones: 0.0, mode }
}

/// A click every half second (120 BPM), a few samples wide like a rimshot's
/// leading edge.
fn click_track(frames: usize) -> Vec<f32> {
    let mut signal = vec![0.0f32; frames];
    for beat in (0..frames).step_by(24_000) {
        for (i, value) in [0.9f32, -0.7, 0.4, -0.2].into_iter().enumerate() {
            if beat + i < frames {
                signal[beat + i] = value;
            }
        }
    }
    signal
}

#[test]
fn output_length_is_exact() {
    let mut input = sine(330.0, SR, 30_011, 0.5);
    for (i, sample) in input.iter_mut().enumerate() {
        *sample += 0.2 * noise(i);
    }
    let input = mono(input);
    for mode in [StretchMode::Beats, StretchMode::Tonal, StretchMode::Repitch] {
        for step in 0..=40 {
            let ratio = 0.25 + step as f64 * 0.09375;
            for semitones in [0.0, 3.0] {
                let out = stretch(&input, &StretchOptions { time_ratio: ratio, pitch_semitones: semitones, mode });
                let want = (30_011.0 * ratio).round() as usize;
                assert_eq!((out.left.len(), out.right.len()), (want, want), "{mode:?} ratio {ratio}");
                assert_eq!(out.sample_rate, SR);
                assert!(out.left.iter().all(|s| s.is_finite()));
            }
        }
    }
    // Lengths that leave no room for any analysis, and out-of-range options.
    for frames in [0usize, 1, 2, 7, 100, 1000] {
        let tiny = mono(vec![0.25; frames]);
        for mode in [StretchMode::Beats, StretchMode::Tonal, StretchMode::Repitch] {
            for ratio in [0.5, 1.0, 1.7, f64::NAN, -3.0, 1e9] {
                let out = stretch(&tiny, &StretchOptions { time_ratio: ratio, pitch_semitones: f64::INFINITY, mode });
                let clamped = if ratio.is_finite() { ratio.clamp(0.25, 4.0) } else { 1.0 };
                assert_eq!(out.frames(), (frames as f64 * clamped).round() as usize);
                assert!(out.left.iter().chain(&out.right).all(|s| s.is_finite()));
            }
        }
    }
}

#[test]
fn unity_ratio_is_transparent() {
    let input = AudioBuffer {
        sample_rate: SR,
        left: (0..50_000).map(|i| noise(i) * 0.5).collect(),
        right: (0..50_000).map(|i| noise(i + 77_777) * 0.5).collect(),
    };
    for mode in [StretchMode::Beats, StretchMode::Tonal, StretchMode::Repitch] {
        assert!(stretch(&input, &options(1.0, mode)) == input, "{mode:?} altered the audio at ratio 1");
    }
}

#[test]
fn stretching_is_deterministic() {
    let input = mono((0..40_000).map(|i| noise(i) * (-((i % 9_000) as f32) / 2_000.0).exp()).collect());
    for mode in MODES {
        let options = StretchOptions { time_ratio: 1.37, pitch_semitones: -2.0, mode };
        assert!(stretch(&input, &options) == stretch(&input, &options));
    }
}

#[test]
fn a_sine_keeps_its_frequency_and_level() {
    println!("mode   ratio    hz        level dB  spurious dB  worst dB @ Hz");
    for mode in MODES {
        for hz in [110.0, 440.0, 3520.0] {
            let input = mono(sine(hz, SR, 96_000, 0.5));
            for ratio in RATIOS {
                let out = stretch(&input, &options(ratio, mode));
                // The steady part, away from the fades in and out.
                let steady = &out.left[4_800..out.left.len() - 4_800];
                let report = measure_sine(steady, SR);
                let level = report.level_db - db(0.5);
                println!(
                    "{mode:?} {ratio:<5} {hz:>6} -> {:>9.3}  {level:>7.3}  {:>10.1}  {:>7.1} @ {:.0}",
                    report.hz, report.spurious_db, report.worst_db, report.worst_hz
                );
                assert!((report.hz - hz).abs() < 0.5, "{mode:?} x{ratio}: {hz} Hz came out at {} Hz", report.hz);
                assert!(level.abs() < 0.5, "{mode:?} x{ratio} {hz} Hz: level changed by {level:.2} dB");
                // Beats mode splices whole samples, which lines a wave up to
                // within half a sample: inaudible in a kick or a tom, but a
                // measurable roughness on a pure high tone. That is what
                // tonal mode is for.
                let limit = match mode {
                    StretchMode::Tonal => -60.0,
                    _ if hz < 1000.0 => -40.0,
                    _ => -20.0,
                };
                assert!(report.spurious_db < limit, "{mode:?} x{ratio} {hz} Hz: spurious {:.1} dB", report.spurious_db);

                // Amplitude wobble: the short-term level must stay put.
                let levels: Vec<f64> = steady.chunks_exact(2_400).map(rms).collect();
                let (lo, hi) = levels.iter().fold((f64::MAX, 0.0f64), |(lo, hi), &l| (lo.min(l), hi.max(l)));
                println!("{:>58}level wobble {:.3} dB", "", db(hi / lo));
                assert!(db(hi / lo) < 0.5, "{mode:?} x{ratio} {hz} Hz: level wobbles by {:.2} dB", db(hi / lo));
            }
        }
    }
}

#[test]
fn a_chord_keeps_its_partials() {
    // An A major chord over two octaves, slightly detuned at the top: many
    // partials, the classic way to make a vocoder sound phasey.
    let partials = [220.0, 277.18, 329.63, 440.0, 554.9, 660.4, 880.0];
    let frames = 192_000;
    let mut signal = vec![0.0f32; frames];
    for (index, hz) in partials.iter().enumerate() {
        for (sample, value) in signal.iter_mut().zip(sine(*hz, SR, frames, 0.1 / (1.0 + index as f64 * 0.2))) {
            *sample += value;
        }
    }
    let input = mono(signal);
    let size = 1 << 16;
    let reference = power_spectrum(&input.left[10_000..], size);
    let lobe = |spectrum: &[f64], hz: f64| -> f64 {
        let bin = (hz * size as f64 / SR).round() as usize;
        spectrum[bin - 5..bin + 6].iter().sum()
    };
    // Tonal mode only: a time-domain splice cannot line up seven unrelated
    // partials at once, which is why beats mode is not for chords.
    println!("ratio  worst partial error dB   residue dB");
    let mode = StretchMode::Tonal;
    {
        for ratio in RATIOS {
            let out = stretch(&input, &options(ratio, mode));
            let spectrum = power_spectrum(&out.left[10_000..], size);
            let mut worst = 0.0f64;
            let mut kept = 0.0;
            for hz in partials {
                let change = 10.0 * (lobe(&spectrum, hz) / lobe(&reference, hz)).log10();
                worst = if change.abs() > worst.abs() { change } else { worst };
                kept += lobe(&spectrum, hz);
            }
            let total: f64 = spectrum.iter().sum();
            let residue = 10.0 * ((total - kept).max(1e-20) / kept).log10();
            println!("{ratio:<5}  {worst:>8.2}                {residue:>7.1}");
            assert!(worst.abs() < 1.0, "x{ratio}: a partial changed by {worst:.2} dB");
            assert!(residue < -35.0, "x{ratio}: residue {residue:.1} dB");
        }
    }
}

#[test]
fn clicks_stay_sharp_and_on_the_beat() {
    let input = mono(click_track(192_000));
    let originals = find_clicks(&input.left, 0.05, 240);
    assert_eq!(originals.len(), 8);
    println!("mode   ratio  clicks  worst timing (ms)  peak (min)  width (max frames)");
    for mode in MODES {
        for ratio in [0.5, 0.75, 0.9, 1.1, 1.5, 2.0] {
            let out = stretch(&input, &options(ratio, mode));
            let clicks = find_clicks(&out.left, 0.05, 240);
            let worst_ms = clicks
                .iter()
                .zip(&originals)
                .map(|(click, original)| (click.frame as f64 - original.frame as f64 * ratio).abs() / SR * 1000.0)
                .fold(0.0, f64::max);
            let min_peak = clicks.iter().map(|click| click.peak).fold(f32::MAX, f32::min);
            let max_width = clicks.iter().map(|click| click.width).max().unwrap_or(0);
            println!(
                "{mode:?} {ratio:<5}  {:>5}  {worst_ms:>10.3}        {min_peak:>6.3}     {max_width:>4}",
                clicks.len()
            );
            assert_eq!(clicks.len(), originals.len(), "{mode:?} x{ratio}: a click was lost or doubled");
            assert!(worst_ms < 1.0, "{mode:?} x{ratio}: a click is {worst_ms:.2} ms off");
            if mode == StretchMode::Beats {
                assert!(min_peak > 0.89 && max_width <= 4, "{mode:?} x{ratio}: a click was altered");
            } else {
                assert!(min_peak > 0.6 && max_width <= 48, "{mode:?} x{ratio}: a click was smeared");
            }
            // Nothing but the clicks: no pre-echo, no ghost copies.
            let mut residue = out.left.clone();
            for click in &clicks {
                let lo = click.frame.saturating_sub(96);
                residue[lo..(click.frame + 96).min(out.left.len())].fill(0.0);
            }
            let limit = if mode == StretchMode::Beats { 1e-6 } else { 0.02 };
            assert!(peak(&residue) < limit, "{mode:?} x{ratio}: {} between the clicks", peak(&residue));
        }
    }
}

/// A synthetic drum loop with the things real ones have: tonal kicks, noisy
/// snares, hats between the beats, a ghost note, all over a little hiss.
fn drum_loop() -> (Vec<f32>, Vec<usize>) {
    let frames = 192_000;
    let mut signal: Vec<f32> = (0..frames).map(|i| 0.001 * noise(i + 5)).collect();
    let mut hits = Vec::new();
    for step in 0..16 {
        let at = step * 12_000;
        if step % 4 == 0 {
            hits.push(at);
            for i in 0..14_000.min(frames - at) {
                let t = i as f32 / SR as f32;
                // 188 Hz falling to 48 Hz, fading to nothing by the end.
                let phase = std::f32::consts::TAU * (48.0 * t + 140.0 / 45.0 * (1.0 - (-t * 45.0).exp()));
                let fade = (1.0 - i as f32 / 14_000.0).min(0.1) * 10.0;
                signal[at + i] += 0.8 * phase.sin() * (-t * 11.0).exp() * fade;
            }
        }
        if step % 8 == 4 {
            for i in 0..9_000.min(frames - at) {
                let t = i as f32 / SR as f32;
                signal[at + i] += 0.5 * noise(at + i) * (-t * 22.0).exp()
                    + 0.3 * (std::f32::consts::TAU * 190.0 * t).sin() * (-t * 30.0).exp();
            }
        }
        if step % 2 == 1 {
            hits.push(at);
            for i in 0..2_400.min(frames - at) {
                let t = i as f32 / SR as f32;
                let hat = noise(at + i) - noise(at + i + 1);
                signal[at + i] += 0.12 * hat * (-t * 90.0).exp();
            }
        }
    }
    // A ghost snare, 16 dB down, off the grid.
    let ghost = 12_000 * 6 + 6_000;
    hits.push(ghost);
    for i in 0..3_000 {
        let t = i as f32 / SR as f32;
        signal[ghost + i] += 0.08 * noise(ghost + i) * (-t * 40.0).exp();
    }
    hits.sort_unstable();
    (signal, hits)
}

#[test]
fn drum_hits_keep_their_timing_and_attack() {
    let (signal, hits) = drum_loop();
    let input = mono(signal);
    let found = detect_onsets(&input);
    println!("detected {} onsets for {} hits (plus snares on kicks)", found.len(), hits.len());
    for &hit in &hits {
        let nearest = found.iter().map(|onset| (onset.frame as i64 - hit as i64).abs()).min().unwrap();
        assert!(nearest <= 48, "hit at {hit} was not detected (nearest onset {nearest} frames away)");
    }

    // A hit is an onset this pronounced. The weakest in the loop, the ghost
    // note, scores over 2; the flicker any stretcher leaves in a noisy tail
    // scores around 0.3.
    let hit_count = |onsets: &[daw_dsp::stretch::Onset]| onsets.iter().filter(|onset| onset.strength >= 0.6).count();
    let flicker = |onsets: &[daw_dsp::stretch::Onset]| {
        onsets.iter().filter(|onset| onset.strength < 0.6).map(|onset| onset.strength).fold(0.0, f32::max)
    };
    println!("mode   ratio  worst onset error (ms)  worst attack error dB  hits  strongest flicker");
    for mode in MODES {
        for ratio in RATIOS {
            let out = stretch(&input, &options(ratio, mode));
            let stretched = detect_onsets(&out);
            let mut worst_ms = 0.0f64;
            let mut worst_attack = 0.0f64;
            for &hit in &hits {
                let want = hit as f64 * ratio;
                let error = stretched.iter().map(|onset| (onset.frame as f64 - want).abs()).fold(f64::MAX, f64::min);
                worst_ms = worst_ms.max(error / SR * 1000.0);
                // Energy of the first 5 ms of the hit, against the original.
                let at = want.round() as usize;
                let attack = db(rms(&out.left[at..at + 240]) / rms(&input.left[hit..hit + 240]));
                worst_attack = if attack.abs() > worst_attack.abs() { attack } else { worst_attack };
            }
            println!(
                "{mode:?} {ratio:<5}  {worst_ms:>10.3}             {worst_attack:>7.2}            {:>3}   {:.2}",
                hit_count(&stretched),
                flicker(&stretched)
            );
            let (timing, attack) = if mode == StretchMode::Beats { (1.0, 0.1) } else { (3.0, 3.0) };
            assert!(worst_ms < timing, "{mode:?} x{ratio}: a hit is {worst_ms:.2} ms off");
            assert!(worst_attack.abs() < attack, "{mode:?} x{ratio}: an attack changed by {worst_attack:.2} dB");
            assert_eq!(hit_count(&stretched), hit_count(&found), "{mode:?} x{ratio}: hits were lost or doubled");
            // The overall level survives. Both modes keep attacks whole and
            // take the change out of the quieter tails between them, so
            // squeezing raises the average level of a drum loop a little and
            // stretching lowers it; the peaks do not move.
            let level = db(rms(&out.left) / rms(&input.left));
            assert!(level.abs() < 2.5, "{mode:?} x{ratio}: level changed by {level:.2} dB");
            assert!((peak(&out.left) / peak(&input.left) - 1.0).abs() < 0.12, "{mode:?} x{ratio}: peak changed");
        }
    }
}

#[test]
fn steady_noise_keeps_its_level() {
    // Noise is where a vocoder loses level (windows that no longer line up
    // add as powers, not amplitudes) and where a splicer leaves holes.
    let white: Vec<f32> = (0..144_000).map(|i| 0.3 * noise(i)).collect();
    let mut state = 0.0f32;
    let dark: Vec<f32> = (0..144_000)
        .map(|i| {
            state = state * 0.98 + 0.05 * noise(i + 7);
            state
        })
        .collect();
    println!("signal  mode   ratio  level change dB  wobble dB (50 ms)");
    for (name, signal) in [("white", white), ("dark", dark)] {
        let input = mono(signal);
        for mode in MODES {
            for ratio in RATIOS {
                let out = stretch(&input, &options(ratio, mode));
                let middle = |samples: &[f32]| rms(&samples[samples.len() / 8..samples.len() * 7 / 8]);
                let change = db(middle(&out.left) / middle(&input.left));
                let levels: Vec<f64> = out.left[4_800..out.frames() - 4_800].chunks_exact(2_400).map(rms).collect();
                let (lo, hi) = levels.iter().fold((f64::MAX, 0.0f64), |(lo, hi), &l| (lo.min(l), hi.max(l)));
                println!("{name:<6} {mode:?} {ratio:<5}  {change:>7.2}         {:.2}", db(hi / lo));
                assert!(change.abs() < 0.5, "{name} {mode:?} x{ratio}: level changed by {change:.2} dB");
                assert!(db(hi / lo) < 3.0, "{name} {mode:?} x{ratio}: level wobbles by {:.2} dB", db(hi / lo));
            }
        }
    }
}

#[test]
fn stereo_image_is_preserved() {
    // A mono-compatible source: the same sound panned, so the channels are
    // identical but for level. Then a wide one with a decorrelated side.
    let frames = 96_000;
    let (drums, _) = drum_loop();
    let tone: Vec<f32> =
        sine(220.0, SR, frames, 0.3).iter().zip(sine(331.0, SR, frames, 0.2)).map(|(a, b)| a + b).collect();
    let mid: Vec<f32> = drums[..frames].iter().zip(&tone).map(|(d, t)| d * 0.6 + t).collect();
    println!("mode   ratio  panned: correlation  balance dB | wide: correlation in -> out");
    for mode in MODES {
        for ratio in RATIOS {
            let panned = AudioBuffer {
                sample_rate: SR,
                left: mid.iter().map(|s| s * 0.9).collect(),
                right: mid.iter().map(|s| s * 0.45).collect(),
            };
            let out = stretch(&panned, &options(ratio, mode));
            let correlation_panned = correlation(&out.left, &out.right);
            let balance = db(rms(&out.left) / rms(&out.right)) - db(2.0);
            // The channels must still be the same signal at a 2:1 level: the
            // residue after scaling one to the other is what would not sum
            // to mono.
            let residue: Vec<f32> = out.left.iter().zip(&out.right).map(|(l, r)| l - 2.0 * r).collect();
            let residue_db = db(rms(&residue) / rms(&out.left));

            let wide = AudioBuffer {
                sample_rate: SR,
                left: mid.iter().enumerate().map(|(i, s)| s + 0.1 * noise(i)).collect(),
                right: mid.iter().enumerate().map(|(i, s)| s + 0.1 * noise(i + 500_000)).collect(),
            };
            let out_wide = stretch(&wide, &options(ratio, mode));
            let (before, after) = (correlation(&wide.left, &wide.right), correlation(&out_wide.left, &out_wide.right));
            println!(
                "{mode:?} {ratio:<5}  {correlation_panned:.6}  {balance:>6.3}  residue {residue_db:>7.1} dB | {before:.4} -> {after:.4}"
            );
            assert!(correlation_panned > 0.9999, "{mode:?} x{ratio}: correlation fell to {correlation_panned}");
            assert!(balance.abs() < 0.01 && residue_db < -80.0, "{mode:?} x{ratio}: the channels drifted apart");
            assert!((after - before).abs() < 0.03, "{mode:?} x{ratio}: width changed from {before:.3} to {after:.3}");
        }
    }
}

#[test]
fn pitch_shifts_without_changing_the_length() {
    let input = mono(sine(440.0, SR, 96_000, 0.5));
    println!("mode   semitones  time ratio  hz");
    for mode in MODES {
        for (semitones, ratio) in [(12.0, 1.0), (-12.0, 1.0), (7.0, 1.0), (-5.0, 1.5), (3.0, 0.75)] {
            let out = stretch(&input, &StretchOptions { time_ratio: ratio, pitch_semitones: semitones, mode });
            assert_eq!(out.frames(), (96_000.0 * ratio).round() as usize);
            let report = measure_sine(&out.left[4_800..out.frames() - 4_800], SR);
            let want = 440.0 * 2f64.powf(semitones / 12.0);
            println!("{mode:?} {semitones:>5}  {ratio:<5}  {:.3} (want {want:.3})", report.hz);
            assert!((report.hz - want).abs() < want * 0.001, "{mode:?}: wanted {want:.2} Hz, got {:.2}", report.hz);
            assert!((report.level_db - db(0.5)).abs() < 0.5);
        }
    }
}

#[test]
fn repitch_changes_pitch_with_speed() {
    let input = mono(sine(440.0, SR, 96_000, 0.5));
    for ratio in [0.5, 0.75, 1.5, 2.0] {
        let out = repitch(&input, ratio);
        assert_eq!(out.frames(), (96_000.0 * ratio).round() as usize);
        let report = measure_sine(&out.left[2_400..out.frames() - 2_400], SR);
        assert!((report.hz - 440.0 / ratio).abs() < 0.2, "ratio {ratio}: {} Hz", report.hz);
        assert!((report.level_db - db(0.5)).abs() < 0.05 && report.spurious_db < -80.0, "{}", report.spurious_db);
    }
}

#[test]
fn a_loop_stays_a_loop() {
    // The stretched file must begin and end on the input's own first and
    // last samples, or a seamless loop would click when repeated.
    let (signal, _) = drum_loop();
    let input = mono(signal);
    for mode in MODES {
        for ratio in RATIOS {
            let out = stretch(&input, &options(ratio, mode));
            let (frames, out_frames) = (input.frames(), out.frames());
            let head = out.left[..64].iter().zip(&input.left[..64]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            let tail = out.left[out_frames - 64..]
                .iter()
                .zip(&input.left[frames - 64..])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            let limit = if mode == StretchMode::Beats { 1e-6 } else { 0.02 };
            assert!(head < limit && tail < limit, "{mode:?} x{ratio}: ends differ by {head} / {tail}");
        }
    }
}

#[test]
fn runs_much_faster_than_real_time() {
    // Ten seconds of stereo at 48 kHz: a drum loop plus a chord.
    let (drums, _) = drum_loop();
    let frames = 480_000;
    let left: Vec<f32> = (0..frames).map(|i| drums[i % drums.len()] * 0.7).collect();
    let right: Vec<f32> = left
        .iter()
        .zip(sine(220.0, SR, frames, 0.2))
        .enumerate()
        .map(|(i, (l, s))| l * 0.8 + s + 0.02 * noise(i))
        .collect();
    let input = AudioBuffer { sample_rate: SR, left, right };
    println!("mode   ratio  x realtime (input)  x realtime (output)");
    for mode in [StretchMode::Beats, StretchMode::Tonal, StretchMode::Repitch] {
        for ratio in [0.5, 0.77, 1.3, 2.0] {
            // Best of three, so that another process taking the core for a
            // moment does not pass for a slow algorithm.
            let mut elapsed = f64::MAX;
            let mut seconds = 0.0;
            for _ in 0..3 {
                let start = Instant::now();
                seconds = stretch(&input, &options(ratio, mode)).seconds();
                elapsed = elapsed.min(start.elapsed().as_secs_f64());
            }
            let (by_input, by_output) = (input.seconds() / elapsed, seconds / elapsed);
            println!("{mode:?} {ratio:<5}  {by_input:>8.0}  {by_output:>8.0}");
            // The target is 20 times real time, and on an idle machine every
            // case here clears it with room to spare (see the printed
            // numbers). The assertion is set lower, so that it catches a
            // real regression without failing whenever the machine is busy.
            assert!(by_input.min(by_output) > 6.0, "{mode:?} x{ratio}: only {by_input:.1}x real time");
        }
    }
}
