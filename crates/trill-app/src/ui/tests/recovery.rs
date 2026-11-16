//! The offer to bring back unsaved work: what a program that died left
//! behind is staged in a folder of the test's own, and a window is opened
//! on a runtime that finds it there. Nothing of the user's is looked at.

use super::*;
use std::path::{Path, PathBuf};
use trill_engine::AudioBackend;
use trill_runtime::Storage;

/// A runtime that keeps what it keeps under `folder`.
fn kept_in(folder: &Path) -> RuntimeOptions {
    RuntimeOptions {
        audio: AudioBackend::Null { sample_rate: 48_000 },
        plugins: None,
        storage: Storage::At { settings: folder.join("settings.json"), library_index: None },
    }
}

/// A program that stops without a goodbye: a project called `name` with
/// `tracks` tracks, the last of them added after it was saved to `file`
/// (or without its ever having been saved).
fn lose(folder: &Path, name: &str, file: Option<&Path>, tracks: usize) {
    let runtime = Runtime::start(kept_in(folder)).unwrap();
    {
        let mut session = runtime.session.lock();
        let first = |project: &mut Project| {
            project.name = name.into();
            for index in 1..tracks {
                project.add_track(format!("Track {index}"));
            }
            Ok(())
        };
        session.edit(first).unwrap();
        if let Some(file) = file {
            session.save(file).unwrap();
        }
        session.edit(|project| Ok(project.add_track("Not saved"))).unwrap();
    }
    runtime.refresh_safety_copy();
    runtime.abandon();
}

/// The window of the next program to start in `folder`.
fn window(folder: &Path, setup: impl FnOnce(&mut Session)) -> Harness {
    Harness::on(Arc::new(Runtime::start(kept_in(folder)).unwrap()), UiOptions::default(), setup)
}

/// The safety copies in `folder`: the project files of its recovery folder.
fn copies(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder.join("recovery")) else { return Vec::new() };
    entries.flatten().map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|e| e == "trill")).collect()
}

/// Lets the worker finish recovering or discarding, and the window see it.
fn settle_recovery(harness: &mut Harness) {
    assert!(harness.app.sounds().wait_until_idle(Duration::from_secs(20)), "the worker never finished");
    harness.idle(3);
}

fn track_names(harness: &Harness) -> Vec<String> {
    harness.project().tracks.iter().map(|track| track.name.clone()).collect()
}

#[test]
fn unsaved_work_is_offered_when_the_window_opens_and_comes_back_unsaved() {
    let folder = tempfile::tempdir().unwrap();
    let song = folder.path().join("songs").join("night shift.trill");
    lose(folder.path(), "Night Shift", Some(&song), 3);
    let mut harness = window(folder.path(), |_| {});

    // The offer is up, and says what it is, where it lived and how long ago.
    assert!(harness.app.recovery().is_open());
    let row = format!("Night Shift \u{00B7} {} \u{00B7} Moments ago", song.display());
    assert!(harness.is_on_screen(&row), "{:?}", harness.labels);
    for button in ["Recover", "Discard\u{2026}", "Decide later"] {
        assert!(harness.is_on_screen(button), "{button}");
    }
    // Nothing has happened yet, and the keys are the offer's.
    assert!(harness.project().tracks.is_empty());
    harness.key(Modifiers::COMMAND, Key::T);
    assert!(harness.project().tracks.is_empty(), "a shortcut does not reach past the question");

    harness.click_label("Recover");
    settle_recovery(&mut harness);
    assert_eq!(harness.project().name, "Night Shift");
    assert_eq!(track_names(&harness), ["Track 1", "Track 2", "Not saved"], "as it was when the program stopped");
    {
        let session = harness.session.lock();
        assert_eq!(session.path(), Some(song.as_path()), "in the place of its file");
        assert!(session.is_modified(), "but with unsaved changes: saving is the user's to do");
    }
    let on_disk = Project::from_json(&std::fs::read_to_string(&song).unwrap()).unwrap();
    assert_eq!(on_disk.tracks.len(), 2, "the file holds what was saved and no more");
    assert!(!harness.app.recovery().is_open());
    assert_eq!(harness.app.state().toasts.len(), 1, "and it says what happened");
    assert_eq!(harness.app.state().selected_track, Some(harness.project().tracks[0].id), "the window shows it");

    // What came back cannot be undone into nothing.
    assert_eq!(harness.undo_depth(), 0);
    harness.key(Modifiers::COMMAND, Key::Z);
    assert_eq!(track_names(&harness).len(), 3);
    // It is this program's to keep safe now, and nobody is offered it twice.
    assert!(harness.runtime.recoverable().is_empty());
    assert_eq!(harness.app.recovery().count(), 0);
    assert_eq!(copies(folder.path()).len(), 1, "one copy: this program's own");
    harness.click_label("File");
    harness.click_label("Recover unsaved work");
    assert!(!harness.app.recovery().is_open(), "with nothing left the menu item is not to be had");
}

#[test]
fn nothing_is_discarded_unless_the_user_says_so_twice() {
    let folder = tempfile::tempdir().unwrap();
    let song = folder.path().join("night shift.trill");
    lose(folder.path(), "Night Shift", Some(&song), 2);
    lose(folder.path(), "Untitled", None, 1);
    let mut harness = window(folder.path(), |_| {});
    let untouched = |harness: &Harness, expected: usize| {
        assert_eq!(harness.runtime.recoverable().len(), expected);
        assert_eq!(copies(folder.path()).len(), expected);
        assert_eq!(harness.app.recovery().count(), expected);
    };

    // Both are offered; one of them was never saved.
    assert_eq!(harness.all_labeled("Recover").len(), 2);
    assert!(harness.is_on_screen("Untitled \u{00B7} Never saved \u{00B7} Moments ago"), "{:?}", harness.labels);
    // The rows are in the runtime's order (most recent first), so the
    // first buttons act on its first copy.
    let first = harness.runtime.recoverable()[0].name.clone();
    let other = if first == "Untitled" { "Night Shift" } else { "Untitled" };
    assert!(harness.all_labeled(&first)[0].top() < harness.all_labeled(other)[0].top());

    // Deciding later closes the offer and touches nothing.
    harness.click_label("Decide later");
    assert!(!harness.app.recovery().is_open());
    untouched(&harness, 2);
    // The copies stay reachable from the File menu, which says how many.
    harness.click_label("File");
    harness.click_label("Recover unsaved work");
    assert!(harness.app.recovery().is_open());
    // Escape is "later" too.
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(2);
    assert!(!harness.app.recovery().is_open());
    untouched(&harness, 2);
    harness.click_label("File");
    harness.click_label("Recover unsaved work");

    // Discarding asks first, and "keep it" keeps it.
    harness.click_label("Discard\u{2026}");
    harness.idle(3);
    assert!(harness.is_on_screen("Discard for good") && harness.is_on_screen("Keep it"));
    untouched(&harness, 2);
    harness.click_label("Keep it");
    harness.idle(3);
    untouched(&harness, 2);
    assert!(harness.is_on_screen("Decide later"), "back at the offer");
    // So does Escape at the question.
    harness.click_label("Discard\u{2026}");
    harness.idle(3);
    harness.key(Modifiers::NONE, Key::Escape);
    harness.idle(3);
    untouched(&harness, 2);

    // Said twice, the copy goes, for good; the other one stays on offer.
    harness.click_label("Discard\u{2026}");
    harness.click_label("Discard for good");
    settle_recovery(&mut harness);
    untouched(&harness, 1);
    assert_eq!(harness.runtime.recoverable()[0].name, other, "the one that was not named is still there");
    assert!(harness.app.recovery().is_open() && harness.all_labeled("Recover").len() == 1);
    assert!(harness.all_labeled(&first).is_empty(), "and the one that was is off the list");
    // None of it touched the open project or made anything to undo.
    assert!(harness.project().tracks.is_empty());
    assert_eq!(harness.undo_depth(), 0);
    assert!(!harness.session.lock().is_modified());

    // Closing the window with the offer still open discards nothing
    // either: the next program is offered what is left.
    drop(harness);
    assert_eq!(copies(folder.path()).len(), 1);
    let next = Runtime::start(kept_in(folder.path())).unwrap();
    assert_eq!(next.recoverable().len(), 1);
}

#[test]
fn recovering_over_unsaved_changes_asks_first() {
    let folder = tempfile::tempdir().unwrap();
    lose(folder.path(), "Night Shift", None, 2);
    // The open project has work of its own that is not saved.
    let mut harness = window(folder.path(), |session| {
        session.edit(|project| Ok(project.add_track("Mine"))).unwrap();
    });
    harness.click_label("Recover");
    harness.idle(3);
    assert!(harness.is_on_screen("Discard and recover") && harness.is_on_screen("Cancel"));
    harness.click_label("Cancel");
    settle_recovery(&mut harness);
    harness.idle(3);
    assert_eq!(track_names(&harness), ["Mine"], "cancelled: nothing is lost on either side");
    assert_eq!(harness.runtime.recoverable().len(), 1);
    assert!(harness.app.recovery().is_open(), "back at the offer");

    harness.click_label("Recover");
    harness.click_label("Discard and recover");
    settle_recovery(&mut harness);
    assert_eq!(track_names(&harness), ["Track 1", "Not saved"]);
    assert_eq!(harness.session.lock().path(), None, "it never had a file, and has none now");
    assert!(harness.session.lock().is_modified());
    assert!(!harness.app.recovery().is_open());
}

#[test]
fn a_copy_that_cannot_be_opened_is_kept_and_the_open_project_left_alone() {
    let folder = tempfile::tempdir().unwrap();
    lose(folder.path(), "Night Shift", None, 2);
    let copy = copies(folder.path()).pop().unwrap();
    std::fs::write(&copy, "{ not a project").unwrap();

    let mut harness = window(folder.path(), |_| {});
    harness.click_label("Recover");
    settle_recovery(&mut harness);
    assert_eq!(harness.app.state().toasts.len(), 1, "it says that it could not, and why");
    assert!(harness.project().tracks.is_empty());
    assert_eq!(harness.project().name, "Untitled");
    assert_eq!(harness.runtime.recoverable().len(), 1, "the copy is kept for a human to look at");
    assert!(copy.exists());
    assert!(harness.app.recovery().is_open(), "and stays on offer, to be discarded or left");
}

#[test]
fn a_run_with_nothing_lost_shows_no_offer() {
    let folder = tempfile::tempdir().unwrap();
    let mut harness = window(folder.path(), |_| {});
    assert!(!harness.app.recovery().is_open());
    assert!(!harness.is_on_screen("Recover") && !harness.is_on_screen("Decide later"));
    // Its own unsaved work is no "lost" work: it is copied for the next
    // program, not offered to this one.
    harness.key(Modifiers::COMMAND, Key::T);
    harness.runtime.refresh_safety_copy();
    harness.idle(2);
    assert_eq!(copies(folder.path()).len(), 1);
    assert!(harness.runtime.recoverable().is_empty() && !harness.app.recovery().is_open());
}
