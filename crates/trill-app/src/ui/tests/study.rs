//! A study in the window: the workspace, the Studies place, the dialog.
//!
//! The study is a real one of a recording the test makes, with every stage
//! run over the real files by the scripted models, here in the test (the
//! window's tests have no second program to study in; the runs in a
//! process of their own are the desk's and the server's to prove). Nothing
//! of the user's is read or written, and no model is run.

use super::*;
use crate::ui::state::BrowserTab;
use crate::ui::study::Side;
use std::f64::consts::TAU;
use std::path::Path;
use trill_core::audio::AudioBuffer;
use trill_engine::AudioBackend;
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Parts, Storage, Studying};
use trill_study::models::Which;
use trill_study::models::scripted::Script;
use trill_study::run::Programs;
use trill_study::{Source, Studies, StudyOptions};

const RATE: f64 = 44_100.0;

fn tone(out: &mut [f32], at: f64, seconds: f64, note: f64, level: f32, harmonics: usize) {
    let hertz = 440.0 * 2f64.powf((note - 69.0) / 12.0);
    let start = (at * RATE) as usize;
    for index in 0..(seconds * RATE) as usize {
        let Some(slot) = out.get_mut(start + index) else { break };
        let time = index as f64 / RATE;
        let fade = (time / 0.004).min(1.0) * ((seconds - time) / 0.012).clamp(0.0, 1.0);
        let wave: f64 = (1..=harmonics).map(|k| (TAU * hertz * k as f64 * time).sin() / (k * k) as f64).sum();
        *slot += level * (wave * fade) as f32;
    }
}

/// Eight bars at 100 beats a minute, starting 0.3 seconds in: a hat on
/// every eighth, a bass note to the beat, and a voice that sings four
/// notes in every other bar: what the scripted models, which split by
/// register, find a voice, drums and a bass in.
fn recording(path: &Path) {
    let beat = 0.6;
    let mut out = vec![0.0f32; ((0.3 + 32.0 * beat + 0.6) * RATE) as usize];
    let mut seed = 77u32;
    for index in 0..64 {
        let start = ((0.3 + index as f64 * beat / 2.0) * RATE) as usize;
        let mut previous = 0.0f32;
        for frame in 0..(0.045 * RATE) as usize {
            let Some(slot) = out.get_mut(start + frame) else { break };
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let hiss = (seed >> 8) as f32 / 8_388_608.0 - 1.0;
            *slot += 0.5 * (hiss - previous) * (-(frame as f32) / (0.009 * RATE as f32)).exp();
            previous = hiss;
        }
    }
    for index in 0..32 {
        tone(&mut out, 0.3 + index as f64 * beat, beat * 0.9, [33.0, 33.0, 40.0, 36.0][index % 4], 0.5, 2);
    }
    for bar in (0..8).step_by(2) {
        for (index, note) in [69.0, 72.0, 76.0, 72.0].into_iter().enumerate() {
            tone(&mut out, 0.3 + (bar * 4 + index) as f64 * beat, beat * 0.8, note, 0.3, 4);
        }
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(path, &AudioBuffer { sample_rate: RATE, left: out.clone(), right: out }, WavFormat::Int24).unwrap();
}

struct Fixture {
    harness: Harness,
    /// The study's id.
    id: String,
    /// Holds the settings, the studies and the recording.
    folder: tempfile::TempDir,
}

fn script() -> Script {
    Script { words: ["one", "two", "three", "four"].map(String::from).to_vec(), ..Script::default() }
}

/// The window on a runtime that keeps a study of a small recording, with
/// the song `setup` makes.
fn fixture(options: impl FnOnce(&str) -> UiOptions, setup: impl FnOnce(&mut Session)) -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let recorded = folder.path().join("refs").join("small song.wav");
    recording(&recorded);
    let studies = Studies::new(folder.path().join("config").join("studies"));
    let (study_folder, study) =
        studies.create("small song", Source::File { path: recorded }, StudyOptions::default()).unwrap();
    let mut models = Which::Scripted(script()).make().unwrap();
    trill_study::run::run(&study_folder, &mut *models, &Programs::default(), &mut |_| {}).unwrap();
    assert_eq!(study_folder.study().unwrap().state, trill_study::State::Done);

    let studying = Studying { stand_in: Some(script()), programs: Some(Programs::default()), ..Studying::default() };
    let storage = Storage::At { settings: folder.path().join("config").join("settings.json"), library_index: None };
    let runtime_options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let runtime = Arc::new(Runtime::start_custom(runtime_options, Parts { studying, ..Parts::default() }).unwrap());
    let harness = Harness::on(runtime, options(&study.id), setup);
    Fixture { harness, id: study.id, folder }
}

/// The window with the study open in the workspace.
fn opened(setup: impl FnOnce(&mut Session)) -> Fixture {
    let mut fixture =
        fixture(|id| UiOptions { study: Some(id.to_owned()), browser: false, ..UiOptions::default() }, setup);
    fixture.harness.wait_until("the study to open", |harness| harness.app.studies().open().is_some());
    fixture.harness.settle_study();
    fixture
}

impl Harness {
    /// Waits until nothing that was asked of studies is still being done.
    fn settle_study(&mut self) {
        self.wait_until("the study view to settle", |harness| !harness.app.studies().is_waiting());
        self.idle(3);
        self.wait_until("the study view to settle", |harness| !harness.app.studies().is_waiting());
        self.wait_until("the workers to settle", |harness| !harness.app.sounds().is_busy());
        self.idle(2);
    }

    /// The place of a moment of the recording on a lane of the workspace,
    /// which shows the whole recording from edge to edge.
    fn in_study(&self, seconds: f64, lane: usize) -> Pos2 {
        let lanes = self.all_labeled("Stems of the study")[0];
        let open = self.app.studies().open().unwrap().clone();
        let x = lanes.left() + (seconds / open.seconds) as f32 * lanes.width();
        let height = lanes.height() / open.stems.len() as f32;
        pos2(x, lanes.top() + (lane as f32 + 0.5) * height)
    }
}

#[test]
fn the_workspace_lays_a_study_out_and_a_stretch_is_selected_heard_and_sent_to_the_song() {
    let Fixture { mut harness, id, folder } = opened(|session| {
        session.edit(|project| project.set_tempo(120.0)).unwrap();
    });
    // What it is, and what was found in it.
    assert!(harness.is_on_screen("Study: small song"));
    for label in ["Mute vocals", "Solo drums", "Mute bass", "Mute other", "Snap to bar", "As recorded", "Play round"] {
        assert!(harness.is_on_screen(label), "{label}");
    }
    assert!(harness.is_on_screen("Word: one") && harness.is_on_screen("Word: four"));
    let open = harness.app.studies().open().unwrap().clone();
    assert_eq!(open.stems.iter().map(|stem| stem.0.as_str()).collect::<Vec<_>>(), ["vocals", "drums", "bass", "other"]);
    let bars = open.bars.clone();
    assert!(bars.len() >= 8, "{bars:?}");

    // Dragged over the drums from inside one bar into the next: the two bars, from line to line, on that lane.
    let (from, to) = (bars[2] + 0.4, bars[3] + 1.6);
    harness.drag(harness.in_study(from, 1), harness.in_study(to, 1));
    harness.idle(2);
    let selection = harness.app.studies().view.selection.clone().expect("a selection");
    assert_eq!((selection.from, selection.to, selection.lanes), (bars[2], bars[4], (1, 1)));
    assert_eq!(harness.project().tracks.len(), 0, "selecting takes nothing");

    // Played round: made as it would lie in the song, by a helper, and stopped again.
    harness.click_label("Play round");
    harness.wait_until("the stretch to sound", |harness| harness.session.lock().is_auditioning());
    assert!(harness.is_on("Play round"));
    harness.click_label("Play round");
    harness.wait_until("the stretch to stop", |harness| !harness.session.lock().is_auditioning());

    // Sent to the song: a track of its own, a clip of the drums' own file that follows the song, one undo step.
    let steps = harness.undo_depth();
    harness.click_label("To the song");
    harness.wait_until("the stretch to arrive", |harness| !harness.project().tracks.is_empty());
    harness.settle_study();
    let project = harness.project();
    let clip = &project.tracks[0].clips[0];
    let audio = clip.audio().unwrap();
    let stem = folder.path().join("config").join("studies").join(&id).join("stems").join("drums.wav");
    assert_eq!(audio.path, stem.display().to_string());
    assert_eq!((clip.start, clip.length, audio.warp), (0.0, 8.0, trill_core::project::WarpMode::Beats));
    assert!((audio.offset_seconds - bars[2]).abs() < 1e-9 && (audio.source_tempo.unwrap() - 100.0).abs() < 0.5);
    assert_eq!(harness.undo_depth(), steps + 1);

    // As recorded, and with the bass soloed: what is heard is what is taken.
    harness.click_label("As recorded");
    harness.drag(harness.in_study(bars[0] + 0.2, 0), harness.in_study(bars[1] - 0.2, 3));
    harness.idle(2);
    assert_eq!(harness.app.studies().view.selection.as_ref().unwrap().lanes, (0, 3));
    harness.click_label("Solo bass");
    harness.key(Modifiers::NONE, Key::Enter);
    // It goes onto the track that is selected, the one the drums came to, where the edit cursor is.
    harness.wait_until("the bass to arrive", |harness| harness.project().tracks[0].clips.len() == 2);
    harness.settle_study();
    assert!(harness.is_on("Solo bass"), "Enter sends the selection: it does not press what was clicked last");
    let project = harness.project();
    assert_eq!(project.tracks.len(), 1);
    let bass = |clip: &&trill_core::Clip| clip.audio().is_some_and(|audio| audio.path.ends_with("bass.wav"));
    let clip = project.tracks[0].clips.iter().find(bass).expect("a clip of the bass alone");
    assert_eq!(clip.audio().unwrap().warp, trill_core::project::WarpMode::Off);
    // One bar of 2.4 seconds is 4.8 beats of a song at 120.
    assert!((clip.length - 4.8).abs() < 0.01, "{}", clip.length);
}

#[test]
fn what_is_found_in_a_study_is_dragged_onto_the_lanes_of_the_song() {
    let mut made = None;
    let Fixture { mut harness, folder: _kept, .. } = opened(|session| made = Some(two_tracks(session)));
    let (first, second, _) = made.unwrap();
    let bars = harness.app.studies().open().unwrap().bars.clone();

    // A selection, picked up inside itself and let go on a lane of the song: a clip there, at that beat.
    harness.drag(harness.in_study(bars[1] + 0.3, 2), harness.in_study(bars[2] - 0.3, 2));
    harness.idle(2);
    let inside = harness.in_study((bars[1] + bars[2]) / 2.0, 2);
    let target = harness.lane(8.0, 1);
    harness.drag_start(Modifiers::NONE, inside, target);
    assert!(harness.hint().starts_with("Drop to add bass, bar 2 to "), "{}", harness.hint());
    harness.drag_end(target);
    harness.wait_until("the bass to arrive", |harness| harness.project().track(second).unwrap().clips.len() == 1);
    harness.settle_study();
    let project = harness.project();
    let clip = &project.track(second).unwrap().clips[0];
    assert_eq!((clip.start, clip.length), (8.0, 4.0));
    assert!(clip.audio().unwrap().path.ends_with("bass.wav"));

    // A sung line from the list beside the lanes: that stretch of the vocal, named by its words.
    harness.click_label("LINES");
    harness.idle(2);
    assert_eq!(harness.app.studies().view.side, Side::Words);
    let line = harness.labeled("one two three four");
    let target = harness.lane(16.0, 0);
    harness.drag_start(Modifiers::NONE, line, target);
    harness.drag_end(target);
    harness.wait_until("the line to arrive", |harness| harness.project().track(first).unwrap().clips.len() == 2);
    harness.settle_study();
    let project = harness.project();
    let clip = project.track(first).unwrap().clips.iter().find(|clip| clip.start == 16.0).unwrap();
    assert_eq!(clip.name, "one two three four");
    assert!(clip.audio().unwrap().path.ends_with("vocals.wav"));

    // The bass line as notes, onto a track.
    harness.click_label("NOTES");
    harness.idle(2);
    let row = harness.labeled("Bass line");
    let target = harness.lane(24.0, 1);
    harness.drag_start(Modifiers::NONE, row, target);
    harness.drag_end(target);
    harness.wait_until("the notes to arrive", |harness| harness.project().track(second).unwrap().clips.len() == 2);
    harness.settle_study();
    let project = harness.project();
    let clip = project.track(second).unwrap().clips.iter().find(|clip| clip.start == 24.0).unwrap();
    assert!(clip.notes().len() >= 3, "{}", clip.notes().len());

    // A single hit, onto a lane as a clip of a file of its own.
    harness.click_label("HITS");
    harness.idle(2);
    let hit = harness.labeled("Hat 1");
    let target = harness.lane(30.0, 0);
    harness.drag_start(Modifiers::NONE, hit, target);
    harness.drag_end(target);
    harness.wait_until("the hit to arrive", |harness| harness.project().track(first).unwrap().clips.len() == 3);
    let project = harness.project();
    let clip = project.track(first).unwrap().clips.iter().find(|clip| clip.start == 30.0).unwrap();
    assert!(clip.audio().unwrap().path.ends_with("hat-1.wav") && clip.name == "Hat 1", "{clip:?}");

    // Escape puts down what is in the air.
    harness.click_label("LOOPS");
    harness.idle(2);
    harness.drag(harness.in_study(bars[1] + 0.3, 2), harness.in_study(bars[2] - 0.3, 2));
    harness.idle(2);
    harness.drag_start(Modifiers::NONE, harness.in_study(bars[1] + 1.0, 2), harness.lane(40.0, 0));
    assert!(harness.app.state().sound_drag.is_some());
    harness.key(Modifiers::NONE, Key::Escape);
    harness.drag_end(harness.lane(40.0, 0));
    harness.idle(3);
    assert_eq!(harness.project().track(first).unwrap().clips.len(), 3);
}

#[test]
fn the_workspace_in_a_window_of_its_own_is_the_same_view_and_drags_into_the_song() {
    let mut made = None;
    let Fixture { mut harness, folder: _kept, .. } = opened(|session| made = Some(two_tracks(session)));
    let (first, _, _) = made.unwrap();
    let bars = harness.app.studies().open().unwrap().bars.clone();
    harness.click_label("In a window of its own");
    harness.idle(4);
    assert!(harness.app.studies().apart);
    // Here, where there is no second window to be had, it is a panel inside the first, and the tab shows it too.
    assert!(harness.all_labeled("Stems of the study").len() >= 2);
    // A stretch selected in it and carried onto a lane of the song.
    let lanes = *harness.all_labeled("Stems of the study").last().unwrap();
    let open = harness.app.studies().open().unwrap().clone();
    let at = |seconds: f64, lane: usize| {
        let height = lanes.height() / open.stems.len() as f32;
        pos2(lanes.left() + (seconds / open.seconds) as f32 * lanes.width(), lanes.top() + (lane as f32 + 0.5) * height)
    };
    harness.drag(at(bars[4] + 0.3, 1), at(bars[5] - 0.3, 1));
    harness.idle(2);
    let selection = harness.app.studies().view.selection.clone().expect("a selection made in the window of its own");
    assert_eq!((selection.from, selection.to), (bars[4], bars[5]));
    let target = harness.lane(4.0, 0);
    harness.drag_start(Modifiers::NONE, at((bars[4] + bars[5]) / 2.0, 1), target);
    harness.drag_end(target);
    harness.wait_until("the stretch to arrive", |harness| harness.project().track(first).unwrap().clips.len() == 2);
    // And put away again.
    harness.settle_study();
    harness.click_label("In a window of its own");
    harness.idle(3);
    assert!(!harness.app.studies().apart);
}

#[test]
fn a_word_is_put_right_by_hand_and_keeps_its_time() {
    let Fixture { mut harness, id, folder } = opened(|_| {});
    let lyrics_file = folder.path().join("config").join("studies").join(&id).join("lyrics.json");
    let before: trill_study::Lyrics = serde_json::from_str(&std::fs::read_to_string(&lyrics_file).unwrap()).unwrap();
    let word = harness.all_labeled("Word: two")[0].center();
    harness.double_click(word);
    harness.idle(3);
    assert!(harness.is_on_screen("The word as it is sung"));
    harness.key(Modifiers::COMMAND, Key::A);
    harness.type_text("too");
    harness.key(Modifiers::NONE, Key::Enter);
    harness.wait_until("the word to be kept", |harness| harness.is_on_screen("Word: too"));
    harness.settle_study();
    let after: trill_study::Lyrics = serde_json::from_str(&std::fs::read_to_string(&lyrics_file).unwrap()).unwrap();
    let (was, is) = (&before.lines[0].words[1], &after.lines[0].words[1]);
    assert_eq!((was.text.trim(), is.text.as_str()), ("two", "too"));
    assert_eq!((was.start, was.end), (is.start, is.end), "its time stays");
    assert_eq!(after.lines[0].text, "one too three four");
    // The list of lines says it too.
    harness.click_label("LINES");
    harness.idle(2);
    assert!(harness.is_on_screen("one too three four"));
}

#[test]
fn the_studies_place_lists_studies_and_a_study_that_the_song_plays_says_so_before_it_goes() {
    let Fixture { mut harness, id, folder } =
        fixture(|_| UiOptions { browser_tab: Some(BrowserTab::Studies), ..UiOptions::default() }, |_| {});
    let study_folder = folder.path().join("config").join("studies").join(&id);
    harness.wait_until("the studies to be listed", |harness| !harness.app.studies().list().is_empty());
    harness.idle(2);
    let row = harness.all_labeled("small song: 0:20")[0].center();
    harness.click(row);
    harness.idle(2);
    for label in ["Open the study", "Drop the stems", "Remove the study"] {
        assert!(harness.is_on_screen(label), "{label}");
    }
    // Opened from there, it is laid out along the bottom.
    harness.click_label("Open the study");
    harness.wait_until("the study to open", |harness| harness.app.studies().open().is_some());
    harness.settle_study();
    assert_eq!(harness.app.state().detail_tab, DetailTab::Study);
    assert!(harness.is_on_screen("Study: small song"));

    // The song comes to play its drums; asked to drop the stems, the question names the song, and "Keep" keeps them.
    let bars = harness.app.studies().open().unwrap().bars.clone();
    harness.drag(harness.in_study(bars[1] + 0.3, 1), harness.in_study(bars[2] - 0.3, 1));
    harness.click_label("To the song");
    harness.wait_until("the stretch to arrive", |harness| !harness.project().tracks.is_empty());
    harness.settle_study();
    harness.click_label("Drop the stems");
    harness.wait_until("the question", |harness| harness.is_on_screen("Keep"));
    let asked =
        harness.labels.iter().map(|(text, _)| text.clone()).find(|text| text.contains("These songs play its files"));
    assert!(asked.is_some_and(|asked| asked.contains("the song that is open (1 place)")), "{:?}", harness.labels);
    harness.click_label("Keep");
    harness.idle(3);
    assert!(study_folder.join("stems").join("drums.wav").is_file());

    // Asked again and answered with yes: the stems go, what was found stays, and the lanes say so.
    harness.click_label("Drop the stems");
    harness.wait_until("the question", |harness| harness.is_on_screen("Keep"));
    harness.click_label("Yes, remove");
    harness.wait_until("the stems to go", |_| !study_folder.join("stems").exists());
    harness.settle_study();
    assert!(study_folder.join("heard.json").is_file());
    assert!(harness.app.studies().open().is_some_and(|open| open.stems.iter().all(|stem| !stem.2)));
    // Sending a stretch of a stem that is gone is a sentence, not a clip.
    harness.click_label("To the song");
    harness.wait_until("the refusal", |harness| harness.app.state().toasts.len() > 0);
    assert_eq!(harness.project().tracks.len(), 1);

    // Removed altogether: the workspace has nothing open, and the list is empty.
    harness.click_label("Remove the study");
    harness.wait_until("the question", |harness| harness.is_on_screen("Keep"));
    harness.click_label("Yes, remove");
    harness.wait_until("the study to go", |_| !study_folder.exists());
    harness.wait_until("the list to empty", |harness| harness.app.studies().list().is_empty());
    assert!(harness.app.studies().open().is_none());
}

#[test]
fn the_dialog_asks_for_a_file_or_a_link_and_says_why_nothing_was_studied() {
    let Fixture { mut harness, folder: _kept, .. } =
        fixture(|_| UiOptions { browser_tab: Some(BrowserTab::Studies), ..UiOptions::default() }, |_| {});
    harness.click_label("Study a recording");
    harness.idle(3);
    assert!(harness.app.studies().dialog.open);
    assert!(harness.is_on_screen("A file, or a link to a recording"));
    // Nothing named: said, and nothing asked for.
    harness.click_label("Study");
    harness.idle(2);
    assert!(harness.is_on_screen("Say what to study: a file, or a link to a recording."));
    // A link: whose the recording is, before anything is fetched.
    harness.click_label("A file, or a link to a recording");
    harness.type_text("https://recordings.test/a-song");
    harness.idle(2);
    assert!(harness.is_on_screen(trill_study::desk::WHOSE));
    // This window was not started as a program that can study: a sentence, and the dialog stays.
    harness.click_label("Study");
    harness.wait_until("the refusal", |harness| harness.app.studies().dialog.refused.is_some());
    harness.idle(2);
    let refused = harness.app.studies().dialog.refused.clone().unwrap();
    assert!(harness.is_on_screen(&refused), "{refused}");
    assert!(harness.app.studies().dialog.open);
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    assert!(!harness.app.studies().dialog.open);
}

/// The window's test program, started again as the process that studies:
/// this is the one test it is then told to run. Run as a test like any
/// other it has nothing to do.
#[test]
fn this_program_studies_when_it_is_started_for_that() {
    trill_study::helper::run_if_requested();
}

/// The window on a runtime that can study: by this test program, started again.
fn studying(options: UiOptions, slow: f64) -> (Harness, tempfile::TempDir) {
    trill_study::helper::serve_by_test("ui::tests::study::this_program_studies_when_it_is_started_for_that");
    let folder = tempfile::tempdir().unwrap();
    recording(&folder.path().join("refs").join("small song.wav"));
    let studying = Studying {
        stand_in: Some(Script { seconds: slow, ..script() }),
        programs: Some(Programs::default()),
        program: Some(std::env::current_exe().unwrap()),
        ..Studying::default()
    };
    let storage = Storage::At { settings: folder.path().join("config").join("settings.json"), library_index: None };
    let runtime_options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let runtime = Arc::new(Runtime::start_custom(runtime_options, Parts { studying, ..Parts::default() }).unwrap());
    (Harness::on(runtime, options, |_| {}), folder)
}

#[test]
fn a_recording_is_studied_from_the_dialog_stopped_taken_up_and_opens_when_it_is_done() {
    let options = UiOptions { browser_tab: Some(BrowserTab::Studies), ..UiOptions::default() };
    let (mut harness, folder) = studying(options, 0.5);
    let recorded = folder.path().join("refs").join("small song.wav");
    harness.click_label("Study a recording");
    harness.idle(3);
    harness.click_label("A file, or a link to a recording");
    harness.type_text(&recorded.display().to_string());
    harness.click_label("Study");
    // The dialog goes, and the study is listed with the stage it is at.
    harness.wait_until("the study to be listed", |harness| !harness.app.studies().list().is_empty());
    assert!(!harness.app.studies().dialog.open);
    harness.wait_until("a stage to be said", |harness| harness.app.studies().list()[0].at.is_some());
    harness.idle(2);
    let said = |harness: &Harness| {
        harness.labels.iter().map(|(text, _)| text.clone()).find(|text| text.starts_with("small song: "))
    };
    let at = said(&harness).unwrap();
    assert!(at.contains(" %"), "{at}");
    // Stopped: every stage that had finished is kept, and it says it can be taken up.
    let row = harness.all_labeled("small song: ")[0].center();
    harness.click(row);
    harness.click_label("Stop the study");
    harness.wait_until("the study to stop", |harness| {
        harness.app.studies().list()[0].study.state == trill_study::State::Stopped
    });
    harness.idle(3);
    assert_eq!(said(&harness).unwrap(), "small song: Stopped: it can be taken up where it was");
    // Taken up, it goes on to its end, and opens in the workspace by itself.
    harness.click_label("Take the study up again");
    harness.wait_until("the study to open", |harness| harness.app.studies().open().is_some());
    harness.settle_study();
    assert_eq!(harness.app.state().detail_tab, DetailTab::Study);
    assert!(harness.is_on_screen("Study: small song") && harness.is_on_screen("Mute drums"));
    assert_eq!(harness.app.studies().list()[0].study.state, trill_study::State::Done);
}

#[test]
fn the_timed_test_is_run_from_the_setup_panel_and_says_what_it_chose() {
    let (mut harness, _folder) = studying(UiOptions::default(), 0.0);
    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Comma);
    harness.wait_until("the panel to read", |harness| !harness.app.setup().is_busy());
    harness.idle(3);
    harness.click_label("This machine");
    harness.idle(3);
    assert!(harness.is_on_screen("Run the timed test"));
    harness.click_label("Run the timed test");
    harness.wait_until("the timed test", |harness| !harness.app.setup().is_busy());
    harness.idle(4);
    harness.wait_until("the panel to read again", |harness| !harness.app.setup().is_busy());
    harness.idle(3);
    // What ran the models, how fast, and that the test is what chose it.
    let chosen =
        harness.labels.iter().map(|(text, _)| text.clone()).find(|text| text.contains("chosen by the timed test"));
    assert!(chosen.is_some(), "{:?}", harness.labels.iter().map(|(text, _)| text).collect::<Vec<_>>());
    assert!(
        harness.app.setup().last().is_none_or(|said| !said.contains("could not")),
        "{:?}",
        harness.app.setup().last()
    );
}

#[test]
fn the_recording_is_put_beside_the_song_as_a_reference_and_what_it_is_as_a_whole_is_taken() {
    let Fixture { mut harness, id, folder: _kept } = opened(|session| {
        session.edit(|project| project.set_tempo(120.0)).unwrap();
    });
    harness.click_label("NOTES");
    harness.idle(2);
    // The recording beside the song: a track that is a reference, of this study, not listened to.
    harness.click_label("Take the recording as a reference");
    harness.wait_until("the reference", |harness| harness.project().has_references());
    harness.settle_study();
    let project = harness.project();
    let track = &project.tracks[0];
    assert_eq!(track.name, "Reference: small song");
    assert_eq!(track.reference, Some(trill_core::Reference { study: Some(id), listening: false }));
    assert_eq!(project.length_beats(), 0.0, "it is no part of the song's length");
    // Its header listens in place of soloing, and says what it is.
    assert!(harness.is_on_screen("Listen to Reference: small song"));
    assert!(!harness.is_on_screen("Solo Reference: small song") && !harness.is_on_screen("Mute Reference: small song"));
    harness.click_label("Listen to Reference: small song");
    harness.idle(3);
    assert!(harness.project().tracks[0].reference.as_ref().unwrap().listening);
    assert!(harness.is_on("Listen to Reference: small song"));

    // Its tempo, bar by bar; its parts as markers; its key; its kit.
    harness.click_label("Take the tempo");
    harness.wait_until("the tempo", |harness| (harness.project().tempo - 100.0).abs() < 0.5);
    harness.click_label("Take the parts");
    harness.wait_until("the markers", |harness| !harness.project().markers.is_empty());
    harness.click_label("Take the key");
    harness.wait_until("the key", |harness| harness.project().key.is_some());
    harness.click_label("Take a kit");
    harness.wait_until("the kit", |harness| harness.project().tracks.len() == 2);
    harness.settle_study();
    let project = harness.project();
    assert_eq!(project.tracks[1].devices[0].uri, "builtin:drumrack");
    assert!(harness.app.studies().last().is_some_and(|(said, wrong)| !wrong && said.starts_with("A drum rack of ")));
    // The song says its key now, and the workspace can move what it takes to it.
    assert!(!harness.is_on("To the song's key"));
}
