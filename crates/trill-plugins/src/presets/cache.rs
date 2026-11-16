//! The on-disk preset cache: what reading each plugin's presets found the
//! last time, one file per plugin, in a folder beside the scan cache.
//!
//! An entry is good for as long as the plugin file is the one it was read
//! from and the plugin's preset folders hold the files they held: both
//! are checked without running any of the plugin's code.

use super::Watch;
use crate::scan::cache::FileStamp;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use trill_core::presets::{PresetInfo, short_id};

/// Bumped whenever the file layout changes, or what is read from a plugin
/// does; older entries are read again.
const FORMAT_VERSION: u32 = 1;
/// Name of the folder, beside the scan cache, that holds the entries.
pub(crate) const FOLDER: &str = "presets";
/// The longest piece of a plugin's id used in its entry's file name.
const NAME_CHARS: usize = 60;

/// What reading one plugin's presets came to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    version: u32,
    /// The plugin file the presets were read from, and its stamp then.
    plugin: String,
    #[serde(flatten)]
    stamp: FileStamp,
    /// The folders the presets come from, and what their files looked
    /// like together ([`super::walk::fingerprint`]).
    pub watch: Vec<Watch>,
    pub fingerprint: u64,
    /// Why the presets could not be read, or not all of them. Remembered
    /// so that a plugin which crashes or hangs while its presets are read
    /// costs that once, not on every start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub presets: Vec<PresetInfo>,
}

impl Entry {
    pub(crate) fn new(
        plugin: &Path,
        stamp: FileStamp,
        watch: Vec<Watch>,
        fingerprint: u64,
        failure: Option<String>,
        presets: Vec<PresetInfo>,
    ) -> Self {
        Self {
            version: FORMAT_VERSION,
            plugin: plugin.to_string_lossy().into_owned(),
            stamp,
            watch,
            fingerprint,
            failure,
            presets,
        }
    }
}

/// The file that holds the entry of the plugin `uri` in the cache folder
/// `folder`. The name says which plugin it is as far as a file name can;
/// the digits after it keep two plugins from sharing one.
pub(crate) fn file(folder: &Path, uri: &str) -> PathBuf {
    let readable: String = uri
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '-' })
        .take(NAME_CHARS)
        .collect();
    folder.join(format!("{readable}-{}.json", short_id(uri)))
}

/// Reads the entry at `path`, if it is one for the plugin file `plugin`
/// as it is now. Anything else (no entry, an unreadable or outdated one,
/// one for another version of the plugin) is `None`: the only cost is
/// reading the presets again.
pub(crate) fn load(path: &Path, plugin: &Path, stamp: FileStamp) -> Option<Entry> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::warn!("cannot read the preset cache '{}': {error}", path.display());
            return None;
        }
    };
    match serde_json::from_str::<Entry>(&text) {
        Ok(entry) if entry.version != FORMAT_VERSION => None,
        Ok(entry) => (entry.stamp == stamp && entry.plugin == plugin.to_string_lossy()).then_some(entry),
        Err(error) => {
            log::warn!("discarding the corrupt preset cache '{}': {error}", path.display());
            None
        }
    }
}

/// Writes `entry` beside its destination and renames it into place, so a
/// crash mid-write cannot leave a truncated entry behind.
pub(crate) fn save(path: &Path, entry: &Entry) -> io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string(entry).map_err(io::Error::other)?;
    // Unique per writer: two programs reading the same plugin's presets
    // must not write through each other's temporary file.
    let unique = format!("{}-{}.tmp", std::process::id(), WRITES.fetch_add(1, Ordering::Relaxed));
    let temporary = path.with_extension(unique);
    std::fs::write(&temporary, json)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    fn preset(name: &str) -> PresetInfo {
        PresetInfo {
            id: short_id(name),
            name: name.into(),
            category: "Factory/Pads".into(),
            tags: vec!["pad".into()],
            locator: format!("clap-file\n{name}.fxp\n"),
            ..PresetInfo::default()
        }
    }

    #[test]
    fn an_entry_round_trips_and_is_good_for_the_plugin_it_was_read_from() {
        let dir = TestDir::new();
        let plugin = dir.file("plugins/synth.clap", b"version one");
        let stamp = FileStamp::of(&plugin).unwrap();
        let path = file(&dir.path().join("cache").join(FOLDER), "clap:com.acme.synth");
        assert!(load(&path, &plugin, stamp).is_none());

        let watch = vec![Watch { path: dir.path().join("patches"), extensions: vec!["fxp".into()] }];
        let entry = Entry::new(&plugin, stamp, watch, 42, None, vec![preset("Warm"), preset("Glass")]);
        save(&path, &entry).unwrap();
        assert_eq!(load(&path, &plugin, stamp), Some(entry.clone()));
        let left_behind = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(left_behind, 1, "the temporary file is renamed away");

        // The plugin was updated, or another file has taken its place.
        std::fs::write(&plugin, b"version two, longer").unwrap();
        assert!(load(&path, &plugin, FileStamp::of(&plugin).unwrap()).is_none());
        let other = dir.file("plugins/other.clap", b"version one");
        assert!(load(&path, &other, stamp).is_none());

        // A failure is remembered with what was found before it.
        let failed = Entry { failure: Some("the plugin crashed".into()), ..entry };
        save(&path, &failed).unwrap();
        assert_eq!(load(&path, &plugin, stamp).unwrap().failure.as_deref(), Some("the plugin crashed"));
    }

    #[test]
    fn unusable_entries_are_no_entries() {
        let dir = TestDir::new();
        let plugin = dir.file("synth.clap", b"x");
        let stamp = FileStamp::of(&plugin).unwrap();
        let corrupt = dir.file("corrupt.json", b"{ not json");
        assert!(load(&corrupt, &plugin, stamp).is_none());

        let future =
            Entry { version: FORMAT_VERSION + 1, ..Entry::new(&plugin, stamp, Vec::new(), 0, None, Vec::new()) };
        let path = dir.path().join("future.json");
        save(&path, &future).unwrap();
        assert!(load(&path, &plugin, stamp).is_none());
    }

    #[test]
    fn every_plugin_has_a_file_of_its_own_with_a_name_that_says_which() {
        let folder = Path::new("cache");
        let surge = file(folder, "clap:org.surge-synth-team.surge-xt");
        let name = surge.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("clap-org.surge-synth-team.surge-xt-") && name.ends_with(".json"), "{name}");
        assert_eq!(surge, file(folder, "clap:org.surge-synth-team.surge-xt"));

        // Ids that differ only where a file name cannot show it.
        assert_ne!(file(folder, "clap:a/b"), file(folder, "clap:a\\b"));
        // And ids of any length or content make a legal name.
        let long = file(folder, &format!("vst3:{}", "é/".repeat(400)));
        assert!(long.file_name().unwrap().to_string_lossy().len() < 100);
    }
}
