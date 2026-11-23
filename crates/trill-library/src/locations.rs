//! Where sample libraries usually are, so that a new user can be offered
//! the folders they already have instead of an empty browser.
//!
//! Nothing here adds a folder to the library. The program shows the list
//! and the user chooses.

use std::path::{Path, PathBuf};

/// A folder on this machine that probably holds sounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestedLocation {
    /// What the folder is, in words: `Splice`, `Ableton Factory Packs`.
    pub label: String,
    pub path: PathBuf,
}

/// The well-known sample folders that exist on this machine: Splice's
/// sounds, Ableton Live's libraries and packs, FL Studio's packs, and the
/// places people keep their own samples.
pub fn suggested_locations() -> Vec<SuggestedLocation> {
    existing(candidates(&Places::from_environment()))
}

/// The folders candidates are looked for in.
#[derive(Clone, Debug, Default)]
struct Places {
    home: Option<PathBuf>,
    /// Where programs are installed: `Program Files` and its 32-bit twin on
    /// Windows, `/Applications` on macOS. Linux's programs keep no samples
    /// of their own in a place of their own.
    #[cfg_attr(not(any(windows, target_os = "macos", test)), allow(dead_code))]
    programs: Vec<PathBuf>,
    /// Windows' `ProgramData`.
    #[cfg_attr(not(any(windows, target_os = "macos", test)), allow(dead_code))]
    program_data: Option<PathBuf>,
}

impl Places {
    fn from_environment() -> Self {
        let variable = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute());
        if cfg!(windows) {
            Self {
                home: variable("USERPROFILE"),
                programs: ["ProgramFiles", "ProgramFiles(x86)"].into_iter().filter_map(variable).collect(),
                program_data: variable("ProgramData"),
            }
        } else if cfg!(target_os = "macos") {
            Self { home: variable("HOME"), programs: vec![PathBuf::from("/Applications")], program_data: None }
        } else {
            Self { home: variable("HOME"), programs: Vec::new(), program_data: None }
        }
    }
}

fn candidates(places: &Places) -> Vec<SuggestedLocation> {
    #[cfg(windows)]
    return windows(places);
    #[cfg(target_os = "macos")]
    return macos(places);
    #[cfg(not(any(windows, target_os = "macos")))]
    return linux(places);
}

fn suggestion(label: impl Into<String>, path: PathBuf) -> SuggestedLocation {
    SuggestedLocation { label: label.into(), path }
}

/// The folders of `parent` whose names start with `prefix`, sorted, with
/// their names. Programs install under names that carry a version ("FL
/// Studio 2024", "Live 12 Suite"), so they are found by what they start
/// with.
#[cfg_attr(not(any(windows, target_os = "macos", test)), allow(dead_code))]
fn versions(parent: &Path, prefix: &str) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|item| Some((item.file_name().to_str()?.to_string(), item.path())))
        .filter(|(name, path)| name.starts_with(prefix) && path.is_dir())
        .collect();
    found.sort();
    found
}

/// Where people keep samples of their own, under the folders `bases`.
fn personal(bases: &[PathBuf]) -> Vec<SuggestedLocation> {
    const NAMES: &[&str] = &["Samples", "Sample Packs", "Sample Library", "Sounds", "Loops"];
    bases
        .iter()
        .flat_map(|base| {
            NAMES.iter().map(move |name| suggestion(format!("Your {}", name.to_lowercase()), base.join(name)))
        })
        .collect()
}

#[cfg(any(windows, test))]
fn windows(places: &Places) -> Vec<SuggestedLocation> {
    let mut found = Vec::new();
    // Documents and Music are under the profile, or under OneDrive when it
    // has been given charge of them.
    let (mut documents, mut music) = (Vec::new(), Vec::new());
    if let Some(home) = &places.home {
        for base in [home.clone(), home.join("OneDrive")] {
            documents.push(base.join("Documents"));
            music.push(base.join("Music"));
        }
        for documents in &documents {
            found.push(suggestion("Splice", documents.join("Splice")));
        }
        found.push(suggestion("Splice", home.join("Splice")));
        for documents in &documents {
            found.push(suggestion("Ableton User Library", documents.join("Ableton").join("User Library")));
            found.push(suggestion("Ableton Factory Packs", documents.join("Ableton").join("Factory Packs")));
        }
    }
    if let Some(program_data) = &places.program_data {
        for (name, live) in versions(&program_data.join("Ableton"), "Live ") {
            found.push(suggestion(format!("Ableton {name} Core Library"), live.join("Resources").join("Core Library")));
        }
    }
    for programs in &places.programs {
        for (name, fl) in versions(&programs.join("Image-Line"), "FL Studio") {
            found.push(suggestion(format!("{name} Packs"), fl.join("Data").join("Patches").join("Packs")));
        }
    }
    if let Some(home) = &places.home {
        let bases: Vec<PathBuf> = music.into_iter().chain(documents).chain([home.clone()]).collect();
        found.extend(personal(&bases));
    }
    found
}

#[cfg(any(target_os = "macos", test))]
fn macos(places: &Places) -> Vec<SuggestedLocation> {
    let mut found = Vec::new();
    if let Some(home) = &places.home {
        found.push(suggestion("Splice", home.join("Splice")));
        found.push(suggestion("Splice", home.join("Documents").join("Splice")));
        let ableton = home.join("Music").join("Ableton");
        found.push(suggestion("Ableton User Library", ableton.join("User Library")));
        found.push(suggestion("Ableton Factory Packs", ableton.join("Factory Packs")));
    }
    for programs in &places.programs {
        for (name, live) in versions(programs, "Ableton Live ") {
            let name = name.trim_end_matches(".app");
            found.push(suggestion(format!("{name} Core Library"), live.join("Contents/App-Resources/Core Library")));
        }
        for (name, fl) in versions(programs, "FL Studio") {
            let name = name.trim_end_matches(".app");
            found.push(suggestion(format!("{name} Packs"), fl.join("Contents/Resources/FL/Data/Patches/Packs")));
        }
    }
    found.push(suggestion("Apple Loops", PathBuf::from("/Library/Audio/Apple Loops")));
    if let Some(home) = &places.home {
        found.extend(personal(&[home.join("Music"), home.join("Documents"), home.clone()]));
    }
    found
}

#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn linux(places: &Places) -> Vec<SuggestedLocation> {
    let mut found = Vec::new();
    if let Some(home) = &places.home {
        found.push(suggestion("Splice", home.join("Splice")));
        found.push(suggestion("Bitwig Studio Library", home.join("Bitwig Studio").join("Library")));
        // Lower-case folder names are as common as capitalised ones here,
        // and are offered beside them, base by base.
        for base in [home.join("Music"), home.join("Documents"), home.clone()] {
            found.extend(personal(std::slice::from_ref(&base)));
            found.push(suggestion("Your samples", base.join("samples")));
        }
    }
    found
}

/// Keeps the candidates that are folders on this machine, once each. Two
/// candidates are the same folder if they resolve to the same place, which
/// takes in spellings that differ only in case where case does not matter,
/// and links.
fn existing(candidates: Vec<SuggestedLocation>) -> Vec<SuggestedLocation> {
    let mut found: Vec<SuggestedLocation> = Vec::new();
    let mut resolved: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        // Resolving fails for what is not there, so it is the existence
        // check as well.
        let Ok(place) = std::fs::canonicalize(&candidate.path) else { continue };
        if place.is_dir() && !resolved.contains(&place) {
            resolved.push(place);
            found.push(candidate);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates the folders `paths` under a temporary folder.
    fn machine(paths: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for path in paths {
            std::fs::create_dir_all(root.path().join(path)).unwrap();
        }
        root
    }

    fn found(root: &Path, candidates: Vec<SuggestedLocation>) -> Vec<(String, String)> {
        existing(candidates)
            .into_iter()
            .map(|location| {
                let relative = location.path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                (location.label, relative)
            })
            .collect()
    }

    fn pair(label: &str, path: &str) -> (String, String) {
        (label.to_string(), path.to_string())
    }

    #[test]
    fn a_windows_machine() {
        let root = machine(&[
            "Users/me/Documents/Splice/Samples/packs",
            "Users/me/Documents/Ableton/User Library",
            "Users/me/Documents/Ableton/Factory Packs",
            "Users/me/Documents/Ableton/Projects",
            "Users/me/OneDrive/Music/Samples",
            "Users/me/Music",
            "ProgramData/Ableton/Live 12 Suite/Resources/Core Library",
            "ProgramData/Ableton/Live 12 Intro/Resources/Core Library",
            "ProgramData/Ableton/.Live 12 Trial_updated/Resources/Core Library",
            "ProgramData/Ableton/Live 11 Lite/Resources",
            "Program Files/Image-Line/FL Studio 2024/Data/Patches/Packs",
            "Program Files/Image-Line/FL Studio ASIO",
            "Program Files/Image-Line/Shared",
        ]);
        let places = Places {
            home: Some(root.path().join("Users/me")),
            programs: vec![root.path().join("Program Files"), root.path().join("Program Files (x86)")],
            program_data: Some(root.path().join("ProgramData")),
        };
        assert_eq!(
            found(root.path(), windows(&places)),
            [
                pair("Splice", "Users/me/Documents/Splice"),
                pair("Ableton User Library", "Users/me/Documents/Ableton/User Library"),
                pair("Ableton Factory Packs", "Users/me/Documents/Ableton/Factory Packs"),
                pair("Ableton Live 12 Intro Core Library", "ProgramData/Ableton/Live 12 Intro/Resources/Core Library"),
                pair("Ableton Live 12 Suite Core Library", "ProgramData/Ableton/Live 12 Suite/Resources/Core Library"),
                pair("FL Studio 2024 Packs", "Program Files/Image-Line/FL Studio 2024/Data/Patches/Packs"),
                pair("Your samples", "Users/me/OneDrive/Music/Samples"),
            ]
        );
    }

    #[test]
    fn a_mac() {
        let root = machine(&[
            "Users/me/Splice/sounds",
            "Users/me/Music/Ableton/User Library",
            "Users/me/Music/Ableton/Factory Packs",
            "Users/me/Music/Sample Packs",
            "Applications/Ableton Live 12 Suite.app/Contents/App-Resources/Core Library",
            "Applications/FL Studio 2024.app/Contents/Resources/FL/Data/Patches/Packs",
            "Applications/Logic Pro.app",
        ]);
        let places = Places {
            home: Some(root.path().join("Users/me")),
            programs: vec![root.path().join("Applications")],
            program_data: None,
        };
        // The system-wide Apple Loops folder is outside the pretend machine;
        // whether it exists depends on where the test runs.
        let inside: Vec<SuggestedLocation> =
            macos(&places).into_iter().filter(|location| location.path.starts_with(root.path())).collect();
        assert_eq!(
            found(root.path(), inside),
            [
                pair("Splice", "Users/me/Splice"),
                pair("Ableton User Library", "Users/me/Music/Ableton/User Library"),
                pair("Ableton Factory Packs", "Users/me/Music/Ableton/Factory Packs"),
                pair(
                    "Ableton Live 12 Suite Core Library",
                    "Applications/Ableton Live 12 Suite.app/Contents/App-Resources/Core Library"
                ),
                pair(
                    "FL Studio 2024 Packs",
                    "Applications/FL Studio 2024.app/Contents/Resources/FL/Data/Patches/Packs"
                ),
                pair("Your sample packs", "Users/me/Music/Sample Packs"),
            ]
        );
    }

    #[test]
    fn a_linux_machine() {
        let root =
            machine(&["home/me/Music/samples", "home/me/Bitwig Studio/Library", "home/me/Documents", "home/me/Loops"]);
        let places = Places { home: Some(root.path().join("home/me")), programs: Vec::new(), program_data: None };
        let found = found(root.path(), linux(&places));
        // On file systems that ignore case, "Samples" and "samples" are one
        // folder, offered once under whichever spelling was tried first.
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(found[0], pair("Bitwig Studio Library", "home/me/Bitwig Studio/Library"));
        assert!(found[1].1.eq_ignore_ascii_case("home/me/Music/samples"));
        assert_eq!(found[2], pair("Your loops", "home/me/Loops"));
    }

    #[test]
    fn nothing_known_about_the_machine_means_no_suggestions() {
        let nowhere = Places::default();
        assert!(existing(windows(&nowhere)).is_empty());
        assert!(existing(linux(&nowhere)).is_empty());
        let nothing_there =
            Places { home: Some(PathBuf::from("/no/such/home")), programs: Vec::new(), program_data: None };
        assert!(existing(windows(&nothing_there)).is_empty());
        assert!(existing(macos(&nothing_there)).iter().all(|location| location.label == "Apple Loops"));
    }

    #[test]
    fn this_machine() {
        // Whatever is here, every suggestion must be a real folder, listed
        // once. Shown for the record of what a test run found.
        let suggestions = suggested_locations();
        for suggestion in &suggestions {
            println!("{}: {}", suggestion.label, suggestion.path.display());
            assert!(suggestion.path.is_dir() && !suggestion.label.is_empty());
        }
        let mut paths: Vec<&PathBuf> = suggestions.iter().map(|suggestion| &suggestion.path).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), suggestions.len());
    }
}
