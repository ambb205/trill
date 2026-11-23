//! The desktop's trash on Linux and the other systems of freedesktop.org:
//! a file goes there as the Trash specification has it, so that the file
//! manager shows it and gives it back.
//!
//! A file on the disk the user's home folder is on goes to the home trash
//! (`$XDG_DATA_HOME/Trash`, which is `~/.local/share/Trash` unless the
//! environment says otherwise). One on another disk goes to that disk's
//! own: `.Trash/<uid>` at its top where the system made a `.Trash` for every
//! user (a folder with the sticky bit, not a link), and `.Trash-<uid>`
//! otherwise. In each, the file goes into `files` and a note of where it was
//! and when into `info`, under the same name; the note is written first and
//! exclusively, so that no two take one name, and the file is only ever
//! renamed into place: never copied, and never deleted. What cannot be
//! renamed there (a disk whose trash cannot be made) is said, and the file
//! is where it was.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Takes `path` into the trash of the disk it is on, and says where it is
/// now.
pub(super) fn trash(path: &Path) -> io::Result<PathBuf> {
    let home = home_trash().ok_or_else(|| io::Error::other("there is no home folder to find the trash by"))?;
    // SAFETY: getuid cannot fail and touches nothing.
    let uid = unsafe { libc::getuid() };
    trash_with(path, &home, uid)
}

/// Where the home trash is: `$XDG_DATA_HOME/Trash`, or
/// `~/.local/share/Trash`.
fn home_trash() -> Option<PathBuf> {
    let absolute = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|path| path.is_absolute());
    let data = absolute("XDG_DATA_HOME").or_else(|| Some(absolute("HOME")?.join(".local").join("share")))?;
    Some(data.join("Trash"))
}

/// [`trash`] with the home trash and the user said: what a test gives.
pub(super) fn trash_with(path: &Path, home: &Path, uid: u32) -> io::Result<PathBuf> {
    let path = resolved(path)?;
    let disk = fs::symlink_metadata(&path)?.dev();
    // The home trash where it is on the same disk (made if it is not
    // there yet: its parent is the user's own).
    if disk_of(home) == Some(disk) {
        let trash = Trash::made(home.to_path_buf())?;
        return trash.take(&path, &path);
    }
    // The disk's own: from the top of the disk the file is on.
    let top = top_of(&path, disk)?;
    let shared = top.join(".Trash");
    let usable = fs::symlink_metadata(&shared)
        .is_ok_and(|found| found.file_type().is_dir() && found.mode() & libc::S_ISVTX != 0);
    let mut tried = Vec::new();
    if usable {
        match Trash::made(shared.join(uid.to_string())) {
            Ok(trash) => return trash.take(&path, path.strip_prefix(&top).unwrap_or(&path)),
            Err(error) => tried.push(error),
        }
    }
    match Trash::made(top.join(format!(".Trash-{uid}"))) {
        Ok(trash) => trash.take(&path, path.strip_prefix(&top).unwrap_or(&path)),
        Err(error) => Err(tried.pop().unwrap_or(error)),
    }
}

/// `path` with its folder as it really lies: a recording reached through a
/// linked folder is where the link leads, and its disk's top is found from
/// there. The file itself is taken as it is (a link that is removed goes to
/// the trash as a link).
fn resolved(path: &Path) -> io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    let name = path.file_name().ok_or_else(|| io::Error::other("a path without a name"))?;
    let folder = path.parent().ok_or_else(|| io::Error::other("a path without a folder"))?;
    Ok(fs::canonicalize(folder)?.join(name))
}

/// The disk `path` (or the nearest folder of it that is there) is on.
fn disk_of(path: &Path) -> Option<u64> {
    path.ancestors().find_map(|step| fs::metadata(step).ok()).map(|found| found.dev())
}

/// The top of the disk `path` is on: its highest folder on the same disk.
fn top_of(path: &Path, disk: u64) -> io::Result<PathBuf> {
    let mut top = path.parent().ok_or_else(|| io::Error::other("a file at the root has no disk of its own"))?;
    while let Some(parent) = top.parent() {
        if fs::metadata(parent)?.dev() != disk {
            break;
        }
        top = parent;
    }
    Ok(top.to_path_buf())
}

/// A trash folder: its `files` and its `info`.
struct Trash {
    root: PathBuf,
}

impl Trash {
    /// The trash at `root`, its two folders made where they are not there
    /// yet, for the user alone. A link is not taken for one.
    fn made(root: PathBuf) -> io::Result<Self> {
        for folder in [root.clone(), root.join("files"), root.join("info")] {
            match fs::symlink_metadata(&folder) {
                Ok(found) if found.file_type().is_dir() => {}
                Ok(_) => return Err(io::Error::other(format!("'{}' is not a folder", folder.display()))),
                Err(_) => fs::DirBuilder::new().mode(0o700).recursive(true).create(&folder)?,
            }
        }
        Ok(Self { root })
    }

    /// Moves `path` into the trash, with a note that it was at `said` (the
    /// whole path in the home trash, from the disk's top in a disk's own).
    fn take(&self, path: &Path, said: &Path) -> io::Result<PathBuf> {
        let name = path.file_name().ok_or_else(|| io::Error::other("a path without a name"))?;
        for number in 1u32..10_000 {
            let candidate = named(name, number);
            let info = self.root.join("info").join(format!("{}.trashinfo", candidate.to_string_lossy()));
            let into = self.root.join("files").join(&candidate);
            if fs::symlink_metadata(&into).is_ok() {
                continue;
            }
            // The note first, and only if no other has that name: it is
            // what holds the name.
            let mut note = match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info) {
                Ok(note) => note,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let written = note.write_all(note_of(said, &now()).as_bytes()).and_then(|()| note.sync_all());
            match written.and_then(|()| rename_new(path, &into)) {
                Ok(()) => return Ok(into),
                // What appeared there meanwhile is left as it is, and the
                // next name is tried.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_file(&info);
                }
                Err(error) => {
                    let _ = fs::remove_file(&info);
                    return Err(error);
                }
            }
        }
        Err(io::Error::other("the trash has no free name for it"))
    }
}

/// Renames `from` to `to` where nothing is at `to`, and never replaces what
/// is: `renameat2` with `RENAME_NOREPLACE`, and where the file system does
/// not know the flag, a look and a rename as close together as can be.
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    let c = |path: &Path| std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other);
    let (from_c, to_c) = (c(from)?, c(to)?);
    // SAFETY: two strings that end, relative to nothing (they are whole
    // paths), and a flag.
    let said = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if said == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOSYS) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            fs::rename(from, to)
        }
        _ => Err(error),
    }
}

/// `name` for the `number`th try: `Take.wav`, then `Take.2.wav` and on.
fn named(name: &OsStr, number: u32) -> std::ffi::OsString {
    if number == 1 {
        return name.to_os_string();
    }
    let path = Path::new(name);
    let stem = path.file_stem().unwrap_or(name).to_string_lossy();
    match path.extension() {
        Some(extension) => format!("{stem}.{number}.{}", extension.to_string_lossy()).into(),
        None => format!("{stem}.{number}").into(),
    }
}

/// What the note of a trashed file says: where it was, every byte that is
/// not a letter, a digit, `/` or one of `-_.~` written as `%XX`, and when,
/// in the local time without a zone, as the specification has it.
fn note_of(said: &Path, when: &str) -> String {
    let mut escaped = String::new();
    for &byte in said.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("[Trash Info]\nPath={escaped}\nDeletionDate={when}\n")
}

/// The local time now, as `2026-11-23T08:14:28`.
fn now() -> String {
    // SAFETY: `time` with a null pointer only returns; `localtime_r` fills
    // in the structure it is given, from a time it is given.
    unsafe {
        let seconds = libc::time(std::ptr::null_mut());
        let mut local: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut local).is_null() {
            return "1970-01-01T00:00:00".to_string();
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            local.tm_year + 1900,
            local.tm_mon + 1,
            local.tm_mday,
            local.tm_hour,
            local.tm_min,
            local.tm_sec
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn a_file_goes_into_the_home_trash_with_a_note_of_where_it_was() {
        let disk = tempfile::tempdir().unwrap();
        let home = disk.path().join("data").join("Trash");
        let take = disk.path().join("takes").join("Voice 1 & 2%.wav");
        fs::create_dir_all(take.parent().unwrap()).unwrap();
        fs::write(&take, b"RIFF").unwrap();

        let went = trash_with(&take, &home, 1000).unwrap();
        assert_eq!(went, home.join("files").join("Voice 1 & 2%.wav"));
        assert!(!take.exists() && read(&went) == "RIFF");
        let note = read(&home.join("info").join("Voice 1 & 2%.wav.trashinfo"));
        let lines: Vec<&str> = note.lines().collect();
        assert_eq!(lines[0], "[Trash Info]");
        assert_eq!(lines[1], format!("Path={}/takes/Voice%201%20%26%202%25.wav", disk.path().display()));
        assert!(lines[2].starts_with("DeletionDate=20") && lines[2].len() == "DeletionDate=2026-11-23T08:14:28".len());
        // The trash is the user's alone.
        assert_eq!(fs::metadata(home.join("files")).unwrap().mode() & 0o777, 0o700);

        // A second file of the same name takes another, and the first is left as it is.
        fs::write(&take, b"RIFF again").unwrap();
        let second = trash_with(&take, &home, 1000).unwrap();
        assert_eq!(second, home.join("files").join("Voice 1 & 2%.2.wav"));
        assert!(home.join("info").join("Voice 1 & 2%.2.wav.trashinfo").exists());
        assert_eq!(read(&went), "RIFF");
    }

    #[test]
    fn a_file_that_cannot_be_moved_is_where_it_was_and_leaves_no_note() {
        let disk = tempfile::tempdir().unwrap();
        let home = disk.path().join("Trash");
        let gone = disk.path().join("gone.wav");
        assert!(trash_with(&gone, &home, 1000).is_err());
        // A trash that is a file is no trash.
        let blocked = disk.path().join("blocked");
        fs::write(&blocked, b"").unwrap();
        let take = disk.path().join("take.wav");
        fs::write(&take, b"RIFF").unwrap();
        assert!(trash_with(&take, &blocked, 1000).is_err());
        assert!(take.exists());
    }

    #[test]
    fn a_recording_reached_through_a_linked_folder_is_where_the_link_leads() {
        let disk = tempfile::tempdir().unwrap();
        let real = disk.path().join("real");
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, disk.path().join("music")).unwrap();
        let through = disk.path().join("music").join("take.wav");
        fs::write(&through, b"RIFF").unwrap();
        let found = resolved(&through).unwrap();
        assert_eq!(found, fs::canonicalize(&real).unwrap().join("take.wav"));
        // Its disk's top is found from where it really is, never inside the link.
        let top = top_of(&found, fs::metadata(&found).unwrap().dev()).unwrap();
        assert!(!top.starts_with(disk.path().join("music")), "{}", top.display());
        assert!(found.starts_with(&top));
        // And it goes to the trash from there, with a note of where it really was.
        let home = disk.path().join("Trash");
        let went = trash_with(&through, &home, 1000).unwrap();
        assert!(!through.exists() && went.exists());
        let note = read(&home.join("info").join("take.wav.trashinfo"));
        assert!(note.contains(&format!("Path={}", found.display())), "{note}");
    }

    #[test]
    fn nothing_in_the_trash_is_ever_replaced() {
        let disk = tempfile::tempdir().unwrap();
        let (from, to) = (disk.path().join("take.wav"), disk.path().join("there.wav"));
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"old").unwrap();
        let refused = rename_new(&from, &to).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!((read(&from), read(&to)), ("new".to_string(), "old".to_string()));
        rename_new(&from, &disk.path().join("free.wav")).unwrap();
        assert!(!from.exists());
    }

    #[test]
    fn a_name_that_is_taken_is_numbered_before_its_ending() {
        assert_eq!(named(OsStr::new("Take.wav"), 1), "Take.wav");
        assert_eq!(named(OsStr::new("Take.wav"), 3), "Take.3.wav");
        assert_eq!(named(OsStr::new("Take"), 2), "Take.2");
        assert_eq!(note_of(Path::new("/a b/ü.wav"), "T"), "[Trash Info]\nPath=/a%20b/%C3%BC.wav\nDeletionDate=T\n");
    }
}
