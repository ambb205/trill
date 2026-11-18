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
//! [`automation`] is what the views share about envelopes, and
//! [`controller_lane`] the lane under the piano roll's notes that edits a
//! clip's MIDI controllers with the same gestures. [`keyboard`]
//! is the computer keyboard played as an instrument, [`recording`] the
//! window's part in arming tracks and recording takes, and
//! [`audio_settings`] the Audio and MIDI panel, [`setup`] the panel of
//! what Trill fetches when asked and of what runs the models.
//! [`device_layouts`] says how each built-in device arranges its
//! parameters, [`sample_panels`] draws what the sampler and the drum rack
//! play, and [`displays`] draws the devices' small curves from the math in
//! [`response`]. [`editors`] opens the windows plugins bring with them,
//! and [`recovery`] offers back the work a crash left unsaved. [`missing`]
//! lists the files the song names that are not on disk, with what was
//! found for each, and relinks them.
//! [`making_of`] plays the session of the song again from its journal, and
//! [`stage`] shows the song filmed as it plays, in place of all of these.
//! [`files`] runs dialogs, imports and exports off the UI
//! thread, and [`sounds`] does the same for everything that reads audio
//! files (waveforms, previews, searches, drops) on the workers of
//! [`work`]; [`dragging`] carries sounds from the browser, or from another
//! program, to where they are dropped. [`timeline`] and [`units`] hold the
//! pure math; [`colors`] maps stored track colors to palette hues; and
//! [`widgets`] is the glue between the views and the kit.

mod app;
mod arrangement;
mod assistant;
mod audio_clip;
mod audio_settings;
mod automation;
mod brand;
mod browser;
mod colors;
mod controller_lane;
mod credits;
mod detail;
mod device_layouts;
mod devices;
mod displays;
mod dragging;
mod editors;
mod files;
mod filming;
mod instrument_panel;
mod keyboard;
mod making_of;
mod missing;
mod mixer;
mod piano_roll;
mod recording;
mod recovery;
mod response;
mod sample_panels;
mod setup;
mod setup_scene;
mod shortcuts;
mod snapshot;
mod sound_presets;
mod sounds;
mod stage;
mod state;
mod status_bar;
mod study;
mod timeline;
mod toast;
mod top_bar;
mod units;
mod wavetable_panel;
mod widgets;
mod work;

#[cfg(test)]
mod tests;

pub use assistant::{Options as AssistantOptions, Scene as AssistantScene};
pub use keyboard::Options as KeyboardOptions;
pub use making_of::Start as MakingOfStart;
pub use making_of::scene::play as play_making_of_scene;
pub use setup_scene::SetupScene;
pub use state::{BrowserTab, DetailTab};
pub use study::{Shown as StudyShown, Side as StudySide};

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
    /// The MIDI controller the lane under the piano roll's notes shows
    /// first; `None` for velocity.
    pub lane: Option<trill_core::ControllerKind>,
    /// Open with the shortcut reference up.
    pub show_shortcuts: bool,
    /// Address of the MCP server AI assistants can connect to, if it runs.
    pub mcp_url: Option<String>,
    /// Name of the theme to start in; one of [`theme_names`].
    pub theme: &'static str,
    /// Whether the browser is open at startup.
    pub browser: bool,
    /// What the browser lists at startup; `None` for what it listed when
    /// the program was last closed.
    pub browser_tab: Option<BrowserTab>,
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
    /// Name of a device whose own window to open at startup.
    pub open_editor: Option<String>,
    /// Initial size of the window's content, in points.
    pub window_size: [f32; 2],
    /// How the assistant's panel starts.
    pub assistant: AssistantOptions,
    /// How the computer keyboard starts, when not as it was left.
    pub keyboard: KeyboardOptions,
    /// Open with the Audio and MIDI panel up.
    pub show_settings: bool,
    /// Open with the Setup panel up.
    pub show_setup: bool,
    /// A made-up machine to show in it, in place of this one.
    pub setup_scene: Option<SetupScene>,
    /// A study to open in the workspace at startup, by its id.
    pub study: Option<String>,
    /// How it is shown: bars selected, the list beside the lanes.
    pub study_shown: Option<StudyShown>,
    /// Open with the dialog that asks for a recording to study up.
    pub show_study_dialog: bool,
    /// Open with the film panel up.
    pub show_film: bool,
    /// Open with the list of missing files up.
    pub show_missing: bool,
    /// Open with the credits up.
    pub show_credits: bool,
    /// Start a take on the armed tracks as soon as the window opens.
    pub record: bool,
    /// Open on the stage.
    pub stage: bool,
    /// Open with the making-of of the song up, started so.
    pub making_of: Option<MakingOfStart>,
    /// Open watching the making-of that was written out to this file.
    pub watch: Option<PathBuf>,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            detail_tab: DetailTab::default(),
            select_clip: None,
            also_select: Vec::new(),
            show_automation: false,
            lane: None,
            show_shortcuts: false,
            mcp_url: None,
            theme: theme_names()[0],
            browser: true,
            browser_tab: None,
            sound_search: None,
            select_sound: None,
            screenshot: None,
            autoplay: false,
            exit_after: None,
            open_editor: None,
            window_size: [1440.0, 900.0],
            assistant: AssistantOptions::default(),
            keyboard: KeyboardOptions::default(),
            show_settings: false,
            show_setup: false,
            setup_scene: None,
            study: None,
            study_shown: None,
            show_study_dialog: false,
            show_film: false,
            show_missing: false,
            show_credits: false,
            record: false,
            stage: false,
            making_of: None,
            watch: None,
        }
    }
}

/// The system's handle of the application's window, as a plugin editor's
/// owner is named: an `HWND` on Windows. `None` elsewhere, where editors
/// are not hosted yet.
fn native_window(creation: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match creation.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(window) => Some(window.hwnd.get()),
        _ => None,
    }
}

/// Where a window that must not be seen is put: far up and to the left of
/// any screen.
const OFF_SCREEN: egui::Pos2 = egui::pos2(-20_000.0, -20_000.0);

/// Opens the window and runs until it is closed.
pub fn run(runtime: Arc<Runtime>, options: UiOptions) -> anyhow::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Trill")
        .with_icon(brand::icon(64))
        .with_inner_size(options.window_size)
        .with_min_inner_size(MIN_WINDOW_SIZE);
    if options.screenshot.is_some() {
        // A window that is only there to have its picture taken stays out
        // of the way of whoever is at the machine: off the screen, off the
        // taskbar, and without taking the keyboard. (Hidden would be
        // simpler, but a hidden window is never asked to paint.)
        viewport = viewport.with_position(OFF_SCREEN).with_active(false).with_taskbar(false);
    }
    let native = eframe::NativeOptions { viewport, ..Default::default() };
    // Failures inside the event loop (a screenshot that could not be saved)
    // have no way to return a value, so they are left here.
    let failure = Arc::new(Mutex::new(None));
    let app_failure = failure.clone();
    eframe::run_native(
        "trill",
        native,
        Box::new(move |creation| {
            let mut app = app::TrillApp::new(&creation.egui_ctx, runtime, options, app_failure);
            // Plugin editors belong to this window: they stay above it and
            // go where it goes.
            app.set_native_window(native_window(creation));
            // The stage draws on the window's own graphics device.
            app.set_graphics(creation.wgpu_render_state.clone());
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow!("the window could not be opened: {error}"))?;

    match failure.lock().take() {
        Some(reason) => Err(anyhow!(reason)),
        None => Ok(()),
    }
}
