//! What this crate hears in recordings that Magpie took apart, against
//! what Magpie found in them. Magpie is the owner's project whose method
//! this crate translates; its saved projects hold the stems of a recording
//! and everything it found, which makes them the reference to hold a
//! translation to.
//!
//! Opt-in, because the recordings are on one machine and are nobody's to
//! pass on. Point `TRILL_MAGPIE_PROJECTS` at Magpie's `data/projects`:
//!
//! ```sh
//! TRILL_MAGPIE_PROJECTS=<...>/data/projects \
//!     cargo test -p trill-listen --test magpie -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Nothing is written anywhere; the numbers are printed.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Instant;
use trill_listen::{Heard, HitKind, Options, Recording, Sound, Stem, TrackedBeats, hear};

fn projects() -> Vec<PathBuf> {
    let Some(root) = std::env::var_os("TRILL_MAGPIE_PROJECTS") else {
        panic!("set TRILL_MAGPIE_PROJECTS to Magpie's data/projects folder");
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("{}: {error}", Path::new(&root).display()))
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|folder| folder.join("project.json").is_file())
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no projects in {}", Path::new(&root).display());
    found
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display())))
        .unwrap()
}

fn recording(folder: &Path, project: &Value) -> Recording {
    let stems = project["stems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stem| {
            let file = folder.join(stem["file"].as_str().unwrap());
            let buffer = trill_media::decode(&file).unwrap_or_else(|error| panic!("{error}"));
            Stem { name: stem["id"].as_str().unwrap().to_owned(), sound: Sound::of(&buffer) }
        })
        .collect();
    Recording::of_stems(stems)
}

fn times(values: &Value) -> Vec<f64> {
    values.as_array().map(|list| list.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
}

/// How well `ours` agrees with `theirs`, each time matched to at most one
/// within `reach` seconds: 1 is every one of both.
fn agreement(theirs: &[f64], ours: &[f64], reach: f64) -> f64 {
    let mut used = vec![false; ours.len()];
    let mut hits = 0usize;
    for &time in theirs {
        let best = (0..ours.len())
            .filter(|&index| !used[index] && (ours[index] - time).abs() <= reach)
            .min_by(|&a, &b| (ours[a] - time).abs().total_cmp(&(ours[b] - time).abs()));
        if let Some(index) = best {
            used[index] = true;
            hits += 1;
        }
    }
    let (precision, recall) = (hits as f64 / ours.len().max(1) as f64, hits as f64 / theirs.len().max(1) as f64);
    if precision + recall > 0.0 { 2.0 * precision * recall / (precision + recall) } else { 0.0 }
}

/// The share of the seconds `theirs` covers in which `same` holds for the
/// two things sounding then, read every twentieth of a second.
fn share<A, B>(theirs: &[(f64, f64, A)], ours: &[(f64, f64, B)], same: impl Fn(&A, &B) -> bool) -> f64 {
    let end = theirs.iter().map(|item| item.1).fold(0.0f64, f64::max);
    let (mut covered, mut agreed) = (0usize, 0usize);
    let mut time = 0.0;
    while time < end {
        if let Some(theirs) = theirs.iter().find(|item| item.0 <= time && time < item.1) {
            covered += 1;
            if ours.iter().any(|item| item.0 <= time && time < item.1 && same(&theirs.2, &item.2)) {
                agreed += 1;
            }
        }
        time += 0.05;
    }
    agreed as f64 / covered.max(1) as f64
}

fn tempo(beats: &[f64]) -> f64 {
    let mut gaps: Vec<f64> = beats.windows(2).map(|pair| pair[1] - pair[0]).collect();
    gaps.sort_by(f64::total_cmp);
    gaps.get(gaps.len() / 2).map_or(0.0, |gap| 60.0 / gap)
}

fn notes_of(list: &Value) -> Vec<(f64, f64, i64)> {
    list.as_array()
        .map(|notes| {
            notes
                .iter()
                .map(|note| {
                    (note["start"].as_f64().unwrap(), note["end"].as_f64().unwrap(), note["midi"].as_i64().unwrap())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One recording: the grid heard on its own, then everything else over
/// Magpie's grid, so that each finding is compared on the same bars.
fn compare(folder: &Path) -> (f64, f64) {
    let project = json(&folder.join("project.json"));
    let detail = json(&folder.join("detail.json"));
    let recording = recording(folder, &project);
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
    println!(
        "\n== {name}: {:.0} s, stems {:?}",
        recording.seconds(),
        recording.stems.iter().map(|stem| stem.name.as_str()).collect::<Vec<_>>()
    );

    // The grid from the onsets alone.
    let started = Instant::now();
    let own = hear(&recording, &Options::default());
    let took = started.elapsed().as_secs_f64();
    let grid = &project["grid"];
    let their_beats = times(&grid["beats"]);
    let their_bars: Vec<f64> =
        grid["bars"].as_array().unwrap().iter().map(|index| their_beats[index.as_u64().unwrap() as usize]).collect();
    let (low, high) = (
        own.grid.beats.first().copied().unwrap_or(0.0).max(their_beats[0]) - 0.05,
        own.grid.beats.last().copied().unwrap_or(0.0).min(*their_beats.last().unwrap()) + 0.05,
    );
    let within =
        |list: &[f64]| -> Vec<f64> { list.iter().copied().filter(|time| (low..=high).contains(time)).collect() };
    let own_bars: Vec<f64> = own.grid.bars.iter().map(|&index| own.grid.beats[index]).collect();
    let beats_agree = agreement(&within(&their_beats), &within(&own.grid.beats), 0.07);
    println!(
        "   grid from onsets ({took:.1} s for everything): {:.2} bpm against Magpie's {} ({}), beats agree {beats_agree:.3}, bar lines agree {:.3}; steady {} against {}",
        tempo(&own.grid.beats),
        grid["bpm"],
        grid["tracker"].as_str().unwrap_or("?"),
        agreement(&within(&their_bars), &within(&own_bars), 0.07),
        own.grid.steady,
        grid["steady"],
    );

    // Everything else over Magpie's own grid.
    let options = Options {
        beats: Some(TrackedBeats { beats: their_beats.clone(), downbeats: their_bars.clone() }),
        corrections: Vec::new(),
    };
    let heard: Heard = hear(&recording, &options);
    let harmony = heard.harmony.as_ref().unwrap();
    let their_key = &project["key"];
    let key_same = harmony.key.key.name() == their_key["name"].as_str().unwrap();
    let alternates: Vec<String> = harmony.key.alternates.iter().map(|other| other.key.name()).collect();
    println!(
        "   key {} ({:.2}; then {}) against {} ({}); tuning {:+.0} cents against {:+.0}",
        harmony.key.key.name(),
        harmony.key.confidence,
        alternates.join(", "),
        their_key["name"].as_str().unwrap(),
        their_key["confidence"],
        harmony.tuning_cents,
        project["tuning_cents"].as_f64().unwrap_or(0.0),
    );

    let their_chords: Vec<(f64, f64, (i64, String))> = project["chords"]
        .as_array()
        .unwrap()
        .iter()
        .map(|chord| {
            (
                chord["start"].as_f64().unwrap(),
                chord["end"].as_f64().unwrap(),
                (chord["root"].as_i64().unwrap(), chord["quality"].as_str().unwrap().to_owned()),
            )
        })
        .collect();
    let our_chords: Vec<(f64, f64, (i64, String))> = harmony
        .chords
        .iter()
        .map(|chord| (chord.start, chord.end, (i64::from(chord.root), chord.quality.suffix().to_owned())))
        .collect();
    let chords_same = share(&their_chords, &our_chords, |a, b| a == b);
    let roots_same = share(&their_chords, &our_chords, |a, b| a.0 == b.0);
    let sevenths = |list: &[(f64, f64, (i64, String))]| list.iter().filter(|chord| chord.2.1.contains('7')).count();
    // Which reading is right cannot be said without the song's own chords.
    // What can be said is how much of each is made of the key's notes
    // (Magpie's key for both; a minor key with its leading note).
    let (tonic, minor) = (their_key["tonic"].as_i64().unwrap(), their_key["minor"].as_bool().unwrap());
    let scale: &[i64] = if minor { &[0, 2, 3, 5, 7, 8, 10, 11] } else { &[0, 2, 4, 5, 7, 9, 11] };
    let in_key = |list: &[(f64, f64, (i64, String))]| -> f64 {
        let (mut inside, mut total) = (0.0, 0.0);
        for (start, end, (root, quality)) in list {
            let steps: &[i64] = match quality.as_str() {
                "" => &[0, 4, 7],
                "m" => &[0, 3, 7],
                "7" => &[0, 4, 7, 10],
                "maj7" => &[0, 4, 7, 11],
                _ => &[0, 3, 7, 10],
            };
            total += end - start;
            if steps.iter().all(|step| scale.contains(&(root + step - tonic).rem_euclid(12))) {
                inside += end - start;
            }
        }
        100.0 * inside / f64::max(total, 1e-9)
    };
    println!(
        "   chords: {} against {}; the same chord {:.0} % of the time, the same root {:.0} %; sevenths {} against {}",
        our_chords.len(),
        their_chords.len(),
        100.0 * chords_same,
        100.0 * roots_same,
        sevenths(&our_chords),
        sevenths(&their_chords),
    );
    println!(
        "   chords made of the key's notes: {:.0} % of ours, {:.0} % of theirs",
        in_key(&our_chords),
        in_key(&their_chords)
    );

    let their_parts: Vec<(usize, String)> = project["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| (part["bar"].as_u64().unwrap() as usize, part["label"].as_str().unwrap().to_owned()))
        .collect();
    let starts = |list: &[(usize, String)]| -> Vec<f64> { list.iter().map(|part| part.0 as f64).collect() };
    let our_parts: Vec<(usize, String)> = heard.parts.iter().map(|part| (part.bar, part.label.clone())).collect();
    let letters =
        |list: &[(usize, String)]| -> String { list.iter().map(|part| part.1.as_str()).collect::<Vec<_>>().join(" ") };
    println!(
        "   parts: {} against {}; boundaries agree {:.2} within a bar\n      ours   {}\n      theirs {}",
        our_parts.len(),
        their_parts.len(),
        agreement(&starts(&their_parts), &starts(&our_parts), 1.0),
        letters(&our_parts),
        letters(&their_parts),
    );

    let their_loops: Vec<String> =
        project["loops"].as_array().unwrap().iter().map(|found| found["id"].as_str().unwrap().to_owned()).collect();
    let same_loops = their_loops.iter().filter(|id| heard.loops.iter().any(|found| &found.id == *id)).count();
    println!(
        "   loops: {} against {}; {} of theirs are ours to the bar",
        heard.loops.len(),
        their_loops.len(),
        same_loops
    );

    let their_shots: Vec<(String, f64)> = project["oneshots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|shot| (shot["kind"].as_str().unwrap().to_owned(), shot["start"].as_f64().unwrap()))
        .collect();
    let mut kinds = String::new();
    for kind in HitKind::DRUMS.into_iter().chain([HitKind::Bass]) {
        let ours = heard.one_shots.iter().filter(|shot| shot.kind == kind).count();
        let theirs = their_shots.iter().filter(|shot| shot.0 == kind.key()).count();
        if ours + theirs > 0 {
            kinds.push_str(&format!(" {} {ours}/{theirs}", kind.key()));
        }
    }
    let same_shots = their_shots
        .iter()
        .filter(|shot| {
            heard.one_shots.iter().any(|ours| ours.kind.key() == shot.0 && (ours.start - shot.1).abs() < 0.02)
        })
        .count();
    println!("   one-shots (ours/theirs):{kinds}; {same_shots} of their {} are ours", their_shots.len());

    let their_groove = detail["groove"].as_array().cloned().unwrap_or_default();
    let mut groove = String::new();
    for kind in [HitKind::Kick, HitKind::Snare, HitKind::Hat] {
        let theirs: Vec<f64> = their_groove
            .iter()
            .filter(|note| note["kind"] == kind.key())
            .map(|note| note["time"].as_f64().unwrap())
            .collect();
        let ours: Vec<f64> = heard.groove.iter().filter(|note| note.kind == kind).map(|note| note.time).collect();
        groove.push_str(&format!(
            " {} {}/{} agree {:.2};",
            kind.key(),
            ours.len(),
            theirs.len(),
            agreement(&theirs, &ours, 0.03)
        ));
    }
    println!("   groove (ours/theirs):{groove}");

    for (name, ours) in [("bass", &heard.bass), ("vocals", &heard.melody)] {
        let theirs = notes_of(&detail["notes"][name]);
        let ours: Vec<(f64, f64, i64)> = ours.iter().map(|note| (note.start, note.end, i64::from(note.midi))).collect();
        println!(
            "   {name} notes: {} against {}; the same note {:.0} % of the time theirs sound",
            ours.len(),
            theirs.len(),
            100.0 * share(&theirs, &ours, |a, b| a == b)
        );
    }
    assert!(
        key_same || alternates.iter().any(|other| other == their_key["name"].as_str().unwrap()),
        "{name}: the key is not even among the runners-up"
    );
    (chords_same, beats_agree)
}

#[test]
#[ignore = "needs Magpie's saved projects: TRILL_MAGPIE_PROJECTS=<...>/data/projects"]
fn what_is_heard_in_magpies_recordings_is_what_magpie_found() {
    let mut chords = Vec::new();
    for folder in projects() {
        chords.push(compare(&folder).0);
    }
    let mean = chords.iter().sum::<f64>() / chords.len() as f64;
    println!("\nthe same chord {:.0} % of the time, over {} recordings", 100.0 * mean, chords.len());
    assert!(mean > 0.5, "chords agree only {:.0} % of the time", 100.0 * mean);
}
