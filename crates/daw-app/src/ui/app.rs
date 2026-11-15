//! The application shell: keeps the snapshot in step with the session, lays
//! out the panels, handles keyboard shortcuts and decides when to repaint.

use super::files::{self, Confirm, Jobs};
use super::snapshot::{Live, Snapshot};
use super::state::{AudioSummary, Cx, FocusArea, UiState};
use super::{UiOptions, arrangement, detail, master, piano_roll, theme, transport};
use daw_core::device::{DeviceDescriptor, DeviceFactory};
use daw_dsp::BuiltinFactory;
use daw_engine::SharedSession;
use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// How often an idle window checks the session for edits made elsewhere.
const IDLE_POLL: Duration = Duration::from_millis(100);
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(1);
/// Height the arrangement keeps however tall the detail panel is made.
const MIN_ARRANGEMENT_HEIGHT: f32 = 180.0;
/// Share of the space below the transport the detail panel starts with, up
/// to its default height.
const DETAIL_SHARE: f32 = 0.47;
/// Frames to let the layout settle before taking a screenshot.
const SCREENSHOT_SETTLE_FRAMES: u32 = 12;

/// Where an unattended run (`--screenshot`, `--exit-after`) stands.
enum Capture {
    NotRequested,
    /// Waiting for the renderer to hand over the pixels and for them to be
    /// written; holds the outcome once there is one.
    Pending(Arc<Mutex<Option<Result<(), String>>>>),
    /// The window has been asked to close.
    Done,
}

/// A command issued from the keyboard.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Command {
    Undo,
    Redo,
    New,
    Open,
    Save,
    SaveAs,
    Export,
    TogglePlay,
    ToggleLoop,
    Rewind,
    AddTrack,
    Duplicate,
    Delete,
    SelectAll,
    /// Move the selected notes by grid steps and semitones.
    Nudge(i32, i32),
}

pub struct DawApp {
    session: SharedSession,
    snapshot: Snapshot,
    live: Live,
    state: UiState,
    jobs: Jobs,
    /// The devices that can be added. Starts as the built-in ones and is
    /// replaced when `catalog` delivers the full list.
    descriptors: Vec<DeviceDescriptor>,
    /// The full device catalog, plugins included, read on a helper thread:
    /// asking for it waits for the first plugin scan, which can take seconds.
    catalog: mpsc::Receiver<Vec<DeviceDescriptor>>,
    audio: AudioSummary,
    options: UiOptions,
    /// Receives the reason if the run fails in a way `ui::run` must report.
    failure: Arc<Mutex<Option<String>>>,
    capture: Capture,
    started: Instant,
    frames: u32,
    last_meter_update: Instant,
    last_housekeeping: Instant,
    /// The window title last sent, to avoid resending it every frame.
    title: String,
}

impl DawApp {
    pub fn new(
        ctx: &egui::Context,
        session: SharedSession,
        options: UiOptions,
        failure: Arc<Mutex<Option<String>>>,
    ) -> Self {
        theme::install(ctx);
        // Edits are made from inside the UI pass. By default egui may run a
        // pass twice with the same input to hide a layout glitch, which
        // would apply a click's edit twice; one pass per frame rules it out.
        ctx.options_mut(|options| options.max_passes = NonZeroUsize::MIN);

        let mut state = UiState::new(options.detail_tab);
        let (snapshot, factory, audio) = {
            let mut guard = session.lock();
            let first = Snapshot::capture(&guard, None);
            state.reconcile(&first.project);
            if let Some((track, clip)) = options.select_clip.and_then(|id| first.clip(id)) {
                state.select_clip(track.id, clip.id);
            }
            if options.autoplay {
                guard.play();
            }
            let info = guard.audio_info();
            let audio = AudioSummary { output: info.output.clone(), warning: info.warning.clone() };
            // Captured again now that the selection is known, so it covers
            // the selected track's devices.
            (Snapshot::capture(&guard, state.selected_track), guard.factory(), audio)
        };
        let (catalog_tx, catalog) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            // The receiver is gone if the window closed first.
            if catalog_tx.send(factory.descriptors()).is_ok() {
                repaint.request_repaint();
            }
        });

        let now = Instant::now();
        let mut jobs = Jobs::default();
        // Unattended runs must be able to exit without a person answering
        // the "discard changes?" question.
        jobs.quit_confirmed = options.screenshot.is_some() || options.exit_after.is_some();
        Self {
            session,
            snapshot,
            live: Live::default(),
            state,
            jobs,
            descriptors: BuiltinFactory.descriptors(),
            catalog,
            audio,
            options,
            failure,
            capture: Capture::NotRequested,
            started: now,
            frames: 0,
            last_meter_update: now,
            last_housekeeping: now,
            title: String::new(),
        }
    }

    /// Brings the snapshot and the live values up to date with the session.
    ///
    /// Uses `try_lock`: if another front end is in the middle of an edit,
    /// this frame is painted from what we already have rather than waiting.
    fn sync(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if let Ok(descriptors) = self.catalog.try_recv() {
            self.descriptors = descriptors;
        }
        match self.session.try_lock() {
            Some(mut session) => {
                if session.revision() != self.snapshot.revision {
                    self.snapshot = Snapshot::capture(&session, self.state.selected_track);
                    self.state.reconcile(&self.snapshot.project);
                }
                // The selection moved (by the user, or by `reconcile` just
                // now): cover the newly selected track's devices.
                if self.snapshot.params_track != self.state.selected_track {
                    self.snapshot = Snapshot::capture(&session, self.state.selected_track);
                }
                let transport = session.transport();
                self.live.playing = transport.playing;
                self.live.position = transport.position_beats;
                self.live.set_cpu_load(session.audio_load());
                self.live.audio_faults = session.audio_faults();
                if now.duration_since(self.last_housekeeping) >= HOUSEKEEPING_INTERVAL {
                    session.housekeeping();
                    self.last_housekeeping = now;
                }
            }
            None => ctx.request_repaint_after(Duration::from_millis(8)),
        }

        // Meters are read through their own handles; no lock involved.
        let elapsed = now.duration_since(self.last_meter_update).as_secs_f32().min(0.25);
        self.last_meter_update = now;
        self.live.update_meters(&self.snapshot, elapsed);
        if std::mem::take(&mut self.state.clear_clip_indicator) {
            self.live.master.clipped = false;
        }
    }

    fn cx(&mut self) -> Cx<'_> {
        Cx {
            session: &self.session,
            snap: &self.snapshot,
            live: &self.live,
            descriptors: &self.descriptors,
            audio: &self.audio,
            mcp_url: self.options.mcp_url.as_deref(),
            state: &mut self.state,
            jobs: &mut self.jobs,
        }
    }

    /// Releases auditioned notes whose time is up or whose key was let go.
    fn release_audition(&mut self, ctx: &egui::Context) {
        let Some(audition) = self.state.audition else { return };
        let done = match audition.release_at {
            Some(time) => Instant::now() >= time,
            None => !ctx.input(|input| input.pointer.primary_down()),
        };
        if done {
            self.cx().stop_audition();
        }
    }

    /// Closes the undo gesture of a drag once the mouse button is up.
    fn close_gesture(&mut self, ctx: &egui::Context) {
        if self.state.gesture_open && !ctx.input(|input| input.pointer.any_down()) {
            self.session.lock().end_gesture();
            self.state.gesture_open = false;
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let marker = if self.snapshot.modified { " \u{2022}" } else { "" };
        let title = format!("{}{marker} - DAW", self.snapshot.project.name);
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    /// Asks before closing a window with unsaved work.
    fn guard_close(&mut self, ctx: &egui::Context) {
        let closing = ctx.input(|input| input.viewport().close_requested());
        if closing && self.snapshot.modified && !self.jobs.quit_confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.jobs.confirm = Some(Confirm::Quit);
        }
    }

    /// Drives `--screenshot` and `--exit-after`.
    fn automate(&mut self, ctx: &egui::Context) {
        let elapsed = self.started.elapsed();
        if let Some(path) = self.options.screenshot.clone() {
            // Playing takes a moment to show on the meters.
            let settle = if self.options.autoplay { Duration::from_millis(1800) } else { Duration::from_millis(400) };
            match &self.capture {
                Capture::NotRequested if self.frames >= SCREENSHOT_SETTLE_FRAMES && elapsed >= settle => {
                    let outcome = Arc::new(Mutex::new(None));
                    let (slot, waker) = (outcome.clone(), ctx.clone());
                    ctx.request_screenshot(move |image| {
                        *slot.lock() = Some(save_png(&path, &image));
                        waker.request_repaint();
                    });
                    self.capture = Capture::Pending(outcome);
                }
                Capture::Pending(outcome) => {
                    let finished = outcome.lock().take();
                    if let Some(result) = finished {
                        if let Err(reason) = result {
                            *self.failure.lock() = Some(format!("could not save the screenshot: {reason}"));
                        }
                        self.capture = Capture::Done;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                _ => {}
            }
            ctx.request_repaint();
        } else if let Some(limit) = self.options.exit_after {
            if elapsed >= limit && !matches!(self.capture, Capture::Done) {
                // A smoke test has nothing else to go by, so say what
                // happened: where playback got to, on what output.
                eprintln!(
                    "daw: closing after {:.1} s; {} at beat {:.2}; output: {}; {} audio fault(s)",
                    elapsed.as_secs_f32(),
                    if self.live.playing { "playing" } else { "stopped" },
                    self.live.position,
                    self.audio.output,
                    self.live.audio_faults,
                );
                self.capture = Capture::Done;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(limit.saturating_sub(elapsed));
        }
    }

    /// The part of a frame that needs no window: catching up with the
    /// session and with background work.
    pub fn tick(&mut self, ctx: &egui::Context) {
        self.jobs.poll(&mut self.state);
        self.sync(ctx);
        self.release_audition(ctx);
        // Edits from other front ends arrive without any input event, so an
        // idle window has to look for them.
        ctx.request_repaint_after(IDLE_POLL);
    }

    /// Lays out and paints the whole window into `ui`.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.frames += 1;
        self.guard_close(&ctx);
        self.update_title(&ctx);

        let commands = if self.jobs.confirm.is_some() { Vec::new() } else { read_shortcuts(&ctx, &self.state) };
        let mut cx = self.cx();
        for command in commands {
            run_command(&ctx, &mut cx, command);
        }

        egui::Panel::top("transport")
            .exact_size(transport::HEIGHT)
            .show_separator_line(false)
            .frame(theme::panel_frame(theme::BG_PANEL))
            .show(ui, |ui| transport::show(ui, &mut cx));
        egui::Panel::right("master")
            .exact_size(master::WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(theme::panel_frame(theme::BG_PANEL))
            .show(ui, |ui| master::show(ui, &mut cx));
        // The detail panel may take most of the window, but never so much
        // that the arrangement above it disappears.
        let below_transport = ui.available_height();
        let tallest = (below_transport - MIN_ARRANGEMENT_HEIGHT).clamp(detail::MIN_HEIGHT, detail::MAX_HEIGHT);
        let default = (below_transport * DETAIL_SHARE).clamp(detail::MIN_HEIGHT, detail::DEFAULT_HEIGHT.min(tallest));
        egui::Panel::bottom("detail")
            .resizable(true)
            .default_size(default)
            .size_range(detail::MIN_HEIGHT..=tallest)
            .show_separator_line(false)
            .frame(theme::panel_frame(theme::BG_PANEL))
            .show(ui, |ui| detail::show(ui, &mut cx));
        egui::CentralPanel::no_frame().show(ui, |ui| arrangement::show(ui, &mut cx));

        files::confirm_modal(&ctx, &mut cx);
        let corner = ctx.content_rect().right_bottom() - egui::vec2(master::WIDTH + 14.0, 14.0);
        cx.state.toasts.show(&ctx, corner);

        self.close_gesture(&ctx);
        // Moving things need every frame; everything else waits for input
        // or the idle poll scheduled in `logic`.
        if self.live.playing || self.live.meters_moving() || self.state.audition.is_some() {
            ctx.request_repaint();
        }
        self.automate(&ctx);
    }

    #[cfg(test)]
    pub fn state(&self) -> &UiState {
        &self.state
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
}

impl eframe::App for DawApp {
    /// Runs before every frame, and on its own while the window is hidden,
    /// so the session keeps being polled and tidied when minimized.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

/// Collects the commands for the keys pressed this frame. Single keys and
/// editing shortcuts are left alone while a text field has the keyboard.
fn read_shortcuts(ctx: &egui::Context, state: &UiState) -> Vec<Command> {
    const COMMAND_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
    // More specific chords first: a chord also matches one with fewer
    // modifiers.
    let always = [
        (COMMAND_SHIFT, Key::S, Command::SaveAs),
        (Modifiers::COMMAND, Key::S, Command::Save),
        (Modifiers::COMMAND, Key::O, Command::Open),
        (Modifiers::COMMAND, Key::N, Command::New),
        (Modifiers::COMMAND, Key::E, Command::Export),
    ];
    let unless_typing = [
        (COMMAND_SHIFT, Key::Z, Command::Redo),
        (Modifiers::COMMAND, Key::Y, Command::Redo),
        (Modifiers::COMMAND, Key::Z, Command::Undo),
        (Modifiers::COMMAND, Key::T, Command::AddTrack),
        (Modifiers::COMMAND, Key::D, Command::Duplicate),
        (Modifiers::NONE, Key::Space, Command::TogglePlay),
        (Modifiers::NONE, Key::L, Command::ToggleLoop),
        (Modifiers::NONE, Key::Home, Command::Rewind),
        (Modifiers::NONE, Key::Delete, Command::Delete),
        (Modifiers::NONE, Key::Backspace, Command::Delete),
    ];
    // Only claimed while notes are selected in the piano roll; otherwise
    // these keys keep their usual meaning.
    let with_notes = [
        (Modifiers::COMMAND, Key::A, Command::SelectAll),
        (Modifiers::NONE, Key::ArrowLeft, Command::Nudge(-1, 0)),
        (Modifiers::NONE, Key::ArrowRight, Command::Nudge(1, 0)),
        (Modifiers::NONE, Key::ArrowUp, Command::Nudge(0, 1)),
        (Modifiers::NONE, Key::ArrowDown, Command::Nudge(0, -1)),
    ];

    let typing = ctx.text_edit_focused();
    let in_piano_roll = state.focus == FocusArea::PianoRoll && state.selected_clip.is_some();
    let mut commands = Vec::new();
    ctx.input_mut(|input| {
        let mut collect = |bindings: &[(Modifiers, Key, Command)]| {
            for (modifiers, key, command) in bindings {
                if input.consume_shortcut(&KeyboardShortcut::new(*modifiers, *key)) {
                    commands.push(*command);
                }
            }
        };
        collect(&always);
        if !typing {
            if in_piano_roll {
                collect(&with_notes);
            }
            collect(&unless_typing);
        }
    });
    commands
}

fn run_command(ctx: &egui::Context, cx: &mut Cx, command: Command) {
    match command {
        Command::Undo => drop(cx.with_session(|session| session.undo())),
        Command::Redo => drop(cx.with_session(|session| session.redo())),
        Command::New => files::new_project(cx),
        Command::Open => files::open(ctx, cx),
        Command::Save => files::save(ctx, cx),
        Command::SaveAs => files::save_as(ctx, cx),
        Command::Export => files::export_wav(ctx, cx),
        Command::TogglePlay => transport::toggle_play(cx),
        Command::ToggleLoop => transport::toggle_loop(cx),
        Command::Rewind => cx.with_session(|session| session.seek(0.0)),
        Command::AddTrack => arrangement::add_track(cx),
        Command::Duplicate => arrangement::duplicate_selected_clip(cx),
        // Delete acts on notes when they are what the user is working on,
        // otherwise on the selected clip.
        Command::Delete => {
            if cx.state.focus == FocusArea::PianoRoll && !cx.state.selected_notes.is_empty() {
                piano_roll::delete_selected_notes(cx);
            } else if cx.state.focus == FocusArea::Arrangement {
                arrangement::delete_selected_clip(cx);
            }
        }
        Command::SelectAll => piano_roll::select_all_notes(cx),
        Command::Nudge(steps, semitones) => piano_roll::nudge_selected_notes(cx, steps, semitones),
    }
}

/// Writes the window's pixels to `path` as a PNG.
fn save_png(path: &Path, image: &egui::ColorImage) -> Result<(), String> {
    if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
        std::fs::create_dir_all(folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    }
    let [width, height] = image.size;
    image::save_buffer(path, image.as_raw(), width as u32, height as u32, image::ExtendedColorType::Rgba8)
        .map_err(|error| format!("{}: {error}", path.display()))
}
