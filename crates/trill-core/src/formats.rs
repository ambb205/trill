//! What a file of an older format meant by what it does not say.
//!
//! A project file holds the parameters of a device that were set, and
//! leaves the rest to the device's defaults. So when a built-in device is
//! changed in a way that would be heard, a file from before would sound
//! different without anything in it having changed. This module is where
//! that is put right: for each such device, the values that give it the
//! sound it had, written into the device as the file is read
//! ([`Project::from_json`](crate::Project::from_json)), and into a preset
//! that was saved from one ([`SavedPreset`](crate::presets::SavedPreset)).
//!
//! The rule for whoever changes a built-in device: a new parameter either
//! defaults to what the device did before, or the device gets a line here.

use std::collections::BTreeMap;

/// The parameter that keeps a built-in device sounding as it did in a song
/// of format 1.
pub const LEGACY: &str = "legacy";

/// The built-in devices that sound different from format 2 on, each with
/// the values that give it the sound it had in format 1:
///
/// * the delay's ping-pong fed back on every bounce and filtered only what
///   it fed back;
/// * the reverb returned 8 to 11 dB more, filtered what came out of it, and
///   had no high cut at all.
pub const FORMAT_1: &[(&str, &[(&str, f64)])] =
    &[("builtin:delay", &[(LEGACY, 1.0)]), ("builtin:reverb", &[(LEGACY, 1.0), ("high_cut", 20_000.0)])];

/// Gives the parameters of a device of type `uri`, as a file of format 1
/// has them, the values that file left unsaid. Nothing the file does say is
/// touched, and a device that sounds as it always did is left alone.
pub fn keep_format_1(uri: &str, params: &mut BTreeMap<String, f64>) {
    let Some((_, values)) = FORMAT_1.iter().find(|(device, _)| *device == uri) else { return };
    for (id, value) in *values {
        params.entry((*id).to_string()).or_insert(*value);
    }
}

/// [`keep_format_1`] for a set of parameter values that holds every
/// parameter the device had when it was written, as a saved preset of a
/// built-in device does: one without a `legacy` value was saved before
/// there was any.
pub fn keep_complete_format_1(uri: &str, params: &mut BTreeMap<String, f64>) {
    if !params.contains_key(LEGACY) {
        keep_format_1(uri, params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_of_format_1_is_given_what_its_file_left_unsaid_and_nothing_else() {
        let mut reverb = BTreeMap::from([("decay".to_string(), 0.7)]);
        keep_format_1("builtin:reverb", &mut reverb);
        assert_eq!(reverb.get(LEGACY), Some(&1.0));
        assert_eq!(reverb.get("high_cut"), Some(&20_000.0));
        assert_eq!(reverb.get("decay"), Some(&0.7));
        assert_eq!(reverb.len(), 3);

        // What a file says is the file's.
        let mut said = BTreeMap::from([(LEGACY.to_string(), 0.0)]);
        keep_format_1("builtin:delay", &mut said);
        assert_eq!(said.get(LEGACY), Some(&0.0));

        // A device that did not change, and one that is not built in.
        for uri in ["builtin:eq", "builtin:compressor", "builtin:polysynth", "clap:com.example.reverb", "vst3:ABCDEF"] {
            let mut params = BTreeMap::new();
            keep_format_1(uri, &mut params);
            assert!(params.is_empty(), "{uri}");
        }
    }

    #[test]
    fn a_complete_set_of_values_says_by_itself_whether_it_is_old() {
        let mut old = BTreeMap::from([("feedback".to_string(), 0.5)]);
        keep_complete_format_1("builtin:delay", &mut old);
        assert_eq!(old.get(LEGACY), Some(&1.0));
        let mut new = BTreeMap::from([("feedback".to_string(), 0.5), (LEGACY.to_string(), 0.0)]);
        keep_complete_format_1("builtin:delay", &mut new);
        assert_eq!(new.get(LEGACY), Some(&0.0));
    }
}
