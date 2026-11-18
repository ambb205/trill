//! The vendors' presets through the host, end to end: listed by a
//! factory with a plugin's other presets, and loaded into a running
//! plugin by its controller, and by a session as an undo step.
//!
//! The plugins are scripted ones that have the vendors' class ids and
//! keep whatever state they are handed (`vst3::scripted::kind::KEEPERS`),
//! and the preset files are made here, in the formats: nothing of a
//! vendor's is read. The real plugins are in `tests/vendor.rs`.

use super::{VendorPlaces, ample, archetype, serum, superior, xfer};
use crate::test_support::{TestDir, there};
use crate::vst3::class_id::ClassId;
use crate::vst3::scripted::{self, kind};
use crate::vst3::state::{self, PluginState};
use crate::vst3::{Vst3Config, Vst3Factory};
use crate::{Scanner, VST3_SCHEME};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{CompositeFactory, DeviceFactory, DeviceInstance, PresetLoadError};
use trill_core::presets::{PresetInfo, PresetList, PresetLookup, PresetStatus};
use trill_core::{ChainRef, Device, Project};
use trill_engine::{AudioBackend, Session};

const SR: f64 = 48_000.0;
const BLOCK: usize = 64;
const WAIT: Duration = Duration::from_secs(30);

struct Rig {
    dir: TestDir,
    factory: Vst3Factory,
}

impl Rig {
    /// A host whose only plugins are the keepers, and whose vendors'
    /// places are folders of the test's own.
    fn new() -> Self {
        let dir = TestDir::new();
        let config = Vst3Config {
            search_paths: Vec::new(),
            scanner: Scanner::InProcess,
            cache_path: None,
            scan_timeout: Duration::from_secs(10),
            preset_paths: Vec::new(),
            vendor_places: VendorPlaces {
                documents: vec![dir.path().join("documents")],
                shared_data: vec![dir.path().join("data")],
                programs: vec![dir.path().join("programs")],
            },
            patient_teardown: true,
        };
        // The module's file is named as an Ample instrument's is, which is
        // what its folder of presets is found by.
        let factory = Vst3Factory::with_modules_in(config, vec![("AMH", scripted::keepers())]);
        Self { dir, factory }
    }

    fn presets(&self, kind: u8) -> PresetList {
        let list = self.factory.presets_within(&scripted::uri(kind), WAIT).expect("a plugin of this factory");
        assert_eq!(list.status, PresetStatus::Ready, "{list:?}");
        list
    }

    fn preset(&self, kind: u8, name: &str) -> PresetInfo {
        match self.presets(kind).find(name) {
            PresetLookup::Found(preset) => preset.clone(),
            other => panic!("'{name}': {other:?}"),
        }
    }

    /// The plugin of `kind`, started from `state` if it is given one.
    fn create(&self, kind: u8, state: Option<&PluginState>) -> DeviceInstance {
        self.factory.create(&device(kind, state), SR, BLOCK).unwrap_or_else(|error| panic!("{error}"))
    }
}

fn class(kind: u8) -> ClassId {
    ClassId::parse(scripted::uri(kind).strip_prefix("vst3:").unwrap()).unwrap()
}

fn device(kind: u8, state: Option<&PluginState>) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, scripted::uri(kind), "plugin", None).unwrap();
    project.device_mut(id).unwrap().state = state.map(|state| state::encode(class(kind), state));
    project.device(id).unwrap().clone()
}

/// The state the plugin has now.
fn state_of(instance: &mut DeviceInstance) -> PluginState {
    match instance.controller.as_mut().unwrap().save_state() {
        Some(blob) => state::decode(&blob).unwrap().1,
        None => PluginState::default(),
    }
}

fn load(instance: &mut DeviceInstance, preset: &PresetInfo) -> Result<(), PresetLoadError> {
    instance.controller.as_mut().unwrap().load_preset(preset)
}

fn note(instance: &mut DeviceInstance) -> Option<String> {
    instance.controller.as_mut().unwrap().take_preset_note()
}

const SERUM: &str = "documents/Xfer/Serum 2 Presets/Presets";
const AMPLE: &str = "documents/Ample Sound/AMH/Presets4";
const SUPERIOR: &str = "programs/Toontrack/Superior/Superior Drummer/SL-Test/s3presetconf";
const ARCHETYPE: &str = "data/Neural DSP/Archetype Test X";

#[test]
fn a_vendors_preset_files_are_listed_as_the_plugins_presets() {
    let rig = Rig::new();
    // Before any file is there, a plugin has none; the keepers have no
    // programs or preset files of the format's own either.
    assert!(rig.presets(kind::KEEPS_SERUM).is_empty());

    rig.dir.file(
        &format!("{SERUM}/Factory/Pad/PD - Warm Room.SerumPreset"),
        &serum::preset_file("PD - Warm Room", "Level 8", 4.0, &[]),
    );
    rig.dir.file(&format!("{SERUM}/User/Mine.SerumPreset"), &serum::preset_file("Mine", "Me", 5.0, &[]));
    let tune = rig.dir.path().join("tune");
    let document = ample::document("9 String Metal", "4.0.0", "AMHBg", &tune, &[("M-Master Vol", "0.8")]);
    rig.dir.file(&format!("{AMPLE}/9 String Metal.amhstate"), document.as_bytes());
    rig.dir.file(
        SUPERIOR,
        &superior::presets_file(&[("Ayotte Default", "Clean Kits", "Kick2"), ("Death Metal", "Metal", "Kick62")]),
    );
    rig.dir.file(&format!("{ARCHETYPE}/Artists/Somebody/80s Lead.xml"), &archetype::preset_file("80s Lead", "1", &[]));

    // Read again, each plugin has its vendor's presets and no other's.
    let listed = |kind: u8| -> Vec<String> {
        rig.factory.refresh_presets(&scripted::uri(kind));
        rig.presets(kind).presets.iter().map(|preset| preset.path()).collect()
    };
    assert_eq!(listed(kind::KEEPS_SERUM), ["Factory/Pad/PD - Warm Room", "User/Mine"]);
    assert_eq!(listed(kind::KEEPS_AMPLE), ["Metal/9 String Metal"]);
    assert_eq!(listed(kind::KEEPS_SUPERIOR), ["Clean Kits/Ayotte Default", "Metal/Death Metal"]);
    assert_eq!(listed(kind::KEEPS_ARCHETYPE), ["Artists/Somebody/80s Lead"]);
    let warm = rig.preset(kind::KEEPS_SERUM, "PD - Warm Room");
    assert_eq!(
        (warm.author.as_str(), warm.id.len(), warm.tags.as_slice()),
        ("Level 8", 10, &["pad".to_string(), "preview".to_string()][..])
    );
    // They are searched like any presets.
    let query = trill_core::presets::PresetQuery { text: "warm", category: Some("pad"), offset: 0, limit: 5 };
    assert_eq!(rig.presets(kind::KEEPS_SERUM).search(&query).total, 1);
}

#[test]
fn a_serum_preset_reaches_both_halves_of_the_running_plugin() {
    let rig = Rig::new();
    let wavetable: Vec<u8> = [0x58, 0x40].into_iter().chain(std::iter::repeat_n(3u8, 64)).collect();
    let file = serum::preset_file("PD - Warm Room", "Level 8", 4.0, &[("Oscillator0", &wavetable)]);
    rig.dir.file(&format!("{SERUM}/Factory/Pad/PD - Warm Room.SerumPreset"), &file);
    let preset = rig.preset(kind::KEEPS_SERUM, "Factory/Pad/PD - Warm Room");

    let mut instance = rig.create(kind::KEEPS_SERUM, None);
    assert!(state_of(&mut instance).is_empty(), "this plugin starts with nothing kept");
    load(&mut instance, &preset).unwrap();
    assert_eq!(note(&mut instance), None, "a synth has nothing to read afterwards");

    let state = state_of(&mut instance);
    for (half, bytes) in [("processor", &state.component), ("controller", &state.controller)] {
        let container = xfer::unpack(bytes).unwrap();
        assert_eq!(xfer::field(&container.header, "component"), half);
        let entries = super::cbor::entries(&container.body).unwrap();
        assert_eq!(entries[0].value, wavetable, "the patch arrived byte for byte");
    }
    assert_eq!(serum::shown(&state, "presetName").as_deref(), Some("PD - Warm Room"));
}

#[test]
fn an_ample_preset_is_laid_over_what_the_running_plugin_has() {
    let rig = Rig::new();
    rig.dir.file("tune/Tune_Default.AMHBg", b"");
    rig.dir.file("tune/Tune_Default.AMHNk", b"");
    let tune = rig.dir.path().join("tune");
    let ours = [("M-Master Vol", "0.9"), ("Auto Legato", "1"), ("Only Ours", "7")];
    let started = ample::state(&ample::document("Default", "4.1.0", "AMHBg", &tune, &ours));
    let theirs = [("M-Master Vol", "0.8"), ("Auto Legato", "1")];
    let elsewhere = PathBuf::from("C:\\Users\\User\\Documents\\Tune");
    let preset = ample::document("Jazz Neck", "4.0.0", "AMHNk", &elsewhere, &theirs);
    rig.dir.file(&format!("{AMPLE}/Jazz Neck.amhstate"), preset.as_bytes());

    let mut instance = rig.create(kind::KEEPS_AMPLE, Some(&started));
    assert_eq!(state_of(&mut instance), started, "the plugin starts from the state it was given");
    load(&mut instance, &rig.preset(kind::KEEPS_AMPLE, "Jazz Neck")).unwrap();
    let state = state_of(&mut instance);
    assert_eq!(ample::shown(&state, "M-Master Vol").as_deref(), Some("0.8"));
    assert_eq!(ample::shown(&state, "Only Ours").as_deref(), Some("7"));
    assert_eq!(ample::shown(&state, ample::LIBRARY).as_deref(), Some("AMHNk"));
    // Other samples take a while to read, and the preset has hammer-ons
    // that nobody asked for switched on: whoever asked is told once.
    let said = note(&mut instance).unwrap();
    assert!(said.contains("reads those samples in the background"), "{said}");
    assert!(said.contains("The preset has \"Auto Legato\" at 1"), "{said}");
    assert_eq!(note(&mut instance), None);
}

#[test]
fn a_kit_and_an_amplifier_preset_are_loaded_the_same_way() {
    let rig = Rig::new();
    rig.dir.file(
        SUPERIOR,
        &superior::presets_file(&[("Ayotte Default", "Clean Kits", "Kick2"), ("Death Metal", "Metal", "Kick62")]),
    );
    let mut drums = rig.create(kind::KEEPS_SUPERIOR, Some(&superior::state("Kick1")));
    load(&mut drums, &rig.preset(kind::KEEPS_SUPERIOR, "Death Metal")).unwrap();
    assert_eq!(superior::shown(&state_of(&mut drums), "drum 0").as_deref(), Some("drum \"Kick62\" \"Felt\" 1"));
    assert!(note(&mut drums).unwrap().contains("reads the kit's samples in the background"));

    let settings = [("gateThreshold", "-62.5"), ("doublerActive", "true")];
    rig.dir.file(&format!("{ARCHETYPE}/User/Lead.xml"), &archetype::preset_file("Lead", "0", &settings));
    let started = archetype::state(&[("gateThreshold", "-80"), ("doublerActive", "false"), ("gateLocked", "false")]);
    let mut amplifier = rig.create(kind::KEEPS_ARCHETYPE, Some(&started));
    load(&mut amplifier, &rig.preset(kind::KEEPS_ARCHETYPE, "User/Lead")).unwrap();
    let state = state_of(&mut amplifier);
    assert_eq!(archetype::shown(&state, "appModel/subModels/parameters@doublerActive").as_deref(), Some("true"));
    assert_eq!(archetype::shown(&state, "appModel/subModels/parameters@gateLocked").as_deref(), Some("false"));
    assert_eq!(archetype::shown(&state, "appModel@selectedAmp").as_deref(), Some("0"));
    assert_eq!(note(&mut amplifier), None);
}

#[test]
fn a_preset_that_cannot_be_loaded_says_why_and_leaves_the_plugin_as_it_was() {
    let rig = Rig::new();
    let tune = rig.dir.path().join("tune");
    let started = ample::state(&ample::document(
        "Default",
        "4.1.0",
        "AMHBg",
        &tune,
        &[("M-Master Vol", "0.9"), ("Amp", "Lead 800")],
    ));
    let file = |name: &str, settings: &[(&str, &str)]| {
        let document = ample::document(name, "4.0.0", "AMHBg", &tune, settings);
        rig.dir.file(&format!("{AMPLE}/{name}.amhstate"), document.as_bytes())
    };
    let gone = file("Gone", &[("M-Master Vol", "0.5")]);
    let damaged = file("Damaged", &[("M-Master Vol", "0.5")]);
    file("Old", &[("M-Master Vol", "0.5")]);
    file("Refused", &[("Amp", "refuse-this")]);
    file("Ignored", &[("Amp", "ignore-this")]);
    let presets: Vec<PresetInfo> =
        ["Gone", "Damaged", "Old", "Refused", "Ignored"].map(|name| rig.preset(kind::KEEPS_AMPLE, name)).into();
    // After they were listed: one file is deleted, one is overwritten
    // with something else, and one with settings the plugin has none of.
    std::fs::remove_file(&gone).unwrap();
    std::fs::write(&damaged, b"<AMG_STATE Ver=\"4.0.0\"><Parameter Name=\"x\" Value=\"1\"/><Broken").unwrap();
    rig.dir.file(
        &format!("{AMPLE}/Old.amhstate"),
        ample::document("Old", "2.0.0", "AMHBg", &tune, &[("Wah", "1"), ("Fuzz", "2")]).as_bytes(),
    );

    let mut instance = rig.create(kind::KEEPS_AMPLE, Some(&started));
    let reasons = [
        "Gone.amhstate' cannot be read: ",
        "Damaged.amhstate' cannot be loaded: the tag 'Broken' is not closed",
        "Old.amhstate' cannot be loaded: none of its settings is one this plugin has",
        "the plugin rejected the preset's state",
        "the plugin was handed the preset and did not take it: its state does not show 1 of the 1 settings the preset gives it ('Amp' among them)",
    ];
    for (preset, reason) in presets.iter().zip(reasons) {
        let error = load(&mut instance, preset).unwrap_err();
        assert!(matches!(&error, PresetLoadError::Failed(said) if said.contains(reason)), "{}: {error}", preset.name);
        assert_eq!(state_of(&mut instance), started, "{}: the plugin is as it was", preset.name);
        assert_eq!(note(&mut instance), None);
    }
    // What is no preset of this plugin at all.
    let other = PresetInfo { locator: "nonsense".into(), ..presets[0].clone() };
    assert!(
        matches!(load(&mut instance, &other), Err(PresetLoadError::Failed(said)) if said.contains("not a preset of a VST3 plugin"))
    );
}

#[test]
fn a_session_loads_a_vendors_preset_as_one_undo_step() {
    let rig = Rig::new();
    let tune = rig.dir.path().join("tune");
    for (name, settings) in [("Quiet", [("M-Master Vol", "0.3")]), ("Refused", [("Amp", "refuse-this")])] {
        let document = ample::document(name, "4.0.0", "AMHBg", &tune, &settings);
        rig.dir.file(&format!("{AMPLE}/{name}.amhstate"), document.as_bytes());
    }
    let uri = scripted::uri(kind::KEEPS_AMPLE);
    assert!(uri.starts_with(VST3_SCHEME));
    let factory = CompositeFactory::new(vec![Box::new(rig.factory.clone())]);
    let mut session = Session::new(Arc::new(factory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    // The plugin as it comes: its master volume at 0.9.
    let device = session.add_device(ChainRef::Master, &uri, None, None).unwrap();
    there(&mut session);
    let volume = |session: &mut Session| {
        session.snapshot_json();
        let blob = session.project().device(device).unwrap().state.clone().unwrap();
        ample::shown(&state::decode(&blob).unwrap().1, "M-Master Vol").unwrap()
    };
    assert_eq!(volume(&mut session), "0.9");

    // By its name, as any preset is asked for; and what the plugin then
    // holds is what the project is saved with.
    rig.factory.presets_within(&uri, WAIT);
    let loaded = session.load_preset(device, "quiet").unwrap();
    assert_eq!(
        (loaded.path().as_str(), session.device_preset(device).map(|preset| preset.name.as_str())),
        ("Metal/Quiet", Some("Quiet"))
    );
    assert_eq!(volume(&mut session), "0.3");
    assert!(session.undo());
    there(&mut session);
    assert_eq!((volume(&mut session), session.device_preset(device)), ("0.9".to_string(), None));
    assert!(session.redo());
    there(&mut session);
    assert_eq!(volume(&mut session), "0.3");

    // One the plugin will not take: a sentence that names the preset, and
    // the sound it had.
    let error = session.load_preset(device, "Refused").unwrap_err().to_string();
    assert!(
        error.contains("did not load the preset 'Refused'") && error.contains("the plugin rejected the preset's state"),
        "{error}"
    );
    assert!(error.contains("The device is as it was"), "{error}");
    assert_eq!(volume(&mut session), "0.3");
    assert_eq!(session.device_preset(device).map(|preset| preset.name.as_str()), Some("Quiet"));
    // A device is added with one, or not at all.
    let second = session.add_device(ChainRef::Master, &uri, Some("Quiet"), None).unwrap();
    there(&mut session);
    assert_eq!(session.device_preset(second).map(|preset| preset.name.as_str()), Some("Quiet"));
    // One added with a preset the plugin refuses is in the song, and that
    // it did not load what it was to start from is said when it has been
    // made; taken back, nothing is left of it.
    let devices = session.project().devices().count();
    let before = session.problems().len();
    let refused = session.add_device(ChainRef::Master, &uri, Some("Refused"), None).unwrap();
    there(&mut session);
    let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
    let said = problems.iter().filter(|problem| problem.contains("did not load the preset 'Refused'")).count();
    assert!(problems.len() == before + 1 && said == 1, "{problems:?}");
    assert!(session.undo());
    there(&mut session);
    assert!(session.project().device(refused).is_err() && session.problems().len() == before);
    assert_eq!(session.project().devices().count(), devices);
}

#[test]
fn a_setting_no_parameter_reaches_is_set_in_the_running_plugin_as_an_undo_step() {
    let rig = Rig::new();
    let uri = scripted::uri(kind::KEEPS_AMPLE);
    let factory = CompositeFactory::new(vec![Box::new(rig.factory.clone())]);
    let mut session = Session::new(Arc::new(factory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    let device = session.add_device(ChainRef::Master, &uri, None, None).unwrap();
    there(&mut session);
    let legato = |session: &Session| {
        let settings = session.device_settings(device).unwrap();
        settings.into_iter().find(|(name, _)| name == "Auto Legato").map(|(_, value)| value)
    };
    // The plugin's state lists its settings; none of them is a parameter
    // of this one, so all are settings no parameter reaches.
    let names: Vec<String> = session.device_settings(device).unwrap().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["M-Master Vol", "Auto Legato", "Amp", ample::LIBRARY]);
    assert_eq!(legato(&session).as_deref(), Some("1"));

    let step = session.undo_step();
    session.set_device_settings(device, &[("Auto Legato".to_string(), "0".to_string())]).unwrap();
    assert_eq!(legato(&session).as_deref(), Some("0"));
    assert_ne!(session.undo_step(), step);
    assert_eq!(session.take_preset_note(device), None);
    assert!(session.undo());
    there(&mut session);
    assert_eq!(legato(&session).as_deref(), Some("1"));
    assert!(session.redo());
    there(&mut session);
    assert_eq!(legato(&session).as_deref(), Some("0"));

    // A name that is no setting, and a set of samples that is not here:
    // a sentence, and the plugin as it was.
    let step = session.undo_step();
    let error =
        session.set_device_settings(device, &[("Legato".to_string(), "1".to_string())]).unwrap_err().to_string();
    assert!(
        error.contains("was not given the settings: the plugin's state has no setting 'Legato'; near it: Auto Legato"),
        "{error}"
    );
    let error = session.set_device_settings(device, &[(ample::LIBRARY.to_string(), "AMHNk".to_string())]).unwrap_err();
    assert!(error.to_string().contains("the set of samples 'AMHNk' is not installed"), "{error}");
    assert_eq!((legato(&session).as_deref(), session.undo_step()), (Some("0"), step));
    // A plugin whose state is not a list of named settings has none.
    let synth = session.add_device(ChainRef::Master, &scripted::uri(kind::KEEPS_SERUM), None, None).unwrap();
    there(&mut session);
    assert!(session.device_settings(synth).unwrap().is_empty());
    let error = session.set_device_settings(synth, &[("x".to_string(), "1".to_string())]).unwrap_err().to_string();
    assert!(error.contains("this plugin's state has no settings that are set by name"), "{error}");
}

// ---- a preset file that lies where nothing is listed from -----------------

/// What the plugin of `kind` makes of the file at `file`.
fn file_preset(rig: &Rig, kind: u8, file: &Path, within: Option<&str>) -> Result<PresetInfo, String> {
    rig.factory.preset_file(&scripted::uri(kind), file, within).expect("a plugin of this factory")
}

#[test]
fn a_makers_preset_file_is_a_preset_by_its_path_wherever_it_lies() {
    let rig = Rig::new();
    // Presets someone bought, kept with their samples.
    let bought = |name: &str, bytes: &[u8]| rig.dir.file(&format!("samples/Pack #1/{name}"), bytes);
    let saw = bought("OMEGA SAW.SerumPreset", &serum::preset_file("Omega Saw", "Somebody", 4.0, &[]));
    let tune = rig.dir.path().join("tune");
    let crunch = ample::document("Crunch", "4.0.0", "AMHBg", &tune, &[("M-Master Vol", "0.4")]);
    let tone = bought("Crunch.amhstate", crunch.as_bytes());
    let kits = bought(
        "kits",
        &superior::presets_file(&[("Ayotte Default", "Clean Kits", "Kick2"), ("Death Metal", "Metal", "Kick62")]),
    );
    let lead = bought("Lead.xml", &archetype::preset_file("Lead", "0", &[("gateThreshold", "-62.5")]));
    // None of them is among the presets a plugin lists.
    for kind in kind::KEEPERS {
        assert!(rig.presets(kind).is_empty());
    }

    // A synth's patch: what the file says of itself, and the sound.
    let preset = file_preset(&rig, kind::KEEPS_SERUM, &saw, None).unwrap();
    assert_eq!((preset.name.as_str(), preset.author.as_str(), preset.id.len()), ("Omega Saw", "Somebody", 10));
    assert_eq!(preset.tags, ["pad", "preview"]);
    let mut synth = rig.create(kind::KEEPS_SERUM, None);
    load(&mut synth, &preset).unwrap();
    assert_eq!(serum::shown(&state_of(&mut synth), "presetName").as_deref(), Some("Omega Saw"));

    // A guitar's tone, laid over what the running plugin has.
    let preset = file_preset(&rig, kind::KEEPS_AMPLE, &tone, None).unwrap();
    assert_eq!((preset.name.as_str(), preset.category.as_str()), ("Crunch", "Metal"));
    let mut guitar = rig.create(kind::KEEPS_AMPLE, None);
    load(&mut guitar, &preset).unwrap();
    assert_eq!(ample::shown(&state_of(&mut guitar), "M-Master Vol").as_deref(), Some("0.4"));

    // A file of kits holds several: one is named after the path.
    let error = file_preset(&rig, kind::KEEPS_SUPERIOR, &kits, None).unwrap_err();
    assert_eq!(
        error,
        format!(
            "it holds 2 presets (Clean Kits/Ayotte Default, Metal/Death Metal): say which with '{}#<name>'",
            kits.display()
        )
    );
    let preset = file_preset(&rig, kind::KEEPS_SUPERIOR, &kits, Some("death metal")).unwrap();
    assert_eq!(preset.path(), "Metal/Death Metal");
    let mut drums = rig.create(kind::KEEPS_SUPERIOR, Some(&superior::state("Kick1")));
    load(&mut drums, &preset).unwrap();
    assert_eq!(superior::shown(&state_of(&mut drums), "drum 0").as_deref(), Some("drum \"Kick62\" \"Felt\" 1"));
    assert!(note(&mut drums).unwrap().contains("reads the kit's samples in the background"));
    let error = file_preset(&rig, kind::KEEPS_SUPERIOR, &kits, Some("Jazz")).unwrap_err();
    assert_eq!(error, "it holds no preset 'Jazz' among its 2 (Clean Kits/Ayotte Default, Metal/Death Metal)");

    // An amplifier's preset.
    let preset = file_preset(&rig, kind::KEEPS_ARCHETYPE, &lead, None).unwrap();
    assert_eq!(preset.name, "Lead");
    let mut amplifier = rig.create(kind::KEEPS_ARCHETYPE, Some(&archetype::state(&[("gateThreshold", "-80")])));
    load(&mut amplifier, &preset).unwrap();
    let state = state_of(&mut amplifier);
    assert_eq!(archetype::shown(&state, "appModel/subModels/parameters@gateThreshold").as_deref(), Some("-62.5"));

    // A file that is where presets are listed from is the preset the
    // list has, with what the list says of it.
    let listed = rig.dir.file(
        &format!("{SERUM}/Factory/Pad/PD - Warm Room.SerumPreset"),
        &serum::preset_file("PD - Warm Room", "Level 8", 4.0, &[]),
    );
    rig.factory.refresh_presets(&scripted::uri(kind::KEEPS_SERUM));
    let by_name = rig.preset(kind::KEEPS_SERUM, "PD - Warm Room");
    assert_eq!(file_preset(&rig, kind::KEEPS_SERUM, &listed, None).unwrap(), by_name);
    assert_eq!(by_name.category, "Factory/Pad");

    // Which plugin a file is for is told by its name, where the name says.
    let uri = |kind: u8| Some(scripted::uri(kind));
    assert_eq!(rig.factory.preset_file_type(&saw), uri(kind::KEEPS_SERUM));
    assert_eq!(rig.factory.preset_file_type(&tone), uri(kind::KEEPS_AMPLE));
    assert_eq!((rig.factory.preset_file_type(&kits), rig.factory.preset_file_type(&lead)), (None, None));
    assert_eq!(rig.factory.preset_file_type(&rig.dir.path().join("samples/Kick.wav")), None);
}

#[test]
fn a_file_that_is_not_the_plugins_preset_says_what_it_is_instead() {
    let rig = Rig::new();
    let file = |name: &str, bytes: &[u8]| rig.dir.file(&format!("samples/{name}"), bytes);
    let tune = rig.dir.path().join("tune");
    let tone = file(
        "Crunch.amhstate",
        ample::document("Crunch", "4.0.0", "AMHBg", &tune, &[("M-Master Vol", "0.4")]).as_bytes(),
    );
    let effect = xfer::pack(
        serde_json::json!({ "fileType": "SerumPreset", "product": "Serum2FX" }).as_object().cloned().unwrap(),
        &[],
    );
    let effect = file("Wide.SerumPreset", &effect);
    let short = file("Short.SerumPreset", &serum::preset_file("Short", "", 5.0, &[])[..30]);
    let refused = |kind: u8, file: &Path| file_preset(&rig, kind, file, None).unwrap_err();
    let synth = "Keeper 20";

    // Another maker's file, with what this plugin does take.
    assert_eq!(
        refused(kind::KEEPS_SERUM, &tone),
        format!("it is not one of Xfer's files. {synth} takes a .SerumPreset file or a .vstpreset file")
    );
    // Its own maker's, of another product, and one that is cut short.
    assert_eq!(
        refused(kind::KEEPS_SERUM, &effect),
        "it is not a Serum 2 preset (it says it is a SerumPreset of Serum2FX)"
    );
    assert_eq!(refused(kind::KEEPS_SERUM, &short), "it is cut short");
    // What is no file.
    assert_eq!(refused(kind::KEEPS_SERUM, &rig.dir.path().join("samples/Gone.SerumPreset")), "there is no such file");
    assert_eq!(refused(kind::KEEPS_SERUM, &rig.dir.path().join("samples")), "it is a folder, not a file");
    // The other makers' readers say what they miss.
    assert_eq!(
        refused(kind::KEEPS_AMPLE, &effect),
        "it is not one of Ample Sound's documents of settings. Keeper 21 takes a preset file of the instrument's \
         own or a .vstpreset file"
    );
    assert!(refused(kind::KEEPS_SUPERIOR, &effect).starts_with("it holds no presets of Superior Drummer's. "));
    assert!(refused(kind::KEEPS_ARCHETYPE, &tone).starts_with("it is not a preset of one of Neural DSP's Archetypes"));

    // A plugin of another format is not this factory's to answer for.
    assert!(rig.factory.preset_file("clap:com.example.synth", &tone, None).is_none());
    assert_eq!(
        rig.factory.preset_file("vst3:00000000000000000000000000000001", &tone, None),
        Some(Err("the plugin is not installed".to_string()))
    );

    // A preset of an instrument that is not the one it is given to shows
    // when it is loaded, and the plugin is as it was.
    let mut guitar = rig.create(kind::KEEPS_AMPLE, None);
    let before = state_of(&mut guitar);
    let bass = file(
        "Slap.amhstate",
        ample::document("Slap", "4.0.0", "ABJ", &tune, &[("M-Master Vol", "0.4")])
            .replace("AMG_STATE", "AEB_STATE")
            .as_bytes(),
    );
    let preset = file_preset(&rig, kind::KEEPS_AMPLE, &bass, None).unwrap();
    let error = load(&mut guitar, &preset).unwrap_err();
    assert!(
        matches!(&error, PresetLoadError::Failed(said) if said.ends_with("it is a preset of another instrument (its settings are 'AEB_STATE')")),
        "{error}"
    );
    assert_eq!(state_of(&mut guitar), before);
}

#[test]
fn a_vst3_preset_file_is_any_plugins_and_is_seen_to_arrive() {
    let rig = Rig::new();
    let tune = rig.dir.path().join("tune");
    let state_with =
        |settings: &[(&str, &str)]| ample::state(&ample::document("Default", "4.1.0", "AMHBg", &tune, settings));
    let file = |name: &str, kind: u8, state: &PluginState| {
        rig.dir.file(&format!("samples/{name}.vstpreset"), &state::encode(class(kind), state))
    };
    let started = state_with(&[("M-Master Vol", "0.9"), ("Amp", "Lead 800")]);
    let quiet = state_with(&[("M-Master Vol", "0.2"), ("Amp", "Clean")]);
    let live = file("From Live", kind::KEEPS_AMPLE, &quiet);

    // The state another host saved, as the file it saved it in.
    let preset = file_preset(&rig, kind::KEEPS_AMPLE, &live, None).unwrap();
    assert_eq!((preset.name.as_str(), preset.id.len()), ("From Live", 10));
    let mut guitar = rig.create(kind::KEEPS_AMPLE, Some(&started));
    load(&mut guitar, &preset).unwrap();
    assert_eq!(state_of(&mut guitar), quiet, "setting for setting");
    assert_eq!(note(&mut guitar), None);
    // Given again, there is nothing to arrive and nothing to say.
    load(&mut guitar, &preset).unwrap();
    assert_eq!(note(&mut guitar), None);

    // A plugin that says yes to a file and is as it was: whoever asked
    // is told that it was not seen to change.
    let ignored = file("Ignored", kind::KEEPS_AMPLE, &state_with(&[("Amp", "ignore-this")]));
    load(&mut guitar, &file_preset(&rig, kind::KEEPS_AMPLE, &ignored, None).unwrap()).unwrap();
    assert_eq!(state_of(&mut guitar), quiet);
    assert!(
        note(&mut guitar)
            .unwrap()
            .starts_with("The plugin took the file without complaint, but its state was the same")
    );
    assert_eq!(note(&mut guitar), None, "said once");
    // One it turns down.
    let refused = file("Refused", kind::KEEPS_AMPLE, &state_with(&[("Amp", "refuse-this")]));
    let error = load(&mut guitar, &file_preset(&rig, kind::KEEPS_AMPLE, &refused, None).unwrap()).unwrap_err();
    assert!(
        matches!(&error, PresetLoadError::Failed(said) if said.starts_with("the plugin rejected its preset")),
        "{error}"
    );
    assert_eq!(state_of(&mut guitar), quiet);

    // Another plugin's file names the plugin; what is no preset file says so.
    let theirs = file("Theirs", kind::KEEPS_ARCHETYPE, &quiet);
    assert_eq!(
        file_preset(&rig, kind::KEEPS_AMPLE, &theirs, None).unwrap_err(),
        "it is a preset of Archetype Test X, not of Keeper 21"
    );
    let nobodys = rig.dir.file(
        "samples/Nobody.vstpreset",
        &state::encode(ClassId::parse("00000000000000000000000000000001").unwrap(), &quiet),
    );
    assert_eq!(
        file_preset(&rig, kind::KEEPS_AMPLE, &nobodys, None).unwrap_err(),
        "it is a preset of another plugin (class 00000000000000000000000000000001), not of Keeper 21"
    );
    let text = rig.dir.file("samples/Text.vstpreset", b"not a preset file at all");
    assert_eq!(
        file_preset(&rig, kind::KEEPS_AMPLE, &text, None).unwrap_err(),
        "it is not a VST3 preset file (it does not begin as one does)"
    );
    // Whose a file is, is in its first bytes.
    assert_eq!(rig.factory.preset_file_type(&live), Some(scripted::uri(kind::KEEPS_AMPLE)));
    assert_eq!(rig.factory.preset_file_type(&theirs), Some(scripted::uri(kind::KEEPS_ARCHETYPE)));
    assert_eq!((rig.factory.preset_file_type(&nobodys), rig.factory.preset_file_type(&text)), (None, None));
}

#[test]
fn a_session_gives_a_device_a_preset_file_as_one_undo_step() {
    let rig = Rig::new();
    let tune = rig.dir.path().join("tune");
    let document = |name: &str, settings: &[(&str, &str)]| ample::document(name, "4.0.0", "AMHBg", &tune, settings);
    let quiet = rig.dir.file("samples/Quiet.amhstate", document("Quiet", &[("M-Master Vol", "0.3")]).as_bytes());
    let refused = rig.dir.file("samples/Refused.amhstate", document("Refused", &[("Amp", "refuse-this")]).as_bytes());
    let (quiet, refused) = (quiet.to_string_lossy().into_owned(), refused.to_string_lossy().into_owned());
    let uri = scripted::uri(kind::KEEPS_AMPLE);
    let factory = CompositeFactory::new(vec![Box::new(rig.factory.clone())]);
    let mut session = Session::new(Arc::new(factory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    let device = session.add_device(ChainRef::Master, &uri, None, None).unwrap();
    there(&mut session);
    let volume = |session: &mut Session, device| {
        session.snapshot_json();
        let blob = session.project().device(device).unwrap().state.clone().unwrap();
        ample::shown(&state::decode(&blob).unwrap().1, "M-Master Vol").unwrap()
    };
    assert_eq!(volume(&mut session, device), "0.9");

    // By its path, through the door a name goes through.
    let loaded = session.load_preset(device, &quiet).unwrap();
    assert_eq!((loaded.path().as_str(), volume(&mut session, device).as_str()), ("Metal/Quiet", "0.3"));
    assert_eq!(session.device_preset(device).map(|preset| preset.name.as_str()), Some("Quiet"));
    assert!(session.undo());
    there(&mut session);
    assert_eq!((volume(&mut session, device), session.device_preset(device)), ("0.9".to_string(), None));
    assert!(session.redo());
    there(&mut session);
    assert_eq!(volume(&mut session, device), "0.3");

    // One the plugin will not take, and a file that is no preset of it:
    // a sentence that names the file, and the sound the device had.
    let step = session.undo_step();
    let error = session.load_preset(device, &refused).unwrap_err().to_string();
    assert!(
        error.contains("did not load the preset 'Refused'") && error.contains("the plugin rejected the preset's state"),
        "{error}"
    );
    let wav = rig.dir.file("samples/Kick.wav", b"RIFF").to_string_lossy().into_owned();
    let error = session.load_preset(device, &wav).unwrap_err().to_string();
    assert!(
        error.starts_with(&format!("'{wav}' cannot be given to '{uri}' as a preset: it is not one of Ample")),
        "{error}"
    );
    assert_eq!((volume(&mut session, device).as_str(), session.undo_step()), ("0.3", step));

    // A device is added with one, or not at all.
    let second = session.add_device(ChainRef::Master, &uri, Some(&quiet), None).unwrap();
    there(&mut session);
    assert_eq!(volume(&mut session, second), "0.3");
    let devices = session.project().devices().count();
    let before = session.problems().len();
    let unloaded = session.add_device(ChainRef::Master, &uri, Some(&refused), None).unwrap();
    there(&mut session);
    let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
    let said = problems.iter().filter(|problem| problem.contains("did not load the preset")).count();
    assert!(problems.len() == before + 1 && said == 1, "{problems:?}");
    assert!(session.undo());
    there(&mut session);
    assert!(session.project().device(unloaded).is_err() && session.problems().len() == before);
    assert_eq!(session.project().devices().count(), devices);
}
