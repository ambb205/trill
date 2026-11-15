//! Runs the scanner executable on one plugin file, under a timeout.

use super::{PluginRecord, ScanOutput};
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// How often a running scanner is checked for completion.
const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// How long to wait for the scanner's output once it has exited. Normally
/// the pipes close with the process; this only matters if the plugin
/// started a process of its own that still holds them open.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// Longest piece of the scanner's stderr quoted in a failure reason.
const MAX_REASON_CHARS: usize = 300;

/// Why a scan produced no plugins.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    /// The plugin could not be scanned. This will not change until the
    /// file does, so it is worth remembering.
    Plugin(String),
    /// The scanner itself could not be run, which says nothing about the
    /// plugin.
    Scanner(String),
}

impl Failure {
    pub(super) fn reason(&self) -> &str {
        match self {
            Failure::Plugin(reason) | Failure::Scanner(reason) => reason,
        }
    }
}

/// Scans `plugin` in a child process. Whatever the plugin does to that
/// process (crash, hang, exit), the result here is only an `Err`.
pub(super) fn run(scanner: &Path, plugin: &Path, timeout: Duration) -> Result<Vec<PluginRecord>, Failure> {
    let mut command = Command::new(scanner);
    command.arg(plugin).env(super::SCANNER_ENV, "1").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_console(&mut command);
    let mut child =
        command.spawn().map_err(|error| Failure::Scanner(format!("cannot start the plugin scanner: {error}")))?;

    // Drain both pipes on their own threads: a child that fills a pipe
    // nobody reads would block forever and look like a hang.
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::Scanner(format!("lost track of the plugin scanner: {error}")));
            }
        }
    };

    let collect = |pipe: Option<Receiver<Vec<u8>>>| {
        pipe.and_then(|bytes| bytes.recv_timeout(PIPE_GRACE).ok()).unwrap_or_default()
    };
    let (stdout, stderr) = (collect(stdout), collect(stderr));
    interpret(status, &stdout, &stderr, timeout).map_err(Failure::Plugin)
}

/// Reads `pipe` to its end on a helper thread. The thread is not joined: it
/// finishes by itself when the last process holding the pipe exits.
fn drain(mut pipe: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    receiver
}

/// A GUI application must not flash a console window for every plugin.
#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

/// Turns what the scanner did into a result. `status` is `None` when the
/// scanner was killed for exceeding `timeout`.
fn interpret(
    status: Option<ExitStatus>,
    stdout: &[u8],
    stderr: &[u8],
    timeout: Duration,
) -> Result<Vec<PluginRecord>, String> {
    // Plugins print to stdout while loading, so the result is the last line
    // that parses rather than the whole output.
    let stdout = String::from_utf8_lossy(stdout);
    let output = stdout.lines().rev().find_map(|line| serde_json::from_str::<ScanOutput>(line.trim()).ok());

    match (output, status) {
        // The descriptors were read; a plugin that then crashes or hangs
        // while unloading has still been scanned.
        (Some(output), status) => {
            if !status.is_some_and(|status| status.success()) {
                log::debug!("the plugin scanner did not exit cleanly after reporting its result");
            }
            Ok(output.plugins)
        }
        (None, None) => Err(format!("the scan timed out after {:.0} s", timeout.as_secs_f64())),
        (None, Some(status)) => {
            let stderr = String::from_utf8_lossy(stderr);
            let detail = stderr.lines().rev().map(str::trim).find(|line| !line.is_empty());
            Err(match detail {
                Some(detail) if !status.success() => {
                    let detail: String = detail.chars().take(MAX_REASON_CHARS).collect();
                    format!("{detail} (scanner {status})")
                }
                _ if status.success() => "the plugin scanner reported nothing".to_string(),
                _ => format!("the plugin crashed the scanner ({status})"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(20);

    #[cfg(windows)]
    fn status(code: u8) -> ExitStatus {
        std::os::windows::process::ExitStatusExt::from_raw(code.into())
    }

    /// On Unix the exit code sits in the second byte of the wait status.
    #[cfg(unix)]
    fn status(code: u8) -> ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(i32::from(code) << 8)
    }

    #[test]
    fn the_last_json_line_is_the_result() {
        let stdout = b"plugin banner
{\"plugins\":[{\"id\":\"a.b\",\"name\":\"AB\"}]}
";
        let plugins = interpret(Some(status(0)), stdout, b"", TIMEOUT).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].id, "a.b");
        assert_eq!(plugins[0].vendor, "", "absent fields default to empty");
    }

    #[test]
    fn a_result_survives_a_crash_or_hang_on_the_way_out() {
        let stdout = b"{\"plugins\":[]}
";
        assert!(interpret(Some(status(3)), stdout, b"boom", TIMEOUT).unwrap().is_empty());
        assert!(interpret(None, stdout, b"", TIMEOUT).unwrap().is_empty());
    }

    #[test]
    fn failures_explain_themselves() {
        let timed_out = interpret(None, b"", b"", Duration::from_secs(20)).unwrap_err();
        assert!(timed_out.contains("timed out after 20 s"), "{timed_out}");

        let reported = interpret(
            Some(status(1)),
            b"",
            b"
cannot open the file

",
            TIMEOUT,
        )
        .unwrap_err();
        assert!(reported.starts_with("cannot open the file"), "{reported}");

        let crashed = interpret(Some(status(1)), b"garbage", b"", TIMEOUT).unwrap_err();
        assert!(crashed.contains("crashed the scanner"), "{crashed}");

        let silent = interpret(Some(status(0)), b"", b"", TIMEOUT).unwrap_err();
        assert!(silent.contains("reported nothing"), "{silent}");
    }

    #[test]
    fn a_missing_scanner_is_an_error_not_a_panic() {
        let error = run(Path::new("no-such-scanner-executable"), Path::new("x.clap"), TIMEOUT).unwrap_err();
        assert!(matches!(&error, Failure::Scanner(reason) if reason.contains("cannot start")), "{error:?}");
    }

    #[test]
    fn a_hanging_scanner_is_killed_at_the_timeout() {
        // A system program that blocks when given one argument stands in
        // for a scanner stuck inside a plugin.
        let (program, argument) = if cfg!(windows) { ("waitfor", "DawPluginScanHangTest") } else { ("sleep", "60") };
        let started = Instant::now();
        let error = run(Path::new(program), Path::new(argument), Duration::from_millis(300)).unwrap_err();
        assert!(matches!(&error, Failure::Plugin(reason) if reason.contains("timed out")), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(15), "the scanner was not killed promptly");
    }
}
