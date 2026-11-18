//! A set of sampled instruments on Trill's side of a fetch: handed its
//! files, it is checked against what the catalogue read, brought into the
//! folder the user chose, made known to the library, played, and what a
//! song owes for it is said.
//!
//! The catalogue here is one the test writes, of an instrument the test
//! makes (an SFZ file and two tones): nothing is fetched and nothing of
//! anyone's is read. `the_catalogue_s_own_sets_arrive_as_they_were_read`
//! is the same on real sets, fetched from where they are published: it
//! runs only when asked.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use trill_catalogue::check::{file_sum, tree_sum};
use trill_catalogue::{Catalogue, Fetch, Set};
use trill_core::ChainRef;
use trill_core::audio::AudioBuffer;
use trill_engine::render::RenderOptions;
use trill_library::{EntryKind, Query};
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Runtime, RuntimeOptions};

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
    runtime.set_removed();
    assert!(runtime.library.wait_until_idle(Duration::from_secs(30)));
    assert!(runtime.library.search(&Query { kind: Some(EntryKind::Instrument), ..Query::default() }).is_empty());
    assert!(runtime.here_of(&catalogue).is_empty());
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

/// A program on this machine that unpacks a zip archive into a folder.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|error| error.to_string())?;
    // Windows comes with a tar that reads zip archives; elsewhere unzip does.
    let windows_tar =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_default()).join("System32").join("tar.exe");
    let done = if windows_tar.is_file() {
        std::process::Command::new(windows_tar).arg("-xf").arg(archive).arg("-C").arg(into).status()
    } else {
        std::process::Command::new("unzip").arg("-q").arg(archive).arg("-d").arg(into).status()
    };
    match done {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("unpacking {} ended with {status}", archive.display())),
        Err(error) => Err(format!("no program to unpack a zip archive was found: {error}")),
    }
}

fn fetch(url: &str, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let done = std::process::Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error", "--retry", "2", "--output"])
        .arg(to)
        .arg(url)
        .status();
    match done {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("curl could not fetch {url}: {status}")),
        Err(error) => Err(format!("curl was not found: {error}")),
    }
}

/// The real thing, on sets of the program's own catalogue: fetched from
/// where they are published with the system's curl (an archive unpacked
/// with the system's own tool, or file by file), checked against what was
/// read, brought into a folder, found by the library and played. By
/// default a few small sets of different makers and both ways of
/// fetching; `TRILL_CATALOGUE_SETS` names others by their ids, with
/// commas between. Everything goes into a temporary folder.
#[test]
#[ignore = "fetches sets of sampled instruments from where they are published (megabytes): \
            cargo test -p trill-runtime --test instruments -- --ignored --nocapture"]
fn the_catalogue_s_own_sets_arrive_as_they_were_read() {
    let catalogue = Catalogue::shipped();
    let asked = std::env::var("TRILL_CATALOGUE_SETS")
        .unwrap_or_else(|_| "synth-bass-1,ixox-flute,jsteeldrum,electric-bass-yr,vcsl-didgeridoo".to_string());
    let folder = tempfile::tempdir().unwrap();
    let kept = folder.path().join("Trill Instruments");
    let runtime = started();
    for id in asked.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        let set: &Set = catalogue.set(id).unwrap_or_else(|| panic!("there is no set '{id}' in the catalogue"));
        let began = std::time::Instant::now();
        let handed = match &set.fetch {
            Fetch::Archive { url, under, .. } => {
                let archive = folder.path().join(format!("{id}.zip"));
                fetch(url, &archive).unwrap();
                let unpacked = folder.path().join(format!("{id}-unpacked"));
                unpack(&archive, &unpacked).unwrap();
                unpacked.join(under.trim_end_matches('/'))
            }
            Fetch::Files { base, files } => {
                let handed = folder.path().join(format!("{id}-files"));
                for file in files {
                    let to = trill_catalogue::check::below(&handed, &file.path);
                    fetch(&trill_catalogue::check::address(base, &file.path), &to).unwrap();
                }
                handed
            }
        };
        let fetched = began.elapsed();
        let arrived = runtime.take_set(set, &handed, &kept, &AtomicBool::new(false)).unwrap();
        assert_eq!((arrived.files, arrived.bytes), (set.files, set.bytes), "{id}: all of it, as it was read");
        assert!(runtime.library.wait_until_idle(Duration::from_secs(120)));
        let here = runtime.here_of(catalogue);
        let mut heard = Vec::new();
        for instrument in &set.instruments {
            let at =
                here.get(&instrument.text).unwrap_or_else(|| panic!("{id}: {} is not in the library", instrument.file));
            // One that came out of the player silent when it was measured is not asked to sound now.
            if instrument
                .marks
                .iter()
                .any(|mark| matches!(mark, trill_catalogue::Mark::Silent | trill_catalogue::Mark::Incomplete))
            {
                continue;
            }
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
            let peak = rendered_peak(&runtime);
            heard.push(format!("{} {:.0} dB", instrument.name, 20.0 * peak.max(1e-9).log10()));
            assert!(peak > 0.001, "{id}: {} is silent on key {key}", instrument.name);
            runtime.session.lock().edit(|project| project.remove_track(track).map(drop)).unwrap();
        }
        println!(
            "{id}: {} files, {:.1} MB, fetched in {:.1} s, whole and as it was read; {}",
            arrived.files,
            arrived.bytes as f64 / (1024.0 * 1024.0),
            fetched.as_secs_f64(),
            heard.join("; ")
        );
    }
}
