//! Shows what an installed plugin tells its host about itself: its
//! channels, its parameters with the text it shows for each value, and
//! what it does to a stereo signal.
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
//! ```
//!
//! The scan cache is this program's own, in the temporary folder. No
//! window is opened.

use std::process::ExitCode;
use std::time::Duration;
use trill_core::device::{CompositeFactory, DeviceDescriptor, DeviceFactory, DeviceInstance, ProcessInfo};
use trill_core::{ChainRef, Project, TimeSignature};
use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};

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
    state: Option<std::path::PathBuf>,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options { limit: 40, ..Options::default() };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match argument.as_str() {
            "--changed" => options.changed = true,
            "--stereo" => options.stereo = true,
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

    let mut project = Project::new("inspect example");
    let id = project
        .add_device(ChainRef::Master, descriptor.uri.clone(), descriptor.name.clone(), None)
        .map_err(|error| error.to_string())?;
    let device = project.device(id).map_err(|error| error.to_string())?;
    let mut instance = factory.create(device, SAMPLE_RATE, BLOCK).map_err(|error| error.to_string())?;
    let params = instance.params.clone();
    let controller = instance.controller.as_mut().ok_or("the plugin has no controller")?;
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

    if let Some(folder) = &options.state {
        std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
        match controller.save_state() {
            Some(state) => {
                let file = folder.join("state.bin");
                std::fs::write(&file, &state).map_err(|error| error.to_string())?;
                println!("state: {} bytes, written to {}", state.len(), file.display());
            }
            None => println!("state: the plugin gives none"),
        }
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
