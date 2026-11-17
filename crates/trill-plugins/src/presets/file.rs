//! A preset asked for as a file: one that lies outside the folders a
//! plugin's presets are listed from.
//!
//! People keep presets they bought, or were sent, wherever they keep their
//! sounds. Such a file is as good a preset as a listed one, and is loaded
//! the same way, from a [`Locator`] that says where it is. What is done
//! here is the part that listing does for the others: saying which preset
//! a path stands for, or in a sentence why it stands for none.

use super::Locator;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;
use trill_core::presets::{PresetInfo, PresetList, PresetLookup, WITHIN_FILE};

/// The most presets of a file that an error names.
const NAMED: usize = 12;
/// How many files' readings are remembered.
const REMEMBERED: usize = 16;

/// Whether there is a file at `file` to read at all; if not, what to say
/// of the path.
pub(crate) fn check(file: &Path) -> Result<(), String> {
    match std::fs::metadata(file) {
        Ok(found) if found.is_file() => Ok(()),
        Ok(_) => Err("it is a folder, not a file".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err("there is no such file".to_string()),
        Err(error) => Err(format!("it cannot be read ({error})")),
    }
}

/// Whether `file` has the extension `wanted` (lower case, without the
/// dot), in whatever case.
pub(crate) fn has_extension(file: &Path, wanted: &str) -> bool {
    extension(file).is_some_and(|extension| extension == wanted)
}

/// The extension of `file` in lower case, without the dot.
pub(crate) fn extension(file: &Path) -> Option<String> {
    file.extension().map(|extension| extension.to_string_lossy().to_lowercase())
}

/// A path as it is compared with another: where the file system does not
/// tell upper case from lower, neither does this.
fn folded(path: &Path) -> Vec<String> {
    let part = |part: std::path::Component<'_>| {
        let text = part.as_os_str().to_string_lossy();
        if cfg!(windows) { text.to_lowercase() } else { text.into_owned() }
    };
    path.components().map(part).collect()
}

/// The file a listed preset is in, if it is in one.
pub(crate) fn file_of(preset: &PresetInfo) -> Option<std::path::PathBuf> {
    match Locator::parse(&preset.locator)? {
        Locator::ClapFile { path, .. } | Locator::Vst3File { path } | Locator::Vendor { path, .. } => Some(path),
        Locator::ClapPlugin { .. } | Locator::Vst3Program { .. } => None,
    }
}

/// The presets among `known`, a plugin's listed ones, that are in the
/// file at `file`: a file in one of the folders the plugin's presets are
/// listed from is the preset the list has, with what the list knows of it.
pub(crate) fn listed(known: Option<&PresetList>, file: &Path) -> Vec<PresetInfo> {
    let wanted = folded(file);
    let Some(known) = known else { return Vec::new() };
    let in_file = |preset: &&PresetInfo| file_of(preset).is_some_and(|path| folded(&path) == wanted);
    known.presets.iter().filter(in_file).cloned().collect()
}

/// The preset of `presets`, those the file at `file` holds, that is
/// meant. A file of one preset means that one. In a file of several,
/// `within` says which, as a preset is asked for anywhere: by its name,
/// its category and name, or its id.
pub(crate) fn choose(presets: Vec<PresetInfo>, within: Option<&str>, file: &Path) -> Result<PresetInfo, String> {
    let names = |presets: &[&PresetInfo]| {
        let shown: Vec<String> = presets.iter().take(NAMED).map(|preset| preset.path()).collect();
        let more = if presets.len() > NAMED { ", ..." } else { "" };
        format!("{}{more}", shown.join(", "))
    };
    let how = format!("'{}{WITHIN_FILE}<name>'", file.display());
    let all: Vec<&PresetInfo> = presets.iter().collect();
    match (presets.len(), within) {
        (0, _) => Err("it holds no presets".to_string()),
        (1, None) => Ok(presets.into_iter().next().expect("one preset")),
        (count, None) => Err(format!("it holds {count} presets ({}): say which with {how}", names(&all))),
        (count, Some(within)) => {
            let list = PresetList::ready(presets.clone());
            match list.find(within) {
                PresetLookup::Found(preset) => Ok(preset.clone()),
                PresetLookup::Ambiguous(found) => {
                    let ids: Vec<String> = found
                        .iter()
                        .take(NAMED)
                        .map(|preset| format!("{} (id {})", preset.path(), preset.id))
                        .collect();
                    Err(format!(
                        "it holds {} presets named '{within}': {}. Ask for one by its category and name, or by its id",
                        found.len(),
                        ids.join(", ")
                    ))
                }
                PresetLookup::Missing { similar } if similar.is_empty() => {
                    Err(format!("it holds no preset '{within}' among its {count} ({})", names(&all)))
                }
                PresetLookup::Missing { similar } => {
                    Err(format!("it holds no preset '{within}' among its {count} (nearest: {})", names(&similar)))
                }
            }
        }
    }
}

/// What a file was when it was read: how long, and when last written.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(file: &Path) -> Option<Self> {
        let found = std::fs::metadata(file).ok()?;
        Some(Self { size: found.len(), modified: found.modified().ok() })
    }
}

/// The last few files that were read to say which presets they hold, with
/// what came of it. Whoever is about to load a preset from a file asks
/// what it is first, off the thread that then loads it; the second asking
/// is answered from here.
#[derive(Default)]
pub(crate) struct Readings {
    read: Mutex<Vec<Reading>>,
}

struct Reading {
    /// What kind of file it was read as.
    kind: String,
    file: Vec<String>,
    stamp: Stamp,
    presets: Result<Vec<PresetInfo>, String>,
}

impl Readings {
    fn remembered(&self) -> MutexGuard<'_, Vec<Reading>> {
        self.read.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What `read` makes of the file at `file`, read as a file of `kind`:
    /// from the last time, if the file is as it was then.
    pub(crate) fn of(
        &self,
        kind: &str,
        file: &Path,
        read: impl FnOnce() -> Result<Vec<PresetInfo>, String>,
    ) -> Result<Vec<PresetInfo>, String> {
        let (wanted, stamp) = (folded(file), Stamp::of(file));
        let same = |reading: &Reading| reading.kind == kind && reading.file == wanted;
        if let Some(stamp) = &stamp
            && let Some(reading) = self.remembered().iter().find(|reading| same(reading) && reading.stamp == *stamp)
        {
            return reading.presets.clone();
        }
        let presets = read();
        if let Some(stamp) = stamp {
            let mut remembered = self.remembered();
            remembered.retain(|reading| !same(reading));
            if remembered.len() >= REMEMBERED {
                remembered.remove(0);
            }
            remembered.push(Reading { kind: kind.to_string(), file: wanted, stamp, presets: presets.clone() });
        }
        presets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use trill_core::presets::short_id;

    fn preset(name: &str, category: &str, locator: Locator) -> PresetInfo {
        let locator = locator.to_text();
        PresetInfo {
            id: short_id(&locator),
            name: name.into(),
            category: category.into(),
            locator,
            ..PresetInfo::default()
        }
    }

    #[test]
    fn a_path_that_is_no_file_says_what_it_is_instead() {
        let dir = TestDir::new();
        let file = dir.file("presets/Lead.VstPreset", b"x");
        assert_eq!(check(&file), Ok(()));
        assert_eq!(check(&dir.path().join("presets")).unwrap_err(), "it is a folder, not a file");
        assert_eq!(check(&dir.path().join("presets/Gone.vstpreset")).unwrap_err(), "there is no such file");
        assert!(has_extension(&file, "vstpreset") && !has_extension(&file, "fxp"));
        assert_eq!(extension(Path::new("kits/presets")), None);
    }

    #[test]
    fn a_file_among_the_listed_ones_is_the_preset_the_list_has() {
        let folder = PathBuf::from(if cfg!(windows) { r"C:\Presets\Synth" } else { "/presets/synth" });
        let pad =
            preset("Warm Pad", "Pads", Locator::Vst3File { path: folder.join("Pads").join("Warm Pad.vstpreset") });
        let program = preset("Prog", "", Locator::Vst3Program { param: 1, index: 2, steps: 3 });
        let kits = folder.join("kits");
        let kit = |index: usize, name: &str| {
            let key = Some(format!("{index}:{name}"));
            preset(name, "Rock", Locator::Vendor { format: crate::vendor::Format::Superior3, path: kits.clone(), key })
        };
        let list = PresetList::ready(vec![pad.clone(), program, kit(0, "Dry"), kit(1, "Roomy")]);

        assert_eq!(listed(Some(&list), &folder.join("Pads").join("Warm Pad.vstpreset")), [pad.clone()]);
        // A path is the same path however its separators are written, and
        // on a disk that does not tell the cases apart, in either case.
        if cfg!(windows) {
            assert_eq!(listed(Some(&list), Path::new("c:/presets/SYNTH/pads/warm pad.vstpreset")), [pad]);
        }
        // A file of several presets is all of them; a file the list does
        // not know, and a list that is not known, none.
        assert_eq!(listed(Some(&list), &kits).len(), 2);
        assert!(listed(Some(&list), &folder.join("Other.vstpreset")).is_empty());
        assert!(listed(None, &kits).is_empty());
    }

    #[test]
    fn one_preset_of_a_file_of_several_is_asked_for_as_presets_are() {
        let file = Path::new("kits");
        let kit = |index: usize, name: &str, menu: &str| {
            let key = Some(format!("{index}:{name}"));
            let locator = Locator::Vendor { format: crate::vendor::Format::Superior3, path: file.into(), key };
            preset(name, menu, locator)
        };
        let kits = vec![kit(0, "Dry", "Rock"), kit(1, "Roomy", "Rock"), kit(2, "Dry", "Jazz")];
        let chosen = |within: Option<&str>| choose(kits.clone(), within, file).map(|preset| preset.path());

        // A file of one preset is that preset; a name that is given must
        // still be its own.
        assert_eq!(choose(kits[..1].to_vec(), None, file).unwrap(), kits[0]);
        assert_eq!(choose(kits[..1].to_vec(), Some("dry"), file).unwrap(), kits[0]);
        let error = choose(kits[..1].to_vec(), Some("Wet"), file).unwrap_err();
        assert_eq!(error, "it holds no preset 'Wet' among its 1 (Rock/Dry)");
        assert_eq!(choose(Vec::new(), None, file).unwrap_err(), "it holds no presets");

        // Of several, one is named: by name, by menu and name, or by id.
        let error = chosen(None).unwrap_err();
        assert_eq!(error, "it holds 3 presets (Rock/Dry, Rock/Roomy, Jazz/Dry): say which with 'kits#<name>'");
        assert_eq!(chosen(Some("roomy")).as_deref(), Ok("Rock/Roomy"));
        assert_eq!(chosen(Some("Jazz/Dry")).as_deref(), Ok("Jazz/Dry"));
        assert_eq!(chosen(Some(&kits[2].id)).as_deref(), Ok("Jazz/Dry"));
        let error = chosen(Some("Dry")).unwrap_err();
        assert!(error.starts_with("it holds 2 presets named 'Dry': Rock/Dry (id "), "{error}");
        assert!(error.ends_with("Ask for one by its category and name, or by its id"), "{error}");
        let error = chosen(Some("Room")).unwrap_err();
        assert_eq!(error, "it holds no preset 'Room' among its 3 (nearest: Rock/Roomy)");
    }

    #[test]
    fn a_file_is_read_once_for_as_long_as_it_is_the_same_file() {
        let dir = TestDir::new();
        let file = dir.file("Warm.preset", b"one");
        let (readings, count) = (Readings::default(), AtomicUsize::new(0));
        let read = |kind: &str, file: &Path| {
            readings.of(kind, file, || {
                let count = count.fetch_add(1, Ordering::Relaxed);
                Ok(vec![preset(&format!("read {count}"), "", Locator::Vst3File { path: file.into() })])
            })
        };
        let name = |read: Result<Vec<PresetInfo>, String>| read.unwrap().remove(0).name;

        assert_eq!(name(read("serum2", &file)), "read 0");
        assert_eq!(name(read("serum2", &file)), "read 0", "remembered");
        // Read as another kind of file it is another question.
        assert_eq!(name(read("ample", &file)), "read 1");
        // Written again, it is read again.
        std::fs::write(&file, b"another, longer").unwrap();
        assert_eq!(name(read("serum2", &file)), "read 2");
        // What cannot be stamped is not remembered, and an error is as
        // good an answer to remember as a preset.
        let gone = dir.path().join("Gone.preset");
        assert_eq!(name(read("serum2", &gone)), "read 3");
        assert_eq!(name(read("serum2", &gone)), "read 4");
        let failed =
            || readings.of("damaged", &file, || Err(format!("damaged {}", count.fetch_add(1, Ordering::Relaxed))));
        assert_eq!((failed(), failed()), (Err("damaged 5".to_string()), Err("damaged 5".to_string())));
        // Only the last few are kept.
        for index in 0..REMEMBERED {
            let other = dir.file(&format!("other {index}.preset"), b"x");
            read("serum2", &other).unwrap();
        }
        assert_eq!(readings.remembered().len(), REMEMBERED);
    }
}
