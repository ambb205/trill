//! The process that does jobs for another: what it does from the moment
//! it finds itself to be one.

use super::{Failure, Job, Keep, MARK, Order, Said, Stage, Told, VERSION, fault};
use crate::probe::ProbeJob;
use crate::ready;
use crate::render::{RenderError, Rendered, render_for_another, written};
use std::io::{BufRead, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use trill_core::device::DeviceFactory;
use trill_media::WavFormat;

/// What every line of the process's log begins with on its standard
/// error: whoever started it passes those on to its own log.
pub(super) const LOGGED: &str = "@trill-job-log@";

/// Makes the devices of a job from what the order says of them, in the
/// process that does it; the error is the job's.
pub type Make = dyn Fn(&serde_json::Value) -> Result<Arc<dyn DeviceFactory>, String>;

/// Whether this process does jobs for another.
static SERVING: AtomicBool = AtomicBool::new(false);
/// Whether it does one job and ends.
static LEAVING: AtomicBool = AtomicBool::new(false);

pub(super) fn serving() -> bool {
    SERVING.load(Ordering::Relaxed)
}

pub(super) fn leaving() -> bool {
    LEAVING.load(Ordering::Relaxed)
}

/// Writes one line of the process's own to its standard output, whole:
/// begun on a line of its own, whatever a plugin left unfinished there.
pub(super) fn say(said: &Said) {
    let Ok(json) = serde_json::to_string(said) else { return };
    let line = format!("\n{MARK}{json}\n");
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(line.as_bytes());
    let _ = out.flush();
}

/// The process's log goes to its standard error, a marked line each.
struct Log;

impl log::Log for Log {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let text = record.args().to_string().replace('\n', " ");
            let _ = writeln!(std::io::stderr().lock(), "{LOGGED}{text}");
        }
    }

    fn flush(&self) {}
}

/// Ends the process where it stands, with nothing taken down: its plugins
/// are left as they are, and so is everything else.
fn leave(code: u32) -> ! {
    let _ = std::io::stdout().lock().flush();
    fault::end_now(code)
}

/// Does jobs for the process that started this one, until that one is
/// gone or has no more: reads [`Order`]s from the standard input, a line
/// each, and answers on the standard output. Never returns.
///
/// `make` makes the devices of a job from what the order says of them
/// (what [`super::Setup::recipe`] said in the process that asks); its
/// error is the job's.
///
/// A program that can be such a process calls this first thing when it
/// finds [`super::JOB_ENV`] in its environment, before it does anything
/// that a process which only renders has no use for.
pub fn serve(make: &Make) -> ! {
    SERVING.store(true, Ordering::Relaxed);
    fault::watch();
    if log::set_logger(&Log).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    say(&Said::Hello(VERSION));
    let mut line = String::new();
    loop {
        line.clear();
        match std::io::stdin().lock().read_line(&mut line) {
            // Whoever asked is gone, or has no more.
            Ok(0) | Err(_) => leave(0),
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let order: Order = match serde_json::from_str(line.trim()) {
            Ok(order) => order,
            Err(error) => {
                say(&Said::Failed(Failure::Other(format!("what was asked could not be read: {error}"))));
                say(&Said::Done);
                continue;
            }
        };
        let last = order.last;
        if last {
            LEAVING.store(true, Ordering::Relaxed);
            // Nothing more is read. If the input closes all the same,
            // whoever asked is gone or wants this stopped: there is
            // nobody to finish for.
            let watching = std::thread::Builder::new().name("trill-job-asker".into()).spawn(|| {
                let mut rest = Vec::new();
                let _ = std::io::stdin().lock().read_to_end(&mut rest);
                leave(1)
            });
            drop(watching);
        }
        ready::remember(order.memory);
        // What a job leaves that is taken down only once it has answered.
        let left: Box<dyn std::any::Any> = match make(&order.devices) {
            Err(reason) => {
                say(&Said::Failed(Failure::Other(reason)));
                Box::new(())
            }
            Ok(factory) => run(order.job, factory),
        };
        say(&Said::Memory(ready::remembered()));
        say(&Said::Done);
        if last {
            // The devices stay as they are: see the module documentation.
            std::mem::forget(left);
            leave(0)
        }
        super::stage(Stage::LettingGo);
        drop(left);
    }
}

/// Does one job and says what came of it. What it hands back is what the
/// job made and is still holding, to be let go of after the answer.
fn run(job: Job, factory: Arc<dyn DeviceFactory>) -> Box<dyn std::any::Any> {
    match job {
        Job::Render { project, options, keep } => {
            let kept = !matches!(keep, Keep::Nothing);
            let rendered =
                render_for_another(&project, factory.as_ref(), &options, kept).and_then(|rendered| match &keep {
                    Keep::Nothing => Ok(rendered),
                    Keep::Samples(path) => written(rendered, path, WavFormat::Float32),
                    Keep::File(path) => written(rendered, path, WavFormat::Int24),
                });
            say(&match rendered {
                Ok(Rendered { sample_rate, track_peaks, track_levels, problems, measured, .. }) => {
                    Said::Rendered(Box::new(Told { sample_rate, track_peaks, track_levels, problems, measured }))
                }
                Err(RenderError::EmptyRange { start, end }) => Said::Failed(Failure::EmptyRange { start, end }),
                Err(RenderError::NotAnswered(waited)) => Said::Failed(Failure::NotAnswered(waited)),
                Err(other) => Said::Failed(Failure::Other(other.to_string())),
            });
            Box::new(())
        }
        Job::Probe { devices, preset, tempo, notes, options } => {
            let job = ProbeJob { devices, factory, preset, tempo, apart: None };
            match job.run_here(&notes, &options) {
                Ok((probed, probing)) => {
                    say(&Said::Probed(probed));
                    Box::new(probing)
                }
                Err(error) => {
                    say(&Said::Failed(Failure::Other(error.to_string())));
                    Box::new(())
                }
            }
        }
        Job::Ready { devices, tempo, pitches, patience } => {
            let job = ProbeJob { devices, factory, preset: None, tempo, apart: None };
            match job.wait_here(&pitches, patience) {
                Ok((readiness, probing)) => {
                    say(&Said::Ready(readiness));
                    Box::new(probing)
                }
                Err(error) => {
                    say(&Said::Failed(Failure::Other(error.to_string())));
                    Box::new(())
                }
            }
        }
    }
}
