//! A track heard where its plugin is not, between programs that share a
//! song: each a runtime of its own, connected over this machine's
//! loopback. One has an instrument the song plays (a scripted one, which
//! is a plugin as far as anyone is concerned); another has not.
//!
//! Nobody does anything here but work in the song: whoever has the
//! instrument records the track for whoever has not, by itself.

use std::path::Path;
use std::time::{Duration, Instant};
use trill_core::device::DeviceFactory;
use trill_core::files::FilePlace;
use trill_core::merge::lot::note;
use trill_core::{ChainRef, ClipId, DeviceId, Rendition, TrackId};
use trill_engine::scripted::{BASS, ScriptedFactory};
use trill_engine::{AudioBackend, RenderOptions};
use trill_runtime::{Reach, Runtime, RuntimeOptions, Storage};

const WAIT: Duration = Duration::from_secs(40);

/// A runtime that keeps what it keeps under `folder`: no audio device,
/// nothing of the user's, and the instrument only where it is `with` it.
fn start(folder: &Path, name: &str, with: bool) -> Runtime {
    let storage = Storage::At { settings: folder.join(name).join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let extra: Vec<Box<dyn DeviceFactory>> = match with {
        true => vec![Box::new(ScriptedFactory::new())],
        false => Vec::new(),
    };
    let runtime = Runtime::start_with(options, extra).expect("the runtime starts");
    runtime.set_name_together(name);
    runtime
}

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn said(runtime: &Runtime) -> String {
    runtime.said_together(0).into_iter().map(|(_, sentence)| sentence).collect::<Vec<_>>().join(" | ")
}

/// The song a program holds as every program that holds it has it.
fn text(runtime: &Runtime) -> String {
    let mut song = runtime.session.lock().said_song();
    song.count_ids_from(0);
    song.to_json()
}

/// A program that has the instrument, with a song in it: a bass line
/// through the instrument and an equaliser, and keys beside it.
fn with_a_song(folder: &Path, name: &str) -> (Runtime, TrackId, ClipId, DeviceId) {
    let runtime = start(folder, name, true);
    let (bass, line, plugin) = {
        let mut session = runtime.session.lock();
        let bass = session.edit(|song| Ok(song.add_track("Bass"))).unwrap();
        let keys = session.edit(|song| Ok(song.add_track("Keys"))).unwrap();
        let plugin = session.add_device(ChainRef::Track(bass), BASS, None, None).unwrap();
        session.add_device(ChainRef::Track(bass), "builtin:eq", None, None).unwrap();
        session.add_device(ChainRef::Track(keys), "builtin:polysynth", None, None).unwrap();
        let line = session.edit(|song| song.add_midi_clip(bass, "Line", 0.0, 8.0)).unwrap();
        session.edit(|song| song.clip_mut(line)?.add_notes(&[note(40, 0.0), note(43, 2.0), note(47, 4.0)])).unwrap();
        (bass, line, plugin)
    };
    (runtime, bass, line, plugin)
}

/// The bass alone, as a program plays it: its loudest sample.
fn bass_alone(runtime: &Runtime, bass: TrackId) -> f32 {
    let job = runtime.session.lock().render_job();
    let options = RenderOptions { only_tracks: Some(vec![bass]), max_tail_seconds: 0.5, ..RenderOptions::default() };
    let rendered = job.render(&options).expect("the song renders");
    rendered.left.iter().chain(&rendered.right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()))
}

fn rendition(runtime: &Runtime, bass: TrackId) -> Option<Rendition> {
    runtime.session.lock().project().track(bass).ok()?.rendition.clone()
}

/// Whether a program hears the bass as the song has it now, by a
/// recording.
fn hears_it(runtime: &Runtime) -> bool {
    runtime.session.lock().renditions().first().is_some_and(|state| state.played && state.as_the_song_is())
}

/// The recordings of renditions a program keeps for the open song.
fn kept(runtime: &Runtime) -> Vec<String> {
    let Some(folder) = runtime.session.lock().received_in() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(folder) else { return Vec::new() };
    let mut names: Vec<String> =
        entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// 1 connection.
#[test]
fn whoever_has_not_got_a_plugin_hears_its_track_as_whoever_has_it_recorded_it_and_again_after_a_change() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, bass, line, plugin) = with_a_song(folder.path(), "Ada");
    let ben = start(folder.path(), "Ben", false);
    let hers = bass_alone(&ada, bass);
    assert!(hers > 0.05, "Ada hears her bass: {hers}");

    // Alone, nothing is recorded for anyone.
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(rendition(&ada, bass), None);

    // Ben joins. His machine has not got the bass, and says so to Ada's.
    ben.join(&invitation, false).unwrap();
    eventually("Ben has the song", || text(&ada) == text(&ben) && ben.together().people.len() == 1);
    eventually("Ada's program knows what his lacks", || {
        ada.together().people.first().is_some_and(|ben| ben.lacks == [plugin])
    });
    assert_eq!(ben.session.lock().lacking(), vec![plugin]);
    assert!(ben.together().people[0].lacks.is_empty(), "and Ada's lacks nothing");

    // Ada's program records the bass for him, by itself, once the song
    // has stood still for a moment; it arrives, and he hears it as she
    // does, through his own equaliser and at his own fader.
    eventually("Ada's program has recorded the bass", || rendition(&ada, bass).is_some());
    let first = rendition(&ada, bass).unwrap();
    assert_eq!((first.after, first.by.as_str()), (plugin, "Ada"));
    assert_eq!(first.is.name, "Bass rendition.wav");
    eventually("Ben hears it", || hears_it(&ben));
    assert_eq!(rendition(&ben, bass), Some(first.clone()));
    let his = bass_alone(&ben, bass);
    assert!((his - hers).abs() < 0.002, "Ben hears {his} where Ada hears {hers}");
    assert!(ben.session.lock().files_arriving().is_empty() && ben.together().files.is_empty());
    assert_eq!(ada.together().recording_for_others, None, "and Ada's program is done with it");
    // What arrived is the recording Ada's program made, by what it is.
    let at_bens = ben.session.lock().project().track(bass).unwrap().rendition_here().unwrap().to_owned();
    assert!(at_bens.ends_with(&first.is.kept_as()), "{at_bens}");
    assert_eq!(std::fs::metadata(&at_bens).unwrap().len(), first.is.bytes);
    assert_eq!(kept(&ben), [first.is.kept_as()]);
    assert_eq!(kept(&ada), [first.is.kept_as()]);

    // Ben moves his fader: heard at once, and nothing is recorded again.
    ben.session.lock().edit(|song| song.set_track_volume(bass, -6.0)).unwrap();
    let quieter = bass_alone(&ben, bass);
    assert!((quieter - his * 0.501).abs() < 0.01, "six decibels under {his}: {quieter}");
    eventually("Ada has his fader", || text(&ada) == text(&ben));
    std::thread::sleep(Duration::from_millis(3200));
    assert_eq!(rendition(&ada, bass), Some(first.clone()), "what comes after the plugin is not recorded again");
    assert!(hears_it(&ben));

    // Ben writes a note into the bass line. He goes on hearing the bass
    // as it was, which he is shown; Ada's program records it again, and
    // then he hears the note. The recording before it is kept by nobody.
    ben.session.lock().edit(|song| song.clip_mut(line)?.add_notes(&[note(52, 6.0)])).unwrap();
    let state = ben.session.lock().renditions()[0].clone();
    assert!(state.played && !state.current, "{state:?}");
    eventually("Ada's program has recorded it again", || rendition(&ada, bass).is_some_and(|now| now.of != first.of));
    eventually("Ben hears the bass as it is now", || hears_it(&ben));
    let second = rendition(&ben, bass).unwrap();
    assert_ne!(second.is, first.is);
    eventually("neither keeps the one before", || {
        kept(&ben) == [second.is.kept_as()] && kept(&ada) == [second.is.kept_as()]
    });
    assert!(bass_alone(&ben, bass) > 0.05);
    assert_eq!(text(&ada), text(&ben));
    for runtime in [&ada, &ben] {
        let said = said(runtime);
        assert!(!said.contains("lost") && !said.contains("no longer") && !said.contains("could not"), "{said}");
    }

    // Ben saves the song and closes his Trill. Opened alone another day,
    // on the machine that still has not got the bass, it plays.
    let file = folder.path().join("Ben's songs").join("Ours.trill");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    ben.session.lock().save(&file).unwrap();
    ben.leave();
    eventually("Ben has left", || ada.together().people.is_empty());
    drop(ben);
    let alone = start(folder.path(), "Ben", false);
    alone.session.lock().load(&file).unwrap();
    assert_eq!(alone.session.lock().lacking(), vec![plugin]);
    assert!(alone.session.lock().renditions()[0].played);
    assert!(bass_alone(&alone, bass) > 0.05, "the bass is in the song he saved");

    // With nobody there who lacks anything, Ada's program records
    // nothing more, whatever she changes.
    ada.session.lock().edit(|song| song.clip_mut(line)?.add_notes(&[note(45, 7.0)])).unwrap();
    std::thread::sleep(Duration::from_millis(3200));
    assert_eq!(rendition(&ada, bass).map(|now| now.is), Some(second.is));
    assert!(!ada.session.lock().renditions()[0].current, "what there is is of the song as it was, and says so");
}

/// 2 connections.
#[test]
fn of_two_programs_that_have_the_plugin_one_records_it_and_what_no_one_can_record_is_left_as_it_was() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, bass, _, plugin) = with_a_song(folder.path(), "Ada");
    let (cy, ben) = (start(folder.path(), "Cy", true), start(folder.path(), "Ben", false));
    let invitation = ada.share(Reach::ThisMachine, 0).unwrap();
    cy.join(&invitation, false).unwrap();
    eventually("Cy has the song", || text(&ada) == text(&cy) && cy.together().people.len() == 1);
    ben.join(&invitation, false).unwrap();
    eventually("all three are there", || {
        [&ada, &cy, &ben].iter().all(|runtime| runtime.together().people.len() == 2) && text(&ada) == text(&ben)
    });
    assert!(cy.session.lock().lacking().is_empty());

    // Ada and Cy both could: whoever has the lower number does, and the
    // other leaves it. Ben hears it.
    let first = [&ada, &cy].into_iter().min_by_key(|runtime| runtime.together().me.id).unwrap();
    let name = first.together().me.name.clone();
    eventually("the bass is recorded", || rendition(&ada, bass).is_some() && rendition(&cy, bass).is_some());
    let made = rendition(&ada, bass).unwrap();
    assert_eq!(made.by, name, "by the one with the lower number");
    eventually("Ben hears it", || hears_it(&ben));
    // The one who has the plugin and did not make it keeps the recording
    // too, to hand it on: Ben may be connected to that one alone (he is,
    // where Cy made it: both joined Ada).
    let other = if name == "Ada" { &cy } else { &ada };
    let theirs = other.session.lock().files_here().into_iter().find(|file| file.place == FilePlace::Rendition(bass));
    assert!(theirs.is_some_and(|file| file.wanted && file.may_go));
    eventually("whoever did not make it has it to hand on", || kept(other) == [made.is.kept_as()]);
    std::thread::sleep(Duration::from_millis(3200));
    assert_eq!(rendition(&ada, bass), Some(made.clone()), "and it is made once");
    assert_eq!(rendition(&cy, bass), Some(made));
    // Neither of the two plays it: they have the plugin.
    for runtime in [&ada, &cy] {
        let state = runtime.session.lock().renditions()[0].clone();
        assert!(!state.played && state.lacking.is_empty(), "{state:?}");
    }

    // A second track through an instrument nobody there has: nobody can
    // record it, nothing is made of it, and it is as it always was.
    let (strings, on_it) = {
        let mut session = ada.session.lock();
        let strings = session.edit(|song| Ok(song.add_track("Strings"))).unwrap();
        let on_it =
            session.edit(|song| song.add_device(ChainRef::Track(strings), "clap:nobody.has.this", "Strings", None));
        (strings, on_it.unwrap())
    };
    eventually("everyone has the track", || text(&ada) == text(&ben) && text(&ada) == text(&cy));
    eventually("and says that they lack its instrument", || {
        ada.together().people.iter().all(|here| here.lacks.contains(&on_it))
    });
    std::thread::sleep(Duration::from_millis(3200));
    assert_eq!(rendition(&ada, strings), None);
    assert!(said(&ada).is_empty() || !said(&ada).contains("could not be recorded"), "{}", said(&ada));
    let _ = plugin;
}
