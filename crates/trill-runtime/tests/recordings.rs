//! Which recordings a song no longer uses is a question about every song
//! Trill knows of, not only the one that is open: a song saved under
//! another name plays the files of the song it was saved from, and unsaved
//! work that can still be recovered plays the takes it was lost with.
//!
//! Devices that exist only here, and a folder of the test's own: nothing
//! of the machine's is opened, and nothing of its user's read or written.
//! A recording that is removed here goes into a folder beside the takes,
//! which is where a run that keeps nothing of the user's puts it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::{TrackId, TrackInput};
use trill_engine::AudioBackend;
use trill_engine::host::scripted::{Hears, ScriptedHost, ScriptedInput};
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

/// A runtime that keeps what it keeps under `folder`, on a host whose
/// microphone hears a tone.
fn runtime_in(folder: &Path) -> (Runtime, ScriptedHost) {
    let host = ScriptedHost::with_speakers();
    host.add_input(ScriptedInput::new("Microphone"));
    host.hear(Hears::Sound(Arc::new(|_, seconds| (0.25 * (seconds * std::f64::consts::TAU * 440.0).sin()) as f32)));
    let storage = Storage::At { settings: folder.join("config").join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let parts = Parts { audio: Some(Arc::new(host.clone())), ..Parts::default() };
    (Runtime::start_custom(options, parts).unwrap(), host)
}

/// A track that records the microphone, armed by the user's own hand.
fn voice_track(runtime: &Runtime, host: &ScriptedHost) -> TrackId {
    let _hand = trill_engine::hand::by_hand();
    let mut session = runtime.session.lock();
    session.set_count_in(0);
    let voice = session.edit(|project| Ok(project.add_track("Voice"))).unwrap();
    session.edit(|project| project.set_track_input(voice, Some(TrackInput::mono(1)))).unwrap();
    session.set_armed(voice, true).unwrap();
    session.ready_input().unwrap();
    drop(session);
    host.advance(24_000);
    voice
}

/// Records a second of take on the armed tracks, and returns its file.
fn a_take(runtime: &Runtime, host: &ScriptedHost) -> PathBuf {
    {
        let mut session = runtime.session.lock();
        session.seek(0.0);
        session.record().unwrap();
    }
    host.advance(48_000);
    let mut session = runtime.session.lock();
    session.stop();
    session.last_recording().expect("something was recorded").takes[0].file.clone()
}

fn save(runtime: &Runtime, song: &Path) {
    runtime.session.lock().save(song).unwrap();
    runtime.song_is_at(song);
}

fn open(runtime: &Runtime, song: &Path) {
    runtime.session.lock().load(song).unwrap();
    runtime.song_is_at(song);
}

/// Takes the take that plays `file`, and what the track plays of it, out
/// of the open song.
fn let_go_of(runtime: &Runtime, voice: TrackId, file: &Path) {
    let mut session = runtime.session.lock();
    let names = |clip: &trill_core::Clip| clip.audio().is_some_and(|audio| Path::new(&audio.path) == file);
    session
        .edit(|project| {
            let track = project.track(voice)?;
            let takes: Vec<_> = track.takes.iter().filter(|take| names(&take.clip)).map(|take| take.clip.id).collect();
            let clips: Vec<_> = track.clips.iter().filter(|clip| names(clip)).map(|clip| clip.id).collect();
            for take in takes {
                project.remove_take(take)?;
            }
            for clip in clips {
                project.remove_clip(clip)?;
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_recording_that_a_song_saved_under_another_name_plays_is_not_unused_and_is_not_removed() {
    let folder = tempfile::tempdir().unwrap();
    let (runtime, host) = runtime_in(folder.path());
    let voice = voice_track(&runtime, &host);
    // Two takes in "Song v1", which is then saved under another name:
    // "Song v2" goes on naming the files beside v1.
    let (v1, v2) = (folder.path().join("Song v1.trill"), folder.path().join("Song v2.trill"));
    a_take(&runtime, &host);
    a_take(&runtime, &host);
    save(&runtime, &v1);
    let takes: Vec<PathBuf> = {
        let session = runtime.session.lock();
        let track = session.project().track(voice).unwrap();
        track.takes.iter().map(|take| take.clip.audio().unwrap().path.clone().into()).collect()
    };
    let beside_v1 = folder.path().join("Song v1 files").join("takes");
    assert_eq!(takes, [beside_v1.join("Voice.wav"), beside_v1.join("Voice 2.wav")]);
    save(&runtime, &v2);
    assert!(!folder.path().join("Song v2 files").exists(), "saving under another name leaves the takes where they are");
    let in_v2 = std::fs::read_to_string(&v2).unwrap();
    assert!(in_v2.contains("Song v1 files"), "and the new song names them there");

    // Back in v1, the first take is let go of, and the song saved and
    // opened again so that no undo can bring it back.
    open(&runtime, &v1);
    let_go_of(&runtime, voice, &takes[0]);
    save(&runtime, &v1);
    open(&runtime, &v1);
    assert_eq!(runtime.session.lock().unused_recordings().len(), 1, "by the open song alone it would go");
    let listed = runtime.unused_recordings();
    assert!(listed.unused.is_empty(), "{listed:?}");
    assert_eq!(listed.played_elsewhere.len(), 1);
    assert_eq!(listed.played_elsewhere[0].path, takes[0]);
    assert!(listed.played_elsewhere[0].by[0].contains("Song v2.trill"), "{:?}", listed.played_elsewhere[0].by);
    assert_eq!(listed.goes, format!("to {}", beside_v1.join("removed").display()));
    let refused = runtime.delete_unused_recordings(std::slice::from_ref(&takes[0])).unwrap_err();
    assert!(refused.contains("is played by") && refused.contains("Song v2.trill"), "{refused}");
    assert!(refused.contains("nothing was removed") && takes[0].is_file());

    // Once v2 has let go of it too, nothing plays it: it is listed, and
    // removed when that is asked for, into a folder and not out of
    // existence.
    open(&runtime, &v2);
    let_go_of(&runtime, voice, &takes[0]);
    save(&runtime, &v2);
    open(&runtime, &v1);
    let listed = runtime.unused_recordings();
    assert_eq!(listed.unused.iter().map(|(path, _)| path.clone()).collect::<Vec<_>>(), [takes[0].clone()]);
    assert!(listed.played_elsewhere.is_empty());
    let held = std::fs::read(&takes[0]).unwrap();
    let removed = runtime.delete_unused_recordings(std::slice::from_ref(&takes[0])).unwrap();
    assert_eq!(removed.set_aside, [beside_v1.join("removed").join("Voice.wav")]);
    assert_eq!(removed.went(), format!("to {}", beside_v1.join("removed").display()));
    assert!(!takes[0].exists());
    assert_eq!(std::fs::read(&removed.set_aside[0]).unwrap(), held);
    // What both songs still play is there, and plays.
    assert!(takes[1].is_file());
    assert!(runtime.session.lock().problems().is_empty());
}

#[test]
fn a_recording_that_unsaved_work_would_bring_back_is_not_unused() {
    let folder = tempfile::tempdir().unwrap();
    let song = folder.path().join("Song.trill");
    // A program saves a song, records a take into it, keeps its copy for
    // safety, and is gone before the take was saved.
    let take = {
        let (lost, host) = runtime_in(folder.path());
        voice_track(&lost, &host);
        save(&lost, &song);
        let take = a_take(&lost, &host);
        lost.refresh_safety_copy();
        lost.abandon();
        take
    };
    assert!(take.starts_with(folder.path().join("Song files").join("takes")) && take.is_file());

    // The next program opens the song as it was saved, without taking
    // the recovery: the take is no part of what is open.
    let (runtime, _host) = runtime_in(folder.path());
    assert_eq!(runtime.recoverable().len(), 1);
    open(&runtime, &song);
    assert_eq!(runtime.session.lock().unused_recordings().len(), 1, "by the open song alone it would go");
    let listed = runtime.unused_recordings();
    assert!(listed.unused.is_empty(), "{listed:?}");
    assert_eq!(listed.played_elsewhere[0].path, take);
    assert!(
        listed.played_elsewhere[0].by[0].contains("unsaved work")
            && listed.played_elsewhere[0].by[0].contains("recovered")
    );
    let refused = runtime.delete_unused_recordings(std::slice::from_ref(&take)).unwrap_err();
    assert!(refused.contains("unsaved work"), "{refused}");
    assert!(take.is_file());

    // The recovery is taken: the take is back, with its recording.
    let copy = runtime.recoverable().remove(0);
    runtime.recover(&copy).unwrap();
    let session = runtime.session.lock();
    let voice = session.project().tracks[0].id;
    assert_eq!(session.project().track(voice).unwrap().takes.len(), 1);
    assert!(session.problems().is_empty(), "{:?}", session.problems());
}
