//! Sampled instruments in the window: the library's one row for each, the
//! ways one reaches a track, and the panel that shows what a Sampled
//! Instrument plays.
//!
//! Each test writes a small instrument of its own (an SFZ file and a few
//! tones) into a temporary folder: nothing of anyone's is read.

use super::sounds::{WAIT, tone};
use super::*;
use crate::ui::instrument_panel::SAMPLED;
use crate::ui::sample_panels;
use crate::ui::sounds::DropTarget;
use crate::ui::state::BrowserTab;
use std::path::PathBuf;
use trill_library::{EntryKind, Query};

/// A folder of instruments, gone when this is dropped.
struct Shelf {
    _folder: tempfile::TempDir,
    root: PathBuf,
    /// Two layers of loudness low down, one above, and a second way of
    /// playing chosen by a key.
    piano: PathBuf,
    /// One recording over five octaves.
    organ: PathBuf,
    /// Names a recording that is not there.
    broken: PathBuf,
    /// A sound that belongs to no instrument.
    clap: PathBuf,
}

const PIANO: &str = "<control> default_path=Samples/
<global> ampeg_release=0.3 sw_lokey=24 sw_hikey=25 sw_default=24
<group> sw_last=24 sw_label=natural
<region> sample=low soft.wav lokey=36 hikey=47 pitch_keycenter=45 hivel=80
<region> sample=low hard.wav lokey=36 hikey=47 pitch_keycenter=45 lovel=81
<region> sample=high.wav lokey=48 hikey=72 pitch_keycenter=57
<group> sw_last=25 sw_label=muted
<region> sample=muted.wav lokey=36 hikey=72 pitch_keycenter=57
";

impl Shelf {
    fn new() -> Self {
        let folder = tempfile::tempdir().unwrap();
        // Spelled as the library spells it, so paths compare equal.
        let root = std::path::absolute(folder.path().join("Instruments")).unwrap();
        for name in ["low soft", "low hard", "high", "muted"] {
            tone(root.join("Small Piano").join("Samples").join(format!("{name}.wav")), 0.4);
        }
        tone(root.join("Organ").join("pipe.wav"), 0.5);
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path
        };
        Self {
            piano: write(root.join("Small Piano").join("Small Piano.sfz"), PIANO),
            organ: write(
                root.join("Organ").join("Reed Organ.sfz"),
                "<region> sample=pipe.wav lokey=36 hikey=96 pitch_keycenter=57\n",
            ),
            broken: write(root.join("Broken").join("Half There.sfz"), "<region> sample=gone.wav key=60\n"),
            clap: tone(root.join("Loose").join("Clap.wav"), 0.2),
            root,
            _folder: folder,
        }
    }
}

impl Harness {
    /// Adds the shelf's folder to the library and waits for the scan and
    /// for the browser to have its list.
    fn add_shelf(&mut self, shelf: &Shelf) {
        self.app.sounds_mut().add_folder(shelf.root.clone());
        self.settle_work();
        assert!(self.runtime.library.wait_until_idle(WAIT));
        self.settle_work();
    }

    /// The device the track in `row` plays through.
    fn instrument_of(&self, row: usize) -> trill_core::Device {
        let project = self.project();
        let track = project.tracks[row].id;
        self.session.lock().instrument_of(track).cloned().expect("the track has an instrument")
    }

    /// Presses on the browser's row called `name`, carries it to `to`
    /// and lets go there.
    fn drop_row(&mut self, name: &str, to: Pos2) {
        self.idle(2);
        let from = self.labeled(name);
        self.drag_start(Modifiers::NONE, from, to);
        self.idle(1);
        self.drag_end(to);
        self.settle_work();
    }
}

/// A song with a synth track and a sampler track, the Devices tab open
/// below and the shelf in the library.
fn with_shelf() -> (Harness, Shelf, [TrackId; 2]) {
    let shelf = Shelf::new();
    let mut ids = None;
    let options = UiOptions { detail_tab: DetailTab::Devices, ..Default::default() };
    let mut harness = Harness::new(options, |session| {
        let tracks = session.edit(|project| Ok([project.add_track("Keys"), project.add_track("Pad")])).unwrap();
        session.add_device(ChainRef::Track(tracks[0]), "builtin:polysynth", None, None).unwrap();
        session.add_device(ChainRef::Track(tracks[1]), sample_panels::SAMPLER, None, None).unwrap();
        ids = Some(tracks);
    });
    harness.add_shelf(&shelf);
    (harness, shelf, ids.unwrap())
}

#[test]
fn an_instrument_of_the_library_is_one_row_that_a_track_takes_and_its_panel_shows() {
    let (mut harness, shelf, tracks) = with_shelf();
    assert_eq!(harness.app.state().browser.tab, BrowserTab::Devices);
    // One row for each instrument, with the devices; its recordings are
    // not sounds of the library to be found one by one.
    assert!(harness.is_on_screen("Small Piano") && harness.is_on_screen("Reed Organ"));
    let sounds = harness.runtime.library.search(&Query { kind: Some(EntryKind::Audio), ..Query::default() });
    let paths: Vec<PathBuf> = sounds.entries().map(|entry| entry.path.clone()).collect();
    assert_eq!(paths, std::slice::from_ref(&shelf.clap), "the instruments' recordings are left out");

    // Carried over a track's header it is offered as the track's instrument.
    let undo = harness.undo_depth();
    let header = harness.header(0, 120.0, 20.0);
    let row = harness.labeled("Small Piano");
    harness.drag_start(Modifiers::NONE, row, header);
    harness.idle(1);
    let drag = harness.app.state().sound_drag.clone().expect("the instrument is in the air");
    assert_eq!(drag.target, Some(DropTarget::TrackInstrument { track: tracks[0] }));
    assert!(harness.hint().contains("the instrument of Keys"), "{}", harness.hint());
    harness.drag_end(header);
    harness.settle_work();

    // It took the synth's place, as one step, and is what the panel shows.
    let device = harness.instrument_of(0);
    assert_eq!(device.uri, SAMPLED);
    assert_eq!(harness.project().tracks[0].devices.len(), 1, "the synth made way for it");
    assert_eq!(harness.undo_depth(), undo + 1);
    assert_eq!(harness.app.state().selected_device, Some(device.id));
    let said = harness.session.lock().sampled(device.id).expect("the device says what it plays");
    assert_eq!((said.name.as_str(), said.recordings, said.layers), ("Small Piano", 4, 2));
    assert!(std::path::Path::new(&said.file) == shelf.piano, "{}", said.file);
    harness.idle(3);
    assert!(harness.is_on_screen("Instrument Small Piano: 4 recordings"), "the map says what it is a map of");
    let shown = &harness.app.snapshot().instruments[&device.id];
    assert_eq!(shown.boxes.len(), 4, "a box for each region");
    assert_eq!(shown.switches, [24, 25], "the keys that choose how it is played");
    assert!(shown.problems.is_empty());
    assert!(harness.is_on_screen("Volume") && harness.is_on_screen("Dynamics"), "its parameters follow the map");
    assert!(harness.session.lock().problems().is_empty());

    // Below the tracks it gets a track of its own, named after it.
    let below = pos2(harness.lane(8.2, 0).x, harness.row_top(2) + 24.0);
    harness.drop_row("Reed Organ", below);
    let project = harness.project();
    assert_eq!(project.tracks.len(), 3);
    assert_eq!(project.tracks[2].name, "Reed Organ");
    assert_eq!(harness.instrument_of(2).uri, SAMPLED);
    assert_eq!(harness.app.state().selected_track, Some(project.tracks[2].id));
    assert_eq!(harness.undo_depth(), undo + 2, "the track and its instrument are one step");

    // On a track that plays through a Sampled Instrument already, the
    // device is given the other instrument: nothing is added.
    harness.drop_row("Reed Organ", harness.header(0, 120.0, 20.0));
    assert_eq!(harness.instrument_of(0).id, device.id, "the same device");
    assert_eq!(harness.session.lock().sampled(device.id).unwrap().name, "Reed Organ");
    harness.session.lock().undo();
    assert_eq!(harness.session.lock().sampled(device.id).unwrap().name, "Small Piano");

    // A sampler's track takes it too: the sampler makes way.
    harness.drop_row("Small Piano", harness.header(1, 120.0, 20.0));
    assert_eq!(harness.instrument_of(1).uri, SAMPLED);
    harness.session.lock().undo();
    assert_eq!(harness.instrument_of(1).uri, sample_panels::SAMPLER, "undo brings the sampler back");
}

#[test]
fn what_a_sampled_instrument_cannot_take_is_refused_and_what_is_wrong_with_one_is_said() {
    let (mut harness, shelf, tracks) = with_shelf();
    harness.drop_row("Small Piano", harness.header(0, 120.0, 20.0));
    let device = harness.instrument_of(0).id;
    harness.idle(3);
    let map = harness.labeled("Instrument Small Piano");

    // A single sound is not an instrument: the panel says so and takes nothing.
    harness.click_label("Sounds");
    harness.settle_work();
    let before = harness.project();
    let clap = harness.labeled("Clap");
    harness.drag_start(Modifiers::NONE, clap, map);
    harness.idle(1);
    let drag = harness.app.state().sound_drag.clone().unwrap();
    assert_eq!(drag.target, None);
    assert!(drag.refusal.is_some_and(|why| why.contains("not a single sound")), "{:?}", drag.refusal);
    harness.drag_end(map);
    harness.settle_work();
    assert_eq!(harness.project(), before);
    assert!(std::path::Path::new(&harness.session.lock().sampled(device).unwrap().file) == shelf.piano);

    // And an instrument is not a sound: a sampler's panel turns it away.
    harness.click_label("Devices");
    harness.click(harness.header(1, 120.0, 20.0));
    harness.idle(3);
    let wave = harness.labeled("Sample waveform");
    let row = harness.labeled("Reed Organ");
    harness.drag_start(Modifiers::NONE, row, wave);
    harness.idle(1);
    let drag = harness.app.state().sound_drag.clone().unwrap();
    assert_eq!(drag.target, None);
    assert!(drag.refusal.is_some_and(|why| why.contains("play one sound")), "{:?}", drag.refusal);
    harness.drag_end(wave);
    harness.settle_work();
    assert_eq!(harness.project(), before);

    // A file another program lets go of over the panel is what the device plays next.
    harness.click(harness.header(0, 120.0, 20.0));
    harness.idle(3);
    let map = harness.labeled("Instrument Small Piano");
    let undo = harness.undo_depth();
    harness.step_with_files(Modifiers::NONE, vec![Event::PointerMoved(map)], &[], std::slice::from_ref(&shelf.organ));
    harness.settle_work();
    assert_eq!(harness.session.lock().sampled(device).unwrap().name, "Reed Organ");
    assert_eq!(harness.undo_depth(), undo + 1);
    assert_eq!(harness.project().tracks[0].id, tracks[0]);

    // An instrument whose recordings are not all there still loads, and
    // the panel says what is missing where its reading would be.
    harness.step_with_files(
        Modifiers::NONE,
        vec![Event::PointerMoved(harness.labeled("Instrument Reed Organ"))],
        &[],
        std::slice::from_ref(&shelf.broken),
    );
    harness.settle_work();
    harness.idle(3);
    assert_eq!(harness.session.lock().sampled(device).unwrap().name, "Half There");
    let problems = harness.app.snapshot().instruments[&device].problems.clone();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("gone.wav"), "{problems:?}");
    assert!(harness.is_on_screen(&problems[0]), "the panel says it");
}
