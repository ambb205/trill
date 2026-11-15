//! Project files and exports: New, Open, Save, Save As, Export WAV.
//!
//! Native dialogs block until the user answers, and loading, saving and
//! rendering touch the disk, so all of that runs on helper threads. A thread
//! reports back through a channel that [`Jobs::poll`] drains each frame; the
//! UI thread never waits on any of it.

use super::state::{Cx, UiState};
use super::toast::ToastKind;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;
use trill_engine::{RenderOptions, RenderReport, SharedSession};
use trill_noir::{Variant, paint, text};

pub const PROJECT_EXTENSION: &str = "trill";

/// What a helper thread reports back.
enum Event {
    /// A dialog was dismissed without choosing anything.
    DialogCancelled,
    Loaded(Result<PathBuf, String>),
    Saved(Result<PathBuf, String>),
    /// The export dialog was answered and rendering has begun.
    ExportStarted(PathBuf),
    Exported(Result<RenderReport, String>),
}

/// An action that would throw away unsaved changes and waits for the user
/// to agree.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Confirm {
    New,
    Open,
    Quit,
}

/// Background file work and its state.
pub struct Jobs {
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    /// A native dialog is up; a second one must not be opened over it.
    dialog_open: bool,
    /// The file being rendered, while an export runs.
    exporting: Option<PathBuf>,
    pub confirm: Option<Confirm>,
    /// Set once the user chose to quit despite unsaved changes.
    pub quit_confirmed: bool,
}

impl Default for Jobs {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self { sender, receiver, dialog_open: false, exporting: None, confirm: None, quit_confirmed: false }
    }
}

impl Jobs {
    /// The file being exported right now, if any.
    pub fn exporting(&self) -> Option<&Path> {
        self.exporting.as_deref()
    }

    /// Applies what helper threads have reported since the last frame.
    pub fn poll(&mut self, state: &mut UiState) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::DialogCancelled => self.dialog_open = false,
                Event::Loaded(result) => {
                    self.dialog_open = false;
                    match result {
                        Ok(path) => {
                            state.reset_for_new_project();
                            state.toasts.info(format!("Opened {}", file_name(&path)));
                        }
                        Err(error) => state.toasts.error(format!("Could not open the project: {error}")),
                    }
                }
                Event::Saved(result) => {
                    self.dialog_open = false;
                    match result {
                        Ok(path) => state.toasts.success(format!("Saved {}", file_name(&path))),
                        Err(error) => state.toasts.error(format!("Could not save the project: {error}")),
                    }
                }
                Event::ExportStarted(path) => {
                    self.dialog_open = false;
                    self.exporting = Some(path);
                }
                Event::Exported(result) => {
                    self.exporting = None;
                    match result {
                        Ok(report) => {
                            let kind =
                                if report.stats.clipped_samples > 0 { ToastKind::Info } else { ToastKind::Success };
                            state.toasts.lasting(kind, export_summary(&report), Duration::from_secs(14));
                        }
                        Err(error) => state.toasts.error(format!("Export failed: {error}")),
                    }
                }
            }
        }
    }

    /// Runs `work` on a helper thread. `work` reports through the callback
    /// it is given, which also wakes the UI.
    fn spawn(&self, ctx: &egui::Context, work: impl FnOnce(&dyn Fn(Event)) + Send + 'static) {
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            work(&|event| {
                // The receiver only goes away when the window has closed.
                let _ = sender.send(event);
                ctx.request_repaint();
            });
        });
    }
}

/// Starts an empty project, asking first if there are unsaved changes.
pub fn new_project(cx: &mut Cx) {
    if cx.snap.modified {
        cx.jobs.confirm = Some(Confirm::New);
    } else {
        discard_and_start_new(cx);
    }
}

fn discard_and_start_new(cx: &mut Cx) {
    cx.with_session(|session| session.new_project());
    cx.state.reset_for_new_project();
}

/// Opens a project file, asking first if there are unsaved changes.
pub fn open(ctx: &egui::Context, cx: &mut Cx) {
    if cx.snap.modified {
        cx.jobs.confirm = Some(Confirm::Open);
    } else {
        open_dialog(ctx, cx);
    }
}

fn open_dialog(ctx: &egui::Context, cx: &mut Cx) {
    if std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let session = cx.session.clone();
    let dialog = project_dialog("Open project", cx.snap.path.as_deref());
    cx.jobs.spawn(ctx, move |report| {
        report(match pollster::block_on(dialog.pick_file()) {
            Some(file) => Event::Loaded(load(&session, file.path())),
            None => Event::DialogCancelled,
        });
    });
}

fn load(session: &SharedSession, path: &Path) -> Result<PathBuf, String> {
    session.lock().load(path).map(|()| path.to_path_buf()).map_err(|error| error.to_string())
}

/// Saves to the project's file, or asks for one if it has never been saved.
pub fn save(ctx: &egui::Context, cx: &mut Cx) {
    match cx.snap.path.clone() {
        Some(path) => {
            let session = cx.session.clone();
            cx.jobs.spawn(ctx, move |report| report(Event::Saved(write(&session, &path))));
        }
        None => save_as(ctx, cx),
    }
}

pub fn save_as(ctx: &egui::Context, cx: &mut Cx) {
    if std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let session = cx.session.clone();
    let dialog = project_dialog("Save project as", cx.snap.path.as_deref())
        .set_file_name(format!("{}.{PROJECT_EXTENSION}", file_stem_for(&cx.snap.project.name)));
    cx.jobs.spawn(ctx, move |report| {
        report(match pollster::block_on(dialog.save_file()) {
            Some(file) => Event::Saved(write(&session, &with_extension(file.path(), PROJECT_EXTENSION))),
            None => Event::DialogCancelled,
        });
    });
}

fn write(session: &SharedSession, path: &Path) -> Result<PathBuf, String> {
    session.lock().save(path).map(|()| path.to_path_buf()).map_err(|error| error.to_string())
}

/// Asks where to put a WAV file, then renders the song into it.
pub fn export_wav(ctx: &egui::Context, cx: &mut Cx) {
    if cx.jobs.exporting.is_some() || std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let session = cx.session.clone();
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("Export audio")
        .add_filter("WAV audio", &["wav"])
        .set_file_name(format!("{}.wav", file_stem_for(&cx.snap.project.name)));
    if let Some(folder) = cx.snap.path.as_deref().and_then(Path::parent) {
        dialog = dialog.set_directory(folder);
    }
    cx.jobs.spawn(ctx, move |report| {
        let Some(file) = pollster::block_on(dialog.save_file()) else {
            report(Event::DialogCancelled);
            return;
        };
        let path = with_extension(file.path(), "wav");
        report(Event::ExportStarted(path.clone()));
        report(Event::Exported(render(&session, &path)));
    });
}

/// Renders the song as it is right now into a WAV file at the engine's
/// sample rate.
fn render(session: &SharedSession, path: &Path) -> Result<RenderReport, String> {
    // The job is a copy of the project, so the session is free again before
    // the long part starts and editing can continue meanwhile.
    let (job, sample_rate) = {
        let mut session = session.lock();
        let sample_rate = session.audio_info().sample_rate.round() as u32;
        (session.render_job(), sample_rate)
    };
    let options = RenderOptions { sample_rate, ..RenderOptions::default() };
    job.render_to_file(path, &options).map_err(|error| error.to_string())
}

fn project_dialog(title: &str, current: Option<&Path>) -> rfd::AsyncFileDialog {
    let dialog = rfd::AsyncFileDialog::new().set_title(title).add_filter("Trill project", &[PROJECT_EXTENSION]);
    match current.and_then(Path::parent) {
        Some(folder) => dialog.set_directory(folder),
        None => dialog,
    }
}

/// Shows the "discard unsaved changes?" question while one is pending, and
/// carries out the answer.
pub fn confirm_modal(ctx: &egui::Context, cx: &mut Cx) {
    let Some(confirm) = cx.jobs.confirm else { return };
    let (verb, proceed) = match confirm {
        Confirm::New => ("starting a new project", "Discard and start new"),
        Confirm::Open => ("opening another project", "Discard and open"),
        Confirm::Quit => ("quitting", "Discard and quit"),
    };

    let theme = cx.theme;
    let mut choice = None;
    // A flat panel with a hairline: a caps title over a rule, the question,
    // and the buttons at the right.
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(14, 12));
    let modal = egui::Modal::new(egui::Id::unique("confirm-discard")).frame(frame).backdrop_color(theme.backdrop).show(
        ctx,
        |ui| {
            ui.set_width(400.0);
            ui.spacing_mut().item_spacing = egui::vec2(theme.metric.gap, 0.0);
            let (title, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
            let caps = theme.text.caps;
            text::paint(
                ui.painter(),
                title,
                egui::Align2::LEFT_CENTER,
                caps,
                "Discard unsaved changes?",
                theme.ink_dim,
            );
            ui.add_space(6.0);
            let (rule, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(format!(
                    "\"{}\" has changes that are not saved. They will be lost by {verb}.",
                    cx.snap.project.name
                ))
                .color(theme.ink),
            );
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if trill_noir::button(ui, theme, proceed, Variant::Danger).clicked() {
                    choice = Some(true);
                }
                if trill_noir::button(ui, theme, "Cancel", Variant::Default).clicked() {
                    choice = Some(false);
                }
            });
        },
    );
    if modal.should_close() {
        choice.get_or_insert(false);
    }

    let Some(proceed) = choice else { return };
    cx.jobs.confirm = None;
    if !proceed {
        return;
    }
    match confirm {
        Confirm::New => discard_and_start_new(cx),
        Confirm::Open => open_dialog(ctx, cx),
        Confirm::Quit => {
            cx.jobs.quit_confirmed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// `path` with `extension` appended unless it already ends in it. Save
/// dialogs do not reliably add the extension themselves.
fn with_extension(path: &Path, extension: &str) -> PathBuf {
    let has_it = path.extension().is_some_and(|existing| existing.eq_ignore_ascii_case(extension));
    if has_it {
        return path.to_path_buf();
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

/// A file name suggested for a project called `name`: characters that file
/// systems reject are replaced.
fn file_stem_for(name: &str) -> String {
    let cleaned: String =
        name.trim().chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '-' } else { c }).collect();
    if cleaned.is_empty() { "Untitled".to_owned() } else { cleaned }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// One line describing a finished export: where it went and how it measures.
fn export_summary(report: &RenderReport) -> String {
    let stats = &report.stats;
    let seconds = stats.duration_seconds.round() as u64;
    let mut summary = format!(
        "Exported {} ({}:{:02}, {} kHz). Peak {:.1} dBFS, loudness {:.1} LUFS.",
        file_name(Path::new(&report.path)),
        seconds / 60,
        seconds % 60,
        report.sample_rate as f32 / 1000.0,
        stats.peak_db,
        stats.loudness_lufs,
    );
    if stats.clipped_samples > 0 {
        summary
            .push_str(&format!(" {} samples clip: lower the master volume and export again.", stats.clipped_samples));
    }
    if !report.problems.is_empty() {
        summary.push_str(&format!(" Rendered with {} unresolved problem(s).", report.problems.len()));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use trill_core::device::CompositeFactory;
    use trill_dsp::BuiltinFactory;
    use trill_engine::analysis::Spectrum;
    use trill_engine::{AudioBackend, AudioStats, Session};

    #[test]
    fn extensions_are_added_only_when_missing() {
        assert_eq!(with_extension(Path::new("song"), "trill"), PathBuf::from("song.trill"));
        assert_eq!(with_extension(Path::new("song.trill"), "trill"), PathBuf::from("song.trill"));
        assert_eq!(with_extension(Path::new("song.TRILL"), "trill"), PathBuf::from("song.TRILL"));
        // Dots in the name are part of the name, not an extension to replace.
        assert_eq!(with_extension(Path::new("mix v1.2"), "wav"), PathBuf::from("mix v1.2.wav"));
        assert_eq!(with_extension(Path::new("dir.d/song"), "wav"), PathBuf::from("dir.d/song.wav"));
    }

    #[test]
    fn suggested_file_names_are_legal() {
        assert_eq!(file_stem_for("Night Drive"), "Night Drive");
        assert_eq!(file_stem_for("a/b: c?"), "a-b- c-");
        assert_eq!(file_stem_for("   "), "Untitled");
    }

    fn report(clipped_samples: usize, problems: Vec<String>) -> RenderReport {
        RenderReport {
            path: "C:/music/night drive.wav".into(),
            sample_rate: 48_000,
            stats: AudioStats {
                duration_seconds: 64.4,
                peak_db: -1.04,
                rms_db: -16.0,
                loudness_lufs: -13.96,
                clipped_samples,
                stereo_correlation: 0.4,
                spectrum: Spectrum { sub_db: -20.0, bass_db: -6.0, mid_db: -3.0, presence_db: -12.0, air_db: -20.0 },
            },
            tracks: Vec::new(),
            problems,
        }
    }

    #[test]
    fn export_summary_names_the_file_and_its_levels() {
        assert_eq!(
            export_summary(&report(0, Vec::new())),
            "Exported night drive.wav (1:04, 48 kHz). Peak -1.0 dBFS, loudness -14.0 LUFS."
        );
        let troubled = export_summary(&report(12, vec!["device 4: missing".into()]));
        assert!(troubled.contains("12 samples clip"), "{troubled}");
        assert!(troubled.contains("1 unresolved problem"), "{troubled}");
    }

    fn shared_session() -> SharedSession {
        let factory = Arc::new(CompositeFactory::new(vec![Box::new(BuiltinFactory)]));
        Arc::new(Mutex::new(Session::new(factory, AudioBackend::Null { sample_rate: 48_000 }).unwrap()))
    }

    /// A scratch folder for one test, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let folder = std::env::temp_dir().join(format!("trill-app-test-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&folder).unwrap();
            Self(folder)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_saved_project_loads_back_identically() {
        let scratch = Scratch::new("save");
        let session = shared_session();
        crate::demo::load(&mut session.lock()).unwrap();
        let saved = session.lock().project().clone();
        assert!(session.lock().is_modified());

        let path = scratch.0.join("night drive.trill");
        assert_eq!(write(&session, &path), Ok(path.clone()));
        assert!(!session.lock().is_modified(), "saving clears the modified mark");

        session.lock().new_project();
        assert!(session.lock().project().tracks.is_empty());
        assert_eq!(load(&session, &path), Ok(path.clone()));
        assert_eq!(*session.lock().project(), saved);
        assert_eq!(session.lock().path(), Some(path.as_path()));

        let missing = load(&session, &scratch.0.join("nope.trill")).unwrap_err();
        assert!(missing.contains("nope.trill"), "{missing}");
        assert_eq!(*session.lock().project(), saved, "a failed open leaves the project alone");
    }

    #[test]
    fn exporting_writes_a_wav_and_measures_it() {
        let scratch = Scratch::new("export");
        let session = shared_session();
        crate::demo::load(&mut session.lock()).unwrap();

        let path = scratch.0.join("night drive.wav");
        let report = render(&session, &path).unwrap();
        assert_eq!(report.sample_rate, 48_000);
        // Sixteen bars at 112 BPM are 34.3 s; the tail rings a little longer.
        assert!((34.0..46.0).contains(&report.stats.duration_seconds), "{} s", report.stats.duration_seconds);
        assert!(report.stats.peak_db > -30.0 && report.stats.peak_db <= 0.0, "peak {} dBFS", report.stats.peak_db);
        assert_eq!(report.stats.clipped_samples, 0, "the master limiter keeps the demo from clipping");
        assert!(report.problems.is_empty());
        let bytes = std::fs::metadata(&path).unwrap().len();
        assert!(bytes > 1_000_000, "a real file was written: {bytes} bytes");
        assert!(export_summary(&report).starts_with("Exported night drive.wav (0:"));

        // An empty project has nothing to render, and says so.
        session.lock().new_project();
        let error = render(&session, &scratch.0.join("empty.wav")).unwrap_err();
        assert!(error.contains("nothing to render"), "{error}");
    }

    #[test]
    fn results_from_helper_threads_reach_the_state() {
        let mut jobs = Jobs::default();
        let mut state = UiState::new(Default::default());
        jobs.dialog_open = true;
        jobs.sender.send(Event::ExportStarted(PathBuf::from("out.wav"))).unwrap();
        jobs.poll(&mut state);
        assert_eq!(jobs.exporting(), Some(Path::new("out.wav")));
        assert!(!jobs.dialog_open);

        jobs.sender.send(Event::Exported(Err("disk full".into()))).unwrap();
        jobs.sender.send(Event::Saved(Ok(PathBuf::from("a/song.trill")))).unwrap();
        jobs.poll(&mut state);
        assert_eq!(jobs.exporting(), None);
        assert_eq!(state.toasts.len(), 2);
    }
}
