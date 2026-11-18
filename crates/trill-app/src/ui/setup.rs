//! The Setup panel: what Trill does not ship and fetches when asked, and
//! what runs the models on this machine.
//!
//! One tab for each list of things to fetch (the parts a study of a
//! recording is made with; whatever another part of the program has added
//! to the same engine), and one for the machine: the timed test of what
//! runs the models, the programs Trill found installed, and where studies
//! are kept.
//!
//! Three rules shape it.
//!
//! **Nothing is fetched that was not read first.** A part is fetched by a
//! button that stands under its size, its licence in the words of its
//! source, who asks to be credited and where it comes from. There is no
//! "fetch everything", and nothing here fetches by itself.
//!
//! **Nothing here waits on a disk, a network or a model.** What the panel
//! shows is read by a helper thread a few times a second while it is open,
//! and everything it is asked to do (fetching, removing, the timed test,
//! moving a folder) is done on one; the panel says what it is waiting for.
//!
//! **Whatever happens reads as a sentence.** A part that could not be
//! fetched says why and what to do under its own name, for as long as it
//! is so; a card that could not run the models says so in its row.

use super::state::Cx;
use eframe::egui::{self, Align, Align2, Id, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use trill_fetch::{ListStatus, PartState, PartStatus, said};
use trill_noir::{Chip, TextStyle, Theme, Variant, paint, prose, text};
use trill_runtime::Runtime;
use trill_study::models::real::Provider;
use trill_study::setup::{Found, Machine, Timed, provider_name};

const WIDTH: f32 = 700.0;
/// Height of a part in a list, and of a row of the machine's.
const ROW: f32 = 24.0;
const GAP: f32 = 6.0;
/// More parts than this scroll.
const MAX_ROWS: usize = 10;
/// How often what is shown is read again while the panel is open, in
/// seconds: slower while nothing is on its way.
const READ_EVERY: f64 = 1.0;
const READ_EVERY_BUSY: f64 = 0.2;

/// Which folder a dialog was asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// Where studies are kept.
    Studies,
    /// Where the parts of this list are kept.
    List(String),
}

/// What a helper thread reports back.
enum Done {
    /// What there is to fetch and what runs the models, as it is now.
    Read(Box<Shown>),
    /// The programs that were found, and what the studies take.
    Looked(Vec<Found>, Option<(PathBuf, usize, u64)>),
    /// Something that was asked for is done, and what there is to say of it.
    Said(Result<String, String>),
    /// The timed test is at this, and so far through.
    Testing(String, f32),
    Tested(Result<(), String>),
    /// The dialog that asks for a folder was answered, or put away.
    Answered,
}

/// What the panel shows, as a helper thread last read it.
#[derive(Default)]
struct Shown {
    lists: Vec<ListStatus>,
    machine: Option<Machine>,
    /// Whether the timed test is to be run (again) on what is here.
    untested: bool,
    runs_on: String,
    /// Whether this program can start itself to time the models.
    can_test: bool,
    /// Why there is nothing to show, where nothing is kept.
    unavailable: Option<String>,
}

/// The panel and what it is waiting for.
pub struct Setup {
    open: bool,
    /// The tab: a list by its place, and after the last list the machine.
    tab: usize,
    shown: Shown,
    /// False until what is shown has been read once since it was opened.
    read: bool,
    reading: bool,
    read_at: f64,
    /// The part whose details are shown, for each list.
    selected: HashMap<String, String>,
    programs: Vec<Found>,
    studies: Option<(PathBuf, usize, u64)>,
    looking: bool,
    /// How many things that were asked for are being done.
    working: usize,
    /// The timed test, while it runs: what it is at, and how far through.
    testing: Option<(String, f32)>,
    stop_test: Arc<AtomicBool>,
    /// What the last thing that was asked for said, and whether it went wrong.
    last: Option<(String, bool)>,
    dialog_open: bool,
    /// Whether what is shown is a made-up machine, which is then not read over.
    staged: bool,
    /// Set when something was done that the programs or the studies may show.
    stale: bool,
    sender: Sender<Done>,
    receiver: Receiver<Done>,
}

impl Default for Setup {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Self {
            open: false,
            tab: 0,
            shown: Shown::default(),
            read: false,
            reading: false,
            read_at: f64::NEG_INFINITY,
            selected: HashMap::new(),
            programs: Vec::new(),
            studies: None,
            looking: false,
            working: 0,
            testing: None,
            stop_test: Arc::default(),
            last: None,
            dialog_open: false,
            staged: false,
            stale: false,
            sender,
            receiver,
        }
    }
}

impl Setup {
    /// True while the panel is on screen: the keyboard is then its own.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// True while the panel shows something that is still on its way: what
    /// there is to fetch, the programs. A screenshot waits for it.
    pub fn is_waiting(&self) -> bool {
        self.open && (!self.read || self.looking)
    }

    /// True while something that was asked for is being done.
    #[cfg(test)]
    pub fn is_busy(&self) -> bool {
        self.reading || self.looking || self.working > 0 || self.testing.is_some()
    }

    /// What the last thing that was asked for said.
    #[cfg(test)]
    pub fn last(&self) -> Option<&str> {
        self.last.as_ref().map(|(words, _)| words.as_str())
    }

    /// Shows a made-up machine in place of this one, for pictures.
    pub fn show(&mut self, scene: super::setup_scene::SetupScene) {
        let staged = scene.staged();
        self.tab = if staged.on_machine { staged.lists.len() } else { 0 };
        for list in &staged.lists {
            self.selected.insert(list.id.clone(), staged.selected.to_owned());
        }
        self.shown = Shown {
            lists: staged.lists,
            machine: staged.machine,
            untested: staged.untested,
            runs_on: staged.runs_on,
            can_test: true,
            unavailable: None,
        };
        (self.programs, self.studies) = (staged.programs, Some(staged.studies));
        (self.staged, self.read) = (true, true);
    }

    /// Takes in what the helper threads have finished.
    pub fn poll(&mut self) {
        while let Ok(done) = self.receiver.try_recv() {
            match done {
                Done::Read(shown) => {
                    if !self.staged {
                        self.shown = *shown;
                    }
                    (self.read, self.reading) = (true, false);
                }
                Done::Looked(programs, studies) => {
                    (self.programs, self.studies, self.looking) = (programs, studies, false);
                }
                Done::Said(said) => {
                    self.working = self.working.saturating_sub(1);
                    self.last = match said {
                        Ok(words) if words.is_empty() => None,
                        Ok(words) => Some((words, false)),
                        Err(why) => Some((sentence(&why), true)),
                    };
                    // What was done shows at once, not at the next look.
                    self.read_at = f64::NEG_INFINITY;
                    self.stale = true;
                }
                Done::Testing(what, share) => self.testing = Some((what, share)),
                Done::Tested(ended) => {
                    self.testing = None;
                    self.last = ended.err().map(|why| (sentence(&why), true));
                    self.read_at = f64::NEG_INFINITY;
                }
                Done::Answered => self.dialog_open = false,
            }
        }
    }

    /// Runs `job` on a thread of its own and has the window drawn again
    /// whenever it reports.
    fn spawn(
        &self,
        ctx: &egui::Context,
        runtime: &Arc<Runtime>,
        job: impl FnOnce(&Runtime, &dyn Fn(Done)) + Send + 'static,
    ) {
        let (sender, ctx, runtime) = (self.sender.clone(), ctx.clone(), runtime.clone());
        let fallback = self.sender.clone();
        let spawned = std::thread::Builder::new().name("trill-setup".into()).spawn(move || {
            job(&runtime, &|done| {
                // The receiver is gone only when the window has closed.
                let _ = sender.send(done);
                ctx.request_repaint();
            });
        });
        if let Err(error) = spawned {
            let _ = fallback.send(Done::Said(Err(format!("a helper thread could not be started: {error}"))));
        }
    }
}

/// A sentence with a capital and a full stop.
fn sentence(words: &str) -> String {
    let words = words.trim();
    let mut letters = words.chars();
    let Some(first) = letters.next() else { return String::new() };
    let mut sentence: String = first.to_uppercase().chain(letters).collect();
    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

/// A sentence that begins with a name that is spelled as it is (a file's):
/// as it was said, with a full stop.
fn stopped(words: &str) -> String {
    let words = words.trim();
    if words.ends_with(['.', '!', '?']) { words.to_owned() } else { format!("{words}.") }
}

/// Reads what the panel shows.
fn read(runtime: &Runtime) -> Shown {
    let (fetches, desk) = match (runtime.fetches(), runtime.studies()) {
        (Ok(fetches), Ok(desk)) => (fetches, desk),
        (Err(why), _) | (_, Err(why)) => return Shown { unavailable: Some(sentence(&why)), ..Shown::default() },
    };
    let setup = desk.setup();
    Shown {
        lists: fetches.status(),
        machine: setup.machine(),
        untested: setup.untested(),
        runs_on: provider_name(setup.provider()),
        can_test: desk.program().is_some(),
        unavailable: None,
    }
}

/// Opens the panel, and starts reading what it shows.
pub fn open(ctx: &egui::Context, cx: &mut Cx) {
    let setup = &mut *cx.setup;
    (setup.open, setup.read, setup.last) = (true, setup.staged, None);
    setup.read_at = f64::NEG_INFINITY;
    look(ctx, cx);
}

pub fn toggle(ctx: &egui::Context, cx: &mut Cx) {
    if cx.setup.open { close(cx) } else { open(ctx, cx) }
}

/// Puts the panel away. What is being fetched goes on; the timed test too.
pub fn close(cx: &mut Cx) {
    cx.setup.open = false;
}

/// Looks for the programs Trill uses, and at what the studies take.
fn look(ctx: &egui::Context, cx: &mut Cx) {
    if cx.setup.staged || std::mem::replace(&mut cx.setup.looking, true) {
        return;
    }
    cx.setup.spawn(ctx, &cx.sounds.runtime, |runtime, report| {
        let Ok(desk) = runtime.studies() else { return report(Done::Looked(Vec::new(), None)) };
        let studies = desk.list();
        let size = studies.iter().map(|report| report.size).sum();
        report(Done::Looked(desk.setup().programs(), Some((desk.folder(), studies.len(), size))));
    });
}

/// Does something that ends in a sentence, on a thread of its own.
fn act(ctx: &egui::Context, cx: &mut Cx, job: impl FnOnce(&Runtime) -> Result<String, String> + Send + 'static) {
    cx.setup.working += 1;
    cx.setup.last = None;
    cx.setup.spawn(ctx, &cx.sounds.runtime, |runtime, report| report(Done::Said(job(runtime))));
}

fn fetch(ctx: &egui::Context, cx: &mut Cx, part: String) {
    act(ctx, cx, move |runtime| runtime.fetches()?.fetch(&part).map(|_| String::new()));
}

fn stop(ctx: &egui::Context, cx: &mut Cx, part: String) {
    act(ctx, cx, move |runtime| {
        let part = runtime.fetches()?.stop(&part)?;
        Ok(format!("What came of {} is kept: fetching it again goes on from there.", part.name))
    });
}

fn remove(ctx: &egui::Context, cx: &mut Cx, part: String) {
    act(ctx, cx, move |runtime| {
        let fetches = runtime.fetches()?;
        let name = fetches.part(&part)?.name;
        Ok(format!("{name} was removed: {} of room made.", said(fetches.remove(&part)?)))
    });
}

fn choose(ctx: &egui::Context, cx: &mut Cx, provider: Provider) {
    act(ctx, cx, move |runtime| runtime.studies()?.setup().choose(provider).map(|_| String::new()));
}

/// Keeps studies, or the parts of a list, in `folder` from now on, and
/// says what was done.
fn kept_in(runtime: &Runtime, what: Kept, folder: PathBuf) -> Result<String, String> {
    match what {
        Kept::Studies => runtime.keep_studies_in(Some(folder)),
        Kept::List(list) => runtime.keep_list_in(&list, Some(folder)),
    }
}

/// Keeps studies, or the parts of a list, in `folder`: what choosing a
/// folder in the system's dialog leads to.
#[cfg(test)]
pub fn keep_in(ctx: &egui::Context, cx: &mut Cx, what: Kept, folder: PathBuf) {
    act(ctx, cx, move |runtime| kept_in(runtime, what, folder));
}

/// Asks where studies, or the parts of a list, are to be kept.
fn keep_in_dialog(ctx: &egui::Context, cx: &mut Cx, what: Kept, now: PathBuf) {
    if std::mem::replace(&mut cx.setup.dialog_open, true) {
        return;
    }
    let title = match &what {
        Kept::Studies => "Keep studies in this folder",
        Kept::List(_) => "Keep what is fetched in this folder",
    };
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
    if let Some(folder) = now.ancestors().find(|folder| folder.is_dir()) {
        dialog = dialog.set_directory(folder);
    }
    cx.setup.working += 1;
    cx.setup.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        let picked = pollster::block_on(dialog.pick_folder()).map(|folder| folder.path().to_path_buf());
        report(Done::Answered);
        report(Done::Said(match picked {
            None => Ok(String::new()),
            Some(folder) => kept_in(runtime, what, folder),
        }));
    });
}

/// Runs the timed test.
fn test(ctx: &egui::Context, cx: &mut Cx) {
    if cx.setup.testing.is_some() {
        return;
    }
    cx.setup.testing = Some(("Starting".to_owned(), 0.0));
    cx.setup.last = None;
    let stop = Arc::new(AtomicBool::new(false));
    cx.setup.stop_test = stop.clone();
    cx.setup.spawn(ctx, &cx.sounds.runtime, move |runtime, report| {
        let tested = (|| {
            let desk = runtime.studies()?;
            let program = desk.program().ok_or("this program cannot start itself to time the models")?;
            if desk.busy() {
                return Err(
                    "a study is running, and the test would be timed against it: run it when the study is done"
                        .to_owned(),
                );
            }
            desk.setup()
                .test(&program, &stop, &mut |what, share| report(Done::Testing(what.to_owned(), share)))
                .map(|_| ())
        })();
        report(Done::Tested(tested));
    });
}

// ---- what the panel says, as pure functions ------------------------------------

/// What a part is doing, in a word or two, and whether that is a problem.
fn state_words(status: &PartStatus) -> (String, bool) {
    let percent = || (status.got as f64 * 100.0 / status.part.size().max(1) as f64).floor();
    match (status.state, &status.error) {
        (PartState::Ready, _) => ("Here".to_owned(), false),
        (PartState::Waiting, _) => ("Waiting its turn".to_owned(), false),
        (PartState::Fetching, _) => (format!("Fetching \u{00B7} {} %", percent()), false),
        (PartState::Checking, _) => ("Checking".to_owned(), false),
        (PartState::Unpacking, _) => ("Unpacking".to_owned(), false),
        (PartState::Missing, Some(_)) => ("Could not be fetched".to_owned(), true),
        (PartState::Missing, None) if status.got > 0 => (format!("Stopped at {} %", percent()), false),
        (PartState::Missing, None) => ("Not here".to_owned(), false),
    }
}

/// How many of a list's parts are here, and the room they take.
fn list_words(list: &ListStatus) -> String {
    let here = list.parts.iter().filter(|status| status.state == PartState::Ready).count();
    let room: u64 = list.parts.iter().map(|status| status.on_disk).sum();
    format!("{here} of {} here \u{00B7} {} on disk", list.parts.len(), said(room))
}

/// What a part is checked against, in a line.
fn checked_by(status: &PartStatus) -> String {
    let short = |sum: &str| format!("{}\u{2026}", &sum[..sum.len().min(12)]);
    let sums: Vec<String> = status
        .part
        .files
        .iter()
        .map(|file| match &file.keep {
            trill_fetch::Keep::Tree { sum, .. } if file.sha256.is_empty() => short(sum),
            _ => short(&file.sha256),
        })
        .collect();
    match sums.as_slice() {
        [one] => format!("SHA-256 {one}"),
        sums if sums.len() <= 3 => format!("{} files, SHA-256 {}", sums.len(), sums.join(", ")),
        sums => format!("{} files, each by its SHA-256 ({}, {} and others)", sums.len(), sums[0], sums[1]),
    }
}

/// The parts `status` is no use without and that are not here, by name.
fn lacking(status: &PartStatus, lists: &[ListStatus]) -> Vec<String> {
    let all = || lists.iter().flat_map(|list| &list.parts);
    status
        .part
        .needs
        .iter()
        .filter(|needed| !all().any(|other| other.part.id == **needed && other.state == PartState::Ready))
        .map(|needed| {
            all().find(|other| other.part.id == *needed).map_or_else(|| needed.clone(), |other| other.part.name.clone())
        })
        .collect()
}

/// One way of running the models as the timed test found it: what a
/// minute of music takes, what else there is to say of it, and whether it
/// did not run at all.
fn timed_words(timed: &Timed) -> (String, String, bool) {
    match (&timed.checked, &timed.error) {
        (Some(checked), _) => (
            format!("{:.1} s for a minute of music", checked.seconds_a_minute),
            format!("Ready in {:.1} s \u{00B7} {} of memory at the most", checked.ready_seconds, said(timed.peak)),
            false,
        ),
        (None, why) => {
            (sentence(&format!("Did not run: {}", why.as_deref().unwrap_or("it did not say why"))), String::new(), true)
        }
    }
}

/// A name that begins a line: with a capital.
fn capital(words: &str) -> String {
    let mut letters = words.chars();
    letters.next().map_or_else(String::new, |first| first.to_uppercase().chain(letters).collect())
}

// ---- the panel -----------------------------------------------------------------

/// A caps heading over a rule, with what is said at its right end.
fn heading(ui: &mut Ui, theme: &Theme, title: &str, right: &str) -> Rect {
    let (row, _) = ui.allocate_exact_size(vec2(WIDTH, 20.0), Sense::hover());
    text::paint(ui.painter(), row, Align2::LEFT_CENTER, theme.text.micro, title, theme.ink_faint);
    if !right.is_empty() {
        text::paint(ui.painter(), row, Align2::RIGHT_CENTER, theme.text.micro, right, theme.ink_faint);
    }
    paint::hline_above(ui.painter(), row.x_range(), row.bottom(), theme.hairline);
    ui.add_space(GAP);
    row
}

/// A paragraph that wraps at `width`.
fn paragraph(ui: &mut Ui, theme: &Theme, color: egui::Color32, words: &str, width: f32) {
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let galley = prose::plain(ui, theme, style, color, words, width);
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, galley.size().y), Sense::hover());
    // The words are painted; this is them for whoever cannot see paint.
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    prose::paint(ui, rect.min, galley);
}

/// A label in small type and what it says beside it, on one line or wrapped under one another.
fn fact(ui: &mut Ui, theme: &Theme, label: &str, words: &str, color: egui::Color32) {
    const LABEL: f32 = 88.0;
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let galley = prose::plain(ui, theme, style, color, words, WIDTH - LABEL);
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, galley.size().y.max(16.0)), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{label}: {words}")));
    let head = Rect::from_min_size(rect.min, vec2(LABEL, 16.0));
    text::paint(ui.painter(), head, Align2::LEFT_CENTER, theme.text.micro, label, theme.ink_faint);
    prose::paint(ui, pos2(rect.left() + LABEL, rect.top()), galley);
    ui.add_space(3.0);
}

/// A small button at the right end of `row`.
fn row_button(ui: &mut Ui, cx: &mut Cx, row: Rect, label: &str, name: &str, dimmed: bool, hint: &str) -> bool {
    let theme = cx.theme;
    let chip = Chip::action(label).dimmed(dimmed).named(name);
    let width = chip.width(ui, theme);
    let rect = Rect::from_min_size(
        pos2(row.right() - width, row.center().y - theme.metric.chip / 2.0),
        vec2(width, theme.metric.chip),
    );
    let response = trill_noir::chip(ui, theme, rect, ui.scope_id().with(("row-button", name)), &chip);
    cx.hint(&response, hint);
    response.clicked() && !dimmed
}

/// Where something is kept, with the button that moves it.
fn kept_line(ui: &mut Ui, ctx: &egui::Context, cx: &mut Cx, what: Kept, folder: &std::path::Path, moves: &str) {
    let theme = cx.theme;
    let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let path = folder.display().to_string();
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("Kept in {path}")));
    let name = match &what {
        Kept::Studies => "Keep studies elsewhere".to_owned(),
        Kept::List(list) => format!("Keep {list} elsewhere"),
    };
    let busy = cx.setup.working > 0 || cx.setup.dialog_open;
    if row_button(ui, cx, row, "Move\u{2026}", &name, busy, moves) {
        keep_in_dialog(ctx, cx, what, folder.to_owned());
    }
    let used = text::paint(ui.painter(), row, Align2::LEFT_CENTER, theme.text.micro, "Kept in", theme.ink_faint);
    let room = Rect::from_min_max(pos2(row.left() + used + 10.0, row.top()), pos2(row.right() - 80.0, row.bottom()));
    let style = TextStyle { size: 12.0, ..theme.text.body };
    text::paint_name(ui.painter(), room, Align2::LEFT_CENTER, style, &path, theme.ink_dim);
}

/// Shows the panel while it is open.
pub fn modal(ctx: &egui::Context, cx: &mut Cx) {
    if !cx.setup.open {
        return;
    }
    let theme = cx.theme;
    // What is shown is read again every so often: sooner while something is on its way.
    let now = ctx.input(|input| input.time);
    let on_its_way = cx.setup.working > 0
        || cx.setup.testing.is_some()
        || cx.setup.shown.lists.iter().flat_map(|list| &list.parts).any(|status| {
            matches!(
                status.state,
                PartState::Waiting | PartState::Fetching | PartState::Checking | PartState::Unpacking
            )
        });
    let every = if on_its_way { READ_EVERY_BUSY } else { READ_EVERY };
    if !cx.setup.staged && !cx.setup.reading && now - cx.setup.read_at >= every {
        (cx.setup.reading, cx.setup.read_at) = (true, now);
        cx.setup.spawn(ctx, &cx.sounds.runtime, |runtime, report| report(Done::Read(Box::new(read(runtime)))));
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(every));
    if std::mem::take(&mut cx.setup.stale) {
        look(ctx, cx);
    }

    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(18, 14));
    let tall = ctx.content_rect().height() - 90.0;
    let modal = egui::Modal::new(Id::unique("setup")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
        ui.set_width(WIDTH);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let (title, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
        text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, "Setup", theme.ink);
        text::paint(ui.painter(), title, Align2::RIGHT_CENTER, theme.text.micro, "Esc closes", theme.ink_faint);
        ui.add_space(8.0);
        let (rule, _) = ui.allocate_exact_size(vec2(WIDTH, 1.0), Sense::hover());
        paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
        ui.add_space(10.0);

        if let Some(why) = cx.setup.shown.unavailable.clone() {
            paragraph(ui, theme, theme.ink, &why, WIDTH);
            return;
        }
        if !cx.setup.read {
            paragraph(ui, theme, theme.ink_dim, "Looking at what is here\u{2026}", WIDTH);
            return;
        }

        // The tabs: a list each, and the machine.
        let titles: Vec<String> = cx.setup.shown.lists.iter().map(|list| list.title.clone()).collect();
        let mut labels: Vec<&str> = titles.iter().map(String::as_str).collect();
        labels.push("This machine");
        cx.setup.tab = cx.setup.tab.min(labels.len() - 1);
        let (tabs, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.chip), Sense::hover());
        let id = ui.scope_id().with("tabs");
        let (clicked, _) = trill_noir::chip_row(ui, theme, tabs.min, WIDTH, id, &labels, Some(cx.setup.tab), false);
        if let Some(tab) = clicked {
            cx.setup.tab = tab;
        }
        ui.add_space(12.0);

        // As tall as what it shows, up to the window's own height: a scroll area by
        // itself takes no more room than it is first offered.
        let tall = tall.max(240.0);
        let body = egui::ScrollArea::vertical()
            .id_salt("setup-body")
            .max_height(tall)
            .min_scrolled_height(tall)
            .auto_shrink([false, true]);
        body.show(ui, |ui| {
            ui.set_width(WIDTH);
            match cx.setup.shown.lists.get(cx.setup.tab).cloned() {
                Some(list) => parts(ui, ctx, cx, &list),
                None => machine(ui, ctx, cx),
            }
            // What the last thing that was asked for said.
            if let Some((words, wrong)) = cx.setup.last.clone() {
                ui.add_space(10.0);
                paragraph(ui, theme, if wrong { theme.warn } else { theme.ink_dim }, &words, WIDTH);
            }
        });
    });
    if modal.should_close() {
        close(cx);
    }
}

/// One list: what it is, where it is kept, its parts, and the part that
/// is looked at with everything there is to read before it is fetched.
fn parts(ui: &mut Ui, ctx: &egui::Context, cx: &mut Cx, list: &ListStatus) {
    let theme = cx.theme;
    if !list.about.is_empty() {
        paragraph(ui, theme, theme.ink_dim, &list.about, WIDTH);
        ui.add_space(8.0);
    }
    kept_line(
        ui,
        ctx,
        cx,
        Kept::List(list.id.clone()),
        &list.folder,
        "Keep what is fetched in another folder: on a disk with more room, say",
    );
    ui.add_space(8.0);
    heading(ui, theme, "Parts", &list_words(list));
    if list.parts.is_empty() {
        paragraph(ui, theme, theme.ink_dim, "Nothing is published for this system yet.", WIDTH);
        return;
    }
    let selected = cx
        .setup
        .selected
        .get(&list.id)
        .filter(|id| list.parts.iter().any(|status| status.part.id == **id))
        .cloned()
        .unwrap_or_else(|| list.parts[0].part.id.clone());
    let mut choose = None;
    let shown = list.parts.len().min(MAX_ROWS) as f32;
    let rows = egui::ScrollArea::vertical()
        .id_salt(("parts", &list.id))
        .max_height(shown * ROW)
        .min_scrolled_height(shown * ROW)
        .auto_shrink([false, true]);
    rows.show(ui, |ui| {
        for (index, status) in list.parts.iter().enumerate() {
            let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::click());
            let (state, wrong) = state_words(status);
            let size = said(status.part.size());
            let spoken = format!("{} \u{00B7} {size} \u{00B7} {state}", status.part.name);
            let chosen = status.part.id == selected;
            response.widget_info(|| WidgetInfo::selected(Role::Button, true, chosen, &spoken));
            if chosen {
                ui.painter().rect_filled(row, 0.0, theme.selected);
            } else if response.hovered() {
                ui.painter().rect_filled(row, 0.0, theme.hover);
            }
            if index > 0 {
                paint::hline(ui.painter(), row.x_range(), row.top(), theme.grid);
            }
            if response.clicked() {
                choose = Some(status.part.id.clone());
            }
            cx.hint(&response, &status.part.what);
            let lamp = pos2(row.left() + 6.0, row.center().y);
            let after = match (status.state, wrong) {
                (PartState::Ready, _) => paint::dot(ui.painter(), lamp, theme.metric.dot, theme.ok),
                (_, true) => paint::dot(ui.painter(), lamp, theme.metric.dot, theme.warn),
                (PartState::Missing, _) => paint::hollow_dot(ui.painter(), lamp, theme.metric.dot, theme.ghost),
                _ => paint::dot(ui.painter(), lamp, theme.metric.dot, theme.signal),
            };
            let words = Rect::from_min_max(pos2(after + 8.0, row.top()), pos2(row.right() - 8.0, row.bottom()));
            let style = TextStyle { size: 12.0, ..theme.text.body };
            let color = if wrong { theme.warn } else { theme.ink_dim };
            let used = text::paint(ui.painter(), words, Align2::RIGHT_CENTER, style, &state, color);
            let sizes = words.with_max_x(words.right() - used.max(130.0) - 14.0);
            let used = text::paint(ui.painter(), sizes, Align2::RIGHT_CENTER, theme.text.value, &size, theme.ink_dim);
            let name = sizes.with_max_x(sizes.right() - used - 14.0);
            let ink = if status.state == PartState::Ready { theme.ink } else { theme.ink_dim };
            text::paint_name(ui.painter(), name, Align2::LEFT_CENTER, theme.text.body, &status.part.name, ink);
            // How far a fetch has come, along the foot of its row.
            if matches!(status.state, PartState::Fetching | PartState::Checking | PartState::Unpacking) {
                let bar = Rect::from_min_max(pos2(row.left(), row.bottom() - 2.0), row.max);
                let share = status.got as f32 / status.part.size().max(1) as f32;
                paint::progress(ui.painter(), bar, share, theme.well, theme.signal);
            }
        }
    });
    if let Some(part) = choose {
        cx.setup.selected.insert(list.id.clone(), part);
    }
    let Some(status) = list.parts.iter().find(|status| status.part.id == selected) else { return };
    ui.add_space(12.0);
    detail(ui, ctx, cx, status);
}

/// Everything there is to read of a part, and under it the button that
/// fetches it, stops its fetch or removes it.
fn detail(ui: &mut Ui, ctx: &egui::Context, cx: &mut Cx, status: &PartStatus) {
    let theme = cx.theme;
    let part = &status.part;
    let (state, _) = state_words(status);
    heading(ui, theme, &part.name, &format!("{} \u{00B7} {state}", said(part.size())));
    paragraph(ui, theme, theme.ink, &part.what, WIDTH);
    ui.add_space(8.0);
    fact(ui, theme, "Licence", &part.licence, theme.ink);
    if !part.credit.is_empty() {
        fact(ui, theme, "Credit", &part.credit, theme.ink);
    }
    fact(ui, theme, "From", &part.from, theme.ink_dim);
    fact(ui, theme, "Checked by", &checked_by(status), theme.ink_dim);
    let lacking = lacking(status, &cx.setup.shown.lists);
    if !lacking.is_empty() {
        let names: Vec<&str> = lacking.iter().map(String::as_str).collect();
        let words = format!(
            "{}, which {} not here",
            trill_core::journal::words::list(&names),
            if names.len() == 1 { "is" } else { "are" }
        );
        fact(ui, theme, "Needs", &words, theme.warn);
    }
    if let Some(why) = &status.error {
        ui.add_space(4.0);
        paragraph(ui, theme, theme.warn, &stopped(why), WIDTH);
    }

    ui.add_space(8.0);
    let (foot, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
    let going =
        matches!(status.state, PartState::Waiting | PartState::Fetching | PartState::Checking | PartState::Unpacking);
    let note = match (status.state, going) {
        (PartState::Ready, _) => format!("{} on disk", said(status.on_disk)),
        (_, true) => format!("{} of {} have come", said(status.got), said(part.size())),
        _ if status.got > 0 => {
            format!("{} of {} are here already: fetching goes on from there", said(status.got), said(part.size()))
        }
        _ => "Nothing is fetched until you press Fetch".to_owned(),
    };
    let layout = Layout::right_to_left(Align::Center);
    let mut cell = ui.new_child(UiBuilder::new().id_salt(("part", &part.id)).max_rect(foot).layout(layout));
    cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
    if cx.setup.working > 0 {
        cell.disable();
    }
    match (status.state, going) {
        (PartState::Ready, _) => {
            if trill_noir::button(&mut cell, theme, "Remove", Variant::Default).clicked() {
                remove(ctx, cx, part.id.clone());
            }
        }
        (_, true) => {
            if trill_noir::button(&mut cell, theme, "Stop", Variant::Default).clicked() {
                stop(ctx, cx, part.id.clone());
            }
        }
        _ => {
            let left = part.size().saturating_sub(status.got);
            let label = if status.got > 0 {
                format!("Go on \u{00B7} {}", said(left))
            } else {
                format!("Fetch \u{00B7} {}", said(left))
            };
            if trill_noir::button(&mut cell, theme, &label, Variant::Primary).clicked() {
                fetch(ctx, cx, part.id.clone());
            }
            if status.got > 0 && trill_noir::button(&mut cell, theme, "Remove", Variant::Default).clicked() {
                remove(ctx, cx, part.id.clone());
            }
        }
    }
    let room = foot.with_max_x(cell.min_rect().left() - 12.0);
    text::paint_name(ui.painter(), room, Align2::LEFT_CENTER, theme.text.micro, &note, theme.ink_faint);
}

/// The machine: what runs the models and the test that finds out, the
/// programs Trill found, and where studies are kept.
fn machine(ui: &mut Ui, ctx: &egui::Context, cx: &mut Cx) {
    let theme = cx.theme;
    let row = heading(ui, theme, "What runs the models", "");
    let testing = cx.setup.testing.clone();
    if cx.setup.shown.can_test {
        match &testing {
            Some(_) => {
                if row_button(
                    ui,
                    cx,
                    row,
                    "Stop",
                    "Stop the timed test",
                    false,
                    "End the timed test: nothing is changed",
                ) {
                    cx.setup.stop_test.store(true, Ordering::Relaxed);
                }
            }
            None => {
                let hint = "Split half a minute of noise on the processor and on every graphics card, and take the fastest that gives the same result: a minute or two";
                if row_button(ui, cx, row, "Run the timed test", "Run the timed test", false, hint) {
                    test(ctx, cx);
                }
            }
        }
    }
    let machine = cx.setup.shown.machine.clone();
    let by = match &machine {
        Some(machine) if !cx.setup.shown.untested && machine.by_user => " \u{00B7} your choice",
        Some(_) if !cx.setup.shown.untested => " \u{00B7} chosen by the timed test",
        _ => "",
    };
    let words = format!("The models run on {}{by}", cx.setup.shown.runs_on);
    let (line, response) = ui.allocate_exact_size(vec2(WIDTH, 20.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &words));
    text::paint(ui.painter(), line, Align2::LEFT_CENTER, theme.text.body, &words, theme.ink);
    if let Some((what, share)) = &testing {
        let (line, response) = ui.allocate_exact_size(vec2(WIDTH, 20.0), Sense::hover());
        let words = format!("{what}\u{2026} {:.0} %", share * 100.0);
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &words));
        let style = TextStyle { size: 12.0, ..theme.text.body };
        text::paint(ui.painter(), line, Align2::LEFT_CENTER, style, &words, theme.ink_dim);
        let bar = Rect::from_min_max(pos2(line.left(), line.bottom() - 2.0), line.max);
        paint::progress(ui.painter(), bar, *share, theme.well, theme.signal);
        ui.add_space(4.0);
    }
    if cx.setup.shown.untested && testing.is_none() {
        let words = match &machine {
            Some(_) => {
                "A part that runs the models has come or gone since the timed test. Until it is run again, the models run on the processor, which is the slowest way."
            }
            None => {
                "The timed test has not been run. Until it has, the models run on the processor, which is the slowest way: a card that is there is not taken to be faster before it was measured."
            }
        };
        ui.add_space(2.0);
        paragraph(ui, theme, theme.ink_dim, words, WIDTH);
    }
    let current = !cx.setup.shown.untested;
    for (index, timed) in machine.iter().flat_map(|machine| &machine.results).enumerate() {
        let chosen = current && machine.as_ref().is_some_and(|machine| machine.chosen == timed.provider);
        let (words, more, wrong) = timed_words(timed);
        let device = capital(&timed.device);
        if wrong {
            // What did not run says why, in as many lines as that takes.
            ui.add_space(4.0);
            paragraph(ui, theme, theme.warn, &format!("{device}. {words}"), WIDTH);
            ui.add_space(4.0);
            continue;
        }
        let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
        let spoken = format!("{device} \u{00B7} {words}");
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &spoken));
        cx.hint(&response, &more);
        if index > 0 {
            paint::hline(ui.painter(), row.x_range(), row.top(), theme.grid);
        }
        let lamp = pos2(row.left() + 6.0, row.center().y);
        let after = if chosen {
            paint::dot(ui.painter(), lamp, theme.metric.dot, theme.ok)
        } else {
            paint::hollow_dot(ui.painter(), lamp, theme.metric.dot, theme.ghost)
        };
        // The switch at the right: this is what runs the models.
        let name = format!("Run the models on {}", timed.device);
        let chip = Chip::new("Use", chosen).dimmed(!current || cx.setup.working > 0).named(&name);
        let width = chip.width(ui, theme);
        let rect = Rect::from_min_size(
            pos2(row.right() - width, row.center().y - theme.metric.chip / 2.0),
            vec2(width, theme.metric.chip),
        );
        let response = trill_noir::chip(ui, theme, rect, ui.scope_id().with(("use", index)), &chip);
        cx.hint(&response, "Have this run the models, in place of what the test chose");
        if response.clicked() && current && !chosen && cx.setup.working == 0 {
            choose(ctx, cx, timed.provider);
        }
        let room = Rect::from_min_max(pos2(after + 8.0, row.top()), pos2(rect.left() - 12.0, row.bottom()));
        let style = TextStyle { size: 12.0, ..theme.text.body };
        let used = text::paint(ui.painter(), room, Align2::RIGHT_CENTER, style, &words, theme.ink_dim);
        let name = room.with_max_x(room.right() - used - 14.0);
        let ink = if chosen { theme.ink } else { theme.ink_dim };
        text::paint_name(ui.painter(), name, Align2::LEFT_CENTER, theme.text.body, &device, ink);
    }

    // The programs that are not Trill's own.
    ui.add_space(14.0);
    let row = heading(ui, theme, "Programs Trill uses and does not ship", "");
    let looking = cx.setup.looking;
    if row_button(
        ui,
        cx,
        row,
        "Look again",
        "Look again for the programs",
        looking,
        "Look again where programs are installed",
    ) {
        look(ctx, cx);
    }
    for (index, program) in cx.setup.programs.clone().iter().enumerate() {
        let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
        let found = match &program.path {
            Some(path) => path.display().to_string(),
            None => format!("Not found \u{00B7} {}", program.get),
        };
        let spoken = format!("{} \u{00B7} {found}", program.name);
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &spoken));
        cx.hint(&response, program.what);
        if index > 0 {
            paint::hline(ui.painter(), row.x_range(), row.top(), theme.grid);
        }
        let lamp = pos2(row.left() + 6.0, row.center().y);
        let after = match program.path {
            Some(_) => paint::dot(ui.painter(), lamp, theme.metric.dot, theme.ok),
            None => paint::hollow_dot(ui.painter(), lamp, theme.metric.dot, theme.ghost),
        };
        let words = Rect::from_min_max(pos2(after + 8.0, row.top()), row.max);
        let used = text::paint(ui.painter(), words, Align2::LEFT_CENTER, theme.text.body, program.name, theme.ink);
        let rest = words.with_min_x(words.left() + used.max(52.0) + 14.0);
        let style = TextStyle { size: 12.0, ..theme.text.body };
        text::paint_name(ui.painter(), rest, Align2::LEFT_CENTER, style, &found, theme.ink_dim);
    }
    if cx.setup.programs.is_empty() && looking {
        paragraph(ui, theme, theme.ink_dim, "Looking\u{2026}", WIDTH);
    }

    // Where studies are kept, and what they take.
    if let Some((folder, count, size)) = cx.setup.studies.clone() {
        ui.add_space(14.0);
        let words = match count {
            0 => "none yet".to_owned(),
            1 => format!("1 study \u{00B7} {}", said(size)),
            count => format!("{count} studies \u{00B7} {}", said(size)),
        };
        heading(ui, theme, "Studies", &words);
        let moves = "Keep studies in another folder, and move the ones there are: a song's stems are a couple of hundred megabytes";
        kept_line(ui, ctx, cx, Kept::Studies, &folder, moves);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_fetch::{Fetched, Part};
    use trill_study::helper::Checked;

    fn status(state: PartState, got: u64, error: Option<&str>) -> PartStatus {
        let sum = "abaac48bc8fd55fa672268c8f13228b9b00f52f0c4cd10945048982eca4643bc";
        PartStatus {
            part: Part {
                id: "stems".into(),
                name: "Stems".into(),
                needs: vec!["runtime".into()],
                files: vec![Fetched::file("https://example.org/stems.onnx", sum, 200 << 20, "stems.onnx")],
                ..Part::default()
            },
            folder: PathBuf::from("models/stems"),
            state,
            got,
            on_disk: got,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn a_part_says_where_it_is_in_a_word_or_two() {
        let said = |state, got, error| state_words(&status(state, got, error));
        assert_eq!(said(PartState::Ready, 200 << 20, None), ("Here".to_owned(), false));
        assert_eq!(said(PartState::Missing, 0, None), ("Not here".to_owned(), false));
        assert_eq!(said(PartState::Missing, 50 << 20, None), ("Stopped at 25 %".to_owned(), false));
        assert_eq!(said(PartState::Fetching, 86 << 20, None), ("Fetching \u{00B7} 43 %".to_owned(), false));
        assert_eq!(said(PartState::Waiting, 0, None), ("Waiting its turn".to_owned(), false));
        assert_eq!(
            said(PartState::Missing, 10, Some("the connection was lost")),
            ("Could not be fetched".to_owned(), true)
        );
        assert_eq!(checked_by(&status(PartState::Ready, 0, None)), "SHA-256 abaac48bc8fd\u{2026}");
    }

    #[test]
    fn a_list_says_how_much_of_it_is_here_and_a_part_what_it_lacks() {
        let runtime = PartStatus {
            part: Part { id: "runtime".into(), name: "The models' runtime".into(), ..Part::default() },
            ..status(PartState::Missing, 0, None)
        };
        let list = ListStatus {
            id: "models".into(),
            title: "Studies".into(),
            about: String::new(),
            folder: PathBuf::from("models"),
            parts: vec![runtime, status(PartState::Ready, 200 << 20, None)],
        };
        assert_eq!(list_words(&list), "1 of 2 here \u{00B7} 200 MB on disk");
        assert_eq!(lacking(&list.parts[1], std::slice::from_ref(&list)), ["The models' runtime"]);
        assert!(lacking(&list.parts[0], std::slice::from_ref(&list)).is_empty());
    }

    #[test]
    fn a_way_of_running_the_models_says_what_it_took_or_why_it_did_not_run() {
        let ran = Timed {
            provider: Provider::Nvidia,
            device: "a graphics card (NVIDIA)".into(),
            checked: Some(Checked { device: String::new(), ready_seconds: 1.84, seconds_a_minute: 3.02, sum: 1.0 }),
            error: None,
            peak: 3800 << 20,
        };
        assert_eq!(
            timed_words(&ran),
            (
                "3.0 s for a minute of music".to_owned(),
                "Ready in 1.8 s \u{00B7} 3.7 GB of memory at the most".to_owned(),
                false
            )
        );
        let failed = Timed { checked: None, error: Some("the process that studies went down".into()), ..ran };
        assert_eq!(
            timed_words(&failed),
            ("Did not run: the process that studies went down.".to_owned(), String::new(), true)
        );
        assert_eq!(capital("a graphics card (NVIDIA)"), "A graphics card (NVIDIA)");
        assert_eq!(sentence("it could not"), "It could not.");
    }
}
