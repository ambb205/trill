//! End-to-end tests against real VST3 plugins.
//!
//! These use whatever is installed in the system's VST3 directories. A test
//! whose plugin is missing prints a note and passes, so the suite runs
//! anywhere; on a machine with the plugins it proves the host against code
//! this project did not write.
//!
//! Every installed plugin is *scanned*, in scanner processes. Only the
//! plugins named here are ever *instantiated*: open-source ones, and the
//! project's own. Whatever else is installed may show a license dialog when
//! it is created, and a test must not.

use daw_core::device::{
    CompositeFactory, DeviceCategory, DeviceFactory, DeviceInstance, Event, EventKind, ParamKind, ProcessInfo,
};
use daw_core::{ChainRef, Device, Note, Project};
use daw_dsp::BuiltinFactory;
use daw_engine::render::{RenderOptions, render};
use daw_engine::{AudioBackend, Session};
use daw_plugins::scan::ScanOutput;
use daw_plugins::vst3::{find_plugin_files, standard_search_paths};
use daw_plugins::{ClapConfig, PluginHost, ScanMode, Scanner, Vst3Config, Vst3Factory};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const SURGE: &str = "Surge XT";
const SURGE_FX: &str = "Surge XT Effects";
const WAVESYNTH: &str = "WaveSynth";
const WAVEFORGE: &str = "WaveForge Development Shell";
const JS80P: &str = "JS80P";

/// The instruments, by name.
const INSTRUMENTS: [&str; 4] = [SURGE, WAVESYNTH, WAVEFORGE, JS80P];
/// Every plugin these tests instantiate.
const ALL: [&str; 5] = [SURGE, SURGE_FX, WAVESYNTH, WAVEFORGE, JS80P];
/// The instruments that have a parameter which scales their output level,
/// with that parameter's name and a value for it that is clearly quieter
/// than the default.
const LEVELS: [(&str, &str, f64); 3] =
    [(SURGE, "Global Volume", 0.0), (WAVESYNTH, "Volume", 0.05), (WAVEFORGE, "Output Gain", 0.05)];

/// The same plugins in the CLAP format, for the tests that host both.
const SURGE_CLAP: &str = "clap:org.surge-synth-team.surge-xt";
const WAVESYNTH_CLAP: &str = "clap:com.wavesynth.wavesynth";

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
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("vst3-host").join(name)
}

fn config() -> Vst3Config {
    Vst3Config {
        scanner: scanner(),
        cache_path: Some(scratch("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        ..Vst3Config::default()
    }
}

/// A factory over the system's plugins. Tests share one scan cache, so the
/// plugins are scanned once per build, not once per test.
fn factory() -> Vst3Factory {
    Vst3Factory::new(config()).expect("the plugin main thread starts")
}

/// The URI of the installed plugin called `name`. Says so when a test is
/// skipped because there is none.
fn installed(factory: &Vst3Factory, name: &str) -> Option<String> {
    let found = factory.descriptors().into_iter().find(|descriptor| descriptor.name == name);
    if found.is_none() {
        eprintln!("skipping '{name}': the plugin is not installed");
    }
    found.map(|descriptor| descriptor.uri)
}

fn device(uri: &str) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, uri, "plugin", None).unwrap();
    project.device(id).unwrap().clone()
}

fn create(factory: &Vst3Factory, device: &Device) -> DeviceInstance {
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
fn the_scanner_process_survives_and_lists_every_installed_plugin() {
    let files = find_plugin_files(&standard_search_paths());
    if files.is_empty() {
        eprintln!("skipping: no VST3 plugins are installed");
        return;
    }
    let file_of = |name: &str| files.iter().find(|path| path.file_name().is_some_and(|file| file == name));
    // Plugins this test knows, by file name: what each should be listed as.
    let known = [
        ("Surge XT.vst3", SURGE, DeviceCategory::Instrument),
        ("Surge XT Effects.vst3", SURGE_FX, DeviceCategory::Effect),
        ("WaveSynth.vst3", WAVESYNTH, DeviceCategory::Instrument),
        ("WaveForge Development Shell.vst3", WAVEFORGE, DeviceCategory::Instrument),
        ("js80p.vst3", JS80P, DeviceCategory::Instrument),
    ];
    let expected: Vec<_> = known.iter().filter(|(file, ..)| file_of(file).is_some()).collect();

    // The executable on its own: one line of JSON per plugin file. A
    // bundle may be named by its directory as well as by its library.
    for (file, name, _) in &expected {
        let library = file_of(file).unwrap();
        let bundle = library.ancestors().nth(3).filter(|bundle| bundle.extension().is_some_and(|e| e == "vst3"));
        for path in [Some(library.as_path()), bundle].into_iter().flatten() {
            let output = Command::new(scanner_path()).arg(path).output().expect("the scanner runs");
            assert!(output.status.success(), "{file}: {}", String::from_utf8_lossy(&output.stderr));
            let stdout = String::from_utf8_lossy(&output.stdout);
            let result: ScanOutput =
                stdout.lines().rev().find_map(|line| serde_json::from_str(line).ok()).expect("a line of JSON");
            assert!(result.plugins.iter().any(|plugin| plugin.name == *name), "{file}: {stdout}");
        }
    }

    // The factory, scanning everything from scratch through that
    // executable. Whatever is installed here, and whatever it does when it
    // is loaded, the scan comes back with every file accounted for.
    let cache = scratch(&format!("scan-test-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&cache);
    let config = Vst3Config { cache_path: Some(cache.clone()), ..config() };
    let scanned = Vst3Factory::new(config.clone()).unwrap();
    let descriptors = scanned.descriptors();
    let failures = scanned.scan_failures();
    for failure in &failures {
        eprintln!("could not scan {}: {}", failure.path.display(), failure.reason);
        assert!(files.contains(&failure.path) && !failure.reason.is_empty());
    }
    eprintln!("{} plugins in {} files, {} of which failed to scan", descriptors.len(), files.len(), failures.len());
    for (file, name, category) in &expected {
        assert!(!failures.iter().any(|failure| failure.path == *file_of(file).unwrap()), "{file} failed to scan");
        let descriptor = descriptors
            .iter()
            .find(|descriptor| descriptor.name == *name)
            .unwrap_or_else(|| panic!("{file} was not found by the scan"));
        assert_eq!(descriptor.category, *category, "{file}");
        assert!(!descriptor.vendor.is_empty(), "{file}");
        assert!(descriptor.params.is_empty() && descriptor.presets.is_empty());
    }
    for descriptor in &descriptors {
        let class = descriptor.uri.strip_prefix("vst3:").unwrap_or_else(|| panic!("{}", descriptor.uri));
        let hex = class.len() == 32 && class.chars().all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c));
        assert!(hex, "{} is not 32 upper-case hexadecimal digits", descriptor.uri);
        assert!(!descriptor.name.is_empty());
    }
    assert!(cache.is_file(), "the scan was cached");

    // A later start reads the cache. No plugin could be scanned in the
    // millisecond this factory allows, so finding them all proves it.
    let impatient = Vst3Config { scan_timeout: Duration::from_millis(1), ..config };
    let cached = Vst3Factory::new(impatient.clone()).unwrap();
    assert_eq!(cached.descriptors(), descriptors);
    assert_eq!(cached.scan_failures(), failures);

    // Ignoring the cache with that timeout, every scan is cut short: each
    // file is reported as failed, and nothing else goes wrong.
    cached.scan(ScanMode::Fresh);
    assert!(cached.descriptors().is_empty());
    let cut_short = cached.scan_failures();
    assert_eq!(cut_short.len(), files.len());
    assert!(cut_short.iter().all(|failure| failure.reason.contains("timed out")), "{cut_short:?}");
    // The failures are remembered, until the next deliberate rescan.
    assert_eq!(Vst3Factory::new(impatient).unwrap().scan_failures(), cut_short);
    scanned.scan(ScanMode::Fresh);
    assert_eq!(scanned.descriptors(), descriptors);
    let _ = std::fs::remove_file(&cache);
}

/// The class id in a device's URI is written the way the VST3 SDK writes
/// it. Plugins built with a recent SDK ship a `moduleinfo.json` that lists
/// their classes in that form, which is independent evidence.
#[test]
fn class_ids_are_written_as_the_plugins_own_module_info_lists_them() {
    let factory = factory();
    let uris: Vec<String> = factory.descriptors().into_iter().map(|descriptor| descriptor.uri).collect();
    let mut checked = 0;
    for library in find_plugin_files(&standard_search_paths()) {
        let Some(bundle) = library.ancestors().nth(3) else { continue };
        let Ok(module_info) =
            std::fs::read_to_string(bundle.join("Contents").join("Resources").join("moduleinfo.json"))
        else {
            continue;
        };
        if factory.scan_failures().iter().any(|failure| failure.path == library) {
            continue;
        }
        // The file is JSON5, and all that is wanted are the class ids.
        let listed: Vec<&str> = module_info
            .split("\"CID\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').nth(1))
            .filter(|class| class.len() == 32)
            .collect();
        assert!(!listed.is_empty(), "{}: no class ids in its module info", bundle.display());
        assert!(
            listed.iter().any(|class| uris.contains(&format!("vst3:{class}"))),
            "{}: none of its classes {listed:?} is a device",
            bundle.display()
        );
        checked += 1;
    }
    eprintln!("checked against {checked} module info files");
}

/// What the DAW falls back to when the scanner executable is missing.
#[test]
fn scanning_inside_the_process_finds_the_same_plugins() {
    // Scanning in this process loads the plugins into it, so only the
    // directories of the plugins named above are searched, and only if
    // nothing else lives there.
    let directories: Vec<PathBuf> = find_plugin_files(&standard_search_paths())
        .iter()
        .filter(|library| {
            library.file_stem().is_some_and(|stem| ALL.iter().any(|name| stem == *name) || stem == "js80p")
        })
        .filter_map(|library| match library.ancestors().nth(3) {
            Some(bundle) if bundle.extension().is_some_and(|extension| extension == "vst3") => bundle.parent(),
            _ => library.parent(),
        })
        .map(Path::to_path_buf)
        .collect();
    let libraries = find_plugin_files(&directories);
    let unknown: Vec<&PathBuf> = libraries
        .iter()
        .filter(|library| {
            !library.file_stem().is_some_and(|stem| ALL.iter().any(|name| stem == *name) || stem == "js80p")
        })
        .collect();
    if libraries.is_empty() || !unknown.is_empty() {
        eprintln!("skipping: no directory holds only the test plugins ({unknown:?})");
        return;
    }

    let isolated = Vst3Factory::new(Vst3Config {
        search_paths: directories.clone(),
        cache_path: Some(scratch("in-process-reference.json")),
        ..config()
    })
    .unwrap();
    let expected = isolated.descriptors();
    assert_eq!(expected.len(), libraries.len());
    let in_process = Vst3Factory::new(Vst3Config {
        search_paths: directories,
        scanner: Scanner::InProcess,
        cache_path: None,
        scan_timeout: Duration::from_secs(120),
    })
    .unwrap();
    assert_eq!(in_process.descriptors(), expected);
    assert!(in_process.scan_failures().is_empty(), "{:?}", in_process.scan_failures());

    // A plugin scanned this way while another instance of it is running
    // must leave that instance working.
    let Some(uri) = INSTRUMENTS.iter().find_map(|name| installed(&isolated, name)) else { return };
    let mut instance = create(&isolated, &device(&uri));
    in_process.scan(ScanMode::Fresh);
    assert_eq!(in_process.descriptors(), expected);
    let (left, _) = run(&mut instance, 20, silence, held_note);
    assert!(peak(&left) > 0.01, "{uri} stopped working after an in-process scan");
}

#[test]
fn the_plugin_host_offers_both_formats_from_one_scan_setup() {
    let clap =
        ClapConfig { scanner: scanner(), cache_path: Some(scratch("host-clap-cache.json")), ..ClapConfig::default() };
    let host =
        PluginHost::with_configs(clap, Vst3Config { cache_path: Some(scratch("host-vst3-cache.json")), ..config() })
            .unwrap();
    host.scan(ScanMode::UseCache);
    let (clap, vst3) = (host.clap().descriptors(), host.vst3().descriptors());
    assert!(clap.iter().all(|descriptor| descriptor.uri.starts_with("clap:")));
    assert!(vst3.iter().all(|descriptor| descriptor.uri.starts_with("vst3:")));
    assert_eq!(host.scan_failures().len(), host.clap().scan_failures().len() + host.vst3().scan_failures().len());

    // The factories it hands out are the same two, sharing its scans.
    let factories = host.factories();
    assert_eq!(factories.len(), 2);
    let devices = CompositeFactory::new(factories);
    assert_eq!(devices.descriptors().len(), clap.len() + vst3.len());
    for name in [SURGE, WAVESYNTH] {
        if let Some(uri) = installed(host.vst3(), name) {
            // Both formats of a plugin are listed side by side, under
            // different URIs.
            let same_name = devices.descriptors().into_iter().filter(|descriptor| descriptor.name == name);
            let uris: Vec<String> = same_name.map(|descriptor| descriptor.uri).collect();
            assert!(uris.contains(&uri), "{name}: {uris:?}");
            let instance = devices.create(&device(&uri), SR, BLOCK).unwrap_or_else(|error| panic!("{error}"));
            assert!(!instance.params.is_empty());
        }
    }
}

// ---- instantiating -------------------------------------------------------

#[test]
fn every_plugin_reports_its_parameters() {
    let factory = factory();
    for name in ALL {
        let Some(uri) = installed(&factory, name) else { continue };
        let mut instance = create(&factory, &device(&uri));
        assert!(!instance.params.is_empty(), "{name}: no parameters");

        let mut ids: Vec<u32> =
            instance.params.iter().map(|param| param.id.parse().expect("ids are VST3 parameter ids")).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), instance.params.len(), "{name}: duplicate parameter id");
        let mut names: Vec<&str> = instance.params.iter().map(|param| param.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), instance.params.len(), "{name}: ambiguous parameter names");
        for param in &instance.params {
            assert!(!param.name.is_empty(), "{name}: unnamed parameter {}", param.id);
            assert_eq!(param.min, 0.0, "{name}/{}", param.name);
            assert!((param.min..=param.max).contains(&param.default), "{name}/{}", param.name);
            // Every value the engine may set is one the plugin can be
            // given without asking it anything.
            match &param.kind {
                ParamKind::Float | ParamKind::Bool => assert_eq!(param.max, 1.0, "{name}/{}", param.name),
                ParamKind::Int => assert!(param.max > 64.0 && param.max.fract() == 0.0, "{name}/{}", param.name),
                ParamKind::Enum(choices) => {
                    assert_eq!(param.max, (choices.len() - 1) as f64, "{name}/{}", param.name);
                    assert!((3..=65).contains(&choices.len()), "{name}/{}", param.name);
                    assert!(choices.iter().all(|choice| !choice.is_empty()), "{name}/{}", param.name);
                }
            }
            if param.kind != ParamKind::Float {
                assert_eq!(param.default.fract(), 0.0, "{name}/{}", param.name);
            }
        }

        // The controller reads live values, in the same order. Nothing has
        // been set yet, so they are the defaults the engine was told.
        let values = instance.controller.as_mut().expect("plugins have a controller").param_values().unwrap();
        assert_eq!(values.len(), instance.params.len());
        for (param, value) in instance.params.iter().zip(&values) {
            assert!((value - param.default).abs() < 1e-6, "{name}/{}: {value} vs {}", param.name, param.default);
        }
        eprintln!("{name}: {} parameters, latency {}", instance.params.len(), instance.processor.latency_samples());
    }
}

#[test]
fn instruments_add_to_the_buffer_and_effects_replace_it() {
    let factory = factory();
    for name in INSTRUMENTS {
        let Some(uri) = installed(&factory, name) else { continue };
        let mut instance = create(&factory, &device(&uri));
        // Nothing playing: what is already in the buffer passes through.
        let (left, right) = run(&mut instance, 4, |_| 0.25, |_| Vec::new());
        assert!(left.iter().chain(&right).all(|sample| (sample - 0.25).abs() < 1e-6), "{name} altered its input");

        // Playing: the same signal with the instrument's sound on top.
        let (left, right) = run(&mut instance, 20, |_| 0.25, held_note);
        assert!(all_finite(&left) && all_finite(&right), "{name}");
        let voice: Vec<f32> = left.iter().map(|sample| sample - 0.25).collect();
        assert!(rms(&voice[BLOCK..]) > 0.01, "{name} is silent");
        let mean = left[BLOCK..].iter().sum::<f32>() / (left.len() - BLOCK) as f32;
        assert!((mean - 0.25).abs() < 0.1, "{name}: the input did not survive under the sound (mean {mean})");
    }

    if let Some(uri) = installed(&factory, SURGE_FX) {
        let mut instance = create(&factory, &device(&uri));
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
fn song(uri: &str) -> (Project, daw_core::DeviceId) {
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
fn composite(plugins: &Vst3Factory) -> CompositeFactory {
    CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(plugins.clone())])
}

fn render_options() -> RenderOptions {
    RenderOptions { sample_rate: SR as u32, max_tail_seconds: 4.0, ..RenderOptions::default() }
}

#[test]
fn an_offline_render_plays_the_plugin_at_the_right_time() {
    let factory = factory();
    let devices = composite(&factory);
    for name in INSTRUMENTS {
        let Some(uri) = installed(&factory, name) else { continue };
        let (project, _) = song(&uri);
        let out = render(&project, &devices, &render_options()).unwrap();
        assert!(out.problems.is_empty(), "{name}: {:?}", out.problems);
        assert!(all_finite(&out.left) && all_finite(&out.right), "{name}: non-finite output");

        assert!(peak(&out.left[seconds(0.0..0.499)]) < 1e-5, "{name}: sound before the note");
        assert!(rms(&out.left[seconds(0.55..0.95)]) > 0.01, "{name}: silent during the note");
        assert!(rms(&out.right[seconds(0.55..0.95)]) > 0.01, "{name}: right channel silent");
        let onset = out.left.iter().position(|sample| sample.abs() > 1e-3).unwrap();
        assert!((24_000..24_000 + 2_400).contains(&onset), "{name}: the note starts at frame {onset}, not 24000");

        // The note ends at 1.0 s; whatever the release, 4 s later it is over.
        assert!(out.left.len() < seconds(0.0..5.0).end, "{name}: the note never ends");
        let track_peak = out.track_peaks.first().map_or(0.0, |(_, peak)| *peak);
        assert!(track_peak > 0.01, "{name}: the track meter saw nothing");
        eprintln!("{name}: onset at frame {onset}, {} frames rendered", out.left.len());
    }
}

#[test]
fn changing_a_parameter_changes_the_sound() {
    let factory = factory();
    let devices = composite(&factory);
    for (name, level, quiet) in LEVELS {
        let Some(uri) = installed(&factory, name) else { continue };
        // Set in the model: the plugin starts out with the value.
        let (mut project, id) = song(&uri);
        let normal = render(&project, &devices, &render_options()).unwrap();
        let param = {
            let probe = create(&factory, &device(&uri));
            probe.params[param_index(&probe, level)].id.clone()
        };
        project.device_mut(id).unwrap().params.insert(param, quiet);
        let quieter = render(&project, &devices, &render_options()).unwrap();
        let during = seconds(0.55..0.95);
        let (loud, soft) = (rms(&normal.left[during.clone()]), rms(&quieter.left[during]));
        assert!(soft < loud * 0.5, "{name}: '{level}' = {quiet} in the model changed {loud} to {soft}");
        // The value was in place from the first frame, not glided to.
        let onset = seconds(0.5..0.55);
        assert!(
            peak(&quieter.left[onset.clone()]) < peak(&normal.left[onset]) * 0.5,
            "{name}: the note starts at the default level"
        );

        // Set while running: the plugin follows, and reports the new value.
        let mut instance = create(&factory, &device(&uri));
        let index = param_index(&instance, level);
        let (before, _) = run(&mut instance, 40, silence, held_note);
        instance.processor.set_param(index, quiet);
        instance.processor.set_param(instance.params.len() + 7, 1.0); // out of range: ignored
        let (after, _) = run(&mut instance, 40, silence, |_| Vec::new());
        let (loud, soft) = (rms(&before[20 * BLOCK..]), rms(&after[20 * BLOCK..]));
        assert!(soft < loud * 0.5, "{name}: '{level}' = {quiet} while playing changed {loud} to {soft}");
        let values = instance.controller.as_mut().unwrap().param_values().unwrap();
        assert!((values[index] - quiet).abs() < 1e-3, "{name}: the plugin reports '{level}' = {}", values[index]);
    }
}

// ---- state ---------------------------------------------------------------

#[test]
fn saved_state_restores_into_a_new_instance() {
    let factory = factory();
    for name in ALL {
        let Some(uri) = installed(&factory, name) else { continue };
        let mut original = create(&factory, &device(&uri));
        // Move a parameter away from its default so there is something to
        // lose: the level parameter where there is one, the first
        // otherwise.
        let known = LEVELS.iter().find(|(plugin, ..)| *plugin == name);
        let (index, value) = match known {
            Some((_, level, quiet)) => (param_index(&original, level), *quiet),
            None => (0, if original.params[0].default < 0.5 { 0.75 } else { 0.25 }),
        };
        original.processor.set_param(index, value);
        run(&mut original, 4, silence, |_| Vec::new());

        let controller = original.controller.as_mut().unwrap();
        let state = controller.save_state().unwrap_or_else(|| panic!("{name} saved no state"));
        assert!(state.starts_with(b"VST3"), "{name}: the state is a VST3 preset");
        let class = uri.strip_prefix("vst3:").unwrap();
        assert_eq!(&state[8..40], class.as_bytes(), "{name}: the state names the plugin it belongs to");
        let saved_values = controller.param_values().unwrap();
        assert!((saved_values[index] - value).abs() < 1e-3, "{name}: the change did not reach the plugin");

        let mut model = device(&uri);
        model.state = Some(state.clone());
        let mut restored = create(&factory, &model);
        let restored_values = restored.controller.as_mut().unwrap().param_values().unwrap();
        assert_eq!(restored_values.len(), saved_values.len());
        // Whether a plugin keeps a given parameter in its state is its own
        // business (the position of a pitch wheel is not part of a patch).
        // What it does keep must come back.
        let kept = (restored_values[index] - value).abs() < 1e-3;
        if known.is_some() {
            assert!(kept, "{name}: '{}' came back as {}", original.params[index].name, restored_values[index]);
        }
        for ((param, saved), restored) in original.params.iter().zip(&saved_values).zip(&restored_values) {
            let moved = param.id == original.params[index].id;
            assert!(
                (saved - restored).abs() < 1e-4 || (moved && !kept),
                "{name}/{}: saved {saved}, restored {restored}",
                param.name
            );
        }
        // The restored state is the new baseline the engine is told.
        for (param, value) in restored.params.iter().zip(&restored_values) {
            assert!((param.default - value).abs() < 1e-6, "{name}/{}", param.name);
        }
        // The restored instance works, not just loads, and saves what it
        // was given.
        let (left, _) = run(&mut restored, 8, |i| noise(i) * 0.25, held_note);
        assert!(all_finite(&left), "{name}");
        let again = restored.controller.as_mut().unwrap().save_state().unwrap();
        eprintln!("{name}: {} bytes of state, {} when saved again; moved value kept: {kept}", state.len(), again.len());

        // Model parameters are applied on top of the saved state.
        if let Some((_, level, _)) = known {
            let info = &original.params[index];
            let on_top = info.min + (info.max - info.min) * 0.75;
            model.params.insert(info.id.clone(), on_top);
            let mut both = create(&factory, &model);
            let values = both.controller.as_mut().unwrap().param_values().unwrap();
            assert!(
                (values[index] - on_top).abs() < 1e-3,
                "{name}: '{level}' is {} after state + params",
                values[index]
            );
            assert!((both.params[index].default - value).abs() < 1e-3, "{name}: the baseline is the state's value");
        }

        // A state of some other plugin, or no state at all, is refused
        // with a reason instead of being fed to the plugin.
        let mut wrong = device(&uri);
        wrong.state = Some(b"saved by some other plugin, in some other format".to_vec());
        let error = factory.create(&wrong, SR, BLOCK).err().expect("a foreign state is refused").to_string();
        assert!(error.contains("not a VST3 state"), "{name}: {error}");
        let mut foreign = state.clone();
        foreign[8..40].copy_from_slice(b"0123456789ABCDEF0123456789ABCDEF");
        wrong.state = Some(foreign);
        let error = factory.create(&wrong, SR, BLOCK).err().expect("another plugin's state is refused").to_string();
        assert!(error.contains("belongs to another plugin"), "{name}: {error}");
    }
}

// ---- voices --------------------------------------------------------------

#[test]
fn all_notes_off_and_reset_end_sounding_notes() {
    let factory = factory();
    for name in INSTRUMENTS {
        let Some(uri) = installed(&factory, name) else { continue };
        let mut instance = create(&factory, &device(&uri));
        let chord = |block: usize| match block {
            0 => vec![
                Event { offset: 0, kind: EventKind::NoteOn { pitch: 60, velocity: 0.8 } },
                Event { offset: 100, kind: EventKind::NoteOn { pitch: 67, velocity: 0.8 } },
            ],
            _ => Vec::new(),
        };

        let (held, _) = run(&mut instance, 100, silence, chord);
        assert!(peak(&held[90 * BLOCK..]) > 0.01, "{name}: held notes do not sustain");

        // The engine's flush: one event that must release both keys.
        let flush = |block: usize| match block {
            0 => vec![Event { offset: 0, kind: EventKind::AllNotesOff }],
            _ => Vec::new(),
        };
        let (released, _) = run(&mut instance, 400, silence, flush);
        assert!(peak(&released[390 * BLOCK..]) < 1e-4, "{name}: notes still sounding 4 s after all-notes-off");

        // Reset: no event at all, the voices must end by themselves.
        let (held, _) = run(&mut instance, 100, silence, chord);
        assert!(peak(&held[90 * BLOCK..]) > 0.01, "{name}: the plugin does not play after a flush");
        instance.processor.reset();
        let (after, _) = run(&mut instance, 400, silence, |_| Vec::new());
        assert!(peak(&after[390 * BLOCK..]) < 1e-4, "{name}: notes still sounding 4 s after reset");
        let stopped_at = after.chunks(BLOCK).position(|block| peak(block) < 1e-4).unwrap();
        eprintln!("{name}: silent {stopped_at} blocks after reset");

        // And it plays again afterwards.
        let (again, _) = run(&mut instance, 20, silence, held_note);
        assert!(peak(&again) > 0.01, "{name}: silent after reset");
    }
}

#[test]
fn a_plugin_can_be_prepared_again_for_another_sample_rate() {
    let factory = factory();
    for name in INSTRUMENTS {
        let Some(uri) = installed(&factory, name) else { continue };
        let mut instance = create(&factory, &device(&uri));
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
            assert!(all_finite(&left) && all_finite(&right), "{name}");
            loudest = loudest.max(peak(&left));
        }
        assert!(loudest > 0.01, "{name}: silent after being prepared for 44.1 kHz");

        // A block larger than promised is split, not passed on or dropped.
        let (mut left, mut right) = (vec![0.0f32; 1_000], vec![0.0f32; 1_000]);
        instance.processor.process(&transport, &[], &mut left, &mut right);
        assert!(all_finite(&left) && peak(&left[600..]) > 0.01, "{name}: the tail of an oversized block is empty");

        // Preparing with the same settings is free and changes nothing.
        instance.processor.prepare(44_100.0, 256);
        instance.processor.process(&transport, &[], &mut left[..256], &mut right[..256]);
        assert!(peak(&left[..256]) > 0.01, "{name}: a redundant prepare interrupted the sound");
    }
}

// ---- lifecycle -----------------------------------------------------------

#[test]
fn many_instances_come_and_go() {
    within(Duration::from_secs(600), || {
        let factory = factory();
        for name in ALL {
            let Some(uri) = installed(&factory, name) else { continue };
            let model = device(&uri);
            // Surge takes a while to instantiate; the others are quick.
            let cycles = if name == SURGE { 3 } else { 12 };
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
fn devices_outlive_the_factory_that_made_them() {
    within(Duration::from_secs(300), || {
        let factory = factory();
        let Some(uri) = INSTRUMENTS.iter().rev().find_map(|name| installed(&factory, name)) else { return };
        let mut instance = create(&factory, &device(&uri));
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
        let mut instance = create(&again, &device(&uri));
        let (left, _) = run(&mut instance, 20, silence, held_note);
        assert!(peak(&left) > 0.01);
    });
}

/// A looping bar of eighth notes on a new track, so there is always
/// something sounding.
fn add_looping_track(session: &mut Session, name: &str) -> daw_core::TrackId {
    session
        .edit(|project| {
            let track = project.add_track(name);
            let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
            let notes: Vec<Note> = (0..8)
                .map(|i| Note { pitch: 55 + i as u8, start: i as f64 * 0.5, length: 0.45, velocity: 110 })
                .collect();
            project.clip_mut(clip)?.add_notes(&notes)?;
            project.set_loop(true, 0.0, 4.0)?;
            Ok(track)
        })
        .unwrap()
}

#[test]
fn a_session_hosts_a_plugin_and_shuts_down_cleanly() {
    within(Duration::from_secs(300), || {
        let plugins = factory();
        let Some((uri, name, quiet)) =
            LEVELS.iter().find_map(|(name, level, quiet)| Some((installed(&plugins, name)?, *level, *quiet)))
        else {
            return;
        };
        let mut session =
            Session::new(Arc::new(composite(&plugins)), AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
        assert!(session.descriptors().iter().any(|descriptor| descriptor.uri == uri));

        let track = add_looping_track(&mut session, "Lead");
        let meter = session.track_meter(track).unwrap();
        let sounding = |limit| eventually(limit, || meter.take().0 > 0.01);

        let device = session.add_device(ChainRef::Track(track), &uri, None, None).unwrap();
        assert!(session.problems().is_empty(), "{:?}", session.problems());
        let params = session.device_params(device).unwrap();
        let level = params.iter().find(|param| param.info.name == name).expect("the level parameter");
        let (level, normal) = (level.info.id.clone(), level.value);
        assert!(normal > quiet, "{uri}: '{name}' starts at {normal}");

        session.play();
        assert!(sounding(Duration::from_secs(10)), "{uri} is silent in the session");
        assert_eq!(session.audio_faults(), 0);

        // An edit that reaches the running plugin as a parameter change.
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
        assert!(saved.contains(&uri) && saved.contains("\"state\""), "the saved project lacks the plugin's state");
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

#[test]
fn clap_and_vst3_instances_coexist_in_one_session() {
    within(Duration::from_secs(300), || {
        let clap = ClapConfig {
            scanner: scanner(),
            cache_path: Some(scratch("coexist-clap-cache.json")),
            scan_timeout: Duration::from_secs(60),
            ..ClapConfig::default()
        };
        let host = PluginHost::with_configs(clap, config()).unwrap();
        // The same plugin in both formats where possible: two libraries
        // built from one code base, sharing this process and one plugin
        // main thread.
        let clap_uris: Vec<String> = host.clap().descriptors().into_iter().map(|descriptor| descriptor.uri).collect();
        let pair = [(WAVESYNTH_CLAP, WAVESYNTH), (SURGE_CLAP, SURGE)].into_iter().find_map(|(clap, vst3)| {
            let clap = clap_uris.iter().find(|uri| *uri == clap)?;
            Some((clap.clone(), installed(host.vst3(), vst3)?))
        });
        let Some((clap_uri, vst3_uri)) = pair else {
            eprintln!("skipping: no plugin is installed in both formats");
            return;
        };

        let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(BuiltinFactory)];
        factories.extend(host.factories());
        let mut session =
            Session::new(Arc::new(CompositeFactory::new(factories)), AudioBackend::Null { sample_rate: SR as u32 })
                .unwrap();

        let tracks = [add_looping_track(&mut session, "CLAP"), add_looping_track(&mut session, "VST3")];
        let devices = [
            session.add_device(ChainRef::Track(tracks[0]), &clap_uri, None, None).unwrap(),
            session.add_device(ChainRef::Track(tracks[1]), &vst3_uri, None, None).unwrap(),
        ];
        assert!(session.problems().is_empty(), "{:?}", session.problems());
        for device in devices {
            assert!(!session.device_params(device).unwrap().is_empty());
        }

        session.play();
        for (track, uri) in tracks.iter().zip([&clap_uri, &vst3_uri]) {
            let meter = session.track_meter(*track).unwrap();
            assert!(eventually(Duration::from_secs(10), || meter.take().0 > 0.01), "{uri} is silent");
        }
        assert_eq!(session.audio_faults(), 0);

        // Both survive a save and a reload, each with its own state.
        let path = scratch(&format!("coexist-{}.json", std::process::id()));
        session.save(&path).unwrap();
        session.load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(session.problems().is_empty(), "{:?}", session.problems());
        let plugin_states: Vec<bool> = session
            .project()
            .devices()
            .filter(|device| [&clap_uri, &vst3_uri].contains(&&device.uri))
            .map(|device| device.state.as_ref().is_some_and(|state| !state.is_empty()))
            .collect();
        assert_eq!(plugin_states, [true, true]);
        session.play();
        for track in session.project().tracks.iter().map(|track| track.id).collect::<Vec<_>>() {
            let meter = session.track_meter(track).unwrap();
            assert!(eventually(Duration::from_secs(10), || meter.take().0 > 0.01), "silent after reloading");
        }

        // Removing one leaves the other playing.
        let vst3_device = session.project().tracks[1].devices[0].id;
        session.edit(|project| project.remove_device(vst3_device).map(|_| ())).unwrap();
        session.housekeeping();
        let meter = session.track_meter(session.project().tracks[0].id).unwrap();
        meter.take();
        assert!(eventually(Duration::from_secs(10), || meter.take().0 > 0.01), "{clap_uri} stopped with {vst3_uri}");
        assert!(session.undo());
        assert!(session.problems().is_empty(), "{:?}", session.problems());

        session.stop();
        session.housekeeping();
        assert_eq!(session.audio_faults(), 0);
        drop(session);
        drop(host);
    });
}
