//! Sounds, as the window deals with them: waveforms, previews, library
//! searches, and the drops that bring a file into the song.
//!
//! Every one of those reads audio files or the library, so none of it
//! happens on the UI thread. [`Sounds`] hands the work to [`Work`]'s
//! threads and keeps what came back:
//!
//! * [`Sounds::wave`] answers at once with what is known about a file's
//!   waveform (still loading, ready, or why not) and asks for it if nobody
//!   has. Views call it while painting and draw a placeholder meanwhile.
//! * [`Sounds::preview`] plays a file over the song; only the latest
//!   request is ever heard.
//! * [`Sounds::search`] keeps the browser's list in step with its query and
//!   with the library as a scan fills it in.
//! * [`Sounds::drop_files`] puts files where they were dropped: on a track
//!   as clips, in a sampler, on the pads of a drum rack. One drop is one
//!   undo step.
//!
//! * [`Sounds::watch_missing`] keeps up with the files the song names that
//!   are not on disk, and what was found for them (`missing.rs`).
//!
//! The edits themselves are the runtime's ([`Runtime::add_sound`],
//! [`Runtime::relink`]) and the session's (`set_sampler_file`,
//! `set_pad_file`): the same ones the MCP server uses.

mod instruments;
mod missing;
mod waves;

pub use instruments::{InstrumentRow, is_instrument, size_words};
pub use missing::{MissingFiles, Relinked, relinked_words};
pub use waves::{WaveData, WaveState};

use super::work::{Priority, Work};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use trill_core::files::{FilePlace, FolderMove, Relink};
use trill_core::project::WarpMode;
use trill_core::{Beats, ClipId, DeviceId, TrackId};
use trill_dsp::drumrack;
use trill_library::{Query, SearchResults, Tag, Tags};
use trill_runtime::{NewSoundTrack, Runtime, SoundClip, SoundFacts};
use waves::Waves;

/// Where dropped sounds go.
#[derive(Clone, Debug, PartialEq)]
pub enum DropTarget {
    /// Onto a track, as audio clips from `beat` on.
    Lane { track: TrackId, beat: Beats },
    /// Below the tracks: a new audio track per file, each clip at `beat`.
    /// `colors` are what the new tracks are given, in order.
    NewTrack { beat: Beats, colors: Vec<u32> },
    /// Into a sampler: the file becomes what it plays.
    Sampler { device: DeviceId },
    /// Onto a pad of a drum rack, and the pads after it for more files.
    Pad { device: DeviceId, note: u8 },
    /// Onto a drum rack as a whole: each file goes to the pad its name
    /// suggests, or failing that the first empty one.
    Rack { device: DeviceId },
    /// An instrument's file into a Sampled Instrument: what it plays.
    Instrument { device: DeviceId },
    /// An instrument's file onto a track: it becomes what the track plays
    /// through, in place of the instrument the track has.
    TrackInstrument { track: TrackId },
    /// An instrument's file below the tracks: a new track that plays
    /// through it, in `color`.
    NewInstrument { color: Option<u32> },
}

impl DropTarget {
    /// The place itself for what is being dragged: a sound (`instrument`
    /// is false) or an instrument's file. An instrument over a track is
    /// the track's instrument, not a clip on it; where the one cannot go
    /// that the other can, the answer is why, for the status bar.
    pub fn for_drag(self, instrument: bool) -> Result<DropTarget, &'static str> {
        match (self, instrument) {
            (DropTarget::Lane { track, .. }, true) => Ok(DropTarget::TrackInstrument { track }),
            (DropTarget::NewTrack { colors, .. }, true) => {
                Ok(DropTarget::NewInstrument { color: colors.first().copied() })
            }
            (DropTarget::Sampler { .. } | DropTarget::Pad { .. } | DropTarget::Rack { .. }, true) => {
                Err("A sampler and a pad play one sound: an instrument goes onto a track or a Sampled Instrument")
            }
            (
                DropTarget::Instrument { .. } | DropTarget::TrackInstrument { .. } | DropTarget::NewInstrument { .. },
                false,
            ) => Err("A Sampled Instrument plays an instrument's file (.sfz), not a single sound"),
            (target, _) => Ok(target),
        }
    }
}

/// What a drop did, for selecting it and telling the user.
#[derive(Clone, Debug, PartialEq)]
pub enum Dropped {
    Clips(Vec<(TrackId, ClipId)>),
    /// A sampler was given `name`.
    Sample {
        device: DeviceId,
        name: String,
    },
    /// These pads of a drum rack were given files.
    Pads {
        device: DeviceId,
        notes: Vec<u8>,
    },
    /// A Sampled Instrument was given the instrument `name`: on `track`,
    /// where the drop said which or made one.
    Instrument {
        device: DeviceId,
        track: Option<TrackId>,
        name: String,
    },
}

/// What a worker reports back.
pub enum Done {
    Wave {
        path: String,
        result: Result<Arc<WaveData>, String>,
    },
    Found {
        ticket: u64,
        results: Box<Found>,
    },
    Preview {
        ticket: u64,
        path: PathBuf,
        /// The sound's length in seconds and when it began to play; `None`
        /// when a later request overtook this one before it was heard.
        result: Result<Option<(f64, Instant)>, String>,
    },
    Dropped {
        id: u64,
        result: Result<Dropped, String>,
    },
    /// A file was taken off a sampler or a pad.
    Cleared(Result<(), String>),
    /// The library's instruments, and the generation they were read at.
    Instruments {
        generation: u64,
        rows: Vec<InstrumentRow>,
    },
    /// What the song owed for its sampled instruments at a revision.
    Owed {
        revision: u64,
        owed: Box<trill_catalogue::Owed>,
    },
    /// A dialog was answered: with a folder for the library, with a file
    /// for a sampler or a pad, or with where a clip's file is now. `None`
    /// when it was dismissed.
    FolderPicked(Option<PathBuf>),
    FilePicked(Option<(PathBuf, DropTarget)>),
    RelinkPicked(Option<(ClipId, PathBuf)>),
    /// A folder was added to the library, or why it was not.
    FolderAdded(Result<PathBuf, String>),
    /// A clip was pointed at another file: the file's name, and where
    /// that says its folder went if its file was missing; or why not.
    Relinked(Result<(String, Option<trill_core::FolderMove>), String>),
    /// What a look for the song's missing files found.
    Looked {
        ticket: u64,
        missing: Box<trill_runtime::Missing>,
    },
    /// Missing files were relinked, or why not.
    FilesRelinked(Result<Relinked, String>),
    /// Dialogs about what is missing were answered: where one file is,
    /// a folder to look in, where a folder of the library went.
    FileLocated(Option<(String, PathBuf)>),
    LookIn(Option<PathBuf>),
    LibraryLocated(Option<(PathBuf, PathBuf)>),
    /// The library was told where a folder of its own went: from where to
    /// where, or why not.
    LibraryMoved(Result<(PathBuf, PathBuf), String>),
    /// Something the user need not hear about unless it failed.
    Quiet(Result<(), String>),
}

/// The answer to a library search.
pub struct Found {
    pub query: Query,
    /// The library's generation the answer was read at.
    pub generation: u64,
    pub results: SearchResults,
    /// How many sounds the library holds, matching or not.
    pub total: usize,
    /// The sound that was selected when the search began, and where in
    /// the results it is.
    pub selected: Option<PathBuf>,
    pub selected_row: Option<usize>,
}

impl Found {
    /// Where in the results the file at `path` is. A lookup in the index
    /// and a pass over the matches' numbers: microseconds, even for a
    /// hundred thousand of them.
    pub fn row_of(&self, path: &Path) -> Option<usize> {
        let id = self.results.index().find(path)?;
        self.results.ids().iter().position(|found| *found == id)
    }
}

/// The sound being auditioned from the browser.
#[derive(Default)]
pub struct Preview {
    /// Counts requests. A worker gives up when its request is no longer
    /// the latest, so holding an arrow key down decodes little and plays
    /// only where it stops.
    latest: Arc<AtomicU64>,
    /// The request that is being loaded.
    pub loading: Option<PathBuf>,
    /// What is playing, how long it is and when it started.
    pub playing: Option<(PathBuf, f64, Instant)>,
    /// Why the last request could not be played.
    pub error: Option<(PathBuf, String)>,
}

impl Preview {
    /// How far through `path` the audition is, 0..1, while it plays.
    pub fn position(&self, path: &Path) -> Option<f32> {
        let (playing, seconds, started) = self.playing.as_ref()?;
        let elapsed = started.elapsed().as_secs_f64();
        (playing == path && *seconds > 0.0 && elapsed < *seconds).then(|| (elapsed / seconds) as f32)
    }

    pub fn is_playing(&self) -> bool {
        self.playing.as_ref().is_some_and(|(_, seconds, started)| started.elapsed().as_secs_f64() < *seconds)
    }
}

/// The browser's list of sounds and how it was asked for.
#[derive(Default)]
pub struct Search {
    /// The answer on show.
    pub found: Option<Found>,
    /// Set when an answer arrives; the browser clears it once it has taken
    /// over what the answer says about the selection.
    pub fresh: bool,
    /// Counts searches sent, and the one still out.
    tickets: u64,
    in_flight: Option<u64>,
}

impl Search {
    /// Makes the list on show count as old, so that the next look sends a
    /// search: for when what is selected changed behind the list's back.
    pub fn outdate(&mut self) {
        if let Some(found) = &mut self.found {
            found.generation = u64::MAX;
        }
    }
}

/// A drop whose file is still being read. The arrangement shows where its
/// clips will appear.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingDrop {
    pub id: u64,
    pub target: DropTarget,
    pub label: String,
    /// The length the first clip will have, in beats, as far as is known.
    pub length: Beats,
}

/// What to say once work has come back: the window shows these as toasts
/// and acts on what was dropped.
pub enum Report {
    Info(String),
    Error(String),
    /// Something that went well and is worth a line.
    Success(String),
    /// Something to read that nobody asked about: it stays a while.
    Notice(String),
    /// A folder of the library is somewhere else now.
    LibraryMoved(PathBuf, PathBuf),
    Dropped(Dropped),
    FolderAdded(PathBuf),
    /// A file chosen in a dialog, to be dropped on `target`.
    Picked(PathBuf, DropTarget),
}

pub struct Sounds {
    // Dropped first: the workers hold the runtime, and it must not end on
    // one of their threads.
    work: Work,
    pub runtime: Arc<Runtime>,
    waves: Waves,
    pub preview: Preview,
    pub search: Search,
    pub pending: Vec<PendingDrop>,
    /// The library's sampled instruments, for the browser's Devices tab.
    instruments: instruments::Shelf,
    /// What the song owes for its sampled instruments, for the credits.
    credits: instruments::Credits,
    /// The files the song names that are not on disk, and what was found.
    pub missing: MissingFiles,
    next_drop: u64,
    /// A native dialog is up; a second one must not open over it.
    dialog_open: bool,
}

impl Sounds {
    pub fn new(ctx: &egui::Context, runtime: Arc<Runtime>) -> Self {
        Self {
            work: Work::start(ctx),
            runtime,
            waves: Waves::default(),
            preview: Preview::default(),
            search: Search::default(),
            pending: Vec::new(),
            instruments: instruments::Shelf::default(),
            credits: instruments::Credits::default(),
            missing: MissingFiles::default(),
            next_drop: 0,
            dialog_open: false,
        }
    }

    /// What is known about the waveform of the file at `path`, asking for
    /// it if nobody has. Never reads the file: the answer is `Loading`
    /// until a worker has.
    pub fn wave(&self, path: &str) -> WaveState {
        self.waves.get(path)
    }

    /// Has the file at `path` looked at again, if its waveform is known:
    /// for a file the engine has just found missing or changed.
    pub fn recheck(&self, path: &str) {
        self.waves.recheck(path);
    }

    /// The same, without asking: for callers that only want what is there.
    pub fn known_wave(&self, path: &str) -> Option<Arc<WaveData>> {
        self.waves.peek(path)
    }

    /// True while something the window shows is still on its way: a scan
    /// of the library, the answer to a search, a waveform drawn as a
    /// placeholder, a drop. A screenshot waits for this to clear. Work that
    /// goes on for as long as the window is open (looking again for a file
    /// that is missing) does not count.
    pub fn is_waiting(&self) -> bool {
        self.waves.is_loading()
            || self.search.in_flight.is_some()
            || !self.pending.is_empty()
            || self.preview.loading.is_some()
            || self.missing.looking
            || self.instruments.in_flight
            || self.credits.in_flight
            || self.runtime.library.scan_status().is_scanning()
    }

    /// True while the workers have anything at all to do, or a scan runs:
    /// for tests, which wait until nothing more can arrive.
    #[cfg(test)]
    pub fn is_busy(&self) -> bool {
        !self.work.is_idle() || self.waves.is_loading() || self.runtime.library.scan_status().is_scanning()
    }

    /// Takes in what the workers finished and sends them what the last
    /// frame asked for. Call once per frame, before the views run.
    pub fn pump(&mut self) -> Vec<Report> {
        let mut reports = Vec::new();
        for done in self.work.finished() {
            match done {
                Done::Wave { path, result } => self.waves.store(&path, result),
                Done::Found { ticket, results } => {
                    if self.search.in_flight == Some(ticket) {
                        self.search.in_flight = None;
                    }
                    // The list on show may hold the last reference to an
                    // index of a hundred thousand files, which takes
                    // milliseconds to free: not here.
                    if let Some(old) = self.search.found.replace(*results) {
                        self.work.spawn(Priority::Bulk, move || {
                            drop(old);
                            Done::Quiet(Ok(()))
                        });
                    }
                    self.search.fresh = true;
                }
                Done::Preview { ticket, path, result } => {
                    if ticket != self.preview.latest.load(Ordering::Relaxed) {
                        continue;
                    }
                    self.preview.loading = None;
                    match result {
                        Ok(Some((seconds, started))) => self.preview.playing = Some((path, seconds, started)),
                        Ok(None) => {}
                        Err(reason) => {
                            self.preview.playing = None;
                            self.preview.error = Some((path, reason.clone()));
                            reports.push(Report::Error(reason));
                        }
                    }
                }
                Done::Dropped { id, result } => {
                    self.pending.retain(|pending| pending.id != id);
                    match result {
                        Ok(dropped) => reports.push(Report::Dropped(dropped)),
                        Err(reason) => reports.push(Report::Error(reason)),
                    }
                }
                Done::Instruments { generation, rows } => {
                    self.instruments.in_flight = false;
                    self.instruments.generation = Some(generation);
                    self.instruments.rows = Arc::new(rows);
                }
                Done::Owed { revision, owed } => {
                    self.credits.in_flight = false;
                    self.credits.revision = Some(revision);
                    self.credits.owed = Some(Arc::new(*owed));
                }
                Done::Cleared(result) | Done::Quiet(result) => {
                    if let Err(reason) = result {
                        reports.push(Report::Error(reason));
                    }
                }
                Done::FolderPicked(answer) => {
                    self.dialog_open = false;
                    if let Some(folder) = answer {
                        self.add_folder(folder);
                    }
                }
                Done::FolderAdded(Ok(folder)) => reports.push(Report::FolderAdded(folder)),
                Done::FolderAdded(Err(reason)) => reports.push(Report::Info(reason)),
                Done::FilePicked(answer) => {
                    self.dialog_open = false;
                    if let Some((path, target)) = answer {
                        reports.push(Report::Picked(path, target));
                    }
                }
                Done::RelinkPicked(answer) => {
                    self.dialog_open = false;
                    if let Some((clip, path)) = answer {
                        self.relink(clip, path);
                    }
                }
                Done::Relinked(Ok((name, moved))) => {
                    // Where its folder went is where the other missing
                    // files are looked for.
                    if let Some(moved) = moved {
                        self.missing.point_at(moved);
                    }
                    self.missing.look_again();
                    reports.push(Report::Info(format!("The clip now plays {name}")));
                }
                Done::Relinked(Err(reason)) => reports.push(Report::Error(reason)),
                Done::Looked { ticket, missing } => reports.extend(self.looked(ticket, *missing).map(Report::Notice)),
                Done::FilesRelinked(result) => {
                    self.missing.busy = false;
                    self.missing.look_again();
                    match result {
                        Ok(relinked) => {
                            reports.push(Report::Success(relinked_words(&relinked)));
                            let moved = relinked.library.into_iter();
                            reports.extend(moved.map(|(from, to)| Report::LibraryMoved(from, to)));
                        }
                        Err(reason) => reports.push(Report::Error(reason)),
                    }
                }
                Done::FileLocated(answer) => {
                    self.dialog_open = false;
                    if let Some((old, new)) = answer {
                        self.relink_file(old, new);
                    }
                }
                Done::LookIn(answer) => {
                    self.dialog_open = false;
                    if let Some(folder) = answer {
                        self.look_in(folder);
                    }
                }
                Done::LibraryLocated(answer) => {
                    self.dialog_open = false;
                    if let Some((from, to)) = answer {
                        self.move_library_folder(from, to);
                    }
                }
                Done::LibraryMoved(result) => {
                    self.missing.busy = false;
                    self.missing.look_again();
                    match result {
                        Ok((from, to)) => {
                            reports.push(Report::Success(format!("The library now has {}", to.display())));
                            reports.push(Report::LibraryMoved(from, to));
                        }
                        Err(reason) => reports.push(Report::Error(reason)),
                    }
                }
            }
        }
        for path in self.waves.take_requests() {
            let runtime = self.runtime.clone();
            self.work.spawn(Priority::Bulk, move || {
                let result = waves::load(&runtime, Path::new(&path)).map(Arc::new).map_err(|error| error.to_string());
                Done::Wave { path, result }
            });
        }
        reports
    }

    // ---- previewing ----------------------------------------------------

    /// Plays the file over the song. Whatever was being previewed, or
    /// loaded to be, gives way.
    pub fn preview(&mut self, path: &Path, gain_db: f32) {
        let ticket = self.preview.latest.fetch_add(1, Ordering::Relaxed) + 1;
        self.preview.loading = Some(path.to_path_buf());
        self.preview.error = None;
        let (runtime, latest, path) = (self.runtime.clone(), self.preview.latest.clone(), path.to_path_buf());
        self.work.spawn(Priority::Urgent, move || {
            let current = || latest.load(Ordering::Relaxed) == ticket;
            let result = (|| {
                if !current() {
                    return Ok(None);
                }
                // The slow part, with the session free. If the user has
                // moved on meanwhile, nothing is played.
                let rate = runtime.session.lock().audio_info().sample_rate;
                runtime.media.audio_at_rate(&path, rate).map_err(|error| error.to_string())?;
                if !current() {
                    return Ok(None);
                }
                let seconds = runtime.preview(&path, gain_db).map_err(|error| error.to_string())?;
                Ok(Some((seconds, Instant::now())))
            })();
            Done::Preview { ticket, path, result }
        });
    }

    /// Fades out what is being previewed and forgets what was about to be.
    pub fn stop_preview(&mut self) {
        self.preview.latest.fetch_add(1, Ordering::Relaxed);
        self.preview.loading = None;
        if self.preview.playing.take().is_some() {
            self.runtime.stop_preview();
        }
    }

    // ---- searching -----------------------------------------------------

    /// Keeps the list in step with `query`: if it, or the library, is not
    /// what the list on show was made from, a search is sent (one at a
    /// time, so typing fast skips the answers nobody will see).
    /// `selected` is looked up in the new list.
    pub fn search(&mut self, query: &Query, selected: Option<&Path>) {
        let generation = self.runtime.library.generation();
        let current =
            self.search.found.as_ref().is_some_and(|found| found.query == *query && found.generation == generation);
        if current || self.search.in_flight.is_some() {
            return;
        }
        self.search.tickets += 1;
        let ticket = self.search.tickets;
        self.search.in_flight = Some(ticket);
        let (runtime, query, selected) = (self.runtime.clone(), query.clone(), selected.map(Path::to_path_buf));
        self.work.spawn(Priority::Urgent, move || {
            // Read before the search, so a change during it is not missed:
            // the answer then counts as old and is asked for again.
            let generation = runtime.library.generation();
            let results = runtime.library.search(&query);
            // Counted here: it is a pass over every file of the library.
            let total = results.index().count(trill_library::EntryKind::Audio);
            let mut found = Found { query, generation, results, total, selected, selected_row: None };
            found.selected_row = found.selected.as_deref().and_then(|path| found.row_of(path));
            Done::Found { ticket, results: Box::new(found) }
        });
    }

    // ---- dropping ------------------------------------------------------

    /// Puts `paths` where they were dropped. Returns at once; the files
    /// are read and the project changed on a worker, as one undo step, and
    /// the outcome arrives through [`Sounds::pump`]. `label` and `length`
    /// describe the first file while it is on its way.
    pub fn drop_files(&mut self, paths: Vec<PathBuf>, target: DropTarget, label: String, length: Beats) {
        if paths.is_empty() {
            return;
        }
        self.next_drop += 1;
        let id = self.next_drop;
        self.pending.push(PendingDrop { id, target: target.clone(), label, length });
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || Done::Dropped { id, result: place(&runtime, &paths, &target) });
    }

    /// The sampled instruments of the library, by name. The list is read
    /// on a worker whenever the library has changed; until the answer is
    /// there this is the list as it was (empty, the first time).
    pub fn instruments(&mut self) -> Arc<Vec<InstrumentRow>> {
        let generation = self.runtime.library.generation();
        if self.instruments.generation != Some(generation) && !self.instruments.in_flight {
            self.instruments.in_flight = true;
            let runtime = self.runtime.clone();
            self.work.spawn(Priority::Urgent, move || {
                let (generation, rows) = instruments::list(&runtime);
                Done::Instruments { generation, rows }
            });
        }
        self.instruments.rows.clone()
    }

    /// What the song owes for the sampled instruments it plays, as of
    /// `revision` of it. Worked out on a worker, since it reads the
    /// instruments' files: until the answer for this revision is there,
    /// this is the answer for an earlier one, or `None` the first time.
    pub fn owed(&mut self, revision: u64) -> Option<Arc<trill_catalogue::Owed>> {
        if self.credits.revision != Some(revision) && !self.credits.in_flight {
            self.credits.in_flight = true;
            let runtime = self.runtime.clone();
            self.work.spawn(Priority::Urgent, move || Done::Owed { revision, owed: Box::new(runtime.owed()) });
        }
        self.credits.owed.clone()
    }

    /// Takes the file off a sampler (`note` is `None`) or off a pad.
    pub fn clear_sample(&mut self, device: DeviceId, note: Option<u8>) {
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || {
            let mut session = runtime.session.lock();
            let result = match note {
                Some(note) => session.set_pad_file(device, note, None),
                None => session.set_sampler_file(device, None),
            };
            Done::Cleared(result.map_err(|error| error.to_string()))
        });
    }

    // ---- the library's folders -----------------------------------------

    /// Asks for a folder and adds it to the library, which starts scanning
    /// it. `start_in` is where the dialog opens.
    pub fn add_folder_dialog(&mut self, start_in: Option<PathBuf>) {
        if std::mem::replace(&mut self.dialog_open, true) {
            return;
        }
        self.work.ask(move || {
            let mut dialog = rfd::AsyncFileDialog::new().set_title("Add a folder of sounds");
            if let Some(folder) = start_in.filter(|folder| folder.is_dir()) {
                dialog = dialog.set_directory(folder);
            }
            Done::FolderPicked(pollster::block_on(dialog.pick_folder()).map(|folder| folder.path().to_path_buf()))
        });
    }

    /// Adds `folder` to the library, which starts scanning it.
    pub fn add_folder(&mut self, folder: PathBuf) {
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || Done::FolderAdded(add_folder(&runtime, &folder)));
    }

    pub fn remove_folder(&mut self, folder: PathBuf) {
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || {
            runtime.library.remove_root(&folder);
            Done::Quiet(runtime.save_library_changes().map_err(|error| error.to_string()))
        });
    }

    /// Asks for an audio file to put on `target` (a sampler or a pad).
    pub fn load_file_dialog(&mut self, target: DropTarget, start_in: Option<PathBuf>) {
        if std::mem::replace(&mut self.dialog_open, true) {
            return;
        }
        self.work.ask(move || {
            let file = pollster::block_on(audio_dialog("Load a sample", start_in).pick_file());
            Done::FilePicked(file.map(|file| (file.path().to_path_buf(), target)))
        });
    }

    /// Asks where the file of `clip` is now and points the clip at it.
    pub fn relink_dialog(&mut self, clip: ClipId, start_in: Option<PathBuf>) {
        if std::mem::replace(&mut self.dialog_open, true) {
            return;
        }
        self.work.ask(move || {
            let file = pollster::block_on(audio_dialog("Locate the audio file", start_in).pick_file());
            Done::RelinkPicked(file.map(|file| (clip, file.path().to_path_buf())))
        });
    }

    /// Points `clip` at the file at `path`: what the dialog's answer does.
    pub fn relink(&mut self, clip: ClipId, path: PathBuf) {
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || Done::Relinked(relink(&runtime, clip, &path)));
    }

    /// Runs `job` on a worker; a failure is shown to the user.
    pub fn quietly(&mut self, job: impl FnOnce(&Runtime) -> Result<(), String> + Send + 'static) {
        let runtime = self.runtime.clone();
        self.work.spawn(Priority::Urgent, move || Done::Quiet(job(&runtime)));
    }

    /// Stops the workers from starting anything, or lets them go on.
    #[cfg(test)]
    pub fn hold(&self, held: bool) {
        self.work.hold(held);
    }

    /// Waits until the workers have nothing left to do.
    #[cfg(test)]
    pub fn wait_until_idle(&self, timeout: std::time::Duration) -> bool {
        self.work.wait_until_idle(timeout)
    }
}

/// A dialog that picks one audio file.
fn audio_dialog(title: &str, start_in: Option<PathBuf>) -> rfd::AsyncFileDialog {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title).add_filter("Audio", trill_media::AUDIO_EXTENSIONS);
    if let Some(folder) = start_in.filter(|folder| folder.is_dir()) {
        dialog = dialog.set_directory(folder);
    }
    dialog
}

fn add_folder(runtime: &Runtime, folder: &Path) -> Result<PathBuf, String> {
    if !runtime.library.add_root(folder) {
        return Err(format!("{} is already in the library", folder.display()));
    }
    // Written now rather than at the housekeeping thread's next look: a
    // folder someone just chose must not be lost to a crash a second later.
    runtime.save_library_changes().map_err(|error| error.to_string())?;
    Ok(folder.to_path_buf())
}

/// Points `clip` at the file at `path`, if that file can be read. A clip
/// whose file is missing takes every other place that names that file
/// along: they are missing the same file, and it is one undo step. Says
/// the new file's name, and for a missing one where the two paths say its
/// folder went.
fn relink(runtime: &Runtime, clip: ClipId, path: &Path) -> Result<(String, Option<FolderMove>), String> {
    runtime.sound_facts(path).map_err(|error| error.to_string())?;
    let stored = stored_path(path);
    let uses = runtime.session.lock().files();
    let here = FilePlace::Clip(clip);
    let old = uses.iter().find(|used| used.place == here).map(|used| used.path.clone());
    let old = old.ok_or_else(|| format!("{clip} plays no audio file"))?;
    let missing = !Path::new(&old).is_file();
    let places = uses.into_iter().filter(|used| if missing { used.path == old } else { used.place == here });
    let relinks: Vec<Relink> = places.map(|used| Relink { place: used.place, path: stored.clone() }).collect();
    runtime.relink(&relinks).map_err(|error| error.to_string())?;
    let moved = missing.then(|| FolderMove::between(&old, &stored)).flatten().map(|(moved, _)| moved);
    Ok((file_name(path), moved))
}

/// The name of the file at `path`, extension included.
pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// The name of a file as a clip or a pad shows it: without its extension.
pub fn file_stem(path: &Path) -> String {
    path.file_stem().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// How a path is written into the project: absolute, so that the song does
/// not depend on where the program was started from.
fn stored_path(path: &Path) -> String {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()).display().to_string()
}

/// How a sound is fitted to the song when it is dropped on a track: a loop
/// whose tempo is known follows the project's, keeping its pitch, and
/// anything else plays as it was recorded.
pub fn warp_for(facts_tags: Tags, tempo: Option<f64>) -> WarpMode {
    if tempo.is_none() || !facts_tags.contains(Tag::Loop) {
        return WarpMode::Off;
    }
    let drums = Tags::of(&[Tag::Drum, Tag::Kick, Tag::Snare, Tag::Clap, Tag::Hat, Tag::Tom, Tag::Cymbal, Tag::Perc]);
    if facts_tags.intersects(drums) { WarpMode::Beats } else { WarpMode::Tonal }
}

/// The length in beats a sound dropped on a track will have in a project
/// at `project_tempo`: what [`warp_for`] and the runtime make of it.
pub fn dropped_length(seconds: f64, tags: Tags, tempo: Option<f64>, project_tempo: f64) -> Beats {
    match (warp_for(tags, tempo), tempo) {
        (WarpMode::Off, _) | (_, None) => seconds * project_tempo / 60.0,
        (_, Some(tempo)) => seconds * tempo / 60.0,
    }
}

/// The pad a sound belongs on by what it is, on the General MIDI notes the
/// drum rack's pads follow.
pub fn pad_for(tags: Tags, name: &str) -> Option<u8> {
    let name = name.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|word| name.contains(word));
    let by_tag = [
        (Tag::Kick, 36),
        (Tag::Snare, 38),
        (Tag::Clap, 39),
        (Tag::Hat, if says(&["open", "ohh", "ohat"]) { 46 } else { 42 }),
        (
            Tag::Tom,
            if says(&["low", "floor"]) {
                41
            } else if says(&["hi", "high"]) {
                48
            } else {
                45
            },
        ),
        (Tag::Cymbal, if says(&["ride"]) { 51 } else { 49 }),
        (Tag::Perc, 37),
    ];
    by_tag.into_iter().find(|(tag, _)| tags.contains(*tag)).map(|(_, note)| note)
}

/// Reads the files and changes the project: the body of a drop. Everything
/// that can fail for one file is done before the project is touched, so a
/// drop is all of its files or none.
fn place(runtime: &Runtime, paths: &[PathBuf], target: &DropTarget) -> Result<Dropped, String> {
    // An instrument is not read as a sound is: the device it goes to
    // reads it, and says what is wrong with it.
    if let Some(instrument) = paths.iter().find(|path| is_instrument(path)) {
        return instruments::place(runtime, instrument, target);
    }
    let rate = runtime.session.lock().audio_info().sample_rate;
    let mut sounds: Vec<(&PathBuf, SoundFacts)> = Vec::with_capacity(paths.len());
    for path in paths {
        let facts = runtime.sound_facts(path).map_err(|error| error.to_string())?;
        // Decoded and converted here, with the session free. The edit
        // below then finds the audio waiting in the cache.
        runtime.media.audio(path).map_err(|error| error.to_string())?;
        runtime.media.audio_at_rate(path, rate).map_err(|error| error.to_string())?;
        sounds.push((path, facts));
    }
    // Several files are several edits; a gesture makes them one undo step.
    let several = sounds.len() > 1;
    let before = {
        let mut session = runtime.session.lock();
        if several {
            session.begin_gesture();
        }
        session.revision()
    };
    let result = place_read(runtime, &sounds, target);
    if several {
        let mut session = runtime.session.lock();
        session.end_gesture();
        // Part of a drop is no drop: take back what the gesture did.
        if result.is_err() && session.revision() != before {
            session.undo();
        }
    }
    result
}

fn place_read(runtime: &Runtime, sounds: &[(&PathBuf, SoundFacts)], target: &DropTarget) -> Result<Dropped, String> {
    let sound_clip = |path: &Path, facts: &SoundFacts, track: TrackId, start: Beats| SoundClip {
        warp: warp_for(facts.tags, facts.tempo),
        ..SoundClip::new(path, track, start)
    };
    match target {
        DropTarget::Lane { track, beat } => {
            // One after the other along the track.
            let mut start = *beat;
            let mut clips = Vec::new();
            for (path, facts) in sounds {
                let added = runtime.add_sound(&sound_clip(path, facts, *track, start)).map_err(|e| e.to_string())?;
                start += added.length;
                clips.push((*track, added.clip));
            }
            Ok(Dropped::Clips(clips))
        }
        DropTarget::NewTrack { beat, colors } => {
            let mut clips = Vec::new();
            for (index, (path, facts)) in sounds.iter().enumerate() {
                let new_track = NewSoundTrack { name: None, color: colors.get(index).copied() };
                // The track of the request is not looked at.
                let sound = sound_clip(path, facts, TrackId(0), *beat);
                let (track, added) = runtime.add_sound_on_new_track(&sound, &new_track).map_err(|e| e.to_string())?;
                clips.push((track, added.clip));
            }
            Ok(Dropped::Clips(clips))
        }
        DropTarget::Sampler { device } => {
            let (path, _) = sounds[0];
            let mut session = runtime.session.lock();
            session.set_sampler_file(*device, Some(&stored_path(path))).map_err(|error| error.to_string())?;
            drop(session);
            runtime.library.mark_used(path);
            Ok(Dropped::Sample { device: *device, name: file_stem(path) })
        }
        DropTarget::Pad { device, note } => {
            let notes: Vec<u8> = (*note..=*drumrack::PAD_NOTES.end()).take(sounds.len()).collect();
            set_pads(runtime, *device, sounds.iter().map(|(path, _)| path.as_path()).zip(notes).collect())
        }
        DropTarget::Rack { device } => {
            let mut taken: Vec<u8> = {
                let session = runtime.session.lock();
                let state = session.project().device(*device).map_err(|error| error.to_string())?.state.clone();
                let state = drumrack::State::from_device_state(state.as_deref()).unwrap_or_default();
                state.pads.iter().map(|pad| pad.note).collect()
            };
            let mut chosen = Vec::new();
            for (path, facts) in sounds {
                let by_name =
                    pad_for(facts.tags, &file_stem(path)).filter(|note| !chosen.iter().any(|(_, n)| n == note));
                let free = || {
                    let mut pads = drumrack::PAD_NOTES;
                    pads.find(|note| !taken.contains(note))
                };
                let Some(note) = by_name.or_else(free) else {
                    return Err(format!(
                        "Every pad of the rack is in use. Drop {} on the pad it should replace.",
                        file_name(path)
                    ));
                };
                taken.push(note);
                chosen.push((path.as_path(), note));
            }
            set_pads(runtime, *device, chosen)
        }
        DropTarget::Instrument { .. } | DropTarget::TrackInstrument { .. } | DropTarget::NewInstrument { .. } => {
            let (path, _) = sounds[0];
            Err(format!(
                "{} is a sound, not an instrument: a Sampled Instrument plays an instrument's file (.sfz).",
                file_name(path)
            ))
        }
    }
}

/// Puts each file on its pad.
fn set_pads(runtime: &Runtime, device: DeviceId, pads: Vec<(&Path, u8)>) -> Result<Dropped, String> {
    let mut notes = Vec::new();
    for (path, note) in pads {
        let mut session = runtime.session.lock();
        session.set_pad_file(device, note, Some(&stored_path(path))).map_err(|error| error.to_string())?;
        drop(session);
        runtime.library.mark_used(path);
        notes.push(note);
    }
    Ok(Dropped::Pads { device, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loops_with_a_tempo_follow_the_song_and_hits_do_not() {
        let drum_loop = Tags::of(&[Tag::Loop, Tag::Drum]);
        let pad_loop = Tags::of(&[Tag::Loop, Tag::Pad]);
        let kick = Tags::of(&[Tag::Kick, Tag::OneShot]);
        assert_eq!(warp_for(drum_loop, Some(140.0)), WarpMode::Beats);
        assert_eq!(warp_for(pad_loop, Some(90.0)), WarpMode::Tonal);
        assert_eq!(warp_for(pad_loop, None), WarpMode::Off, "nothing to follow the tempo with");
        assert_eq!(warp_for(kick, Some(140.0)), WarpMode::Off);
        assert_eq!(warp_for(Tags::EMPTY, Some(120.0)), WarpMode::Off);

        // A two-second loop recorded at 120 is four beats of any song; a
        // two-second hit is as many beats as the song fits into two seconds.
        assert_eq!(dropped_length(2.0, drum_loop, Some(120.0), 90.0), 4.0);
        assert_eq!(dropped_length(2.0, kick, None, 90.0), 3.0);
        assert_eq!(dropped_length(2.0, kick, Some(120.0), 90.0), 3.0);
    }

    #[test]
    fn sounds_find_the_pad_their_name_asks_for() {
        let one = |tag| Tags::of(&[tag, Tag::OneShot]);
        assert_eq!(pad_for(one(Tag::Kick), "Kick Deep 01"), Some(36));
        assert_eq!(pad_for(one(Tag::Snare), "SNR tight"), Some(38));
        assert_eq!(pad_for(one(Tag::Clap), "clap"), Some(39));
        assert_eq!(pad_for(one(Tag::Hat), "HH Closed"), Some(42));
        assert_eq!(pad_for(one(Tag::Hat), "Hat Open Long"), Some(46));
        assert_eq!(pad_for(one(Tag::Tom), "Floor Tom"), Some(41));
        assert_eq!(pad_for(one(Tag::Tom), "Tom High"), Some(48));
        assert_eq!(pad_for(one(Tag::Tom), "Tom 2"), Some(45));
        assert_eq!(pad_for(one(Tag::Cymbal), "Ride Bell"), Some(51));
        assert_eq!(pad_for(one(Tag::Cymbal), "Crash"), Some(49));
        assert_eq!(pad_for(one(Tag::Perc), "Shaker"), Some(37));
        assert_eq!(pad_for(one(Tag::Bass), "Sub Bass C"), None, "not a drum: any free pad will do");
        for note in [36, 37, 38, 39, 41, 42, 45, 46, 48, 49, 51] {
            assert!(drumrack::PAD_NOTES.contains(&note));
        }
    }

    #[test]
    fn names_are_shown_with_and_without_their_extension() {
        assert_eq!(file_name(Path::new("C:/Samples/Kit/Kick 01.wav")), "Kick 01.wav");
        assert_eq!(file_stem(Path::new("C:/Samples/Kit/Kick 01.wav")), "Kick 01");
        assert_eq!(file_stem(Path::new("mix v1.2.flac")), "mix v1.2");
    }
}
