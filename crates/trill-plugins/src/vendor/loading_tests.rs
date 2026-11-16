//! The vendors' presets through the host, end to end: listed by a
//! factory with a plugin's other presets, and loaded into a running
//! plugin by its controller, and by a session as an undo step.
//!
//! The plugins are scripted ones that have the vendors' class ids and
//! keep whatever state they are handed (`vst3::scripted::kind::KEEPERS`),
//! and the preset files are made here, in the formats: nothing of a
//! vendor's is read. The real plugins are in `tests/vendor.rs`.

use super::{VendorPlaces, ample, archetype, serum, superior, xfer};
use crate::test_support::TestDir;
use crate::vst3::class_id::ClassId;
use crate::vst3::scripted::{self, kind};
use crate::vst3::state::{self, PluginState};
use crate::vst3::{Vst3Config, Vst3Factory};
use crate::{Scanner, VST3_SCHEME};
use std::path::PathBuf;
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
    // Other samples take a while to read, and whoever asked is told once.
    assert!(note(&mut instance).unwrap().contains("reads those samples in the background"));
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
    assert_eq!((volume(&mut session), session.device_preset(device)), ("0.9".to_string(), None));
    assert!(session.redo());
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
    assert_eq!(session.device_preset(second).map(|preset| preset.name.as_str()), Some("Quiet"));
    let devices = session.project().devices().count();
    assert!(session.add_device(ChainRef::Master, &uri, Some("Refused"), None).is_err());
    assert_eq!(session.project().devices().count(), devices);
}

#[test]
fn a_setting_no_parameter_reaches_is_set_in_the_running_plugin_as_an_undo_step() {
    let rig = Rig::new();
    let uri = scripted::uri(kind::KEEPS_AMPLE);
    let factory = CompositeFactory::new(vec![Box::new(rig.factory.clone())]);
    let mut session = Session::new(Arc::new(factory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    let device = session.add_device(ChainRef::Master, &uri, None, None).unwrap();
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
    assert_eq!(legato(&session).as_deref(), Some("1"));
    assert!(session.redo());
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
    assert!(session.device_settings(synth).unwrap().is_empty());
    let error = session.set_device_settings(synth, &[("x".to_string(), "1".to_string())]).unwrap_err().to_string();
    assert!(error.contains("this plugin's state has no settings that are set by name"), "{error}");
}
