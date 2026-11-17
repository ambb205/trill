//! End-to-end tests of the engine: project in, audio out.

use std::sync::Arc;
use std::time::Duration;
use trill_core::project::{AudioContent, AutomationPoint, AutomationTarget};
use trill_core::{ChainRef, ClipId, Note, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::render::{RenderOptions, render};
use trill_engine::{AudioBackend, AudioSources, Processor, Session};
use trill_media::WavFormat;

const SR: u32 = 48_000;

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

fn note(pitch: u8, start: f64, length: f64) -> Note {
    Note { pitch, start, length, velocity: 100 }
}

/// A track with a snappy sine synth, so note edges are easy to measure.
fn sine_track(project: &mut Project, name: &str) -> TrackId {
    let track = project.add_track(name);
    let device = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    let params = &mut project.device_mut(device).unwrap().params;
    for (key, value) in [("osc1_wave", 0.0), ("attack", 0.001), ("release", 0.01), ("sustain", 1.0)] {
        params.insert(key.to_string(), value);
    }
    track
}

fn clip_with(project: &mut Project, track: TrackId, start: f64, length: f64, notes: &[Note]) -> ClipId {
    let clip = project.add_midi_clip(track, "clip", start, length).unwrap();
    project.clip_mut(clip).unwrap().add_notes(notes).unwrap();
    clip
}

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR, max_tail_seconds: 2.0, ..RenderOptions::default() }
}

fn seconds(range: std::ops::Range<f64>) -> std::ops::Range<usize> {
    (range.start * SR as f64) as usize..(range.end * SR as f64) as usize
}

/// Writes a mono signal as a WAV file at `sample_rate`.
fn write_wav(path: &std::path::Path, sample_rate: u32, samples: &[f32]) {
    let buffer = trill_core::audio::AudioBuffer {
        sample_rate: sample_rate as f64,
        left: samples.to_vec(),
        right: samples.to_vec(),
    };
    trill_media::write_wav(path, &buffer, WavFormat::Float32).unwrap();
}

#[test]
fn notes_sound_exactly_where_they_are_written() {
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "A");
    // 120 BPM: beat 1 is at 0.5 s and the note lasts until 1.0 s.
    clip_with(&mut project, track, 0.0, 4.0, &[note(69, 1.0, 1.0)]);

    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    assert!(out.problems.is_empty());
    assert_eq!(peak(&out.left[seconds(0.0..0.499)]), 0.0, "silent before the note");
    assert!(rms(&out.left[seconds(0.51..0.99)]) > 0.05, "sounding during the note");
    assert!(peak(&out.left[seconds(1.1..2.0)]) < 1e-3, "silent after the release");

    // The first non-zero sample is within a sample of 0.5 s.
    let onset = out.left.iter().position(|s| s.abs() > 0.0).unwrap();
    assert!((onset as i64 - 24_000).abs() <= 2, "onset at frame {onset}");
}

#[test]
fn rendering_is_deterministic() {
    let mut project = Project::new("t");
    let track = project.add_track("Pad");
    let synth = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    project.device_mut(synth).unwrap().params.insert("unison".into(), 5.0);
    project.add_device(ChainRef::Track(track), "builtin:reverb", "Reverb", None).unwrap();
    clip_with(&mut project, track, 0.0, 4.0, &[note(60, 0.0, 2.0), note(64, 0.5, 2.0), note(67, 1.0, 2.0)]);

    let first = render(&project, &BuiltinFactory, &options()).unwrap();
    let second = render(&project, &BuiltinFactory, &options()).unwrap();
    assert!(first.left == second.left && first.right == second.right);
    assert!(rms(&first.left) > 0.01);
}

/// Recompiling and swapping the graph on every block, as happens while the
/// user edits during playback, must be inaudible: running devices, sounding
/// notes and fader positions all carry over.
#[test]
fn swapping_graphs_mid_playback_changes_nothing() {
    let mut project = Project::new("t");
    let pad = project.add_track("Pad");
    project.add_device(ChainRef::Track(pad), "builtin:polysynth", "Synth", None).unwrap();
    project.add_device(ChainRef::Track(pad), "builtin:delay", "Delay", None).unwrap();
    clip_with(&mut project, pad, 0.0, 8.0, &[note(48, 0.0, 6.0), note(55, 0.25, 6.0), note(60, 1.0, 0.5)]);
    let drums = project.add_track("Drums");
    project.add_device(ChainRef::Track(drums), "builtin:drumkit", "Drums", None).unwrap();
    let hits: Vec<Note> = (0..16).map(|i| note(if i % 4 == 0 { 36 } else { 42 }, i as f64 * 0.5, 0.25)).collect();
    clip_with(&mut project, drums, 0.0, 8.0, &hits);
    project.set_track_pan(drums, 0.3).unwrap();
    project.add_device(ChainRef::Master, "builtin:limiter", "Limiter", None).unwrap();

    let run = |swap_every_block: bool| {
        let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
        let mut compile = |project: &Project| {
            Compiler {
                factory: &BuiltinFactory,
                bank: &mut bank,
                meters: &mut meters,
                audio: &audio,
                stand_ins: None,
                sample_rate: SR as f64,
                purpose: trill_core::device::Purpose::Playing,
            }
            .compile(project, true)
            .0
        };
        let mut processor = Processor::new(SR as f64);
        drop(processor.swap_graph(compile(&project)));
        processor.play();
        let (mut left, mut right) = (vec![0.0f32; 96_000], vec![0.0f32; 96_000]);
        for (l, r) in left.chunks_mut(441).zip(right.chunks_mut(441)) {
            if swap_every_block {
                drop(processor.swap_graph(compile(&project)));
            }
            processor.process(l, r);
        }
        (left, right)
    };

    let (steady_l, steady_r) = run(false);
    let (swapped_l, swapped_r) = run(true);
    assert!(rms(&steady_l) > 0.01);
    assert!(steady_l == swapped_l && steady_r == swapped_r, "graph swaps altered the audio");
}

/// Notes that land exactly on a block boundary are the classic place to
/// drop or double a note. Whatever the buffer size, the audio must match.
#[test]
fn output_does_not_depend_on_the_block_size() {
    let mut project = Project::new("t");
    let synth = sine_track(&mut project, "A");
    // At 120 BPM and 48 kHz every beat falls on a multiple of 480 frames.
    let melody: Vec<Note> = (0..16).map(|i| note(60 + (i % 5) as u8, i as f64 * 0.5, 0.5)).collect();
    clip_with(&mut project, synth, 0.0, 8.0, &melody);
    let drums = project.add_track("Drums");
    project.add_device(ChainRef::Track(drums), "builtin:drumkit", "Drums", None).unwrap();
    let hits: Vec<Note> = (0..32).map(|i| note(if i % 4 == 0 { 36 } else { 42 }, i as f64 * 0.25, 0.25)).collect();
    clip_with(&mut project, drums, 0.0, 8.0, &hits);

    let run = |block: usize| {
        let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
        let (graph, _) = Compiler {
            factory: &BuiltinFactory,
            bank: &mut bank,
            meters: &mut meters,
            audio: &audio,
            stand_ins: None,
            sample_rate: SR as f64,
            purpose: trill_core::device::Purpose::Playing,
        }
        .compile(&project, true);
        let mut processor = Processor::new(SR as f64);
        drop(processor.swap_graph(graph));
        processor.play();
        let mut left = vec![0.0f32; 4 * SR as usize];
        let mut right = left.clone();
        for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
            processor.process(l, r);
        }
        left
    };

    let reference = run(512);
    assert!(rms(&reference) > 0.01);
    for block in [480, 37, 1, 4096] {
        assert!(run(block) == reference, "block size {block} produced different audio");
    }
}

#[test]
fn loop_wraps_on_the_exact_sample() {
    let mut project = Project::new("t");
    let track = project.add_track("Drums");
    project.add_device(ChainRef::Track(track), "builtin:drumkit", "Drums", None).unwrap();
    clip_with(&mut project, track, 0.0, 4.0, &[note(42, 0.0, 0.1)]);
    // One beat at 120 BPM is 24000 frames.
    project.set_loop(true, 0.0, 1.0).unwrap();

    let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
    let (graph, _) = Compiler {
        factory: &BuiltinFactory,
        bank: &mut bank,
        meters: &mut meters,
        audio: &audio,
        stand_ins: None,
        sample_rate: SR as f64,
        purpose: trill_core::device::Purpose::Playing,
    }
    .compile(&project, true);
    let mut processor = Processor::new(SR as f64);
    drop(processor.swap_graph(graph));
    processor.play();

    let (mut left, mut right) = (vec![0.0f32; 60_000], vec![0.0f32; 60_000]);
    for (l, r) in left.chunks_mut(997).zip(right.chunks_mut(997)) {
        processor.process(l, r);
    }
    assert!((processor.position() - 0.5).abs() < 1e-9, "position {}", processor.position());
    for hit in [0usize, 24_000, 48_000] {
        assert!(peak(&left[hit..hit + 200]) > 0.01, "no hat at frame {hit}");
        if hit > 0 {
            assert!(peak(&left[hit - 2000..hit]) < 1e-3, "hat sounded early before frame {hit}");
        }
    }
}

#[test]
fn mute_and_solo_decide_what_is_heard() {
    let mut project = Project::new("t");
    let a = sine_track(&mut project, "A");
    let b = sine_track(&mut project, "B");
    clip_with(&mut project, a, 0.0, 2.0, &[note(60, 0.0, 2.0)]);
    clip_with(&mut project, b, 0.0, 2.0, &[note(67, 0.0, 2.0)]);

    let peaks = |project: &Project| {
        let out = render(project, &BuiltinFactory, &options()).unwrap();
        (out.track_peaks[0].1, out.track_peaks[1].1)
    };
    let (pa, pb) = peaks(&project);
    assert!(pa > 0.05 && pb > 0.05);

    project.track_mut(a).unwrap().mute = true;
    let (pa, pb) = peaks(&project);
    assert!(pa == 0.0 && pb > 0.05);

    project.track_mut(a).unwrap().mute = false;
    project.track_mut(a).unwrap().solo = true;
    let (pa, pb) = peaks(&project);
    assert!(pa > 0.05 && pb == 0.0);
}

#[test]
fn automation_drives_volume_and_device_parameters() {
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "A");
    clip_with(&mut project, track, 0.0, 8.0, &[note(69, 0.0, 8.0)]);
    let fade = vec![AutomationPoint::new(0.0, -60.0), AutomationPoint::new(8.0, 0.0)];
    project.set_automation(AutomationTarget::TrackVolume { track }, fade).unwrap();
    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    let early = rms(&out.left[seconds(0.1..0.5)]);
    let late = rms(&out.left[seconds(3.4..3.9)]);
    assert!(late > early * 100.0, "fade-in not applied: early {early}, late {late}");

    // Automate a filter closed: the tone must get much quieter at the end.
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "A");
    clip_with(&mut project, track, 0.0, 8.0, &[note(93, 0.0, 8.0)]);
    let filter = project.add_device(ChainRef::Track(track), "builtin:filter", "Filter", None).unwrap();
    let sweep = vec![AutomationPoint::new(0.0, 20_000.0), AutomationPoint::new(6.0, 60.0)];
    project.set_automation(AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() }, sweep).unwrap();
    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    let open = rms(&out.left[seconds(0.1..0.5)]);
    let closed = rms(&out.left[seconds(3.4..3.9)]);
    assert!(closed < open * 0.02, "filter sweep not applied: open {open}, closed {closed}");
}

#[test]
fn automation_that_is_switched_off_drives_nothing() {
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "A");
    clip_with(&mut project, track, 0.0, 8.0, &[note(93, 0.0, 8.0)]);
    let filter = project.add_device(ChainRef::Track(track), "builtin:filter", "Filter", None).unwrap();
    let plain = render(&project, &BuiltinFactory, &options()).unwrap();

    let volume = AutomationTarget::TrackVolume { track };
    let cutoff = AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() };
    let master = AutomationTarget::MasterVolume;
    project
        .set_automation(volume.clone(), vec![AutomationPoint::new(0.0, -60.0), AutomationPoint::new(8.0, 0.0)])
        .unwrap();
    project
        .set_automation(cutoff.clone(), vec![AutomationPoint::new(0.0, 20_000.0), AutomationPoint::new(6.0, 60.0)])
        .unwrap();
    project.set_automation(master.clone(), vec![AutomationPoint::new(0.0, -30.0)]).unwrap();
    let automated = render(&project, &BuiltinFactory, &options()).unwrap();
    assert_ne!(automated.left, plain.left);

    // Off, every kind of lane leaves its target where it is set by hand:
    // the song is, to the sample, the one without automation.
    for target in [&volume, &cutoff, &master] {
        project.set_automation_enabled(target, false).unwrap();
    }
    let off = render(&project, &BuiltinFactory, &options()).unwrap();
    assert!(off.left == plain.left && off.right == plain.right, "a lane that is off still moves something");

    // And on again, the one with it.
    for target in [&volume, &cutoff, &master] {
        project.set_automation_enabled(target, true).unwrap();
    }
    let on = render(&project, &BuiltinFactory, &options()).unwrap();
    assert!(on.left == automated.left && on.right == automated.right);
}

#[test]
fn audio_clips_play_in_time_and_in_tune() {
    let dir = std::env::temp_dir().join(format!("trill-test-{}", std::process::id()));
    let path = dir.join("tone.wav");
    // One second of 440 Hz at 44.1 kHz, so the engine has to resample.
    let tone: Vec<f32> =
        (0..44_100).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44_100.0).sin() * 0.5).collect();
    write_wav(&path, 44_100, &tone);

    let mut project = Project::new("t");
    let track = project.add_track("Audio");
    let content = AudioContent::new(path.display().to_string());
    project.add_audio_clip(track, "tone", 2.0, 2.0, content).unwrap();

    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    assert_eq!(peak(&out.left[seconds(0.0..0.999)]), 0.0, "silent before the clip");
    let body = &out.left[seconds(1.1..1.9)];
    assert!((peak(body) - 0.5).abs() < 0.01, "level changed: {}", peak(body));
    let crossings = body.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    let hz = crossings as f64 / 0.8;
    assert!((hz - 440.0).abs() < 2.0, "expected 440 Hz, measured {hz}");
}

#[test]
fn missing_audio_files_are_reported_not_fatal() {
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "A");
    clip_with(&mut project, track, 0.0, 1.0, &[note(60, 0.0, 1.0)]);
    let content = AudioContent::new("does/not/exist.wav");
    project.add_audio_clip(track, "gone", 0.0, 1.0, content).unwrap();
    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    assert_eq!(out.problems.len(), 1);
    assert!(rms(&out.left) > 0.01, "the rest of the project still renders");
}

/// A stand-in for a hosted plugin: one parameter whose real value lives
/// inside the device and can only be learned by asking its controller.
struct StatefulFactory;

struct Silent;

impl trill_core::device::DeviceProcessor for Silent {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}
    fn set_param(&mut self, _index: usize, _value: f64) {}
    fn process(
        &mut self,
        _: &trill_core::device::ProcessInfo,
        _: &[trill_core::device::Event],
        _: &mut [f32],
        _: &mut [f32],
    ) {
    }
    fn reset(&mut self) {}
}

struct Remembers(Vec<u8>);

impl trill_core::device::DeviceController for Remembers {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        Some(self.0.clone())
    }

    fn param_values(&mut self) -> Option<Vec<f64>> {
        Some(vec![0.7])
    }
}

impl trill_core::device::DeviceFactory for StatefulFactory {
    fn descriptors(&self) -> Vec<trill_core::device::DeviceDescriptor> {
        vec![trill_core::device::DeviceDescriptor {
            uri: "test:stateful".into(),
            name: "Stateful".into(),
            vendor: "Test".into(),
            category: trill_core::device::DeviceCategory::Effect,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(
        &self,
        device: &trill_core::Device,
        _sample_rate: f64,
        _max_block: usize,
    ) -> Result<trill_core::device::DeviceInstance, trill_core::device::DeviceError> {
        if device.uri != "test:stateful" {
            return Err(trill_core::device::DeviceError::Unknown(device.uri.clone()));
        }
        let amount = trill_core::device::ParamInfo {
            id: "amount".into(),
            name: "Amount".into(),
            min: 0.0,
            max: 1.0,
            default: 0.2,
            unit: String::new(),
            kind: trill_core::device::ParamKind::Float,
        };
        // A restored instance carries its saved state; a new one starts fresh.
        let state = device.state.clone().unwrap_or_else(|| b"fresh".to_vec());
        Ok(trill_core::device::DeviceInstance {
            processor: Box::new(Silent),
            params: vec![amount],
            controller: Some(Box::new(Remembers(state))),
        })
    }
}

#[test]
fn stateful_devices_report_their_own_values_and_survive_undo() {
    let factory = trill_core::device::CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(StatefulFactory)]);
    let mut session = Session::new(Arc::new(factory), AudioBackend::Null { sample_rate: SR }).unwrap();
    let device = session.add_device(ChainRef::Master, "test:stateful", None, None).unwrap();
    let amount = |session: &Session| session.device_params(device).unwrap()[0].value;

    assert_eq!(amount(&session), 0.7, "the device's own value, not the descriptor default");
    session.set_device_params(device, &[("amount".into(), 0.4)]).unwrap();
    assert_eq!(amount(&session), 0.4, "a value set through the session wins at once");

    // Removing the device stores its state in the undo history, so undo
    // brings back the same device rather than a fresh one.
    session.edit(|project| project.remove_device(device)).unwrap();
    assert!(session.device_params(device).is_err());
    assert!(session.undo());
    assert_eq!(session.project().device(device).unwrap().state.as_deref(), Some(&b"fresh"[..]));
    assert_eq!(amount(&session), 0.4);

    // Giving the device a different state creates it again from that state;
    // undo brings the old one back.
    session.set_device_state(device, Some(b"other".to_vec())).unwrap();
    let saved_state = |session: &mut Session| {
        let dir = std::env::temp_dir().join(format!("trill-state-{}", std::process::id()));
        let file = dir.join("p.trill");
        session.save(&file).unwrap();
        let project = Project::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        project.device(device).unwrap().state.clone()
    };
    assert_eq!(saved_state(&mut session).as_deref(), Some(&b"other"[..]), "the running device holds the new state");
    assert!(session.undo());
    assert_eq!(saved_state(&mut session).as_deref(), Some(&b"fresh"[..]), "undo recreated it from the old state");

    // Saving captures state into the project file.
    let dir = std::env::temp_dir().join(format!("trill-stateful-{}", std::process::id()));
    let file = dir.join("p.trill");
    session.save(&file).unwrap();
    let saved = std::fs::read_to_string(&file).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(saved.contains("\"state\": \"ZnJlc2g=\""), "state is stored base64-encoded: {saved}");
}

/// Index of the first sample louder than silence, in seconds.
fn onset_seconds(samples: &[f32]) -> f64 {
    samples.iter().position(|s| s.abs() > 1e-4).expect("the render is not silent") as f64 / SR as f64
}

#[test]
fn trimmed_split_and_looped_clips_play_what_they_show() {
    // Notes at clip beats 0, 2 and 4 (0.5 s apart at 120 BPM).
    let build = || {
        let mut project = Project::new("t");
        let track = sine_track(&mut project, "A");
        let clip =
            clip_with(&mut project, track, 0.0, 8.0, &[note(60, 0.0, 0.5), note(64, 2.0, 0.5), note(67, 4.0, 0.5)]);
        (project, clip)
    };

    // Trimming the left edge to beat 1 hides the first note; the second
    // still sounds at beat 2 (1.0 s), not earlier and not later.
    let (mut project, clip) = build();
    project.trim_clip_start(clip, 1.0).unwrap();
    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    assert!((onset_seconds(&out.left) - 1.0).abs() < 0.002, "onset {}", onset_seconds(&out.left));

    // Splitting changes nothing audible.
    let (project, _) = build();
    let whole = render(&project, &BuiltinFactory, &options()).unwrap();
    let (mut project, clip) = build();
    project.split_clip(clip, 3.0).unwrap();
    let split = render(&project, &BuiltinFactory, &options()).unwrap();
    assert!(whole.left == split.left && whole.right == split.right, "a split must be inaudible");

    // Deleting the first half leaves the rest exactly in place.
    let (mut project, clip) = build();
    project.split_clip(clip, 3.0).unwrap();
    project.remove_clip(clip).unwrap();
    let rest = render(&project, &BuiltinFactory, &options()).unwrap();
    assert!((onset_seconds(&rest.left) - 2.0).abs() < 0.002, "onset {}", onset_seconds(&rest.left));

    // A two-beat loop entered one beat in: the note on beat 0 of the loop
    // first sounds one beat after the clip starts, then every two beats.
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "A");
    let clip = clip_with(&mut project, track, 0.0, 8.0, &[note(60, 0.0, 0.25)]);
    project.clip_mut(clip).unwrap().set_loop_length(Some(2.0)).unwrap();
    project.trim_clip_start(clip, 1.0).unwrap();
    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    assert_eq!(peak(&out.left[seconds(0.0..0.99)]), 0.0, "the clip now starts at beat 1 and its note at beat 2");
    for beat in [2.0, 4.0, 6.0] {
        let at = beat * 0.5;
        assert!(rms(&out.left[seconds(at + 0.01..at + 0.1)]) > 0.02, "no note at beat {beat}");
        assert!(peak(&out.left[seconds(at - 0.3..at - 0.01)]) < 1e-3, "sound before beat {beat}");
    }
}

#[test]
fn audio_fades_and_master_automation_shape_the_level() {
    let dir = std::env::temp_dir().join(format!("trill-fades-{}", std::process::id()));
    let path = dir.join("tone.wav");
    let tone: Vec<f32> =
        (0..96_000).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5).collect();
    write_wav(&path, 48_000, &tone);

    let mut project = Project::new("t");
    let track = project.add_track("Audio");
    // Four beats = 2 s; fade in over the first beat, out over the last two.
    let clip = project.add_audio_clip(track, "tone", 0.0, 4.0, AudioContent::new(path.display().to_string())).unwrap();
    project.set_clip_fades(clip, 1.0, 2.0).unwrap();
    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    let level = |range: std::ops::Range<f64>| peak(&out.left[seconds(range)]);
    assert!(level(0.0..0.05) < 0.06, "the fade-in starts from silence: {}", level(0.0..0.05));
    assert!((level(0.24..0.26) - 0.25).abs() < 0.03, "halfway through the fade-in: {}", level(0.24..0.26));
    assert!((level(0.6..0.9) - 0.5).abs() < 0.01, "full level between the fades");
    assert!((level(1.49..1.51) - 0.25).abs() < 0.03, "halfway through the fade-out: {}", level(1.49..1.51));
    assert!(level(1.95..2.0) < 0.03, "the fade-out ends in silence");

    // Master volume: held at -60 dB for the first beat, then a jump to 0 dB.
    project.set_clip_fades(clip, 0.0, 0.0).unwrap();
    let points = vec![
        AutomationPoint { beat: 0.0, value: -60.0, shape: trill_core::Segment::Hold },
        AutomationPoint::new(1.0, 0.0),
    ];
    project.set_automation(AutomationTarget::MasterVolume, points).unwrap();
    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(peak(&out.left[seconds(0.1..0.45)]) < 0.001, "held down before beat 1");
    assert!((peak(&out.left[seconds(0.7..1.0)]) - 0.5).abs() < 0.01, "at full level after the jump");
}

#[test]
fn groups_and_returns_carry_the_signal_they_are_sent() {
    use trill_core::TrackKind;
    // One held sine; its level at the master tells us where it went.
    let build = || {
        let mut project = Project::new("t");
        let track = sine_track(&mut project, "A");
        clip_with(&mut project, track, 0.0, 4.0, &[note(69, 0.0, 4.0)]);
        (project, track)
    };
    let level = |project: &Project| {
        let out = render(project, &BuiltinFactory, &options()).unwrap();
        (rms(&out.left[seconds(0.5..1.5)]), out)
    };
    let (project, _) = build();
    let (dry, _) = level(&project);
    assert!(dry > 0.05);

    // A send at unity to an empty return doubles the signal at the master;
    // at -6 dB it adds half; muting the return removes it again.
    let (mut project, track) = build();
    let verb = project.add_track_of_kind("Return", TrackKind::Return);
    project.set_send(track, verb, Some(0.0)).unwrap();
    let (with_send, out) = level(&project);
    assert!((with_send / dry - 2.0).abs() < 0.01, "ratio {}", with_send / dry);
    let return_peak = out.track_peaks.iter().find(|(id, _)| *id == verb).unwrap().1;
    assert!(return_peak > 0.05, "the return's meter shows what it received");
    project.set_send(track, verb, Some(-6.0206)).unwrap();
    assert!((level(&project).0 / dry - 1.5).abs() < 0.01);
    project.track_mut(verb).unwrap().mute = true;
    assert!((level(&project).0 / dry - 1.0).abs() < 0.01);

    // The send is taken after the fader: pull the track down and the
    // return follows.
    let (mut project, track) = build();
    let verb = project.add_track_of_kind("Return", TrackKind::Return);
    project.set_send(track, verb, Some(0.0)).unwrap();
    project.set_track_volume(track, -6.0206).unwrap();
    assert!((level(&project).0 / dry - 1.0).abs() < 0.01, "half the level, twice (direct + return)");

    // A group's fader and devices act on everything routed through it.
    let (mut project, track) = build();
    let second = sine_track(&mut project, "B");
    clip_with(&mut project, second, 0.0, 4.0, &[note(69, 0.0, 4.0)]);
    let bus = project.add_track_of_kind("Bus", TrackKind::Group);
    project.set_track_group(track, Some(bus)).unwrap();
    project.set_track_group(second, Some(bus)).unwrap();
    let (both, _) = level(&project);
    assert!((both / dry - 2.0).abs() < 0.01, "two identical tracks through a unity group");
    project.set_track_volume(bus, -6.0206).unwrap();
    assert!((level(&project).0 / dry - 1.0).abs() < 0.01, "the group fader halves both");
    let utility = project.add_device(ChainRef::Track(bus), "builtin:utility", "Utility", None).unwrap();
    project.device_mut(utility).unwrap().params.insert("gain".into(), -6.0206);
    assert!((level(&project).0 / dry - 0.5).abs() < 0.01, "and its devices process the sum");
    project.track_mut(bus).unwrap().mute = true;
    assert_eq!(level(&project).0, 0.0, "muting the group silences its members");

    // Solo: a soloed member is heard through its group; the other is not.
    project.track_mut(bus).unwrap().mute = false;
    project.track_mut(track).unwrap().solo = true;
    assert!((level(&project).0 / dry - 0.25).abs() < 0.01, "one member, through the -6 dB fader and -6 dB utility");
}

fn session() -> Session {
    Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: SR }).unwrap()
}

#[test]
fn session_edits_undo_and_redo() {
    let mut session = session();
    let empty = session.project().clone();
    assert_eq!(empty.master.devices.len(), 1, "new projects get a master limiter");
    assert!(!session.can_undo());

    let track = session.edit(|p| Ok(p.add_track("Keys"))).unwrap();
    let with_track = session.project().clone();
    session.edit(|p| p.add_midi_clip(track, "c", 0.0, 4.0)).unwrap();
    let with_clip = session.project().clone();

    // A failing edit changes nothing and adds no undo step.
    let revision = session.revision();
    let failed = session.edit(|p| {
        p.add_track("Half done");
        p.set_tempo(-5.0)
    });
    assert!(failed.is_err());
    assert_eq!(session.project(), &with_clip);
    assert_eq!(session.revision(), revision);

    assert!(session.undo());
    assert_eq!(session.project(), &with_track);
    assert!(session.undo());
    assert_eq!(session.project(), &empty);
    assert!(!session.undo());
    assert!(session.redo() && session.redo());
    assert_eq!(session.project(), &with_clip);
    assert!(!session.redo());

    // A new edit discards the redo history.
    session.undo();
    session.edit(|p| p.set_tempo(90.0)).unwrap();
    assert!(!session.can_redo());
}

#[test]
fn session_devices_presets_and_parameters() {
    let mut session = session();
    let track = session.edit(|p| Ok(p.add_track("Bass"))).unwrap();
    let chain = ChainRef::Track(track);

    let before = session.project().clone();
    assert!(session.add_device(chain, "builtin:nope", None, None).is_err());
    assert!(session.add_device(chain, "builtin:polysynth", Some("no_such_preset"), None).is_err());
    assert_eq!(session.project(), &before, "failed adds leave the project untouched");

    let synth = session.add_device(chain, "builtin:polysynth", Some("bass"), None).unwrap();
    let value = |session: &Session, id: &str| {
        session.device_params(synth).unwrap().into_iter().find(|p| p.info.id == id).unwrap().value
    };
    assert_eq!(value(&session, "mono"), 1.0, "preset values are applied");
    assert_eq!(value(&session, "lfo_rate"), 5.0, "unset parameters report their default");

    session.set_device_params(synth, &[("cutoff".into(), 1e9), ("resonance".into(), 0.5)]).unwrap();
    assert_eq!(value(&session, "cutoff"), 20_000.0, "values are clamped to range");
    assert_eq!(value(&session, "resonance"), 0.5);
    assert!(session.set_device_params(synth, &[("bogus".into(), 1.0)]).is_err());

    session.undo();
    assert_eq!(value(&session, "resonance"), 0.25, "undo restores the preset value");

    session.apply_preset(synth, "sub_bass").unwrap();
    assert_eq!(value(&session, "osc1_wave"), 0.0);
    assert_eq!(value(&session, "resonance"), 0.1, "values the preset does not set return to default");
    assert!(session.apply_preset(synth, "nope").is_err());
}

#[test]
fn a_gesture_is_one_undo_step() {
    let mut session = session();
    let track = session.edit(|p| Ok(p.add_track("A"))).unwrap();
    let before = session.project().clone();

    session.begin_gesture();
    for step in 1..=20 {
        session.edit(|p| p.set_track_volume(track, -(step as f32))).unwrap();
    }
    session.end_gesture();
    assert_eq!(session.project().track(track).unwrap().volume_db, -20.0);

    session.edit(|p| p.set_track_pan(track, 0.5)).unwrap();
    assert!(session.undo());
    assert_eq!(session.project().track(track).unwrap().volume_db, -20.0, "the edit after the gesture is its own step");
    assert!(session.undo());
    assert_eq!(session.project(), &before, "the whole drag undoes at once");
}

#[test]
fn session_transport_runs_in_real_time() {
    let mut session = session();
    let track = session.edit(|p| Ok(p.add_track("A"))).unwrap();
    session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
    session
        .edit(|p| {
            let clip = p.add_midi_clip(track, "c", 0.0, 16.0)?;
            p.clip_mut(clip)?.add_notes(&[note(60, 0.0, 16.0)])
        })
        .unwrap();

    assert!(!session.transport().playing);
    session.play();
    std::thread::sleep(Duration::from_millis(300));
    let status = session.transport();
    assert!(status.playing);
    // 300 ms at 120 BPM is 0.6 beats; allow generous scheduling slack.
    assert!(status.position_beats > 0.2 && status.position_beats < 2.0, "position {}", status.position_beats);
    let (l, r) = session.track_meter(track).unwrap().take();
    assert!(l > 0.01 && r > 0.01, "the track meter shows signal");

    // Edit while playing, then stop and rewind.
    session.edit(|p| p.set_tempo(140.0)).unwrap();
    session.stop();
    session.seek(4.0);
    std::thread::sleep(Duration::from_millis(100));
    let status = session.transport();
    assert!(!status.playing);
    assert_eq!(status.position_beats, 4.0);
    assert_eq!(session.audio_faults(), 0);
}

/// Parameter changes reach the running device (without a graph swap), and
/// undo sends the old value back.
#[test]
fn live_parameter_changes_are_heard() {
    let mut session = session();
    let track = session.edit(|p| Ok(p.add_track("A"))).unwrap();
    let synth = session.add_device(ChainRef::Track(track), "builtin:polysynth", None, None).unwrap();
    session
        .edit(|p| {
            let clip = p.add_midi_clip(track, "c", 0.0, 64.0)?;
            p.clip_mut(clip)?.add_notes(&[note(60, 0.0, 64.0)])
        })
        .unwrap();
    let meter = session.track_meter(track).unwrap();
    let level = |meter: &trill_engine::Meter| {
        std::thread::sleep(Duration::from_millis(120));
        meter.take();
        std::thread::sleep(Duration::from_millis(120));
        meter.take().0
    };

    session.play();
    let loud = level(&meter);
    assert!(loud > 0.05, "expected signal, got {loud}");

    session.set_device_params(synth, &[("volume".into(), -61.0)]).unwrap();
    let quiet = level(&meter);
    assert!(quiet < loud * 0.05, "volume change not heard: {loud} -> {quiet}");

    assert!(session.undo());
    let restored = level(&meter);
    assert!(restored > loud * 0.5, "undo did not restore the level: {restored}");
    session.stop();
}

#[test]
fn auditioning_and_the_metronome_are_heard_but_not_rendered() {
    let mut session = session();
    let track = session.edit(|p| Ok(p.add_track("A"))).unwrap();
    session.edit(|p| p.add_midi_clip(track, "empty", 0.0, 8.0)).unwrap();
    let master = session.master_meter();
    let tone: Vec<f32> = (0..96_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let audio = Arc::new(trill_core::audio::AudioBuffer { sample_rate: 48_000.0, left: tone.clone(), right: tone });

    // The song is silent and stopped; the audition is still heard, and it
    // does not go through (or register on) the master.
    session.audition(audio, 0.0);
    std::thread::sleep(Duration::from_millis(150));
    assert!(session.is_auditioning());
    session.stop_audition();
    std::thread::sleep(Duration::from_millis(150));
    assert!(!session.is_auditioning());
    assert_eq!(master.take(), (0.0, 0.0), "auditioning is not part of the mix");

    session.set_metronome(true);
    assert!(session.metronome());
    session.play();
    std::thread::sleep(Duration::from_millis(150));
    session.stop();
    assert_eq!(session.audio_faults(), 0);

    // A render of the same project is pure silence: no click, no audition.
    let out = session.render_job().render(&options()).unwrap();
    assert_eq!(peak(&out.left), 0.0);
}

#[test]
fn session_saves_loads_and_renders() {
    let dir = std::env::temp_dir().join(format!("trill-session-{}", std::process::id()));
    let project_path = dir.join("song.trill.json");
    let wav_path = dir.join("song.wav");

    let mut session = session();
    let track = session.edit(|p| Ok(p.add_track("Lead"))).unwrap();
    session.add_device(ChainRef::Track(track), "builtin:polysynth", Some("pluck"), None).unwrap();
    session
        .edit(|p| {
            let clip = p.add_midi_clip(track, "c", 0.0, 4.0)?;
            p.clip_mut(clip)?.add_notes(&[note(60, 0.0, 1.0), note(64, 1.0, 1.0), note(67, 2.0, 2.0)])
        })
        .unwrap();
    assert!(session.is_modified());
    session.save(&project_path).unwrap();
    assert!(!session.is_modified());
    let saved = session.project().clone();

    let report = session.render_job().render_to_file(&wav_path, &options()).unwrap();
    assert!(report.stats.duration_seconds >= 2.0);
    assert!(report.stats.peak_db > -30.0 && report.stats.peak_db <= 0.0);
    assert_eq!(report.stats.clipped_samples, 0);
    assert_eq!(report.tracks.len(), 1);
    assert!(std::fs::metadata(&wav_path).unwrap().len() > 100_000);

    session.new_project();
    assert!(session.project().tracks.is_empty());
    session.load(&project_path).unwrap();
    assert_eq!(session.project(), &saved);
    assert!(!session.can_undo(), "loading starts a fresh history");
    assert!(session.problems().is_empty());

    std::fs::remove_dir_all(&dir).ok();
}
