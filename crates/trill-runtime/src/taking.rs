//! Taking from a study: what a study does to a song.
//!
//! A stretch of a stem as an audio clip that lies on the song's bars, a
//! single hit on a lane, a pad or a sampler, a kit, a part as notes, the
//! parts as markers, the recording's tempo as the song's, the recording
//! itself as a reference. Each is one undo step, and each is done here
//! for both front ends: they say what and where, and do nothing of their
//! own with a study.
//!
//! What is taken is worked out first, without the session
//! ([`Runtime::plan_stretch`]): that plan is also what a front end plays
//! to its user before anything is taken, so that what is heard is what
//! would be taken.

use crate::Runtime;
use crate::studying::Studied;
use std::path::{Path, PathBuf};
use trill_core::project::{AudioContent, Reference, SongKey, WarpMode};
use trill_core::tempo::TempoChange;
use trill_core::{Beats, ChainRef, Clip, ClipId, DeviceId, Drum, Note, Project, TrackId};
use trill_listen::{Heard, HitKind, MidiPart, OneShot};
use trill_media::WavFormat;

/// The folder inside a study's own where what was cut from its stems is
/// kept: several stems as one sound, a single hit.
pub const CUTS: &str = "cuts";

/// How far a bar line of a stretch may lie from where one speed for the
/// whole stretch would put it, in seconds, before the stretch is taken in
/// pieces that each have their own.
const DRIFT_SECONDS: f64 = 0.012;
/// How far a bar line of the song may come to lie from the recording's
/// when its tempo is taken, in seconds: bars that agree to within this are
/// one tempo.
const TEMPO_SECONDS: f64 = 0.003;
/// A stretch that begins and ends this near to beats of the recording is
/// taken to be whole beats long.
const ON_A_BEAT: f64 = 0.02;

/// Where what is taken goes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Lane {
    Track(TrackId),
    /// A track made for it, after the last.
    New,
}

/// How what is taken is pitched.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum Pitch {
    /// Where it was recorded.
    #[default]
    AsRecorded,
    /// Moved by this many semitones.
    Semitones(f64),
    /// Moved the shorter way to the key the song says it is in.
    SongKey,
}

/// A stretch of a study's recording to put into the song.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeStretch {
    pub study: String,
    /// The stems it is of; none for the whole recording.
    pub stems: Vec<String>,
    /// Where it begins and ends, in seconds of the recording.
    pub from: f64,
    pub to: f64,
    /// `None` names it after its stems and its bars.
    pub name: Option<String>,
    pub lane: Lane,
    /// Where in the song it begins, in beats.
    pub at: Beats,
    /// At the recording's own speed, untouched, whatever the song's tempo.
    /// Otherwise it follows the song: a beat of the recording is a beat of
    /// the song.
    pub as_recorded: bool,
    pub pitch: Pitch,
    /// How many times it is laid end to end; at least once.
    pub times: u32,
}

impl TakeStretch {
    pub fn new(study: impl Into<String>, stems: Vec<String>, from: f64, to: f64) -> Self {
        Self {
            study: study.into(),
            stems,
            from,
            to,
            name: None,
            lane: Lane::New,
            at: 0.0,
            as_recorded: false,
            pitch: Pitch::AsRecorded,
            times: 1,
        }
    }
}

/// One clip of a stretch that was planned.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    /// Where it begins after the start of the stretch, in beats of the song.
    pub start: Beats,
    pub length: Beats,
    /// Where in the file it begins, in seconds.
    pub offset_seconds: f64,
    /// The tempo this piece of the recording has; `None` as recorded.
    pub source_tempo: Option<f64>,
}

/// A file a plan needs that is not there yet: stems added up over a
/// stretch, with a short fade at each end where it is a single hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Cut {
    pub stems: Vec<PathBuf>,
    pub from: f64,
    pub to: f64,
    /// Faded in and out over a moment, as a hit lifted out of a part is.
    pub hit: bool,
}

/// A stretch as it will lie in the song: the file, the clips, and what
/// was decided on the way.
#[derive(Clone, Debug, PartialEq)]
pub struct StretchPlan {
    pub name: String,
    pub file: PathBuf,
    /// Set where `file` has to be made first.
    pub cut: Option<Cut>,
    pub warp: WarpMode,
    pub semitones: f64,
    /// One round of it, in beats of the song.
    pub length: Beats,
    pub pieces: Vec<Piece>,
    /// What was decided that the user did not say, each a sentence.
    pub notes: Vec<String>,
}

/// What came of taking a stretch.
#[derive(Clone, Debug, PartialEq)]
pub struct Taken {
    pub track: TrackId,
    pub clips: Vec<ClipId>,
    pub plan: StretchPlan,
}

/// Where a single hit goes.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum HitTo {
    /// Onto a lane as an audio clip, at this beat.
    Lane(Lane, Beats),
    /// Onto the pad of a drum rack that `note` plays; `None` for the pad
    /// its kind of drum usually has.
    Pad(DeviceId, Option<u8>),
    /// Into a sampler, which then plays it at its own pitch on the key of
    /// the note it is.
    Sampler(DeviceId),
    /// Into a sampler on a track made for it.
    NewSampler,
}

/// What came of taking a single hit.
#[derive(Clone, Debug, PartialEq)]
pub struct TakenHit {
    pub label: String,
    pub file: PathBuf,
    pub track: Option<TrackId>,
    pub clip: Option<ClipId>,
    pub device: Option<DeviceId>,
    /// The pad it is on, or the key a sampler plays it unchanged on.
    pub note: Option<u8>,
}

/// What came of taking a kit.
#[derive(Clone, Debug, PartialEq)]
pub struct TakenKit {
    pub track: TrackId,
    pub device: DeviceId,
    /// Each pad that was filled: its note, the hit by its label, its file.
    pub pads: Vec<(u8, String, PathBuf)>,
}

/// A part of a study to write as notes.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeNotes {
    pub study: String,
    pub part: MidiPart,
    pub from: f64,
    pub to: f64,
    pub lane: Lane,
    pub at: Beats,
    /// Steps to a beat that note starts are moved onto; `None` for what
    /// the part usually has (a groove on sixteenths, the rest as played).
    pub quantize: Option<u32>,
    /// The instrument of a track that is made for it; `None` for the
    /// usual one of its kind.
    pub instrument: Option<String>,
}

/// What a front end may do to the notes of a part before they are written
/// on a track that is there: `None` leaves them, or the notes the clip is
/// to hold in their place (an instrument's keyswitches among them).
pub type Keyed<'a> = &'a dyn Fn(&trill_engine::Session, TrackId, &[Note]) -> Result<Option<Vec<Note>>, String>;

/// What came of writing a part as notes.
#[derive(Clone, Debug, PartialEq)]
pub struct TakenNotes {
    pub track: TrackId,
    pub clip: ClipId,
    pub notes: usize,
    pub length: Beats,
    /// The instrument that was made with a new track.
    pub device: Option<DeviceId>,
    /// Drums the part has and the track's kit has not: left out.
    pub lacking: Vec<String>,
    pub said: Vec<String>,
}

/// What came of taking a recording's tempo.
#[derive(Clone, Debug, PartialEq)]
pub struct TakenTempo {
    /// How many bars of the recording were laid out.
    pub bars: usize,
    /// How many tempos they came to.
    pub tempos: usize,
    pub slowest: f64,
    pub fastest: f64,
    /// Where the recording's first bar line lies in the song, and its end.
    pub from: Beats,
    pub to: Beats,
    pub beats_per_bar: usize,
}

/// What came of putting a recording beside the song.
#[derive(Clone, Debug, PartialEq)]
pub struct TakenReference {
    pub track: TrackId,
    pub clip: ClipId,
    pub file: PathBuf,
    pub tempo: Option<TakenTempo>,
    pub said: Vec<String>,
}

fn done(studied: &Studied) -> Result<&Heard, String> {
    match (&studied.heard, studied.report.study.state) {
        (Some(heard), trill_study::State::Done) => Ok(heard),
        _ => Err(format!(
            "the study {} ({}) is not done: nothing can be taken from it until it is",
            studied.report.study.id, studied.report.study.title
        )),
    }
}

/// The beat of the recording's grid that starts its first bar.
pub fn first_bar(heard: &Heard) -> f64 {
    heard.grid.bars.first().copied().unwrap_or(0) as f64
}

/// The bar a time of the recording lies in, counted from 1 at its first
/// bar line, as the tools count them.
pub fn bar_of(heard: &Heard, seconds: f64) -> i64 {
    let per_bar = heard.grid.beats_per_bar.max(1) as f64;
    ((heard.grid.beat_at(seconds) - first_bar(heard) + 1e-6) / per_bar).floor() as i64 + 1
}

/// The start of bar `bar` of the recording (from 1), in seconds.
pub fn bar_start(heard: &Heard, bar: i64) -> f64 {
    let per_bar = heard.grid.beats_per_bar.max(1) as f64;
    heard.grid.seconds_at(first_bar(heard) + (bar - 1) as f64 * per_bar)
}

/// The bar lines of the recording around a stretch: the one at or before
/// its start and the one at or after its end. What is taken between them
/// keeps its place in the bar when it is put on a bar of the song.
pub fn whole_bars(heard: &Heard, from: f64, to: f64) -> (f64, f64) {
    let grid = &heard.grid;
    let per_bar = grid.beats_per_bar.max(1) as f64;
    let first = first_bar(heard);
    let down = |beat: f64| first + ((beat - first + 1e-3) / per_bar).floor() * per_bar;
    let up = |beat: f64| first + ((beat - first - 1e-3) / per_bar).ceil() * per_bar;
    let start = grid.seconds_at(down(grid.beat_at(from))).max(0.0);
    let end = grid.seconds_at(up(grid.beat_at(to))).min(heard.seconds.max(to));
    (start, end.max(start))
}

/// Where a moment of the recording a clip plays lies in the song, in
/// beats: what draws a study's parts, chords and words along a clip of it.
pub fn beat_of(project: &Project, clip: &Clip, seconds: f64) -> Option<Beats> {
    let audio = clip.audio()?;
    let into = seconds - audio.offset_seconds;
    Some(match (audio.warp, audio.source_tempo) {
        (WarpMode::Off, _) | (_, None) => {
            let map = project.tempo_map();
            map.beat_at_seconds(map.seconds_at(clip.start) + into)
        }
        (_, Some(tempo)) => clip.start + into * tempo / 60.0,
    })
}

/// The moment of the recording a clip plays at a beat of the song: the
/// other way round from [`beat_of`].
pub fn second_of(project: &Project, clip: &Clip, beat: Beats) -> Option<f64> {
    let audio = clip.audio()?;
    Some(match (audio.warp, audio.source_tempo) {
        (WarpMode::Off, _) | (_, None) => {
            let map = project.tempo_map();
            audio.offset_seconds + map.seconds_at(beat) - map.seconds_at(clip.start)
        }
        (_, Some(tempo)) => audio.offset_seconds + (beat - clip.start) * 60.0 / tempo,
    })
}

fn stem_word(stems: &[String]) -> String {
    match stems {
        [] => "Recording".to_owned(),
        stems => {
            let mut names: Vec<String> = stems.iter().map(|stem| capital(stem)).collect();
            names.sort();
            names.join(" + ")
        }
    }
}

fn capital(word: &str) -> String {
    let mut letters = word.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// A file name that is one everywhere.
fn file_word(text: &str) -> String {
    let kept: String = text.chars().map(|c| if c.is_alphanumeric() || "-_+".contains(c) { c } else { '_' }).collect();
    kept.chars().take(60).collect()
}

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 { format!("1 {one}") } else { format!("{count} {many}") }
}

/// The key a study read in its recording, as the song says its own.
pub fn key_of(heard: &Heard) -> Option<SongKey> {
    heard.key().map(|reading| SongKey::new(reading.key.tonic, reading.key.minor))
}

impl Runtime {
    /// A stretch as it would lie in the song: which file, which clips,
    /// how it is warped and pitched. Changes nothing and makes no file.
    pub fn plan_stretch(&self, studied: &Studied, take: &TakeStretch) -> Result<StretchPlan, String> {
        let heard = done(studied)?;
        let study = &studied.report.study;
        let (from, to) = (take.from.max(0.0), take.to.min(heard.seconds));
        if !(from.is_finite() && to.is_finite()) || to - from < 0.01 {
            return Err(format!(
                "there is nothing between {:.2} s and {:.2} s of a recording that is {:.2} s long",
                take.from, take.to, heard.seconds
            ));
        }
        let mut stems: Vec<String> = Vec::new();
        for stem in &take.stems {
            let name = stem.trim().to_lowercase();
            if !study.stems.contains(&name) {
                return Err(format!(
                    "the study {} has no stem '{stem}'; its stems are {}",
                    study.id,
                    study.stems.join(", ")
                ));
            }
            if !stems.contains(&name) {
                stems.push(name);
            }
        }
        if stems.len() == study.stems.len() {
            stems.clear();
        }
        let mut notes = Vec::new();

        // The file: a stem's own, the recording's own, or several stems cut into one.
        // `origin` is the moment of the recording the file begins at.
        let (file, cut, origin) = self.file_of(studied, &stems, from, to)?;

        // The clips.
        let grid = &heard.grid;
        let (first, last) = (grid.beat_at(from), grid.beat_at(to));
        let mut beats = last - first;
        let near = |beat: f64| (beat - beat.round()).abs() < ON_A_BEAT * grid.bpm.max(30.0) / 60.0;
        if near(first) && near(last) && (last.round() - first.round()) >= 1.0 {
            beats = last.round() - first.round();
        }
        let drums_only = stems.len() == 1 && stems[0] == "drums";
        let (project_tempo, song_key) = {
            let session = self.session.lock();
            (session.project().tempo_map(), session.project().key)
        };
        let semitones = match take.pitch {
            Pitch::AsRecorded => 0.0,
            Pitch::Semitones(semitones) if semitones.is_finite() && semitones.abs() <= 24.0 => semitones,
            Pitch::Semitones(semitones) => {
                return Err(format!("{semitones} semitones is further than a sound can be moved: 24 either way"));
            }
            Pitch::SongKey if drums_only => {
                notes.push("drums have no key: they are left at their pitch".to_owned());
                0.0
            }
            Pitch::SongKey => {
                let Some(song) = song_key else {
                    return Err("the song does not say what key it is in: say it (set_song_key), or say by how many \
                                semitones to move the sound"
                        .to_owned());
                };
                let Some(read) = key_of(heard) else {
                    return Err(format!(
                        "no key was read in the study {}: say by how many semitones to move the sound",
                        study.id
                    ));
                };
                let by = song.semitones_from(read);
                notes.push(match by {
                    0 => format!("{} and {} have the same notes: left at its pitch", read.name(), song.name()),
                    by => format!(
                        "moved {} from {} to {}",
                        if by > 0 {
                            plural(by as usize, "semitone up", "semitones up")
                        } else {
                            plural((-by) as usize, "semitone down", "semitones down")
                        },
                        read.name(),
                        song.name()
                    ),
                });
                f64::from(by)
            }
        };
        if take.as_recorded && semitones != 0.0 {
            return Err(
                "as recorded is the recording untouched: a sound that is moved to another pitch follows the song's \
                 tempo (leave `as_recorded` out)"
                    .to_owned(),
            );
        }
        if semitones != 0.0 {
            notes.push(
                "the pitch is moved by the stretcher, and a voice's formants move with it: a couple of semitones pass, \
                 beyond three or four a voice sounds smaller or larger than it was and hits lose some of their edge"
                    .to_owned(),
            );
        }
        let (warp, pieces, length) = if take.as_recorded {
            let begins = project_tempo.seconds_at(take.at);
            let length = project_tempo.beat_at_seconds(begins + (to - from)) - take.at;
            (WarpMode::Off, vec![Piece { start: 0.0, length, offset_seconds: from, source_tempo: None }], length)
        } else {
            if beats <= 0.0 {
                return Err("that stretch is no beat of the recording long".to_owned());
            }
            let scale = beats / (last - first);
            let pieces = pieces_of(heard, from, to, scale);
            if pieces.len() > 1 {
                notes.push(format!(
                    "the recording's tempo moves over these bars, so it lies in {} clips that each follow the song by \
                     their own tempo; taking the study's tempo into the song first keeps it in one, untouched",
                    pieces.len()
                ));
            }
            let warp = if drums_only { WarpMode::Beats } else { WarpMode::Tonal };
            (warp, pieces, beats)
        };
        let pieces: Vec<Piece> =
            pieces.into_iter().map(|piece| Piece { offset_seconds: piece.offset_seconds - origin, ..piece }).collect();
        let name = take.name.clone().filter(|name| !name.trim().is_empty()).unwrap_or_else(|| {
            let (first_bar, last_bar) = (bar_of(heard, from + 1e-3), bar_of(heard, to - 1e-3));
            let bars =
                if first_bar == last_bar { format!("bar {first_bar}") } else { format!("bars {first_bar}-{last_bar}") };
            format!("{} {bars} ({})", stem_word(&stems), study.title)
        });
        Ok(StretchPlan { name, file, cut, warp, semitones, length, pieces, notes })
    }

    /// The file a stretch of `stems` is a clip of, and how to make it if
    /// it is not there.
    fn file_of(
        &self,
        studied: &Studied,
        stems: &[String],
        from: f64,
        to: f64,
    ) -> Result<(PathBuf, Option<Cut>, f64), String> {
        let study = &studied.report.study;
        let folder = &studied.folder;
        let have = |stem: &String| folder.stem(stem);
        let missing = study.stems.iter().find(|stem| !have(stem).is_file());
        match stems {
            [one] => {
                let file = have(one);
                if !file.is_file() {
                    return Err(dropped(study, one));
                }
                Ok((file, None, 0.0))
            }
            [] => {
                let source = match &study.source {
                    trill_study::store::Source::File { path } if path.is_file() => Some(path.clone()),
                    _ => folder.kept_source(),
                };
                match (source, missing) {
                    (Some(source), _) => Ok((source, None, 0.0)),
                    (None, Some(stem)) => Err(dropped(study, stem)),
                    (None, None) => {
                        let file = folder.path.join(CUTS).join("recording.wav");
                        let stems = study.stems.iter().map(have).collect();
                        let cut = (!file.is_file()).then_some(Cut { stems, from: 0.0, to: f64::MAX, hit: false });
                        Ok((file, cut, 0.0))
                    }
                }
            }
            several => {
                if let Some(stem) = several.iter().find(|stem| !have(stem).is_file()) {
                    return Err(dropped(study, stem));
                }
                let mut names = several.to_vec();
                names.sort();
                let name = format!("{} {:.3}-{:.3}.wav", names.join("+"), from, to);
                let file = folder.path.join(CUTS).join(file_word(&name).replace("_wav", ".wav"));
                let stems = several.iter().map(have).collect();
                // A cut is the stretch and nothing else: the clip begins at its start.
                Ok((file.clone(), (!file.is_file()).then_some(Cut { stems, from, to, hit: false }), from))
            }
        }
    }

    /// Makes the file a plan needs, where it is not there yet: read through
    /// the media cache, written whole.
    pub fn make_cut(&self, file: &Path, cut: &Cut) -> Result<(), String> {
        let mut sum: Option<trill_core::audio::AudioBuffer> = None;
        for stem in &cut.stems {
            let audio = self.media.audio(stem).map_err(|error| format!("{}: {error}", stem.display()))?;
            let frames = audio.frames();
            let at = |seconds: f64| ((seconds * audio.sample_rate).round().max(0.0) as usize).min(frames);
            let (first, last) = (at(cut.from), if cut.to == f64::MAX { frames } else { at(cut.to) });
            let (left, right) = (&audio.left[first..last.max(first)], &audio.right[first..last.max(first)]);
            match &mut sum {
                None => {
                    sum = Some(trill_core::audio::AudioBuffer {
                        sample_rate: audio.sample_rate,
                        left: left.to_vec(),
                        right: right.to_vec(),
                    });
                }
                Some(sum) => {
                    for (into, from) in sum.left.iter_mut().zip(left) {
                        *into += from;
                    }
                    for (into, from) in sum.right.iter_mut().zip(right) {
                        *into += from;
                    }
                }
            }
        }
        let Some(mut sum) = sum.filter(|sum| sum.frames() > 0) else {
            return Err(format!("there is nothing to cut for {}", file.display()));
        };
        if cut.hit {
            // A hit lifted out of a part: in over a millisecond, out over five.
            let frames = sum.frames();
            let (rise, fall) = ((sum.sample_rate * 0.001) as usize, (sum.sample_rate * 0.005) as usize);
            for index in 0..rise.min(frames) {
                let gain = index as f32 / rise as f32;
                sum.left[index] *= gain;
                sum.right[index] *= gain;
            }
            for index in 0..fall.min(frames) {
                let gain = index as f32 / fall as f32;
                sum.left[frames - 1 - index] *= gain;
                sum.right[frames - 1 - index] *= gain;
            }
        }
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let partial = file.with_extension("partial.wav");
        trill_media::write_wav(&partial, &sum, WavFormat::Float32).map_err(|error| error.to_string())?;
        std::fs::rename(&partial, file).map_err(|error| format!("{}: {error}", file.display()))
    }

    /// Puts a stretch of a study into the song as audio: one undo step.
    pub fn take_stretch(&self, take: &TakeStretch) -> Result<Taken, String> {
        let studied = self.study(&take.study)?;
        let plan = self.plan_stretch(&studied, take)?;
        if let Some(cut) = &plan.cut {
            self.make_cut(&plan.file, cut)?;
        }
        let times = take.times.clamp(1, 256);
        let mut session = self.session.lock();
        let (track, clips) = session
            .edit(|project| {
                let track = match take.lane {
                    Lane::Track(track) => track,
                    Lane::New => project.add_track(plan.name.clone()),
                };
                let mut clips = Vec::new();
                for round in 0..times {
                    for piece in &plan.pieces {
                        let content = AudioContent {
                            path: plan.file.display().to_string(),
                            offset_seconds: piece.offset_seconds,
                            gain_db: 0.0,
                            fade_in: 0.0,
                            fade_out: 0.0,
                            warp: plan.warp,
                            source_tempo: piece.source_tempo,
                            transpose: plan.semitones,
                        };
                        let start = take.at + f64::from(round) * plan.length + piece.start;
                        clips.push(project.add_audio_clip(track, plan.name.clone(), start, piece.length, content)?);
                    }
                }
                Ok((track, clips))
            })
            .map_err(|error| error.to_string())?;
        Ok(Taken { track, clips, plan })
    }

    /// The file of a single hit of a study, cut from the stem it was heard
    /// in if it has not been cut before.
    pub fn hit_file(&self, studied: &Studied, hit: &OneShot) -> Result<PathBuf, String> {
        let study = &studied.report.study;
        let stem = if hit.kind == HitKind::Bass { "bass" } else { "drums" }.to_owned();
        let from = studied.folder.stem(&stem);
        let file = studied.folder.path.join(CUTS).join(format!("{}.wav", file_word(&hit.id)));
        if file.is_file() {
            return Ok(file);
        }
        if !from.is_file() {
            return Err(dropped(study, &stem));
        }
        self.make_cut(&file, &Cut { stems: vec![from], from: hit.start, to: hit.end, hit: true })?;
        Ok(file)
    }

    /// Puts a single hit of a study into the song: on a lane, on a pad of
    /// a drum rack, or into a sampler that then plays it by its own pitch.
    /// One undo step.
    pub fn take_hit(&self, study: &str, id: &str, to: HitTo) -> Result<TakenHit, String> {
        let studied = self.study(study)?;
        let heard = done(&studied)?;
        let hit = heard.one_shots.iter().find(|hit| hit.id == id).ok_or_else(|| {
            let some: Vec<&str> = heard.one_shots.iter().take(8).map(|hit| hit.id.as_str()).collect();
            format!(
                "the study {study} has no single hit '{id}'; it has {} (listen with show \"one_shots\" lists them), \
                 such as {}",
                heard.one_shots.len(),
                some.join(", ")
            )
        })?;
        let file = self.hit_file(&studied, hit)?;
        let path = file.display().to_string();
        let mut taken = TakenHit {
            label: hit.label.clone(),
            file: file.clone(),
            track: None,
            clip: None,
            device: None,
            note: None,
        };
        let mut session = self.session.lock();
        match to {
            HitTo::Lane(lane, at) => {
                let map = session.project().tempo_map();
                let length = map.beat_at_seconds(map.seconds_at(at) + (hit.end - hit.start)) - at;
                let content = AudioContent {
                    path,
                    offset_seconds: 0.0,
                    gain_db: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                    warp: WarpMode::Off,
                    source_tempo: None,
                    transpose: 0.0,
                };
                let (track, clip) = session
                    .edit(|project| {
                        let track = match lane {
                            Lane::Track(track) => track,
                            Lane::New => project.add_track(hit.label.clone()),
                        };
                        Ok((track, project.add_audio_clip(track, hit.label.clone(), at, length.max(0.01), content)?))
                    })
                    .map_err(|error| error.to_string())?;
                (taken.track, taken.clip) = (Some(track), Some(clip));
            }
            HitTo::Pad(device, note) => {
                let note = note.unwrap_or_else(|| pad_of(hit.kind));
                session.set_pad_file(device, note, Some(&path)).map_err(|error| error.to_string())?;
                (taken.device, taken.note) = (Some(device), Some(note));
            }
            HitTo::Sampler(device) => {
                let group = session.begin_group();
                let filled = session.set_sampler_file(device, Some(&path)).and_then(|()| {
                    session.edit(|project| {
                        tune(project, device, hit)?;
                        Ok(())
                    })
                });
                session.end_group(group);
                filled.map_err(|error| error.to_string())?;
                (taken.device, taken.note) = (Some(device), hit.note);
            }
            HitTo::NewSampler => {
                let state = trill_dsp::sampler::State::new(path).to_bytes();
                let (track, device) = session
                    .edit(|project| {
                        let track = project.add_track(hit.label.clone());
                        let device = project.add_device(ChainRef::Track(track), "builtin:sampler", "Sampler", None)?;
                        project.device_mut(device)?.state = Some(state);
                        tune(project, device, hit)?;
                        Ok((track, device))
                    })
                    .map_err(|error| error.to_string())?;
                (taken.track, taken.device, taken.note) = (Some(track), Some(device), hit.note);
            }
        }
        Ok(taken)
    }

    /// A drum rack of a study's cleanest hit of each kind, on a track made
    /// for it: one undo step.
    pub fn take_kit(&self, study: &str, name: Option<&str>) -> Result<TakenKit, String> {
        let studied = self.study(study)?;
        let heard = done(&studied)?;
        let mut pads: Vec<(u8, String, PathBuf)> = Vec::new();
        for kind in HitKind::DRUMS {
            let of_kind = heard.one_shots.iter().filter(|hit| hit.kind == kind);
            let Some(best) = of_kind.max_by(|a, b| a.score.total_cmp(&b.score)) else { continue };
            pads.push((pad_of(kind), best.label.clone(), self.hit_file(&studied, best)?));
        }
        if pads.is_empty() {
            return Err(format!("no single drum hit was found in the study {study}: there is no kit to take from it"));
        }
        pads.sort_by_key(|pad| pad.0);
        let filled = pads.iter().map(|(note, _, file)| (*note, file.display().to_string()));
        let state = trill_dsp::drumrack::State::with_pads(filled).map_err(|error| error.to_string())?.to_bytes();
        let title = name.map(str::to_owned).unwrap_or_else(|| format!("Kit ({})", studied.report.study.title));
        let mut session = self.session.lock();
        let group = session.begin_group();
        let made = (|| {
            let track = session.edit(|project| Ok(project.add_track(title))).map_err(|error| error.to_string())?;
            let device = session
                .add_device(ChainRef::Track(track), "builtin:drumrack", None, None)
                .map_err(|error| error.to_string())?;
            session.set_device_state(device, Some(state)).map_err(|error| error.to_string())?;
            Ok::<_, String>((track, device))
        })();
        let step = session.end_group(group);
        match made {
            Ok((track, device)) => Ok(TakenKit { track, device, pads }),
            Err(error) => {
                if step.is_some() {
                    session.undo();
                }
                Err(error)
            }
        }
    }

    /// Writes a part of a study as notes: on a track that is there, by what
    /// that track's instrument means by its keys, or on one made for it.
    /// One undo step.
    pub fn take_notes(&self, take: &TakeNotes, keyed: Option<Keyed<'_>>) -> Result<TakenNotes, String> {
        let studied = self.study(&take.study)?;
        let heard = done(&studied)?;
        self.write_notes(heard, &studied.report.study.title, take, keyed)
    }

    /// [`Runtime::take_notes`] of anything that was heard, a study's stems
    /// or one sound: `title` names where it is from.
    pub fn write_notes(
        &self,
        heard: &Heard,
        title: &str,
        take: &TakeNotes,
        keyed: Option<Keyed<'_>>,
    ) -> Result<TakenNotes, String> {
        let file = heard.midi(take.part, take.from, take.to, take.quantize.map(|steps| steps.min(48)));
        let Some(part) = file.tracks.first().filter(|track| !track.notes.is_empty()) else {
            return Err(format!(
                "no {} was heard between {:.1} s and {:.1} s of {title}: listen says what was found, and in which bars",
                take.part.name().to_lowercase(),
                take.from,
                take.to,
            ));
        };
        let grid = &heard.grid;
        let length = (grid.beat_at(take.to.min(heard.seconds)) - grid.beat_at(take.from.max(0.0))).max(1.0);
        let length = if (length - length.round()).abs() < 0.05 { length.round() } else { length.ceil() };
        let name = format!("{} ({title})", take.part.name());
        let drums = take.part == MidiPart::Groove;
        let mut session = self.session.lock();
        let (mut lacking, mut said) = (Vec::new(), Vec::new());
        let mut notes: Vec<Note> = part.notes.clone();
        let mut count = notes.len();
        if let Lane::Track(track) = take.lane {
            let plays_drums = session.plays_drums(track);
            let profile = session.profile_of(track);
            match (drums, plays_drums) {
                (true, true) => {
                    (notes, lacking) = on_kit(&profile, notes);
                    if notes.is_empty() {
                        return Err(format!(
                            "the kit on {track} has none of the drums this groove plays ({}): nothing was written",
                            lacking.join(", ")
                        ));
                    }
                }
                (true, false) => said.push(format!(
                    "{track} is not known to play drums: the groove is written on the General MIDI keys (kick 36, \
                     snare 38, closed hat 42)"
                )),
                (false, true) => {
                    return Err(format!(
                        "{track} plays drums: a {} is for an instrument that plays pitches",
                        take.part.name().to_lowercase()
                    ));
                }
                (false, false) => {}
            }
            count = notes.len();
            // An instrument that is played in more than one way is told which, for the whole clip.
            if let Some(keyed) = keyed
                && let Some(all) = keyed(&session, track, &notes)?
            {
                notes = all;
            }
        }

        let uri = match (&take.instrument, drums) {
            (Some(uri), _) => uri.clone(),
            (None, true) => "builtin:drumkit".to_owned(),
            (None, false) => "builtin:polysynth".to_owned(),
        };
        if take.lane == Lane::New && !session.factory().descriptors().iter().any(|descriptor| descriptor.uri == uri) {
            return Err(format!("unknown device '{uri}'; list_devices shows what is available"));
        }
        let group = session.begin_group();
        let written = (|| {
            let (track, clip) = session
                .edit(|project| {
                    let track = match take.lane {
                        Lane::Track(track) => track,
                        Lane::New => project.add_track(take.part.name()),
                    };
                    let clip = project.add_midi_clip(track, name, take.at, length)?;
                    project.clip_mut(clip)?.add_notes(&notes)?;
                    Ok((track, clip))
                })
                .map_err(|error| error.to_string())?;
            let device = match take.lane {
                Lane::New => Some(
                    session.add_device(ChainRef::Track(track), &uri, None, None).map_err(|error| error.to_string())?,
                ),
                Lane::Track(_) => None,
            };
            Ok::<_, String>((track, clip, device))
        })();
        let step = session.end_group(group);
        match written {
            Ok((track, clip, device)) => Ok(TakenNotes { track, clip, notes: count, length, device, lacking, said }),
            Err(error) => {
                if step.is_some() {
                    session.undo();
                }
                Err(error)
            }
        }
    }

    /// The parts of a study as markers of the song, the recording's first
    /// bar line at `at`: one undo step. Returns each marker's beat and name.
    pub fn take_parts(&self, study: &str, at: Beats) -> Result<Vec<(Beats, String)>, String> {
        let studied = self.study(study)?;
        let heard = done(&studied)?;
        if heard.parts.is_empty() {
            return Err(format!("no parts were found in the study {study}"));
        }
        let per_bar = heard.grid.beats_per_bar.max(1) as f64;
        let markers: Vec<(Beats, String)> = heard
            .parts
            .iter()
            .map(|part| {
                let without = match part.without.as_slice() {
                    [] => String::new(),
                    without => format!(" (no {})", without.join(", ")),
                };
                (at + part.bar as f64 * per_bar, format!("{}{without}", part.label))
            })
            .collect();
        let mut session = self.session.lock();
        session
            .edit(|project| {
                for (beat, name) in &markers {
                    project.add_marker(*beat, name.clone())?;
                }
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        Ok(markers)
    }

    /// The recording's tempo as the song's, bar by bar, its first bar line
    /// at `at`: one undo step. Nothing in the song moves: its beats come to
    /// last as long as the recording's.
    pub fn take_tempo(&self, study: &str, at: Beats) -> Result<TakenTempo, String> {
        let studied = self.study(study)?;
        let heard = done(&studied)?;
        let mut session = self.session.lock();
        session.edit(|project| laid_tempo(project, heard, at)).map_err(|error| error.to_string())
    }

    /// Puts a study's recording beside the song as a reference: a track
    /// that is silent unless listened to and is in no render, with the
    /// recording's first bar line at `at`. With `tempo` the song takes the
    /// recording's tempo from there in the same step, which is what makes
    /// its bars the song's. One undo step.
    pub fn take_reference(&self, study: &str, at: Beats, tempo: bool) -> Result<TakenReference, String> {
        let studied = self.study(study)?;
        let heard = done(&studied)?;
        let (file, cut, _) = self.file_of(&studied, &[], 0.0, heard.seconds)?;
        if let Some(cut) = &cut {
            self.make_cut(&file, cut)?;
        }
        let title = studied.report.study.title.clone();
        let line = heard.grid.seconds_at(first_bar(heard)).max(0.0);
        let mut said = Vec::new();
        let mut session = self.session.lock();
        let (track, clip, tempo) = session
            .edit(|project| {
                let tempo = if tempo { Some(laid_tempo(project, heard, at)?) } else { None };
                let map = project.tempo_map();
                // The first bar line on `at`, with what comes before it in front where there is room.
                let before = map.seconds_at(at);
                let (start, offset) =
                    if before >= line { (map.beat_at_seconds(before - line), 0.0) } else { (0.0, line - before) };
                let length = map.beat_at_seconds(map.seconds_at(start) + (heard.seconds - offset)) - start;
                let content = AudioContent {
                    path: file.display().to_string(),
                    offset_seconds: offset,
                    gain_db: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                    warp: WarpMode::Off,
                    source_tempo: Some(heard.grid.bpm).filter(|bpm| *bpm > 0.0),
                    transpose: 0.0,
                };
                let track = project.add_track(format!("Reference: {title}"));
                let clip = project.add_audio_clip(track, title.clone(), start, length.max(0.01), content)?;
                project.set_reference(track, Some(Reference { study: Some(study.to_owned()), listening: false }))?;
                Ok((track, clip, tempo))
            })
            .map_err(|error| error.to_string())?;
        if tempo.is_none() {
            said.push(
                "the recording plays at its own speed: its bars are the song's only where the song has its tempo \
                 (take the study's tempo to lay it out bar by bar)"
                    .to_owned(),
            );
        }
        Ok(TakenReference { track, clip, file, tempo, said })
    }
}

fn dropped(study: &trill_study::store::Study, stem: &str) -> String {
    format!(
        "the stem '{stem}' of the study {} ({}) is not there: its stems were dropped, or the file is gone. Studying \
         the recording again makes them again",
        study.id, study.title
    )
}

/// A groove written on the General MIDI keys, on the keys of the kit a
/// track has: the kit is asked for each drum by its name. A drum the kit
/// has not got is left out and named, never a hit on the key the name
/// usually means.
fn on_kit(profile: &trill_core::InstrumentProfile, notes: Vec<Note>) -> (Vec<Note>, Vec<String>) {
    let mut lacking = Vec::new();
    let kept = notes
        .into_iter()
        .filter_map(|note| {
            let name = trill_core::pitch::drum_name(note.pitch).unwrap_or("drum");
            match profile.find_drum(name) {
                Drum::Key(key) => Some(Note { pitch: key.pitch, ..note }),
                Drum::Lacking(name) => {
                    if !lacking.contains(&name) {
                        lacking.push(name);
                    }
                    None
                }
                Drum::Unknown => Some(note),
            }
        })
        .collect();
    (kept, lacking)
}

/// The pad of a drum rack a kind of hit usually has.
pub fn pad_of(kind: HitKind) -> u8 {
    match kind {
        HitKind::Kick => 36,
        HitKind::Snare => 38,
        HitKind::Hat => 42,
        HitKind::OpenHat => 46,
        HitKind::Cymbal => 49,
        HitKind::Tom => 45,
        HitKind::Perc => 39,
        HitKind::Bass => 47,
    }
}

/// Gives a sampler the pitch of the hit it plays: the key that plays it
/// unchanged, and how far the hit is from that key's pitch.
fn tune(project: &mut Project, device: DeviceId, hit: &OneShot) -> Result<(), trill_core::EditError> {
    let Some(note) = hit.note else { return Ok(()) };
    let params = &mut project.device_mut(device)?.params;
    params.insert("root".to_owned(), f64::from(note));
    params.insert("fine".to_owned(), -hit.cents.unwrap_or(0.0).clamp(-100.0, 100.0));
    Ok(())
}

/// The clips a stretch that follows the song lies in: one where one speed
/// keeps every bar line of it in place, else as few as do, cut at bar
/// lines. `scale` is beats of the song to a beat of the recording.
fn pieces_of(heard: &Heard, from: f64, to: f64, scale: f64) -> Vec<Piece> {
    let grid = &heard.grid;
    let (first, last) = (grid.beat_at(from), grid.beat_at(to));
    // Every beat of the grid inside the stretch, and which of them begin a bar.
    let inside: Vec<(f64, f64, bool)> = grid
        .beats
        .iter()
        .enumerate()
        .filter(|(_, time)| **time > from + 1e-6 && **time < to - 1e-6)
        .map(|(index, time)| (index as f64, *time, grid.bars.contains(&index)))
        .collect();
    let piece = |begin: (f64, f64), end: (f64, f64)| Piece {
        start: (begin.0 - first) * scale,
        length: (end.0 - begin.0) * scale,
        offset_seconds: begin.1,
        source_tempo: Some((end.0 - begin.0) * scale * 60.0 / (end.1 - begin.1)),
    };
    // How far the beats between two ends lie from where one speed puts them.
    let drift = |begin: (f64, f64), end: (f64, f64)| {
        let between = inside.iter().filter(|(beat, _, _)| *beat > begin.0 && *beat < end.0);
        between
            .map(|(beat, time, _)| {
                let even = begin.1 + (beat - begin.0) / (end.0 - begin.0) * (end.1 - begin.1);
                (even - time).abs()
            })
            .fold(0.0, f64::max)
    };
    let (start, end) = ((first, from), (last, to));
    if drift(start, end) <= DRIFT_SECONDS {
        return vec![piece(start, end)];
    }
    let lines: Vec<(f64, f64)> =
        inside.iter().filter(|(_, _, bar)| *bar).map(|(beat, time, _)| (*beat, *time)).chain([end]).collect();
    let mut pieces = Vec::new();
    let mut begin = start;
    let mut reached = begin;
    for line in lines {
        if reached != begin && drift(begin, line) > DRIFT_SECONDS {
            pieces.push(piece(begin, reached));
            begin = reached;
        }
        reached = line;
    }
    pieces.push(piece(begin, end));
    pieces
}

/// Lays the recording's bars into the song's tempo map from `at` on, and
/// puts back after its last bar the tempo the song had there.
fn laid_tempo(project: &mut Project, heard: &Heard, at: Beats) -> Result<TakenTempo, trill_core::EditError> {
    let invalid = |text: String| Err(trill_core::EditError::Invalid(text));
    let grid = &heard.grid;
    let lines = grid.bar_times(heard.seconds);
    if lines.len() < 2 {
        return invalid("no bars were found in this recording: there is no tempo to take from it".to_owned());
    }
    if !at.is_finite() || at < 0.0 {
        return invalid(format!("{at} is not a beat of the song"));
    }
    let per_bar = grid.beats_per_bar.max(1) as f64;
    // Runs of bars that one tempo keeps to within a few milliseconds of the recording.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut begin = 0;
    for end in 2..lines.len() {
        let even =
            |index: usize| lines[begin] + (index - begin) as f64 / (end - begin) as f64 * (lines[end] - lines[begin]);
        if (begin + 1..end).any(|index| (even(index) - lines[index]).abs() > TEMPO_SECONDS) {
            runs.push((begin, end - 1));
            begin = end - 1;
        }
    }
    runs.push((begin, lines.len() - 1));
    let bars = lines.len() - 1;
    let (from, to) = (at, at + bars as f64 * per_bar);
    let after = project.tempo_map().tempo_at(to);
    let tempo_of = |(first, last): (usize, usize)| {
        ((last - first) as f64 * per_bar * 60.0 / (lines[last] - lines[first]))
            .clamp(trill_core::project::MIN_TEMPO, trill_core::project::MAX_TEMPO)
    };
    let mut changes: Vec<TempoChange> = project
        .tempo_changes
        .iter()
        .filter(|change| change.beat < from - 1e-9 || change.beat > to + 1e-9)
        .cloned()
        .collect();
    let (mut slowest, mut fastest) = (f64::MAX, 0.0f64);
    let mut first_tempo = None;
    for run in &runs {
        let tempo = tempo_of(*run);
        (slowest, fastest) = (slowest.min(tempo), fastest.max(tempo));
        let beat = at + run.0 as f64 * per_bar;
        if beat <= trill_core::tempo::SAME_BEAT {
            first_tempo = Some(tempo);
        } else {
            changes.push(TempoChange { beat, tempo, ramp: false });
        }
    }
    if !changes.iter().any(|change| (change.beat - to).abs() <= 1e-9) && project.length_shown() > to {
        changes.push(TempoChange { beat: to, tempo: after, ramp: false });
    }
    if let Some(tempo) = first_tempo {
        project.set_tempo(tempo)?;
    }
    project.set_tempo_changes(changes)?;
    // The recording's bars are the song's only in its own time signature.
    let numerator = grid.beats_per_bar.clamp(1, 32) as u8;
    if project.meter_map().signature_at(at).numerator != numerator
        || project.meter_map().signature_at(at).denominator != 4
    {
        project.set_time_signature_at(at, numerator, 4)?;
    }
    Ok(TakenTempo { bars, tempos: runs.len(), slowest, fastest, from, to, beats_per_bar: grid.beats_per_bar })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_listen::beats::Grid;

    fn heard(beats: Vec<f64>, per_bar: usize) -> Heard {
        let bars = (0..beats.len()).step_by(per_bar).collect();
        let seconds = beats.last().copied().unwrap_or(0.0);
        Heard {
            seconds,
            grid: Grid { beats, bars, bpm: 120.0, beats_per_bar: per_bar, steady: true, ..Grid::default() },
            ..Heard::default()
        }
    }

    #[test]
    fn a_steady_stretch_is_one_clip_and_one_whose_tempo_moves_is_cut_at_bar_lines() {
        // Sixteen beats at 120: one speed keeps every beat in place.
        let steady = heard((0..=16).map(|beat| f64::from(beat) * 0.5).collect(), 4);
        let pieces = pieces_of(&steady, 0.0, 8.0, 1.0);
        assert_eq!(pieces.len(), 1);
        assert!((pieces[0].length - 16.0).abs() < 1e-9 && (pieces[0].source_tempo.unwrap() - 120.0).abs() < 1e-9);

        // Two bars at 120 and two at 100: a bar line would lie a fifth of a second off.
        let mut times: Vec<f64> = (0..=8).map(|beat| f64::from(beat) * 0.5).collect();
        times.extend((1..=8).map(|beat| 4.0 + f64::from(beat) * 0.6));
        let moving = heard(times, 4);
        let pieces = pieces_of(&moving, 0.0, 8.8, 1.0);
        assert_eq!(pieces.len(), 2, "{pieces:?}");
        assert!((pieces[0].length - 8.0).abs() < 1e-9 && (pieces[0].source_tempo.unwrap() - 120.0).abs() < 1e-6);
        assert!((pieces[1].start - 8.0).abs() < 1e-9 && (pieces[1].offset_seconds - 4.0).abs() < 1e-9);
        assert!((pieces[1].source_tempo.unwrap() - 100.0).abs() < 1e-6);
    }

    #[test]
    fn a_recordings_tempo_is_laid_into_the_song_bar_by_bar_and_the_song_keeps_its_own_after() {
        let mut times: Vec<f64> = (0..=8).map(|beat| 0.25 + f64::from(beat) * 0.5).collect();
        times.extend((1..=8).map(|beat| 4.25 + f64::from(beat) * 0.6));
        let moving = heard(times, 4);
        let mut project = Project::new("t");
        let track = project.add_track("Keys");
        project.add_midi_clip(track, "long", 0.0, 64.0).unwrap();
        let taken = laid_tempo(&mut project, &moving, 8.0).unwrap();
        assert_eq!((taken.bars, taken.tempos), (4, 2));
        assert!((taken.slowest - 100.0).abs() < 1e-6 && (taken.fastest - 120.0).abs() < 1e-6);
        let map = project.tempo_map();
        assert!((map.tempo_at(0.0) - 120.0).abs() < 1e-9 && (map.tempo_at(17.0) - 100.0).abs() < 1e-6);
        assert!((map.tempo_at(24.0) - 120.0).abs() < 1e-9, "the song's own tempo again after the recording");
        // Every bar of the recording lasts in the song what it lasts in the recording.
        for (bar, seconds) in [(0.0, 2.0), (1.0, 2.0), (2.0, 2.4), (3.0, 2.4)] {
            let lasted = map.seconds_at(8.0 + (bar + 1.0) * 4.0) - map.seconds_at(8.0 + bar * 4.0);
            assert!((lasted - seconds).abs() < 1e-6, "bar {bar}: {lasted}");
        }
        // Taken again from the start, the song begins at the recording's tempo.
        let taken = laid_tempo(&mut project, &moving, 0.0).unwrap();
        assert_eq!(taken.from, 0.0);
        assert!((project.tempo - 120.0).abs() < 1e-6);
    }

    #[test]
    fn a_groove_is_written_on_the_keys_the_kit_has_and_a_drum_it_lacks_is_left_out_and_named() {
        let hit = |pitch: u8, start: f64| Note { pitch, start, length: 0.25, velocity: 100 };
        let groove = vec![hit(36, 0.0), hit(42, 0.5), hit(38, 1.0), hit(42, 1.5)];
        // A kit that says what is on its keys: a kick on C3, a snare on D3, and no hat.
        let mut kit = trill_core::InstrumentProfile { whole: true, ..Default::default() };
        kit.sounds.insert("kick".to_owned(), 48);
        kit.sounds.insert("snare".to_owned(), 50);
        let (notes, lacking) = on_kit(&kit, groove.clone());
        assert_eq!(notes.iter().map(|note| (note.pitch, note.start)).collect::<Vec<_>>(), [(48, 0.0), (50, 1.0)]);
        assert_eq!(lacking, ["closed_hat"]);
        // A track that says nothing of its kit is played on General MIDI's keys.
        let (notes, lacking) = on_kit(&trill_core::InstrumentProfile::default(), groove.clone());
        assert_eq!(notes, groove);
        assert!(lacking.is_empty());
    }

    #[test]
    fn a_stretch_is_widened_to_the_bar_lines_around_it_and_bars_are_counted_from_the_first_line() {
        let steady = heard((0..=16).map(|beat| 1.0 + f64::from(beat) * 0.5).collect(), 4);
        assert_eq!(whole_bars(&steady, 3.4, 4.9), (3.0, 5.0));
        assert_eq!(whole_bars(&steady, 3.0, 5.0), (3.0, 5.0));
        assert_eq!((bar_of(&steady, 1.0), bar_of(&steady, 2.99), bar_of(&steady, 3.0)), (1, 1, 2));
        assert!((bar_start(&steady, 3) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_moment_of_a_recording_is_found_in_the_song_through_its_clip_and_back() {
        let mut project = Project::new("t");
        project.set_tempo(100.0).unwrap();
        let track = project.add_track("Audio");
        let content = |warp, source_tempo| AudioContent {
            path: "a.wav".into(),
            offset_seconds: 10.0,
            gain_db: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            warp,
            source_tempo,
            transpose: 0.0,
        };
        let warped = project.add_audio_clip(track, "w", 8.0, 16.0, content(WarpMode::Tonal, Some(120.0))).unwrap();
        let plain = project.add_audio_clip(track, "p", 32.0, 16.0, content(WarpMode::Off, None)).unwrap();
        let (warped, plain) = (project.clip(warped).unwrap(), project.clip(plain).unwrap());
        // Warped: a beat of the recording is a beat of the song.
        assert!((beat_of(&project, warped, 12.0).unwrap() - 12.0).abs() < 1e-9);
        assert!((second_of(&project, warped, 12.0).unwrap() - 12.0).abs() < 1e-9);
        // As recorded: a second of the recording is a second of the song, at 100 beats a minute.
        assert!((beat_of(&project, plain, 13.0).unwrap() - 37.0).abs() < 1e-9);
        assert!((second_of(&project, plain, 37.0).unwrap() - 13.0).abs() < 1e-9);
    }
}
