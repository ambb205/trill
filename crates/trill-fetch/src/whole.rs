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
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let there = folder.ancestors().find(|folder| folder.is_dir())?;
        let name = std::ffi::CString::new(there.as_os_str().as_bytes()).ok()?;
        // SAFETY: the name is a string that ends, and the record is the one the call fills in.
        let mut said: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(name.as_ptr(), &mut said) } != 0 {
            return None;
        }
        // What may be written by whoever is not the system's own user: the blocks available, not the blocks free.
        Some((said.f_bavail as u64).saturating_mul(said.f_frsize as u64))
    }
    #[cfg(not(any(windows, unix)))]
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

/// The signal that ended a program, in words ("signal 6, SIGABRT"), where
/// a signal did: on Unix a program that aborts or faults is ended by one,
/// and has no code to say. `None` for a program that ended by itself, and
/// on a system without signals.
pub fn ended_by_signal(status: &std::process::ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let signal = status.signal()?;
        let name = match signal {
            libc::SIGABRT => "SIGABRT",
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGBUS => "SIGBUS",
            libc::SIGILL => "SIGILL",
            libc::SIGFPE => "SIGFPE",
            libc::SIGKILL => "SIGKILL",
            libc::SIGTERM => "SIGTERM",
            libc::SIGINT => "SIGINT",
            libc::SIGHUP => "SIGHUP",
            libc::SIGPIPE => "SIGPIPE",
            libc::SIGTRAP => "SIGTRAP",
            _ => return Some(format!("signal {signal}")),
        };
        Some(format!("signal {signal}, {name}"))
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// The memory the machine has free to be used at once, in bytes: on Linux
/// what the kernel reckons can be had without swapping (`MemAvailable`).
/// `None` where it is not asked here.
pub fn memory_free() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let said = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = said.lines().find(|line| line.starts_with("MemAvailable:"))?;
        let kilobytes: u64 = line["MemAvailable:".len()..].trim().strip_suffix("kB")?.trim().parse().ok()?;
        Some(kilobytes * 1024)
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Has a fault end this process by SIGKILL, of which no system writes a
/// core or a crash report: for a process a test takes down on purpose, so
/// that a test leaves nothing on the machine. The fault that follows is
/// real, and whoever started the process sees it ended by a signal.
/// Nothing that really crashes comes here. Nothing on a system without
/// signals.
pub fn end_unreported_on_a_fault() {
    #[cfg(unix)]
    {
        extern "C" fn end(_signal: libc::c_int) {
            // SAFETY: ends this process, which may be called from a handler.
            unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        }
        for signal in
            [libc::SIGILL, libc::SIGTRAP, libc::SIGABRT, libc::SIGFPE, libc::SIGBUS, libc::SIGSEGV, libc::SIGSYS]
        {
            // SAFETY: a handler that only ends the process.
            unsafe { libc::signal(signal, end as extern "C" fn(libc::c_int) as libc::sighandler_t) };
        }
    }
}

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

    #[cfg(unix)]
    #[test]
    fn a_program_ended_by_a_signal_says_which_and_one_that_ended_by_itself_says_none() {
        let ended = |script: &str| Command::new("sh").args(["-c", script]).status().unwrap();
        // Signals that end a program without a core: one that makes a core has the system write a crash report
        // of `sh`, which a test does not leave behind.
        assert_eq!(ended_by_signal(&ended("kill -TERM $$")), Some(format!("signal {}, SIGTERM", libc::SIGTERM)));
        assert_eq!(ended_by_signal(&ended("kill -USR1 $$")), Some(format!("signal {}", libc::SIGUSR1)));
        assert_eq!(ended_by_signal(&ended("exit 3")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_machine_says_how_much_memory_it_has_free() {
        let free = memory_free().expect("Linux says");
        assert!(free > 1 << 20, "{free}");
    }

    #[cfg(unix)]
    #[test]
    fn a_fault_made_on_purpose_ends_the_process_by_sigkill() {
        use std::os::unix::process::ExitStatusExt;
        // A process of its own: this test program started again for the one test below.
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "whole::tests::faults_on_purpose", "--ignored", "--nocapture"])
            .env("TRILL_FAULT_ON_PURPOSE", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
        assert_eq!(ended_by_signal(&status), Some(format!("signal {}, SIGKILL", libc::SIGKILL)));
    }

    /// Run only by the test above, in a process of its own.
    #[cfg(unix)]
    #[test]
    #[ignore = "takes its process down: run by a_fault_made_on_purpose_ends_the_process_by_sigkill"]
    fn faults_on_purpose() {
        if std::env::var_os("TRILL_FAULT_ON_PURPOSE").is_some() {
            end_unreported_on_a_fault();
            std::process::abort();
        }
    }

    #[cfg(any(windows, unix))]
    #[test]
    fn a_folder_that_is_not_there_yet_is_on_the_disk_its_nearest_folder_is_on() {
        let here = std::env::temp_dir();
        assert!(disk_free(&here.join("not").join("made").join("yet")).expect("the system can be asked") > 0);
    }
}
