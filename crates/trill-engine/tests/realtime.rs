//! The realtime rule, enforced for what the engine itself does on the audio
//! thread: playing audio clips in every way it can (copied, interpolated
//! from a stand-in, cross-fading from a stand-in to the real audio, joined
//! across a split) and taking over a new graph must never allocate or free
//! memory.
//!
//! So must playing by hand and recording it: taking live input off its
//! queue, placing it, following the armed tracks, counting in, reporting
//! what was played and where, and letting go of it all. And so must
//! playing the controllers clips hold: reading their curves every few
//! frames, putting every one of them back where the loop starts, giving
//! way to a controller moved by hand and taking it back. And so must
//! playing envelopes: reading lanes of thousands of points for a fader, a
//! pan, a send and a device's parameter, gliding to where they are, and
//! picking up where a send had got to in the graph that takes over.
//!
//! And so must routing: a compressor that listens to another track is
//! handed that track's audio block by block, whatever order that puts the
//! tracks in, and a track that is not heard plays into a group that is
//! listened to.
//!
//! This test binary replaces the global allocator with one that counts the
//! calls made by the current thread while the processor runs. The devices
//! get the same treatment in `trill-dsp` and `trill-plugins`.

mod common;
mod rig;

use common::*;
use rig::Rig;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};
use trill_core::Project;
use trill_core::project::WarpMode;
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters, StandIns};
use trill_engine::graph::Graph;
use trill_engine::{AudioSources, Processor};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
    /// Set by a thread that a song is spread over: every call it makes
    /// from then on is counted, whoever is watching.
    static ON_A_TEAM: Cell<bool> = const { Cell::new(false) };
}

/// The calls made by the threads a song is spread over, all told.
static TEAM_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct CountingAllocator;

fn count() {
    // `try_with` because the allocator is also called while a thread's
    // locals are being torn down.
    let _ = WATCHING.try_with(|watching| {
        if watching.get() {
            let _ = CALLS.try_with(|calls| calls.set(calls.get() + 1));
        }
    });
    let _ = ON_A_TEAM.try_with(|on_a_team| {
        if on_a_team.get() {
            TEAM_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count();
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `work` and returns how many times it called the allocator.
fn allocator_calls(work: impl FnOnce()) -> usize {
    CALLS.with(|calls| calls.set(0));
    WATCHING.with(|watching| watching.set(true));
    work();
    WATCHING.with(|watching| watching.set(false));
    CALLS.with(Cell::get)
}

/// The controller's side of a live graph: what `Session` keeps.
struct Controller {
    bank: DeviceBank,
    meters: Meters,
    stand_ins: StandIns,
    audio: AudioSources,
}

impl Controller {
    fn compile(&mut self, project: &Project) -> Box<Graph> {
        Compiler {
            factory: &BuiltinFactory,
            bank: &mut self.bank,
            meters: &mut self.meters,
            audio: &self.audio,
            stand_ins: Some(&mut self.stand_ins),
            sample_rate: SR,
            purpose: trill_core::device::Purpose::Playing,
        }
        .compile(project, true)
        .0
    }

    /// Compiles until every clip has its real audio, and returns that graph.
    fn compile_prepared(&mut self, project: &Project) -> Box<Graph> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let graph = self.compile(project);
            if self.stand_ins.is_empty() {
                return graph;
            }
            assert!(Instant::now() < deadline, "the stretched audio never became ready");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Plays `frames` frames into `left`/`right` in blocks of awkward sizes and
/// returns the peak, so the caller can check that something was heard.
fn play(processor: &mut Processor, left: &mut [f32], right: &mut [f32], frames: usize) -> f32 {
    let mut level = 0.0f32;
    let mut done = 0;
    let mut block = 0;
    while done < frames {
        let size = [512, 441, 1, 37, 480][block % 5].min(frames - done);
        processor.process(&mut left[..size], &mut right[..size]);
        level = left[..size].iter().fold(level, |max, sample| max.max(sample.abs()));
        done += size;
        block += 1;
    }
    level
}

#[test]
fn playing_audio_clips_and_swapping_graphs_never_touches_the_allocator() {
    let folder = folder();
    let tone = write_wav(&file(&folder, "tone 100bpm.wav"), 44_100.0, &sine(440.0, 44_100.0, 4.8, 0.5));
    let clicks = write_wav(&file(&folder, "clicks 100bpm.wav"), SR, &click_track(100.0, 8, SR));

    // An unwarped clip cut in two (copied, joined at the cut) and a warped
    // one, looping over them so that wraps and seeks are covered too.
    let (mut project, track) = audio_project(120.0);
    let plain = add_clip(&mut project, track, &tone, 0.0, 8.0);
    project.split_clip(plain, 3.3).unwrap();
    let other = project.add_track("Warped");
    add_warped(&mut project, other, &tone, 8.0, WarpMode::Tonal, 100.0);
    project.set_loop(true, 1.0, 7.0).unwrap();

    let mut controller = Controller {
        bank: DeviceBank::default(),
        meters: Meters::default(),
        stand_ins: StandIns::default(),
        audio: AudioSources::default(),
    };
    let mut processor = Processor::new(SR);
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    // Graphs are freed by the controller, never by the audio thread: the
    // processor hands the old one back and it is dropped out here.
    let mut retired = Vec::with_capacity(8);
    retired.push(processor.swap_graph(controller.compile_prepared(&project)));
    processor.play();
    // Once without watching, to fault in pages.
    play(&mut processor, &mut left, &mut right, 4_096);

    // A third clip that is not ready: the graph plays its stand-in.
    let third = project.add_track("Clicks");
    add_warped(&mut project, third, &clicks, 8.0, WarpMode::Beats, 100.0);
    let with_stand_in = controller.compile(&project);
    assert_eq!(controller.stand_ins.len(), 1, "the new clip should be on a stand-in");
    let mut next = Some(with_stand_in);
    let mut level = 0.0;
    let calls = allocator_calls(|| {
        retired.push(processor.swap_graph(next.take().expect("a graph to swap in")));
        level = play(&mut processor, &mut left, &mut right, 6 * SR as usize);
    });
    assert!(level > 0.3, "the clips should be sounding: {level}");
    assert_eq!(calls, 0, "{calls} allocator calls while playing copied, joined and stand-in audio");

    // The real audio arrives mid-playback: a swap, a cross-fade, and on.
    let mut next = Some(controller.compile_prepared(&project));
    let calls = allocator_calls(|| {
        retired.push(processor.swap_graph(next.take().expect("a graph to swap in")));
        level = play(&mut processor, &mut left, &mut right, 6 * SR as usize);
        processor.seek(2.5);
        processor.stop();
        play(&mut processor, &mut left, &mut right, 2_048);
    });
    assert!(level > 0.3);
    assert_eq!(calls, 0, "{calls} allocator calls while handing over from a stand-in");
    assert_eq!(retired.len(), 3);
}

#[test]
fn playing_envelopes_never_touches_the_allocator() {
    use trill_core::project::{AutomationPoint, AutomationTarget, Segment};
    use trill_core::{ChainRef, TrackKind};

    let folder = folder();
    let tone = write_wav(&file(&folder, "tone.wav"), SR, &sine(440.0, SR, 4.0, 0.5));
    let (mut project, track) = audio_project(120.0);
    add_clip(&mut project, track, &tone, 0.0, 8.0);
    let utility = project.add_device(ChainRef::Track(track), "builtin:utility", "Utility", None).unwrap();
    let echo = project.add_track_of_kind("Echo", TrackKind::Return);
    let hall = project.add_track_of_kind("Hall", TrackKind::Return);
    project.add_device(ChainRef::Track(echo), "builtin:delay", "Delay", None).unwrap();
    project.set_send(track, hall, Some(-12.0)).unwrap();
    // A lane as a compressor drawn on paper leaves it: thousands of
    // points, a dip on every beat. And lines, steps and bends on the rest.
    let dips: Vec<AutomationPoint> = (0..7_332)
        .map(|index| index as f64 * 8.0 / 7_332.0)
        .map(|beat| AutomationPoint::new(beat, -12.0 * (-(beat.fract()) * 6.0).exp()))
        .collect();
    let point = AutomationPoint::new;
    let held = |beat: f64, value: f64| AutomationPoint { beat, value, shape: Segment::Hold };
    let bent = AutomationPoint { beat: 1.25, value: -3.0, shape: Segment::Curve(-0.6) };
    let throw = AutomationTarget::Send { track, to: echo };
    project.set_automation(AutomationTarget::TrackVolume { track }, dips.clone()).unwrap();
    project.set_automation(AutomationTarget::TrackPan { track }, vec![point(0.0, -1.0), point(8.0, 1.0)]).unwrap();
    project.set_automation(throw.clone(), vec![held(0.0, -70.0), point(1.0, -70.0), bent, held(3.0, -70.0)]).unwrap();
    project
        .set_automation(AutomationTarget::Send { track, to: hall }, vec![point(0.0, -30.0), point(8.0, 0.0)])
        .unwrap();
    project
        .set_automation(AutomationTarget::MasterVolume, vec![point(0.0, -6.0), point(4.0, 0.0), held(6.0, -3.0)])
        .unwrap();
    let gain = AutomationTarget::DeviceParam { device: utility, param: "gain".into() };
    project.write_automation(gain, dips, trill_core::Travel::GAIN).unwrap();
    // A tape stop on the same track: slowed to a stop, set going again and
    // let go, so that its reader comes in, reads between frames, stands
    // still and hands the sound back while the allocator is watched.
    let tape = project.add_device(ChainRef::Track(track), "builtin:tapestop", "Tape Stop", None).unwrap();
    let speed = AutomationTarget::DeviceParam { device: tape, param: "speed".into() };
    let spin = vec![held(0.0, 1.0), point(1.0, 1.0), held(1.75, 0.0), point(2.0, 0.0), point(2.5, 0.6), held(3.0, 1.0)];
    project.set_automation(speed, spin).unwrap();
    // Round a loop, so that every envelope jumps and is glided to.
    project.set_loop(true, 0.5, 3.5).unwrap();

    let mut controller = Controller {
        bank: DeviceBank::default(),
        meters: Meters::default(),
        stand_ins: StandIns::default(),
        audio: AudioSources::default(),
    };
    let mut processor = Processor::new(SR);
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut retired = Vec::with_capacity(8);
    retired.push(processor.swap_graph(controller.compile_prepared(&project)));
    processor.play();
    // Once without watching, to fault in pages.
    play(&mut processor, &mut left, &mut right, 4_096);

    // What an edit arrives as while the song plays: the throw's envelope is
    // switched off (the send is the hand's), a send is taken away, another
    // is opened by hand.
    project.set_automation_enabled(&throw, false).unwrap();
    project.set_send(track, echo, Some(-6.0)).unwrap();
    let held_by_hand = controller.compile_prepared(&project);
    project.set_send(track, hall, None).unwrap();
    project.set_automation_enabled(&throw, true).unwrap();
    let handed_back = controller.compile_prepared(&project);
    let mut next = [Some(held_by_hand), Some(handed_back)];
    let mut level = 0.0;
    let calls = allocator_calls(|| {
        level = play(&mut processor, &mut left, &mut right, 5 * SR as usize);
        retired.push(processor.swap_graph(next[0].take().expect("a graph to swap in")));
        play(&mut processor, &mut left, &mut right, 2 * SR as usize);
        retired.push(processor.swap_graph(next[1].take().expect("a graph to swap in")));
        play(&mut processor, &mut left, &mut right, 2 * SR as usize);
        processor.seek(2.25);
        play(&mut processor, &mut left, &mut right, 4_096);
        processor.stop();
        play(&mut processor, &mut left, &mut right, 2_048);
    });
    assert!(level > 0.05, "the clip should be sounding: {level}");
    assert_eq!(calls, 0, "{calls} allocator calls while playing envelopes");
    assert_eq!(retired.len(), 3);
}

#[test]
fn starting_and_coming_round_after_a_key_that_switches_never_touches_the_allocator() {
    use trill_core::{Articulation, ChainRef, InstrumentProfile, Lasts, Note};

    // A part that chooses a way of playing at the top, which stays, and
    // presses another key before every other note, for that note: wherever
    // the song starts, jumps or comes round, what is chosen there is
    // pressed again.
    let (mut project, track) = audio_project(120.0);
    project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    let profile = InstrumentProfile {
        articulations: vec![
            Articulation::key("open", 24).as_default(),
            Articulation::key("muted", 25),
            Articulation::key("slide", 27).lasting(Lasts::Once),
        ],
        ..InstrumentProfile::default()
    };
    project.set_track_profile(track, profile).unwrap();
    let clip = project.add_midi_clip(track, "part", 0.0, 8.0).unwrap();
    let note = |pitch: u8, start: f64, length: f64| Note { pitch, start, length, velocity: 100, ..Default::default() };
    let mut notes = vec![note(25, 0.0, 0.03)];
    for step in 0..32u8 {
        let at = f64::from(step) * 0.25;
        if step % 2 == 1 {
            notes.push(note(27, at - 0.03, 0.03));
        }
        notes.push(note(60 + step % 5, at, 0.2));
    }
    project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    project.set_loop(true, 1.0, 3.0).unwrap();

    let mut controller = Controller {
        bank: DeviceBank::default(),
        meters: Meters::default(),
        stand_ins: StandIns::default(),
        audio: AudioSources::default(),
    };
    let mut processor = Processor::new(SR);
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let retired = processor.swap_graph(controller.compile_prepared(&project));
    processor.play();
    // Once without watching, to fault in pages.
    play(&mut processor, &mut left, &mut right, 4_096);

    let mut level = 0.0;
    let calls = allocator_calls(|| {
        // Round the loop five times.
        level = play(&mut processor, &mut left, &mut right, 5 * SR as usize);
        // To just after a key that is for the next note, while it plays.
        processor.seek(2.24);
        play(&mut processor, &mut left, &mut right, 4_096);
        // And while it stands still, and started again from there.
        processor.stop();
        play(&mut processor, &mut left, &mut right, 2_048);
        processor.seek(1.5);
        play(&mut processor, &mut left, &mut right, 2_048);
        processor.play();
        play(&mut processor, &mut left, &mut right, 4_096);
    });
    assert!(level > 0.01, "the part should be sounding: {level}");
    assert_eq!(calls, 0, "{calls} allocator calls while keys that switch were pressed again");
    drop(retired);
}

#[test]
fn notes_and_controllers_on_sixteen_channels_never_touch_the_allocator() {
    use trill_core::project::AutomationPoint;
    use trill_core::{ChainRef, Channel, ControllerKind, Note};

    // A part that uses every channel: a key on each (the same key on
    // some), with a wheel, a pressure, an expression and a pedal on each.
    // What a track keeps of keys and controllers is kept by channel, and
    // all of it was made before the song plays.
    let (mut project, track) = audio_project(120.0);
    project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
    let clip = project.add_midi_clip(track, "part", 0.0, 8.0).unwrap();
    let mut notes = Vec::new();
    for channel in Channel::all() {
        for step in 0..16u8 {
            let start = f64::from(step) * 0.5 + f64::from(channel.number()) * 0.01;
            let pitch = 48 + (channel.number() % 4) * 3 + step % 3;
            notes.push(Note { pitch, start, length: 0.45, velocity: 90, ..Default::default() }.on(channel));
        }
    }
    let clip = project.clip_mut(clip).unwrap();
    clip.add_notes(&notes).unwrap();
    for channel in Channel::all() {
        let ramp = |from: f64, to: f64| vec![AutomationPoint::new(0.0, from), AutomationPoint::new(8.0, to)];
        clip.set_controller_on(channel, ControllerKind::PitchBend, ramp(-1.0, 1.0)).unwrap();
        clip.set_controller_on(channel, ControllerKind::Pressure, ramp(0.0, 1.0)).unwrap();
        clip.set_controller_on(channel, ControllerKind::EXPRESSION, ramp(1.0, 0.4)).unwrap();
        let pedal =
            vec![AutomationPoint::new(1.0, 1.0), AutomationPoint::new(2.0, 0.0), AutomationPoint::new(5.0, 1.0)];
        clip.set_controller_on(channel, ControllerKind::SUSTAIN, pedal).unwrap();
    }
    project.set_loop(true, 1.0, 5.0).unwrap();

    let mut controller = Controller {
        bank: DeviceBank::default(),
        meters: Meters::default(),
        stand_ins: StandIns::default(),
        audio: AudioSources::default(),
    };
    let mut processor = Processor::new(SR);
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let retired = processor.swap_graph(controller.compile_prepared(&project));
    processor.play();
    // Once without watching, to fault in pages.
    play(&mut processor, &mut left, &mut right, 4_096);

    let mut level = 0.0;
    let calls = allocator_calls(|| {
        // Round the loop twice.
        level = play(&mut processor, &mut left, &mut right, 5 * SR as usize);
        // A jump while it plays, with keys down on every channel.
        processor.seek(2.24);
        play(&mut processor, &mut left, &mut right, 4_096);
        // Stopped, put elsewhere, and started again from there.
        processor.stop();
        play(&mut processor, &mut left, &mut right, 2_048);
        processor.seek(3.5);
        play(&mut processor, &mut left, &mut right, 2_048);
        processor.play();
        play(&mut processor, &mut left, &mut right, 4_096);
    });
    assert!(level > 0.01, "the part should be sounding: {level}");
    assert_eq!(calls, 0, "{calls} allocator calls while sixteen channels played");
    drop(retired);
}

#[test]
fn a_song_spread_over_several_threads_touches_the_allocator_on_none_of_them() {
    use std::sync::atomic::Ordering;
    use trill_core::project::{AutomationPoint, AutomationTarget};
    use trill_core::{ChainRef, Note, TrackKind};
    use trill_engine::team::Team;

    // Six tracks that can be made at once (audio that is warped, synths
    // that are played and moved), a group of two of them, a return that
    // all of them feed, and a compressor that listens to another track:
    // every kind of round a block has.
    let folder = folder();
    let tone = write_wav(&file(&folder, "tone.wav"), SR, &sine(330.0, SR, 4.0, 0.4));
    let (mut project, audio) = audio_project(120.0);
    add_clip(&mut project, audio, &tone, 0.0, 8.0);
    let room = project.add_track_of_kind("Room", TrackKind::Return);
    project.add_device(ChainRef::Track(room), "builtin:reverb", "Reverb", None).unwrap();
    let band = project.add_track_of_kind("Band", TrackKind::Group);
    project.add_device(ChainRef::Track(band), "builtin:eq", "EQ", None).unwrap();
    let mut synths = Vec::new();
    for index in 0..5u8 {
        let track = project.add_track(format!("Synth {index}"));
        project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        let filter = project.add_device(ChainRef::Track(track), "builtin:filter", "Filter", None).unwrap();
        let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
        let notes: Vec<Note> = (0..16)
            .map(|step| Note {
                pitch: 48 + index * 4 + step % 5,
                start: f64::from(step) * 0.5,
                length: 0.4,
                velocity: 100,
                ..Default::default()
            })
            .collect();
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        project.set_send(track, room, Some(-12.0)).unwrap();
        if index < 2 {
            project.set_track_group(track, Some(band)).unwrap();
        }
        if index == 3 {
            let cutoff = AutomationTarget::DeviceParam { device: filter, param: "cutoff".into() };
            let line = vec![AutomationPoint::new(0.0, 300.0), AutomationPoint::new(8.0, 5000.0)];
            project.set_automation(cutoff, line).unwrap();
        }
        synths.push(track);
    }
    let duck = project.add_device(ChainRef::Track(synths[4]), "builtin:compressor", "Duck", None).unwrap();
    project.set_sidechain(duck, Some(synths[0])).unwrap();
    project.set_loop(true, 1.0, 7.0).unwrap();

    let mut controller = Controller {
        bank: DeviceBank::default(),
        meters: Meters::default(),
        stand_ins: StandIns::default(),
        audio: AudioSources::default(),
    };
    // The synths are said to keep something between them: they take
    // turns, in the order of the song, and what comes after each (its
    // filter, the compressor) is called out to whoever is free when its
    // turn is over.
    struct InTurn;
    impl trill_core::device::DeviceFactory for InTurn {
        fn descriptors(&self) -> Vec<trill_core::device::DeviceDescriptor> {
            BuiltinFactory.descriptors()
        }
        fn create(
            &self,
            device: &trill_core::Device,
            sample_rate: f64,
            max_block: usize,
        ) -> Result<trill_core::device::DeviceInstance, trill_core::device::DeviceError> {
            BuiltinFactory.create(device, sample_rate, max_block)
        }
        fn beside(&self, uri: &str) -> trill_core::device::Beside {
            match uri {
                "builtin:polysynth" => trill_core::device::Beside::InTurn("synths".to_string()),
                _ => trill_core::device::Beside::Anything,
            }
        }
    }
    let spread = |controller: &mut Controller, project: &Project| {
        let mut graph = controller.compile_prepared(project);
        assert!(graph.spread(project, &InTurn) >= 4, "the song can be made on several threads");
        graph
    };
    let mut processor = Processor::new(SR);
    let mut retired = Vec::with_capacity(8);
    retired.push(processor.swap_graph(spread(&mut controller, &project)));
    // Three threads beside this one, each of which counts whatever it
    // asks of the allocator from the moment it exists.
    assert!(processor.play_over(Some(Box::new(Team::settled(3, || ON_A_TEAM.set(true))))).is_none());
    processor.spread_from(0.0);
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    // Once without watching, to fault in pages.
    play(&mut processor, &mut left, &mut right, 4_096);

    // What an edit arrives as while the song plays: a track more.
    let extra = project.add_track("Extra");
    project.add_device(ChainRef::Track(extra), "builtin:polysynth", "Synth", None).unwrap();
    let mut next = Some(spread(&mut controller, &project));
    let before = TEAM_CALLS.load(Ordering::Relaxed);
    let mut level = 0.0;
    let calls = allocator_calls(|| {
        // Round the loop, with keys played by hand on two of the tracks.
        level = play(&mut processor, &mut left, &mut right, 3 * SR as usize);
        for (track, pitch) in [(synths[1], 72), (synths[2], 76)] {
            processor.live_event(track, trill_core::device::EventKind::NoteOn { pitch, velocity: 0.8 });
        }
        play(&mut processor, &mut left, &mut right, 2 * SR as usize);
        retired.push(processor.swap_graph(next.take().expect("a graph to swap in")));
        play(&mut processor, &mut left, &mut right, 2 * SR as usize);
        processor.seek(2.25);
        play(&mut processor, &mut left, &mut right, 4_096);
        processor.stop();
        play(&mut processor, &mut left, &mut right, 2_048);
        processor.play();
        play(&mut processor, &mut left, &mut right, 4_096);
    });
    assert!(level > 0.05, "the song should be sounding: {level}");
    assert_eq!(calls, 0, "{calls} allocator calls on the thread that asked for the blocks");
    let on_the_team = TEAM_CALLS.load(Ordering::Relaxed) - before;
    assert_eq!(on_the_team, 0, "{on_the_team} allocator calls on the threads the song is spread over");
    assert_eq!(processor.threads(), 4);
    assert!(processor.take_spread_blocks() > 500, "the blocks were spread");
    assert_eq!(retired.len(), 2);
}

#[test]
fn a_tempo_and_a_meter_that_change_never_touch_the_allocator() {
    use trill_core::project::{AutomationPoint, AutomationTarget, WarpMode};
    use trill_core::{ChainRef, Note};

    // A song that jumps, glides and changes meter, with a recording at its
    // own speed, one that follows the tempo (a piece for every tempo it
    // lies under), notes, and an envelope: everything that is counted in
    // beats, through everything the tempo does.
    let folder = folder();
    let tone = write_wav(&file(&folder, "tone.wav"), SR, &sine(440.0, SR, 4.0, 0.5));
    let (mut project, track) = audio_project(120.0);
    project.set_tempo_at(2.0, 96.5, false).unwrap();
    project.set_tempo_at(4.0, 96.5, false).unwrap();
    project.set_tempo_at(6.0, 150.0, true).unwrap();
    project.set_tempo_at(6.0, 133.0, false).unwrap();
    project.set_time_signature_at(4.0, 7, 8).unwrap();
    project.set_time_signature_at(7.5, 3, 4).unwrap();
    add_clip(&mut project, track, &tone, 0.0, 8.0);
    let follows = project.add_track("Loop");
    let warped = add_clip(&mut project, follows, &tone, 0.0, 8.0);
    project.set_clip_warp(warped, WarpMode::Repitch, Some(100.0), 0.0).unwrap();
    let keys = project.add_track("Keys");
    project.add_device(ChainRef::Track(keys), "builtin:polysynth", "Synth", None).unwrap();
    let clip = project.add_midi_clip(keys, "notes", 0.0, 8.0).unwrap();
    let notes: Vec<Note> = (0..32)
        .map(|step| Note { pitch: 60, start: step as f64 * 0.25, length: 0.2, velocity: 100, ..Default::default() })
        .collect();
    project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    let point = AutomationPoint::new;
    project.set_automation(AutomationTarget::TrackVolume { track }, vec![point(0.0, -12.0), point(8.0, 0.0)]).unwrap();
    // Round a loop that has every change in it.
    project.set_loop(true, 1.0, 7.75).unwrap();

    let mut controller = Controller {
        bank: DeviceBank::default(),
        meters: Meters::default(),
        stand_ins: StandIns::default(),
        audio: AudioSources::default(),
    };
    let mut processor = Processor::new(SR);
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut retired = Vec::with_capacity(8);
    retired.push(processor.swap_graph(controller.compile_prepared(&project)));
    processor.set_metronome(true);
    processor.play();
    // Once without watching, to fault in pages.
    play(&mut processor, &mut left, &mut right, 4_096);

    // An edit that arrives while it plays: the tempo is another from the
    // bar the song is in.
    project.set_tempo_at(2.0, 110.0, false).unwrap();
    let mut next = Some(controller.compile_prepared(&project));
    let mut level = 0.0;
    let calls = allocator_calls(|| {
        level = play(&mut processor, &mut left, &mut right, 8 * SR as usize);
        retired.push(processor.swap_graph(next.take().expect("a graph to swap in")));
        play(&mut processor, &mut left, &mut right, 6 * SR as usize);
        processor.seek(5.0);
        play(&mut processor, &mut left, &mut right, 3 * SR as usize);
        processor.stop();
        play(&mut processor, &mut left, &mut right, 2_048);
    });
    assert!(level > 0.05, "the song should be sounding: {level}");
    assert_eq!(calls, 0, "{calls} allocator calls while the tempo and the meter changed");
    assert_eq!(retired.len(), 2);
}

#[test]
fn a_device_that_listens_to_another_track_never_touches_the_allocator() {
    use trill_core::{ChainRef, TrackKind};

    let folder = folder();
    let tone = write_wav(&file(&folder, "tone.wav"), SR, &sine(110.0, SR, 4.0, 0.5));
    let clicks = write_wav(&file(&folder, "clicks 120bpm.wav"), SR, &click_track(120.0, 8, SR));

    // A bass whose compressor listens to the drum group, which comes after
    // it in the track list and is a group: the tracks are played in another
    // order than they are listed in. The drums send to a return, and the
    // master has a compressor that listens to the clicks themselves.
    let (mut project, bass) = audio_project(120.0);
    add_clip(&mut project, bass, &tone, 0.0, 8.0);
    let squeeze = project.add_device(ChainRef::Track(bass), "builtin:compressor", "Duck", None).unwrap();
    let params = &mut project.device_mut(squeeze).unwrap().params;
    params.extend([("threshold".to_string(), -40.0), ("ratio".to_string(), 20.0), ("attack".to_string(), 0.5)]);
    let drums = project.add_track_of_kind("Drums", TrackKind::Group);
    let room = project.add_track_of_kind("Room", TrackKind::Return);
    project.add_device(ChainRef::Track(room), "builtin:reverb", "Room", None).unwrap();
    let kit = project.add_track("Kit");
    add_clip(&mut project, kit, &clicks, 0.0, 8.0);
    project.set_track_group(kit, Some(drums)).unwrap();
    project.set_send(kit, room, Some(-6.0)).unwrap();
    project.set_sidechain(squeeze, Some(drums)).unwrap();
    let glue = project.add_device(ChainRef::Master, "builtin:compressor", "Glue", None).unwrap();
    project.device_mut(glue).unwrap().params.insert("detector".to_string(), 1.0);
    project.set_sidechain(glue, Some(kit)).unwrap();
    project.set_loop(true, 0.0, 8.0).unwrap();

    let mut controller = Controller {
        bank: DeviceBank::default(),
        meters: Meters::default(),
        stand_ins: StandIns::default(),
        audio: AudioSources::default(),
    };
    let mut processor = Processor::new(SR);
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut retired = Vec::with_capacity(8);
    retired.push(processor.swap_graph(controller.compile_prepared(&project)));
    processor.play();
    play(&mut processor, &mut left, &mut right, 4_096);

    // The same song with the bass soloed (the kit then plays into its group
    // for the compressor and into nothing else), and with the sidechain
    // taken away again: each a graph swapped in while the song plays.
    let mut soloed = project.clone();
    soloed.track_mut(bass).unwrap().solo = true;
    let mut plain = project.clone();
    plain.set_sidechain(squeeze, None).unwrap();
    let mut graphs = vec![controller.compile(&plain), controller.compile(&soloed), controller.compile(&project)];
    let mut levels = Vec::with_capacity(4);
    let calls = allocator_calls(|| {
        levels.push(play(&mut processor, &mut left, &mut right, 3 * SR as usize));
        while let Some(graph) = graphs.pop() {
            retired.push(processor.swap_graph(graph));
            levels.push(play(&mut processor, &mut left, &mut right, 3 * SR as usize));
        }
        processor.stop();
        play(&mut processor, &mut left, &mut right, 2_048);
    });
    assert!(levels.iter().all(|level| *level > 0.05), "the song should be sounding: {levels:?}");
    assert_eq!(calls, 0, "{calls} allocator calls while devices listened to other tracks");
    assert_eq!(retired.len(), 4);
}

#[test]
fn playing_by_hand_and_recording_never_touch_the_allocator() {
    use trill_core::device::EventKind;

    // The audio thread of this session is whoever asks the scripted device
    // for a buffer: this thread, watched for exactly that long. What the
    // session does in between (and it allocates freely) is not watched.
    let mut rig = Rig::new();
    let keys = rig.sine_track("Keys");
    let bass = rig.sine_track("Bass");
    let drums = rig.session.edit(|project| Ok(project.add_track("Drums"))).unwrap();
    rig.session.add_device(trill_core::ChainRef::Track(drums), "builtin:drumkit", None, None).unwrap();
    rig.session.edit(|project| project.set_loop(true, 0.0, 2.0)).unwrap();
    // Controllers in clips, played from the song's first frame: a swell, a
    // pedal, a bent wheel and a mod wheel under the take on one track, and
    // on another a clip that loops them and is entered off the beat.
    rig.session
        .edit(|project| {
            use trill_core::project::{AutomationPoint, Segment};
            use trill_core::{ControllerKind, Note};
            let point = AutomationPoint::new;
            let held = |beat: f64, value: f64| AutomationPoint { beat, value, shape: Segment::Hold };
            for (track, start, length) in [(keys, 0.0, 4.0), (drums, 0.0, 4.0)] {
                let id = project.add_midi_clip(track, "curves", start, length)?;
                let clip = project.clip_mut(id)?;
                clip.add_notes(&[Note { pitch: 40, start: 0.25, length: 0.5, velocity: 90, ..Default::default() }])?;
                let bent = AutomationPoint { beat: 0.5, value: 0.1, shape: Segment::Curve(0.7) };
                clip.set_controller(ControllerKind::EXPRESSION, vec![point(0.0, 0.4), bent, point(1.75, 1.0)])?;
                clip.set_controller(ControllerKind::SUSTAIN, vec![held(0.25, 1.0), held(1.5, 0.0)])?;
                clip.set_controller(ControllerKind::PitchBend, vec![point(0.0, 0.0), point(1.0, 0.5), held(1.0, 0.0)])?;
                clip.set_controller(ControllerKind::MOD_WHEEL, vec![point(0.0, 1.0), point(2.0, 0.0)])?;
                clip.set_controller(ControllerKind::Pressure, vec![point(0.0, 0.0), point(0.5, 1.0)])?;
            }
            let looped = project.track(drums)?.clips[0].id;
            project.clip_mut(looped)?.set_loop_length(Some(0.75))?;
            project.trim_clip_start(looped, 0.3).map(|_| ())
        })
        .unwrap();
    rig.session.set_metronome(true);
    rig.session.set_armed(keys, true).unwrap();
    rig.session.set_armed(bass, true).unwrap();
    // Once without watching, to fault in pages.
    rig.session.play_live(EventKind::NoteOn { pitch: 50, velocity: 0.5 });
    rig.run(8);
    rig.session.stop();
    rig.run(8);

    let mut calls = 0;
    let mut level = 0.0f32;
    let mut watched = |rig: &mut Rig, buffers: usize| {
        for _ in 0..buffers {
            calls += allocator_calls(|| {
                rig.host.advance(rig::BUFFER);
            });
            let meter = rig.session.master_meter().take();
            level = level.max(meter.0);
        }
    };

    // A bar of count-in, then four passes of a two-beat loop.
    rig.session.record().unwrap();
    let live = rig.session.live();
    for step in 0..620usize {
        let now = rig.host.now_ns();
        let early = |frames: f64| now.saturating_sub(Rig::ns(frames));
        let pitch = 48 + (step % 24) as u8;
        match step % 10 {
            // Keys go down and come up at odd moments within the buffer.
            0 => assert!(live.play_at(early(301.5), EventKind::NoteOn { pitch, velocity: 0.8 })),
            1 => assert!(live.play_at(early(77.0), EventKind::NoteOn { pitch: pitch + 7, velocity: 0.6 })),
            3 => assert!(live.play_at(early(12.0), EventKind::PitchBend { value: 0.4 })),
            4 => assert!(live.play_at(early(440.0), EventKind::Control { controller: 64, value: 1.0 })),
            5 => assert!(live.play_at(early(1.0), EventKind::NoteOff { pitch: pitch - 5 })),
            6 => assert!(live.play_at(early(200.0), EventKind::NoteOff { pitch: pitch + 1 })),
            7 => assert!(live.play_at(early(0.0), EventKind::Control { controller: 1, value: 0.7 })),
            8 => assert!(live.play_at(early(90.0), EventKind::Control { controller: 64, value: 0.0 })),
            // The hand takes the expression from the song now and then, and
            // leans on the keys.
            2 if step % 40 == 2 => rig.session.send_controller(keys, trill_core::ControllerKind::EXPRESSION, 0.3),
            2 => assert!(live.play_at(early(33.0), EventKind::ChannelPressure { value: 0.6 })),
            // And notes that name their track, as the window plays them.
            9 => rig.session.note_on(drums, 36 + (step % 3) as u8, 110),
            _ => {}
        }
        match step {
            // Arming changes while keys are down: the release path.
            150 => rig.session.set_armed(bass, false).unwrap(),
            260 => rig.session.set_armed(bass, true).unwrap(),
            // The keyboard's port goes away with keys down.
            340 => rig.session.release_live(),
            _ => {}
        }
        // The housekeeping thread writes what was played into the project,
        // which swaps a new graph in under the audio thread.
        if step % 20 == 19 {
            rig.session.housekeeping();
        }
        watched(&mut rig, 1);
    }
    assert!(
        rig.session.record_status().pass >= 3,
        "the loop should have come round: {:?}",
        rig.session.record_status()
    );

    // The take ends; the buffer that hears of it is watched too.
    rig.host.set_pump(false);
    rig.session.stop();
    watched(&mut rig, 4);
    assert!(level > 0.05, "something should have been sounding: {level}");
    assert_eq!(calls, 0, "{calls} allocator calls on the audio thread while playing by hand and recording");
    assert_eq!(rig.session.audio_faults(), 0);
    // It was a real take: there are notes in two clips, and what was
    // played on the wheels and the pedal with them.
    let take = rig.session.last_take();
    assert_eq!(take.len(), 2, "{take:?}");
    assert!(take.iter().all(|clip| clip.notes > 20), "{take:?}");
    assert!(take.iter().all(|clip| clip.controllers.len() >= 4), "{take:?}");
    assert!(take.iter().any(|clip| !clip.created), "one of them was recorded over the clip with the curves: {take:?}");
}

#[test]
fn the_counting_allocator_counts() {
    // Guards the test above against passing because nothing is counted.
    let calls = allocator_calls(|| drop(std::hint::black_box(vec![0u8; 4096])));
    assert_eq!(calls, 2, "one allocation and one free");
    assert_eq!(allocator_calls(|| ()), 0);
}

#[test]
fn inputs_that_are_heard_through_tracks_on_several_threads_touch_the_allocator_on_none_of_them() {
    use std::sync::atomic::Ordering;
    use trill_core::{ChainRef, Monitoring, Note, TrackInput};
    use trill_engine::team::Team;

    // Four tracks that each let a channel of the input through to an
    // effect of their own, one of them a pair, and two synths beside them:
    // six tracks that can be made at once, four of which read the input's
    // frames for the block.
    let mut project = Project::new("heard");
    for index in 0..4u16 {
        let track = project.add_track(format!("In {index}"));
        let input = TrackInput { channel: index + 1, stereo: index == 3, monitor: Monitoring::Always };
        project.set_track_input(track, Some(input)).unwrap();
        let effect = ["builtin:reverb", "builtin:eq", "builtin:compressor", "builtin:filter"][index as usize];
        project.add_device(ChainRef::Track(track), effect, "Effect", None).unwrap();
    }
    for index in 0..2u8 {
        let track = project.add_track(format!("Synth {index}"));
        project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        let clip = project.add_midi_clip(track, "clip", 0.0, 8.0).unwrap();
        let notes: Vec<Note> = (0..16)
            .map(|step| Note {
                pitch: 50 + index * 7,
                start: f64::from(step) * 0.5,
                length: 0.4,
                velocity: 100,
                ..Default::default()
            })
            .collect();
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
    }
    project.set_loop(true, 0.0, 8.0).unwrap();
    let mut controller = Controller {
        bank: DeviceBank::default(),
        meters: Meters::default(),
        stand_ins: StandIns::default(),
        audio: AudioSources::default(),
    };
    let mut graph = controller.compile_prepared(&project);
    assert!(graph.spread(&project, &BuiltinFactory) >= 4, "the song can be made on several threads");
    let mut processor = Processor::new(SR);
    let _first = processor.swap_graph(graph);
    assert!(processor.play_over(Some(Box::new(Team::settled(3, || ON_A_TEAM.set(true))))).is_none());
    processor.spread_from(0.0);
    // An input of five channels on a clock of its own, a little fast, in
    // buffers that are not the processor's.
    let (mut sink, source) = trill_engine::capture::pair(44_100.0, 5);
    processor.hear(source, 3000, false);
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut handed = 0u64;
    let mut made = 0u64;
    let mut run = |processor: &mut Processor, buffers: usize| -> f32 {
        let mut level = 0.0f32;
        for _ in 0..buffers {
            // What the input has handed over by the moment the buffer is
            // asked for, at 44.1 kHz and 180 parts a million fast.
            let now = made as f64 / SR;
            while (handed + 441) as f64 / (44_100.0 * 1.00018) <= now {
                let first = handed;
                let heard_ns = (first as f64 / (44_100.0 * 1.00018) * 1e9) as u64;
                sink.push(heard_ns, 441, |frame, channel| {
                    let at = (first + frame as u64) as f32 * (0.01 + channel as f32 * 0.003);
                    0.2 * at.sin()
                });
                handed += 441;
            }
            processor.begin_input((now * 1e9) as u64, 480);
            processor.process(&mut left[..480], &mut right[..480]);
            level = left[..480].iter().fold(level, |max, sample| max.max(sample.abs()));
            made += 480;
        }
        level
    };
    // Once without watching, to fault in pages and let the clocks be
    // told apart.
    run(&mut processor, 300);
    let before = TEAM_CALLS.load(Ordering::Relaxed);
    let mut level = 0.0;
    let calls = allocator_calls(|| {
        level = run(&mut processor, 600);
        processor.stop();
        run(&mut processor, 20);
        processor.play();
        run(&mut processor, 100);
    });
    assert!(level > 0.05, "the inputs should be heard: {level}");
    assert_eq!(calls, 0, "{calls} allocator calls on the thread that asked for the blocks and pushed the input");
    let on_the_team = TEAM_CALLS.load(Ordering::Relaxed) - before;
    assert_eq!(on_the_team, 0, "{on_the_team} allocator calls on the threads the song is spread over");
    assert!(processor.take_spread_blocks() > 500, "the blocks were spread");
}

#[test]
fn recording_an_input_and_hearing_it_never_touch_the_allocator() {
    use std::sync::Arc;
    use trill_core::{ChainRef, Monitoring, TrackInput};
    use trill_engine::host::scripted::{Hears, ScriptedHost, ScriptedInput};

    // The device's thread and the input's are this one, for a buffer:
    // watched for exactly that long, the input's buffers with it.
    let host = ScriptedHost::with_speakers();
    host.add_input(ScriptedInput { ppm: 150.0, ..ScriptedInput::new("Microphone") });
    host.hear(Hears::Sound(Arc::new(|channel, seconds| {
        (0.3 * (seconds * 2000.0 * (channel + 1) as f64).sin()) as f32
    })));
    let mut rig = Rig::with(host, trill_engine::AudioConfig::default(), trill_engine::Patience::default());
    let folder = folder();
    rig.session.keep_unsaved_recordings_in(folder.path().join("takes"));
    let limiter = rig.session.project().master.devices[0].id;
    rig.session.edit(|project| project.remove_device(limiter).map(|_| ())).unwrap();
    let voice = rig.session.edit(|project| Ok(project.add_track("Voice"))).unwrap();
    let pair = TrackInput { channel: 1, stereo: true, monitor: Monitoring::Always };
    rig.session.edit(|project| project.set_track_input(voice, Some(pair))).unwrap();
    rig.session.add_device(ChainRef::Track(voice), "builtin:reverb", None, None).unwrap();
    let guitar = rig.session.edit(|project| Ok(project.add_track("Guitar"))).unwrap();
    let one = TrackInput { channel: 2, stereo: false, monitor: Monitoring::Armed };
    rig.session.edit(|project| project.set_track_input(guitar, Some(one))).unwrap();
    rig.sine_track("Keys");
    rig.session.edit(|project| project.set_loop(true, 0.0, 2.0)).unwrap();
    rig.session.set_armed(voice, true).unwrap();
    rig.session.set_armed(guitar, true).unwrap();
    rig.session.housekeeping();
    assert!(rig.session.input_status().open.is_some());
    rig.run(60);

    let mut calls = 0;
    let mut level = 0.0f32;
    let mut watched = |rig: &mut Rig, buffers: usize| {
        for _ in 0..buffers {
            calls += allocator_calls(|| {
                rig.host.advance(rig::BUFFER);
            });
            level = level.max(rig.session.master_meter().take().0);
        }
    };
    // A bar of count-in, then four and a bit times round the loop, with
    // the session's chores (which allocate freely) between buffers; a
    // track disarmed and armed again on the way, and the take stopped.
    rig.session.record().unwrap();
    for step in 0..700 {
        match step {
            200 => rig.session.set_armed(guitar, false).unwrap(),
            260 => rig.session.set_armed(guitar, true).unwrap(),
            _ if step % 20 == 0 => rig.session.housekeeping(),
            _ => {}
        }
        watched(&mut rig, 1);
    }
    rig.session.stop();
    watched(&mut rig, 8);
    assert_eq!(calls, 0, "{calls} allocator calls on the audio thread and the input's while recording and monitoring");
    assert!(level > 0.05, "the input was heard: {level}");
    assert_eq!(rig.session.audio_faults(), 0);
    let recorded = rig.session.last_recording().expect("the take has ended").clone();
    assert!(recorded.takes.len() >= 8, "four passes on two tracks: {recorded:?}");
    assert_eq!(recorded.silent_frames, 0);
}
