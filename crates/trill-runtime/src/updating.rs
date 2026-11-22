//! A newer Trill: what the runtime knows of one, for both front ends.
//!
//! The work is `trill-update`'s ([`trill_update::Updates`]: looking,
//! fetching, staging). Here it is given a place to remember in, the
//! settings ([`trill_library::UpdateSettings`]: whether Trill looks when
//! it starts, and a version its user asked to hear no more of) and the
//! one thing a front end shows unasked: an [`UpdateNotice`].
//!
//! A runtime looks for nothing until its program says it is one
//! ([`Runtime::look_for_updates_as_usual`], which `trill` and `trill-mcp`
//! call when they start), and then only where it is a release: a test's
//! runtime, a run that keeps nothing and a build of one's own ask nothing
//! of a network. A test hands in a program of its own
//! ([`crate::Parts::updating`]) and a shelf to fetch from.

use crate::{Runtime, Storage};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use trill_library::SaveError;
use trill_update::{Options, Step, Updates, Version};

/// The folder, in the program's data folder, that what is known of a
/// newer Trill is kept in, and a build while it is fetched.
pub const UPDATE_FOLDER: &str = "update";
/// A program that looks when it starts does not look again within this.
const LOOK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// What there is to tell the user of a newer Trill without being asked:
/// one line, which they can silence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateNotice {
    pub version: Version,
    /// It is fetched and staged: it goes in the next time the window
    /// starts while no other Trill is open.
    pub ready: bool,
}

impl UpdateNotice {
    /// The line, as both front ends say it.
    pub fn words(&self) -> String {
        match self.ready {
            true => {
                format!("Trill {} is fetched: it goes in the next time Trill starts while no other is open", self.version)
            }
            false => format!("Trill {} is out", self.version),
        }
    }

    /// The same in the few words a status bar has room for.
    pub fn short(&self) -> String {
        match self.ready {
            true => format!("Trill {} goes in at the next start", self.version),
            false => format!("Trill {} is out", self.version),
        }
    }
}

/// What a runtime holds of updates: made with it, where its settings say.
pub(crate) fn open(storage: &Storage, settings_folder: Option<&Path>, program: Option<Options>) -> Updates {
    let kept: Option<PathBuf> = match storage {
        Storage::User => trill_library::data_dir().map(|data| data.join(UPDATE_FOLDER)),
        Storage::At { .. } => settings_folder.map(|folder| folder.join(UPDATE_FOLDER)),
        Storage::Nowhere => None,
    };
    Updates::new(program.unwrap_or_else(|| Options::of_this_program(kept)))
}

impl Runtime {
    /// What this program knows of a newer Trill, and the way to look for
    /// one and to fetch it. Both wait for a network: not for the thread
    /// that draws, and not with the session held.
    pub fn updates(&self) -> &Updates {
        &self.updating
    }

    /// Has this program fetch with the system's `curl` what its user asks
    /// of a newer Trill: looking for one, and fetching it. A runtime that
    /// was not told so looks for nothing.
    pub fn look_for_updates_when_asked(&self) {
        self.updating.fetch_by(Arc::new(trill_fetch::Curl));
    }

    /// [`Runtime::look_for_updates_when_asked`], and looks whether a newer
    /// Trill is out now, where the settings say it does at a start: what
    /// the window does, which has someone to tell. One that keeps nothing
    /// looks only when it is asked.
    pub fn look_for_updates_as_usual(&self) {
        self.look_for_updates_when_asked();
        self.look_as_a_start_does();
    }

    /// Looks whether a newer Trill is out, on a thread of its own, where
    /// the settings say Trill does at a start, where this run keeps what
    /// it learns, and where it has not looked lately. What a start does
    /// once it was said who fetches.
    pub fn look_as_a_start_does(&self) {
        if self.settings().updates.look_at_start && self.settings_folder().is_some() {
            self.updating.look_if_due(LOOK_EVERY);
        }
    }

    /// What there is to say of a newer Trill without being asked: nothing
    /// where none is known, and nothing of a version the user asked to
    /// hear no more of.
    pub fn update_notice(&self) -> Option<UpdateNotice> {
        let status = self.updating.status();
        let version = status.newer_version()?;
        let quiet = self.settings().updates.quiet_about.and_then(|quiet| quiet.parse::<Version>().ok());
        if quiet.is_some_and(|quiet| quiet >= version) {
            return None;
        }
        Some(UpdateNotice { version, ready: matches!(status.step, Step::Ready(_)) })
    }

    /// Says no more of `version` by itself (`None`: of nothing; every
    /// version is said again). What is in File, Setup stays as it is.
    pub fn keep_quiet_about(&self, version: Option<Version>) -> Result<(), SaveError> {
        self.update_settings(|settings| settings.updates.quiet_about = version.map(|version| version.to_string()))
    }

    /// Whether Trill looks for a newer one when it starts.
    pub fn set_look_at_start(&self, look: bool) -> Result<(), SaveError> {
        self.update_settings(|settings| settings.updates.look_at_start = look)
    }
}
