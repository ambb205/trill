//! A study's run, through the real process and the real files, with
//! scripted models in place of the real ones: nothing is fetched and no
//! graphics card is asked.
//!
//! The process is `trill-study-job`, the program this crate builds for
//! it; `trill` and `trill-mcp` start themselves.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use trill_core::audio::AudioBuffer;
use trill_media::{WavFormat, write_wav};
use trill_study::helper::{Job, Order, Outcome, Said, attend};
use trill_study::models::Which;
use trill_study::models::scripted::Script;
use trill_study::run::Programs;
use trill_study::store::{Source, Stage, StageState, State, Studies, StudyFolder, StudyOptions, partial_of};

mod common;
use common::{RATE, program, recording, script};

/// A new study of a recording made for the test.
fn study_in(root: &Path) -> StudyFolder {
    let source = root.join("small song.wav");
    recording(&source);
    Studies::new(root.join("studies"))
        .create("small song", Source::File { path: source }, StudyOptions::default())
        .unwrap()
        .0
}

/// Runs the study in a process of its own and answers with how it ended
/// and what it said. `stop_at` stops it when that stage begins.
fn run(folder: &StudyFolder, script: Script, limit: Option<u64>, stop_at: Option<Stage>) -> (Outcome, Vec<Said>) {
    let order =
        Order::Study { folder: folder.path.clone(), models: Which::Scripted(script), programs: Programs::default() };
    let mut job = Job::start(&program(), &order).unwrap();
    let stop = AtomicBool::new(false);
    let mut said = Vec::new();
    let (outcome, _) = attend(&mut job, limit, &stop, &mut |heard| {
        if matches!(heard, Said::Stage(stage) if Some(*stage) == stop_at) {
            stop.store(true, Ordering::Relaxed);
        }
        said.push(heard.clone());
    });
    (outcome, said)
}

fn partial_files(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(partial_files(&path));
        } else if path.extension().is_some_and(|ending| ending == "partial") {
            found.push(path);
        }
    }
    found
}

#[test]
fn a_recording_is_studied_stage_by_stage_in_a_process_of_its_own() {
    let root = tempfile::tempdir().unwrap();
    let folder = study_in(root.path());
    let (outcome, said) = run(&folder, script(), None, None);
    assert_eq!(outcome, Outcome::Done, "{said:?}");

    // It said each stage as it began it, in order, and how far it had come.
    let stages: Vec<Stage> =
        said.iter().filter_map(|said| if let Said::Stage(stage) = said { Some(*stage) } else { None }).collect();
    assert_eq!(stages, [Stage::Read, Stage::Stems, Stage::Beats, Stage::Words, Stage::WordTimes, Stage::Listen]);
    assert!(said.iter().any(|said| matches!(said, Said::Progress(share) if *share > 0.5)));
    assert!(said.contains(&Said::Device("a script".to_owned())));

    let study = folder.study().unwrap();
    assert_eq!((study.state, &study.error), (State::Done, &None));
    assert!((study.seconds - 20.1).abs() < 0.01);
    assert_eq!(study.stems, ["vocals", "drums", "bass", "other"]);
    assert_eq!(study.device.as_deref(), Some("a script"));
    assert!(
        study.stages.iter().all(|report| report.state == StageState::Done && report.seconds.is_some()),
        "{:?}",
        study.stages
    );

    // The stems add up to the recording, to what 24 bits hold.
    let mix = trill_media::decode(&root.path().join("small song.wav")).unwrap();
    let stems: Vec<AudioBuffer> =
        study.stems.iter().map(|name| trill_media::decode(&folder.stem(name)).unwrap()).collect();
    let worst = (0..mix.frames())
        .map(|index| (mix.left[index] - stems.iter().map(|stem| stem.left[index]).sum::<f32>()).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 4.0 / 8_388_608.0, "the stems are {worst} from the recording at worst");

    // What was found is in the folder: beats, words in lines with their times and notes, and what was heard.
    let beats = folder.beats().unwrap();
    assert!(beats.beats.len() >= 28 && !beats.downbeats.is_empty());
    let lyrics = folder.lyrics().unwrap();
    assert_eq!(lyrics.language.as_deref(), Some("en"));
    assert!(lyrics.placed);
    assert_eq!(
        lyrics.lines.len(),
        4,
        "{:?}",
        lyrics.lines.iter().map(|line| (line.start, line.end, &line.text)).collect::<Vec<_>>()
    );
    assert_eq!(lyrics.lines[0].text, "one two three four");
    let first = &lyrics.lines[0].words[0];
    assert!((first.start - 0.3).abs() < 0.03 && first.end > first.start + 0.2, "{first:?}");
    assert_eq!(
        lyrics.lines[0].words.iter().map(|word| word.note).collect::<Vec<_>>(),
        [Some(69), Some(72), Some(76), Some(72)]
    );
    let heard = folder.heard().unwrap();
    assert!((heard.grid.bpm - 100.0).abs() < 0.7, "{}", heard.grid.bpm);
    assert_eq!(heard.grid.tracker, trill_listen::Tracker::Model);
    assert_eq!(heard.stems, study.stems);
    assert!(!heard.bass.is_empty() && !heard.loops.is_empty());
    // And the outline of each stem, for whoever draws them: a column for each fiftieth of a second.
    let outlines = folder.outlines().unwrap();
    assert_eq!(outlines.stems.iter().map(|outline| outline.name.as_str()).collect::<Vec<_>>(), study.stems);
    assert_eq!(outlines.stems[2].max.len(), 1005);
    assert!(outlines.stems[2].max.iter().any(|&high| high > 40), "the bass is in its outline");
    assert!(partial_files(&folder.path).is_empty());
}

#[test]
fn a_run_that_is_stopped_is_taken_up_where_it_was() {
    let root = tempfile::tempdir().unwrap();
    let folder = study_in(root.path());
    // Stopped as the beats are begun: the stems are done, nothing after them is.
    let slow = Script { seconds: 1.5, ..script() };
    let (outcome, _) = run(&folder, slow, None, Some(Stage::Beats));
    assert_eq!(outcome, Outcome::Stopped);
    let stopped = folder.study().unwrap();
    assert_eq!(stopped.stems.len(), 4);
    assert!(folder.beats().is_none() && folder.heard().is_none());
    let written = std::fs::metadata(folder.stem("drums")).unwrap().modified().unwrap();

    let (outcome, said) = run(&folder, script(), None, None);
    assert_eq!(outcome, Outcome::Done, "{said:?}");
    let study = folder.study().unwrap();
    assert_eq!(study.state, State::Done);
    // The stems were not made again.
    let stems = study.stages.iter().find(|report| report.stage == Stage::Stems).unwrap();
    assert_eq!((stems.state, stems.note.as_str()), (StageState::Done, "already done"));
    assert_eq!(std::fs::metadata(folder.stem("drums")).unwrap().modified().unwrap(), written);
    assert!(folder.heard().is_some() && partial_files(&folder.path).is_empty());

    // A study that is whole is not done again at all.
    let (outcome, _) = run(&folder, Script { fail_at: Some(Stage::Stems), ..script() }, None, None);
    assert_eq!(outcome, Outcome::Done);
}

#[test]
fn a_model_that_fails_is_a_sentence_that_names_its_stage_and_the_run_goes_on_from_there() {
    let root = tempfile::tempdir().unwrap();
    let folder = study_in(root.path());
    let (outcome, _) = run(&folder, Script { fail_at: Some(Stage::Beats), ..script() }, None, None);
    assert_eq!(
        outcome,
        Outcome::Failed("Finding the beat failed: the scripted model was told to fail here".to_owned())
    );
    let study = folder.study().unwrap();
    assert_eq!(study.state, State::Failed);
    assert_eq!(study.error.as_deref(), Some("Finding the beat failed: the scripted model was told to fail here"));
    let state = |stage: Stage| study.stages.iter().find(|report| report.stage == stage).map(|report| report.state);
    assert_eq!(
        (state(Stage::Stems), state(Stage::Beats), state(Stage::Words)),
        (Some(StageState::Done), Some(StageState::Failed), None)
    );

    let (outcome, _) = run(&folder, script(), None, None);
    assert_eq!(outcome, Outcome::Done);
    let study = folder.study().unwrap();
    assert_eq!((study.state, study.error), (State::Done, None));
}

#[test]
fn a_model_that_takes_its_process_down_costs_a_sentence() {
    let root = tempfile::tempdir().unwrap();
    let folder = study_in(root.path());
    let (outcome, _) = run(&folder, Script { crash_at: Some(Stage::Words), ..script() }, None, None);
    let Outcome::Failed(sentence) = outcome else { panic!("{outcome:?}") };
    // Windows ends a process that aborts with a code; Unix with a signal, and no code.
    let how = if cfg!(windows) { "(it ended with code 0x" } else { "(it was ended by signal 6, SIGABRT)" };
    assert!(
        sentence.starts_with(&format!("the process that studies went down while writing down the words {how}")),
        "{sentence}"
    );
    assert!(
        sentence.contains("every stage of the study that had finished is kept: run it again to take it up there"),
        "{sentence}"
    );
    // And it is: the stems and the beats are there, and the next run finishes.
    assert!(folder.beats().is_some());
    assert_eq!(run(&folder, script(), None, None).0, Outcome::Done);
}

#[test]
fn a_run_that_takes_more_memory_than_it_was_given_is_stopped() {
    let root = tempfile::tempdir().unwrap();
    let folder = study_in(root.path());
    let (outcome, _) = run(&folder, Script { hog_at: Some(Stage::Stems), ..script() }, Some(600 << 20), None);
    let Outcome::Failed(sentence) = outcome else { panic!("{outcome:?}") };
    assert!(sentence.starts_with("the process that studies took more memory than this run is given ("), "{sentence}");
    assert!(sentence.contains(" of 600 MB) while splitting it into stems, and was stopped"), "{sentence}");
    assert!(partial_files(&folder.path).is_empty());
}

#[test]
fn a_recording_that_cannot_be_studied_says_why() {
    let root = tempfile::tempdir().unwrap();
    let studies = Studies::new(root.path().join("studies"));
    let fail = |path: PathBuf| -> String {
        let (folder, _) = studies.create("x", Source::File { path }, StudyOptions::default()).unwrap();
        match run(&folder, script(), None, None).0 {
            Outcome::Failed(sentence) => sentence,
            other => panic!("{other:?}"),
        }
    };
    let gone = fail(root.path().join("gone.wav"));
    assert!(gone.starts_with("Reading the audio failed: ") && gone.contains("gone.wav"), "{gone}");
    assert!(gone.ends_with("anything else through ffmpeg where it is installed"), "{gone}");

    let short = root.path().join("short.wav");
    write_wav(
        &short,
        &AudioBuffer { sample_rate: RATE, left: vec![0.1; 22_050], right: vec![0.1; 22_050] },
        WavFormat::Int16,
    )
    .unwrap();
    let short = fail(short);
    assert!(short.contains("there are 0.5 seconds of audio in") && short.ends_with("too little to study"), "{short}");

    // And a program that is not one that studies is said too.
    let order = Order::Study {
        folder: root.path().to_owned(),
        models: Which::Scripted(script()),
        programs: Programs::default(),
    };
    let missing = Job::start(&root.path().join("no such program"), &order).err().unwrap();
    assert!(
        missing.starts_with("the process that studies ('") && missing.contains("could not be started"),
        "{missing}"
    );
}

#[test]
fn the_models_are_timed_on_noise() {
    let order = Order::Check { models: Which::Scripted(script()), seconds: 8.0 };
    let mut job = Job::start(&program(), &order).unwrap();
    let (outcome, _) = attend(&mut job, None, &AtomicBool::new(false), &mut |_| {});
    let Outcome::Checked(checked) = outcome else { panic!("{outcome:?}") };
    assert_eq!(checked.device, "a script");
    assert!(checked.seconds_a_minute > 0.0 && checked.sum > 0.0, "{checked:?}");
    // The same noise gives the same stems, to the number.
    let mut again = Job::start(&program(), &order).unwrap();
    let Outcome::Checked(second) = attend(&mut again, None, &AtomicBool::new(false), &mut |_| {}).0 else { panic!() };
    assert_eq!(second.sum, checked.sum);
    assert!(!partial_of(Path::new("a.wav")).to_string_lossy().is_empty());
}
