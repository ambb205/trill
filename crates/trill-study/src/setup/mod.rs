//! The setup of studies: the parts a study needs that Trill does not ship,
//! and the timed test that says what on this machine runs the models.
//!
//! - **The parts are one list among what Trill fetches** ([`list`], handed
//!   to `trill-fetch`'s engine, whose rules they keep): the models, their
//!   runtime and what drives a graphics card, each from where its makers
//!   publish it ([`catalogue`]), fetched when the user asks for that part
//!   and never otherwise.
//! - **What runs the models is measured, not assumed.** The timed test
//!   ([`Setup::test`]) splits the same half minute of noise on the
//!   processor and on every graphics card the fetched parts can drive,
//!   each in a process of its own, and takes the fastest that gave the
//!   same stems as the others. A card that is there and slower than the
//!   processor is not used; one whose driver faults costs a line in the
//!   result. Until the test has run, and again when a part that runs
//!   models changes, studies run on the processor.
//! - **The programs that are not Trill's own** (`ffmpeg`, the one that
//!   fetches a link) are found when they are wanted ([`Setup::programs`]),
//!   and said with how to come by them when they are not there.

pub mod catalogue;

use crate::helper::{self, Checked, Job, Order, Outcome, Said};
use crate::models::Which;
use crate::models::real::{
    self, ANY_CARD, BEATS, NVIDIA, Provider, RUNTIME, STEMS, STEMS_BEST, WORDS, WORDS_PROCESSOR,
};
use crate::models::scripted::Script;
use crate::room;
use crate::run::Programs;
use crate::store::{StemChoice, StudyOptions};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use trill_fetch::{Fetches, List, find_program, write_json};

/// What the list of a study's parts is asked for by.
pub const MODELS: &str = "models";

/// The parts of studies as a list for the engine that fetches, kept in `folder`.
pub fn list(folder: PathBuf) -> List {
    List {
        id: MODELS.to_owned(),
        title: "Studies".to_owned(),
        about: "What takes a recording apart: models that others made and publish, and what runs them. Trill ships none of it and hosts none of it; each part comes from its makers when you ask for it, and is checked against what it was when Trill was made.".to_owned(),
        folder,
        parts: catalogue::parts(),
        changed: None,
    }
}

/// One way of running the models, timed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timed {
    pub provider: Provider,
    /// What ran them, as a person reads it.
    pub device: String,
    /// What it took, where it ran.
    #[serde(default)]
    pub checked: Option<Checked>,
    /// Why it did not, where it did not.
    #[serde(default)]
    pub error: Option<String>,
    /// The most memory its process held, in bytes.
    #[serde(default)]
    pub peak: u64,
}

/// What the timed test found on this machine: `machine.json`, beside the parts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    /// When it was tested, in seconds since 1970.
    pub tested: u64,
    /// What was tested: the checksums of the parts that run the models, so
    /// that a test of other versions is known for one.
    pub of: Vec<String>,
    pub results: Vec<Timed>,
    /// What runs the models: the fastest that gave the same stems as the
    /// others, or what the user chose in its place.
    pub chosen: Provider,
    #[serde(default)]
    pub by_user: bool,
}

/// A program that is not Trill's own, and whether it is here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub name: &'static str,
    /// What Trill uses it for.
    pub what: &'static str,
    pub path: Option<PathBuf>,
    /// How to come by it.
    pub get: &'static str,
}

/// The setup of studies on this machine.
pub struct Setup {
    fetches: Arc<Fetches>,
    /// Scripted models in place of the real ones, which then need no part: for tests.
    stand_in: Option<Script>,
    /// The programs that are taken to be installed, in place of looking: for tests.
    programs: Option<Programs>,
}

/// How many seconds of noise the timed test splits.
const TEST_SECONDS: f64 = 30.0;
/// The memory the timed test wants free before it tries anything, and the
/// most its process may take: it was measured to hold under four gigabytes.
const TEST_NEEDS: u64 = 6 << 30;
const TEST_LIMIT: u64 = 12 << 30;
/// How long one way of running the models may take over the test.
const TEST_WITHIN: Duration = Duration::from_secs(360);
/// How many graphics cards are tried.
const MOST_CARDS: usize = 4;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// What runs the models, as the settings name it.
pub fn provider_name(provider: Provider) -> String {
    match provider {
        Provider::Processor => "the processor".to_owned(),
        Provider::AnyCard { adapter: 0 } => "a graphics card".to_owned(),
        Provider::AnyCard { adapter } => format!("graphics card {}", adapter + 1),
        Provider::Nvidia => "an NVIDIA card through NVIDIA's libraries".to_owned(),
    }
}

/// The fastest of `results` that gave the same stems as the others: the
/// same as the processor's where the processor ran, and otherwise the same
/// as most of them. A card that computes something else is a card whose
/// driver is wrong, however fast it is.
pub fn choose(results: &[Timed]) -> Provider {
    let ran: Vec<(&Timed, &Checked)> =
        results.iter().filter_map(|timed| timed.checked.as_ref().map(|checked| (timed, checked))).collect();
    let same = |a: f64, b: f64| (a - b).abs() <= 1e-3 * a.abs().max(b.abs()).max(1e-9);
    let reference =
        ran.iter().find(|(timed, _)| timed.provider == Provider::Processor).map(|(_, checked)| checked.sum).or_else(
            || {
                ran.iter()
                    .map(|(_, checked)| checked.sum)
                    .max_by_key(|&sum| ran.iter().filter(|(_, other)| same(sum, other.sum)).count())
            },
        );
    let Some(reference) = reference else { return Provider::Processor };
    ran.iter()
        .filter(|(_, checked)| same(checked.sum, reference))
        .min_by(|a, b| a.1.seconds_a_minute.total_cmp(&b.1.seconds_a_minute))
        .map_or(Provider::Processor, |(timed, _)| timed.provider)
}

impl Setup {
    /// The setup over an engine that has the list of a study's parts
    /// ([`list`]) among its lists. `stand_in` and `programs` are for tests:
    /// scripted models, which need no part, and the programs that are
    /// taken to be installed.
    pub fn over(fetches: Arc<Fetches>, stand_in: Option<Script>, programs: Option<Programs>) -> Self {
        Self { fetches, stand_in, programs }
    }

    /// The setup as a program has it when nothing else fetches: the parts
    /// that are published for this system, kept in `folder`, fetched by the
    /// system's `curl`.
    pub fn real(folder: impl Into<PathBuf>) -> Self {
        let fetches = Fetches::new(Arc::new(trill_fetch::Curl));
        fetches.add(list(folder.into())).expect("the parts of studies keep the rules for a list");
        Self::over(Arc::new(fetches), None, None)
    }

    /// The engine that fetches: its parts, how each is, fetching and removing.
    pub fn fetches(&self) -> &Arc<Fetches> {
        &self.fetches
    }

    /// The folder the parts are kept in.
    pub fn folder(&self) -> PathBuf {
        self.fetches.folder_of(MODELS).unwrap_or_default()
    }

    /// Whether all of the part `id` is here, as it was pinned.
    pub fn ready(&self, id: &str) -> bool {
        self.fetches.ready(id)
    }

    fn name_of(&self, id: &str) -> Option<(String, u64)> {
        self.fetches.part(id).ok().map(|part| (part.name.clone(), part.size()))
    }

    // ---- what runs the models -------------------------------------------

    /// The checksums of the parts that run the models, as far as they are here.
    fn tested_of(&self) -> Vec<String> {
        [RUNTIME, ANY_CARD, NVIDIA, STEMS]
            .into_iter()
            .filter(|id| self.ready(id))
            .filter_map(|id| self.fetches.part(id).ok())
            .flat_map(|part| part.files.into_iter().map(|file| file.sha256))
            .collect()
    }

    /// What the timed test last found, if it has run.
    pub fn machine(&self) -> Option<Machine> {
        serde_json::from_str(&std::fs::read_to_string(self.folder().join("machine.json")).ok()?).ok()
    }

    /// Whether the timed test is to be run (again): it never ran, or a
    /// part that runs the models has come, gone or changed since.
    pub fn untested(&self) -> bool {
        self.stand_in.is_none() && self.machine().is_none_or(|machine| machine.of != self.tested_of())
    }

    /// What runs the models now: what the timed test chose, and the
    /// processor until it has run on what is here.
    pub fn provider(&self) -> Provider {
        match self.machine() {
            Some(machine) if machine.of == self.tested_of() => machine.chosen,
            _ => Provider::Processor,
        }
    }

    /// The parts a study asked for with `options` runs on, by their ids.
    pub fn wanted_for(&self, options: &StudyOptions) -> Vec<&'static str> {
        let provider = self.provider();
        let mut wanted = vec![RUNTIME];
        match provider {
            Provider::Processor => {}
            Provider::AnyCard { .. } => wanted.push(ANY_CARD),
            Provider::Nvidia => wanted.push(NVIDIA),
        }
        wanted.push(if options.stems == StemChoice::Best { STEMS_BEST } else { STEMS });
        wanted.push(BEATS);
        if options.words {
            wanted.push(if provider == Provider::Processor { WORDS_PROCESSOR } else { WORDS });
        }
        wanted
    }

    /// The models a study asked for with `options` is to be run by, or a
    /// sentence that names the parts it needs and that are not here.
    pub fn models_for(&self, options: &StudyOptions) -> Result<Which, String> {
        if let Some(script) = &self.stand_in {
            return Ok(Which::Scripted(script.clone()));
        }
        let provider = self.provider();
        let lacking: Vec<String> = self
            .wanted_for(options)
            .into_iter()
            .filter(|id| !self.ready(id))
            .map(|id| match self.name_of(id) {
                Some((name, size)) => format!("{name} ('{id}', {})", room::said(size)),
                None => format!("'{id}', which is not published for this system yet"),
            })
            .collect();
        if !lacking.is_empty() {
            return Err(format!(
                "a study on {} needs parts that have not been fetched: {}. Setup, in the File menu, lists them with whose they are; over MCP, `setup` does",
                provider_name(provider),
                lacking.join("; ")
            ));
        }
        Ok(Which::Real(real::Setup { parts: self.folder(), provider }))
    }

    /// Runs the timed test: the same noise split on the processor and on
    /// every card the parts that are here can drive, each in a process of
    /// its own (`program`, started as one that studies), and the fastest
    /// that gave the same stems chosen. Takes a minute or two. `said`
    /// hears what is being tried and how far the whole test has come.
    pub fn test(&self, program: &Path, stop: &AtomicBool, said: &mut dyn FnMut(&str, f32)) -> Result<Machine, String> {
        let mut candidates: Vec<(Provider, Which)> = Vec::new();
        let mut results: Vec<Timed> = Vec::new();
        let folder = self.folder();
        let real = |provider: Provider| Which::Real(real::Setup { parts: folder.clone(), provider });
        if let Some(script) = &self.stand_in {
            candidates.push((Provider::Processor, Which::Scripted(script.clone())));
        } else {
            let lacking: Vec<String> = [RUNTIME, STEMS]
                .into_iter()
                .filter(|id| !self.ready(id))
                .map(|id| self.name_of(id).map_or_else(|| id.to_owned(), |(name, _)| name))
                .collect();
            if !lacking.is_empty() {
                return Err(format!(
                    "the timed test splits half a minute of noise into stems, and needs these fetched first: {}",
                    lacking.join(", ")
                ));
            }
            candidates.push((Provider::Processor, real(Provider::Processor)));
            if self.ready(ANY_CARD) {
                said("Looking for graphics cards", 0.0);
                let first = Provider::AnyCard { adapter: 0 };
                let order = Order::Cards { setup: real::Setup { parts: folder.clone(), provider: first } };
                let mut cards: Vec<String> = Vec::new();
                match within(program, &order, stop, TEST_WITHIN, &mut |heard| {
                    if let Said::Cards(found) = heard {
                        cards = found.clone();
                    }
                }) {
                    (Outcome::Stopped, _) => return Err("the timed test was stopped".to_owned()),
                    (Outcome::Failed(why), held) => results.push(Timed {
                        provider: first,
                        device: provider_name(first),
                        checked: None,
                        error: Some(why),
                        peak: held.peak,
                    }),
                    _ => {}
                }
                for adapter in 0..cards.len().min(MOST_CARDS) {
                    let provider = Provider::AnyCard { adapter: adapter as u32 };
                    candidates.push((provider, real(provider)));
                }
            }
            if self.ready(NVIDIA) {
                candidates.push((Provider::Nvidia, real(Provider::Nvidia)));
            }
        }
        let count = candidates.len() as f32;
        for (index, (provider, models)) in candidates.into_iter().enumerate() {
            let device = provider_name(provider);
            let trying = format!("Timing {device}");
            said(&trying, index as f32 / count);
            if let Some(free) = room::free().filter(|&free| free < TEST_NEEDS) {
                results.push(Timed {
                    provider,
                    device,
                    checked: None,
                    error: Some(format!(
                        "there were {} of memory free, and the test wants {}: close something and run it again",
                        room::said(free),
                        room::said(TEST_NEEDS)
                    )),
                    peak: 0,
                });
                continue;
            }
            let order = Order::Check { models, seconds: TEST_SECONDS };
            let (outcome, held) = within(program, &order, stop, TEST_WITHIN, &mut |heard| {
                if let Said::Progress(share) = heard {
                    said(&trying, (index as f32 + share.clamp(0.0, 1.0)) / count);
                }
            });
            let failed = |why: String| Timed {
                provider,
                device: device.clone(),
                checked: None,
                error: Some(why),
                peak: held.peak,
            };
            let timed = match outcome {
                Outcome::Stopped => return Err("the timed test was stopped".to_owned()),
                Outcome::Checked(checked) => Timed {
                    provider,
                    device: checked.device.clone(),
                    checked: Some(checked),
                    error: None,
                    peak: held.peak,
                },
                Outcome::Failed(why) => failed(why),
                Outcome::Done => failed("it ended without saying what it took".to_owned()),
            };
            results.push(timed);
        }
        said("Done", 1.0);
        let machine =
            Machine { tested: now(), of: self.tested_of(), chosen: choose(&results), results, by_user: false };
        write_json(&folder.join("machine.json"), &machine)?;
        Ok(machine)
    }

    /// Has `provider` run the models in place of what the timed test
    /// chose: one of those it timed, which ran.
    pub fn choose(&self, provider: Provider) -> Result<Machine, String> {
        let Some(mut machine) = self.machine().filter(|machine| machine.of == self.tested_of()) else {
            return Err(
                "the timed test has not been run on what is here: run it, and choose among what it timed".to_owned()
            );
        };
        if !machine.results.iter().any(|timed| timed.provider == provider && timed.checked.is_some()) {
            let ran: Vec<String> = machine
                .results
                .iter()
                .filter(|timed| timed.checked.is_some())
                .map(|timed| provider_name(timed.provider))
                .collect();
            return Err(format!(
                "the timed test did not run the models on {}. It ran them on: {}",
                provider_name(provider),
                if ran.is_empty() { "nothing".to_owned() } else { ran.join(", ") }
            ));
        }
        machine.by_user = provider != choose(&machine.results);
        machine.chosen = provider;
        write_json(&self.folder().join("machine.json"), &machine)?;
        Ok(machine)
    }

    // ---- programs that are not Trill's own ------------------------------

    /// The programs a run may use, as they are found now.
    pub fn found(&self) -> Programs {
        match &self.programs {
            Some(programs) => programs.clone(),
            None => Programs { ffmpeg: find_program("ffmpeg"), fetcher: find_program("yt-dlp") },
        }
    }

    /// The programs Trill uses and does not ship, with where each was
    /// found. Looks through the folders programs are in: not for every frame.
    pub fn programs(&self) -> Vec<Found> {
        let found = self.found();
        let curl = if self.programs.is_some() { None } else { find_program("curl") };
        vec![
            Found {
                name: "ffmpeg",
                what: "Reads a recording in a format Trill does not read itself, and makes a film of a song.",
                path: found.ffmpeg,
                get: if cfg!(windows) {
                    "winget install Gyan.FFmpeg, or ffmpeg.org"
                } else if cfg!(target_os = "macos") {
                    "brew install ffmpeg, or ffmpeg.org"
                } else {
                    "your system's packages, or ffmpeg.org"
                },
            },
            Found {
                name: "yt-dlp",
                what: "Fetches the recording behind a link, for a study of it. Needs ffmpeg.",
                path: found.fetcher,
                get: if cfg!(windows) {
                    "winget install yt-dlp.yt-dlp, or github.com/yt-dlp/yt-dlp"
                } else if cfg!(target_os = "macos") {
                    "brew install yt-dlp, or github.com/yt-dlp/yt-dlp"
                } else {
                    "your system's packages, or github.com/yt-dlp/yt-dlp"
                },
            },
            Found {
                name: "curl",
                what: "Fetches the parts listed here. It comes with the system.",
                path: curl,
                get: "curl.se",
            },
        ]
    }
}

/// Starts a process that studies on `order` and attends it, for `within`
/// at the longest.
fn within(
    program: &Path,
    order: &Order,
    stop: &AtomicBool,
    within: Duration,
    on: &mut dyn FnMut(&Said),
) -> (Outcome, room::Held) {
    let mut job = match Job::start(program, order) {
        Ok(job) => job,
        Err(why) => return (Outcome::Failed(why), room::Held::default()),
    };
    let begun = Instant::now();
    let (halt, ended, late) = (AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false));
    let (outcome, held) = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !ended.load(Ordering::Relaxed) {
                if stop.load(Ordering::Relaxed) {
                    halt.store(true, Ordering::Relaxed);
                }
                if begun.elapsed() > within {
                    late.store(true, Ordering::Relaxed);
                    halt.store(true, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let attended = helper::attend(&mut job, Some(TEST_LIMIT), &halt, on);
        ended.store(true, Ordering::Relaxed);
        attended
    });
    match outcome {
        Outcome::Stopped if late.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) => (
            Outcome::Failed(format!("it was not done within {} minutes, and was stopped", within.as_secs() / 60)),
            held,
        ),
        other => (other, held),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_fetch::scripted::{Shelf, part};

    fn quiet() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_study_is_told_which_parts_it_lacks_and_runs_on_the_processor_until_the_machine_is_tested() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let parts = [RUNTIME, STEMS, BEATS, WORDS_PROCESSOR]
            .into_iter()
            .map(|id| part(&shelf, id, id, &[("file.bin", id.as_bytes().repeat(100))]))
            .collect();
        let fetches = Arc::new(Fetches::new(shelf));
        fetches.add(List { parts, ..list(folder.path().to_owned()) }).unwrap();
        let setup = Setup::over(fetches.clone(), None, Some(Programs::default()));
        let lacking = setup.models_for(&StudyOptions::default()).unwrap_err();
        assert!(
            lacking.starts_with(
                "a study on the processor needs parts that have not been fetched: runtime ('runtime', 0 MB); stems"
            ) && lacking.contains("words-processor")
                && lacking.ends_with("over MCP, `setup` does"),
            "{lacking}"
        );
        for id in [RUNTIME, STEMS, BEATS] {
            fetches.fetch_now(id, &quiet(), &mut |_, _| {}).unwrap();
        }
        // Without the words, the model that writes them down is not wanted.
        let options = StudyOptions { words: false, ..StudyOptions::default() };
        let which = setup.models_for(&options).unwrap();
        assert_eq!(which, Which::Real(real::Setup { parts: folder.path().to_owned(), provider: Provider::Processor }));
        assert!(setup.models_for(&StudyOptions::default()).unwrap_err().contains("words-processor"));
        // The best stems are a part of their own, which is not published here.
        let best = StudyOptions { stems: StemChoice::Best, words: false, ..StudyOptions::default() };
        assert!(
            setup.models_for(&best).unwrap_err().contains("'stems-best', which is not published for this system yet")
        );
        assert!(setup.untested() && setup.machine().is_none());
        assert_eq!(setup.provider(), Provider::Processor);
        assert!(setup.programs().iter().all(|program| program.path.is_none()));
    }

    fn timed(provider: Provider, seconds: f64, sum: f64) -> Timed {
        Timed {
            provider,
            device: provider_name(provider),
            checked: Some(Checked {
                device: provider_name(provider),
                ready_seconds: 1.0,
                seconds_a_minute: seconds,
                sum,
            }),
            error: None,
            peak: 0,
        }
    }

    #[test]
    fn the_fastest_that_gave_the_same_stems_is_chosen() {
        let card = Provider::AnyCard { adapter: 0 };
        let slow_card = Provider::AnyCard { adapter: 1 };
        // A card that is there and slower than the processor is not used.
        assert_eq!(
            choose(&[timed(Provider::Processor, 25.0, 100.0), timed(slow_card, 67.0, 100.00001)]),
            Provider::Processor
        );
        assert_eq!(
            choose(&[
                timed(Provider::Processor, 25.0, 100.0),
                timed(card, 6.5, 100.00002),
                timed(Provider::Nvidia, 3.4, 99.99999)
            ]),
            Provider::Nvidia
        );
        // A card that computes something else is not chosen, however fast.
        assert_eq!(choose(&[timed(Provider::Processor, 25.0, 100.0), timed(card, 1.0, 71.0)]), Provider::Processor);
        // Where the processor did not run, most of the others say what is right.
        let failed =
            Timed { checked: None, error: Some("it went down".into()), ..timed(Provider::Processor, 0.0, 0.0) };
        assert_eq!(
            choose(&[
                failed.clone(),
                timed(card, 9.0, 50.0),
                timed(slow_card, 30.0, 50.0),
                timed(Provider::Nvidia, 2.0, 12.0)
            ]),
            card
        );
        assert_eq!(choose(&[failed]), Provider::Processor);
        assert_eq!(choose(&[]), Provider::Processor);
        assert_eq!(provider_name(slow_card), "graphics card 2");
    }
}
