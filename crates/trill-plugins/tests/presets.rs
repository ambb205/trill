//! Presets of real plugins: found by the scanner process, loaded into a
//! running plugin, and kept by a project.
//!
//! Like the other end-to-end tests these use whatever is installed in the
//! system's plugin directories, and a test whose plugin is missing prints
//! a note and passes. They are skipped unless asked for. The one test
//! here that needs no plugin runs with the rest.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::presets::{PresetInfo, PresetList, PresetQuery, PresetStatus};
use trill_core::{ChainRef, DeviceId, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::render::RenderOptions;
use trill_engine::{AudioBackend, Session};
use trill_plugins::{ClapConfig, ClapFactory, Scanner, Vst3Config, Vst3Factory};

const SURGE: &str = "clap:org.surge-synth-team.surge-xt";
const SURGE_VST3: &str = "vst3:ABCDEF019182FAEB566D624153675854";
const JS80P: &str = "vst3:00565354414D4A386A73383070000000";

const SR: f64 = 48_000.0;
/// How long the presets of one plugin may take to read. The first time a
/// machine reads three thousand preset files takes most of a minute.
const PATIENCE: Duration = Duration::from_secs(300);

fn scanner_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trill-plugin-scan"))
}

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("presets").join(name)
}

fn clap() -> ClapFactory {
    let config = ClapConfig {
        scanner: Scanner::Executable(scanner_path()),
        cache_path: Some(scratch("clap").join("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        ..ClapConfig::default()
    };
    ClapFactory::new(config).expect("the plugin main thread starts")
}

/// A VST3 factory over the system's plugins that looks for preset files
/// in `presets` only: none of the user's own are read.
fn vst3(presets: &Path) -> Vst3Factory {
    let config = Vst3Config {
        scanner: Scanner::Executable(scanner_path()),
        cache_path: Some(scratch("vst3").join("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        preset_paths: vec![presets.to_path_buf()],
        ..Vst3Config::default()
    };
    Vst3Factory::new(config).expect("the plugin main thread starts")
}

/// Whether `uri` is installed here. Says so when a test is skipped for it.
fn installed(factory: &dyn DeviceFactory, uri: &str) -> bool {
    let found = factory.descriptors().iter().any(|descriptor| descriptor.uri == uri);
    if !found {
        eprintln!("skipping '{uri}': the plugin is not installed");
    }
    found
}

/// The presets of `uri`, read to the end, with how long that took.
fn listed(factory: &dyn DeviceFactory, uri: &str) -> PresetList {
    let started = Instant::now();
    let list = factory.presets_within(uri, PATIENCE).expect("a plugin of this factory");
    eprintln!("{uri}: {} presets after {:.2} s ({:?})", list.len(), started.elapsed().as_secs_f64(), list.status);
    assert!(!list.is_reading(), "{uri}: its presets were not read within {PATIENCE:?}");
    list
}

fn search<'a>(list: &'a PresetList, text: &str, category: Option<&str>) -> Vec<&'a PresetInfo> {
    list.search(&PresetQuery { text, category, offset: 0, limit: 50 }).presets
}

/// A session over the built-in devices and `plugins`, with a track that
/// plays three notes from beat 1 on.
fn session(plugins: Box<dyn DeviceFactory>) -> (Session, trill_core::TrackId) {
    let devices = CompositeFactory::new(vec![Box::new(BuiltinFactory), plugins]);
    let mut session = Session::new(Arc::new(devices), AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let track = session
        .edit(|project| {
            let track = project.add_track("Lead");
            let clip = project.add_midi_clip(track, "clip", 0.0, 8.0)?;
            let notes: Vec<Note> = [(1.0, 60), (2.5, 64), (4.0, 67)]
                .map(|(start, pitch)| Note { pitch, start, length: 1.2, velocity: 110, ..Default::default() })
                .into();
            project.clip_mut(clip)?.add_notes(&notes)?;
            project.set_loop(true, 0.0, 8.0)?;
            Ok(track)
        })
        .unwrap();
    (session, track)
}

/// What the song sounds like, as a render makes it from the project: in
/// plugins of its own, created from the state the project holds.
#[derive(Clone, Copy, Debug)]
struct Sound {
    rms: f32,
    peak: f32,
    /// How much of the energy of the first note lies in its first tenth
    /// of a second: high for a pluck, low for a pad that swells.
    attack: f32,
}

impl Sound {
    /// Whether two renders are of the same patch, as far as plugins that
    /// start every note at another phase let that be told.
    fn like(self, other: Sound) -> bool {
        let near = |a: f32, b: f32| (a - b).abs() <= a.abs().max(b.abs()) * 0.3 + 1e-4;
        near(self.rms, other.rms) && near(self.attack, other.attack)
    }
}

fn rendered(session: &mut Session) -> Sound {
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let out = session.render_job().render(&options).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    assert!(out.left.iter().chain(&out.right).all(|sample| sample.is_finite()));
    let energy = |samples: &[f32]| samples.iter().map(|sample| sample * sample).sum::<f32>();
    // The first note starts at beat 1: half a second in.
    let first = SR as usize / 2;
    let note = &out.left[first..first + SR as usize / 2];
    Sound {
        rms: (energy(&out.left) / out.left.len().max(1) as f32).sqrt(),
        peak: out.left.iter().fold(0.0, |peak, sample| peak.max(sample.abs())),
        attack: energy(&note[..SR as usize / 10]) / energy(note).max(1e-12),
    }
}

/// Loads presets from `candidates` until one changes the sound from
/// `before`, and returns it with what it sounds like.
fn load_one_that_sounds_different(
    session: &mut Session,
    device: DeviceId,
    candidates: &[&PresetInfo],
    before: Sound,
) -> (PresetInfo, Sound) {
    for preset in candidates.iter().take(8) {
        let started = Instant::now();
        session.load_preset(device, &preset.id).unwrap_or_else(|error| panic!("{}: {error}", preset.path()));
        let took = started.elapsed();
        let after = rendered(session);
        eprintln!("{} loaded in {took:.0?}: {after:?}", preset.path());
        if !after.like(before) && after.peak > 1e-3 {
            return ((*preset).clone(), after);
        }
    }
    panic!("none of the presets tried changed the sound from {before:?}");
}

// ---- without any plugin ----------------------------------------------------

#[test]
fn the_reader_says_why_it_cannot_read_and_exits() {
    let folder = scratch("reader");
    std::fs::create_dir_all(&folder).unwrap();
    let garbage = folder.join("garbage.clap");
    std::fs::write(&garbage, b"this is not a shared library").unwrap();

    let read = |arguments: &[&str]| {
        let output = Command::new(scanner_path()).args(arguments).output().expect("the scanner runs");
        (output.status.code(), String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let path = garbage.to_string_lossy();
    for format in ["clap", "vst3"] {
        let (code, printed) = read(&["--presets", format, &path, "com.acme.synth"]);
        assert_eq!(code, Some(1), "{format}: {printed}");
        let last: serde_json::Value = serde_json::from_str(printed.lines().last().unwrap_or_default()).unwrap();
        assert!(last["failed"].as_str().is_some_and(|reason| !reason.is_empty()), "{format}: {printed}");
    }
    assert_eq!(read(&["--presets", "clap", &path]).0, Some(2), "too few arguments");
    assert_eq!(read(&["--presets", "lv2", &path, "id"]).0, Some(1), "no such format");
}

// ---- CLAP ------------------------------------------------------------------

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn surge_xt_tells_of_its_presets_through_the_scanner_process() {
    let factory = clap();
    if !installed(&factory, SURGE) {
        return;
    }
    // Asking never waits, however long the reading takes.
    let started = Instant::now();
    let first = factory.presets(SURGE).expect("a CLAP plugin");
    assert!(started.elapsed() < Duration::from_secs(2), "asking took {:?}", started.elapsed());
    eprintln!("asked in {:?}: {} presets, {:?}", started.elapsed(), first.len(), first.status);

    let list = listed(&factory, SURGE);
    assert_eq!(list.status, PresetStatus::Ready);
    assert!(list.len() > 500, "Surge XT ships hundreds of patches; {} were found", list.len());
    let mut ids: Vec<&str> = list.presets.iter().map(|preset| preset.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), list.len(), "every preset has an id of its own");
    assert!(list.presets.iter().all(|preset| !preset.name.is_empty() && !preset.locator.is_empty()));
    assert!(list.categories().len() > 10, "{:?}", list.categories());

    // Searched the way a user or an assistant would.
    let pads = search(&list, "", Some("pads"));
    assert!(pads.len() > 20, "{} presets are filed under Pads", pads.len());
    assert!(pads.iter().all(|preset| preset.category.to_lowercase().contains("pads")));
    let warm = search(&list, "warm pad", None);
    assert!(!warm.is_empty(), "nothing matches 'warm pad'");
    eprintln!("'warm pad': {:?}", warm.iter().map(|preset| preset.path()).collect::<Vec<_>>());
    assert!(list.presets.iter().any(|preset| !preset.author.is_empty()), "Surge's patches name their makers");

    // Read once: the cache answers the next run of the program.
    let again = clap();
    let started = Instant::now();
    assert_eq!(listed(&again, SURGE).presets, list.presets);
    assert!(started.elapsed() < Duration::from_secs(20), "the cache took {:?}", started.elapsed());
    assert!(scratch("clap").join("presets").is_dir(), "kept beside the scan cache");
}

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn surge_xt_loads_a_preset_while_it_plays_and_the_project_keeps_it() {
    let factory = clap();
    if !installed(&factory, SURGE) {
        return;
    }
    let list = listed(&factory, SURGE);
    let (mut session, track) = session(Box::new(factory.clone()));
    let device = session.add_device(ChainRef::Track(track), SURGE, None, None).unwrap();
    assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    assert_eq!(session.device_presets(device).unwrap().len(), list.len());
    let before = rendered(&mut session);
    assert!(before.peak > 1e-3, "the patch Surge starts with is heard: {before:?}");

    // Loaded while the song plays, which it goes on doing.
    session.play();
    std::thread::sleep(Duration::from_millis(200));
    let pads = search(&list, "", Some("factory presets/pads"));
    assert!(!pads.is_empty(), "Surge's factory pads were not found");
    let step = session.undo_step();
    let (preset, after) = load_one_that_sounds_different(&mut session, device, &pads, before);
    let position = session.transport().position_beats;
    std::thread::sleep(Duration::from_millis(200));
    let transport = session.transport();
    assert!(transport.playing && transport.position_beats != position, "{transport:?}");
    session.stop();
    assert_eq!(session.device_preset(device).map(|preset| preset.id.as_str()), Some(preset.id.as_str()));

    // One undo step for each preset tried; all of them undone, the sound
    // is the one from before.
    while session.undo_step() != step {
        assert!(session.undo());
        assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    }
    assert!(rendered(&mut session).like(before), "{:?} after undo, {before:?} before", rendered(&mut session));
    assert_eq!(session.device_preset(device), None);

    // Started from a preset, by its name and category, in one step.
    let second =
        session.add_device(ChainRef::Track(track), SURGE, Some(&preset.path()), None).unwrap_or_else(|e| panic!("{e}"));
    assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    session.edit(|project| project.remove_device(device).map(|_| ())).unwrap();
    let started_from = rendered(&mut session);
    assert!(started_from.like(after), "{started_from:?} added with the preset, {after:?} loaded into it");

    // Saved, and opened again in a session of its own.
    let path = scratch("songs").join("surge.trill");
    session.save(&path).unwrap();
    let saved = Project::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(saved.device(second).unwrap().state.as_ref().is_some_and(|state| state.len() > 1000));
    assert_eq!(saved.device(second).unwrap().preset.as_ref().map(|preset| &preset.name), Some(&preset.name));
    drop(session);
    let (mut reopened, _) = self::session(Box::new(factory));
    reopened.load(&path).unwrap();
    assert!(reopened.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    let loaded = rendered(&mut reopened);
    assert!(loaded.like(after), "{loaded:?} from the file, {after:?} when it was saved");
    assert!(!loaded.like(before));
}

// ---- VST3 ------------------------------------------------------------------

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn js80p_lists_its_programs_and_switches_between_them() {
    let presets = scratch("js80p-presets");
    let _ = std::fs::remove_dir_all(&presets);
    let factory = vst3(&presets);
    if !installed(&factory, JS80P) {
        return;
    }
    let list = listed(&factory, JS80P);
    assert_eq!(list.status, PresetStatus::Ready);
    assert!(list.len() > 20, "JS80P has a list of programs; {} were found", list.len());
    assert!(
        !search(&list, "organ", None).is_empty(),
        "{:?}",
        list.presets.iter().map(PresetInfo::path).collect::<Vec<_>>()
    );

    let (mut session, track) = session(Box::new(factory.clone()));
    let device = session.add_device(ChainRef::Track(track), JS80P, None, None).unwrap();
    assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    let before = rendered(&mut session);
    assert!(before.peak > 1e-3, "{before:?}");
    let step = session.undo_step();
    let programs: Vec<&PresetInfo> = list.presets.iter().skip(1).collect();
    let (preset, after) = load_one_that_sounds_different(&mut session, device, &programs, before);

    // The program as a preset file, in the place VST3 keeps those: what
    // the plugin has now is what a file of it holds.
    let state = session.render_job().project.device(device).unwrap().state.clone().expect("JS80P saves state");
    let descriptor = factory.descriptors().into_iter().find(|descriptor| descriptor.uri == JS80P).unwrap();
    let folder = presets.join(&descriptor.vendor).join(&descriptor.name).join("Mine");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Kept.vstpreset"), state).unwrap();

    while session.undo_step() != step {
        assert!(session.undo());
        assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    }
    assert!(rendered(&mut session).like(before));

    // Found by a refresh, without the plugin being asked, and loaded.
    session.refresh_presets(JS80P);
    let list = listed(&factory, JS80P);
    let kept = search(&list, "kept", Some("mine"));
    assert_eq!(kept.len(), 1, "{:?}", list.presets.iter().take(3).collect::<Vec<_>>());
    assert_eq!(kept[0].tags, ["user"]);
    session.load_preset(device, "Mine/Kept").unwrap();
    let from_file = rendered(&mut session);
    assert!(from_file.like(after), "{from_file:?} from the file, {after:?} as the program '{}'", preset.name);

    let path = scratch("songs").join("js80p.trill");
    session.save(&path).unwrap();
    drop(session);
    let (mut reopened, _) = self::session(Box::new(factory));
    reopened.load(&path).unwrap();
    assert!(reopened.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    assert!(rendered(&mut reopened).like(after));
    assert_eq!(reopened.device_preset(device).map(|preset| preset.name.as_str()), Some("Kept"));
}

#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn surge_xt_as_a_vst3_lists_no_programs_and_takes_a_preset_file() {
    let presets = scratch("surge-vst3-presets");
    let _ = std::fs::remove_dir_all(&presets);
    let factory = vst3(&presets);
    if !installed(&factory, SURGE_VST3) {
        return;
    }
    // Surge keeps its patches in files of its own, and its VST3 build
    // lists them neither as programs nor as preset files: there is
    // nothing for a host to list. (Its CLAP build tells of all of them.)
    assert_eq!(listed(&factory, SURGE_VST3), PresetList::default());

    // What a host can do for it is the format's own preset files. One
    // made from a state Surge saved loads like any other.
    let (mut session, track) = session(Box::new(factory.clone()));
    let device = session.add_device(ChainRef::Track(track), SURGE_VST3, None, None).unwrap();
    assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    let before = rendered(&mut session);
    let volume = session
        .device_params(device)
        .unwrap()
        .into_iter()
        .find(|param| param.info.name == "Global Volume")
        .expect("Surge has a global volume");
    session.set_device_params(device, &[(volume.info.id.clone(), volume.info.min)]).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let quiet = rendered(&mut session);
    assert!(quiet.rms < before.rms * 0.5, "{quiet:?} with the volume down, {before:?} before");
    let state = session.render_job().project.device(device).unwrap().state.clone().expect("Surge saves state");
    assert!(session.undo());
    assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");

    let descriptor = factory.descriptors().into_iter().find(|descriptor| descriptor.uri == SURGE_VST3).unwrap();
    let folder = presets.join(&descriptor.vendor).join(&descriptor.name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("Quiet.vstpreset"), state).unwrap();
    session.refresh_presets(SURGE_VST3);
    assert_eq!(listed(&factory, SURGE_VST3).len(), 1);
    session.load_preset(device, "Quiet").unwrap();
    let loaded = rendered(&mut session);
    assert!(loaded.rms < before.rms * 0.5, "{loaded:?} from the preset file, {before:?} before");
    assert!(session.undo());
    assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    assert!(rendered(&mut session).like(before));
}

/// A preset file of Surge XT's own, copied to where someone keeps the
/// presets they bought, is loaded by its path: the plugin reads the file
/// itself, as it reads the ones in its own folders.
#[test]
#[ignore = "needs the plugins installed on this machine; run with `cargo test -p trill-plugins -- --include-ignored`"]
fn surge_xt_is_given_a_preset_file_that_lies_outside_its_folders() {
    let factory = clap();
    if !installed(&factory, SURGE) {
        return;
    }
    let list = listed(&factory, SURGE);
    let (mut session, track) = session(Box::new(factory.clone()));
    let device = session.add_device(ChainRef::Track(track), SURGE, None, None).unwrap();
    assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    let before = rendered(&mut session);
    let bought = scratch("bought");
    let _ = std::fs::remove_dir_all(&bought);
    std::fs::create_dir_all(&bought).unwrap();

    // Copies of a few of its pads, until one is heard to differ from the
    // patch it starts with.
    let pads = search(&list, "", Some("factory presets/pads"));
    let step = session.undo_step();
    let mut found = None;
    for preset in pads.iter().take(8) {
        let source = trill_plugins::preset_file_of(preset).expect("a preset in a file");
        let copy = bought.join(source.file_name().unwrap());
        std::fs::copy(&source, &copy).unwrap();
        let loaded = session.load_preset(device, &copy.to_string_lossy()).unwrap_or_else(|error| panic!("{error}"));
        let after = rendered(&mut session);
        eprintln!("{} by its path: {after:?} (it starts as {before:?})", copy.display());
        assert_eq!(session.device_preset(device).map(|preset| preset.name.clone()), Some(loaded.name));
        if !after.like(before) && after.peak > 1e-3 {
            found = Some((copy, after));
            break;
        }
    }
    let (copy, after) = found.expect("one of the copied pads sounds different from the patch it starts with");
    // By its path it is the sound it is by its name.
    while session.undo_step() != step {
        assert!(session.undo());
        assert!(session.wait_until_opened(std::time::Duration::from_secs(180)), "the plugin never arrived");
    }
    assert!(rendered(&mut session).like(before));
    let name = copy.file_stem().unwrap().to_string_lossy().into_owned();
    session.load_preset(device, &format!("Pads/{name}")).unwrap_or_else(|error| panic!("{error}"));
    let by_name = rendered(&mut session);
    assert!(by_name.like(after), "{by_name:?} by its name, {after:?} by the path of a copy");

    // A file that is none of its presets is refused before the plugin is
    // asked, by what kind of file it is.
    let other = bought.join("Kick.wav");
    std::fs::write(&other, b"RIFF").unwrap();
    let error = session.load_preset(device, &other.to_string_lossy()).unwrap_err().to_string();
    eprintln!("{error}");
    assert!(error.contains("it is not a kind of file Surge XT keeps presets in"), "{error}");
}
