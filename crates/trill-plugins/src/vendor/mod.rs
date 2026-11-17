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

pub use child::{VENDOR_FILE_ARGUMENT, VENDOR_PRESET_ARGUMENT};
pub(crate) use child::{run_builder, run_lister};
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
use trill_core::presets::PresetInfo;

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
        Self::of_class(&plugin.id)
    }

    /// The extension the preset files of `plugin`, a plugin of this
    /// format, have: in lower case and without the dot. `None` where the
    /// files' names do not tell them from other files (an Archetype's are
    /// called `.xml`, a drum library's file of presets has no extension).
    pub(crate) fn extension(self, plugin: &PluginRef) -> Option<String> {
        match self {
            Format::Serum2 => Some(serum::EXTENSION.to_string()),
            Format::Ample => ample::extension(plugin),
            Format::Superior3 | Format::Archetype => None,
        }
    }

    /// What a preset file of this format is called, for a sentence about
    /// a file that is none.
    pub(crate) fn file_kind(self) -> &'static str {
        match self {
            Format::Serum2 => "a .SerumPreset file",
            Format::Ample => "a preset file of the instrument's own",
            Format::Superior3 => "a file of Superior Drummer presets",
            Format::Archetype => "a preset file of the Archetype's own",
        }
    }

    /// [`of`](Self::of), for a plugin known by its id alone.
    pub(crate) fn of_class(id: &str) -> Option<Self> {
        match id {
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

/// The presets in the file at `file`, taken as a preset file of `format`,
/// wherever it lies: one for most formats, and for a format that keeps
/// many presets to a file all of them. They are as [`list`] has them, but
/// for what only a file's place among the vendor's folders says.
///
/// An error is the end of a sentence about the file: why it is not a
/// preset file of the format. Whether it is a preset of the right plugin
/// of the format's shows when it is loaded ([`build`]).
pub(crate) fn file_presets(format: Format, file: &Path) -> Result<Vec<PresetInfo>, String> {
    match format {
        Format::Serum2 => serum::file_preset(file).map(|preset| vec![preset]),
        Format::Ample => ample::file_preset(file).map(|preset| vec![preset]),
        Format::Superior3 => superior::file_presets(file),
        Format::Archetype => archetype::file_preset(file).map(|preset| vec![preset]),
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

/// Every setting in `state`, a state of a plugin of `format`, by name,
/// with the value it has: for the formats whose state is a list of named
/// settings. `None` for the others, and for a state that is not what the
/// format's reader knows.
pub(crate) fn settings(format: Format, state: &PluginState) -> Option<Vec<(String, String)>> {
    match format {
        Format::Ample => ample::settings(state),
        Format::Serum2 | Format::Superior3 | Format::Archetype => None,
    }
}

/// What the keys of a plugin of `format` play, as `state` says it: for the
/// formats whose state names what is on its keys (a drum sampler's kit).
/// `None` for the others, and for a state the format's reader does not
/// know. What a maker's library calls its drums is read in `places`.
pub(crate) fn key_sounds(
    format: Format,
    state: &PluginState,
    places: &VendorPlaces,
) -> Option<Vec<trill_core::KeySound>> {
    match format {
        Format::Superior3 => superior::key_sounds(state, places),
        Format::Serum2 | Format::Ample | Format::Archetype => None,
    }
}

/// `current` with `values` in place of the values of the settings they
/// name: see [`settings`]. An error says which name is no setting.
pub(crate) fn with_settings(
    format: Format,
    current: &PluginState,
    values: &[(String, String)],
) -> Result<Built, String> {
    match format {
        Format::Ample => ample::with_settings(current, values),
        Format::Serum2 | Format::Superior3 | Format::Archetype => {
            Err("this plugin's state has no settings that are set by name".to_string())
        }
    }
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
    read_bounded(file).map_err(|reason| format!("'{}' {reason}", file.display()))
}

/// [`read_whole`], with an error that is what to say of the file after
/// naming it: "cannot be read", "is too large".
pub(crate) fn read_bounded(file: &Path) -> Result<Vec<u8>, String> {
    let cannot = |error: std::io::Error| format!("cannot be read: {error}");
    let size = std::fs::metadata(file).map_err(cannot)?.len();
    if size > MAX_FILE {
        return Err(format!("is too large to be a preset ({} MB)", size / (1024 * 1024)));
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
