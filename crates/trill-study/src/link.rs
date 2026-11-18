//! Programs that are not Trill's own, found when they are needed and
//! never fetched or shipped: `ffmpeg`, for a recording in a format Trill
//! does not read itself, and the program that fetches a recording from a
//! link.

pub use trill_fetch::find_program;

use crate::store::StudyFolder;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use trill_fetch::{hide_console, last_line};

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
        .stderr(Stdio::piped())
        .env_remove(crate::helper::ENV);
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

/// What the lines begin with that the program that fetches is told to
/// write for Trill, among whatever else it writes.
const SAID: &str = "@trill@";

/// A recording that was fetched from a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub path: PathBuf,
    /// What the page it came from calls it.
    pub title: Option<String>,
    /// And whose it says it is.
    pub by: Option<String>,
}

/// Has the program the user installed for it (`yt-dlp`) fetch the audio
/// behind `url` into `folder`, as `source` with whatever ending it has.
/// A recording longer than `longest` seconds is not fetched.
pub fn fetch(
    fetcher: &Path,
    ffmpeg: Option<&Path>,
    url: &str,
    folder: &Path,
    longest: f64,
    progress: &mut dyn FnMut(f32),
) -> Result<Fetched, String> {
    // A link, and nothing the program could take for one of its options or for a file of this machine.
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(char::is_whitespace) {
        return Err(format!("'{url}' is not a link: one begins with https://"));
    }
    std::fs::create_dir_all(folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let mut command = Command::new(fetcher);
    command
        .args(["--no-playlist", "--no-warnings", "--no-simulate", "--force-overwrites"])
        .args(["--quiet", "--progress", "--newline"])
        // The best audio there is, and never the half minute some sites hand out as a preview.
        .args(["-f", "bestaudio[format_id!$=_preview]/best[format_id!$=_preview]"])
        .args([
            "--progress-template",
            "download:@trill@got=%(progress.downloaded_bytes)s/%(progress.total_bytes,progress.total_bytes_estimate)s",
        ])
        .args(["--print", "before_dl:@trill@title=%(track,title)s"])
        .args(["--print", "before_dl:@trill@by=%(artist,uploader,channel)s"])
        .args(["--print", "before_dl:@trill@seconds=%(duration)s"])
        .arg("-o")
        .arg(folder.join("source.%(ext)s"));
    if let Some(ffmpeg) = ffmpeg {
        command.arg("--ffmpeg-location").arg(ffmpeg);
    }
    command.arg("--").arg(url).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // What marks this process as one that studies is its own, and not for a program it starts.
    command.env_remove(crate::helper::ENV);
    hide_console(&mut command);
    let mut process = command
        .spawn()
        .map_err(|error| format!("the program that fetches ('{}') could not be started: {error}", fetcher.display()))?;
    let (Some(stdout), Some(mut stderr)) = (process.stdout.take(), process.stderr.take()) else {
        let _ = process.kill();
        return Err("the program that fetches could not be heard".to_owned());
    };
    let complaints = std::thread::spawn(move || {
        let mut said = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut said);
        said
    });
    let known = |value: &str| Some(value.trim().to_owned()).filter(|value| !value.is_empty() && value != "NA");
    let (mut title, mut by, mut too_long) = (None, None, None);
    for line in std::io::BufRead::split(std::io::BufReader::new(stdout), b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        let Some(said) = line.trim().strip_prefix(SAID) else { continue };
        if let Some(value) = said.strip_prefix("title=") {
            title = known(value);
        } else if let Some(value) = said.strip_prefix("by=") {
            by = known(value);
        } else if let Some(value) = said.strip_prefix("seconds=") {
            if let Some(seconds) = value.trim().parse::<f64>().ok().filter(|&seconds| seconds > longest + 1.0) {
                too_long = Some(seconds);
                let _ = process.kill();
                break;
            }
        } else if let Some((got, of)) = said.strip_prefix("got=").and_then(|value| value.split_once('/'))
            && let (Ok(got), Ok(of)) = (got.trim().parse::<f64>(), of.trim().parse::<f64>())
        {
            progress(if of > 0.0 { (got / of).clamp(0.0, 1.0) as f32 } else { 0.0 });
        }
    }
    let status = process.wait();
    let complaints = complaints.join().unwrap_or_default();
    let source = || StudyFolder::new(folder).kept_source();
    if let Some(seconds) = too_long {
        if let Some(path) = source() {
            let _ = std::fs::remove_file(path);
        }
        return Err(format!(
            "the recording behind the link is {:.0} minutes long, and a recording is studied up to {:.0} minutes",
            seconds / 60.0,
            longest / 60.0
        ));
    }
    if !status.is_ok_and(|status| status.success()) {
        let said = last_line(&complaints);
        // What a site says to a program it no longer knows is no more than "forbidden": where
        // the program is old, that is most often why, and the sentence says so.
        let old = old_version(fetcher)
            .map(|version| {
                format!(
                    ". The program that fetches is old (yt-dlp {version}), and sites change what they ask of one more often than that: update it (the way it was installed, or with yt-dlp -U), then take the study up again"
                )
            })
            .unwrap_or_default();
        return Err(format!(
            "the recording behind {url} could not be fetched: {}{old}",
            said.trim_start_matches("ERROR:").trim()
        ));
    }
    let Some(path) = source() else {
        return Err(format!("the program that fetches finished and left no recording of {url}"));
    };
    progress(1.0);
    Ok(Fetched { path, title, by })
}

/// From how many days on the program that fetches is old: its own
/// warning comes at the same age.
const OLD_DAYS: i64 = 90;

/// The version the program that fetches says it is, where that is old
/// today. Asked only once a fetch has failed.
fn old_version(fetcher: &Path) -> Option<String> {
    let mut command = Command::new(fetcher);
    command.arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).env_remove(crate::helper::ENV);
    hide_console(&mut command);
    let output = command.output().ok().filter(|output| output.status.success())?;
    let version = String::from_utf8_lossy(&output.stdout).lines().next()?.trim().to_owned();
    let today = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() / 86_400;
    (days_old(&version, i64::try_from(today).ok()?)? > OLD_DAYS).then_some(version)
}

/// The days since 1970 of a day of the calendar.
fn day_number(year: i64, month: i64, day: i64) -> i64 {
    // The year begins in March, so that the day a leap year adds is its last.
    let year = if month <= 2 { year - 1 } else { year };
    let (era, of_era) = (year.div_euclid(400), year.rem_euclid(400));
    let of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468
}

/// How many days old a version is on the day `today` (in days since
/// 1970), where it is named for its day as yt-dlp's are (`2025.10.22`; a
/// build between two releases has more after that).
fn days_old(version: &str, today: i64) -> Option<i64> {
    let mut numbers = version.trim().split('.').map(|number| number.parse::<i64>().ok());
    let (year, month, day) = (numbers.next()??, numbers.next()??, numbers.next()??);
    ((2000..3000).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day))
        .then(|| today - day_number(year, month, day))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_named_for_its_day_says_how_old_it_is() {
        assert_eq!((day_number(1970, 1, 1), day_number(2000, 3, 1), day_number(2026, 11, 18)), (0, 11_017, 20_775));
        let today = day_number(2026, 11, 18);
        assert_eq!(days_old("2026.11.18", today), Some(0));
        assert_eq!(days_old("2025.10.22", today), Some(392));
        // Through the end of a February with a 29th, and of a build between two releases.
        assert_eq!(days_old("2024.02.28", day_number(2024, 3, 1)), Some(2));
        assert_eq!(days_old(" 2026.10.26.232815\n", today), Some(23));
        // What is not named for a day says nothing, and neither does a day there is not.
        for version in ["", "7.1", "v2.4.1", "N-118193-g1234", "2026.13.01", "2026.00.10", "12.10.22"] {
            assert_eq!(days_old(version, today), None, "{version}");
        }
    }
}
