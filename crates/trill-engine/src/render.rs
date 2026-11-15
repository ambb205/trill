//! Offline rendering: the same processor as realtime playback, run as fast
//! as the CPU allows.

use crate::analysis::{AudioStats, analyze};
use crate::audio_file::{AudioCache, AudioFileError, write_wav};
use crate::compile::{Compiler, DeviceBank, Meters, Problem};
use crate::graph::MAX_BLOCK;
use crate::processor::Processor;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use trill_core::device::DeviceFactory;
use trill_core::{Project, TrackId};

/// Output below this level counts as silence when trimming the tail.
const SILENCE: f32 = 1e-5;
const SILENCE_SECONDS: f64 = 0.25;

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub sample_rate: u32,
    pub start_beat: f64,
    /// Defaults to the end of the last clip.
    pub end_beat: Option<f64>,
    /// Longest reverb/delay tail to keep after the end. The render stops
    /// early once the output has gone silent.
    pub max_tail_seconds: f64,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { sample_rate: 48_000, start_beat: 0.0, end_beat: None, max_tail_seconds: 10.0 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("nothing to render: the range {start}..{end} beats is empty")]
    EmptyRange { start: f64, end: f64 },
    #[error(transparent)]
    File(#[from] AudioFileError),
}

pub struct Rendered {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Post-fader peak of each track over the whole render, linear.
    pub track_peaks: Vec<(TrackId, f32)>,
    pub problems: Vec<Problem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TrackLevel {
    pub track: TrackId,
    pub name: String,
    /// Post-fader peak, dBFS.
    pub peak_db: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct RenderReport {
    pub path: String,
    pub sample_rate: u32,
    #[serde(flatten)]
    pub stats: AudioStats,
    pub tracks: Vec<TrackLevel>,
    pub problems: Vec<String>,
}

/// A snapshot of the project that can be rendered without holding on to the
/// session, so a long bounce does not block editing.
pub struct RenderJob {
    pub project: Project,
    pub factory: Arc<dyn DeviceFactory>,
}

impl RenderJob {
    pub fn render(&self, options: &RenderOptions) -> Result<Rendered, RenderError> {
        render(&self.project, self.factory.as_ref(), options)
    }

    /// Renders to a WAV file and measures the result.
    pub fn render_to_file(&self, path: &Path, options: &RenderOptions) -> Result<RenderReport, RenderError> {
        let rendered = self.render(options)?;
        write_wav(path, rendered.sample_rate, &rendered.left, &rendered.right)?;
        let stats = analyze(&rendered.left, &rendered.right, rendered.sample_rate);
        let tracks = rendered
            .track_peaks
            .iter()
            .map(|(id, peak)| TrackLevel {
                track: *id,
                name: self.project.track(*id).map(|t| t.name.clone()).unwrap_or_default(),
                peak_db: if *peak > 0.0 { (20.0 * peak.log10()).max(-120.0) } else { -120.0 },
            })
            .collect();
        Ok(RenderReport {
            path: path.display().to_string(),
            sample_rate: rendered.sample_rate,
            stats,
            tracks,
            problems: rendered.problems.iter().map(ToString::to_string).collect(),
        })
    }
}

/// Renders `project` with freshly created devices.
pub fn render(
    project: &Project,
    factory: &dyn DeviceFactory,
    options: &RenderOptions,
) -> Result<Rendered, RenderError> {
    let start = options.start_beat.max(0.0);
    let end = options.end_beat.unwrap_or_else(|| project.length_beats());
    // Also rejects NaN, which compares as neither greater nor less.
    if end.partial_cmp(&start) != Some(std::cmp::Ordering::Greater) {
        return Err(RenderError::EmptyRange { start, end });
    }
    let sample_rate = options.sample_rate as f64;

    let mut bank = DeviceBank::default();
    let mut meters = Meters::default();
    let mut audio = AudioCache::default();
    let (graph, problems) = Compiler { factory, bank: &mut bank, meters: &mut meters, audio: &mut audio, sample_rate }
        .compile(project, false);

    let mut processor = Processor::new(sample_rate);
    drop(processor.swap_graph(graph));
    processor.seek(start);
    processor.play();

    let body_frames = ((end - start) * 60.0 / project.tempo * sample_rate).round() as usize;
    let tail_frames = (options.max_tail_seconds.max(0.0) * sample_rate) as usize;
    let mut left = vec![0.0f32; body_frames + tail_frames];
    let mut right = vec![0.0f32; body_frames + tail_frames];

    for (l, r) in left[..body_frames].chunks_mut(MAX_BLOCK).zip(right[..body_frames].chunks_mut(MAX_BLOCK)) {
        processor.process(l, r);
    }

    // Let the tail ring out, stopping once it has been silent for a while.
    processor.stop();
    let silence_frames = (SILENCE_SECONDS * sample_rate) as usize;
    let mut rendered = body_frames;
    let mut quiet = 0;
    while rendered < left.len() {
        let frames = (left.len() - rendered).min(MAX_BLOCK);
        let (l, r) = (&mut left[rendered..rendered + frames], &mut right[rendered..rendered + frames]);
        processor.process(l, r);
        rendered += frames;
        let silent = l.iter().chain(r.iter()).all(|sample| sample.abs() < SILENCE);
        quiet = if silent { quiet + frames } else { 0 };
        if quiet >= silence_frames {
            rendered -= quiet;
            break;
        }
    }
    left.truncate(rendered);
    right.truncate(rendered);

    let track_peaks = project
        .tracks
        .iter()
        .filter_map(|track| meters.tracks.get(&track.id).map(|meter| (track.id, meter)))
        .map(|(id, meter)| {
            let (l, r) = meter.peek();
            (id, l.max(r))
        })
        .collect();

    Ok(Rendered { sample_rate: options.sample_rate, left, right, track_peaks, problems })
}
