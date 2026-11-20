//! The host against the scripted plugins of [`super::scripted`]: everything
//! that can be checked exactly, and everything that depends on a plugin
//! misbehaving. The integration tests cover real, installed plugins.

use super::class_id::ClassId;
use super::probe::records;
use super::scripted::{self, Seen, kind};
use super::state::{self, PluginState};
use super::{Vst3Config, Vst3Factory};
use crate::plugin_thread::plugin_thread;
use crate::test_support::eventually;
use std::time::{Duration, Instant};
use trill_core::device::{
    DeviceCategory, DeviceError, DeviceFactory, DeviceInstance, Event, EventKind, ParamKind, ProcessInfo,
};
use trill_core::{ChainRef, Device, Note, Project};
use trill_engine::render::{RenderOptions, render};
use vst3::Steinberg::Vst::ParamID;
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kPlaying;

const SR: f64 = 48_000.0;
const BLOCK: usize = 64;

fn factory() -> Vst3Factory {
    Vst3Factory::with_modules(vec![("scripted", scripted::factory())])
}

fn device(kind: u8) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, scripted::uri(kind), "plugin", None).unwrap();
    project.device(id).unwrap().clone()
}

fn create(factory: &Vst3Factory, device: &Device) -> DeviceInstance {
    factory.create(device, SR, BLOCK).unwrap_or_else(|error| panic!("{error}"))
}

/// Position of the parameter with VST3 id `id` in the device's list.
fn index(instance: &DeviceInstance, id: ParamID) -> usize {
    let id = id.to_string();
    instance.params.iter().position(|param| param.id == id).unwrap_or_else(|| panic!("no parameter {id}"))
}

/// The value the plugin's controller currently reports for parameter `id`.
fn value(instance: &mut DeviceInstance, id: ParamID) -> f64 {
    let index = index(instance, id);
    instance.controller.as_mut().unwrap().param_values().expect("the plugin reports values")[index]
}

/// The serial number of the instance, by which its observations are kept.
fn serial(instance: &mut DeviceInstance) -> u64 {
    (value(instance, scripted::SERIAL) * 1e6).round() as u64
}

fn seen(instance: &mut DeviceInstance) -> Seen {
    scripted::seen(serial(instance))
}

fn playing() -> ProcessInfo {
    ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    }
}

/// Processes one block of `frames` frames that holds `fill` on the left and
/// `-fill` on the right.
fn block(instance: &mut DeviceInstance, frames: usize, events: &[Event], fill: f32) -> (Vec<f32>, Vec<f32>) {
    let (mut left, mut right) = (vec![fill; frames], vec![-fill; frames]);
    instance.processor.process(&playing(), events, &mut left, &mut right);
    (left, right)
}

fn on(offset: u32, velocity: f32) -> Event {
    Event::new(offset, EventKind::NoteOn { pitch: 60, velocity })
}

fn off(offset: u32) -> Event {
    Event::new(offset, EventKind::NoteOff { pitch: 60 })
}

/// A block of silence with `samples` at the given frames.
fn impulses(frames: usize, samples: &[(usize, f32)]) -> Vec<f32> {
    let mut expected = vec![0.0; frames];
    for &(frame, sample) in samples {
        expected[frame] = sample;
    }
    expected
}

fn load_failure(factory: &Vst3Factory, device: &Device) -> String {
    match factory.create(device, SR, BLOCK) {
        Err(DeviceError::LoadFailed { uri, reason }) => {
            assert_eq!(uri, device.uri);
            reason
        }
        Err(other) => panic!("expected a load failure, got: {other}"),
        Ok(_) => panic!("expected a load failure, but '{}' loaded", device.uri),
    }
}

/// The lifecycle steps of `serial` that are about being active and
/// processing.
fn activity(serial: u64) -> Vec<&'static str> {
    let steps = ["activate", "deactivate", "start", "stop"];
    scripted::lifecycle(serial).into_iter().filter(|step| steps.contains(step)).collect()
}

// ---- listing -------------------------------------------------------------

#[test]
fn plugins_are_listed_by_what_they_say_they_are() {
    let factory = factory();
    let descriptors = factory.descriptors();
    // The class the factory fails to describe, and the controller class,
    // are not plugins.
    assert_eq!(descriptors.len(), kind::ALL.len());
    let find = |kind| descriptors.iter().find(|descriptor| descriptor.uri == scripted::uri(kind)).unwrap();
    assert_eq!(find(kind::IMPULSE).category, DeviceCategory::Instrument);
    assert_eq!(find(kind::LOST_CONTROLLER).category, DeviceCategory::Instrument);
    assert_eq!(find(kind::MONO_GAIN).category, DeviceCategory::Effect);
    assert_eq!(find(kind::BARE).category, DeviceCategory::Effect);
    assert_eq!(find(kind::IMPULSE).description, "VST3 plugin (Instrument, Synth)");
    assert_eq!(find(kind::MONO_GAIN).description, "VST3 plugin (Fx)");
    // The newest factory interface is the one listened to: it has the
    // Unicode names.
    assert_eq!(find(kind::IMPULSE).name, "Scripted 0 v3");
    assert!(descriptors.iter().all(|descriptor| descriptor.vendor == "Vendor ♪"));
    assert!(scripted::host_contexts() >= 1, "the module was told who its host is");
}

#[test]
fn older_modules_are_read_through_the_factory_interface_they_have() {
    let second = records(&scripted::old_factory(2));
    assert_eq!(second.len(), kind::ALL.len());
    let impulse = second.iter().find(|record| format!("vst3:{}", record.id) == scripted::uri(kind::IMPULSE)).unwrap();
    assert_eq!(
        (impulse.name.as_str(), impulse.vendor.as_str(), impulse.version.as_str()),
        ("Scripted 0 v2", "Class Vendor", "2.0")
    );
    assert_eq!(impulse.features, ["Instrument", "Synth"]);

    // The first version knows neither sub-categories nor class vendors:
    // everything is an effect, by the module's vendor.
    let first = records(&scripted::old_factory(1));
    assert_eq!(first.len(), kind::ALL.len());
    let impulse = first.iter().find(|record| format!("vst3:{}", record.id) == scripted::uri(kind::IMPULSE)).unwrap();
    assert_eq!(
        (impulse.name.as_str(), impulse.vendor.as_str(), impulse.version.as_str()),
        ("Scripted 0 v1", "trill-plugins tests", "")
    );
    assert!(impulse.features.is_empty());
}

#[test]
fn parameters_are_listed_mapped_and_told_apart() {
    let factory = factory();
    let instance = create(&factory, &device(kind::IMPULSE));
    // Out are: the repeated id, the parameter the plugin cannot describe,
    // and those that are hidden, read-only or not automatable.
    assert_eq!(instance.params.len(), scripted::LISTED_PARAMS);
    let param = |id| &instance.params[index(&instance, id)];
    let listed: Vec<&str> = instance.params.iter().map(|param| param.id.as_str()).collect();
    assert_eq!(
        listed,
        ["1", "2", "3", "4", "5", "6", "7", "4000000000", "9"],
        "ids are decimal, in the plugin's order"
    );

    // A continuous parameter is used as it is, 0 to 1, with its unit.
    let gain = param(scripted::GAIN);
    assert_eq!((gain.name.as_str(), gain.min, gain.max, gain.default), ("Gain", 0.0, 1.0, 0.5));
    assert_eq!((&gain.kind, gain.unit.as_str()), (&ParamKind::Float, "x"));

    // One step is a switch.
    let switch = param(scripted::SWITCH);
    assert_eq!((&switch.kind, switch.min, switch.max, switch.default), (&ParamKind::Bool, 0.0, 1.0, 0.0));

    // A few named steps are choices, and the default is what the plugin
    // starts with (the second), not what it declares (the third).
    let shape = param(scripted::SHAPE);
    let choices = scripted::SHAPES.map(str::to_string).to_vec();
    assert_eq!((&shape.kind, shape.max, shape.default), (&ParamKind::Enum(choices), 3.0, 1.0));
    assert_eq!(shape.clamp(2.6), 3.0);

    // Many steps, or steps without names, are integers.
    let voices = param(scripted::VOICES);
    assert_eq!(
        (&voices.kind, voices.max, voices.default, voices.unit.as_str()),
        (&ParamKind::Int, 127.0, 16.0, "voices")
    );
    let unnamed = param(scripted::UNNAMED);
    assert_eq!(
        (&unnamed.kind, unnamed.max, unnamed.default, unnamed.unit.as_str()),
        (&ParamKind::Int, 4.0, 2.0, "steps")
    );

    // Same title, different units.
    assert_eq!(param(scripted::CUTOFF_1).name, "Filter 1/Cutoff");
    assert_eq!(param(scripted::CUTOFF_2).name, "Filter 2/Cutoff");
    assert_eq!((param(scripted::CUTOFF_1).default, param(scripted::CUTOFF_2).default), (0.25, 0.75));
}

#[test]
fn a_value_is_shown_in_the_plugins_words_and_found_from_them() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::STEREO_GAIN));
    let at = |id| index(&instance, id);
    let (gain, switch, shape) = (at(scripted::GAIN), at(scripted::SWITCH), at(scripted::SHAPE));
    let (voices, unnamed) = (at(scripted::VOICES), at(scripted::UNNAMED));
    let (cutoff, echo) = (at(scripted::CUTOFF_1), at(scripted::ECHO));
    let controller = instance.controller.as_mut().unwrap();
    let text = |text: &str| Some(text.to_string());

    // The unit the plugin keeps apart is put after the number, and not put
    // twice where the plugin says it in the text. Steps are asked about by
    // their number. A value the plugin has no text for has none.
    let asked = [(gain, 0.5), (switch, 1.0), (shape, 2.0), (voices, 16.0), (cutoff, 0.0), (echo, 0.25)];
    assert_eq!(
        controller.param_texts(&asked),
        [text("1.00 x"), text("On"), text("Square"), text("17 voices"), text("20 Hz"), text("25 %")]
    );
    assert_eq!(controller.param_texts(&[(unnamed, 2.0), (9999, 0.0)]), [None, None]);

    // The plugin reads its own gain from a bare number: it is asked with
    // the unit taken off as well.
    assert_eq!(controller.param_value(gain, "1.5 x"), Some(0.75));
    assert_eq!(controller.param_value(gain, "1.5"), Some(0.75));
    // Beyond its range it answers with the end of it, which is not what
    // was asked for.
    assert_eq!(controller.param_value(gain, "9 x"), None);
    // What it does not convert is found among what it shows: a choice or a
    // switch by its name, a step by its number, a frequency by bisection.
    assert_eq!(controller.param_value(shape, "noise"), Some(3.0));
    assert_eq!(controller.param_value(shape, "Triangle"), None);
    assert_eq!((controller.param_value(switch, "on"), controller.param_value(switch, "Off")), (Some(1.0), Some(0.0)));
    assert_eq!(controller.param_value(voices, "17 voices"), Some(16.0));
    assert_eq!(controller.param_value(voices, "128"), Some(127.0));
    assert_eq!(controller.param_value(voices, "129"), None);
    let found = controller.param_value(cutoff, "1000 Hz").unwrap();
    assert_eq!(controller.param_texts(&[(cutoff, found)]), [text("1000 Hz")]);
    assert!((found - 0.566).abs() < 0.01, "{found}");
    assert_eq!(controller.param_value(unnamed, "2"), None, "nothing is shown to find it by");

    // Asked about the echo, the plugin says 0 whatever the text. That is
    // believed only where it shows 0 as what was asked for.
    assert_eq!(controller.param_value(echo, "0 %"), Some(0.0));
    let found = controller.param_value(echo, "40%").unwrap();
    assert_eq!(controller.param_texts(&[(echo, found)]), [text("40 %")]);
    assert_eq!(controller.param_value(echo, "140 %"), None);
}

#[test]
fn a_plugin_says_what_channels_it_has_and_what_its_own_defaults_are() {
    let factory = factory();
    let layout = |kind| create(&factory, &device(kind)).controller.as_ref().unwrap().channels().unwrap();
    assert_eq!(layout(kind::STEREO_GAIN).describe(), "stereo in, stereo out");
    assert_eq!(layout(kind::MONO_GAIN).describe(), "mono in, mono out");
    assert!(layout(kind::MONO_GAIN).folds_input() && !layout(kind::STEREO_GAIN).folds_input());
    assert_eq!(layout(kind::IMPULSE).describe(), "no audio in, mono out");

    // A parameter is listed with the value it started with as its default
    // (the second shape); the plugin's own default is the one it declares
    // (the third).
    let instance = create(&factory, &device(kind::STEREO_GAIN));
    let defaults = instance.controller.as_ref().unwrap().param_defaults().unwrap();
    assert_eq!(defaults.len(), instance.params.len());
    let shape = index(&instance, scripted::SHAPE);
    assert_eq!((instance.params[shape].default, defaults[shape]), (1.0, 2.0));
    let voices = index(&instance, scripted::VOICES);
    assert_eq!((instance.params[voices].default, defaults[voices]), (16.0, 16.0));
}

// ---- audio and events ----------------------------------------------------

#[test]
fn notes_land_on_their_exact_frame_and_an_instrument_adds_to_the_buffer() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let (left, right) = block(&mut instance, BLOCK, &[on(17, 0.5), off(40), on(63, 1.0)], 0.25);

    let mut expected = vec![0.25; BLOCK];
    (expected[17], expected[40], expected[63]) = (0.75, -0.25, 1.25);
    assert_eq!(left, expected);
    // The plugin's single output channel goes to both sides, on top of
    // what each side held.
    let expected_right: Vec<f32> = expected.iter().map(|sample| sample - 0.5).collect();
    assert_eq!(right, expected_right);

    // It was given no audio input, one mono output, and all four lists.
    let seen = seen(&mut instance);
    assert_eq!(seen.buses, (vec![], vec![(1, 0, true)]));
    assert!(!seen.lists_missing, "a list was missing from a process call");
    assert_eq!(seen.event_taken, Some(false), "the plugin's own events have nowhere to go");
}

#[test]
fn a_parameter_set_between_blocks_applies_from_the_first_frame() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let gain = index(&instance, scripted::GAIN);

    instance.processor.set_param(gain, 0.1);
    instance.processor.set_param(gain, 1.0); // the last value wins
    instance.processor.set_param(10_000, 1.0); // no such parameter
    instance.processor.set_param(gain, f64::NAN); // not a value
    let (left, _) = block(&mut instance, BLOCK, &[on(0, 0.5), on(9, 0.25)], 0.0);
    // A normalized gain of 1 doubles.
    assert_eq!(left, impulses(BLOCK, &[(0, 1.0), (9, 0.5)]));
    assert_eq!(seen(&mut instance).received, [(scripted::GAIN, 1.0, 0)], "one change, at the start of the block");
    // The controller was told, and reports it.
    assert_eq!(value(&mut instance, scripted::GAIN), 1.0);
    // And so was what the processor changed by itself in response.
    assert_eq!(value(&mut instance, scripted::ECHO), 1.0);
    // Whether the controller also saw the first, overwritten value
    // depends on when the plugin main thread last looked; what it was
    // told last is what counts.
    let sets = seen(&mut instance).controller_sets;
    let last = |id| sets.iter().rev().find(|(param, _)| *param == id).map(|(_, value)| *value);
    assert_eq!((last(scripted::GAIN), last(scripted::ECHO)), (Some(1.0), Some(1.0)));
    assert!(sets.iter().all(|(id, _)| [scripted::GAIN, scripted::ECHO].contains(id)), "{sets:?}");

    // Nothing is sent again while nothing changes.
    block(&mut instance, BLOCK, &[], 0.0);
    assert_eq!(seen(&mut instance).received.len(), 1);
    assert_eq!(seen(&mut instance).controller_sets.len(), sets.len());
}

#[test]
fn stepped_parameters_reach_the_plugin_normalized_and_come_back_as_steps() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let (switch, shape) = (index(&instance, scripted::SWITCH), index(&instance, scripted::SHAPE));
    let (voices, unnamed) = (index(&instance, scripted::VOICES), index(&instance, scripted::UNNAMED));
    /// The value the processor was last sent for parameter `id`.
    fn sent(instance: &mut DeviceInstance, id: ParamID) -> Option<f64> {
        seen(instance).received.iter().rev().find(|(param, ..)| *param == id).map(|(_, value, _)| *value)
    }

    instance.processor.set_param(shape, 3.0);
    instance.processor.set_param(voices, 64.0);
    instance.processor.set_param(unnamed, 99.0); // beyond the last step
    instance.processor.set_param(switch, 1.0);
    block(&mut instance, BLOCK, &[], 0.0);
    assert_eq!(sent(&mut instance, scripted::SHAPE), Some(1.0));
    assert_eq!(sent(&mut instance, scripted::VOICES), Some(64.0 / 127.0));
    assert_eq!(sent(&mut instance, scripted::UNNAMED), Some(1.0));
    assert_eq!(sent(&mut instance, scripted::SWITCH), Some(1.0));

    for (id, plain) in
        [(scripted::SHAPE, 3.0), (scripted::VOICES, 64.0), (scripted::UNNAMED, 4.0), (scripted::SWITCH, 1.0)]
    {
        assert_eq!(value(&mut instance, id), plain);
    }
    // A value between steps is the nearest step.
    instance.processor.set_param(shape, 1.4);
    block(&mut instance, BLOCK, &[], 0.0);
    assert_eq!(sent(&mut instance, scripted::SHAPE), Some(1.0 / 3.0));
    assert_eq!(value(&mut instance, scripted::SHAPE), 1.0);
}

#[test]
fn an_edit_in_the_plugins_own_editor_reaches_its_processor() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let switch = index(&instance, scripted::SWITCH);

    // Turning the switch on makes the scripted editor move a control that
    // no host can: it is not automatable, and no device lists it.
    instance.processor.set_param(switch, 1.0);
    block(&mut instance, BLOCK, &[], 0.0);
    assert_eq!(value(&mut instance, scripted::SWITCH), 1.0); // asking carries the switch to the controller
    block(&mut instance, BLOCK, &[], 0.0);

    let seen = seen(&mut instance);
    assert_eq!(seen.received, [(scripted::SWITCH, 1.0, 0), (scripted::MANUAL, scripted::MANUAL_EDIT, 0)]);
    // The edit came from the controller; it is not told its own news.
    assert_eq!(seen.controller_sets, [(scripted::SWITCH, 1.0)]);
}

#[test]
fn an_oversized_block_reaches_the_plugin_in_pieces_it_was_set_up_for() {
    let factory = factory();
    let mut instance = factory.create(&device(kind::IMPULSE), SR, 32).unwrap();
    let events = [on(0, 1.0), on(31, 0.5), on(32, 0.25), on(99, 0.125)];
    let (left, right) = block(&mut instance, 100, &events, 0.0);
    assert_eq!(left, impulses(100, &[(0, 1.0), (31, 0.5), (32, 0.25), (99, 0.125)]));
    assert_eq!(right, left);
    let seen = seen(&mut instance);
    assert_eq!((seen.largest_block, seen.blocks), (32, 4));
    assert_eq!(seen.setup.map(|(rate, block, ..)| (rate, block)), Some((SR, 32)));
}

#[test]
fn a_mono_effect_gets_both_sides_mixed_and_replaces_them() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::MONO_GAIN));
    let (mut left, mut right) = (vec![1.0f32; BLOCK], vec![0.5f32; BLOCK]);
    instance.processor.process(&playing(), &[on(0, 1.0)], &mut left, &mut right);
    assert_eq!(left, vec![0.75; BLOCK], "the mono input is the average of both sides");
    assert_eq!(right, left, "the mono output replaces both sides");
    // It was asked to be stereo, said no, and was taken as it is.
    assert_eq!(seen(&mut instance).buses, (vec![(1, 0, true)], vec![(1, 0, true)]));

    instance.processor.set_param(index(&instance, scripted::GAIN), 1.0);
    let (left, right) = block(&mut instance, BLOCK, &[], 0.5);
    // `block` puts 0.5 left and -0.5 right, which cancel in mono.
    assert_eq!((left, right), (vec![0.0; BLOCK], vec![0.0; BLOCK]));
}

#[test]
fn a_plugin_that_can_be_stereo_is_made_stereo_and_only_its_main_buses_are_used() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::STEREO_GAIN));
    let (left, right) = block(&mut instance, BLOCK, &[], 0.5);
    assert_eq!((left, right), (vec![0.5; BLOCK], vec![-0.5; BLOCK]), "each side on its own channel");
    instance.processor.set_param(index(&instance, scripted::GAIN), 0.25);
    let (left, right) = block(&mut instance, BLOCK, &[], 0.5);
    // The plugin fills its second output with 9.0; none of it comes back.
    assert_eq!((left, right), (vec![0.25; BLOCK], vec![-0.25; BLOCK]));

    let seen = seen(&mut instance);
    // The main buses went stereo. The side-chain is there, silent and
    // flagged so; the second output has buffers to write to.
    assert_eq!(seen.buses, (vec![(2, 0, true), (1, 0b1, true)], vec![(2, 0, true), (2, 0, true)]));
    // Only the main buses were switched on.
    let (audio, input, output) = (0, 0, 1);
    assert_eq!(
        seen.activated,
        [(audio, input, 0, true), (audio, input, 1, false), (audio, output, 0, true), (audio, output, 1, false)]
    );

    // Latency is read once the plugin is active, and again whenever it is
    // set up anew.
    assert_eq!(instance.processor.latency_samples(), 48);
    instance.processor.prepare(96_000.0, BLOCK);
    assert_eq!(instance.processor.latency_samples(), 96);
}

#[test]
fn the_plugin_is_told_where_the_song_is() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let mut info = ProcessInfo {
        sample_rate: SR,
        tempo: 90.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 12.5,
        song: Default::default(),
        loop_range: Some((8.0, 16.0)),
    }
    .steady();
    let (mut left, mut right) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    instance.processor.process(&info, &[], &mut left, &mut right);
    let context = seen(&mut instance).context.expect("a process context");
    assert_eq!((context.tempo, context.projectTimeMusic, context.barPositionMusic), (90.0, 12.5, 12.0));
    assert_eq!(context.projectTimeSamples, 400_000, "12.5 beats at 90 BPM are 8 1/3 seconds");
    assert_eq!((context.cycleStartMusic, context.cycleEndMusic), (8.0, 16.0));
    assert_eq!((context.sampleRate, context.continousTimeSamples), (SR, 0));
    assert!(context.state & kPlaying as u32 != 0);

    info = ProcessInfo { playing: false, position_beats: 3.0, ..info }.steady();
    instance.processor.process(&info, &[], &mut left, &mut right);
    let context = seen(&mut instance).context.expect("a process context");
    assert!(context.state & kPlaying as u32 == 0);
    assert_eq!(context.projectTimeMusic, 3.0);
    assert_eq!(context.continousTimeSamples, BLOCK as i64, "the clock counts frames");
}

/// The scripted plugin as [`crate::transport_tests`] stages it.
fn stage(factory: &Vst3Factory) -> crate::transport_tests::Stage<'_> {
    crate::transport_tests::Stage {
        format: crate::transport::written::Format::Vst3,
        factory,
        uri: scripted::uri(kind::IMPULSE),
        serial,
    }
}

#[test]
fn a_plugin_with_a_side_chain_hears_the_track_its_device_listens_to_and_has_it_switched_on_only_then() {
    use vst3::Steinberg::Vst::BusDirections_::kInput;
    use vst3::Steinberg::Vst::MediaTypes_::kAudio;
    use vst3::Steinberg::Vst::{BusDirection, MediaType};
    let factory = factory();
    crate::key_tests::a_plugin_with_a_key_input_hears_the_track_its_device_listens_to(&crate::key_tests::Stage {
        format: crate::transport::written::Format::Vst3,
        factory: &factory,
        instrument: scripted::uri(kind::IMPULSE),
        keyed: scripted::uri(kind::STEREO_GAIN),
        serial,
        key_input: Some(|serial| {
            let seen = scripted::seen(serial);
            let on = seen.activated.contains(&(kAudio as MediaType, kInput as BusDirection, 1, true));
            let silent = seen.buses.0.get(1).is_some_and(|(_, flags, _)| *flags != 0);
            (on, silent)
        }),
    });
    // A plugin that has the bus takes a key, and is one that is made
    // again for it; one without takes none.
    let keyed = create(&factory, &device(kind::STEREO_GAIN));
    assert!(keyed.processor.takes_key() && keyed.processor.key_is_set_when_made());
    let plain = create(&factory, &device(kind::MONO_GAIN));
    assert!(!plain.processor.takes_key() && !plain.processor.key_is_set_when_made());
    assert!(!create(&factory, &device(kind::IMPULSE)).processor.takes_key());
}

#[test]
fn the_plugin_is_told_the_songs_time_and_bars_through_changes_of_tempo_and_meter_whatever_the_buffer_size() {
    crate::transport_tests::a_plugin_is_told_the_songs_time_and_bars_whatever_the_buffer_size(&stage(&factory()));
}

#[test]
fn the_plugin_is_told_where_the_song_went_when_it_loops_or_is_moved() {
    crate::transport_tests::a_plugin_is_told_where_the_song_went_when_it_loops_or_is_moved(&stage(&factory()));
}

#[test]
fn all_notes_off_and_reset_end_exactly_the_notes_that_are_down() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let serial = serial(&mut instance);
    let key = |offset, pitch| Event::new(offset, EventKind::NoteOn { pitch, velocity: 1.0 });
    block(&mut instance, BLOCK, &[key(0, 60), key(0, 64), key(0, 67), off(1)], 0.0);

    // Two keys are down: two note-offs, at the event's frame.
    let flush = Event::new(5, EventKind::AllNotesOff);
    let (left, _) = block(&mut instance, BLOCK, &[flush], 0.0);
    assert_eq!(left, impulses(BLOCK, &[(5, -1.0)]));
    let (left, _) = block(&mut instance, BLOCK, &[flush], 0.0);
    assert_eq!(left, impulses(BLOCK, &[]), "nothing is down any more");

    // Reset: processing is stopped and started, and with the next block
    // the key is released and the plugin is sent "all sound off" through
    // the parameter it maps that controller to.
    block(&mut instance, BLOCK, &[key(0, 72)], 0.0);
    instance.processor.reset();
    assert_eq!(activity(serial), ["activate", "start", "stop", "start"]);
    let (left, _) = block(&mut instance, BLOCK, &[key(8, 60)], 0.0);
    assert_eq!(left, impulses(BLOCK, &[(0, -0.5 - 0.125), (8, 1.0)]));
    assert_eq!(scripted::seen(serial).received, [(scripted::ALL_SOUND_OFF, 0.0, 0)]);
    let (left, _) = block(&mut instance, BLOCK, &[], 0.0);
    assert_eq!(left, impulses(BLOCK, &[]), "the reset is carried out once");
}

// ---- model values and state ----------------------------------------------

#[test]
fn model_values_are_in_place_before_the_first_block() {
    let factory = factory();
    let mut model = device(kind::IMPULSE);
    model.params.insert(scripted::GAIN.to_string(), 0.25);
    model.params.insert(scripted::SHAPE.to_string(), 99.0); // clamped to the last choice
    model.params.insert(scripted::METER.to_string(), 0.5); // read-only: not a parameter of the device
    model.params.insert("424242".into(), 1.0); // not a parameter of this plugin
    model.params.insert("cutoff".into(), 1.0); // not a VST3 id at all
    let mut instance = create(&factory, &model);
    let serial = serial(&mut instance);

    assert_eq!(value(&mut instance, scripted::GAIN), 0.25);
    assert_eq!(value(&mut instance, scripted::SHAPE), 3.0);
    // The defaults are the values without the model's: what the engine
    // returns to when a setting is removed.
    assert_eq!(instance.params[index(&instance, scripted::GAIN)].default, 0.5);
    assert_eq!(instance.params[index(&instance, scripted::SHAPE)].default, 1.0);

    // The values went to the processor in a block without frames, and the
    // plugin was then activated again, so it starts from them.
    assert_eq!(activity(serial), ["activate", "start", "stop", "deactivate", "activate"]);
    let before = scripted::seen(serial);
    assert_eq!((before.blocks, before.flushes), (1, 1));
    assert_eq!(before.received, [(scripted::GAIN, 0.25, 0), (scripted::SHAPE, 1.0, 0)]);
    let (left, _) = block(&mut instance, BLOCK, &[on(0, 1.0)], 0.0);
    assert_eq!(left[0], 0.5);
    // They go out once more with the first real block, for plugins that
    // forget them when activated.
    assert_eq!(scripted::seen(serial).received.len(), 4);

    // Without model values there is nothing to apply and one activation.
    let mut plain = create(&factory, &device(kind::IMPULSE));
    let serial = self::serial(&mut plain);
    assert_eq!(activity(serial), ["activate"]);
    assert_eq!(scripted::seen(serial).blocks, 0);
}

#[test]
fn state_round_trips_and_model_values_go_on_top_of_it() {
    let factory = factory();
    let mut original = create(&factory, &device(kind::IMPULSE));
    // One value the processor keeps, and one only the controller does.
    original.processor.set_param(index(&original, scripted::GAIN), 0.25);
    original.processor.set_param(index(&original, scripted::VOICES), 100.0);
    block(&mut original, BLOCK, &[], 0.0);
    let saved = original.controller.as_mut().unwrap().save_state().expect("the plugin has state");

    // Both halves' states are in the blob, under the plugin's class id.
    let (class, parts) = state::decode(&saved).unwrap();
    assert_eq!(format!("vst3:{class}"), scripted::uri(kind::IMPULSE));
    assert!(parts.component.starts_with(b"SCRP"), "the component's own bytes");
    assert!(parts.controller.starts_with(b"CTRL"), "the controller's own bytes");

    let mut model = device(kind::IMPULSE);
    model.state = Some(saved.clone());
    let mut restored = create(&factory, &model);
    assert_eq!(value(&mut restored, scripted::GAIN), 0.25);
    assert_eq!(value(&mut restored, scripted::VOICES), 100.0);
    // The restored state is the new baseline.
    assert_eq!(restored.params[index(&restored, scripted::GAIN)].default, 0.25);
    // The processor has it too: it plays at the restored gain.
    let (left, _) = block(&mut restored, BLOCK, &[on(0, 1.0)], 0.0);
    assert_eq!(left[0], 0.5);
    // Each half was given its state, and the controller the component's.
    let serial = serial(&mut restored);
    assert!(scripted::lifecycle(serial).contains(&"setState"));
    let controller = scripted::controller_lifecycle(serial);
    assert!(controller.contains(&"setComponentState") && controller.contains(&"controller.setState"), "{controller:?}");
    // Saved again, it is the same state.
    assert_eq!(restored.controller.as_mut().unwrap().save_state(), Some(saved));

    model.params.insert(scripted::GAIN.to_string(), 0.75);
    let mut both = create(&factory, &model);
    assert_eq!(value(&mut both, scripted::GAIN), 0.75);
    assert_eq!(value(&mut both, scripted::VOICES), 100.0);
    assert_eq!(both.params[index(&both, scripted::GAIN)].default, 0.25);

    // An empty state is no state.
    let mut empty = device(kind::IMPULSE);
    empty.state = Some(Vec::new());
    assert_eq!(value(&mut create(&factory, &empty), scripted::GAIN), 0.5);
}

#[test]
fn a_fresh_plugins_controller_is_brought_in_line_with_its_component() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let serial = serial(&mut instance);
    // Nothing was saved, so the component was not given a state; but the
    // controller was shown the one the component starts with.
    assert!(!scripted::lifecycle(serial).contains(&"setState"));
    assert!(scripted::controller_lifecycle(serial).contains(&"setComponentState"));
}

#[test]
fn a_state_that_is_not_the_plugins_fails_the_device() {
    let factory = factory();
    let mut model = device(kind::IMPULSE);
    let class = ClassId::parse(scripted::uri(kind::IMPULSE).strip_prefix("vst3:").unwrap()).unwrap();

    model.state = Some(b"saved by some other plugin, in some other format".to_vec());
    let reason = load_failure(&factory, &model);
    assert!(reason.contains("not a VST3 state"), "{reason}");

    // Well-formed, but of another plugin.
    let other = ClassId::parse(scripted::uri(kind::MONO_GAIN).strip_prefix("vst3:").unwrap()).unwrap();
    let parts = PluginState { component: b"SCRP".to_vec(), controller: Vec::new() };
    model.state = Some(state::encode(other, &parts));
    let reason = load_failure(&factory, &model);
    assert!(reason.contains("belongs to another plugin"), "{reason}");

    // Of this plugin, but nothing it recognizes.
    let parts = PluginState { component: b"garbage".to_vec(), controller: Vec::new() };
    model.state = Some(state::encode(class, &parts));
    let reason = load_failure(&factory, &model);
    assert!(reason.contains("rejected its saved state"), "{reason}");

    // Every one of those plugins was taken down again properly.
    for serial in scripted::instances(kind::IMPULSE) {
        let steps = scripted::lifecycle(serial);
        if !steps.contains(&"activate") && steps.contains(&"destroy") {
            assert_eq!(steps[steps.len() - 2..], ["terminate", "destroy"], "{steps:?}");
        }
    }
}

// ---- threads and lifecycle -----------------------------------------------

#[test]
fn the_plugin_is_called_on_the_threads_it_was_promised() {
    let factory = factory();
    let mut model = device(kind::IMPULSE);
    model.params.insert(scripted::GAIN.to_string(), 0.25);
    let mut instance = create(&factory, &model);
    block(&mut instance, BLOCK, &[], 0.0);
    instance.processor.reset();
    instance.processor.prepare(44_100.0, 32);
    let state = instance.controller.as_mut().unwrap().save_state();
    assert!(state.is_some());
    let seen = seen(&mut instance);

    // Everything VST3 wants on "the UI thread" came from one thread: the
    // process's plugin main thread, which is not this one.
    let main = plugin_thread().unwrap().call(Duration::from_secs(10), |_| std::thread::current().id()).unwrap();
    assert_eq!(seen.main_threads, [main]);
    assert_ne!(main, std::thread::current().id());
    // Processing, and starting and stopping it, happened here: whichever
    // thread drives the processor is the plugin's audio thread.
    assert_eq!(seen.audio_threads, [std::thread::current().id()]);
}

#[test]
fn the_lifecycle_runs_in_order_through_to_destruction() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let serial = serial(&mut instance);
    block(&mut instance, BLOCK, &[], 0.0);
    block(&mut instance, BLOCK, &[], 0.0);

    // A new sample rate means deactivating, setting up and activating
    // again.
    instance.processor.prepare(44_100.0, 32);
    assert_eq!(scripted::seen(serial).setup, Some((44_100.0, 32, 0, 0)), "realtime, 32-bit float");
    instance.processor.prepare(44_100.0, 32); // nothing changed: nothing happens
    let (left, _) = block(&mut instance, 32, &[on(31, 1.0)], 0.0);
    assert_eq!(left[31], 1.0);

    // The two halves found each other through the host: the controller
    // learned its instance's serial, and its greeting arrived, in a
    // message the host made.
    let seen = scripted::seen(serial);
    assert_eq!(seen.greeting, Some(("héllo".to_string(), vec![1, 2, 3])));
    assert_eq!(seen.host, "Trill");

    let DeviceInstance { processor, mut controller, .. } = instance;
    drop(processor);
    assert_eq!(
        scripted::lifecycle(serial),
        [
            "create",
            "initialize",
            "connect",
            "arrange",
            "setup",
            "activate",
            "start",
            "stop",
            "deactivate",
            "setup",
            "activate",
            "start",
            "stop",
            "deactivate",
            "disconnect",
            "terminate",
            "destroy"
        ]
    );
    assert_eq!(
        scripted::controller_lifecycle(serial),
        [
            "create",
            "initialize",
            "handler set",
            "connect",
            "setComponentState",
            "disconnect",
            "handler cleared",
            "terminate",
            "destroy"
        ]
    );
    // The controller outlives the plugin, and says so quietly.
    let controller = controller.as_mut().unwrap();
    assert_eq!(controller.save_state(), None);
    assert_eq!(controller.param_values(), None);
}

#[test]
fn an_instance_never_processed_is_torn_down_without_being_started() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::MONO_GAIN));
    let serial = serial(&mut instance);
    drop(instance);
    // A component that is its own controller is initialized and terminated
    // once, and has nothing to be connected to.
    assert_eq!(
        scripted::lifecycle(serial),
        [
            "create",
            "initialize",
            "handler set",
            "setComponentState",
            "arrange",
            "setup",
            "activate",
            "deactivate",
            "handler cleared",
            "terminate",
            "destroy"
        ]
    );
}

// ---- plugins that do not cooperate ---------------------------------------

#[test]
fn a_plugin_without_buses_state_or_controller_is_hosted_as_a_passthrough() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::BARE));
    assert!(instance.params.is_empty());
    let (left, right) = block(&mut instance, BLOCK, &[on(0, 1.0)], 0.25);
    assert_eq!((left, right), (vec![0.25; BLOCK], vec![-0.25; BLOCK]));
    instance.processor.set_param(0, 1.0);
    instance.processor.reset();
    assert_eq!(instance.processor.latency_samples(), 0);
    let controller = instance.controller.as_mut().unwrap();
    assert_eq!(controller.save_state(), None);
    assert_eq!(controller.param_values(), None);

    // It was still processed, with no buses at all.
    let serial = *scripted::instances(kind::BARE).last().unwrap();
    let seen = scripted::seen(serial);
    assert_eq!((seen.blocks, seen.buses), (1, (vec![], vec![])));
    drop(instance);
    let steps = scripted::lifecycle(serial);
    assert_eq!(
        steps,
        [
            "create",
            "initialize",
            "setup",
            "activate",
            "start",
            "stop",
            "start",
            "stop",
            "deactivate",
            "terminate",
            "destroy"
        ]
    );
}

#[test]
fn a_plugin_whose_controller_class_is_missing_is_hosted_without_parameters() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::LOST_CONTROLLER));
    assert!(instance.params.is_empty());
    assert_eq!(instance.controller.as_mut().unwrap().param_values(), None);
    // It still plays, and its component's state is still saved.
    let (left, _) = block(&mut instance, BLOCK, &[on(3, 1.0)], 0.0);
    assert_eq!(left, impulses(BLOCK, &[(3, 1.0)]));
    let saved = instance.controller.as_mut().unwrap().save_state().expect("the component has state");
    let (_, parts) = state::decode(&saved).unwrap();
    assert!(parts.component.starts_with(b"SCRP") && parts.controller.is_empty());
}

#[test]
fn plugins_that_cannot_be_loaded_are_errors_not_panics() {
    let factory = factory();
    let reason = load_failure(&factory, &device(kind::FAILS_INIT));
    assert!(reason.contains("failed to initialize"), "{reason}");
    // What was never initialized is not terminated, only released.
    let serial = *scripted::instances(kind::FAILS_INIT).last().unwrap();
    assert_eq!(scripted::lifecycle(serial), ["create", "destroy"]);

    let reason = load_failure(&factory, &device(kind::FAILS_ACTIVATE));
    assert!(reason.contains("cannot activate"), "{reason}");
    // What was initialized is, without having been active.
    let serial = *scripted::instances(kind::FAILS_ACTIVATE).last().unwrap();
    let steps = scripted::lifecycle(serial);
    assert_eq!(steps[steps.len() - 3..], ["handler cleared", "terminate", "destroy"], "{steps:?}");
    assert!(!steps.contains(&"deactivate"));

    // The host is none the worse for it.
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let (left, _) = block(&mut instance, BLOCK, &[on(0, 1.0)], 0.0);
    assert_eq!(left[0], 1.0);
}

#[test]
fn a_plugin_that_cannot_be_activated_again_fails_or_is_bypassed() {
    let factory = factory();
    // Without model values it is activated once, and works.
    let mut plain = create(&factory, &device(kind::FAILS_REACTIVATE));
    let (left, right) = block(&mut plain, BLOCK, &[], 0.5);
    assert_eq!((left, right), (vec![0.0; BLOCK], vec![0.0; BLOCK]), "processed: both sides cancel in mono");

    // With model values it has to be activated a second time to start from
    // them. That it cannot is why the device cannot be created.
    let mut model = device(kind::FAILS_REACTIVATE);
    model.params.insert(scripted::GAIN.to_string(), 0.25);
    let reason = load_failure(&factory, &model);
    assert!(reason.contains("cannot activate"), "{reason}");
    let serial = *scripted::instances(kind::FAILS_REACTIVATE).last().unwrap();
    assert_eq!(activity(serial), ["activate", "start", "stop", "deactivate"]);
    let steps = scripted::lifecycle(serial);
    assert_eq!(steps[steps.len() - 3..], ["handler cleared", "terminate", "destroy"], "{steps:?}");

    // A device that already exists has nowhere to fail to: when a new
    // sample rate cannot be had, it passes audio through.
    plain.processor.prepare(44_100.0, 32);
    let (left, right) = block(&mut plain, 32, &[], 0.5);
    assert_eq!((left, right), (vec![0.5; 32], vec![-0.5; 32]));
    plain.processor.reset();
    plain.processor.set_param(0, 1.0);
}

#[test]
fn a_plugin_that_fails_while_running_is_bypassed() {
    let factory = factory();
    // Fails its third block: two blocks processed (to mono: silence), then
    // audio passes through.
    let mut instance = create(&factory, &device(kind::FAILS_PROCESS));
    let serial = serial(&mut instance);
    for expected in [0.0, 0.0, 0.5, 0.5] {
        let (left, right) = block(&mut instance, BLOCK, &[], 0.5);
        assert_eq!((left, right), (vec![expected; BLOCK], vec![-expected; BLOCK]));
    }
    assert_eq!(scripted::seen(serial).blocks, 3, "a failed plugin is not asked again");
    // Nor is it reset; but it is still torn down properly.
    instance.processor.reset();
    drop(instance);
    assert_eq!(activity(serial), ["activate", "start", "stop", "deactivate"]);
    assert_eq!(scripted::lifecycle(serial).last(), Some(&"destroy"));
}

#[test]
fn a_plugin_made_for_a_render_is_set_up_and_processed_offline() {
    use trill_core::device::Purpose;
    use vst3::Steinberg::Vst::ProcessModes_::{kOffline, kRealtime};
    let factory = factory();
    // Made to be played, as every device of a session is: realtime.
    let mut played = create(&factory, &device(kind::IMPULSE));
    block(&mut played, BLOCK, &[], 0.0);
    let seen = self::seen(&mut played);
    assert_eq!(seen.setup.map(|(.., mode, _)| mode), Some(kRealtime));
    assert_eq!(seen.block_mode, Some(kRealtime));

    // Made for a render: told so when it is set up, and with every block,
    // and again when it is set up for another sample rate.
    let mut rendered = factory
        .create_for(&device(kind::IMPULSE), SR, BLOCK, Purpose::Rendering)
        .unwrap_or_else(|error| panic!("{error}"));
    let (left, _) = block(&mut rendered, BLOCK, &[on(3, 1.0)], 0.0);
    assert_eq!(left[3], 1.0, "it plays as it would played live");
    let seen = self::seen(&mut rendered);
    assert_eq!(seen.setup.map(|(.., mode, _)| mode), Some(kOffline));
    assert_eq!(seen.block_mode, Some(kOffline));
    rendered.processor.prepare(44_100.0, 32);
    block(&mut rendered, 32, &[], 0.0);
    let seen = self::seen(&mut rendered);
    assert_eq!(seen.setup, Some((44_100.0, 32, kOffline, 0)));
}

// ---- through the engine --------------------------------------------------

#[test]
fn the_engine_renders_a_plugin_note_on_its_exact_frame() {
    let factory = factory();
    let mut project = Project::new("t");
    let track = project.add_track("Lead");
    project.add_device(ChainRef::Track(track), scripted::uri(kind::IMPULSE), "Impulse", None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 4.0).unwrap();
    // Beat 1 to beat 2 at 120 BPM and 48 kHz: frames 24000 to 48000.
    project
        .clip_mut(clip)
        .unwrap()
        .add_notes(&[Note { pitch: 60, start: 1.0, length: 1.0, velocity: 127, ..Default::default() }])
        .unwrap();

    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.1, ..RenderOptions::default() };
    let out = render(&project, &factory, &options).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    let sounding: Vec<usize> =
        out.left.iter().enumerate().filter(|(_, sample)| **sample != 0.0).map(|(frame, _)| frame).collect();
    assert_eq!(sounding, [24_000, 48_000], "one sample for the note-on and one for the note-off");
    assert!(out.left[24_000] > 0.0 && out.left[48_000] < 0.0);
    assert_eq!(out.left, out.right);
}

// ---- plugins that must not be taken down too soon ---------------------------

/// A plugin that goes on setting itself up after it is made, and does not
/// look whether it is still wanted when that is done, is one the host is
/// told to leave alone for a while ([`super::quirks`]): let go of young,
/// it is kept until it is old enough, and taken down then, in order.
#[test]
fn a_plugin_that_goes_on_setting_itself_up_is_not_taken_down_under_its_work() {
    let factory = factory();
    let patience = scripted::SETTING_UP * 3;
    let mut instance = create(&factory, &device(kind::SETS_UP_BEHIND));
    let serial = serial(&mut instance);
    let made = Instant::now();
    block(&mut instance, BLOCK, &[], 0.0);

    // Let go of at once. Whoever let go does not wait for it: the plugin
    // has been told to stop, and that is all.
    let DeviceInstance { processor, mut controller, .. } = instance;
    drop(processor);
    assert!(made.elapsed() < scripted::SETTING_UP, "the test was too slow to say anything");
    assert_eq!(scripted::lifecycle(serial).last(), Some(&"stop"));
    // It no longer answers for the device that had it.
    assert_eq!(controller.as_mut().unwrap().save_state(), None);

    // When it is old enough it goes, by the plugin thread's own doing,
    // and in the order VST3 requires.
    let gone = || scripted::lifecycle(serial).last() == Some(&"destroy");
    assert!(eventually(Duration::from_secs(20), gone), "{:?}", scripted::lifecycle(serial));
    assert!(made.elapsed() >= patience, "taken down after {:?}", made.elapsed());
    let ending: Vec<&str> = scripted::lifecycle(serial).into_iter().rev().take(5).rev().collect();
    assert_eq!(ending, ["stop", "deactivate", "handler cleared", "terminate", "destroy"]);
    assert!(!scripted::taken_down_under_its_work(serial));

    // One that has had its time goes when the plugin thread has next
    // served its windows.
    let mut old = create(&factory, &device(kind::SETS_UP_BEHIND));
    let older = self::serial(&mut old);
    std::thread::sleep(patience + Duration::from_millis(50));
    drop(old);
    let gone = || scripted::lifecycle(older).last() == Some(&"destroy");
    assert!(eventually(Duration::from_secs(20), gone), "{:?}", scripted::lifecycle(older));
    assert!(!scripted::taken_down_under_its_work(older));
}

/// The serial of an instance that was made on a plugin thread's state
/// directly: what its serial parameter says.
fn serial_of(created: &super::instance::Created) -> u64 {
    let serial = created.params.iter().find(|spec| spec.id == scripted::SERIAL).expect("the serial parameter");
    (serial.info.default * 1e6).round() as u64
}

fn sets_up_behind(path: std::path::PathBuf) -> super::instance::CreateRequest {
    super::instance::CreateRequest {
        path,
        class: ClassId::parse(scripted::uri(kind::SETS_UP_BEHIND).strip_prefix("vst3:").unwrap()).unwrap(),
        name: "kept".into(),
        state: None,
        values: Vec::new(),
        sample_rate: SR,
        max_block: BLOCK as u32,
        offline: false,
        key_bus: None,
    }
}

/// What a plugin posts to itself for an instance reaches the instance,
/// however long the thread was kept from its windows meanwhile. An
/// instance that is being kept is not taken down on the way out of a job
/// that outlasted its time: what the plugin posted while the job ran is
/// delivered first, and the time the job took is time the plugin has not
/// had. (Taken down in the thread's idling, as kept instances once were,
/// this plugin complains. The Archetype it stands for ended the process
/// that way: in 3 of 19 attempts at six renders of a song with three of
/// them, each time straight after the plugin thread had made the next
/// render's.) On a plugin thread of the test's own, because the test keeps
/// it busy.
#[cfg(windows)]
#[test]
fn a_plugin_that_is_kept_hears_from_its_work_before_it_is_taken_down_however_busy_the_thread_was() {
    use crate::main_thread::MainThread;
    use crate::plugin_thread::PluginThread;
    const WAIT: Duration = Duration::from_secs(20);
    let main = MainThread::<PluginThread>::spawn("kept and heard").unwrap();
    let patience = scripted::SETTING_UP * 3;
    let made = move |thread: &mut PluginThread| {
        let (path, _) = thread.vst3.preload("kept and heard", scripted::factory());
        let created =
            thread.vst3.create(sets_up_behind(path), Duration::ZERO).unwrap_or_else(|error| panic!("{error}"));
        let serial = serial_of(&created);
        thread.vst3.destroy(created.id, Some(created.audio), true);
        serial
    };
    let serial = main.call(WAIT, made).unwrap();
    let let_go = Instant::now();

    // The thread is kept from its windows until the plugin's work is done
    // and the instance's time is up, by the clock.
    let busy = patience + scripted::SETTING_UP;
    main.call(WAIT, move |_| std::thread::sleep(busy)).unwrap();
    let gone = || scripted::lifecycle(serial).last() == Some(&"destroy");
    assert!(eventually(Duration::from_secs(20), gone), "{:?}", scripted::lifecycle(serial));
    assert!(!scripted::taken_down_under_its_work(serial), "what the plugin posted to itself found the instance gone");
    // And it had its time after the job, not during it.
    assert!(let_go.elapsed() >= busy + patience / 2, "taken down {:?} after it was let go of", let_go.elapsed());
}

/// What the patience is for: without it, the same plugin is taken down
/// while it is still at work, and says so.
#[test]
fn without_that_patience_the_plugin_is_taken_down_under_its_work() {
    let config = Vst3Config {
        search_paths: Vec::new(),
        scanner: crate::Scanner::InProcess,
        cache_path: None,
        scan_timeout: Duration::from_secs(5),
        preset_paths: Vec::new(),
        vendor_places: crate::VendorPlaces::default(),
        patient_teardown: false,
    };
    let factory = Vst3Factory::with_modules_in(config, vec![("scripted", scripted::factory())]);
    let mut instance = create(&factory, &device(kind::SETS_UP_BEHIND));
    let serial = serial(&mut instance);
    drop(instance);
    assert_eq!(scripted::lifecycle(serial).last(), Some(&"destroy"), "gone at once");
    assert!(eventually(Duration::from_secs(20), || scripted::taken_down_under_its_work(serial)));
}

/// Plugins that are waiting to be old enough go with the plugin thread
/// when that ends first: nothing is left behind, and nothing waits. (The
/// thread's state is made here and not on the thread every test shares,
/// which ends when the last of them is done with it.)
#[test]
fn plugins_that_are_being_kept_go_with_the_plugin_thread() {
    use super::instance::Vst3Thread;
    use crate::main_thread::ThreadState;
    let mut thread = Vst3Thread::new();
    let (path, _) = thread.preload("kept", scripted::factory());
    let made = Instant::now();
    let created = thread.create(sets_up_behind(path), Duration::ZERO).unwrap_or_else(|error| panic!("{error}"));
    let serial = serial_of(&created);
    thread.destroy(created.id, Some(created.audio), true);
    assert_eq!(scripted::lifecycle(serial).last(), Some(&"activate"), "kept as it was");
    assert!(thread.wants_idle(), "the thread goes on looking after it");
    // Before its time it is left, whatever the thread does; the thread
    // ending takes it down.
    thread.idle(false);
    thread.served(Duration::ZERO);
    assert_eq!(scripted::lifecycle(serial).last(), Some(&"activate"));
    drop(thread);
    assert_eq!(scripted::lifecycle(serial).last(), Some(&"destroy"));
    assert!(made.elapsed() < scripted::SETTING_UP * 3, "it was not waited for");
}

// ---- channels -----------------------------------------------------------------

#[test]
fn a_plugin_is_told_each_note_wheel_and_controller_on_the_channel_it_is_on() {
    use crate::notes::{Told, heard, said};
    use trill_core::Channel;
    let factory = factory();
    let mut instance = create(&factory, &device(kind::CHANNELS));
    let channel = |number: u8| Channel::new(number).unwrap();
    let (second, sixth, twelfth) = (channel(2), channel(6), channel(12));
    let down = EventKind::NoteOn { pitch: 40, velocity: 1.0 };
    let events = [
        Event::on(sixth, 0, down),
        Event::on(second, 1, down),
        Event::new(2, down),
        // The plugin names a parameter for the wheel, the pressure and
        // the mod wheel on each of its channels...
        Event::on(sixth, 3, EventKind::PitchBend { value: 0.5 }),
        Event::on(sixth, 4, EventKind::ChannelPressure { value: 0.5 }),
        Event::on(second, 5, EventKind::Control { controller: 1, value: 0.5 }),
        // ...and for the expression pedal on its first only, which is
        // then where the second channel's goes.
        Event::on(second, 6, EventKind::Control { controller: 11, value: 0.5 }),
        Event::on(sixth, 7, EventKind::NoteOff { pitch: 40 }),
        // Its event bus has eight channels: the twelfth is its first.
        Event::on(twelfth, 8, EventKind::NoteOn { pitch: 41, velocity: 1.0 }),
        Event::on(twelfth, 9, EventKind::PitchBend { value: -0.5 }),
        Event::on(twelfth, 10, EventKind::NoteOff { pitch: 41 }),
    ];
    let (left, _) = block(&mut instance, BLOCK, &events, 0.0);
    assert_eq!(
        heard(&left),
        [
            (0, Told::NoteOn, 6),
            (1, Told::NoteOn, 2),
            (2, Told::NoteOn, 1),
            (3, Told::Wheel, 6),
            (4, Told::Pressure, 6),
            (5, Told::Controller, 2),
            (6, Told::Controller, 1),
            (7, Told::NoteOff, 6),
            (8, Told::NoteOn, 1),
            (9, Told::Wheel, 1),
            (10, Told::NoteOff, 1),
        ]
    );
    // It has no parameter for the pedal, so the host works each
    // channel's out: the second's holds the second's key and no other.
    let pedal = |value: f32| Event::on(second, 0, EventKind::Control { controller: 64, value });
    let up = |channel: Channel, offset: u32| Event::on(channel, offset, EventKind::NoteOff { pitch: 40 });
    let (left, _) = block(&mut instance, BLOCK, &[pedal(1.0), up(second, 1), up(Channel::FIRST, 2)], 0.0);
    assert_eq!(heard(&left), [(2, Told::NoteOff, 1)]);
    let (left, _) = block(&mut instance, BLOCK, &[pedal(0.0)], 0.0);
    assert_eq!(heard(&left), [(0, Told::NoteOff, 2)]);

    // A reset lets go of the keys that are down, each on its own channel,
    // and tells every channel anything was said on that all sound is off,
    // through the parameter the plugin names for that on each: the first,
    // the second and the sixth, and none of the others.
    block(&mut instance, BLOCK, &[Event::on(sixth, 0, down), Event::on(second, 1, down)], 0.0);
    instance.processor.reset();
    let (left, _) = block(&mut instance, BLOCK, &[], 0.0);
    let told = [
        said(Told::AllSoundOff, 2),
        said(Told::AllSoundOff, 6),
        said(Told::AllSoundOff, 1),
        said(Told::NoteOff, 2),
        said(Told::NoteOff, 6),
    ];
    assert_eq!(left[0], told.iter().fold(0.0, |sum, sample| sum + sample));
    assert!(left[1..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn the_engine_plays_a_plugin_each_channel_of_a_song() {
    use crate::notes::{Told, heard, said};
    let project = song_on_two_channels(scripted::uri(kind::CHANNELS));
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.1, ..RenderOptions::default() };
    let mut out = render(&project, &factory(), &options).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    // Where a song starts, its instrument is told where everything
    // stands: the sixth channel's wheel, through the parameter the plugin
    // names for it. (It names none for the pedal.)
    assert!((out.left[0] - said(Told::Wheel, 6)).abs() < 1e-4, "{} at the first frame", out.left[0]);
    out.left[0] = 0.0;
    let heard = heard(&out.left);
    // A beat is 24000 frames. The keys go down on their own channels. The
    // sixth's comes up when its note ends; the second's when its pedal
    // does, at beat 3, which the host works out for a plugin that names
    // no parameter for it.
    let notes: Vec<_> = heard.iter().filter(|(_, what, _)| matches!(what, Told::NoteOn | Told::NoteOff)).collect();
    assert_eq!(
        notes,
        [
            &(6_000, Told::NoteOn, 6),
            &(12_000, Told::NoteOn, 2),
            &(54_000, Told::NoteOff, 6),
            &(72_000, Told::NoteOff, 2)
        ]
    );
    // The wheel is the sixth channel's and no other's, all through its
    // climb.
    let wheel: Vec<_> = heard.iter().filter(|(_, what, _)| *what == Told::Wheel).collect();
    assert!(wheel.len() > 100, "{} values of the wheel", wheel.len());
    assert!(wheel.iter().all(|(_, _, channel)| *channel == 6));
    // (Its last word is where the clip ends: back to rest, on the same channel.)
    let (last, climb) = wheel.split_last().unwrap();
    assert!(climb.iter().all(|(frame, ..)| (24_000..=42_100).contains(frame)) && last.0 == 96_000);
    assert!(!heard.iter().any(|(_, what, _)| *what == Told::Controller), "the pedal is no parameter of this plugin");
}

/// A song for an instrument that gives its channels meanings: one key on
/// the sixth channel a quarter of a beat in and, a quarter later, the same
/// key on the second, both held for two beats; the sixth's wheel goes up
/// from beat 1 to beat 1.75, and the second has a pedal that is down from
/// beat 0.75 to beat 3. (No two things on one frame, so that a plugin that
/// makes a sample of each thing it is told says each of them apart.)
fn song_on_two_channels(instrument: String) -> Project {
    use trill_core::{AutomationPoint, Channel, ControllerKind};
    let (second, sixth) = (Channel::new(2).unwrap(), Channel::new(6).unwrap());
    let mut project = Project::new("t");
    let track = project.add_track("Guitar");
    project.add_device(ChainRef::Track(track), instrument, "Channels", None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 4.0).unwrap();
    let clip = project.clip_mut(clip).unwrap();
    let key = |start: f64| Note { pitch: 40, start, length: 2.0, velocity: 127, ..Default::default() };
    clip.add_notes(&[key(0.25).on(sixth), key(0.5).on(second)]).unwrap();
    let wheel = vec![AutomationPoint::new(1.0, 0.0), AutomationPoint::new(1.75, 1.0)];
    clip.set_controller_on(sixth, ControllerKind::PitchBend, wheel).unwrap();
    let held = |beat: f64, value: f64| AutomationPoint { beat, value, shape: trill_core::project::Segment::Hold };
    let pedal = vec![held(0.75, 1.0), held(3.0, 0.0)];
    clip.set_controller_on(second, ControllerKind::SUSTAIN, pedal).unwrap();
    project
}
