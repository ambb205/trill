//! The device factory for `vst3:` URIs.

use super::class_id::ClassId;
use super::controller::Vst3Controller;
use super::instance::{CreateRequest, Created};
use super::processor::Vst3Processor;
use super::{VST3_SCHEME, find_plugin_files, probe, standard_search_paths};
use crate::main_thread::MainThread;
use crate::plugin_thread::{self, CREATE_TIMEOUT, PluginThread, plugin_thread};
use crate::presets::store::{PluginRef, PresetFormat, PresetStore};
use crate::scan::catalog::{Format, Inventory, Settings};
use crate::scan::{self, ScanFailure, ScanMode, Scanner};
use crate::vendor::VendorPlaces;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use trill_core::Device;
use trill_core::device::{DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, Purpose, split_uri};
use trill_core::presets::PresetList;

/// The VST3 sub-category that marks a plugin as an instrument.
const INSTRUMENT_CATEGORY: &str = "Instrument";
/// File name of the scan cache inside the application's cache directory.
const CACHE_FILE: &str = "vst3-plugins.json";

/// How VST3 plugins are found and listed.
static FORMAT: Format = Format {
    name: "VST3",
    scheme: VST3_SCHEME,
    find_files: find_plugin_files,
    probe,
    is_instrument: |record| record.features.iter().any(|category| category == INSTRUMENT_CATEGORY),
    // A VST3 class describes itself only by its sub-categories.
    description: |record| match record.features.as_slice() {
        [] => "VST3 plugin".to_string(),
        categories => format!("VST3 plugin ({})", categories.join(", ")),
    },
};

/// How the presets of VST3 plugins are found: preset files in the
/// format's folders, and the programs a plugin lists itself.
static PRESETS: PresetFormat = PresetFormat {
    scheme: VST3_SCHEME,
    read_files: super::presets::preset_files,
    read_in_thread: |thread, path, class, sink| thread.vst3.read_presets(path, class, sink),
};

/// Where and how [`Vst3Factory`] looks for plugins.
#[derive(Clone, Debug)]
pub struct Vst3Config {
    /// Directories searched, recursively, for `.vst3` bundles and
    /// single-file plugins. Earlier directories win when a plugin is
    /// installed more than once.
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
    /// The folders `.vstpreset` files are looked for in, the user's own
    /// first. A plugin's presets are in `<vendor>/<plugin name>` below
    /// each, in whatever folders the user sorts them into.
    pub preset_paths: Vec<PathBuf>,
    /// Where vendors keep presets in formats of their own, for the
    /// plugins whose vendor's format is read ([`VendorPlaces`]).
    pub vendor_places: VendorPlaces,
}

impl Default for Vst3Config {
    /// The standard VST3 directories of this system, plugins and presets,
    /// and a cache in the user's cache directory.
    fn default() -> Self {
        Self {
            search_paths: standard_search_paths(),
            scanner: Scanner::Auto,
            cache_path: scan::cache::default_directory().map(|directory| directory.join(CACHE_FILE)),
            scan_timeout: Duration::from_secs(20),
            preset_paths: super::standard_preset_paths(),
            vendor_places: VendorPlaces::system(),
        }
    }
}

impl From<Vst3Config> for Settings {
    /// Everything but the preset folders, which are the preset store's.
    fn from(config: Vst3Config) -> Self {
        Self {
            search_paths: config.search_paths,
            scanner: config.scanner,
            cache_path: config.cache_path,
            scan_timeout: config.scan_timeout,
        }
    }
}

/// Makes installed VST3 plugins available as devices.
///
/// A device's URI is `vst3:` followed by the class id of the plugin's
/// processor, as 32 hexadecimal digits: the form in which the VST3 SDK
/// prints class ids, and the same on every platform.
///
/// Plugins are found by scanning the configured directories. The first scan
/// happens on first use, or when [`scan`](Self::scan) is called; later
/// calls reuse its result, so [`descriptors`](DeviceFactory::descriptors)
/// is cheap.
///
/// The factory may be used from any thread. All plugin main-thread work is
/// carried out on the process's plugin main thread, the one CLAP plugins
/// use too, which runs for as long as a factory or a device created by one
/// exists, and for the rest of the process once a plugin has shown its
/// editor.
///
/// Cloning is cheap and yields a handle to the same factory: keep one to
/// scan and report with, and give another to the engine.
#[derive(Clone)]
pub struct Vst3Factory {
    shared: Arc<Shared>,
}

struct Shared {
    main: MainThread<PluginThread>,
    installed: Inventory,
    presets: PresetStore,
    /// Where a vendor's preset is made into a state: in the scanner
    /// process, like everything that reads files Trill did not write.
    scanner: Scanner,
}

impl Vst3Factory {
    /// Starts the plugin main thread if it is not running yet. No plugin is
    /// scanned or loaded until the factory is first used.
    ///
    /// Fails only if the operating system cannot start a thread.
    pub fn new(mut config: Vst3Config) -> std::io::Result<Self> {
        let main = plugin_thread()?;
        let folders = std::mem::take(&mut config.preset_paths);
        let places = std::mem::take(&mut config.vendor_places);
        let scanner = config.scanner.clone();
        let settings: Settings = config.into();
        let presets = PresetStore::new(&PRESETS, settings.preset_settings(folders, places), main.clone());
        let installed = Inventory::new(&FORMAT, settings, main.clone());
        Ok(Self { shared: Arc::new(Shared { main, installed, presets, scanner }) })
    }

    /// The installed plugin `uri` names, as the plugin whose presets are
    /// wanted. `None` for a URI of another format; an error, which is the
    /// list to answer with, when the plugin is not installed or (unless
    /// `scan` says to wait for it) plugins have not been scanned yet.
    fn preset_source(&self, uri: &str, scan: bool) -> Option<Result<PluginRef, PresetList>> {
        let (VST3_SCHEME, class) = split_uri(uri)? else { return None };
        // Written out again, the id has the catalog's case whatever the
        // URI's was. What is no class id names no plugin.
        let Some(class) = ClassId::parse(class) else { return Some(Err(PresetList::default())) };
        Some(self.shared.installed.preset_source(&class.to_string(), scan))
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
impl Vst3Factory {
    /// A factory whose catalog is `modules`, plugin factories compiled into
    /// the test binary, each with a name to tell it by, instead of the
    /// result of a scan.
    pub(crate) fn with_modules(modules: Vec<(&'static str, vst3::ComPtr<vst3::Steinberg::IPluginFactory>)>) -> Self {
        let config = Vst3Config {
            search_paths: Vec::new(),
            scanner: Scanner::InProcess,
            cache_path: None,
            scan_timeout: Duration::from_secs(5),
            preset_paths: Vec::new(),
            vendor_places: VendorPlaces::default(),
        };
        Self::with_modules_in(config, modules)
    }

    /// [`with_modules`](Self::with_modules), configured by `config`.
    pub(crate) fn with_modules_in(
        config: Vst3Config,
        modules: Vec<(&'static str, vst3::ComPtr<vst3::Steinberg::IPluginFactory>)>,
    ) -> Self {
        let factory = Self::new(config).expect("the plugin main thread starts");
        let mut plugins = Vec::new();
        for (name, module) in modules {
            let (path, records) = factory
                .shared
                .main
                .call(Duration::from_secs(10), move |thread| thread.vst3.preload(name, module))
                .expect("the plugin main thread answers");
            plugins.extend(records.into_iter().map(|record| (path.clone(), record)));
        }
        factory.shared.installed.set_scanned(scan::ScanResults { plugins, failures: Vec::new() });
        factory
    }
}

impl DeviceFactory for Vst3Factory {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.shared.installed.catalog().descriptors.clone()
    }

    fn create(&self, device: &Device, sample_rate: f64, max_block: usize) -> Result<DeviceInstance, DeviceError> {
        self.create_for(device, sample_rate, max_block, Purpose::Playing)
    }

    fn create_for(
        &self,
        device: &Device,
        sample_rate: f64,
        max_block: usize,
        purpose: Purpose,
    ) -> Result<DeviceInstance, DeviceError> {
        let class = match split_uri(&device.uri) {
            Some((VST3_SCHEME, class)) => class,
            // Not ours: the application chains factories and tries the next.
            _ => return Err(DeviceError::Unknown(device.uri.clone())),
        };
        let failed = |reason: String| DeviceError::LoadFailed { uri: device.uri.clone(), reason };
        let class = ClassId::parse(class)
            .ok_or_else(|| failed(format!("'{class}' is not a VST3 class id (32 hexadecimal digits)")))?;

        let (main, catalog) = (&self.shared.main, self.shared.installed.catalog());
        // Written out again, the id has the catalog's case whatever the
        // URI's was.
        let installed = catalog.find(&class.to_string()).map_err(failed)?;

        let request = CreateRequest {
            path: installed.path.clone(),
            class,
            name: installed.name.clone(),
            state: device.state.clone(),
            // Parameter keys are VST3 parameter ids. Anything else was not
            // written by this host and has no meaning to the plugin.
            values: device.params.iter().filter_map(|(key, value)| Some((key.parse().ok()?, *value))).collect(),
            sample_rate,
            max_block: u32::try_from(max_block).unwrap_or(u32::MAX),
            offline: purpose == Purpose::Rendering,
        };

        let created = instantiate(main, request, CREATE_TIMEOUT).map_err(failed)?;

        let params = created.params.iter().map(|spec| spec.info.clone()).collect();
        let scanner = self.shared.scanner.clone();
        let controller = Vst3Controller::new(main.clone(), installed.name.clone(), &created, class, scanner);
        let processor = Vst3Processor::new(
            main.clone(),
            installed.name.clone(),
            installed.instrument,
            created,
            sample_rate,
            max_block,
        )
        .map_err(failed)?;
        Ok(DeviceInstance { processor: Box::new(processor), params, controller: Some(Box::new(controller)) })
    }

    /// The plugin's preset files in the format's preset folders, and the
    /// programs of its program lists. The files are found without the
    /// plugin; the lists are read in the scanner process the first time
    /// they are asked for, and from the cache after that.
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

    fn known_presets(&self, uri: &str) -> Option<PresetList> {
        match self.preset_source(uri, false)? {
            Ok(plugin) => self.shared.presets.known(&plugin),
            Err(_) => None,
        }
    }
}

/// Creates an instance on the plugin main thread, giving up after
/// `timeout`. An instance that arrives after that is destroyed.
fn instantiate(main: &MainThread<PluginThread>, request: CreateRequest, timeout: Duration) -> Result<Created, String> {
    plugin_thread::instantiate(
        main,
        timeout,
        move |thread| thread.vst3.create(request),
        |thread, created: Created| thread.vst3.destroy(created.id, Some(created.audio)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use trill_core::{ChainRef, Project};

    /// A class id no plugin has.
    const NOBODY: &str = "0123456789ABCDEF0123456789ABCDEF";

    /// A factory that sees no plugins and touches nothing outside `dir`.
    fn empty_factory(dir: &TestDir) -> Vst3Factory {
        Vst3Factory::new(Vst3Config {
            search_paths: vec![dir.path().join("plugins")],
            scanner: Scanner::Auto,
            cache_path: Some(dir.path().join("cache.json")),
            scan_timeout: Duration::from_secs(5),
            preset_paths: Vec::new(),
            vendor_places: VendorPlaces::default(),
        })
        .unwrap()
    }

    fn device(uri: &str) -> Device {
        let mut project = Project::new("t");
        let id = project.add_device(ChainRef::Master, uri, "x", None).unwrap();
        project.device(id).unwrap().clone()
    }

    fn load_failure(factory: &Vst3Factory, uri: &str) -> String {
        match factory.create(&device(uri), 48_000.0, 512) {
            Err(DeviceError::LoadFailed { uri: failed, reason }) => {
                assert_eq!(failed, uri);
                reason
            }
            other => panic!("expected a load failure, got {:?}", other.err()),
        }
    }

    #[test]
    fn the_factory_is_shareable_across_threads() {
        fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
        assert_send_sync_clone::<Vst3Factory>();
        fn assert_send<T: Send>() {}
        assert_send::<Vst3Processor>();
        assert_send::<Vst3Controller>();
    }

    #[test]
    fn plugins_are_classified_and_described_by_their_sub_categories() {
        let record = |features: &[&str]| scan::PluginRecord {
            id: NOBODY.into(),
            name: "X".into(),
            vendor: "Acme".into(),
            version: "1.0".into(),
            description: String::new(),
            features: features.iter().map(|feature| feature.to_string()).collect(),
        };
        assert!((FORMAT.is_instrument)(&record(&["Instrument", "Synth"])));
        assert!((FORMAT.is_instrument)(&record(&["Fx", "Instrument"])));
        assert!(!(FORMAT.is_instrument)(&record(&["Fx", "Reverb"])));
        assert!(!(FORMAT.is_instrument)(&record(&["instrument"])), "sub-categories are case-sensitive");
        assert!(!(FORMAT.is_instrument)(&record(&[])));
        assert_eq!((FORMAT.description)(&record(&["Instrument", "Synth"])), "VST3 plugin (Instrument, Synth)");
        assert_eq!((FORMAT.description)(&record(&[])), "VST3 plugin");
    }

    #[test]
    fn other_schemes_are_unknown_so_factories_can_be_chained() {
        let dir = TestDir::new();
        let factory = empty_factory(&dir);
        for uri in
            ["builtin:polysynth", "clap:com.acme.synth", "no-scheme", "", "VST3:0123456789ABCDEF0123456789ABCDEF"]
        {
            assert!(
                matches!(factory.create(&device(uri), 48_000.0, 512), Err(DeviceError::Unknown(u)) if u == uri),
                "{uri}"
            );
        }
    }

    #[test]
    fn a_malformed_class_id_is_a_load_failure_that_says_so() {
        let dir = TestDir::new();
        let factory = empty_factory(&dir);
        for uri in ["vst3:", "vst3:ABCDEF", "vst3:com.acme.synth", "vst3:0123456789ABCDEF0123456789ABCDEFG"] {
            let reason = load_failure(&factory, uri);
            assert!(reason.contains("not a VST3 class id"), "{uri}: {reason}");
        }
    }

    #[test]
    fn a_missing_plugin_is_a_load_failure_with_a_reason() {
        let dir = TestDir::new();
        let factory = empty_factory(&dir);
        for uri in [format!("vst3:{NOBODY}"), format!("vst3:{}", NOBODY.to_lowercase())] {
            let reason = load_failure(&factory, &uri);
            assert!(reason.contains("not installed"), "{reason}");
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
        use crate::vst3::scripted::{self, kind};
        let factory = Vst3Factory::with_modules(vec![("scripted-slow", scripted::factory())]);
        let uri = scripted::uri(kind::SLOW_INIT);
        let class = ClassId::parse(uri.strip_prefix("vst3:").unwrap()).unwrap();
        let catalog = factory.shared.installed.catalog();
        let installed = catalog.find(&class.to_string()).unwrap_or_else(|reason| panic!("{reason}"));
        let request = || CreateRequest {
            path: installed.path.clone(),
            class,
            name: installed.name.clone(),
            state: None,
            values: Vec::new(),
            sample_rate: 48_000.0,
            max_block: 64,
            offline: false,
        };
        let main = &factory.shared.main;
        let before = scripted::instances(kind::SLOW_INIT).len();

        let error = instantiate(main, request(), Duration::from_millis(20)).err().expect("a timeout");
        assert!(error.contains("too long"), "{error}");
        // A patient caller, queued behind the slow job, gets its instance.
        let created = instantiate(main, request(), Duration::from_secs(30)).unwrap_or_else(|error| panic!("{error}"));
        main.call(Duration::from_secs(30), move |thread| thread.vst3.destroy(created.id, Some(created.audio))).unwrap();

        // Both instances existed, and both are gone again. The first one
        // was never seen by anyone but the plugin thread.
        let serials = scripted::instances(kind::SLOW_INIT);
        assert_eq!(serials.len(), before + 2);
        for serial in &serials[before..] {
            let steps = scripted::lifecycle(*serial);
            assert!(steps.contains(&"activate"), "{steps:?}");
            assert_eq!(
                steps[steps.len() - 4..],
                ["deactivate", "handler cleared", "terminate", "destroy"],
                "{steps:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_broken_plugin_file_is_reported_and_remembered() {
        let dir = TestDir::new();
        let broken = dir.file("plugins/broken.vst3", b"this is not a shared library");
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
