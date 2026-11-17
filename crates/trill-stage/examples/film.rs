//! Stills and films of the stage, from a song file.
//!
//!     cargo run -p trill-stage --example film -- SONG.trill [--film OUT.mp4]
//!         [--size WIDTHxHEIGHT] [--rate FRAMES] [--plain] [--own-sound | --audio MIX.wav]
//!         [--still SECONDS OUT.png]... [--clip FROM TO OUT.mp4]...
//!         [--making STORY.journal [--speed N] [--whole OUT.mp4]]
//!
//! `--film` is the whole song with its own sound, made as the window's
//! File, Export a film makes it: the song rendered here, each part lit by
//! its own level, the two put together by ffmpeg. This program hosts no
//! plugins: a song that plays through them is filmed from the window or
//! over MCP (the `film` tool), and here its plugin tracks are silent.
//!
//! `--own-sound` lights stills and clips by the song rendered here; `--audio`
//! by a recording of it that begins where the song does, which a clip then
//! carries. Without either the picture is lit by the notes alone and a clip
//! is silent. `--making` films how a song was made from a journal written
//! out with its conversation (`remake` writes a stand-in).
//! `--plain` draws each frame once at its own size, as the window does;
//! otherwise frames are exposed from several moments and drawn larger.
//! `--still` and `--clip` may be given several times; times are in seconds.
//!
//! Nothing of the user's is read or written: the song file is only read,
//! and the pictures go where they are told to.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::journal::Journal;
use trill_core::{Project, TrackId};
use trill_engine::render::RenderJob;
use trill_engine::{AudioBackend, Session};
use trill_stage::export::{self, FilmRequest, SongFilm};
use trill_stage::film::{Audio, Filmer, Progress, Shoot};
use trill_stage::making::Making;
use trill_stage::voice::Waves;
use trill_stage::{Format, Score, Song, Sound};

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
    /// Where to write the whole song with its own sound.
    film: Option<PathBuf>,
    /// Light the pictures by the song rendered here.
    own_sound: bool,
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
        film: None,
        own_sound: false,
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
            "--film" => asked.film = Some(args.next().ok_or("--film needs a file to write")?.into()),
            "--own-sound" => asked.own_sound = true,
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
            "usage: film SONG.trill [--film OUT.mp4] [--size WxH] [--rate N] [--plain] [--own-sound | --audio \
                    MIX.wav] [--still SECONDS OUT.png] [--clip FROM TO OUT.mp4] [--making STORY.journal [--whole \
                    OUT.mp4]]"
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

/// The song at `path` ready to be rendered here, on the built-in devices
/// and no audio device, and its tracks that play through a drum kit.
/// Nothing is kept of the session: no journal, no settings.
fn rendering(path: &Path) -> Result<(RenderJob, Vec<TrackId>), String> {
    let backend = AudioBackend::Null { sample_rate: 48_000 };
    let mut session = Session::new(Arc::new(trill_dsp::BuiltinFactory), backend).map_err(|error| error.to_string())?;
    session.load(path).map_err(|error| format!("{} could not be opened: {error}", path.display()))?;
    let tracks = session.project().tracks.iter().map(|track| track.id);
    let drums = tracks.filter(|track| session.instrument_plays_drums(*track)).collect();
    Ok((session.render_job(), drums))
}

/// Films the whole song into `path` with its own sound, as the window does.
fn film_whole(asked: &Asked, path: &Path, shoot: Shoot) -> Result<(), String> {
    let (render, drums) = rendering(&asked.song)?;
    let film = SongFilm { render, drums, request: FilmRequest::new(path, shoot) };
    let made = film.run(&Progress::default())?;
    eprintln!(
        "{}: {:.1} s, {} frames in {:.1} s; {:.1} LUFS, peak {:.1} dB",
        made.path.display(),
        made.seconds,
        made.frames,
        made.took,
        made.loudness_lufs.unwrap_or(f32::NEG_INFINITY),
        made.peak_db.unwrap_or(f32::NEG_INFINITY)
    );
    for note in &made.notes {
        eprintln!("{note}");
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let asked = parse()?;
    let json = std::fs::read_to_string(&asked.song)
        .map_err(|error| format!("{} could not be read: {error}", asked.song.display()))?;
    let project =
        Project::from_json(&json).map_err(|error| format!("{} is not a song: {error}", asked.song.display()))?;
    let shoot = match asked.plain {
        true => Shoot { rate: asked.rate, ..Shoot::plain(asked.format) },
        false => Shoot { rate: asked.rate, ..Shoot::fine(asked.format) },
    };
    if let Some(path) = &asked.film {
        film_whole(&asked, path, shoot)?;
    }
    // With the recordings it plays, as a film of it has them.
    let song = Song::from(Score::read(&project).with_voices(Waves::default().voiced(&project)));
    let sound = match (&asked.audio, asked.own_sound) {
        (Some(path), _) => {
            let (left, right, rate) = read_wav(path)?;
            Sound::measure(&left, &right, rate)
        }
        (None, true) => export::listen(&rendering(&asked.song)?.0, 48_000)?,
        (None, false) => Sound::default(),
    };
    if asked.stills.is_empty() && asked.clips.is_empty() && asked.making.is_none() {
        return Ok(());
    }
    let mut filmer = Filmer::new()?;
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
