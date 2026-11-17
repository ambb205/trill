//! What is on each key of an instrument whose keys are drums, in the
//! words the rest of the program names drums by.
//!
//! A drum plugin says of its kit which drum hangs in each place and which
//! keys strike it how ([`KeySound`]), and a track's drum names are worked
//! out from that. An instrument's file says less: at best a label for a
//! key, usually only the names of the recordings and of the folders they
//! are in (`Kick_01`, `sn-rim`, `Samples/oh/htom/oh_htom_center_vl3`,
//! `Samples/China/1-China-AmbL`). This reads a drum and a stroke out of
//! such words and says them as a kit does: a place (`Kick`, `Snare`,
//! `Hats`, `Racktom 2`, `Cymbal 1`), what is in it for a cymbal (`Crash`,
//! `China`), and the stroke (`Center`, `Rimshot`, `Open Edge 2`).
//!
//! * The folder a recording is in says more than its name where a kit is
//!   recorded through several microphones: every drum is in the kick's
//!   microphone too, and the file says so. So the folder is asked first,
//!   and words that name a microphone are taken out of the file's name.
//! * A key has many recordings (takes, layers, microphones): what most of
//!   them say is what the key is.
//! * Words that say nothing at all (a number, `sample 7`) fall back on
//!   what General MIDI has on the key, since a kit that does not name its
//!   recordings is nearly always laid out that way. Words that do say
//!   something, of a drum this does not know, are what the key is called.

use trill_core::KeySound;

/// A drum read out of words: the place, what is in it, the stroke.
type Read = (String, &'static str, &'static str);

/// Words that name a microphone, not a drum.
fn is_microphone(word: &str) -> bool {
    const NAMED: &[&str] = &[
        "oh",
        "ohl",
        "ohr",
        "overhead",
        "overheads",
        "room",
        "rooml",
        "roomr",
        "amb",
        "ambl",
        "ambr",
        "ambience",
        "close",
        "far",
        "mid",
        "lofi",
        "mono",
        "stereo",
        "trigger",
        "top",
        "bottom",
        "front",
        "direct",
    ];
    NAMED.contains(&word) || (word.len() > 3 && word.ends_with("mic"))
}

/// The words of a name, lower case, without the ones that name a
/// microphone.
fn words_of(name: &str) -> Vec<String> {
    name.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty() && !is_microphone(word))
        .map(str::to_string)
        .collect()
}

/// Words as a drum and a stroke. `None` for words that name no drum.
fn drum(name: &str) -> Option<Read> {
    let parts = words_of(name);
    let said = parts.join(" ");
    let has = |word: &str| said.contains(word);
    // A short code stands alone or before a number: `sd`, `sd2`.
    let code = |word: &str| {
        parts.iter().any(|part| part.strip_prefix(word).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit())))
    };
    // The number a drum's own word carries (`tom2`), or the word after it (`tom 2`).
    let number_of = |word: &str| {
        let at = parts.iter().position(|part| part.contains(word))?;
        let own: String = parts[at].chars().filter(char::is_ascii_digit).collect();
        let next = parts.get(at + 1).filter(|next| next.chars().all(|c| c.is_ascii_digit()));
        let digits = if own.is_empty() { next.cloned().unwrap_or_default() } else { own };
        digits.parse::<u32>().ok().filter(|number| (1..=9).contains(number))
    };

    if has("kick") || has("kdrum") || has("bass drum") || has("bassdrum") || code("bd") || code("kik") {
        return Some(("Kick".to_string(), "", "Hit"));
    }
    if has("snare") || code("sd") || code("sn") || code("snr") {
        let stroke = if has("rimshot") || has("rim shot") {
            "Rimshot"
        } else if has("side") || has("stick") || has("cross") || has("rim") {
            "Sidestick"
        } else if has("roll") || has("buzz") {
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
        } else if has("half") || has("semi") || parts.iter().any(|part| part == "14" || part == "12") {
            "Open Edge 1"
        } else if has("open") || code("hho") || parts.iter().any(|part| part == "34") {
            "Open Edge 2"
        } else {
            "Closed Tip"
        };
        return Some(("Hats".to_string(), "", stroke));
    }
    if has("tom") {
        // By its number where it has one; else by where it stands.
        let floor = has("floor") || has("ftom") || has("ltom") || has("low");
        let by_word = if has("htom") || has("hi") {
            1
        } else if has("mtom") || has("mid") {
            2
        } else {
            3
        };
        let number = number_of("tom").unwrap_or(if floor { 1 } else { by_word });
        let place = if floor { format!("Floortom {number}") } else { format!("Racktom {number}") };
        return Some((place, "", "Center"));
    }
    if has("ride") {
        return Some(("Ride".to_string(), "Ride", if has("bell") { "Bell Shank" } else { "Bow Tip" }));
    }
    for (word, what) in [("china", "China"), ("splash", "Splash"), ("crash", "Crash"), ("cymbal", "Crash")] {
        if has(word) {
            let stroke = if has("choke") || has("mute") { "Mute Hit" } else { "Crash" };
            // Which cymbal it is, is settled when the whole kit is known.
            return Some(("Cymbal".to_string(), what, stroke));
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

/// The name of a recording's file as words: `Drums/Kick_01.wav` is
/// "Kick 01".
fn file_words(recording: &str) -> String {
    let file = recording.rsplit('/').next().unwrap_or(recording);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    stem.replace(['_', '-'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The folder a recording is in.
fn folder_of(recording: &str) -> &str {
    let mut parts = recording.rsplit('/');
    parts.next();
    parts.next().unwrap_or("")
}

/// What a key plays, by what the instrument says of it: its `label` for
/// the key if it has one, else the `recordings` the key's regions play.
/// With it comes what tells one cymbal from another, for [`sounds`].
fn read(key: u8, label: Option<&str>, recordings: &[&str]) -> (Option<Read>, String) {
    if let Some(label) = label {
        let general = || trill_core::pitch::drum_name(key).filter(|_| says_nothing(label));
        let found = drum(label).or_else(|| general().and_then(|name| drum(&name.replace('_', " "))));
        return (found, label.to_string());
    }
    // What most of its recordings say, each by its folder and failing
    // that by its own name.
    let mut votes: Vec<(Read, String, usize)> = Vec::new();
    for recording in recordings {
        let folder = folder_of(recording);
        let stem = file_words(recording);
        let (found, told) = match drum(folder) {
            // The folder says the drum; the name may say the stroke.
            Some(by_folder) => match drum(&format!("{folder} {stem}")) {
                Some(both) if both.0 == by_folder.0 => (Some(both), folder.to_string()),
                _ => (Some(by_folder), folder.to_string()),
            },
            None => (drum(&stem), stem.clone()),
        };
        let Some(found) = found else { continue };
        match votes.iter_mut().find(|(known, _, _)| *known == found) {
            Some((_, _, count)) => *count += 1,
            None => votes.push((found, told, 1)),
        }
    }
    if let Some((found, told, _)) = votes.into_iter().max_by_key(|(_, _, count)| *count) {
        return (Some(found), told);
    }
    let words = recordings.first().map(|recording| file_words(recording)).unwrap_or_default();
    let general = trill_core::pitch::drum_name(key).filter(|_| says_nothing(&words));
    (general.and_then(|name| drum(&name.replace('_', " "))), words)
}

/// What every key of a kit plays: `keys` are the keys that sound, each
/// with the label the instrument has for it and the recordings its
/// regions play.
pub(crate) fn sounds(keys: &[(u8, Option<&str>, Vec<&str>)]) -> Vec<KeySound> {
    // Each cymbal by what the kit calls it: the first is `Cymbal 1`.
    let mut cymbals: Vec<String> = Vec::new();
    keys.iter()
        .map(|(key, label, recordings)| {
            let (found, told) = read(*key, *label, recordings);
            match found {
                Some((place, instrument, stroke)) => {
                    let place = if place == "Cymbal" {
                        // Without what says how it is struck: a choke is the same cymbal.
                        let which: Vec<String> = words_of(&told)
                            .into_iter()
                            .filter(|word| !["choke", "mute", "hit"].contains(&word.as_str()))
                            .collect();
                        let which = which.join(" ");
                        let at = cymbals.iter().position(|known| *known == which).unwrap_or_else(|| {
                            cymbals.push(which);
                            cymbals.len() - 1
                        });
                        format!("Cymbal {}", at + 1)
                    } else {
                        place
                    };
                    KeySound { key: *key, place, instrument: instrument.to_string(), stroke: stroke.to_string() }
                }
                None => KeySound { key: *key, place: told, instrument: String::new(), stroke: String::new() },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labelled(key: u8, label: &str) -> (String, String, String) {
        let sound = sounds(&[(key, Some(label), Vec::new())]).remove(0);
        (sound.place, sound.instrument, sound.stroke)
    }

    fn drum_of(words: &str) -> (String, String) {
        let (place, _, stroke) = labelled(100, words);
        (place, stroke)
    }

    #[test]
    fn the_names_of_recordings_say_which_drum_and_how_it_is_struck() {
        for (words, place, stroke) in [
            ("Kick 01", "Kick", "Hit"),
            ("BD_hard", "Kick", "Hit"),
            ("Bass Drum 2", "Kick", "Hit"),
            ("KdrumL", "Kick", "Hit"),
            ("Snare Center", "Snare", "Center"),
            ("sd3", "Snare", "Center"),
            ("SN rimshot v4", "Snare", "Rimshot"),
            ("snare sidestick", "Snare", "Sidestick"),
            ("Snare Roll", "Snare", "Closed Roll"),
            ("HH closed", "Hats", "Closed Tip"),
            ("hihat open 3", "Hats", "Open Edge 2"),
            ("hat half open", "Hats", "Open Edge 1"),
            ("hh 34 vl2", "Hats", "Open Edge 2"),
            ("HiHat Pedal", "Hats", "Closed Pedal"),
            ("Tom 2", "Racktom 2", "Center"),
            ("1-Tom4-AmbL", "Racktom 4", "Center"),
            ("tom1", "Racktom 1", "Center"),
            ("Floor Tom", "Floortom 1", "Center"),
            ("htom center vl3", "Racktom 1", "Center"),
            ("ltom", "Floortom 1", "Center"),
            ("Ride Bell", "Ride", "Bell Shank"),
            ("flatride ping", "Ride", "Bow Tip"),
            ("Crash 2", "Cymbal 1", "Crash"),
            ("china choke", "Cymbal 1", "Mute Hit"),
        ] {
            assert_eq!(drum_of(words), (place.to_string(), stroke.to_string()), "{words}");
        }
        assert_eq!(labelled(100, "China 18").1, "China");
    }

    #[test]
    fn a_kit_recorded_through_several_microphones_is_named_by_its_drums_not_by_its_microphones() {
        // Every drum is in the kick's microphone too, and the snare's, and
        // the files say which microphone before they say which drum.
        let kit = [
            (36u8, None, vec!["Samples/kickmic/kick/kickmic_kick_vl1.flac", "Samples/oh/kick/oh_kick_vl1.flac"]),
            (
                38,
                None,
                vec![
                    "Samples/kickmic/snare/kickmic_snare_center_vl1.flac",
                    "Samples/snaremic/snare/snaremic_snare_center_vl1.flac",
                ],
            ),
            (42, None, vec!["Samples/kickmic/hh/kickmic_hh_closed_vl1.flac", "Samples/oh/hh/oh_hh_closed_vl1.flac"]),
            (46, None, vec!["Samples/oh/hh/oh_hh_34_vl1_rr1.flac", "Samples/snaremic/hh/snaremic_hh_34_vl1_rr1.flac"]),
            (48, None, vec!["Samples/oh/htom/oh_htom_center_vl1.flac"]),
            (
                49,
                None,
                vec![
                    "Samples/China/1-China-AmbL.flac",
                    "Samples/China/1-China-KdrumL.flac",
                    "Samples/China/2-China-Snare.flac",
                ],
            ),
            (57, None, vec!["Samples/CrashL/1-CrashL-OHL.flac"]),
            (55, None, vec!["Samples/CrashR/1-CrashR-OHL.flac"]),
            (52, None, vec!["Samples/CrashR/1-CrashR-choke-OHL.flac"]),
            (35, None, vec!["Samples/KdrumL/1-KdrumL-KdrumL.flac", "Samples/KdrumL/1-KdrumL-Snare.flac"]),
        ];
        let said: Vec<(u8, String, String, String)> =
            sounds(&kit).into_iter().map(|sound| (sound.key, sound.place, sound.instrument, sound.stroke)).collect();
        let at = |key: u8| {
            said.iter()
                .find(|(known, ..)| *known == key)
                .map(|(_, place, what, stroke)| (place.as_str(), what.as_str(), stroke.as_str()))
                .unwrap()
        };
        assert_eq!(at(36), ("Kick", "", "Hit"));
        assert_eq!(at(38), ("Snare", "", "Center"));
        assert_eq!((at(42), at(46)), (("Hats", "", "Closed Tip"), ("Hats", "", "Open Edge 2")));
        assert_eq!(at(48), ("Racktom 1", "", "Center"));
        // The cymbals each have a place of their own, and a choke is a stroke of the cymbal it chokes.
        assert_eq!(at(49), ("Cymbal 1", "China", "Crash"));
        assert_eq!((at(57), at(55)), (("Cymbal 2", "Crash", "Crash"), ("Cymbal 3", "Crash", "Crash")));
        assert_eq!(at(52), ("Cymbal 3", "Crash", "Mute Hit"));
        assert_eq!(at(35), ("Kick", "", "Hit"), "a kick in its own microphone, not a snare");
    }

    #[test]
    fn words_that_name_no_drum_fall_back_on_general_midi_and_then_on_themselves() {
        let named = |key: u8, recording: &'static str| {
            let sound = sounds(&[(key, None, vec![recording])]).remove(0);
            (sound.place, sound.stroke)
        };
        // A kit whose recordings are numbered is laid out as General MIDI is.
        assert_eq!(named(36, "kit/036.wav").0, "Kick");
        assert_eq!(named(38, "kit/sample 7.wav"), ("Snare".to_string(), "Center".to_string()));
        assert_eq!(named(46, "x.wav"), ("Hats".to_string(), "Open Edge 2".to_string()));
        assert_eq!(labelled(49, "y").1, "Crash");
        // A key General MIDI has nothing on is called what the instrument calls it.
        assert_eq!(labelled(100, "Thunder Sheet"), ("Thunder Sheet".to_string(), String::new(), String::new()));
        // And what the words say goes before what the key would be, even of a drum that is not known here.
        assert_eq!(labelled(36, "Cowbell low").0, "Cowbell low");
        assert_eq!(named(36, "perc/Agogo High v1.wav").0, "Agogo High v1");
        assert_eq!(labelled(60, "Kick sub").0, "Kick");
        assert!(says_nothing("036") && says_nothing("Sample 12 v3") && !says_nothing("Agogo"));
    }
}
