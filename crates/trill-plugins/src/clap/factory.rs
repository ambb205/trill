//! The device factory for `clap:` URIs.

use super::controller::ClapController;
use super::instance::{CreateRequest, Created};
use super::processor::ClapProcessor;
use super::{CLAP_SCHEME, find_plugin_files, probe, standard_search_paths};
use crate::main_thread::MainThread;
use crate::plugin_thread::{self, CREATE_TIMEOUT, PluginThread, plugin_thread};
use crate::presets::store::{PluginRef, PresetFormat, PresetStore};
use crate::scan::catalog::{Format, Inventory, Settings};
use crate::scan::{self, ScanFailure, ScanMode, Scanner};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use trill_core::Device;
use trill_core::device::{DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, split_uri};
use trill_core::presets::PresetList;

/// The CLAP feature that marks a plugin as an instrument.
const INSTRUMENT_FEATURE: &str = "instrument";
/// File name of the scan cache inside the application's cache directory.
const CACHE_FILE: &str = "clap-plugins.json";

/// How CLAP plugins are found and listed.
static FORMAT: Format = Format {
    name: "CLAP",
    scheme: CLAP_SCHEME,
    find_files: find_plugin_files,
    probe,
    is_instrument: |record| record.features.iter().any(|feature| feature == INSTRUMENT_FEATURE),
    description: |record| record.description.clone(),
};

/// How the presets of CLAP plugins are found: all of them through the
/// plugin's own preset-discovery providers.
static PRESETS: PresetFormat = PresetFormat {
    scheme: CLAP_SCHEME,
    read_files: |_plugin, _folders, _sink| {},
    read_in_thread: |thread, path, id, sink| thread.clap.read_presets(path, id, sink),
};

/// Where and how [`ClapFactory`] looks for plugins.
#[derive(Clone, Debug)]
pub struct ClapConfig {
    /// Directories searched, recursively, for `.clap` files. Earlier
    /// directories win when a plugin is installed more than once.
    pub search_paths: Vec<PathBuf>,
    /// Where plugin files are scanned: by default in a separate process,
    /// so that a plugin which crashes while loading cannot crash the DAW.
    pub scanner: Scanner,
    /// Where scan results are kept between runs. `None` disables the cache.
    /// What is found of each plugin's presets is kept in a folder
    /// `presets` beside this file.
    pub cache_path: Option<PathBuf>,
    /// Longest a single plugin file may take to scan before it is skipped.
    /// Reading a plugin's presets, of which there may be thousands, is
    /// given six times as long.
    pub scan_timeout: Duration,
}

impl Default for ClapConfig {
    /// The standard CLAP directories of this system and a cache in the
    /// user's cache directory.
    fn default() -> Self {
        Self {
            search_paths: standard_search_paths(),
            scanner: Scanner::Auto,
            cache_path: scan::cache::default_directory().map(|directory| directory.join(CACHE_FILE)),
            scan_timeout: Duration::from_secs(20),
        }
    }
}

impl From<ClapConfig> for Settings {
    fn from(config: ClapConfig) -> Self {
        Self {
            search_paths: config.search_paths,
            scanner: config.scanner,
            cache_path: config.cache_path,
            scan_timeout: config.scan_timeout,
        }
    }
}

/// Makes installed CLAP plugins available as devices.
///
/// Plugins are found by scanning the configured directories. The first scan
/// happens on first use, or when [`scan`](Self::scan) is called; later
/// calls reuse its result, so [`descriptors`](DeviceFactory::descriptors)
/// is cheap.
///
/// The factory may be used from any thread. All plugin main-thread work is
/// carried out on the process's plugin main thread, which runs for as long
/// as a factory or a device created by one exists, and for the rest of
/// the process once a plugin has shown its editor.
///
/// Cloning is cheap and yields a handle to the same factory: keep one to
/// scan and report with, and give another to the engine.
#[derive(Clone)]
pub struct ClapFactory {
    shared: Arc<Shared>,
}

struct Shared {
    main: MainThread<PluginThread>,
    installed: Inventory,
    presets: PresetStore,
}

impl ClapFactory {
    /// Starts the plugin main thread if it is not running yet. No plugin is
    /// scanned or loaded until the factory is first used.
    ///
    /// Fails only if the operating system cannot start a thread.
    pub fn new(config: ClapConfig) -> std::io::Result<Self> {
        let main = plugin_thread()?;
        let settings: Settings = config.into();
        let presets = PresetStore::new(&PRESETS, settings.preset_settings(Vec::new()), main.clone());
        let installed = Inventory::new(&FORMAT, settings, main.clone());
        Ok(Self { shared: Arc::new(Shared { main, installed, presets }) })
    }

    /// The installed plugin `uri` names, as the plugin whose presets are
    /// wanted. `None` for a URI of another format; an error, which is the
    /// list to answer with, when the plugin is not installed or (unless
    /// `scan` says to wait for it) plugins have not been scanned yet.
    fn preset_source(&self, uri: &str, scan: bool) -> Option<Result<PluginRef, PresetList>> {
        let (CLAP_SCHEME, plugin_id) = split_uri(uri)? else { return None };
        Some(self.shared.installed.preset_source(plugin_id, scan))
    }

    /// Looks for plugins again. With [`ScanMode::UseCache`] only new and
    /// changed files are scanned, which is how newly installed plugins are
    /// picked up; [`ScanMode::Fresh`] also retries the files that failed.
    ///
    /// Blocks until the scan is done. Devices that already exist are not
    /// affected.
    pub fn scan(&self, mode: ScanMode) {
        self.shared.installed.scan(mode);
    }

    /// The plugin files that could not be scanned, with the reason. Their
    /// plugins are missing from [`descriptors`](DeviceFactory::descriptors).
    pub fn scan_failures(&self) -> Vec<ScanFailure> {
        self.shared.installed.catalog().failures.clone()
    }
}

#[cfg(test)]
impl ClapFactory {
    /// A factory whose catalog is `entries`, plugins compiled into the test
    /// binary, instead of the result of a scan.
    pub(crate) fn with_entries(entries: &[&'static clack_host::entry::EntryDescriptor]) -> Self {
        let config = ClapConfig {
            search_paths: Vec::new(),
            scanner: Scanner::InProcess,
            cache_path: None,
            scan_timeout: Duration::from_secs(5),
        };
        let factory = Self::new(config).expect("the plugin main thread starts");
        let mut plugins = Vec::new();
        for &descriptor in entries {
            let (path, records) = factory
                .shared
                .main
                .call(Duration::from_secs(10), move |thread| thread.clap.preload(descriptor))
                .expect("the plugin main thread answers")
                .expect("the scripted plugin loads");
            plugins.extend(records.into_iter().map(|record| (path.clone(), record)));
        }
        factory.shared.installed.set_scanned(scan::ScanResults { plugins, failures: Vec::new() });
        factory
    }
}

impl DeviceFactory for ClapFactory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.shared.installed.catalog().descriptors.clone()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        let plugin_id = match split_uri(&device.uri) {
            Some((CLAP_SCHEME, plugin_id)) => plugin_id,
            // Not ours: the application chains factories and tries the next.
            _ => return Err(DeviceError::Unknown(device.uri.clone())),
        };
        let failed = |reason: String| DeviceError::LoadFailed { uri: device.uri.clone(), reason };

        let (main, catalog) = (&self.shared.main, self.shared.installed.catalog());
        let installed = catalog.find(plugin_id).map_err(failed)?;

        let request = CreateRequest {
            path: installed.path.clone(),
            plugin_id: plugin_id.to_string(),
            name: installed.name.clone(),
            state: device.state.clone(),
            // Parameter keys are CLAP parameter ids. Anything else was not
            // written by this host and has no meaning to the plugin.
            values: device.params.iter().filter_map(|(key, value)| Some((key.parse().ok()?, *value))).collect(),
            sample_rate,
            max_block: u32::try_from(max_block).unwrap_or(u32::MAX),
        };

        let created = instantiate(main, request, CREATE_TIMEOUT).map_err(failed)?;

        let params = created.params.iter().map(|spec| spec.info.clone()).collect();
        let controller = ClapController::new(
            main.clone(),
            created.id,
            plugin_id.to_string(),
            created.editor.clone(),
            created.feedback.clone(),
        );
        let processor = ClapProcessor::new(
            main.clone(),
            plugin_id.to_string(),
            installed.instrument,
            created,
            sample_rate,
            max_block,
        );
        Ok(DeviceInstance { processor: Box::new(processor), params, controller: Some(Box::new(controller)) })
    }

    /// The presets the plugin's preset-discovery providers tell of. They
    /// are read in the scanner process the first time they are asked for,
    /// and from the cache after that.
    fn presets(&self, uri: &str) -> Option<PresetList> {
        Some(match self.preset_source(uri, false)? {
            Ok(plugin) => self.shared.presets.list(&plugin),
            Err(list) => list,
        })
    }

    fn presets_within(&self, uri: &str, timeout: Duration) -> Option<PresetList> {
        Some(match self.preset_source(uri, true)? {
            Ok(plugin) => self.shared.presets.wait(&plugin, timeout),
            Err(list) => list,
        })
    }

    fn refresh_presets(&self, uri: &str) {
        if let Some(Ok(plugin)) = self.preset_source(uri, false) {
            self.shared.presets.refresh(&plugin);
        }
    }
}

/// Creates an instance on the plugin main thread, giving up after
/// `timeout`. An instance that arrives after that is destroyed.
fn instantiate(main: &MainThread<PluginThread>, request: CreateRequest, timeout: Duration) -> Result<Created, String> {
    plugin_thread::instantiate(
        main,
        timeout,
        move |thread| thread.clap.create(request),
        |thread, created: Created| thread.clap.destroy(created.id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clap::scripted::{self, kind};
    use crate::test_support::TestDir;
    use trill_core::{ChainRef, Project};

    /// A factory that sees no plugins and touches nothing outside `dir`.
    fn empty_factory(dir: &TestDir) -> ClapFactory {
        ClapFactory::new(ClapConfig {
            search_paths: vec![dir.path().join("plugins")],
            scanner: Scanner::Auto,
            cache_path: Some(dir.path().join("cache.json")),
            scan_timeout: Duration::from_secs(5),
        })
        .unwrap()
    }

    fn device(uri: &str) -> Device {
        let mut project = Project::new("t");
        let id = project.add_device(ChainRef::Master, uri, "x", None).unwrap();
        project.device(id).unwrap().clone()
    }

    #[test]
    fn the_factory_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ClapFactory>();
    }

    #[test]
    fn the_instrument_feature_makes_an_instrument() {
        let record = |features: &[&str]| scan::PluginRecord {
            id: "com.acme.x".into(),
            name: "X".into(),
            vendor: "Acme".into(),
            version: "1.0".into(),
            description: "About X".into(),
            features: features.iter().map(|feature| feature.to_string()).collect(),
        };
        assert!((FORMAT.is_instrument)(&record(&["stereo", "instrument"])));
        assert!(!(FORMAT.is_instrument)(&record(&["audio-effect", "reverb"])));
        assert!(!(FORMAT.is_instrument)(&record(&[])));
        assert_eq!((FORMAT.description)(&record(&[])), "About X", "a plugin is listed with its own description");
    }

    #[test]
    fn other_schemes_are_unknown_so_factories_can_be_chained() {
        let dir = TestDir::new();
        let factory = empty_factory(&dir);
        for uri in ["builtin:polysynth", "vst3:ABCDEF", "no-scheme", "", "CLAP:upper.case"] {
            assert!(
                matches!(factory.create(&device(uri), 48_000.0, 512), Err(DeviceError::Unknown(u)) if u == uri),
                "{uri}"
            );
        }
    }

    #[test]
    fn a_missing_plugin_is_a_load_failure_with_a_reason() {
        let dir = TestDir::new();
        let factory = empty_factory(&dir);
        match factory.create(&device("clap:com.acme.not-installed"), 48_000.0, 512) {
            Err(DeviceError::LoadFailed { uri, reason }) => {
                assert_eq!(uri, "clap:com.acme.not-installed");
                assert!(reason.contains("not installed"), "{reason}");
            }
            other => panic!("expected a load failure, got {:?}", other.err()),
        }
    }

    #[test]
    fn an_empty_system_has_no_devices_and_no_failures() {
        let dir = TestDir::new();
        let factory = empty_factory(&dir);
        assert!(factory.descriptors().is_empty());
        assert!(factory.scan_failures().is_empty());
        factory.scan(ScanMode::Fresh);
        assert!(factory.descriptors().is_empty());
    }

    #[test]
    fn an_instance_that_arrives_after_its_caller_gave_up_is_destroyed() {
        let factory = ClapFactory::with_entries(&[scripted::entry(kind::SLOW_INIT)]);
        let catalog = factory.shared.installed.catalog();
        let installed = catalog.find(scripted::id(kind::SLOW_INIT)).unwrap_or_else(|reason| panic!("{reason}"));
        let request = || CreateRequest {
            path: installed.path.clone(),
            plugin_id: scripted::id(kind::SLOW_INIT).to_string(),
            name: installed.name.clone(),
            state: None,
            values: Vec::new(),
            sample_rate: 48_000.0,
            max_block: 64,
        };
        let main = &factory.shared.main;

        let error = instantiate(main, request(), Duration::from_millis(20)).err().expect("a timeout");
        assert!(error.contains("too long"), "{error}");
        // A patient caller, queued behind the slow job, gets its instance.
        let created = instantiate(main, request(), Duration::from_secs(30)).unwrap_or_else(|error| panic!("{error}"));
        main.call(Duration::from_secs(30), move |thread| thread.clap.destroy(created.id)).unwrap();

        // Both instances existed, and both are gone again. The first one
        // was never seen by anyone but the plugin thread.
        let serials = scripted::instances(kind::SLOW_INIT);
        assert_eq!(serials.len(), 2);
        for serial in serials {
            assert_eq!(scripted::lifecycle(serial), ["init", "activate", "deactivate", "destroy"]);
        }
    }

    #[test]
    fn a_broken_plugin_file_is_reported_and_remembered() {
        let dir = TestDir::new();
        let broken = dir.file("plugins/broken.clap", b"this is not a shared library");
        let factory = empty_factory(&dir);

        assert!(factory.descriptors().is_empty());
        let failures = factory.scan_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path, broken);
        assert!(!failures[0].reason.is_empty());

        // A second factory finds the failure in the cache.
        let again = empty_factory(&dir);
        assert_eq!(again.scan_failures(), failures);
    }
}
