//! Missing files: a song whose samples were moved says so in the status
//! bar, lists what is missing with what was found, and is put right in one
//! action; and a folder of the library that is gone is said to be.
//!
//! Each test makes its own files in a temporary folder and runs on a
//! runtime that keeps nothing: nothing of the user's is read or written.

use super::sounds::{WAIT, tone};
use super::*;
use std::path::{Path, PathBuf};
use trill_core::project::AudioContent;
use trill_core::{ChainRef, DeviceId};
use trill_runtime::Confidence;

/// A song whose samples went elsewhere after it was saved.
struct Moved {
    _folder: tempfile::TempDir,
    /// Where the pack was when the song was made, and where it is now.
    old: PathBuf,
    new: PathBuf,
    song: PathBuf,
    rack: DeviceId,
}

impl Moved {
    fn riser(&self, root: &Path) -> PathBuf {
        root.join("FX").join("Riser 2 Bars.wav")
    }

    fn kick(&self, root: &Path) -> PathBuf {
        root.join("Kit").join("Kicks").join("Kick 01.wav")
    }
}

/// A window on a song that plays a riser in two clips on "FX" and a kick
/// on a drum rack on "Kit", from a pack that was moved to `new` (below the
/// temporary folder) after the song was saved. The song has been opened
/// again since, as it would be the next day.
fn moved_song(options: UiOptions, new: &[&str]) -> (Harness, Moved) {
    let folder = tempfile::tempdir().unwrap();
    let base = std::path::absolute(folder.path()).unwrap();
    let old = base.join("Music").join("packs");
    let new = new.iter().fold(base.clone(), |path, part| path.join(part));
    let riser = tone(old.join("FX").join("Riser 2 Bars.wav"), 2.0);
    let kick = tone(old.join("Kit").join("Kicks").join("Kick 01.wav"), 0.25);
    tone(old.join("Kit").join("Snare.wav"), 0.2);
    let mut rack = None;
    let harness = Harness::new(options, |session| {
        let fx = session.edit(|project| Ok(project.add_track("FX"))).unwrap();
        let path = riser.display().to_string();
        session
            .edit(|project| {
                project.add_audio_clip(fx, "Riser", 0.0, 4.0, AudioContent::new(&path))?;
                project.add_audio_clip(fx, "Riser again", 8.0, 2.0, AudioContent::new(&path))
            })
            .unwrap();
        let kit = session.edit(|project| Ok(project.add_track("Kit"))).unwrap();
        let device = session.add_device(ChainRef::Track(kit), "builtin:drumrack", None, None).unwrap();
        session.set_pad_file(device, 36, Some(&kick.display().to_string())).unwrap();
        rack = Some(device);
    });
    let song = base.join("Songs").join("night.trill");
    std::fs::create_dir_all(song.parent().unwrap()).unwrap();
    harness.session.lock().save(&song).unwrap();
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::rename(&old, &new).unwrap();
    harness.runtime.media.clear();
    harness.session.lock().load(&song).unwrap();
    (harness, Moved { _folder: folder, old, new, song, rack: rack.unwrap() })
}

impl Harness {
    /// What the status bar says of missing files, if it says anything.
    fn missing_line(&self) -> Option<String> {
        let line = self.labels.iter().find_map(|(text, _)| text.strip_prefix("Missing files: "));
        line.map(str::to_owned)
    }

    /// The files the song names, in its order.
    fn song_files(&self) -> Vec<PathBuf> {
        self.session.lock().files().into_iter().map(|file| PathBuf::from(file.path)).collect()
    }

    /// Waits for the library and for whatever the window does about it.
    fn settle_library(&mut self) {
        self.settle_work();
        assert!(self.runtime.library.wait_until_idle(WAIT));
        self.settle_work();
    }
}

const FAR: &[&str] = &["Other", "a", "b", "c", "d", "packs"];

#[test]
fn a_song_that_opens_with_files_missing_says_so_and_is_put_right_in_one_action() {
    let (mut harness, moved) = moved_song(UiOptions::default(), FAR);
    harness.settle_work();

    // Said once, where it stays: a line in the status bar, and a word when
    // the song opened.
    assert_eq!(harness.missing_line().as_deref(), Some("2 files missing"));
    let said = harness.app.state().toasts.texts().join(" | ");
    assert!(
        said.contains("2 audio files this song plays are not on disk. Missing files, in the bar below, lists them."),
        "{said}"
    );
    assert_eq!(harness.app.sounds().missing.found.places(), 3);

    // The list: what is missing, where it is used, and that nothing was found.
    harness.click_label("Missing files: 2 files missing");
    harness.settle_work();
    assert!(harness.app.sounds().missing.open);
    assert!(
        harness.is_on_screen(
            "2 audio files this song plays are not on disk. 2 clips and 1 sample on FX and Kit are silent."
        )
    );
    assert!(harness.is_on_screen("Riser 2 Bars.wav \u{00B7} 2 clips \u{00B7} FX \u{00B7} Not found"));
    assert!(harness.is_on_screen("Kick 01.wav \u{00B7} pad 36 \u{00B7} Kit \u{00B7} Not found"));
    assert!(!harness.is_on_screen("Relink all 2") && !harness.is_on_screen("Use"));
    // The keyboard is the list's: a shortcut does not reach the song under it.
    let tracks = harness.project().tracks.len();
    harness.key(Modifiers::COMMAND, Key::T);
    assert_eq!(harness.project().tracks.len(), tracks);
    harness.click_label("Close");
    assert!(!harness.app.sounds().missing.open);

    // The library is pointed at where the samples are: the files are found
    // there without anyone asking, and the line says so.
    let toasts = harness.app.state().toasts.len();
    harness.app.sounds_mut().add_folder(moved.new.parent().unwrap().to_path_buf());
    harness.settle_library();
    assert_eq!(harness.missing_line().as_deref(), Some("2 files missing \u{00B7} all found"));
    assert_eq!(
        harness.app.state().toasts.len(),
        toasts + 1,
        "the folder was added; nothing is said twice of the files"
    );
    assert_eq!(harness.song_files()[0], moved.riser(&moved.old), "finding relinks nothing");

    harness.click_label("Missing files: 2 files missing \u{00B7} all found");
    harness.settle_work();
    let new = moved.new.display().to_string();
    assert!(harness.is_on_screen(&format!("Found 2 of 2 in {new}")), "{:?}", harness.labels);
    let folder = moved.riser(&moved.new).parent().unwrap().display().to_string();
    assert!(harness.is_on_screen(&format!("Riser 2 Bars.wav \u{00B7} 2 clips \u{00B7} FX \u{00B7} In {folder}")));
    assert!(!harness.is_on_screen("Use"), "what is sure needs no second look");

    // One action, one undo step.
    let undo = harness.undo_depth();
    harness.click_label("Relink all 2");
    harness.settle_work();
    assert_eq!(harness.song_files(), [moved.riser(&moved.new), moved.riser(&moved.new), moved.kick(&moved.new)]);
    assert_eq!(harness.undo_depth(), undo + 1);
    assert!(harness.session.lock().problems().is_empty());
    assert_eq!(harness.missing_line(), None);
    assert!(harness.app.state().toasts.texts().contains(&"Relinked 2 files in 3 places"));
    assert!(harness.is_on_screen("Every file this song plays is where the song says it is."));
    harness.click_label("Close");
    harness.settle();

    // Taken back, the song is missing them again, and says so.
    harness.key(Modifiers::COMMAND, Key::Z);
    harness.settle_work();
    assert_eq!(harness.song_files()[0], moved.riser(&moved.old));
    assert_eq!(harness.missing_line().as_deref(), Some("2 files missing \u{00B7} all found"));
    let _ = (&moved.song, moved.rack);
}

#[test]
fn a_file_that_only_has_the_name_is_offered_and_taken_only_by_a_button_of_its_own() {
    let (mut harness, moved) = moved_song(UiOptions::default(), FAR);
    // The pack is gone for good. A library elsewhere has the very riser,
    // and a kick of the same name that is another kick.
    std::fs::remove_dir_all(&moved.new).unwrap();
    let library = moved.old.parent().unwrap().join("Library");
    let riser = tone(library.join("Riser 2 Bars.wav"), 2.0);
    let other_kick = tone(library.join("Drums").join("Kick 01.wav"), 0.6);
    harness.app.sounds_mut().add_folder(library.clone());
    harness.settle_library();
    assert_eq!(harness.missing_line().as_deref(), Some("2 files missing \u{00B7} 1 found"));

    harness.click_label("Missing files: 2 files missing \u{00B7} 1 found");
    harness.settle_work();
    let found = &harness.app.sounds().missing.found;
    assert_eq!(found.files[1].found().map(|candidate| candidate.confidence), Some(Confidence::Unsure));
    let folder = other_kick.parent().unwrap().display().to_string();
    assert!(harness.is_on_screen(&format!("Kick 01.wav \u{00B7} pad 36 \u{00B7} Kit \u{00B7} Unsure: {folder}")));
    assert!(harness.is_on_screen(&format!("Found 1 of 2 in {}", library.display())), "{:?}", harness.labels);

    // The one action takes what is sure and leaves the kick alone.
    harness.click_label("Relink 1");
    harness.settle_work();
    assert_eq!(harness.song_files(), [riser.clone(), riser, moved.kick(&moved.old)]);
    assert_eq!(harness.missing_line().as_deref(), Some("1 file missing"));
    // Its row says why it is unsure to whoever points at it.
    harness.move_to(harness.labeled(&format!("Kick 01.wav \u{00B7} pad 36 \u{00B7} Kit \u{00B7} Unsure: {folder}")));
    harness.idle(2);
    // And only its own button takes it.
    let undo = harness.undo_depth();
    harness.click_label("Use");
    harness.settle_work();
    assert_eq!(harness.song_files()[2], other_kick);
    assert_eq!(harness.undo_depth(), undo + 1);
    assert_eq!(harness.missing_line(), None);
}

#[test]
fn one_file_pointed_at_by_hand_leads_to_the_others_and_so_does_a_folder() {
    let (mut harness, moved) = moved_song(UiOptions::default(), FAR);
    harness.settle_work();
    assert_eq!(harness.missing_line().as_deref(), Some("2 files missing"));

    // What the dialog's answer does: every place that names the riser
    // plays it where it is now, as one step.
    let undo = harness.undo_depth();
    let old_riser = moved.riser(&moved.old).display().to_string();
    harness.app.sounds_mut().relink_file(old_riser, moved.riser(&moved.new));
    harness.settle_work();
    assert_eq!(harness.song_files()[..2], [moved.riser(&moved.new), moved.riser(&moved.new)]);
    assert_eq!(harness.undo_depth(), undo + 1);
    // Where it was and where it is say where its folder went: the kick is
    // found there on that word, and waits to be relinked.
    assert_eq!(harness.missing_line().as_deref(), Some("1 file missing \u{00B7} found"));
    assert_eq!(harness.song_files()[2], moved.kick(&moved.old));
    harness.app.sounds_mut().relink_found(Confidence::Sure);
    harness.settle_work();
    assert_eq!(harness.song_files()[2], moved.kick(&moved.new));
    assert_eq!(harness.missing_line(), None);

    // A file that is no audio is refused, and the song left as it is.
    harness.key(Modifiers::COMMAND, Key::Z);
    harness.settle_work();
    let notes = moved.new.join("notes.txt");
    std::fs::write(&notes, "not a sound").unwrap();
    let before = harness.song_files();
    harness.app.sounds_mut().relink_file(moved.kick(&moved.old).display().to_string(), notes);
    harness.settle_work();
    assert_eq!(harness.song_files(), before);
    assert!(harness.app.state().toasts.texts().iter().any(|text| text.contains("nothing was relinked")));
}

#[test]
fn a_folder_to_look_in_finds_what_the_library_does_not_cover() {
    let (mut harness, moved) = moved_song(UiOptions::default(), FAR);
    harness.settle_work();
    assert_eq!(harness.missing_line().as_deref(), Some("2 files missing"));
    // What the folder dialog's answer does.
    harness.app.sounds_mut().look_in(moved.new.parent().unwrap().parent().unwrap().to_path_buf());
    harness.settle_work();
    assert_eq!(harness.missing_line().as_deref(), Some("2 files missing \u{00B7} all found"));
    assert!(harness.app.sounds().missing.has_pointed());
}

#[test]
fn a_library_folder_that_is_gone_is_marked_and_goes_along_with_the_songs_files() {
    let options = UiOptions { browser_tab: Some(BrowserTab::Sounds), ..UiOptions::default() };
    let folder = tempfile::tempdir().unwrap();
    let base = std::path::absolute(folder.path()).unwrap();
    let (old, new) = (base.join("Music").join("packs"), base.join("Music").join("trill_samples").join("packs"));
    let riser = tone(old.join("FX").join("Riser 2 Bars.wav"), 2.0);
    let kick = tone(old.join("Kit").join("Kick 01.wav"), 0.25);
    let mut harness = Harness::new(options, |session| {
        let fx = session.edit(|project| Ok(project.add_track("FX"))).unwrap();
        let content = AudioContent::new(riser.display().to_string());
        session.edit(|project| project.add_audio_clip(fx, "Riser", 0.0, 4.0, content)).unwrap();
    });
    harness.app.sounds_mut().add_folder(old.clone());
    harness.settle_library();
    harness.runtime.library.set_favorite(&kick, true);
    // The folder is chosen and unfolded in the browser, which remembers both.
    harness.app.state_mut().browser.sounds.folder = Some(old.join("Kit"));
    harness.app.state_mut().browser.sounds.expanded.insert(old.clone());
    let song = base.join("night.trill");
    harness.session.lock().save(&song).unwrap();

    // Moved while nobody looked; the song is opened and the library scanned.
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::rename(&old, &new).unwrap();
    harness.runtime.media.clear();
    harness.session.lock().load(&song).unwrap();
    harness.runtime.library.rescan();
    harness.settle_library();

    // The library still lists what it knew, and the folder is said to be gone.
    assert_eq!(harness.runtime.library.roots(), [old.clone()]);
    let found = harness.app.sounds().missing.found.clone();
    assert_eq!(found.folders.len(), 1, "{found:#?}");
    assert!(found.folders[0].found.as_ref().is_some_and(|there| there.sure() && there.path == new));
    assert_eq!(harness.missing_line().as_deref(), Some("1 file missing \u{00B7} found"));

    harness.click_label("Missing files: 1 file missing \u{00B7} found");
    harness.settle_work();
    assert!(harness.is_on_screen(&format!("Library folder {} \u{00B7} Now at {}", old.display(), new.display())));
    assert!(harness.is_on("Point the sound library there too"));
    assert!(harness.is_on_screen("Move") && harness.is_on_screen("Locate\u{2026}"));

    // Relinking the song takes the library along, with what it keeps.
    harness.click_label("Relink all 1");
    harness.settle_library();
    assert_eq!(harness.song_files(), [new.join("FX").join("Riser 2 Bars.wav")]);
    assert_eq!(harness.runtime.library.roots(), [new.clone()]);
    assert!(harness.runtime.library.is_favorite(&new.join("Kit").join("Kick 01.wav")));
    assert!(harness.app.sounds().missing.found.is_empty());
    assert_eq!(harness.missing_line(), None);
    let browser = &harness.app.state().browser.sounds;
    assert_eq!(browser.folder, Some(new.join("Kit")), "the folder chosen in the browser is the same folder");
    assert!(browser.expanded.contains(&new) && !browser.expanded.contains(&old));
    let said = harness.app.state().toasts.texts().join(" | ");
    assert!(said.contains("Relinked 1 file, and the library now has packs"), "{said}");

    // A library folder that is gone with no song missing anything is
    // still said, and put right by itself.
    let third = base.join("Third").join("packs");
    std::fs::create_dir_all(third.parent().unwrap()).unwrap();
    std::fs::rename(&new, &third).unwrap();
    harness.click_label("Close");
    harness.runtime.media.clear();
    harness.session.lock().new_project();
    harness.runtime.library.rescan();
    harness.settle_library();
    assert_eq!(harness.missing_line().as_deref(), Some("Library folder missing"));
    harness.app.sounds_mut().move_library_folder(new.clone(), third.clone());
    harness.settle_library();
    assert_eq!(harness.runtime.library.roots(), [third]);
    assert_eq!(harness.missing_line(), None);
}
