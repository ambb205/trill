//! Watches an installed instrument load: makes it, started from a preset
//! if one is named, and plays its keys over and over, each by itself,
//! printing what each comes to beside what the program shows of its own
//! work (memory, bytes read, processor time, handles). For finding out
//! what a sampler does while it reads its samples, what of that can be
//! told from outside, and whether the wait before a probe and a render
//! ([`trill_engine::ready`]) ended when the instrument was all there.
//!
//! ```text
//! cargo run -p trill-plugins --example loading -- <device uri> [options]
//!
//! --preset <name>    start from this preset (a kit of Superior Drummer 3, say); given
//!                    several times, one instrument after another is made, `-` being
//!                    the instrument as it comes
//! --keys <a,b,..>    the keys to play (default: kick, snare, hat, a tom, ride, cymbals)
//! --seconds <n>      how long to go on with each instrument (default 40)
//! --every <ms>       how long to rest between rounds of the keys (default 100)
//! --pause <ms>       how long to rest between one instrument and the next (default 0)
//! --ring             do not silence the plugin after every key
//! --quiet            print only what came of each instrument, not every round
//! --probe            wait for the instrument as a probe and a render do, then play the
//!                    keys as a probe does (`--hold` and `--tail` seconds a key, default
//!                    0.05 and 0.15), again and again for `--seconds`
//! --trace            with `--probe`: print every try the wait makes
//! ```
//!
//! The caches are this program's own, in the temporary folder. No window
//! is opened, and nothing is heard.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{CompositeFactory, DeviceFactory, DeviceInstance, Event, EventKind, ProcessInfo, Purpose};
use trill_core::presets::{PresetInfo, PresetLookup};
use trill_core::{ChainRef, Project, TimeSignature};
use trill_dsp::BuiltinFactory;
use trill_engine::probe::{ProbeNote, ProbeOptions};
use trill_engine::ready::Vitals;
use trill_engine::session::ProbeTarget;
use trill_engine::{AudioBackend, Session};
use trill_plugins::{Scanner, Vst3Config, Vst3Factory};

const SAMPLE_RATE: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Above this a key has sounded: -80 dB.
const HEARD: f32 = 1e-4;

struct Options {
    device: String,
    presets: Vec<String>,
    keys: Vec<u8>,
    seconds: f64,
    every: u64,
    pause: u64,
    ring: bool,
    quiet: bool,
    probe: bool,
    hold: f64,
    tail: f64,
    trace: bool,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options {
        device: String::new(),
        presets: Vec::new(),
        keys: vec![36, 38, 42, 48, 51, 49, 57, 55, 52],
        seconds: 40.0,
        every: 100,
        pause: 0,
        ring: false,
        quiet: false,
        probe: false,
        hold: 0.05,
        tail: 0.15,
        trace: false,
    };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        fn number<T: std::str::FromStr>(name: &str, text: String) -> Result<T, String> {
            text.parse().map_err(|_| format!("{name} needs a number"))
        }
        match argument.as_str() {
            "--preset" => options.presets.push(value("--preset")?),
            "--keys" => {
                let keys = value("--keys")?;
                options.keys =
                    keys.split(',').map(|key| number("--keys", key.trim().to_string())).collect::<Result<_, _>>()?;
            }
            "--seconds" => options.seconds = number("--seconds", value("--seconds")?)?,
            "--every" => options.every = number("--every", value("--every")?)?,
            "--pause" => options.pause = number("--pause", value("--pause")?)?,
            "--hold" => options.hold = number("--hold", value("--hold")?)?,
            "--tail" => options.tail = number("--tail", value("--tail")?)?,
            "--ring" => options.ring = true,
            "--quiet" => options.quiet = true,
            "--probe" => options.probe = true,
            "--trace" => options.trace = true,
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            _ => options.device = argument.clone(),
        }
    }
    if options.device.is_empty() {
        return Err("which plugin? Give a device URI".to_string());
    }
    Ok(options)
}

impl Options {
    /// The instruments to make, one after another: each a preset to start
    /// from, or none.
    fn rounds(&self) -> Vec<Option<&str>> {
        match self.presets.as_slice() {
            [] => vec![None],
            named => named.iter().map(|name| Some(name.as_str()).filter(|name| *name != "-")).collect(),
        }
    }
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn level(peak: f32) -> String {
    if peak > HEARD { format!("{:>4.0}", 20.0 * peak.log10()) } else { "   -".to_string() }
}

fn factory() -> Result<Vst3Factory, String> {
    let scratch = std::env::temp_dir().join("trill-loading-example");
    let config = Vst3Config {
        scanner: Scanner::Auto,
        cache_path: Some(scratch.join("vst3").join("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    Vst3Factory::new(config).map_err(|error| error.to_string())
}

const HEADING: &str = "   time     in use      asked        read  reads      cpu  others  hndls";

/// One line of what the program shows, since `before`: memory in use and
/// asked for, bytes and reads from files, processor time of the program
/// and of its threads other than this one, and handles held.
fn vitals(at: f64, before: &Vitals) -> String {
    let now = Vitals::now().unwrap_or_default();
    format!(
        "{at:>7.2} s {:>7.1} MB {:>7.1} MB {:>8.1} MB {:>6} {:>6.2} s {:>6.2} s {:>5}",
        megabytes(now.in_use),
        megabytes(now.asked),
        megabytes(now.read - before.read),
        now.reads - before.reads,
        (now.busy - before.busy).as_secs_f64(),
        ((now.busy - before.busy).saturating_sub(now.own - before.own)).as_secs_f64(),
        now.handles
    )
}

/// Makes each instrument here and plays its keys over and over, each
/// from silence: the plugin is reset after every key.
fn watch(options: &Options) -> Result<(), String> {
    let factory = factory()?;
    let list =
        factory.presets_within(&options.device, Duration::from_secs(120)).ok_or("no such plugin is installed")?;
    for wanted in options.rounds() {
        let preset = match wanted {
            None => None,
            Some(wanted) => match list.find(wanted) {
                PresetLookup::Found(preset) => Some(preset.clone()),
                _ => return Err(format!("no one preset is '{wanted}'")),
            },
        };
        watch_one(options, &factory, preset.as_ref())?;
        std::thread::sleep(Duration::from_millis(options.pause));
    }
    Ok(())
}

fn watch_one(options: &Options, factory: &Vst3Factory, preset: Option<&PresetInfo>) -> Result<(), String> {
    let mut project = Project::new("loading example");
    let id = project
        .add_device(ChainRef::Master, options.device.clone(), "watched".to_string(), None)
        .map_err(|error| error.to_string())?;
    let device = project.device(id).map_err(|error| error.to_string())?;
    let before = Vitals::now().unwrap_or_default();
    let begun = Instant::now();
    let mut instance =
        factory.create_for(device, SAMPLE_RATE, 1024, Purpose::Rendering).map_err(|error| error.to_string())?;
    println!("made after {:.2} s", begun.elapsed().as_secs_f64());
    if let Some(preset) = preset {
        let controller = instance.controller.as_mut().ok_or("the plugin has no controller")?;
        controller.load_preset(preset).map_err(|error| error.to_string())?;
        println!("given '{}' after {:.2} s", preset.name, begun.elapsed().as_secs_f64());
    }
    let info = ProcessInfo {
        sample_rate: SAMPLE_RATE,
        tempo: 120.0,
        time_signature: TimeSignature::default(),
        playing: false,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    };
    let (mut left, mut right) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    // Runs the plugin for `blocks` blocks and returns the highest sample.
    let mut run = |instance: &mut DeviceInstance, events: &[Event], blocks: usize| {
        let mut peak = 0f32;
        for block in 0..blocks {
            left.fill(0.0);
            right.fill(0.0);
            instance.processor.process(&info, if block == 0 { events } else { &[] }, &mut left, &mut right);
            peak = left.iter().chain(&right).fold(peak, |peak, sample| peak.max(sample.abs()));
        }
        peak
    };
    println!("{HEADING}   keys {:?}, and what is left after a reset", options.keys);
    let mut sounding = vec![false; options.keys.len()];
    let mut arrived_at = 0.0;
    // When the program last read anything, and the pauses in its reading
    // that were followed by more.
    let (mut read_before, mut read_at) = (before.read, 0.0);
    let mut pauses: Vec<String> = Vec::new();
    while begun.elapsed().as_secs_f64() < options.seconds {
        let at = begun.elapsed().as_secs_f64();
        let line = vitals(at, &before);
        let read_now = Vitals::now().unwrap_or_default().read;
        if read_now != read_before {
            if at - read_at > 0.5 {
                let keys = sounding.iter().filter(|sounded| **sounded).count();
                let read = megabytes(read_before - before.read);
                pauses.push(format!("{read_at:.2} to {at:.2} s at {read:.0} MB with {keys} keys"));
            }
            (read_before, read_at) = (read_now, at);
        }
        let mut said = Vec::new();
        let mut left_over = 0f32;
        for (key, sounded) in options.keys.iter().zip(&mut sounding) {
            // 50 ms of the key, then let go and reset, with a block for the
            // reset to be acted on.
            let on = [Event { offset: 0, kind: EventKind::NoteOn { pitch: *key, velocity: 100.0 / 127.0 } }];
            let peak = run(&mut instance, &on, 10);
            run(&mut instance, &[Event { offset: 0, kind: EventKind::NoteOff { pitch: *key } }], 1);
            if !options.ring {
                instance.processor.reset();
                run(&mut instance, &[], 1);
                left_over = left_over.max(run(&mut instance, &[], 2));
            }
            if peak > HEARD && !*sounded {
                (*sounded, arrived_at) = (true, at);
            }
            said.push(level(peak));
        }
        if !options.quiet {
            println!("{line}  {} | {}", said.join(" "), level(left_over));
        }
        std::thread::sleep(Duration::from_millis(options.every));
    }
    println!(
        "{} of {} keys sounded, the last to arrive at {arrived_at:.2} s; reading stopped at {read_at:.2} s after \
         {:.0} MB; pauses in reading: {}",
        sounding.iter().filter(|sounded| **sounded).count(),
        sounding.len(),
        megabytes(read_before - before.read),
        if pauses.is_empty() { "none".to_string() } else { pauses.join("; ") }
    );
    println!("{}", vitals(begun.elapsed().as_secs_f64(), &before));
    Ok(())
}

/// Waits for each instrument as a probe does, then plays the keys as a
/// probe does.
fn probe(options: &Options) -> Result<(), String> {
    let factory = factory()?;
    factory.presets_within(&options.device, Duration::from_secs(120)).ok_or("no such plugin is installed")?;
    let devices: Arc<dyn DeviceFactory> =
        Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(factory.clone())]));
    let mut session =
        Session::new(devices, AudioBackend::Null { sample_rate: 48_000 }).map_err(|error| error.to_string())?;
    let notes: Vec<ProbeNote> =
        options.keys.iter().map(|pitch| ProbeNote { pitch: *pitch, velocity: 100, with: Vec::new() }).collect();
    let probe = ProbeOptions { hold_seconds: options.hold, tail_seconds: options.tail, ..ProbeOptions::default() };
    for preset in options.rounds() {
        println!("{}", preset.unwrap_or("as it comes"));
        let target = ProbeTarget::Device { uri: options.device.clone(), preset: preset.map(str::to_string) };
        let job = session.probe_job(&target).map_err(|error| error.to_string())?;
        let begun = Instant::now();
        let before = Vitals::now().unwrap_or_default();
        let mut probing = job.open(&notes, &probe).map_err(|error| error.to_string())?;
        println!(
            "the wait: {:.2} s, {}; back after {:.2} s",
            probing.waited_seconds,
            if probing.answered { "it answered" } else { "it never answered" },
            begun.elapsed().as_secs_f64()
        );
        println!("{HEADING}   keys {:?}", options.keys);
        let mut first = None;
        loop {
            let line = vitals(begun.elapsed().as_secs_f64(), &before);
            let heard = probing.play(&notes);
            let said: Vec<String> = heard
                .iter()
                .map(|note| if note.sounded { format!("{:>4.0}", note.peak_db) } else { level(0.0) })
                .collect();
            println!("{line}  {}", said.join(" "));
            first.get_or_insert(heard.iter().filter(|note| note.sounded).count());
            if begun.elapsed().as_secs_f64() >= options.seconds {
                break;
            }
            std::thread::sleep(Duration::from_millis(options.every));
        }
        println!("{} of {} keys sounded the first time they were played", first.unwrap_or_default(), notes.len());
        drop(probing);
        std::thread::sleep(Duration::from_millis(options.pause));
    }
    Ok(())
}

/// Prints what the wait says of every try it makes.
struct Said;

impl log::Log for Said {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target().starts_with("trill_engine::ready")
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            println!("  {}", record.args());
        }
    }

    fn flush(&self) {}
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let done = parse(&arguments).and_then(|options| {
        if options.trace && log::set_logger(&Said).is_ok() {
            log::set_max_level(log::LevelFilter::Trace);
        }
        if options.probe { probe(&options) } else { watch(&options) }
    });
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
