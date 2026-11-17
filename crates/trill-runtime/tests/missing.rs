//! A song whose files are not where it says: what is said to be missing,
//! what is found and how surely, and putting the song right. On folders
//! of generated audio in a temporary place, through the runtime as a front
//! end uses it.

use std::path::{Path, PathBuf};
use std::time::Duration;
use trill_core::audio::AudioBuffer;
use trill_core::project::AudioContent;
use trill_core::{ChainRef, FolderMove, Note};
use trill_engine::AudioBackend;
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Confidence, Look, Missing, Runtime, RuntimeOptions, Storage};

const WAIT: Duration = Duration::from_secs(60);

/// A runtime that keeps its settings and index under `folder`: no audio
/// device, no plugins, nothing of the user's.
fn start(folder: &Path) -> Runtime {
    let storage = Storage::At {
        settings: folder.join("config").join("settings.json"),
        library_index: Some(folder.join("cache").join("library-index.json")),
    };
    Runtime::start(RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage })
        .expect("the runtime starts")
}

/// Writes `seconds` of a tone and returns the file's path.
fn tone(path: PathBuf, seconds: f64) -> PathBuf {
    let rate = 44_100.0;
    let channel: Vec<f32> = (0..(seconds * rate) as usize)
        .map(|n| (n as f64 * 220.0 * std::f64::consts::TAU / rate).sin() as f32 * 0.5)
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(&path, &AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }, WavFormat::Int16)
        .unwrap();
    path
}

/// A small pack: a kick, a snare and a loop of two seconds, and files the
/// song does not use.
fn pack(root: &Path) -> [PathBuf; 3] {
    tone(root.join("Kit One").join("Hats").join("Hat Closed.wav"), 0.1);
    tone(root.join("Loops").join("Pad Loop 90bpm.wav"), 1.0);
    [
        tone(root.join("Kit One").join("Kicks").join("Kick 01.wav"), 0.2),
        tone(root.join("Kit One").join("Snares").join("Snare Tight.wav"), 0.25),
        tone(root.join("Loops").join("Bass Loop 120bpm.wav"), 2.0),
    ]
}

/// A song at 120 BPM that plays the three files in four places: the loop
/// in two clips on "FX", the kick and the snare on a drum rack on "Kit".
/// Saved at `song`.
fn song(runtime: &Runtime, [kick, snare, a_loop]: &[PathBuf; 3], song: &Path) {
    let mut session = runtime.session.lock();
    session.new_project();
    let fx = session.edit(|project| Ok(project.add_track("FX"))).unwrap();
    let path = a_loop.display().to_string();
    session
        .edit(|project| {
            project.add_audio_clip(fx, "Loop", 0.0, 4.0, AudioContent::new(&path))?;
            project.add_audio_clip(fx, "Loop again", 8.0, 2.0, AudioContent::new(&path))
        })
        .unwrap();
    let kit = session.edit(|project| Ok(project.add_track("Kit"))).unwrap();
    let rack = session.add_device(ChainRef::Track(kit), "builtin:drumrack", None, None).unwrap();
    session.set_pad_file(rack, 36, Some(&kick.display().to_string())).unwrap();
    session.set_pad_file(rack, 38, Some(&snare.display().to_string())).unwrap();
    session
        .edit(|project| {
            let clip = project.add_midi_clip(kit, "Beat", 0.0, 4.0)?;
            project.clip_mut(clip)?.add_notes(&[Note { pitch: 36, start: 0.0, length: 0.5, velocity: 100 }])
        })
        .unwrap();
    std::fs::create_dir_all(song.parent().unwrap()).unwrap();
    session.save(song).unwrap();
}

fn move_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(from, to).unwrap();
}

/// Each missing file by its name, with how sure the best thing found for
/// it is.
fn found(missing: &Missing) -> Vec<(String, Option<Confidence>)> {
    let name = |path: &str| Path::new(path).file_name().unwrap().to_string_lossy().into_owned();
    missing.files.iter().map(|file| (name(&file.path), file.found().map(|candidate| candidate.confidence))).collect()
}

fn why(missing: &Missing, name: &str) -> String {
    let file = missing.files.iter().find(|file| file.path.ends_with(name)).expect("the file is missing");
    file.found().map(|candidate| candidate.why.clone()).unwrap_or_default()
}

fn scanned(runtime: &Runtime) {
    assert!(runtime.library.wait_until_idle(WAIT), "the library is still scanning");
}

#[test]
fn files_whose_folder_went_elsewhere_are_found_through_the_library_and_relinked_in_one_step() {
    let folder = tempfile::tempdir().unwrap();
    let old = folder.path().join("Music").join("packs");
    // Far from where it was: only a library that covers it finds it.
    let new = folder.path().join("Other Drive").join("a").join("b").join("c").join("d").join("packs");
    let song_file = folder.path().join("Songs").join("night.trill");
    let runtime = start(folder.path());
    let files = pack(&old);
    song(&runtime, &files, &song_file);
    assert!(runtime.files_missing().is_empty() && runtime.missing_files(&Look::default()).is_empty());

    move_folder(&old, &new);
    runtime.session.lock().load(&song_file).unwrap();

    // What is missing is said without looking for anything.
    let missing = runtime.files_missing();
    assert_eq!((missing.files.len(), missing.places()), (3, 4));
    assert_eq!(missing.tracks(), ["FX", "Kit"]);
    assert_eq!(missing.files[0].uses.len(), 2, "the loop, in both its clips");
    assert_eq!(missing.files[0].path, files[2].display().to_string());
    assert!(missing.files.iter().all(|file| file.candidates.is_empty()));
    let known = missing.files[0].known.as_ref().expect("the song wrote down what its file was");
    assert_eq!((known.frames, known.sample_rate), (Some(88_200), Some(44_100)));

    // Nobody knows where the folder went.
    let lost = runtime.missing_files(&Look::default());
    assert_eq!(lost.found(Confidence::Unsure), 0, "{lost:#?}");
    assert!(lost.moves.is_empty() && lost.relinks(Confidence::Unsure).is_empty());

    // The library is pointed at the new place, as on a machine whose
    // library was put right first.
    runtime.library.add_root(new.parent().unwrap());
    scanned(&runtime);
    let missing = runtime.missing_files(&Look::default());
    let sure = Some(Confidence::Sure);
    assert_eq!(
        found(&missing),
        [
            ("Bass Loop 120bpm.wav".to_owned(), sure),
            ("Kick 01.wav".to_owned(), sure),
            ("Snare Tight.wav".to_owned(), sure)
        ]
    );
    assert_eq!(missing.moves, [(FolderMove::new(old.to_string_lossy(), new.to_string_lossy()), 3)]);
    assert_eq!(missing.found_in(Confidence::Sure).as_deref(), Some(&*new.to_string_lossy()));
    let said = why(&missing, "Kick 01.wav");
    assert!(said.starts_with("where its folder went, like 2 other files of the song"), "{said}");
    assert!(said.ends_with("and it has the size and length of the file the song knew"), "{said}");
    assert_eq!(missing.files[0].found().unwrap().seconds, Some(2.0));
    assert!(missing.folders.is_empty());

    // Finding relinks nothing.
    assert_eq!(runtime.files_missing().places(), 4);
    assert!(!runtime.session.lock().can_undo());

    // Relinked: all of them, one step.
    let relinks = missing.relinks(Confidence::Sure);
    assert_eq!(relinks.len(), 4);
    assert_eq!(runtime.relink(&relinks).unwrap(), 4);
    assert!(runtime.files_missing().is_empty());
    {
        let mut session = runtime.session.lock();
        assert!(session.problems().is_empty(), "{:?}", session.problems());
        let now: Vec<String> = session.files().into_iter().map(|file| file.path).collect();
        assert!(now.iter().all(|path| Path::new(path).starts_with(&new)), "{now:?}");
        assert!(session.undo() && !session.can_undo(), "it was one step");
    }
    assert_eq!(runtime.files_missing().places(), 4);
    assert!(runtime.session.lock().redo());
    assert!(runtime.missing_files(&Look::default()).is_empty());
}

#[test]
fn a_library_folder_that_was_moved_nearby_is_found_with_the_songs_files_and_put_right_with_its_favorites() {
    let folder = tempfile::tempdir().unwrap();
    let old = folder.path().join("Music").join("packs");
    let new = folder.path().join("Music").join("trill_samples").join("packs");
    let song_file = folder.path().join("Songs").join("night.trill");
    let files = pack(&old);
    {
        let runtime = start(folder.path());
        runtime.library.add_root(&old);
        scanned(&runtime);
        runtime.library.set_favorite(&files[0], true);
        runtime.library.mark_used(&files[2]);
        song(&runtime, &files, &song_file);
        runtime.save_library_changes().unwrap();
    }
    // Moved while the program was not running. A folder that merely has
    // the name is nearby too.
    move_folder(&old, &new);
    tone(folder.path().join("Music").join("backup").join("packs").join("unrelated.wav"), 0.1);

    let runtime = start(folder.path());
    scanned(&runtime);
    assert_eq!(runtime.library.roots(), [old.clone()], "the library still has the folder where it was");
    runtime.session.lock().load(&song_file).unwrap();

    let missing = runtime.missing_files(&Look::default());
    assert_eq!(missing.folders.len(), 1);
    let gone = &missing.folders[0];
    let there = gone.found.as_ref().expect("it was looked for near where it was");
    assert_eq!((gone.path.as_path(), gone.files, there.path.as_path()), (old.as_path(), 5, new.as_path()));
    assert!(there.sure() && there.checked == 5 && there.there == 5, "{there:?}");
    assert_eq!(missing.library_moves(), [(old.clone(), new.clone())]);
    // The same move finds the song's files, and the song knew them.
    assert_eq!(missing.found(Confidence::Sure), 3, "{missing:#?}");
    let said = why(&missing, "Snare Tight.wav");
    assert!(said.starts_with("where the library's folder went, and it has the size and length"), "{said}");
    assert_eq!(missing.found_in(Confidence::Sure).as_deref(), Some(&*new.to_string_lossy()));

    // Both put right.
    assert_eq!(runtime.relink(&missing.relinks(Confidence::Sure)).unwrap(), 4);
    let moved = runtime.move_library_folder(&old, &new).unwrap();
    assert_eq!((moved.files, moved.favorites, moved.recent), (5, 1, 1));
    scanned(&runtime);
    assert!(runtime.missing_files(&Look::default()).is_empty());
    let kick = new.join("Kit One").join("Kicks").join("Kick 01.wav");
    assert!(runtime.library.is_favorite(&kick));
    // And it is in the settings, for the next run.
    let saved = std::fs::read_to_string(folder.path().join("config").join("settings.json")).unwrap();
    assert!(
        saved.contains("trill_samples") && !saved.contains(&old.to_string_lossy().replace('\\', "\\\\")),
        "{saved}"
    );

    // A folder that cannot be the one is refused in words.
    let error = runtime.move_library_folder(&new, &folder.path().join("Nowhere")).unwrap_err();
    assert!(error.contains("cannot read"), "{error}");
}

#[test]
fn a_folder_moved_within_the_folder_it_was_in_is_found_without_a_library() {
    let folder = tempfile::tempdir().unwrap();
    let old = folder.path().join("Music").join("packs");
    let new = folder.path().join("Music").join("sorted").join("2024").join("packs");
    let song_file = folder.path().join("Songs").join("night.trill");
    let runtime = start(folder.path());
    let files = pack(&old);
    song(&runtime, &files, &song_file);
    move_folder(&old, &new);
    runtime.session.lock().load(&song_file).unwrap();

    let missing = runtime.missing_files(&Look::default());
    assert_eq!(missing.found(Confidence::Sure), 3, "{missing:#?}");
    assert_eq!(missing.moves, [(FolderMove::new(old.to_string_lossy(), new.to_string_lossy()), 3)]);
    assert_eq!(runtime.relink(&missing.relinks(Confidence::Sure)).unwrap(), 4);
    assert!(runtime.files_missing().is_empty());
}

#[test]
fn a_file_that_merely_has_the_name_is_not_taken_for_the_file() {
    let folder = tempfile::tempdir().unwrap();
    let old = folder.path().join("Music").join("packs");
    let library = folder.path().join("Library");
    let song_file = folder.path().join("Songs").join("night.trill");
    let runtime = start(folder.path());
    let files = pack(&old);
    song(&runtime, &files, &song_file);
    // The folder is gone for good. Elsewhere there are files of the same
    // names: a kick that is another kick (longer), the very snare, and a
    // loop that is shorter than the song's clip plays.
    std::fs::remove_dir_all(&old).unwrap();
    tone(library.join("Drums").join("Kick 01.wav"), 0.5);
    tone(library.join("Drums").join("Snare Tight.wav"), 0.25);
    tone(library.join("Bass Loop 120bpm.wav"), 1.0);
    runtime.library.add_root(&library);
    scanned(&runtime);
    runtime.session.lock().load(&song_file).unwrap();

    let missing = runtime.missing_files(&Look::default());
    assert_eq!(
        found(&missing),
        [
            ("Bass Loop 120bpm.wav".to_owned(), Some(Confidence::Unsure)),
            ("Kick 01.wav".to_owned(), Some(Confidence::Unsure)),
            ("Snare Tight.wav".to_owned(), Some(Confidence::Sure)),
        ],
        "{missing:#?}"
    );
    let said = why(&missing, "Kick 01.wav");
    assert_eq!(
        said,
        "the only file of that name in the library, but it is not the file the song knew: 0.50 s long where that was 0.20 s"
    );
    let said = why(&missing, "Snare Tight.wav");
    assert_eq!(
        said,
        "the only file of that name in the library, and it has the size and length of the file the song knew"
    );
    // Only what is sure is taken without being looked at.
    assert_eq!(missing.relinks(Confidence::Sure).len(), 1);
    assert_eq!(missing.relinks(Confidence::Likely).len(), 1);
    assert_eq!(missing.relinks(Confidence::Unsure).len(), 4, "everything found, for whoever has looked");

    // A second snare of the same name: the one that is what the song knew
    // is still the file, and the other is said not to be.
    tone(library.join("Old").join("Snare Tight.wav"), 0.4);
    runtime.library.rescan();
    scanned(&runtime);
    let missing = runtime.missing_files(&Look::default());
    let snare = missing.files.iter().find(|file| file.path.ends_with("Snare Tight.wav")).unwrap();
    assert_eq!(snare.candidates.len(), 2);
    assert_eq!(snare.candidates[0].confidence, Confidence::Sure);
    assert!(snare.candidates[0].path.contains("Drums") && snare.candidates[0].why.starts_with("one of 2 files"));
    assert_eq!(snare.candidates[1].confidence, Confidence::Unsure);

    // The same song from a file that wrote nothing down about its files,
    // as every song saved before this was: now nothing tells the files of
    // a name apart, and how far a clip plays is all there is to go by.
    let plain = folder.path().join("Songs").join("plain.trill");
    std::fs::write(&plain, runtime.session.lock().project().to_json()).unwrap();
    runtime.session.lock().load(&plain).unwrap();
    let missing = runtime.missing_files(&Look::default());
    assert!(missing.files.iter().all(|file| file.known.is_none()));
    assert_eq!(
        found(&missing),
        [
            ("Bass Loop 120bpm.wav".to_owned(), Some(Confidence::Unsure)),
            ("Kick 01.wav".to_owned(), Some(Confidence::Likely)),
            ("Snare Tight.wav".to_owned(), Some(Confidence::Unsure)),
        ],
        "{missing:#?}"
    );
    assert_eq!(why(&missing, "Kick 01.wav"), "the only file of that name in the library");
    assert_eq!(why(&missing, "Snare Tight.wav"), "one of 2 files of that name");
    let said = why(&missing, "Bass Loop 120bpm.wav");
    assert_eq!(
        said,
        "the only file of that name in the library, but it is 1.00 s long and the clip \u{201C}Loop\u{201D} plays 2.00 s of its file"
    );
    assert!(missing.relinks(Confidence::Sure).is_empty(), "nothing is relinked on a name");
}

#[test]
fn a_folder_or_one_file_pointed_at_by_hand_leads_to_the_rest() {
    let folder = tempfile::tempdir().unwrap();
    let old = folder.path().join("Music").join("packs");
    // Renamed as well as moved, and far away.
    let new = folder.path().join("Archive").join("a").join("b").join("c").join("d").join("Sample Packs");
    let song_file = folder.path().join("Songs").join("night.trill");
    let runtime = start(folder.path());
    let files = pack(&old);
    song(&runtime, &files, &song_file);
    move_folder(&old, &new);
    // Another file of the kick's name, inside the folder too.
    tone(new.join("Old Kicks").join("Kick 01.wav"), 0.3);
    runtime.session.lock().load(&song_file).unwrap();
    assert_eq!(runtime.missing_files(&Look::default()).found(Confidence::Unsure), 0);

    // A folder is pointed at: the files are found in it by their names,
    // and where they are says where the folder went.
    let look = Look { folders: vec![folder.path().join("Archive")], ..Look::default() };
    let missing = runtime.missing_files(&look);
    assert_eq!(missing.found(Confidence::Sure), 3, "{missing:#?}");
    assert_eq!(missing.moves[0], (FolderMove::new(old.to_string_lossy(), new.to_string_lossy()), 3));
    let kick = missing.files.iter().find(|file| file.path.ends_with("Kick 01.wav")).unwrap();
    assert_eq!(kick.candidates.len(), 2, "the other kick is offered, and is not the one");
    assert!(kick.candidates[0].path.contains("Kit One") && kick.candidates[1].confidence == Confidence::Unsure);

    // One file is pointed at instead: its old and new place say where its
    // folder went, and the others are looked for there on that word.
    let new_kick = new.join("Kit One").join("Kicks").join("Kick 01.wav");
    let (moved, alike) = FolderMove::between(&files[0].to_string_lossy(), &new_kick.to_string_lossy()).unwrap();
    assert_eq!(alike, 3);
    let missing = runtime.missing_files(&Look { moves: vec![moved], ..Look::default() });
    assert_eq!(missing.found(Confidence::Sure), 3);
    assert!(why(&missing, "Snare Tight.wav").starts_with("where the folder that was named puts it"));
    assert_eq!(runtime.relink(&missing.relinks(Confidence::Sure)).unwrap(), 4);
    assert!(runtime.files_missing().is_empty());

    // A file given by hand that cannot be read changes nothing.
    let missing_before = runtime.files_missing();
    let place = runtime.session.lock().files()[0].place.clone();
    let text = folder.path().join("notes.txt");
    std::fs::write(&text, "not audio").unwrap();
    let error = runtime.relink(&[trill_core::Relink { place, path: text.display().to_string() }]).unwrap_err();
    assert!(error.to_string().contains("nothing was relinked"), "{error}");
    assert_eq!(runtime.files_missing(), missing_before);
}
