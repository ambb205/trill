//! Scans sample folders and reports what the library makes of them: how
//! long a first scan, a restart and a rescan take, how fast searches are,
//! and what was read from the names.
//!
//! ```text
//! cargo run --release -p daw-library --example library_report [FOLDER]...
//! ```
//!
//! With no folders, the well-known sample folders found on this machine are
//! scanned. The index is written to a temporary file and removed again, so
//! the first scan is always a cold one and nothing of the user's is touched.

use daw_library::{EntryKind, Library, LibrarySettings, Query, ScanPhase, Tag, Tags, suggested_locations};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    let mut roots: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if roots.is_empty() {
        for location in suggested_locations() {
            println!("found {}: {}", location.label, location.path.display());
            roots.push(location.path);
        }
    }
    if roots.is_empty() {
        println!("No folders given and no well-known sample folders found.");
        return;
    }
    let index_path = std::env::temp_dir().join(format!("daw-library-report-{}.json", std::process::id()));
    let settings = LibrarySettings { roots, ..LibrarySettings::default() };

    cold_scan(&settings, &index_path);
    let library = warm_start(&settings, &index_path);
    searches(&library);
    metadata(&library);
    doubts(&library);
    drop(library);
    let _ = std::fs::remove_file(&index_path);
}

/// The first scan of the folders: everything has to be read.
fn cold_scan(settings: &LibrarySettings, index_path: &std::path::Path) {
    println!("\n== First scan ==");
    let library = Library::open(settings, Some(index_path.to_path_buf()));
    let started = Instant::now();
    library.rescan();
    let mut listed_after = None;
    let mut last_report = Instant::now();
    while !library.wait_until_idle(Duration::from_millis(20)) {
        let status = library.scan_status();
        if listed_after.is_none() && status.phase != ScanPhase::Listing {
            listed_after = Some(started.elapsed());
            println!(
                "{:>8.2?}  folders listed: {} files in {} folders are searchable by name; {} headers to read",
                started.elapsed(),
                status.files,
                status.folders,
                status.headers_to_read
            );
        }
        if last_report.elapsed() > Duration::from_secs(2) {
            last_report = Instant::now();
            println!(
                "{:>8.2?}  {:?}: {} of {} headers",
                started.elapsed(),
                status.phase,
                status.headers_read,
                status.headers_to_read
            );
        }
    }
    let summary = library.scan_status().last.expect("the scan ended");
    println!(
        "{:>8.2?}  done: {} files, {} headers read, {} unreadable",
        started.elapsed(),
        summary.files,
        summary.headers_read,
        summary.unreadable
    );
    println!("problems: {} described, {} more", summary.problems.len(), summary.problems_omitted);
    for problem in summary.problems.iter().take(6) {
        println!("  {problem}");
    }
}

/// A restart: the saved index is loaded, then a scan finds nothing new.
fn warm_start(settings: &LibrarySettings, index_path: &std::path::Path) -> Library {
    println!("\n== Restart ==");
    let megabytes = std::fs::metadata(index_path).map_or(0.0, |file| file.len() as f64 / 1_048_576.0);
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let started = Instant::now();
        let library = Library::open(settings, Some(index_path.to_path_buf()));
        best = best.min(started.elapsed());
        assert_eq!(library.index_problem(), None);
    }
    let library = Library::open(settings, Some(index_path.to_path_buf()));
    println!(
        "opening the library from its saved index ({megabytes:.1} MiB): {best:.2?} for {} files",
        library.index().len()
    );

    let started = Instant::now();
    library.rescan();
    library.wait_until_idle(Duration::from_secs(3_600));
    let summary = library.scan_status().last.expect("the scan ended");
    println!("rescan with nothing changed: {:.2?}, {} headers read", started.elapsed(), summary.headers_read);
    library
}

fn searches(library: &Library) {
    println!("\n== Searches (best of 9) ==");
    let text = |text: &str| Query { text: text.into(), limit: Some(500), ..Query::default() };
    let queries = [
        ("(everything)", text("")),
        ("k", text("k")),
        ("kick", text("kick")),
        ("a", text("a")),
        ("snare tight", text("snare tight")),
        ("vocal 140", text("vocal 140")),
        ("bass f#m", text("bass f#m")),
        ("hat (unlimited)", Query::text("hat")),
        ("loops 120-130 BPM", Query { tags: Tags::of(&[Tag::Loop]), tempo: Some(120.0..=130.0), ..text("") }),
        (
            "one-shot kicks under 1 s",
            Query { tags: Tags::of(&[Tag::Kick, Tag::OneShot]), seconds: Some(0.0..=1.0), ..text("") },
        ),
        ("zzzzzz", text("zzzzzz")),
    ];
    let mut slowest = Duration::ZERO;
    for (label, query) in queries {
        let mut best = Duration::MAX;
        let mut found = (0, 0);
        for _ in 0..9 {
            let started = Instant::now();
            let results = library.search(&query);
            best = best.min(started.elapsed());
            found = (results.total(), results.len());
        }
        slowest = slowest.max(best);
        let first = library.search(&query).entries().next().map(|entry| entry.name.clone()).unwrap_or_default();
        println!("{:>8.3} ms  {label}: {} matches; first: {first}", best.as_secs_f64() * 1000.0, found.0);
    }
    println!("slowest: {:.3} ms", slowest.as_secs_f64() * 1000.0);
}

fn metadata(library: &Library) {
    println!("\n== What was read ==");
    let index = library.index();
    for kind in EntryKind::ALL {
        println!("{:>8} {}", index.count(kind), kind.label());
    }
    let audio: Vec<_> =
        index.entries().iter().filter(|entry| entry.kind == EntryKind::Audio && !entry.unreadable).collect();
    let share = |count: usize| 100.0 * count as f64 / audio.len().max(1) as f64;
    let count = |test: &dyn Fn(&daw_library::Entry) -> bool| audio.iter().filter(|entry| test(entry)).count();
    let unreadable = index.entries().iter().filter(|entry| entry.unreadable).count();
    println!("{unreadable:>8} audio files that cannot be decoded");
    println!("of the {} readable audio files:", audio.len());
    for (label, count) in [
        ("have a tempo", count(&|entry| entry.tempo.is_some())),
        ("have a key", count(&|entry| entry.key.is_some())),
        ("have at least one tag", count(&|entry| !entry.tags.is_empty())),
        ("are loops", count(&|entry| entry.tags.contains(Tag::Loop))),
        ("are one-shots", count(&|entry| entry.tags.contains(Tag::OneShot))),
        ("state no length", count(&|entry| entry.seconds().is_none())),
        ("are mono", count(&|entry| entry.audio.is_some_and(|audio| audio.channels == 1))),
    ] {
        println!("{count:>8} ({:>4.1} %) {label}", share(count));
    }

    let mut by_tag: Vec<(usize, &str)> = Tag::all()
        .map(|tag| (count(&|entry| entry.tags.contains(tag)), tag.label()))
        .filter(|(count, _)| *count > 0)
        .collect();
    by_tag.sort_by(|a, b| b.cmp(a));
    println!("tags: {}", by_tag.iter().map(|(count, label)| format!("{label} {count}")).collect::<Vec<_>>().join(", "));

    let mut tempos: BTreeMap<u32, usize> = BTreeMap::new();
    let mut rates: BTreeMap<u32, usize> = BTreeMap::new();
    for entry in &audio {
        if let Some(tempo) = entry.tempo {
            *tempos.entry(tempo as u32).or_default() += 1;
        }
        if let Some(facts) = entry.audio {
            *rates.entry(facts.sample_rate).or_default() += 1;
        }
    }
    let mut common: Vec<(usize, u32)> = tempos.iter().map(|(tempo, count)| (*count, *tempo)).collect();
    common.sort_by(|a, b| b.cmp(a));
    let listed: Vec<String> = common.iter().take(12).map(|(count, tempo)| format!("{tempo} ({count})")).collect();
    println!("commonest tempos: {}", listed.join(", "));
    let range = tempos.keys().next().zip(tempos.keys().next_back());
    println!("tempo range: {range:?}; sample rates: {rates:?}");

    println!("\n== A sample of entries, to judge the reading by eye ==");
    let stride = (audio.len() / 70).max(1);
    for entry in audio.iter().step_by(stride) {
        let folder = entry
            .path
            .parent()
            .and_then(|parent| parent.file_name())
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        println!(
            "{:>7} {:>5} {:>4}  {:<36} {folder}/{}",
            entry.seconds().map_or("?".into(), |seconds| format!("{seconds:.2}s")),
            entry.tempo.map_or("-".into(), |tempo| format!("{tempo}")),
            entry.key.map_or("-", |key| key.name()),
            format!("{:?}", entry.tags),
            entry.name
        );
    }
}

/// Why files cannot be decoded, and the readings most open to doubt:
/// tempos taken from bare numbers, and keys.
fn doubts(library: &Library) {
    let index = library.index();
    let unreadable: Vec<_> = index.entries().iter().filter(|entry| entry.unreadable).collect();
    if !unreadable.is_empty() {
        println!("\n== Why files cannot be decoded (a sample of up to 400) ==");
        let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
        for entry in unreadable.iter().step_by((unreadable.len() / 400).max(1)) {
            let reason = match daw_media::probe(&entry.path) {
                Ok(_) => "readable now".to_string(),
                Err(error) => error.to_string().replace(&entry.path.display().to_string(), "<file>"),
            };
            *reasons.entry(reason).or_default() += 1;
        }
        for (reason, count) in reasons {
            println!("{count:>6}  {reason}");
        }
    }

    let readable = || index.entries().iter().filter(|entry| !entry.unreadable);
    let show = |title: &str, entries: Vec<&daw_library::Entry>| {
        println!("\n== {title}: {} files; a sample ==", entries.len());
        for entry in entries.iter().step_by((entries.len() / 40).max(1)) {
            let folder = entry.path.parent().and_then(|parent| parent.file_name()).map(|name| name.to_string_lossy());
            println!(
                "{:>7} {:>5} {:>4}  {}/{}",
                entry.seconds().map_or("?".into(), |seconds| format!("{seconds:.2}s")),
                entry.tempo.map_or("-".into(), |tempo| format!("{tempo}")),
                entry.key.map_or("-", |key| key.name()),
                folder.unwrap_or_default(),
                entry.name
            );
        }
    };
    let unmarked = |entry: &&daw_library::Entry| {
        let path = entry.path.to_string_lossy().to_lowercase();
        entry.tempo.is_some() && !path.contains("bpm")
    };
    show("Tempos read from numbers not marked bpm", readable().filter(unmarked).collect());
    show("Keys", readable().filter(|entry| entry.key.is_some()).collect());
    let no_length = |entry: &&daw_library::Entry| entry.audio.is_some_and(|audio| audio.frames.is_none());
    show("Audio whose header states no length", readable().filter(no_length).collect());
}
