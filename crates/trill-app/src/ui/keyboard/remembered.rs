//! What the computer keyboard was set to when the program was last closed:
//! whether it was on, its octave and velocity, and whether the picture of
//! its keys was showing.
//!
//! Kept in a file of its own beside the settings (`keyboard.json`), as the
//! browser keeps what it showed, and read as leniently: anything in it
//! that makes no sense is what the keyboard starts with. A run that keeps
//! no settings (a screenshot, a test) neither reads nor writes it.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

const FILE: &str = "keyboard.json";

/// The part of the keyboard's state that outlives the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remembered {
    pub on: bool,
    /// MIDI pitch of the lowest key: a C.
    pub lowest: u8,
    pub velocity: u8,
    /// Whether the strip shows the picture of the keys.
    pub key_map: bool,
}

impl Default for Remembered {
    fn default() -> Self {
        Self { on: false, lowest: super::DEFAULT_LOWEST, velocity: super::DEFAULT_VELOCITY, key_map: true }
    }
}

impl Remembered {
    /// Reads what was remembered in `folder`; `None` if nothing was, or if
    /// the file cannot be made sense of.
    pub fn load(folder: &Path) -> Option<Self> {
        let text = std::fs::read(folder.join(FILE)).ok()?;
        serde_json::from_slice(&text).ok()
    }

    /// Writes it to `folder`, replacing the file in one step.
    pub fn save(&self, folder: &Path) -> Result<(), String> {
        let path = folder.join(FILE);
        let fail = |error: std::io::Error| format!("cannot remember the keyboard in '{}': {error}", path.display());
        let mut text = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        text.push(b'\n');
        std::fs::create_dir_all(folder).map_err(fail)?;
        let temporary = folder.join(format!("{FILE}.{}.part", std::process::id()));
        let written = std::fs::File::create(&temporary)
            .and_then(|mut file| file.write_all(&text))
            .and_then(|()| std::fs::rename(&temporary, &path));
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written.map_err(fail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_comes_back_as_it_was_left() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(Remembered::load(folder.path()), None, "nothing was remembered yet");
        let left = Remembered { on: true, lowest: 36, velocity: 70, key_map: false };
        left.save(folder.path()).unwrap();
        assert_eq!(Remembered::load(folder.path()), Some(left));
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1, "no temporary file is left behind");
    }

    #[test]
    fn a_file_from_another_version_or_a_damaged_one_does_no_harm() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join(FILE);
        // Names this version does not know are ignored and missing ones
        // are what the keyboard starts with.
        std::fs::write(&file, r#"{ "on": true, "aftertouch": "poly" }"#).unwrap();
        let read = Remembered::load(folder.path()).expect("it is read");
        assert_eq!(read, Remembered { on: true, ..Remembered::default() });
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(Remembered::load(folder.path()), None);
        std::fs::write(&file, r#"{ "velocity": 900 }"#).unwrap();
        assert_eq!(Remembered::load(folder.path()), None, "a number that is no velocity is no file");
    }
}
