//! The sample players measured from the outside: created through the
//! factory as the engine creates them, and rendered through the engine's
//! offline renderer. Run with `--nocapture` to see the numbers.

mod common;

use common::*;
use std::sync::Arc;
use trill_core::device::{DeviceError, DeviceFactory, Event, EventKind, ProcessInfo};
use trill_core::{ChainRef, Device, DeviceId, Note, Project, TrackId};
use trill_dsp::{BuiltinFactory, Builtins, drumrack, sample_problems, sampler};
use trill_engine::Problem;
use trill_engine::render::{RenderOptions, render};

const SR: f64 = 48_000.0;

/// A project with one track holding `uri` in the given state.
fn project_with(uri: &str, state: Option<Vec<u8>>, params: &[(&str, f64)]) -> (Project, TrackId, DeviceId) {
    let mut project = Project::new("t");
    let track = project.add_track("Sampled");
    let id = project.add_device(ChainRef::Track(track), uri, "Device", None).unwrap();
    let device = project.device_mut(id).unwrap();
    device.state = state;
    for (name, value) in params {
        device.params.insert(name.to_string(), *value);
    }
    (project, track, id)
}

fn device(uri: &str, state: Option<Vec<u8>>, params: &[(&str, f64)]) -> Device {
    let (project, _, id) = project_with(uri, state, params);
    project.device(id).unwrap().clone()
}

fn info() -> ProcessInfo {
    ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        loop_range: None,
    }
}

/// Plays one note for `frames` frames and returns the left channel.
fn play(factory: &dyn DeviceFactory, device: &Device, pitch: u8, frames: usize) -> Vec<f32> {
    let mut processor = factory.create(device, SR, 512).unwrap().processor;
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    for (index, (l, r)) in left.chunks_mut(512).zip(right.chunks_mut(512)).enumerate() {
        let events = [Event { offset: 0, kind: EventKind::NoteOn { pitch, velocity: 1.0 } }];
        processor.process(&info(), if index == 0 { &events } else { &[] }, l, r);
    }
    left
}

/// A bright test tone: every harmonic of `hz` up to 20 kHz at equal level,
/// which is about as much high-frequency content as a sample can hold.
fn bright_tone(hz: f64, sample_rate: f64, frames: usize) -> Vec<f32> {
    let harmonics = (20_000.0 / hz) as usize;
    let mut signal = vec![0.0f32; frames];
    for harmonic in 1..=harmonics {
        // Spread the phases so the peaks do not pile up.
        let phase = (harmonic * harmonic) as f64 * 0.37;
        for (index, sample) in signal.iter_mut().enumerate() {
            let angle = std::f64::consts::TAU * hz * harmonic as f64 * index as f64 / sample_rate + phase;
            *sample += (angle.sin() / harmonics as f64) as f32;
        }
    }
    signal
}

/// Splits a spectrum into the power on the harmonics of `hz` and the power
/// everywhere else, and returns the latter relative to the former in dB.
fn inharmonic_db(samples: &[f32], hz: f64) -> f64 {
    let size = 1 << 15;
    let spectrum = power_spectrum(samples, size);
    let bin_hz = SR / size as f64;
    let (mut harmonic, mut other) = (0.0, 0.0);
    for (bin, power) in spectrum.iter().enumerate().skip(4) {
        let nearest = ((bin as f64 * bin_hz) / hz).round().max(1.0) * hz;
        // The analysis window spreads each component over a few bins.
        if (bin as f64 * bin_hz - nearest).abs() <= 6.0 * bin_hz {
            harmonic += power;
        } else {
            other += power;
        }
    }
    10.0 * (other.max(1e-30) / harmonic.max(1e-30)).log10()
}

#[test]
fn playing_far_above_the_root_does_not_alias() {
    // A 110 Hz tone with harmonics to 20 kHz. Played two octaves up every
    // harmonic above 6 kHz would land past Nyquist; whatever is then found
    // between the harmonics of the new pitch is aliasing.
    let loader = Arc::new(MemoryLoader::default());
    loader.insert("bright.wav", bright_tone(110.0, SR, 400_000), SR);
    let factory = Builtins::with_loader(loader);
    let state = Some(sampler::State::new("bright.wav").to_bytes());
    let plain = [("volume", 0.0), ("velocity_sens", 0.0)];
    let sampler = device("builtin:sampler", state, &plain);

    println!("semitones above root   pitch Hz   aliasing dB   harmonics kept below 16 kHz");
    for semitones in [0u8, 5, 7, 12, 17, 19, 24, 31, 36] {
        let ratio = 2f64.powf(semitones as f64 / 12.0);
        let hz = 110.0 * ratio;
        let out = play(&factory, &sampler, 60 + semitones, 48_000);
        let steady = &out[2_000..2_000 + (1 << 15)];
        let aliasing = inharmonic_db(steady, hz);

        // What should survive did survive: harmonics up to 16 kHz at the
        // level they had in the file.
        let size = 1 << 15;
        let spectrum = power_spectrum(steady, size);
        let source_harmonics = (20_000.0 / 110.0) as usize as f64;
        let mut worst = 0.0f64;
        let mut count = 0;
        let mut harmonic = 1.0;
        while hz * harmonic < 16_000.0 && 110.0 * harmonic < 20_000.0 {
            let bin = (hz * harmonic * size as f64 / SR).round() as usize;
            let level = 10.0 * spectrum[bin - 5..bin + 6].iter().sum::<f64>().log10() - db(1.0 / source_harmonics);
            worst = if level.abs() > worst.abs() { level } else { worst };
            count += 1;
            harmonic += 1.0;
        }
        println!("{semitones:>10}            {hz:>8.2}   {aliasing:>8.1}      {count:>3} within {worst:+.2} dB");
        assert!(aliasing < -55.0, "+{semitones} st: aliasing at {aliasing:.1} dB");
        assert!(worst.abs() < 1.0, "+{semitones} st: a harmonic below 16 kHz changed by {worst:.2} dB");
        let pitch = peak_hz(&spectrum, hz, size);
        assert!((pitch - hz).abs() < hz * 5e-4, "+{semitones} st: pitch {pitch:.2}, wanted {hz:.2}");
    }
}

/// Frequency of the strongest component within 3% of `around`, from the
/// centroid of its lobe in a power spectrum.
fn peak_hz(spectrum: &[f64], around: f64, size: usize) -> f64 {
    let bin_hz = SR / size as f64;
    let (low, high) = ((around * 0.97 / bin_hz) as usize, (around * 1.03 / bin_hz).ceil() as usize + 1);
    let peak = (low..high).max_by(|a, b| spectrum[*a].total_cmp(&spectrum[*b])).unwrap();
    let lobe = peak.saturating_sub(4)..peak + 5;
    let weight: f64 = spectrum[lobe.clone()].iter().sum();
    lobe.map(|bin| bin as f64 * spectrum[bin]).sum::<f64>() / weight * bin_hz
}

#[test]
fn playing_below_the_root_adds_no_images() {
    // Slowed down, the gaps between the stored samples have to be filled in.
    // A poor interpolator fills them with a mirror image of the spectrum.
    let loader = Arc::new(MemoryLoader::default());
    loader.insert("bright.wav", bright_tone(440.0, SR, 240_000), SR);
    let factory = Builtins::with_loader(loader);
    let state = Some(sampler::State::new("bright.wav").to_bytes());
    let sampler = device("builtin:sampler", state, &[("volume", 0.0), ("velocity_sens", 0.0)]);
    println!("semitones below root   pitch Hz   images dB");
    for semitones in [1u8, 5, 12, 19, 24] {
        let hz = 440.0 * 2f64.powf(-(semitones as f64) / 12.0);
        let out = play(&factory, &sampler, 60 - semitones, 60_000);
        let images = inharmonic_db(&out[2_000..2_000 + (1 << 15)], hz);
        println!("{semitones:>10}            {hz:>8.2}   {images:>8.1}");
        assert!(images < -50.0, "-{semitones} st: images at {images:.1} dB");
    }
}

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 127 }
}

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR as u32, max_tail_seconds: 1.0, ..RenderOptions::default() }
}

/// Frame of the first sample louder than `threshold` at or after `from`.
fn first_above(samples: &[f32], from: usize, threshold: f32) -> Option<usize> {
    samples[from..].iter().position(|sample| sample.abs() > threshold).map(|index| index + from)
}

#[test]
fn the_engine_renders_a_sampler_track_at_the_right_times_and_pitches() {
    // The sample is a 330 Hz tone recorded at 44.1 kHz; the engine runs at
    // 48 kHz; the root note is 64. The clip plays the root, a fifth above
    // and an octave below, half a second apart (120 BPM).
    let loader = Arc::new(MemoryLoader::default());
    loader.insert("tone.wav", sine(330.0, 44_100.0, 44_100, 0.5), 44_100.0);
    let factory = Builtins::with_loader(loader.clone());
    let state = sampler::State::new("tone.wav").to_bytes();
    let params = [("root", 64.0), ("volume", 0.0), ("velocity_sens", 0.0), ("release", 0.01)];
    let (mut project, track, id) = project_with("builtin:sampler", Some(state.clone()), &params);
    let clip = project.add_midi_clip(track, "clip", 0.0, 4.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[note(64, 1.0, 0.5), note(71, 2.0, 0.5), note(52, 3.0, 0.5)]).unwrap();

    let out = render(&project, &factory, &options()).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    assert!(sample_problems(project.device(id).unwrap(), loader.as_ref()).is_empty());

    println!("note  starts at frame (wanted)  pitch Hz (wanted)  ends at frame (wanted)");
    for (beat, hz) in [(1.0, 330.0), (2.0, 494.413), (3.0, 165.0)] {
        let start = (beat * 0.5 * SR) as usize;
        let end = start + (0.25 * SR) as usize;
        // Silent until the note, sounding during it, gone 20 ms after it.
        assert!(peak(&out.left[start - 2_000..start - 1]) < 1e-4, "beat {beat}: sound before the note");
        let onset = first_above(&out.left, start - 2_000, 1e-3).unwrap();
        let measured = zero_crossing_hz(&out.left[start + 500..end - 500], SR);
        let last = out.left[..end + 2_000].iter().rposition(|sample| sample.abs() > 1e-3).unwrap();
        println!("{beat:>4}  {onset:>8} ({start})        {measured:>8.3} ({hz})   {last:>8} ({end})");
        assert!((onset as i64 - start as i64).abs() <= 3, "beat {beat}: starts at {onset}, wanted {start}");
        assert!((measured - hz).abs() < hz * 3e-4, "beat {beat}: {measured:.3} Hz, wanted {hz}");
        assert!(last >= end - 8 && last < end + 600, "beat {beat}: ends at {last}, wanted {end}");
        assert!((rms(&out.left[start + 500..end - 500]) - 0.5 / 2f64.sqrt()).abs() < 0.01);
    }
    assert!(out.left == out.right, "a mono sample, centered, stays mono");

    // The loader is asked for the file when the render creates its instance,
    // again when the engine asks the factory what is wrong with the device,
    // and once more by `sample_problems` above. Caching is the loader's job.
    assert_eq!(loader.loads(), 3);
    // Rendering again gives the same audio, bit for bit.
    let again = render(&project, &factory, &options()).unwrap();
    assert!(again.left == out.left && again.right == out.right);
    // And the state the device hands back for saving is the one it was given.
    let mut instance = factory.create(project.device(id).unwrap(), SR, 512).unwrap();
    assert_eq!(instance.controller.as_mut().unwrap().save_state(), Some(state));
}

#[test]
fn the_engine_renders_a_drum_rack_pattern() {
    // Kick: 60 Hz thump. Snare: noise burst. Hats: open is long, closed short.
    let loader = Arc::new(MemoryLoader::default());
    let decaying = |frames: usize, hz: f64, rate: f64| -> Vec<f32> {
        sine(hz, SR, frames, 0.8).iter().enumerate().map(|(i, s)| s * (-(i as f64) / SR * rate).exp() as f32).collect()
    };
    loader.insert("kick.wav", decaying(12_000, 60.0, 12.0), SR);
    loader.insert("snare.wav", (0..9_000).map(|i| noise(i) * 0.5 * (-(i as f32) / 2_000.0).exp()).collect(), SR);
    loader.insert("open.wav", (0..36_000).map(|i| (noise(i) - noise(i + 1)) * 0.2).collect(), SR);
    loader.insert("closed.wav", (0..2_400).map(|i| (noise(i + 9) - noise(i + 10)) * 0.2).collect(), SR);
    let factory = Builtins::with_loader(loader.clone());
    let pads = [(36, "kick.wav"), (38, "snare.wav"), (42, "closed.wav"), (46, "open.wav"), (49, "missing.wav")];
    let state = drumrack::State::with_pads(pads).unwrap().to_bytes();
    let (mut project, track, id) = project_with("builtin:drumrack", Some(state), &[("pad38_level", 0.5)]);
    let clip = project.add_midi_clip(track, "beat", 0.0, 4.0).unwrap();
    project
        .clip_mut(clip)
        .unwrap()
        .add_notes(&[
            note(36, 0.0, 0.1),
            note(38, 1.0, 0.1),
            note(46, 2.0, 0.1),
            note(42, 2.5, 0.1),
            // A pad whose file is missing, and a note with no pad.
            note(49, 3.0, 0.1),
            note(90, 3.0, 0.1),
        ])
        .unwrap();

    let out = render(&project, &factory, &options()).unwrap();
    let problems = sample_problems(project.device(id).unwrap(), loader.as_ref());
    assert_eq!(problems, ["pad 49: cannot load 'missing.wav': 'missing.wav' does not exist"]);
    // The engine reports the same thing, against the pad, and the device is
    // not failed by it.
    let reported = Problem::Sample { device: id, slot: Some("pad 49".into()), reason: problems[0].clone() };
    assert_eq!(out.problems, [reported], "a missing sample must be reported and must not fail the device");

    let beat = |beats: f64| (beats * 0.5 * SR) as usize;
    // Kick on beat 0: 60 Hz, ringing well past its 0.1-beat note.
    assert!((first_above(&out.left, 0, 1e-3).unwrap() as i64) < 64);
    let kick_hz = zero_crossing_hz(&out.left[200..9_000], SR);
    assert!((kick_hz - 60.0).abs() < 1.0, "kick at {kick_hz:.2} Hz");
    assert!(rms(&out.left[4_800..9_600]) > 0.05, "one-shot: the kick outlasts its note");
    // Snare on beat 1, at half level.
    let snare = first_above(&out.left, beat(1.0) - 100, 0.01).unwrap();
    assert!((snare as i64 - beat(1.0) as i64).abs() <= 2, "snare at {snare}");
    assert!((peak(&out.left[beat(1.0)..beat(1.0) + 2_000]) - 0.25).abs() < 0.03);
    // Open hat on beat 2 is choked by the closed hat on beat 2.5.
    assert!(rms(&out.left[beat(2.0) + 1_000..beat(2.5) - 100]) > 0.05);
    assert!(peak(&out.left[beat(2.5) + 3_000..beat(3.0) - 10]) < 1e-3, "the open hat was not choked");
    // Nothing at all on beat 3.
    assert_eq!(peak(&out.left[beat(3.0)..]), 0.0);
}

#[test]
fn a_factory_without_a_loader_reports_the_device_through_the_engine() {
    // What the workspace does today: `BuiltinFactory`, no loader. A sampler
    // with a file cannot be created; the engine lists it as a problem and
    // carries on with the rest of the project.
    let state = sampler::State::new("tone.wav").to_bytes();
    let (mut project, track, id) = project_with("builtin:sampler", Some(state), &[]);
    let clip = project.add_midi_clip(track, "clip", 0.0, 2.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[note(60, 0.0, 1.0)]).unwrap();
    let synth_track = project.add_track("Synth");
    project.add_device(ChainRef::Track(synth_track), "builtin:polysynth", "Synth", None).unwrap();
    let clip = project.add_midi_clip(synth_track, "clip", 0.0, 2.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[note(60, 0.0, 1.0)]).unwrap();

    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    match out.problems.as_slice() {
        [Problem::Device { device, reason }] => {
            assert_eq!(*device, id);
            assert!(reason.contains("tone.wav") && reason.contains("Builtins::with_loader"), "{reason}");
        }
        other => panic!("expected one device problem, got {other:?}"),
    }
    assert!(rms(&out.left) > 0.01, "the synth track still plays");

    // An empty sampler is fine without a loader.
    project.device_mut(id).unwrap().state = None;
    assert!(render(&project, &BuiltinFactory, &options()).unwrap().problems.is_empty());
    assert!(matches!(
        BuiltinFactory.create(&device("builtin:sampler", Some(sampler::State::new("x.wav").to_bytes()), &[]), SR, 512),
        Err(DeviceError::LoadFailed { .. })
    ));
}
