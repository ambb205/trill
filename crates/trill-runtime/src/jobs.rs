//! Renders, measurements and probes of songs with plugins, done in a
//! process of their own.
//!
//! The engine knows how a job crosses to another process and back
//! ([`trill_engine::apart`]). What it does not know is which program that
//! process is, and how the process comes by the devices of the song. A
//! runtime knows both: the program is the one that is running, started a
//! second time (as its plugin scanner is), and the devices are made the
//! way the runtime makes its own, from what the job is told of them here
//! ([`Devices`]): the plugins it hosts, as this runtime found them, so
//! that the process scans nothing and reads no cache; the rate the
//! built-in sample players are handed their files at; and nothing else
//! of the user's.
//!
//! A program that is to be such a process calls
//! [`run_as_job_if_requested`] first thing in `main`. `trill` and
//! `trill-mcp` do, and so does `trill-job`, a program that is nothing
//! else, for whoever has a runtime in a program that cannot be started
//! twice (a test).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use trill_core::Device;
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_dsp::Builtins;
use trill_engine::apart::{Apart, JOB_ENV, Recipe, Setup};
use trill_engine::scripted::{self, Loading, ScriptedFactory};
use trill_media::MediaCache;
use trill_plugins::{Known, PluginHost};

/// Which program does a runtime's jobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    /// The running program, started a second time. It must call
    /// [`run_as_job_if_requested`] first thing in `main`.
    CurrentExecutable,
    /// This program, which calls [`run_as_job_if_requested`] first thing:
    /// `trill-job`, or any other that does.
    Executable(PathBuf),
}

/// How a runtime's jobs are done apart: see [`crate::Runtime::do_jobs_apart`].
pub struct Jobs {
    pub program: Program,
    /// Plugins to tell the process of beside the runtime's own: where the
    /// runtime was handed a plugin host among its extra devices.
    pub plugins: Option<PluginHost>,
    /// The scripted instruments the runtime was handed among its extra
    /// devices ([`trill_engine::scripted`]), which the process then
    /// offers too. For tests.
    pub scripted: Option<Loading>,
    /// Whether probes share a process that is kept between them: see
    /// [`Setup::keep_for_probes`].
    pub keep_for_probes: bool,
    /// See [`Setup::keep_for_renders`]: for measuring, and for nothing
    /// else.
    pub keep_for_renders: bool,
    /// Whether the process keeps a plugin that is known to fault when it
    /// is taken down young until it is old enough
    /// ([`trill_plugins::Vst3Config::patient_teardown`]). On, but for
    /// finding out what such a plugin does to the process without it.
    pub patient_teardown: bool,
    /// What to take the machine's memory to be, in place of asking it:
    /// see [`Setup::room`]. For tests.
    #[doc(hidden)]
    pub room: Option<fn() -> Option<trill_engine::apart::Room>>,
}

/// The variable that says where a program does its jobs, for finding out
/// what a process of its own costs or does: `here` (in the program's own
/// process, as before there was another), `kept` (one process that is
/// kept from one job to the next, for renders and probes alike),
/// `kept-probes` (renders in a process each, probes in one that is kept).
/// Anything else, and nothing, is the usual: see [`Jobs::usual`].
pub const JOBS_ENV: &str = "TRILL_JOBS";

impl Jobs {
    /// How a program does its jobs unless its environment says otherwise
    /// ([`JOBS_ENV`]): a render, a measurement and a probe each in a
    /// process that has just started. `None` where the environment says
    /// to do them here.
    ///
    /// Probes too, because a process that is kept between them was
    /// measured to save nothing (November 2026, the second probe of an
    /// instrument, kept against just started: a sampled bass 1.7 and
    /// 1.8 s, a drum sampler 3.6 and 3.6 s, a synthesizer 0.5 and 0.7 s:
    /// the instrument is made anew either way, and that is what takes the
    /// time), while it goes on holding what its plugins leave behind (0.4
    /// GB after nine probes), takes its plugins down when it is done with
    /// them, which is where plugins fault, and does not begin an
    /// instrument that draws at random at the same place twice.
    pub fn usual(program: Program) -> Option<Self> {
        let asked = std::env::var(JOBS_ENV).unwrap_or_default();
        match asked.trim() {
            "here" => None,
            "kept" => Some(Self { keep_for_probes: true, keep_for_renders: true, ..Self::by(program) }),
            "kept-probes" => Some(Self { keep_for_probes: true, ..Self::by(program) }),
            _ => Some(Self::by(program)),
        }
    }

    /// Jobs done by `program`, each in a process that has just started.
    pub fn by(program: Program) -> Self {
        Self {
            program,
            plugins: None,
            scripted: None,
            keep_for_probes: false,
            keep_for_renders: false,
            patient_teardown: true,
            room: None,
        }
    }
}

/// What a process is told of the devices of a job: how to make them
/// without looking for anything.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Devices {
    /// The plugins the job hosts, as the runtime that asks found them.
    plugins: Vec<Known>,
    /// The rate the built-in sample players are handed their files at:
    /// the engine's, in the runtime that asks.
    loader_rate: Option<f64>,
    scripted: Option<Loading>,
    patient_teardown: bool,
}

/// What it takes for the engine to do the jobs of a runtime that hosts
/// `plugins` and hands sample files out of `media` apart, as `jobs` says.
/// An error is about the program: the one that is running cannot be
/// found.
pub(crate) fn apart(jobs: Jobs, plugins: Option<PluginHost>, media: Arc<MediaCache>) -> Result<Apart, String> {
    let program = match jobs.program {
        Program::Executable(path) => path,
        Program::CurrentExecutable => std::env::current_exe()
            .map_err(|error| format!("this program's own file cannot be found to start it a second time: {error}"))?,
    };
    let hosts: Vec<PluginHost> = plugins.into_iter().chain(jobs.plugins).collect();
    let (scripted, patient_teardown) = (jobs.scripted, jobs.patient_teardown);
    let room = jobs.room.unwrap_or(trill_engine::apart::Room::now);
    let recipe: Recipe = Box::new(move |devices: &[Device]| {
        let mut known: Vec<Known> = Vec::new();
        for device in devices {
            let ours = device.uri.starts_with("builtin:");
            let scripted =
                scripted.is_some() && [scripted::BASS, scripted::DRUMS, scripted::AMP].contains(&&*device.uri);
            if ours || scripted {
                continue;
            }
            // A device no other process could make: the job is done here.
            let plugin = hosts.iter().find_map(|host| host.known(&device.uri))?;
            if !known.contains(&plugin) {
                known.push(plugin);
            }
        }
        let devices = Devices { plugins: known, loader_rate: media.loader_rate(), scripted, patient_teardown };
        serde_json::to_value(devices).ok()
    });
    let setup = Setup {
        keep_for_probes: jobs.keep_for_probes,
        keep_for_renders: jobs.keep_for_renders,
        room,
        ..Setup::new(program, recipe)
    };
    Ok(Apart::new(setup))
}

/// Makes the devices of a job in the process that does it, from what the
/// runtime that asked said of them.
fn make(told: &serde_json::Value) -> Result<Arc<dyn DeviceFactory>, String> {
    let devices: Devices = serde_json::from_value(told.clone())
        .map_err(|error| format!("what was said of the song's devices could not be read: {error}"))?;
    let media = Arc::new(MediaCache::default());
    media.set_loader_rate(devices.loader_rate);
    let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(Builtins::with_loader(media))];
    if let Some(loading) = devices.scripted {
        factories.push(Box::new(ScriptedFactory::loading(loading)));
    }
    if !devices.plugins.is_empty() {
        let host = PluginHost::knowing(&devices.plugins, devices.patient_teardown)
            .map_err(|error| format!("the plugin host could not be started: {error}"))?;
        factories.extend(host.factories());
    }
    Ok(Arc::new(CompositeFactory::new(factories)))
}

/// Turns this process into one that does jobs for the process that
/// started it (a render, a measurement, a probe, or a study of a
/// recording), if it was started as one, and otherwise returns at once.
///
/// A program that uses [`Program::CurrentExecutable`] calls this before
/// anything else in `main`: the process that renders is the program's own
/// executable, told apart by a variable in its environment.
pub fn run_as_job_if_requested() {
    // A study's run is done by the program started a second time too.
    trill_study::helper::run_if_requested();
    if std::env::var_os(JOB_ENV).is_some() {
        trill_engine::apart::serve(&make)
    }
}
