//! Proves that the host side of a plugin does not touch the memory
//! allocator on the audio thread.
//!
//! This test binary replaces Rust's global allocator with one that counts.
//! Plugins are separate libraries with allocators of their own, so what is
//! counted is exactly this crate's code (and that of the bindings it uses)
//! and none of theirs.

use daw_core::device::{DeviceFactory, Event, EventKind, ProcessInfo};
use daw_core::{ChainRef, Project};
use daw_plugins::{ClapConfig, ClapFactory, Scanner, Vst3Config, Vst3Factory};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};

thread_local! {
    /// Whether this thread is currently playing the audio thread.
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    /// Allocator calls made by this thread while it was.
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

fn count() {
    // `try_with`: the allocator also runs while a thread's locals are being
    // torn down, when they can no longer be reached.
    let _ = WATCHING.try_with(|watching| {
        if watching.get() {
            let _ = CALLS.try_with(|calls| calls.set(calls.get() + 1));
        }
    });
}

// SAFETY: every method forwards its arguments unchanged to the system
// allocator, which upholds the `GlobalAlloc` contract; counting does not
// allocate (the thread-locals are `const`-initialized `Cell`s).
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count();
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `body` and returns how often it used the allocator.
fn allocator_calls(body: impl FnOnce()) -> usize {
    CALLS.set(0);
    WATCHING.set(true);
    body();
    WATCHING.set(false);
    CALLS.get()
}

#[test]
fn the_counter_counts() {
    assert_eq!(allocator_calls(|| {}), 0);
    assert!(allocator_calls(|| drop(std::hint::black_box(vec![0u8; 64]))) >= 2);
}

fn scanner() -> Scanner {
    Scanner::Executable(PathBuf::from(env!("CARGO_BIN_EXE_trill-plugin-scan")))
}

fn scratch(format: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(format).join("realtime-scan-cache.json")
}

/// Drives the device `uri` of `factory` the way the engine's audio thread
/// does, counting the allocator calls that makes.
fn drive(factory: &dyn DeviceFactory, uri: &str) {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, uri, "plugin", None).unwrap();
    let device = project.device_mut(id).unwrap();
    // A model value, so the first block carries parameter changes too.
    let probe = factory.create(device, 48_000.0, 512).unwrap();
    let first = probe.params[0].clone();
    drop(probe);
    device.params.insert(first.id.clone(), (first.min + first.max) / 2.0);
    let mut instance = factory.create(device, 48_000.0, 512).unwrap();
    let params = instance.params.len();

    // Everything the audio thread will touch is set up beforehand, as
    // the engine does.
    let events: Vec<Vec<Event>> = (0..64)
        .map(|block| match block % 16 {
            0 => vec![
                Event { offset: 0, kind: EventKind::NoteOn { pitch: 60, velocity: 0.8 } },
                Event { offset: 17, kind: EventKind::NoteOn { pitch: 64, velocity: 0.5 } },
                Event { offset: 511, kind: EventKind::NoteOn { pitch: 67, velocity: 1.0 } },
            ],
            5 => vec![Event { offset: 3, kind: EventKind::NoteOff { pitch: 64 } }],
            9 => vec![Event { offset: 0, kind: EventKind::AllNotesOff }],
            _ => Vec::new(),
        })
        .collect();
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut big = (vec![0.0f32; 2_000], vec![0.0f32; 2_000]);

    let calls = allocator_calls(|| {
        for (block, events) in events.iter().enumerate() {
            let info = ProcessInfo {
                sample_rate: 48_000.0,
                tempo: 120.0,
                time_signature: Default::default(),
                playing: block % 8 != 7,
                position_beats: block as f64 * 512.0 / 24_000.0,
                loop_range: (block % 4 == 3).then_some((0.0, 8.0)),
            };
            // The very first block also starts the plugin.
            instance.processor.set_param(block % params, first.min);
            instance.processor.set_param((block * 7) % params, first.max);
            instance.processor.set_param(params + 1, 0.5);
            left.fill(0.1);
            right.fill(-0.1);
            instance.processor.process(&info, events, &mut left, &mut right);
            if block == 30 {
                instance.processor.reset();
            }
            if block == 40 {
                // Larger than the plugin was set up for.
                instance.processor.process(&info, events, &mut big.0, &mut big.1);
            }
            std::hint::black_box(instance.processor.latency_samples());
        }
    });
    assert_eq!(calls, 0, "{uri}: the host used the allocator on the audio thread");
    assert!(left.iter().chain(&right).all(|sample| sample.is_finite()), "{uri}");
}

#[test]
fn processing_setting_parameters_and_resetting_do_not_allocate() {
    let factory = ClapFactory::new(ClapConfig {
        scanner: scanner(),
        cache_path: Some(scratch("clap-host")),
        ..ClapConfig::default()
    })
    .unwrap();
    let descriptors = factory.descriptors();
    if descriptors.is_empty() {
        eprintln!("skipping: no CLAP plugins are installed");
        return;
    }
    for descriptor in descriptors {
        drive(&factory, &descriptor.uri);
    }
}

/// The same for VST3, whose host side has more of its own on the audio
/// thread: the event and parameter-change lists a plugin calls back into
/// are this crate's objects.
#[test]
fn vst3_processing_setting_parameters_and_resetting_do_not_allocate() {
    let factory = Vst3Factory::new(Vst3Config {
        scanner: scanner(),
        cache_path: Some(scratch("vst3-host")),
        scan_timeout: std::time::Duration::from_secs(60),
        ..Vst3Config::default()
    })
    .unwrap();
    // Only plugins that are known not to ask for a license when created.
    let open = ["Surge XT", "Surge XT Effects", "WaveSynth", "WaveForge Development Shell", "JS80P"];
    let descriptors: Vec<_> =
        factory.descriptors().into_iter().filter(|descriptor| open.contains(&descriptor.name.as_str())).collect();
    if descriptors.is_empty() {
        eprintln!("skipping: none of the VST3 plugins this test uses is installed");
        return;
    }
    for descriptor in descriptors {
        drive(&factory, &descriptor.uri);
    }
}
