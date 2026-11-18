//! Studies as a program has them: asked for at a desk, each given its
//! turn in a process of its own, with scripted models in place of the real
//! ones and a stand-in for the program that fetches a link. Nothing is
//! fetched and no graphics card is asked.

mod common;

use common::{program, recording, script};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use trill_fetch::{Fetches, List, Nothing};
use trill_listen::Correction;
use trill_study::models::real::Provider;
use trill_study::models::scripted::Script;
use trill_study::run::Programs;
use trill_study::setup::{self, Setup};
use trill_study::store::{Source, Stage, StageState, State, StudyOptions};
use trill_study::{Desk, DeskOptions};

const LONG: Duration = Duration::from_secs(120);

fn desk_with(root: &Path, script: Option<Script>, programs: Programs, free: Option<fn() -> Option<u64>>) -> Desk {
    // With nothing published: no part is here, and none can be fetched.
    let fetches = Fetches::new(Arc::new(Nothing));
    fetches.add(List { parts: Vec::new(), ..setup::list(root.join("models")) }).unwrap();
    let setup = Setup::over(Arc::new(fetches), script, Some(programs));
    Desk::new(DeskOptions {
        studies: root.join("studies"),
        setup: Arc::new(setup),
        program: Some(program()),
        free,
        turn: Some(root.join("turn")),
    })
}

fn desk(root: &Path, script: Script) -> Desk {
    desk_with(root, Some(script), Programs::default(), None)
}

fn song(root: &Path) -> Source {
    let path = root.join("small song.wav");
    if !path.is_file() {
        recording(&path);
    }
    Source::File { path }
}

fn state_of(report: &trill_study::Report, stage: Stage) -> Option<(StageState, String)> {
    report.study.stages.iter().find(|report| report.stage == stage).map(|report| (report.state, report.note.clone()))
}

#[test]
fn studies_are_done_one_at_a_time_in_the_order_they_were_asked_for() {
    let root = tempfile::tempdir().unwrap();
    let desk = desk(root.path(), Script { seconds: 0.3, ..script() });
    assert!(desk.list().is_empty() && !desk.busy());
    let first = desk.start(song(root.path()), StudyOptions::default(), None).unwrap();
    let second = desk.start(song(root.path()), StudyOptions::default(), Some("  The same again ")).unwrap();
    // A study is called after its file unless it is given a name.
    assert_eq!((first.title.as_str(), second.title.as_str()), ("small song", "The same again"));
    assert_eq!(first.state, State::Waiting);
    assert!(desk.busy());

    // While the first runs, it says the stage it is at; the second waits.
    let mut seen = None;
    for _ in 0..400 {
        let report = desk.report(&first.id).unwrap();
        if report.at.is_some() {
            seen = Some((report, desk.report(&second.id).unwrap()));
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let (running, waiting) = seen.expect("the first study was seen running");
    assert_eq!(running.study.state, State::Running);
    assert_eq!((waiting.study.state, waiting.at), (State::Waiting, None));

    let done = desk.wait(&first.id, LONG).unwrap();
    assert_eq!((done.study.state, done.at), (State::Done, None));
    assert!(done.size > 1_000_000, "{}", done.size);
    assert_eq!(desk.wait(&second.id, LONG).unwrap().study.state, State::Done);
    assert!(!desk.busy());
    // Latest first.
    let listed: Vec<String> = desk.list().into_iter().map(|report| report.study.id).collect();
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&first.id) && listed.contains(&second.id));
    // Nothing is left that says a process attends it.
    assert!(!root.path().join("studies").join(&first.id).join("run.pid").exists());
    assert_eq!(
        desk.resume(&first.id).unwrap_err(),
        format!("study {} is done: there is nothing of it left to do", first.id)
    );
}

#[test]
fn a_study_is_stopped_taken_up_again_and_removed_while_it_runs() {
    let root = tempfile::tempdir().unwrap();
    let desk = desk(root.path(), Script { seconds: 1.2, ..script() });
    let study = desk.start(song(root.path()), StudyOptions::default(), None).unwrap();
    // Stopped once the stems are done and something after them is going on.
    for _ in 0..1500 {
        if desk.report(&study.id).unwrap().at.is_some_and(|(stage, _)| stage == Stage::Beats) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    desk.stop(&study.id).unwrap();
    let stopped = desk.wait(&study.id, LONG).unwrap();
    assert_eq!((stopped.study.state, &stopped.study.error), (State::Stopped, &None));
    assert_eq!(stopped.study.stems.len(), 4);
    assert_eq!(state_of(&stopped, Stage::Beats).unwrap().0, StageState::Pending);
    assert_eq!(desk.stop(&study.id).unwrap_err(), format!("study {} is not running", study.id));

    assert_eq!(desk.resume(&study.id).unwrap().state, State::Waiting);
    assert_eq!(desk.resume(&study.id).unwrap_err(), format!("study {} is running already", study.id));
    let done = desk.wait(&study.id, LONG).unwrap();
    assert_eq!(done.study.state, State::Done);
    assert_eq!(state_of(&done, Stage::Stems).unwrap(), (StageState::Done, "already done".to_owned()));

    // One that is running is stopped before it is removed, and its folder goes.
    let another = desk.start(song(root.path()), StudyOptions::default(), None).unwrap();
    for _ in 0..1500 {
        if desk.report(&another.id).unwrap().at.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(desk.delete(&another.id).unwrap().id, another.id);
    assert!(!root.path().join("studies").join(&another.id).exists());
    assert!(!desk.busy());
    let gone = desk.report(&another.id).unwrap_err();
    assert!(
        gone.starts_with(&format!("there is no study '{}'. Studies: {} (small song)", another.id, study.id)),
        "{gone}"
    );
}

#[test]
fn a_study_the_machine_has_no_room_for_or_that_lacks_its_parts_is_refused_before_anything_is_made() {
    let root = tempfile::tempdir().unwrap();
    let cramped = desk_with(root.path(), Some(script()), Programs::default(), Some(|| Some(1 << 30)));
    let refused = cramped.start(song(root.path()), StudyOptions::default(), None).unwrap_err();
    assert!(
        refused.starts_with("there is not the memory free to study this now: a recording of its length takes about 1.")
            && refused.contains("and 1.0 GB are free. Nothing was begun"),
        "{refused}"
    );
    assert!(cramped.list().is_empty() && !root.path().join("studies").exists());

    // With the real models and nothing fetched, the parts are named.
    let bare = desk_with(root.path(), None, Programs::default(), None);
    let lacking = bare.start(song(root.path()), StudyOptions::default(), None).unwrap_err();
    assert!(lacking.starts_with("a study on the processor needs parts that have not been fetched: "), "{lacking}");
    assert!(bare.list().is_empty());

    // And a recording that is not there is not studied.
    let desk = desk(root.path(), script());
    let gone =
        desk.start(Source::File { path: root.path().join("gone.wav") }, StudyOptions::default(), None).unwrap_err();
    assert!(gone.starts_with("there is no file at ") && gone.ends_with("gone.wav"), "{gone}");
}

#[test]
fn a_grid_that_is_corrected_is_heard_again_without_a_model() {
    let root = tempfile::tempdir().unwrap();
    let desk = desk(root.path(), script());
    let study = desk.start(song(root.path()), StudyOptions::default(), None).unwrap();
    assert_eq!(desk.wait(&study.id, LONG).unwrap().study.state, State::Done);
    let folder = desk.studies().folder(&study.id);
    let before = folder.heard().unwrap();
    assert!((before.grid.bpm - 100.0).abs() < 0.7, "{}", before.grid.bpm);

    // The models are told to fail wherever they are asked: none is.
    drop(desk);
    let desk =
        self::desk(root.path(), Script { fail_at: Some(Stage::Stems), crash_at: Some(Stage::Beats), ..script() });
    let corrected = desk.correct(&study.id, vec![Correction::Half]).unwrap();
    assert_eq!(corrected.state, State::Waiting);
    let done = desk.wait(&study.id, LONG).unwrap();
    assert_eq!((done.study.state, &done.study.error), (State::Done, &None));
    assert_eq!(done.study.corrections, [Correction::Half]);
    let after = folder.heard().unwrap();
    assert!((after.grid.bpm - before.grid.bpm / 2.0).abs() < 0.4, "{}", after.grid.bpm);
    // What ran the models is still what did, and the words are as they were.
    assert_eq!(done.study.device.as_deref(), Some("a script"));
    assert_eq!(folder.lyrics().unwrap().lines.len(), 4);
    // The same corrections again are nothing to do.
    assert_eq!(desk.correct(&study.id, vec![Correction::Half]).unwrap().state, State::Done);
    assert!(!desk.busy());
}

#[test]
fn a_study_that_a_program_left_running_when_it_ended_is_shown_stopped_and_taken_up() {
    let root = tempfile::tempdir().unwrap();
    let study = {
        let desk = desk(root.path(), Script { seconds: 3.0, ..script() });
        let study = desk.start(song(root.path()), StudyOptions::default(), None).unwrap();
        for _ in 0..1500 {
            if desk.report(&study.id).unwrap().at.is_some_and(|(stage, _)| stage == Stage::Stems) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        study
        // The desk goes, as it does when its program ends, and takes the run with it.
    };
    let desk = desk(root.path(), script());
    let left = desk.report(&study.id).unwrap();
    assert_eq!(left.study.state, State::Stopped, "{:?}", left.study);
    assert!(left.study.stages.iter().all(|report| report.state != StageState::Running));
    // Even one whose folder still says it runs: nothing attends it.
    let folder = desk.studies().folder(&study.id);
    let mut written = folder.study().unwrap();
    written.state = State::Running;
    folder.write(&written).unwrap();
    assert_eq!(desk.list()[0].study.state, State::Stopped);
    desk.resume(&study.id).unwrap();
    assert_eq!(desk.wait(&study.id, LONG).unwrap().study.state, State::Done);
}

fn link_to(path: &Path, seconds: u32) -> String {
    let name: String = path.to_string_lossy().bytes().map(|byte| format!("{byte:02x}")).collect();
    format!("https://recordings.test/{seconds}/{name}")
}

#[test]
fn a_link_is_fetched_by_the_program_installed_for_it_and_studied() {
    let root = tempfile::tempdir().unwrap();
    let Source::File { path } = song(root.path()) else { unreachable!() };
    // No program that fetches: said before anything is made.
    let without = desk_with(root.path(), Some(script()), Programs::default(), None);
    let link = Source::Link { url: link_to(&path, 20), by: None };
    let refused = without.start(link.clone(), StudyOptions::default(), None).unwrap_err();
    assert!(
        refused.starts_with("a link is fetched by yt-dlp, which needs ffmpeg, and both are not installed")
            && refused.ends_with("or study a file you have"),
        "{refused}"
    );
    assert!(without.list().is_empty());
    let not_a_link = without.start(Source::Link { url: "--exec calc".into(), by: None }, StudyOptions::default(), None);
    assert_eq!(not_a_link.unwrap_err(), "'--exec calc' is not a link: one begins with https://");

    // With a stand-in for it, the recording is fetched into the study's folder and studied there.
    let programs = Programs { ffmpeg: Some(PathBuf::from("ffmpeg-is-not-asked")), fetcher: Some(program()) };
    let desk = desk_with(root.path(), Some(script()), programs, None);
    let study = desk.start(link, StudyOptions::default(), None).unwrap();
    let done = desk.wait(&study.id, LONG).unwrap();
    assert_eq!((done.study.state, &done.study.error), (State::Done, &None));
    assert_eq!(state_of(&done, Stage::Fetch).unwrap().0, StageState::Done);
    // It is called what its page calls it, and says whose the page says it is.
    assert_eq!(done.study.title, "A stand-in song");
    assert_eq!(done.study.source, Source::Link { url: link_to(&path, 20), by: Some("Nobody".to_owned()) });
    let folder = desk.studies().folder(&study.id);
    assert_eq!(folder.kept_source().unwrap().file_name().unwrap(), "source.wav");
    assert_eq!(folder.lyrics().unwrap().lines.len(), 4);

    // A page that is not there, and a recording that is too long, are sentences.
    let gone = desk
        .start(
            Source::Link { url: "https://recordings.test/gone".into(), by: None },
            StudyOptions::default(),
            Some("Gone"),
        )
        .unwrap();
    let failed = desk.wait(&gone.id, LONG).unwrap().study;
    assert_eq!(failed.state, State::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some(
            "Fetching the recording failed: the recording behind https://recordings.test/gone could not be fetched: [generic] Unable to download webpage: HTTP Error 404: Not Found"
        )
    );
    let long = desk.start(Source::Link { url: link_to(&path, 3000), by: None }, StudyOptions::default(), None).unwrap();
    let failed = desk.wait(&long.id, LONG).unwrap().study;
    assert_eq!(
        failed.error.as_deref(),
        Some(
            "Fetching the recording failed: the recording behind the link is 50 minutes long, and a recording is studied up to 15 minutes"
        )
    );
    assert!(desk.studies().folder(&long.id).kept_source().is_none());
}

#[test]
fn the_timed_test_runs_what_there_is_and_keeps_what_it_chose() {
    let root = tempfile::tempdir().unwrap();
    let desk = desk(root.path(), script());
    let setup = desk.setup();
    assert!(setup.machine().is_none());
    let mut said: Vec<(String, f32)> = Vec::new();
    let machine = setup
        .test(&program(), &AtomicBool::new(false), &mut |what, share| said.push((what.to_owned(), share)))
        .unwrap();
    assert_eq!(machine.chosen, Provider::Processor);
    assert_eq!(machine.results.len(), 1);
    let timed = &machine.results[0];
    assert_eq!((timed.device.as_str(), &timed.error), ("a script", &None));
    assert!(
        timed.checked.as_ref().is_some_and(|checked| checked.seconds_a_minute > 0.0) && timed.peak > 0,
        "{timed:?}"
    );
    assert_eq!(said.first().map(|(what, _)| what.as_str()), Some("Timing the processor"));
    assert_eq!(said.last(), Some(&("Done".to_owned(), 1.0)));
    assert!(said.windows(2).all(|pair| pair[0].1 <= pair[1].1), "{said:?}");
    // It is kept beside the parts, and read again.
    assert_eq!(setup.machine(), Some(machine.clone()));
    assert_eq!(
        setup.choose(Provider::Nvidia).unwrap_err(),
        "the timed test did not run the models on an NVIDIA card through NVIDIA's libraries. It ran them on: the processor"
    );
    assert_eq!(setup.choose(Provider::Processor).unwrap(), machine);
    // A program that is not one that studies is a line in the result, not an error of the test.
    let broken = setup.test(&root.path().join("no such program"), &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert!(broken.results[0].error.as_deref().is_some_and(|why| why.contains("could not be started")), "{broken:?}");
    assert_eq!(broken.chosen, Provider::Processor);
}

#[test]
fn two_programs_on_one_machine_study_one_at_a_time() {
    // Two desks, as two programs have them, each with studies of its own, and one turn between them.
    let root = tempfile::tempdir().unwrap();
    let (here, there) = (root.path().join("here"), root.path().join("there"));
    let desk_of = |folder: &Path| {
        let fetches = Fetches::new(Arc::new(Nothing));
        fetches.add(List { parts: Vec::new(), ..setup::list(folder.join("models")) }).unwrap();
        let script = Script { seconds: 0.4, ..script() };
        let setup = Setup::over(Arc::new(fetches), Some(script), Some(Programs::default()));
        Desk::new(DeskOptions {
            studies: folder.join("studies"),
            setup: Arc::new(setup),
            program: Some(program()),
            free: None,
            turn: Some(root.path().join("turn")),
        })
    };
    let (first, second) = (desk_of(&here), desk_of(&there));
    let a = first.start(song(root.path()), StudyOptions::default(), None).unwrap();
    // The first has the turn before the second asks.
    let began = std::time::Instant::now();
    while first.report(&a.id).unwrap().at.is_none() {
        assert!(began.elapsed() < LONG, "the first study never began");
        std::thread::sleep(Duration::from_millis(20));
    }
    let b = second.start(song(root.path()), StudyOptions::default(), None).unwrap();
    // While the first runs, the second waits and says for what; it has not begun a stage.
    let mut waited = false;
    loop {
        let (one, other) = (first.report(&a.id).unwrap(), second.report(&b.id).unwrap());
        if one.study.state == State::Done {
            break;
        }
        if other.behind_another {
            waited = true;
            assert!(other.at.is_none() && other.study.stages.iter().all(|stage| stage.state == StageState::Pending));
        }
        assert!(began.elapsed() < LONG, "the first study never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(waited, "the second study never waited for the first");
    let done = second.wait(&b.id, LONG).unwrap();
    assert_eq!(done.study.state, State::Done, "{:?}", done.study.error);
    assert!(!done.behind_another && !root.path().join("turn").exists(), "the turn is given back");

    // A turn left by a program that is gone is taken over.
    std::fs::write(root.path().join("turn"), "4294967294").unwrap();
    let c = first.start(song(root.path()), StudyOptions::default(), None).unwrap();
    assert_eq!(first.wait(&c.id, LONG).unwrap().study.state, State::Done);
}
