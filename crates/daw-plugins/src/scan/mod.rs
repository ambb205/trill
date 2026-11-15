//! Plugin scanning: finding out what a plugin file contains without
//! trusting it.
//!
//! Reading a plugin's descriptors means loading its library and running its
//! initialization code, and a broken plugin can crash or hang while doing
//! that. So each file is scanned in a throwaway child process (the
//! `daw-plugin-scan` executable) under a timeout. A file whose scan fails is
//! reported and skipped; it can never take the DAW down.
//!
//! Results are cached on disk by path and modification time, so only new or
//! updated plugins are scanned on later startups.
//!
//! Nothing here is specific to one plugin format; [`probe_file`] picks the
//! format from the file's name.

pub(crate) mod cache;
pub(crate) mod catalog;
mod child;

use cache::{FileStamp, Outcome, ScanCache};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// File name of the scanner executable, without the platform's extension.
pub const SCANNER_NAME: &str = "daw-plugin-scan";
/// Set in the environment of every scanner process. It is what tells an
/// application run as its own scanner ([`Scanner::CurrentExecutable`]) apart
/// from a normal start.
pub const SCANNER_ENV: &str = "DAW_PLUGIN_SCAN";

/// Upper bound on scanner processes running at once. Scanning is dominated
/// by loading libraries from disk, so more does not help.
const MAX_PARALLEL_SCANS: usize = 4;

/// One plugin found in a plugin file, as the plugin describes itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRecord {
    /// The plugin's own identifier, unique across vendors: for CLAP the
    /// descriptor id, such as `org.surge-synth-team.surge-xt`; for VST3 the
    /// processor's class id as 32 hexadecimal digits.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Free-form classification tags in the format's own vocabulary: CLAP
    /// features (`instrument`, `reverb`, `stereo`) or VST3 sub-categories
    /// (`Instrument`, `Synth`).
    #[serde(default)]
    pub features: Vec<String>,
}

/// A plugin file that could not be scanned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanFailure {
    pub path: PathBuf,
    /// Human-readable cause: a crash, a timeout, a load error.
    pub reason: String,
}

/// Where plugin files are scanned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scanner {
    /// In a `daw-plugin-scan` process, using the executable found next to
    /// the running one. If there is none, plugins are scanned inside this
    /// process and a warning is logged.
    #[default]
    Auto,
    /// In a process of this scanner executable. If the file does not
    /// exist, this behaves like [`Scanner::Auto`].
    Executable(PathBuf),
    /// In a second process of the running application itself, so there is
    /// no separate scanner file to ship or lose. The application must call
    /// [`run_as_scanner_if_requested`] first thing in `main`.
    CurrentExecutable,
    /// Inside this process, deliberately. A plugin that crashes while it is
    /// scanned then takes the application with it; this exists for
    /// environments that cannot start processes and for debugging a
    /// plugin's loading.
    InProcess,
}

impl Scanner {
    /// The scanner executable to run, or `None` to scan in this process.
    pub(crate) fn resolve(&self) -> Option<PathBuf> {
        match self {
            Scanner::InProcess => None,
            Scanner::CurrentExecutable => {
                let found = std::env::current_exe().ok();
                if found.is_none() {
                    log::warn!("cannot find this application's executable; scanning plugins inside this process");
                }
                found
            }
            Scanner::Executable(path) if path.is_file() => Some(path.clone()),
            Scanner::Executable(_) | Scanner::Auto => {
                let found = locate_scanner();
                if found.is_none() {
                    log::warn!(
                        "the plugin scanner '{SCANNER_NAME}' was not found next to the application; scanning \
                         plugins inside this process, where a faulty plugin can crash it"
                    );
                }
                found
            }
        }
    }
}

/// Whether a scan may reuse earlier results.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScanMode {
    /// Scan only files that are new or have changed since they were cached.
    /// Failures are cached too, so a plugin that hangs costs its timeout
    /// once, not on every startup.
    UseCache,
    /// Scan every file again, including those that failed before.
    Fresh,
}

/// What the scanner executable prints on success: one line of JSON.
#[derive(Debug, Serialize, Deserialize)]
pub struct ScanOutput {
    pub plugins: Vec<PluginRecord>,
}

/// Reads the descriptors of the plugins in `path` **in the current
/// process**.
///
/// This loads the plugin's library and runs its initialization code. A
/// misbehaving plugin can crash or hang the process, which is why the DAW
/// calls this from the scanner executable rather than directly.
pub fn probe_file(path: &Path) -> Result<Vec<PluginRecord>, String> {
    let extension = path.extension().and_then(|extension| extension.to_str()).unwrap_or_default();
    if extension.eq_ignore_ascii_case(crate::clap::PLUGIN_EXTENSION) {
        crate::clap::probe(path)
    } else if crate::vst3::is_plugin_file(path) {
        crate::vst3::probe(path)
    } else {
        Err(format!("'{}' is not a supported plugin file", path.display()))
    }
}

/// Entry point of the scanner executable: `daw-plugin-scan <plugin file>`.
///
/// Prints a [`ScanOutput`] as one line of JSON on stdout and exits with
/// success, or prints the reason on stderr and exits with failure. A plugin
/// that crashes the process is reported by the exit status alone.
pub fn scanner_main() -> ExitCode {
    ExitCode::from(run_scanner())
}

/// Turns this process into a plugin scanner if it was started as one, and
/// otherwise returns at once.
///
/// An application that uses [`Scanner::CurrentExecutable`] calls this before
/// anything else in `main`: the scanning child is the application's own
/// executable, told apart by the [`SCANNER_ENV`] variable its parent sets.
pub fn run_as_scanner_if_requested() {
    if std::env::var_os(SCANNER_ENV).is_some() {
        std::process::exit(i32::from(run_scanner()));
    }
}

/// Scans the plugin file named by the only argument. Returns the exit code.
fn run_scanner() -> u8 {
    let mut arguments = std::env::args_os().skip(1);
    let (Some(path), None) = (arguments.next(), arguments.next()) else {
        eprintln!("usage: {SCANNER_NAME} <plugin file>");
        return 2;
    };
    suppress_crash_dialogs();
    match probe_file(Path::new(&path)) {
        Ok(plugins) => match serde_json::to_string(&ScanOutput { plugins }) {
            Ok(json) => {
                println!("{json}");
                0
            }
            Err(error) => {
                eprintln!("cannot encode the scan result: {error}");
                1
            }
        },
        Err(reason) => {
            eprintln!("{reason}");
            1
        }
    }
}

/// A crashing plugin must end the scanner quietly. By default Windows may
/// instead show a dialog and keep the dead process around until the user
/// dismisses it, which would turn every crash into a timeout.
#[cfg(windows)]
fn suppress_crash_dialogs() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX, SetErrorMode,
    };
    // SAFETY: `SetErrorMode` only changes this process's error-mode flags
    // and has no preconditions.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    }
}

#[cfg(not(windows))]
fn suppress_crash_dialogs() {}

/// Finds the scanner executable next to the running one.
///
/// Cargo puts test and example binaries one directory below the other
/// binaries (`deps/`, `examples/`), so the parent is tried as well; that
/// keeps `cargo test` and `cargo run --example` isolated too.
fn locate_scanner() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    let file_name = format!("{SCANNER_NAME}{}", std::env::consts::EXE_SUFFIX);
    let nested = directory.file_name().is_some_and(|name| name == "deps" || name == "examples");
    let candidates = [Some(directory), directory.parent().filter(|_| nested)];
    candidates.into_iter().flatten().map(|directory| directory.join(&file_name)).find(|path| path.is_file())
}

/// How to scan a set of plugin files.
pub(crate) struct ScanJob<'a> {
    pub files: &'a [PathBuf],
    pub mode: ScanMode,
    /// Where results are remembered between runs; `None` disables caching.
    pub cache_path: Option<&'a Path>,
    /// The scanner executable, or `None` to scan in this process.
    pub scanner: Option<&'a Path>,
    /// Longest a single file may take before it is given up on.
    pub timeout: Duration,
}

/// The outcome for every file of a [`ScanJob`], in the order given.
#[derive(Debug, Default)]
pub(crate) struct ScanResults {
    pub plugins: Vec<(PathBuf, PluginRecord)>,
    pub failures: Vec<ScanFailure>,
}

/// Scans `job.files`, reusing cached results where allowed.
///
/// `in_process` is the fallback used when there is no scanner executable;
/// it is called for one file at a time.
pub(crate) fn scan_files(
    job: &ScanJob<'_>,
    in_process: &(dyn Fn(&Path) -> Result<Vec<PluginRecord>, String> + Sync),
) -> ScanResults {
    let mut cache = match (job.mode, job.cache_path) {
        (ScanMode::UseCache, Some(path)) => ScanCache::load(path),
        _ => ScanCache::default(),
    };

    // Decide per file whether the cached outcome still applies.
    let stamps: Vec<Option<FileStamp>> = job.files.iter().map(|file| FileStamp::of(file).ok()).collect();
    let mut outcomes: Vec<Option<Outcome>> = job
        .files
        .iter()
        .zip(&stamps)
        .map(|(file, stamp)| stamp.and_then(|stamp| cache.get(file, stamp)).cloned())
        .collect();
    let pending: Vec<usize> = (0..job.files.len()).filter(|&index| outcomes[index].is_none()).collect();

    if !pending.is_empty() {
        // The flag says whether the outcome may be cached.
        let scan_one = |file: &Path| -> (Outcome, bool) {
            let result = match job.scanner {
                Some(scanner) => child::run(scanner, file, job.timeout),
                None => in_process(file).map_err(child::Failure::Plugin),
            };
            match result {
                Ok(plugins) => (Outcome::Plugins(plugins), true),
                Err(failure) => {
                    log::warn!("skipping plugin '{}': {}", file.display(), failure.reason());
                    let cacheable = matches!(failure, child::Failure::Plugin(_));
                    (Outcome::Failed(failure.reason().to_string()), cacheable)
                }
            }
        };

        // In-process scanning stays sequential: the plugins share this
        // process, and their initialization is not assumed to be reentrant.
        let workers = if job.scanner.is_some() {
            std::thread::available_parallelism().map_or(1, usize::from).min(MAX_PARALLEL_SCANS).min(pending.len())
        } else {
            1
        };
        let next = AtomicUsize::new(0);
        let finished: Mutex<Vec<(usize, Outcome, bool)>> = Mutex::new(Vec::with_capacity(pending.len()));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    // Each worker takes the next file nobody has taken yet.
                    while let Some(&index) = pending.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let (outcome, cacheable) = scan_one(&job.files[index]);
                        finished.lock().unwrap_or_else(PoisonError::into_inner).push((index, outcome, cacheable));
                    }
                });
            }
        });
        for (index, outcome, cacheable) in finished.into_inner().unwrap_or_else(PoisonError::into_inner) {
            if cacheable && let Some(stamp) = stamps[index] {
                cache.insert(&job.files[index], stamp, outcome.clone());
            }
            outcomes[index] = Some(outcome);
        }
    }

    if let Some(path) = job.cache_path {
        // Forget files that were uninstalled, so the cache cannot grow
        // without bound.
        cache.retain(job.files);
        if let Err(error) = cache.save(path) {
            log::warn!("cannot write the plugin scan cache '{}': {error}", path.display());
        }
    }

    let mut results = ScanResults::default();
    for (file, outcome) in job.files.iter().zip(outcomes) {
        match outcome {
            Some(Outcome::Plugins(plugins)) => {
                results.plugins.extend(plugins.into_iter().map(|plugin| (file.clone(), plugin)));
            }
            Some(Outcome::Failed(reason)) => results.failures.push(ScanFailure { path: file.clone(), reason }),
            None => results
                .failures
                .push(ScanFailure { path: file.clone(), reason: "the scan did not complete".to_string() }),
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    fn record(id: &str) -> PluginRecord {
        PluginRecord {
            id: id.to_string(),
            name: id.to_uppercase(),
            vendor: "Acme".into(),
            version: "1.0".into(),
            description: String::new(),
            features: vec!["instrument".into()],
        }
    }

    /// Scans in-process with a fake probe that fails for files named `bad`.
    fn scan(files: &[PathBuf], cache: Option<&Path>, mode: ScanMode, calls: &AtomicUsize) -> ScanResults {
        let job = ScanJob { files, mode, cache_path: cache, scanner: None, timeout: Duration::from_secs(5) };
        scan_files(&job, &|path: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            if stem == "bad" { Err("it crashed".to_string()) } else { Ok(vec![record(&stem)]) }
        })
    }

    #[test]
    fn results_keep_file_order_and_separate_failures() {
        let dir = TestDir::new();
        let files = [dir.file("b.clap", b"1"), dir.file("bad.clap", b"2"), dir.file("a.clap", b"3")];
        let calls = AtomicUsize::new(0);
        let results = scan(&files, None, ScanMode::UseCache, &calls);

        let ids: Vec<&str> = results.plugins.iter().map(|(_, plugin)| plugin.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert_eq!(results.plugins[0].0, files[0]);
        assert_eq!(results.failures, [ScanFailure { path: files[1].clone(), reason: "it crashed".into() }]);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn unchanged_files_are_not_scanned_again() {
        let dir = TestDir::new();
        let cache = dir.path().join("cache").join("scan.json");
        let files = [dir.file("a.clap", b"1"), dir.file("bad.clap", b"2")];
        let calls = AtomicUsize::new(0);

        let first = scan(&files, Some(&cache), ScanMode::UseCache, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let second = scan(&files, Some(&cache), ScanMode::UseCache, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "both the success and the failure come from the cache");
        assert_eq!(first.plugins, second.plugins);
        assert_eq!(first.failures, second.failures);

        // A changed file is scanned again; the untouched one is not.
        std::fs::write(&files[0], b"a longer file").unwrap();
        scan(&files, Some(&cache), ScanMode::UseCache, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        // A fresh scan ignores the cache, then refills it.
        scan(&files, Some(&cache), ScanMode::Fresh, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        scan(&files, Some(&cache), ScanMode::UseCache, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn a_file_that_cannot_be_stamped_is_never_cached() {
        let dir = TestDir::new();
        let cache = dir.path().join("scan.json");
        let files = [dir.path().join("vanished.clap")];
        let calls = AtomicUsize::new(0);
        scan(&files, Some(&cache), ScanMode::UseCache, &calls);
        scan(&files, Some(&cache), ScanMode::UseCache, &calls);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "without a modification time there is nothing to key on");
    }

    #[test]
    fn the_scanner_choice_resolves_to_an_executable_or_to_this_process() {
        let dir = TestDir::new();
        let executable = dir.file("scanner.exe", b"");
        assert_eq!(Scanner::InProcess.resolve(), None);
        assert_eq!(Scanner::Executable(executable.clone()).resolve(), Some(executable));
        // A missing executable falls back to looking beside this one, which
        // is also what `Auto` does.
        assert_eq!(Scanner::Executable(dir.path().join("missing")).resolve(), Scanner::Auto.resolve());
        assert_eq!(Scanner::default(), Scanner::Auto);
    }

    #[test]
    fn unsupported_files_are_rejected_before_loading() {
        for file in ["plugin.dll", "plugin.vst", "plugin", "vst3/plugin.so"] {
            let error = probe_file(Path::new(file)).unwrap_err();
            assert!(error.contains("not a supported plugin file"), "{error}");
        }
    }

    #[test]
    fn the_format_is_picked_from_the_files_name() {
        // Neither file exists, so each format's loader reports that, in
        // its own words, rather than the file being turned away.
        for file in ["missing.clap", "missing.vst3", "Missing.vst3/Contents/x86_64-linux/Missing.so"] {
            let error = probe_file(Path::new(file)).unwrap_err();
            assert!(!error.contains("not a supported plugin file"), "{file}: {error}");
        }
    }
}
