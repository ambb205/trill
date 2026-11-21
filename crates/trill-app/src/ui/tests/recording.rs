//! Arming, recording and where keyboards play, driven through the window on
//! audio devices and MIDI ports that exist only here.
//!
//! The audio device is the engine's scripted one: it has no thread of its
//! own, and asks for audio only when a test advances it. So where a note is
//! played and where it lands in a take are exact, on any machine and under
//! any load. The instruments are probes that write down what they are told
//! to play, which is how these tests know what a key really reached.

use super::*;
use crate::ui::KeyboardOptions;
use crate::ui::keyboard::{Sent, Silence};
use crate::ui::state::LaneShown;
use trill_core::device::{
    DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor, Event as Played,
    EventKind, ProcessInfo,
};
use trill_core::project::Device;
use trill_core::{ControllerKind, TrackKind};
use trill_engine::RecordState;
use trill_engine::host::scripted::ScriptedHost;
use trill_runtime::midi::scripted::ScriptedMidi;
use trill_runtime::{Parts, Storage};

pub(super) const PROBE_URI: &str = "test:probe";
/// Frames in a beat at the tempo and rate these projects run at.
const BEAT: usize = 24_000;
/// One buffer of the scripted device.
const BUFFER: usize = 480;

/// What the probes were told to play, in order, and by which of them.
#[derive(Default)]
pub(super) struct Heard(Mutex<Vec<(DeviceId, EventKind)>>);

impl Heard {
    /// The notes heard since the last call: without the housekeeping every
    /// instrument is sent when the engine starts or stops, and without the
    /// controllers (the computer keyboard gives a track its expression
    /// before the first note it plays there).
    pub(super) fn take(&self) -> Vec<(DeviceId, EventKind)> {
        let notes = |kind: &EventKind| matches!(kind, EventKind::NoteOn { .. } | EventKind::NoteOff { .. });
        self.everything().into_iter().filter(|(_, kind)| notes(kind)).collect()
    }

    /// Everything heard since the last call but the housekeeping, the
    /// controllers among the notes in the order they came.
    pub(super) fn everything(&self) -> Vec<(DeviceId, EventKind)> {
        let heard = std::mem::take(&mut *self.0.lock());
        heard.into_iter().filter(|(_, kind)| !matches!(kind, EventKind::AllNotesOff)).collect()
    }
}

struct Probe {
    device: DeviceId,
    heard: Arc<Heard>,
}

impl DeviceProcessor for Probe {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, _info: &ProcessInfo, events: &[Played], _left: &mut [f32], _right: &mut [f32]) {
        if !events.is_empty() {
            self.heard.0.lock().extend(events.iter().map(|event| (self.device, event.kind)));
        }
    }

    fn reset(&mut self) {}
}

struct ProbeFactory(Arc<Heard>);

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
        let probe = Probe { device: device.id, heard: self.0.clone() };
        Ok(DeviceInstance { processor: Box::new(probe), params: Vec::new(), controller: None })
    }
}

/// The made-up hardware a window was opened on.
pub(super) struct Stage {
    pub host: ScriptedHost,
    pub midi: ScriptedMidi,
    pub heard: Arc<Heard>,
}

impl Stage {
    /// The device asks for `frames` more frames, and the window is given a
    /// frame to notice what came of it.
    pub(super) fn run(&self, harness: &mut Harness, frames: usize) {
        self.host.advance(frames);
        // What the housekeeping thread does a few times a second: here at
        // once, so that a test need not wait for it.
        harness.session.lock().housekeeping();
        harness.idle(1);
    }
}

/// Opens the window on `host` and `midi`, keeping settings as `storage`
/// says, on a project prepared by `setup`.
pub(super) fn on_stage(
    host: ScriptedHost,
    midi: ScriptedMidi,
    options: UiOptions,
    storage: Storage,
    setup: impl FnOnce(&mut Session),
) -> (Harness, Stage) {
    let heard = Arc::new(Heard::default());
    let parts = Parts {
        devices: vec![Box::new(ProbeFactory(heard.clone()))],
        audio: Some(Arc::new(host.clone())),
        midi: Some(Box::new(midi.clone())),
        ..Parts::default()
    };
    let runtime = Runtime::start_custom(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }, parts).unwrap();
    let mut harness = Harness::on(Arc::new(runtime), options, setup);
    // What the computer keyboard plays is stamped with the device's clock,
    // which moves only as the test advances it.
    let clock = host.clone();
    harness.app.keyboard_mut().set_clock(Arc::new(move || clock.now_ns()));
    // The engine's first buffers tell every instrument to let go; the
    // tests begin after that.
    host.advance(2 * BUFFER);
    heard.take();
    harness.idle(1);
    (harness, Stage { host, midi, heard })
}

/// Three tracks: `Keys` and `Bass` with a probe each, and `Audio` with
/// nothing to play.
fn trio(session: &mut Session) -> [TrackId; 3] {
    let tracks = session
        .edit(|project| Ok([project.add_track("Keys"), project.add_track("Bass"), project.add_track("Audio")]))
        .unwrap();
    for track in &tracks[..2] {
        session.add_device(ChainRef::Track(*track), PROBE_URI, None, None).unwrap();
    }
    tracks
}

/// The window on the trio, with one keyboard plugged in, and the computer
/// keyboard on or off.
fn with_trio(keyboard: bool) -> (Harness, Stage, [TrackId; 3]) {
    let midi = ScriptedMidi::new();
    midi.plug("Keystation");
    let options = UiOptions { keyboard: KeyboardOptions { on: keyboard, hold: Vec::new() }, ..Default::default() };
    let mut tracks = None;
    let (mut harness, stage) = on_stage(ScriptedHost::with_speakers(), midi, options, Storage::Nowhere, |session| {
        tracks = Some(trio(session))
    });
    assert!(harness.runtime.rescan_midi_inputs());
    harness.idle(1);
    (harness, stage, tracks.unwrap())
}

/// The probe on `track`.
fn probe(harness: &Harness, track: TrackId) -> DeviceId {
    harness.project().track(track).unwrap().devices[0].id
}

fn on(pitch: u8) -> EventKind {
    EventKind::NoteOn { pitch, velocity: 100.0 / 127.0 }
}

fn off(pitch: u8) -> EventKind {
    EventKind::NoteOff { pitch }
}

fn key(key: Key, pressed: bool) -> Event {
    Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }
}

fn armed(harness: &Harness) -> Vec<TrackId> {
    harness.session.lock().armed().to_vec()
}

fn recording(harness: &Harness) -> RecordState {
    harness.session.lock().record_status().state
}

/// What the toasts on show say.
fn said(harness: &Harness) -> Vec<String> {
    harness.app.state().toasts.texts().into_iter().map(str::to_owned).collect()
}

// ---- the selection reaches the session -----------------------------------------

#[test]
fn the_session_is_kept_told_of_the_selection() {
    let (mut harness, stage, [keys, bass, _]) = with_trio(false);
    let told = |harness: &Harness| harness.session.lock().selected_track();
    assert_eq!(told(&harness), Some(keys), "from the start: the first track is the selected one");

    // A MIDI keyboard plays what is selected in the window.
    stage.midi.send("Keystation", &[0x90, 60, 100]);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(stage.heard.take(), [(probe(&harness, keys), on(60))]);
    stage.midi.send("Keystation", &[0x80, 60, 0]);
    stage.run(&mut harness, 2 * BUFFER);
    stage.heard.take();

    // A click on another track, and the keyboard plays that one: told in
    // the frame of the click, not when the window next gets round to it.
    harness.move_to(harness.header(1, 120.0, 14.0));
    harness.button(harness.header(1, 120.0, 14.0), PointerButton::Primary, true);
    harness.button(harness.header(1, 120.0, 14.0), PointerButton::Primary, false);
    assert_eq!(told(&harness), Some(bass));
    stage.midi.send("Keystation", &[0x90, 62, 100]);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(stage.heard.take(), [(probe(&harness, bass), on(62))]);

    // The master selected for its devices: the keys stay with the track.
    harness.click(pos2(harness.header(0, 60.0, 0.0).x, harness.master_top() + 12.0));
    assert!(harness.app.state().master_selected);
    assert_eq!(told(&harness), Some(bass));

    // The selection moves without the window: its track is deleted from
    // elsewhere. The window selects another, and the session is told.
    harness.session.lock().edit(|project| project.remove_track(bass).map(drop)).unwrap();
    harness.idle(2);
    assert_eq!(harness.app.state().selected_track, Some(keys));
    assert_eq!(told(&harness), Some(keys));

    // The session forgets (the track went and came back within a moment,
    // by an edit and its undo elsewhere): the window says it again.
    harness.session.lock().set_selected_track(None);
    harness.idle(1);
    assert_eq!(told(&harness), Some(keys));

    // Nothing left to select.
    harness.session.lock().new_project();
    harness.idle(2);
    assert_eq!((harness.app.state().selected_track, told(&harness)), (None, None));
}

// ---- arming -----------------------------------------------------------------------

#[test]
fn arming_a_track_disarms_the_rest_unless_ctrl_is_held() {
    let (mut harness, _stage, [keys, bass, audio]) = with_trio(false);
    // (Counting the undo steps undoes and redoes them, so it comes first.)
    let depth = harness.undo_depth();
    let revision = harness.session.lock().revision();
    assert!(harness.is_on_screen("Arm Keys") && harness.is_on_screen("Arm Bass"));
    assert!(!harness.is_on_screen("Arm Audio"), "a track with nothing to play has nothing to arm");
    assert!(!harness.is_on("Arm Keys"));

    harness.click_label("Arm Keys");
    assert_eq!(armed(&harness), [keys]);
    harness.idle(1);
    assert!(harness.is_on("Arm Keys") && !harness.is_on("Arm Bass"), "armed is seen at a glance");

    harness.click_label("Arm Bass");
    assert_eq!(armed(&harness), [bass], "arming one disarms the other");
    let again = harness.labeled("Arm Keys");
    harness.click_holding(Modifiers::COMMAND, again);
    assert_eq!(armed(&harness), [bass, keys], "unless Ctrl is held");
    harness.click_label("Arm Bass");
    assert_eq!(armed(&harness), [keys], "a click on an armed track disarms that one");

    // The mixer's strips have the same control, in the same state.
    harness.click_label("Mixer");
    harness.idle(2);
    assert!(harness.is_on("Arm Keys in the mixer") && !harness.is_on("Arm Bass in the mixer"));
    assert!(!harness.is_on_screen("Arm Audio in the mixer"));
    harness.click_label("Arm Bass in the mixer");
    assert_eq!(armed(&harness), [bass]);

    // Armed from elsewhere (an assistant, over MCP): the window shows it.
    harness.session.lock().set_armed(keys, true).unwrap();
    harness.idle(2);
    assert!(harness.is_on("Arm Keys") && harness.is_on("Arm Keys in the mixer"));

    // Arming is not part of the project: nothing to save, nothing to undo.
    assert_eq!(harness.session.lock().revision(), revision);
    assert!(!harness.app.snapshot().modified || harness.app.snapshot().revision == revision);
    assert_eq!(harness.undo_depth(), depth);
    // (Which took the tracks away and brought them back, and with them
    // went what was armed: armed again for what follows.)
    harness.idle(2);
    harness.session.lock().set_armed(keys, true).unwrap();
    harness.session.lock().set_armed(bass, true).unwrap();
    harness.idle(2);

    // A track that loses its instrument while armed keeps its control
    // until it has been disarmed with it.
    let synth = probe(&harness, bass);
    harness.session.lock().edit(|project| project.remove_device(synth).map(drop)).unwrap();
    harness.idle(2);
    assert!(harness.is_on("Arm Bass"));
    harness.click_label("Arm Bass");
    harness.idle(2);
    assert!(!harness.is_on_screen("Arm Bass"));
    assert_eq!(armed(&harness), [keys]);
    let _ = audio;
}

// ---- recording ---------------------------------------------------------------------

#[test]
fn a_take_is_counted_in_recorded_and_selected_as_one_undo_step() {
    let (mut harness, stage, [keys, ..]) = with_trio(true);
    let depth = harness.undo_depth();
    harness.idle(2);
    harness.click_label("Arm Keys");
    let before = harness.project();
    assert_eq!(armed(&harness), [keys]);
    assert!(harness.is_on_screen("Count-in: 1 bar"), "the count-in stands beside the record button");

    // F9: the metronome counts a bar in before the song starts.
    harness.key(Modifiers::NONE, Key::F9);
    assert_eq!(recording(&harness), RecordState::CountIn);
    stage.run(&mut harness, BUFFER);
    assert!(harness.is_on_screen("Take: Count-in"), "{:?}", harness.labels);
    assert!(!harness.session.lock().transport().playing, "the song waits for the count");
    assert!(harness.hint().is_empty() || !harness.hint().contains("Recording"));

    // A bar later the song rolls and what is played is written down.
    stage.run(&mut harness, 4 * BEAT);
    assert_eq!(recording(&harness), RecordState::Recording);
    assert!(harness.is_on_screen("Take: Rec"));
    assert!(harness.session.lock().transport().playing);

    // Two notes on the computer keyboard: a C for half a beat, and after
    // half a beat's rest an E still held when the take is stopped.
    stage.run(&mut harness, BEAT);
    harness.step(Modifiers::NONE, vec![key(Key::A, true)]);
    stage.run(&mut harness, BEAT / 2);
    harness.step(Modifiers::NONE, vec![key(Key::A, false)]);
    stage.run(&mut harness, BEAT / 2);
    harness.step(Modifiers::NONE, vec![key(Key::D, true)]);
    stage.run(&mut harness, BEAT);
    assert_eq!(harness.session.lock().record_status().notes, 1, "the note that has ended so far");
    assert_eq!(harness.project().track(keys).unwrap().clips.len(), 1, "the clip is there while it is recorded");

    // Space ends the take where the song is.
    harness.key(Modifiers::NONE, Key::Space);
    assert_eq!(recording(&harness), RecordState::Idle);
    stage.run(&mut harness, 2 * BUFFER);
    assert!(!harness.session.lock().transport().playing, "and stops the song with it");
    assert!(!harness.is_on_screen("Take: Rec") && harness.is_on_screen("Count-in: 1 bar"));

    // What was played is in a new clip, where it was heard.
    let project = harness.project();
    let clip = &project.track(keys).unwrap().clips[0];
    let notes = clip.notes();
    assert_eq!(notes.iter().map(|note| (note.pitch, note.velocity)).collect::<Vec<_>>(), [(60, 100), (64, 100)]);
    let at = |index: usize| clip.start + notes[index].start;
    assert!((at(0) - 1.0).abs() < 0.1, "the C was played a beat in: {}", at(0));
    assert!((notes[0].length - 0.5).abs() < 0.1, "and held for half a beat: {}", notes[0].length);
    assert!((at(1) - 2.0).abs() < 0.1, "the E a beat later: {}", at(1));
    assert!((notes[1].length - 1.0).abs() < 0.1, "and ended with the take: {}", notes[1].length);

    // The take is what is selected now.
    harness.idle(2);
    assert_eq!(harness.selected_clips(), [clip.id]);
    assert_eq!(harness.app.state().selected_track, Some(keys));
    assert!(said(&harness).is_empty(), "nothing needs saying about a take that went well: {:?}", said(&harness));
    // Nothing is left sounding: the E's key is still down, and its note
    // ended with the take all the same when the key comes up.
    harness.step(Modifiers::NONE, vec![key(Key::D, false)]);
    assert_eq!(harness.app.keyboard().sounding(), []);

    // And it is one undo step, whatever it took to write.
    assert_eq!(harness.undo_depth(), depth + 1);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(harness.project(), before);
}

#[test]
fn space_and_stop_end_a_take_and_call_off_a_count_in() {
    let (mut harness, stage, [keys, ..]) = with_trio(false);
    harness.click_label("Arm Keys");

    // During the count-in the song has not started; Space must not start
    // it, it calls the take off.
    harness.click_label("Record");
    stage.run(&mut harness, BUFFER);
    assert_eq!(recording(&harness), RecordState::CountIn);
    harness.key(Modifiers::NONE, Key::Space);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(recording(&harness), RecordState::Idle);
    assert!(!harness.session.lock().transport().playing);
    assert_eq!(said(&harness), ["Nothing was played during the take, so nothing was recorded"]);
    assert!(harness.project().track(keys).unwrap().clips.is_empty());

    // The Stop button ends a take that is rolling.
    harness.key(Modifiers::NONE, Key::F9);
    stage.run(&mut harness, 4 * BEAT + BUFFER);
    assert_eq!(recording(&harness), RecordState::Recording);
    harness.click_label("Stop");
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(recording(&harness), RecordState::Idle);
    assert!(!harness.session.lock().transport().playing);

    // So do the record button and its key, pressed again.
    harness.click_label("Record");
    stage.run(&mut harness, BUFFER);
    assert_eq!(recording(&harness), RecordState::CountIn);
    harness.click_label("Record");
    assert_eq!(recording(&harness), RecordState::Idle);
    harness.key(Modifiers::NONE, Key::F9);
    stage.run(&mut harness, BUFFER);
    harness.key(Modifiers::NONE, Key::F9);
    assert_eq!(recording(&harness), RecordState::Idle);

    // A take ended from elsewhere (an assistant stops the song): the
    // window shows that it is over.
    harness.key(Modifiers::NONE, Key::F9);
    stage.run(&mut harness, BUFFER);
    assert!(harness.is_on_screen("Take: Count-in"));
    harness.session.lock().stop();
    stage.run(&mut harness, BUFFER);
    assert!(!harness.is_on_screen("Take: Count-in") && harness.is_on_screen("Count-in: 1 bar"));
}

#[test]
fn the_count_in_is_chosen_beside_the_record_button() {
    let (mut harness, stage, _) = with_trio(false);
    harness.click_label("Arm Keys");
    harness.click_label("Count-in: 1 bar");
    harness.click_label("2 bars");
    assert_eq!(harness.session.lock().count_in(), 2);
    harness.idle(2);
    assert!(harness.is_on_screen("Count-in: 2 bars"));
    harness.click_label("Count-in: 2 bars");
    harness.click_label("No count-in");
    assert_eq!(harness.session.lock().count_in(), 0);

    // Without one, the song rolls at once.
    harness.key(Modifiers::NONE, Key::F9);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(recording(&harness), RecordState::Recording);
    assert!(harness.is_on_screen("Take: Rec"));
    harness.key(Modifiers::NONE, Key::Space);

    // Set from elsewhere, the window shows it.
    harness.session.lock().set_count_in(4);
    harness.idle(2);
    assert!(harness.is_on_screen("Count-in: 4 bars"));
}

#[test]
fn a_take_says_which_pass_of_the_loop_it_is_on() {
    let (mut harness, stage, _) = with_trio(false);
    harness.session.lock().edit(|project| project.set_loop(true, 0.0, 4.0)).unwrap();
    harness.session.lock().set_count_in(0);
    harness.click_label("Arm Keys");
    harness.key(Modifiers::NONE, Key::F9);
    stage.run(&mut harness, 2 * BUFFER);
    assert!(harness.is_on_screen("Take: Pass 1"), "{:?}", harness.labels);
    stage.run(&mut harness, 4 * BEAT);
    assert!(harness.is_on_screen("Take: Pass 2"));
    // The whole of it, in words, for whoever points at it.
    harness.move_to(harness.labeled("Take: Pass 2"));
    harness.idle(2);
    let hint = harness.hint();
    assert!(hint.starts_with("Recording on Keys \u{00B7} pass 2 of the loop"), "{hint}");
    assert!(hint.ends_with("Space or Stop ends the take"), "{hint}");
    harness.key(Modifiers::NONE, Key::Space);
    assert_eq!(recording(&harness), RecordState::Idle);
}

#[test]
fn recording_with_nothing_armed_arms_the_selected_track_or_says_why_not() {
    let (mut harness, stage, [keys, _, audio]) = with_trio(false);

    // The selected track has nothing to play.
    harness.click(harness.header(2, 120.0, 14.0));
    assert_eq!(harness.app.state().selected_track, Some(audio));
    harness.key(Modifiers::NONE, Key::F9);
    assert_eq!(recording(&harness), RecordState::Idle);
    assert!(armed(&harness).is_empty());
    let reason = said(&harness).join(" ");
    assert!(reason.contains("Audio has no instrument") && reason.contains("Select or arm a track"), "{reason}");

    // A group holds no clips.
    let group = harness.session.lock().edit(|project| Ok(project.add_track_of_kind("Bus", TrackKind::Group))).unwrap();
    harness.app.state_mut().select_track(group);
    harness.idle(2);
    harness.key(Modifiers::NONE, Key::F9);
    assert_eq!(recording(&harness), RecordState::Idle);
    assert!(said(&harness).iter().any(|text| text.contains("Bus is a group")), "{:?}", said(&harness));

    // A track that can be played is armed for it, and stays armed: the
    // window shows what the take is being recorded on.
    harness.click(harness.header(0, 120.0, 14.0));
    harness.key(Modifiers::NONE, Key::F9);
    assert_eq!(armed(&harness), [keys]);
    assert_eq!(recording(&harness), RecordState::CountIn);
    stage.run(&mut harness, BUFFER);
    assert!(harness.is_on("Arm Keys"));
    harness.key(Modifiers::NONE, Key::Space);

    // No track at all.
    let (mut harness, _stage) =
        on_stage(ScriptedHost::with_speakers(), ScriptedMidi::new(), UiOptions::default(), Storage::Nowhere, |_| {});
    harness.key(Modifiers::NONE, Key::F9);
    assert_eq!(recording(&harness), RecordState::Idle);
    assert!(said(&harness)[0].starts_with("There is no track to record on. Add one (Ctrl+T)"), "{:?}", said(&harness));
}

// ---- the computer keyboard is a keyboard like any other ------------------------------

#[test]
fn the_computer_keyboard_plays_where_a_midi_keyboard_plays() {
    let (mut harness, stage, [keys, bass, _]) = with_trio(true);
    let log = |harness: &mut Harness| std::mem::take(&mut harness.app.keyboard_mut().sent);

    // Nothing armed: the selected track, for both.
    harness.key(Modifiers::NONE, Key::A);
    stage.run(&mut harness, 2 * BUFFER);
    stage.midi.send("Keystation", &[0x90, 72, 100]);
    stage.midi.send("Keystation", &[0x80, 72, 0]);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(log(&mut harness), [Sent::On(keys, 60, 100), Sent::Off(keys, 60)]);
    let on_keys = probe(&harness, keys);
    let heard = stage.heard.take();
    assert!(heard.iter().all(|(device, _)| *device == on_keys), "{heard:?}");
    assert_eq!(heard.iter().map(|(_, kind)| *kind).collect::<Vec<_>>(), [on(60), off(60), on(72), off(72)]);

    // A track armed: both play that one, whatever is selected.
    harness.click_label("Arm Bass");
    assert_eq!(harness.app.state().selected_track, Some(keys), "arming does not move the selection");
    harness.key(Modifiers::NONE, Key::A);
    stage.run(&mut harness, 2 * BUFFER);
    stage.midi.send("Keystation", &[0x90, 72, 100]);
    stage.midi.send("Keystation", &[0x80, 72, 0]);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(log(&mut harness), [Sent::On(bass, 60, 100), Sent::Off(bass, 60)]);
    let on_bass = probe(&harness, bass);
    let heard = stage.heard.take();
    assert!(heard.iter().all(|(device, _)| *device == on_bass), "{heard:?}");
    assert_eq!(heard.len(), 4);
    assert_eq!(harness.app.keyboard().playing().name, "Bass");
    assert!(harness.app.keyboard().playing().armed);

    // Two armed: both, from one key.
    let other = harness.labeled("Arm Keys");
    harness.click_holding(Modifiers::COMMAND, other);
    harness.key(Modifiers::NONE, Key::D);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(
        log(&mut harness),
        [Sent::On(bass, 64, 100), Sent::On(keys, 64, 100), Sent::Off(bass, 64), Sent::Off(keys, 64)]
    );
    let mut heard = stage.heard.take();
    heard.sort_by_key(|(device, kind)| (matches!(kind, EventKind::NoteOff { .. }), device.0));
    let (first, second) = (on_keys.min(on_bass), on_keys.max(on_bass));
    assert_eq!(heard, [(first, on(64)), (second, on(64)), (first, off(64)), (second, off(64))]);
    assert_eq!(harness.app.keyboard().playing().name, "Bass, Keys");

    // Disarmed again, the selection is what counts.
    harness.click_label("Arm Bass");
    harness.click_label("Arm Keys");
    assert!(armed(&harness).is_empty());
    harness.click(harness.header(1, 120.0, 14.0));
    harness.key(Modifiers::NONE, Key::A);
    assert_eq!(log(&mut harness), [Sent::On(bass, 60, 100), Sent::Off(bass, 60)]);
}

#[test]
fn held_notes_end_when_the_keys_are_given_other_tracks_to_play() {
    let (mut harness, stage, [keys, bass, _]) = with_trio(true);
    let log = |harness: &mut Harness| std::mem::take(&mut harness.app.keyboard_mut().sent);
    let on_keys = probe(&harness, keys);

    // A key held on the selected track while another track is armed: its
    // note ends on the track it began on, and its key-up later ends nothing.
    harness.step(Modifiers::NONE, vec![key(Key::A, true)]);
    harness.click_label("Arm Bass");
    assert_eq!(log(&mut harness), [Sent::On(keys, 60, 100), Sent::Off(keys, 60)]);
    assert_eq!(harness.app.keyboard().sounding(), []);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(stage.heard.take(), [(on_keys, on(60)), (on_keys, off(60))], "the instrument was told so");
    harness.step(Modifiers::NONE, vec![key(Key::A, false)]);
    assert_eq!(log(&mut harness), []);

    // A second track armed beside the first: the note held on the first
    // goes on, since the keys still play it.
    harness.step(Modifiers::NONE, vec![key(Key::S, true)]);
    let other = harness.labeled("Arm Keys");
    harness.click_holding(Modifiers::COMMAND, other);
    assert_eq!(log(&mut harness), [Sent::On(bass, 62, 100)]);
    assert_eq!(harness.app.keyboard().sounding(), [(bass, 62)]);
    // Disarmed, it ends there.
    harness.click_label("Arm Bass");
    assert_eq!(log(&mut harness), [Sent::Off(bass, 62)]);
    harness.step(Modifiers::NONE, vec![key(Key::S, false)]);

    // Armed and disarmed from elsewhere, under held keys.
    harness.step(Modifiers::NONE, vec![key(Key::D, true)]);
    assert_eq!(log(&mut harness), [Sent::On(keys, 64, 100)]);
    harness.session.lock().set_armed(keys, false).unwrap();
    harness.session.lock().set_armed(bass, true).unwrap();
    harness.idle(1);
    assert_eq!(log(&mut harness), [Sent::Off(keys, 64)]);
    assert_eq!(harness.app.keyboard().sounding(), []);
    harness.step(Modifiers::NONE, vec![key(Key::D, false)]);
    assert_eq!(log(&mut harness), []);
}

#[test]
fn held_notes_end_when_the_audio_device_changes() {
    let host = ScriptedHost::with_speakers();
    host.add_device(trill_engine::host::scripted::ScriptedDevice::new("Interface"));
    let options = UiOptions { keyboard: KeyboardOptions { on: true, hold: Vec::new() }, ..Default::default() };
    let (mut harness, _stage) = on_stage(host, ScriptedMidi::new(), options, Storage::Nowhere, |session| {
        trio(session);
    });
    for held in [Key::A, Key::D, Key::G] {
        harness.step(Modifiers::NONE, vec![key(held, true)]);
    }
    assert_eq!(harness.app.keyboard().sounding().len(), 3);
    // Another device is chosen (from elsewhere here; the panel does the
    // same from a worker). The new stream starts in silence, and the
    // keyboard holds nothing it would have to end later.
    let wanted = trill_engine::AudioConfig { output: Some("Interface".into()), ..Default::default() };
    harness.runtime.set_audio(wanted).1.unwrap();
    harness.idle(1);
    assert_eq!(harness.app.keyboard().sounding(), []);
    let sent = &harness.app.keyboard().sent;
    let ends = sent.iter().filter(|message| matches!(message, Sent::Off(..))).count();
    assert_eq!((sent.len(), ends), (6, 3), "every note that was started was ended: {sent:?}");
}

// ---- controllers: what the keyboard gives a track, takes, and silence ---------------

fn control(kind: ControllerKind, step: u8) -> EventKind {
    kind.event(f32::from(step) / 127.0)
}

/// The clip's curve for `kind` as beats in the clip and steps of 127.
fn curve_of(harness: &Harness, clip: ClipId, kind: ControllerKind) -> Vec<(f64, i32)> {
    let project = harness.project();
    let points = project.clip(clip).unwrap().controller(kind).map(|curve| curve.points.clone()).unwrap_or_default();
    points.iter().map(|point| (point.beat, (point.value * 127.0).round() as i32)).collect()
}

#[test]
fn an_instrument_hears_the_keyboards_expression_before_the_first_note() {
    let (mut harness, stage, [keys, ..]) = with_trio(true);
    let on_keys = probe(&harness, keys);
    let expression = ControllerKind::EXPRESSION;
    let wheel = ControllerKind::MOD_WHEEL;

    // Typed: the instrument is given breath, then the note. One that is
    // silent until it has expression sounds from the first key.
    harness.step(Modifiers::NONE, vec![key(Key::A, true)]);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(
        stage.heard.everything(),
        [(on_keys, control(expression, 127)), (on_keys, control(wheel, 0)), (on_keys, on(60))],
        "in that order"
    );

    // Less of it under the held note, and some wheel.
    harness.key(Modifiers::NONE, Key::B);
    harness.key(Modifiers::NONE, Key::Comma);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(stage.heard.everything(), [(on_keys, control(expression, 120)), (on_keys, control(wheel, 10))]);

    // Switched off: the note ends, and the instrument is left with full
    // expression and its wheel at rest.
    harness.key(Modifiers::COMMAND, Key::K);
    stage.run(&mut harness, 2 * BUFFER);
    let mut heard = stage.heard.everything();
    heard.sort_by_key(|(_, kind)| !matches!(kind, EventKind::NoteOff { .. }));
    assert_eq!(heard, [(on_keys, off(60)), (on_keys, control(expression, 127)), (on_keys, control(wheel, 0))]);
    harness.step(Modifiers::NONE, vec![key(Key::A, false)]);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(stage.heard.everything(), []);
}

#[test]
fn a_take_records_the_keyboards_controllers_and_the_lane_shows_them_going_down() {
    let (mut harness, stage, [keys, ..]) = with_trio(true);
    let depth = harness.undo_depth();
    harness.idle(2);
    // A note before the take: from here on the track has the keyboard's
    // expression, and a take begins with it standing there.
    harness.key(Modifiers::NONE, Key::A);
    stage.run(&mut harness, 2 * BUFFER);
    harness.click_label("Arm Keys");
    harness.key(Modifiers::NONE, Key::F9);
    stage.run(&mut harness, 4 * BEAT + BUFFER);
    assert_eq!(recording(&harness), RecordState::Recording);

    // A note held from a beat in. Under it the wheel goes up in three
    // steps, a quarter of a beat apart, and then expression down in two.
    stage.run(&mut harness, BEAT);
    harness.step(Modifiers::NONE, vec![key(Key::A, true)]);
    stage.run(&mut harness, BEAT / 2);
    for _ in 0..3 {
        harness.key(Modifiers::NONE, Key::Comma);
        stage.run(&mut harness, BEAT / 4);
    }
    for _ in 0..2 {
        harness.key(Modifiers::NONE, Key::B);
        stage.run(&mut harness, BEAT / 4);
    }

    // The take is still running, and its clip is what the Clip tab shows:
    // the lane, asked for the wheel, has the curve so far.
    assert_eq!(recording(&harness), RecordState::Recording);
    let clip = harness.project().track(keys).unwrap().clips[0].id;
    assert_eq!(harness.app.state().selected_clip, Some(clip), "the take's clip is shown as it is written");
    harness.click_label("Mod wheel lane");
    harness.idle(2);
    assert!(harness.is_on_screen("Mod wheel curve"));
    let so_far = curve_of(&harness, clip, ControllerKind::MOD_WHEEL);
    assert_eq!(so_far.iter().map(|(_, step)| *step).collect::<Vec<_>>(), [0, 10, 20, 30], "{so_far:?}");
    assert_eq!(so_far[0].0, 0.0, "at rest from the start of the take");
    assert!((so_far[1].0 - 1.5).abs() < 0.1 && (so_far[3].0 - 2.0).abs() < 0.1, "where they were played: {so_far:?}");
    // One more step, and the lane has it a moment later.
    harness.key(Modifiers::NONE, Key::Comma);
    stage.run(&mut harness, BEAT / 4);
    assert_eq!(curve_of(&harness, clip, ControllerKind::MOD_WHEEL).len(), 5);
    // The chip of what the take has written says so without being opened.
    harness.move_to(harness.labeled("Expression lane"));
    harness.idle(1);
    assert!(harness.hint().contains("the clip has a curve for it"), "{}", harness.hint());

    harness.step(Modifiers::NONE, vec![key(Key::A, false)]);
    stage.run(&mut harness, BEAT / 4);
    harness.key(Modifiers::NONE, Key::Space);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(recording(&harness), RecordState::Idle);

    // What was played is in the clip: the note, the expression it was
    // played with from the take's first moment, and the wheel.
    let project = harness.project();
    let taken = project.clip(clip).unwrap();
    assert_eq!(taken.notes().len(), 1);
    let expression = curve_of(&harness, clip, ControllerKind::EXPRESSION);
    assert_eq!(expression.iter().map(|(_, step)| *step).collect::<Vec<_>>(), [127, 120, 110], "{expression:?}");
    assert_eq!(expression[0].0, 0.0, "it stood at full as the take began");
    let at = |kind: ControllerKind, beat: f64| (taken.controller(kind).unwrap().value_at(beat) * 127.0).round() as i32;
    assert_eq!((at(ControllerKind::EXPRESSION, 1.0), at(ControllerKind::EXPRESSION, 3.5)), (127, 110));
    assert_eq!((at(ControllerKind::MOD_WHEEL, 1.0), at(ControllerKind::MOD_WHEEL, 3.5)), (0, 40));
    assert_eq!(harness.selected_clips(), [clip]);

    // The whole take, controllers and all, is one undo step.
    assert_eq!(harness.undo_depth(), depth + 1);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.project().track(keys).unwrap().clips.is_empty());
}

#[test]
fn a_midi_keyboards_wheel_and_pedal_appear_in_the_lane_as_they_are_recorded() {
    let (mut harness, stage, [keys, ..]) = with_trio(false);
    harness.idle(2);
    harness.click_label("Arm Keys");
    harness.key(Modifiers::NONE, Key::F9);
    stage.run(&mut harness, 4 * BEAT + BUFFER);
    assert_eq!(recording(&harness), RecordState::Recording);

    // A note, the pedal down under it, and the pitch wheel pushed up.
    stage.run(&mut harness, BEAT);
    stage.midi.send("Keystation", &[0x90, 60, 100]);
    stage.run(&mut harness, BEAT / 4);
    stage.midi.send("Keystation", &[0xB0, 64, 127]);
    stage.run(&mut harness, BEAT / 4);
    for step in 1..=8u8 {
        // 14 bits, centre 8192: up to half way in eight reports.
        let value = 8192 + u16::from(step) * 512;
        stage.midi.send("Keystation", &[0xE0, (value & 0x7F) as u8, (value >> 7) as u8]);
        stage.run(&mut harness, BEAT / 16);
    }

    // Still recording: the clip is in the piano roll and its lane has
    // the pedal and the wheel, each under its chip.
    assert_eq!(recording(&harness), RecordState::Recording);
    let clip = harness.project().track(keys).unwrap().clips[0].id;
    assert_eq!(harness.app.state().selected_clip, Some(clip));
    harness.click_label("Sustain lane");
    harness.idle(2);
    assert!(harness.is_on_screen("Sustain curve"));
    // (When each arrived is not looked at: a port's messages are stamped
    // with the machine's clock, which is not the scripted device's. The
    // test before this one has the places, from the computer keyboard.)
    let pedal = curve_of(&harness, clip, ControllerKind::SUSTAIN);
    assert_eq!(pedal.last().map(|(_, step)| *step), Some(127), "down: {pedal:?}");
    harness.click_label("Pitch wheel lane");
    harness.idle(2);
    assert!(harness.is_on_screen("Pitch wheel curve"));
    let project = harness.project();
    let wheel = project.clip(clip).unwrap().controller(ControllerKind::PitchBend).expect("the wheel so far");
    let reached = wheel.points.last().unwrap().value;
    assert!((reached - 0.5).abs() < 0.05, "half way up by now: {:?}", wheel.points);

    // The take ends with the pedal down and the wheel up: both are let go
    // of there, in the clip as on the instrument.
    stage.midi.send("Keystation", &[0x80, 60, 0]);
    stage.run(&mut harness, BEAT / 4);
    harness.key(Modifiers::NONE, Key::Space);
    stage.run(&mut harness, 2 * BUFFER);
    let project = harness.project();
    let taken = project.clip(clip).unwrap();
    let last = |kind: ControllerKind| taken.controller(kind).unwrap().points.last().unwrap().value;
    assert_eq!((last(ControllerKind::SUSTAIN), last(ControllerKind::PitchBend)), (0.0, 0.0));
    assert!(taken.controller(ControllerKind::EXPRESSION).is_none(), "a MIDI keyboard says nothing it was not made to");
}

#[test]
fn an_instrument_that_is_given_notes_and_stays_silent_is_pointed_out_where_one_looks() {
    // A clip with a long note on a track whose instrument (the probe)
    // never puts anything out, like a modelled sax without its breath.
    let (mut harness, stage, [keys, bass, _]) = with_trio(false);
    let clip = harness
        .session
        .lock()
        .edit(|project| {
            let clip = project.add_midi_clip(keys, "Line", 0.0, 8.0)?;
            project.clip_mut(clip)?.add_notes(&[Note {
                pitch: 60,
                start: 3.0,
                length: 4.0,
                velocity: 100,
                ..Default::default()
            }])?;
            Ok(clip)
        })
        .unwrap();
    harness.idle(2);
    harness.click(harness.lane(2.0, 0));
    harness.idle(2);
    assert_eq!(harness.app.state().selected_clip, Some(clip));
    let notice = "Probe is given notes and stays silent";
    let mark = format!("Keys: {notice}");
    assert!(!harness.is_on_screen(notice) && !harness.is_on_screen(&mark));

    // The song plays from where the click put the playhead, a beat ahead
    // of the note. Half a second of silence under the note is not said
    // anything about; after a second of it the track and the lane both
    // say so.
    harness.key(Modifiers::NONE, Key::Space);
    stage.run(&mut harness, 2 * BEAT);
    assert!(harness.session.lock().transport().playing);
    assert_eq!(stage.heard.take().len(), 1, "the note has begun");
    assert!(!harness.is_on_screen(&mark), "not yet: an instrument may take a moment");
    stage.run(&mut harness, 2 * BEAT);
    harness.idle(2);
    assert!(harness.is_on_screen(&mark), "on the track's header: {:?}", harness.labels);
    assert!(harness.is_on_screen(notice), "and in the lane's strip under the notes");
    assert!(!harness.is_on_screen("Bass: Probe is given notes and stays silent"), "only the track that is");

    // It says what to do, and a click goes there.
    harness.move_to(harness.labeled(&mark));
    harness.idle(2);
    let advice = harness.hint();
    assert!(advice.contains("wait for expression") && advice.contains("computer keyboard"), "{advice}");
    harness.click_label(notice);
    harness.idle(2);
    assert_eq!(harness.app.state().piano.lane, LaneShown::Controller(ControllerKind::EXPRESSION));
    assert!(harness.is_on_screen("Expression curve"));

    // It stays after the song stops: that is when one looks.
    harness.key(Modifiers::NONE, Key::Space);
    stage.run(&mut harness, 2 * BUFFER);
    harness.idle(2);
    assert!(harness.is_on_screen(&mark));
    // From the header of a track that is not the one being looked at, the
    // click selects the track as well.
    harness.click(harness.header(1, 120.0, 14.0));
    assert_eq!(harness.app.state().selected_track, Some(bass));
    harness.app.state_mut().piano.lane = LaneShown::Velocity;
    harness.click_label(&mark);
    harness.idle(1);
    assert_eq!(harness.app.state().selected_track, Some(keys));
    assert_eq!(harness.app.state().piano.lane, LaneShown::Controller(ControllerKind::EXPRESSION));
}

#[test]
fn the_keyboards_strip_says_when_what_it_plays_makes_no_sound() {
    let (mut harness, stage, _) = with_trio(true);
    let silence = |harness: &Harness| harness.app.keyboard().playing().silence.clone();
    harness.step(Modifiers::NONE, vec![key(Key::A, true)]);
    stage.run(&mut harness, BEAT);
    assert_eq!(silence(&harness), None, "half a second in, nothing is said");
    stage.run(&mut harness, 2 * BEAT);
    harness.idle(1);
    assert_eq!(silence(&harness), Some(Silence::Unanswered("Probe".to_owned())));
    harness.move_to(harness.labeled("Octave down") - vec2(60.0, 0.0));
    harness.idle(2);
    assert_eq!(
        harness.hint(),
        "Probe is given notes and makes no sound \u{00B7} it may have no sound loaded, or its expression or volume is \
         at nothing"
    );
    // The keys go on playing it all the same: it may answer yet.
    harness.step(Modifiers::NONE, vec![key(Key::A, false)]);
    harness.step(Modifiers::NONE, vec![key(Key::D, true)]);
    stage.run(&mut harness, 2 * BUFFER);
    assert_eq!(stage.heard.take().last().map(|(_, kind)| *kind), Some(on(64)));
}

// ---- the real thing ----------------------------------------------------------------

/// SWAM Alto Sax 3, as a VST3 plugin: silent until it is given expression.
const SWAM_SAX: &str = "vst3:565354575361337377616D20616C746F";

/// The window's own code on the installed instrument the feature was asked
/// for, without a window: the plugin is hosted with no editor, the audio
/// device is the scripted one, and the keys are events handed to a frame.
/// Nothing of the user's is read beside the plugin itself.
///
/// ```sh
/// cargo test -p trill-app a_modelled_saxophone -- --include-ignored --nocapture
/// ```
#[test]
#[ignore = "needs SWAM Alto Sax 3 (VST3) installed; run with `cargo test -p trill-app a_modelled_saxophone -- --include-ignored --nocapture`"]
fn a_modelled_saxophone_sounds_when_it_is_simply_typed_on() {
    use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};
    let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/app-controllers");
    let clap =
        ClapConfig { cache_path: Some(scratch.join("clap.json")), scanner: Scanner::Auto, ..ClapConfig::default() };
    let vst3 = Vst3Config {
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    let plugins = PluginHost::with_configs(clap, vst3).unwrap();
    if !plugins.vst3().descriptors().iter().any(|descriptor| descriptor.uri == SWAM_SAX) {
        eprintln!("skipping: {SWAM_SAX} is not installed on this machine");
        return;
    }
    let host = ScriptedHost::with_speakers();
    let parts = Parts {
        devices: plugins.factories(),
        audio: Some(Arc::new(host.clone())),
        midi: Some(Box::new(ScriptedMidi::new())),
        ..Parts::default()
    };
    let runtime = Runtime::start_custom(RuntimeOptions::hermetic(), parts).unwrap();
    let mut sax = None;
    let mut harness = Harness::on(Arc::new(runtime), UiOptions::default(), |session| {
        let track = session.edit(|project| Ok(project.add_track("Sax"))).unwrap();
        session.add_device(ChainRef::Track(track), SWAM_SAX, None, None).unwrap();
        sax = Some(track);
    });
    let sax = sax.unwrap();
    let clock = host.clone();
    harness.app.keyboard_mut().set_clock(Arc::new(move || clock.now_ns()));
    let run = |harness: &mut Harness, seconds: f64| {
        for _ in 0..(seconds * 100.0) as usize {
            host.advance(BUFFER);
        }
        harness.session.lock().housekeeping();
        harness.idle(2);
    };
    // The loudest the output has been since it was last asked, in dB.
    host.keep_output();
    let peak = |_: &Harness| {
        let (left, right) = host.take_output();
        let loudest = left.iter().chain(&right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        20.0 * f64::from(loudest).max(1e-6).log10()
    };
    let notices = |harness: &Harness| -> Vec<String> {
        harness.labels.iter().map(|(text, _)| text.clone()).filter(|text| text.contains("is given notes")).collect()
    };
    // The plugin goes on waking up for a moment after it is loaded, and
    // is silent whatever it is given until it has: a few seconds of the
    // machine's own time, which a window gets by being looked at.
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        run(&mut harness, 0.05);
    }
    peak(&harness);

    // A note as a MIDI keyboard without an expression pedal sends it: the
    // saxophone stays silent, and the window says so on the track.
    harness.session.lock().note_on(sax, 64, 100);
    run(&mut harness, 1.5);
    let unblown = peak(&harness);
    let said = notices(&harness);
    harness.session.lock().note_off(sax, 64);
    run(&mut harness, 0.3);
    peak(&harness);

    // The computer keyboard is switched on and the same note typed.
    harness.key(Modifiers::COMMAND, Key::K);
    assert!(harness.app.keyboard().is_on());
    harness.step(Modifiers::NONE, vec![key(Key::D, true)]);
    run(&mut harness, 1.5);
    let typed = peak(&harness);
    let said_while_typing = notices(&harness);
    // Under the held note, expression down to 60 with the B key.
    for _ in 0..7 {
        harness.key(Modifiers::NONE, Key::B);
    }
    assert_eq!(harness.app.keyboard().expression(), 60);
    run(&mut harness, 1.0);
    peak(&harness);
    run(&mut harness, 1.0);
    let softer = peak(&harness);
    // And down to nothing.
    for _ in 0..6 {
        harness.key(Modifiers::NONE, Key::B);
    }
    assert_eq!(harness.app.keyboard().expression(), 0);
    // The instrument's own room rings on for a while after the breath.
    run(&mut harness, 2.5);
    peak(&harness);
    run(&mut harness, 1.0);
    let none = peak(&harness);
    harness.step(Modifiers::NONE, vec![key(Key::D, false)]);
    run(&mut harness, 0.2);

    eprintln!("a note without expression: {unblown:.1} dB, and the window says {said:?}");
    eprintln!("the same note typed on the computer keyboard: {typed:.1} dB, and the window says {said_while_typing:?}");
    eprintln!("with expression at 60: {softer:.1} dB; at 0: {none:.1} dB");
    assert!(unblown < -80.0, "silent without expression: {unblown:.1} dB");
    assert!(said.iter().any(|text| text.starts_with("Sax: ")), "the track is marked: {said:?}");
    assert!(typed > -40.0, "typed, it sounds: {typed:.1} dB");
    assert!(said_while_typing.is_empty(), "and nothing is said about a silence there is not: {said_while_typing:?}");
    assert!(softer < typed - 3.0, "less expression is less sound: {softer:.1} dB after {typed:.1} dB");
    assert!(none < typed - 30.0, "and none is next to nothing: {none:.1} dB");
}
