//! Where VST3 plugins are installed, per the VST 3 SDK's "Plug-in
//! Locations", and how to find the plugin files below those places.
//!
//! A VST3 plugin is a *bundle*: a directory `Name.vst3` with the library
//! for each platform inside it, at a place that depends on the platform.
//! On Windows it may also be a single library named `Name.vst3`, the layout
//! of the first VST3 versions. Either way, what this host calls the plugin
//! *file* is the library: the thing that is loaded, and whose modification
//! time says whether the plugin changed. (A bundle directory's own time
//! does not change when the library inside it is replaced.)

use super::PLUGIN_EXTENSION;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Deepest directory nesting searched below a search path. Vendors use one
/// or two levels; the limit only stops symlink cycles.
const MAX_DEPTH: usize = 12;

/// The directory inside a bundle's `Contents` that holds this platform's
/// library, and the library's file extension.
#[cfg(windows)]
const BUNDLE_LIBRARY: (&str, &str) = (
    if cfg!(target_arch = "x86_64") {
        "x86_64-win"
    } else if cfg!(target_arch = "aarch64") {
        "arm64-win"
    } else {
        "x86-win"
    },
    PLUGIN_EXTENSION,
);
/// A macOS bundle holds one universal binary, without an extension.
#[cfg(target_os = "macos")]
const BUNDLE_LIBRARY: (&str, &str) = ("MacOS", "");
/// Elsewhere the directory is named after the machine, as `uname -m`
/// prints it: `x86_64-linux`, `aarch64-linux`, `i386-linux`.
#[cfg(all(unix, not(target_os = "macos")))]
const BUNDLE_LIBRARY: (&str, &str) = (
    if cfg!(target_arch = "x86_64") {
        "x86_64-linux"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64-linux"
    } else if cfg!(target_arch = "x86") {
        "i386-linux"
    } else if cfg!(target_arch = "arm") {
        "armv7l-linux"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64-linux"
    } else {
        "unknown-linux"
    },
    "so",
);

/// The directories a VST3 host must search on this system, the user's
/// before the system's. When a plugin is installed in several places the
/// first one wins.
pub fn standard_search_paths() -> Vec<PathBuf> {
    search_paths(|name| std::env::var_os(name))
}

fn search_paths(env: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    // An unset and an empty variable mean the same thing.
    let var = |name: &str| env(name).filter(|value| !value.is_empty());
    let mut paths = Vec::new();
    platform_paths(&var, &mut paths);
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}

#[cfg(windows)]
fn platform_paths(var: &dyn Fn(&str) -> Option<OsString>, paths: &mut Vec<PathBuf>) {
    if let Some(local) = var("LOCALAPPDATA") {
        paths.push(PathBuf::from(local).join("Programs").join("Common").join("VST3"));
    }
    if let Some(common) = var("COMMONPROGRAMFILES") {
        paths.push(PathBuf::from(common).join("VST3"));
    }
}

#[cfg(target_os = "macos")]
fn platform_paths(var: &dyn Fn(&str) -> Option<OsString>, paths: &mut Vec<PathBuf>) {
    if let Some(home) = var("HOME") {
        paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"));
    }
    paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_paths(var: &dyn Fn(&str) -> Option<OsString>, paths: &mut Vec<PathBuf>) {
    if let Some(home) = var("HOME") {
        paths.push(PathBuf::from(home).join(".vst3"));
    }
    paths.push(PathBuf::from("/usr/lib/vst3"));
    paths.push(PathBuf::from("/usr/local/lib/vst3"));
}

fn named_vst3(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case(PLUGIN_EXTENSION))
}

/// The library of this platform inside the bundle directory `bundle`:
/// `Name.vst3/Contents/<platform>/Name.<extension>`. `None` if the bundle
/// was not built for this platform.
pub(crate) fn bundle_library(bundle: &Path) -> Option<PathBuf> {
    let (directory, extension) = BUNDLE_LIBRARY;
    let mut name = bundle.file_stem()?.to_os_string();
    if !extension.is_empty() {
        name.push(".");
        name.push(extension);
    }
    let library = bundle.join("Contents").join(directory).join(name);
    library.is_file().then_some(library)
}

/// Whether `path` is a VST3 plugin file: a library named `*.vst3`, or one
/// inside a bundle directory of that name. Only on Windows does a bundle's
/// library carry the extension itself.
pub(crate) fn is_plugin_file(path: &Path) -> bool {
    path.ancestors().take(4).any(named_vst3)
}

/// The library to load for `path`, which may be a plugin file already or a
/// bundle directory.
pub(crate) fn resolve(path: &Path) -> Result<PathBuf, String> {
    if !path.is_dir() {
        return Ok(path.to_path_buf());
    }
    bundle_library(path).ok_or_else(|| {
        format!("the bundle has no library for this platform (Contents/{}/{})", BUNDLE_LIBRARY.0, library_name(path))
    })
}

/// For messages: what the library of `bundle` would be called.
fn library_name(bundle: &Path) -> String {
    let stem = bundle.file_stem().unwrap_or_default().to_string_lossy();
    match BUNDLE_LIBRARY.1 {
        "" => stem.into_owned(),
        extension => format!("{stem}.{extension}"),
    }
}

/// Every VST3 plugin file below `directories`, in search order. Vendors
/// nest plugins in subdirectories, so the search is recursive, but it does
/// not look inside bundles. Bundles without a library for this platform,
/// and directories that do not exist or cannot be read, are skipped.
pub fn find_plugin_files(directories: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for directory in directories {
        collect(directory, 0, &mut files);
    }
    // The same file can be reached twice, through overlapping search paths
    // or a symlink. Scanning it twice would only produce duplicate plugins.
    let mut seen = HashSet::new();
    files.retain(|file| seen.insert(std::fs::canonicalize(file).unwrap_or_else(|_| file.clone())));
    files
}

fn collect(directory: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else { return };
    // Whether each entry is a directory. Plugins ship thousands of content
    // files beside them, so this comes from the directory listing where it
    // can; only symlinks cost a look at what they point to, which also
    // finds linked plugins.
    let mut entries: Vec<(PathBuf, bool)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let kind = entry.file_type().ok()?;
            let path = entry.path();
            let is_dir = if kind.is_symlink() { std::fs::metadata(&path).ok()?.is_dir() } else { kind.is_dir() };
            Some((path, is_dir))
        })
        .collect();
    // Directory order is arbitrary; sorting makes "first one wins" stable.
    entries.sort();
    for (path, is_dir) in entries {
        match (named_vst3(&path), is_dir) {
            (true, true) => match bundle_library(&path) {
                Some(library) => files.push(library),
                None => log::debug!("'{}' has no library for this platform; skipping it", path.display()),
            },
            // Single-file plugins only ever existed on Windows.
            (true, false) if cfg!(windows) => files.push(path),
            (false, true) if depth < MAX_DEPTH => collect(&path, depth + 1, files),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use std::collections::HashMap;

    fn with_env(vars: &[(&str, &str)]) -> Vec<PathBuf> {
        let vars: HashMap<String, OsString> =
            vars.iter().map(|(name, value)| (name.to_string(), OsString::from(value))).collect();
        search_paths(|name| vars.get(name).cloned())
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_follow_the_specification() {
        let paths = with_env(&[
            ("COMMONPROGRAMFILES", r"C:\Program Files\Common Files"),
            ("LOCALAPPDATA", r"C:\Users\me\AppData\Local"),
        ]);
        assert_eq!(
            paths,
            [
                PathBuf::from(r"C:\Users\me\AppData\Local\Programs\Common\VST3"),
                PathBuf::from(r"C:\Program Files\Common Files\VST3"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_paths_follow_the_specification() {
        let paths = with_env(&[("HOME", "/Users/me")]);
        assert_eq!(
            paths,
            [PathBuf::from("/Users/me/Library/Audio/Plug-Ins/VST3"), PathBuf::from("/Library/Audio/Plug-Ins/VST3")]
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_paths_follow_the_specification() {
        let paths = with_env(&[("HOME", "/home/me")]);
        assert_eq!(
            paths,
            [PathBuf::from("/home/me/.vst3"), PathBuf::from("/usr/lib/vst3"), PathBuf::from("/usr/local/lib/vst3")]
        );
    }

    #[test]
    fn unset_and_empty_variables_add_nothing() {
        let none = with_env(&[("HOME", ""), ("LOCALAPPDATA", ""), ("COMMONPROGRAMFILES", "")]);
        assert!(none.iter().all(|path| path.is_absolute()), "only fixed system paths remain: {none:?}");
        assert_eq!(none, with_env(&[]));
    }

    /// Creates a bundle named `name` with a library for this platform, and
    /// returns the library.
    fn bundle(dir: &TestDir, name: &str) -> PathBuf {
        let stem = Path::new(name).file_stem().unwrap().to_string_lossy().into_owned();
        dir.file(&format!("{name}/Contents/{}/{}", BUNDLE_LIBRARY.0, library_name(Path::new(&stem))), b"library")
    }

    #[test]
    fn a_bundle_resolves_to_the_library_of_this_platform() {
        let dir = TestDir::new();
        let library = bundle(&dir, "Vendor/Synth.vst3");
        let bundle_path = dir.path().join("Vendor").join("Synth.vst3");
        assert_eq!(bundle_library(&bundle_path), Some(library.clone()));
        assert_eq!(resolve(&bundle_path), Ok(library.clone()));
        assert_eq!(resolve(&library), Ok(library.clone()), "a library is what it is");
        assert!(is_plugin_file(&library));

        // Built for some other platform only.
        dir.file("Other.vst3/Contents/pdp11-unix/Other.so", b"library");
        dir.file("Other.vst3/Contents/Resources/moduleinfo.json", b"{}");
        let other = dir.path().join("Other.vst3");
        assert_eq!(bundle_library(&other), None);
        let error = resolve(&other).unwrap_err();
        assert!(error.contains("no library for this platform") && error.contains(BUNDLE_LIBRARY.0), "{error}");

        // The library must be named after its bundle.
        dir.file(&format!("Renamed.vst3/Contents/{}/{}", BUNDLE_LIBRARY.0, library_name(Path::new("Else"))), b"x");
        assert_eq!(bundle_library(&dir.path().join("Renamed.vst3")), None);
    }

    #[test]
    fn plugin_files_are_told_from_other_files_by_name() {
        assert!(is_plugin_file(Path::new("plugins/Old.vst3")));
        assert!(is_plugin_file(Path::new("plugins/Old.VST3")));
        assert!(is_plugin_file(Path::new("Synth.vst3/Contents/x86_64-linux/Synth.so")));
        assert!(is_plugin_file(Path::new("Synth.vst3/Contents/MacOS/Synth")));
        assert!(!is_plugin_file(Path::new("plugins/Synth.clap")));
        assert!(!is_plugin_file(Path::new("plugins/vst3/Synth.so")), "an extension is required, not just the name");
        assert!(!is_plugin_file(Path::new("Synth.vst3/Contents/Resources/Samples/deep/kick.so")));
    }

    #[test]
    fn finds_bundles_and_single_files_recursively_in_a_stable_order() {
        let dir = TestDir::new();
        let b = bundle(&dir, "b.vst3");
        let nested = bundle(&dir, "Vendor/Deep/a.VST3");
        let single = dir.file("Vendor/old.vst3", b"a library on its own");
        dir.file("Vendor/readme.txt", b"");
        dir.file("Vendor/vst3", b"an extension is required, not just the name");
        // Nothing inside a bundle is a plugin of its own.
        dir.file(&format!("b.vst3/Contents/{}/extra.vst3", BUNDLE_LIBRARY.0), b"");
        dir.file("b.vst3/Contents/Resources/inner.vst3/Contents/x/inner.vst3", b"");
        // A bundle for another platform is not found at all.
        dir.file("c.vst3/Contents/pdp11-unix/c.so", b"");

        let found = find_plugin_files(&[dir.path().to_path_buf()]);
        let expected = if cfg!(windows) { vec![nested, single, b] } else { vec![nested, b] };
        assert_eq!(found, expected, "directories are searched in sorted order");
    }

    #[test]
    fn missing_directories_are_skipped_and_overlaps_are_not_repeated() {
        let dir = TestDir::new();
        let a = bundle(&dir, "sub/a.vst3");
        let found =
            find_plugin_files(&[dir.path().join("does-not-exist"), dir.path().to_path_buf(), dir.path().join("sub")]);
        assert_eq!(found, [a]);
    }
}
