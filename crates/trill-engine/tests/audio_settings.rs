//! Choosing an audio device while the program runs, and losing one: the
//! project, its history and the playhead outlive every change, and every
//! failure ends in a stream that works and a sentence that says why it is
//! not the one asked for.

mod common;
mod rig;

use common::{rms, zero_crossing_hz};
use rig::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use trill_core::device::{
    CompositeFactory, DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    Event, EventKind, ProcessInfo,
};
use trill_core::project::Device;
use trill_core::{ChainRef, Note};
use trill_dsp::BuiltinFactory;
use trill_engine::host::scripted::{ScriptedDevice, ScriptedHost};
use trill_engine::host::{NullHost, StreamFailure, StreamRequest};
use trill_engine::{AudioBackend, AudioConfig, AudioSetup, Patience, Session, SessionError};
use trill_media::MediaCache;

/// `Speakers`, and an interface that also runs at 96 kHz.
fn two_devices() -> ScriptedHost {
    let host = ScriptedHost::with_speakers();
    host.add_device(ScriptedDevice {
        sample_rates: vec![48_000, 44_100, 96_000],
        shared_latency: 960,
        ..device("Interface")
    });
    host
}

fn wish(output: &str) -> AudioConfig {
    AudioConfig { output: Some(output.into()), ..AudioConfig::default() }
}

/// Does the session's chores until `done`, for at most two seconds.
fn tend_until(rig: &mut Rig, mut done: impl FnMut(&Session) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        rig.session.housekeeping();
        if done(&rig.session) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_devices_are_listed_and_the_stream_says_what_it_is() {
    let mut rig = Rig::with(two_devices(), AudioConfig::default(), Patience::default());
    let devices = rig.session.audio_devices();
    assert_eq!(
        devices.outputs.iter().map(|device| device.name.as_str()).collect::<Vec<_>>(),
        ["Speakers", "Interface"]
    );
    assert!(devices.outputs[0].is_default && !devices.outputs[1].is_default);
    assert_eq!(devices.outputs[1].sample_rates, [44_100, 48_000, 96_000]);
    assert_eq!(devices.outputs[1].default_sample_rate, 48_000);
    assert!(devices.outputs[1].exclusive);

    // Nothing asked for: the system's default device, as the system runs it.
    assert_eq!(rig.session.audio_config(), &AudioConfig::default());
    let info = rig.session.audio_info().clone();
    assert_eq!(info.device.as_deref(), Some("Speakers"));
    assert_eq!(info.output, "Speakers (48000 Hz)");
    assert_eq!((info.sample_rate, info.buffer_frames, info.latency_frames), (48_000.0, 480, 1_440));
    assert_eq!((info.buffer_ms(), info.latency_ms()), (10.0, 30.0));
    assert!(!info.exclusive && info.warning.is_none());
    rig.run(1);
    assert_eq!(rig.session.output_latency_ms(), 30.0);
    assert_eq!(
        rig.host.requests(),
        [StreamRequest { device: None, sample_rate: None, buffer_size: None, exclusive: false }]
    );
}

#[test]
fn the_device_and_buffer_change_under_a_running_song_without_losing_it() {
    let mut rig = Rig::with(two_devices(), AudioConfig::default(), Patience::default());
    let track = rig.probe_track("Keys");
    let clip = rig.session.edit(|project| project.add_midi_clip(track, "Part", 0.0, 16.0)).unwrap();
    let note = Note { pitch: 60, start: 4.0, length: 1.0, velocity: 100 };
    rig.session.edit(|project| project.clip_mut(clip)?.add_notes(&[note])).unwrap();
    rig.session.play();
    rig.run(100);
    let project = rig.session.project().clone();
    let (step, revision) = (rig.session.undo_step(), rig.session.audio_revision());
    let before = rig.session.transport();
    assert!(before.playing && before.position_beats > 1.9);
    rig.probes.take();

    // To the interface, for this program alone, in small buffers.
    let asked = AudioConfig { buffer_size: Some(64), exclusive: true, sample_rate: Some(48_000), ..wish("Interface") };
    rig.session.set_audio_config(asked.clone());
    let info = rig.session.audio_info().clone();
    assert_eq!(info.device.as_deref(), Some("Interface"));
    assert_eq!((info.buffer_frames, info.latency_frames, info.exclusive), (64, 64, true));
    assert!((info.latency_ms() - 64.0 / 48.0).abs() < 1e-9);
    assert_eq!(info.output, "Interface (48000 Hz, exclusive)");
    assert_eq!(info.warning, None);
    assert_eq!(rig.session.audio_config(), &asked);
    assert_eq!(rig.session.audio_revision(), revision + 1);
    assert_eq!(rig.host.running().unwrap().buffer_frames, 64);

    // The project, its history and the song as it plays are untouched,
    // and no device was made again.
    assert_eq!(rig.session.project(), &project);
    assert_eq!(rig.session.undo_step(), step);
    assert_eq!(rig.probes.created.load(Ordering::Relaxed), 1);
    rig.run(1);
    let after = rig.session.transport();
    assert!(after.playing, "the song plays on");
    assert!((after.position_beats - before.position_beats).abs() < 0.03, "{before:?} then {after:?}");
    // Nothing may hang over the change: every note was let go of.
    assert!(rig.probes.events().iter().any(|(_, kind)| *kind == EventKind::AllNotesOff));
    // And the song carries on into its note.
    rig.run_until(rig.now() + 2.5 * FRAMES_PER_BEAT);
    assert!(rig.probes.events().iter().any(|(_, kind)| matches!(kind, EventKind::NoteOn { pitch: 60, .. })));

    // Back to sharing it: the size asked for is no longer the device's to
    // give, and the stream says what it got.
    rig.session.set_audio_config(AudioConfig { buffer_size: Some(64), ..wish("Interface") });
    let info = rig.session.audio_info();
    assert_eq!((info.buffer_frames, info.latency_frames, info.exclusive), (480, 960, false));
    assert_eq!(info.warning, None);
}

#[test]
fn a_new_sample_rate_makes_the_devices_again_and_keeps_the_song() {
    let mut rig = Rig::new();
    let probe = rig.probe_track("Probe");
    let keys = rig.sine_track("Keys");
    let clip = rig.session.edit(|project| project.add_midi_clip(keys, "A", 0.0, 8.0)).unwrap();
    let note = Note { pitch: 69, start: 4.0, length: 2.0, velocity: 100 };
    rig.session.edit(|project| project.clip_mut(clip)?.add_notes(&[note])).unwrap();
    rig.session.set_metronome(true);
    rig.session.set_armed(probe, true).unwrap();
    rig.session.seek(3.0);
    rig.session.play();
    rig.run(50);
    let project = rig.session.project().clone();
    let step = rig.session.undo_step();
    let position = rig.session.transport().position_beats;
    assert!(position > 3.9);

    rig.session.set_audio_config(AudioConfig { sample_rate: Some(44_100), ..AudioConfig::default() });
    let info = rig.session.audio_info().clone();
    assert_eq!((info.sample_rate, info.device.as_deref(), info.warning), (44_100.0, Some("Speakers"), None));
    assert_eq!(rig.session.project(), &project);
    assert_eq!(rig.session.undo_step(), step);
    assert_eq!(rig.probes.created.load(Ordering::Relaxed), 2, "every device is made again for the new rate");
    assert!(rig.session.problems().is_empty());

    // The playhead is where it was; the song has stopped.
    rig.run(2);
    let transport = rig.session.transport();
    assert!(!transport.playing);
    assert!((transport.position_beats - position).abs() < 1e-9, "{transport:?} against {position}");
    // What the audio thread had been told, it was told again: the armed
    // track still plays, the metronome is still on.
    rig.probes.take();
    rig.session.play_live(on(72));
    rig.run(2);
    assert_eq!(rig.probes.events().len(), 1);
    assert!(rig.session.metronome());

    // And the song plays in tune at the new rate.
    rig.session.seek(4.0);
    rig.host.keep_output();
    rig.host.take_output();
    rig.session.set_metronome(false);
    rig.session.play();
    rig.run(100);
    let (left, _) = rig.host.take_output();
    let tone = &left[4_410..30_000];
    assert!(rms(tone) > 0.01, "the note should be sounding");
    let pitch = zero_crossing_hz(tone, 44_100.0);
    assert!((pitch - 440.0).abs() < 2.0, "A4 came out at {pitch} Hz");
}

#[test]
fn what_cannot_be_had_is_replaced_by_the_nearest_thing_and_says_so() {
    let host = two_devices();
    host.add_device(ScriptedDevice { exclusive: false, ..device("Monitor") });
    let mut rig = Rig::with(host, AudioConfig::default(), Patience::default());
    let project = rig.session.project().clone();

    // A rate the device does not run at: its own rate.
    let asked = AudioConfig { sample_rate: Some(96_000), ..wish("Speakers") };
    rig.session.set_audio_config(asked.clone());
    let info = rig.session.audio_info().clone();
    assert_eq!((info.device.as_deref(), info.sample_rate), (Some("Speakers"), 48_000.0));
    let warning = info.warning.unwrap();
    assert!(warning.contains("'Speakers' (96000 Hz): 'Speakers' does not run at 96000 Hz"), "{warning}");
    assert!(warning.ends_with("Using Speakers (48000 Hz) instead."), "{warning}");
    assert_eq!(rig.session.audio_config(), &asked, "the wish is kept as it was made");

    // A device that cannot be taken exclusively: shared.
    rig.session.set_audio_config(AudioConfig { exclusive: true, buffer_size: Some(128), ..wish("Monitor") });
    let info = rig.session.audio_info().clone();
    assert_eq!((info.device.as_deref(), info.exclusive, info.buffer_frames), (Some("Monitor"), false, 480));
    let warning = info.warning.unwrap();
    assert!(warning.contains("cannot be taken exclusively") && warning.contains("Using Monitor"), "{warning}");

    // A device that is not there: the system's default.
    rig.host.set_default("Interface");
    rig.session.set_audio_config(wish("Headphones"));
    let info = rig.session.audio_info().clone();
    assert_eq!(info.device.as_deref(), Some("Interface"));
    let warning = info.warning.unwrap();
    assert!(warning.contains("there is no audio device called 'Headphones'"), "{warning}");
    assert!(warning.ends_with("Using Interface (48000 Hz) instead."), "{warning}");

    // And what can be had, is, without a word.
    rig.session.set_audio_config(AudioConfig { sample_rate: Some(96_000), ..wish("Interface") });
    let info = rig.session.audio_info().clone();
    assert_eq!((info.device.as_deref(), info.sample_rate, info.warning), (Some("Interface"), 96_000.0, None));
    assert_eq!(rig.session.project(), &project);
}

#[test]
fn with_no_device_at_all_the_program_runs_without_sound_or_says_why_it_cannot() {
    // Nothing to play through: the session starts all the same.
    let mut rig = Rig::with(ScriptedHost::new(), AudioConfig::default(), Patience::default());
    let info = rig.session.audio_info().clone();
    assert_eq!((info.device, info.sample_rate), (None, 48_000.0));
    let warning = info.warning.unwrap();
    assert!(warning.starts_with("no audio output (") && warning.ends_with("running without sound"), "{warning}");
    assert!(warning.contains("the system has no audio output device"), "{warning}");
    assert!(rig.session.audio_devices().outputs.is_empty());
    // It runs: the song moves, at the pace of a clock.
    let track = rig.sine_track("Keys");
    rig.session.play();
    let moved = tend_until(&mut rig, |session| session.transport().position_beats > 0.05);
    assert!(moved, "the song should be moving");
    assert!(rig.session.project().track(track).is_ok());

    // A caller that insists on a device is told there is none.
    let setup =
        AudioSetup { silent_fallback: false, ..AudioSetup::on(Arc::new(ScriptedHost::new()), AudioConfig::default()) };
    let refused = Session::with_audio(Arc::new(BuiltinFactory), Arc::new(MediaCache::default()), setup);
    match refused {
        Err(SessionError::Engine(error)) => {
            assert!(error.to_string().contains("no audio output device"), "{error}");
        }
        other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
    }
    // The null backend never needs one.
    assert!(Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 44_100 }).is_ok());
}

#[test]
fn a_device_that_disappears_is_replaced_and_taken_up_again_when_it_is_back() {
    let mut rig = Rig::with(two_devices(), wish("Interface"), impatient());
    let track = rig.probe_track("Keys");
    rig.session.set_selected_track(Some(track));
    rig.session.play();
    rig.run(20);
    assert_eq!(rig.session.audio_info().device.as_deref(), Some("Interface"));
    let project = rig.session.project().clone();
    let revision = rig.session.audio_revision();

    // Unplugged while the song plays and a key is down.
    rig.session.play_live(on(60));
    rig.run(2);
    rig.probes.take();
    rig.host.remove_device("Interface");
    assert_eq!(rig.host.advance(BUFFER), 0, "nothing asks for audio any more");
    rig.session.housekeeping();
    let info = rig.session.audio_info().clone();
    assert_eq!(info.device.as_deref(), Some("Speakers"), "the next best device takes over");
    let warning = info.warning.unwrap();
    assert_eq!(warning, "The audio device was disconnected (Interface (48000 Hz)). Now using Speakers (48000 Hz).");
    assert_eq!(rig.session.audio_revision(), revision + 1);
    assert_eq!(rig.session.audio_config(), &wish("Interface"), "it is still the device that is wanted");
    assert_eq!(rig.session.project(), &project);
    rig.run(2);
    assert!(rig.session.transport().playing, "the song plays on through the other device");
    assert!(rig.probes.events().iter().any(|(_, kind)| *kind == EventKind::AllNotesOff), "and no key hangs");

    // The last device goes too.
    rig.host.remove_device("Speakers");
    rig.session.housekeeping();
    let info = rig.session.audio_info().clone();
    assert_eq!(info.device, None);
    assert_eq!(
        info.warning.as_deref(),
        Some(
            "The audio device was disconnected (Speakers (48000 Hz)). Running without sound until an audio device is there."
        )
    );
    // Everything still works; there is only nothing to hear.
    rig.session.edit(|project| project.set_track_volume(track, -3.0)).unwrap();
    let position = rig.session.transport().position_beats;
    assert!(tend_until(&mut rig, |session| session.transport().position_beats > position + 0.02));
    assert_eq!(rig.session.audio_faults(), 0);

    // Plugged in again: taken up without being asked.
    rig.host.add_device(ScriptedDevice { sample_rates: vec![48_000], ..device("Interface") });
    let back = tend_until(&mut rig, |session| session.audio_info().device.as_deref() == Some("Interface"));
    assert!(back, "{:?}", rig.session.audio_info());
    assert_eq!(rig.session.audio_info().warning, None);
    rig.run(2);
    assert!(rig.session.transport().playing);
    assert_eq!(rig.session.undo_step().is_some(), true);
}

#[test]
fn a_stream_that_reports_an_error_or_goes_quiet_is_opened_again() {
    let host = ScriptedHost::with_speakers();
    host.set_watched(true);
    let mut rig = Rig::with(host, AudioConfig::default(), impatient());
    rig.run(2);
    rig.session.housekeeping();
    let (revision, opened) = (rig.session.audio_revision(), rig.host.requests().len());

    // The driver reports an error it does not recover from.
    rig.host.fail_stream(StreamFailure::Failed);
    rig.session.housekeeping();
    assert_eq!(rig.session.audio_info().device.as_deref(), Some("Speakers"));
    assert_eq!(
        rig.session.audio_info().warning.as_deref(),
        Some("The audio device stopped working (Speakers (48000 Hz)). Now using Speakers (48000 Hz).")
    );
    assert_eq!((rig.session.audio_revision(), rig.host.requests().len()), (revision + 1, opened + 1));

    // The device stops asking for audio and says nothing.
    rig.run(2);
    rig.session.housekeeping();
    assert_eq!(rig.session.audio_revision(), revision + 1, "a stream that is asking is left alone");
    std::thread::sleep(Duration::from_millis(120));
    rig.session.housekeeping();
    assert_eq!(rig.session.audio_revision(), revision + 2);
    let warning = rig.session.audio_info().warning.clone().unwrap();
    assert!(warning.starts_with("The audio device stopped responding"), "{warning}");
    assert!(rig.host.running().is_some(), "and there is a stream again");
}

#[test]
fn a_driver_that_does_not_answer_costs_a_wait_not_the_program() {
    let mut rig = Rig::with(ScriptedHost::with_speakers(), AudioConfig::default(), impatient());
    let track = rig.probe_track("Keys");
    let project = rig.session.project().clone();

    // Every attempt to open the device now takes far longer than anyone
    // should wait.
    rig.host.set_open_delay(Duration::from_millis(700));
    let started = Instant::now();
    rig.session.set_audio_config(AudioConfig { buffer_size: Some(256), ..AudioConfig::default() });
    let waited = started.elapsed();
    assert!(waited < Duration::from_millis(650), "waited {waited:?}");
    let info = rig.session.audio_info().clone();
    assert_eq!(info.device, None);
    let warning = info.warning.unwrap();
    assert!(warning.contains("the device did not answer") && warning.ends_with("running without sound"), "{warning}");
    assert_eq!(rig.session.project(), &project);
    // The listing waits no longer than that either.
    rig.session.edit(|project| project.set_track_pan(track, 0.5)).unwrap();
    rig.session.play();
    assert!(tend_until(&mut rig, |session| session.transport().position_beats > 0.02), "the program runs on");

    // Once the driver answers again, asking again gets the device.
    rig.host.set_open_delay(Duration::ZERO);
    std::thread::sleep(Duration::from_millis(1_500));
    rig.session.set_audio_config(AudioConfig::default());
    assert_eq!(rig.session.audio_info().device.as_deref(), Some("Speakers"));
    assert_eq!(rig.session.audio_info().warning, None);
    rig.run(2);
    assert!(rig.session.transport().playing);
}

/// What a [`Stuck`] device and its test share.
#[derive(Default)]
struct StuckState {
    /// Makes the next block a device processes hang.
    hang: AtomicBool,
    hanging: AtomicBool,
    release: AtomicBool,
    frames: AtomicU64,
}

/// A device that can be made to never come back from `process`, as a
/// plugin with a deadlock would.
struct Stuck(Arc<StuckState>);

impl DeviceProcessor for Stuck {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], left: &mut [f32], _right: &mut [f32]) {
        self.0.frames.fetch_add(left.len() as u64, Ordering::Relaxed);
        if self.0.hang.swap(false, Ordering::Relaxed) {
            self.0.hanging.store(true, Ordering::Relaxed);
            while !self.0.release.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn reset(&mut self) {}
}

struct StuckFactory(Arc<StuckState>);

impl DeviceFactory for StuckFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        vec![DeviceDescriptor {
            uri: "test:stuck".into(),
            name: "Stuck".into(),
            vendor: "Test".into(),
            category: DeviceCategory::Effect,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != "test:stuck" {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        Ok(DeviceInstance { processor: Box::new(Stuck(self.0.clone())), params: Vec::new(), controller: None })
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_device_that_never_comes_back_from_a_block_does_not_hang_a_change_of_stream() {
    // On a real audio thread this time: the null backend's.
    let state = Arc::new(StuckState::default());
    let factory = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(StuckFactory(state.clone()))]);
    let setup =
        AudioSetup { patience: impatient(), ..AudioSetup::on(Arc::new(NullHost::new(48_000)), AudioConfig::default()) };
    let mut session = Session::with_audio(Arc::new(factory), Arc::new(MediaCache::default()), setup).unwrap();
    let track = session.edit(|project| Ok(project.add_track("Keys"))).unwrap();
    session.add_device(ChainRef::Track(track), "test:stuck", None, None).unwrap();
    wait_for("the device should be processing", || state.frames.load(Ordering::Relaxed) > 0);
    let project = session.project().clone();

    state.hang.store(true, Ordering::Relaxed);
    wait_for("the device should be hanging", || state.hanging.load(Ordering::Relaxed));

    // The audio thread is inside the device and is not coming out.
    let started = Instant::now();
    session.set_audio_config(AudioConfig { sample_rate: Some(44_100), ..AudioConfig::default() });
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(2), "waited {waited:?}");
    assert_eq!(session.audio_info().sample_rate, 44_100.0);
    assert_eq!(session.project(), &project);

    // A new audio thread plays the project, with a device made anew.
    let frames = state.frames.load(Ordering::Relaxed);
    wait_for("the new audio thread should be processing", || state.frames.load(Ordering::Relaxed) > frames);
    session.play();
    wait_for("the song should be moving", || session.transport().position_beats > 0.02);
    session.edit(|project| project.set_track_volume(track, -6.0)).unwrap();
    session.stop();

    // Let the old thread go, so the test leaves nothing behind.
    state.release.store(true, Ordering::Relaxed);
}
