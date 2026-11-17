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
//!
//! **A render does not go on without an instrument.** If a plugin that has
//! notes to play never answered them for as long as it was waited for,
//! what would come out is not the song, and nothing says so but a line
//! that is easily missed. The render stops there, before the first
//! sample, with [`RenderError::NotAnswered`] naming the track: nothing is
//! rendered and no file is written. The one exception is a silence the
//! track itself explains: what it knows of its instrument says that the
//! notes it plays here are keyswitches, or lie outside its range.
//!
//! **Only what is heard is made.** A track that is muted, or left out by a
//! solo or by [`RenderOptions::only_tracks`], is not compiled into the
//! graph at all: its devices are not created, its plugins not waited for.
//! Hearing one track of a song costs what that track costs.
//!
//! **Stems.** The same pass can write parts of the song to files of their
//! own ([`RenderOptions::stems`]): every track as it leaves its fader,
//! what goes into each track's effects, every group, every return, and the
//! mix as it reaches the master's devices. Each is taken where it goes by
//! and written a block at a time, so one render stands for as many as the
//! song has tracks and none of them is held in memory. A track's stem is
//! what a render of a project holding that track alone gives.
//!
//! **A render uses the machine, within bounds.** The tracks of a block
//! that do not feed one another are made on several threads at once
//! ([`crate::spread`]), and added up in the order one thread would have
//! added them: the samples are the same, in less time. Half the machine's
//! processors at the most, and eight, unless it is told another number
//! ([`usual_threads`]); and no device is made that one thread would not
//! have made. Plugins that are not known to keep
//! nothing between their instances are run in turn all the same, and one
//! that is not to be run beside anything leaves the render on one thread,
//! which the report says ([`Measured::alone`]).

use crate::analysis::{Analyzer, AudioStats, Spectrum};
use crate::compile::{Compiler, DeviceBank, Meters, Problem};
use crate::graph::{Chain, Graph, MAX_BLOCK, Tap, TrackNode};
use crate::processor::Processor;
use crate::ready::{self, Waited};
use crate::sources::AudioSources;
use crate::spread;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::audio::AudioBuffer;
use trill_core::device::{DeviceFactory, Purpose};
use trill_core::{DeviceId, Project, Track, TrackId, TrackKind};
use trill_media::{MediaError, WavFormat, WavWriter, write_wav};

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
    /// Parts of the song to write to files of their own as the render
    /// goes. `None` writes none.
    pub stems: Option<Stems>,
    /// How many threads make the render at the most: [`usual_threads`]
    /// where this is `None`, and never more than the song has tracks that
    /// can be made at once. With 1 it is made as a song is played, a
    /// track after the other. The samples are the same whatever this is,
    /// wherever the devices themselves do the same thing twice.
    pub threads: Option<usize>,
    /// Takes every device to tolerate being run beside any other, whatever
    /// is known of it ([`trill_core::device::Beside`]). For finding out
    /// what a plugin does when it is, and for nothing else.
    pub trusting: bool,
}

/// Which parts of the song a render writes to files of their own, beside
/// whatever becomes of the mix. Each is a 32-bit float WAV file as long as
/// the render, so that a part which goes over full scale before the
/// master brings it down is kept as it is. Only what the render plays has
/// a file: a track that is muted has none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stems {
    /// The folder the files go in. It is made if it is not there; files
    /// of the same names are replaced.
    pub folder: PathBuf,
    /// Every track that has clips, as it leaves its fader and pan: what
    /// it adds to the mix or to its group, without what its sends add to
    /// the returns. What a project holding that track alone renders to.
    pub tracks: bool,
    /// What goes into the effects of every such track: its instrument
    /// alone (or its audio), before any effect, the fader and the pan.
    pub instruments: bool,
    /// Every group as it leaves its fader: its members through its own
    /// devices.
    pub groups: bool,
    /// Every return as it leaves its fader, fed by every track's send.
    pub returns: bool,
    /// The mix as it reaches the master's devices: every track outside a
    /// group, every group and every return, added up.
    pub premaster: bool,
}

impl Stems {
    /// Every kind of stem, into `folder`.
    pub fn all(folder: impl Into<PathBuf>) -> Self {
        Self { folder: folder.into(), tracks: true, instruments: true, groups: true, returns: true, premaster: true }
    }

    /// The stem a track has as it leaves its fader, if it is to have one.
    fn kind_of(&self, track: &Track) -> Option<StemKind> {
        match track.kind {
            TrackKind::Normal => (self.tracks && plays(track)).then_some(StemKind::Track),
            TrackKind::Group => self.groups.then_some(StemKind::Group),
            TrackKind::Return => self.returns.then_some(StemKind::Return),
        }
    }

    /// Begins the file of one stem: `track 12 Drums.wav`, `premaster.wav`.
    fn begin(&self, what: StemKind, track: Option<&Track>, sample_rate: f64) -> Result<WavWriter, RenderError> {
        let name = match track {
            Some(track) => {
                // What a file may be called everywhere, and not too long.
                let kept = |c: char| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' };
                let name: String = track.name.chars().map(kept).take(48).collect();
                format!("{} {} {}", what.word(), track.id.0, name.trim())
            }
            None => what.word().to_string(),
        };
        Ok(WavWriter::create(&self.folder.join(format!("{}.wav", name.trim())), sample_rate, WavFormat::Float32)?)
    }
}

/// Whether a track has anything of its own to play.
fn plays(track: &Track) -> bool {
    track.clips.iter().any(|clip| !clip.muted)
}

/// What a stem is of.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StemKind {
    /// A track as it leaves its fader and pan.
    Track,
    /// What goes into a track's effects.
    Instrument,
    /// A group as it leaves its fader.
    Group,
    /// A return as it leaves its fader.
    Return,
    /// The mix as it reaches the master's devices.
    Premaster,
}

impl StemKind {
    fn word(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Instrument => "instrument",
            Self::Group => "group",
            Self::Return => "return",
            Self::Premaster => "premaster",
        }
    }
}

/// A file a render wrote beside the mix, and what is in it, measured as
/// the mix is.
#[derive(Clone, Debug, Serialize)]
pub struct Stem {
    pub path: String,
    pub what: StemKind,
    /// The track it is of; `None` for the mix before the master.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackId>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(flatten)]
    pub stats: AudioStats,
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
            stems: None,
            threads: None,
            trusting: false,
        }
    }
}

/// The most threads a render uses on a machine that has few.
const FEWEST_USUAL_THREADS: usize = 1;
/// The most threads a render uses unless it is told otherwise.
const MOST_USUAL_THREADS: usize = 8;

/// How many threads a render uses when it is not told
/// ([`RenderOptions::threads`]): half as many as the machine has
/// processors, and eight at the most. A render has no claim on the whole
/// machine: something else is usually running on it, the song is being
/// played beside it, and the last threads add little, because a few tracks
/// of a song cost most of it. `TRILL_RENDER_THREADS` in the environment
/// sets another number for every render of the program (1 for none
/// beside the thread that asks).
///
/// The threads cost a render no plugins: it makes one instance of each
/// device of the song, as it does on one thread, and what it holds beside
/// them is room for a block of each track.
pub fn usual_threads() -> usize {
    let processors = std::thread::available_parallelism().map_or(1, usize::from);
    let asked = std::env::var("TRILL_RENDER_THREADS").ok().and_then(|number| number.trim().parse::<usize>().ok());
    asked.filter(|number| *number >= 1).unwrap_or((processors / 2).clamp(FEWEST_USUAL_THREADS, MOST_USUAL_THREADS))
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
    /// Instruments that have notes to play never answered them, and
    /// nothing was rendered: see the module documentation. The ones that
    /// did not answer come first; after them, what else the wait found
    /// that is worth saying (an instrument that answered some of its
    /// notes and not others), which is found once and would be lost.
    #[error("{}", not_answered(.0))]
    NotAnswered(Vec<WaitedFor>),
}

fn not_answered(waited: &[WaitedFor]) -> String {
    let waited: Vec<&WaitedFor> = waited.iter().filter(|waited| !waited.answered).collect();
    let named: Vec<String> = waited
        .iter()
        .map(|waited| format!("{} on '{}' ({:.0} s)", waited.device_name, waited.track_name, waited.seconds))
        .collect();
    format!(
        "nothing was rendered: {} never answered the notes {} to play for as long as {} waited for, so what came out \
         would not be the song. If it was still loading, try again in a moment; if its notes are ones it does not \
         play, move them; or mute the track to go on without it",
        named.join(" and "),
        if waited.len() == 1 { "it is" } else { "they are" },
        if waited.len() == 1 { "it was" } else { "they were" },
    )
}

pub struct Rendered {
    pub sample_rate: u32,
    /// The mix. Empty when the render was only measured.
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Post-fader peak of each track over the whole render, linear.
    pub track_peaks: Vec<(TrackId, f32)>,
    /// How loud each track that was played was as the render went: after
    /// its fader, in dB, one number for every tenth of a second
    /// ([`Analyzer::levels`]). No audio of a track is kept for it.
    pub track_levels: Vec<(TrackId, Vec<f32>)>,
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
    /// The files that were written beside the mix, in the order of the
    /// song's tracks.
    pub stems: Vec<Stem>,
    /// How many threads made the render.
    pub threads: usize,
    /// The devices that kept it on one thread though it was given more:
    /// they are not to be run beside anything.
    pub alone: Vec<Alone>,
}

/// A device that is not to be run beside anything, which leaves a render
/// on one thread.
#[derive(Clone, Debug, Serialize)]
pub struct Alone {
    /// The track it is on; `None` for the master.
    pub track: Option<TrackId>,
    /// That track's name, or "master".
    pub track_name: String,
    pub device: DeviceId,
    pub device_name: String,
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
    /// The track it is on; `None` for the master.
    pub track: Option<TrackId>,
    /// That track's name, or "master".
    pub track_name: String,
    pub device: DeviceId,
    pub device_name: String,
    pub seconds: f64,
    /// False when it never answered the notes it was to play, and the
    /// render went ahead without it.
    pub answered: bool,
    /// The pitches it is to play that made no sound while it was waited
    /// for, though others did, the first time that was seen of it: it
    /// does not play them, or had still not loaded them when the render
    /// began.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub silent: Vec<u8>,
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
    /// See [`Measured::stems`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stems: Vec<Stem>,
    /// See [`Measured::threads`].
    pub threads: usize,
    /// See [`Measured::alone`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alone: Vec<Alone>,
    pub problems: Vec<String>,
}

impl RenderReport {
    fn new(path: Option<&Path>, sample_rate: u32, measured: Measured, problems: &[Problem]) -> Self {
        let Measured { stats, tracks, sections, dynamics, waited, stems, threads, alone } = measured;
        Self {
            path: path.map(|path| path.display().to_string()),
            sample_rate,
            stats,
            tracks,
            sections,
            dynamics,
            waited,
            stems,
            threads,
            alone,
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
    let begun = Instant::now();

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
    // What listens to the render: every track as it leaves its fader, and
    // whatever is to be a file of its own. The files are begun before the
    // plugins are waited for, so that a folder that cannot be written to
    // is found out at once.
    let stems = options.stems.as_ref();
    // The folder for them, if this render is what makes it: a render that
    // comes to nothing leaves no empty folder behind.
    let made_folder = stems.map(|stems| &stems.folder).filter(|folder| !folder.exists());
    let listener = |file: Option<WavWriter>| {
        let analyzer = Analyzer::new(options.sample_rate);
        let analyzer = if file.is_some() { analyzer.with_true_peak() } else { analyzer };
        Box::new(Tap { analyzer, file })
    };
    // A track that is made only for what it feeds (a soloed return, a
    // device that listens to it) is not heard itself: it has no row and
    // no file.
    for node in graph.tracks.iter_mut().filter(|node| node.heard) {
        let track = project.track(node.id).ok();
        let file = match (track, stems) {
            (Some(track), Some(stems)) => match stems.kind_of(track) {
                Some(kind) => Some(stems.begin(kind, Some(track), sample_rate)?),
                None => None,
            },
            _ => None,
        };
        node.tap = Some(listener(file));
        if let (Some(track), Some(stems)) = (track, stems)
            && stems.instruments
            && track.kind == TrackKind::Normal
            && plays(track)
        {
            let file = stems.begin(StemKind::Instrument, Some(track), sample_rate)?;
            node.source = Some((source_devices(track, node, &plugins), listener(Some(file))));
        }
    }
    if let Some(stems) = stems.filter(|stems| stems.premaster) {
        graph.master.tap = Some(listener(Some(stems.begin(StemKind::Premaster, None, sample_rate)?)));
    }
    let made = begun.elapsed();
    // How many threads make it: as many as it was given and can use,
    // unless a device of it is not to be run beside anything. The plugins
    // are rehearsed on them too.
    let alone = spread::company(&mut graph, project, factory, options.trusting);
    let given = options.threads.unwrap_or_else(usual_threads).max(1);
    let threads = if alone.is_empty() { given } else { 1 };
    let patience = (options.patience, threads);
    let waited = ready::wake(&mut graph, project, (&plugins, factory), (start, end), sample_rate, patience);
    let woke = begun.elapsed();
    let name_of = |track: Option<TrackId>| match track {
        Some(track) => project.track(track).map(|track| track.name.clone()).unwrap_or_default(),
        None => "master".to_string(),
    };
    let named = |Waited { track, device, seconds, answered, silent, .. }: Waited| WaitedFor {
        track,
        track_name: name_of(track),
        device,
        device_name: project.device(device).map(|device| device.name.clone()).unwrap_or_default(),
        seconds,
        answered,
        silent,
    };
    // An instrument that never answered, and whose track does not say why:
    // the render does not go on without it.
    let explained = |waited: &Waited| {
        waited
            .track
            .and_then(|track| project.track(track).ok())
            .is_some_and(|track| silent_by_design(track, start, end))
    };
    let mut missing: Vec<WaitedFor> = waited
        .iter()
        .filter(|waited| !waited.answered && !waited.effect && !explained(waited))
        .cloned()
        .map(named)
        .collect();
    if !missing.is_empty() {
        let partly = waited.iter().filter(|waited| waited.answered && !waited.silent.is_empty());
        missing.extend(partly.cloned().map(named));
        // With the graph go the files that were begun.
        drop(graph);
        if let Some(folder) = made_folder {
            let _ = std::fs::remove_dir(folder);
        }
        return Err(RenderError::NotAnswered(missing));
    }

    let alone: Vec<Alone> = alone
        .into_iter()
        .map(|device| {
            let track = project.tracks.iter().find(|track| track.devices.iter().any(|on| on.id == device));
            Alone {
                track: track.map(|track| track.id),
                track_name: name_of(track.map(|track| track.id)),
                device,
                device_name: project.device(device).map(|device| device.name.clone()).unwrap_or_default(),
            }
        })
        .collect();
    let mut processor = Processor::new(sample_rate);
    drop(processor.swap_graph(graph));
    let threads = if alone.is_empty() { processor.spread_over(given) } else { 1 };
    // Only where it made a difference is it worth saying.
    let alone = if given > 1 { alone } else { Vec::new() };
    processor.seek(start);
    processor.play();

    // How many frames into the render a beat of the song falls: through
    // whatever the tempo does on the way, and for one tempo as it was
    // always worked out.
    let tempo_map = project.tempo_map();
    let seconds_per_beat = 60.0 / project.tempo;
    let frames_to = |beat: f64| match tempo_map.is_constant() {
        true => (beat - start).max(0.0) * seconds_per_beat * sample_rate,
        false => tempo_map.frames_between(start, beat.max(start), sample_rate),
    };
    let body_frames = frames_to(end).round() as usize;
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
    // How long the render is: every stem is cut to it.
    let total_frames = (body_frames + rendered - kept_frames) as u64;
    if !keep {
        (left, right) = (Vec::new(), Vec::new());
    }

    // A track that was not played (it is muted, or left out by a solo) put
    // out nothing.
    let track_peaks = project
        .tracks
        .iter()
        .map(|track| {
            let (l, r) = meters.tracks.get(&track.id).map_or((0.0, 0.0), |meter| meter.peek());
            (track.id, l.max(r))
        })
        .collect();

    // The graph comes back from the processor with what its devices and
    // its taps have counted.
    let waited_for_threads = processor.waited_for_threads();
    let mut graph = processor.swap_graph(Box::new(Graph::empty()));
    let mut stems = Vec::new();
    for track in &project.tracks {
        let Some(node) = graph.tracks.iter_mut().find(|node| node.id == track.id) else { continue };
        let kind = match track.kind {
            TrackKind::Normal => StemKind::Track,
            TrackKind::Group => StemKind::Group,
            TrackKind::Return => StemKind::Return,
        };
        let taps = [(kind, node.tap.as_mut()), (StemKind::Instrument, node.source.as_mut().map(|(_, tap)| tap))];
        for (what, tap) in taps {
            stems.extend(tap.and_then(|tap| finished(what, Some(track), tap, total_frames)).transpose()?);
        }
    }
    let premaster = graph.master.tap.as_mut();
    stems.extend(premaster.and_then(|tap| finished(StemKind::Premaster, None, tap, total_frames)).transpose()?);
    let frame_of = |beat: f64| frames_to(beat).round() as usize;
    let spans: Vec<(usize, usize)> =
        options.sections.iter().map(|section| (frame_of(section.start_beat), frame_of(section.end_beat))).collect();
    let mut tracks = Vec::with_capacity(project.tracks.len());
    let mut track_levels = Vec::with_capacity(project.tracks.len());
    for track in &project.tracks {
        let Some(tap) = graph.tracks.iter().find(|node| node.id == track.id).and_then(|node| node.tap.as_ref()) else {
            continue;
        };
        let tap = &tap.analyzer;
        track_levels.push((track.id, tap.levels()));
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

    let waited = waited
        .into_iter()
        // A plugin that was ready at once is not news, and neither is an
        // effect that lets no noise through: a gate does that for a living.
        // But notes that stayed silent on an instrument that played others
        // are: it may not have had them loaded.
        .filter(|waited| match waited.answered {
            true => waited.seconds >= WORTH_SAYING_SECONDS || !waited.silent.is_empty(),
            false => !waited.effect,
        })
        .map(named)
        .collect();

    let played = begun.elapsed();
    // The devices go, each plugin in its turn on the plugins' own thread.
    drop(graph);
    log::info!(
        "a render of {:.1} s of audio on {threads} thread(s): its devices were made in {:.1} s, waited for {:.1} s, \
         played in {:.1} s (of which {:.1} s waiting for its threads) and let go of in {:.1} s",
        total_frames as f64 / sample_rate,
        made.as_secs_f64(),
        (woke - made).as_secs_f64(),
        (played - woke).as_secs_f64(),
        waited_for_threads.as_secs_f64(),
        (begun.elapsed() - played).as_secs_f64()
    );

    let measured = Measured { stats: mix.stats(), tracks, sections, dynamics, waited, stems, threads, alone };
    Ok(Rendered { sample_rate: options.sample_rate, left, right, track_peaks, track_levels, problems, measured })
}

/// A wait shorter than this is not reported: the plugin was as good as
/// ready.
const WORTH_SAYING_SECONDS: f64 = 1.0;

/// Whether what a track knows of its instrument says that none of the
/// notes it plays from `from` to `to` makes a sound: every one of them is
/// a keyswitch, or lies outside the instrument's range.
fn silent_by_design(track: &Track, from: f64, to: f64) -> bool {
    let profile = &track.profile;
    let unmuted = track.clips.iter().filter(|clip| !clip.muted);
    let mut notes = unmuted
        .flat_map(|clip| clip.timeline_notes())
        .filter(|note| note.start >= from - 1e-9 && note.start < to)
        .peekable();
    let silent = |pitch: u8| {
        let outside = profile.range.is_some() && !profile.in_range(pitch) && profile.sound_name(pitch).is_none();
        profile.is_keyswitch(pitch) || outside
    };
    !profile.is_empty() && notes.peek().is_some() && notes.all(|note| silent(note.pitch))
}

/// How many of a track's devices, from the first, make what goes into its
/// effects: up to and with its instrument. None for a track that plays
/// audio and has no instrument.
fn source_devices(track: &Track, node: &TrackNode, plugins: &ready::Plugins) -> usize {
    let is_instrument = |uri: &str| trill_dsp::is_instrument(uri).unwrap_or_else(|| plugins.is_instrument(uri));
    let instrument = track.devices.iter().find(|device| device.enabled && is_instrument(&device.uri));
    let at = instrument.and_then(|instrument| node.chain.devices.iter().position(|slot| slot.id == instrument.id));
    at.map_or(0, |at| at + 1)
}

/// Ends the file a tap wrote, if it wrote one, at `frames` frames, and
/// says what is in it.
fn finished(what: StemKind, track: Option<&Track>, tap: &mut Tap, frames: u64) -> Option<Result<Stem, RenderError>> {
    let file = tap.file.take()?;
    let path = file.path().display().to_string();
    Some(file.finish(Some(frames)).map_err(RenderError::from).map(|_| Stem {
        path,
        what,
        track: track.map(|track| track.id),
        name: track.map(|track| track.name.clone()).unwrap_or_default(),
        stats: tap.analyzer.stats(),
    }))
}

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
