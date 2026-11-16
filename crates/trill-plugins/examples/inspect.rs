//! Shows what an installed plugin tells its host about itself: its
//! channels, its parameters with the text it shows for each value, what
//! it does to a stereo signal, and what its state is.
//!
//! The last is where reading a vendor's own presets begins. A plugin that
//! lists no presets still has a state, and its presets are files
//! somewhere. If the state is text, a document or a container that is
//! known, and the files are the same kind of thing, a preset can be made
//! into a state: `--state` says what the state is and writes it out to
//! look at, `--files` finds the files, and `--preset` shows what the
//! state becomes when one is loaded.
//!
//! ```text
//! cargo run -p trill-plugins --example inspect -- <device> [words...] [options]
//!
//! <device>            a device URI (`vst3:<class id>`, `clap:<plugin id>`), or a scheme
//!                     and a plugin's name (`vst3:Archetype Tim Henson X`), or `list`
//! [words...]          only the parameters whose name has every word
//! --changed           only the parameters that are not at the plugin's own default
//! --limit <n>         how many parameters to print (default 40)
//! --set <name>=<text> set a parameter by the text the plugin shows for a value
//!                     ("Mic=Dynamic 57", "Gain=-6 dB"), and say what it came to
//! --stereo            play noise that differs left and right through the plugin and
//!                     say how alike its two outputs are (1.0: the same on both sides)
//! --preset <preset>   load this preset first (an id, a name, or a category and a name)
//! --state <folder>    write the plugin's state into this folder, each half in a file
//!                     of its own, to look at
//! --files             list the files that look like presets in the places this
//!                     plugin's vendor would keep them (nothing is opened)
//! ```
//!
//! The scan cache is this program's own, in the temporary folder. No
//! window is opened, and nothing is written but what `--state` asks for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use trill_core::device::{CompositeFactory, DeviceDescriptor, DeviceFactory, DeviceInstance, ProcessInfo};
use trill_core::presets::PresetLookup;
use trill_core::{ChainRef, Project, TimeSignature};
use trill_plugins::{ClapConfig, PluginHost, Scanner, VendorPlaces, Vst3Config, describe_state, split_state};

const SAMPLE_RATE: f64 = 48_000.0;
const BLOCK: usize = 512;

#[derive(Default)]
struct Options {
    device: String,
    words: Vec<String>,
    changed: bool,
    limit: usize,
    set: Vec<(String, String)>,
    stereo: bool,
    preset: Option<String>,
    state: Option<PathBuf>,
    files: bool,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options { limit: 40, ..Options::default() };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match argument.as_str() {
            "--changed" => options.changed = true,
            "--stereo" => options.stereo = true,
            "--files" => options.files = true,
            "--preset" => options.preset = Some(value("--preset")?),
            "--state" => options.state = Some(value("--state")?.into()),
            "--limit" => options.limit = value("--limit")?.parse().map_err(|_| "--limit needs a number")?,
            "--set" => {
                let pair = value("--set")?;
                let (name, text) = pair.split_once('=').ok_or("--set needs <name>=<text>")?;
                options.set.push((name.trim().to_string(), text.trim().to_string()));
            }
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            _ if options.device.is_empty() => options.device = argument.clone(),
            _ => options.words.push(argument.to_lowercase()),
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

fn info() -> ProcessInfo {
    ProcessInfo {
        sample_rate: SAMPLE_RATE,
        tempo: 120.0,
        time_signature: TimeSignature::default(),
        playing: true,
        position_beats: 0.0,
        loop_range: None,
    }
}

/// Noise that is the same every time and different for every `seed`.
fn noise(seed: u32, index: usize) -> f32 {
    let x = (index as u32 ^ seed.wrapping_mul(0x9E37_79B9)).wrapping_mul(2_654_435_761).rotate_left(13);
    (x.wrapping_mul(0x85EB_CA6B) >> 8) as f32 / 8_388_608.0 - 1.0
}

/// How alike two signals are: 1 for the same, 0 for unrelated.
fn correlation(left: &[f32], right: &[f32]) -> f64 {
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| f64::from(*a) * f64::from(*b)).sum::<f64>();
    let power = (dot(left, left) * dot(right, right)).sqrt();
    if power < 1e-12 { 0.0 } else { dot(left, right) / power }
}

/// Plays two seconds of quiet noise through the plugin, different on each
/// side or only on the left, and returns the second second of what comes
/// out.
fn through(instance: &mut DeviceInstance, right_too: bool) -> (Vec<f32>, Vec<f32>) {
    let blocks = (2.0 * SAMPLE_RATE / BLOCK as f64) as usize;
    let (mut out_left, mut out_right) = (Vec::new(), Vec::new());
    for block in 0..blocks {
        let mut left: Vec<f32> = (0..BLOCK).map(|i| 0.1 * noise(1, block * BLOCK + i)).collect();
        let mut right: Vec<f32> =
            (0..BLOCK).map(|i| if right_too { 0.1 * noise(2, block * BLOCK + i) } else { 0.0 }).collect();
        instance.processor.process(&info(), &[], &mut left, &mut right);
        if block >= blocks / 2 {
            out_left.extend(left);
            out_right.extend(right);
        }
    }
    (out_left, out_right)
}

fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
}

/// How deep a vendor's folder is looked into, and how many of its files
/// are counted at most.
const FILES_DEPTH: usize = 7;
const FILES_COUNTED: usize = 300_000;
/// Words that are in too many vendors' and plugins' names to find a
/// folder by.
const COMMON_WORDS: [&str; 12] = [
    "audio",
    "sound",
    "sounds",
    "music",
    "plugin",
    "plugins",
    "records",
    "studio",
    "studios",
    "instruments",
    "software",
    "digital",
];

/// Whether a file looks like one that holds presets: by what it is
/// called, or by the folder of the vendor's it is in (`below` is its
/// path from the vendor's folder down).
fn preset_like(below: &Path) -> bool {
    let has = |text: &str| ["preset", "patch", "bank"].iter().any(|word| text.contains(word));
    let lowered =
        |part: Option<&std::ffi::OsStr>| part.map(|part| part.to_string_lossy().to_lowercase()).unwrap_or_default();
    let (extension, name) = (lowered(below.extension()), lowered(below.file_name()));
    let known = ["fxp", "fxb", "vstpreset", "aupreset", "nmsv", "h2p", "xml", "json", "ini", ""];
    let folders = lowered(below.parent().map(Path::as_os_str));
    has(&name) || extension.ends_with("state") || (has(&folders) && known.contains(&extension.as_str()))
}

/// Counts the files below `folder` that look like presets, by the folder
/// of the vendor's they are in and their extension.
fn count_files(
    vendor: &Path,
    folder: &Path,
    depth: usize,
    counted: &mut usize,
    found: &mut BTreeMap<(PathBuf, String), (usize, String)>,
) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        *counted += 1;
        if *counted > FILES_COUNTED {
            return;
        }
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() && depth < FILES_DEPTH => {
                count_files(vendor, &path, depth + 1, counted, found);
            }
            Ok(metadata) if metadata.is_file() && preset_like(path.strip_prefix(vendor).unwrap_or(&path)) => {
                let extension =
                    path.extension().map(|extension| extension.to_string_lossy().to_lowercase()).unwrap_or_default();
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                let entry = found.entry((folder.to_path_buf(), extension)).or_insert((0, name));
                entry.0 += 1;
            }
            _ => {}
        }
    }
}

/// Lists what looks like the presets of a plugin, in the places a vendor
/// keeps such things: the folders named after the vendor or the plugin in
/// the user's documents, the machine's shared data, the programs' folders
/// and the user's application data.
fn list_files(descriptor: &DeviceDescriptor) {
    let places = VendorPlaces::system();
    let mut roots: Vec<PathBuf> =
        places.documents.iter().chain(&places.shared_data).chain(&places.programs).cloned().collect();
    for name in ["APPDATA", "LOCALAPPDATA", "PUBLIC", "CommonProgramFiles"] {
        roots.extend(std::env::var_os(name).map(PathBuf::from));
    }
    roots.extend(std::env::var_os("PUBLIC").map(|public| PathBuf::from(public).join("Documents")));
    let words: Vec<String> = format!("{} {}", descriptor.vendor, descriptor.name)
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() >= 4 && !COMMON_WORDS.contains(&word.as_str()))
        .collect();
    let names = [descriptor.vendor.to_lowercase(), descriptor.name.to_lowercase()];
    let mut found = BTreeMap::new();
    let mut looked = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for folder in entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()) {
            let name = folder.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default();
            if names.contains(&name) || words.iter().any(|word| name.contains(word.as_str())) {
                let mut counted = 0;
                count_files(&folder, &folder, 0, &mut counted, &mut found);
                looked.push(folder);
            }
        }
    }
    println!("files: looked in {} folders named after '{}' or '{}'", looked.len(), descriptor.vendor, descriptor.name);
    for folder in &looked {
        println!("  {}", folder.display());
    }
    if found.is_empty() {
        println!("  nothing there looks like presets");
    }
    // The fullest folders first: presets come by the dozen.
    let mut listed: Vec<_> = found.into_iter().collect();
    listed.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for ((folder, extension), (count, example)) in listed.into_iter().take(24) {
        let kind = if extension.is_empty() { "without an extension".to_string() } else { format!(".{extension}") };
        println!("  {count:>5} {kind:<22} in {}  (such as '{example}')", folder.display());
    }
}

/// Says what a state is, and writes it into `folder` if one is given.
fn show_state(state: Option<Vec<u8>>, folder: Option<&Path>) -> Result<(), String> {
    let Some(state) = state else {
        println!("state: the plugin gives none");
        return Ok(());
    };
    let halves = match split_state(&state) {
        Some((component, controller)) => vec![("component", component), ("controller", controller)],
        None => vec![("state", state)],
    };
    for (name, bytes) in &halves {
        println!("{name} state: {} bytes: {}", bytes.len(), describe_state(bytes));
        if let (Some(folder), false) = (folder, bytes.is_empty()) {
            std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
            let file = folder.join(format!("{name}.bin"));
            std::fs::write(&file, bytes).map_err(|error| error.to_string())?;
            println!("  written to {}", file.display());
        }
    }
    Ok(())
}

fn run(options: &Options) -> Result<(), String> {
    let cache = std::env::temp_dir().join("trill-presets-example");
    let host = PluginHost::with_configs(
        ClapConfig { scanner: Scanner::Auto, cache_path: Some(cache.join("clap.json")), ..ClapConfig::default() },
        Vst3Config { scanner: Scanner::Auto, cache_path: Some(cache.join("vst3.json")), ..Vst3Config::default() },
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
    if options.files {
        list_files(descriptor);
    }

    let mut project = Project::new("inspect example");
    let id = project
        .add_device(ChainRef::Master, descriptor.uri.clone(), descriptor.name.clone(), None)
        .map_err(|error| error.to_string())?;
    let device = project.device(id).map_err(|error| error.to_string())?;
    let mut instance = factory.create(device, SAMPLE_RATE, BLOCK).map_err(|error| error.to_string())?;
    let params = instance.params.clone();
    let controller = instance.controller.as_mut().ok_or("the plugin has no controller")?;
    if let Some(wanted) = &options.preset {
        let list = factory.presets_within(&descriptor.uri, Duration::from_secs(300)).unwrap_or_default();
        let preset = match list.find(wanted) {
            PresetLookup::Found(preset) => preset.clone(),
            PresetLookup::Ambiguous(found) => {
                let paths: Vec<String> = found.iter().map(|preset| preset.path()).collect();
                return Err(format!("'{wanted}' is one of several presets: {}", paths.join(", ")));
            }
            PresetLookup::Missing { .. } => {
                return Err(format!("the plugin has no preset '{wanted}' among its {}", list.len()));
            }
        };
        controller.load_preset(&preset).map_err(|error| format!("'{}' was not loaded: {error}", preset.path()))?;
        println!("preset: loaded '{}'", preset.path());
        if let Some(note) = controller.take_preset_note() {
            println!("  {note}");
        }
    }
    show_state(controller.save_state(), options.state.as_deref())?;
    match controller.channels() {
        Some(layout) => println!("channels: {}", layout.describe()),
        None => println!("channels: not said"),
    }

    let values = controller.param_values().unwrap_or_else(|| params.iter().map(|param| param.default).collect());
    let defaults = controller.param_defaults().unwrap_or_else(|| params.iter().map(|param| param.default).collect());
    // A knob at its default is reported as 0.499999 by some plugins.
    let moved = |index: usize| (values[index] - defaults[index]).abs() > 1e-4 * (params[index].max - params[index].min);
    let changed = (0..params.len()).filter(|index| moved(*index)).count();
    println!("{} parameters, {changed} not at the plugin's own default", params.len());

    let shown: Vec<usize> = (0..params.len())
        .filter(|index| !options.changed || moved(*index))
        .filter(|index| {
            let name = params[*index].name.to_lowercase();
            options.words.iter().all(|word| name.contains(word.as_str()))
        })
        .collect();
    let page: Vec<usize> = shown.iter().copied().take(options.limit).collect();
    // The value, and the ends of its range, as the plugin shows them.
    let asked: Vec<(usize, f64)> = page
        .iter()
        .flat_map(|&index| [(index, values[index]), (index, params[index].min), (index, params[index].max)])
        .collect();
    let texts = controller.param_texts(&asked);
    println!("{} match; showing {}", shown.len(), page.len());
    for (row, &index) in page.iter().enumerate() {
        let text = |part: usize| texts[row * 3 + part].clone().unwrap_or_else(|| "?".to_string());
        println!(
            "  {:>10}  {:<40} {:>8.4}  {:<18} ({} .. {}){}",
            params[index].id,
            params[index].name,
            values[index],
            text(0),
            text(1),
            text(2),
            if moved(index) { "  *" } else { "" }
        );
    }

    for (name, text) in &options.set {
        let Some(index) = params.iter().position(|param| param.name.eq_ignore_ascii_case(name) || param.id == *name)
        else {
            println!("set: no parameter '{name}'");
            continue;
        };
        match controller.param_value(index, text) {
            Some(value) => {
                let shown = controller.param_texts(&[(index, value)]).remove(0).unwrap_or_default();
                println!(
                    "set: '{}' as '{text}' is the value {value:.6}, which the plugin shows as '{shown}'",
                    params[index].name
                );
                instance.processor.set_param(index, value);
            }
            None => println!("set: '{}' has no value the plugin shows as '{text}'", params[index].name),
        }
    }

    if options.stereo {
        // Let a value that was just set reach the plugin.
        std::thread::sleep(Duration::from_millis(50));
        let (left, right) = through(&mut instance, true);
        println!(
            "stereo in (different noise on each side): out correlation {:.3}, rms {:.4} / {:.4}",
            correlation(&left, &right),
            rms(&left),
            rms(&right)
        );
        let (left, right) = through(&mut instance, false);
        println!(
            "left only in: out correlation {:.3}, rms {:.4} / {:.4}",
            correlation(&left, &right),
            rms(&left),
            rms(&right)
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse(&arguments).and_then(|options| run(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
