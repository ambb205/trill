//! What an installed effect costs when it is fed nothing, and whether
//! telling it so lets it rest.
//!
//! A host says of every block whether a plugin's input holds nothing but
//! silence (a VST3 bus's silence flags, a CLAP channel that is constant),
//! and a plugin may take that as leave to do no work. Whether one does is
//! its own business. This measures it: an effect is run over the same
//! number of blocks of three inputs, and the time each block takes is
//! taken from the clock:
//!
//! - noise at -20 dB, which is what it is for;
//! - silence, which the host says is silence;
//! - a signal 240 dB under full scale, which nobody can hear and which is
//!   not silence, so that nothing is said of it.
//!
//! A plugin that rests on the word costs next to nothing on the second
//! and as much as ever on the third; one that looks at its input itself
//! may rest on both; one that does neither costs the same on all three.
//!
//! What it found on the machine it was written on (November 2026, blocks
//! of 480 frames at 48 kHz) is in `docs/plugins-and-presets.md`.
//!
//! ```text
//! cargo test -p trill-plugins --test silence -- --include-ignored --nocapture --test-threads 1
//! ```
//!
//! `TRILL_SILENCE_PLUGINS` (URIs, separated by semicolons) measures other
//! plugins. One instance of each is made, one after the other; nothing of
//! the user's is read or written, and no window opens.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use trill_core::device::{DeviceFactory, DeviceProcessor, ProcessInfo};
use trill_core::{ChainRef, Project};
use trill_plugins::{Scanner, Vst3Config, Vst3Factory};

/// The effects of the song this was first asked of: two amplifiers of
/// Neural DSP and a reverb of Valhalla DSP.
const EFFECTS: [&str; 3] = [
    "vst3:ABCDEF019182FAEB4E4453504E544858",
    "vst3:ABCDEF019182FAEB4E4453504E435758",
    "vst3:5653547665653376616C68616C6C6176",
];
const FRAMES: usize = 480;
/// Blocks run before any is timed, and blocks that are timed: two seconds
/// and five.
const WARM_UP: usize = 200;
const TIMED: usize = 500;

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("silence").join(name)
}

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

/// Noise that is the same every time: a block of it at -20 dB.
fn noise(seed: &mut u32, block: &mut [f32]) {
    for sample in block {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        *sample = ((*seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.1;
    }
}

/// How long a block takes on average, in microseconds, over [`TIMED`]
/// blocks of what `fill` puts in, and the loudest sample that came out.
fn cost(processor: &mut dyn DeviceProcessor, mut fill: impl FnMut(&mut [f32], &mut [f32])) -> (f64, f32) {
    let info = ProcessInfo {
        sample_rate: 48_000.0,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: false,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    }
    .steady();
    let (mut left, mut right) = (vec![0.0f32; FRAMES], vec![0.0f32; FRAMES]);
    let (mut spent, mut loudest) = (Duration::ZERO, 0.0f32);
    for block in 0..WARM_UP + TIMED {
        fill(&mut left, &mut right);
        let began = Instant::now();
        processor.process(&info, &[], &mut left, &mut right);
        if block >= WARM_UP {
            spent += began.elapsed();
            loudest = left.iter().chain(&right).fold(loudest, |most, sample| most.max(sample.abs()));
        }
    }
    (spent.as_secs_f64() * 1e6 / TIMED as f64, loudest)
}

#[test]
#[ignore = "needs installed VST3 effects (as it comes: Archetype Tim Henson X and Cory Wong X, ValhallaVintageVerb); \
            run with `cargo test -p trill-plugins --test silence -- --include-ignored --nocapture --test-threads 1`"]
fn what_an_effect_costs_on_silence_it_is_told_of_and_on_silence_it_is_not() {
    let factory = vst3();
    let asked = std::env::var("TRILL_SILENCE_PLUGINS").ok();
    let uris: Vec<String> = match &asked {
        Some(list) => list.split(';').map(str::trim).filter(|uri| !uri.is_empty()).map(str::to_string).collect(),
        None => EFFECTS.iter().map(|uri| uri.to_string()).collect(),
    };
    let known = factory.descriptors();
    let mut measured = 0;
    for uri in &uris {
        let Some(descriptor) = known.iter().find(|descriptor| descriptor.uri.eq_ignore_ascii_case(uri)) else {
            eprintln!("{uri}: not installed here");
            continue;
        };
        let mut project = Project::new("silence");
        let id = project.add_device(ChainRef::Master, &descriptor.uri, &descriptor.name, None).unwrap();
        let device = project.device(id).unwrap();
        let mut instance = match factory.create(device, 48_000.0, 1_024) {
            Ok(instance) => instance,
            Err(error) => {
                eprintln!("{}: could not be made: {error}", descriptor.name);
                continue;
            }
        };
        let processor = instance.processor.as_mut();
        let mut seed = 0x2545_F491u32;
        // An amplifier may pass nothing for a moment after it is made, and
        // costs nothing while it does: it is fed noise until something
        // comes out, for twenty seconds at the most, before anything is
        // timed.
        let woke = Instant::now();
        loop {
            let (_, heard) = cost(processor, |left, right| {
                noise(&mut seed, left);
                noise(&mut seed, right);
            });
            if heard > 0.0 || woke.elapsed() > Duration::from_secs(20) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let (on_noise, heard) = cost(processor, |left, right| {
            noise(&mut seed, left);
            noise(&mut seed, right);
        });
        let (told, after) = cost(processor, |left, right| {
            left.fill(0.0);
            right.fill(0.0);
        });
        let (untold, _) = cost(processor, |left, right| {
            left.fill(1e-12);
            right.fill(1e-12);
        });
        // And silence it is told of once more, in case the first time it
        // was still ringing out.
        let (told_again, at_rest) = cost(processor, |left, right| {
            left.fill(0.0);
            right.fill(0.0);
        });
        let block_us = FRAMES as f64 / 48_000.0 * 1e6;
        let share = |micros: f64| micros / block_us * 100.0;
        eprintln!(
            "{}: on noise {on_noise:.0} us a block ({:.1} % of a processor, peak {heard:.3}); on silence it is told of \
             {told:.0} us ({:.1} %, peak {after:.6}), and again {told_again:.0} us ({:.1} %, peak {at_rest:.6}); on \
             silence it is not told of {untold:.0} us ({:.1} %)",
            descriptor.name,
            share(on_noise),
            share(told),
            share(told_again),
            share(untold),
        );
        measured += 1;
        drop(instance);
    }
    assert!(measured > 0, "none of {uris:?} is installed here");
}
