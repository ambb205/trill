//! What a file of an older format meant by what it does not say.
//!
//! A project file holds the parameters of a device that were set, and
//! leaves the rest to the device's defaults. So when a built-in device is
//! changed in a way that would be heard, a file from before would sound
//! different without anything in it having changed. This module is where
//! that is put right: for each such device, the values that give it the
//! sound it had, written into the device as the file is read
//! ([`Project::from_json`](crate::Project::from_json)), into a preset that
//! was saved from one ([`SavedPreset`](crate::presets::SavedPreset)), and
//! into what a journal kept of a song as it is read
//! ([`Journal::from_text`](crate::journal::Journal::from_text)).
//!
//! The rule for whoever changes a built-in device: a new parameter either
//! defaults to what the device did before, or the device gets a line here
//! and the format's number goes up.

use crate::project::{AutomationLane, AutomationTarget, Device, FORMAT_VERSION, Project};
use crate::travel::Travel;
use std::collections::BTreeMap;

/// The parameter that keeps a built-in device sounding as it did in a song
/// of the format before it changed.
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

/// The built-in devices that sound different from format 4 on, each with
/// the values that give it the sound it had in formats 1 to 3:
///
/// * the sampler and the drum rack never played a hit louder than their
///   volume, by laws of their own (and were less sensitive, and louder,
///   where the file says nothing);
/// * the poly synth's noise was 13 dB louder, as high as its oscillators;
/// * the saturator turned its level down as its drive rose;
/// * the utility's lowest gain was 70 dB down and not silence, and its gain
///   came to where it was set through a hundredth of a second of smoothing.
pub const FORMAT_3: &[(&str, &[(&str, f64)])] = &[
    ("builtin:sampler", &[(LEGACY, 1.0), ("velocity_sens", 0.6), ("volume", -6.0)]),
    ("builtin:drumrack", &[(LEGACY, 1.0), ("velocity_sens", 1.0), ("volume", 0.0)]),
    ("builtin:polysynth", &[(LEGACY, 1.0)]),
    ("builtin:saturator", &[(LEGACY, 1.0)]),
    ("builtin:utility", &[(LEGACY, 1.0)]),
];

/// The first format in which the devices of [`FORMAT_3`] and the
/// parameters of [`MOVED_3`] are as they are now.
const CHANGED_IN: u32 = 4;

/// A parameter whose numbers mean something else from one format on: the
/// same sound is another value.
pub struct Moved {
    pub uri: &'static str,
    pub param: &'static str,
    /// What the parameter was where a file of the older format says
    /// nothing: its default of the day.
    pub unsaid: f64,
    /// What is added to a value of the older format to say the same now.
    pub by: f64,
    /// A parameter the device has had only since: a set of values that
    /// holds every parameter and lacks this one is of the older format.
    pub since: &'static str,
}

/// The parameters that moved from format 4 on:
///
/// * the poly synth's volume is 21 dB up, so that one voice at a volume
///   setting is as loud as one of the synth a mix is translated from: what
///   was 0 dB is now −21 dB.
pub const MOVED_3: &[Moved] =
    &[Moved { uri: "builtin:polysynth", param: "volume", unsaid: 0.0, by: -21.0, since: "osc_level" }];

fn give(table: &[(&str, &[(&str, f64)])], uri: &str, params: &mut BTreeMap<String, f64>) {
    let Some((_, values)) = table.iter().find(|(device, _)| *device == uri) else { return };
    for (id, value) in *values {
        params.entry((*id).to_string()).or_insert(*value);
    }
}

fn moved(uri: &str) -> impl Iterator<Item = &'static Moved> + '_ {
    MOVED_3.iter().filter(move |moved| moved.uri == uri)
}

/// Makes the parameters of a device of type `uri`, as a file of `format`
/// has them, say in this build what they meant in that one: the values the
/// file left unsaid are given, and a value whose scale has moved since is
/// moved. Nothing else the file says is touched, and a device that sounds
/// as it always did is left alone.
pub fn keep(format: u32, uri: &str, params: &mut BTreeMap<String, f64>) {
    if format < 2 {
        give(FORMAT_1, uri, params);
    }
    if format < CHANGED_IN {
        give(FORMAT_3, uri, params);
        for moved in moved(uri) {
            let value = params.get(moved.param).copied().unwrap_or(moved.unsaid);
            params.insert(moved.param.to_string(), value + moved.by);
        }
    }
}

/// What `value` of the parameter `param` of a device of type `uri`, written
/// in a file of `format` outside the device itself (a point of an
/// automation envelope), is in this build.
pub fn kept_value(format: u32, uri: &str, param: &str, value: f64) -> f64 {
    if format < CHANGED_IN
        && let Some(moved) = moved(uri).find(|moved| moved.param == param)
    {
        return value + moved.by;
    }
    value
}

/// What the travel of an envelope on `param`, as a file of `format` has it,
/// is in this build. Points that are all moved by the same amount draw the
/// same curve where the lines between them are straight in the units, as
/// every envelope of formats 1 and 2 is. Along a fader they do only if the
/// fader's floor, the level its travel is measured from, moves with them:
/// so it does.
pub fn kept_travel(format: u32, uri: &str, param: &str, travel: Travel) -> Travel {
    if format < CHANGED_IN
        && let Some(moved) = moved(uri).find(|moved| moved.param == param)
        && let Travel::Fader { floor } = travel
    {
        return Travel::Fader { floor: floor + moved.by };
    }
    travel
}

/// [`keep`] for a device as a file or a journal of `format` has it.
pub fn keep_device(format: u32, device: &mut Device) {
    keep(format, &device.uri, &mut device.params);
}

/// Makes an envelope, as a file or a journal of `format` has it, play in
/// this build the curve it played then. `uri` is the type of the device it
/// moves; an envelope on anything else is as it was.
pub fn keep_lane(format: u32, uri: &str, lane: &mut AutomationLane) {
    let AutomationTarget::DeviceParam { param, .. } = &lane.target else { return };
    for point in &mut lane.points {
        point.value = kept_value(format, uri, param, point.value);
    }
    lane.travel = kept_travel(format, uri, param, lane.travel);
}

/// Makes a whole song of an older format say in this build what it meant
/// in its own: every device of it and every envelope on one, by the
/// format the song says it is. It is then a song of this build's format.
/// (An envelope of a song older than format 3 says nothing of how its
/// lines travel and has been read as it was meant then, straight in its
/// units: there is nothing to write into it.)
pub fn keep_project(project: &mut Project) {
    let format = project.format_version;
    if format < FORMAT_VERSION {
        let mut types = BTreeMap::new();
        let devices = project.tracks.iter_mut().flat_map(|track| track.devices.iter_mut());
        for device in devices.chain(project.master.devices.iter_mut()) {
            keep_device(format, device);
            types.insert(device.id, device.uri.as_str());
        }
        for lane in &mut project.automation {
            if let AutomationTarget::DeviceParam { device, .. } = &lane.target
                && let Some(uri) = types.get(device)
            {
                keep_lane(format, uri, lane);
            }
        }
    }
    project.format_version = FORMAT_VERSION;
}

/// [`keep`] for a set of parameter values that holds every parameter the
/// device had when it was written, as a saved preset of a built-in device
/// does. Such a set says by itself how old it is: one without a `legacy`
/// value was saved before its device had any, and one without a parameter
/// that came with a move was saved before the move.
pub fn keep_complete(uri: &str, params: &mut BTreeMap<String, f64>) {
    if !params.contains_key(LEGACY) {
        give(FORMAT_1, uri, params);
        give(FORMAT_3, uri, params);
    }
    for moved in moved(uri) {
        if !params.contains_key(moved.since) {
            let value = params.get(moved.param).copied().unwrap_or(moved.unsaid);
            params.insert(moved.param.to_string(), value + moved.by);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_of_format_1_is_given_what_its_file_left_unsaid_and_nothing_else() {
        let mut reverb = BTreeMap::from([("decay".to_string(), 0.7)]);
        keep(1, "builtin:reverb", &mut reverb);
        assert_eq!(reverb.get(LEGACY), Some(&1.0));
        assert_eq!(reverb.get("high_cut"), Some(&20_000.0));
        assert_eq!(reverb.get("decay"), Some(&0.7));
        assert_eq!(reverb.len(), 3);

        // What a file says is the file's.
        let mut said = BTreeMap::from([(LEGACY.to_string(), 0.0)]);
        keep(1, "builtin:delay", &mut said);
        assert_eq!(said.get(LEGACY), Some(&0.0));

        // A file of the format they changed in, or of a later one, has them
        // as they are now.
        for format in [2, 3] {
            let mut later = BTreeMap::new();
            keep(format, "builtin:reverb", &mut later);
            assert!(later.is_empty());
        }

        // A device that did not change, and one that is not built in.
        for uri in ["builtin:eq", "builtin:compressor", "builtin:shifter", "clap:com.example.reverb", "vst3:ABCDEF"] {
            for format in [1, 2, 3] {
                let mut params = BTreeMap::new();
                keep(format, uri, &mut params);
                assert!(params.is_empty(), "{uri}");
            }
        }
    }

    #[test]
    fn a_device_of_formats_1_to_3_keeps_its_law_its_defaults_and_its_level() {
        // The sample players: the law, and the two defaults that changed
        // with it, where the file says nothing.
        for format in [1, 2, 3] {
            let mut sampler = BTreeMap::from([("volume".to_string(), -12.0)]);
            keep(format, "builtin:sampler", &mut sampler);
            assert_eq!(
                (sampler.get(LEGACY), sampler.get("velocity_sens"), sampler.get("volume")),
                (Some(&1.0), Some(&0.6), Some(&-12.0))
            );
            let mut rack = BTreeMap::new();
            keep(format, "builtin:drumrack", &mut rack);
            assert_eq!(
                (rack.get(LEGACY), rack.get("velocity_sens"), rack.get("volume")),
                (Some(&1.0), Some(&1.0), Some(&0.0))
            );
            for uri in ["builtin:saturator", "builtin:utility"] {
                let mut params = BTreeMap::from([("gain".to_string(), -70.0)]);
                keep(format, uri, &mut params);
                assert_eq!((params.get(LEGACY), params.len()), (Some(&1.0), 2), "{uri}");
            }

            // The synth's volume says the same sound with another number,
            // in the device and wherever else the file has it.
            let mut loud = BTreeMap::from([("volume".to_string(), 8.0)]);
            keep(format, "builtin:polysynth", &mut loud);
            assert_eq!(loud, BTreeMap::from([(LEGACY.to_string(), 1.0), ("volume".to_string(), -13.0)]));
            let mut unsaid = BTreeMap::new();
            keep(format, "builtin:polysynth", &mut unsaid);
            assert_eq!(unsaid, BTreeMap::from([(LEGACY.to_string(), 1.0), ("volume".to_string(), -21.0)]));
            assert_eq!(kept_value(format, "builtin:polysynth", "volume", -48.0), -69.0);
            assert_eq!(kept_value(format, "builtin:polysynth", "cutoff", 900.0), 900.0);
            assert_eq!(kept_value(format, "builtin:sampler", "volume", -6.0), -6.0);
        }

        // A file of this format is what it says.
        for uri in ["builtin:sampler", "builtin:drumrack", "builtin:saturator", "builtin:utility", "builtin:polysynth"]
        {
            let mut params = BTreeMap::new();
            keep(4, uri, &mut params);
            assert!(params.is_empty(), "{uri}");
        }
        assert_eq!(kept_value(4, "builtin:polysynth", "volume", 8.0), 8.0);
    }

    #[test]
    fn an_envelope_that_is_moved_plays_the_curve_it_played() {
        // An envelope on the synth's volume as a file of format 3 has it:
        // along a fader that was silent at the bottom of the range of the
        // day, -48 dB.
        let then = Travel::Fader { floor: -48.0 };
        let now = kept_travel(3, "builtin:polysynth", "volume", then);
        assert_eq!(now, Travel::Fader { floor: -69.0 });
        let moved = |value: f64| kept_value(3, "builtin:polysynth", "volume", value);
        for (from, to) in [(0.0, -48.0), (6.0, -30.0), (-48.0, 12.0), (-12.0, -12.0)] {
            for step in 0..=16 {
                let progress = f64::from(step) / 16.0;
                let was = then.between(from, to, progress);
                let is = now.between(moved(from), moved(to), progress);
                assert!((is - (was - 21.0)).abs() < 1e-9, "{from} to {to} at {progress}: {is} for {was}");
            }
        }
        // Left where it was, the fader's floor would bend the line: half
        // way from 0 dB to the bottom it would be 6 dB out.
        let bent = then.between(moved(0.0), moved(-48.0), 0.5) - (then.between(0.0, -48.0, 0.5) - 21.0);
        assert!(bent.abs() > 5.0, "{bent}");
        // Straight in its units, or by ratio, a line needs nothing: every
        // envelope of formats 1 and 2 is the first.
        for travel in [Travel::Linear, Travel::Log] {
            assert_eq!(kept_travel(2, "builtin:polysynth", "volume", travel), travel);
            let was = travel.between(0.0, -48.0, 0.25);
            assert!((travel.between(moved(0.0), moved(-48.0), 0.25) - (was - 21.0)).abs() < 1e-9);
        }
        // An envelope on a parameter that did not move, and one of this
        // format, are left as they are.
        assert_eq!(kept_travel(3, "builtin:sampler", "volume", then), then);
        assert_eq!(kept_travel(3, "builtin:polysynth", "cutoff", Travel::Log), Travel::Log);
        assert_eq!(kept_travel(4, "builtin:polysynth", "volume", then), then);
    }

    #[test]
    fn a_complete_set_of_values_says_by_itself_whether_it_is_old() {
        let mut old = BTreeMap::from([("feedback".to_string(), 0.5)]);
        keep_complete("builtin:delay", &mut old);
        assert_eq!(old.get(LEGACY), Some(&1.0));
        let mut new = BTreeMap::from([("feedback".to_string(), 0.5), (LEGACY.to_string(), 0.0)]);
        keep_complete("builtin:delay", &mut new);
        assert_eq!(new.get(LEGACY), Some(&0.0));

        // A sampler saved before it had the choice plays by the old law; one
        // saved since says which.
        let mut old = BTreeMap::from([("velocity_sens".to_string(), 0.8), ("volume".to_string(), -3.0)]);
        keep_complete("builtin:sampler", &mut old);
        assert_eq!(
            (old.get(LEGACY), old.get("velocity_sens"), old.get("volume")),
            (Some(&1.0), Some(&0.8), Some(&-3.0))
        );
        let mut new = BTreeMap::from([("velocity_sens".to_string(), 0.8), (LEGACY.to_string(), 0.0)]);
        keep_complete("builtin:sampler", &mut new);
        assert_eq!((new.get(LEGACY), new.len()), (Some(&0.0), 2));

        // A synth saved before its volume moved has none of the parameters
        // that came with the move; one saved since has, and is left alone.
        let mut old = BTreeMap::from([("volume".to_string(), 5.0), ("cutoff".to_string(), 900.0)]);
        keep_complete("builtin:polysynth", &mut old);
        assert_eq!((old.get("volume"), old.get(LEGACY)), (Some(&-16.0), Some(&1.0)));
        let mut new =
            BTreeMap::from([("volume".to_string(), -16.0), ("osc_level".to_string(), 1.0), (LEGACY.to_string(), 0.0)]);
        keep_complete("builtin:polysynth", &mut new);
        assert_eq!((new.get("volume"), new.get(LEGACY)), (Some(&-16.0), Some(&0.0)));
    }
}
