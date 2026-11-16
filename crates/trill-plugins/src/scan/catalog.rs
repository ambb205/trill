//! The plugins of one format that are installed on this system: found by
//! scanning the first time they are asked for, and remembered from then
//! on.
//!
//! Every plugin format lists its plugins the same way. What differs is
//! described by a [`Format`].

use super::{PluginRecord, ScanFailure, ScanJob, ScanMode, ScanResults, Scanner, scan_files};
use crate::main_thread::MainThread;
use crate::plugin_thread::PluginThread;
use crate::presets::store::PluginRef;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;
use trill_core::device::{DeviceCategory, DeviceDescriptor};
use trill_core::presets::PresetList;

/// What tells one plugin format from another while its plugins are scanned
/// and listed.
pub(crate) struct Format {
    /// For messages: `CLAP`, `VST3`.
    pub name: &'static str,
    /// URI scheme of the format's devices.
    pub scheme: &'static str,
    /// Finds the format's plugin files below the given directories.
    pub find_files: fn(&[PathBuf]) -> Vec<PathBuf>,
    /// Reads the plugins in one file, in this process.
    pub probe: fn(&Path) -> Result<Vec<PluginRecord>, String>,
    /// Whether a plugin is an instrument rather than an effect.
    pub is_instrument: fn(&PluginRecord) -> bool,
    /// The description a plugin is listed with.
    pub description: fn(&PluginRecord) -> String,
}

/// Where and how to look for the plugins of a format. The public
/// configuration of each format's factory converts into this.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub search_paths: Vec<PathBuf>,
    pub scanner: Scanner,
    pub cache_path: Option<PathBuf>,
    pub scan_timeout: Duration,
}

impl Settings {
    /// Where and how the presets of the plugins these settings find are
    /// read: by the same scanner, cached beside the scan cache, and given
    /// more time, since a plugin has more presets than a file has
    /// plugins. `folders` are where the format keeps preset files.
    pub(crate) fn preset_settings(&self, folders: Vec<PathBuf>) -> crate::presets::store::Settings {
        let beside = self.cache_path.as_ref().and_then(|path| path.parent());
        crate::presets::store::Settings {
            scanner: self.scanner.clone(),
            cache_folder: beside.map(|folder| folder.join(crate::presets::cache::FOLDER)),
            timeout: self.scan_timeout * crate::presets::store::PRESET_PATIENCE,
            folders,
        }
    }
}

/// An installed plugin.
pub(crate) struct Installed {
    /// The plugin file it was found in.
    pub path: PathBuf,
    pub name: String,
    pub vendor: String,
    pub instrument: bool,
}

/// What the last scan found.
#[derive(Default)]
pub(crate) struct Catalog {
    /// By the plugin's id within its format.
    plugins: HashMap<String, Installed>,
    pub descriptors: Vec<DeviceDescriptor>,
    pub failures: Vec<ScanFailure>,
}

impl Catalog {
    fn build(format: &Format, results: ScanResults) -> Self {
        let mut catalog = Self { failures: results.failures, ..Self::default() };
        for (path, record) in results.plugins {
            if let Some(existing) = catalog.plugins.get(&record.id) {
                log::debug!(
                    "'{}' in '{}' is already provided by '{}'",
                    record.id,
                    path.display(),
                    existing.path.display()
                );
                continue;
            }
            let instrument = (format.is_instrument)(&record);
            catalog.descriptors.push(DeviceDescriptor {
                uri: format!("{}:{}", format.scheme, record.id),
                name: record.name.clone(),
                vendor: record.vendor.clone(),
                category: if instrument { DeviceCategory::Instrument } else { DeviceCategory::Effect },
                description: (format.description)(&record),
                // A plugin reports its parameters once it is instantiated.
                params: Vec::new(),
                presets: Vec::new(),
            });
            let installed = Installed { path, name: record.name, vendor: record.vendor, instrument };
            catalog.plugins.insert(record.id, installed);
        }
        // Scan order is an accident of the file system; lists read better
        // by name.
        catalog.descriptors.sort_by_cached_key(|descriptor| (descriptor.name.to_lowercase(), descriptor.uri.clone()));
        catalog
    }

    /// The installed plugin with this id, or why a device cannot be made of
    /// it.
    pub(crate) fn find(&self, id: &str) -> Result<&Installed, String> {
        self.plugins.get(id).ok_or_else(|| match self.failures.len() {
            0 => "the plugin is not installed".to_string(),
            count => format!("the plugin is not installed ({count} plugin files could not be scanned)"),
        })
    }

    /// How many plugins were found.
    pub(crate) fn len(&self) -> usize {
        self.plugins.len()
    }

    /// The installed plugin with this id, as the plugin whose presets are
    /// wanted.
    pub(crate) fn plugin_ref(&self, scheme: &str, id: &str) -> Option<PluginRef> {
        let installed = self.plugins.get(id)?;
        Some(PluginRef {
            uri: format!("{scheme}:{id}"),
            id: id.to_string(),
            path: installed.path.clone(),
            name: installed.name.clone(),
            vendor: installed.vendor.clone(),
        })
    }
}

/// The installed plugins of one format.
///
/// The first scan happens when the catalog is first asked for, or when
/// [`scan`](Self::scan) is called; later requests reuse its result.
pub(crate) struct Inventory {
    format: &'static Format,
    settings: Settings,
    /// Where plugins are scanned when there is no scanner process.
    main: MainThread<PluginThread>,
    /// `None` until the first scan.
    catalog: RwLock<Option<Arc<Catalog>>>,
    /// Held while scanning, so concurrent callers share one scan.
    scanning: Mutex<()>,
}

impl Inventory {
    pub(crate) fn new(format: &'static Format, settings: Settings, main: MainThread<PluginThread>) -> Self {
        Self { format, settings, main, catalog: RwLock::new(None), scanning: Mutex::new(()) }
    }

    /// Looks for plugins again and replaces the catalog. Blocks until the
    /// scan is done.
    pub(crate) fn scan(&self, mode: ScanMode) {
        let _scanning = self.scanning.lock().unwrap_or_else(PoisonError::into_inner);
        self.scan_locked(mode);
    }

    /// The catalog, scanning first if that has not happened yet.
    pub(crate) fn catalog(&self) -> Arc<Catalog> {
        if let Some(catalog) = self.current() {
            return catalog;
        }
        let _scanning = self.scanning.lock().unwrap_or_else(PoisonError::into_inner);
        // Another thread may have completed the first scan meanwhile.
        match self.current() {
            Some(catalog) => catalog,
            None => self.scan_locked(ScanMode::UseCache),
        }
    }

    /// The catalog if there is one already. Never scans, so never waits
    /// for longer than a scan takes to hand its result over.
    pub(crate) fn scanned(&self) -> Option<Arc<Catalog>> {
        self.current()
    }

    /// The installed plugin `id`, as the plugin whose presets are wanted;
    /// or the list to answer with when there is no asking the plugin: an
    /// empty one for a plugin that is not installed, and one that is
    /// still being read while plugins have not been scanned (unless
    /// `scan` says to scan, and wait for that, now).
    pub(crate) fn preset_source(&self, id: &str, scan: bool) -> Result<PluginRef, PresetList> {
        let catalog = if scan { Some(self.catalog()) } else { self.scanned() };
        match catalog {
            None => Err(PresetList::reading(Vec::new())),
            Some(catalog) => catalog.plugin_ref(self.format.scheme, id).ok_or_else(PresetList::default),
        }
    }

    /// Makes `results` the catalog, as if a scan had produced them.
    #[cfg(test)]
    pub(crate) fn set_scanned(&self, results: ScanResults) {
        self.replace(Catalog::build(self.format, results));
    }

    fn current(&self) -> Option<Arc<Catalog>> {
        self.catalog.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn replace(&self, catalog: Catalog) -> Arc<Catalog> {
        let catalog = Arc::new(catalog);
        *self.catalog.write().unwrap_or_else(PoisonError::into_inner) = Some(catalog.clone());
        catalog
    }

    /// Scans and replaces the catalog. The caller holds `scanning`.
    fn scan_locked(&self, mode: ScanMode) -> Arc<Catalog> {
        let files = (self.format.find_files)(&self.settings.search_paths);
        // Nothing to scan, nothing to warn about.
        let scanner = if files.is_empty() { None } else { self.settings.scanner.resolve() };

        // Scanning in this process loads the plugins into it, so it runs
        // where all other plugin code runs: on the plugin main thread.
        let (main, timeout, probe) = (self.main.clone(), self.settings.scan_timeout, self.format.probe);
        let in_process = move |path: &Path| {
            let path = path.to_path_buf();
            main.call(timeout, move |_| probe(&path)).unwrap_or_else(|error| Err(error.to_string()))
        };
        let job = ScanJob {
            files: &files,
            mode,
            cache_path: self.settings.cache_path.as_deref(),
            scanner: scanner.as_deref(),
            timeout: self.settings.scan_timeout,
        };
        let catalog = Catalog::build(self.format, scan_files(&job, &in_process));
        log::info!(
            "found {} {} plugins in {} files ({} failed to scan)",
            catalog.len(),
            self.format.name,
            files.len(),
            catalog.failures.len()
        );
        self.replace(catalog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_thread::plugin_thread;
    use crate::test_support::TestDir;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static PROBES: AtomicUsize = AtomicUsize::new(0);

    /// A format whose plugin files are named `*.fake` and contain the
    /// plugin's id, or `broken`.
    static FAKE: Format = Format {
        name: "FAKE",
        scheme: "fake",
        find_files: |directories| {
            let mut files: Vec<PathBuf> = directories
                .iter()
                .filter_map(|directory| std::fs::read_dir(directory).ok())
                .flat_map(|entries| entries.filter_map(|entry| Some(entry.ok()?.path())))
                .filter(|path| path.extension().is_some_and(|extension| extension == "fake"))
                .collect();
            files.sort();
            files
        },
        probe: |path| {
            PROBES.fetch_add(1, Ordering::SeqCst);
            let id = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
            if id == "broken" { Err("it is broken".to_string()) } else { Ok(vec![record(&id, &id, &[])]) }
        },
        is_instrument: |record| record.features.iter().any(|feature| feature == "synth"),
        description: |record| format!("About {}", record.name),
    };

    fn record(id: &str, name: &str, features: &[&str]) -> PluginRecord {
        PluginRecord {
            id: id.to_string(),
            name: name.to_string(),
            vendor: "Acme".into(),
            version: "1.0".into(),
            description: String::new(),
            features: features.iter().map(|feature| feature.to_string()).collect(),
        }
    }

    #[test]
    fn the_catalog_classifies_sorts_and_deduplicates() {
        let results = ScanResults {
            plugins: vec![
                (PathBuf::from("b.fake"), record("verb", "zeta Verb", &["reverb"])),
                (PathBuf::from("a.fake"), record("synth", "Alpha Synth", &["synth", "stereo"])),
                (PathBuf::from("c.fake"), record("synth", "Alpha Synth (copy)", &["synth"])),
                (PathBuf::from("d.fake"), record("plain", "Middle", &[])),
            ],
            failures: vec![ScanFailure { path: PathBuf::from("bad.fake"), reason: "crashed".into() }],
        };
        let catalog = Catalog::build(&FAKE, results);

        let listed: Vec<(&str, &str, DeviceCategory)> = catalog
            .descriptors
            .iter()
            .map(|descriptor| (descriptor.uri.as_str(), descriptor.name.as_str(), descriptor.category))
            .collect();
        assert_eq!(
            listed,
            [
                ("fake:synth", "Alpha Synth", DeviceCategory::Instrument),
                ("fake:plain", "Middle", DeviceCategory::Effect),
                ("fake:verb", "zeta Verb", DeviceCategory::Effect),
            ]
        );
        let synth = &catalog.descriptors[0];
        assert_eq!((synth.vendor.as_str(), synth.description.as_str()), ("Acme", "About Alpha Synth"));
        assert!(synth.params.is_empty() && synth.presets.is_empty());

        let installed = catalog.find("synth").unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(installed.path, Path::new("a.fake"), "the first installation wins");
        assert_eq!(installed.name, "Alpha Synth");
        assert!(installed.instrument);
        assert_eq!((catalog.len(), catalog.failures.len()), (3, 1));
    }

    #[test]
    fn a_missing_plugin_says_how_many_files_could_not_be_scanned() {
        let failure = || ScanFailure { path: PathBuf::from("bad.fake"), reason: "crashed".into() };
        let clean = Catalog::build(&FAKE, ScanResults::default());
        assert_eq!(clean.find("synth").err().unwrap(), "the plugin is not installed");
        let dirty = Catalog::build(&FAKE, ScanResults { plugins: Vec::new(), failures: vec![failure(), failure()] });
        assert_eq!(
            dirty.find("synth").err().unwrap(),
            "the plugin is not installed (2 plugin files could not be scanned)"
        );
    }

    #[test]
    fn the_first_request_scans_and_later_ones_do_not() {
        let dir = TestDir::new();
        dir.file("plugins/one.fake", b"one");
        dir.file("plugins/bad.fake", b"broken");
        dir.file("plugins/ignored.txt", b"two");
        let settings = Settings {
            search_paths: vec![dir.path().join("plugins")],
            scanner: Scanner::InProcess,
            cache_path: None,
            scan_timeout: Duration::from_secs(10),
        };
        let inventory = Inventory::new(&FAKE, settings, plugin_thread().unwrap());

        let before = PROBES.load(Ordering::SeqCst);
        let catalog = inventory.catalog();
        assert_eq!(catalog.descriptors.len(), 1);
        assert_eq!(catalog.descriptors[0].uri, "fake:one");
        assert_eq!(catalog.failures.len(), 1);
        assert_eq!(catalog.failures[0].reason, "it is broken");
        assert_eq!(PROBES.load(Ordering::SeqCst), before + 2);

        // Asking again costs nothing; a deliberate scan looks again.
        assert!(Arc::ptr_eq(&inventory.catalog(), &catalog));
        assert_eq!(PROBES.load(Ordering::SeqCst), before + 2);
        dir.file("plugins/two.fake", b"two");
        inventory.scan(ScanMode::UseCache);
        assert_eq!(inventory.catalog().descriptors.len(), 2);
        assert_eq!(catalog.descriptors.len(), 1, "a catalog handed out earlier is a snapshot");

        inventory.set_scanned(ScanResults::default());
        assert!(inventory.catalog().descriptors.is_empty());
    }
}
