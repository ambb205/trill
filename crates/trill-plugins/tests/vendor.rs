//! The presets vendors keep in files of their own: the scanner process
//! that reads them, on a file made here, and the installed plugins they
//! belong to.
//!
//! The first test needs nothing installed: it packs a preset in Xfer's
//! container itself and has the scanner executable list it and build its
//! state. The rest use the plugins and the vendors' preset folders of
//! this machine and are skipped unless asked for; one whose plugin is not
//! installed prints a note and passes. They print what they find, since
//! what is heard is the point:
//!
//! ```text
//! cargo test -p trill-plugins --test vendor -- --include-ignored --nocapture --test-threads 1
//! ```
//!
//! Vendors' folders are only read. The caches are the test's own, under
//! the target folder, and no window is opened.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_core::presets::{PresetList, PresetStatus};
use trill_core::{ChainRef, DeviceId, Note, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::probe::{Heard, ProbeNote, ProbeOptions};
use trill_engine::session::ProbeTarget;
use trill_engine::{AudioBackend, RenderOptions, Session};
use trill_plugins::{Scanner, Vst3Config, Vst3Factory};

const SERUM_2: &str = "vst3:56534558667350736572756D20320000";
const SUPERIOR: &str = "vst3:565354534433617375706572696F7220";
/// The Ample instruments, with a preset of each that is not its default.
const AMPLE: [(&str, &str, &str); 4] = [
    ("Ample Guitar PF", "vst3:ABCDEF019182FAEB4170736441677066", "Clean Shimmer"),
    ("Ample Metal Eclipse", "vst3:ABCDEF019182FAEB41707364416D6574", "Clean Tapping"),
    ("Ample Metal Hellrazer", "vst3:ABCDEF019182FAEB41707364416D6872", "9 String Metal"),
    ("Ample Metal R", "vst3:ABCDEF019182FAEB41707364416D7274", "Adidas"),
];
/// What the class ids of Neural DSP's plugins begin with.
const NEURAL: &str = "vst3:ABCDEF019182FAEB4E445350";

const PATIENCE: Duration = Duration::from_secs(120);

fn scanner_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trill-plugin-scan"))
}

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("vendor").join(name)
}

// ---- the scanner process, on a file made here --------------------------------

/// A CBOR text string.
fn text(value: &str) -> Vec<u8> {
    assert!(value.len() < 24);
    let mut out = vec![0x60 | value.len() as u8];
    out.extend(value.as_bytes());
    out
}

/// One of Xfer's containers: a JSON header, then zstd-compressed CBOR.
fn container(header: serde_json::Value, body: &[u8]) -> Vec<u8> {
    let compressed = ruzstd::encoding::compress_to_vec(body, ruzstd::encoding::CompressionLevel::Fastest);
    let header = header.to_string().into_bytes();
    let mut out = b"XferJson\0".to_vec();
    out.extend((header.len() as u64).to_le_bytes());
    out.extend(header);
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(2u32.to_le_bytes());
    out.extend(compressed);
    out
}

/// A preset of three settings, as Serum 2 writes its preset files.
fn serum_preset(name: &str) -> Vec<u8> {
    let mut body = vec![0xa4];
    for (key, value) in [("fileType", text("SerumPreset")), ("mpeEnabled", vec![0xf5]), ("presetName", text(name))] {
        body.extend(text(key));
        body.extend(value);
    }
    body.extend(text("Oscillator0"));
    body.extend([0x58, 0x20]);
    body.extend([7u8; 32]);
    let header = serde_json::json!({
        "fileType": "SerumPreset", "presetAuthor": "Somebody", "presetDescription": "made in a test",
        "presetName": name, "product": "Serum2", "productVersion": "2.0.12", "tags": ["Pad"], "version": 5.0,
    });
    container(header, &body)
}

/// A VST3 state blob of a plugin of `class` that has no state.
fn empty_state(class: &str) -> Vec<u8> {
    let mut blob = b"VST3".to_vec();
    blob.extend(1i32.to_le_bytes());
    blob.extend(class.as_bytes());
    blob.extend(48i64.to_le_bytes());
    blob.extend(b"List");
    blob.extend(0i32.to_le_bytes());
    blob
}

/// The chunks of a VST3 state blob, by their four-letter ids.
fn chunks(blob: &[u8]) -> Vec<(String, Vec<u8>)> {
    assert_eq!(&blob[..4], b"VST3");
    let list = i64::from_le_bytes(blob[40..48].try_into().unwrap()) as usize;
    assert_eq!(&blob[list..list + 4], b"List");
    let count = i32::from_le_bytes(blob[list + 4..list + 8].try_into().unwrap()) as usize;
    (0..count)
        .map(|index| {
            let entry = &blob[list + 8 + index * 20..list + 28 + index * 20];
            let offset = i64::from_le_bytes(entry[4..12].try_into().unwrap()) as usize;
            let size = i64::from_le_bytes(entry[12..20].try_into().unwrap()) as usize;
            (String::from_utf8_lossy(&entry[..4]).into_owned(), blob[offset..offset + size].to_vec())
        })
        .collect()
}

/// What a container holds: its header and its body, unpacked.
fn opened(container: &[u8]) -> (serde_json::Value, Vec<u8>) {
    use std::io::Read;
    assert!(container.starts_with(b"XferJson\0"));
    let length = u64::from_le_bytes(container[9..17].try_into().unwrap()) as usize;
    let header = serde_json::from_slice(&container[17..17 + length]).unwrap();
    let mut body = Vec::new();
    ruzstd::decoding::StreamingDecoder::new(&container[17 + length + 8..]).unwrap().read_to_end(&mut body).unwrap();
    (header, body)
}

#[test]
fn the_scanner_process_lists_a_vendors_presets_and_builds_the_state_of_one() {
    let folder = scratch("scanner");
    let _ = std::fs::remove_dir_all(&folder);
    let presets =
        folder.join("documents").join("Xfer").join("Serum 2 Presets").join("Presets").join("Factory").join("Pad");
    std::fs::create_dir_all(&presets).unwrap();
    let file = presets.join("PD - Made Here.SerumPreset");
    std::fs::write(&file, serum_preset("PD - Made Here")).unwrap();
    let class = SERUM_2.strip_prefix("vst3:").unwrap();
    let run = |arguments: &[&str]| {
        let output = Command::new(scanner_path()).args(arguments).output().expect("the scanner runs");
        (output.status.code(), String::from_utf8_lossy(&output.stdout).into_owned())
    };

    // Listed before the plugin is so much as looked for: there is no such
    // plugin file, which is how the listing ends, and the preset is there.
    let about = serde_json::json!({ "name": "Serum 2", "vendor": "Xfer Records", "places": { "documents": [folder.join("documents")], "shared_data": [], "programs": [] } });
    let (code, printed) = run(&["--presets", "vst3", "no-such-plugin.vst3", class, &about.to_string()]);
    let lines: Vec<serde_json::Value> = printed.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    let listed: Vec<&serde_json::Value> =
        lines.iter().filter_map(|line| line["presets"].as_array()).flatten().collect();
    assert_eq!(listed.len(), 1, "{printed}");
    assert_eq!(
        (listed[0]["name"].as_str(), listed[0]["category"].as_str()),
        (Some("PD - Made Here"), Some("Factory/Pad"))
    );
    assert_eq!((listed[0]["author"].as_str(), listed[0]["tags"][0].as_str()), (Some("Somebody"), Some("pad")));
    assert!(listed[0]["locator"].as_str().unwrap().starts_with("vendor-serum2\n"), "{printed}");
    assert!(
        lines.iter().any(|line| line["watch"][0]["path"].as_str().is_some_and(|path| path.ends_with("Presets"))),
        "{printed}"
    );
    assert_eq!(code, Some(1), "the plugin could not be loaded: {printed}");
    assert!(lines.last().unwrap()["failed"].is_string(), "{printed}");
    // Told of no places, it looks nowhere.
    let (_, printed) = run(&["--presets", "vst3", "no-such-plugin.vst3", class]);
    assert!(!printed.contains("PD - Made Here"), "{printed}");

    // The state of the preset: both halves of the plugin get the patch,
    // each told which half it is.
    let (now, then) = (folder.join("now"), folder.join("then"));
    std::fs::write(&now, empty_state(class)).unwrap();
    let paths = |file: &Path| [file, &now, &then].map(|path| path.to_string_lossy().into_owned());
    let [preset, now_path, then_path] = paths(&file);
    let (code, printed) = run(&["--vendor-preset", "serum2", &preset, "", &now_path, &then_path]);
    assert_eq!(code, Some(0), "{printed}");
    let outcome: serde_json::Value = serde_json::from_str(printed.lines().last().unwrap()).unwrap();
    assert_eq!(outcome["built"], true);
    assert_eq!(outcome["marks"][0], serde_json::json!({ "name": "presetName", "value": "PD - Made Here" }));
    let built = chunks(&std::fs::read(&then).unwrap());
    assert_eq!(built.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["Comp", "Cont"]);
    for ((_, half), component) in built.iter().zip(["processor", "controller"]) {
        let (header, body) = opened(half);
        assert_eq!((header["component"].as_str(), header["product"].as_str()), (Some(component), Some("Serum2")));
        assert_eq!(header["hash"].as_str().map(str::len), Some(32));
        let has = |needle: &[u8]| body.windows(needle.len()).any(|window| window == needle);
        assert!(has(&[text("component"), text(component)].concat()), "the settings say which half they are for");
        assert!(has(&[7u8; 32]) && !has(b"fileType"), "the patch, without what the file says of itself");
    }

    // A file that is no preset, one that claims to unpack to four
    // gigabytes, and a format there is none of: a sentence each, and no
    // state.
    std::fs::remove_file(&then).unwrap();
    let garbage = folder.join("Garbage.SerumPreset");
    std::fs::write(&garbage, b"not a preset at all").unwrap();
    let mut lying = serum_preset("Liar");
    let size_at = 17 + u64::from_le_bytes(lying[9..17].try_into().unwrap()) as usize;
    lying[size_at..size_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let huge = folder.join("Huge.SerumPreset");
    std::fs::write(&huge, lying).unwrap();
    for (file, format, reason) in [
        (&garbage, "serum2", "cannot be loaded: it is not one of Xfer's files"),
        (&huge, "serum2", "cannot be loaded: its settings are too large"),
        (&folder.join("missing.SerumPreset"), "serum2", "cannot be read"),
        (&file, "lv2", "'lv2' is not a preset format"),
    ] {
        let started = Instant::now();
        let [preset, now_path, then_path] = paths(file);
        let (code, printed) = run(&["--vendor-preset", format, &preset, "", &now_path, &then_path]);
        let outcome: serde_json::Value = serde_json::from_str(printed.lines().last().unwrap_or_default()).unwrap();
        assert_eq!(code, Some(1), "{printed}");
        assert!(outcome["failed"].as_str().unwrap().contains(reason), "{printed}");
        assert!(!then.exists() && started.elapsed() < Duration::from_secs(20));
    }
    assert_eq!(run(&["--vendor-preset", "serum2"]).0, Some(2), "too few arguments");

    // What one file holds, wherever it lies: for a preset that is asked
    // for by its path. The same file, bought and kept with the samples.
    let bought = folder.join("samples").join("Pack").join("Made Here.SerumPreset");
    std::fs::create_dir_all(bought.parent().unwrap()).unwrap();
    std::fs::copy(&file, &bought).unwrap();
    let (code, printed) = run(&["--vendor-file", "serum2", &bought.to_string_lossy()]);
    let lines: Vec<serde_json::Value> = printed.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    assert_eq!((code, lines.len()), (Some(0), 2), "{printed}");
    let held = lines[0]["presets"].as_array().unwrap();
    assert_eq!(held.len(), 1, "{printed}");
    assert_eq!((held[0]["name"].as_str(), held[0]["author"].as_str()), (Some("PD - Made Here"), Some("Somebody")));
    assert!(held[0].get("category").is_none_or(|category| category == ""), "its folders say nothing of it: {printed}");
    let locator = held[0]["locator"].as_str().unwrap();
    assert!(
        locator.starts_with(
            "vendor-serum2
"
        ) && locator.contains("Made Here.SerumPreset"),
        "{printed}"
    );
    assert_eq!(lines[1]["done"], true);
    for (file, format, reason) in [
        (&garbage, "serum2", "it is not one of Xfer's files"),
        (&garbage, "ample", "it is not one of Ample Sound's documents of settings"),
        (&garbage, "superior3", "it holds no presets of Superior Drummer's"),
        (&garbage, "archetype", "it is not a preset of one of Neural DSP's Archetypes"),
        (&folder.join("missing.SerumPreset"), "serum2", "it cannot be read"),
        (&file, "lv2", "'lv2' is not a preset format"),
    ] {
        let (code, printed) = run(&["--vendor-file", format, &file.to_string_lossy()]);
        let said: serde_json::Value = serde_json::from_str(printed.lines().last().unwrap_or_default()).unwrap();
        assert_eq!(code, Some(1), "{printed}");
        assert!(said["failed"].as_str().unwrap().starts_with(reason), "{printed}");
    }
    assert_eq!(run(&["--vendor-file", "serum2"]).0, Some(2), "too few arguments");
}

// ---- the installed plugins ------------------------------------------------------

/// A VST3 factory over the system's plugins and the vendors' own preset
/// folders. Preset files of the format's own are looked for in a folder
/// of the test's.
fn vst3() -> Vst3Factory {
    let config = Vst3Config {
        scanner: Scanner::Executable(scanner_path()),
        cache_path: Some(scratch("vst3").join("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        preset_paths: vec![scratch("vst3 presets")],
        ..Vst3Config::default()
    };
    Vst3Factory::new(config).expect("the plugin main thread starts")
}

fn installed(factory: &dyn DeviceFactory, uri: &str) -> bool {
    let found = factory.descriptors().iter().any(|descriptor| descriptor.uri == uri);
    if !found {
        eprintln!("skipping '{uri}': the plugin is not installed");
    }
    found
}

fn listed(factory: &dyn DeviceFactory, uri: &str) -> PresetList {
    let started = Instant::now();
    let list = factory.presets_within(uri, PATIENCE).expect("a plugin of this factory");
    eprintln!("{uri}: {} presets after {:.1} s ({:?})", list.len(), started.elapsed().as_secs_f64(), list.status);
    assert_eq!(list.status, PresetStatus::Ready, "{uri}");
    list
}

fn session(factory: &Vst3Factory) -> Session {
    let devices = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(factory.clone())]);
    Session::new(Arc::new(devices), AudioBackend::Null { sample_rate: 48_000 }).unwrap()
}

/// Plays `pitches` through what `target` names, on a copy of it that is
/// waited for until it is ready, and returns what each came to.
fn probe(session: &mut Session, target: &ProbeTarget, pitches: &[u8]) -> Vec<Heard> {
    let job = session.probe_job(target).unwrap_or_else(|error| panic!("{target:?}: {error}"));
    let notes: Vec<ProbeNote> =
        pitches.iter().map(|pitch| ProbeNote { pitch: *pitch, velocity: 100, with: Vec::new() }).collect();
    let options = ProbeOptions { patience: Duration::from_secs(90), ..ProbeOptions::default() };
    let started = Instant::now();
    let probed = job.run(&notes, &options).unwrap_or_else(|error| panic!("{target:?}: {error}"));
    eprintln!(
        "  waited {:.1} s for it to be ready ({}), {:.1} s in all",
        probed.waited_seconds,
        if probed.answered { "it answered" } else { "it never answered" },
        started.elapsed().as_secs_f64()
    );
    probed.heard
}

fn said(label: &str, heard: &[Heard]) {
    for note in heard {
        eprintln!(
            "  {label:<34} key {:>3}: {}  peak {:>6.1} dB  rings {:>5.2} s  brightness {:>6.0} Hz  width {:.2}",
            note.note.pitch,
            if note.sounded { "heard " } else { "silent" },
            note.peak_db,
            note.rings_seconds,
            note.brightness_hz.unwrap_or(0.0),
            note.width
        );
    }
}

/// Whether two sounds of one key differ by more than the same sound does
/// from one playing to the next: in level, in how long it rings, or in
/// how bright it is.
fn differs(one: &Heard, other: &Heard) -> bool {
    let apart = |a: f64, b: f64, by: f64| (a - b).abs() > by * a.abs().max(b.abs()).max(1e-9);
    one.sounded != other.sounded
        || (one.peak_db - other.peak_db).abs() > 3.0
        || apart(one.rings_seconds, other.rings_seconds, 0.3)
        || apart(one.brightness_hz.unwrap_or(0.0), other.brightness_hz.unwrap_or(0.0), 0.2)
}

fn heard_to_differ(what: &str, before: &[Heard], after: &[Heard]) {
    let changed = before.iter().zip(after).filter(|(before, after)| differs(before, after)).count();
    eprintln!("  {what}: {changed} of {} keys sound different from the default", before.len());
    assert!(after.iter().any(|note| note.sounded), "{what}: nothing was heard");
    assert!(changed > 0, "{what}: it sounds like the default");
}

#[test]
#[ignore = "needs Serum 2 (VST3) and its presets; run with `cargo test -p trill-plugins --test vendor -- --include-ignored --nocapture --test-threads 1`"]
fn serum_2s_own_presets_are_listed_and_a_loaded_one_is_heard() {
    let factory = vst3();
    if !installed(&factory, SERUM_2) {
        return;
    }
    let list = listed(&factory, SERUM_2);
    assert!(list.len() > 100, "{} presets", list.len());
    let mut session = session(&factory);
    let keys = [36, 48, 60];
    let target = |preset: Option<&str>| ProbeTarget::Device { uri: SERUM_2.into(), preset: preset.map(str::to_string) };
    let default = probe(&mut session, &target(None), &keys);
    said("as it comes", &default);
    for preset in ["PD - Warm Room", "808 - Simple Electro"] {
        let heard = probe(&mut session, &target(Some(preset)), &keys);
        said(preset, &heard);
        heard_to_differ(preset, &default, &heard);
    }
}

#[test]
#[ignore = "needs the Ample Sound instruments and their presets; run with `cargo test -p trill-plugins --test vendor -- --include-ignored --nocapture --test-threads 1`"]
fn the_ample_instruments_presets_are_listed_and_a_loaded_one_is_heard() {
    let factory = vst3();
    for (name, uri, preset) in AMPLE {
        if !installed(&factory, uri) {
            continue;
        }
        eprintln!("{name}");
        let list = listed(&factory, uri);
        assert!(list.len() > 10, "{name}: {} presets", list.len());
        let mut session = session(&factory);
        let keys = [52, 59, 64];
        let target = |preset: Option<&str>| ProbeTarget::Device { uri: uri.into(), preset: preset.map(str::to_string) };
        let default = probe(&mut session, &target(None), &keys);
        said("as it comes", &default);
        let heard = probe(&mut session, &target(Some(preset)), &keys);
        said(preset, &heard);
        heard_to_differ(preset, &default, &heard);
    }
}

#[test]
#[ignore = "needs Superior Drummer 3 and its core library; run with `cargo test -p trill-plugins --test vendor -- --include-ignored --nocapture --test-threads 1`"]
fn superior_drummers_kits_are_listed_and_a_loaded_one_is_heard() {
    let factory = vst3();
    if !installed(&factory, SUPERIOR) {
        return;
    }
    let list = listed(&factory, SUPERIOR);
    assert!(list.len() > 100, "{} kits", list.len());
    let mut session = session(&factory);
    // Kick, snare, closed hat, a tom, the ride, and four of the cymbals.
    let keys = [36, 38, 42, 48, 51, 49, 57, 55, 52];
    let target =
        |preset: Option<&str>| ProbeTarget::Device { uri: SUPERIOR.into(), preset: preset.map(str::to_string) };
    // A sampler's copy reads its kit anew, one drum after another, and
    // every probe waits for it: asked once, each drum is there.
    let default = probe(&mut session, &target(None), &keys);
    said("as it comes", &default);
    assert!(default.iter().all(|note| note.sounded), "every drum of the kit it starts on is there");
    // A plain kit, and one that stacks a second drum on the kick or the
    // snare, as half of the kits do: in another host such kits were
    // thought to lose their cymbals. They do not: the cymbals of the two
    // are as loud as each other.
    let cymbals = |heard: &[Heard]| heard[5..].iter().map(|note| note.peak_db).fold(f32::MIN, f32::max);
    let mut loudest = Vec::new();
    for kit in ["Black Metal", "Hard Rock"] {
        let heard = probe(&mut session, &target(Some(kit)), &keys);
        said(kit, &heard);
        heard_to_differ(kit, &default, &heard);
        assert!(heard.iter().all(|note| note.sounded), "{kit}: every drum of the kit is there");
        loudest.push(cymbals(&heard));
    }
    eprintln!("  loudest cymbal: {:.1} dB in the plain kit, {:.1} dB in the stacked one", loudest[0], loudest[1]);
    assert!((loudest[0] - loudest[1]).abs() < 12.0, "{loudest:?}");
    // The one kit found that does come out with quiet cymbals when they
    // are hit alone ("California Punk", which it shares its mixer with,
    // was the other host's example). The plugin holds the mixer exactly
    // as the preset has it; whether its own browser makes the kit sound
    // otherwise is not known. Said, not judged.
    let outlier = probe(&mut session, &target(Some("Djent Style")), &keys);
    said("Djent Style", &outlier);
    assert!(outlier.iter().all(|note| note.sounded), "its cymbals are quiet, not missing");
    eprintln!("  loudest cymbal of 'Djent Style': {:.1} dB", cymbals(&outlier));
}

#[test]
#[ignore = "needs a Neural DSP Archetype and its presets; run with `cargo test -p trill-plugins --test vendor -- --include-ignored --nocapture --test-threads 1`"]
fn an_archetypes_presets_are_listed_and_a_loaded_one_is_heard() {
    let factory = vst3();
    let amplifiers: Vec<_> =
        factory.descriptors().into_iter().filter(|descriptor| descriptor.uri.starts_with(NEURAL)).collect();
    if amplifiers.is_empty() {
        eprintln!("skipping: no Neural DSP plugin is installed");
    }
    for amplifier in amplifiers {
        eprintln!("{}", amplifier.name);
        let list = listed(&factory, &amplifier.uri);
        assert!(list.len() > 20, "{}: {} presets", amplifier.name, list.len());
        // A synth through the amplifier: what the notes come to is what
        // the amplifier makes of them.
        let mut session = session(&factory);
        let (track, amp): (TrackId, DeviceId) = {
            let track = session.edit(|project| Ok(project.add_track("Guitar"))).unwrap();
            session.add_device(ChainRef::Track(track), "builtin:polysynth", Some("pluck"), None).unwrap();
            (track, session.add_device(ChainRef::Track(track), &amplifier.uri, None, None).unwrap())
        };
        let target = ProbeTarget::Track { track, effects: true };
        let keys = [40, 52, 64];
        let default = probe(&mut session, &target, &keys);
        said("as it comes", &default);
        // Two presets that are far apart: the first of some artist's, and
        // the last in the list.
        let artists = list.presets.iter().find(|preset| preset.category.starts_with("Artists"));
        for preset in [artists, list.presets.last()].into_iter().flatten() {
            let started = Instant::now();
            session.load_preset(amp, &preset.id).unwrap_or_else(|error| panic!("{}: {error}", preset.path()));
            eprintln!("  loaded '{}' in {:.0} ms", preset.path(), started.elapsed().as_secs_f64() * 1000.0);
            let heard = probe(&mut session, &target, &keys);
            said(&preset.name, &heard);
            heard_to_differ(&preset.path(), &default, &heard);
        }
    }
}

/// What the second of two notes sounds like when it begins while the first
/// still rings: the quarter of a second from where it starts, and how loud
/// its first 80 ms are against the first note's (in dB). `switch` is a
/// keyswitch to press just before the first note.
fn second_note(session: &mut Session, track: TrackId, low: u8, interval: u8, switch: Option<u8>) -> (Vec<f32>, f64) {
    let (first_at, second_at) = (1.0, 3.0);
    let mut notes = vec![
        Note { pitch: low, start: first_at, length: 4.0, velocity: 100 },
        Note { pitch: low + interval, start: second_at, length: 2.0, velocity: 100 },
    ];
    notes.extend(switch.map(|key| Note { pitch: key, start: first_at - 0.03, length: 0.01, velocity: 100 }));
    session
        .edit(|project| {
            let clips: Vec<_> = project.track(track)?.clips.iter().map(|clip| clip.id).collect();
            for clip in clips {
                project.remove_clip(clip)?;
            }
            let clip = project.add_midi_clip(track, "two notes", 0.0, 5.0)?;
            project.clip_mut(clip)?.add_notes(&notes)
        })
        .unwrap();
    let seconds_a_beat = 60.0 / session.project().tempo;
    let options = RenderOptions {
        end_beat: Some(5.0),
        max_tail_seconds: 0.5,
        patience: Duration::from_secs(90),
        ..RenderOptions::default()
    };
    let rendered = session.render_job().render(&options).unwrap();
    let rate = f64::from(rendered.sample_rate);
    let mono: Vec<f32> = rendered.left.iter().zip(&rendered.right).map(|(left, right)| (left + right) / 2.0).collect();
    let at = |seconds: f64| ((seconds * rate) as usize).min(mono.len());
    let peak = |from: f64| mono[at(from)..at(from + 0.08)].iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let db = |level: f32| 20.0 * f64::from(level.max(1e-9)).log10();
    let (first, second) = (first_at * seconds_a_beat, second_at * seconds_a_beat);
    assert!(db(peak(first)) > -60.0, "the first note is heard");
    (mono[at(second)..at(second + 0.25)].to_vec(), db(peak(second)) - db(peak(first)))
}

/// How alike two stretches of sound are, sample for sample: 1 for the
/// same, about 0 for two that have nothing to do with each other.
fn alike(one: &[f32], other: &[f32]) -> f64 {
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| f64::from(*a) * f64::from(*b)).sum::<f64>();
    dot(one, other) / (dot(one, one) * dot(other, other)).sqrt().max(1e-12)
}

/// What the Ample instruments' "Auto Legato" does and how it is switched
/// off, which the shipped instrument profiles tell a model: at 1 (where
/// the guitars start, and where most of the maker's presets leave it) a
/// note that begins while another rings close by is hammered on, as if
/// the hammer key had been pressed; at 0 it is picked.
#[test]
#[ignore = "needs the Ample Sound instruments and their presets; run with `cargo test -p trill-plugins --test vendor -- --include-ignored --nocapture --test-threads 1`"]
fn an_ample_instruments_automatic_legato_joins_notes_that_ring_over_each_other_until_it_is_0() {
    let factory = vst3();
    // A key of each to play, and its hammer keyswitch.
    let played = [(64, 29), (64, 29), (64, 17), (57, 29)];
    for ((name, uri, preset), (low, hammer)) in AMPLE.into_iter().zip(played) {
        if !installed(&factory, uri) {
            continue;
        }
        eprintln!("{name}");
        listed(&factory, uri);
        let mut session = session(&factory);
        let track = session.edit(|project| Ok(project.add_track("Guitar"))).unwrap();
        let device = session.add_device(ChainRef::Track(track), uri, None, None).unwrap();
        let params = session.device_params(device).unwrap();
        let index = params.iter().position(|param| param.info.name == "Auto Legato").expect("a parameter of that name");
        let id = params[index].info.id.clone();
        let shown = |session: &Session| {
            let value = session.device_params(device).unwrap()[index].value;
            session.device_param_texts(device, &[(index, value)]).unwrap().remove(0)
        };
        let set = |session: &mut Session, text: &str| {
            let value = session.device_param_value(device, index, text).unwrap().expect("a value it shows");
            session.set_device_params(device, &[(id.clone(), value)]).unwrap();
            assert_eq!(shown(session), text, "{name}");
        };
        eprintln!("  'Auto Legato' is parameter {id}, and as the instrument comes it shows {}", shown(&session));
        for interval in [2, 4] {
            set(&mut session, "0");
            let (hammered, hammered_db) = second_note(&mut session, track, low, interval, Some(hammer));
            let (again, _) = second_note(&mut session, track, low, interval, Some(hammer));
            eprintln!(
                "  a note {interval} keys up, hammered on by its keyswitch: begins {hammered_db:+.1} dB against the first                  (played twice, {:.2} alike)",
                alike(&hammered, &again)
            );
            for text in ["0", "1", "2"] {
                set(&mut session, text);
                let (heard, db) = second_note(&mut session, track, low, interval, None);
                // (The instrument has more than one recording of a
                // hammer-on, and takes them in turn.)
                let like = alike(&heard, &hammered).max(alike(&heard, &again));
                eprintln!(
                    "    at {text}, no keyswitch: begins {db:+.1} dB against the first, {like:.2} like the hammered one"
                );
                match text {
                    "0" => assert!(like < 0.65, "{name}: at 0 the note is picked, not hammered on ({like:.2})"),
                    "1" => assert!(like > 0.8, "{name}: at 1 the note is hammered on unasked ({like:.2})"),
                    _ => {}
                }
            }
        }
        // One of the maker's presets puts it back where the preset has it.
        set(&mut session, "0");
        session.apply_preset(device, preset).unwrap_or_else(|error| panic!("{name}: {error}"));
        eprintln!("  set to 0 and then given the preset '{preset}', it shows {}", shown(&session));
        assert_eq!(shown(&session), "1", "{name}: '{preset}' has it on");
        let note = session.take_preset_note(device).unwrap_or_default();
        assert!(note.contains("The preset has \"Auto Legato\" at 1"), "{name}: the reply says so: {note}");
    }
}
