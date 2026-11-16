//! The instrument profiles that ship with the program: plugins whose keys
//! were measured, matched by the plugin's id.
//!
//! Every fact here was found by playing the plugin and measuring what came
//! out, over many songs written for these instruments. An entry is data: to
//! add one, measure the plugin (the probe tool plays a key and says what
//! sounded), then write what was found as the others are written. The
//! opt-in tests in `tests/known_instruments.rs` play some of these entries
//! on the installed plugins.

use trill_core::{Articulation, InstrumentProfile, Lasts, Role};

/// Ample Sound's instruments share one layout: a run of keys under the
/// range that sound nothing and choose how the notes after them are
/// played, and keys over the range that play a noise.
const AMPLE: &str = "Keyswitches sit under the range and sound nothing: press one just before its note (0.03 beats, \
                     held 0.01, never two at once). open, palm_mute, harmonic, tap and pinch_harmonic stay chosen until \
                     another is pressed; slide slides into the next note from below, once; hammer and glide join \
                     notes that overlap (hammer-ons and pull-offs, or a slide along the string) and must be pressed \
                     before the first of them. The keys above the range play noises, not notes. This depends on a \
                     setting of the plugin: with its automatic legato (auto hammer-on) switched on, notes that ring \
                     over each other are joined without the hammer key as well, which spoils a picked chord and \
                     makes a fast run over neighbouring keys sound wrong.";

/// What the instruments modelled by Audio Modeling have in common.
const SWAM: &str = "A physical model, not recordings: it is silent until it is given expression (controller 11), \
                    which is its breath or bow and its dynamic; above the middle that changes the tone more than \
                    the level. Velocity is the tongue: how fast a note speaks, not how loud it is. A note that \
                    starts before the last one ends is slurred, and then its velocity is how fast the pitch gets \
                    there. It needs a few seconds after loading before it sounds.";

const SWAM_OCTAVE: &str = " It sounds an octave under the key until its transpose setting is changed.";

/// The four Ample instruments by class id, and Superior Drummer 3.
const PF: &str = "vst3:ABCDEF019182FAEB4170736441677066";
const ECLIPSE: &str = "vst3:ABCDEF019182FAEB41707364416D6574";
const HELLRAZER: &str = "vst3:ABCDEF019182FAEB41707364416D6872";
const RAY5: &str = "vst3:ABCDEF019182FAEB41707364416D7274";
const SUPERIOR: &str = "vst3:565354534433617375706572696F7220";

/// The SWAM instruments that were measured: id, name, and whether it
/// sounds an octave under the key as it starts up.
const SWAM_INSTRUMENTS: &[(&str, &str, bool)] = &[
    ("vst3:565354575373337377616D20736F7072", "SWAM Soprano Sax 3", false),
    ("vst3:565354575361337377616D20616C746F", "SWAM Alto Sax 3", false),
    ("vst3:565354575374337377616D2074656E6F", "SWAM Tenor Sax 3", true),
    ("vst3:565354575362337377616D2062617269", "SWAM Baritone Sax 3", true),
    ("vst3:565354574363337377616D20636C6172", "SWAM Clarinet 3", false),
    ("vst3:565354425462337377616D207472756D", "SWAM Trumpet", false),
    ("vst3:565354425466337377616D20666C7567", "SWAM Flugelhorn", false),
    ("vst3:565354424274337377616D2074656E6F", "SWAM Tenor Trombone", true),
    ("vst3:565354424262337377616D2062617373", "SWAM Bass Trombone", true),
    ("vst3:565354536462337377616D20646F7562", "SWAM Double Bass 3", true),
];

/// The device types an entry ships for.
pub(super) fn uris() -> impl Iterator<Item = &'static str> {
    [PF, ECLIPSE, HELLRAZER, RAY5, SUPERIOR].into_iter().chain(SWAM_INSTRUMENTS.iter().map(|(uri, ..)| *uri))
}

/// What ships about the device type `uri`: its name and its profile.
pub(super) fn entry(uri: &str) -> Option<(&'static str, InstrumentProfile)> {
    let (name, profile) = match uri {
        PF => ("Ample Guitar PF", ample_guitar_pf()),
        ECLIPSE => ("Ample Metal Eclipse", ample_metal_eclipse()),
        HELLRAZER => ("Ample Metal Hellrazer", ample_metal_hellrazer()),
        RAY5 => ("Ample Metal Ray5", ample_metal_ray5()),
        SUPERIOR => ("Superior Drummer 3", superior_drummer_3()),
        _ => {
            let (_, name, octave_down) = SWAM_INSTRUMENTS.iter().find(|(id, ..)| *id == uri)?;
            (*name, swam(*octave_down))
        }
    };
    // Checked by the tests below, for every entry.
    Some((name, profile.validated().expect("the shipped profiles are valid")))
}

fn sounds(keys: &[(&str, u8)]) -> std::collections::BTreeMap<String, u8> {
    keys.iter().map(|(name, key)| ((*name).to_string(), *key)).collect()
}

/// The keyswitches every Ample instrument has, from the key `open` is on:
/// they lie in the same order on each. Those that only some have follow.
fn ample_switches(open: u8, open_hit: (u8, u8)) -> Vec<Articulation> {
    vec![
        Articulation::key("open", open).hit(open_hit.0, open_hit.1).as_default(),
        Articulation::key("harmonic", open + 1),
        Articulation::key("palm_mute", open + 2),
        Articulation::key("slide", open + 3).lasting(Lasts::Once),
        Articulation::key("glide", open + 4).lasting(Lasts::Joins),
        Articulation::key("hammer", open + 5).lasting(Lasts::Joins),
    ]
}

/// The noises above the range of the three guitars.
fn guitar_noises() -> std::collections::BTreeMap<String, u8> {
    sounds(&[("scratch", 89), ("slap", 90), ("muting", 91), ("chk", 92), ("scrape", 101)])
}

/// A PRS Custom 24 with humbuckers. Every velocity up to 126 plays an open
/// note.
fn ample_guitar_pf() -> InstrumentProfile {
    let mut articulations = ample_switches(24, (1, 126));
    articulations.extend([Articulation::key("tap", 22), Articulation::key("pinch_harmonic", 23)]);
    InstrumentProfile {
        sounds: guitar_noises(),
        articulations,
        range: Some((40, 88)),
        notes: format!("An electric guitar (a PRS Custom 24). {AMPLE}"),
        ..InstrumentProfile::default()
    }
}

/// An ESP Eclipse tuned down to C. How hard a note is hit also chooses how
/// it is played: under 64 the string is muted.
fn ample_metal_eclipse() -> InstrumentProfile {
    let open = 24;
    let mut articulations = ample_switches(open, (64, 126));
    articulations.extend([
        Articulation::key("tap", 30),
        Articulation::key("pinch_harmonic", 31),
        Articulation::key("muted", open).hit(32, 63),
        Articulation::key("choked", open).hit(16, 31),
        Articulation::key("dead", open).hit(1, 15),
    ]);
    InstrumentProfile {
        sounds: guitar_noises(),
        articulations,
        range: Some((36, 84)),
        notes: format!(
            "A metal guitar tuned down to C (an ESP Eclipse). With open chosen, how hard a note is hit also chooses \
             how it is played: 64 and up rings, under 64 the string is muted, under 32 choked, under 16 dead. {AMPLE}"
        ),
        ..InstrumentProfile::default()
    }
}

/// A nine-string Schecter Hellraiser: a guitar that reaches down into the
/// bass's notes, with its switches an octave lower than the others'.
fn ample_metal_hellrazer() -> InstrumentProfile {
    let open = 12;
    let mut articulations = ample_switches(open, (64, 126));
    articulations.extend([
        Articulation::key("tap", 18),
        Articulation::key("pinch_harmonic", 19),
        Articulation::key("muted", open).hit(32, 63),
        Articulation::key("dead", open).hit(1, 31),
    ]);
    InstrumentProfile {
        sounds: guitar_noises(),
        articulations,
        range: Some((25, 88)),
        notes: format!(
            "A nine-string metal guitar, reaching down into a bass's notes. With open chosen, how hard a note is hit \
             also chooses how it is played: 64 and up rings, under 64 the string is muted, under 32 dead. {AMPLE}"
        ),
        ..InstrumentProfile::default()
    }
}

/// A five-string Music Man StingRay bass, played with fingers or a pick.
fn ample_metal_ray5() -> InstrumentProfile {
    let open = 24;
    let mut articulations = ample_switches(open, (64, 126));
    articulations.extend([
        Articulation::key("muted", open).hit(32, 63),
        Articulation::key("dead", open).hit(1, 31),
        Articulation::key("accent", open).hit(127, 127),
    ]);
    InstrumentProfile {
        sounds: sounds(&[("slap", 83), ("pop", 84), ("scratch", 89), ("muting", 91)]),
        articulations,
        range: Some((33, 76)),
        sounds_octaves: -1,
        notes: format!(
            "A five-string bass guitar that sounds an octave below its keys. With open chosen, how hard a note is \
             hit also chooses how it is played: 64 and up rings, under 64 the string is muted, under 32 dead, and \
             127 is an accent. {AMPLE}"
        ),
        ..InstrumentProfile::default()
    }
}

/// Superior Drummer 3's core library: the keys that mean the same in every
/// kit of it. General MIDI holds in the middle of the keyboard, so only
/// what differs from it is listed.
fn superior_drummer_3() -> InstrumentProfile {
    InstrumentProfile {
        role: Some(Role::Drums),
        sounds: sounds(&[
            ("rimshot", 40),
            ("closed_hat_edge", 22),
            ("wide_open_hat", 26),
            ("high_tom", 48),
            ("cymbal_1", 86),
        ]),
        notes: "The core library's kits. Snare: snare 38, rimshot 40, rim (sidestick) 37. Hi-hat: closed_hat 42 is \
                the tip and closed_hat_edge 22 the edge, pedal_hat 44, open_hat 46, wide_open_hat 26. Toms: \
                high_tom 48, mid_tom 47, low_tom 43 and floor_tom 41. ride 51 and ride_bell 53. The five cymbal \
                positions are cymbal_1 86, crash 49, splash 55, crash2 57 and china 52; which cymbal hangs in each \
                depends on the kit, so those names are General MIDI's and not a promise. A kit takes most of a \
                minute to load its samples."
            .to_string(),
        ..InstrumentProfile::default()
    }
}

fn swam(octave_down: bool) -> InstrumentProfile {
    InstrumentProfile {
        sounds_octaves: if octave_down { -1 } else { 0 },
        slurs: true,
        notes: if octave_down { format!("{SWAM}{SWAM_OCTAVE}") } else { SWAM.to_string() },
        ..InstrumentProfile::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_entry_is_a_valid_profile_under_a_plugin_id() {
        let all: Vec<&str> = uris().collect();
        assert_eq!(all.len(), 15);
        for uri in &all {
            let (name, profile) = entry(uri).unwrap_or_else(|| panic!("{uri} has no entry"));
            assert!(!name.is_empty() && !profile.is_empty() && !profile.notes.is_empty(), "{uri}");
            // A VST3 class id: thirty-two hexadecimal digits.
            let id = uri.strip_prefix("vst3:").unwrap_or_else(|| panic!("{uri} is not a VST3 id"));
            assert!(id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()), "{uri}");
            assert_eq!(all.iter().filter(|other| *other == uri).count(), 1, "{uri} is listed twice");
        }
        assert_eq!(entry("vst3:00000000000000000000000000000000"), None);
        assert_eq!(entry("builtin:polysynth"), None);
    }

    #[test]
    fn the_guitars_keep_their_switches_under_the_range_and_their_noises_over_it() {
        for uri in [PF, ECLIPSE, HELLRAZER, RAY5] {
            let (name, profile) = entry(uri).unwrap();
            let (low, high) = profile.range.unwrap();
            for articulation in &profile.articulations {
                assert!(articulation.key.unwrap() < low, "{name}: {} is not under the range", articulation.name);
            }
            assert!(profile.sounds.values().all(|key| *key > high), "{name}: a noise is inside the range");
            // The way of playing a note that nothing was said about, and the three ornaments.
            assert_eq!(profile.default_articulation().unwrap().name, "open", "{name}");
            assert_eq!(profile.articulation("slide").unwrap().lasts, Lasts::Once, "{name}");
            for joined in ["hammer", "glide"] {
                assert_eq!(profile.articulation(joined).unwrap().lasts, Lasts::Joins, "{name}");
            }
            assert_eq!(profile.role, None, "{name}: a guitar is pitched without being told");
            // Only the bass sounds an octave from its keys.
            assert_eq!(profile.sounds_octaves, if uri == RAY5 { -1 } else { 0 }, "{name}");
            assert!(profile.notes.contains("automatic legato"), "{name}: the setting its switches depend on");
        }
        let key = |uri: &str, name: &str| entry(uri).unwrap().1.articulation(name).and_then(|a| a.key);
        assert_eq!((key(PF, "palm_mute"), key(PF, "tap"), key(PF, "hammer")), (Some(26), Some(22), Some(29)));
        assert_eq!((key(ECLIPSE, "palm_mute"), key(ECLIPSE, "tap")), (Some(26), Some(30)));
        assert_eq!(
            (key(HELLRAZER, "open"), key(HELLRAZER, "palm_mute"), key(HELLRAZER, "pinch_harmonic")),
            (Some(12), Some(14), Some(19))
        );
        assert_eq!(key(RAY5, "tap"), None, "the bass has no tapping");
        // Soft notes are muted on the metal instruments, not on the PRS.
        let hit = |uri: &str, name: &str| entry(uri).unwrap().1.articulation(name).and_then(|a| a.velocity);
        assert_eq!((hit(PF, "open"), hit(PF, "muted")), (Some((1, 126)), None));
        for uri in [ECLIPSE, HELLRAZER, RAY5] {
            assert_eq!((hit(uri, "open"), hit(uri, "muted")), (Some((64, 126)), Some((32, 63))));
        }
        assert_eq!(hit(RAY5, "accent"), Some((127, 127)));
    }

    #[test]
    fn the_drum_kit_names_its_own_keys_and_general_midi_fills_the_rest() {
        let (_, kit) = entry(SUPERIOR).unwrap();
        assert_eq!(kit.role, Some(Role::Drums));
        let key = |name: &str| kit.drum(name).map(|drum| drum.pitch);
        assert_eq!(
            (key("kick"), key("snare"), key("rimshot"), key("sidestick")),
            (Some(36), Some(38), Some(40), Some(37))
        );
        assert_eq!((key("closed_hat"), key("closed_hat_edge"), key("pedal_hat")), (Some(42), Some(22), Some(44)));
        assert_eq!((key("open_hat"), key("wide_open_hat"), key("half_open_hat")), (Some(46), Some(26), Some(46)));
        assert_eq!(
            (key("high_tom"), key("mid_tom"), key("low_tom"), key("floor_tom")),
            (Some(48), Some(47), Some(43), Some(41))
        );
        assert_eq!(
            (key("ride"), key("ride_bell"), key("china"), key("splash")),
            (Some(51), Some(53), Some(52), Some(55))
        );
        assert_eq!((key("crash"), key("crash2"), key("cymbal_1")), (Some(49), Some(57), Some(86)));
        assert_eq!(kit.drum_name(40), "rimshot");
        assert_eq!(kit.drum_name(22), "closed_hat_edge");
    }

    #[test]
    fn the_modelled_winds_slur_and_some_sound_an_octave_down() {
        let down: Vec<&str> =
            SWAM_INSTRUMENTS.iter().filter(|(_, _, octave)| *octave).map(|(_, name, _)| *name).collect();
        assert_eq!(
            down,
            [
                "SWAM Tenor Sax 3",
                "SWAM Baritone Sax 3",
                "SWAM Tenor Trombone",
                "SWAM Bass Trombone",
                "SWAM Double Bass 3"
            ]
        );
        for (uri, name, octave_down) in SWAM_INSTRUMENTS {
            let (listed, profile) = entry(uri).unwrap();
            assert_eq!(listed, *name);
            assert!(profile.slurs && profile.notes.contains("expression"), "{name}");
            assert_eq!(profile.sounds_octaves, if *octave_down { -1 } else { 0 }, "{name}");
            assert!(profile.articulations.is_empty() && profile.range.is_none(), "{name}");
        }
    }
}
