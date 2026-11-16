//! Warped audio clips, measured: timing against the beat, pitch, what a
//! live graph plays while the stretched audio is still being made, and what
//! editing does to what is heard. Run with `--nocapture` for the numbers.

mod common;

use common::*;
use std::sync::Arc;
use std::time::Duration;
use trill_core::Project;
use trill_core::project::WarpMode;
use trill_dsp::{BuiltinFactory, Builtins};
use trill_engine::render::{RenderOptions, Rendered, render};
use trill_engine::{AudioBackend, Problem, Session};
use trill_media::MediaCache;

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR as u32, max_tail_seconds: 1.0, ..RenderOptions::default() }
}

fn rendered(project: &Project) -> Rendered {
    let out = render(project, &BuiltinFactory, &options()).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    out
}

/// Eight beats of clicks recorded at 100 BPM.
fn clicks_at_100(folder: &tempfile::TempDir) -> String {
    write_wav(&file(folder, "clicks 100bpm.wav"), SR, &click_track(100.0, 8, SR))
}

/// Eight beats (4.8 s) of 440 Hz "recorded at 100 BPM".
fn tone_at_100(folder: &tempfile::TempDir) -> String {
    write_wav(&file(folder, "tone 100bpm.wav"), SR, &sine(440.0, SR, 4.8, 0.5))
}

#[test]
fn a_warped_click_track_lands_on_the_beats_of_the_project() {
    let folder = folder();
    let path = clicks_at_100(&folder);
    println!("project BPM   mode      worst click error (ms)   softest click");
    for tempo in [120.0, 90.0, 150.0, 100.0, 61.7] {
        for mode in [WarpMode::Beats, WarpMode::Repitch, WarpMode::Tonal] {
            let (mut project, track) = audio_project(tempo);
            add_warped(&mut project, track, &path, 8.0, mode, 100.0);
            let out = rendered(&project);
            // Tonal smears a click over its analysis window; it is the wrong
            // mode for clicks and is only held to being in time.
            let tolerance = if mode == WarpMode::Tonal { 6.0 } else { 1.0 };
            let worst = assert_clicks_on_the_beat(&out.left, SR, tempo, 8, tolerance);
            let softest = click_times(&out.left, SR, 0.2)
                .iter()
                .map(|time| peak(&out.left[(time * SR) as usize - 8..(time * SR) as usize + 8]))
                .fold(f32::MAX, f32::min);
            println!("{tempo:>8}      {mode:<8?}  {worst:>8.3}                {softest:.3}");
            if mode == WarpMode::Beats {
                assert!(softest > 0.85, "{tempo} BPM: Beats mode should copy the clicks, the softest is {softest}");
            }
            // Eight beats of recording fill eight beats of the project.
            let end = end_of_sound(&out.left, 0.01) as f64 / SR;
            assert!(end < 8.0 * 60.0 / tempo, "{tempo} BPM {mode:?}: sound at {end:.3} s, past the clip");
        }
    }

    // A render at another sample rate stretches a copy of its own, at that
    // rate, and is as exact.
    let (mut project, track) = audio_project(120.0);
    add_warped(&mut project, track, &path, 8.0, WarpMode::Beats, 100.0);
    let out = render(&project, &BuiltinFactory, &RenderOptions { sample_rate: 44_100, ..options() }).unwrap();
    let worst = assert_clicks_on_the_beat(&out.left, 44_100.0, 120.0, 8, 1.0);
    println!("at 44.1 kHz  Beats     {worst:>8.3}");

    // Unwarped, the same file ignores the tempo: its clicks stay 0.6 s apart.
    let (mut project, track) = audio_project(120.0);
    add_clip(&mut project, track, &path, 0.0, 16.0);
    assert_clicks_on_the_beat(&rendered(&project).left, SR, 100.0, 8, 0.05);
}

#[test]
fn tonal_keeps_the_pitch_repitch_moves_it_and_transpose_shifts_it() {
    let folder = folder();
    let path = tone_at_100(&folder);
    // Mode, source tempo, transpose; then the pitch and the length wanted.
    // The project is at 120 BPM, so a clip that follows it is 4.0 s long.
    let semitone = 2f64.powf(1.0 / 12.0);
    let cases: [(WarpMode, Option<f64>, f64, f64, f64); 9] = [
        (WarpMode::Off, None, 0.0, 440.0, 4.8),
        (WarpMode::Off, Some(100.0), 7.0, 440.0, 4.8),
        (WarpMode::Tonal, Some(100.0), 0.0, 440.0, 4.0),
        (WarpMode::Beats, Some(100.0), 0.0, 440.0, 4.0),
        (WarpMode::Repitch, Some(100.0), 0.0, 528.0, 4.0),
        (WarpMode::Repitch, Some(100.0), 5.0, 528.0, 4.0),
        (WarpMode::Tonal, Some(100.0), 12.0, 880.0, 4.0),
        (WarpMode::Tonal, Some(100.0), -5.0, 440.0 / semitone.powi(5), 4.0),
        // No known tempo: nothing to follow, but the pitch can still move.
        (WarpMode::Tonal, None, 7.0, 440.0 * semitone.powi(7), 4.8),
    ];
    println!("mode      source BPM   transpose   pitch Hz (wanted)     length s (wanted)   level");
    for (mode, source_tempo, transpose, hz, seconds) in cases {
        let (mut project, track) = audio_project(120.0);
        let clip = add_clip(&mut project, track, &path, 0.0, 12.0);
        project.set_clip_warp(clip, mode, source_tempo, transpose).unwrap();
        let out = rendered(&project);
        let end = end_of_sound(&out.left, 0.05) as f64 / SR;
        let body = &out.left[(0.3 * SR) as usize..((seconds - 0.3) * SR) as usize];
        let (measured, level) = (zero_crossing_hz(body, SR), rms(body) * std::f32::consts::SQRT_2);
        println!(
            "{mode:<8?}  {:<10}   {transpose:>5}      {measured:>8.3} ({hz:.3})   {end:.4} ({seconds})       {level:.3}",
            source_tempo.map_or("-".to_string(), |tempo| tempo.to_string())
        );
        assert!((measured - hz).abs() < hz * 0.002, "{mode:?} {transpose:+}: {measured:.3} Hz, wanted {hz:.3}");
        assert!((end - seconds).abs() < 0.01, "{mode:?} {transpose:+}: ends at {end:.4} s, wanted {seconds}");
        assert!((level - 0.5).abs() < 0.03, "{mode:?} {transpose:+}: level {level}");
    }
}

#[test]
fn warped_clips_keep_pitch_and_length_at_any_render_rate() {
    let folder = folder();
    let path = tone_at_100(&folder);
    let (mut project, track) = audio_project(120.0);
    let clip = add_warped(&mut project, track, &path, 12.0, WarpMode::Tonal, 100.0);
    project.set_clip_warp(clip, WarpMode::Tonal, Some(100.0), 7.0).unwrap();
    let wanted = 440.0 * 2f64.powf(7.0 / 12.0);
    for rate in [44_100.0, 96_000.0] {
        let out = render(&project, &BuiltinFactory, &RenderOptions { sample_rate: rate as u32, ..options() }).unwrap();
        assert!(out.problems.is_empty(), "{:?}", out.problems);
        let hz = zero_crossing_hz(&out.left[(0.3 * rate) as usize..(3.7 * rate) as usize], rate);
        let end = end_of_sound(&out.left, 0.05) as f64 / rate;
        println!("{rate} Hz: {hz:.3} Hz (wanted {wanted:.3}), ends at {end:.4} s (wanted 4)");
        assert!((hz - wanted).abs() < wanted * 0.002, "{rate}: {hz:.3} Hz, wanted {wanted:.3}");
        assert!((end - 4.0).abs() < 0.01, "{rate}: ends at {end:.4} s, wanted 4.0");
    }
}

#[test]
fn live_playback_starts_in_time_on_a_stand_in_and_changes_tempo_the_same_way() {
    let folder = folder();
    let path = clicks_at_100(&folder);
    let (mut project, track) = audio_project(120.0);
    add_warped(&mut project, track, &path, 8.0, WarpMode::Beats, 100.0);

    // The first graph cannot have the stretched audio yet. What it plays
    // instead is in time all the same.
    let mut rig = LiveRig::new(SR);
    assert_eq!(rig.compile(&project), 1, "one clip is waiting for its audio");
    let stand_in = rig.play(4 * SR as usize);
    let early = assert_clicks_on_the_beat(&stand_in, SR, 120.0, 8, 1.0);

    // Once it is ready the same clicks are there, now copied from the file.
    rig.compile_until_prepared(&project);
    rig.seek(0.0);
    let real = rig.play(4 * SR as usize);
    let late = assert_clicks_on_the_beat(&real, SR, 120.0, 8, 1.0);
    assert!(peak(&real) > 0.85, "the prepared audio keeps the clicks' level: {}", peak(&real));
    assert!(rig.problems.is_empty());
    println!("120 BPM: stand-in {early:.3} ms off, prepared {late:.3} ms off");

    // A new tempo: in time at once (on a stand-in again), then prepared.
    for tempo in [90.0, 133.0] {
        project.set_tempo(tempo).unwrap();
        assert_eq!(rig.compile(&project), 1, "the new tempo needs new audio");
        rig.seek(0.0);
        let frames = (8.0 * 60.0 / tempo * SR) as usize;
        let early = assert_clicks_on_the_beat(&rig.play(frames), SR, tempo, 8, 1.0);
        rig.compile_until_prepared(&project);
        rig.seek(0.0);
        let real = rig.play(frames);
        let late = assert_clicks_on_the_beat(&real, SR, tempo, 8, 1.0);
        assert!(peak(&real) > 0.85);
        println!("{tempo} BPM: stand-in {early:.3} ms off, prepared {late:.3} ms off");
    }

    // Back to a tempo it has been at: the audio is still there.
    project.set_tempo(120.0).unwrap();
    assert_eq!(rig.compile(&project), 0, "audio prepared before is kept for coming back to");
}

#[test]
fn the_stand_in_is_repitched_and_the_hand_over_does_not_click() {
    let folder = folder();
    let path = tone_at_100(&folder);
    let (mut project, track) = audio_project(120.0);
    add_warped(&mut project, track, &path, 8.0, WarpMode::Tonal, 100.0);

    // A full-scale jump would be up to 1.0; two tones cross-fading can move
    // no faster than both of their slopes together.
    let slope = |hz: f64| (std::f64::consts::TAU * hz / SR * 0.5) as f32;
    let limit = (slope(528.0) + slope(440.0)) * 1.05;
    println!("handed over at frame   largest step around it (limit {limit:.4})");
    // Hand over at different points of the waveform.
    for before in [9_000usize, 23_457, 31_111, 48_003] {
        let mut rig = LiveRig::new(SR);
        assert_eq!(rig.compile(&project), 1);
        let mut out = rig.play(before);
        rig.compile_until_prepared(&project);
        out.extend(rig.play(24_000));

        let stand_in_hz = zero_crossing_hz(&out[2_000..before - 500], SR);
        assert!((stand_in_hz - 528.0).abs() < 1.0, "the stand-in is the file played faster: {stand_in_hz:.2} Hz");
        let real_hz = zero_crossing_hz(&out[before + 2_000..], SR);
        assert!((real_hz - 440.0).abs() < 1.0, "the prepared audio is at pitch: {real_hz:.2} Hz");
        let step = largest_step(&out[before - 200..before + 1_500]);
        println!("{before:>10}             {step:.4}");
        assert!(step < limit, "handing over at frame {before} clicked: a step of {step}");
        let level = peak(&out[before - 200..before + 1_500]);
        assert!(level > 0.3 && level < 0.75, "no hole and no bump across the hand-over: {level}");
    }
}

/// A loop with something of everything: hits on the beat, a tone under
/// them, and noise, so that any change to what is played shows.
fn busy_loop(folder: &tempfile::TempDir) -> String {
    let mut signal = click_track(100.0, 8, SR);
    for (index, sample) in signal.iter_mut().enumerate() {
        let x = (index as u32).wrapping_mul(2_654_435_761).rotate_left(13).wrapping_mul(0x9E37_79B9);
        let noise = (x >> 8) as f32 / 8_388_608.0 - 1.0;
        let tone = (std::f64::consts::TAU * 196.0 * index as f64 / SR).sin() as f32;
        *sample = *sample * 0.6 + tone * 0.25 + noise * 0.05;
    }
    write_wav(&file(folder, "loop 100bpm.wav"), SR, &signal)
}

#[test]
fn splitting_a_clip_is_inaudible_warped_or_not() {
    let folder = folder();
    let path = busy_loop(&folder);
    for (tempo, mode) in [
        (120.0, WarpMode::Beats),
        (120.0, WarpMode::Tonal),
        (120.0, WarpMode::Repitch),
        (120.0, WarpMode::Off),
        // Tempos at which no beat falls on a whole frame.
        (131.0, WarpMode::Beats),
        (97.3, WarpMode::Off),
    ] {
        let build = || {
            let (mut project, track) = audio_project(tempo);
            let clip = add_warped(&mut project, track, &path, 8.0, mode, 100.0);
            (project, clip)
        };
        let (whole, _) = build();
        let whole = rendered(&whole);
        assert!(rms(&whole.left) > 0.05);

        // Cut in three, at points that are not round numbers of anything.
        let (mut project, clip) = build();
        let second = project.split_clip(clip, 3.0).unwrap();
        project.split_clip(second, 5.37).unwrap();
        let split = rendered(&project);
        assert!(whole.left == split.left && whole.right == split.right, "{tempo} BPM {mode:?}: the split is audible");

        // The same while the clip is still on its stand-in.
        let play = |project: &Project| {
            let mut rig = LiveRig::new(SR);
            rig.compile(project);
            rig.play(3 * SR as usize)
        };
        assert!(play(&build().0) == play(&project), "{tempo} BPM {mode:?}: the split is audible on the stand-in");

        // Removing the first piece leaves the others exactly in place.
        project.remove_clip(clip).unwrap();
        let rest = rendered(&project);
        let cut = (3.0 * 60.0 / tempo * SR) as usize;
        assert_eq!(peak(&rest.left[..cut - 1]), 0.0);
        // After the fade that now guards the second piece's open edge.
        assert!(whole.left[cut + 480..] == rest.left[cut + 480..], "{tempo} BPM {mode:?}: the rest moved");
    }
}

#[test]
fn trimming_a_warped_clip_hides_and_reveals_without_moving_anything() {
    let folder = folder();
    let path = clicks_at_100(&folder);
    let (mut project, track) = audio_project(120.0);
    let clip = add_warped(&mut project, track, &path, 8.0, WarpMode::Beats, 100.0);
    project.move_clip(clip, None, 4.0).unwrap();
    let whole = rendered(&project);
    let beat = |beats: f64| (beats * 0.5 * SR) as usize;

    // In by three beats: the first three clicks are gone, the rest are
    // where they were (the clip starts on beat 4, clicks on the half beat).
    assert_eq!(project.trim_clip_start(clip, 7.0).unwrap(), 7.0);
    let trimmed = rendered(&project);
    assert_eq!(peak(&trimmed.left[..beat(7.0)]), 0.0);
    assert!(whole.left[beat(7.0) + 480..] == trimmed.left[beat(7.0) + 480..], "trimming moved what is left");
    let clicks = click_times(&trimmed.left, SR, 0.2);
    assert_eq!(clicks.len(), 5);
    assert!((clicks[0] - 7.5 * 0.5).abs() < 0.001, "the first click left is the one on beat 7.5: {clicks:?}");

    // Out again, past where the recording starts: it stops at the start.
    assert_eq!(project.trim_clip_start(clip, 0.0).unwrap(), 4.0);
    let restored = rendered(&project);
    assert!(whole.left == restored.left, "trimming in and back out is lossless");

    // The same trim after a tempo change still lands on the new beats.
    project.set_tempo(90.0).unwrap();
    project.trim_clip_start(clip, 6.0).unwrap();
    let out = rendered(&project);
    let clicks = click_times(&out.left, SR, 0.2);
    let expected: Vec<f64> = (2..8).map(|index| (4.0 + index as f64 + 0.5) * 60.0 / 90.0).collect();
    assert_eq!(clicks.len(), expected.len(), "{clicks:?}");
    for (found, expected) in clicks.iter().zip(&expected) {
        assert!((found - expected).abs() < 0.001, "{clicks:?} against {expected:?}");
    }
}

#[test]
fn fades_are_timeline_lengths_whatever_the_warp() {
    let folder = folder();
    let path = tone_at_100(&folder);
    let (mut project, track) = audio_project(120.0);
    let clip = add_warped(&mut project, track, &path, 8.0, WarpMode::Tonal, 100.0);
    // One beat in, two beats out: 0.5 s and 1.0 s at 120 BPM.
    project.set_clip_fades(clip, 1.0, 2.0).unwrap();
    let out = rendered(&project);
    let level = |seconds: f64| peak(&out.left[((seconds - 0.01) * SR) as usize..((seconds + 0.01) * SR) as usize]);
    assert!(level(0.02) < 0.05, "the fade-in starts from silence: {}", level(0.02));
    assert!((level(0.25) - 0.25).abs() < 0.04, "half way up the fade-in: {}", level(0.25));
    assert!((level(1.5) - 0.5).abs() < 0.03, "full level between the fades: {}", level(1.5));
    assert!((level(3.5) - 0.25).abs() < 0.04, "half way down the fade-out: {}", level(3.5));
    assert!(level(3.98) < 0.05, "the fade-out ends in silence: {}", level(3.98));
}

#[test]
fn renders_of_a_warped_project_are_identical_every_time() {
    let folder = folder();
    let (loop_path, tone_path) = (busy_loop(&folder), tone_at_100(&folder));
    let build = |project: &mut Project| {
        project.set_tempo(127.0).unwrap();
        let track = project.add_track("Loops");
        let beats = add_warped(project, track, &loop_path, 8.0, WarpMode::Beats, 100.0);
        project.duplicate_clip(beats, 8.0, None).unwrap();
        let other = project.add_track("Tones");
        let tonal = add_warped(project, other, &tone_path, 8.0, WarpMode::Tonal, 100.0);
        project.set_clip_warp(tonal, WarpMode::Tonal, Some(100.0), 3.0).unwrap();
        let repitched = add_warped(project, other, &tone_path, 4.0, WarpMode::Repitch, 100.0);
        project.move_clip(repitched, None, 8.0).unwrap();
    };

    // Two renders that share nothing: everything is decoded and stretched
    // twice, and must come out the same to the last bit.
    let mut project = Project::new("t");
    build(&mut project);
    let (first, second) = (rendered(&project), rendered(&project));
    assert!(rms(&first.left) > 0.05);
    assert!(first.left == second.left && first.right == second.right, "two renders of one project differ");

    // A session's render, which shares the session's caches and may find
    // the audio already prepared, half prepared or not started.
    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let limiter = session.project().master.devices[0].id;
    session
        .edit(|project| {
            project.remove_device(limiter)?;
            build(project);
            Ok(())
        })
        .unwrap();
    let at_once = session.render_job().render(&options()).unwrap();
    assert!(session.wait_until_prepared(Duration::from_secs(60)));
    let later = session.render_job().render(&options()).unwrap();
    assert!(at_once.left == first.left && at_once.right == first.right, "a session's render differs from a plain one");
    assert!(later.left == first.left && later.right == first.right, "a render after preparation differs");
}

#[test]
fn the_session_says_how_many_clips_it_is_still_preparing() {
    let folder = folder();
    let path = clicks_at_100(&folder);
    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter)).unwrap();
    let track = session.edit(|project| Ok(project.add_track("Audio"))).unwrap();
    assert_eq!(session.preparing(), 0);

    // An unwarped clip needs no preparing; warping it does.
    let clip = session
        .edit(|project| {
            project.set_tempo(120.0)?;
            Ok(add_clip(project, track, &path, 0.0, 8.0))
        })
        .unwrap();
    assert_eq!(session.preparing(), 0);
    session.edit(|project| project.set_clip_warp(clip, WarpMode::Beats, Some(100.0), 0.0)).unwrap();
    assert_eq!(session.preparing(), 1);
    let revision = session.revision();

    // It plays meanwhile, and housekeeping swaps the real audio in.
    session.play();
    assert!(session.wait_until_prepared(Duration::from_secs(60)), "the clip was never prepared");
    assert_eq!(session.preparing(), 0);
    std::thread::sleep(Duration::from_millis(100));
    assert!(session.transport().playing && session.audio_faults() == 0);
    session.stop();
    assert_eq!(session.revision(), revision, "preparing audio is not an edit");
    assert!(!session.can_redo() && session.problems().is_empty());

    // A second clip of the same file at the same tempo shares the audio; a
    // tempo change needs new audio for both; undoing it does not.
    session.edit(|project| project.duplicate_clip(clip, 8.0, None)).unwrap();
    assert_eq!(session.preparing(), 0, "the same stretch of the same file is made once");
    session.edit(|project| project.set_tempo(90.0)).unwrap();
    assert_eq!(session.preparing(), 2);
    assert!(session.wait_until_prepared(Duration::from_secs(60)));
    assert!(session.undo());
    assert_eq!(session.preparing(), 0, "the audio for the tempo before is still there");

    // What is rendered is on the beat.
    let out = session.render_job().render(&options()).unwrap();
    assert_clicks_on_the_beat(&out.left[..4 * SR as usize], SR, 120.0, 8, 1.0);

    // A warped clip whose file is gone is a problem that names the clip.
    let gone = session
        .edit(|project| {
            let gone = add_clip(project, track, "gone 100bpm.wav", 16.0, 4.0);
            project.set_clip_warp(gone, WarpMode::Tonal, Some(100.0), 0.0)?;
            Ok(gone)
        })
        .unwrap();
    assert_eq!(session.preparing(), 0, "there is nothing to prepare for a file that is not there");
    assert!(
        matches!(session.problems(), [Problem::AudioFile { clip, .. }] if *clip == gone),
        "{:?}",
        session.problems()
    );
    session.edit(|project| project.remove_clip(gone)).unwrap();
    assert!(session.problems().is_empty(), "the problem goes with the clip");
}
