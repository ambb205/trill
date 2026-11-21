//! A comp as it sounds: where a stretch of one take meets another on the
//! track, the two are faded across one another, so that the seam neither
//! dips nor clicks. Rendered, and held to numbers.

mod common;

use common::*;
use trill_core::project::AudioContent;
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, render};

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.0, ..RenderOptions::default() }
}

/// Four seconds of noise that is the same every time for a `seed`.
fn noise(seed: u64) -> Vec<f32> {
    let mut state = seed;
    (0..4 * SR as usize)
        .map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.6
        })
        .collect()
}

/// A track with two takes over its first eight beats, the first of them
/// on the track with four beats of the second in its middle.
fn comped(folder: &tempfile::TempDir, first: &[f32], second: &[f32]) -> trill_core::Project {
    let (mut project, track) = audio_project(120.0);
    let a = write_wav(&file(folder, "a.wav"), SR, first);
    let b = write_wav(&file(folder, "b.wav"), SR, second);
    let one = project.add_take(track, "Take 1", 0.0, 8.0, AudioContent::new(a)).unwrap();
    let two = project.add_take(track, "Take 2", 0.0, 8.0, AudioContent::new(b)).unwrap();
    project.choose_take(one).unwrap();
    project.comp_take(two, 2.0, 6.0).unwrap();
    project
}

#[test]
fn two_takes_of_unrelated_noise_keep_their_level_across_the_seam() {
    let folder = folder();
    let (a, b) = (noise(11), noise(977));
    let out = render(&comped(&folder, &a, &b), &BuiltinFactory, &options()).unwrap().left;
    let level = |from: usize, to: usize| 20.0 * rms(&out[from..to]).log10();
    let away = level(12_000, 36_000);
    // The seams are at beats 2 and 6: frames 48,000 and 144,000. Across
    // each, in stretches as long as the fade and a third as long, the
    // level is what it is away from it.
    let mut worst: f32 = 0.0;
    for seam in [48_000usize, 144_000] {
        for (half, name) in [(288usize, "the fade"), (96, "its middle")] {
            let off = level(seam - half, seam + half) - away;
            worst = worst.max(off.abs());
            assert!(
                off.abs() < 1.0,
                "at frame {seam}, over {name}, the level is {off:.2} dB from what it is elsewhere"
            );
        }
    }
    println!("a comp's seams between unrelated noise: the level within {worst:.2} dB of what it is elsewhere");
    // On either side of a seam the track plays the one take and then the
    // other, each as it is in its file.
    for (frame, take) in [(40_000usize, &a), (60_000, &b), (150_000, &a)] {
        assert!((out[frame] - take[frame]).abs() < 1e-6, "frame {frame}");
    }
}

#[test]
fn a_seam_between_two_tones_has_no_step_in_it() {
    let folder = folder();
    let (a, b) = (sine(220.0, SR, 4.0, 0.5), sine(330.0, SR, 4.0, 0.5));
    let out = render(&comped(&folder, &a, &b), &BuiltinFactory, &options()).unwrap().left;
    // The steepest either tone gets from one frame to the next, and both
    // at once where they are faded across one another.
    let steepest = 0.5 * std::f32::consts::TAU * (220.0 + 330.0) / SR as f32;
    for seam in [48_000usize, 144_000] {
        let step = largest_step(&out[seam - 600..seam + 600]);
        assert!(step <= steepest * 1.05, "the largest step at the seam is {step}, and a tone alone steps {steepest}");
    }
    // A track that plays one take from end to end is that take, to the
    // sample: pieces of one recording that carry on from one another are
    // not faded.
    let (mut project, track) = audio_project(120.0);
    let path = write_wav(&file(&folder, "whole.wav"), SR, &a);
    let take = project.add_take(track, "Take", 0.0, 8.0, AudioContent::new(path)).unwrap();
    project.choose_take(take).unwrap();
    let whole = render(&project, &BuiltinFactory, &options()).unwrap().left;
    project.comp_take(take, 2.0, 6.0).unwrap();
    assert_eq!(project.track(track).unwrap().clips.len(), 3, "three pieces of the one take");
    assert_eq!(
        render(&project, &BuiltinFactory, &options()).unwrap().left,
        whole,
        "which play as the whole of it does"
    );
}
