//! Where graphs get the audio their clips play.
//!
//! A clip names a file. What the audio thread needs is samples at the
//! engine's rate that it can read at (as nearly as possible) one frame per
//! output frame, because the only interpolation it can afford is linear.
//! [`AudioSources`] closes that gap off the audio thread:
//!
//! * Files are decoded and converted to the engine's rate by the shared
//!   [`MediaCache`], band-limited, once per file and rate.
//! * A clip that is warped or transposed gets a stretched copy of its whole
//!   file from the engine's stretcher, which makes them on a worker thread
//!   and keeps them in a bounded cache. The whole file, not the
//!   part the clip shows: trimming, splitting and moving a clip then only
//!   change where in that copy it reads, so they are instant and a split is
//!   inaudible by construction.
//! * Until a stretched copy is ready, a live graph plays the plain file at
//!   the speed that keeps it in time (a *stand-in*: right timing, wrong
//!   pitch). A rendered graph waits for the real thing instead.

use crate::stretcher::{Lookup, StretchKey, Stretcher};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_core::project::{AudioContent, WarpMode};
use trill_dsp::stretch::{MAX_TIME_RATIO, MIN_TIME_RATIO, StretchMode, StretchOptions};
use trill_media::{FileStamp, MediaCache};

/// The decoded files and stretched copies that graphs draw on. Cloning is
/// cheap and gives a handle to the same caches, which is how an offline
/// render shares what the session has already prepared.
#[derive(Clone)]
pub struct AudioSources {
    media: Arc<MediaCache>,
    stretcher: Arc<Stretcher>,
}

impl Default for AudioSources {
    /// Sources with a media cache of their own, for a render that shares
    /// nothing with a session.
    fn default() -> Self {
        Self::new(Arc::new(MediaCache::default()))
    }
}

/// Audio for one clip, and how timeline time maps onto it.
pub(crate) struct ClipAudio {
    pub data: Arc<AudioBuffer>,
    /// Frames of `data` per second of the recording.
    pub frames_per_second: f64,
    /// Frames of `data` per output frame.
    pub step: f64,
    /// The stretched copy this clip plays or is waiting for.
    pub key: Option<StretchKey>,
    /// Whether `data` is a stand-in for a stretched copy still being made.
    pub stand_in: bool,
    /// Why the clip does not sound as it should, although it plays.
    pub problem: Option<String>,
}

impl AudioSources {
    /// Sources that decode through `media`.
    pub fn new(media: Arc<MediaCache>) -> Self {
        Self { media, stretcher: Arc::new(Stretcher::new(Stretcher::DEFAULT_BUDGET)) }
    }

    /// The cache files are decoded through.
    pub fn media(&self) -> &Arc<MediaCache> {
        &self.media
    }

    /// The audio `content` plays in a graph running at `sample_rate` in a
    /// project at `tempo`. With `wait`, a stretched copy that is not ready
    /// is made before this returns; without, the clip gets a stand-in and
    /// the copy is made in the background. Fails only if the file cannot be
    /// read at all.
    ///
    /// `held` belongs to one graph being compiled: the copies it has already
    /// given a stand-in for. Every later clip of that graph waiting for the
    /// same copy gets the stand-in too, even if the worker finished in the
    /// meantime. Without it the pieces of a split clip could come out half
    /// stand-in and half stretched, with an audible seam between them.
    pub(crate) fn clip_audio(
        &self,
        content: &AudioContent,
        tempo: f64,
        sample_rate: f64,
        wait: bool,
        held: &mut HashSet<StretchKey>,
    ) -> Result<ClipAudio, String> {
        let path = Path::new(&content.path);
        let time_ratio = content.time_ratio(tempo);
        let load = || self.media.audio_at_rate(path, sample_rate).map_err(|error| error.to_string());
        // The file itself, read at whatever speed keeps the clip in time.
        let plain =
            |data: Arc<AudioBuffer>, key: Option<StretchKey>, stand_in: bool, problem: Option<String>| ClipAudio {
                frames_per_second: data.sample_rate,
                step: data.sample_rate / sample_rate / time_ratio,
                data,
                key,
                stand_in,
                problem,
            };
        let Some(options) = stretch_options(content, time_ratio) else {
            return Ok(plain(load()?, None, false, None));
        };

        // The stretcher keeps ratios within what sounds like the recording.
        // Beyond that the copy is read faster or slower to make up the
        // difference, which keeps the clip in time at any tempo.
        let stretched = |data: Arc<AudioBuffer>, key: StretchKey| ClipAudio {
            frames_per_second: data.sample_rate * options.time_ratio,
            step: data.sample_rate / sample_rate * options.time_ratio / time_ratio,
            data,
            key: Some(key),
            stand_in: false,
            problem: None,
        };
        let failed = || Some(format!("'{}' could not be time-stretched and plays repitched instead", content.path));
        let key = StretchKey::new(path, FileStamp::of(path), sample_rate, &options);
        if held.contains(&key) {
            return Ok(plain(load()?, Some(key), true, None));
        }
        match self.stretcher.lookup(&key) {
            Lookup::Ready(data) => Ok(stretched(data, key)),
            Lookup::Failed => Ok(plain(load()?, Some(key), false, failed())),
            found => {
                let source = load()?;
                if wait {
                    return Ok(match self.stretcher.prepare_now(&key, &source) {
                        Some(data) => stretched(data, key),
                        None => plain(source, Some(key), false, failed()),
                    });
                }
                if matches!(found, Lookup::Absent) {
                    self.stretcher.request(key.clone(), source.clone());
                }
                held.insert(key.clone());
                Ok(plain(source, Some(key), true, None))
            }
        }
    }

    /// Tells the stretcher which copies the live graph uses or waits for.
    pub(crate) fn set_wanted(&self, wanted: HashSet<StretchKey>) {
        self.stretcher.set_wanted(wanted);
    }

    /// See [`Stretcher::finished`].
    pub(crate) fn finished(&self) -> u64 {
        self.stretcher.finished()
    }

    /// See [`Stretcher::wait_for_progress`].
    pub(crate) fn wait_for_progress(&self, seen: u64, deadline: std::time::Instant) -> bool {
        self.stretcher.wait_for_progress(seen, deadline)
    }
}

/// How the file of `content` has to be stretched to play as the clip asks,
/// or `None` if it plays as it is.
fn stretch_options(content: &AudioContent, time_ratio: f64) -> Option<StretchOptions> {
    let mode = match content.warp {
        WarpMode::Off => return None,
        WarpMode::Repitch => StretchMode::Repitch,
        WarpMode::Beats => StretchMode::Beats,
        WarpMode::Tonal => StretchMode::Tonal,
    };
    let options = StretchOptions {
        time_ratio: time_ratio.clamp(MIN_TIME_RATIO, MAX_TIME_RATIO),
        pitch_semitones: content.pitch_shift(),
        mode,
    };
    (options.time_ratio != 1.0 || options.pitch_semitones != 0.0).then_some(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use trill_media::{WavFormat, write_wav};

    const SR: f64 = 48_000.0;

    /// Two seconds of tone at 44.1 kHz, so that loading converts it.
    fn file(folder: &Path) -> String {
        let channel: Vec<f32> = (0..88_200).map(|n| (n as f32 * 0.03).sin() * 0.5).collect();
        let buffer = AudioBuffer { sample_rate: 44_100.0, left: channel.clone(), right: channel };
        let path = folder.join("tone 100bpm.wav");
        write_wav(&path, &buffer, WavFormat::Float32).unwrap();
        path.display().to_string()
    }

    fn content(path: &str, warp: WarpMode, source_tempo: Option<f64>, transpose: f64) -> AudioContent {
        AudioContent { warp, source_tempo, transpose, ..AudioContent::new(path) }
    }

    #[test]
    fn unwarped_clips_play_the_converted_file_one_frame_per_frame() {
        let folder = std::env::temp_dir().join(format!("trill-sources-plain-{}", std::process::id()));
        let path = file(&folder);
        let sources = AudioSources::default();
        for clip in [
            content(&path, WarpMode::Off, None, 0.0),
            // Off ignores a known tempo and a transposition.
            content(&path, WarpMode::Off, Some(100.0), 5.0),
            // Warped to the tempo it already has: nothing to do.
            content(&path, WarpMode::Beats, Some(120.0), 0.0),
            content(&path, WarpMode::Repitch, None, 7.0),
        ] {
            let audio = sources.clip_audio(&clip, 120.0, SR, false, &mut HashSet::new()).unwrap();
            assert_eq!((audio.data.sample_rate, audio.data.frames()), (SR, 96_000), "converted to the engine's rate");
            assert_eq!((audio.step, audio.frames_per_second), (1.0, SR));
            assert!(audio.key.is_none() && !audio.stand_in && audio.problem.is_none());
        }
        assert!(
            sources
                .clip_audio(
                    &content("no/such.wav", WarpMode::Beats, Some(100.0), 0.0),
                    120.0,
                    SR,
                    true,
                    &mut HashSet::new()
                )
                .is_err()
        );
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn warped_clips_get_a_stand_in_and_then_a_stretched_copy() {
        let folder = std::env::temp_dir().join(format!("trill-sources-warp-{}", std::process::id()));
        let path = file(&folder);
        let sources = AudioSources::default();
        let clip = content(&path, WarpMode::Tonal, Some(100.0), 0.0);

        // Live: the file itself for now, read 1.2 times as fast.
        let mut first_graph = HashSet::new();
        let first = sources.clip_audio(&clip, 120.0, SR, false, &mut first_graph).unwrap();
        assert!(first.stand_in && first.key.is_some());
        assert!((first.step - 1.2).abs() < 1e-12 && first.frames_per_second == SR);
        assert_eq!(first.data.frames(), 96_000);

        // Once the worker is done: a copy 100/120 as long, read 1:1.
        let deadline = Instant::now() + Duration::from_secs(30);
        assert!(sources.wait_for_progress(0, deadline));
        // A graph that already gave one clip the stand-in keeps to it, so
        // the pieces of a split clip never differ within one graph...
        let same_graph = sources.clip_audio(&clip, 120.0, SR, false, &mut first_graph).unwrap();
        assert!(same_graph.stand_in && same_graph.data.frames() == 96_000);
        // ...and the next graph gets the copy.
        let ready = sources.clip_audio(&clip, 120.0, SR, false, &mut HashSet::new()).unwrap();
        assert!(!ready.stand_in && ready.key == first.key);
        assert_eq!(ready.data.frames(), 80_000);
        assert_eq!(ready.step, 1.0);
        assert!((ready.frames_per_second - SR * 100.0 / 120.0).abs() < 1e-6, "a second of file is 40000 frames of it");

        // A render waits instead, and at another rate gets another copy.
        let rendered = sources.clip_audio(&clip, 120.0, 44_100.0, true, &mut HashSet::new()).unwrap();
        assert!(!rendered.stand_in);
        assert_eq!((rendered.data.sample_rate, rendered.data.frames()), (44_100.0, 73_500));
        assert_eq!(sources.finished(), 2);

        // Far outside what the stretcher does, the rest is made up in speed.
        let extreme = content(&path, WarpMode::Beats, Some(600.0), 0.0);
        let audio = sources.clip_audio(&extreme, 60.0, SR, true, &mut HashSet::new()).unwrap();
        assert_eq!(audio.data.frames(), 96_000 * 4, "stretched as far as it goes (four times)");
        assert!((audio.step - 0.4).abs() < 1e-12, "and read slowly enough to last ten times as long");
        std::fs::remove_dir_all(&folder).ok();
    }
}
