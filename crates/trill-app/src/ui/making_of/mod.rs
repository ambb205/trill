//! The making of the song at hand: its session played again.
//!
//! The session keeps a journal of every step that changed the song, of what
//! was said between the user and the assistant, and of what was looked at
//! and heard on the way (`trill_core::journal`). This is where it is
//! watched: the arrangement and the detail panel show the song as it was at
//! a moment of the session, clips arriving and notes filling in; down the
//! right the conversation as it came, with every step under who made it;
//! along the bottom the whole session as a line of marks, to play, pause,
//! step through and scrub.
//!
//! **Nothing here touches the song.** What is watched is a copy of the
//! journal, taken when the making-of is opened, laid out by
//! `trill_core::journal::replay`, which reads nothing else: the same
//! journal and the same moment give the same picture every time. The views
//! that draw the song are the window's own, handed a snapshot of the song
//! as it was ([`Snapshot::of_song`]) and a view state of the replay's own,
//! so that leaving puts the window back exactly as it was. They are also
//! handed a session of the replay's own, empty and silent, in place of the
//! real one: whatever a view might do to its session it does to that. And
//! they are under glass: the pointer does not reach them. A song cannot be
//! edited from here even by accident.
//!
//! **Time is the replay's.** The waiting is cut out of a session; what is
//! left plays at a speed the viewer picks. Each step goes where the action
//! was: the arrangement scrolls to it, a clip being written opens in the
//! piano roll, a device being added shows in the Devices tab.
//!
//! [`scene`] is a canned session, for the pictures of this view.
//!
//! A view that films a session (the stage) needs no more than what
//! [`Watching::moment`] and the replay's `frame` give: a song as it was, a
//! time, and where the action is.

pub mod scene;
mod story;
mod strip;

use super::assistant::places::Route;
use super::assistant::reply::Replies;
use super::shortcuts::Command;
use super::snapshot::{Live, Snapshot};
use super::state::UiState;
use super::{DetailTab, brand, status_bar, top_bar};
use eframe::egui::{self, Align2, Id, Rect, Sense, Ui, pos2, vec2};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use trill_core::device::{DeviceDescriptor, DeviceFactory};
use trill_core::journal::replay::{Cursor, Replay};
use trill_core::journal::{Author, Entry, Journal, Place};
use trill_dsp::BuiltinFactory;
use trill_engine::{AudioBackend, Session, SharedSession};
use trill_noir::{Chip, Icon, Theme, paint, text};

/// The speeds a replay can be watched at, and what each is called.
pub const SPEEDS: [(f64, &str); 5] = [(0.5, "\u{00BD}\u{00D7}"), (1.0, "1\u{00D7}"), (2.0, "2\u{00D7}"), (4.0, "4\u{00D7}"), (8.0, "8\u{00D7}")];
/// The most time one frame moves the replay on by, in seconds: a window
/// that was hidden for a while does not jump.
const LONGEST_FRAME: f64 = 0.25;
/// A step back within this long of a step's beginning goes to the step
/// before it, as the same button does on any player.
const STEP_BACK_GRACE: f64 = 0.4;
/// Width of the story down the right, as it comes and at its limits.
const STORY_WIDTH: f32 = 360.0;
const STORY_LIMITS: std::ops::RangeInclusive<f32> = 280.0..=520.0;

/// What is on show: a journal being read, a replay, or why there is none.
enum Stage {
    Reading(Receiver<Result<Journal, String>>),
    Watching(Box<Watching>),
    Nothing(String),
}

/// The making-of, open.
pub struct MakingOf {
    stage: Stage,
    /// What the song is called, for the bar.
    title: String,
    factory: Arc<dyn DeviceFactory>,
    /// Where the replay starts, and whether it plays from there.
    start: Start,
}

/// Where a replay starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Start {
    /// How far in, as a fraction of its length: 0 the beginning, 1 the end.
    pub at: f64,
    pub playing: bool,
}

impl Default for Start {
    /// From the beginning, playing: whoever opens it wants to watch.
    fn default() -> Self {
        Self { at: 0.0, playing: true }
    }
}

/// A replay being watched.
pub struct Watching {
    replay: Replay,
    cursor: Cursor,
    /// Where the replay is, in seconds at speed 1.
    time: f64,
    playing: bool,
    speed: f64,
    /// The window's clock at the last frame.
    clock: Option<f64>,
    /// The song as it was, as the views draw it, and which state of the
    /// cursor's song it was made of.
    snapshot: Snapshot,
    version: Option<u64>,
    /// The replay's own view of the window: what is selected, scrolled to
    /// and open in it. The window's own is left as it is.
    state: UiState,
    live: Live,
    /// What the views are given in place of the session.
    sandbox: SharedSession,
    /// The moment under way.
    moment: Option<usize>,
    /// Where the view last went.
    went: Option<Route>,
    /// How the assistant's replies were last set.
    replies: Replies,
    /// The moment the story was last scrolled to.
    told: Option<usize>,
    /// How far through the moment under way the replay is, from 0 to 1.
    progress: f64,
}

/// A span of time as it is said: "8 s", "12 min", "3 h 20 min", "2 days".
pub fn span(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    match seconds {
        0..=59 => format!("{seconds} s"),
        60..=3_599 => format!("{} min", seconds / 60),
        3_600..=86_399 => match seconds % 3_600 / 60 {
            0 => format!("{} h", seconds / 3_600),
            minutes => format!("{} h {minutes} min", seconds / 3_600),
        },
        86_400..=172_799 => "1 day".to_owned(),
        _ => format!("{} days", seconds / 86_400),
    }
}

/// A position in a replay as a clock shows it: "0:42".
pub fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0).floor() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Who made an entry, as a heading: "You", "The assistant", a client's
/// name.
pub fn who(author: &Author) -> String {
    match author {
        Author::Person => "You".to_owned(),
        Author::Assistant => "The assistant".to_owned(),
        Author::Client(name) if name.is_empty() => "A client".to_owned(),
        Author::Client(name) => name.clone(),
    }
}

/// The color an author's marks are drawn in.
pub fn hue(theme: &Theme, author: &Author) -> egui::Color32 {
    match author {
        Author::Person => theme.ink,
        Author::Assistant => theme.info,
        Author::Client(_) => theme.data,
    }
}

impl MakingOf {
    /// Opens the making-of of the song `session` has open. Its journal is
    /// copied on a helper thread; the replay shows once it is there.
    pub fn open(ctx: &egui::Context, session: &SharedSession, factory: Arc<dyn DeviceFactory>, start: Start) -> Self {
        let (sender, journal) = channel();
        let (session, waker) = (session.clone(), ctx.clone());
        let title = session.lock().project().name.clone();
        std::thread::spawn(move || {
            let copied = session.lock().journal().cloned();
            let journal = copied.ok_or_else(|| {
                "No journal is kept, so there is nothing to play again. Keep one from the View menu.".to_owned()
            });
            if sender.send(journal).is_ok() {
                waker.request_repaint();
            }
        });
        Self { stage: Stage::Reading(journal), title, factory, start }
    }

    /// Opens the making-of that was written out to `file`, to watch what
    /// someone handed over.
    pub fn of_file(ctx: &egui::Context, file: PathBuf, factory: Arc<dyn DeviceFactory>) -> Self {
        let (sender, journal) = channel();
        let waker = ctx.clone();
        let title = file.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        std::thread::spawn(move || {
            let read = std::fs::read_to_string(&file).map_err(|error| format!("{}: {error}", file.display()));
            let journal = read.and_then(|text| Journal::from_text(&text).map_err(|error| error.to_string()));
            if sender.send(journal).is_ok() {
                waker.request_repaint();
            }
        });
        Self { stage: Stage::Reading(journal), title, factory, start: Start::default() }
    }

    /// The part of a frame that needs no window: takes in the journal when
    /// it has been read, and moves the replay on by the time that passed.
    pub fn tick(&mut self, ctx: &egui::Context, descriptors: &[DeviceDescriptor]) {
        if let Stage::Reading(journal) = &self.stage
            && let Ok(read) = journal.try_recv()
        {
            self.stage = match read {
                Ok(journal) if journal.entries().all(Entry::is_aside) => Stage::Nothing(
                    "Nothing has happened in this song since its journal began. Make something, and come back."
                        .to_owned(),
                ),
                Ok(journal) => Stage::Watching(Box::new(Watching::new(journal, self.start))),
                Err(reason) => Stage::Nothing(reason),
            };
        }
        if let Stage::Watching(watching) = &mut self.stage {
            watching.advance(ctx);
            watching.look(descriptors, self.factory.as_ref());
        }
    }

    /// The replay, once it is there.
    pub fn watching(&self) -> Option<&Watching> {
        match &self.stage {
            Stage::Watching(watching) => Some(watching),
            _ => None,
        }
    }

    pub fn watching_mut(&mut self) -> Option<&mut Watching> {
        match &mut self.stage {
            Stage::Watching(watching) => Some(watching),
            _ => None,
        }
    }

    /// Why there is nothing to watch, if there is not.
    pub fn nothing(&self) -> Option<&str> {
        match &self.stage {
            Stage::Nothing(reason) => Some(reason),
            _ => None,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Does what a key asked for. Returns whether the making-of is to be
    /// left.
    pub fn run(&mut self, command: Command) -> bool {
        match command {
            Command::Escape | Command::MakingOf => return true,
            _ => {}
        }
        if let Stage::Watching(watching) = &mut self.stage {
            match command {
                Command::TogglePlay => watching.toggle(),
                Command::Rewind => watching.seek(0.0),
                Command::Nudge(steps, _) if steps != 0 => watching.step(steps > 0),
                _ => {}
            }
        }
        false
    }
}

impl Watching {
    fn new(journal: Journal, start: Start) -> Self {
        let replay = Replay::new(journal);
        let cursor = Cursor::new(&replay);
        // What the views are handed in place of the session: empty, silent,
        // and nobody else's. It starts no audio device.
        let sandbox = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 });
        let sandbox = Arc::new(parking_lot::Mutex::new(sandbox.expect("a session without an audio device starts")));
        let snapshot = Snapshot::of_song(trill_core::Project::new(""), 0, &[], &BuiltinFactory);
        let time = replay.length() * start.at.clamp(0.0, 1.0);
        Self {
            cursor,
            time,
            playing: start.playing && time < replay.length(),
            speed: 1.0,
            clock: None,
            snapshot,
            version: None,
            state: UiState::new(DetailTab::Clip),
            live: Live::default(),
            sandbox,
            moment: None,
            went: None,
            replies: Replies::default(),
            told: None,
            progress: 0.0,
            replay,
        }
    }

    /// Moves the replay on by the time that passed since the last frame.
    fn advance(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|input| input.time);
        let passed = self.clock.replace(now).map_or(0.0, |before| (now - before).clamp(0.0, LONGEST_FRAME));
        if !self.playing {
            return;
        }
        self.time += passed * self.speed;
        if self.time >= self.replay.length() {
            self.time = self.replay.length();
            self.playing = false;
        }
        ctx.request_repaint();
    }

    /// Brings the snapshot in step with the song as it was at this moment,
    /// and the view to where the action is.
    fn look(&mut self, descriptors: &[DeviceDescriptor], factory: &dyn DeviceFactory) {
        let frame = self.replay.frame(self.time, &mut self.cursor);
        let moment = frame.moment;
        self.progress = frame.progress;
        if self.version != Some(frame.version) {
            self.version = Some(frame.version);
            self.snapshot = Snapshot::of_song(frame.song.clone(), frame.version, descriptors, factory);
            self.state.reconcile(&self.snapshot.project);
        } else if self.moment == moment {
            return;
        }
        self.moment = moment;
        // Where this entry landed: gone to once, and again when more of it
        // has arrived (a clip is not there to select until it is).
        let places = moment.map_or(&[][..], |index| self.replay.places_at(index));
        let route = Route::to(places, &self.snapshot.project);
        if route.is_some() && route != self.went {
            self.went.clone_from(&route);
            if let Some(route) = route {
                route.take(&mut self.state);
            }
        }
        // The Clip tab with nothing in it says nothing about the song: it
        // shows the clip that was last worked on, if the moment itself
        // names none (the replay was begun in the middle, or scrubbed).
        if self.state.detail_tab == DetailTab::Clip
            && self.state.selected_clip.is_none()
            && let Some(index) = moment
        {
            let project = &self.snapshot.project;
            let steps = self.replay.moments()[..=index].iter().rev().filter_map(|moment| self.replay.entry(moment).step());
            let worked_on = steps.flat_map(|step| step.places.iter().rev()).find_map(|place| match place {
                Place::Clips { clips } => clips.iter().rev().find_map(|(id, _)| {
                    let id = trill_core::ClipId(*id);
                    Some((project.clip_track(id).ok()?, id))
                }),
                _ => None,
            });
            if let Some((track, clip)) = worked_on {
                self.state.select_clip(track, clip);
            }
        }
    }

    pub fn replay(&self) -> &Replay {
        &self.replay
    }

    /// Where the replay is, in seconds at speed 1.
    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// The moment under way, as the replay numbers them.
    pub fn moment(&self) -> Option<usize> {
        self.moment
    }

    /// The entry of the moment under way.
    pub fn entry(&self) -> Option<&Entry> {
        self.moment.map(|index| self.replay.entry(&self.replay.moments()[index]))
    }

    /// The song as it was at this moment, as the views are shown it.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// How many steps there are, and which of them the replay has reached:
    /// asides are nobody's steps and are not counted.
    pub fn steps(&self) -> (usize, usize) {
        let is_step = |moment: &trill_core::journal::replay::Moment| {
            self.replay.entry(moment).step().is_some_and(|step| !step.aside)
        };
        let moments = self.replay.moments();
        let reached = self.moment.map_or(0, |index| moments[..=index].iter().filter(|moment| is_step(moment)).count());
        (reached, moments.iter().filter(|moment| is_step(moment)).count())
    }

    /// Plays, or holds the replay where it is. At the end, playing starts
    /// over.
    pub fn toggle(&mut self) {
        if !self.playing && self.time >= self.replay.length() {
            self.time = 0.0;
        }
        self.playing = !self.playing;
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
    }

    /// Goes to `time`, in seconds at speed 1.
    pub fn seek(&mut self, time: f64) {
        self.time = time.clamp(0.0, self.replay.length());
        if self.time >= self.replay.length() {
            self.playing = false;
        }
    }

    /// Goes to the beginning of the next step, or of the one before.
    pub fn step(&mut self, forward: bool) {
        let moments = self.replay.moments();
        let target = match (self.moment, forward) {
            (None, true) => self.replay.step_from(0, false),
            (None, false) => None,
            (Some(index), true) => self.replay.step_from(index + 1, false),
            (Some(index), false) => {
                // Well into a step, back is its own beginning.
                let own = self.replay.step_from(index + 1, true).filter(|found| {
                    *found < index || self.time - moments[*found].starts > STEP_BACK_GRACE
                });
                own.or_else(|| self.replay.step_from(index, true))
            }
        };
        match target {
            Some(index) => self.time = moments[index].starts,
            None if forward => self.time = self.replay.length(),
            None => self.time = 0.0,
        }
        if self.time >= self.replay.length() {
            self.playing = false;
        }
    }

    /// What the views of the song are handed: the snapshot, the replay's
    /// own state, and a session that is not the song's.
    pub fn parts(&mut self) -> (&SharedSession, &Snapshot, &Live, &mut UiState) {
        (&self.sandbox, &self.snapshot, &self.live, &mut self.state)
    }

    /// The replay's own view of the window.
    #[cfg(test)]
    pub fn view(&self) -> &UiState {
        &self.state
    }

    /// What the status bar says the pointer is over.
    pub fn hint(&mut self) -> &mut super::state::Hint {
        &mut self.state.hint
    }
}

// ---- the bars ---------------------------------------------------------------------

const EDGE: f32 = 10.0;
const GAP: f32 = 14.0;
const BOX: f32 = 22.0;
const BUTTON: f32 = 26.0;
/// Where the transport begins, which is where the window's own does.
const TRANSPORT_LEFT: f32 = 372.0;

fn boxed(bar: Rect, left: f32, width: f32) -> Rect {
    Rect::from_min_size(pos2(left, bar.center().y - BOX / 2.0), vec2(width, BOX))
}

/// The bar across the top: what is being watched, the transport of the
/// replay where the song's own is, and the way out. Returns whether the
/// way out was taken.
pub fn bar(ui: &mut Ui, theme: &Theme, making_of: &mut MakingOf) -> bool {
    let bar = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(bar, 0.0, theme.panel);
    paint::hline_above(&painter, bar.x_range(), bar.bottom(), theme.hairline);
    let id = ui.scope_id().with("making-of");
    let row = |x: f32| Rect::from_min_max(pos2(x, bar.top()), bar.max);

    // The way out, at the right, laid out first so the rest knows its room.
    let leave = Chip::action("Leave");
    let width = leave.width(ui, theme);
    let leave_rect = Rect::from_min_size(
        pos2(bar.right() - EDGE - width, bar.center().y - theme.metric.chip / 2.0),
        vec2(width, theme.metric.chip),
    );
    let response = trill_noir::chip(ui, theme, leave_rect, id.with("leave"), &leave);
    let mut hint = None;
    if response.hovered() {
        hint = Some("Back to the song as it is \u{00B7} Esc".to_owned());
    }
    let left = response.clicked();

    let mut x = bar.left() + EDGE;
    x += brand::paint_centered(&painter, x, bar.center().y, 1.0).width() + 9.0;
    x += text::paint(&painter, row(x), Align2::LEFT_CENTER, theme.text.brand, "TRILL", theme.ink) + GAP;
    // What this is, in the one loud color: nobody should take the song
    // they are watching for the song they have open.
    x = paint::dot(&painter, pos2(x, bar.center().y), theme.metric.dot, theme.signal) + 6.0;
    x += text::paint(&painter, row(x), Align2::LEFT_CENTER, theme.text.caps, "Making of", theme.ink) + 9.0;
    let name_room = row(x).with_max_x((bar.left() + TRANSPORT_LEFT - GAP).max(x));
    text::paint(&painter, name_room, Align2::LEFT_CENTER, theme.text.body, &making_of.title, theme.ink_dim);

    let Some(watching) = making_of.watching_mut() else {
        return left;
    };
    let mut x = (bar.left() + TRANSPORT_LEFT).min(leave_rect.left() - 420.0).max(x + GAP);
    let button = |x: &mut f32| {
        let rect = boxed(bar, *x, BUTTON);
        *x += BUTTON + 4.0;
        rect
    };
    let playing = watching.is_playing();
    let response = trill_noir::icon_button(ui, theme, button(&mut x), id.with("back"), Icon::StepBack, false, theme.ink_dim);
    if response.hovered() {
        hint = Some("To the step before \u{00B7} Left".to_owned());
    }
    if response.clicked() {
        watching.step(false);
    }
    let (icon, color) = if playing { (Icon::Pause, theme.ok) } else { (Icon::Play, theme.ink_dim) };
    let response = trill_noir::icon_button(ui, theme, button(&mut x), id.with("play"), icon, playing, color);
    if response.hovered() {
        hint = Some("Play the session again, or hold it here \u{00B7} Space".to_owned());
    }
    if response.clicked() {
        watching.toggle();
    }
    let response =
        trill_noir::icon_button(ui, theme, button(&mut x), id.with("forward"), Icon::StepForward, false, theme.ink_dim);
    if response.hovered() {
        hint = Some("To the next step \u{00B7} Right".to_owned());
    }
    if response.clicked() {
        watching.step(true);
    }
    x += 6.0;

    // Where the replay is, of how long it is.
    let position = boxed(bar, x, 118.0);
    paint::frame(&painter, position, theme.hairline);
    let inner = position.shrink2(vec2(8.0, 0.0));
    let now = clock(watching.time() / 1.0);
    text::paint(&painter, inner, Align2::LEFT_CENTER, theme.text.readout, &now, theme.ink);
    let whole = clock(watching.replay().length());
    text::paint(&painter, inner, Align2::RIGHT_CENTER, theme.text.value, &whole, theme.ink_faint);
    let response = ui.interact(position, id.with("position"), Sense::hover());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("Replay at {now} of {whole}")));
    if response.hovered() {
        hint = Some("Where the replay is, and how long it is at this speed\u{2019}s 1\u{00D7}: the waiting is cut out".to_owned());
    }
    x = position.right() + 8.0;

    // The speed, one of a few.
    let labels: Vec<&str> = SPEEDS.iter().map(|(_, label)| *label).collect();
    let chosen = SPEEDS.iter().position(|(speed, _)| *speed == watching.speed());
    let origin = pos2(x, bar.center().y - theme.metric.chip / 2.0);
    let (clicked, _) = trill_noir::chip_row(ui, theme, origin, 400.0, id.with("speed"), &labels, chosen, false);
    if let Some(index) = clicked {
        watching.set_speed(SPEEDS[index].0);
    }
    let speeds = Rect::from_min_size(origin, vec2(trill_noir::chip_row_width(ui, theme, &labels), theme.metric.chip));
    if ui.rect_contains_pointer(speeds) {
        hint = Some("How fast the session plays again".to_owned());
    }
    x = speeds.right() + GAP;

    // Which step of how many.
    let (reached, steps) = watching.steps();
    let count = format!("Step {reached} of {steps}");
    let room = row(x).with_max_x(leave_rect.left() - GAP);
    if room.width() > 60.0 {
        text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.caps, &count, theme.ink_dim);
    }
    if let Some(hint) = hint {
        watching.hint().set(hint);
    }
    left
}

/// The bar along the bottom: what this is, how much of it there is, whose
/// marks are whose, and what the pointer is over.
pub fn status(ui: &mut Ui, theme: &Theme, making_of: &mut MakingOf) {
    let bar = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(bar, 0.0, theme.panel);
    paint::hline(&painter, bar.x_range(), bar.top(), theme.hairline);
    let mut x = bar.left() + EDGE;
    let rest = |x: f32| Rect::from_min_max(pos2(x, bar.top()), bar.max);
    let word = |x: &mut f32, color: egui::Color32, words: &str| {
        *x = paint::dot(&painter, pos2(*x, bar.center().y), theme.metric.dot, color) + 5.0;
        *x += text::paint(&painter, rest(*x), Align2::LEFT_CENTER, theme.text.micro, words, theme.ink_faint) + 16.0;
    };
    word(&mut x, theme.signal, "Making of");
    let Some(watching) = making_of.watching_mut() else { return };
    // Whose marks are whose, for the authors this session had.
    let mut authors: Vec<&Author> = Vec::new();
    for entry in watching.replay.journal().entries() {
        if !authors.contains(&&entry.by) {
            authors.push(&entry.by);
        }
    }
    for author in authors {
        word(&mut x, hue(theme, author), &who(author));
    }
    let journal = watching.replay.journal();
    if let (Some(first), Some(last)) = (journal.entries().next(), journal.last()) {
        let took = span(last.at.saturating_sub(first.at) as f64 / 1000.0);
        let words = format!("{took} of work in {}", span(watching.replay.length()));
        x += text::paint(&painter, rest(x), Align2::LEFT_CENTER, theme.text.micro, &words, theme.ink_faint) + 16.0;
    }
    let dropped = journal.dropped();
    if dropped.entries > 0 {
        let words = format!("{} earlier entries were let go", dropped.entries);
        x += text::paint(&painter, rest(x), Align2::LEFT_CENTER, theme.text.micro, &words, theme.warn) + 16.0;
    }
    if let Some(broken) = watching.replay.broken() {
        let words = format!("The journal does not add up past here: {broken}");
        x += text::paint(&painter, rest(x), Align2::LEFT_CENTER, theme.text.micro, &words, theme.warn) + 16.0;
    }
    let idle = "Space plays \u{00B7} Left and Right step \u{00B7} Esc leaves";
    let hint = if watching.state.hint.text().is_empty() { idle } else { watching.state.hint.text() };
    let room = Rect::from_min_max(pos2(x + 16.0, bar.top()), pos2(bar.right() - EDGE, bar.bottom()));
    text::paint(&painter, room, Align2::RIGHT_CENTER, theme.text.micro, hint, theme.ink_faint);
}

/// What stands where the song would while there is none to show: that the
/// journal is being read, or why there is nothing to watch.
pub fn waiting(ui: &mut Ui, theme: &Theme, making_of: &MakingOf) {
    let area = ui.max_rect();
    let painter = ui.painter();
    painter.rect_filled(area, 0.0, theme.bg);
    let words = making_of.nothing().unwrap_or("Reading the journal\u{2026}");
    let galley = trill_noir::prose::plain(ui, theme, theme.text.body, theme.ink_dim, words, (area.width() - 80.0).min(520.0));
    let size = galley.size();
    let origin = pos2(area.center().x - size.x / 2.0, area.center().y - size.y / 2.0);
    ui.interact(Rect::from_min_size(origin, size), ui.scope_id().with("making-of-waiting"), Sense::hover())
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, words));
    trill_noir::prose::paint(ui, origin, galley);
}

/// The glass over the views of the song: what the pointer does there is a
/// click on the replay, which plays or holds it, and nothing else.
pub fn glass(ui: &mut Ui, watching: &mut Watching, rect: Rect, id: Id) {
    let response = ui.interact(rect, id, Sense::click_and_drag());
    if response.clicked() {
        watching.toggle();
    }
    if response.hovered() {
        watching.hint().set("The song as it was \u{00B7} click to play or hold \u{00B7} nothing here can be edited");
    }
}

pub use story::show as story;
pub use strip::{HEIGHT as STRIP_HEIGHT, show as strip};

/// How wide the story is, as it comes and at its limits.
pub fn story_width() -> (f32, std::ops::RangeInclusive<f32>) {
    (STORY_WIDTH, STORY_LIMITS)
}

/// The heights of the two bars, which are the window's own.
pub fn bar_heights() -> (f32, f32) {
    (top_bar::HEIGHT, status_bar::HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_plainly() {
        assert_eq!(span(0.4), "0 s");
        assert_eq!(span(59.4), "59 s");
        assert_eq!(span(61.0), "1 min");
        assert_eq!(span(46.0 * 60.0 + 58.0), "46 min");
        assert_eq!(span(3.0 * 3600.0), "3 h");
        assert_eq!(span(3.0 * 3600.0 + 20.0 * 60.0 + 5.0), "3 h 20 min");
        assert_eq!(span(86_400.0 * 1.5), "1 day");
        assert_eq!(span(86_400.0 * 6.2), "6 days");
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(42.9), "0:42");
        assert_eq!(clock(190.0), "3:10");
        assert_eq!(who(&Author::Person), "You");
        assert_eq!(who(&Author::Client("claude-code".into())), "claude-code");
        assert_eq!(who(&Author::Client(String::new())), "A client");
    }
}
