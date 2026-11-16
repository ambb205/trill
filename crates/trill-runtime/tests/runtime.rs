//! The runtime as a front end uses it: started, used, dropped and started
//! again, with its settings and library in temporary folders.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use trill_core::audio::AudioBuffer;
use trill_core::project::WarpMode;
use trill_core::{ChainRef, TrackId};
use trill_engine::AudioBackend;
use trill_library::{Query, Settings, Tag};
use trill_media::{WavFormat, write_wav};
use trill_runtime::{NewSoundTrack, Runtime, RuntimeOptions, SoundClip, SoundError, Storage};

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

/// Writes `seconds` of a 220 Hz tone and returns the file's path.
fn tone(path: PathBuf, seconds: f64) -> PathBuf {
    let rate = 44_100.0;
    let channel: Vec<f32> = (0..(seconds * rate) as usize)
        .map(|n| (n as f64 * 220.0 * std::f64::consts::TAU / rate).sin() as f32 * 0.5)
        .collect();
    write_wav(&path, &AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }, WavFormat::Int16)
        .unwrap();
    path
}

/// A small sample pack: two drum hits and a four-beat loop at 100 BPM.
fn pack(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        tone(root.join("Drums").join("Kick 01.wav"), 0.2),
        tone(root.join("Drums").join("Snare Tight.wav"), 0.25),
        tone(root.join("Loops").join("Bass Loop 100bpm.wav"), 2.4),
    )
}

/// Polls until `done` or the time is up.
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn track(runtime: &Runtime) -> TrackId {
    runtime.session.lock().edit(|project| Ok(project.add_track("Audio"))).unwrap()
}

#[test]
fn library_folders_favorites_and_recents_survive_a_restart() {
    let folder = tempfile::tempdir().unwrap();
    let samples = folder.path().join("samples");
    let (kick, snare, _) = pack(&samples);
    let settings_file = folder.path().join("config").join("settings.json");

    let runtime = start(folder.path());
    assert!(runtime.library.roots().is_empty() && runtime.settings_warning().is_none());
    // Starting, looking and asking to save what has not changed write nothing.
    runtime.save_library_changes().unwrap();
    assert!(!settings_file.exists() && !folder.path().join("cache").exists(), "an untouched run leaves no files");
    assert!(runtime.library.add_root(&samples));
    assert!(runtime.library.wait_until_idle(WAIT));
    assert_eq!(runtime.library.search(&Query::text("kick")).len(), 1);
    runtime.library.set_favorite(&snare, true);
    runtime.preview(&kick, -12.0).unwrap();

    // Nobody said "save": the housekeeping thread notices by itself.
    eventually("the settings are written", || {
        let saved = Settings::load(&settings_file).settings.library;
        saved.roots == [samples.clone()] && saved.favorites == [snare.clone()] && saved.recent == [kick.clone()]
    });
    // What a front end keeps there is saved on request and left alone by
    // the library's saves.
    runtime.update_settings(|settings| settings.ui.browser_width = 333.0).unwrap();
    runtime.library.set_favorite(&kick, true);
    drop(runtime);
    let saved = Settings::load(&settings_file).settings;
    assert_eq!(saved.library.favorites, [snare.clone(), kick.clone()], "dropping the runtime saves what changed last");
    assert_eq!(saved.ui.browser_width, 333.0);

    // The next run starts where this one stopped, searchable at once.
    let runtime = start(folder.path());
    assert_eq!(runtime.library.roots(), std::slice::from_ref(&samples));
    assert_eq!(runtime.library.search(&Query::text("kick")).len(), 1, "the saved index is loaded before any scan");
    assert!(runtime.library.is_favorite(&snare) && runtime.library.recent() == [kick.clone()]);
    assert_eq!(runtime.settings().ui.browser_width, 333.0);
    assert_eq!(runtime.settings().library.roots, std::slice::from_ref(&samples));

    // Another program changed its own part of the file meanwhile; saving
    // the library's part keeps that change.
    let mut other = Settings::load(&settings_file).settings;
    other.ui.theme = "graphite".into();
    other.save(&settings_file).unwrap();
    assert!(runtime.library.remove_root(&samples));
    runtime.save_library_changes().unwrap();
    let saved = Settings::load(&settings_file).settings;
    assert!(saved.library.roots.is_empty());
    assert_eq!((saved.ui.theme.as_str(), saved.ui.browser_width), ("graphite", 333.0));
}

#[test]
fn a_hermetic_runtime_reads_and_writes_nothing() {
    let runtime = Runtime::start(RuntimeOptions::hermetic()).unwrap();
    assert_eq!(runtime.settings(), Settings::default(), "defaults, whatever the user's settings say");
    assert!(runtime.library.roots().is_empty() && runtime.library.index().is_empty());
    let status = runtime.library.scan_status();
    assert!(!status.is_scanning() && status.last.is_none(), "nothing is scanned");
    assert!(runtime.plugins().is_none());
    assert_eq!(runtime.session.lock().audio_info().sample_rate, 48_000.0);

    // Settings can still be changed; they last as long as the runtime.
    runtime.update_settings(|settings| settings.ui.theme = "graphite".into()).unwrap();
    assert_eq!(runtime.settings().ui.theme, "graphite");
    runtime.library.set_favorite(Path::new("somewhere/kick.wav"), true);
    runtime.save_library_changes().unwrap();
}

#[test]
fn sounds_are_described_previewed_and_placed() {
    let folder = tempfile::tempdir().unwrap();
    let samples = folder.path().join("samples");
    let (kick, _, bass_loop) = pack(&samples);
    let runtime = start(folder.path());

    // Names are read the same way whether or not the library has the file.
    let facts = runtime.sound_facts(&bass_loop).unwrap();
    assert!((facts.seconds - 2.4).abs() < 1e-3);
    assert_eq!((facts.sample_rate, facts.channels, facts.tempo), (44_100, 2, Some(100.0)));
    assert!(facts.tags.contains(Tag::Loop) && facts.tags.contains(Tag::Bass), "{:?}", facts.tags);
    runtime.library.add_root(&samples);
    assert!(runtime.library.wait_until_idle(WAIT));
    assert_eq!(runtime.sound_facts(&bass_loop).unwrap(), facts, "indexed or not, the same answer");
    let hit = runtime.sound_facts(&kick).unwrap();
    assert!(hit.tempo.is_none() && hit.tags.contains(Tag::Kick) && hit.tags.contains(Tag::OneShot));
    assert!(runtime.sound_facts(&samples.join("nothing.wav")).unwrap_err().to_string().contains("does not exist"));

    // Previewing plays it and remembers it; stopping fades it out.
    assert!((runtime.preview(&bass_loop, -6.0).unwrap() - 2.4).abs() < 1e-3);
    eventually("the preview is heard", || runtime.session.lock().is_auditioning());
    runtime.stop_preview();
    eventually("the preview has stopped", || !runtime.session.lock().is_auditioning());
    assert_eq!(runtime.library.recent(), std::slice::from_ref(&bass_loop));
    assert!(runtime.preview(&samples.join("nothing.wav"), 0.0).is_err());

    // Placed as it is: as long as the file, at the project's tempo.
    let track = track(&runtime);
    runtime.session.lock().edit(|project| project.set_tempo(120.0)).unwrap();
    let plain = runtime.add_sound(&SoundClip::new(&bass_loop, track, 4.0)).unwrap();
    assert!((plain.length - 4.8).abs() < 1e-3, "2.4 s at 120 BPM is 4.8 beats: {}", plain.length);
    assert_eq!(plain.source_tempo, Some(100.0), "the tempo in the name is kept for warping later");
    // Warped: its four recorded beats are four beats of the song.
    let warped = SoundClip { warp: WarpMode::Beats, ..SoundClip::new(&bass_loop, track, 12.0) };
    let warped = runtime.add_sound(&warped).unwrap();
    assert!((warped.length - 4.0).abs() < 1e-3, "{}", warped.length);
    {
        let session = runtime.session.lock();
        let clip = session.project().clip(warped.clip).unwrap();
        let audio = clip.audio().unwrap();
        assert_eq!(
            (clip.name.as_str(), clip.start, audio.warp, audio.source_tempo),
            ("Bass Loop 100bpm", 12.0, WarpMode::Beats, Some(100.0))
        );
        assert!(Path::new(&audio.path).is_absolute());
        assert!(session.problems().is_empty(), "{:?}", session.problems());
    }
    // The runtime's own thread swaps the stretched audio in.
    eventually("the warped clip is prepared", || runtime.session.lock().preparing() == 0);

    // A given tempo beats the name; an offset shortens the default length.
    let custom = SoundClip {
        warp: WarpMode::Tonal,
        source_tempo: Some(50.0),
        offset_seconds: 1.2,
        name: Some("Half".into()),
        ..SoundClip::new(&bass_loop, track, 20.0)
    };
    let custom = runtime.add_sound(&custom).unwrap();
    assert!((custom.length - 1.0).abs() < 1e-3, "1.2 s left at 50 BPM is one beat: {}", custom.length);

    // What cannot be done changes nothing and says what to do instead.
    let before = runtime.session.lock().project().clone();
    let refusals = [
        (SoundClip { warp: WarpMode::Beats, ..SoundClip::new(&kick, track, 0.0) }, "source tempo"),
        (SoundClip { offset_seconds: 9.0, ..SoundClip::new(&kick, track, 0.0) }, "past the end"),
        (SoundClip::new(samples.join("nothing.wav"), track, 0.0), "does not exist"),
        (SoundClip::new(&kick, TrackId(999), 0.0), "track 999 not found"),
        (SoundClip { transpose: 99.0, ..SoundClip::new(&kick, track, 0.0) }, "transpose"),
        (SoundClip { length: Some(0.0), ..SoundClip::new(&kick, track, 0.0) }, "length"),
    ];
    for (sound, expected) in refusals {
        let error = runtime.add_sound(&sound).expect_err(expected);
        assert!(error.to_string().contains(expected), "'{error}' should mention '{expected}'");
        assert!(!matches!(error, SoundError::Invalid(ref message) if message.is_empty()));
    }
    assert_eq!(runtime.session.lock().project(), &before, "a refused sound must change nothing");

    // Each placed sound was one undo step.
    let mut session = runtime.session.lock();
    assert!(session.undo() && session.undo() && session.undo());
    assert!(session.project().tracks[0].clips.is_empty());
}

#[test]
fn a_sound_on_a_track_of_its_own_is_one_undo_step() {
    let folder = tempfile::tempdir().unwrap();
    let (kick, _, bass_loop) = pack(&folder.path().join("samples"));
    let runtime = start(folder.path());
    let before = runtime.session.lock().project().clone();

    // The track is named after the file unless told otherwise, goes last
    // and holds the clip; `track` of the request is not looked at.
    let sound = SoundClip::new(&bass_loop, TrackId(999), 8.0);
    let (track, added) = runtime.add_sound_on_new_track(&sound, &NewSoundTrack::default()).unwrap();
    let named = NewSoundTrack { name: Some("Drums".into()), color: Some(0x4EC9B0) };
    let (second, _) = runtime.add_sound_on_new_track(&SoundClip::new(&kick, track, 0.0), &named).unwrap();
    {
        let session = runtime.session.lock();
        let project = session.project();
        let ids: Vec<TrackId> = project.tracks.iter().map(|track| track.id).collect();
        assert_eq!(ids, [track, second]);
        let first = project.track(track).unwrap();
        assert_eq!((first.name.as_str(), first.devices.len(), first.clips.len()), ("Bass Loop 100bpm", 0, 1));
        assert_eq!((first.clips[0].id, first.clips[0].start), (added.clip, 8.0));
        let drums = project.track(second).unwrap();
        assert_eq!((drums.name.as_str(), drums.color, drums.clips.len()), ("Drums", 0x4EC9B0, 1));
        assert!(session.problems().is_empty(), "{:?}", session.problems());
    }

    // A sound that cannot be placed leaves no empty track behind.
    let depth = runtime.session.lock().project().tracks.len();
    let missing = SoundClip::new(folder.path().join("nothing.wav"), track, 0.0);
    assert!(runtime.add_sound_on_new_track(&missing, &NewSoundTrack::default()).is_err());
    let unwarpable = SoundClip { warp: WarpMode::Beats, ..SoundClip::new(&kick, track, 0.0) };
    assert!(runtime.add_sound_on_new_track(&unwarpable, &NewSoundTrack::default()).is_err());
    let negative = SoundClip::new(&kick, track, -4.0);
    assert!(runtime.add_sound_on_new_track(&negative, &NewSoundTrack::default()).is_err());
    assert_eq!(runtime.session.lock().project().tracks.len(), depth);

    // Track and clip come and go together.
    let mut session = runtime.session.lock();
    assert!(session.undo());
    assert_eq!(session.project().tracks.len(), 1);
    assert!(session.undo());
    assert_eq!(session.project(), &before);
    assert!(!session.undo(), "two sounds were two steps");
}

#[test]
fn dropping_the_runtime_stops_its_thread_and_leaves_the_session_usable() {
    let folder = tempfile::tempdir().unwrap();
    let runtime = start(folder.path());
    let session = runtime.session.clone();
    let device = session.lock().add_device(ChainRef::Master, "builtin:sampler", None, None).unwrap();
    let started = Instant::now();
    drop(runtime);
    assert!(started.elapsed() < Duration::from_secs(5), "dropping the runtime hung");
    // A front end that still holds the session can go on using it.
    let sample = tone(folder.path().join("a.wav"), 0.1);
    session.lock().set_sampler_file(device, Some(&sample.display().to_string())).unwrap();
    assert!(session.lock().problems().is_empty());
    // Nothing about the library changed, so no settings file was written.
    assert!(!folder.path().join("config").exists(), "a run that changes no setting leaves no file behind");
}
