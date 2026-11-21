//! Presets against the scripted plugins of [`super::scripted`]: what a
//! plugin's providers tell of is listed, and a running plugin loads what
//! is listed. The integration tests do the same with real plugins.

use super::ClapFactory;
use super::scripted::{self, GAIN, kind};
use crate::presets::Locator;
use crate::test_support::{eventually, there};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{
    CompositeFactory, DeviceFactory, DeviceInstance, Event, EventKind, PresetLoadError, ProcessInfo,
};
use trill_core::presets::{PresetInfo, PresetList, PresetLookup, PresetStatus};
use trill_core::{ChainRef, DeviceId, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::render::RenderOptions;
use trill_engine::{AudioBackend, Session};

const SR: f64 = 48_000.0;
const BLOCK: usize = 64;
const WAIT: Duration = Duration::from_secs(60);

fn factory() -> ClapFactory {
    let kinds = [kind::PRESETS, kind::BROKEN_PRESETS, kind::IMPULSE];
    ClapFactory::with_entries(&kinds.map(scripted::entry))
}

fn uri(kind: u8) -> String {
    format!("clap:{}", scripted::id(kind))
}

/// The presets of a scripted plugin, once they have been read.
fn listed(factory: &ClapFactory, kind: u8) -> PresetList {
    let list = factory.presets_within(&uri(kind), WAIT).expect("a CLAP plugin");
    assert!(!list.is_reading(), "the presets were not read within {WAIT:?}");
    list
}

fn preset<'a>(list: &'a PresetList, wanted: &str) -> &'a PresetInfo {
    match list.find(wanted) {
        PresetLookup::Found(preset) => preset,
        other => panic!("'{wanted}': {other:?}"),
    }
}

fn create(factory: &ClapFactory, kind: u8) -> DeviceInstance {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, uri(kind), "plugin", None).unwrap();
    factory.create(project.device(id).unwrap(), SR, BLOCK).unwrap_or_else(|error| panic!("{error}"))
}

/// The gain the plugin reports, which is all a scripted preset sets.
fn gain(instance: &mut DeviceInstance) -> f64 {
    let index = instance.params.iter().position(|param| param.id == GAIN.to_string()).expect("a gain");
    instance.controller.as_mut().unwrap().param_values().expect("the plugin reports values")[index]
}

fn load(instance: &mut DeviceInstance, preset: &PresetInfo) -> Result<(), PresetLoadError> {
    instance.controller.as_mut().unwrap().load_preset(preset)
}

/// What a note at full velocity comes out as: the plugin's gain.
fn impulse(instance: &mut DeviceInstance) -> f32 {
    let info = ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    };
    let (mut left, mut right) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let note = Event::new(5, EventKind::NoteOn { pitch: 60, velocity: 1.0 });
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
fn a_plugins_providers_tell_of_its_presets_wherever_they_are() {
    let factory = factory();
    let list = listed(&factory, kind::PRESETS);
    assert_eq!(list.status, PresetStatus::Ready);
    let paths: Vec<String> = list.presets.iter().map(PresetInfo::path).collect();
    assert_eq!(
        paths,
        [
            // The files of the folder the provider declared, then its
            // subfolders; the preset for another plugin is left out.
            "Factory/Bass",
            // A bank: one file, two presets, filed under their sound pack.
            "Deep Space/Kick",
            "Deep Space/Snare",
            "Factory/Mute",
            "Factory/Rotten",
            "Factory/Pads/Warm",
            "Factory/Pads/Soft/Air",
            // A file the provider failed on, and read when it was asked
            // again after the rest.
            "Factory/Flaky",
            // The presets that are part of the plugin.
            "Built in/Quiet",
            "Built in/Loud",
        ]
    );

    let warm = preset(&list, "Pads/Warm");
    assert_eq!((warm.author.as_str(), warm.description.as_str()), ("Ada, Bo", "A warm one."));
    assert_eq!(warm.tags, ["pad", "factory"]);
    let file = scripted::preset_folder().join("Pads").join("Warm.patch");
    assert_eq!(Locator::parse(&warm.locator), Some(Locator::ClapFile { path: file, key: None }));

    let kick = preset(&list, "kick");
    assert_eq!(kick.tags, ["user", "favorite"], "its own flags, not the folder's");
    let bank = scripted::preset_folder().join("Drums.bank");
    assert_eq!(Locator::parse(&kick.locator), Some(Locator::ClapFile { path: bank, key: Some("Kick".into()) }));

    let quiet = preset(&list, "Built in/Quiet");
    assert_eq!(quiet.tags, ["synth"]);
    assert_eq!(Locator::parse(&quiet.locator), Some(Locator::ClapPlugin { key: Some("quiet".into()) }));

    // Every preset has an id of its own to be asked for by.
    let mut ids: Vec<&str> = list.presets.iter().map(|preset| preset.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 10);
    assert!(ids.iter().all(|id| id.len() == 10));
    assert_eq!(preset(&list, &warm.id), warm);

    // The other plugin of the same file is told of the same presets, and
    // asking again reads nothing again.
    assert_eq!(listed(&factory, kind::LATE_PRESETS).len(), 10);
    assert_eq!(factory.presets(&uri(kind::PRESETS)), Some(list));
}

#[test]
fn the_first_request_answers_at_once_and_a_refresh_keeps_the_list() {
    let factory = factory();
    let started = Instant::now();
    let first = factory.presets(&uri(kind::PRESETS)).expect("a CLAP plugin");
    assert!(started.elapsed() < Duration::from_secs(1), "asking took {:?}", started.elapsed());
    // Read already, by another test's request for the same plugin file,
    // or being read: never anything in between.
    assert!(first.is_reading() || first.len() == 10, "{first:?}");
    assert_eq!(listed(&factory, kind::PRESETS).len(), 10);

    factory.refresh_presets(&uri(kind::PRESETS));
    assert_eq!(factory.presets(&uri(kind::PRESETS)).unwrap().len(), 10, "listed while it is read again");
    assert_eq!(listed(&factory, kind::PRESETS).status, PresetStatus::Ready);
}

#[test]
fn a_provider_that_fails_costs_a_message_and_not_the_presets() {
    let factory = factory();
    let list = listed(&factory, kind::BROKEN_PRESETS);
    match &list.status {
        PresetStatus::Failed(reason) => {
            assert!(reason.contains("'test.presets.broken' did not start"), "{reason}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert_eq!(list.len(), 10, "what the working provider told of is listed");
}

#[test]
fn plugins_without_providers_have_no_presets_and_other_formats_are_not_answered_for() {
    let factory = factory();
    assert_eq!(listed(&factory, kind::IMPULSE), PresetList::default());
    assert_eq!(factory.presets_within("clap:not.installed", WAIT), Some(PresetList::default()));
    for other in ["vst3:ABCDEF019182FAEB5776737957767331", "builtin:polysynth", "no-scheme"] {
        assert_eq!(factory.presets(other), None, "{other}");
        assert_eq!(factory.presets_within(other, WAIT), None, "{other}");
        factory.refresh_presets(other);
    }
}

// ---- loading -------------------------------------------------------------

#[test]
fn a_running_plugin_loads_a_preset_from_a_file_a_bank_or_itself() {
    let factory = factory();
    let list = listed(&factory, kind::PRESETS);
    let mut instance = create(&factory, kind::PRESETS);
    assert_eq!((gain(&mut instance), impulse(&mut instance)), (1.0, 1.0));

    for (wanted, expected) in [("Pads/Warm", 0.75), ("Snare", 0.375), ("Loud", 1.5), ("Pads/Soft/Air", 0.25)] {
        load(&mut instance, preset(&list, wanted)).unwrap_or_else(|error| panic!("{wanted}: {error}"));
        assert_eq!(gain(&mut instance), expected, "{wanted}");
        assert_eq!(impulse(&mut instance), expected as f32, "{wanted}: it is heard");
    }
    // What a project saves is the preset: a plugin created from it has it.
    let state = instance.controller.as_mut().unwrap().save_state();
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, uri(kind::PRESETS), "plugin", None).unwrap();
    project.device_mut(id).unwrap().state = state;
    let mut restored = factory.create(project.device(id).unwrap(), SR, BLOCK).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(impulse(&mut restored), 0.25);
}

#[test]
fn a_preset_that_does_not_load_says_why_and_changes_nothing() {
    let factory = factory();
    let list = listed(&factory, kind::PRESETS);
    let mut instance = create(&factory, kind::PRESETS);
    load(&mut instance, preset(&list, "Bass")).unwrap();

    // The plugin's own words where it gave any.
    let reason = failure(load(&mut instance, preset(&list, "Rotten")));
    assert_eq!(reason, "the plugin could not load it: the patch is rotten");
    let reason = failure(load(&mut instance, preset(&list, "Mute")));
    assert_eq!(reason, "the plugin refused it without saying why");
    // What it said about one preset is not said about the next.
    let reason = failure(load(&mut instance, preset(&list, "Mute")));
    assert_eq!(reason, "the plugin refused it without saying why");

    // A file that has gone since it was listed: the system's reason.
    let gone = Locator::ClapFile { path: scripted::preset_folder().join("gone.patch"), key: None };
    let gone = PresetInfo { name: "Gone".into(), locator: gone.to_text(), ..PresetInfo::default() };
    let reason = failure(load(&mut instance, &gone));
    assert!(reason.starts_with("the plugin could not load it: ") && reason.len() > 35, "{reason}");
    let nothing = Locator::ClapPlugin { key: Some("no such key".into()) };
    let nothing = PresetInfo { locator: nothing.to_text(), ..PresetInfo::default() };
    assert!(failure(load(&mut instance, &nothing)).ends_with("there is no such built-in preset"));

    // Presets that are not this format's.
    for locator in ["", "warm_pad", "vst3-file\nC:/a.vstpreset\n"] {
        let foreign = PresetInfo { locator: locator.into(), ..PresetInfo::default() };
        assert_eq!(failure(load(&mut instance, &foreign)), "it is not a preset of a CLAP plugin");
    }
    assert_eq!(gain(&mut instance), 0.5, "the preset before all that is still the one it has");

    // A plugin that cannot be given presets at all.
    let mut plain = create(&factory, kind::IMPULSE);
    let reason = failure(load(&mut plain, preset(&list, "Bass")));
    assert!(reason.contains("no CLAP preset-load extension"), "{reason}");
}

#[test]
fn a_plugin_that_takes_a_preset_on_with_its_next_block_is_waited_for_a_moment_and_no_longer() {
    let factory = factory();
    let list = listed(&factory, kind::LATE_PRESETS);
    let mut instance = create(&factory, kind::LATE_PRESETS);

    // Nothing is processing it, so the preset cannot arrive: the wait
    // for it ends by itself, and it is no failure.
    let started = Instant::now();
    load(&mut instance, preset(&list, "Bass")).unwrap();
    assert!(started.elapsed() < Duration::from_secs(20), "waited {:?}", started.elapsed());
    assert_eq!(gain(&mut instance), 1.0);
    // The block that brings it also plays with it.
    assert_eq!(impulse(&mut instance), 0.5);
    assert_eq!(gain(&mut instance), 0.5);
}

// ---- a preset file that lies anywhere ---------------------------------------

#[test]
fn a_preset_file_outside_the_plugins_folders_is_loaded_by_its_path() {
    let factory = factory();
    let dir = crate::test_support::TestDir::new();
    let bought = dir.file("samples/Pack/Bought Lead.patch", b"gain=0.125");
    let bank = dir.file(
        "samples/Pack/Bought.bank",
        b"Kick=0.625
Snare=0.375",
    );
    let other = dir.file("samples/Pack/Kick.wav", b"RIFF");
    let of = |kind: u8, file: &std::path::Path, within: Option<&str>| {
        factory.preset_file(&uri(kind), file, within).expect("a plugin of this factory")
    };
    let list = listed(&factory, kind::PRESETS);
    assert!(matches!(list.find("Bought Lead"), PresetLookup::Missing { .. }));

    // The file is the plugin's to read: it is told where the file is,
    // and with a bank which of its presets is meant.
    let lead = of(kind::PRESETS, &bought, None).unwrap();
    assert_eq!((lead.name.as_str(), lead.id.len()), ("Bought Lead", 10));
    assert_eq!(Locator::parse(&lead.locator), Some(Locator::ClapFile { path: bought.clone(), key: None }));
    let mut instance = create(&factory, kind::PRESETS);
    load(&mut instance, &lead).unwrap();
    assert_eq!((gain(&mut instance), impulse(&mut instance)), (0.125, 0.125));
    let snare = of(kind::PRESETS, &bank, Some("Snare")).unwrap();
    load(&mut instance, &snare).unwrap();
    assert_eq!(gain(&mut instance), 0.375);

    // A listed file's path is the listed preset; one of a listed bank's
    // is asked for as the list has it.
    let warm = scripted::preset_folder().join("Pads").join("Warm.patch");
    assert_eq!(&of(kind::PRESETS, &warm, None).unwrap(), preset(&list, "Pads/Warm"));
    let drums = scripted::preset_folder().join("Drums.bank");
    assert_eq!(&of(kind::PRESETS, &drums, Some("kick")).unwrap(), preset(&list, "kick"));
    let error = of(kind::PRESETS, &drums, None).unwrap_err();
    assert!(error.starts_with("it holds 2 presets (Deep Space/Kick, Deep Space/Snare): say which with '"), "{error}");

    // A kind of file the plugin keeps no presets in, and what is no file.
    let name = factory.descriptors().into_iter().find(|descriptor| descriptor.uri == uri(kind::PRESETS)).unwrap().name;
    assert_eq!(
        of(kind::PRESETS, &other, None).unwrap_err(),
        format!("it is not a kind of file {name} keeps presets in (its own are .bank, .patch)")
    );
    assert_eq!(of(kind::PRESETS, &dir.path().join("Gone.patch"), None).unwrap_err(), "there is no such file");
    assert!(factory.preset_file("vst3:00000000000000000000000000000001", &bought, None).is_none());
    assert_eq!(factory.preset_file_type(&bought), None, "whose a file is, only the plugin can say");

    // A file the plugin turns down says so in the plugin's words, and a
    // plugin that cannot be given presets says that.
    let rotten = dir.file("samples/Pack/Rotten.patch", b"rotten");
    let reason = failure(load(&mut instance, &of(kind::PRESETS, &rotten, None).unwrap()));
    assert_eq!(reason, "the plugin could not load it: the patch is rotten");
    assert_eq!(gain(&mut instance), 0.375, "it has what it had");
    let mut plain = create(&factory, kind::IMPULSE);
    let reason = failure(load(&mut plain, &of(kind::IMPULSE, &bought, None).unwrap()));
    assert!(reason.contains("no CLAP preset-load extension"), "{reason}");

    // Through a session: one undo step.
    let (mut session, track) = session(&factory);
    let device = session.add_device(ChainRef::Track(track), &uri(kind::PRESETS), None, None).unwrap();
    there(&mut session);
    let step = session.undo_step();
    assert_eq!(session.load_preset(device, &bought.to_string_lossy()).unwrap().name, "Bought Lead");
    assert_eq!(live_gain(&mut session, device), 0.125);
    assert_eq!(preset_name(&session, device).as_deref(), Some("Bought Lead"));
    assert!(session.undo());
    there(&mut session);
    assert_eq!((session.undo_step(), preset_name(&session, device)), (step, None));
    let within = format!("{}#Kick", bank.display());
    assert_eq!(session.load_preset(device, &within).unwrap().name, "Bought");
    assert_eq!(live_gain(&mut session, device), 0.625);
}

// ---- through the session ---------------------------------------------------

/// A session over the built-in devices and the scripted plugins, with a
/// track that plays one note at full velocity on beat 1, and nothing on
/// the master to come between that note and the output.
fn session(factory: &ClapFactory) -> (Session, trill_core::TrackId) {
    let devices = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(factory.clone())]);
    let mut session = Session::new(Arc::new(devices), AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let track = session
        .edit(|project| {
            let limiter = project.master.devices[0].id;
            project.remove_device(limiter)?;
            let track = project.add_track("Lead");
            let clip = project.add_midi_clip(track, "clip", 0.0, 4.0)?;
            project.clip_mut(clip)?.add_notes(&[Note {
                pitch: 60,
                start: 1.0,
                length: 1.0,
                velocity: 127,
                ..Default::default()
            }])?;
            Ok(track)
        })
        .unwrap();
    (session, track)
}

/// The sample the song's one note comes out as: the gain of whatever
/// preset the instrument has, in an instance made from the project. To
/// four places: the track's fader is a multiplication too.
fn rendered(session: &mut Session) -> f32 {
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.1, ..RenderOptions::default() };
    let out = session.render_job().render(&options).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    // Beat 1 at 120 BPM and 48 kHz.
    (out.left[24_000] * 1e4).round() / 1e4
}

/// The gain the running plugin behind a device has, read from the state
/// it saves: the four letters that mark a scripted state, then the gain.
fn live_gain(session: &mut Session, device: DeviceId) -> f64 {
    let state = session.render_job().project.device(device).unwrap().state.clone().expect("the plugin saves state");
    f64::from_le_bytes(state[4..12].try_into().unwrap())
}

fn preset_name(session: &Session, device: DeviceId) -> Option<String> {
    session.device_preset(device).map(|preset| preset.name.clone())
}

#[test]
fn the_session_lists_loads_undoes_and_saves_a_plugins_presets() {
    let factory = factory();
    listed(&factory, kind::PRESETS);
    let (mut session, track) = session(&factory);
    let uri = uri(kind::PRESETS);

    // Listed for the device type and for a device of it, like a built-in
    // device's.
    assert_eq!(session.presets(&uri).len(), 10);
    let device = session.add_device(ChainRef::Track(track), &uri, Some("Pads/Warm"), None).unwrap();
    there(&mut session);
    assert_eq!(session.device_presets(device).unwrap().len(), 10);
    assert_eq!(preset_name(&session, device).as_deref(), Some("Warm"));
    assert_eq!(rendered(&mut session), 0.75);

    // Another preset, in place: one step.
    let step = session.undo_step();
    let loaded = session.load_preset(device, "Pads/Soft/Air").unwrap();
    assert_eq!((loaded.name.as_str(), loaded.category.as_str()), ("Air", "Factory/Pads/Soft"));
    assert_eq!(rendered(&mut session), 0.25);
    assert!(session.undo());
    there(&mut session);
    assert_eq!(session.undo_step(), step);
    assert_eq!((rendered(&mut session), preset_name(&session, device).as_deref()), (0.75, Some("Warm")));
    assert!(session.redo());
    there(&mut session);
    assert_eq!((rendered(&mut session), preset_name(&session, device).as_deref()), (0.25, Some("Air")));

    // A preset that does not load: said, and nothing changes.
    let error = session.load_preset(device, "Rotten").unwrap_err().to_string();
    assert!(error.contains("did not load the preset 'Rotten'") && error.contains("the patch is rotten"), "{error}");
    assert!(error.contains("The device is as it was"), "{error}");
    assert_eq!((rendered(&mut session), preset_name(&session, device).as_deref()), (0.25, Some("Air")));
    // Added with such a preset: the song has the device at once, and when
    // it has been made, that it did not load what it was to start from is
    // a problem of the song, in a sentence. Taken back, nothing is left.
    let rotten = session.add_device(ChainRef::Track(track), &uri, Some("Rotten"), None).unwrap();
    there(&mut session);
    let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
    let said = |problem: &String| {
        problem.contains("did not load the preset 'Rotten' it was to start from")
            && problem.contains("the patch is rotten")
    };
    assert!(problems.len() == 1 && said(&problems[0]), "{problems:?}");
    assert!(session.undo());
    there(&mut session);
    assert!(session.project().device(rotten).is_err() && session.problems().is_empty());
    assert_eq!(session.project().track(track).unwrap().devices.len(), 1, "nothing is left of it");

    // What is saved is what was loaded, and it comes back from the file.
    let folder = std::env::temp_dir().join(format!("trill-clap-preset-save-{}", std::process::id()));
    let path = folder.join("song.trill");
    session.save(&path).unwrap();
    session.new_project();
    session.load(&path).unwrap();
    there(&mut session);
    assert_eq!((rendered(&mut session), preset_name(&session, device).as_deref()), (0.25, Some("Air")));
    std::fs::remove_dir_all(&folder).ok();
}

#[test]
fn values_set_with_a_preset_go_on_top_of_it_even_when_it_arrives_late() {
    let factory = factory();
    listed(&factory, kind::LATE_PRESETS);
    let (mut session, track) = session(&factory);
    // The engine is processing the plugin, so the preset arrives with a
    // block, a moment after the plugin said yes to it.
    let device = session.add_device(ChainRef::Track(track), &uri(kind::LATE_PRESETS), Some("Bass"), None).unwrap();
    there(&mut session);
    session.set_device_params(device, &[(GAIN.to_string(), 0.875)]).unwrap();
    // Had the value reached the plugin before the preset did, the preset
    // would have overwritten it, for good.
    assert!(eventually(WAIT, || live_gain(&mut session, device) == 0.875), "{}", live_gain(&mut session, device));
    assert_eq!(rendered(&mut session), 0.875);

    // Undone, the value is the preset's again: in the running plugin,
    // and in what a render makes of the project.
    assert!(session.undo());
    there(&mut session);
    assert!(eventually(WAIT, || live_gain(&mut session, device) == 0.5), "{}", live_gain(&mut session, device));
    assert_eq!(rendered(&mut session), 0.5);
}
