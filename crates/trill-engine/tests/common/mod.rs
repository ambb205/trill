//! Fixtures and measurements shared by the audio tests: files with known
//! content, and ways of telling what came out without listening to it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::audio::AudioBuffer;
use trill_core::project::{AudioContent, WarpMode};
use trill_core::{ClipId, Project, TrackId};
use trill_dsp::BuiltinFactory;
use trill_engine::compile::{Compiler, DeviceBank, Meters, StandIns};
use trill_engine::{AudioSources, Problem, Processor};
use trill_media::{MediaCache, WavFormat};

pub const SR: f64 = 48_000.0;

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |max, sample| max.max(sample.abs()))
}

pub fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// The largest jump between neighbouring samples: what a click looks like.
pub fn largest_step(samples: &[f32]) -> f32 {
    samples.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max)
}

/// Frequency of a tone, from the spacing of its rising zero crossings (each
/// located by linear interpolation).
pub fn zero_crossing_hz(samples: &[f32], sample_rate: f64) -> f64 {
    let crossings: Vec<f64> = samples
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[0] <= 0.0 && pair[1] > 0.0)
        .map(|(i, pair)| i as f64 + (-pair[0] / (pair[1] - pair[0])) as f64)
        .collect();
    match (crossings.first(), crossings.last()) {
        (Some(first), Some(last)) if crossings.len() > 1 => (crossings.len() - 1) as f64 * sample_rate / (last - first),
        _ => 0.0,
    }
}

/// The frame after the last one louder than `threshold`.
pub fn end_of_sound(samples: &[f32], threshold: f32) -> usize {
    samples.iter().rposition(|sample| sample.abs() > threshold).map_or(0, |last| last + 1)
}

pub fn sine(hz: f64, sample_rate: f64, seconds: f64, level: f64) -> Vec<f32> {
    let frames = (seconds * sample_rate).round() as usize;
    (0..frames).map(|n| (level * (std::f64::consts::TAU * hz * n as f64 / sample_rate).sin()) as f32).collect()
}

/// A click track at `tempo`: `beats` beats long, with a click half way
/// through every beat, so that none sits on an edge of the file where a
/// clip's fade would soften it. Each click is a few samples wide, like the
/// leading edge of a rimshot.
pub fn click_track(tempo: f64, beats: usize, sample_rate: f64) -> Vec<f32> {
    let beat = 60.0 / tempo * sample_rate;
    let mut signal = vec![0.0f32; (beats as f64 * beat).round() as usize];
    for index in 0..beats {
        let at = ((index as f64 + 0.5) * beat).round() as usize;
        for (offset, value) in [0.9f32, -0.7, 0.4, -0.2].into_iter().enumerate() {
            signal[at + offset] = value;
        }
    }
    signal
}

/// Where the clicks of `samples` are, in seconds: the loudest sample of
/// every burst above `threshold`, bursts being at least 20 ms apart.
pub fn click_times(samples: &[f32], sample_rate: f64, threshold: f32) -> Vec<f64> {
    let gap = (0.02 * sample_rate) as usize;
    let mut bursts: Vec<(usize, usize)> = Vec::new();
    for (frame, sample) in samples.iter().enumerate() {
        if sample.abs() > threshold {
            match bursts.last_mut() {
                Some(burst) if frame - burst.1 <= gap => burst.1 = frame,
                _ => bursts.push((frame, frame)),
            }
        }
    }
    bursts
        .into_iter()
        .map(|(first, last)| {
            let loudest =
                (first..=last).max_by(|a, b| samples[*a].abs().total_cmp(&samples[*b].abs())).unwrap_or(first);
            loudest as f64 / sample_rate
        })
        .collect()
}

/// Asserts that there is exactly one click on every half beat of the first
/// `beats` beats at `tempo`, each within `tolerance_ms` of where it belongs,
/// and returns the worst error in milliseconds.
pub fn assert_clicks_on_the_beat(
    samples: &[f32],
    sample_rate: f64,
    tempo: f64,
    beats: usize,
    tolerance_ms: f64,
) -> f64 {
    let found = click_times(samples, sample_rate, 0.2);
    let expected: Vec<f64> = (0..beats).map(|index| (index as f64 + 0.5) * 60.0 / tempo).collect();
    assert_eq!(found.len(), expected.len(), "clicks at {found:?}, wanted them at {expected:?}");
    let worst =
        found.iter().zip(&expected).map(|(found, expected)| (found - expected).abs() * 1000.0).fold(0.0, f64::max);
    assert!(worst <= tolerance_ms, "a click is {worst:.3} ms off its beat: {found:?} against {expected:?}");
    worst
}

/// Writes a mono signal as a 32-bit float WAV file and returns its path as
/// the project stores it.
pub fn write_wav(path: &Path, sample_rate: f64, samples: &[f32]) -> String {
    let buffer = AudioBuffer { sample_rate, left: samples.to_vec(), right: samples.to_vec() };
    trill_media::write_wav(path, &buffer, WavFormat::Float32).unwrap();
    path.display().to_string()
}

/// Converts `source` to `target` with ffmpeg; `false` if ffmpeg is missing
/// or cannot make that format, in which case the caller skips its check.
pub fn ffmpeg(source: &Path, target: &Path, options: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(source)
        .args(options)
        .arg(target)
        .output()
        .is_ok_and(|output| output.status.success() && target.exists())
}

/// A project at `tempo` with one empty audio track.
pub fn audio_project(tempo: f64) -> (Project, TrackId) {
    let mut project = Project::new("t");
    project.set_tempo(tempo).unwrap();
    let track = project.add_track("Audio");
    (project, track)
}

/// Puts `path` on `track` from beat `start` for `length` beats.
pub fn add_clip(project: &mut Project, track: TrackId, path: &str, start: f64, length: f64) -> ClipId {
    project.add_audio_clip(track, "clip", start, length, AudioContent::new(path)).unwrap()
}

/// The same, warped: recorded at `source_tempo`, following the project.
pub fn add_warped(
    project: &mut Project,
    track: TrackId,
    path: &str,
    length: f64,
    warp: WarpMode,
    source_tempo: f64,
) -> ClipId {
    let clip = add_clip(project, track, path, 0.0, length);
    project.set_clip_warp(clip, warp, Some(source_tempo), 0.0).unwrap();
    clip
}

/// A graph played the way the session plays one live, but with the samples
/// in hand: the compiler in its live mode (stand-ins allowed), a processor,
/// and a swap after every compile. Only for projects without devices, whose
/// graphs can be thrown away freely.
pub struct LiveRig {
    bank: DeviceBank,
    meters: Meters,
    stand_ins: StandIns,
    audio: AudioSources,
    processor: Processor,
    pub problems: Vec<Problem>,
}

impl LiveRig {
    pub fn new(sample_rate: f64) -> Self {
        let mut processor = Processor::new(sample_rate);
        processor.play();
        Self {
            bank: DeviceBank::default(),
            meters: Meters::default(),
            stand_ins: StandIns::default(),
            audio: AudioSources::new(Arc::new(MediaCache::default())),
            processor,
            problems: Vec::new(),
        }
    }

    /// Compiles `project` and swaps the graph in. Returns how many of its
    /// clips play a stand-in.
    pub fn compile(&mut self, project: &Project) -> usize {
        let (graph, problems) = Compiler {
            factory: &BuiltinFactory,
            bank: &mut self.bank,
            meters: &mut self.meters,
            audio: &self.audio,
            stand_ins: Some(&mut self.stand_ins),
            sample_rate: self.processor.sample_rate(),
            purpose: trill_core::device::Purpose::Playing,
        }
        .compile(project, false);
        self.problems = problems;
        drop(self.processor.swap_graph(graph));
        self.stand_ins.len()
    }

    /// Compiles until no clip plays a stand-in, as the session's
    /// housekeeping does as stretched audio becomes ready.
    pub fn compile_until_prepared(&mut self, project: &Project) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.compile(project) > 0 {
            assert!(Instant::now() < deadline, "the stretched audio never became ready");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Plays on for `frames` frames and returns the left channel.
    pub fn play(&mut self, frames: usize) -> Vec<f32> {
        let (mut left, mut right) = (vec![0.0f32; frames], vec![0.0f32; frames]);
        for (l, r) in left.chunks_mut(441).zip(right.chunks_mut(441)) {
            self.processor.process(l, r);
        }
        left
    }

    /// Moves the playhead to `beat`.
    pub fn seek(&mut self, beat: f64) {
        self.processor.seek(beat);
    }
}

/// A folder for a test's files, removed when the test ends.
pub fn folder() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary folder")
}

pub fn file(folder: &tempfile::TempDir, name: &str) -> PathBuf {
    folder.path().join(name)
}
