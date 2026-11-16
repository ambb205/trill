//! The realtime rule, enforced for what the engine itself does on the audio
//! thread: playing audio clips in every way it can (copied, interpolated
//! from a stand-in, cross-fading from a stand-in to the real audio, joined
//! across a split) and taking over a new graph must never allocate or free
//! memory.
//!
//! So must playing by hand and recording it: taking live input off its
//! queue, placing it, following the armed tracks, counting in, reporting
//! what was played and where, and letting go of it all.
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
}

struct CountingAllocator;

fn count() {
    // `try_with` because the allocator is also called while a thread's
    // locals are being torn down.
    let _ = WATCHING.try_with(|watching| {
        if watching.get() {
            let _ = CALLS.try_with(|calls| calls.set(calls.get() + 1));
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
    // It was a real take: there are notes in two clips.
    let take = rig.session.last_take();
    assert_eq!(take.len(), 2, "{take:?}");
    assert!(take.iter().all(|clip| clip.notes > 20), "{take:?}");
}

#[test]
fn the_counting_allocator_counts() {
    // Guards the test above against passing because nothing is counted.
    let calls = allocator_calls(|| drop(std::hint::black_box(vec![0u8; 4096])));
    assert_eq!(calls, 2, "one allocation and one free");
    assert_eq!(allocator_calls(|| ()), 0);
}
