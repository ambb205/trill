//! Reads the classes of a VST3 module.

use super::class_id::ClassId;
use super::module::Module;
use super::{paths, text};
use crate::scan::PluginRecord;
use std::path::Path;
use vst3::ComPtr;
use vst3::Steinberg::{
    IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactory3, IPluginFactory3Trait, IPluginFactoryTrait,
    PClassInfo, PClassInfo2, PClassInfoW, PFactoryInfo, kResultOk,
};

/// The category of the classes that are plugins: a module's processors.
/// Its other classes (edit controllers, mostly) are created through them.
const AUDIO_EFFECT_CATEGORY: &str = "Audio Module Class";
/// What separates the entries of a class's sub-categories, such as
/// `Instrument|Synth`.
const SUB_CATEGORY_SEPARATOR: char = '|';
/// The most classes read from one module. Shells that wrap a vendor's
/// whole catalog have hundreds; a plugin reporting nonsense must not make
/// the scan loop for minutes.
const MAX_CLASSES: i32 = 1 << 14;

/// Loads the VST3 plugin at `path` (its library, or its bundle directory)
/// into the current process and lists the plugins it contains. The library
/// is unloaded again before returning.
///
/// See [`crate::scan::probe_file`] for why the DAW does this in a child
/// process.
pub(crate) fn probe(path: &Path) -> Result<Vec<PluginRecord>, String> {
    let module = Module::load(&paths::resolve(path)?)?;
    Ok(records(module.factory()))
}

/// Lists the plugins a module's factory offers.
pub(crate) fn records(factory: &ComPtr<IPluginFactory>) -> Vec<PluginRecord> {
    let mut info = PFactoryInfo { vendor: [0; 64], url: [0; 256], email: [0; 128], flags: 0 };
    // SAFETY: `factory` is a live factory and `info` a local it fills in.
    let described = unsafe { factory.getFactoryInfo(&mut info) } == kResultOk;
    let factory_vendor = if described { text::from_chars(&info.vendor) } else { String::new() };
    classes(factory).into_iter().filter_map(|class| record(class, &factory_vendor)).collect()
}

/// One class of a module, as told by whichever version of the factory
/// interface the module implements.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClassInfo {
    id: ClassId,
    category: String,
    name: String,
    /// Empty when the factory predates per-class vendors.
    vendor: String,
    version: String,
    sub_categories: String,
}

/// Every class of the factory, each described by the newest interface that
/// answers for it: the third version with Unicode names, the second with
/// sub-categories and a vendor, or the first with the bare minimum.
fn classes(factory: &ComPtr<IPluginFactory>) -> Vec<ClassInfo> {
    let factory2 = factory.cast::<IPluginFactory2>();
    let factory3 = factory.cast::<IPluginFactory3>();
    // SAFETY: `factory` is a live factory.
    let count = unsafe { factory.countClasses() }.clamp(0, MAX_CLASSES);
    (0..count)
        .filter_map(|index| {
            let unicode = factory3.as_ref().and_then(|factory| {
                let mut info = PClassInfoW {
                    cid: [0; 16],
                    cardinality: 0,
                    category: [0; 32],
                    name: [0; 64],
                    classFlags: 0,
                    subCategories: [0; 128],
                    vendor: [0; 64],
                    version: [0; 64],
                    sdkVersion: [0; 64],
                };
                // SAFETY: `factory` is a live factory and `info` a local
                // it fills in.
                (unsafe { factory.getClassInfoUnicode(index, &mut info) } == kResultOk).then(|| ClassInfo {
                    id: ClassId::from_tuid(&info.cid),
                    category: text::from_chars(&info.category),
                    name: text::from_utf16(&info.name),
                    vendor: text::from_utf16(&info.vendor),
                    version: text::from_utf16(&info.version),
                    sub_categories: text::from_chars(&info.subCategories),
                })
            });
            let extended = || {
                let factory = factory2.as_ref()?;
                let mut info = PClassInfo2 {
                    cid: [0; 16],
                    cardinality: 0,
                    category: [0; 32],
                    name: [0; 64],
                    classFlags: 0,
                    subCategories: [0; 128],
                    vendor: [0; 64],
                    version: [0; 64],
                    sdkVersion: [0; 64],
                };
                // SAFETY: as above.
                (unsafe { factory.getClassInfo2(index, &mut info) } == kResultOk).then(|| ClassInfo {
                    id: ClassId::from_tuid(&info.cid),
                    category: text::from_chars(&info.category),
                    name: text::from_chars(&info.name),
                    vendor: text::from_chars(&info.vendor),
                    version: text::from_chars(&info.version),
                    sub_categories: text::from_chars(&info.subCategories),
                })
            };
            let basic = || {
                let mut info = PClassInfo { cid: [0; 16], cardinality: 0, category: [0; 32], name: [0; 64] };
                // SAFETY: as above.
                (unsafe { factory.getClassInfo(index, &mut info) } == kResultOk).then(|| ClassInfo {
                    id: ClassId::from_tuid(&info.cid),
                    category: text::from_chars(&info.category),
                    name: text::from_chars(&info.name),
                    vendor: String::new(),
                    version: String::new(),
                    sub_categories: String::new(),
                })
            };
            unicode.or_else(extended).or_else(basic)
        })
        .collect()
}

/// The plugin a class is, if it is one.
fn record(class: ClassInfo, factory_vendor: &str) -> Option<PluginRecord> {
    // A class without an id could never be instantiated.
    if class.category != AUDIO_EFFECT_CATEGORY || class.id.is_nil() {
        return None;
    }
    let id = class.id.to_string();
    let mut features: Vec<String> = Vec::new();
    for category in class.sub_categories.split(SUB_CATEGORY_SEPARATOR).map(str::trim) {
        // Plugins have been seen to name a category twice.
        if !category.is_empty() && !features.iter().any(|feature| feature == category) {
            features.push(category.to_string());
        }
    }
    Some(PluginRecord {
        // A plugin has to be listed under something.
        name: if class.name.is_empty() { id.clone() } else { class.name },
        id,
        vendor: if class.vendor.is_empty() { factory_vendor.to_string() } else { class.vendor },
        version: class.version,
        // VST3 classes carry no description.
        description: String::new(),
        features,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    fn class(category: &str, name: &str, sub_categories: &str) -> ClassInfo {
        ClassInfo {
            id: ClassId::parse("ABCDEF019182FAEB5776737957767331").unwrap(),
            category: category.to_string(),
            name: name.to_string(),
            vendor: "Acme".to_string(),
            version: "2.1".to_string(),
            sub_categories: sub_categories.to_string(),
        }
    }

    #[test]
    fn a_file_that_is_not_a_library_is_an_error() {
        let dir = TestDir::new();
        let path = dir.file("fake.vst3", b"this is not a shared library");
        assert!(probe(&path).is_err());
        assert!(probe(&dir.path().join("missing.vst3")).is_err());
        // A bundle is resolved to its library first.
        dir.file("Empty.vst3/Contents/Resources/moduleinfo.json", b"{}");
        let error = probe(&dir.path().join("Empty.vst3")).unwrap_err();
        assert!(error.contains("no library for this platform"), "{error}");
    }

    #[test]
    fn processor_classes_become_records() {
        let synth = class(AUDIO_EFFECT_CATEGORY, "Acme Synth", "Instrument|Synth");
        assert_eq!(
            record(synth, "Factory Vendor"),
            Some(PluginRecord {
                id: "ABCDEF019182FAEB5776737957767331".into(),
                name: "Acme Synth".into(),
                vendor: "Acme".into(),
                version: "2.1".into(),
                description: String::new(),
                features: vec!["Instrument".into(), "Synth".into()],
            })
        );
        // Sub-categories are split leniently, and each is listed once.
        let odd = record(class(AUDIO_EFFECT_CATEGORY, "X", " Fx | |Reverb|Fx|"), "").unwrap();
        assert_eq!(odd.features, ["Fx", "Reverb"]);
        assert!(record(class(AUDIO_EFFECT_CATEGORY, "X", ""), "").unwrap().features.is_empty());
    }

    #[test]
    fn other_classes_are_not_plugins() {
        assert_eq!(record(class("Component Controller Class", "Acme Synth", "Instrument"), ""), None);
        assert_eq!(record(class("", "Acme Synth", ""), ""), None);
        let nil = ClassInfo { id: ClassId::from_tuid(&[0; 16]), ..class(AUDIO_EFFECT_CATEGORY, "X", "") };
        assert_eq!(record(nil, ""), None);
    }

    #[test]
    fn missing_names_and_vendors_are_filled_in() {
        let anonymous = ClassInfo { vendor: String::new(), ..class(AUDIO_EFFECT_CATEGORY, "", "Fx") };
        let record = record(anonymous, "Factory Vendor").unwrap();
        assert_eq!(record.name, "ABCDEF019182FAEB5776737957767331", "a nameless plugin is listed under its id");
        assert_eq!(record.vendor, "Factory Vendor", "the module's vendor stands in for the class's");
    }
}
