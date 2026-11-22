//! A newer Trill, as the runtime has it for both front ends: what it
//! says without being asked, what its user silenced, what a start does.
//!
//! The releases are made up and published on a shelf of the test's, under
//! a key of the test's, for a Trill installed in a folder of the test's:
//! nothing is asked of a network, and the program that runs the test is
//! neither looked at nor changed.

use std::path::Path;
use std::time::{Duration, Instant};
use trill_engine::AudioBackend;
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage, UPDATE_FOLDER, UpdateNotice};
use trill_update::scripted::{ADDRESS, Makers};
use trill_update::{Came, Place, Version};

fn version(text: &str) -> Version {
    text.parse().unwrap()
}

/// A runtime of a Trill `of` that the makers released, installed in
/// `folder/Trill`, with its settings in `folder/config`: it fetches from
/// the makers' shelf.
fn runtime(makers: &Makers, folder: &Path, of: &str) -> Runtime {
    let home = folder.join("Trill");
    std::fs::create_dir_all(&home).unwrap();
    if !home.join("trill.exe").exists() {
        std::fs::write(home.join("trill.exe"), format!("window {of}")).unwrap();
    }
    let config = folder.join("config");
    let storage = Storage::At { settings: config.join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let updating = makers.program(of, &home, Some(config.join(UPDATE_FOLDER)));
    let runtime = Runtime::start_custom(options, Parts { updating: Some(updating), ..Parts::default() }).unwrap();
    runtime.updates().fetch_by(makers.shelf.clone());
    runtime
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let began = Instant::now();
    while !done() {
        assert!(began.elapsed() < Duration::from_secs(20), "waited too long for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_runtime_that_was_not_told_to_looks_for_nothing() {
    // As a test's runtime is, and a run that keeps nothing: the program is no release, and nobody fetches.
    let hermetic = Runtime::start(RuntimeOptions::hermetic()).unwrap();
    assert!(hermetic.updates().status().off.is_some());
    assert_eq!(hermetic.update_notice(), None);
    hermetic.look_as_a_start_does();
    assert!(hermetic.updates().look().is_err());

    // A release that nobody told who fetches asks nothing either, at a start or when asked.
    let (makers, folder) = (Makers::new(), tempfile::tempdir().unwrap());
    makers.publish("0.3.0", &[("trill.exe", b"window 0.3.0")]);
    let home = folder.path().join("Trill");
    let options = RuntimeOptions {
        audio: AudioBackend::Null { sample_rate: 48_000 },
        plugins: None,
        storage: Storage::At { settings: folder.path().join("config").join("settings.json"), library_index: None },
    };
    let updating = makers.program("0.2.0", &home, None);
    let quiet = Runtime::start_custom(options, Parts { updating: Some(updating), ..Parts::default() }).unwrap();
    quiet.look_as_a_start_does();
    assert!(quiet.updates().look().unwrap_err().contains("fetches nothing"));
    std::thread::sleep(Duration::from_millis(50));
    assert!(makers.shelf.asked().is_empty(), "{:?}", makers.shelf.asked());
}

#[test]
fn a_newer_trill_is_one_line_until_its_user_says_no_more_of_it_and_a_later_one_is_said_again() {
    let (makers, folder) = (Makers::new(), tempfile::tempdir().unwrap());
    let runtime = runtime(&makers, folder.path(), "0.2.0");
    assert_eq!(runtime.update_notice(), None);
    makers.publish("0.3.0", &[("trill.exe", b"window 0.3.0")]);
    runtime.updates().look().unwrap();
    let notice = runtime.update_notice().unwrap();
    assert_eq!(notice, UpdateNotice { version: version("0.3.0"), ready: false });
    assert_eq!((notice.words().as_str(), notice.short().as_str()), ("Trill 0.3.0 is out", "Trill 0.3.0 is out"));

    runtime.keep_quiet_about(Some(version("0.3.0"))).unwrap();
    assert_eq!(runtime.update_notice(), None);
    // What is known is known all the same: it is only not pointed at.
    assert_eq!(runtime.updates().status().newer_version(), Some(version("0.3.0")));

    // The next run remembers both, and asks nothing to know them.
    drop(runtime);
    let asked = makers.shelf.asked().len();
    let again = self::runtime(&makers, folder.path(), "0.2.0");
    assert_eq!(again.updates().status().newer_version(), Some(version("0.3.0")));
    assert_eq!(again.update_notice(), None);
    assert_eq!(makers.shelf.asked().len(), asked);

    // A version after the one that was silenced is said.
    makers.publish("0.4.0", &[("trill.exe", b"window 0.4.0")]);
    again.updates().look().unwrap();
    assert_eq!(again.update_notice().map(|notice| notice.version), Some(version("0.4.0")));
    // And "say it again" says whatever there is.
    again.keep_quiet_about(Some(version("0.4.0"))).unwrap();
    assert_eq!(again.update_notice(), None);
    again.keep_quiet_about(None).unwrap();
    assert_eq!(again.update_notice().map(|notice| notice.version), Some(version("0.4.0")));
}

#[test]
fn a_newer_trill_that_was_fetched_waits_where_trill_is_installed_and_says_so() {
    let (makers, folder) = (Makers::new(), tempfile::tempdir().unwrap());
    let runtime = runtime(&makers, folder.path(), "0.2.0");
    makers.publish("0.3.0", &[("trill.exe", b"window 0.3.0"), ("licences/Trill.txt", b"the licence")]);
    runtime.updates().look().unwrap();
    assert_eq!(runtime.updates().fetch().unwrap(), Came::Ready(version("0.3.0")));
    let notice = runtime.update_notice().unwrap();
    assert!(notice.ready);
    assert_eq!(notice.short(), "Trill 0.3.0 goes in at the next start");
    assert!(notice.words().contains("goes in the next time Trill starts while no other is open"));
    // Nothing of the Trill that runs has changed.
    let home = folder.path().join("Trill");
    assert_eq!(std::fs::read_to_string(home.join("trill.exe")).unwrap(), "window 0.2.0");
    assert_eq!(Place::at(&home).staged().unwrap().version, version("0.3.0"));
    // The next run finds it ready without asking, and what it fetched is the announcement and the build.
    drop(runtime);
    let asked = makers.shelf.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[0], ADDRESS);
    let again = self::runtime(&makers, folder.path(), "0.2.0");
    assert!(again.update_notice().unwrap().ready);
    assert_eq!(makers.shelf.asked().len(), 2);
}

#[test]
fn a_start_looks_once_where_its_user_has_not_switched_that_off() {
    let (makers, folder) = (Makers::new(), tempfile::tempdir().unwrap());
    makers.publish("0.3.0", &[("trill.exe", b"window 0.3.0")]);
    // Switched off, a start asks nothing.
    let runtime = runtime(&makers, folder.path(), "0.2.0");
    runtime.set_look_at_start(false).unwrap();
    runtime.look_as_a_start_does();
    std::thread::sleep(Duration::from_millis(100));
    assert!(makers.shelf.asked().is_empty(), "{:?}", makers.shelf.asked());
    assert_eq!(runtime.update_notice(), None);
    // Switched on, it looks, and what it found is there to be said.
    runtime.set_look_at_start(true).unwrap();
    runtime.look_as_a_start_does();
    wait_until("the start to have looked", || runtime.update_notice().is_some());
    assert_eq!(makers.shelf.asked(), [ADDRESS]);
    // The next start, a moment later, does not ask again: it knows.
    drop(runtime);
    let again = self::runtime(&makers, folder.path(), "0.2.0");
    again.look_as_a_start_does();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(makers.shelf.asked(), [ADDRESS]);
    assert_eq!(again.update_notice().map(|notice| notice.version), Some(version("0.3.0")));
}
