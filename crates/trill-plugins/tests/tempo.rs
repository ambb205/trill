//! An installed plugin that keeps time of its own, through changes of the
//! song's tempo: Serum 2's arpeggiator, which plays its steps by what the
//! host tells it of the song with every block.
//!
//! A key is held from the start of a song that goes from 120 to 90 at
//! beat 8 and to 150 at beat 16, and the song is rendered. Where each of
//! the arpeggiator's notes begins is found in the sound, turned into the
//! beat the song's tempo map has at that moment, and held to the steps
//! the arpeggiator played before the first change: as many to the beat,
//! and on the same grid, in every tempo. A plugin that was not told of a
//! change would play on at the steps of the tempo before, which is a
//! third off by the second beat after one.
//!
//! What it found (2026-11-17): sixteenths at 120, 90 and 150, 125, 167 and
//! 100 ms apart, 32, 33 and 31 of each 32 heard on the grid. Serum 2 goes
//! by the beats and the tempo it is told, which were the song's before
//! the time and the bar were: told the time the hosts used to work out
//! from the block's tempo, it played the same notes. So this holds that
//! an installed plugin follows a change, and not that it minds the time.
//!
//! `TRILL_TEMPO_PRESETS` (names, separated by semicolons) renders other
//! presets and says what is heard of each, without holding them to
//! anything: most cannot be heard note by note.
//!
//! ```text
//! cargo test -p trill-plugins --test tempo -- --include-ignored --nocapture --test-threads 1
//! ```
//!
//! It makes three instances of the plugin, one after the other (the
//! song's, the one a render waits for and the render's). Nothing of the
//! user's is read or written, and no window opens.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::presets::PresetStatus;
use trill_core::{ChainRef, Note, TempoMap};
use trill_dsp::BuiltinFactory;
use trill_engine::{AudioBackend, RenderOptions, Session};
use trill_plugins::{Scanner, Vst3Config, Vst3Factory};

const SERUM_2: &str = "vst3:56534558667350736572756D20320000";
/// A preset of its makers' whose arpeggiator plays short notes, far
/// enough apart to be told from each other in the sound.
const ARPEGGIO: &str = "ARP - Aardvark";
/// The song: the tempo it begins at, and each change with its beat.
const BEGINS_AT: f64 = 120.0;
const CHANGES: [(f64, f64); 2] = [(8.0, 90.0), (16.0, 150.0)];
const ENDS: f64 = 24.0;

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("tempo").join(name)
}

/// A VST3 factory over the system's plugins and the makers' own preset
/// folders, with caches of the test's own.
fn vst3() -> Vst3Factory {
    let config = Vst3Config {
        scanner: Scanner::Executable(PathBuf::from(env!("CARGO_BIN_EXE_trill-plugin-scan"))),
        cache_path: Some(scratch("vst3").join("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        preset_paths: vec![scratch("vst3 presets")],
        ..Vst3Config::default()
    };
    Vst3Factory::new(config).expect("the plugin main thread starts")
}

/// The frames at which a sound begins: where the level, taken a
/// millisecond at a time, comes to well over what it was in the ten
/// milliseconds before, and no other did in the forty before that.
fn onsets(left: &[f32], right: &[f32], rate: f64) -> Vec<usize> {
    let hop = (rate / 1000.0) as usize;
    let power: Vec<f64> = left
        .chunks(hop)
        .zip(right.chunks(hop))
        .map(|(l, r)| l.iter().zip(r).map(|(l, r)| f64::from(l * l + r * r)).sum::<f64>() / hop as f64)
        .collect();
    let loudest = power.iter().copied().fold(0.0, f64::max);
    let floor = loudest * 1e-4;
    let mut found: Vec<usize> = Vec::new();
    for at in 10..power.len().saturating_sub(3) {
        let before = power[at - 10..at].iter().sum::<f64>() / 10.0;
        let after = power[at..at + 3].iter().sum::<f64>() / 3.0;
        let rises = after > 4.0 * before + floor;
        if rises && found.last().is_none_or(|last| at * hop - last >= 40 * hop) {
            found.push(at * hop);
        }
    }
    found
}

/// The middle one of `values`.
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// How far `beat` is from the nearest step of a grid of `step` beats that
/// has a step at `origin`.
fn off_grid(beat: f64, origin: f64, step: f64) -> f64 {
    let along = (beat - origin).rem_euclid(step);
    along.min(step - along)
}

#[test]
#[ignore = "needs Serum 2 (VST3) and its presets; run with `cargo test -p trill-plugins --test tempo -- --include-ignored --nocapture --test-threads 1`"]
fn serum_2s_arpeggiator_plays_its_steps_on_the_songs_beats_through_changes_of_tempo() {
    let factory = vst3();
    if !factory.descriptors().iter().any(|descriptor| descriptor.uri == SERUM_2) {
        eprintln!("skipping: Serum 2 is not installed");
        return;
    }
    let list = factory.presets_within(SERUM_2, Duration::from_secs(120)).expect("a plugin of this factory");
    assert_eq!(list.status, PresetStatus::Ready);
    let presets: Vec<String> = match std::env::var("TRILL_TEMPO_PRESETS") {
        // For looking at others: names, separated by semicolons.
        Ok(names) => names.split(';').map(str::to_string).collect(),
        Err(_) => vec![ARPEGGIO.to_string()],
    };

    for name in &presets {
        let preset = list.presets.iter().find(|preset| preset.name == *name).unwrap_or_else(|| panic!("no '{name}'"));
        let devices = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(factory.clone())]);
        let mut session = Session::new(Arc::new(devices), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
        session
            .edit(|project| {
                project.set_tempo(BEGINS_AT)?;
                for (beat, tempo) in CHANGES {
                    project.set_tempo_at(beat, tempo, false)?;
                }
                let track = project.add_track("Arpeggio");
                let clip = project.add_midi_clip(track, "held", 0.0, ENDS)?;
                project.clip_mut(clip)?.add_notes(&[Note {
                    pitch: 48,
                    start: 0.0,
                    length: ENDS,
                    velocity: 110,
                    ..Default::default()
                }])?;
                Ok(track)
            })
            .and_then(|track| session.add_device(ChainRef::Track(track), SERUM_2, None, None))
            .and_then(|device| session.load_preset(device, &preset.id))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let map: TempoMap = session.project().tempo_map();

        let options = RenderOptions { end_beat: Some(ENDS), max_tail_seconds: 0.2, ..RenderOptions::default() };
        let out = session.render_job().render(&options).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(out.problems.is_empty(), "{name}: {:?}", out.problems);
        let rate = f64::from(out.sample_rate);
        let beats: Vec<f64> =
            onsets(&out.left, &out.right, rate).iter().map(|frame| map.beat_at_seconds(*frame as f64 / rate)).collect();

        // The steps the arpeggiator plays, from the song's first tempo:
        // how many beats lie between its notes, and where its grid lies.
        let first: Vec<f64> = beats.iter().copied().filter(|beat| *beat < CHANGES[0].0 - 0.5).collect();
        assert!(first.len() >= 8, "{name}: {} notes are heard to begin before the first change", first.len());
        let step = median(&mut first.windows(2).map(|pair| pair[1] - pair[0]).collect::<Vec<f64>>());
        let step = (step * 24.0).round() / 24.0;
        assert!(step > 0.0, "{name}: the notes are {step} beats apart");
        let origin = median(&mut first.iter().map(|beat| beat.rem_euclid(step)).collect::<Vec<f64>>());

        // Every tempo of the song: as many notes to the beat, on that grid.
        let mut from = 0.0;
        let mut tempo = BEGINS_AT;
        let mut held = true;
        for (until, next) in CHANGES.iter().copied().chain([(ENDS, 0.0)]) {
            let here: Vec<f64> = beats.iter().copied().filter(|beat| *beat >= from && *beat < until).collect();
            let apart = match here.len() {
                0 | 1 => f64::NAN,
                _ => median(&mut here.windows(2).map(|pair| pair[1] - pair[0]).collect::<Vec<f64>>()),
            };
            // A note is on the grid if it begins within 15 ms of a step:
            // the listening is good to a few milliseconds, and hears the
            // odd echo as a note.
            let near = |beat: &&f64| off_grid(**beat, origin, step) * 60.0 / tempo < 0.015;
            let on_grid = here.iter().filter(near).count();
            let wanted = (until - from) / step;
            eprintln!(
                "{name}: beats {from} to {until} at {tempo} BPM: {} notes heard for {wanted:.0} steps of {step:.4} \
                 beats, {on_grid} of them on the grid; {apart:.4} beats ({:.1} ms) apart",
                here.len(),
                apart * 60_000.0 / tempo
            );
            // Nine steps in ten are heard where the song has them, and
            // they are as far apart in beats as they were.
            held &= on_grid as f64 >= wanted * 0.9 && (apart - step).abs() < 0.01;
            (from, tempo) = (until, next);
        }
        if presets.len() == 1 {
            assert!(held, "{name}: the arpeggiator did not keep to the song's beats through its changes of tempo");
        }
    }
}
