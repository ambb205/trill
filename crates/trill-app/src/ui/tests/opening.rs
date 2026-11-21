//! A song that is opened: what the window says while its file is read,
//! and while its devices arrive.
//!
//! The devices that take time are the engine's scripted ones, told to take
//! as long to make as a plugin does. A song of three of them and three
//! built-in devices is written to a file by a runtime that makes everything
//! where it stands, and opened in a window whose devices take their time.
//! What a moment of an opening looks like is also held on made-up ones
//! ([`OpeningScene`]), which do not depend on how fast anything is.

use super::*;
use crate::ui::OpeningScene;
use std::path::{Path, PathBuf};
use trill_engine::scripted::{AMP, BASS, DRUMS, Loading, ScriptedFactory};
use trill_runtime::Parts;

/// How long each scripted device takes to make: long enough that a window
/// has drawn the song many times before the first of them is there.
const MAKING: Duration = Duration::from_millis(700);

fn slow() -> ScriptedFactory {
    ScriptedFactory::loading(Loading { making: MAKING, ..Loading::default() })
}

/// A runtime that has `scripted`'s devices beside the built-in ones.
fn runtime(scripted: &ScriptedFactory) -> Arc<Runtime> {
    let parts = Parts { devices: vec![Box::new(scripted.clone())], ..Parts::default() };
    Arc::new(Runtime::start_custom(RuntimeOptions::hermetic(), parts).unwrap())
}

/// A bass through an amplifier, drums, and a built-in synth through a
/// reverb, with the limiter a song has on its master: three devices that
/// take time to make and three that take none. Written to `file` by a
/// runtime whose devices are there as soon as they are asked for.
fn write_song(file: &Path, name: &str) {
    let runtime = runtime(&ScriptedFactory::new());
    let mut session = runtime.session.lock();
    let track = |session: &mut Session, name: &str| session.edit(|project| Ok(project.add_track(name))).unwrap();
    session
        .edit(|project| {
            project.name = name.to_owned();
            Ok(())
        })
        .unwrap();
    let bass = track(&mut session, "Bass");
    session.add_device(ChainRef::Track(bass), BASS, None, None).unwrap();
    session.add_device(ChainRef::Track(bass), AMP, None, None).unwrap();
    let drums = track(&mut session, "Drums");
    session.add_device(ChainRef::Track(drums), DRUMS, None, None).unwrap();
    let keys = track(&mut session, "Keys");
    session.add_device(ChainRef::Track(keys), "builtin:polysynth", None, None).unwrap();
    session.add_device(ChainRef::Track(keys), "builtin:reverb", None, None).unwrap();
    session.save(file).unwrap();
}

/// A window on a song of two tracks, whose devices take their time, and
/// the song of [`write_song`] in a file beside it.
fn window(scripted: &ScriptedFactory, folder: &Path) -> (Harness, PathBuf) {
    let file = folder.join("Night Shift.trill");
    write_song(&file, "Night Shift");
    let harness = Harness::on(runtime(scripted), UiOptions::default(), |session| {
        two_tracks(session);
    });
    (harness, file)
}

fn track_names(harness: &Harness) -> Vec<String> {
    harness.app.snapshot().project.tracks.iter().map(|track| track.name.clone()).collect()
}

fn opens(harness: &mut Harness, file: &Path) {
    let ctx = harness.ctx.clone();
    harness.app.open_file(&ctx, file.to_path_buf());
}

#[test]
fn the_window_says_which_song_it_is_opening_from_the_frame_after_it_is_chosen() {
    let folder = tempfile::tempdir().unwrap();
    let (mut harness, file) = window(&slow(), folder.path());
    assert_eq!(track_names(&harness), ["One", "Two"]);

    // Someone else has the session for a while, as the thread that reads
    // a long song has: the window cannot look at it.
    let (held, is_held) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let session = harness.session.clone();
    let holder = std::thread::spawn(move || {
        let guard = session.lock();
        held.send(()).unwrap();
        let _ = released.recv();
        drop(guard);
    });
    is_held.recv().unwrap();
    harness.session_is_held = true;

    opens(&mut harness, &file);
    harness.wait_until("the window to say it is opening the song", |harness| harness.app.opening().is_some());
    harness.idle(2);
    assert!(harness.is_on_screen("Opening Night Shift.trill"), "the window does not say what it is opening");
    // The song before is still what there is to draw, under it.
    assert_eq!(track_names(&harness), ["One", "Two"]);
    // Nothing is asked of the session meanwhile: a key that did would
    // wait here for whoever has it, and so would a click.
    harness.key(Modifiers::NONE, Key::Space);
    let play = harness.labeled("Play");
    harness.click(play);
    harness.idle(3);
    assert!(harness.is_on_screen("Opening Night Shift.trill"));

    release.send(()).unwrap();
    holder.join().unwrap();
    harness.session_is_held = false;
    harness.wait_until("the song", |harness| track_names(harness) == ["Bass", "Drums", "Keys"]);
    harness.idle(2);
    assert!(!harness.is_on_screen("Opening Night Shift.trill"), "the song is there: nothing is being opened");
    assert_eq!(harness.app.opening(), None);
    assert!(!harness.session.lock().transport().playing, "the key and the click went nowhere");
}

#[test]
fn a_song_is_on_the_screen_before_its_devices_and_the_window_says_how_far_they_are() {
    let folder = tempfile::tempdir().unwrap();
    let scripted = slow();
    let (mut harness, file) = window(&scripted, folder.path());
    let began = Instant::now();
    opens(&mut harness, &file);
    harness.wait_until("the song", |harness| track_names(harness) == ["Bass", "Drums", "Keys"]);
    harness.idle(2);
    assert!(began.elapsed() < MAKING, "the song waited for a device: {:?}", began.elapsed());

    // The song is there and none of the devices that take time is: the
    // window says how far the whole is, and each track that waits for one
    // says so.
    let arriving = harness.app.live().arriving.clone().expect("devices are on their way");
    assert_eq!((arriving.made, arriving.of, arriving.coming.len()), (3, 6, 3));
    assert!(harness.app.live().arriving_words.starts_with("3 of 6 devices"), "{}", harness.app.live().arriving_words);
    assert!(harness.is_on_screen("Loading devices: 3 of 6 devices"));
    assert!(harness.is_on_screen("Bass: Scripted Bass is not there yet"));
    assert!(harness.is_on_screen("Drums: Scripted Drums is not there yet"));
    assert!(!harness.is_on_screen("Keys: "), "the built-in devices were there with the song");
    assert!(scripted.made().len() <= 1, "at most the first is being made: {:?}", scripted.made());

    // The window is alive: a track is selected and its devices shown,
    // each saying that it is not there in place of what it has not got.
    let bass = harness.app.snapshot().project.tracks[0].id;
    harness.app.state_mut().select_track(bass);
    harness.app.state_mut().detail_tab = DetailTab::Devices;
    harness.idle(3);
    assert!(harness.is_on_screen("Being made Scripted Bass") || harness.is_on_screen("Waiting Scripted Bass"));
    assert!(harness.is_on_screen("Waiting Scripted Amp"));

    // Play is not refused, and says why some of the song is silent.
    harness.click_label("Play");
    harness.wait_for("playback to start", |session| session.transport().playing);
    let said: Vec<String> = harness.app.state().toasts.texts().iter().map(|text| text.to_string()).collect();
    assert!(
        said.iter().any(|text| text.starts_with("Playing while") && text.contains("still being made")),
        "playing a song that is not all there says nothing: {said:?}"
    );
    harness.click_label("Stop");

    // They arrive, one by one, and then nothing more is said of it. The
    // song is no more changed than it was when it was opened.
    harness.wait_until("every device", |harness| harness.app.live().arriving.is_none());
    harness.idle(3);
    for gone in ["Loading devices:", "Bass: Scripted Bass is not there yet", "Waiting Scripted Amp"] {
        assert!(!harness.is_on_screen(gone), "still on the screen: {gone}");
    }
    let session = harness.session.lock();
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert!(!session.is_modified() && !session.can_undo(), "devices arriving are no edit");
    let made: Vec<String> = scripted.made().into_iter().map(|(uri, _)| uri).collect();
    assert_eq!(made, [BASS, AMP, DRUMS], "in the order of the song, each once");
}

#[test]
fn another_song_chosen_in_the_middle_is_the_one_that_is_there() {
    let folder = tempfile::tempdir().unwrap();
    let scripted = slow();
    let (mut harness, first) = window(&scripted, folder.path());
    let second = folder.path().join("Day Shift.trill");
    {
        let runtime = runtime(&ScriptedFactory::new());
        let mut session = runtime.session.lock();
        let solo = session.edit(|project| Ok(project.add_track("Solo"))).unwrap();
        session.add_device(ChainRef::Track(solo), DRUMS, None, None).unwrap();
        session.save(&second).unwrap();
    }
    opens(&mut harness, &first);
    harness.wait_until("the first song", |harness| track_names(harness) == ["Bass", "Drums", "Keys"]);
    assert!(harness.app.live().arriving.is_some() || harness.session.lock().arriving().is_some());
    opens(&mut harness, &second);
    harness.wait_until("the second song", |harness| track_names(harness) == ["Solo"]);
    harness.wait_until("its devices", |harness| {
        harness.app.live().arriving.is_none() && harness.session.lock().arriving().is_none()
    });
    harness.idle(3);
    // The second song has its one device and nothing of the first's.
    let session = harness.session.lock();
    assert_eq!(session.project().devices().filter(|device| device.uri == DRUMS).count(), 1);
    assert!(session.project().devices().all(|device| device.uri != BASS && device.uri != AMP));
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert!(session.loading().iter().all(|id| session.project().device(*id).is_ok()));
    let drums = session.project().tracks[0].devices[0].id;
    assert!(!session.is_coming(drums), "the second song's drums are there");
    let made: Vec<String> = scripted.made().into_iter().map(|(uri, _)| uri).collect();
    assert_eq!(made.last().map(String::as_str), Some(DRUMS), "{made:?}");
    assert!(made.len() <= 2, "more of the first song was made than the device in hand: {made:?}");
}

#[test]
fn a_window_closed_in_the_middle_lets_go_of_what_was_being_made() {
    let folder = tempfile::tempdir().unwrap();
    let scripted = slow();
    let (mut harness, file) = window(&scripted, folder.path());
    opens(&mut harness, &file);
    harness.wait_until("the song", |harness| track_names(harness) == ["Bass", "Drums", "Keys"]);
    harness.wait_until("the first device to be asked for", |_| !scripted.made().is_empty());
    let began = Instant::now();
    drop(harness);
    // The window is gone. What was being made is finished and let go of,
    // and nothing more is begun: within the time one device takes, and a
    // little.
    let deadline = Instant::now() + Duration::from_secs(20);
    let settled = |story: &[String]| {
        let made = story.iter().filter(|line| line.starts_with("made")).count();
        made == story.iter().filter(|line| line.starts_with("gone")).count()
    };
    while !settled(&scripted.story()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let story = scripted.story();
    assert!(settled(&story), "something that was made was never let go of: {story:?}");
    assert!(scripted.made().len() < 3, "devices were made for a window that had gone: {story:?}");
    assert!(began.elapsed() < MAKING * 3, "closing waited for more than the device in hand: {:?}", began.elapsed());
}

#[test]
fn a_plugin_added_in_the_window_is_in_the_song_at_once_and_the_window_goes_on_answering() {
    let scripted = slow();
    let mut harness = Harness::on(runtime(&scripted), UiOptions::default(), |session| {
        two_tracks(session);
    });
    let one = harness.app.snapshot().project.tracks[0].id;
    harness.app.state_mut().select_track(one);
    harness.app.state_mut().detail_tab = DetailTab::Devices;
    // (Searched for, which brings its row up from the end of the list.)
    harness.app.state_mut().browser.search = "Scripted Ba".into();
    // (The list of devices is read on a helper thread: on a machine that
    // is busy it is not there within three frames.)
    harness.wait_until("the list of devices", |harness| harness.app.catalog_ready());
    harness.idle(3);

    // Double-clicked in the browser: the song has it before the frame is
    // over, and the frame is not the time a plugin takes to make.
    harness.click_label("Scripted Bass");
    harness.idle(30);
    let began = Instant::now();
    harness.double_click_label("Scripted Bass");
    harness.idle(2);
    assert!(began.elapsed() < MAKING, "the window stood still for the plugin: {:?}", began.elapsed());
    let devices: Vec<String> = harness.project().tracks[0].devices.iter().map(|device| device.uri.clone()).collect();
    let said: Vec<String> = harness.app.state().toasts.texts().iter().map(|text| text.to_string()).collect();
    assert_eq!(devices, [BASS], "the window said: {said:?}");
    // It says so where it is shown, and how far the whole is.
    assert!(harness.is_on_screen("Being made Scripted Bass") || harness.is_on_screen("Waiting Scripted Bass"));
    assert!(harness.is_on_screen("One: Scripted Bass is not there yet"));
    assert!(harness.is_on_screen("Loading devices: 1 of 2 devices"));
    // One undo takes it back, before it is there; redo brings it again.
    harness.key(Modifiers::COMMAND, Key::Z);
    harness.idle(2);
    assert!(harness.project().tracks[0].devices.is_empty());
    assert!(!harness.is_on_screen("One: Scripted Bass is not there yet"));
    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    harness.idle(2);
    assert_eq!(harness.project().tracks[0].devices.len(), 1);

    harness.wait_until("the plugin", |harness| {
        harness.app.live().arriving.is_none() && harness.session.lock().arriving().is_none()
    });
    harness.idle(3);
    for gone in ["Loading devices:", "One: Scripted Bass is not there yet", "Being made Scripted Bass"] {
        assert!(!harness.is_on_screen(gone), "still on the screen: {gone}");
    }
    let session = harness.session.lock();
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    let bass = session.project().tracks[0].devices[0].id;
    assert!(!session.is_coming(bass));
}

/// A window on four tracks with an instrument each, in the middle of a
/// made-up opening.
fn in_the_middle(scene: OpeningScene) -> Harness {
    let options = UiOptions { opening_scene: Some(scene), ..Default::default() };
    let mut harness = Harness::new(options, |session| {
        for name in ["One", "Two", "Three", "Four"] {
            let track = session.edit(|project| Ok(project.add_track(name))).unwrap();
            session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
        }
    });
    harness.idle(3);
    harness
}

#[test]
fn while_a_file_is_read_the_song_before_is_under_glass() {
    let mut harness = in_the_middle(OpeningScene::Reading);
    assert!(harness.is_on_screen("Opening Night Shift.trill"));
    // Nothing under the plate takes a click or a key.
    let play = harness.labeled("Play");
    harness.click(play);
    harness.key(Modifiers::NONE, Key::Space);
    harness.idle(3);
    assert!(!harness.session.lock().transport().playing);
    assert!(harness.app.live().arriving.is_none(), "the devices are not spoken of before the song is there");
}

#[test]
fn while_devices_arrive_each_one_that_is_not_there_says_so_where_it_is_shown() {
    let mut harness = in_the_middle(OpeningScene::Arriving);
    // Five devices with the master's limiter; the instruments of the
    // third and fourth track are to come, and the third's is being made.
    assert!(harness.is_on_screen("Loading devices: 3 of 5 devices: Poly Synth on Three"));
    assert!(harness.is_on_screen("Three: Poly Synth is not there yet"));
    assert!(harness.is_on_screen("Four: Poly Synth is not there yet"));
    assert!(!harness.is_on_screen("One: Poly Synth is not there yet"));

    let tracks: Vec<TrackId> = harness.app.snapshot().project.tracks.iter().map(|track| track.id).collect();
    harness.app.state_mut().detail_tab = DetailTab::Devices;
    for (track, said) in
        [(tracks[2], Some("Being made Poly Synth")), (tracks[3], Some("Waiting Poly Synth")), (tracks[0], None)]
    {
        harness.app.state_mut().select_track(track);
        harness.idle(3);
        for word in ["Being made Poly Synth", "Waiting Poly Synth"] {
            assert_eq!(harness.is_on_screen(word), said == Some(word), "{word} on the panel of {track:?}");
        }
    }

    // Starting the song says that it is not all there, and starts it.
    harness.click_label("Play");
    harness.wait_for("playback to start", |session| session.transport().playing);
    let said: Vec<String> = harness.app.state().toasts.texts().iter().map(|text| text.to_string()).collect();
    assert_eq!(
        said,
        ["Playing while 2 of the song\u{2019}s 5 devices are still being made. A track whose instrument is not there \
          yet is silent until it is."]
    );
}
