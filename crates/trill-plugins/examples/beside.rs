//! Finds out what an installed plugin may be run beside when a render uses
//! more than one thread: whether a few instances of it give the same
//! samples run at once as run one after the other.
//!
//! A song is made with the plugin on three tracks of its own (as their
//! instrument, or after a built-in synth if it is an effect) and two
//! tracks of built-in devices, and rendered four times:
//!
//! 1. on one thread, and
//! 2. on one thread again: a plugin that does not do the same thing twice
//!    cannot be held to anything sample for sample;
//! 3. on several threads with its instances in turn, as a plugin nothing
//!    is known of is run: each block of each instance may be made by
//!    another thread than the last, beside the built-in tracks;
//! 4. on several threads with its instances at once.
//!
//! What is left of each later render when it is taken from the first says
//! what the plugin does. One that gives the same samples all four times
//! may be listed in `src/beside.rs` as one to run beside anything.
//!
//! ```text
//! cargo run -p trill-plugins --example beside                 every installed plugin
//! cargo run -p trill-plugins --example beside -- <uri> ...    these
//! cargo run -p trill-plugins --example beside -- --list       what is installed
//! ```
//!
//! Each plugin is measured in a process of its own, so that one that
//! crashes costs a line that says so. The caches are this program's own,
//! in the temporary folder. No window is opened, and nothing is heard.

use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};
use trill_core::controllers::ControllerKind;
use trill_core::device::{CompositeFactory, DeviceCategory, DeviceDescriptor, DeviceFactory};
use trill_core::project::AutomationPoint;
use trill_core::{ChainRef, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::render::{RenderOptions, Rendered, render};
use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};

/// How many tracks get the plugin.
const INSTANCES: usize = 3;
const BEATS: f64 = 8.0;

fn host() -> Result<PluginHost, String> {
    let scratch = std::env::temp_dir().join("trill-beside-example");
    let clap =
        ClapConfig { cache_path: Some(scratch.join("clap.json")), scanner: Scanner::Auto, ..ClapConfig::default() };
    let vst3 = Vst3Config {
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        scan_timeout: Duration::from_secs(60),
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    PluginHost::with_configs(clap, vst3).map_err(|error| error.to_string())
}

fn installed(host: &PluginHost) -> Vec<DeviceDescriptor> {
    host.clap().descriptors().into_iter().chain(host.vst3().descriptors()).collect()
}

/// The song: the plugin on [`INSTANCES`] tracks, each playing something
/// else, and two tracks of built-in devices.
fn song(plugin: &DeviceDescriptor) -> Project {
    let mut project = Project::new("beside");
    project.tempo = 120.0;
    let instrument = plugin.category == DeviceCategory::Instrument;
    // Keys that are something on a drum kit and on anything with a range.
    let lines: [&[u8]; 3] = [&[36, 43, 48, 38], &[40, 45, 52, 42], &[48, 55, 60, 49]];
    for index in 0..INSTANCES {
        let track = project.add_track(format!("{} {}", plugin.name, index + 1));
        if !instrument {
            project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        }
        project.add_device(ChainRef::Track(track), plugin.uri.clone(), plugin.name.clone(), None).unwrap();
        let clip = project.add_midi_clip(track, "clip", 0.0, BEATS).unwrap();
        let line = lines[index % lines.len()];
        let notes: Vec<Note> = (0..(BEATS * 2.0) as usize)
            .map(|step| Note { pitch: line[step % line.len()], start: step as f64 * 0.5, length: 0.4, velocity: 100 })
            .collect();
        let clip = project.clip_mut(clip).unwrap();
        clip.add_notes(&notes).unwrap();
        // A modelled wind is silent without its breath.
        let _ = clip.set_controller(ControllerKind::EXPRESSION, vec![AutomationPoint::new(0.0, 0.8)]);
        project.track_mut(track).unwrap().pan = index as f32 * 0.4 - 0.4;
    }
    for (name, pitch) in [("Built-in low", 45u8), ("Built-in high", 69)] {
        let track = project.add_track(name);
        project.add_device(ChainRef::Track(track), "builtin:polysynth", "Synth", None).unwrap();
        project.add_device(ChainRef::Track(track), "builtin:eq", "EQ", None).unwrap();
        let clip = project.add_midi_clip(track, "clip", 0.0, BEATS).unwrap();
        let notes: Vec<Note> =
            (0..BEATS as usize).map(|beat| Note { pitch, start: beat as f64, length: 0.8, velocity: 90 }).collect();
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        project.track_mut(track).unwrap().volume_db = -12.0;
    }
    project
}

/// What is left of `other` when it is taken from `first`, against
/// `first`, in dB: far down means the same samples.
fn left_over(first: &Rendered, other: &Rendered) -> f64 {
    let frames = first.left.len().min(other.left.len());
    let (mut left, mut whole) = (0.0f64, 1e-30f64);
    for frame in 0..frames {
        left += f64::from(first.left[frame] - other.left[frame]).powi(2);
        left += f64::from(first.right[frame] - other.right[frame]).powi(2);
        whole += f64::from(first.left[frame]).powi(2) + f64::from(first.right[frame]).powi(2);
    }
    10.0 * (left.max(1e-30) / whole).log10()
}

fn said(first: &Rendered, other: &Rendered) -> String {
    if first.left == other.left && first.right == other.right {
        return "the same samples".to_string();
    }
    // Where they first differ, and by how much at the most.
    let pairs = first.left.iter().zip(&other.left).chain(first.right.iter().zip(&other.right));
    let most = pairs.fold(0.0f32, |most, (one, other)| most.max((one - other).abs()));
    let differs = |(one, other): (&f32, &f32)| one != other;
    let at = first.left.iter().zip(&other.left).position(differs).unwrap_or(first.left.len().min(other.left.len()));
    // And second by second.
    let seconds: Vec<String> = (first.left.chunks(48_000).zip(other.left.chunks(48_000)))
        .map(|(one, other)| {
            let left: f64 = one.iter().zip(other).map(|(one, other)| f64::from(one - other).powi(2)).sum();
            let whole: f64 = one.iter().map(|one| f64::from(*one).powi(2)).sum();
            format!("{:.0}", 10.0 * (left.max(1e-30) / whole.max(1e-30)).log10())
        })
        .collect();
    format!(
        "{:.0} dB left (from {:.2} s, {:.0} dB at the most; by the second: {})",
        left_over(first, other),
        at as f64 / 48_000.0,
        20.0 * f64::from(most.max(1e-12)).log10(),
        seconds.join(" ")
    )
}

/// Measures one plugin, in this process.
fn measure(uri: &str) -> Result<(), String> {
    let host = host()?;
    let plugin = installed(&host).into_iter().find(|plugin| plugin.uri == uri).ok_or("no such plugin is installed")?;
    let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(BuiltinFactory)];
    factories.extend(host.factories());
    let factory = CompositeFactory::new(factories);
    let project = song(&plugin);
    let options = |threads: usize, trusting: bool| RenderOptions {
        max_tail_seconds: 1.0,
        threads: Some(threads),
        trusting,
        ..RenderOptions::default()
    };
    let timed = |threads: usize, trusting: bool| {
        // Said before each render, so that a process that ends in the
        // middle of one has said which.
        println!("STAGE a render on {threads} thread(s){}", if trusting { ", its instances at once" } else { "" });
        let begun = Instant::now();
        let rendered = render(&project, &factory, &options(threads, trusting)).map_err(|error| error.to_string())?;
        Ok::<_, String>((rendered, begun.elapsed().as_secs_f64()))
    };
    let (first, one) = timed(1, false)?;
    let loudest = first.left.iter().chain(&first.right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let plugin_peak = first.track_peaks.iter().take(INSTANCES).fold(0.0f32, |peak, (_, track)| peak.max(*track));
    let kind = if plugin.category == DeviceCategory::Instrument { "instrument" } else { "effect" };
    let mut line = format!(
        "RESULT {uri} | {} ({kind}) | peak {:.1} dB, its tracks {:.1} dB | one thread {one:.1} s",
        plugin.name,
        20.0 * loudest.max(1e-9).log10(),
        20.0 * plugin_peak.max(1e-9).log10(),
    );
    // With TRILL_BESIDE_IN_TURN set, its instances are never run at once:
    // for telling what a plugin that ends the process ends it over.
    let at_once = std::env::var_os("TRILL_BESIDE_IN_TURN").is_none();
    for (what, threads, trusting) in [
        ("alone again", 1, false),
        ("alone a third time", 1, false),
        ("in turn", INSTANCES + 2, false),
        ("in turn again", INSTANCES + 2, false),
        (if at_once { "at once" } else { "in turn a third time" }, INSTANCES + 2, at_once),
        (if at_once { "at once again" } else { "in turn a fourth time" }, INSTANCES + 2, at_once),
    ] {
        let (other, seconds) = timed(threads, trusting)?;
        line.push_str(&format!(
            " | {what} ({} threads, {seconds:.1} s): {}",
            other.measured.threads,
            said(&first, &other)
        ));
    }
    println!("{line}");
    Ok(())
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, uri] = arguments.as_slice()
        && flag == "--one"
    {
        return match measure(uri) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                println!("RESULT {uri} | could not be measured: {error}");
                ExitCode::from(3)
            }
        };
    }
    let host = match host() {
        Ok(host) => host,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let plugins = installed(&host);
    if arguments.iter().any(|argument| argument == "--list") {
        for plugin in &plugins {
            println!("{}  {} by {}", plugin.uri, plugin.name, plugin.vendor);
        }
        return ExitCode::SUCCESS;
    }
    let wanted: Vec<String> = match arguments.is_empty() {
        true => plugins.iter().map(|plugin| plugin.uri.clone()).collect(),
        false => arguments,
    };
    drop(host);
    let program = std::env::current_exe().expect("this program has a path");
    let mut crashed = 0;
    for uri in &wanted {
        let begun = Instant::now();
        match Command::new(&program).args(["--one", uri]).output() {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout);
                let result = text.lines().find(|line| line.starts_with("RESULT "));
                match (result, output.status.success()) {
                    (Some(result), _) => println!("{}", &result["RESULT ".len()..]),
                    (None, _) => {
                        crashed += 1;
                        println!(
                            "{uri} | THE PROCESS ENDED WITHOUT A RESULT after {:.0} s: {} | it had begun {} renders, the                              last of them: {}",
                            begun.elapsed().as_secs_f64(),
                            output.status,
                            text.lines().filter(|line| line.starts_with("STAGE ")).count(),
                            text.lines().last().unwrap_or("nothing")
                        );
                    }
                }
            }
            Err(error) => println!("{uri} | could not be started: {error}"),
        }
    }
    println!("{} plugins, {crashed} processes that ended without a result", wanted.len());
    ExitCode::SUCCESS
}
