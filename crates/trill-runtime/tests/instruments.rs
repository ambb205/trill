//! A set of sampled instruments on Trill's side of a fetch: handed its
//! files, it is checked against what the catalogue read, brought into the
//! folder the user chose, made known to the library, played, and what a
//! song owes for it is said.
//!
//! The catalogue here is one the test writes, of an instrument the test
//! makes (an SFZ file and two tones): nothing is fetched and nothing of
//! anyone's is read. The tests at the end are the same on real sets,
//! fetched from where they are published through the engine that fetches:
//! they run only when asked.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use trill_catalogue::check::{address, file_sum, tree_sum};
use trill_catalogue::{Catalogue, Fetch, Licence, Mark, Set};
use trill_core::ChainRef;
use trill_core::audio::AudioBuffer;
use trill_engine::AudioBackend;
use trill_engine::render::RenderOptions;
use trill_fetch::PartState;
use trill_fetch::scripted::Shelf;
use trill_library::{EntryKind, Query};
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Ask, INSTRUMENTS, Parts, Runtime, RuntimeOptions, SETS_FOLDER, Storage, Studying};

const SAMPLED: &str = "builtin:sampled";

/// Writes `seconds` of a tone at `hertz`.
fn tone(path: &Path, hertz: f64, seconds: f64) {
    let rate = 44_100.0;
    let frames = (seconds * rate) as usize;
    let channel: Vec<f32> = (0..frames)
        .map(|n| {
            let fade = (1.0 - n as f64 / frames as f64).min(n as f64 / 64.0).clamp(0.0, 1.0);
            ((n as f64 * hertz * std::f64::consts::TAU / rate).sin() * fade * 0.6) as f32
        })
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(path, &AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }, WavFormat::Int16)
        .unwrap();
}

/// A set as its maker lays it out, in `folder`: an instrument's file, two
/// recordings and a licence. Returns the instrument's file.
fn made_set(folder: &Path) -> PathBuf {
    tone(&folder.join("Samples").join("low.wav"), 130.81, 0.6);
    tone(&folder.join("Samples").join("high.wav"), 523.25, 0.6);
    std::fs::write(folder.join("LICENSE"), "Attribution 4.0 International\n").unwrap();
    let file = folder.join("Small Keys.sfz");
    let sfz = "<control> default_path=Samples/\n<global> ampeg_release=0.2\n\
               <region> sample=low.wav lokey=36 hikey=59 pitch_keycenter=48\n\
               <region> sample=high.wav lokey=60 hikey=84 pitch_keycenter=72\n";
    std::fs::write(&file, sfz).unwrap();
    file
}

/// A catalogue of that one set, as an attribution set fetched as an
/// archive or (`by_file`) file by file.
fn catalogue_of(folder: &Path, by_file: bool) -> Catalogue {
    let tree = tree_sum(folder).unwrap();
    let sum = |path: &str| file_sum(&folder.join(path)).unwrap();
    let size = |path: &str| std::fs::metadata(folder.join(path)).unwrap().len();
    let paths = ["LICENSE", "Samples/high.wav", "Samples/low.wav", "Small Keys.sfz"];
    let (fetch, files) = if by_file {
        let lines: String = paths.iter().map(|path| format!("{} {} {path}\n", sum(path), size(path))).collect();
        (
            "base = \"https://example.org/small-keys/raw/0123456789abcdef0123456789abcdef01234567/\"\nlist = \"small-keys\"".to_string(),
            format!("format = 1\n[list]\nsmall-keys = '''\n{lines}'''\n"),
        )
    } else {
        (
            format!(
                "archive = \"https://example.org/small-keys/archive/0123456789abcdef0123456789abcdef01234567.zip\"\n\
                 under = \"small-keys-0123456789abcdef0123456789abcdef01234567/\"\nsum = \"{}\"",
                tree.sum
            ),
            "format = 1\n".to_string(),
        )
    };
    let instruments = format!(
        r#"format = 1
[[set]]
id = "small-keys"
name = "Small Keys"
about = "Two tones that stand for a keyboard."
by = "A. Maker"
licence = "CC-BY-4.0"
licence_at = "https://example.org/small-keys/LICENSE"
licence_says = "Attribution 4.0 International"
credit = "Small Keys by A. Maker (CC BY 4.0)"
from = "https://example.org/small-keys"
version = "0123456789abcdef0123456789abcdef01234567"
read = "2026-11-17"
bytes = {bytes}
files = 4
[set.fetch]
{fetch}
[[set.instrument]]
file = "Small Keys.sfz"
text = "{text}"
name = "Small Keys"
kind = "keyboard"
words = "Two tones that stand for a keyboard"
range = [36, 84]
layers = 1
takes = 1
recordings = 2
bytes = {recordings}
"#,
        bytes = tree.bytes,
        text = sum("Small Keys.sfz"),
        recordings = size("Samples/high.wav") + size("Samples/low.wav"),
    );
    Catalogue::read(&instruments, &files, "format = 1\n").unwrap()
}

fn started() -> Runtime {
    Runtime::start(RuntimeOptions::hermetic()).expect("the runtime starts")
}

/// The peak of the song, rendered.
fn rendered_peak(runtime: &Runtime) -> f32 {
    let job = runtime.session.lock().render_job();
    let rendered = job.render(&RenderOptions { max_tail_seconds: 1.0, ..RenderOptions::default() }).unwrap();
    rendered.left.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn a_set_handed_over_is_checked_brought_in_found_by_the_library_played_and_credited() {
    let folder = tempfile::tempdir().unwrap();
    let handed = folder.path().join("downloads").join("small-keys-0123");
    made_set(&handed);
    let catalogue = catalogue_of(&handed, false);
    let set = catalogue.set("small-keys").unwrap();
    let kept = folder.path().join("Trill Instruments");
    let runtime = started();
    assert!(runtime.library.roots().is_empty() && runtime.here_of(&catalogue).is_empty());

    let arrived = runtime.take_set(set, &handed, &kept, &AtomicBool::new(false)).unwrap();
    assert_eq!((arrived.set.as_str(), arrived.files), ("small-keys", 4));
    let at = std::path::absolute(kept.join("small-keys")).unwrap();
    assert_eq!(arrived.folder, at);
    assert_eq!(arrived.instruments, [at.join("Small Keys.sfz")]);
    assert_eq!(
        arrived.library_folder,
        Some(std::path::absolute(&kept).unwrap()),
        "the folder the user chose is the library's"
    );
    assert!(!kept.join("small-keys.arriving").exists(), "nothing is left under the name it had while it was copied");
    assert!(handed.join("Small Keys.sfz").is_file(), "what was handed over is still where it was");

    // The library knows the instrument, as one thing; its recordings are not sounds to be found.
    assert!(runtime.library.wait_until_idle(Duration::from_secs(30)));
    let found = runtime.library.search(&Query { kind: Some(EntryKind::Instrument), ..Query::default() });
    let names: Vec<String> = found.entries().map(|entry| entry.name.clone()).collect();
    assert_eq!(names, ["Small Keys"]);
    assert!(runtime.library.search(&Query { kind: Some(EntryKind::Audio), ..Query::default() }).is_empty());
    let here = runtime.here_of(&catalogue);
    assert_eq!(
        here.get(&set.instruments[0].text),
        Some(&at.join("Small Keys.sfz")),
        "known for the catalogue's by its file"
    );

    // It plays, and the song owes its maker a line, once for both tracks.
    let file = at.join("Small Keys.sfz").display().to_string();
    {
        let mut session = runtime.session.lock();
        for name in ["Keys", "More keys"] {
            let track = session.edit(|project| Ok(project.add_track(name))).unwrap();
            session.add_device(ChainRef::Track(track), SAMPLED, Some(&file), None).unwrap();
            session
                .edit(|project| {
                    let clip = project.add_midi_clip(track, "c", 0.0, 2.0)?;
                    project.clip_mut(clip)?.add_notes(&[trill_core::Note {
                        pitch: 48,
                        start: 0.0,
                        length: 1.0,
                        velocity: 100,
                    }])
                })
                .unwrap();
        }
        assert!(session.problems().is_empty(), "{:?}", session.problems());
    }
    assert!(rendered_peak(&runtime) > 0.05, "the instrument sounds");
    let owed = runtime.owed_by(&catalogue);
    assert_eq!(owed.text(), "Small Keys by A. Maker (CC BY 4.0)");
    assert_eq!(owed.lines[0].1, ["Small Keys (Keys)", "Small Keys (More keys)"]);
    assert!(owed.free.is_empty() && owed.unknown.is_empty());
    // The program's own catalogue does not know this file: it says so, and guesses nothing.
    let owed = runtime.owed();
    assert!(owed.lines.is_empty(), "{owed:?}");
    assert_eq!(owed.unknown, ["Small Keys.sfz, on Keys", "Small Keys.sfz, on More keys"]);

    // Taken again, it is there already: nothing is copied, and nothing changes.
    let again = runtime.take_set(set, &handed, &kept, &AtomicBool::new(false)).unwrap();
    assert_eq!((again.folder, again.library_folder), (at.clone(), None));

    // Removed, the library no longer lists it.
    std::fs::remove_dir_all(&at).unwrap();
    runtime.set_removed(&at);
    assert!(runtime.library.wait_until_idle(Duration::from_secs(30)));
    assert!(runtime.library.search(&Query { kind: Some(EntryKind::Instrument), ..Query::default() }).is_empty());
    assert!(runtime.here_of(&catalogue).is_empty());
}

/// Puts a set on a shelf where its entry says it is published: as the
/// archive its source makes of it, or file by file.
fn publish(shelf: &Shelf, set: &Set, made: &Path) {
    let paths = ["LICENSE", "Samples/high.wav", "Samples/low.wav", "Small Keys.sfz"];
    let read = |path: &str| std::fs::read(path.split('/').fold(made.to_owned(), |at, piece| at.join(piece))).unwrap();
    match &set.fetch {
        Fetch::Archive { url, under, .. } => {
            let members: Vec<(String, Vec<u8>)> =
                paths.iter().map(|path| (format!("{under}{path}"), read(path))).collect();
            // With something else in it that is not the set's, as an archive of a repository has.
            let mut borrowed: Vec<(&str, &[u8], bool)> =
                members.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice(), true)).collect();
            borrowed.push(("elsewhere/notes.txt", b"not the set's", false));
            shelf.put(url, trill_fetch::zip::archive_of(&borrowed));
        }
        Fetch::Files { base, files } => {
            for file in files {
                shelf.put(&address(base, &file.path), read(&file.path));
            }
        }
    }
}

fn instruments_of(runtime: &Runtime) -> Vec<PathBuf> {
    assert!(runtime.library.wait_until_idle(Duration::from_secs(30)));
    let found = runtime.library.search(&Query { kind: Some(EntryKind::Instrument), ..Query::default() });
    let mut paths: Vec<PathBuf> = found.entries().map(|entry| entry.path.clone()).collect();
    paths.sort();
    paths
}

fn wait_for(runtime: &Runtime, set: &str, state: PartState) {
    let fetches = runtime.fetches().unwrap();
    for _ in 0..1_000 {
        if fetches.status_of(set).unwrap().state == state && (state.is_going() || !fetches.fetching()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{set} never came to {state:?}: {:?}", fetches.status_of(set).unwrap());
}

#[test]
fn the_sets_of_a_catalogue_are_asked_for_fetched_made_the_library_s_played_moved_and_removed() {
    let folder = tempfile::tempdir().unwrap();
    // Two sets as their makers publish them: one as the archive of a commit, one file by file.
    let (first, second) = (folder.path().join("made").join("one"), folder.path().join("made").join("two"));
    made_set(&first);
    let other = made_set(&second);
    std::fs::write(&other, format!("{}// another instrument\n", std::fs::read_to_string(&other).unwrap())).unwrap();
    let mut catalogue = catalogue_of(&first, false);
    let mut by_file = catalogue_of(&second, true).sets.remove(0);
    (by_file.id, by_file.name) = ("more-keys".to_owned(), "More Keys".to_owned());
    (by_file.licence, by_file.credit) = (Licence::Cc0, None);
    by_file.instruments[0].marks = vec![Mark::Starter];
    catalogue.sets.push(by_file);
    let shelf = Arc::new(Shelf::default());
    publish(&shelf, &catalogue.sets[0], &first);
    publish(&shelf, &catalogue.sets[1], &second);

    let config = folder.path().join("config");
    let storage = Storage::At { settings: config.join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let studying = Studying { fetcher: Some(shelf.clone()), parts: Some(Vec::new()), ..Studying::default() };
    let parts = Parts { studying, catalogue: Some(catalogue.clone()), ..Parts::default() };
    let runtime = Runtime::start_custom(options, parts).unwrap();
    let fetches = runtime.fetches().unwrap().clone();

    // The catalogue is a list of the engine that fetches: a part a set, each with whose it is.
    let kept = std::path::absolute(config.join(SETS_FOLDER)).unwrap();
    let lists = fetches.status();
    let list = lists.iter().find(|list| list.id == INSTRUMENTS).expect("the catalogue's sets are offered");
    assert_eq!((list.title.as_str(), list.parts.len()), ("Instruments", 2));
    assert_eq!(std::path::absolute(&list.folder).unwrap(), kept);
    let keys = &list.parts[0].part;
    assert!(keys.licence.starts_with("CC BY 4.0 (attribution). \u{201C}Attribution 4.0 International\u{201D} Read at https://example.org/small-keys/LICENSE"), "{}", keys.licence);
    assert_eq!(keys.credit, "Small Keys by A. Maker (CC BY 4.0)");
    assert!(
        keys.from.starts_with("A. Maker, at https://example.org/small-keys (as it was at 0123456789ab)"),
        "{}",
        keys.from
    );
    assert!(keys.files[0].sized_by_guess(), "an archive its source makes when asked is sized by guess");
    assert_eq!(list.parts[1].part.files.len(), 4, "a set that is fetched file by file is a file each");
    assert!(shelf.asked().is_empty() && runtime.library.roots().is_empty(), "nothing was fetched by being listed");

    // The starter selection is one of them: asked for, it comes, and is the library's.
    let asked = runtime.ask_for_instruments(&Ask::Starter).unwrap();
    assert_eq!(asked.queued, ["more-keys"]);
    wait_for(&runtime, "more-keys", PartState::Ready);
    assert_eq!(runtime.library.roots(), [kept.clone()], "the folder the sets are kept in is the library's");
    assert_eq!(instruments_of(&runtime), [kept.join("more-keys").join("Small Keys.sfz")]);
    assert_eq!(runtime.settings().library.roots, [kept.clone()], "and the settings remember it");
    assert_eq!(fetches.status_of("small-keys").unwrap().state, PartState::Missing, "only what was asked for");

    // Everything: what is here is left, the rest comes. Known for the catalogue's by their files.
    let asked = runtime.ask_for_instruments(&Ask::Everything).unwrap();
    assert_eq!((asked.queued.as_slice(), asked.here), (["small-keys".to_owned()].as_slice(), 1));
    wait_for(&runtime, "small-keys", PartState::Ready);
    let at = kept.join("small-keys");
    assert_eq!(instruments_of(&runtime), [kept.join("more-keys").join("Small Keys.sfz"), at.join("Small Keys.sfz")]);
    assert!(!at.join("notes.txt").exists() && !kept.join("elsewhere").exists(), "only what is the set's was kept");
    assert_eq!(runtime.catalogue_here().len(), 2);
    assert_eq!(
        catalogue.sets[0].check(&at).map(|whole| whole.files),
        Ok(4),
        "what the engine kept is the set as it was read"
    );

    // It plays, and the song owes its maker a line.
    let file = at.join("Small Keys.sfz").display().to_string();
    {
        let mut session = runtime.session.lock();
        let track = session.edit(|project| Ok(project.add_track("Keys"))).unwrap();
        session.add_device(ChainRef::Track(track), SAMPLED, Some(&file), None).unwrap();
        session
            .edit(|project| {
                let clip = project.add_midi_clip(track, "c", 0.0, 2.0)?;
                project.clip_mut(clip)?.add_notes(&[trill_core::Note {
                    pitch: 48,
                    start: 0.0,
                    length: 1.0,
                    velocity: 100,
                }])
            })
            .unwrap();
        assert!(session.problems().is_empty(), "{:?}", session.problems());
    }
    assert!(rendered_peak(&runtime) > 0.05, "the instrument sounds");
    assert_eq!(runtime.owed().text(), "Small Keys by A. Maker (CC BY 4.0)");

    // Kept on another disk, say: what was fetched goes along, and the library with it.
    let elsewhere = std::path::absolute(folder.path().join("big disk").join("sets")).unwrap();
    let said = runtime.keep_list_in(INSTRUMENTS, Some(elsewhere.clone())).unwrap();
    assert!(said.ends_with("what was fetched before was moved there"), "{said}");
    assert_eq!(runtime.library.roots(), [elsewhere.clone()], "the folder they were in is the library's no longer");
    assert_eq!(instruments_of(&runtime).len(), 2);
    assert!(
        elsewhere.join("small-keys").join("Samples").join("low.wav").is_file() && !kept.join("small-keys").exists()
    );

    // A set that is removed is the library's no longer; the last one takes the folder with it.
    fetches.remove("more-keys").unwrap();
    assert_eq!(instruments_of(&runtime), [elsewhere.join("small-keys").join("Small Keys.sfz")]);
    fetches.remove("small-keys").unwrap();
    assert!(runtime.library.roots().is_empty() && instruments_of(&runtime).is_empty());
    assert!(runtime.settings().library.roots.is_empty());

    // One set alone, by its id; and an ask that means nothing says so.
    let asked = runtime.ask_for_instruments(&Ask::Sets(vec!["small-keys".to_owned()])).unwrap();
    assert_eq!(asked.queued, ["small-keys"]);
    wait_for(&runtime, "small-keys", PartState::Ready);
    assert_eq!(runtime.library.roots(), [elsewhere]);
    assert_eq!(runtime.stop_instruments(), Ok(0));
    assert_eq!(
        runtime.ask_for_instruments(&Ask::Sets(vec!["nothing".to_owned()])).unwrap_err(),
        "that asks for no set of the catalogue"
    );
}

#[test]
fn a_set_that_is_not_all_there_or_not_what_was_read_is_not_made_the_library_s() {
    let folder = tempfile::tempdir().unwrap();
    let handed = folder.path().join("handed");
    made_set(&handed);
    let kept = folder.path().join("kept");
    let runtime = started();
    let stop = AtomicBool::new(false);

    for by_file in [false, true] {
        let catalogue = catalogue_of(&handed, by_file);
        let set = catalogue.set("small-keys").unwrap();
        let staging = kept.join("small-keys.arriving");
        let target = kept.join("small-keys");

        // Stopped before it began: nothing is anywhere but under the other name, and nothing is the library's.
        let error = runtime.take_set(set, &handed, &kept, &AtomicBool::new(true)).unwrap_err();
        assert!(error.starts_with("Taking Small Keys was stopped. What was copied is kept at "), "{error}");
        assert!(error.ends_with("and taking it again goes on from there."), "{error}");
        assert!(!target.exists() && runtime.library.roots().is_empty());

        // A fetch that ended half way: a recording did not come.
        let high = handed.join("Samples").join("high.wav");
        let aside = folder.path().join("high.wav");
        std::fs::rename(&high, &aside).unwrap();
        let error = runtime.take_set(set, &handed, &kept, &stop).unwrap_err();
        let said = if by_file {
            "1 of its 4 files are not there (Samples/high.wav)"
        } else {
            "it has 3 files where the set has 4: it was not unpacked to the end"
        };
        assert!(error.contains(said), "{error}");
        assert!(error.contains(&handed.display().to_string()), "it says where it looked: {error}");
        assert!(error.ends_with(
            "What was copied is kept, and taking it again from a folder that has all of it goes on from there."
        ));
        assert!(
            !target.exists() && staging.join("Small Keys.sfz").is_file(),
            "what came is kept, under the other name"
        );
        assert!(runtime.library.roots().is_empty(), "and nothing of it is the library's");

        // The source has changed: a recording is another file than was read. Nothing of the set is kept.
        tone(&high, 440.0, 0.6);
        let error = runtime.take_set(set, &handed, &kept, &stop).unwrap_err();
        assert!(error.contains("is not the set Trill read"), "{error}");
        assert!(
            error.contains("replaced where it is published") || error.contains("replaced where they are published"),
            "{error}"
        );
        assert!(error.ends_with("Nothing of it was kept."), "{error}");
        assert!(!target.exists() && !staging.exists() && runtime.library.roots().is_empty());

        // With the file that was read back, it arrives; what was left from before is gone on with.
        std::fs::remove_file(&high).unwrap();
        std::fs::rename(&aside, &high).unwrap();
        let arrived = runtime.take_set(set, &handed, &kept, &stop).unwrap();
        assert_eq!(arrived.files, 4);
        assert!(target.join("Samples").join("high.wav").is_file() && !staging.exists());

        // What lies at its place and is not it whole is not written over.
        std::fs::remove_file(target.join("Samples").join("low.wav")).unwrap();
        let error = runtime.take_set(set, &handed, &kept, &stop).unwrap_err();
        assert!(error.contains("move what is at") && error.contains("away first"), "{error}");
        assert!(!target.join("Samples").join("low.wav").exists(), "it was left as it was");

        // Told that a set arrived in a folder where there is another, it says so and adds nothing.
        let other = folder.path().join("other").join("small-keys");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("notes.txt"), "not a set").unwrap();
        assert!(runtime.set_arrived(set, &other).is_err());
        assert!(!runtime.library.roots().iter().any(|root| root.ends_with("other")));
        std::fs::remove_dir_all(&kept).unwrap();
        runtime.library.remove_root(&std::path::absolute(&kept).unwrap());
    }
}

// ---- the real thing: sets of the program's own catalogue, fetched from where they are published ----
//
// None of these runs unless it is asked for: each fetches over the network, with the system's
// `curl`, through the engine that fetches (`trill-fetch`) and the list the runtime hands it, which is
// how the Setup panel fetches a set. Everything goes into a temporary folder (`TMP` says where).

/// A runtime that keeps its settings and what it fetches under `folder`,
/// and fetches as a program does. No part of studies is offered.
fn fetching(folder: &Path) -> Runtime {
    let storage = Storage::At { settings: folder.join("config").join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let studying = Studying { parts: Some(Vec::new()), ..Studying::default() };
    let runtime = Runtime::start_custom(options, Parts { studying, ..Parts::default() }).unwrap();
    // As a program does when it starts: it fetches with curl, and offers the catalogue that is built in.
    runtime.study_and_fetch_as_usual();
    runtime
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Plays one note of the instrument in the file `at`, and answers with its peak.
fn peak_of(runtime: &Runtime, instrument: &trill_catalogue::Instrument, at: &Path) -> f32 {
    let key = instrument.range.map_or(60, |(low, high)| low + (high - low) / 2);
    let track = {
        let mut session = runtime.session.lock();
        let track = session.edit(|project| Ok(project.add_track(&instrument.name))).unwrap();
        session.add_device(ChainRef::Track(track), SAMPLED, Some(&at.display().to_string()), None).unwrap();
        session
            .edit(|project| {
                let clip = project.add_midi_clip(track, "c", 0.0, 2.0)?;
                project.clip_mut(clip)?.add_notes(&[trill_core::Note {
                    pitch: key,
                    start: 0.0,
                    length: 1.0,
                    velocity: 110,
                }])
            })
            .unwrap();
        track
    };
    let peak = rendered_peak(runtime);
    runtime.session.lock().edit(|project| project.remove_track(track).map(drop)).unwrap();
    peak
}

/// Whether an instrument is one that sounds on a key of its own as it loads.
fn sounds(instrument: &trill_catalogue::Instrument) -> bool {
    !instrument.marks.iter().any(|mark| matches!(mark, Mark::Silent | Mark::Incomplete))
}

/// Fetches the set `id` through the engine, checks it a second time by
/// the catalogue's own rule, finds its instruments in the library and
/// plays some. Answers with what its archive took (0 for a set that is
/// fetched file by file), how long the fetch took, and what was heard.
fn fetched_checked_and_played(runtime: &Runtime, set: &Set, play: usize) -> Result<(u64, f64, Vec<String>), String> {
    let fetches = runtime.fetches().unwrap();
    let folder = fetches.part_folder(&set.id).unwrap();
    let partial = folder.join(format!("{}.zip.partial", set.version));
    let (began, mut archive) = (std::time::Instant::now(), 0u64);
    fetches
        .fetch_now(&set.id, &AtomicBool::new(false), &mut |_, _| {
            archive = archive.max(std::fs::metadata(&partial).map_or(0, |found| found.len()));
        })
        .map_err(|ended| format!("{ended:?}"))?;
    let took = began.elapsed().as_secs_f64();
    // The catalogue's own rule, on what the engine kept: every file read again.
    let whole = set.check(&folder)?;
    if (whole.files, whole.bytes) != (set.files, set.bytes) {
        return Err(format!(
            "{} files and {} bytes arrived, where {} and {} were read",
            whole.files, whole.bytes, set.files, set.bytes
        ));
    }
    if !runtime.library.wait_until_idle(Duration::from_secs(600)) {
        return Err("the library did not finish looking through it".to_owned());
    }
    let here = runtime.catalogue_here();
    let mut heard = Vec::new();
    for instrument in &set.instruments {
        let Some(at) = here.get(&instrument.text) else {
            return Err(format!("{} is not in the library", instrument.file));
        };
        if heard.len() < play && sounds(instrument) {
            let peak = peak_of(runtime, instrument, at);
            if peak <= 0.001 {
                return Err(format!("{} is silent", instrument.name));
            }
            heard.push(format!("{} {:.0} dB", instrument.name, 20.0 * peak.max(1e-9).log10()));
        }
    }
    Ok((archive, took, heard))
}

/// Sets of the program's own catalogue, each fetched through the engine,
/// checked twice, found by the library, played and removed again.
///
/// By default a few small sets of different makers and both ways of
/// fetching. `TRILL_CATALOGUE_SETS` names others by their ids with commas
/// between, or `starter`, or `all`: every set, one at a time, each removed
/// when it has been checked (26 GB over the network, and an hour or two).
/// `TRILL_CATALOGUE_REPORT` names a file that gets a line for each set as
/// it is done: its id, `ok` with what its archive took and how long, or
/// `FAILED` with why; a set that file already says is `ok` is passed over,
/// so that a run that was stopped is gone on with. It stops before a set
/// when less than 30 GB are free on the disk.
#[test]
#[ignore = "fetches sets of sampled instruments from where they are published (megabytes, or with \
            TRILL_CATALOGUE_SETS=all gigabytes): cargo test -p trill-runtime --test instruments the_catalogue -- --ignored --nocapture"]
fn the_catalogue_s_own_sets_arrive_as_they_were_read() {
    let catalogue = Catalogue::shipped();
    let asked = std::env::var("TRILL_CATALOGUE_SETS")
        .unwrap_or_else(|_| "synth-bass-1,ixox-flute,jsteeldrum,electric-bass-yr,vcsl-didgeridoo".to_string());
    let sets: Vec<&Set> = match asked.as_str() {
        "all" => Ask::Everything.sets(catalogue),
        "starter" => Ask::Starter.sets(catalogue),
        ids => ids
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| catalogue.set(id).unwrap_or_else(|| panic!("there is no set '{id}' in the catalogue")))
            .collect(),
    };
    let report = std::env::var_os("TRILL_CATALOGUE_REPORT").map(PathBuf::from);
    let done: Vec<String> = report
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| {
            text.lines()
                .filter_map(|line| line.split_once('\t'))
                .filter(|(_, rest)| rest.starts_with("ok"))
                .map(|(id, _)| id.to_owned())
                .collect()
        })
        .unwrap_or_default();
    let say = |line: String| {
        println!("{line}");
        if let Some(path) = &report {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
            writeln!(file, "{line}").unwrap();
        }
    };
    let folder = tempfile::tempdir().unwrap();
    let runtime = fetching(folder.path());
    let fetches = runtime.fetches().unwrap().clone();
    // A few instruments of each set are played where every set is fetched; all of them otherwise.
    let play = if sets.len() > 20 { 2 } else { usize::MAX };
    let mut failed = Vec::new();
    for set in sets {
        if done.contains(&set.id) {
            continue;
        }
        if let Some(free) = trill_fetch::disk_free(folder.path()).filter(|free| *free < 30 << 30) {
            println!("stopped before {}: {:.0} GB are free on the disk", set.id, megabytes(free) / 1024.0);
            break;
        }
        match fetched_checked_and_played(&runtime, set, play) {
            Ok((archive, took, heard)) => say(format!(
                "{}\tok\t{archive}\t{took:.1} s\t{} files, {:.1} MB, whole and as it was read; {}",
                set.id,
                set.files,
                megabytes(set.bytes),
                heard.join("; ")
            )),
            Err(why) => {
                say(format!("{}\tFAILED\t{why}", set.id));
                failed.push(set.id.clone());
            }
        }
        // Each is removed when it has been checked: the next has the room.
        let _ = fetches.remove(&set.id);
        assert!(!fetches.part_folder(&set.id).unwrap().exists());
    }
    assert!(failed.is_empty(), "these did not arrive as they were read: {failed:?}");
}

/// A fetch that is stopped part of the way and asked for again: how much
/// of what had come is gone on from, measured. On a set fetched as one
/// archive (`TRILL_CATALOGUE_ARCHIVE`, by default one of 166 MB) and on
/// one fetched file by file (`TRILL_CATALOGUE_FILES`, 178 MB).
#[test]
#[ignore = "fetches two sets of sampled instruments, each twice in part (hundreds of megabytes): \
            cargo test -p trill-runtime --test instruments a_fetch_that_is_stopped -- --ignored --nocapture"]
fn a_fetch_that_is_stopped_is_gone_on_with_as_far_as_its_source_lets_it() {
    let catalogue = Catalogue::shipped();
    let of = |name: &str, usual: &str| {
        let id = std::env::var(name).unwrap_or_else(|_| usual.to_owned());
        catalogue.set(&id).unwrap_or_else(|| panic!("there is no set '{id}' in the catalogue"))
    };
    let folder = tempfile::tempdir().unwrap();
    let runtime = fetching(folder.path());
    let fetches = runtime.fetches().unwrap().clone();
    for set in [of("TRILL_CATALOGUE_ARCHIVE", "swagbass"), of("TRILL_CATALOGUE_FILES", "vsco-2-ce-cellos")] {
        let all = set.fetched_bytes();
        // Stopped when about four tenths of it have come.
        let stop = Arc::new(AtomicBool::new(false));
        let (stops, began) = (stop.clone(), std::time::Instant::now());
        let mut had = 0u64;
        let ended = fetches.fetch_now(&set.id, &stop, &mut |_, got| {
            had = got;
            if got >= all * 4 / 10 {
                stops.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        assert_eq!(ended, Err(trill_fetch::Ended::Stopped), "{}", set.id);
        let first = began.elapsed().as_secs_f64();
        let stopped = fetches.status_of(&set.id).unwrap();
        assert_eq!(stopped.state, PartState::Missing);
        // Asked for again: the least that is said to have come says what was gone on from. A source
        // that does not go on from the middle begins again, and the count falls back.
        let (again, mut least) = (std::time::Instant::now(), u64::MAX);
        fetches
            .fetch_now(&set.id, &AtomicBool::new(false), &mut |state, got| {
                if state == PartState::Fetching {
                    least = least.min(got);
                }
            })
            .unwrap();
        let second = again.elapsed().as_secs_f64();
        assert!(fetches.ready(&set.id));
        set.check(&fetches.part_folder(&set.id).unwrap()).unwrap();
        println!(
            "{}: {:.0} MB to fetch. Stopped after {first:.1} s with {:.0} MB come ({:.0} MB kept on disk); asked for again it went on from {:.0} MB and was whole in {second:.1} s",
            set.id,
            megabytes(all),
            megabytes(had),
            megabytes(stopped.on_disk),
            megabytes(if least == u64::MAX { 0 } else { least }),
        );
        fetches.remove(&set.id).unwrap();
    }
}

/// A set kept under a deep folder of the user's, on a system where a path
/// of more than 260 letters is not a matter of course: fetched both ways,
/// checked, found by the library and played.
#[test]
#[ignore = "fetches two small sets of sampled instruments into a deep folder (36 MB): \
            cargo test -p trill-runtime --test instruments a_set_kept_under_a_deep_folder -- --ignored --nocapture"]
fn a_set_kept_under_a_deep_folder_arrives_and_plays() {
    let catalogue = Catalogue::shipped();
    let folder = tempfile::tempdir().unwrap();
    let runtime = fetching(folder.path());
    // Deep enough that every file of a set lies past 260 letters.
    let mut deep = folder.path().join("Music");
    while deep.as_os_str().len() < 250 {
        deep.push("A folder with quite a long name");
    }
    runtime.keep_list_in(INSTRUMENTS, Some(deep.clone())).unwrap();
    for id in ["ixox-flute", "vcsl-didgeridoo"] {
        let set = catalogue.set(id).unwrap();
        let (_, took, heard) =
            fetched_checked_and_played(&runtime, set, usize::MAX).unwrap_or_else(|why| panic!("{id}: {why}"));
        let longest = match &set.fetch {
            Fetch::Files { files, .. } => files.iter().map(|file| file.path.len()).max().unwrap_or(0),
            Fetch::Archive { .. } => 0,
        };
        println!(
            "{id}: kept under a folder of {} letters{}, fetched in {took:.1} s, whole and as it was read; {}",
            deep.join(id).as_os_str().len(),
            if longest > 0 { format!(" (its longest path is {} more)", longest + 1) } else { String::new() },
            heard.join("; ")
        );
        assert!(deep.join(id).is_dir());
    }
    assert_eq!(runtime.library.roots(), [std::path::absolute(&deep).unwrap()]);
}
