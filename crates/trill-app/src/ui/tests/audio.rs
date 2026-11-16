//! The Audio and MIDI panel and the status bar's account of the audio
//! stream, on devices and ports that exist only here: nothing of this
//! machine's hardware is opened, and nothing of its user's settings is read
//! or written.

use super::recording::{Stage, on_stage};
use super::*;
use std::time::{Duration, Instant};
use trill_engine::AudioConfig;
use trill_engine::host::scripted::{ScriptedDevice, ScriptedHost};
use trill_runtime::Storage;
use trill_runtime::midi::scripted::ScriptedMidi;

/// Speakers (the default), an interface that runs at more rates, and a
/// monitor that cannot be taken exclusively.
fn three_outputs() -> ScriptedHost {
    let host = ScriptedHost::with_speakers();
    host.add_device(ScriptedDevice { sample_rates: vec![48_000, 44_100, 96_000], ..ScriptedDevice::new("Interface") });
    host.add_device(ScriptedDevice { exclusive: false, ..ScriptedDevice::new("Monitor") });
    host
}

fn studio() -> (Harness, Stage) {
    studio_keeping(Storage::Nowhere)
}

/// The window on three outputs and two MIDI inputs, one of which another
/// program holds.
fn studio_keeping(storage: Storage) -> (Harness, Stage) {
    let midi = ScriptedMidi::new();
    midi.plug("Keystation");
    midi.plug("Pads");
    midi.refuse("Pads", Some("another program is using this port"));
    let (mut harness, stage) = on_stage(three_outputs(), midi, UiOptions::default(), storage, |session| {
        session.edit(|project| Ok(project.add_track("Keys"))).unwrap();
    });
    assert!(harness.runtime.rescan_midi_inputs());
    harness.idle(1);
    (harness, stage)
}

/// Opens the panel and waits for the list of devices.
fn open_panel(harness: &mut Harness) {
    harness.key(Modifiers::COMMAND, Key::Comma);
    assert!(harness.app.settings().is_open());
    harness.settle_work();
    harness.idle(2);
}

/// Chooses `choice` from the menu of the form row whose label starts with
/// `row`.
fn choose(harness: &mut Harness, row: &str, choice: &str) {
    harness.click_label(row);
    harness.click_last_label(choice);
    harness.idle(2);
}

fn draft(harness: &Harness) -> AudioConfig {
    harness.app.settings().draft().clone()
}

/// What the scripted device is running as: its name, rate, buffer, and
/// whether it was taken exclusively.
fn running(stage: &Stage) -> (String, u32, u32, bool) {
    let format = stage.host.running().expect("a stream is running");
    (format.device, format.sample_rate, format.buffer_frames, format.exclusive)
}

/// One frame of the window with nothing waited for: what the window's own
/// thread does while a worker is busy with the audio device. (The
/// harness's own frames wait for the session, so that a test's next click
/// lands on what the session holds; here that is what must not happen.)
fn frame(harness: &mut Harness, events: Vec<Event>) {
    harness.time += FRAME;
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(harness.time),
        events,
        focused: true,
        ..Default::default()
    };
    harness.app.tick(&harness.ctx);
    let mut output = harness.ctx.run_ui(input, |ui| harness.app.frame(ui));
    output.textures_delta.clear();
}

// ---- reaching the panel ----------------------------------------------------------

#[test]
fn the_panel_opens_from_the_menu_the_status_bar_and_the_keyboard() {
    let (mut harness, _stage) = studio();
    harness.click_label("File");
    harness.click_label("Audio and MIDI");
    assert!(harness.app.settings().is_open());
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(!harness.app.settings().is_open(), "Escape closes it");

    // The status bar's audio items are a button for it.
    let bar = harness.labeled("Audio and MIDI settings");
    assert!(bar.y > SCREEN.y - status_bar::HEIGHT, "in the status bar");
    harness.move_to(bar);
    harness.idle(2);
    assert!(harness.hint().starts_with("Audio and MIDI settings"), "{}", harness.hint());
    harness.click(bar);
    assert!(harness.app.settings().is_open());
    harness.key(Modifiers::COMMAND, Key::Comma);
    assert!(!harness.app.settings().is_open(), "its own key closes it too");

    // While it is up the keyboard is its own, but for playing the song to
    // hear what a setting does.
    open_panel(&mut harness);
    harness.key(Modifiers::COMMAND, Key::T);
    assert_eq!(harness.project().tracks.len(), 1, "Ctrl+T is not acted on behind it");
    harness.key(Modifiers::NONE, Key::Space);
    harness.idle(1);
    harness.session.lock().housekeeping();
    assert!(harness.app.settings().is_open());
}

// ---- the form ------------------------------------------------------------------------

#[test]
fn a_choice_is_seen_with_what_it_costs_and_made_with_apply() {
    let (mut harness, stage) = studio();
    assert_eq!(running(&stage), ("Speakers".to_owned(), 48_000, 480, false));
    open_panel(&mut harness);
    assert!(harness.is_on_screen("Device: System default \u{00B7} Speakers"), "{:?}", harness.labels);
    assert!(harness.is_on_screen("Running now: Speakers"), "what is running is named");
    let line = "48 kHz \u{00B7} 480 frames \u{00B7} 10.0 ms \u{00B7} shared \u{00B7} heard 30.0 ms later, as the device reports";
    assert!(harness.is_on_screen(line), "{:?}", harness.labels);

    // Another device: nothing happens yet, and the cost is said.
    choose(&mut harness, "Device:", "Interface");
    assert_eq!(draft(&harness).output.as_deref(), Some("Interface"));
    assert_eq!(running(&stage).0, "Speakers", "choosing is not applying");
    assert!(harness.is_on_screen("The sound stops for a moment while the device is opened."));

    // Exclusive mode says what it does to other programs before it does it.
    harness.click_label("Exclusive mode");
    harness.idle(2);
    assert!(draft(&harness).exclusive);
    let cost = "Exclusive mode takes Interface for Trill alone: other programs make no sound on it while Trill runs.";
    assert!(harness.is_on_screen(cost), "{:?}", harness.labels);

    // A sample rate the device offers; its cost is the heaviest.
    choose(&mut harness, "Sample rate:", "44.1 kHz");
    assert_eq!(draft(&harness).sample_rate, Some(44_100));
    assert!(
        harness.is_on_screen("Going from 48 kHz to 44.1 kHz stops playback and closes any open plugin windows."),
        "{:?}",
        harness.labels
    );
    choose(&mut harness, "Buffer:", "128 frames");
    assert_eq!(draft(&harness).buffer_size, Some(128));
    assert_eq!(running(&stage), ("Speakers".to_owned(), 48_000, 480, false), "still nothing has changed");

    // Apply: the stream is opened as asked, on a worker.
    harness.click_label("Apply");
    harness.settle_work();
    harness.idle(2);
    assert_eq!(running(&stage), ("Interface".to_owned(), 44_100, 128, true));
    let audio = harness.app.audio();
    assert_eq!(audio.info.device.as_deref(), Some("Interface"));
    assert_eq!((audio.info.sample_rate, audio.info.buffer_frames, audio.info.exclusive), (44_100.0, 128, true));
    assert_eq!(audio.info.warning, None);
    assert!((audio.latency_ms - 128.0 * 1000.0 / 44_100.0).abs() < 0.01, "as the device reports: {}", audio.latency_ms);
    assert_eq!(audio.config, draft(&harness), "what is asked for is what the form shows");
    assert!(harness.is_on_screen("Running now: Interface"));
    let line = "44.1 kHz \u{00B7} 128 frames \u{00B7} 2.9 ms \u{00B7} exclusive \u{00B7} heard 2.9 ms later, as the device reports";
    assert!(harness.is_on_screen(line), "{:?}", harness.labels);
    assert!(!harness.is_on_screen("The sound stops for a moment"), "nothing is left to apply, so nothing costs");
    assert_eq!(harness.runtime.settings().audio.output_device.as_deref(), Some("Interface"));
}

#[test]
fn revert_puts_the_form_back_and_a_new_device_drops_a_rate_it_does_not_run_at() {
    let (mut harness, stage) = studio();
    open_panel(&mut harness);
    choose(&mut harness, "Device:", "Interface");
    choose(&mut harness, "Sample rate:", "96 kHz");
    assert_eq!(draft(&harness).sample_rate, Some(96_000));
    // The speakers do not run at 96 kHz: asked for them, the form asks
    // for their own rate instead of a rate they would refuse.
    choose(&mut harness, "Device:", "Speakers");
    assert_eq!((draft(&harness).output.as_deref(), draft(&harness).sample_rate), (Some("Speakers"), None));

    harness.click_label("Revert");
    harness.idle(2);
    assert_eq!(draft(&harness), AudioConfig::default());
    assert_eq!(running(&stage).0, "Speakers");

    // A device that cannot be taken says so before it is tried.
    choose(&mut harness, "Device:", "Monitor");
    harness.click_label("Exclusive mode");
    harness.idle(2);
    assert!(
        harness.is_on_screen("Monitor cannot be taken for one program alone. It will be opened shared."),
        "{:?}",
        harness.labels
    );
}

// ---- nothing waits on hardware -----------------------------------------------------------

#[test]
fn a_slow_device_does_not_hold_the_window_up() {
    let (mut harness, stage) = studio();
    open_panel(&mut harness);
    choose(&mut harness, "Device:", "Interface");
    // The driver takes its time to answer.
    harness.idle(3);
    let apply = harness.labeled("Apply");
    stage.host.set_open_delay(Duration::from_millis(400));
    let button = |pressed| Event::PointerButton {
        pos: apply,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    frame(&mut harness, vec![Event::PointerMoved(apply)]);
    frame(&mut harness, vec![button(true)]);
    frame(&mut harness, vec![button(false)]);
    assert!(harness.app.settings().is_applying());

    // The session is busy opening the device. The window goes on painting
    // frames, and none of them waits for it.
    let began = Instant::now();
    let mut frames = 0;
    while harness.app.settings().is_applying() {
        let before = Instant::now();
        frame(&mut harness, Vec::new());
        frames += 1;
        assert!(before.elapsed() < Duration::from_millis(150), "a frame waited for the audio device");
        assert!(began.elapsed() < Duration::from_secs(20), "the device was never opened");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(frames >= 10, "the window painted while the device was being opened: {frames} frames");
    assert!(began.elapsed() >= Duration::from_millis(300), "and the device did take its time");

    // While it was, the form could not be applied a second time, and the
    // keys that need the session were not acted on.
    stage.host.set_open_delay(Duration::ZERO);
    harness.settle_work();
    harness.idle(2);
    assert_eq!(running(&stage).0, "Interface");
    assert_eq!(harness.app.audio().info.device.as_deref(), Some("Interface"));
}

#[test]
fn while_a_device_is_being_opened_nothing_else_asks_for_the_session() {
    let (mut harness, stage) = studio();
    open_panel(&mut harness);
    choose(&mut harness, "Device:", "Interface");
    // The workers are held: the panel is applying for as long as the test
    // says, without anything in fact holding the session.
    harness.app.sounds().hold(true);
    harness.click_label("Apply");
    assert!(harness.app.settings().is_applying());
    harness.idle(3);
    assert!(!harness.is_on_screen("Device: Interface") || harness.is_on_screen("Device: Interface"));

    // Apply again, and Revert: the buttons wait.
    let apply = harness.labeled("Apply");
    harness.click(apply);
    harness.click_label("Revert");
    assert_eq!(draft(&harness).output.as_deref(), Some("Interface"), "the form is as it was sent");
    // Space, which would ask the session to play, is not acted on.
    harness.key(Modifiers::NONE, Key::Space);
    assert!(harness.app.settings().is_applying());

    harness.app.sounds().hold(false);
    harness.settle_work();
    harness.idle(2);
    assert!(!harness.app.settings().is_applying());
    assert_eq!(running(&stage).0, "Interface");
    assert_eq!(
        stage.host.requests().iter().filter(|request| request.device.as_deref() == Some("Interface")).count(),
        1
    );
}

// ---- when things go wrong, in sentences ---------------------------------------------------

#[test]
fn a_device_that_vanishes_is_said_calmly_and_gone_back_to() {
    let (mut harness, stage) = studio();
    open_panel(&mut harness);
    choose(&mut harness, "Device:", "Interface");
    harness.click_label("Apply");
    harness.settle_work();
    harness.idle(2);
    assert_eq!(running(&stage).0, "Interface");

    // It is unplugged while the panel is open. The engine falls back to
    // what there is; the panel and the status bar follow, in its words.
    stage.host.remove_device("Interface");
    harness.session.lock().housekeeping();
    harness.idle(2);
    let audio = harness.app.audio().clone();
    assert_eq!(audio.info.device.as_deref(), Some("Speakers"));
    let warning = audio.info.warning.clone().expect("the stream is not what was asked for");
    assert!(harness.is_on_screen("Running now: Speakers"), "{:?}", harness.labels);
    let said =
        harness.labels.iter().map(|(text, _)| text.clone()).find(|text| text.contains("Trill goes back to Interface"));
    let said = said.unwrap_or_else(|| panic!("the panel does not say what happened: {:?}", harness.labels));
    assert!(said.to_lowercase().starts_with(&warning.to_lowercase()[..20]), "in the engine's words: {said}");
    assert_eq!(audio.config.output.as_deref(), Some("Interface"), "and the interface is still what is wanted");

    // The list, looked at again, no longer has it; the form says which
    // device it is waiting for.
    harness.click_label("Refresh");
    harness.settle_work();
    harness.idle(2);
    assert!(harness.is_on_screen("Device: Interface \u{00B7} not connected"), "{:?}", harness.labels);

    // The status bar points at the same sentence.
    harness.key(Modifiers::NONE, Key::Escape);
    harness.move_to(harness.labeled("Audio and MIDI settings"));
    harness.idle(2);
    assert!(harness.hint().starts_with(&warning), "{}", harness.hint());

    // Plugged in again, it is taken up again without being asked.
    stage.host.add_device(ScriptedDevice::new("Interface"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while stage.host.running().is_none_or(|format| format.device != "Interface") {
        assert!(Instant::now() < deadline, "the interface was not gone back to");
        harness.session.lock().housekeeping();
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.idle(2);
    assert_eq!(harness.app.audio().info.device.as_deref(), Some("Interface"));
    assert_eq!(harness.app.audio().info.warning, None);
}

#[test]
fn a_machine_with_no_output_says_so_and_carries_on() {
    let (mut harness, stage) =
        on_stage(ScriptedHost::new(), ScriptedMidi::new(), UiOptions::default(), Storage::Nowhere, |_| {});
    assert!(harness.app.audio().silent());
    open_panel(&mut harness);
    assert!(harness.is_on_screen("No audio output is connected."), "{:?}", harness.labels);
    assert!(harness.is_on_screen("Running now: No audio device"));
    assert!(harness.is_on_screen("No MIDI input is plugged in."), "{:?}", harness.labels);
    assert!(harness.is_on_screen("Device: System default"));

    // One is plugged in: Refresh finds it, and it can be chosen.
    stage.host.add_device(ScriptedDevice::new("Headphones"));
    harness.click_label("Refresh");
    harness.settle_work();
    harness.idle(2);
    assert!(!harness.is_on_screen("No audio output is connected."));
    choose(&mut harness, "Device:", "Headphones");
    harness.click_label("Apply");
    harness.settle_work();
    harness.idle(2);
    assert_eq!(running(&stage).0, "Headphones");
    assert!(!harness.app.audio().silent());
    assert!(harness.is_on_screen("Running now: Headphones"));
}

#[test]
fn a_choice_that_cannot_be_written_down_is_in_use_and_says_so() {
    // The settings are to be kept under a path that is a file: writing
    // them there fails, however the machine is set up.
    let folder = tempfile::tempdir().unwrap();
    let blocker = folder.path().join("not-a-folder");
    std::fs::write(&blocker, b"in the way").unwrap();
    let storage = Storage::At { settings: blocker.join("settings.json"), library_index: None };
    let (mut harness, stage) = studio_keeping(storage);
    open_panel(&mut harness);
    choose(&mut harness, "Device:", "Interface");
    harness.click_label("Apply");
    harness.settle_work();
    harness.idle(2);
    assert_eq!(running(&stage).0, "Interface", "the stream changed all the same");
    let problem = "The choice is in use, but could not be written down for next time";
    assert!(harness.is_on_screen(problem), "{:?}", harness.labels);
    assert_eq!(harness.app.state().toasts.len(), 0, "said in the panel, not thrown at the user");
}

// ---- MIDI inputs ---------------------------------------------------------------------------

#[test]
fn midi_inputs_are_listed_switched_and_rescanned() {
    let (mut harness, stage) = studio();
    open_panel(&mut harness);
    assert!(harness.is_on_screen("Keystation \u{00B7} Listening"), "{:?}", harness.labels);
    let held = "Pads \u{00B7} Not listening: another program is using this port.";
    assert!(harness.is_on_screen(held), "what is wrong with a port is said: {:?}", harness.labels);
    assert!(harness.is_on("Listen to Keystation") && harness.is_on("Listen to Pads"));

    // The lamp lights when the input sends something, and goes out again.
    let now = |harness: &Harness| harness.time;
    assert!(!harness.app.settings().lamp_is_lit("Keystation", now(&harness)));
    stage.midi.send("Keystation", &[0x90, 60, 100]);
    harness.idle(1);
    assert!(harness.app.settings().lamp_is_lit("Keystation", now(&harness)));
    assert!(!harness.app.settings().lamp_is_lit("Pads", now(&harness)));
    harness.idle(20);
    assert!(!harness.app.settings().lamp_is_lit("Keystation", now(&harness)));
    stage.midi.send("Keystation", &[0x80, 60, 0]);

    // Switched off: the port is closed, and stays listed.
    harness.click_label("Listen to Keystation");
    harness.settle_work();
    harness.idle(2);
    assert!(stage.midi.open().is_empty());
    assert!(harness.is_on_screen("Keystation \u{00B7} Off"), "{:?}", harness.labels);
    assert!(!harness.is_on("Listen to Keystation"));
    assert_eq!(harness.runtime.settings().midi.disabled_inputs, ["Keystation"], "and that is remembered");
    harness.click_label("Listen to Keystation");
    harness.settle_work();
    harness.idle(2);
    assert_eq!(stage.midi.open(), ["Keystation"]);

    // The other program lets go of the pads, and a new keyboard is plugged
    // in: Rescan finds both.
    stage.midi.refuse("Pads", None);
    stage.midi.plug("Launchkey");
    harness.click_label("Rescan");
    harness.settle_work();
    harness.idle(2);
    assert!(harness.is_on_screen("Pads \u{00B7} Listening") && harness.is_on_screen("Launchkey \u{00B7} Listening"));
    assert_eq!(stage.midi.open(), ["Keystation", "Launchkey", "Pads"]);

    // Pulled out again while switched off, an input stays listed so that
    // it can be switched back on.
    harness.click_label("Listen to Launchkey");
    harness.settle_work();
    stage.midi.unplug("Launchkey");
    harness.click_label("Rescan");
    harness.settle_work();
    harness.idle(2);
    assert!(harness.is_on_screen("Launchkey \u{00B7} Off \u{00B7} not plugged in"), "{:?}", harness.labels);

    // A system that does not answer a look at the ports is said so, and
    // the list stays as it was.
    harness.app.settings().report_rescan_timed_out();
    harness.idle(2);
    assert!(harness.is_on_screen("The system did not answer within a few seconds."), "{:?}", harness.labels);
    assert!(harness.is_on_screen("Pads \u{00B7} Listening"));
    harness.click_label("Rescan");
    harness.settle_work();
    harness.idle(2);
    assert!(!harness.is_on_screen("The system did not answer within a few seconds."));
}

// ---- the status bar ---------------------------------------------------------------------------

#[test]
fn the_window_follows_the_stream_when_it_is_changed_from_elsewhere() {
    let (mut harness, stage) = studio();
    let before = harness.app.audio().clone();
    assert_eq!(before.info.device.as_deref(), Some("Speakers"));
    assert_eq!((before.info.buffer_frames, before.latency_ms), (480, 30.0));

    // An assistant chooses another device over MCP: no click, no panel.
    let wanted =
        AudioConfig { output: Some("Interface".into()), buffer_size: Some(64), exclusive: true, ..before.config };
    harness.runtime.set_audio(wanted.clone()).1.unwrap();
    harness.idle(1);
    let after = harness.app.audio().clone();
    assert_eq!(after.info.device.as_deref(), Some("Interface"));
    assert_eq!((after.info.buffer_frames, after.info.exclusive), (64, true));
    assert!((after.latency_ms - 64.0 / 48.0).abs() < 0.01);
    assert_eq!(after.config, wanted);
    assert!(after.revision > before.revision);
    assert_eq!(running(&stage).0, "Interface");

    // A panel opened now starts from what is asked for now.
    open_panel(&mut harness);
    assert_eq!(draft(&harness), wanted);
    assert!(harness.is_on_screen("Device: Interface"));
}
