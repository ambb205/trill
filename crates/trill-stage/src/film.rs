//! A film of the stage: frame by frame, each frame the picture at its
//! exact time, with sound beside it.
//!
//! Nothing is captured from a screen and nothing depends on how busy the
//! machine is: frame 600 of a film at sixty frames a second is the stage
//! at ten seconds, however long it took to draw. A frame may be exposed
//! from several moments inside it (motion blur) and drawn larger than it
//! is kept, which only a film can afford.
//!
//! What is filmed is a [`Subject`]: something that has a picture for any
//! moment. A song as it plays is one ([`Playing`]); a song being made is
//! another ([`crate::making::Making`]).
//!
//! The frames are handed to `ffmpeg`, a program found on the path when a
//! film is made, which writes the file. Where there is none, the frames
//! are written as pictures in a folder with the sound beside them, and
//! the result says so.
//!
//! This module films what it is given. The whole of an export (rendering
//! the song's own sound, measuring it, filming, putting the two together)
//! is [`crate::export`]. A film is made on the thread that asks for it,
//! which should not be the window's; [`Progress`] is how another thread
//! watches it and stops it.

use crate::gpu::{Gpu, Quality, Renderer};
use crate::picture::{Format, Picture};
use crate::sound::Sound;
use crate::stage::{Song, Stage};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::Instant;
use trill_noir::Theme;

/// How a film is shot.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Shoot {
    pub format: Format,
    /// Frames a second.
    pub rate: u32,
    /// How many moments each frame is exposed from: 1 for none of the
    /// blur of motion.
    pub exposures: u32,
    /// How much of a frame's time its exposures are spread over, 0..=1:
    /// half is what a film camera's shutter leaves open.
    pub shutter: f64,
    /// How many times larger each way a frame is drawn than it is kept: 1
    /// or 2.
    pub oversample: u32,
}

impl Shoot {
    /// The best a film is shot at: eight moments a frame, drawn at twice
    /// the size.
    pub fn fine(format: Format) -> Self {
        // A picture of 3840 by 2160 drawn twice as large is 7680 across:
        // more than the layers of some cards hold, and it is fine enough
        // as it is.
        let oversample = if format.width.max(format.height) > 2560 { 1 } else { 2 };
        Self { format, rate: 60, exposures: 8, shutter: 0.5, oversample }
    }

    /// One moment a frame at the size it is kept: what the window shows.
    pub fn plain(format: Format) -> Self {
        Self { format, rate: 60, exposures: 1, shutter: 0.0, oversample: 1 }
    }
}

/// What a film is busy with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Not begun.
    Waiting,
    /// The song's own sound is being rendered: its plugins are waited for
    /// as a render waits for them, which can take a minute.
    Sound,
    /// Frames are being drawn.
    Frames,
    /// Over: finished, stopped or failed.
    Done,
}

/// How far a film has got, and the way to stop it.
#[derive(Debug, Default)]
pub struct Progress {
    phase: AtomicU8,
    done: AtomicU32,
    total: AtomicU32,
    stop: AtomicBool,
}

impl Progress {
    /// Frames finished, and how many there will be.
    pub fn frames(&self) -> (u32, u32) {
        (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }

    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Relaxed) {
            0 => Phase::Waiting,
            1 => Phase::Sound,
            2 => Phase::Frames,
            _ => Phase::Done,
        }
    }

    pub(crate) fn enter(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }

    /// Asks the film to stop after the frame it is on. While the sound is
    /// being rendered that is once the render is through.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// What came of a film.
#[derive(Clone, Debug, PartialEq)]
pub struct Filmed {
    /// The film, or the folder of frames where there was no `ffmpeg`.
    pub path: PathBuf,
    pub frames: u32,
    /// How long it took, in seconds.
    pub took: f64,
    /// What should be known about it: that it is a folder of frames, and
    /// why.
    pub note: Option<String>,
}

/// Something a film can be made of: it has a picture for any moment.
pub trait Subject {
    /// The picture at `seconds`, as `shoot` asks for it. `seed` numbers
    /// the picture, for what differs from one to the next (the grain).
    fn picture(&mut self, stage: &mut Stage, seconds: f64, shoot: &Shoot, seed: u32) -> Picture;
}

/// A song as it plays: the moment is a time in the song.
pub struct Playing<'a> {
    pub song: &'a Song,
    pub sound: &'a Sound,
}

impl Subject for Playing<'_> {
    fn picture(&mut self, stage: &mut Stage, seconds: f64, shoot: &Shoot, seed: u32) -> Picture {
        let (heard, beat) = (self.sound.heard(seconds), self.song.score.beat_at(seconds));
        stage.picture(self.song, &heard, beat, shoot.format, shoot.oversample, seed)
    }
}

/// The sound that goes beside a film's frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    /// A recording.
    pub path: PathBuf,
    /// Where in the recording the film begins, in seconds.
    pub from: f64,
    /// How long into the film the recording begins, in seconds: silence
    /// until then.
    pub delay: f64,
    /// How long the recording takes to come up to its level at the start,
    /// in seconds.
    pub fade_in: f64,
}

impl Audio {
    /// The recording at `path`, heard from `from` seconds in.
    pub fn from(path: impl Into<PathBuf>, from: f64) -> Self {
        Self { path: path.into(), from, delay: 0.0, fade_in: 0.0 }
    }
}

/// Paints and draws frames, on a graphics device of its own.
pub struct Filmer {
    stage: Stage,
    renderer: Renderer,
    pixels: Vec<u8>,
    /// The program the frames are handed to.
    encoder: PathBuf,
}

impl Filmer {
    /// The error is a sentence: there is no graphics card to draw on.
    pub fn new() -> Result<Self, String> {
        Ok(Self::on(Gpu::headless()?))
    }

    pub fn on(gpu: Gpu) -> Self {
        Self {
            stage: Stage::new(Theme::noir()),
            renderer: Renderer::new(gpu, Quality::FILM),
            pixels: Vec::new(),
            encoder: PathBuf::from("ffmpeg"),
        }
    }

    /// Hands the frames to `program` instead of the `ffmpeg` on the path.
    pub fn with_encoder(mut self, program: impl Into<PathBuf>) -> Self {
        self.encoder = program.into();
        self
    }

    /// What the graphics card calls itself.
    pub fn card(&self) -> &str {
        &self.renderer.gpu().name
    }

    /// The frame of `subject` whose time begins at `seconds`: rows of red,
    /// green, blue and alpha bytes, top row first.
    ///
    /// The picture is of the middle of the frame's time, which is where
    /// its exposures are centered: what happens during a frame is in it.
    pub fn frame_of(&mut self, subject: &mut dyn Subject, seconds: f64, shoot: &Shoot) -> Result<&[u8], String> {
        let rate = shoot.rate.max(1) as f64;
        let number = (seconds * rate).round().max(0.0) as u32;
        let exposures = shoot.exposures.clamp(1, 64);
        for exposure in 0..exposures {
            let within = 0.5 + shoot.shutter.clamp(0.0, 1.0) * ((exposure as f64 + 0.5) / exposures as f64 - 0.5);
            // The grain is one frame's, not one exposure's; the scatter of
            // the blur differs from exposure to exposure and evens out.
            let seed = number.wrapping_mul(64).wrapping_add(exposure);
            let picture = subject.picture(&mut self.stage, seconds + within / rate, shoot, seed);
            self.renderer.expose(&picture, 1.0 / exposures as f32, exposure == 0);
        }
        self.renderer.finish();
        self.renderer.read(&mut self.pixels)?;
        Ok(&self.pixels)
    }

    /// The frame of `song`, as `sound` lights it, whose time begins at
    /// `seconds` into the song.
    pub fn frame(&mut self, song: &Song, sound: &Sound, seconds: f64, shoot: &Shoot) -> Result<&[u8], String> {
        self.frame_of(&mut Playing { song, sound }, seconds, shoot)
    }

    /// Writes the frame of `song` at `seconds` as a PNG.
    pub fn still(
        &mut self,
        song: &Song,
        sound: &Sound,
        seconds: f64,
        shoot: &Shoot,
        path: &Path,
    ) -> Result<(), String> {
        self.still_of(&mut Playing { song, sound }, seconds, shoot, path)
    }

    /// Writes the frame of `subject` at `seconds` as a PNG.
    pub fn still_of(
        &mut self,
        subject: &mut dyn Subject,
        seconds: f64,
        shoot: &Shoot,
        path: &Path,
    ) -> Result<(), String> {
        let Format { width, height } = shoot.format;
        let pixels = self.frame_of(subject, seconds, shoot)?;
        write_png(path, pixels, width, height)
    }

    /// Films `song` from `from` to `to` seconds into `path`, with the
    /// sound of `audio` (a recording that begins where the song does)
    /// beside the picture. The error is a sentence, and so is
    /// [`Filmed::note`].
    #[expect(clippy::too_many_arguments, reason = "what a film is made of; each is a different thing")]
    pub fn film(
        &mut self,
        song: &Song,
        sound: &Sound,
        from: f64,
        to: f64,
        shoot: &Shoot,
        audio: Option<&Path>,
        path: &Path,
        progress: &Progress,
    ) -> Result<Filmed, String> {
        let audio = audio.map(|audio| Audio::from(audio, from));
        self.film_of(&mut Playing { song, sound }, from, to, shoot, audio.as_ref(), path, progress)
    }

    /// Films `subject` from `from` to `to` of its seconds into `path`,
    /// with `audio` beside the picture.
    #[expect(clippy::too_many_arguments, reason = "what a film is made of; each is a different thing")]
    pub fn film_of(
        &mut self,
        subject: &mut dyn Subject,
        from: f64,
        to: f64,
        shoot: &Shoot,
        audio: Option<&Audio>,
        path: &Path,
        progress: &Progress,
    ) -> Result<Filmed, String> {
        let started = Instant::now();
        let rate = shoot.rate.max(1) as f64;
        let total = ((to - from).max(0.0) * rate).round() as u32;
        if total == 0 {
            return Err("There is nothing to film: the stretch asked for has no length.".to_string());
        }
        progress.total.store(total, Ordering::Relaxed);
        progress.done.store(0, Ordering::Relaxed);
        progress.enter(Phase::Frames);
        if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
            std::fs::create_dir_all(folder)
                .map_err(|error| format!("{} could not be made: {error}", folder.display()))?;
        }
        let mut sink = Sink::open(&self.encoder, path, shoot, audio, to - from)?;
        for frame in 0..total {
            if progress.is_stopped() {
                sink.abandon();
                return Err(format!("The film was stopped at frame {frame} of {total}; nothing of it was kept."));
            }
            let pixels = match self.frame_of(subject, from + frame as f64 / rate, shoot) {
                Ok(pixels) => pixels,
                Err(reason) => {
                    sink.abandon();
                    return Err(reason);
                }
            };
            sink.write(frame, pixels, shoot.format)?;
            progress.done.store(frame + 1, Ordering::Relaxed);
        }
        let (path, note) = sink.close()?;
        Ok(Filmed { path, frames: total, took: started.elapsed().as_secs_f64(), note })
    }
}

/// Where the frames go: into `ffmpeg`, or into a folder.
enum Sink {
    Encoder { child: Child, path: PathBuf },
    Folder { folder: PathBuf, note: String },
}

impl Sink {
    fn open(encoder: &Path, path: &Path, shoot: &Shoot, audio: Option<&Audio>, length: f64) -> Result<Self, String> {
        let Format { width, height } = shoot.format;
        let mut command = Command::new(encoder);
        command.args(["-y", "-hide_banner", "-loglevel", "error"]);
        command.args(["-f", "rawvideo", "-pixel_format", "rgba"]);
        command.args(["-video_size", &format!("{width}x{height}"), "-framerate", &shoot.rate.to_string()]);
        command.args(["-i", "pipe:0"]);
        if let Some(audio) = audio {
            if audio.delay > 0.0 {
                command.args(["-itsoffset", &format!("{:.6}", audio.delay)]);
            }
            let lasts = (length - audio.delay).max(0.0);
            command.args(["-ss", &format!("{:.6}", audio.from.max(0.0)), "-t", &format!("{lasts:.6}")]);
            command.arg("-i").arg(&audio.path);
            command.args(["-map", "0:v", "-map", "1:a", "-c:a", "aac", "-b:a", "320k"]);
            if audio.fade_in > 0.0 {
                command.args(["-af", &format!("afade=t=in:st=0:d={:.3}", audio.fade_in)]);
            }
        }
        // The colors as screens take them to be, in a file every player
        // opens.
        command.args(["-vf", "scale=out_color_matrix=bt709:out_range=tv"]);
        command.args(["-c:v", "libx264", "-preset", "medium", "-crf", "17", "-pix_fmt", "yuv420p"]);
        command.args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]);
        command.args(["-movflags", "+faststart"]);
        command.arg(path);
        command.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => Ok(Self::Encoder { child, path: path.to_path_buf() }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // No ffmpeg: the frames as pictures, and the sound beside them.
                let folder = path.with_extension("frames");
                std::fs::create_dir_all(&folder)
                    .map_err(|error| format!("{} could not be made: {error}", folder.display()))?;
                let mut sound = String::new();
                if let Some(audio) = audio {
                    let beside = folder.join(audio.path.file_name().unwrap_or_default());
                    std::fs::copy(&audio.path, &beside)
                        .map_err(|error| format!("{} could not be copied: {error}", audio.path.display()))?;
                    sound = format!(
                        ", with the sound beside them ({}, from {:.3} s of it)",
                        beside.file_name().unwrap_or_default().to_string_lossy(),
                        audio.from.max(0.0) - audio.delay
                    );
                }
                let note = format!(
                    "ffmpeg was not found, so the film was written as numbered pictures in {}, {} a second{sound}. \
                     Install ffmpeg and film again to get one file.",
                    folder.display(),
                    shoot.rate
                );
                Ok(Self::Folder { folder, note })
            }
            Err(error) => Err(format!("ffmpeg could not be started: {error}")),
        }
    }

    fn write(&mut self, frame: u32, pixels: &[u8], format: Format) -> Result<(), String> {
        match self {
            Self::Encoder { child, .. } => {
                let written = child.stdin.as_mut().map(|stdin| stdin.write_all(pixels));
                match written {
                    Some(Ok(())) => Ok(()),
                    // The pipe broke: ffmpeg has gone, and says why.
                    _ => Err(format!("ffmpeg stopped at frame {frame}: {}", said(child))),
                }
            }
            Self::Folder { folder, .. } => {
                write_png(&folder.join(format!("frame-{:06}.png", frame + 1)), pixels, format.width, format.height)
            }
        }
    }

    fn close(self) -> Result<(PathBuf, Option<String>), String> {
        match self {
            Self::Encoder { mut child, path } => {
                drop(child.stdin.take());
                let finished = child.wait().map_err(|error| format!("ffmpeg could not be waited for: {error}"))?;
                if !finished.success() {
                    return Err(format!("ffmpeg could not write {}: {}", path.display(), said(&mut child)));
                }
                Ok((path, None))
            }
            Self::Folder { folder, note } => Ok((folder, Some(note))),
        }
    }

    /// Ends a film that was stopped, leaving nothing behind.
    fn abandon(self) {
        match self {
            Self::Encoder { mut child, path } => {
                drop(child.stdin.take());
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(path);
            }
            Self::Folder { folder, .. } => {
                let _ = std::fs::remove_dir_all(folder);
            }
        }
    }
}

/// What ffmpeg wrote of its trouble, once it has ended.
fn said(child: &mut Child) -> String {
    use std::io::Read;
    drop(child.stdin.take());
    let _ = child.wait();
    let mut text = String::new();
    if let Some(mut errors) = child.stderr.take() {
        let _ = errors.read_to_string(&mut text);
    }
    match text.trim() {
        "" => "it gave no reason".to_string(),
        reason => reason.lines().last().unwrap_or(reason).to_string(),
    }
}

fn write_png(path: &Path, pixels: &[u8], width: u32, height: u32) -> Result<(), String> {
    if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
        std::fs::create_dir_all(folder).map_err(|error| format!("{} could not be made: {error}", folder.display()))?;
    }
    image::save_buffer(path, pixels, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| format!("{} could not be written: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_film_of_the_largest_pictures_is_not_drawn_larger_still() {
        assert_eq!(Shoot::fine(Format::HD).oversample, 2);
        assert_eq!(Shoot::fine(Format::UPRIGHT).oversample, 2);
        assert_eq!(Shoot::fine(Format::UHD).oversample, 1);
        assert_eq!((Shoot::plain(Format::HD).exposures, Shoot::plain(Format::HD).oversample), (1, 1));
    }

    #[test]
    fn progress_counts_frames_says_what_a_film_is_busy_with_and_carries_the_wish_to_stop() {
        let progress = Progress::default();
        assert_eq!((progress.frames(), progress.phase()), ((0, 0), Phase::Waiting));
        progress.enter(Phase::Sound);
        assert_eq!(progress.phase(), Phase::Sound);
        progress.enter(Phase::Done);
        assert_eq!(progress.phase(), Phase::Done);
        assert!(!progress.is_stopped());
        progress.stop();
        assert!(progress.is_stopped());
    }
}
