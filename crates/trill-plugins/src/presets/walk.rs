//! Walking a folder of preset files: listing them, and telling whether
//! anything about them has changed since they were last read.

use super::Watch;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Deepest nesting followed below a preset folder. Banks nest three or
/// four deep; the limit only stops a cycle of links.
const MAX_DEPTH: usize = 16;
/// The most preset files taken from one folder. A plugin that points at
/// the root of a disk must not keep the reader busy for an hour.
pub(crate) const MAX_FILES: usize = 100_000;

/// Whether `path` has one of `extensions` (without the dot, lower case).
/// With no extensions every file has.
fn has_extension(path: &Path, extensions: &[String]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else { return false };
    extensions.iter().any(|wanted| wanted.eq_ignore_ascii_case(extension))
}

/// The files below `folder` that have one of `extensions`, sorted so that
/// a folder's files come before its subfolders', each by name: the order
/// a browser shows them in. Hidden files and folders are left out, and
/// what cannot be read is skipped.
pub(crate) fn files(folder: &Path, extensions: &[String]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    visit(folder, extensions, 0, &mut found);
    found
}

fn visit(folder: &Path, extensions: &[String], depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let (mut files, mut folders) = (Vec::new(), Vec::new());
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_none_or(|name| name.to_string_lossy().starts_with('.')) {
            continue;
        }
        // Following links: presets are often kept on another disk.
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => folders.push(path),
            Ok(metadata) if metadata.is_file() && has_extension(&path, extensions) => files.push(path),
            _ => {}
        }
    }
    let by_name = |a: &PathBuf, b: &PathBuf| {
        let name = |path: &PathBuf| path.file_name().map(|name| name.to_string_lossy().to_lowercase());
        name(a).cmp(&name(b)).then_with(|| a.cmp(b))
    };
    files.sort_by(by_name);
    folders.sort_by(by_name);
    for file in files {
        if found.len() >= MAX_FILES {
            return;
        }
        found.push(file);
    }
    if depth < MAX_DEPTH {
        for folder in folders {
            visit(&folder, extensions, depth + 1, found);
        }
    }
}

/// A number that changes when a file is added to, removed from, renamed
/// in or written to one of the watched folders. Reading no file, it costs
/// a listing of the folders.
pub(crate) fn fingerprint(watch: &[Watch]) -> u64 {
    let mut total: u64 = 0xcbf2_9ce4_8422_2325;
    for folder in watch {
        // A folder that is not there counts too: it may appear.
        total = mix(total, hash(folder.path.to_string_lossy().as_bytes()));
        // What is watched may be a single file that holds many presets.
        let alone = folder.path.is_file().then(|| folder.path.clone());
        for file in alone.into_iter().chain(files(&folder.path, &folder.extensions)) {
            let (modified, size) = std::fs::metadata(&file)
                .map(|metadata| {
                    let modified = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok());
                    (modified.map_or(0, |elapsed| elapsed.as_nanos() as u64), metadata.len())
                })
                .unwrap_or_default();
            let entry = mix(mix(hash(file.to_string_lossy().as_bytes()), modified), size);
            // Added up, so that the order the system lists files in does
            // not matter.
            total = total.wrapping_add(entry);
        }
    }
    total
}

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn mix(hash: u64, value: u64) -> u64 {
    (hash ^ value).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(23)
}

/// Where a file lies below `root`, as the category to file a preset
/// under: its folders, joined the way categories are. Empty for a file
/// directly in `root`, or one that is not below it at all.
pub(crate) fn category_below(root: &Path, file: &Path) -> String {
    let Some(relative) = file.parent().and_then(|parent| parent.strip_prefix(root).ok()) else { return String::new() };
    let parts: Vec<String> = relative
        .components()
        .map(|part| super::tidy(&part.as_os_str().to_string_lossy(), super::NAME_LIMIT))
        .filter(|part| !part.is_empty())
        .collect();
    parts.join(&trill_core::presets::CATEGORY_SEPARATOR.to_string())
}

/// Joins the parts of a category that are not empty.
pub(crate) fn category(parts: &[&str]) -> String {
    let parts: Vec<&str> = parts.iter().copied().filter(|part| !part.is_empty()).collect();
    parts.join(&trill_core::presets::CATEGORY_SEPARATOR.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    fn watch(dir: &TestDir, extensions: &[&str]) -> Watch {
        Watch { path: dir.path().join("presets"), extensions: extensions.iter().map(|e| e.to_string()).collect() }
    }

    fn names(dir: &TestDir, extensions: &[&str]) -> Vec<String> {
        let watch = watch(dir, extensions);
        files(&watch.path, &watch.extensions)
            .iter()
            .map(|file| file.strip_prefix(&watch.path).unwrap().to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn files_are_listed_folder_by_folder_in_name_order() {
        let dir = TestDir::new();
        for file in ["Pads/warm.fxp", "Pads/Soft/air.FXP", "Pads/Glass.fxp", "bass.fxp", "Zebra.fxp", "notes.txt"] {
            dir.file(&format!("presets/{file}"), b"x");
        }
        dir.file("presets/.hidden/secret.fxp", b"x");
        dir.file("presets/Pads/.DS_Store", b"x");

        assert_eq!(
            names(&dir, &["fxp"]),
            ["bass.fxp", "Zebra.fxp", "Pads/Glass.fxp", "Pads/warm.fxp", "Pads/Soft/air.FXP"]
        );
        assert_eq!(names(&dir, &["txt", "fxp"]).len(), 6);
        assert_eq!(names(&dir, &[]).len(), 6, "no extension: every file that is not hidden");
        assert!(files(&dir.path().join("missing"), &[]).is_empty());
    }

    #[test]
    fn the_fingerprint_follows_the_files() {
        let dir = TestDir::new();
        let watched = [watch(&dir, &["fxp"])];
        let missing = fingerprint(&watched);
        let one = dir.file("presets/Pads/one.fxp", b"one");
        let with_one = fingerprint(&watched);
        assert_ne!(missing, with_one, "a folder that appears is a change");
        assert_eq!(fingerprint(&watched), with_one, "and nothing changing is none");

        dir.file("presets/Pads/notes.txt", b"not a preset");
        assert_eq!(fingerprint(&watched), with_one, "files that do not count do not count");
        dir.file("presets/two.fxp", b"two");
        let with_two = fingerprint(&watched);
        assert_ne!(with_two, with_one);
        std::fs::write(&one, b"one, edited").unwrap();
        let edited = fingerprint(&watched);
        assert_ne!(edited, with_two);
        std::fs::rename(&one, one.with_file_name("renamed.fxp")).unwrap();
        assert_ne!(fingerprint(&watched), edited);
        // A single file can be watched as well as a folder.
        let bank = [Watch { path: dir.file("bank.db", b"presets"), extensions: Vec::new() }];
        let before = fingerprint(&bank);
        std::fs::write(&bank[0].path, b"more presets").unwrap();
        assert_ne!(fingerprint(&bank), before);
        // Another set of folders is another fingerprint, even when empty.
        assert_ne!(fingerprint(&[]), fingerprint(&[watch(&TestDir::new(), &[])]));
    }

    #[test]
    fn a_file_is_filed_under_its_folders() {
        let root = Path::new("presets");
        assert_eq!(category_below(root, &root.join("Pads").join("Soft").join("air.fxp")), "Pads/Soft");
        assert_eq!(category_below(root, &root.join("air.fxp")), "");
        assert_eq!(category_below(root, Path::new("elsewhere/air.fxp")), "");
        assert_eq!(category(&["Factory", "", "Pads/Soft"]), "Factory/Pads/Soft");
        assert_eq!(category(&["", ""]), "");
    }
}
