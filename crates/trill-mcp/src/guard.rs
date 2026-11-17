//! Limits on a client that works beside the user rather than for them.
//!
//! A program the user set up themselves (an MCP client pointed at the
//! server) is trusted as the user is: it reads and writes where it is told.
//! The assistant in the window is different. It acts on what it reads, and
//! some of what it reads was written by strangers: the names of sample
//! files, a MIDI file from the internet, a project somebody sent. A line of
//! text in one of those must not be able to make it overwrite a file
//! somewhere on the disk, or throw away the song the user has open.
//!
//! So a guarded client:
//!
//! * **writes only into the user's working folders**: the folder of the
//!   open project, and one folder of the program's own (`Documents/Trill`
//!   by default), and only the kind of file the tool makes. A film is the
//!   largest thing it can make, and one it gives no name goes into `films`
//!   in the first of those folders rather than wherever temporary files go
//!   (`TrillServer::write_path_or`);
//! * **cannot replace a project that has unsaved changes**: it saves
//!   first, or asks;
//! * **cannot delete** a project that was lost in a crash.
//!
//! Reading is not limited: nothing a tool reads from a file comes back
//! except as sound, notes or a refusal.
//!
//! A preset file is read the same way, from wherever it lies on this
//! machine's disks. What comes back of it is the sound, the name it goes
//! by, and where it is no preset a refusal that repeats at most a few
//! words the file says of itself; a preset is one undo step; and a file
//! that a plugin chokes on is as much a risk in a project somebody sent,
//! which carries its plugins' states. Keeping the assistant to some
//! folders would not change that: the folders it would be kept to are
//! the very ones strangers' files are in. One thing is different about a
//! path, though, and is refused: a file on **another machine**
//! (`\\server\share\...`). Opening one signs the user in to whatever
//! server the path names, with their credentials, and a path is text that
//! a stranger can supply.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use trill_engine::Session;

/// What a guarded client is held to.
#[derive(Clone, Debug)]
pub(crate) struct Guard {
    /// The program's own folder for what the assistant saves when the
    /// project has no folder yet. Made the first time it is written to.
    pub folder: PathBuf,
}

impl Guard {
    /// The folders a guarded client may write into right now: the open
    /// project's own first, since that is where its files belong.
    fn folders(&self, session: &Session) -> Vec<PathBuf> {
        let project = session.path().and_then(Path::parent).filter(|folder| !folder.as_os_str().is_empty());
        project.map(Path::to_path_buf).into_iter().chain([self.folder.clone()]).collect()
    }

    /// Where a guarded client may write the `kind` of file it asked to put
    /// at `requested`, or why not. A bare name goes into the first working
    /// folder; a name without an ending gets the first of `endings`.
    pub fn write_target(
        &self,
        session: &Session,
        requested: &str,
        kind: &str,
        endings: &[&str],
    ) -> Result<PathBuf, String> {
        let folders = self.folders(session);
        let mut path = PathBuf::from(requested);
        match path.extension().and_then(|ending| ending.to_str()).map(str::to_ascii_lowercase) {
            None => {
                path.set_extension(endings[0]);
            }
            Some(ending) if endings.contains(&ending.as_str()) => {}
            Some(ending) => {
                return Err(format!("{kind} is written as a .{} file, not .{ending}", endings[0]));
            }
        }
        let candidates: Vec<PathBuf> = match path.is_absolute() {
            true => vec![path.clone()],
            false => vec![folders[0].join(&path)],
        };
        for folder in &folders {
            for candidate in &candidates {
                if let Some(inside) = inside(folder, candidate) {
                    return Ok(inside);
                }
            }
        }
        let places: Vec<String> = folders.iter().map(|folder| folder.display().to_string()).collect();
        Err(format!(
            "from here {kind} can only be written inside {}. Give a file name, or a path inside one of those; \
             '{requested}' is somewhere else. The user can save anywhere from the window.",
            places.join(" or ")
        ))
    }

    /// Whether a guarded client may have the preset file at `path` read:
    /// any file on this machine's own disks, and none on another machine.
    /// See the module documentation for why.
    pub fn may_read_preset(&self, path: &Path) -> Result<(), String> {
        // A link is read, not followed: following it is what would reach
        // the other machine.
        let leads_away = path.ancestors().any(|step| {
            let link = std::fs::symlink_metadata(step).is_ok_and(|found| found.file_type().is_symlink());
            link && std::fs::read_link(step).is_ok_and(|target| on_another_machine(&target))
        });
        if on_another_machine(path) || leads_away {
            return Err(format!(
                "'{}' is not on one of this machine's own disks. From here a preset file is only read from those: \
                 opening a file on a network share signs the user in to the machine the path names. The user can \
                 copy the file to a folder of theirs, or load it from the window.",
                path.display()
            ));
        }
        Ok(())
    }

    /// Refuses to replace the open project while it has unsaved changes.
    pub fn may_replace_project(&self, session: &Session) -> Result<(), String> {
        if session.is_modified() {
            return Err(format!(
                "'{}' has unsaved changes, and opening another project would lose them. Save it first with \
                 save_project, or ask the user what to do.",
                session.project().name
            ));
        }
        Ok(())
    }
}

/// Whether `path` names a place that is not a disk of this machine: a
/// share on another one, or a device. A drive letter is this machine's,
/// whatever the user has mapped it to: that they did themselves.
fn on_another_machine(path: &Path) -> bool {
    use std::path::Prefix;
    // What a link says of where it leads may begin the way the system
    // itself writes paths.
    let text = path.to_string_lossy();
    let plain = text.strip_prefix(r"\??\").unwrap_or(&text);
    if plain.len() >= 4 && plain[..4].eq_ignore_ascii_case(r"UNC\") {
        return true;
    }
    match Path::new(plain).components().next() {
        Some(Component::Prefix(prefix)) => !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)),
        // Two slashes in front are a share's name on every system that
        // has shares, however its paths are otherwise written.
        _ => plain.starts_with(r"\\") || plain.starts_with("//"),
    }
}

/// `path` as it really lies on the disk, if that is inside `folder`.
///
/// Climbing out with `..` is caught without touching the disk. Links are
/// caught by asking the disk where the part of the path that exists really
/// is; the part that does not exist yet cannot be a link.
fn inside(folder: &Path, path: &Path) -> Option<PathBuf> {
    let path = without_dots(path)?;
    std::fs::create_dir_all(folder).ok()?;
    let folder = folder.canonicalize().ok()?;
    let mut existing = path.as_path();
    let mut missing: Vec<OsString> = Vec::new();
    while !existing.exists() {
        missing.push(existing.file_name()?.to_owned());
        existing = existing.parent()?;
    }
    let mut real = existing.canonicalize().ok()?;
    real.extend(missing.iter().rev());
    real.starts_with(&folder).then(|| plain(&real))
}

/// `path` with `.` dropped and `..` applied, or `None` if `..` would climb
/// above where the path starts.
fn without_dots(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    return None;
                }
                out.pop();
            }
            other => out.push(other),
        }
    }
    Some(out)
}

/// A path without the `\\?\` that Windows puts in front of the real
/// location of a file: correct, and nothing anyone wants to read.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// The folder a guarded client gets when it is not told another:
/// `Documents/Trill` in the user's home folder.
pub(crate) fn default_folder() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join("Documents").join("Trill"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_file_is_read_from_this_machines_disks_and_no_other() {
        let guard = Guard { folder: PathBuf::from("work") };
        let folder = tempfile::tempdir().unwrap();
        // Wherever it lies on a disk: the working folders are not the limit.
        let file = folder.path().join("Bought.SerumPreset");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(guard.may_read_preset(&file), Ok(()));
        assert_eq!(guard.may_read_preset(&folder.path().join("gone").join("Lead.vstpreset")), Ok(()));
        if cfg!(windows) {
            assert_eq!(guard.may_read_preset(Path::new(r"D:\Samples\Pack\Lead.vstpreset")), Ok(()));
            assert_eq!(guard.may_read_preset(Path::new(r"\\?\D:\Samples\Lead.vstpreset")), Ok(()));
        }
        // A share on another machine, however it is written.
        let shares = [
            r"\\files.example\presets\Lead.vstpreset",
            "//files.example/presets/Lead.vstpreset",
            r"\\?\UNC\files.example\presets\Lead.vstpreset",
            r"\\.\pipe\presets",
        ];
        for share in shares {
            let refusal = guard.may_read_preset(Path::new(share)).unwrap_err();
            assert!(refusal.starts_with(&format!("'{share}' is not on one of this machine's own disks")), "{refusal}");
            assert!(refusal.contains("signs the user in"), "{refusal}");
        }
        assert!(on_another_machine(Path::new(r"\??\UNC\files.example\presets")));
        assert!(!on_another_machine(Path::new(r"\??\C:\Samples")) || !cfg!(windows));
        assert!(!on_another_machine(Path::new("Pack/Lead.vstpreset")));
    }

    /// A link on this machine's disk that leads to another machine is
    /// where it leads. Making one needs a privilege Windows does not
    /// always grant; without it there is nothing to test.
    #[test]
    fn a_link_to_another_machine_is_not_followed() {
        let folder = tempfile::tempdir().unwrap();
        let link = folder.path().join("shared");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(r"\\files.example\presets", &link);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink("//files.example/presets", &link);
        if let Err(error) = linked {
            eprintln!("SKIPPED: this machine does not let the test make a link ({error})");
            return;
        }
        let guard = Guard { folder: PathBuf::from("work") };
        let refusal = guard.may_read_preset(&link.join("Lead.vstpreset")).unwrap_err();
        assert!(refusal.contains("is not on one of this machine's own disks"), "{refusal}");
    }

    #[test]
    fn dots_are_applied_and_climbing_out_is_refused() {
        let path = |text: &str| without_dots(Path::new(text)).map(|path| path.to_string_lossy().replace('\\', "/"));
        assert_eq!(path("a/./b/../c.wav").as_deref(), Some("a/c.wav"));
        assert_eq!(path("a/b/../../c.wav").as_deref(), Some("c.wav"));
        assert_eq!(path("../c.wav"), None);
        assert_eq!(path("a/../../c.wav"), None);
    }

    #[test]
    fn only_what_really_lies_inside_the_folder_is_inside_it() {
        let root = tempfile::tempdir().unwrap();
        let (work, other) = (root.path().join("work"), root.path().join("other"));
        std::fs::create_dir_all(&other).unwrap();

        // New files, in the folder and in folders that do not exist yet.
        let song = inside(&work, &work.join("song.trill")).expect("a new file in the folder");
        assert!(song.ends_with("work/song.trill") || song.ends_with(r"work\song.trill"), "{song:?}");
        assert!(!song.to_string_lossy().starts_with(r"\\?\"), "{song:?}");
        assert!(inside(&work, &work.join("bounces").join("deep").join("mix.wav")).is_some());
        // An existing file.
        std::fs::write(work.join("old.trill"), "x").unwrap();
        assert!(inside(&work, &work.join("old.trill")).is_some());

        // Beside it, above it, and out through `..`.
        assert_eq!(inside(&work, &other.join("song.trill")), None);
        assert_eq!(inside(&work, &root.path().join("song.trill")), None);
        assert_eq!(inside(&work, &work.join("..").join("other").join("song.trill")), None);
        assert_eq!(inside(&work, &work.join("a").join("..").join("..").join("song.trill")), None);
        // A folder whose name merely starts the same way.
        std::fs::create_dir_all(root.path().join("work-2")).unwrap();
        assert_eq!(inside(&work, &root.path().join("work-2").join("song.trill")), None);
    }

    /// A link inside the folder that points out of it leads out of it.
    /// Making one needs a privilege Windows does not always grant; without
    /// it there is nothing to test, and the test says so.
    #[test]
    fn a_link_out_of_the_folder_is_followed_and_refused() {
        let root = tempfile::tempdir().unwrap();
        let (work, other) = (root.path().join("work"), root.path().join("other"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&other, work.join("out"));
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&other, work.join("out"));
        if let Err(error) = linked {
            eprintln!("SKIPPED: this machine does not let the test make a link ({error})");
            return;
        }
        assert_eq!(inside(&work, &work.join("out").join("song.trill")), None);
        assert_eq!(inside(&work, &work.join("out").join("new").join("song.trill")), None);
    }
}
