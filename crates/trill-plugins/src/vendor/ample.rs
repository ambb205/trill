//! Ample Sound's guitars and basses: the preset files in each
//! instrument's `Presets4` folder, and the settings in its state.
//!
//! The state of one of these instruments is one XML document, in JUCE's
//! envelope ([`super::juce`]): some 470 settings as
//! `<Parameter Name="..." Value="..."/>`, the set of samples in use
//! (`DBName`: for a guitar, which pickup was recorded), chords, strum
//! patterns and the paths of its folders. A preset file is the same
//! document as it was when someone saved it, on their machine.
//!
//! So a preset is loaded by laying its settings over the ones the plugin
//! has now, in the plugin's own document: every value that the preset
//! names is replaced, and nothing else is touched, least of all the paths.
//! If the preset is for another set of samples and that set is installed,
//! the document is switched to it as well, and the plugin reads those
//! samples in the background.
//!
//! The same document is how settings are reached that the plugin shows a
//! host no parameter for: [`settings`] and [`with_settings`].

use super::juce::Envelope;
use super::xml::{self, Tag};
use super::{Built, Mark, VendorPlaces, read_start, same_value};
use crate::presets::store::PluginRef;
use crate::presets::{Locator, NAME_LIMIT, Sink, Watch, tidy, walk};
use crate::vst3::state::PluginState;
use std::ops::Range;
use std::path::{Path, PathBuf};
use trill_core::presets::PresetInfo;

/// What the class ids of Ample Sound's plugins begin with: JUCE's own
/// prefix, then the maker's four letters, `Apsd`.
const MAKER: &str = "ABCDEF019182FAEB41707364";
/// How much of a preset file says what it is a preset of.
const ROOT_ROOM: usize = 4096;
/// The setting that says which set of samples is in use.
pub(crate) const LIBRARY: &str = "DBName";
/// What the plugin goes on doing when it is switched to other samples.
const LIBRARY_NOTE: &str = "the plugin reads those samples in the background: for some seconds its notes are silent \
                            or cut short";

pub(crate) fn is_ours(id: &str) -> bool {
    id.get(..MAKER.len()).is_some_and(|start| start.eq_ignore_ascii_case(MAKER))
}

/// The folders an instrument's presets are in, with the extension its
/// preset files have. An instrument's folder under `Ample Sound` in the
/// user's documents is named as the plugin's file is (`AMH`), and so are
/// its preset files (`.amhstate`).
fn folders(plugin: &PluginRef, places: &VendorPlaces) -> Vec<(PathBuf, String)> {
    let Some(code) = plugin.path.file_stem().map(|stem| stem.to_string_lossy().into_owned()) else { return Vec::new() };
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Vec::new();
    }
    let extension = format!("{}state", code.to_lowercase());
    places
        .documents
        .iter()
        .map(|documents| (documents.join("Ample Sound").join(&code).join("Presets4"), extension.clone()))
        .collect()
}

/// Lists the preset files in an instrument's folders: each by its file's
/// name, with what its first line says of it (who made it, what kind of
/// playing it is for, which pickup).
pub(crate) fn list(plugin: &PluginRef, places: &VendorPlaces, sink: &mut dyn Sink) {
    for (folder, extension) in folders(plugin, places) {
        let extensions = vec![extension];
        sink.watch(Watch { path: folder.clone(), extensions: extensions.clone() });
        let mut batch = Vec::new();
        for file in walk::files(&folder, &extensions) {
            let Ok(start) = read_start(&file, ROOT_ROOM) else { continue };
            let start = String::from_utf8_lossy(&start);
            let Some(Ok(Tag::Start { name, attributes, .. })) = xml::tags(&start).next() else { continue };
            if !name.ends_with("_STATE") {
                continue;
            }
            let said = |wanted: &str| {
                let found = attributes.iter().find(|attribute| attribute.name == wanted);
                tidy(&found.map(|attribute| attribute.value()).unwrap_or_default(), NAME_LIMIT)
            };
            let stem = file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
            let tags = ["Type", "Dist", "Lib"].map(said).map(|tag| tag.to_lowercase());
            let mut kinds: Vec<String> = Vec::new();
            for tag in tags.into_iter().filter(|tag| !tag.is_empty()) {
                if !kinds.contains(&tag) {
                    kinds.push(tag);
                }
            }
            batch.push(PresetInfo {
                name: tidy(&stem, NAME_LIMIT),
                category: said("Category"),
                author: said("Author"),
                tags: kinds,
                locator: Locator::Vendor { format: super::Format::Ample, path: file, key: None }.to_text(),
                ..PresetInfo::default()
            });
        }
        sink.presets(batch);
    }
}

/// One setting of a document, and where its value is written.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Setting {
    name: String,
    value: String,
    at: Range<usize>,
}

/// What this reader knows of one of these documents.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Document {
    /// The name of its root element, which says what kind of instrument
    /// it is a document of (`AMG_STATE`: a metal guitar).
    kind: String,
    /// The version of the plugin that wrote it.
    version: String,
    /// What it is called, and where that is written.
    name: Option<Setting>,
    settings: Vec<Setting>,
    /// The set of samples in use.
    library: Option<Setting>,
    /// The tuning file, which is named after the set of samples.
    tune: Option<Setting>,
}

impl Document {
    fn read(text: &str) -> Result<Self, String> {
        let mut document = Document::default();
        for tag in xml::tags(text) {
            let Tag::Start { name, attributes, .. } = tag? else { continue };
            let said = |wanted: &str| attributes.iter().find(|attribute| attribute.name == wanted);
            let setting = |named: &str, wanted: &str| {
                said(wanted).map(|value| Setting {
                    name: named.to_string(),
                    value: value.value(),
                    at: value.at.clone(),
                })
            };
            if document.kind.is_empty() {
                document.kind = name.to_string();
                document.version = said("Ver").map(|version| version.value()).unwrap_or_default();
                document.name = setting("Name", "Name");
                continue;
            }
            match name {
                "Parameter" => {
                    if let (Some(named), Some(value)) = (said("Name"), said("Value")) {
                        document.settings.push(Setting {
                            name: named.value(),
                            value: value.value(),
                            at: value.at.clone(),
                        });
                    }
                }
                "DBName" => document.library = setting(LIBRARY, "Value"),
                "Tune-Path" => document.tune = setting("Tune-Path", "Value"),
                _ => {}
            }
        }
        if !document.kind.ends_with("_STATE") || document.settings.is_empty() {
            return Err("it is not one of Ample Sound's documents of settings".to_string());
        }
        Ok(document)
    }

    fn setting(&self, name: &str) -> Option<&Setting> {
        self.settings.iter().find(|setting| setting.name == name)
    }
}

/// The document a plugin's state is, and the envelope it is in.
fn state_document(state: &PluginState) -> Result<(Envelope<'_>, Document), String> {
    let envelope =
        Envelope::open(&state.component).ok_or("the plugin's state is not the document this reader knows")?;
    let document =
        Document::read(envelope.text).map_err(|reason| format!("the plugin's state cannot be read: {reason}"))?;
    Ok((envelope, document))
}

/// The tuning file's path for another set of samples: the file is named
/// after the set (`Tune_Default.AMHBg`).
fn tune_for(path: &str, library: &str) -> Option<String> {
    let (stem, _) = path.rsplit_once('.')?;
    Some(format!("{stem}.{library}"))
}

/// The state that makes the plugin the preset in `file`: the document of
/// `current`, the plugin's state now, with the preset's settings laid
/// over its own.
pub(crate) fn build(file: &[u8], current: &PluginState) -> Result<Built, String> {
    let (envelope, ours) = state_document(current)?;
    let preset = String::from_utf8_lossy(file);
    let theirs = Document::read(&preset)?;
    if theirs.kind != ours.kind {
        return Err(format!("it is a preset of another instrument (its settings are '{}')", theirs.kind));
    }

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut marks = Vec::new();
    let mut known = 0;
    for setting in &ours.settings {
        let Some(theirs) = theirs.setting(&setting.name) else { continue };
        known += 1;
        if !same_value(&setting.value, &theirs.value) {
            edits.push((setting.at.clone(), xml::escape(&theirs.value)));
            marks.push(Mark { name: setting.name.clone(), value: theirs.value.clone() });
        }
    }
    // The instruments' own folders hold presets saved by the version
    // before (3.7 beside 4.1), all of whose settings the plugin still
    // has: what tells a preset of this plugin is that, not its version.
    if known == 0 {
        return Err("none of its settings is one this plugin has".to_string());
    }
    if known * 2 < theirs.settings.len() {
        return Err(format!(
            "it is not a preset of this plugin, or of a version of it this one can take (version {}; the plugin \
             is version {} and has {known} of its {} settings)",
            theirs.version,
            ours.version,
            theirs.settings.len()
        ));
    }
    if let (Some(name), Some(theirs)) = (&ours.name, &theirs.name) {
        edits.push((name.at.clone(), xml::escape(&theirs.value)));
    }

    // The set of samples the preset was made on, if it is another one and
    // it is here to be read.
    let mut note = None;
    if let (Some(library), Some(wanted)) = (&ours.library, &theirs.library)
        && library.value != wanted.value
        && !wanted.value.is_empty()
    {
        let tune = ours.tune.as_ref().and_then(|tune| Some((tune, tune_for(&tune.value, &wanted.value)?)));
        match tune {
            Some((tune, path)) if Path::new(&path).is_file() => {
                edits.push((library.at.clone(), xml::escape(&wanted.value)));
                edits.push((tune.at.clone(), xml::escape(&path)));
                marks.push(Mark { name: LIBRARY.to_string(), value: wanted.value.clone() });
                note = Some(format!(
                    "The preset plays another set of samples ('{}' in place of '{}'), and {LIBRARY_NOTE}.",
                    wanted.value, library.value
                ));
            }
            _ => {
                note = Some(format!(
                    "The preset was made on the set of samples '{}', which is not installed: it plays on '{}'.",
                    wanted.value, library.value
                ));
            }
        }
    }

    let component = envelope.with(&xml::replace(envelope.text, edits));
    Ok(Built { state: PluginState { component, controller: current.controller.clone() }, marks, note })
}

/// What `state` shows for the setting `name`.
pub(crate) fn shown(state: &PluginState, name: &str) -> Option<String> {
    let (_, document) = state_document(state).ok()?;
    match name {
        LIBRARY => document.library.map(|library| library.value),
        name => document.setting(name).map(|setting| setting.value.clone()),
    }
}

/// Every setting in a plugin's state, by name, with the value it has: the
/// ones the plugin shows a host as parameters, and the rest. `None` for a
/// state that is not one of these documents.
pub(crate) fn settings(state: &PluginState) -> Option<Vec<(String, String)>> {
    let (_, document) = state_document(state).ok()?;
    let mut settings: Vec<(String, String)> =
        document.settings.into_iter().map(|setting| (setting.name, setting.value)).collect();
    settings.extend(document.library.map(|library| (LIBRARY.to_string(), library.value)));
    Some(settings)
}

/// `current` with `values` in place of the values of the settings they
/// name. A name that is no setting is an error that says which names come
/// near.
pub(crate) fn with_settings(current: &PluginState, values: &[(String, String)]) -> Result<Built, String> {
    let (envelope, ours) = state_document(current)?;
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut marks = Vec::new();
    let mut note = None;
    for (name, value) in values {
        if name == LIBRARY {
            let library = ours.library.as_ref().ok_or("the plugin's state does not say which samples it plays")?;
            let tune = ours.tune.as_ref().and_then(|tune| Some((tune, tune_for(&tune.value, value)?)));
            let Some((tune, path)) = tune.filter(|(_, path)| Path::new(path).is_file()) else {
                return Err(format!(
                    "the set of samples '{value}' is not installed; the plugin plays '{}'",
                    library.value
                ));
            };
            edits.push((library.at.clone(), xml::escape(value)));
            edits.push((tune.at.clone(), xml::escape(&path)));
            note = Some(format!("It plays another set of samples now, and {LIBRARY_NOTE}."));
        } else {
            let setting = ours.setting(name).ok_or_else(|| {
                let words: Vec<String> = name.split_whitespace().map(str::to_lowercase).collect();
                let near: Vec<&str> = ours
                    .settings
                    .iter()
                    .map(|setting| setting.name.as_str())
                    .filter(|known| words.iter().any(|word| known.to_lowercase().contains(word.as_str())))
                    .take(12)
                    .collect();
                match near.is_empty() {
                    true => format!("the plugin's state has no setting '{name}'"),
                    false => format!("the plugin's state has no setting '{name}'; near it: {}", near.join(", ")),
                }
            })?;
            edits.push((setting.at.clone(), xml::escape(value)));
        }
        marks.push(Mark { name: name.clone(), value: value.clone() });
    }
    let component = envelope.with(&xml::replace(envelope.text, edits));
    Ok(Built { state: PluginState { component, controller: current.controller.clone() }, marks, note })
}

/// An instrument's document made here, in the format: `settings` and the
/// set of samples `library`, with its tuning file in `folder`.
#[cfg(test)]
pub(crate) fn document(name: &str, version: &str, library: &str, folder: &Path, settings: &[(&str, &str)]) -> String {
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\n<AMG_STATE Name=\"{name}\" Ver=\"{version}\" Author=\"Somebody\" \
         Category=\"Metal\" Type=\"Rhythm\" Lib=\"Bridge\">\n  <Chord-Path Value=\"C:\\Users\\User\\Documents\\Chords\"/>"
    );
    for (index, (name, value)) in settings.iter().enumerate() {
        text.push_str(&format!("<Parameter Name=\"{name}\" Value=\"{}\" Index=\"{index}\"/>", xml::escape(value)));
    }
    let tune = folder.join(format!("Tune_Default.{library}"));
    text.push_str(&format!("<DBName Value=\"{library}\"/><Tune-Path Value=\"{}\"/></AMG_STATE>", tune.display()));
    text
}

/// A state of one of these instruments made here: `text` in the envelope,
/// with what a plugin format's wrapper adds after it.
#[cfg(test)]
pub(crate) fn state(text: &str) -> PluginState {
    let mut component = b"VC2!".to_vec();
    component.extend((text.len() as u32).to_le_bytes());
    component.extend(text.as_bytes());
    component.extend(b"\0\0\0\0JUCEPrivateData\0\x01\x01Bypass\0");
    PluginState { component, controller: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Found;
    use crate::test_support::TestDir;

    fn plugin(stem: &str) -> PluginRef {
        PluginRef {
            uri: format!("vst3:{MAKER}416D6872"),
            id: format!("{MAKER}416D6872"),
            path: PathBuf::from(format!("plugins/{stem}.vst3/Contents/x86_64-win/{stem}.vst3")),
            name: "Ample Metal Hellrazer".into(),
            vendor: "Ample Sound".into(),
        }
    }

    const OURS: [(&str, &str); 4] =
        [("M-Master Vol", "0.9"), ("Auto Legato", "1"), ("AMP-Cabinet Mic1 Index", "Con414s"), ("Only Ours", "7")];

    #[test]
    fn presets_are_the_files_in_the_instruments_own_folder() {
        let dir = TestDir::new();
        let documents = dir.path().join("documents");
        let tune = dir.path().join("tune");
        let preset = document("9 String Metal", "4.0.0", "AMHBg", &tune, &OURS);
        dir.file("documents/Ample Sound/AMH/Presets4/9 String Metal.amhstate", preset.as_bytes());
        dir.file(
            "documents/Ample Sound/AMH/Presets4/Clean.amhstate",
            document("Clean", "4.0.0", "AMHBg", &tune, &OURS).as_bytes(),
        );
        // Not presets of it: another instrument's, a note, and a file that
        // is no document.
        dir.file("documents/Ample Sound/AME/Presets4/Other.amestate", preset.as_bytes());
        dir.file("documents/Ample Sound/AMH/Presets4/readme.txt", b"hello");
        dir.file("documents/Ample Sound/AMH/Presets4/Broken.amhstate", b"\0\0\0");

        let places = VendorPlaces { documents: vec![documents.clone()], ..VendorPlaces::default() };
        let mut found = Found::default();
        list(&plugin("AMH"), &places, &mut found);
        let names: Vec<&str> = found.presets.iter().map(|preset| preset.name.as_str()).collect();
        assert_eq!(names, ["9 String Metal", "Clean"]);
        let first = &found.presets[0];
        assert_eq!((first.category.as_str(), first.author.as_str()), ("Metal", "Somebody"));
        assert_eq!(first.tags, ["rhythm", "bridge"]);
        let folder = documents.join("Ample Sound").join("AMH").join("Presets4");
        assert_eq!(found.watch, [Watch { path: folder, extensions: vec!["amhstate".to_string()] }]);

        // Another instrument has its own folder; one with no folder, or a
        // file name that is no folder's, has no presets.
        let mut other = Found::default();
        list(&plugin("AME"), &places, &mut other);
        assert_eq!(other.presets.len(), 1);
        let mut none = Found::default();
        list(&plugin("AGM"), &places, &mut none);
        list(&plugin("My Plugin"), &places, &mut none);
        assert!(none.presets.is_empty() && none.watch.len() == 1, "{none:?}");
        assert!(is_ours("abcdef019182faeb417073644170") && !is_ours("ABCDEF019182FAEB4E445350") && !is_ours("short"));
    }

    #[test]
    fn a_presets_settings_are_laid_over_the_plugins_own() {
        let dir = TestDir::new();
        let tune = dir.path().join("tune");
        let text = document("Default", "4.1.0", "AMHBg", &tune, &OURS);
        let current = state(&text);
        // The preset: another level, hammer-ons as they were, another
        // microphone, a setting this version no longer has, and a path of
        // the machine it was saved on.
        let theirs =
            [("M-Master Vol", "0.8"), ("Auto Legato", "1.0"), ("AMP-Cabinet Mic1 Index", "Dyn<609>"), ("Gone", "3")];
        let preset = document("9 String Metal", "4.0.0", "AMHBg", Path::new("C:\\Users\\User\\Tune"), &theirs);

        let built = build(preset.as_bytes(), &current).unwrap();
        let expected = text
            .replace("Name=\"Default\"", "Name=\"9 String Metal\"")
            .replace("Value=\"0.9\"", "Value=\"0.8\"")
            .replace("Value=\"Con414s\"", "Value=\"Dyn&lt;609&gt;\"");
        assert_eq!(built.state, state(&expected), "only the values that differ are touched");
        let marks: Vec<(&str, &str)> =
            built.marks.iter().map(|mark| (mark.name.as_str(), mark.value.as_str())).collect();
        assert_eq!(marks, [("M-Master Vol", "0.8"), ("AMP-Cabinet Mic1 Index", "Dyn<609>")]);
        assert_eq!(built.note, None);
        assert_eq!(shown(&built.state, "AMP-Cabinet Mic1 Index").as_deref(), Some("Dyn<609>"));
        assert_eq!(shown(&built.state, "Only Ours").as_deref(), Some("7"), "what the preset does not name stays");
        assert_eq!(shown(&built.state, LIBRARY).as_deref(), Some("AMHBg"));
        assert_eq!(shown(&built.state, "Gone"), None);
    }

    #[test]
    fn a_preset_for_other_samples_switches_to_them_if_they_are_installed() {
        let dir = TestDir::new();
        let tune = dir.path().join("tune");
        dir.file("tune/Tune_Default.AGPFBg", b"");
        dir.file("tune/Tune_Default.AGPFNk", b"");
        let current = state(&document("Default", "4.1.0", "AGPFBg", &tune, &OURS));
        let neck = document("Jazz", "4.0.0", "AGPFNk", Path::new("elsewhere"), &OURS);
        let built = build(neck.as_bytes(), &current).unwrap();
        assert_eq!(shown(&built.state, LIBRARY).as_deref(), Some("AGPFNk"));
        let (_, read) = state_document(&built.state).unwrap();
        assert_eq!(Path::new(&read.tune.unwrap().value), tune.join("Tune_Default.AGPFNk"));
        assert_eq!(built.marks, [Mark { name: LIBRARY.into(), value: "AGPFNk".into() }]);
        let note = built.note.unwrap();
        assert!(note.contains("'AGPFNk' in place of 'AGPFBg'") && note.contains("silent or cut short"), "{note}");

        // A set that is not here: the preset's settings, on the samples
        // the plugin has.
        let both = document("Both", "4.0.0", "AGPFBt", Path::new("elsewhere"), &[("M-Master Vol", "0.5")]);
        let built = build(both.as_bytes(), &current).unwrap();
        assert_eq!(shown(&built.state, LIBRARY).as_deref(), Some("AGPFBg"));
        assert_eq!(built.marks.len(), 1);
        assert!(built.note.unwrap().contains("'AGPFBt', which is not installed: it plays on 'AGPFBg'"));
    }

    #[test]
    fn a_file_or_a_state_that_will_not_do_says_why() {
        let tune = Path::new("tune");
        let current = state(&document("Default", "4.1.0", "AMHBg", tune, &OURS));
        let wrong = |file: &str| build(file.as_bytes(), &current).unwrap_err();
        assert_eq!(wrong("RIFF"), "it is not one of Ample Sound's documents of settings");
        assert_eq!(
            wrong("<AMG_STATE Ver=\"4.0.0\"><Parameter Name=\"x\" Value=\"1\"></AMG_STATE"),
            "the end of 'AMG_STATE' is not closed"
        );
        let bass = document("Funk", "4.0.0", "AMRFg", tune, &OURS).replace("AMG_STATE", "ABG_STATE");
        assert_eq!(wrong(&bass), "it is a preset of another instrument (its settings are 'ABG_STATE')");
        // A preset from the version before is taken, as the plugin takes
        // it: all its settings are still there.
        let old = document("Old", "3.7.0", "AMHBg", tune, &OURS);
        assert!(build(old.as_bytes(), &current).is_ok());
        let foreign = document("Foreign", "4.0.0", "AMHBg", tune, &[("Something Else", "1")]);
        assert_eq!(wrong(&foreign), "none of its settings is one this plugin has");
        let mostly = [("M-Master Vol", "0.5"), ("Wah", "1"), ("Fuzz", "2"), ("Octaver", "3")];
        assert_eq!(
            wrong(&document("Mostly Foreign", "2.0.0", "AMHBg", tune, &mostly)),
            "it is not a preset of this plugin, or of a version of it this one can take (version 2.0.0; the plugin is \
             version 4.1.0 and has 1 of its 4 settings)"
        );
        // A plugin whose state is something else.
        let preset = document("Fine", "4.0.0", "AMHBg", tune, &OURS);
        let opaque = PluginState { component: b"opaque bytes".to_vec(), controller: Vec::new() };
        assert_eq!(
            build(preset.as_bytes(), &opaque).unwrap_err(),
            "the plugin's state is not the document this reader knows"
        );
        assert_eq!(settings(&opaque), None);
    }

    #[test]
    fn the_settings_in_a_state_are_read_and_set_by_name() {
        let dir = TestDir::new();
        let tune = dir.path().join("tune");
        dir.file("tune/Tune_Default.AMRPk", b"");
        let current = state(&document("Default", "4.1.0", "AMRFg", &tune, &OURS));
        let all = settings(&current).unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[1], ("Auto Legato".to_string(), "1".to_string()));
        assert_eq!(all[4], (LIBRARY.to_string(), "AMRFg".to_string()));

        let values = [("Auto Legato".to_string(), "0".to_string()), (LIBRARY.to_string(), "AMRPk".to_string())];
        let built = with_settings(&current, &values).unwrap();
        assert_eq!(shown(&built.state, "Auto Legato").as_deref(), Some("0"));
        assert_eq!(shown(&built.state, LIBRARY).as_deref(), Some("AMRPk"));
        assert_eq!(built.marks.len(), 2);
        assert!(built.note.unwrap().contains("another set of samples"));
        assert_eq!(with_settings(&current, &values[..1]).unwrap().note, None);

        let error = with_settings(&current, &[("Legato".to_string(), "0".to_string())]).unwrap_err();
        assert_eq!(error, "the plugin's state has no setting 'Legato'; near it: Auto Legato");
        let error = with_settings(&current, &[("Zebra".to_string(), "0".to_string())]).unwrap_err();
        assert_eq!(error, "the plugin's state has no setting 'Zebra'");
        let error = with_settings(&current, &[(LIBRARY.to_string(), "AMRXx".to_string())]).unwrap_err();
        assert_eq!(error, "the set of samples 'AMRXx' is not installed; the plugin plays 'AMRFg'");
    }
}
