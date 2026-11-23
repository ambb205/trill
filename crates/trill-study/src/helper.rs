//! The process that studies: a run done by the program started a second
//! time, as a song's renders are (`trill-engine`'s `apart`).
//!
//! A model's runtime is somebody else's code driving a graphics card, and
//! it is fed whole recordings. In a process of its own:
//!
//! - **One that faults costs a sentence.** The process that dies is the
//!   one that was studying. Trill is as it was, the study's folder holds
//!   every stage that had finished, and the error says which stage it was.
//! - **It cannot take the machine with it.** Before it is started the
//!   machine is asked how much memory it has left, and a run it has no
//!   room for is refused, not begun. While it runs it is asked what it
//!   holds, and one that takes more than the run said it needs is ended:
//!   a tensor of the wrong shape asks for forty gigabytes without a word.
//! - **Its memory comes back.** A runtime keeps what it took from a
//!   graphics card for as long as it lives. A process that has ended has
//!   given all of it back.
//!
//! **What crosses.** To the process, one line of JSON on its standard
//! input ([`Order`]): the study's folder, which models to use and where
//! their files are, and the programs that were found for it. Back, lines
//! on its standard output ([`Said`]), each with a mark, because a runtime
//! may write there too: the stage it is at, how far it has come, what
//! runs the models, and how it ended. What it found is in the folder.
//!
//! **Stopping** is ending the process. Everything in a study's folder is
//! written under another name until it is whole, so nothing half-written
//! is left under a name that counts, and the run is taken up again at the
//! stage it was in.

use crate::models::Which;
use crate::room::{self, Held};
use crate::run::{self, Event, Programs};
use crate::store::{Stage, StudyFolder};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Set in the environment of a process that is started to study for
/// another: a program that can be one looks for it first thing.
pub const ENV: &str = "TRILL_STUDY";
/// What every line of the process's own begins with.
const MARK: &str = "@trill-study@";
/// What the process says first; a program that says anything else is not
/// one that studies.
const VERSION: u32 = 1;
/// How long a process may take to say that it is there.
const HELLO_WITHIN: Duration = Duration::from_secs(60);
/// How often whoever attends a process looks whether it is to be stopped.
const LOOK_EVERY: Duration = Duration::from_millis(50);
/// How much of what the process said last, on its standard error, is kept.
const LAST_WORDS: usize = 600;

/// What a process that studies is asked to do: one thing, and then it ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    /// Run every stage of the study in `folder` that is not done.
    Study { folder: PathBuf, models: Which, programs: Programs },
    /// Time the models on this machine: split so many seconds of noise
    /// into stems, and say how long a minute of music takes.
    Check { models: Which, seconds: f64 },
    /// Say which graphics cards the part for any card finds here.
    Cards { setup: crate::models::real::Setup },
}

/// What a timed test found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checked {
    /// What ran the models.
    pub device: String,
    /// How long the models took to be ready.
    pub ready_seconds: f64,
    /// How long a minute of music takes to split.
    pub seconds_a_minute: f64,
    /// A number that is the same wherever the models gave the same stems.
    pub sum: f64,
}

/// What the process says, a line each.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Said {
    Hello(u32),
    Stage(Stage),
    Progress(f32),
    Device(String),
    Checked(Checked),
    /// The graphics cards there are, by who made each, in the system's order.
    Cards(Vec<String>),
    /// It did what it was asked.
    Done,
    /// It could not, and why.
    Failed(String),
}

/// A process that studies, as whoever started it has it.
pub struct Job {
    process: Child,
    /// Its input, held open for as long as it is wanted: a process whose
    /// asker has gone finds out by its closing, and ends.
    _input: Option<std::process::ChildStdin>,
    lines: Receiver<Said>,
    /// What it wrote last to its standard error.
    words: Arc<Mutex<String>>,
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

/// What a program that studies is started with besides the mark in its
/// environment: nothing, for a program that calls [`run_if_requested`]
/// first thing.
static STARTED_WITH: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Has a test program study: it cannot call [`run_if_requested`] first
/// thing, so it is started again to run the one test `test`, which does.
/// Every process that studies is started so from then on. For a crate
/// that builds no program of its own that could study (the window's
/// tests), as `trill_engine::apart` has it for renders.
pub fn serve_by_test(test: &str) {
    *STARTED_WITH.lock() = ["--exact", test, "--nocapture", "--test-threads", "1"].map(str::to_owned).to_vec();
}

impl Job {
    /// Starts `program` as a process that studies, hands it `order`, and
    /// waits for it to say that it is one.
    pub fn start(program: &Path, order: &Order) -> Result<Self, String> {
        let cannot = |reason: String| {
            format!("the process that studies ('{}') could not be started: {reason}", program.display())
        };
        let mut command = Command::new(program);
        command.args(STARTED_WITH.lock().iter());
        command.env(ENV, "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_console(&mut command);
        let mut process = command.spawn().map_err(|error| cannot(error.to_string()))?;
        let (said, lines) = mpsc::channel();
        let words = Arc::new(Mutex::new(String::new()));
        let (Some(stdout), Some(stderr), Some(mut stdin)) =
            (process.stdout.take(), process.stderr.take(), process.stdin.take())
        else {
            let _ = process.kill();
            return Err(cannot("its output could not be read".to_owned()));
        };
        let kept = words.clone();
        let threads = [
            std::thread::Builder::new().name("trill-study-said".into()).spawn(move || listen(stdout, &said)),
            std::thread::Builder::new().name("trill-study-log".into()).spawn(move || overhear(stderr, &kept)),
        ];
        let mut job = Self { process, _input: None, lines, words };
        if threads.iter().any(Result::is_err) {
            job.end();
            return Err(cannot("its output could not be read".to_owned()));
        }
        // Its one order. Its input stays open after it, and says nothing more.
        let line = serde_json::to_string(order).map_err(|error| cannot(error.to_string()))?;
        if stdin.write_all(line.as_bytes()).and_then(|()| stdin.write_all(b"\n")).and_then(|()| stdin.flush()).is_err()
        {
            job.end();
            return Err(cannot("it took no order".to_owned()));
        }
        job._input = Some(stdin);
        match job.lines.recv_timeout(HELLO_WITHIN) {
            Ok(Said::Hello(VERSION)) => Ok(job),
            Ok(Said::Hello(other)) => {
                job.end();
                Err(cannot(format!("it speaks version {other}, not {VERSION}")))
            }
            _ => {
                job.end();
                let words = job.words.lock().clone();
                Err(cannot(format!("it did not say that it studies. {words}").trim().to_owned()))
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.process.id()
    }

    /// Ends the process now, whatever it is doing.
    pub fn end(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }

    /// What the process holds, and the most it has held.
    pub fn held(&self) -> Option<Held> {
        room::held_by(self.pid())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.end();
    }
}

/// Reads the process's own lines from its standard output and passes
/// everything else over.
fn listen(stdout: impl Read, said: &mpsc::Sender<Said>) {
    for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        if let Some(json) = line.trim_end().strip_prefix(MARK)
            && let Ok(heard) = serde_json::from_str::<Said>(json)
            && said.send(heard).is_err()
        {
            return;
        }
    }
}

/// Keeps the last of what the process writes to its standard error.
fn overhear(stderr: impl Read, kept: &Mutex<String>) {
    for line in BufReader::new(stderr).split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        log::debug!("study: {line}");
        let mut kept = kept.lock();
        kept.push_str(line);
        kept.push(' ');
        if kept.len() > LAST_WORDS {
            let from = kept.char_indices().map(|(at, _)| at).find(|&at| at >= kept.len() - LAST_WORDS).unwrap_or(0);
            kept.drain(..from);
        }
    }
}

/// How a process that studies ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Done,
    /// The timed test's answer.
    Checked(Checked),
    /// Whoever attended it stopped it.
    Stopped,
    /// A sentence: what happened, at which stage, and what to do.
    Failed(String),
}

/// Attends a process until it ends: passes on what it says, ends it when
/// `stop` is set, and ends it when it holds more than `limit` bytes.
/// Answers with how it ended and the most memory it held.
pub fn attend(job: &mut Job, limit: Option<u64>, stop: &AtomicBool, on: &mut dyn FnMut(&Said)) -> (Outcome, Held) {
    let (mut stage, mut checked, mut held) = (None, None, Held::default());
    let mut looked = Instant::now();
    let during = |stage: Option<Stage>| stage.map_or(String::new(), |stage| format!(" while {}", stage.while_doing()));
    let outcome = loop {
        if stop.load(Ordering::Relaxed) {
            job.end();
            break Outcome::Stopped;
        }
        if looked.elapsed() >= room::LOOK_EVERY {
            looked = Instant::now();
            if let Some(now) = job.held() {
                held = Held { now: now.now, peak: held.peak.max(now.peak) };
                if limit.is_some_and(|limit| now.now > limit) {
                    job.end();
                    break Outcome::Failed(format!(
                        "the process that studies took more memory than this run is given ({} of {}){}, and was stopped before \
                         the machine ran short. Nothing of the study is lost: every stage that had finished is kept. If it \
                         happens again with this recording, study a shorter part of it",
                        room::said(now.now),
                        room::said(limit.unwrap_or(0)),
                        during(stage),
                    ));
                }
            }
        }
        match job.lines.recv_timeout(LOOK_EVERY) {
            Ok(said) => {
                on(&said);
                match said {
                    Said::Stage(now) => stage = Some(now),
                    Said::Checked(found) => checked = Some(found),
                    Said::Done => break checked.take().map_or(Outcome::Done, Outcome::Checked),
                    Said::Failed(why) => break Outcome::Failed(why),
                    Said::Hello(_) | Said::Progress(_) | Said::Device(_) | Said::Cards(_) => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Its output closed without a word on how it ended: it went down.
            Err(RecvTimeoutError::Disconnected) => {
                let status = job.process.wait().ok();
                let code = status.and_then(|status| status.code());
                // What it said last is on its way through another thread.
                std::thread::sleep(LOOK_EVERY);
                let words = job.words.lock().trim().to_owned();
                let code = code.map_or(String::new(), |code| format!(" (it ended with code {:#x})", code as u32));
                // A process that faults on a Unix is ended by a signal, and has no code.
                #[cfg(unix)]
                let code = match status.and_then(|status| std::os::unix::process::ExitStatusExt::signal(&status)) {
                    Some(signal) => format!(" (it was ended by signal {signal})"),
                    None => code,
                };
                let mut sentence = format!(
                    "the process that studies went down{}{code}. Trill is as it was, and every stage of the study that had \
                     finished is kept: run it again to take it up there",
                    during(stage)
                );
                if !words.is_empty() {
                    sentence.push_str(&format!(". It said last: {words}"));
                }
                break Outcome::Failed(sentence);
            }
        }
    };
    if let Some(last) = job.held() {
        held.peak = held.peak.max(last.peak);
    }
    job.end();
    (outcome, held)
}

/// Writes one line of the process's own to its standard output, whole:
/// begun on a line of its own, whatever a runtime left unfinished there.
fn say(said: &Said) {
    let Ok(json) = serde_json::to_string(said) else { return };
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(format!("\n{MARK}{json}\n").as_bytes());
    let _ = out.flush();
}

/// Noise that is the same on every run, for the timed test.
fn noise(seconds: f64) -> trill_listen::Sound {
    let mut seed = 0x2545_F491u32;
    let mut channel = || -> Vec<f32> {
        (0..(seconds * trill_listen::RATE) as usize)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed >> 8) as f32 / 8_388_608.0 * 0.2 - 0.1
            })
            .collect()
    };
    trill_listen::Sound { left: channel(), right: channel() }
}

fn check(models: &Which, seconds: f64) -> Result<Checked, String> {
    let begun = Instant::now();
    let mut models = models.make()?;
    // Once to have everything made ready, which is not what a song costs.
    models.stems(&noise(8.0), crate::store::StemChoice::Fast, &mut |_| {})?;
    let ready_seconds = begun.elapsed().as_secs_f64();
    let seconds = seconds.clamp(8.0, 120.0);
    let begun = Instant::now();
    let stems =
        models.stems(&noise(seconds), crate::store::StemChoice::Fast, &mut |share| say(&Said::Progress(share)))?;
    let taken = begun.elapsed().as_secs_f64();
    let sum = stems.iter().flat_map(|stem| &stem.sound.left).map(|&sample| f64::from(sample.abs())).sum::<f64>();
    Ok(Checked { device: models.device(), ready_seconds, seconds_a_minute: taken * 60.0 / seconds, sum })
}

fn serve(order: Order) -> Result<(), String> {
    match order {
        Order::Study { folder, models, programs } => {
            // Made when a stage first asks for one: a study with only
            // listening left to do loads no runtime.
            let mut models = crate::models::Lazy::new(models);
            run::run(&StudyFolder::new(folder), &mut models, &programs, &mut |event| match event {
                Event::Stage(stage) => say(&Said::Stage(stage)),
                Event::Progress(share) => say(&Said::Progress(share)),
                Event::Device(device) => say(&Said::Device(device)),
            })
        }
        Order::Check { models, seconds } => {
            let checked = check(&models, seconds)?;
            say(&Said::Device(checked.device.clone()));
            say(&Said::Checked(checked));
            Ok(())
        }
        Order::Cards { setup } => {
            say(&Said::Cards(crate::models::real::OnnxModels::new(setup)?.cards()));
            Ok(())
        }
    }
}

/// Turns this process into one that studies for the process that started
/// it, if it was started as one, and otherwise returns at once.
///
/// A program that may be started a second time for this calls it before
/// anything else in `main`.
pub fn run_if_requested() {
    if std::env::var_os(ENV).is_none() {
        return;
    }
    say(&Said::Hello(VERSION));
    let mut line = String::new();
    let order = match std::io::stdin().lock().read_line(&mut line) {
        Ok(read) if read > 0 => {
            serde_json::from_str::<Order>(&line).map_err(|error| format!("its order could not be read: {error}"))
        }
        _ => Err("it was given no order".to_owned()),
    };
    // Whoever started this holds its input open and says nothing more on
    // it. When the input closes, the asker has gone (stopped, or taken
    // down), and what is done here would be done for nobody: on a
    // graphics card, with nothing watching what it takes.
    let _ = std::thread::Builder::new().name("trill-study-asker".into()).spawn(|| {
        let mut rest = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut rest);
        std::process::exit(2)
    });
    let ended = order.and_then(|order| {
        // An error of Trill's own is a sentence too, and not a process gone without a word.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(order))).unwrap_or_else(|panic| {
            let what = panic
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned());
            Err(format!(
                "the study stopped on an error of Trill's own: {}",
                what.unwrap_or_else(|| "it did not say what".to_owned())
            ))
        })
    });
    match ended {
        Ok(()) => say(&Said::Done),
        Err(why) => say(&Said::Failed(why)),
    }
    // It ends where it stands: a runtime that is taken down is where a
    // runtime is most likely to fault, and a process that is going has no
    // reason to.
    std::process::exit(0)
}
