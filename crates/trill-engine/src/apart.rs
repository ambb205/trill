//! Jobs done apart: a render, a measurement or a probe of a song with
//! plugins, in a process of its own.
//!
//! A plugin runs in the process that hosts it, and a plugin that faults
//! takes that process down. While a song plays that is the price of
//! hosting plugins at all. A render, a measurement and a probe need not
//! pay it: each makes every device afresh, runs offline and hands back
//! numbers and files, so each can be done by another process, and is,
//! wherever the session was given one ([`Session::do_jobs_apart`]). Three
//! things come of it:
//!
//! - **A plugin that crashes costs a sentence.** The process that dies is
//!   the one that was rendering. The song that is open is untouched, what
//!   the job had begun to write is removed, and the error says what
//!   happened, at which stage, and which plugin it was if that can be
//!   told ([`Crash`]).
//! - **The same render twice.** Some plugins keep things for all their
//!   instances in a process: a stream of chances from which they draw
//!   which recording to play. Nothing a host does puts that back while an
//!   instance lives, and the song's own always does. A process that has
//!   just started begins it at the same place every time.
//! - Nothing is taken down. A process that has done its one job ends
//!   where it stands, its plugins as they are: taking a plugin down is
//!   where a plugin is most likely to fault, and a process that is going
//!   has no reason to.
//!
//! **What stays here.** A song of built-in devices is rendered in this
//! process as it always was: nothing in it can take the process down that
//! is not Trill's own, and it renders to the same samples every time
//! already. So there are two places a render may run and one way it is
//! made: both end in the same function with the same project.
//!
//! **What crosses.** To the process, as one line of JSON on its standard
//! input ([`Order`]): the project as it is now, each plugin's state as
//! its running instance has it (the session takes that down before it
//! hands out a job), the options, how to make the devices (which whoever
//! set this up says, and which the engine does not look into), and what
//! is remembered of plugins from earlier waits ([`crate::ready::Memory`]),
//! without which every key that plays nothing would be waited out anew.
//! The song's audio files cross as their paths: the process decodes and
//! stretches them itself. Back, as lines on its standard output
//! ([`Said`]): which stage it is at, how far it has come, what it
//! measured, the problems it met, and what it now remembers. The mix
//! comes back as a file, and stems are written where they were asked for.
//! Plugins write to the standard output as well (some say what they are
//! doing there), so every line of the process's own carries a mark and
//! everything else is passed over.
//!
//! **Fresh, or kept.** A render and a measurement each get a process that
//! has just started, which is what makes them repeat and what makes
//! leaving the plugins as they are possible. A probe is small and comes
//! in dozens: whoever sets this up says whether probes share a process
//! that is kept between them ([`Setup::keep_for_probes`]). One that is
//! kept is ended when it has not been asked anything for a while, and is
//! replaced by a new one after a fault.
//!
//! **Stopping.** Whoever watches a render ([`crate::render::Watch`]) stops
//! it by ending the process. Files are written under other names until
//! they are whole, and those are removed, so that nothing half-written is
//! left under a name anyone asked for. A process whose asker has gone
//! ends itself: its standard input closes.
//!
//! **Memory.** A process of its own holds the song's plugins a second
//! time, beside the ones that play. Before one is started the machine is
//! asked how much memory it has left ([`Room`]), and a job it has no
//! room for is refused with a sentence, not begun. One render runs at a
//! time.

mod fault;
mod room;
mod serve;

pub use room::Room;
pub use serve::{Make, serve};

use crate::compile::Problem;
use crate::probe::{ProbeError, ProbeJob, ProbeNote, ProbeOptions, Probed, Readiness};
use crate::ready::{self, Plugins};
use crate::render::{Measured, RenderError, RenderJob, RenderOptions, Rendered, WaitedFor, Watch, devices_made};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use trill_core::device::DeviceFactory;
use trill_core::presets::PresetInfo;
use trill_core::{Device, DeviceId, Project, TrackId};

/// Set in the environment of a process that is started to do jobs for
/// another: a program that can be one looks for it first thing.
pub const JOB_ENV: &str = "TRILL_JOB";
/// The variable that has such a process say more than a program does, in
/// the log of the program that started it: `debug`, or `trace` for every
/// try of every wait for a plugin ([`crate::ready`]), which is how to
/// find out what an instrument was asked before a render began.
pub const LOG_ENV: &str = "TRILL_JOB_LOG";
/// What every line of the process's own begins with.
const MARK: &str = "@trill-job@";
/// What the process says first; a program that says anything else is not
/// one that does jobs.
const VERSION: u32 = 1;
/// How long a process may take to say that it is there.
const HELLO_WITHIN: Duration = Duration::from_secs(60);
/// How often whoever waits for a process looks whether it is to be
/// stopped.
const LOOK_EVERY: Duration = Duration::from_millis(50);
/// How long a process that has answered is given to end by itself.
const END_WITHIN: Duration = Duration::from_secs(5);
/// How long a process that is kept may go unasked before it is ended.
const KEPT_FOR: Duration = Duration::from_secs(45);
/// The least memory that must be free for any job to be started.
const LEAST_FREE: u64 = 1 << 30;
/// How much of what the process's standard error said is kept for an
/// error message.
const TAIL: usize = 1500;

/// Where a job is in its course, as its process says before each part:
/// what was going on when a process went down.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    /// The device is being made.
    Making(DeviceId),
    /// The plugins are being waited for.
    Waiting,
    /// The song, or the notes of a probe, are being played.
    Playing,
    /// The devices are being let go of.
    LettingGo,
}

/// What becomes of the mix of a render that is done apart.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Mix<'a> {
    /// It is measured and let go of.
    Nothing,
    /// It comes back as samples.
    Samples,
    /// It is written to this file.
    File(&'a Path),
}

/// What a process is asked: one line of JSON on its standard input.
#[derive(Serialize, Deserialize)]
pub(crate) struct Order {
    /// How to make the devices: what [`Setup::recipe`] said of this job.
    devices: serde_json::Value,
    /// What is remembered of plugins from earlier waits.
    memory: ready::Memory,
    /// Nothing follows: the process ends when this is done, leaving its
    /// devices as they are.
    last: bool,
    job: Job,
}

#[derive(Serialize, Deserialize)]
enum Job {
    Render { project: Box<Project>, options: RenderOptions, keep: Keep },
    Probe { devices: Vec<Device>, preset: Option<PresetInfo>, tempo: f64, notes: Vec<ProbeNote>, options: ProbeOptions },
    Ready { devices: Vec<Device>, tempo: f64, pitches: Vec<u8>, patience: Duration },
}

/// What a process does with the mix it renders.
#[derive(Serialize, Deserialize)]
enum Keep {
    Nothing,
    /// Written as 32-bit floats, for whoever asked to read back.
    Samples(PathBuf),
    /// Written as a 24-bit file: the render's file.
    File(PathBuf),
}

/// What a process says: one line of JSON each on its standard output,
/// after [`MARK`].
#[derive(Serialize, Deserialize)]
enum Said {
    /// First of all: it is a process that does jobs.
    Hello(u32),
    Stage(Stage),
    /// How far a render has come, from 0 to 1.
    Progress(f32),
    /// It is going down: the exception's code, and the file of the module
    /// the fault was in.
    Fault {
        code: u32,
        module: String,
    },
    Rendered(Box<Told>),
    Probed(Probed),
    Ready(Readiness),
    Failed(Failure),
    /// What it now remembers of plugins.
    Memory(ready::Memory),
    /// The job is over.
    Done,
}

/// What a render that was done apart found, without its samples.
#[derive(Serialize, Deserialize)]
struct Told {
    sample_rate: u32,
    track_peaks: Vec<(TrackId, f32)>,
    track_levels: Vec<(TrackId, Vec<f32>)>,
    problems: Vec<Problem>,
    measured: Measured,
}

/// How a job that was done apart failed, where its process lived to say.
#[derive(Serialize, Deserialize)]
enum Failure {
    EmptyRange {
        start: f64,
        end: f64,
    },
    NotAnswered(Vec<WaitedFor>),
    /// A probe's or a render's own words for it.
    Other(String),
}

/// A job's process was taken down. Its `Display` is the sentence for
/// whoever asked.
#[derive(Clone, Debug, PartialEq)]
pub struct Crash {
    /// What was being done: "render", "measurement", "probe".
    pub job: &'static str,
    /// What it was doing when it went, if it had said.
    pub stage: Option<Stage>,
    /// The exception or the exit code it went with.
    pub code: Option<u32>,
    /// The file of the module the fault was in, where the process said.
    pub module: Option<PathBuf>,
    /// The devices of the job that are that module's, or the one that was
    /// being made: each as its name and where it is.
    pub devices: Vec<String>,
    /// The last the process wrote to its standard error, where nothing
    /// better is known.
    pub last_words: String,
}

impl std::fmt::Display for Crash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let when = match self.stage {
            Some(Stage::Making(_)) => " while its devices were being made",
            Some(Stage::Waiting) => " while its plugins were waited for",
            Some(Stage::Playing) => " while it was being played",
            Some(Stage::LettingGo) => " as its devices were let go of",
            None => "",
        };
        let how = match self.code {
            Some(0xC000_0005) => "an access violation".to_string(),
            Some(0xC000_00FD) => "a stack overflow".to_string(),
            Some(0xC000_001D) => "an illegal instruction".to_string(),
            Some(0xC000_0094) => "a division by zero".to_string(),
            Some(0xC000_0374) => "a corrupted heap".to_string(),
            Some(0xC000_0409) => "a check that failed inside it".to_string(),
            Some(code) => format!("code {code:#X}"),
            None => "no word of why".to_string(),
        };
        let file = self.module.as_deref().and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned());
        let place = file.map(|file| format!(" in {file}")).unwrap_or_default();
        match self.devices.as_slice() {
            [] => write!(f, "the {} was taken down{when}, with {how}{place}", self.job)?,
            devices => {
                write!(f, "a plugin took the {} down{when}: {} ({how}{place})", self.job, devices.join(" and "))?
            }
        }
        write!(f, ". It ran in a process of its own, so the song that is open is untouched")?;
        // A render writes files, and what it had begun is cleared away.
        if self.job == "render" {
            write!(f, ", and what it had begun to write was removed")?;
        }
        write!(f, ". Try again; if it happens again, ")?;
        match (self.job, self.devices.is_empty()) {
            ("probe", _) => write!(f, "probe the instrument without the devices after it, or as it comes")?,
            // (In a window that is soloing a track; for a tool, naming it.)
            (_, true) => write!(f, "hear the tracks one at a time to find which device it is")?,
            (_, false) => write!(f, "mute that track or take the device out to go on without it")?,
        }
        if self.devices.is_empty() && self.module.is_none() && !self.last_words.is_empty() {
            write!(f, ". The last it said: {}", self.last_words)?;
        }
        Ok(())
    }
}

/// How a job says how to make its devices: given the devices it will
/// make, what the process is to be told ([`serve`] hands it to whoever
/// makes the devices there), or `None` for a job that no other process
/// could do, which is then done here.
pub type Recipe = Box<dyn Fn(&[Device]) -> Option<serde_json::Value> + Send + Sync>;

/// What it takes to do jobs apart.
pub struct Setup {
    /// The program to start: one that calls [`serve`] when it finds
    /// [`JOB_ENV`] in its environment.
    pub program: PathBuf,
    /// What it is started with: nothing, for a program that finds out
    /// what it is by its environment. A test's own program is told which
    /// of its tests is the one that serves ([`Setup::by_test`]).
    pub arguments: Vec<String>,
    pub recipe: Recipe,
    /// Whether probes share a process that is kept between them. With
    /// `false` each has a process that has just started, as every render
    /// has.
    pub keep_for_probes: bool,
    /// Whether renders are done by the process that is kept, too. They
    /// then take their devices down as a render in this process does, and
    /// do not repeat a plugin's chances. For measuring what a fresh
    /// process costs, and for nothing else.
    pub keep_for_renders: bool,
    /// How much memory the machine has left: [`Room::now`], but for tests.
    pub room: fn() -> Option<Room>,
}

impl Setup {
    /// Jobs done by `program`, each in a process that has just started.
    pub fn new(program: PathBuf, recipe: Recipe) -> Self {
        let arguments = Vec::new();
        Self { program, arguments, recipe, keep_for_probes: false, keep_for_renders: false, room: Room::now }
    }

    /// Jobs done by the test program that is running, started again to
    /// run its test `test` and no other: a test that calls [`serve`] when
    /// it finds [`JOB_ENV`] in its environment, and does nothing
    /// otherwise. For the tests of a crate that builds no program which
    /// could do them.
    pub fn by_test(test: &str, recipe: Recipe) -> std::io::Result<Self> {
        let arguments = [test, "--exact", "--nocapture", "--test-threads=1"].map(str::to_string).to_vec();
        Ok(Self { arguments, ..Self::new(std::env::current_exe()?, recipe) })
    }
}

/// Where jobs are done apart: a session is given one, and hands it to
/// every job it makes. Cloning is cheap and yields a handle to the same.
#[derive(Clone)]
pub struct Apart {
    inner: Arc<Inner>,
}

struct Inner {
    setup: Setup,
    /// One render at a time.
    rendering: Mutex<()>,
    /// The process that is kept, while there is one.
    kept: Mutex<Option<Worker>>,
    /// Whether a thread is looking after the kept process's age.
    minded: AtomicBool,
    /// Counts the jobs that were handed to a process, and the processes
    /// that were started for them.
    jobs: AtomicU64,
    started: AtomicU64,
}

impl Apart {
    pub fn new(setup: Setup) -> Self {
        let inner = Inner {
            setup,
            rendering: Mutex::new(()),
            kept: Mutex::new(None),
            minded: AtomicBool::new(false),
            jobs: AtomicU64::new(0),
            started: AtomicU64::new(0),
        };
        Self { inner: Arc::new(inner) }
    }

    /// The program that does the jobs.
    pub fn program(&self) -> &Path {
        &self.inner.setup.program
    }

    /// How many jobs have been handed to a process so far, and how many
    /// processes were started for them: fewer where one is kept.
    pub fn asked(&self) -> (u64, u64) {
        (self.inner.jobs.load(Ordering::Relaxed), self.inner.started.load(Ordering::Relaxed))
    }

    /// Renders `job` in a process of its own; `None` for a job that is
    /// done here: a song without plugins, or one whose devices no other
    /// process could make.
    pub(crate) fn render(
        &self,
        job: &RenderJob,
        options: &RenderOptions,
        mix: Mix<'_>,
    ) -> Option<Result<Rendered, RenderError>> {
        let devices = devices_made(&job.project, options);
        if !devices.iter().any(|device| Plugins::is_plugin(&device.uri)) {
            return None;
        }
        let recipe = (self.inner.setup.recipe)(&devices)?;
        Some(self.render_apart(job, options, mix, recipe, &devices))
    }

    fn render_apart(
        &self,
        job: &RenderJob,
        options: &RenderOptions,
        mix: Mix<'_>,
        recipe: serde_json::Value,
        devices: &[Device],
    ) -> Result<Rendered, RenderError> {
        let what = if matches!(mix, Mix::Nothing) { "measurement" } else { "render" };
        // One render at a time: each holds the song's plugins once more.
        let _one = self.inner.rendering.lock();
        let plugins = |devices: &[Device]| devices.iter().filter(|device| Plugins::is_plugin(&device.uri)).count();
        let all: Vec<Device> = job.project.devices().cloned().collect();
        let share = plugins(devices) as f64 / plugins(&all).max(1) as f64;
        self.room_for(what, share.min(1.0)).map_err(RenderError::NoRoom)?;

        // Where the samples come back, and what there is to clear away if
        // the process does not live to finish.
        // (Named by this process and a number no other render of it has:
        // a program may do jobs for several sessions at once.)
        static HANDED_BACK: AtomicU64 = AtomicU64::new(0);
        let number = HANDED_BACK.fetch_add(1, Ordering::Relaxed);
        let samples = std::env::temp_dir().join("trill-jobs").join(format!("{}-{number}.wav", std::process::id()));
        let keep = match mix {
            Mix::Nothing => Keep::Nothing,
            Mix::Samples => Keep::Samples(samples.clone()),
            Mix::File(path) => Keep::File(path.to_path_buf()),
        };
        let stems = options.stems.as_ref().map(|stems| stems.folder.clone());
        let made_folder = stems.clone().filter(|folder| !folder.exists());
        let cleared = |pid: u32| {
            let written = match mix {
                Mix::File(path) => path.parent().map(Path::to_path_buf),
                _ => samples.parent().map(Path::to_path_buf),
            };
            for folder in written.iter().chain(&stems) {
                sweep(folder, pid);
            }
            let _ = std::fs::remove_file(&samples);
            if let Some(folder) = &made_folder {
                let _ = std::fs::remove_dir(folder);
            }
        };

        let kept = self.inner.setup.keep_for_renders;
        let order = Order {
            devices: recipe,
            memory: ready::remembered(),
            last: !kept,
            job: Job::Render { project: Box::new(job.project.clone()), options: options.clone(), keep },
        };
        let watch = options.watch.as_deref();
        let (asked, pid) = self.ask(&order, watch, kept).map_err(RenderError::Apart)?;
        match asked {
            Asked::Said(Said::Rendered(told)) => {
                let Told { sample_rate, track_peaks, track_levels, problems, measured } = *told;
                let (left, right) = match mix {
                    Mix::Samples => {
                        let read = trill_media::decode(&samples);
                        let _ = std::fs::remove_file(&samples);
                        let audio = read.map_err(RenderError::File)?;
                        (audio.left, audio.right)
                    }
                    _ => (Vec::new(), Vec::new()),
                };
                Ok(Rendered { sample_rate, left, right, track_peaks, track_levels, problems, measured })
            }
            Asked::Said(Said::Failed(failure)) => {
                cleared(pid);
                Err(match failure {
                    Failure::EmptyRange { start, end } => RenderError::EmptyRange { start, end },
                    Failure::NotAnswered(waited) => RenderError::NotAnswered(waited),
                    Failure::Other(reason) => RenderError::Apart(reason),
                })
            }
            Asked::Said(_) => {
                cleared(pid);
                Err(RenderError::Apart(format!("the process that did the {what} answered something else")))
            }
            Asked::Stopped => {
                cleared(pid);
                Err(RenderError::Stopped)
            }
            Asked::Gone(gone) => {
                cleared(pid);
                let crash = gone.crash(what, devices, job.factory.as_ref(), Some(&job.project));
                Err(RenderError::Crashed(Box::new(crash)))
            }
        }
    }

    /// Plays `notes` through the devices of `job` in a process of its
    /// own; `None` for a probe that is done here.
    pub(crate) fn probe(
        &self,
        job: &ProbeJob,
        notes: &[ProbeNote],
        options: &ProbeOptions,
    ) -> Option<Result<Probed, ProbeError>> {
        let asked = Job::Probe {
            devices: job.devices.clone(),
            preset: job.preset.clone(),
            tempo: job.tempo,
            notes: notes.to_vec(),
            options: options.clone(),
        };
        Some(self.small(job, asked, "probe")?.and_then(|said| match said {
            Said::Probed(probed) => Ok(probed),
            _ => Err(ProbeError::Apart("the process that did the probe answered something else".into())),
        }))
    }

    /// Whether the devices of `job` are ready, found out in a process of
    /// its own; `None` where that is found out here.
    pub(crate) fn ready(
        &self,
        job: &ProbeJob,
        pitches: &[u8],
        patience: Duration,
    ) -> Option<Result<Readiness, ProbeError>> {
        let asked = Job::Ready { devices: job.devices.clone(), tempo: job.tempo, pitches: pitches.to_vec(), patience };
        Some(self.small(job, asked, "probe")?.and_then(|said| match said {
            Said::Ready(readiness) => Ok(readiness),
            _ => Err(ProbeError::Apart("the process that did the probe answered something else".into())),
        }))
    }

    /// A job on a few devices, done by the process that is kept if one
    /// is, and by a new one if not.
    fn small(&self, job: &ProbeJob, asked: Job, what: &'static str) -> Option<Result<Said, ProbeError>> {
        if !job.devices.iter().any(|device| Plugins::is_plugin(&device.uri)) {
            return None;
        }
        let recipe = (self.inner.setup.recipe)(&job.devices)?;
        if let Err(refusal) = self.room_for(what, 0.0) {
            return Some(Err(ProbeError::Apart(refusal)));
        }
        let kept = self.inner.setup.keep_for_probes;
        let order = Order { devices: recipe, memory: ready::remembered(), last: !kept, job: asked };
        let asked = match self.ask(&order, None, kept) {
            Ok((asked, _)) => asked,
            Err(reason) => return Some(Err(ProbeError::Apart(reason))),
        };
        Some(match asked {
            Asked::Said(Said::Failed(Failure::Other(reason))) => Err(ProbeError::Device(reason)),
            Asked::Said(Said::Failed(_)) | Asked::Stopped => Err(ProbeError::Apart(format!("the {what} was not done"))),
            Asked::Said(said) => Ok(said),
            Asked::Gone(gone) => {
                let mut crash = gone.crash(what, &job.devices, job.factory.as_ref(), None);
                // Nothing else was running: it was one of these.
                if crash.devices.is_empty() {
                    let plugins = job.devices.iter().filter(|device| Plugins::is_plugin(&device.uri));
                    crash.devices = plugins.map(|device| device.name.clone()).collect();
                }
                Err(ProbeError::Apart(crash.to_string()))
            }
        })
    }

    /// Refuses a job the machine has no room for. `share` is how much of
    /// the song's plugins the job makes.
    fn room_for(&self, what: &str, share: f64) -> Result<(), String> {
        let Some(room) = (self.inner.setup.room)() else { return Ok(()) };
        // A process of its own holds the plugins it makes once more: what
        // this one holds for them now and an eighth over, since it has
        // their code and what they share to itself (measured: a song that
        // held 8.0 GB open took 8.6 GB there, one of 2.9 GB took 3.2 GB).
        let holds = ready::Vitals::now().map_or(0, |vitals| vitals.asked);
        let needs = ((holds as f64 * share * 1.125) as u64).max(LEAST_FREE);
        let gigabytes = |bytes: u64| bytes as f64 / (1u64 << 30) as f64;
        if room.free >= needs && room.can_take >= needs {
            return Ok(());
        }
        Err(format!(
            "the {what} was not started: it runs in a process of its own, which would take about {:.1} GB for the \
             song's plugins, and the machine has {:.1} GB of memory free of {:.0} GB (it can still commit {:.1} \
             GB). Close something, or take plugins out of the song, and try again",
            gigabytes(needs),
            gigabytes(room.free),
            gigabytes(room.total),
            gigabytes(room.can_take)
        ))
    }

    /// Hands `order` to a process (the one that is kept, if `kept`, and a
    /// new one otherwise) and waits for what comes of it. With the answer
    /// comes the process's id, for clearing away what it left.
    fn ask(&self, order: &Order, watch: Option<&Watch>, kept: bool) -> Result<(Asked, u32), String> {
        self.inner.jobs.fetch_add(1, Ordering::Relaxed);
        let start = || {
            self.inner.started.fetch_add(1, Ordering::Relaxed);
            Worker::start(&self.inner.setup.program, &self.inner.setup.arguments)
        };
        if !kept {
            let mut worker = start()?;
            let asked = worker.ask(order, watch);
            let pid = worker.pid;
            if matches!(asked, Asked::Said(_)) {
                worker.end();
            }
            return Ok((asked, pid));
        }
        let mut place = self.inner.kept.lock();
        // One that went since it last answered is not asked again.
        let still_there = place.as_mut().is_some_and(|worker| matches!(worker.process.try_wait(), Ok(None)));
        let mut worker = match place.take().filter(|_| still_there) {
            Some(worker) => worker,
            None => start()?,
        };
        let mut asked = worker.ask(order, watch);
        // One that goes without having taken the job up went over the job
        // before, as it let go of that one's devices: the job is another's.
        if matches!(&asked, Asked::Gone(gone) if !gone.began) {
            worker = start()?;
            asked = worker.ask(order, watch);
        }
        let pid = worker.pid;
        // One that answered is kept for the next; one that did not is gone.
        if matches!(asked, Asked::Said(_)) {
            worker.used = Instant::now();
            *place = Some(worker);
            drop(place);
            self.mind_the_kept();
        }
        Ok((asked, pid))
    }

    /// Sees to it that the process that is kept is ended once nothing has
    /// been asked of it for a while: it holds plugins in memory.
    fn mind_the_kept(&self) {
        if self.inner.minded.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner: Weak<Inner> = Arc::downgrade(&self.inner);
        let minder = move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(inner) = inner.upgrade() else { return };
                let mut kept = inner.kept.lock();
                match kept.as_ref() {
                    Some(worker) if worker.used.elapsed() < KEPT_FOR => {}
                    _ => {
                        *kept = None;
                        inner.minded.store(false, Ordering::SeqCst);
                        return;
                    }
                }
            }
        };
        if std::thread::Builder::new().name("trill-jobs".into()).spawn(minder).is_err() {
            self.inner.minded.store(false, Ordering::SeqCst);
        }
    }

    /// Ends the process that is kept, if there is one: the next job that
    /// would have gone to it starts another.
    pub fn end_kept(&self) {
        *self.inner.kept.lock() = None;
    }
}

/// Removes what the process `pid` left unfinished in `folder`: files are
/// written under a name that ends in the id of the process that writes
/// them until they are whole.
fn sweep(folder: &Path, pid: u32) {
    let ending = format!(".{pid}.part");
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().ends_with(&ending) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// What came of asking a process.
enum Asked {
    /// Its answer.
    Said(Said),
    /// Whoever watched stopped it.
    Stopped,
    /// It went without answering.
    Gone(Gone),
}

/// What is known of a process that went without answering.
struct Gone {
    /// Whether it had taken the job up: said anything of it. A process
    /// that is kept may go over what it was letting go of from the job
    /// before, and then the job it was handed next is not what took it
    /// down.
    began: bool,
    stage: Option<Stage>,
    fault: Option<(u32, String)>,
    /// How it ended, as the system tells: an exception's code, or the
    /// number it left with.
    code: Option<u32>,
    last_words: String,
}

impl Gone {
    /// The sentence's parts: which of `devices` the fault was in, by the
    /// module it was in or by what was being made.
    fn crash(
        self,
        job: &'static str,
        devices: &[Device],
        factory: &dyn DeviceFactory,
        project: Option<&Project>,
    ) -> Crash {
        let module = self.fault.as_ref().map(|(_, module)| PathBuf::from(module)).filter(|m| !m.as_os_str().is_empty());
        let named = |device: &Device| match project
            .and_then(|project| project.tracks.iter().find(|track| track.devices.iter().any(|on| on.id == device.id)))
        {
            Some(track) => format!("{} on '{}'", device.name, track.name),
            None if project.is_some_and(|p| p.master.devices.iter().any(|on| on.id == device.id)) => {
                format!("{} on the master", device.name)
            }
            None => device.name.clone(),
        };
        let mut blamed: Vec<String> = Vec::new();
        if let Some(module) = &module {
            for device in devices {
                let of_it = factory.module(&device.uri).is_some_and(|plugin| same_plugin(&plugin, module));
                if of_it && !blamed.contains(&named(device)) {
                    blamed.push(named(device));
                }
            }
        }
        // Where the module tells nothing, the device that was being made does.
        if let (true, Some(Stage::Making(id))) = (blamed.is_empty(), self.stage) {
            blamed.extend(devices.iter().find(|device| device.id == id).map(named));
        }
        Crash {
            job,
            stage: self.stage,
            code: self.fault.as_ref().map(|(code, _)| *code).or(self.code),
            module,
            devices: blamed,
            last_words: self.last_words,
        }
    }
}

/// Whether the module a fault was in is the plugin's file, or lies in the
/// plugin's folder (a plugin that is a folder keeps its code inside).
fn same_plugin(plugin: &Path, module: &Path) -> bool {
    let lower = |path: &Path| path.to_string_lossy().replace('/', "\\").to_lowercase();
    let (plugin, module) = (lower(plugin), lower(module));
    // The folder of a plugin that is one: the path up to the first name
    // in it that ends in its format's extension.
    let folder = |path: &str| {
        let mut end = 0;
        for name in path.split('\\') {
            end += name.len();
            if name.ends_with(".vst3") || name.ends_with(".clap") {
                return Some(path[..end].to_string());
            }
            end += 1;
        }
        None
    };
    plugin == module || matches!((folder(&plugin), folder(&module)), (Some(one), Some(other)) if one == other)
}

/// A process that does jobs, as whoever asks it holds it.
struct Worker {
    process: Child,
    /// Kept open for as long as the process is wanted: it ends itself when
    /// this closes.
    stdin: Option<ChildStdin>,
    lines: Receiver<Said>,
    /// The last of what it wrote to its standard error.
    words: Arc<Mutex<String>>,
    pid: u32,
    /// When it was last asked anything.
    used: Instant,
}

impl Worker {
    /// Starts `program` as a process that does jobs and waits for it to
    /// say that it is one.
    fn start(program: &Path, arguments: &[String]) -> Result<Self, String> {
        let mut command = Command::new(program);
        command.args(arguments).env(JOB_ENV, "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_console(&mut command);
        let cannot = |reason: String| {
            format!("the process that renders ('{}') could not be started: {reason}", program.display())
        };
        let mut process = command.spawn().map_err(|error| cannot(error.to_string()))?;
        let pid = process.id();
        let (stdout, stderr) = (process.stdout.take(), process.stderr.take());
        let (said, lines) = mpsc::channel();
        let words = Arc::new(Mutex::new(String::new()));
        let threads = stdout.zip(stderr).and_then(|(stdout, stderr)| {
            let listen = std::thread::Builder::new().name("trill-job-said".into()).spawn(move || listen(stdout, &said));
            let kept = words.clone();
            let log = std::thread::Builder::new().name("trill-job-log".into()).spawn(move || overhear(stderr, &kept));
            listen.ok().zip(log.ok())
        });
        let worker = Self { stdin: process.stdin.take(), process, lines, words, pid, used: Instant::now() };
        if threads.is_none() {
            return Err(cannot("its output could not be read".into()));
        }
        match worker.lines.recv_timeout(HELLO_WITHIN) {
            Ok(Said::Hello(VERSION)) => Ok(worker),
            Ok(Said::Hello(other)) => Err(cannot(format!("it speaks version {other}, not {VERSION}"))),
            Ok(_) | Err(_) => {
                let words = worker.words.lock().clone();
                Err(cannot(format!("it did not say that it does jobs. {words}")))
            }
        }
    }

    /// Hands the process `order` and waits for what comes of it.
    fn ask(&mut self, order: &Order, watch: Option<&Watch>) -> Asked {
        self.used = Instant::now();
        let written = serde_json::to_string(order).map_err(|error| error.to_string()).and_then(|mut line| {
            line.push('\n');
            let stdin = self.stdin.as_mut().ok_or("its input is closed")?;
            stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush()).map_err(|error| error.to_string())
        });
        if written.is_err() {
            return Asked::Gone(self.gone(false, None, None));
        }
        let (mut stage, mut fault, mut answer) = (None, None, None);
        let mut began = false;
        loop {
            if watch.is_some_and(Watch::stopped) {
                let _ = self.process.kill();
                let _ = self.process.wait();
                return Asked::Stopped;
            }
            match self.lines.recv_timeout(LOOK_EVERY) {
                Ok(Said::Stage(now)) => {
                    began |= now != Stage::LettingGo;
                    stage = Some(now);
                }
                Ok(Said::Progress(share)) => {
                    if let Some(watch) = watch {
                        watch.came_to(share);
                    }
                }
                Ok(Said::Fault { code, module }) => fault = Some((code, module)),
                Ok(Said::Memory(memory)) => ready::remember(memory),
                Ok(Said::Hello(_)) => {}
                Ok(Said::Done) => match answer.take() {
                    Some(said) => return Asked::Said(said),
                    None => return Asked::Gone(self.gone(began, stage, fault)),
                },
                Ok(said) => answer = Some(said),
                Err(RecvTimeoutError::Timeout) => {}
                // Its output closed: it is gone, or going.
                Err(RecvTimeoutError::Disconnected) => return Asked::Gone(self.gone(began, stage, fault)),
            }
        }
    }

    /// What is known of the process now that it went without answering.
    fn gone(&mut self, began: bool, stage: Option<Stage>, fault: Option<(u32, String)>) -> Gone {
        // It is given a moment to be all gone, and is ended if it is not.
        let begun = Instant::now();
        let status = loop {
            match self.process.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if begun.elapsed() < END_WITHIN => std::thread::sleep(LOOK_EVERY),
                _ => {
                    let _ = self.process.kill();
                    break self.process.wait().ok();
                }
            }
        };
        let code = status.and_then(|status| status.code()).map(|code| code as u32);
        // What it said last is on its way through another thread.
        std::thread::sleep(LOOK_EVERY);
        Gone { began, stage, fault, code, last_words: self.words.lock().trim().to_string() }
    }

    /// Lets a process that has answered its last job end by itself.
    fn end(mut self) {
        let begun = Instant::now();
        while begun.elapsed() < END_WITHIN {
            if !matches!(self.process.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(LOOK_EVERY);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Its input closes, which a process that is still there takes as
        // the word to go; and it is not left to take its time over it.
        self.stdin = None;
        if matches!(self.process.try_wait(), Ok(None)) {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
    }
}

/// Reads what a process says, line by line, and hands on what is its own.
fn listen(stdout: impl Read, said: &mpsc::Sender<Said>) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        // Plugins write here too: only what follows the mark is the
        // process's own.
        let Some(at) = text.find(MARK) else { continue };
        match serde_json::from_str::<Said>(text[at + MARK.len()..].trim()) {
            Ok(message) => {
                if said.send(message).is_err() {
                    return;
                }
            }
            Err(error) => log::warn!("a process that renders said something that cannot be read: {error}"),
        }
    }
}

/// Reads what a process writes to its standard error: its log, which is
/// passed on, and whatever else, of which the last is kept.
fn overhear(stderr: impl Read, kept: &Mutex<String>) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { return };
        match line.strip_prefix(serve::LOGGED) {
            Some(logged) => log::info!("{logged}"),
            None => {
                let mut kept = kept.lock();
                kept.push_str(&line);
                kept.push('\n');
                let surplus = kept.len().saturating_sub(TAIL);
                if surplus > 0 {
                    let cut = (surplus..kept.len()).find(|at| kept.is_char_boundary(*at)).unwrap_or(kept.len());
                    kept.drain(..cut);
                }
            }
        }
    }
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

/// Says which stage a job is at, in a process that does jobs for
/// another; nothing anywhere else.
pub(crate) fn stage(stage: Stage) {
    if serve::serving() {
        serve::say(&Said::Stage(stage));
    }
}

/// Says how far a render has come, in a process that does jobs for
/// another; nothing anywhere else.
pub(crate) fn progress(share: f32) {
    if serve::serving() {
        serve::say(&Said::Progress(share));
    }
}

/// Whether this is a process that does one job and ends: it leaves its
/// devices as they are.
pub(crate) fn leaves_devices() -> bool {
    serve::leaving()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fault_is_laid_to_the_plugin_whose_file_or_folder_it_was_in() {
        let plugin = Path::new("C:/Program Files/Common Files/VST3/Amp X.vst3");
        assert!(same_plugin(plugin, Path::new("c:\\program files\\common files\\vst3\\amp x.vst3")));
        // A plugin that is a folder, known by the code inside it or by the folder.
        let inside = Path::new("C:/Program Files/Common Files/VST3/Amp X.vst3/Contents/x86_64-win/Amp X.vst3");
        assert!(same_plugin(plugin, inside) && same_plugin(inside, plugin));
        let helper = Path::new("C:/Program Files/Common Files/VST3/Amp X.vst3/Contents/x86_64-win/helper.dll");
        assert!(same_plugin(inside, helper));
        // Not another plugin, and not the system's.
        assert!(!same_plugin(plugin, Path::new("C:/Program Files/Common Files/VST3/Amp Y.vst3")));
        assert!(!same_plugin(inside, Path::new("C:/Windows/System32/ntdll.dll")));
        assert!(!same_plugin(plugin, Path::new("C:/Program Files/Common Files/VST3/Amp X.vst3.bak")));
    }

    #[test]
    fn a_crash_is_a_sentence_that_says_what_which_and_what_next() {
        let crash = Crash {
            job: "render",
            stage: Some(Stage::Playing),
            code: Some(0xC000_0005),
            module: Some(PathBuf::from("C:/VST3/Amp X.vst3")),
            devices: vec!["Amp X on 'Lead'".into(), "Amp X on 'Rhythm'".into()],
            last_words: String::new(),
        };
        let said = crash.to_string();
        assert!(said.starts_with(
            "a plugin took the render down while it was being played: Amp X on 'Lead' and Amp X on 'Rhythm' (an \
             access violation in Amp X.vst3). It ran in a process of its own, so the song that is open is untouched"
        ));
        assert!(said.ends_with("mute that track or take the device out to go on without it"), "{said}");

        // Where nothing can be named, it says what is known and how to find out.
        let unknown =
            Crash { stage: None, code: Some(3), module: None, devices: Vec::new(), last_words: "bang".into(), ..crash };
        let said = unknown.to_string();
        assert!(said.starts_with("the render was taken down, with code 0x3. It ran in a process of its own"), "{said}");
        assert!(said.ends_with("to find which device it is. The last it said: bang"), "{said}");

        // A probe wrote nothing, and is of one instrument.
        let probe = Crash { job: "probe", devices: vec!["Bass".into()], stage: Some(Stage::Waiting), ..unknown };
        assert_eq!(
            probe.to_string(),
            "a plugin took the probe down while its plugins were waited for: Bass (code 0x3). It ran in a process \
             of its own, so the song that is open is untouched. Try again; if it happens again, probe the \
             instrument without the devices after it, or as it comes"
        );
    }

    #[test]
    fn what_a_process_says_is_read_back_as_it_was_said() {
        for said in [Said::Hello(VERSION), Said::Stage(Stage::Making(DeviceId(7))), Said::Progress(0.25), Said::Done] {
            let line = serde_json::to_string(&said).unwrap();
            let back: Said = serde_json::from_str(&line).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), line);
        }
    }
}
