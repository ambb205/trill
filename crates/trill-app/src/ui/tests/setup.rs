//! The Setup panel: what Trill fetches when asked, and what runs the
//! models. On parts that exist only here, fetched from a shelf of the
//! test's own: nothing is asked of a network, nothing of the user's is
//! read or written, and no model is run.

use super::*;
use crate::ui::setup::Kept;
use trill_engine::AudioBackend;
use trill_fetch::scripted::{Shelf, part};
use trill_runtime::{Parts, Storage, Studying};
use trill_study::models::scripted::Script;
use trill_study::run::Programs;

const DOT: &str = "\u{00B7}";

struct Fixture {
    harness: Harness,
    shelf: Arc<Shelf>,
    /// Holds the settings, what is fetched and the studies.
    folder: tempfile::TempDir,
}

/// The window on a runtime that keeps its settings in a folder of the
/// test's, with two parts published on a shelf.
fn fixture() -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let shelf = Arc::new(Shelf::default());
    let mut beats = part(&shelf, "beats", "Beats", &[("beats.onnx", vec![4u8; 1 << 20])]);
    beats.credit = "Made by Somebody, who asks to be named.".to_owned();
    beats.needs = vec!["stems".to_owned()];
    let studying = Studying {
        fetcher: Some(shelf.clone()),
        parts: Some(vec![part(&shelf, "stems", "Stems", &[("stems.onnx", vec![3u8; 3 << 20])]), beats]),
        stand_in: Some(Script::default()),
        programs: Some(Programs::default()),
        program: None,
        free: None,
    };
    let storage = Storage::At { settings: folder.path().join("config").join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let runtime = Arc::new(Runtime::start_custom(options, Parts { studying, ..Parts::default() }).unwrap());
    let harness = Harness::on(runtime, UiOptions::default(), |_| {});
    Fixture { harness, shelf, folder }
}

impl Harness {
    /// Waits until the Setup panel has read what it shows and nothing it
    /// was asked for is still being done.
    fn settle_setup(&mut self) {
        self.wait_until("the Setup panel to settle", |harness| !harness.app.setup().is_busy());
        self.idle(3);
        self.wait_until("the Setup panel to settle", |harness| !harness.app.setup().is_busy());
        self.idle(2);
    }

    fn open_setup(&mut self) {
        self.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Comma);
        assert!(self.app.setup().is_open());
        self.settle_setup();
    }

    /// Waits until a widget labeled `label` is on screen.
    fn wait_for_label(&mut self, label: &str) {
        let wanted = label.to_owned();
        self.wait_until(&format!("'{label}'"), move |harness| harness.is_on_screen(&wanted));
    }
}

#[test]
fn the_panel_lists_what_there_is_to_fetch_with_whose_it_is_and_fetches_only_what_is_asked_for() {
    let Fixture { mut harness, shelf, folder } = fixture();
    harness.open_setup();
    // Each part, its size and where it is; the first is the one looked at.
    assert!(harness.is_on_screen(&format!("Stems {DOT} 3 MB {DOT} Not here")));
    assert!(harness.is_on_screen(&format!("Beats {DOT} 1 MB {DOT} Not here")));
    let kept = folder.path().join("config").join("models");
    assert!(harness.is_on_screen(&format!("Kept in {}", kept.display())));
    // Everything there is to read of it stands over the button that fetches it.
    assert!(harness.is_on_screen("A part made up for a test."));
    assert!(harness.is_on_screen("Licence: Nobody's: it was made up for a test."));
    assert!(harness.is_on_screen("From: shelf.test"));
    assert!(harness.is_on_screen("Checked by: SHA-256 "));
    assert!(harness.is_on_screen(&format!("Fetch {DOT} 3 MB")));
    // Opening the panel and reading it fetched nothing.
    assert!(shelf.asked().is_empty());

    // Another part: who asks to be named for it, and what it is no use without.
    harness.click_label(&format!("Beats {DOT} 1 MB {DOT} Not here"));
    harness.idle(2);
    assert!(harness.is_on_screen("Credit: Made by Somebody, who asks to be named."));
    assert!(harness.is_on_screen("Needs: Stems, which is not here"));
    harness.click_label(&format!("Fetch {DOT} 1 MB"));
    harness.wait_for_label(&format!("Beats {DOT} 1 MB {DOT} Here"));
    assert_eq!(shelf.asked(), ["https://shelf.test/beats/beats.onnx"]);
    assert_eq!(std::fs::read(kept.join("beats").join("beats.onnx")).unwrap().len(), 1 << 20);
    assert!(harness.is_on_screen(&format!("Stems {DOT} 3 MB {DOT} Not here")), "only what was asked for");

    // And removed again, with a word on the room that made.
    harness.settle_setup();
    harness.click_label("Remove");
    harness.wait_for_label(&format!("Beats {DOT} 1 MB {DOT} Not here"));
    harness.settle_setup();
    assert_eq!(harness.app.setup().last(), Some("Beats was removed: 1 MB of room made."));
    assert!(harness.is_on_screen("Beats was removed: 1 MB of room made."));
    assert!(!kept.join("beats").exists());

    // Escape puts the panel away.
    harness.key(Modifiers::NONE, Key::Escape);
    assert!(!harness.app.setup().is_open());
}

#[test]
fn a_fetch_that_fails_says_why_under_its_part_and_is_gone_on_with_and_one_can_be_stopped() {
    let Fixture { mut harness, shelf, folder } = fixture();
    harness.open_setup();
    // The connection is lost after a part of it.
    shelf.cut_after(1 << 20);
    harness.click_label(&format!("Fetch {DOT} 3 MB"));
    harness.wait_for_label(&format!("Stems {DOT} 3 MB {DOT} Could not be fetched"));
    harness.settle_setup();
    assert!(harness.is_on_screen(
        "stems.onnx could not be fetched from shelf.test: the connection was lost. What came of it is kept, and \
         fetching it again goes on from there."
    ));
    // What came is kept, and the button goes on from there.
    assert!(harness.is_on_screen(&format!("Go on {DOT} 2 MB")));
    harness.click_label(&format!("Go on {DOT} 2 MB"));
    harness.wait_for_label(&format!("Stems {DOT} 3 MB {DOT} Here"));
    assert!(shelf.began()[1] >= 1 << 20, "{:?}", shelf.began());
    assert_eq!(
        std::fs::read(folder.path().join("config").join("models").join("stems").join("stems.onnx")).unwrap().len(),
        3 << 20
    );

    // A fetch that is stopped keeps what came, and says so.
    harness.settle_setup();
    shelf.slow(Duration::from_millis(25));
    harness.click_label(&format!("Beats {DOT} 1 MB {DOT} Not here"));
    harness.click_label(&format!("Fetch {DOT} 1 MB"));
    harness.wait_until("the fetch to be under way", |harness| {
        harness.is_on_screen(&format!("Beats {DOT} 1 MB {DOT} Fetching"))
    });
    // The panel's own, not the transport's behind it.
    harness.click_last_label("Stop");
    harness
        .wait_until("the fetch to stop", |harness| harness.is_on_screen(&format!("Beats {DOT} 1 MB {DOT} Stopped at")));
    harness.settle_setup();
    assert_eq!(harness.app.setup().last(), Some("What came of Beats is kept: fetching it again goes on from there."));
    assert!(!folder.path().join("config").join("models").join("beats").join("beats.onnx").exists());
}

#[test]
fn the_machines_tab_says_what_runs_the_models_which_programs_were_found_and_where_studies_are_kept() {
    let Fixture { mut harness, shelf: _, folder } = fixture();
    harness.open_setup();
    harness.click_label("This machine");
    harness.idle(2);
    assert!(harness.is_on_screen("The models run on the processor"));
    // The programs Trill does not ship, and how to come by one that is not here.
    assert!(harness.is_on_screen(&format!("ffmpeg {DOT} Not found {DOT} ")));
    assert!(harness.is_on_screen(&format!("yt-dlp {DOT} Not found {DOT} ")));
    let studies = folder.path().join("config").join("studies");
    assert!(harness.is_on_screen(&format!("Kept in {}", studies.display())));

    // Studies are kept elsewhere when the user says where; the settings remember it.
    let elsewhere = folder.path().join("big disk").join("studies");
    harness.app.keep_in(&harness.ctx, Kept::Studies, elsewhere.clone());
    harness.settle_setup();
    let said = harness.app.setup().last().unwrap().to_owned();
    assert_eq!(said, format!("Studies are kept in {} from now on", elsewhere.display()));
    assert!(harness.is_on_screen(&said));
    harness.wait_for_label(&format!("Kept in {}", elsewhere.display()));
    assert_eq!(harness.runtime.settings().studies.folder, Some(elsewhere));
    // And so are the parts, from their own tab.
    let parts = folder.path().join("big disk").join("models");
    harness.app.keep_in(&harness.ctx, Kept::List("models".to_owned()), parts.clone());
    harness.settle_setup();
    harness.click_label("Studies");
    harness.wait_for_label(&format!("Kept in {}", parts.display()));
    assert_eq!(harness.runtime.settings().fetched.get("models"), Some(&parts));
}

#[test]
fn a_run_that_keeps_nothing_says_so_in_the_panel() {
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    // From the File menu, as from its key.
    harness.click_label("File");
    harness.click_label("Setup\u{2026}");
    assert!(harness.app.setup().is_open());
    harness.settle_setup();
    assert!(harness.is_on_screen("This run keeps nothing on disk, and a study is a folder on disk"));
    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Comma);
    assert!(!harness.app.setup().is_open());
}
