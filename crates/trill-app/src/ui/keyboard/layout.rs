//! Which key of the computer keyboard does what: one table, read by the key
//! handling, by the pictures of the keys and by the shortcut reference.
//!
//! The home row is the white keys and the row above it the black ones, as
//! on Ableton's computer MIDI keyboard and in every "musical typing" window
//! since: `A` is a C, `W` the C sharp above it, and so on up to an F an
//! octave and a fourth higher.

use eframe::egui::Key;

/// What a key of the computer keyboard is for while the keyboard plays.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Role {
    /// Plays the note this many semitones above the keyboard's lowest C.
    Note(u8),
    /// Moves the keys down or up by an octave.
    Octave(i8),
    /// Makes the next notes softer or louder.
    Velocity(i8),
    /// Holds what is played for as long as it is down, like a piano's
    /// right pedal.
    Sustain,
}

/// A key that plays, the letter printed on it, and what it does.
pub struct PlayingKey {
    pub key: Key,
    pub label: &'static str,
    pub role: Role,
}

const fn note(key: Key, label: &'static str, semitone: u8) -> PlayingKey {
    PlayingKey { key, label, role: Role::Note(semitone) }
}

/// The keys that play notes, lowest first.
pub const NOTES: [PlayingKey; 18] = [
    note(Key::A, "A", 0),
    note(Key::W, "W", 1),
    note(Key::S, "S", 2),
    note(Key::E, "E", 3),
    note(Key::D, "D", 4),
    note(Key::F, "F", 5),
    note(Key::T, "T", 6),
    note(Key::G, "G", 7),
    note(Key::Y, "Y", 8),
    note(Key::H, "H", 9),
    note(Key::U, "U", 10),
    note(Key::J, "J", 11),
    note(Key::K, "K", 12),
    note(Key::O, "O", 13),
    note(Key::L, "L", 14),
    note(Key::P, "P", 15),
    note(Key::Semicolon, ";", 16),
    note(Key::Quote, "'", 17),
];

/// The keys that change how the others play.
pub const CONTROLS: [PlayingKey; 5] = [
    PlayingKey { key: Key::Z, label: "Z", role: Role::Octave(-1) },
    PlayingKey { key: Key::X, label: "X", role: Role::Octave(1) },
    PlayingKey { key: Key::C, label: "C", role: Role::Velocity(-1) },
    PlayingKey { key: Key::V, label: "V", role: Role::Velocity(1) },
    PlayingKey { key: Key::Tab, label: "Tab", role: Role::Sustain },
];

/// Semitones from the lowest key to the highest.
pub const SPAN: u8 = NOTES.len() as u8 - 1;

/// The key a key event is about: where the key is on the keyboard when
/// the system says (`physical`), so that the two rows are a piano on every
/// layout (on AZERTY the home row begins with Q), else what it types.
///
/// A key is known by this from the moment it goes down until it comes up,
/// so nothing here may depend on Shift: with Shift held, the key that types
/// `;` types `:` instead, and is still the same key.
pub fn identify(logical: Key, physical: Option<Key>) -> Key {
    match physical.unwrap_or(logical) {
        Key::Colon => Key::Semicolon,
        key => key,
    }
}

/// What `key` is for while the keyboard plays, if anything.
pub fn role(key: Key) -> Option<Role> {
    NOTES.iter().chain(&CONTROLS).find(|playing| playing.key == key).map(|playing| playing.role)
}

/// Whether the keyboard takes `key` for itself while it is on: the letters
/// whose own shortcuts then need Alt.
pub fn plays(key: Key) -> bool {
    role(key).is_some()
}

/// The letter on the key that plays `semitone` above the lowest C.
pub fn label_for(semitone: u8) -> Option<&'static str> {
    NOTES.iter().find(|playing| playing.role == Role::Note(semitone)).map(|playing| playing.label)
}

/// The key with `letter` on it, for options that name keys by their
/// letters: a note, or one of the keys that move the octave and the
/// velocity.
pub fn key_lettered(letter: char) -> Option<Key> {
    let mut buffer = [0; 4];
    let label: &str = letter.encode_utf8(&mut buffer);
    NOTES.iter().chain(&CONTROLS).find(|playing| playing.label.eq_ignore_ascii_case(label)).map(|playing| playing.key)
}

/// The label of a control key.
fn control(role: Role) -> &'static str {
    CONTROLS.iter().find(|playing| playing.role == role).map_or("", |playing| playing.label)
}

/// The labels of the note keys on white (`black` false) or black keys, in
/// order: `"A S D F G H J K L ; '"`.
fn row(black: bool) -> String {
    let labels = NOTES.iter().filter(|playing| match playing.role {
        Role::Note(semitone) => is_black(semitone) == black,
        _ => false,
    });
    labels.map(|playing| playing.label).collect::<Vec<_>>().join(" ")
}

/// Whether the key `semitone` above a C is a black one.
pub fn is_black(semitone: u8) -> bool {
    matches!(semitone % 12, 1 | 3 | 6 | 8 | 10)
}

/// What the keys do, for the shortcut reference: what happens, then the
/// keys for it. Made from the table above, so the reference cannot list a
/// key that does something else.
pub fn reference() -> Vec<(String, String)> {
    let pair = |down: Role, up: Role| format!("{} / {}", control(down), control(up));
    vec![
        ("White keys, from C".to_owned(), row(false)),
        ("Black keys".to_owned(), row(true)),
        ("Octave down, up".to_owned(), pair(Role::Octave(-1), Role::Octave(1))),
        ("Softer, louder".to_owned(), pair(Role::Velocity(-1), Role::Velocity(1))),
        ("Sustain, while held".to_owned(), control(Role::Sustain).to_owned()),
        ("Shortcuts on those letters, while on".to_owned(), "Alt+letter".to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_rows_are_a_piano_from_c() {
        // The layout musicians know: the home row is the white keys.
        assert_eq!(row(false), "A S D F G H J K L ; '");
        assert_eq!(row(true), "W E T Y U O P");
        let semitones: Vec<u8> = NOTES
            .iter()
            .map(|playing| match playing.role {
                Role::Note(semitone) => semitone,
                other => panic!("{other:?} among the notes"),
            })
            .collect();
        assert_eq!(semitones, (0..=SPAN).collect::<Vec<u8>>(), "every semitone once, lowest first");
        assert_eq!(SPAN, 17, "from a C up to the F an octave above");
        assert_eq!(label_for(0), Some("A"));
        assert_eq!(label_for(12), Some("K"), "the octave is under the right hand");
        assert_eq!(label_for(18), None);
    }

    #[test]
    fn no_key_has_two_jobs() {
        let all: Vec<Key> = NOTES.iter().chain(&CONTROLS).map(|playing| playing.key).collect();
        for (index, key) in all.iter().enumerate() {
            assert!(!all[..index].contains(key), "{key:?} is listed twice");
            assert!(plays(*key));
        }
        assert_eq!(role(Key::Z), Some(Role::Octave(-1)));
        assert_eq!(role(Key::X), Some(Role::Octave(1)));
        assert_eq!(role(Key::C), Some(Role::Velocity(-1)));
        assert_eq!(role(Key::V), Some(Role::Velocity(1)));
        assert_eq!(role(Key::Tab), Some(Role::Sustain));
        for free in [Key::Q, Key::R, Key::I, Key::B, Key::N, Key::M, Key::Space, Key::Num0, Key::Enter] {
            assert_eq!(role(free), None, "{free:?} is left to the rest of the window");
        }
    }

    #[test]
    fn a_key_is_known_by_where_it_is() {
        // AZERTY: the key where QWERTY has A types Q, and it is the C.
        assert_eq!(identify(Key::Q, Some(Key::A)), Key::A);
        assert_eq!(role(identify(Key::Q, Some(Key::A))), Some(Role::Note(0)));
        // A backend that does not say where a key is: what it types.
        assert_eq!(identify(Key::A, None), Key::A);
        // Shift between a key going down and coming up changes what it
        // types and not which key it is.
        assert_eq!(identify(Key::Colon, None), Key::Semicolon);
        assert_eq!(identify(Key::Colon, Some(Key::Semicolon)), Key::Semicolon);
    }

    #[test]
    fn keys_are_found_by_their_letters() {
        assert_eq!(key_lettered('a'), Some(Key::A));
        assert_eq!(key_lettered('K'), Some(Key::K));
        assert_eq!(key_lettered(';'), Some(Key::Semicolon));
        assert_eq!(key_lettered('z'), Some(Key::Z), "the octave keys have letters too");
        assert_eq!(key_lettered('q'), None);
        assert_eq!(key_lettered('T'), Some(Key::T), "T is the F sharp, not the start of Tab");
    }

    #[test]
    fn the_reference_is_made_from_the_table() {
        let lines = reference();
        let keys_for = |does: &str| lines.iter().find(|(what, _)| what == does).map(|(_, keys)| keys.as_str());
        assert_eq!(keys_for("White keys, from C"), Some("A S D F G H J K L ; '"));
        assert_eq!(keys_for("Black keys"), Some("W E T Y U O P"));
        assert_eq!(keys_for("Octave down, up"), Some("Z / X"));
        assert_eq!(keys_for("Softer, louder"), Some("C / V"));
        assert_eq!(keys_for("Sustain, while held"), Some("Tab"));
    }
}
