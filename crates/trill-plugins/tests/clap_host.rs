//! End-to-end tests against real CLAP plugins.
//!
//! These use whatever is installed in the system's CLAP directories. A test
//! whose plugin is missing prints a note and passes, so the suite runs
//! anywhere; on a machine with the plugins it proves the host against code
//! this project did not write.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use trill_core::device::{
    CompositeFactory, DeviceCategory, DeviceFactory, DeviceInstance, Event, EventKind, ParamKind, ProcessInfo,
};
use trill_core::{ChainRef, Device, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, render};
use trill_engine::{AudioBackend, Session};
use trill_plugins::clap::{find_plugin_files, standard_search_paths};
use trill_plugins::scan::ScanOutput;
use trill_plugins::{ClapConfig, ClapFactory, ScanMode, Scanner};

const SURGE: &str = "clap:org.surge-synth-team.surge-xt";
const SURGE_FX: &str = "clap:org.surge-synth-team.surge-xt-fx";
const WAVESYNTH: &str = "clap:com.wavesynth.wavesynth";
const WAVEFORGE: &str = "clap:dev.waveforge.waveforge";

/// The instruments, each with a parameter that scales its output level and
/// a value for it that is clearly quieter than the default.
const INSTRUMENTS: [(&str, &str, f64); 3] =
    [(SURGE, "Global Volume", 0.0), (WAVESYNTH, "Volume", 0.05), (WAVEFORGE, "Output Gain", 0.05)];
const ALL: [&str; 4] = [SURGE, SURGE_FX, WAVESYNTH, WAVEFORGE];

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

// ---- helpers -------------------------------------------------------------

fn scanner_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trill-plugin-scan"))
}

fn scanner() -> Scanner {
    Scanner::Executable(scanner_path())
}

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("clap-host").join(name)
}

/// A factory over the system's plugins. Tests share one scan cache, so the
/// plugins are scanned once per build, not once per test.
fn factory() -> ClapFactory {
    let config = ClapConfig {
        scanner: scanner(),
        cache_path: Some(scratch("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        ..ClapConfig::default()
    };
    ClapFactory::new(config).expect("the plugin main thread starts")
}

/// Whether `uri` is installed here. Says so when a test is skipped for it.
fn installed(factory: &ClapFactory, uri: &str) -> bool {
    let found = factory.descriptors().iter().any(|descriptor| descriptor.uri == uri);
    if !found {
        eprintln!("skipping '{uri}': the plugin is not installed");
    }
    found
}

fn device(uri: &str) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, uri, "plugin", None).unwrap();
    project.device(id).unwrap().clone()
}

fn create(factory: &ClapFactory, device: &Device) -> DeviceInstance {
    factory.create(device, SR, BLOCK).unwrap_or_else(|error| panic!("{error}"))
}

fn param_index(instance: &DeviceInstance, name: &str) -> usize {
    instance.params.iter().position(|param| param.name == name).unwrap_or_else(|| panic!("no parameter named '{name}'"))
}

fn info(block: usize) -> ProcessInfo {
    ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: (block * BLOCK) as f64 / SR * 2.0,
        loop_range: None,
    }
}

/// Runs `blocks` blocks. `input` fills each block before processing and
/// `events` supplies the events of block `n`.
fn run(
    instance: &mut DeviceInstance,
    blocks: usize,
    input: impl Fn(usize) -> f32,
    events: impl Fn(usize) -> Vec<Event>,
) -> (Vec<f32>, Vec<f32>) {
    let (mut out_left, mut out_right) = (Vec::new(), Vec::new());
    for block in 0..blocks {
        let mut left: Vec<f32> = (0..BLOCK).map(|i| input(block * BLOCK + i)).collect();
        let mut right = left.clone();
        instance.processor.process(&info(block), &events(block), &mut left, &mut right);
        out_left.extend(left);
        out_right.extend(right);
    }
    (out_left, out_right)
}

fn note_on(pitch: u8) -> Vec<Event> {
    vec![Event { offset: 0, kind: EventKind::NoteOn { pitch, velocity: 0.8 } }]
}

/// Holds middle C from the first block on.
fn held_note(block: usize) -> Vec<Event> {
    if block == 0 { note_on(60) } else { Vec::new() }
}

fn silence(_: usize) -> f32 {
    0.0
}

fn noise(i: usize) -> f32 {
    let x = (i as u32).wrapping_mul(2_654_435_761).rotate_left(13).wrapping_mul(0x9E37_79B9);
    (x >> 8) as f32 / 8_388_608.0 - 1.0
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |max, sample| max.max(sample.abs()))
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

fn all_finite(samples: &[f32]) -> bool {
    samples.iter().all(|sample| sample.is_finite())
}

fn seconds(range: std::ops::Range<f64>) -> std::ops::Range<usize> {
    (range.start * SR) as usize..(range.end * SR) as usize
}

/// Fails a test that deadlocks instead of hanging the whole suite.
fn within(limit: Duration, body: impl FnOnce() + Send + 'static) {
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        body();
        let _ = done.send(());
    });
    match finished.recv_timeout(limit) {
        Ok(()) => worker.join().expect("the test body finished"),
        // The body panicked: report its panic, not a timeout.
        Err(RecvTimeoutError::Disconnected) => {
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(RecvTimeoutError::Timeout) => panic!("not finished after {limit:?}: deadlocked?"),
    }
}

/// Polls `condition` until it holds or `limit` passes.
fn eventually(limit: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

// ---- scanning ------------------------------------------------------------

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn the_scanner_process_finds_the_installed_plugins() {
    let files = find_plugin_files(&standard_search_paths());
    if files.is_empty() {
        eprintln!("skipping: no CLAP plugins are installed");
        return;
    }
    // Plugins this test knows, by file name: what each should be listed as.
    let known = [
        ("Surge XT.clap", SURGE, DeviceCategory::Instrument),
        ("Surge XT Effects.clap", SURGE_FX, DeviceCategory::Effect),
        ("WaveSynth.clap", WAVESYNTH, DeviceCategory::Instrument),
        ("WaveForge Development Shell.clap", WAVEFORGE, DeviceCategory::Instrument),
    ];
    let expected: Vec<_> = known
        .iter()
        .filter(|(file, ..)| files.iter().any(|path| path.file_name().is_some_and(|name| name == *file)))
        .collect();

    // The executable on its own: one line of JSON per plugin file.
    for (file, uri, _) in &expected {
        let path = files.iter().find(|path| path.file_name().is_some_and(|name| name == *file)).unwrap();
        let output = Command::new(scanner_path()).arg(path).output().expect("the scanner runs");
        assert!(output.status.success(), "{file}: {}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: ScanOutput =
            stdout.lines().rev().find_map(|line| serde_json::from_str(line).ok()).expect("a line of JSON");
        assert!(result.plugins.iter().any(|plugin| format!("clap:{}", plugin.id) == *uri), "{file}: {stdout}");
    }

    // The factory, scanning from scratch through that executable.
    let cache = scratch(&format!("scan-test-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&cache);
    let config = ClapConfig {
        scanner: scanner(),
        cache_path: Some(cache.clone()),
        scan_timeout: Duration::from_secs(60),
        ..ClapConfig::default()
    };
    let scanned = ClapFactory::new(config.clone()).unwrap();
    let descriptors = scanned.descriptors();
    assert!(scanned.scan_failures().is_empty(), "{:?}", scanned.scan_failures());
    for (file, uri, category) in &expected {
        let descriptor =
            descriptors.iter().find(|d| d.uri == *uri).unwrap_or_else(|| panic!("{file} was not found by the scan"));
        assert_eq!(descriptor.category, *category, "{file}");
        assert!(!descriptor.name.is_empty() && !descriptor.vendor.is_empty(), "{file}");
        assert!(descriptor.params.is_empty() && descriptor.presets.is_empty());
    }
    assert!(cache.is_file(), "the scan was cached");

    // A later start reads the cache. No plugin could be scanned in the
    // millisecond this factory allows, so finding them all proves it.
    let impatient = ClapConfig { scan_timeout: Duration::from_millis(1), ..config };
    let cached = ClapFactory::new(impatient.clone()).unwrap();
    assert_eq!(cached.descriptors(), descriptors);
    assert!(cached.scan_failures().is_empty());

    // Ignoring the cache with that timeout, every scan is cut short: each
    // file is reported as failed, and nothing else goes wrong.
    cached.scan(ScanMode::Fresh);
    assert!(cached.descriptors().is_empty());
    let failures = cached.scan_failures();
    assert_eq!(failures.len(), files.len());
    assert!(failures.iter().all(|failure| failure.reason.contains("timed out")), "{failures:?}");
    // The failures are remembered, until the next deliberate rescan.
    assert_eq!(ClapFactory::new(impatient).unwrap().scan_failures(), failures);
    scanned.scan(ScanMode::Fresh);
    assert_eq!(scanned.descriptors(), descriptors);
    let _ = std::fs::remove_file(&cache);
}

/// What the DAW falls back to when the scanner executable is missing.
#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn scanning_inside_the_process_finds_the_same_plugins() {
    let isolated = factory();
    let expected = isolated.descriptors();
    if expected.is_empty() {
        eprintln!("skipping: no CLAP plugins are installed");
        return;
    }
    let in_process = ClapFactory::new(ClapConfig {
        scanner: Scanner::InProcess,
        cache_path: None,
        scan_timeout: Duration::from_secs(120),
        ..ClapConfig::default()
    })
    .unwrap();
    assert_eq!(in_process.descriptors(), expected);
    assert!(in_process.scan_failures().is_empty(), "{:?}", in_process.scan_failures());

    // A plugin scanned this way while another instance of it is running
    // must leave that instance working.
    let Some(uri) = INSTRUMENTS.iter().map(|(uri, ..)| *uri).find(|uri| installed(&isolated, uri)) else { return };
    let mut instance = create(&isolated, &device(uri));
    in_process.scan(ScanMode::Fresh);
    assert_eq!(in_process.descriptors(), expected);
    let (left, _) = run(&mut instance, 20, silence, held_note);
    assert!(peak(&left) > 0.01, "{uri} stopped working after an in-process scan");
}

// ---- instantiating -------------------------------------------------------

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn every_plugin_reports_its_parameters() {
    let factory = factory();
    for uri in ALL {
        if !installed(&factory, uri) {
            continue;
        }
        let mut instance = create(&factory, &device(uri));
        assert!(!instance.params.is_empty(), "{uri}: no parameters");

        let mut ids: Vec<u32> =
            instance.params.iter().map(|param| param.id.parse().expect("ids are CLAP parameter ids")).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), instance.params.len(), "{uri}: duplicate parameter id");
        let mut names: Vec<&str> = instance.params.iter().map(|param| param.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), instance.params.len(), "{uri}: ambiguous parameter names");
        for param in &instance.params {
            assert!(!param.name.is_empty(), "{uri}: unnamed parameter {}", param.id);
            assert!(param.min <= param.max && (param.min..=param.max).contains(&param.default), "{uri}/{}", param.name);
            assert!(matches!(param.kind, ParamKind::Float | ParamKind::Int));
        }

        // The controller reads live values, in the same order. Nothing has
        // been set yet, so they are the defaults the engine was told.
        let values = instance.controller.as_mut().expect("plugins have a controller").param_values().unwrap();
        assert_eq!(values.len(), instance.params.len());
        for (param, value) in instance.params.iter().zip(&values) {
            assert!((value - param.default).abs() < 1e-6, "{uri}/{}: {value} vs {}", param.name, param.default);
        }
        assert_eq!(instance.processor.latency_samples(), 0, "{uri}: none of these plugins has latency");
    }
}

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn instruments_add_to_the_buffer_and_effects_replace_it() {
    let factory = factory();
    for (uri, ..) in INSTRUMENTS {
        if !installed(&factory, uri) {
            continue;
        }
        let mut instance = create(&factory, &device(uri));
        // Nothing playing: what is already in the buffer passes untouched.
        let (left, right) = run(&mut instance, 4, |_| 0.25, |_| Vec::new());
        assert!(left.iter().chain(&right).all(|sample| *sample == 0.25), "{uri} altered its input");

        // Playing: the same signal with the instrument's sound on top.
        let (left, right) = run(&mut instance, 20, |_| 0.25, held_note);
        assert!(all_finite(&left) && all_finite(&right), "{uri}");
        let voice: Vec<f32> = left.iter().map(|sample| sample - 0.25).collect();
        assert!(rms(&voice[BLOCK..]) > 0.01, "{uri} is silent");
        let mean = left[BLOCK..].iter().sum::<f32>() / (left.len() - BLOCK) as f32;
        assert!((mean - 0.25).abs() < 0.1, "{uri}: the input did not survive under the sound (mean {mean})");
    }

    if installed(&factory, SURGE_FX) {
        let mut instance = create(&factory, &device(SURGE_FX));
        let (left, right) = run(&mut instance, 20, |i| noise(i) * 0.5, |_| Vec::new());
        assert!(all_finite(&left) && all_finite(&right));
        assert!(rms(&left) > 0.01 && rms(&right) > 0.01, "the effect produced nothing");
        let changed = left.iter().enumerate().filter(|(i, sample)| (**sample - noise(*i) * 0.5).abs() > 1e-4).count();
        assert!(changed > left.len() / 2, "the effect passed its input through unprocessed");
    }
}

// ---- rendering -----------------------------------------------------------

/// A project with `uri` on a track, playing middle C from beat 1 to beat 2
/// (0.5 s to 1.0 s at 120 BPM), and the built-in limiter on the master.
fn song(uri: &str) -> (Project, trill_core::DeviceId) {
    let mut project = Project::new("t");
    let track = project.add_track("Lead");
    let device = project.add_device(ChainRef::Track(track), uri, "Plugin", None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 4.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[Note { pitch: 60, start: 1.0, length: 1.0, velocity: 100 }]).unwrap();
    project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();
    (project, device)
}

/// Built-in devices and plugins behind one factory, as the application
/// sets them up.
fn composite(plugins: &ClapFactory) -> CompositeFactory {
    CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(plugins.clone())])
}

fn render_options() -> RenderOptions {
    RenderOptions { sample_rate: SR as u32, max_tail_seconds: 4.0, ..RenderOptions::default() }
}

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn an_offline_render_plays_the_plugin_at_the_right_time() {
    let factory = factory();
    let devices = composite(&factory);
    for (uri, ..) in INSTRUMENTS {
        if !installed(&factory, uri) {
            continue;
        }
        let (project, _) = song(uri);
        let out = render(&project, &devices, &render_options()).unwrap();
        assert!(out.problems.is_empty(), "{uri}: {:?}", out.problems);
        assert!(all_finite(&out.left) && all_finite(&out.right), "{uri}: non-finite output");

        assert!(peak(&out.left[seconds(0.0..0.499)]) < 1e-5, "{uri}: sound before the note");
        assert!(rms(&out.left[seconds(0.55..0.95)]) > 0.01, "{uri}: silent during the note");
        assert!(rms(&out.right[seconds(0.55..0.95)]) > 0.01, "{uri}: right channel silent");
        let onset = out.left.iter().position(|sample| sample.abs() > 1e-3).unwrap();
        assert!((24_000..24_000 + 2_400).contains(&onset), "{uri}: the note starts at frame {onset}, not 24000");

        // The note ends at 1.0 s; whatever the release, 4 s later it is over.
        assert!(out.left.len() < seconds(0.0..5.0).end, "{uri}: the note never ends");
        let track_peak = out.track_peaks.first().map_or(0.0, |(_, peak)| *peak);
        assert!(track_peak > 0.01, "{uri}: the track meter saw nothing");
    }
}

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn changing_a_parameter_changes_the_sound() {
    let factory = factory();
    let devices = composite(&factory);
    for (uri, name, quiet) in INSTRUMENTS {
        if !installed(&factory, uri) {
            continue;
        }
        // Set in the model: the plugin starts out with the value.
        let (mut project, id) = song(uri);
        let normal = render(&project, &devices, &render_options()).unwrap();
        let param = {
            let probe = create(&factory, &device(uri));
            probe.params[param_index(&probe, name)].id.clone()
        };
        project.device_mut(id).unwrap().params.insert(param, quiet);
        let quieter = render(&project, &devices, &render_options()).unwrap();
        let during = seconds(0.55..0.95);
        let (loud, soft) = (rms(&normal.left[during.clone()]), rms(&quieter.left[during]));
        assert!(soft < loud * 0.5, "{uri}: '{name}' = {quiet} in the model changed {loud} to {soft}");

        // Set while running: the plugin follows, and reports the new value.
        let mut instance = create(&factory, &device(uri));
        let index = param_index(&instance, name);
        let (before, _) = run(&mut instance, 40, silence, held_note);
        instance.processor.set_param(index, quiet);
        instance.processor.set_param(instance.params.len() + 7, 1.0); // out of range: ignored
        let (after, _) = run(&mut instance, 40, silence, |_| Vec::new());
        let (loud, soft) = (rms(&before[20 * BLOCK..]), rms(&after[20 * BLOCK..]));
        assert!(soft < loud * 0.5, "{uri}: '{name}' = {quiet} while playing changed {loud} to {soft}");
        let values = instance.controller.as_mut().unwrap().param_values().unwrap();
        assert!((values[index] - quiet).abs() < 1e-3, "{uri}: the plugin reports '{name}' = {}", values[index]);
    }
}

// ---- state ---------------------------------------------------------------

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn saved_state_restores_into_a_new_instance() {
    let factory = factory();
    for uri in ALL {
        if !installed(&factory, uri) {
            continue;
        }
        let mut original = create(&factory, &device(uri));
        // Move a parameter away from its default so there is something to
        // lose: the level parameter for instruments, the first otherwise.
        let known = INSTRUMENTS.iter().find(|(instrument, ..)| *instrument == uri);
        let (index, value) = match known {
            Some((_, name, quiet)) => (param_index(&original, name), *quiet),
            None => (0, 0.5),
        };
        original.processor.set_param(index, value);
        run(&mut original, 4, silence, |_| Vec::new());

        let controller = original.controller.as_mut().unwrap();
        let state = controller.save_state().unwrap_or_else(|| panic!("{uri} saved no state"));
        assert!(!state.is_empty());
        let saved_values = controller.param_values().unwrap();
        if known.is_some() {
            assert!((saved_values[index] - value).abs() < 1e-3, "{uri}: the change did not reach the plugin");
        }

        let mut model = device(uri);
        model.state = Some(state);
        let mut restored = create(&factory, &model);
        let restored_values = restored.controller.as_mut().unwrap().param_values().unwrap();
        assert_eq!(restored_values.len(), saved_values.len());
        for ((param, saved), restored) in original.params.iter().zip(&saved_values).zip(&restored_values) {
            assert!((saved - restored).abs() < 1e-4, "{uri}/{}: saved {saved}, restored {restored}", param.name);
        }
        // The restored instance works, not just loads.
        let (left, _) = run(&mut restored, 8, |i| noise(i) * 0.25, held_note);
        assert!(all_finite(&left), "{uri}");

        // Model parameters are applied on top of the saved state.
        if let Some((_, name, _)) = known {
            let info = &original.params[index];
            let on_top = info.min + (info.max - info.min) * 0.75;
            model.params.insert(info.id.clone(), on_top);
            let mut both = create(&factory, &model);
            let values = both.controller.as_mut().unwrap().param_values().unwrap();
            assert!((values[index] - on_top).abs() < 1e-3, "{uri}: '{name}' is {} after state + params", values[index]);
        }
    }
}

// ---- voices --------------------------------------------------------------

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn all_notes_off_and_reset_end_sounding_notes() {
    let factory = factory();
    for (uri, ..) in INSTRUMENTS {
        if !installed(&factory, uri) {
            continue;
        }
        let mut instance = create(&factory, &device(uri));
        let chord = |block: usize| match block {
            0 => vec![
                Event { offset: 0, kind: EventKind::NoteOn { pitch: 60, velocity: 0.8 } },
                Event { offset: 100, kind: EventKind::NoteOn { pitch: 67, velocity: 0.8 } },
            ],
            _ => Vec::new(),
        };

        let (held, _) = run(&mut instance, 100, silence, chord);
        assert!(peak(&held[90 * BLOCK..]) > 0.01, "{uri}: held notes do not sustain");

        // The engine's flush: one event that must release both keys.
        let flush = |block: usize| match block {
            0 => vec![Event { offset: 0, kind: EventKind::AllNotesOff }],
            _ => Vec::new(),
        };
        let (released, _) = run(&mut instance, 400, silence, flush);
        assert!(peak(&released[390 * BLOCK..]) < 1e-4, "{uri}: notes still sounding 4 s after all-notes-off");

        // Reset: no event at all, the voices must end by themselves.
        let (held, _) = run(&mut instance, 100, silence, chord);
        assert!(peak(&held[90 * BLOCK..]) > 0.01, "{uri}: the plugin does not play after a flush");
        instance.processor.reset();
        let (after, _) = run(&mut instance, 400, silence, |_| Vec::new());
        assert!(peak(&after[390 * BLOCK..]) < 1e-4, "{uri}: notes still sounding 4 s after reset");
        let stopped_at = after.chunks(BLOCK).position(|block| peak(block) < 1e-4).unwrap();
        eprintln!("{uri}: silent {stopped_at} blocks after reset");

        // And it plays again afterwards.
        let (again, _) = run(&mut instance, 20, silence, held_note);
        assert!(peak(&again) > 0.01, "{uri}: silent after reset");
    }
}

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn a_plugin_can_be_prepared_again_for_another_sample_rate() {
    let factory = factory();
    for (uri, ..) in INSTRUMENTS {
        if !installed(&factory, uri) {
            continue;
        }
        let mut instance = create(&factory, &device(uri));
        run(&mut instance, 4, silence, held_note);

        instance.processor.prepare(44_100.0, 256);
        let transport = ProcessInfo {
            sample_rate: 44_100.0,
            tempo: 120.0,
            time_signature: Default::default(),
            playing: true,
            position_beats: 0.0,
            loop_range: None,
        };
        let (mut left, mut right) = (vec![0.0f32; 256], vec![0.0f32; 256]);
        let mut loudest = 0.0f32;
        for block in 0..40 {
            left.fill(0.0);
            right.fill(0.0);
            instance.processor.process(&transport, &held_note(block), &mut left, &mut right);
            assert!(all_finite(&left) && all_finite(&right), "{uri}");
            loudest = loudest.max(peak(&left));
        }
        assert!(loudest > 0.01, "{uri}: silent after being prepared for 44.1 kHz");

        // A block larger than promised is split, not passed on or dropped.
        let (mut left, mut right) = (vec![0.0f32; 1_000], vec![0.0f32; 1_000]);
        instance.processor.process(&transport, &[], &mut left, &mut right);
        assert!(all_finite(&left) && peak(&left[600..]) > 0.01, "{uri}: the tail of an oversized block is empty");

        // Preparing with the same settings is free and changes nothing.
        instance.processor.prepare(44_100.0, 256);
        instance.processor.process(&transport, &[], &mut left[..256], &mut right[..256]);
        assert!(peak(&left[..256]) > 0.01, "{uri}: a redundant prepare interrupted the sound");
    }
}

// ---- lifecycle -----------------------------------------------------------

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn many_instances_come_and_go() {
    within(Duration::from_secs(600), || {
        let factory = factory();
        for uri in ALL {
            if !installed(&factory, uri) {
                continue;
            }
            let model = device(uri);
            // Surge takes seconds to instantiate; the others are quick.
            let cycles = if uri == SURGE { 3 } else { 12 };
            for _ in 0..cycles {
                let mut instance = create(&factory, &model);
                run(&mut instance, 2, |i| noise(i) * 0.1, held_note);
            }

            // Several alive at once, created and dropped from different
            // threads while others are processing.
            let workers: Vec<_> = (0..3)
                .map(|worker| {
                    let (factory, model) = (factory.clone(), model.clone());
                    thread::spawn(move || {
                        for _ in 0..2 {
                            let mut instance = create(&factory, &model);
                            let (left, _) = run(&mut instance, 30, |i| noise(i + worker) * 0.1, held_note);
                            assert!(all_finite(&left));
                            let state = instance.controller.as_mut().unwrap().save_state();
                            assert!(state.is_some());
                            // Controller first, then processor, then the
                            // reverse: either order must be fine.
                            if worker % 2 == 0 {
                                drop(instance.controller.take());
                            } else {
                                let DeviceInstance { processor, mut controller, .. } = instance;
                                drop(processor);
                                let orphan = controller.as_mut().unwrap();
                                assert!(orphan.save_state().is_none(), "the plugin is gone with its processor");
                                assert!(orphan.param_values().is_none());
                            }
                        }
                    })
                })
                .collect();
            for worker in workers {
                worker.join().expect("a worker thread panicked");
            }
        }
    });
}

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn devices_outlive_the_factory_that_made_them() {
    within(Duration::from_secs(300), || {
        let factory = factory();
        let Some((uri, ..)) = INSTRUMENTS.iter().rev().find(|(uri, ..)| installed(&factory, uri)) else { return };
        let mut instance = create(&factory, &device(uri));
        // The session drops its factory while devices are still running in
        // the engine; the plugin must keep working and tear down cleanly.
        drop(factory);
        let (left, _) = run(&mut instance, 20, silence, held_note);
        assert!(peak(&left) > 0.01);
        assert!(instance.controller.as_mut().unwrap().save_state().is_some());
        drop(instance);

        // Unless another test holds a factory right now, the plugin main
        // thread has shut down and unloaded the plugin; this starts over.
        let again = self::factory();
        let mut instance = create(&again, &device(uri));
        let (left, _) = run(&mut instance, 20, silence, held_note);
        assert!(peak(&left) > 0.01);
    });
}

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn a_session_hosts_a_plugin_and_shuts_down_cleanly() {
    within(Duration::from_secs(300), || {
        let plugins = factory();
        let Some((uri, name, quiet)) = INSTRUMENTS.iter().find(|(uri, ..)| installed(&plugins, uri)).copied() else {
            return;
        };
        let mut session =
            Session::new(Arc::new(composite(&plugins)), AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
        assert!(session.descriptors().iter().any(|descriptor| descriptor.uri == uri));

        // A looping bar of eighth notes, so there is always something sounding.
        let track = session
            .edit(|project| {
                let track = project.add_track("Lead");
                let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
                let notes: Vec<Note> = (0..8)
                    .map(|i| Note { pitch: 55 + i as u8, start: i as f64 * 0.5, length: 0.45, velocity: 110 })
                    .collect();
                project.clip_mut(clip)?.add_notes(&notes)?;
                project.set_loop(true, 0.0, 4.0)?;
                Ok(track)
            })
            .unwrap();
        let meter = session.track_meter(track).unwrap();
        let sounding = |limit| eventually(limit, || meter.take().0 > 0.01);

        let device = session.add_device(ChainRef::Track(track), uri, None, None).unwrap();
        assert!(session.problems().is_empty(), "{:?}", session.problems());
        let params = session.device_params(device).unwrap();
        let level = params.iter().find(|param| param.info.name == name).expect("the level parameter");
        let (level, normal) = (level.info.id.clone(), level.value);
        assert!(normal > quiet, "{uri}: '{name}' starts at {normal}");

        session.play();
        assert!(sounding(Duration::from_secs(10)), "{uri} is silent in the session");
        assert_eq!(session.audio_faults(), 0);

        // An edit that reaches the running plugin as a parameter event.
        session.set_device_params(device, &[(level.clone(), quiet)]).unwrap();
        assert_eq!(session.project().device(device).unwrap().params[&level], quiet);

        // Removing the device silences the track; its state is kept for undo.
        session.edit(|project| project.remove_device(device).map(|_| ())).unwrap();
        thread::sleep(Duration::from_millis(100));
        session.housekeeping();
        meter.take();
        thread::sleep(Duration::from_millis(200));
        assert!(meter.take().0 < 1e-4, "the removed plugin is still audible");

        assert!(session.undo());
        let restored = session.project().device(device).expect("undo brings the device back");
        assert!(restored.state.as_ref().is_some_and(|state| !state.is_empty()), "no state was captured on removal");
        assert!(session.problems().is_empty(), "{:?}", session.problems());

        // The plugin came back as it was removed: quiet. Turn it up again.
        session.set_device_params(device, &[(level, normal)]).unwrap();
        assert!(sounding(Duration::from_secs(10)), "{uri} is silent after undo");

        // A project file round trip captures the plugin's state.
        let path = scratch(&format!("session-{}.json", std::process::id()));
        session.save(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(uri) && saved.contains("\"state\""), "the saved project lacks the plugin's state");
        session.load(&path).unwrap();
        assert!(session.problems().is_empty(), "{:?}", session.problems());
        let _ = std::fs::remove_file(&path);
        session.play();
        let meter = session.track_meter(session.project().tracks[0].id).unwrap();
        assert!(eventually(Duration::from_secs(10), || meter.take().0 > 0.01), "{uri} is silent after reloading");

        session.stop();
        session.housekeeping();
        assert_eq!(session.audio_faults(), 0);
        drop(session);
        drop(plugins);
    });
}
