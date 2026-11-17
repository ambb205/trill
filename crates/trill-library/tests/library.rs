//! The library end to end, on folders of generated audio: scanning,
//! saving, reloading, rescanning, searching and browsing.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use trill_library::{
    Entry, EntryKind, Key, Library, LibrarySettings, Query, ScanPhase, ScanSummary, Settings, Tag, Tags, natural_cmp,
};

const WAIT: Duration = Duration::from_secs(60);

/// A 16-bit WAV file of silence: `channels` at `rate`, `seconds` long.
fn wav(seconds: f64, rate: u32, channels: u16) -> Vec<u8> {
    let frames = (seconds * f64::from(rate)).round() as u32;
    let block = u32::from(channels) * 2;
    let data = frames * block;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * block).to_le_bytes());
    bytes.extend_from_slice(&(block as u16).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    bytes.resize(44 + data as usize, 0);
    bytes
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A small pack with one of everything a scan has to cope with.
fn pack(root: &Path) {
    let short = wav(0.25, 8_000, 1);
    for name in ["Kick 1.wav", "Kick 2.wav", "Kick 10.wav", "kick 3.WAV"] {
        write(&root.join("Drums/Kicks").join(name), &short);
    }
    write(&root.join("Drums/Snares/Tight.wav"), &wav(0.5, 44_100, 2));
    // Two bars at 120 BPM.
    write(&root.join("Loops/Melody Loop 120bpm Am.wav"), &wav(4.0, 8_000, 2));
    write(&root.join("Loops/Warm Pad 90 F#m.wav"), &wav(3.0, 8_000, 1));
    write(&root.join("Loops/readme.txt"), b"not for the library");
    write(&root.join("MIDI/Bassline 128bpm.mid"), b"MThd\0\0\0\x06\0\0\0\x01\0\x60");
    write(&root.join("Lead Saw.vstpreset"), b"VST3");
    write(&root.join("song.trill"), b"{}");
    // Not what its name says.
    write(&root.join("Broken/not audio.wav"), "just text\n".repeat(100).as_bytes());
    // Things a scan must not list.
    write(&root.join(".hidden/Secret Kick.wav"), &short);
    write(&root.join("__MACOSX/Drums/._Kick 1.wav"), b"\0\x05\x16\x07resource fork");
    write(&root.join("Drums/.DS_Store"), b"\0");
    write(&root.join("Docs/notes.txt"), b"no library files in here");
    std::fs::create_dir_all(root.join("Empty/Nested")).unwrap();
}

fn open(roots: &[&Path], index: Option<PathBuf>) -> Library {
    let settings =
        LibrarySettings { roots: roots.iter().map(|root| root.to_path_buf()).collect(), ..Default::default() };
    Library::open(&settings, index)
}

fn scan(library: &Library) -> ScanSummary {
    library.rescan();
    assert!(library.wait_until_idle(WAIT), "the scan did not finish");
    let status = library.scan_status();
    assert_eq!(status.phase, ScanPhase::Idle);
    assert!(!status.is_scanning());
    status.last.expect("a scan ran")
}

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

fn found(library: &Library, text: &str) -> Vec<String> {
    library.search(&Query::text(text)).entries().map(|entry| entry.name.clone()).collect()
}

#[test]
fn a_scan_lists_reads_and_describes_what_is_there() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("My Pack");
    pack(&root);
    let library = open(&[&root], None);
    assert!(library.index().is_empty(), "nothing is scanned until asked");
    let generation = library.generation();

    let summary = scan(&library);
    assert!(library.generation() > generation);
    assert_eq!((summary.files, summary.headers_read, summary.unreadable, summary.cancelled), (11, 8, 1, false));
    assert!(
        summary.problems.is_empty(),
        "a file that is not audio is no problem with the scan: {:?}",
        summary.problems
    );

    let index = library.index();
    assert_eq!(index.len(), 11);
    assert_eq!(index.count(EntryKind::Audio), 8);
    assert_eq!(
        (index.count(EntryKind::Midi), index.count(EntryKind::Preset), index.count(EntryKind::Project)),
        (1, 1, 1)
    );
    assert!(index.entries().iter().all(|entry| entry.size > 0 && entry.modified.is_some()));

    // The tree: only folders with something in them, files in natural order.
    let roots = index.roots();
    assert_eq!(roots.len(), 1);
    let top = index.folder(roots[0]).unwrap();
    assert_eq!((top.name(), top.path()), ("My Pack", root.as_path()));
    let subfolders: Vec<&str> = top.subfolders().iter().map(|id| index.folder(*id).unwrap().name()).collect();
    assert_eq!(subfolders, ["Broken", "Drums", "Loops", "MIDI"]);
    assert_eq!(names(&index.entries()[top.files()]), ["Lead Saw", "song"]);
    assert_eq!(top.all_files().len(), 11);
    let kicks = index.folder(index.find_folder(&root.join("Drums/Kicks")).unwrap()).unwrap();
    assert_eq!(names(&index.entries()[kicks.files()]), ["Kick 1", "Kick 2", "kick 3", "Kick 10"]);
    assert!(index.find_folder(&root.join("Empty")).is_none() && index.find_folder(&root.join("Docs")).is_none());
    assert!(index.find(&root.join(".hidden/Secret Kick.wav")).is_none());

    // What the headers say.
    let entry = |relative: &str| index.entry(index.find(&root.join(relative)).expect(relative)).unwrap().clone();
    let snare = entry("Drums/Snares/Tight.wav");
    let audio = snare.audio.unwrap();
    assert_eq!((audio.sample_rate, audio.channels), (44_100, 2));
    assert!((snare.seconds().unwrap() - 0.5).abs() < 1.0e-6);
    assert_eq!(entry("Drums/Kicks/Kick 1.wav").audio.unwrap().channels, 1);

    // What the names say.
    assert_eq!(snare.tags, Tags::of(&[Tag::Snare, Tag::Drum, Tag::OneShot]), "from its folder and its length");
    let melody = entry("Loops/Melody Loop 120bpm Am.wav");
    assert_eq!((melody.tempo, melody.key, melody.tags), (Some(120.0), Key::parse("Am"), Tags::of(&[Tag::Loop])));
    let pad = entry("Loops/Warm Pad 90 F#m.wav");
    assert_eq!((pad.tempo, pad.key), (Some(90.0), Key::parse("F#m")));
    assert!(pad.tags.contains(Tag::Pad) && pad.tags.contains(Tag::Loop));
    let midi = entry("MIDI/Bassline 128bpm.mid");
    assert_eq!((midi.kind, midi.tempo, midi.audio), (EntryKind::Midi, Some(128.0), None));
    assert!(midi.tags.contains(Tag::Bass));
    assert_eq!(entry("song.trill").tags, Tags::EMPTY, "a project is not a sound");

    // A file that is not audio is listed as unreadable and kept out of
    // searches unless they ask.
    let broken = entry("Broken/not audio.wav");
    assert!(broken.unreadable && broken.audio.is_none());
    let reason = trill_media::probe(&broken.path).unwrap_err().to_string();
    assert!(reason.contains("is not a WAV, AIFF, FLAC, MP3 or Ogg Vorbis file"), "{reason}");
    assert!(found(&library, "audio").is_empty());
    let everything = Query { text: "audio".into(), include_unreadable: true, ..Query::default() };
    assert_eq!(library.search(&everything).len(), 1);

    // Searching and filtering through the library.
    assert_eq!(found(&library, "kick"), ["Kick 1", "Kick 2", "kick 3", "Kick 10"]);
    assert_eq!(found(&library, "drums tight"), ["Tight"]);
    let loops = Query { tags: Tags::of(&[Tag::Loop]), tempo: Some(100.0..=130.0), ..Query::default() };
    let results = library.search(&loops);
    assert_eq!(results.entries().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), ["Melody Loop 120bpm Am"]);
    assert_eq!((results.len(), results.total(), results.is_empty()), (1, 1, false));
    assert_eq!(results.index().entry(results.ids()[0]).unwrap().name, "Melody Loop 120bpm Am");
    let limited = library.search(&Query { limit: Some(3), ..Query::default() });
    assert_eq!((limited.len(), limited.total()), (3, 10));
}

#[test]
fn the_index_is_saved_and_the_next_launch_starts_from_it() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Pack");
    pack(&root);
    let index_path = folder.path().join("cache/library-index.json");

    let before: Vec<Entry> = {
        let library = open(&[&root], Some(index_path.clone()));
        assert_eq!(library.index_problem(), None, "no index yet is not a problem");
        scan(&library);
        library.index().entries().to_vec()
    };
    assert!(index_path.is_file());

    // A new launch: everything is there before any scan.
    let library = open(&[&root], Some(index_path.clone()));
    assert_eq!(library.index_problem(), None);
    assert_eq!(library.index().entries(), before.as_slice());
    assert_eq!(found(&library, "kick").len(), 4);
    assert_eq!(library.scan_status().last, None);

    // Scanning again reads no file: nothing changed.
    let summary = scan(&library);
    assert_eq!((summary.files, summary.headers_read, summary.unreadable), (11, 0, 1));
    assert!(summary.problems.is_empty(), "a file known to be unreadable is not tried again: {:?}", summary.problems);
    assert_eq!(library.index().entries(), before.as_slice());

    // A launch with a root taken away drops that root's files at once.
    drop(library);
    let other = folder.path().join("Other");
    write(&other.join("Clap.wav"), &wav(0.2, 8_000, 1));
    let library = open(&[&other], Some(index_path));
    assert!(library.index().is_empty());
    assert_eq!(scan(&library).files, 1);
}

#[test]
fn a_rescan_reads_only_what_changed() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Pack");
    pack(&root);
    let index_path = folder.path().join("index.json");
    let library = open(&[&root], Some(index_path.clone()));
    scan(&library);

    // One file changes length, one changes content only (same size, new
    // time), one is new, one is deleted, one unreadable file is repaired
    // and one folder is renamed.
    write(&root.join("Drums/Kicks/Kick 1.wav"), &wav(0.75, 8_000, 1));
    let same_size = root.join("Drums/Kicks/Kick 2.wav");
    let later = std::fs::metadata(&same_size).unwrap().modified().unwrap() + Duration::from_secs(5);
    std::fs::File::options().write(true).open(&same_size).unwrap().set_modified(later).unwrap();
    write(&root.join("Drums/Kicks/Kick 11.wav"), &wav(0.1, 8_000, 1));
    std::fs::remove_file(root.join("Drums/Kicks/Kick 10.wav")).unwrap();
    write(&root.join("Broken/not audio.wav"), &wav(1.0, 8_000, 1));
    std::fs::rename(root.join("Loops"), root.join("Phrases")).unwrap();

    let summary = scan(&library);
    // Changed, touched, new, repaired, and the two under the renamed folder.
    assert_eq!((summary.files, summary.headers_read, summary.unreadable), (11, 6, 0));
    let index = library.index();
    let entry = |relative: &str| index.entry(index.find(&root.join(relative)).expect(relative)).unwrap();
    assert!((entry("Drums/Kicks/Kick 1.wav").seconds().unwrap() - 0.75).abs() < 1.0e-6);
    assert_eq!(entry("Drums/Kicks/Kick 2.wav").modified, Some(later));
    assert!(index.find(&root.join("Drums/Kicks/Kick 10.wav")).is_none());
    assert!(!entry("Broken/not audio.wav").unreadable);
    assert!(index.find_folder(&root.join("Loops")).is_none());
    assert_eq!(entry("Phrases/Melody Loop 120bpm Am.wav").tempo, Some(120.0));
    let kicks = index.folder(index.find_folder(&root.join("Drums/Kicks")).unwrap()).unwrap();
    assert_eq!(names(&index.entries()[kicks.files()]), ["Kick 1", "Kick 2", "kick 3", "Kick 11"]);

    // And once more with nothing changed.
    assert_eq!(scan(&library).headers_read, 0);
    // The saved index is the rescanned one.
    drop(library);
    let reopened = open(&[&root], Some(index_path));
    assert_eq!(reopened.index().entries(), index.entries());
}

#[test]
fn a_damaged_or_outdated_index_is_set_aside_and_rebuilt() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Pack");
    pack(&root);
    let index_path = folder.path().join("index.json");
    scan(&open(&[&root], Some(index_path.clone())));
    let good = std::fs::read(&index_path).unwrap();

    let damaged: [(&str, Vec<u8>); 4] = [
        ("damaged", b"\x00\x01 this is not an index".to_vec()),
        ("damaged", good[..good.len() * 2 / 3].to_vec()),
        ("different version", b"{\"trill-library-index\":0}\n{\"folders\":[]}".to_vec()),
        ("damaged", Vec::new()),
    ];
    for (problem, bytes) in damaged {
        std::fs::write(&index_path, &bytes).unwrap();
        let library = open(&[&root], Some(index_path.clone()));
        assert!(library.index().is_empty(), "nothing from the bad file is used");
        let said = library.index_problem().expect("the problem is reported");
        assert!(said.contains(problem), "{said}");

        // The scan rebuilds everything and the file is good again.
        let summary = scan(&library);
        assert_eq!((summary.files, summary.headers_read), (11, 8));
        assert_eq!(library.index_problem(), None);
        drop(library);
        let reopened = open(&[&root], Some(index_path.clone()));
        assert_eq!((reopened.index().len(), reopened.index_problem()), (11, None));
    }

    // An index path that cannot be written does not stop the library.
    let blocked = folder.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    let library = open(&[&root], Some(blocked));
    assert!(library.index_problem().is_some());
    assert_eq!(scan(&library).files, 11);
}

#[test]
fn folders_that_cannot_be_read_are_reported_not_fatal() {
    let folder = tempfile::tempdir().unwrap();
    let good = folder.path().join("Good");
    write(&good.join("Kick.wav"), &wav(0.2, 8_000, 1));
    let missing = folder.path().join("Never There");
    let a_file = folder.path().join("a file.wav");
    write(&a_file, &wav(0.2, 8_000, 1));
    let index_path = folder.path().join("index.json");

    let library = open(&[&missing, &good, &a_file], Some(index_path.clone()));
    let summary = scan(&library);
    assert_eq!(summary.files, 1);
    assert_eq!(summary.problems.len(), 2, "{:?}", summary.problems);
    assert!(summary.problems[0].contains("Never There") && summary.problems[0].starts_with("cannot read"));
    assert!(summary.problems[1].contains("a file.wav"));
    assert_eq!(library.index().roots().len(), 1);

    // A root that was scanned and then cannot be reached (a drive that is
    // unplugged) keeps what was known about it, so that plugging it back
    // in does not mean reading every file again.
    let away = folder.path().join("Good (unplugged)");
    std::fs::rename(&good, &away).unwrap();
    let summary = scan(&library);
    assert_eq!((summary.files, summary.headers_read), (1, 0));
    assert!(
        summary.problems.iter().any(|problem| problem.contains("keeping what was known")),
        "{:?}",
        summary.problems
    );
    assert_eq!(found(&library, "kick"), ["Kick"]);
    std::fs::rename(&away, &good).unwrap();
    let summary = scan(&library);
    assert_eq!((summary.files, summary.headers_read), (1, 0), "nothing is read again when it returns");

    // And it survives a restart while away.
    std::fs::rename(&good, &away).unwrap();
    drop(library);
    let library = open(&[&missing, &good], Some(index_path));
    assert_eq!(scan(&library).files, 1);
    assert_eq!(library.index().len(), 1);
}

#[test]
fn odd_file_names_are_listed_and_searchable() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Odd ✨ Names");
    let short = wav(0.2, 8_000, 1);
    let long_name = format!("{} kick.wav", "very long name ".repeat(12));
    let odd = [
        "ünïcode — snare (Größe) [№ 5].wav",
        "ドラム キック.wav",
        "name with 'quotes' & {braces}, commas; semicolons.wav",
        "   leading spaces.wav",
        "UPPER.WAV",
        "two.dots..wav",
        "100% #1 kick!.wav",
        long_name.as_str(),
    ];
    for name in odd {
        write(&root.join("Sub folder (1) & more").join(name), &short);
    }
    let index_path = folder.path().join("index.json");
    let library = open(&[&root], Some(index_path.clone()));
    let summary = scan(&library);
    assert_eq!((summary.files, summary.headers_read, summary.unreadable), (odd.len(), odd.len(), 0));
    assert!(summary.problems.is_empty(), "{:?}", summary.problems);

    assert_eq!(found(&library, "größe"), ["ünïcode — snare (Größe) [№ 5]"]);
    assert_eq!(found(&library, "GRÖSSE").len(), 0, "folding is by letter, not by spelling rules");
    assert_eq!(found(&library, "キック"), ["ドラム キック"]);
    assert_eq!(found(&library, "100%"), ["100% #1 kick!"]);
    assert_eq!(found(&library, "✨ upper"), ["UPPER"]);
    assert_eq!(found(&library, "kick").len(), 2);

    // All of it survives being saved and loaded.
    let scanned = library.index().entries().to_vec();
    drop(library);
    let reopened = open(&[&root], Some(index_path));
    assert_eq!(reopened.index().entries(), scanned.as_slice());
    assert_eq!(scan(&reopened).headers_read, 0);
}

/// Names that are not valid Unicode cannot be written into the saved index,
/// but must not upset anything: they are listed while the program runs and
/// found again by the next scan.
#[cfg(any(unix, windows))]
#[test]
fn names_that_are_not_unicode_are_listed_but_not_saved() {
    #[cfg(unix)]
    let bad_name: std::ffi::OsString = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(b"bad \xff\xfe kick.wav".to_vec())
    };
    #[cfg(windows)]
    let bad_name: std::ffi::OsString = {
        use std::os::windows::ffi::OsStringExt;
        // An unpaired surrogate: valid as a Windows file name, not as text.
        let mut units: Vec<u16> = "bad ".encode_utf16().collect();
        units.push(0xD800);
        units.extend(" kick.wav".encode_utf16());
        std::ffi::OsString::from_wide(&units)
    };

    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Pack");
    write(&root.join("good kick.wav"), &wav(0.2, 8_000, 1));
    let bad_path = root.join(&bad_name);
    if std::fs::write(&bad_path, wav(0.2, 8_000, 1)).is_err() {
        println!("SKIPPED: this file system refuses names that are not Unicode");
        return;
    }
    let index_path = folder.path().join("index.json");
    let library = open(&[&root], Some(index_path.clone()));
    let summary = scan(&library);
    assert_eq!((summary.files, summary.headers_read, summary.unreadable), (2, 2, 0));
    let index = library.index();
    let bad = index.entry(index.find(&bad_path).expect("found by its exact path")).unwrap();
    assert!(bad.name.contains('\u{fffd}') && bad.name.ends_with(" kick"), "{}", bad.name);
    assert!(bad.audio.is_some());
    assert_eq!(found(&library, "kick").len(), 2);

    drop(library);
    let reopened = open(&[&root], Some(index_path));
    assert_eq!(reopened.index().len(), 1, "only the name that can be written was saved");
    let summary = scan(&reopened);
    assert_eq!((summary.files, summary.headers_read), (2, 1), "the other is found again");
}

#[test]
fn roots_can_be_added_and_removed_while_running() {
    let folder = tempfile::tempdir().unwrap();
    let (first, second) = (folder.path().join("First"), folder.path().join("Second"));
    write(&first.join("Drums/Kick.wav"), &wav(0.2, 8_000, 1));
    write(&second.join("Snare.wav"), &wav(0.2, 8_000, 1));
    let index_path = folder.path().join("index.json");
    let library = open(&[], Some(index_path.clone()));
    assert!(library.roots().is_empty());

    // Adding a root starts scanning it without being asked.
    assert!(library.add_root(&first));
    assert!(library.wait_until_idle(WAIT));
    assert_eq!(found(&library, ""), ["Kick"]);
    assert!(!library.add_root(&first), "already a root");
    assert!(!library.add_root(&first.join("Drums")), "inside a root");
    assert!(library.add_root(&second));
    assert!(library.wait_until_idle(WAIT));
    assert_eq!(library.roots(), [first.clone(), second.clone()]);
    assert_eq!(found(&library, ""), ["Kick", "Snare"], "roots are listed in the order they were added");
    assert_eq!(library.scan_status().last.unwrap().headers_read, 1, "the first root was not read again");

    // Removing one takes its files out at once, without a scan.
    let generation = library.generation();
    assert!(library.remove_root(&first));
    assert!(library.generation() > generation);
    assert_eq!(found(&library, ""), ["Snare"]);
    assert!(!library.remove_root(&first), "no longer a root");
    assert_eq!(library.settings().roots, std::slice::from_ref(&second));

    // A folder holding existing roots replaces them.
    assert!(library.add_root(folder.path()));
    assert!(library.wait_until_idle(WAIT));
    assert_eq!(library.roots(), [folder.path().to_path_buf()]);
    assert_eq!(library.index().count(EntryKind::Audio), 2);

    drop(library);
    let reopened = open(&[folder.path()], Some(index_path));
    assert_eq!(reopened.index().len(), 2);
}

#[test]
fn favorites_and_recently_used_files_persist_through_settings() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Pack");
    pack(&root);
    let library = open(&[&root], None);
    scan(&library);
    let kick = root.join("Drums/Kicks/Kick 2.wav");
    let pad = root.join("Loops/Warm Pad 90 F#m.wav");
    let gone = root.join("Deleted Long Ago.wav");

    let generation = library.generation();
    library.set_favorite(&pad, true);
    library.set_favorite(&kick, true);
    library.set_favorite(&kick, true);
    library.set_favorite(&gone, true);
    assert!(library.generation() > generation);
    assert!(library.is_favorite(&kick) && !library.is_favorite(&root.join("Drums/Kicks/Kick 1.wav")));
    assert_eq!(
        library.favorites(),
        [pad.clone(), kick.clone(), gone.clone()],
        "in the order they were marked, once each"
    );

    let favorites = |text: &str| {
        let query = Query { text: text.into(), favorites_only: true, ..Query::default() };
        library.search(&query).entries().map(|entry| entry.name.clone()).collect::<Vec<_>>()
    };
    assert_eq!(favorites(""), ["Kick 2", "Warm Pad 90 F#m"], "a favorite whose file is gone matches nothing");
    assert_eq!(favorites("pad"), ["Warm Pad 90 F#m"]);
    library.set_favorite(&pad, false);
    assert_eq!(favorites(""), ["Kick 2"]);

    for path in [&kick, &pad, &kick, &kick] {
        library.mark_used(path);
    }
    assert_eq!(library.recent(), [kick.clone(), pad.clone()], "latest first, once each");
    for number in 0..80 {
        library.mark_used(&root.join(format!("file {number}.wav")));
    }
    assert_eq!(library.recent().len(), 50, "the list is bounded");
    assert_eq!(library.recent()[0], root.join("file 79.wav"));

    // Through the settings file and back into a new library.
    let settings_path = folder.path().join("config/settings.json");
    let settings = Settings { library: library.settings(), ..Settings::default() };
    settings.save(&settings_path).unwrap();
    let loaded = Settings::load(&settings_path);
    assert_eq!(loaded.warning, None);
    let reopened = Library::open(&loaded.settings.library, None);
    assert_eq!(reopened.roots(), [root]);
    assert_eq!(reopened.favorites(), [kick.clone(), gone]);
    assert!(reopened.is_favorite(&kick));
    assert_eq!(reopened.recent(), library.recent());
}

#[test]
fn a_scan_never_blocks_and_can_be_cancelled_or_abandoned() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Big");
    let short = wav(0.1, 8_000, 1);
    for pack in 0..20 {
        for file in 0..100 {
            write(&root.join(format!("Pack {pack}/Kicks/Kick {file}.wav")), &short);
        }
    }
    let index_path = folder.path().join("index.json");
    let library = open(&[&root], Some(index_path.clone()));

    // Starting a scan returns before the scan could have finished.
    let started = std::time::Instant::now();
    library.rescan();
    let returned_after = started.elapsed();
    assert!(returned_after < Duration::from_millis(50), "rescan took {returned_after:?} to return");
    // The library stays usable while it runs.
    let _ = library.search(&Query::text("kick"));
    let status = library.scan_status();
    assert!(status.headers_read <= status.headers_to_read.max(2_000));

    // Cancelling ends it promptly. Whether it had finished already is a
    // race this test does not try to win; either way the library is whole.
    library.cancel_scan();
    assert!(library.wait_until_idle(WAIT));
    let summary = library.scan_status().last.unwrap();
    let index = library.index();
    if summary.cancelled {
        assert!(index.len() == 2_000 || index.is_empty(), "a cancelled scan keeps a whole listing or nothing");
        assert!(summary.headers_read <= 2_000);
    } else {
        assert_eq!((index.len(), summary.headers_read), (2_000, 2_000));
    }

    // The next scan finishes the job, reading only what is still unread.
    let rest = scan(&library);
    assert!(!rest.cancelled);
    assert_eq!(rest.files, 2_000);
    assert_eq!(rest.headers_read + if index.is_empty() { 0 } else { summary.headers_read }, 2_000);
    assert!(library.index().entries().iter().all(|entry| entry.audio.is_some()));

    // Scans started on top of one another: only the last one counts.
    for _ in 0..5 {
        library.rescan();
    }
    assert!(library.wait_until_idle(WAIT));
    assert_eq!(library.index().len(), 2_000);
    assert_eq!(library.scan_status().last.unwrap().headers_read, 0);

    // Dropping the library in the middle of a scan neither hangs nor
    // leaves a damaged index behind.
    std::fs::remove_file(&index_path).unwrap();
    let abandoned = open(&[&root], Some(index_path.clone()));
    abandoned.rescan();
    drop(abandoned);
    let reopened = open(&[&root], Some(index_path));
    assert_eq!(reopened.index_problem(), None);
    assert_eq!(scan(&reopened).files, 2_000);
}

#[test]
fn listings_follow_natural_order_everywhere() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("Pack");
    let short = wav(0.1, 8_000, 1);
    for name in ["b 10", "B 9", "a", "A 2", "a 10", "_x", "10", "9"] {
        write(&root.join(name).join("x.wav"), &short);
        write(&root.join("files").join(format!("{name}.wav")), &short);
    }
    let library = open(&[&root], None);
    scan(&library);
    let index = library.index();
    let top = index.folder(index.roots()[0]).unwrap();
    // Numbers by value, then punctuation, then letters without regard to case.
    let mut folders: Vec<&str> = top.subfolders().iter().map(|id| index.folder(*id).unwrap().name()).collect();
    assert_eq!(folders, ["9", "10", "_x", "a", "A 2", "a 10", "B 9", "b 10", "files"]);
    let files = index.folder(index.find_folder(&root.join("files")).unwrap()).unwrap();
    let listed = names(&index.entries()[files.files()]);
    assert_eq!(listed, ["9", "10", "_x", "a", "A 2", "a 10", "B 9", "b 10"]);
    // The same order `natural_cmp` gives a caller sorting its own list.
    folders.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(folders, ["9", "10", "_x", "a", "A 2", "a 10", "B 9", "b 10", "files"]);
    let modified: Option<SystemTime> = index.entries()[0].modified;
    assert!(modified.is_some_and(|time| time <= SystemTime::now() + Duration::from_secs(60)));
}

#[test]
fn a_folder_of_the_library_that_moved_is_said_to_be_missing_and_takes_what_is_kept_along() {
    let folder = tempfile::tempdir().unwrap();
    let (old, new) = (folder.path().join("Packs"), folder.path().join("Samples").join("Packs"));
    let other = folder.path().join("Other");
    pack(&old);
    write(&other.join("Clap.wav"), &wav(0.2, 8_000, 1));
    let index_path = folder.path().join("index.json");
    let library = open(&[&old, &other], Some(index_path.clone()));
    let first = scan(&library);
    assert!(first.missing.is_empty() && library.missing_roots().is_empty());
    let kick = old.join("Drums/Kicks/Kick 1.wav");
    let pad = old.join("Loops/Warm Pad 90 F#m.wav");
    library.set_favorite(&kick, true);
    library.set_favorite(&other.join("Clap.wav"), true);
    library.mark_used(&pad);
    library.mark_used(&other.join("Clap.wav"));
    library.mark_used(&kick);

    // The folder goes elsewhere while nobody is looking.
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::rename(&old, &new).unwrap();
    let summary = scan(&library);
    let missing = library.missing_roots();
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!((missing[0].path.as_path(), missing[0].files), (old.as_path(), first.files - 1));
    assert!(!missing[0].reason.is_empty());
    assert_eq!(summary.missing, missing);
    assert_eq!(summary.files, first.files, "what was known of it is still listed");

    // Saying where it went cannot be got wrong quietly.
    let error = library.move_root(&folder.path().join("Never A Root"), &new).unwrap_err();
    assert!(error.contains("is not a folder of the library") && error.contains("Other"), "{error}");
    let error = library.move_root(&old, &folder.path().join("Nowhere")).unwrap_err();
    assert!(error.contains("cannot read") && error.contains("the library is as it was"), "{error}");
    assert_eq!(library.roots(), [old.clone(), other.clone()]);
    assert_eq!(library.missing_roots().len(), 1);

    // Said: the library covers the new place, in the old one's position.
    let moved = library.move_root(&old, &new).unwrap();
    assert_eq!((moved.files, moved.favorites, moved.recent), (first.files - 1, 1, 2));
    assert_eq!(library.roots(), [new.clone(), other.clone()]);
    assert!(library.wait_until_idle(WAIT));
    let after = library.scan_status().last.expect("moving a folder scans it");
    assert_eq!((after.files, after.headers_read), (first.files, 0), "no file is opened again: {after:?}");
    assert!(library.missing_roots().is_empty() && after.missing.is_empty());

    // Favorites and recently used files are the same files, where they are.
    let new_kick = new.join("Drums/Kicks/Kick 1.wav");
    assert_eq!(library.favorites(), [new_kick.clone(), other.join("Clap.wav")]);
    assert!(library.is_favorite(&new_kick) && !library.is_favorite(&kick));
    assert_eq!(library.recent(), [new_kick.clone(), other.join("Clap.wav"), new.join("Loops/Warm Pad 90 F#m.wav")]);
    let index = library.index();
    assert!(index.find(&new_kick).is_some() && index.find(&kick).is_none());
    let entry = index.entry(index.find(&new_kick).unwrap()).unwrap();
    assert_eq!(entry.audio.map(|audio| audio.sample_rate), Some(8_000));
    let favorites = library.search(&Query { favorites_only: true, ..Query::default() });
    assert_eq!(favorites.len(), 2);

    // And all of it survives a restart, through the settings.
    let settings = library.settings();
    drop(library);
    let library = Library::open(&settings, Some(index_path));
    assert_eq!(library.index().len(), first.files, "the saved index is the moved one");
    assert_eq!(scan(&library).headers_read, 0);
    assert!(library.is_favorite(&new_kick));

    // A folder said to be where another folder of the library already is
    // simply joins it.
    let moved = library.move_root(&other, &new).unwrap();
    assert_eq!(moved.favorites, 1);
    assert_eq!(library.roots(), [new.clone()]);
    assert!(library.wait_until_idle(WAIT));
    assert_eq!(library.index().roots().len(), 1);
}
