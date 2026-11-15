//! Reads the descriptors of the plugins in a CLAP file.

use crate::scan::PluginRecord;
use clack_host::plugin::PluginDescriptor;
use clack_host::prelude::PluginEntry;
use std::ffi::CStr;
use std::path::Path;

/// Loads the CLAP file at `path` into the current process and lists the
/// plugins it contains. The library is unloaded again before returning.
///
/// See [`crate::scan::probe_file`] for why the DAW does this in a child
/// process.
pub(crate) fn probe(path: &Path) -> Result<Vec<PluginRecord>, String> {
    // SAFETY: loading a plugin runs code this crate has no control over;
    // no check made here could make that sound. The CLAP ABI is trusted for
    // files named `.clap`, and the damage a plugin that violates it can do
    // is contained by running this function in the scanner process.
    let entry = unsafe { PluginEntry::load(path) }.map_err(|error| error.to_string())?;
    records(&entry)
}

/// Lists the plugins a loaded CLAP library contains.
pub(crate) fn records(entry: &PluginEntry) -> Result<Vec<PluginRecord>, String> {
    let factory = entry.get_plugin_factory().ok_or("the file does not provide a CLAP plugin factory")?;
    Ok(factory.plugin_descriptors().filter_map(record).collect())
}

/// Plugin strings are meant to be UTF-8, but nothing enforces it.
fn text(value: Option<&CStr>) -> String {
    value.map(|value| value.to_string_lossy().into_owned()).unwrap_or_default()
}

fn record(descriptor: &PluginDescriptor) -> Option<PluginRecord> {
    // Without an id the plugin could never be instantiated.
    let id = text(descriptor.id());
    if id.is_empty() {
        log::debug!("ignoring a CLAP plugin without an id");
        return None;
    }
    Some(PluginRecord {
        name: name_or_id(text(descriptor.name()), &id),
        id,
        vendor: text(descriptor.vendor()),
        version: text(descriptor.version()),
        description: text(descriptor.description()),
        features: descriptor.features().map(|feature| feature.to_string_lossy().into_owned()).collect(),
    })
}

/// A plugin has to be listed under something.
fn name_or_id(name: String, id: &str) -> String {
    if name.trim().is_empty() { id.to_string() } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn a_file_that_is_not_a_library_is_an_error() {
        let dir = TestDir::new();
        let path = dir.file("fake.clap", b"this is not a shared library");
        assert!(probe(&path).is_err());
        assert!(probe(&dir.path().join("missing.clap")).is_err());
    }

    #[test]
    fn descriptors_become_records() {
        let descriptor = PluginDescriptor::new("com.acme.synth", "Acme Synth")
            .with_vendor("Acme")
            .with_version("2.1")
            .with_description("A synth")
            .with_features([c"instrument", c"stereo"]);
        assert_eq!(
            record(&descriptor),
            Some(PluginRecord {
                id: "com.acme.synth".into(),
                name: "Acme Synth".into(),
                vendor: "Acme".into(),
                version: "2.1".into(),
                description: "A synth".into(),
                features: vec!["instrument".into(), "stereo".into()],
            })
        );
    }

    #[test]
    fn a_nameless_plugin_is_listed_under_its_id() {
        assert_eq!(name_or_id(String::new(), "com.acme.x"), "com.acme.x");
        assert_eq!(name_or_id("  ".into(), "com.acme.x"), "com.acme.x");
        assert_eq!(name_or_id("Synth".into(), "com.acme.x"), "Synth");
    }
}
