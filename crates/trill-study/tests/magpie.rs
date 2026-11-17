//! What a study finds in recordings that Magpie took apart, against what
//! Magpie found in them: the stems, the beats, the words and where each is
//! sung, with what the run took. Opt-in, because it needs the owner's
//! Magpie projects, the parts a setup fetched, and a minute of a graphics
//! card for each recording:
//!
//! ```text
//! TRILL_MAGPIE_PROJECTS=<...>/data/projects TRILL_STUDY_PARTS=<the folder the parts were fetched into> \
//!   cargo test -p trill-study --test magpie -- --ignored --nocapture --test-threads 1
//! ```
//!
//! `TRILL_STUDY_ON` says what runs the models: `processor` (the default),
//! `card`, `card2` or `nvidia`. Nothing of a recording is printed but
//! numbers, and nothing is written outside a temporary folder.
//!
//! What it holds a study to is what was measured when it was written, with
//! room: Magpie's own two runs of one recording differ by as much in their
//! stems (its model shifts the recording by a random amount), and its
//! transcriber is another model than the one a study runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use trill_study::helper::{Job, Order, Outcome, attend};
use trill_study::models::Which;
use trill_study::models::real::{Provider, STEMS_BEST, Setup};
use trill_study::run::Programs;
use trill_study::store::{Source, StemChoice, Studies, StudyOptions};
use trill_study::{link, room};

fn provider() -> Provider {
    match std::env::var("TRILL_STUDY_ON").unwrap_or_default().as_str() {
        "nvidia" => Provider::Nvidia,
        "card" => Provider::AnyCard { adapter: 0 },
        "card2" => Provider::AnyCard { adapter: 1 },
        _ => Provider::Processor,
    }
}

/// A word without its case and its punctuation.
fn plain(text: &str) -> String {
    text.to_lowercase().chars().filter(|letter| letter.is_alphanumeric() || *letter == '\'').collect()
}

/// The pairs of words two transcripts have in common, in order: the
/// longest run of them there is.
fn in_common(ours: &[(String, f64)], theirs: &[(String, f64)]) -> Vec<(usize, usize)> {
    let (n, m) = (ours.len(), theirs.len());
    let mut longest = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            longest[i][j] = if ours[i].0 == theirs[j].0 {
                longest[i + 1][j + 1] + 1
            } else {
                longest[i + 1][j].max(longest[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut pairs) = (0, 0, Vec::new());
    while i < n && j < m {
        if ours[i].0 == theirs[j].0 {
            pairs.push((i, j));
            (i, j) = (i + 1, j + 1);
        } else if longest[i + 1][j] >= longest[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// How well `found` agrees with `known`, each a list of times: the share
/// of each that has one of the other within 70 ms, as one number.
fn agreement(known: &[f64], found: &[f64]) -> f64 {
    let (first, last) = match (known.first(), known.last(), found.first(), found.last()) {
        (Some(a), Some(b), Some(c), Some(d)) => (a.max(*c) - 0.05, b.min(*d) + 0.05),
        _ => return 0.0,
    };
    let within =
        |times: &[f64]| -> Vec<f64> { times.iter().copied().filter(|time| (first..=last).contains(time)).collect() };
    let (known, found) = (within(known), within(found));
    let mut used = vec![false; found.len()];
    let mut hits = 0;
    for time in &known {
        let nearest = found
            .iter()
            .enumerate()
            .filter(|(index, other)| !used[*index] && (*other - time).abs() <= 0.07)
            .min_by(|a, b| (a.1 - time).abs().total_cmp(&(b.1 - time).abs()));
        if let Some((index, _)) = nearest {
            used[index] = true;
            hits += 1;
        }
    }
    let (precision, recall) = (hits as f64 / found.len().max(1) as f64, hits as f64 / known.len().max(1) as f64);
    if precision + recall == 0.0 { 0.0 } else { 2.0 * precision * recall / (precision + recall) }
}

/// The level of `theirs` over the level of what `ours` differs from it by,
/// in dB, and how loud `theirs` is.
fn alike(theirs: &Path, ours: &Path) -> (f64, f64) {
    let (a, b) = (trill_media::decode(theirs).unwrap(), trill_media::decode(ours).unwrap());
    assert_eq!(a.sample_rate, b.sample_rate, "{}", theirs.display());
    let frames = a.frames().min(b.frames());
    let (mut level, mut difference) = (0.0f64, 0.0f64);
    for (x, y) in [(&a.left, &b.left), (&a.right, &b.right)] {
        for index in 0..frames {
            level += f64::from(x[index]).powi(2);
            difference += f64::from(x[index] - y[index]).powi(2);
        }
    }
    (10.0 * (level / difference.max(1e-20)).log10(), 10.0 * (level / (2 * frames).max(1) as f64).max(1e-20).log10())
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

#[test]
#[ignore = "needs Magpie's projects (TRILL_MAGPIE_PROJECTS) and the parts a setup fetched (TRILL_STUDY_PARTS)"]
fn a_study_finds_what_magpie_found_in_the_recordings_it_took_apart() {
    let projects = PathBuf::from(
        std::env::var("TRILL_MAGPIE_PROJECTS").expect("TRILL_MAGPIE_PROJECTS is the folder of Magpie's projects"),
    );
    let parts = PathBuf::from(
        std::env::var("TRILL_STUDY_PARTS").expect("TRILL_STUDY_PARTS is the folder a setup fetched the parts into"),
    );
    let provider = provider();
    let scratch = tempfile::tempdir().unwrap();
    let studies = Studies::new(scratch.path());
    let program = PathBuf::from(env!("CARGO_BIN_EXE_trill-study-job"));
    let mut folders: Vec<PathBuf> = std::fs::read_dir(&projects)
        .unwrap()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|folder| folder.join("project.json").is_file())
        .collect();
    folders.sort();
    assert!(!folders.is_empty(), "no project of Magpie's in {}", projects.display());
    let (mut their_words, mut shared_words) = (0usize, 0usize);

    for folder in &folders {
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        let project: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join("project.json")).unwrap()).unwrap();
        let source = std::fs::read_dir(folder)
            .unwrap()
            .filter_map(|entry| Some(entry.ok()?.path()))
            .find(|path| path.file_stem().is_some_and(|stem| stem == "source"))
            .expect("the project keeps its recording");
        let seconds = project["duration"].as_f64().unwrap_or(0.0);
        // The stems Magpie made with its four models are compared with ours of the four, where they were fetched.
        let best = project["options"]["stems"] == "best" && parts.join(STEMS_BEST).join("htdemucs_ft_0.onnx").is_file();
        let options =
            StudyOptions { stems: if best { StemChoice::Best } else { StemChoice::Fast }, ..StudyOptions::default() };
        let (study, _) = studies.create(&name, Source::File { path: source }, options).unwrap();
        let order = Order::Study {
            folder: study.path.clone(),
            models: Which::Real(Setup { parts: parts.clone(), provider }),
            programs: Programs { ffmpeg: link::find_program("ffmpeg"), fetcher: None },
        };
        let began = Instant::now();
        let mut job = Job::start(&program, &order).unwrap();
        let (outcome, held) = attend(&mut job, Some(room::need(seconds)), &AtomicBool::new(false), &mut |_| {});
        assert_eq!(outcome, Outcome::Done, "{name}");
        let done = study.study().unwrap();
        println!(
            "{name}: {:.0} s of music ({} stems) studied in {:.1} s on {}, {} at the most of {} given",
            seconds,
            if best { "the best" } else { "the fast" },
            began.elapsed().as_secs_f64(),
            done.device.as_deref().unwrap_or("?"),
            room::said(held.peak),
            room::said(room::need(seconds)),
        );
        let stages: Vec<String> = done
            .stages
            .iter()
            .map(|stage| format!("{} {:.1}", stage.stage.while_doing(), stage.seconds.unwrap_or(0.0)))
            .collect();
        println!("  {}", stages.join(" | "));

        // The stems, against the ones Magpie kept.
        let mut said = Vec::new();
        for stem in &done.stems {
            let (decibels, level) = alike(&folder.join("stems").join(format!("{stem}.flac")), &study.stem(stem));
            said.push(format!("{stem} {decibels:.1}"));
            // A stem with next to nothing in it is alike or not by chance.
            if level > -50.0 {
                assert!(decibels > 10.0, "{name}: the {stem} of a study are {decibels:.1} dB from Magpie's");
            }
        }
        println!("  stems, Magpie's over what differs: {} dB", said.join(" | "));

        // The beats and the bar lines, against Magpie's grid.
        let times = |value: &serde_json::Value| -> Vec<f64> {
            value.as_array().into_iter().flatten().filter_map(serde_json::Value::as_f64).collect()
        };
        let their_beats = times(&project["grid"]["beats"]);
        let their_bars: Vec<f64> = project["grid"]["bars"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|index| their_beats.get(index.as_u64()? as usize).copied())
            .collect();
        let beats = study.beats().unwrap();
        let (on_beats, on_bars) = (agreement(&their_beats, &beats.beats), agreement(&their_bars, &beats.downbeats));
        let tracker = project["grid"]["tracker"].as_str().unwrap_or("?");
        println!(
            "  beats {on_beats:.3}, bar lines {on_bars:.3} against Magpie's grid (which is its {tracker} tracker's)"
        );
        if tracker == "beat_this" {
            // The same model in another runtime: the same beats.
            assert!(on_beats > 0.97 && on_bars > 0.95, "{name}: beats {on_beats:.3}, bar lines {on_bars:.3}");
        }

        // The words, and where each is sung.
        let words_of = |lines: &serde_json::Value| -> Vec<(String, f64)> {
            let mut words = Vec::new();
            for line in lines.as_array().into_iter().flatten() {
                for word in line["words"].as_array().into_iter().flatten() {
                    let text = plain(word["text"].as_str().unwrap_or(""));
                    if !text.is_empty() {
                        words.push((text, word["start"].as_f64().unwrap_or(0.0)));
                    }
                }
            }
            words
        };
        let theirs = words_of(&project["lyrics"]["lines"]);
        let ours = words_of(&serde_json::to_value(study.lyrics().unwrap()).unwrap()["lines"]);
        let pairs = in_common(&ours, &theirs);
        let mut apart: Vec<f64> = pairs.iter().map(|&(i, j)| (ours[i].1 - theirs[j].1).abs()).collect();
        let near = apart.iter().filter(|seconds| **seconds <= 0.2).count();
        println!(
            "  words: Magpie's {}, a study's {}, the same {} | where they begin: {:.0} ms apart at the median, {:.0} % within 200 ms",
            theirs.len(),
            ours.len(),
            pairs.len(),
            median(&mut apart) * 1000.0,
            100.0 * near as f64 / pairs.len().max(1) as f64,
        );
        their_words += theirs.len();
        shared_words += pairs.len();
        if theirs.is_empty() {
            // Nobody sings: nothing is made up of what is left of the band in the vocal stem.
            assert!(ours.is_empty(), "{name}: {} words where Magpie heard none", ours.len());
        } else {
            assert!(
                median(&mut apart) < 0.08,
                "{name}: the words begin {:.0} ms from Magpie's at the median",
                median(&mut apart) * 1000.0
            );
            // And not many times as many: a transcriber that goes round in circles writes hundreds.
            assert!(
                ours.len() < theirs.len() * 3 / 2 + 20,
                "{name}: {} words where Magpie has {}",
                ours.len(),
                theirs.len()
            );
        }
        assert!(held.peak <= room::need(seconds), "{name}: {} held", room::said(held.peak));
    }
    println!(
        "of Magpie's {their_words} words in all, a study has {shared_words} ({:.0} %)",
        100.0 * shared_words as f64 / their_words.max(1) as f64
    );
    assert!(shared_words * 4 >= their_words * 3, "{shared_words} of {their_words}");
}
