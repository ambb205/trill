//! Recording sound, end to end: a session, an output and an input that
//! exist only here, each with a clock, a latency and a buffer of its own
//! that the test sets, and the test as their threads. Time moves only as
//! the output is advanced, so where a sound was heard and where it landed
//! are exact on any machine.
//!
//! What an input hears is a function of the host's time (a bump at a
//! moment the test chooses, a sine), or what the output plays, as through
//! a cable.

mod rig;

use parking_lot::Mutex;
use rig::*;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use trill_core::project::AudioContent;
use trill_core::{ChainRef, ClipId, Monitoring, TrackId, TrackInput};
use trill_engine::host::AudioConfig;
use trill_engine::host::scripted::{Hears, ScriptedHost, ScriptedInput, Stamped};
use trill_engine::{Correction, Measurement, RecordState, Session, SessionError};

/// A host with `Speakers` and one input, `Microphone`, as `input` has it.
fn studio(input: ScriptedInput) -> ScriptedHost {
    let host = ScriptedHost::with_speakers();
    host.add_input(input);
    host
}

fn microphone() -> ScriptedInput {
    ScriptedInput::new("Microphone")
}

/// A rig on `host` whose song counts nothing in and keeps its takes in a
/// folder of the test's.
fn rig_on(host: ScriptedHost, config: AudioConfig) -> (Rig, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let mut rig = Rig::with(host, config, trill_engine::Patience::default());
    rig.session.keep_unsaved_recordings_in(folder.path().join("unsaved"));
    rig.session.set_count_in(0);
    (rig, folder)
}

/// A track that records `input`, armed, with the input open.
fn armed_track(rig: &mut Rig, name: &str, input: TrackInput) -> TrackId {
    let track = rig.session.edit(|project| Ok(project.add_track(name))).unwrap();
    rig.session.edit(|project| project.set_track_input(track, Some(input))).unwrap();
    rig.session.set_armed(track, true).unwrap();
    rig.session.housekeeping();
    assert!(rig.session.input_status().open.is_some(), "{:?}", rig.session.input_status());
    track
}

/// Runs until the place an input that was just opened is read at has been
/// put for good, which takes a quarter of a second of its time stamps: a
/// take that is asked for before then waits for it, and the song with it.
fn settle(rig: &mut Rig) {
    rig.run_until(rig.now() + 0.3 * SR);
}

/// Short bumps at moments of the host's clock, which a test adds to as it
/// goes: what an input hears of them, on its first channel.
#[derive(Clone, Default)]
struct Bumps(Arc<Mutex<Vec<f64>>>);

impl Bumps {
    fn at(&self, seconds: f64) {
        self.0.lock().push(seconds);
    }

    /// What the first channel hears, with `second` on the second.
    fn heard(&self, second: impl Fn(f64) -> f32 + Send + Sync + 'static) -> Hears {
        let bumps = self.0.clone();
        Hears::Sound(Arc::new(move |channel, seconds| {
            if channel == 1 {
                return second(seconds);
            }
            let bumps = bumps.lock();
            let near = bumps.iter().filter(|at| (**at - seconds).abs() < 0.001);
            near.map(|at| (0.8 * (-((seconds - at) * SR / 3.0).powi(2)).exp()) as f32).sum()
        }))
    }
}

/// `count` frames of a file a take was written to, from frame `from`:
/// every frame's channels side by side.
fn frames_of(path: &Path, channels: usize, from: u64, count: usize) -> Vec<f32> {
    let mut file = std::fs::File::open(path).unwrap();
    // The header of a file that was recorded is 56 bytes.
    file.seek(SeekFrom::Start(56 + from * channels as u64 * 4)).unwrap();
    let mut bytes = vec![0u8; count * channels * 4];
    let read = file.read(&mut bytes).unwrap();
    bytes[..read - read % 4].chunks_exact(4).map(|sample| f32::from_le_bytes(sample.try_into().unwrap())).collect()
}

/// Where the highest point of `samples` lies, between frames too.
fn top_of(samples: &[f32]) -> f64 {
    let (top, _) = samples
        .iter()
        .enumerate()
        .fold((0, f32::MIN), |best, (index, sample)| if *sample > best.1 { (index, *sample) } else { best });
    if top == 0 || top + 1 >= samples.len() {
        return top as f64;
    }
    let (a, b, c) = (samples[top - 1] as f64, samples[top] as f64, samples[top + 1] as f64);
    top as f64 + 0.5 * (a - c) / (a - 2.0 * b + c)
}

/// The audio clip a track plays at `beat`: its id, where it starts, and
/// what it plays.
fn clip_at(session: &Session, track: TrackId, beat: f64) -> (ClipId, f64, f64, AudioContent) {
    let track = session.project().track(track).unwrap();
    let clip = track
        .clips
        .iter()
        .find(|clip| clip.audio().is_some() && clip.start <= beat + 1e-9 && clip.end() > beat)
        .unwrap_or_else(|| panic!("no audio clip at beat {beat}: {:?}", track.clips));
    (clip.id, clip.start, clip.length, clip.audio().unwrap().clone())
}

/// How far from its place a bump lies in the file `audio` names, in
/// frames: one that was heard `frames_in` frames of the song after the
/// moment `audio` begins to play at.
fn bump_off(audio: &AudioContent, frames_in: f64) -> f64 {
    let in_file = audio.offset_seconds * SR + frames_in;
    let from = in_file.round() as u64 - 64;
    let near = frames_of(Path::new(&audio.path), 1, from, 128);
    from as f64 + top_of(&near) - in_file
}

/// How far from its place the bump that was heard with song frame
/// `song_frame` lies in what the track plays, in frames: found in the file
/// the clip under it names, where the clip has that frame of the song. (In
/// a song of one tempo.)
fn off_its_place(session: &Session, track: TrackId, song_frame: f64) -> f64 {
    let beat = song_frame / FRAMES_PER_BEAT;
    let (_, start, _, audio) = clip_at(session, track, beat);
    bump_off(&audio, song_frame - start * FRAMES_PER_BEAT)
}

/// Takes the limiter off the master: a test that looks at what comes out
/// wants it as it went in.
fn without_limiter(rig: &mut Rig) {
    let limiter = rig.session.project().master.devices[0].id;
    rig.session.edit(|project| project.remove_device(limiter).map(|_| ())).unwrap();
}

/// Waits, in the machine's own time, until `done` says so: for the thread
/// that writes a take, which does not keep the test's clock.
fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "what was waited for never came");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Runs until the host's clock reaches `frame` while a take is recorded,
/// no further ahead of the thread that writes it than that thread can
/// make up: the test's clock runs hundreds of times as fast as the
/// machine's, and frames that find no room on their way to disk are,
/// rightly, silence.
fn run_taking(rig: &mut Rig, frame: f64) {
    let written = |rig: &Rig| rig.session.growing_takes().iter().map(|take| take.file.frames()).min();
    // What was handed over before this is written first, however far
    // behind the thread that writes had come: it has caught up when it
    // has nothing more to write. (Or what it was behind by adds up from
    // one call to the next, until there is no room on the way to disk.)
    let mut seen = written(rig);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(30));
        let now = written(rig);
        if std::mem::replace(&mut seen, now) == now {
            break;
        }
    }
    let began = rig.now();
    let before = written(rig).unwrap_or(0) as f64;
    while rig.now() < frame {
        let until = (rig.now() + 48_000.0).min(frame);
        rig.run_until(until);
        rig.session.housekeeping();
        // Within a second of what has been handed over since: well inside
        // what there is room for on the way to disk, on a machine that
        // gives the thread that writes its turn late.
        // (A file that takes no more is not waited for: that is its test.)
        let failed = |rig: &Rig| rig.session.growing_takes().iter().any(|take| take.file.failed().is_some());
        wait_until(|| {
            failed(rig) || written(rig).is_none_or(|frames| frames as f64 - before + 48_000.0 >= rig.now() - began)
        });
    }
}

/// The pitch of a tone by how often it crosses nothing on its way up.
fn pitch_of(samples: &[f32]) -> f64 {
    let crossings = samples.windows(2).filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0).count();
    crossings as f64 * SR / samples.len() as f64
}

#[test]
fn a_click_heard_with_a_frame_of_the_song_lands_on_that_frame_at_any_buffer_size() {
    // Shared, as the devices come, and taken exclusively at five sizes.
    let shapes: [Option<u32>; 6] = [None, Some(64), Some(128), Some(256), Some(512), Some(1024)];
    for buffer in shapes {
        let config = AudioConfig { exclusive: buffer.is_some(), buffer_size: buffer, ..AudioConfig::default() };
        let (mut rig, _folder) = rig_on(studio(microphone()), config);
        let bumps = Bumps::default();
        rig.host.hear(bumps.heard(|_| 0.0));
        let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
        rig.run(40);
        settle(&mut rig);

        // The song's first frame is the first frame of the next buffer:
        // it is heard the output's latency later.
        let latency = rig.host.running().unwrap().latency_frames as f64;
        let heard = |song_frame: f64| (rig.now() + latency + song_frame) / SR;
        let song_frames = [0.0, 12_000.0, 24_000.0, 61_234.0, 100_000.5, 143_999.0];
        for frame in song_frames {
            bumps.at(heard(frame));
        }
        rig.session.record().unwrap();
        rig.run_until(rig.now() + 6.5 * FRAMES_PER_BEAT);
        rig.session.stop();

        let recorded = rig.session.last_recording().expect("the take has ended").clone();
        assert_eq!(recorded.takes.len(), 1, "{buffer:?}: {recorded:?}");
        assert_eq!(recorded.silent_frames, 0, "{buffer:?}: nothing was late");
        assert_eq!(recorded.correction, Correction::None);
        let (_, start, length, _) = clip_at(&rig.session, voice, 1.0);
        assert_eq!(start, 0.0, "the clip begins where the song began");
        assert!(length > 6.4 && length < 6.6, "and is as long as the take: {length}");
        for frame in song_frames {
            let off = off_its_place(&rig.session, voice, frame);
            assert!(off.abs() <= 1.0, "{buffer:?}: the click at song frame {frame} is {off:.3} frames off");
        }
    }
}

#[test]
fn a_take_begun_after_a_count_in_and_from_the_middle_of_the_song_is_where_it_was_sung() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    rig.session.set_count_in(1);
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.0));
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    rig.session.seek(8.0);
    rig.run(40);
    // A bar of count-in is four beats of the device's own frames; the
    // song's frame at beat 8 is the first after it. And something is sung
    // into the count-in too, half a beat before the song begins.
    let rolls = rig.now() + 4.0 * FRAMES_PER_BEAT;
    let latency = LATENCY;
    for beats in [-0.5, 0.0, 1.25, 3.0] {
        bumps.at((rolls + latency + beats * FRAMES_PER_BEAT) / SR);
    }
    rig.session.record().unwrap();
    assert_eq!(rig.session.record_status().state, RecordState::CountIn);
    rig.run_until(rolls + 4.5 * FRAMES_PER_BEAT);
    // While it is recorded it can be drawn as it grows. (The thread that
    // writes keeps the machine's time, which this test does not.)
    rig.session.housekeeping();
    let growing = rig.session.growing_takes();
    assert_eq!(growing.len(), 1);
    assert_eq!((growing[0].track, growing[0].start_beat), (voice, Some(8.0)));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while growing[0].file.frames() < 3 * 48_000 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(growing[0].file.frames() > 3 * 48_000, "written as it is sung: {}", growing[0].file.frames());
    assert!(growing[0].file.columns(0).len() > 300, "with what it looks like");
    rig.session.stop();

    let (_, start, _, audio) = clip_at(&rig.session, voice, 9.0);
    assert_eq!(start, 8.0);
    for beats in [0.0, 1.25, 3.0] {
        let off = off_its_place(&rig.session, voice, (8.0 + beats) * FRAMES_PER_BEAT);
        assert!(off.abs() <= 1.0, "the click {beats} beats in is {off:.3} frames off");
    }
    // What was sung into the count-in is in the file, ahead of where the
    // clip begins: drawing the clip's start out brings it back.
    let before = audio.offset_seconds * SR - 0.5 * FRAMES_PER_BEAT;
    let near = frames_of(Path::new(&audio.path), 1, before.round() as u64 - 64, 128);
    let off = before.round() - 64.0 + top_of(&near) - before;
    assert!(off.abs() <= 1.0, "the pickup is {off:.3} frames off");
    // One undo step for the take, and its file stays.
    assert!(rig.session.undo());
    assert!(rig.session.project().track(voice).unwrap().clips.is_empty());
    assert!(rig.session.project().track(voice).unwrap().takes.is_empty());
    assert!(Path::new(&audio.path).is_file(), "undoing a take leaves its recording");
}

#[test]
fn a_monitored_input_is_heard_after_exactly_the_time_trill_says_while_the_clocks_drift() {
    for ppm in [0.0, 200.0, -200.0] {
        let host = studio(ScriptedInput { ppm, ..microphone() });
        let (mut rig, _folder) = rig_on(host, AudioConfig::default());
        let bumps = Bumps::default();
        rig.host.hear(bumps.heard(|_| 0.0));
        let input = TrackInput { channel: 1, stereo: false, monitor: Monitoring::Always };
        let voice = rig.session.edit(|project| Ok(project.add_track("Voice"))).unwrap();
        // The limiter on the master would get in the way of a bump.
        let limiter = rig.session.project().master.devices[0].id;
        rig.session.edit(|project| project.remove_device(limiter).map(|_| ())).unwrap();
        rig.session.edit(|project| project.set_track_input(voice, Some(input))).unwrap();
        rig.session.housekeeping();
        let said = rig.session.recording_latency().expect("the input is open: it is monitored");
        // What Trill says: the device's own figures and its own hand-over.
        assert!((said.output_ms - 30.0).abs() < 1e-9 && (said.input_ms - 5.0).abs() < 1e-9, "{said:?}");
        let sum = said.output_ms + said.input_ms + said.handover_ms;
        assert!((said.round_trip_ms - sum).abs() < 1e-9, "{said:?}");

        rig.host.keep_output();
        rig.run(10);
        rig.host.take_output();
        let began = rig.now();
        // A bump every second for a minute and a half.
        for second in 1..90 {
            bumps.at(began / SR + second as f64);
        }
        rig.run_until(began + 91.0 * SR);
        let (left, right) = rig.host.take_output();
        assert_eq!(left, right, "one channel is in the middle");
        let mut worst: f64 = 0.0;
        for second in 1..90 {
            // The frame of the output that is at the speaker when the
            // bump is `round trip` old.
            let made = second as f64 * SR + said.round_trip_ms * SR / 1000.0 - LATENCY;
            let from = made.round() as usize - 64;
            let off = (from as f64 + top_of(&left[from..from + 128]) - made) / SR * 1000.0;
            assert!(off.abs() < 1.0, "{ppm} ppm: the bump at {second} s is heard {off:.3} ms off");
            if second > 3 {
                worst = worst.max(off.abs());
            }
        }
        assert!(worst < 0.05, "{ppm} ppm: once the clocks are told apart, {worst:.4} ms at the most");
        println!(
            "monitored at {ppm} ppm: heard {:.3} ms after it is sung, as said, to within {worst:.4} ms",
            said.round_trip_ms
        );
        let drift = rig.session.recording_latency().unwrap().drift_ppm;
        assert!((drift - ppm).abs() < 5.0, "the input's clock is read as {drift:.1} ppm off, and is {ppm}");
    }
}

#[test]
fn an_input_is_open_only_while_something_needs_it_and_says_what_is_wrong() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    let voice = rig.session.edit(|project| Ok(project.add_track("Voice"))).unwrap();
    let keys = rig.session.edit(|project| Ok(project.add_track("Keys"))).unwrap();
    rig.session.add_device(ChainRef::Track(keys), "builtin:polysynth", None, None).unwrap();
    let open = |rig: &mut Rig| {
        rig.session.housekeeping();
        rig.host.capturing().is_some()
    };
    assert!(!open(&mut rig), "nothing records an input");
    rig.session.set_armed(keys, true).unwrap();
    assert!(!open(&mut rig), "a track that records a keyboard needs none");
    rig.session.edit(|project| project.set_track_input(voice, Some(TrackInput::mono(1)))).unwrap();
    assert!(!open(&mut rig), "a track with an input that is not armed needs none either");
    rig.session.set_armed(voice, true).unwrap();
    assert!(open(&mut rig), "armed, it does");
    assert!(rig.session.input_status().needed);
    rig.session.set_armed(voice, false).unwrap();
    assert!(!open(&mut rig), "and no longer once it is disarmed");

    // Looking at the level opens it, for as long as anyone looks.
    rig.session.watch_input(true).unwrap();
    assert!(open(&mut rig));
    rig.host.hear(Hears::Sound(Arc::new(|channel, _| if channel == 0 { 0.25 } else { 1.5 })));
    rig.run(20);
    let levels = rig.session.input_levels();
    assert_eq!(levels, [(0.25, false), (1.5, true)], "the second channel is over full scale");
    rig.session.watch_input(false).unwrap();
    assert!(!open(&mut rig));
    assert!(rig.session.input_levels().is_empty());

    // A track that records a channel the device has not got is a sentence.
    let wide = TrackInput { channel: 2, stereo: true, monitor: Monitoring::Off };
    rig.session.edit(|project| project.set_track_input(voice, Some(wide))).unwrap();
    rig.session.set_armed(voice, true).unwrap();
    rig.session.housekeeping();
    let problem = rig.session.input_status().problem.expect("it cannot be recorded as it is");
    assert!(problem.contains("'Voice' records inputs 2 and 3") && problem.contains("2 channels"), "{problem}");
    let SessionError::Recording(refusal) = rig.session.record().unwrap_err() else { panic!("a refusal") };
    assert_eq!(refusal, problem);
    assert!(!rig.session.is_recording());

    // And so is a device that is not there.
    rig.host.remove_input("Microphone");
    rig.session.housekeeping();
    rig.session.edit(|project| project.set_track_input(voice, Some(TrackInput::mono(1)))).unwrap();
    let SessionError::Recording(refusal) = rig.session.record().unwrap_err() else { panic!("a refusal") };
    assert!(refusal.contains("the input cannot be opened") && refusal.contains("audio settings"), "{refusal}");
    assert_eq!(rig.session.measurement(), Measurement::Idle);
}

#[test]
fn every_pass_of_a_loop_is_a_take_that_begins_on_the_loops_first_frame_through_a_change_of_tempo() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    without_limiter(&mut rig);
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.0));
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    // 120 to the minute up to beat 12, and 90 from there: the loop, from
    // beat 8 to beat 16, is 96,000 frames and then 128,000.
    rig.session.edit(|project| project.set_tempo_at(12.0, 90.0, false)).unwrap();
    rig.session.edit(|project| project.set_loop(true, 8.0, 16.0)).unwrap();
    rig.session.seek(6.0);
    rig.run(40);

    // The song begins at beat 6, two beats ahead of the loop, and goes
    // round three and a bit times. A bump a thousand frames into every
    // pass, and one at beat 13 of each.
    let began = rig.now() + LATENCY;
    let pass_at = |pass: usize| if pass == 0 { 48_000.0 } else { 272_000.0 + (pass - 1) as f64 * 224_000.0 };
    bumps.at((began + 1000.0) / SR);
    for pass in 0..4 {
        bumps.at((began + pass_at(pass) + 1000.0) / SR);
        if pass < 3 {
            bumps.at((began + pass_at(pass) + 128_000.0) / SR);
        }
    }
    rig.session.record().unwrap();
    let until = rig.now() + pass_at(3) + 60_000.0;
    run_taking(&mut rig, until);
    rig.session.stop();

    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!(recorded.silent_frames, 0);
    let said: Vec<(f64, f64, bool)> = recorded
        .takes
        .iter()
        .map(|take| (take.start_beat, (take.length_beats * 100.0).round() / 100.0, take.chosen))
        .collect();
    // 60,000 frames at 120 are two and a half beats. The track plays the
    // last whole pass, and of the first what was sung before the loop.
    assert_eq!(said, [(6.0, 10.0, true), (8.0, 8.0, false), (8.0, 8.0, true), (8.0, 2.5, false)]);
    let track = rig.session.project().track(voice).unwrap().clone();
    assert_eq!(track.takes.iter().map(|take| take.lane).collect::<Vec<_>>(), [0, 1, 2, 3], "each in a lane of its own");
    assert_eq!(
        track.takes.iter().map(|take| take.clip.name.as_str()).collect::<Vec<_>>(),
        ["Take 1", "Take 2", "Take 3", "Take 4"]
    );
    let one_file =
        track.takes.iter().all(|take| take.clip.audio().unwrap().path == track.takes[0].clip.audio().unwrap().path);
    assert!(one_file, "one recording, as it was sung: a pass carries straight on from the one before it");

    // Each pass has its bumps where they were sung: the first a thousand
    // frames in, and at the loop's own first frame plus a thousand.
    let mut worst: f64 = 0.0;
    let mut check = |audio: &AudioContent, frames_in: f64, what: &str| {
        let off = bump_off(audio, frames_in);
        worst = worst.max(off.abs());
        assert!(off.abs() <= 1.0, "{what} is {off:.3} frames off");
    };
    let first = track.takes[0].clip.audio().unwrap();
    check(first, 1000.0, "the bump as the take began");
    check(first, 49_000.0, "the bump as the first pass came into the loop");
    check(first, 176_000.0, "the bump at beat 13 of the first pass");
    for pass in 1..4 {
        let audio = track.takes[pass].clip.audio().unwrap();
        check(audio, 1000.0, &format!("the bump as pass {pass} began"));
        if pass < 3 {
            check(audio, 128_000.0, &format!("the bump at beat 13 of pass {pass}"));
        }
    }
    println!("looped takes through a tempo change: at the most {worst:.3} frames from their place");

    // The track plays what led in to the loop, and the last pass that ran
    // the whole of it, faded across one another at the loop's start.
    let plays: Vec<(f64, f64, bool)> =
        track.clips.iter().map(|clip| (clip.start, clip.end(), clip.audio().unwrap().crossfade)).collect();
    assert_eq!(plays, [(6.0, 8.0, false), (8.0, 16.0, true)]);
    assert_eq!(track.clips[1].audio().unwrap().offset_seconds, track.takes[2].clip.audio().unwrap().offset_seconds);

    // And played round, the loop's first frame is that pass's first frame
    // every time: its bumps are heard a thousand and 128,000 frames after
    // the song comes round.
    rig.session.set_armed(voice, false).unwrap();
    rig.session.seek(8.0);
    rig.run(4);
    rig.host.keep_output();
    rig.host.take_output();
    rig.session.play();
    rig.run_until(rig.now() + 2.0 * 224_000.0 + 4800.0);
    let (left, _) = rig.host.take_output();
    for round in 0..2 {
        for frames_in in [1000.0, 128_000.0] {
            let at = round as f64 * 224_000.0 + frames_in;
            let from = at as usize - 64;
            let off = from as f64 + top_of(&left[from..from + 128]) - at;
            assert!(off.abs() <= 1.0, "round {round}: the bump {frames_in} frames in is heard {off:.3} frames off");
        }
    }
}

#[test]
fn a_take_that_is_punched_in_records_inside_the_loops_stretch_and_the_song_plays_through() {
    let (mut rig, folder) = rig_on(studio(microphone()), AudioConfig::default());
    without_limiter(&mut rig);
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.0));
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    let keys = rig.probe_track("Keys");
    rig.session.set_armed(keys, true).unwrap();
    // What the track has already: a tone over the first sixteen beats.
    let tone: Vec<f32> =
        (0..8 * 48_000).map(|n| (0.25 * (n as f64 * std::f64::consts::TAU * 500.0 / SR).sin()) as f32).collect();
    let before = folder.path().join("before.wav");
    let buffer = trill_core::audio::AudioBuffer { sample_rate: SR, left: tone.clone(), right: tone };
    trill_media::write_wav(&before, &buffer, trill_media::WavFormat::Float32).unwrap();
    let old = AudioContent::new(before.to_string_lossy());
    rig.session.edit(|project| project.add_audio_clip(voice, "Before", 0.0, 16.0, old.clone()).map(|_| ())).unwrap();
    rig.session.edit(|project| project.set_loop(true, 4.0, 8.0)).unwrap();
    rig.session.set_punch(true);
    rig.session.seek(2.0);
    rig.run(40);

    // From beat 2 to beat 10: bumps and keys before the punch, in it and
    // after it.
    let began = rig.now() + LATENCY;
    let at = |beat: f64| began + (beat - 2.0) * FRAMES_PER_BEAT;
    for beat in [3.0, 5.0, 7.0, 9.0] {
        bumps.at(at(beat) / SR);
    }
    rig.host.keep_output();
    rig.host.take_output();
    rig.session.record().unwrap();
    for (beat, pitch) in [(3.0, 60), (5.0, 62), (7.5, 64), (9.0, 65)] {
        rig.key(at(beat), on(pitch));
        rig.key(at(beat) + 6000.0, off(pitch));
    }
    rig.run_until(at(10.0));
    assert!(rig.session.transport().position_beats > 9.5, "the song played through its loop");
    rig.session.stop();
    let (left, _) = rig.host.take_output();

    // The take is the loop's stretch, and what was there is on either
    // side of it and kept whole in a lane.
    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!(recorded.takes.len(), 1);
    assert_eq!((recorded.takes[0].start_beat, recorded.takes[0].length_beats), (4.0, 4.0));
    let track = rig.session.project().track(voice).unwrap().clone();
    let plays: Vec<(f64, f64, &str)> =
        track.clips.iter().map(|clip| (clip.start, clip.end(), clip.name.as_str())).collect();
    assert_eq!(plays, [(0.0, 4.0, "Before"), (4.0, 8.0, "Take 1"), (8.0, 16.0, "Before")]);
    let kept: Vec<(&str, f64, f64)> =
        track.takes.iter().map(|take| (take.clip.name.as_str(), take.clip.start, take.clip.length)).collect();
    assert_eq!(kept, [("Take 1", 4.0, 4.0), ("Before", 0.0, 16.0)]);
    for beat in [5.0, 7.0] {
        let off = off_its_place(&rig.session, voice, beat * FRAMES_PER_BEAT);
        assert!(off.abs() <= 1.0, "the bump at beat {beat} is {off:.3} frames off");
    }
    // Only the keys played inside it are the take's, in a clip of its length.
    let notes = rig.session.project().track(keys).unwrap().clips.clone();
    assert_eq!(notes.len(), 1);
    assert_eq!((notes[0].start, notes[0].length), (4.0, 4.0));
    let played: Vec<(u8, f64)> =
        notes[0].notes().iter().map(|note| (note.pitch, (note.start * 100.0).round() / 100.0)).collect();
    assert_eq!(played, [(62, 1.0), (64, 3.5)]);

    // While it was recorded, what the track had was heard up to the punch
    // and after it, and not inside it. (The output's own frames: the song
    // was at beat 2 at the first of them.)
    let out = |beat: f64| ((beat - 2.0) * FRAMES_PER_BEAT) as usize;
    let level = |from: f64, to: f64| {
        let part = &left[out(from)..out(to)];
        (part.iter().map(|sample| sample * sample).sum::<f32>() / part.len() as f32).sqrt()
    };
    assert!(level(2.5, 3.9) > 0.15, "heard before the punch: {}", level(2.5, 3.9));
    assert!(level(4.1, 7.9) < 0.001, "not heard under the take: {}", level(4.1, 7.9));
    assert!(level(8.1, 9.5) > 0.15, "heard again after it: {}", level(8.1, 9.5));
    // And undone in one step, notes and audio alike.
    assert!(rig.session.undo());
    assert!(rig.session.project().track(keys).unwrap().clips.is_empty());
    assert_eq!(rig.session.project().track(voice).unwrap().clips.len(), 1);
    assert!(rig.session.project().track(voice).unwrap().takes.is_empty());
}

#[test]
fn what_the_devices_do_not_say_is_found_by_a_measurement_and_then_taken_off() {
    // A cable from the output to the input, and both devices slower than
    // they say: 113 frames more to the speaker, 317 more from the
    // microphone.
    let host = studio(ScriptedInput { unsaid: 317, ..microphone() });
    host.set_output_unsaid(113);
    let (mut rig, folder) = rig_on(host, AudioConfig::default());
    without_limiter(&mut rig);
    rig.host.hear(Hears::Output { gain: 0.5 });
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    // A track that plays bumps at frames of the song the test knows.
    let song_frames = [24_000.0, 60_000.0, 100_000.0];
    let mut clicks = vec![0.0f32; 4 * 48_000];
    for frame in song_frames {
        for (index, sample) in clicks.iter_mut().enumerate() {
            let away = (index as f64 - frame) / 3.0;
            if away.abs() < 20.0 {
                *sample += (0.8 * (-away * away).exp()) as f32;
            }
        }
    }
    let path = folder.path().join("clicks.wav");
    let buffer = trill_core::audio::AudioBuffer { sample_rate: SR, left: clicks.clone(), right: clicks };
    trill_media::write_wav(&path, &buffer, trill_media::WavFormat::Float32).unwrap();
    let played = rig.session.edit(|project| Ok(project.add_track("Clicks"))).unwrap();
    let content = AudioContent::new(path.to_string_lossy());
    rig.session
        .edit(|project| project.add_audio_clip(played, "Clicks", 0.0, 8.0, content.clone()).map(|_| ()))
        .unwrap();
    rig.run(40);

    let take = |rig: &mut Rig| -> Vec<f64> {
        rig.session.seek(0.0);
        rig.run(4);
        rig.session.record().unwrap();
        rig.run_until(rig.now() + 5.0 * FRAMES_PER_BEAT);
        rig.session.stop();
        // Looked for half a second either side of where it belongs.
        let (_, _, _, audio) = clip_at(&rig.session, voice, 0.5);
        let found = |frame: &f64| {
            let in_file = audio.offset_seconds * SR + frame;
            let from = in_file.round() as u64 - 1000;
            from as f64 + top_of(&frames_of(Path::new(&audio.path), 1, from, 2000)) - in_file
        };
        song_frames.iter().map(found).collect()
    };
    // By what the devices say, everything is late by what they leave out.
    let before = take(&mut rig);
    for off in &before {
        assert!((off - 430.0).abs() <= 1.0, "late by what the devices do not say: {before:?}");
    }
    let said = rig.session.recording_latency().unwrap();
    assert_eq!((said.measured_ms, said.adjust_ms, said.correction), (None, 0.0, Correction::None));

    // Measured: the noise comes back 430 frames later than the devices
    // account for.
    rig.session.measure_latency(1).unwrap();
    assert_eq!(rig.session.measurement(), Measurement::Running);
    assert!(rig.session.record().is_err(), "nothing is recorded while it runs");
    rig.run_until(rig.now() + 8.0 * SR);
    rig.session.housekeeping();
    let Measurement::Done(measured) = rig.session.measurement() else { panic!("{:?}", rig.session.measurement()) };
    assert!((measured.more_than_said_ms * 48.0 - 430.0).abs() < 0.5, "{measured:?}");
    let longer = measured.round_trip_ms - said.round_trip_ms;
    assert!((longer - measured.more_than_said_ms).abs() < 0.02, "{measured:?} {said:?}");
    // What it offers is the round trip less Trill's own part of it.
    let beyond = measured.round_trip_ms - said.handover_ms - said.input_ms;
    assert!((measured.beyond_ms - beyond).abs() < 0.02, "{measured:?} {said:?}");
    assert!(measured.likeness > 0.98 && (measured.heard_db - -26.0).abs() < 0.5, "{measured:?}");
    // It is offered, and changes nothing until it is taken.
    assert_eq!(rig.session.latency_correction(), (None, 0.0));
    rig.session.set_measured_latency(Some(measured.beyond_ms));
    let now = rig.session.recording_latency().unwrap();
    assert_eq!(now.correction, Correction::Measured);
    assert!((now.round_trip_ms - measured.round_trip_ms).abs() < 0.02);

    let after = take(&mut rig);
    for off in &after {
        assert!(off.abs() <= 1.0, "in its place once the measurement is taken off: {after:?}");
    }
    assert_eq!(rig.session.last_recording().unwrap().correction, Correction::Measured);
    println!(
        "placement by what the devices say: {before:.2?} frames late; measured {:.3} ms more than they say, then {after:.2?}",
        measured.more_than_said_ms
    );

    // A figure added by hand counts on top of either, and both can be
    // taken away.
    rig.session.set_latency_adjustment(5.0);
    assert_eq!(rig.session.latency_correction(), (Some(measured.beyond_ms), 5.0));
    assert!((rig.session.recording_latency().unwrap().round_trip_ms - measured.round_trip_ms - 5.0).abs() < 0.02);
    rig.session.set_measured_latency(None);
    assert_eq!(rig.session.recording_latency().unwrap().correction, Correction::ByHand);
    rig.session.set_latency_adjustment(0.0);
    assert_eq!(rig.session.latency_correction(), (None, 0.0));

    // With nothing between the output and the input, the measurement says
    // so, and what to do.
    rig.host.hear(Hears::Nothing);
    rig.session.measure_latency(1).unwrap();
    rig.run_until(rig.now() + 8.0 * SR);
    rig.session.housekeeping();
    let Measurement::Failed(why) = rig.session.measurement() else { panic!("{:?}", rig.session.measurement()) };
    assert!(why.contains("did not come back on input 1") && why.contains("cable"), "{why}");
    assert!(rig.session.measure_latency(3).unwrap_err().to_string().contains("channels 1 to 2"));
}

#[test]
fn an_input_that_goes_away_ends_the_take_whole_up_to_there_and_the_song_plays_on() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.0));
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    rig.run(40);
    let began = rig.now() + LATENCY;
    bumps.at((began + 12_000.0) / SR);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 48_000.0);
    rig.host.remove_input("Microphone");
    rig.run_until(rig.now() + 48_000.0);
    rig.session.housekeeping();

    assert!(!rig.session.is_recording(), "the take is over");
    assert!(rig.session.transport().playing, "and the song plays on");
    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!(recorded.takes.len(), 1);
    let why = recorded.ended_early.expect("it ended before it was stopped");
    assert!(why.contains("'Microphone' was disconnected") && why.contains("whole up to that moment"), "{why}");
    assert_eq!(rig.session.take_recording_notice(), Some(why));
    assert_eq!(rig.session.take_recording_notice(), None, "said once");
    let off = off_its_place(&rig.session, voice, 12_000.0);
    assert!(off.abs() <= 1.0, "what was sung before is where it was sung: {off:.3}");
    let problem = rig.session.input_status().problem.expect("the input is gone");
    assert!(problem.contains("Microphone") || problem.contains("cannot be opened"), "{problem}");
    rig.session.stop();

    // A stream that is opened anew under a take ends it the same way.
    // (The device is back; arming the track again looks for it at once.)
    rig.host.add_input(microphone());
    rig.session.set_armed(voice, false).unwrap();
    rig.session.set_armed(voice, true).unwrap();
    rig.session.housekeeping();
    assert!(rig.session.input_status().open.is_some(), "{:?}", rig.session.input_status());
    rig.session.seek(0.0);
    settle(&mut rig);
    let began = rig.now() + LATENCY;
    bumps.at((began + 12_000.0) / SR);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 48_000.0);
    // The output is made again under the take: taken for the program
    // alone, with another buffer, at the rate it had.
    rig.session.set_audio_config(AudioConfig { exclusive: true, buffer_size: Some(256), ..AudioConfig::default() });
    assert!(!rig.session.is_recording());
    let notice = rig.session.take_recording_notice().expect("it is said");
    assert!(notice.contains("the audio device changed") && notice.contains("whole up to that moment"), "{notice}");
    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!(recorded.takes.len(), 1, "whole up to there");
    assert!(recorded.takes[0].seconds > 0.9, "a second of it had been sung: {:?}", recorded.takes[0]);
    let off = off_its_place(&rig.session, voice, 12_000.0);
    assert!(off.abs() <= 1.0, "and what was sung is where it was sung: {off:.3}");
    // The song plays on, through the stream that came.
    let position = rig.session.transport().position_beats;
    rig.run(40);
    let status = rig.session.transport();
    assert!(status.playing, "the song was not stopped for it");
    assert!(status.position_beats > position + 0.2, "{} after {position}", status.position_beats);
}

/// A tone for an input to hear, a quarter of full scale.
fn tone() -> Hears {
    Hears::Sound(Arc::new(|_, seconds| (0.25 * (seconds * std::f64::consts::TAU * 440.0).sin()) as f32))
}

/// Records a take of four seconds on a rig whose files are held to
/// `limits`, and returns the rig once the take has ended by itself, with
/// the track it was on.
fn a_take_cut_short_by(limits: trill_media::GrowingLimits) -> (Rig, TrackId, tempfile::TempDir) {
    let (mut rig, folder) = rig_on(studio(microphone()), AudioConfig::default());
    rig.host.hear(tone());
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    rig.run(40);
    rig.session.hold_recordings_to(limits);
    rig.session.seek(0.0);
    rig.run(4);
    rig.session.record().unwrap();
    // Four seconds are asked for; the file takes fewer, and the session
    // comes to know at a round of its chores.
    let until = rig.now() + 4.0 * SR;
    run_taking(&mut rig, until);
    for _ in 0..400 {
        if !rig.session.is_recording() {
            break;
        }
        rig.run(4);
        rig.session.housekeeping();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!rig.session.is_recording(), "the take ended by itself");
    (rig, voice, folder)
}

/// What every take that a file cut short has to be: on its track, as
/// long as the file and no longer, the file reading back whole with the
/// tone in it to its last frame, and the song playing on.
fn whole_up_to_there(rig: &mut Rig, voice: TrackId, holds: std::ops::RangeInclusive<u64>, why: &str) {
    let recorded = rig.session.last_recording().unwrap().clone();
    let said = recorded.ended_early.clone().expect("it ended before it was stopped");
    assert!(said.contains(why) && said.contains("the take ends there, whole up to that moment"), "{said}");
    assert_eq!(rig.session.take_recording_notice(), Some(said));
    assert_eq!(recorded.takes.len(), 1);
    // The file reads, and holds what its header says: all that was put
    // on disk before it could take no more.
    let file = recorded.takes[0].file.clone();
    let audio = trill_media::MediaCache::default().audio(&file).expect("the file reads");
    let frames = audio.frames() as u64;
    assert!(holds.contains(&frames), "the file holds {frames} frames, not {holds:?}");
    let last = &audio.left[audio.frames() - 24_000..];
    let loudest = last.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!((loudest - 0.25).abs() < 0.01, "the tone is in it to its end: {loudest}");
    assert!((pitch_of(last) - 440.0).abs() < 3.0);
    // The take is as long as the file, to the frame, and no longer.
    let (_, start, length, clip) = clip_at(&rig.session, voice, 0.5);
    let ends_in_file = clip.offset_seconds * SR + (start + length) * FRAMES_PER_BEAT - start * FRAMES_PER_BEAT;
    assert!(ends_in_file <= frames as f64 + 0.5, "the clip ends at frame {ends_in_file} of a file of {frames}");
    assert!(ends_in_file >= frames as f64 - 2.0, "and holds all of it: {ends_in_file} of {frames}");
    assert_eq!(rig.session.project().track(voice).unwrap().takes.len(), 1, "kept as a take, like any other");
    // The song plays on.
    let position = rig.session.transport().position_beats;
    rig.run(40);
    let status = rig.session.transport();
    assert!(status.playing && status.position_beats > position + 0.2, "the song plays on: {status:?}");
}

#[test]
fn a_disk_that_fills_ends_the_take_whole_up_to_there_and_the_song_plays_on() {
    // The disk has room for two and a half seconds of it: the header's 56
    // bytes and 120,000 frames of four bytes. A second at a time is put on
    // disk, so two seconds are there when the third finds no room.
    let limits = trill_media::GrowingLimits { fails_at: Some(56 + 120_000 * 4), ..Default::default() };
    let (mut rig, voice, _folder) = a_take_cut_short_by(limits);
    // (A second, and what the writer had in hand when the second was full.)
    whole_up_to_there(&mut rig, voice, 96_000..=119_999, "the disk is full");
}

#[test]
fn a_file_as_long_as_a_file_can_be_ends_the_take_whole_up_to_there_and_the_song_plays_on() {
    // A file may be 100,000 frames here, and four gigabytes as it comes.
    let limits = trill_media::GrowingLimits { most_bytes: 56 + 100_000 * 4, ..Default::default() };
    let (mut rig, voice, _folder) = a_take_cut_short_by(limits);
    whole_up_to_there(&mut rig, voice, 100_000..=100_000, "as long as one file can be");
}

#[test]
fn frames_that_never_arrive_are_silence_of_their_length_and_what_follows_is_in_its_place() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.25));
    let voice = armed_track(&mut rig, "Voice", TrackInput { channel: 1, stereo: true, monitor: Monitoring::Off });
    rig.run(40);
    let began = rig.now() + LATENCY;
    for frame in [12_000.0, 96_000.0] {
        bumps.at((began + frame) / SR);
    }
    rig.session.record().unwrap();
    // For three tenths of a second the device hands nothing over.
    rig.run_until(rig.now() + 30_000.0);
    rig.host.stall_input(true);
    rig.run_until(rig.now() + 14_400.0);
    rig.host.stall_input(false);
    rig.run_until(rig.now() + 96_000.0);
    rig.session.stop();

    let recorded = rig.session.last_recording().unwrap().clone();
    assert!(
        recorded.silent_frames >= 14_400 && recorded.silent_frames < 16_000,
        "{} frames were silent",
        recorded.silent_frames
    );
    for frame in [12_000.0, 96_000.0] {
        // (Two channels side by side: the first of them.)
        let (_, start, _, audio) = clip_at(&rig.session, voice, 0.1);
        let in_file = audio.offset_seconds * SR + frame - start * FRAMES_PER_BEAT;
        let near = frames_of(Path::new(&audio.path), 2, in_file.round() as u64 - 64, 128);
        let first: Vec<f32> = near.chunks_exact(2).map(|pair| pair[0]).collect();
        let off = in_file.round() - 64.0 + top_of(&first) - in_file;
        assert!(off.abs() <= 1.0, "the bump at frame {frame} is {off:.3} frames off");
    }
    // The second channel heard a level that stood still: it is that, and
    // nothing where nothing arrived.
    let (_, _, _, audio) = clip_at(&rig.session, voice, 0.1);
    let at = |frame: f64| frames_of(Path::new(&audio.path), 2, (audio.offset_seconds * SR + frame) as u64, 1)[1];
    assert!((at(20_000.0) - 0.25).abs() < 1e-4 && (at(80_000.0) - 0.25).abs() < 1e-4);
    assert_eq!(at(37_000.0), 0.0, "silence where the device handed nothing over");
}

#[test]
fn a_songs_recordings_come_beside_it_when_it_is_saved_and_the_unused_ones_can_be_let_go_of() {
    let (mut rig, folder) = rig_on(studio(microphone()), AudioConfig::default());
    rig.host.hear(Hears::Sound(Arc::new(|_, seconds| (0.3 * (seconds * std::f64::consts::TAU * 440.0).sin()) as f32)));
    let voice = armed_track(&mut rig, "Lead Vox", TrackInput::mono(2));
    rig.run(40);
    let record = |rig: &mut Rig, frames: f64| {
        rig.session.seek(0.0);
        rig.run(4);
        rig.session.record().unwrap();
        rig.run_until(rig.now() + frames);
        rig.session.stop();
        rig.session.last_recording().unwrap().takes[0].file.clone()
    };
    // A folder of this song's own, in the one the program keeps for
    // songs that have no place yet.
    let unsaved = rig.session.recordings_folder();
    assert!(unsaved.starts_with(folder.path().join("unsaved")) && unsaved != folder.path().join("unsaved"));
    let first = record(&mut rig, 48_000.0);
    assert_eq!(first, unsaved.join("Lead Vox.wav"), "an unsaved song's takes are in the program's own folder");
    assert_eq!(rig.session.recordings_folder(), unsaved);

    // A copy kept for safety while a second take is recorded has that
    // take in it as far as it has been written, and the file reads.
    rig.session.seek(0.0);
    rig.run(4);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 3.0 * SR);
    let growing = rig.session.growing_takes()[0].file.clone();
    wait_until(|| growing.frames() > 100_000);
    let copy = trill_core::Project::from_json(&rig.session.snapshot_json()).unwrap();
    let in_copy = copy.track(voice).unwrap();
    assert_eq!(in_copy.takes.len(), 2, "the take that is being recorded is in the copy");
    let so_far = in_copy
        .clips
        .iter()
        .find(|clip| clip.audio().is_some_and(|audio| audio.path.ends_with("Lead Vox 2.wav")))
        .unwrap();
    assert!(so_far.length > 3.5 && so_far.length < 6.5, "as far as it had got: {} beats", so_far.length);
    let as_it_was = folder.path().join("as a program that died left it.wav");
    std::fs::copy(growing.path(), &as_it_was).unwrap();
    let read = trill_media::decode(&as_it_was).unwrap();
    assert!(read.frames() > 50_000 && read.left.iter().any(|sample| sample.abs() > 0.2), "{} frames", read.frames());
    assert_eq!(
        rig.session.project().track(voice).unwrap().takes.len(),
        1,
        "and the song is as it was until the take ends"
    );
    rig.session.stop();
    let second = rig.session.last_recording().unwrap().takes[0].file.clone();
    assert_eq!(second, unsaved.join("Lead Vox 2.wav"));

    // Saved: both recordings are beside the song, the song and every
    // state undo can reach name them there, and nothing is left behind.
    let song = folder.path().join("songs").join("My Song.trill");
    rig.session.save(&song).unwrap();
    let beside = folder.path().join("songs").join("My Song files").join("takes");
    let paths = |session: &Session| -> Vec<String> {
        let track = session.project().track(voice).unwrap();
        track.takes.iter().map(|take| take.clip.audio().unwrap().path.clone()).collect()
    };
    let (new_first, new_second) = (beside.join("Lead Vox.wav"), beside.join("Lead Vox 2.wav"));
    assert_eq!(paths(&rig.session), [new_first.to_string_lossy(), new_second.to_string_lossy()]);
    assert!(new_first.is_file() && new_second.is_file());
    assert!(!first.exists() && !second.exists(), "nothing is left where they were");
    assert!(std::fs::read_to_string(&song).unwrap().contains("My Song files"));
    assert!(rig.session.undo(), "the second take is undone");
    assert_eq!(paths(&rig.session), [new_first.to_string_lossy()], "the state before names the file where it is now");
    assert!(rig.session.redo());
    assert_eq!(rig.session.recordings_folder(), beside, "and the next take is written beside the song");
    assert!(rig.session.unused_recordings().is_empty(), "undo can reach both");

    // A take that is removed leaves its file; once no state names it, it
    // is listed with its size, and deleted when that is asked for.
    let third = record(&mut rig, 24_000.0);
    assert_eq!(third, beside.join("Lead Vox 3.wav"));
    assert!(rig.session.undo());
    rig.session.edit(|project| project.set_track_volume(voice, -3.0)).unwrap();
    let unused = rig.session.unused_recordings();
    assert_eq!(unused.len(), 1, "{unused:?}");
    assert_eq!(unused[0].0, third);
    assert!(unused[0].1 > 24_000 * 4, "its size in bytes: {}", unused[0].1);
    let nobody_else = |_: &Path| None;
    let refusal =
        rig.session.delete_unused_recordings(std::slice::from_ref(&new_first), &nobody_else).unwrap_err().to_string();
    assert!(refusal.contains("nothing was deleted") && refusal.contains("Lead Vox 3.wav"), "{refusal}");
    assert!(new_first.is_file());
    // One that whoever keeps the session says another song plays is
    // refused with that song's name: this song is not the only one.
    let another = |path: &Path| (path == third).then(|| "'My Song v2'".to_string());
    let refusal = rig.session.delete_unused_recordings(std::slice::from_ref(&third), &another).unwrap_err().to_string();
    assert!(refusal.contains("is played by 'My Song v2'") && refusal.contains("nothing was removed"), "{refusal}");
    assert!(third.is_file());
    // Removed, it is out of the song's folder and not out of existence:
    // in a folder beside the takes, where a run that keeps nothing of
    // the user's puts it, with all it held.
    let held = std::fs::read(&third).unwrap();
    assert_eq!(rig.session.removed_recordings_go(), format!("to {}", beside.join("removed").display()));
    let removed = rig.session.delete_unused_recordings(std::slice::from_ref(&third), &nobody_else).unwrap();
    assert_eq!((removed.bytes, removed.recycled), (unused[0].1, 0));
    assert_eq!(removed.set_aside, [beside.join("removed").join("Lead Vox 3.wav")]);
    assert!(!third.exists() && rig.session.unused_recordings().is_empty());
    assert_eq!(std::fs::read(&removed.set_aside[0]).unwrap(), held, "not a byte of it is lost");
    // What was set aside is not listed again, and the name is free for
    // the next take.
    assert_eq!(record(&mut rig, 24_000.0), third);
}

#[test]
fn a_take_is_heard_in_place_before_it_is_chosen_without_the_song_being_changed() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    without_limiter(&mut rig);
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    rig.run(40);
    // Two takes over the same four beats: a low tone, then a high one.
    for hz in [500.0, 1500.0] {
        rig.host.hear(Hears::Sound(Arc::new(move |_, seconds| {
            (0.3 * (seconds * std::f64::consts::TAU * hz).sin()) as f32
        })));
        rig.session.seek(0.0);
        rig.run(8);
        rig.session.record().unwrap();
        rig.run_until(rig.now() + 4.0 * FRAMES_PER_BEAT);
        rig.session.stop();
    }
    rig.session.set_armed(voice, false).unwrap();
    let track = rig.session.project().track(voice).unwrap().clone();
    let (low, high) = (track.takes[0].clip.id, track.takes[1].clip.id);
    assert_eq!(track.clips.len(), 1, "the second take took the first one's place");
    let heard = |rig: &mut Rig| {
        rig.session.seek(0.0);
        rig.run(4);
        rig.host.keep_output();
        rig.host.take_output();
        rig.session.play();
        rig.run_until(rig.now() + 72_000.0);
        rig.session.stop();
        pitch_of(&rig.host.take_output().0[12_000..60_000])
    };
    assert!((heard(&mut rig) - 1500.0).abs() < 5.0, "the track plays the second take");
    let before = rig.session.project().clone();
    let depth = rig.session.undo_step();
    rig.session.hear_take(Some(low)).unwrap();
    assert_eq!(rig.session.heard_take(), Some(low));
    assert!((heard(&mut rig) - 500.0).abs() < 5.0, "the first is heard in its place");
    assert_eq!(rig.session.project(), &before, "and the song is as it was");
    assert_eq!(rig.session.undo_step(), depth, "with nothing to undo");
    rig.session.hear_take(None).unwrap();
    assert!((heard(&mut rig) - 1500.0).abs() < 5.0);
    assert!(rig.session.hear_take(Some(ClipId(9999))).is_err());
    // Chosen, it is the song's: one step.
    rig.session.edit(|project| project.choose_take(low).map(|_| ())).unwrap();
    assert!((heard(&mut rig) - 500.0).abs() < 5.0);
    let _ = high;
}

/// Records thirty minutes of the song from an input whose clock runs
/// `ppm` parts a million fast: a bump every seven seconds on the first
/// channel, each at a frame of the song the test knows, and a sine of 1 kHz
/// on the second. Returns how far the worst bump is from its place, in
/// milliseconds (of all of them, and of those after the first three
/// seconds), and how far the sine is from its pitch at the worst, in
/// cents, from two seconds in.
fn thirty_minutes(ppm: f64) -> (f64, f64, f64) {
    const MINUTES: f64 = 30.0;
    const EVERY: f64 = 7.0 * SR;
    let host = studio(ScriptedInput { ppm, ..microphone() });
    let (mut rig, _folder) = rig_on(host, AudioConfig::default());
    let voice = armed_track(&mut rig, "Voice", TrackInput { channel: 1, stereo: true, monitor: Monitoring::Off });
    rig.run(40);
    // The song's frame 24,000 is heard at this moment of the host's clock,
    // and every seven seconds after it another.
    let first = (rig.now() + LATENCY + 24_000.0) / SR;
    rig.host.hear(Hears::Sound(Arc::new(move |channel, seconds| {
        if channel == 1 {
            return (0.5 * (seconds * std::f64::consts::TAU * 1000.0).sin()) as f32;
        }
        let since = seconds - first;
        let nearest = (since / 7.0).round().max(0.0) * 7.0;
        let away = (since - nearest) * SR / 3.0;
        if away.abs() < 20.0 { (0.8 * (-away * away).exp()) as f32 } else { 0.0 }
    })));
    rig.session.record().unwrap();
    let until = rig.now() + MINUTES * 60.0 * SR + SR;
    run_taking(&mut rig, until);
    rig.session.stop();
    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!(recorded.silent_frames, 0, "nothing was late or lost");
    let (_, start, length, audio) = clip_at(&rig.session, voice, 1.0);
    assert_eq!(start, 0.0);
    assert!(length >= MINUTES * 120.0, "thirty minutes at 120 to the minute: {length} beats");

    // The bumps, each where the song has its frame.
    let path = Path::new(&audio.path);
    let (mut worst, mut settled) = (0.0f64, 0.0f64);
    let mut bump = 0.0;
    while 24_000.0 + bump * EVERY < MINUTES * 60.0 * SR {
        let song_frame = 24_000.0 + bump * EVERY;
        let in_file = audio.offset_seconds * SR + song_frame;
        let near = frames_of(path, 2, in_file.round() as u64 - 64, 128);
        let left: Vec<f32> = near.chunks_exact(2).map(|pair| pair[0]).collect();
        let off_ms = (in_file.round() - 64.0 + top_of(&left) - in_file) / SR * 1000.0;
        assert!(off_ms.abs() < 0.5, "{ppm} ppm: the click at {:.0} s is {off_ms:.4} ms off", song_frame / SR);
        worst = worst.max(off_ms.abs());
        if song_frame > 3.0 * SR {
            settled = settled.max(off_ms.abs());
        }
        bump += 1.0;
    }

    // The sine, a fifth of a second at a time from two seconds in: how far
    // its phase moves from one half of that to the other is how far it is
    // from 1 kHz.
    let mut file = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path).unwrap());
    let from = (audio.offset_seconds * SR).round() as u64 + 2 * 48_000;
    file.seek(SeekFrom::Start(56 + from * 8)).unwrap();
    let turn: Vec<(f64, f64)> = (0..48).map(|n| (n as f64 * std::f64::consts::TAU / 48.0).sin_cos()).collect();
    let mut bytes = vec![0u8; 9600 * 8];
    let mut cents = 0.0f64;
    let mut windows = 0;
    while file.read_exact(&mut bytes).is_ok() && (windows as f64) < (MINUTES * 60.0 - 3.0) * 5.0 {
        let mut halves = [(0.0f64, 0.0f64); 2];
        for (frame, pair) in bytes.chunks_exact(8).enumerate() {
            let sample = f32::from_le_bytes(pair[4..8].try_into().unwrap()) as f64;
            let (sin, cos) = turn[frame % 48];
            let half = &mut halves[frame / 4800];
            (half.0, half.1) = (half.0 + sample * cos, half.1 - sample * sin);
        }
        // The second half's phase against the first's, a tenth of a second on.
        let (re, im) = (
            halves[1].0 * halves[0].0 + halves[1].1 * halves[0].1,
            halves[1].1 * halves[0].0 - halves[1].0 * halves[0].1,
        );
        let hertz_off = im.atan2(re) / (std::f64::consts::TAU * 0.1);
        let off = 1200.0 * (1.0 + hertz_off / 1000.0).log2();
        assert!(off.abs() < 1.0, "{ppm} ppm: {off:.4} cents off, {:.1} s in", 2.0 + windows as f64 * 0.2);
        cents = cents.max(off.abs());
        windows += 1;
    }
    assert!(windows > 8000, "the whole of it was looked at: {windows} stretches");
    (worst, settled, cents)
}

#[test]
fn thirty_minutes_from_an_input_whose_clock_is_fast_stay_in_place_and_in_tune() {
    let (worst, settled, cents) = thirty_minutes(200.0);
    println!(
        "200 ppm fast, thirty minutes: clicks within {worst:.4} ms ({settled:.4} ms after three seconds), the sine within {cents:.4} cents"
    );
    assert!(settled < 0.05, "{settled}");
}

#[test]
fn thirty_minutes_from_an_input_whose_clock_is_slow_stay_in_place_and_in_tune() {
    let (worst, settled, cents) = thirty_minutes(-200.0);
    println!(
        "200 ppm slow, thirty minutes: clicks within {worst:.4} ms ({settled:.4} ms after three seconds), the sine within {cents:.4} cents"
    );
    assert!(settled < 0.05, "{settled}");
}

/// A song with inputs that are heard through tracks of their own, each
/// with an effect, beside synths that play: made on `threads` threads from
/// its first block, played for a while and recorded. Returns what came
/// out, and how many blocks were made on more than one thread.
fn heard_on(threads: usize) -> (Vec<f32>, Vec<f32>, u64) {
    use trill_core::Note;
    let host = studio(ScriptedInput { ppm: 120.0, channels: 4, ..microphone() });
    host.hear(Hears::Sound(Arc::new(|channel, seconds| {
        (0.2 * (seconds * (900.0 + 700.0 * channel as f64)).sin()) as f32
    })));
    let (mut rig, _folder) = rig_on(host, AudioConfig::default());
    rig.host.keep_output();
    rig.session.set_play_threads(Some(threads));
    rig.session.spread_from(0.0);
    for (index, effect) in ["builtin:reverb", "builtin:eq", "builtin:filter"].iter().enumerate() {
        let track = rig.session.edit(|project| Ok(project.add_track(format!("In {index}")))).unwrap();
        let input = TrackInput { channel: index as u16 + 1, stereo: index == 2, monitor: Monitoring::Always };
        rig.session.edit(|project| project.set_track_input(track, Some(input))).unwrap();
        rig.session.add_device(ChainRef::Track(track), effect, None, None).unwrap();
    }
    for index in 0..2u8 {
        let synth = rig.sine_track("Synth");
        rig.session
            .edit(|project| {
                let clip = project.add_midi_clip(synth, "clip", 0.0, 8.0)?;
                let notes: Vec<Note> = (0..8)
                    .map(|step| Note {
                        pitch: 50 + index * 5,
                        start: f64::from(step),
                        length: 0.5,
                        velocity: 100,
                        ..Default::default()
                    })
                    .collect();
                project.clip_mut(clip)?.add_notes(&notes)
            })
            .unwrap();
    }
    // The input is opened and the threads are made by the session's own
    // chores, a few buffers in.
    rig.run(2);
    rig.session.housekeeping();
    rig.run(2);
    rig.session.housekeeping();
    assert!(rig.session.input_status().open.is_some());
    rig.session.take_load();
    rig.host.take_output();
    rig.session.play();
    rig.run(700);
    let (left, right) = rig.host.take_output();
    assert_eq!(rig.session.audio_faults(), 0);
    (left, right, rig.session.take_load().spread_blocks)
}

#[test]
fn a_song_with_inputs_that_are_heard_is_the_same_samples_on_one_thread_and_on_several() {
    let (left, right, spread) = heard_on(1);
    assert_eq!(spread, 0);
    assert!(left.iter().any(|sample| sample.abs() > 0.05), "the inputs and the synths are heard");
    assert!(left != right, "a pair is left and right");
    for threads in [2, 4] {
        let (also_left, also_right, spread) = heard_on(threads);
        assert!(spread > 500, "{threads} threads: the song was spread over them ({spread} blocks)");
        assert!(also_left == left && also_right == right, "{threads} threads: other samples");
    }
}

/// How much of a buffer's time the callback takes, on average, for a song
/// of eight tracks at 128 frames a buffer: each recording a channel of the
/// input and letting it through, or (`plain`) neither.
fn share_of_a_buffer(plain: bool) -> f64 {
    let host = studio(ScriptedInput { channels: 8, ..microphone() });
    host.hear(Hears::Sound(Arc::new(|channel, seconds| {
        (0.2 * (seconds * (900.0 + 300.0 * channel as f64)).sin()) as f32
    })));
    let config = AudioConfig { exclusive: true, buffer_size: Some(128), ..AudioConfig::default() };
    let (mut rig, _folder) = rig_on(host, config);
    rig.session.set_play_threads(Some(1));
    for index in 0..8u16 {
        let track = rig.session.edit(|project| Ok(project.add_track(format!("In {index}")))).unwrap();
        if !plain {
            let input = TrackInput { channel: index + 1, stereo: false, monitor: Monitoring::Always };
            rig.session.edit(|project| project.set_track_input(track, Some(input))).unwrap();
            rig.session.set_armed(track, true).unwrap();
        }
    }
    rig.session.housekeeping();
    assert_eq!(rig.session.input_status().open.is_some(), !plain);
    rig.run(400);
    if !plain {
        rig.session.record().unwrap();
    } else {
        rig.session.play();
    }
    rig.run(400);
    rig.session.take_load();
    // Three seconds of the song, in buffers of 128 frames.
    rig.run(1125);
    let load = rig.session.take_load();
    rig.session.stop();
    assert_eq!(load.smallest_frames, 128, "{load:?}");
    f64::from(load.mean)
}

#[test]
fn recording_eight_inputs_and_hearing_them_costs_a_small_share_of_a_buffer_of_128_frames() {
    // Measured three times, the least of them: what else the machine is
    // doing only ever adds.
    let measure = |plain: bool| (0..3).map(|_| share_of_a_buffer(plain)).fold(f64::MAX, f64::min);
    let (without, with) = (measure(true), measure(false));
    let cost = (with - without).max(0.0);
    println!(
        "eight inputs recorded and heard at 128 frames: {:.2} % of a buffer's time ({:.1} microseconds), on {:.2} % for the song without them",
        cost * 100.0,
        cost * 128.0 / SR * 1e6,
        without * 100.0
    );
    assert!(cost < 0.08, "eight inputs take {:.1} % of a buffer's time", cost * 100.0);
}

// ---- nothing that was recorded lands nowhere -----------------------------------

/// A rig with a track that records, armed, whose input hears bumps; the
/// song counts nothing in unless a test says so.
fn rolling_rig() -> (Rig, TrackId, Bumps, tempfile::TempDir) {
    let (mut rig, folder) = rig_on(studio(microphone()), AudioConfig::default());
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.0));
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    rig.run(40);
    (rig, voice, bumps, folder)
}

/// How many frames of the device have gone by while the song rolled, as
/// the test counts them: the take is asked for now, and the next buffer
/// is the first that rolls.
struct Rolled {
    began: f64,
}

impl Rolled {
    fn start(rig: &mut Rig) -> Self {
        rig.session.record().unwrap();
        Self { began: rig.now() }
    }

    /// Stops the take and says how many frames the song rolled for.
    fn stop(self, rig: &mut Rig) -> f64 {
        let frames = rig.now() - self.began;
        rig.session.stop();
        frames
    }
}

/// Every take the last recording left, as (start beat, length in frames,
/// on the track), and the sum of their lengths in frames: what was sung
/// while the song rolled, all of which has to be in one of them.
fn landed(rig: &Rig) -> (Vec<(f64, f64, bool)>, f64) {
    let recorded = rig.session.last_recording().expect("a recording").clone();
    assert!(recorded.ended_early.is_none(), "{:?}", recorded.ended_early);
    let takes: Vec<(f64, f64, bool)> =
        recorded.takes.iter().map(|take| (take.start_beat, take.seconds * SR, take.chosen)).collect();
    let sum = takes.iter().map(|take| take.1).sum();
    (takes, sum)
}

/// The file of the last recording is in use by the song: it is not among
/// the recordings that could be removed.
fn in_use(rig: &Rig) {
    let recorded = rig.session.last_recording().unwrap();
    assert!(!recorded.takes.is_empty(), "nothing landed");
    let unused = rig.session.unused_recordings();
    assert!(unused.is_empty(), "a recording whose take is in the song is offered for removal: {unused:?}");
}

#[test]
fn a_take_sung_past_a_loop_that_is_on_lands_where_it_was_sung() {
    // The loop is on over the first two bars, and the song stands at bar
    // five: it never comes round, and a take sung there is a take there.
    let (mut rig, voice, bumps, _folder) = rolling_rig();
    rig.session.edit(|project| project.set_loop(true, 0.0, 8.0)).unwrap();
    rig.session.seek(16.0);
    rig.run(4);
    let began = rig.now() + LATENCY;
    bumps.at((began + 30_000.0) / SR);
    let rolled = Rolled::start(&mut rig);
    rig.run_until(rig.now() + 3.0 * SR);
    let frames = rolled.stop(&mut rig);

    let (takes, sum) = landed(&rig);
    assert_eq!(takes.len(), 1, "{takes:?}");
    assert_eq!((takes[0].0, takes[0].2), (16.0, true), "it lies where the song was, and is on the track");
    assert!((sum - frames).abs() <= 2.0, "{sum} frames in takes of {frames} sung");
    let off = off_its_place(&rig.session, voice, 16.0 * FRAMES_PER_BEAT + 30_000.0);
    assert!(off.abs() <= 1.0, "{off:.3}");
    in_use(&rig);
}

#[test]
fn a_song_that_is_sent_elsewhere_during_a_take_ends_one_stretch_and_begins_another() {
    let (mut rig, voice, bumps, _folder) = rolling_rig();
    // From bar three; a second in, the song is sent back to bar one.
    rig.session.seek(8.0);
    rig.run(4);
    let began = rig.now() + LATENCY;
    bumps.at((began + 12_000.0) / SR);
    let rolled = Rolled::start(&mut rig);
    rig.run_until(rig.now() + SR);
    rig.session.seek(0.0);
    let jumped = rig.now() + LATENCY;
    bumps.at((jumped + 20_000.0) / SR);
    rig.run_until(rig.now() + SR);
    // And then on, to bar seven.
    rig.session.seek(24.0);
    let again = rig.now() + LATENCY;
    bumps.at((again + 9_000.0) / SR);
    rig.run_until(rig.now() + SR);
    let frames = rolled.stop(&mut rig);

    let (takes, sum) = landed(&rig);
    let places: Vec<(f64, bool)> = takes.iter().map(|take| (take.0, take.2)).collect();
    assert_eq!(places, [(8.0, true), (0.0, true), (24.0, true)], "each where it was sung, each on the track");
    for take in &takes {
        assert!((take.1 - SR).abs() <= 1.0, "a second each: {takes:?}");
    }
    assert!((sum - frames).abs() <= 3.0, "{sum} frames in takes of {frames} sung");
    // What was sung to each stretch is in its place there.
    for (beat, frames_in) in [(8.0, 12_000.0), (0.0, 20_000.0), (24.0, 9_000.0)] {
        let off = off_its_place(&rig.session, voice, beat * FRAMES_PER_BEAT + frames_in);
        assert!(off.abs() <= 1.0, "the stretch at beat {beat}: {off:.3}");
    }
    assert_eq!(rig.session.project().track(voice).unwrap().takes.len(), 3);
    in_use(&rig);

    // Sent back and stopped before where the take began: both are there.
    rig.session.seek(8.0);
    rig.run(4);
    let rolled = Rolled::start(&mut rig);
    rig.run_until(rig.now() + SR);
    rig.session.seek(2.0);
    rig.run_until(rig.now() + 0.5 * SR);
    let frames = rolled.stop(&mut rig);
    let (takes, sum) = landed(&rig);
    assert_eq!(takes.iter().map(|take| take.0).collect::<Vec<_>>(), [8.0, 2.0]);
    assert!((sum - frames).abs() <= 2.0, "{sum} frames in takes of {frames} sung");
    in_use(&rig);
}

#[test]
fn a_loop_that_is_switched_off_during_a_take_lets_the_take_go_on_past_its_end() {
    let (mut rig, voice, bumps, _folder) = rolling_rig();
    rig.session.edit(|project| project.set_loop(true, 0.0, 4.0)).unwrap();
    rig.session.seek(0.0);
    rig.run(4);
    let rolled = Rolled::start(&mut rig);
    // Once round, and half a second into the second pass the loop goes off.
    let pass = 4.0 * FRAMES_PER_BEAT;
    rig.run_until(rig.now() + pass + 24_000.0);
    rig.session.edit(|project| project.set_loop(false, 0.0, 4.0)).unwrap();
    // What is sung a beat past the loop's end is sung there.
    let second = rig.now() - 24_000.0 + LATENCY;
    bumps.at((second + 5.0 * FRAMES_PER_BEAT) / SR);
    rig.run_until(rig.now() - 24_000.0 + 6.0 * FRAMES_PER_BEAT);
    let frames = rolled.stop(&mut rig);

    let (takes, sum) = landed(&rig);
    let places: Vec<(f64, f64, bool)> =
        takes.iter().map(|take| (take.0, (take.1 / FRAMES_PER_BEAT * 100.0).round() / 100.0, take.2)).collect();
    // One pass of the loop, and one stretch that goes on past its end:
    // the track plays that one.
    assert_eq!(places, [(0.0, 4.0, false), (0.0, 6.0, true)], "{takes:?}");
    assert!((sum - frames).abs() <= 2.0, "{sum} frames in takes of {frames} sung");
    let off = off_its_place(&rig.session, voice, 5.0 * FRAMES_PER_BEAT);
    assert!(off.abs() <= 1.0, "{off:.3}");
    in_use(&rig);
}

#[test]
fn all_that_was_sung_is_in_a_take_round_a_loop_punched_in_and_after_a_count_in() {
    // Round a loop: every pass, and of the last one what was sung of it.
    let (mut rig, _voice, _bumps, _folder) = rolling_rig();
    rig.session.edit(|project| project.set_loop(true, 4.0, 8.0)).unwrap();
    rig.session.seek(4.0);
    rig.run(4);
    let rolled = Rolled::start(&mut rig);
    let until = rig.now() + 2.0 * 4.0 * FRAMES_PER_BEAT + 2.5 * FRAMES_PER_BEAT;
    run_taking(&mut rig, until);
    let frames = rolled.stop(&mut rig);
    let (takes, sum) = landed(&rig);
    assert_eq!(takes.len(), 3, "{takes:?}");
    assert!((sum - frames).abs() <= 3.0, "{sum} frames in takes of {frames} sung");
    in_use(&rig);

    // Punched in over a stretch the song plays through: what was sung
    // inside it, and nothing else, is the take.
    rig.session.edit(|project| project.set_loop(true, 2.0, 5.0)).unwrap();
    rig.session.set_punch(true);
    rig.session.seek(0.0);
    rig.run(4);
    let rolled = Rolled::start(&mut rig);
    rig.run_until(rig.now() + 7.0 * FRAMES_PER_BEAT);
    rolled.stop(&mut rig);
    let (takes, sum) = landed(&rig);
    assert_eq!(takes.iter().map(|take| take.0).collect::<Vec<_>>(), [2.0]);
    assert!((sum - 3.0 * FRAMES_PER_BEAT).abs() <= 1.5, "three beats were inside: {sum}");
    in_use(&rig);
    // And a song that is sent back into the stretch comes into it again.
    rig.session.seek(1.0);
    rig.run(4);
    let rolled = Rolled::start(&mut rig);
    rig.run_until(rig.now() + 3.0 * FRAMES_PER_BEAT);
    rig.session.seek(6.0);
    rig.run_until(rig.now() + FRAMES_PER_BEAT);
    rig.session.seek(3.0);
    rig.run_until(rig.now() + FRAMES_PER_BEAT);
    rolled.stop(&mut rig);
    let (takes, sum) = landed(&rig);
    // In from beat two until the song is sent away at four, and from
    // three, where it comes back, for a beat.
    assert_eq!(takes.iter().map(|take| take.0).collect::<Vec<_>>(), [2.0, 3.0], "{takes:?}");
    assert!((sum - 3.0 * FRAMES_PER_BEAT).abs() <= 2.5, "two beats and one: {sum}");
    in_use(&rig);
    rig.session.set_punch(false);
    rig.session.edit(|project| project.set_loop(false, 2.0, 5.0)).unwrap();

    // After a count-in: the take is what was sung once the song rolled.
    rig.session.set_count_in(1);
    rig.session.seek(8.0);
    rig.run(4);
    rig.session.record().unwrap();
    let asked = rig.now();
    // A bar is counted in, two seconds at this tempo; then three seconds.
    rig.run_until(asked + 2.0 * SR + 3.0 * SR);
    rig.session.stop();
    let (takes, sum) = landed(&rig);
    assert_eq!(takes.len(), 1);
    assert_eq!(takes[0].0, 8.0);
    assert!((sum - 3.0 * SR).abs() <= 2.0, "three seconds were sung to the song: {sum}");
    in_use(&rig);
}

// ---- one clock is a belief ---------------------------------------------------

/// Records ninety seconds from an input that is said to count time by the
/// output's clock and runs `ppm` parts a million apart from it, with a
/// bump every second. Returns how many seconds into the take the session
/// came to say that the input has a clock of its own (`None` if it never
/// did), and how far the worst bump was from its place before that and
/// after it, in milliseconds.
fn believed_to_share_a_clock(ppm: f64) -> (Option<f64>, f64, f64) {
    use trill_engine::InputClock;
    const SECONDS: usize = 90;
    let host = studio(ScriptedInput { ppm, shares_clock: true, ..microphone() });
    let (mut rig, _folder) = rig_on(host, AudioConfig::default());
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    assert_eq!(rig.session.input_status().clock, Some(InputClock::Shared), "as it was opened");
    rig.run(40);
    rig.session.record().unwrap();
    // The take begins when the place the input is read at has been put
    // for good. An input that is read frame for frame takes a second over
    // that, and one whose clock runs away from under it never comes to
    // it: the take then begins when it has waited as long as a take does.
    let mut waited = 0;
    while rig.session.transport().position_beats == 0.0 {
        rig.run(1);
        waited += 1;
        assert!(waited < 400, "{ppm} ppm: the take never began");
    }
    let began = rig.now() - rig.session.transport().position_beats * FRAMES_PER_BEAT;
    // The song's frame 24,000 is heard at this moment of the host's
    // clock, and every second after it another.
    let first = (began + LATENCY + 24_000.0) / SR;
    rig.host.hear(Hears::Sound(Arc::new(move |_, seconds| {
        let since = seconds - first;
        let nearest = since.round().max(0.0);
        let away = (since - nearest) * SR / 3.0;
        if away.abs() < 20.0 { (0.8 * (-away * away).exp()) as f32 } else { 0.0 }
    })));
    let jumps = rig.session.input_health().unwrap().jumps;
    let mut found = None;
    for second in 1..=SECONDS {
        run_taking(&mut rig, began + second as f64 * SR);
        if found.is_none() && rig.session.input_status().clock == Some(InputClock::FoundOwn) {
            found = Some(second as f64);
        }
    }
    // Found out or not, the reading was never put somewhere else at once.
    let health = rig.session.input_health().unwrap();
    assert_eq!((health.jumps, health.lost, health.late), (jumps, 0, 0), "{ppm} ppm: {health:?}");
    rig.session.stop();
    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!(recorded.silent_frames, 0, "nothing was late or lost");
    let (_, start, _, audio) = clip_at(&rig.session, voice, 1.0);
    assert_eq!(start, 0.0);
    let path = Path::new(&audio.path);
    let (mut before, mut after, mut last) = (0.0f64, 0.0f64, None::<f64>);
    for bump in 0..SECONDS - 1 {
        let song_frame = 24_000.0 + bump as f64 * SR;
        let in_file = audio.offset_seconds * SR + song_frame;
        let near = frames_of(path, 1, in_file.round() as u64 - 256, 512);
        let off_ms = (in_file.round() - 256.0 + top_of(&near) - in_file) / SR * 1000.0;
        // From one second to the next a bump moves by a little: the
        // place is steered to where it belongs over a few seconds, and
        // never put there. (At the most it is half a millisecond out
        // when it is found out, and comes in by less than that a second.)
        if let Some(last) = last {
            assert!((off_ms - last).abs() < 0.4, "{ppm} ppm: {:.3} ms in a second, at {bump} s", off_ms - last);
        }
        last = Some(off_ms);
        match found {
            Some(found) if song_frame / SR >= found => after = after.max(off_ms.abs()),
            _ => before = before.max(off_ms.abs()),
        }
    }
    (found, before, after)
}

#[test]
fn an_input_that_is_said_to_share_the_outputs_clock_and_does_not_is_found_out() {
    for ppm in [20.0, 100.0, -100.0] {
        let (found, before, after) = believed_to_share_a_clock(ppm);
        let found = found.unwrap_or_else(|| panic!("{ppm} ppm apart, and it was never found out"));
        println!(
            "said to share the output's clock, {ppm} ppm apart: found out {found:.0} s into the take; a click was \
             {before:.3} ms from its place at the worst before that, and {after:.3} ms after"
        );
        assert!(found <= 40.0, "{ppm} ppm: found out after {found} s");
        assert!(before < 2.0, "{ppm} ppm: {before} ms before it was found out");
        assert!(after < 0.5, "{ppm} ppm: {after} ms once it was found out");
    }
    // And one that does share it goes on being believed.
    let (found, before, _) = believed_to_share_a_clock(0.0);
    assert_eq!(found, None, "a pair that is one clock is read as one");
    assert!(before < 0.03, "{before} ms");
}

// ---- a song's recordings come beside it ---------------------------------------

/// A rig whose input hears a tone, with a track that records it, armed.
fn toned_rig() -> (Rig, TrackId, tempfile::TempDir) {
    let (mut rig, folder) = rig_on(studio(microphone()), AudioConfig::default());
    rig.host.hear(tone());
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    rig.run(40);
    (rig, voice, folder)
}

/// Records a take of `frames` frames from the start of the song, and
/// returns its file.
fn a_take(rig: &mut Rig, frames: f64) -> std::path::PathBuf {
    rig.session.seek(0.0);
    rig.run(4);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + frames);
    rig.session.stop();
    rig.session.last_recording().unwrap().takes[0].file.clone()
}

/// The path of every recording the song's takes name.
fn take_paths(session: &Session, track: TrackId) -> Vec<std::path::PathBuf> {
    let track = session.project().track(track).unwrap();
    track.takes.iter().map(|take| take.clip.audio().unwrap().path.clone().into()).collect()
}

#[test]
fn a_recording_that_a_sampler_plays_is_in_use_and_comes_beside_the_song_with_the_rest() {
    let (mut rig, voice, folder) = toned_rig();
    let first = a_take(&mut rig, 48_000.0);
    // The take's recording is given to a sampler, and then the take and
    // what the track plays of it are let go of: the sampler still plays it.
    let keys = rig.session.edit(|project| Ok(project.add_track("Keys"))).unwrap();
    let sampler = rig.session.add_device(ChainRef::Track(keys), "builtin:sampler", None, None).unwrap();
    rig.session.set_sampler_file(sampler, Some(&first.to_string_lossy())).unwrap();
    let take = rig.session.project().track(voice).unwrap().takes[0].clip.id;
    rig.session
        .edit(|project| {
            project.remove_take(take)?;
            let clip = project.track(voice)?.clips[0].id;
            project.remove_clip(clip).map(|_| ())
        })
        .unwrap();
    // Nothing undo can reach is in the way of the question.
    let song = folder.path().join("songs").join("Sampled.trill");
    rig.session.save(&song).unwrap();
    rig.session.load(&song).unwrap();
    assert!(rig.session.project().track(voice).unwrap().takes.is_empty());
    assert!(rig.session.unused_recordings().is_empty(), "a sampler plays it: {:?}", rig.session.unused_recordings());
    // And the first save brought it beside the song, where the sampler
    // names it: by the means a relink renames a file.
    let beside = folder.path().join("songs").join("Sampled files").join("takes").join("Voice.wav");
    assert!(beside.is_file() && !first.exists(), "nothing is left where it was");
    let names: Vec<String> = rig.session.files().iter().map(|used| used.path.clone()).collect();
    assert_eq!(names, [beside.to_string_lossy()], "{names:?}");
    assert!(rig.session.problems().is_empty(), "{:?}", rig.session.problems());

    // A song opened by a path that is spelled in another case names the
    // same files, where the file system has it so.
    if cfg!(windows) {
        let shouted = folder.path().join("SONGS").join("SAMPLED.TRILL");
        rig.session.load(&shouted).unwrap();
        let again = a_take_on_a_new_track(&mut rig);
        assert!(again.is_file());
        let unused = rig.session.unused_recordings();
        assert!(unused.is_empty(), "spelled otherwise, and the same files: {unused:?}");
    }
}

/// A track that records, armed, in the song that is open, and a take on it.
fn a_take_on_a_new_track(rig: &mut Rig) -> std::path::PathBuf {
    armed_track(rig, "More", TrackInput::mono(1));
    rig.run(40);
    a_take(rig, 24_000.0)
}

#[test]
fn recordings_that_cannot_be_given_a_second_name_are_copied_away_from_the_session_and_named_when_whole() {
    let (mut rig, voice, folder) = toned_rig();
    // As across two volumes, where no file has two names.
    rig.session.copy_recordings_beside(true);
    let first = a_take(&mut rig, 96_000.0);
    let second = a_take(&mut rig, 48_000.0);
    let held = std::fs::read(&first).unwrap();
    let song = folder.path().join("songs").join("Far.trill");
    rig.session.save(&song).unwrap();

    // Saved at once: the song names each recording where it is, and says
    // that they are on their way.
    assert!(!rig.session.is_modified());
    assert_eq!(take_paths(&rig.session, voice), [first.clone(), second.clone()]);
    assert!(std::fs::read_to_string(&song).unwrap().contains(&first.to_string_lossy().replace('\\', "\\\\")));
    assert!(first.is_file() && second.is_file());
    let coming = rig.session.recordings_coming().expect("they are being copied");
    assert_eq!(coming.files, 2);
    assert!(coming.said().starts_with("Bringing 2 recordings beside the song"), "{}", coming.said());

    // Whole: the song, what undo can reach and the file that was saved
    // name them beside it, and the first copies are gone.
    wait_until(|| {
        rig.session.housekeeping();
        rig.session.recordings_coming().is_none()
    });
    let beside = folder.path().join("songs").join("Far files").join("takes");
    let (new_first, new_second) = (beside.join("Voice.wav"), beside.join("Voice 2.wav"));
    assert_eq!(take_paths(&rig.session, voice), [new_first.clone(), new_second.clone()]);
    assert_eq!(std::fs::read(&new_first).unwrap(), held, "not a byte of it is other than it was");
    assert!(!first.exists() && !second.exists(), "nothing is left where they were");
    assert!(!rig.session.is_modified(), "naming a file where it now is is no change to the song");
    let saved = trill_core::Project::from_json(&std::fs::read_to_string(&song).unwrap()).unwrap();
    let in_file: Vec<String> =
        saved.track(voice).unwrap().takes.iter().map(|take| take.clip.audio().unwrap().path.clone()).collect();
    assert_eq!(in_file, [new_first.to_string_lossy(), new_second.to_string_lossy()], "the song's file says so too");
    assert!(rig.session.undo(), "the second take is undone");
    assert_eq!(
        take_paths(&rig.session, voice),
        std::slice::from_ref(&new_first),
        "the state before names it where it is now"
    );
    assert!(rig.session.problems().is_empty(), "{:?}", rig.session.problems());
    assert!(!beside.join("Voice.coming").exists());
}

#[test]
fn a_take_that_is_still_being_recorded_when_the_song_is_first_saved_comes_beside_it_when_it_ends() {
    let (mut rig, voice, folder) = toned_rig();
    rig.session.seek(0.0);
    rig.run(4);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 48_000.0);
    // Saved in the middle of the take.
    let song = folder.path().join("songs").join("Early.trill");
    rig.session.save(&song).unwrap();
    let writing = rig.session.growing_takes()[0].file.path().to_path_buf();
    assert!(writing.starts_with(folder.path().join("unsaved")), "it is being written where it was begun");
    rig.run_until(rig.now() + 48_000.0);
    rig.session.stop();

    // Ended: it is beside the song, and the song names it there.
    let beside = folder.path().join("songs").join("Early files").join("takes").join("Voice.wav");
    assert_eq!(take_paths(&rig.session, voice), std::slice::from_ref(&beside));
    assert!(beside.is_file());
    let audio = trill_media::MediaCache::default().audio(&beside).unwrap();
    assert!(audio.frames() >= 96_000, "all of it: {} frames", audio.frames());
    assert!(rig.session.problems().is_empty(), "{:?}", rig.session.problems());
    // The name it was written under goes when the song's file says where
    // it is: until then a copy kept for safety may still name it.
    assert!(writing.is_file());
    rig.session.save(&song).unwrap();
    assert!(!writing.exists() && beside.is_file());
}

#[test]
fn the_recordings_of_an_unsaved_song_that_was_recovered_come_beside_it_at_its_first_save() {
    // A program records a take for a song that has no place, keeps a copy
    // for safety, and is gone.
    let folder = tempfile::tempdir().unwrap();
    let runs = folder.path().join("recordings");
    let (copy, first) = {
        let mut lost = Rig::with(studio(microphone()), AudioConfig::default(), trill_engine::Patience::default());
        lost.session.keep_unsaved_recordings_in(runs.join("an earlier run"));
        lost.session.set_count_in(0);
        lost.host.hear(tone());
        armed_track(&mut lost, "Voice", TrackInput::mono(1));
        lost.run(40);
        let first = a_take(&mut lost, 48_000.0);
        let copy = folder.path().join("copy.trill");
        std::fs::write(&copy, lost.session.snapshot_json()).unwrap();
        (copy, first)
    };
    assert!(first.starts_with(runs.join("an earlier run")));

    // Another run of the program recovers it, and saves it for the first time.
    let mut rig = Rig::with(studio(microphone()), AudioConfig::default(), trill_engine::Patience::default());
    rig.session.keep_unsaved_recordings_in(runs.join("this run"));
    rig.session.load_recovered(&copy, None).unwrap();
    let voice = rig.session.project().tracks[0].id;
    assert_eq!(take_paths(&rig.session, voice), std::slice::from_ref(&first));
    let song = folder.path().join("songs").join("Back.trill");
    rig.session.save(&song).unwrap();
    let beside = folder.path().join("songs").join("Back files").join("takes").join("Voice.wav");
    assert_eq!(take_paths(&rig.session, voice), std::slice::from_ref(&beside), "it came beside the song");
    assert!(beside.is_file() && !first.exists(), "and nothing is left in the folder of the run that is gone");
    assert!(std::fs::read_to_string(&song).unwrap().contains("Back files"));
}

#[test]
fn a_punched_take_of_notes_alone_plays_through_the_loop_and_holds_what_was_played_inside_it() {
    // No track records an input: there is no audio of the take at all, and
    // the song must play through its loop all the same.
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    let keys = rig.probe_track("Keys");
    rig.session.set_armed(keys, true).unwrap();
    rig.session.edit(|project| project.set_loop(true, 4.0, 8.0)).unwrap();
    rig.session.set_punch(true);
    rig.session.seek(2.0);
    rig.run(40);
    assert!(rig.host.capturing().is_none(), "nothing here records an input");

    let began = rig.now() + LATENCY;
    let at = |beat: f64| began + (beat - 2.0) * FRAMES_PER_BEAT;
    rig.session.record().unwrap();
    for (beat, pitch) in [(3.0, 60), (5.0, 62), (7.5, 64), (9.0, 65)] {
        rig.key(at(beat), on(pitch));
        rig.key(at(beat) + 6000.0, off(pitch));
    }
    rig.run_until(at(10.0));
    let reached = rig.session.transport().position_beats;
    assert!(reached > 9.5, "the song played through its loop, and is at beat {reached:.2}");
    rig.session.stop();

    // One clip, the punch's stretch, with the keys that were played inside
    // it where they were played: not a second pass laid over the first.
    let clips = rig.session.project().track(keys).unwrap().clips.clone();
    assert_eq!(clips.len(), 1);
    assert_eq!((clips[0].start, clips[0].length), (4.0, 4.0));
    let played: Vec<(u8, f64)> =
        clips[0].notes().iter().map(|note| (note.pitch, (note.start * 100.0).round() / 100.0)).collect();
    assert_eq!(played, [(62, 1.0), (64, 3.5)]);
    assert!(rig.session.project().track(keys).unwrap().takes.is_empty(), "one pass, and nothing to choose from");

    // The take is over: the song goes round its loop again.
    rig.session.seek(6.0);
    rig.run(4);
    rig.session.play();
    rig.run_until(rig.now() + 3.0 * FRAMES_PER_BEAT);
    let position = rig.session.transport().position_beats;
    assert!((4.0..8.0).contains(&position), "it came round: beat {position:.2}");
    rig.session.stop();
}

// ---- what a device says of a buffer, and what it does not ---------------------

#[test]
fn a_buffer_that_is_stamped_late_wrongly_or_not_at_all_during_a_take_moves_nothing_and_loses_nothing() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.0));
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    rig.run(40);
    let began = rig.now() + LATENCY;
    let song_frames = [12_000.0, 36_000.0, 60_000.0, 84_000.0, 108_000.0];
    for frame in song_frames {
        bumps.at((began + frame) / SR);
    }
    let before = rig.session.input_health().unwrap();
    let start = rig.now();
    rig.session.record().unwrap();
    // A buffer handed over thirty milliseconds late and stamped then (a
    // stamp that late was once read as frames that had not come); one the
    // device says it has no stamp for; one stamped a second before the one
    // before it; and three in a row stamped late.
    let odd: [(f64, Vec<Stamped>); 4] = [
        (24_000.0, vec![Stamped::Late(0.03)]),
        (48_000.0, vec![Stamped::Not]),
        (72_000.0, vec![Stamped::Early(1.0)]),
        (96_000.0, vec![Stamped::Late(0.025), Stamped::Late(0.04), Stamped::Late(0.022)]),
    ];
    for (at, stamps) in odd {
        rig.run_until(start + at);
        rig.host.stamp_next(stamps);
    }
    rig.run_until(start + 120_000.0);
    rig.session.stop();

    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!(recorded.silent_frames, 0, "every frame came, and none was taken for missing");
    let after = rig.session.input_health().unwrap();
    assert_eq!((after.lost, after.late, after.jumps), (before.lost, before.late, before.jumps), "{after:?}");
    for frame in song_frames {
        let off = off_its_place(&rig.session, voice, frame);
        assert!(off.abs() <= 1.0, "the bump at frame {frame} is {off:.3} frames off");
    }
}

#[test]
fn a_take_asked_for_as_its_input_opens_begins_when_the_inputs_place_is_put_and_has_no_step_in_it() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.0));
    // The first buffer the input hands over is stamped five milliseconds
    // late, as a first buffer often is: the place it is read at is put by
    // that, and put again a quarter of a second on, when the least late of
    // its stamps have had their say.
    rig.host.stamp_next([Stamped::Late(0.005)]);
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    let asked = rig.now();
    // A bump every twentieth of a second from here, for a second and a half.
    let heard: Vec<f64> = (1..30).map(|n| asked + n as f64 * 2_400.0 + 777.0).collect();
    for at in &heard {
        bumps.at(at / SR);
    }
    // No count-in, and no moment given to the input: the take is asked
    // for with the very next thing the session is told.
    rig.session.record().unwrap();
    rig.run(4);
    rig.session.housekeeping();
    assert_eq!(rig.session.record_status().state, RecordState::CountIn, "it waits, as through a count-in");
    assert_eq!(rig.session.transport().position_beats, 0.0, "and the song with it");
    rig.run_until(asked + 1.5 * SR);
    rig.session.housekeeping();
    assert_eq!(rig.session.record_status().state, RecordState::Recording);
    // Where the song's first frame was made, by where the song is now.
    let rolled = rig.now() - rig.session.transport().position_beats * FRAMES_PER_BEAT;
    let waited = (rolled - asked) / SR;
    assert!((0.24..0.33).contains(&waited), "the take began {waited:.3} s after it was asked for");
    let during = rig.session.input_health().unwrap().jumps;
    rig.run_until(asked + 2.0 * SR);
    rig.session.stop();

    // The place was put again once, before the take's first frame: not in it.
    let health = rig.session.input_health().unwrap();
    assert_eq!((during, health.jumps), (1, 1), "{health:?}");
    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!((recorded.takes.len(), recorded.silent_frames), (1, 0));
    // Every bump the take holds is where it was heard, the first of them
    // a few hundredths of a second into it.
    let mut held = 0;
    for at in &heard {
        let song_frame = at - LATENCY - rolled;
        if song_frame < 100.0 {
            continue;
        }
        held += 1;
        let off = off_its_place(&rig.session, voice, song_frame);
        assert!(off.abs() <= 1.0, "the bump {:.3} s into the take is {off:.3} frames off", song_frame / SR);
    }
    assert!(held >= 20, "{held} bumps were in the take");
}

// ---- nobody waits with the session held, and nothing is open for nothing ---------

#[test]
fn stopping_a_take_does_not_wait_for_its_last_sound_where_the_audio_thread_runs_by_itself() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    let bumps = Bumps::default();
    rig.host.hear(bumps.heard(|_| 0.0));
    let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
    rig.run(40);
    let began = rig.now() + LATENCY;
    // Sung a hundred frames before the song stops: it is heard after the
    // stop was asked for, and comes in later still.
    bumps.at((began + 47_900.0) / SR);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 48_000.0);

    // From here the host is one whose audio thread runs by itself, as a
    // device's does, and this test is that thread: waiting for it makes
    // nothing happen.
    rig.host.set_watched(true);
    rig.host.set_pump(false);
    let asked = std::time::Instant::now();
    rig.session.stop();
    let took = asked.elapsed();
    assert!(took < std::time::Duration::from_millis(100), "stop took {took:?} with the session held");
    assert!(rig.session.is_recording(), "the take is not over: its last sound is still coming in");
    rig.session.housekeeping();
    assert!(rig.session.is_recording() && rig.session.last_recording().is_none());

    // The audio thread goes on, and a round of housekeeping sees the take in.
    rig.run(30);
    rig.session.housekeeping();
    assert!(!rig.session.is_recording(), "the take is over once its last sound is in");
    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!((recorded.takes.len(), recorded.silent_frames), (1, 0));
    assert!((recorded.takes[0].length_beats - 2.0).abs() < 0.03, "{:?}", recorded.takes[0]);
    let off = off_its_place(&rig.session, voice, 47_900.0);
    assert!(off.abs() <= 1.0, "what was sung to the last of it is there, {off:.3} frames from its place");

    // An audio thread that never says where the take ended is not waited
    // for for ever: the take ends where the song was last known to be,
    // with what there is of it.
    rig.session.seek(0.0);
    rig.run(4);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 48_000.0);
    rig.session.stop();
    assert!(rig.session.is_recording());
    wait_until(|| {
        rig.session.housekeeping();
        !rig.session.is_recording()
    });
    let recorded = rig.session.last_recording().unwrap().clone();
    assert_eq!(recorded.takes.len(), 1, "{recorded:?}");
    assert!(recorded.takes[0].seconds > 0.9, "whole up to where it was last heard of: {:?}", recorded.takes[0]);
    rig.host.set_watched(false);
    rig.host.set_pump(true);
}

#[test]
fn an_input_opened_for_an_audio_thread_that_is_away_is_closed_again_and_opened_when_it_is_back() {
    let (mut rig, _folder) = rig_on(studio(microphone()), AudioConfig::default());
    let voice = rig.session.edit(|project| Ok(project.add_track("Voice"))).unwrap();
    rig.session.edit(|project| project.set_track_input(voice, Some(TrackInput::mono(1)))).unwrap();
    rig.run(2);

    // The audio thread goes away, and more is asked of it than its queue
    // holds: what is said to it from here on is lost.
    rig.host.set_pump(false);
    for step in 0..1_200 {
        rig.session.set_metronome(step % 2 == 0);
    }
    // The track is armed: the input is needed, and is opened. The audio
    // thread cannot be told of it, so nothing would read what it hears.
    rig.session.set_armed(voice, true).unwrap();
    rig.session.housekeeping();
    assert!(rig.host.capturing().is_none(), "a microphone is not left open for nothing");
    let status = rig.session.input_status();
    assert!(status.open.is_none(), "{status:?}");
    let problem = status.problem.expect("and it is said");
    assert!(problem.contains("is not answering") && problem.contains("'Microphone' was closed again"), "{problem}");
    assert!(!problem.contains("Choose the device"), "nothing is wrong with the choice of device: {problem}");
    // A take is refused the same way, and leaves no file.
    let refused = rig.session.record().unwrap_err();
    assert!(matches!(&refused, SessionError::Recording(why) if why.contains("is not answering")), "{refused}");
    assert!(rig.host.capturing().is_none() && !rig.session.is_recording());

    // It is back: it is told again what it missed, and the input is opened.
    rig.host.set_pump(true);
    rig.run(4);
    rig.session.housekeeping();
    rig.run(2);
    rig.session.housekeeping();
    assert!(rig.host.capturing().is_some(), "{:?}", rig.session.input_status());
    assert!(rig.session.input_status().problem.is_none());
    settle(&mut rig);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 24_000.0);
    rig.session.stop();
    assert_eq!(rig.session.last_recording().unwrap().takes.len(), 1);
}

// ---- one clock, one rate: a frame for a frame ------------------------------------

#[test]
fn an_input_on_the_outputs_clock_at_its_rate_is_recorded_as_the_device_gave_it_to_the_bit() {
    // What the input hears is noise by the frame of the host's clock:
    // every frame another value, so that one that is not the frame it
    // should be, or is anything but itself, shows.
    let heard = |frame: u64| {
        let mut bits = frame.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
        bits ^= bits >> 29;
        bits = bits.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        bits ^= bits >> 32;
        ((bits >> 40) as f32 / (1u64 << 23) as f32 - 1.0) * 0.5
    };
    for exclusive in [false, true] {
        let host = studio(ScriptedInput { shares_clock: true, ..microphone() });
        let config = AudioConfig { exclusive, buffer_size: exclusive.then_some(256), ..AudioConfig::default() };
        let (mut rig, _folder) = rig_on(host, config);
        rig.host.hear(Hears::Sound(Arc::new(move |_, seconds| heard((seconds * SR).round() as u64))));
        let voice = armed_track(&mut rig, "Voice", TrackInput::mono(1));
        assert_eq!(rig.session.input_status().clock, Some(trill_engine::InputClock::Shared));
        settle(&mut rig);
        let before = rig.session.input_health().unwrap();
        rig.session.record().unwrap();
        let until = rig.now() + 3.0 * SR;
        run_taking(&mut rig, until);
        rig.session.stop();

        let recorded = rig.session.last_recording().unwrap().clone();
        assert_eq!((recorded.takes.len(), recorded.silent_frames), (1, 0));
        let after = rig.session.input_health().unwrap();
        assert_eq!((after.jumps, after.lost, after.late), (before.jumps, 0, 0), "{after:?}");
        assert_eq!(rig.session.input_status().clock, Some(trill_engine::InputClock::Shared), "still one clock");
        // Frame n of the file is frame n + K of what the device heard, for
        // one K, and not a bit of it is other than the device gave: no
        // filter and no place between frames lies in between.
        let (_, _, _, audio) = clip_at(&rig.session, voice, 1.0);
        let file = frames_of(Path::new(&audio.path), 1, 0, 3 * 48_000);
        assert!(file.len() >= 2 * 48_000, "{} frames", file.len());
        let from = 1000usize;
        let ahead = (0..400_000u64)
            .find(|ahead| (0..8).all(|n| file[from + n].to_bits() == heard(from as u64 + n as u64 + ahead).to_bits()))
            .expect("the file holds frames of what the device heard, as they were");
        let other = (0..file.len()).find(|n| file[*n].to_bits() != heard(*n as u64 + ahead).to_bits());
        assert_eq!(other, None, "exclusive: {exclusive}; the first frame that is not what the device gave");
    }
}

// ---- a song that others hold too ----------------------------------------------

#[test]
fn a_take_is_said_to_whoever_else_has_the_song_when_it_has_landed_and_is_its_makers_to_take_back() {
    let (mut rig, voice, _folder) = toned_rig();
    // Another program is handed the song, and hears what is changed in it.
    rig.session.hear_changes(true);
    rig.session.share();
    let null = trill_engine::AudioBackend::Null { sample_rate: 48_000 };
    let mut other = Session::new(Arc::new(trill_dsp::BuiltinFactory), null).unwrap();
    other.hear_changes(true);
    other.take_song(&rig.session.whole_song()).unwrap();
    rig.session.take_said();
    assert_eq!(rig.session.project().track(voice).unwrap().input, Some(TrackInput::mono(1)));
    assert_eq!(other.project().track(voice).unwrap().input, None, "which input a track records is one's own");

    // While the take is recorded nothing of it is said, whatever the
    // session's chores come to know of it meanwhile.
    rig.session.seek(0.0);
    rig.run(4);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 48_000.0);
    rig.session.housekeeping();
    assert!(rig.session.is_recording());
    assert!(rig.session.take_said().is_empty(), "a take that is being recorded is nobody else's yet");
    rig.run_until(rig.now() + 48_000.0);
    rig.session.stop();
    let file = rig.session.last_recording().unwrap().takes[0].file.clone();

    // Landed: said, and the other has the take in its lane and what the
    // track plays of it, naming the file where its maker has it.
    let said = rig.session.take_said();
    assert!(!said.is_empty());
    let floors = rig.session.floors();
    other.merge("Ada", said, Some(&floors), rig.session.replica()).unwrap();
    let held = |session: &Session| -> (Vec<(u32, ClipId)>, Vec<ClipId>) {
        let track = session.project().track(voice).unwrap();
        let takes = track.takes.iter().map(|take| (take.lane, take.clip.id)).collect();
        (takes, track.clips.iter().map(|clip| clip.id).collect())
    };
    assert_eq!(held(&other), held(&rig.session));
    assert_eq!((held(&other).0.len(), held(&other).1.len()), (1, 1), "a take, and the clip that plays it");
    assert_eq!(take_paths(&other, voice), [file]);
    assert_eq!(other.project().track(voice).unwrap().input, None);
    assert!(other.input_status().open.is_none(), "and nothing of it opens an input there");

    // The step is its maker's: taken back there, the take is gone here
    // too, and nobody else's undo has it.
    assert!(!other.can_undo());
    assert!(rig.session.undo());
    let said = rig.session.take_said();
    let floors = rig.session.floors();
    other.merge("Ada", said, Some(&floors), rig.session.replica()).unwrap();
    assert_eq!(held(&other), (Vec::new(), Vec::new()));
}
