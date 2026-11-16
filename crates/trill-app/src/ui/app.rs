//! The application shell: keeps the snapshot in step with the session, lays
//! out the panels, handles keyboard shortcuts and decides when to repaint.

use super::arrangement::ops;
use super::files::{self, Confirm, Jobs};
use super::shortcuts::{self, Command};
use super::snapshot::{Live, Snapshot};
use super::state::{AudioSummary, Cx, FocusArea, LaneOwner, Tool, UiState, lane_owner};
use super::{UiOptions, arrangement, automation, browser, detail, piano_roll, status_bar, top_bar};
use eframe::egui;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use trill_core::TrackId;
use trill_core::device::{DeviceDescriptor, DeviceFactory};
use trill_dsp::BuiltinFactory;
use trill_engine::SharedSession;
use trill_noir::Theme;

/// How often an idle window checks the session for edits made elsewhere.
const IDLE_POLL: Duration = Duration::from_millis(100);
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(1);
/// Height the arrangement keeps however tall the detail panel is made:
/// the ruler, two tracks and the master.
const MIN_ARRANGEMENT_HEIGHT: f32 = 200.0;
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

pub struct TrillApp {
    session: SharedSession,
    snapshot: Snapshot,
    live: Live,
    state: UiState,
    jobs: Jobs,
    /// The colors, type and metrics the window is painted with.
    theme: Theme,
    /// The devices that can be added. Starts as the built-in ones and is
    /// replaced when `catalog` delivers the full list.
    descriptors: Vec<DeviceDescriptor>,
    /// The full device catalog, plugins included, read on a helper thread:
    /// asking for it waits for the first plugin scan, which can take seconds.
    catalog: mpsc::Receiver<Vec<DeviceDescriptor>>,
    /// True once `catalog` has delivered; until then the scan is running.
    catalog_ready: bool,
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
    /// Whether a text field had the keyboard when the last frame ended.
    was_typing: bool,
}

impl TrillApp {
    pub fn new(
        ctx: &egui::Context,
        session: SharedSession,
        options: UiOptions,
        failure: Arc<Mutex<Option<String>>>,
    ) -> Self {
        let theme = Theme::named(options.theme).unwrap_or_else(Theme::noir);
        trill_noir::install(ctx, &theme);
        // Edits are made from inside the UI pass. By default egui may run a
        // pass twice with the same input to hide a layout glitch, which
        // would apply a click's edit twice; one pass per frame rules it out.
        ctx.options_mut(|options| options.max_passes = NonZeroUsize::MIN);

        let mut state = UiState::new(options.detail_tab);
        state.browser.open = options.browser;
        state.shortcuts_open = options.show_shortcuts;
        let (snapshot, factory, audio) = {
            let mut guard = session.lock();
            let first = Snapshot::capture(&guard, &[]);
            state.reconcile(&first.project);
            if let Some((track, clip)) = options.select_clip.and_then(|id| first.clip(id)) {
                state.select_clip(track.id, clip.id);
            }
            for (track, clip) in options.also_select.iter().filter_map(|id| first.clip(*id)) {
                if !state.is_clip_selected(clip.id) {
                    state.toggle_clip(track.id, clip.id);
                }
            }
            if options.show_automation {
                for lane in &first.project.automation {
                    if let Some(owner) = lane_owner(&first.project, &lane.target) {
                        state.arrangement.open_lane(owner, lane.target.clone());
                    }
                }
            }
            if options.autoplay {
                guard.play();
            }
            let info = guard.audio_info();
            let audio = AudioSummary {
                output: info.output.clone(),
                sample_rate: info.sample_rate,
                warning: info.warning.clone(),
            };
            // Captured again now that the selection is known, so it covers
            // the devices of the tracks on show.
            (Snapshot::capture(&guard, &params_tracks(&state)), guard.factory(), audio)
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
            theme,
            descriptors: BuiltinFactory.descriptors(),
            catalog,
            catalog_ready: false,
            audio,
            options,
            failure,
            capture: Capture::NotRequested,
            started: now,
            frames: 0,
            last_meter_update: now,
            last_housekeeping: now,
            title: String::new(),
            was_typing: false,
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
            self.catalog_ready = true;
        }
        match self.session.try_lock() {
            Some(mut session) => {
                if session.revision() != self.snapshot.revision {
                    self.snapshot = Snapshot::capture(&session, &params_tracks(&self.state));
                    self.state.reconcile(&self.snapshot.project);
                }
                // The selection moved or a lane was opened (by the user,
                // or by `reconcile` just now): cover the devices now shown.
                let wanted = params_tracks(&self.state);
                if !self.snapshot.covers(&wanted) {
                    self.snapshot = Snapshot::capture(&session, &wanted);
                }
                let transport = session.transport();
                self.live.playing = transport.playing;
                self.live.position = transport.position_beats;
                self.live.metronome = session.metronome();
                self.live.set_cpu_load(session.audio_load());
                self.live.audio_faults = session.audio_faults();
                if now.duration_since(self.last_housekeeping) >= HOUSEKEEPING_INTERVAL {
                    session.housekeeping();
                    self.last_housekeeping = now;
                }
            }
            None => ctx.request_repaint_after(Duration::from_millis(8)),
        }

        self.state.resolve_device(&self.snapshot.project);

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
            theme: &self.theme,
            descriptors: &self.descriptors,
            catalog_ready: self.catalog_ready,
            audio: &self.audio,
            mcp_url: self.options.mcp_url.as_deref(),
            state: &mut self.state,
            jobs: &mut self.jobs,
        }
    }

    /// The commands for the keys pressed this frame. A question on screen
    /// takes the keyboard; the shortcut reference lets only its own key
    /// through, which closes it.
    fn read_commands(&self, ctx: &egui::Context) -> Vec<Command> {
        if self.jobs.confirm.is_some() {
            return Vec::new();
        }
        let state = &self.state;
        let selection = match state.focus {
            FocusArea::Arrangement => !state.selected_clips.is_empty(),
            FocusArea::PianoRoll => !state.selected_notes.is_empty(),
            FocusArea::Automation => state.arrangement.points.as_ref().is_some_and(|chosen| !chosen.points.is_empty()),
        };
        // The key that ends typing (Escape, Enter) takes the focus away
        // before this runs, and must not then be read as a shortcut: the
        // field that just lost the keyboard still has to see it.
        let typing = ctx.text_edit_focused() || self.was_typing;
        let mut commands = shortcuts::read(ctx, typing, selection);
        if state.shortcuts_open {
            commands.retain(|command| *command == Command::ShowShortcuts);
        }
        commands
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
        let title = format!("{}{marker} - Trill", self.snapshot.project.name);
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
                    "trill: closing after {:.1} s; {} at beat {:.2}; output: {}; {} audio fault(s)",
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

        let commands = self.read_commands(&ctx);
        let mut cx = self.cx();
        for command in commands {
            run_command(&ctx, &mut cx, command);
        }

        // Panels are claimed from the outside in: the bars, the browser
        // down the left, the detail panel under what is left, and the
        // arrangement in the middle. Each paints its own surface.
        let bare = egui::Frame::NONE;
        egui::Panel::top("top-bar")
            .exact_size(top_bar::HEIGHT)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| top_bar::show(ui, &mut cx));
        egui::Panel::bottom("status-bar")
            .exact_size(status_bar::HEIGHT)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| status_bar::show(ui, &mut cx));
        if cx.state.browser.open {
            egui::Panel::left("browser")
                .exact_size(browser::WIDTH)
                .resizable(false)
                .show_separator_line(false)
                .frame(bare)
                .show(ui, |ui| browser::show(ui, &mut cx));
        }
        // The detail panel may take most of the window, but never so much
        // that the arrangement above it disappears.
        let between_bars = ui.available_height();
        let tallest = (between_bars - MIN_ARRANGEMENT_HEIGHT).clamp(detail::MIN_HEIGHT, detail::MAX_HEIGHT);
        egui::Panel::bottom("detail")
            .resizable(true)
            .default_size(detail::DEFAULT_HEIGHT.min(tallest))
            .size_range(detail::MIN_HEIGHT..=tallest)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| detail::show(ui, &mut cx));
        egui::CentralPanel::no_frame().show(ui, |ui| arrangement::show(ui, &mut cx));

        files::confirm_modal(&ctx, &mut cx);
        shortcuts::window(&ctx, &mut cx);
        let corner = ctx.content_rect().right_bottom() - egui::vec2(12.0, status_bar::HEIGHT + 10.0);
        cx.state.toasts.show(&ctx, cx.theme, corner);

        // The status bar is laid out before the views that ask for hints,
        // so a new hint is shown by the frame after this one.
        if self.state.hint.end_frame() {
            ctx.request_repaint();
        }
        // What copy and cut left for the system clipboard.
        if let Some(text) = self.state.copied_text.take() {
            ctx.copy_text(text);
        }
        if let Some(theme) = self.state.theme_request.take().and_then(Theme::named) {
            trill_noir::install(&ctx, &theme);
            self.theme = theme;
            ctx.request_repaint();
        }

        self.close_gesture(&ctx);
        self.was_typing = ctx.text_edit_focused();
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

    #[cfg(test)]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    #[cfg(test)]
    pub fn catalog_ready(&self) -> bool {
        self.catalog_ready
    }
}

impl eframe::App for TrillApp {
    /// Runs before every frame, and on its own while the window is hidden,
    /// so the session keeps being polled and tidied when minimized.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    /// A screenshot run sees no pointer, so the picture does not depend on
    /// where the mouse happened to be: nothing in it is hovered.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.options.screenshot.is_some() {
            raw_input.events.retain(|event| !is_pointer_event(event));
        }
    }
}

/// True for the events that say where the pointer is or what it does.
fn is_pointer_event(event: &egui::Event) -> bool {
    use egui::Event;
    matches!(
        event,
        Event::PointerMoved(_)
            | Event::MouseMoved(_)
            | Event::PointerButton { .. }
            | Event::PointerGone
            | Event::MouseWheel { .. }
            | Event::Touch { .. }
    )
}

/// The tracks whose devices the snapshot should describe: the selected
/// one, for the Devices tab, and every track with an automation lane open,
/// whose parameters the lanes draw. Ascending and without repeats, as
/// [`Snapshot::covers`] expects.
fn params_tracks(state: &UiState) -> Vec<TrackId> {
    let lanes = state.arrangement.lanes.iter().filter_map(|lane| match lane.owner {
        LaneOwner::Track(track) => Some(track),
        LaneOwner::Master => None,
    });
    let mut tracks: Vec<TrackId> = state.selected_track.into_iter().chain(lanes).collect();
    tracks.sort_by_key(|track| track.0);
    tracks.dedup();
    tracks
}

/// Carries out a command from the keyboard. Editing commands go to the pane
/// the user last worked in.
fn run_command(ctx: &egui::Context, cx: &mut Cx, command: Command) {
    let focus = cx.state.focus;
    let in_notes = focus == FocusArea::PianoRoll && cx.state.selected_clip.is_some();
    match command {
        Command::New => files::new_project(cx),
        Command::Open => files::open(ctx, cx),
        Command::Save => files::save(ctx, cx),
        Command::SaveAs => files::save_as(ctx, cx),
        Command::ExportWav => files::export_wav(ctx, cx),
        Command::Undo => drop(cx.with_session(|session| session.undo())),
        Command::Redo => drop(cx.with_session(|session| session.redo())),
        Command::Cut if in_notes => piano_roll::cut_notes(cx),
        Command::Cut => ops::cut(cx),
        Command::Copy if in_notes => piano_roll::copy_notes(cx),
        Command::Copy => ops::copy(cx),
        Command::Paste if in_notes => piano_roll::paste_notes(cx),
        Command::Paste => ops::paste(cx),
        Command::Duplicate if in_notes && !cx.state.selected_notes.is_empty() => piano_roll::duplicate_notes(cx),
        Command::Duplicate => ops::duplicate_selection(cx),
        // Delete acts on what the user is working on: notes, points or
        // clips.
        Command::Delete => match focus {
            FocusArea::PianoRoll if !cx.state.selected_notes.is_empty() => piano_roll::delete_selected_notes(cx),
            FocusArea::PianoRoll => {}
            FocusArea::Automation => arrangement::delete_selected_points(cx),
            FocusArea::Arrangement => ops::delete_selection(cx),
        },
        Command::SelectAll => match focus {
            FocusArea::PianoRoll => piano_roll::select_all_notes(cx),
            FocusArea::Automation => arrangement::select_all_points(cx),
            FocusArea::Arrangement => ops::select_all(cx),
        },
        Command::Escape => {
            if in_notes && !cx.state.selected_notes.is_empty() {
                cx.state.selected_notes.clear();
            } else {
                ops::escape(cx);
            }
        }
        Command::Nudge(steps, semitones) if in_notes => piano_roll::nudge_selected_notes(cx, steps, semitones),
        Command::Nudge(steps, _) => ops::nudge(cx, steps),
        Command::Quantize => piano_roll::quantize(cx),
        Command::Split => ops::split_at_cursor(cx),
        Command::MuteClips => ops::toggle_mute(cx),
        Command::RenameClip => ops::rename_selected(cx),
        Command::LoopSelection => ops::loop_selection(cx),
        Command::DeleteTime => ops::delete_time(cx),
        Command::DuplicateTime => ops::duplicate_time(cx),
        Command::AddMarker => ops::add_marker(cx),
        Command::AddTrack => arrangement::add_track(cx),
        Command::GroupTrack => arrangement::group_selected_track(cx),
        Command::ToggleAutomation => match cx.state.selected_track.filter(|_| !cx.state.master_selected) {
            Some(track) => automation::toggle_lanes(cx, LaneOwner::Track(track)),
            None => automation::toggle_lanes(cx, LaneOwner::Master),
        },
        Command::TogglePlay => top_bar::toggle_play(cx),
        Command::Rewind => cx.with_session(|session| session.seek(0.0)),
        Command::ToggleLoop => top_bar::toggle_loop(cx),
        Command::ToggleMetronome => top_bar::toggle_metronome(cx),
        Command::ToggleFollow => cx.state.arrangement.follow = !cx.state.arrangement.follow,
        Command::ZoomToSelection => arrangement::zoom_to_selection(ctx, cx),
        Command::ZoomToSong => arrangement::zoom_to_song(ctx, cx),
        Command::ToggleDraw => toggle_tool(cx, Tool::Draw),
        Command::ToggleBlade => toggle_tool(cx, Tool::Blade),
        Command::ToggleBrowser => cx.state.browser.open = !cx.state.browser.open,
        Command::ShowShortcuts => cx.state.shortcuts_open = !cx.state.shortcuts_open,
    }
}

/// Takes `tool` up, or puts it away if it is the one in hand.
fn toggle_tool(cx: &mut Cx, tool: Tool) {
    let current = &mut cx.state.arrangement.tool;
    *current = if *current == tool { Tool::Pointer } else { tool };
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
