//! Serum 2's presets: the `.SerumPreset` files in its presets folder.
//!
//! A preset file is one of Xfer's containers ([`super::xfer`]) holding a
//! map of every setting of a patch. The plugin's state is two more of
//! them, one for its processor and one for its edit controller, and each
//! is the same map, told which half it is for: each half reads the
//! settings it knows. So a preset is loaded by packing its map twice,
//! under a header that names the half, and handing the plugin both.

use super::{Built, Mark, VendorPlaces, cbor, read_start, xfer};
use crate::presets::store::PluginRef;
use crate::presets::{DESCRIPTION_LIMIT, Locator, NAME_LIMIT, Sink, TAG_LIMIT, Watch, tidy, walk};
use crate::vst3::state::PluginState;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use trill_core::presets::PresetInfo;

/// Serum 2's processor class.
pub(crate) const CLASS: &str = "56534558667350736572756D20320000";
const PRODUCT: &str = "Serum2";
const FILE_TYPE: &str = "SerumPreset";
pub(crate) const EXTENSION: &str = "serumpreset";
/// The most of a file's own words about itself that a refusal repeats.
const SAID_LIMIT: usize = 40;
/// The two halves of the plugin, as its states name them.
const HALVES: [&str; 2] = ["processor", "controller"];
/// What a state's header shows of the preset it came from.
const NAME_FIELD: &str = "presetName";

/// Where Serum 2 keeps its presets: `Xfer/Serum 2 Presets/Presets` in the
/// user's documents, in banks and folders (`Factory/Pad`, `User`).
fn folders(places: &VendorPlaces) -> Vec<PathBuf> {
    places.documents.iter().map(|documents| documents.join("Xfer").join("Serum 2 Presets").join("Presets")).collect()
}

/// Lists the presets in Serum 2's folders. Only the header of each file
/// is read: its name, who made it, what they say of it, and its tags.
pub(crate) fn list(_plugin: &PluginRef, places: &VendorPlaces, sink: &mut dyn Sink) {
    let extensions = vec![EXTENSION.to_string()];
    for folder in folders(places) {
        // Watched whether or not it is there: it may appear.
        sink.watch(Watch { path: folder.clone(), extensions: extensions.clone() });
        let mut batch = Vec::new();
        for file in walk::files(&folder, &extensions) {
            let Ok(mut preset) = file_preset(&file) else { continue };
            preset.category = walk::category_below(&folder, &file);
            batch.push(preset);
        }
        sink.presets(batch);
    }
}

/// The preset in the file at `file`, wherever that lies, by what the
/// file's header says of it: its name, who made it, what they say of it,
/// and its tags. An error says why the file is not a preset of Serum 2's,
/// as the end of a sentence about it.
pub(crate) fn file_preset(file: &Path) -> Result<PresetInfo, String> {
    let start = read_start(file, xfer::header_room()).map_err(|error| format!("it cannot be read ({error})"))?;
    let (header, _) = xfer::header(&start)?;
    let (product, kind) = (xfer::field(&header, "product"), xfer::field(&header, "fileType"));
    if product != PRODUCT || kind != FILE_TYPE {
        return Err(not_ours(kind, product));
    }
    let stem = file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let named = tidy(xfer::field(&header, NAME_FIELD), NAME_LIMIT);
    let tags = header.get("tags").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    Ok(PresetInfo {
        name: if named.is_empty() { tidy(&stem, NAME_LIMIT) } else { named },
        author: tidy(xfer::field(&header, "presetAuthor"), NAME_LIMIT),
        tags: tags
            .iter()
            .filter_map(Value::as_str)
            .map(|tag| tidy(tag, NAME_LIMIT).to_lowercase())
            .filter(|tag| !tag.is_empty())
            .take(TAG_LIMIT)
            .collect(),
        description: tidy(xfer::field(&header, "presetDescription"), DESCRIPTION_LIMIT),
        locator: Locator::Vendor { format: super::Format::Serum2, path: file.to_path_buf(), key: None }.to_text(),
        ..PresetInfo::default()
    })
}

/// What is said of one of Xfer's files that is something else than a
/// preset of Serum 2: what the file calls itself, in a few words of its
/// own.
fn not_ours(kind: &str, product: &str) -> String {
    let (kind, product) = (tidy(kind, SAID_LIMIT), tidy(product, SAID_LIMIT));
    format!(
        "it is not a Serum 2 preset (it says it is a {} of {})",
        if kind.is_empty() { "file" } else { &kind },
        if product.is_empty() { "an unnamed product" } else { &product }
    )
}

/// The version of its settings a container says it holds.
fn version(header: &Map<String, Value>) -> Option<f64> {
    header.get("version").and_then(Value::as_f64)
}

/// The two states that make the plugin the patch in the preset file
/// `file`. `current` is the state the plugin has now, which says how new
/// a patch it can read.
pub(crate) fn build(file: &[u8], current: &PluginState) -> Result<Built, String> {
    let preset = xfer::unpack(file)?;
    let (product, kind) = (xfer::field(&preset.header, "product"), xfer::field(&preset.header, "fileType"));
    if product != PRODUCT || kind != FILE_TYPE {
        return Err(not_ours(kind, product));
    }
    // A plugin reads patches older than itself, not newer ones.
    let installed = xfer::header(&current.component).ok().and_then(|(header, _)| version(&header));
    if let (Some(theirs), Some(ours)) = (version(&preset.header), installed)
        && theirs > ours
    {
        return Err(format!(
            "it was saved by a newer Serum 2 than the one installed (its settings are version {theirs}, and this \
             one reads up to {ours})"
        ));
    }

    let entries = cbor::entries(&preset.body)?;
    // What the file says of itself as a file is not a setting, and which
    // half a state is for is said anew for each.
    let kept: Vec<(&[u8], &[u8])> = entries
        .iter()
        .filter(|entry| !matches!(entry.name, Some("fileType" | "component")))
        .map(|entry| (entry.key, entry.value))
        .collect();
    let about = |name: &str| preset.header.get(name).cloned().unwrap_or(Value::String(String::new()));
    let state_of = |half: &str| {
        let (key, value) = (cbor::text("component"), cbor::text(half));
        let mut settings = kept.clone();
        settings.push((&key, &value));
        let mut header = Map::new();
        header.insert("component".into(), half.into());
        header.insert("product".into(), PRODUCT.into());
        for field in ["productVersion", "url", "vendor", "version"] {
            if let Some(value) = preset.header.get(field) {
                header.insert(field.into(), value.clone());
            }
        }
        if half == "controller" {
            for field in ["presetAuthor", "presetDescription", NAME_FIELD] {
                header.insert(field.into(), about(field));
            }
            header.insert("presetComment".into(), "".into());
        }
        xfer::pack(header, &cbor::map(&settings))
    };
    let state = PluginState { component: state_of(HALVES[0]), controller: state_of(HALVES[1]) };
    let name = xfer::field(&preset.header, NAME_FIELD).to_string();
    Ok(Built { state, marks: vec![Mark { name: NAME_FIELD.to_string(), value: name }], note: None })
}

/// What `state`, a state of the plugin, shows of `name`: the name of the
/// preset it holds.
pub(crate) fn shown(state: &PluginState, name: &str) -> Option<String> {
    let (header, _) = xfer::header(&state.controller).ok()?;
    (name == NAME_FIELD).then(|| xfer::field(&header, NAME_FIELD).to_string())
}

/// A preset file's path as the test of another module needs it.
#[cfg(test)]
pub(crate) fn preset_folder(documents: &std::path::Path) -> PathBuf {
    folders(&VendorPlaces { documents: vec![documents.to_path_buf()], ..VendorPlaces::default() }).remove(0)
}

/// A preset file made here, in the format: a patch of three settings.
#[cfg(test)]
pub(crate) fn preset_file(name: &str, author: &str, version: f64, settings: &[(&str, &[u8])]) -> Vec<u8> {
    use serde_json::json;
    let (file_type, product) = (cbor::text(FILE_TYPE), cbor::text(PRODUCT));
    let keys: Vec<Vec<u8>> = settings.iter().map(|(key, _)| cbor::text(key)).collect();
    let mut entries: Vec<(&[u8], &[u8])> =
        keys.iter().zip(settings).map(|(key, (_, value))| (&key[..], *value)).collect();
    let (type_key, product_key) = (cbor::text("fileType"), cbor::text("product"));
    entries.push((&type_key, &file_type));
    entries.push((&product_key, &product));
    let header = json!({
        "fileType": FILE_TYPE, "hash": "", "presetAuthor": author, "presetDescription": format!("about {name}"),
        "presetName": name, "product": PRODUCT, "productVersion": "2.0.12", "tags": ["Pad", "Preview"],
        "url": "https://example.invalid/", "vendor": "Somebody", "version": version,
    });
    xfer::pack(header.as_object().cloned().unwrap(), &cbor::map(&entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Found;
    use crate::test_support::TestDir;

    fn plugin() -> PluginRef {
        PluginRef {
            uri: format!("vst3:{CLASS}"),
            id: CLASS.into(),
            path: PathBuf::from("Serum2.vst3"),
            name: "Serum 2".into(),
            vendor: "Xfer Records".into(),
        }
    }

    /// The state a plugin of settings version `version` gives.
    fn state_of_version(version: f64) -> PluginState {
        let header = |half: &str| {
            let header = serde_json::json!({ "component": half, "product": PRODUCT, "version": version });
            header.as_object().cloned().unwrap()
        };
        PluginState {
            component: xfer::pack(header("processor"), &cbor::map(&[])),
            controller: xfer::pack(header("controller"), &cbor::map(&[])),
        }
    }

    #[test]
    fn presets_are_listed_from_the_headers_of_the_files_in_its_folder() {
        let dir = TestDir::new();
        let documents = dir.path().join("documents");
        let folder = preset_folder(&documents);
        let relative = |file: &str| format!("documents/Xfer/Serum 2 Presets/Presets/{file}");
        dir.file(
            &relative("Factory/Pad/PD - Warm Room.SerumPreset"),
            &preset_file("PD - Warm Room", "Level 8", 4.0, &[]),
        );
        dir.file(&relative("Factory/Bass/BA - Sub.SerumPreset"), &preset_file("BA - Sub", "", 5.0, &[]));
        dir.file(&relative("User/Mine.SerumPreset"), &preset_file("", "Me", 5.0, &[]));
        // Files that are not presets of it: another product's, a file that
        // is no container, one cut off inside its header, and a wavetable.
        let other = xfer::pack(
            serde_json::json!({ "fileType": "SerumPreset", "product": "Other" }).as_object().cloned().unwrap(),
            &[],
        );
        dir.file(&relative("User/Other.SerumPreset"), &other);
        dir.file(&relative("User/Text.SerumPreset"), b"not a container");
        dir.file(&relative("User/Short.SerumPreset"), &preset_file("Short", "", 5.0, &[])[..30]);
        dir.file(&relative("Factory/Pad/table.wav"), b"RIFF");

        let places =
            VendorPlaces { documents: vec![documents, dir.path().join("no such folder")], ..VendorPlaces::default() };
        let mut found = Found::default();
        list(&plugin(), &places, &mut found);
        let listed: Vec<(String, &str)> =
            found.presets.iter().map(|preset| (preset.path(), preset.author.as_str())).collect();
        assert_eq!(
            listed,
            [
                ("Factory/Bass/BA - Sub".to_string(), ""),
                ("Factory/Pad/PD - Warm Room".to_string(), "Level 8"),
                ("User/Mine".to_string(), "Me")
            ]
        );
        let warm = &found.presets[1];
        assert_eq!(
            (warm.description.as_str(), warm.tags.as_slice()),
            ("about PD - Warm Room", &["pad".to_string(), "preview".to_string()][..])
        );
        let locator = Locator::parse(&warm.locator).unwrap();
        assert_eq!(
            locator,
            Locator::Vendor {
                format: super::super::Format::Serum2,
                path: folder.join("Factory/Pad/PD - Warm Room.SerumPreset"),
                key: None
            }
        );
        // Both folders are watched, the one that is not there too.
        assert_eq!(found.watch.len(), 2);
        assert_eq!((&found.watch[0].path, &found.watch[0].extensions), (&folder, &vec![EXTENSION.to_string()]));
    }

    #[test]
    fn a_preset_becomes_the_two_states_the_plugin_takes() {
        let wavetable = [0x58, 0x20].into_iter().chain(std::iter::repeat_n(9u8, 32)).collect::<Vec<u8>>();
        let settings: [(&str, &[u8]); 3] =
            [("Osc", &wavetable), ("mpeEnabled", &[0xf5]), ("version", &[0xf9, 0x44, 0x00])];
        let file = preset_file("PD - Warm Room", "Level 8", 4.0, &settings);
        let built = build(&file, &state_of_version(11.0)).unwrap();
        assert_eq!(built.marks, [Mark { name: "presetName".into(), value: "PD - Warm Room".into() }]);
        assert_eq!(built.note, None);

        for (half, bytes) in [("processor", &built.state.component), ("controller", &built.state.controller)] {
            let container = xfer::unpack(bytes).unwrap();
            // The header names the half, and carries on what the file said
            // of the product and of the version of its settings.
            assert_eq!(xfer::field(&container.header, "component"), half);
            assert_eq!(xfer::field(&container.header, "product"), "Serum2");
            assert_eq!(xfer::field(&container.header, "productVersion"), "2.0.12");
            assert_eq!(container.header["version"], serde_json::json!(4.0));
            assert!(!container.header.contains_key("fileType"));
            assert_eq!(container.header.contains_key("presetName"), half == "controller");
            // The settings are the file's, byte for byte, without what it
            // says of itself as a file and with the half they are for.
            let entries = cbor::entries(&container.body).unwrap();
            let names: Vec<&str> = entries.iter().filter_map(|entry| entry.name).collect();
            assert_eq!(names, ["Osc", "mpeEnabled", "version", "product", "component"]);
            assert_eq!(entries[0].value, wavetable);
            assert_eq!(entries[4].value, cbor::text(half));
        }
        // Which is what the plugin's state then shows.
        assert_eq!(shown(&built.state, "presetName").as_deref(), Some("PD - Warm Room"));
        assert_eq!(shown(&built.state, "something else"), None);
        assert_eq!(shown(&PluginState::default(), "presetName"), None);
    }

    #[test]
    fn a_file_that_will_not_do_says_why() {
        let current = state_of_version(5.0);
        let wrong = |file: &[u8]| build(file, &current).unwrap_err();
        assert_eq!(wrong(b"RIFF"), "it is not one of Xfer's files");
        // A patch from a newer Serum than the one that is to read it.
        let newer = wrong(&preset_file("New", "", 12.0, &[]));
        assert!(
            newer.contains("saved by a newer Serum 2") && newer.contains("version 12") && newer.contains("up to 5"),
            "{newer}"
        );
        assert!(build(&preset_file("Same", "", 5.0, &[]), &current).is_ok());
        // With a plugin whose state says nothing of versions, it is tried.
        assert!(build(&preset_file("New", "", 12.0, &[]), &PluginState::default()).is_ok());
        // Another product's file, and settings that are no map.
        let other = xfer::pack(
            serde_json::json!({ "fileType": "SerumFX", "product": "Serum2" }).as_object().cloned().unwrap(),
            &[],
        );
        assert_eq!(wrong(&other), "it is not a Serum 2 preset (it says it is a SerumFX of Serum2)");
        let list = xfer::pack(
            serde_json::json!({ "fileType": FILE_TYPE, "product": PRODUCT }).as_object().cloned().unwrap(),
            &[0x80],
        );
        assert_eq!(wrong(&list), "it is not a map of settings");
        let cut = preset_file("Cut", "", 5.0, &[]);
        assert!(wrong(&cut[..cut.len() - 3]).starts_with("its settings"));
    }
}
