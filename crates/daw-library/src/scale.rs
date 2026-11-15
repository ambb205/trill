//! The library at the size it is built for: a hundred thousand files.
//!
//! These tests measure what the user would feel (a search per keystroke,
//! the wait at launch) and fail if either leaves its budget. Each timing is
//! the best of several runs, because a test machine has other things going
//! on and the question is what the code costs, not what the neighbours do.

use crate::entry::{AudioFacts, Entry, EntryKind};
use crate::index::{FolderId, Index, IndexBuilder};
use crate::infer::{FolderReading, NameReading};
use crate::search::{Query, search};
use crate::settings::LibrarySettings;
use crate::tags::{Tag, Tags};
use crate::{Key, Library, store};
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

const FILES: usize = 100_000;

/// A search over a library this size must cost less than a frame or so.
const SEARCH_BUDGET: Duration = Duration::from_millis(10);

/// Opening the library from its saved index must not be felt at launch.
const WARM_START_BUDGET: Duration = Duration::from_millis(200);

struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % sides as u64) as usize
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.roll(from.len())]
    }
}

/// A library under `base` shaped like real ones: packs, each divided into
/// kinds of sound, with names in the styles packs use.
fn synthetic_library(base: &Path) -> Index {
    const MAKERS: &[&str] =
        &["NOIZU", "BISHU", "KITTY", "Analog", "Deep", "Future", "Vintage", "Lo-Fi", "Cinematic", "Urban"];
    const STYLES: &[&str] =
        &["House", "Techno", "Trap", "Garage", "Drum & Bass", "Ambient", "Hip Hop", "Dubstep", "Disco", "Pop"];
    const WORDS: &[&str] = &[
        "dark", "warm", "tight", "wide", "dusty", "bright", "heavy", "soft", "crisp", "deep", "punchy", "airy",
        "gritty", "smooth", "broken", "lush", "raw", "clean", "wonky", "hollow",
    ];
    const KEYS: &[&str] = &["Am", "C", "F#m", "Dm", "Gmin", "Ebmaj", "Bb", "Emin", "C#m", "Fmaj"];
    // Folder, the word files in it are named with, and whether they are loops.
    const FOLDERS: &[(&str, &str, bool)] = &[
        ("Drums/Kicks", "kick", false),
        ("Drums/Snares", "snare", false),
        ("Drums/Claps", "clap", false),
        ("Drums/Hi Hats", "hihat", false),
        ("Drums/Percussion", "perc", false),
        ("Loops/Drum Loops", "drum_loop", true),
        ("Loops/Bass Loops", "bass_loop", true),
        ("Loops/Melodic Loops", "synth_loop", true),
        ("One Shots/Bass", "bass", false),
        ("One Shots/Synth", "synth_stab", false),
        ("One Shots/FX", "fx_riser", false),
        ("One Shots/Vocals", "vocal", false),
    ];
    let packs = 100;
    let per_folder = FILES / (packs * FOLDERS.len()) + 1;

    let mut dice = Dice(0x9e37_79b9_7f4a_7c15);
    let mut builder = IndexBuilder::with_capacity(FILES);
    let mut made = 0;
    for library in 0..4 {
        let library_name = format!("Library {library}");
        let root_path = base.join(&library_name);
        let root = builder.folder(root_path.clone(), None).unwrap();
        for pack in 0..packs / 4 {
            let maker = dice.pick(MAKERS);
            let pack_name = format!("{maker} {} Vol {}", dice.pick(STYLES), pack + 1);
            let pack_path = root_path.join(&pack_name);
            let pack_id = builder.folder(pack_path.clone(), Some(root)).unwrap();
            let mut group: Option<(&str, FolderId)> = None;
            for (folder, kind, is_loop) in FOLDERS {
                let (group_name, leaf) = folder.split_once('/').unwrap();
                let group_id = match group {
                    Some((name, id)) if name == group_name => id,
                    _ => {
                        let id = builder.folder(pack_path.join(group_name), Some(pack_id)).unwrap();
                        group = Some((group_name, id));
                        id
                    }
                };
                let folder_path = pack_path.join(group_name).join(leaf);
                builder.folder(folder_path.clone(), Some(group_id)).unwrap();
                let reading = FolderReading::of(&[&library_name, &pack_name, group_name, leaf]);
                for number in 0..per_folder {
                    if made == FILES {
                        break;
                    }
                    let word = dice.pick(WORDS);
                    let (name, seconds) = if *is_loop {
                        let bpm = 80 + 5 * dice.roll(20);
                        let name = format!("{maker}_{kind}_{word}_{number:02}_{bpm}_{}", dice.pick(KEYS));
                        (name, 240.0 / bpm as f64 * 4.0)
                    } else {
                        (format!("{maker}_{kind}_{word}_{number:02}"), 0.2 + dice.roll(30) as f64 / 10.0)
                    };
                    let frames = (seconds * 44_100.0).round() as u64;
                    let inferred = NameReading::of(&name, &reading).finish(Some(seconds));
                    builder
                        .file(Entry {
                            path: folder_path.join(format!("{name}.wav")),
                            name,
                            kind: EntryKind::Audio,
                            size: 100_000 + dice.roll(900_000) as u64,
                            modified: Some(UNIX_EPOCH + Duration::new(1_700_000_000 + made as u64, 123_456_700)),
                            audio: Some(AudioFacts { frames: Some(frames), sample_rate: 44_100, channels: 2 }),
                            unreadable: false,
                            tempo: inferred.tempo,
                            key: inferred.key,
                            tags: inferred.tags,
                        })
                        .unwrap();
                    made += 1;
                }
            }
        }
    }
    builder.finish()
}

/// The shortest of `runs` timings of `work`, with the result of that run.
fn best_of<T>(runs: usize, mut work: impl FnMut() -> T) -> (Duration, T) {
    let mut best: Option<(Duration, T)> = None;
    for _ in 0..runs.max(1) {
        let started = Instant::now();
        let result = work();
        let elapsed = started.elapsed();
        if best.as_ref().is_none_or(|(shortest, _)| elapsed < *shortest) {
            best = Some((elapsed, result));
        }
    }
    best.expect("at least one run")
}

#[test]
fn searching_a_hundred_thousand_files_is_quicker_than_a_keystroke() {
    let started = Instant::now();
    let index = synthetic_library(Path::new("/sounds"));
    println!("built a {}-file index (names read, search text laid out) in {:?}", index.len(), started.elapsed());
    assert_eq!(index.len(), FILES);

    let text = |text: &str| Query { text: text.into(), limit: Some(500), ..Query::default() };
    let queries: Vec<(&str, Query)> = vec![
        ("everything, first 500", Query { limit: Some(500), ..Query::default() }),
        ("everything, unlimited", Query::default()),
        ("k", text("k")),
        ("ki", text("ki")),
        ("kick", text("kick")),
        ("kick, unlimited", Query::text("kick")),
        ("a (in nearly every name)", text("a")),
        ("dark kick", text("dark kick")),
        ("noizu techno snare tight", text("noizu techno snare tight")),
        ("drum loop 140", text("drum loop 140")),
        ("bass f#m", text("bass f#m")),
        ("vol 17 hats", text("vol 17 hats")),
        ("zzzz (nothing)", text("zzzz")),
        ("tag: loop + drum", Query { tags: Tags::of(&[Tag::Loop, Tag::Drum]), limit: Some(500), ..Query::default() }),
        (
            "tempo 120-130, key Am",
            Query { tempo: Some(120.0..=130.0), key: Key::parse("Am"), limit: Some(500), ..Query::default() },
        ),
        ("one-shots under 1 s", Query { tags: Tags::of(&[Tag::OneShot]), seconds: Some(0.0..=1.0), ..text("") }),
        ("warm, in one pack", Query { folder: Some(index.folders[1].path().to_path_buf()), ..text("warm") }),
        (
            "text and every filter at once",
            Query {
                text: "deep bass loop".into(),
                kind: Some(EntryKind::Audio),
                tags: Tags::of(&[Tag::Loop]),
                tempo: Some(100.0..=160.0),
                seconds: Some(1.0..=20.0),
                limit: Some(500),
                ..Query::default()
            },
        ),
    ];

    let mut slowest = Duration::ZERO;
    for (label, query) in &queries {
        let (elapsed, (ids, total)) = best_of(7, || search(&index, query, &[]));
        println!("{:>9.3} ms  {label}: {total} matches, {} returned", elapsed.as_secs_f64() * 1000.0, ids.len());
        slowest = slowest.max(elapsed);
        assert!(elapsed < SEARCH_BUDGET, "'{label}' took {elapsed:?}");
    }
    println!("slowest search: {slowest:?}");

    // The numbers are only worth having if the searches found things.
    let count = |query: &Query| search(&index, query, &[]).1;
    assert_eq!(count(&Query::default()), FILES);
    assert!(count(&Query::text("kick")) > 5_000);
    assert!(count(&Query::text("dark kick")) > 100);
    assert_eq!(count(&Query::text("zzzz")), 0);
    assert!(count(&Query { tags: Tags::of(&[Tag::Loop, Tag::Drum]), ..Query::default() }) > 5_000);
}

#[test]
fn a_hundred_thousand_files_open_from_the_saved_index_at_once() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("library-index.json");
    // Roots as a real library has them: absolute, so that opening the
    // library finds them as it left them.
    let base = folder.path().join("sounds");
    let index = synthetic_library(&base);

    let (saving, ()) = best_of(3, || store::save(&path, &index).unwrap());
    let megabytes = std::fs::metadata(&path).unwrap().len() as f64 / 1_048_576.0;
    println!("saved {} files in {saving:?} ({megabytes:.1} MiB)", index.len());

    let roots = (0..4).map(|library| base.join(format!("Library {library}"))).collect();
    let settings = LibrarySettings { roots, ..Default::default() };
    let (opening, library) = best_of(5, || Library::open(&settings, Some(path.clone())));
    println!("warm start (Library::open with a {}-file index): {opening:?}", index.len());
    assert_eq!(library.index_problem(), None);
    let loaded = library.index();
    assert_eq!(loaded.len(), FILES);
    let differing = loaded.entries().iter().zip(index.entries()).find(|(loaded, saved)| loaded != saved);
    assert!(differing.is_none(), "what is loaded is not what was saved: {differing:#?}");
    assert_eq!(library.search(&Query::text("dark kick")).total(), search(&index, &Query::text("dark kick"), &[]).1);
    assert!(opening < WARM_START_BUDGET, "opening took {opening:?}");
}
