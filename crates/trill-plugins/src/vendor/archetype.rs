//! Neural DSP's Archetype amplifiers: the preset files each keeps in its
//! folder of the machine's shared data.
//!
//! An Archetype's state is an XML document in JUCE's envelope
//! ([`super::juce`]): a tree called `appModel`, with the amplifier that is
//! chosen and, under `subModels`, every section's settings as attributes
//! (`gateThreshold="-80"`, `doublerSpread="6"`). A preset file holds the
//! same tree in JUCE's binary form, under a root that names the product,
//! the preset and its version. (The files are called `.xml`; they are
//! not.)
//!
//! So a preset is loaded by reading its tree and laying it over the
//! document the plugin has now: every value the preset names is set, and
//! what it does not name (which controls are locked against presets, the
//! plugin's version) stays. All the Archetypes are built alike, so one
//! reader serves them; which product a file belongs to is where it lies.

use super::juce::{self, Envelope};
use super::xml::Element;
use super::{Built, Mark, VendorPlaces, read_start, same_value};
use crate::presets::store::PluginRef;
use crate::presets::{Locator, NAME_LIMIT, Sink, Watch, tidy, walk};
use crate::vst3::state::PluginState;
use std::path::PathBuf;
use trill_core::presets::{CATEGORY_SEPARATOR, PresetInfo};

/// What the class ids of Neural DSP's plugins begin with: JUCE's own
/// prefix, then the maker's four letters, `NDSP`.
const MAKER: &str = "ABCDEF019182FAEB4E445350";
const EXTENSION: &str = "xml";
/// What separates the steps of the way to a setting from each other, and
/// the way from the setting's name.
const STEP: char = '/';
const SETTING: char = '@';
/// The folder of presets that players other than the maker have made:
/// each has a folder of their own in it.
const ARTISTS: &str = "Artists";

pub(crate) fn is_ours(id: &str) -> bool {
    id.get(..MAKER.len()).is_some_and(|start| start.eq_ignore_ascii_case(MAKER))
}

/// Where a product's presets are: a folder named as the product is, under
/// `Neural DSP` in the machine's shared data.
fn folders(plugin: &PluginRef, places: &VendorPlaces) -> Vec<PathBuf> {
    let name = plugin.name.trim();
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', ':']) {
        return Vec::new();
    }
    places.shared_data.iter().map(|data| data.join("Neural DSP").join(name)).collect()
}

/// Whether a file begins the way a tree in JUCE's binary form does: with
/// a name, and not with the `<` of the text its extension promises.
fn begins_a_tree(start: &[u8]) -> bool {
    let name = start.split(|byte| *byte == 0).next().unwrap_or_default();
    name.len() < start.len()
        && !name.is_empty()
        && name.iter().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(byte))
}

/// Lists the preset files in a product's folders: each by its file's
/// name, under the folders it lies in, which say whose it is.
pub(crate) fn list(plugin: &PluginRef, places: &VendorPlaces, sink: &mut dyn Sink) {
    let extensions = vec![EXTENSION.to_string()];
    for folder in folders(plugin, places) {
        sink.watch(Watch { path: folder.clone(), extensions: extensions.clone() });
        let mut batch = Vec::new();
        for file in walk::files(&folder, &extensions) {
            if !read_start(&file, 64).is_ok_and(|start| begins_a_tree(&start)) {
                continue;
            }
            let stem = file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
            let category = walk::category_below(&folder, &file);
            // Whose preset it is: the artist's, or the bank's maker.
            let mut parts = category.split(CATEGORY_SEPARATOR);
            let author = match (parts.next(), parts.next()) {
                (Some(ARTISTS), Some(artist)) => artist,
                (Some("User") | None, _) => "",
                (Some(bank), _) => bank,
            };
            batch.push(PresetInfo {
                name: tidy(&stem, NAME_LIMIT),
                author: author.to_string(),
                locator: Locator::Vendor { format: super::Format::Archetype, path: file, key: None }.to_text(),
                category,
                ..PresetInfo::default()
            });
        }
        sink.presets(batch);
    }
}

/// Lays `theirs` over `ours`: every setting of theirs is set in ours, and
/// what theirs has below it is laid over what ours has there. `way` is
/// how `ours` is reached from the top; each setting that changes is noted
/// with the way to it.
fn lay_over(ours: &mut Element, theirs: &Element, way: &str, marks: &mut Vec<Mark>, known: &mut (usize, usize)) {
    for (name, value) in &theirs.attributes {
        known.1 += 1;
        match ours.attribute(name) {
            Some(old) => {
                known.0 += 1;
                if !same_value(old, value) {
                    marks.push(Mark { name: format!("{way}{SETTING}{name}"), value: value.clone() });
                }
            }
            None => marks.push(Mark { name: format!("{way}{SETTING}{name}"), value: value.clone() }),
        }
        ours.set(name, value);
    }
    let mut seen: Vec<&str> = Vec::new();
    for child in &theirs.children {
        if seen.contains(&child.name.as_str()) {
            continue;
        }
        seen.push(&child.name);
        let twins = theirs.children.iter().filter(|other| other.name == child.name).count();
        let place = ours.children.iter().position(|other| other.name == child.name);
        match (twins, place) {
            // One of a kind on both sides: the same thing, a level down.
            (1, Some(place)) if ours.children.iter().filter(|other| other.name == child.name).count() == 1 => {
                let way = format!("{way}{STEP}{}", child.name);
                lay_over(&mut ours.children[place], child, &way, marks, known);
            }
            // A list: theirs in place of ours, where ours had it.
            (_, place) => {
                let at = place.unwrap_or(ours.children.len());
                ours.children.retain(|other| other.name != child.name);
                let at = at.min(ours.children.len());
                let theirs = theirs.children.iter().filter(|other| other.name == child.name).cloned();
                ours.children.splice(at..at, theirs);
            }
        }
    }
}

/// The document a plugin's state is, and the envelope it is in.
fn state_document(state: &PluginState) -> Result<(Envelope<'_>, Element), String> {
    let envelope =
        Envelope::open(&state.component).ok_or("the plugin's state is not the document this reader knows")?;
    let document =
        Element::parse(envelope.text).map_err(|reason| format!("the plugin's state cannot be read: {reason}"))?;
    Ok((envelope, document))
}

/// The state that makes the plugin the preset in `file`: the document of
/// `current`, the plugin's state now, with the preset's tree laid over
/// it.
pub(crate) fn build(file: &[u8], current: &PluginState) -> Result<Built, String> {
    let (envelope, mut ours) = state_document(current)?;
    let preset = juce::tree(file)?;
    // The preset's root is about the file; the settings are the tree in
    // it that is called what the state's document is.
    let theirs = if preset.name == ours.name { Some(&preset) } else { preset.child(&ours.name) };
    let theirs =
        theirs.ok_or_else(|| format!("it is not a preset of this plugin (it has no '{}' in it)", ours.name))?;

    let mut marks = Vec::new();
    let mut known = (0, 0);
    let top = ours.name.clone();
    lay_over(&mut ours, theirs, &top, &mut marks, &mut known);
    // Another product's preset is the same kind of tree with other
    // settings in it.
    if known.0 * 2 < known.1 {
        return Err(format!(
            "it is not a preset of this plugin (the plugin has {} of its {} settings)",
            known.0, known.1
        ));
    }
    let component = envelope.with(&ours.to_document());
    Ok(Built { state: PluginState { component, controller: current.controller.clone() }, marks, note: None })
}

/// What `state` shows for the setting `name`, which is the way to it:
/// `appModel/subModels/parameters@gateThreshold`.
pub(crate) fn shown(state: &PluginState, name: &str) -> Option<String> {
    let (_, document) = state_document(state).ok()?;
    let (way, setting) = name.rsplit_once(SETTING)?;
    let mut steps = way.split(STEP);
    let mut at = Some(&document).filter(|root| Some(root.name.as_str()) == steps.next())?;
    for step in steps {
        at = at.child(step)?;
    }
    at.attribute(setting).map(str::to_string)
}

/// A state of one of these plugins made here: a document with the
/// `settings` of its one section, in the envelope.
#[cfg(test)]
pub(crate) fn state(settings: &[(&str, &str)]) -> PluginState {
    let attributes: Vec<String> = settings.iter().map(|(name, value)| format!("{name}=\"{value}\"")).collect();
    let text = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?> <appModel selectedAmp=\"2\" pluginVersion=\"1.1.0\"><subModels>\
         <parameters {}><subModels/></parameters><midiMap><listElements><map cc=\"1\"/></listElements></midiMap>\
         </subModels></appModel>",
        attributes.join(" ")
    );
    let mut component = b"VC2!".to_vec();
    component.extend((text.len() as u32).to_le_bytes());
    component.extend(text.as_bytes());
    component.extend(b"\0\0\0JUCEPrivateData\0");
    PluginState { component, controller: Vec::new() }
}

/// A preset file made here, in the format: the amplifier it chooses and
/// the `settings` of its one section.
#[cfg(test)]
pub(crate) fn preset_file(name: &str, amp: &str, settings: &[(&str, &str)]) -> Vec<u8> {
    let element = |name: &str, attributes: &[(&str, &str)], children: Vec<Element>| Element {
        name: name.to_string(),
        attributes: attributes.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect(),
        children,
    };
    let maps = vec![element("map", &[("cc", "7")], vec![]), element("map", &[("cc", "11")], vec![])];
    let models = vec![
        element("parameters", settings, vec![element("subModels", &[], vec![])]),
        element("midiMap", &[], vec![element("listElements", &[], maps)]),
    ];
    let model = element("appModel", &[("selectedAmp", amp)], vec![element("subModels", &[], models)]);
    juce::tree_bytes(&element("amp-x", &[("version", "0.4.0"), ("name", name)], vec![model]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Found;
    use crate::test_support::TestDir;

    fn plugin(name: &str) -> PluginRef {
        PluginRef {
            uri: format!("vst3:{MAKER}4E544858"),
            id: format!("{MAKER}4E544858"),
            path: PathBuf::from("Archetype.vst3"),
            name: name.into(),
            vendor: "Neural DSP".into(),
        }
    }

    const OURS: [(&str, &str); 4] =
        [("gateThreshold", "-80"), ("doublerActive", "false"), ("inputGain", "0"), ("gateLocked", "false")];

    #[test]
    fn presets_are_the_files_in_the_products_folder_under_whose_they_are() {
        let dir = TestDir::new();
        let file = preset_file("x", "1", &[]);
        let folder = "data/Neural DSP/Archetype Test X";
        dir.file(&format!("{folder}/Default.xml"), &file);
        dir.file(&format!("{folder}/Artists/Somebody (Some Band)/80s Lead.xml"), &file);
        dir.file(&format!("{folder}/Neural DSP/Clean.xml"), &file);
        dir.file(&format!("{folder}/User/Mine.xml"), &file);
        // Not presets: text that really is XML, and another product's.
        dir.file(&format!("{folder}/User/settings.xml"), b"<?xml version=\"1.0\"?><settings/>");
        dir.file(&format!("{folder}/User/empty.xml"), b"");
        dir.file("data/Neural DSP/Archetype Other X/Theirs.xml", &file);

        let places = VendorPlaces { shared_data: vec![dir.path().join("data")], ..VendorPlaces::default() };
        let mut found = Found::default();
        list(&plugin("Archetype Test X"), &places, &mut found);
        let listed: Vec<(String, &str)> =
            found.presets.iter().map(|preset| (preset.path(), preset.author.as_str())).collect();
        assert_eq!(
            listed,
            [
                ("Default".to_string(), ""),
                ("Artists/Somebody (Some Band)/80s Lead".to_string(), "Somebody (Some Band)"),
                ("Neural DSP/Clean".to_string(), "Neural DSP"),
                ("User/Mine".to_string(), ""),
            ]
        );
        assert_eq!(found.watch, [Watch { path: dir.path().join(folder), extensions: vec!["xml".to_string()] }]);
        // A product without a folder, or with a name that is no folder's.
        let mut none = Found::default();
        list(&plugin("Archetype Missing X"), &places, &mut none);
        list(&plugin("../Evil"), &places, &mut none);
        list(&plugin(""), &places, &mut none);
        assert!(none.presets.is_empty() && none.watch.len() == 1, "{none:?}");
        assert!(is_ours("abcdef019182faeb4e4453504e544858") && !is_ours("ABCDEF019182FAEB41707364") && !is_ours(""));
    }

    #[test]
    fn a_presets_tree_is_laid_over_the_plugins_document() {
        let current = state(&OURS);
        let theirs = [("gateThreshold", "-62.5"), ("doublerActive", "true"), ("inputGain", "0.0"), ("newKnob", "a<b")];
        let built = build(&preset_file("80s Lead", "0", &theirs), &current).unwrap();
        let (envelope, document) = state_document(&built.state).unwrap();
        assert_eq!(envelope.tail, b"\0\0\0JUCEPrivateData\0", "what follows the document is as it was");

        // The preset's values, the plugin's own where the preset has none,
        // and a list replaced whole.
        assert_eq!(
            (document.attribute("selectedAmp"), document.attribute("pluginVersion")),
            (Some("0"), Some("1.1.0"))
        );
        let parameters = document.child("subModels").unwrap().child("parameters").unwrap();
        let values: Vec<(&str, &str)> =
            parameters.attributes.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
        assert_eq!(
            values,
            [
                ("gateThreshold", "-62.5"),
                ("doublerActive", "true"),
                ("inputGain", "0.0"),
                ("gateLocked", "false"),
                ("newKnob", "a<b")
            ]
        );
        let maps =
            &document.child("subModels").unwrap().child("midiMap").unwrap().child("listElements").unwrap().children;
        assert_eq!(maps.iter().map(|map| map.attribute("cc").unwrap()).collect::<Vec<_>>(), ["7", "11"]);

        // What changed is what the state is to show: not the gain, which
        // is the same number written another way.
        let marks: Vec<(&str, &str)> =
            built.marks.iter().map(|mark| (mark.name.as_str(), mark.value.as_str())).collect();
        assert_eq!(
            marks,
            [
                ("appModel@selectedAmp", "0"),
                ("appModel/subModels/parameters@gateThreshold", "-62.5"),
                ("appModel/subModels/parameters@doublerActive", "true"),
                ("appModel/subModels/parameters@newKnob", "a<b"),
            ]
        );
        for mark in &built.marks {
            assert_eq!(shown(&built.state, &mark.name).as_deref(), Some(mark.value.as_str()), "{}", mark.name);
        }
        assert_eq!(shown(&current, "appModel/subModels/parameters@gateThreshold").as_deref(), Some("-80"));
        assert_eq!(shown(&current, "appModel/subModels/nothing@gateThreshold"), None);
        assert_eq!(shown(&current, "other/subModels/parameters@gateThreshold"), None);
        assert_eq!(built.note, None);
    }

    #[test]
    fn a_file_or_a_state_that_will_not_do_says_why() {
        let current = state(&OURS);
        let wrong = |file: &[u8]| build(file, &current).unwrap_err();
        assert_eq!(wrong(b"<?xml version=\"1.0\"?><appModel/>"), "it ends in the middle of its settings");
        assert_eq!(wrong(b""), "it ends in the middle of its settings");
        // A tree of another kind, and another product's preset: the same
        // kind of tree with other settings in it.
        let other = juce::tree_bytes(&Element { name: "something".into(), ..Element::default() });
        assert_eq!(wrong(&other), "it is not a preset of this plugin (it has no 'appModel' in it)");
        let foreign = preset_file("Theirs", "1", &[("wahPosition", "1"), ("wahActive", "true"), ("octaver", "2")]);
        assert_eq!(wrong(&foreign), "it is not a preset of this plugin (the plugin has 1 of its 4 settings)");
        let opaque = PluginState { component: b"opaque".to_vec(), controller: Vec::new() };
        let preset = preset_file("Fine", "1", &OURS);
        assert_eq!(build(&preset, &opaque).unwrap_err(), "the plugin's state is not the document this reader knows");
        assert_eq!(shown(&opaque, "appModel@selectedAmp"), None);
    }
}
