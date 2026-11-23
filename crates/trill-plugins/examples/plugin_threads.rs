//! What an installed plugin does with the thread it is driven from: made,
//! played in real time and listened to, its state taken and given back,
//! its presets listed; then left to idle while the system's `sample` tool
//! says which threads ran the plugin's own code. For finding out, plugin by
//! plugin, whether a plugin needs its work to run on the process's first
//! thread, and whether it does when Trill drives it from there.
//!
//! ```text
//! cargo run -p trill-plugins --example plugin_threads -- <device> [how] [--seconds N]
//!
//! <device>   a device URI (`clap:<plugin id>`, `vst3:<class id>`), a scheme and a
//!            plugin's name (`vst3:Archetype Tim Henson X`), or `list`
//! [how]      first     the first thread drives the plugins, as the program without a
//!                      window and every job process do on macOS (the default there)
//!            own       a plugin thread of its own, and a first thread that runs no
//!                      loop (what those did before, and what Windows and Linux do)
//!            own-loop  a plugin thread of its own, and a first thread that runs its
//!                      loop (what the window did before)
//! --seconds  how long it is played, a second at a time (default 6)
//! ```
//!
//! An instrument is given a chord to hold and strike again every second; an
//! effect a quiet tone. Its scan and presets are this program's own, in the
//! temporary folder. No window is opened, and nothing is heard. Which
//! threads ran the plugin's code is said on macOS only, where `sample` is.

use std::process::ExitCode;
use std::time::{Duration, Instant};
use trill_core::device::{DeviceFactory, DeviceInstance, Event, EventKind, ProcessInfo};
use trill_core::{ChainRef, Project, TimeSignature};
use trill_plugins::{ClapConfig, ClapFactory, Scanner, Vst3Config, Vst3Factory};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn info() -> ProcessInfo {
    ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: TimeSignature::default(),
        playing: true,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    }
}

fn factory(scheme: &str) -> Result<Box<dyn DeviceFactory>, String> {
    let scratch = std::env::temp_dir().join("trill-plugin-threads");
    let made: std::io::Result<Box<dyn DeviceFactory>> = if scheme == "clap" {
        let config =
            ClapConfig { scanner: Scanner::Auto, cache_path: Some(scratch.join("clap.json")), ..ClapConfig::default() };
        ClapFactory::new(config).map(|factory| Box::new(factory) as Box<dyn DeviceFactory>)
    } else {
        let config = Vst3Config {
            scanner: Scanner::Auto,
            cache_path: Some(scratch.join("vst3.json")),
            preset_paths: vec![scratch.join("presets")],
            vendor_places: Default::default(),
            ..Vst3Config::default()
        };
        Vst3Factory::new(config).map(|factory| Box::new(factory) as Box<dyn DeviceFactory>)
    };
    made.map_err(|error| format!("the plugin host could not start: {error}"))
}

/// The URI of `device`, which may be a scheme and a plugin's name, and the
/// plugin's name.
fn find(factory: &dyn DeviceFactory, device: &str) -> Option<(String, String)> {
    let found = factory.descriptors().into_iter().find(|descriptor| {
        descriptor.uri == device
            || device.split_once(':').is_some_and(|(_, name)| descriptor.name.eq_ignore_ascii_case(name))
    })?;
    Some((found.uri.clone(), found.name.clone()))
}

/// Plays one second in real time and returns its peak.
fn second(instance: &mut DeviceInstance, from: &mut usize, instrument: bool) -> f32 {
    let blocks = (SR / BLOCK as f64) as usize;
    let begun = Instant::now();
    let mut peak = 0f32;
    for block in 0..blocks {
        let chord = [48u8, 55, 60, 64];
        let events: Vec<Event> = match (instrument, block) {
            (true, 0) => {
                chord.iter().map(|pitch| Event::new(0, EventKind::NoteOn { pitch: *pitch, velocity: 0.8 })).collect()
            }
            (true, 70) => chord.iter().map(|pitch| Event::new(0, EventKind::NoteOff { pitch: *pitch })).collect(),
            _ => Vec::new(),
        };
        let tone = |i: usize| if instrument { 0.0 } else { 0.1 * ((*from + i) as f32 * 0.06).sin() };
        let mut left: Vec<f32> = (0..BLOCK).map(tone).collect();
        let mut right = left.clone();
        *from += BLOCK;
        instance.processor.process(&info(), &events, &mut left, &mut right);
        peak = left.iter().chain(&right).fold(peak, |peak, sample| peak.max(sample.abs()));
        // In real time: some plugins make what they make by the clock.
        let due = begun + Duration::from_secs_f64((block + 1) as f64 * BLOCK as f64 / SR);
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    peak
}

fn probe(device: String, seconds: usize) -> Result<(), String> {
    let scheme = device.split_once(':').map_or("vst3", |(scheme, _)| scheme).to_string();
    let factory = factory(&scheme)?;
    if device.ends_with(":list") || device == "list" {
        for descriptor in factory.descriptors() {
            println!("{}  {}", descriptor.uri, descriptor.name);
        }
        return Ok(());
    }
    let (uri, name) = find(factory.as_ref(), &device).ok_or(format!("no plugin '{device}' is installed"))?;
    let instrument = factory.descriptors().iter().any(|descriptor| {
        descriptor.uri == uri && descriptor.category == trill_core::device::DeviceCategory::Instrument
    });
    let mut project = Project::new("plugin threads");
    let id = project.add_device(ChainRef::Master, &uri, &name, None).map_err(|error| error.to_string())?;
    let made = Instant::now();
    let mut instance = factory.create(project.device(id).unwrap(), SR, BLOCK).map_err(|error| error.to_string())?;
    println!("{name}: made in {:.0?}, {} parameters", made.elapsed(), instance.params.len());
    let driven = trill_plugins::first_thread::on_the_plugin_thread(Duration::from_secs(10), first_thread);
    println!("driven from {}", if driven == Some(true) { "the first thread" } else { "a thread of its own" });
    let mut from = 0;
    let peaks: Vec<String> =
        (0..seconds).map(|_| format!("{:.3}", second(&mut instance, &mut from, instrument))).collect();
    println!("played {seconds} s in real time, the peak of each second: {}", peaks.join(" "));
    if let Some(state) = instance.controller.as_mut().and_then(|controller| controller.save_state()) {
        project.device_mut(id).unwrap().state = Some(state.clone());
        let again = factory.create(project.device(id).unwrap(), SR, BLOCK).map_err(|error| error.to_string())?;
        let mut again = again;
        let back = again.controller.as_mut().and_then(|controller| controller.save_state());
        println!(
            "its state: {} bytes, given back {}",
            state.len(),
            if back == Some(state) { "the same" } else { "otherwise" }
        );
    }
    let presets = factory.presets_within(&uri, Duration::from_secs(60)).map_or(0, |list| list.presets.len());
    println!("presets it lists: {presets}");
    idle(&mut instance, &name);
    let gone = Instant::now();
    drop(instance);
    drop(factory);
    println!("let go of in {:.0?}", gone.elapsed());
    Ok(())
}

/// Whether the calling thread is the process's first.
fn first_thread() -> bool {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_main_np() -> i32;
        }
        // SAFETY: asks of the calling thread only.
        unsafe { pthread_main_np() == 1 }
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Lets the plugin idle (played silence, as a song that stands still) while
/// `sample` looks at every thread, and says which ran the plugin's code.
fn idle(instance: &mut DeviceInstance, name: &str) {
    let sampled = std::env::temp_dir().join(format!("trill-plugin-threads-{}.txt", std::process::id()));
    let looking = std::process::Command::new("/usr/bin/sample")
        .args([&std::process::id().to_string(), "2", "-file"])
        .arg(&sampled)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let begun = Instant::now();
    while begun.elapsed() < Duration::from_secs(3) {
        let (mut left, mut right) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        instance.processor.process(&info(), &[], &mut left, &mut right);
        std::thread::sleep(Duration::from_millis(10));
    }
    let Ok(mut looking) = looking else {
        println!("which threads ran its code: not known (no `sample` here)");
        return;
    };
    let _ = looking.wait();
    let Ok(text) = std::fs::read_to_string(&sampled) else { return };
    let _ = std::fs::remove_file(&sampled);
    // For each thread, the samples in which the plugin's code was called by
    // a run loop or the main queue: the plugin's own posted work, and not
    // what Trill asked of it or a thread of its own running.
    let mut threads: Vec<(String, usize)> = Vec::new();
    // What each line of the open call tree is: its depth, whether it is the
    // loop or the queue calling out, whether it is the plugin's code.
    let mut open: Vec<(usize, bool, bool)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.split_whitespace().nth(1).is_some_and(|word| word.starts_with("Thread_")) {
            let label = match (line.contains("com.apple.main-thread"), trimmed.split_once(": ")) {
                (true, _) => "the first thread".to_string(),
                (false, Some((_, named))) => format!("'{}'", named.trim()),
                (false, None) => "a thread without a name".to_string(),
            };
            threads.push((label, 0));
            open.clear();
            continue;
        }
        let Some(depth) = line.find(|c: char| c.is_ascii_digit()) else { continue };
        let Some(count) = line[depth..].split_whitespace().next().and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };
        while open.last().is_some_and(|(at, ..)| *at >= depth) {
            open.pop();
        }
        let callout = line.contains("CFRUNLOOP_IS_CALLING_OUT") || line.contains("_dispatch_main_queue");
        let plugin = line.contains(&format!("(in {name})"));
        let posted = open.iter().any(|(_, callout, _)| *callout);
        let first_of_it = !open.last().is_some_and(|(.., plugin)| *plugin);
        if plugin
            && posted
            && first_of_it
            && let Some(thread) = threads.last_mut()
        {
            thread.1 += count;
        }
        open.push((depth, callout, plugin));
    }
    let ran: Vec<String> = threads
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| format!("{label} ({count} samples)"))
        .collect();
    println!(
        "where its own posted work ran while it idled: {}",
        if ran.is_empty() { "nowhere that was looked at".to_string() } else { ran.join(", ") }
    );
}

fn main() -> ExitCode {
    trill_plugins::scan::run_as_scanner_if_requested();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(device) = args.first().cloned() else {
        eprintln!("usage: plugin_threads <device> [first|own|own-loop] [--seconds N]");
        return ExitCode::from(2);
    };
    let how = args.get(1).filter(|how| !how.starts_with("--")).cloned().unwrap_or_else(|| "first".into());
    let seconds =
        args.iter().position(|arg| arg == "--seconds").and_then(|at| args.get(at + 1)?.parse().ok()).unwrap_or(6);
    let run = move || match probe(device, seconds) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    };
    println!("the plugins are driven as `{how}` has it");
    match how.as_str() {
        #[cfg(target_os = "macos")]
        "first" => trill_plugins::first_thread::lend(run),
        #[cfg(target_os = "macos")]
        "own-loop" => {
            let worker = std::thread::spawn(run);
            unsafe extern "C" {
                static kCFRunLoopDefaultMode: *const std::ffi::c_void;
                fn CFRunLoopRunInMode(mode: *const std::ffi::c_void, seconds: f64, after_one: u8) -> i32;
            }
            while !worker.is_finished() {
                // SAFETY: the first thread runs its own loop for a moment.
                unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 0) };
            }
            ExitCode::from(worker.join().unwrap_or(1) as u8)
        }
        _ => ExitCode::from(run() as u8),
    }
}
