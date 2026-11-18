//! What the browser was showing when the program was last closed: the tab,
//! the search, the filters and the folder, so that the next session starts
//! where the last one left off.
//!
//! Kept in a file of its own beside the settings (`browser.json`), because
//! the settings file has a fixed set of names and drops what it does not
//! know. Losing this file costs nothing but a search to type again, so it
//! is read leniently: anything unreadable in it is the default.
//!
//! A run that keeps no settings (a screenshot, a test) has no folder to
//! keep this in either, and neither reads nor writes it.

use crate::ui::state::{BrowserState, BrowserTab, SoundForm, TempoFilter};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use trill_library::{Key, Tag};

const FILE: &str = "browser.json";

/// The part of the browser's state that outlives the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remembered {
    /// `"devices"` or `"sounds"`.
    tab: String,
    search: String,
    /// `"any"`, `"one_shot"` or `"loop"`.
    form: String,
    /// A tag's label, as the library spells it.
    kind: Option<String>,
    /// `"any"`, `"song"`, or the two ends of a range.
    tempo: Tempo,
    /// A key's name: `"F#m"`.
    key: Option<String>,
    favorites: bool,
    folder: Option<PathBuf>,
    expanded: Vec<PathBuf>,
    folders_open: bool,
    auto_preview: bool,
    preview_gain_db: f32,
    last_added: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Tempo {
    #[default]
    Any,
    Song,
    Between(f32, f32),
}

impl Default for Remembered {
    fn default() -> Self {
        Self::of(&BrowserState::new())
    }
}

impl Remembered {
    pub fn of(browser: &BrowserState) -> Self {
        let sounds = &browser.sounds;
        let mut expanded: Vec<PathBuf> = sounds.expanded.iter().cloned().collect();
        // A set has no order; the file should not change for no reason.
        expanded.sort();
        Self {
            tab: match browser.tab {
                BrowserTab::Devices => "devices",
                BrowserTab::Sounds => "sounds",
                BrowserTab::Studies => "studies",
            }
            .to_owned(),
            search: sounds.search.clone(),
            form: match sounds.form {
                SoundForm::Any => "any",
                SoundForm::OneShot => "one_shot",
                SoundForm::Loop => "loop",
            }
            .to_owned(),
            kind: sounds.tag.map(|tag| tag.label().to_owned()),
            tempo: match sounds.tempo {
                TempoFilter::Any => Tempo::Any,
                TempoFilter::Song => Tempo::Song,
                TempoFilter::Between(low, high) => Tempo::Between(low, high),
            },
            key: sounds.key.map(|key| key.name().to_owned()),
            favorites: sounds.favorites,
            folder: sounds.folder.clone(),
            expanded,
            folders_open: sounds.folders_open,
            auto_preview: sounds.auto_preview,
            preview_gain_db: sounds.preview_gain_db,
            last_added: sounds.last_added.clone(),
        }
    }

    /// Puts the browser back the way it was. What makes no sense any more
    /// (a tag this version does not have, a volume out of range) is left
    /// at what the browser starts with.
    pub fn apply(&self, browser: &mut BrowserState) {
        browser.tab = match self.tab.as_str() {
            "sounds" => BrowserTab::Sounds,
            "studies" => BrowserTab::Studies,
            _ => BrowserTab::Devices,
        };
        let sounds = &mut browser.sounds;
        sounds.search = self.search.clone();
        sounds.form = match self.form.as_str() {
            "one_shot" => SoundForm::OneShot,
            "loop" => SoundForm::Loop,
            _ => SoundForm::Any,
        };
        sounds.tag = self.kind.as_deref().and_then(Tag::from_label);
        sounds.tempo = match self.tempo {
            Tempo::Between(low, high) if low.is_finite() && high.is_finite() && low <= high => {
                TempoFilter::Between(low, high)
            }
            Tempo::Song => TempoFilter::Song,
            Tempo::Any | Tempo::Between(..) => TempoFilter::Any,
        };
        sounds.key = self.key.as_deref().and_then(Key::parse);
        sounds.favorites = self.favorites;
        sounds.folder = self.folder.clone();
        sounds.expanded = self.expanded.iter().cloned().collect();
        sounds.folders_open = self.folders_open;
        sounds.auto_preview = self.auto_preview;
        if (-36.0..=6.0).contains(&self.preview_gain_db) {
            sounds.preview_gain_db = self.preview_gain_db;
        }
        sounds.last_added = self.last_added.clone();
    }

    /// Reads what was remembered in `folder`; `None` if nothing was, or if
    /// the file cannot be made sense of.
    pub fn load(folder: &Path) -> Option<Self> {
        let text = std::fs::read(folder.join(FILE)).ok()?;
        serde_json::from_slice(&text).ok()
    }

    /// Writes it to `folder`, replacing the file in one step.
    pub fn save(&self, folder: &Path) -> Result<(), String> {
        let path = folder.join(FILE);
        let fail = |error: std::io::Error| format!("cannot remember the browser in '{}': {error}", path.display());
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

    fn browser() -> BrowserState {
        let mut browser = BrowserState::new();
        browser.tab = BrowserTab::Sounds;
        browser.width = 333.0;
        let sounds = &mut browser.sounds;
        sounds.search = "deep kick".into();
        sounds.form = SoundForm::OneShot;
        sounds.tag = Some(Tag::Kick);
        sounds.tempo = TempoFilter::Between(110.0, 129.0);
        sounds.key = Key::parse("F#m");
        sounds.favorites = true;
        sounds.folder = Some("D:/Samples/Drums".into());
        sounds.expanded.extend(["D:/Samples".into(), "D:/Samples/Drums".into()]);
        sounds.folders_open = false;
        sounds.auto_preview = false;
        sounds.preview_gain_db = -12.0;
        sounds.last_added = Some("D:/Samples".into());
        // What is not remembered: the selection belongs to the session.
        sounds.selected = Some("D:/Samples/Drums/Kick.wav".into());
        browser
    }

    #[test]
    fn the_browser_comes_back_as_it_was_left() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(Remembered::load(folder.path()), None, "nothing was remembered yet");
        let left = browser();
        let remembered = Remembered::of(&left);
        remembered.save(&folder.path().join("made").join("on the way")).unwrap();
        remembered.save(folder.path()).unwrap();
        assert_eq!(Remembered::load(folder.path()), Some(remembered.clone()));
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 2, "no temporary file is left behind");

        let mut next = BrowserState::new();
        Remembered::load(folder.path()).unwrap().apply(&mut next);
        assert_eq!(Remembered::of(&next), remembered);
        let (was, is) = (&left.sounds, &next.sounds);
        assert_eq!(next.tab, BrowserTab::Sounds);
        assert_eq!((is.search.as_str(), is.form, is.tag, is.tempo), ("deep kick", was.form, was.tag, was.tempo));
        assert_eq!((is.key, is.favorites, &is.folder, &is.expanded), (was.key, true, &was.folder, &was.expanded));
        assert_eq!((is.folders_open, is.auto_preview, is.preview_gain_db), (false, false, -12.0));
        assert_eq!(is.last_added, was.last_added);
        assert_eq!(is.selected, None);
        assert_eq!(next.width, BrowserState::DEFAULT_WIDTH, "the width is the settings' to keep");
    }

    #[test]
    fn a_file_from_another_version_or_a_damaged_one_does_no_harm() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join(FILE);
        // Names this version does not know are ignored, missing ones are
        // the defaults, and values that make no sense are left out.
        let text = r#"{ "tab": "sounds", "search": "pad", "kind": "theremin", "key": "H#", "form": "sideways",
            "tempo": { "between": [140.0, 90.0] }, "preview_gain_db": 400.0, "new_in_version_nine": [1, 2, 3] }"#;
        std::fs::write(&file, text).unwrap();
        let mut browser = BrowserState::new();
        Remembered::load(folder.path()).expect("it is read").apply(&mut browser);
        let sounds = &browser.sounds;
        assert_eq!((browser.tab, sounds.search.as_str()), (BrowserTab::Sounds, "pad"));
        assert_eq!((sounds.tag, sounds.key, sounds.form, sounds.tempo), (None, None, SoundForm::Any, TempoFilter::Any));
        assert_eq!(sounds.preview_gain_db, -6.0);
        assert!(sounds.auto_preview && sounds.folders_open, "what the file does not say stays as the browser starts");

        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(Remembered::load(folder.path()), None);
        // Whatever else is in the file, the worst it can be is the defaults.
        std::fs::write(&file, "[]").unwrap();
        assert_eq!(Remembered::load(folder.path()).unwrap_or_default(), Remembered::default());
        assert_eq!(Remembered::default(), Remembered::of(&BrowserState::new()));
    }
}
