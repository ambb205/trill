//! Lists the presets an installed plugin offers, the way the DAW finds
//! them, and optionally loads one: a small host around `trill-plugins` for
//! seeing what a plugin gives a host to work with.
//!
//! ```text
//! cargo run -p trill-plugins --example presets -- <device> [words...] [options]
//!
//! <device>            a device URI (`clap:<plugin id>`, `vst3:<class id>`), or a scheme
//!                     and a plugin's name (`clap:Surge XT`), or `list`
//! [words...]          only the presets that have every word
//! --category <path>   only the presets filed under this category
//! --limit <n>         how many to print (default 15)
//! --fresh             read the presets again, whatever the cache holds
//! --load <preset>     create the plugin, play a note, load this preset (an id, a name,
//!                     or a category and a name; or the whole path of a preset file that
//!                     lies anywhere), play the note again, and say how the sound and
//!                     the saved state changed
//! ```
//!
//! The caches are this program's own, in the temporary folder. No window
//! is opened.

use std::process::ExitCode;
use std::time::{Duration, Instant};
use trill_core::device::{
    CompositeFactory, DeviceDescriptor, DeviceFactory, DeviceInstance, Event, EventKind, ProcessInfo,
};
use trill_core::presets::{PresetFile, PresetList, PresetLookup, PresetQuery, PresetStatus};
use trill_core::{ChainRef, Project, TimeSignature};
use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};

const SAMPLE_RATE: f64 = 48_000.0;
const BLOCK: usize = 512;
/// How long the presets of one plugin may take to read.
const PATIENCE: Duration = Duration::from_secs(300);
/// How long a note is held, and how long a plugin is given to take a
/// preset on before it is played again.
const NOTE_SECONDS: f64 = 1.0;
const SETTLE: Duration = Duration::from_millis(1500);

#[derive(Default)]
struct Options {
    device: String,
    words: Vec<String>,
    category: Option<String>,
    limit: usize,
    fresh: bool,
    load: Option<String>,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options { limit: 15, ..Options::default() };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match argument.as_str() {
            "--category" => options.category = Some(value("--category")?),
            "--limit" => options.limit = value("--limit")?.parse().map_err(|_| "--limit needs a number")?,
            "--load" => options.load = Some(value("--load")?),
            "--fresh" => options.fresh = true,
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            _ if options.device.is_empty() => options.device = argument.clone(),
            _ => options.words.push(argument.clone()),
        }
    }
    if options.device.is_empty() {
        return Err("which plugin? Give a device URI, or `list`".to_string());
    }
    Ok(options)
}

/// The descriptor `wanted` names: by its URI, or by scheme and name.
fn find<'a>(descriptors: &'a [DeviceDescriptor], wanted: &str) -> Result<&'a DeviceDescriptor, String> {
    if let Some(exact) = descriptors.iter().find(|descriptor| descriptor.uri == wanted) {
        return Ok(exact);
    }
    let (scheme, name) = wanted.split_once(':').unwrap_or(("", wanted));
    let matching: Vec<&DeviceDescriptor> = descriptors
        .iter()
        .filter(|descriptor| scheme.is_empty() || descriptor.uri.starts_with(&format!("{scheme}:")))
        .filter(|descriptor| descriptor.name.eq_ignore_ascii_case(name))
        .collect();
    match matching.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no installed plugin is '{wanted}'; `list` shows what there is")),
        several => Err(format!(
            "'{wanted}' is one of several: {}",
            several.iter().map(|descriptor| descriptor.uri.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// What a held middle C sounds like: the peak and the loudness of a
/// second of it, and a number that tells one sound from another.
#[derive(Debug, PartialEq)]
struct Heard {
    peak: f32,
    rms: f32,
    fingerprint: u64,
}

fn play(instance: &mut DeviceInstance) -> Heard {
    let info = ProcessInfo {
        sample_rate: SAMPLE_RATE,
        tempo: 120.0,
        time_signature: TimeSignature::default(),
        playing: false,
        position_beats: 0.0,
        loop_range: None,
    };
    let blocks = (NOTE_SECONDS * SAMPLE_RATE / BLOCK as f64) as usize;
    let (mut peak, mut energy, mut fingerprint) = (0f32, 0f64, 0xcbf2_9ce4_8422_2325u64);
    let (mut left, mut right) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    // The tail of the note, and of whatever sounded before it, is played
    // out without being listened to.
    for block in 0..blocks * 3 {
        left.fill(0.0);
        right.fill(0.0);
        let events: &[Event] = match block {
            0 => &[Event { offset: 0, kind: EventKind::NoteOn { pitch: 60, velocity: 0.8 } }],
            b if b == blocks => &[Event { offset: 0, kind: EventKind::NoteOff { pitch: 60 } }],
            _ => &[],
        };
        instance.processor.process(&info, events, &mut left, &mut right);
        if block < blocks {
            for sample in left.iter().chain(&right) {
                peak = peak.max(sample.abs());
                energy += f64::from(*sample) * f64::from(*sample);
                // Coarse, so that noise in the last bits does not count.
                let step = (sample * 256.0).round() as i64 as u64;
                fingerprint = (fingerprint ^ step).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    Heard { peak, rms: (energy / (blocks * BLOCK * 2) as f64).sqrt() as f32, fingerprint }
}

/// Keeps a plugin processing while it does something on its own time.
fn idle(instance: &mut DeviceInstance, duration: Duration) {
    let info = ProcessInfo {
        sample_rate: SAMPLE_RATE,
        tempo: 120.0,
        time_signature: TimeSignature::default(),
        playing: false,
        position_beats: 0.0,
        loop_range: None,
    };
    let (mut left, mut right) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let until = Instant::now() + duration;
    while Instant::now() < until {
        left.fill(0.0);
        right.fill(0.0);
        instance.processor.process(&info, &[], &mut left, &mut right);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn create(
    factory: &dyn DeviceFactory,
    descriptor: &DeviceDescriptor,
    state: Option<Vec<u8>>,
) -> Result<DeviceInstance, String> {
    let mut project = Project::new("presets example");
    let id = project
        .add_device(ChainRef::Master, descriptor.uri.clone(), descriptor.name.clone(), None)
        .map_err(|error| error.to_string())?;
    project.device_mut(id).map_err(|error| error.to_string())?.state = state;
    let device = project.device(id).map_err(|error| error.to_string())?;
    factory.create(device, SAMPLE_RATE, BLOCK).map_err(|error| error.to_string())
}

fn load(
    factory: &dyn DeviceFactory,
    descriptor: &DeviceDescriptor,
    list: &PresetList,
    wanted: &str,
) -> Result<(), String> {
    // A preset that is asked for by the path of its file is the device
    // type's to make of the file; any other is one of the listed ones.
    let from_file = match PresetFile::asked(wanted) {
        Some(asked) => {
            let read = factory.preset_file(&descriptor.uri, &asked.path, asked.within.as_deref());
            let read = read.ok_or("the device is not one of this host's")?;
            Some(read.map_err(|reason| format!("'{}' is no preset of it: {reason}", asked.path.display()))?)
        }
        None => None,
    };
    let preset = match (&from_file, list.find(wanted)) {
        (Some(preset), _) => preset,
        (None, PresetLookup::Found(preset)) => preset,
        (None, PresetLookup::Ambiguous(found)) => {
            let paths: Vec<String> = found.iter().map(|preset| format!("{} ({})", preset.path(), preset.id)).collect();
            return Err(format!("several presets are '{wanted}': {}", paths.join(", ")));
        }
        (None, PresetLookup::Missing { similar }) => {
            let paths: Vec<String> = similar.iter().map(|preset| preset.path()).collect();
            return Err(format!("no preset is '{wanted}'; nearest: {}", paths.join(", ")));
        }
    };
    println!("\nloading {} ({})", preset.path(), preset.id);
    let mut instance = create(factory, descriptor, None)?;
    let before = play(&mut instance);
    let controller = instance.controller.as_mut().ok_or("the plugin has no controller")?;
    let state_before = controller.save_state();
    let started = Instant::now();
    controller.load_preset(preset).map_err(|error| format!("the preset did not load: {error}"))?;
    println!("the plugin answered in {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);

    // How long until the plugin's saved state shows the preset.
    let mut changed_after = None;
    let watch = Instant::now();
    while watch.elapsed() < SETTLE {
        idle(&mut instance, Duration::from_millis(10));
        let now = instance.controller.as_mut().and_then(|controller| controller.save_state());
        if changed_after.is_none() && now != state_before {
            changed_after = Some(started.elapsed());
        }
    }
    match changed_after {
        Some(after) => {
            println!("its saved state had changed {:.0} ms after it was asked", after.as_secs_f64() * 1000.0)
        }
        None => println!("its saved state did not change"),
    }
    let after = play(&mut instance);
    println!("before: peak {:.3}, rms {:.4}, sound {:016x}", before.peak, before.rms, before.fingerprint);
    println!("after:  peak {:.3}, rms {:.4}, sound {:016x}", after.peak, after.rms, after.fingerprint);
    println!("the sound {}", if before == after { "DID NOT CHANGE" } else { "changed" });

    // What a project would save, in a plugin of its own.
    let state = instance.controller.as_mut().and_then(|controller| controller.save_state());
    let size = state.as_ref().map_or(0, Vec::len);
    let mut restored = create(factory, descriptor, state)?;
    let at_once = play(&mut restored);
    idle(&mut restored, Duration::from_millis(300));
    let again = play(&mut restored);
    if at_once.rms < again.rms * 0.1 {
        println!("(a note played the moment the plugin was restored was lost: rms {:.4})", at_once.rms);
    }
    println!(
        "restored from its saved state ({size} bytes): peak {:.3}, rms {:.4}, sound {:016x}",
        again.peak, again.rms, again.fingerprint
    );
    let near = (again.rms - after.rms).abs() <= after.rms.max(1e-6) * 0.5;
    println!("the restored plugin sounds {}", if near { "like the preset" } else { "UNLIKE the preset" });
    Ok(())
}

fn run(options: &Options) -> Result<(), String> {
    let cache = std::env::temp_dir().join("trill-presets-example");
    let scanner = Scanner::Auto;
    let host = PluginHost::with_configs(
        ClapConfig { scanner: scanner.clone(), cache_path: Some(cache.join("clap.json")), ..ClapConfig::default() },
        Vst3Config { scanner, cache_path: Some(cache.join("vst3.json")), ..Vst3Config::default() },
    )
    .map_err(|error| format!("cannot start the plugin host: {error}"))?;
    let factory = CompositeFactory::new(host.factories());
    let descriptors = factory.descriptors();
    if options.device == "list" {
        for descriptor in &descriptors {
            println!("{}\t{}\t{}", descriptor.uri, descriptor.name, descriptor.vendor);
        }
        return Ok(());
    }
    let descriptor = find(&descriptors, &options.device)?;
    println!("plugin: {} by {} ({})", descriptor.name, descriptor.vendor, descriptor.uri);

    let started = Instant::now();
    if options.fresh {
        factory.refresh_presets(&descriptor.uri);
    }
    let first = factory.presets(&descriptor.uri).unwrap_or_default();
    println!(
        "asked in {:.1} ms: {} presets, {:?}",
        started.elapsed().as_secs_f64() * 1000.0,
        first.len(),
        first.status
    );
    let list = factory.presets_within(&descriptor.uri, PATIENCE).unwrap_or_default();
    println!("read in {:.2} s: {} presets", started.elapsed().as_secs_f64(), list.len());
    match &list.status {
        PresetStatus::Ready => {}
        PresetStatus::Reading => println!("still being read after {} s", PATIENCE.as_secs()),
        PresetStatus::Failed(reason) => println!("FAILED: {reason}"),
    }

    let categories = list.categories();
    println!("{} categories", categories.len());
    for (category, count) in categories.iter().take(options.limit) {
        println!("  {count:5}  {}", if category.is_empty() { "(none)" } else { category });
    }

    let text = options.words.join(" ");
    let query = PresetQuery { text: &text, category: options.category.as_deref(), offset: 0, limit: options.limit };
    let searched = Instant::now();
    let page = list.search(&query);
    println!(
        "\n{} match '{text}'{} (searched in {:.2} ms)",
        page.total,
        options.category.as_ref().map(|category| format!(" in '{category}'")).unwrap_or_default(),
        searched.elapsed().as_secs_f64() * 1000.0
    );
    for preset in &page.presets {
        let by = if preset.author.is_empty() { String::new() } else { format!("  by {}", preset.author) };
        let tags = if preset.tags.is_empty() { String::new() } else { format!("  [{}]", preset.tags.join(", ")) };
        println!("  {}  {}{by}{tags}", preset.id, preset.path());
    }

    if let Some(wanted) = &options.load {
        load(&factory, descriptor, &list, wanted)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let outcome = parse(&arguments).and_then(|options| run(&options));
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
