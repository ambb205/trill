//! The realtime rule, enforced: nothing a device does on the audio thread
//! may allocate or free memory. An allocator call can take a lock or ask the
//! operating system for pages, either of which can stall the audio thread
//! for longer than a buffer lasts.
//!
//! This test binary replaces the global allocator with one that counts the
//! calls made by the current thread while a device is being driven: every
//! built-in device, with every parameter swept over its whole range (which
//! takes each through every slope, shape and detector it has), and with a
//! key to listen to where it takes one.

mod common;

use common::MemoryLoader;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use trill_core::device::{DeviceFactory, DeviceInstance, Event, EventKind, KeyInput, ProcessInfo};
use trill_core::{ChainRef, Device, Project};
use trill_dsp::{Builtins, drumrack, sampler};

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

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

/// A loader with a stereo file at a different rate from the engine's, so
/// that the sample players interpolate, and long enough to loop.
fn factory() -> Builtins {
    let loader = MemoryLoader::default();
    let left = common::sine(196.0, 44_100.0, 30_000, 0.4);
    let right = common::sine(294.0, 44_100.0, 30_000, 0.4);
    loader.insert_stereo("tone.wav", left, right, 44_100.0);
    Builtins::with_loader(Arc::new(loader))
}

fn model(uri: &str) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, uri, "d", None).unwrap();
    let device = project.device_mut(id).unwrap();
    device.state = match uri {
        "builtin:sampler" => Some(sampler::State::new("tone.wav").to_bytes()),
        "builtin:drumrack" => {
            Some(drumrack::State::with_pads(drumrack::PAD_NOTES.map(|note| (note, "tone.wav"))).unwrap().to_bytes())
        }
        _ => None,
    };
    device.clone()
}

fn info(block: usize) -> ProcessInfo {
    ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: (block * BLOCK) as f64 / SR * 2.0,
        loop_range: None,
    }
}

/// Events for block `block`: far more notes than any device has voices, on
/// and off at awkward offsets, with the odd panic button.
fn events(block: usize) -> Vec<Event> {
    let mut events = Vec::new();
    let pitch = |step: usize| 24 + ((block * 7 + step * 5) % 72) as u8;
    for step in 0..6 {
        let offset = ((block * 37 + step * 97) % BLOCK) as u32;
        let kind = if (block + step).is_multiple_of(3) {
            EventKind::NoteOff { pitch: pitch(step + 1) }
        } else {
            EventKind::NoteOn { pitch: pitch(step), velocity: 0.3 + 0.1 * step as f32 }
        };
        events.push(Event { offset, kind });
    }
    // Drum notes too, so the drum devices are busy.
    events.push(Event { offset: 3, kind: EventKind::NoteOn { pitch: 36 + (block % 16) as u8, velocity: 0.9 } });
    if block % 50 == 49 {
        events.push(Event { offset: 100, kind: EventKind::AllNotesOff });
    }
    events.sort_by_key(|event| event.offset);
    events
}

/// Drives one device the way the engine does: blocks of audio with events,
/// parameters changing between blocks, the occasional reset.
fn drive(instance: &mut DeviceInstance, blocks: usize, all_events: &[Vec<Event>], input: &[f32]) {
    let mut left = [0.0f32; BLOCK];
    let mut right = [0.0f32; BLOCK];
    for (block, events) in all_events.iter().enumerate().take(blocks) {
        // Sweep every parameter across its whole range, a few per block.
        for step in 0..4 {
            let index = (block * 4 + step) % instance.params.len().max(1);
            if let Some(param) = instance.params.get(index) {
                let position = ((block / instance.params.len().max(1)) % 5) as f64 / 4.0;
                instance.processor.set_param(index, param.min + (param.max - param.min) * position);
            }
        }
        // Out-of-range indices and values must be ignored, not panic.
        instance.processor.set_param(usize::MAX, f64::NAN);
        instance.processor.set_param(0, f64::INFINITY);

        left.copy_from_slice(&input[..BLOCK]);
        right.copy_from_slice(&input[BLOCK..]);
        // Block sizes vary, as they do with some audio drivers.
        let frames = if block % 7 == 3 { 1 + (block * 31) % BLOCK } else { BLOCK };
        let events: &[Event] = if frames == BLOCK { events } else { &[] };
        // A device that can listen to another track does so every other
        // block, as when its sidechain is switched while it plays.
        if instance.processor.takes_key() && block % 2 == 1 {
            let key = KeyInput { left: &input[BLOCK..BLOCK + frames], right: &input[..frames] };
            instance.processor.process_keyed(&info(block), events, &mut left[..frames], &mut right[..frames], key);
        } else {
            instance.processor.process(&info(block), events, &mut left[..frames], &mut right[..frames]);
        }
        assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
        if block % 97 == 96 {
            instance.processor.reset();
        }
    }
}

#[test]
fn no_device_touches_the_allocator_on_the_audio_thread() {
    let factory = factory();
    let blocks = 600;
    let all_events: Vec<Vec<Event>> = (0..blocks).map(events).collect();
    let input: Vec<f32> = (0..BLOCK * 2).map(|i| common::noise(i) * 0.25).collect();

    for descriptor in factory.descriptors() {
        let mut instance = factory.create(&model(&descriptor.uri), SR, BLOCK).unwrap();
        // Once without watching: a device may finish setting itself up on
        // its first block only if `prepare` could not have done it, and none
        // of these has that excuse, so this is only to fault in pages.
        drive(&mut instance, 2, &all_events, &input);
        instance.processor.reset();

        let calls = allocator_calls(|| drive(&mut instance, blocks, &all_events, &input));
        assert_eq!(calls, 0, "{}: {calls} allocator calls while processing", descriptor.uri);
    }
}

#[test]
fn the_counting_allocator_counts() {
    // Guards the test above against passing because nothing is counted.
    let calls = allocator_calls(|| drop(std::hint::black_box(vec![0u8; 4096])));
    assert_eq!(calls, 2, "one allocation and one free");
    assert_eq!(allocator_calls(|| ()), 0);
}
