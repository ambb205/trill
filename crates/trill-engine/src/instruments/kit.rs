//! The drums of the kit that is loaded, by name.
//!
//! A drum plugin's kit decides what its keys play, and kits of one plugin
//! do not agree: one hangs a china where another has a crash, a third
//! leaves a tom out. Where a plugin says what is on its keys
//! ([`KeySound`]: the place, what is in it, the stroke), the names a song
//! is written by are worked out from that here, so that "china" is the
//! key this kit's china is on, and a kit without one has no "china".
//!
//! The names are the ones the writing tools take everywhere: `kick`,
//! `snare`, `rimshot`, `closed_hat`, `open_hat`, `high_tom`, `floor_tom`,
//! `crash`, `china`, `splash`, `ride`, `ride_bell`. A cymbal is named for
//! what it is (the maker's words for it say "China", "Splash", "Crash")
//! and also for where it hangs (`cymbal_1`); a second of a kind is
//! `crash2`. Strokes beyond the usual keep the drum's name with the
//! stroke's after it: `snare_rim_only`, `crash_choke`, `ride_bow_shank`.

use std::collections::BTreeMap;
use trill_core::pitch::parse_drum;
use trill_core::{InstrumentProfile, KeySound};

/// A kit's drums by name, and what the kit is in words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KitMap {
    /// Every name the kit's keys go by.
    pub names: BTreeMap<String, u8>,
    /// What hangs where, for whoever writes for it: one line.
    pub told: String,
}

/// What kind of thing is in a place of a kit.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Kick,
    Snare,
    Hats,
    RackTom,
    FloorTom,
    Tom,
    Ride,
    Cymbal,
    Other,
}

fn kind_of(place: &str) -> Kind {
    let place = place.to_ascii_lowercase();
    let has = |word: &str| place.contains(word);
    if has("kick") || has("bass drum") {
        Kind::Kick
    } else if has("snare") {
        Kind::Snare
    } else if has("hat") {
        Kind::Hats
    } else if has("rack") && has("tom") {
        Kind::RackTom
    } else if has("floor") {
        Kind::FloorTom
    } else if has("tom") {
        Kind::Tom
    } else if has("ride") {
        Kind::Ride
    } else if has("cymbal") || has("crash") || has("china") || has("splash") {
        Kind::Cymbal
    } else {
        Kind::Other
    }
}

/// Words as a name: lower case, with whatever is not a letter or a digit
/// as one underscore.
fn slug(words: &str) -> String {
    let mut name = String::new();
    for c in words.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c.to_ascii_lowercase());
        } else if !name.ends_with('_') && !name.is_empty() {
            name.push('_');
        }
    }
    name.trim_matches('_').to_string()
}

/// The number at the end of a place's name: 2 for "Racktom 2".
fn number_of(place: &str) -> Option<u32> {
    let digits: String = place.chars().rev().take_while(char::is_ascii_digit).collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// What a cymbal is, by its maker's words for it.
fn cymbal_type(instrument: &str) -> Option<&'static str> {
    let said = instrument.to_ascii_lowercase();
    ["china", "splash", "stack", "crash", "ride"].into_iter().find(|word| said.contains(word))
}

/// A place of the kit with the keys that strike it.
struct Place<'a> {
    name: &'a str,
    instrument: &'a str,
    kind: Kind,
    /// Each stroke with its keys, in the order the kit lists them.
    strokes: Vec<(&'a str, Vec<u8>)>,
}

impl Place<'_> {
    /// The keys of the stroke that goes by one of `words`.
    fn stroke(&self, words: &[&str]) -> Option<&[u8]> {
        words.iter().find_map(|word| {
            self.strokes.iter().find(|(stroke, _)| stroke.eq_ignore_ascii_case(word)).map(|(_, keys)| keys.as_slice())
        })
    }
}

/// The drums of a kit by name: see the module's words. `sounds` is what
/// the kit's instrument said of its keys.
pub fn drum_map(sounds: &[KeySound]) -> KitMap {
    // The kit place by place, in the order it lists them.
    let mut places: Vec<Place> = Vec::new();
    for sound in sounds {
        let at = match places.iter().position(|place| place.name == sound.place) {
            Some(at) => at,
            None => {
                places.push(Place {
                    name: &sound.place,
                    instrument: &sound.instrument,
                    kind: kind_of(&sound.place),
                    strokes: Vec::new(),
                });
                places.len() - 1
            }
        };
        let place = &mut places[at];
        match place.strokes.iter_mut().find(|(stroke, _)| *stroke == sound.stroke) {
            Some((_, keys)) => keys.push(sound.key),
            None => place.strokes.push((&sound.stroke, vec![sound.key])),
        }
    }

    let mut map = KitMap::default();
    let mut told: Vec<String> = Vec::new();
    // A name is given once: the first drum that takes it keeps it. Of the
    // keys that play one stroke, the one General MIDI has that drum on is
    // the one to write, where it is among them.
    let mut name = |name: String, keys: &[u8]| {
        if name.is_empty() || !name.chars().any(|c| c.is_ascii_alphabetic()) || name.len() > 40 {
            return;
        }
        let Some(first) = keys.first() else { return };
        let general = parse_drum(&name).filter(|key| keys.contains(key));
        let usual = keys.iter().copied().find(|key| (35..=59).contains(key));
        map.names.entry(name).or_insert(general.or(usual).unwrap_or(*first));
    };
    let mut say = |name: &str, place: &Place| {
        let what = if place.instrument.is_empty() { place.name } else { place.instrument };
        told.push(format!("{name}: {what}"));
    };

    // The strokes that are no way of playing a drum: what a pad or a pedal sends.
    let played = |stroke: &str| {
        let stroke = stroke.to_ascii_lowercase();
        !(stroke.contains("trigger")
            || stroke.contains("control")
            || stroke.contains("trig")
            || stroke.contains("ctrl"))
    };
    // Whatever strokes a drum has beyond the ones named for it, under its own name.
    let rest = |base: &str, place: &Place, taken: &[&str], name: &mut dyn FnMut(String, &[u8])| {
        for (stroke, keys) in &place.strokes {
            if played(stroke) && !taken.iter().any(|word| stroke.eq_ignore_ascii_case(word)) {
                name(format!("{base}_{}", slug(stroke)), keys);
            }
        }
    };

    for place in places.iter().filter(|place| place.kind == Kind::Kick) {
        let (open, hit) = (place.stroke(&["Open"]), place.stroke(&["Hit"]));
        if let Some(keys) = open.or(hit) {
            name("kick".into(), keys);
            say("kick", place);
        }
        if let (Some(_), Some(keys)) = (open, hit) {
            name("kick2".into(), keys);
        }
        rest("kick", place, &["Open", "Hit"], &mut name);
    }
    for place in places.iter().filter(|place| place.kind == Kind::Snare) {
        for (stroke, drum) in [
            ("Center", "snare"),
            ("Rimshot", "rimshot"),
            // General MIDI's name for the stick laid across the head.
            ("Sidestick", "rim"),
            ("Flam", "flam"),
            ("Closed Roll", "snare_roll"),
        ] {
            if let Some(keys) = place.stroke(&[stroke]) {
                name(drum.into(), keys);
            }
        }
        if place.stroke(&["Center"]).is_some() {
            say("snare", place);
        }
        rest("snare", place, &["Center", "Rimshot", "Sidestick", "Flam", "Closed Roll"], &mut name);
    }
    for place in places.iter().filter(|place| place.kind == Kind::Hats) {
        let usual = [
            ("Closed Tip", "closed_hat"),
            ("Closed Edge", "closed_hat_edge"),
            ("Tight Tip", "tight_hat"),
            ("Tight Edge", "tight_hat_edge"),
            ("Closed Pedal", "pedal_hat"),
            ("Open Pedal", "pedal_hat_open"),
            ("Open Edge 1", "half_open_hat"),
            ("Open Edge 2", "open_hat"),
            ("Open Edge 3", "wide_open_hat"),
            ("Closed Bell", "hat_bell"),
        ];
        for (stroke, drum) in usual {
            if let Some(keys) = place.stroke(&[stroke]) {
                name(drum.into(), keys);
            }
        }
        // A kit with fewer degrees of open has an open hat all the same.
        if let Some(keys) =
            place.stroke(&["Open Edge 2", "Open Edge 3", "Open Edge 1", "Open Edge 4", "Open", "Open Edge"])
        {
            name("open_hat".into(), keys);
        }
        if let Some(keys) = place.stroke(&["Closed Tip", "Closed Edge", "Closed"]) {
            name("closed_hat".into(), keys);
            say("hi-hat", place);
        }
        let taken: Vec<&str> = usual.iter().map(|(stroke, _)| *stroke).collect();
        rest("hat", place, &taken, &mut name);
    }

    // The toms from the highest down: the rack toms by their numbers, then the floor toms.
    let mut toms: Vec<&Place> = Vec::new();
    for kind in [Kind::RackTom, Kind::Tom, Kind::FloorTom] {
        let mut of_kind: Vec<&Place> = places.iter().filter(|place| place.kind == kind).collect();
        of_kind.sort_by_key(|place| number_of(place.name).unwrap_or(u32::MAX));
        toms.extend(of_kind);
    }
    fn centre<'a>(place: &'a Place) -> Option<&'a [u8]> {
        place.stroke(&["Center"]).or(place.strokes.first().map(|(_, keys)| keys.as_slice()))
    }
    let count = toms.len();
    if count > 0 {
        let at = |index: usize| toms[index.min(count - 1)];
        // Four names over however many toms there are: with four, each has its own.
        let general = [
            ("high_tom", at(0)),
            ("mid_tom", at(if count >= 3 { 1 } else { 0 })),
            ("low_tom", at(if count >= 3 { count - 2 } else { count - 1 })),
            ("floor_tom", at(count - 1)),
        ];
        for (drum, place) in general {
            if let Some(keys) = centre(place) {
                name(drum.into(), keys);
            }
        }
    }
    for place in &toms {
        let base = match (place.kind, number_of(place.name)) {
            (Kind::RackTom, Some(number)) => format!("rack_tom_{number}"),
            (Kind::FloorTom, Some(number)) => format!("floor_tom_{number}"),
            _ => slug(place.name),
        };
        if let Some(keys) = centre(place) {
            name(base.clone(), keys);
            say(&base, place);
        }
        rest(&base, place, &["Center"], &mut name);
    }

    // The cymbals: each by what it is, the second of a kind with a 2, and by where it hangs.
    let mut seen: BTreeMap<&'static str, usize> = BTreeMap::new();
    for place in places.iter().filter(|place| matches!(place.kind, Kind::Cymbal | Kind::Ride)) {
        let ride = place.kind == Kind::Ride;
        let kind = if ride { Some("ride") } else { cymbal_type(place.instrument) };
        let hangs = number_of(place.name).filter(|_| !ride).map(|number| format!("cymbal_{number}"));
        let base = kind.map(|kind| {
            let count = seen.entry(kind).or_default();
            *count += 1;
            if *count == 1 { kind.to_string() } else { format!("{kind}{count}") }
        });
        // What it is struck as when nothing more is said: a ride on its bow, another on its edge.
        let main: &[&str] =
            if kind == Some("ride") { &["Bow Tip", "Edge", "Crash"] } else { &["Crash", "Edge", "Bow Tip"] };
        let strokes: &[(&str, &str)] = if kind == Some("ride") {
            &[
                ("Bell Shank", "bell"),
                ("Edge", "edge"),
                ("Crash", "edge"),
                ("Mute Hit", "choke"),
                ("Bow Shank", "bow_shank"),
            ]
        } else {
            &[("Mute Hit", "choke"), ("Bow Tip", "bow"), ("Bell Shank", "bell")]
        };
        let mut taken: Vec<&str> = Vec::new();
        for base in base.iter().chain(&hangs) {
            if let Some(keys) = place.stroke(main) {
                name(base.clone(), keys);
            }
            for (stroke, suffix) in strokes {
                if let Some(keys) = place.stroke(&[stroke]) {
                    name(format!("{base}_{suffix}"), keys);
                }
            }
        }
        if let Some(first) = base.as_ref().or(hangs.as_ref()) {
            let stroke = main.iter().find(|stroke| place.stroke(&[stroke]).is_some());
            taken.extend(stroke.iter().copied());
            taken.extend(strokes.iter().map(|(stroke, _)| *stroke).filter(|stroke| place.stroke(&[stroke]).is_some()));
            rest(first, place, &taken, &mut name);
            match (&base, &hangs) {
                (Some(base), Some(hangs)) => say(&format!("{base} ({hangs})"), place),
                _ => say(first, place),
            }
        }
    }

    // Whatever else the kit has: a pad, a shaker, a clap, under its place's name.
    for place in places.iter().filter(|place| place.kind == Kind::Other) {
        let base = slug(place.name);
        let mut first = true;
        for (stroke, keys) in place.strokes.iter().filter(|(stroke, _)| played(stroke)) {
            // A place with one sound is called by its own name alone.
            let same = slug(stroke) == base || place.strokes.len() == 1;
            name(if same { base.clone() } else { format!("{base}_{}", slug(stroke)) }, keys);
            if first {
                say(&base, place);
                first = false;
            }
        }
    }

    map.told = told.join("; ");
    map
}

impl KitMap {
    /// What `profile`, a track's own, comes to with this kit under it: the
    /// kit's names, the track's own before them where it has any, and the
    /// word that these are the whole of what the kit plays.
    /// `shipped` is what ships for the instrument, whose words about keys
    /// are about no kit in particular and give way to this kit's.
    pub fn under(&self, mut profile: InstrumentProfile, shipped: Option<&InstrumentProfile>) -> InstrumentProfile {
        let own = std::mem::take(&mut profile.sounds);
        // What ships for the instrument names keys as most kits have them:
        // this kit's own word is better than that.
        let shipped_names = shipped.map(|shipped| &shipped.sounds);
        profile.sounds = self.names.clone();
        for (name, key) in own {
            if shipped_names.is_none_or(|shipped| shipped.get(&name) != Some(&key)) {
                profile.sounds.insert(name, key);
            }
        }
        profile.whole = true;
        let general = shipped.map(|shipped| shipped.notes.as_str()).unwrap_or_default();
        let own_notes = if profile.notes == general { "" } else { profile.notes.as_str() };
        let mut notes = format!(
            "The kit that is loaded, as the plugin's own state has it ({} names; every key that plays something \
             has one, and a drum that is not named here is not in this kit): {}.",
            self.names.len(),
            self.told
        );
        if !own_notes.is_empty() {
            notes = format!("{own_notes} {notes}");
        }
        // No longer than a profile's notes may be.
        profile.notes = notes.chars().take(trill_core::instrument::MAX_NOTES_TEXT).collect();
        profile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound(key: u8, place: &str, instrument: &str, stroke: &str) -> KeySound {
        KeySound { key, place: place.into(), instrument: instrument.into(), stroke: stroke.into() }
    }

    /// A kit as a rock preset has it: two rack toms and two floor toms,
    /// the third rack tom's place empty, a crash where the first cymbal
    /// hangs and a china in the last.
    fn rock() -> Vec<KeySound> {
        let mut kit = vec![
            sound(36, "Kick", "16x20\" Maple", "Open"),
            sound(34, "Kick", "16x20\" Maple", "Open"),
            sound(35, "Kick", "16x20\" Maple", "Hit"),
            sound(6, "Snare", "6.5x14\" Brass", "Zone Trigger"),
            sound(38, "Snare", "6.5x14\" Brass", "Center"),
            sound(40, "Snare", "6.5x14\" Brass", "Rimshot"),
            sound(37, "Snare", "6.5x14\" Brass", "Sidestick"),
            sound(76, "Snare", "6.5x14\" Brass", "Sidestick"),
            sound(71, "Snare", "6.5x14\" Brass", "Rim Only"),
            sound(69, "Snare", "6.5x14\" Brass", "Flam"),
            sound(22, "Hi-Hat", "15\" Heavy", "Closed Edge"),
            sound(61, "Hi-Hat", "15\" Heavy", "Closed Tip"),
            sound(11, "Hi-Hat", "15\" Heavy", "Closed Tip"),
            sound(42, "Hi-Hat", "15\" Heavy", "Closed Tip"),
            sound(21, "Hi-Hat", "15\" Heavy", "Closed Pedal"),
            sound(10, "Hi-Hat", "15\" Heavy", "Closed Pedal"),
            sound(44, "Hi-Hat", "15\" Heavy", "Closed Pedal"),
            sound(24, "Hi-Hat", "15\" Heavy", "Open Edge 1"),
            sound(25, "Hi-Hat", "15\" Heavy", "Open Edge 2"),
            sound(46, "Hi-Hat", "15\" Heavy", "Open Edge 2"),
            sound(26, "Hi-Hat", "15\" Heavy", "Open Edge 3"),
            sound(13, "Hi-Hat", "15\" Heavy", "Open Tip 1"),
            sound(48, "Racktom 1", "10x12\" Birch", "Center"),
            sound(82, "Racktom 1", "10x12\" Birch", "Rimshot"),
            sound(47, "Racktom 2", "11x13\" Birch", "Center"),
            sound(43, "Floortom 1", "16x16\" Birch", "Center"),
            sound(41, "Floortom 2", "16x18\" Birch", "Center"),
        ];
        let cymbals = [
            ("Cymbal 1", "17\" Medium Crash", [86, 83, 84, 85]),
            ("Cymbal 2", "16\" Thin Crash", [49, 50, 27, 28]),
            ("Cymbal 3", "10\" Splash", [55, 56, 96, 97]),
            ("Cymbal 4", "18\" Medium Crash", [57, 58, 31, 32]),
            ("Cymbal 5", "20\" Sehhar China", [52, 54, 108, 109]),
        ];
        for (place, what, keys) in cymbals {
            for (key, stroke) in keys.into_iter().zip(["Crash", "Mute Hit", "Bow Tip", "Bell Shank"]) {
                kit.push(sound(key, place, what, stroke));
            }
        }
        for (key, stroke) in [(59, "Edge"), (51, "Bow Tip"), (53, "Bell Shank"), (29, "Bow Shank"), (118, "Mute Hit")] {
            kit.push(sound(key, "Ride", "20\" Turk Ride", stroke));
        }
        kit
    }

    #[test]
    fn a_kits_drums_are_named_for_what_they_are_and_land_on_its_own_keys() {
        let kit = drum_map(&rock());
        let key = |name: &str| kit.names.get(name).copied();
        // The drums and the hat, on the keys General MIDI has them on where the kit has them there.
        assert_eq!(
            (key("kick"), key("kick2"), key("snare"), key("rimshot"), key("rim")),
            (Some(36), Some(35), Some(38), Some(40), Some(37))
        );
        assert_eq!((key("closed_hat"), key("closed_hat_edge"), key("pedal_hat")), (Some(42), Some(22), Some(44)));
        assert_eq!((key("half_open_hat"), key("open_hat"), key("wide_open_hat")), (Some(24), Some(46), Some(26)));
        assert_eq!((key("flam"), key("snare_rim_only"), key("hat_open_tip_1")), (Some(69), Some(71), Some(13)));
        // Four toms, four names, from the highest down; each also by its place.
        assert_eq!(
            (key("high_tom"), key("mid_tom"), key("low_tom"), key("floor_tom")),
            (Some(48), Some(47), Some(43), Some(41))
        );
        assert_eq!((key("rack_tom_1"), key("floor_tom_2"), key("rack_tom_1_rimshot")), (Some(48), Some(41), Some(82)));
        assert_eq!(key("rack_tom_3"), None, "a place the kit leaves empty has no name");
        // The cymbals by what the maker calls them: the china is where this kit hangs it.
        assert_eq!((key("crash"), key("crash2"), key("crash3")), (Some(86), Some(49), Some(57)));
        assert_eq!(
            (key("splash"), key("china"), key("cymbal_5"), key("cymbal_1")),
            (Some(55), Some(52), Some(52), Some(86))
        );
        assert_eq!(
            (key("crash_choke"), key("china_choke"), key("china_bell"), key("cymbal_3_bow")),
            (Some(83), Some(54), Some(109), Some(96))
        );
        // A ride is struck on its bow, and its other strokes are its own.
        assert_eq!(
            (key("ride"), key("ride_bell"), key("ride_edge"), key("ride_choke")),
            (Some(51), Some(53), Some(59), Some(118))
        );
        assert_eq!(key("snare_zone_trigger"), None, "what a pad sends is no drum");
        assert!(
            kit.told.contains("china (cymbal_5): 20\" Sehhar China") && kit.told.contains("kick: 16x20\" Maple"),
            "{}",
            kit.told
        );
        assert!(
            kit.names
                .keys()
                .all(|name| name.len() <= 40 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        );
    }

    #[test]
    fn another_kit_of_the_same_plugin_has_its_china_elsewhere_and_fewer_drums() {
        // The china in the first cymbal's place, one tom, no hi-hat pedal, a kick on other keys.
        let kit = vec![
            sound(34, "Kick", "", "Open"),
            sound(36, "Kick", "", "Hit"),
            sound(35, "Kick", "", "Hit"),
            sound(38, "Snare", "", "Center"),
            sound(61, "Hi-Hat", "", "Closed Tip"),
            sound(60, "Hi-Hat", "", "Open Edge 4"),
            sound(45, "Racktom 2", "", "Center"),
            sound(52, "Cymbal 1", "20\" Traditional China", "Crash"),
            sound(86, "Cymbal 1", "20\" Traditional China", "Crash"),
            sound(110, "Cymbal 5", "", "Crash"),
            sound(3, "Tambourine Pad", "", "Hit"),
            sound(4, "Shaker Pad", "", "Forward"),
            sound(5, "Shaker Pad", "", "Back"),
        ];
        let kit = drum_map(&kit);
        let key = |name: &str| kit.names.get(name).copied();
        assert_eq!(
            (key("kick"), key("kick2")),
            (Some(34), Some(35)),
            "the key it is on here; its other stroke where General MIDI has a second kick"
        );
        assert_eq!((key("closed_hat"), key("open_hat"), key("pedal_hat")), (Some(61), Some(60), None));
        assert_eq!(
            (key("high_tom"), key("floor_tom"), key("rack_tom_2")),
            (Some(45), Some(45), Some(45)),
            "one tom is every tom"
        );
        assert_eq!((key("china"), key("cymbal_1"), key("crash")), (Some(52), Some(52), None), "this kit has no crash");
        assert_eq!(key("cymbal_5"), Some(110), "a cymbal nothing is said of is known by where it hangs");
        assert_eq!(
            (key("tambourine_pad"), key("shaker_pad_forward"), key("shaker_pad_back")),
            (Some(3), Some(4), Some(5))
        );

        // Under a track's profile: the kit's names, and the word that there are no others.
        let shipped = InstrumentProfile {
            sounds: [("high_tom".to_string(), 48), ("cymbal_1".to_string(), 86)].into(),
            notes: "What ships.".into(),
            ..InstrumentProfile::default()
        };
        let mut own = shipped.clone();
        own.sounds.insert("gong".into(), 110);
        let profile = kit.under(own, Some(&shipped));
        assert!(profile.whole);
        assert_eq!(
            (profile.sounds["high_tom"], profile.sounds["cymbal_1"]),
            (45, 52),
            "the kit's word, not what ships"
        );
        assert_eq!(profile.sounds["gong"], 110, "a name the track was given by hand stays");
        assert!(
            profile.notes.starts_with("The kit that is loaded") && !profile.notes.contains("What ships"),
            "{}",
            profile.notes
        );
        assert_eq!(profile.find_drum("crash"), trill_core::Drum::Lacking("crash".into()));
        // Nothing said of its keys: nothing named.
        assert_eq!(drum_map(&[]), KitMap::default());
    }
}
