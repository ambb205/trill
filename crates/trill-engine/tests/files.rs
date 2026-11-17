//! The files a song plays, through the session: files that are gone and
//! given back in one step, and a song that travels with its samples.

mod common;

use common::*;
use std::path::Path;
use std::sync::Arc;
use trill_core::journal::Entry;
use trill_core::project::AudioContent;
use trill_core::{ChainRef, DeviceId, FilePlace, Note, Project, Relink};
use trill_dsp::Builtins;
use trill_engine::render::RenderOptions;
use trill_engine::{AudioBackend, Problem, Session};
use trill_media::MediaCache;

/// A session wired the way the application wires one, without the master
/// limiter.
fn session() -> Session {
    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter)).unwrap();
    session
}

fn render(session: &mut Session) -> Vec<f32> {
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.5, ..RenderOptions::default() };
    session.render_job().render(&options).unwrap().left
}

/// The samples of a song in `folder`: a riser of two seconds and a kick.
fn samples(folder: &Path) -> (String, String) {
    std::fs::create_dir_all(folder.join("fx")).unwrap();
    std::fs::create_dir_all(folder.join("kit")).unwrap();
    let riser = write_wav(&folder.join("fx").join("riser.wav"), 44_100.0, &sine(440.0, 44_100.0, 2.0, 0.5));
    let kick = write_wav(&folder.join("kit").join("kick.wav"), 44_100.0, &sine(110.0, 44_100.0, 0.5, 0.5));
    (riser, kick)
}

/// A song at 120 BPM: the riser twice on an audio track (beats 0 and 8),
/// and a drum rack whose only pad plays the kick at beat 4.
fn song(session: &mut Session, riser: &str, kick: &str) -> DeviceId {
    let fx = session.edit(|project| Ok(project.add_track("FX"))).unwrap();
    session
        .edit(|project| {
            project.add_audio_clip(fx, "Riser", 0.0, 4.0, AudioContent::new(riser))?;
            let again = AudioContent { offset_seconds: 0.25, gain_db: -6.0, ..AudioContent::new(riser) };
            project.add_audio_clip(fx, "Riser again", 8.0, 2.0, again)
        })
        .unwrap();
    let kit = session.edit(|project| Ok(project.add_track("Kit"))).unwrap();
    let rack = session.add_device(ChainRef::Track(kit), "builtin:drumrack", None, None).unwrap();
    session.set_pad_file(rack, 36, Some(kick)).unwrap();
    session.set_device_params(rack, &[("volume".into(), -3.0)]).unwrap();
    session
        .edit(|project| {
            let clip = project.add_midi_clip(kit, "Beat", 4.0, 4.0)?;
            project.clip_mut(clip)?.add_notes(&[Note { pitch: 36, start: 0.0, length: 0.5, velocity: 127 }])
        })
        .unwrap();
    rack
}

/// How loud the riser's two clips and the kick come out: beats 0, 8 and 4
/// at 120 BPM.
fn heard(session: &mut Session) -> [f32; 3] {
    let out = render(session);
    let at = |beat: f64| {
        let from = ((beat * 0.5 + 0.05) * SR) as usize;
        out.get(from..from + 4_800).map_or(0.0, peak)
    };
    [at(0.0), at(8.0), at(4.0)]
}

fn paths(session: &Session) -> Vec<String> {
    session.files().into_iter().map(|file| file.path).collect()
}

fn lines(session: &Session) -> Vec<String> {
    let journal = session.journal().expect("a journal is kept");
    journal.entries().filter(|entry| !entry.is_aside()).map(Entry::line).collect()
}

/// Every place of the song that plays a file under `from`, to the same
/// file under `to`.
fn all_under(session: &Session, from: &Path, to: &Path) -> Vec<Relink> {
    let moved = trill_core::FolderMove::new(from.to_string_lossy(), to.to_string_lossy());
    session
        .files()
        .into_iter()
        .filter_map(|file| Some(Relink { place: file.place, path: moved.apply(&file.path)? }))
        .collect()
}

#[test]
fn files_that_moved_are_given_back_in_one_undo_step_and_the_song_sounds_again() {
    let folder = folder();
    let (old, new) = (folder.path().join("packs"), folder.path().join("moved").join("packs"));
    let (riser, kick) = samples(&old);
    let song_file = folder.path().join("song.trill");
    let mut session = session();
    let rack = song(&mut session, &riser, &kick);
    let whole = heard(&mut session);
    println!("with its files: riser {:.3}, riser again {:.3}, kick {:.3}", whole[0], whole[1], whole[2]);
    assert!(whole.iter().all(|level| *level > 0.1), "{whole:?}");
    session.save(&song_file).unwrap();

    // The folder goes elsewhere, and the song is opened again.
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::rename(&old, &new).unwrap();
    session.load(&song_file).unwrap();
    assert_eq!(heard(&mut session), [0.0; 3], "nothing of the files is heard");
    let problems: Vec<String> = session.problems().iter().map(Problem::to_string).collect();
    assert_eq!(problems.len(), 3, "two clips and a pad: {problems:?}");
    // The song opened as it was saved, and nothing has been done to it yet.
    assert!(!session.is_modified() && !session.can_undo());
    assert_eq!(paths(&session), [riser.clone(), riser.clone(), kick.clone()]);
    // What was written down about the files when they were there is what
    // they will be known by.
    let known = session.known_files().get(&riser).cloned().expect("the riser was written down");
    assert_eq!((known.frames, known.sample_rate, known.channels), (Some(88_200), Some(44_100), Some(2)));
    assert_eq!(known.bytes, Some(std::fs::metadata(new.join("fx").join("riser.wav")).unwrap().len()));

    // Relinked: every place at once.
    let before = session.project().clone();
    let relinks = all_under(&session, &old, &new);
    assert_eq!(relinks.len(), 3);
    assert_eq!(session.relink(&relinks).unwrap(), 3);
    let (new_riser, new_kick) = (
        new.join("fx").join("riser.wav").display().to_string(),
        new.join("kit").join("kick.wav").display().to_string(),
    );
    assert_eq!(paths(&session), [new_riser.clone(), new_riser.clone(), new_kick.clone()]);
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    let back = heard(&mut session);
    println!("relinked:       riser {:.3}, riser again {:.3}, kick {:.3}", back[0], back[1], back[2]);
    assert_eq!(back, whole, "the song sounds as it did: each clip its stretch, the rack its settings");
    assert_eq!(lines(&session).last().map(String::as_str), Some("Relinked 2 files in 3 places"));
    assert_eq!(session.known_files().get(&new_riser).and_then(|file| file.frames), Some(88_200));

    // One step: taken back, the song is what it was when it was opened.
    assert!(session.undo());
    assert_eq!(session.project(), &before);
    assert!(!session.can_undo(), "it was one step");
    assert_eq!(heard(&mut session), [0.0; 3]);
    assert!(session.redo());
    assert_eq!(heard(&mut session), whole);

    // The same again changes nothing and makes no step.
    let revision = session.revision();
    assert_eq!(session.relink(&relinks).unwrap(), 0);
    assert_eq!(session.revision(), revision);

    // One file, by itself, goes by its name in the journal.
    let other = write_wav(&folder.path().join("Other Kick.wav"), 48_000.0, &sine(90.0, 48_000.0, 0.5, 0.5));
    let pad = FilePlace::Sample { device: rack, slot: "pad 36".into() };
    assert_eq!(session.relink(&[Relink { place: pad.clone(), path: other }]).unwrap(), 1);
    assert_eq!(
        lines(&session).last().map(String::as_str),
        Some("Relinked \u{201C}Other Kick.wav\u{201D}"),
        "{:#?}",
        lines(&session)
    );

    // What cannot be done changes nothing, and says what is wrong.
    let before = session.project().clone();
    let revision = session.revision();
    let gone = folder.path().join("nowhere").join("kick.wav").display().to_string();
    let clip = session.files()[0].place.clone();
    let refused = [
        (
            vec![Relink { place: clip.clone(), path: new_kick.clone() }, Relink { place: pad.clone(), path: gone }],
            "nothing was relinked",
        ),
        (
            vec![Relink { place: FilePlace::Clip(trill_core::ClipId(9_999)), path: new_kick.clone() }],
            "clip 9999 not found",
        ),
        (
            vec![Relink { place: FilePlace::Sample { device: rack, slot: "pad 40".into() }, path: new_kick.clone() }],
            "its files are in: pad 36",
        ),
        (vec![Relink { place: pad, path: song_file.display().to_string() }], "nothing was relinked"),
    ];
    for (relinks, expected) in refused {
        let error = session.relink(&relinks).unwrap_err().to_string();
        assert!(error.contains(expected), "'{error}' should say '{expected}'");
        assert_eq!((session.project(), session.revision()), (&before, revision), "{expected}");
    }
}

#[test]
fn a_song_saved_with_its_samples_beside_it_plays_them_wherever_the_folder_goes() {
    let folder = folder();
    let home = folder.path().join("Songs").join("Night");
    let (riser, kick) = samples(&home.join("samples"));
    let mut session = session();
    song(&mut session, &riser, &kick);
    let whole = heard(&mut session);
    session.save(&home.join("night.trill")).unwrap();

    // The file says where each sample is from the song, and what it was.
    let text = std::fs::read_to_string(home.join("night.trill")).unwrap();
    assert!(text.contains(r#""beside": "samples/fx/riser.wav""#), "{text}");
    assert!(text.contains(r#""beside": "samples/kit/kick.wav""#));
    assert!(text.contains(r#""frames": 88200"#) && text.contains(r#""sample_rate": 44100"#));
    // And the song in it is the song: a build that knows nothing of the
    // table reads what it always read.
    assert_eq!(&Project::from_json(&text).unwrap(), session.project());

    // The whole folder is sent somewhere else, and nothing is left behind.
    let sent = folder.path().join("Sent").join("Deep").join("Night");
    std::fs::create_dir_all(sent.parent().unwrap()).unwrap();
    std::fs::rename(&home, &sent).unwrap();
    session.load(&sent.join("night.trill")).unwrap();
    let there = |name: &str, root: &Path| root.join("samples").join(name).display().to_string();
    assert_eq!(
        paths(&session),
        [there("fx/riser.wav", &sent), there("fx/riser.wav", &sent), there("kit/kick.wav", &sent)]
            .map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR))
    );
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    assert_eq!(heard(&mut session), whole);
    assert!(!session.is_modified() && !session.can_undo(), "it is how the song opened, not something done to it");
    // Saved from there, the paths in the file are the ones that hold now.
    session.save(&sent.join("night.trill")).unwrap();
    let saved = Project::from_json(&std::fs::read_to_string(sent.join("night.trill")).unwrap()).unwrap();
    assert_eq!(&saved, session.project());

    // A copy of the folder, in which one file is another file of the same
    // name: the copy's kick is the kick, and wins; its riser is not the
    // riser the song knew, so the song keeps the one it was saved with.
    let copy = folder.path().join("Copy");
    for part in ["samples/fx", "samples/kit"] {
        std::fs::create_dir_all(copy.join(part)).unwrap();
    }
    std::fs::copy(sent.join("night.trill"), copy.join("night.trill")).unwrap();
    std::fs::copy(sent.join("samples/kit/kick.wav"), copy.join("samples/kit/kick.wav")).unwrap();
    write_wav(&copy.join("samples/fx/riser.wav"), 44_100.0, &sine(660.0, 44_100.0, 1.0, 0.5));
    session.load(&copy.join("night.trill")).unwrap();
    let played = paths(&session);
    assert_eq!(Path::new(&played[0]), sent.join("samples/fx/riser.wav"), "not the file the song knew");
    assert_eq!(Path::new(&played[2]), copy.join("samples/kit/kick.wav"), "the file beside the song wins");
    assert_eq!(heard(&mut session), whole);

    // The song file alone, somewhere its samples are not beside it: the
    // paths stand as they are.
    let alone = folder.path().join("alone.trill");
    std::fs::copy(sent.join("night.trill"), &alone).unwrap();
    session.load(&alone).unwrap();
    assert_eq!(Path::new(&paths(&session)[0]), sent.join("samples/fx/riser.wav"));
    assert!(session.problems().is_empty());

    // A file written before there was a table opens as it always did.
    let old = folder.path().join("old.trill");
    std::fs::write(&old, session.project().to_json()).unwrap();
    let before = session.project().clone();
    session.load(&old).unwrap();
    assert_eq!(session.project(), &before);
    assert!(session.known_files().is_empty());
}

#[test]
fn what_was_known_of_a_file_that_is_gone_stays_in_the_song_through_a_save() {
    let folder = folder();
    let (riser, kick) = samples(&folder.path().join("packs"));
    let song_file = folder.path().join("song.trill");
    let mut session = session();
    song(&mut session, &riser, &kick);
    session.save(&song_file).unwrap();
    let known = session.known_files().get(&riser).cloned().unwrap();

    std::fs::remove_file(&riser).unwrap();
    session.load(&song_file).unwrap();
    // Changed and saved while the file is gone.
    session.edit(|project| project.set_tempo(100.0)).unwrap();
    session.save(&song_file).unwrap();
    session.load(&song_file).unwrap();
    assert_eq!(session.known_files().get(&riser), Some(&known), "the size and length of a file that is not there");
    assert_eq!(session.known_files().files().len(), 2);

    // A safety copy carries the table too, and its paths stand as they are
    // wherever the copy is kept.
    let copy = folder.path().join("copies").join("kept.trill");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::write(&copy, session.snapshot_json()).unwrap();
    session.new_project();
    assert!(session.known_files().is_empty());
    session.load_recovered(&copy, Some(&song_file)).unwrap();
    assert_eq!(paths(&session), [riser.clone(), riser.clone(), kick]);
    assert_eq!(session.known_files().get(&riser), Some(&known));
}
