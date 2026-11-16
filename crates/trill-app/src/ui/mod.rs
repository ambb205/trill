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
//! The look is Instrument Noir, from the `trill-noir` crate: its
//! [`trill_noir::Theme`] holds every color, type size and metric, and its
//! widgets (bars, chips, tabs, meters) are what the views are made of.
//! Views place those widgets by hand, at exact rectangles, and never pick a
//! color or a font size of their own.
//!
//! Modules: [`app`] is the shell (panels, repaint policy) and
//! [`shortcuts`] the table of keys it reads. [`top_bar`], [`status_bar`],
//! [`browser`], [`arrangement`] and [`detail`] (with [`piano_roll`],
//! [`audio_clip`], [`devices`] and [`mixer`]) are the views; the
//! arrangement and the browser are modules of their own parts.
//! [`automation`] is what the views share about envelopes.
//! [`device_layouts`] says how each built-in device arranges its
//! parameters, [`sample_panels`] draws what the sampler and the drum rack
//! play, and [`displays`] draws the devices' small curves from the math in
//! [`response`]. [`files`] runs dialogs, imports and exports off the UI
//! thread, and [`sounds`] does the same for everything that reads audio
//! files (waveforms, previews, searches, drops) on the workers of
//! [`work`]; [`dragging`] carries sounds from the browser, or from another
//! program, to where they are dropped. [`timeline`] and [`units`] hold the
//! pure math; [`colors`] maps stored track colors to palette hues; and
//! [`widgets`] is the glue between the views and the kit.

mod app;
mod arrangement;
mod audio_clip;
mod automation;
mod brand;
mod browser;
mod colors;
mod detail;
mod device_layouts;
mod devices;
mod displays;
mod dragging;
mod files;
mod mixer;
mod piano_roll;
mod response;
mod sample_panels;
mod shortcuts;
mod snapshot;
mod sounds;
mod state;
mod status_bar;
mod timeline;
mod toast;
mod top_bar;
mod units;
mod widgets;
mod work;

#[cfg(test)]
mod tests;

pub use state::{BrowserTab, DetailTab};

use anyhow::anyhow;
use eframe::egui;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use trill_core::ClipId;
use trill_runtime::Runtime;

/// The smallest the window can be made: every part of it still fits.
const MIN_WINDOW_SIZE: [f32; 2] = [1060.0, 640.0];

/// Names of the themes the window can be painted in, the default first.
pub fn theme_names() -> Vec<&'static str> {
    trill_noir::Theme::all().iter().map(|theme| theme.name).collect()
}

/// How the window starts up. The defaults are an ordinary interactive run.
#[derive(Clone, Debug)]
pub struct UiOptions {
    /// The detail tab shown first.
    pub detail_tab: DetailTab,
    /// A clip to select at startup.
    pub select_clip: Option<ClipId>,
    /// More clips to select with it.
    pub also_select: Vec<ClipId>,
    /// Unfold a lane for every envelope the project has.
    pub show_automation: bool,
    /// Open with the shortcut reference up.
    pub show_shortcuts: bool,
    /// Address of the MCP server AI assistants can connect to, if it runs.
    pub mcp_url: Option<String>,
    /// Name of the theme to start in; one of [`theme_names`].
    pub theme: &'static str,
    /// Whether the browser is open at startup.
    pub browser: bool,
    /// What the browser lists at startup.
    pub browser_tab: BrowserTab,
    /// What to search the sound library for at startup.
    pub sound_search: Option<String>,
    /// Name of a sound to select once the library has it.
    pub select_sound: Option<String>,
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
            also_select: Vec::new(),
            show_automation: false,
            show_shortcuts: false,
            mcp_url: None,
            theme: theme_names()[0],
            browser: true,
            browser_tab: BrowserTab::default(),
            sound_search: None,
            select_sound: None,
            screenshot: None,
            autoplay: false,
            exit_after: None,
            window_size: [1440.0, 900.0],
        }
    }
}

/// Opens the window and runs until it is closed.
pub fn run(runtime: Arc<Runtime>, options: UiOptions) -> anyhow::Result<()> {
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
        Box::new(move |creation| Ok(Box::new(app::TrillApp::new(&creation.egui_ctx, runtime, options, app_failure)))),
    )
    .map_err(|error| anyhow!("the window could not be opened: {error}"))?;

    match failure.lock().take() {
        Some(reason) => Err(anyhow!(reason)),
        None => Ok(()),
    }
}
