//! Recording through real devices, where the machine has a cable from an
//! output to an input: a virtual one, "Line 1 (Virtual Audio Cable)",
//! unless `TRILL_CABLE_OUTPUT` and `TRILL_CABLE_INPUT` name another pair
//! that is joined.
//!
//! Everything here is opt-in, and keeps the machine's time:
//!
//! ```text
//! cargo test -p trill-engine --test cable -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Nothing is played unless both ends are there, and nothing is played
//! into an input another program is recording from: the test looks first
//! and says who it found. What it plays is quiet (a tenth of full scale)
//! and goes nowhere but into the cable. It opens no microphone: only the
//! device it is told is the other end of the cable.

mod common;

use common::{file, folder, write_wav};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::project::AudioContent;
use trill_core::{TrackId, TrackInput};
use trill_dsp::BuiltinFactory;
use trill_engine::host::{AudioHost, SystemHost};
use trill_engine::{AudioConfig, AudioSetup, Measurement, Session};
use trill_media::MediaCache;

const CABLE: &str = "Line 1 (Virtual Audio Cable)";

/// The two ends of the cable, if the machine has them both.
fn ends() -> Option<(String, String)> {
    let output = std::env::var("TRILL_CABLE_OUTPUT").unwrap_or_else(|_| CABLE.to_string());
    let input = std::env::var("TRILL_CABLE_INPUT").unwrap_or_else(|_| CABLE.to_string());
    let devices = SystemHost::new().devices();
    let there = devices.outputs.iter().any(|device| device.name == output)
        && devices.inputs.iter().any(|device| device.name == input);
    if !there {
        println!("no cable on this machine ('{output}' to '{input}'): nothing is played, and nothing is measured");
    }
    there.then_some((output, input))
}

/// The programs other than this one that have a stream open on the input
/// called `name`, by their process ids: someone who would hear what is
/// played into the cable.
#[cfg(windows)]
fn listeners(name: &str) -> Vec<u32> {
    use cpal::traits::{DeviceTrait, HostTrait};
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
    use windows::core::{HSTRING, Interface};
    let host = cpal::default_host();
    let Ok(mut inputs) = host.input_devices() else { return Vec::new() };
    let named = |device: &cpal::Device| device.description().is_ok_and(|said| said.name() == name);
    let Some(id) = inputs.find(named).and_then(|device| device.id().ok()) else { return Vec::new() };
    let id = id.to_string();
    let endpoint = id.strip_prefix("wasapi:").unwrap_or(&id);
    let mut found = Vec::new();
    // SAFETY: plain COM calls on objects this function creates and owns.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return found;
        };
        let Ok(device) = enumerator.GetDevice(&HSTRING::from(endpoint)) else { return found };
        let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { return found };
        let Ok(sessions) = manager.GetSessionEnumerator() else { return found };
        for index in 0..sessions.GetCount().unwrap_or(0) {
            let Ok(session) = sessions.GetSession(index) else { continue };
            if session.GetState().ok() != Some(AudioSessionStateActive) {
                continue;
            }
            let process = session.cast::<IAudioSessionControl2>().ok().and_then(|session| session.GetProcessId().ok());
            if let Some(process) = process.filter(|process| *process != std::process::id() && *process != 0) {
                found.push(process);
            }
        }
    }
    found
}

#[cfg(not(windows))]
fn listeners(_name: &str) -> Vec<u32> {
    Vec::new()
}

/// A session that plays into the cable and records from its other end, or
/// nothing where that is not to be done.
fn session_on_the_cable(exclusive: bool) -> Option<(Session, tempfile::TempDir, f64)> {
    let (output, input) = ends()?;
    let others = listeners(&input);
    if !others.is_empty() {
        println!("another program is recording from '{input}' (process {others:?}): nothing is played into it");
        return None;
    }
    let config = AudioConfig { output: Some(output), input: Some(input), exclusive, ..AudioConfig::default() };
    let setup = AudioSetup { silent_fallback: false, ..AudioSetup::on(Arc::new(SystemHost::new()), config) };
    let mut session = match Session::with_audio(Arc::new(BuiltinFactory), Arc::new(MediaCache::default()), setup) {
        Ok(session) => session,
        Err(error) => {
            println!("the cable's output could not be opened ({error}): nothing is measured");
            return None;
        }
    };
    let folder = folder();
    session.keep_unsaved_recordings_in(folder.path().join("takes"));
    session.set_count_in(0);
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter).map(|_| ())).unwrap();
    let rate = session.audio_info().sample_rate;
    println!(
        "playing into {} ({} frames at a time, {:.1} ms to the speaker by its own account)",
        session.audio_info().output,
        session.audio_info().buffer_frames,
        session.audio_info().latency_ms()
    );
    Some((session, folder, rate))
}

/// Lets `seconds` of the machine's time go by, doing the session's chores.
fn pass(session: &mut Session, seconds: f64) {
    let until = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < until {
        session.housekeeping();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A track that plays `samples` from the song's start, and one that
/// records the first channel of the input, armed.
fn played_and_recorded(session: &mut Session, folder: &tempfile::TempDir, rate: f64, samples: &[f32]) -> TrackId {
    let path = write_wav(&file(folder, &format!("played {}.wav", samples.len())), rate, samples);
    let beats = samples.len() as f64 / rate * 2.0;
    let played = session.edit(|project| Ok(project.add_track("Played"))).unwrap();
    session
        .edit(|project| project.add_audio_clip(played, "Played", 0.0, beats, AudioContent::new(path)).map(|_| ()))
        .unwrap();
    let recorded = session.edit(|project| Ok(project.add_track("Recorded"))).unwrap();
    session.edit(|project| project.set_track_input(recorded, Some(TrackInput::mono(1)))).unwrap();
    session.set_armed(recorded, true).unwrap();
    session.ready_input().unwrap();
    let input = session.input_status().open.expect("the input is open");
    println!(
        "recording from {} ({} Hz, {} channels, {} frames at a time{}{}), read {} frames after it is heard",
        input.device,
        input.sample_rate,
        input.channels,
        input.buffer_frames,
        if input.exclusive { ", exclusively" } else { "" },
        if input.shares_clock { ", on the output's own clock" } else { ", on a clock of its own" },
        input.delay_frames
    );
    if let Some(warning) = &input.warning {
        println!("  {warning}");
    }
    pass(session, 1.5);
    recorded
}

/// Bumps a tenth of full scale high at `frames`, in `length` frames.
fn bumps(frames: &[f64], length: usize) -> Vec<f32> {
    let mut samples = vec![0.0f32; length];
    for at in frames {
        let from = (*at as usize).saturating_sub(40);
        for (index, sample) in samples.iter_mut().enumerate().skip(from).take(80) {
            let away = (index as f64 - at) / 4.0;
            *sample += (0.1 * (-away * away).exp()) as f32;
        }
    }
    samples
}

/// `count` frames of the one channel of a take's file, from `from`.
fn frames_of(path: &Path, from: u64, count: usize) -> Vec<f32> {
    let mut file = std::fs::File::open(path).unwrap();
    file.seek(SeekFrom::Start(56 + from * 4)).unwrap();
    let mut bytes = vec![0u8; count * 4];
    let read = file.read(&mut bytes).unwrap();
    bytes[..read - read % 4].chunks_exact(4).map(|sample| f32::from_le_bytes(sample.try_into().unwrap())).collect()
}

/// Records the song from its start for `seconds` and returns the file of
/// the take and the frame of it that was sung to the song's first.
fn take(session: &mut Session, recorded: TrackId, seconds: f64, rate: f64) -> (String, f64) {
    session.seek(0.0);
    pass(session, 0.3);
    session.record().unwrap();
    pass(session, seconds);
    session.stop();
    let said = session.last_recording().expect("the take has ended").clone();
    assert!(said.ended_early.is_none(), "{said:?}");
    println!(
        "  the take: {:.2} s, {} frames silent, {:.1} ms taken off",
        said.takes[0].seconds, said.silent_frames, said.taken_off_ms
    );
    let track = session.project().track(recorded).unwrap();
    let audio = track.clips.iter().find_map(|clip| clip.audio()).expect("the track plays the take").clone();
    (audio.path.clone(), audio.offset_seconds * rate)
}

/// How far from its place the bump played at `song_frame` lies in a take,
/// in frames: looked for a twentieth of a second either side.
fn off(path: &str, first: f64, song_frame: f64, rate: f64) -> f64 {
    let reach = (rate * 0.05) as usize;
    let in_file = first + song_frame;
    let from = (in_file.round() as u64).saturating_sub(reach as u64);
    let near = frames_of(Path::new(path), from, 2 * reach);
    let (top, _) = near.iter().enumerate().fold(
        (0, f32::MIN),
        |best, (index, sample)| {
            if *sample > best.1 { (index, *sample) } else { best }
        },
    );
    let between = match (top.checked_sub(1).map(|before| near[before]), near.get(top + 1)) {
        (Some(a), Some(c)) => 0.5 * f64::from(a - c) / f64::from(a - 2.0 * near[top] + c),
        _ => 0.0,
    };
    from as f64 + top as f64 + between - in_file
}

fn through_the_cable(exclusive: bool) {
    let Some((mut session, folder, rate)) = session_on_the_cable(exclusive) else { return };
    let song_frames: Vec<f64> = (1..=8).map(|second| second as f64 * rate * 0.5).collect();
    let recorded = played_and_recorded(&mut session, &folder, rate, &bumps(&song_frames, (rate * 5.0) as usize));
    let said = session.recording_latency().unwrap();
    println!(
        "by the devices' account: {:.2} ms out, {:.2} ms in, {:.2} ms handed over; {:.2} ms round",
        said.output_ms, said.input_ms, said.handover_ms, said.round_trip_ms
    );

    // By what the devices say.
    let (path, first) = take(&mut session, recorded, 5.0, rate);
    let before: Vec<f64> = song_frames.iter().map(|frame| off(&path, first, *frame, rate)).collect();
    println!("placed by what the devices say: {before:.2?} frames late");

    // Measured, three times: it should say the same each time.
    let mut found = Vec::new();
    for _ in 0..3 {
        session.measure_latency(1).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.measurement() == Measurement::Running && Instant::now() < deadline {
            pass(&mut session, 0.1);
        }
        match session.measurement() {
            Measurement::Done(measured) => {
                println!(
                    "measured: {:.3} ms round, {:.3} ms more than the devices say (likeness {:.3}, heard at {:.1} dB)",
                    measured.round_trip_ms, measured.more_than_said_ms, measured.likeness, measured.heard_db
                );
                found.push(measured);
            }
            other => panic!("the measurement did not come back through the cable: {other:?}"),
        }
    }
    let spread = found.iter().map(|m| m.round_trip_ms).fold(f64::MIN, f64::max)
        - found.iter().map(|m| m.round_trip_ms).fold(f64::MAX, f64::min);
    println!("three measurements lie within {spread:.3} ms of one another");
    session.set_measured_latency(Some(found[2].beyond_ms));

    let (path, first) = take(&mut session, recorded, 5.0, rate);
    let after: Vec<f64> = song_frames.iter().map(|frame| off(&path, first, *frame, rate)).collect();
    println!("placed with the measurement taken off: {after:.2?} frames");
    let worst = after.iter().fold(0.0f64, |worst, off| worst.max(off.abs()));
    println!("drift between the two ends as it is read: {:.1} ppm", session.recording_latency().unwrap().drift_ppm);
    assert!(worst < rate * 0.002, "within two milliseconds of its place: {worst:.2} frames");

    // A null: what was recorded of four seconds of noise against what was
    // played, each at its own level, where the two lie on one another best.
    let mut seed = 77u64;
    let noise: Vec<f32> = (0..(rate * 4.0) as usize)
        .map(|index| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let edge = (index.min((rate * 4.0) as usize - index) as f32 / 2000.0).min(1.0);
            ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.1 * edge
        })
        .collect();
    // Slowly changing, so that half a frame's offset does not show as a
    // difference: three passes of a mean over nine frames.
    let mut smooth = noise;
    for _ in 0..3 {
        smooth = (0..smooth.len())
            .map(|n| smooth[n.saturating_sub(4)..(n + 5).min(smooth.len())].iter().sum::<f32>() / 9.0)
            .collect();
    }
    let level = smooth.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let smooth: Vec<f32> = smooth.iter().map(|sample| sample * 0.1 / level).collect();
    let path_played = write_wav(&file(&folder, "noise.wav"), rate, &smooth);
    let played = session.project().tracks[0].id;
    let clip = session.project().tracks[0].clips[0].id;
    session.edit(|project| project.remove_clip(clip).map(|_| ())).unwrap();
    session
        .edit(|project| project.add_audio_clip(played, "Noise", 0.0, 8.0, AudioContent::new(path_played)).map(|_| ()))
        .unwrap();
    let (path, first) = take(&mut session, recorded, 4.5, rate);
    let heard = frames_of(Path::new(&path), first.round() as u64, smooth.len());
    let mut best = (f64::MAX, 0i64);
    for shift in -24i64..=24 {
        let pairs = (2000..smooth.len() - 2000).map(|n| (smooth[n] as f64, heard[(n as i64 + shift) as usize] as f64));
        let (mut both, mut own) = (0.0, 0.0);
        for (played, heard) in pairs.clone() {
            both += played * heard;
            own += heard * heard;
        }
        let gain = both / own.max(1e-30);
        let (mut left, mut all) = (0.0, 0.0);
        for (played, heard) in pairs {
            left += (played - gain * heard).powi(2);
            all += played * played;
        }
        let db = 10.0 * (left / all).log10();
        if db < best.0 {
            best = (db, shift);
        }
    }
    println!(
        "null of what was recorded against what was played: {:.1} dB, {} frames from where it was put",
        best.0, best.1
    );
    assert!(best.0 < -20.0, "what came back is what was played");
}

#[test]
#[ignore = "plays into a cable from an output to an input of this machine, and records from its other end"]
fn a_take_through_the_cable_is_placed_by_what_a_measurement_finds_and_nulls_against_what_was_played() {
    through_the_cable(false);
}

#[test]
#[ignore = "takes both ends of the cable for this program alone"]
fn the_same_with_both_ends_of_the_cable_taken_exclusively() {
    through_the_cable(true);
}

#[test]
#[ignore = "ten minutes of the machine's time, through the cable"]
fn the_first_click_of_a_ten_minute_take_and_the_last_are_both_in_their_place() {
    let Some((mut session, folder, rate)) = session_on_the_cable(false) else { return };
    // A click every ten seconds for ten minutes.
    let song_frames: Vec<f64> = (0..60).map(|click| (0.5 + click as f64 * 10.0) * rate).collect();
    let recorded = played_and_recorded(&mut session, &folder, rate, &bumps(&song_frames, (rate * 600.0) as usize));
    // What the cable leaves unsaid, first.
    session.measure_latency(1).unwrap();
    while session.measurement() == Measurement::Running {
        pass(&mut session, 0.1);
    }
    let Measurement::Done(measured) = session.measurement() else { panic!("{:?}", session.measurement()) };
    session.set_measured_latency(Some(measured.beyond_ms));
    let (path, first) = take(&mut session, recorded, 598.0, rate);
    let offs: Vec<f64> = song_frames.iter().map(|frame| off(&path, first, *frame, rate)).collect();
    let (start, end) = (offs[0], offs[59]);
    let worst = offs.iter().fold(0.0f64, |worst, off| worst.max(off.abs()));
    println!(
        "ten minutes through the cable: the first click {start:.2} frames from its place, the last {end:.2}, the worst of sixty {worst:.2}; \
         the two ends read {:.1} ppm apart",
        session.recording_latency().unwrap().drift_ppm
    );
    assert!(worst < rate * 0.002, "{offs:.1?}");
}

/// How steady the cable itself is, with nothing of Trill between its ends:
/// bumps are written at frames of the output's own count and looked for at
/// frames of the input's own count, as the system's audio hands the two
/// over. A cable that is a wire keeps them a fixed number of frames apart.
#[test]
#[ignore = "plays into the cable and records from it through the system's audio alone"]
fn how_far_the_cable_itself_wanders() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::Mutex;
    let Some((output, input)) = ends() else { return };
    if !listeners(&input).is_empty() {
        println!("another program is recording from '{input}': nothing is played into it");
        return;
    }
    let host = cpal::default_host();
    let named = |name: &str, device: &cpal::Device| device.description().is_ok_and(|said| said.name() == name);
    let speaker = host.output_devices().unwrap().find(|device| named(&output, device)).unwrap();
    let microphone = host.input_devices().unwrap().find(|device| named(&input, device)).unwrap();
    let (out_config, in_config) =
        (speaker.default_output_config().unwrap(), microphone.default_input_config().unwrap());
    assert_eq!(out_config.sample_format(), cpal::SampleFormat::F32);
    assert_eq!(in_config.sample_format(), cpal::SampleFormat::F32);
    let rate = out_config.sample_rate() as f64;
    let (out_channels, in_channels) = (out_config.channels() as usize, in_config.channels() as usize);
    let every = (rate / 4.0) as u64;
    let mut played = 0u64;
    let heard: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let keep = heard.clone();
    let failed = |error: cpal::Error| println!("the cable's stream said: {error}");
    let taking = microphone
        .build_input_stream(
            in_config.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                keep.lock().unwrap().extend(data.chunks(in_channels).map(|frame| frame[0]));
            },
            failed,
            None,
        )
        .unwrap();
    let playing = speaker
        .build_output_stream(
            out_config.config(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for frame in data.chunks_mut(out_channels) {
                    let away = ((played % every) as f64 - 100.0) / 4.0;
                    frame.fill(if away.abs() < 10.0 { (0.1 * (-away * away).exp()) as f32 } else { 0.0 });
                    played += 1;
                }
            },
            failed,
            None,
        )
        .unwrap();
    taking.play().unwrap();
    playing.play().unwrap();
    std::thread::sleep(Duration::from_secs(20));
    drop(playing);
    drop(taking);
    let heard = heard.lock().unwrap().clone();
    // Where each bump is in what was heard, against the one before it:
    // a quarter of a second of frames, if the cable is a wire.
    let mut tops = Vec::new();
    let mut index = 1;
    while index + 1 < heard.len() {
        if heard[index] > 0.05 && heard[index] >= heard[index - 1] && heard[index] > heard[index + 1] {
            let (a, b, c) = (f64::from(heard[index - 1]), f64::from(heard[index]), f64::from(heard[index + 1]));
            tops.push(index as f64 + 0.5 * (a - c) / (a - 2.0 * b + c));
            index += 200;
        }
        index += 1;
    }
    let wander: Vec<f64> = tops.iter().enumerate().map(|(n, top)| top - tops[0] - n as f64 * every as f64).collect();
    let (least, most) =
        wander.iter().fold((f64::MAX, f64::MIN), |(least, most), off| (least.min(*off), most.max(*off)));
    println!(
        "the cable alone, over {} bumps a quarter of a second apart: the distance between its ends moves through {:.1} frames ({:.2} ms)",
        tops.len(),
        most - least,
        (most - least) / rate * 1000.0
    );
    println!("  each bump against where a wire would have it, in frames: {:.1?}", &wander[..wander.len().min(80)]);
}
