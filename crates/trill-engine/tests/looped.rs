//! An audio clip that plays a stretch of its recording round: drawn out,
//! it repeats, at the song's tempo and through its changes, and a clip
//! that does not loop plays as it always did.

mod common;

use common::*;
use trill_core::project::{AudioLoop, WarpMode};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, render};

const SR: f64 = 48_000.0;

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.0, ..RenderOptions::default() }
}

/// Has an audio clip begin `seconds` into its recording.
fn begin_at(project: &mut trill_core::Project, clip: trill_core::ClipId, seconds: f64) {
    match &mut project.clip_mut(clip).unwrap().content {
        trill_core::project::ClipContent::Audio(audio) => audio.offset_seconds = seconds,
        trill_core::project::ClipContent::Midi(_) => panic!("not an audio clip"),
    }
}

/// Four seconds: silence, then half a second of a 300 Hz tone that dies away (the stretch that is looped),
/// then 1000 Hz for as long as the file lasts (what a clip that read on past the loop would play).
fn recording(path: &std::path::Path) -> String {
    let mut signal = vec![0.0f32; (4.0 * SR) as usize];
    let burst = sine(300.0, SR, 0.5, 0.8);
    for (index, sample) in burst.iter().enumerate() {
        let fade = (-(index as f32) / (0.12 * SR as f32)).exp();
        signal[(1.0 * SR) as usize + index] = sample * fade;
    }
    let after = sine(1000.0, SR, 2.5, 0.5);
    signal[(1.5 * SR) as usize..].copy_from_slice(&after);
    write_wav(path, SR, &signal)
}

#[test]
fn a_clip_that_plays_round_plays_its_stretch_again_and_again_and_nothing_past_it() {
    let folder = folder();
    let path = recording(&file(&folder, "burst.wav"));
    // At 120 a beat is the half second the loop lasts: eight beats are eight rounds.
    let (mut project, track) = audio_project(120.0);
    let clip = add_clip(&mut project, track, &path, 0.0, 8.0);
    begin_at(&mut project, clip, 1.0);
    project.set_audio_loop(clip, Some(AudioLoop { start_seconds: 1.0, seconds: 0.5 })).unwrap();
    let looped = render(&project, &BuiltinFactory, &options()).unwrap().left;
    assert_eq!(looped.len(), (4.0 * SR) as usize);

    let round = (0.5 * SR) as usize;
    // Clear of the few milliseconds over which one round is faded into the next.
    let (from, to) = (480, round - 480);
    for index in 0..8 {
        let heard = &looped[index * round..(index + 1) * round];
        // The stretch, from its start: the tone at its pitch, loud where it begins and gone by its end.
        let pitch = zero_crossing_hz(&heard[from..9_600], SR);
        assert!((pitch - 300.0).abs() < 6.0, "round {index} sounds at {pitch} Hz");
        assert!(rms(&heard[from..4_800]) > 8.0 * rms(&heard[to - 4_800..to]), "round {index} does not begin again");
        // And every round is the first, to the sample.
        assert!(heard[from..to] == looped[from..to], "round {index} is not the first again");
    }

    // The same stretch as eight clips laid end to end is the same sound, away from where they meet.
    let (mut copies, track) = audio_project(120.0);
    for index in 0..8 {
        let copy = add_clip(&mut copies, track, &path, index as f64, 1.0);
        begin_at(&mut copies, copy, 1.0);
    }
    let laid = render(&copies, &BuiltinFactory, &options()).unwrap().left;
    for index in 0..8 {
        let (a, b) = (index * round + from, index * round + to);
        assert!(looped[a..b] == laid[a..b], "round {index} differs from a copy laid there");
    }

    // Without the loop the clip plays on into what follows in the file, as it always did.
    project.set_audio_loop(clip, None).unwrap();
    let plain = render(&project, &BuiltinFactory, &options()).unwrap().left;
    assert!(plain[from..to] == looped[from..to], "the first half second is the same recording");
    let pitch = zero_crossing_hz(&plain[round + 4_800..2 * round], SR);
    assert!((pitch - 1000.0).abs() < 10.0, "past the stretch it plays what follows: {pitch} Hz");
}

#[test]
fn a_loop_drawn_out_keeps_time_with_the_song_at_another_tempo_and_through_a_change_of_tempo() {
    let folder = folder();
    // Four beats of clicks recorded at 120, a click in the middle of each beat.
    let path = write_wav(&file(&folder, "clicks.wav"), SR, &click_track(120.0, 4, SR));
    // The song is at 90. The clip follows it, and loops the first beat of the recording: drawn out to
    // eight beats it clicks once in every beat of the song.
    let (mut project, track) = audio_project(90.0);
    let clip = add_warped(&mut project, track, &path, 8.0, WarpMode::Repitch, 120.0);
    project.set_audio_loop(clip, Some(AudioLoop { start_seconds: 0.0, seconds: 0.5 })).unwrap();
    let rendered = render(&project, &BuiltinFactory, &options()).unwrap().left;
    let worst = assert_clicks_on_the_beat(&rendered, SR, 90.0, 8, 1.0);
    println!("a loop of one beat at 90: the worst click is {worst:.3} ms off its beat");

    // Begun a quarter of a beat into the loop, as a clip whose start was drawn in is: every click comes a
    // quarter of a beat sooner, and the loop comes round where it did in the recording.
    let shifted = project.split_clip(clip, 0.25).unwrap();
    assert!((project.clip(shifted).unwrap().audio().unwrap().offset_seconds - 0.125).abs() < 1e-9);
    project.remove_clip(clip).unwrap();
    project.move_clip(shifted, None, 0.0).unwrap();
    let rendered = render(&project, &BuiltinFactory, &options()).unwrap().left;
    let clicks = click_times(&rendered, SR, 0.2);
    let beat = 60.0 / 90.0;
    assert_eq!(clicks.len(), 8, "{clicks:?}");
    for (index, at) in clicks.iter().enumerate() {
        assert!((at - (index as f64 + 0.25) * beat).abs() < 0.002, "click {index} at {at}");
    }

    // The tempo changes under it at beat 4: the rounds go on a beat each at the new tempo.
    let (mut project, track) = audio_project(90.0);
    let clip = add_warped(&mut project, track, &path, 8.0, WarpMode::Repitch, 120.0);
    project.set_audio_loop(clip, Some(AudioLoop { start_seconds: 0.0, seconds: 0.5 })).unwrap();
    project.set_tempo_at(4.0, 150.0, false).unwrap();
    let rendered = render(&project, &BuiltinFactory, &options()).unwrap().left;
    let clicks = click_times(&rendered, SR, 0.2);
    let map = project.tempo_map();
    let wanted: Vec<f64> = (0..8).map(|index| map.seconds_at(index as f64 + 0.5)).collect();
    assert_eq!(clicks.len(), wanted.len(), "{clicks:?} against {wanted:?}");
    for (found, wanted) in clicks.iter().zip(&wanted) {
        assert!((found - wanted).abs() < 0.002, "{clicks:?} against {wanted:?}");
    }
}
