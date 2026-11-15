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
//! The look is Instrument Noir, from the `daw-noir` crate: its
//! [`daw_noir::Theme`] holds every color, type size and metric, and its
//! widgets (bars, chips, tabs, meters) are what the views are made of.
//! Views place those widgets by hand, at exact rectangles, and never pick a
//! color or a font size of their own.
//!
//! Modules: [`app`] is the shell (panels, shortcuts, repaint policy).
//! [`top_bar`], [`status_bar`], [`browser`], [`arrangement`] and [`detail`]
//! (with [`piano_roll`] and [`devices`]) are the views. [`device_layouts`]
//! says how each built-in device arranges its parameters, and [`displays`]
//! draws their small curves from the math in [`response`]. [`files`] runs
//! dialogs and exports off the UI thread; [`timeline`] and [`units`] hold
//! the pure math; [`colors`] maps stored track colors to palette hues; and
//! [`widgets`] is the glue between the views and the kit.

mod app;
mod arrangement;
mod brand;
mod browser;
mod colors;
mod detail;
mod device_layouts;
mod devices;
mod displays;
mod files;
mod piano_roll;
mod response;
mod snapshot;
mod state;
mod status_bar;
mod timeline;
mod toast;
mod top_bar;
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

/// The smallest the window can be made: every part of it still fits.
const MIN_WINDOW_SIZE: [f32; 2] = [1060.0, 640.0];

/// Names of the themes the window can be painted in, the default first.
pub fn theme_names() -> Vec<&'static str> {
    daw_noir::Theme::all().iter().map(|theme| theme.name).collect()
}

/// How the window starts up. The defaults are an ordinary interactive run.
#[derive(Clone, Debug)]
pub struct UiOptions {
    /// The detail tab shown first.
    pub detail_tab: DetailTab,
    /// A clip to select at startup.
    pub select_clip: Option<ClipId>,
    /// Address of the MCP server AI assistants can connect to, if it runs.
    pub mcp_url: Option<String>,
    /// Name of the theme to start in; one of [`theme_names`].
    pub theme: &'static str,
    /// Whether the device browser is open at startup.
    pub browser: bool,
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
            theme: theme_names()[0],
            browser: true,
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
            .with_title("Trill")
            .with_icon(brand::icon(64))
            .with_inner_size(options.window_size)
            .with_min_inner_size(MIN_WINDOW_SIZE),
        ..Default::default()
    };
    // Failures inside the event loop (a screenshot that could not be saved)
    // have no way to return a value, so they are left here.
    let failure = Arc::new(Mutex::new(None));
    let app_failure = failure.clone();
    eframe::run_native(
        "trill",
        native,
        Box::new(move |creation| Ok(Box::new(app::DawApp::new(&creation.egui_ctx, session, options, app_failure)))),
    )
    .map_err(|error| anyhow!("the window could not be opened: {error}"))?;

    match failure.lock().take() {
        Some(reason) => Err(anyhow!(reason)),
        None => Ok(()),
    }
}
