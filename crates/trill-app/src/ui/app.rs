//! The application shell: keeps the snapshot in step with the session, lays
//! out the panels, handles keyboard shortcuts and decides when to repaint.

use super::arrangement::ops;
use super::assistant::{self, Assistant};
use super::audio_settings::{self, Settings};
use super::editors::{self, Editors};
use super::files::{self, Confirm, Jobs};
use super::filming::{self, Filming};
use super::keyboard::{self, Keyboard};
use super::making_of::{self, MakingOf};
use super::missing;
use super::recovery::{self, Recovery};
use super::setup::{self, Setup};
use super::shortcuts::{self, Command};
use super::snapshot::{Live, Snapshot};
use super::sounds::{Dropped, Report, Sounds};
use super::stage::StageView;
use super::state::{
    AudioSummary, BrowserState, BrowserTab, Cx, FocusArea, LaneOwner, StoryAsk, Tool, UiState, lane_owner,
};
use super::{
    UiOptions, arrangement, automation, browser, controller_lane, detail, dragging, piano_roll, recording, status_bar,
    top_bar,
};
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
use trill_runtime::Runtime;

/// How often an idle window checks the session for edits made elsewhere.
const IDLE_POLL: Duration = Duration::from_millis(100);
/// Height the arrangement keeps however tall the detail panel is made:
/// the ruler, two tracks and the master.
const MIN_ARRANGEMENT_HEIGHT: f32 = 200.0;
/// Frames to let the layout settle before taking a screenshot.
const SCREENSHOT_SETTLE_FRAMES: u32 = 12;
/// Frames the browser's tab, search and filters must stay as they are
/// before they are written down for the next session: not on every key of
/// a search.
const REMEMBER_AFTER_FRAMES: u32 = 45;
/// How long a screenshot waits for the library to be scanned and for the
/// waveforms on screen to be drawn before it is taken regardless.
const SCREENSHOT_PATIENCE: Duration = Duration::from_secs(20);

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
    // Declared first so that it is dropped first: its workers hold the
    // runtime, which should end here and not on one of their threads.
    sounds: Sounds,
    assistant: Assistant,
    session: SharedSession,
    snapshot: Snapshot,
    live: Live,
    state: UiState,
    editors: Editors,
    jobs: Jobs,
    recovery: Recovery,
    /// The computer keyboard, played as an instrument.
    keyboard: Keyboard,
    /// The Audio and MIDI panel.
    settings: Settings,
    /// The film panel, and the film being made if one is.
    filming: Filming,
    setup: Setup,
    /// The song filmed as it plays, shown in place of the panes.
    stage: StageView,
    /// Whether the window had the whole screen before the stage took it:
    /// `None` while the stage is not up, or did not take it.
    screen_before: Option<bool>,
    /// A film was asked for where its panel cannot be put up (over the
    /// making-of): it opens with the next frame of the song's own window.
    film_asked: Option<filming::Of>,
    /// What the window last asked of the screen for the stage: the whole of
    /// it, or to have its own size back.
    #[cfg(test)]
    asked_whole_screen: Option<bool>,
    /// The window's graphics device, which the stage draws on. `None`
    /// where the window has none to give (the tests).
    graphics: Option<eframe::egui_wgpu::RenderState>,
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
    /// The selection the session was last told of; `None` before the first
    /// time. Saves asking for the session when nothing moved.
    told_selection: Option<Option<TrackId>>,
    /// The project a take was last seen running in: one that ends because
    /// its project was replaced leaves nothing to select.
    take_generation: u64,
    /// The window title last sent, to avoid resending it every frame.
    title: String,
    /// Whether a text field had the keyboard when the last frame ended.
    was_typing: bool,
    /// The browser's width as the settings have it.
    saved_browser_width: f32,
    /// The frame count when the workers were last seen busy, for
    /// screenshots, which wait until what they show has arrived.
    busy_at_frame: u32,
    /// Where what the browser shows is kept between sessions; `None` in a
    /// run that keeps nothing.
    remember_in: Option<std::path::PathBuf>,
    /// What is written there, what the browser showed on the last frame,
    /// and the frame it last changed on.
    remembered: (browser::Remembered, browser::Remembered, u32),
    /// The making-of, while it is up: the session of the song played
    /// again, in place of the window's usual views.
    making_of: Option<MakingOf>,
    /// Who makes the session's devices: asked which of them are drums, for
    /// a song as it was.
    factory: Arc<dyn DeviceFactory>,
}

impl TrillApp {
    pub fn new(
        ctx: &egui::Context,
        runtime: Arc<Runtime>,
        options: UiOptions,
        failure: Arc<Mutex<Option<String>>>,
    ) -> Self {
        let session = runtime.session.clone();
        let theme = Theme::named(options.theme).unwrap_or_else(Theme::noir);
        trill_noir::install(ctx, &theme);
        // Edits are made from inside the UI pass. By default egui may run a
        // pass twice with the same input to hide a layout glitch, which
        // would apply a click's edit twice; one pass per frame rules it out.
        ctx.options_mut(|options| options.max_passes = NonZeroUsize::MIN);

        let mut state = UiState::new(options.detail_tab);
        state.browser.open = options.browser;
        // The settings are in memory already; nothing is read here.
        let saved_browser_width =
            runtime.settings().ui.browser_width.clamp(BrowserState::MIN_WIDTH, BrowserState::MAX_WIDTH);
        state.browser.width = saved_browser_width;
        // What the browser showed last time: a file of a few lines, read
        // once, before the window is up. What the command line asks for
        // comes after it.
        let remember_in = runtime.settings_folder();
        if let Some(remembered) = remember_in.as_deref().and_then(browser::Remembered::load) {
            remembered.apply(&mut state.browser);
        }
        let remembered = browser::Remembered::of(&state.browser);
        if let Some(tab) = options.browser_tab {
            state.browser.tab = tab;
        }
        if let Some(search) = &options.sound_search {
            state.browser.sounds.search = search.clone();
        }
        state.browser.sounds.pending_select = options.select_sound.clone();
        state.shortcuts_open = options.show_shortcuts;
        state.stage_open = options.stage;
        if let Some(kind) = options.lane {
            state.piano.lane = super::state::LaneShown::Controller(kind);
        }
        let (snapshot, factory, audio, live_input) = {
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
            // A keyboard plays the selected track from the first frame on.
            guard.set_selected_track(state.selected_track);
            if options.record
                && let Err(error) = guard.record()
            {
                log::warn!("--record: {error}");
            }
            let audio = AudioSummary::of(&guard);
            // Captured again now that the selection is known, so it covers
            // the devices of the tracks on show.
            (Snapshot::capture(&guard, &params_tracks(&state)), guard.factory(), audio, guard.live())
        };
        let (catalog_tx, catalog) = mpsc::channel();
        let repaint = ctx.clone();
        let devices = factory.clone();
        let making_of = match (&options.watch, options.making_of) {
            (Some(file), _) => Some(MakingOf::of_file(ctx, file.clone(), devices.clone())),
            (None, Some(start)) => Some(MakingOf::open(ctx, &session, devices.clone(), start)),
            (None, None) => None,
        };
        std::thread::spawn(move || {
            // The receiver is gone if the window closed first.
            if catalog_tx.send(factory.descriptors()).is_ok() {
                repaint.request_repaint();
            }
        });

        let assistant = Assistant::new(ctx, &runtime, &options);
        let now = Instant::now();
        let mut jobs = Jobs::default();
        // Unattended runs must be able to exit without a person answering
        // the "discard changes?" question, and must not take the keyboard
        // from whoever is at the machine.
        let unattended = options.screenshot.is_some() || options.exit_after.is_some();
        jobs.quit_confirmed = unattended;
        // What an earlier program did not live to save: found when the
        // runtime started, so this is a look at a list in memory.
        let recovery = Recovery::new(runtime.recoverable());
        let keyboard = Keyboard::new(remember_in.clone(), &options.keyboard, live_input);
        Self {
            sounds: Sounds::new(ctx, runtime),
            assistant,
            session,
            snapshot,
            live: Live::default(),
            state,
            editors: Editors::new(!unattended),
            jobs,
            recovery,
            keyboard,
            settings: Settings::default(),
            filming: Filming::default(),
            setup: Setup::default(),
            stage: StageView::new(),
            screen_before: None,
            film_asked: None,
            #[cfg(test)]
            asked_whole_screen: None,
            graphics: None,
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
            told_selection: None,
            take_generation: 0,
            title: String::new(),
            was_typing: false,
            saved_browser_width,
            busy_at_frame: 0,
            remember_in,
            remembered: (remembered.clone(), remembered, 0),
            making_of,
            factory: devices,
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
                // A save changes no revision, only where the project lives
                // and whether it has unsaved changes.
                let saved =
                    session.is_modified() != self.snapshot.modified || session.path() != self.snapshot.path.as_deref();
                if session.revision() != self.snapshot.revision || saved {
                    self.snapshot = Snapshot::capture(&session, &params_tracks(&self.state));
                    self.state.reconcile(&self.snapshot.project);
                    self.recheck_problem_files();
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
                // A picture shows no load: how busy the machine happened to be
                // while it was taken is not part of what the window looks like.
                let load = if self.options.screenshot.is_some() { 0.0 } else { session.audio_load() };
                self.live.set_cpu_load(load);
                self.live.audio_faults = session.audio_faults();
                self.live.preparing = session.preparing();
                self.live.update_editors(&self.snapshot, &session);
                self.live.update_silent(&self.snapshot, &session);
                // Which plugins are still reading their samples. (Not in a
                // picture: how long ago a device was made is not part of
                // what the window looks like.)
                self.live.loading = if self.options.screenshot.is_some() { Vec::new() } else { session.loading() };
                // The session's chores (a take written into the project, a
                // lost audio device replaced) are the runtime's
                // housekeeping thread's to do: looking for another device
                // can take seconds, and this thread paints.

                // The audio stream, when it has changed: the device, what
                // it runs at, and why that is not what was asked for.
                if session.audio_revision() != self.audio.revision {
                    self.audio = AudioSummary::of(&session);
                    // A new stream may have new meters behind it.
                    self.snapshot = Snapshot::capture(&session, &params_tracks(&self.state));
                }
                self.audio.hear_latency(session.output_latency_ms());

                // Where keyboards play: the session is kept told of the
                // selection, also when it moved without the window (the
                // selected track was deleted from elsewhere, say, and the
                // session forgot it with the track).
                if session.selected_track() != self.state.selected_track {
                    session.set_selected_track(self.state.selected_track);
                }
                self.told_selection = Some(self.state.selected_track);

                // Arming and recording, and what a take that just ended
                // left behind, wherever it was ended from.
                let was_recording = self.live.take.is_some();
                self.live.update_recording(&session);
                let same_project = self.take_generation == self.state.project_generation;
                match &self.live.take {
                    Some(take) if same_project => recording::take_writes(&mut self.state, &self.snapshot, &take.clips),
                    Some(_) => {}
                    None => self.state.take_shown = None,
                }
                if was_recording && self.live.take.is_none() && same_project {
                    recording::take_ended(&mut self.state, &self.snapshot, session.last_take());
                }
                self.take_generation = self.state.project_generation;
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
            editors: &mut self.editors,
            jobs: &mut self.jobs,
            recovery: &mut self.recovery,
            sounds: &mut self.sounds,
            assistant: &mut self.assistant,
            keyboard: &mut self.keyboard,
            settings: &mut self.settings,
            filming: &mut self.filming,
            setup: &mut self.setup,
        }
    }

    /// Tells the session which track is selected, if that moved since it
    /// was last told: a MIDI keyboard plays the selected track while none
    /// is armed, and must not be a frame behind a click.
    fn tell_selection(&mut self) {
        // Not while a stream is being opened: the session is busy with
        // that, and the next look at it (`sync`) catches up.
        if self.told_selection == Some(self.state.selected_track) || self.settings.is_applying() {
            return;
        }
        self.session.lock().set_selected_track(self.state.selected_track);
        self.told_selection = Some(self.state.selected_track);
    }

    /// Takes in what the sound workers finished: says what went wrong,
    /// selects what was dropped, and passes on what was chosen in a dialog.
    fn take_reports(&mut self) {
        for report in self.sounds.pump() {
            match report {
                Report::Info(text) => self.state.toasts.info(text),
                Report::Error(text) => self.state.toasts.error(text),
                Report::Success(text) => self.state.toasts.success(text),
                Report::Notice(text) => {
                    self.state.toasts.lasting(super::toast::ToastKind::Info, text, Duration::from_secs(12));
                }
                Report::LibraryMoved(from, to) => self.state.browser.sounds.folder_moved(&from, &to),
                Report::FolderAdded(folder) => {
                    let name = folder
                        .file_name()
                        .map_or_else(|| folder.display().to_string(), |name| name.to_string_lossy().into_owned());
                    self.state.toasts.success(format!("Added {name} to the library"));
                    self.state.browser.sounds.last_added = Some(folder);
                }
                Report::Picked(path, target) => {
                    let label = super::sounds::file_stem(&path);
                    self.sounds.drop_files(vec![path], target, label, 0.0);
                }
                Report::Dropped(Dropped::Clips(clips)) => {
                    // Shown by the next snapshot; selected now, so that
                    // the Clip tab opens on what was just dropped.
                    if let Some((track, _)) = clips.last() {
                        self.state.select_track(*track);
                    }
                    self.state.select_clips(clips.into_iter().map(|(_, clip)| clip).collect());
                    self.state.arrangement.time_selection = None;
                    self.state.focus = FocusArea::Arrangement;
                }
                Report::Dropped(Dropped::Sample { device, .. }) => self.state.selected_device = Some(device),
                Report::Dropped(Dropped::Pads { device, notes }) => {
                    self.state.selected_device = Some(device);
                    if let Some(note) = notes.last() {
                        self.state.selected_pads.insert(device, *note);
                    }
                }
            }
        }
    }

    /// Has the files the engine could not use looked at again. A waveform
    /// read while its file was still there would otherwise go on being
    /// shown for a clip that has fallen silent.
    fn recheck_problem_files(&self) {
        for clip in self.snapshot.clip_problems.keys() {
            if let Some(audio) = self.snapshot.clip(*clip).and_then(|(_, clip)| clip.audio()) {
                self.sounds.recheck(&audio.path);
            }
        }
        let samples = self.snapshot.samples.values().flatten();
        for file in samples.filter(|file| file.problem.is_some()) {
            self.sounds.recheck(&file.path);
        }
    }

    /// Writes down what the browser shows, once it has stopped changing,
    /// for the next session to start from. With `now`, without waiting.
    fn remember_browser(&mut self, now: bool) {
        let Some(folder) = self.remember_in.clone() else { return };
        let current = browser::Remembered::of(&self.state.browser);
        let (saved, seen, changed_at) = &mut self.remembered;
        if current != *seen {
            *seen = current.clone();
            *changed_at = self.frames;
        }
        if current == *saved || (!now && self.frames < *changed_at + REMEMBER_AFTER_FRAMES) {
            return;
        }
        *saved = current.clone();
        if now {
            if let Err(reason) = current.save(&folder) {
                log::warn!("{reason}");
            }
        } else {
            self.sounds.quietly(move |_| current.save(&folder));
        }
    }

    /// Remembers the browser's width once it has stopped being dragged.
    fn save_browser_width(&mut self, ctx: &egui::Context) {
        let width = self.state.browser.width.round();
        if width == self.saved_browser_width || ctx.input(|input| input.pointer.any_down()) {
            return;
        }
        self.saved_browser_width = width;
        self.sounds.quietly(move |runtime| {
            runtime.update_settings(|settings| settings.ui.browser_width = width).map_err(|error| error.to_string())
        });
    }

    /// The commands for the keys pressed this frame. A question on screen
    /// takes the keyboard; the shortcut reference lets only its own key
    /// through, which closes it.
    fn read_commands(&self, ctx: &egui::Context) -> Vec<Command> {
        if self.jobs.confirm.is_some() || self.recovery.is_open() || self.sounds.missing.open {
            return Vec::new();
        }
        let state = &self.state;
        let selection = match state.focus {
            FocusArea::Arrangement => !state.selected_clips.is_empty(),
            FocusArea::PianoRoll => !state.selected_notes.is_empty(),
            FocusArea::Automation => state.arrangement.points.as_ref().is_some_and(|chosen| !chosen.points.is_empty()),
            FocusArea::Controllers => !state.piano.lane_points.is_empty(),
            // The arrows belong to the list; see `browser::keys`.
            FocusArea::Browser => false,
        };
        // The key that ends typing (Escape, Enter) takes the focus away
        // before this runs, and must not then be read as a shortcut: the
        // field that just lost the keyboard still has to see it.
        let typing = ctx.text_edit_focused() || self.was_typing;
        let mut commands = shortcuts::read(ctx, typing, selection, self.keyboard.is_on());
        // While words are selected in a panel, Ctrl+C means those words.
        if trill_noir::prose::has_selection(ctx) {
            commands.retain(|command| !matches!(command, Command::Copy | Command::Cut));
        }
        if state.shortcuts_open {
            commands.retain(|command| matches!(command, Command::ShowShortcuts | Command::Escape));
        }
        if state.stage_open {
            // The stage is watched, not worked in: the transport's keys
            // and the ones that leave it are all it answers.
            commands.retain(|command| {
                matches!(
                    command,
                    Command::ToggleStage
                        | Command::Escape
                        | Command::TogglePlay
                        | Command::Rewind
                        | Command::ToggleLoop
                        | Command::ToggleMetronome
                )
            });
        }
        if self.filming.is_open() {
            // The film panel has the keyboard, but for the key that closes
            // it and for playing the song.
            commands.retain(|command| matches!(command, Command::ExportFilm | Command::Escape | Command::TogglePlay));
        }
        if self.setup.is_open() {
            // The Setup panel has the keyboard, but for the keys that close
            // it and for playing the song.
            commands.retain(|command| matches!(command, Command::Setup | Command::Escape | Command::TogglePlay));
        }
        if self.settings.is_open() {
            // The panel has the keyboard, but for the key that closes it
            // and for playing the song to hear what a setting does. While
            // a stream is being opened the session is busy: nothing that
            // needs it is run.
            let applying = self.settings.is_applying();
            commands.retain(|command| match command {
                Command::AudioSettings | Command::Escape => true,
                Command::TogglePlay => !applying,
                _ => false,
            });
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
            // What is still on its way (a scan, a waveform, a clip being
            // stretched) should be in the picture as it will be once it has
            // arrived: wait for it, then a few frames more.
            let arriving = self.sounds.is_waiting()
                || self.live.preparing > 0
                || self.settings.is_waiting()
                || self.setup.is_waiting();
            if arriving && elapsed < SCREENSHOT_PATIENCE {
                self.busy_at_frame = self.frames;
            }
            let settled = self.frames >= self.busy_at_frame + SCREENSHOT_SETTLE_FRAMES;
            match &self.capture {
                Capture::NotRequested if settled && elapsed >= settle => {
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
                    self.audio.info.output,
                    self.live.audio_faults,
                );
                // How the window kept up: frames since it opened, and what
                // the stage's part of them cost while it was shown.
                eprintln!(
                    "trill: {} frames, {:.1} a second",
                    self.frames,
                    self.frames as f32 / elapsed.as_secs_f32().max(1e-3)
                );
                if let Some(report) = self.stage.report() {
                    eprintln!("trill: {report}");
                }
                self.capture = Capture::Done;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(limit.saturating_sub(elapsed));
        }
    }

    /// The part of a frame that needs no window: catching up with the
    /// session and with background work.
    pub fn tick(&mut self, ctx: &egui::Context) {
        let replaced = self.jobs.poll(&mut self.state) | self.recovery.poll(&mut self.state);
        self.editors.poll(&mut self.state.toasts);
        self.settings.poll();
        self.setup.poll();
        self.filming.poll(ctx, &mut self.state);
        self.take_reports();
        self.sync(ctx);
        // The files the song names that are not on disk: looked for by a
        // worker whenever what could change the answer has changed.
        self.sounds.watch_missing(&self.snapshot);
        if replaced {
            // The window's state was reset for a project that a helper
            // thread opened. The snapshot may have caught up with that
            // project before the news of it arrived, in which case nothing
            // else would give the fresh state its selection.
            self.state.reconcile(&self.snapshot.project);
        }
        // Closed and idle, the assistant has nothing to do and is not asked.
        if self.assistant.wants_tick() {
            self.assistant.tick(ctx, &self.snapshot, &mut self.state, &self.live);
        }
        self.release_audition(ctx);
        if let Some(making_of) = &mut self.making_of {
            making_of.tick(ctx, &self.descriptors);
        }
        // Edits from other front ends arrive without any input event, so an
        // idle window has to look for them.
        ctx.request_repaint_after(IDLE_POLL);
    }

    /// Lays out and paints the making-of in place of the window's usual
    /// views: its own bars, the story down the right, the session as a
    /// line along the bottom, and between them the arrangement and the
    /// detail panel showing the song as it was. Those two are the window's
    /// own views, handed the replay's snapshot and view state and a session
    /// that is not the song's, and they are under glass: nothing done here
    /// reaches the song.
    fn frame_of_making(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Self {
            making_of: held,
            theme,
            descriptors,
            catalog_ready,
            audio,
            editors,
            jobs,
            recovery,
            sounds,
            assistant,
            keyboard,
            settings,
            filming,
            setup,
            film_asked,
            ..
        } = self;
        let Some(making_of) = held else { return };
        let mut leave = false;
        // The keys: the transport's, the arrows, and the way out. The
        // arrows are read as they are with something selected.
        for command in shortcuts::read(&ctx, false, true, false) {
            leave |= making_of.run(command);
        }

        let bare = egui::Frame::NONE;
        let (bar, status) = making_of::bar_heights();
        egui::Panel::top("making-of-bar")
            .exact_size(bar)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| leave |= making_of::bar(ui, theme, making_of));
        if making_of.take_film_ask() {
            // The panel that makes a film is the song's own window's.
            *film_asked = Some(filming::Of::Making);
            leave = true;
        }
        egui::Panel::bottom("making-of-status")
            .exact_size(status)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| making_of::status(ui, theme, making_of));
        let Some(watching) = making_of.watching_mut() else {
            egui::CentralPanel::no_frame().show(ui, |ui| making_of::waiting(ui, theme, making_of));
            if leave {
                *held = None;
            }
            return;
        };
        egui::Panel::bottom("making-of-line")
            .exact_size(making_of::STRIP_HEIGHT)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| making_of::strip(ui, theme, watching));
        let (width, limits) = making_of::story_width();
        let widest = (ui.available_width() - assistant::WINDOW_REST).clamp(*limits.start(), *limits.end());
        egui::Panel::right("making-of-story")
            .resizable(true)
            .default_size(width.min(widest))
            .size_range(*limits.start()..=widest)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| making_of::story(ui, theme, watching));

        // The song as it was, in the window's own views.
        let tallest = (ui.available_height() - MIN_ARRANGEMENT_HEIGHT).clamp(detail::MIN_HEIGHT, detail::MAX_HEIGHT);
        let mut song = |ui: &mut egui::Ui, watching: &mut making_of::Watching, name: &str, arrangement: bool| {
            let area = ui.max_rect();
            {
                let (session, snap, live, state) = watching.parts();
                let mut cx = Cx {
                    session,
                    snap,
                    live,
                    theme,
                    descriptors,
                    catalog_ready: *catalog_ready,
                    audio,
                    mcp_url: None,
                    state,
                    editors,
                    jobs,
                    recovery,
                    sounds,
                    assistant,
                    keyboard,
                    settings,
                    filming,
                    setup,
                };
                if arrangement {
                    arrangement::show(ui, &mut cx);
                } else {
                    detail::show(ui, &mut cx);
                }
            }
            making_of::glass(ui, watching, area, ui.scope_id().with(name));
        };
        egui::Panel::bottom("detail")
            .resizable(true)
            .default_size(detail::DEFAULT_HEIGHT.min(tallest))
            .size_range(detail::MIN_HEIGHT..=tallest)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| song(ui, watching, "making-of-detail-glass", false));
        egui::CentralPanel::no_frame().show(ui, |ui| song(ui, watching, "making-of-song-glass", true));

        // The status bar is laid out before what asks for hints, so a new
        // hint is shown by the frame after this one.
        if watching.hint().end_frame() {
            ctx.request_repaint();
        }
        if leave {
            *held = None;
            ctx.request_repaint();
        }
    }

    /// Lays out and paints the whole window into `ui`.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.frames += 1;
        self.guard_close(&ctx);
        self.update_title(&ctx);

        // A question on screen is about the song as it is: it is asked
        // there, not over the making-of.
        if self.jobs.confirm.is_some() || self.recovery.is_open() {
            self.making_of = None;
        }
        if self.making_of.is_some() {
            self.frame_of_making(ui);
            self.automate(&ctx);
            return;
        }

        // The sound browser's keys come first: while its list is being
        // worked in, Space and the letters are its own.
        let typing = ctx.text_edit_focused() || self.was_typing;
        // Before even those, the computer keyboard's: a note leaves for
        // the engine before anything else in the frame is looked at.
        keyboard::keys(&ctx, &mut self.cx(), typing);
        // Not on the stage, where the browser is not to be seen: Space is
        // the transport's there, whatever was being worked in before.
        if !self.state.stage_open {
            browser::keys(&ctx, &mut self.cx(), typing);
        }
        let commands = self.read_commands(&ctx);
        let open_editor = self.options.open_editor.take();
        let open_settings = std::mem::take(&mut self.options.show_settings);
        let mut film_asked = self.film_asked.take();
        if std::mem::take(&mut self.options.show_film) {
            film_asked.get_or_insert(filming::Of::Song);
        }
        let open_missing = std::mem::take(&mut self.options.show_missing);
        let open_setup = std::mem::take(&mut self.options.show_setup);
        if let Some(scene) = self.options.setup_scene.take() {
            self.setup.show(scene);
        }
        let mut cx = self.cx();
        if film_asked.is_some() {
            filming::open_on(&ctx, &mut cx, film_asked);
        }
        for command in commands {
            run_command(&ctx, &mut cx, command);
        }
        if open_settings {
            audio_settings::open(&mut cx);
        }
        if open_setup {
            setup::open(&ctx, &mut cx);
        }
        if open_missing {
            missing::open(&mut cx);
        }
        if let Some(name) = open_editor {
            editors::open_named(&mut cx, &name);
        }
        let on_stage = cx.state.stage_open;
        self.take_the_screen(&ctx, on_stage);
        if on_stage {
            // The stage has the whole window: no pane is laid out, and so
            // none is touched.
            self.show_stage(ui);
            return;
        }
        self.stage.closed(self.graphics.as_ref());
        let mut cx = self.cx();
        dragging::begin(&ctx, &mut cx);

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
                .resizable(true)
                .default_size(cx.state.browser.width)
                .size_range(BrowserState::MIN_WIDTH..=BrowserState::MAX_WIDTH)
                .show_separator_line(false)
                .frame(bare)
                .show(ui, |ui| browser::show(ui, &mut cx));
        }
        if cx.assistant.is_open() {
            // As wide as it is dragged, short of crowding out the rest.
            let widest =
                (ui.available_width() - assistant::WINDOW_REST).clamp(assistant::MIN_WIDTH, assistant::MAX_WIDTH);
            egui::Panel::right("assistant")
                .resizable(true)
                .default_size(cx.assistant.width().min(widest))
                .size_range(assistant::MIN_WIDTH..=widest)
                .show_separator_line(false)
                .frame(bare)
                .show(ui, |ui| assistant::show(ui, &mut cx));
        }
        // The detail panel may take most of the window, but never so much
        // that the arrangement above it disappears.
        let between_bars = ui.available_height();
        let keys_strip = keyboard::height(&cx);
        let tallest = (between_bars - MIN_ARRANGEMENT_HEIGHT - keys_strip.unwrap_or(0.0))
            .clamp(detail::MIN_HEIGHT, detail::MAX_HEIGHT);
        egui::Panel::bottom("detail")
            .resizable(true)
            .default_size(detail::DEFAULT_HEIGHT.min(tallest))
            .size_range(detail::MIN_HEIGHT..=tallest)
            .show_separator_line(false)
            .frame(bare)
            .show(ui, |ui| detail::show(ui, &mut cx));
        // While the computer keyboard is on, what it plays is shown in a
        // strip between the song and the detail panel.
        if let Some(height) = keys_strip {
            egui::Panel::bottom("computer-keyboard")
                .exact_size(height)
                .show_separator_line(false)
                .frame(bare)
                .show(ui, |ui| keyboard::show(ui, &mut cx));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| arrangement::show(ui, &mut cx));

        dragging::end(&ctx, &mut cx);
        files::confirm_modal(&ctx, &mut cx);
        recovery::modal(&ctx, &mut cx);
        missing::modal(&ctx, &mut cx);
        audio_settings::modal(&ctx, &mut cx);
        setup::modal(&ctx, &mut cx);
        filming::modal(&ctx, &mut cx);
        shortcuts::window(&ctx, &mut cx);
        // Clear of the assistant's panel: its Send and Stop are in that corner.
        let aside = if cx.assistant.is_open() { cx.assistant.width() } else { 0.0 };
        let corner = ctx.content_rect().right_bottom() - egui::vec2(12.0 + aside, status_bar::HEIGHT + 10.0);
        cx.state.toasts.show(&ctx, cx.theme, corner);
        // What the views changed (the selection, who has the keyboard)
        // stops the notes it must before the frame is over.
        keyboard::settle(&ctx, &mut cx);

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

        // A making-of was asked for: the session of the song, or a file's.
        match self.state.story.take() {
            Some(StoryAsk::Watch) => {
                let start = making_of::Start::default();
                self.making_of = Some(MakingOf::open(&ctx, &self.session, self.factory.clone(), start));
            }
            Some(StoryAsk::WatchFile(file)) => {
                self.making_of = Some(MakingOf::of_file(&ctx, file, self.factory.clone()));
            }
            None => {}
        }

        self.close_gesture(&ctx);
        self.tell_selection();
        self.save_browser_width(&ctx);
        self.remember_browser(false);
        self.was_typing = ctx.text_edit_focused();
        // Moving things need every frame; everything else waits for input
        // or the idle poll scheduled in `logic`.
        let previewing = self.sounds.preview.is_playing() && self.state.browser.tab == BrowserTab::Sounds;
        let moving = self.live.playing || self.live.take.is_some() || self.live.meters_moving();
        if moving || self.state.audition.is_some() || previewing {
            ctx.request_repaint();
        }
        self.automate(&ctx);
    }

    /// Names the system's window this runs in, for what must belong to it
    /// (plugin editors).
    pub fn set_native_window(&mut self, window: Option<isize>) {
        self.editors.set_owner(window);
    }

    /// Hands over the window's graphics device, for the stage to draw on.
    pub fn set_graphics(&mut self, graphics: Option<eframe::egui_wgpu::RenderState>) {
        self.graphics = graphics;
    }

    /// Gives the stage the whole screen as it opens, and the window its
    /// own size back as it leaves: F11 is the key people expect that of. A
    /// window that had the whole screen already keeps it. Not in a run
    /// that nobody watches (a picture, a timed run), which is of the window
    /// as it was asked for.
    fn take_the_screen(&mut self, ctx: &egui::Context, on_stage: bool) {
        let unattended = self.options.screenshot.is_some() || self.options.exit_after.is_some();
        match (on_stage, self.screen_before) {
            (true, None) if !unattended => {
                let whole = ctx.input(|input| input.viewport().fullscreen).unwrap_or(false);
                self.screen_before = Some(whole);
                if !whole {
                    self.ask_whole_screen(ctx, true);
                }
            }
            (false, Some(whole)) => {
                self.screen_before = None;
                if !whole {
                    self.ask_whole_screen(ctx, false);
                }
            }
            _ => {}
        }
    }

    fn ask_whole_screen(&mut self, ctx: &egui::Context, whole: bool) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(whole));
        #[cfg(test)]
        {
            self.asked_whole_screen = Some(whole);
        }
    }

    /// The frame while the stage is open: the stage, and what has to go
    /// on whatever the window shows.
    fn show_stage(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::no_frame().show(ui, |ui| {
            self.stage.show(ui, &self.snapshot, &self.live, self.audio.latency_ms, self.graphics.as_ref());
        });
        // A question that came up meanwhile (unsaved work on closing the
        // window) is asked over the stage.
        let mut cx = self.cx();
        files::confirm_modal(&ctx, &mut cx);
        keyboard::settle(&ctx, &mut cx);
        if let Some(theme) = self.state.theme_request.take().and_then(Theme::named) {
            trill_noir::install(&ctx, &theme);
            self.theme = theme;
        }
        self.tell_selection();
        self.was_typing = ctx.text_edit_focused();
        let moving = self.live.playing || self.live.take.is_some() || self.live.meters_moving();
        if moving {
            ctx.request_repaint();
        }
        self.automate(&ctx);
    }

    #[cfg(test)]
    pub fn filming(&self) -> &Filming {
        &self.filming
    }

    #[cfg(test)]
    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    /// Keeps studies, or the parts of a list, in `folder`: what choosing a
    /// folder in the system's dialog leads to.
    #[cfg(test)]
    pub fn keep_in(&mut self, ctx: &egui::Context, what: setup::Kept, folder: std::path::PathBuf) {
        setup::keep_in(ctx, &mut self.cx(), what, folder);
    }

    /// Makes the film the panel describes at `path`: what choosing a place
    /// in the system's dialog leads to.
    #[cfg(test)]
    pub fn film_to(&mut self, ctx: &egui::Context, path: std::path::PathBuf) {
        filming::export_to(ctx, &mut self.cx(), path);
    }

    #[cfg(test)]
    pub fn stage(&self) -> &StageView {
        &self.stage
    }

    /// What the stage last asked of the screen.
    #[cfg(test)]
    pub fn asked_whole_screen(&self) -> Option<bool> {
        self.asked_whole_screen
    }

    #[cfg(test)]
    pub fn making_of(&self) -> Option<&MakingOf> {
        self.making_of.as_ref()
    }

    #[cfg(test)]
    pub fn making_of_mut(&mut self) -> Option<&mut MakingOf> {
        self.making_of.as_mut()
    }

    #[cfg(test)]
    pub fn editors(&self) -> &Editors {
        &self.editors
    }

    #[cfg(test)]
    pub fn recovery(&self) -> &Recovery {
        &self.recovery
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

    #[cfg(test)]
    pub fn sounds(&self) -> &Sounds {
        &self.sounds
    }

    #[cfg(test)]
    pub fn sounds_mut(&mut self) -> &mut Sounds {
        &mut self.sounds
    }

    #[cfg(test)]
    pub fn live(&self) -> &Live {
        &self.live
    }

    #[cfg(test)]
    pub fn state_mut(&mut self) -> &mut UiState {
        &mut self.state
    }

    #[cfg(test)]
    pub fn assistant(&self) -> &Assistant {
        &self.assistant
    }

    #[cfg(test)]
    pub fn assistant_mut(&mut self) -> &mut Assistant {
        &mut self.assistant
    }

    #[cfg(test)]
    pub fn keyboard(&self) -> &Keyboard {
        &self.keyboard
    }

    #[cfg(test)]
    pub fn keyboard_mut(&mut self) -> &mut Keyboard {
        &mut self.keyboard
    }

    #[cfg(test)]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    #[cfg(test)]
    pub fn audio(&self) -> &AudioSummary {
        &self.audio
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

    /// What the browser shows is written down on the way out, if it
    /// changed since it last was; and no note typed on the computer
    /// keyboard is left without its end.
    fn on_exit(&mut self) {
        self.remember_browser(true);
        self.keyboard.close();
    }

    /// A screenshot run sees no pointer, so the picture does not depend on
    /// where the mouse happened to be: nothing in it is hovered.
    ///
    /// The computer keyboard hears here that the window is no longer the
    /// one typed into: this runs while the window is hidden, too, when no
    /// frame does.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.options.screenshot.is_some() {
            raw_input.events.retain(|event| !is_pointer_event(event));
        }
        self.keyboard.window(raw_input);
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
        Command::ExportFilm if cx.filming.is_open() => filming::close(cx),
        Command::ExportFilm => filming::open(ctx, cx),
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
            FocusArea::Controllers => controller_lane::delete_selected(cx),
            FocusArea::Arrangement => ops::delete_selection(cx),
            FocusArea::Browser => {}
        },
        Command::SelectAll => match focus {
            FocusArea::PianoRoll => piano_roll::select_all_notes(cx),
            FocusArea::Automation => arrangement::select_all_points(cx),
            FocusArea::Controllers => controller_lane::select_all(cx),
            FocusArea::Arrangement | FocusArea::Browser => ops::select_all(cx),
        },
        Command::Escape => {
            if cx.state.stage_open {
                cx.state.stage_open = false;
            } else if cx.state.shortcuts_open {
                cx.state.shortcuts_open = false;
            } else if cx.settings.is_open() {
                audio_settings::toggle(cx);
            } else if cx.setup.is_open() {
                setup::close(cx);
            } else if cx.filming.is_open() {
                filming::close(cx);
            } else if dragging::cancel(cx) {
                // The sound in the air was put down.
            } else if cx.sounds.preview.is_playing() || cx.sounds.preview.loading.is_some() {
                // A sound being auditioned is the first thing Escape ends.
                cx.sounds.stop_preview();
            } else if in_notes && !cx.state.selected_notes.is_empty() {
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
        Command::InsertTime => ops::insert_time(cx),
        Command::CopyTime => ops::copy_time(cx),
        Command::PasteTime => ops::paste_time(cx, trill_core::PasteMode::Insert),
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
        Command::ToggleAssistant => cx.assistant.toggle(),
        Command::ToggleKeyboard => cx.keyboard.toggle(),
        Command::ToggleStage => cx.state.stage_open = !cx.state.stage_open,
        Command::MakingOf => cx.state.story = Some(StoryAsk::Watch),
        Command::Record => recording::toggle(cx),
        Command::AudioSettings => audio_settings::toggle(cx),
        Command::Setup => setup::toggle(ctx, cx),
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
