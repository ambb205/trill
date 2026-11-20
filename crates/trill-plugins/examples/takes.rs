//! Renders one part of a song by itself through its installed instrument,
//! exactly as a render of the song does (the wait before it, the blocks,
//! the stem of the instrument alone), and writes that stem to a folder.
//! For finding out what an instrument's takes depend on: the same part is
//! made twice, each time by this program started anew, with one thing
//! changed, and the two stems are compared from outside.
//!
//! ```text
//! cargo run -p trill-plugins --example takes -- <song.trill> <track name> <folder> [options]
//!
//! --idle <ms>      another instrument beside it takes this long to be ready, so the
//!                  wait before the render goes on that long after this one answered
//! --busy <n>       n threads of this program do nothing but count while it renders
//! --spread <way>   notes of the part that begin on the same beat are given one frame
//!                  apart: `up`, the lowest key first, or `down`, the highest
//! --apart <n>      how many frames apart `--spread` gives them (default 1)
//! --pace <x>       the part is played no faster than x times the speed of the song
//!                  (1 is as it plays live), by a device behind its instrument that
//!                  does nothing but wait
//! --unwaited <s>   no render: the part's devices are made, left alone for s seconds
//!                  and then played as the engine plays a song, with nothing asked of
//!                  them before (no wait, no rehearsal, no reset). What the track
//!                  comes to goes to `unwaited.wav` in the folder
//! --from <beat>    where the render begins (default 0)
//! --to <beat>      where it ends (default: the end of the song)
//! --rate <hz>      the sample rate (default 44100)
//! --threads <n>    how many threads the render may use (default 1)
//! ```
//!
//! The song is read and never written; every other track of it is muted
//! in the copy that is rendered. The caches are this program's own, in the
//! temporary folder. No window is opened, and nothing is heard.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use trill_core::device::{
    CompositeFactory, DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    Event, ProcessInfo, Purpose,
};
use trill_core::{ChainRef, Device, Note, Project};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters};
use trill_engine::render::{RenderOptions, Stems, render};
use trill_engine::scripted::{BASS, Loading, ScriptedFactory};
use trill_engine::{AudioSources, Processor};
use trill_plugins::{Scanner, Vst3Config, Vst3Factory};

/// A device that passes what it is given and, while the song plays, waits
/// until as much time has gone by as the song has, or a part of it.
const PACER: &str = "pacer:no-faster-than-the-song";

struct Pacers {
    speed: f64,
}

struct Pacer {
    rate: f64,
    speed: f64,
    played: u64,
    begun: Option<Instant>,
}

impl DeviceFactory for Pacers {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        vec![DeviceDescriptor {
            uri: PACER.to_string(),
            name: "Pacer".to_string(),
            vendor: "Trill examples".to_string(),
            category: DeviceCategory::Effect,
            description: "Waits, so that a render goes no faster than the song".to_string(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(&self, device: &Device, sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != PACER {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        let pacer = Pacer { rate: sample_rate, speed: self.speed, played: 0, begun: None };
        Ok(DeviceInstance { processor: Box::new(pacer), params: Vec::new(), controller: None })
    }
}

impl DeviceProcessor for Pacer {
    fn prepare(&mut self, sample_rate: f64, _max_block: usize) {
        self.rate = sample_rate;
    }

    fn set_param(&mut self, _index: usize, _value: f64) {}

    fn process(&mut self, info: &ProcessInfo, _events: &[Event], left: &mut [f32], _right: &mut [f32]) {
        if !info.playing {
            return;
        }
        self.played += left.len() as u64;
        let begun = *self.begun.get_or_insert_with(Instant::now);
        let due = begun + Duration::from_secs_f64(self.played as f64 / self.rate / self.speed);
        if let Some(early) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(early);
        }
    }

    fn reset(&mut self) {}
}

struct Options {
    song: PathBuf,
    track: String,
    folder: PathBuf,
    idle: u64,
    busy: usize,
    spread: Option<bool>,
    apart: u32,
    pace: Option<f64>,
    unwaited: Option<f64>,
    from: f64,
    to: Option<f64>,
    rate: u32,
    threads: usize,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let [song, track, folder, rest @ ..] = arguments else {
        return Err("usage: takes <song.trill> <track name> <folder> [--idle <ms>] [--busy <n>] [--from <beat>] \
                    [--to <beat>] [--rate <hz>] [--threads <n>]"
            .to_string());
    };
    let mut options = Options {
        song: song.into(),
        track: track.clone(),
        folder: folder.into(),
        idle: 0,
        busy: 0,
        spread: None,
        apart: 1,
        pace: None,
        unwaited: None,
        from: 0.0,
        to: None,
        rate: 44_100,
        threads: 1,
    };
    let mut rest = rest.iter();
    while let Some(name) = rest.next() {
        let value = rest.next().ok_or(format!("{name} needs a value"))?;
        let number = || value.parse::<f64>().map_err(|_| format!("{name} takes a number, not '{value}'"));
        match name.as_str() {
            "--idle" => options.idle = number()? as u64,
            "--busy" => options.busy = number()? as usize,
            "--unwaited" => options.unwaited = Some(number()?.max(0.0)),
            "--pace" => options.pace = Some(number()?.max(0.01)),
            "--apart" => options.apart = (number()? as u32).max(1),
            "--spread" => {
                options.spread = Some(match value.as_str() {
                    "up" => true,
                    "down" => false,
                    other => return Err(format!("--spread takes `up` or `down`, not '{other}'")),
                })
            }
            "--from" => options.from = number()?,
            "--to" => options.to = Some(number()?),
            "--rate" => options.rate = number()? as u32,
            "--threads" => options.threads = (number()? as usize).max(1),
            other => return Err(format!("there is no option '{other}'")),
        }
    }
    Ok(options)
}

fn plugins() -> Result<Vst3Factory, String> {
    let scratch = std::env::temp_dir().join("trill-takes-example");
    let config = Vst3Config {
        scanner: Scanner::Auto,
        cache_path: Some(scratch.join("vst3").join("scan-cache.json")),
        scan_timeout: Duration::from_secs(60),
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    Vst3Factory::new(config).map_err(|error| error.to_string())
}

fn run(options: &Options) -> Result<(), String> {
    let text =
        std::fs::read_to_string(&options.song).map_err(|error| format!("{}: {error}", options.song.display()))?;
    let mut project = Project::from_json(&text).map_err(|error| error.to_string())?;
    let wanted = project.tracks.iter().find(|track| track.name == options.track).map(|track| track.id);
    let wanted = wanted.ok_or_else(|| {
        let names: Vec<&str> = project.tracks.iter().map(|track| track.name.as_str()).collect();
        format!("the song has no track '{}': it has {}", options.track, names.join(", "))
    })?;
    // The part alone: nothing else of the song is made.
    for track in &mut project.tracks {
        (track.mute, track.solo) = (track.id != wanted, false);
    }
    if let Some(up) = options.spread {
        // One frame, in beats: the song is of one tempo.
        let frame = project.tempo / 60.0 / f64::from(options.rate) * f64::from(options.apart);
        let clips: Vec<_> =
            project.track(wanted).map_err(|error| error.to_string())?.clips.iter().map(|clip| clip.id).collect();
        let mut moved = 0;
        for id in clips {
            let clip = project.clip_mut(id).map_err(|error| error.to_string())?;
            let mut notes = clip.notes().to_vec();
            notes.sort_by(|a, b| {
                let keys = if up { a.pitch.cmp(&b.pitch) } else { b.pitch.cmp(&a.pitch) };
                a.start.total_cmp(&b.start).then(keys)
            });
            let mut run = 0;
            for at in 1..notes.len() {
                run = if (notes[at].start - notes[at - 1].start).abs() < frame / 2.0 + f64::from(run) * frame {
                    run + 1
                } else {
                    0
                };
                if run > 0 {
                    notes[at].start += f64::from(run) * frame;
                    moved += 1;
                }
            }
            if !notes.is_empty() {
                clip.set_notes(notes).map_err(|error| error.to_string())?;
            }
        }
        println!("{moved} notes that began with another were given a frame or more after it");
    }
    if options.pace.is_some() {
        project.add_device(ChainRef::Track(wanted), PACER, "Pacer", None).map_err(|error| error.to_string())?;
    }
    if options.idle > 0 {
        // An instrument that is slow to be ready, which nobody hears: the
        // wait goes on for it after the part's own has answered.
        let slow = project.add_track("Slow to be ready");
        project.add_device(ChainRef::Track(slow), BASS, "Slow", None).map_err(|error| error.to_string())?;
        let clip = project.add_midi_clip(slow, "one note", options.from, 4.0).map_err(|error| error.to_string())?;
        let note = Note { pitch: 40, start: 0.0, length: 0.5, velocity: 100, ..Default::default() };
        project.clip_mut(clip).and_then(|clip| clip.add_notes(&[note])).map_err(|error| error.to_string())?;
        project.track_mut(slow).map_err(|error| error.to_string())?.volume_db = -200.0;
    }
    let slow = Loading { first: Duration::from_millis(options.idle), ..Loading::default() };
    let factory: Arc<dyn DeviceFactory> = Arc::new(CompositeFactory::new(vec![
        Box::new(BuiltinFactory),
        Box::new(ScriptedFactory::loading(slow)),
        Box::new(Pacers { speed: options.pace.unwrap_or(1.0) }),
        Box::new(plugins()?),
    ]));

    if let Some(seconds) = options.unwaited {
        return play_unwaited(&project, factory.as_ref(), options, seconds);
    }

    // Threads that do nothing but keep the machine's processors busy.
    let done = Arc::new(AtomicBool::new(false));
    let busy: Vec<_> = (0..options.busy)
        .map(|_| {
            let done = done.clone();
            std::thread::spawn(move || {
                let mut count = 0u64;
                while !done.load(Ordering::Relaxed) {
                    count = std::hint::black_box(count.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
                }
            })
        })
        .collect();

    let render_options = RenderOptions {
        sample_rate: options.rate,
        start_beat: options.from,
        end_beat: options.to,
        max_tail_seconds: 6.0,
        threads: Some(options.threads),
        stems: Some(Stems { folder: options.folder.clone(), instruments: true, ..Stems::default() }),
        ..RenderOptions::default()
    };
    let begun = Instant::now();
    let rendered = render(&project, factory.as_ref(), &render_options);
    done.store(true, Ordering::Relaxed);
    busy.into_iter().for_each(|thread| drop(thread.join()));
    let rendered = rendered.map_err(|error| error.to_string())?;
    println!(
        "rendered {:.1} s of '{}' in {:.1} s on {} thread(s), into {}",
        rendered.left.len() as f64 / f64::from(rendered.sample_rate),
        options.track,
        begun.elapsed().as_secs_f64(),
        rendered.measured.threads,
        options.folder.display()
    );
    for waited in &rendered.measured.waited {
        println!(
            "  waited {:.1} s for {} on '{}' (answered: {}, silent: {:?})",
            waited.seconds, waited.device_name, waited.track_name, waited.answered, waited.silent
        );
    }
    for problem in &rendered.problems {
        println!("  problem: {problem}");
    }
    Ok(())
}

/// Makes the devices of `project`, leaves them alone for `seconds`, and
/// plays the stretch block by block as the engine plays a song: nothing
/// waits for them, rehearses them or resets them first.
fn play_unwaited(
    project: &Project,
    factory: &dyn DeviceFactory,
    options: &Options,
    seconds: f64,
) -> Result<(), String> {
    let rate = f64::from(options.rate);
    let (mut bank, mut meters, audio) = (DeviceBank::default(), Meters::default(), AudioSources::default());
    let mut compiler = Compiler {
        factory,
        bank: &mut bank,
        meters: &mut meters,
        audio: &audio,
        stand_ins: None,
        sample_rate: rate,
        purpose: Purpose::Rendering,
    };
    let (graph, problems) = compiler.compile(project, true);
    for problem in &problems {
        println!("  problem: {problem}");
    }
    let mut processor = Processor::new(rate);
    drop(processor.swap_graph(graph));
    std::thread::sleep(Duration::from_secs_f64(seconds));
    let to = options.to.ok_or("--unwaited needs --to")?;
    let frames = ((to - options.from) * 60.0 / project.tempo * rate) as usize + 6 * options.rate as usize;
    processor.seek(options.from);
    processor.play();
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut out: Vec<f32> = Vec::with_capacity(frames);
    let begun = Instant::now();
    while out.len() < frames {
        processor.process(&mut left, &mut right);
        out.extend_from_slice(&left);
    }
    // A file of one channel of 32-bit floats.
    let bytes = (out.len() * 4) as u32;
    let mut file: Vec<u8> = Vec::with_capacity(44 + out.len() * 4);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + bytes).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&3u16.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&options.rate.to_le_bytes());
    file.extend_from_slice(&(options.rate * 4).to_le_bytes());
    file.extend_from_slice(&4u16.to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&bytes.to_le_bytes());
    out.iter().for_each(|sample| file.extend_from_slice(&sample.to_le_bytes()));
    std::fs::create_dir_all(&options.folder).map_err(|error| error.to_string())?;
    let path = options.folder.join("unwaited.wav");
    std::fs::write(&path, file).map_err(|error| error.to_string())?;
    let peak = out.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    println!(
        "played {:.1} s of '{}' in {:.1} s after leaving it alone for {seconds} s, peak {peak:.3}, into {}",
        out.len() as f64 / rate,
        options.track,
        begun.elapsed().as_secs_f64(),
        path.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    // Plugin files are scanned by this program started a second time.
    trill_plugins::scan::run_as_scanner_if_requested();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match parse(&arguments).and_then(|options| run(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("{problem}");
            ExitCode::FAILURE
        }
    }
}
