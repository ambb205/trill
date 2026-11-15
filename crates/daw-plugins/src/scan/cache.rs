//! The on-disk scan cache: what each plugin file contained the last time it
//! was scanned, keyed by path and validated by modification time and size.

use super::PluginRecord;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

/// Bumped whenever the file layout changes; older caches are discarded.
const FORMAT_VERSION: u32 = 1;
/// Name of the application's directory below the user's cache directory.
const APPLICATION_DIRECTORY: &str = "daw";

/// The directory for this application's caches, following each platform's
/// convention. `None` if the environment does not say where the user's
/// files live.
pub(crate) fn default_directory() -> Option<PathBuf> {
    directory_from(|name| std::env::var_os(name))
}

fn directory_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let var = |name: &str| env(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        var("HOME")?.join("Library").join("Caches")
    } else {
        var("XDG_CACHE_HOME").or_else(|| Some(var("HOME")?.join(".cache")))?
    };
    Some(base.join(APPLICATION_DIRECTORY))
}

/// Identifies one version of a file on disk.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileStamp {
    /// Modification time in nanoseconds since the Unix epoch.
    modified: u64,
    size: u64,
}

impl FileStamp {
    pub(crate) fn of(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let modified = metadata.modified()?.duration_since(UNIX_EPOCH).map_err(io::Error::other)?;
        // A macOS plugin is a bundle directory, whose own length says
        // nothing; its modification time still changes on reinstall.
        let size = if metadata.is_file() { metadata.len() } else { 0 };
        Ok(Self { modified: u64::try_from(modified.as_nanos()).unwrap_or(u64::MAX), size })
    }
}

/// What scanning a file produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Plugins(Vec<PluginRecord>),
    /// Remembered so a crashing or hanging plugin is not retried, and does
    /// not cost its timeout again, on every startup.
    Failed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    #[serde(flatten)]
    stamp: FileStamp,
    outcome: Outcome,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ScanCache {
    version: u32,
    entries: BTreeMap<String, Entry>,
}

impl Default for ScanCache {
    fn default() -> Self {
        Self { version: FORMAT_VERSION, entries: BTreeMap::new() }
    }
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl ScanCache {
    /// Reads the cache at `path`. A missing, unreadable or outdated cache is
    /// an empty one: the only cost is scanning again.
    pub(crate) fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                log::warn!("cannot read the plugin scan cache '{}': {error}", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(cache) if cache.version == FORMAT_VERSION => cache,
            Ok(cache) => {
                log::info!("discarding the plugin scan cache (format {})", cache.version);
                Self::default()
            }
            Err(error) => {
                log::warn!("discarding the corrupt plugin scan cache '{}': {error}", path.display());
                Self::default()
            }
        }
    }

    /// The remembered outcome for `path`, if the file has not changed since.
    pub(crate) fn get(&self, path: &Path, stamp: FileStamp) -> Option<&Outcome> {
        self.entries.get(&key(path)).filter(|entry| entry.stamp == stamp).map(|entry| &entry.outcome)
    }

    pub(crate) fn insert(&mut self, path: &Path, stamp: FileStamp, outcome: Outcome) {
        self.entries.insert(key(path), Entry { stamp, outcome });
    }

    /// Drops every entry whose file is not in `files`.
    pub(crate) fn retain(&mut self, files: &[PathBuf]) {
        let keep: HashSet<String> = files.iter().map(|file| key(file)).collect();
        self.entries.retain(|path, _| keep.contains(path));
    }

    /// Writes the cache beside its destination and renames it into place,
    /// so a crash mid-write cannot leave a truncated cache behind.
    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        static WRITES: AtomicU64 = AtomicU64::new(0);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        // Unique per writer: two scans finishing together (two factories,
        // two DAWs) must not write through each other's temporary file.
        let unique = format!("{}-{}.tmp", std::process::id(), WRITES.fetch_add(1, Ordering::Relaxed));
        let temporary = path.with_extension(unique);
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    fn plugins(id: &str) -> Outcome {
        Outcome::Plugins(vec![PluginRecord {
            id: id.to_string(),
            name: "Name".into(),
            vendor: "Vendor".into(),
            version: "1.2.3".into(),
            description: "Description".into(),
            features: vec!["instrument".into(), "stereo".into()],
        }])
    }

    #[test]
    fn the_default_directory_follows_the_platform() {
        let env = |name: &str| match name {
            "LOCALAPPDATA" => Some(OsString::from("local")),
            "HOME" => Some(OsString::from("home")),
            _ => None,
        };
        let expected = if cfg!(windows) {
            Path::new("local").join("daw")
        } else if cfg!(target_os = "macos") {
            Path::new("home").join("Library").join("Caches").join("daw")
        } else {
            Path::new("home").join(".cache").join("daw")
        };
        assert_eq!(directory_from(env), Some(expected));
        assert_eq!(directory_from(|_| None), None);
        assert_eq!(directory_from(|_| Some(OsString::new())), None, "an empty variable is no directory");
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = TestDir::new();
        let plugin = dir.file("a.clap", b"contents");
        let broken = dir.file("b.clap", b"other");
        let path = dir.path().join("nested").join("cache.json");

        let mut cache = ScanCache::default();
        cache.insert(&plugin, FileStamp::of(&plugin).unwrap(), plugins("com.acme.a"));
        cache.insert(&broken, FileStamp::of(&broken).unwrap(), Outcome::Failed("timed out".into()));
        cache.save(&path).unwrap();

        let loaded = ScanCache::load(&path);
        assert_eq!(loaded.get(&plugin, FileStamp::of(&plugin).unwrap()), Some(&plugins("com.acme.a")));
        assert_eq!(loaded.get(&broken, FileStamp::of(&broken).unwrap()), Some(&Outcome::Failed("timed out".into())));
        let left_behind = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(left_behind, 1, "the temporary file is renamed away");
    }

    #[test]
    fn a_changed_file_misses() {
        let dir = TestDir::new();
        let plugin = dir.file("a.clap", b"one");
        let mut cache = ScanCache::default();
        let stamp = FileStamp::of(&plugin).unwrap();
        cache.insert(&plugin, stamp, plugins("x"));

        assert!(cache.get(&plugin, stamp).is_some());
        assert!(cache.get(&plugin, FileStamp { size: stamp.size + 1, ..stamp }).is_none());
        assert!(cache.get(&plugin, FileStamp { modified: stamp.modified + 1, ..stamp }).is_none());
        assert!(cache.get(&dir.path().join("other.clap"), stamp).is_none());
    }

    #[test]
    fn retain_forgets_uninstalled_files() {
        let dir = TestDir::new();
        let kept = dir.file("kept.clap", b"1");
        let gone = dir.file("gone.clap", b"2");
        let mut cache = ScanCache::default();
        cache.insert(&kept, FileStamp::of(&kept).unwrap(), plugins("kept"));
        cache.insert(&gone, FileStamp::of(&gone).unwrap(), plugins("gone"));

        cache.retain(std::slice::from_ref(&kept));
        assert!(cache.get(&kept, FileStamp::of(&kept).unwrap()).is_some());
        assert!(cache.get(&gone, FileStamp::of(&gone).unwrap()).is_none());
    }

    #[test]
    fn unusable_caches_load_as_empty() {
        let dir = TestDir::new();
        let plugin = dir.file("a.clap", b"1");
        let stamp = FileStamp::of(&plugin).unwrap();

        assert!(ScanCache::load(&dir.path().join("missing.json")).entries.is_empty());

        let corrupt = dir.file("corrupt.json", b"{ not json");
        assert!(ScanCache::load(&corrupt).entries.is_empty());

        let mut future = ScanCache { version: FORMAT_VERSION + 1, ..ScanCache::default() };
        future.insert(&plugin, stamp, plugins("x"));
        let path = dir.path().join("future.json");
        future.save(&path).unwrap();
        assert!(ScanCache::load(&path).get(&plugin, stamp).is_none());
    }
}
