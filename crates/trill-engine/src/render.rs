//! Offline rendering: the same processor as realtime playback, run as fast
//! as the CPU allows, and measured as it goes.
//!
//! A render plays freshly created devices. Plugins among them are told
//! that they are being rendered ([`Purpose::Rendering`]) and are waited
//! for until they answer the notes they are about to be given
//! ([`crate::ready`]), so that the first note of the range sounds like
//! every other.
//!
//! What is measured: the mix (level, true peak, loudness, spectral
//! balance, stereo correlation), every track after its fader in the same
//! terms, the loudness and peak of each part of the song the caller names,
//! and what every built-in compressor and limiter took off. All of it is
//! taken as the audio goes by; no track's audio is kept, and the mix's is
//! kept only when it is asked for ([`RenderJob::measure`] keeps none).

use crate::analysis::{Analyzer, AudioStats, Spectrum};
use crate::compile::{Compiler, DeviceBank, Meters, Problem};
use crate::graph::{Chain, Graph, MAX_BLOCK};
use crate::processor::Processor;
use crate::ready::{self, Waited};
use crate::sources::AudioSources;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use trill_core::audio::AudioBuffer;
use trill_core::device::{DeviceFactory, Purpose};
use trill_core::{DeviceId, Project, TrackId};
use trill_media::{MediaError, WavFormat, write_wav};

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
    /// Hear only these tracks, as if they were soloed (and unmuted), with
    /// the song's own mute and solo left as they are. `None` for the song
    /// as it stands.
    pub only_tracks: Option<Vec<TrackId>>,
    /// The parts of the song to measure one by one.
    pub sections: Vec<Section>,
    /// How long a plugin that has notes to play may take to answer them
    /// before the render starts without waiting further. See
    /// [`crate::ready`].
    pub patience: Duration,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            start_beat: 0.0,
            end_beat: None,
            max_tail_seconds: 10.0,
            only_tracks: None,
            sections: Vec::new(),
            patience: Duration::from_secs(10),
        }
    }
}

/// A part of the song to measure on its own: a verse, eight bars.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Section {
    pub name: String,
    pub start_beat: f64,
    pub end_beat: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("nothing to render: the range {start}..{end} beats is empty")]
    EmptyRange { start: f64, end: f64 },
    #[error(transparent)]
    File(#[from] MediaError),
}

pub struct Rendered {
    pub sample_rate: u32,
    /// The mix. Empty when the render was only measured.
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Post-fader peak of each track over the whole render, linear.
    pub track_peaks: Vec<(TrackId, f32)>,
    pub problems: Vec<Problem>,
    /// What was measured as the render went.
    pub measured: Measured,
}

/// What a render measured.
#[derive(Clone, Debug, Serialize)]
pub struct Measured {
    /// The mix.
    #[serde(flatten)]
    pub stats: AudioStats,
    pub tracks: Vec<TrackLevel>,
    pub sections: Vec<SectionLevel>,
    pub dynamics: Vec<DynamicsLevel>,
    /// The plugins that were waited for before the render started, where
    /// the wait is worth knowing about: it took a while, or was in vain.
    pub waited: Vec<WaitedFor>,
}

/// One track as it leaves its fader: what it adds to the mix or to its
/// group. A group's or a return's is what comes out of it.
#[derive(Clone, Debug, Serialize)]
pub struct TrackLevel {
    pub track: TrackId,
    pub name: String,
    /// Post-fader peak, dBFS.
    pub peak_db: f32,
    /// Integrated loudness over the render, LUFS.
    pub loudness_lufs: f32,
    pub rms_db: f32,
    pub stereo_correlation: f32,
    pub spectrum: Spectrum,
    /// The loudness in each of the render's sections, in their order.
    pub section_loudness: Vec<f32>,
}

/// One part of the song, in the mix.
#[derive(Clone, Debug, Serialize)]
pub struct SectionLevel {
    #[serde(flatten)]
    pub section: Section,
    pub loudness_lufs: f32,
    pub peak_db: f32,
}

/// What a compressor or limiter took off over the render, in dB.
#[derive(Clone, Debug, Serialize)]
pub struct DynamicsLevel {
    pub device: DeviceId,
    pub name: String,
    /// The track it is on; `None` for the master.
    pub track: Option<TrackId>,
    /// The most it took off at any moment.
    pub most_db: f32,
    /// What it took off on average while it took off half a dB or more.
    pub typical_db: f32,
    /// The share of the render it spent doing so, 0..=1.
    pub active: f32,
}

/// A plugin that was waited for before the render started.
#[derive(Clone, Debug, Serialize)]
pub struct WaitedFor {
    pub track: TrackId,
    pub track_name: String,
    pub device: DeviceId,
    pub device_name: String,
    pub seconds: f64,
    /// False when it never answered the notes it was to play, and the
    /// render went ahead without it.
    pub answered: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RenderReport {
    /// The file that was written; `None` when the render was only measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub sample_rate: u32,
    /// The mix.
    #[serde(flatten)]
    pub stats: AudioStats,
    pub tracks: Vec<TrackLevel>,
    pub sections: Vec<SectionLevel>,
    pub dynamics: Vec<DynamicsLevel>,
    /// See [`Measured::waited`].
    pub waited: Vec<WaitedFor>,
    pub problems: Vec<String>,
}

impl RenderReport {
    fn new(path: Option<&Path>, sample_rate: u32, measured: Measured, problems: &[Problem]) -> Self {
        let Measured { stats, tracks, sections, dynamics, waited } = measured;
        Self {
            path: path.map(|path| path.display().to_string()),
            sample_rate,
            stats,
            tracks,
            sections,
            dynamics,
            waited,
            problems: problems.iter().map(ToString::to_string).collect(),
        }
    }
}

/// A snapshot of the project that can be rendered without holding on to the
/// session, so a long bounce does not block editing.
pub struct RenderJob {
    pub project: Project,
    pub factory: Arc<dyn DeviceFactory>,
    /// The session's decoded files and stretched audio, so that the render
    /// decodes and stretches nothing the session already has.
    pub audio: AudioSources,
}

impl RenderJob {
    pub fn render(&self, options: &RenderOptions) -> Result<Rendered, RenderError> {
        render_from(&self.project, self.factory.as_ref(), &self.audio, options, true)
    }

    /// Renders to a 24-bit WAV file and measures the result.
    pub fn render_to_file(&self, path: &Path, options: &RenderOptions) -> Result<RenderReport, RenderError> {
        let Rendered { sample_rate, left, right, problems, measured, .. } = self.render(options)?;
        let audio = AudioBuffer { sample_rate: f64::from(sample_rate), left, right };
        write_wav(path, &audio, WavFormat::Int24)?;
        Ok(RenderReport::new(Some(path), sample_rate, measured, &problems))
    }

    /// Renders and measures, and keeps nothing: no file is written and the
    /// audio itself is let go of as it is made. The numbers are the ones
    /// [`RenderJob::render_to_file`] would report.
    pub fn measure(&self, options: &RenderOptions) -> Result<RenderReport, RenderError> {
        let rendered = render_from(&self.project, self.factory.as_ref(), &self.audio, options, false)?;
        Ok(RenderReport::new(None, rendered.sample_rate, rendered.measured, &rendered.problems))
    }
}

/// Renders `project` with freshly created devices, reading its audio files
/// through caches of its own. A session's renders share the session's
/// caches instead: see [`RenderJob`].
pub fn render(
    project: &Project,
    factory: &dyn DeviceFactory,
    options: &RenderOptions,
) -> Result<Rendered, RenderError> {
    render_from(project, factory, &AudioSources::default(), options, true)
}

/// `project` with only `tracks` to be heard: each of them soloed and
/// unmuted, and no other track soloed.
fn hearing_only(project: &Project, tracks: &[TrackId]) -> Project {
    let mut heard = project.clone();
    for track in &mut heard.tracks {
        track.solo = tracks.contains(&track.id);
        track.mute &= !track.solo;
    }
    heard
}

/// Renders `project` with freshly created devices and the audio of `audio`.
/// With `keep` the mix is returned; without, it is only measured.
///
/// A render never plays a stand-in: clips are stretched, or waited for,
/// before the first sample is made, so the same project always renders to
/// the same samples.
fn render_from(
    project: &Project,
    factory: &dyn DeviceFactory,
    audio: &AudioSources,
    options: &RenderOptions,
    keep: bool,
) -> Result<Rendered, RenderError> {
    let start = options.start_beat.max(0.0);
    let end = options.end_beat.unwrap_or_else(|| project.length_beats());
    // Also rejects NaN, which compares as neither greater nor less.
    if end.partial_cmp(&start) != Some(std::cmp::Ordering::Greater) {
        return Err(RenderError::EmptyRange { start, end });
    }
    let sample_rate = options.sample_rate as f64;
    let only = options.only_tracks.as_deref().map(|tracks| hearing_only(project, tracks));
    let project = only.as_ref().unwrap_or(project);

    let mut bank = DeviceBank::default();
    let mut meters = Meters::default();
    let (mut graph, problems) = Compiler {
        factory,
        bank: &mut bank,
        meters: &mut meters,
        audio,
        stand_ins: None,
        sample_rate,
        purpose: Purpose::Rendering,
    }
    .compile(project, false);

    let plugins = ready::Plugins::of(factory);
    let waited = ready::wake(&mut graph, project, &plugins, (start, end), sample_rate, options.patience);
    for track in &mut graph.tracks {
        track.tap = Some(Box::new(Analyzer::new(options.sample_rate)));
    }

    let mut processor = Processor::new(sample_rate);
    drop(processor.swap_graph(graph));
    processor.seek(start);
    processor.play();

    let seconds_per_beat = 60.0 / project.tempo;
    let body_frames = ((end - start) * seconds_per_beat * sample_rate).round() as usize;
    let tail_frames = (options.max_tail_seconds.max(0.0) * sample_rate) as usize;
    let mut mix = Analyzer::new(options.sample_rate).with_true_peak();

    // The body is measured a block at a time, and held on to only if it is
    // wanted. The tail is held until it is known how much of it is sound.
    let kept_frames = if keep { body_frames } else { 0 };
    let mut left = vec![0.0f32; kept_frames + tail_frames];
    let mut right = vec![0.0f32; kept_frames + tail_frames];
    if keep {
        for (l, r) in left[..body_frames].chunks_mut(MAX_BLOCK).zip(right[..body_frames].chunks_mut(MAX_BLOCK)) {
            processor.process(l, r);
            mix.push(l, r);
        }
    } else {
        let (mut l, mut r) = (vec![0.0f32; MAX_BLOCK], vec![0.0f32; MAX_BLOCK]);
        let mut done = 0;
        while done < body_frames {
            let frames = (body_frames - done).min(MAX_BLOCK);
            processor.process(&mut l[..frames], &mut r[..frames]);
            mix.push(&l[..frames], &r[..frames]);
            done += frames;
        }
    }

    // Let the tail ring out, stopping once it has been silent for a while.
    processor.stop();
    let silence_frames = (SILENCE_SECONDS * sample_rate) as usize;
    let mut rendered = kept_frames;
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
    mix.push(&left[kept_frames..], &right[kept_frames..]);
    if !keep {
        (left, right) = (Vec::new(), Vec::new());
    }

    let track_peaks = project
        .tracks
        .iter()
        .filter_map(|track| meters.tracks.get(&track.id).map(|meter| (track.id, meter)))
        .map(|(id, meter)| {
            let (l, r) = meter.peek();
            (id, l.max(r))
        })
        .collect();

    // The graph comes back from the processor with what its devices and
    // its taps have counted.
    let mut graph = processor.swap_graph(Box::new(Graph::empty()));
    let frame_of = |beat: f64| ((beat - start).max(0.0) * seconds_per_beat * sample_rate).round() as usize;
    let spans: Vec<(usize, usize)> =
        options.sections.iter().map(|section| (frame_of(section.start_beat), frame_of(section.end_beat))).collect();
    let mut tracks = Vec::with_capacity(project.tracks.len());
    for track in &project.tracks {
        let Some(tap) = graph.tracks.iter().find(|node| node.id == track.id).and_then(|node| node.tap.as_ref()) else {
            continue;
        };
        let stats = tap.stats();
        tracks.push(TrackLevel {
            track: track.id,
            name: track.name.clone(),
            peak_db: stats.peak_db,
            loudness_lufs: stats.loudness_lufs,
            rms_db: stats.rms_db,
            stereo_correlation: stats.stereo_correlation,
            spectrum: stats.spectrum,
            section_loudness: spans.iter().map(|(from, to)| tap.loudness_between(*from, *to)).collect(),
        });
    }
    let sections = options
        .sections
        .iter()
        .zip(&spans)
        .map(|(section, (from, to))| SectionLevel {
            section: section.clone(),
            loudness_lufs: mix.loudness_between(*from, *to),
            peak_db: mix.peak_between(*from, *to),
        })
        .collect();
    let mut dynamics = Vec::new();
    for node in &mut graph.tracks {
        taken_off(&mut node.chain, Some(node.id), project, &mut dynamics);
    }
    taken_off(&mut graph.master.chain, None, project, &mut dynamics);

    let name_of = |track: TrackId| project.track(track).map(|track| track.name.clone()).unwrap_or_default();
    let waited = waited
        .into_iter()
        // A plugin that was ready at once is not news.
        .filter(|waited| !waited.answered || waited.seconds >= WORTH_SAYING_SECONDS)
        .map(|Waited { track, device, seconds, answered }| WaitedFor {
            track,
            track_name: name_of(track),
            device,
            device_name: project.device(device).map(|device| device.name.clone()).unwrap_or_default(),
            seconds,
            answered,
        })
        .collect();

    let measured = Measured { stats: mix.stats(), tracks, sections, dynamics, waited };
    Ok(Rendered { sample_rate: options.sample_rate, left, right, track_peaks, problems, measured })
}

/// A wait shorter than this is not reported: the plugin was as good as
/// ready.
const WORTH_SAYING_SECONDS: f64 = 1.0;

/// Asks every device of `chain` what it took off, and writes down the
/// answers of those that take anything off at all.
fn taken_off(chain: &mut Chain, track: Option<TrackId>, project: &Project, out: &mut Vec<DynamicsLevel>) {
    for slot in chain.devices.iter_mut().filter(|slot| slot.enabled) {
        let Some(reduction) = slot.processor.as_mut().and_then(|processor| processor.gain_reduction()) else {
            continue;
        };
        out.push(DynamicsLevel {
            device: slot.id,
            name: project.device(slot.id).map(|device| device.name.clone()).unwrap_or_default(),
            track,
            most_db: reduction.most_db,
            typical_db: reduction.typical_db,
            active: reduction.active,
        });
    }
}
