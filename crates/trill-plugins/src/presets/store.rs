//! The presets of one format's plugins, as far as they have been read:
//! who asks gets what is known at once, and the reading happens on a
//! thread of the store's own.

use super::cache::{self, Entry};
use super::{Found, Line, PRESETS_ARGUMENT, Sink, identify, walk};
use crate::main_thread::MainThread;
use crate::plugin_thread::PluginThread;
use crate::scan::Scanner;
use crate::scan::cache::FileStamp;
use crate::scan::child::{self, Finished};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use trill_core::presets::{PresetInfo, PresetList, PresetStatus};

/// How many times longer reading a plugin's presets may take than
/// scanning a plugin file.
pub(crate) const PRESET_PATIENCE: u32 = 6;

/// What tells one plugin format from another while its presets are read.
pub(crate) struct PresetFormat {
    /// The format's URI scheme, which is also what the scanner process is
    /// told the format by.
    pub scheme: &'static str,
    /// Finds the presets that are files in the folders the format keeps
    /// presets in (`folders`). No code of the plugin's runs for this.
    pub read_files: fn(&PluginRef, &[PathBuf], &mut dyn Sink),
    /// Finds the presets only the plugin's own code can tell of, in this
    /// process: what is done on the plugin main thread when there is no
    /// scanner process to do it in.
    pub read_in_thread: fn(&mut PluginThread, &Path, &str, &mut dyn Sink) -> Result<(), String>,
}

/// The plugin whose presets are wanted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginRef {
    /// Its device URI: what its cache entry is filed under.
    pub uri: String,
    /// Its id within its format.
    pub id: String,
    /// The plugin file it was found in.
    pub path: PathBuf,
    pub name: String,
    pub vendor: String,
}

/// Where and how a store reads presets.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub scanner: Scanner,
    /// The folder the cache entries are kept in; `None` disables caching.
    pub cache_folder: Option<PathBuf>,
    /// Longest the reading of one plugin's presets may take.
    pub timeout: Duration,
    /// The folders the format keeps preset files in, for
    /// [`PresetFormat::read_files`].
    pub folders: Vec<PathBuf>,
}

/// What is known of one plugin's presets, and whether more is coming.
struct Listing {
    list: PresetList,
    /// Asked to be read again while it was being read.
    again: bool,
}

struct Shared {
    format: &'static PresetFormat,
    settings: Settings,
    main: MainThread<PluginThread>,
    /// By plugin id.
    listings: Mutex<HashMap<String, Listing>>,
    /// Signalled whenever a listing changes.
    changed: Condvar,
    /// Held while reading: one plugin at a time. Reading is dominated by
    /// the disk, and a plugin's code is not assumed to like company.
    turn: Mutex<()>,
    /// Set when the store is dropped: whatever is being read is no longer
    /// wanted.
    closed: AtomicBool,
}

/// The presets of one format's plugins. See the module documentation.
pub(crate) struct PresetStore {
    shared: Arc<Shared>,
}

impl PresetStore {
    pub(crate) fn new(format: &'static PresetFormat, settings: Settings, main: MainThread<PluginThread>) -> Self {
        let shared = Shared {
            format,
            settings,
            main,
            listings: Mutex::new(HashMap::new()),
            changed: Condvar::new(),
            turn: Mutex::new(()),
            closed: AtomicBool::new(false),
        };
        Self { shared: Arc::new(shared) }
    }

    /// The presets of `plugin` as far as they are known. Never waits: the
    /// first request starts the reading and returns an empty list that
    /// says so.
    pub(crate) fn list(&self, plugin: &PluginRef) -> PresetList {
        let mut listings = self.shared.listings();
        if let Some(listing) = listings.get(&plugin.id) {
            return listing.list.clone();
        }
        let list = PresetList::reading(Vec::new());
        listings.insert(plugin.id.clone(), Listing { list: list.clone(), again: false });
        drop(listings);
        self.start(plugin.clone(), false);
        list
    }

    /// [`list`](Self::list), waiting up to `timeout` for a list that is
    /// being read to be complete.
    pub(crate) fn wait(&self, plugin: &PluginRef, timeout: Duration) -> PresetList {
        let deadline = Instant::now() + timeout;
        let mut list = self.list(plugin);
        let mut listings = self.shared.listings();
        loop {
            if let Some(listing) = listings.get(&plugin.id) {
                list = listing.list.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !list.is_reading() || remaining.is_zero() {
                return list;
            }
            listings = self.shared.changed.wait_timeout(listings, remaining).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Has the presets of `plugin` read again, whatever the cache says.
    /// What is known stays listed meanwhile.
    pub(crate) fn refresh(&self, plugin: &PluginRef) {
        let mut listings = self.shared.listings();
        match listings.get_mut(&plugin.id) {
            // The reading under way may have passed what has changed
            // since; it is followed by another.
            Some(listing) if listing.list.is_reading() => listing.again = true,
            Some(listing) => {
                listing.list.status = PresetStatus::Reading;
                drop(listings);
                self.shared.changed.notify_all();
                self.start(plugin.clone(), true);
            }
            None => {
                listings.insert(plugin.id.clone(), Listing { list: PresetList::reading(Vec::new()), again: false });
                drop(listings);
                self.start(plugin.clone(), true);
            }
        }
    }

    /// Reads the presets of `plugin` on a thread of its own.
    fn start(&self, plugin: PluginRef, fresh: bool) {
        let shared = self.shared.clone();
        let id = plugin.id.clone();
        let spawned =
            std::thread::Builder::new().name("trill-presets".into()).spawn(move || shared.read(plugin, fresh));
        if let Err(error) = spawned {
            let failed = PresetStatus::Failed(format!("cannot start a thread to read the presets on: {error}"));
            let mut listings = self.shared.listings();
            if let Some(listing) = listings.get_mut(&id) {
                listing.list.status = failed;
            }
            drop(listings);
            self.shared.changed.notify_all();
        }
    }
}

impl Drop for PresetStore {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
    }
}

impl Shared {
    fn listings(&self) -> MutexGuard<'_, HashMap<String, Listing>> {
        self.listings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Reads the presets of `plugin` and makes them its listing, again
    /// for as long as someone asks for that meanwhile.
    fn read(&self, plugin: PluginRef, mut fresh: bool) {
        let _turn = self.turn.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let list = if self.closed() {
                PresetList {
                    status: PresetStatus::Failed("the plugin host was shut down".into()),
                    presets: Vec::new().into(),
                }
            } else {
                self.read_once(&plugin, fresh)
            };
            let mut listings = self.listings();
            let Some(listing) = listings.get_mut(&plugin.id) else { return };
            if std::mem::take(&mut listing.again) && !self.closed() {
                listing.list = PresetList::reading(list.presets);
                fresh = true;
                continue;
            }
            listing.list = list;
            drop(listings);
            self.changed.notify_all();
            return;
        }
    }

    /// Shows `presets` as what is known of `plugin` while its presets are
    /// still being read.
    fn show(&self, plugin: &PluginRef, presets: Vec<PresetInfo>) {
        if let Some(listing) = self.listings().get_mut(&plugin.id) {
            listing.list = PresetList::reading(presets);
        }
        self.changed.notify_all();
    }

    /// The presets of `plugin`: from the cache if that is still good and
    /// `fresh` does not rule it out, and otherwise read anew.
    fn read_once(&self, plugin: &PluginRef, fresh: bool) -> PresetList {
        // Without a stamp there is nothing to tell an entry's age by.
        let stamp = FileStamp::of(&plugin.path).ok();
        let cache_file = self.settings.cache_folder.as_ref().map(|folder| cache::file(folder, &plugin.uri));
        let cached = match (fresh, stamp, &cache_file) {
            (false, Some(stamp), Some(file)) => cache::load(file, &plugin.path, stamp),
            _ => None,
        };
        if let Some(entry) = cached {
            // What was found last time is worth showing while the folders
            // are looked over; it is very likely what will be found.
            self.show(plugin, entry.presets.clone());
            if walk::fingerprint(&entry.watch) == entry.fingerprint {
                return listed(entry.presets, entry.failure);
            }
            log::debug!("the preset folders of '{}' have changed; reading its presets again", plugin.uri);
        }

        let started = Instant::now();
        let mut found = Found::default();
        (self.format.read_files)(plugin, &self.settings.folders, &mut found);
        let Outcome { failure, lasting } = self.run_plugin_code(plugin, &mut found);
        let presets = identify(found.presets);
        match &failure {
            None => log::info!(
                "read {} presets of '{}' in {:.1} s",
                presets.len(),
                plugin.uri,
                started.elapsed().as_secs_f64()
            ),
            Some(reason) => {
                log::warn!("reading the presets of '{}' failed ({} were found): {reason}", plugin.uri, presets.len())
            }
        }
        if let (true, Some(stamp), Some(file)) = (lasting, stamp, &cache_file) {
            let fingerprint = walk::fingerprint(&found.watch);
            let entry = Entry::new(&plugin.path, stamp, found.watch, fingerprint, failure.clone(), presets.clone());
            if let Err(error) = cache::save(file, &entry) {
                log::warn!("cannot write the preset cache '{}': {error}", file.display());
            }
        }
        listed(presets, failure)
    }

    /// Has the plugin's own code tell of its presets: in a scanner
    /// process, or on the plugin main thread if there is none to be had.
    fn run_plugin_code(&self, plugin: &PluginRef, found: &mut Found) -> Outcome {
        match self.settings.scanner.resolve() {
            Some(scanner) => {
                let arguments: [&OsStr; 4] = [
                    OsStr::new(PRESETS_ARGUMENT),
                    OsStr::new(self.format.scheme),
                    plugin.path.as_os_str(),
                    OsStr::new(&plugin.id),
                ];
                match child::run_process(&scanner, &arguments, self.settings.timeout, Some(&self.closed)) {
                    Ok(finished) => interpret(&finished, self.settings.timeout, found),
                    // Says nothing about the plugin.
                    Err(reason) => Outcome { failure: Some(reason), lasting: false },
                }
            }
            None => {
                let (read, path, id) = (self.format.read_in_thread, plugin.path.clone(), plugin.id.clone());
                let read = move |thread: &mut PluginThread| {
                    let mut found = Found::default();
                    let result = read(thread, &path, &id, &mut found);
                    (found, result)
                };
                match self.main.call(self.settings.timeout, read) {
                    Ok((part, result)) => {
                        found.presets(part.presets);
                        part.watch.into_iter().for_each(|watch| found.watch(watch));
                        Outcome { failure: result.err(), lasting: true }
                    }
                    Err(error) => Outcome { failure: Some(error.to_string()), lasting: false },
                }
            }
        }
    }
}

/// How reading with a plugin's own code ended.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    /// Why it did not get to the end, if it did not.
    failure: Option<String>,
    /// Whether the outcome will be the same next time, and so is worth
    /// remembering. A plugin that crashes does so again; a scanner that
    /// could not be started may start tomorrow.
    lasting: bool,
}

fn listed(presets: Vec<PresetInfo>, failure: Option<String>) -> PresetList {
    PresetList { status: failure.map_or(PresetStatus::Ready, PresetStatus::Failed), presets: presets.into() }
}

/// Turns what a scanner process printed while reading presets into what
/// was found, and how it ended. Whatever the process printed before a
/// plugin took it down counts.
fn interpret(finished: &Finished, timeout: Duration, found: &mut Found) -> Outcome {
    let (mut done, mut failed) = (false, None);
    let before = found.presets.len();
    for line in String::from_utf8_lossy(&finished.stdout).lines() {
        // Plugins print to standard output too; what is no line of the
        // reader's is theirs.
        let Ok(line) = serde_json::from_str::<Line>(line.trim()) else { continue };
        found.presets(line.presets);
        line.watch.into_iter().for_each(|watch| found.watch(watch));
        done |= line.done;
        failed = line.failed.or(failed);
    }
    let read = found.presets.len() - before;
    let cut_short = |what: String| match read {
        0 => what,
        read => format!("{what}; {read} presets were read before that"),
    };
    let failure = match (done, failed, finished.status) {
        // The presets were all read; a plugin that then crashes or hangs
        // while unloading has still been read.
        (true, _, _) => None,
        (false, Some(reason), _) => Some(reason),
        (false, None, None) if finished.cancelled => {
            return Outcome { failure: Some("reading the presets was stopped".into()), lasting: false };
        }
        (false, None, None) => {
            Some(cut_short(format!("reading the presets timed out after {:.0} s", timeout.as_secs_f64())))
        }
        (false, None, Some(status)) if status.success() => Some(cut_short("the preset reader stopped early".into())),
        (false, None, Some(status)) => Some(cut_short(format!(
            "the plugin failed while its presets were read: {}",
            child::explain_exit(status, &finished.stderr)
        ))),
    };
    Outcome { failure, lasting: true }
}

#[cfg(test)]
mod tests {
    use super::super::Watch;
    use super::*;
    use crate::plugin_thread::plugin_thread;
    use crate::test_support::{TestDir, eventually};
    use std::process::ExitStatus;

    /// How often the fake format's plugin code ran, by plugin id.
    static READS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The file beside a fake plugin that says how many milliseconds its
    /// code takes.
    const SLOWNESS: &str = "slowness";

    fn reads(id: &str) -> usize {
        READS.lock().unwrap().iter().filter(|read| *read == id).count()
    }

    fn preset(name: &str, locator: &str) -> PresetInfo {
        PresetInfo { name: name.into(), locator: locator.into(), ..PresetInfo::default() }
    }

    /// A format whose plugins keep `*.patch` files in a folder beside
    /// them: each is a preset, found by "the plugin's code", which fails
    /// on a file named `Rotten.patch` after telling of the ones before
    /// it. Files named `*.filepreset` in the format's folders are presets
    /// found without it.
    static FAKE: PresetFormat = PresetFormat {
        scheme: "fake",
        read_files: |_plugin, folders, sink| {
            for folder in folders {
                let extensions = vec!["filepreset".to_string()];
                sink.watch(Watch { path: folder.clone(), extensions: extensions.clone() });
                for file in walk::files(folder, &extensions) {
                    let name = file.file_stem().unwrap().to_string_lossy().into_owned();
                    sink.presets(vec![preset(&name, &format!("file:{name}"))]);
                }
            }
        },
        read_in_thread: |_thread, path, id, sink| {
            READS.lock().unwrap().push(id.to_string());
            let slowness = std::fs::read_to_string(path.with_file_name(SLOWNESS)).unwrap_or_default();
            std::thread::sleep(Duration::from_millis(slowness.parse().unwrap_or(0)));
            let extensions = vec!["patch".to_string()];
            let folder = path.parent().unwrap().join("patches");
            sink.watch(Watch { path: folder.clone(), extensions: extensions.clone() });
            for file in walk::files(&folder, &extensions) {
                let name = file.file_stem().unwrap().to_string_lossy().into_owned();
                if name == "Rotten" {
                    return Err("the plugin choked on Rotten.patch".to_string());
                }
                sink.presets(vec![preset(&name, &format!("patch:{name}"))]);
            }
            Ok(())
        },
    };

    const WAIT: Duration = Duration::from_secs(30);

    struct Rig {
        dir: TestDir,
        plugin: PluginRef,
    }

    impl Rig {
        /// A plugin `id` with two patches, in a folder of its own.
        fn new(id: &str) -> Self {
            let dir = TestDir::new();
            let path = dir.file("plugin/synth.fake", b"the plugin");
            dir.file("plugin/patches/Warm.patch", b"");
            dir.file("plugin/patches/Glass.patch", b"");
            let plugin = PluginRef {
                uri: format!("fake:{id}"),
                id: id.to_string(),
                path,
                name: "Synth".into(),
                vendor: "Acme".into(),
            };
            Self { dir, plugin }
        }

        fn store(&self) -> PresetStore {
            let settings = Settings {
                scanner: Scanner::InProcess,
                cache_folder: Some(self.dir.path().join("cache").join(cache::FOLDER)),
                timeout: WAIT,
                folders: vec![self.dir.path().join("shared presets")],
            };
            PresetStore::new(&FAKE, settings, plugin_thread().unwrap())
        }

        /// Makes the plugin's code take `milliseconds` from now on.
        fn slow(&self, milliseconds: u64) {
            std::fs::write(self.plugin.path.with_file_name(SLOWNESS), milliseconds.to_string()).unwrap();
        }

        fn names(&self, store: &PresetStore) -> Vec<String> {
            let list = store.wait(&self.plugin, WAIT);
            assert!(!list.is_reading(), "{list:?}");
            list.presets.iter().map(|preset| preset.name.clone()).collect()
        }
    }

    #[test]
    fn the_first_request_starts_the_reading_and_does_not_wait_for_it() {
        let rig = Rig::new("first-request");
        rig.dir.file("shared presets/Mine.filepreset", b"");
        let store = rig.store();
        rig.slow(150);
        let started = Instant::now();
        let first = store.list(&rig.plugin);
        assert!(started.elapsed() < Duration::from_millis(100), "asking took {:?}", started.elapsed());
        assert_eq!((first.status.clone(), first.len()), (PresetStatus::Reading, 0));

        // The files first, then what the plugin's code found, each with
        // an id of its own.
        let list = store.wait(&rig.plugin, WAIT);
        assert_eq!(list.status, PresetStatus::Ready);
        assert_eq!(rig.names(&store), ["Mine", "Glass", "Warm"]);
        assert!(list.presets.iter().all(|preset| preset.id.len() == 10));
        assert_eq!(store.list(&rig.plugin), list, "asking again is asking for the same list");
        assert_eq!(reads("first-request"), 1);
    }

    #[test]
    fn what_was_read_is_remembered_until_the_plugin_or_its_folders_change() {
        let rig = Rig::new("remembered");
        assert_eq!(rig.names(&rig.store()), ["Glass", "Warm"]);
        assert_eq!(reads("remembered"), 1);

        // Another run of the program: the cache answers.
        assert_eq!(rig.names(&rig.store()), ["Glass", "Warm"]);
        assert_eq!(reads("remembered"), 1);

        // A preset was saved meanwhile.
        rig.dir.file("plugin/patches/New.patch", b"");
        assert_eq!(rig.names(&rig.store()), ["Glass", "New", "Warm"]);
        assert_eq!(reads("remembered"), 2);
        // A preset file appeared in the format's own folders.
        rig.dir.file("shared presets/Mine.filepreset", b"");
        assert_eq!(rig.names(&rig.store()), ["Mine", "Glass", "New", "Warm"]);
        assert_eq!(reads("remembered"), 3);
        assert_eq!(rig.names(&rig.store()).len(), 4);
        assert_eq!(reads("remembered"), 3);

        // The plugin was updated.
        std::fs::write(&rig.plugin.path, b"the plugin, version two").unwrap();
        assert_eq!(rig.names(&rig.store()).len(), 4);
        assert_eq!(reads("remembered"), 4);
    }

    #[test]
    fn a_failure_keeps_what_was_found_and_is_not_tried_again_unasked() {
        let rig = Rig::new("failing");
        rig.dir.file("plugin/patches/Rotten.patch", b"");
        let store = rig.store();
        let list = store.wait(&rig.plugin, WAIT);
        assert_eq!(list.status, PresetStatus::Failed("the plugin choked on Rotten.patch".into()));
        assert_eq!(rig.names(&store), ["Glass"], "what came before the failure is listed");
        assert_eq!(reads("failing"), 1);

        // Remembered: the next run does not walk into it again.
        let next = rig.store();
        assert_eq!(next.wait(&rig.plugin, WAIT), list);
        assert_eq!(reads("failing"), 1);

        // Asked to, it tries again; and with the cause gone, succeeds.
        next.refresh(&rig.plugin);
        assert_eq!(next.wait(&rig.plugin, WAIT), list);
        assert_eq!(reads("failing"), 2);
        std::fs::remove_file(rig.dir.path().join("plugin/patches/Rotten.patch")).unwrap();
        assert_eq!(rig.names(&rig.store()), ["Glass", "Warm"]);
    }

    #[test]
    fn a_refresh_reads_again_and_keeps_the_list_meanwhile() {
        let rig = Rig::new("refreshed");
        let store = rig.store();
        assert_eq!(rig.names(&store), ["Glass", "Warm"]);

        // Nothing has changed that the cache would notice, and still:
        store.refresh(&rig.plugin);
        let meanwhile = store.list(&rig.plugin);
        assert!(meanwhile.len() == 2, "{meanwhile:?}");
        assert_eq!(rig.names(&store), ["Glass", "Warm"]);
        assert_eq!(reads("refreshed"), 2);

        // Asked for while a reading is under way, it follows that one.
        rig.slow(100);
        store.refresh(&rig.plugin);
        assert!(eventually(WAIT, || reads("refreshed") == 3));
        rig.dir.file("plugin/patches/Late.patch", b"");
        store.refresh(&rig.plugin);
        rig.slow(0);
        assert!(eventually(WAIT, || reads("refreshed") == 4));
        assert_eq!(rig.names(&store), ["Glass", "Late", "Warm"]);

        // A plugin nobody asked about yet can be refreshed too.
        let other = Rig::new("refreshed-unasked");
        let store = other.store();
        store.refresh(&other.plugin);
        assert_eq!(other.names(&store), ["Glass", "Warm"]);
    }

    #[test]
    fn waiting_gives_up_at_its_timeout_with_what_there_is() {
        let rig = Rig::new("slow");
        let store = rig.store();
        rig.slow(400);
        let list = store.wait(&rig.plugin, Duration::from_millis(20));
        assert!(list.is_reading());
        assert_eq!(rig.names(&store), ["Glass", "Warm"]);
    }

    #[test]
    fn a_plugin_file_that_cannot_be_stamped_is_read_every_time() {
        let rig = Rig::new("unstamped");
        let plugin = PluginRef { path: rig.dir.path().join("plugin").join("<not a file>"), ..rig.plugin.clone() };
        for run in 1..=2 {
            let store = rig.store();
            assert_eq!(store.wait(&plugin, WAIT).len(), 2);
            assert_eq!(reads("unstamped"), run);
        }
    }

    #[cfg(windows)]
    fn status(code: u32) -> ExitStatus {
        std::os::windows::process::ExitStatusExt::from_raw(code)
    }

    #[cfg(unix)]
    fn status(code: u32) -> ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw((code as i32) << 8)
    }

    fn finished(status: Option<ExitStatus>, stdout: &str) -> Finished {
        Finished { status, stdout: stdout.as_bytes().to_vec(), stderr: Vec::new(), cancelled: false }
    }

    const TWO: &str = r#"plugin banner
{"watch":[{"path":"patches","extensions":["fxp"]}]}
{"presets":[{"id":"","name":"Warm","locator":"a"},{"id":"","name":"Glass","locator":"b"}]}
"#;

    #[test]
    fn a_reader_that_finished_is_complete_however_it_ended() {
        let output = format!("{TWO}{{\"done\":true}}\n");
        for ended in [Some(status(0)), Some(status(3)), None] {
            let mut found = Found::default();
            let outcome = interpret(&finished(ended, &output), WAIT, &mut found);
            assert_eq!(outcome, Outcome { failure: None, lasting: true });
            assert_eq!((found.presets.len(), found.watch.len()), (2, 1));
        }
    }

    #[test]
    fn a_reader_that_was_cut_short_says_how_and_keeps_what_it_had() {
        let failure = |finished: &Finished| {
            let mut found = Found::default();
            let outcome = interpret(finished, Duration::from_secs(120), &mut found);
            assert!(outcome.lasting);
            (outcome.failure.unwrap(), found.presets.len())
        };

        // The plugin took the process down halfway.
        let (reason, kept) = failure(&finished(Some(status(0xC000_0005)), TWO));
        assert!(reason.contains("the plugin failed while its presets were read"), "{reason}");
        assert!(reason.contains("crashed the scanner") && reason.ends_with("2 presets were read before that"));
        assert_eq!(kept, 2);
        // It hung, and was stopped.
        let (reason, kept) = failure(&finished(None, TWO));
        assert_eq!(reason, "reading the presets timed out after 120 s; 2 presets were read before that");
        assert_eq!(kept, 2);
        let (reason, _) = failure(&finished(None, ""));
        assert_eq!(reason, "reading the presets timed out after 120 s");
        // It said itself what went wrong.
        let (reason, kept) = failure(&finished(Some(status(1)), &format!("{TWO}{{\"failed\":\"no such file\"}}\n")));
        assert_eq!((reason.as_str(), kept), ("no such file", 2));
        // It ended without a word.
        let (reason, _) = failure(&finished(Some(status(0)), ""));
        assert_eq!(reason, "the preset reader stopped early");

        // Stopped because nobody wants the result any more: not a thing
        // to remember about the plugin.
        let mut found = Found::default();
        let stopped = Finished { cancelled: true, ..finished(None, TWO) };
        assert!(!interpret(&stopped, WAIT, &mut found).lasting);
    }

    #[test]
    fn a_scanner_that_cannot_be_run_is_a_failure_that_is_not_remembered() {
        let rig = Rig::new("no-scanner");
        let settings = Settings {
            // A file, so that it is taken for the scanner; not a program.
            scanner: Scanner::Executable(rig.plugin.path.clone()),
            cache_folder: Some(rig.dir.path().join("cache")),
            timeout: WAIT,
            folders: Vec::new(),
        };
        let store = PresetStore::new(&FAKE, settings, plugin_thread().unwrap());
        let list = store.wait(&rig.plugin, WAIT);
        assert!(matches!(&list.status, PresetStatus::Failed(reason) if reason.contains("cannot start")), "{list:?}");
        assert!(!rig.dir.path().join("cache").exists(), "nothing was learned about the plugin");
        assert_eq!(reads("no-scanner"), 0);
    }
}
