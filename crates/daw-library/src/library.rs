//! The library: the roots the user chose, the index of what is in them,
//! and the background scans that keep the two in step.

use crate::Entry;
use crate::index::{EntryId, Index};
use crate::scan::{Progress, Scan, ScanPhase, ScanSummary};
use crate::search::{Query, search};
use crate::settings::{LibrarySettings, cache_dir};
use crate::store;
use parking_lot::{Condvar, Mutex, RwLock};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How many recently used files are remembered.
const RECENT_LIMIT: usize = 50;

/// The sound library.
///
/// Every method returns at once: scanning happens on a background thread,
/// and readers work from a snapshot of the index ([`Library::index`]) that
/// a scan replaces rather than alters. Poll [`Library::generation`] to
/// learn when there is something new to show.
///
/// The library keeps the index on disk between runs. Roots, favorites and
/// recently used files belong to the user's settings instead: read them
/// with [`Library::settings`] and save them with the rest.
pub struct Library {
    shared: Arc<Shared>,
    /// The thread of the scan started last.
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct Shared {
    index: RwLock<Arc<Index>>,
    state: Mutex<State>,
    /// Notified whenever a scan ends.
    scan_ended: Condvar,
    /// Advances whenever anything a view shows may have changed.
    generation: AtomicU64,
    index_path: Option<PathBuf>,
}

struct State {
    roots: Vec<PathBuf>,
    favorites: Vec<PathBuf>,
    /// The same paths as `favorites`, for answering "is this one?" quickly.
    favorite_set: HashSet<PathBuf>,
    /// Most recently used first.
    recent: Vec<PathBuf>,
    /// Counts scans started. A scan publishes its results only while it is
    /// still the latest, so one that was superseded cannot overwrite the
    /// work of its successor.
    scans_started: u64,
    running: Option<Running>,
    last_scan: Option<ScanSummary>,
    index_problem: Option<String>,
}

/// The scan in progress.
struct Running {
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
}

/// What the library is doing, for a status line or progress bar.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanStatus {
    pub phase: ScanPhase,
    /// Folders listed so far by the running scan.
    pub folders: usize,
    /// Files found so far by the running scan.
    pub files: usize,
    /// Audio files whose headers the running scan has read, out of
    /// [`ScanStatus::headers_to_read`].
    pub headers_read: usize,
    pub headers_to_read: usize,
    /// How the last scan that ended went.
    pub last: Option<ScanSummary>,
}

impl ScanStatus {
    pub fn is_scanning(&self) -> bool {
        self.phase != ScanPhase::Idle
    }
}

/// The result of a search: the matching entries, best first, together with
/// the index they belong to.
pub struct SearchResults {
    index: Arc<Index>,
    ids: Vec<EntryId>,
    total: usize,
}

impl SearchResults {
    /// The index the results refer to.
    pub fn index(&self) -> &Arc<Index> {
        &self.index
    }

    /// The matches, best first, up to the query's limit.
    pub fn ids(&self) -> &[EntryId] {
        &self.ids
    }

    /// The matches, best first, up to the query's limit.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &Entry> {
        self.ids.iter().map(|id| &self.index.entries()[id.index()])
    }

    /// Number of matches returned.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Number of matches there were before the query's limit cut them off.
    pub fn total(&self) -> usize {
        self.total
    }
}

impl Library {
    /// Opens the library described by `settings`.
    ///
    /// If `index_path` names an index saved by an earlier run, it is loaded
    /// before this returns (a hundred thousand files take about a tenth of
    /// a second), so the library is searchable immediately. A file that is
    /// damaged or from another version is ignored; [`Library::index_problem`]
    /// says so. Nothing is scanned until [`Library::rescan`] is called,
    /// which a program normally does right away to pick up what changed
    /// while it was not running.
    pub fn open(settings: &LibrarySettings, index_path: Option<PathBuf>) -> Library {
        let mut roots: Vec<PathBuf> = Vec::new();
        for root in &settings.roots {
            add_to_roots(&mut roots, absolute(root));
        }
        let (index, index_problem) = match index_path.as_deref().map(store::load) {
            Some(Ok(index)) => (index, None),
            Some(Err(store::LoadError::Missing)) | None => (Index::default(), None),
            Some(Err(problem)) => (Index::default(), Some(problem.to_string())),
        };
        // The saved index may cover roots that have since been removed.
        let index = if index
            .roots()
            .iter()
            .all(|id| index.folder(*id).is_some_and(|root| roots.iter().any(|r| r == root.path())))
        {
            index
        } else {
            index.retain_roots(|root| roots.iter().any(|known| known == root))
        };

        let mut state = State {
            roots,
            favorites: Vec::new(),
            favorite_set: HashSet::new(),
            recent: settings.recent.iter().take(RECENT_LIMIT).cloned().collect(),
            scans_started: 0,
            running: None,
            last_scan: None,
            index_problem,
        };
        for favorite in &settings.favorites {
            state.set_favorite(favorite, true);
        }
        let shared = Shared {
            index: RwLock::new(Arc::new(index)),
            state: Mutex::new(state),
            scan_ended: Condvar::new(),
            generation: AtomicU64::new(0),
            index_path,
        };
        Library { shared: Arc::new(shared), worker: Mutex::new(None) }
    }

    /// Where the index is kept when the caller has no reason to put it
    /// elsewhere: `library-index.json` in the per-user cache directory.
    pub fn default_index_path() -> Option<PathBuf> {
        cache_dir().map(|folder| folder.join("library-index.json"))
    }

    /// The index as it stands. Cheap; hold on to it for as long as a
    /// consistent view is needed, and fetch it again when
    /// [`Library::generation`] has moved on.
    pub fn index(&self) -> Arc<Index> {
        self.shared.index.read().clone()
    }

    /// A number that changes whenever the index, the roots, the favorites
    /// or the recently used files do.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }

    /// Why the saved index was not used, if it was not: it was damaged or
    /// written by another version. The next scan rebuilds it.
    pub fn index_problem(&self) -> Option<String> {
        self.shared.state.lock().index_problem.clone()
    }

    /// Searches the library. Fast enough to call on every keystroke.
    pub fn search(&self, query: &Query) -> SearchResults {
        let index = self.index();
        let favorites: Vec<EntryId> = if query.favorites_only {
            let state = self.shared.state.lock();
            let mut ids: Vec<EntryId> = state.favorites.iter().filter_map(|path| index.find(path)).collect();
            ids.sort_unstable();
            ids
        } else {
            Vec::new()
        };
        let (ids, total) = search(&index, query, &favorites);
        SearchResults { index, ids, total }
    }

    // ---- roots ---------------------------------------------------------

    /// The folders the library covers.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.shared.state.lock().roots.clone()
    }

    /// Adds a folder to the library and starts scanning it. Returns `false`
    /// if the folder is already covered, as itself or inside another root.
    /// A root that contains existing roots takes their place.
    pub fn add_root(&self, path: &Path) -> bool {
        let added = add_to_roots(&mut self.shared.state.lock().roots, absolute(path));
        if added {
            self.shared.changed();
            self.rescan();
        }
        added
    }

    /// Takes a folder out of the library. Its files leave the index at
    /// once; nothing on disk is touched. Returns `false` if it was not a
    /// root.
    pub fn remove_root(&self, path: &Path) -> bool {
        let path = absolute(path);
        let scanning = {
            let mut state = self.shared.state.lock();
            let Some(position) = state.roots.iter().position(|root| *root == path) else { return false };
            state.roots.remove(position);
            // Built before the index is locked for writing, so that readers
            // wait only for the swap. No scan can publish in between: that
            // takes the state lock, which is held here.
            let current = self.shared.index.read().clone();
            let remaining = Arc::new(current.retain_roots(|root| root != path));
            *self.shared.index.write() = remaining;
            state.running.is_some()
        };
        self.shared.changed();
        if scanning {
            // The running scan was working from the old roots.
            self.rescan();
        } else {
            self.shared.save_index();
        }
        true
    }

    // ---- scanning ------------------------------------------------------

    /// Starts scanning the roots on a background thread and returns at
    /// once. A scan already running is cancelled and replaced.
    ///
    /// Only files that are new or whose size or modified time changed are
    /// opened; for a library that has not changed the scan is a listing of
    /// its folders. The index is saved when the scan ends.
    pub fn rescan(&self) {
        let (job, roots, cancel, progress) = {
            let mut state = self.shared.state.lock();
            if let Some(running) = &state.running {
                running.cancel.store(true, Ordering::Relaxed);
            }
            state.scans_started += 1;
            let running = Running { cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(Progress::default()) };
            let handles = (state.scans_started, state.roots.clone(), running.cancel.clone(), running.progress.clone());
            running.progress.set_phase(ScanPhase::Listing);
            state.running = Some(running);
            handles
        };
        let shared = self.shared.clone();
        let spawned = std::thread::Builder::new()
            .name("library-scan".into())
            .spawn(move || shared.run_scan(job, &roots, &cancel, &progress));
        match spawned {
            // The thread of a superseded scan is left to finish by itself;
            // it has been told to stop and can no longer publish anything.
            Ok(handle) => *self.worker.lock() = Some(handle),
            Err(_) => self.shared.end_scan(job, None),
        }
    }

    /// Stops the running scan, if there is one. What it has learned so far
    /// is kept, and the next scan carries on from there.
    pub fn cancel_scan(&self) {
        if let Some(running) = &self.shared.state.lock().running {
            running.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// What the library is doing and how the last scan went.
    pub fn scan_status(&self) -> ScanStatus {
        let state = self.shared.state.lock();
        let (phase, folders, files, headers_read, headers_to_read) = match &state.running {
            Some(Running { progress, .. }) => (
                progress.phase(),
                progress.folders.load(Ordering::Relaxed),
                progress.files.load(Ordering::Relaxed),
                progress.read.load(Ordering::Relaxed),
                progress.to_read.load(Ordering::Relaxed),
            ),
            None => (ScanPhase::Idle, 0, 0, 0, 0),
        };
        ScanStatus { phase, folders, files, headers_read, headers_to_read, last: state.last_scan.clone() }
    }

    /// Blocks until no scan is running or `timeout` has passed, and returns
    /// whether the library is idle. For tests and command-line tools; an
    /// interactive program watches [`Library::scan_status`] instead.
    pub fn wait_until_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock();
        while state.running.is_some() {
            if self.shared.scan_ended.wait_until(&mut state, deadline).timed_out() {
                return state.running.is_none();
            }
        }
        true
    }

    // ---- favorites and recently used -----------------------------------

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.shared.state.lock().favorite_set.contains(path)
    }

    /// Marks or unmarks a file as a favorite. Favorites are remembered by
    /// path, so they survive rescans, and a favorite whose file has gone
    /// simply matches nothing until it returns.
    pub fn set_favorite(&self, path: &Path, favorite: bool) {
        if self.shared.state.lock().set_favorite(path, favorite) {
            self.shared.changed();
        }
    }

    /// The favorites, in the order they were added.
    pub fn favorites(&self) -> Vec<PathBuf> {
        self.shared.state.lock().favorites.clone()
    }

    /// Records that a file was used (auditioned, dragged into a project),
    /// moving it to the front of the recently used list.
    pub fn mark_used(&self, path: &Path) {
        {
            let mut state = self.shared.state.lock();
            if state.recent.first().is_some_and(|latest| latest == path) {
                return;
            }
            state.recent.retain(|used| used != path);
            state.recent.insert(0, path.to_path_buf());
            state.recent.truncate(RECENT_LIMIT);
        }
        self.shared.changed();
    }

    /// Recently used files, latest first.
    pub fn recent(&self) -> Vec<PathBuf> {
        self.shared.state.lock().recent.clone()
    }

    /// The roots, favorites and recently used files, in the form the
    /// program's settings store them.
    pub fn settings(&self) -> LibrarySettings {
        let state = self.shared.state.lock();
        LibrarySettings { roots: state.roots.clone(), favorites: state.favorites.clone(), recent: state.recent.clone() }
    }
}

impl Drop for Library {
    /// Stops the running scan and waits for it to save what it has, so
    /// that the next launch carries on from there.
    fn drop(&mut self) {
        self.cancel_scan();
        if let Some(worker) = self.worker.lock().take() {
            let _ = worker.join();
        }
    }
}

impl State {
    /// Returns whether anything changed.
    fn set_favorite(&mut self, path: &Path, favorite: bool) -> bool {
        if favorite {
            let added = self.favorite_set.insert(path.to_path_buf());
            if added {
                self.favorites.push(path.to_path_buf());
            }
            added
        } else {
            let removed = self.favorite_set.remove(path);
            if removed {
                self.favorites.retain(|known| known != path);
            }
            removed
        }
    }
}

impl Shared {
    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// The body of a scan thread.
    fn run_scan(&self, job: u64, roots: &[PathBuf], cancel: &AtomicBool, progress: &Progress) {
        let previous = self.index.read().clone();
        let scan = Scan { roots, previous: &previous, cancel, progress };
        let (index, summary) = scan.run(&mut |early| {
            self.publish(job, early);
        });
        if let Some(index) = index
            && self.publish(job, index)
        {
            self.save_index();
        }
        self.end_scan(job, Some(summary));
    }

    /// Makes `index` the library's index, unless the scan that built it has
    /// been superseded. Returns whether it was published.
    fn publish(&self, job: u64, index: Index) -> bool {
        // Holding the state lock keeps a newer scan from starting between
        // the check and the swap.
        let state = self.state.lock();
        if state.scans_started != job {
            return false;
        }
        *self.index.write() = Arc::new(index);
        drop(state);
        self.changed();
        true
    }

    fn end_scan(&self, job: u64, summary: Option<ScanSummary>) {
        let mut state = self.state.lock();
        if state.scans_started == job {
            state.running = None;
            if summary.is_some() {
                state.last_scan = summary;
                state.index_problem = None;
            }
            drop(state);
            self.changed();
            self.scan_ended.notify_all();
        }
    }

    /// Writes the index to disk. Failing to is not an error worth
    /// interrupting anyone for: the cost is a scan at the next launch.
    fn save_index(&self) {
        if let Some(path) = &self.index_path {
            let index = self.index.read().clone();
            if let Err(error) = store::save(path, &index) {
                log::warn!("could not save the library index to '{}': {error}", path.display());
            }
        }
    }
}

/// `path` made absolute against the working directory, without touching the
/// disk, so that the same folder is not listed under two spellings.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `inner` is `outer` or lies inside it. Where file names are not
/// case-sensitive, neither is this.
fn covers(outer: &Path, inner: &Path) -> bool {
    let mut inner = inner.components();
    outer.components().all(|part| {
        inner.next().is_some_and(|other| {
            if cfg!(any(windows, target_os = "macos")) {
                part.as_os_str().eq_ignore_ascii_case(other.as_os_str())
            } else {
                part == other
            }
        })
    })
}

/// Adds `root` to `roots`, keeping them free of duplicates and of roots
/// inside other roots. Returns `false` if `root` was already covered.
fn add_to_roots(roots: &mut Vec<PathBuf>, root: PathBuf) -> bool {
    if root.as_os_str().is_empty() || roots.iter().any(|known| covers(known, &root)) {
        return false;
    }
    roots.retain(|known| !covers(&root, known));
    roots.push(root);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_never_overlap() {
        let mut roots = Vec::new();
        assert!(add_to_roots(&mut roots, "/samples/drums".into()));
        assert!(add_to_roots(&mut roots, "/samples/bass".into()));
        assert!(!add_to_roots(&mut roots, "/samples/drums".into()), "already there");
        assert!(!add_to_roots(&mut roots, "/samples/drums/kicks".into()), "inside a root");
        assert!(!add_to_roots(&mut roots, "".into()));
        assert!(add_to_roots(&mut roots, "/samples/drums2".into()), "a name that merely starts the same");
        assert!(add_to_roots(&mut roots, "/samples".into()), "a folder holding roots replaces them");
        assert_eq!(roots, [PathBuf::from("/samples")]);
    }

    #[test]
    fn covering_follows_the_file_system_on_letter_case() {
        assert!(covers(Path::new("/a/b"), Path::new("/a/b/c")));
        assert!(covers(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!covers(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(!covers(Path::new("/a/b"), Path::new("/a/bc")));
        assert_eq!(covers(Path::new("/Samples"), Path::new("/samples/x")), cfg!(any(windows, target_os = "macos")));
    }
}
