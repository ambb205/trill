//! What is on each key of an instrument whose keys are drums, in the
//! words the rest of the program names drums by.
//!
//! A drum plugin says of its kit which drum hangs in each place and which
//! keys strike it how ([`KeySound`]), and a track's drum names are worked
//! out from that. An instrument's file says less: at best a label for a
//! key, usually only the name of the recording (`Kick_01`, `sn-rim`,
//! `HH open 3`). This reads a drum and a stroke out of such words and says
//! them as a kit does: a place (`Kick`, `Snare`, `Hats`, `Racktom 2`,
//! `Cymbal 1`), what is in it for a cymbal (`Crash`, `China`), and the
//! stroke (`Center`, `Rimshot`, `Open Edge 2`).
//!
//! Words that say nothing at all (a number, `sample 7`) fall back on what
//! General MIDI has on the key, since a kit that does not name its
//! recordings is nearly always laid out that way. Words that do say
//! something, of a drum this does not know, are what the key is called.

use trill_core::KeySound;

/// A key's words as a drum and a stroke: the place, what is in it, the
/// stroke. `None` for words that name no drum.
fn drum(words: &str) -> Option<(String, &'static str, &'static str)> {
    let said = words.to_lowercase();
    let has = |word: &str| said.contains(word);
    let parts: Vec<&str> = said.split(|c: char| !c.is_ascii_alphanumeric()).filter(|part| !part.is_empty()).collect();
    // A short code stands alone or before a number: `sd`, `sd2`.
    let code = |word: &str| {
        parts.iter().any(|part| {
            part.strip_prefix(word).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
        })
    };
    // The number a word carries: `tom2`, `tom 2`, `floor tom 1`.
    let number = || {
        parts.iter().find_map(|part| {
            let digits: String = part.chars().skip_while(|c| !c.is_ascii_digit()).collect();
            let named = part.chars().take_while(|c| !c.is_ascii_digit()).collect::<String>();
            let counts = named.is_empty() || named == "tom" || named == "crash" || named == "cymbal";
            digits.parse::<u32>().ok().filter(|number| counts && (1..=9).contains(number))
        })
    };

    if has("kick") || has("bass drum") || has("bassdrum") || code("bd") || code("kik") {
        return Some(("Kick".to_string(), "", "Hit"));
    }
    if has("snare") || code("sd") || code("sn") || code("snr") {
        let stroke = if has("rimshot") || has("rim shot") {
            "Rimshot"
        } else if has("side") || has("stick") || has("cross") || has("rim") {
            "Sidestick"
        } else if has("roll") {
            "Closed Roll"
        } else if has("flam") {
            "Flam"
        } else {
            "Center"
        };
        return Some(("Snare".to_string(), "", stroke));
    }
    if has("hat") || code("hh") || code("hho") || code("hhc") {
        let stroke = if has("pedal") || has("foot") || has("chick") {
            "Closed Pedal"
        } else if has("half") || has("semi") {
            "Open Edge 1"
        } else if has("open") || code("hho") || code("oh") {
            "Open Edge 2"
        } else {
            "Closed Tip"
        };
        return Some(("Hats".to_string(), "", stroke));
    }
    if has("tom") {
        // By its number where it has one; else by where it stands.
        let floor = has("floor") || has("low");
        let by_word = if has("hi") { 1 } else if has("mid") { 2 } else { 3 };
        let number = number().unwrap_or(if floor { 1 } else { by_word });
        let place = if floor { format!("Floortom {number}") } else { format!("Racktom {number}") };
        return Some((place, "", "Center"));
    }
    if has("ride") {
        return Some(("Ride".to_string(), "Ride", if has("bell") { "Bell Shank" } else { "Bow Tip" }));
    }
    for (word, what) in [("china", "China"), ("splash", "Splash"), ("crash", "Crash"), ("cymbal", "Crash")] {
        if has(word) {
            let stroke = if has("choke") || has("mute") { "Mute Hit" } else { "Crash" };
            return Some((format!("Cymbal {}", number().unwrap_or(1)), what, stroke));
        }
    }
    None
}

/// Words that name nothing: numbers, single letters, and what every
/// recording of a kit might be called.
fn says_nothing(words: &str) -> bool {
    const EMPTY: &[&str] = &["sample", "smp", "drum", "drums", "perc", "hit", "key", "note", "wav", "vel", "layer"];
    words.to_lowercase().split(|c: char| !c.is_ascii_alphabetic()).all(|part| part.len() <= 2 || EMPTY.contains(&part))
}

/// What `key` plays, given what the instrument says of it (`words`: its
/// label for the key, or the name of the recording).
pub(crate) fn sound(key: u8, words: &str) -> KeySound {
    let general = || {
        let name = trill_core::pitch::drum_name(key).filter(|_| says_nothing(words))?;
        drum(&name.replace('_', " "))
    };
    match drum(words).or_else(general) {
        Some((place, instrument, stroke)) => {
            KeySound { key, place, instrument: instrument.to_string(), stroke: stroke.to_string() }
        }
        None => KeySound { key, place: words.to_string(), instrument: String::new(), stroke: String::new() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(key: u8, words: &str) -> (String, String, String) {
        let sound = sound(key, words);
        (sound.place, sound.instrument, sound.stroke)
    }

    fn drum_of(words: &str) -> (String, String) {
        let (place, _, stroke) = said(100, words);
        (place, stroke)
    }

    #[test]
    fn the_names_of_recordings_say_which_drum_and_how_it_is_struck() {
        for (words, place, stroke) in [
            ("Kick 01", "Kick", "Hit"),
            ("BD_hard", "Kick", "Hit"),
            ("Bass Drum 2", "Kick", "Hit"),
            ("Snare Center", "Snare", "Center"),
            ("sd3", "Snare", "Center"),
            ("SN rimshot v4", "Snare", "Rimshot"),
            ("snare sidestick", "Snare", "Sidestick"),
            ("Snare Roll", "Snare", "Closed Roll"),
            ("HH closed", "Hats", "Closed Tip"),
            ("hihat open 3", "Hats", "Open Edge 2"),
            ("hat half open", "Hats", "Open Edge 1"),
            ("HiHat Pedal", "Hats", "Closed Pedal"),
            ("Tom 2", "Racktom 2", "Center"),
            ("tom1", "Racktom 1", "Center"),
            ("Floor Tom", "Floortom 1", "Center"),
            ("hi tom", "Racktom 1", "Center"),
            ("Ride Bell", "Ride", "Bell Shank"),
            ("ride ping", "Ride", "Bow Tip"),
            ("Crash 2", "Cymbal 2", "Crash"),
            ("china choke", "Cymbal 1", "Mute Hit"),
        ] {
            assert_eq!(drum_of(words), (place.to_string(), stroke.to_string()), "{words}");
        }
        assert_eq!(said(100, "China 18").1, "China");
    }

    #[test]
    fn words_that_name_no_drum_fall_back_on_general_midi_and_then_on_themselves() {
        // A kit whose recordings are numbered is laid out as General MIDI is.
        assert_eq!(said(36, "036").0, "Kick");
        assert_eq!((said(38, "sample 7").0, said(38, "sample 7").2), ("Snare".to_string(), "Center".to_string()));
        assert_eq!((said(46, "x").0, said(46, "x").2), ("Hats".to_string(), "Open Edge 2".to_string()));
        assert_eq!(said(49, "y").1, "Crash");
        // A key General MIDI has nothing on is called what the instrument calls it.
        assert_eq!(said(100, "Thunder Sheet"), ("Thunder Sheet".to_string(), String::new(), String::new()));
        // And what the words say goes before what the key would be, even of a drum that is not known here.
        assert_eq!(said(36, "Cowbell low").0, "Cowbell low");
        assert_eq!(said(60, "Kick sub").0, "Kick");
        assert!(says_nothing("036") && says_nothing("Sample 12 v3") && !says_nothing("Agogo"));
    }
}
