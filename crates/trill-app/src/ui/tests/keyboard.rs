//! The computer keyboard played as an instrument: what its keys send to the
//! session, when it leaves the keys to the rest of the window, what it
//! shows, and, case by case, that no note it starts is left without its end.
//!
//! What was sent is read from the keyboard's own log of its calls to
//! `Session::note_on` and `note_off`, in order; one test listens to the
//! engine as well.

use super::*;
use crate::ui::KeyboardOptions;
use crate::ui::assistant::testing::Script;
use crate::ui::keyboard::{Sent, Silence};
use crate::ui::state::{FocusArea, Tool};
use trill_core::{ControllerKind, TrackKind};

const CTRL: Modifiers = Modifiers::COMMAND;
const ALT: Modifiers = Modifiers::ALT;
const NONE: Modifiers = Modifiers::NONE;

/// Three tracks: two with a synth, and one with nothing to play.
fn band(session: &mut Session) -> [TrackId; 3] {
    session
        .edit(|project| {
            let tracks = [project.add_track("Keys"), project.add_track("Bass"), project.add_track("Empty")];
            for track in &tracks[..2] {
                project.add_device(ChainRef::Track(*track), "builtin:polysynth", "Poly Synth", None)?;
            }
            Ok(tracks)
        })
        .unwrap()
}

fn with_band(options: UiOptions) -> (Harness, [TrackId; 3]) {
    let mut tracks = None;
    let harness = Harness::new(options, |session| tracks = Some(band(session)));
    (harness, tracks.unwrap())
}

fn switched_on() -> UiOptions {
    UiOptions { keyboard: KeyboardOptions { on: true, hold: Vec::new() }, ..Default::default() }
}

/// The window on the band, with the keyboard switched on. The first track
/// is the selected one.
fn playing() -> (Harness, [TrackId; 3]) {
    with_band(switched_on())
}

fn key_event(key: Key, pressed: bool) -> Event {
    Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: NONE }
}

/// One frame in which `key` goes down.
fn down(harness: &mut Harness, key: Key) {
    harness.step(NONE, vec![key_event(key, true)]);
}

/// One frame in which `key` comes up.
fn up(harness: &mut Harness, key: Key) {
    harness.step(NONE, vec![key_event(key, false)]);
}

/// A letter typed the way a windowing backend delivers it: the key and the
/// text it makes, then the key coming up.
fn type_letter(harness: &mut Harness, key: Key, letter: &str) {
    harness.step(NONE, vec![key_event(key, true), Event::Text(letter.to_owned())]);
    up(harness, key);
}

/// Everything the keyboard has sent to the session, in order.
fn log(harness: &Harness) -> Vec<Sent> {
    harness.app.keyboard().sent.clone()
}

fn forget_log(harness: &mut Harness) {
    harness.app.keyboard_mut().sent.clear();
}

/// The notes the keyboard has started and not stopped.
fn sounding(harness: &Harness) -> Vec<(TrackId, u8)> {
    harness.app.keyboard().sounding()
}

/// The notes of `log` that were started and never stopped. A note stopped
/// that was not sounding, or started twice over, is a failure of its own.
fn unended(log: &[Sent]) -> Vec<(TrackId, u8)> {
    let mut open: Vec<(TrackId, u8)> = Vec::new();
    for message in log {
        match *message {
            Sent::On(track, pitch, _) => {
                assert!(!open.contains(&(track, pitch)), "{pitch} was started while it was sounding: {log:?}");
                open.push((track, pitch));
            }
            Sent::Off(track, pitch) => {
                let index = open.iter().position(|note| *note == (track, pitch));
                open.remove(index.unwrap_or_else(|| panic!("{pitch} was stopped without sounding: {log:?}")));
            }
        }
    }
    open
}

/// Holds a C major chord down on the home row.
fn hold_a_chord(harness: &mut Harness) {
    for key in [Key::A, Key::D, Key::G] {
        down(harness, key);
    }
    assert_eq!(sounding(harness).len(), 3, "the chord sounds");
}

/// Nothing is sounding, and the log agrees.
#[track_caller]
fn assert_all_ended(harness: &Harness) {
    assert_eq!(sounding(harness), [], "the keyboard still holds notes");
    assert_eq!(unended(&log(harness)), [], "notes were started and never stopped");
}

/// The fingers come off the chord, after whatever stopped its notes: that
/// must send nothing more.
#[track_caller]
fn let_go_of_the_chord(harness: &mut Harness) {
    let before = log(harness);
    for key in [Key::A, Key::D, Key::G] {
        up(harness, key);
    }
    assert_eq!(log(harness), before, "a key that holds no note stops none");
}

const EXPRESSION: ControllerKind = ControllerKind::EXPRESSION;
const MOD_WHEEL: ControllerKind = ControllerKind::MOD_WHEEL;

/// Every controller the keyboard has sent to the session, in order, in
/// steps of 127.
fn controls(harness: &Harness) -> Vec<(TrackId, ControllerKind, u8)> {
    harness.app.keyboard().controls.clone()
}

fn forget_controls(harness: &mut Harness) {
    harness.app.keyboard_mut().controls.clear();
}

/// Where a controller is when no keyboard has it anywhere else: full
/// expression, the mod wheel down.
fn at_rest(kind: ControllerKind) -> u8 {
    if kind == EXPRESSION { 127 } else { 0 }
}

/// What the keyboard holds and what it has sent agree: the last word each
/// track had about a controller is what the keyboard has written down for
/// it, and for a track it has written nothing down for, rest. A controller
/// cannot be moved and forgotten.
#[track_caller]
fn assert_controllers_accounted_for(harness: &Harness, context: &str) {
    let keyboard = harness.app.keyboard();
    let told = keyboard.told();
    let mut last: Vec<((TrackId, ControllerKind), u8)> = Vec::new();
    for (track, kind, value) in &keyboard.controls {
        match last.iter_mut().find(|(which, _)| *which == (*track, *kind)) {
            Some(entry) => entry.1 = *value,
            None => last.push(((*track, *kind), *value)),
        }
    }
    for ((track, kind), value) in last {
        let held = told
            .iter()
            .find(|(on, ..)| *on == track)
            .and_then(|(_, expression, mod_wheel)| if kind == EXPRESSION { *expression } else { *mod_wheel });
        let expected = held.unwrap_or(at_rest(kind));
        assert_eq!(value, expected, "{context}: {kind:?} on {track:?} was left at {value}; told {told:?}");
    }
    if !keyboard.is_on() {
        assert!(told.is_empty(), "{context}: switched off with controllers held: {told:?}");
    }
}

/// The keyboard holds no controller anywhere, and every one it moved is
/// back at rest.
#[track_caller]
fn assert_all_given_back(harness: &Harness) {
    assert_eq!(harness.app.keyboard().told(), [], "the keyboard still holds controllers");
    assert_controllers_accounted_for(harness, "given back");
}

/// Holds a chord on `track` with the expression turned down to 100 and the
/// mod wheel up at 20, and forgets what was sent to get there.
fn play_with_controllers(harness: &mut Harness, track: TrackId) {
    hold_a_chord(harness);
    for key in [Key::B, Key::B, Key::B, Key::Comma, Key::Comma] {
        harness.key(NONE, key);
    }
    assert_eq!(harness.app.keyboard().told(), [(track, Some(100), Some(20))]);
    assert_eq!(sounding(harness).len(), 3, "the chord goes on under them");
    forget_controls(harness);
}

/// How the news that the window is no longer typed into arrives.
#[derive(Copy, Clone, Debug)]
enum Lost {
    /// A frame is run on it, as while the window can be seen.
    Seen,
    /// Only what runs while the window is hidden hears of it: no frame.
    Hidden,
    /// A frame is run on it without the hook before the frame having run.
    FrameOnly,
}

/// The window stops being the one typed into.
fn lose_focus(harness: &mut Harness, how: Lost) {
    harness.time += FRAME;
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(harness.time),
        events: vec![Event::WindowFocused(false)],
        focused: false,
        ..Default::default()
    };
    if !matches!(how, Lost::FrameOnly) {
        eframe::App::raw_input_hook(&mut harness.app, &harness.ctx, &mut input);
    }
    if !matches!(how, Lost::Hidden) {
        harness.app.tick(&harness.ctx);
        let mut output = harness.ctx.run_ui(input, |ui| harness.app.frame(ui));
        output.textures_delta.clear();
    }
}

/// The devices' search field at the top of the browser.
fn device_search(harness: &Harness) -> Pos2 {
    pos2(harness.work_left() / 2.0, top_bar::HEIGHT + browser::HEADER + 8.0 + 11.0)
}

// ---- typing plays notes --------------------------------------------------------

#[test]
fn the_home_row_plays_the_selected_track() {
    let (mut harness, [keys, ..]) = playing();
    let before = harness.undo_depth();
    down(&mut harness, Key::A);
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100)], "the note leaves in the frame its key arrives in");
    assert_eq!(sounding(&harness), [(keys, 60)]);
    // The press was the keyboard's: nothing later in the frame saw a key.
    assert!(!harness.ctx.input(|input| input.key_pressed(Key::A)));
    up(&mut harness, Key::A);
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100), Sent::Off(keys, 60)]);
    assert_all_ended(&harness);

    // The row above is the black keys, and the right hand reaches into
    // the next octave.
    let rest = [
        (Key::W, 61),
        (Key::S, 62),
        (Key::E, 63),
        (Key::D, 64),
        (Key::F, 65),
        (Key::T, 66),
        (Key::G, 67),
        (Key::Y, 68),
        (Key::H, 69),
        (Key::U, 70),
        (Key::J, 71),
        (Key::K, 72),
        (Key::O, 73),
        (Key::L, 74),
        (Key::P, 75),
        (Key::Semicolon, 76),
        (Key::Quote, 77),
    ];
    for (key, pitch) in rest {
        forget_log(&mut harness);
        harness.key(NONE, key);
        assert_eq!(log(&harness), [Sent::On(keys, pitch, 100), Sent::Off(keys, pitch)], "{key:?}");
    }

    // Playing is not editing, and the letters did nothing else.
    assert_eq!(harness.undo_depth(), before);
    assert!(harness.app.state().arrangement.lanes.is_empty(), "A opened no automation lane");
    assert!(!harness.project().loop_region.enabled, "L did not switch the loop");
    assert!(!harness.session.lock().metronome(), "K did not start the metronome");
    assert!(harness.app.state().arrangement.follow, "F left the view following");
}

#[test]
fn chords_sound_together_held_keys_sustain_and_key_repeat_does_not_retrigger() {
    let (mut harness, [keys, ..]) = playing();
    hold_a_chord(&mut harness);
    assert_eq!(sounding(&harness), [(keys, 60), (keys, 64), (keys, 67)]);

    // Held for a second, the system repeating the keys as it does.
    for _ in 0..60 {
        let repeats = [Key::A, Key::D, Key::G].map(|key| key_event(key, true)).to_vec();
        harness.step(NONE, repeats);
    }
    assert_eq!(log(&harness).len(), 3, "three notes, each started once");
    assert_eq!(sounding(&harness).len(), 3, "and still sounding");

    up(&mut harness, Key::D);
    assert_eq!(sounding(&harness), [(keys, 60), (keys, 67)], "letting one key go leaves the others");
    assert_eq!(log(&harness).last(), Some(&Sent::Off(keys, 64)));
    up(&mut harness, Key::A);
    up(&mut harness, Key::G);
    assert_all_ended(&harness);
}

#[test]
fn a_key_that_goes_down_and_up_within_a_frame_is_a_short_note() {
    let (mut harness, [keys, ..]) = playing();
    harness.step(NONE, vec![key_event(Key::A, true), key_event(Key::A, false)]);
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100), Sent::Off(keys, 60)]);
    // Two keys rolled within one frame keep their order.
    forget_log(&mut harness);
    let roll = vec![key_event(Key::S, true), key_event(Key::D, true), key_event(Key::S, false)];
    harness.step(NONE, roll);
    assert_eq!(log(&harness), [Sent::On(keys, 62, 100), Sent::On(keys, 64, 100), Sent::Off(keys, 62)]);
    up(&mut harness, Key::D);
    assert_all_ended(&harness);
}

#[test]
fn the_keys_are_where_they_are_on_the_keyboard_whatever_they_type() {
    let (mut harness, [keys, ..]) = playing();
    // An AZERTY keyboard: the first key of the home row types Q.
    let azerty =
        |pressed| Event::Key { key: Key::Q, physical_key: Some(Key::A), pressed, repeat: false, modifiers: NONE };
    harness.step(NONE, vec![azerty(true)]);
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100)], "the C, as on any layout");
    harness.step(NONE, vec![azerty(false)]);
    assert_all_ended(&harness);
    // And the key that types A there is no note at all: it is on the row
    // above the black keys.
    let top_left =
        |pressed| Event::Key { key: Key::A, physical_key: Some(Key::Q), pressed, repeat: false, modifiers: NONE };
    forget_log(&mut harness);
    harness.step(NONE, vec![top_left(true)]);
    harness.step(NONE, vec![top_left(false)]);
    assert_eq!(log(&harness), []);
}

#[test]
fn z_and_x_move_the_octave_and_c_and_v_the_velocity() {
    let (mut harness, [keys, ..]) = playing();
    let view = harness.app.state().arrangement.time;
    harness.key(NONE, Key::Z);
    assert_eq!(harness.app.keyboard().lowest(), 48);
    harness.key(NONE, Key::A);
    assert_eq!(log(&harness), [Sent::On(keys, 48, 100), Sent::Off(keys, 48)]);
    harness.key(NONE, Key::X);
    harness.key(NONE, Key::X);
    assert_eq!((harness.app.keyboard().lowest(), harness.app.keyboard().highest()), (72, 89));

    harness.key(NONE, Key::C);
    assert_eq!(harness.app.keyboard().velocity(), 90);
    forget_log(&mut harness);
    harness.key(NONE, Key::K);
    assert_eq!(log(&harness), [Sent::On(keys, 84, 90), Sent::Off(keys, 84)], "the octave above, a step softer");
    for expected in [100, 110, 120, 127, 127] {
        harness.key(NONE, Key::V);
        assert_eq!(harness.app.keyboard().velocity(), expected);
    }
    // A held velocity key goes on stepping, as a held arrow would.
    down(&mut harness, Key::C);
    for _ in 0..3 {
        down(&mut harness, Key::C);
    }
    up(&mut harness, Key::C);
    assert_eq!(harness.app.keyboard().velocity(), 90);

    // The ends of the range: every key is still a note or nothing at all.
    for _ in 0..12 {
        harness.key(NONE, Key::Z);
    }
    assert_eq!(harness.app.keyboard().lowest(), 0);
    for _ in 0..12 {
        harness.key(NONE, Key::X);
    }
    assert_eq!((harness.app.keyboard().lowest(), harness.app.keyboard().highest()), (108, 125));
    forget_log(&mut harness);
    harness.key(NONE, Key::Quote);
    assert_eq!(log(&harness), [Sent::On(keys, 125, 90), Sent::Off(keys, 125)]);

    assert_eq!(harness.app.state().arrangement.time, view, "Z moved the octave and zoomed nothing");
    assert_eq!(harness.app.state().arrangement.tool, Tool::Pointer, "C changed the velocity and took up no blade");
}

#[test]
fn a_typed_note_is_heard_until_its_key_comes_up() {
    let (mut harness, [keys, ..]) = playing();
    let meter = harness.session.lock().track_meter(keys).expect("the track is metered");
    // The loudest sample in the next `window`.
    let peak = |window: Duration| {
        meter.take();
        std::thread::sleep(window);
        let (left, right) = meter.take();
        left.max(right)
    };
    let wait_until = |what: &str, patience: Duration, met: &dyn Fn(f32) -> bool| {
        let deadline = Instant::now() + patience;
        while !met(peak(Duration::from_millis(40))) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
        }
    };
    assert!(peak(Duration::from_millis(60)) < 1e-4, "nothing sounds before a key is pressed");
    down(&mut harness, Key::A);
    wait_until("the note to be heard", Duration::from_secs(5), &|level| level > 0.01);
    up(&mut harness, Key::A);
    wait_until("the note to die away", Duration::from_secs(15), &|level| level < 1e-4);
}

// ---- the switch ----------------------------------------------------------------

#[test]
fn the_switch_is_in_the_top_bar_in_the_view_menu_and_on_ctrl_k() {
    let (mut harness, [keys, ..]) = with_band(UiOptions::default());
    assert!(!harness.app.keyboard().is_on(), "off until it is asked for");
    assert!(!harness.is_on_screen("Octave up"), "and its strip is not there");
    let switch = harness.labeled("Computer keyboard");
    assert!(switch.y < top_bar::HEIGHT, "the switch is in the top bar");

    harness.click(switch);
    assert!(harness.app.keyboard().is_on());
    harness.idle(2);
    assert!(harness.is_on_screen("Octave up") && harness.is_on_screen("Louder"), "the strip says what it plays");
    harness.key(NONE, Key::A);
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100), Sent::Off(keys, 60)]);

    harness.key(CTRL, Key::K);
    assert!(!harness.app.keyboard().is_on(), "Ctrl+K switches it");
    harness.idle(2);
    assert!(!harness.is_on_screen("Octave up"));
    assert!(!harness.session.lock().metronome(), "and is not the metronome's K");

    harness.click_label("View");
    harness.click_last_label("Computer keyboard");
    assert!(harness.app.keyboard().is_on(), "the View menu has it too");
    harness.key(CTRL, Key::K);
    assert!(!harness.app.keyboard().is_on());
}

#[test]
fn switched_off_the_letters_are_the_shortcuts_they_always_were() {
    let (mut harness, ..) = with_band(UiOptions::default());
    harness.key(NONE, Key::A);
    assert_eq!(harness.app.state().arrangement.lanes.len(), 1, "A shows the track's automation");
    harness.key(NONE, Key::L);
    assert!(harness.project().loop_region.enabled, "L loops");
    harness.key(NONE, Key::K);
    assert!(harness.session.lock().metronome(), "K is the metronome");
    harness.key(NONE, Key::F);
    assert!(!harness.app.state().arrangement.follow, "F stops following");
    harness.key(NONE, Key::C);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Blade, "C is the blade");
    harness.key(NONE, Key::Tab);
    harness.key(NONE, Key::X);
    harness.key(NONE, Key::V);
    // A letter with no shortcut is left for whatever wants it.
    down(&mut harness, Key::S);
    assert!(harness.ctx.input(|input| input.key_pressed(Key::S)));
    up(&mut harness, Key::S);
    let keyboard = harness.app.keyboard();
    assert_eq!((keyboard.lowest(), keyboard.velocity(), keyboard.sustaining()), (60, 100, false));
    assert_eq!(log(&harness), [], "and nothing was played");
    harness.move_to(harness.labeled("Loop"));
    harness.idle(2);
    assert!(harness.hint().ends_with("\u{00B7} L"), "{}", harness.hint());
}

#[test]
fn switched_on_alt_with_a_letter_is_the_shortcut_the_letter_had() {
    let (mut harness, ..) = playing();
    harness.key(ALT, Key::A);
    assert_eq!(harness.app.state().arrangement.lanes.len(), 1, "Alt+A shows the track's automation");
    harness.key(ALT, Key::L);
    assert!(harness.project().loop_region.enabled, "Alt+L loops");
    harness.key(ALT, Key::K);
    assert!(harness.session.lock().metronome(), "Alt+K is the metronome");
    harness.key(ALT, Key::F);
    assert!(!harness.app.state().arrangement.follow, "Alt+F stops following");
    harness.key(ALT, Key::C);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Blade, "Alt+C is the blade");

    // So are the letters that step expression and the mod wheel.
    harness.key(ALT, Key::B);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Draw, "Alt+B is the pencil");
    harness.key(ALT, Key::M);
    assert_eq!(harness.project().markers.len(), 1, "Alt+M adds a marker");
    assert_eq!((harness.app.keyboard().expression(), harness.app.keyboard().mod_wheel()), (127, 0), "and step nothing");
    // Every chord that had a modifier of its own is what it was.
    harness.key(CTRL, Key::T);
    assert_eq!(harness.project().tracks.len(), 4, "Ctrl+T still adds a track");
    harness.key(CTRL, Key::Z);
    harness.key(CTRL, Key::Z);
    assert_eq!((harness.project().tracks.len(), harness.project().markers.len()), (3, 0), "Ctrl+Z still undoes");
    harness.key(NONE, Key::Space);
    harness.wait_for("playback to start", |session| session.transport().playing);
    harness.idle(1);
    harness.key(NONE, Key::Space);
    harness.wait_for("playback to stop", |session| !session.transport().playing);
    assert_eq!(log(&harness), [], "none of that played a note");

    // The hints say which keys it is now.
    harness.move_to(harness.labeled("Loop"));
    harness.idle(2);
    assert!(harness.hint().ends_with("\u{00B7} Alt+L"), "{}", harness.hint());
    harness.move_to(harness.labeled("Metronome"));
    harness.idle(2);
    assert!(harness.hint().ends_with("\u{00B7} Alt+K"), "{}", harness.hint());
}

#[test]
fn it_is_remembered_between_sessions() {
    let home = tempfile::tempdir().unwrap();
    let start = || {
        let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
        Arc::new(Runtime::start(RuntimeOptions { storage, ..RuntimeOptions::hermetic() }).unwrap())
    };
    let remembered = home.path().join("keyboard.json");

    // The first session: off, and nothing is written for having looked.
    let mut harness = Harness::on(start(), UiOptions::default(), |session| {
        band(session);
    });
    assert!(!harness.app.keyboard().is_on());
    harness.idle(60);
    assert!(!remembered.exists());

    harness.key(CTRL, Key::K);
    harness.key(NONE, Key::Z);
    harness.key(NONE, Key::C);
    harness.click_label("Hide the keys");
    // Written down once it has stopped changing, without waiting for the
    // program to close.
    harness.idle(60);
    harness.settle_work();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&remembered).unwrap()).unwrap();
    assert_eq!(saved, serde_json::json!({ "on": true, "lowest": 48, "velocity": 90, "key_map": false }));
    // And at once on the way out, whatever changed last.
    harness.key(NONE, Key::V);
    harness.key(NONE, Key::V);
    eframe::App::on_exit(&mut harness.app);
    drop(harness);

    let mut harness = Harness::on(start(), UiOptions::default(), |session| {
        band(session);
    });
    let keyboard = harness.app.keyboard();
    assert!(keyboard.is_on(), "on, as it was left");
    assert_eq!((keyboard.lowest(), keyboard.velocity(), keyboard.shows_key_map()), (48, 110, false));
    assert!(harness.is_on_screen("Show the keys"));

    // Switched off, it stays off.
    harness.key(CTRL, Key::K);
    eframe::App::on_exit(&mut harness.app);
    drop(harness);
    let harness = Harness::on(start(), UiOptions::default(), |_| {});
    assert!(!harness.app.keyboard().is_on());
    assert_eq!(harness.app.keyboard().lowest(), 48, "with its octave kept for the next time");
}

// ---- it never gets in the way of typing ----------------------------------------

#[test]
fn nothing_plays_while_a_search_field_a_name_or_a_number_is_typed() {
    let (mut harness, _) = playing();

    // The devices' search field.
    harness.click(device_search(&harness));
    for (key, letter) in [(Key::S, "s"), (Key::A, "a"), (Key::D, "d")] {
        type_letter(&mut harness, key, letter);
    }
    assert_eq!(harness.app.state().browser.search, "sad", "the letters went to the field");
    assert_eq!(harness.app.keyboard().playing().silence, Some(Silence::Typing), "and the strip says why");
    harness.key(NONE, Key::Escape);
    harness.idle(2);

    // A track's name.
    harness.double_click(harness.header(0, 80.0, 14.0));
    harness.idle(2);
    assert!(harness.app.state().renaming_track.is_some(), "the name is being typed");
    harness.key(CTRL, Key::A);
    for (key, letter) in [(Key::F, "f"), (Key::A, "a"), (Key::D, "d"), (Key::E, "e")] {
        type_letter(&mut harness, key, letter);
    }
    harness.key(NONE, Key::Enter);
    assert_eq!(harness.project().tracks[0].name, "fade");

    // The tempo, typed as a number. Letters are no number, and no note.
    harness.settle();
    harness.double_click_label("Tempo");
    harness.idle(2);
    harness.key(CTRL, Key::A);
    type_letter(&mut harness, Key::Num9, "9");
    type_letter(&mut harness, Key::A, "a");
    harness.key(NONE, Key::Tab);
    assert!(!harness.app.keyboard().sustaining(), "Tab is the field's while it is typed in");
    harness.key(NONE, Key::Escape);
    harness.idle(2);

    assert_eq!(log(&harness), [], "nothing was played in all of that");
    assert_eq!(harness.app.keyboard().lowest(), 60, "and Z, X, C and V moved nothing");

    // With the field left, the keys play again.
    harness.key(NONE, Key::A);
    assert_eq!(log(&harness).len(), 2);
    assert_eq!(harness.app.keyboard().playing().silence, None);
}

#[test]
fn nothing_plays_while_the_assistant_is_written_to() {
    let (mut harness, _) = playing();
    harness.app.assistant_mut().use_script(Script::ready());
    harness.key(CTRL | Modifiers::SHIFT, Key::A);
    assert!(harness.app.assistant().is_open());
    harness.idle(3);
    let field = harness.app.assistant().field();
    harness.click(field.center());
    for (key, letter) in [(Key::A, "a"), (Key::D, "d"), (Key::D, "d")] {
        type_letter(&mut harness, key, letter);
    }
    assert_eq!(harness.app.assistant().draft(), "add");
    assert_eq!(log(&harness), []);
}

#[test]
fn nothing_plays_while_typing_searches_the_sounds() {
    let options = UiOptions { browser_tab: Some(BrowserTab::Sounds), ..switched_on() };
    let (mut harness, _) = with_band(options);
    // The list of sounds is being worked in: letters start a search there.
    harness.app.state_mut().focus = FocusArea::Browser;
    harness.idle(1);
    assert_eq!(harness.app.keyboard().playing().silence, Some(Silence::Searching));
    type_letter(&mut harness, Key::K, "k");
    harness.idle(2);
    assert_eq!(harness.app.state().browser.sounds.search, "k");
    assert_eq!(log(&harness), []);
}

#[test]
fn nothing_plays_with_ctrl_alt_shift_or_the_windows_key_held() {
    let (mut harness, [keys, ..]) = playing();
    for modifiers in [CTRL, ALT, Modifiers::SHIFT, CTRL | Modifiers::SHIFT, ALT | Modifiers::SHIFT, CTRL | ALT] {
        harness.key(modifiers, Key::D);
        harness.key(modifiers, Key::X);
        harness.key(modifiers, Key::Tab);
    }
    assert_eq!(log(&harness), []);
    assert_eq!((harness.app.keyboard().lowest(), harness.app.keyboard().sustaining()), (60, false));

    harness.app.keyboard_mut().windows_key = true;
    harness.key(NONE, Key::D);
    harness.key(NONE, Key::X);
    assert_eq!(log(&harness), []);
    assert_eq!(harness.app.keyboard().lowest(), 60);
    harness.app.keyboard_mut().windows_key = false;

    // A note that was started ends when its key comes up, whatever has
    // been pressed down beside it by then.
    down(&mut harness, Key::D);
    let release = Event::Key { key: Key::D, physical_key: None, pressed: false, repeat: false, modifiers: CTRL };
    harness.step(CTRL, vec![release]);
    assert_eq!(log(&harness), [Sent::On(keys, 64, 100), Sent::Off(keys, 64)]);
}

// ---- no stuck notes, case by case ------------------------------------------------

#[test]
fn notes_stop_when_the_switch_goes_off() {
    // By its shortcut.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.key(CTRL, Key::K);
    assert!(!harness.app.keyboard().is_on());
    assert_all_ended(&harness);
    let_go_of_the_chord(&mut harness);

    // By its button, with the sustain held as well.
    let (mut harness, _) = playing();
    down(&mut harness, Key::Tab);
    hold_a_chord(&mut harness);
    up(&mut harness, Key::A);
    assert_eq!(sounding(&harness).len(), 3, "the sustain keeps the note whose key came up");
    harness.click_label("Computer keyboard");
    assert!(!harness.app.keyboard().is_on());
    assert_all_ended(&harness);
    assert!(!harness.app.keyboard().sustaining());
}

#[test]
fn notes_stop_when_another_track_is_selected() {
    let (mut harness, [keys, bass, empty]) = playing();
    hold_a_chord(&mut harness);
    // A click on another track's header, the keys still down.
    harness.click(harness.header(1, 120.0, 14.0));
    assert_eq!(harness.app.state().selected_track, Some(bass));
    assert_all_ended(&harness);
    let stopped: Vec<Sent> = log(&harness).into_iter().skip(3).collect();
    assert_eq!(stopped, [Sent::Off(keys, 60), Sent::Off(keys, 64), Sent::Off(keys, 67)], "on the track they began on");
    let_go_of_the_chord(&mut harness);

    // What is played next is played there.
    forget_log(&mut harness);
    down(&mut harness, Key::A);
    assert_eq!(log(&harness), [Sent::On(bass, 60, 100)]);

    // The selection moved by something other than a click (a sound dropped
    // on a track, an edit from elsewhere): in the frame it is noticed.
    harness.app.state_mut().select_track(empty);
    harness.idle(1);
    assert_all_ended(&harness);
    assert_eq!(log(&harness), [Sent::On(bass, 60, 100), Sent::Off(bass, 60)]);
}

#[test]
fn notes_stop_when_the_octave_changes_under_their_keys() {
    let (mut harness, [keys, ..]) = playing();
    down(&mut harness, Key::A);
    down(&mut harness, Key::G);
    down(&mut harness, Key::X);
    assert_eq!(harness.app.keyboard().lowest(), 72);
    assert_all_ended(&harness);
    let before = log(&harness);
    assert_eq!(before[2..], [Sent::Off(keys, 60), Sent::Off(keys, 67)], "the notes that sounded, not an octave above");
    // The keys come up: they hold nothing now, and stop nothing.
    up(&mut harness, Key::A);
    up(&mut harness, Key::G);
    up(&mut harness, Key::X);
    assert_eq!(log(&harness), before);
    // Pressed again, they play where the keyboard now is.
    harness.key(NONE, Key::A);
    assert_eq!(log(&harness)[before.len()..], [Sent::On(keys, 72, 100), Sent::Off(keys, 72)]);

    // The same from the strip's own octave keys, clicked with a note held.
    down(&mut harness, Key::D);
    harness.click_label("Octave down");
    assert_eq!(harness.app.keyboard().lowest(), 60);
    assert_all_ended(&harness);
}

#[test]
fn notes_stop_when_the_window_is_no_longer_typed_into() {
    for how in [Lost::Seen, Lost::Hidden, Lost::FrameOnly] {
        let (mut harness, [keys, ..]) = playing();
        down(&mut harness, Key::Tab);
        hold_a_chord(&mut harness);
        lose_focus(&mut harness, how);
        assert_all_ended(&harness);
        assert!(!harness.app.keyboard().sustaining(), "{how:?}: the sustain is let go with them");
        // Back in the window, the keys that were held come up at some
        // point; no key-up is owed to them.
        let before = log(&harness);
        harness.idle(2);
        let_go_of_the_chord(&mut harness);
        up(&mut harness, Key::Tab);
        assert_eq!(log(&harness), before, "{how:?}");
        // And the keyboard plays as before.
        harness.key(NONE, Key::A);
        assert_eq!(log(&harness)[before.len()..], [Sent::On(keys, 60, 100), Sent::Off(keys, 60)], "{how:?}");
    }
}

#[test]
fn a_note_begun_just_before_the_window_lost_the_keyboard_is_ended_there() {
    // Away and back between two frames, as after the window was hidden:
    // the key that went down before leaving sends its key-up elsewhere,
    // so its note ends where the keyboard was lost; what goes down after
    // coming back plays.
    let (mut harness, [keys, ..]) = playing();
    let events =
        vec![key_event(Key::A, true), Event::WindowFocused(false), Event::WindowFocused(true), key_event(Key::D, true)];
    harness.step(NONE, events);
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100), Sent::Off(keys, 60), Sent::On(keys, 64, 100)]);
    up(&mut harness, Key::D);
    assert_all_ended(&harness);
}

#[test]
fn nothing_plays_in_a_window_that_is_not_typed_into() {
    // Key events do not reach a window without the keyboard; were one to,
    // it would start nothing that no key-up could be counted on to end.
    let (mut harness, _) = playing();
    harness.time += FRAME;
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
        time: Some(harness.time),
        events: vec![key_event(Key::A, true)],
        focused: false,
        ..Default::default()
    };
    harness.app.tick(&harness.ctx);
    let mut output = harness.ctx.run_ui(input, |ui| harness.app.frame(ui));
    output.textures_delta.clear();
    assert_eq!(log(&harness), []);
}

#[test]
fn notes_stop_when_a_dialog_takes_the_keyboard() {
    // The question a project with unsaved changes asks before giving way.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.key(CTRL, Key::N);
    assert!(harness.is_on_screen("Discard and start new"), "the question is up");
    assert_all_ended(&harness);
    let_go_of_the_chord(&mut harness);
    // Behind it nothing plays.
    harness.key(NONE, Key::S);
    assert_eq!(log(&harness).len(), 6);

    // The shortcut reference.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.key(NONE, Key::F1);
    assert!(harness.app.state().shortcuts_open);
    assert_all_ended(&harness);
    let_go_of_the_chord(&mut harness);
    harness.key(NONE, Key::S);
    assert_eq!(log(&harness).len(), 6, "nothing plays behind it");
    harness.key(NONE, Key::Escape);
    harness.idle(2);
    harness.key(NONE, Key::S);
    assert_eq!(log(&harness).len(), 8, "closed, the keys play again");
}

#[test]
fn notes_stop_when_a_text_field_takes_the_keyboard() {
    // A click into a search field with the keys still down.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.click(device_search(&harness));
    assert_all_ended(&harness);
    let_go_of_the_chord(&mut harness);

    // A track's name, begun with a double-click.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.double_click(harness.header(0, 80.0, 14.0));
    harness.idle(2);
    assert!(harness.app.state().renaming_track.is_some());
    assert_all_ended(&harness);
    let_go_of_the_chord(&mut harness);

    // The assistant's panel, which opens with its message field ready.
    let (mut harness, _) = playing();
    harness.app.assistant_mut().use_script(Script::ready());
    hold_a_chord(&mut harness);
    harness.key(CTRL | Modifiers::SHIFT, Key::A);
    harness.idle(3);
    assert_all_ended(&harness);
    let_go_of_the_chord(&mut harness);

    // The list of sounds, where typing is a search.
    let options = UiOptions { browser_tab: Some(BrowserTab::Sounds), ..switched_on() };
    let (mut harness, _) = with_band(options);
    hold_a_chord(&mut harness);
    harness.app.state_mut().focus = FocusArea::Browser;
    harness.idle(1);
    assert_all_ended(&harness);
}

#[test]
fn notes_stop_when_the_project_gives_way_to_another() {
    // Through the window: the question, and its answer.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.key(CTRL, Key::N);
    harness.click_label("Discard and start new");
    harness.idle(2);
    assert!(harness.project().tracks.is_empty(), "a new project is open");
    assert_all_ended(&harness);

    // From elsewhere, as an assistant does over MCP.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.session.lock().new_project();
    harness.idle(1);
    assert_all_ended(&harness);
    let_go_of_the_chord(&mut harness);

    // Another project whose tracks happen to have the same ids: the notes
    // were the old project's all the same.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.app.state_mut().project_generation += 1;
    harness.idle(1);
    assert_all_ended(&harness);
}

#[test]
fn notes_stop_when_what_they_play_is_taken_away() {
    // The track is deleted from elsewhere.
    let (mut harness, [keys, ..]) = playing();
    hold_a_chord(&mut harness);
    harness.session.lock().edit(|project| project.remove_track(keys).map(drop)).unwrap();
    harness.idle(1);
    assert_all_ended(&harness);
    let_go_of_the_chord(&mut harness);

    // Its instrument is.
    let (mut harness, [keys, ..]) = playing();
    hold_a_chord(&mut harness);
    let synth = harness.project().track(keys).unwrap().devices[0].id;
    harness.session.lock().edit(|project| project.remove_device(synth).map(drop)).unwrap();
    harness.idle(1);
    assert_all_ended(&harness);
    assert_eq!(harness.app.keyboard().playing().silence, Some(Silence::NoInstrument("Keys".to_owned())));

    // Undo takes back the edit that made the tracks.
    let (mut harness, _) = playing();
    hold_a_chord(&mut harness);
    harness.key(CTRL, Key::Z);
    assert!(harness.project().tracks.is_empty());
    assert_all_ended(&harness);
}

#[test]
fn notes_stop_when_the_program_exits() {
    let (mut harness, _) = playing();
    down(&mut harness, Key::Tab);
    hold_a_chord(&mut harness);
    up(&mut harness, Key::G);
    eframe::App::on_exit(&mut harness.app);
    assert_all_ended(&harness);
}

#[test]
fn the_sustain_key_holds_notes_until_it_comes_up() {
    let (mut harness, [keys, ..]) = playing();
    down(&mut harness, Key::Tab);
    assert!(harness.app.keyboard().sustaining());
    harness.key(NONE, Key::A);
    harness.key(NONE, Key::D);
    assert_eq!(sounding(&harness), [(keys, 60), (keys, 64)], "the keys came up and the notes ring on");
    assert_eq!(harness.app.keyboard().pedalled(), 2);

    // A note struck again while it rings is one note, begun afresh.
    let before = log(&harness).len();
    down(&mut harness, Key::A);
    assert_eq!(log(&harness)[before..], [Sent::Off(keys, 60), Sent::On(keys, 60, 100)]);
    assert_eq!(sounding(&harness).len(), 2);
    up(&mut harness, Key::A);

    // The octave can move under the pedal: what rings keeps ringing.
    harness.key(NONE, Key::X);
    harness.key(NONE, Key::A);
    assert_eq!(sounding(&harness), [(keys, 64), (keys, 60), (keys, 72)]);

    // A key still down when the pedal comes up keeps its own note.
    down(&mut harness, Key::G);
    up(&mut harness, Key::Tab);
    assert!(!harness.app.keyboard().sustaining());
    assert_eq!(sounding(&harness), [(keys, 79)], "the pedal's notes ended; the held key's did not");
    up(&mut harness, Key::G);
    assert_all_ended(&harness);

    // Tab sustains; it does not walk the keyboard focus through the window.
    assert!(harness.ctx.memory(|memory| memory.focused().is_none()));
}

#[test]
fn whatever_is_done_every_note_that_starts_also_ends() {
    let (mut harness, tracks) = playing();
    let mut seed = 0x2545_F491_4F6C_DD1D_u64;
    let mut random = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let notes = [Key::A, Key::W, Key::S, Key::D, Key::F, Key::G, Key::K, Key::L, Key::Quote];
    // The keys the player's fingers are on, whatever the window knows.
    let mut fingers: Vec<Key> = Vec::new();
    let mut pedal = false;
    for step in 0..1200 {
        match random(100) {
            0..=41 => {
                let key = notes[random(notes.len())];
                if !fingers.contains(&key) {
                    fingers.push(key);
                }
                down(&mut harness, key);
            }
            42..=71 if !fingers.is_empty() => {
                let key = fingers.remove(random(fingers.len()));
                up(&mut harness, key);
            }
            // With no finger to lift: a step of expression or of the wheel.
            42..=60 => harness.key(NONE, [Key::B, Key::N, Key::M, Key::Comma][random(4)]),
            72..=76 => harness.key(NONE, [Key::Z, Key::X][random(2)]),
            77..=80 => {
                pedal = !pedal;
                harness.step(NONE, vec![key_event(Key::Tab, pedal)]);
            }
            81..=83 => {
                let track = tracks[random(3)];
                harness.app.state_mut().select_track(track);
                harness.idle(1);
            }
            84..=85 => harness.key(CTRL, Key::K),
            86..=88 => lose_focus(&mut harness, [Lost::Seen, Lost::Hidden, Lost::FrameOnly][random(3)]),
            89..=90 => harness.key(NONE, Key::F1),
            91..=92 => {
                // The edit that made the tracks is undone, or done again.
                let mut session = harness.session.lock();
                if !session.undo() {
                    session.redo();
                }
            }
            93..=94 => {
                harness.click(device_search(&harness));
                type_letter(&mut harness, Key::A, "a");
                harness.key(NONE, Key::Escape);
            }
            95 => harness.key(NONE, [Key::C, Key::V, Key::B, Key::Comma, Key::N, Key::M][random(6)]),
            96..=97 => {
                // A track is armed or disarmed, as an assistant might:
                // the keys then play other tracks than a moment ago.
                let track = tracks[random(3)];
                let mut session = harness.session.lock();
                let armed = session.is_armed(track);
                let _ = session.set_armed(track, !armed);
                drop(session);
                harness.idle(1);
            }
            _ => harness.idle(1 + random(3)),
        }
        // Now and then the player puts things right and plays on, or most
        // of the run would be spent switched off behind a dialog.
        if random(100) < 25 {
            if harness.app.state().shortcuts_open {
                harness.key(NONE, Key::F1);
            }
            if harness.project().tracks.is_empty() {
                harness.session.lock().redo();
                harness.idle(1);
            }
            if !harness.app.keyboard().is_on() {
                harness.key(CTRL, Key::K);
            }
        }

        // What the log says is still sounding is exactly what the keyboard
        // has written down to stop.
        let keyboard = harness.app.keyboard();
        let (mut open, mut known) = (unended(&keyboard.sent), keyboard.sounding());
        open.sort_by_key(|(track, pitch)| (track.0, *pitch));
        known.sort_by_key(|(track, pitch)| (track.0, *pitch));
        assert_eq!(open, known, "step {step}");
        // No note is held by a key no finger is on, and only the pedal
        // holds a note without a key.
        for key in keyboard.keys_down() {
            assert!(fingers.contains(&key), "step {step}: {key:?} holds a note and is not down");
        }
        assert!(keyboard.pedalled() == 0 || keyboard.sustaining(), "step {step}: a note is held by nothing");
        if !keyboard.is_on() {
            assert!(known.is_empty(), "step {step}: switched off with notes sounding");
        }
        let tracks = &keyboard.playing().tracks;
        assert!(known.iter().all(|(on, _)| tracks.contains(on)), "step {step}: a note sounds on a track not played");
        // And the same care for controllers: every one the keyboard moved
        // is where it holds it, on a track it plays, or back at rest.
        assert_controllers_accounted_for(&harness, &format!("step {step}"));
        let held = keyboard.told();
        assert!(
            held.iter().all(|(on, ..)| tracks.contains(on)),
            "step {step}: a controller is held on a track not played"
        );
    }

    // The player takes their hands away.
    for key in fingers.drain(..) {
        up(&mut harness, key);
    }
    up(&mut harness, Key::Tab);
    assert_all_ended(&harness);
    let started = log(&harness).iter().filter(|message| matches!(message, Sent::On(..))).count();
    assert!(started > 150, "the run did play: {started} notes");
    // And switch the keyboard off: nothing is left where it was put.
    if harness.app.keyboard().is_on() {
        harness.key(CTRL, Key::K);
    }
    assert_all_given_back(&harness);
    let moved = controls(&harness).len();
    assert!(moved > 60, "the run did move controllers: {moved} messages");
}

// ---- expression and the mod wheel ---------------------------------------------------

#[test]
fn a_track_is_given_expression_before_the_first_note_typed_on_it() {
    let (mut harness, [keys, bass, _]) = playing();
    assert_eq!(controls(&harness), [], "switched on, the keyboard has said nothing yet");
    assert_eq!((harness.app.keyboard().expression(), harness.app.keyboard().mod_wheel()), (127, 0));

    // The first note: the track is told the keyboard's expression (full)
    // and where its wheel is (at rest), so that an instrument which waits
    // for breath sounds when it is simply typed on.
    down(&mut harness, Key::A);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 127), (keys, MOD_WHEEL, 0)]);
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100)]);
    up(&mut harness, Key::A);
    harness.key(NONE, Key::D);
    harness.key(NONE, Key::G);
    assert_eq!(controls(&harness).len(), 2, "once: the track has them from then on");

    // Another track is told when it is first played, and the one left
    // behind is owed nothing: it was at full and at rest.
    harness.click(harness.header(1, 120.0, 14.0));
    assert_eq!(controls(&harness).len(), 2);
    assert_eq!(harness.app.keyboard().told(), []);
    harness.key(NONE, Key::A);
    assert_eq!(controls(&harness)[2..], [(bass, EXPRESSION, 127), (bass, MOD_WHEEL, 0)]);

    // Back on the first, it is told again: something else may have moved
    // its controllers since.
    harness.click(harness.header(0, 120.0, 14.0));
    harness.key(NONE, Key::A);
    assert_eq!(controls(&harness)[4..], [(keys, EXPRESSION, 127), (keys, MOD_WHEEL, 0)]);
}

#[test]
fn b_and_n_step_expression_and_m_and_comma_the_mod_wheel() {
    let (mut harness, [keys, ..]) = playing();
    // Before a note is played they are set and shown, and nothing is sent.
    harness.key(NONE, Key::B);
    assert_eq!(harness.app.keyboard().expression(), 120);
    assert!(harness.is_on_screen("Expression 120") && harness.is_on_screen("Mod wheel 0"), "{:?}", harness.labels);
    assert_eq!(controls(&harness), []);
    assert_eq!(harness.app.state().arrangement.tool, Tool::Pointer, "B is no pencil while the keyboard plays");

    // The first note brings what they are by now.
    down(&mut harness, Key::A);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 120), (keys, MOD_WHEEL, 0)]);
    forget_controls(&mut harness);

    // Under a held note every step is heard at once.
    harness.key(NONE, Key::B);
    harness.key(NONE, Key::B);
    harness.key(NONE, Key::N);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 110), (keys, EXPRESSION, 100), (keys, EXPRESSION, 110)]);
    forget_controls(&mut harness);
    harness.key(NONE, Key::Comma);
    harness.key(NONE, Key::Comma);
    harness.key(NONE, Key::M);
    assert_eq!(controls(&harness), [(keys, MOD_WHEEL, 10), (keys, MOD_WHEEL, 20), (keys, MOD_WHEEL, 10)]);
    assert!(harness.project().markers.is_empty(), "M adds no marker while the keyboard plays");
    assert_eq!(sounding(&harness), [(keys, 60)], "the note goes on under them");
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100)]);

    // A key held down keeps stepping, as the velocity's does.
    forget_controls(&mut harness);
    let repeat = Event::Key { key: Key::B, physical_key: None, pressed: true, repeat: true, modifiers: NONE };
    harness.step(NONE, vec![key_event(Key::B, true)]);
    harness.step(NONE, vec![repeat.clone()]);
    harness.step(NONE, vec![repeat]);
    up(&mut harness, Key::B);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 100), (keys, EXPRESSION, 90), (keys, EXPRESSION, 80)]);

    // The strip's own keys do the same, and it shows where both are.
    forget_controls(&mut harness);
    harness.click_label("More expression");
    harness.click_label("Mod wheel up");
    harness.click_label("Mod wheel down");
    harness.click_label("Less expression");
    harness.click_label("Less expression");
    assert_eq!(
        controls(&harness),
        [
            (keys, EXPRESSION, 90),
            (keys, MOD_WHEEL, 20),
            (keys, MOD_WHEEL, 10),
            (keys, EXPRESSION, 80),
            (keys, EXPRESSION, 70)
        ]
    );
    harness.idle(1);
    assert!(harness.is_on_screen("Expression 70") && harness.is_on_screen("Mod wheel 10"), "{:?}", harness.labels);
    harness.move_to(harness.labeled("Less expression") + vec2(30.0, 0.0));
    harness.idle(2);
    assert!(harness.hint().contains("B gives less and N more"), "{}", harness.hint());

    // At nothing, the strip says why nothing sounds.
    for _ in 0..8 {
        harness.key(NONE, Key::B);
    }
    assert_eq!(harness.app.keyboard().expression(), 0);
    harness.move_to(harness.labeled("Less expression") + vec2(30.0, 0.0));
    harness.idle(2);
    assert_eq!(harness.hint(), "Expression is at 0: nothing sounds \u{00B7} N gives more");

    // With a modifier held the letters are not the keyboard's.
    forget_controls(&mut harness);
    harness.key(CTRL, Key::B);
    harness.key(Modifiers::SHIFT, Key::N);
    assert_eq!(controls(&harness), []);
    assert_eq!(harness.app.keyboard().expression(), 0);
}

#[test]
fn controllers_are_given_back_when_the_switch_goes_off() {
    // By its shortcut.
    let (mut harness, [keys, ..]) = playing();
    play_with_controllers(&mut harness, keys);
    harness.key(CTRL, Key::K);
    assert!(!harness.app.keyboard().is_on());
    assert_all_ended(&harness);
    assert_all_given_back(&harness);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 127), (keys, MOD_WHEEL, 0)], "full again, and at rest");
    let_go_of_the_chord(&mut harness);

    // By its button.
    let (mut harness, [keys, ..]) = playing();
    play_with_controllers(&mut harness, keys);
    harness.click_label("Computer keyboard");
    assert_all_given_back(&harness);
    let_go_of_the_chord(&mut harness);

    // Switched on again, the keyboard is as it was left, and a track
    // hears of it with the next note, not before.
    forget_controls(&mut harness);
    harness.click_label("Computer keyboard");
    harness.idle(1);
    assert_eq!((harness.app.keyboard().expression(), harness.app.keyboard().mod_wheel()), (100, 20));
    assert_eq!(controls(&harness), []);
    down(&mut harness, Key::A);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 100), (keys, MOD_WHEEL, 20)]);

    // A keyboard at full and at rest owes nothing, and sends nothing.
    let (mut harness, [keys, ..]) = playing();
    hold_a_chord(&mut harness);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 127), (keys, MOD_WHEEL, 0)]);
    harness.key(CTRL, Key::K);
    assert_all_given_back(&harness);
    assert_eq!(controls(&harness).len(), 2, "nothing was anywhere but where it belongs");
}

#[test]
fn controllers_are_given_back_when_another_track_is_selected() {
    let (mut harness, [keys, bass, empty]) = playing();
    play_with_controllers(&mut harness, keys);
    harness.click(harness.header(1, 120.0, 14.0));
    assert_eq!(harness.app.state().selected_track, Some(bass));
    assert_all_ended(&harness);
    assert_all_given_back(&harness);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 127), (keys, MOD_WHEEL, 0)], "on the track they were moved on");
    let_go_of_the_chord(&mut harness);

    // What is played next is played there, with the keyboard as it is.
    forget_controls(&mut harness);
    down(&mut harness, Key::A);
    assert_eq!(controls(&harness), [(bass, EXPRESSION, 100), (bass, MOD_WHEEL, 20)]);

    // The selection moved by something other than a click, to a track
    // with nothing to play.
    harness.app.state_mut().select_track(empty);
    harness.idle(1);
    assert_all_given_back(&harness);

    // Armed from elsewhere, under held keys: the keys play that track now.
    let (mut harness, [keys, bass, _]) = playing();
    play_with_controllers(&mut harness, keys);
    harness.session.lock().set_armed(bass, true).unwrap();
    harness.idle(1);
    assert_all_ended(&harness);
    assert_all_given_back(&harness);
}

#[test]
fn controllers_are_given_back_when_the_window_is_no_longer_typed_into() {
    for how in [Lost::Seen, Lost::Hidden, Lost::FrameOnly] {
        let (mut harness, [keys, ..]) = playing();
        play_with_controllers(&mut harness, keys);
        lose_focus(&mut harness, how);
        assert_all_ended(&harness);
        assert_all_given_back(&harness);
        assert_eq!(controls(&harness), [(keys, EXPRESSION, 127), (keys, MOD_WHEEL, 0)], "{how:?}");
        // Said once, however long the window stays away.
        lose_focus(&mut harness, how);
        lose_focus(&mut harness, Lost::Hidden);
        assert_eq!(controls(&harness).len(), 2, "{how:?}");

        // Typed into again, the next note brings the keyboard's own back.
        forget_controls(&mut harness);
        harness.step(NONE, vec![Event::WindowFocused(true)]);
        let_go_of_the_chord(&mut harness);
        assert_eq!(controls(&harness), [], "{how:?}");
        down(&mut harness, Key::S);
        assert_eq!(controls(&harness), [(keys, EXPRESSION, 100), (keys, MOD_WHEEL, 20)], "{how:?}");
    }
}

#[test]
fn controllers_are_given_back_when_something_else_takes_the_keyboard() {
    // A search field.
    let (mut harness, [keys, ..]) = playing();
    play_with_controllers(&mut harness, keys);
    harness.click(device_search(&harness));
    assert_all_given_back(&harness);
    // What is typed there steps nothing.
    type_letter(&mut harness, Key::B, "b");
    type_letter(&mut harness, Key::M, "m");
    assert_eq!((harness.app.keyboard().expression(), harness.app.keyboard().mod_wheel()), (100, 20));

    // A dialog.
    let (mut harness, [keys, ..]) = playing();
    play_with_controllers(&mut harness, keys);
    harness.key(NONE, Key::F1);
    assert!(harness.app.state().shortcuts_open);
    assert_all_given_back(&harness);

    // Another project, from the window and from elsewhere.
    let (mut harness, [keys, ..]) = playing();
    play_with_controllers(&mut harness, keys);
    harness.session.lock().new_project();
    harness.idle(1);
    assert_all_given_back(&harness);

    // The track goes, or its instrument does.
    let (mut harness, [keys, ..]) = playing();
    play_with_controllers(&mut harness, keys);
    let synth = harness.project().track(keys).unwrap().devices[0].id;
    harness.session.lock().edit(|project| project.remove_device(synth).map(drop)).unwrap();
    harness.idle(1);
    assert_all_given_back(&harness);

    // The program exits.
    let (mut harness, [keys, ..]) = playing();
    play_with_controllers(&mut harness, keys);
    eframe::App::on_exit(&mut harness.app);
    assert_all_ended(&harness);
    assert_all_given_back(&harness);
}

#[test]
fn a_note_asked_for_with_the_mouse_is_given_expression_too() {
    let (mut harness, [keys, ..]) = playing();
    let picture = harness.all_labeled("Computer keyboard keys")[0];
    let first = pos2(picture.left() + 0.5 * picture.width() / 11.0, picture.bottom() - 4.0);
    harness.move_to(first);
    harness.button(first, PointerButton::Primary, true);
    harness.idle(1);
    assert_eq!(controls(&harness), [(keys, EXPRESSION, 127), (keys, MOD_WHEEL, 0)]);
    harness.button(first, PointerButton::Primary, false);
    harness.idle(2);

    // Switched off, the mouse's notes are plain notes: the keyboard has
    // no part in them and moves no controller.
    let (mut harness, _, _, _) = with_two_tracks(UiOptions::default());
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    let key = pos2(harness.note_cell(0.0, 60).x - 20.0, harness.note_cell(0.0, 60).y);
    harness.button(key, PointerButton::Primary, true);
    harness.idle(1);
    assert!(harness.app.state().audition.is_some(), "the piano roll's key sounds");
    assert_eq!(controls(&harness), []);
    harness.button(key, PointerButton::Primary, false);
}

// ---- the player can see what they are doing ----------------------------------------

#[test]
fn the_strip_says_what_plays_and_why_nothing_sounds() {
    let (mut harness, [keys, bass, empty]) = playing();
    let playing = |harness: &Harness| harness.app.keyboard().playing().clone();
    assert_eq!(playing(&harness).silence, None);
    assert_eq!((playing(&harness).name.as_str(), playing(&harness).instrument.as_str()), ("Keys", "Poly Synth"));

    // A track with nothing to play: the keys are still notes, not
    // shortcuts, and the strip says why none is heard.
    harness.click(harness.header(2, 120.0, 14.0));
    assert_eq!(harness.app.state().selected_track, Some(empty));
    harness.idle(1);
    assert_eq!(playing(&harness).silence, Some(Silence::NoInstrument("Empty".to_owned())));
    harness.key(NONE, Key::A);
    assert_eq!(log(&harness), []);
    assert!(harness.app.state().arrangement.lanes.is_empty(), "A is not a shortcut again for being silent");
    harness.move_to(harness.labeled("Octave down") - vec2(60.0, 0.0));
    harness.idle(2);
    assert_eq!(harness.hint(), "Empty has no instrument \u{00B7} add one from the browser");

    // Muted, and silenced by another track's solo: played all the same,
    // so that it sounds the moment it is let through.
    harness.click(harness.header(0, 120.0, 14.0));
    harness.click_label("Mute Keys");
    harness.idle(1);
    assert_eq!(playing(&harness).silence, Some(Silence::Muted("Keys".to_owned())));
    harness.key(NONE, Key::A);
    assert_eq!(log(&harness), [Sent::On(keys, 60, 100), Sent::Off(keys, 60)]);
    harness.click_label("Mute Keys");
    harness.click_label("Solo Bass");
    harness.click(harness.header(0, 120.0, 14.0));
    harness.idle(1);
    assert_eq!(playing(&harness).silence, Some(Silence::Soloed("Keys".to_owned())));
    harness.click(harness.header(1, 120.0, 14.0));
    harness.idle(1);
    assert_eq!((playing(&harness).tracks, playing(&harness).silence), (vec![bass], None));

    // A group mixes other tracks and plays no notes of its own.
    let group = harness.session.lock().edit(|project| Ok(project.add_track_of_kind("Bus", TrackKind::Group))).unwrap();
    harness.app.state_mut().select_track(group);
    harness.idle(1);
    assert_eq!(playing(&harness).silence, Some(Silence::Group("Bus".to_owned())));

    // No track at all.
    let mut harness = Harness::new(switched_on(), |_| {});
    assert_eq!(harness.app.keyboard().playing().silence, Some(Silence::NoTrack));
    harness.key(NONE, Key::A);
    assert_eq!(log(&harness), []);
}

#[test]
fn the_strip_shows_the_octave_and_velocity_and_its_keys_can_be_clicked() {
    let (mut harness, _) = playing();
    harness.click_label("Octave up");
    assert_eq!(harness.app.keyboard().lowest(), 72);
    harness.click_label("Octave down");
    harness.click_label("Octave down");
    assert_eq!(harness.app.keyboard().lowest(), 48);
    harness.click_label("Louder");
    assert_eq!(harness.app.keyboard().velocity(), 110);
    harness.click_label("Softer");
    harness.click_label("Softer");
    assert_eq!(harness.app.keyboard().velocity(), 90);
    harness.move_to(harness.labeled("Octave down"));
    harness.idle(2);
    assert!(harness.hint().contains("C3 \u{2013} F4"), "{}", harness.hint());

    // The picture of the keys folds away and comes back.
    assert!(harness.is_on_screen("Computer keyboard keys"));
    harness.click_label("Hide the keys");
    harness.idle(2);
    assert!(!harness.app.keyboard().shows_key_map());
    assert!(!harness.is_on_screen("Computer keyboard keys") && harness.is_on_screen("Octave up"));
    harness.click_label("Show the keys");
    harness.idle(2);
    assert!(harness.is_on_screen("Computer keyboard keys"));
}

#[test]
fn the_picture_of_the_keys_shows_what_is_held_and_plays_under_the_mouse() {
    let (mut harness, [keys, ..]) = playing();
    // What the picture lights is what the keyboard says is sounding, by
    // the letter that plays it.
    down(&mut harness, Key::A);
    down(&mut harness, Key::T);
    let keyboard = harness.app.keyboard();
    assert!(keyboard.is_sounding(keys, 60) && keyboard.is_sounding(keys, 66) && !keyboard.is_sounding(keys, 62));
    assert_eq!(keyboard.pitches(), [60, 66]);
    assert_eq!((keyboard.letter(60), keyboard.letter(66), keyboard.letter(59)), (Some("A"), Some("T"), None));
    up(&mut harness, Key::A);
    up(&mut harness, Key::T);

    // Pressed with the mouse, a key of the picture sounds while held, and
    // sliding along them plays each.
    let picture = harness.all_labeled("Computer keyboard keys")[0];
    let white = |index: f32| pos2(picture.left() + (index + 0.5) * picture.width() / 11.0, picture.bottom() - 4.0);
    let held = |harness: &Harness| harness.app.state().audition.map(|audition| (audition.track, audition.pitch));
    harness.move_to(white(0.0));
    harness.idle(2);
    assert_eq!(harness.hint(), "A plays C4 \u{00B7} or press it here");
    harness.button(white(0.0), PointerButton::Primary, true);
    harness.idle(1);
    assert_eq!(held(&harness), Some((keys, 60)));
    harness.move_to(white(2.0));
    harness.idle(1);
    assert_eq!(held(&harness), Some((keys, 64)), "the third white key is the E");
    // High on the join of two white keys is the black key between them.
    let c_sharp = pos2(picture.left() + picture.width() / 11.0, picture.top() + 4.0);
    harness.move_to(c_sharp);
    harness.idle(1);
    assert_eq!(held(&harness), Some((keys, 61)));
    harness.button(c_sharp, PointerButton::Primary, false);
    harness.idle(2);
    assert_eq!(held(&harness), None, "releasing the mouse releases the note");
    assert_eq!(log(&harness).len(), 4, "the mouse's notes are not the keyboard's");
}

#[test]
fn the_piano_roll_follows_the_octave() {
    let (mut harness, _, _, _) = with_two_tracks(switched_on());
    harness.click(harness.lane(10.0, 0));
    harness.idle(2);
    let grid_top = harness.detail_top() + detail::TAB_BAR + piano_roll::RULER_HEIGHT;
    let grid_bottom = SCREEN.y - status_bar::HEIGHT - piano_roll::VELOCITY_LANE;
    let in_view = |harness: &Harness, pitch: u8| {
        let row = harness.note_cell(0.0, pitch).y;
        row > grid_top && row < grid_bottom
    };
    let framed = harness.app.state().piano.pitch;
    assert!(!in_view(&harness, 77), "the clip was framed on its one note, below the top of the keyboard's range");

    // Switching the keyboard off and on moves nothing: the view stays on
    // the notes being looked at.
    harness.key(CTRL, Key::K);
    harness.key(CTRL, Key::K);
    harness.idle(2);
    assert!(harness.app.keyboard().is_on());
    assert_eq!(harness.app.state().piano.pitch, framed);

    // Moving the keys is asking to see them. An octave up, they are
    // brought into view by as little as it takes; two octaves down from
    // the start, the same.
    harness.key(NONE, Key::X);
    harness.idle(2);
    assert!(in_view(&harness, 72) && in_view(&harness, 89), "{:?}", harness.app.state().piano.pitch);
    harness.key(NONE, Key::Z);
    harness.key(NONE, Key::Z);
    harness.key(NONE, Key::Z);
    harness.idle(2);
    let (low, high) = (harness.app.keyboard().lowest(), harness.app.keyboard().highest());
    assert_eq!((low, high), (36, 53));
    assert!(in_view(&harness, low) && in_view(&harness, high), "{:?}", harness.app.state().piano.pitch);
    assert_ne!(harness.app.state().piano.pitch, framed);
    // The roll's own keys still play under the mouse.
    let key = harness.note_cell(0.0, 40) - vec2(piano_roll::KEYS_WIDTH / 2.0, 0.0);
    harness.move_to(key);
    harness.button(key, PointerButton::Primary, true);
    harness.idle(1);
    assert_eq!(harness.app.state().audition.map(|audition| audition.pitch), Some(40));
    harness.button(key, PointerButton::Primary, false);
    harness.idle(2);
    assert!(harness.app.state().audition.is_none());

    // An octave change while another tab is showing does not move the
    // roll behind its back.
    let before = harness.app.state().piano.pitch;
    harness.click_label("Mixer");
    harness.key(NONE, Key::X);
    harness.key(NONE, Key::X);
    harness.key(NONE, Key::X);
    harness.idle(5);
    harness.click_label("Clip");
    harness.idle(2);
    assert_eq!(harness.app.state().piano.pitch, before);
}
