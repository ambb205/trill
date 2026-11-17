//! One entry point for applications: every plugin format behind one
//! object.

use crate::clap::{ClapConfig, ClapFactory};
use crate::scan::{ScanFailure, ScanMode, Scanner};
use crate::vst3::{Vst3Config, Vst3Factory};
use trill_core::device::DeviceFactory;

/// The plugin formats the DAW hosts, set up together.
///
/// An application that just wants "the user's plugins" makes one of these
/// instead of a factory per format: it scans them all with one scanner
/// choice and hands out their factories to chain behind the built-in
/// devices. The formats' own factories stay available, and usable on their
/// own, for anything format-specific.
///
/// Cloning is cheap and yields a handle to the same host.
#[derive(Clone)]
pub struct PluginHost {
    clap: ClapFactory,
    vst3: Vst3Factory,
}

impl PluginHost {
    /// A host for the plugins installed in each format's standard
    /// locations, with the default cache and timeouts, scanning with
    /// `scanner`. No plugin is scanned or loaded until the host is first
    /// used.
    ///
    /// Fails only if the operating system cannot start a thread.
    pub fn new(scanner: Scanner) -> std::io::Result<Self> {
        Self::with_configs(
            ClapConfig { scanner: scanner.clone(), ..ClapConfig::default() },
            Vst3Config { scanner, ..Vst3Config::default() },
        )
    }

    /// A host with each format configured separately.
    pub fn with_configs(clap: ClapConfig, vst3: Vst3Config) -> std::io::Result<Self> {
        Ok(Self { clap: ClapFactory::new(clap)?, vst3: Vst3Factory::new(vst3)? })
    }

    /// The CLAP side of the host.
    pub fn clap(&self) -> &ClapFactory {
        &self.clap
    }

    /// The VST3 side of the host.
    pub fn vst3(&self) -> &Vst3Factory {
        &self.vst3
    }

    /// A device factory per format, to put into a
    /// [`CompositeFactory`](trill_core::device::CompositeFactory). They share
    /// their scans with this host, which stays usable for rescanning.
    pub fn factories(&self) -> Vec<Box<dyn DeviceFactory>> {
        vec![Box::new(self.clap.clone()), Box::new(self.vst3.clone())]
    }

    /// Looks for plugins of every format again; see
    /// [`ClapFactory::scan`]. Blocks until all scans are done.
    pub fn scan(&self, mode: ScanMode) {
        self.clap.scan(mode);
        self.vst3.scan(mode);
    }

    /// The plugin files of any format that could not be scanned, with the
    /// reason.
    pub fn scan_failures(&self) -> Vec<ScanFailure> {
        let mut failures = self.clap.scan_failures();
        failures.extend(self.vst3.scan_failures());
        failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use std::time::Duration;
    use trill_core::device::{CompositeFactory, DeviceError};
    use trill_core::{ChainRef, Project};

    /// A host that looks for plugins of both formats in `dir` only.
    fn host(dir: &TestDir) -> PluginHost {
        let clap = ClapConfig {
            search_paths: vec![dir.path().join("plugins")],
            scanner: Scanner::InProcess,
            cache_path: Some(dir.path().join("clap.json")),
            scan_timeout: Duration::from_secs(5),
        };
        let vst3 = Vst3Config {
            search_paths: clap.search_paths.clone(),
            scanner: Scanner::InProcess,
            cache_path: Some(dir.path().join("vst3.json")),
            scan_timeout: Duration::from_secs(5),
            preset_paths: Vec::new(),
            vendor_places: crate::VendorPlaces::default(),
            patient_teardown: true,
        };
        PluginHost::with_configs(clap, vst3).unwrap()
    }

    #[test]
    fn the_host_is_shareable_across_threads() {
        fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
        assert_send_sync_clone::<PluginHost>();
    }

    #[test]
    fn both_formats_are_scanned_and_report_their_failures_together() {
        let dir = TestDir::new();
        let broken_clap = dir.file("plugins/broken.clap", b"this is not a shared library");
        let host = host(&dir);
        assert!(host.scan_failures().iter().any(|failure| failure.path == broken_clap));
        assert_eq!(host.scan_failures().len(), host.clap().scan_failures().len() + host.vst3().scan_failures().len());

        // Each format keeps its own cache.
        host.scan(ScanMode::Fresh);
        assert!(dir.path().join("clap.json").is_file());
        assert!(dir.path().join("vst3.json").is_file());
    }

    #[test]
    fn the_factories_chain_so_each_format_gets_its_own_uris() {
        let dir = TestDir::new();
        let host = host(&dir);
        let factories = host.factories();
        assert_eq!(factories.len(), 2);
        let devices = CompositeFactory::new(factories);
        assert!(devices.descriptors().is_empty());

        let create = |uri: &str| {
            let mut project = Project::new("t");
            let id = project.add_device(ChainRef::Master, uri, "x", None).unwrap();
            devices.create(project.device(id).unwrap(), 48_000.0, 512).err().expect("nothing is installed")
        };
        // A URI of a hosted format is answered by that format's factory;
        // anything else by none of them.
        assert!(matches!(create("clap:com.acme.synth"), DeviceError::LoadFailed { .. }));
        assert!(matches!(create("vst3:0123456789ABCDEF0123456789ABCDEF"), DeviceError::LoadFailed { .. }));
        assert!(matches!(create("lv2:urn:acme:synth"), DeviceError::Unknown(_)));
    }
}
