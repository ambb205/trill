//! A take that outlives the program that was recording it.
//!
//! A program that is killed in the middle of a take gets to do nothing
//! more: no file is closed, no header is put right, nothing is saved. What
//! is left is what was on disk at that moment. So this is tried with a
//! real process and a real kill: the test starts its own test program a
//! second time (as a render's job is started), that process records from
//! an input that exists only there, keeping the machine's time, and writes
//! a copy of its song for safety as a runtime does; and when it has
//! recorded for a while it is killed, by its PID, with no warning.
//!
//! What the test then holds: the take's file is a WAV file that Trill's
//! own reader reads, holding all but the last two seconds of what had been
//! sung; and the safety copy opens in another session, shows the take on
//! its track, and plays it.

mod common;
mod rig;

use common::{peak, zero_crossing_hz};
use rig::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::TrackInput;
use trill_engine::host::AudioConfig;
use trill_engine::host::scripted::{Hears, ScriptedHost, ScriptedInput};
use trill_engine::{Patience, RenderOptions};
use trill_media::MediaCache;

/// Says, to the process that is to be killed, where it keeps what it
/// writes. Set only by the test that starts it.
const VICTIM: &str = "TRILL_TAKE_VICTIM";
const RATE: u64 = 48_000;

fn progress_file(folder: &Path) -> PathBuf {
    folder.join("progress.txt")
}

fn copy_file(folder: &Path) -> PathBuf {
    folder.join("copy.trill")
}

/// Writes `text` to `path` whole: under another name first, so that
/// whoever reads it never finds half of it.
fn write_whole(path: &Path, text: &str) {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, text).unwrap();
    std::fs::rename(&temporary, path).unwrap();
}

/// The process that is killed: it records a tone until then. Run by the
/// test below, as this test program started a second time; by itself, in
/// the ordinary run, it does nothing.
#[test]
fn a_process_that_records_until_it_is_killed() {
    let Some(folder) = std::env::var_os(VICTIM).map(PathBuf::from) else { return };
    let host = ScriptedHost::with_speakers();
    host.add_input(ScriptedInput::new("Microphone"));
    host.hear(Hears::Sound(Arc::new(|_, seconds| (0.25 * (seconds * std::f64::consts::TAU * 440.0).sin()) as f32)));
    let mut rig = Rig::with(host, AudioConfig::default(), Patience::default());
    rig.session.keep_unsaved_recordings_in(folder.join("unsaved"));
    rig.session.set_count_in(0);
    let voice = rig.session.edit(|project| Ok(project.add_track("Voice"))).unwrap();
    rig.session.edit(|project| project.set_track_input(voice, Some(TrackInput::mono(1)))).unwrap();
    rig.session.set_armed(voice, true).unwrap();
    rig.session.housekeeping();
    rig.run(40);
    rig.session.record().unwrap();
    let began = rig.now();
    let mut copied = 0.0;
    // Twenty milliseconds of the device for twenty of the machine's: the
    // thread that writes the take keeps the machine's time. A minute at
    // the most, in case nobody comes to kill it.
    for _ in 0..3000 {
        rig.run(2);
        rig.session.housekeeping();
        let sung = rig.now() - began;
        if let Some(take) = rig.session.growing_takes().first() {
            write_whole(&progress_file(&folder), &format!("{}\n{}", sung as u64, take.file.path().display()));
        }
        // A copy of the song for safety, as a runtime keeps one: from
        // three seconds in, and again every second.
        if sung >= 3.0 * RATE as f64 && sung - copied >= RATE as f64 {
            write_whole(&copy_file(&folder), &rig.session.snapshot_json());
            copied = sung;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How many frames the process had sung into its take when it last said,
/// and the file it is written to.
fn progress(folder: &Path) -> Option<(u64, PathBuf)> {
    let text = std::fs::read_to_string(progress_file(folder)).ok()?;
    let (sung, file) = text.split_once('\n')?;
    Some((sung.parse().ok()?, PathBuf::from(file)))
}

#[test]
fn a_take_whose_process_is_killed_is_a_file_that_reads_and_its_safety_copy_plays_it() {
    let folder = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["a_process_that_records_until_it_is_killed", "--exact", "--nocapture", "--test-threads", "1"])
        .env(VICTIM, folder.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the test program starts a second time");
    // Until it has sung for five seconds and has kept a copy.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        assert!(Instant::now() < deadline, "the process never got to recording");
        assert!(child.try_wait().unwrap().is_none(), "the process ended by itself");
        let far_enough = progress(folder.path()).is_some_and(|(sung, _)| sung >= 5 * RATE);
        if far_enough && copy_file(folder.path()).is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let (sung, file) = progress(folder.path()).unwrap();
    // Killed: by its PID, with nothing said to it first.
    let pid = child.id();
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success(), "process {pid} was killed, and did not end by itself");

    // The take's file is a WAV file that reads, and holds all but the
    // last two seconds of what had been sung.
    let audio = MediaCache::default().audio(&file).expect("the file of a killed take reads as a WAV file");
    let held = audio.frames() as u64;
    println!(
        "killed after {:.2} s of take: the file holds {:.2} s ({} frames), {:.2} s less",
        sung as f64 / RATE as f64,
        held as f64 / RATE as f64,
        held,
        sung.saturating_sub(held) as f64 / RATE as f64
    );
    assert_eq!(audio.sample_rate, RATE as f64);
    assert!(held + 2 * RATE >= sung, "{held} frames of {sung} sung");
    assert!(held >= 3 * RATE, "{held}");
    // What it holds is what was sung, to its last second.
    let last = &audio.left[(held - RATE) as usize..held as usize];
    assert!((peak(last) - 0.25).abs() < 0.01, "{}", peak(last));
    assert!((zero_crossing_hz(last, RATE as f64) - 440.0).abs() < 2.0);

    // The copy that was kept for safety opens in a program that never
    // saw the take, shows it on its track, and plays it.
    let mut reader = Rig::new();
    reader.session.load_recovered(&copy_file(folder.path()), None).unwrap();
    assert!(reader.session.problems().is_empty(), "{:?}", reader.session.problems());
    let project = reader.session.project().clone();
    let track = project.tracks.iter().find(|track| track.name == "Voice").expect("the track is there");
    let (clip, take) = (&track.clips[0], &track.takes[0]);
    assert_eq!(Path::new(&clip.audio().expect("a recording").path), file.as_path());
    assert_eq!(take.clip.audio().unwrap().path, clip.audio().unwrap().path, "and it is kept as a take");
    // Two beats to the second: the copy was made three seconds in or later.
    assert!(clip.start == 0.0 && clip.length >= 4.0, "{} beats from {}", clip.length, clip.start);
    let options = RenderOptions { sample_rate: RATE as u32, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let played = reader.session.render_job().render(&options).unwrap();
    let second = &played.left[RATE as usize..2 * RATE as usize];
    assert!(peak(second) > 0.1, "the take is heard: {}", peak(second));
    assert!((zero_crossing_hz(second, RATE as f64) - 440.0).abs() < 2.0);
}
