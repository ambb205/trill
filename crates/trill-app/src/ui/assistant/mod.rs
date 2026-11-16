//! The assistant: a panel down the right side for talking to an AI about
//! the song that is open.
//!
//! It runs the user's own installed, signed-in Claude Code (through
//! `trill-assistant`, whose five rules bind this module too) against Trill's
//! own MCP server, so everything the assistant does is done with the same
//! tools any MCP client has, and shows up in the window as it happens. What
//! the panel adds to an outside client:
//!
//! * **It knows what the user is looking at.** Every message goes with a
//!   description of the selection, the playhead, the loop and the open tab
//!   ([`context`]).
//! * **One request, one undo step.** Everything changed while one message is
//!   answered is gathered in an undo group of the session and is one Undo,
//!   offered on the turn itself.
//! * **It can be watched and stopped.** The reply appears as it is written,
//!   each tool call is a line in plain words ([`tools`]), and Stop ends the
//!   turn.
//!
//! The rules, as they apply here: the panel never asks for, reads or stores
//! a credential and has no sign-in of its own (when the CLI is not signed
//! in, it says so in the back end's words and tells the user to run
//! `claude`); it starts whatever CLI the user installed; it relays what the
//! CLI says about limits and promises nothing itself; and the feature is
//! called the assistant, with "Claude Code" appearing only in sentences
//! that say what is being used.
//!
//! Modules: [`backend`] is the seam to what answers (the real CLI, or a
//! script for tests and pictures); [`transcript`] the conversation as shown
//! and kept; [`store`] what is kept between runs; [`view`] the panel
//! itself; [`scenes`] canned conversations for the screenshots. This file
//! is the part in between: whether the assistant is ready, the turn under
//! way and its undo group, and which conversation belongs to which project.
//!
//! Closed and idle, the assistant does nothing at all: [`Assistant::tick`]
//! is not called, and nothing was started that would need it.

mod backend;
mod context;
mod scenes;
mod store;
mod tools;
mod transcript;
mod view;

pub use scenes::Scene;
pub use view::{show, toggle_button};

use super::UiOptions;
use super::snapshot::{Live, Snapshot};
use super::state::UiState;
use backend::{Backend, Chat, Installed, McpServer, Nowhere, Problem, Ready};
use context::Context;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use store::Store;
use transcript::{Ending, Item, Offer, Transcript, TurnEnd};
use trill_assistant::{Event, FailureReason, LoginMethod, SessionId};
use trill_core::Project;
use trill_engine::SharedSession;
use trill_engine::session::{UndoGroup, UndoStep};
use trill_runtime::Runtime;

pub const DEFAULT_WIDTH: f32 = 360.0;
pub const MIN_WIDTH: f32 = 300.0;
pub const MAX_WIDTH: f32 = 560.0;
/// Width the rest of the window keeps however wide the panel is dragged.
pub const WINDOW_REST: f32 = 440.0;

/// How often the turn under way is looked at while the panel shows it: the
/// reply should read as being written, not as arriving in lumps.
const WATCH: Duration = Duration::from_millis(33);
/// What a turn limit's "Continue" sends.
const CONTINUE: &str = "Continue.";

/// How the assistant starts up.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Open the panel, whatever it was when the program last closed.
    pub open: bool,
    /// Show a canned conversation instead of talking to anything.
    pub scene: Option<Scene>,
    /// The model to ask the CLI for; `None` leaves the choice to the CLI,
    /// which is what the user has set up there.
    pub model: Option<String>,
    /// The MCP server was turned off on purpose (`--no-mcp`).
    pub mcp_off: bool,
}

/// Whether the assistant can be talked to.
#[derive(Clone, Debug, PartialEq)]
pub enum Readiness {
    /// Nobody has looked yet: the panel was never opened.
    Unknown,
    /// Looking for the CLI and for the server it works through.
    Checking,
    Ready(Ready),
    Unavailable(Problem),
}

/// A turn between its message and its end.
struct Turn {
    /// The undo group everything it changes is gathered in.
    group: Option<UndoGroup>,
    started: Instant,
    /// Stop was pressed; the end has not come yet.
    stopping: bool,
}

/// Who the conversation is billed to, when that is not the user's Claude
/// account. Always shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Billing {
    /// The CLI found an API key and is using it: where it found it.
    KeyFrom(String),
    /// The CLI is signed in with an API key.
    ApiKey,
    /// The CLI is signed in some other way, in its word for it.
    Other(String),
}

impl std::fmt::Display for Billing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Billing::KeyFrom(source) => write!(
                f,
                "Claude Code is using an API key ({source}), so this conversation is billed to that API account, \
                 not to your Claude login."
            ),
            Billing::ApiKey => f.write_str(
                "Claude Code is signed in with an API key, so this conversation is billed to that API account.",
            ),
            Billing::Other(method) => {
                write!(f, "Claude Code is signed in through {method}, so this conversation is billed there.")
            }
        }
    }
}

/// The steps Undo and Redo would take now, as of one revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct History {
    undo: Option<UndoStep>,
    redo: Option<UndoStep>,
    /// The snapshot revision this was read at.
    seen: Option<u64>,
}

/// What can be done about the change a turn made.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Reversal {
    /// It is the last thing that changed the song: Undo takes it back.
    Undo(UndoStep),
    /// It was taken back: Redo brings it back.
    Redo(UndoStep),
    /// Other changes were made since, or a turn is under way.
    Buried,
}

pub struct Assistant {
    backend: Box<dyn Backend>,
    /// The script behind a scripted back end, for whoever plays it.
    script: Option<backend::Script>,
    session: SharedSession,
    store: Store,
    open: bool,
    width: f32,
    /// The open state and width last handed to the store.
    kept_panel: (bool, f32),
    readiness: Readiness,
    /// Look again on the next tick.
    check_wanted: bool,
    chat: Option<Box<dyn Chat>>,
    /// The conversation's id: what continuing it another day takes.
    conversation: Option<SessionId>,
    /// The project the conversation is about: how many projects this
    /// window has had open, and the file. `None` before the first tick.
    project: Option<(u64, Option<PathBuf>)>,
    transcript: Transcript,
    /// A kept transcript that was asked for and has not arrived.
    awaiting: Option<SessionId>,
    turn: Option<Turn>,
    billing: Option<Billing>,
    /// The model answering, as the CLI names it.
    model: Option<String>,
    history: History,
    /// What is typed and not sent.
    draft: String,
    /// Messages sent in this window, for recalling with the arrow keys.
    sent: Vec<String>,
    /// Which of them the message field shows, while one is being recalled.
    recalled: Option<usize>,
    /// Give the message field the keyboard on the next frame.
    focus_composer: bool,
    /// Scroll the conversation to its end on the next frame.
    reveal_end: bool,
    /// Where the message field was on the last frame.
    field: egui::Rect,
    /// A canned conversation still to be played.
    scene: Option<Scene>,
    /// How long the turn under way claims to have run, when a picture must
    /// not depend on the clock.
    frozen_seconds: Option<u64>,
}

impl Assistant {
    pub fn new(ctx: &egui::Context, runtime: &Arc<Runtime>, options: &UiOptions) -> Self {
        let scene = options.assistant.scene;
        // A canned conversation keeps nothing: it is nobody's.
        let folder = if scene.is_some() { None } else { runtime.settings_folder() };
        let store = Store::open(folder.clone(), ctx);
        let (was_open, width) = store.panel();
        let server = match (&options.mcp_url, options.assistant.mcp_off) {
            (Some(url), _) => McpServer::Running(url.clone()),
            (None, true) => McpServer::Off,
            (None, false) => McpServer::Failed,
        };
        let script = scene.map(Scene::script);
        let backend: Box<dyn Backend> = match (&script, folder) {
            (Some(script), _) => Box::new(script.clone()),
            // The tests never start anybody's CLI: they bring a script.
            (None, Some(folder)) if !cfg!(test) => {
                Box::new(Installed::new(runtime.clone(), folder, options.assistant.model.clone(), server))
            }
            (None, _) => Box::new(Nowhere),
        };
        let open = options.assistant.open || scene.is_some() || was_open;
        let width = width.clamp(MIN_WIDTH, MAX_WIDTH);
        Self {
            backend,
            script,
            session: runtime.session.clone(),
            store,
            open,
            width,
            kept_panel: (was_open, width),
            readiness: Readiness::Unknown,
            check_wanted: false,
            chat: None,
            conversation: None,
            project: None,
            transcript: Transcript::default(),
            awaiting: None,
            turn: None,
            billing: None,
            model: None,
            history: History::default(),
            draft: String::new(),
            sent: Vec::new(),
            recalled: None,
            focus_composer: false,
            reveal_end: false,
            field: egui::Rect::NOTHING,
            scene,
            frozen_seconds: None,
        }
    }

    // ---- what the window asks ---------------------------------------------

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Shows the panel, with the keyboard in its message field, or puts it
    /// away. A turn under way carries on either way.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.focus_composer = self.open;
        // Remembered at once: closed, the panel is not looked at again.
        self.kept_panel = (self.open, self.width.round());
        self.store.set_panel(self.open, self.width.round());
    }

    /// The panel's width as last laid out, in points.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// Whether a turn is under way.
    pub fn is_working(&self) -> bool {
        self.turn.is_some()
    }

    /// Whether [`Self::tick`] has anything to do. Closed and idle, it has
    /// not, and is not called.
    pub fn wants_tick(&self) -> bool {
        self.open
            || self.turn.is_some()
            || self.readiness == Readiness::Checking
            || self.awaiting.is_some()
            || self.scene.is_some()
    }

    /// The part of a frame that needs no window: follows the project,
    /// collects what the back end and the store have ready, and takes in
    /// the events of the turn under way. Never waits for anything.
    pub fn tick(&mut self, ctx: &egui::Context, snapshot: &Snapshot, state: &UiState, live: &Live) {
        self.follow_project(state.project_generation, snapshot.path.as_ref());

        if self.open && self.readiness == Readiness::Unknown {
            self.check_wanted = true;
        }
        if std::mem::take(&mut self.check_wanted) && self.readiness != Readiness::Checking {
            self.readiness = Readiness::Checking;
            self.backend.check(ctx);
        }
        if self.readiness == Readiness::Checking
            && let Some(answer) = self.backend.checked()
        {
            self.take_answer(answer);
        }
        if let Some(scene) = self.scene.take() {
            scene.play(self, snapshot, state, live);
        }

        self.pump(&snapshot.project);

        if let Some((session, transcript)) = self.store.loaded()
            && self.awaiting.take_if(|awaited| *awaited == session).is_some()
            && self.transcript.is_empty()
            && let Some(transcript) = transcript
        {
            self.transcript = transcript;
        }

        self.read_history(snapshot.revision);

        // A turn reports through a queue nobody wakes the window for.
        if self.turn.is_some() && self.open {
            ctx.request_repaint_after(WATCH);
        }
        self.keep_panel(ctx);
    }

    // ---- readiness --------------------------------------------------------

    /// Looks again for the CLI and the server it works through.
    pub fn check_again(&mut self) {
        self.check_wanted = true;
    }

    fn take_answer(&mut self, answer: Result<Ready, Problem>) {
        // The server's address may have changed; a conversation is opened
        // again with the one that holds now.
        if self.turn.is_none() {
            self.chat = None;
        }
        self.readiness = match answer {
            Ok(ready) => {
                let login = match &ready.cli.login {
                    LoginMethod::Subscription { .. } => None,
                    LoginMethod::ApiKey => Some(Billing::ApiKey),
                    LoginMethod::Other(method) => Some(Billing::Other(method.clone())),
                };
                // What a turn reported about a key in use says more.
                if !matches!(self.billing, Some(Billing::KeyFrom(_))) {
                    self.billing = login;
                }
                Readiness::Ready(ready)
            }
            Err(problem) => Readiness::Unavailable(problem),
        };
    }

    // ---- the conversation ---------------------------------------------------

    /// Sends `message` with `context`, what the user is looking at, and
    /// starts the assistant's turn. Everything the turn changes will be one
    /// undo step. Returns whether it was sent: not while the assistant is
    /// unavailable or still answering, and not an empty message.
    pub fn submit(&mut self, message: &str, context: &Context) -> bool {
        let message = message.trim();
        let Readiness::Ready(ready) = &self.readiness else { return false };
        if message.is_empty() || self.turn.is_some() {
            return false;
        }
        if self.chat.is_none() {
            match self.backend.open(ready, self.conversation.clone()) {
                Ok(chat) => self.chat = Some(chat),
                Err(reason) => {
                    self.transcript.push_user(message);
                    self.transcript.end(TurnEnd {
                        ending: Ending::Failed {
                            text: format!("Trill could not start the conversation: {reason}."),
                            offer: Offer::Nothing,
                        },
                        changed: false,
                        step: None,
                    });
                    return false;
                }
            }
        }
        let Some(chat) = &mut self.chat else { return false };

        let group = self.session.lock().begin_group();
        if let Err(error) = chat.send(message, Some(&context.text)) {
            // Only a mistake in this file gets here; the group must not
            // stay open for it.
            let _ = self.session.lock().end_group(group);
            log::warn!("the assistant did not take the message: {error}");
            return false;
        }
        self.transcript.push_user(message);
        if self.sent.last().is_none_or(|last| last != message) {
            self.sent.push(message.to_owned());
        }
        self.recalled = None;
        self.awaiting = None;
        self.turn = Some(Turn { group: Some(group), started: Instant::now(), stopping: false });
        true
    }

    /// Stops the turn under way. It ends when the back end says it has:
    /// until then nothing new can be sent.
    pub fn stop(&mut self) {
        if let (Some(turn), Some(chat)) = (&mut self.turn, &mut self.chat)
            && !turn.stopping
        {
            turn.stopping = true;
            chat.cancel();
        }
    }

    /// Whether Stop was pressed and the turn has not ended yet.
    pub fn is_stopping(&self) -> bool {
        self.turn.as_ref().is_some_and(|turn| turn.stopping)
    }

    /// How long the turn under way has run, in whole seconds.
    pub fn working_for(&self) -> Option<u64> {
        let turn = self.turn.as_ref()?;
        Some(self.frozen_seconds.unwrap_or_else(|| turn.started.elapsed().as_secs()))
    }

    /// What a failed or stopped turn offers to send, if anything: the same
    /// message again, or a word to carry on.
    pub fn offered(&self, offer: Offer) -> Option<String> {
        match offer {
            Offer::Nothing => None,
            Offer::TryAgain => self.transcript.last_message().map(str::to_owned),
            Offer::Continue => Some(CONTINUE.to_owned()),
        }
    }

    /// Steps through the messages sent before, back or forward, and puts
    /// the one arrived at in the message field. Stepping forward past the
    /// last empties the field again. Returns whether the field changed.
    fn recall(&mut self, back: bool) -> bool {
        let next = match (self.recalled, back) {
            (None, true) => self.sent.len().checked_sub(1),
            (None, false) => return false,
            (Some(index), true) => Some(index.saturating_sub(1)),
            (Some(index), false) => (index + 1 < self.sent.len()).then_some(index + 1),
        };
        if next == self.recalled {
            return false;
        }
        self.recalled = next;
        self.draft = next.and_then(|index| self.sent.get(index)).cloned().unwrap_or_default();
        true
    }

    /// Starts over: the assistant remembers nothing of what was said, and
    /// the panel is cleared. Not while a turn is under way.
    pub fn new_conversation(&mut self) {
        if self.turn.is_some() {
            return;
        }
        self.chat = None;
        self.conversation = None;
        self.awaiting = None;
        self.transcript = Transcript::default();
        if let Some((_, Some(path))) = &self.project {
            self.store.forget(path);
        }
        self.focus_composer = true;
    }

    /// Takes in what the turn under way has reported since the last look.
    fn pump(&mut self, project: &Project) {
        // Until the turn has ended: what follows its end is the next turn's.
        while self.turn.is_some()
            && let Some(event) = self.chat.as_mut().and_then(|chat| chat.poll())
        {
            match event {
                Event::Started(start) => {
                    self.model = Some(start.model);
                    if let Some(source) = start.api_key_source {
                        self.billing = Some(Billing::KeyFrom(source));
                    }
                    self.adopt(start.session_id);
                }
                Event::Finished(summary) => self.finish(transcript::finished(&summary)),
                Event::Cancelled => self.finish(Ending::Stopped),
                Event::Failed(reason) => self.fail(&reason),
                other => self.transcript.apply(&other, project),
            }
        }
    }

    /// The conversation goes by `session` from now on.
    fn adopt(&mut self, session: SessionId) {
        if self.conversation.as_ref() == Some(&session) {
            return;
        }
        if let Some((_, Some(path))) = &self.project {
            self.store.remember(path, &session);
        }
        self.conversation = Some(session);
    }

    fn fail(&mut self, reason: &FailureReason) {
        let text = transcript::failure_text(reason, SystemTime::now());
        self.finish(Ending::Failed { text, offer: transcript::offer_for(reason) });
        if transcript::calls_for_a_check(reason) {
            // What the check finds says what is wrong now, and whether it
            // still is.
            self.check_wanted = true;
        }
        if *reason == FailureReason::SessionNotFound {
            // The CLI has let go of the conversation; the next message
            // starts a new one.
            self.conversation = None;
            if let Some((_, Some(path))) = &self.project {
                self.store.forget(path);
            }
        }
    }

    /// Ends the turn: its undo group is closed, however the turn went, and
    /// the conversation so far is kept.
    fn finish(&mut self, ending: Ending) {
        let Some(turn) = self.turn.take() else { return };
        let step = turn.group.and_then(|group| self.session.lock().end_group(group));
        self.transcript.end(TurnEnd { ending, changed: step.is_some(), step });
        // The back end's word on which conversation this is, should the
        // start of the turn have gone unseen.
        if let Some(session) = self.chat.as_ref().and_then(|chat| chat.session_id()) {
            self.adopt(session);
        }
        self.history.seen = None;
        self.frozen_seconds = None;
        if let Some(session) = &self.conversation {
            self.store.keep_transcript(session, &self.transcript);
        }
    }

    // ---- which conversation belongs to which project ----------------------

    /// Keeps the conversation with its project. `generation` counts the
    /// projects this window has had open; `path` is the open one's file.
    fn follow_project(&mut self, generation: u64, path: Option<&PathBuf>) {
        match &self.project {
            Some((known, file)) if *known == generation => {
                // The same project under another name: it was saved for
                // the first time, or saved as. The conversation goes along.
                if file.as_ref() != path {
                    if let (Some(path), Some(session)) = (path, &self.conversation) {
                        self.store.remember(path, session);
                        self.store.keep_transcript(session, &self.transcript);
                    }
                    self.project = Some((generation, path.cloned()));
                }
            }
            // Another project was opened. A turn under way was about the
            // one that is gone: it is stopped, and the panel changes over
            // once it has ended.
            Some(_) if self.turn.is_some() => self.stop(),
            _ => {
                self.project = Some((generation, path.cloned()));
                self.chat = None;
                self.transcript = Transcript::default();
                self.conversation = path.and_then(|path| self.store.conversation(path));
                self.awaiting = self.conversation.clone();
                if let Some(session) = &self.conversation {
                    self.store.load_transcript(session);
                }
            }
        }
    }

    // ---- undoing what a turn changed --------------------------------------

    /// Finds out which steps Undo and Redo would take, when the project has
    /// changed since that was last asked and a turn's change is on show.
    fn read_history(&mut self, revision: u64) {
        if self.history.seen == Some(revision) {
            return;
        }
        let shows_a_step =
            self.transcript.items.iter().any(|item| matches!(item, Item::End(end) if end.step.is_some()));
        if !shows_a_step {
            return;
        }
        // If the session is busy this frame, the next one asks again.
        if let Some(session) = self.session.try_lock() {
            self.history = History { undo: session.undo_step(), redo: session.redo_step(), seen: Some(revision) };
        }
    }

    /// What can be done now about the change `step` holds.
    pub fn reversal(&self, step: UndoStep) -> Reversal {
        if self.turn.is_some() {
            Reversal::Buried
        } else if self.history.undo == Some(step) {
            Reversal::Undo(step)
        } else if self.history.redo == Some(step) {
            Reversal::Redo(step)
        } else {
            Reversal::Buried
        }
    }

    /// Takes back, or brings back, everything one turn changed. Nothing
    /// happens if the step is no longer the one Undo or Redo would take.
    pub fn reverse(&mut self, reversal: Reversal) {
        let mut session = self.session.lock();
        match reversal {
            Reversal::Undo(step) if session.undo_step() == Some(step) => drop(session.undo()),
            Reversal::Redo(step) if session.redo_step() == Some(step) => drop(session.redo()),
            _ => {}
        }
        self.history = History { undo: session.undo_step(), redo: session.redo_step(), seen: None };
    }

    // ---- between runs -----------------------------------------------------

    /// Remembers whether the panel is open and how wide, once the width has
    /// stopped being dragged.
    fn keep_panel(&mut self, ctx: &egui::Context) {
        let now = (self.open, self.width.round());
        if now != self.kept_panel && !ctx.input(|input| input.pointer.any_down()) {
            self.kept_panel = now;
            self.store.set_panel(now.0, now.1);
        }
    }
}

impl Drop for Assistant {
    /// A turn under way ends with the window: the CLI is stopped, the undo
    /// group closed, and the conversation kept as far as it got.
    fn drop(&mut self) {
        if self.turn.is_some() {
            if let Some(chat) = &mut self.chat {
                chat.cancel();
            }
            self.finish(Ending::Stopped);
        }
        self.store.set_panel(self.open, self.width.round());
    }
}

#[cfg(test)]
impl Assistant {
    /// Puts a scripted back end in place of whatever the window started
    /// with, as if nothing had been looked at yet.
    pub fn use_script(&mut self, script: backend::Script) {
        self.backend = Box::new(script.clone());
        self.script = Some(script);
        self.chat = None;
        self.readiness = Readiness::Unknown;
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn conversation(&self) -> Option<&SessionId> {
        self.conversation.as_ref()
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// Where the message field was on the last frame.
    pub fn field(&self) -> egui::Rect {
        self.field
    }
}

#[cfg(test)]
pub(in crate::ui) mod testing {
    //! What the interaction tests need of the assistant's insides.
    pub use super::backend::{Problem, Script, Sent, ready};
    pub use super::transcript::{Ending, Item, Offer, ToolState};
}
