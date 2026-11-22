//! Which Trill this is, whether a newer one is out, and putting it in
//! place.
//!
//! - [`this`] says what the program that is running is: its version, and
//!   for a release the commit it was built from. A release is a build its
//!   makers made as one (`scripts/release.sh`); everything else is *a build
//!   of your own*, which looks for nothing and is nobody's to update.
//! - [`release`] is a release as it is announced: a small text under its
//!   makers' signature, which says what is new and where the build for
//!   each kind of machine is, with its checksum.
//! - [`updates`] is what a program knows of a newer one and does about
//!   it, when asked: looking, and fetching it to where it is staged.
//! - [`place`] is the folder Trill is installed in: an update staged
//!   there, put in place, and put back.
//! - [`start`] is the first thing a program does once it has turned out
//!   to be no job: it puts a staged update in place where no Trill runs,
//!   and says for as long as it lives that one does.
//!
//! The rules, which hold by every route:
//!
//! - **Nothing is taken that its makers did not sign.** An announcement is
//!   read under the keys built into the program and under nothing else;
//!   a build is held to the checksum the announcement gives, as it is
//!   fetched and again right before it goes in.
//! - **Nothing is fetched unasked.** The one thing a program fetches by
//!   itself is the announcement, a text that is the same for everyone,
//!   and only where its user has not switched that off. A build comes
//!   when its user asks for it.
//! - **Nothing changes under a program that runs.** Trill starts itself a
//!   second time by the path of its own file; an update goes in at a
//!   start, while no Trill runs from the folder, or not at all.
//! - **A version that does not come up is put back.** The program that
//!   put an update in place is the old one: it waits to see the new one
//!   up, and where that ends at once it puts back what was there and goes
//!   on itself, with a sentence.

pub mod place;
pub mod release;
pub mod scripted;
pub mod updates;
mod version;

pub use place::{Place, Staged, came_up, updated_from};
pub use release::{Build, Piece, PublicKey, Release, SecretKey};
pub use updates::{Came, Options, Status, Step, Updates};
pub use version::Version;

use place::Trial;

/// What an installer knows an installed Trill by, in the list of
/// installed programs: the `AppId` of `installer/trill.iss`.
pub const INSTALLED_AS: &str = "{B4E0A3C2-7D5F-4E19-9C1A-5A6D2F8E3B71}";
/// The name every Trill that runs holds, by which an installer sees that
/// one does: the `AppMutex` of `installer/trill.iss`.
pub const RUNS_AS: &str = "TrillIsRunning";

/// The program that is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct This {
    pub version: Version,
    /// The commit a release was built from. Nothing for a build of one's
    /// own, which is no release.
    pub commit: Option<String>,
}

impl This {
    /// Whether its makers built it as a release.
    pub fn is_release(&self) -> bool {
        self.commit.is_some()
    }

    /// `Trill 0.2.0 (build 0ce65fb)`, or `Trill 0.2.0, a build of your own`.
    pub fn words(&self) -> String {
        match &self.commit {
            Some(commit) => format!("Trill {} (build {commit})", self.version),
            None => format!("Trill {}, a build of your own", self.version),
        }
    }
}

/// What the program that is running is. The version is the workspace's;
/// the commit is what `scripts/release.sh` built it with
/// (`TRILL_RELEASE_COMMIT` when it was compiled).
pub fn this() -> This {
    This {
        version: env!("CARGO_PKG_VERSION").parse().unwrap_or_default(),
        commit: option_env!("TRILL_RELEASE_COMMIT").filter(|commit| !commit.is_empty()).map(str::to_owned),
    }
}

/// Which of Trill's programs is starting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// The window: it puts a staged update in place.
    Window,
    /// The server without a window, which an AI client starts and speaks
    /// to through its pipes: it never does, because the program a client
    /// started has to be the one that answers it.
    Server,
}

/// What a start came to. Held until the program ends: it says that a
/// Trill runs.
pub struct Started {
    _running: Option<place::Running>,
    /// What there is to tell the user of an update: that this version
    /// took an older one's place, or that one did not go in.
    pub said: Option<String>,
}

/// Holds the name an installer sees a running Trill by, until the
/// program ends.
fn hold_the_name() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::core::HSTRING;
        // SAFETY: the name is a string that ends. The handle is never closed: the system closes it
        // when the program ends, which is how long the name is to be held.
        let _ = unsafe { CreateMutexW(None, false, &HSTRING::from(RUNS_AS)) };
    }
}

/// Writes the version that is installed now into the list of installed
/// programs, where an installer put Trill there: an update that went in
/// by itself would otherwise be listed as the version before it.
fn note_what_is_installed(version: Version) {
    #[cfg(windows)]
    {
        let key = format!(r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{INSTALLED_AS}_is1");
        let Some(reg) = trill_fetch::find_program("reg") else { return };
        let run = |args: &[&str]| {
            let mut command = std::process::Command::new(&reg);
            command.args(args).stdin(std::process::Stdio::null());
            command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            trill_fetch::hide_console(&mut command);
            command.status().is_ok_and(|status| status.success())
        };
        // Only where an installer made the entry: Trill makes none of its own.
        if run(&["query", &key, "/v", "DisplayVersion"]) {
            run(&["add", &key, "/v", "DisplayVersion", "/t", "REG_SZ", "/d", &version.to_string(), "/f"]);
        }
    }
    #[cfg(not(windows))]
    let _ = version;
}

/// The first thing a program does once it has turned out to be no job
/// (a scan, a render, a study): where an update is staged and no Trill
/// runs from this folder, the window puts it in place, starts the new
/// version with what it was started with itself, and ends once that is
/// up. Otherwise it answers, and what it answers with says for as long
/// as it is held that a Trill runs here.
///
/// A build of one's own does none of it.
pub fn start(program: Program) -> Started {
    let this = this();
    let plain = Started { _running: None, said: None };
    if !this.is_release() {
        return plain;
    }
    let (Some(place), Ok(me)) = (Place::of_this_program(), std::env::current_exe()) else { return plain };
    hold_the_name();
    let before = place::stamp(&me);
    let mut said = None;
    if let Some(from) = updated_from().filter(|from| *from < this.version) {
        note_what_is_installed(this.version);
        said = Some(format!("Trill {} has taken the place of {from}.", this.version));
    }
    if let Some(alone) = place.alone() {
        if place.mend(&alone) {
            said = Some(
                "An update of Trill was cut short while it went in. Trill is as it was before; \
                 the update can be fetched again in File, Setup."
                    .to_owned(),
            );
        }
        match place.staged() {
            Some(staged) if staged.version <= this.version => {
                place.unstage();
                place.tidy(&alone);
            }
            Some(staged) if program == Program::Window => match place.apply(&staged, &alone) {
                Ok(()) => {
                    // The new one is to find that no Trill runs, as this one did.
                    drop(alone);
                    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
                    let tried = place.try_out(&me, &args, this.version);
                    match &tried.ended {
                        Trial::CameUp | Trial::StillStarting => std::process::exit(0),
                        Trial::Failed(why) => {
                            said = Some(match place.put_back() {
                                Ok(_) => format!(
                                    "Trill {} did not start here: {why}. Trill {} was put back and is what runs now.",
                                    staged.version, this.version
                                ),
                                Err(error) => format!(
                                    "Trill {} did not start here: {why}. Putting {} back failed too ({error}): \
                                     run the installer again.",
                                    staged.version, this.version
                                ),
                            });
                        }
                    }
                }
                Err(why) => said = Some(format!("The update to Trill {} did not go in: {why}.", staged.version)),
            },
            // The server leaves it for the window.
            Some(_) => {}
            None => place.tidy(&alone),
        }
    }
    let running = place.run();
    // An update went in while this one waited to be let run: its own file is another program now.
    if place::stamp(&me) != before {
        match program {
            Program::Window => {
                let again = std::process::Command::new(&me).args(std::env::args_os().skip(1)).spawn();
                if again.is_ok() {
                    std::process::exit(0);
                }
            }
            Program::Server => {
                eprintln!("Trill was updated while this program started: start it again.");
                std::process::exit(75);
            }
        }
    }
    Started { _running: Some(running), said }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_says_which_trill_it_is() {
        let this = this();
        assert_eq!(this.version.to_string(), env!("CARGO_PKG_VERSION"));
        // A test is never a release: nothing that is tested here looks, fetches or is updated.
        assert!(!this.is_release());
        assert_eq!(this.words(), format!("Trill {}, a build of your own", env!("CARGO_PKG_VERSION")));
        let release = This { version: Version::new(0, 2, 0), commit: Some("0ce65fb".to_owned()) };
        assert_eq!(release.words(), "Trill 0.2.0 (build 0ce65fb)");
    }

    #[test]
    fn a_build_of_ones_own_starts_as_it_always_did() {
        // No update is looked for where the program lies, no lock is taken, nothing is written.
        let started = start(Program::Window);
        assert!(started.said.is_none());
        let here = Place::of_this_program().unwrap();
        assert!(!here.folder().join(place::WORK).exists());
    }
}
