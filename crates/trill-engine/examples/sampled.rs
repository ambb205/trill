//! Looks at sampled instruments with the program's own reader and player:
//! what a file says, what loading one costs, how well it was recorded,
//! and what it sounds like on a phrase.
//!
//! ```text
//! cargo run -p trill-engine --example sampled -- survey <folder>
//! cargo run -p trill-engine --example sampled -- about <file.sfz>
//! cargo run -p trill-engine --example sampled -- load <file.sfz>
//! cargo run -p trill-engine --example sampled -- measure <file.sfz>
//! cargo run -p trill-engine --example sampled -- play <file.sfz> <piano|keys|strings|bass|drums> <out.wav>
//! cargo run -p trill-engine --example sampled -- facts <folder> <out.jsonl>
//! cargo run -p trill-engine --example sampled -- tour <list.txt> <out.wav>
//! ```
//!
//! * `survey` reads every `.sfz` under a folder and says of each what it
//!   is, then counts what the files ask for that the player does not do.
//! * `about` says what one instrument is, as a model is told.
//! * `load` measures what holding one costs: the time until every note
//!   can begin (the start of every recording in memory), what that holds,
//!   and the time and memory of reading every recording whole.
//! * `measure` reads the recordings themselves: how far each is from its
//!   key's pitch, how even their levels are from key to key, how much
//!   noise they carry.
//! * `play` renders a phrase that suits the kind of instrument to a WAV
//!   file, for listening. Several files joined by `|` are played together,
//!   each the notes of the phrase that lie in its range: the sections of
//!   a string orchestra.
//! * `facts` writes a line of JSON for every instrument under a folder:
//!   what its file says, the files it is made of, what it asks for that
//!   the player does not do, what `measure` finds, and how loud a few
//!   notes of it come out of the player. It is what an entry of the
//!   catalogue (`catalogue/README.md`) is written from.
//! * `tour` plays many instruments one after the other into one file,
//!   each a short phrase, and writes the times beside it (`<out>.txt`):
//!   for hearing a kind of instrument side by side. A line of the list
//!   is `<phrase><tab><file.sfz><tab><what to call it>`, the phrases
//!   being `short-keys`, `short-line`, `short-chords`, `short-bass`,
//!   `short-drums` and `each` (every key it has, for what is struck).
//!
//! Nothing is written but the file `play` names, and nothing of the user's
//! is read: point it at folders of your own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use trill_core::controllers::ControllerKind;
use trill_core::device::{DeviceFactory, Purpose};
use trill_core::project::AutomationPoint;
use trill_core::{ChainRef, Note, Project};
use trill_dsp::Builtins;
use trill_dsp::sampled::{self, Loaded, State};
use trill_engine::ready::Vitals;
use trill_engine::render::RenderOptions;
use trill_engine::{AudioBackend, Session};
use trill_media::{MediaCache, WavFormat};

const SR: f64 = 48_000.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let done = match words.as_slice() {
        ["survey", folder] => survey(Path::new(folder)),
        ["about", file] => about(file),
        ["load", file] => load(file),
        ["measure", file] => measure(file),
        ["play", file, phrase, out] => play(file, phrase, Path::new(out)),
        ["facts", folder, out] => facts(Path::new(folder), Path::new(out)),
        ["tour", list, out] => tour(Path::new(list), Path::new(out)),
        _ => Err("usage: sampled survey <folder> | about <file> | load <file> | measure <file> | play <file> \
                  <piano|keys|strings|bass|drums> <out.wav> | facts <folder> <out.jsonl> | tour <list.txt> <out.wav>"
            .to_string()),
    };
    if let Err(problem) = done {
        eprintln!("{problem}");
        std::process::exit(1);
    }
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn memory() -> u64 {
    Vitals::now().map_or(0, |vitals| vitals.in_use)
}

fn text(path: &Path) -> Result<String, String> {
    std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).map_err(|error| error.to_string())
}

fn files_under(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(listing) = std::fs::read_dir(folder) else { return };
    let mut items: Vec<PathBuf> = listing.filter_map(|item| item.ok().map(|item| item.path())).collect();
    items.sort();
    for item in items {
        if item.is_dir() {
            files_under(&item, found);
        } else if trill_dsp::sfz::is_sfz(&item) {
            found.push(item);
        }
    }
}

/// A name without the numbers in it, so that `locc64` and `locc1` are
/// counted as one thing.
fn without_numbers(name: &str) -> String {
    let mut out = String::new();
    let mut last_digit = false;
    for c in name.chars() {
        if c.is_ascii_digit() {
            if !last_digit {
                out.push('N');
            }
            last_digit = true;
        } else {
            out.push(c);
            last_digit = false;
        }
    }
    out
}

fn survey(folder: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    files_under(folder, &mut files);
    if files.is_empty() {
        return Err(format!("there are no .sfz files under '{}'", folder.display()));
    }
    let begun = Instant::now();
    let (mut read, mut failed) = (0, Vec::new());
    // What is asked for and not done: by name, in how many files, how often, and what is missed.
    let mut asked: BTreeMap<String, (usize, usize, &'static str)> = BTreeMap::new();
    let mut whole = 0;
    println!(
        "{:<52} {:>7} {:>6} {:>9} {:>6} {:>5} {:>5}  not done",
        "instrument", "regions", "files", "range", "layers", "takes", "ways"
    );
    for file in &files {
        match trill_dsp::sfz::read(file, &text) {
            Ok(instrument) => {
                read += 1;
                let summary = instrument.summary();
                let range = summary.range.map_or("-".to_string(), |(low, high)| format!("{low}..{high}"));
                let kind = if summary.drums { " drums" } else { "" };
                let name: String = summary.name.chars().take(50).collect();
                println!(
                    "{name:<52} {:>7} {:>6} {range:>9} {:>6} {:>5} {:>5}  {}{kind}",
                    summary.regions,
                    summary.recordings,
                    summary.layers,
                    summary.takes,
                    summary.switches.len(),
                    instrument.unsupported.len(),
                );
                if instrument.unsupported.is_empty() {
                    whole += 1;
                }
                let mut seen: Vec<String> = Vec::new();
                for un in &instrument.unsupported {
                    let name = without_numbers(&un.name);
                    let entry = asked.entry(name.clone()).or_insert((0, 0, un.miss));
                    if !seen.contains(&name) {
                        entry.0 += 1;
                        seen.push(name);
                    }
                    entry.1 += un.count;
                }
            }
            Err(reason) => failed.push(format!("{}: {reason}", file.display())),
        }
    }
    println!();
    println!(
        "{read} of {} files read in {:.2} s; {whole} ask for nothing that is not done",
        files.len(),
        begun.elapsed().as_secs_f64()
    );
    for failure in &failed {
        println!("  not read: {failure}");
    }
    let mut by_miss: BTreeMap<&str, Vec<(String, usize, usize)>> = BTreeMap::new();
    for (name, (in_files, times, miss)) in asked {
        by_miss.entry(miss).or_default().push((name, in_files, times));
    }
    println!();
    println!("asked for and not done, by what a listener misses (name: files, times):");
    for (miss, mut names) in by_miss {
        names.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        println!("  {miss}");
        for (name, in_files, times) in names {
            println!("    {name}: {in_files}, {times}");
        }
    }
    Ok(())
}

fn factory() -> Builtins {
    Builtins::with_loader(Arc::new(MediaCache::default()))
}

fn loaded(factory: &Builtins, file: &str) -> Result<Arc<Loaded>, String> {
    let state = State::new(file).to_bytes();
    let source = factory.instruments().ok_or("no instruments")?;
    source.load(Some(&state))?.ok_or_else(|| "it names no instrument".to_string())
}

fn about(file: &str) -> Result<(), String> {
    let factory = factory();
    let loaded = loaded(&factory, file)?;
    println!("{}", loaded.about());
    println!();
    println!(
        "on disk: {:.1} MB in {} recordings, {} missing",
        megabytes(loaded.bytes),
        loaded.recordings.len(),
        loaded.missing.len()
    );
    for warning in loaded.warnings() {
        println!("warning: {}", warning.message);
    }
    let profile = loaded.profile();
    println!(
        "profile: range {:?}, role {:?}, bend {:?}, articulations {:?}",
        profile.range,
        profile.role,
        profile.bend_range,
        profile.articulation_names()
    );
    if let Some(sounds) = loaded.key_sounds() {
        let named: Vec<String> =
            sounds.iter().map(|sound| format!("{}: {} {}", sound.key, sound.place, sound.stroke)).collect();
        println!("keys: {}", named.join("; "));
    }
    Ok(())
}

fn load(file: &str) -> Result<(), String> {
    let before = memory();
    let factory = factory();
    let begun = Instant::now();
    let loaded = loaded(&factory, file)?;
    let read_file = begun.elapsed();
    println!("{}", loaded.summary.name);
    println!(
        "  on disk                  {:>9.1} MB in {} recordings",
        megabytes(loaded.bytes),
        loaded.recordings.len()
    );
    println!(
        "  its file read            {:>9.1} ms ({} regions; every recording looked for)",
        read_file.as_secs_f64() * 1e3,
        loaded.summary.regions
    );

    // As a device that plays live has it: the start of every recording.
    let device = trill_core::Device { state: Some(State::new(file).to_bytes()), ..model() };
    let begun = Instant::now();
    let instance = factory.create_for(&device, SR, 512, Purpose::Playing).map_err(|error| error.to_string())?;
    let made = begun.elapsed();
    while loaded.settled().0 < loaded.settled().1 {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let starts = begun.elapsed();
    let held = memory().saturating_sub(before);
    let (mut whole, mut longest, mut frames) = (0, 0usize, 0u64);
    for recording in &loaded.recordings {
        if let Some((facts, start)) = recording.start() {
            whole += usize::from(start.is_whole());
            longest = longest.max(facts.frames);
            frames += facts.frames as u64;
        }
    }
    println!("  the device made          {:>9.1} ms", made.as_secs_f64() * 1e3);
    println!("  every note can begin     {:>9.2} s  (the start of every recording read)", starts.as_secs_f64());
    println!(
        "  held then                {:>9.1} MB ({} recordings are short enough to be whole)",
        megabytes(held),
        whole
    );
    println!(
        "  its recordings are       {:>9.1} minutes of sound; the longest {:.1} s",
        frames as f64 / 48_000.0 / 60.0,
        longest as f64 / 48_000.0
    );

    // How long the rest of one recording takes to read: what a note's start has to last for.
    let pool = &factory.instruments().ok_or("no instruments")?.pool;
    pool.set_budget(usize::MAX);
    let mut times: Vec<f64> = Vec::new();
    let begun = Instant::now();
    for recording in &loaded.recordings {
        let one = Instant::now();
        let read = pool.whole_now(recording);
        if read.is_some_and(|audio| audio.frames() > sampled::START_FRAMES * 2) {
            times.push(one.elapsed().as_secs_f64() * 1e3);
        }
    }
    let all = begun.elapsed();
    let everything = memory().saturating_sub(before);
    times.sort_by(f64::total_cmp);
    if !times.is_empty() {
        println!(
            "  the rest of one read in  {:>9.1} ms at the middle, {:.1} ms at the worst (one thread, the files as the system has them)",
            times[times.len() / 2],
            times[times.len() - 1]
        );
    }
    println!("  every recording whole    {:>9.2} s on one thread", all.as_secs_f64());
    println!(
        "  held then                {:>9.1} MB (the pool counts {:.1} MB of whole recordings)",
        megabytes(everything),
        megabytes(pool.held().whole_bytes as u64)
    );
    drop(instance);
    Ok(())
}

fn model() -> trill_core::Device {
    let mut project = Project::new("measure");
    let id = project.add_device(ChainRef::Master, sampled::URI, "Sampled", None).expect("a device");
    project.device(id).expect("just added").clone()
}

/// The pitch of `samples` near `expected` Hz, by how well the sound
/// matches itself some whole periods later: within 50 cents either side.
/// (Several periods, so that a high note, whose period is a dozen frames,
/// is told as finely as a low one.)
fn pitch_near(samples: &[f32], sample_rate: f64, expected: f64) -> Option<f64> {
    let periods = (240.0 * expected / sample_rate).ceil().max(1.0);
    let period = periods * sample_rate / expected;
    let (low, high) = ((period / 1.029).floor() as usize, (period * 1.029).ceil() as usize + 1);
    let window = samples.len().checked_sub(high + 2)?;
    if window < high * 3 || low < 2 {
        return None;
    }
    let difference = |lag: usize| -> f64 {
        samples[..window].iter().zip(&samples[lag..lag + window]).map(|(a, b)| f64::from(a - b).powi(2)).sum()
    };
    let scores: Vec<f64> = (low - 1..=high + 1).map(difference).collect();
    let best = (1..scores.len() - 1).min_by(|a, b| scores[*a].total_cmp(&scores[*b]))?;
    // The bottom of the dip, between the lags that were tried.
    let (before, at, after) = (scores[best - 1], scores[best], scores[best + 1]);
    let bend = before - 2.0 * at + after;
    let shift = if bend > 0.0 { 0.5 * (before - after) / bend } else { 0.0 };
    // At either end of what was tried there is no dip to speak of.
    (best > 1 && best < scores.len() - 2).then(|| periods * sample_rate / ((low - 1 + best) as f64 + shift))
}

fn peak_db(samples: &[f32]) -> f64 {
    20.0 * f64::from(samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs())).max(1e-9)).log10()
}

fn rms_db(samples: &[f32]) -> f64 {
    let power = samples.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>() / samples.len().max(1) as f64;
    10.0 * power.max(1e-20).log10()
}

/// What reading an instrument's recordings found.
#[derive(Default)]
struct Measured {
    /// How many recordings were read, and how many keys have one of their own.
    read: usize,
    keys: usize,
    /// How far under its peak the quietest twentieth of a second of each
    /// recording lies, in dB, the quietest first.
    noise: Vec<f64>,
    /// By recorded key: how far off equal temperament it is, in cents.
    cents: Vec<(u8, f64)>,
    /// By recorded key: its peak as the instrument levels it, in dB.
    levels: Vec<(u8, f64)>,
}

impl Measured {
    /// The mean, the spread (one deviation) and the worst, with its key.
    fn tuning(&self) -> Option<(f64, f64, f64, u8)> {
        let cents = &self.cents;
        if cents.is_empty() {
            return None;
        }
        let mean = cents.iter().map(|(_, off)| off).sum::<f64>() / cents.len() as f64;
        let spread = (cents.iter().map(|(_, off)| (off - mean).powi(2)).sum::<f64>() / cents.len() as f64).sqrt();
        let worst = cents.iter().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).copied().unwrap_or((0, 0.0));
        Some((mean, spread, worst.1, worst.0))
    }

    /// How the peak steps from one recorded key to the next: root mean
    /// square, and the most.
    fn evenness(&self) -> Option<(f64, f64)> {
        if self.levels.len() <= 2 {
            return None;
        }
        let steps: Vec<f64> = self.levels.windows(2).map(|pair| pair[1].1 - pair[0].1).collect();
        let worst = steps.iter().fold(0.0f64, |worst, step| worst.max(step.abs()));
        let spread = (steps.iter().map(|step| step * step).sum::<f64>() / steps.len() as f64).sqrt();
        Some((spread, worst))
    }

    /// The noise at the middle recording, and of the noisiest tenth.
    fn noise(&self) -> Option<(f64, f64)> {
        (!self.noise.is_empty()).then(|| (-self.noise[self.noise.len() / 2], -self.noise[self.noise.len() * 9 / 10]))
    }
}

/// Reads an instrument's recordings: every one for its noise, or with
/// `sampled` at most that many of them from all over it; and the loudest
/// of each recorded key for its pitch and its level.
fn measured(factory: &Builtins, loaded: &Loaded, sampled: Option<usize>) -> Result<Measured, String> {
    let pool = &factory.instruments().ok_or("no instruments")?.pool;
    let regions = &loaded.instrument.regions;
    // The loudest layer of every root key, among the regions a key going down sounds.
    let mut loudest: BTreeMap<u8, usize> = BTreeMap::new();
    for (index, region) in regions.iter().enumerate() {
        if !region.on_key_down() || region.recording.is_none() || region.pitch_by_key == 0.0 {
            continue;
        }
        let better = loudest.get(&region.root).is_none_or(|known| regions[*known].velocities.1 < region.velocities.1);
        if better {
            loudest.insert(region.root, index);
        }
    }
    // By key: how far off its pitch a recording is, and its peak.
    let mut cents: Vec<(u8, f64)> = Vec::new();
    let mut levels: Vec<(u8, f64)> = Vec::new();
    let mut noise: Vec<f64> = Vec::new();
    let mut measured = 0;
    let every = sampled.map_or(1, |most| loaded.recordings.len().div_ceil(most.max(1)).max(1));
    for recording in loaded.recordings.iter().step_by(every) {
        let Some(audio) = pool.whole_now(recording) else { continue };
        let (left, _) = audio.channels();
        let samples = &left[sampled::pool::GUARD..sampled::pool::GUARD + audio.frames()];
        // The quietest twentieth of a second in it: what it carries when nothing sounds.
        // (Of recordings a second long or more: a short one is never quiet.)
        let window = 2_400;
        let quietest =
            samples.chunks(window).filter(|chunk| chunk.len() == window).map(rms_db).fold(f64::MAX, f64::min);
        if quietest < f64::MAX && samples.len() >= 48_000 {
            noise.push(quietest - peak_db(samples));
        }
        measured += 1;
    }
    for (root, index) in &loudest {
        let region = &regions[*index];
        let recording = &loaded.recordings[region.recording.unwrap_or(0) as usize];
        let (Some(audio), Some((facts, _))) = (pool.whole_now(recording), recording.start()) else { continue };
        let (left, _) = audio.channels();
        let samples = &left[sampled::pool::GUARD..sampled::pool::GUARD + audio.frames()];
        let rate = facts.sample_rate;
        let from = ((0.15 * rate) as usize + region.offset as usize).min(samples.len());
        let to = (from + (0.6 * rate) as usize).min(samples.len());
        if to - from < (0.2 * rate) as usize {
            continue;
        }
        let expected =
            440.0 * 2f64.powf((f64::from(*root) - 69.0) / 12.0) / 2f64.powf(f64::from(region.tune) / 1_200.0);
        // Between C2 and C7: outside that a piano is tuned wide on purpose.
        if (36..=96).contains(root)
            && let Some(heard) = pitch_near(&samples[from..to], rate, expected)
        {
            cents.push((*root, 1_200.0 * (heard / expected).log2()));
        }
        // How hard it speaks: its peak, as the instrument levels it.
        levels.push((*root, peak_db(&samples[..to]) + f64::from(region.volume)));
    }
    noise.sort_by(f64::total_cmp);
    Ok(Measured { read: measured, keys: loudest.len(), noise, cents, levels })
}

fn measure(file: &str) -> Result<(), String> {
    let factory = factory();
    let loaded = loaded(&factory, file)?;
    let found = measured(&factory, &loaded, None)?;
    println!("{}: {} recordings read, {} keys recorded at their loudest", loaded.summary.name, found.read, found.keys);
    if let Some((middle, noisiest)) = found.noise() {
        println!(
            "  noise: the quietest twentieth of a second of a recording lies {middle:.0} dB under its peak at the middle, {noisiest:.0} dB for the noisiest tenth"
        );
    }
    match found.tuning() {
        Some((mean, spread, worst, key)) => println!(
            "  tuning (C2 to C7, {} keys): {mean:+.1} cents from equal temperament on average, {spread:.1} cents of spread (one deviation), the worst {worst:+.1} on key {key}",
            found.cents.len(),
        ),
        None => println!("  tuning: not measured (no recordings that follow the keys, or none long enough)"),
    }
    if let Some((spread, worst)) = found.evenness() {
        println!(
            "  evenness: from one recorded key to the next the peak steps by {spread:.1} dB (root mean square), {worst:.1} dB at the most"
        );
    }
    Ok(())
}

/// What an instrument was heard to do when the player played it.
struct Heard {
    /// The peak of all of it at the device's own volume, in dB.
    peak_db: f64,
    seconds: f64,
    /// What the session said was wrong.
    problems: Vec<String>,
    /// How many keys were played one after the other.
    keys: usize,
    /// How the peak steps from one of those keys to the next, in dB: the
    /// root mean square and the most. `None` for a kit, whose keys are
    /// different drums.
    steps: Option<(f64, f64)>,
    /// The peak of each of those keys, in dB.
    levels: Vec<(u8, f64)>,
}

/// The instrument in `file` through the player, as a render makes it: the
/// keys it has recordings of, one after the other (at most forty of them,
/// from all over it), and a chord. A kit is hit on every key it has.
/// `woken` are controllers to hold at their middle: for an instrument that
/// says nothing until one of them is moved.
fn heard(file: &str, woken: &[u8]) -> Result<Heard, String> {
    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: SR as u32 })
        .map_err(|error| error.to_string())?;
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter)).map_err(|error| error.to_string())?;
    // A second to a beat: a note's place in the song is its place in seconds.
    session.edit(|project| project.set_tempo(60.0)).map_err(|error| error.to_string())?;
    let track = session.edit(|project| Ok(project.add_track("Heard"))).map_err(|error| error.to_string())?;
    let device = session
        .add_device(ChainRef::Track(track), sampled::URI, Some(file), None)
        .map_err(|error| error.to_string())?;
    let said = session.sampled(device).ok_or("the instrument did not load")?;
    let drums = session.plays_drums(track);
    let sounding = || said.regions.iter().filter(|region| !region.release);
    let mut keys: Vec<u8> = if drums {
        sounding().map(|region| region.keys.0).collect()
    } else {
        // The key each recording was made on, where it plays that key.
        sounding().map(|region| region.root.clamp(region.keys.0, region.keys.1)).collect()
    };
    keys.sort_unstable();
    keys.dedup();
    if keys.is_empty() {
        return Err("no key of it plays anything".to_string());
    }
    let every = keys.len().div_ceil(40).max(1);
    let keys: Vec<u8> = keys.into_iter().step_by(every).collect();
    let (low, high) = (keys[0], keys[keys.len() - 1]);
    let mut notes: Vec<Note> = keys
        .iter()
        .enumerate()
        .map(|(step, key)| note(*key, step as f64, if drums { 0.25 } else { 0.8 }, 100))
        .collect();
    let after = keys.len() as f64;
    if !drums {
        let middle = low + (high - low) / 2;
        notes.extend([middle, (middle + 4).min(high), (middle + 7).min(high)].map(|key| note(key, after, 1.8, 100)));
    }
    let beats = after + 2.0;
    session
        .edit(|project| {
            let clip = project.add_midi_clip(track, "Heard", 0.0, beats)?;
            project.clip_mut(clip)?.add_notes(&notes)?;
            // An instrument that is silent until its wheel or its expression is up.
            for kind in [ControllerKind::MOD_WHEEL, ControllerKind::EXPRESSION] {
                let (_, top) = kind.range();
                project.clip_mut(clip)?.set_controller(kind, vec![AutomationPoint::new(0.0, top * 0.8)])?;
            }
            for kind in woken.iter().filter_map(|number| ControllerKind::numbered(*number)) {
                let (_, top) = kind.range();
                project.clip_mut(clip)?.set_controller(kind, vec![AutomationPoint::new(0.0, top * 0.5)])?;
            }
            Ok(())
        })
        .map_err(|error| error.to_string())?;
    let problems: Vec<String> = session.problems().iter().map(ToString::to_string).collect();
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: 2.0, ..RenderOptions::default() };
    let rendered = session.render_job().render(&options).map_err(|error| error.to_string())?;
    if rendered.left.iter().chain(&rendered.right).any(|sample| !sample.is_finite()) {
        return Err("it came out of the player as something that is not a number".to_string());
    }
    let peak_of = |from: usize, to: usize| -> f64 {
        let (from, to) = (from.min(rendered.left.len()), to.min(rendered.left.len()));
        let all = rendered.left[from..to].iter().chain(&rendered.right[from..to]);
        20.0 * f64::from(all.fold(0.0f32, |peak, sample| peak.max(sample.abs())).max(1e-9)).log10()
    };
    let second = SR as usize;
    let levels: Vec<f64> = (0..keys.len()).map(|step| peak_of(step * second, (step + 1) * second)).collect();
    let steps = (!drums && levels.len() > 2).then(|| {
        let steps: Vec<f64> = levels.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let worst = steps.iter().fold(0.0f64, |worst, step| worst.max(step.abs()));
        ((steps.iter().map(|step| step * step).sum::<f64>() / steps.len() as f64).sqrt(), worst)
    });
    Ok(Heard {
        peak_db: peak_of(0, rendered.left.len()),
        seconds: rendered.left.len() as f64 / SR,
        problems,
        keys: keys.len(),
        steps,
        levels: keys.iter().copied().zip(levels.iter().copied()).collect(),
    })
}

/// A line of JSON for every instrument under `folder`; see the notes at
/// the top.
fn facts(folder: &Path, out: &Path) -> Result<(), String> {
    use serde_json::json;
    use std::io::Write;
    // Whole paths: an instrument is given to a device by the whole path of its file.
    let folder = &std::path::absolute(folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let mut files = Vec::new();
    files_under(folder, &mut files);
    if files.is_empty() {
        return Err(format!("there are no .sfz files under '{}'", folder.display()));
    }
    let said = |error: std::io::Error| format!("{}: {error}", out.display());
    let mut written = std::io::BufWriter::new(std::fs::File::create(out).map_err(said)?);
    let below = |path: &Path| path.strip_prefix(folder).unwrap_or(path).to_string_lossy().replace('\\', "/");
    let (mut instruments, mut parts, mut unreadable) = (0, 0, 0);
    for file in &files {
        let name = file.to_string_lossy().into_owned();
        // A pool of its own for each: what one read is let go of before the next.
        let factory = factory();
        let loaded = match loaded(&factory, &name) {
            Ok(loaded) => loaded,
            Err(reason) => {
                unreadable += 1;
                let line = json!({ "file": below(file), "unreadable": reason });
                writeln!(written, "{line}").map_err(said)?;
                continue;
            }
        };
        // A file that others include is not an instrument.
        if loaded.instrument.regions.is_empty() {
            parts += 1;
            continue;
        }
        instruments += 1;
        let summary = &loaded.summary;
        let found = measured(&factory, &loaded, Some(160))?;
        let unsupported: Vec<_> = loaded
            .instrument
            .unsupported
            .iter()
            .map(|un| json!({ "name": un.name, "count": un.count, "miss": un.miss }))
            .collect();
        let missing: Vec<String> =
            loaded.missing.iter().map(|index| below(Path::new(&loaded.paths[*index as usize]))).collect();
        let played = heard(&name, &[]);
        // An instrument that says nothing as it loads may be waiting for a
        // controller: its own, all at their middle.
        let silent = played.as_ref().is_ok_and(|heard| heard.peak_db < -60.0);
        let own: Vec<u8> =
            summary.controllers.iter().map(|(number, _)| *number).filter(|number| *number != 64).collect();
        let woken = (silent && !own.is_empty()).then(|| heard(&name, &own).map(|heard| heard.peak_db).ok()).flatten();
        let line = json!({
            "file": below(file),
            "name": summary.name,
            "regions": summary.regions,
            "recordings": loaded.paths.iter().map(|path| below(Path::new(path))).collect::<Vec<_>>(),
            "texts": loaded.text_files().iter().map(|path| below(path)).collect::<Vec<_>>(),
            "missing": missing,
            "bytes": loaded.bytes,
            "range": summary.range,
            "drums": summary.drums,
            "layers": summary.layers,
            "takes": summary.takes,
            "ways": summary.switches.iter().map(|(_, name)| name).collect::<Vec<_>>(),
            "controllers": summary.controllers,
            "releases": summary.releases,
            "loops": summary.loops,
            "unsupported": unsupported,
            "says": loaded.unsupported(),
            "problems": loaded.instrument.problems,
            "noise": found.noise().map(|(middle, noisiest)| [middle, noisiest]),
            "tuning": found.tuning().map(|(mean, spread, worst, key)| json!({ "keys": found.cents.len(), "mean": mean, "spread": spread, "worst": worst, "worst_key": key })),
            "cents": found.cents,
            "unreadable": loaded.unreadable_why(),
            "evenness": found.evenness().map(|(spread, worst)| [spread, worst]),
            "heard": match &played {
                Ok(heard) => json!({
                    "peak_db": heard.peak_db,
                    "seconds": heard.seconds,
                    "problems": heard.problems,
                    "keys": heard.keys,
                    "steps": heard.steps.map(|(spread, worst)| [spread, worst]),
                    "levels": heard.levels,
                    "woken": woken,
                }),
                Err(reason) => json!({ "failed": reason }),
            },
        });
        writeln!(written, "{line}").map_err(said)?;
        written.flush().map_err(said)?;
    }
    println!(
        "{instruments} instruments, {parts} files that are parts of others, {unreadable} that could not be read: {}",
        out.display()
    );
    Ok(())
}

fn note(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
    Note { pitch, start, length, velocity }
}

/// Notes of a phrase that suits `kind`, at 96 BPM, with the controller
/// curves it wants, and its length in beats.
#[allow(clippy::type_complexity)]
fn phrase(
    kind: &str,
    session: &Session,
    track: trill_core::TrackId,
) -> Result<(Vec<Note>, Vec<(ControllerKind, Vec<(f64, f64)>)>, f64), String> {
    let chords: [[u8; 4]; 4] = [[57, 60, 64, 67], [53, 57, 60, 64], [48, 55, 60, 64], [55, 59, 62, 65]];
    let mut notes = Vec::new();
    let mut curves = Vec::new();
    match kind {
        "piano" => {
            // Am7, Fmaj7, C, G7: broken chords over a bass note, soft to loud, held by the pedal; then a run and a full chord.
            for (bar, chord) in chords.iter().enumerate() {
                let at = bar as f64 * 4.0;
                let loud = [45u8, 62, 84, 104][bar];
                notes.push(note(chord[0] - 24, at, 3.8, loud));
                for step in 0..8 {
                    let key = chord[[0, 1, 2, 3, 2, 1, 2, 3][step]] + if step > 3 { 12 } else { 0 };
                    notes.push(note(key, at + step as f64 * 0.5, 0.45, loud + (step as u8 % 3) * 4));
                }
            }
            for step in 0..16u8 {
                notes.push(note(
                    60 + [0u8, 2, 4, 5, 7, 9, 11, 12, 14, 16, 17, 19, 21, 23, 24, 26][step as usize],
                    16.0 + f64::from(step) * 0.25,
                    0.22,
                    70 + step * 3,
                ));
            }
            for key in [36u8, 48, 60, 64, 67, 72, 84] {
                notes.push(note(key, 20.0, 6.0, 118));
            }
            let pedal: Vec<(f64, f64)> = (0..4)
                .flat_map(|bar| [(bar as f64 * 4.0 + 0.1, 127.0), (bar as f64 * 4.0 + 3.8, 0.0)])
                .chain([(20.0, 127.0), (27.0, 0.0)])
                .collect();
            curves.push((ControllerKind::SUSTAIN, pedal));
            Ok((notes, curves, 28.0))
        }
        "keys" => {
            // The same changes as an electric piano is played: chords on the off-beats, a line on top.
            for (bar, chord) in chords.iter().enumerate() {
                let at = bar as f64 * 4.0;
                notes.push(note(chord[0] - 12, at, 1.9, 96));
                notes.push(note(chord[0] - 12, at + 2.5, 1.2, 84));
                for hit in [0.5, 1.5, 3.0] {
                    for key in chord {
                        notes.push(note(*key, at + hit, 0.8, 70 + (bar as u8) * 9));
                    }
                }
                for (step, up) in [12u8, 14, 16, 19, 16, 14].iter().enumerate() {
                    notes.push(note(chord[1] + up, at + step as f64 * 0.5 + 0.25, 0.4, 88 + (step as u8 % 2) * 20));
                }
            }
            for key in [45u8, 57, 60, 64, 67, 72] {
                notes.push(note(key, 16.0, 6.0, 60));
            }
            Ok((notes, curves, 24.0))
        }
        "strings" => {
            // Held chords that swell and fall, then a line in octaves.
            for (bar, chord) in chords.iter().enumerate() {
                for key in chord {
                    notes.push(note(*key, bar as f64 * 4.0, 3.9, 80));
                }
                notes.push(note(chord[0] - 12, bar as f64 * 4.0, 3.9, 80));
            }
            for (step, key) in [69u8, 72, 76, 79, 77, 76, 72, 74].iter().enumerate() {
                notes.push(note(*key, 16.0 + step as f64, 0.95, 96));
                notes.push(note(key - 12, 16.0 + step as f64, 0.95, 96));
            }
            let swell: Vec<(f64, f64)> = (0..6)
                .flat_map(|bar| {
                    [(bar as f64 * 4.0, 30.0), (bar as f64 * 4.0 + 2.2, 120.0), (bar as f64 * 4.0 + 3.9, 45.0)]
                })
                .collect();
            curves.push((ControllerKind::MOD_WHEEL, swell.clone()));
            curves.push((
                ControllerKind::EXPRESSION,
                swell.into_iter().map(|(beat, value)| (beat, 60.0 + value * 0.5)).collect(),
            ));
            Ok((notes, curves, 26.0))
        }
        "bass" => {
            // A walking line over the changes, two bars each, with a few short notes.
            let roots = [45u8, 41, 36, 43];
            for (bar, root) in roots.iter().enumerate() {
                for (step, up) in [0u8, 4, 7, 9, 12, 9, 7, 5].iter().enumerate() {
                    let short = step % 4 == 3;
                    notes.push(note(
                        root + up - 12,
                        bar as f64 * 8.0 + step as f64,
                        if short { 0.4 } else { 0.92 },
                        78 + (step as u8 % 4) * 9,
                    ));
                }
            }
            notes.push(note(33, 32.0, 4.0, 110));
            Ok((notes, curves, 37.0))
        }
        "drums" => {
            // Four bars of a groove with a fill and a crash, by the names this kit has its drums under.
            let profile = session.profile_of(track);
            let key = |names: &[&str]| names.iter().find_map(|name| profile.drum(name).map(|drum| drum.pitch));
            let kick = key(&["kick"]).ok_or("the kit has no kick")?;
            let snare = key(&["snare", "rimshot"]).ok_or("the kit has no snare")?;
            let hat = key(&["closed_hat", "ride", "hat"]);
            let open = key(&["open_hat"]);
            let crash = key(&["crash", "china", "splash"]);
            let toms: Vec<u8> =
                ["high_tom", "mid_tom", "low_tom", "floor_tom"].iter().filter_map(|name| key(&[name])).collect();
            for bar in 0..4 {
                let at = bar as f64 * 4.0;
                for beat in [0.0, 1.5, 2.0] {
                    notes.push(note(kick, at + beat, 0.2, 112));
                }
                notes.push(note(kick, at + 3.25, 0.2, 90));
                for beat in [1.0, 3.0] {
                    notes.push(note(snare, at + beat, 0.2, 110));
                }
                notes.push(note(snare, at + 1.75, 0.1, 42));
                notes.push(note(snare, at + 2.75, 0.1, 50));
                if let Some(hat) = hat {
                    for step in 0..8 {
                        let last = bar == 3 && step >= 6;
                        if !last {
                            notes.push(note(hat, at + step as f64 * 0.5, 0.2, if step % 2 == 0 { 96 } else { 62 }));
                        }
                    }
                }
                if let (Some(open), true) = (open, bar % 2 == 1) {
                    notes.push(note(open, at + 3.5, 0.4, 92));
                }
            }
            if let Some(crash) = crash {
                notes.push(note(crash, 0.0, 1.0, 105));
                notes.push(note(crash, 16.0, 2.0, 118));
            }
            for (step, tom) in toms.iter().cycle().take(4).enumerate() {
                notes.push(note(*tom, 15.0 + step as f64 * 0.25, 0.2, 90 + step as u8 * 8));
            }
            notes.push(note(kick, 16.0, 0.3, 120));
            Ok((notes, curves, 20.0))
        }
        "short-keys" => {
            // Two bars of broken chords, soft then loud, and a chord left to ring.
            for (bar, chord) in chords.iter().take(2).enumerate() {
                let at = bar as f64 * 4.0;
                let loud = [64u8, 100][bar];
                notes.push(note(chord[0] - 12, at, 3.8, loud));
                for step in 0..8 {
                    let key = chord[[0, 1, 2, 3, 2, 1, 2, 3][step]] + if step > 3 { 12 } else { 0 };
                    notes.push(note(key, at + step as f64 * 0.5, 0.45, loud + (step as u8 % 3) * 4));
                }
            }
            for key in [48u8, 60, 64, 67, 72] {
                notes.push(note(key, 8.0, 4.0, 96));
            }
            Ok((notes, curves, 12.0))
        }
        "short-line" | "short-chords" => {
            if kind == "short-chords" {
                // Two held chords, then the line in octaves.
                for (bar, chord) in chords.iter().take(2).enumerate() {
                    for key in chord {
                        notes.push(note(*key, bar as f64 * 4.0, 3.9, 84));
                    }
                    notes.push(note(chord[0] - 12, bar as f64 * 4.0, 3.9, 84));
                }
                for (step, key) in [69u8, 72, 76, 79].iter().enumerate() {
                    notes.push(note(*key, 8.0 + step as f64, 0.95, 96));
                    notes.push(note(key - 12, 8.0 + step as f64, 0.95, 96));
                }
            } else {
                // A line that rises and falls, each note held, and a last long one.
                for (step, key) in [69u8, 72, 76, 79, 77, 76, 72, 74].iter().enumerate() {
                    notes.push(note(*key, step as f64, 0.95, 92));
                }
                notes.push(note(69, 8.0, 4.0, 100));
            }
            let swell = vec![(0.0, 96.0), (7.9, 84.0), (8.0, 60.0), (10.0, 122.0), (12.0, 70.0)];
            curves.push((ControllerKind::MOD_WHEEL, swell.clone()));
            curves.push((
                ControllerKind::EXPRESSION,
                swell.into_iter().map(|(beat, value)| (beat, 60.0 + value * 0.5)).collect(),
            ));
            Ok((notes, curves, 12.0))
        }
        "short-bass" => {
            // A bar of a walking line, twice, and a long low note.
            for (step, up) in [0u8, 4, 7, 9, 12, 9, 7, 5].iter().enumerate() {
                let short = step % 4 == 3;
                notes.push(note(33 + up, step as f64, if short { 0.4 } else { 0.92 }, 78 + (step as u8 % 4) * 9));
            }
            notes.push(note(33, 8.0, 3.0, 110));
            Ok((notes, curves, 11.0))
        }
        "short-drums" | "each" => {
            let profile = session.profile_of(track);
            let key = |names: &[&str]| names.iter().find_map(|name| profile.drum(name).map(|drum| drum.pitch));
            let (kick, snare) = (key(&["kick"]), key(&["snare", "rimshot"]));
            if let (Some(kick), Some(snare), true) = (kick, snare, kind == "short-drums") {
                // Two bars of a groove, a fill and a crash.
                let hat = key(&["closed_hat", "ride", "hat"]);
                let crash = key(&["crash", "china", "splash"]);
                let toms: Vec<u8> =
                    ["high_tom", "mid_tom", "low_tom", "floor_tom"].iter().filter_map(|name| key(&[name])).collect();
                for bar in 0..2 {
                    let at = bar as f64 * 4.0;
                    for beat in [0.0, 1.5, 2.0] {
                        notes.push(note(kick, at + beat, 0.2, 112));
                    }
                    for beat in [1.0, 3.0] {
                        notes.push(note(snare, at + beat, 0.2, 110));
                    }
                    notes.push(note(snare, at + 1.75, 0.1, 42));
                    if let Some(hat) = hat {
                        for step in 0..8 {
                            if !(bar == 1 && step >= 6) {
                                let loud = if step % 2 == 0 { 96 } else { 62 };
                                notes.push(note(hat, at + step as f64 * 0.5, 0.2, loud));
                            }
                        }
                    }
                }
                for (step, tom) in toms.iter().cycle().take(4).enumerate() {
                    notes.push(note(*tom, 7.0 + step as f64 * 0.25, 0.2, 90 + step as u8 * 8));
                }
                notes.push(note(kick, 8.0, 0.3, 120));
                if let Some(crash) = crash {
                    notes.push(note(crash, 8.0, 2.0, 118));
                }
                return Ok((notes, curves, 11.0));
            }
            // Every key it has a sound on, one after the other: sixteen of them at most, from all over it.
            let device = session.instrument_of(track).map(|device| device.id).ok_or("the track has no instrument")?;
            let said = session.sampled(device).ok_or("the instrument did not load")?;
            let mut keys: Vec<u8> =
                said.regions.iter().filter(|region| !region.release).map(|region| region.keys.0).collect();
            keys.sort_unstable();
            keys.dedup();
            if keys.is_empty() {
                return Err("no key of it plays anything".to_string());
            }
            let every = keys.len().div_ceil(16).max(1);
            let keys: Vec<u8> = keys.into_iter().step_by(every).collect();
            for (step, key) in keys.iter().enumerate() {
                notes.push(note(*key, step as f64 * 0.75, 0.6, 104));
            }
            Ok((notes, curves, keys.len() as f64 * 0.75 + 1.0))
        }
        other => Err(format!(
            "'{other}' is no phrase: piano, keys, strings, bass, drums, or short-keys, short-line, short-chords, \
             short-bass, short-drums, each"
        )),
    }
}

/// Whether a phrase names the keys it means, and is not to be moved to
/// where an instrument's range is.
fn keys_as_written(kind: &str) -> bool {
    matches!(kind, "drums" | "short-drums" | "each")
}

/// A phrase as it was rendered: brought to a peak of -3 dB.
struct Played {
    sound: trill_core::audio::AudioBuffer,
    notes: usize,
    /// Its peak at the device's own volume, in dB.
    peak_db: f64,
    took: std::time::Duration,
}

fn play(file: &str, kind: &str, out: &Path) -> Result<(), String> {
    let played = played(file, kind, 6.0, true)?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    trill_media::write_wav(out, &played.sound, WavFormat::Int24).map_err(|error| error.to_string())?;
    println!(
        "{} notes, {:.1} s of sound rendered in {:.2} s; its peak was {:.1} dB at the device's own volume; written to {}",
        played.notes,
        played.sound.left.len() as f64 / SR,
        played.took.as_secs_f64(),
        played.peak_db,
        out.display()
    );
    Ok(())
}

/// Every instrument of a list, one after the other: see the notes at the
/// top.
fn tour(list: &Path, out: &Path) -> Result<(), String> {
    use std::fmt::Write;
    let lines = text(list)?;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut times = String::new();
    let gap = (0.7 * SR) as usize;
    let mut count = 0;
    for line in lines.lines().filter(|line| !line.trim().is_empty() && !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('\t').collect();
        let [kind, file, name] = fields[..] else {
            return Err(format!("a line of the list is not '<phrase><tab><file><tab><name>': {line}"));
        };
        let at = left.len() as f64 / SR;
        let stamp = format!("{}:{:02}", (at / 60.0) as u32, (at % 60.0) as u32);
        match played(file, kind, 2.5, false) {
            Ok(played) => {
                let _ = writeln!(times, "{stamp}  {name}");
                left.extend_from_slice(&played.sound.left);
                right.extend_from_slice(&played.sound.right);
                left.extend(std::iter::repeat_n(0.0, gap));
                right.extend(std::iter::repeat_n(0.0, gap));
                count += 1;
            }
            // One that cannot be played is said, and the rest are played.
            Err(reason) => {
                let _ = writeln!(times, "  --   {name}: not played ({reason})");
            }
        }
    }
    if left.is_empty() {
        return Err(format!("nothing of '{}' could be played", list.display()));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let seconds = left.len() as f64 / SR;
    let sound = trill_core::audio::AudioBuffer { sample_rate: SR, left, right };
    trill_media::write_wav(out, &sound, WavFormat::Int16).map_err(|error| error.to_string())?;
    let beside = out.with_extension("txt");
    std::fs::write(&beside, &times).map_err(|error| format!("{}: {error}", beside.display()))?;
    println!("{count} instruments, {:.0} s: {} and {}", seconds, out.display(), beside.display());
    Ok(())
}

/// The phrase `kind` on the instrument in `file` (several joined by `|`
/// are played together), with at most `tail` seconds after its last note.
fn played(file: &str, kind: &str, tail: f64, said_aloud: bool) -> Result<Played, String> {
    let media = Arc::new(MediaCache::default());
    let factory = Arc::new(Builtins::with_loader(media.clone()));
    let mut session = Session::with_media(factory, media, AudioBackend::Null { sample_rate: SR as u32 })
        .map_err(|error| error.to_string())?;
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter)).map_err(|error| error.to_string())?;
    session.edit(|project| project.set_tempo(96.0)).map_err(|error| error.to_string())?;
    let files: Vec<&str> = file.split('|').collect();
    let mut written = 0;
    for file in &files {
        let track = session.edit(|project| Ok(project.add_track("Phrase"))).map_err(|error| error.to_string())?;
        let device = session
            .add_device(ChainRef::Track(track), sampled::URI, Some(file), None)
            .map_err(|error| error.to_string())?;
        let said = session.sampled(device).ok_or("the instrument did not load")?;
        if said_aloud {
            println!("{}", said.about);
        }
        let (notes, curves, beats) = phrase(kind, &session, track)?;
        // A bass that is written an octave up, a kit: what the instrument's
        // range says. One instrument takes every note, moved by octaves to
        // where it can play it; of several, each takes the notes it has.
        let range = session.profile_of(track).range;
        let notes: Vec<Note> = notes
            .into_iter()
            .filter_map(|mut note| {
                if let (Some((low, high)), false) = (range, keys_as_written(kind)) {
                    if files.len() > 1 {
                        return (low..=high).contains(&note.pitch).then_some(note);
                    }
                    while note.pitch < low && note.pitch + 12 <= high {
                        note.pitch += 12;
                    }
                    while note.pitch > high && note.pitch >= low + 12 {
                        note.pitch -= 12;
                    }
                }
                Some(note)
            })
            .collect();
        written += notes.len();
        session
            .edit(|project| {
                let clip = project.add_midi_clip(track, "Phrase", 0.0, beats)?;
                project.clip_mut(clip)?.add_notes(&notes)?;
                for (kind, points) in &curves {
                    let (low, high) = kind.range();
                    let points = points
                        .iter()
                        .map(|(beat, value)| AutomationPoint::new(*beat, low + (high - low) * value / 127.0))
                        .collect();
                    project.clip_mut(clip)?.set_controller(*kind, points)?;
                }
                Ok(())
            })
            .map_err(|error| error.to_string())?;
    }
    if said_aloud {
        for problem in session.problems() {
            println!("problem: {problem:?}");
        }
    }
    let begun = Instant::now();
    let options = RenderOptions { sample_rate: SR as u32, max_tail_seconds: tail, ..RenderOptions::default() };
    let rendered = session.render_job().render(&options).map_err(|error| error.to_string())?;
    let took = begun.elapsed();
    let peak = rendered.left.iter().chain(&rendered.right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak < 1e-4 {
        return Err("it came out silent".to_string());
    }
    // Brought to a peak of -3 dB, so that the files are heard at one level.
    let gain = 0.707 / peak;
    let sound = trill_core::audio::AudioBuffer {
        sample_rate: SR,
        left: rendered.left.iter().map(|sample| sample * gain).collect(),
        right: rendered.right.iter().map(|sample| sample * gain).collect(),
    };
    Ok(Played { sound, notes: written, peak_db: 20.0 * f64::from(peak.max(1e-9)).log10(), took })
}
