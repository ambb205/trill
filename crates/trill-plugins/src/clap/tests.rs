//! The host against the scripted plugins of [`super::scripted`]: everything
//! that can be checked exactly, and everything that depends on a plugin
//! misbehaving. The integration tests cover real, installed plugins.

use super::ClapFactory;
use super::scripted::{self, kind};
use std::time::{Duration, Instant};
use trill_core::device::{
    DeviceCategory, DeviceError, DeviceFactory, DeviceInstance, Event, EventKind, ParamKind, ProcessInfo,
};
use trill_core::{ChainRef, Device, Note, Project};
use trill_engine::render::{RenderOptions, render};

const SR: f64 = 48_000.0;
const BLOCK: usize = 64;

fn factory() -> ClapFactory {
    ClapFactory::with_entries(&kind::ALL.map(scripted::entry))
}

fn uri(kind: u8) -> String {
    format!("clap:{}", scripted::id(kind))
}

fn device(kind: u8) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, uri(kind), "plugin", None).unwrap();
    project.device(id).unwrap().clone()
}

fn create(factory: &ClapFactory, device: &Device) -> DeviceInstance {
    factory.create(device, SR, BLOCK).unwrap_or_else(|error| panic!("{error}"))
}

/// Position of the parameter with CLAP id `id` in the device's list.
fn index(instance: &DeviceInstance, id: u32) -> usize {
    let id = id.to_string();
    instance.params.iter().position(|param| param.id == id).unwrap_or_else(|| panic!("no parameter {id}"))
}

/// The value the plugin currently reports for parameter `id`.
fn value(instance: &mut DeviceInstance, id: u32) -> f64 {
    let index = index(instance, id);
    instance.controller.as_mut().unwrap().param_values().expect("the plugin reports values")[index]
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

fn load_failure(factory: &ClapFactory, device: &Device) -> String {
    match factory.create(device, SR, BLOCK) {
        Err(DeviceError::LoadFailed { uri, reason }) => {
            assert_eq!(uri, device.uri);
            reason
        }
        Err(other) => panic!("expected a load failure, got: {other}"),
        Ok(_) => panic!("expected a load failure, but '{}' loaded", device.uri),
    }
}

// ---- listing -------------------------------------------------------------

#[test]
fn plugins_are_listed_by_what_they_say_they_are() {
    let factory = factory();
    let descriptors = factory.descriptors();
    assert_eq!(descriptors.len(), kind::ALL.len());
    let category = |kind| descriptors.iter().find(|descriptor| descriptor.uri == uri(kind)).unwrap().category;
    assert_eq!(category(kind::IMPULSE), DeviceCategory::Instrument);
    assert_eq!(category(kind::MIDI_IMPULSE), DeviceCategory::Instrument);
    assert_eq!(category(kind::MONO_GAIN), DeviceCategory::Effect);
    assert_eq!(category(kind::BARE), DeviceCategory::Effect);
    assert!(descriptors.iter().all(|descriptor| descriptor.vendor == "trill-plugins tests"));
}

#[test]
fn parameters_are_listed_sanitized_and_told_apart() {
    let factory = factory();
    let instance = create(&factory, &device(kind::IMPULSE));
    // The repeated id and the parameter the plugin cannot describe are out.
    assert_eq!(instance.params.len(), scripted::PARAM_COUNT);
    let param = |id| &instance.params[index(&instance, id)];

    let gain = param(scripted::GAIN);
    assert_eq!((gain.name.as_str(), gain.min, gain.max, gain.default), ("Gain", 0.0, 2.0, 1.0));
    assert_eq!((&gain.kind, gain.unit.as_str()), (&ParamKind::Float, ""));

    // Stepped parameters are integers, and the default is what the plugin
    // starts with (3), not what it declares (2).
    let steps = param(scripted::STEPS);
    assert_eq!((&steps.kind, steps.default), (&ParamKind::Int, 3.0));
    assert_eq!(steps.clamp(2.6), 3.0);

    // Same name, different modules.
    assert_eq!(param(scripted::CUTOFF_1).name, "Filter 1/Cutoff");
    assert_eq!(param(scripted::CUTOFF_2).name, "Filter 2/Cutoff");
    // A value the plugin only settles on while activating.
    assert_eq!(param(scripted::CUTOFF_1).default, scripted::SETTLED_CUTOFF);

    // Declared as 1..-1 with a NaN default.
    let reversed = param(scripted::REVERSED);
    assert_eq!((reversed.min, reversed.max, reversed.default), (-1.0, 1.0, 0.25));
    assert_eq!(reversed.clamp(5.0), 1.0);
}

#[test]
fn a_value_is_shown_in_the_plugins_words_and_found_from_them() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::MONO_GAIN));
    let at = |id| index(&instance, id);
    let (gain, steps) = (at(scripted::GAIN), at(scripted::STEPS));
    let (first, second) = (at(scripted::CUTOFF_1), at(scripted::CUTOFF_2));
    let controller = instance.controller.as_mut().unwrap();
    let text = |text: &str| Some(text.to_string());

    // Whatever value is asked about, not only the one it has now; and
    // nothing for a parameter there is not.
    let shown = controller.param_texts(&[(gain, 0.5), (steps, 3.0), (first, 440.0), (9999, 0.0)]);
    assert_eq!(shown, [text("0.50 x"), text("Plate"), text("440 Hz"), None]);

    // The plugin reads its own gain, with the unit or without.
    assert_eq!(controller.param_value(gain, "1.5 x"), Some(1.5));
    assert_eq!(controller.param_value(gain, "1.5"), Some(1.5));
    assert_eq!(controller.param_value(gain, "loud"), None);
    // It does not convert the name of a step, which is then found among
    // the names it shows.
    assert_eq!(controller.param_value(steps, "hall"), Some(2.0));
    assert_eq!(controller.param_value(steps, " SPRING "), Some(4.0));
    assert_eq!(controller.param_value(steps, "Cathedral"), None);
    // Nor a cutoff, which is found by its number.
    let found = controller.param_value(first, "1000 Hz").unwrap();
    assert_eq!(controller.param_texts(&[(first, found)]), [text("1000 Hz")]);

    // Asked about the second cutoff, the plugin says 20 whatever the text.
    // That is believed only where it shows 20 as what was asked for.
    assert_eq!(controller.param_value(second, "20 Hz"), Some(20.0));
    let found = controller.param_value(second, "8000").unwrap();
    assert_eq!(controller.param_texts(&[(second, found)]), [text("8000 Hz")]);
    assert_eq!(controller.param_value(second, "8 kHz"), None, "not a unit it shows");
    assert_eq!(controller.param_value(second, "30000 Hz"), None, "not a value it has");
    assert_eq!(controller.param_value(9999, "1"), None);
}

#[test]
fn a_plugin_says_what_channels_it_has_and_what_its_own_defaults_are() {
    let factory = factory();
    let effect = create(&factory, &device(kind::MONO_GAIN));
    let layout = effect.controller.as_ref().unwrap().channels().unwrap();
    assert_eq!((layout.inputs, layout.outputs, layout.folds_input()), (1, 1, true));
    assert_eq!(layout.describe(), "mono in, mono out");
    let instrument = create(&factory, &device(kind::IMPULSE));
    let layout = instrument.controller.as_ref().unwrap().channels().unwrap();
    assert_eq!((layout.describe().as_str(), layout.folds_input()), ("no audio in, mono out", false));

    // A parameter is listed with the value it started with as its default
    // (3); the plugin's own default is the one it declares (2).
    let defaults = effect.controller.as_ref().unwrap().param_defaults().unwrap();
    assert_eq!(defaults.len(), effect.params.len());
    let steps = index(&effect, scripted::STEPS);
    assert_eq!((effect.params[steps].default, defaults[steps]), (3.0, 2.0));
    let gain = index(&effect, scripted::GAIN);
    assert_eq!((effect.params[gain].default, defaults[gain]), (1.0, 1.0));
    // A default that is no number is one the host makes up, in the range.
    let reversed = defaults[index(&effect, scripted::REVERSED)];
    assert!((-1.0..=1.0).contains(&reversed), "{reversed}");
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
}

#[test]
fn a_parameter_set_between_blocks_applies_from_the_first_frame() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let (gain, serial) = (index(&instance, scripted::GAIN), index(&instance, scripted::SERIAL));
    let original_serial = value(&mut instance, scripted::SERIAL);

    instance.processor.set_param(gain, 0.1);
    instance.processor.set_param(gain, 2.0); // the last value wins
    instance.processor.set_param(serial, 0.5); // read-only: never sent
    instance.processor.set_param(10_000, 1.0); // no such parameter
    let (left, _) = block(&mut instance, BLOCK, &[on(0, 0.5), on(9, 0.25)], 0.0);
    assert_eq!(left, impulses(BLOCK, &[(0, 1.0), (9, 0.5)]));
    assert_eq!(value(&mut instance, scripted::GAIN), 2.0);
    assert_eq!(value(&mut instance, scripted::SERIAL), original_serial);
}

#[test]
fn an_oversized_block_reaches_the_plugin_in_pieces_it_was_activated_for() {
    let factory = factory();
    let mut instance = factory.create(&device(kind::IMPULSE), SR, 32).unwrap();
    let events = [on(0, 1.0), on(31, 0.5), on(32, 0.25), on(99, 0.125)];
    let (left, right) = block(&mut instance, 100, &events, 0.0);
    assert_eq!(left, impulses(100, &[(0, 1.0), (31, 0.5), (32, 0.25), (99, 0.125)]));
    assert_eq!(right, left);
    assert_eq!(value(&mut instance, scripted::LARGEST_BLOCK), 32.0);
    assert_eq!(value(&mut instance, scripted::BLOCKS), 4.0);
}

#[test]
fn a_mono_effect_gets_both_sides_mixed_and_replaces_them() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::MONO_GAIN));
    let (mut left, mut right) = (vec![1.0f32; BLOCK], vec![0.5f32; BLOCK]);
    instance.processor.process(&playing(), &[on(0, 1.0)], &mut left, &mut right);
    assert_eq!(left, vec![0.75; BLOCK], "the mono input is the average of both sides");
    assert_eq!(right, left, "the mono output replaces both sides");

    instance.processor.set_param(index(&instance, scripted::GAIN), 2.0);
    let (left, right) = block(&mut instance, BLOCK, &[], 0.5);
    // `block` puts 0.5 left and -0.5 right, which cancel in mono.
    assert_eq!((left, right), (vec![0.0; BLOCK], vec![0.0; BLOCK]));
}

#[test]
fn a_plugin_that_only_takes_midi_gets_midi() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::MIDI_IMPULSE));
    let (left, _) = block(&mut instance, BLOCK, &[on(3, 1.0), off(9), on(20, 0.5)], 0.0);
    // Velocity 0.5 is MIDI velocity 64.
    assert_eq!(left, impulses(BLOCK, &[(3, 1.0), (9, -0.5), (20, 64.0 / 127.0)]));

    // One key is still down. A reset releases it and sends "all sound off".
    instance.processor.reset();
    let (left, _) = block(&mut instance, BLOCK, &[], 0.0);
    assert_eq!(left, impulses(BLOCK, &[(0, -0.5 - 0.125)]));
    assert_eq!(value(&mut instance, scripted::RESETS), 1.0);
}

#[test]
fn all_notes_off_and_reset_end_exactly_the_notes_that_are_down() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let key = |offset, pitch| Event::new(offset, EventKind::NoteOn { pitch, velocity: 1.0 });
    block(&mut instance, BLOCK, &[key(0, 60), key(0, 64), key(0, 67), off(1)], 0.0);

    // Two keys are down: two note-offs, at the event's frame.
    let flush = Event::new(5, EventKind::AllNotesOff);
    let (left, _) = block(&mut instance, BLOCK, &[flush], 0.0);
    assert_eq!(left, impulses(BLOCK, &[(5, -1.0)]));
    let (left, _) = block(&mut instance, BLOCK, &[flush], 0.0);
    assert_eq!(left, impulses(BLOCK, &[]), "nothing is down any more");

    // Reset: the plugin's own reset, plus a note-off and a choke per key.
    block(&mut instance, BLOCK, &[key(0, 72)], 0.0);
    instance.processor.reset();
    assert_eq!(value(&mut instance, scripted::RESETS), 1.0);
    let (left, _) = block(&mut instance, BLOCK, &[key(8, 60)], 0.0);
    assert_eq!(left, impulses(BLOCK, &[(0, -0.5 - 0.25), (8, 1.0)]));
    let (left, _) = block(&mut instance, BLOCK, &[], 0.0);
    assert_eq!(left, impulses(BLOCK, &[]), "the reset is carried out once");
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
        loop_range: None,
    }
    .steady();
    let (mut left, mut right) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    instance.processor.process(&info, &[], &mut left, &mut right);
    assert_eq!(value(&mut instance, scripted::SEEN_TEMPO), 90.0);
    assert_eq!(value(&mut instance, scripted::SEEN_BEATS), 12.5);
    assert!((value(&mut instance, scripted::SEEN_SECONDS) - 12.5 * 60.0 / 90.0).abs() < 1e-8);
    assert_eq!(value(&mut instance, scripted::SEEN_PLAYING), 1.0);
    assert_eq!(value(&mut instance, scripted::SEEN_STEADY_TIME), 0.0);

    info = ProcessInfo { playing: false, position_beats: 3.0, ..info }.steady();
    instance.processor.process(&info, &[], &mut left, &mut right);
    assert_eq!(value(&mut instance, scripted::SEEN_PLAYING), 0.0);
    assert_eq!(value(&mut instance, scripted::SEEN_BEATS), 3.0);
    assert_eq!(value(&mut instance, scripted::SEEN_STEADY_TIME), BLOCK as f64, "the clock counts frames");
}

/// The scripted plugin as [`crate::transport_tests`] stages it.
fn stage(factory: &ClapFactory) -> crate::transport_tests::Stage<'_> {
    crate::transport_tests::Stage {
        format: crate::transport::written::Format::Clap,
        factory,
        uri: uri(kind::IMPULSE),
        serial: |instance| value(instance, scripted::SERIAL) as u64,
    }
}

#[test]
fn a_plugin_with_a_second_input_hears_the_track_its_device_listens_to() {
    let factory = factory();
    crate::key_tests::a_plugin_with_a_key_input_hears_the_track_its_device_listens_to(&crate::key_tests::Stage {
        format: crate::transport::written::Format::Clap,
        factory: &factory,
        instrument: uri(kind::IMPULSE),
        keyed: uri(kind::KEYED),
        serial: |instance| value(instance, scripted::SERIAL) as u64,
        // CLAP has nothing to switch on.
        key_input: None,
    });
    // Only a plugin that has the input takes a key.
    assert!(create(&factory, &device(kind::KEYED)).processor.takes_key());
    assert!(!create(&factory, &device(kind::MONO_GAIN)).processor.takes_key());
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

// ---- model values and state ----------------------------------------------

#[test]
fn model_values_are_in_place_before_the_first_block() {
    let factory = factory();
    let mut model = device(kind::IMPULSE);
    model.params.insert(scripted::GAIN.to_string(), 0.5);
    model.params.insert(scripted::STEPS.to_string(), 99.0); // clamped to the range
    // Read-only. Serials are whole numbers, so 0.5 can only be this value.
    model.params.insert(scripted::SERIAL.to_string(), 0.5);
    model.params.insert("424242".into(), 1.0); // not a parameter of this plugin
    model.params.insert("cutoff".into(), 1.0); // not a CLAP id at all
    let mut instance = create(&factory, &model);

    assert_eq!(value(&mut instance, scripted::GAIN), 0.5);
    assert_eq!(value(&mut instance, scripted::STEPS), 4.0);
    assert_ne!(value(&mut instance, scripted::SERIAL), 0.5);
    // The defaults are the values without the model's: what the engine
    // returns to when a setting is removed.
    assert_eq!(instance.params[index(&instance, scripted::GAIN)].default, 1.0);
    assert_eq!(instance.params[index(&instance, scripted::STEPS)].default, 3.0);

    // The values went in while the plugin was inactive, between two
    // activations, so it starts from them.
    let serial = value(&mut instance, scripted::SERIAL) as u64;
    assert_eq!(scripted::lifecycle(serial), ["init", "activate", "deactivate", "activate"]);
    let (left, _) = block(&mut instance, BLOCK, &[on(0, 1.0)], 0.0);
    assert_eq!(left[0], 0.5);

    // Without model values there is nothing to apply and one activation.
    let mut plain = create(&factory, &device(kind::IMPULSE));
    let serial = value(&mut plain, scripted::SERIAL) as u64;
    assert_eq!(scripted::lifecycle(serial), ["init", "activate"]);
}

#[test]
fn state_round_trips_and_model_values_go_on_top_of_it() {
    let factory = factory();
    let mut original = create(&factory, &device(kind::IMPULSE));
    original.processor.set_param(index(&original, scripted::GAIN), 0.5);
    original.processor.set_param(index(&original, scripted::STEPS), 1.0);
    block(&mut original, BLOCK, &[], 0.0);
    let state = original.controller.as_mut().unwrap().save_state().expect("the plugin has state");
    assert!(state.starts_with(b"FAKE"), "the state is the plugin's own bytes");

    let mut model = device(kind::IMPULSE);
    model.state = Some(state);
    let mut restored = create(&factory, &model);
    assert_eq!(value(&mut restored, scripted::GAIN), 0.5);
    assert_eq!(value(&mut restored, scripted::STEPS), 1.0);
    // The restored state is the new baseline.
    assert_eq!(restored.params[index(&restored, scripted::GAIN)].default, 0.5);

    model.params.insert(scripted::GAIN.to_string(), 1.5);
    let mut both = create(&factory, &model);
    assert_eq!(value(&mut both, scripted::GAIN), 1.5);
    assert_eq!(value(&mut both, scripted::STEPS), 1.0);
    assert_eq!(both.params[index(&both, scripted::GAIN)].default, 0.5);

    // An empty state is no state.
    let mut empty = device(kind::IMPULSE);
    empty.state = Some(Vec::new());
    assert_eq!(value(&mut create(&factory, &empty), scripted::GAIN), 1.0);
}

#[test]
fn a_state_the_plugin_rejects_fails_the_device() {
    let factory = factory();
    let mut model = device(kind::IMPULSE);
    model.state = Some(b"saved by some other plugin".to_vec());
    let reason = load_failure(&factory, &model);
    assert!(reason.contains("rejected its saved state"), "{reason}");
}

// ---- threads and lifecycle -----------------------------------------------

#[test]
fn the_plugin_is_called_on_the_threads_it_was_promised() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    // Activation happened on a thread the host calls main and not audio.
    assert_eq!(value(&mut instance, scripted::SEEN_MAIN_THREAD), 1.0);
    // Processing happens on one it calls audio and not main, even though
    // this is just the test's thread.
    block(&mut instance, BLOCK, &[], 0.0);
    assert_eq!(value(&mut instance, scripted::SEEN_AUDIO_THREAD), 1.0);

    // The plugin asked for a main-thread callback from its first block.
    let deadline = Instant::now() + Duration::from_secs(10);
    while value(&mut instance, scripted::CALLBACKS) < 1.0 {
        assert!(Instant::now() < deadline, "the plugin's main-thread callback was never made");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_lifecycle_runs_in_order_through_to_destruction() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let serial = value(&mut instance, scripted::SERIAL) as u64;
    block(&mut instance, BLOCK, &[], 0.0);
    block(&mut instance, BLOCK, &[], 0.0);

    // A new sample rate means deactivating and activating again.
    instance.processor.prepare(44_100.0, 32);
    assert_eq!(value(&mut instance, scripted::SEEN_SAMPLE_RATE), 44_100.0);
    assert_eq!(value(&mut instance, scripted::SEEN_MAX_FRAMES), 32.0);
    instance.processor.prepare(44_100.0, 32); // nothing changed: nothing happens
    let (left, _) = block(&mut instance, 32, &[on(31, 1.0)], 0.0);
    assert_eq!(left[31], 1.0);

    let DeviceInstance { processor, mut controller, .. } = instance;
    drop(processor);
    assert_eq!(
        scripted::lifecycle(serial),
        ["init", "activate", "start", "stop", "deactivate", "activate", "start", "stop", "deactivate", "destroy"]
    );
    // The controller outlives the plugin, and says so quietly.
    let controller = controller.as_mut().unwrap();
    assert_eq!(controller.save_state(), None);
    assert_eq!(controller.param_values(), None);
}

#[test]
fn an_instance_never_processed_is_torn_down_without_being_started() {
    let factory = factory();
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let serial = value(&mut instance, scripted::SERIAL) as u64;
    drop(instance);
    assert_eq!(scripted::lifecycle(serial), ["init", "activate", "deactivate", "destroy"]);
}

// ---- plugins that do not cooperate ---------------------------------------

#[test]
fn a_plugin_without_any_extension_is_hosted_as_a_passthrough() {
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
}

#[test]
fn plugins_that_cannot_be_loaded_are_errors_not_panics() {
    let factory = factory();
    let reason = load_failure(&factory, &device(kind::FAILS_INIT));
    assert!(reason.contains("instantiate"), "{reason}");
    let reason = load_failure(&factory, &device(kind::FAILS_ACTIVATE));
    assert!(reason.contains("activate"), "{reason}");

    // The host is none the worse for it.
    let mut instance = create(&factory, &device(kind::IMPULSE));
    let (left, _) = block(&mut instance, BLOCK, &[on(0, 1.0)], 0.0);
    assert_eq!(left[0], 1.0);
}

#[test]
fn a_plugin_that_fails_while_running_is_bypassed() {
    let factory = factory();

    // Refuses to start: audio passes through untouched from the outset.
    let mut never = create(&factory, &device(kind::FAILS_START));
    for _ in 0..3 {
        let (left, right) = block(&mut never, BLOCK, &[], 0.5);
        assert_eq!((left, right), (vec![0.5; BLOCK], vec![-0.5; BLOCK]));
    }

    // Fails its third block: two blocks processed (to mono: silence), then
    // audio passes through.
    let mut later = create(&factory, &device(kind::FAILS_PROCESS));
    let serial = value(&mut later, scripted::SERIAL) as u64;
    for expected in [0.0, 0.0, 0.5, 0.5] {
        let (left, right) = block(&mut later, BLOCK, &[], 0.5);
        assert_eq!((left, right), (vec![expected; BLOCK], vec![-expected; BLOCK]));
    }
    assert_eq!(value(&mut later, scripted::BLOCKS), 3.0, "a failed plugin is not asked again");
    // It is still torn down properly.
    drop(later);
    assert_eq!(scripted::lifecycle(serial), ["init", "activate", "start", "stop", "deactivate", "destroy"]);
}

#[test]
fn a_plugin_made_for_a_render_is_told_so_before_it_is_activated() {
    use trill_core::device::Purpose;
    let factory = factory();
    let mut played = create(&factory, &device(kind::IMPULSE));
    let mut rendered = factory
        .create_for(&device(kind::IMPULSE), SR, BLOCK, Purpose::Rendering)
        .unwrap_or_else(|error| panic!("{error}"));
    let steps_of = |instance: &mut DeviceInstance| scripted::lifecycle(value(instance, scripted::SERIAL) as u64);
    let steps = steps_of(&mut played);
    assert!(!steps.iter().any(|step| step.starts_with("render")), "left as it comes: {steps:?}");
    let steps = steps_of(&mut rendered);
    let told = steps.iter().position(|step| *step == "render offline").expect("told it is rendered");
    let activated = steps.iter().position(|step| *step == "activate").expect("activated");
    assert!(told < activated, "{steps:?}");
}

// ---- through the engine --------------------------------------------------

#[test]
fn the_engine_renders_a_plugin_note_on_its_exact_frame() {
    let factory = factory();
    let mut project = Project::new("t");
    let track = project.add_track("Lead");
    project.add_device(ChainRef::Track(track), uri(kind::IMPULSE), "Impulse", None).unwrap();
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

// ---- channels -----------------------------------------------------------------

#[test]
fn a_plugin_is_told_each_note_wheel_and_controller_on_the_channel_it_is_on() {
    use crate::notes::{Told, heard, said};
    use trill_core::Channel;
    let factory = factory();
    let mut instance = create(&factory, &device(kind::CHANNELS));
    let (second, sixth) = (Channel::new(2).unwrap(), Channel::new(6).unwrap());
    let down = EventKind::NoteOn { pitch: 40, velocity: 1.0 };
    let events = [
        Event::on(sixth, 0, down),
        Event::on(second, 1, down),
        Event::new(2, down),
        Event::on(sixth, 3, EventKind::PitchBend { value: 0.5 }),
        Event::on(sixth, 4, EventKind::ChannelPressure { value: 0.5 }),
        Event::on(second, 5, EventKind::Control { controller: 11, value: 0.5 }),
        Event::on(sixth, 6, EventKind::NoteOff { pitch: 40 }),
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
            (6, Told::NoteOff, 6),
        ]
    );
    // A reset lets go of the two keys that are down, each on its own
    // channel, chokes them, and tells every channel anything was said on
    // that all sound is off: all of it on the block's first frame.
    instance.processor.reset();
    let (left, _) = block(&mut instance, BLOCK, &[], 0.0);
    let told = [
        said(Told::NoteOff, 1),
        said(Told::Choke, 1),
        said(Told::NoteOff, 2),
        said(Told::Choke, 2),
        said(Told::AllSoundOff, 1),
        said(Told::AllSoundOff, 2),
        said(Told::AllSoundOff, 6),
    ];
    assert_eq!(left[0], told.iter().fold(0.0, |sum, sample| sum + sample));
    assert!(left[1..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn the_engine_plays_a_plugin_each_channel_of_a_song() {
    use crate::notes::{Told, heard, said};
    let project = song_on_two_channels(uri(kind::CHANNELS));
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.1, ..RenderOptions::default() };
    let mut out = render(&project, &factory(), &options).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    // Where a song starts, its instrument is told where everything
    // stands: the sixth channel's wheel and the second's pedal, each on
    // its own channel, both on the first frame.
    let first = said(Told::Wheel, 6) + said(Told::Controller, 2);
    assert!((out.left[0] - first).abs() < 1e-4, "{} at the first frame", out.left[0]);
    out.left[0] = 0.0;
    let heard = heard(&out.left);
    // A beat is 24000 frames. The keys: down and up on their own channels.
    let notes: Vec<_> = heard.iter().filter(|(_, what, _)| matches!(what, Told::NoteOn | Told::NoteOff)).collect();
    assert_eq!(
        notes,
        [
            &(6_000, Told::NoteOn, 6),
            &(12_000, Told::NoteOn, 2),
            &(54_000, Told::NoteOff, 6),
            &(60_000, Told::NoteOff, 2)
        ]
    );
    // The wheel is the sixth channel's and no other's, all through its
    // climb; the pedal is the second's.
    let wheel: Vec<_> = heard.iter().filter(|(_, what, _)| *what == Told::Wheel).collect();
    assert!(wheel.len() > 100, "{} values of the wheel", wheel.len());
    assert!(wheel.iter().all(|(_, _, channel)| *channel == 6));
    // (Its last word is where the clip ends: back to rest, on the same channel.)
    let (last, climb) = wheel.split_last().unwrap();
    assert!(climb.iter().all(|(frame, ..)| (24_000..=42_100).contains(frame)) && last.0 == 96_000);
    let pedal: Vec<_> = heard.iter().filter(|(_, what, _)| *what == Told::Controller).collect();
    // (A curve is looked at every 32 frames of the song: 18000 is between two of them.)
    assert_eq!(pedal, [&(18_016, Told::Controller, 2), &(72_000, Told::Controller, 2)]);
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
    let held = |beat: f64, value: f64| AutomationPoint { beat, value, shape: trill_core::project::Segment::Hold, id: 0 };
    let pedal = vec![held(0.75, 1.0), held(3.0, 0.0)];
    clip.set_controller_on(second, ControllerKind::SUSTAIN, pedal).unwrap();
    project
}
