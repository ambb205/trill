//! Whether what arrived in a folder is the set that was read.
//!
//! A set is pinned in one of two ways ([`Fetch`]). File by file, each
//! file has its own checksum, and a file that is missing or is not the
//! one that was read is named. As an archive that its source makes anew
//! each time it is asked for, the archive's own bytes cannot be pinned,
//! so what it unpacks to is: the *tree checksum* of a folder is the
//! SHA-256 of one line for each file below it, `<the file's SHA-256>`,
//! two spaces, `<its path>` and a line feed, with forward slashes in the
//! paths and the lines in the order of their paths as bytes. That is the
//! checksum the program's fetching pins an unpacked archive by, so one
//! number in the catalogue serves both.
//!
//! Two files in a set's folder are not the set's and are passed over: the
//! record the fetching keeps there (`part.json`), and whatever of a file
//! had come when a fetch was stopped (`*.partial`).

use crate::{Fetch, Set};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The record the program's fetching keeps in a folder it fetched into.
const KEPT: &str = "part.json";
/// What a file is called while it is being fetched.
const PARTIAL: &str = ".partial";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The SHA-256 of a file, in hexadecimal.
pub fn file_sum(path: &Path) -> Result<String, String> {
    let said = |error: std::io::Error| format!("{}: {error}", path.display());
    let mut file = std::fs::File::open(path).map_err(said)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).map_err(said)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// The SHA-256 of a text, in hexadecimal: what an instrument's file is
/// known again by.
pub fn text_sum(text: &[u8]) -> String {
    hex(&Sha256::digest(text))
}

/// What is in a folder: see the notes at the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    /// The tree checksum, in hexadecimal.
    pub sum: String,
    pub files: u32,
    pub bytes: u64,
}

/// Every file below `folder` that is a set's, as `(path, size)`, the paths
/// with forward slashes.
fn files_below(folder: &Path) -> Result<Vec<(String, u64)>, String> {
    let mut found = Vec::new();
    let mut waiting = vec![(folder.to_path_buf(), String::new())];
    while let Some((at, below)) = waiting.pop() {
        let listing = std::fs::read_dir(&at).map_err(|error| format!("{}: {error}", at.display()))?;
        for entry in listing {
            let entry = entry.map_err(|error| format!("{}: {error}", at.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(format!("{} has a file whose name is not text", at.display()));
            };
            let path = if below.is_empty() { name.to_string() } else { format!("{below}/{name}") };
            let kind = entry.file_type().map_err(|error| format!("{}: {error}", entry.path().display()))?;
            if kind.is_dir() {
                waiting.push((entry.path(), path));
            } else if kind.is_file() && path != KEPT && !name.ends_with(PARTIAL) {
                let size = entry.metadata().map_err(|error| format!("{}: {error}", entry.path().display()))?.len();
                found.push((path, size));
            }
        }
    }
    Ok(found)
}

/// The tree checksum of everything in `folder`.
pub fn tree_sum(folder: &Path) -> Result<Tree, String> {
    if !folder.is_dir() {
        return Err(format!("there is no folder at {}", folder.display()));
    }
    let mut files = files_below(folder)?;
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = Sha256::new();
    let mut bytes = 0;
    for (path, size) in &files {
        let sum = file_sum(&below(folder, path))?;
        hasher.update(format!("{sum}  {path}\n").as_bytes());
        bytes += size;
    }
    Ok(Tree { sum: hex(&hasher.finalize()), files: files.len() as u32, bytes })
}

/// The tree checksum of files that are known by their own checksums,
/// without reading them: what a set's line in the catalogue is written
/// from.
pub fn tree_sum_of<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut files: Vec<(&str, &str)> = files.into_iter().collect();
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = Sha256::new();
    for (path, sum) in files {
        hasher.update(format!("{sum}  {path}\n").as_bytes());
    }
    hex(&hasher.finalize())
}

/// The file at `path` (forward slashes) below `folder`.
pub fn below(folder: &Path, path: &str) -> PathBuf {
    path.split('/').fold(folder.to_path_buf(), |at, part| at.join(part))
}

/// The address of the file at `path` below `base`: the path with every
/// character an address cannot hold written as `%XX`.
pub fn address(base: &str, path: &str) -> String {
    let mut address = String::with_capacity(base.len() + path.len() + 16);
    address.push_str(base);
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => address.push(byte as char),
            other => address.push_str(&format!("%{other:02X}")),
        }
    }
    address
}

/// A set that is all there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whole {
    pub files: u32,
    pub bytes: u64,
    /// The files of its instruments, where they are now.
    pub instruments: Vec<PathBuf>,
}

/// A few names, and how many more there are.
fn named(paths: &[String]) -> String {
    let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
    match paths.len().saturating_sub(shown.len()) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

impl Set {
    /// Whether `folder` holds this set, all of it and as it was read. An
    /// error is a sentence: what is missing or is not what was read, and
    /// what to do. Reads every file of the set, which for a large one
    /// takes seconds.
    pub fn check(&self, folder: &Path) -> Result<Whole, String> {
        if !folder.is_dir() {
            return Err(format!("{} is not at {}: there is no such folder", self.name, folder.display()));
        }
        let (files, bytes) = match &self.fetch {
            Fetch::Files { files, .. } => {
                let (mut missing, mut changed) = (Vec::new(), Vec::new());
                for file in files {
                    let at = below(folder, &file.path);
                    match std::fs::metadata(&at) {
                        Ok(found) if found.is_file() => {
                            if found.len() != file.size || file_sum(&at)? != file.sha256 {
                                changed.push(file.path.clone());
                            }
                        }
                        _ => missing.push(file.path.clone()),
                    }
                }
                if !missing.is_empty() {
                    return Err(format!(
                        "{} is not whole at {}: {} of its {} files are not there ({}). It was not fetched to the end: \
                         fetch it again, and what is there is kept",
                        self.name,
                        folder.display(),
                        missing.len(),
                        files.len(),
                        named(&missing)
                    ));
                }
                if !changed.is_empty() {
                    return Err(format!(
                        "{} at {} is not the set Trill read: {} of its {} files are other files than were read ({}). \
                         They were replaced where they are published, changed on this disk, or damaged on the way: \
                         remove them and fetch it again",
                        self.name,
                        folder.display(),
                        changed.len(),
                        files.len(),
                        named(&changed)
                    ));
                }
                (files.len() as u32, files.iter().map(|file| file.size).sum())
            }
            Fetch::Archive { sum, .. } => {
                let found = tree_sum(folder)?;
                if found.sum != *sum {
                    let what = if found.files < self.files {
                        format!(
                            "it has {} files where the set has {}: it was not unpacked to the end. Fetch it again",
                            found.files, self.files
                        )
                    } else if found.files > self.files {
                        format!(
                            "it has {} files where the set has {}: something else is in its folder. Keep the set in \
                             a folder of its own",
                            found.files, self.files
                        )
                    } else {
                        format!(
                            "its files are other files than were read (their checksum begins {} where {} was \
                             expected): it was replaced where it is published, changed on this disk, or damaged on \
                             the way. Remove it and fetch it again",
                            &found.sum[..12],
                            &sum[..12]
                        )
                    };
                    return Err(format!("{} at {} is not the set Trill read: {what}", self.name, folder.display()));
                }
                (found.files, found.bytes)
            }
        };
        let instruments = self.instruments.iter().map(|instrument| below(folder, &instrument.file)).collect();
        Ok(Whole { files, bytes, instruments })
    }
}
