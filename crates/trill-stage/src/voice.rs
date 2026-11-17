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
    /// How much of the recording a beat runs through, in seconds.
    pub seconds_per_beat: f64,
    /// What it is played at, as a factor.
    pub gain: f32,
}

impl Voiced {
    /// How far the recording swings each way between two beats of the
    /// timeline, as it is played there.
    pub fn between(&self, from: Beats, to: Beats) -> (f32, f32) {
        if to <= self.start || from >= self.end {
            return (0.0, 0.0);
        }
        let second =
            |beat: Beats| self.enters + (beat.clamp(self.start, self.end) - self.start) * self.seconds_per_beat;
        let (low, high) = self.wave.between(second(from), second(to));
        (low * self.gain, high * self.gain)
    }
}

/// What each track of `project` plays of recordings, where `wave` has the
/// outline of a file: audio clips, and the notes of the built-in sampler.
/// Muted clips and tracks play nothing; a file `wave` does not know is
/// left out.
pub fn voiced(project: &Project, mut wave: impl FnMut(&str) -> Option<Arc<Wave>>) -> Vec<(TrackId, Vec<Voiced>)> {
    let mut found = Vec::new();
    for track in project.tracks.iter().filter(|track| track.kind == TrackKind::Normal && !track.mute) {
        let mut spans = Vec::new();
        for clip in track.clips.iter().filter(|clip| !clip.muted) {
            let Some(audio) = clip.audio() else { continue };
            let Some(wave) = wave(&audio.path) else { continue };
            spans.push(Voiced {
                start: clip.start,
                end: clip.end(),
                wave,
                enters: audio.offset_seconds,
                seconds_per_beat: audio.seconds_per_beat(project.tempo),
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
                        seconds_per_beat: 60.0 / project.tempo * pace,
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
    use trill_core::project::AudioContent;
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
        assert_eq!((*id, span.start, span.end), (track, 8.0, 12.0));
        // Half a second a beat at 120; the file is silent until half a
        // second in, which is half a beat after the clip enters it.
        assert_eq!(span.between(8.0, 8.4), (0.0, 0.0));
        let (_, high) = span.between(8.6, 8.8);
        assert!((high - 0.501).abs() < 0.01, "at -6 dB: {high}");
        assert_eq!(span.between(2.0, 8.0), (0.0, 0.0), "nothing before the clip");
        assert_eq!(span.between(12.0, 20.0), (0.0, 0.0));
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
        assert_eq!((spans[0].start, spans[0].end, spans[0].seconds_per_beat), (4.0, 6.0, 1.0));
        // At the root the file's second half begins half a beat in; an
        // octave up, a quarter of a beat in.
        assert_eq!(spans[0].between(4.0, 4.4), (0.0, 0.0));
        assert_eq!(spans[0].between(4.6, 4.8), (0.0, 0.8));
        assert_eq!((spans[1].start, spans[1].seconds_per_beat), (8.0, 2.0));
        assert_eq!(spans[1].between(8.3, 8.4), (0.0, 0.8));
        // The file is a second long: after that a held note is silent.
        assert_eq!(spans[0].between(5.5, 6.0), (0.0, 0.0));
    }
}
