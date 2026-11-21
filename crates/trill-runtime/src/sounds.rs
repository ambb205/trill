//! Sounds: what a front end does with an audio file before and as it
//! becomes part of the song. Each of these needs the session, the media
//! cache and the library together, which is why they are here and not in
//! any one of them, and why no front end should do them by hand.

use crate::Runtime;
use std::path::{Path, PathBuf};
use trill_core::project::{AudioContent, WarpMode};
use trill_core::{Beats, ClipId, TrackId};
use trill_engine::SessionError;
use trill_library::{Key, Tags, infer};
use trill_media::MediaError;

/// How many of the folders above a file are asked what kind of sound it
/// is, for a file the library has not indexed. Folders further up name
/// collections, not their contents.
const FOLDERS_READ: usize = 4;

/// What is known about an audio file without playing it: what its header
/// says, and what its name and folders say, read the way the library reads
/// them.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundFacts {
    pub seconds: f64,
    pub sample_rate: u32,
    /// Channels in the file. Everything plays back as stereo.
    pub channels: u16,
    /// The tempo it was recorded at, in beats per minute, if its name or
    /// folders say. This is what a warped clip of it follows the project
    /// tempo with.
    pub tempo: Option<f64>,
    pub key: Option<Key>,
    pub tags: Tags,
}

/// A sound to put on a track as an audio clip.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundClip {
    pub path: PathBuf,
    pub track: TrackId,
    /// Where on the timeline the clip starts, in beats.
    pub start: Beats,
    /// Its length in beats; `None` for as long as the file lasts from
    /// `offset_seconds` on.
    pub length: Option<Beats>,
    /// `None` names the clip after the file.
    pub name: Option<String>,
    /// Where in the file the clip starts, in seconds of the recording.
    pub offset_seconds: f64,
    pub gain_db: f32,
    pub fade_in: Beats,
    pub fade_out: Beats,
    pub warp: WarpMode,
    /// The tempo the file was recorded at; `None` to go by its name.
    pub source_tempo: Option<f64>,
    pub transpose: f64,
}

impl SoundClip {
    /// The whole file at `start` on `track`, unwarped, at unity gain.
    pub fn new(path: impl Into<PathBuf>, track: TrackId, start: Beats) -> Self {
        Self {
            path: path.into(),
            track,
            start,
            length: None,
            name: None,
            offset_seconds: 0.0,
            gain_db: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            warp: WarpMode::Off,
            source_tempo: None,
            transpose: 0.0,
        }
    }
}

/// A track to make for a sound: see [`Runtime::add_sound_on_new_track`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewSoundTrack {
    /// `None` names the track after the file.
    pub name: Option<String>,
    /// 0xRRGGBB; `None` takes the color the project hands out next.
    pub color: Option<u32>,
}

/// The clip [`Runtime::add_sound`] made.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedSound {
    pub clip: ClipId,
    /// The clip's length on the timeline, in beats.
    pub length: Beats,
    /// The length of the whole file, in seconds.
    pub file_seconds: f64,
    /// The tempo stored with the clip: the one given, or the one its name
    /// states.
    pub source_tempo: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SoundError {
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The request cannot be met as it stands; the message says what to
    /// change.
    #[error("{0}")]
    Invalid(String),
}

impl Runtime {
    /// What is known about the audio file at `path`. Reads the file's
    /// header, not its audio (unless the format does not state its length
    /// up front).
    pub fn sound_facts(&self, path: &Path) -> Result<SoundFacts, MediaError> {
        let path = absolute(path);
        let info = trill_media::probe(&path)?;
        let seconds = match info.seconds() {
            Some(seconds) => seconds,
            None => self.media.audio(&path)?.seconds(),
        };
        // The library has read the name in the light of the whole pack it
        // came in. For a file it does not know, the name and the nearest
        // folders are read here by the same rules.
        let index = self.library.index();
        let indexed = index.find(&path).and_then(|id| index.entry(id)).filter(|entry| entry.audio.is_some());
        let (tempo, key, tags) = match indexed {
            Some(entry) => (entry.tempo, entry.key, entry.tags),
            None => {
                let stem = path.file_stem().map(|stem| stem.to_string_lossy()).unwrap_or_default();
                let folders: Vec<String> = path
                    .parent()
                    .into_iter()
                    .flat_map(Path::iter)
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect();
                let nearest: Vec<&str> =
                    folders.iter().skip(folders.len().saturating_sub(FOLDERS_READ)).map(String::as_str).collect();
                let inferred = infer(&stem, &nearest, Some(seconds));
                (inferred.tempo, inferred.key, inferred.tags)
            }
        };
        Ok(SoundFacts {
            seconds,
            sample_rate: info.sample_rate,
            channels: info.channels,
            tempo: tempo.map(f64::from),
            key,
            tags,
        })
    }

    /// Plays the file over the song without adding it to the project, as a
    /// browser does when a sound is clicked, and returns its length in
    /// seconds. Replaces whatever was being previewed. The file counts as
    /// recently used.
    ///
    /// Decoding happens here, on the caller's thread, with the session
    /// free: a long file delays the preview, not the rest of the program.
    pub fn preview(&self, path: &Path, gain_db: f32) -> Result<f64, MediaError> {
        let path = absolute(path);
        let sample_rate = self.session.lock().audio_info().sample_rate;
        let audio = self.media.audio_at_rate(&path, sample_rate)?;
        let seconds = audio.seconds();
        self.session.lock().audition(audio, gain_db);
        self.library.mark_used(&path);
        Ok(seconds)
    }

    /// Fades out the sound being previewed, if there is one.
    pub fn stop_preview(&self) {
        self.session.lock().stop_audition();
    }

    /// Puts a sound on a track as an audio clip: one undo step.
    ///
    /// Left to itself the clip is as long as the file. A clip that is to
    /// follow the project tempo needs the tempo the file was recorded at:
    /// the one given, or failing that the one its name states. That tempo
    /// is stored with the clip even when it is not warped, so warping can
    /// be turned on later without asking again. Fails, changing nothing,
    /// if the file cannot be read, if warping is asked for and no tempo is
    /// to be had, or if the project refuses the clip.
    pub fn add_sound(&self, sound: &SoundClip) -> Result<AddedSound, SoundError> {
        self.place_sound(sound, None).map(|(_, added)| added)
    }

    /// Like [`Runtime::add_sound`], on a track made for the sound: what
    /// dropping a file below the last track does. The new track goes last,
    /// holds no devices (audio clips need none) and is returned with the
    /// clip. `sound.track` is not looked at. Track and clip are one undo
    /// step, and a failure leaves neither behind.
    pub fn add_sound_on_new_track(
        &self,
        sound: &SoundClip,
        track: &NewSoundTrack,
    ) -> Result<(TrackId, AddedSound), SoundError> {
        self.place_sound(sound, Some(track))
    }

    /// The body of both: on `sound.track`, or on a track made as `new_track`
    /// says.
    fn place_sound(
        &self,
        sound: &SoundClip,
        new_track: Option<&NewSoundTrack>,
    ) -> Result<(TrackId, AddedSound), SoundError> {
        let path = absolute(&sound.path);
        let facts = self.sound_facts(&path)?;
        let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let source_tempo = sound.source_tempo.or(facts.tempo);
        if sound.warp != WarpMode::Off && source_tempo.is_none() {
            return Err(SoundError::Invalid(format!(
                "'{file_name}' does not say in its name what tempo it was recorded at, so it cannot follow the                  project's. Give its tempo in BPM as the source tempo, or leave warp off."
            )));
        }
        let remaining = facts.seconds - sound.offset_seconds;
        // Written so that an offset that is not a number is refused too.
        if !sound.offset_seconds.is_finite() || remaining <= 0.0 {
            return Err(SoundError::Invalid(format!(
                "an offset of {} s is past the end of '{file_name}', which is {:.3} s long",
                sound.offset_seconds, facts.seconds
            )));
        }
        let content = AudioContent {
            path: path.display().to_string(),
            offset_seconds: sound.offset_seconds,
            gain_db: sound.gain_db,
            fade_in: sound.fade_in,
            fade_out: sound.fade_out,
            warp: sound.warp,
            source_tempo,
            transpose: sound.transpose,
            looped: None,
            ..Default::default()
        };
        let stem = path.file_stem().map_or_else(|| "Audio".to_string(), |stem| stem.to_string_lossy().into_owned());
        let name = sound.name.clone().unwrap_or_else(|| stem.clone());

        let mut session = self.session.lock();
        let length = sound.length.unwrap_or(remaining / content.seconds_per_beat(session.project().tempo));
        let (track, clip) = session.edit(|project| {
            let track = match new_track {
                Some(new_track) => {
                    let track = project.add_track(new_track.name.clone().unwrap_or(stem));
                    if let Some(color) = new_track.color {
                        project.track_mut(track)?.color = color;
                    }
                    track
                }
                None => sound.track,
            };
            Ok((track, project.add_audio_clip(track, name, sound.start, length, content)?))
        })?;
        drop(session);
        self.library.mark_used(&path);
        Ok((track, AddedSound { clip, length, file_seconds: facts.seconds, source_tempo }))
    }
}

/// `path` made absolute against the working directory, so that a project
/// does not depend on where the program was started from, and so that the
/// library, which lists absolute paths, recognizes the file.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}
