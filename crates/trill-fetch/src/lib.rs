//! What Trill fetches when its user asks: *parts* that others publish
//! (the models a study runs, their runtime, sets of sampled instruments),
//! none of which Trill ships or hosts.
//!
//! The rules are the same for every part, whoever lists it:
//!
//! - **Nothing is fetched unasked.** A part comes when someone asks for
//!   that part by name ([`Fetches::fetch`], or several at once with
//!   [`Fetches::fetch_many`]), having been shown its size, its licence in
//!   the words of its source, who is to be credited and where it comes
//!   from ([`Part`]). A test never fetches: it hands the engine a shelf of
//!   its own ([`scripted`]).
//! - **By the system's own `curl`** ([`Curl`]), found when something is
//!   fetched: Trill brings no way of its own to talk to a network, and
//!   sends nothing but the request for the file.
//! - **A file is what it was pinned to be, or it is not kept.** Each is
//!   written under another name, checked against its checksum when it is
//!   whole, and only then given its own name. An archive is checked as it
//!   was fetched, or by what it unpacks to ([`Keep::Tree`]), or both.
//! - **A fetch can be stopped, and is gone on with.** What had come of a
//!   file when a fetch was stopped or failed is kept, and the next fetch
//!   begins there (where the place it comes from can begin in the middle;
//!   an archive that is made when it is asked for is begun again).
//! - **The room is asked for first.** Before a part is fetched the disk is
//!   asked whether it has the room for all of it: the files, and what an
//!   archive unpacks to beside the archive itself.
//! - **A failure is a sentence**: what went wrong, and what to do.
//!
//! # Handing it a list of your own
//!
//! A crate that has things to fetch makes a [`List`] of [`Part`]s and
//! adds it ([`Fetches::add`]): the list's `folder` is where its parts are
//! kept (each in a folder named by its id), which the user may choose
//! ([`Fetches::set_folder`]); its `changed` is called when a part has
//! arrived or was removed, with the part's folder. A part's files are
//! [`Fetched`] in their order; `examples/pin.rs` prints the size and
//! checksums of a file or an archive, to write one down by. Both front
//! ends then show the list and fetch from it with no more said: the
//! window's settings, and the `setup` tool.
//!
//! What a part of many files needs is here too. A part may be thousands
//! of small files: they are fetched several at once over connections that
//! are kept open ([`Fetcher::fetch_many`]), and the part's record is
//! written a batch at a time, so that a fetch that is stopped has kept
//! what it had. A part may be an archive its source makes when it is
//! asked for, whose bytes and size nobody can pin: its size is then an
//! estimate ([`Fetched::sized_by_guess`]) and what it unpacks to decides.
//! A part may be published in a second place ([`Part::elsewhere`]), which
//! is tried when the first could not give it.

pub mod scripted;
pub mod whole;
pub mod zip;

pub use whole::{disk_free, find_program, hide_console, last_line, partial_of, said, write_json, write_whole};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};
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
    /// Its size as it is fetched, in bytes. An estimate where the file is
    /// pinned by what it unpacks to and by nothing else
    /// ([`Fetched::sized_by_guess`]).
    pub size: u64,
    pub keep: Keep,
    /// What an archive unpacks to, in bytes, where that is known: the
    /// disk is asked for that room, beside the archive's own, before the
    /// archive is fetched. Nothing (0) where it is not known, or for a
    /// file that is kept as it comes.
    pub unpacked: u64,
}

/// The letters an address may hold as they are; every other byte of a
/// path is written `%XX`.
fn plain_in_an_address(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// The address of the file at `path` (forward slashes) below `base`: the
/// path with every byte an address cannot hold as it is written `%XX`. A
/// path may have spaces, commas, `#` and letters of any alphabet in it.
pub fn address(base: &str, path: &str) -> String {
    let mut address = String::with_capacity(base.len() + path.len() + 16);
    address.push_str(base);
    for byte in path.bytes() {
        if plain_in_an_address(byte) {
            address.push(byte as char);
        } else {
            address.push_str(&format!("%{byte:02X}"));
        }
    }
    address
}

impl Fetched {
    /// A file kept as it comes, at `path` below the part's folder.
    pub fn file(url: &str, sha256: &str, size: u64, path: &str) -> Self {
        Self { url: url.to_owned(), sha256: sha256.to_owned(), size, keep: Keep::As(path.to_owned()), unpacked: 0 }
    }

    /// The file at `path` below the address `base` (which ends in a
    /// slash), kept at that same path below the part's folder: the path
    /// is written into the address as an address can hold it
    /// ([`address`]).
    pub fn file_below(base: &str, path: &str, sha256: &str, size: u64) -> Self {
        Self::file(&address(base, path), sha256, size, path)
    }

    /// An archive, of which `keep` says what is kept.
    pub fn archive(url: &str, sha256: &str, size: u64, keep: Keep) -> Self {
        Self { url: url.to_owned(), sha256: sha256.to_owned(), size, keep, unpacked: 0 }
    }

    /// The same archive, known to unpack to `bytes`.
    pub fn unpacking_to(mut self, bytes: u64) -> Self {
        self.unpacked = bytes;
        self
    }

    /// Whether its size is an estimate: an archive pinned by what it
    /// unpacks to and not by its own bytes, which its source makes anew
    /// each time it is asked for. However large it comes, what it unpacks
    /// to decides whether it is the one.
    pub fn sized_by_guess(&self) -> bool {
        self.sha256.is_empty() && matches!(&self.keep, Keep::Tree { sum, .. } if !sum.is_empty())
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

    fn is_archive(&self) -> bool {
        !matches!(self.keep, Keep::As(_))
    }

    /// The room on disk that what is kept of it takes, as far as is known.
    fn room(&self) -> u64 {
        if self.is_archive() && self.unpacked > 0 { self.unpacked } else { self.size }
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
    /// Other places the whole of it is published, each as the files it is
    /// fetched as there and pinned on its own: tried in their order when
    /// the place before could not give it. Whatever place it came from,
    /// it is the same part.
    pub elsewhere: Vec<Vec<Fetched>>,
}

impl Part {
    /// What is fetched for it, in bytes: from the first place it is
    /// published. An estimate where a file of it is sized by guess.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// The room it takes on disk once it is here, as far as is known.
    pub fn room(&self) -> u64 {
        self.files.iter().map(Fetched::room).sum()
    }

    /// The ways it can be fetched: its own files, then each other place's.
    fn ways(&self) -> impl Iterator<Item = &[Fetched]> {
        std::iter::once(self.files.as_slice()).chain(self.elsewhere.iter().map(Vec::as_slice))
    }

    /// Whether the files that are `done` are all of it, by any of its ways.
    fn whole(&self, done: &HashSet<String>) -> bool {
        self.ways().any(|files| files.iter().all(|file| done.contains(file.pin())))
    }

    /// The most room the disk must have while it is fetched: what is not
    /// here yet of its files, and what its archives unpack to beside them.
    fn wanted(&self) -> u64 {
        self.files.iter().map(|file| file.size + if file.is_archive() { file.unpacked } else { 0 }).sum()
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

/// A list as the engine holds it: its parts are handed about, not copied.
#[derive(Clone)]
struct Held {
    id: String,
    title: String,
    about: String,
    folder: PathBuf,
    parts: Vec<Arc<Part>>,
    changed: Option<Hook>,
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

    /// Brings several files, each an address and the file it goes into,
    /// and says how many bytes of them all there are as it goes. One
    /// after the other, unless whoever fetches can do better: keep a
    /// connection open for them, or fetch a few at once. `beside` is a
    /// folder on the disk the files go to, in which it may keep what it
    /// needs while it fetches and leaves nothing.
    fn fetch_many(
        &self,
        files: &[(String, PathBuf)],
        beside: &Path,
        stop: &AtomicBool,
        got: &mut dyn FnMut(u64),
    ) -> Result<(), Ended> {
        let _ = beside;
        let mut before = 0u64;
        for (url, to) in files {
            self.fetch(url, to, stop, &mut |bytes| got(before + bytes))?;
            before += length(to);
        }
        Ok(())
    }
}

/// The system's own `curl`, which Windows, macOS and nearly every Linux
/// come with: found when something is fetched, never shipped.
pub struct Curl;

/// How many files `curl` fetches at once where a part is many small ones.
/// Measured with `examples/many.rs` on a set of 140 recordings (95 MB)
/// from a host that serves files one by one: a `curl` for each file, as
/// a part was fetched before, took 40.6 s; one `curl` that keeps its
/// connection open 7.8 s; four at once 3.9 s, eight 3.6 s, sixteen 3.2 s.
/// Keeping the connection is most of it, and past four at once there is
/// little left to gain from asking more of somebody else's server.
pub const AT_ONCE: usize = 4;

fn curl() -> Result<PathBuf, Ended> {
    find_program("curl").ok_or_else(|| {
        Ended::Failed(
            "Trill fetches with the system's own curl, which was not found here: install curl and fetch again"
                .to_owned(),
        )
    })
}

/// Waits for `process`, saying how much has come as it goes, and ends it
/// when `stop` is set.
fn wait_for(
    process: &mut std::process::Child,
    stop: &AtomicBool,
    said: &mut dyn FnMut(),
) -> Result<std::process::ExitStatus, Ended> {
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = process.kill();
            let _ = process.wait();
            return Err(Ended::Stopped);
        }
        said();
        match process.try_wait() {
            Ok(Some(status)) => {
                said();
                return Ok(status);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(Ended::Failed(error.to_string())),
        }
    }
}

/// What a program wrote to its standard error, once it has ended.
fn said_of(process: &mut std::process::Child) -> String {
    let mut said = Vec::new();
    if let Some(mut stderr) = process.stderr.take() {
        let _ = stderr.read_to_end(&mut said);
    }
    last_line(&said)
}

/// How `curl` is told to behave, whatever it fetches: to fail on what a
/// server refuses, to follow where it is sent, to say only what went
/// wrong, and to give up a connection that brings next to nothing for a
/// minute.
const CURL_ALWAYS: [&str; 10] = [
    "--fail",
    "--location",
    "--silent",
    "--show-error",
    "--connect-timeout",
    "30",
    "--speed-limit",
    "1000",
    "--speed-time",
    "60",
];
/// How often a file that was lost on the way is gone on with before the
/// fetch is given up: `curl` cannot begin again by itself what it writes
/// into a pipe.
const TRIES: usize = 3;
/// A path longer than this is not handed to `curl`: on Windows it cannot
/// write to one of more than 260 letters.
const LONG_PATH: usize = 230;

impl Fetcher for Curl {
    /// `curl` writes what it fetches into a pipe, and the file is written
    /// here: so that it is written where the system lets this program
    /// write (a path of any length, with any letters), and so that how
    /// much has come is known without asking the disk.
    fn fetch(&self, url: &str, to: &Path, stop: &AtomicBool, got: &mut dyn FnMut(u64)) -> Result<(), Ended> {
        let program = curl()?;
        let io = |error: std::io::Error| Ended::Failed(format!("{}: {error}", to.display()));
        let mut lost = 0;
        loop {
            let have = length(to);
            let mut command = Command::new(&program);
            command
                .args(CURL_ALWAYS)
                .args(["--continue-at", &have.to_string(), "--output", "-", "--"])
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            hide_console(&mut command);
            let mut process = command.spawn().map_err(|error| {
                Ended::Failed(format!("curl ('{}') could not be started: {error}", program.display()))
            })?;
            let mut out = process.stdout.take().ok_or_else(|| Ended::Failed("curl gave no pipe to read".to_owned()))?;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(to).map_err(io)?;
            let written = std::sync::atomic::AtomicU64::new(have);
            // Read on a thread of its own, so that a fetch is stopped at once and not at the next piece.
            let (status, wrote) = std::thread::scope(|scope| {
                let copying = scope.spawn(|| {
                    let mut buffer = vec![0u8; 1 << 16];
                    loop {
                        match out.read(&mut buffer) {
                            Ok(0) => return Ok(()),
                            Ok(read) => {
                                file.write_all(&buffer[..read])?;
                                written.fetch_add(read as u64, Ordering::Relaxed);
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(error) => return Err(error),
                        }
                    }
                });
                let status = wait_for(&mut process, stop, &mut || got(written.load(Ordering::Relaxed)));
                (status, copying.join().unwrap_or(Ok(())))
            });
            got(length(to));
            let status = status?;
            wrote.map_err(io)?;
            if status.success() {
                return Ok(());
            }
            let said = said_of(&mut process);
            // 33: the server does not go on from the middle. 416: there was more here than there is there.
            if have > 0 && (status.code() == Some(33) || said.contains("416")) {
                let _ = std::fs::remove_file(to);
                continue;
            }
            // Lost on the way, with more come than before: gone on with from there.
            lost += 1;
            let transient = matches!(status.code(), Some(18 | 28 | 52 | 55 | 56 | 92));
            if transient && lost < TRIES && length(to) > have {
                continue;
            }
            return Err(Ended::Failed(said.trim_start_matches("curl: ").to_owned()));
        }
    }

    /// One `curl` for all of them: its connections are kept open from one
    /// file to the next, and [`AT_ONCE`] are fetched at a time. What had
    /// come of one of them before is not gone on with: they are small.
    fn fetch_many(
        &self,
        files: &[(String, PathBuf)],
        beside: &Path,
        stop: &AtomicBool,
        got: &mut dyn FnMut(u64),
    ) -> Result<(), Ended> {
        if let [(url, to)] = files {
            return self.fetch(url, to, stop, got);
        }
        curl_many(AT_ONCE, files, beside, stop, got)
    }
}

/// The folder `curl` writes into while it fetches several files: in the
/// folder the files go to, unless that lies too deep for it, and then
/// where the system keeps what is for the moment.
fn staging(beside: &Path) -> PathBuf {
    let within = beside.join(STAGING);
    if within.as_os_str().len() < LONG_PATH {
        within
    } else {
        std::env::temp_dir().join(format!("trill-fetch-{}", std::process::id()))
    }
}

/// What the folder is called that `curl` writes into while it fetches
/// several files of a part: in the part's folder, and gone when it is done.
const STAGING: &str = ".fetching";

/// Has one `curl` bring `files`, `at_once` of them at a time: what
/// [`Curl`] does with [`AT_ONCE`], and what `examples/many.rs` measures
/// with other counts.
///
/// `curl` writes each under a short name of its own in a folder that is
/// gone when it is done ([`staging`]), and each is then given its place:
/// a file's own path may be longer than `curl` can write to, and may hold
/// what `curl` reads as more than a name (a `#` and a number).
pub fn curl_many(
    at_once: usize,
    files: &[(String, PathBuf)],
    beside: &Path,
    stop: &AtomicBool,
    got: &mut dyn FnMut(u64),
) -> Result<(), Ended> {
    let program = curl()?;
    let staging = staging(beside);
    let io = |error: std::io::Error| Ended::Failed(format!("{}: {error}", staging.display()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(io)?;
    let staged: Vec<PathBuf> = (0..files.len()).map(|index| staging.join(format!("{index:04}"))).collect();
    // What it is told goes in on its standard input, in the words of its
    // own configuration: an address and a file for each. Between quotes a
    // backslash and a quote are written with a backslash.
    let quoted = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    let mut told = format!(
        "fail\nfail-early\nlocation\nsilent\nshow-error\nconnect-timeout = 30\nspeed-limit = 1000\n\
         speed-time = 60\nretry = 2\nparallel\nparallel-max = {}\n",
        at_once.max(1)
    );
    for ((url, to), staged) in files.iter().zip(&staged) {
        let _ = std::fs::remove_file(to);
        told.push_str(&format!("url = \"{}\"\noutput = \"{}\"\n", quoted(url), quoted(&staged.to_string_lossy())));
    }
    let mut command = Command::new(&program);
    command.args(["--config", "-"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    hide_console(&mut command);
    let ended = command
        .spawn()
        .map_err(|error| Ended::Failed(format!("curl ('{}') could not be started: {error}", program.display())))
        .and_then(|mut process| {
            if let Some(mut stdin) = process.stdin.take() {
                // If it has gone already, why is said when it is waited for.
                let _ = stdin.write_all(told.as_bytes());
            }
            let status = wait_for(&mut process, stop, &mut || got(staged.iter().map(|staged| length(staged)).sum()))?;
            if status.success() {
                Ok(())
            } else {
                Err(Ended::Failed(said_of(&mut process).trim_start_matches("curl: ").to_owned()))
            }
        });
    // However it ended, what came is given its place: whoever asked sees which of them are whole.
    for ((_, to), staged) in files.iter().zip(&staged) {
        if staged.is_file() && std::fs::rename(staged, to).is_err() {
            // Another disk: copied, and then removed where it was.
            let _ = std::fs::copy(staged, to).and_then(|_| std::fs::remove_file(staged));
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    ended
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
/// A file smaller than this, kept as it comes, is one of many: such files
/// are fetched several at once, and what had come of one is not gone on
/// with. A larger one is fetched alone and gone on with from the middle.
const SMALL: u64 = 32 << 20;
/// How many small files are fetched and checked before the part's record
/// is written: what a fetch that is stopped has kept is whole batches.
const BATCH: usize = 64;
/// How long what was read of a part's folder is taken to hold, where the
/// folder's record has not changed: a list of a hundred parts is asked
/// for several times a second while one of them is fetched.
const LOOK_HOLDS: Duration = Duration::from_secs(2);

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

/// Whether anything but the record of a part is in its folder, however deep.
fn holds_files(folder: &Path) -> bool {
    fn any(folder: &Path, top: bool) -> bool {
        let Ok(entries) = std::fs::read_dir(folder) else { return false };
        entries.filter_map(Result::ok).any(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => any(&entry.path(), false),
            _ => !(top && entry.file_name() == KEPT),
        })
    }
    any(folder, true)
}

/// Removes whatever is still under the name it was fetched by, however deep.
fn sweep(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.filter_map(Result::ok) {
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => sweep(&entry.path()),
            _ if entry.file_name().to_string_lossy().ends_with(".partial") => {
                let _ = std::fs::remove_file(entry.path());
            }
            _ => {}
        }
    }
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

impl PartState {
    /// Whether it was asked for and is not here yet: waiting, or on its way.
    pub fn is_going(self) -> bool {
        matches!(self, PartState::Waiting | PartState::Fetching | PartState::Checking | PartState::Unpacking)
    }
}

/// One part as it is now.
#[derive(Clone, Debug, PartialEq)]
pub struct PartStatus {
    pub part: Arc<Part>,
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

/// What became of asking for several parts at once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asked {
    /// The parts that are now waiting their turn, in the order they are fetched in.
    pub queued: Vec<String>,
    /// How many of what was asked for are here already.
    pub here: usize,
    /// How many were asked for before and are waiting or on their way.
    pub going: usize,
    /// What is still to be fetched for the ones that were queued, in bytes.
    pub bytes: u64,
}

/// A fetch that is going on, or waiting its turn.
struct Going {
    stop: AtomicBool,
    at: Mutex<(PartState, u64)>,
}

/// What was last read of a part's folder.
struct Seen {
    /// The size and the time of the folder's record when it was read.
    stamp: Option<(u64, SystemTime)>,
    at: Instant,
    whole: bool,
    /// How many bytes of what is fetched for the part are here: the files
    /// that are done with, and what has come of the others.
    here: u64,
    on_disk: u64,
}

/// What was asked for and waits its turn.
#[derive(Default)]
struct Queue {
    waiting: VecDeque<String>,
    /// Whether the thread that fetches is at work.
    working: bool,
}

/// Everything that can be fetched, and what is being.
pub struct Fetches {
    fetcher: Mutex<Arc<dyn Fetcher>>,
    lists: Mutex<Vec<Held>>,
    going: Mutex<HashMap<String, Arc<Going>>>,
    failed: Mutex<HashMap<String, String>>,
    /// One part is fetched at a time, in the order they were asked for.
    queue: Mutex<Queue>,
    /// One part is fetched at a time, whoever fetches it.
    turn: Mutex<()>,
    seen: Mutex<HashMap<PathBuf, Arc<Seen>>>,
}

fn stamp_of(folder: &Path) -> Option<(u64, SystemTime)> {
    let metadata = std::fs::metadata(folder.join(KEPT)).ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
}

/// The room there is not, as a sentence.
fn no_room(name: &str, folder: &Path, free: u64, wanted: u64) -> String {
    format!(
        "there is not the room on the disk for {name}: {} are free where {} is kept, and {} are wanted. Make room, or keep it on another disk",
        whole::said(free),
        folder.display(),
        whole::said(wanted),
    )
}

/// Why a way of fetching a part did not give it.
enum Went {
    Stopped,
    /// Nothing another place could mend: the disk has not the room, the
    /// folder is not Trill's.
    Refused(String),
    /// What another place may still give.
    Failed(String),
}

impl From<Ended> for Went {
    fn from(ended: Ended) -> Self {
        match ended {
            Ended::Stopped => Went::Stopped,
            Ended::Failed(why) => Went::Failed(why),
        }
    }
}

impl Fetches {
    /// An engine with no lists yet, fetching by `fetcher`.
    pub fn new(fetcher: Arc<dyn Fetcher>) -> Self {
        Self {
            fetcher: Mutex::new(fetcher),
            lists: Mutex::new(Vec::new()),
            going: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashMap::new()),
            queue: Mutex::new(Queue::default()),
            turn: Mutex::new(()),
            seen: Mutex::new(HashMap::new()),
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
        let mut ids: HashSet<&str> = lists.iter().flat_map(|list| &list.parts).map(|part| part.id.as_str()).collect();
        for part in &list.parts {
            let plain =
                !part.id.is_empty() && part.id.chars().all(|letter| letter.is_ascii_alphanumeric() || letter == '-');
            if !plain || !ids.insert(&part.id) {
                return Err(format!(
                    "'{}' cannot be a part's id: letters, digits and dashes, and no other part's",
                    part.id
                ));
            }
            if part.licence.trim().is_empty() || part.from.trim().is_empty() {
                return Err(format!("{} does not say whose it is: a part has a licence line and a source", part.id));
            }
            if part.ways().any(<[Fetched]>::is_empty) {
                return Err(format!("{} has nothing to fetch", part.id));
            }
            for file in part.ways().flatten() {
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
        drop(ids);
        lists.push(Held {
            id: list.id,
            title: list.title,
            about: list.about,
            folder: list.folder,
            parts: list.parts.into_iter().map(Arc::new).collect(),
            changed: list.changed,
        });
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
        self.seen.lock().clear();
        Ok(())
    }

    /// Tells the list `id`, whose parts were kept in `from` and were moved
    /// to where it keeps them now, of each part that is here: removed from
    /// where it was, arrived where it is.
    pub fn moved(&self, id: &str, from: &Path) {
        let Some(list) = self.lists.lock().iter().find(|list| list.id == id).cloned() else { return };
        let Some(hook) = &list.changed else { return };
        self.seen.lock().clear();
        for part in list.parts.iter().filter(|part| self.ready(&part.id)) {
            hook(Change::Removed, part, &from.join(&part.id));
            hook(Change::Arrived, part, &list.folder.join(&part.id));
        }
    }

    /// The part `id` with its folder and its list's hook, or a sentence
    /// that names the parts there are.
    fn find(&self, id: &str) -> Result<(Arc<Part>, PathBuf, Option<Hook>), String> {
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
        self.find(id).map(|(part, ..)| (*part).clone())
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
            part.whole(&kept.done.into_iter().collect())
        })
    }

    /// What is in the folder of `part`, read again only when its record
    /// has changed or what was read is no longer taken to hold.
    fn see(&self, part: &Part, folder: &Path) -> Arc<Seen> {
        let stamp = stamp_of(folder);
        if let Some(seen) = self.seen.lock().get(folder)
            && seen.stamp == stamp
            && seen.at.elapsed() < LOOK_HOLDS
        {
            return seen.clone();
        }
        let seen = if stamp.is_none() && !folder.is_dir() {
            // Nothing was ever fetched into it: nothing to look for.
            Seen { stamp, at: Instant::now(), whole: false, here: 0, on_disk: 0 }
        } else {
            let kept = kept(folder, false);
            let done: HashSet<String> = kept.done.iter().cloned().collect();
            let whole = part.whole(&done);
            // What has come of the files that are not done with.
            let coming = |file: &Fetched| length(&partial_of(&folder.join(file.name()))).min(file.size);
            let (mut here, mut partial) = (0u64, 0u64);
            for file in &part.files {
                if done.contains(file.pin()) {
                    here += file.size;
                } else if !whole {
                    let come = coming(file);
                    here += come;
                    partial += come;
                }
            }
            Seen {
                stamp,
                at: Instant::now(),
                whole,
                here: if whole { part.size() } else { here },
                on_disk: kept.files.iter().map(|(_, size)| size).sum::<u64>() + partial,
            }
        };
        let seen = Arc::new(seen);
        self.seen.lock().insert(folder.to_owned(), seen.clone());
        seen
    }

    /// Forgets what was read of `folder`: something is about to change
    /// there, or has.
    fn forget(&self, folder: &Path) {
        self.seen.lock().remove(folder);
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
                        let (state, got, on_disk) = match going.get(&part.id) {
                            Some(going) => {
                                let (state, got) = *going.at.lock();
                                (state, got, got)
                            }
                            None => {
                                let seen = self.see(part, &folder);
                                let state = if seen.whole { PartState::Ready } else { PartState::Missing };
                                (state, seen.here, seen.on_disk)
                            }
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
    /// have come. From the first place it is published, and from each of
    /// the others in turn where the one before could not give it.
    pub fn fetch_now(&self, id: &str, stop: &AtomicBool, said: &mut dyn FnMut(PartState, u64)) -> Result<(), Ended> {
        let (part, folder, hook) = self.find(id).map_err(Ended::Failed)?;
        let _turn = self.turn.lock();
        let io = |error: std::io::Error| Ended::Failed(format!("{}: {error}", folder.display()));
        if !ours(&folder) {
            return Err(Ended::Failed(format!(
                "there is a folder at {} already that Trill did not make: move it, or keep these elsewhere, and fetch again",
                folder.display()
            )));
        }
        std::fs::create_dir_all(&folder).map_err(io)?;
        let mut kept = kept(&folder, true);
        // The folder is Trill's from here on, whatever becomes of the fetch.
        if !folder.join(KEPT).is_file() {
            write_json(&folder.join(KEPT), &kept).map_err(Ended::Failed)?;
        }
        // What a fetch that ended with the program left of its own.
        let _ = std::fs::remove_dir_all(folder.join(STAGING));
        self.forget(&folder);
        let mut whys: Vec<String> = Vec::new();
        let mut went = Ok(());
        for files in part.ways() {
            went = self.fetch_files(&part, files, &folder, &mut kept, stop, said);
            match &went {
                Ok(()) | Err(Went::Stopped | Went::Refused(_)) => break,
                Err(Went::Failed(why)) => whys.push(why.clone()),
            }
        }
        // A fetch that left nothing leaves no record either: the folder is as it was.
        if went.is_err() && kept.done.is_empty() && !holds_files(&folder) {
            let _ = std::fs::remove_file(folder.join(KEPT));
        }
        self.forget(&folder);
        match went {
            Ok(()) => {}
            Err(Went::Stopped) => return Err(Ended::Stopped),
            Err(Went::Refused(why)) => return Err(Ended::Failed(why)),
            Err(Went::Failed(_)) => {
                // Every place it is published was tried: what each said.
                let mut why = whys.remove(0);
                for other in &whys {
                    why.push_str(&format!(". The next place it is published was tried: {other}"));
                }
                return Err(Ended::Failed(why));
            }
        }
        // What a place that could not give it left under another name is not the part's.
        if !whys.is_empty() {
            sweep(&folder);
        }
        said(PartState::Ready, part.size());
        if let Some(hook) = hook {
            hook(Change::Arrived, &part, &folder);
        }
        Ok(())
    }

    /// One way of fetching `part`: the files it is published as in one place.
    fn fetch_files(
        &self,
        part: &Part,
        files: &[Fetched],
        folder: &Path,
        kept: &mut Kept,
        stop: &AtomicBool,
        said: &mut dyn FnMut(PartState, u64),
    ) -> Result<(), Went> {
        let done: HashSet<String> = kept.done.iter().cloned().collect();
        let is_done = |file: &Fetched| done.contains(file.pin());
        // The room for all of it, before any of it: what is still to come
        // of each file, and what an archive unpacks to beside itself.
        let mut wanted = 0u64;
        for file in files.iter().filter(|file| !is_done(file)) {
            let have = length(&partial_of(&folder.join(file.name()))).min(file.size);
            wanted += file.size - have + if file.is_archive() { file.unpacked } else { 0 };
        }
        if wanted > 0
            && let Some(free) = disk_free(folder).filter(|&free| free < wanted + MARGIN)
        {
            return Err(Went::Refused(no_room(&part.name, folder, free, wanted + MARGIN)));
        }
        let mut before: u64 = files.iter().filter(|file| is_done(file)).map(|file| file.size).sum();
        let mut index = 0;
        while index < files.len() {
            if is_done(&files[index]) {
                index += 1;
                continue;
            }
            // A run of small files that are kept as they come is fetched several at once.
            let small = |file: &Fetched| !file.is_archive() && file.size < SMALL && !is_done(file);
            let run = files[index..].iter().take(BATCH).take_while(|file| small(file)).count();
            if run >= 2 {
                self.fetch_small(part, &files[index..index + run], folder, kept, &mut before, stop, said)?;
                index += run;
            } else {
                self.fetch_one(&files[index], folder, kept, &mut before, stop, said)?;
                index += 1;
            }
        }
        Ok(())
    }

    /// Fetches, checks and keeps one file: a large one, or an archive.
    fn fetch_one(
        &self,
        file: &Fetched,
        folder: &Path,
        kept: &mut Kept,
        before: &mut u64,
        stop: &AtomicBool,
        said: &mut dyn FnMut(PartState, u64),
    ) -> Result<(), Went> {
        let io = |error: std::io::Error| Went::Failed(format!("{}: {error}", folder.display()));
        let name = file.name();
        let from = host(&file.url);
        let Some(target) = zip::below(folder, &name) else {
            return Err(Went::Refused(format!("{name} is not a path below the part's folder")));
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let partial = partial_of(&target);
        let guessed = file.sized_by_guess();
        if !guessed && length(&partial) > file.size {
            let _ = std::fs::remove_file(&partial);
        }
        // An archive whose size is a guess is all here when it is one that can be read to its
        // end. Anything less of it is not gone on with: its source makes it anew each time, and
        // the rest of another making is not the rest of this one. (Measured on such a source:
        // asked to go on from the middle, it begins again.)
        let whole_archive = guessed && length(&partial) > 0 && zip::listing(&partial).is_ok();
        if guessed && !whole_archive {
            let _ = std::fs::remove_file(&partial);
        }
        let have = length(&partial);
        let all_here = if guessed { whole_archive } else { have >= file.size };
        if !all_here {
            said(PartState::Fetching, *before + have.min(file.size));
            let fetcher = self.fetcher.lock().clone();
            let start = *before;
            let fetched = fetcher
                .fetch(&file.url, &partial, stop, &mut |got| said(PartState::Fetching, start + got.min(file.size)));
            if fetched.is_err() && guessed {
                let _ = std::fs::remove_file(&partial);
            }
            fetched.map_err(|ended| match ended {
                Ended::Stopped => Went::Stopped,
                Ended::Failed(why) if guessed => Went::Failed(format!(
                    "{name} could not be fetched from {from}: {why}. Its source makes it anew each time it is asked for, so fetching it again begins again"
                )),
                Ended::Failed(why) => Went::Failed(format!(
                    "{name} could not be fetched from {from}: {why}. What came of it is kept, and fetching it again goes on from there"
                )),
            })?;
        }
        said(PartState::Checking, *before + file.size);
        let not_it = |sum: &str, pin: &str| {
            Went::Failed(format!(
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
        } else if !guessed && length(&partial) != file.size {
            let _ = std::fs::remove_file(&partial);
            return Err(not_it("another size", "the size it was"));
        }
        let at = *before + file.size;
        let mut unpack = |wanted: &dyn Fn(&str) -> Option<String>| {
            said(PartState::Unpacking, at);
            zip::unpack(&partial, folder, wanted, stop, &mut |_| {})
        };
        let left: Vec<(String, u64)> = match &file.keep {
            Keep::As(_) => {
                std::fs::rename(&partial, &target).map_err(io)?;
                vec![(name.clone(), file.size)]
            }
            Keep::Members(members) => {
                let written =
                    unpack(&|member| members.iter().find(|(from, _)| from == member).map(|(_, to)| to.clone()))?;
                if let Some((lacking, _)) = members.iter().find(|(_, to)| !written.iter().any(|(path, _)| path == to)) {
                    return Err(Went::Failed(format!(
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
                    return Err(Went::Failed(format!(
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
                })
                .map_err(|ended| {
                    // An archive that cannot be read to its end did not all come: it is begun again.
                    if matches!(&ended, Ended::Failed(_)) && guessed {
                        let _ = std::fs::remove_file(&partial);
                    }
                    ended
                })?;
                if written.is_empty() {
                    return Err(Went::Failed(format!(
                        "nothing of {under} is in {name} from {from}, as it was when Trill was made"
                    )));
                }
                if !sum.is_empty() {
                    said(PartState::Checking, at);
                    let paths: Vec<String> = written.iter().map(|(path, _)| path.clone()).collect();
                    let found = tree_checksum(folder, &paths, stop)?;
                    if found != *sum {
                        for path in &paths {
                            let _ = zip::below(folder, path).map(std::fs::remove_file);
                        }
                        let _ = std::fs::remove_file(&partial);
                        return Err(not_it(&found, sum));
                    }
                }
                written
            }
        };
        if file.is_archive() {
            let _ = std::fs::remove_file(&partial);
        }
        kept.done.push(file.pin().to_owned());
        for (path, size) in left {
            kept.files.retain(|(known, _)| *known != path);
            kept.files.push((path, size));
        }
        write_json(&folder.join(KEPT), kept).map_err(Went::Failed)?;
        *before += file.size;
        Ok(())
    }

    /// Fetches, checks and keeps a batch of small files, several at once.
    #[allow(clippy::too_many_arguments)]
    fn fetch_small(
        &self,
        part: &Part,
        files: &[Fetched],
        folder: &Path,
        kept: &mut Kept,
        before: &mut u64,
        stop: &AtomicBool,
        said: &mut dyn FnMut(PartState, u64),
    ) -> Result<(), Went> {
        let io = |error: std::io::Error| Went::Failed(format!("{}: {error}", folder.display()));
        let from = host(&files[0].url);
        let mut targets = Vec::with_capacity(files.len());
        // What is asked for, and how much of the batch came whole before.
        let (mut asked, mut here, mut coming) = (Vec::new(), 0u64, 0u64);
        for file in files {
            let name = file.name();
            let Some(target) = zip::below(folder, &name) else {
                return Err(Went::Refused(format!("{name} is not a path below the part's folder")));
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            let partial = partial_of(&target);
            if length(&partial) > file.size {
                let _ = std::fs::remove_file(&partial);
            }
            if length(&partial) == file.size {
                here += file.size;
            } else {
                coming += file.size;
                asked.push((file.url.clone(), partial.clone()));
            }
            targets.push((target, partial));
        }
        let start = *before + here;
        if !asked.is_empty() {
            said(PartState::Fetching, start);
            let fetcher = self.fetcher.lock().clone();
            fetcher
                .fetch_many(&asked, folder, stop, &mut |got| said(PartState::Fetching, start + got.min(coming)))
                .map_err(|ended| match ended {
                    Ended::Stopped => Went::Stopped,
                    Ended::Failed(why) => Went::Failed(format!(
                        "files of {} could not be fetched from {from}: {why}. What came is kept, and fetching it again goes on from there",
                        part.name
                    )),
                })?;
        }
        said(PartState::Checking, start + coming);
        // Each is checked and kept by itself: one that is not the file it
        // was pinned to be does not take the others with it.
        let mut wrong: Option<Went> = None;
        for (file, (target, partial)) in files.iter().zip(&targets) {
            let sum = if length(partial) == file.size {
                match checksum(partial, stop) {
                    Ok(sum) => sum,
                    // Stopped while it was checked: what was checked before it is kept.
                    Err(ended) => {
                        wrong = Some(ended.into());
                        break;
                    }
                }
            } else {
                "another size".to_owned()
            };
            if sum != file.sha256 {
                let _ = std::fs::remove_file(partial);
                wrong.get_or_insert_with(|| {
                    Went::Failed(format!(
                        "{} from {from} is not the file Trill was made for: it was replaced where it is published, or damaged on the way (its checksum begins {}, where {} was expected). It was not kept",
                        file.name(),
                        &sum[..12.min(sum.len())],
                        &file.sha256[..12.min(file.sha256.len())],
                    ))
                });
                continue;
            }
            if let Err(error) = std::fs::rename(partial, target) {
                wrong = Some(io(error));
                break;
            }
            kept.done.push(file.pin().to_owned());
            let name = file.name();
            kept.files.retain(|(known, _)| *known != name);
            kept.files.push((name, file.size));
            *before += file.size;
        }
        write_json(&folder.join(KEPT), kept).map_err(Went::Failed)?;
        match wrong {
            Some(went) => Err(went),
            None => Ok(()),
        }
    }

    /// Has the thread that fetches take up what waits, if it is not at it.
    fn work(self: &Arc<Self>) -> Result<(), String> {
        {
            let mut queue = self.queue.lock();
            if queue.working {
                return Ok(());
            }
            queue.working = true;
        }
        let engine = self.clone();
        let spawned = std::thread::Builder::new().name("trill-fetch".into()).spawn(move || {
            loop {
                let id = {
                    let mut queue = engine.queue.lock();
                    match queue.waiting.pop_front() {
                        Some(id) => id,
                        None => {
                            queue.working = false;
                            return;
                        }
                    }
                };
                // One that was stopped while it waited is no longer there.
                let Some(going) = engine.going.lock().get(&id).cloned() else { continue };
                let ended = if going.stop.load(Ordering::Relaxed) {
                    Err(Ended::Stopped)
                } else {
                    engine.fetch_now(&id, &going.stop, &mut |state, got| *going.at.lock() = (state, got))
                };
                if let Err(Ended::Failed(why)) = ended {
                    log::warn!("{id}: {why}");
                    engine.failed.lock().insert(id.clone(), why);
                }
                engine.going.lock().remove(&id);
            }
        });
        if let Err(error) = spawned {
            let mut queue = self.queue.lock();
            queue.working = false;
            let mut going = self.going.lock();
            for id in queue.waiting.drain(..) {
                going.remove(&id);
            }
            return Err(format!("the thread that fetches could not be started: {error}"));
        }
        Ok(())
    }

    /// Puts `parts` at the end of what waits, and has them fetched.
    fn enqueue(self: &Arc<Self>, parts: &[Arc<Part>]) -> Result<(), String> {
        {
            let mut going = self.going.lock();
            let mut failed = self.failed.lock();
            let mut queue = self.queue.lock();
            for part in parts {
                let waits = Going { stop: AtomicBool::new(false), at: Mutex::new((PartState::Waiting, 0)) };
                going.insert(part.id.clone(), Arc::new(waits));
                failed.remove(&part.id);
                queue.waiting.push_back(part.id.clone());
            }
        }
        self.work()
    }

    /// Starts fetching the part `id` and answers at once:
    /// [`Fetches::status`] says how it goes. Parts are fetched one at a
    /// time, in the order they were asked for.
    pub fn fetch(self: &Arc<Self>, id: &str) -> Result<Part, String> {
        let (part, ..) = self.find(id)?;
        if self.ready(id) {
            return Err(format!("{} is here already", part.name));
        }
        if self.going.lock().contains_key(&part.id) {
            return Err(format!("{} is being fetched already", part.name));
        }
        self.enqueue(std::slice::from_ref(&part))?;
        Ok((*part).clone())
    }

    /// Asks for several parts at once: the ones that are not here and not
    /// asked for already wait their turn in the order given, and are
    /// fetched one after the other. Refused as a whole, with nothing
    /// asked for, when one of them is no part, or when the disk they are
    /// kept on has not the room for all that is not here yet.
    /// [`Fetches::status`] says how each goes; [`Fetches::stop_all`] stops
    /// them, and asking again goes on with what is left.
    pub fn fetch_many(self: &Arc<Self>, ids: &[String]) -> Result<Asked, String> {
        let mut asked = Asked::default();
        let mut wanted: Vec<(Arc<Part>, PathBuf)> = Vec::new();
        for id in ids {
            let (part, folder, _) = self.find(id)?;
            if wanted.iter().any(|(known, _)| known.id == part.id) {
                continue;
            }
            if self.going.lock().contains_key(&part.id) {
                asked.going += 1;
            } else if self.ready(id) {
                asked.here += 1;
            } else {
                wanted.push((part, folder));
            }
        }
        // The room for all of them: what each takes once it is here, and
        // beside that the largest of what is fetched and then unpacked.
        let mut rooms: HashMap<PathBuf, (u64, u64)> = HashMap::new();
        for (part, folder) in &wanted {
            let disk = folder.ancestors().find(|folder| folder.is_dir()).unwrap_or(folder).to_owned();
            let room = rooms.entry(disk).or_default();
            room.0 += part.room();
            room.1 = room.1.max(part.wanted().saturating_sub(part.room()));
            asked.bytes += part.size();
        }
        for (disk, (kept, beside)) in rooms {
            let all = kept + beside + MARGIN;
            if let Some(free) = disk_free(&disk).filter(|&free| free < all) {
                return Err(format!(
                    "there is not the room on the disk for these {}: {} are free where they are kept ({}), and {} are wanted. Make room, keep them on another disk, or ask for fewer",
                    wanted.len(),
                    whole::said(free),
                    disk.display(),
                    whole::said(all),
                ));
            }
        }
        let parts: Vec<Arc<Part>> = wanted.into_iter().map(|(part, _)| part).collect();
        asked.queued = parts.iter().map(|part| part.id.clone()).collect();
        if !parts.is_empty() {
            self.enqueue(&parts)?;
        }
        Ok(asked)
    }

    /// Stops the fetch of the part `id`. What has come of it is kept.
    pub fn stop(&self, id: &str) -> Result<Part, String> {
        let (part, ..) = self.find(id)?;
        let mut going = self.going.lock();
        let Some(one) = going.get(&part.id) else {
            return Err(format!("{} is not being fetched", part.name));
        };
        one.stop.store(true, Ordering::Relaxed);
        // One that was still waiting is not asked for any more, at once.
        let mut queue = self.queue.lock();
        if let Some(place) = queue.waiting.iter().position(|waiting| *waiting == part.id) {
            queue.waiting.remove(place);
            going.remove(&part.id);
        }
        Ok((*part).clone())
    }

    /// Stops everything that is being fetched or waiting to be, of the
    /// list `list` or of every list, and answers with how many parts that
    /// was. What has come of the one on its way is kept; asking for them
    /// again goes on from there.
    pub fn stop_all(&self, list: Option<&str>) -> usize {
        let of_list: Option<HashSet<String>> = list.map(|id| {
            let lists = self.lists.lock();
            lists.iter().filter(|held| held.id == id).flat_map(|held| &held.parts).map(|part| part.id.clone()).collect()
        });
        let mut going = self.going.lock();
        let mut queue = self.queue.lock();
        let stopped: Vec<String> =
            going.keys().filter(|id| of_list.as_ref().is_none_or(|ids| ids.contains(*id))).cloned().collect();
        for id in &stopped {
            if let Some(one) = going.get(id) {
                one.stop.store(true, Ordering::Relaxed);
            }
            if let Some(place) = queue.waiting.iter().position(|waiting| waiting == id) {
                queue.waiting.remove(place);
                going.remove(id);
            }
        }
        stopped.len()
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
        self.forget(&folder);
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
        let heard: Arc<Mutex<Vec<(Change, PathBuf)>>> = Arc::default();
        let hears = heard.clone();
        let hook: Hook = Arc::new(move |change, _, folder| hears.lock().push((change, folder.to_owned())));
        engine.add(List { changed: Some(hook), ..list("more", vec![named("two")]) }).unwrap();
        assert_eq!(engine.list_ids(), ["models", "more"]);
        // A part that is here and whose list is kept elsewhere: its owner hears where it went.
        engine.fetch_now("two", &quiet(), &mut |_, _| {}).unwrap();
        let (was, now) = (folder.path().join("instruments"), folder.path().join("moved"));
        std::fs::rename(&was, &now).unwrap();
        engine.set_folder("more", now.clone()).unwrap();
        engine.moved("more", &was);
        assert_eq!(heard.lock()[1..], [(Change::Removed, was.join("two")), (Change::Arrived, now.join("two"))]);
        engine.remove("two").unwrap();
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

    /// The bytes of a made-up recording: different for every one.
    fn recording(number: usize, length: usize) -> Vec<u8> {
        (0..length).map(|index| ((index * 7 + number * 131) % 251) as u8).collect()
    }

    /// A set of `count` small recordings in folders, with an instrument's
    /// file, named the way sets are: with spaces, commas and a `#`.
    fn set_of(shelf: &Shelf, id: &str, count: usize) -> Part {
        let mut files: Vec<(String, Vec<u8>)> =
            vec![("Grand Piano, Small.sfz".to_owned(), b"<region> sample=Samples/C#4 soft.wav".to_vec())];
        for number in 0..count {
            let folder = if number % 2 == 0 { "Samples" } else { "Samples/Loud, close" };
            files.push((format!("{folder}/C#{number} soft.wav"), recording(number, 3_000 + number)));
        }
        let borrowed: Vec<(&str, Vec<u8>)> = files.iter().map(|(name, bytes)| (name.as_str(), bytes.clone())).collect();
        let mut set = part(shelf, id, "A small grand", &borrowed);
        // Published at addresses that hold their paths as an address can.
        for file in &mut set.files {
            let Keep::As(path) = &file.keep else { unreachable!() };
            let url = address(&format!("https://shelf.test/{id}/"), path);
            let on_shelf = files.iter().find(|(name, _)| name == path).unwrap().1.clone();
            shelf.take(&file.url);
            shelf.put(&url, on_shelf);
            file.url = url;
        }
        set
    }

    #[test]
    fn an_address_holds_a_path_as_an_address_can() {
        assert_eq!(
            address("https://e.org/raw/abc/", "Grand Piano, Steinway B/C#4 é (soft).wav"),
            "https://e.org/raw/abc/Grand%20Piano%2C%20Steinway%20B/C%234%20%C3%A9%20%28soft%29.wav"
        );
        let file = Fetched::file_below("https://e.org/raw/abc/", "Samples/A 1.wav", &"ab".repeat(32), 12);
        assert_eq!(
            (file.url.as_str(), &file.keep),
            ("https://e.org/raw/abc/Samples/A%201.wav", &Keep::As("Samples/A 1.wav".into()))
        );
        // Such an address is one a list may hold; one with a space in it is not.
        assert!(!file.url.contains(char::is_whitespace) && !file.sized_by_guess());
    }

    #[test]
    fn a_part_of_many_small_files_is_fetched_several_at_once_and_what_a_stopped_fetch_had_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let set = set_of(&shelf, "small-grand", 150);
        let all: u64 = set.size();
        let engine = engine(folder.path(), vec![set.clone()], shelf.clone(), None);

        // The connection is lost at once.
        shelf.cut_after(0);
        let mut seen = 0u64;
        let ended = engine.fetch_now("small-grand", &quiet(), &mut |_, got| {
            assert!(got >= seen && got <= all, "what has come only grows, and is never more than there is");
            seen = got;
        });
        let Err(Ended::Failed(why)) = ended else { panic!("{ended:?}") };
        assert!(
            why.starts_with("files of A small grand could not be fetched from shelf.test: the connection was lost")
                && why.ends_with("fetching it again goes on from there"),
            "{why}"
        );
        // Nothing was kept, and the folder is still Trill's to go on in. Fetched again, a batch at a time.
        let kept_folder = folder.path().join("small-grand");
        assert!(ours(&kept_folder), "a folder a fetch was stopped in is still Trill's to go on in");
        engine.fetch_now("small-grand", &quiet(), &mut |_, _| {}).unwrap();
        assert!(engine.ready("small-grand"));
        assert_eq!(shelf.together(), [BATCH, BATCH, BATCH, 151 - 2 * BATCH], "a batch at a time");
        let status = engine.status_of("small-grand").unwrap();
        assert_eq!((status.state, status.got, status.on_disk), (PartState::Ready, all, all));
        assert_eq!(std::fs::read(kept_folder.join("Samples").join("C#4 soft.wav")).unwrap(), recording(4, 3_004));
        assert_eq!(
            std::fs::read(kept_folder.join("Samples").join("Loud, close").join("C#7 soft.wav")).unwrap(),
            recording(7, 3_007)
        );
        // Nothing is left under the name it was written by, however deep.
        fn partials(folder: &Path) -> usize {
            std::fs::read_dir(folder)
                .unwrap()
                .map(|entry| entry.unwrap())
                .map(|entry| match entry.file_type().unwrap().is_dir() {
                    true => partials(&entry.path()),
                    false => usize::from(entry.file_name().to_string_lossy().ends_with(".partial")),
                })
                .sum()
        }
        assert_eq!(partials(&kept_folder), 0);

        // Stopped in its third batch, it has kept two batches; gone on with, only the rest is asked for.
        engine.remove("small-grand").unwrap();
        let before = shelf.asked().len();
        let stop = AtomicBool::new(false);
        let mut batches = 0;
        let ended = engine.fetch_now("small-grand", &stop, &mut |state, _| {
            if state == PartState::Checking {
                batches += 1;
                if batches == 2 {
                    stop.store(true, Ordering::Relaxed);
                }
            }
        });
        assert_eq!(ended, Err(Ended::Stopped));
        let stopped = engine.status_of("small-grand").unwrap();
        assert_eq!(stopped.state, PartState::Missing);
        assert!(stopped.got > 0 && stopped.got < all && stopped.error.is_none(), "{stopped:?}");
        let kept_now: Kept = serde_json::from_str(&std::fs::read_to_string(kept_folder.join(KEPT)).unwrap()).unwrap();
        assert!(kept_now.done.len() >= BATCH && kept_now.done.len() < 151, "{}", kept_now.done.len());
        engine.fetch_now("small-grand", &quiet(), &mut |_, _| {}).unwrap();
        assert!(engine.ready("small-grand"));
        // Nothing that had come was asked for again: not what was kept, nor what was whole and unchecked.
        assert_eq!(shelf.asked().len() - before, 151);

        // One file that is not the one pinned is refused by name; the others of its batch are kept.
        engine.remove("small-grand").unwrap();
        shelf.put(&set.files[3].url, vec![9u8; set.files[3].size as usize]);
        let Err(Ended::Failed(why)) = engine.fetch_now("small-grand", &quiet(), &mut |_, _| {}) else { panic!() };
        assert!(why.starts_with("Samples/C#2 soft.wav from shelf.test is not the file Trill was made for"), "{why}");
        assert!(kept_folder.join("Samples").join("C#4 soft.wav").is_file() && !engine.ready("small-grand"));
        assert!(!kept_folder.join("Samples").join("C#2 soft.wav").exists());
    }

    #[test]
    fn an_archive_whose_size_is_a_guess_is_taken_however_large_it_comes_and_what_it_unpacks_to_decides() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let sfz: &[u8] = b"<region> sample=samples/C4.flac";
        let members: [(&str, &[u8], bool); 2] =
            [("kit-0123abc/Kit.sfz", sfz, true), ("kit-0123abc/samples/C4.flac", &[4u8; 40_000], false)];
        let archive = zip::archive_of(&members);
        let lines: String = [("Kit.sfz", members[0].1), ("samples/C4.flac", members[1].1)]
            .iter()
            .map(|(path, bytes)| format!("{}  {path}\n", hex(&Sha256::digest(bytes))))
            .collect();
        let sum = hex(&Sha256::digest(lines.as_bytes()));
        let mut kit = part(&shelf, "kit", "A kit", &[("0123abc.zip", archive.clone())]);
        kit.files[0].sha256 = String::new();
        kit.files[0].keep = Keep::Tree { under: "kit-0123abc/".into(), sum };
        // Nobody can say how large its source will make it: a third too small, here.
        let guess = archive.len() as u64 * 2 / 3;
        kit.files[0] = kit.files[0].clone().unpacking_to(40_000 + sfz.len() as u64);
        kit.files[0].size = guess;
        assert!(kit.files[0].sized_by_guess());
        assert_eq!((kit.size(), kit.room()), (guess, 40_000 + sfz.len() as u64));
        let engine = engine(folder.path(), vec![kit.clone()], shelf.clone(), None);
        let mut most = 0u64;
        engine.fetch_now("kit", &quiet(), &mut |_, got| most = most.max(got)).unwrap();
        assert!(engine.ready("kit") && most <= guess, "what is said to have come is never more than was said to come");
        assert_eq!(std::fs::read(folder.path().join("kit").join("Kit.sfz")).unwrap(), sfz);

        // What had come of it when the connection was lost is not kept: its source makes it anew each
        // time, so it is begun again, and it says so.
        engine.remove("kit").unwrap();
        shelf.cut_after(10_000);
        let Err(Ended::Failed(why)) = engine.fetch_now("kit", &quiet(), &mut |_, _| {}) else { panic!() };
        assert!(why.ends_with("so fetching it again begins again"), "{why}");
        assert!(!folder.path().join("kit").join("0123abc.zip.partial").exists());
        let lost = engine.status_of("kit").unwrap();
        assert_eq!((lost.state, lost.got, lost.on_disk), (PartState::Missing, 0, 0));
        engine.fetch_now("kit", &quiet(), &mut |_, _| {}).unwrap();
        assert!(engine.ready("kit") && *shelf.began().last().unwrap() == 0);

        // One that unpacks to something else is refused, whatever its size.
        engine.remove("kit").unwrap();
        let other = zip::archive_of(&[members[0], ("kit-0123abc/samples/C4.flac", &[5u8; 40_000], false)]);
        shelf.put(&kit.files[0].url, other);
        let Err(Ended::Failed(why)) = engine.fetch_now("kit", &quiet(), &mut |_, _| {}) else { panic!() };
        assert!(why.starts_with("0123abc.zip from shelf.test is not the file Trill was made for"), "{why}");
        // And a file pinned by its own bytes still has to be the size it was pinned to be.
        let exact = part(&shelf, "exact", "Exact", &[("a.bin", vec![1u8; 100])]);
        assert!(!exact.files[0].sized_by_guess());
    }

    #[cfg(windows)]
    #[test]
    fn the_room_is_asked_for_before_anything_is_fetched_the_archive_and_what_it_unpacks_to() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let archive = zip::archive_of(&[("big/a.wav", &[1u8; 5_000], false)]);
        let mut big = part(&shelf, "big", "A big set", &[("big.zip", archive)]);
        big.files[0].keep = Keep::Tree { under: "big/".into(), sum: String::new() };
        // It says it unpacks to more than any disk has.
        big.files[0] = big.files[0].clone().unpacking_to(1 << 60);
        let small = part(&shelf, "small", "A small set", &[("a.bin", vec![1u8; 100])]);
        let engine = engine(folder.path(), vec![big, small], shelf.clone(), None);
        let Err(Ended::Failed(why)) = engine.fetch_now("big", &quiet(), &mut |_, _| {}) else { panic!() };
        assert!(why.starts_with("there is not the room on the disk for A big set: "), "{why}");
        assert!(why.ends_with("Make room, or keep it on another disk"), "{why}");
        // Nothing was asked of anyone, and nothing is left where it would have been kept.
        assert!(shelf.asked().is_empty());
        assert!(std::fs::read_dir(folder.path().join("big")).unwrap().next().is_none());

        // Asked for with others, the whole is refused before any of it begins.
        let refused = engine.fetch_many(&["small".to_owned(), "big".to_owned()]).unwrap_err();
        assert!(refused.starts_with("there is not the room on the disk for these 2: "), "{refused}");
        assert!(refused.ends_with("Make room, keep them on another disk, or ask for fewer"), "{refused}");
        assert!(shelf.asked().is_empty() && !engine.fetching());
        assert_eq!(engine.status_of("small").unwrap().state, PartState::Missing);
    }

    #[test]
    fn several_parts_asked_for_at_once_wait_their_turn_and_the_whole_is_stopped_and_gone_on_with() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let names = ["piano", "strings", "bass", "kit"];
        let parts: Vec<Part> = names
            .iter()
            .enumerate()
            .map(|(number, id)| part(&shelf, id, id, &[("set.bin", recording(number, 200_000))]))
            .collect();
        let heard: Arc<Mutex<Vec<String>>> = Arc::default();
        let hears = heard.clone();
        let hook: Hook = Arc::new(move |change, part, _| {
            if change == Change::Arrived {
                hears.lock().push(part.id.clone());
            }
        });
        let engine = engine(folder.path(), parts, shelf.clone(), Some(hook));
        let ids: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        assert!(
            engine
                .fetch_many(&["piano".to_owned(), "harp".to_owned()])
                .unwrap_err()
                .starts_with("there is no part 'harp'")
        );
        assert!(!engine.fetching(), "an ask with a part that is none asks for nothing");

        // All four are asked for at once: one is on its way and the others wait, in the order given.
        shelf.slow(Duration::from_millis(15));
        let asked = engine.fetch_many(&ids).unwrap();
        assert_eq!((asked.queued.as_slice(), asked.here, asked.going, asked.bytes), (ids.as_slice(), 0, 0, 800_000));
        wait_for(&engine, "piano", PartState::Fetching);
        for waiting in &names[1..] {
            assert_eq!(engine.status_of(waiting).unwrap().state, PartState::Waiting);
        }
        // Asked for again while they wait, nothing is asked for twice.
        let again = engine.fetch_many(&ids).unwrap();
        assert_eq!((again.queued.len(), again.going), (0, 4));
        assert_eq!(engine.fetch("strings").unwrap_err(), "strings is being fetched already");
        // One that waits can be taken out alone, and is not here and not waiting at once.
        engine.stop("kit").unwrap();
        assert_eq!(engine.status_of("kit").unwrap().state, PartState::Missing);
        wait_for(&engine, "piano", PartState::Ready);
        wait_for(&engine, "strings", PartState::Fetching);

        // The whole is stopped: what was on its way keeps what came, and nothing waits any more.
        for _ in 0..500 {
            if engine.status_of("strings").unwrap().got > 40_000 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(engine.stop_all(Some("models")), 2);
        assert_eq!(engine.status_of("bass").unwrap().state, PartState::Missing, "what waited is not asked for at once");
        let stopped = wait_for(&engine, "strings", PartState::Missing);
        assert!(stopped.got > 0 && stopped.got < 200_000 && stopped.error.is_none(), "{stopped:?}");
        assert!(!engine.fetching());
        assert_eq!(engine.stop_all(None), 0);

        // Asked for again, it is gone on with: what is here is left, what had come is gone on from.
        shelf.slow(Duration::ZERO);
        let rest = engine.fetch_many(&ids).unwrap();
        assert_eq!((rest.queued.as_slice(), rest.here), (&ids[1..], 1));
        for id in &names {
            wait_for(&engine, id, PartState::Ready);
        }
        assert_eq!(*heard.lock(), names, "each arrived once, in the order they were asked for");
        // The one that was stopped part of the way was gone on with from where it was.
        let began = shelf.began();
        assert!(began.iter().filter(|at| **at > 0).count() == 1, "{began:?}");
        assert_eq!(std::fs::read(folder.path().join("bass").join("set.bin")).unwrap(), recording(2, 200_000));
    }

    #[test]
    fn a_part_its_first_place_cannot_give_is_fetched_from_the_next_and_is_the_same_part() {
        let folder = tempfile::tempdir().unwrap();
        let shelf = Arc::new(Shelf::default());
        let (sfz, wav): (&[u8], &[u8]) = (b"<region> sample=a.wav", &[7u8; 9_000]);
        let mut flute = part(&shelf, "flute", "A flute", &[("Flute.sfz", sfz.to_vec()), ("a.wav", wav.to_vec())]);
        // Somebody keeps the whole of it as one archive, pinned by its own bytes and by what it unpacks to.
        let archive = zip::archive_of(&[("flute/Flute.sfz", sfz, true), ("flute/a.wav", wav, true)]);
        let lines: String = [("Flute.sfz", sfz), ("a.wav", wav)]
            .iter()
            .map(|(path, bytes)| format!("{}  {path}\n", hex(&Sha256::digest(bytes))))
            .collect();
        let kept_as = Keep::Tree { under: "flute/".into(), sum: hex(&Sha256::digest(lines.as_bytes())) };
        let mirror = "https://mirror.test/flute.zip";
        shelf.put(mirror, archive.clone());
        flute.elsewhere =
            vec![vec![Fetched::archive(mirror, &hex(&Sha256::digest(&archive)), archive.len() as u64, kept_as)]];
        let engine = engine(folder.path(), vec![flute.clone()], shelf.clone(), None);
        // A second place has to be pinned like the first.
        let mut unpinned = Part { id: "oboe".into(), ..flute.clone() };
        unpinned.elsewhere[0][0].sha256 = "abc".into();
        unpinned.elsewhere[0][0].keep = Keep::Tree { under: String::new(), sum: String::new() };
        let more = List {
            id: "more".into(),
            title: "More".into(),
            about: String::new(),
            folder: folder.path().join("more"),
            parts: vec![unpinned],
            changed: None,
        };
        assert!(engine.add(more).unwrap_err().ends_with("is not pinned to a size and a checksum"));

        // Where it is first published it is there: the second place is not asked.
        engine.fetch_now("flute", &quiet(), &mut |_, _| {}).unwrap();
        assert!(engine.ready("flute") && !shelf.asked().iter().any(|url| url == mirror));
        engine.remove("flute").unwrap();

        // A file of it is gone from there: it comes from the second place, and is the same part.
        shelf.take(&flute.files[1].url);
        engine.fetch_now("flute", &quiet(), &mut |_, _| {}).unwrap();
        assert!(engine.ready("flute") && shelf.asked().iter().any(|url| url == mirror));
        let kept = folder.path().join("flute");
        assert_eq!(
            (std::fs::read(kept.join("Flute.sfz")).unwrap(), std::fs::read(kept.join("a.wav")).unwrap()),
            (sfz.to_vec(), wav.to_vec())
        );
        assert_eq!(engine.status_of("flute").unwrap().state, PartState::Ready);
        engine.remove("flute").unwrap();

        // Gone from both, it says what each place said.
        shelf.take(mirror);
        let Err(Ended::Failed(why)) = engine.fetch_now("flute", &quiet(), &mut |_, _| {}) else { panic!() };
        assert!(why.starts_with("files of A flute could not be fetched from shelf.test: the server said 404"), "{why}");
        assert!(
            why.contains("The next place it is published was tried: flute.zip could not be fetched from mirror.test"),
            "{why}"
        );
        assert!(!engine.ready("flute"));
    }
}
