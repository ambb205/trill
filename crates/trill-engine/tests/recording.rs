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
use trill_engine::host::scripted::{Hears, ScriptedHost, ScriptedInput};
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
    let began = rig.now();
    let written = |rig: &Rig| rig.session.growing_takes().iter().map(|take| take.file.frames()).min();
    let before = written(rig).unwrap_or(0) as f64;
    while rig.now() < frame {
        let until = (rig.now() + 96_000.0).min(frame);
        rig.run_until(until);
        rig.session.housekeeping();
        // Within four seconds of what has been handed over since.
        wait_until(|| written(rig).is_none_or(|frames| frames as f64 - before + 192_000.0 >= rig.now() - began));
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
        assert!(worst < 0.03, "{ppm} ppm: once the clocks are told apart, {worst:.4} ms at the most");
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
    rig.session.watch_input(true);
    assert!(open(&mut rig));
    rig.host.hear(Hears::Sound(Arc::new(|channel, _| if channel == 0 { 0.25 } else { 1.5 })));
    rig.run(20);
    let levels = rig.session.input_levels();
    assert_eq!(levels, [(0.25, false), (1.5, true)], "the second channel is over full scale");
    rig.session.watch_input(false);
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
    // 60,000 frames at 120 are two and a half beats.
    assert_eq!(said, [(6.0, 10.0, false), (8.0, 8.0, false), (8.0, 8.0, true), (8.0, 2.5, false)]);
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
    assert_eq!((said.correction_ms, said.correction), (0.0, Correction::None));

    // Measured: the noise comes back 430 frames later than the devices
    // account for.
    rig.session.measure_latency(1).unwrap();
    assert_eq!(rig.session.measurement(), Measurement::Running);
    assert!(rig.session.record().is_err(), "nothing is recorded while it runs");
    rig.run_until(rig.now() + 3.0 * SR);
    rig.session.housekeeping();
    let Measurement::Done(measured) = rig.session.measurement() else { panic!("{:?}", rig.session.measurement()) };
    assert!((measured.correction_ms * 48.0 - 430.0).abs() < 0.5, "{measured:?}");
    assert!(
        (measured.round_trip_ms - said.round_trip_ms - measured.correction_ms).abs() < 0.02,
        "{measured:?} {said:?}"
    );
    assert!(measured.likeness > 0.98 && (measured.heard_db - -26.0).abs() < 0.5, "{measured:?}");
    // It is offered, and changes nothing until it is taken.
    assert_eq!(rig.session.latency_correction(), (0.0, Correction::None));
    rig.session.set_latency_correction(measured.correction_ms, Correction::Measured);
    let now = rig.session.recording_latency().unwrap();
    assert_eq!(now.correction, Correction::Measured);
    assert!((now.round_trip_ms - measured.round_trip_ms).abs() < 0.02);

    let after = take(&mut rig);
    for off in &after {
        assert!(off.abs() <= 1.0, "in its place once the measurement is taken off: {after:?}");
    }
    assert_eq!(rig.session.last_recording().unwrap().correction, Correction::Measured);
    println!(
        "placement by what the devices say: {before:.2?} frames late; measured {:.3} ms, then {after:.2?}",
        measured.correction_ms
    );

    // A figure set by hand counts the same, and none takes it away.
    rig.session.set_latency_correction(5.0, Correction::ByHand);
    assert_eq!(rig.session.latency_correction(), (5.0, Correction::ByHand));
    rig.session.set_latency_correction(123.0, Correction::None);
    assert_eq!(rig.session.latency_correction(), (0.0, Correction::None));

    // With nothing between the output and the input, the measurement says
    // so, and what to do.
    rig.host.hear(Hears::Nothing);
    rig.session.measure_latency(1).unwrap();
    rig.run_until(rig.now() + 3.0 * SR);
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
    rig.run(4);
    rig.session.record().unwrap();
    rig.run_until(rig.now() + 48_000.0);
    rig.session.set_audio_config(AudioConfig { exclusive: true, buffer_size: Some(256), ..AudioConfig::default() });
    assert!(!rig.session.is_recording());
    let notice = rig.session.take_recording_notice().expect("it is said");
    assert!(notice.contains("the audio device changed"), "{notice}");
    assert_eq!(rig.session.last_recording().unwrap().takes.len(), 1, "whole up to there");
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
    let unsaved = folder.path().join("unsaved");
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
    let refusal = rig.session.delete_unused_recordings(&[new_first.clone()]).unwrap_err().to_string();
    assert!(refusal.contains("nothing was deleted") && refusal.contains("Lead Vox 3.wav"), "{refusal}");
    assert!(new_first.is_file());
    assert_eq!(rig.session.delete_unused_recordings(&[third.clone()]).unwrap(), unused[0].1);
    assert!(!third.exists() && rig.session.unused_recordings().is_empty());
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
