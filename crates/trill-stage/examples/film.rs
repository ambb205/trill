//! Stills and films of the stage, from a song file.
//!
//!     cargo run -p trill-stage --example film -- SONG.trill [--audio MIX.wav]
//!         [--size WIDTHxHEIGHT] [--rate FRAMES] [--plain]
//!         [--still SECONDS OUT.png]... [--clip FROM TO OUT.mp4]...
//!
//! `--audio` is a recording of the song that begins where the song does: it
//! is measured to light the picture, and a clip carries its sound. Without
//! one the picture is lit by the notes alone and a clip is silent.
//! `--plain` draws each frame once at its own size, as the window does;
//! otherwise frames are exposed from several moments and drawn larger.
//! `--still` and `--clip` may be given several times; times are in seconds.
//!
//! Nothing of the user's is read or written: the song file is only read,
//! and the pictures go where they are told to.

use std::path::{Path, PathBuf};
use trill_core::Project;
use trill_core::journal::Journal;
use trill_stage::film::{Audio, Filmer, Progress, Shoot};
use trill_stage::making::Making;
use trill_stage::{Format, Song, Sound};

struct Asked {
    song: PathBuf,
    audio: Option<PathBuf>,
    format: Format,
    rate: u32,
    plain: bool,
    stills: Vec<(f64, PathBuf)>,
    clips: Vec<(f64, f64, PathBuf)>,
    /// A journal to film the making-of from, and how fast.
    making: Option<PathBuf>,
    speed: Option<f64>,
    /// Where to write the whole of the making-of.
    whole: Option<PathBuf>,
}

fn parse() -> Result<Asked, String> {
    let mut args = std::env::args().skip(1);
    let mut asked = Asked {
        song: PathBuf::new(),
        audio: None,
        format: Format::HD,
        rate: 60,
        plain: false,
        stills: Vec::new(),
        clips: Vec::new(),
        making: None,
        speed: None,
        whole: None,
    };
    let number = |text: Option<String>, what: &str| -> Result<f64, String> {
        let text = text.ok_or_else(|| format!("{what} is missing"))?;
        text.parse().map_err(|_| format!("{what} should be a number, not '{text}'"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--audio" => asked.audio = Some(args.next().ok_or("--audio needs a file")?.into()),
            "--size" => {
                let size = args.next().ok_or("--size needs WIDTHxHEIGHT")?;
                let (width, height) = size.split_once('x').ok_or("--size needs WIDTHxHEIGHT")?;
                let side = |text: &str| text.parse::<u32>().map_err(|_| format!("'{text}' is not a size"));
                asked.format = Format::new(side(width)?, side(height)?);
            }
            "--rate" => asked.rate = number(args.next(), "--rate")? as u32,
            "--plain" => asked.plain = true,
            "--making" => asked.making = Some(args.next().ok_or("--making needs a journal")?.into()),
            "--speed" => asked.speed = Some(number(args.next(), "--speed")?),
            "--whole" => asked.whole = Some(args.next().ok_or("--whole needs a file to write")?.into()),
            "--still" => {
                let seconds = number(args.next(), "the time of --still")?;
                asked.stills.push((seconds, args.next().ok_or("--still needs a file to write")?.into()));
            }
            "--clip" => {
                let (from, to) =
                    (number(args.next(), "the start of --clip")?, number(args.next(), "the end of --clip")?);
                asked.clips.push((from, to, args.next().ok_or("--clip needs a file to write")?.into()));
            }
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            other => asked.song = other.into(),
        }
    }
    if asked.song.as_os_str().is_empty() {
        return Err(
            "usage: film SONG.trill [--audio MIX.wav] [--size WxH] [--still SECONDS OUT.png] [--clip FROM TO OUT.mp4]"
                .into(),
        );
    }
    Ok(asked)
}

/// Reads a WAV file as two channels of samples and its rate.
fn read_wav(path: &Path) -> Result<(Vec<f32>, Vec<f32>, u32), String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|error| format!("{} could not be read: {error}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(Result::ok).collect(),
        hound::SampleFormat::Int => {
            let full = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().filter_map(Result::ok).map(|sample| sample as f32 / full).collect()
        }
    };
    let channels = spec.channels.max(1) as usize;
    let left: Vec<f32> = samples.iter().step_by(channels).copied().collect();
    let right: Vec<f32> = match channels {
        1 => left.clone(),
        _ => samples.iter().skip(1).step_by(channels).copied().collect(),
    };
    Ok((left, right, spec.sample_rate))
}

fn run() -> Result<(), String> {
    let asked = parse()?;
    let json = std::fs::read_to_string(&asked.song)
        .map_err(|error| format!("{} could not be read: {error}", asked.song.display()))?;
    let project =
        Project::from_json(&json).map_err(|error| format!("{} is not a song: {error}", asked.song.display()))?;
    let song = Song::of(&project);
    let sound = match &asked.audio {
        Some(path) => {
            let (left, right, rate) = read_wav(path)?;
            Sound::measure(&left, &right, rate)
        }
        None => Sound::default(),
    };
    let mut filmer = Filmer::new()?;
    let shoot = match asked.plain {
        true => Shoot { rate: asked.rate, ..Shoot::plain(asked.format) },
        false => Shoot { rate: asked.rate, ..Shoot::fine(asked.format) },
    };
    eprintln!(
        "{}: {:.1} s, {} takes; drawing on {} at {}x{}, {} exposures a frame, drawn {}x",
        song.score.name,
        song.score.duration(),
        song.plan.takes.len(),
        filmer.card(),
        shoot.format.width,
        shoot.format.height,
        shoot.exposures,
        shoot.oversample
    );
    if let Some(journal) = &asked.making {
        return film_making(&asked, journal, &project, sound, &mut filmer, &shoot);
    }
    for (seconds, path) in &asked.stills {
        filmer.still(&song, &sound, *seconds, &shoot, path)?;
        eprintln!("{} at {seconds} s", path.display());
    }
    for (from, to, path) in &asked.clips {
        let filmed =
            filmer.film(&song, &sound, *from, *to, &shoot, asked.audio.as_deref(), path, &Progress::default())?;
        eprintln!(
            "{}: {} frames in {:.1} s ({:.1} a second)",
            filmed.path.display(),
            filmed.frames,
            filmed.took,
            filmed.frames as f64 / filmed.took
        );
        if let Some(note) = filmed.note {
            eprintln!("{note}");
        }
    }
    Ok(())
}

/// Films the making of `project` as `journal` tells it: stills and
/// clips at times of the film, or the whole of it. The recording given as
/// `--audio` plays under it, timed to end as the film does.
fn film_making(
    asked: &Asked,
    journal: &Path,
    project: &Project,
    sound: Sound,
    filmer: &mut Filmer,
    shoot: &Shoot,
) -> Result<(), String> {
    let text = std::fs::read_to_string(journal)
        .map_err(|error| format!("{} could not be read: {error}", journal.display()))?;
    let journal =
        Journal::from_text(&text).map_err(|error| format!("{} is not a journal: {error:?}", journal.display()))?;
    let music = sound.duration();
    let mut making = Making::of(journal, project, &[], asked.speed.unwrap_or(1.0))?;
    if asked.speed.is_none() && music > 12.0 && making.length() > music {
        let speed = Making::speed_to_last(making.story_length(), music);
        making = Making::of(Journal::from_text(&text).map_err(|error| format!("{error:?}"))?, project, &[], speed)?;
    }
    let length = making.length();
    let from = (music - length).max(0.0);
    eprintln!(
        "the making of: {} steps in {} shots, a story of {:.1} s, a film of {length:.1} s",
        making.steps(),
        making.shots().len(),
        making.story_length()
    );
    let mut making = making.with_sound(sound, from);
    for (seconds, path) in &asked.stills {
        filmer.still_of(&mut making, *seconds, shoot, path)?;
        eprintln!("{} at {seconds} s", path.display());
    }
    let whole = asked.whole.iter().map(|path| (0.0, length, path.clone()));
    for (start, end, path) in asked.clips.iter().cloned().chain(whole) {
        let audio = asked.audio.as_ref().map(|audio| Audio {
            path: audio.clone(),
            from: from + start,
            delay: 0.0,
            fade_in: if from + start > 0.0 { 1.5 } else { 0.0 },
        });
        let filmed = filmer.film_of(&mut making, start, end, shoot, audio.as_ref(), &path, &Progress::default())?;
        eprintln!(
            "{}: {} frames in {:.1} s ({:.1} a second)",
            filmed.path.display(),
            filmed.frames,
            filmed.took,
            filmed.frames as f64 / filmed.took
        );
    }
    Ok(())
}

fn main() {
    if let Err(reason) = run() {
        eprintln!("{reason}");
        std::process::exit(1);
    }
}
