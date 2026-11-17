//! The desk studies are done at: one at a time, each in a process of its
//! own, in the order they were asked for.
//!
//! This is what a program holds of studies ([`Desk`]). Asking for one
//! ([`Desk::start`]) makes its folder and answers at once; a thread of the
//! desk's gives each its turn, starts the process that studies
//! ([`crate::helper`]) and attends it. A run is refused, before anything
//! is started, when the parts it needs have not been fetched or the
//! machine has not the memory free that a recording of its length takes;
//! it is ended when it takes more than that. However it ends, the study's
//! folder says so, and holds every stage that had finished: taking it up
//! again ([`Desk::resume`]) begins at the stage it was in.

use crate::helper::{Job, Order, Outcome, Said, attend};
use crate::room;
use crate::setup::Setup;
use crate::store::{LONGEST_SECONDS, Source, Stage, StageState, State, Studies, Study, StudyFolder, StudyOptions};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use trill_listen::Correction;

/// The file in a study's folder that names the process attending its run.
const ATTENDED_BY: &str = "run.pid";
/// What a recording whose length is not known yet is taken to be, in
/// seconds, when the machine is asked whether it has room.
const UNKNOWN_SECONDS: f64 = 6.0 * 60.0;
/// What a link costs to say, once: whose the recording is.
pub const WHOSE: &str = "A recording behind a link is somebody's: study only what you may. It is fetched by the program installed on this machine for that, kept in the study's own folder here, and goes nowhere else.";

/// What a desk is made of.
pub struct DeskOptions {
    /// The folder studies are kept in, one folder each.
    pub studies: PathBuf,
    pub setup: Arc<Setup>,
    /// The program that is started as the process that studies: one that
    /// calls [`crate::helper::run_if_requested`] first thing.
    pub program: PathBuf,
    /// The memory that is free, in place of asking the machine: for tests.
    pub free: Option<fn() -> Option<u64>>,
}

/// A study as it is now.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub study: Study,
    /// The stage it is at and how far through that it is (0 to 1), while it runs.
    pub at: Option<(Stage, f32)>,
    /// The room it takes on disk, in bytes.
    pub size: u64,
}

struct Running {
    id: String,
    stop: Arc<AtomicBool>,
    stage: Option<Stage>,
    share: f32,
}

struct Turns {
    root: PathBuf,
    waiting: VecDeque<String>,
    running: Option<Running>,
    /// Whether the thread that gives studies their turn is there.
    serving: bool,
    closing: bool,
}

struct Shared {
    setup: Arc<Setup>,
    program: PathBuf,
    free: fn() -> Option<u64>,
    turns: Mutex<Turns>,
    changed: Condvar,
}

/// The studies of a program, and the one that is being done.
pub struct Desk {
    shared: Arc<Shared>,
}

/// Writes how a run ended into the study's folder.
fn mark(folder: &StudyFolder, state: State, error: Option<String>) {
    let Ok(mut study) = folder.study() else { return };
    for report in &mut study.stages {
        if report.state == StageState::Running {
            report.state = if state == State::Failed { StageState::Failed } else { StageState::Pending };
        }
    }
    (study.state, study.error) = (state, error);
    if let Err(why) = folder.write(&study) {
        log::warn!("a study's state could not be written: {why}");
    }
}

impl Shared {
    fn studies(&self) -> Studies {
        Studies::new(self.turns.lock().root.clone())
    }

    /// The memory a run of `study` is taken to need, and the most it may take.
    fn needs(study: &Study) -> (u64, u64) {
        let known = if study.seconds > 0.0 {
            Some(study.seconds)
        } else if let Source::File { path } = &study.source {
            trill_media::probe(path).ok().and_then(|probed| probed.seconds())
        } else {
            None
        };
        match known {
            Some(seconds) => (room::need(seconds), room::need(seconds)),
            None => (room::need(UNKNOWN_SECONDS), room::need(LONGEST_SECONDS)),
        }
    }

    /// Why a run of `study` cannot be begun now, if it cannot.
    fn refused(&self, study: &Study) -> Result<(crate::models::Which, u64), String> {
        let models = self.setup.models_for(&study.options)?;
        let (need, limit) = Self::needs(study);
        if let Some(free) = (self.free)().filter(|&free| free < need) {
            return Err(format!(
                "there is not the memory free to study this now: a recording of its length takes about {}, and {} are free. Nothing was begun: close something, and run the study again",
                room::said(need),
                room::said(free)
            ));
        }
        Ok((models, limit))
    }

    /// One study's turn: its run in a process of its own, attended to its end.
    fn run(&self, id: &str, stop: &AtomicBool) {
        let Ok((folder, study)) = self.studies().open(id) else { return };
        let (models, limit) = match self.refused(&study) {
            Ok(ready) => ready,
            Err(why) => return mark(&folder, State::Failed, Some(why)),
        };
        let order = Order::Study { folder: folder.path.clone(), models, programs: self.setup.found() };
        let mut job = match Job::start(&self.program, &order) {
            Ok(job) => job,
            Err(why) => return mark(&folder, State::Failed, Some(why)),
        };
        let _ = std::fs::write(folder.path.join(ATTENDED_BY), std::process::id().to_string());
        let (outcome, held) = attend(&mut job, Some(limit), stop, &mut |said| {
            let mut turns = self.turns.lock();
            let Some(running) = turns.running.as_mut() else { return };
            match said {
                Said::Stage(stage) => (running.stage, running.share) = (Some(*stage), 0.0),
                Said::Progress(share) => running.share = share.clamp(0.0, 1.0),
                _ => {}
            }
        });
        let _ = std::fs::remove_file(folder.path.join(ATTENDED_BY));
        log::info!("study {id}: {outcome:?}, {} at the most", room::said(held.peak));
        match outcome {
            // The run wrote how it ended itself.
            Outcome::Done | Outcome::Checked(_) => {}
            Outcome::Stopped => mark(&folder, State::Stopped, None),
            Outcome::Failed(why) => mark(&folder, State::Failed, Some(why)),
        }
    }

    /// Gives every study that is waiting its turn, and ends when none is.
    fn serve(self: Arc<Self>) {
        loop {
            let (id, stop) = {
                let mut turns = self.turns.lock();
                let next = if turns.closing { None } else { turns.waiting.pop_front() };
                let Some(id) = next else {
                    turns.serving = false;
                    self.changed.notify_all();
                    return;
                };
                let stop = Arc::new(AtomicBool::new(false));
                turns.running = Some(Running { id: id.clone(), stop: stop.clone(), stage: None, share: 0.0 });
                (id, stop)
            };
            self.changed.notify_all();
            self.run(&id, &stop);
            self.turns.lock().running = None;
            self.changed.notify_all();
        }
    }

    /// Puts the study `id` in line, and has a thread serve the line.
    fn wait_turn(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let mut turns = self.turns.lock();
        turns.waiting.push_back(id.to_owned());
        if !turns.serving {
            let shared = self.clone();
            std::thread::Builder::new()
                .name("trill-studies".into())
                .spawn(move || shared.serve())
                .map_err(|error| format!("the thread that runs studies could not be started: {error}"))?;
            turns.serving = true;
        }
        Ok(())
    }

    /// Whether the study `id` is running or waiting here.
    fn ours(&self, id: &str) -> bool {
        let turns = self.turns.lock();
        turns.running.as_ref().is_some_and(|running| running.id == id) || turns.waiting.iter().any(|known| known == id)
    }

    /// A study as it is: one that its folder says is running, and that
    /// nothing is running, was left so by a program that ended.
    fn as_it_is(&self, folder: &StudyFolder, mut study: Study) -> Report {
        let at = {
            let turns = self.turns.lock();
            turns
                .running
                .as_ref()
                .filter(|running| running.id == study.id)
                .and_then(|running| Some((running.stage?, running.share)))
        };
        if matches!(study.state, State::Running | State::Waiting) && !self.ours(&study.id) {
            let elsewhere = std::fs::read_to_string(folder.path.join(ATTENDED_BY))
                .ok()
                .and_then(|pid| pid.trim().parse::<u32>().ok())
                .is_some_and(|pid| pid != std::process::id() && room::held_by(pid).is_some());
            if !elsewhere {
                study.state = State::Stopped;
                for report in &mut study.stages {
                    if report.state == StageState::Running {
                        report.state = StageState::Pending;
                    }
                }
            }
        }
        Report { size: folder.size(), study, at }
    }
}

impl Desk {
    pub fn new(options: DeskOptions) -> Self {
        Self {
            shared: Arc::new(Shared {
                setup: options.setup,
                program: options.program,
                free: options.free.unwrap_or(room::free),
                turns: Mutex::new(Turns {
                    root: options.studies,
                    waiting: VecDeque::new(),
                    running: None,
                    serving: false,
                    closing: false,
                }),
                changed: Condvar::new(),
            }),
        }
    }

    pub fn setup(&self) -> &Arc<Setup> {
        &self.shared.setup
    }

    /// The folder studies are kept in.
    pub fn folder(&self) -> PathBuf {
        self.shared.turns.lock().root.clone()
    }

    /// Keeps studies in `folder` from now on. The ones in the folder they
    /// were in stay there: whoever moves the studies moves those.
    pub fn set_folder(&self, folder: PathBuf) -> Result<(), String> {
        let mut turns = self.shared.turns.lock();
        if turns.running.is_some() || !turns.waiting.is_empty() {
            return Err("a study is running: stop it, or wait for it, before studies are kept elsewhere".to_owned());
        }
        turns.root = folder;
        Ok(())
    }

    /// The studies as they are on disk: for reading what one holds.
    pub fn studies(&self) -> Studies {
        self.shared.studies()
    }

    /// Whether a study is running or waiting for its turn.
    pub fn busy(&self) -> bool {
        let turns = self.shared.turns.lock();
        turns.running.is_some() || !turns.waiting.is_empty()
    }

    /// Asks for a study of `source`, and answers once its folder is made:
    /// the run is done when its turn comes. Refused, with nothing made,
    /// when the parts it needs are not here, when the recording is not,
    /// or when the machine has not the memory for it and nothing else is
    /// running that could give some back.
    pub fn start(&self, source: Source, options: StudyOptions, title: Option<&str>) -> Result<Study, String> {
        let title = match (&source, title.map(str::trim).filter(|title| !title.is_empty())) {
            (_, Some(title)) => title.to_owned(),
            (Source::File { path }, None) => {
                path.file_stem().map_or_else(|| "A recording".to_owned(), |stem| stem.to_string_lossy().into_owned())
            }
            (Source::Link { url, .. }, None) => url.clone(),
        };
        match &source {
            Source::File { path } => {
                if !path.is_file() {
                    return Err(format!("there is no file at {}", path.display()));
                }
            }
            Source::Link { url, .. } => {
                if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(char::is_whitespace) {
                    return Err(format!("'{url}' is not a link: one begins with https://"));
                }
                let found = self.shared.setup.found();
                let lacking: Vec<&str> = [("yt-dlp", found.fetcher.is_none()), ("ffmpeg", found.ffmpeg.is_none())]
                    .into_iter()
                    .filter_map(|(name, lacking)| lacking.then_some(name))
                    .collect();
                if !lacking.is_empty() {
                    return Err(format!(
                        "a link is fetched by yt-dlp, which needs ffmpeg, and {} not installed where programs are looked for. Trill ships neither: install {} (Settings, Studies says how) and ask again, or study a file you have",
                        if lacking.len() == 1 { format!("{} is", lacking[0]) } else { "both are".to_owned() },
                        if lacking.len() == 1 { "it" } else { "them" },
                    ));
                }
            }
        }
        let studies = self.shared.studies();
        let draft = Study {
            id: String::new(),
            title: title.clone(),
            source: source.clone(),
            created: 0,
            seconds: 0.0,
            options: options.clone(),
            stems: Vec::new(),
            stages: Vec::new(),
            state: State::Waiting,
            error: None,
            device: None,
            corrections: Vec::new(),
        };
        if self.busy() {
            // Its turn will say whether there is room then.
            self.shared.setup.models_for(&options)?;
        } else {
            self.shared.refused(&draft)?;
        }
        let (_, study) = studies.create(&title, source, options)?;
        self.shared.wait_turn(&study.id)?;
        Ok(study)
    }

    /// Takes the study `id` up where it was: every stage that is not done.
    pub fn resume(&self, id: &str) -> Result<Study, String> {
        let (folder, mut study) = self.shared.studies().open(id)?;
        if self.shared.ours(id) {
            return Err(format!("study {id} is running already"));
        }
        if study.state == State::Done && folder.heard().is_some() {
            return Err(format!("study {id} is done: there is nothing of it left to do"));
        }
        self.shared.setup.models_for(&study.options)?;
        (study.state, study.error) = (State::Waiting, None);
        folder.write(&study)?;
        self.shared.wait_turn(id)?;
        Ok(study)
    }

    /// Stops the study `id`: the process that runs it is ended, and every
    /// stage that had finished is kept.
    pub fn stop(&self, id: &str) -> Result<Study, String> {
        let (folder, study) = self.shared.studies().open(id)?;
        let mut turns = self.shared.turns.lock();
        if let Some(running) = turns.running.as_ref().filter(|running| running.id == id) {
            running.stop.store(true, Ordering::Relaxed);
            return Ok(study);
        }
        let before = turns.waiting.len();
        turns.waiting.retain(|known| known != id);
        if turns.waiting.len() == before {
            return Err(format!("study {id} is not running"));
        }
        drop(turns);
        mark(&folder, State::Stopped, None);
        folder.study()
    }

    pub fn report(&self, id: &str) -> Result<Report, String> {
        let (folder, study) = self.shared.studies().open(id)?;
        Ok(self.shared.as_it_is(&folder, study))
    }

    /// Every study, latest first.
    pub fn list(&self) -> Vec<Report> {
        let studies = self.shared.studies();
        studies.list().into_iter().map(|study| self.shared.as_it_is(&studies.folder(&study.id), study)).collect()
    }

    /// Waits until the study `id` is neither running nor waiting, for
    /// `within` at the longest, and says how it is then.
    pub fn wait(&self, id: &str, within: Duration) -> Result<Report, String> {
        let until = Instant::now() + within;
        let mut turns = self.shared.turns.lock();
        loop {
            let busy = turns.running.as_ref().is_some_and(|running| running.id == id)
                || turns.waiting.iter().any(|known| known == id);
            if !busy || self.shared.changed.wait_until(&mut turns, until).timed_out() {
                break;
            }
        }
        drop(turns);
        self.report(id)
    }

    /// Removes the study `id` with everything in its folder; one that is
    /// running is stopped first.
    pub fn delete(&self, id: &str) -> Result<Study, String> {
        self.shared.studies().open(id)?;
        if self.shared.ours(id) {
            self.stop(id)?;
            if self.shared.ours(id) && self.wait(id, Duration::from_secs(20)).is_ok() && self.shared.ours(id) {
                return Err(format!("study {id} is running and did not stop: ask again in a moment"));
            }
        }
        self.shared.studies().delete(id)
    }

    /// Corrects the beat grid of the study `id` and hears everything that
    /// follows from the grid again: its chords, parts and loops. No model
    /// is run for it.
    pub fn correct(&self, id: &str, corrections: Vec<Correction>) -> Result<Study, String> {
        let (folder, mut study) = self.shared.studies().open(id)?;
        if self.shared.ours(id) {
            return Err(format!("study {id} is running: its grid is corrected when it is done"));
        }
        if study.stems.is_empty() || folder.beats().is_none() {
            return Err(format!("study {id} has no beats yet to correct: take it up again first"));
        }
        if study.corrections == corrections && folder.heard().is_some() {
            return Ok(study);
        }
        (study.corrections, study.state, study.error) = (corrections, State::Waiting, None);
        folder.write(&study)?;
        folder.forget_heard();
        self.shared.wait_turn(id)?;
        Ok(study)
    }
}

impl Drop for Desk {
    /// A program that ends takes its study down with it: the run is taken
    /// up again where it was, the next time.
    fn drop(&mut self) {
        let mut turns = self.shared.turns.lock();
        turns.closing = true;
        turns.waiting.clear();
        if let Some(running) = &turns.running {
            running.stop.store(true, Ordering::Relaxed);
        }
        let until = Instant::now() + Duration::from_secs(10);
        while turns.serving {
            if self.shared.changed.wait_until(&mut turns, until).timed_out() {
                break;
            }
        }
    }
}
