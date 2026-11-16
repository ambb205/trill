//! A VST3 plugin's presets. The format has two kinds:
//!
//! * **Preset files** (`.vstpreset`), kept in folders the VST 3 SDK lays
//!   down for every host alike: a folder per vendor, in it a folder per
//!   plugin, below that whatever the user sorts presets into. A file says
//!   in its first bytes which plugin it is for, so finding them takes
//!   none of the plugin's code ([`preset_files`]). Loading one hands the
//!   states in it to the plugin, the way a saved project's are.
//! * **Programs**: a plugin may list presets of its own, by name, as the
//!   *program lists* of its edit controller, and has a parameter that
//!   selects one. Reading the lists means creating the plugin, which is
//!   done in the scanner process ([`read_presets`]); loading a program is
//!   setting that parameter.
//!
//! Many plugins have neither, and keep presets in a format and a browser
//! of their own that a host cannot see into.

use super::class_id::ClassId;
use super::host::HostApplication;
use super::instance::{Instance, introduce};
use super::module::Module;
use super::{paths, state, text};
use crate::presets::store::PluginRef;
use crate::presets::{Locator, NAME_LIMIT, Sink, TAG_LIMIT, Watch, tidy, walk};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use trill_core::presets::PresetInfo;
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::kIsProgramChange;
use vst3::Steinberg::Vst::{
    IEditController, IEditControllerTrait, IUnitInfo, IUnitInfoTrait, ParamID, ParameterInfo, ProgramListID,
    ProgramListInfo, String128, UnitID, UnitInfo, kNoProgramListId,
};
use vst3::Steinberg::kResultOk;
use vst3::{ComPtr, ComWrapper};

/// The extension of a VST3 preset file, without the dot.
const PRESET_EXTENSION: &str = "vstpreset";
/// Upper bounds on what is read from a plugin, so that a plugin reporting
/// nonsense counts cannot keep the reader busy for minutes.
const MAX_LISTS: i32 = 256;
const MAX_PROGRAMS: i32 = 1 << 16;
const MAX_PARAMS: i32 = 1 << 20;
const MAX_UNITS: i32 = 1 << 12;
/// How many programs are handed on at a time.
const BATCH: usize = 64;
/// The attributes of a program, and of a preset file's meta information,
/// that say what kind of sound it is. Each holds words separated by `|`.
const SOUND_ATTRIBUTES: [&str; 3] = ["MusicalInstrument", "MusicalStyle", "MusicalCharacter"];

/// The folders VST3 preset files are kept in on this system, the user's
/// own first: in each, a plugin's presets are in `<vendor>/<plugin name>`.
pub fn standard_preset_paths() -> Vec<PathBuf> {
    paths::preset_paths()
}

/// What a vendor's or a plugin's name is as the name of a folder: the
/// characters no file name may hold are replaced.
fn folder_name(name: &str) -> String {
    let replaced: String =
        name.trim().chars().map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    replaced.trim_end_matches(['.', ' ']).to_string()
}

/// Finds the preset files of `plugin` in `folders`, without running any of
/// the plugin's code: a file belongs to the plugin if it lies in the
/// plugin's folder and names the plugin's class.
pub(crate) fn preset_files(plugin: &PluginRef, folders: &[PathBuf], sink: &mut dyn Sink) {
    let Some(class) = ClassId::parse(&plugin.id) else { return };
    let name = folder_name(&plugin.name);
    if name.is_empty() {
        return;
    }
    let extensions = vec![PRESET_EXTENSION.to_string()];
    for (index, root) in folders.iter().enumerate() {
        let vendor = folder_name(&plugin.vendor);
        let folder = if vendor.is_empty() { root.join(&name) } else { root.join(vendor).join(&name) };
        // Watched whether or not it is there: the first preset the user
        // saves creates it.
        sink.watch(Watch { path: folder.clone(), extensions: extensions.clone() });
        // The first folder is the user's own; the others are where
        // vendors install presets.
        let origin = if index == 0 { "user" } else { "factory" };
        let mut found = Vec::new();
        for file in walk::files(&folder, &extensions) {
            let Some(glance) = std::fs::File::open(&file).ok().and_then(|mut file| state::glance(&mut file)) else {
                log::debug!("'{}' is not a VST3 preset file; skipping it", file.display());
                continue;
            };
            if glance.class != class {
                continue;
            }
            let mut tags = sound_tags(attributes(&glance.info).iter().map(|(id, value)| (id.as_str(), value.as_str())));
            tags.push(origin.to_string());
            let name = file.file_stem().map(|stem| tidy(&stem.to_string_lossy(), NAME_LIMIT)).unwrap_or_default();
            found.push(PresetInfo {
                name,
                category: walk::category_below(&folder, &file),
                tags,
                locator: Locator::Vst3File { path: file }.to_text(),
                ..PresetInfo::default()
            });
        }
        sink.presets(found);
    }
}

/// The words of the attributes that say what kind of sound a preset is,
/// as tags.
fn sound_tags<'a>(attributes: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for (id, value) in attributes {
        if !SOUND_ATTRIBUTES.contains(&id) {
            continue;
        }
        for word in value.split('|').map(|word| tidy(word, NAME_LIMIT)) {
            if !word.is_empty() && !tags.contains(&word) && tags.len() < TAG_LIMIT {
                tags.push(word);
            }
        }
    }
    tags
}

/// The attributes of a preset file's meta information, which is a small
/// XML document: `<Attribute id="MusicalStyle" value="Pop|Rock" .../>`.
/// Read by hand rather than by an XML parser: all that is wanted of it is
/// a few words, and whatever cannot be made out is no loss.
fn attributes(xml: &[u8]) -> Vec<(String, String)> {
    let xml = String::from_utf8_lossy(xml);
    let mut found = Vec::new();
    for element in xml.split('<').filter_map(|element| element.strip_prefix("Attribute")) {
        let element = element.split('>').next().unwrap_or_default();
        let value_of = |name: &str| {
            let after = element.split_once(&format!("{name}=\""))?.1;
            // The name must be the attribute's own, not the end of
            // another's.
            let before = element.len() - after.len() - name.len() - 2;
            if !element[..before].ends_with(char::is_whitespace) {
                return None;
            }
            Some(unescape(after.split('"').next()?))
        };
        if let (Some(id), Some(value)) = (value_of("id"), value_of("value")) {
            found.push((id, value));
        }
    }
    found
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Loads the VST3 plugin at `path` into the current process, creates the
/// plugin of class `class` and lists the programs of its program lists.
///
/// See [`crate::presets::read_in_this_process`] for why the DAW does this
/// in a child process.
pub(crate) fn read_presets(path: &Path, class: &str, sink: &mut dyn Sink) -> Result<(), String> {
    let class = ClassId::parse(class).ok_or_else(|| format!("'{class}' is not a VST3 class id"))?;
    let host = HostApplication::new();
    let module = introduce(Module::load(&paths::resolve(path)?)?, &host);
    programs(&module, &host, class, sink)
}

/// A parameter that selects a program.
struct Selector {
    param: ParamID,
    unit: UnitID,
    /// How many steps the parameter has above its lowest; 0 if it does
    /// not say.
    steps: u32,
}

/// Creates the plugin of class `class` far enough to ask its edit
/// controller for its program lists, and lists their programs. A plugin
/// without program lists has none to list, which is not an error.
pub(super) fn programs(
    module: &Module,
    host: &ComWrapper<HostApplication>,
    class: ClassId,
    sink: &mut dyn Sink,
) -> Result<(), String> {
    let mut instance = Instance::new(module, host, class, "the plugin")?;
    instance.attach_controller(module, host);
    let Some(controller) = instance.controller.as_ref().map(|controller| controller.edit.clone()) else {
        return Ok(());
    };
    let Some(units) = controller.cast::<IUnitInfo>() else { return Ok(()) };
    let selectors = selectors(&controller);
    if selectors.is_empty() {
        return Ok(());
    }
    // SAFETY: `units` is an interface of the live edit controller, used on
    // the thread that created it; every structure passed is a local the
    // plugin fills in.
    unsafe {
        // Which program list each unit shows.
        let lists_of_units: HashMap<UnitID, ProgramListID> = (0..units.getUnitCount().clamp(0, MAX_UNITS))
            .filter_map(|index| {
                let mut info = UnitInfo { id: 0, parentUnitId: 0, name: [0; 128], programListId: kNoProgramListId };
                (units.getUnitInfo(index, &mut info) == kResultOk).then_some((info.id, info.programListId))
            })
            .collect();

        let count = units.getProgramListCount().clamp(0, MAX_LISTS);
        for list_index in 0..count {
            let mut list = ProgramListInfo { id: 0, name: [0; 128], programCount: 0 };
            if units.getProgramListInfo(list_index, &mut list) != kResultOk {
                continue;
            }
            // A list is chosen from by the program parameter of a unit
            // that shows it. A plugin with one list and one such
            // parameter means the two for each other, whatever it says
            // about units.
            let selector = selectors
                .iter()
                .find(|selector| lists_of_units.get(&selector.unit) == Some(&list.id))
                .or_else(|| selectors.first().filter(|_| count == 1 && selectors.len() == 1));
            let Some(selector) = selector else { continue };

            let programs = list.programCount.clamp(0, MAX_PROGRAMS) as u32;
            // The steps of the parameter are the programs it can reach.
            let steps = if selector.steps == 0 { programs.saturating_sub(1) } else { selector.steps };
            let category = tidy(&text::from_utf16(&list.name), NAME_LIMIT);
            let mut found = Vec::new();
            for index in 0..programs.min(steps + 1) {
                let mut name: String128 = [0; 128];
                let named = units.getProgramName(list.id, index as i32, &mut name) == kResultOk;
                let name = if named { tidy(&text::from_utf16(&name), NAME_LIMIT) } else { String::new() };
                let attributes: Vec<(&str, String)> = SOUND_ATTRIBUTES
                    .iter()
                    .filter_map(|attribute| {
                        let id = std::ffi::CString::new(*attribute).ok()?;
                        let mut value: String128 = [0; 128];
                        let known = units.getProgramInfo(list.id, index as i32, id.as_ptr(), &mut value) == kResultOk;
                        known.then(|| (*attribute, text::from_utf16(&value)))
                    })
                    .collect();
                found.push(PresetInfo {
                    name: if name.is_empty() { format!("Program {}", index + 1) } else { name },
                    category: category.clone(),
                    tags: sound_tags(attributes.iter().map(|(id, value)| (*id, value.as_str()))),
                    locator: Locator::Vst3Program { param: selector.param, index, steps }.to_text(),
                    ..PresetInfo::default()
                });
            }
            // Slots that are only numbered are the MIDI program numbers a
            // plugin answers to, with nothing behind them the plugin could
            // name: no presets to choose a sound by.
            if only_numbered(found.iter().map(|program| program.name.as_str())) {
                continue;
            }
            for batch in found.chunks(BATCH) {
                sink.presets(batch.to_vec());
            }
        }
    }
    Ok(())
}

/// Whether the names of a program list say nothing but each program's
/// number: `Prog 1`, `Prog 2`, ... or `000`, `001`, ..., one word in front
/// of a count that runs through the list. Two programs at least: one
/// program called `Program 1` is one program.
fn only_numbered<'a>(names: impl Iterator<Item = &'a str>) -> bool {
    let mut expected: Option<(String, u64)> = None;
    let mut count = 0;
    for name in names {
        let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
        let Ok(number) = name[stem.len()..].parse::<u64>() else { return false };
        let stem = stem.trim().to_lowercase();
        match &expected {
            Some((first, next)) if *first != stem || *next != number => return false,
            _ => expected = Some((stem, number + 1)),
        }
        count += 1;
    }
    count >= 2
}

/// The plugin's parameters that select a program.
fn selectors(controller: &ComPtr<IEditController>) -> Vec<Selector> {
    // SAFETY: `controller` is a live edit controller, used on the thread
    // that created it; `info` is a local the plugin fills in.
    unsafe {
        (0..controller.getParameterCount().clamp(0, MAX_PARAMS))
            .filter_map(|index| {
                let mut info = ParameterInfo {
                    id: 0,
                    title: [0; 128],
                    shortTitle: [0; 128],
                    units: [0; 128],
                    stepCount: 0,
                    defaultNormalizedValue: 0.0,
                    unitId: 0,
                    flags: 0,
                };
                let described = controller.getParameterInfo(index, &mut info) == kResultOk;
                (described && info.flags & kIsProgramChange != 0).then(|| Selector {
                    param: info.id,
                    unit: info.unitId,
                    steps: u32::try_from(info.stepCount).unwrap_or(0),
                })
            })
            .collect()
    }
}

/// The normalized value of a program parameter that selects program
/// `index` of `steps + 1`.
pub(super) fn program_value(index: u32, steps: u32) -> f64 {
    if steps == 0 { 0.0 } else { f64::from(index.min(steps)) / f64::from(steps) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Found;
    use crate::test_support::TestDir;
    use crate::vst3::state::PluginState;

    const CLASS: &str = "ABCDEF019182FAEB5776737957767331";
    const OTHER: &str = "00000000000000000000000000000001";

    fn plugin(name: &str, vendor: &str) -> PluginRef {
        PluginRef {
            uri: format!("vst3:{CLASS}"),
            id: CLASS.into(),
            path: PathBuf::from("synth.vst3"),
            name: name.into(),
            vendor: vendor.into(),
        }
    }

    /// A preset file for the plugin of class `class`, as this host writes
    /// a plugin's state.
    fn preset_file(class: &str) -> Vec<u8> {
        let state = PluginState { component: b"component".to_vec(), controller: Vec::new() };
        state::encode(ClassId::parse(class).unwrap(), &state)
    }

    fn found(plugin: &PluginRef, folders: &[PathBuf]) -> Found {
        let mut found = Found::default();
        preset_files(plugin, folders, &mut found);
        found
    }

    #[test]
    fn preset_files_are_found_in_the_plugins_folder_of_each_place() {
        let dir = TestDir::new();
        let (user, shared) = (dir.path().join("user"), dir.path().join("shared"));
        dir.file("user/Acme/Synth/Warm.vstpreset", &preset_file(CLASS));
        dir.file("user/Acme/Synth/Pads/Soft/Air.VSTPRESET", &preset_file(CLASS));
        dir.file("shared/Acme/Synth/Bass/Deep.vstpreset", &preset_file(CLASS));
        // Not this plugin's: another class, another plugin's folder, not
        // a preset file at all.
        dir.file("user/Acme/Synth/Theirs.vstpreset", &preset_file(OTHER));
        dir.file("user/Acme/Other Synth/Elsewhere.vstpreset", &preset_file(CLASS));
        dir.file("user/Acme/Synth/notes.txt", b"hello");
        dir.file("user/Acme/Synth/Broken.vstpreset", b"VST3 but not really");

        let found = found(&plugin("Synth", "Acme"), &[user.clone(), shared.clone()]);
        let listed: Vec<(&str, &str, &[String])> = found
            .presets
            .iter()
            .map(|preset| (preset.name.as_str(), preset.category.as_str(), preset.tags.as_slice()))
            .collect();
        assert_eq!(
            listed,
            [
                ("Warm", "", ["user".to_string()].as_slice()),
                ("Air", "Pads/Soft", ["user".to_string()].as_slice()),
                ("Deep", "Bass", ["factory".to_string()].as_slice()),
            ]
        );
        let warm = Locator::Vst3File { path: user.join("Acme").join("Synth").join("Warm.vstpreset") };
        assert_eq!(Locator::parse(&found.presets[0].locator), Some(warm));
        // Both places are watched, so that a preset saved later is seen.
        let watched: Vec<&Path> = found.watch.iter().map(|watch| watch.path.as_path()).collect();
        assert_eq!(watched, [user.join("Acme").join("Synth"), shared.join("Acme").join("Synth")]);
        assert_eq!(found.watch[0].extensions, ["vstpreset"]);
    }

    #[test]
    fn a_plugins_folder_is_named_after_it_as_far_as_a_folder_can_be() {
        let dir = TestDir::new();
        dir.file("presets/A_B Audio/Synth_ One_/x.vstpreset", &preset_file(CLASS));
        dir.file("presets/Lonely/y.vstpreset", &preset_file(CLASS));
        let folders = [dir.path().join("presets")];
        assert_eq!(found(&plugin("Synth: One?", "A/B Audio"), &folders).presets.len(), 1);
        // A plugin that names no vendor has its folder directly in place.
        assert_eq!(found(&plugin("Lonely", ""), &folders).presets[0].name, "y");
        // No name, no folder; a missing place is no presets, and still
        // watched.
        assert!(found(&plugin("", "Acme"), &folders).watch.is_empty());
        let missing = found(&plugin("Synth", "Acme"), &[dir.path().join("nowhere")]);
        assert!(missing.presets.is_empty());
        assert_eq!(missing.watch.len(), 1);

        assert_eq!(folder_name("  Acme. "), "Acme");
        assert_eq!(folder_name(r#"a\b/c:d*e?f"g<h>i|j"#), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn a_preset_files_meta_information_becomes_tags() {
        let xml = br#"<?xml version="1.0" encoding="utf-8"?>
<MetaInfo>
  <Attribute id="MediaType" value="VstPreset" type="string" flags="writeProtected"/>
  <Attribute id="MusicalInstrument" value="Synth Pad|Analog" type="string" flags=""/>
  <Attribute id="MusicalStyle" value="Ambient &amp; Chill|Analog" type="string"/>
  <Attribute id="PlugInName" value="Synth" type="string"/>
  <Attribute valid="no" value="nothing"/>
  <Attribute id="MusicalCharacter" value="" type="string"/>
</MetaInfo>"#;
        let attributes = attributes(xml);
        assert_eq!(attributes.len(), 5, "{attributes:?}");
        assert_eq!(attributes[0], ("MediaType".to_string(), "VstPreset".to_string()));
        let tags = sound_tags(attributes.iter().map(|(id, value)| (id.as_str(), value.as_str())));
        assert_eq!(tags, ["Synth Pad", "Analog", "Ambient & Chill"]);
        assert!(super::attributes(b"").is_empty());
        assert!(super::attributes(b"<Attribute id=\"cut off").is_empty());
        assert!(super::attributes(&[0xff, 0xfe, b'<', 0]).is_empty());
    }

    #[test]
    fn a_list_of_numbered_slots_is_no_list_of_presets() {
        let numbered = |names: &[&str]| only_numbered(names.iter().copied());
        // What a synth that only answers MIDI program changes lists.
        let slots: Vec<String> = (1..=128).map(|n| format!("Prog {n}")).collect();
        assert!(only_numbered(slots.iter().map(String::as_str)));
        assert!(numbered(&["Program 0", "Program 1", "Program 2"]));
        assert!(numbered(&["000", "001", "002"]));
        assert!(numbered(&["preset 7", "Preset 8"]));
        // Names are names, also when some end in a number.
        assert!(!numbered(&["Quiet", "Loud", "Program 3"]));
        assert!(!numbered(&["Pad 1", "Pad 2", "Bass 1"]));
        assert!(!numbered(&["Strings 1", "Strings 3"]), "not a count");
        assert!(!numbered(&["Program 1"]), "one program is a program");
        assert!(!numbered(&[]));
        assert!(!numbered(&["99999999999999999999999", "100000000000000000000000"]));
    }

    #[test]
    fn a_program_is_selected_by_its_place_among_the_steps() {
        assert_eq!(program_value(0, 127), 0.0);
        assert_eq!(program_value(127, 127), 1.0);
        assert_eq!(program_value(1, 2), 0.5);
        assert_eq!(program_value(9, 2), 1.0, "a program past the last selects the last");
        assert_eq!(program_value(0, 0), 0.0);
    }
}
