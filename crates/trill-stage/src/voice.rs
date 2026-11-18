//! Recordings on the stage: the outline of what an audio clip or a sampler
//! plays, so that a voice has a picture as a note has.
//!
//! A part that plays a recording shows it as a waveform along its lane:
//! where it swings wide the singer is loud. Two things play recordings in
//! a song: an audio clip, which enters its file somewhere and runs through
//! it at its own pace, and the built-in sampler, which plays its one file
//! from the start for every note, faster or slower by how far the note is
//! from the sampler's root.
//!
//! What a project says of this is read by [`voiced`], a pure function of
//! the project and of the outlines it is handed. The outlines come from
//! the files, which is the one part of a frame that is not the song's own:
//! [`Waves`] reads them once and keeps them.

use crate::score::Time;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_core::{Beats, Project, Track, TrackId, TrackKind};

/// Outline values kept per second of a recording.
const PER_SECOND: f64 = 400.0;
/// The built-in device that plays one recording for every note.
const SAMPLER: &str = "builtin:sampler";

/// The outline of a recording: how far it swings each way, so many times
/// a second.
#[derive(Clone, Debug, PartialEq)]
pub struct Wave {
    per_second: f64,
    /// The lowest and highest of the two channels' mean in each step.
    swings: Vec<(f32, f32)>,
}

impl Wave {
    /// The outline of `left` and `right` at `sample_rate`.
    pub fn of(left: &[f32], right: &[f32], sample_rate: f64) -> Self {
        let frames = left.len().min(right.len());
        let step = (sample_rate / PER_SECOND).max(1.0);
        let count = (frames as f64 / step).ceil() as usize;
        let mut swings = Vec::with_capacity(count);
        for index in 0..count {
            let (first, last) = ((index as f64 * step) as usize, (((index + 1) as f64 * step) as usize).min(frames));
            let mut swing = (0.0f32, 0.0f32);
            for frame in first..last {
                let mono = (left[frame] + right[frame]) / 2.0;
                if mono.is_finite() {
                    swing = (swing.0.min(mono), swing.1.max(mono));
                }
            }
            swings.push(swing);
        }
        Self { per_second: sample_rate / step, swings }
    }

    /// How long the recording is, in seconds.
    pub fn seconds(&self) -> f64 {
        self.swings.len() as f64 / self.per_second
    }

    /// How far the recording swings each way between `from` and `to`
    /// seconds: nothing outside it.
    pub fn between(&self, from: f64, to: f64) -> (f32, f32) {
        if to <= 0.0 || from >= self.seconds() {
            return (0.0, 0.0);
        }
        let first = ((from.max(0.0) * self.per_second) as usize).min(self.swings.len() - 1);
        let last = ((to * self.per_second).ceil() as usize).clamp(first + 1, self.swings.len());
        self.swings[first..last].iter().fold((0.0f32, 0.0f32), |all, swing| (all.0.min(swing.0), all.1.max(swing.1)))
    }
}

/// A stretch of a lane that plays a stretch of a recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Voiced {
    /// Where on the timeline it plays.
    pub start: Beats,
    pub end: Beats,
    pub wave: Arc<Wave>,
    /// Where in the recording it enters, in seconds.
    pub enters: f64,
    /// How the recording runs against the song from there.
    pub pace: Pace,
    /// What it is played at, as a factor.
    pub gain: f32,
}

/// How a recording runs against the song that plays it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Pace {
    /// At a speed of its own, whatever the song's tempo does: so many
    /// seconds of it to a second of the song. A clip that does not follow
    /// the tempo (1), a sample played as a note (faster the higher).
    Time(f64),
    /// With the song's beats: so many seconds of it to a beat. A clip that
    /// follows the tempo, which a faster song runs through faster.
    Beat(f64),
}

impl Voiced {
    /// How far the recording swings each way between two beats of the
    /// timeline, as it is played there.
    pub fn between(&self, from: Beats, to: Beats, time: &Time) -> (f32, f32) {
        if to <= self.start || from >= self.end {
            return (0.0, 0.0);
        }
        let second = |beat: Beats| {
            let beat = beat.clamp(self.start, self.end);
            self.enters
                + match self.pace {
                    Pace::Time(rate) => time.between(self.start, beat) * rate,
                    Pace::Beat(seconds) => (beat - self.start) * seconds,
                }
        };
        let (low, high) = self.wave.between(second(from), second(to));
        (low * self.gain, high * self.gain)
    }
}

/// Whether a track's recordings are in a film: a muted track plays
/// nothing, and a reference is beside the song.
fn heard(track: &trill_core::Track) -> bool {
    track.kind == TrackKind::Normal && !track.mute && track.reference.is_none()
}

/// What each track of `project` plays of recordings, where `wave` has the
/// outline of a file: audio clips, and the notes of the built-in sampler.
/// Muted clips and tracks play nothing; a file `wave` does not know is
/// left out.
pub fn voiced(project: &Project, mut wave: impl FnMut(&str) -> Option<Arc<Wave>>) -> Vec<(TrackId, Vec<Voiced>)> {
    let mut found = Vec::new();
    for track in project.tracks.iter().filter(|track| heard(track)) {
        let mut spans = Vec::new();
        for clip in track.clips.iter().filter(|clip| !clip.muted) {
            let Some(audio) = clip.audio() else { continue };
            let Some(wave) = wave(&audio.path) else { continue };
            spans.push(Voiced {
                start: clip.start,
                end: clip.end(),
                wave,
                enters: audio.offset_seconds,
                pace: match audio.followed_tempo() {
                    Some(recorded) => Pace::Beat(60.0 / recorded),
                    None => Pace::Time(1.0),
                },
                gain: 10f32.powf(audio.gain_db / 20.0),
            });
        }
        if let Some((path, root)) = sampler_of(track)
            && let Some(wave) = wave(&path)
        {
            for clip in track.clips.iter().filter(|clip| !clip.muted) {
                for note in clip.timeline_notes() {
                    // An octave up runs through the recording twice as fast.
                    let pace = 2f64.powf((f64::from(note.pitch) - root) / 12.0);
                    spans.push(Voiced {
                        start: note.start,
                        end: note.end(),
                        wave: wave.clone(),
                        enters: 0.0,
                        pace: Pace::Time(pace),
                        gain: f32::from(note.velocity) / 127.0,
                    });
                }
            }
        }
        if !spans.is_empty() {
            spans.sort_by(|a, b| a.start.total_cmp(&b.start));
            found.push((track.id, spans));
        }
    }
    found
}

/// The file the sampler of `track` plays and the note it plays it at as it
/// was recorded, if the track plays through one.
fn sampler_of(track: &Track) -> Option<(String, f64)> {
    let device = track.devices.iter().find(|device| device.enabled && device.uri == SAMPLER)?;
    let file = trill_dsp::sample_refs(device).into_iter().next()?;
    Some((file.path, device.params.get("root").copied().unwrap_or(60.0)))
}

/// Whether `project` plays any recording at all: an audio clip, or notes
/// through the built-in sampler. Reading their outlines is only worth a
/// thread for a song that does.
pub fn plays_recordings(project: &Project) -> bool {
    let heard = project.tracks.iter().filter(|track| heard(track));
    heard.into_iter().any(|track| track.clips.iter().any(|clip| clip.audio().is_some()) || sampler_of(track).is_some())
}

/// The outlines of the files a song plays, read once each.
#[derive(Default)]
pub struct Waves {
    read: HashMap<String, Option<Arc<Wave>>>,
}

impl Waves {
    /// The outline of the file at `path`, read the first time it is asked
    /// for. `None` for a file that cannot be read: its part is then shown
    /// by its notes alone.
    pub fn of(&mut self, path: &str) -> Option<Arc<Wave>> {
        if let Some(known) = self.read.get(path) {
            return known.clone();
        }
        let wave = match trill_media::decode(Path::new(path)) {
            Ok(AudioBuffer { sample_rate, left, right }) => Some(Arc::new(Wave::of(&left, &right, sample_rate))),
            Err(error) => {
                log::warn!("the stage shows no waveform for {path}: {error}");
                None
            }
        };
        self.read.insert(path.to_string(), wave.clone());
        wave
    }

    /// Every recording `project` plays, as [`voiced`] gives them.
    pub fn voiced(&mut self, project: &Project) -> Vec<(TrackId, Vec<Voiced>)> {
        voiced(project, |path| self.of(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::project::{AudioContent, WarpMode};
    use trill_core::{ChainRef, Note};

    /// One second at 8 kHz: silent, then a swing of `high` from the middle on.
    fn recording(high: f32) -> Arc<Wave> {
        let samples: Vec<f32> = (0..8000).map(|frame| if frame < 4000 { 0.0 } else { high }).collect();
        Arc::new(Wave::of(&samples, &samples, 8000.0))
    }

    #[test]
    fn an_outline_says_how_far_a_recording_swings_in_any_stretch() {
        let wave = recording(0.5);
        assert!((wave.seconds() - 1.0).abs() < 1e-9);
        assert_eq!(wave.between(0.0, 0.4), (0.0, 0.0));
        assert_eq!(wave.between(0.6, 0.7), (0.0, 0.5));
        assert_eq!(wave.between(0.45, 0.55), (0.0, 0.5), "a stretch holds the most of what is in it");
        assert_eq!(wave.between(-3.0, -1.0), (0.0, 0.0));
        assert_eq!(wave.between(1.5, 2.0), (0.0, 0.0), "past its end there is nothing");
        // Both ways: a recording that swings below the line as well.
        let both = Wave::of(&[0.2, -0.6, 0.1], &[0.2, -0.6, 0.1], 3.0);
        assert_eq!(both.between(0.0, 1.0), (-0.6, 0.2));
    }

    #[test]
    fn an_audio_clip_plays_its_file_from_where_it_enters_it_at_its_own_pace() {
        let mut project = Project::new("song");
        project.set_tempo(120.0).unwrap();
        let track = project.add_track("Voice");
        let content = AudioContent { offset_seconds: 0.25, gain_db: -6.0, ..AudioContent::new("take.wav") };
        project.add_audio_clip(track, "Take", 8.0, 4.0, content).unwrap();
        let found = voiced(&project, |path| (path == "take.wav").then(|| recording(1.0)));
        let [(id, spans)] = found.as_slice() else { panic!("{found:?}") };
        let [span] = spans.as_slice() else { panic!("{spans:?}") };
        let time = Time::of(&project);
        assert_eq!((*id, span.start, span.end), (track, 8.0, 12.0));
        // Half a second a beat at 120; the file is silent until half a
        // second in, which is half a beat after the clip enters it.
        assert_eq!(span.between(8.0, 8.4, &time), (0.0, 0.0));
        let (_, high) = span.between(8.6, 8.8, &time);
        assert!((high - 0.501).abs() < 0.01, "at -6 dB: {high}");
        assert_eq!(span.between(2.0, 8.0, &time), (0.0, 0.0), "nothing before the clip");
        assert_eq!(span.between(12.0, 20.0, &time), (0.0, 0.0));
        // A file nobody could read leaves the track without a picture.
        assert!(voiced(&project, |_| None).is_empty());
        project.track_mut(track).unwrap().mute = true;
        assert!(voiced(&project, |_| Some(recording(1.0))).is_empty(), "and so does a mute");
    }

    #[test]
    fn a_sampler_plays_its_file_from_the_start_for_every_note_faster_the_higher() {
        let mut project = Project::new("song");
        project.set_tempo(60.0).unwrap();
        let track = project.add_track("Shout");
        let sampler = project.add_device(ChainRef::Track(track), SAMPLER, "Sampler", None).unwrap();
        // The sampler's own state names its file, as the device writes it.
        let state = br#"{"version": 1, "path": "hey.wav"}"#.to_vec();
        project.device_mut(sampler).unwrap().state = Some(state);
        let clip = project.add_midi_clip(track, "shouts", 4.0, 8.0).unwrap();
        let notes = [
            Note { pitch: 60, start: 0.0, length: 2.0, velocity: 127 },
            Note { pitch: 72, start: 4.0, length: 2.0, velocity: 127 },
        ];
        project.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
        let found = voiced(&project, |path| (path == "hey.wav").then(|| recording(0.8)));
        let spans = &found[0].1;
        assert_eq!(spans.len(), 2, "one stretch for each note");
        let time = Time::of(&project);
        assert_eq!((spans[0].start, spans[0].end, spans[0].pace), (4.0, 6.0, Pace::Time(1.0)));
        // At the root the file's second half begins half a beat in; an
        // octave up, a quarter of a beat in.
        assert_eq!(spans[0].between(4.0, 4.4, &time), (0.0, 0.0));
        assert_eq!(spans[0].between(4.6, 4.8, &time), (0.0, 0.8));
        assert_eq!((spans[1].start, spans[1].pace), (8.0, Pace::Time(2.0)));
        assert_eq!(spans[1].between(8.3, 8.4, &time), (0.0, 0.8));
        // The file is a second long: after that a held note is silent.
        assert_eq!(spans[0].between(5.5, 6.0, &time), (0.0, 0.0));
    }

    #[test]
    fn a_recording_at_its_own_speed_keeps_it_through_a_change_of_tempo_and_one_that_follows_does_not() {
        // 60, and from beat 4 on twice as fast: a second a beat, then half.
        let mut project = Project::new("song");
        project.set_tempo(60.0).unwrap();
        project.set_tempo_at(4.0, 120.0, false).unwrap();
        let track = project.add_track("Voice");
        project.add_audio_clip(track, "Take", 3.0, 4.0, AudioContent::new("take.wav")).unwrap();
        let followed =
            AudioContent { source_tempo: Some(60.0), warp: WarpMode::Beats, ..AudioContent::new("loop.wav") };
        project.add_audio_clip(track, "Loop", 8.0, 4.0, followed).unwrap();
        // Two seconds: silent for the first, then sounding.
        let two: Vec<f32> = (0..16_000).map(|frame| if frame < 8000 { 0.0 } else { 0.5 }).collect();
        let wave = Arc::new(Wave::of(&two, &two, 8000.0));
        let found = voiced(&project, |_| Some(wave.clone()));
        let spans = &found[0].1;
        let time = Time::of(&project);
        assert_eq!((spans[0].pace, spans[1].pace), (Pace::Time(1.0), Pace::Beat(1.0)));
        // The take enters at beat 3, a second before the change: its second
        // second begins at beat 4, and lasts two beats of the faster song.
        assert_eq!(spans[0].between(3.0, 3.9, &time), (0.0, 0.0));
        assert_eq!(spans[0].between(4.1, 5.9, &time), (0.0, 0.5));
        assert_eq!(spans[0].between(6.1, 7.0, &time), (0.0, 0.0), "the file is over two seconds after it began");
        // The loop follows the tempo: a beat of it to a beat of the song.
        assert_eq!(spans[1].between(8.0, 8.9, &time), (0.0, 0.0));
        assert_eq!(spans[1].between(9.1, 9.9, &time), (0.0, 0.5));
    }
}
