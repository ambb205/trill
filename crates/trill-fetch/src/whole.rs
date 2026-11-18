//! Files that are whole or not there, and the small things said about them:
//! how a number of bytes is read, how much room a disk has, where programs are.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The name a file is written under until it is whole.
pub fn partial_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    path.with_file_name(name)
}

/// Writes `bytes` under another name and gives the file its own once it is whole.
pub fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let said = |error: std::io::Error| format!("{}: {error}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(said)?;
    }
    let partial = partial_of(path);
    std::fs::write(&partial, bytes).map_err(said)?;
    std::fs::rename(&partial, path).map_err(said)
}

/// Writes `value` to `path` whole or not at all.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string(value).map_err(|error| error.to_string())?;
    write_whole(path, text.as_bytes())
}

/// The length of a file, and nothing for one that is not there.
pub fn length(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// A number of bytes as people say it, and what is under half a megabyte
/// in kilobytes (`80 KB`) where [`said`] would call it nothing: for a list
/// whose parts may be that small.
pub fn said_closely(bytes: u64) -> String {
    if bytes > 0 && bytes < 1 << 19 { format!("{} KB", bytes.div_ceil(1 << 10)) } else { said(bytes) }
}

/// A number of bytes as people say it: `3.4 GB`, `512 MB`.
pub fn said(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / (1u64 << 20) as f64)
    }
}

/// The room that is free on the disk `folder` is on (or will be, once it
/// is made), in bytes. `None` where the machine cannot be asked.
pub fn disk_free(folder: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        use windows::core::HSTRING;
        let there = folder.ancestors().find(|folder| folder.is_dir())?;
        let mut free = 0u64;
        // SAFETY: the name is a string that ends, and the number is the one the call fills in.
        unsafe { GetDiskFreeSpaceExW(&HSTRING::from(there.as_os_str()), Some(&mut free), None, None).ok()? };
        Some(free)
    }
    #[cfg(not(windows))]
    {
        let _ = folder;
        None
    }
}

/// Where the program `name` is, if it is installed where programs are
/// looked for: the folders of `PATH`.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let endings: &[&str] = if cfg!(windows) { &["exe", "cmd", "bat"] } else { &[""] };
    let folders = std::env::var_os("PATH")?;
    std::env::split_paths(&folders)
        .flat_map(|folder| {
            endings.iter().map(move |ending| {
                if ending.is_empty() { folder.join(name) } else { folder.join(format!("{name}.{ending}")) }
            })
        })
        .find(|path| path.is_file())
}

/// Keeps a program that is started from opening a window of its own.
#[cfg(windows)]
pub fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console(_command: &mut Command) {}

/// The last line a program wrote to its standard error, which is where
/// they say what went wrong.
pub fn last_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("it did not say why")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_written_whole_and_bytes_are_said_as_people_say_them() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("deep").join("in").join("part.json");
        write_json(&path, &vec![1, 2, 3]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1,2,3]");
        assert!(!partial_of(&path).exists());
        assert_eq!(partial_of(Path::new("a/model.onnx")), Path::new("a/model.onnx.partial"));
        assert_eq!((length(&path), length(&folder.path().join("gone"))), (7, 0));
        assert_eq!((said(3 << 30), said(512 << 20)), ("3.0 GB".to_owned(), "512 MB".to_owned()));
        // Said closely, what is small is not said to be nothing; nothing is.
        let closely = [said_closely(79_744), said_closely(600 << 10), said_closely(0), said_closely(3 << 30)];
        assert_eq!(closely, ["78 KB", "1 MB", "0 MB", "3.0 GB"]);
        assert_eq!(said(79_744), "0 MB");
    }

    #[test]
    fn a_program_that_is_not_installed_is_not_found_and_the_last_line_is_what_went_wrong() {
        assert_eq!(find_program("a-program-nobody-has-installed"), None);
        assert_eq!(last_line(b"warming up\r\n\r\nERROR: no such thing\r\n\r\n"), "ERROR: no such thing");
        assert_eq!(last_line(b""), "it did not say why");
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_that_is_not_there_yet_is_on_the_disk_its_nearest_folder_is_on() {
        let here = std::env::temp_dir();
        assert!(disk_free(&here.join("not").join("made").join("yet")).expect("Windows can be asked") > 0);
    }
}
