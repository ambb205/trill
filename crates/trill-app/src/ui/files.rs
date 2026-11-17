//! Project files, imports and exports: New, Open, Save, Save As, Import
//! and Export MIDI, Export WAV.
//!
//! Native dialogs block until the user answers, and loading, saving and
//! rendering touch the disk, so all of that runs on helper threads. A thread
//! reports back through a channel that [`Jobs::poll`] drains each frame; the
//! UI thread never waits on any of it.

use super::state::{Cx, StoryAsk, UiState};
use super::toast::ToastKind;
use super::{colors, widgets};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;
use trill_core::midi_file::{self, MidiFile};
use trill_core::project::ImportedPart;
use trill_core::{Beats, ChainRef, DeviceId};
use trill_engine::render::Stems;
use trill_engine::{RenderOptions, RenderReport, Session, SessionError, SharedSession};

pub const PROJECT_EXTENSION: &str = "trill";
const MIDI_EXTENSIONS: [&str; 2] = ["mid", "midi"];
/// What imported parts play through, so they are heard at once.
const DRUM_INSTRUMENT: &str = "builtin:drumkit";
const MELODIC_INSTRUMENT: &str = "builtin:polysynth";

/// What a helper thread reports back.
enum Event {
    /// A dialog was dismissed without choosing anything.
    DialogCancelled,
    Loaded(Result<PathBuf, String>),
    Saved(Result<PathBuf, String>),
    /// The export dialog was answered and rendering has begun.
    ExportStarted(PathBuf),
    Exported(Result<Box<RenderReport>, String>),
    /// The name of the MIDI file read and how many tracks it became.
    MidiImported(Result<(String, usize), String>),
    MidiExported(Result<PathBuf, String>),
    /// The story of the song was written out, with or without the
    /// conversation.
    StoryExported(Result<(PathBuf, bool), String>),
    /// A story written out to a file was chosen, to watch.
    StoryPicked(PathBuf),
    /// A device was given the preset in a file.
    PresetLoaded(Result<PresetLoaded, String>),
}

/// What giving a device a preset file came to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PresetLoaded {
    preset: String,
    device: String,
    /// What the device goes on doing with it, where that is worth saying.
    note: Option<String>,
}

/// An action that would throw away unsaved changes and waits for the user
/// to agree.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Confirm {
    New,
    Open,
    Quit,
    /// Not about unsaved changes: the journal of the song is to be
    /// deleted, which cannot be taken back.
    DeleteJournal,
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
    /// Returns true if the open project was replaced by one from a file.
    pub fn poll(&mut self, state: &mut UiState) -> bool {
        let mut replaced = false;
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::DialogCancelled => self.dialog_open = false,
                Event::Loaded(result) => {
                    self.dialog_open = false;
                    match result {
                        Ok(path) => {
                            replaced = true;
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
                Event::MidiImported(result) => {
                    self.dialog_open = false;
                    match result {
                        Ok((name, 0)) => state.toasts.info(format!("{name} has no notes to import")),
                        Ok((name, 1)) => state.toasts.success(format!("Imported 1 track from {name}")),
                        Ok((name, count)) => state.toasts.success(format!("Imported {count} tracks from {name}")),
                        Err(error) => state.toasts.error(format!("Could not import the MIDI file: {error}")),
                    }
                }
                Event::MidiExported(result) => {
                    self.dialog_open = false;
                    match result {
                        Ok(path) => state.toasts.success(format!("Exported {}", file_name(&path))),
                        Err(error) => state.toasts.error(format!("Could not export MIDI: {error}")),
                    }
                }
                Event::PresetLoaded(result) => {
                    self.dialog_open = false;
                    match result {
                        Ok(PresetLoaded { preset, device, note }) => {
                            let loaded = format!("{device} has the preset \u{201C}{preset}\u{201D}");
                            match note {
                                // What a sampler reads for a while, or a
                                // setting the preset switched on.
                                Some(note) => {
                                    let lasting = Duration::from_secs(14);
                                    state.toasts.lasting(ToastKind::Info, format!("{loaded}. {note}"), lasting);
                                }
                                None => state.toasts.success(loaded),
                            }
                        }
                        // The session's own sentence: which file, and why not.
                        Err(error) => state.toasts.error(error),
                    }
                }
                Event::StoryExported(result) => {
                    self.dialog_open = false;
                    match result {
                        Ok((path, true)) => state.toasts.success(format!(
                            "Wrote {}: how the song was made, with the conversation",
                            file_name(&path)
                        )),
                        Ok((path, false)) => state.toasts.success(format!(
                            "Wrote {}: how the song was made, without the conversation",
                            file_name(&path)
                        )),
                        Err(error) => state.toasts.error(format!("Could not write the making-of: {error}")),
                    }
                }
                Event::StoryPicked(path) => {
                    self.dialog_open = false;
                    state.story = Some(StoryAsk::WatchFile(path));
                }
            }
        }
        replaced
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
    export(ctx, cx, false);
}

/// [`export_wav`], with every track, group and return written to a file
/// of its own as well, in a folder beside the song's.
pub fn export_stems(ctx: &egui::Context, cx: &mut Cx) {
    export(ctx, cx, true);
}

fn export(ctx: &egui::Context, cx: &mut Cx, stems: bool) {
    if cx.jobs.exporting.is_some() || std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let session = cx.session.clone();
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title(if stems { "Export audio and stems" } else { "Export audio" })
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
        report(Event::Exported(render(&session, &path, stems).map(Box::new)));
    });
}

/// The folder the stems of the song at `path` go in: beside it, named
/// after it.
fn stems_folder(path: &Path) -> PathBuf {
    let name = path.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{name} stems"))
}

/// Renders the song as it is right now into a WAV file at the engine's
/// sample rate. With `stems`, the same pass writes every track, group and
/// return to a file of its own in a folder beside it.
fn render(session: &SharedSession, path: &Path, stems: bool) -> Result<RenderReport, String> {
    // The job is a copy of the project, so the session is free again before
    // the long part starts and editing can continue meanwhile.
    let (job, sample_rate) = {
        let mut session = session.lock();
        let sample_rate = session.audio_info().sample_rate.round() as u32;
        (session.render_job(), sample_rate)
    };
    let stems = stems.then(|| Stems {
        folder: stems_folder(path),
        tracks: true,
        groups: true,
        returns: true,
        ..Stems::default()
    });
    let options = RenderOptions { sample_rate, stems, ..RenderOptions::default() };
    job.render_to_file(path, &options).map_err(|error| error.to_string())
}

/// Asks for a MIDI file and adds its parts as new tracks at the edit
/// cursor.
pub fn import_midi(ctx: &egui::Context, cx: &mut Cx) {
    if std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let session = cx.session.clone();
    let at = cx.state.arrangement.cursor;
    let mut dialog = rfd::AsyncFileDialog::new().set_title("Import MIDI").add_filter("MIDI file", &MIDI_EXTENSIONS);
    if let Some(folder) = cx.snap.path.as_deref().and_then(Path::parent) {
        dialog = dialog.set_directory(folder);
    }
    cx.jobs.spawn(ctx, move |report| {
        report(match pollster::block_on(dialog.pick_file()) {
            Some(file) => Event::MidiImported(read_midi(&session, file.path(), at)),
            None => Event::DialogCancelled,
        });
    });
}

/// Asks for a preset file and gives it to `device`: for a preset that is
/// not in the device's menu because of where it lies, such as one that
/// was bought and is kept with the samples.
pub fn load_preset_file(ctx: &egui::Context, cx: &mut Cx, device: DeviceId) {
    if std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let session = cx.session.clone();
    // Any file: what a preset's file is called is its maker's choice.
    let mut dialog = rfd::AsyncFileDialog::new().set_title("Load preset file");
    if let Some(folder) = cx.snap.path.as_deref().and_then(Path::parent) {
        dialog = dialog.set_directory(folder);
    }
    cx.jobs.spawn(ctx, move |report| {
        report(match pollster::block_on(dialog.pick_file()) {
            Some(file) => Event::PresetLoaded(give_preset_file(&session, device, file.path())),
            None => Event::DialogCancelled,
        });
    });
}

/// Gives `device` the preset in the file at `path`, as one undo step. An
/// error is the session's sentence about the file, and the device is as
/// it was.
fn give_preset_file(session: &SharedSession, device: DeviceId, path: &Path) -> Result<PresetLoaded, String> {
    let mut session = session.lock();
    let name = session.project().device(device).map_err(|error| error.to_string())?.name.clone();
    let preset = session.load_preset(device, &path.to_string_lossy()).map_err(|error| error.to_string())?;
    Ok(PresetLoaded { preset: preset.name, device: name, note: session.take_preset_note(device) })
}

/// Reads the MIDI file at `path` and imports it at beat `at`. Returns the
/// file's name and how many tracks it became.
fn read_midi(session: &SharedSession, path: &Path, at: Beats) -> Result<(String, usize), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let file = midi_file::parse(&bytes).map_err(|error| format!("{}: {error}", file_name(path)))?;
    let parts = import_midi_file(&mut session.lock(), &file, at).map_err(|error| error.to_string())?;
    Ok((file_name(path), parts.len()))
}

/// Adds the parts of a MIDI file as new tracks at beat `at`, each with an
/// instrument so it sounds right away (a drum kit for drum parts, a synth
/// for the rest) and the next hue of the palette, all as one undo step.
pub fn import_midi_file(session: &mut Session, file: &MidiFile, at: Beats) -> Result<Vec<ImportedPart>, SessionError> {
    session.begin_gesture();
    let first = session.project().tracks.len();
    let imported = session.edit(|project| {
        let parts = project.import_midi(file, at)?;
        for (offset, part) in parts.iter().enumerate() {
            project.track_mut(part.track)?.color = colors::new_track_color(first + offset);
        }
        Ok(parts)
    });
    let result = imported.and_then(|parts| {
        for part in &parts {
            let instrument = if part.drums { DRUM_INSTRUMENT } else { MELODIC_INSTRUMENT };
            session.add_device(ChainRef::Track(part.track), instrument, None, None)?;
        }
        Ok(parts)
    });
    session.end_gesture();
    result
}

/// Asks where to put a MIDI file and writes the song's notes into it.
pub fn export_midi(ctx: &egui::Context, cx: &mut Cx) {
    if std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let session = cx.session.clone();
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title("Export MIDI")
        .add_filter("MIDI file", &MIDI_EXTENSIONS)
        .set_file_name(format!("{}.mid", file_stem_for(&cx.snap.project.name)));
    if let Some(folder) = cx.snap.path.as_deref().and_then(Path::parent) {
        dialog = dialog.set_directory(folder);
    }
    cx.jobs.spawn(ctx, move |report| {
        report(match pollster::block_on(dialog.save_file()) {
            Some(file) => Event::MidiExported(write_midi(&session, &with_midi_extension(file.path()))),
            None => Event::DialogCancelled,
        });
    });
}

/// Writes every MIDI note of the song, where it sounds, to `path`.
fn write_midi(session: &SharedSession, path: &Path) -> Result<PathBuf, String> {
    let file = session.lock().project().export_midi();
    if file.tracks.is_empty() {
        return Err("the song has no MIDI notes to export".to_owned());
    }
    std::fs::write(path, midi_file::write(&file)).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path.to_path_buf())
}

/// `path` ending in `.mid`, unless it already names a MIDI file.
fn with_midi_extension(path: &Path) -> PathBuf {
    let named = |extension: &&str| path.extension().is_some_and(|existing| existing.eq_ignore_ascii_case(extension));
    if MIDI_EXTENSIONS.iter().any(named) { path.to_path_buf() } else { with_extension(path, "mid") }
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
    if confirm == Confirm::DeleteJournal {
        let body = format!(
            "How \"{}\" was made, step by step, with what was said to the assistant on the way, will be gone for \
             good. The song itself is not touched.",
            cx.snap.project.name
        );
        let title = "Delete this song\u{2019}s journal?";
        let Some(proceed) = widgets::question(ctx, cx.theme, "confirm-journal", title, &body, "Delete", "Cancel")
        else {
            return;
        };
        cx.jobs.confirm = None;
        if proceed {
            match cx.with_session(|session| session.delete_journal()) {
                Ok(()) => cx.state.toasts.info("The journal is deleted. The story begins again from here."),
                Err(error) => cx.state.toasts.error(format!("Could not delete the journal: {error}")),
            }
        }
        return;
    }
    let (verb, proceed) = match confirm {
        Confirm::New => ("starting a new project", "Discard and start new"),
        Confirm::Open => ("opening another project", "Discard and open"),
        Confirm::Quit => ("quitting", "Discard and quit"),
        Confirm::DeleteJournal => return,
    };

    let body = format!("\"{}\" has changes that are not saved. They will be lost by {verb}.", cx.snap.project.name);
    let title = "Discard unsaved changes?";
    let choice = widgets::question(ctx, cx.theme, "confirm-discard", title, &body, proceed, "Cancel");
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
        Confirm::DeleteJournal => {}
    }
}

/// Asks where to put the story of the song and writes it there: the song
/// as it was when its journal began and every step since, to hand on and
/// watch elsewhere. Without `conversation`, nothing that was said between
/// the user and the assistant is in it.
pub fn export_story(ctx: &egui::Context, cx: &mut Cx, conversation: bool) {
    if std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let session = cx.session.clone();
    let ending = trill_engine::journal::EXPORT_ENDING;
    let title = if conversation { "Export the making-of, with the conversation" } else { "Export the making-of" };
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title(title)
        .add_filter("Trill making-of", &[ending])
        .set_file_name(format!("{}.{ending}", file_stem_for(&cx.snap.project.name)));
    if let Some(folder) = cx.snap.path.as_deref().and_then(Path::parent) {
        dialog = dialog.set_directory(folder);
    }
    cx.jobs.spawn(ctx, move |report| {
        report(match pollster::block_on(dialog.save_file()) {
            Some(file) => {
                let path = with_extension(file.path(), ending);
                let written = session.lock().export_journal(&path, conversation);
                Event::StoryExported(written.map(|()| (path, conversation)).map_err(|error| error.to_string()))
            }
            None => Event::DialogCancelled,
        });
    });
}

/// Asks for a making-of that was written out to a file, to watch.
pub fn watch_story(ctx: &egui::Context, cx: &mut Cx) {
    if std::mem::replace(&mut cx.jobs.dialog_open, true) {
        return;
    }
    let ending = trill_engine::journal::EXPORT_ENDING;
    let mut dialog =
        rfd::AsyncFileDialog::new().set_title("Watch a making-of").add_filter("Trill making-of", &[ending]);
    if let Some(folder) = cx.snap.path.as_deref().and_then(Path::parent) {
        dialog = dialog.set_directory(folder);
    }
    cx.jobs.spawn(ctx, move |report| {
        report(match pollster::block_on(dialog.pick_file()) {
            Some(file) => Event::StoryPicked(file.path().to_path_buf()),
            None => Event::DialogCancelled,
        });
    });
}

/// Keeps a journal of how songs are made from now on, or stops, and
/// remembers the choice for the next run.
pub fn keep_journal(cx: &mut Cx, keep: bool) {
    cx.with_session(|session| session.set_journal_keeping(keep));
    cx.sounds.quietly(move |runtime| {
        runtime.update_settings(|settings| settings.journal.keep = keep).map_err(|error| error.to_string())
    });
    let said = if keep {
        "A journal is kept from here on"
    } else {
        "No journal is kept from here on. What was written stays until it is deleted."
    };
    cx.state.toasts.info(said);
}

/// `path` with `extension` appended unless it already ends in it. Save
/// dialogs do not reliably add the extension themselves.
pub(super) fn with_extension(path: &Path, extension: &str) -> PathBuf {
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
pub(super) fn file_stem_for(name: &str) -> String {
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
        file_name(Path::new(report.path.as_deref().unwrap_or_default())),
        seconds / 60,
        seconds % 60,
        report.sample_rate as f32 / 1000.0,
        stats.peak_db,
        stats.loudness_lufs,
    );
    if let Some(first) = report.stems.first() {
        let folder = Path::new(&first.path).parent().map(file_name).unwrap_or_default();
        summary.push_str(&format!(" {} stems in \u{201C}{folder}\u{201D}.", report.stems.len()));
    }
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
            path: Some("C:/music/night drive.wav".into()),
            sample_rate: 48_000,
            stats: AudioStats {
                duration_seconds: 64.4,
                peak_db: -1.04,
                true_peak_db: None,
                rms_db: -16.0,
                loudness_lufs: -13.96,
                clipped_samples,
                stereo_correlation: 0.4,
                spectrum: Spectrum { sub_db: -20.0, bass_db: -6.0, mid_db: -3.0, presence_db: -12.0, air_db: -20.0 },
            },
            tracks: Vec::new(),
            sections: Vec::new(),
            dynamics: Vec::new(),
            waited: Vec::new(),
            stems: Vec::new(),
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
        let report = render(&session, &path, false).unwrap();
        assert_eq!(report.sample_rate, 48_000);
        // Sixteen bars at 112 BPM are 34.3 s; the tail rings a little longer.
        assert!((34.0..46.0).contains(&report.stats.duration_seconds), "{} s", report.stats.duration_seconds);
        assert!(report.stats.peak_db > -30.0 && report.stats.peak_db <= 0.0, "peak {} dBFS", report.stats.peak_db);
        assert_eq!(report.stats.clipped_samples, 0, "the master limiter keeps the demo from clipping");
        assert!(report.problems.is_empty());
        let bytes = std::fs::metadata(&path).unwrap().len();
        assert!(bytes > 1_000_000, "a real file was written: {bytes} bytes");
        assert!(export_summary(&report).starts_with("Exported night drive.wav (0:"));
        assert!(!scratch.0.join("night drive stems").exists(), "no stems unless they are asked for");

        // With stems: the same song, and every track that plays, every
        // group and every return in a folder beside it.
        let with_stems = render(&session, &path, true).unwrap();
        assert_eq!(with_stems.stats, report.stats, "the song itself is the same");
        let playing = {
            let session = session.lock();
            let has_clips =
                |track: &&trill_core::Track| track.kind != trill_core::TrackKind::Normal || !track.clips.is_empty();
            session.project().tracks.iter().filter(has_clips).count()
        };
        assert!(playing > 2 && with_stems.stems.len() == playing, "{} of {playing}", with_stems.stems.len());
        for stem in &with_stems.stems {
            let file = Path::new(&stem.path);
            assert_eq!(file.parent(), Some(scratch.0.join("night drive stems").as_path()));
            assert!(std::fs::metadata(file).unwrap().len() > 1_000_000, "{}", stem.path);
        }
        let summary = export_summary(&with_stems);
        let ending = format!(" {playing} stems in \u{201C}night drive stems\u{201D}.");
        assert!(summary.ends_with(&ending), "{summary}");

        // An empty project has nothing to render, and says so.
        session.lock().new_project();
        let error = render(&session, &scratch.0.join("empty.wav"), false).unwrap_err();
        assert!(error.contains("nothing to render"), "{error}");
    }

    /// A two-part MIDI file: a melody and a drum pattern.
    fn midi_song() -> MidiFile {
        let note = |pitch: u8, start: f64| trill_core::Note { pitch, start, length: 0.5, velocity: 100 };
        MidiFile {
            tempo: Some(100.0),
            time_signature: None,
            tracks: vec![
                trill_core::midi_file::MidiTrack {
                    name: "Lead".into(),
                    drums: false,
                    notes: vec![note(60, 0.0), note(64, 5.0)],
                    controllers: Vec::new(),
                },
                trill_core::midi_file::MidiTrack {
                    name: String::new(),
                    drums: true,
                    notes: vec![note(36, 0.0), note(38, 1.0)],
                    controllers: Vec::new(),
                },
                trill_core::midi_file::MidiTrack {
                    name: "Empty".into(),
                    drums: false,
                    notes: Vec::new(),
                    controllers: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn importing_midi_adds_playable_tracks_as_one_undo_step() {
        let session = shared_session();
        session.lock().edit(|project| Ok(project.add_track("Existing"))).unwrap();
        let parts = import_midi_file(&mut session.lock(), &midi_song(), 8.0).unwrap();
        assert_eq!(parts.len(), 2, "a part without notes makes no track");

        let project = session.lock().project().clone();
        assert_eq!(project.tracks.len(), 3);
        let (lead, drums) = (&project.tracks[1], &project.tracks[2]);
        assert_eq!((lead.name.as_str(), lead.devices[0].uri.as_str()), ("Lead", MELODIC_INSTRUMENT));
        assert_eq!(drums.devices[0].uri, DRUM_INSTRUMENT, "the drum part gets a drum kit");
        assert_eq!((lead.clips[0].start, lead.clips[0].length), (8.0, 8.0), "at the cursor, in whole bars");
        assert_eq!(lead.clips[0].notes().len(), 2);
        assert_eq!(lead.color, colors::new_track_color(1), "the next hues of the palette");
        assert_eq!(drums.color, colors::new_track_color(2));
        assert_eq!(project.tempo, 120.0, "the song keeps its tempo");

        assert!(session.lock().undo());
        assert_eq!(session.lock().project().tracks.len(), 1, "tracks, clips and instruments go in one step");
    }

    #[test]
    fn a_device_is_given_the_preset_in_a_file_as_one_undo_step() {
        let scratch = Scratch::new("preset-file");
        let session = shared_session();
        // A sound someone saved, as the file it was saved in.
        let (synth, file) = {
            let mut session = session.lock();
            session.saved_presets().open(scratch.0.join("saved"));
            let synth = session.add_device(ChainRef::Master, MELODIC_INSTRUMENT, Some("warm_pad"), None).unwrap();
            session.set_device_params(synth, &[("cutoff".into(), 432.0)]).unwrap();
            let file = session.save_preset(synth, "Dark Pad", "", &[]).unwrap().file.unwrap();
            session.load_preset(synth, "sub_bass").unwrap();
            (synth, file)
        };
        let cutoff = |session: &SharedSession| {
            let params = session.lock().device_params(synth).unwrap();
            params.iter().find(|param| param.info.id == "cutoff").unwrap().value
        };
        assert_ne!(cutoff(&session), 432.0);
        let step = session.lock().undo_step();

        let loaded = give_preset_file(&session, synth, &file).unwrap();
        assert_eq!((loaded.preset.as_str(), loaded.device.as_str(), loaded.note), ("Dark Pad", "Poly Synth", None));
        assert_eq!(cutoff(&session), 432.0);
        assert!(session.lock().undo());
        assert_eq!(session.lock().undo_step(), step);
        assert_ne!(cutoff(&session), 432.0);

        // A file that is no preset of the device: the sentence, and the
        // device as it was.
        let wav = scratch.0.join("Kick.wav");
        std::fs::write(&wav, b"RIFF").unwrap();
        let error = give_preset_file(&session, synth, &wav).unwrap_err();
        assert!(error.contains("Kick.wav") && error.contains("takes no preset files"), "{error}");
        assert_eq!(session.lock().undo_step(), step);
        assert!(give_preset_file(&session, DeviceId(9_999), &file).is_err());
    }

    #[test]
    fn a_midi_file_survives_a_round_trip_through_disk() {
        let scratch = Scratch::new("midi");
        let session = shared_session();
        let empty = write_midi(&session, &scratch.0.join("empty.mid")).unwrap_err();
        assert!(empty.contains("no MIDI notes"), "{empty}");

        import_midi_file(&mut session.lock(), &midi_song(), 0.0).unwrap();
        let path = scratch.0.join("song.mid");
        assert_eq!(write_midi(&session, &path), Ok(path.clone()));

        session.lock().new_project();
        assert_eq!(read_midi(&session, &path, 4.0), Ok(("song.mid".to_owned(), 2)));
        let project = session.lock().project().clone();
        let starts: Vec<f64> = project.tracks[0].clips[0].timeline_notes().iter().map(|note| note.start).collect();
        assert_eq!(starts, [4.0, 9.0], "the notes, moved to where the file was imported");
        assert_eq!(project.tracks[1].devices[0].uri, DRUM_INSTRUMENT, "the drum part is still one");

        let broken = scratch.0.join("broken.mid");
        std::fs::write(&broken, b"not a midi file").unwrap();
        let error = read_midi(&session, &broken, 0.0).unwrap_err();
        assert!(error.contains("broken.mid"), "{error}");
        assert_eq!(session.lock().project().tracks.len(), 2, "a failed import changes nothing");
        assert!(read_midi(&session, &scratch.0.join("missing.mid"), 0.0).is_err());
    }

    #[test]
    fn exported_midi_files_get_a_midi_extension() {
        assert_eq!(with_midi_extension(Path::new("song")), PathBuf::from("song.mid"));
        assert_eq!(with_midi_extension(Path::new("song.mid")), PathBuf::from("song.mid"));
        assert_eq!(with_midi_extension(Path::new("song.MIDI")), PathBuf::from("song.MIDI"));
        assert_eq!(with_midi_extension(Path::new("song.v2")), PathBuf::from("song.v2.mid"));
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

        jobs.dialog_open = true;
        jobs.sender.send(Event::MidiImported(Ok(("beat.mid".into(), 3)))).unwrap();
        jobs.poll(&mut state);
        assert!(!jobs.dialog_open, "the dialog is done, so another may open");
        jobs.sender.send(Event::MidiExported(Err("read-only folder".into()))).unwrap();
        jobs.poll(&mut state);
        assert_eq!(state.toasts.len(), 4);

        jobs.dialog_open = true;
        let loaded = PresetLoaded { preset: "Dark Pad".into(), device: "Poly Synth".into(), note: None };
        jobs.sender.send(Event::PresetLoaded(Ok(loaded.clone()))).unwrap();
        jobs.poll(&mut state);
        assert!(!jobs.dialog_open);
        let with_note = PresetLoaded { note: Some("It reads its samples.".into()), ..loaded };
        jobs.sender.send(Event::PresetLoaded(Ok(with_note))).unwrap();
        jobs.sender.send(Event::PresetLoaded(Err("'x.wav' cannot be given".into()))).unwrap();
        jobs.poll(&mut state);
        let said = state.toasts.texts();
        assert_eq!(
            said[said.len() - 3..],
            [
                "Poly Synth has the preset \u{201C}Dark Pad\u{201D}",
                "Poly Synth has the preset \u{201C}Dark Pad\u{201D}. It reads its samples.",
                "'x.wav' cannot be given",
            ]
        );
    }
}
