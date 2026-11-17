//! A study on disk: one folder each, holding the recording's stems and
//! everything that was found in them.
//!
//! ```text
//! <studies>/<id>/
//!   study.json    what it is of, how it was asked for, and how far it has come
//!   source.*      the recording itself, only when it was fetched or had to be converted
//!   stems/*.wav   the stems, which add up to the recording
//!   outlines.json the outline of each stem, for drawing it without reading it
//!   beats.json    the beats and downbeats a model found
//!   words.json    the words as the transcriber heard them, a stretch at a time
//!   lyrics.json   the words in sung lines, each word with its time and its note
//!   heard.json    what `trill-listen` hears in the stems over those beats
//! ```
//!
//! Each of the last five is the product of one stage of a run, written
//! whole or not at all: a stage whose product is there is done, which is
//! how a run that was stopped is taken up where it was.

use crate::lyrics::Lyrics;
use crate::models::Transcript;
pub use trill_fetch::{partial_of, write_json, write_whole};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use trill_listen::{Correction, Heard, TrackedBeats};

/// The longest recording that is studied, in seconds.
pub const LONGEST_SECONDS: f64 = 15.0 * 60.0;

/// One stage of a run, in the order they are done.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// A link is fetched by the program the user installed for it.
    Fetch,
    /// The audio is read, and brought to the rate everything works at.
    Read,
    Stems,
    Beats,
    Words,
    WordTimes,
    /// Key, chords, parts, loops, single hits and notes, heard in the stems.
    Listen,
}

impl Stage {
    pub const ALL: [Stage; 7] =
        [Stage::Fetch, Stage::Read, Stage::Stems, Stage::Beats, Stage::Words, Stage::WordTimes, Stage::Listen];

    /// What is being done, as a person reads it while it is done.
    pub fn doing(self) -> &'static str {
        match self {
            Stage::Fetch => "Fetching the recording",
            Stage::Read => "Reading the audio",
            Stage::Stems => "Splitting it into stems",
            Stage::Beats => "Finding the beat",
            Stage::Words => "Writing down the words",
            Stage::WordTimes => "Placing the words",
            Stage::Listen => "Listening to the stems",
        }
    }

    /// The same in the middle of a sentence: "failed while …".
    pub fn while_doing(self) -> &'static str {
        match self {
            Stage::Fetch => "fetching the recording",
            Stage::Read => "reading the audio",
            Stage::Stems => "splitting it into stems",
            Stage::Beats => "finding the beat",
            Stage::Words => "writing down the words",
            Stage::WordTimes => "placing the words",
            Stage::Listen => "listening to the stems",
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageState {
    #[default]
    Pending,
    Running,
    Done,
    /// Not wanted, or nothing to do it on: no vocals, no words asked for.
    Skipped,
    Failed,
}

/// How one stage of a study went.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StageReport {
    pub stage: Stage,
    pub state: StageState,
    /// How long it took, once it has ended.
    pub seconds: Option<f64>,
    /// A word on how it went: "already done", "no singing was heard".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// Which model splits the recording.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StemChoice {
    /// One model for all four stems.
    #[default]
    Fast,
    /// Four models, one tuned for each stem: the cleanest result, at four
    /// times the work.
    Best,
}

/// How a study was asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StudyOptions {
    #[serde(default)]
    pub stems: StemChoice,
    /// Whether the words are written down.
    pub words: bool,
    /// The language sung, as its two letters (`en`), or none for the
    /// transcriber to tell.
    #[serde(default)]
    pub language: Option<String>,
}

impl Default for StudyOptions {
    fn default() -> Self {
        Self { stems: StemChoice::Fast, words: true, language: None }
    }
}

/// Where a recording came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A file the user has.
    File { path: PathBuf },
    /// A link, with what the page it leads to says of the recording.
    Link {
        url: String,
        #[serde(default)]
        by: Option<String>,
    },
}

/// How far a study has come.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Asked for, and waiting for its turn.
    #[default]
    Waiting,
    Running,
    Done,
    /// Stopped by whoever asked for it; it can be taken up again.
    Stopped,
    /// See [`Study::error`]; it can be taken up again.
    Failed,
}

/// What `study.json` holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Study {
    pub id: String,
    pub title: String,
    pub source: Source,
    /// When it was asked for, in seconds since 1970.
    pub created: u64,
    /// The length of the recording, once it has been read.
    #[serde(default)]
    pub seconds: f64,
    pub options: StudyOptions,
    /// The names of its stems, once it has been split, in the model's order.
    #[serde(default)]
    pub stems: Vec<String>,
    pub stages: Vec<StageReport>,
    #[serde(default)]
    pub state: State,
    /// Why it failed: which stage, and what to do.
    #[serde(default)]
    pub error: Option<String>,
    /// What ran the models: "the processor", or a graphics card by name.
    #[serde(default)]
    pub device: Option<String>,
    /// Corrections to the beat grid, in the order they were made: what
    /// is heard in the stems is heard over the grid they give.
    #[serde(default)]
    pub corrections: Vec<Correction>,
}

impl Study {
    pub fn report(&mut self, stage: Stage) -> &mut StageReport {
        if !self.stages.iter().any(|report| report.stage == stage) {
            self.stages.push(StageReport { stage, state: StageState::Pending, seconds: None, note: String::new() });
        }
        self.stages.iter_mut().find(|report| report.stage == stage).expect("just made sure of it")
    }

    /// The stage that is running, if one is.
    pub fn running(&self) -> Option<Stage> {
        self.stages.iter().find(|report| report.state == StageState::Running).map(|report| report.stage)
    }
}

/// One study's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudyFolder {
    pub path: PathBuf,
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

impl StudyFolder {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn study(&self) -> Result<Study, String> {
        let file = self.path.join("study.json");
        let text = std::fs::read_to_string(&file).map_err(|error| format!("{}: {error}", file.display()))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("{} is not a study Trill can read: {error}", file.display()))
    }

    pub fn write(&self, study: &Study) -> Result<(), String> {
        write_json(&self.path.join("study.json"), study)
    }

    /// Where the stem `name` is kept.
    pub fn stem(&self, name: &str) -> PathBuf {
        self.path.join("stems").join(format!("{name}.wav"))
    }

    fn product(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub fn beats(&self) -> Option<TrackedBeats> {
        read_json::<Beats>(&self.product("beats.json"))
            .map(|beats| TrackedBeats { beats: beats.beats, downbeats: beats.downbeats })
    }

    pub fn write_beats(&self, beats: &TrackedBeats) -> Result<(), String> {
        write_json(
            &self.product("beats.json"),
            &Beats { beats: beats.beats.clone(), downbeats: beats.downbeats.clone() },
        )
    }

    pub fn transcript(&self) -> Option<Transcript> {
        read_json(&self.product("words.json"))
    }

    pub fn write_transcript(&self, transcript: &Transcript) -> Result<(), String> {
        write_json(&self.product("words.json"), transcript)
    }

    pub fn lyrics(&self) -> Option<Lyrics> {
        read_json(&self.product("lyrics.json"))
    }

    pub fn write_lyrics(&self, lyrics: &Lyrics) -> Result<(), String> {
        write_json(&self.product("lyrics.json"), lyrics)
    }

    /// The outline of each stem, in the stems' order: what a waveform is
    /// drawn from without reading the stem, and after the stems were dropped.
    pub fn outlines(&self) -> Option<Outlines> {
        read_json(&self.product("outlines.json"))
    }

    pub fn write_outlines(&self, stems: &[trill_listen::Stem]) -> Result<(), String> {
        let outlines = Outlines { per_second: OUTLINE_PER_SECOND, stems: stems.iter().map(Outline::of).collect() };
        write_json(&self.product("outlines.json"), &outlines)
    }

    pub fn heard(&self) -> Option<Heard> {
        read_json(&self.product("heard.json"))
    }

    pub fn write_heard(&self, heard: &Heard) -> Result<(), String> {
        write_json(&self.product("heard.json"), heard)
    }

    /// Forgets what was heard in the stems, so that it is heard again:
    /// over a grid that was corrected.
    pub fn forget_heard(&self) {
        let _ = std::fs::remove_file(self.product("heard.json"));
    }

    /// The recording itself where the study keeps a copy of it.
    pub fn kept_source(&self) -> Option<PathBuf> {
        std::fs::read_dir(&self.path).ok()?.filter_map(|entry| Some(entry.ok()?.path())).find(|path| {
            path.file_stem().is_some_and(|stem| stem == "source")
                && path.extension().is_some_and(|ending| ending != "partial")
        })
    }

    /// The room the study takes on disk, in bytes.
    pub fn size(&self) -> u64 {
        trill_fetch::folder_size(&self.path)
    }

    /// Removes the stems and keeps what was found in them: a third of a
    /// megabyte in place of a couple of hundred. They can be made again.
    pub fn drop_stems(&self) -> Result<(), String> {
        let stems = self.path.join("stems");
        if stems.is_dir() {
            std::fs::remove_dir_all(&stems).map_err(|error| format!("{}: {error}", stems.display()))?;
        }
        Ok(())
    }
}

/// How many columns of an outline there are to a second of the stem:
/// enough to draw a whole song across a window, and a third of a megabyte
/// for four stems of four minutes. Closer in, the stem itself is read.
pub const OUTLINE_PER_SECOND: f64 = 50.0;

/// The outline of one stem: the lowest and the highest it goes in each
/// column, both channels together, in 127ths of full scale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outline {
    pub name: String,
    pub min: Vec<i8>,
    pub max: Vec<i8>,
}

impl Outline {
    /// The outline of `stem`.
    pub fn of(stem: &trill_listen::Stem) -> Self {
        let per_column = (trill_listen::RATE / OUTLINE_PER_SECOND) as usize;
        let frames = stem.sound.left.len();
        let columns = frames.div_ceil(per_column);
        let (mut min, mut max) = (Vec::with_capacity(columns), Vec::with_capacity(columns));
        let scaled = |sample: f32| (sample.clamp(-1.0, 1.0) * 127.0).round() as i8;
        for column in 0..columns {
            let (from, to) = (column * per_column, ((column + 1) * per_column).min(frames));
            let (mut low, mut high) = (0.0f32, 0.0f32);
            for channel in [&stem.sound.left, &stem.sound.right] {
                for &sample in channel.get(from..to.min(channel.len())).unwrap_or_default() {
                    low = low.min(sample);
                    high = high.max(sample);
                }
            }
            min.push(scaled(low));
            max.push(scaled(high));
        }
        Self { name: stem.name.clone(), min, max }
    }
}

/// What `outlines.json` holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outlines {
    /// How many columns there are to a second.
    pub per_second: f64,
    pub stems: Vec<Outline>,
}

/// What `beats.json` holds.
#[derive(Serialize, Deserialize)]
struct Beats {
    beats: Vec<f64>,
    downbeats: Vec<f64>,
}

/// Every study in one folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Studies {
    pub root: PathBuf,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// A name for a new study's folder: ten letters that no other has.
fn new_id(root: &Path) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos());
    let mut seed = (nanos as u64) ^ (u64::from(std::process::id()) << 40);
    loop {
        // A few turns of a generator, so that two made in one moment differ.
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let id = format!("{:010x}", seed & 0xff_ffff_ffff);
        if !root.join(&id).exists() {
            return id;
        }
    }
}

impl Studies {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Makes the folder of a new study of `source`, called `title`.
    pub fn create(&self, title: &str, source: Source, options: StudyOptions) -> Result<(StudyFolder, Study), String> {
        std::fs::create_dir_all(&self.root).map_err(|error| format!("{}: {error}", self.root.display()))?;
        let id = new_id(&self.root);
        let folder = StudyFolder::new(self.root.join(&id));
        std::fs::create_dir_all(&folder.path).map_err(|error| format!("{}: {error}", folder.path.display()))?;
        let study = Study {
            id,
            title: title.to_owned(),
            source,
            created: now(),
            seconds: 0.0,
            options,
            stems: Vec::new(),
            stages: Vec::new(),
            state: State::Waiting,
            error: None,
            device: None,
            corrections: Vec::new(),
        };
        folder.write(&study)?;
        Ok((folder, study))
    }

    pub fn folder(&self, id: &str) -> StudyFolder {
        StudyFolder::new(self.root.join(id))
    }

    /// The study `id`, or a sentence that names the ones there are.
    pub fn open(&self, id: &str) -> Result<(StudyFolder, Study), String> {
        // An id is a folder's name and nothing else: never a way out of the root.
        let plain =
            !id.is_empty() && id.chars().all(|letter| letter.is_ascii_alphanumeric() || letter == '-' || letter == '_');
        let folder = self.folder(id);
        if !plain || !folder.path.join("study.json").is_file() {
            let known: Vec<String> =
                self.list().into_iter().map(|study| format!("{} ({})", study.id, study.title)).collect();
            return Err(if known.is_empty() {
                format!("there is no study '{id}', and none has been made yet")
            } else {
                format!("there is no study '{id}'. Studies: {}", known.join(", "))
            });
        }
        let study = folder.study()?;
        Ok((folder, study))
    }

    /// Every study that can be read, latest first.
    pub fn list(&self) -> Vec<Study> {
        let mut studies: Vec<Study> = std::fs::read_dir(&self.root)
            .map(|entries| {
                entries.filter_map(Result::ok).filter_map(|entry| StudyFolder::new(entry.path()).study().ok()).collect()
            })
            .unwrap_or_default();
        studies.sort_by(|a, b| b.created.cmp(&a.created).then(a.id.cmp(&b.id)));
        studies
    }

    /// Removes the study `id` and everything in its folder.
    pub fn delete(&self, id: &str) -> Result<Study, String> {
        let (folder, study) = self.open(id)?;
        std::fs::remove_dir_all(&folder.path).map_err(|error| format!("{}: {error}", folder.path.display()))?;
        Ok(study)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_study_is_made_found_again_and_removed() {
        let root = tempfile::tempdir().unwrap();
        let studies = Studies::new(root.path().join("studies"));
        assert!(studies.list().is_empty());
        let source = Source::File { path: PathBuf::from("D:/music/Swansong.flac") };
        let (folder, study) = studies.create("Swansong", source.clone(), StudyOptions::default()).unwrap();
        assert_eq!(study.id.len(), 10);
        assert_eq!((study.state, study.title.as_str()), (State::Waiting, "Swansong"));
        let (again, read) = studies.open(&study.id).unwrap();
        assert_eq!((again, &read), (folder.clone(), &study));
        let (_, second) = studies.create("Another", source, StudyOptions::default()).unwrap();
        assert_ne!(second.id, study.id);
        assert_eq!(studies.list().len(), 2);

        // What is not a study is said with the ones that are.
        let missing = studies.open("nope").unwrap_err();
        assert!(
            missing.starts_with("there is no study 'nope'. Studies: ") && missing.contains("(Swansong)"),
            "{missing}"
        );
        assert!(studies.open("../studies").is_err());

        assert_eq!(studies.delete(&study.id).unwrap().title, "Swansong");
        assert!(!folder.path.exists());
        assert_eq!(studies.list().len(), 1);
    }

    #[test]
    fn a_product_is_written_whole_and_read_back() {
        let root = tempfile::tempdir().unwrap();
        let folder = StudyFolder::new(root.path().join("abc"));
        assert!(folder.beats().is_none() && folder.heard().is_none() && folder.lyrics().is_none());
        let beats = TrackedBeats { beats: vec![0.5, 1.0, 1.5], downbeats: vec![0.5] };
        folder.write_beats(&beats).unwrap();
        assert_eq!(folder.beats(), Some(beats));
        // Nothing is left under the name it was written by.
        assert!(!partial_of(&folder.path.join("beats.json")).exists());
        assert_eq!(folder.stem("drums"), folder.path.join("stems").join("drums.wav"));
        assert!(folder.size() > 20);

        std::fs::create_dir_all(folder.path.join("stems")).unwrap();
        std::fs::write(folder.stem("drums"), [0u8; 4096]).unwrap();
        assert!(folder.size() > 4096);
        folder.drop_stems().unwrap();
        assert!(folder.size() < 4096 && folder.beats().is_some());

        // The outline of a stem: its lowest and highest in each fiftieth of a second, and kept when the stems go.
        let mut left = vec![0.0f32; 44_100];
        (left[10], left[900], left[44_099]) = (0.5, -1.0, 0.25);
        let stem =
            trill_listen::Stem { name: "drums".into(), sound: trill_listen::Sound { right: vec![0.0; 44_100], left } };
        folder.write_outlines(std::slice::from_ref(&stem)).unwrap();
        let outlines = folder.outlines().unwrap();
        assert_eq!((outlines.per_second, outlines.stems.len()), (OUTLINE_PER_SECOND, 1));
        let outline = &outlines.stems[0];
        assert_eq!((outline.name.as_str(), outline.min.len(), outline.max.len()), ("drums", 50, 50));
        assert_eq!((outline.max[0], outline.min[0], outline.min[1], outline.max[1]), (64, 0, -127, 0));
        assert_eq!((outline.max[49], outline.max[20]), (32, 0));
        folder.drop_stems().unwrap();
        assert!(folder.outlines().is_some());
    }

    #[test]
    fn a_study_keeps_one_report_for_each_stage() {
        let root = tempfile::tempdir().unwrap();
        let (_, mut study) = Studies::new(root.path())
            .create("x", Source::Link { url: "https://example.org/a".into(), by: None }, StudyOptions::default())
            .unwrap();
        study.report(Stage::Stems).state = StageState::Running;
        study.report(Stage::Stems).note = "chunk 3 of 40".into();
        assert_eq!(study.stages.len(), 1);
        assert_eq!(study.running(), Some(Stage::Stems));
        assert_eq!(Stage::Stems.doing(), "Splitting it into stems");
        assert_eq!(Stage::WordTimes.while_doing(), "placing the words");
        let text = serde_json::to_string(&study).unwrap();
        assert_eq!(serde_json::from_str::<Study>(&text).unwrap(), study);
    }
}
