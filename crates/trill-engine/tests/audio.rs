//! Audio clips from file to output: every format the media layer decodes,
//! at any sample rate, played at the right pitch, level and length. Run
//! with `--nocapture` to see the measurements.

mod common;

use common::*;
use std::sync::Arc;
use trill_core::project::{AudioContent, WarpMode};
use trill_core::{ChainRef, Note};
use trill_dsp::{BuiltinFactory, Builtins, sampler};
use trill_engine::render::{RenderOptions, render};
use trill_engine::{AudioBackend, Problem, Session};
use trill_media::MediaCache;

fn options(sample_rate: f64) -> RenderOptions {
    RenderOptions { sample_rate: sample_rate as u32, max_tail_seconds: 1.0, ..RenderOptions::default() }
}

#[test]
fn clips_in_every_format_play_in_tune_and_for_their_length() {
    // One second of 440 Hz at 44.1 kHz, in each format, in a 48 kHz engine.
    let folder = folder();
    let source = file(&folder, "tone.wav");
    write_wav(&source, 44_100.0, &sine(440.0, 44_100.0, 1.0, 0.5));
    // File, how to make it, and how far a lossy codec may move the level
    // and the end of the sound.
    // (Decoding itself, in every format and bit depth, is the media layer's
    // business and tested there; this is about what the engine does next.)
    let formats: [(&str, &[&str], f32, f64); 4] = [
        ("tone.wav", &[], 0.005, 1.0),
        ("tone.flac", &["-c:a", "flac"], 0.005, 1.0),
        ("tone.mp3", &["-c:a", "libmp3lame", "-b:a", "192k"], 0.05, 5.0),
        ("tone.ogg", &["-c:a", "libvorbis", "-q:a", "6"], 0.05, 5.0),
    ];
    let mut skipped = Vec::new();
    println!("file          pitch Hz   level   ends at s");
    for (name, encode, level_tolerance, end_tolerance_ms) in formats {
        let path = file(&folder, name);
        if !encode.is_empty() && !ffmpeg(&source, &path, encode) {
            skipped.push(name);
            continue;
        }
        // The clip starts on beat 2 (1 s at 120 BPM) and is longer than the
        // file, so the file's own end is what ends the sound.
        let (mut project, track) = audio_project(120.0);
        add_clip(&mut project, track, &path.display().to_string(), 2.0, 4.0);
        let out = render(&project, &BuiltinFactory, &options(SR)).unwrap();
        assert!(out.problems.is_empty(), "{name}: {:?}", out.problems);

        let at = |seconds: f64| (seconds * SR) as usize;
        assert_eq!(peak(&out.left[..at(0.999)]), 0.0, "{name}: sound before the clip");
        let body = &out.left[at(1.1)..at(1.9)];
        let (hz, level, end) = (zero_crossing_hz(body, SR), peak(body), end_of_sound(&out.left, 0.05) as f64 / SR);
        println!("{name:<12}  {hz:>8.3}   {level:.4}  {end:.5}");
        assert!((hz - 440.0).abs() < 0.2, "{name}: {hz:.3} Hz, wanted 440");
        assert!((level - 0.5).abs() < 0.5 * level_tolerance, "{name}: level {level}");
        assert!((end - 2.0).abs() * 1000.0 < end_tolerance_ms, "{name}: the sound ends at {end:.5} s, wanted 2.0");
        assert!(out.left == out.right, "{name}: a mono file plays the same on both sides");
    }
    if !skipped.is_empty() {
        println!("skipped (no encoder on this machine): {skipped:?}");
    }
}

#[test]
fn a_file_at_the_engines_rate_is_copied_not_interpolated() {
    // Noise: the hardest thing to interpolate without changing it.
    let folder = folder();
    let noise: Vec<f32> = (0..96_000u32)
        .map(|n| {
            let x = n.wrapping_mul(2_654_435_761).rotate_left(13).wrapping_mul(0x9E37_79B9);
            ((x >> 8) as f32 / 8_388_608.0 - 1.0) * 0.5
        })
        .collect();
    let path = write_wav(&file(&folder, "noise.wav"), SR, &noise);

    // At 120 BPM a beat is a whole number of frames; at 140 it is 20571.43,
    // so a clip on beat 1 starts between two frames. Either way the file's
    // own samples must come out, moved by a whole number of frames.
    for (tempo, shift) in [(120.0, 24_000), (140.0, 20_571), (97.3, 29_599)] {
        let (mut project, track) = audio_project(tempo);
        add_clip(&mut project, track, &path, 1.0, 16.0);
        let out = render(&project, &BuiltinFactory, &options(SR)).unwrap();
        // Past the click guard at the clip's start. The mixer's gains are
        // unity to within a rounding error, which is all that may differ;
        // interpolating noise would change it by a tenth of full scale.
        let played = &out.left[shift + 480..shift + 90_000];
        let worst = played.iter().zip(&noise[480..90_000]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(worst < 1e-6, "{tempo} BPM: the file's samples were altered on the way out, by up to {worst}");
    }
}

#[test]
fn renders_at_other_rates_than_the_engines_are_right_for_clips_and_samplers() {
    // A session running at 48 kHz, whose caches hold everything at 48 kHz,
    // asked for renders at other rates. Both the clip (a 44.1 kHz file) and
    // the sampler (another) must come out at their pitch and length.
    let folder = folder();
    let clip_file = write_wav(&file(&folder, "clip.wav"), 44_100.0, &sine(440.0, 44_100.0, 1.0, 0.5));
    let sample_file = write_wav(&file(&folder, "sample.wav"), 44_100.0, &sine(330.0, 44_100.0, 2.0, 0.5));

    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    // Without the master limiter, whose look-ahead delays everything by a
    // millisecond and a half.
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter)).unwrap();
    let audio_track = session.edit(|project| Ok(project.add_track("Audio"))).unwrap();
    session
        .edit(|project| project.add_audio_clip(audio_track, "tone", 0.0, 4.0, AudioContent::new(clip_file.clone())))
        .unwrap();
    // The sampler plays its root from beat 4 to beat 6 (2 s to 3 s).
    let sampler_track = session.edit(|project| Ok(project.add_track("Sampler"))).unwrap();
    let device = session.add_device(ChainRef::Track(sampler_track), "builtin:sampler", None, None).unwrap();
    session.set_device_state(device, Some(sampler::State::new(sample_file).to_bytes())).unwrap();
    session.set_device_params(device, &[("volume".into(), 0.0), ("velocity_sens".into(), 0.0)]).unwrap();
    session
        .edit(|project| {
            let clip = project.add_midi_clip(sampler_track, "note", 4.0, 2.0)?;
            project.clip_mut(clip)?.add_notes(&[Note { pitch: 60, start: 0.0, length: 2.0, velocity: 100 }])
        })
        .unwrap();
    assert!(session.problems().is_empty(), "{:?}", session.problems());

    println!("render rate   clip Hz   clip ends s   sampler Hz   sampler starts s");
    for rate in [48_000.0, 44_100.0, 96_000.0, 32_000.0] {
        let out = session.render_job().render(&options(rate)).unwrap();
        assert_eq!(out.sample_rate, rate as u32);
        let at = |seconds: f64| (seconds * rate) as usize;
        let clip_hz = zero_crossing_hz(&out.left[at(0.1)..at(0.9)], rate);
        let clip_end = end_of_sound(&out.left[..at(1.5)], 0.05) as f64 / rate;
        let sampler_hz = zero_crossing_hz(&out.left[at(2.1)..at(2.9)], rate);
        let sampler_start =
            (at(1.5) + out.left[at(1.5)..].iter().position(|s| s.abs() > 0.01).expect("the sampler plays")) as f64
                / rate;
        println!("{rate:>8}     {clip_hz:>8.3}   {clip_end:.5}      {sampler_hz:>8.3}    {sampler_start:.5}");
        assert!((clip_hz - 440.0).abs() < 0.2, "{rate}: the clip plays at {clip_hz:.3} Hz");
        assert!((clip_end - 1.0).abs() < 0.001, "{rate}: the one-second clip ends at {clip_end:.5} s");
        assert!((sampler_hz - 330.0).abs() < 0.2, "{rate}: the sampler plays at {sampler_hz:.3} Hz");
        assert!((sampler_start - 2.0).abs() < 0.001, "{rate}: the note starts at {sampler_start:.5} s");
        assert!(
            (peak(&out.left[at(0.1)..at(0.9)]) - 0.5).abs() < 0.01
                && (peak(&out.left[at(2.1)..at(2.9)]) - 0.5).abs() < 0.01
        );
    }
}

#[test]
fn clips_pointing_far_outside_their_file_are_silent_not_a_crash() {
    // Offsets the model accepts (any finite number of seconds) but no file
    // reaches: copied, interpolated and stretched clips alike must simply
    // have nothing to play. A panic here would be a panic on the audio
    // thread.
    let folder = folder();
    let path = write_wav(&file(&folder, "tone.wav"), SR, &sine(440.0, SR, 1.0, 0.5));
    let (mut project, track) = audio_project(20.0);
    let cases = [
        (1e300, WarpMode::Off, None),
        (1e300, WarpMode::Repitch, Some(100.0)),
        // A ratio far beyond what the stretcher covers, so the copy is
        // read at a speed of its own.
        (1e300, WarpMode::Beats, Some(999.0)),
        (1e15, WarpMode::Tonal, Some(999.0)),
    ];
    for (index, (offset_seconds, warp, source_tempo)) in cases.into_iter().enumerate() {
        let content = AudioContent { offset_seconds, warp, source_tempo, ..AudioContent::new(path.clone()) };
        project.add_audio_clip(track, "far", index as f64, 1.0, content).unwrap();
    }
    let out = render(&project, &BuiltinFactory, &options(SR)).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    assert_eq!(peak(&out.left), 0.0);

    // The same on stand-ins, which is what a live graph plays first.
    let mut rig = LiveRig::new(SR);
    rig.compile(&project);
    assert_eq!(peak(&rig.play(4 * SR as usize)), 0.0);
}

#[test]
fn unreadable_clips_are_named_and_everything_else_plays() {
    let folder = folder();
    let good = write_wav(&file(&folder, "good.wav"), SR, &sine(440.0, SR, 1.0, 0.5));
    let garbage = file(&folder, "garbage.wav");
    std::fs::write(&garbage, vec![7u8; 4_096]).unwrap();
    let (mut project, track) = audio_project(120.0);
    add_clip(&mut project, track, &good, 0.0, 2.0);
    let missing = add_clip(&mut project, track, "no/such/file.wav", 2.0, 2.0);
    let damaged = add_clip(&mut project, track, &garbage.display().to_string(), 4.0, 2.0);

    let out = render(&project, &BuiltinFactory, &options(SR)).unwrap();
    assert!(rms(&out.left[4_800..43_200]) > 0.3, "the good clip plays");
    match out.problems.as_slice() {
        [
            Problem::AudioFile { clip: first, reason: gone, .. },
            Problem::AudioFile { clip: second, reason: bad, .. },
        ] => {
            assert_eq!((*first, *second), (missing, damaged));
            assert!(gone.contains("file.wav") && gone.contains("does not exist"), "{gone}");
            assert!(bad.contains("garbage.wav"), "{bad}");
        }
        other => panic!("expected the two bad clips to be reported, got {other:?}"),
    }
    assert!(out.problems[0].to_string().starts_with(&format!("{missing}: ")), "{}", out.problems[0]);
}
