//! Proves that the host side of a plugin does not touch the memory
//! allocator on the audio thread.
//!
//! This test binary replaces Rust's global allocator with one that counts.
//! Plugins are separate libraries with allocators of their own, so what is
//! counted is exactly this crate's code (and that of the bindings it uses)
//! and none of theirs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use trill_core::device::{DeviceFactory, Event, EventKind, KeyInput, ProcessInfo};
use trill_core::{ChainRef, Channel, Project};
use trill_plugins::{ClapConfig, ClapFactory, Scanner, Vst3Config, Vst3Factory};

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

/// How long the presets of one plugin may take to read, and a preset to
/// load.
const PRESET_PATIENCE: std::time::Duration = std::time::Duration::from_secs(300);

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
    let channel = |number: u8| Channel::new(number).expect("a channel");
    let (second, sixth, twelfth) = (channel(2), channel(6), channel(12));
    let events: Vec<Vec<Event>> = (0..64)
        .map(|block| match block % 16 {
            0 => vec![
                Event::new(0, EventKind::NoteOn { pitch: 60, velocity: 0.8 }),
                Event::new(17, EventKind::NoteOn { pitch: 64, velocity: 0.5 }),
                Event::new(511, EventKind::NoteOn { pitch: 67, velocity: 1.0 }),
            ],
            // Notes, a wheel, a pressure, a pedal and a controller on
            // other channels than the first: what the host keeps of keys
            // and pedals it keeps by channel, and the parameter a VST3
            // plugin names for a controller is looked up by channel.
            2 => vec![
                Event::on(sixth, 0, EventKind::NoteOn { pitch: 40, velocity: 0.9 }),
                Event::on(second, 1, EventKind::NoteOn { pitch: 40, velocity: 0.9 }),
                Event::on(sixth, 2, EventKind::PitchBend { value: 0.5 }),
                Event::on(second, 3, EventKind::ChannelPressure { value: 0.7 }),
                Event::on(sixth, 4, EventKind::Control { controller: 64, value: 1.0 }),
                Event::on(twelfth, 5, EventKind::Control { controller: 1, value: 0.3 }),
            ],
            // Six strings moving the wheel and the pressure all through a
            // block, as a guitar with a string to a channel does.
            3 => (0..16u32)
                .flat_map(|step| {
                    let value = step as f32 / 16.0;
                    (1..=6u8).flat_map(move |string| {
                        let channel = Channel::new(string).expect("a channel");
                        [
                            Event::on(channel, step * 32, EventKind::PitchBend { value }),
                            Event::on(channel, step * 32, EventKind::ChannelPressure { value }),
                        ]
                    })
                })
                .collect(),
            5 => vec![Event::new(3, EventKind::NoteOff { pitch: 64 })],
            6 => vec![
                Event::on(sixth, 0, EventKind::NoteOff { pitch: 40 }),
                Event::on(sixth, 9, EventKind::Control { controller: 64, value: 0.0 }),
            ],
            9 => vec![Event::new(0, EventKind::AllNotesOff)],
            _ => Vec::new(),
        })
        .collect();
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut big = (vec![0.0f32; 2_000], vec![0.0f32; 2_000]);
    let key = (vec![0.25f32; 2_000], vec![-0.25f32; 2_000]);

    let calls = allocator_calls(|| {
        for (block, events) in events.iter().enumerate() {
            let info = ProcessInfo {
                sample_rate: 48_000.0,
                tempo: 120.0,
                time_signature: Default::default(),
                playing: block % 8 != 7,
                position_beats: block as f64 * 512.0 / 24_000.0,
                song: Default::default(),
                loop_range: (block % 4 == 3).then_some((0.0, 8.0)),
            }
            .steady();
            // The very first block also starts the plugin.
            instance.processor.set_param(block % params, first.min);
            instance.processor.set_param((block * 7) % params, first.max);
            instance.processor.set_param(params + 1, 0.5);
            left.fill(0.1);
            right.fill(-0.1);
            // Every third block with a key to listen to, which a plugin
            // that has an input for one is then handed.
            match block % 3 {
                1 => {
                    let key = KeyInput { left: &key.0[..512], right: &key.1[..512] };
                    instance.processor.process_keyed(&info, events, &mut left, &mut right, key);
                }
                _ => instance.processor.process(&info, events, &mut left, &mut right),
            }
            if block == 30 {
                instance.processor.reset();
            }
            if block == 40 {
                // Larger than the plugin was set up for.
                instance.processor.process(&info, events, &mut big.0, &mut big.1);
                let key = KeyInput { left: &key.0, right: &key.1 };
                instance.processor.process_keyed(&info, events, &mut big.0, &mut big.1, key);
            }
            std::hint::black_box(instance.processor.latency_samples());
        }
    });
    assert_eq!(calls, 0, "{uri}: the host used the allocator on the audio thread");
    assert!(left.iter().chain(&right).all(|sample| sample.is_finite()), "{uri}");

    // A song that plays is made on more than one thread: a device is
    // processed by one of them at a time, and not by the same one from one
    // block to the next. Two threads take the blocks in turn here, neither
    // is the one the plugin was made on, and neither may allocate.
    let steady = ProcessInfo {
        sample_rate: 48_000.0,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    };
    let turn = std::sync::atomic::AtomicUsize::new(0);
    let blocks = 48;
    let hopped = {
        let taken = std::sync::Mutex::new((&mut instance.processor, &mut left, &mut right));
        let take_turns = |mine: usize| {
            allocator_calls(|| {
                for block in (mine..blocks).step_by(2) {
                    while turn.load(std::sync::atomic::Ordering::Acquire) != block {
                        std::hint::spin_loop();
                    }
                    let mut held = taken.lock().unwrap();
                    let (processor, left, right) = &mut *held;
                    left.fill(0.1);
                    right.fill(-0.1);
                    processor.set_param(block % params, first.max);
                    processor.process(&steady, &events[block % events.len()], left, right);
                    drop(held);
                    turn.store(block + 1, std::sync::atomic::Ordering::Release);
                }
            })
        };
        std::thread::scope(|scope| {
            let (one, other) = (scope.spawn(|| take_turns(0)), scope.spawn(|| take_turns(1)));
            (one.join().expect("a thread that plays the plugin"), other.join().expect("the other one"))
        })
    };
    assert_eq!(hopped, (0, 0), "{uri}: the host used the allocator on a thread a block was handed to");
    assert!(left.iter().chain(&right).all(|sample| sample.is_finite()), "{uri}");

    // A preset loaded while the plugin plays: asked for on another
    // thread, as the session does, and taken on by the plugin under the
    // audio thread's feet. Whatever of that passes through the host's
    // side of the audio thread (a program change is a parameter change)
    // must not allocate there either.
    let preset = factory.presets_within(uri, PRESET_PATIENCE).and_then(|list| list.presets.first().cloned());
    let (Some(preset), Some(mut controller)) = (preset, instance.controller.take()) else { return };
    let loading = std::thread::spawn(move || {
        let loaded = controller.load_preset(&preset);
        // The controller goes with the instance, on the thread it came from.
        (controller, preset, loaded)
    });
    let info = ProcessInfo {
        sample_rate: 48_000.0,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    };
    let patience = std::time::Instant::now() + PRESET_PATIENCE;
    let calls = allocator_calls(|| {
        // Until the plugin has answered, and a while longer for what it
        // does after answering.
        let mut after = 0;
        while after < 64 && std::time::Instant::now() < patience {
            after += usize::from(loading.is_finished());
            left.fill(0.1);
            right.fill(-0.1);
            instance.processor.process(&info, &events[after % events.len()], &mut left, &mut right);
        }
    });
    let (controller, preset, loaded) = loading.join().expect("the loading thread finished");
    instance.controller = Some(controller);
    match loaded {
        Ok(()) => eprintln!("{uri}: loaded '{}' while it played", preset.path()),
        Err(error) => eprintln!("{uri}: '{}' did not load ({error}); played on all the same", preset.path()),
    }
    assert_eq!(calls, 0, "{uri}: the host used the allocator on the audio thread while a preset was loaded");
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
        // The presets a plugin lists itself are enough to load one of;
        // the user's own preset files are not this test's to read.
        preset_paths: Vec::new(),
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
