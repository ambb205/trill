//! Scanning: turning folders on disk into an [`Index`].
//!
//! A scan lists the folders, reads the header of every audio file it has
//! not seen in its present state, reads what the names say, and builds a
//! new index. Listing is cheap and reading headers is not, so a scan reuses
//! everything the previous index knows about files whose size and modified
//! time are unchanged: the second scan of a library touches no audio file.
//!
//! Disks are untrusted. Folders that cannot be read, names that are not
//! Unicode and files that are not what their extension claims are noted
//! and stepped over; nothing here panics on what it finds.

use crate::entry::{AudioFacts, Entry, EntryKind, display_name};
use crate::index::{FolderId, Index, IndexBuilder, OutOfOrder};
use crate::infer::{FolderReading, NameReading, discount_counting};
use crate::natural::natural_cmp;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};
use trill_media::MediaError;

/// Folders that are never library content, whatever is in them: archive
/// litter, the recycle bin, and the previews Ableton keeps beside its packs
/// (renderings of presets, not samples).
const SKIPPED_FOLDERS: &[&str] = &["__MACOSX", "$RECYCLE.BIN", "System Volume Information", "Ableton Folder Info"];

/// Folders nested deeper than this below a root are not entered. Real
/// libraries are a handful of levels deep; this only bounds the damage of a
/// pathological tree.
const MAX_DEPTH: usize = 64;

/// The most header-reading threads. Reading headers is mostly waiting for
/// the disk, and more threads than this stop helping.
const MAX_READERS: usize = 8;

/// How many problems a scan describes in words; the rest are counted.
const MAX_PROBLEMS: usize = 50;

/// What a scan is doing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScanPhase {
    /// No scan is running.
    Idle,
    /// Walking the folders to see what is there.
    Listing,
    /// Reading the headers of new and changed audio files.
    Reading,
    /// Building the index.
    Indexing,
}

/// How a finished (or cancelled) scan went.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanSummary {
    /// Files in the library after the scan.
    pub files: usize,
    /// Audio files whose headers this scan had to read: the new and the
    /// changed. Zero when nothing changed since the last scan.
    pub headers_read: usize,
    /// Audio files in the library that cannot be decoded: damaged, or in a
    /// format this program does not read. Each is marked
    /// [`unreadable`](crate::Entry::unreadable); `trill_media::probe` says why
    /// for any one of them.
    pub unreadable: usize,
    pub elapsed: Duration,
    /// The scan was stopped before it finished. What it had learned by
    /// then is kept, unless it was stopped while still listing folders.
    pub cancelled: bool,
    /// Folders and files the scan could not get at, and why, in plain
    /// words: at most a few dozen, with the number left out in
    /// [`ScanSummary::problems_omitted`].
    pub problems: Vec<String>,
    pub problems_omitted: usize,
}

/// Counters a running scan keeps up to date for anyone watching.
#[derive(Default)]
pub(crate) struct Progress {
    phase: AtomicU8,
    pub folders: AtomicUsize,
    pub files: AtomicUsize,
    pub to_read: AtomicUsize,
    pub read: AtomicUsize,
}

impl Progress {
    pub(crate) fn phase(&self) -> ScanPhase {
        match self.phase.load(Ordering::Relaxed) {
            1 => ScanPhase::Listing,
            2 => ScanPhase::Reading,
            3 => ScanPhase::Indexing,
            _ => ScanPhase::Idle,
        }
    }

    pub(crate) fn set_phase(&self, phase: ScanPhase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }
}

/// Problems met during a scan, bounded so that a disk full of them cannot
/// fill memory with their descriptions.
#[derive(Default)]
struct Problems {
    notes: Vec<String>,
    omitted: usize,
}

impl Problems {
    fn note(&mut self, problem: impl FnOnce() -> String) {
        if self.notes.len() < MAX_PROBLEMS {
            self.notes.push(problem());
        } else {
            self.omitted += 1;
        }
    }
}

/// The scan was cancelled.
struct Cancelled;

/// A folder as found on disk.
struct Listed {
    path: PathBuf,
    /// Position of the parent in the listing; `None` for the root.
    parent: Option<usize>,
    /// Names of the folders from the root down to this one, for inference.
    names: Vec<String>,
    files: Vec<Entry>,
    /// Files in this folder and beneath it.
    total: usize,
}

/// What becomes of one library root in the new index.
enum Root {
    /// Its folders as just listed, in tree order.
    Listed(Vec<Listed>),
    /// It could not be read (an unplugged drive, say), so what the previous
    /// index knew about it is carried over untouched.
    Kept(FolderId),
}

/// One scan of a set of roots.
pub(crate) struct Scan<'a> {
    pub roots: &'a [PathBuf],
    /// The index to reuse what is still true from.
    pub previous: &'a Index,
    pub cancel: &'a AtomicBool,
    pub progress: &'a Progress,
}

impl Scan<'_> {
    /// Runs the scan. `early` is handed an index as soon as the folders
    /// have been listed, when there are headers still to read, so that a
    /// first scan shows names within moments instead of after everything.
    ///
    /// Returns the new index, or `None` if the scan was cancelled before it
    /// knew what was on disk, and an account of how it went.
    pub(crate) fn run(&self, early: &mut dyn FnMut(Index)) -> (Option<Index>, ScanSummary) {
        let started = Instant::now();
        let mut problems = Problems::default();
        let mut headers_read = 0;
        let index = self.scan(early, &mut problems, &mut headers_read);
        let cancelled = self.cancel.load(Ordering::Relaxed);
        let summary = ScanSummary {
            files: index.as_ref().map_or(self.previous.len(), Index::len),
            headers_read,
            unreadable: index
                .as_ref()
                .unwrap_or(self.previous)
                .entries()
                .iter()
                .filter(|entry| entry.unreadable)
                .count(),
            elapsed: started.elapsed(),
            cancelled,
            problems: problems.notes,
            problems_omitted: problems.omitted,
        };
        (index, summary)
    }

    fn scan(&self, early: &mut dyn FnMut(Index), problems: &mut Problems, headers_read: &mut usize) -> Option<Index> {
        self.progress.set_phase(ScanPhase::Listing);
        let mut roots = self.list_roots(problems).ok()?;

        // Audio files to read the header of: (root, folder, file).
        let mut unread: Vec<(usize, usize, usize)> = Vec::new();
        for (root_at, root) in roots.iter().enumerate() {
            let Root::Listed(folders) = root else { continue };
            for (folder_at, folder) in folders.iter().enumerate() {
                unread.extend(
                    folder
                        .files
                        .iter()
                        .enumerate()
                        .filter(|(_, file)| file.is_pending())
                        .map(|(at, _)| (root_at, folder_at, at)),
                );
            }
        }

        if !unread.is_empty() {
            self.progress.to_read.store(unread.len(), Ordering::Relaxed);
            if let Ok(index) = self.build(&roots) {
                early(index);
            }
            self.progress.set_phase(ScanPhase::Reading);
            let paths: Vec<&Path> = unread
                .iter()
                .filter_map(|&(root, folder, file)| match &roots[root] {
                    Root::Listed(folders) => Some(folders[folder].files[file].path.as_path()),
                    Root::Kept(_) => None,
                })
                .collect();
            let headers = self.read_headers(&paths);
            for ((root, folder, file), header) in unread.into_iter().zip(headers) {
                let (Root::Listed(folders), Some(header)) = (&mut roots[root], header) else { continue };
                *headers_read += 1;
                let entry = &mut folders[folder].files[file];
                match header {
                    Ok(facts) => entry.audio = Some(facts),
                    // The file is there but is not audio we can use. That
                    // is a fact about the file, not a problem with the scan
                    // (a library can hold thousands, such as the samples of
                    // another program's packs): it is marked, and not looked
                    // at again until it changes.
                    Err(MediaError::Unsupported { .. } | MediaError::Corrupt { .. } | MediaError::Empty { .. }) => {
                        entry.unreadable = true;
                    }
                    // The file could not be reached this time (in use,
                    // offline, just deleted). It stays unexamined, so the
                    // next scan tries again.
                    Err(error) => problems.note(|| error.to_string()),
                }
            }
        }

        self.progress.set_phase(ScanPhase::Indexing);
        self.build(&roots).ok()
    }

    /// Lists every root, in order.
    fn list_roots(&self, problems: &mut Problems) -> Result<Vec<Root>, Cancelled> {
        let mut roots = Vec::with_capacity(self.roots.len());
        for (position, root) in self.roots.iter().enumerate() {
            // A root inside another is already covered by it.
            if self.roots[..position].iter().any(|earlier| root.starts_with(earlier)) {
                continue;
            }
            let mut folders = Vec::new();
            let name =
                root.file_name().map_or_else(|| root.to_string_lossy(), |name| name.to_string_lossy()).into_owned();
            match self.list_folder(root.clone(), None, vec![name], 0, &mut folders, problems)? {
                Ok(()) => roots.push(Root::Listed(without_empty_folders(folders))),
                Err(error) => {
                    let known = self.previous.find_folder(root).filter(|id| self.previous.roots().contains(id));
                    problems.note(|| match known {
                        Some(_) => {
                            format!("cannot read '{}' ({error}); keeping what was known about it", root.display())
                        }
                        None => format!("cannot read '{}': {error}", root.display()),
                    });
                    roots.extend(known.map(Root::Kept));
                }
            }
        }
        Ok(roots)
    }

    /// Lists `path` and the folders beneath it into `out`, in tree order.
    /// The inner error is the reason `path` itself could not be read.
    fn list_folder(
        &self,
        path: PathBuf,
        parent: Option<usize>,
        names: Vec<String>,
        depth: usize,
        out: &mut Vec<Listed>,
        problems: &mut Problems,
    ) -> Result<std::io::Result<()>, Cancelled> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        let listing = match std::fs::read_dir(&path) {
            Ok(listing) => listing,
            Err(error) => return Ok(Err(error)),
        };
        let known = Known::of(self.previous, &path);
        let mut files: Vec<Entry> = Vec::new();
        let mut subfolders: Vec<OsString> = Vec::new();
        for item in listing {
            let item = match item {
                Ok(item) => item,
                Err(error) => {
                    problems.note(|| format!("cannot read part of '{}': {error}", path.display()));
                    continue;
                }
            };
            let name = item.file_name();
            if is_hidden(&name) {
                continue;
            }
            let Ok(file_type) = item.file_type() else { continue };
            if file_type.is_dir() {
                subfolders.push(name);
                continue;
            }
            let Some(kind) = EntryKind::of(Path::new(&name)) else { continue };
            // Links to folders are not followed, which rules out loops;
            // links to files are, so `metadata` rather than the listing's.
            let metadata = if file_type.is_symlink() { std::fs::metadata(item.path()) } else { item.metadata() };
            let Ok(metadata) = metadata.map_err(|error| {
                problems.note(|| format!("cannot examine '{}': {error}", item.path().display()));
            }) else {
                continue;
            };
            if metadata.is_file() {
                files.push(known.entry(path.join(&name), kind, metadata.len(), metadata.modified().ok()));
            }
        }
        files.sort_by(|a, b| natural_cmp(&a.name, &b.name).then_with(|| a.path.cmp(&b.path)));
        subfolders.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()).then_with(|| a.cmp(b)));

        self.progress.folders.fetch_add(1, Ordering::Relaxed);
        self.progress.files.fetch_add(files.len(), Ordering::Relaxed);
        let this = out.len();
        out.push(Listed { path: path.clone(), parent, names: names.clone(), total: files.len(), files });

        for name in subfolders {
            let subfolder = path.join(&name);
            if depth + 1 >= MAX_DEPTH {
                problems.note(|| format!("'{}' is nested too deeply to scan", subfolder.display()));
                continue;
            }
            let mut names = names.clone();
            names.push(name.to_string_lossy().into_owned());
            if let Err(error) = self.list_folder(subfolder.clone(), Some(this), names, depth + 1, out, problems)? {
                problems.note(|| format!("cannot read '{}': {error}", subfolder.display()));
            }
        }
        Ok(Ok(()))
    }

    /// Reads the headers of `paths` on several threads. An item is `None`
    /// if the scan was cancelled before its turn came.
    fn read_headers(&self, paths: &[&Path]) -> Vec<Option<Result<AudioFacts, MediaError>>> {
        let next = AtomicUsize::new(0);
        let readers =
            std::thread::available_parallelism().map_or(1, |count| count.get()).min(MAX_READERS).min(paths.len());
        let read_some = || {
            let mut read = Vec::new();
            while !self.cancel.load(Ordering::Relaxed) {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some(path) = paths.get(at) else { break };
                let facts = trill_media::probe(path).map(|info| AudioFacts {
                    frames: info.frames,
                    sample_rate: info.sample_rate,
                    channels: info.channels,
                });
                read.push((at, facts));
                self.progress.read.fetch_add(1, Ordering::Relaxed);
            }
            read
        };
        let mut headers: Vec<Option<Result<AudioFacts, MediaError>>> = paths.iter().map(|_| None).collect();
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..readers).map(|_| scope.spawn(read_some)).collect();
            // A reader that panicked took its results with it; those files
            // stay unexamined and are read by the next scan.
            for (at, facts) in workers.into_iter().filter_map(|worker| worker.join().ok()).flatten() {
                headers[at] = Some(facts);
            }
        });
        headers
    }

    /// Builds the index for `roots` as they stand, reading from names what
    /// they say about each file.
    fn build(&self, roots: &[Root]) -> Result<Index, OutOfOrder> {
        let files = roots
            .iter()
            .map(|root| match root {
                Root::Listed(folders) => folders.iter().map(|folder| folder.files.len()).sum(),
                Root::Kept(id) => self.previous.folder(*id).map_or(0, |folder| folder.all_files().len()),
            })
            .sum();
        let mut builder = IndexBuilder::with_capacity(files);
        for root in roots {
            let folders = match root {
                Root::Listed(folders) => folders,
                Root::Kept(id) => {
                    self.previous.copy_root(*id, &mut builder)?;
                    continue;
                }
            };
            let mut ids: Vec<FolderId> = Vec::with_capacity(folders.len());
            for folder in folders {
                let parent = match folder.parent {
                    Some(parent) => Some(*ids.get(parent).ok_or(OutOfOrder)?),
                    None => None,
                };
                ids.push(builder.folder(folder.path.clone(), parent)?);

                let names: Vec<&str> = folder.names.iter().map(String::as_str).collect();
                let from_folders = FolderReading::of(&names);
                let mut readings: Vec<(&str, NameReading)> = folder
                    .files
                    .iter()
                    .map(|file| (file.name.as_str(), NameReading::of(&file.name, &from_folders)))
                    .collect();
                discount_counting(&mut readings);
                for (file, (_, reading)) in folder.files.iter().zip(readings) {
                    let mut entry = file.clone();
                    // A project is not a sound; its name describes a song.
                    if entry.kind != EntryKind::Project {
                        let inferred = reading.finish(entry.seconds());
                        (entry.tempo, entry.key, entry.tags) = (inferred.tempo, inferred.key, inferred.tags);
                    }
                    builder.file(entry)?;
                }
            }
        }
        Ok(builder.finish())
    }
}

/// Files and folders that are not content: anything whose name starts with
/// a dot (which takes in the `._name` files macOS scatters through archives)
/// and the [`SKIPPED_FOLDERS`].
fn is_hidden(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || SKIPPED_FOLDERS.iter().any(|skipped| skipped.eq_ignore_ascii_case(&name))
}

/// Drops the folders with no library files anywhere beneath them, keeping
/// the root itself, so that the tree shows only places worth opening.
fn without_empty_folders(mut folders: Vec<Listed>) -> Vec<Listed> {
    // Children come after their parents, so going backwards adds each
    // folder's count to its parent after its own is complete.
    for at in (0..folders.len()).rev() {
        if let Some(parent) = folders[at].parent {
            folders[parent].total += folders[at].total;
        }
    }
    let mut new_position: Vec<Option<usize>> = Vec::with_capacity(folders.len());
    let mut kept = Vec::new();
    for mut folder in folders {
        if folder.total == 0 && folder.parent.is_some() {
            new_position.push(None);
            continue;
        }
        // A kept folder's parent holds at least the same files, so it was
        // kept too.
        folder.parent = folder.parent.and_then(|parent| new_position[parent]);
        new_position.push(Some(kept.len()));
        kept.push(folder);
    }
    kept
}

/// What the previous index knows about the files of one folder.
struct Known<'a> {
    files: &'a [Entry],
    /// By file name, for folders large enough that looking along the list
    /// for every file would add up.
    by_name: Option<HashMap<&'a OsStr, &'a Entry>>,
}

impl<'a> Known<'a> {
    const LOOKUP_FROM: usize = 32;

    fn of(previous: &'a Index, folder: &Path) -> Self {
        let files = previous
            .find_folder(folder)
            .and_then(|id| previous.folder(id))
            .map_or(&[][..], |folder| &previous.entries()[folder.files()]);
        let by_name = (files.len() >= Self::LOOKUP_FROM)
            .then(|| files.iter().filter_map(|entry| Some((entry.path.file_name()?, entry))).collect());
        Self { files, by_name }
    }

    /// The entry for a file just found on disk: what was known about it if
    /// it has not changed, and otherwise a fresh entry, which for audio
    /// means one whose header is yet to be read.
    fn entry(&self, path: PathBuf, kind: EntryKind, size: u64, modified: Option<SystemTime>) -> Entry {
        let name = path.file_name();
        let known = match &self.by_name {
            Some(by_name) => name.and_then(|name| by_name.get(name).copied()),
            None => self.files.iter().find(|entry| entry.path.file_name() == name),
        };
        let unchanged = known.filter(|known| known.kind == kind && known.size == size && known.modified == modified);
        Entry {
            name: display_name(&path),
            path,
            kind,
            size,
            modified,
            audio: unchanged.and_then(|known| known.audio),
            unreadable: unchanged.is_some_and(|known| known.unreadable),
            // Read from the names again by every scan, so that what is
            // inferred always follows the current rules and neighbours.
            tempo: None,
            key: None,
            tags: crate::tags::Tags::EMPTY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-channel, 16-bit WAV of silence, a tenth of a second long.
    fn wav() -> Vec<u8> {
        let data = 1_600u32;
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(36 + data).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
        bytes.extend_from_slice(&8_000u32.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(b"\x02\0\x10\0data");
        bytes.extend_from_slice(&data.to_le_bytes());
        bytes.resize(44 + data as usize, 0);
        bytes
    }

    fn tree(files: usize) -> (tempfile::TempDir, PathBuf) {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("Pack");
        for number in 0..files {
            let path = root.join(format!("Folder {}", number % 7)).join(format!("Kick {number}.wav"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, wav()).unwrap();
        }
        (folder, root)
    }

    #[test]
    fn cancelled_while_listing_nothing_changes() {
        let (_folder, root) = tree(20);
        let roots = [root];
        let (previous, cancel, progress) = (Index::default(), AtomicBool::new(true), Progress::default());
        let scan = Scan { roots: &roots, previous: &previous, cancel: &cancel, progress: &progress };
        let mut early = 0;
        let (index, summary) = scan.run(&mut |_| early += 1);
        assert!(index.is_none(), "an incomplete listing is not an index");
        assert_eq!(early, 0);
        assert!(summary.cancelled);
        assert_eq!((summary.files, summary.headers_read), (0, 0));
    }

    #[test]
    fn cancelled_while_reading_keeps_the_listing_and_the_next_scan_reads_the_rest() {
        let (_folder, root) = tree(200);
        let roots = [root];
        let (previous, cancel, progress) = (Index::default(), AtomicBool::new(false), Progress::default());
        let scan = Scan { roots: &roots, previous: &previous, cancel: &cancel, progress: &progress };

        // The early index arrives once the folders are listed and before
        // any header is read; cancelling there stops the scan at that point.
        let mut early: Option<Index> = None;
        let (index, summary) = scan.run(&mut |listed| {
            early = Some(listed);
            cancel.store(true, Ordering::Relaxed);
        });
        let early = early.expect("the listing is shown before headers are read");
        assert_eq!(early.len(), 200);
        assert!(early.entries().iter().all(Entry::is_pending));
        assert_eq!(early.entries()[0].tags.len(), 2, "names are read even before headers are");
        assert_eq!((progress.to_read.load(Ordering::Relaxed), progress.read.load(Ordering::Relaxed)), (200, 0));

        let index = index.expect("what was learned is kept");
        assert!(summary.cancelled);
        assert_eq!((summary.files, summary.headers_read, summary.unreadable), (200, 0, 0));
        assert!(index.entries().iter().all(Entry::is_pending));

        // Carrying on from the cancelled scan reads exactly what is unread.
        let (cancel, progress) = (AtomicBool::new(false), Progress::default());
        let resumed = Scan { roots: &roots, previous: &index, cancel: &cancel, progress: &progress };
        let (index, summary) = resumed.run(&mut |_| {});
        let index = index.unwrap();
        assert!(!summary.cancelled);
        assert_eq!((summary.files, summary.headers_read), (200, 200));
        assert!(index.entries().iter().all(|entry| entry.audio.is_some_and(|audio| audio.sample_rate == 8_000)));
        assert_eq!(progress.phase(), ScanPhase::Indexing);
        assert_eq!((progress.folders.load(Ordering::Relaxed), progress.files.load(Ordering::Relaxed)), (8, 200));

        // With everything known, a scan has nothing to show early.
        let (cancel, progress) = (AtomicBool::new(false), Progress::default());
        let again = Scan { roots: &roots, previous: &index, cancel: &cancel, progress: &progress };
        let mut early = 0;
        let (unchanged, summary) = again.run(&mut |_| early += 1);
        assert_eq!((early, summary.headers_read), (0, 0));
        assert_eq!(unchanged.unwrap().entries(), index.entries());
    }

    #[test]
    fn hidden_and_housekeeping_folders_are_passed_over() {
        for name in
            [".git", ".DS_Store", "._Kick.wav", "__MACOSX", "$RECYCLE.BIN", "$Recycle.Bin", "Ableton Folder Info"]
        {
            assert!(is_hidden(OsStr::new(name)), "{name}");
        }
        for name in ["Kicks", "_underscore", "Ableton", "a.b"] {
            assert!(!is_hidden(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn a_tree_too_deep_is_cut_off_not_followed_forever() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("Deep");
        let mut deepest = root.clone();
        for _ in 0..MAX_DEPTH + 3 {
            deepest.push("d");
        }
        if std::fs::create_dir_all(&deepest).is_err() {
            println!("SKIPPED: this file system cannot nest folders {} deep", MAX_DEPTH + 3);
            return;
        }
        std::fs::write(root.join("d/d/Kick.wav"), wav()).unwrap();
        let _ = std::fs::write(deepest.join("Unreachable.wav"), wav());
        let roots = [root];
        let (previous, cancel, progress) = (Index::default(), AtomicBool::new(false), Progress::default());
        let (index, summary) =
            Scan { roots: &roots, previous: &previous, cancel: &cancel, progress: &progress }.run(&mut |_| {});
        assert_eq!(index.unwrap().len(), 1);
        assert!(summary.problems.iter().any(|problem| problem.contains("nested too deeply")), "{:?}", summary.problems);
    }

    #[test]
    fn problems_are_counted_once_there_are_too_many_to_describe() {
        let mut problems = Problems::default();
        for number in 0..MAX_PROBLEMS + 25 {
            problems.note(|| format!("problem {number}"));
        }
        assert_eq!((problems.notes.len(), problems.omitted), (MAX_PROBLEMS, 25));
    }
}
