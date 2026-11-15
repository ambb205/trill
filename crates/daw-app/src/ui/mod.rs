//! The desktop GUI.
//!
//! The session is the source of truth and may be edited by other front ends
//! at any time, so the GUI is built around three rules:
//!
//! * Views paint from a [`snapshot::Snapshot`], a copy of the project taken
//!   when the session's revision changes. The session lock is never held
//!   while painting.
//! * Every change goes through the session (`Cx::edit` and friends in
//!   [`state`]); nothing is edited in the snapshot.
//! * The window repaints continuously only while something moves (playback,
//!   meters). Idle, it polls the revision a few times a second so edits made
//!   elsewhere still show up promptly.
//!
//! Modules: [`app`] is the shell (panels, shortcuts, repaint policy);
//! [`transport`], [`arrangement`], [`detail`] (with [`piano_roll`] and
//! [`devices`]) and [`master`] are the views; [`files`] runs dialogs and
//! exports off the UI thread; [`timeline`] and [`units`] hold the pure math;
//! [`widgets`] and [`theme`] the custom controls and the look.

mod app;
mod arrangement;
mod detail;
mod devices;
mod files;
mod master;
mod piano_roll;
mod snapshot;
mod state;
mod theme;
mod timeline;
mod toast;
mod transport;
mod units;
mod widgets;

#[cfg(test)]
mod tests;

pub use state::DetailTab;

use anyhow::anyhow;
use daw_core::ClipId;
use daw_engine::SharedSession;
use eframe::egui;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How the window starts up. The defaults are an ordinary interactive run.
#[derive(Clone, Debug)]
pub struct UiOptions {
    /// The detail tab shown first.
    pub detail_tab: DetailTab,
    /// A clip to select at startup.
    pub select_clip: Option<ClipId>,
    /// Address of the MCP server AI assistants can connect to, if it runs.
    pub mcp_url: Option<String>,
    /// Render a few frames, save the window to this PNG and exit.
    pub screenshot: Option<PathBuf>,
    /// Start playback as soon as the window opens.
    pub autoplay: bool,
    /// Close the window by itself after this long.
    pub exit_after: Option<Duration>,
    /// Initial size of the window's content, in points.
    pub window_size: [f32; 2],
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            detail_tab: DetailTab::default(),
            select_clip: None,
            mcp_url: None,
            screenshot: None,
            autoplay: false,
            exit_after: None,
            window_size: [1440.0, 900.0],
        }
    }
}

/// Opens the window and runs until it is closed.
pub fn run(session: SharedSession, options: UiOptions) -> anyhow::Result<()> {
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DAW")
            .with_inner_size(options.window_size)
            .with_min_inner_size([1060.0, 640.0]),
        ..Default::default()
    };
    // Failures inside the event loop (a screenshot that could not be saved)
    // have no way to return a value, so they are left here.
    let failure = Arc::new(Mutex::new(None));
    let app_failure = failure.clone();
    eframe::run_native(
        "daw",
        native,
        Box::new(move |creation| Ok(Box::new(app::DawApp::new(&creation.egui_ctx, session, options, app_failure)))),
    )
    .map_err(|error| anyhow!("the window could not be opened: {error}"))?;

    match failure.lock().take() {
        Some(reason) => Err(anyhow!(reason)),
        None => Ok(()),
    }
}
