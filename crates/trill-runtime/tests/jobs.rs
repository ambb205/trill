//! Renders, measurements and probes done in a process of their own.
//!
//! The process is `trill-job`, the program this crate builds for it, and
//! the plugins are the engine's scripted instruments, which the process
//! can make too: nothing needs to be installed. One of them can be told
//! to take its process down, as a plugin that faults does (it writes
//! where nothing may be written), at each of the moments a plugin was
//! seen to: as it is made, as it is tried before the song, in the middle
//! of the song, and as it is let go of.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::{ChainRef, Note, Project, TrackId, TrackKind};
use trill_engine::apart::{Room, Stage};
use trill_engine::probe::{ProbeError, ProbeNote, ProbeOptions};
use trill_engine::render::{RenderError, RenderOptions, Rendered, Stems, Watch};
use trill_engine::scripted::{AMP, BASS, DRUMS, Dies, Loading, ScriptedFactory};
use trill_engine::session::ProbeTarget;
use trill_runtime::{Jobs, Parts, Program, Runtime, RuntimeOptions};

const SR: u32 = 48_000;

/// The program that does the jobs: built with these tests.
fn program() -> Program {
    Program::Executable(PathBuf::from(env!("CARGO_BIN_EXE_trill-job")))
}

/// A runtime that touches nothing of the machine's, with the scripted
/// instruments beside the built-in devices.
fn started(loading: Loading) -> Runtime {
    let parts = Parts { devices: vec![Box::new(ScriptedFactory::loading(loading))], ..Parts::default() };
    Runtime::start_custom(RuntimeOptions::hermetic(), parts).expect("the runtime starts")
}

/// [`started`] whose jobs are done by `trill-job`.
fn apart(loading: Loading) -> Runtime {
    let runtime = started(loading);
    runtime.do_jobs_apart(Jobs { scripted: Some(loading), ..Jobs::by(program()) }).unwrap();
    runtime
}

fn part(project: &mut Project, name: &str, instrument: &str, pitches: &[u8]) -> TrackId {
    let track = project.add_track(name);
    project.add_device(ChainRef::Track(track), instrument, name, None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 16.0).unwrap();
    let notes: Vec<Note> = (0..32)
        .map(|step| Note { pitch: pitches[step % pitches.len()], start: step as f64 * 0.5, length: 0.4, velocity: 100 })
        .collect();
    project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    track
}

/// A song with a scripted bass through a scripted amplifier, scripted
/// drums, a built-in synth with a moving filter, a group, a return and a
/// limiter on the master. Returns the bass's and the drums' tracks.
fn song(runtime: &Runtime) -> (TrackId, TrackId) {
    let mut session = runtime.session.lock();
    session
        .edit(|project| {
            project.tempo = 132.0;
            let room = project.add_track_of_kind("Room", TrackKind::Return);
            project.add_device(ChainRef::Track(room), "builtin:reverb", "Reverb", None)?;
            let band = project.add_track_of_kind("Band", TrackKind::Group);
            project.add_device(ChainRef::Track(band), "builtin:compressor", "Glue", None)?;
            let bass = part(project, "Bass", BASS, &[40, 40, 43, 45]);
            project.add_device(ChainRef::Track(bass), AMP, "Amp", None)?;
            project.set_track_group(bass, Some(band))?;
            let drums = part(project, "Drums", DRUMS, &[36, 42, 38, 42]);
            project.set_track_group(drums, Some(band))?;
            project.set_send(drums, room, Some(-12.0))?;
            let lead = part(project, "Lead", "builtin:polysynth", &[60, 64, 67, 72]);
            project.add_device(ChainRef::Track(lead), "builtin:filter", "Filter", None)?;
            project.set_send(lead, room, Some(-9.0))?;
            project.track_mut(lead)?.pan = 0.3;
            Ok((bass, drums))
        })
        .unwrap()
}

fn plain() -> RenderOptions {
    RenderOptions { sample_rate: SR, max_tail_seconds: 2.0, ..RenderOptions::default() }
}

fn rendered(runtime: &Runtime, options: &RenderOptions) -> Result<Rendered, RenderError> {
    let job = runtime.session.lock().render_job();
    job.render(options)
}

/// The names of what is in `folder`, in order.
fn files(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(folder) {
        Ok(entries) => entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

fn jobs_done(runtime: &Runtime) -> (u64, u64) {
    runtime.session.lock().jobs_apart().map(|apart| apart.asked()).unwrap_or_default()
}

#[test]
fn a_song_rendered_in_a_process_of_its_own_is_the_samples_and_the_figures_it_is_here() {
    let folder = tempfile::tempdir().unwrap();
    let runtime = apart(Loading::default());
    song(&runtime);
    let with_stems = |name: &str| RenderOptions { stems: Some(Stems::all(folder.path().join(name))), ..plain() };

    runtime.do_jobs_here();
    let here = rendered(&runtime, &with_stems("here")).unwrap();
    assert!(here.left.iter().any(|sample| sample.abs() > 0.05), "the song sounds");
    let job = runtime.session.lock().render_job();
    let here_file = job.render_to_file(&folder.path().join("here.wav"), &plain()).unwrap();
    let here_measured = job.measure(&plain()).unwrap();

    runtime.do_jobs_apart(Jobs { scripted: Some(Loading::default()), ..Jobs::by(program()) }).unwrap();
    let there = rendered(&runtime, &with_stems("there")).unwrap();
    assert_eq!(jobs_done(&runtime), (1, 1), "it was handed to a process, which was started for it");
    // Not close: the same.
    assert!(there.left == here.left && there.right == here.right, "the mix is other samples");
    assert_eq!(there.track_peaks, here.track_peaks);
    assert_eq!(there.track_levels, here.track_levels);
    assert_eq!(there.problems, here.problems);
    // Everything that was measured, but for where the stems lie and how
    // long the plugins were waited for.
    let figures = |rendered: &Rendered| {
        let mut measured = rendered.measured.clone();
        measured.stems.iter_mut().for_each(|stem| stem.path.clear());
        measured.waited.clear();
        serde_json::to_string(&measured).unwrap()
    };
    assert_eq!(figures(&there), figures(&here), "everything that was measured");
    assert_eq!(there.measured.stems.len(), 3 + 3 + 1 + 1 + 1, "each part twice, the group, the return, the premaster");
    for (stem, other) in there.measured.stems.iter().zip(&here.measured.stems) {
        let (written, expected) = (std::fs::read(&stem.path).unwrap(), std::fs::read(&other.path).unwrap());
        assert!(written == expected, "{} is other samples than {}", stem.path, other.path);
    }

    // A file, and a measurement that keeps nothing.
    let job = runtime.session.lock().render_job();
    let there_file = job.render_to_file(&folder.path().join("there.wav"), &plain()).unwrap();
    let (written, expected) = (
        std::fs::read(folder.path().join("there.wav")).unwrap(),
        std::fs::read(folder.path().join("here.wav")).unwrap(),
    );
    assert!(written == expected, "the file is other bytes");
    assert_eq!(there_file.stats, here_file.stats);
    let there_measured = job.measure(&plain()).unwrap();
    assert_eq!(there_measured.stats, here_measured.stats);
    assert_eq!(there_measured.path, None);
    assert_eq!(jobs_done(&runtime), (3, 3), "each in a process that had just started");
    // Nothing is left behind but what was asked for.
    assert_eq!(files(folder.path()), ["here", "here.wav", "there", "there.wav"]);
    assert!(files(&folder.path().join("there")).iter().all(|name| name.ends_with(".wav")));
}

#[test]
fn a_song_without_plugins_is_rendered_here_whatever_a_process_could_do() {
    let runtime = apart(Loading::default());
    let (bass, drums) = song(&runtime);
    // With the plugins' tracks muted nothing of theirs is made: the render
    // is this process's own.
    runtime
        .session
        .lock()
        .edit(|project| {
            project.track_mut(bass)?.mute = true;
            project.track_mut(drums)?.mute = true;
            Ok(())
        })
        .unwrap();
    let alone = rendered(&runtime, &plain()).unwrap();
    assert!(alone.left.iter().any(|sample| sample.abs() > 0.01));
    assert_eq!(jobs_done(&runtime), (0, 0));
    // And a probe of a built-in instrument.
    let job = runtime.session.lock().probe_job(&ProbeTarget::Device { uri: "builtin:polysynth".into(), preset: None });
    let probed =
        job.unwrap().run(&[ProbeNote { pitch: 60, velocity: 100, with: Vec::new() }], &ProbeOptions::default());
    assert!(probed.unwrap().heard[0].sounded);
    assert_eq!(jobs_done(&runtime), (0, 0));
}

#[test]
fn a_plugin_that_takes_its_process_down_costs_a_sentence_and_leaves_the_song_and_the_folder_as_they_were() {
    let folder = tempfile::tempdir().unwrap();
    for (dies, stage, during) in [
        (Dies::Made, "while its devices were being made", None),
        (Dies::Rehearsed, "while its plugins were waited for", Some(Stage::Waiting)),
        (Dies::Played(40), "while it was being played", Some(Stage::Playing)),
    ] {
        let runtime = apart(Loading { dies: Some(dies), ..Loading::default() });
        let (bass, _) = song(&runtime);
        let (mix, stems) = (folder.path().join("mix.wav"), folder.path().join("mix stems"));
        let options = RenderOptions { stems: Some(Stems::all(&stems)), ..plain() };
        let job = runtime.session.lock().render_job();
        // (A job takes down what state the song's own instruments have.)
        let before = serde_json::to_string(runtime.session.lock().project()).unwrap();
        let Err(RenderError::Crashed(crash)) = job.render_to_file(&mix, &options) else {
            panic!("{dies:?}: the render went on with a plugin that takes its process down")
        };
        let said = crash.to_string();
        assert!(said.contains(stage), "{dies:?}: {said}");
        assert!(said.contains("the song that is open is untouched"), "{said}");
        match dies {
            // The device that was being made is named, with its track.
            Dies::Made => {
                assert!(matches!(crash.stage, Some(Stage::Making(_))), "{crash:?}");
                assert_eq!(crash.devices, ["Bass on 'Bass'"]);
                assert!(said.starts_with("a plugin took the render down while its devices were being made: Bass on"));
                assert!(said.ends_with("mute that track or take the device out to go on without it"), "{said}");
            }
            _ => assert_eq!(crash.stage, during, "{crash:?}"),
        }
        // Where the system says, it was the fault it was, in the program
        // the instrument is part of.
        #[cfg(windows)]
        {
            assert_eq!(crash.code, Some(0xC000_0005), "{crash:?}");
            assert!(said.contains("an access violation in trill-job.exe"), "{said}");
        }

        // Nothing was written, and nothing begun is left.
        assert_eq!(files(folder.path()), Vec::<String>::new(), "{dies:?}");
        // The session is alive and the song is what it was.
        let mut session = runtime.session.lock();
        assert_eq!(serde_json::to_string(session.project()).unwrap(), before);
        assert!(session.problems().is_empty(), "{:?}", session.problems());
        // And goes on: without the track, the rest is rendered.
        session.edit(|project| project.track_mut(bass).map(|track| track.mute = true)).unwrap();
        let job = session.render_job();
        drop(session);
        let report = job.render_to_file(&mix, &options).unwrap();
        assert!(report.stats.peak_db > -40.0 && mix.exists());
        std::fs::remove_file(&mix).unwrap();
        std::fs::remove_dir_all(&stems).unwrap();
        // A measurement is refused in the same words, and a probe of the
        // instrument, where it gets that far.
        runtime.session.lock().edit(|project| project.track_mut(bass).map(|track| track.mute = false)).unwrap();
        let job = runtime.session.lock().render_job();
        let Err(RenderError::Crashed(crash)) = job.measure(&plain()) else { panic!("{dies:?}: it was measured") };
        assert!(crash.to_string().contains("took the measurement down") || crash.devices.is_empty(), "{crash}");
    }
}

#[test]
fn a_plugin_that_would_take_its_process_down_when_let_go_of_is_never_let_go_of() {
    // A process that has done its job ends where it stands: the plugin's
    // end, where plugins fault most, never comes.
    let runtime = apart(Loading { dies: Some(Dies::LetGo), ..Loading::default() });
    song(&runtime);
    let healthy = started(Loading::default());
    song(&healthy);
    let (there, here) = (rendered(&runtime, &plain()).unwrap(), rendered(&healthy, &plain()).unwrap());
    assert!(there.left == here.left && there.right == here.right);
    assert_eq!(jobs_done(&runtime), (1, 1));
}

#[test]
fn a_render_that_is_stopped_ends_at_once_and_leaves_nothing_half_written() {
    for is_apart in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let runtime = apart(Loading::default());
        if !is_apart {
            runtime.do_jobs_here();
        }
        let (bass, _) = song(&runtime);
        // A long song: the same bars a few hundred times over.
        runtime
            .session
            .lock()
            .edit(|project| {
                let clip = project.track(bass)?.clips[0].id;
                project.clip_mut(clip)?.set_loop_length(Some(16.0))?;
                project.set_clip_span(clip, 0.0, 6400.0)
            })
            .unwrap();
        let watch = Watch::new();
        let (mix, stems) = (folder.path().join("long.wav"), folder.path().join("long stems"));
        let options = RenderOptions { stems: Some(Stems::all(&stems)), watch: Some(watch.clone()), ..plain() };
        let job = runtime.session.lock().render_job();
        let stopper = {
            let watch = watch.clone();
            std::thread::spawn(move || {
                let begun = Instant::now();
                // Once it is well under way.
                while watch.done() < 0.02 && begun.elapsed() < Duration::from_secs(120) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let seen = watch.done();
                watch.stop();
                (seen, Instant::now())
            })
        };
        let outcome = job.render_to_file(&mix, &options);
        let ended = Instant::now();
        let (seen, stopped_at) = stopper.join().unwrap();
        assert!(
            matches!(outcome, Err(RenderError::Stopped)),
            "apart {is_apart}: {:?}",
            outcome.map(|report| report.path)
        );
        assert!((0.02..0.9).contains(&seen), "apart {is_apart}: it had come to {seen} when it was stopped");
        assert!(ended.duration_since(stopped_at) < Duration::from_secs(5), "apart {is_apart}: it went on");
        assert_eq!(files(folder.path()), Vec::<String>::new(), "apart {is_apart}");
    }
}

#[test]
fn a_job_the_machine_has_no_room_for_is_refused_in_a_sentence_and_not_begun() {
    let runtime = started(Loading::default());
    song(&runtime);
    let full: fn() -> Option<Room> = || Some(Room { free: 300 << 20, can_take: 2 << 30, total: 16 << 30 });
    let jobs = Jobs { scripted: Some(Loading::default()), room: Some(full), ..Jobs::by(program()) };
    runtime.do_jobs_apart(jobs).unwrap();
    let Err(RenderError::NoRoom(said)) = rendered(&runtime, &plain()) else { panic!("it was begun") };
    assert!(said.starts_with("the render was not started: it runs in a process of its own, which would take about"));
    assert!(
        said.contains("the machine has 0.3 GB of memory free of 16 GB") && said.ends_with("and try again"),
        "{said}"
    );
    let job = runtime.session.lock().probe_job(&ProbeTarget::Device { uri: BASS.into(), preset: None }).unwrap();
    let refused = job.run(&[ProbeNote { pitch: 40, velocity: 100, with: Vec::new() }], &ProbeOptions::default());
    assert!(
        matches!(&refused, Err(ProbeError::Apart(said)) if said.starts_with("the probe was not started")),
        "{refused:?}"
    );
    assert_eq!(jobs_done(&runtime), (0, 0), "no process was started for either");

    // A machine that cannot say how much it has is not refused anything.
    let silent: fn() -> Option<Room> = || None;
    let jobs = Jobs { scripted: Some(Loading::default()), room: Some(silent), ..Jobs::by(program()) };
    runtime.do_jobs_apart(jobs).unwrap();
    assert!(rendered(&runtime, &plain()).is_ok());
}

#[test]
fn probes_in_a_process_of_their_own_hear_what_they_hear_here_and_one_that_is_kept_is_replaced_after_a_fault() {
    let notes: Vec<ProbeNote> =
        [40u8, 45, 52].iter().map(|pitch| ProbeNote { pitch: *pitch, velocity: 100, with: Vec::new() }).collect();
    let probe = |runtime: &Runtime, target: &ProbeTarget| {
        let job = runtime.session.lock().probe_job(target).unwrap();
        job.run(&notes, &ProbeOptions::default())
    };
    let bass = ProbeTarget::Device { uri: BASS.into(), preset: Some("up".into()) };
    let drums = ProbeTarget::Device { uri: DRUMS.into(), preset: None };

    // Here, in a process that has just started, and in one that is kept.
    let runtime = apart(Loading::default());
    runtime.do_jobs_here();
    let here = probe(&runtime, &bass).unwrap();
    assert!(here.heard.iter().all(|note| note.sounded) && here.heard[0].pitch.is_some());
    runtime.do_jobs_apart(Jobs { scripted: Some(Loading::default()), ..Jobs::by(program()) }).unwrap();
    assert_eq!(probe(&runtime, &bass).unwrap().heard, here.heard);
    assert_eq!(probe(&runtime, &bass).unwrap().heard, here.heard);
    assert_eq!(jobs_done(&runtime), (2, 2), "a process each");
    let kept = Jobs { scripted: Some(Loading::default()), keep_for_probes: true, ..Jobs::by(program()) };
    runtime.do_jobs_apart(kept).unwrap();
    for _ in 0..3 {
        assert_eq!(probe(&runtime, &bass).unwrap().heard, here.heard);
    }
    assert_eq!(jobs_done(&runtime), (3, 1), "one process for the three");
    // The track's own instrument, as it is in the song.
    let (track, _) = song(&runtime);
    let on_track = probe(&runtime, &ProbeTarget::Track { track, effects: true }).unwrap();
    assert!(on_track.heard.iter().all(|note| note.sounded));
    // Whether a device is ready is asked the same way.
    let device = runtime.session.lock().project().track(track).unwrap().devices[0].id;
    let (job, pitches) = runtime.session.lock().readiness_job(device).unwrap();
    assert!(job.wait_until_ready(&pitches, Duration::from_secs(5)).unwrap().ready);
    assert_eq!(jobs_done(&runtime), (5, 1));

    // A bass that takes its process down as it is tried: the probe says
    // so, and the next one, of the drums, has a process of its own.
    let dying = Loading { dies: Some(Dies::Rehearsed), ..Loading::default() };
    let runtime = started(dying);
    runtime.do_jobs_apart(Jobs { scripted: Some(dying), keep_for_probes: true, ..Jobs::by(program()) }).unwrap();
    assert!(probe(&runtime, &drums).unwrap().heard[0].sounded);
    let Err(ProbeError::Apart(said)) = probe(&runtime, &bass) else { panic!("the bass was probed") };
    assert!(
        said.starts_with("a plugin took the probe down while its plugins were waited for: Scripted Bass ("),
        "{said}"
    );
    assert!(said.contains("the song that is open is untouched. Try again"), "{said}");
    assert!(probe(&runtime, &drums).unwrap().heard[0].sounded);
    assert_eq!(jobs_done(&runtime), (3, 2), "the process that went was replaced");

    // One that would go as it is let go of has answered by then: the probe
    // is whole, and the process that went with it is replaced.
    let later = Loading { dies: Some(Dies::LetGo), ..Loading::default() };
    let runtime = started(later);
    runtime.do_jobs_apart(Jobs { scripted: Some(later), keep_for_probes: true, ..Jobs::by(program()) }).unwrap();
    // (As it comes: a probe that is handed a preset makes its instrument
    // a second time, and lets go of the first on the way.)
    let plain_bass = ProbeTarget::Device { uri: BASS.into(), preset: None };
    assert!(probe(&runtime, &plain_bass).unwrap().heard.iter().all(|note| note.sounded));
    assert!(probe(&runtime, &drums).unwrap().heard[0].sounded);
    assert!(probe(&runtime, &plain_bass).unwrap().heard.iter().all(|note| note.sounded));
    assert_eq!(jobs_done(&runtime), (3, 2), "the process that went with the bass it let go of was replaced");
}

#[test]
fn an_instrument_that_draws_at_random_is_the_same_samples_in_every_process_that_has_just_started() {
    // A bass that draws how loud each note is from a stream that every
    // bass of its program shares, that nothing puts back, and that begins
    // at the same place in a program that has just started: as a sampled
    // instrument draws which recording it plays.
    let random = Loading { random: true, ..Loading::default() };
    let runtime = apart(random);
    song(&runtime);
    let (first, second) = (rendered(&runtime, &plain()).unwrap(), rendered(&runtime, &plain()).unwrap());
    assert!(first.left == second.left && first.right == second.right, "two processes drew differently");
    assert_eq!(jobs_done(&runtime), (2, 2));
    // What it is known to do comes back with the render, across the
    // processes: it was rendered as it is.
    let [draws] = &first.measured.draws[..] else { panic!("{:?}", first.measured.draws) };
    assert_eq!((draws.track_name.as_str(), draws.device_name.as_str(), draws.held), ("Bass", "Bass", false));
    assert!(draws.again, "and it says that the song is these samples again, rendered so");

    // One process that renders the song twice has drawn on by the second
    // time: other samples, as in a program that renders for itself.
    runtime.do_jobs_apart(Jobs { scripted: Some(random), keep_for_renders: true, ..Jobs::by(program()) }).unwrap();
    let (third, fourth) = (rendered(&runtime, &plain()).unwrap(), rendered(&runtime, &plain()).unwrap());
    assert_eq!(jobs_done(&runtime), (2, 1), "one process for the two");
    assert!(third.left == first.left, "its first is what any process begins with");
    assert!(fourth.left != third.left, "it drew the same twice");
    assert!(!third.measured.draws[0].again && !fourth.measured.draws[0].again, "which it does not promise");

    // A render that is to be repeatable takes the chance out of its own
    // copy of the bass, in whatever process: the same samples from the one
    // that has drawn before, and what was done is said.
    let repeatable = RenderOptions { repeatable: true, ..plain() };
    let (fifth, sixth) = (rendered(&runtime, &repeatable).unwrap(), rendered(&runtime, &repeatable).unwrap());
    assert!(fifth.left == sixth.left && fifth.right == sixth.right);
    assert!(fifth.left != first.left, "nothing was taken out of it");
    assert!(fifth.measured.draws[0].held && fifth.measured.draws[0].costs == "every note as loud as the last");
    assert_eq!(jobs_done(&runtime), (4, 1));
}

#[test]
fn what_one_process_learned_of_a_plugin_the_next_one_knows() {
    // A note the bass does not play is waited out for half the patience
    // the first time it is met, and is known as a key that plays nothing
    // from then on: also by a process that has only just started.
    let runtime = apart(Loading::default());
    runtime
        .session
        .lock()
        .edit(|project| {
            part(project, "Bass", BASS, &[41, 20, 44, 46]);
            Ok(())
        })
        .unwrap();
    let patient = RenderOptions { patience: Duration::from_secs(3), ..plain() };
    let begun = Instant::now();
    let first = rendered(&runtime, &patient).unwrap();
    let took = begun.elapsed();
    assert!(took >= Duration::from_millis(1500), "it was held to the silent key: {took:?}");
    assert_eq!(first.measured.waited[0].silent, [20], "{:?}", first.measured.waited);
    let begun = Instant::now();
    let again = rendered(&runtime, &patient).unwrap();
    assert!(begun.elapsed() < took, "{:?} after {took:?}", begun.elapsed());
    assert!(again.measured.waited.iter().all(|waited| waited.silent.is_empty()), "{:?}", again.measured.waited);
    assert!(again.left == first.left);
    assert_eq!(jobs_done(&runtime), (2, 2));
}

#[test]
fn a_program_that_cannot_be_started_is_said_and_the_song_is_as_it_was() {
    let runtime = started(Loading::default());
    song(&runtime);
    let missing = Program::Executable(PathBuf::from("no such folder").join("trill-job"));
    runtime.do_jobs_apart(Jobs { scripted: Some(Loading::default()), ..Jobs::by(missing) }).unwrap();
    let Err(RenderError::Apart(said)) = rendered(&runtime, &plain()) else { panic!("something rendered it") };
    assert!(said.starts_with("the process that renders (") && said.contains("could not be started"), "{said}");
    // In this process it still renders.
    runtime.do_jobs_here();
    assert!(rendered(&runtime, &plain()).is_ok());
    let _ = Arc::new(runtime);
}
