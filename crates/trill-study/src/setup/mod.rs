//! The setup: what a study needs that Trill does not ship, fetched by the
//! user a part at a time, and the timed test that says what on this
//! machine runs the models.
//!
//! - **Nothing is fetched unasked.** A part comes when someone asks for
//!   that part by name ([`Setup::fetch`]), from where its makers publish
//!   it ([`catalogue`]), by the system's own `curl` ([`Curl`]): Trill
//!   brings no way of its own to talk to the network. A test never
//!   fetches: it hands the setup a shelf of its own ([`scripted`]).
//! - **A file is what it was pinned to be, or it is not kept.** Each is
//!   written under another name, checked against its checksum when it is
//!   whole, and only then given its own name. What had come of a file when
//!   a fetch was stopped or failed is gone on from the next time.
//! - **What runs the models is measured, not assumed.** The timed test
//!   ([`Setup::test`]) splits the same half minute of noise on the
//!   processor and on every graphics card the fetched parts can drive,
//!   each in a process of its own, and takes the fastest that gave the
//!   same stems as the others. A card that is there and slower than the
//!   processor is not used; one whose driver faults costs a line in the
//!   result. Until the test has run, and again when a part that runs
//!   models changes, studies run on the processor.

pub mod catalogue;
pub mod scripted;
pub mod zip;

pub use catalogue::{Fetched, Keep, Part};

use crate::helper::{self, Checked, Job, Order, Outcome, Said};
use crate::link::{find_program, hide_console, last_line};
use crate::models::Which;
use crate::models::real::{
    self, ANY_CARD, BEATS, NVIDIA, Provider, RUNTIME, STEMS, STEMS_BEST, WORDS, WORDS_PROCESSOR,
};
use crate::models::scripted::Script;
use crate::room;
use crate::run::Programs;
use crate::store::{StemChoice, StudyOptions, partial_of, write_json};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How a fetch ended without the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// Whoever asked for it stopped it.
    Stopped,
    /// A sentence: what went wrong, and what to do.
    Failed(String),
}

/// How a file comes from where it is published.
pub trait Fetcher: Send + Sync {
    /// Brings what is at `url` into the file `to`, going on from whatever
    /// of it is there already, and says how many bytes of it there are as
    /// it goes. Ends at once when `stop` is set, and leaves what has come.
    fn fetch(&self, url: &str, to: &Path, stop: &AtomicBool, got: &mut dyn FnMut(u64)) -> Result<(), Ended>;
}

fn length(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// The system's own `curl`, which Windows, macOS and nearly every Linux
/// come with: found when something is fetched, never shipped.
pub struct Curl;

impl Fetcher for Curl {
    fn fetch(&self, url: &str, to: &Path, stop: &AtomicBool, got: &mut dyn FnMut(u64)) -> Result<(), Ended> {
        let Some(program) = find_program("curl") else {
            return Err(Ended::Failed(
                "Trill fetches with the system's own curl, which was not found here: install curl and fetch again"
                    .to_owned(),
            ));
        };
        // Twice at most: once more from the start, where what had come could not be gone on from.
        for attempt in 0..2 {
            let mut command = Command::new(&program);
            command
                .args(["--fail", "--location", "--silent", "--show-error", "--connect-timeout", "30"])
                // A connection that brings next to nothing for a minute is given up.
                .args(["--speed-limit", "1000", "--speed-time", "60", "--retry", "2", "--continue-at", "-", "--output"])
                .arg(to)
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            hide_console(&mut command);
            let mut process = command.spawn().map_err(|error| {
                Ended::Failed(format!("curl ('{}') could not be started: {error}", program.display()))
            })?;
            let status = loop {
                if stop.load(Ordering::Relaxed) {
                    let _ = process.kill();
                    let _ = process.wait();
                    return Err(Ended::Stopped);
                }
                got(length(to));
                match process.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(error) => return Err(Ended::Failed(error.to_string())),
                }
            };
            got(length(to));
            if status.success() {
                return Ok(());
            }
            let mut said = Vec::new();
            if let Some(mut stderr) = process.stderr.take() {
                let _ = stderr.read_to_end(&mut said);
            }
            let said = last_line(&said);
            // 33: the server does not go on from the middle. 416: there was more here than there is there.
            if attempt == 0 && to.exists() && (status.code() == Some(33) || said.contains("416")) {
                let _ = std::fs::remove_file(to);
                continue;
            }
            return Err(Ended::Failed(said.trim_start_matches("curl: ").to_owned()));
        }
        Err(Ended::Failed("it could not be gone on with, nor begun again".to_owned()))
    }
}

/// A fetcher that fetches nothing: for a run that touches nothing outside itself.
pub struct Nothing;

impl Fetcher for Nothing {
    fn fetch(&self, _url: &str, _to: &Path, _stop: &AtomicBool, _got: &mut dyn FnMut(u64)) -> Result<(), Ended> {
        Err(Ended::Failed("this run was started to touch nothing outside itself, and fetches nothing".to_owned()))
    }
}

/// The SHA-256 of a file, in hexadecimal.
fn checksum(path: &Path, stop: &AtomicBool) -> Result<String, Ended> {
    let said = |error: std::io::Error| Ended::Failed(format!("{}: {error}", path.display()));
    let mut file = std::fs::File::open(path).map_err(said)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Ended::Stopped);
        }
        let read = file.read(&mut buffer).map_err(said)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Who a file is fetched from: the address without the way to it.
fn host(url: &str) -> &str {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or(url)
}

/// What `part.json` in a part's folder holds: what of it was fetched,
/// checked and kept.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Kept {
    /// The checksums of the fetched files that are done with.
    done: Vec<String>,
    /// What they left in the folder: each file's name and size.
    files: Vec<(String, u64)>,
}

/// Where a part is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PartState {
    /// Not here, or not all of it.
    Missing,
    /// Asked for, and waiting for the part that is being fetched.
    Waiting,
    Fetching,
    /// All of a file has come and is being checked against its checksum.
    Checking,
    Ready,
}

/// One part as the setup has it now.
#[derive(Clone, Debug, PartialEq)]
pub struct PartStatus {
    pub part: Part,
    pub state: PartState,
    /// How many bytes of what is fetched for it have come.
    pub got: u64,
    /// The room it takes on disk now.
    pub on_disk: u64,
    /// Why it was not fetched, the last time it was tried.
    pub error: Option<String>,
}

/// A fetch that is going on, or waiting its turn.
struct Going {
    stop: AtomicBool,
    at: Mutex<(PartState, u64)>,
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

/// What a setup is made of.
pub struct SetupOptions {
    /// The folder the parts are kept in.
    pub folder: PathBuf,
    pub parts: Vec<Part>,
    pub fetcher: Arc<dyn Fetcher>,
    /// Scripted models in place of the real ones, which then need no part: for tests.
    pub stand_in: Option<Script>,
    /// The programs that are taken to be installed, in place of looking: for tests.
    pub programs: Option<Programs>,
}

/// The setup of the models on this machine.
pub struct Setup {
    folder: Mutex<PathBuf>,
    parts: Vec<Part>,
    fetcher: Arc<dyn Fetcher>,
    stand_in: Option<Script>,
    programs: Option<Programs>,
    going: Mutex<HashMap<&'static str, Arc<Going>>>,
    failed: Mutex<HashMap<&'static str, String>>,
    /// One part is fetched at a time.
    turn: Mutex<()>,
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
/// Room left on a disk beyond what a part takes.
const DISK_MARGIN: u64 = 512 << 20;

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
    pub fn new(options: SetupOptions) -> Self {
        Self {
            folder: Mutex::new(options.folder),
            parts: options.parts,
            fetcher: options.fetcher,
            stand_in: options.stand_in,
            programs: options.programs,
            going: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashMap::new()),
            turn: Mutex::new(()),
        }
    }

    /// The setup as a program has it: the parts that are published for
    /// this system, kept in `folder`, fetched by the system's `curl`.
    pub fn real(folder: impl Into<PathBuf>) -> Self {
        Self::new(SetupOptions {
            folder: folder.into(),
            parts: catalogue::parts(),
            fetcher: Arc::new(Curl),
            stand_in: None,
            programs: None,
        })
    }

    /// The folder the parts are kept in.
    pub fn folder(&self) -> PathBuf {
        self.folder.lock().clone()
    }

    /// Keeps the parts in `folder` from now on. What is in the folder
    /// they were in stays there: whoever moves the setup moves that.
    pub fn set_folder(&self, folder: PathBuf) -> Result<(), String> {
        if !self.going.lock().is_empty() {
            return Err(
                "a part is being fetched: stop it, or wait for it, before the parts are kept elsewhere".to_owned()
            );
        }
        *self.folder.lock() = folder;
        Ok(())
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The part `id`, or a sentence that names the ones there are.
    pub fn part(&self, id: &str) -> Result<Part, String> {
        self.parts.iter().find(|part| part.id == id).copied().ok_or_else(|| {
            let known: Vec<&str> = self.parts.iter().map(|part| part.id).collect();
            format!("there is no part '{id}'. Parts: {}", known.join(", "))
        })
    }

    fn part_folder(&self, part: &Part) -> PathBuf {
        self.folder().join(part.id)
    }

    /// What of a part is done with, if everything it left is still there.
    fn kept(&self, part: &Part) -> Kept {
        let folder = self.part_folder(part);
        let kept: Kept = std::fs::read_to_string(folder.join("part.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let whole = kept.files.iter().all(|(name, size)| {
            std::fs::metadata(folder.join(name)).is_ok_and(|metadata| metadata.is_file() && metadata.len() == *size)
        });
        if whole { kept } else { Kept::default() }
    }

    /// Whether all of the part `id` is here, as it was pinned.
    pub fn ready(&self, id: &str) -> bool {
        self.parts.iter().find(|part| part.id == id).is_some_and(|part| {
            let kept = self.kept(part);
            part.files.iter().all(|file| kept.done.iter().any(|done| done == file.sha256))
        })
    }

    /// Every part as it is now.
    pub fn status(&self) -> Vec<PartStatus> {
        let going = self.going.lock().clone();
        let failed = self.failed.lock().clone();
        self.parts
            .iter()
            .map(|part| {
                let folder = self.part_folder(part);
                let kept = self.kept(part);
                let done = |file: &Fetched| kept.done.iter().any(|done| done == file.sha256);
                let on_disk = crate::store::StudyFolder::new(&folder).size();
                let (state, got) = match going.get(part.id) {
                    Some(going) => *going.at.lock(),
                    None if part.files.iter().all(done) => (PartState::Ready, part.size()),
                    None => {
                        // What has come of the files that are not done with.
                        let got = part
                            .files
                            .iter()
                            .map(|file| {
                                if done(file) {
                                    file.size
                                } else {
                                    length(&partial_of(&folder.join(fetched_name(file)))).min(file.size)
                                }
                            })
                            .sum();
                        (PartState::Missing, got)
                    }
                };
                PartStatus { part: *part, state, got, on_disk, error: failed.get(part.id).cloned() }
            })
            .collect()
    }

    /// Fetches the part `id` now, and answers when it is here or the fetch
    /// has ended otherwise. `said` hears where it is and how many bytes
    /// have come.
    pub fn fetch_now(&self, id: &str, stop: &AtomicBool, said: &mut dyn FnMut(PartState, u64)) -> Result<(), Ended> {
        let part = self.part(id).map_err(Ended::Failed)?;
        let folder = self.part_folder(&part);
        let io = |error: std::io::Error| Ended::Failed(format!("{}: {error}", folder.display()));
        std::fs::create_dir_all(&folder).map_err(io)?;
        let mut kept = self.kept(&part);
        let mut before = 0u64;
        for file in part.files {
            if kept.done.iter().any(|done| done == file.sha256) {
                before += file.size;
                continue;
            }
            let name = fetched_name(file);
            let target = folder.join(&name);
            let partial = partial_of(&target);
            let from = host(file.url);
            if length(&partial) > file.size {
                let _ = std::fs::remove_file(&partial);
            }
            let have = length(&partial);
            // An archive is unpacked beside itself before it is removed.
            let unpacked = if matches!(file.keep, Keep::As(_)) { 0 } else { file.size * 2 };
            let wanted = file.size - have + unpacked + DISK_MARGIN;
            if let Some(free) = room::disk_free(&folder).filter(|&free| free < wanted) {
                return Err(Ended::Failed(format!(
                    "there is not the room on the disk for {}: {} are free where {} is kept, and {} are wanted. Make room, or keep the parts on another disk (Settings, Studies)",
                    part.name,
                    room::said(free),
                    folder.display(),
                    room::said(wanted),
                )));
            }
            if have < file.size {
                said(PartState::Fetching, before + have);
                self.fetcher
                    .fetch(file.url, &partial, stop, &mut |got| said(PartState::Fetching, before + got.min(file.size)))
                    .map_err(|ended| match ended {
                        Ended::Stopped => Ended::Stopped,
                        Ended::Failed(why) => Ended::Failed(format!(
                            "{name} could not be fetched from {from}: {why}. What came of it is kept, and fetching it again goes on from there"
                        )),
                    })?;
            }
            said(PartState::Checking, before + file.size);
            let sum = checksum(&partial, stop)?;
            if sum != file.sha256 {
                let _ = std::fs::remove_file(&partial);
                return Err(Ended::Failed(format!(
                    "{name} from {from} is not the file Trill was made for: it was replaced where it is published, or damaged on the way (its checksum begins {}, where {} was expected). It was not kept",
                    &sum[..12],
                    &file.sha256[..12.min(file.sha256.len())],
                )));
            }
            let left: Vec<String> = match file.keep {
                Keep::As(_) => {
                    std::fs::rename(&partial, &target).map_err(io)?;
                    vec![name]
                }
                Keep::Members(members) => {
                    let wanted =
                        |member: &str| members.iter().find(|(from, _)| *from == member).map(|(_, to)| (*to).to_owned());
                    let written = zip::unpack(&partial, &folder, &wanted).map_err(Ended::Failed)?;
                    if let Some((lacking, _)) = members.iter().find(|(_, to)| !written.iter().any(|name| name == to)) {
                        return Err(Ended::Failed(format!(
                            "{lacking} is not in {name} from {from}, as it was when Trill was made"
                        )));
                    }
                    let _ = std::fs::remove_file(&partial);
                    written
                }
                Keep::Under(under, ending) => {
                    let wanted = |member: &str| {
                        member
                            .strip_prefix(under)
                            .filter(|rest| rest.ends_with(ending) && !rest.contains('/'))
                            .map(str::to_owned)
                    };
                    let written = zip::unpack(&partial, &folder, &wanted).map_err(Ended::Failed)?;
                    if written.is_empty() {
                        return Err(Ended::Failed(format!(
                            "nothing of {under} is in {name} from {from}, as it was when Trill was made"
                        )));
                    }
                    let _ = std::fs::remove_file(&partial);
                    written
                }
            };
            kept.done.push(file.sha256.to_owned());
            for name in left {
                let size = length(&folder.join(&name));
                kept.files.retain(|(known, _)| *known != name);
                kept.files.push((name, size));
            }
            write_json(&folder.join("part.json"), &kept).map_err(Ended::Failed)?;
            before += file.size;
        }
        said(PartState::Ready, before);
        Ok(())
    }

    /// Starts fetching the part `id` and answers at once: [`Setup::status`]
    /// says how it goes. Parts are fetched one at a time, in the order
    /// they were asked for.
    pub fn fetch(self: &Arc<Self>, id: &str) -> Result<Part, String> {
        let part = self.part(id)?;
        if self.ready(id) {
            return Err(format!("{} is here already", part.name));
        }
        let going = Arc::new(Going { stop: AtomicBool::new(false), at: Mutex::new((PartState::Waiting, 0)) });
        {
            let mut all = self.going.lock();
            if all.contains_key(part.id) {
                return Err(format!("{} is being fetched already", part.name));
            }
            all.insert(part.id, going.clone());
        }
        self.failed.lock().remove(part.id);
        let setup = self.clone();
        let spawned = std::thread::Builder::new().name("trill-setup-fetch".into()).spawn(move || {
            let ended = {
                let _turn = setup.turn.lock();
                if going.stop.load(Ordering::Relaxed) {
                    Err(Ended::Stopped)
                } else {
                    setup.fetch_now(part.id, &going.stop, &mut |state, got| *going.at.lock() = (state, got))
                }
            };
            if let Err(Ended::Failed(why)) = ended {
                log::warn!("{}: {why}", part.name);
                setup.failed.lock().insert(part.id, why);
            }
            setup.going.lock().remove(part.id);
        });
        if let Err(error) = spawned {
            self.going.lock().remove(part.id);
            return Err(format!("the thread that fetches could not be started: {error}"));
        }
        Ok(part)
    }

    /// Stops the fetch of the part `id`. What has come of it is kept.
    pub fn stop(&self, id: &str) -> Result<Part, String> {
        let part = self.part(id)?;
        match self.going.lock().get(part.id) {
            Some(going) => {
                going.stop.store(true, Ordering::Relaxed);
                Ok(part)
            }
            None => Err(format!("{} is not being fetched", part.name)),
        }
    }

    /// Whether anything is being fetched or waiting to be.
    pub fn fetching(&self) -> bool {
        !self.going.lock().is_empty()
    }

    /// Removes the part `id` and everything that came of it, and answers
    /// with the room that made.
    pub fn remove(&self, id: &str) -> Result<u64, String> {
        let part = self.part(id)?;
        if self.going.lock().contains_key(part.id) {
            return Err(format!("{} is being fetched: stop that first", part.name));
        }
        let folder = self.part_folder(&part);
        let freed = crate::store::StudyFolder::new(&folder).size();
        if folder.is_dir() {
            std::fs::remove_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        }
        self.failed.lock().remove(part.id);
        Ok(freed)
    }

    // ---- what runs the models -------------------------------------------

    /// The checksums of the parts that run the models, as far as they are here.
    fn tested_of(&self) -> Vec<String> {
        self.parts
            .iter()
            .filter(|part| [RUNTIME, ANY_CARD, NVIDIA, STEMS].contains(&part.id) && self.ready(part.id))
            .flat_map(|part| part.files.iter().map(|file| file.sha256.to_owned()))
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

    /// The models a study asked for with `options` is to be run by, or a
    /// sentence that names the parts it needs and that are not here.
    pub fn models_for(&self, options: &StudyOptions) -> Result<Which, String> {
        if let Some(script) = &self.stand_in {
            return Ok(Which::Scripted(script.clone()));
        }
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
        let lacking: Vec<String> = wanted
            .into_iter()
            .filter(|id| !self.ready(id))
            .map(|id| match self.parts.iter().find(|part| part.id == id) {
                Some(part) => format!("{} ('{id}', {})", part.name, room::said(part.size())),
                None => format!("'{id}', which is not published for this system yet"),
            })
            .collect();
        if !lacking.is_empty() {
            return Err(format!(
                "a study on {} needs parts that have not been fetched: {}. Settings, Studies lists them with whose they are; over MCP, `study_setup` does",
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
            let lacking: Vec<&str> = [RUNTIME, STEMS]
                .into_iter()
                .filter(|id| !self.ready(id))
                .map(|id| self.parts.iter().find(|part| part.id == id).map_or(id, |part| part.name))
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
            let timed = match outcome {
                Outcome::Stopped => return Err("the timed test was stopped".to_owned()),
                Outcome::Checked(checked) => Timed {
                    provider,
                    device: checked.device.clone(),
                    checked: Some(checked),
                    error: None,
                    peak: held.peak,
                },
                Outcome::Failed(why) => Timed { provider, device, checked: None, error: Some(why), peak: held.peak },
                Outcome::Done => Timed {
                    provider,
                    device,
                    checked: None,
                    error: Some("it ended without saying what it took".to_owned()),
                    peak: held.peak,
                },
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
                what: "Fetches the parts above. It comes with the system.",
                path: curl,
                get: "curl.se",
            },
        ]
    }
}

/// The name a fetched file is kept under: its own, or the archive's.
fn fetched_name(file: &Fetched) -> String {
    match file.keep {
        Keep::As(name) => name.to_owned(),
        _ => file.url.rsplit('/').next().unwrap_or("archive").to_owned(),
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
    use super::scripted::{Shelf, part};
    use super::*;

    fn setup(folder: &Path, parts: Vec<Part>, shelf: Arc<Shelf>) -> Arc<Setup> {
        Arc::new(Setup::new(SetupOptions {
            folder: folder.to_owned(),
            parts,
            fetcher: shelf,
            stand_in: None,
            programs: Some(Programs::default()),
        }))
    }

    fn wait_for(setup: &Setup, id: &str, state: PartState) -> PartStatus {
        for _ in 0..600 {
            let status = setup.status().into_iter().find(|status| status.part.id == id).unwrap();
            if status.state == state && (state != PartState::Missing || !setup.fetching()) {
                return status;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{id} never came to {state:?}");
    }

    #[test]
    fn a_part_is_fetched_checked_and_kept_and_removed_again() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let model = part(
            &shelf,
            "beats",
            "Beats",
            &[("beat.onnx", vec![7u8; 300_000]), ("tokens.txt", b"a 1\nb 2\n".to_vec())],
        );
        let setup = setup(folder.path(), vec![model], shelf.clone());
        let before = &setup.status()[0];
        assert_eq!((before.state, before.got, before.on_disk), (PartState::Missing, 0, 0));
        assert!(!setup.ready("beats"));

        assert_eq!(setup.fetch("beats").unwrap().name, "Beats");
        let ready = wait_for(&setup, "beats", PartState::Ready);
        assert_eq!(ready.got, 300_008);
        assert!(ready.on_disk >= 300_008 && ready.error.is_none(), "{ready:?}");
        assert!(setup.ready("beats"));
        assert_eq!(std::fs::read(folder.path().join("beats").join("tokens.txt")).unwrap(), b"a 1\nb 2\n");
        // Nothing is left under the name it was written by.
        assert!(!folder.path().join("beats").join("beat.onnx.partial").exists());
        // What is here is not fetched again.
        assert_eq!(setup.fetch("beats").unwrap_err(), "Beats is here already");
        assert_eq!(shelf.asked().len(), 2);

        // A file that went missing makes the part missing, and all of it is fetched again.
        std::fs::remove_file(folder.path().join("beats").join("tokens.txt")).unwrap();
        assert!(!setup.ready("beats"));
        setup.fetch_now("beats", &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert!(setup.ready("beats"));

        assert!(setup.remove("beats").unwrap() >= 300_008);
        assert!(!setup.ready("beats") && !folder.path().join("beats").exists());
        assert_eq!(setup.part("nope").unwrap_err(), "there is no part 'nope'. Parts: beats");
    }

    #[test]
    fn a_file_that_is_not_the_one_pinned_is_refused_and_not_kept() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let model = part(&shelf, "stems", "Stems", &[("stems.onnx", vec![3u8; 50_000])]);
        // Replaced where it is published.
        shelf.put(model.files[0].url, vec![4u8; 50_000]);
        let setup = setup(folder.path(), vec![model], shelf);
        let ended = setup.fetch_now("stems", &AtomicBool::new(false), &mut |_, _| {}).unwrap_err();
        let Ended::Failed(why) = ended else { panic!("{ended:?}") };
        assert!(
            why.starts_with(
                "stems.onnx from shelf.test is not the file Trill was made for: it was replaced where it is published"
            ) && why.ends_with("It was not kept"),
            "{why}"
        );
        assert!(!setup.ready("stems"));
        let left: Vec<_> = std::fs::read_dir(folder.path().join("stems")).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn a_fetch_that_fails_or_is_stopped_is_gone_on_with_from_where_it_was() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let bytes: Vec<u8> = (0..400_000u32).map(|index| (index % 251) as u8).collect();
        let model = part(&shelf, "words", "Words", &[("words.onnx", bytes.clone())]);
        let setup = setup(folder.path(), vec![model], shelf.clone());

        // The connection is lost after a part of it.
        shelf.cut_after(150_000);
        setup.fetch("words").unwrap();
        let failed = wait_for(&setup, "words", PartState::Missing);
        let why = failed.error.clone().unwrap();
        assert!(
            why.starts_with("words.onnx could not be fetched from shelf.test: the connection was lost")
                && why.ends_with("fetching it again goes on from there"),
            "{why}"
        );
        assert!(failed.got >= 150_000 && failed.got < 400_000, "{failed:?}");

        // Fetched again, it is stopped part of the way, and what came is still kept.
        shelf.slow(Duration::from_millis(15));
        setup.fetch("words").unwrap();
        for _ in 0..500 {
            if setup.status()[0].got > failed.got + 60_000 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(setup.fetch("words").unwrap_err(), "Words is being fetched already");
        assert_eq!(setup.remove("words").unwrap_err(), "Words is being fetched: stop that first");
        setup.stop("words").unwrap();
        let stopped = wait_for(&setup, "words", PartState::Missing);
        assert!(stopped.error.is_none() && stopped.got > failed.got && stopped.got < 400_000, "{stopped:?}");
        assert_eq!(setup.stop("words").unwrap_err(), "Words is not being fetched");

        // And the third time it is gone on with to the end, and is the file.
        shelf.slow(Duration::ZERO);
        setup.fetch("words").unwrap();
        wait_for(&setup, "words", PartState::Ready);
        assert_eq!(std::fs::read(folder.path().join("words").join("words.onnx")).unwrap(), bytes);
        // Each fetch began where the one before it left off.
        let asked = shelf.began();
        assert_eq!(asked[0], 0);
        assert!(asked[1] >= 150_000 && asked[2] > asked[1], "{asked:?}");
    }

    #[test]
    fn the_members_a_part_wants_are_taken_out_of_its_archives() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let library = vec![9u8; 20_000];
        let zipped = zip::tests::archive(&[
            ("runtime-1.0/lib/runtime.dll", &library, true),
            ("runtime-1.0/lib/runtime.lib", &[1u8; 900], true),
            ("runtime-1.0/README", b"read me", false),
        ]);
        let wheel = zip::tests::archive(&[
            ("maker/lib/bin/one.dll", &[1u8; 700], true),
            ("maker/lib/bin/two.dll", &[2u8; 800], false),
            ("maker/lib/bin/deeper/three.dll", &[3u8; 100], false),
            ("maker/lib/include/one.h", b"header", false),
        ]);
        let mut runtime = part(&shelf, "runtime", "The runtime", &[("runtime-1.0.zip", zipped), ("maker.whl", wheel)]);
        let files: Vec<Fetched> = vec![
            Fetched { keep: Keep::Members(&[("runtime-1.0/lib/runtime.dll", "runtime.dll")]), ..runtime.files[0] },
            Fetched { keep: Keep::Under("maker/lib/bin/", ".dll"), ..runtime.files[1] },
        ];
        runtime.files = Box::leak(files.into_boxed_slice());
        let setup = setup(folder.path(), vec![runtime], shelf);
        setup.fetch_now("runtime", &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(folder.path().join("runtime"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        // The archives themselves are not kept.
        assert_eq!(names, ["one.dll", "part.json", "runtime.dll", "two.dll"]);
        assert_eq!(std::fs::read(folder.path().join("runtime").join("runtime.dll")).unwrap(), library);
        assert!(setup.ready("runtime"));
    }

    #[test]
    fn a_study_is_told_which_parts_it_lacks_and_runs_on_the_processor_until_the_machine_is_tested() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let parts: Vec<Part> = [RUNTIME, STEMS, BEATS, WORDS_PROCESSOR]
            .into_iter()
            .map(|id| part(&shelf, id, id, &[("file.bin", id.as_bytes().repeat(100))]))
            .collect();
        let setup = setup(folder.path(), parts, shelf);
        let lacking = setup.models_for(&StudyOptions::default()).unwrap_err();
        assert!(
            lacking.starts_with(
                "a study on the processor needs parts that have not been fetched: runtime ('runtime', 0 MB); stems"
            ) && lacking.contains("words-processor")
                && lacking.ends_with("over MCP, `study_setup` does"),
            "{lacking}"
        );
        for id in [RUNTIME, STEMS, BEATS] {
            setup.fetch_now(id, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
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

    #[test]
    fn a_fetcher_for_a_run_that_touches_nothing_fetches_nothing() {
        let folder = tempfile::tempdir().unwrap();
        let ended =
            Nothing.fetch("https://example.org/a", &folder.path().join("a"), &AtomicBool::new(false), &mut |_| {});
        assert!(matches!(ended, Err(Ended::Failed(why)) if why.ends_with("fetches nothing")));
        assert_eq!(host("https://huggingface.co/a/b/resolve/c/d.onnx"), "huggingface.co");
    }
}
