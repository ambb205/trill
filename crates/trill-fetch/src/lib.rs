//! What Trill fetches when its user asks: *parts* that others publish
//! (the models a study runs, their runtime, sets of sampled instruments),
//! none of which Trill ships or hosts.
//!
//! The rules are the same for every part, whoever lists it:
//!
//! - **Nothing is fetched unasked.** A part comes when someone asks for
//!   that part by name ([`Fetches::fetch`]), having been shown its size,
//!   its licence in the words of its source, who is to be credited and
//!   where it comes from ([`Part`]). A test never fetches: it hands the
//!   engine a shelf of its own ([`scripted`]).
//! - **By the system's own `curl`** ([`Curl`]), found when something is
//!   fetched: Trill brings no way of its own to talk to a network, and
//!   sends nothing but the request for the file.
//! - **A file is what it was pinned to be, or it is not kept.** Each is
//!   written under another name, checked against its checksum when it is
//!   whole, and only then given its own name. An archive is checked as it
//!   was fetched, or by what it unpacks to ([`Keep::Tree`]), or both.
//! - **A fetch can be stopped, and is gone on with.** What had come of a
//!   file when a fetch was stopped or failed is kept, and the next fetch
//!   begins there.
//! - **A failure is a sentence**: what went wrong, and what to do.
//!
//! # Handing it a list of your own
//!
//! A crate that has things to fetch makes a [`List`] of [`Part`]s and
//! adds it ([`Fetches::add`]): the list's `folder` is where its parts are
//! kept (each in a folder named by its id), which the user may choose
//! ([`Fetches::set_folder`]); its `changed` is called when a part has
//! arrived or was removed, with the part's folder. A part's files are
//! [`Fetched`] one by one in their order; `examples/pin.rs` prints the
//! size and checksums of a file or an archive, to write one down by.
//! Both front ends then show the list and fetch from it with no more
//! said: the window's settings, and the `setup` tool.

pub mod scripted;
pub mod whole;
pub mod zip;

pub use whole::{disk_free, find_program, hide_console, last_line, partial_of, said, write_json, write_whole};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use whole::length;

/// What becomes of a fetched file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Keep {
    /// It is one of the part's files, at this path below the part's
    /// folder (forward slashes).
    As(String),
    /// It is an archive: these of its members are kept, each at the path
    /// after it.
    Members(Vec<(String, String)>),
    /// It is an archive: every member directly in `folder` whose name
    /// ends in `ending` is kept, by its own name.
    Under { folder: String, ending: String },
    /// It is an archive: everything below `under` in it is kept, with its
    /// folders (`under` ends in a slash, or is empty for all of it).
    /// `sum` pins what is unpacked ([`tree_checksum`]): for an archive
    /// that is made anew each time it is asked for, whose own bytes
    /// cannot be pinned. Empty where the archive's own checksum is enough.
    Tree { under: String, sum: String },
}

/// One file a part is fetched as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// Where it is published: an address that names one version, and
    /// that asks nothing of whoever fetches it.
    pub url: String,
    /// Its SHA-256 as it is fetched, in hexadecimal. Empty only for an
    /// archive pinned by what it unpacks to.
    pub sha256: String,
    /// Its size as it is fetched, in bytes.
    pub size: u64,
    pub keep: Keep,
}

impl Fetched {
    /// A file kept as it comes, at `path` below the part's folder.
    pub fn file(url: &str, sha256: &str, size: u64, path: &str) -> Self {
        Self { url: url.to_owned(), sha256: sha256.to_owned(), size, keep: Keep::As(path.to_owned()) }
    }

    /// An archive, of which `keep` says what is kept.
    pub fn archive(url: &str, sha256: &str, size: u64, keep: Keep) -> Self {
        Self { url: url.to_owned(), sha256: sha256.to_owned(), size, keep }
    }

    /// What it is pinned to.
    fn pin(&self) -> &str {
        match &self.keep {
            Keep::Tree { sum, .. } if self.sha256.is_empty() => sum,
            _ => &self.sha256,
        }
    }

    /// The name it is kept under while it is fetched: its own, or the archive's.
    fn name(&self) -> String {
        match &self.keep {
            Keep::As(path) => path.clone(),
            _ => self.url.rsplit('/').next().filter(|name| !name.is_empty()).unwrap_or("archive").to_owned(),
        }
    }
}

/// One thing that can be fetched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Part {
    /// The name of its folder, and what it is asked for by: letters,
    /// digits and dashes, and no other part's.
    pub id: String,
    pub name: String,
    /// What it is for, and when it is wanted.
    pub what: String,
    /// Its licence, in the words of its source where it has any.
    pub licence: String,
    /// Who asks to be named for it, and how, in their words. Empty where
    /// nobody does.
    pub credit: String,
    /// Who publishes the files, and where.
    pub from: String,
    /// The parts it is no use without.
    pub needs: Vec<String>,
    pub files: Vec<Fetched>,
}

impl Part {
    /// What is fetched for it, in bytes.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

/// What happened to a part of a list.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// All of it is here now, as it was pinned.
    Arrived,
    /// It was removed.
    Removed,
}

/// What a list has run when one of its parts arrives or is removed: it
/// is told which, and the part's folder. Run on the thread that fetched.
pub type Hook = Arc<dyn Fn(Change, &Part, &Path) + Send + Sync>;

/// A list of parts, as whoever has them hands it to the engine.
#[derive(Clone)]
pub struct List {
    /// What the list is asked for by: `models`, `instruments`.
    pub id: String,
    /// What it is called where it is shown.
    pub title: String,
    /// A line on what these are, shown over them.
    pub about: String,
    /// Where its parts are kept, each in a folder named by its id.
    pub folder: PathBuf,
    pub parts: Vec<Part>,
    pub changed: Option<Hook>,
}

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
                .arg("--")
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The SHA-256 of a file, in hexadecimal.
pub fn checksum(path: &Path, stop: &AtomicBool) -> Result<String, Ended> {
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
    Ok(hex(&hasher.finalize()))
}

/// The checksum of the files at `paths` below `folder` (forward slashes),
/// whatever archive they came in: the SHA-256 of one line for each file,
/// `<the file's SHA-256>  <its path>` and a line feed, the lines in the
/// order of their paths as bytes.
pub fn tree_checksum(folder: &Path, paths: &[String], stop: &AtomicBool) -> Result<String, Ended> {
    let mut paths: Vec<&String> = paths.iter().collect();
    paths.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let mut hasher = Sha256::new();
    for path in paths {
        let file =
            zip::below(folder, path).ok_or_else(|| Ended::Failed(format!("{path} is not a path below a folder")))?;
        hasher.update(format!("{}  {path}\n", checksum(&file, stop)?).as_bytes());
    }
    Ok(hex(&hasher.finalize()))
}

/// Who a file is fetched from: the address without the way to it.
fn host(url: &str) -> &str {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or(url)
}

/// What `part.json` in a part's folder holds: what of it was fetched,
/// checked and kept.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Kept {
    /// The pins of the fetched files that are done with.
    done: Vec<String>,
    /// What they left in the folder: each file's path and size.
    files: Vec<(String, u64)>,
}

/// The file in a part's folder that says what of it is here.
const KEPT: &str = "part.json";
/// How many of a part's files are looked at when all the parts are
/// listed: a set of samples is thousands, and a list is asked for often.
const LOOKED_AT: usize = 16;
/// Room left on a disk beyond what a part takes.
const MARGIN: u64 = 256 << 20;

fn kept(folder: &Path, every: bool) -> Kept {
    let kept: Kept = std::fs::read_to_string(folder.join(KEPT))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let looked_at = if every { kept.files.len() } else { LOOKED_AT };
    let whole = kept.files.iter().take(looked_at).all(|(path, size)| {
        zip::below(folder, path)
            .and_then(|file| std::fs::metadata(file).ok())
            .is_some_and(|metadata| metadata.is_file() && metadata.len() == *size)
    });
    if whole { kept } else { Kept::default() }
}

/// Whether a folder is one a part can be fetched into or removed from:
/// not there, empty, or holding what a fetch left. Never a folder of the
/// user's own that happens to have the name.
fn ours(folder: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else { return true };
    let names: Vec<String> =
        entries.filter_map(Result::ok).map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.is_empty() || names.iter().any(|name| name == KEPT || name.ends_with(".partial"))
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
    /// An archive is being unpacked.
    Unpacking,
    Ready,
}

/// One part as it is now.
#[derive(Clone, Debug, PartialEq)]
pub struct PartStatus {
    pub part: Part,
    /// Where it is kept, or would be.
    pub folder: PathBuf,
    pub state: PartState,
    /// How many bytes of what is fetched for it have come.
    pub got: u64,
    /// The room it takes on disk now.
    pub on_disk: u64,
    /// Why it was not fetched, the last time it was tried.
    pub error: Option<String>,
}

/// One list as it is now.
#[derive(Clone, Debug, PartialEq)]
pub struct ListStatus {
    pub id: String,
    pub title: String,
    pub about: String,
    pub folder: PathBuf,
    pub parts: Vec<PartStatus>,
}

/// A fetch that is going on, or waiting its turn.
struct Going {
    stop: AtomicBool,
    at: Mutex<(PartState, u64)>,
}

/// Everything that can be fetched, and what is being.
pub struct Fetches {
    fetcher: Mutex<Arc<dyn Fetcher>>,
    lists: Mutex<Vec<List>>,
    going: Mutex<HashMap<String, Arc<Going>>>,
    failed: Mutex<HashMap<String, String>>,
    /// One part is fetched at a time.
    turn: Mutex<()>,
}

impl Fetches {
    /// An engine with no lists yet, fetching by `fetcher`.
    pub fn new(fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            fetcher: Mutex::new(fetcher),
            lists: Mutex::new(Vec::new()),
            going: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashMap::new()),
            turn: Mutex::new(()),
        }
    }

    /// Has `fetcher` fetch from now on. An engine is made fetching nothing
    /// ([`Nothing`]) wherever a test could be the one to make it, and is
    /// handed the system's `curl` by a program that is really one.
    pub fn fetch_by(&self, fetcher: Arc<dyn Fetcher>) {
        *self.fetcher.lock() = fetcher;
    }

    /// Adds a list. Refused when a part of it cannot be fetched by these
    /// rules: an id that is not a plain folder's name or that another part
    /// has, a file that is pinned to nothing, an address that is not
    /// `https`.
    pub fn add(&self, list: List) -> Result<(), String> {
        let mut lists = self.lists.lock();
        if lists.iter().any(|known| known.id == list.id) {
            return Err(format!("there is a list '{}' already", list.id));
        }
        for part in &list.parts {
            let plain =
                !part.id.is_empty() && part.id.chars().all(|letter| letter.is_ascii_alphanumeric() || letter == '-');
            let taken = lists.iter().flat_map(|list| &list.parts).any(|known| known.id == part.id)
                || list.parts.iter().filter(|other| other.id == part.id).count() > 1;
            if !plain || taken {
                return Err(format!(
                    "'{}' cannot be a part's id: letters, digits and dashes, and no other part's",
                    part.id
                ));
            }
            if part.licence.trim().is_empty() || part.from.trim().is_empty() {
                return Err(format!("{} does not say whose it is: a part has a licence line and a source", part.id));
            }
            if part.files.is_empty() {
                return Err(format!("{} has nothing to fetch", part.id));
            }
            for file in &part.files {
                let pinned = file.pin().len() == 64 && file.pin().chars().all(|letter| letter.is_ascii_hexdigit());
                if !pinned || file.size == 0 {
                    return Err(format!("{} of {} is not pinned to a size and a checksum", file.url, part.id));
                }
                if !file.url.starts_with("https://") || file.url.contains(char::is_whitespace) {
                    return Err(format!("{} of {} is not an https address", file.url, part.id));
                }
                if let Keep::As(path) = &file.keep
                    && (zip::below(Path::new("part"), path).is_none() || path == KEPT)
                {
                    return Err(format!("{path} of {} is not a path below the part's folder", part.id));
                }
            }
        }
        lists.push(list);
        Ok(())
    }

    /// The ids of the lists there are.
    pub fn list_ids(&self) -> Vec<String> {
        self.lists.lock().iter().map(|list| list.id.clone()).collect()
    }

    /// Where the list `id` keeps its parts.
    pub fn folder_of(&self, id: &str) -> Option<PathBuf> {
        self.lists.lock().iter().find(|list| list.id == id).map(|list| list.folder.clone())
    }

    /// Keeps the parts of the list `id` in `folder` from now on. What is
    /// in the folder they were in stays there: whoever moves a list moves
    /// that.
    pub fn set_folder(&self, id: &str, folder: PathBuf) -> Result<(), String> {
        let mut lists = self.lists.lock();
        let known: Vec<String> = lists.iter().map(|list| list.id.clone()).collect();
        let Some(list) = lists.iter_mut().find(|list| list.id == id) else {
            return Err(format!("there is no list '{id}'. Lists: {}", known.join(", ")));
        };
        let going = self.going.lock();
        if list.parts.iter().any(|part| going.contains_key(&part.id)) {
            return Err(format!(
                "a part of {} is being fetched: stop it, or wait for it, before they are kept elsewhere",
                list.title
            ));
        }
        list.folder = folder;
        Ok(())
    }

    /// The part `id` with its folder and its list's hook, or a sentence
    /// that names the parts there are.
    fn find(&self, id: &str) -> Result<(Part, PathBuf, Option<Hook>), String> {
        let lists = self.lists.lock();
        for list in lists.iter() {
            if let Some(part) = list.parts.iter().find(|part| part.id == id) {
                return Ok((part.clone(), list.folder.join(&part.id), list.changed.clone()));
            }
        }
        let known: Vec<&str> = lists.iter().flat_map(|list| &list.parts).map(|part| part.id.as_str()).collect();
        let more = known.len().saturating_sub(24);
        let mut names = known.iter().take(24).copied().collect::<Vec<_>>().join(", ");
        if more > 0 {
            names.push_str(&format!(" and {more} more"));
        }
        Err(format!("there is no part '{id}'. Parts: {names}"))
    }

    /// The part `id`, or a sentence that names the ones there are.
    pub fn part(&self, id: &str) -> Result<Part, String> {
        self.find(id).map(|(part, ..)| part)
    }

    /// Where the part `id` is kept, or would be.
    pub fn part_folder(&self, id: &str) -> Option<PathBuf> {
        self.find(id).ok().map(|(_, folder, _)| folder)
    }

    /// Whether all of the part `id` is here, as it was pinned: every file
    /// it left is looked at.
    pub fn ready(&self, id: &str) -> bool {
        self.find(id).is_ok_and(|(part, folder, _)| {
            let kept = kept(&folder, true);
            part.files.iter().all(|file| kept.done.iter().any(|done| done == file.pin()))
        })
    }

    /// Every list with its parts as they are now.
    pub fn status(&self) -> Vec<ListStatus> {
        let lists = self.lists.lock().clone();
        let going = self.going.lock().clone();
        let failed = self.failed.lock().clone();
        lists
            .into_iter()
            .map(|list| {
                let parts = list
                    .parts
                    .iter()
                    .map(|part| {
                        let folder = list.folder.join(&part.id);
                        let kept = kept(&folder, false);
                        let done = |file: &Fetched| kept.done.iter().any(|done| done == file.pin());
                        // What has come of the files that are not done with.
                        let coming = |file: &Fetched| length(&partial_of(&folder.join(file.name()))).min(file.size);
                        let here: u64 =
                            part.files.iter().map(|file| if done(file) { file.size } else { coming(file) }).sum();
                        let on_disk = kept.files.iter().map(|(_, size)| size).sum::<u64>()
                            + part.files.iter().filter(|file| !done(file)).map(coming).sum::<u64>();
                        let (state, got) = match going.get(&part.id) {
                            Some(going) => *going.at.lock(),
                            None if part.files.iter().all(done) => (PartState::Ready, part.size()),
                            None => (PartState::Missing, here),
                        };
                        PartStatus {
                            part: part.clone(),
                            folder,
                            state,
                            got,
                            on_disk,
                            error: failed.get(&part.id).cloned(),
                        }
                    })
                    .collect();
                ListStatus { id: list.id, title: list.title, about: list.about, folder: list.folder, parts }
            })
            .collect()
    }

    /// The part `id` as it is now.
    pub fn status_of(&self, id: &str) -> Option<PartStatus> {
        self.status().into_iter().flat_map(|list| list.parts).find(|status| status.part.id == id)
    }

    /// Fetches the part `id` now, and answers when it is here or the fetch
    /// has ended otherwise. `said` hears where it is and how many bytes
    /// have come.
    pub fn fetch_now(&self, id: &str, stop: &AtomicBool, said: &mut dyn FnMut(PartState, u64)) -> Result<(), Ended> {
        let (part, folder, hook) = self.find(id).map_err(Ended::Failed)?;
        let io = |error: std::io::Error| Ended::Failed(format!("{}: {error}", folder.display()));
        if !ours(&folder) {
            return Err(Ended::Failed(format!(
                "there is a folder at {} already that Trill did not make: move it, or keep these elsewhere, and fetch again",
                folder.display()
            )));
        }
        std::fs::create_dir_all(&folder).map_err(io)?;
        let mut kept = kept(&folder, true);
        let mut before = 0u64;
        for file in &part.files {
            if kept.done.iter().any(|done| done == file.pin()) {
                before += file.size;
                continue;
            }
            let name = file.name();
            let from = host(&file.url);
            let Some(target) = zip::below(&folder, &name) else {
                return Err(Ended::Failed(format!("{name} is not a path below the part's folder")));
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            let partial = partial_of(&target);
            if length(&partial) > file.size {
                let _ = std::fs::remove_file(&partial);
            }
            let have = length(&partial);
            let wanted = file.size - have + MARGIN;
            if let Some(free) = disk_free(&folder).filter(|&free| free < wanted) {
                return Err(Ended::Failed(format!(
                    "there is not the room on the disk for {}: {} are free where {} is kept, and {} are wanted. Make room, or keep it on another disk",
                    part.name,
                    whole::said(free),
                    folder.display(),
                    whole::said(wanted),
                )));
            }
            if have < file.size {
                said(PartState::Fetching, before + have);
                let fetcher = self.fetcher.lock().clone();
                fetcher
                    .fetch(&file.url, &partial, stop, &mut |got| said(PartState::Fetching, before + got.min(file.size)))
                    .map_err(|ended| match ended {
                        Ended::Stopped => Ended::Stopped,
                        Ended::Failed(why) => Ended::Failed(format!(
                            "{name} could not be fetched from {from}: {why}. What came of it is kept, and fetching it again goes on from there"
                        )),
                    })?;
            }
            said(PartState::Checking, before + file.size);
            let not_it = |sum: &str, pin: &str| {
                Ended::Failed(format!(
                    "{name} from {from} is not the file Trill was made for: it was replaced where it is published, or damaged on the way (its checksum begins {}, where {} was expected). It was not kept",
                    &sum[..12.min(sum.len())],
                    &pin[..12.min(pin.len())],
                ))
            };
            if !file.sha256.is_empty() {
                let sum = checksum(&partial, stop)?;
                if sum != file.sha256 {
                    let _ = std::fs::remove_file(&partial);
                    return Err(not_it(&sum, &file.sha256));
                }
            } else if length(&partial) != file.size {
                let _ = std::fs::remove_file(&partial);
                return Err(not_it("another size", "the size it was"));
            }
            let mut unpack = |wanted: &dyn Fn(&str) -> Option<String>| {
                said(PartState::Unpacking, before + file.size);
                zip::unpack(&partial, &folder, wanted, stop, &mut |_| {})
            };
            let left: Vec<(String, u64)> = match &file.keep {
                Keep::As(_) => {
                    std::fs::rename(&partial, &target).map_err(io)?;
                    vec![(name.clone(), file.size)]
                }
                Keep::Members(members) => {
                    let written =
                        unpack(&|member| members.iter().find(|(from, _)| from == member).map(|(_, to)| to.clone()))?;
                    if let Some((lacking, _)) =
                        members.iter().find(|(_, to)| !written.iter().any(|(path, _)| path == to))
                    {
                        return Err(Ended::Failed(format!(
                            "{lacking} is not in {name} from {from}, as it was when Trill was made"
                        )));
                    }
                    written
                }
                Keep::Under { folder: under, ending } => {
                    let written = unpack(&|member| {
                        member
                            .strip_prefix(under.as_str())
                            .filter(|rest| rest.ends_with(ending.as_str()) && !rest.contains('/'))
                            .map(str::to_owned)
                    })?;
                    if written.is_empty() {
                        return Err(Ended::Failed(format!(
                            "nothing of {under} is in {name} from {from}, as it was when Trill was made"
                        )));
                    }
                    written
                }
                Keep::Tree { under, sum } => {
                    let written = unpack(&|member| {
                        member
                            .strip_prefix(under.as_str())
                            .filter(|rest| !rest.is_empty() && *rest != KEPT)
                            .map(str::to_owned)
                    })?;
                    if written.is_empty() {
                        return Err(Ended::Failed(format!(
                            "nothing of {under} is in {name} from {from}, as it was when Trill was made"
                        )));
                    }
                    if !sum.is_empty() {
                        said(PartState::Checking, before + file.size);
                        let paths: Vec<String> = written.iter().map(|(path, _)| path.clone()).collect();
                        let found = tree_checksum(&folder, &paths, stop)?;
                        if found != *sum {
                            for path in &paths {
                                let _ = zip::below(&folder, path).map(std::fs::remove_file);
                            }
                            let _ = std::fs::remove_file(&partial);
                            return Err(not_it(&found, sum));
                        }
                    }
                    written
                }
            };
            if !matches!(file.keep, Keep::As(_)) {
                let _ = std::fs::remove_file(&partial);
            }
            kept.done.push(file.pin().to_owned());
            for (path, size) in left {
                kept.files.retain(|(known, _)| *known != path);
                kept.files.push((path, size));
            }
            write_json(&folder.join(KEPT), &kept).map_err(Ended::Failed)?;
            before += file.size;
        }
        said(PartState::Ready, before);
        if let Some(hook) = hook {
            hook(Change::Arrived, &part, &folder);
        }
        Ok(())
    }

    /// Starts fetching the part `id` and answers at once:
    /// [`Fetches::status`] says how it goes. Parts are fetched one at a
    /// time, in the order they were asked for.
    pub fn fetch(self: &Arc<Self>, id: &str) -> Result<Part, String> {
        let part = self.part(id)?;
        if self.ready(id) {
            return Err(format!("{} is here already", part.name));
        }
        let going = Arc::new(Going { stop: AtomicBool::new(false), at: Mutex::new((PartState::Waiting, 0)) });
        {
            let mut all = self.going.lock();
            if all.contains_key(&part.id) {
                return Err(format!("{} is being fetched already", part.name));
            }
            all.insert(part.id.clone(), going.clone());
        }
        self.failed.lock().remove(&part.id);
        let (engine, id, name) = (self.clone(), part.id.clone(), part.name.clone());
        let spawned = std::thread::Builder::new().name("trill-fetch".into()).spawn(move || {
            let ended = {
                let _turn = engine.turn.lock();
                if going.stop.load(Ordering::Relaxed) {
                    Err(Ended::Stopped)
                } else {
                    engine.fetch_now(&id, &going.stop, &mut |state, got| *going.at.lock() = (state, got))
                }
            };
            if let Err(Ended::Failed(why)) = ended {
                log::warn!("{name}: {why}");
                engine.failed.lock().insert(id.clone(), why);
            }
            engine.going.lock().remove(&id);
        });
        if let Err(error) = spawned {
            self.going.lock().remove(&part.id);
            return Err(format!("the thread that fetches could not be started: {error}"));
        }
        Ok(part)
    }

    /// Stops the fetch of the part `id`. What has come of it is kept.
    pub fn stop(&self, id: &str) -> Result<Part, String> {
        let part = self.part(id)?;
        match self.going.lock().get(&part.id) {
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
        let (part, folder, hook) = self.find(id)?;
        if self.going.lock().contains_key(&part.id) {
            return Err(format!("{} is being fetched: stop that first", part.name));
        }
        if !ours(&folder) {
            return Err(format!("the folder at {} is not one Trill made, and is left alone", folder.display()));
        }
        let freed = folder_size(&folder);
        if folder.is_dir() {
            std::fs::remove_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
        }
        self.failed.lock().remove(&part.id);
        if let Some(hook) = hook {
            hook(Change::Removed, &part, &folder);
        }
        Ok(freed)
    }
}

/// The room a folder takes on disk, in bytes.
pub fn folder_size(folder: &Path) -> u64 {
    std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| match entry.metadata() {
                    Ok(metadata) if metadata.is_dir() => folder_size(&entry.path()),
                    Ok(metadata) => metadata.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::scripted::{Shelf, part};
    use super::*;

    fn engine(folder: &Path, parts: Vec<Part>, shelf: Arc<Shelf>, changed: Option<Hook>) -> Arc<Fetches> {
        let engine = Fetches::new(shelf);
        engine
            .add(List {
                id: "models".into(),
                title: "Models".into(),
                about: "Made up for a test.".into(),
                folder: folder.to_owned(),
                parts,
                changed,
            })
            .unwrap();
        Arc::new(engine)
    }

    fn wait_for(engine: &Fetches, id: &str, state: PartState) -> PartStatus {
        for _ in 0..600 {
            let status = engine.status_of(id).unwrap();
            if status.state == state && (state != PartState::Missing || !engine.fetching()) {
                return status;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{id} never came to {state:?}");
    }

    fn quiet() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn a_part_is_fetched_checked_and_kept_and_removed_again() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let model = part(
            &shelf,
            "beats",
            "Beats",
            &[("beat.onnx", vec![7u8; 300_000]), ("tokens/tokens.txt", b"a 1\nb 2\n".to_vec())],
        );
        let heard: Arc<Mutex<Vec<(Change, String, PathBuf)>>> = Arc::default();
        let hears = heard.clone();
        let hook: Hook =
            Arc::new(move |change, part, folder| hears.lock().push((change, part.id.clone(), folder.to_owned())));
        let engine = engine(folder.path(), vec![model], shelf.clone(), Some(hook));
        let before = engine.status_of("beats").unwrap();
        assert_eq!((before.state, before.got, before.on_disk), (PartState::Missing, 0, 0));
        assert_eq!(before.folder, folder.path().join("beats"));
        assert!(!engine.ready("beats"));

        assert_eq!(engine.fetch("beats").unwrap().name, "Beats");
        let ready = wait_for(&engine, "beats", PartState::Ready);
        assert_eq!((ready.got, ready.on_disk, &ready.error), (300_008, 300_008, &None));
        assert!(engine.ready("beats"));
        // A file may be kept in a folder of the part's.
        assert_eq!(
            std::fs::read(folder.path().join("beats").join("tokens").join("tokens.txt")).unwrap(),
            b"a 1\nb 2\n"
        );
        // Nothing is left under the name it was written by.
        assert!(!folder.path().join("beats").join("beat.onnx.partial").exists());
        // Whoever listed it heard that it arrived, and where.
        assert_eq!(*heard.lock(), [(Change::Arrived, "beats".to_owned(), folder.path().join("beats"))]);
        // What is here is not fetched again.
        assert_eq!(engine.fetch("beats").unwrap_err(), "Beats is here already");
        assert_eq!(shelf.asked().len(), 2);

        // A file that went missing makes the part missing, and all of it is fetched again.
        std::fs::remove_file(folder.path().join("beats").join("beat.onnx")).unwrap();
        assert!(!engine.ready("beats"));
        engine.fetch_now("beats", &quiet(), &mut |_, _| {}).unwrap();
        assert!(engine.ready("beats"));

        assert!(engine.remove("beats").unwrap() >= 300_008);
        assert!(!engine.ready("beats") && !folder.path().join("beats").exists());
        assert_eq!(heard.lock().last().unwrap().0, Change::Removed);
        assert_eq!(engine.part("nope").unwrap_err(), "there is no part 'nope'. Parts: beats");
    }

    #[test]
    fn a_file_that_is_not_the_one_pinned_is_refused_and_not_kept() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let model = part(&shelf, "stems", "Stems", &[("stems.onnx", vec![3u8; 50_000])]);
        // Replaced where it is published.
        shelf.put(&model.files[0].url, vec![4u8; 50_000]);
        let engine = engine(folder.path(), vec![model], shelf, None);
        let Err(Ended::Failed(why)) = engine.fetch_now("stems", &quiet(), &mut |_, _| {}) else { panic!() };
        assert!(
            why.starts_with(
                "stems.onnx from shelf.test is not the file Trill was made for: it was replaced where it is published"
            ) && why.ends_with("It was not kept"),
            "{why}"
        );
        assert!(!engine.ready("stems"));
        let left: Vec<_> = std::fs::read_dir(folder.path().join("stems")).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn a_fetch_that_fails_or_is_stopped_is_gone_on_with_from_where_it_was() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let bytes: Vec<u8> = (0..400_000u32).map(|index| (index % 251) as u8).collect();
        let model = part(&shelf, "words", "Words", &[("words.onnx", bytes.clone())]);
        let engine = engine(folder.path(), vec![model], shelf.clone(), None);

        // The connection is lost after a part of it.
        shelf.cut_after(150_000);
        engine.fetch("words").unwrap();
        let failed = wait_for(&engine, "words", PartState::Missing);
        let why = failed.error.clone().unwrap();
        assert!(
            why.starts_with("words.onnx could not be fetched from shelf.test: the connection was lost")
                && why.ends_with("fetching it again goes on from there"),
            "{why}"
        );
        assert!(failed.got >= 150_000 && failed.got < 400_000, "{failed:?}");

        // Fetched again, it is stopped part of the way, and what came is still kept.
        shelf.slow(Duration::from_millis(15));
        engine.fetch("words").unwrap();
        for _ in 0..500 {
            if engine.status_of("words").unwrap().got > failed.got + 60_000 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(engine.fetch("words").unwrap_err(), "Words is being fetched already");
        assert_eq!(engine.remove("words").unwrap_err(), "Words is being fetched: stop that first");
        assert!(engine.set_folder("models", folder.path().join("elsewhere")).unwrap_err().contains("is being fetched"));
        engine.stop("words").unwrap();
        let stopped = wait_for(&engine, "words", PartState::Missing);
        assert!(stopped.error.is_none() && stopped.got > failed.got && stopped.got < 400_000, "{stopped:?}");
        assert_eq!(engine.stop("words").unwrap_err(), "Words is not being fetched");

        // And the third time it is gone on with to the end, and is the file.
        shelf.slow(Duration::ZERO);
        engine.fetch("words").unwrap();
        wait_for(&engine, "words", PartState::Ready);
        assert_eq!(std::fs::read(folder.path().join("words").join("words.onnx")).unwrap(), bytes);
        // Each fetch began where the one before it left off.
        let began = shelf.began();
        assert_eq!(began[0], 0);
        assert!(began[1] >= 150_000 && began[2] > began[1], "{began:?}");
    }

    #[test]
    fn what_a_part_wants_is_taken_out_of_its_archives() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let library = vec![9u8; 20_000];
        let zipped = zip::archive_of(&[
            ("runtime-1.0/lib/runtime.dll", &library, true),
            ("runtime-1.0/lib/runtime.lib", &[1u8; 900], true),
            ("runtime-1.0/README", b"read me", false),
        ]);
        let wheel = zip::archive_of(&[
            ("maker/lib/bin/one.dll", &[1u8; 700], true),
            ("maker/lib/bin/two.dll", &[2u8; 800], false),
            ("maker/lib/bin/deeper/three.dll", &[3u8; 100], false),
            ("maker/lib/include/one.h", b"header", false),
        ]);
        let mut runtime = part(&shelf, "runtime", "The runtime", &[("runtime-1.0.zip", zipped), ("maker.whl", wheel)]);
        runtime.files[0].keep = Keep::Members(vec![("runtime-1.0/lib/runtime.dll".into(), "runtime.dll".into())]);
        runtime.files[1].keep = Keep::Under { folder: "maker/lib/bin/".into(), ending: ".dll".into() };
        let engine = engine(folder.path(), vec![runtime], shelf, None);
        let mut states = Vec::new();
        engine
            .fetch_now("runtime", &quiet(), &mut |state, _| {
                if states.last() != Some(&state) {
                    states.push(state);
                }
            })
            .unwrap();
        assert!(states.contains(&PartState::Unpacking) && states.last() == Some(&PartState::Ready), "{states:?}");
        let mut names: Vec<String> = std::fs::read_dir(folder.path().join("runtime"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        // The archives themselves are not kept.
        assert_eq!(names, ["one.dll", "part.json", "runtime.dll", "two.dll"]);
        assert_eq!(std::fs::read(folder.path().join("runtime").join("runtime.dll")).unwrap(), library);
        assert!(engine.ready("runtime"));
    }

    #[test]
    fn an_archive_that_is_made_anew_each_time_is_pinned_by_what_it_unpacks_to() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let members: [(&str, &[u8], bool); 4] = [
            ("piano-abc123/Piano.sfz", b"<region> sample=samples/C4.flac", true),
            ("piano-abc123/samples/C4.flac", &[4u8; 4000], false),
            ("piano-abc123/samples/A4.flac", &[5u8; 5000], true),
            ("another-thing/readme.txt", b"not wanted", false),
        ];
        let archive = zip::archive_of(&members);
        // What it unpacks to, worked out the way anyone can: a line for each file, in the order of their paths.
        let lines: String =
            [("Piano.sfz", members[0].1), ("samples/A4.flac", members[2].1), ("samples/C4.flac", members[1].1)]
                .iter()
                .map(|(path, bytes)| format!("{}  {path}\n", hex(&Sha256::digest(bytes))))
                .collect();
        let sum = hex(&Sha256::digest(lines.as_bytes()));
        let mut piano = part(&shelf, "piano", "A piano", &[("piano-abc123.zip", archive.clone())]);
        piano.credit = "Samples by Somebody, who asks to be named.".into();
        // Pinned by what it unpacks to, and not by its own bytes.
        piano.files[0].sha256 = String::new();
        piano.files[0].keep = Keep::Tree { under: "piano-abc123/".into(), sum: sum.clone() };
        let engine = engine(folder.path(), vec![piano.clone()], shelf.clone(), None);
        engine.fetch_now("piano", &quiet(), &mut |_, _| {}).unwrap();
        let kept = folder.path().join("piano");
        assert_eq!(std::fs::read(kept.join("samples").join("A4.flac")).unwrap(), [5u8; 5000]);
        assert!(kept.join("Piano.sfz").is_file() && !kept.join("readme.txt").exists());
        assert!(!kept.join("piano-abc123.zip.partial").exists());
        let status = engine.status_of("piano").unwrap();
        assert_eq!((status.state, status.on_disk), (PartState::Ready, 9000 + members[0].1.len() as u64));
        assert_eq!(status.part.credit, "Samples by Somebody, who asks to be named.");

        // The same archive packed another way is the same part; one with another sample in it is not.
        engine.remove("piano").unwrap();
        let repacked =
            zip::archive_of(&[members[2], (members[1].0, members[1].1, true), (members[0].0, members[0].1, false)]);
        piano.files[0].size = repacked.len() as u64;
        shelf.put(&piano.files[0].url, repacked);
        let again = self::engine(folder.path(), vec![piano.clone()], shelf.clone(), None);
        again.fetch_now("piano", &quiet(), &mut |_, _| {}).unwrap();
        again.remove("piano").unwrap();
        let other = zip::archive_of(&[members[0], members[1], ("piano-abc123/samples/A4.flac", &[6u8; 5000], true)]);
        piano.files[0].size = other.len() as u64;
        shelf.put(&piano.files[0].url, other);
        let changed = self::engine(folder.path(), vec![piano], shelf, None);
        let Err(Ended::Failed(why)) = changed.fetch_now("piano", &quiet(), &mut |_, _| {}) else { panic!() };
        assert!(why.starts_with("piano-abc123.zip from shelf.test is not the file Trill was made for"), "{why}");
        // And nothing of it is left.
        assert!(!kept.join("Piano.sfz").exists() && !changed.ready("piano"));
    }

    #[test]
    fn a_list_is_refused_that_breaks_the_rules_and_a_folder_of_the_users_is_left_alone() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let good = part(&shelf, "one", "One", &[("a.bin", vec![1u8; 10])]);
        let engine = engine(folder.path(), vec![good.clone()], shelf.clone(), None);
        let list = |id: &str, parts: Vec<Part>| List {
            id: id.into(),
            title: "Instruments".into(),
            about: String::new(),
            folder: folder.path().join("instruments"),
            parts,
            changed: None,
        };
        assert_eq!(engine.add(list("models", vec![])).unwrap_err(), "there is a list 'models' already");
        // No other part's id, and nothing that is not a folder's plain name.
        let refused = engine.add(list("more", vec![good.clone()])).unwrap_err();
        assert!(refused.starts_with("'one' cannot be a part's id"), "{refused}");
        let named = |id: &str| Part { id: id.into(), ..good.clone() };
        assert!(engine.add(list("more", vec![named("../up")])).unwrap_err().contains("cannot be a part's id"));
        // Pinned, from an https address, and saying whose it is.
        let mut unpinned = named("two");
        unpinned.files[0].sha256 = String::new();
        assert!(
            engine.add(list("more", vec![unpinned])).unwrap_err().ends_with("is not pinned to a size and a checksum")
        );
        let mut plain = named("two");
        plain.files[0].url = "http://shelf.test/a.bin".into();
        assert!(engine.add(list("more", vec![plain])).unwrap_err().ends_with("is not an https address"));
        let nobody = Part { licence: " ".into(), ..named("two") };
        assert!(engine.add(list("more", vec![nobody])).unwrap_err().contains("does not say whose it is"));
        let mut outside = named("two");
        outside.files[0].keep = Keep::As("../a.bin".into());
        assert!(engine.add(list("more", vec![outside])).unwrap_err().contains("is not a path below the part's folder"));

        // A second list is kept where its owner says, and moved when the user chooses.
        engine.add(list("more", vec![named("two")])).unwrap();
        assert_eq!(engine.list_ids(), ["models", "more"]);
        let chosen = folder.path().join("chosen");
        engine.set_folder("more", chosen.clone()).unwrap();
        assert_eq!(engine.folder_of("more"), Some(chosen.clone()));
        assert_eq!(
            engine.set_folder("nope", chosen.clone()).unwrap_err(),
            "there is no list 'nope'. Lists: models, more"
        );
        // A folder of that name that is the user's own is neither fetched into nor removed.
        std::fs::create_dir_all(chosen.join("two")).unwrap();
        std::fs::write(chosen.join("two").join("my own.wav"), b"mine").unwrap();
        let Err(Ended::Failed(theirs)) = engine.fetch_now("two", &quiet(), &mut |_, _| {}) else { panic!() };
        assert!(theirs.contains("already that Trill did not make"), "{theirs}");
        assert!(engine.remove("two").unwrap_err().ends_with("is not one Trill made, and is left alone"));
        assert!(chosen.join("two").join("my own.wav").is_file());
        assert_eq!(engine.status().len(), 2);
    }

    #[test]
    fn a_fetcher_for_a_run_that_touches_nothing_fetches_nothing() {
        let folder = tempfile::tempdir().unwrap();
        let ended = Nothing.fetch("https://example.org/a", &folder.path().join("a"), &quiet(), &mut |_| {});
        assert!(matches!(ended, Err(Ended::Failed(why)) if why.ends_with("fetches nothing")));
        assert_eq!(host("https://huggingface.co/a/b/resolve/c/d.onnx"), "huggingface.co");
    }
}
