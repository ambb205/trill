//! A song whose tempo and time signature change on the way, heard: every
//! note at the frame its beat falls on, whatever the buffer size; what a
//! device is told as the changes pass; a loop across a change; audio that
//! keeps its own speed and audio that follows the tempo; an envelope; the
//! metronome and the count-in in the meter they are in. Run with
//! `--nocapture` for the numbers.

mod common;
mod rig;

use common::{add_clip, click_times, click_track, file, folder, peak, write_wav, zero_crossing_hz};
use rig::{FRAMES_PER_BEAT, Rig, SR};
use std::sync::{Arc, Mutex};
use trill_core::device::{
    CompositeFactory, DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    Event, EventKind, ProcessInfo, Purpose,
};
use trill_core::project::{AutomationPoint, AutomationTarget, Segment, WarpMode};
use trill_core::{ChainRef, Device, Note, Project, TimeSignature, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::render::{RenderOptions, render};
use trill_engine::{AudioSources, Processor, RecordState};

const TICK_URI: &str = "test:tick";

/// What a device was told about the song at the start of a block.
#[derive(Copy, Clone, Debug)]
struct Told {
    /// The block's first frame, counted from the device's first.
    frame: usize,
    frames: usize,
    tempo: f64,
    signature: TimeSignature,
    beat: f64,
}

/// An instrument that answers every note with one sample, at the frame the
/// note was placed on, and writes down what it is told about the song.
struct Ticker {
    frame: usize,
    told: Arc<Mutex<Vec<Told>>>,
}

impl DeviceProcessor for Ticker {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, info: &ProcessInfo, events: &[Event], left: &mut [f32], right: &mut [f32]) {
        for event in events {
            if let EventKind::NoteOn { velocity, .. } = event.kind {
                left[event.offset as usize] += velocity;
                right[event.offset as usize] += velocity;
            }
        }
        if info.playing {
            self.told.lock().unwrap().push(Told {
                frame: self.frame,
                frames: left.len(),
                tempo: info.tempo,
                signature: info.time_signature,
                beat: info.position_beats,
            });
            self.frame += left.len();
        }
    }

    fn reset(&mut self) {}
}

struct TickerFactory(Arc<Mutex<Vec<Told>>>);

impl DeviceFactory for TickerFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        vec![DeviceDescriptor {
            uri: TICK_URI.into(),
            name: "Ticker".into(),
            vendor: "Test".into(),
            category: DeviceCategory::Instrument,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != TICK_URI {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        let ticker = Ticker { frame: 0, told: self.0.clone() };
        Ok(DeviceInstance { processor: Box::new(ticker), params: Vec::new(), controller: None })
    }
}

/// The built-in devices and the ticker, and what the tickers are told.
fn factory() -> (CompositeFactory, Arc<Mutex<Vec<Told>>>) {
    let told = Arc::new(Mutex::new(Vec::new()));
    (CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(TickerFactory(told.clone()))]), told)
}

/// A track whose instrument ticks at each of `beats`.
fn ticking(project: &mut Project, beats: &[f64]) -> TrackId {
    let track = project.add_track("Ticks");
    project.add_device(ChainRef::Track(track), TICK_URI, "Ticker", None).unwrap();
    let length = beats.iter().fold(0.0f64, |end, beat| end.max(*beat)).ceil() + 1.0;
    let clip = project.add_midi_clip(track, "ticks", 0.0, length).unwrap();
    let notes: Vec<Note> = beats.iter().map(|&start| Note { pitch: 60, start, length: 0.05, velocity: 127 }).collect();
    project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    track
}

/// The frames at which anything sounds.
fn ticks(samples: &[f32]) -> Vec<usize> {
    samples.iter().enumerate().filter(|(_, sample)| **sample != 0.0).map(|(frame, _)| frame).collect()
}

fn options() -> RenderOptions {
    RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.5, ..RenderOptions::default() }
}

/// Plays `project` from `from` for `frames` frames in buffers of `block`,
/// looping if it loops, as the audio thread would.
fn played(project: &Project, factory: &dyn DeviceFactory, from: f64, frames: usize, block: usize) -> Vec<f32> {
    let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
    let (graph, problems) = Compiler {
        factory,
        bank: &mut bank,
        meters: &mut meters,
        audio: &audio,
        stand_ins: None,
        sample_rate: SR,
        purpose: Purpose::Playing,
    }
    .compile(project, true);
    assert!(problems.is_empty(), "{problems:?}");
    let mut processor = Processor::new(SR);
    drop(processor.swap_graph(graph));
    processor.seek(from);
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
        processor.process(l, r);
    }
    left
}

/// A song of 120 that slows to 90 at beat 4, goes to 140 at beat 8, glides
/// from beat 12 down to 70 at beat 16 and takes off at 200 from there.
fn changing() -> Project {
    let mut project = Project::new("t");
    project.set_tempo_at(4.0, 90.0, false).unwrap();
    project.set_tempo_at(8.0, 140.0, false).unwrap();
    project.set_tempo_at(12.0, 140.0, false).unwrap();
    project.set_tempo_at(16.0, 70.0, true).unwrap();
    project.set_tempo_at(16.0, 200.0, false).unwrap();
    project
}

#[test]
fn every_note_sounds_at_the_frame_its_beat_falls_on_whatever_the_buffer_size() {
    let mut project = changing();
    // On the beats and between them, on the changes themselves, a hair
    // before one, and all through the glide.
    let mut beats: Vec<f64> = (0..80).map(|quarter| quarter as f64 * 0.25).collect();
    beats.extend([3.999, 7.9995, 13.37, 15.999, 16.001]);
    beats.sort_by(f64::total_cmp);
    ticking(&mut project, &beats);
    let map = project.tempo_map();
    let (factory, _) = factory();

    let out = render(&project, &factory, &options()).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    let found = ticks(&out.left);
    assert_eq!(found.len(), beats.len(), "one tick for every note: {found:?}");
    let mut worst = 0.0f64;
    for (beat, frame) in beats.iter().zip(&found) {
        let wanted = map.frames_at(*beat, SR);
        let off = *frame as f64 - wanted;
        worst = worst.max(off.abs());
        assert!(off.abs() <= 1.0, "the note at beat {beat} sounds at frame {frame}, {off:+.3} from {wanted:.3}");
    }
    println!("{} notes through five tempos and a glide: the worst is {worst:.3} frames from its beat", beats.len());
    // The song is as long as its tempo makes it: twenty-one beats, of
    // which the last five at 200.
    let seconds = map.seconds_at(21.0);
    assert!((seconds - (2.0 + 4.0 * 60.0 / 90.0 + 4.0 * 60.0 / 140.0 + 240.0 / 70.0 * 2.0f64.ln() + 1.5)).abs() < 1e-9);

    // Played in buffers of any size it is the same samples, and they are
    // the render's.
    let frames = (seconds * SR) as usize;
    let reference = played(&project, &factory, 0.0, frames, 512);
    assert!(reference[..] == out.left[..frames], "what is played is what is rendered");
    for block in [480, 441, 37, 1024, 4096] {
        assert!(played(&project, &factory, 0.0, frames, block) == reference, "buffers of {block} frames differ");
    }
    // Started in the middle, from a beat inside the glide, every note from
    // there on sounds where its beat falls, counted from there.
    let from = map.frames_at(13.0, SR);
    let rest = played(&project, &factory, 13.0, frames - from.ceil() as usize, 512);
    let later: Vec<f64> =
        beats.iter().filter(|beat| **beat >= 13.0).map(|beat| map.frames_at(*beat, SR) - from).collect();
    let found = ticks(&rest);
    assert_eq!(found.len(), later.len(), "{found:?}");
    for (frame, wanted) in found.iter().zip(&later) {
        assert!((*frame as f64 - wanted).abs() <= 1.0, "started at beat 13, a note sounds at {frame} for {wanted:.3}");
    }
}

#[test]
fn a_device_is_told_the_tempo_and_the_time_signature_as_they_pass() {
    let mut project = changing();
    // Two bars of 4/4, 7/8 from beat 8, 3/4 from beat 15.
    project.set_time_signature_at(8.0, 7, 8).unwrap();
    project.set_time_signature_at(15.0, 3, 4).unwrap();
    ticking(&mut project, &[0.0]);
    let (map, bars) = (project.tempo_map(), project.meter_map());
    let (factory, told) = factory();
    let frames = (map.seconds_at(20.0) * SR) as usize;
    for block in [512, 333] {
        told.lock().unwrap().clear();
        played(&project, &factory, 0.0, frames, block);
        let told = told.lock().unwrap().clone();
        assert!(told.len() > frames / block);
        for (index, block) in told.iter().enumerate() {
            // Where the song is said to be is where its tempo has it: in
            // a glide, which is taken in short even steps, to within a
            // tenth of a frame.
            let wanted = map.beat_after(0.0, block.frame as f64, SR);
            let near = if map.is_ramping_at(wanted) { 5e-6 } else { 1e-9 };
            assert!((block.beat - wanted).abs() < near, "frame {}: beat {} for {wanted}", block.frame, block.beat);
            // The time signature is the one the block starts in, and a
            // block does not reach into the next one by more than a frame.
            assert_eq!(block.signature, bars.signature_at(block.beat), "at beat {}", block.beat);
            let next = told.get(index + 1).map_or(f64::INFINITY, |next| next.beat);
            if let Some(change) = bars.next_change(block.beat).filter(|change| *change < next) {
                let past = map.frames_between(change, next, SR);
                assert!(past < 1.0, "a block runs {past} frames past the change of meter at {change}");
            }
            // The tempo is the song's there. In a glide it is the tempo
            // of the step the block lies in, and steps are short.
            let tempo = map.tempo_at(block.beat);
            match map.is_ramping_at(block.beat) {
                false => {
                    let exact = block.tempo == tempo;
                    // The one frame a jump falls inside is told at the
                    // tempo before it.
                    let straddles =
                        map.next_change(block.beat).is_some_and(|change| change < next) || block.frames <= 1;
                    assert!(
                        exact || straddles,
                        "at beat {} the device is told {} BPM for {tempo}",
                        block.beat,
                        block.tempo
                    );
                }
                true => {
                    assert!((block.tempo - tempo).abs() < 0.2, "gliding at {}: {} for {tempo}", block.beat, block.tempo)
                }
            }
            if let Some(change) = map.next_change(block.beat).filter(|change| *change < next) {
                let past = map.frames_between(change, next, SR);
                assert!(past < 1.0 + 1e-6, "a block runs {past} frames past the change of tempo at {change}");
            }
        }
        // Every tempo of the song was told, each from the frame it began.
        for (beat, tempo) in [(0.0, 120.0), (4.0, 90.0), (8.0, 140.0), (16.0, 200.0)] {
            let first = told.iter().find(|block| block.tempo == tempo).unwrap();
            let wanted = map.frames_at(beat, SR).ceil();
            assert!((first.frame as f64 - wanted).abs() <= 1.0, "{tempo} BPM from frame {}, for {wanted}", first.frame);
        }
    }
}

#[test]
fn a_loop_across_a_change_of_tempo_comes_round_on_the_frame() {
    // 120, and 60 from beat 4. The loop is beats 2 to 6: one second of the
    // first and two of the second.
    let mut project = Project::new("t");
    project.set_tempo_at(4.0, 60.0, false).unwrap();
    project.set_loop(true, 2.0, 6.0).unwrap();
    ticking(&mut project, &[0.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    let (factory, _) = factory();
    let round = 3 * SR as usize;
    let out = played(&project, &factory, 2.0, 4 * round, 512);
    let wanted: Vec<usize> =
        (0..4).flat_map(|pass| [0, 24_000, 48_000, 96_000].map(|frame| pass * round + frame)).collect();
    assert_eq!(ticks(&out), wanted, "beats 2 to 5, four times over, and nothing of beats 0, 6 and 7");
    for block in [37, 1024] {
        assert!(played(&project, &factory, 2.0, 4 * round, block) == out, "buffers of {block} frames differ");
    }
}

#[test]
fn audio_at_its_own_speed_ignores_the_tempo_and_audio_that_follows_it_lands_on_the_beats() {
    let folder = folder();
    let path = write_wav(&file(&folder, "clicks 100bpm.wav"), SR, &click_track(100.0, 8, SR));
    let rendered = |project: &Project| {
        let out = render(project, &BuiltinFactory, &RenderOptions { max_tail_seconds: 1.0, ..options() }).unwrap();
        assert!(out.problems.is_empty(), "{:?}", out.problems);
        out.left
    };
    let off_by = |samples: &[f32], wanted: &[f64]| {
        let found = click_times(samples, SR, 0.2);
        assert_eq!(found.len(), wanted.len(), "clicks at {found:?}, wanted them at {wanted:?}");
        found.iter().zip(wanted).map(|(found, wanted)| (found - wanted).abs() * 1000.0).fold(0.0, f64::max)
    };

    // 120, 80 from beat 4, and from beat 6 a glide to 160 at beat 10.
    let mut song = Project::new("t");
    song.set_tempo_at(4.0, 80.0, false).unwrap();
    song.set_tempo_at(6.0, 80.0, false).unwrap();
    song.set_tempo_at(10.0, 160.0, true).unwrap();
    let map = song.tempo_map();
    let track = song.add_track("Audio");

    // At its own speed: the clicks stay 0.6 s apart whatever the song's
    // tempo does under them. Cut in two anywhere, it is the same sound.
    let mut plain = song.clone();
    let clip = add_clip(&mut plain, track, &path, 0.0, 12.0);
    let own: Vec<f64> = (0..8).map(|index| (index as f64 + 0.5) * 0.6).collect();
    let whole = rendered(&plain);
    let worst = off_by(&whole, &own);
    assert!(worst < 0.05, "a click of the recording is {worst} ms from where the recording has it");
    let second = plain.split_clip(clip, 5.0).unwrap();
    plain.split_clip(second, 7.5).unwrap();
    let cut = rendered(&plain);
    let apart = whole.iter().zip(&cut).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(apart < 1e-6, "cut at beats 5 and 7.5, the recording differs by {apart}");

    // Following the tempo: recorded at 100, its eight beats are the
    // song's first eight, through the jump and into the glide.
    println!("mode      worst click error (ms)");
    for (mode, tolerance) in [(WarpMode::Beats, 3.0), (WarpMode::Repitch, 3.0)] {
        let mut warped = song.clone();
        let clip = add_clip(&mut warped, track, &path, 0.0, 8.0);
        warped.set_clip_warp(clip, mode, Some(100.0), 0.0).unwrap();
        let beats: Vec<f64> = (0..8).map(|index| map.seconds_at(index as f64 + 0.5)).collect();
        let out = rendered(&warped);
        let worst = off_by(&out, &beats);
        println!("{mode:<8?}  {worst:>8.3}");
        assert!(worst < tolerance, "{mode:?}: a click is {worst} ms off its beat");
        // Cut where the tempo is steady and where it glides, it lands the
        // same.
        let second = warped.split_clip(clip, 2.0).unwrap();
        warped.split_clip(second, 7.0).unwrap();
        let worst = off_by(&rendered(&warped), &beats);
        assert!(worst < tolerance, "{mode:?}, cut: a click is {worst} ms off its beat");
    }

    // A tone that follows the tempo goes on through the changes: where
    // two tempos meet there is no hole in it.
    let tone = write_wav(&file(&folder, "tone 100bpm.wav"), SR, &common::sine(440.0, SR, 4.8, 0.5));
    let mut held = song.clone();
    let clip = add_clip(&mut held, track, &tone, 0.0, 8.0);
    held.set_clip_warp(clip, WarpMode::Repitch, Some(100.0), 0.0).unwrap();
    let out = rendered(&held);
    let seam = (map.seconds_at(4.0) * SR) as usize;
    let around = common::rms(&out[seam - 480..seam + 480]);
    assert!(around > 0.2, "around the change at beat 4 the tone is at {around}, of 0.35");
    // Sped up or slowed down, it is at the pitch the tempo makes of it.
    let pitch = |from: f64, to: f64| {
        let (from, to) = ((map.seconds_at(from) * SR) as usize, (map.seconds_at(to) * SR) as usize);
        zero_crossing_hz(&out[from..to], SR)
    };
    assert!((pitch(1.0, 3.0) - 440.0 * 1.2).abs() < 3.0, "at 120: {} Hz", pitch(1.0, 3.0));
    assert!((pitch(4.5, 5.5) - 440.0 * 0.8).abs() < 3.0, "at 80: {} Hz", pitch(4.5, 5.5));
}

#[test]
fn an_envelope_and_a_loop_of_notes_follow_the_beats_through_a_change() {
    // A tone under an envelope that opens at beat 6, in a song that goes
    // from 120 to 90 at beat 4: it opens where beat 6 is.
    let mut project = Project::new("t");
    project.set_tempo_at(4.0, 90.0, false).unwrap();
    let track = project.add_track("Lead");
    let synth = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    let params = &mut project.device_mut(synth).unwrap().params;
    for (key, value) in [("osc1_wave", 0.0), ("attack", 0.001), ("release", 0.01), ("sustain", 1.0)] {
        params.insert(key.to_string(), value);
    }
    let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[Note { pitch: 69, start: 0.0, length: 8.0, velocity: 100 }]).unwrap();
    let held = |beat: f64, value: f64| AutomationPoint { beat, value, shape: Segment::Hold };
    let volume = AutomationTarget::TrackVolume { track };
    project.set_automation(volume, vec![held(0.0, -70.0), held(6.0, 0.0)]).unwrap();
    let out = render(&project, &BuiltinFactory, &options()).unwrap();
    let opens = project.tempo_map().frames_at(6.0, SR) as usize;
    assert_eq!(opens, 4 * 24_000 + 2 * 32_000);
    assert_eq!(peak(&out.left[..opens - 64]), 0.0, "nothing before beat 6");
    assert!(peak(&out.left[opens + 64..opens + 480]) > 0.1, "and the tone from there");
}

#[test]
fn the_metronome_and_the_count_in_click_the_meter_they_are_in() {
    // One bar of 4/4 at 120, then 7/8 at 150: an eighth is a fifth of a
    // second there.
    let mut rig = Rig::new();
    let track = rig.probe_track("Keys");
    rig.session
        .edit(|project| {
            project.set_time_signature_at(4.0, 7, 8)?;
            project.set_tempo_at(4.0, 150.0, false)
        })
        .unwrap();
    rig.run(2);
    let accented = |samples: &[f32], second: f64| {
        let at = (second * SR) as usize;
        zero_crossing_hz(&samples[at + 24..at + 480], SR) > 1300.0
    };

    // Played from the top with the metronome on: four quarters, the first
    // accented, then eighths, with the first of every seven accented.
    rig.session.set_metronome(true);
    rig.host.keep_output();
    let started = rig.now();
    rig.session.play();
    rig.run_until(started + 4.0 * FRAMES_PER_BEAT + 3.6 * SR);
    rig.session.stop();
    rig.run(4);
    let (left, _) = rig.host.take_output();
    let clicks = click_times(&left, SR, 0.1);
    let wanted: Vec<f64> =
        (0..4).map(|beat| beat as f64 * 0.5).chain((0..18).map(|eighth| 2.0 + eighth as f64 * 0.2)).collect();
    let lag = clicks[0];
    assert!(lag < 0.05, "the first click {lag} s after play was asked for");
    assert_eq!(clicks.len(), wanted.len(), "{clicks:?}");
    for (click, wanted) in clicks.iter().zip(&wanted) {
        assert!((click - lag - wanted).abs() < 0.002, "a click at {} s for {wanted}: {clicks:?}", click - lag);
    }
    let accents: Vec<f64> = wanted.iter().copied().filter(|second| accented(&left, lag + second)).collect();
    assert_eq!(accents.len(), 4, "{accents:?}");
    for (accent, wanted) in accents.iter().zip([0.0, 2.0, 3.4, 4.8]) {
        assert!((accent - wanted).abs() < 1e-9, "bars begin at {accents:?}");
    }

    // A take from the second bar of 7/8 is counted in with a bar of it:
    // seven eighths at 150, the first accented, and the song starts with
    // the next.
    rig.session.set_metronome(false);
    rig.session.set_armed(track, true).unwrap();
    rig.session.seek(7.5);
    rig.run(4);
    rig.host.take_output();
    rig.host.keep_output();
    let asked = rig.now();
    rig.session.record().unwrap();
    rig.run_until(asked + 1.0 * SR);
    assert_eq!(rig.session.record_status().state, RecordState::CountIn);
    rig.run_until(asked + 1.4 * SR + 2_400.0);
    assert_eq!(rig.session.record_status().state, RecordState::Recording);
    let rolling = rig.session.transport().position_beats;
    assert!(rolling > 7.5 && rolling < 7.5 + 0.2, "the song set off at beat 7.5, and is at {rolling}");
    rig.run_until(asked + 2.0 * SR);
    rig.session.stop();
    rig.run(4);
    let (left, _) = rig.host.take_output();
    let clicks = click_times(&left, SR, 0.1);
    assert_eq!(clicks.len(), 7, "seven eighths, and none once the song is rolling: {clicks:?}");
    for (index, click) in clicks.iter().enumerate() {
        assert!((click - index as f64 * 0.2).abs() < 0.002, "click {index} at {click} s");
        assert_eq!(accented(&left, *click), index == 0, "click {index}");
    }
}
