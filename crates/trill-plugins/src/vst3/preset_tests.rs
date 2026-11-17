//! Presets against the scripted plugins of [`super::scripted`]: preset
//! files in the format's folders and the programs a plugin lists are
//! found, and a running plugin loads either. The integration tests do the
//! same with real plugins.

use super::class_id::ClassId;
use super::scripted::{self, GAIN, PROGRAM, PROGRAM_LIST_NAME, kind};
use super::state::{self, PluginState};
use super::{Vst3Config, Vst3Factory};
use crate::presets::Locator;
use crate::scan::Scanner;
use crate::test_support::{TestDir, eventually};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{
    CompositeFactory, DeviceFactory, DeviceInstance, Event, EventKind, PresetLoadError, ProcessInfo,
};
use trill_core::presets::{PresetInfo, PresetList, PresetLookup, PresetStatus};
use trill_core::{ChainRef, DeviceId, Note, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::render::RenderOptions;
use trill_engine::{AudioBackend, Session};

const SR: f64 = 48_000.0;
const BLOCK: usize = 64;
const WAIT: Duration = Duration::from_secs(60);

/// The scripted plugins, with preset folders of the test's own.
struct Rig {
    dir: TestDir,
    factory: Vst3Factory,
}

impl Rig {
    fn new() -> Self {
        let dir = TestDir::new();
        let config = Vst3Config {
            search_paths: Vec::new(),
            scanner: Scanner::InProcess,
            cache_path: Some(dir.path().join("cache").join("vst3.json")),
            scan_timeout: Duration::from_secs(10),
            preset_paths: vec![dir.path().join("user"), dir.path().join("shared")],
            vendor_places: crate::VendorPlaces::default(),
            patient_teardown: true,
        };
        let factory = Vst3Factory::with_modules_in(config, vec![("scripted", scripted::factory())]);
        Self { dir, factory }
    }

    /// The folder of the plugin `kind` in the preset place `place`.
    fn folder(&self, place: &str, kind: u8) -> PathBuf {
        let uri = scripted::uri(kind);
        let descriptor = self.factory.descriptors().into_iter().find(|descriptor| descriptor.uri == uri).unwrap();
        self.dir.path().join(place).join(descriptor.vendor).join(descriptor.name)
    }

    /// Writes a preset file for the plugin `kind` that sets its gain.
    fn preset_file(&self, place: &str, kind: u8, name: &str, gain: f64) -> PathBuf {
        let path = self.folder(place, kind).join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, preset_bytes(kind, gain)).unwrap();
        path
    }

    fn listed(&self, kind: u8) -> PresetList {
        let list = self.factory.presets_within(&scripted::uri(kind), WAIT).expect("a VST3 plugin");
        assert!(!list.is_reading(), "the presets were not read within {WAIT:?}");
        list
    }

    fn create(&self, kind: u8) -> DeviceInstance {
        let mut project = Project::new("t");
        let id = project.add_device(ChainRef::Master, scripted::uri(kind), "plugin", None).unwrap();
        self.factory.create(project.device(id).unwrap(), SR, BLOCK).unwrap_or_else(|error| panic!("{error}"))
    }
}

fn class(kind: u8) -> ClassId {
    ClassId::parse(scripted::uri(kind).strip_prefix("vst3:").unwrap()).unwrap()
}

/// The bytes of a preset file for the plugin `kind`: its component's
/// state with the gain `gain`, and its controller's.
fn preset_bytes(kind: u8, gain: f64) -> Vec<u8> {
    let values = |magic: &[u8; 4], values: &[f64]| {
        let mut bytes = magic.to_vec();
        bytes.extend(values.iter().flat_map(|value| value.to_le_bytes()));
        bytes
    };
    let state = PluginState { component: values(b"SCRP", &[gain, 0.0]), controller: values(b"CTRL", &[0.5]) };
    state::encode(class(kind), &state)
}

fn preset<'a>(list: &'a PresetList, wanted: &str) -> &'a PresetInfo {
    match list.find(wanted) {
        PresetLookup::Found(preset) => preset,
        other => panic!("'{wanted}': {other:?}"),
    }
}

/// The gain the plugin's controller reports.
fn gain(instance: &mut DeviceInstance) -> f64 {
    let index = instance.params.iter().position(|param| param.id == GAIN.to_string()).expect("a gain");
    instance.controller.as_mut().unwrap().param_values().expect("the plugin reports values")[index]
}

fn load(instance: &mut DeviceInstance, preset: &PresetInfo) -> Result<(), PresetLoadError> {
    instance.controller.as_mut().unwrap().load_preset(preset)
}

/// What a note at full velocity comes out as: twice the plugin's gain.
fn impulse(instance: &mut DeviceInstance) -> f32 {
    let info = ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        loop_range: None,
    };
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let note = Event { offset: 5, kind: EventKind::NoteOn { pitch: 60, velocity: 1.0 } };
    instance.processor.process(&info, &[note], &mut left, &mut right);
    left[5]
}

fn failure(result: Result<(), PresetLoadError>) -> String {
    match result {
        Err(PresetLoadError::Failed(reason)) => reason,
        other => panic!("expected a failure with a reason, got {other:?}"),
    }
}

// ---- listing -------------------------------------------------------------

#[test]
fn preset_files_and_programs_are_listed_together() {
    let rig = Rig::new();
    let mine = rig.preset_file("user", kind::IMPULSE, "Mine.vstpreset", 0.4);
    rig.preset_file("user", kind::IMPULSE, "Leads/Sharp.vstpreset", 0.3);
    rig.preset_file("shared", kind::IMPULSE, "Stock.vstpreset", 0.2);
    // In the plugin's folder, and for another plugin all the same.
    std::fs::write(rig.folder("user", kind::IMPULSE).join("Stray.vstpreset"), preset_bytes(kind::MONO_GAIN, 0.1))
        .unwrap();

    let list = rig.listed(kind::IMPULSE);
    assert_eq!(list.status, PresetStatus::Ready);
    let paths: Vec<String> = list.presets.iter().map(PresetInfo::path).collect();
    let programs = |name: &str| format!("{PROGRAM_LIST_NAME}/{name}");
    assert_eq!(
        paths,
        [
            // The files, the user's own first, by folder.
            "Mine".to_string(),
            "Leads/Sharp".to_string(),
            "Stock".to_string(),
            // Then the programs of the plugin's list. One has no name.
            programs("Quiet"),
            programs("Loud"),
            programs("Program 3"),
        ]
    );
    assert_eq!(preset(&list, "Mine").tags, ["user"]);
    assert_eq!(preset(&list, "Stock").tags, ["factory"]);
    assert_eq!(preset(&list, "Quiet").tags, ["Synth", "Soft Pad"]);
    assert!(preset(&list, "Loud").tags.is_empty());
    assert_eq!(Locator::parse(&preset(&list, "Mine").locator), Some(Locator::Vst3File { path: mine }));
    let loud = Locator::Vst3Program { param: PROGRAM, index: 1, steps: 2 };
    assert_eq!(Locator::parse(&preset(&list, "Loud").locator), Some(loud));
    let mut ids: Vec<&str> = list.presets.iter().map(|preset| preset.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 6);

    // Each plugin has its own folder, and every scripted plugin the same
    // three programs.
    assert_eq!(rig.listed(kind::MONO_GAIN).len(), 3);
}

#[test]
fn a_preset_saved_while_the_program_runs_is_found_by_a_refresh() {
    let rig = Rig::new();
    let uri = scripted::uri(kind::IMPULSE);
    assert_eq!(rig.listed(kind::IMPULSE).len(), 3, "only the programs");

    rig.preset_file("user", kind::IMPULSE, "New.vstpreset", 0.6);
    assert_eq!(rig.factory.presets(&uri).unwrap().len(), 3, "nobody has looked again yet");
    rig.factory.refresh_presets(&uri);
    assert_eq!(rig.factory.presets(&uri).unwrap().len(), 3, "listed while it is read again");
    let list = rig.listed(kind::IMPULSE);
    assert_eq!(list.presets[0].name, "New");
    assert_eq!(list.len(), 4);
}

#[test]
fn plugins_without_a_controller_and_urls_of_others_have_nothing_to_list() {
    let rig = Rig::new();
    // Its edit controller cannot be had, and with it its program lists.
    assert_eq!(rig.listed(kind::LOST_CONTROLLER), PresetList::default());
    // A plugin that cannot even be created: a message, like any failure.
    match rig.listed(kind::FAILS_INIT).status {
        PresetStatus::Failed(reason) => assert!(reason.contains("failed to initialize"), "{reason}"),
        other => panic!("expected a failure, got {other:?}"),
    }
    assert_eq!(rig.factory.presets_within("vst3:0123456789ABCDEF0123456789ABCDEF", WAIT), Some(PresetList::default()));
    assert_eq!(rig.factory.presets("vst3:not a class id"), Some(PresetList::default()));
    for other in ["clap:com.acme.synth", "builtin:polysynth", "no-scheme"] {
        assert_eq!(rig.factory.presets(other), None, "{other}");
        assert_eq!(rig.factory.presets_within(other, WAIT), None, "{other}");
        rig.factory.refresh_presets(other);
    }
    // In either case of its digits, a class id names the same plugin.
    let lower = scripted::uri(kind::IMPULSE).to_lowercase();
    assert_eq!(rig.factory.presets_within(&lower, WAIT).unwrap().len(), 3);
}

// ---- loading -------------------------------------------------------------

#[test]
fn a_running_plugin_switches_program_with_its_next_block() {
    let rig = Rig::new();
    let list = rig.listed(kind::IMPULSE);
    let mut instance = rig.create(kind::IMPULSE);
    assert_eq!(impulse(&mut instance), 1.0, "the gain it starts with is a half");

    // Nothing is processing it, so the wait for the program to arrive
    // ends by itself; the controller shows the program at once.
    load(&mut instance, preset(&list, "Loud")).unwrap();
    assert_eq!(gain(&mut instance), 0.75);
    assert_eq!(impulse(&mut instance), 1.5, "the block that brings the program plays with it");
    load(&mut instance, preset(&list, "Program 3")).unwrap();
    assert_eq!((gain(&mut instance), impulse(&mut instance)), (0.25, 0.5));

    // What a project saves is the program: a plugin created from it has it.
    let state = instance.controller.as_mut().unwrap().save_state();
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, scripted::uri(kind::IMPULSE), "plugin", None).unwrap();
    project.device_mut(id).unwrap().state = state;
    let mut restored =
        rig.factory.create(project.device(id).unwrap(), SR, BLOCK).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!((gain(&mut restored), impulse(&mut restored)), (0.25, 0.5));
}

#[test]
fn a_running_plugin_takes_on_the_states_of_a_preset_file() {
    let rig = Rig::new();
    rig.preset_file("user", kind::IMPULSE, "Mine.vstpreset", 0.4);
    rig.preset_file("shared", kind::IMPULSE, "Stock.vstpreset", 0.2);
    let list = rig.listed(kind::IMPULSE);
    let mut instance = rig.create(kind::IMPULSE);

    load(&mut instance, preset(&list, "Mine")).unwrap();
    assert_eq!((gain(&mut instance), impulse(&mut instance)), (0.4, 0.8));
    load(&mut instance, preset(&list, "Stock")).unwrap();
    assert_eq!((gain(&mut instance), impulse(&mut instance)), (0.2, 0.4));
}

#[test]
fn a_preset_that_does_not_load_says_why() {
    let rig = Rig::new();
    let folder = rig.folder("user", kind::IMPULSE);
    rig.preset_file("user", kind::IMPULSE, "Good.vstpreset", 0.4);
    let list = rig.listed(kind::IMPULSE);
    let mut instance = rig.create(kind::IMPULSE);
    load(&mut instance, preset(&list, "Good")).unwrap();

    let file = |name: &str, bytes: &[u8]| {
        let path = folder.join(name);
        std::fs::write(&path, bytes).unwrap();
        PresetInfo { name: name.into(), locator: Locator::Vst3File { path }.to_text(), ..PresetInfo::default() }
    };
    // Files that changed or went since they were listed.
    let reason = failure(load(&mut instance, &file("theirs", &preset_bytes(kind::MONO_GAIN, 0.1))));
    assert!(reason.starts_with("the preset belongs to another plugin ("), "{reason}");
    assert!(failure(load(&mut instance, &file("empty", b""))).ends_with("is empty"));
    let text = b"this is not a preset file at all, whatever it may be called in this folder";
    let reason = failure(load(&mut instance, &file("text", text)));
    assert_eq!(reason, "the preset is not a VST3 state");
    let whole = preset_bytes(kind::IMPULSE, 0.9);
    let reason = failure(load(&mut instance, &file("cut", &whole[..whole.len() - 10])));
    assert!(reason.starts_with("the preset is damaged"), "{reason}");
    let gone =
        PresetInfo { locator: Locator::Vst3File { path: folder.join("gone") }.to_text(), ..PresetInfo::default() };
    assert!(failure(load(&mut instance, &gone)).contains("gone' cannot be read: "));
    // A state the plugin itself turns down.
    let foreign = state::encode(class(kind::IMPULSE), &PluginState { component: b"junk".to_vec(), controller: vec![] });
    let reason = failure(load(&mut instance, &file("junk", &foreign)));
    assert!(reason.starts_with("the plugin rejected its preset"), "{reason}");

    // A program of a parameter the plugin does not have, and presets
    // that are not this format's.
    let program = Locator::Vst3Program { param: 999, index: 0, steps: 2 };
    let program = PresetInfo { locator: program.to_text(), ..PresetInfo::default() };
    assert!(failure(load(&mut instance, &program)).contains("no longer has the parameter"));
    for locator in ["", "warm_pad", "clap-plugin\n\nquiet"] {
        let foreign = PresetInfo { locator: locator.into(), ..PresetInfo::default() };
        assert_eq!(failure(load(&mut instance, &foreign)), "it is not a preset of a VST3 plugin");
    }
    assert_eq!((gain(&mut instance), impulse(&mut instance)), (0.4, 0.8), "it still has the preset before all that");
}

// ---- through the session ---------------------------------------------------

/// A session over the built-in devices and the scripted plugins, with a
/// track that plays one note at full velocity on beat 1, and nothing on
/// the master to come between that note and the output.
fn session(rig: &Rig) -> (Session, TrackId) {
    let devices = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(rig.factory.clone())]);
    let mut session = Session::new(Arc::new(devices), AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let track = session
        .edit(|project| {
            let limiter = project.master.devices[0].id;
            project.remove_device(limiter)?;
            let track = project.add_track("Lead");
            let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
            project.clip_mut(clip)?.add_notes(&[Note { pitch: 60, start: 1.0, length: 1.0, velocity: 127 }])?;
            Ok(track)
        })
        .unwrap();
    (session, track)
}

/// The sample the song's one note comes out as, to four places: twice the
/// gain of whatever preset the instrument has, in an instance made from
/// the project.
fn rendered(session: &mut Session) -> f32 {
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.1, ..RenderOptions::default() };
    let out = session.render_job().render(&options).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    (out.left[24_000] * 1e4).round() / 1e4
}

fn preset_name(session: &Session, device: DeviceId) -> Option<String> {
    session.device_preset(device).map(|preset| preset.name.clone())
}

#[test]
fn the_session_lists_loads_undoes_and_saves_programs_and_preset_files() {
    let rig = Rig::new();
    rig.preset_file("user", kind::IMPULSE, "Mine.vstpreset", 0.4);
    rig.listed(kind::IMPULSE);
    let (mut session, track) = session(&rig);
    let uri = scripted::uri(kind::IMPULSE);
    assert_eq!(session.presets(&uri).len(), 4);

    // A program to start from. The engine is processing the plugin, so
    // the program arrives with a block.
    let device = session.add_device(ChainRef::Track(track), &uri, Some("Loud"), None).unwrap();
    assert_eq!(session.device_presets(device).unwrap().len(), 4);
    assert!(eventually(WAIT, || rendered(&mut session) == 1.5), "{}", rendered(&mut session));
    assert_eq!(preset_name(&session, device).as_deref(), Some("Loud"));

    // A preset file, in place: one step, taken back and brought again.
    let step = session.undo_step();
    session.load_preset(device, "Mine").unwrap();
    assert_eq!((rendered(&mut session), preset_name(&session, device).as_deref()), (0.8, Some("Mine")));
    assert!(session.undo());
    assert_eq!(session.undo_step(), step);
    assert_eq!((rendered(&mut session), preset_name(&session, device).as_deref()), (1.5, Some("Loud")));
    assert!(session.redo());
    assert_eq!(rendered(&mut session), 0.8);

    // Another program on top, and a value on top of that.
    session.load_preset(device, "Quiet").unwrap();
    assert!(eventually(WAIT, || rendered(&mut session) == 0.25), "{}", rendered(&mut session));
    session.set_device_params(device, &[(GAIN.to_string(), 0.3)]).unwrap();
    assert_eq!(rendered(&mut session), 0.6);
    assert!(session.undo());
    assert!(eventually(WAIT, || rendered(&mut session) == 0.25), "{}", rendered(&mut session));

    // A preset that does not load: said, and nothing changes.
    std::fs::write(rig.folder("user", kind::IMPULSE).join("Mine.vstpreset"), b"scribbled over").unwrap();
    let error = session.load_preset(device, "Mine").unwrap_err().to_string();
    assert!(error.contains("did not load the preset 'Mine': the preset is not a VST3 state"), "{error}");
    assert_eq!((rendered(&mut session), preset_name(&session, device).as_deref()), (0.25, Some("Quiet")));

    // What is saved is what was loaded, and it comes back from the file.
    let path = rig.dir.path().join("song.trill");
    session.save(&path).unwrap();
    session.new_project();
    session.load(&path).unwrap();
    assert_eq!((rendered(&mut session), preset_name(&session, device).as_deref()), (0.25, Some("Quiet")));
}

// ---- a preset file that lies anywhere --------------------------------------

#[test]
fn a_preset_file_outside_the_preset_folders_is_loaded_by_its_path() {
    let rig = Rig::new();
    rig.preset_file("user", kind::IMPULSE, "Mine.vstpreset", 0.4);
    let bought = rig.dir.file("samples/Pack/Bought Lead.vstpreset", &preset_bytes(kind::IMPULSE, 0.3));
    let theirs = rig.dir.file("samples/Pack/Theirs.vstpreset", &preset_bytes(kind::MONO_GAIN, 0.1));
    let patch = rig.dir.file("samples/Pack/Patch.fxp", b"CcnK");
    let uri = scripted::uri(kind::IMPULSE);
    let of = |file: &std::path::Path| rig.factory.preset_file(&uri, file, None).expect("a plugin of this factory");
    assert!(matches!(rig.listed(kind::IMPULSE).find("Bought Lead"), PresetLookup::Missing { .. }));

    // The file is not listed, and is a preset all the same.
    let lead = of(&bought).unwrap();
    assert_eq!((lead.name.as_str(), lead.category.as_str(), lead.id.len()), ("Bought Lead", "", 10));
    let mut instance = rig.create(kind::IMPULSE);
    load(&mut instance, &lead).unwrap();
    assert_eq!((gain(&mut instance), impulse(&mut instance)), (0.3, 0.6));
    assert_eq!(instance.controller.as_mut().unwrap().take_preset_note(), None);
    // A listed file's path is the listed preset.
    let mine = rig.folder("user", kind::IMPULSE).join("Mine.vstpreset");
    assert_eq!(&of(&mine).unwrap(), preset(&rig.listed(kind::IMPULSE), "Mine"));

    // Another plugin's file, and a kind of file this plugin has no use for.
    let (this, other) = (plugin_name(&rig, kind::IMPULSE), plugin_name(&rig, kind::MONO_GAIN));
    assert_eq!(of(&theirs).unwrap_err(), format!("it is a preset of {other}, not of {this}"));
    assert_eq!(of(&patch).unwrap_err(), format!("{this} is given presets from .vstpreset files, and this is not one"));
    assert_eq!(rig.factory.preset_file_type(&bought), Some(uri.clone()));
    assert_eq!(rig.factory.preset_file_type(&patch), None);

    // Through a session: one undo step, like a preset by name.
    let (mut session, track) = session(&rig);
    let device = session.add_device(ChainRef::Track(track), &uri, None, None).unwrap();
    let step = session.undo_step();
    let loaded = session.load_preset(device, &bought.to_string_lossy()).unwrap();
    assert_eq!(loaded.name, "Bought Lead");
    assert_eq!(session.device_preset(device).map(|preset| preset.name.as_str()), Some("Bought Lead"));
    assert!(session.undo());
    assert_eq!((session.undo_step(), session.device_preset(device)), (step, None));
    let error = session.load_preset(device, &theirs.to_string_lossy()).unwrap_err().to_string();
    assert!(error.ends_with(&format!("as a preset: it is a preset of {other}, not of {this}")), "{error}");
}

fn plugin_name(rig: &Rig, kind: u8) -> String {
    let uri = scripted::uri(kind);
    rig.factory.descriptors().into_iter().find(|descriptor| descriptor.uri == uri).unwrap().name
}
