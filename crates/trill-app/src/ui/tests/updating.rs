//! A newer Trill in the window: the line in the status bar, the tab of
//! Setup it opens, fetching from there, and saying no more of a version.
//!
//! The releases here are made up by the test and published on a shelf of
//! its own, under a key of its own, for a Trill that is installed in a
//! folder of the test's: nothing is asked of a network, and nothing of
//! the program that runs the test is looked at or changed.

use super::*;
use trill_engine::AudioBackend;
use trill_runtime::{Parts, Storage};
use trill_update::scripted::Makers;
use trill_update::{Place, Step};

struct Fixture {
    harness: Harness,
    makers: Makers,
    /// Trill is installed in `Trill` in it, and keeps its settings in `config`.
    folder: tempfile::TempDir,
}

/// A window on a Trill 0.2.0 that its makers released, installed in a
/// folder of the test's, which fetches from the makers' shelf.
fn fixture() -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let makers = Makers::new();
    let home = folder.path().join("Trill");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("trill.exe"), "window 0.2.0").unwrap();
    let config = folder.path().join("config");
    let storage = Storage::At { settings: config.join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let updating = makers.program("0.2.0", &home, Some(config.join("update")));
    let runtime = Arc::new(Runtime::start_custom(options, Parts { updating: Some(updating), ..Parts::default() }).unwrap());
    runtime.updates().fetch_by(makers.shelf.clone());
    let harness = Harness::on(runtime, UiOptions::default(), |_| {});
    Fixture { harness, makers, folder }
}

impl Harness {
    /// Waits until the Setup panel has read what it shows and nothing it was asked for is still being done.
    fn settle_updates(&mut self) {
        self.wait_until("the Setup panel to settle", |harness| !harness.app.setup().is_busy());
        self.idle(3);
        self.wait_until("the Setup panel to settle", |harness| !harness.app.setup().is_busy());
        self.idle(2);
    }

    /// Whether anything on screen begins with these words.
    fn says(&self, beginning: &str) -> bool {
        self.labels.iter().any(|(text, _)| text.starts_with(beginning))
    }
}

#[test]
fn a_newer_trill_is_one_line_in_the_status_bar_that_opens_what_there_is_to_read_and_fetches_it() {
    let Fixture { mut harness, makers, folder } = fixture();
    let home = folder.path().join("Trill");
    harness.idle(2);
    assert!(!harness.says("A newer Trill"), "nothing is known yet, so nothing is said");

    makers.publish("0.3.0", &[("trill.exe", b"window 0.3.0")]);
    harness.runtime.updates().look().unwrap();
    harness.idle(2);
    assert!(harness.is_on_screen("A newer Trill: Trill 0.3.0 is out"), "{:?}", harness.hint());

    // The line opens the tab of Setup that is about Trill itself: which this one is, what is new in the
    // other, its size and where it comes from, over the button that fetches it.
    harness.click_label("A newer Trill: Trill 0.3.0 is out");
    harness.settle_updates();
    assert!(harness.is_on_screen("Trill 0.2.0 (build abc1234)."));
    assert!(harness.is_on_screen("What is new in 0.3.0."));
    assert!(harness.is_on_screen("From: shelf.test"));
    assert!(harness.is_on_screen("Its page: https://shelf.test/v0.3.0"));
    let asked = makers.shelf.asked().len();
    assert_eq!(asked, 1, "looking fetched the announcement and nothing else: {:?}", makers.shelf.asked());

    // Fetched when it is asked for, and staged where Trill is installed: nothing changes yet.
    harness.click_label("Fetch the newer Trill");
    harness.wait_until("the newer Trill to be fetched", |harness| {
        matches!(harness.runtime.updates().status().step, Step::Ready(_))
    });
    harness.settle_updates();
    assert!(harness.says("Trill 0.3.0 is fetched. It goes in the next time Trill starts"));
    assert!(harness.is_on_screen("A newer Trill: Trill 0.3.0 goes in at the next start"));
    assert_eq!(Place::at(&home).staged().unwrap().version, "0.3.0".parse().unwrap());
    assert_eq!(std::fs::read_to_string(home.join("trill.exe")).unwrap(), "window 0.2.0");

    // Its user says no more of it: the line goes, and what was fetched stays ready.
    harness.click_label("Say no more of this version");
    harness.settle_updates();
    assert!(!harness.says("A newer Trill"), "the status bar says nothing of it any more");
    assert!(harness.is_on_screen("Say again that a newer Trill is out"));
    assert_eq!(harness.runtime.settings().updates.quiet_about.as_deref(), Some("0.3.0"));
    assert!(matches!(harness.runtime.updates().status().step, Step::Ready(_)));
    // And it is said again when they ask.
    harness.click_label("Say again that a newer Trill is out");
    harness.settle_updates();
    assert!(harness.is_on_screen("A newer Trill: Trill 0.3.0 goes in at the next start"));

    // Put away, Trill stays as it is and the newer one is out as before.
    harness.click_label("Put the fetched update away");
    harness.settle_updates();
    assert_eq!(Place::at(&home).staged(), None);
    assert!(harness.is_on_screen("A newer Trill: Trill 0.3.0 is out"));
}

#[test]
fn looking_from_the_tab_says_what_it_found_and_what_went_wrong() {
    let Fixture { mut harness, makers, folder: _folder } = fixture();
    // Nothing is published: the look fails, and says so where it was asked for.
    harness.key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Comma);
    harness.settle_updates();
    harness.click_label("Trill");
    harness.settle_updates();
    assert!(harness.is_on_screen("Trill has not looked yet whether a newer one is out."));
    harness.click_label("Look for a newer Trill");
    harness.settle_updates();
    assert!(harness.says("Trill could not look for a newer version"), "{:?}", harness.app.setup().last());
    // Published, the same button finds it.
    makers.publish("0.3.0", &[("trill.exe", b"window 0.3.0")]);
    harness.click_label("Look for a newer Trill");
    harness.settle_updates();
    assert_eq!(harness.app.setup().last(), Some("Trill 0.3.0 is out"));
    assert!(harness.is_on_screen("Fetch the newer Trill"));

    // Looking at a start is switched off here, and stays off.
    assert!(harness.runtime.settings().updates.look_at_start);
    harness.click_label("Whether Trill looks when it starts");
    harness.settle_updates();
    assert!(!harness.runtime.settings().updates.look_at_start);
}

#[test]
fn a_window_on_a_build_of_ones_own_says_nothing_of_a_newer_trill_and_asks_nothing() {
    // The program a test runs in is no release: what it shows of itself says so, and it has no button that looks.
    let mut harness = Harness::new(UiOptions::default(), |_| {});
    harness.idle(2);
    assert!(!harness.says("A newer Trill"));
    assert!(harness.app.setup().notice().is_none());
    let status = harness.runtime.updates().status();
    assert!(status.off.as_deref().is_some_and(|why| why.contains("a build of your own")), "{:?}", status.off);
    assert!(harness.runtime.updates().look().is_err());
}
