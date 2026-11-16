//! Finding the user's CLI and asking it whether it is ready.
//!
//! Detection looks for the program, runs `--version`, and runs
//! `auth status`, which is the CLI's own report of whether someone is signed
//! in. Nothing else is consulted: this module never opens a credential or
//! configuration file, and the account details that `auth status` prints
//! (e-mail address, organisation) are read past and dropped.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::env as hygiene;
use crate::process;

/// The oldest CLI the assistant is offered with.
///
/// Version 2.1.94 was checked to accept the whole command line this crate
/// builds, to connect to Trill's MCP server under it, to offer no tool of its
/// own, and to report a missing login the way newer versions do. Complete
/// conversations were verified on 2.1.325.
pub const MINIMUM_CLI_VERSION: CliVersion = CliVersion::new(2, 1, 94);

/// How long the CLI gets to answer `--version` or `auth status`. Generous
/// because the first start of a large executable on a busy machine is slow.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// A CLI version, such as `2.1.325`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CliVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl CliVersion {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    /// Reads the version that `--version` prints first, as in
    /// `2.1.325 (Claude Code)`.
    fn parse(output: &str) -> Option<Self> {
        let mut numbers = output.split_whitespace().next()?.split('.').map(|part| {
            let digits = part.find(|c: char| !c.is_ascii_digit()).map_or(part, |end| &part[..end]);
            digits.parse::<u32>().ok()
        });
        Some(Self::new(numbers.next()??, numbers.next()??, numbers.next()??))
    }
}

impl fmt::Display for CliVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A CLI that can be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInstallation {
    /// The executable itself. For an npm installation this is the program
    /// behind the `claude.cmd` launcher, which cannot be started directly.
    pub path: PathBuf,
    pub version: CliVersion,
}

/// How the CLI says the user is signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginMethod {
    /// A Claude account, which is what signing in with `claude` gives.
    ClaudeAccount {
        /// The plan as the CLI names it (`"pro"`, `"max"`), when it says.
        /// For display only: it implies nothing about limits.
        plan: Option<String>,
    },
    /// An API key. Messages are billed to an API account.
    ApiKey,
    /// A method this crate does not know by name, in the CLI's word for it.
    Other(String),
}

/// A CLI that is installed, recent enough and signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliStatus {
    pub installation: CliInstallation,
    pub login: LoginMethod,
}

/// Why the assistant cannot be offered. The `Display` text is one line for
/// the user that says what to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unavailable {
    #[error(
        "Claude Code is not installed. Install it from https://claude.com/claude-code, run `claude` in a terminal \
         to sign in, then try again."
    )]
    NotInstalled,

    /// Only a launcher script was found, of a kind this crate cannot see
    /// through to a program it can start (and so stop) reliably.
    #[error(
        "Trill cannot start the Claude Code installed at {}. Update it with `npm install -g \
         @anthropic-ai/claude-code@latest`, or install it again from https://claude.com/claude-code.",
        path.display()
    )]
    UnsupportedInstall { path: PathBuf },

    #[error(
        "Claude Code at {} is not working ({detail}). Check that `claude --version` works in a terminal.",
        path.display()
    )]
    NotWorking { path: PathBuf, detail: String },

    #[error(
        "Claude Code {} is too old for Trill, which needs {minimum} or newer. Run `claude update` in a terminal.",
        installation.version
    )]
    TooOld { installation: CliInstallation, minimum: CliVersion },

    /// Signing in is the user's to do, in the CLI: this crate never does it.
    #[error("Claude Code is not signed in. Open a terminal, run `claude`, sign in there, then try again.")]
    NotLoggedIn { installation: CliInstallation },
}

/// Finds the CLI and checks that it can be used: installed, recent enough,
/// signed in.
///
/// `cli_path` is a location the user chose; `None` searches `PATH` and the
/// places the installers use. `keep_redirecting_environment` must match the
/// [`AssistantConfig`](crate::AssistantConfig) the conversation will use, so
/// that the login reported is the one a message would actually use.
///
/// This starts the CLI twice and takes a second or so. Call it from a
/// background thread, not from a frame of the UI.
pub fn detect(cli_path: Option<&Path>, keep_redirecting_environment: bool) -> Result<CliStatus, Unavailable> {
    let path = match cli_path {
        Some(path) => usable(path)?,
        None => find(&candidates(&|name| env::var_os(name)))?,
    };
    let installation = installation(path)?;
    let login = login(&installation, keep_redirecting_environment)?;
    Ok(CliStatus { installation, login })
}

/// Where the CLI may be, most likely first.
fn candidates(variable: &dyn Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let directory = |name: &str| variable(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    let mut candidates = Vec::new();

    // A GUI program's PATH can be older than the installation, so the
    // installers' own locations are tried as well.
    let on_path = variable("PATH").map(|path| env::split_paths(&path).collect::<Vec<_>>()).unwrap_or_default();
    if cfg!(windows) {
        for directory in on_path {
            candidates.push(directory.join("claude.exe"));
            candidates.push(directory.join("claude.cmd"));
        }
        if let Some(home) = directory("USERPROFILE") {
            candidates.push(home.join(".local").join("bin").join("claude.exe"));
        }
        if let Some(roaming) = directory("APPDATA") {
            candidates.push(roaming.join("npm").join("claude.cmd"));
        }
        if let Some(local) = directory("LOCALAPPDATA") {
            candidates.push(local.join("Microsoft").join("WinGet").join("Links").join("claude.exe"));
        }
    } else {
        candidates.extend(on_path.into_iter().map(|directory| directory.join("claude")));
        if let Some(home) = directory("HOME") {
            candidates.push(home.join(".local").join("bin").join("claude"));
            candidates.push(home.join(".claude").join("local").join("claude"));
        }
        candidates.push(PathBuf::from("/usr/local/bin/claude"));
        candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
    }
    candidates
}

/// The first candidate that exists and can be started.
fn find(candidates: &[PathBuf]) -> Result<PathBuf, Unavailable> {
    let mut unsupported = None;
    for candidate in candidates.iter().filter(|candidate| candidate.is_file()) {
        match usable(candidate) {
            Ok(path) => return Ok(path),
            Err(reason) => unsupported = unsupported.or(Some(reason)),
        }
    }
    Err(unsupported.unwrap_or(Unavailable::NotInstalled))
}

/// The program to start for the CLI found at `path`.
///
/// npm installs a `claude.cmd` launcher. A batch file can only run through
/// the command interpreter, whose quoting rules make arguments unsafe to
/// pass and which would sit between Trill and the process it needs to stop.
/// The launcher only forwards to the executable inside the package, so that
/// executable is started instead. Nothing is modified or copied.
fn usable(path: &Path) -> Result<PathBuf, Unavailable> {
    if !path.is_file() {
        return Err(Unavailable::NotInstalled);
    }
    let is_launcher_script = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat"));
    if !is_launcher_script {
        return Ok(path.to_owned());
    }
    let packaged = path
        .parent()
        .map(|directory| {
            directory.join("node_modules").join("@anthropic-ai").join("claude-code").join("bin").join("claude.exe")
        })
        .filter(|packaged| packaged.is_file());
    packaged.ok_or_else(|| Unavailable::UnsupportedInstall { path: path.to_owned() })
}

fn installation(path: PathBuf) -> Result<CliInstallation, Unavailable> {
    let mut command = Command::new(&path);
    command.arg("--version");
    hygiene::apply(&mut command, false);
    let answer = process::run_captured(&mut command, ANSWER_TIMEOUT).map_err(|error| not_working(&path, &error))?;
    let Some(version) = CliVersion::parse(&answer) else {
        return Err(Unavailable::NotWorking { path, detail: "it did not report a version".to_owned() });
    };
    let installation = CliInstallation { path, version };
    if version < MINIMUM_CLI_VERSION {
        return Err(Unavailable::TooOld { installation, minimum: MINIMUM_CLI_VERSION });
    }
    Ok(installation)
}

fn login(installation: &CliInstallation, keep_redirecting_environment: bool) -> Result<LoginMethod, Unavailable> {
    let mut command = Command::new(&installation.path);
    command.args(["auth", "status", "--json"]);
    hygiene::apply(&mut command, keep_redirecting_environment);
    let answer =
        process::run_captured(&mut command, ANSWER_TIMEOUT).map_err(|error| not_working(&installation.path, &error))?;
    // The exit code is 1 when nobody is signed in, and the report is printed
    // all the same, so the report decides.
    match read_login_report(&answer) {
        Some(LoginReport::SignedIn(method)) => Ok(method),
        Some(LoginReport::SignedOut) => Err(Unavailable::NotLoggedIn { installation: installation.clone() }),
        None => Err(Unavailable::NotWorking {
            path: installation.path.clone(),
            detail: "`claude auth status` gave no usable answer".to_owned(),
        }),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LoginReport {
    SignedIn(LoginMethod),
    SignedOut,
}

/// Reads the CLI's `auth status` report. Only the three fields named here are
/// looked at. `None` when the text is not such a report.
fn read_login_report(report: &str) -> Option<LoginReport> {
    let report: Value = serde_json::from_str(report.trim()).ok()?;
    if !report.get("loggedIn")?.as_bool()? {
        return Some(LoginReport::SignedOut);
    }
    let plan = report["subscriptionType"].as_str().filter(|plan| !plan.is_empty()).map(str::to_owned);
    Some(LoginReport::SignedIn(match report["authMethod"].as_str().unwrap_or_default() {
        "claude.ai" => LoginMethod::ClaudeAccount { plan },
        "api_key" => LoginMethod::ApiKey,
        other => LoginMethod::Other(other.to_owned()),
    }))
}

fn not_working(path: &Path, error: &io::Error) -> Unavailable {
    Unavailable::NotWorking { path: path.to_owned(), detail: error.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn versions_are_read_from_what_the_cli_prints() {
        assert_eq!(CliVersion::parse("2.1.325 (Claude Code)\n"), Some(CliVersion::new(2, 1, 325)));
        assert_eq!(CliVersion::parse("3.0.0-beta.2"), Some(CliVersion::new(3, 0, 0)));
        assert_eq!(CliVersion::parse("claude: command not found"), None);
        assert_eq!(CliVersion::parse(""), None);
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(CliVersion::new(2, 1, 94) < CliVersion::new(2, 1, 325));
        assert!(CliVersion::new(2, 10, 0) > CliVersion::new(2, 9, 99));
        assert_eq!(CliVersion::new(2, 1, 325).to_string(), "2.1.325");
    }

    #[test]
    fn a_login_report_is_read_without_keeping_account_details() {
        let report = r#"{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty",
            "email": "someone@example.com", "orgName": "Someone", "subscriptionType": "max"}"#;
        let read = read_login_report(report).expect("a report");

        assert_eq!(read, LoginReport::SignedIn(LoginMethod::ClaudeAccount { plan: Some("max".into()) }));
        assert!(!format!("{read:?}").contains("someone"));
    }

    #[test]
    fn other_login_reports() {
        let signed_in = |method| Some(LoginReport::SignedIn(method));

        assert_eq!(read_login_report(r#"{"loggedIn":true,"authMethod":"api_key"}"#), signed_in(LoginMethod::ApiKey));
        assert_eq!(
            read_login_report(r#"{"loggedIn":true,"authMethod":"bedrock"}"#),
            signed_in(LoginMethod::Other("bedrock".into()))
        );
        assert_eq!(read_login_report(r#"{"loggedIn":false,"authMethod":"none"}"#), Some(LoginReport::SignedOut));
        assert_eq!(read_login_report("Login method: something else"), None);
        assert_eq!(read_login_report(r#"{"authMethod":"claude.ai"}"#), None);
    }

    /// A scratch directory under the system's temporary directory.
    fn scratch(name: &str) -> PathBuf {
        let directory = env::temp_dir().join(format!("trill-assistant-detect-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("a scratch directory");
        directory
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
        fs::write(path, b"").expect("a file");
    }

    #[test]
    fn nothing_found_means_not_installed() {
        let root = scratch("none");
        assert_eq!(find(&[root.join("claude.exe"), root.join("claude")]), Err(Unavailable::NotInstalled));
        assert_eq!(usable(&root.join("claude.exe")), Err(Unavailable::NotInstalled));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_npm_launcher_resolves_to_the_program_it_forwards_to() {
        let root = scratch("npm");
        let launcher = root.join("claude.cmd");
        let packaged = root.join("node_modules/@anthropic-ai/claude-code/bin/claude.exe");
        touch(&launcher);
        touch(&packaged);

        assert_eq!(usable(&launcher).map(|path| path.canonicalize().unwrap()), Ok(packaged.canonicalize().unwrap()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_launcher_that_cannot_be_seen_through_is_passed_over_for_a_real_program() {
        let root = scratch("order");
        let old_launcher = root.join("first/claude.cmd");
        let program = root.join("second/claude.exe");
        touch(&old_launcher);

        // Alone, the launcher is reported, so the user learns what to update.
        assert_eq!(
            find(&[old_launcher.clone(), program.clone()]),
            Err(Unavailable::UnsupportedInstall { path: old_launcher.clone() })
        );

        touch(&program);
        assert_eq!(find(&[old_launcher, program.clone()]), Ok(program));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_search_covers_path_and_the_installers_own_locations() {
        let variable = |name: &str| -> Option<OsString> {
            match name {
                "PATH" => Some(env::join_paths([PathBuf::from("first"), PathBuf::from("second")]).unwrap()),
                "USERPROFILE" | "HOME" => Some("home".into()),
                "APPDATA" => Some("roaming".into()),
                "LOCALAPPDATA" => Some("local".into()),
                _ => None,
            }
        };
        let found = candidates(&variable);

        let first_on_path =
            if cfg!(windows) { Path::new("first").join("claude.exe") } else { Path::new("first").join("claude") };
        assert_eq!(found.first(), Some(&first_on_path));
        let native = if cfg!(windows) { "home/.local/bin/claude.exe" } else { "home/.local/bin/claude" };
        assert!(found.contains(&PathBuf::from(native).components().collect()), "{found:?}");
        assert!(
            found.iter().position(|path| path.starts_with("second"))
                < found.iter().position(|path| path.starts_with("home"))
        );
    }
}
