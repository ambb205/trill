//! Recording an input of the audio device through the window: giving a
//! track its input, arming it, the take as it grows, its takes in lanes and
//! what is made of them, the round trip and its measurement, and the
//! recordings the song no longer uses.
//!
//! The devices exist only here. The input hears a tone that is worked out,
//! or what the output plays; nothing of this machine's hardware is opened,
//! least of all a microphone. The scripted device has no clock of its own,
//! so a take is as long as the test makes it; the thread that writes takes
//! does keep the machine's time, and the tests wait for it where it matters.

use super::recording::{Stage, on_stage};
use super::*;
use crate::ui::RecordingScene;
use std::f64::consts::TAU;
use trill_core::{Monitoring, TrackInput};
use trill_engine::host::scripted::{Hears, ScriptedHost, ScriptedInput};
use trill_engine::{Correction, Measurement};
use trill_runtime::Storage;
use trill_runtime::midi::scripted::ScriptedMidi;

/// Frames in a beat at the tempo and rate these songs run at.
const BEAT: usize = 24_000;

/// The window on speakers and a microphone that hears a tone, a quarter of
/// full scale on its first channel and half on its second, with one track
/// that has nothing to play. Takes are written into the folder returned.
fn studio() -> (Harness, Stage, TrackId, tempfile::TempDir) {
    let host = ScriptedHost::with_speakers();
    host.add_input(ScriptedInput::new("Microphone"));
    host.hear(Hears::Sound(Arc::new(|channel, seconds| {
        let level = if channel == 0 { 0.25 } else { 0.5 };
        (level * (seconds * TAU * 440.0).sin()) as f32
    })));
    let folder = tempfile::tempdir().unwrap();
    let mut voice = None;
    let recordings = folder.path().to_path_buf();
    let (harness, stage) = on_stage(host, ScriptedMidi::new(), UiOptions::default(), Storage::Nowhere, |session| {
        session.keep_unsaved_recordings_in(recordings);
        session.set_count_in(0);
        voice = Some(session.edit(|project| Ok(project.add_track("Voice"))).unwrap());
    });
    (harness, stage, voice.unwrap(), folder)
}

/// Advances the device by `frames` while a take is recorded, no further
/// ahead of the thread that writes it than that thread can make up.
fn roll(harness: &mut Harness, stage: &Stage, frames: usize) {
    let written = |harness: &Harness| {
        let session = harness.session.lock();
        session.growing_takes().iter().map(|take| take.file.frames()).min()
    };
    let before = written(harness).unwrap_or(0);
    let mut rolled = 0;
    while rolled < frames {
        let step = 9_600.min(frames - rolled);
        stage.run(harness, step);
        rolled += step;
        let deadline = Instant::now() + Duration::from_secs(20);
        while written(harness).is_some_and(|frames| (frames - before) as usize + 96_000 < rolled) {
            assert!(Instant::now() < deadline, "the take was never written");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// The label on screen that starts with `start`.
fn label(harness: &Harness, start: &str) -> String {
    let found = harness.labels.iter().find(|(text, _)| text.starts_with(start));
    found.unwrap_or_else(|| panic!("nothing on screen is labeled '{start}…'")).0.clone()
}

/// A point in the `lane`th lane of takes under the first track, at `beat`.
fn take_lane(harness: &Harness, beat: f64, lane: usize) -> Pos2 {
    let height = arrangement::takes::LANE_HEIGHT;
    let y = harness.row_top(0) + arrangement::TRACK_HEIGHT + lane as f32 * height + 0.6 * height;
    pos2(harness.lane(beat, 0).x, y)
}

/// The names and starts of what the track plays.
fn plays(harness: &Harness, track: TrackId) -> Vec<(String, f64)> {
    let project = harness.project();
    project.track(track).unwrap().clips.iter().map(|clip| (clip.name.clone(), clip.start)).collect()
}

#[test]
fn a_track_is_given_an_input_from_its_menu_and_arming_it_opens_the_input() {
    let (mut harness, stage, voice, _folder) = studio();
    assert!(!harness.is_on_screen("Arm Voice"), "there is nothing to record on it yet");
    assert!(!harness.is_on_screen("Input live"));

    // The track's menu says what it records.
    harness.right_click(harness.header(0, 60.0, 12.0));
    harness.click_label("Input 2");
    assert_eq!(harness.project().track(voice).unwrap().input, Some(TrackInput::mono(2)));
    harness.idle(2);
    assert!(harness.is_on_screen("Input of Voice: input 2"), "{:?}", harness.labels);
    assert!(harness.is_on_screen("Arm Voice"), "a track that records an input can be armed");
    assert!(!harness.is_on_screen("Input live"), "nothing needs the input yet, so it is closed");
    assert!(stage.host.capturing().is_none(), "and the device is not asked for it");

    // Arming it is what opens the input, and the window says so for as
    // long as it is open.
    harness.move_to(harness.labeled("Arm Voice"));
    harness.idle(2);
    assert!(harness.hint().starts_with("Arm: opens the audio input"), "{}", harness.hint());
    harness.click_label("Arm Voice");
    stage.run(&mut harness, 4_800);
    harness.idle(2);
    assert!(stage.host.capturing().is_some());
    assert!(harness.is_on_screen("Input live \u{00B7} Microphone"), "{:?}", harness.labels);
    harness.move_to(harness.labeled("Input live"));
    harness.idle(2);
    assert!(harness.hint().contains("a track that records it is armed or heard"), "{}", harness.hint());

    // What arrives is shown beside the track: the second channel, at half
    // of full scale.
    stage.run(&mut harness, 48_000);
    harness.idle(2);
    let level = label(&harness, "Input level of Voice");
    assert!(level.contains("6.0 dB") && !level.contains("clipped"), "{level}");

    // The monitor switch, and beside it how late the input is heard.
    harness.click_label("Monitor Voice");
    assert_eq!(harness.project().track(voice).unwrap().input.unwrap().monitor, Monitoring::Armed);
    harness.idle(2);
    assert!(harness.is_on("Monitor Voice"));
    harness.move_to(harness.labeled("Monitor Voice"));
    harness.idle(2);
    assert!(harness.hint().contains("ms, as the devices say, not measured"), "{}", harness.hint());
    harness.click_label("Monitor Voice");
    assert_eq!(harness.project().track(voice).unwrap().input.unwrap().monitor, Monitoring::Off);

    // The input's own menu, on the header: a pair, and heard always.
    harness.click_label("Input of Voice");
    harness.click_label("Inputs 1 and 2");
    harness.click_label("Input of Voice");
    harness.click_label("Always");
    let input = harness.project().track(voice).unwrap().input.unwrap();
    assert_eq!((input.channel, input.stereo, input.monitor), (1, true, Monitoring::Always));

    // Disarmed, a track that is always heard still needs the input; told
    // to record a keyboard again, nothing does, and it closes.
    harness.click_label("Arm Voice");
    stage.run(&mut harness, 4_800);
    harness.idle(2);
    assert!(harness.is_on_screen("Input live"), "it is heard through the track");
    harness.right_click(harness.header(0, 60.0, 12.0));
    harness.click_label("What a keyboard plays");
    assert_eq!(harness.project().track(voice).unwrap().input, None);
    stage.run(&mut harness, 4_800);
    harness.idle(2);
    assert!(!harness.is_on_screen("Input live") && stage.host.capturing().is_none());
    assert!(!harness.is_on_screen("Arm Voice"));
}

#[test]
fn the_stage_says_that_an_input_is_live_too() {
    let (mut harness, stage, voice, _folder) = studio();
    harness.session.lock().edit(|project| project.set_track_input(voice, Some(TrackInput::mono(1)))).unwrap();
    harness.idle(2);
    harness.click_label("Arm Voice");
    stage.run(&mut harness, 4_800);
    harness.idle(2);
    assert!(harness.is_on_screen("Input live \u{00B7} Microphone"));
    // The stage takes the status bar away, and not the words.
    harness.key(Modifiers::NONE, Key::F11);
    harness.idle(3);
    assert!(harness.app.state().stage_open);
    assert!(harness.is_on_screen("Input live \u{00B7} Microphone"), "{:?}", harness.labels);
    harness.key(Modifiers::NONE, Key::F11);
    harness.session.lock().set_armed(voice, false).unwrap();
    stage.run(&mut harness, 4_800);
    harness.key(Modifiers::NONE, Key::F11);
    harness.idle(3);
    assert!(!harness.is_on_screen("Input live"), "nothing is said of an input that is closed");
}

#[test]
fn a_take_grows_on_its_track_and_its_passes_unfold_as_lanes_to_choose_from() {
    let (mut harness, stage, voice, _folder) = studio();
    harness
        .session
        .lock()
        .edit(|project| {
            project.set_track_input(voice, Some(TrackInput::mono(1)))?;
            project.set_loop(true, 0.0, 4.0)
        })
        .unwrap();
    harness.idle(2);
    harness.click_label("Arm Voice");
    stage.run(&mut harness, 4_800);
    let before = harness.undo_depth();

    // Record: the take grows with what the writer has seen of it.
    harness.key(Modifiers::NONE, Key::F9);
    roll(&mut harness, &stage, 2 * BEAT);
    // The thread that writes the take keeps the machine's time.
    harness.wait_until("the take's waveform", |harness| {
        harness.app.live().input.growing.first().is_some_and(|grown| grown.columns.len() > 100)
    });
    {
        let growing = &harness.app.live().input.growing;
        assert_eq!(growing.len(), 1);
        assert_eq!((growing[0].track, growing[0].start_beat), (voice, Some(0.0)));
        assert!(growing[0].columns.len() > 100, "{} columns", growing[0].columns.len());
        let loudest = growing[0].columns.iter().fold(0.0f32, |peak, [_, high]| peak.max(*high));
        assert!((loudest - 0.25).abs() < 0.01, "the waveform is the tone's: {loudest}");
    }
    harness.move_to(harness.labeled("Stop"));
    harness.idle(2);
    // Twice round the loop and two beats more, then stop.
    roll(&mut harness, &stage, 8 * BEAT);
    harness.key(Modifiers::NONE, Key::Space);
    harness.idle(3);
    assert!(harness.app.live().take.is_none() && harness.app.live().input.growing.is_empty());

    // Each pass is a take, the track plays the last whole one, and the
    // whole of it is one undo step. With more than one take to choose
    // from, the lanes unfold.
    let project = harness.project();
    let track = project.track(voice).unwrap();
    let names: Vec<&str> = track.takes.iter().map(|take| take.clip.name.as_str()).collect();
    assert_eq!(names, ["Take 1", "Take 2", "Take 3"]);
    assert_eq!(plays(&harness, voice), [("Take 2".to_owned(), 0.0)]);
    assert_eq!(harness.undo_depth(), before + 1);
    assert!(harness.app.state().arrangement.takes_open.contains(&voice));
    assert!(harness.is_on_screen("Hear Take 1 in place") && harness.is_on_screen("Hear Take 3 in place"));
    let (first, second, third) = (track.takes[0].clip.id, track.takes[1].clip.id, track.takes[2].clip.id);

    // A drag across a take puts that stretch on the track: one step.
    harness.drag(take_lane(&harness, 1.0, 0), take_lane(&harness, 2.0, 0));
    assert_eq!(
        plays(&harness, voice),
        [("Take 2".to_owned(), 0.0), ("Take 1".to_owned(), 1.0), ("Take 2".to_owned(), 2.0)]
    );
    assert_eq!(harness.undo_depth(), before + 2);
    assert_eq!(harness.project().track(voice).unwrap().takes.len(), 3, "nothing was covered that was not kept");
    // A double-click puts the whole of one there.
    harness.settle();
    harness.double_click(take_lane(&harness, 0.5, 2));
    assert_eq!(plays(&harness, voice), [("Take 3".to_owned(), 0.0), ("Take 2".to_owned(), 2.0)]);
    harness.key(Modifiers::COMMAND, Key::Z);
    // And the button of a lane's header does the same.
    harness.click_label("Put this take on the track");
    assert_eq!(plays(&harness, voice), [("Take 1".to_owned(), 0.0)]);

    // Heard in place, without the song changing.
    let steps = harness.undo_depth();
    harness.click_label("Hear Take 3 in place");
    assert_eq!(harness.session.lock().heard_take(), Some(third));
    harness.idle(2);
    assert!(harness.is_on("Hear Take 3 in place"));
    harness.click_label("Hear Take 3 in place");
    assert_eq!(harness.session.lock().heard_take(), None);
    assert_eq!(harness.undo_depth(), steps, "hearing a take is no edit");

    // The menu of a take: a name of its own, and letting go of it, which
    // leaves its recording on disk.
    harness.right_click(take_lane(&harness, 0.5, 1));
    harness.click_label("Rename");
    harness.idle(2);
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("Keeper");
    harness.key(Modifiers::NONE, Key::Enter);
    assert_eq!(harness.project().take(second).unwrap().1.clip.name, "Keeper");
    let file = harness.project().take(first).unwrap().1.clip.audio().unwrap().path.clone();
    harness.right_click(take_lane(&harness, 0.5, 0));
    harness.click_label("Let go of the take");
    assert!(harness.project().take(first).is_err());
    assert!(std::path::Path::new(&file).is_file(), "a recording is never deleted by letting go of a take");

    // The lanes fold away from the header, and with their key.
    harness.click_label("Fold the takes of Voice away");
    assert!(!harness.app.state().arrangement.takes_open.contains(&voice));
    harness.idle(2);
    assert!(!harness.is_on_screen("Hear Keeper in place"));
    harness.key(Modifiers::NONE, Key::T);
    assert!(harness.app.state().arrangement.takes_open.contains(&voice));
}

#[test]
fn punched_in_a_take_records_only_inside_the_loops_stretch() {
    let (mut harness, stage, voice, _folder) = studio();
    harness
        .session
        .lock()
        .edit(|project| {
            project.set_track_input(voice, Some(TrackInput::mono(1)))?;
            project.set_loop(true, 2.0, 4.0)
        })
        .unwrap();
    harness.idle(2);
    harness.click_label("Arm Voice");
    stage.run(&mut harness, 4_800);

    // The switch at the transport, and its key.
    harness.click_label("Punch in");
    assert!(harness.session.lock().punch());
    harness.idle(2);
    assert!(harness.is_on("Punch in"));
    harness.key(Modifiers::NONE, Key::P);
    assert!(!harness.session.lock().punch());
    harness.key(Modifiers::NONE, Key::P);
    harness.move_to(harness.labeled("Punch in"));
    harness.idle(2);
    assert!(
        harness.hint().starts_with("Punched in: a take records only from bar 1 beat 3 to bar 2,"),
        "{}",
        harness.hint()
    );

    harness.key(Modifiers::NONE, Key::F9);
    roll(&mut harness, &stage, 6 * BEAT);
    harness.key(Modifiers::NONE, Key::Space);
    harness.idle(3);
    // The song played through, and the take is the stretch alone.
    let project = harness.project();
    let track = project.track(voice).unwrap();
    assert_eq!(track.takes.len(), 1);
    let clip = &track.clips[0];
    assert!((clip.start - 2.0).abs() < 1e-6 && (clip.end() - 4.0).abs() < 1e-3, "{} to {}", clip.start, clip.end());
}

#[test]
fn the_panel_shows_the_inputs_level_only_when_asked_and_measures_the_round_trip() {
    let folder = tempfile::tempdir().unwrap();
    let host = ScriptedHost::with_speakers();
    // More lies between the microphone and its buffers than the input says.
    host.add_input(ScriptedInput { unsaid: 240, ..ScriptedInput::new("Microphone") });
    // A cable from the output to the input.
    host.hear(Hears::Output { gain: 0.5 });
    let storage = Storage::At { settings: folder.path().join("settings.json"), library_index: None };
    let (mut harness, stage) = on_stage(host, ScriptedMidi::new(), UiOptions::default(), storage, |session| {
        session.edit(|project| Ok(project.add_track("Voice"))).unwrap();
    });

    // The panel does not open the input by being looked at.
    harness.key(Modifiers::COMMAND, Key::Comma);
    harness.settle_work();
    harness.idle(2);
    assert!(harness.is_on_screen("Input: System default \u{00B7} Microphone"), "{:?}", harness.labels);
    harness.click_label("Input and round trip");
    stage.run(&mut harness, 4_800);
    harness.idle(2);
    assert!(stage.host.capturing().is_none() && !harness.is_on_screen("Input live"));
    assert!(label(&harness, "Input: ").contains("The input is closed"));

    // Its switch opens it and shows what arrives; the round trip is then
    // known, as the devices say it.
    harness.click_label("Show the input\u{2019}s level");
    stage.run(&mut harness, 48_000);
    harness.idle(2);
    assert!(stage.host.capturing().is_some());
    assert!(harness.is_on_screen("Input live \u{00B7} Microphone"));
    assert!(label(&harness, "Input: ").starts_with("Input: Microphone \u{00B7} 2 channels \u{00B7} 48 kHz"));
    assert!(harness.is_on_screen("Input level: 1 at"), "{:?}", harness.labels);
    let said = label(&harness, "Round trip: ");
    assert!(said.ends_with("as the devices say, not measured"), "{said}");
    let said_ms = harness.app.live().input.latency.unwrap().round_trip_ms;

    // The measurement: a second of noise, listened for.
    stage.run(&mut harness, 5 * 48_000);
    harness.click_label("Measure the round trip");
    for _ in 0..60 {
        stage.run(&mut harness, 24_000);
        if !matches!(harness.session.lock().measurement(), Measurement::Running) {
            break;
        }
    }
    harness.idle(2);
    let Measurement::Done(measured) = harness.session.lock().measurement() else {
        panic!("the measurement found nothing: {:?}", harness.session.lock().measurement());
    };
    assert!((measured.more_than_said_ms - 5.0).abs() < 0.1, "240 frames went unsaid: {measured:?}");
    let found = label(&harness, "The sound took");
    assert!(found.contains("5.0 ms more than the devices say") && found.contains("clearly"), "{found}");
    // Nothing changes until it is taken.
    assert_eq!(harness.app.live().input.latency.unwrap().correction, Correction::None);
    harness.click_label("Use what was measured");
    harness.settle_work();
    harness.idle(3);
    let latency = harness.app.live().input.latency.unwrap();
    assert_eq!(latency.correction, Correction::Measured);
    assert!((latency.round_trip_ms - said_ms - 5.0).abs() < 0.1, "{latency:?}");
    assert!(label(&harness, "Round trip: ").ends_with("as measured"));
    // It is kept for this pair of devices.
    let settings = std::fs::read_to_string(folder.path().join("settings.json")).unwrap();
    assert!(settings.contains("round_trips") && settings.contains("Microphone"), "{settings}");

    // By hand, half a millisecond at a time; and back to what they say.
    harness.click_label("Put takes earlier");
    harness.settle_work();
    harness.idle(3);
    let nudged = harness.app.live().input.latency.unwrap();
    assert!((nudged.adjust_ms - 0.5).abs() < 1e-9 && (nudged.round_trip_ms - latency.round_trip_ms - 0.5).abs() < 0.01);
    harness.click_label("Take off the nudge");
    harness.settle_work();
    harness.idle(3);
    harness.click_label("Forget the measurement");
    harness.settle_work();
    harness.idle(3);
    let back = harness.app.live().input.latency.unwrap();
    assert_eq!((back.correction, back.adjust_ms), (Correction::None, 0.0));

    // The panel goes, and with it the only thing that needed the input.
    harness.key(Modifiers::NONE, Key::Escape);
    stage.run(&mut harness, 4_800);
    harness.idle(2);
    assert!(!harness.app.settings().is_open());
    assert!(stage.host.capturing().is_none() && !harness.is_on_screen("Input live"));
}

#[test]
fn unused_recordings_are_listed_and_go_only_when_asked_twice_and_never_out_of_existence() {
    let (mut harness, stage, voice, folder) = studio();
    harness.session.lock().edit(|project| project.set_track_input(voice, Some(TrackInput::mono(1)))).unwrap();
    harness.idle(2);
    harness.click_label("Arm Voice");
    stage.run(&mut harness, 4_800);
    harness.key(Modifiers::NONE, Key::F9);
    roll(&mut harness, &stage, 2 * BEAT);
    harness.key(Modifiers::NONE, Key::Space);
    harness.idle(3);
    let file = harness.project().track(voice).unwrap().clips[0].audio().unwrap().path.clone();
    let file = std::path::PathBuf::from(file);
    assert!(file.starts_with(folder.path()) && file.is_file());

    // The list is made by a worker, which reads the songs Trill knows of.
    fn open_list(harness: &mut Harness) {
        harness.click_label("File");
        harness.click_label("Unused recordings");
        harness.settle_work();
        harness.idle(3);
    }

    // While undo can reach the take, its recording is in use.
    open_list(&mut harness);
    assert!(harness.is_on_screen("No recording of this song can go."), "{:?}", harness.labels);
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(harness.app.state().unused_recordings.is_none());

    // The take undone and something else done since: nothing can bring it
    // back, and the recording is listed with its size.
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.project().track(voice).unwrap().takes.is_empty());
    harness.session.lock().edit(|project| Ok(project.add_track("Other"))).unwrap();
    harness.idle(2);
    open_list(&mut harness);
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    assert!(harness.is_on_screen(&name), "{:?}", harness.labels);
    // Asked once, it asks back, and says where the recording would go: a
    // run that keeps nothing of the user's puts nothing in their bin.
    let removed = file.parent().unwrap().join("removed");
    harness.click_label("Remove\u{2026}");
    harness.idle(2);
    assert!(file.is_file(), "nothing goes before the question is answered");
    let question = label(&harness, "Remove this recording?");
    assert!(question.contains(&format!("It goes to {}", removed.display())), "{question}");
    assert!(harness.is_on_screen("Remove them") && harness.is_on_screen("Keep them"));
    // Told to keep it, it does.
    harness.click_label("Keep them");
    assert!(file.is_file() && harness.app.state().unused_recordings.is_none());
    // Asked twice, it goes: out of the song's folder, and not out of
    // existence.
    let held = std::fs::metadata(&file).unwrap().len();
    open_list(&mut harness);
    harness.click_label("Remove\u{2026}");
    harness.click_label("Remove them");
    harness.settle_work();
    harness.idle(3);
    assert!(!file.exists());
    assert_eq!(std::fs::metadata(removed.join(&name)).unwrap().len(), held, "what it held is all there");
    assert!(harness.app.state().unused_recordings.is_none());
    let said: Vec<String> = harness.app.state().toasts.texts().iter().map(|text| text.to_string()).collect();
    let told = format!("to {}", removed.display());
    assert!(said.iter().any(|text| text.starts_with("Removed 1 recording") && text.ends_with(&told)), "{said:?}");
}

#[test]
fn a_made_up_session_shows_its_takes_and_a_level_that_is_the_same_on_every_frame() {
    let (parts, staging) = RecordingScene::Takes.open();
    let folder = tempfile::tempdir().unwrap();
    let options = RuntimeOptions { storage: Storage::Nowhere, ..RuntimeOptions::hermetic() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    runtime.session.lock().keep_unsaved_recordings_in(folder.path().to_path_buf());
    let voice = staging.play(&runtime).expect("the scene plays out");
    let options = UiOptions { recording_scene: Some(RecordingScene::Takes), ..Default::default() };
    let mut harness = Harness::on(runtime, options, |_| {});

    let project = harness.project();
    let track = project.track(voice).unwrap();
    assert_eq!(track.takes.len(), 3, "three passes of the loop, and no sliver of a fourth");
    let names: Vec<&str> = track.clips.iter().map(|clip| clip.name.as_str()).collect();
    assert_eq!(names, ["Take 3", "Take 1", "Take 3"], "a stretch of the first in the last");
    assert!(harness.app.state().arrangement.takes_open.contains(&voice));
    assert!(harness.is_on_screen("Hear Take 2 in place"));
    assert!(harness.is_on_screen("Input live \u{00B7} Studio Interface"));
    let level = label(&harness, "Input level of Voice");
    harness.idle(30);
    assert_eq!(label(&harness, "Input level of Voice"), level, "a picture is of no particular moment");
    // The monitor switch says how late the input is heard.
    assert!(harness.is_on("Monitor Voice"));
    let late = harness.app.live().input.latency.unwrap().round_trip_ms;
    assert!((late - 49.5).abs() < 0.6, "{late}");
}

#[test]
fn an_input_the_song_says_is_always_heard_is_held_until_its_switch_is_clicked() {
    let (mut harness, stage, voice, _folder) = studio();
    // As a song that was just opened has it, or one the assistant wrote
    // to: nobody at this window switched it on. (What the test asks of
    // the session itself is asked by nobody's hand.)
    let always = TrackInput { channel: 1, stereo: false, monitor: Monitoring::Always };
    harness.session.lock().edit(|project| project.set_track_input(voice, Some(always))).unwrap();
    stage.run(&mut harness, 4_800);
    harness.idle(2);
    assert!(harness.session.lock().input_held(voice));
    assert!(harness.is_on_screen("Monitor Voice, held"), "{:?}", harness.labels);
    assert!(!harness.is_on_screen("Input live") && stage.host.capturing().is_none());
    assert!(stage.host.input_requests().is_empty(), "the device was never asked for its input");
    let said: Vec<String> = harness.app.state().toasts.texts().iter().map(|text| text.to_string()).collect();
    assert!(said.iter().any(|text| text.contains("is held, not heard")), "{said:?}");
    harness.move_to(harness.labeled("Monitor Voice, held"));
    harness.idle(2);
    assert!(
        harness.hint().starts_with("Held: the song says input 1 is heard through this track always"),
        "{}",
        harness.hint()
    );

    // One click lets it go: that is the user's hand.
    harness.click_label("Monitor Voice, held");
    stage.run(&mut harness, 4_800);
    harness.idle(2);
    assert!(!harness.session.lock().input_held(voice));
    assert!(harness.is_on("Monitor Voice"));
    assert!(harness.is_on_screen("Input live \u{00B7} Microphone") && stage.host.capturing().is_some());
    assert_eq!(harness.project().track(voice).unwrap().input, Some(always), "the song was not changed by it");

    // Arming is not asked of the session by anybody but the window's own
    // hand: asked otherwise, it is refused and nothing is armed.
    let other = harness.session.lock().edit(|project| Ok(project.add_track("Other"))).unwrap();
    harness.session.lock().edit(|project| project.set_track_input(other, Some(TrackInput::mono(2)))).unwrap();
    let refused = harness.session.lock().set_armed(other, true).unwrap_err().to_string();
    assert!(refused.contains("arming it opens the microphone"), "{refused}");
    harness.idle(2);
    harness.click_label("Arm Other");
    assert!(harness.session.lock().is_armed(other), "the arm button is the user's hand");
}
