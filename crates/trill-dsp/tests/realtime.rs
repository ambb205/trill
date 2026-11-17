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
//!
//! The sampled instrument is the one device that has other threads work
//! for it while it plays: its recordings are longer than what is kept of
//! them, so every note here begins on a start and goes on into a whole
//! recording that a worker read meanwhile, and the budget is so small
//! that whole recordings are let go of all the time. None of that may
//! show on the audio thread: it takes what is there and frees nothing.

mod common;

use common::{MemoryLoader, MemoryTexts};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use trill_core::device::{DeviceFactory, DeviceInstance, Event, EventKind, KeyInput, ProcessInfo};
use trill_core::{ChainRef, Device, Project};
use trill_dsp::{Builtins, drumrack, sampled, sampler};

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
    // Recordings several times as long as what is kept of each, two of
    // them with two sides.
    for (index, hz) in [110.0, 147.0, 196.0, 262.0, 330.0, 440.0].into_iter().enumerate() {
        let left = common::sine(hz, 44_100.0, 70_000, 0.3);
        let right = common::sine(hz * if index % 3 == 0 { 1.5 } else { 1.0 }, 44_100.0, 70_000, 0.3);
        loader.insert_stereo(&format!("/i/long_{index}.wav"), left, right, 44_100.0);
    }
    let texts = MemoryTexts::default();
    texts.insert("/i/Busy.sfz", BUSY);
    let factory = Builtins::with_sources(Arc::new(loader), Box::new(texts));
    // Room for two whole recordings: the others come and go.
    factory.instruments().unwrap().pool.set_budget(2 * 70_000 * 8);
    factory
}

/// An instrument that does a little of everything: layers by velocity and
/// by a controller, takes in turn and at random, loops with and without a
/// crossfade, sounds for a key coming up, a filter with an envelope, a
/// vibrato on the wheel, switches, and groups that silence one another.
const BUSY: &str = "<control> set_cc1=20\n<global> sw_lokey=24 sw_hikey=26 sw_default=24 ampeg_release=0.3\n\
    <master> lokey=27 hikey=60\n\
    <group> sw_last=24 lovel=1 hivel=70 loop_mode=loop_sustain loop_start=20000 loop_end=30000 loop_crossfade=0.02\n\
    <region> sample=long_0.wav pitch_keycenter=40 xfout_locc1=30 xfout_hicc1=90 pitchlfo_freq=5 pitchlfo_depth_oncc1=40\n\
    <region> sample=long_1.wav pitch_keycenter=45 xfin_locc1=30 xfin_hicc1=90 cutoff=900 fileg_depth=3600 fileg_decay=0.2\n\
    <group> sw_last=24 lovel=71 hivel=127 seq_length=2 loop_mode=loop_continuous loop_start=20000 loop_end=64999\n\
    <region> sample=long_2.wav pitch_keycenter=50 seq_position=1 cutoff_cc74=2400 cutoff=2000 fil_type=hpf_1p\n\
    <region> sample=long_3.wav pitch_keycenter=50 seq_position=2 amplfo_freq=4 amplfo_depth=3\n\
    <group> sw_last=25 offset=18000 offset_random=500 delay_random=0.01 pitch_random=20 amp_random=2\n\
    <region> sample=long_4.wav pitch_keycenter=48 lorand=0 hirand=0.5\n\
    <region> sample=long_5.wav pitch_keycenter=48 lorand=0.5 hirand=1 end=40000\n\
    <master> lokey=61 hikey=96 loop_mode=one_shot\n\
    <region> sample=long_4.wav pitch_keycenter=72 group=1 off_by=2 note_polyphony=2\n\
    <region> sample=tone.wav key=62 group=2 off_mode=normal\n\
    <master> trigger=release lokey=27 hikey=60 rt_decay=3\n<region> sample=long_5.wav pitch_keycenter=50 volume=-12";

fn model(uri: &str) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, uri, "d", None).unwrap();
    let device = project.device_mut(id).unwrap();
    device.state = match uri {
        "builtin:sampler" => Some(sampler::State::new("tone.wav").to_bytes()),
        "builtin:drumrack" => {
            Some(drumrack::State::with_pads(drumrack::PAD_NOTES.map(|note| (note, "tone.wav"))).unwrap().to_bytes())
        }
        sampled::URI => Some(sampled::State::new("/i/Busy.sfz").to_bytes()),
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
        song: Default::default(),
        loop_range: None,
    }
    .steady()
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
    // The pedal down and up, the wheels moving, and now and then a key
    // that switches how an instrument is played.
    let control = |controller: u8, value: f32| EventKind::Control { controller, value };
    if block % 11 == 5 {
        events.push(Event { offset: 40, kind: control(64, if block % 22 == 5 { 1.0 } else { 0.0 }) });
    }
    events.push(Event { offset: 7, kind: control(1, ((block * 13) % 100) as f32 / 99.0) });
    events.push(Event { offset: 9, kind: control(74, ((block * 7) % 50) as f32 / 49.0) });
    events.push(Event { offset: 11, kind: EventKind::PitchBend { value: ((block * 29) % 200) as f32 / 100.0 - 1.0 } });
    if block % 17 == 2 {
        events.push(Event { offset: 1, kind: EventKind::NoteOn { pitch: 24 + (block % 3) as u8, velocity: 0.5 } });
    }
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
        // A device that reads what it plays in the background is given the
        // moment that takes: notes before it would only be silent.
        let reading = |instance: &DeviceInstance| instance.controller.as_ref().and_then(|c| c.reading());
        while reading(&instance).is_some_and(|reading| reading.is_loading()) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // Once without watching: a device may finish setting itself up on
        // its first block only if `prepare` could not have done it, and none
        // of these has that excuse, so this is only to fault in pages.
        drive(&mut instance, 2, &all_events, &input);
        instance.processor.reset();

        let calls = allocator_calls(|| drive(&mut instance, blocks, &all_events, &input));
        assert_eq!(calls, 0, "{}: {calls} allocator calls while processing", descriptor.uri);
    }
    // The sampled instrument was kept busy by what this is about: notes
    // that ran out of their start before the rest was read (the test
    // plays six seconds in a moment), and whole recordings that came all
    // the same and went again for the budget.
    let held = factory.instruments().unwrap().pool.held();
    assert!(held.short_notes > 0, "no note ran out of its start: {held:?}");
    assert!(held.whole_bytes <= held.budget + 70_000 * 8 * 6, "{held:?}");
}

#[test]
fn the_sampled_instrument_plays_from_what_workers_read_without_the_audio_thread_allocating() {
    // The same device, driven at the speed a song plays: slowly enough
    // for the workers to read the rest of every recording behind its
    // note, which is the path the test above cannot hold still for.
    let factory = factory();
    let mut instance = factory.create(&model(sampled::URI), SR, BLOCK).unwrap();
    let reading = |instance: &DeviceInstance| instance.controller.as_ref().and_then(|c| c.reading());
    while reading(&instance).is_some_and(|reading| reading.is_loading()) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let blocks = 120;
    let all_events: Vec<Vec<Event>> = (0..blocks).map(events).collect();
    let input = vec![0.0f32; BLOCK * 2];
    let (mut calls, mut loudest) = (0, 0.0f32);
    for block in 0..blocks {
        let mut left = [0.0f32; BLOCK];
        let mut right = [0.0f32; BLOCK];
        calls += allocator_calls(|| {
            instance.processor.process(&info(block), &all_events[block], &mut left, &mut right);
        });
        loudest = loudest.max(common::peak(&left));
        let _ = &input;
        // A block is ten milliseconds of song; give the workers three.
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    assert_eq!(calls, 0, "{calls} allocator calls while processing");
    assert!(loudest > 0.05, "it played: {loudest}");
    let held = factory.instruments().unwrap().pool.held();
    assert!(held.whole_bytes > 0, "whole recordings were read behind the notes: {held:?}");
}

#[test]
fn the_counting_allocator_counts() {
    // Guards the test above against passing because nothing is counted.
    let calls = allocator_calls(|| drop(std::hint::black_box(vec![0u8; 4096])));
    assert_eq!(calls, 2, "one allocation and one free");
    assert_eq!(allocator_calls(|| ()), 0);
}
