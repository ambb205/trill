//! The journal of a session: how a song came to be, step by step.
//!
//! A [`Journal`] begins with a song as it was (its *base*) and holds, in the
//! order they happened, the entries of the story since:
//!
//! * **steps**, each a change to the song with who made it, when, what it
//!   was in words and where in the song it landed ([`Step`]);
//! * what was **said** between a person and the assistant, and the
//!   assistant's **plan** as it stood;
//! * the **looks** a client took without changing anything: reading the
//!   project, trying out an instrument, measuring a mix, with what came back
//!   in short ([`Look`]);
//! * **marks** about the session itself.
//!
//! The song as it was after any entry is the base with the steps up to
//! there applied ([`Journal::song_after`]), and [`replay`] turns the whole
//! into something that can be watched.
//!
//! **What a step keeps** is what it changed and no more ([`Change`]); a
//! device's state is held once however many steps it lasts through.
//!
//! **Who made a step.** A tool call of an MCP client or of the assistant is
//! *opened* before it runs and *closed* after ([`Journal::begin_call`],
//! [`Journal::end_call`]): everything it changes in between is one step,
//! under the words the call goes by. An edit made at the window is a step
//! of its own, called by what it changed ([`words::describe`]); the edits of
//! one drag are one step.
//!
//! **Undo is part of the story.** Nothing is ever taken out of a journal by
//! undoing: a step taken back is followed by a step that takes it back
//! (*Took back: Added the track "Bass"*), and redoing it by one that brings
//! it back. What happened, happened, and is seen to. The one exception is a
//! call that backs out of its own unfinished work, which leaves no trace
//! ([`Journal::retract`]).
//!
//! **It is bounded.** Past [`Limits`], the oldest entries are folded into
//! the base: the story then begins later, with the song as it was there,
//! and [`Journal::dropped`] says how much went.
//!
//! This module is the model: it reads no clock and touches no file. The
//! session records into it (`trill-engine`), and its text
//! ([`Journal::to_text`]) is what is kept on disk: in the program's own
//! folder, never in the project file, because it holds a conversation.

mod change;
pub mod place;
pub mod replay;
pub mod words;

pub use change::{BlobId, Blobs, Broken, Change, Op, StateChange, States};
pub use place::{Focus, Place};

use crate::{ChainRef, Device, DeviceId, Project};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The version of the journal's text this build writes.
pub const FORMAT_VERSION: u32 = 1;

/// Who made an entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    /// Someone at the window.
    Person,
    /// The assistant in the window.
    Assistant,
    /// A client of the MCP server, by the name it gave.
    Client(String),
}

impl Author {
    /// Who, in a word or two: "you", "the assistant", the client's name.
    pub fn name(&self) -> &str {
        match self {
            Author::Person => "you",
            Author::Assistant => "the assistant",
            Author::Client(name) if name.is_empty() => "a client",
            Author::Client(name) => name,
        }
    }
}

/// Whether a step takes earlier steps back or brings them back.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversal {
    Undo,
    Redo,
}

/// The steps a step reverses: from the entry numbered `first` to the one
/// numbered `last`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reversed {
    pub kind: Reversal,
    pub first: u64,
    pub last: u64,
}

/// A change to the song.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// What it was, in words: "Added the track \u{201C}Bass\u{201D}".
    pub label: String,
    /// Where in the song it landed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<Place>,
    /// The tool that made it, for a call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// What the call heard, in short, if it also listened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard: Option<String>,
    /// For a step that takes steps back or brings them back: which.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverses: Option<Reversed>,
    /// A step nobody made: what a device came to on its own in its
    /// editor's window, kept so that the song as it was can be rebuilt as
    /// it was. It is not shown as part of the story and takes no time in a
    /// replay.
    #[serde(default, skip_serializing_if = "is_false")]
    pub aside: bool,
    pub change: Change,
}

/// A call that changed nothing in the song.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Look {
    /// What it was, in words: "Measured the song".
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// What came back, in short, for a call that listened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard: Option<String>,
    /// The call was refused or went wrong.
    #[serde(default, skip_serializing_if = "is_false")]
    pub failed: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// How a step of the assistant's plan stands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    Pending,
    Doing,
    Done,
}

/// A step of the assistant's plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub text: String,
    pub state: PlanState,
}

/// What an entry is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum What {
    /// The song changed.
    Step(Step),
    /// Something said in the conversation with the assistant: by the
    /// person, their message; by the assistant, a passage of its reply.
    Said { text: String },
    /// The assistant's plan, as it stood from here on.
    Plan { items: Vec<PlanItem> },
    /// A call that changed nothing: a look at the song, or a listen.
    Look(Look),
    /// Something about the session itself: a request ended, the song was
    /// changed while no journal was kept.
    Mark { label: String },
}

/// What may still be added to an entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Open {
    /// The call that made it has not ended.
    Call(u64),
    /// The drag that made it may go on.
    Gesture,
}

/// One entry of the story.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Its number, counted from the beginning of the journal; it keeps it
    /// when older entries are let go.
    pub seq: u64,
    /// When, in milliseconds since 1970.
    pub at: u64,
    pub by: Author,
    pub what: What,
    #[serde(skip)]
    open: Option<Open>,
    /// How many bytes it takes written down, once that has been counted.
    #[serde(skip)]
    size: usize,
}

impl PartialEq for Entry {
    /// The same entry, whatever is known of it only while a session runs.
    fn eq(&self, other: &Self) -> bool {
        (self.seq, self.at, &self.by, &self.what) == (other.seq, other.at, &other.by, &other.what)
    }
}

impl Entry {
    /// The step, if the entry is one.
    pub fn step(&self) -> Option<&Step> {
        match &self.what {
            What::Step(step) => Some(step),
            _ => None,
        }
    }

    /// What it is in a line: a step's or a look's words, what was said, the
    /// plan's size.
    pub fn line(&self) -> String {
        match &self.what {
            What::Step(step) => step.label.clone(),
            What::Look(look) => look.label.clone(),
            What::Said { text } => text.clone(),
            What::Plan { items } => {
                let done = items.iter().filter(|item| item.state == PlanState::Done).count();
                format!("Plan: {done} of {} done", items.len())
            }
            What::Mark { label } => label.clone(),
        }
    }

    /// Whether it is an aside: a step nobody made, which is not shown.
    pub fn is_aside(&self) -> bool {
        self.step().is_some_and(|step| step.aside)
    }

    /// Whether it is part of a conversation: what would hand someone the
    /// words exchanged with the assistant.
    pub fn is_conversation(&self) -> bool {
        matches!(self.what, What::Said { .. } | What::Plan { .. })
    }
}

/// Names a call between its beginning and its end.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct CallId(u64);

#[derive(Clone)]
struct OpenCall {
    id: u64,
    by: Author,
    tool: String,
    label: String,
    /// How many entries it has made.
    entries: u32,
}

/// Who is making a step.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Who {
    /// Someone at the window, in one edit.
    Person,
    /// Someone at the window, in a drag: `continues` after its first edit.
    Gesture { continues: bool },
    /// A call that is open.
    Call(CallId),
}

/// Where in the journal a recorded edit went: which entry, and which of
/// that entry's parts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub seq: u64,
    from: usize,
    to: usize,
}

impl Mark {
    /// The mark of this edit and `later`, the next one, together: `None`
    /// unless the later one went to the same entry right after this one.
    pub fn through(self, later: Mark) -> Option<Mark> {
        (self.seq == later.seq && self.to == later.from).then_some(Mark { seq: self.seq, from: self.from, to: later.to })
    }
}

/// The calls that are open, for taking along when another song's journal
/// takes the place of this one ([`Journal::take_calls`]).
#[derive(Default)]
pub struct OpenCalls {
    calls: Vec<OpenCall>,
    last: u64,
}

/// What a call came to, said when it ends.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// Where what it made or changed is, if the call itself says; else its
    /// steps keep what was read from their changes.
    pub places: Vec<Place>,
    /// What it heard, in short.
    pub heard: Option<String>,
    /// It was refused or went wrong.
    pub failed: bool,
}

/// How much of a journal was let go to keep it small.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropped {
    /// How many entries.
    pub entries: u64,
    /// How many of them were steps.
    pub steps: u64,
    /// When the last of them happened: the story now begins after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
}

/// How large a journal may grow before its oldest entries are let go.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most entries.
    pub entries: usize,
    /// The most bytes the story may add to the song it began with: its
    /// entries written down, and the device states only they name.
    pub bytes: usize,
}

impl Default for Limits {
    /// Twenty thousand entries or 32 MB, whichever comes first: weeks of
    /// work on a song, and a file that is still read in a moment.
    fn default() -> Self {
        Self { entries: 20_000, bytes: 32 << 20 }
    }
}

/// What a file holds of a journal, for adding to it what it lacks
/// ([`Journal::text_since`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// Which shape of the journal the file is of: letting go of old
    /// entries makes another, and the file is then written anew.
    generation: Option<u64>,
    /// The number of the first entry the file lacks.
    next_seq: u64,
    blobs: BTreeSet<BlobId>,
}

/// What has to be written to bring a file up to date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Writing {
    Nothing,
    /// These lines, after what the file holds.
    Append(String),
    /// The file, anew.
    Whole(String),
}

/// One line of a journal's text, as it is read.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Line {
    Journal { version: u32, began: u64, dropped: Dropped },
    Base { song: Box<Project>, states: BTreeMap<DeviceId, BlobId> },
    Blob { id: BlobId, data: String },
    Entry(Box<Entry>),
}

/// One line of a journal's text, as it is written: the same, without a
/// copy of what it writes.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum LineOut<'a> {
    Journal { version: u32, began: u64, dropped: Dropped },
    Base { song: &'a Project, states: &'a BTreeMap<DeviceId, BlobId> },
    Blob { id: BlobId, data: String },
    Entry(&'a Entry),
}

fn line(line: &LineOut<'_>) -> String {
    let mut text = serde_json::to_string(line).expect("a journal is always serializable");
    text.push('\n');
    text
}

/// How a song came to be; see the [module](self).
#[derive(Clone)]
pub struct Journal {
    /// The song as it was when the story begins, without its devices'
    /// states, which `base_states` names.
    base: Project,
    base_states: BTreeMap<DeviceId, BlobId>,
    blobs: Blobs,
    entries: VecDeque<Entry>,
    /// The number the next entry is given.
    next_seq: u64,
    began: u64,
    dropped: Dropped,
    limits: Limits,
    calls: Vec<OpenCall>,
    last_call: u64,
    /// Moves on whenever entries a file may already hold are let go.
    generation: u64,
    /// How many bytes the base takes written down; counted when asked.
    base_size: Option<usize>,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("entries", &self.entries.len())
            .field("next_seq", &self.next_seq)
            .field("dropped", &self.dropped)
            .field("blobs", &self.blobs.len())
            .finish_non_exhaustive()
    }
}

impl Journal {
    /// A journal that begins `at` this moment with `song` as it is.
    pub fn begin(song: &Project, at: u64) -> Self {
        let mut blobs = Blobs::default();
        let (base, base_states) = change::without_states(song, &mut blobs);
        Self {
            base,
            base_states,
            blobs,
            entries: VecDeque::new(),
            next_seq: 1,
            began: at,
            dropped: Dropped::default(),
            limits: Limits::default(),
            calls: Vec::new(),
            last_call: 0,
            generation: 0,
            base_size: None,
        }
    }

    /// Holds the journal to other limits than the usual.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    // ---- reading --------------------------------------------------------

    /// When the story begins, in milliseconds since 1970: when the journal
    /// was begun, or when the last entry that was let go happened.
    pub fn began(&self) -> u64 {
        self.dropped.until.unwrap_or(self.began)
    }

    /// How much was let go to keep the journal small.
    pub fn dropped(&self) -> Dropped {
        self.dropped
    }

    /// The entries, oldest first.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &Entry> + DoubleEndedIterator + '_ {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry at `index`, counted from the oldest that is held.
    pub fn get(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// The last entry.
    pub fn last(&self) -> Option<&Entry> {
        self.entries.back()
    }

    /// How many steps are held, asides not counted.
    pub fn steps(&self) -> usize {
        self.entries.iter().filter(|entry| entry.step().is_some_and(|step| !step.aside)).count()
    }

    /// The number of the oldest entry held, or of the next one if none is.
    pub fn first_seq(&self) -> u64 {
        self.entries.front().map_or(self.next_seq, |entry| entry.seq)
    }

    /// The number the next entry will have.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Where the entry numbered `seq` is among those held.
    pub fn index_of(&self, seq: u64) -> Option<usize> {
        let index = usize::try_from(seq.checked_sub(self.first_seq())?).ok()?;
        (index < self.entries.len()).then_some(index)
    }

    /// The song as it was when the story begins.
    pub fn base(&self, states: States<'_>) -> Result<Project, Broken> {
        let mut song = self.base.clone();
        if let States::With(blobs) = states {
            change::with_states(&mut song, &self.base_states, blobs)?;
        }
        Ok(song)
    }

    /// The device states the journal holds: what [`States::With`] is given.
    pub fn blobs(&self) -> &Blobs {
        &self.blobs
    }

    /// The song as it was after the first `count` entries held: the base
    /// for none, the song as it is for all of them.
    pub fn song_after(&self, count: usize, states: States<'_>) -> Result<Project, Broken> {
        let mut song = self.base(states)?;
        for entry in self.entries.iter().take(count) {
            if let What::Step(step) = &entry.what {
                step.change.apply(&mut song, states)?;
            }
        }
        Ok(song)
    }

    /// The song as it was after the entry numbered `seq`. A number from
    /// before the story begins gives the song as it was at the beginning.
    pub fn song_at(&self, seq: u64, states: States<'_>) -> Result<Project, Broken> {
        let count = seq.saturating_add(1).saturating_sub(self.first_seq());
        self.song_after(usize::try_from(count).unwrap_or(usize::MAX), states)
    }

    /// The song as the journal has it now, states and all.
    pub fn head(&self) -> Result<Project, Broken> {
        self.song_after(self.entries.len(), States::With(&self.blobs))
    }

    // ---- recording ------------------------------------------------------

    fn push(&mut self, at: u64, by: Author, what: What, open: Option<Open>) -> u64 {
        // A drag's entry can only grow while it is the last.
        if let Some(last) = self.entries.back_mut()
            && last.open == Some(Open::Gesture)
        {
            last.open = None;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push_back(Entry { seq, at, by, what, open, size: 0 });
        seq
    }

    /// Records that the song went from `before` to `after`, at the hands
    /// of `who`. The edits of one call, and those of one drag, that follow
    /// one another are one step. `label` are the words for it where the
    /// maker has better ones than what the change reads as; a call's steps
    /// go by the call's.
    ///
    /// `None` if the two are the same song.
    pub fn record(
        &mut self,
        at: u64,
        who: Who,
        before: &Project,
        after: &Project,
        label: Option<&str>,
    ) -> Option<Mark> {
        let change = Change::between(before, after, &mut self.blobs);
        if change.is_empty() {
            return None;
        }
        let open = match who {
            Who::Person => None,
            Who::Gesture { .. } => Some(Open::Gesture),
            Who::Call(call) => self.calls.iter().any(|open| open.id == call.0).then_some(Open::Call(call.0)),
        };
        let grows = match who {
            Who::Person | Who::Gesture { continues: false } => false,
            Who::Gesture { continues: true } | Who::Call(_) => true,
        };
        if grows
            && let Some(last) = self.entries.back_mut()
            && last.open.is_some()
            && last.open == open
            && let What::Step(step) = &mut last.what
            && step.reverses.is_none()
        {
            let from = step.change.len();
            for place in place::of_change(&change, after) {
                if !step.places.contains(&place) {
                    step.places.push(place);
                }
            }
            step.change.then(change);
            last.size = 0;
            return Some(Mark { seq: last.seq, from, to: step.change.len() });
        }

        let call = match open {
            Some(Open::Call(id)) => self.calls.iter_mut().find(|call| call.id == id),
            _ => None,
        };
        let (by, label, tool) = match call {
            Some(call) => {
                call.entries += 1;
                (call.by.clone(), call.label.clone(), Some(call.tool.clone()))
            }
            None => {
                let label = label.map_or_else(|| words::describe(before, after, &change), str::to_owned);
                (Author::Person, label, None)
            }
        };
        let to = change.len();
        let places = place::of_change(&change, after);
        let step = Step { label, places, tool, heard: None, reverses: None, aside: false, change };
        let seq = self.push(at, by, What::Step(step), open);
        Some(Mark { seq, from: 0, to })
    }

    /// Records that steps were taken back or brought back: the song went
    /// from `before` to `after` by undoing or redoing the entries numbered
    /// `first` to `last`, which were `steps` steps whose first went by
    /// `label`.
    #[allow(clippy::too_many_arguments)]
    pub fn reverse(
        &mut self,
        at: u64,
        who: Who,
        kind: Reversal,
        (first, last): (u64, u64),
        (label, steps): (&str, u32),
        before: &Project,
        after: &Project,
    ) -> Option<Mark> {
        let change = Change::between(before, after, &mut self.blobs);
        if change.is_empty() {
            return None;
        }
        let verb = match kind {
            Reversal::Undo => "Took back",
            Reversal::Redo => "Brought back",
        };
        let label = match steps {
            0 | 1 => format!("{verb}: {label}"),
            steps => format!("{verb} {steps} steps, from: {label}"),
        };
        let call = match who {
            Who::Call(call) => self.calls.iter_mut().find(|open| open.id == call.0),
            _ => None,
        };
        let (by, tool, open) = match call {
            Some(call) => {
                call.entries += 1;
                (call.by.clone(), Some(call.tool.clone()), Some(Open::Call(call.id)))
            }
            None => (Author::Person, None, None),
        };
        let to = change.len();
        let places = place::of_change(&change, after);
        let reverses = Some(Reversed { kind, first, last });
        let step = Step { label, places, tool, heard: None, reverses, aside: false, change };
        let seq = self.push(at, by, What::Step(step), open);
        Some(Mark { seq, from: 0, to })
    }

    /// Takes a recorded edit out again, as if it had not been made: for a
    /// maker that backs out of its own unfinished work. Only the edit the
    /// journal ends with can be taken out; `false`, and nothing done, for
    /// any other.
    pub fn retract(&mut self, mark: Mark) -> bool {
        let Some(last) = self.entries.back_mut() else { return false };
        let What::Step(step) = &mut last.what else { return false };
        if last.seq != mark.seq || step.change.len() != mark.to || step.reverses.is_some() {
            return false;
        }
        step.change.truncate(mark.from);
        last.size = 0;
        if step.change.is_empty() {
            if let Some(Open::Call(id)) = last.open
                && let Some(call) = self.calls.iter_mut().find(|call| call.id == id)
            {
                call.entries = call.entries.saturating_sub(1);
            }
            self.entries.pop_back();
            self.next_seq -= 1;
        }
        true
    }

    /// Whether the entry numbered `seq` was made by `call`, which is still
    /// open.
    pub fn is_of_call(&self, seq: u64, call: CallId) -> bool {
        self.index_of(seq)
            .and_then(|index| self.entries.get(index))
            .is_some_and(|entry| entry.open == Some(Open::Call(call.0)))
    }

    /// The drag that was under way is over: its step is as it stays.
    pub fn end_gesture(&mut self) {
        if let Some(last) = self.entries.back_mut()
            && last.open == Some(Open::Gesture)
        {
            last.open = None;
        }
    }

    /// Opens a call of `tool` by `by`: what changes at its hands until
    /// [`Journal::end_call`] goes by `label`.
    pub fn begin_call(&mut self, by: Author, tool: &str, label: impl Into<String>) -> CallId {
        self.last_call += 1;
        self.calls.push(OpenCall { id: self.last_call, by, tool: tool.to_owned(), label: label.into(), entries: 0 });
        CallId(self.last_call)
    }

    /// Whether `call` has begun and not ended.
    pub fn is_open(&self, call: CallId) -> bool {
        self.calls.iter().any(|open| open.id == call.0)
    }

    /// Hands over the calls that are open: another song is taking this
    /// one's place in the middle of them. What they made here stands as
    /// it is.
    pub fn take_calls(&mut self) -> OpenCalls {
        for entry in self.entries.iter_mut().filter(|entry| matches!(entry.open, Some(Open::Call(_)))) {
            entry.open = None;
        }
        let calls = std::mem::take(&mut self.calls);
        OpenCalls { calls: calls.into_iter().map(|call| OpenCall { entries: 0, ..call }).collect(), last: self.last_call }
    }

    /// Takes on calls that were opened in the journal of the song this
    /// one's song replaced: what they do from here on is entered here.
    pub fn adopt_calls(&mut self, open: OpenCalls) {
        self.last_call = self.last_call.max(open.last);
        self.calls.extend(open.calls);
    }

    /// Closes a call. Its steps are told where they landed, if the call
    /// says; one that changed nothing is entered as a look.
    pub fn end_call(&mut self, at: u64, call: CallId, outcome: Outcome) {
        let Some(index) = self.calls.iter().position(|open| open.id == call.0) else { return };
        let OpenCall { id, by, tool, label, entries } = self.calls.remove(index);
        if entries == 0 {
            let look = Look { label, tool: Some(tool), heard: outcome.heard, failed: outcome.failed };
            self.push(at, by, What::Look(look), None);
            return;
        }
        let mut heard = outcome.heard;
        let mut left = entries;
        for entry in self.entries.iter_mut().rev() {
            if left == 0 {
                break;
            }
            if entry.open != Some(Open::Call(id)) {
                continue;
            }
            entry.open = None;
            entry.size = 0;
            left -= 1;
            if let What::Step(step) = &mut entry.what {
                if !outcome.places.is_empty() {
                    step.places.clone_from(&outcome.places);
                }
                // On the last of its steps, which is the first met here.
                if let Some(heard) = heard.take() {
                    step.heard = Some(heard);
                }
            }
        }
    }

    /// Enters what was said in the conversation.
    pub fn say(&mut self, at: u64, by: Author, text: impl Into<String>) -> u64 {
        self.push(at, by, What::Said { text: text.into() }, None)
    }

    /// Enters the assistant's plan as it stands.
    pub fn plan(&mut self, at: u64, items: Vec<PlanItem>) -> u64 {
        self.push(at, Author::Assistant, What::Plan { items }, None)
    }

    /// Enters something about the session itself.
    pub fn mark(&mut self, at: u64, by: Author, label: impl Into<String>) -> u64 {
        self.push(at, by, What::Mark { label: label.into() }, None)
    }

    /// Enters, as an aside ([`Step::aside`]), what a device came to on
    /// its own in its editor's window, which is not an edit of anybody's:
    /// `device` as it is now, in `chain`, with its state if
    /// `state_changed`. One that follows another for the same device takes
    /// its place: a state is megabytes, and a knob turned for a minute
    /// would otherwise be kept at every look.
    pub fn note_device(&mut self, at: u64, chain: ChainRef, device: &Device, state_changed: bool, label: &str) {
        let change = Change::of_device(chain, device, state_changed, &mut self.blobs);
        if let Some(last) = self.entries.back_mut()
            && last.open.is_none()
            && let What::Step(step) = &mut last.what
            && step.tool.is_none()
            && step.label == label
            && step.change.is_of_device(device.id)
        {
            step.change.follow_device(change);
            last.at = at;
            last.size = 0;
            // A file that holds the entry as it was is written anew, and
            // a state only it named is let go.
            self.generation += 1;
            self.sweep();
            return;
        }
        let places = vec![Place::Device { id: device.id.0, name: device.name.clone() }];
        let step =
            Step { label: label.to_owned(), places, tool: None, heard: None, reverses: None, aside: true, change };
        self.push(at, Author::Person, What::Step(step), None);
    }

    // ---- staying small --------------------------------------------------

    /// How many entries, from the oldest, are as they will stay.
    fn settled(&self) -> usize {
        self.entries.iter().position(|entry| entry.open.is_some()).unwrap_or(self.entries.len())
    }

    /// Counts the bytes of every settled entry that has not been counted.
    fn measure(&mut self) {
        let settled = self.settled();
        for entry in self.entries.iter_mut().take(settled).filter(|entry| entry.size == 0) {
            entry.size = line(&LineOut::Entry(entry)).len();
        }
    }

    /// The states only the story names: not the song's as it began.
    fn story_blobs(&self) -> BTreeSet<BlobId> {
        let base: BTreeSet<BlobId> = self.base_states.values().copied().collect();
        let steps = self.entries.iter().filter_map(Entry::step);
        steps.flat_map(|step| step.change.blobs()).filter(|id| !base.contains(id)).collect()
    }

    /// How many bytes the story adds to the song it began with: its
    /// entries written down, and the states only they name as a file holds
    /// them.
    pub fn story_bytes(&mut self) -> usize {
        self.measure();
        let entries: usize = self.entries.iter().map(|entry| entry.size).sum();
        let states: usize = self.story_blobs().iter().filter_map(|id| self.blobs.get(*id)).map(|held| held.len()).sum();
        entries + states.div_ceil(3) * 4
    }

    /// How many bytes the whole journal takes written down, the song it
    /// began with included.
    pub fn bytes(&mut self) -> usize {
        let base = match self.base_size {
            Some(size) => size,
            None => {
                let song = serde_json::to_string(&self.base).map_or(0, |text| text.len());
                let states: usize =
                    self.base_states.values().filter_map(|id| self.blobs.get(*id)).map(|held| held.len()).sum();
                *self.base_size.insert(song + states.div_ceil(3) * 4)
            }
        };
        base + self.story_bytes()
    }

    /// Folds the oldest entry into the base. `false` if there is none that
    /// is settled, or if its change does not apply.
    fn fold_oldest(&mut self) -> bool {
        if self.settled() == 0 {
            return false;
        }
        let Some(entry) = self.entries.front() else { return false };
        if let What::Step(step) = &entry.what {
            if step.change.apply(&mut self.base, States::Without).is_err() {
                return false;
            }
            for op in step.change.ops() {
                if let Op::Device { device, state, .. } = op {
                    match state {
                        StateChange::Keep => {}
                        StateChange::Clear => drop(self.base_states.remove(&device.id)),
                        StateChange::Set(id) => drop(self.base_states.insert(device.id, *id)),
                    }
                }
            }
            self.dropped.steps += 1;
        }
        self.dropped.entries += 1;
        self.dropped.until = Some(entry.at);
        self.entries.pop_front();
        true
    }

    /// Lets go of the oldest entries if the journal is over its limits,
    /// until it is three quarters of the way to them: the song as it was
    /// after them is where the story then begins. Returns how many went.
    ///
    /// What goes first is the oldest, whatever it is: steps, what was said
    /// then, looks. The song loses nothing; the story loses its beginning.
    pub fn keep_within_limits(&mut self) -> usize {
        let Limits { entries, bytes } = self.limits;
        let too_many = self.entries.len() > entries;
        if !too_many && self.story_bytes() <= bytes {
            return 0;
        }
        let (entries, bytes) = (entries / 4 * 3, bytes / 4 * 3);
        let mut folded = 0;
        while too_many && self.entries.len() > entries && self.fold_oldest() {
            folded += 1;
        }
        // By bytes, a sixteenth of what is left at a time: counting the
        // states reads every step.
        while self.story_bytes() > bytes {
            let batch = (self.entries.len() / 16).max(1);
            let went = (0..batch).take_while(|_| self.fold_oldest()).count();
            folded += went;
            if went == 0 {
                break;
            }
        }
        if folded > 0 {
            self.sweep();
            self.generation += 1;
            self.base_size = None;
        }
        folded
    }

    /// Lets go of the states nothing names any more, and of the names of
    /// states for devices the base no longer has.
    fn sweep(&mut self) {
        let held: BTreeSet<DeviceId> = self.base.devices().map(|device| device.id).collect();
        self.base_states.retain(|device, _| held.contains(device));
        let mut used: BTreeSet<BlobId> = self.base_states.values().copied().collect();
        used.extend(self.entries.iter().filter_map(Entry::step).flat_map(|step| step.change.blobs()));
        self.blobs.retain(&used);
    }

    // ---- as text --------------------------------------------------------

    fn head_lines(&self, written: &mut BTreeSet<BlobId>, out: &mut String) {
        let dropped = self.dropped;
        out.push_str(&line(&LineOut::Journal { version: FORMAT_VERSION, began: self.began, dropped }));
        for id in self.base_states.values() {
            self.blob_line(*id, written, out);
        }
        out.push_str(&line(&LineOut::Base { song: &self.base, states: &self.base_states }));
    }

    fn blob_line(&self, id: BlobId, written: &mut BTreeSet<BlobId>, out: &mut String) {
        if let Some(bytes) = self.blobs.get(id)
            && written.insert(id)
        {
            out.push_str(&line(&LineOut::Blob { id, data: STANDARD.encode(bytes) }));
        }
    }

    fn entry_lines(&self, entry: &Entry, written: &mut BTreeSet<BlobId>, out: &mut String) {
        if let What::Step(step) = &entry.what {
            for id in step.change.blobs() {
                self.blob_line(id, written, out);
            }
        }
        out.push_str(&line(&LineOut::Entry(entry)));
    }

    /// The journal as text, one thing to a line: what a file of it holds.
    /// Entries that may still change (a call that has not ended) are left
    /// out. Without `conversation`, so is everything that was said and
    /// planned: what is left is how the song was made, for handing on.
    pub fn to_text(&self, conversation: bool) -> String {
        let mut out = String::new();
        let mut written = BTreeSet::new();
        self.head_lines(&mut written, &mut out);
        for entry in self.entries.iter().take(self.settled()) {
            if conversation || !entry.is_conversation() {
                self.entry_lines(entry, &mut written, &mut out);
            }
        }
        out
    }

    /// What to write to bring up to date a file that holds what `written`
    /// says, which is then brought up to date itself: the lines the file
    /// lacks, or the whole of it where old entries were let go since.
    pub fn text_since(&self, written: &mut Written) -> Writing {
        let settled = self.settled();
        let upto = self.entries.get(settled).map_or(self.next_seq, |entry| entry.seq);
        if written.generation != Some(self.generation) || written.next_seq > upto {
            let mut out = String::new();
            let mut blobs = BTreeSet::new();
            self.head_lines(&mut blobs, &mut out);
            for entry in self.entries.iter().take(settled) {
                self.entry_lines(entry, &mut blobs, &mut out);
            }
            *written = Written { generation: Some(self.generation), next_seq: upto, blobs };
            return Writing::Whole(out);
        }
        if written.next_seq == upto {
            return Writing::Nothing;
        }
        let mut out = String::new();
        let from = self.index_of(written.next_seq).unwrap_or(settled);
        for entry in self.entries.iter().take(settled).skip(from) {
            self.entry_lines(entry, &mut written.blobs, &mut out);
        }
        written.next_seq = upto;
        Writing::Append(out)
    }

    /// Reads a journal back from its text. A line that cannot be read ends
    /// the reading there: what came before it is the journal, which is
    /// what a file cut short by a crash should come back as. `Err` if not
    /// even the beginning is there, or the text is of a newer version.
    pub fn from_text(text: &str) -> Result<Self, Broken> {
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        let read = |line: &str| serde_json::from_str::<Line>(line).ok();
        let Some(Line::Journal { version, began, dropped }) = lines.next().and_then(read) else {
            return Err(Broken("this is not a journal".into()));
        };
        if version > FORMAT_VERSION {
            return Err(Broken(format!(
                "the journal was written by a newer version (format {version}, this build reads up to {FORMAT_VERSION})"
            )));
        }
        let mut blobs = Blobs::default();
        let mut base = None;
        let mut entries: VecDeque<Entry> = VecDeque::new();
        for text in lines {
            match read(text) {
                Some(Line::Blob { id, data }) => match STANDARD.decode(data) {
                    Ok(bytes) => blobs.put(id, bytes),
                    Err(_) => break,
                },
                Some(Line::Base { song, states }) if base.is_none() && entries.is_empty() => {
                    base = Some((*song, states))
                }
                Some(Line::Entry(entry)) if base.is_some() => {
                    // Numbered on from the one before, with no gaps but
                    // those a conversation left out leaves.
                    if entries.back().is_some_and(|last| entry.seq <= last.seq) {
                        break;
                    }
                    entries.push_back(*entry);
                }
                _ => break,
            }
        }
        let Some((base, base_states)) = base else {
            return Err(Broken("the journal does not say what song it began with".into()));
        };
        // A journal that was handed on without its conversation has gaps
        // in its numbers; here they are closed.
        let first = entries.front().map_or(dropped.entries + 1, |entry| entry.seq);
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.seq = first + index as u64;
        }
        let next_seq = first + entries.len() as u64;
        Ok(Self {
            base,
            base_states,
            blobs,
            entries,
            next_seq,
            began,
            dropped,
            limits: Limits::default(),
            calls: Vec::new(),
            last_call: 0,
            generation: 0,
            base_size: None,
        })
    }

    /// Takes the journal up again for `song`, which was opened from the
    /// file the journal was kept for. If the song is not as the journal
    /// left it (it was changed while no journal was kept, or on another
    /// machine), a step that says so brings the journal to it. A journal
    /// that no longer adds up is begun again from the song as it is.
    pub fn resume(mut self, song: &Project, at: u64) -> Self {
        match self.head() {
            Ok(head) if head == *song => self,
            Ok(head) => {
                let change = Change::between(&head, song, &mut self.blobs);
                let places = place::of_change(&change, song);
                let label = "The song was changed while no journal was kept".to_owned();
                let step = Step { label, places, tool: None, heard: None, reverses: None, aside: false, change };
                self.push(at, Author::Person, What::Step(step), None);
                self
            }
            Err(_) => Self::begin(song, at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChainRef, ClipId, Note, TrackId};

    fn note_at(start: f64, pitch: u8) -> Note {
        Note { pitch, start, length: 0.5, velocity: 96 }
    }

    /// Makes an edit and records it, returning where it went.
    fn step<R>(
        journal: &mut Journal,
        song: &mut Project,
        who: Who,
        edit: impl FnOnce(&mut Project) -> R,
    ) -> Option<Mark> {
        let before = song.clone();
        edit(song);
        let at = 1_000 * journal.next_seq();
        journal.record(at, who, &before, song, None)
    }

    fn labels(journal: &Journal) -> Vec<String> {
        journal.entries().map(Entry::line).collect()
    }

    /// A song built in six steps of a person's, and its journal.
    fn built() -> (Journal, Project, TrackId, ClipId) {
        let mut song = Project::new("Song");
        let mut journal = Journal::begin(&song, 0);
        let mut track = TrackId(0);
        let mut clip = ClipId(0);
        step(&mut journal, &mut song, Who::Person, |p| track = p.add_track("Bass"));
        step(&mut journal, &mut song, Who::Person, |p| {
            p.add_device(ChainRef::Track(track), "builtin:polysynth", "Poly Synth", None).unwrap();
        });
        step(&mut journal, &mut song, Who::Person, |p| clip = p.add_midi_clip(track, "Line", 0.0, 8.0).unwrap());
        step(&mut journal, &mut song, Who::Person, |p| {
            p.clip_mut(clip).unwrap().add_notes(&[note_at(0.0, 36), note_at(1.0, 43)]).unwrap();
        });
        step(&mut journal, &mut song, Who::Person, |p| p.set_tempo(96.0).unwrap());
        step(&mut journal, &mut song, Who::Person, |p| p.set_track_volume(track, -6.0).unwrap());
        (journal, song, track, clip)
    }

    #[test]
    fn the_song_as_it_was_after_any_step_comes_back_from_the_journal() {
        let (journal, song, track, clip) = built();
        assert_eq!(
            labels(&journal),
            [
                "Added the track \u{201C}Bass\u{201D}",
                "Added Poly Synth to \u{201C}Bass\u{201D}",
                "Added the clip \u{201C}Line\u{201D} to \u{201C}Bass\u{201D}",
                "Added 2 notes to \u{201C}Line\u{201D}",
                "Set the tempo to 96 BPM",
                "Changed the volume of \u{201C}Bass\u{201D}",
            ]
        );
        let with = States::With(journal.blobs());
        assert_eq!(journal.song_after(0, with).unwrap(), Project::new("Song"));
        assert_eq!(journal.head().unwrap(), song);

        let after_track = journal.song_at(1, with).unwrap();
        assert_eq!((after_track.tracks.len(), after_track.tracks[0].devices.len()), (1, 0));
        let after_clip = journal.song_at(3, with).unwrap();
        assert!(after_clip.clip(clip).unwrap().notes().is_empty());
        let after_notes = journal.song_at(4, with).unwrap();
        assert_eq!((after_notes.clip(clip).unwrap().notes().len(), after_notes.tempo), (2, 120.0));
        assert_eq!(journal.song_at(5, with).unwrap().tempo, 96.0);
        assert_eq!(journal.song_at(5, with).unwrap().track(track).unwrap().volume_db, 0.0);
        // A number past the end is the song as it is; one before the
        // beginning, the song as it began.
        assert_eq!(journal.song_at(99, with).unwrap(), song);
        assert_eq!(journal.song_at(0, with).unwrap(), Project::new("Song"));

        // Each entry knows who made it, when, and where it landed.
        let entry = journal.get(3).unwrap();
        assert_eq!((entry.seq, entry.at, &entry.by), (4, 4_000, &Author::Person));
        assert_eq!(entry.step().unwrap().places, [Place::Clips { clips: vec![(clip.0, "Line".into())] }]);
        assert_eq!((journal.steps(), journal.first_seq(), journal.next_seq()), (6, 1, 7));
    }

    #[test]
    fn a_calls_edits_are_one_step_under_its_words_and_a_look_is_entered_when_it_ends() {
        let mut song = Project::new("Song");
        let mut journal = Journal::begin(&song, 0);
        let client = || Author::Client("claude-code".into());

        // A call that makes three edits.
        let call = journal.begin_call(client(), "create_track", "Added the track \u{201C}Keys\u{201D}");
        let mut track = TrackId(0);
        step(&mut journal, &mut song, Who::Call(call), |p| track = p.add_track("Keys"));
        step(&mut journal, &mut song, Who::Call(call), |p| {
            p.add_device(ChainRef::Track(track), "builtin:polysynth", "Poly Synth", None).unwrap();
        });
        step(&mut journal, &mut song, Who::Call(call), |p| p.set_track_volume(track, -3.0).unwrap());
        assert_eq!(journal.len(), 1, "one step for the call");
        // While the call runs its step may still change, and is not written.
        assert!(!journal.to_text(true).contains("Keys"));
        journal.end_call(
            9_000,
            call,
            Outcome { places: vec![Place::Track { id: track.0, name: "Keys".into() }], ..Outcome::default() },
        );
        assert!(journal.to_text(true).contains("Keys"));
        let entry = journal.last().unwrap();
        let made = entry.step().unwrap();
        assert_eq!(
            (&entry.by, made.label.as_str(), made.tool.as_deref()),
            (&client(), "Added the track \u{201C}Keys\u{201D}", Some("create_track"))
        );
        assert_eq!(made.places, [Place::Track { id: track.0, name: "Keys".into() }]);
        assert_eq!(journal.head().unwrap(), song);

        // A call that changes nothing is a look, with what it heard.
        let call = journal.begin_call(Author::Assistant, "measure", "Measured the song");
        journal.end_call(
            10_000,
            call,
            Outcome { heard: Some("-14.2 LUFS, peak -1.0 dB".into()), ..Outcome::default() },
        );
        let look = Look {
            label: "Measured the song".into(),
            tool: Some("measure".into()),
            heard: Some("-14.2 LUFS, peak -1.0 dB".into()),
            failed: false,
        };
        assert_eq!(journal.last().unwrap().what, What::Look(look));
        assert_eq!(journal.last().unwrap().by, Author::Assistant);

        // Someone edits at the window while a call is under way: the
        // call's edits before and after are steps of their own, in order.
        let call = journal.begin_call(client(), "update_track", "Changed the pan of \u{201C}Keys\u{201D}");
        step(&mut journal, &mut song, Who::Call(call), |p| p.set_track_pan(track, 0.25).unwrap());
        step(&mut journal, &mut song, Who::Person, |p| p.set_tempo(90.0).unwrap());
        step(&mut journal, &mut song, Who::Call(call), |p| p.set_track_pan(track, 0.5).unwrap());
        journal.end_call(11_000, call, Outcome::default());
        let last: Vec<(Author, String)> =
            journal.entries().rev().take(3).map(|entry| (entry.by.clone(), entry.line())).collect();
        assert_eq!(
            last,
            [
                (client(), "Changed the pan of \u{201C}Keys\u{201D}".to_owned()),
                (Author::Person, "Set the tempo to 90 BPM".to_owned()),
                (client(), "Changed the pan of \u{201C}Keys\u{201D}".to_owned()),
            ]
        );
        assert_eq!(journal.head().unwrap(), song);

        // A call that failed, and one that heard something and changed
        // something too.
        let call = journal.begin_call(client(), "delete_track", "Deleted the track track 99");
        journal.end_call(12_000, call, Outcome { failed: true, ..Outcome::default() });
        assert!(matches!(&journal.last().unwrap().what, What::Look(look) if look.failed));
        let call = journal.begin_call(client(), "probe", "Tried out the instrument of \u{201C}Keys\u{201D}");
        step(&mut journal, &mut song, Who::Call(call), |p| p.track_mut(track).unwrap().profile.range = Some((36, 96)));
        journal.end_call(13_000, call, Outcome { heard: Some("plays 36 to 96".into()), ..Outcome::default() });
        assert_eq!(journal.last().unwrap().step().unwrap().heard.as_deref(), Some("plays 36 to 96"));
    }

    #[test]
    fn a_drag_is_one_step_and_work_backed_out_of_leaves_no_trace() {
        let mut song = Project::new("Song");
        let mut journal = Journal::begin(&song, 0);
        let mut track = TrackId(0);
        step(&mut journal, &mut song, Who::Person, |p| track = p.add_track("Keys"));

        // A fader dragged: many edits, one step, called by its first.
        step(&mut journal, &mut song, Who::Gesture { continues: false }, |p| p.set_track_volume(track, -1.0).unwrap());
        for db in 2..=20 {
            step(&mut journal, &mut song, Who::Gesture { continues: true }, |p| {
                p.set_track_volume(track, -(db as f32)).unwrap();
            });
        }
        journal.end_gesture();
        assert_eq!(
            labels(&journal),
            ["Added the track \u{201C}Keys\u{201D}", "Changed the volume of \u{201C}Keys\u{201D}"]
        );
        assert_eq!(journal.head().unwrap().track(track).unwrap().volume_db, -20.0);
        // The next drag is another step, even of the same fader.
        step(&mut journal, &mut song, Who::Gesture { continues: false }, |p| p.set_track_volume(track, -3.0).unwrap());
        journal.end_gesture();
        assert_eq!(journal.len(), 3);

        // An edit that is backed out of at once is as if never made.
        let before = song.clone();
        let mark = step(&mut journal, &mut song, Who::Person, |p| p.add_track("Doomed")).unwrap();
        song = before.clone();
        assert!(journal.retract(mark));
        assert_eq!((journal.len(), journal.next_seq()), (3, 4));
        assert_eq!(journal.head().unwrap(), song);

        // A call backs out of its second edit: the first stands.
        let call = journal.begin_call(Author::Assistant, "create_track", "Added the track \u{201C}Pad\u{201D}");
        step(&mut journal, &mut song, Who::Call(call), |p| p.add_track("Pad"));
        let kept = song.clone();
        let mark = step(&mut journal, &mut song, Who::Call(call), |p| p.set_tempo(77.0).unwrap()).unwrap();
        assert!(journal.is_of_call(mark.seq, call));
        song = kept;
        assert!(journal.retract(mark));
        journal.end_call(0, call, Outcome::default());
        assert_eq!(journal.len(), 4);
        assert_eq!(journal.head().unwrap(), song);
        // And out of its only edit: the call is then a look that failed.
        let call = journal.begin_call(Author::Assistant, "add_device", "Added nope to the master");
        let kept = song.clone();
        let mark = step(&mut journal, &mut song, Who::Call(call), |p| p.set_tempo(55.0).unwrap()).unwrap();
        song = kept;
        assert!(journal.retract(mark));
        journal.end_call(0, call, Outcome { failed: true, ..Outcome::default() });
        assert!(matches!(&journal.last().unwrap().what, What::Look(look) if look.failed));
        assert_eq!(journal.head().unwrap(), song);

        // Only the last edit can be taken out.
        let first = step(&mut journal, &mut song, Who::Person, |p| p.set_tempo(100.0).unwrap()).unwrap();
        step(&mut journal, &mut song, Who::Person, |p| p.set_tempo(101.0).unwrap());
        assert!(!journal.retract(first));
    }

    #[test]
    fn a_step_taken_back_stays_in_the_story_followed_by_the_one_that_takes_it_back() {
        let (mut journal, song, track, _) = built();
        let before_volume = journal.song_at(5, States::With(journal.blobs())).unwrap();

        // Undo of the last step, then redo.
        let mark = journal
            .reverse(
                7_000,
                Who::Person,
                Reversal::Undo,
                (6, 6),
                ("Changed the volume of \u{201C}Bass\u{201D}", 1),
                &song,
                &before_volume,
            )
            .unwrap();
        assert_eq!(journal.len(), 7, "nothing is taken out of the story");
        let entry = journal.last().unwrap();
        assert_eq!(entry.line(), "Took back: Changed the volume of \u{201C}Bass\u{201D}");
        assert_eq!(entry.step().unwrap().reverses, Some(Reversed { kind: Reversal::Undo, first: 6, last: 6 }));
        assert_eq!(journal.head().unwrap().track(track).unwrap().volume_db, 0.0);
        assert!(!journal.retract(mark), "a step taken back is not backed out of");
        journal.reverse(
            8_000,
            Who::Person,
            Reversal::Redo,
            (6, 6),
            ("Changed the volume of \u{201C}Bass\u{201D}", 1),
            &before_volume,
            &song,
        );
        assert_eq!(journal.last().unwrap().line(), "Brought back: Changed the volume of \u{201C}Bass\u{201D}");
        assert_eq!(journal.head().unwrap(), song);
        // The song as it was at each point is still there to be seen.
        let with = States::With(journal.blobs());
        assert_eq!(journal.song_at(6, with).unwrap().track(track).unwrap().volume_db, -6.0);
        assert_eq!(journal.song_at(7, with).unwrap().track(track).unwrap().volume_db, 0.0);
        assert_eq!(journal.song_at(8, with).unwrap().track(track).unwrap().volume_db, -6.0);

        // A whole request of the assistant's taken back at once.
        let blank = Project::new("Song");
        journal.reverse(
            9_000,
            Who::Person,
            Reversal::Undo,
            (1, 6),
            ("Added the track \u{201C}Bass\u{201D}", 6),
            &song,
            &blank,
        );
        assert_eq!(journal.last().unwrap().line(), "Took back 6 steps, from: Added the track \u{201C}Bass\u{201D}");
        assert_eq!(journal.head().unwrap(), blank);
    }

    #[test]
    fn the_conversation_and_the_plan_are_entered_as_they_come() {
        let (mut journal, _, _, _) = built();
        journal.say(10_000, Author::Person, "Make the bass busier in the second half.");
        journal.plan(
            10_500,
            vec![
                PlanItem { text: "Read the bass line".into(), state: PlanState::Doing },
                PlanItem { text: "Add passing notes".into(), state: PlanState::Pending },
            ],
        );
        journal.say(11_000, Author::Assistant, "I'll add passing notes from bar 3.");
        journal.mark(12_000, Author::Assistant, "Finished in 12 s");
        let said: Vec<(&Author, String)> = journal.entries().skip(6).map(|entry| (&entry.by, entry.line())).collect();
        assert_eq!(
            said,
            [
                (&Author::Person, "Make the bass busier in the second half.".to_owned()),
                (&Author::Assistant, "Plan: 0 of 2 done".to_owned()),
                (&Author::Assistant, "I'll add passing notes from bar 3.".to_owned()),
                (&Author::Assistant, "Finished in 12 s".to_owned()),
            ]
        );
        assert!(journal.get(6).unwrap().is_conversation() && journal.get(7).unwrap().is_conversation());
        assert!(!journal.get(9).unwrap().is_conversation() && !journal.get(0).unwrap().is_conversation());
    }

    #[test]
    fn a_journal_reads_back_from_its_text_and_a_file_is_only_added_to() {
        let (mut journal, mut song, track, clip) = built();
        journal.say(10_000, Author::Person, "Give it a pad.");
        let before = song.clone();
        let device = song.add_device(ChainRef::Track(track), "clap:pad", "Pad", None).unwrap();
        song.device_mut(device).unwrap().state = Some(vec![42; 10_000]);
        journal.record(11_000, Who::Person, &before, &song, None);

        // Whole, and read back: the same story and the same songs.
        let mut written = Written::default();
        let Writing::Whole(mut file) = journal.text_since(&mut written) else { panic!("a new file is written whole") };
        let read = Journal::from_text(&file).unwrap();
        assert_eq!(read.entries().collect::<Vec<_>>(), journal.entries().collect::<Vec<_>>());
        assert_eq!(read.head().unwrap(), journal.head().unwrap());
        assert_eq!(journal.text_since(&mut written), Writing::Nothing);

        // More happens: only the new lines are written, and a state the
        // file already holds is not written again.
        let before = song.clone();
        song.clip_mut(clip).unwrap().add_notes(&[note_at(4.0, 40)]).unwrap();
        journal.record(12_000, Who::Person, &before, &song, None);
        let before = song.clone();
        song.device_mut(device).unwrap().state = None;
        journal.record(13_000, Who::Person, &before, &song, None);
        let before = song.clone();
        song.device_mut(device).unwrap().state = Some(vec![42; 10_000]);
        journal.record(14_000, Who::Person, &before, &song, None);
        let Writing::Append(more) = journal.text_since(&mut written) else { panic!("a file is added to") };
        assert_eq!(more.lines().count(), 3, "three entries and no state: {more}");
        assert!(more.len() < 1_000, "{} bytes", more.len());
        file.push_str(&more);
        let read = Journal::from_text(&file).unwrap();
        assert_eq!(read.head().unwrap(), song);
        assert_eq!(read.len(), journal.len());

        // A file cut short in the middle of a line reads back up to it.
        let cut = &file[..file.len() - 40];
        let read = Journal::from_text(cut).unwrap();
        assert_eq!(read.len(), journal.len() - 1);
        assert_eq!(read.next_seq(), journal.next_seq() - 1);
        // What is not a journal says so; so does one from the future.
        assert!(Journal::from_text("").is_err());
        assert!(Journal::from_text("{\"name\":\"a project\"}").is_err());
        let newer = file.replacen("\"version\":1", "\"version\":99", 1);
        assert!(Journal::from_text(&newer).unwrap_err().0.contains("newer version"));

        // Handed on without the conversation, nothing that was said is in
        // the text, and the songs are the same.
        let quiet = journal.to_text(false);
        assert!(file.contains("Give it a pad.") && !quiet.contains("Give it a pad."));
        let read = Journal::from_text(&quiet).unwrap();
        assert_eq!(read.len(), journal.len() - 1);
        assert_eq!(read.head().unwrap(), song);
        assert!(read.entries().zip(read.first_seq()..).all(|(entry, seq)| entry.seq == seq));
    }

    #[test]
    fn a_hundred_steps_on_a_song_with_a_large_state_cost_little_more_than_one() {
        // A megabyte of plugin state that does not compress into nothing.
        let state: Vec<u8> = (0..1_000_000u32).map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let mut song = Project::new("Song");
        let track = song.add_track("Keys");
        let device = song.add_device(ChainRef::Track(track), "clap:sampler", "Big Sampler", None).unwrap();
        song.device_mut(device).unwrap().state = Some(state);
        let clip = song.add_midi_clip(track, "Part", 0.0, 64.0).unwrap();
        let project_bytes = song.to_json().len();
        assert!(project_bytes > 1_300_000, "the project file is over a megabyte: {project_bytes}");

        let mut journal = Journal::begin(&song, 0);
        let with_none = journal.to_text(true).len();
        let mut sizes = Vec::new();
        for index in 0..100u32 {
            // What a session is made of: notes written, a fader moved, a
            // parameter of the big device itself turned.
            let before = song.clone();
            match index % 4 {
                0 => {
                    let notes: Vec<Note> =
                        (0..8).map(|step| note_at((index * 2 + step) as f64 * 0.25, 48 + step as u8)).collect();
                    song.clip_mut(clip).unwrap().add_notes(&notes).unwrap();
                }
                1 => song.set_track_volume(track, -(index as f32) / 10.0).unwrap(),
                2 => drop(song.device_mut(device).unwrap().params.insert("cutoff".into(), index as f64)),
                _ => song.set_clip_span(clip, 0.0, 64.0 + index as f64).unwrap(),
            }
            journal.record(u64::from(index), Who::Person, &before, &song, None);
            sizes.push(journal.to_text(true).len());
        }
        let (with_one, with_hundred) = (sizes[0], sizes[99]);
        let one = with_one - with_none;
        let hundred = with_hundred - with_none;
        assert!(one < 1_000, "one step: {one} bytes");
        assert!(hundred < 60_000, "a hundred steps: {hundred} bytes, where the project is {project_bytes}");
        // The journal with a hundred steps is the song it began with and
        // a twentieth more at most: not a hundred songs.
        assert!(
            with_hundred < with_one + with_one / 20,
            "{with_one} bytes with one step, {with_hundred} with a hundred"
        );
        println!("project {project_bytes} B; journal: empty {with_none} B, one step +{one} B, a hundred +{hundred} B");
        assert_eq!(journal.blobs().len(), 1, "the state is held once");
        assert_eq!(journal.head().unwrap(), song);
        assert_eq!(journal.story_bytes(), hundred, "what the story adds is what its entries take written down");
    }

    #[test]
    fn past_its_limits_a_journal_lets_go_of_its_oldest_entries_and_says_so() {
        let mut song = Project::new("Song");
        let track = song.add_track("Keys");
        let device = song.add_device(ChainRef::Track(track), "clap:pad", "Pad", None).unwrap();
        let mut journal = Journal::begin(&song, 0);
        journal.set_limits(Limits { entries: 40, bytes: usize::MAX });
        assert_eq!(journal.keep_within_limits(), 0);

        let mut songs = vec![song.clone()];
        for index in 1..=100u32 {
            let before = song.clone();
            song.set_track_volume(track, -(index as f32) / 10.0).unwrap();
            journal.record(u64::from(index) * 1_000, Who::Person, &before, &song, None);
            songs.push(song.clone());
            journal.keep_within_limits();
            assert!(journal.len() <= 40, "never over the limit after a look: {}", journal.len());
        }
        // The oldest went; the newest are all there, numbered as they were.
        let dropped = journal.dropped();
        assert_eq!(dropped.entries as usize + journal.len(), 100);
        assert_eq!((dropped.steps, dropped.until), (dropped.entries, Some(dropped.entries * 1_000)));
        assert_eq!(journal.began(), dropped.entries * 1_000);
        assert_eq!((journal.first_seq(), journal.next_seq()), (dropped.entries + 1, 101));
        // The story begins with the song as it was after what went, and
        // every step since still leads to the song as it was.
        let with = States::With(journal.blobs());
        assert_eq!(journal.song_after(0, with).unwrap(), songs[dropped.entries as usize]);
        for seq in journal.first_seq()..journal.next_seq() {
            assert_eq!(journal.song_at(seq, with).unwrap(), songs[seq as usize], "after entry {seq}");
        }
        // A file that held the old shape is written anew, and reads back.
        let mut written = Written { generation: Some(0), next_seq: 30, blobs: BTreeSet::new() };
        let Writing::Whole(file) = journal.text_since(&mut written) else { panic!("written anew") };
        let read = Journal::from_text(&file).unwrap();
        assert_eq!((read.dropped(), read.first_seq(), read.next_seq()), (dropped, journal.first_seq(), 101));
        assert_eq!(read.head().unwrap(), song);

        // By bytes: states that only old steps named go with them, and the
        // one the song still has stays, in the base.
        let mut journal = Journal::begin(&song, 0);
        journal.set_limits(Limits { entries: usize::MAX, bytes: 400_000 });
        for index in 0..20u8 {
            let before = song.clone();
            song.device_mut(device).unwrap().state = Some(vec![index; 60_000]);
            journal.record(u64::from(index), Who::Person, &before, &song, None);
            journal.keep_within_limits();
            assert!(journal.story_bytes() <= 400_000);
        }
        assert!(journal.dropped().entries > 10 && journal.len() >= 3, "{journal:?}");
        assert!(journal.blobs().len() <= journal.len() + 1, "states nothing names are let go: {journal:?}");
        assert_eq!(journal.head().unwrap(), song);
        let base = journal.base(States::With(journal.blobs())).unwrap();
        assert_eq!(base.device(device).unwrap().state.as_ref().map(Vec::len), Some(60_000));
    }

    #[test]
    fn a_journal_is_taken_up_again_for_the_song_it_was_kept_for() {
        let (journal, song, track, _) = built();
        let text = journal.to_text(true);

        // The song as the journal left it: nothing is added.
        let resumed = Journal::from_text(&text).unwrap().resume(&song, 50_000);
        assert_eq!(resumed.len(), 6);

        // The song was changed elsewhere: a step says so and brings the
        // journal to it.
        let mut changed = song.clone();
        changed.set_track_pan(track, -0.5).unwrap();
        changed.add_track("Elsewhere");
        let resumed = Journal::from_text(&text).unwrap().resume(&changed, 50_000);
        assert_eq!(resumed.len(), 7);
        assert_eq!(resumed.last().unwrap().line(), "The song was changed while no journal was kept");
        assert_eq!(resumed.head().unwrap(), changed);

        // A journal that does not add up is begun again.
        let broken = text.replace("\"tracks\":[1]", "\"tracks\":[77]");
        assert_ne!(broken, text);
        let resumed = Journal::from_text(&broken).unwrap().resume(&song, 50_000);
        assert!(resumed.is_empty());
        assert_eq!((resumed.head().unwrap(), resumed.began()), (song, 50_000));
    }

    #[test]
    fn what_a_device_came_to_on_its_own_is_one_step_however_often_it_is_looked_at() {
        let mut song = Project::new("Song");
        let track = song.add_track("Keys");
        let chain = ChainRef::Track(track);
        let device = song.add_device(chain, "clap:pad", "Pad", None).unwrap();
        let mut journal = Journal::begin(&song, 0);
        let label = "Changed \u{201C}Pad\u{201D} in its own window";
        for look in 1..=5u8 {
            song.device_mut(device).unwrap().state = Some(vec![look; 50_000]);
            journal.note_device(u64::from(look), chain, song.device(device).unwrap(), true, label);
        }
        // A knob of it that the project keeps, turned in the same window:
        // the state that came before is not lost for it.
        song.device_mut(device).unwrap().params.insert("cutoff".into(), 0.25);
        journal.note_device(6, chain, song.device(device).unwrap(), false, label);
        assert_eq!(labels(&journal), [label]);
        assert_eq!(journal.last().unwrap().at, 6);
        assert_eq!(journal.head().unwrap(), song);
        assert_eq!(journal.blobs().len(), 1, "the states it passed through are let go");
        assert_eq!(
            journal.last().unwrap().step().unwrap().places,
            [Place::Device { id: device.0, name: "Pad".into() }]
        );

        // Anything in between, and the next one is its own step.
        let before = song.clone();
        song.set_tempo(99.0).unwrap();
        journal.record(7, Who::Person, &before, &song, None);
        song.device_mut(device).unwrap().state = None;
        journal.note_device(8, chain, song.device(device).unwrap(), true, label);
        assert_eq!(journal.len(), 3);
        assert_eq!(journal.head().unwrap(), song);
    }

    #[test]
    fn calls_that_are_open_go_along_when_another_song_is_opened() {
        let mut song = Project::new("Old");
        let mut journal = Journal::begin(&song, 0);
        let call = journal.begin_call(Author::Assistant, "new_project", "Started a new project");
        step(&mut journal, &mut song, Who::Call(call), |p| p.add_track("Left behind"));

        // The call replaces the song: its journal ends, another begins,
        // and what the call does from here on is entered there.
        let mut song = Project::new("New");
        let mut next = Journal::begin(&song, 5);
        next.adopt_calls(journal.take_calls());
        assert!(next.is_open(call) && !journal.is_open(call));
        step(&mut next, &mut song, Who::Call(call), |p| p.name = "Sketch".into());
        next.end_call(6, call, Outcome::default());
        let entry = next.last().unwrap();
        assert_eq!((entry.line().as_str(), &entry.by), ("Started a new project", &Author::Assistant));
        assert_eq!(next.head().unwrap(), song);
        // The journal that was left has the call's step there as it stood.
        assert!(journal.to_text(true).contains("Left behind"));

        // A call that did nothing in the new song is a look in it.
        let call = next.begin_call(Author::Assistant, "load_project", "Opened \u{201C}day.trill\u{201D}");
        let mut loaded = Journal::begin(&song, 9);
        loaded.adopt_calls(next.take_calls());
        loaded.end_call(10, call, Outcome::default());
        assert_eq!(loaded.last().unwrap().line(), "Opened \u{201C}day.trill\u{201D}");
    }

    #[test]
    fn edits_of_one_undo_step_are_backed_out_of_together() {
        let mut song = Project::new("Song");
        let mut journal = Journal::begin(&song, 0);
        let blank = song.clone();
        // A drag of three edits, backed out of as one.
        let first = step(&mut journal, &mut song, Who::Gesture { continues: false }, |p| p.add_track("A")).unwrap();
        let second = step(&mut journal, &mut song, Who::Gesture { continues: true }, |p| p.set_tempo(90.0)).unwrap();
        let third = step(&mut journal, &mut song, Who::Gesture { continues: true }, |p| p.set_tempo(91.0)).unwrap();
        let whole = first.through(second).and_then(|mark| mark.through(third)).expect("they follow one another");
        assert!(journal.retract(whole));
        assert!(journal.is_empty());
        assert_eq!(journal.head().unwrap(), blank);

        // Edits that went to different entries are not one mark.
        let mut song = blank;
        let one = step(&mut journal, &mut song, Who::Person, |p| p.set_tempo(90.0)).unwrap();
        let other = step(&mut journal, &mut song, Who::Person, |p| p.set_tempo(91.0)).unwrap();
        assert_eq!(one.through(other), None);
        assert_eq!(first.through(third), None, "nor are edits with one between them");
    }
}
