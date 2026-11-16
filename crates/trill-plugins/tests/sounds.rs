//! What installed plugins tell a host about their sound: each parameter's
//! value in the plugin's own words, the channels they take and give, and
//! whether they have presets a host can list.
//!
//! These need the plugins named in them and are skipped unless asked for;
//! a test whose plugin is not installed prints a note and passes. They
//! print what they find, since what a plugin says is the point:
//!
//! ```text
//! cargo test -p trill-plugins --test sounds -- --include-ignored --nocapture --test-threads 1
//! ```
//!
//! No window is opened, and nothing of the user's is read or written: the
//! caches are the test's own, under the target folder.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use trill_core::device::{DeviceFactory, DeviceInstance, ProcessInfo};
use trill_core::presets::{PresetList, PresetStatus};
use trill_core::{ChainRef, Project, TimeSignature};
use trill_plugins::{Scanner, Vst3Config, Vst3Factory};

const TIM_HENSON: &str = "vst3:ABCDEF019182FAEB4E4453504E544858";
const SERUM_2: &str = "vst3:56534558667350736572756D20320000";
/// Instruments and an amplifier that keep their presets in their own
/// windows: three Ample guitars and basses, Superior Drummer 3, and the
/// amplifier above.
const WINDOWED: [(&str, &str); 5] = [
    ("Ample Guitar PF", "vst3:ABCDEF019182FAEB4170736441677066"),
    ("Ample Metal Hellrazer", "vst3:ABCDEF019182FAEB41707364416D6872"),
    ("Ample Metal R", "vst3:ABCDEF019182FAEB41707364416D7274"),
    ("Superior Drummer 3", "vst3:565354534433617375706572696F7220"),
    ("Archetype Tim Henson X", TIM_HENSON),
];

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const PATIENCE: Duration = Duration::from_secs(120);

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("sounds").join(name)
}

/// A VST3 factory over the system's plugins that looks for preset files
/// in a folder of the test's own: none of the user's are read.
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

fn installed(factory: &dyn DeviceFactory, uri: &str) -> bool {
    let found = factory.descriptors().iter().any(|descriptor| descriptor.uri == uri);
    if !found {
        eprintln!("skipping '{uri}': the plugin is not installed");
    }
    found
}

fn create(factory: &dyn DeviceFactory, uri: &str) -> DeviceInstance {
    let mut project = Project::new("sounds");
    let id = project.add_device(ChainRef::Master, uri, "plugin", None).unwrap();
    factory.create(project.device(id).unwrap(), SR, BLOCK).unwrap_or_else(|error| panic!("{uri}: {error}"))
}

/// Position of the parameter called `name`.
fn named(instance: &DeviceInstance, name: &str) -> usize {
    let found = instance.params.iter().position(|param| param.name.eq_ignore_ascii_case(name));
    found.unwrap_or_else(|| panic!("no parameter is called '{name}'"))
}

fn info() -> ProcessInfo {
    ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: TimeSignature::default(),
        playing: true,
        position_beats: 0.0,
        loop_range: None,
    }
}

/// Noise that is the same every time and different for every `seed`.
fn noise(seed: u32, index: usize) -> f32 {
    let x = (index as u32 ^ seed.wrapping_mul(0x9E37_79B9)).wrapping_mul(2_654_435_761).rotate_left(13);
    (x.wrapping_mul(0x85EB_CA6B) >> 8) as f32 / 8_388_608.0 - 1.0
}

/// Plays `seconds` of quiet noise through the plugin, different on each
/// side, and returns what comes out.
fn through(instance: &mut DeviceInstance, seconds: f64, from: &mut usize) -> (Vec<f32>, Vec<f32>) {
    let blocks = (seconds * SR / BLOCK as f64) as usize;
    let (mut out_left, mut out_right) = (Vec::new(), Vec::new());
    for _ in 0..blocks {
        let mut left: Vec<f32> = (0..BLOCK).map(|i| 0.1 * noise(1, *from + i)).collect();
        let mut right: Vec<f32> = (0..BLOCK).map(|i| 0.1 * noise(2, *from + i)).collect();
        *from += BLOCK;
        instance.processor.process(&info(), &[], &mut left, &mut right);
        out_left.extend(left);
        out_right.extend(right);
    }
    (out_left, out_right)
}

/// How alike the plugin's two outputs are when it is given different
/// noise on each side: 1 for the same on both sides, 0 for unrelated.
/// A plugin that has just been created may pass nothing for a moment
/// (an amplifier reads its cabinets first), so it is played until it
/// sounds, then measured over a second.
fn alikeness(instance: &mut DeviceInstance) -> f64 {
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| f64::from(*a) * f64::from(*b)).sum::<f64>();
    let mut at = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (left, right) = through(instance, 0.25, &mut at);
        if dot(&left, &left) + dot(&right, &right) > 1e-6 {
            break;
        }
        assert!(Instant::now() < deadline, "the plugin passed no sound in twenty seconds");
        std::thread::sleep(Duration::from_millis(100));
    }
    through(instance, 1.0, &mut at);
    let (left, right) = through(instance, 1.0, &mut at);
    dot(&left, &right) / (dot(&left, &left) * dot(&right, &right)).sqrt()
}

fn listed(factory: &dyn DeviceFactory, uri: &str) -> PresetList {
    let started = Instant::now();
    let list = factory.presets_within(uri, PATIENCE).expect("a plugin of this factory");
    eprintln!("{uri}: {} presets after {:.1} s ({:?})", list.len(), started.elapsed().as_secs_f64(), list.status);
    assert!(!list.is_reading(), "{uri}: its presets were not read within {PATIENCE:?}");
    list
}

#[test]
#[ignore = "needs Archetype Tim Henson X (VST3); run with `cargo test -p trill-plugins --test sounds -- --include-ignored --nocapture --test-threads 1`"]
fn an_amplifier_shows_its_values_in_its_own_words_and_takes_them_back() {
    let factory = vst3();
    if !installed(&factory, TIM_HENSON) {
        return;
    }
    let mut instance = create(&factory, TIM_HENSON);
    let (gain, mic) = (named(&instance, "Input Gain"), named(&instance, "Cherubs Amp Cab L Mic Type"));
    let (steps, count) = (instance.params[mic].max, instance.params.len());
    let controller = instance.controller.as_mut().unwrap();
    let shown = |controller: &mut Box<dyn trill_core::device::DeviceController>, index: usize, value: f64| {
        controller.param_texts(&[(index, value)]).remove(0).unwrap_or_else(|| panic!("no text for {value}"))
    };

    // A number in its unit, and every step of a choice by its name.
    let ends = [shown(controller, gain, 0.0), shown(controller, gain, 0.5), shown(controller, gain, 1.0)];
    eprintln!("Input Gain at 0, 0.5 and 1: {ends:?}");
    assert_eq!(ends, ["-24.0 dB", "0.0 dB", "+24.0 dB"]);
    let mics: Vec<String> = (0..=steps as usize).map(|step| shown(controller, mic, step as f64)).collect();
    eprintln!("Cab L Mic Type: {mics:?}");
    assert!(mics.iter().any(|name| name == "Dynamic 57") && mics.len() >= 3, "{mics:?}");

    // Set by what it shows: a number with its unit or without, and a
    // choice's name in any case.
    let quieter = controller.param_value(gain, "-6 dB").expect("-6 dB is a gain it has");
    assert_eq!(shown(controller, gain, quieter), "-6.0 dB");
    assert!((quieter - 0.375).abs() < 1e-3, "{quieter}");
    assert_eq!(
        controller.param_value(gain, "-6").map(|value| shown(controller, gain, value)).as_deref(),
        Some("-6.0 dB")
    );
    let dynamic = controller.param_value(mic, "dynamic 57").expect("a microphone it has");
    assert_eq!(shown(controller, mic, dynamic), "Dynamic 57");
    // What it does not show is not a value: asked for these, the plugin
    // itself answers with an end of the range.
    assert_eq!(controller.param_value(gain, "99 dB"), None);
    assert_eq!(controller.param_value(mic, "Dynamic 58"), None);

    // The patch it starts with is not all at the plugin's own defaults,
    // which is what tells the parameters that matter from the rest.
    let values = controller.param_values().expect("the plugin reports its values");
    let defaults = controller.param_defaults().expect("the plugin declares defaults");
    let moved = values.iter().zip(&defaults).filter(|(value, default)| (*value - *default).abs() > 1e-4).count();
    eprintln!("{count} parameters, {moved} not at the plugin's own default");
    assert!(count > 100 && moved > 0 && moved < count / 2, "{moved} of {count}");
}

#[test]
#[ignore = "needs Archetype Tim Henson X (VST3); run with `cargo test -p trill-plugins --test sounds -- --include-ignored --nocapture --test-threads 1`"]
fn an_amplifier_is_given_stereo_and_is_mono_inside_until_its_doubler_is_on() {
    let factory = vst3();
    if !installed(&factory, TIM_HENSON) {
        return;
    }
    let mut instance = create(&factory, TIM_HENSON);
    // The host hands it both sides, each on a channel of its own.
    let layout = instance.controller.as_ref().unwrap().channels().expect("the plugin has buses");
    eprintln!("channels: {}", layout.describe());
    assert_eq!((layout.inputs, layout.outputs, layout.folds_input()), (2, 2, false));

    // Different noise left and right comes out nearly the same on both
    // sides: the amplifier mixes its input, as an amplifier with one
    // input jack does.
    let narrow = alikeness(&mut instance);
    eprintln!("different noise on each side in: the two outputs are {narrow:.3} alike");
    assert!(narrow > 0.8, "{narrow}");

    // Its own doubler is what makes it wide.
    let doubler = named(&instance, "Doubler Active");
    let on = instance.controller.as_mut().unwrap().param_value(doubler, "On").expect("the doubler switches on");
    instance.processor.set_param(doubler, on);
    std::thread::sleep(Duration::from_millis(50));
    let wide = alikeness(&mut instance);
    eprintln!("with 'Doubler Active' on: {wide:.3} alike");
    assert!(wide < narrow - 0.3, "{narrow} without the doubler, {wide} with it");
}

#[test]
#[ignore = "needs the Ample guitars, Superior Drummer 3 and Archetype Tim Henson X; run with `cargo test -p trill-plugins --test sounds -- --include-ignored --nocapture --test-threads 1`"]
fn plugins_that_keep_their_presets_in_their_own_windows_list_none() {
    let factory = vst3();
    for (name, uri) in WINDOWED {
        if !installed(&factory, uri) {
            continue;
        }
        let list = listed(&factory, uri);
        assert_eq!((list.len(), &list.status), (0, &PresetStatus::Ready), "{name}");
        // And that is known from then on without asking the plugin again.
        assert_eq!(factory.known_presets(uri).map(|known| known.len()), Some(0), "{name}");
    }
}

#[test]
#[ignore = "needs Serum 2 (VST3); run with `cargo test -p trill-plugins --test sounds -- --include-ignored --nocapture --test-threads 1`"]
fn a_synth_whose_programs_are_numbered_slots_lists_no_presets() {
    let factory = vst3();
    if !installed(&factory, SERUM_2) {
        return;
    }
    // It tells a host of 128 programs called "Prog 1" to "Prog 128": the
    // MIDI program numbers it answers to, with nothing behind them until
    // the user maps presets to them in its window. Its real presets are
    // files of a format of its own.
    let list = listed(&factory, SERUM_2);
    assert_eq!((list.len(), &list.status), (0, &PresetStatus::Ready));
}
