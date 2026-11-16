//! The realtime rule, enforced for what the engine itself does on the audio
//! thread: playing audio clips in every way it can (copied, interpolated
//! from a stand-in, cross-fading from a stand-in to the real audio, joined
//! across a split) and taking over a new graph must never allocate or free
//! memory.
//!
//! This test binary replaces the global allocator with one that counts the
//! calls made by the current thread while the processor runs. The devices
//! get the same treatment in `trill-dsp` and `trill-plugins`.

mod common;

use common::*;
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
fn the_counting_allocator_counts() {
    // Guards the test above against passing because nothing is counted.
    let calls = allocator_calls(|| drop(std::hint::black_box(vec![0u8; 4096])));
    assert_eq!(calls, 2, "one allocation and one free");
    assert_eq!(allocator_calls(|| ()), 0);
}
