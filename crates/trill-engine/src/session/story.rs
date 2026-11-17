//! The session's journal: how the open song came to be.
//!
//! Every change the session makes to its project is entered in a
//! [`Journal`] as it is made: who made it, when, what it was and where it
//! landed (`trill_core::journal`). This file is the session's side of
//! that: who is taken to have made a step, what an undo step is in the
//! story, and when the journal is read and written.
//!
//! * **Who.** A thread that acts for a call
//!   ([`journal::acting`](crate::journal::acting)) makes that call's steps.
//!   Any other makes a person's: an edit is a step, the edits of a drag
//!   ([`Session::begin_gesture`]) are one, and so are those of a take.
//! * **Undo.** Each undo step remembers which entries it made ([`Story`]).
//!   Undoing it enters a step that takes them back, and redoing one that
//!   brings them back; nothing leaves the journal. An operation that could
//!   not be finished, and a call that undoes what it has just done itself,
//!   back out instead: the edit is taken out of the journal as if it had
//!   not been made, which to anyone watching it was not.
//! * **Where it is kept.** In memory, always. On disk only once the
//!   session is told a folder ([`Session::keep_journals_in`]), which an
//!   application does when it starts and a run that leaves nothing behind
//!   never does; and there only in that folder, under a name worked out
//!   from the project's path. It is written when the project is saved, so
//!   that the file of a song and the journal of it tell the same story,
//!   and read when a project is opened. A project that was changed since
//!   its journal was written (while none was kept, or on another machine)
//!   gets a step that says so.
//!
//!   The project file holds nothing of it. A journal has the conversation
//!   with the assistant in it; to hand it on is [`Session::export_journal`],
//!   which is asked for by name and leaves the conversation out unless told
//!   to keep it.
//! * **Whether it is kept** is the user's to say
//!   ([`Session::set_journal_keeping`]), and one that is kept can be
//!   deleted ([`Session::delete_journal`]).

use super::{Session, SessionError, io_error};
use crate::journal::{actor, file_for, now_ms};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::journal::words::quoted;
use trill_core::journal::{Author, CallId, Journal, Mark, Outcome, PlanItem, Reversal, Who, Writing, Written};
use trill_core::{DeviceId, Project};

/// How many entries may be added before the journal is next held to its
/// limits.
const LOOK_EVERY: u64 = 256;

/// What an undo step is in the journal.
#[derive(Clone, Debug, Default)]
pub(super) struct Story {
    /// The entries it made, from the first to the last; `None` for a step
    /// made while no journal was kept, or in a journal that has since been
    /// begun again.
    span: Option<(u64, u64)>,
    /// What the first of them goes by.
    label: String,
    /// How many of them there are.
    steps: u32,
    /// Where its edits went, if they went to one entry, one after another:
    /// what backing out of the step takes out again.
    mark: Option<Mark>,
}

impl Story {
    /// Makes this the story of a step that also made `next`: a drag's
    /// further edit.
    pub fn and(&mut self, next: Story) {
        let Some((first, last)) = next.span else { return };
        match self.span {
            None => *self = next,
            Some((_, held)) => {
                if held != first {
                    self.steps += next.steps;
                }
                self.span = self.span.map(|(start, _)| (start, last));
                self.mark = match (self.mark, next.mark) {
                    (Some(mine), Some(theirs)) => mine.through(theirs),
                    _ => None,
                };
            }
        }
    }

    /// The story of several undo steps that became one: a group's.
    pub fn joined<'a>(stories: impl IntoIterator<Item = &'a Story>) -> Story {
        let mut joined = Story::default();
        for story in stories {
            joined.and(story.clone());
        }
        // A group is not backed out of: it is taken back, and seen to be.
        joined.mark = None;
        joined
    }
}

/// Edits that are one step by one name, however many holds of the session
/// they are made in: a take being recorded.
struct Run {
    label: String,
    continues: bool,
}

/// The journal of the open project and what is known of its file.
pub(super) struct Keeping {
    journal: Option<Journal>,
    /// Whether a journal is kept at all.
    keep: bool,
    /// Where journals are kept between runs.
    folder: Option<PathBuf>,
    /// The file the open project's journal was last written to, what it
    /// holds, and how long it was left.
    file: Option<(PathBuf, u64)>,
    written: Written,
    run: Option<Run>,
    /// The number of the journal's next entry when it was last held to its
    /// limits.
    looked: u64,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Keeping {
    pub fn new() -> Self {
        Self {
            journal: None,
            keep: true,
            folder: None,
            file: None,
            written: Written::default(),
            run: None,
            looked: 0,
            clock: Arc::new(now_ms),
        }
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }
}

/// Replaces `path` with `text` in one step.
fn replace(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let temporary = path.with_extension(format!("{}.part", std::process::id()));
    let written = std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(text.as_bytes())
}

impl Session {
    // ---- reading and settings -------------------------------------------

    /// The journal of the open project: how it came to be since it was
    /// opened, or since its journal began. `None` while none is kept.
    pub fn journal(&self) -> Option<&Journal> {
        self.story.journal.as_ref()
    }

    /// Whether a journal is kept.
    pub fn keeps_journal(&self) -> bool {
        self.story.keep
    }

    /// Keeps a journal from now on, or stops keeping one. Stopping lets go
    /// of the journal in memory and leaves what is on disk as it is (see
    /// [`Session::delete_journal`]); starting takes the open project's
    /// journal up from there if it has one, with a step for what changed
    /// in between.
    pub fn set_journal_keeping(&mut self, keep: bool) {
        if keep == self.story.keep {
            return;
        }
        self.story.keep = keep;
        self.restart_journal();
        if keep && let Some(path) = self.path.clone() {
            self.take_up_journal(&path);
        }
    }

    /// Says where journals are kept between runs: a folder of the
    /// program's own. An application says so once, when it starts; a
    /// session that is to leave nothing behind is never told, and keeps
    /// its journal in memory only.
    pub fn keep_journals_in(&mut self, folder: PathBuf) {
        self.story.folder = Some(folder);
    }

    /// The journal of the open project is forgotten: its file, if it has
    /// one, is deleted, and the story begins again here, with the song as
    /// it is.
    pub fn delete_journal(&mut self) -> Result<(), SessionError> {
        if let (Some(folder), Some(path)) = (&self.story.folder, &self.path) {
            let file = file_for(folder, path);
            match std::fs::remove_file(&file) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error(&file, error)),
            }
        }
        self.restart_journal();
        Ok(())
    }

    /// Writes the journal out to `path`, for handing on: the song as it
    /// was when the story begins and every step since. What was said
    /// between the user and the assistant, and the assistant's plan, are
    /// left out unless `conversation` is asked for.
    pub fn export_journal(&self, path: &Path, conversation: bool) -> Result<(), SessionError> {
        let Some(journal) = &self.story.journal else {
            return Err(SessionError::Io("no journal is kept, so there is none to write out".into()));
        };
        replace(path, &journal.to_text(conversation)).map_err(|error| io_error(path, error))
    }

    /// What to write to bring a copy of the journal up to date that holds
    /// what `written` says: for the safety copy of a project with unsaved
    /// changes. [`Writing::Nothing`] while no journal is kept.
    pub fn journal_since(&mut self, written: &mut Written) -> Writing {
        match &mut self.story.journal {
            Some(journal) => journal.text_since(written),
            None => Writing::Nothing,
        }
    }

    /// Has the journal tell time by `clock` (milliseconds since 1970)
    /// instead of by the system's: for canned sessions, whose pictures
    /// must not depend on how fast they were played.
    #[doc(hidden)]
    pub fn set_journal_clock(&mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) {
        self.story.clock = clock;
    }

    // ---- what a front end enters ----------------------------------------

    /// Opens a call of `tool` by `by`: what a thread acting for it
    /// ([`journal::acting`](crate::journal::acting)) changes until
    /// [`Session::end_call`] is one step that goes by `label`. `None`
    /// while no journal is kept.
    pub fn begin_call(&mut self, by: Author, tool: &str, label: impl Into<String>) -> Option<CallId> {
        Some(self.story.journal.as_mut()?.begin_call(by, tool, label))
    }

    /// Closes a call: see [`Journal::end_call`].
    pub fn end_call(&mut self, call: CallId, outcome: Outcome) {
        let at = self.story.now();
        if let Some(journal) = &mut self.story.journal {
            journal.end_call(at, call, outcome);
        }
    }

    /// Enters what was said in the conversation with the assistant.
    pub fn say(&mut self, by: Author, text: &str) {
        let at = self.story.now();
        if let Some(journal) = &mut self.story.journal {
            journal.say(at, by, text);
        }
    }

    /// Enters the assistant's plan as it stands.
    pub fn note_plan(&mut self, items: Vec<PlanItem>) {
        let at = self.story.now();
        if let Some(journal) = &mut self.story.journal {
            journal.plan(at, items);
        }
    }

    /// Enters something about the session itself.
    pub fn mark_journal(&mut self, by: Author, label: &str) {
        let at = self.story.now();
        if let Some(journal) = &mut self.story.journal {
            journal.mark(at, by, label);
        }
    }

    // ---- what the session enters itself ---------------------------------

    /// Enters the edit that turned `before` into the project as it is, and
    /// says what it is in the story.
    pub(super) fn record_step(&mut self, before: &Project) -> Story {
        let at = self.story.now();
        let Some(journal) = &mut self.story.journal else { return Story::default() };
        let (who, label) = match (actor().filter(|call| journal.is_open(*call)), self.gesture, &mut self.story.run) {
            (Some(call), ..) => (Who::Call(call), None),
            (None, Some(continues), _) => (Who::Gesture { continues }, None),
            (None, None, Some(run)) => {
                let continues = std::mem::replace(&mut run.continues, true);
                (Who::Gesture { continues }, Some(run.label.clone()))
            }
            (None, None, None) => (Who::Person, None),
        };
        let Some(mark) = journal.record(at, who, before, &self.project, label.as_deref()) else {
            return Story::default();
        };
        let label = journal.index_of(mark.seq).and_then(|index| journal.get(index)).map(|entry| entry.line());
        Story { span: Some((mark.seq, mark.seq)), label: label.unwrap_or_default(), steps: 1, mark: Some(mark) }
    }

    /// Enters that the step `story` tells of was taken back or brought
    /// back, which turned `leaving` into the project as it is.
    pub(super) fn record_reversal(&mut self, kind: Reversal, story: &Story, leaving: &Project) {
        let at = self.story.now();
        let Some(journal) = &mut self.story.journal else { return };
        let who = match actor().filter(|call| journal.is_open(*call)) {
            Some(call) => Who::Call(call),
            None => Who::Person,
        };
        let label = if story.label.is_empty() { "an earlier step" } else { story.label.as_str() };
        let span = story.span.unwrap_or_default();
        journal.reverse(at, who, kind, span, (label, story.steps), leaving, &self.project);
    }

    /// Takes the step `story` tells of out of the journal as if it had
    /// not been made, if it can be: it is what the journal ends with. With
    /// `own`, only for a call that is undoing what it made itself.
    pub(super) fn retract_step(&mut self, story: &Story, own: bool) -> bool {
        let (Some(mark), Some(journal)) = (story.mark, &mut self.story.journal) else { return false };
        if own && !actor().is_some_and(|call| journal.is_of_call(mark.seq, call)) {
            return false;
        }
        journal.retract(mark)
    }

    /// The story of a step with no way left to back out of it: one that
    /// has been taken back, and is kept to be brought back.
    pub(super) fn settled(mut story: Story) -> Story {
        story.mark = None;
        story
    }

    /// The drag that was under way is over.
    pub(super) fn end_journal_gesture(&mut self) {
        if let Some(journal) = &mut self.story.journal {
            journal.end_gesture();
        }
    }

    /// From here until [`Session::end_run`], what is changed without a
    /// call or a drag is one step that goes by `label`.
    pub(super) fn begin_run(&mut self, label: &str) {
        self.end_journal_gesture();
        self.story.run = Some(Run { label: label.to_owned(), continues: false });
    }

    pub(super) fn end_run(&mut self) {
        if self.story.run.take().is_some() {
            self.end_journal_gesture();
        }
    }

    /// Enters what a device came to on its own in its editor's window:
    /// its state if `state_changed`, and the values it has. Nobody's edit,
    /// and not shown as a step; kept so that the song as it was can be
    /// rebuilt as it was.
    pub(super) fn note_device(&mut self, id: DeviceId, state_changed: bool) {
        let at = self.story.now();
        let Some(journal) = &mut self.story.journal else { return };
        let (Ok(device), Ok(chain)) = (self.project.device(id), self.project.device_chain(id)) else { return };
        let label = format!("{} changed in its own window", quoted(&device.name));
        journal.note_device(at, chain, device, state_changed, &label);
    }

    // ---- when the project is replaced, opened and saved -------------------

    /// Begins the journal again, here, with the project as it is: another
    /// project has taken the place of the one it was about. Calls that are
    /// open go along. The steps that can be undone are no longer told of
    /// in the journal.
    pub(super) fn restart_journal(&mut self) {
        let calls = self.story.journal.as_mut().map(Journal::take_calls);
        let at = self.story.now();
        self.story.run = None;
        self.story.file = None;
        self.story.written = Written::default();
        self.story.looked = 0;
        self.story.journal = self.story.keep.then(|| {
            let mut journal = Journal::begin(&self.project, at);
            if let Some(calls) = calls {
                journal.adopt_calls(calls);
            }
            journal
        });
        for step in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            step.story = Story::default();
        }
    }

    /// Reads `file` as the journal of the project that has just been
    /// opened and takes it up, if it is one.
    fn resume_journal(&mut self, file: &Path) -> bool {
        if !self.story.keep {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(file) else { return false };
        let read = match Journal::from_text(&text) {
            Ok(read) => read,
            Err(error) => {
                log::warn!("the journal in {} was not taken up: {error}", file.display());
                return false;
            }
        };
        let at = self.story.now();
        let mut journal = read.resume(&self.project, at);
        if let Some(calls) = self.story.journal.as_mut().map(Journal::take_calls) {
            journal.adopt_calls(calls);
        }
        self.story.journal = Some(journal);
        true
    }

    /// Takes up the journal kept for the project file at `path`, if there
    /// is one: the project has just been opened from there.
    pub(super) fn take_up_journal(&mut self, path: &Path) {
        if let Some(folder) = self.story.folder.clone() {
            self.resume_journal(&file_for(&folder, path));
        }
    }

    /// Takes up the journal that was kept beside the safety copy at
    /// `copy`, or else the one of the file the lost project lived in.
    pub(super) fn take_up_recovered_journal(&mut self, copy: &Path, original: Option<&Path>) {
        if !self.resume_journal(&copy.with_extension(crate::journal::FILE_ENDING))
            && let Some(original) = original
        {
            self.take_up_journal(original);
        }
    }

    /// Writes the journal to the file kept for the project at `path`,
    /// which has just been saved there: the lines the file lacks, or all
    /// of it. A journal that cannot be written is a warning in the log;
    /// the song is saved.
    pub(super) fn write_journal(&mut self, path: &Path) {
        let (Some(journal), Some(folder)) = (&mut self.story.journal, &self.story.folder) else { return };
        journal.keep_within_limits();
        self.story.looked = journal.next_seq();
        let file = file_for(folder, path);
        // A file that is not as it was left (another program wrote it, or
        // it is another project's) is written anew.
        let length = std::fs::metadata(&file).map(|found| found.len()).ok();
        if self.story.file.as_ref().map(|(held, length)| (held, Some(*length))) != Some((&file, length)) {
            self.story.written = Written::default();
        }
        let result = match journal.text_since(&mut self.story.written) {
            Writing::Nothing => Ok(()),
            Writing::Append(text) => append(&file, &text),
            Writing::Whole(text) => replace(&file, &text),
        };
        match result.and_then(|()| std::fs::metadata(&file)) {
            Ok(found) => self.story.file = Some((file, found.len())),
            Err(error) => {
                log::warn!("the journal could not be written to {}: {error}", file.display());
                self.story.file = None;
                self.story.written = Written::default();
            }
        }
    }

    /// Holds the journal to its limits, now and then.
    pub(super) fn tend_journal(&mut self) {
        if let Some(journal) = &mut self.story.journal
            && journal.next_seq() >= self.story.looked + LOOK_EVERY
        {
            journal.keep_within_limits();
            self.story.looked = journal.next_seq();
        }
    }
}
