//! MIDI inputs and the audio device as a front end reaches them through
//! the runtime, with ports and devices that exist only here: nothing of
//! this machine's hardware, and nothing of its user's settings.

use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use trill_core::device::{
    DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor, Event, EventKind,
    ProcessInfo,
};
use trill_core::project::Device;
use trill_core::{ChainRef, TrackId};
use trill_engine::host::scripted::{ScriptedDevice, ScriptedHost};
use trill_engine::{AudioBackend, AudioConfig};
use trill_library::Settings;
use trill_runtime::midi::scripted::ScriptedMidi;
use trill_runtime::{MidiInputInfo, Parts, Runtime, RuntimeOptions, Storage};

const PROBE_URI: &str = "test:probe";

/// What the probes of a runtime were told to play, in order, and how many
/// blocks they have been through.
#[derive(Default)]
struct Probes {
    events: Mutex<Vec<EventKind>>,
    blocks: AtomicU64,
}

type Heard = Arc<Probes>;

struct Probe(Heard);

impl DeviceProcessor for Probe {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, _info: &ProcessInfo, events: &[Event], _left: &mut [f32], _right: &mut [f32]) {
        if !events.is_empty() {
            self.0.events.lock().extend(events.iter().map(|event| event.kind));
        }
        self.0.blocks.fetch_add(1, Ordering::Relaxed);
    }

    fn reset(&mut self) {}
}

struct ProbeFactory(Heard);

impl DeviceFactory for ProbeFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        vec![DeviceDescriptor {
            uri: PROBE_URI.into(),
            name: "Probe".into(),
            vendor: "Test".into(),
            category: DeviceCategory::Instrument,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != PROBE_URI {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        Ok(DeviceInstance { processor: Box::new(Probe(self.0.clone())), params: Vec::new(), controller: None })
    }
}

fn kept_in(folder: &Path) -> RuntimeOptions {
    RuntimeOptions {
        audio: AudioBackend::Null { sample_rate: 48_000 },
        plugins: None,
        storage: Storage::At { settings: folder.join("settings.json"), library_index: None },
    }
}

/// A runtime with `midi` for its ports, keeping its settings in `folder`,
/// and a track with a probe on it that is selected: what a keyboard plays.
fn start(folder: &Path, midi: &ScriptedMidi) -> (Runtime, Heard, TrackId) {
    let heard = Heard::default();
    let parts = Parts {
        devices: vec![Box::new(ProbeFactory(heard.clone()))],
        midi: Some(Box::new(midi.clone())),
        ..Parts::default()
    };
    let runtime = Runtime::start_custom(kept_in(folder), parts).expect("the runtime starts");
    let track = {
        let mut session = runtime.session.lock();
        let track = session.edit(|project| Ok(project.add_track("Keys"))).unwrap();
        session.add_device(ChainRef::Track(track), PROBE_URI, None, None).unwrap();
        session.set_selected_track(Some(track));
        track
    };
    assert!(runtime.rescan_midi_inputs());
    // The engine's first block tells every instrument to let go of
    // everything; the tests begin after that.
    let deadline = Instant::now() + Duration::from_secs(20);
    while heard.blocks.load(Ordering::Relaxed) < 2 {
        assert!(Instant::now() < deadline, "the audio thread never ran");
        std::thread::sleep(Duration::from_millis(2));
    }
    heard.events.lock().clear();
    (runtime, heard, track)
}

/// Waits until the probes have been told `wanted` more events, and
/// returns them.
fn hears(heard: &Heard, wanted: usize) -> Vec<EventKind> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let mut heard = heard.events.lock();
            if heard.len() >= wanted {
                return std::mem::take(&mut *heard);
            }
        }
        assert!(Instant::now() < deadline, "the instrument heard {:?}, not {wanted} events", heard.events.lock());
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Asserts that nothing reaches the probes for a moment.
fn hears_nothing(heard: &Heard) {
    let blocks = heard.blocks.load(Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(20);
    while heard.blocks.load(Ordering::Relaxed) < blocks + 4 {
        assert!(Instant::now() < deadline, "the audio thread stopped");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(*heard.events.lock(), [], "nothing should have been played");
}

fn input(name: &str, enabled: bool, present: bool, open: bool) -> MidiInputInfo {
    MidiInputInfo { name: name.into(), enabled, present, open, problem: None, events: 0 }
}

/// The inputs with their message counts left out.
fn inputs(runtime: &Runtime) -> Vec<MidiInputInfo> {
    runtime.midi_inputs().into_iter().map(|input| MidiInputInfo { events: 0, ..input }).collect()
}

fn on(pitch: u8) -> EventKind {
    EventKind::NoteOn { pitch, velocity: 100.0 / 127.0 }
}

#[test]
fn a_keyboard_that_is_plugged_in_plays_without_being_asked() {
    let folder = tempfile::tempdir().unwrap();
    let midi = ScriptedMidi::new();
    midi.plug("Keystation");
    let (runtime, heard, _) = start(folder.path(), &midi);
    assert_eq!(inputs(&runtime), [input("Keystation", true, true, true)]);

    // Notes with their velocity, the wheels and the pedal all arrive.
    assert!(midi.send("Keystation", &[0x90, 60, 100]));
    assert_eq!(hears(&heard, 1), [on(60)]);
    midi.send("Keystation", &[0xE0, 0, 96]);
    midi.send("Keystation", &[0xB0, 1, 127]);
    midi.send("Keystation", &[0xB0, 64, 127]);
    // Active sensing and aftertouch are counted, not played.
    midi.send("Keystation", &[0xFE]);
    midi.send("Keystation", &[0xD0, 40]);
    let played = hears(&heard, 3);
    assert!(matches!(played[0], EventKind::PitchBend { value } if (value - 0.5).abs() < 0.001), "{played:?}");
    assert_eq!(played[1], EventKind::Control { controller: 1, value: 1.0 });
    assert_eq!(played[2].sustain(), Some(true));
    assert_eq!(runtime.midi_inputs()[0].events, 6, "the lamp has something to go by");
    midi.send("Keystation", &[0xB0, 64, 0]);
    midi.send("Keystation", &[0xE0, 0, 64]);
    hears(&heard, 2);

    // A second device is plugged in while the program runs.
    midi.plug("Pads");
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(inputs(&runtime), [input("Keystation", true, true, true), input("Pads", true, true, true)]);
    midi.send("Pads", &[0x99, 36, 100]);
    assert_eq!(hears(&heard, 1), [on(36)]);

    // The keyboard is pulled out with a key, the pedal and the wheel held.
    // It cannot say they came up, so they are let go of for it. Nobody
    // knows which port a key came from, so every key that is down is let
    // go of, the pad's too: one more than need be rather than one too few.
    midi.send("Keystation", &[0xB0, 64, 127]);
    midi.send("Keystation", &[0xE0, 0, 96]);
    hears(&heard, 2);
    midi.unplug("Keystation");
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(inputs(&runtime), [input("Pads", true, true, true)]);
    let released = hears(&heard, 4);
    assert_eq!(released[..2], [EventKind::NoteOff { pitch: 36 }, EventKind::NoteOff { pitch: 60 }]);
    assert_eq!(released[2].sustain(), Some(false));
    assert_eq!(released[3], EventKind::PitchBend { value: 0.0 });
    // The pad coming up later has nothing left to end.
    midi.send("Pads", &[0x89, 36, 0]);
    hears_nothing(&heard);

    // Plugged in again, it plays again. Nothing of this needed saving.
    midi.plug("Keystation");
    assert!(runtime.rescan_midi_inputs());
    midi.send("Keystation", &[0x90, 62, 100]);
    assert_eq!(hears(&heard, 1), [on(62)]);
    assert!(!folder.path().join("settings.json").exists());
    drop(runtime);
    assert!(midi.open().is_empty(), "the ports are closed with the program");
}

#[test]
fn what_is_played_goes_to_the_armed_track() {
    let folder = tempfile::tempdir().unwrap();
    let midi = ScriptedMidi::new();
    midi.plug("Keystation");
    let (runtime, heard, selected) = start(folder.path(), &midi);
    // A second track, without an instrument to hear with: armed, it takes
    // the keyboard away from the selected one.
    let armed = runtime.session.lock().edit(|project| Ok(project.add_track("Silent"))).unwrap();
    runtime.session.lock().set_armed(armed, true).unwrap();
    midi.send("Keystation", &[0x90, 60, 100]);
    hears_nothing(&heard);
    midi.send("Keystation", &[0x80, 60, 0]);

    runtime.session.lock().set_armed(armed, false).unwrap();
    midi.send("Keystation", &[0x90, 64, 100]);
    assert_eq!(hears(&heard, 1), [on(64)]);
    assert_eq!(runtime.session.lock().live_tracks(), [selected]);
}

#[test]
fn an_input_that_is_switched_off_stays_off_and_is_remembered() {
    let folder = tempfile::tempdir().unwrap();
    let midi = ScriptedMidi::new();
    midi.plug("Keystation");
    midi.plug("Loopback");
    let (runtime, heard, _) = start(folder.path(), &midi);

    runtime.set_midi_input_enabled("Loopback", false).unwrap();
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(inputs(&runtime), [input("Keystation", true, true, true), input("Loopback", false, true, false)]);
    assert_eq!(midi.open(), ["Keystation"]);
    assert!(!midi.send("Loopback", &[0x90, 60, 100]), "nobody listens to it");
    hears_nothing(&heard);

    // Switching an input off while a key is down on it lets go of the key.
    midi.send("Keystation", &[0x90, 62, 100]);
    assert_eq!(hears(&heard, 1), [on(62)]);
    runtime.set_midi_input_enabled("Keystation", false).unwrap();
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(hears(&heard, 1), [EventKind::NoteOff { pitch: 62 }]);
    assert!(midi.open().is_empty());
    let saved = Settings::load(&folder.path().join("settings.json")).settings;
    assert_eq!(saved.midi.disabled_inputs, ["Keystation", "Loopback"]);
    drop(runtime);

    // The next run leaves them alone, and still lists the one that has
    // been unplugged meanwhile, so that it can be switched on again.
    midi.unplug("Loopback");
    midi.plug("Pads");
    let (runtime, heard, _) = start(folder.path(), &midi);
    assert_eq!(
        inputs(&runtime),
        [
            input("Keystation", false, true, false),
            input("Pads", true, true, true),
            input("Loopback", false, false, false)
        ]
    );
    assert_eq!(midi.open(), ["Pads"]);

    runtime.set_midi_input_enabled("Keystation", true).unwrap();
    runtime.set_midi_input_enabled("Loopback", true).unwrap();
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(inputs(&runtime), [input("Keystation", true, true, true), input("Pads", true, true, true)]);
    midi.send("Keystation", &[0x90, 65, 100]);
    assert_eq!(hears(&heard, 1), [on(65)]);
    assert!(Settings::load(&folder.path().join("settings.json")).settings.midi.disabled_inputs.is_empty());
    // Plugged in again, the loopback is listened to like any other.
    midi.plug("Loopback");
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(midi.open(), ["Keystation", "Loopback", "Pads"]);
}

#[test]
fn a_port_that_cannot_be_opened_says_why_and_is_tried_again() {
    let folder = tempfile::tempdir().unwrap();
    let midi = ScriptedMidi::new();
    midi.plug("Keystation");
    midi.plug("Pads");
    midi.refuse("Keystation", Some("another program is using this port"));
    let (runtime, heard, _) = start(folder.path(), &midi);
    let listed = inputs(&runtime);
    assert_eq!(listed[0].problem.as_deref(), Some("another program is using this port"));
    assert!(listed[0].enabled && listed[0].present && !listed[0].open);
    assert_eq!(listed[1], input("Pads", true, true, true), "the other port is not affected");

    // The other program lets go.
    midi.refuse("Keystation", None);
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(inputs(&runtime)[0], input("Keystation", true, true, true));

    // The system cannot list its ports for a while: what is open stays
    // open and nothing falls over.
    midi.break_listing(Some("the MIDI service is not answering"));
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(inputs(&runtime).len(), 2);
    midi.send("Pads", &[0x90, 40, 100]);
    assert_eq!(hears(&heard, 1), [on(40)]);
    midi.break_listing(None);
    assert!(runtime.rescan_midi_inputs());
    assert_eq!(midi.open(), ["Keystation", "Pads"]);
}

#[test]
fn a_run_without_audio_hardware_opens_no_midi_ports() {
    let runtime = Runtime::start(RuntimeOptions::hermetic()).unwrap();
    assert!(runtime.midi_inputs().is_empty());
    assert!(runtime.rescan_midi_inputs());
    runtime.set_midi_input_enabled("Anything", false).unwrap();
    assert!(runtime.midi_inputs().is_empty());
    let status = runtime.audio_status();
    assert_eq!((status.info.device, status.config), (None, AudioConfig::default()));
    assert!(runtime.audio_devices().outputs.is_empty());
}

fn two_devices() -> ScriptedHost {
    let host = ScriptedHost::with_speakers();
    host.add_device(ScriptedDevice::new("Interface"));
    host
}

fn start_on(folder: &Path, host: &ScriptedHost) -> Runtime {
    let parts = Parts { audio: Some(Arc::new(host.clone())), ..Parts::default() };
    Runtime::start_custom(kept_in(folder), parts).expect("the runtime starts")
}

#[test]
fn the_audio_choice_is_applied_at_once_and_at_the_next_start() {
    let folder = tempfile::tempdir().unwrap();
    let settings_file = folder.path().join("settings.json");
    let host = two_devices();
    let runtime = start_on(folder.path(), &host);
    let names = |runtime: &Runtime| -> Vec<String> {
        runtime.audio_devices().outputs.into_iter().map(|device| device.name).collect()
    };
    assert_eq!(names(&runtime), ["Speakers", "Interface"]);
    let status = runtime.audio_status();
    assert_eq!(status.info.device.as_deref(), Some("Speakers"));
    assert_eq!((status.config, status.latency_ms), (AudioConfig::default(), 30.0));
    assert!(!settings_file.exists(), "looking writes nothing");

    // A project is open and the song is somewhere in it.
    let track = runtime.session.lock().edit(|project| Ok(project.add_track("Keys"))).unwrap();
    runtime.session.lock().seek(12.0);
    host.advance(960);

    let wanted = AudioConfig {
        output: Some("Interface".into()),
        buffer_size: Some(128),
        exclusive: true,
        ..AudioConfig::default()
    };
    let (status, saved) = runtime.set_audio(wanted.clone());
    saved.unwrap();
    assert_eq!(status.info.device.as_deref(), Some("Interface"));
    assert_eq!((status.info.buffer_frames, status.info.exclusive, status.info.warning), (128, true, None));
    assert!((status.latency_ms - 128.0 / 48.0).abs() < 1e-9);
    assert_eq!(status.config, wanted);
    host.advance(256);
    {
        let session = runtime.session.lock();
        assert!(session.project().track(track).is_ok());
        assert_eq!(session.transport().position_beats, 12.0, "the playhead is where it was");
    }
    let saved = Settings::load(&settings_file).settings.audio;
    assert_eq!(
        (saved.output_device.as_deref(), saved.buffer_size, saved.exclusive),
        (Some("Interface"), Some(128), true)
    );
    drop(runtime);

    // The next run opens what was chosen.
    let host = two_devices();
    let runtime = start_on(folder.path(), &host);
    let status = runtime.audio_status();
    assert_eq!(
        (status.info.device.as_deref(), status.info.buffer_frames, status.info.exclusive),
        (Some("Interface"), 128, true)
    );
    assert_eq!(status.config, wanted);
    drop(runtime);

    // And if the interface is not plugged in that day, the program starts
    // on what there is, says so, and goes on wanting the interface.
    let runtime = start_on(folder.path(), &ScriptedHost::with_speakers());
    let status = runtime.audio_status();
    assert_eq!(status.info.device.as_deref(), Some("Speakers"));
    let warning = status.info.warning.unwrap();
    assert!(warning.contains("there is no audio device called 'Interface'"), "{warning}");
    assert_eq!(status.config, wanted);
    assert_eq!(Settings::load(&settings_file).settings.audio.output_device.as_deref(), Some("Interface"));
}

#[test]
fn nothing_is_written_when_nothing_is_kept() {
    let host = two_devices();
    let parts = Parts { audio: Some(Arc::new(host.clone())), ..Parts::default() };
    let runtime = Runtime::start_custom(RuntimeOptions::hermetic(), parts).unwrap();
    let wanted = AudioConfig { output: Some("Interface".into()), ..AudioConfig::default() };
    let (status, saved) = runtime.set_audio(wanted.clone());
    saved.unwrap();
    assert_eq!(status.info.device.as_deref(), Some("Interface"));
    assert_eq!(runtime.settings().audio.output_device.as_deref(), Some("Interface"), "for as long as the run lasts");
    assert_eq!(runtime.settings_folder(), None);
}
