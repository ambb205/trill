//! A reference track: a recording beside the song. It is silent unless
//! it is listened to, heard in place of the song while it is, and never
//! in a render or in a stem, whatever is being listened to.

use trill_core::project::Reference;
use trill_core::{ChainRef, Note, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::render::{RenderOptions, Stems, render};
use trill_engine::{AudioSources, Processor};

const SR: u32 = 48_000;

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// A track that holds one note of a plain sine for eight beats.
fn sine_track(project: &mut Project, name: &str, pitch: u8) -> TrackId {
    let track = project.add_track(name);
    let device = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    let params = &mut project.device_mut(device).unwrap().params;
    for (key, value) in [("osc1_wave", 0.0), ("attack", 0.001), ("release", 0.01), ("sustain", 1.0)] {
        params.insert(key.to_string(), value);
    }
    let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[Note { pitch, start: 0.0, length: 8.0, velocity: 100 }]).unwrap();
    track
}

/// A song of one track, and the same song with a reference beside it.
fn songs() -> (Project, Project, TrackId, TrackId) {
    let mut song = Project::new("t");
    let part = sine_track(&mut song, "Part", 58);
    let mut with = song.clone();
    let reference = sine_track(&mut with, "Their record", 70);
    with.set_reference(reference, Some(Reference { study: Some("0a1b2c3d4e".into()), listening: false })).unwrap();
    (song, with, part, reference)
}

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR, max_tail_seconds: 1.0, ..RenderOptions::default() }
}

#[test]
fn a_render_is_of_the_song_whether_or_not_a_reference_is_listened_to() {
    let (song, with, part, reference) = songs();
    let alone = render(&song, &BuiltinFactory, &options()).unwrap();
    let beside = render(&with, &BuiltinFactory, &options()).unwrap();
    assert!(rms(&alone.left) > 0.01);
    assert!(alone.left == beside.left && alone.right == beside.right, "a reference changed the render");

    // Listened to, it is heard in place of the song while the song plays: and still not rendered.
    let mut listening = with.clone();
    listening.listen_to_reference(reference, true).unwrap();
    let still = render(&listening, &BuiltinFactory, &options()).unwrap();
    assert!(alone.left == still.left, "a reference that is listened to changed the render");

    // A render that writes stems writes none of it.
    let folder = tempfile::tempdir().unwrap();
    let stems = RenderOptions { stems: Some(Stems::all(folder.path().join("stems"))), ..options() };
    let rendered = render(&listening, &BuiltinFactory, &stems).unwrap();
    let of: Vec<Option<TrackId>> = rendered.measured.stems.iter().map(|stem| stem.track).collect();
    assert!(of.contains(&Some(part)) && !of.contains(&Some(reference)), "{of:?}");

    // A recording that goes on after the song makes the render no longer.
    let mut long = with.clone();
    let clip = long.track(reference).unwrap().clips[0].id;
    long.set_clip_span(clip, 0.0, 64.0).unwrap();
    assert_eq!(long.length_beats(), 8.0);
    assert_eq!(long.length_shown(), 64.0);
    assert_eq!(render(&long, &BuiltinFactory, &options()).unwrap().left.len(), alone.left.len());

    // Asked for by name, it is rendered as the track it is: that is how a mix is measured against it.
    let only = RenderOptions { only_tracks: Some(vec![reference]), ..options() };
    let asked = render(&with, &BuiltinFactory, &only).unwrap();
    assert!((crossings(&asked.left[24_000..72_000]) as f64 - 466.2).abs() < 4.0);
    assert!((rms(&asked.left[24_000..72_000]) / rms(&alone.left[24_000..72_000]) - 1.0).abs() < 0.05);
}

/// How often a second of sound goes up through nothing: its pitch, of a sine.
fn crossings(samples: &[f32]) -> usize {
    let rising = samples.windows(2).filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0).count();
    rising * SR as usize / samples.len()
}

#[test]
fn a_reference_is_silent_while_the_song_plays_and_heard_alone_while_it_is_listened_to() {
    let (song, with, _, reference) = songs();
    let mut listening = with.clone();
    listening.listen_to_reference(reference, true).unwrap();

    // What is heard of each song by itself, for a second, as a song that plays is compiled.
    let heard = |project: &Project| {
        let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
        let mut compiler = Compiler {
            factory: &BuiltinFactory,
            bank: &mut bank,
            meters: &mut meters,
            audio: &audio,
            stand_ins: None,
            sample_rate: SR as f64,
            purpose: trill_core::device::Purpose::Playing,
        };
        let mut processor = Processor::new(SR as f64);
        drop(processor.swap_graph(compiler.compile(project, true).0));
        processor.play();
        let (mut left, mut right) = (vec![0.0f32; SR as usize], vec![0.0f32; SR as usize]);
        for (l, r) in left.chunks_mut(480).zip(right.chunks_mut(480)) {
            processor.process(l, r);
        }
        left
    };
    let (alone, beside, listened) = (heard(&song), heard(&with), heard(&listening));
    // With the reference beside it the song sounds as it does alone.
    let apart: Vec<f32> = alone.iter().zip(&beside).map(|(a, b)| a - b).collect();
    assert!(rms(&alone[24_000..]) > 0.01 && rms(&apart) < 1e-6, "{}", rms(&apart));
    // Listened to, the reference is what is heard: another pitch, and none of the song under it.
    let (song_hz, heard_hz) = (crossings(&alone[24_000..]), crossings(&listened[24_000..]));
    assert!((song_hz as f64 - 233.1).abs() < 4.0, "{song_hz}");
    assert!((heard_hz as f64 - 466.2).abs() < 4.0, "{heard_hz}");
    assert!((rms(&listened[24_000..]) / rms(&alone[24_000..]) - 1.0).abs() < 0.05);
}

#[test]
fn only_a_track_that_feeds_nothing_can_be_a_reference_and_a_file_saved_before_has_none() {
    let (_, mut with, part, reference) = songs();
    // What is not a reference has nothing to listen to apart from the song.
    let refused = with.listen_to_reference(part, true).unwrap_err().to_string();
    assert!(refused.contains("is not a reference"), "{refused}");
    // A track that plays through a group is part of the song.
    let group = with.add_track_of_kind("Band", trill_core::TrackKind::Group);
    with.set_track_group(part, Some(group)).unwrap();
    let refused = with.set_reference(part, Some(Reference::default())).unwrap_err().to_string();
    assert!(refused.contains("a reference is beside the song"), "{refused}");
    assert!(with.set_reference(group, Some(Reference::default())).is_err());
    // It is written into the file only where there is one, and read back.
    let text = serde_json::to_string(&with).unwrap();
    assert_eq!(text.matches("\"reference\"").count(), 1);
    let back: Project = serde_json::from_str(&text).unwrap();
    assert_eq!(back.track(reference).unwrap().reference, with.track(reference).unwrap().reference);
    with.set_reference(reference, None).unwrap();
    assert!(!serde_json::to_string(&with).unwrap().contains("\"reference\""));
}
