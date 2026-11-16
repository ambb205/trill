//! What the assistant's panel keeps between runs, beside the settings:
//!
//! * `assistant.json`: whether the panel was open and how wide, what was
//!   chosen in it (the model, how hard it is to think, whether it may use
//!   the web, how many steps one request may take), and for each project
//!   file the id of the conversation about it. The id is how a conversation
//!   is taken up again; it is the CLI's name for it, not a credential.
//! * `assistant/transcripts/<id>.json`: the conversation as the panel
//!   showed it, to read back.
//!
//! Nothing goes into the project file, and a project that was never saved
//! has no place here: its conversation lasts as long as the window.
//!
//! The first file is a few lines and is read once, before the window is
//! up. Everything else (every write, and reading a transcript) happens on a
//! thread of the store's own, in the order asked, so the window never waits
//! for the disk. A run that keeps no settings has no folder for any of
//! this, and neither reads nor writes.

use super::transcript::Transcript;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use trill_assistant::{DEFAULT_MAX_TURNS, Effort, SessionId};

const FILE: &str = "assistant.json";
/// The fewest and the most steps a request can be allowed. The panel offers
/// a few values between; a file edited by hand is held to these.
pub const STEP_LIMITS: std::ops::RangeInclusive<u32> = 20..=10_000;
/// Conversations are remembered for this many projects, the most recently
/// used ones.
const MAX_CONVERSATIONS: usize = 200;

/// The conversation about one project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    session: String,
    /// When it was last used, in seconds since 1970, to know which to let
    /// go of first.
    #[serde(default)]
    used: u64,
    /// Which instructions the assistant was given when the conversation
    /// began ([`instructions`]). The CLI records them then and keeps to
    /// them for as long as the conversation lasts.
    #[serde(default)]
    instructions: String,
}

/// A mark that tells one wording of the assistant's instructions from
/// another: sixteen hex digits worked out from the text.
///
/// A conversation goes on under the instructions it began with, whatever
/// Trill has since learned to tell the assistant. One begun under other
/// instructions is therefore not taken up again: they may say nothing of
/// the tools the assistant is now given, nor that a page from the web is
/// to be read and never obeyed.
fn instructions() -> String {
    // FNV-1a: small, and the same on every machine and in every version.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in trill_assistant::DEFAULT_SYSTEM_PROMPT.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// What `assistant.json` holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Remembered {
    open: bool,
    width: f32,
    /// The model chosen in the panel, by the name the CLI is given; absent
    /// while the choice is left to the CLI.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// How hard the model is asked to think, in the CLI's word for it;
    /// absent while that is left to the CLI.
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    /// Whether the assistant may search the web and read pages. On unless
    /// the user switched it off.
    web: bool,
    /// The most steps one request may take; absent while it is Trill's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    step_limit: Option<u32>,
    /// By project file.
    conversations: BTreeMap<String, Kept>,
}

impl Default for Remembered {
    fn default() -> Self {
        Self {
            open: false,
            width: super::DEFAULT_WIDTH,
            model: None,
            effort: None,
            web: true,
            step_limit: None,
            conversations: BTreeMap::new(),
        }
    }
}

/// Something for the store's thread to do.
enum Job {
    Write { path: PathBuf, text: String },
    Remove(PathBuf),
    Read { session: SessionId, path: PathBuf },
}

/// The store's thread, started the first time there is something for it.
struct Worker {
    jobs: Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    /// Lets the thread finish what was asked of it: the last conversation
    /// is written before the program ends.
    fn drop(&mut self) {
        let (closed, _) = channel();
        drop(std::mem::replace(&mut self.jobs, closed));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct Store {
    /// The settings folder; `None` in a run that keeps nothing.
    folder: Option<PathBuf>,
    remembered: Remembered,
    worker: Option<Worker>,
    loaded_tx: Sender<(SessionId, Option<Transcript>)>,
    loaded: Receiver<(SessionId, Option<Transcript>)>,
    /// Woken when a transcript has been read.
    ctx: eframe::egui::Context,
}

/// A project file as the key conversations are remembered by: the whole
/// path, spelled one way.
fn key(project: &Path) -> String {
    let absolute = std::path::absolute(project).unwrap_or_else(|_| project.to_owned());
    let text = absolute.to_string_lossy().replace('\\', "/");
    // Windows does not tell `Song.trill` from `song.trill`.
    if cfg!(windows) { text.to_lowercase() } else { text }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// How often a file is tried again when it cannot be had this moment, and
/// how long the first wait is; each one after is that much longer.
const TRIES: u32 = 6;
const FIRST_WAIT: Duration = Duration::from_millis(40);

/// Does `attempt` until it succeeds, a few times over. On Windows a file
/// that has just been written is opened by the virus scanner for a moment,
/// and renaming or reading it in that moment is refused; a moment later it
/// is not. A file that is not there is not waited for. Only ever called on
/// the store's own thread, where a wait holds nobody up.
fn persistently<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut tried = 1;
    loop {
        match attempt() {
            Err(error) if tried < TRIES && error.kind() != std::io::ErrorKind::NotFound => {
                std::thread::sleep(FIRST_WAIT * tried);
                tried += 1;
            }
            result => return result,
        }
    }
}

/// Replaces `path` with `text` in one step.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.part", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = std::fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .and_then(|()| persistently(|| std::fs::rename(&temporary, path)));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

impl Store {
    /// Opens what is kept in `folder`. Reads `assistant.json`, which is a
    /// few lines; anything unreadable in it is the default.
    pub fn open(folder: Option<PathBuf>, ctx: &eframe::egui::Context) -> Self {
        let remembered = folder
            .as_deref()
            .and_then(|folder| std::fs::read(folder.join(FILE)).ok())
            .and_then(|text| serde_json::from_slice(&text).ok())
            .unwrap_or_default();
        let (loaded_tx, loaded) = channel();
        Self { folder, remembered, worker: None, loaded_tx, loaded, ctx: ctx.clone() }
    }

    /// Whether the panel was open when the program last closed, and how
    /// wide.
    pub fn panel(&self) -> (bool, f32) {
        (self.remembered.open, self.remembered.width)
    }

    pub fn set_panel(&mut self, open: bool, width: f32) {
        if (self.remembered.open, self.remembered.width) != (open, width) {
            self.remembered.open = open;
            self.remembered.width = width;
            self.save();
        }
    }

    /// The model chosen in the panel; `None` while the choice is the
    /// CLI's. A name that could not be given to the CLI (the file was
    /// edited by hand) is no choice.
    pub fn model(&self) -> Option<&str> {
        self.remembered.model.as_deref().filter(|name| trill_assistant::is_model_name(name))
    }

    pub fn set_model(&mut self, model: Option<&str>) {
        if self.remembered.model.as_deref() != model {
            self.remembered.model = model.map(str::to_owned);
            self.save();
        }
    }

    /// How hard the model is asked to think; `None` while that is the
    /// CLI's to decide. A word that is not one of the CLI's levels (the
    /// file was edited by hand) is no choice.
    pub fn effort(&self) -> Option<Effort> {
        self.remembered.effort.as_deref().and_then(Effort::named)
    }

    pub fn set_effort(&mut self, effort: Option<Effort>) {
        let name = effort.map(|effort| effort.name().to_owned());
        if self.remembered.effort != name {
            self.remembered.effort = name;
            self.save();
        }
    }

    /// Whether the assistant may search the web and read pages.
    pub fn web(&self) -> bool {
        self.remembered.web
    }

    pub fn set_web(&mut self, web: bool) {
        if self.remembered.web != web {
            self.remembered.web = web;
            self.save();
        }
    }

    /// The most steps one request may take.
    pub fn step_limit(&self) -> u32 {
        let limit = self.remembered.step_limit.unwrap_or(DEFAULT_MAX_TURNS);
        limit.clamp(*STEP_LIMITS.start(), *STEP_LIMITS.end())
    }

    pub fn set_step_limit(&mut self, limit: u32) {
        let kept = (limit != DEFAULT_MAX_TURNS).then_some(limit);
        if self.remembered.step_limit != kept {
            self.remembered.step_limit = kept;
            self.save();
        }
    }

    /// The conversation about `project`, if one is remembered that the
    /// assistant can carry on: one begun under the instructions it is given
    /// now.
    pub fn conversation(&self, project: &Path) -> Option<SessionId> {
        let kept = self.remembered.conversations.get(&key(project))?;
        SessionId::parse(&kept.session).filter(|_| kept.instructions == instructions())
    }

    /// Whether a conversation about `project` is remembered that was begun
    /// under other instructions than the assistant is given now, and so is
    /// not taken up again.
    pub fn has_outdated_conversation(&self, project: &Path) -> bool {
        let kept = self.remembered.conversations.get(&key(project));
        kept.is_some_and(|kept| SessionId::parse(&kept.session).is_some() && kept.instructions != instructions())
    }

    /// Remembers that `session` is the conversation about `project`.
    pub fn remember(&mut self, project: &Path, session: &SessionId) {
        let kept = Kept { session: session.as_str().to_owned(), used: now(), instructions: instructions() };
        let previous = self.remembered.conversations.insert(key(project), kept);
        if let Some(previous) = previous.filter(|previous| previous.session != session.as_str()) {
            self.remove_transcript(&previous.session);
        }
        while self.remembered.conversations.len() > MAX_CONVERSATIONS {
            let oldest = self.remembered.conversations.iter().min_by_key(|(_, kept)| kept.used);
            let Some(oldest) = oldest.map(|(project, _)| project.clone()) else { break };
            if let Some(kept) = self.remembered.conversations.remove(&oldest) {
                self.remove_transcript(&kept.session);
            }
        }
        self.save();
    }

    /// Lets go of `session` as the conversation about `project`, keeping
    /// what was kept of it: the conversation has gone on to another
    /// project. Nothing happens if the project's conversation is another.
    pub fn release(&mut self, project: &Path, session: &SessionId) {
        let key = key(project);
        if self.remembered.conversations.get(&key).is_some_and(|kept| kept.session == session.as_str()) {
            self.remembered.conversations.remove(&key);
            self.save();
        }
    }

    /// Forgets the conversation about `project`, and what was kept of it.
    pub fn forget(&mut self, project: &Path) {
        if let Some(kept) = self.remembered.conversations.remove(&key(project)) {
            self.remove_transcript(&kept.session);
            self.save();
        }
    }

    /// Keeps `transcript` as what `session` looked like.
    pub fn keep_transcript(&mut self, session: &SessionId, transcript: &Transcript) {
        if let Some(path) = self.transcript_path(session.as_str()) {
            self.ask(Job::Write { path, text: transcript.to_json() });
        }
    }

    /// Asks for what `session` looked like. It arrives through
    /// [`Self::loaded`], after anything asked to be written before.
    pub fn load_transcript(&mut self, session: &SessionId) {
        if let Some(path) = self.transcript_path(session.as_str()) {
            self.ask(Job::Read { session: session.clone(), path });
        }
    }

    /// A transcript that was asked for and has been read; `None` inside if
    /// there was none to read.
    pub fn loaded(&mut self) -> Option<(SessionId, Option<Transcript>)> {
        self.loaded.try_recv().ok()
    }

    fn transcript_path(&self, session: &str) -> Option<PathBuf> {
        // The id is a UUID (`SessionId::parse` accepts nothing else), so it
        // is safe as a file name.
        let id = SessionId::parse(session)?;
        Some(self.folder.as_ref()?.join("assistant").join("transcripts").join(format!("{id}.json")))
    }

    fn remove_transcript(&mut self, session: &str) {
        if let Some(path) = self.transcript_path(session) {
            self.ask(Job::Remove(path));
        }
    }

    fn save(&mut self) {
        let Some(folder) = &self.folder else { return };
        let path = folder.join(FILE);
        match serde_json::to_string_pretty(&self.remembered) {
            Ok(mut text) => {
                text.push('\n');
                self.ask(Job::Write { path, text });
            }
            Err(error) => log::warn!("the assistant's panel could not be remembered: {error}"),
        }
    }

    fn ask(&mut self, job: Job) {
        if self.worker.is_none() {
            let (jobs, queue) = channel();
            let (loaded, ctx) = (self.loaded_tx.clone(), self.ctx.clone());
            let thread =
                std::thread::Builder::new().name("assistant-store".into()).spawn(move || work(&queue, &loaded, &ctx));
            match thread {
                Ok(thread) => self.worker = Some(Worker { jobs, thread: Some(thread) }),
                Err(error) => {
                    log::warn!("the assistant's conversations cannot be kept: {error}");
                    return;
                }
            }
        }
        if let Some(worker) = &self.worker {
            let _ = worker.jobs.send(job);
        }
    }

    /// Waits until everything asked for has been done. For tests, which
    /// look at the files next.
    #[cfg(test)]
    pub fn flush(&mut self) {
        self.worker = None;
    }
}

fn work(queue: &Receiver<Job>, loaded: &Sender<(SessionId, Option<Transcript>)>, ctx: &eframe::egui::Context) {
    for job in queue {
        match job {
            Job::Write { path, text } => {
                if let Err(error) = write(&path, &text) {
                    log::warn!("could not write '{}': {error}", path.display());
                }
            }
            Job::Remove(path) => {
                let _ = std::fs::remove_file(path);
            }
            Job::Read { session, path } => {
                let text = persistently(|| std::fs::read_to_string(&path));
                let transcript = text.ok().and_then(|text| Transcript::from_json(&text));
                // The receiver is gone only when the window has closed.
                let _ = loaded.send((session, transcript));
                ctx.request_repaint();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::assistant::transcript::{Ending, TurnEnd};
    use std::time::Instant;

    fn id(last: char) -> SessionId {
        SessionId::parse(&format!("39367d22-e40b-4a04-9358-33e91538ff0{last}")).unwrap()
    }

    fn open(folder: &Path) -> Store {
        Store::open(Some(folder.to_owned()), &eframe::egui::Context::default())
    }

    fn wait_for_loaded(store: &mut Store) -> (SessionId, Option<Transcript>) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(loaded) = store.loaded() {
                return loaded;
            }
            assert!(Instant::now() < deadline, "the transcript never arrived");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn the_panel_and_each_projects_conversation_come_back() {
        let home = tempfile::tempdir().unwrap();
        let mut store = open(home.path());
        assert_eq!(store.panel(), (false, crate::ui::assistant::DEFAULT_WIDTH), "closed until it is asked for");
        assert_eq!(store.conversation(Path::new("songs/a.trill")), None);
        assert_eq!(store.model(), None, "the model is the CLI's choice until one is chosen");
        assert_eq!(store.effort(), None, "and so is how hard it thinks");
        assert!(store.web(), "the web is on until it is switched off");
        assert_eq!(store.step_limit(), DEFAULT_MAX_TURNS);
        store.flush();
        assert!(!home.path().join(FILE).exists(), "nothing is written for having looked");

        store.set_panel(true, 412.0);
        store.set_model(Some("sonnet"));
        store.set_effort(Some(Effort::ExtraHigh));
        store.set_web(false);
        store.set_step_limit(2000);
        store.remember(Path::new("songs/a.trill"), &id('a'));
        store.remember(Path::new("songs/b.trill"), &id('b'));
        let mut transcript = Transcript::default();
        transcript.push_user("make it swing");
        transcript.end(TurnEnd::unchanged(Ending::Stopped));
        store.keep_transcript(&id('a'), &transcript);
        drop(store);

        let mut store = open(home.path());
        assert_eq!(store.panel(), (true, 412.0));
        assert_eq!(store.model(), Some("sonnet"));
        assert_eq!((store.effort(), store.web(), store.step_limit()), (Some(Effort::ExtraHigh), false, 2000));
        assert_eq!(store.conversation(Path::new("songs/a.trill")), Some(id('a')));
        assert_eq!(store.conversation(Path::new("songs/b.trill")), Some(id('b')));
        assert_eq!(store.conversation(Path::new("songs/c.trill")), None);
        // However the path is spelled, it is the same file.
        assert_eq!(store.conversation(&std::path::absolute("songs/a.trill").unwrap()), Some(id('a')));

        store.load_transcript(&id('a'));
        assert_eq!(wait_for_loaded(&mut store), (id('a'), Some(transcript)));
        store.load_transcript(&id('b'));
        assert_eq!(wait_for_loaded(&mut store), (id('b'), None), "none was kept for that one");

        // Back to the CLI's own choices and Trill's own limit: they are
        // gone from the file.
        store.set_model(None);
        store.set_effort(None);
        store.set_step_limit(DEFAULT_MAX_TURNS);
        store.set_web(true);
        store.flush();
        let kept = std::fs::read_to_string(home.path().join(FILE)).unwrap();
        assert!(!kept.contains("model") && !kept.contains("sonnet"), "{kept}");
        assert!(!kept.contains("effort") && !kept.contains("step_limit") && kept.contains("\"web\": true"), "{kept}");
        let store = open(home.path());
        assert_eq!(
            (store.model(), store.effort(), store.web(), store.step_limit()),
            (None, None, true, DEFAULT_MAX_TURNS)
        );
    }

    #[test]
    fn a_new_conversation_takes_the_place_of_the_old_and_forgetting_removes_it() {
        let home = tempfile::tempdir().unwrap();
        let transcripts = home.path().join("assistant").join("transcripts");
        let project = Path::new("songs/a.trill");
        let mut store = open(home.path());
        store.remember(project, &id('a'));
        store.keep_transcript(&id('a'), &Transcript::default());
        store.flush();
        assert!(transcripts.join(format!("{}.json", id('a'))).exists());

        store.remember(project, &id('b'));
        store.keep_transcript(&id('b'), &Transcript::default());
        store.flush();
        assert!(!transcripts.join(format!("{}.json", id('a'))).exists(), "the old one's record goes");
        assert!(transcripts.join(format!("{}.json", id('b'))).exists());
        assert_eq!(std::fs::read_dir(&transcripts).unwrap().count(), 1, "and no temporary file is left behind");

        // A conversation that goes on to another project is let go of
        // here and keeps its record.
        let other = Path::new("songs/b.trill");
        store.release(project, &id('a'));
        assert_eq!(store.conversation(project), Some(id('b')), "only the conversation named is let go of");
        store.release(project, &id('b'));
        store.remember(other, &id('b'));
        store.flush();
        assert_eq!((store.conversation(project), store.conversation(other)), (None, Some(id('b'))));
        assert!(transcripts.join(format!("{}.json", id('b'))).exists(), "what was kept of it stays");

        store.forget(other);
        store.flush();
        assert_eq!(store.conversation(other), None);
        assert_eq!(std::fs::read_dir(&transcripts).unwrap().count(), 0);
        assert_eq!(open(home.path()).conversation(other), None);
    }

    #[test]
    fn a_conversation_begun_under_other_instructions_is_not_taken_up_again() {
        let home = tempfile::tempdir().unwrap();
        let project = Path::new("songs/a.trill");
        let mut store = open(home.path());
        store.remember(project, &id('a'));
        assert_eq!(store.conversation(project), Some(id('a')));
        assert!(!store.has_outdated_conversation(project));
        store.flush();
        let file = home.path().join(FILE);
        let kept = std::fs::read_to_string(&file).unwrap();
        let mark = instructions();
        assert_eq!(mark.len(), 16);
        assert!(kept.contains(&format!("\"instructions\": \"{mark}\"")), "{kept}");

        // As an earlier version of Trill, with other instructions, left it;
        // and as one from before the mark was kept at all.
        for earlier in [
            kept.replace(&mark, "0123456789abcdef"),
            kept.replace(&format!(",\n      \"instructions\": \"{mark}\""), ""),
        ] {
            assert_ne!(earlier, kept);
            std::fs::write(&file, earlier).unwrap();
            let mut store = open(home.path());
            assert_eq!(store.conversation(project), None, "it would go on under the old instructions");
            assert!(store.has_outdated_conversation(project));
            assert!(!store.has_outdated_conversation(Path::new("songs/b.trill")));
            // The conversation that takes its place is of today.
            store.remember(project, &id('b'));
            assert_eq!(store.conversation(project), Some(id('b')));
            assert!(!store.has_outdated_conversation(project));
        }
    }

    #[test]
    fn only_so_many_conversations_are_remembered() {
        let home = tempfile::tempdir().unwrap();
        let mut store = open(home.path());
        for index in 0..MAX_CONVERSATIONS + 5 {
            store.remember(Path::new(&format!("songs/{index}.trill")), &id('a'));
            // Make the first few the oldest, whatever the clock says.
            if let Some(kept) = store.remembered.conversations.get_mut(&key(Path::new(&format!("songs/{index}.trill"))))
            {
                kept.used = index as u64;
            }
        }
        assert_eq!(store.remembered.conversations.len(), MAX_CONVERSATIONS);
        assert_eq!(store.conversation(Path::new("songs/0.trill")), None, "the least recently used went");
        assert!(store.conversation(Path::new(&format!("songs/{}.trill", MAX_CONVERSATIONS + 4))).is_some());
    }

    #[test]
    fn a_file_held_for_a_moment_is_tried_again_and_a_missing_one_is_not() {
        use std::io::{Error, ErrorKind};
        // Refused twice, as when a scanner has the file open, then let go.
        let mut calls = 0;
        let result = persistently(|| {
            calls += 1;
            if calls < 3 { Err(Error::from(ErrorKind::PermissionDenied)) } else { Ok(calls) }
        });
        assert_eq!(result.unwrap(), 3);

        // Held for good: given up after a few tries, with the reason.
        let mut calls = 0;
        let result: std::io::Result<()> = persistently(|| {
            calls += 1;
            Err(Error::from(ErrorKind::PermissionDenied))
        });
        assert_eq!(result.unwrap_err().kind(), ErrorKind::PermissionDenied);
        assert_eq!(calls, TRIES);

        // Not there: there is nothing to wait for.
        let mut calls = 0;
        let started = Instant::now();
        let result: std::io::Result<()> = persistently(|| {
            calls += 1;
            Err(Error::from(ErrorKind::NotFound))
        });
        assert_eq!((result.unwrap_err().kind(), calls), (ErrorKind::NotFound, 1));
        assert!(started.elapsed() < FIRST_WAIT);
    }

    #[test]
    fn a_damaged_file_or_no_folder_does_no_harm() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(FILE), "{ not json").unwrap();
        assert_eq!(open(home.path()).panel(), (false, crate::ui::assistant::DEFAULT_WIDTH));
        // Names this version does not know are ignored, and an id that is
        // not an id is no conversation.
        let text = r#"{ "open": true, "new_in_version_nine": 1, "model": "--dangerously-skip-permissions",
            "effort": "--tools=Bash", "step_limit": 4000000000,
            "conversations": { "x": { "session": "--resume=evil" } } }"#;
        std::fs::write(home.path().join(FILE), text).unwrap();
        let store = open(home.path());
        assert!(store.panel().0);
        assert_eq!(store.model(), None, "a name the command line would take for an option is no model");
        assert_eq!(store.effort(), None, "and a word that is not a level is no effort");
        assert_eq!(store.step_limit(), *STEP_LIMITS.end(), "a limit out of all proportion is held to the most");
        assert!(store.web(), "a file that says nothing of the web leaves it on");
        assert_eq!(store.remembered.conversations.len(), 1);
        assert!(store.remembered.conversations.values().all(|kept| SessionId::parse(&kept.session).is_none()));

        // A run that keeps nothing: nothing is read, nothing written, and
        // nothing ever arrives.
        let mut nowhere = Store::open(None, &eframe::egui::Context::default());
        nowhere.set_panel(true, 400.0);
        nowhere.remember(Path::new("a.trill"), &id('a'));
        nowhere.keep_transcript(&id('a'), &Transcript::default());
        nowhere.load_transcript(&id('a'));
        assert!(nowhere.worker.is_none(), "no thread is started for it");
        assert_eq!(nowhere.loaded(), None);
        assert_eq!(nowhere.conversation(Path::new("a.trill")), Some(id('a')), "it lasts as long as the window");
    }
}
