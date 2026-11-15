//! Where CLAP plugins are installed, per the CLAP specification
//! (`clap/entry.h`), and how to find the plugin files below those places.

use super::PLUGIN_EXTENSION;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment variable with extra search directories, separated like
/// `PATH`.
const CLAP_PATH: &str = "CLAP_PATH";

/// Deepest directory nesting searched below a search path. Vendors use one
/// or two levels; the limit only stops symlink cycles.
const MAX_DEPTH: usize = 12;

/// The directories a CLAP host must search on this system, most specific
/// first: `CLAP_PATH`, then the user's plugins, then the system's. When a
/// plugin is installed in several places the first one wins.
pub fn standard_search_paths() -> Vec<PathBuf> {
    search_paths(|name| std::env::var_os(name))
}

fn search_paths(env: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    // An unset and an empty variable mean the same thing.
    let var = |name: &str| env(name).filter(|value| !value.is_empty());
    let mut paths: Vec<PathBuf> = Vec::new();

    if let Some(extra) = var(CLAP_PATH) {
        paths.extend(std::env::split_paths(&extra).filter(|path| !path.as_os_str().is_empty()));
    }
    platform_paths(&var, &mut paths);

    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}

#[cfg(windows)]
fn platform_paths(var: &dyn Fn(&str) -> Option<OsString>, paths: &mut Vec<PathBuf>) {
    if let Some(local) = var("LOCALAPPDATA") {
        paths.push(PathBuf::from(local).join("Programs").join("Common").join("CLAP"));
    }
    if let Some(common) = var("COMMONPROGRAMFILES") {
        paths.push(PathBuf::from(common).join("CLAP"));
    }
}

#[cfg(target_os = "macos")]
fn platform_paths(var: &dyn Fn(&str) -> Option<OsString>, paths: &mut Vec<PathBuf>) {
    if let Some(home) = var("HOME") {
        paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/CLAP"));
    }
    paths.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_paths(var: &dyn Fn(&str) -> Option<OsString>, paths: &mut Vec<PathBuf>) {
    if let Some(home) = var("HOME") {
        paths.push(PathBuf::from(home).join(".clap"));
    }
    paths.push(PathBuf::from("/usr/lib/clap"));
}

/// Every CLAP plugin file below `directories`, in search order. Vendors
/// nest plugins in subdirectories, so the search is recursive. Directories
/// that do not exist or cannot be read are skipped.
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
        let named_clap = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case(PLUGIN_EXTENSION));
        if named_clap && is_dir == PLUGIN_IS_DIRECTORY {
            files.push(path);
        } else if is_dir && depth < MAX_DEPTH {
            collect(&path, depth + 1, files);
        }
    }
}

/// On macOS a plugin is a bundle: a directory named `*.clap`. Elsewhere it
/// is a single library file.
const PLUGIN_IS_DIRECTORY: bool = cfg!(target_os = "macos");

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
            ("CLAP_PATH", r"D:\Plugins;E:\More"),
        ]);
        assert_eq!(
            paths,
            [
                PathBuf::from(r"D:\Plugins"),
                PathBuf::from(r"E:\More"),
                PathBuf::from(r"C:\Users\me\AppData\Local\Programs\Common\CLAP"),
                PathBuf::from(r"C:\Program Files\Common Files\CLAP"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_paths_follow_the_specification() {
        let paths = with_env(&[("HOME", "/Users/me"), ("CLAP_PATH", "/opt/clap:/extra")]);
        assert_eq!(
            paths,
            [
                PathBuf::from("/opt/clap"),
                PathBuf::from("/extra"),
                PathBuf::from("/Users/me/Library/Audio/Plug-Ins/CLAP"),
                PathBuf::from("/Library/Audio/Plug-Ins/CLAP"),
            ]
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_paths_follow_the_specification() {
        let paths = with_env(&[("HOME", "/home/me"), ("CLAP_PATH", "/opt/clap:/extra")]);
        assert_eq!(
            paths,
            [
                PathBuf::from("/opt/clap"),
                PathBuf::from("/extra"),
                PathBuf::from("/home/me/.clap"),
                PathBuf::from("/usr/lib/clap"),
            ]
        );
    }

    #[test]
    fn unset_and_empty_variables_add_nothing_and_duplicates_collapse() {
        let none = with_env(&[("CLAP_PATH", "")]);
        assert!(none.iter().all(|path| path.is_absolute()), "only fixed system paths remain: {none:?}");

        let separator = if cfg!(windows) { ";" } else { ":" };
        let twice = with_env(&[("CLAP_PATH", &["/same", "/same", ""].join(separator))]);
        assert_eq!(twice.iter().filter(|path| *path == Path::new("/same")).count(), 1);
    }

    /// Creates something `find_plugin_files` accepts as a plugin.
    fn plugin(dir: &TestDir, name: &str) -> PathBuf {
        if cfg!(target_os = "macos") {
            let path = dir.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        } else {
            dir.file(name, b"not really a plugin")
        }
    }

    #[test]
    fn finds_plugins_recursively_in_a_stable_order() {
        let dir = TestDir::new();
        let b = plugin(&dir, "b.clap");
        let nested = plugin(&dir, "Vendor/Deep/a.CLAP");
        dir.file("Vendor/readme.txt", b"");
        dir.file("Vendor/clap", b"an extension is required, not just the name");

        let found = find_plugin_files(&[dir.path().to_path_buf()]);
        assert_eq!(found, [nested, b], "directories are searched in sorted order");
    }

    #[test]
    fn missing_directories_are_skipped_and_overlaps_are_not_repeated() {
        let dir = TestDir::new();
        let a = plugin(&dir, "sub/a.clap");
        let found =
            find_plugin_files(&[dir.path().join("does-not-exist"), dir.path().to_path_buf(), dir.path().join("sub")]);
        assert_eq!(found, [a]);
    }
}
