//! The whole of a film of a song: its own sound rendered, measured, filmed
//! and put together.
//!
//! A film is not a picture laid over a recording. The same job renders the
//! song, as an export of its audio does (fresh devices, plugins waited for
//! until they answer, the same samples every time), and what that render
//! measured lights the picture: the mix sets its mood, and every part is
//! lit by its own level, which the render reads off each track as the mix
//! is made ([`trill_engine::render::Rendered::track_levels`]). Then the
//! frames are drawn, each at its exact time, and handed to `ffmpeg` with
//! the sound.
//!
//! A [`SongFilm`] holds everything it needs (a snapshot of the song, the
//! request), so it runs away from the session and from the window's
//! thread; [`Progress`] is how it is watched and stopped.

use crate::director::{Direction, Plan};
use crate::film::{Audio, Filmer, Phase, Playing, Progress, Shoot};
use crate::score::Score;
use crate::sound::Sound;
use crate::stage::Song;
use crate::voice::Waves;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use trill_core::audio::AudioBuffer;
use trill_core::{Beats, Project, TrackId};
use trill_engine::render::{RenderJob, RenderOptions, Rendered};
use trill_media::{WavFormat, write_wav};

/// Values a second in the levels a render keeps of every track.
pub(crate) const LEVELS_PER_SECOND: f64 = 10.0;

/// What a film of a song is asked to be.
#[derive(Clone, Debug, PartialEq)]
pub struct FilmRequest {
    /// Where it is written.
    pub path: PathBuf,
    pub shoot: Shoot,
    /// The stretch of the song, in beats: from, and to (`None` for the
    /// song's end).
    pub from: Beats,
    pub to: Option<Beats>,
    /// What is asked of the director, over its own plan.
    pub directions: Vec<Direction>,
    /// The longest the film goes on after the stretch while the sound
    /// rings out, in seconds. It ends sooner where the sound does.
    pub tail: f64,
    /// The rate the sound is rendered at.
    pub sample_rate: u32,
    /// How long a plugin may take to answer before the render goes ahead
    /// without it.
    pub patience: Duration,
}

impl FilmRequest {
    /// The whole song, shot as `shoot` says, into `path`.
    pub fn new(path: impl Into<PathBuf>, shoot: Shoot) -> Self {
        Self {
            path: path.into(),
            shoot,
            from: 0.0,
            to: None,
            directions: Vec::new(),
            tail: 4.0,
            sample_rate: 48_000,
            patience: Duration::from_secs(10),
        }
    }
}

/// What came of an export.
#[derive(Clone, Debug, PartialEq)]
pub struct Exported {
    /// The film, or the folder of its frames where there was no `ffmpeg`.
    pub path: PathBuf,
    pub frames: u32,
    /// How long the film is, in seconds.
    pub seconds: f64,
    /// How long making it took, in seconds.
    pub took: f64,
    /// What should be known about it, in sentences: that it is a folder
    /// of frames, a plugin that was waited for or never answered, a file
    /// the song could not read.
    pub notes: Vec<String>,
    /// The loudness and the peak of its sound; `None` for a silent film.
    pub loudness_lufs: Option<f32>,
    pub peak_db: Option<f32>,
}

/// A film of a song as it plays, with its own sound.
pub struct SongFilm {
    /// The song, as a render takes it.
    pub render: RenderJob,
    /// The tracks that play through a drum instrument, where the song
    /// itself does not say what their notes are: a session knows that of
    /// plugins.
    pub drums: Vec<TrackId>,
    pub request: FilmRequest,
}

impl SongFilm {
    /// The song as it will be filmed: its score, and the director's plan
    /// with what was asked of it. Nothing is rendered or read for this.
    pub fn song(&self) -> Song {
        let score = score_of(&self.render.project, &self.drums);
        let plan = Plan::directed(&score, &self.request.directions);
        Song { score, plan }
    }

    /// The stretch that will be filmed, in beats.
    pub fn stretch(&self) -> (Beats, Beats) {
        let length = self.song().score.length;
        let from = self.request.from.clamp(0.0, length);
        (from, self.request.to.unwrap_or(length).clamp(from, length))
    }

    /// Makes the film. The error is a sentence.
    pub fn run(self, progress: &Progress) -> Result<Exported, String> {
        let outcome = self.make(progress);
        progress.enter(Phase::Done);
        outcome
    }

    fn make(self, progress: &Progress) -> Result<Exported, String> {
        let started = Instant::now();
        let request = &self.request;
        // The card first: without one there is no film, and finding that
        // out should not take a render.
        let mut filmer = Filmer::new()?;
        let Song { score, plan } = self.song();
        let (from, to) = self.stretch();
        if to <= from {
            return Err("There is nothing to film: the stretch asked for has no length.".to_string());
        }
        let score = score.with_voices(Waves::default().voiced(&self.render.project));
        let song = Song { score, plan };

        progress.enter(Phase::Sound);
        let options = RenderOptions {
            sample_rate: request.sample_rate,
            start_beat: from,
            end_beat: Some(to),
            max_tail_seconds: request.tail.max(0.0),
            patience: request.patience,
            ..RenderOptions::default()
        };
        let rendered =
            self.render.render(&options).map_err(|error| format!("The song could not be rendered: {error}."))?;
        if progress.is_stopped() {
            return Err("The film was stopped before its first frame; nothing of it was kept.".to_string());
        }
        let sound = sound_of(&rendered).starting_at(song.score.seconds(from));
        let seconds = rendered.left.len() as f64 / f64::from(rendered.sample_rate);
        let mut notes = notes_of(&rendered);
        let wav = sound_file(&request.path);
        write_sound(&wav, &rendered)?;
        let stats = &rendered.measured.stats;
        let (loudness, peak) = (stats.loudness_lufs, stats.peak_db);
        drop(rendered);

        let start = song.score.seconds(from);
        let audio = Audio::from(&wav, 0.0);
        let mut subject = Playing { song: &song, sound: &sound };
        let filmed =
            filmer.film_of(&mut subject, start, start + seconds, &request.shoot, Some(&audio), &request.path, progress);
        let _ = std::fs::remove_file(&wav);
        let filmed = filmed?;
        notes.extend(filmed.note);
        Ok(Exported {
            path: filmed.path,
            frames: filmed.frames,
            seconds,
            took: started.elapsed().as_secs_f64(),
            notes,
            loudness_lufs: Some(loudness),
            peak_db: Some(peak),
        })
    }
}

/// The score of `project`, with `drums` the tracks that play through a
/// drum instrument.
pub(crate) fn score_of(project: &Project, drums: &[TrackId]) -> Score {
    Score::of(project, |track| drums.contains(&track.id))
}

/// What a render sounds like: the mix measured, and every track from the
/// levels the render read off it.
pub(crate) fn sound_of(rendered: &Rendered) -> Sound {
    let mut sound = Sound::measure(&rendered.left, &rendered.right, rendered.sample_rate);
    for (track, levels) in &rendered.track_levels {
        sound.add_track_levels(*track, levels, LEVELS_PER_SECOND);
    }
    sound
}

/// Where a film's sound is kept while the film is made: beside it.
pub(crate) fn sound_file(film: &Path) -> PathBuf {
    film.with_extension("sound.wav")
}

/// Writes the mix of `rendered` to `path`.
pub(crate) fn write_sound(path: &Path, rendered: &Rendered) -> Result<(), String> {
    if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
        std::fs::create_dir_all(folder).map_err(|error| format!("{} could not be made: {error}", folder.display()))?;
    }
    let buffer = AudioBuffer {
        sample_rate: f64::from(rendered.sample_rate),
        left: rendered.left.clone(),
        right: rendered.right.clone(),
    };
    write_wav(path, &buffer, WavFormat::Int24)
        .map_err(|error| format!("The film's sound could not be written: {error}."))
}

/// What a render has to say about itself, in sentences: the plugins it
/// waited for, and what the song could not use.
pub(crate) fn notes_of(rendered: &Rendered) -> Vec<String> {
    let mut notes = Vec::new();
    for waited in &rendered.measured.waited {
        let whose = format!("{} on {}", waited.device_name, waited.track_name);
        notes.push(match waited.answered {
            true if waited.silent.is_empty() => format!("{whose} was waited for {:.0} s.", waited.seconds),
            true => format!(
                "{whose} was waited for {:.0} s and left {} of its notes silent: it may not have had them loaded.",
                waited.seconds,
                waited.silent.len()
            ),
            false => format!(
                "{whose} did not answer its notes within {:.0} s; the film was made without waiting further, and \
                 it may be silent in it.",
                waited.seconds
            ),
        });
    }
    notes.extend(rendered.problems.iter().map(|problem| format!("{problem}.")));
    notes
}
