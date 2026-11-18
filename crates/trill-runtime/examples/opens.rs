//! Opens a song and says how long it took to be there: when the song was
//! on hand (its tracks, clips and mixer, and every device that is made in
//! no time), when each of its other devices arrived and what its making
//! took, when all of them were there, and when none was still reading
//! what it plays. Under that, what the plugin host says of each plugin it
//! made: how long it waited for the thread plugins are made on, and what
//! each step took (its file, the instance, its state, activating).
//!
//! ```text
//! cargo run -p trill-runtime --example opens -- <song.trill> [options]
//! cargo run -p trill-runtime --example opens -- long [options]
//!
//! `long` in place of a file is a long song without plugins that is made
//! here, saved under the profile and then opened like any other: 48 tracks
//! of the built-in instruments with their effects, 128 bars of clips on
//! each (some 50,000 notes), and eight recordings of four minutes, at
//! another rate than the device's, each on a track of its own. They are
//! written once (350 MB) and found there the next time.
//!
//! --after <song.trill>  opens this song first and waits until it is whole: what is
//!                       measured is then a song opened over another, whose plugins
//!                       have to be taken down
//! --at-once             makes every device where the song is opened, before the song
//!                       is there, as it was before devices came after
//! --profile <folder>    where the program keeps what it keeps. Default:
//!                       `target/opens`. Never the user's own.
//! --down-first          with `--after`: takes the plugins of the song before down
//!                       ahead of making this song's, in place of after
//! --no-plugins          the built-in devices only
//! --sampled <file.sfz>  with `long`: four more tracks, each playing this
//!                       sampled instrument
//! ```
//!
//! The song is read and never written. It opens with the plugins that are
//! installed, which takes as much memory as the song takes in the window:
//! look at what the machine has free first. No window is opened, and
//! nothing is heard.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use trill_core::ChainRef;
use trill_core::audio::AudioBuffer;
use trill_core::project::{AudioContent, Note, TrackKind};
use trill_engine::host::NullHost;
use trill_engine::{AudioBackend, Session};
use trill_media::{WavFormat, write_wav};
use trill_plugins::Scanner;
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

/// What the plugin host said of the plugins it made and took down, each
/// with when it was said.
static SAID: Mutex<Vec<(Instant, String)>> = Mutex::new(Vec::new());

/// Keeps the lines that say what a making took, and lets the rest go.
struct Makings;

impl log::Log for Makings {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target() == trill_plugins::making::TARGET
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata())
            && let Ok(mut said) = SAID.lock()
        {
            said.push((Instant::now(), record.args().to_string()));
        }
    }

    fn flush(&self) {}
}

struct Options {
    song: PathBuf,
    after: Option<PathBuf>,
    at_once: bool,
    profile: PathBuf,
    plugins: bool,
    sampled: Option<PathBuf>,
    down_first: bool,
}

/// The word that stands for the song made here: see the top of the file.
const LONG: &str = "long";
/// What the long song is made of.
const BARS: usize = 128;
const RECORDINGS: usize = 8;
const RECORDING_SECONDS: usize = 240;

/// Writes the recordings the long song plays, where they are not there
/// yet: tones that fade in and out, at 44.1 kHz, so that a device at 48
/// has each to convert.
fn recordings(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for index in 0..RECORDINGS {
        let file = folder.join(format!("recording {}.wav", index + 1));
        if !file.exists() {
            let (rate, frames) = (44_100.0, 44_100 * RECORDING_SECONDS);
            let pitch = 110.0 * (index + 1) as f64;
            let tone = |frame: usize| {
                let at = frame as f64 / rate;
                ((at * pitch * std::f64::consts::TAU).sin() * 0.2 * (at * 0.5).sin().abs()) as f32
            };
            let left: Vec<f32> = (0..frames).map(tone).collect();
            let buffer = AudioBuffer { sample_rate: rate, right: left.clone(), left };
            write_wav(&file, &buffer, WavFormat::Int16).map_err(|error| error.to_string())?;
        }
        files.push(file);
    }
    Ok(files)
}

/// Makes the long song in `session`: see the top of the file.
fn long_song(session: &mut Session, recordings: &[PathBuf], sampled: Option<&Path>) -> Result<(), String> {
    let said = |error: trill_engine::SessionError| error.to_string();
    let room = session
        .edit(|project| {
            project.name = "Long".to_string();
            project.tempo = 120.0;
            Ok(project.add_track_of_kind("Room", TrackKind::Return))
        })
        .map_err(said)?;
    session.add_device(ChainRef::Track(room), "builtin:reverb", None, None).map_err(said)?;
    // The instrument, its sound if it has one, the lowest note of its
    // part, and how long each note is.
    type Part = (&'static str, Option<&'static str>, u8, f64);
    let parts: [Part; 8] = [
        ("builtin:drumkit", None, 36, 0.5),
        ("builtin:wavetable", Some("Solid Bass"), 36, 0.5),
        ("builtin:wavetable", Some("Glass Pad"), 60, 4.0),
        ("builtin:wavetable", Some("PWM Strings"), 55, 4.0),
        ("builtin:wavetable", Some("Choir"), 48, 2.0),
        ("builtin:wavetable", Some("Acid Line"), 45, 0.25),
        ("builtin:polysynth", None, 72, 0.5),
        ("builtin:polysynth", None, 60, 1.0),
    ];
    let sampled = sampled.map(|file| file.to_string_lossy().into_owned());
    let tracks = 48 + if sampled.is_some() { 4 } else { 0 };
    for index in 0..tracks {
        let (instrument, sound, lowest, length) = match (&sampled, index >= 48) {
            (Some(file), true) => ("builtin:sampled", Some(file.as_str()), 48, 1.0),
            _ => parts[index % parts.len()],
        };
        let name = format!("Part {}", index + 1);
        let track = session.edit(|project| Ok(project.add_track(name.clone()))).map_err(said)?;
        session.add_device(ChainRef::Track(track), instrument, sound, None).map_err(said)?;
        session.add_device(ChainRef::Track(track), "builtin:eq", None, None).map_err(said)?;
        session.add_device(ChainRef::Track(track), "builtin:compressor", None, None).map_err(said)?;
        let edited = session.edit(|project| {
            // A clip every four bars, each with its notes.
            for clip in 0..BARS / 4 {
                let id = project.add_midi_clip(track, format!("{name} {}", clip + 1), clip as f64 * 16.0, 16.0)?;
                let steps = (16.0 / length) as usize;
                let notes: Vec<Note> = (0..steps)
                    .map(|step| {
                        let pitch = lowest + [0, 5, 3, 7][(step + clip) % 4];
                        Note { pitch, start: step as f64 * length, length: length * 0.9, velocity: 96 }
                    })
                    .collect();
                project.clip_mut(id)?.add_notes(&notes)?;
            }
            project.set_send(track, room, Some(-18.0))
        });
        edited.map_err(said)?;
    }
    for (index, file) in recordings.iter().enumerate() {
        let edited = session.edit(|project| {
            let track = project.add_track(format!("Recording {}", index + 1));
            let audio = AudioContent { path: file.to_string_lossy().into_owned(), ..AudioContent::default() };
            // At the speed it was recorded at: four minutes are 480 beats at 120.
            project.add_audio_clip(track, "Recording", 0.0, RECORDING_SECONDS as f64 * 2.0, audio)?;
            Ok(())
        });
        edited.map_err(said)?;
    }
    session.add_device(ChainRef::Master, "builtin:limiter", None, None).map_err(said)?;
    Ok(())
}

/// Makes the long song with a runtime of its own, saves it under
/// `options.profile`, and returns where. The runtime is gone when this
/// returns, and with it everything it had read of the song's recordings.
fn make_long(options: &Options) -> Result<PathBuf, String> {
    let folder = options.profile.join("long");
    let began = Instant::now();
    let recordings = recordings(&folder)?;
    let storage =
        Storage::At { settings: folder.join("settings.json"), library_index: Some(folder.join("library-index.json")) };
    let runtime_options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let runtime = Runtime::start_custom(runtime_options, Parts::default()).map_err(|error| error.to_string())?;
    let file = folder.join(if options.sampled.is_some() { "long with sampled.trill" } else { "long.trill" });
    let mut session = runtime.session.lock();
    long_song(&mut session, &recordings, options.sampled.as_deref())?;
    session.save(&file).map_err(|error| error.to_string())?;
    let (tracks, notes) = {
        let project = session.project();
        let notes: usize = project.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.notes().len()).sum();
        (project.tracks.len(), notes)
    };
    println!(
        "the long song was made and saved in {:.1} s: {tracks} tracks, {notes} notes, {} recordings of {} s",
        seconds(began.elapsed()),
        recordings.len(),
        RECORDING_SECONDS
    );
    Ok(file)
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let [song, rest @ ..] = arguments else {
        return Err("usage: opens <song.trill>|long [--after <song.trill>] [--at-once] [--profile <folder>] \
                    [--no-plugins] [--sampled <file.sfz>]"
            .to_string());
    };
    let mut options = Options {
        song: song.into(),
        after: None,
        at_once: false,
        profile: PathBuf::from("target").join("opens"),
        plugins: true,
        sampled: None,
        down_first: false,
    };
    let mut rest = rest.iter();
    while let Some(name) = rest.next() {
        match name.as_str() {
            "--at-once" => options.at_once = true,
            "--down-first" => options.down_first = true,
            "--no-plugins" => options.plugins = false,
            "--after" | "--profile" | "--sampled" => {
                let value = rest.next().ok_or(format!("{name} needs a value"))?;
                match name.as_str() {
                    "--after" => options.after = Some(value.into()),
                    "--sampled" => options.sampled = Some(value.into()),
                    _ => options.profile = value.into(),
                }
            }
            other => return Err(format!("there is no option '{other}'")),
        }
    }
    Ok(options)
}

fn seconds(time: Duration) -> f64 {
    time.as_secs_f64()
}

/// Opens `song` and says how it went; with `quietly`, says only that it
/// was opened.
fn open(runtime: &Runtime, song: &Path, quietly: bool) -> Result<(), String> {
    let began = Instant::now();
    let mark = SAID.lock().map(|said| said.len()).unwrap_or(0);
    let (name, devices) = {
        let mut session = runtime.session.lock();
        session.load(song).map_err(|error| format!("{}: {error}", song.display()))?;
        let project = session.project();
        (project.name.clone(), project.devices().count())
    };
    let there = began.elapsed();
    if quietly {
        // Until it is whole, looked at two hundred times a second for
        // three minutes at the most.
        let mut session = runtime.session.lock();
        session.wait_until_opened(Duration::from_secs(180));
        println!("'{name}' was opened first: whole after {:.1} s", seconds(began.elapsed()));
        return Ok(());
    }
    println!(
        "'{name}': the song was there {:.0} ms after it was asked for: {} tracks, {devices} devices",
        seconds(there) * 1e3,
        runtime.session.lock().project().tracks.len()
    );
    // Its devices as they arrive: looked at fifty times a second, for
    // three minutes at the most.
    let mut said = 0;
    let mut first = None;
    for _ in 0..9_000 {
        let (made, arriving): (Vec<_>, _) = {
            let session = runtime.session.lock();
            (session.made_since_opening().to_vec(), session.arriving())
        };
        for made in &made[said.min(made.len())..] {
            first.get_or_insert(made.there_after);
            println!(
                "  {:6.2} s  {:<34} made in {:5.0} ms{}",
                seconds(made.there_after),
                made.name,
                seconds(made.took) * 1e3,
                made.failed.as_ref().map(|reason| format!("  COULD NOT BE MADE: {reason}")).unwrap_or_default()
            );
        }
        said = made.len();
        if arriving.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let whole = began.elapsed();
    let (made, problems) = {
        let session = runtime.session.lock();
        let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
        (session.made_since_opening().to_vec(), problems)
    };
    let making: Duration = made.iter().map(|made| made.took).sum();
    match (made.len(), first) {
        (0, _) | (_, None) => println!("every device was there with the song: none is made after it"),
        (count, Some(first)) => println!(
            "{count} devices came after the song: the first {:.2} s after it was asked for, the last {:.2} s; their \
             making took {:.2} s in all, one after the other",
            seconds(first),
            seconds(whole),
            seconds(making)
        ),
    }
    for problem in problems {
        println!("  problem: {problem}");
    }
    // Until none of them is still reading what it plays: three minutes at
    // the most.
    for _ in 0..720 {
        if runtime.session.lock().loading().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let loading = runtime.session.lock().loading().len();
    println!(
        "nothing was loading any more {:.1} s after it was asked for ({loading} device(s) still are)",
        seconds(began.elapsed())
    );
    // What the plugin host said of each making and each taking down.
    let said: Vec<(Instant, String)> =
        SAID.lock().map(|said| said[mark.min(said.len())..].to_vec()).unwrap_or_default();
    if !said.is_empty() {
        println!("what the plugin host says of it:");
    }
    for (at, line) in said {
        println!("  {:6.2} s  {line}", seconds(at.saturating_duration_since(began)));
    }
    Ok(())
}

fn run(options: &Options) -> Result<(), String> {
    // The song made here is made first, by a runtime that is gone before
    // the one that opens it starts.
    let made = (options.song.as_os_str() == LONG).then(|| make_long(options)).transpose()?;
    let song = made.as_deref().unwrap_or(&options.song);
    let storage = Storage::At {
        settings: options.profile.join("settings.json"),
        library_index: Some(options.profile.join("library-index.json")),
    };
    let runtime_options = RuntimeOptions {
        audio: AudioBackend::Null { sample_rate: 48_000 },
        plugins: options.plugins.then_some(Scanner::CurrentExecutable),
        storage,
    };
    let device = NullHost::like_a_device(48_000, 480);
    let parts = Parts { audio: Some(Arc::new(device)), ..Parts::default() };
    let runtime = Runtime::start_custom(runtime_options, parts).map_err(|error| error.to_string())?;
    runtime.session.lock().open_all_at_once(options.at_once);
    runtime.session.lock().take_down_before_making(options.down_first);
    if let Some(first) = &options.after {
        open(&runtime, first, true)?;
    }
    open(&runtime, song, false)
}

fn main() -> ExitCode {
    // Plugin files are scanned by this program started a second time.
    trill_plugins::scan::run_as_scanner_if_requested();
    trill_runtime::jobs::run_as_job_if_requested();
    if log::set_logger(&Makings).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse(&arguments).and_then(|options| run(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("{problem}");
            ExitCode::FAILURE
        }
    }
}
