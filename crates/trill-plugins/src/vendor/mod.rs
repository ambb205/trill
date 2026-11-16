//! Presets that vendors keep in files of their own.
//!
//! Many plugins give a host no presets: no program list, no preset files
//! in the standard folders, no discovery provider. Their presets are
//! files in the vendor's own corner of the disk, in the vendor's own
//! format, and the plugin's own browser is the only thing that lists
//! them. But a preset is a sound, a sound is a state, and a state is what
//! a host hands a plugin. Where a vendor's format can be read, its preset
//! files are listed here like any other presets, and one is loaded by
//! building the state it stands for and handing the plugin that.
//!
//! Each format has a module: what the files are, where they are kept, how
//! a state is made of one, and what a state shows of the preset it came
//! from, which is how a load is known to have worked:
//!
//! * [`serum`]: Serum 2's `.SerumPreset` files (Xfer's container, [`xfer`]).
//! * [`ample`]: the preset files of Ample Sound's guitars and basses, which
//!   are the XML document the plugin's state is.
//! * [`superior`]: the factory kits of Superior Drummer 3's sound
//!   libraries, which are the text the plugin's state is, many to a file.
//! * [`archetype`]: the presets of Neural DSP's Archetype amplifiers,
//!   which are the tree the plugin's state is, in JUCE's binary form.
//!
//! A reader takes files Trill did not write. It reads no more of a file
//! than it has set aside room for, checks every length it is told against
//! what is there, and never trusts nesting to end; and it is run where
//! presets are read anyway, in the scanner process, so that a file which
//! gets past all that costs a message ([`child`]).
//!
//! Nothing is ever written to a vendor's folders.

pub(crate) mod ample;
pub(crate) mod archetype;
mod cbor;
pub(crate) mod child;
mod juce;
#[cfg(test)]
mod loading_tests;
mod look;
mod places;
pub(crate) mod serum;
pub(crate) mod superior;
mod xfer;
mod xml;

pub use child::VENDOR_PRESET_ARGUMENT;
pub(crate) use child::run_builder;
pub use look::{describe_state, split_state};
pub use places::VendorPlaces;

use crate::presets::Sink;
use crate::presets::store::PluginRef;
use crate::vst3::state::PluginState;
/// The two halves of a plugin's state: its processor's and its edit
/// controller's.
pub(crate) use crate::vst3::state::PluginState as States;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

/// The largest preset file read whole. Presets are kilobytes; the largest
/// file of presets seen is fifteen megabytes.
const MAX_FILE: u64 = 256 * 1024 * 1024;

/// The vendors' preset formats that are read.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Format {
    Serum2,
    Ample,
    Superior3,
    Archetype,
}

/// Every format there is.
const FORMATS: [Format; 4] = [Format::Serum2, Format::Ample, Format::Superior3, Format::Archetype];

impl Format {
    /// The format's name in a preset's locator.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Format::Serum2 => "serum2",
            Format::Ample => "ample",
            Format::Superior3 => "superior3",
            Format::Archetype => "archetype",
        }
    }

    pub(crate) fn parse(tag: &str) -> Option<Self> {
        FORMATS.into_iter().find(|format| format.tag() == tag)
    }

    /// The format the vendor of `plugin` keeps its presets in, if it is
    /// one that is read. A plugin is told by its id, or where a vendor
    /// builds all its plugins alike, by the vendor's name.
    pub(crate) fn of(plugin: &PluginRef) -> Option<Self> {
        match plugin.id.as_str() {
            serum::CLASS => Some(Format::Serum2),
            superior::CLASS => Some(Format::Superior3),
            id if ample::is_ours(id) => Some(Format::Ample),
            id if archetype::is_ours(id) => Some(Format::Archetype),
            _ => None,
        }
    }
}

/// One thing a plugin's state shows once the plugin has taken a preset:
/// a setting by its name, and the value the preset gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Mark {
    pub name: String,
    pub value: String,
}

/// What loading a preset takes: the state to hand the plugin, and what
/// to expect of it afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Built {
    pub state: PluginState,
    /// What the plugin's state shows once it has taken the preset.
    pub marks: Vec<Mark>,
    /// What the plugin goes on doing once it has the state, where that
    /// takes long enough to be worth saying: reading samples, as a rule.
    pub note: Option<String>,
}

/// Lists the presets the vendor of `plugin` keeps for it in `places`,
/// with the folders they are in. Nothing for a plugin whose vendor's
/// format is not read, or which has no presets there.
pub(crate) fn list(plugin: &PluginRef, places: &VendorPlaces, sink: &mut dyn Sink) {
    match Format::of(plugin) {
        Some(Format::Serum2) => serum::list(plugin, places, sink),
        Some(Format::Ample) => ample::list(plugin, places, sink),
        Some(Format::Superior3) => superior::list(plugin, places, sink),
        Some(Format::Archetype) => archetype::list(plugin, places, sink),
        None => {}
    }
}

/// Builds the state that makes a plugin the preset in `file`, a preset
/// file of `format`; `key` tells the preset apart in a file that holds
/// several. `current` is the state the plugin has now: some formats only
/// say what a preset changes.
///
/// An error is a sentence about the preset: why it cannot be loaded.
pub(crate) fn build(format: Format, file: &Path, key: Option<&str>, current: &PluginState) -> Result<Built, String> {
    let bytes = read_whole(file)?;
    match format {
        Format::Serum2 => serum::build(&bytes, current),
        Format::Ample => ample::build(&bytes, current),
        Format::Superior3 => superior::build(&bytes, key, current),
        Format::Archetype => archetype::build(&bytes, current),
    }
    .map_err(|reason| format!("'{}' cannot be loaded: {reason}", file.display()))
}

/// The marks of `marks` that `state`, a state of a plugin of `format`,
/// does not show: none once the plugin has taken the preset they belong
/// to.
pub(crate) fn missing<'a>(format: Format, marks: &'a [Mark], state: &PluginState) -> Vec<&'a Mark> {
    let shown = |mark: &Mark| match format {
        Format::Serum2 => serum::shown(state, &mark.name),
        Format::Ample => ample::shown(state, &mark.name),
        Format::Superior3 => superior::shown(state, &mark.name),
        Format::Archetype => archetype::shown(state, &mark.name),
    };
    marks.iter().filter(|mark| shown(mark).is_none_or(|value| !same_value(&value, &mark.value))).collect()
}

/// Whether a plugin whose state lacks `missing` of the `marks` of a preset
/// has taken the preset all the same. A plugin may round a value, or keep
/// a setting it has outgrown at its own; one that shows nine in ten has
/// not ignored what it was handed.
pub(crate) fn took(marks: &[Mark], missing: &[&Mark]) -> bool {
    missing.len() * 10 <= marks.len()
}

/// Whether a plugin shows `shown` for a setting that was given `wanted`:
/// the same text, or the same number written another way (`0.3` for
/// `0.30000001`, `1` for `1.0`).
fn same_value(shown: &str, wanted: &str) -> bool {
    if shown == wanted {
        return true;
    }
    match (shown.trim().parse::<f64>(), wanted.trim().parse::<f64>()) {
        (Ok(shown), Ok(wanted)) => (shown - wanted).abs() <= 1e-4 * wanted.abs().max(1.0),
        _ => false,
    }
}

/// The first `room` bytes of a file, or all of it if it is shorter.
fn read_start(file: &Path, room: usize) -> std::io::Result<Vec<u8>> {
    let mut start = Vec::new();
    std::fs::File::open(file)?.take(room as u64).read_to_end(&mut start)?;
    Ok(start)
}

/// A file, whole, if it is no larger than a file of presets can be.
fn read_whole(file: &Path) -> Result<Vec<u8>, String> {
    let cannot = |error: std::io::Error| format!("'{}' cannot be read: {error}", file.display());
    let size = std::fs::metadata(file).map_err(cannot)?.len();
    if size > MAX_FILE {
        return Err(format!("'{}' is too large to be a preset ({} MB)", file.display(), size / (1024 * 1024)));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file).map_err(cannot)?.take(MAX_FILE).read_to_end(&mut bytes).map_err(cannot)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_the_same_however_the_plugin_writes_the_number() {
        assert!(same_value("Dyn57", "Dyn57") && !same_value("Dyn57", "dyn57"));
        assert!(same_value("0.30000001", "0.3") && same_value("1", "1.0") && same_value("-80", "-80.0"));
        assert!(same_value("20000.5", "20000") && !same_value("0.31", "0.3") && !same_value("true", "1"));
    }

    #[test]
    fn a_preset_is_taken_when_nearly_all_of_it_shows() {
        let marks: Vec<Mark> = (0..40).map(|n| Mark { name: format!("s{n}"), value: n.to_string() }).collect();
        let lacking = |count: usize| took(&marks, &marks.iter().take(count).collect::<Vec<_>>());
        assert!(lacking(0) && lacking(4) && !lacking(5) && !lacking(40));
        // With one thing to show, it shows or it does not.
        assert!(took(&marks[..1], &[]) && !took(&marks[..1], &[&marks[0]]));
        assert!(took(&[], &[]), "a preset that changes nothing has nothing to show");
    }

    #[test]
    fn formats_are_named_in_locators() {
        for format in FORMATS {
            assert_eq!(Format::parse(format.tag()), Some(format));
        }
        assert_eq!(Format::parse("vst3-file"), None);
    }
}
