//! Envelopes, heard: a send that is opened for a note, how a line gets
//! from one point to the next for each kind of control, a song saved
//! before lines followed their controls, and that all of it is the same
//! samples whatever the buffer size and in a render.

mod common;

use common::{SR, largest_step, peak, rms};
use std::sync::{Arc, Mutex};
use trill_core::device::{
    DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor, Event, ParamInfo,
    ParamKind, ProcessInfo, Purpose,
};
use trill_core::project::{AutomationPoint, AutomationTarget, Segment};
use trill_core::{ChainRef, Device, DeviceId, Note, Project, TrackId, TrackKind, Travel};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::render::{RenderOptions, render};
use trill_engine::{AudioSources, Processor};

/// Frames in a beat at 120 BPM.
const BEAT: usize = 24_000;

fn point(beat: f64, value: f64) -> AutomationPoint {
    AutomationPoint::new(beat, value)
}

fn held(beat: f64, value: f64) -> AutomationPoint {
    AutomationPoint { beat, value, shape: Segment::Hold }
}

/// A track that plays one steady sine for `beats` beats.
fn sine_track(project: &mut Project, name: &str, beats: f64) -> TrackId {
    let track = project.add_track(name);
    let device = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    let params = &mut project.device_mut(device).unwrap().params;
    for (key, value) in [("osc1_wave", 0.0), ("attack", 0.001), ("release", 0.01), ("sustain", 1.0)] {
        params.insert(key.to_string(), value);
    }
    let clip = project.add_midi_clip(track, "clip", 0.0, beats).unwrap();
    project.clip_mut(clip).unwrap().add_notes(&[Note { pitch: 69, start: 0.0, length: beats, velocity: 100 }]).unwrap();
    track
}

fn rendered(project: &Project) -> Vec<f32> {
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let out = render(project, &BuiltinFactory, &options).unwrap();
    assert!(out.problems.is_empty(), "{:?}", out.problems);
    out.left
}

/// What was made of `plain` to get `shaped`, in dB, around `beat`: read
/// where the wave is far enough from zero to say.
fn gain_db(shaped: &[f32], plain: &[f32], beat: f64) -> f64 {
    let at = (beat * BEAT as f64) as usize;
    let loud = 0.5 * peak(plain);
    let ratios: Vec<f64> = (at.saturating_sub(120)..at + 120)
        .filter(|&frame| plain[frame].abs() > loud)
        .map(|frame| (shaped[frame] / plain[frame]) as f64)
        .collect();
    assert!(ratios.len() > 40, "nothing to read the gain from at beat {beat}");
    20.0 * (ratios.iter().sum::<f64>() / ratios.len() as f64).abs().log10()
}

/// One sine, heard only through what it sends to a return: the track
/// plays into a group that is muted, so the mix is the return alone.
fn through_a_send(beats: f64) -> (Project, TrackId, TrackId) {
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "Lead", beats);
    let dry = project.add_track_of_kind("Dry", TrackKind::Group);
    project.set_track_group(track, Some(dry)).unwrap();
    project.track_mut(dry).unwrap().mute = true;
    let echo = project.add_track_of_kind("Echo", TrackKind::Return);
    (project, track, echo)
}

/// The sine of [`through_a_send`] as it is with nothing done to it.
fn plain_sine(beats: f64) -> Vec<f32> {
    let mut project = Project::new("t");
    sine_track(&mut project, "Lead", beats);
    rendered(&project)
}

#[test]
fn a_send_thrown_open_for_a_note_feeds_the_return_then_and_only_then() {
    let plain = plain_sine(12.0);
    let (mut project, track, echo) = through_a_send(12.0);
    assert_eq!(peak(&rendered(&project)), 0.0, "with no send nothing reaches the return");

    // Shut, opened over two beats, held open for two, and shut again. The
    // track has no send of its own: the envelope is the send.
    let throw = AutomationTarget::Send { track, to: echo };
    let points = vec![point(0.0, -70.0), point(2.0, -70.0), point(4.0, -6.0), point(6.0, -6.0), point(8.0, -70.0)];
    project.set_automation(throw.clone(), points).unwrap();
    assert!(project.track(track).unwrap().sends.is_empty());
    let thrown = rendered(&project);

    assert_eq!(peak(&thrown[..2 * BEAT]), 0.0, "shut means nothing at all is sent");
    assert_eq!(peak(&thrown[8 * BEAT + 64..]), 0.0, "and shut again it is silent again");
    assert!((gain_db(&thrown, &plain, 5.0) + 6.0).abs() < 0.01, "open: {}", gain_db(&thrown, &plain, 5.0));
    // On the way it is where a send's fader would be: half way between
    // -70 and -6 dB along its travel is -22.7 dB, not the -38 dB half way
    // in dB.
    let half_way = Travel::SEND.between(-70.0, -6.0, 0.5);
    assert!((half_way + 22.72).abs() < 0.01, "{half_way}");
    assert!((gain_db(&thrown, &plain, 3.0) - half_way).abs() < 0.05, "{}", gain_db(&thrown, &plain, 3.0));
    assert!((gain_db(&thrown, &plain, 7.0) - half_way).abs() < 0.05, "{}", gain_db(&thrown, &plain, 7.0));
    // Smooth: it adds no step of its own to the wave.
    assert!(largest_step(&thrown) <= largest_step(&plain), "{} > {}", largest_step(&thrown), largest_step(&plain));

    // A send set by hand is moved by the envelope too, and is itself again
    // when the envelope is switched off.
    project.set_send(track, echo, Some(-12.0)).unwrap();
    assert!(rendered(&project) == thrown, "the envelope is what counts while it is on");
    project.set_automation_enabled(&throw, false).unwrap();
    let by_hand = rendered(&project);
    assert!((gain_db(&by_hand, &plain, 1.0) + 12.0).abs() < 0.01, "{}", gain_db(&by_hand, &plain, 1.0));
    assert!((gain_db(&by_hand, &plain, 10.0) + 12.0).abs() < 0.01);
    // Off, and with no send set by hand, there is no send.
    project.set_send(track, echo, None).unwrap();
    assert_eq!(peak(&rendered(&project)), 0.0);
}

#[test]
fn a_send_that_jumps_open_is_a_step_and_does_not_click_more_than_one() {
    let plain = plain_sine(8.0);
    let (mut project, track, echo) = through_a_send(8.0);
    // Held shut, then open at once on beat 2, and shut at once on beat 4.
    let gate = vec![held(0.0, -70.0), held(2.0, 0.0), held(4.0, -70.0)];
    project.set_automation(AutomationTarget::Send { track, to: echo }, gate).unwrap();
    let gated = rendered(&project);
    assert_eq!(peak(&gated[..2 * BEAT - 32]), 0.0, "nothing before the step");
    for beat in [2.01, 3.0, 3.99] {
        assert!(gain_db(&gated, &plain, beat).abs() < 0.001, "open at 0 dB it is the track, at beat {beat}");
    }
    assert_eq!(peak(&gated[4 * BEAT + 64..]), 0.0, "nothing after it");
    // The step takes two thirds of a millisecond: no sample jumps by more
    // than a fraction of the wave's height.
    assert!(largest_step(&gated) < 0.08 * peak(&plain), "{} of {}", largest_step(&gated), peak(&plain));
}

#[test]
fn a_line_between_two_points_is_straight_along_the_travel_of_what_it_moves() {
    let plain = plain_sine(16.0);
    let fade = vec![point(0.0, 0.0), point(16.0, -60.0)];

    // A track's fader: a stop from 0 dB to -60 dB is at -16.3 dB half way.
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "Lead", 16.0);
    let volume = AutomationTarget::TrackVolume { track };
    project.set_automation(volume.clone(), fade.clone()).unwrap();
    let faded = rendered(&project);
    for (beat, wanted) in [(4.0, -8.15), (8.0, -16.30), (12.0, -29.99)] {
        let got = gain_db(&faded, &plain, beat);
        assert!((got - wanted).abs() < 0.06, "the fader at beat {beat}: {got} dB");
    }
    // The master's fader goes the same way.
    project.set_automation(volume.clone(), Vec::new()).unwrap();
    project.set_automation(AutomationTarget::MasterVolume, fade.clone()).unwrap();
    assert!((gain_db(&rendered(&project), &plain, 8.0) + 16.30).abs() < 0.06);
    project.set_automation(AutomationTarget::MasterVolume, Vec::new()).unwrap();

    // A song saved before lines followed their controls says nothing of
    // how they travel, and plays them straight in decibels as it did.
    project.write_automation(volume.clone(), fade.clone(), Travel::Linear).unwrap();
    let this_format = format!("\"format_version\": {}", trill_core::project::FORMAT_VERSION);
    assert!(project.to_json().contains(&this_format));
    let old = Project::from_json(&project.to_json().replace(&this_format, "\"format_version\": 1")).unwrap();
    assert!(!project.to_json().contains("travel"));
    let straight = rendered(&old);
    for (beat, wanted) in [(4.0, -15.0), (8.0, -30.0), (12.0, -45.0)] {
        let got = gain_db(&straight, &plain, beat);
        assert!((got - wanted).abs() < 0.06, "straight in decibels at beat {beat}: {got} dB");
    }
    assert!(straight == rendered(&project));

    // A bend still bends, along the fader: more of the fall comes late.
    let bent = vec![AutomationPoint { beat: 0.0, value: 0.0, shape: Segment::Curve(0.5) }, point(16.0, -60.0)];
    project.write_automation(volume.clone(), bent, Travel::FADER).unwrap();
    let late = gain_db(&rendered(&project), &plain, 8.0);
    assert!(late > -12.0 && late < -4.0, "{late}");

    // A gain that reaches silence has a travel of its own: a little
    // further down half way than a fader, and less far down later on.
    project.set_automation(volume.clone(), Vec::new()).unwrap();
    let utility = project.add_device(ChainRef::Track(track), "builtin:utility", "Utility", None).unwrap();
    let gain = AutomationTarget::DeviceParam { device: utility, param: "gain".into() };
    project.write_automation(gain.clone(), fade.clone(), Travel::GAIN).unwrap();
    let on_a_gain = rendered(&project);
    for (beat, wanted) in [(4.0, -8.37), (8.0, -16.74), (12.0, -27.42)] {
        let got = gain_db(&on_a_gain, &plain, beat);
        assert!((got - wanted).abs() < 0.1, "the gain at beat {beat}: {got} dB");
    }

    // An envelope that a song of formats 3 to 5 saved along the fader of
    // its day plays along it still: -20 dB half way, on the mixer's fader
    // and on a gain alike. Such a file calls that law a fader, with the
    // level it was measured from.
    let of_its_day = Travel::Square { floor: -70.0 };
    project.write_automation(volume.clone(), fade.clone(), of_its_day).unwrap();
    project.set_automation(gain.clone(), Vec::new()).unwrap();
    let this_format = format!("\"format_version\": {}", trill_core::project::FORMAT_VERSION);
    let saved = project.to_json();
    assert!(saved.contains("\"square\": {") && saved.contains(&this_format), "{saved}");
    let earlier = saved.replace("\"square\": {", "\"fader\": {").replace(&this_format, "\"format_version\": 5");
    let old = Project::from_json(&earlier).unwrap();
    assert_eq!(old.automation[0].travel, of_its_day, "what a file of format 5 calls a fader");
    let as_it_was = rendered(&old);
    for (beat, wanted) in [(4.0, -9.17), (8.0, -20.0), (12.0, -33.94)] {
        let got = gain_db(&as_it_was, &plain, beat);
        assert!((got - wanted).abs() < 0.06, "the fader of formats 3 to 5 at beat {beat}: {got} dB");
    }
    assert!(as_it_was == rendered(&project), "and written again by this build it is the same song");
}

/// How a control of a built-in device travels, as its device says it.
fn travel_of(uri: &str, param: &str) -> Travel {
    let descriptors = BuiltinFactory.descriptors();
    let device = descriptors.iter().find(|descriptor| descriptor.uri == uri).unwrap();
    Travel::of_param(device.params.iter().find(|info| info.id == param).unwrap())
}

#[test]
fn a_fade_of_two_points_falls_as_ableton_live_s_does() {
    // The three fades of the bench's song: a Utility's gain from 0 dB to
    // -60 dB, two points each, over 11, 5 and 6.5 beats. What Live shows
    // along each such line, in the middle of every beat of it, is read
    // from what it shows at 257 places along a Utility's gain.
    let fades: [(&str, f64, &[f64]); 3] = [
        ("the pad, out", 11.0, &[-1.52, -4.56, -7.61, -10.68, -13.71, -16.75, -19.92, -23.87, -28.87, -35.93, -47.97]),
        ("the lead, out", 5.0, &[-3.34, -10.02, -16.75, -24.76, -39.81]),
        ("the lead, drifting", 6.5, &[-2.57, -7.72, -12.91, -18.06, -24.38, -34.37]),
    ];
    // What the fader of formats 3 to 5 made of the first: Live's for a
    // third of the way, and 4 to 7 dB under it through the second half.
    let before = [-1.58, -4.84, -8.28, -11.92, -15.81, -20.0, -24.57, -29.66, -35.49, -42.53, -52.16];
    assert_eq!(travel_of("builtin:utility", "gain"), Travel::GAIN, "a Utility's gain says it is one");
    let plain = plain_sine(16.0);
    let mut worst = 0.0f64;
    for (name, beats, live) in fades {
        let mut project = Project::new("t");
        let track = sine_track(&mut project, "Lead", 16.0);
        let utility = project.add_device(ChainRef::Track(track), "builtin:utility", "Fade", None).unwrap();
        let gain = AutomationTarget::DeviceParam { device: utility, param: "gain".into() };
        let line = vec![point(0.0, 0.0), point(2.0, 0.0), point(2.0 + beats, -60.0)];
        project.write_automation(gain.clone(), line.clone(), travel_of("builtin:utility", "gain")).unwrap();
        let faded = rendered(&project);
        for (index, shown) in live.iter().enumerate() {
            let got = gain_db(&faded, &plain, 2.5 + index as f64);
            worst = worst.max((got - shown).abs());
            assert!((got - shown).abs() < 0.35, "{name}, beat {}: {got:.2} dB where Live shows {shown}", index + 1);
        }
        if beats == 11.0 {
            // The same two points along the fader of before, for the
            // difference the bench heard.
            project.write_automation(gain, line, Travel::Square { floor: -70.0 }).unwrap();
            let was = rendered(&project);
            for (index, then) in before.iter().enumerate() {
                let got = gain_db(&was, &plain, 2.5 + index as f64);
                assert!((got - then).abs() < 0.35, "{name} as it was, beat {}: {got:.2} dB for {then}", index + 1);
            }
            assert!(before[7] - live[7] < -5.5, "5.8 dB under Live's three quarters of the way");
        }
    }
    println!("three fades of two points, 22 beats in all: the worst is {worst:.2} dB from what Live shows");

    // A send and an instrument's volume are faders, and travel as Live's
    // do: from 0 dB to -60 dB over eight beats, what Live shows at each
    // beat along its mixer's send, and along Operator's volume (which it
    // shows to the decibel).
    let sent = [-4.66, -9.32, -13.98, -18.64, -23.3, -30.13, -42.35];
    let (mut project, track, echo) = through_a_send(16.0);
    let send = AutomationTarget::Send { track, to: echo };
    project.set_automation(send.clone(), vec![point(0.0, 0.0), point(2.0, 0.0), point(10.0, -60.0)]).unwrap();
    assert_eq!(project.automation[0].travel, Travel::SEND);
    let thrown = rendered(&project);
    for (index, shown) in sent.iter().enumerate() {
        let got = gain_db(&thrown, &plain, 3.0 + index as f64);
        assert!((got - shown).abs() < 0.1, "the send, beat {}: {got:.2} dB where Live shows {shown}", index + 1);
    }
    let volume = [-4.11, -8.16, -12.0, -16.0, -21.0, -30.0, -42.94];
    assert_eq!(travel_of("builtin:polysynth", "volume"), Travel::FADER, "its top is 6 dB over unity, as a track's is");
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "Lead", 16.0);
    let fader = AutomationTarget::TrackVolume { track };
    project.set_automation(fader, vec![point(0.0, 0.0), point(2.0, 0.0), point(10.0, -60.0)]).unwrap();
    let faded = rendered(&project);
    for (index, shown) in volume.iter().enumerate() {
        let got = gain_db(&faded, &plain, 3.0 + index as f64);
        assert!((got - shown).abs() < 0.6, "a volume, beat {}: {got:.2} dB where Live shows {shown}", index + 1);
    }
}

/// A device with one parameter, a frequency, that writes down what it is
/// set to at every frame it is given.
struct Watched {
    value: f64,
    seen: Arc<Mutex<Vec<f64>>>,
}

impl DeviceProcessor for Watched {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

    fn set_param(&mut self, _index: usize, value: f64) {
        self.value = value;
    }

    fn process(&mut self, _: &ProcessInfo, _: &[Event], left: &mut [f32], _: &mut [f32]) {
        let mut seen = self.seen.lock().unwrap();
        seen.extend(std::iter::repeat_n(self.value, left.len()));
    }

    fn reset(&mut self) {}
}

struct WatchedFactory(Arc<Mutex<Vec<f64>>>);

impl DeviceFactory for WatchedFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        vec![DeviceDescriptor {
            uri: "test:watched".into(),
            name: "Watched".into(),
            vendor: "Test".into(),
            category: DeviceCategory::Effect,
            description: String::new(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != "test:watched" {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        let cutoff = ParamInfo {
            id: "cutoff".into(),
            name: "Cutoff".into(),
            min: 20.0,
            max: 20_000.0,
            default: 1000.0,
            unit: "Hz".into(),
            kind: ParamKind::Float,
        };
        Ok(DeviceInstance {
            processor: Box::new(Watched { value: 1000.0, seen: self.0.clone() }),
            params: vec![cutoff],
            controller: None,
        })
    }
}

/// Plays `project` from the top in buffers of `block` frames and returns
/// what the watched device was set to at every frame.
fn watched(project: &Project, block: usize, frames: usize) -> Vec<f64> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let factory = WatchedFactory(seen.clone());
    let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
    let (graph, problems) = Compiler {
        factory: &factory,
        bank: &mut bank,
        meters: &mut meters,
        audio: &audio,
        stand_ins: None,
        sample_rate: SR,
        purpose: Purpose::Playing,
    }
    .compile(project, false);
    assert!(problems.is_empty(), "{problems:?}");
    let mut processor = Processor::new(SR);
    drop(processor.swap_graph(graph));
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
        processor.process(l, r);
    }
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), frames);
    seen
}

#[test]
fn a_parameter_is_moved_along_its_travel_at_the_same_frames_whatever_the_buffer() {
    let mut project = Project::new("t");
    let track = project.add_track("A");
    let device: DeviceId = project.add_device(ChainRef::Track(track), "test:watched", "Watched", None).unwrap();
    let cutoff = AutomationTarget::DeviceParam { device, param: "cutoff".into() };
    let sweep = vec![point(0.0, 300.0), point(4.0, 9000.0)];
    // By ratio, as a frequency's knob travels: the middle pitch half way.
    project.write_automation(cutoff.clone(), sweep.clone(), Travel::Log).unwrap();
    let frames = 4 * BEAT + 2_000;
    let reference = watched(&project, 512, frames);
    let at = |seen: &[f64], beat: f64| seen[(beat * BEAT as f64) as usize];
    let middle = (300.0f64 * 9000.0).sqrt();
    assert!((at(&reference, 2.0) / middle - 1.0).abs() < 0.01, "{} Hz half way", at(&reference, 2.0));
    assert!((at(&reference, 1.0) / (300.0 * 30.0f64.powf(0.25)) - 1.0).abs() < 0.01);
    assert_eq!(at(&reference, 4.05), 9000.0);
    // It moves every 256 frames of the song, and holds between.
    for (frame, pair) in reference.windows(2).enumerate().take(4 * BEAT - 1) {
        let moved = pair[1] != pair[0];
        assert_eq!(moved, (frame + 1) % 256 == 0, "at frame {}: {:?}", frame + 1, pair);
    }
    // Whatever the buffer size: the same value at the same frame.
    for block in [480, 441, 37, 1, 4096] {
        assert!(watched(&project, block, frames) == reference, "buffers of {block} frames moved it elsewhere");
    }

    // Straight in hertz, as a lane written before travels: 4,650 Hz half way.
    project.write_automation(cutoff, sweep, Travel::Linear).unwrap();
    let straight = watched(&project, 512, frames);
    assert!((at(&straight, 2.0) - 4650.0).abs() < 15.0, "{}", at(&straight, 2.0));
}

/// A song in which an envelope moves everything an envelope can: a
/// track's fader and pan, a send to a return with an echo on it, a gain,
/// the master's fader and, with `sweep`, a filter's cutoff, with steps and
/// bends among the lines.
fn moving_everything(sweep: bool) -> Project {
    let mut project = Project::new("t");
    let track = project.add_track("Lead");
    let synth = project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    project.device_mut(synth).unwrap().params.insert("osc1_wave".into(), 1.0);
    let filter = project.add_device(ChainRef::Track(track), "builtin:filter", "Filter", None).unwrap();
    let utility = project.add_device(ChainRef::Track(track), "builtin:utility", "Utility", None).unwrap();
    let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
    let notes: Vec<Note> = (0..16)
        .map(|step| Note { pitch: 45 + (step % 7) as u8, start: step as f64 * 0.5, length: 0.45, velocity: 100 })
        .collect();
    project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    let echo = project.add_track_of_kind("Echo", TrackKind::Return);
    project.add_device(ChainRef::Track(echo), "builtin:delay", "Delay", None).unwrap();

    let bent = |beat: f64, value: f64, bend: f32| AutomationPoint { beat, value, shape: Segment::Curve(bend) };
    project
        .set_automation(
            AutomationTarget::TrackVolume { track },
            vec![point(0.0, -24.0), bent(1.5, 0.0, 0.6), held(3.0, -9.0), point(3.25, -30.0), point(7.0, 3.0)],
        )
        .unwrap();
    project
        .set_automation(
            AutomationTarget::TrackPan { track },
            vec![point(0.0, -1.0), point(2.75, 1.0), point(8.0, -0.3)],
        )
        .unwrap();
    project
        .set_automation(
            AutomationTarget::Send { track, to: echo },
            vec![
                point(0.0, -70.0),
                point(1.0, -70.0),
                point(1.25, -3.0),
                held(2.0, -70.0),
                held(4.5, -6.0),
                point(5.5, -70.0),
            ],
        )
        .unwrap();
    project
        .set_automation(AutomationTarget::MasterVolume, vec![point(0.0, 0.0), point(4.0, -6.0), point(8.0, 0.0)])
        .unwrap();
    if sweep {
        project
            .write_automation(
                AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() },
                vec![point(0.0, 300.0), point(3.0, 9000.0), point(8.0, 400.0)],
                Travel::Log,
            )
            .unwrap();
    }
    project
        .write_automation(
            AutomationTarget::DeviceParam { device: utility, param: "gain".into() },
            vec![point(0.0, 0.0), point(6.0, -40.0), point(8.0, 0.0)],
            Travel::GAIN,
        )
        .unwrap();
    project
}

/// Plays `project` from the top for eight beats in buffers of `block`
/// frames.
fn played(project: &Project, block: usize, looping: bool) -> (Vec<f32>, Vec<f32>) {
    let run = |block: usize, looping: bool| {
        let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
        let (graph, _) = Compiler {
            factory: &BuiltinFactory,
            bank: &mut bank,
            meters: &mut meters,
            audio: &audio,
            stand_ins: None,
            sample_rate: SR,
            purpose: Purpose::Playing,
        }
        .compile(project, looping);
        let mut processor = Processor::new(SR);
        drop(processor.swap_graph(graph));
        processor.play();
        let mut left = vec![0.0f32; 8 * BEAT];
        let mut right = left.clone();
        for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
            processor.process(l, r);
        }
        (left, right)
    };
    run(block, looping)
}

#[test]
fn automation_plays_the_same_at_any_buffer_size_and_in_a_render() {
    let mut project = moving_everything(false);
    // Round the transport's loop too, so that every envelope jumps.
    project.set_loop(true, 1.0, 6.5).unwrap();

    let reference = played(&project, 512, true);
    assert!(rms(&reference.0) > 0.01 && rms(&reference.1) > 0.01);
    assert!(reference.0 != reference.1, "the pan moves");
    for block in [480, 441, 37, 1, 4096] {
        assert!(
            played(&project, block, true) == reference,
            "looping, buffers of {block} frames produced different audio"
        );
    }
    // Straight through: a render is what playback is, to the bit.
    let live = played(&project, 441, false);
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let offline = render(&project, &BuiltinFactory, &options).unwrap();
    assert!(live.0[..] == offline.left[..8 * BEAT], "the render differs from playback on the left");
    assert!(live.1[..] == offline.right[..8 * BEAT], "the render differs from playback on the right");
    assert!(played(&project, 1, false).0[..2 * BEAT] == live.0[..2 * BEAT]);
}

#[test]
fn a_filter_sweep_is_rendered_as_it_is_played() {
    // The built-in filter reads its cutoff every sixteen frames, counted
    // from the start of the block it is handed, so it is held to this at
    // the buffer sizes that are a multiple of that; a parameter reaches it
    // at the same frames at any size (see the watched device above).
    let mut project = moving_everything(true);
    project.set_loop(true, 1.0, 6.5).unwrap();
    let reference = played(&project, 512, true);
    for block in [480, 1024, 4096] {
        assert!(
            played(&project, block, true) == reference,
            "looping, buffers of {block} frames produced different audio"
        );
    }
    let live = played(&project, 480, false);
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 0.5, ..RenderOptions::default() };
    let offline = render(&project, &BuiltinFactory, &options).unwrap();
    assert!(live.0[..] == offline.left[..8 * BEAT] && live.1[..] == offline.right[..8 * BEAT]);
    // And the sweep is heard: the same song with the filter standing still
    // is another sound.
    assert!(played(&moving_everything(false), 480, false).0 != live.0);
}

#[test]
fn swapping_graphs_under_moving_envelopes_changes_nothing() {
    let project = moving_everything(true);
    let run = |swap_every_block: bool| {
        let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
        let mut compile = |project: &Project| {
            Compiler {
                factory: &BuiltinFactory,
                bank: &mut bank,
                meters: &mut meters,
                audio: &audio,
                stand_ins: None,
                sample_rate: SR,
                purpose: Purpose::Playing,
            }
            .compile(project, false)
            .0
        };
        let mut processor = Processor::new(SR);
        drop(processor.swap_graph(compile(&project)));
        processor.play();
        let (mut left, mut right) = (vec![0.0f32; 8 * BEAT], vec![0.0f32; 8 * BEAT]);
        for (l, r) in left.chunks_mut(441).zip(right.chunks_mut(441)) {
            if swap_every_block {
                drop(processor.swap_graph(compile(&project)));
            }
            processor.process(l, r);
        }
        (left, right)
    };
    let steady = run(false);
    assert!(rms(&steady.0) > 0.01);
    assert!(run(true) == steady, "graph swaps altered what the envelopes play");
}

#[test]
fn a_fader_moved_while_the_song_plays_gets_there_without_a_step() {
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "Lead", 8.0);
    let echo = project.add_track_of_kind("Echo", TrackKind::Return);
    let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
    let mut compile = |project: &Project| {
        Compiler {
            factory: &BuiltinFactory,
            bank: &mut bank,
            meters: &mut meters,
            audio: &audio,
            stand_ins: None,
            sample_rate: SR,
            purpose: Purpose::Playing,
        }
        .compile(project, false)
        .0
    };
    let mut processor = Processor::new(SR);
    drop(processor.swap_graph(compile(&project)));
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; 4 * BEAT], vec![0.0f32; 4 * BEAT]);
    // A beat of the song into the buffers from `from` on, in quarters.
    fn play(processor: &mut Processor, left: &mut [f32], right: &mut [f32], from: usize) {
        let (left, right) = (&mut left[from..from + BEAT], &mut right[from..from + BEAT]);
        for (l, r) in left.chunks_mut(BEAT / 4).zip(right.chunks_mut(BEAT / 4)) {
            processor.process(l, r);
        }
    }
    play(&mut processor, &mut left, &mut right, 0);
    let steady = largest_step(&left[BEAT / 2..BEAT]);
    // The fader is pulled right down, a send is opened by hand, and the
    // track is muted: each between two buffers, as an edit arrives.
    project.set_track_volume(track, -24.0).unwrap();
    drop(processor.swap_graph(compile(&project)));
    play(&mut processor, &mut left, &mut right, BEAT);
    project.set_send(track, echo, Some(0.0)).unwrap();
    drop(processor.swap_graph(compile(&project)));
    play(&mut processor, &mut left, &mut right, 2 * BEAT);
    project.track_mut(track).unwrap().mute = true;
    drop(processor.swap_graph(compile(&project)));
    play(&mut processor, &mut left, &mut right, 3 * BEAT);
    assert!(largest_step(&left) <= steady * 1.02, "a step of {} against {steady}", largest_step(&left));
    assert!(rms(&left[BEAT + BEAT / 2..2 * BEAT]) < rms(&left[BEAT / 2..BEAT]) * 0.07, "down 24 dB");
    assert!(rms(&left[2 * BEAT + BEAT / 2..3 * BEAT]) > rms(&left[BEAT + BEAT / 2..2 * BEAT]) * 1.9, "and the return");
    assert_eq!(peak(&left[3 * BEAT + 600..]), 0.0, "muted, ten milliseconds on");
}

/// Plays `first` for `swap_at` frames and `second` from there on, in
/// buffers of `block` frames cut so that the change falls on the same
/// frame whatever their size: an edit that arrives at one moment of the
/// song.
fn played_across_a_change(first: &Project, second: &Project, swap_at: usize, block: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
    let mut compile = |project: &Project| {
        Compiler {
            factory: &BuiltinFactory,
            bank: &mut bank,
            meters: &mut meters,
            audio: &audio,
            stand_ins: None,
            sample_rate: SR,
            purpose: Purpose::Playing,
        }
        .compile(project, false)
        .0
    };
    let mut processor = Processor::new(SR);
    drop(processor.swap_graph(compile(first)));
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; 2 * swap_at], vec![0.0f32; 2 * swap_at]);
    for half in 0..2 {
        if half == 1 {
            drop(processor.swap_graph(compile(second)));
        }
        let range = half * swap_at..(half + 1) * swap_at;
        for (l, r) in left[range.clone()].chunks_mut(block).zip(right[range].chunks_mut(block)) {
            processor.process(l, r);
        }
    }
    (left, right)
}

#[test]
fn a_solo_that_changes_over_a_send_that_moves_makes_no_step_at_any_buffer_size() {
    // A tone that is heard itself and through a return, its send swelling
    // all the while along its envelope.
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "Lead", 8.0);
    let echo = project.add_track_of_kind("Echo", TrackKind::Return);
    let send = AutomationTarget::Send { track, to: echo };
    project.set_automation(send, vec![point(0.0, -24.0), point(8.0, 0.0)]).unwrap();
    // Soloed, the return is heard alone, as the track feeds it. Muted, the
    // track is not heard and feeds nothing.
    let mut soloed = project.clone();
    soloed.track_mut(echo).unwrap().solo = true;
    let mut muted = project.clone();
    muted.track_mut(track).unwrap().mute = true;
    // The change comes four beats in, on a frame that is no buffer's edge
    // for most of these sizes and not where the wave crosses nothing.
    let swap_at = 4 * BEAT + 173;
    let plain = plain_sine(8.0);
    let steady = largest_step(&plain[BEAT..2 * BEAT]);

    for (name, before, after) in [
        ("soloing the return", &project, &soloed),
        ("letting go of the solo", &soloed, &project),
        ("muting the track", &project, &muted),
        ("unmuting the track", &muted, &project),
    ] {
        let reference = played_across_a_change(before, after, swap_at, 512);
        // No step: from one sample to the next it never moves by more
        // than the track and its return together do anyway.
        let around = largest_step(&reference.0[swap_at - 960..swap_at + 1920]);
        assert!(around <= 2.0 * steady * 1.02, "{name}: a step of {around}, where {steady} a part is usual");
        // The gate is through in ten milliseconds, and the send has gone on
        // along its envelope meanwhile: a little later the level is the
        // envelope's alone (with the track's own, where that is heard).
        let at = 5.0;
        let level = gain_db(&reference.0, &plain, at);
        let sent = Travel::SEND.between(-24.0, 0.0, at / 8.0);
        let both = 20.0 * (1.0 + 10f64.powf(sent / 20.0)).log10();
        let wanted = match name {
            "soloing the return" => sent,
            "muting the track" => f64::NEG_INFINITY,
            _ => both,
        };
        if wanted.is_finite() {
            assert!((level - wanted).abs() < 0.05, "{name}: {level} dB at beat {at}, for {wanted}");
        } else {
            assert_eq!(peak(&reference.0[swap_at + 600..]), 0.0, "{name}: nothing is heard ten milliseconds on");
        }
        // And it is the same samples however the buffers fall.
        for block in [480, 441, 37, 1024] {
            let other = played_across_a_change(before, after, swap_at, block);
            assert!(other == reference, "{name}: buffers of {block} frames produced different audio");
        }
    }
}

#[test]
fn a_lane_of_thousands_of_points_plays_the_curve_it_follows() {
    // As a compressor worked out on paper is drawn: 7,332 points on one
    // lane, a dip on every beat.
    let beats = 16.0;
    let count = 7_332;
    let dip = |beat: f64| -12.0 * (-(beat.fract()) * 6.0).exp();
    let points: Vec<AutomationPoint> =
        (0..count).map(|index| index as f64 * beats / count as f64).map(|beat| point(beat, dip(beat))).collect();
    let plain = plain_sine(beats);
    let mut project = Project::new("t");
    let track = sine_track(&mut project, "Lead", beats);
    project.set_automation(AutomationTarget::TrackVolume { track }, points).unwrap();
    let ducked = rendered(&project);
    for beat in [0.5, 3.02, 7.25, 12.6, 15.9] {
        let got = gain_db(&ducked, &plain, beat);
        assert!((got - dip(beat)).abs() < 0.1, "at beat {beat}: {got} dB for {} dB", dip(beat));
    }
    // And it is saved and loaded as it is.
    assert_eq!(Project::from_json(&project.to_json()).unwrap(), project);
}

/// A song the size of the bench's second pass: seventeen tracks and three
/// returns, 7,747 notes, and 29,443 automation points, of which 26,561 are
/// on seven lanes (one of 7,332) and the rest spread over a hundred more.
/// With `points` false it is the same song without its envelopes.
fn a_song_of_many_points(points: bool) -> Project {
    let mut project = Project::new("many");
    project.set_tempo(160.0).unwrap();
    let returns: Vec<TrackId> =
        (0..3).map(|index| project.add_track_of_kind(format!("Return {index}"), TrackKind::Return)).collect();
    for id in &returns {
        project.add_device(ChainRef::Track(*id), "builtin:delay", "Delay", None).unwrap();
    }
    let beats = 456.0;
    let dense = |count: usize| -> Vec<AutomationPoint> {
        (0..count)
            .map(|index| index as f64 * beats / count as f64)
            .map(|beat| point(beat, -12.0 * (-(beat.fract()) * 6.0).exp()))
            .collect()
    };
    // The lanes that are not drawings of a compressor: a hundred of them,
    // 2,882 points between them.
    let sparse = |lane: usize| -> Vec<AutomationPoint> {
        let count = if lane < 82 { 29 } else { 28 };
        (0..count).map(|index| point(index as f64 * 16.0, ((index * 7 + lane) % 30) as f64 / 30.0)).collect()
    };
    let big = [7_332usize, 3_205, 3_205, 3_205, 3_205, 3_205, 3_204];
    let mut lanes = 0;
    for index in 0..17 {
        let track = project.add_track(format!("Track {index}"));
        project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        let filter = project.add_device(ChainRef::Track(track), "builtin:filter", "Filter", None).unwrap();
        let utility = project.add_device(ChainRef::Track(track), "builtin:utility", "Utility", None).unwrap();
        let eq = project.add_device(ChainRef::Track(track), "builtin:eq", "EQ", None).unwrap();
        let clip = project.add_midi_clip(track, "clip", 0.0, beats).unwrap();
        let notes: Vec<Note> = (0..456)
            .map(|step| Note {
                pitch: 40 + ((step * 5 + index * 3) % 36) as u8,
                start: step as f64,
                length: 0.9,
                velocity: 90,
            })
            .collect();
        project.clip_mut(clip).unwrap().add_notes(&notes[..if index == 0 { 451 } else { 456 }]).unwrap();
        project.set_send(track, returns[index % 3], Some(-12.0)).unwrap();
        if !points {
            continue;
        }
        if let Some(count) = big.get(index) {
            let gain = AutomationTarget::DeviceParam { device: utility, param: "gain".into() };
            project.write_automation(gain, dense(*count), Travel::GAIN).unwrap();
        }
        let own = [
            (AutomationTarget::TrackVolume { track }, Travel::FADER, (-60.0, 0.0)),
            (AutomationTarget::TrackPan { track }, Travel::Linear, (-1.0, 1.0)),
            (AutomationTarget::Send { track, to: returns[index % 3] }, Travel::SEND, (-70.0, 0.0)),
            (AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() }, Travel::Log, (40.0, 9000.0)),
            (AutomationTarget::DeviceParam { device: eq, param: "low_cut".into() }, Travel::Log, (30.0, 400.0)),
            (AutomationTarget::DeviceParam { device: eq, param: "mid1_gain".into() }, Travel::Linear, (-12.0, 12.0)),
        ];
        for (target, travel, (low, high)) in own {
            if lanes == 100 {
                break;
            }
            let mut envelope = sparse(lanes);
            for point in &mut envelope {
                point.value = low + (high - low) * point.value;
            }
            project.write_automation(target, envelope, travel).unwrap();
            lanes += 1;
        }
    }
    project
}

/// Not a correctness test: prints what a song of 29,443 automation points
/// costs beside the same song without them. Run with
/// `cargo test -p trill-engine --test automation -- --ignored what_many --nocapture`.
#[test]
#[ignore = "a measurement, not an assertion"]
fn what_many_points_cost() {
    use std::time::Instant;
    use trill_engine::{AudioBackend, Session};

    /// The best of `runs` timings of `work`, in milliseconds.
    fn best(runs: usize, mut work: impl FnMut()) -> f64 {
        (0..runs)
            .map(|_| {
                let started = Instant::now();
                work();
                started.elapsed().as_secs_f64() * 1000.0
            })
            .fold(f64::INFINITY, f64::min)
    }

    for with_points in [false, true] {
        let project = a_song_of_many_points(with_points);
        let points: usize = project.automation.iter().map(|lane| lane.points.len()).sum();
        let notes: usize = project.tracks.iter().flat_map(|track| &track.clips).map(|clip| clip.notes().len()).sum();
        let longest = project.automation.iter().map(|lane| lane.points.len()).max().unwrap_or(0);
        eprintln!("--- {points} points on {} lanes (the longest {longest}), {notes} notes", project.automation.len());

        let json = project.to_json();
        eprintln!("project file: {:.0} KB", json.len() as f64 / 1024.0);
        let written = best(10, || drop(project.to_json()));
        let read = best(10, || drop(Project::from_json(&json).unwrap()));
        eprintln!("  written in {written:.2} ms, read in {read:.2} ms");
        eprintln!("a copy of the project (an undo step): {:.3} ms", best(50, || drop(project.clone())));
        let other = project.clone();
        eprintln!("comparing two: {:.3} ms", best(50, || assert!(project == other)));

        // What the session does at every edit: compile, and swap.
        let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
        let mut compile = || {
            Compiler {
                factory: &BuiltinFactory,
                bank: &mut bank,
                meters: &mut meters,
                audio: &audio,
                stand_ins: None,
                sample_rate: SR,
                purpose: Purpose::Playing,
            }
            .compile(&project, false)
            .0
        };
        let graph = compile();
        eprintln!("compiling the graph: {:.3} ms", best(50, || drop(compile())));

        // An edit through the front door, all of it: a fader moved.
        let mut session =
            Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
        let folder = common::folder();
        let path = common::file(&folder, "many.trill");
        std::fs::write(&path, &json).unwrap();
        session.load(&path).unwrap();
        let track = project.tracks[5].id;
        let mut level = -1.0;
        let edit = best(50, || {
            level -= 0.1;
            session.edit(|project| project.set_track_volume(track, level)).unwrap();
        });
        eprintln!("an edit through the session (a fader moved): {edit:.3} ms");
        // And one that rewrites the longest lane, as a drag in it does.
        if let Some(lane) = project.automation.iter().max_by_key(|lane| lane.points.len()).cloned() {
            let mut nudge = 0.0;
            let rewrite = best(50, || {
                nudge += 0.01;
                let mut points = lane.points.clone();
                points[100].value += nudge;
                session.edit(|project| project.set_automation(lane.target.clone(), points)).unwrap();
            });
            eprintln!("an edit that rewrites the lane of {longest} points: {rewrite:.3} ms");
        }

        // The audio thread: ten seconds of the song from its middle.
        let mut processor = Processor::new(SR);
        drop(processor.swap_graph(graph));
        processor.seek(200.0);
        processor.play();
        let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        let seconds = 10.0;
        let blocks = (seconds * SR / 512.0) as usize;
        let playing = best(3, || {
            for _ in 0..blocks {
                processor.process(&mut left, &mut right);
            }
        });
        eprintln!("playing {seconds} s of it: {playing:.1} ms ({:.2} % of real time)", playing / (seconds * 10.0));
    }
}
