//! Programs that are not Trill's own, found when they are needed and
//! never fetched or shipped: `ffmpeg`, for a recording in a format Trill
//! does not read itself, and the program that fetches a recording from a
//! link.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

#[cfg(windows)]
pub(crate) fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn hide_console(_command: &mut Command) {}

/// The last line a program wrote to its standard error, which is where
/// they say what went wrong.
pub(crate) fn last_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("it did not say why")
        .to_owned()
}

/// Has `ffmpeg` write the audio of `from` as a WAV file at `to`, and
/// answers with `to`.
pub fn convert(ffmpeg: &Path, from: &Path, to: &Path) -> Result<PathBuf, String> {
    let partial = crate::store::partial_of(to);
    let mut command = Command::new(ffmpeg);
    command
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(from)
        .args(["-vn", "-map", "0:a:0", "-ac", "2", "-ar", "44100", "-c:a", "pcm_s24le", "-f", "wav"])
        .arg(&partial)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let output =
        command.output().map_err(|error| format!("ffmpeg ('{}') could not be started: {error}", ffmpeg.display()))?;
    if !output.status.success() || !partial.is_file() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("ffmpeg could not read the audio of {}: {}", from.display(), last_line(&output.stderr)));
    }
    std::fs::rename(&partial, to).map_err(|error| format!("{}: {error}", to.display()))?;
    Ok(to.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_that_is_not_installed_is_not_found_and_the_last_line_is_what_went_wrong() {
        assert_eq!(find_program("a-program-nobody-has-installed"), None);
        assert_eq!(last_line(b"warming up\r\n\r\nERROR: no such thing\r\n\r\n"), "ERROR: no such thing");
        assert_eq!(last_line(b""), "it did not say why");
    }
}
