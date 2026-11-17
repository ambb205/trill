//! A film of the song, from the window: File, Export a film.
//!
//! The form asks what the song does not say of its film: what it is of
//! (the song as it plays, or how it was made), its size and rate, how much
//! work goes into a frame, and which stretch. Where it goes is asked by
//! the system's own dialog once the form is answered.
//!
//! The film is made on a helper thread from a copy of the song
//! ([`trill_stage::export`]), on a graphics device of its own, so the
//! window goes on being worked in. How far it is shows in the status bar
//! and in this panel, which is also where it is stopped. Whatever ends it
//! ends in a sentence: there is no film without a graphics card, and
//! without `ffmpeg` it is numbered pictures with the sound beside them,
//! which the form says before any work is done.

use super::state::{Cx, UiState};
use super::toast::ToastKind;
use eframe::egui::{self, Align, Align2, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};
use trill_core::{Beats, Project};
use trill_engine::SharedSession;
use trill_noir::{TextStyle, Theme, Variant, paint, text};
use trill_stage::Format;
use trill_stage::export::{Exported, FilmRequest, SongFilm};
use trill_stage::film::{Phase, Progress, Shoot};
use trill_stage::making::{MakingFilm, MakingRequest};

/// Width of the panel.
const WIDTH: f32 = 540.0;
/// Width of the column of labels down its left.
const LABEL: f32 = 78.0;
/// Space between two rows of the form.
const ROW_GAP: f32 = 9.0;
/// How often the window looks at a film that is being made.
const WATCH: Duration = Duration::from_millis(200);

/// The sizes a film is offered in: the two a screen has, and one for a
/// phone held upright.
const SIZES: [(&str, Format); 3] = [
    ("1920 \u{00D7} 1080", Format::HD),
    ("3840 \u{00D7} 2160", Format::UHD),
    ("1080 \u{00D7} 1920 upright", Format::UPRIGHT),
];
/// The rates, in frames a second.
const RATES: [u32; 3] = [60, 30, 24];

/// What a film is of.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Of {
    /// The song as it plays, filmed by the director.
    #[default]
    Song,
    /// How it was made: the journal played again, the finished song under it.
    Making,
}

/// Which stretch of the song is filmed.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Stretch {
    #[default]
    Whole,
    /// What is selected in the arrangement.
    Selection,
    /// A section, counted from the song's first.
    Section(usize),
}

/// What the form holds: the choices as they stand.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub of: Of,
    /// Which of [`SIZES`].
    pub size: usize,
    /// Which of [`RATES`].
    pub rate: usize,
    /// Every frame exposed from several moments and drawn larger than it
    /// is kept; otherwise as the window shows the stage.
    pub fine: bool,
    pub stretch: Stretch,
}

impl Default for Draft {
    fn default() -> Self {
        Self { of: Of::Song, size: 0, rate: 0, fine: true, stretch: Stretch::Whole }
    }
}

impl Draft {
    /// How the film is shot.
    pub fn shoot(&self) -> Shoot {
        let format = SIZES[self.size.min(SIZES.len() - 1)].1;
        let rate = RATES[self.rate.min(RATES.len() - 1)];
        if self.fine { Shoot { rate, ..Shoot::fine(format) } } else { Shoot { rate, ..Shoot::plain(format) } }
    }
}

/// A section of the song: a marker's name, and the beats to the next one
/// (or to the song's end).
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub name: String,
    pub start: Beats,
    pub end: Beats,
}

/// The song's sections, in order: what its markers divide it into.
pub fn sections(project: &Project) -> Vec<Section> {
    let length = project.length_beats();
    let mut markers: Vec<(Beats, &str)> =
        project.markers.iter().map(|marker| (marker.beat, marker.name.as_str())).collect();
    markers.sort_by(|a, b| a.0.total_cmp(&b.0));
    let ends = markers.iter().skip(1).map(|next| next.0).chain([length]);
    let named = markers.iter().zip(ends).filter(|((start, _), end)| end > start);
    named.map(|((start, name), end)| Section { name: (*name).to_owned(), start: *start, end }).collect()
}

/// The beats a draft's stretch covers: from, and to (`None` for the song's
/// end). A stretch that is no longer there (the selection was dropped, the
/// markers moved) is the whole song.
fn stretch_of(draft: &Draft, project: &Project, selection: Option<(Beats, Beats)>) -> (Beats, Option<Beats>) {
    match draft.stretch {
        Stretch::Whole => (0.0, None),
        Stretch::Selection => match selection {
            Some((start, end)) if end > start => (start.max(0.0), Some(end)),
            _ => (0.0, None),
        },
        Stretch::Section(index) => match sections(project).get(index) {
            Some(section) => (section.start, Some(section.end)),
            None => (0.0, None),
        },
    }
}

/// A count with its thousands apart: 12,960.
fn counted(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Seconds as minutes and seconds: 3:07.
fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// A length of time as one would say it: "40 s", "6 min", "1 h 12 min".
fn spoken(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    match seconds {
        0..90 => format!("{seconds} s"),
        90..3600 => format!("{} min", (seconds + 30) / 60),
        _ => format!("{} h {} min", seconds / 3600, (seconds % 3600) / 60),
    }
}

/// What the form's choices come to, in one line: which bars, how long,
/// how many frames.
fn summary(draft: &Draft, project: &Project, selection: Option<(Beats, Beats)>, steps: usize) -> String {
    let rate = RATES[draft.rate.min(RATES.len() - 1)];
    if draft.of == Of::Making {
        let steps = if steps == 1 { "1 step".to_owned() } else { format!("{} steps", counted(steps as u64)) };
        return format!(
            "{steps} of the journal \u{00B7} the finished song plays under it \u{00B7} {rate} frames a second"
        );
    }
    let length = project.length_beats();
    if length <= 0.0 {
        return "The song is empty: there is nothing to film yet".to_owned();
    }
    let (from, to) = stretch_of(draft, project, selection);
    let to = to.unwrap_or(length).min(length).max(from);
    let bar = project.time_signature.beats_per_bar();
    let (first, last) =
        ((from / bar).floor() as u64 + 1, ((to / bar).ceil() as u64).max((from / bar).floor() as u64 + 1));
    let seconds = (to - from) * 60.0 / project.tempo.max(1.0);
    let frames = (seconds * f64::from(rate)).round() as u64;
    let bars = if first == last { format!("Bar {first}") } else { format!("Bars {first} to {last}") };
    format!("{bars} \u{00B7} {} \u{00B7} {} frames", clock(seconds), counted(frames))
}

/// What the panel shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum View {
    Closed,
    /// The choices.
    Form,
    /// A film being made: how far it is, and the way to stop it.
    Watching,
}

/// A film that is being made.
struct Running {
    path: PathBuf,
    progress: Arc<Progress>,
    started: Instant,
    /// When the first frame was begun, once it has been: frames are timed
    /// from there, not from the render before them.
    frames_from: Option<Instant>,
}

/// What a helper thread reports back.
enum Event {
    /// The system's dialog was dismissed without choosing a file.
    Cancelled,
    Started(PathBuf, Arc<Progress>),
    Finished(Result<Exported, String>),
    /// Whether there is an `ffmpeg` to make one file of the frames.
    Encoder(bool),
}

/// The film panel and the film being made, if one is.
pub struct Filming {
    view: View,
    draft: Draft,
    /// How many steps the song's journal had when the form was opened:
    /// with none there is no making-of to film.
    steps: usize,
    /// Whether `ffmpeg` was found when the form was opened; `None` until
    /// that is known.
    encoder: Option<bool>,
    running: Option<Running>,
    /// The system's dialog is up; a second one must not be opened over it.
    dialog_open: bool,
    sender: Sender<Event>,
    receiver: Receiver<Event>,
}

impl Default for Filming {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            view: View::Closed,
            draft: Draft::default(),
            steps: 0,
            encoder: None,
            running: None,
            dialog_open: false,
            sender,
            receiver,
        }
    }
}

impl Filming {
    /// True while the panel is on screen: the keyboard is then its own.
    pub fn is_open(&self) -> bool {
        self.view != View::Closed
    }

    /// The choices as they stand.
    #[cfg(test)]
    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    /// The film being made, if one is: its file, and how far it is in
    /// words short enough for the status bar.
    pub fn making(&self) -> Option<(&Path, String)> {
        let running = self.running.as_ref()?;
        let words = match (running.progress.phase(), running.progress.frames()) {
            (Phase::Frames, (done, total)) if total > 0 => {
                format!("{}%", (u64::from(done) * 100 / u64::from(total)).min(100))
            }
            (Phase::Done, _) => "finishing".to_owned(),
            _ => "rendering its sound".to_owned(),
        };
        Some((&running.path, words))
    }

    /// Takes in what the helper threads reported, and keeps the window
    /// looking while a film is made.
    pub fn poll(&mut self, ctx: &egui::Context, state: &mut UiState) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::Cancelled => self.dialog_open = false,
                Event::Encoder(found) => self.encoder = Some(found),
                Event::Started(path, progress) => {
                    self.dialog_open = false;
                    self.running = Some(Running { path, progress, started: Instant::now(), frames_from: None });
                    self.view = View::Watching;
                }
                Event::Finished(outcome) => {
                    let stopped = self.running.take().is_some_and(|running| running.progress.is_stopped());
                    self.dialog_open = false;
                    if self.view == View::Watching {
                        self.view = View::Closed;
                    }
                    match outcome {
                        Ok(exported) => {
                            // What should be known about it (no ffmpeg, a
                            // plugin that was not ready) is not good news.
                            let kind = if exported.notes.is_empty() { ToastKind::Success } else { ToastKind::Info };
                            state.toasts.lasting(kind, made(&exported), Duration::from_secs(16));
                        }
                        Err(sentence) if stopped => state.toasts.info(sentence),
                        Err(sentence) => state.toasts.error(format!("No film was made. {sentence}")),
                    }
                }
            }
        }
        if let Some(running) = &mut self.running {
            if running.frames_from.is_none() && running.progress.phase() == Phase::Frames {
                running.frames_from = Some(Instant::now());
            }
            ctx.request_repaint_after(WATCH);
        }
    }

    /// Runs `work` on a helper thread. `work` reports through the callback
    /// it is given, which also wakes the window.
    fn spawn(&self, ctx: &egui::Context, work: impl FnOnce(&dyn Fn(Event)) + Send + 'static) {
        let (sender, ctx) = (self.sender.clone(), ctx.clone());
        std::thread::spawn(move || {
            work(&|event| {
                // The receiver only goes away when the window has closed.
                let _ = sender.send(event);
                ctx.request_repaint();
            });
        });
    }
}

/// What came of a film, in a line: where it is, how long, how loud, how
/// long it took, and whatever should be known about it.
fn made(exported: &Exported) -> String {
    let name = exported.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let mut words = format!("Filmed {name}: {} long", clock(exported.seconds));
    match exported.loudness_lufs {
        // Below anything that could be heard: said as what it is.
        Some(loudness) if loudness <= -90.0 => words.push_str(", silent"),
        Some(loudness) => words.push_str(&format!(", {loudness:.1} LUFS")),
        None => {}
    }
    words.push_str(&format!(", in {}", spoken(exported.took)));
    for note in &exported.notes {
        words.push_str(". ");
        words.push_str(note.trim_end_matches('.'));
    }
    words
}

/// Puts the panel up: the form, or the film being made if one is.
pub fn open(ctx: &egui::Context, cx: &mut Cx) {
    open_on(ctx, cx, None);
}

/// [`open`], with what the film is of already chosen.
pub fn open_on(ctx: &egui::Context, cx: &mut Cx, of: Option<Of>) {
    if cx.filming.running.is_some() {
        cx.filming.view = View::Watching;
        return;
    }
    // Asked once, as the panel opens: not while it is drawn.
    let steps = cx.with_session(|session| session.journal().map_or(0, |journal| journal.steps()));
    let filming = &mut *cx.filming;
    filming.steps = steps;
    if let Some(of) = of {
        filming.draft.of = of;
    }
    if steps == 0 {
        filming.draft.of = Of::Song;
    }
    filming.view = View::Form;
    // Whether there is an ffmpeg takes starting a program to find out.
    filming.encoder = None;
    filming.spawn(ctx, |report| report(Event::Encoder(trill_stage::film::encoder_found())));
}

/// Takes the panel down. A film being made goes on.
pub fn close(cx: &mut Cx) {
    cx.filming.view = View::Closed;
}

/// Asks where the film goes, then makes it.
fn choose(ctx: &egui::Context, cx: &mut Cx) {
    if cx.filming.running.is_some() || std::mem::replace(&mut cx.filming.dialog_open, true) {
        return;
    }
    let stem = super::files::file_stem_for(&cx.snap.project.name);
    let name = match cx.filming.draft.of {
        Of::Song => format!("{stem}.mp4"),
        Of::Making => format!("{stem} making of.mp4"),
    };
    let mut dialog =
        rfd::AsyncFileDialog::new().set_title("Export a film").add_filter("MP4 video", &["mp4"]).set_file_name(name);
    if let Some(folder) = cx.snap.path.as_deref().and_then(Path::parent) {
        dialog = dialog.set_directory(folder);
    }
    let job = job(cx);
    cx.filming.spawn(ctx, move |report| match pollster::block_on(dialog.save_file()) {
        Some(file) => job.run(super::files::with_extension(file.path(), "mp4"), report),
        None => report(Event::Cancelled),
    });
}

/// Makes the film the form describes at `path`, without asking where: what
/// the system's dialog leads to once it is answered.
#[cfg(test)]
pub fn export_to(ctx: &egui::Context, cx: &mut Cx, path: PathBuf) {
    if cx.filming.running.is_some() {
        return;
    }
    let job = job(cx);
    cx.filming.spawn(ctx, move |report| job.run(path, report));
}

/// What a helper thread needs to make the film: the choices, and the
/// session to copy the song from.
struct Job {
    session: SharedSession,
    draft: Draft,
    /// The stretch, resolved while the window's state was to hand.
    stretch: (Beats, Option<Beats>),
}

fn job(cx: &Cx) -> Job {
    let selection = cx.state.arrangement.time_selection.as_ref().map(|selection| (selection.start, selection.end));
    let draft = cx.filming.draft;
    Job { session: cx.session.clone(), draft, stretch: stretch_of(&draft, &cx.snap.project, selection) }
}

impl Job {
    /// Copies the song out of the session and films it. Runs on a helper
    /// thread and is as long as the film takes.
    fn run(self, path: PathBuf, report: &dyn Fn(Event)) {
        let shoot = self.draft.shoot();
        // The job is a copy of the song, so the session is free again
        // before the long part starts and editing can go on meanwhile.
        let (render, drums, sample_rate, journal) = {
            let mut session = self.session.lock();
            let tracks = session.project().tracks.iter().map(|track| track.id);
            let drums: Vec<_> = tracks.filter(|track| session.instrument_plays_drums(*track)).collect();
            let journal = session.journal().filter(|journal| journal.steps() > 0).cloned();
            let sample_rate = session.audio_info().sample_rate.round() as u32;
            (session.render_job(), drums, sample_rate, journal)
        };
        let progress = Arc::new(Progress::default());
        report(Event::Started(path.clone(), progress.clone()));
        let outcome = match (self.draft.of, journal) {
            (Of::Song, _) if render.project.length_beats() <= 0.0 => {
                Err("The song is empty: there is nothing to film yet.".to_owned())
            }
            (Of::Song, _) => {
                let (from, to) = self.stretch;
                let request = FilmRequest { from, to, sample_rate, ..FilmRequest::new(path, shoot) };
                SongFilm { render, drums, request }.run(&progress)
            }
            (Of::Making, Some(journal)) => {
                let request = MakingRequest { sample_rate, ..MakingRequest::new(path, shoot) };
                MakingFilm { journal, render, drums, request }.run(&progress)
            }
            (Of::Making, None) => Err("No journal with steps in it is kept for this song, so there is no making of \
                                       it to film."
                .to_owned()),
        };
        report(Event::Finished(outcome));
    }
}

/// Shows the panel while it is open.
pub fn modal(ctx: &egui::Context, cx: &mut Cx) {
    match cx.filming.view {
        View::Closed => {}
        View::Form => form(ctx, cx),
        // A film that ended while the panel was not looked at.
        View::Watching if cx.filming.running.is_none() => cx.filming.view = View::Closed,
        View::Watching => watching(ctx, cx),
    }
}

/// The panel's frame, its title over a rule, and `body` under it. Returns
/// whether it was dismissed (Escape, or a click beside it).
fn panel(ctx: &egui::Context, theme: &Theme, title: &str, body: impl FnOnce(&mut Ui)) -> bool {
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(14, 12));
    let modal =
        egui::Modal::new(egui::Id::unique("film")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
            let (heading, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
            text::paint(ui.painter(), heading, Align2::LEFT_CENTER, theme.text.caps, title, theme.ink_dim);
            ui.add_space(6.0);
            rule(ui, theme);
            ui.add_space(12.0);
            body(ui);
        });
    modal.should_close()
}

fn rule(ui: &mut Ui, theme: &Theme) {
    let (rule, _) = ui.allocate_exact_size(vec2(WIDTH, 1.0), Sense::hover());
    paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
}

/// A line of words that is also there for whoever cannot see paint.
fn line(ui: &mut Ui, theme: &Theme, color: egui::Color32, words: &str) {
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    text::paint(ui.painter(), rect, Align2::LEFT_CENTER, style, words, color);
}

/// A row of the form: its label, and one chip per choice with the chosen
/// one lit. Returns the choice that was clicked, if one was.
fn choice(ui: &mut Ui, theme: &Theme, label: &str, choices: &[&str], chosen: Option<usize>) -> Option<usize> {
    let origin = ui.cursor().min;
    let chips = pos2(origin.x + LABEL, origin.y);
    let id = ui.scope_id().with(("film", label));
    let (clicked, height) = trill_noir::chip_row(ui, theme, chips, WIDTH - LABEL, id, choices, chosen, false);
    let (row, _) = ui.allocate_exact_size(vec2(WIDTH, height), Sense::hover());
    let name = Rect::from_min_size(row.min, vec2(LABEL, theme.metric.chip));
    text::paint(ui.painter(), name, Align2::LEFT_CENTER, theme.text.micro, label, theme.ink_faint);
    ui.add_space(ROW_GAP);
    clicked
}

/// A line of small words under a row, in the chips' column.
fn aside(ui: &mut Ui, theme: &Theme, words: &str) {
    let (row, _) = ui.allocate_exact_size(vec2(WIDTH, 14.0), Sense::hover());
    let room = row.with_min_x(row.left() + LABEL);
    // A sentence, so set as one: the small caps are for labels.
    let style = TextStyle { size: 11.5, ..theme.text.body };
    text::paint(ui.painter(), room, Align2::LEFT_CENTER, style, words, theme.ink_dim);
    ui.add_space(ROW_GAP);
}

/// The choices, and the two buttons under them.
fn form(ctx: &egui::Context, cx: &mut Cx) {
    let theme = cx.theme;
    let project = &cx.snap.project;
    let selection = cx.state.arrangement.time_selection.as_ref().map(|selection| (selection.start, selection.end));
    let selection = selection.filter(|(start, end)| end > start);
    let named = sections(project);
    let (steps, encoder) = (cx.filming.steps, cx.filming.encoder);
    let mut draft = cx.filming.draft;
    // A stretch that is no longer there is the whole song.
    let gone = match draft.stretch {
        Stretch::Whole => false,
        Stretch::Selection => selection.is_none(),
        Stretch::Section(index) => index >= named.len(),
    };
    if gone {
        draft.stretch = Stretch::Whole;
    }
    let (mut go, mut leave) = (false, false);
    let dismissed = panel(ctx, theme, "Export a film", |ui| {
        // What it is of. How it was made is offered when a journal says how.
        let mut of = vec!["The song"];
        if steps > 0 {
            of.push("How it was made");
        }
        if let Some(index) = choice(ui, theme, "Film", &of, Some(usize::from(draft.of == Of::Making))) {
            draft.of = if index == 1 { Of::Making } else { Of::Song };
        }
        let sizes: Vec<&str> = SIZES.iter().map(|(name, _)| *name).collect();
        if let Some(index) = choice(ui, theme, "Size", &sizes, Some(draft.size)) {
            draft.size = index;
        }
        let rates: Vec<String> = RATES.iter().map(|rate| format!("{rate} a second")).collect();
        let rates: Vec<&str> = rates.iter().map(String::as_str).collect();
        if let Some(index) = choice(ui, theme, "Rate", &rates, Some(draft.rate)) {
            draft.rate = index;
        }
        if let Some(index) = choice(ui, theme, "Frames", &["Fine", "Plain"], Some(usize::from(!draft.fine))) {
            draft.fine = index == 0;
        }
        ui.add_space(-ROW_GAP + 3.0);
        // What the choice costs, as this size is shot: the largest is not drawn larger still.
        let about = match (draft.fine, draft.shoot().oversample) {
            (true, 2..) => "Eight moments a frame, drawn at twice its size: slower to make than the song plays",
            (true, _) => "Eight moments a frame: slower to make than the song plays",
            (false, _) => "One moment a frame, as the window shows the stage: about twice as fast to make",
        };
        aside(ui, theme, about);
        if draft.of == Of::Song {
            // The stretch: all of it, what is selected, or a section by its marker.
            let mut stretches = vec![("Whole song".to_owned(), Stretch::Whole)];
            if selection.is_some() {
                stretches.push(("Selection".to_owned(), Stretch::Selection));
            }
            stretches.extend(named.iter().enumerate().map(|(index, section)| {
                let name = if section.name.trim().is_empty() {
                    format!("Section {}", index + 1)
                } else {
                    section.name.clone()
                };
                (name, Stretch::Section(index))
            }));
            let names: Vec<&str> = stretches.iter().map(|(name, _)| name.as_str()).collect();
            let chosen = stretches.iter().position(|(_, stretch)| *stretch == draft.stretch);
            if let Some(index) = choice(ui, theme, "Stretch", &names, chosen) {
                draft.stretch = stretches[index].1;
            }
        }
        ui.add_space(3.0);
        rule(ui, theme);
        ui.add_space(9.0);
        line(ui, theme, theme.ink, &summary(&draft, project, selection, steps));
        match encoder {
            Some(false) => line(
                ui,
                theme,
                theme.warn,
                "ffmpeg was not found: the film will be numbered pictures with its sound beside them",
            ),
            Some(true) => line(ui, theme, theme.ink_faint, "Its own sound is rendered first, plugins waited for"),
            None => line(ui, theme, theme.ink_faint, "Looking for ffmpeg\u{2026}"),
        }
        ui.add_space(12.0);
        let (foot, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
        let layout = Layout::right_to_left(Align::Center);
        let mut cell = ui.new_child(UiBuilder::new().id_salt("film-buttons").max_rect(foot).layout(layout));
        cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
        // Nothing to film, nothing to choose a place for.
        let empty = draft.of == Of::Song && project.length_beats() <= 0.0;
        let choose = |ui: &mut Ui| trill_noir::button(ui, theme, "Choose where\u{2026}", Variant::Primary).clicked();
        go = cell.add_enabled_ui(!empty, choose).inner;
        leave = trill_noir::button(&mut cell, theme, "Cancel", Variant::Default).clicked();
    });
    cx.filming.draft = draft;
    if go {
        choose(ctx, cx);
    } else if leave || dismissed {
        close(cx);
    }
}

/// How far the film is, and the way to stop it.
fn watching(ctx: &egui::Context, cx: &mut Cx) {
    let theme = cx.theme;
    let Some(running) = &cx.filming.running else { return };
    let name = running.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let (done, total) = running.progress.frames();
    let stopping = running.progress.is_stopped();
    let elapsed = running.started.elapsed().as_secs_f64();
    let (what, fraction, how_far) = match running.progress.phase() {
        Phase::Frames if total > 0 => {
            let fraction = done as f32 / total as f32;
            let mut words = format!("Frame {} of {}", counted(u64::from(done)), counted(u64::from(total)));
            // What is left, from how fast the frames have come so far.
            let spent = running.frames_from.map_or(0.0, |from| from.elapsed().as_secs_f64());
            if done >= 30 && spent > 2.0 {
                let left = spent / f64::from(done) * f64::from(total - done.min(total));
                words.push_str(&format!(" \u{00B7} about {} left", spoken(left)));
            }
            ("Drawing the frames", fraction, words)
        }
        Phase::Done => ("Finishing", 1.0, "Putting the picture and the sound together".to_owned()),
        _ => (
            "Rendering the song\u{2019}s own sound",
            0.0,
            format!("Plugins are waited for \u{00B7} {} so far", spoken(elapsed)),
        ),
    };
    let what = if stopping { "Stopping after this frame" } else { what };
    let (mut stop, mut hide) = (false, false);
    let dismissed = panel(ctx, theme, &format!("Filming {name}"), |ui| {
        line(ui, theme, theme.ink, what);
        ui.add_space(6.0);
        let (track, response) = ui.allocate_exact_size(vec2(WIDTH, 6.0), Sense::hover());
        let percent = format!("{}% of the film is made", (fraction * 100.0).round());
        response.widget_info(|| WidgetInfo::labeled(Role::ProgressIndicator, true, &percent));
        paint::progress(ui.painter(), track, fraction, theme.well, theme.signal);
        ui.add_space(6.0);
        line(ui, theme, theme.ink_dim, &how_far);
        ui.add_space(12.0);
        let (foot, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
        let note = "It goes on while you work \u{00B7} File \u{203A} Export a film shows this again";
        text::paint(ui.painter(), foot, Align2::LEFT_CENTER, theme.text.micro, note, theme.ink_faint);
        let layout = Layout::right_to_left(Align::Center);
        let mut cell = ui.new_child(UiBuilder::new().id_salt("film-watching").max_rect(foot).layout(layout));
        cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
        hide = trill_noir::button(&mut cell, theme, "Hide", Variant::Default).clicked();
        if stopping {
            cell.disable();
        }
        stop = trill_noir::button(&mut cell, theme, "Stop the film", Variant::Danger).clicked();
    });
    if stop {
        running.progress.stop();
    }
    if hide || dismissed {
        close(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sixteen bars at 120: a verse and a chorus of eight bars each.
    fn song() -> Project {
        let mut project = Project::new("Filmed");
        let track = project.add_track("Bass");
        project.add_midi_clip(track, "bass", 0.0, 64.0).unwrap();
        project.add_marker(32.0, "Chorus").unwrap();
        project.add_marker(0.0, "Verse").unwrap();
        project
    }

    #[test]
    fn the_songs_markers_divide_it_into_the_sections_a_film_may_be_of() {
        let project = song();
        let named: Vec<(String, f64, f64)> =
            sections(&project).into_iter().map(|section| (section.name, section.start, section.end)).collect();
        assert_eq!(named, [("Verse".to_owned(), 0.0, 32.0), ("Chorus".to_owned(), 32.0, 64.0)]);
        // A marker past the song's end names nothing that could be filmed.
        let mut project = project;
        project.add_marker(96.0, "Later").unwrap();
        assert_eq!(sections(&project).len(), 2);
        assert!(sections(&Project::new("Empty")).is_empty());
    }

    #[test]
    fn a_stretch_is_the_whole_song_the_selection_or_a_section_and_the_whole_when_it_is_gone() {
        let project = song();
        let draft = |stretch| Draft { stretch, ..Draft::default() };
        assert_eq!(stretch_of(&draft(Stretch::Whole), &project, Some((4.0, 8.0))), (0.0, None));
        assert_eq!(stretch_of(&draft(Stretch::Selection), &project, Some((4.0, 8.0))), (4.0, Some(8.0)));
        assert_eq!(stretch_of(&draft(Stretch::Section(1)), &project, None), (32.0, Some(64.0)));
        assert_eq!(stretch_of(&draft(Stretch::Selection), &project, None), (0.0, None));
        assert_eq!(stretch_of(&draft(Stretch::Section(7)), &project, None), (0.0, None));
    }

    #[test]
    fn the_form_says_what_its_choices_come_to() {
        let project = song();
        let whole = Draft::default();
        assert_eq!(summary(&whole, &project, None, 0), "Bars 1 to 16 \u{00B7} 0:32 \u{00B7} 1,920 frames");
        let chorus = Draft { stretch: Stretch::Section(1), rate: 2, ..whole };
        assert_eq!(summary(&chorus, &project, None, 0), "Bars 9 to 16 \u{00B7} 0:16 \u{00B7} 384 frames");
        let bar = Draft { stretch: Stretch::Selection, ..whole };
        assert_eq!(summary(&bar, &project, Some((8.0, 12.0)), 0), "Bar 3 \u{00B7} 0:02 \u{00B7} 120 frames");
        let making = Draft { of: Of::Making, ..whole };
        assert!(summary(&making, &project, None, 1204).starts_with("1,204 steps of the journal"));
        assert!(summary(&whole, &Project::new("Empty"), None, 0).contains("nothing to film"));
    }

    #[test]
    fn a_draft_is_shot_as_it_says() {
        let fine = Draft::default().shoot();
        assert_eq!(fine, Shoot::fine(Format::HD));
        let upright = Draft { size: 2, rate: 1, fine: false, ..Draft::default() }.shoot();
        assert_eq!(upright, Shoot { rate: 30, ..Shoot::plain(Format::UPRIGHT) });
    }

    #[test]
    fn numbers_and_times_read_the_way_one_would_say_them() {
        assert_eq!(
            (counted(7), counted(1_000), counted(12_960), counted(1_234_567)),
            ("7".into(), "1,000".into(), "12,960".into(), "1,234,567".into())
        );
        assert_eq!((clock(0.0), clock(187.4), clock(3600.0)), ("0:00".into(), "3:07".into(), "60:00".into()));
        assert_eq!((spoken(40.0), spoken(372.0), spoken(4320.0)), ("40 s".into(), "6 min".into(), "1 h 12 min".into()));
    }

    #[test]
    fn what_came_of_a_film_is_one_line_with_what_should_be_known() {
        let mut exported = Exported {
            path: PathBuf::from("films/night shift.mp4"),
            frames: 11_520,
            seconds: 192.0,
            took: 372.0,
            notes: Vec::new(),
            loudness_lufs: Some(-14.23),
            peak_db: Some(-1.0),
        };
        assert_eq!(made(&exported), "Filmed night shift.mp4: 3:12 long, -14.2 LUFS, in 6 min");
        let silent = Exported { loudness_lufs: Some(-120.0), ..exported.clone() };
        assert_eq!(made(&silent), "Filmed night shift.mp4: 3:12 long, silent, in 6 min");
        exported.notes.push("ffmpeg was not found, so the film was written as numbered pictures.".to_owned());
        assert!(
            made(&exported).ends_with("in 6 min. ffmpeg was not found, so the film was written as numbered pictures")
        );
    }

    #[test]
    fn a_film_that_ends_says_so_once_and_frees_the_panel() {
        let ctx = egui::Context::default();
        let mut filming = Filming::default();
        let mut state = UiState::new(Default::default());
        let progress = Arc::new(Progress::default());
        filming.dialog_open = true;
        filming.sender.send(Event::Started(PathBuf::from("out.mp4"), progress.clone())).unwrap();
        filming.poll(&ctx, &mut state);
        assert!(filming.is_open() && !filming.dialog_open);
        assert_eq!(
            filming.making().map(|(path, words)| (path.to_path_buf(), words)),
            Some((PathBuf::from("out.mp4"), "rendering its sound".to_owned()))
        );

        // Stopped by the user: said as news, not as a failure.
        progress.stop();
        let stopped = "The film was stopped at frame 12 of 400; nothing of it was kept.";
        filming.sender.send(Event::Finished(Err(stopped.to_owned()))).unwrap();
        filming.poll(&ctx, &mut state);
        assert!(!filming.is_open() && filming.making().is_none());
        assert_eq!(state.toasts.texts(), [stopped]);

        // One that failed says that no film was made, and why.
        filming.sender.send(Event::Started(PathBuf::from("out.mp4"), Arc::new(Progress::default()))).unwrap();
        filming.sender.send(Event::Finished(Err("No graphics card was found.".to_owned()))).unwrap();
        filming.poll(&ctx, &mut state);
        assert_eq!(state.toasts.texts()[1], "No film was made. No graphics card was found.");
        assert!(!filming.is_open());
    }
}
