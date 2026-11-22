//! The audio of a shared song crossing between two programs on this
//! machine, measured: what reading a file to say what it is costs, how
//! long a file of a size takes to arrive, how long it is from an edit in
//! one program to the other's song while the file crosses, and what the
//! process holds in memory meanwhile.
//!
//! ```sh
//! cargo run -p trill-runtime --example files_cross -- 50                       # a 50 MB file, directly
//! cargo run -p trill-runtime --example files_cross -- 50 --through-here        # through a meeting point in this process
//! cargo run -p trill-runtime --example files_cross -- 50 --through ADDRESS KEY # through one that runs somewhere
//! cargo run -p trill-runtime --example files_cross -- --count SONG.trill       # what of a song would travel
//! ```
//!
//! The file is made here, of a tone and noise, under the system's
//! temporary folder, and removed at the end; nothing of the user's is read
//! or written. Both programs are runtimes in this one process, without
//! audio devices and without plugins, connected over the loopback as two
//! programs are: the connection is the real one, encrypted, with its
//! threads, and so are the threads that read, hand out and take in.
//!
//! `--count` opens a song and says what sharing it would hand to whoever
//! joins: how many files, how many megabytes, from which folders. It looks
//! at each file (that it is there, how large it is) and sends nothing
//! anywhere.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::audio::AudioBuffer;
use trill_core::files::FilePlace;
use trill_core::project::AudioContent;
use trill_engine::AudioBackend;
use trill_engine::hand::by_hand;
use trill_runtime::{Meeting, Reach, Runtime, RuntimeOptions, Storage};
use trill_together::meet;

fn start(folder: &Path, name: &str) -> Runtime {
    let storage = Storage::At { settings: folder.join(name).join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let runtime = Runtime::start(options).expect("the runtime starts");
    runtime.set_name_together(name);
    runtime
}

/// Waits until `done`, looking every fifth of a millisecond.
fn until(what: &str, mut done: impl FnMut() -> bool) -> Duration {
    let began = Instant::now();
    while !done() {
        assert!(began.elapsed() < Duration::from_secs(600), "timed out waiting until {what}");
        std::thread::sleep(Duration::from_micros(200));
    }
    began.elapsed()
}

fn middle(mut times: Vec<Duration>) -> (f64, f64, f64) {
    times.sort();
    let ms = |index: usize| times[index.min(times.len() - 1)].as_secs_f64() * 1e3;
    (ms(times.len() / 2), ms(times.len() * 9 / 10), ms(times.len() - 1))
}

/// What this process holds in memory, in megabytes, as the system counts
/// it (its working set), where the system can be asked.
fn held_mb() -> Option<f64> {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Counters {
            size: u32,
            faults: u32,
            peak: usize,
            working_set: usize,
            rest: [usize; 6],
        }
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn K32GetProcessMemoryInfo(process: *mut std::ffi::c_void, counters: *mut Counters, size: u32) -> i32;
        }
        let mut counters =
            Counters { size: size_of::<Counters>() as u32, faults: 0, peak: 0, working_set: 0, rest: [0; 6] };
        // SAFETY: the counters are a struct of the size the call is told, which it fills in.
        let asked = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.size) };
        (asked != 0).then(|| counters.working_set as f64 / f64::from(1 << 20))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Writes a recording of about `megabytes` at `path`: a tone with noise
/// on it, so that no stretch of it is like another.
fn recording(path: &Path, megabytes: u64) {
    let rate = 48_000.0;
    // Stereo, 16 bits: four bytes a frame.
    let frames = (megabytes << 20) / 4;
    let mut lot = 0x2545_F491_4F6C_DD1Du64;
    let channel: Vec<f32> = (0..frames)
        .map(|n| {
            lot ^= lot << 13;
            lot ^= lot >> 7;
            lot ^= lot << 17;
            let noise = (lot >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
            (n as f64 * 220.0 * std::f64::consts::TAU / rate).sin() as f32 * 0.4 + noise * 0.1
        })
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let buffer = AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel };
    trill_media::write_wav(path, &buffer, trill_media::WavFormat::Int16).expect("the file is written");
}

/// What of the song at `file` would travel: nothing is sent anywhere.
fn count(file: &str) {
    let folder = std::env::temp_dir().join(format!("trill-files-count-{}", std::process::id()));
    let runtime = start(&folder, "me");
    runtime.session.lock().load(Path::new(file)).expect("the song opens");
    let going = runtime.files_to_go();
    let (clips, samples) = {
        let session = runtime.session.lock();
        let files = session.files_here();
        let clips = files.iter().filter(|file| matches!(file.place, FilePlace::Clip(_))).count();
        (clips, files.len() - clips)
    };
    println!("{file}");
    println!("  {clips} clips and takes play a file, and {samples} slots of samplers and drum racks");
    println!(
        "  {} files would go to whoever joins, {:.1} MB together",
        going.files,
        going.bytes as f64 / f64::from(1 << 20)
    );
    for (folder, files, bytes) in &going.folders {
        println!("    {files} in {folder}: {:.1} MB", *bytes as f64 / f64::from(1 << 20));
    }
    println!("    {} outside the song's folder, Trill's recordings and the sample library", going.elsewhere.len());
    if going.gone > 0 {
        println!("  {} files the song names are not on this computer", going.gone);
    }
    let missing = runtime.files_missing();
    println!("  missing as the song is opened here: {}", missing.files.len());
    drop(runtime);
    let _ = std::fs::remove_dir_all(&folder);
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let usage = || -> ! {
        eprintln!(
            "usage: files_cross MEGABYTES [--through-here | --through ADDRESS KEY]\n       files_cross --count SONG.trill"
        );
        std::process::exit(2)
    };
    if let [flag, file] = arguments.as_slice()
        && flag == "--count"
    {
        return count(file);
    }
    let Some(megabytes) = arguments.first().and_then(|size| size.parse::<u64>().ok()).filter(|size| *size > 0) else {
        usage()
    };
    let through: Option<(String, String, Option<meet::Server>)> = match &arguments[1..] {
        [] => None,
        [here] if here == "--through-here" => {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port on the loopback");
            let address = listener.local_addr().unwrap().to_string();
            let key = meet::Key::new().expect("a key by lot");
            let keys = meet::Keys::of(vec![meet::keys::Given::of(&key, "Ada")]);
            let log: meet::Log = Arc::new(|_: &str| {});
            // (As much as a key may pass in a day as this measurement takes.)
            let limits = meet::Limits { bytes_a_day: 64 << 30, ..meet::Limits::default() };
            let server = meet::Server::start(vec![listener], keys, limits, log).expect("it starts");
            Some((address, key.to_string(), Some(server)))
        }
        [through, address, key] if through == "--through" => Some((address.clone(), key.clone(), None)),
        _ => usage(),
    };
    let folder = std::env::temp_dir().join(format!("trill-files-cross-{}", std::process::id()));
    let file: PathBuf = folder.join("made here").join("a long take.wav");
    let began = Instant::now();
    recording(&file, megabytes);
    let bytes = std::fs::metadata(&file).unwrap().len();
    let mb = bytes as f64 / f64::from(1 << 20);
    println!("a recording of {mb:.1} MB, made in {:.1} s", began.elapsed().as_secs_f64());

    // ---- what reading it costs ---------------------------------------------
    let began = Instant::now();
    let content = trill_together::files::content_of(&file, |_| true).unwrap().unwrap();
    let hashed = began.elapsed();
    println!(
        "read to say what it is (SHA-256 of all of it): {:.0} ms, {:.0} MB a second",
        hashed.as_secs_f64() * 1e3,
        mb / hashed.as_secs_f64()
    );

    let (ada, ben) = (start(&folder, "Ada"), start(&folder, "Ben"));
    let (track, clip) = {
        let _hand = by_hand();
        let mut session = ada.session.lock();
        let track = session.edit(|song| Ok(song.add_track("Audio"))).unwrap();
        session.edit(|song| Ok(song.add_track("Keys"))).unwrap();
        let recording = AudioContent::new(file.to_string_lossy());
        let clip = session.edit(|song| song.add_audio_clip(track, "A long take", 0.0, 16.0, recording)).unwrap();
        (track, clip)
    };
    let resting = held_mb();
    let invitation = match &through {
        None => ada.share(Reach::ThisMachine, 0).expect("sharing starts"),
        Some((address, key, _)) => {
            ada.set_meeting_point(address, Some(key)).expect("what a keeper handed over");
            let invitation = ada.share_through_meeting_point().expect("sharing starts");
            until("the meeting is open at the meeting point", || {
                matches!(ada.together().meeting, Some((_, Meeting::Held)))
            });
            println!("through the meeting point at {address}");
            invitation
        }
    };
    // Its program says what the file is once it has read it: decoded, as
    // the song plays it, and hashed.
    let began = Instant::now();
    let said = |runtime: &Runtime| {
        let files = runtime.session.lock().files_here();
        files.into_iter().find(|file| file.place == FilePlace::Clip(clip)).and_then(|file| file.is)
    };
    until("the program has said what its file is", || said(&ada).is_some());
    assert_eq!(said(&ada).unwrap().content(), content);
    println!(
        "the song says what the file is {:.0} ms after it was shared (it is read as audio, then hashed)",
        began.elapsed().as_secs_f64() * 1e3
    );

    // ---- crossing, with the song worked in meanwhile -------------------------
    let whole = |runtime: &Runtime| {
        let files = runtime.session.lock().files_here();
        let here = files.into_iter().find(|file| file.place == FilePlace::Clip(clip));
        here.is_some_and(|file| std::fs::metadata(&file.path).is_ok_and(|found| found.len() == bytes))
            && runtime.together().files.is_empty()
    };
    let began = Instant::now();
    ben.join(&invitation, true).expect("the invitation is read");
    until("the one who joined has the song", || ben.session.lock().project().tracks.len() == 2);
    println!("joined: the song was in the other program after {:.0} ms", began.elapsed().as_secs_f64() * 1e3);
    let mut edits = Vec::new();
    let mut slowest: Option<(usize, Duration, Duration, Duration)> = None;
    let mut most = resting.unwrap_or(0.0);
    let mut round = 0usize;
    while !whole(&ben) {
        // An edit, and how long until the other's song has it.
        let before = ben.session.lock().revision();
        let at = Instant::now();
        let volume = -1.0 - (round % 20) as f32;
        ada.session.lock().edit(|song| song.set_track_volume(track, volume)).unwrap();
        let made = at.elapsed();
        let arrived = || ben.session.lock().project().track(track).is_ok_and(|track| track.volume_db == volume);
        let _ = before;
        until("the other has the edit", || arrived() || whole(&ben));
        if arrived() {
            edits.push(at.elapsed());
            if slowest.is_none_or(|(_, slowest, _, _)| at.elapsed() > slowest) {
                slowest = Some((edits.len(), at.elapsed(), at.duration_since(began), made));
            }
        }
        most = most.max(held_mb().unwrap_or(0.0));
        round += 1;
        std::thread::sleep(Duration::from_millis(7 + (round as u64 * 13) % 31));
    }
    let crossed = began.elapsed();
    println!(
        "the file was whole in the other program {:.2} s after joining: {:.0} MB a second",
        crossed.as_secs_f64(),
        mb / crossed.as_secs_f64()
    );
    let at_ben = ben.session.lock().files_here();
    let arrived = at_ben.iter().find(|file| file.place == FilePlace::Clip(clip)).expect("the clip names a file");
    let same = trill_together::files::content_of(Path::new(&arrived.path), |_| true).unwrap().unwrap() == content;
    assert!(same, "what arrived is not the file");
    if edits.len() >= 3 {
        let count = edits.len();
        let (mid, nine, worst) = middle(edits);
        println!(
            "meanwhile, {count} edits of a fader, each to the other's song: {mid:.0} ms in the middle, {nine:.0} ms \
             for nine in ten, {worst:.0} ms at worst"
        );
        if let Some((which, took, at, made)) = slowest {
            println!(
                "    (the slowest was edit {which} of {count}, made {:.2} s after joining: {:.0} ms; making it took {:.0} ms)",
                at.as_secs_f64(),
                took.as_secs_f64() * 1e3,
                made.as_secs_f64() * 1e3
            );
        }
    } else {
        println!("meanwhile there was time for {} edits: too few to say how long they took", edits.len());
    }
    // The same edits with nothing crossing, for comparison.
    let mut quiet = Vec::new();
    for round in 0..60usize {
        let volume = -21.0 - (round % 20) as f32;
        let at = Instant::now();
        ada.session.lock().edit(|song| song.set_track_volume(track, volume)).unwrap();
        until("the other has the edit", || {
            ben.session.lock().project().track(track).is_ok_and(|track| track.volume_db == volume)
        });
        quiet.push(at.elapsed());
        std::thread::sleep(Duration::from_millis(7 + (round as u64 * 13) % 31));
    }
    let (mid, nine, worst) = middle(quiet);
    println!(
        "afterwards, with nothing crossing: {mid:.0} ms in the middle, {nine:.0} ms for nine in ten, {worst:.0} ms at worst"
    );
    // The connection has to have held throughout: one that was let go of
    // and came back would show as an edit that took a second.
    let said: Vec<String> = [&ada, &ben]
        .iter()
        .flat_map(|runtime| runtime.said_together(0))
        .map(|(_, sentence)| sentence)
        .filter(|sentence| sentence.contains("lost") || sentence.contains("no longer") || sentence.contains("thrown"))
        .collect();
    match said.is_empty() {
        true => println!("the connection held throughout, and nothing that arrived was thrown away"),
        false => println!("NOT AS IT SHOULD BE: {}", said.join(" | ")),
    }
    let held = said.is_empty();
    if let Some(resting) = resting {
        println!(
            "memory: the process (both programs, the file decoded once to be played) held {resting:.0} MB before and \
             at most {most:.0} MB while the file crossed"
        );
    }
    drop((ada, ben));
    std::thread::sleep(Duration::from_millis(300));
    drop(through);
    let _ = std::fs::remove_dir_all(&folder);
    if !held {
        std::process::exit(1);
    }
}
