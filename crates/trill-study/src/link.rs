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
        return Err(format!(
            "the recording behind {url} could not be fetched: {}",
            said.trim_start_matches("ERROR:").trim()
        ));
    }
    let Some(path) = source() else {
        return Err(format!("the program that fetches finished and left no recording of {url}"));
    };
    progress(1.0);
    Ok(Fetched { path, title, by })
}
