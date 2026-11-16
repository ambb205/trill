//! Sounds: the browser's library tab, previews, drops onto tracks, samplers
//! and pads, waveforms on audio clips, the audio clip panel and the panels
//! of the sampler and the drum rack.
//!
//! Each test gets a sample pack of its own in a temporary folder and a
//! runtime that keeps nothing: no test reads or writes the user's settings,
//! library or samples.

use super::*;
use crate::ui::sounds::{DropTarget, WaveState};
use crate::ui::state::{BrowserTab, DragSource, FocusArea, SoundForm, TempoFilter};
use crate::ui::{audio_clip, sample_panels};
use std::path::{Path, PathBuf};
use trill_core::audio::AudioBuffer;
use trill_core::project::WarpMode;
use trill_core::{Clip, TrackKind};
use trill_dsp::{drumrack, sampler};
use trill_library::Query;
use trill_media::{WavFormat, write_wav};
use trill_runtime::SoundClip;

pub(super) const WAIT: Duration = Duration::from_secs(60);

/// A file another program let go of over the window.
#[derive(Debug)]
pub(super) struct Dropped(pub PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|error| error.to_string())
    }
}

/// Writes `seconds` of a decaying tone and returns the file's path.
pub(super) fn tone(path: PathBuf, seconds: f64) -> PathBuf {
    let rate = 44_100.0;
    let frames = (seconds * rate) as usize;
    let channel: Vec<f32> = (0..frames)
        .map(|n| {
            let time = n as f64 / rate;
            ((time * 220.0 * std::f64::consts::TAU).sin() * (1.0 - n as f64 / frames as f64) * 0.8) as f32
        })
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(&path, &AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }, WavFormat::Int16)
        .unwrap();
    path
}

/// A small sample pack in a folder of its own, gone when this is dropped.
struct Pack {
    _folder: tempfile::TempDir,
    root: PathBuf,
    kick: PathBuf,
    snare: PathBuf,
    hat: PathBuf,
    /// Four beats at 100 BPM: 2.4 s.
    bass_loop: PathBuf,
    /// Eight beats at 120 BPM in F sharp minor: 4 s.
    drum_loop: PathBuf,
    pluck: PathBuf,
}

impl Pack {
    fn new() -> Self {
        let folder = tempfile::tempdir().unwrap();
        // Spelled as the library spells it, so paths compare equal.
        let root = std::path::absolute(folder.path().join("Test Pack")).unwrap();
        Self {
            kick: tone(root.join("Drums").join("Kick 01.wav"), 0.5),
            snare: tone(root.join("Drums").join("Snare Tight.wav"), 0.25),
            hat: tone(root.join("Drums").join("HH Closed.wav"), 0.1),
            bass_loop: tone(root.join("Loops").join("Bass Loop 100bpm.wav"), 2.4),
            drum_loop: tone(root.join("Loops").join("Drum Loop 120bpm F#m.wav"), 4.0),
            pluck: tone(root.join("Keys").join("Pluck C3.wav"), 0.6),
            root,
            _folder: folder,
        }
    }

    fn names() -> [&'static str; 6] {
        ["HH Closed", "Kick 01", "Snare Tight", "Pluck C3", "Bass Loop 100bpm", "Drum Loop 120bpm F#m"]
    }
}

pub(super) fn sounds_tab() -> UiOptions {
    UiOptions { browser_tab: Some(BrowserTab::Sounds), ..Default::default() }
}

impl Harness {
    /// Runs frames until the sound workers have nothing left to do and
    /// what they finished has been taken in and drawn.
    pub(super) fn settle_work(&mut self) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.idle(2);
            let busy = self.app.sounds().is_busy();
            if !busy {
                // What was taken in may have asked for more.
                self.idle(2);
                if !self.app.sounds().is_busy() {
                    self.idle(1);
                    return;
                }
            }
            assert!(Instant::now() < deadline, "the sound workers never finished");
            self.app.sounds().wait_until_idle(Duration::from_millis(200));
        }
    }

    /// Adds the pack to the library, as choosing it in the dialog does, and
    /// waits for the scan and the list.
    fn add_pack(&mut self, pack: &Pack) {
        self.app.sounds_mut().add_folder(pack.root.clone());
        self.settle_work();
        assert!(self.runtime.library.wait_until_idle(WAIT));
        self.settle_work();
    }

    /// The names of the sounds the browser lists, in order.
    fn listed(&self) -> Vec<String> {
        let found = self.app.sounds().search.found.as_ref().expect("a search has come back");
        found.results.entries().map(|entry| entry.name.clone()).collect()
    }

    fn sound_browser(&self) -> &crate::ui::state::SoundBrowser {
        &self.app.state().browser.sounds
    }

    fn selected_sound(&self) -> Option<String> {
        self.sound_browser().selected.as_deref().map(crate::ui::sounds::file_stem)
    }

    /// Presses on the row of the sound called `name` and carries it to `to`
    /// without letting go.
    fn carry_sound(&mut self, name: &str, to: Pos2) {
        self.idle(2);
        let from = self.labeled(name);
        self.drag_start(Modifiers::NONE, from, to);
        self.idle(1);
    }

    /// Drags the sound called `name` from the browser and drops it at `to`.
    fn drop_sound(&mut self, name: &str, to: Pos2) {
        self.carry_sound(name, to);
        self.drag_end(to);
        self.settle_work();
    }

    fn audio_clips(&self, track: usize) -> Vec<Clip> {
        self.project().tracks[track].clips.iter().filter(|clip| clip.audio().is_some()).cloned().collect()
    }

    /// The file on the pad for `note` of the drum rack `device`.
    fn pad_file(&self, device: DeviceId, note: u8) -> Option<String> {
        let project = self.project();
        let state = drumrack::State::from_device_state(project.device(device).unwrap().state.as_deref()).unwrap();
        state.path(note).map(str::to_owned)
    }

    fn sampler_file(&self, device: DeviceId) -> Option<String> {
        let project = self.project();
        sampler::State::from_device_state(project.device(device).unwrap().state.as_deref()).unwrap().path
    }

    fn wave(&self, path: &Path) -> WaveState {
        self.app.sounds().wave(&path.to_string_lossy())
    }

    fn decodes(&self) -> u64 {
        self.runtime.media.stats().decodes
    }
}

/// A window on the Sounds tab with the pack in its library and a project
/// of two empty tracks.
fn with_pack(options: UiOptions) -> (Harness, Pack, [TrackId; 2]) {
    let pack = Pack::new();
    let mut tracks = None;
    let mut harness = Harness::new(UiOptions { browser_tab: Some(BrowserTab::Sounds), ..options }, |session| {
        tracks = Some(session.edit(|project| Ok([project.add_track("One"), project.add_track("Two")])).unwrap());
    });
    harness.add_pack(&pack);
    (harness, pack, tracks.unwrap())
}

fn same_file(stored: &str, path: &Path) -> bool {
    Path::new(stored) == path
}

// ---- the browser ---------------------------------------------------------

#[test]
fn the_browser_switches_between_devices_and_sounds() {
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    assert_eq!(harness.app.state().browser.tab, BrowserTab::Devices);
    assert!(harness.is_on_screen("Poly Synth"));
    harness.click_label("Sounds");
    assert_eq!(harness.app.state().browser.tab, BrowserTab::Sounds);
    assert!(!harness.is_on_screen("Poly Synth"));
    // With no folders it says how to get some.
    assert!(harness.is_on_screen("Add folder\u{2026}"));
    assert!(harness.runtime.library.roots().is_empty(), "a test's library starts empty");
    harness.click_label("Devices");
    assert!(harness.is_on_screen("Poly Synth"));
}

#[test]
fn the_library_is_listed_and_the_search_narrows_it() {
    let (mut harness, _pack, _) = with_pack(sounds_tab());
    assert_eq!(harness.listed(), Pack::names(), "every sound, in the order of the folders");
    for name in Pack::names() {
        assert!(harness.is_on_screen(name), "{name} has a row");
    }

    // Clicking the field and typing searches as the keys go down.
    let field = pos2(harness.work_left() / 2.0, top_bar::HEIGHT + browser::HEADER + 8.0 + 11.0);
    harness.click(field);
    harness.type_text("loop");
    harness.settle_work();
    assert_eq!(harness.listed(), ["Bass Loop 100bpm", "Drum Loop 120bpm F#m"]);
    assert!(!harness.is_on_screen("Kick 01") && harness.is_on_screen("Bass Loop 100bpm"));
    // Words are found in folders, tags, tempos and keys too.
    harness.app.state_mut().browser.sounds.search = "drums".into();
    harness.settle_work();
    assert_eq!(
        harness.listed(),
        ["HH Closed", "Kick 01", "Snare Tight", "Drum Loop 120bpm F#m"],
        "the folder called Drums, and the loop that is drums"
    );
    harness.app.state_mut().browser.sounds.search = "100bpm".into();
    harness.settle_work();
    assert_eq!(harness.listed(), ["Bass Loop 100bpm"]);
    // The cross empties the field.
    harness.click_label("Clear the search");
    harness.settle_work();
    assert_eq!(harness.listed().len(), 6);
}

#[test]
fn the_filters_narrow_the_list_by_form_tempo_key_and_kind() {
    let (mut harness, _pack, _) = with_pack(sounds_tab());
    // The transport has a Loop too; the filter is the one in the browser.
    harness.click(harness.labeled_below("Loop", top_bar::HEIGHT));
    harness.settle_work();
    assert_eq!(harness.sound_browser().form, SoundForm::Loop);
    assert_eq!(harness.listed(), ["Bass Loop 100bpm", "Drum Loop 120bpm F#m"]);
    harness.click_label("One-shot");
    harness.settle_work();
    assert_eq!(harness.listed(), ["HH Closed", "Kick 01", "Snare Tight", "Pluck C3"]);
    harness.click_label("All");
    harness.settle_work();
    assert_eq!(harness.listed().len(), 6);

    // The tempo, from a menu.
    harness.click_label("BPM: Any");
    harness.click_label("90\u{2013}109 BPM");
    harness.settle_work();
    assert_eq!(harness.sound_browser().tempo, TempoFilter::Between(90.0, 109.0));
    assert_eq!(harness.listed(), ["Bass Loop 100bpm"]);
    // "Song tempo" follows the project, which is at 120.
    harness.click_label("BPM: 90\u{2013}109");
    harness.click_label("Song tempo (120)");
    harness.settle_work();
    assert_eq!(harness.listed(), ["Drum Loop 120bpm F#m"]);
    harness.click_label("BPM: 120");
    harness.click_label("Any tempo");
    harness.settle_work();

    harness.click_label("Key: Any");
    harness.click_label("F#m");
    harness.settle_work();
    assert_eq!(harness.listed(), ["Drum Loop 120bpm F#m"]);
    harness.click_label("Key: F#m");
    harness.click_label("Any key");
    harness.settle_work();

    harness.click_label("Kind: Any");
    harness.click_label("Kick");
    harness.settle_work();
    assert_eq!(harness.listed(), ["Kick 01"]);
}

#[test]
fn favorites_are_marked_on_a_row_and_can_be_all_that_is_shown() {
    let (mut harness, pack, _) = with_pack(sounds_tab());
    // The star appears on the row under the pointer.
    harness.move_to(harness.labeled("Snare Tight"));
    harness.idle(2);
    harness.click_label("Add Snare Tight to the favorites");
    assert!(harness.runtime.library.is_favorite(&pack.snare));
    assert!(harness.sound_browser().selected.is_none(), "marking a favorite does not choose or play the sound");

    harness.click_label("Favorites only");
    harness.settle_work();
    assert_eq!(harness.listed(), ["Snare Tight"]);
    harness.click_label("Remove Snare Tight from the favorites");
    harness.settle_work();
    assert!(!harness.runtime.library.is_favorite(&pack.snare));
    assert!(harness.listed().is_empty());
    harness.click_label("Favorites only");
    harness.settle_work();
    assert_eq!(harness.listed().len(), 6);
}

#[test]
fn folders_are_added_browsed_rescanned_and_removed() {
    let pack = Pack::new();
    let mut harness = Harness::new(sounds_tab(), |_| {});
    assert!(harness.is_on_screen("Add folder\u{2026}"));
    harness.add_pack(&pack);
    assert_eq!(harness.runtime.library.roots(), std::slice::from_ref(&pack.root));
    assert!(!harness.is_on_screen("Add folder\u{2026}"));
    assert!(harness.is_on_screen("All folders") && harness.is_on_screen("Test Pack"));
    assert_eq!(harness.app.state().toasts.len(), 1, "adding a folder is confirmed");

    // Choosing a folder unfolds it and looks only inside it.
    harness.click_label("Test Pack");
    harness.settle_work();
    assert!(harness.is_on_screen("Drums") && harness.is_on_screen("Loops") && harness.is_on_screen("Keys"));
    assert_eq!(harness.listed().len(), 6);
    harness.click_label("Loops");
    harness.settle_work();
    assert_eq!(harness.sound_browser().folder.as_deref(), Some(pack.root.join("Loops").as_path()));
    assert_eq!(harness.listed(), ["Bass Loop 100bpm", "Drum Loop 120bpm F#m"]);
    harness.click_label("All folders");
    harness.settle_work();
    assert_eq!(harness.listed().len(), 6);

    // A file that arrives later is found by a rescan.
    tone(pack.root.join("Drums").join("Clap Wide.wav"), 0.2);
    harness.click_label("Rescan");
    harness.settle_work();
    assert!(harness.runtime.library.wait_until_idle(WAIT));
    harness.settle_work();
    assert!(harness.listed().contains(&"Clap Wide".to_owned()));

    // Removing the folder empties the list; the files stay on disk.
    harness.right_click(harness.labeled("Test Pack"));
    harness.click_label("Remove from the library");
    harness.settle_work();
    assert!(harness.runtime.library.roots().is_empty());
    assert!(harness.is_on_screen("Add folder\u{2026}"));
    assert!(pack.kick.exists());
}

#[test]
fn the_keyboard_moves_through_the_list_previews_and_searches() {
    let (mut harness, pack, _) = with_pack(sounds_tab());
    harness.click_label("Kick 01");
    harness.settle_work();
    assert_eq!(harness.app.state().focus, FocusArea::Browser);
    assert_eq!(harness.selected_sound().as_deref(), Some("Kick 01"));

    harness.key(Modifiers::NONE, Key::ArrowDown);
    assert_eq!(harness.selected_sound().as_deref(), Some("Snare Tight"));
    harness.key(Modifiers::NONE, Key::ArrowUp);
    harness.key(Modifiers::NONE, Key::ArrowUp);
    assert_eq!(harness.selected_sound().as_deref(), Some("HH Closed"));
    harness.key(Modifiers::NONE, Key::ArrowUp);
    assert_eq!(harness.selected_sound().as_deref(), Some("HH Closed"), "the list ends there");
    harness.key(Modifiers::NONE, Key::End);
    assert_eq!(harness.selected_sound().as_deref(), Some("Drum Loop 120bpm F#m"));
    harness.key(Modifiers::NONE, Key::Home);
    assert_eq!(harness.sound_browser().selected_row, Some(0));
    harness.settle_work();

    // Space and Enter play the chosen sound; Space does not start the song.
    harness.app.sounds_mut().stop_preview();
    harness.key(Modifiers::NONE, Key::Space);
    harness.settle_work();
    assert!(!harness.session.lock().transport().playing);
    assert_eq!(harness.app.sounds().preview.playing.as_ref().map(|(path, ..)| path.clone()), Some(pack.hat.clone()));
    harness.app.sounds_mut().stop_preview();
    harness.key(Modifiers::NONE, Key::Enter);
    harness.settle_work();
    assert!(harness.app.sounds().preview.playing.is_some());

    // Letters are a search, not shortcuts: "a" would show automation.
    let lanes = harness.app.state().arrangement.lanes.len();
    harness.type_text("s");
    harness.step(
        Modifiers::NONE,
        vec![Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }],
    );
    harness.idle(2);
    harness.type_text("nare");
    harness.settle_work();
    assert_eq!(harness.sound_browser().search, "snare");
    assert_eq!(harness.app.state().arrangement.lanes.len(), lanes);
    assert_eq!(harness.listed(), ["Snare Tight"]);
    // The arrows still move through what is listed while the field has
    // the keyboard.
    harness.key(Modifiers::NONE, Key::ArrowDown);
    assert_eq!(harness.selected_sound().as_deref(), Some("Snare Tight"));

    // A click elsewhere gives the keyboard back: Space plays the song.
    harness.click(harness.lane(2.0, 1));
    assert_eq!(harness.app.state().focus, FocusArea::Arrangement);
    harness.key(Modifiers::NONE, Key::Space);
    harness.wait_for("playback to start", |session| session.transport().playing);
}

#[test]
fn the_browser_is_remembered_from_one_session_to_the_next() {
    let pack = Pack::new();
    let home = tempfile::tempdir().unwrap();
    let start = || {
        let storage = trill_runtime::Storage::At { settings: home.path().join("settings.json"), library_index: None };
        let options = RuntimeOptions { storage, ..RuntimeOptions::hermetic() };
        Arc::new(Runtime::start(options).unwrap())
    };
    let remembered = home.path().join("browser.json");

    // The first session: nothing to remember yet, so the browser is as it
    // starts, and nothing is written for having looked.
    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    assert_eq!(harness.app.state().browser.tab, BrowserTab::Devices);
    harness.idle(60);
    assert!(!remembered.exists());
    harness.click_label("Sounds");
    harness.add_pack(&pack);
    harness.click_label("Test Pack");
    harness.click_label("Loops");
    harness.app.state_mut().browser.sounds.search = "bass".into();
    harness.click(harness.labeled_below("Loop", top_bar::HEIGHT));
    harness.click_label("Preview on selection");
    harness.settle_work();
    assert_eq!(harness.listed(), ["Bass Loop 100bpm"]);
    // Written once it has stopped changing, by a worker.
    harness.idle(60);
    harness.settle_work();
    assert!(remembered.exists());
    drop(harness);

    // The next session starts where that one left off, the library's
    // folder included (that part is the settings').
    let mut harness = Harness::on(start(), UiOptions::default(), |_| {});
    assert!(harness.runtime.library.wait_until_idle(WAIT));
    harness.settle_work();
    let browser = &harness.app.state().browser;
    assert_eq!(browser.tab, BrowserTab::Sounds);
    assert_eq!((browser.sounds.search.as_str(), browser.sounds.form), ("bass", SoundForm::Loop));
    assert_eq!(browser.sounds.folder.as_deref(), Some(pack.root.join("Loops").as_path()));
    assert!(!browser.sounds.auto_preview);
    assert!(harness.is_on_screen("Loops"), "the tree is unfolded down to the folder");
    assert_eq!(harness.listed(), ["Bass Loop 100bpm"]);
    // What the command line asks for wins over what was remembered.
    drop(harness);
    let options =
        UiOptions { browser_tab: Some(BrowserTab::Devices), sound_search: Some("kick".into()), ..Default::default() };
    let harness = Harness::on(start(), options, |_| {});
    assert_eq!(harness.app.state().browser.tab, BrowserTab::Devices);
    assert_eq!(harness.app.state().browser.sounds.search, "kick");
}

// ---- previewing ----------------------------------------------------------

#[test]
fn choosing_a_sound_plays_it_and_escape_or_the_next_choice_ends_it() {
    let (mut harness, pack, _) = with_pack(sounds_tab());
    harness.click_label("Drum Loop 120bpm F#m");
    assert_eq!(harness.selected_sound().as_deref(), Some("Drum Loop 120bpm F#m"));
    harness.settle_work();
    let (path, seconds, _) = harness.app.sounds().preview.playing.clone().expect("the sound plays");
    assert_eq!(path, pack.drum_loop);
    assert!((seconds - 4.0).abs() < 1e-3);
    harness.wait_for("the audition to be heard", Session::is_auditioning);
    let position = harness.app.sounds().preview.position(&pack.drum_loop).expect("it is under way");
    assert!((0.0..0.9).contains(&position), "{position}");
    // Its waveform and what is known about it are in the strip.
    let WaveState::Ready(wave) = harness.wave(&pack.drum_loop) else { panic!("the strip has its waveform") };
    assert_eq!((wave.facts.tempo, wave.facts.sample_rate, wave.facts.channels), (Some(120.0), 44_100, 2));
    assert_eq!(wave.facts.key.map(|key| key.name()), Some("F#m"));
    assert!((wave.seconds() - 4.0).abs() < 1e-3);
    assert_eq!(harness.runtime.library.recent().first(), Some(&pack.drum_loop), "what is heard counts as used");

    // Another choice takes over.
    harness.click_label("Bass Loop 100bpm");
    harness.settle_work();
    assert_eq!(
        harness.app.sounds().preview.playing.as_ref().map(|(path, ..)| path.clone()),
        Some(pack.bass_loop.clone())
    );
    assert!(harness.app.sounds().preview.position(&pack.drum_loop).is_none());

    // Escape ends it, and only then does its usual work.
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(harness.app.sounds().preview.playing.is_none());
    harness.wait_for("the audition to stop", |session| !session.is_auditioning());
    // The strip's button plays it again and stops it.
    harness.click_label("Preview");
    harness.settle_work();
    assert!(harness.app.sounds().preview.playing.is_some());
    harness.click_label("Preview");
    assert!(harness.app.sounds().preview.playing.is_none());
    assert!(!harness.session.lock().transport().playing, "the song was never started");
}

#[test]
fn auto_preview_can_be_turned_off() {
    let (mut harness, _pack, _) = with_pack(sounds_tab());
    assert!(harness.sound_browser().auto_preview);
    harness.click_label("Preview on selection");
    assert!(!harness.sound_browser().auto_preview);
    harness.click_label("Kick 01");
    harness.settle_work();
    assert_eq!(harness.selected_sound().as_deref(), Some("Kick 01"));
    assert!(harness.app.sounds().preview.playing.is_none(), "chosen, not played");
    harness.key(Modifiers::NONE, Key::ArrowDown);
    harness.settle_work();
    assert!(harness.app.sounds().preview.playing.is_none());
    // Asking for it still plays.
    harness.key(Modifiers::NONE, Key::Enter);
    harness.settle_work();
    assert!(harness.app.sounds().preview.playing.is_some());
}

#[test]
fn a_sound_that_cannot_be_read_says_so_instead_of_playing() {
    let (mut harness, pack, _) = with_pack(sounds_tab());
    // Listed by the scan, then gone from the disk.
    std::fs::remove_file(&pack.snare).unwrap();
    harness.click_label("Snare Tight");
    harness.settle_work();
    assert!(harness.app.sounds().preview.playing.is_none());
    let (path, reason) = harness.app.sounds().preview.error.clone().expect("the failure is kept");
    assert_eq!(path, pack.snare);
    assert!(reason.contains("does not exist"), "{reason}");
    assert_eq!(harness.app.state().toasts.len(), 2, "the folder was added, and this");
    assert!(harness.wave(&pack.snare).failure().is_some_and(|reason| reason.contains("does not exist")));
}

// ---- dragging and dropping -------------------------------------------------

#[test]
fn a_sound_dropped_on_a_lane_becomes_a_clip_at_the_snapped_place() {
    let (mut harness, pack, tracks) = with_pack(sounds_tab());
    let undo = harness.undo_depth();
    // In the air: a ghost says where it would land, and nothing has changed.
    harness.carry_sound("Kick 01", harness.lane(4.3, 1));
    let drag = harness.app.state().sound_drag.clone().expect("the sound is being dragged");
    assert_eq!((drag.source, drag.label.as_str()), (DragSource::Browser, "Kick 01"));
    assert_eq!(drag.target, Some(DropTarget::Lane { track: tracks[1], beat: 4.0 }), "the grid's nearest line");
    assert!(harness.hint().contains("Drop to add Kick 01 to Two at 2.1.1"), "{}", harness.hint());
    assert!(harness.audio_clips(1).is_empty());
    harness.drag_end(harness.lane(4.3, 1));
    harness.settle_work();

    let clips = harness.audio_clips(1);
    assert_eq!(clips.len(), 1);
    let clip = &clips[0];
    let audio = clip.audio().unwrap();
    assert_eq!((clip.name.as_str(), clip.start), ("Kick 01", 4.0));
    assert!(same_file(&audio.path, &pack.kick));
    assert_eq!(audio.warp, WarpMode::Off, "a hit plays as it was recorded");
    assert!((clip.length - 1.0).abs() < 1e-3, "half a second at 120 is a beat: {}", clip.length);
    assert!(harness.app.state().sound_drag.is_none());
    // The new clip is selected, on its track.
    assert_eq!(harness.selected_clips(), [clip.id]);
    assert_eq!(harness.app.state().selected_track, Some(tracks[1]));
    assert!(harness.session.lock().problems().is_empty());

    // One drop, one undo step.
    assert_eq!(harness.undo_depth(), undo + 1);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert!(harness.audio_clips(1).is_empty());
}

#[test]
fn a_loop_dropped_on_a_lane_follows_the_songs_tempo() {
    let (mut harness, pack, _) = with_pack(sounds_tab());
    harness.session.lock().edit(|project| project.set_tempo(90.0)).unwrap();
    // The ghost is as long as the clip will be: its four beats.
    harness.carry_sound("Bass Loop 100bpm", harness.lane(8.0, 0));
    let drag = harness.app.state().sound_drag.clone().unwrap();
    assert!((drag.beats(90.0, 4.0) - 4.0).abs() < 1e-9);
    harness.drag_end(harness.lane(8.0, 0));
    harness.settle_work();
    let clip = harness.audio_clips(0).remove(0);
    let audio = clip.audio().unwrap();
    assert_eq!((audio.warp, audio.source_tempo), (WarpMode::Tonal, Some(100.0)));
    assert!((clip.length - 4.0).abs() < 1e-3, "four beats of it are four beats of the song: {}", clip.length);
    assert!(same_file(&audio.path, &pack.bass_loop));
    assert_eq!(harness.runtime.library.recent().first(), Some(&pack.bass_loop));
}

#[test]
fn a_sound_dropped_below_the_tracks_gets_a_track_of_its_own() {
    let (mut harness, pack, _) = with_pack(sounds_tab());
    let undo = harness.undo_depth();
    let below = pos2(harness.lane(8.2, 0).x, harness.row_top(2) + 24.0);
    harness.carry_sound("Snare Tight", below);
    let target = harness.app.state().sound_drag.as_ref().unwrap().target.clone();
    assert!(matches!(target, Some(DropTarget::NewTrack { beat, .. }) if beat == 8.0), "{target:?}");
    assert!(harness.hint().contains("new audio track"), "{}", harness.hint());
    harness.drag_end(below);
    harness.settle_work();

    let project = harness.project();
    assert_eq!(project.tracks.len(), 3);
    let track = &project.tracks[2];
    assert_eq!((track.name.as_str(), track.kind, track.devices.len()), ("Snare Tight", TrackKind::Normal, 0));
    assert_eq!(track.clips.len(), 1);
    assert_eq!(track.clips[0].start, 8.0);
    assert!(same_file(&track.clips[0].audio().unwrap().path, &pack.snare));
    assert_eq!(harness.app.state().selected_track, Some(track.id));

    // The track and its clip are one step.
    assert_eq!(harness.undo_depth(), undo + 1);
    harness.session.lock().undo();
    assert_eq!(harness.project().tracks.len(), 2);
}

#[test]
fn a_sound_dropped_on_an_empty_project_starts_the_song() {
    let pack = Pack::new();
    let mut harness = Harness::new(sounds_tab(), |_| {});
    harness.add_pack(&pack);
    let middle = pos2(harness.work_left() + 500.0, 300.0);
    harness.drop_sound("Drum Loop 120bpm F#m", middle);
    let project = harness.project();
    assert_eq!(project.tracks.len(), 1);
    let clip = &project.tracks[0].clips[0];
    assert_eq!((clip.start, clip.audio().unwrap().warp), (0.0, WarpMode::Beats), "a drum loop keeps its hits sharp");
    assert!((clip.length - 8.0).abs() < 1e-3);
    assert_eq!(harness.undo_depth(), 1);
}

#[test]
fn groups_and_returns_refuse_sounds_and_escape_cancels_a_drag() {
    let (mut harness, _pack, _) = with_pack(sounds_tab());
    let group = harness.session.lock().edit(|project| Ok(project.add_track_of_kind("Bus", TrackKind::Group))).unwrap();
    harness.idle(2);
    let before = harness.project();
    harness.carry_sound("Kick 01", harness.lane(4.0, 2));
    let drag = harness.app.state().sound_drag.clone().unwrap();
    assert_eq!(drag.target, None);
    assert!(drag.refusal.is_some_and(|refusal| refusal.contains("group")), "{:?}", drag.refusal);
    harness.drag_end(harness.lane(4.0, 2));
    harness.settle_work();
    assert_eq!(harness.project(), before, "nothing was dropped on {group}");

    // Over a lane that would take it, Escape puts the sound down.
    harness.carry_sound("Kick 01", harness.lane(4.0, 0));
    assert!(harness.app.state().sound_drag.as_ref().unwrap().target.is_some());
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(harness.app.state().sound_drag.is_none());
    harness.drag_end(harness.lane(4.0, 0));
    harness.settle_work();
    assert_eq!(harness.project(), before);
    // Let go over the browser itself: nothing either.
    harness.carry_sound("Kick 01", pos2(100.0, 400.0));
    harness.drag_end(pos2(100.0, 400.0));
    harness.settle_work();
    assert_eq!(harness.project(), before);
}

#[test]
fn a_double_click_adds_a_sound_at_the_edit_cursor() {
    let (mut harness, pack, tracks) = with_pack(sounds_tab());
    // The cursor and the selected track say where.
    harness.click(harness.lane(12.0, 1));
    harness.settle();
    let undo = harness.undo_depth();
    harness.double_click_label("Kick 01");
    harness.settle_work();
    let clips = harness.audio_clips(1);
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].start, 12.0);
    assert!(same_file(&clips[0].audio().unwrap().path, &pack.kick));
    assert_eq!(harness.undo_depth(), undo + 1);

    // The same from the row's menu, which also offers a track of its own.
    harness.settle();
    harness.right_click(harness.labeled("Snare Tight"));
    harness.click_label("Add to Two at the edit cursor");
    harness.settle_work();
    assert_eq!(harness.audio_clips(1).len(), 2);
    harness.right_click(harness.labeled("Snare Tight"));
    harness.click_label("Add to a new track");
    harness.settle_work();
    let project = harness.project();
    assert_eq!(project.tracks.len(), 3);
    assert_eq!(project.tracks[2].clips[0].start, 12.0);
    assert_eq!(project.track(tracks[0]).unwrap().clips.len(), 0);
}

#[test]
fn a_rows_menu_copies_the_path_and_shows_the_folder() {
    let (mut harness, pack, _) = with_pack(sounds_tab());
    harness.right_click(harness.labeled("Kick 01"));
    harness.click_label("Copy path");
    assert_eq!(harness.clipboard.as_deref(), Some(pack.kick.display().to_string().as_str()));
    harness.right_click(harness.labeled("Kick 01"));
    harness.click_label("Show only its folder");
    harness.settle_work();
    assert_eq!(harness.sound_browser().folder.as_deref(), pack.kick.parent());
    assert_eq!(harness.listed(), ["HH Closed", "Kick 01", "Snare Tight"]);
    harness.right_click(harness.labeled("Kick 01"));
    harness.click_label("Add to the favorites");
    assert!(harness.runtime.library.is_favorite(&pack.kick));
}

/// A project with an audio track, a sampler track and a drum rack track.
fn with_instruments(options: UiOptions) -> (Harness, Pack, [TrackId; 3], DeviceId, DeviceId) {
    let pack = Pack::new();
    let mut ids = None;
    let mut harness = Harness::new(UiOptions { browser_tab: Some(BrowserTab::Sounds), ..options }, |session| {
        let tracks = session
            .edit(|project| Ok([project.add_track("Audio"), project.add_track("Keys"), project.add_track("Kit")]))
            .unwrap();
        let sampler = session.add_device(ChainRef::Track(tracks[1]), sample_panels::SAMPLER, None, None).unwrap();
        let rack = session.add_device(ChainRef::Track(tracks[2]), sample_panels::DRUM_RACK, None, None).unwrap();
        ids = Some((tracks, sampler, rack));
    });
    harness.add_pack(&pack);
    let (tracks, sampler, rack) = ids.unwrap();
    (harness, pack, tracks, sampler, rack)
}

#[test]
fn a_sound_dropped_on_a_header_goes_to_the_tracks_instrument() {
    let (mut harness, pack, tracks, sampler, rack) = with_instruments(sounds_tab());
    harness.click(harness.lane(8.0, 0));
    harness.settle();
    let undo = harness.undo_depth();

    // A track without a sample player takes a clip at the edit cursor.
    let header = |harness: &Harness, row: usize| harness.header(row, 120.0, 20.0);
    harness.carry_sound("Kick 01", header(&harness, 0));
    assert_eq!(
        harness.app.state().sound_drag.as_ref().unwrap().target,
        Some(DropTarget::Lane { track: tracks[0], beat: 8.0 })
    );
    harness.drag_end(header(&harness, 0));
    harness.settle_work();
    assert_eq!(harness.audio_clips(0).len(), 1);
    assert_eq!(harness.audio_clips(0)[0].start, 8.0);

    // A sampler's track: the file becomes what it plays.
    harness.carry_sound("Pluck C3", header(&harness, 1));
    assert_eq!(harness.app.state().sound_drag.as_ref().unwrap().target, Some(DropTarget::Sampler { device: sampler }));
    harness.drag_end(header(&harness, 1));
    harness.settle_work();
    assert!(same_file(&harness.sampler_file(sampler).unwrap(), &pack.pluck));
    assert!(harness.project().track(tracks[1]).unwrap().clips.is_empty(), "no clip was made for it");

    // A drum rack's track: each sound goes to the pad its name asks for.
    harness.drop_sound("Kick 01", header(&harness, 2));
    harness.drop_sound("Snare Tight", header(&harness, 2));
    harness.drop_sound("HH Closed", header(&harness, 2));
    assert!(same_file(&harness.pad_file(rack, 36).unwrap(), &pack.kick));
    assert!(same_file(&harness.pad_file(rack, 38).unwrap(), &pack.snare));
    assert!(same_file(&harness.pad_file(rack, 42).unwrap(), &pack.hat));
    // What is not a drum takes the first pad that is free.
    harness.drop_sound("Pluck C3", header(&harness, 2));
    assert!(same_file(&harness.pad_file(rack, 37).unwrap(), &pack.pluck));
    assert!(harness.session.lock().problems().is_empty());
    assert_eq!(harness.undo_depth(), undo + 6, "six drops, six steps");
}

#[test]
fn sounds_are_dropped_on_the_sampler_and_on_single_pads() {
    let (mut harness, pack, tracks, sampler, rack) =
        with_instruments(UiOptions { detail_tab: DetailTab::Devices, ..sounds_tab() });
    // The sampler's panel.
    harness.click(harness.header(1, 120.0, 20.0));
    harness.idle(3);
    assert!(harness.is_on_screen("Sample waveform"));
    assert!(!harness.is_on_screen("Remove the sample"), "nothing is loaded yet");
    let undo = harness.undo_depth();
    let wave = harness.labeled("Sample waveform");
    harness.carry_sound("Pluck C3", wave);
    assert_eq!(harness.app.state().sound_drag.as_ref().unwrap().target, Some(DropTarget::Sampler { device: sampler }));
    harness.drag_end(wave);
    harness.settle_work();
    assert!(same_file(&harness.sampler_file(sampler).unwrap(), &pack.pluck));
    assert_eq!(harness.undo_depth(), undo + 1);
    // The panel now has its waveform and a way to take the file out.
    assert!(matches!(harness.wave(&pack.pluck), WaveState::Ready(_)));
    harness.click_label("Remove the sample");
    harness.settle_work();
    assert_eq!(harness.sampler_file(sampler), None);
    assert_eq!(harness.undo_depth(), undo + 2);
    harness.session.lock().undo();
    assert!(harness.sampler_file(sampler).is_some());

    // The drum rack: sixteen pads, each of which takes a sound.
    harness.click(harness.header(2, 120.0, 20.0));
    harness.idle(3);
    for note in drumrack::PAD_NOTES {
        let label = format!("Pad {}: empty", sample_panels::pad_title(note));
        assert!(harness.is_on_screen(&label), "{label}");
    }
    let undo = harness.undo_depth();
    let snare_pad = harness.labeled("Pad 38 Snare: empty");
    harness.carry_sound("Kick 01", snare_pad);
    assert_eq!(
        harness.app.state().sound_drag.as_ref().unwrap().target,
        Some(DropTarget::Pad { device: rack, note: 38 }),
        "the pad under the pointer, not the one the name would choose"
    );
    harness.drag_end(snare_pad);
    harness.settle_work();
    assert!(same_file(&harness.pad_file(rack, 38).unwrap(), &pack.kick));
    assert_eq!(harness.pad_file(rack, 36), None);
    assert_eq!(harness.undo_depth(), undo + 1);
    assert!(harness.is_on_screen("Pad 38 Snare: Kick 01"));
    assert_eq!(harness.app.state().selected_pads.get(&rack), Some(&38), "the pad that was filled is the one shown");
    // Beside the pads, on the panel: the pad the name asks for.
    let beside = harness.labeled("Pad 51 Ride: empty") + vec2(sample_panels::PAD.x, 0.0);
    harness.drop_sound("HH Closed", beside);
    assert!(same_file(&harness.pad_file(rack, 42).unwrap(), &pack.hat));
    let _ = tracks;
}

#[test]
fn files_from_another_program_land_like_sounds_from_the_browser() {
    let (mut harness, pack, tracks) = with_pack(UiOptions::default());
    let undo = harness.undo_depth();
    // Held over the window: a drag begins, though nobody says where.
    harness.step_with_files(Modifiers::NONE, Vec::new(), std::slice::from_ref(&pack.kick), &[]);
    let drag = harness.app.state().sound_drag.clone().expect("the files are being dragged");
    assert_eq!((drag.source, drag.label.as_str(), drag.pointer), (DragSource::System, "Kick 01", None));
    // Let go: the drop waits for the pointer to report, then lands there.
    harness.step_with_files(Modifiers::NONE, Vec::new(), &[], std::slice::from_ref(&pack.kick));
    assert!(harness.app.state().system_drop.is_some());
    assert!(harness.audio_clips(0).is_empty());
    harness.move_to(harness.lane(6.2, 0));
    harness.settle_work();
    let clips = harness.audio_clips(0);
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].start, 6.0);
    assert!(same_file(&clips[0].audio().unwrap().path, &pack.kick));
    assert!(harness.app.state().sound_drag.is_none() && harness.app.state().system_drop.is_none());
    assert_eq!(harness.undo_depth(), undo + 1);

    // Several files at once go one after the other, as one step.
    let files = [pack.snare.clone(), pack.bass_loop.clone()];
    harness.step_with_files(Modifiers::NONE, vec![Event::PointerMoved(harness.lane(16.0, 1))], &[], &files);
    harness.settle_work();
    let clips = harness.audio_clips(1);
    assert_eq!(clips.iter().map(|clip| clip.name.as_str()).collect::<Vec<_>>(), ["Snare Tight", "Bass Loop 100bpm"]);
    assert_eq!(clips[0].start, 16.0);
    assert!((clips[1].start - clips[0].end()).abs() < 1e-9, "the second starts where the first ends");
    assert_eq!(harness.undo_depth(), undo + 2);
    harness.session.lock().undo();
    assert!(harness.audio_clips(1).is_empty(), "both went with one undo");

    // If the pointer never reports, the files go to the selected track at
    // the edit cursor.
    harness.click(harness.lane(24.0, 1));
    harness.settle();
    harness.step(Modifiers::NONE, vec![Event::PointerGone]);
    harness.step_with_files(Modifiers::NONE, Vec::new(), &[], std::slice::from_ref(&pack.hat));
    harness.idle(30);
    harness.settle_work();
    let clips = harness.audio_clips(1);
    assert_eq!((clips.len(), clips[0].start), (1, 24.0));
    assert_eq!(harness.app.state().selected_track, Some(tracks[1]));

    // What is not audio is turned away with a word.
    let toasts = harness.app.state().toasts.len();
    let text = pack.root.join("notes.txt");
    std::fs::write(&text, "not a sound").unwrap();
    harness.step_with_files(Modifiers::NONE, Vec::new(), &[], &[text]);
    harness.settle_work();
    assert_eq!(harness.app.state().toasts.len(), toasts + 1);
    assert_eq!(harness.audio_clips(1).len(), 1);
}

#[test]
fn a_drop_that_fails_changes_nothing_and_says_why() {
    let (mut harness, pack, _) = with_pack(sounds_tab());
    let before = harness.project();
    let (toasts, undo) = (harness.app.state().toasts.len(), harness.undo_depth());
    std::fs::remove_file(&pack.kick).unwrap();
    harness.drop_sound("Kick 01", harness.lane(4.0, 0));
    assert_eq!(harness.project(), before);
    assert_eq!(harness.undo_depth(), undo);
    assert_eq!(harness.app.state().toasts.len(), toasts + 1);
    assert!(harness.app.sounds().pending.is_empty(), "the ghost goes when the drop has failed");
}

// ---- waveforms and where files are read ------------------------------------

/// A project with one audio clip per file of the pack, made the way a
/// drop makes them.
fn with_audio_clips(options: UiOptions) -> (Harness, Pack, TrackId, Vec<ClipId>) {
    let pack = Pack::new();
    let files = [pack.kick.clone(), pack.bass_loop.clone(), pack.drum_loop.clone()];
    let harness = Harness::new(options, |_| {});
    let track = harness.session.lock().edit(|project| Ok(project.add_track("Audio"))).unwrap();
    let clips = files
        .iter()
        .enumerate()
        .map(|(index, file)| harness.runtime.add_sound(&SoundClip::new(file, track, index as f64 * 8.0)).unwrap().clip)
        .collect();
    (harness, pack, track, clips)
}

#[test]
fn nothing_is_decoded_on_the_ui_thread() {
    let (mut harness, pack, track, clips) = with_audio_clips(sounds_tab());
    harness.app.sounds_mut().add_folder(pack.root.clone());
    harness.settle_work();
    assert!(harness.runtime.library.wait_until_idle(WAIT));
    harness.settle_work();
    // Every waveform on screen has arrived. Now forget all decoded audio
    // and every waveform, and stop the workers: from here on, whatever is
    // decoded is decoded by the thread that runs the window.
    assert!(matches!(harness.wave(&pack.kick), WaveState::Ready(_)));
    harness.runtime.media.clear();
    harness.app.sounds().hold(true);
    let before = harness.decodes();
    let project = harness.project();

    // Looking at files nobody has read: a sound chosen (and so previewed),
    // its strip, a clip in the Clip tab, a sound dragged over the lanes
    // and dropped, another dropped from outside, the view scrolled and
    // zoomed.
    let fresh = tone(pack.root.join("Drums").join("Never Read.wav"), 0.3);
    assert!(matches!(harness.wave(&fresh), WaveState::Loading));
    harness.click_label("Snare Tight");
    harness.key(Modifiers::NONE, Key::ArrowDown);
    harness.key(Modifiers::NONE, Key::Enter);
    harness.click(harness.lane(1.0, 0));
    harness.carry_sound("HH Closed", harness.lane(26.0, 0));
    harness.drag_end(harness.lane(26.0, 0));
    harness.step_with_files(
        Modifiers::NONE,
        vec![Event::PointerMoved(harness.lane(29.0, 0))],
        &[],
        std::slice::from_ref(&fresh),
    );
    harness.wheel(Modifiers::COMMAND, harness.lane(8.0, 0), 2.0);
    harness.wheel(Modifiers::NONE, harness.lane(8.0, 0), -3.0);
    harness.idle(30);

    assert_eq!(harness.decodes(), before, "the window decoded a file itself");
    assert_eq!(harness.project(), project, "the drops wait for their files");
    assert!(matches!(harness.wave(&fresh), WaveState::Loading), "a placeholder meanwhile");
    assert_eq!(harness.app.sounds().pending.len(), 2, "both drops show where they will land");
    assert!(harness.app.sounds().preview.playing.is_none() && harness.app.sounds().preview.loading.is_some());
    assert!(harness.app.sounds().is_busy());

    // Let the workers go: everything asked for arrives.
    harness.app.sounds().hold(false);
    harness.settle_work();
    assert!(harness.decodes() > before);
    assert!(matches!(harness.wave(&fresh), WaveState::Ready(_)));
    assert!(harness.app.sounds().pending.is_empty());
    assert_eq!(harness.audio_clips(0).len(), clips.len() + 2);
    assert!(harness.app.sounds().preview.playing.is_some());
    let _ = track;
}

#[test]
fn an_audio_clip_shows_a_placeholder_until_its_waveform_arrives() {
    let (mut harness, pack, _, _) = with_audio_clips(UiOptions::default());
    harness.runtime.media.clear();
    harness.app.sounds().hold(true);
    harness.idle(5);
    // In view and asked for, not yet there.
    for file in [&pack.kick, &pack.bass_loop, &pack.drum_loop] {
        assert!(matches!(harness.wave(file), WaveState::Loading));
    }
    harness.app.sounds().hold(false);
    harness.settle_work();
    let WaveState::Ready(wave) = harness.wave(&pack.bass_loop) else { panic!("the waveform arrived") };
    assert!((wave.seconds() - 2.4).abs() < 1e-3);
    // The decaying tone is loud at its start and quiet at its end.
    let columns = wave.columns(0.0..2.4, 8);
    assert!(columns[0].mixed.max > 0.5 && columns[7].mixed.max < 0.2, "{:?}", (columns[0].mixed, columns[7].mixed));
    // A file asked for a hundred times is read once.
    let decodes = harness.decodes();
    harness.idle(60);
    assert_eq!(harness.decodes(), decodes);
}

// ---- the end of the file ----------------------------------------------------

#[test]
fn an_audio_clip_cannot_be_dragged_past_the_end_of_its_file() {
    let (mut harness, _pack, _, clips) = with_audio_clips(UiOptions::default());
    harness.settle_work();
    // The kick: half a second, one beat at 120, at the start of the track.
    let kick = clips[0];
    assert!((harness.project().clip(kick).unwrap().length - 1.0).abs() < 1e-3);
    harness.drag(harness.lane(1.0, 0) - vec2(2.0, 0.0), harness.lane(6.0, 0));
    let length = harness.project().clip(kick).unwrap().length;
    assert!((length - 1.0).abs() < 1e-3, "the file ends there: {length}");

    // Trimmed shorter, it grows back as far as the file goes and no further.
    harness.session.lock().edit(|project| project.set_clip_span(kick, 0.0, 0.5)).unwrap();
    harness.idle(2);
    harness.drag_holding(Modifiers::ALT, harness.lane(0.5, 0) - vec2(2.0, 0.0), harness.lane(5.0, 0));
    let length = harness.project().clip(kick).unwrap().length;
    assert!((length - 1.0).abs() < 1e-3, "{length}");
    // Entered later, there is less of the file left to play.
    harness.session.lock().edit(|project| project.trim_clip_start(kick, 0.5).map(drop)).unwrap();
    harness.idle(2);
    harness.drag_holding(Modifiers::ALT, harness.lane(1.0, 0) - vec2(2.0, 0.0), harness.lane(5.0, 0));
    let clip = harness.project().clip(kick).unwrap().clone();
    assert!((clip.end() - 1.0).abs() < 1e-3, "{}", clip.end());

    // A warped clip ends where its recording does too: the four beats of
    // the loop recorded at 100.
    let bass = clips[1];
    harness
        .session
        .lock()
        .edit(|project| {
            project.set_clip_warp(bass, WarpMode::Beats, Some(100.0), 0.0)?;
            project.set_clip_span(bass, 8.0, 2.0)
        })
        .unwrap();
    harness.idle(2);
    harness.drag(harness.lane(10.0, 0) - vec2(2.0, 0.0), harness.lane(15.0, 0));
    let length = harness.project().clip(bass).unwrap().length;
    assert!((length - 4.0).abs() < 1e-3, "{length}");

    // The length field of the Clip tab stops there as well.
    harness.click(harness.lane(8.5, 0));
    harness.idle(3);
    let field = harness.labeled("Clip length");
    harness.drag(field, field - vec2(0.0, 400.0));
    let length = harness.project().clip(bass).unwrap().length;
    assert!(length <= 4.0 + 1e-3, "{length}");
    // A MIDI clip has no file to run out of.
    let midi =
        harness.session.lock().edit(|project| project.add_midi_clip(project.tracks[0].id, "m", 32.0, 1.0)).unwrap();
    harness.idle(2);
    harness.drag(harness.lane(33.0, 0) - vec2(2.0, 0.0), harness.lane(40.0, 0));
    assert_eq!(harness.project().clip(midi).unwrap().length, 8.0);
}

// ---- the audio clip panel ---------------------------------------------------

#[test]
fn the_clip_tab_shows_an_audio_clips_file_and_sets_gain_offset_and_fades() {
    let (mut harness, _pack, _, clips) = with_audio_clips(UiOptions::default());
    harness.settle_work();
    let drum = clips[2];
    harness.click(harness.lane(18.0, 0));
    harness.settle_work();
    assert_eq!(harness.app.state().selected_clip, Some(drum));
    for label in [
        "Gain",
        "Offset into the file",
        "Fade in",
        "Fade out",
        "Source tempo",
        "Transpose",
        "Off",
        "Repitch",
        "Beats",
        "Tonal",
    ] {
        assert!(harness.is_on_screen(label), "{label} is on the Clip tab");
    }
    assert!(!harness.is_on_screen("Quantize"), "an audio clip has no notes to quantize");
    let audio = |harness: &Harness| harness.project().clip(drum).unwrap().audio().unwrap().clone();

    // Gain: one drag of the bar, one undo step.
    let undo = harness.undo_depth();
    let gain = harness.labeled("Gain");
    harness.drag(gain, gain + vec2(40.0, 0.0));
    let louder = audio(&harness).gain_db;
    assert!(louder > 0.5, "{louder}");
    assert_eq!(harness.undo_depth(), undo + 1);
    // A double-click returns it to unity.
    harness.settle();
    harness.double_click(gain);
    assert_eq!(audio(&harness).gain_db, 0.0);

    // Offset: the clip enters the file later.
    let undo = harness.undo_depth();
    let offset = harness.labeled("Offset into the file");
    harness.drag(offset, offset - vec2(0.0, 60.0));
    let entered = audio(&harness).offset_seconds;
    assert!(entered > 0.1 && entered < 4.0, "{entered}");
    assert_eq!(harness.undo_depth(), undo + 1);

    // Fades, in beats, and never longer than the clip between them.
    let fade_in = harness.labeled("Fade in");
    harness.drag(fade_in, fade_in - vec2(0.0, 50.0));
    assert!((audio(&harness).fade_in - 1.0).abs() < 1e-6, "{}", audio(&harness).fade_in);
    let fade_out = harness.labeled("Fade out");
    harness.drag(fade_out, fade_out - vec2(0.0, 2000.0));
    let faded = audio(&harness);
    let length = harness.project().clip(drum).unwrap().length;
    assert!((faded.fade_in + faded.fade_out - length).abs() < 1e-6, "{faded:?}");
}

#[test]
fn warping_is_set_from_the_clip_tab_and_shows_while_it_is_prepared() {
    let pack = Pack::new();
    // Long enough that stretching it takes a moment.
    let long = tone(pack.root.join("Loops").join("Long Loop 100bpm.wav"), 24.0);
    let harness = Harness::new(UiOptions::default(), |_| {});
    let track = harness.session.lock().edit(|project| Ok(project.add_track("Audio"))).unwrap();
    let clip = harness.runtime.add_sound(&SoundClip::new(&long, track, 0.0)).unwrap().clip;
    let mut harness = harness;
    harness.settle_work();
    harness.click(harness.lane(2.0, 0));
    harness.settle_work();
    let read = |harness: &Harness| harness.project().clip(clip).unwrap().clone();
    assert!((read(&harness).length - 48.0).abs() < 1e-3, "24 s at 120 BPM");

    // Beats: it follows the tempo by the tempo in its name, and becomes as
    // many beats long as it has beats in it.
    let undo = harness.undo_depth();
    harness.click_label("Beats");
    let warped = read(&harness);
    let audio = warped.audio().unwrap();
    assert_eq!((audio.warp, audio.source_tempo), (WarpMode::Beats, Some(100.0)));
    assert!((warped.length - 40.0).abs() < 1e-3, "24 s at 100 BPM is forty beats: {}", warped.length);
    assert_eq!(harness.undo_depth(), undo + 1, "the mode and the new length are one step");
    // Until the stretched copy exists, the engine plays a stand-in and the
    // window says so.
    harness.idle(1);
    assert_eq!(harness.app.live().preparing, 1);
    assert!(harness.session.lock().wait_until_prepared(WAIT));
    harness.idle(2);
    assert_eq!(harness.app.live().preparing, 0);
    assert!(harness.session.lock().problems().is_empty());

    // The source tempo: halved, the same audio is half as many beats.
    harness.click_label("\u{00F7}2");
    let halved = read(&harness);
    assert_eq!(halved.audio().unwrap().source_tempo, Some(50.0));
    assert!((halved.length - 20.0).abs() < 1e-3, "{}", halved.length);
    harness.click_label("\u{00D7}2");
    assert_eq!(read(&harness).audio().unwrap().source_tempo, Some(100.0));
    // Dragged, in tenths.
    let source = harness.labeled("Source tempo");
    harness.drag(source, source - vec2(0.0, 30.0));
    let tempo = read(&harness).audio().unwrap().source_tempo.unwrap();
    assert!((tempo - 103.0).abs() < 1e-6, "{tempo}");

    // Transposing works in the modes that keep pitch.
    let transpose = harness.labeled("Transpose");
    harness.drag(transpose, transpose + vec2(25.0, 0.0));
    let shifted = read(&harness).audio().unwrap().transpose;
    assert!(shifted >= 1.0 && shifted.fract() == 0.0, "whole semitones: {shifted}");
    // Repitch ties pitch to speed: the bar is there but does nothing.
    harness.click_label("Repitch");
    assert_eq!(read(&harness).audio().unwrap().warp, WarpMode::Repitch);
    harness.settle();
    harness.drag(transpose, transpose - vec2(40.0, 0.0));
    assert_eq!(read(&harness).audio().unwrap().transpose, shifted);

    // Off again: the clip is as long as its seconds at the song's tempo.
    harness.click_label("Off");
    let plain = read(&harness);
    assert_eq!(plain.audio().unwrap().warp, WarpMode::Off);
    assert!(harness.session.lock().wait_until_prepared(WAIT));
}

#[test]
fn a_clip_without_a_tempo_in_its_name_assumes_the_songs_when_warped() {
    let (mut harness, _pack, _, clips) = with_audio_clips(UiOptions::default());
    harness.settle_work();
    let kick = clips[0];
    harness.click(harness.lane(0.5, 0));
    harness.settle_work();
    let toasts = harness.app.state().toasts.len();
    harness.click_label("Tonal");
    let audio = harness.project().clip(kick).unwrap().audio().unwrap().clone();
    assert_eq!((audio.warp, audio.source_tempo), (WarpMode::Tonal, Some(120.0)));
    assert!((harness.project().clip(kick).unwrap().length - 1.0).abs() < 1e-3, "nothing moves until it is corrected");
    assert_eq!(harness.app.state().toasts.len(), toasts + 1, "and the user is told what was assumed");
}

#[test]
fn a_clip_whose_file_is_missing_says_so_and_can_be_pointed_at_it_again() {
    let (mut harness, pack, _, clips) = with_audio_clips(UiOptions::default());
    harness.settle_work();
    // The file goes away under the project, as when a folder is moved.
    let moved = pack.root.join("Elsewhere").join("Kick 01.wav");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&pack.kick, &moved).unwrap();
    harness.runtime.media.clear();
    let kick = clips[0];
    // Any edit recompiles the song, which finds the file gone.
    harness.session.lock().edit(|project| project.set_clip_fades(kick, 0.1, 0.0)).unwrap();
    harness.click(harness.lane(0.5, 0));
    harness.settle_work();
    // The engine found the file gone, and so the waveform that was read
    // while it was there has been looked at again and let go.
    assert!(harness.app.snapshot().clip_problems.contains_key(&kick));
    assert!(harness.wave(&pack.kick).failure().is_some_and(|reason| reason.contains("does not exist")));
    assert!(harness.is_on_screen("Locate the file\u{2026}"));

    // Pointed at where it is now, the clip plays again, as one undo step.
    let undo = harness.undo_depth();
    harness.app.sounds_mut().relink(kick, moved.clone());
    harness.settle_work();
    let clip = harness.project().clip(kick).unwrap().clone();
    assert!(same_file(&clip.audio().unwrap().path, &moved));
    assert_eq!(clip.audio().unwrap().fade_in, 0.1, "everything else about the clip is kept");
    assert_eq!(harness.undo_depth(), undo + 1);
    assert!(harness.session.lock().problems().is_empty());
    assert!(!harness.is_on_screen("Locate the file\u{2026}"));
    assert!(matches!(harness.wave(&moved), WaveState::Ready(_)));
    // A file that is not audio is refused and the clip left alone.
    let text = pack.root.join("notes.txt");
    std::fs::write(&text, "not a sound").unwrap();
    harness.app.sounds_mut().relink(kick, text);
    harness.settle_work();
    assert!(same_file(&harness.project().clip(kick).unwrap().audio().unwrap().path, &moved));
}

// ---- the sampler and the drum rack ------------------------------------------

#[test]
fn the_drum_rack_shows_its_pads_and_the_controls_of_the_selected_one() {
    let (mut harness, pack, tracks, _, rack) =
        with_instruments(UiOptions { detail_tab: DetailTab::Devices, ..sounds_tab() });
    let kick = pack.kick.display().to_string();
    harness.session.lock().set_pad_file(rack, 36, Some(&kick)).unwrap();
    harness.session.lock().set_pad_file(rack, 42, Some(&pack.hat.display().to_string())).unwrap();
    harness.click(harness.header(2, 120.0, 20.0));
    harness.settle_work();
    assert!(harness.is_on_screen("Pad 36 Kick: Kick 01") && harness.is_on_screen("Pad 42 Closed Hat: HH Closed"));
    assert!(harness.is_on_screen("Pad 51 Ride: empty"));

    // Eight bars for the whole rack, not ninety-eight: the six of the
    // selected pad and the rack's two.
    let bars: Vec<&str> = harness.values.iter().map(|(label, _)| label.as_str()).collect();
    // What comes after the track headers and the panels' own handles.
    let panel = bars.rsplit(|label| *label == "Resize panel").next().unwrap();
    assert_eq!(
        panel,
        ["Volume", "Velocity", "Level", "Pan", "Tune", "Start", "Decay", "Choke", "Preview volume"],
        "{bars:?}"
    );

    // A click selects a pad and plays it through the track.
    harness.click_label("Pad 42 Closed Hat: HH Closed");
    assert_eq!(harness.app.state().selected_pads.get(&rack), Some(&42));
    let audition = harness.app.state().audition.expect("the pad is heard");
    assert_eq!((audition.track, audition.pitch), (tracks[2], 42));
    // Its controls are the ones shown: the level bar sets pad 42's level.
    harness.settle();
    let level = harness.labeled("Level");
    let undo = harness.undo_depth();
    harness.drag(level, level - vec2(30.0, 0.0));
    let project = harness.project();
    let device = project.device(rack).unwrap();
    assert!(device.params.get("pad42_level").is_some_and(|level| *level < 1.0), "{:?}", device.params);
    assert!(!device.params.contains_key("pad36_level"));
    assert_eq!(harness.undo_depth(), undo + 1);

    // The cross on a pad under the pointer takes its sound away.
    harness.move_to(harness.labeled("Pad 36 Kick: Kick 01"));
    harness.idle(2);
    let undo = harness.undo_depth();
    harness.click_label("Clear pad 36");
    harness.settle_work();
    assert_eq!(harness.pad_file(rack, 36), None);
    assert!(harness.pad_file(rack, 42).is_some());
    assert_eq!(harness.undo_depth(), undo + 1);
    // And the menu does the same.
    harness.right_click(harness.labeled("Pad 42 Closed Hat: HH Closed"));
    harness.click_label("Clear the pad");
    harness.settle_work();
    assert_eq!(harness.pad_file(rack, 42), None);
}

#[test]
fn the_sampler_shows_its_file_and_plays_it() {
    let (mut harness, pack, _, sampler, _) =
        with_instruments(UiOptions { detail_tab: DetailTab::Devices, ..sounds_tab() });
    harness.session.lock().set_sampler_file(sampler, Some(&pack.pluck.display().to_string())).unwrap();
    harness.click(harness.header(1, 120.0, 20.0));
    harness.settle_work();
    assert!(harness.is_on_screen("Remove the sample") && harness.is_on_screen("Load"));
    assert!(matches!(harness.wave(&pack.pluck), WaveState::Ready(_)), "its waveform is drawn");
    // The parameters are still all there beside it.
    assert!(harness.values.iter().any(|(label, _)| label == "Cutoff"));
    // A click on the waveform plays the file.
    harness.click_label("Sample waveform");
    harness.settle_work();
    assert_eq!(harness.app.sounds().preview.playing.as_ref().map(|(path, ..)| path.clone()), Some(pack.pluck.clone()));
    // "In browser" chooses it there, to hear it or drag it elsewhere.
    harness.click_label("In browser");
    harness.settle_work();
    assert_eq!(harness.sound_browser().selected.as_deref(), Some(pack.pluck.as_path()));
    assert_eq!(harness.sound_browser().selected_row, Some(3), "found in the list");
}

#[test]
fn a_missing_sample_is_reported_on_its_pad() {
    let (mut harness, pack, _, _, rack) =
        with_instruments(UiOptions { detail_tab: DetailTab::Devices, ..sounds_tab() });
    harness.session.lock().set_pad_file(rack, 38, Some(&pack.snare.display().to_string())).unwrap();
    std::fs::remove_file(&pack.snare).unwrap();
    harness.runtime.media.clear();
    // Loading the project again is when the rack looks for its files.
    let file = pack.root.join("song.trill");
    harness.session.lock().save(&file).unwrap();
    harness.session.lock().load(&file).unwrap();
    harness.app.state_mut().reset_for_new_project();
    harness.idle(3);
    harness.click(harness.header(2, 120.0, 20.0));
    harness.settle_work();
    let rack = harness.project().tracks[2].devices[0].id;
    let files = harness.app.snapshot().samples.get(&rack).cloned().expect("the rack's files are known");
    assert_eq!(files.len(), 1);
    assert!(files[0].problem.as_deref().is_some_and(|problem| problem.contains("does not exist")), "{files:?}");
    assert!(harness.is_on_screen("Pad 38 Snare: Snare Tight"));
}

// ---- measurements -----------------------------------------------------------

#[test]
fn searching_the_library_matches_what_the_library_finds() {
    let (mut harness, _pack, _) = with_pack(sounds_tab());
    harness.app.state_mut().browser.sounds.search = "loop".into();
    harness.settle_work();
    let direct = harness.runtime.library.search(&Query { text: "loop".into(), ..audio_clip_query() });
    let names: Vec<String> = direct.entries().map(|entry| entry.name.clone()).collect();
    assert_eq!(harness.listed(), names);
    let _ = audio_clip::TOOLBAR;
}

fn audio_clip_query() -> Query {
    Query { kind: Some(trill_library::EntryKind::Audio), ..Query::default() }
}
