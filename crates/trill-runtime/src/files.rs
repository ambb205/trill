//! The files of a shared song, between the programs that hold it.
//!
//! The song says what each file it plays *is* (`trill_core::files::FileIs`)
//! and every machine has its own place for it; no path is said or taken
//! (`trill_engine::session::content`). This is what gets the bytes from
//! whoever has them to whoever has not:
//!
//! * **This program says what its own files are.** A thread of its own
//!   reads each file the song names that is on this disk, once, and again
//!   only when the file was written since ([`Reading`]); what it found is
//!   written into the song as a property of the clip.
//! * **It hands out what its open song plays and it has**, to whoever is
//!   in the song and asks by content: [`trill_together::files::Offered`],
//!   a list made here from the song and the disk. Nothing a program says
//!   adds to it.
//! * **It asks for what the song plays and it has not got**, from whoever
//!   said they have it, a stretch at a time, and takes in only the bytes
//!   it asked for ([`trill_together::files::Received`]): into the folder
//!   of files from other programs, under a name made here, a file of the
//!   song once all of it is here and is what was asked for.
//!
//! None of it is done on the thread that connects the session to the wire
//! ([`crate::together`]), which only passes things on: disks are read and
//! written by the two threads here, and what is said of the song goes
//! ahead of a file's bytes on every connection (`trill_together::net`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, SystemTime};
use trill_core::ClipId;
use trill_core::files::{Content, FileIs};
use trill_engine::FileHere;
use trill_together::files::{Begun, Offered, Received, Shelved, Took, content_of};
use trill_together::{LinkId, MAX_PIECE, MAX_WANT, Message, Pieces};

/// The folder files from other programs are kept in, in the program's own
/// folder.
pub const RECEIVED_FOLDER: &str = "received";
/// How many bytes of files may wait to be said to one program: beyond
/// that the thread that hands out reads no further for it, until the
/// connection has taken some.
const QUEUED: usize = 1 << 20;
/// How many bytes one program may have asked for that were not read yet.
/// One that asks for more is let go of: nobody who takes what it asks for
/// gets near it.
const MAX_PENDING: u64 = 32 << 20;
/// How long the thread that hands out waits when every connection it
/// reads for has enough waiting.
const DRAINING: Duration = Duration::from_millis(2);
/// How many bytes are asked for ahead when a file begins to arrive, and
/// the fewest that ever are.
const FIRST_WINDOW: u64 = 256 << 10;
const LEAST_WINDOW: u64 = 2 * MAX_PIECE as u64;
/// How long what is asked for ahead should take to arrive: the window is
/// what arrived in that time. What the other side has to say of the song
/// meanwhile waits behind no more than this of a file.
const AHEAD: Duration = Duration::from_millis(250);
/// How long a program may leave a question for bytes unanswered before
/// someone else is asked.
const STALLED: Duration = Duration::from_secs(20);
/// How long bytes from a program that is no longer asked for a file are
/// let pass: they were asked for, and were on their way.
const FORGIVEN: Duration = Duration::from_secs(30);

/// A file that is on its way from another program, as a front end shows
/// it.
#[derive(Clone, Debug, PartialEq)]
pub struct OnItsWay {
    /// The name its maker has it under.
    pub name: String,
    pub bytes: u64,
    /// How many of them are here.
    pub here: u64,
    /// Who it is coming from, once someone was asked.
    pub from: Option<String>,
}

// ---- reading what one's own files are ---------------------------------------

/// What the thread that reads is asked: the files the song names, to be
/// looked at on this disk.
struct Look {
    files: Vec<FileHere>,
    /// The folder files from other programs are kept in.
    received: PathBuf,
}

/// What looking at the song's files on this disk came to. A look answers
/// at once with what it knows, and again after each file it had to read.
#[derive(Default)]
struct Looked {
    /// Clips whose file is on this disk and is not what the song says it
    /// is, or of which the song does not say: with where it is and what
    /// it was found to be.
    say: Vec<(ClipId, String, FileIs)>,
    /// Files of the song this machine has, to hand out.
    have: Vec<(Content, Shelved)>,
    /// Files the song says it plays that are not here: only ones whose
    /// place is in the folder of files from other programs.
    lack: Vec<FileIs>,
    /// Whether this is the last word of the look.
    all: bool,
}

/// What this machine has read of a file: what it was, and when it had
/// last been written then.
struct Read {
    bytes: u64,
    written: Option<SystemTime>,
    content: Content,
}

/// The thread that reads files to say what they are. One look at a time;
/// a file is read once, and again only if it was written since.
fn read(looks: Receiver<Look>, looked: Sender<Looked>) {
    let mut index: HashMap<String, Read> = HashMap::new();
    while let Ok(mut look) = looks.recv() {
        // (Of several looks that waited, the last is the song as it is.)
        while let Ok(later) = looks.try_recv() {
            look = later;
        }
        let mut by_path: HashMap<String, Vec<&FileHere>> = HashMap::new();
        for file in &look.files {
            by_path.entry(file.path.clone()).or_default().push(file);
        }
        let mut first = Looked::default();
        let mut unread: Vec<(String, u64, Option<SystemTime>)> = Vec::new();
        let found_to_be = |content: &Content, written, path: &str, clips: &[&FileHere], into: &mut Looked| {
            for clip in clips.iter().filter(|clip| clip.is.as_ref().map(FileIs::content).as_ref() != Some(content)) {
                into.say.push((clip.clip, path.to_owned(), FileIs::new(content.clone(), path)));
            }
            into.have.push((content.clone(), Shelved { path: PathBuf::from(path), written }));
        };
        for (path, clips) in &by_path {
            let found = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file);
            // A file that came from another program lies under its hash:
            // it was held against that when it arrived, and is never
            // written again.
            let kept = clips.iter().find_map(|clip| clip.is.as_ref().filter(|is| arrived(&look.received, path, is)));
            match (found, kept) {
                (Some(found), Some(is)) if found.len() == is.bytes => {
                    first.have.push((is.content(), Shelved { path: PathBuf::from(path), written: None }));
                }
                // (Under its name and not its length: not it.)
                (Some(_), Some(is)) => first.lack.push(is.clone()),
                (Some(found), None) => {
                    let written = found.modified().ok();
                    match index.get(path) {
                        Some(read) if read.bytes == found.len() && read.written == written => {
                            found_to_be(&read.content, written, path, clips, &mut first);
                        }
                        _ => unread.push((path.to_string(), found.len(), written)),
                    }
                }
                (None, Some(is)) => first.lack.push(is.clone()),
                // A file of one's own that is gone is a missing file, as
                // it always was: nobody is asked for it by this.
                (None, None) => {}
            }
        }
        first.all = unread.is_empty();
        if looked.send(first).is_err() {
            return;
        }
        let last = unread.len().saturating_sub(1);
        for (at, (path, bytes, written)) in unread.into_iter().enumerate() {
            let mut next = Looked { all: at == last, ..Looked::default() };
            match content_of(Path::new(&path), |_| true) {
                // Written while it was read: it is read again at the next look.
                Ok(Some(content)) if content.bytes == bytes => {
                    found_to_be(&content, written, &path, &by_path[&path], &mut next);
                    index.insert(path, Read { bytes, written, content });
                }
                Ok(_) => {}
                Err(error) => log::warn!("a file of the song could not be read to say what it is: {error}"),
            }
            if looked.send(next).is_err() {
                return;
            }
        }
    }
}

/// Whether `path` is the place a file that is `is` has in `received`, the
/// folder of files from other programs.
fn arrived(received: &Path, path: &str, is: &FileIs) -> bool {
    Path::new(path) == received.join(is.kept_as())
}

// ---- handing out and taking in ----------------------------------------------

/// What the thread that hands out and takes in is asked.
enum Job {
    /// The files that are handed out from here on.
    Offer(Offered),
    /// Someone asked for bytes of a file.
    Serve { link: LinkId, file: Content, at: u64, len: u64 },
    /// A connection is gone: nothing more is read for it.
    Gone(LinkId),
    /// A file is wanted: how much of it is here?
    Begin(FileIs),
    /// Bytes that were asked for arrived.
    Piece { file: Content, at: u64, bytes: Vec<u8> },
    /// A file is no longer wanted.
    LetGo(Content),
}

/// What it says back.
enum Done {
    /// How much of a file that was begun is here, or that it is here whole.
    Begun {
        file: Content,
        begun: Result<Begun, String>,
    },
    Took {
        file: Content,
        took: Took,
    },
    /// A program asked for more than it took.
    TooMuch(LinkId),
}

/// A stretch of a file that someone asked for and that is not read yet.
struct Owed {
    file: Content,
    at: u64,
    left: u64,
}

/// The thread that reads files for whoever asked and writes what arrives.
fn pass(jobs: Receiver<Job>, done: Sender<Done>, pieces: Pieces, dir: PathBuf) {
    let mut offered = Offered::default();
    let mut received = Received::new(dir);
    let mut owed: HashMap<LinkId, Vec<Owed>> = HashMap::new();
    loop {
        // While something is owed, jobs are looked for and not waited
        // for longer than the connections take to drain.
        let job = match owed.is_empty() {
            true => jobs.recv().map_err(|_| RecvTimeoutError::Disconnected),
            false => jobs.recv_timeout(Duration::ZERO),
        };
        match job {
            Ok(Job::Offer(now)) => offered = now,
            Ok(Job::Serve { link, file, at, len }) => {
                // (No further than the file's end, whatever was asked.)
                let len = len.min(file.bytes.saturating_sub(at));
                let pending = owed.entry(link).or_default();
                let asked: u64 = pending.iter().map(|owed| owed.left).sum();
                if asked + len > MAX_PENDING {
                    owed.remove(&link);
                    let _ = done.send(Done::TooMuch(link));
                } else {
                    pending.push(Owed { file, at, left: len });
                }
            }
            Ok(Job::Gone(link)) => drop(owed.remove(&link)),
            Ok(Job::Begin(is)) => {
                let begun = received.begin(&is).map_err(|error| error.to_string());
                let _ = done.send(Done::Begun { file: is.content(), begun });
            }
            Ok(Job::Piece { file, at, bytes }) => {
                let took = received.take(&file, at, &bytes);
                let _ = done.send(Done::Took { file, took });
            }
            Ok(Job::LetGo(file)) => received.let_go(&file),
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // A piece for every connection that has room for one: each in its
        // turn, so that one that asks for much holds nobody else up.
        let mut read_any = false;
        owed.retain(|link, pending| {
            let Some(first) = pending.first_mut() else { return false };
            match pieces.waiting(*link) {
                None => return false,
                Some(waiting) if waiting >= QUEUED => return true,
                Some(_) => {}
            }
            read_any = true;
            let answer = match offered.read(&first.file, first.at, first.left) {
                Some(bytes) => {
                    let read = bytes.len() as u64;
                    let piece = Message::Piece { file: first.file.clone(), at: first.at, bytes };
                    (first.at, first.left) = (first.at + read, first.left - read);
                    piece
                }
                // Not here, whatever the reason: the rest of what was
                // asked for of it is not answered piece by piece.
                None => {
                    first.left = 0;
                    Message::Nothing { file: first.file.clone() }
                }
            };
            if pieces.say(*link, &answer).is_none() {
                return false;
            }
            if first.left == 0 {
                pending.remove(0);
            }
            !pending.is_empty()
        });
        if !owed.is_empty() && !read_any {
            std::thread::sleep(DRAINING);
        }
    }
}

// ---- what the thread that drives the song holds -----------------------------

/// A file that is wanted and on its way.
struct Incoming {
    is: FileIs,
    /// Who is being asked for it.
    source: Option<LinkId>,
    /// Whether it was asked how much of it is here, and answered.
    begun: Option<bool>,
    /// How many bytes are written here, as far as was heard.
    here: u64,
    /// Where the next bytes that arrive have to begin.
    expect: u64,
    /// Up to where bytes were asked for.
    asked_to: u64,
    /// How many bytes are asked for ahead.
    window: u64,
    /// For the rate: when counting began, and how much arrived since.
    since: Instant,
    arrived: u64,
    /// When bytes last arrived, or were last asked for.
    heard: Instant,
}

/// What came of something that was heard, for the thread that drives the
/// song to act on.
pub(crate) enum Heard {
    /// Nothing more to do.
    Fine,
    /// To be said to the program that said it.
    Answer(Message),
    /// The program said what nobody who holds the song with this one
    /// says: it is let go of, and why.
    LetGo(&'static str),
}

/// Something for the thread that drives the song to do after the files
/// were tended.
pub(crate) enum Tended {
    /// To be said to one program.
    Say(LinkId, Message),
    /// To be said to the user.
    Tell(String),
    /// What a clip's file was found to be, to be written into the song.
    SayFile(ClipId, String, FileIs),
    /// A file arrived: the song plays it from here on.
    Here(PathBuf),
    /// A program is to be let go of, and why.
    LetGo(LinkId, &'static str),
}

/// The files of the shared song, as the thread that drives it holds them.
pub(crate) struct Files {
    looks: Sender<Look>,
    looked: Receiver<Looked>,
    jobs: Sender<Job>,
    done: Receiver<Done>,
    /// Whether a look is under way, and when the last one was asked for.
    looking: bool,
    asked: Option<(u64, Instant)>,
    /// What this machine has of the song's files, as of the last look,
    /// and what the look that is under way has found so far.
    have: HashMap<Content, Shelved>,
    finding: HashMap<Content, Shelved>,
    lacking: HashSet<Content>,
    /// What each program was told this one has.
    told: HashMap<LinkId, HashSet<Content>>,
    /// What each program said it has.
    theirs: HashMap<LinkId, HashSet<Content>>,
    incoming: HashMap<Content, Incoming>,
    /// Programs that are not asked for a file again: what they sent as
    /// it was not it, or they have not got it after all.
    not_from: HashSet<(LinkId, Content)>,
    /// Bytes that may still arrive from a program that is no longer asked.
    forgiven: HashMap<(LinkId, Content), Instant>,
    /// Files this machine could not keep: not asked for again this run.
    failed: HashSet<Content>,
}

/// How often the song's files are looked at while nothing changes.
const LOOK_EVERY: Duration = Duration::from_millis(1500);
/// How soon after a change they are looked at again at the earliest.
const LOOK_AFTER: Duration = Duration::from_millis(200);
/// How many files a program's saying of what it has may come to.
const MAX_THEIRS: usize = 65_536;

impl Files {
    /// Starts the two threads. Files from other programs are kept in
    /// `dir`.
    pub fn new(pieces: Pieces, dir: PathBuf) -> std::io::Result<Self> {
        let (looks, to_look) = channel();
        let (say_looked, looked) = channel();
        std::thread::Builder::new().name("trill-files-read".into()).spawn(move || read(to_look, say_looked))?;
        let (jobs, to_do) = channel();
        let (say_done, done) = channel();
        std::thread::Builder::new()
            .name("trill-files-pass".into())
            .spawn(move || pass(to_do, say_done, pieces, dir))?;
        Ok(Self {
            looks,
            looked,
            jobs,
            done,
            looking: false,
            asked: None,
            have: HashMap::new(),
            finding: HashMap::new(),
            lacking: HashSet::new(),
            told: HashMap::new(),
            theirs: HashMap::new(),
            incoming: HashMap::new(),
            not_from: HashSet::new(),
            forgiven: HashMap::new(),
            failed: HashSet::new(),
        })
    }

    /// Whether the song's files are to be looked at now: the song has
    /// changed since they were, or it is a while ago.
    pub fn look_is_due(&self, revision: u64) -> bool {
        match (self.looking, self.asked) {
            (true, _) => false,
            (false, None) => true,
            (false, Some((seen, when))) => when.elapsed() >= if seen == revision { LOOK_EVERY } else { LOOK_AFTER },
        }
    }

    /// Has the files the song names looked at on this disk, away from
    /// here: what comes of it is taken up by [`Files::tend`].
    pub fn look(&mut self, revision: u64, files: Vec<FileHere>, received: PathBuf) {
        (self.looking, self.asked) = (true, Some((revision, Instant::now())));
        // What is handed out stays what it is while the look goes on:
        // what it finds is added as it is found, and what it no longer
        // finds goes when it is over.
        self.finding.clear();
        self.lacking.clear();
        let _ = self.looks.send(Look { files, received });
    }

    /// The song is another, or nobody is in it any more: nothing is on
    /// its way, and nothing is handed out.
    pub fn forget(&mut self) {
        for file in self.incoming.keys() {
            let _ = self.jobs.send(Job::LetGo(file.clone()));
        }
        self.incoming.clear();
        self.have.clear();
        self.finding.clear();
        self.lacking.clear();
        self.told.clear();
        self.theirs.clear();
        self.not_from.clear();
        self.forgiven.clear();
        self.asked = None;
        let _ = self.jobs.send(Job::Offer(Offered::default()));
    }

    /// A connection is gone.
    pub fn lost(&mut self, link: LinkId) {
        self.told.remove(&link);
        self.theirs.remove(&link);
        self.not_from.retain(|(from, _)| *from != link);
        self.forgiven.retain(|(from, _), _| *from != link);
        let _ = self.jobs.send(Job::Gone(link));
        for incoming in self.incoming.values_mut().filter(|incoming| incoming.source == Some(link)) {
            // It goes on from where it is, from whoever else has it.
            (incoming.source, incoming.begun) = (None, None);
        }
    }

    /// Something of files was heard from a program that holds the song
    /// with this one.
    pub fn heard(&mut self, link: LinkId, message: Message) -> Heard {
        match message {
            Message::Have { files } => {
                let theirs = self.theirs.entry(link).or_default();
                if theirs.len() + files.len() > MAX_THEIRS {
                    return Heard::LetGo("it says it has more files than a song plays");
                }
                theirs.extend(files);
                Heard::Fine
            }
            Message::Want { file, at, len } => {
                if len == 0 || len > MAX_WANT {
                    return Heard::LetGo("it asked for a stretch of a file that nobody asks for");
                }
                // What is not handed out is answered at once, and the
                // same whatever the reason.
                if !self.have.contains_key(&file) || at >= file.bytes {
                    return Heard::Answer(Message::Nothing { file });
                }
                let _ = self.jobs.send(Job::Serve { link, file, at, len });
                Heard::Fine
            }
            Message::Nothing { file } => {
                self.theirs.entry(link).or_default().remove(&file);
                if let Some(incoming) = self.incoming.get_mut(&file).filter(|incoming| incoming.source == Some(link)) {
                    self.not_from.insert((link, file));
                    (incoming.source, incoming.begun) = (None, None);
                }
                Heard::Fine
            }
            Message::Piece { file, at, bytes } => {
                let len = bytes.len() as u64;
                let asked = self.incoming.get_mut(&file).filter(|incoming| {
                    incoming.source == Some(link) && incoming.expect == at && at + len <= incoming.asked_to
                });
                let Some(incoming) = asked else {
                    // Bytes that were asked for and were on their way when
                    // their file was no longer wanted from there pass.
                    return match self.forgiven.get(&(link, file)).is_some_and(|until| *until > Instant::now()) {
                        true => Heard::Fine,
                        false => Heard::LetGo("it sent bytes of a file that were not asked for"),
                    };
                };
                incoming.expect += len;
                incoming.arrived += len;
                incoming.heard = Instant::now();
                // As much is asked for ahead as arrives in a moment.
                let counted = incoming.since.elapsed();
                if counted >= AHEAD {
                    let rate = incoming.arrived as f64 / counted.as_secs_f64();
                    incoming.window = ((rate * AHEAD.as_secs_f64()) as u64).clamp(LEAST_WINDOW, MAX_WANT);
                    (incoming.since, incoming.arrived) = (Instant::now(), 0);
                }
                let _ = self.jobs.send(Job::Piece { file, at, bytes });
                Heard::Fine
            }
            _ => Heard::Fine,
        }
    }

    /// Tends the files: takes up what the two threads found, tells the
    /// programs in `synced` what this one has, and asks for what it lacks.
    /// `name` says who a connection is, for a sentence.
    pub fn tend(&mut self, synced: &[LinkId], name: &dyn Fn(LinkId) -> String) -> Vec<Tended> {
        let mut out = Vec::new();
        let mut offer_anew = false;
        while let Ok(looked) = self.looked.try_recv() {
            for (clip, path, is) in looked.say {
                out.push(Tended::SayFile(clip, path, is));
            }
            offer_anew |= !looked.have.is_empty();
            self.finding.extend(looked.have.iter().cloned());
            self.have.extend(looked.have);
            // What is lacked now is wanted; what was wanted and is no
            // longer named by the song is let go of when the look is over.
            for is in looked.lack {
                let file = is.content();
                self.lacking.insert(file.clone());
                if !self.failed.contains(&file) && !self.incoming.contains_key(&file) {
                    let now = Instant::now();
                    let incoming = Incoming {
                        is,
                        source: None,
                        begun: None,
                        here: 0,
                        expect: 0,
                        asked_to: 0,
                        window: FIRST_WINDOW,
                        since: now,
                        arrived: 0,
                        heard: now,
                    };
                    self.incoming.insert(file, incoming);
                }
            }
            if looked.all && self.looking {
                self.looking = false;
                // What the look did not find is no longer handed out (a
                // file that arrived meanwhile is: nobody looked for it).
                let found = std::mem::take(&mut self.finding);
                let before = self.have.len();
                self.have.retain(|file, shelved| found.contains_key(file) || shelved.written.is_none());
                offer_anew |= self.have.len() != before;
                let lacking = std::mem::take(&mut self.lacking);
                self.want_only(&lacking);
            }
        }
        while let Ok(done) = self.done.try_recv() {
            match done {
                Done::Begun { file, begun } => self.begun(file, begun, &mut out),
                Done::Took { file, took } => self.took(file, took, name, &mut out),
                Done::TooMuch(link) => {
                    out.push(Tended::LetGo(link, "it asked for more of the song's files than it took"));
                }
            }
        }
        if offer_anew {
            let _ = self.jobs.send(Job::Offer(Offered::new(self.have.clone())));
        }
        // What this program has, said to whoever was not told.
        for link in synced {
            let told = self.told.entry(*link).or_default();
            let mut news: Vec<Content> = self.have.keys().filter(|file| !told.contains(*file)).cloned().collect();
            news.sort();
            for some in news.chunks(trill_together::MAX_HAVE) {
                out.push(Tended::Say(*link, Message::Have { files: some.to_vec() }));
            }
            told.extend(self.have.keys().cloned());
        }
        self.ask(synced, &mut out);
        let now = Instant::now();
        self.forgiven.retain(|_, until| *until > now);
        out
    }

    /// How much of a file that was begun is here.
    fn begun(&mut self, file: Content, begun: Result<Begun, String>, out: &mut Vec<Tended>) {
        let Some(incoming) = self.incoming.get_mut(&file) else { return };
        match begun {
            Ok(Begun::Here(path)) => {
                self.incoming.remove(&file);
                self.have.insert(file.clone(), Shelved { path: path.clone(), written: None });
                let _ = self.jobs.send(Job::Offer(Offered::new(self.have.clone())));
                out.push(Tended::Here(path));
            }
            Ok(Begun::From(here)) => {
                (incoming.here, incoming.expect, incoming.asked_to) = (here, here, here);
                (incoming.begun, incoming.heard) = (Some(true), Instant::now());
                (incoming.since, incoming.arrived) = (Instant::now(), 0);
            }
            Err(why) => {
                let named = incoming.is.name.clone();
                self.incoming.remove(&file);
                self.failed.insert(file);
                out.push(Tended::Tell(format!(
                    "'{named}', a file of the shared song, cannot be kept on this machine ({why}). The song goes on \
                     without it here."
                )));
            }
        }
    }

    /// What came of bytes that arrived.
    fn took(&mut self, file: Content, took: Took, name: &dyn Fn(LinkId) -> String, out: &mut Vec<Tended>) {
        let Some(incoming) = self.incoming.get_mut(&file) else { return };
        match took {
            Took::More(here) => incoming.here = here,
            Took::Whole(path) => {
                self.incoming.remove(&file);
                self.have.insert(file.clone(), Shelved { path: path.clone(), written: None });
                let _ = self.jobs.send(Job::Offer(Offered::new(self.have.clone())));
                out.push(Tended::Here(path));
            }
            Took::Other => {
                let (named, from) = (incoming.is.name.clone(), incoming.source);
                if let Some(from) = from {
                    self.not_from.insert((from, file.clone()));
                    self.forgiven.insert((from, file), Instant::now() + FORGIVEN);
                }
                (incoming.source, incoming.begun) = (None, None);
                let who = from.map_or_else(|| "another program".to_owned(), name);
                out.push(Tended::Tell(format!(
                    "What arrived from {who} as '{named}' was not the file the song names: its bytes are others. It \
                     was thrown away, and the file is asked for from whoever else has it."
                )));
            }
            // (The thread that takes in was told of bytes this one did
            // not ask for: it wrote nothing, and whoever sent them is
            // found out where they are heard.)
            Took::NotAsked => {}
            Took::Failed(why) => {
                let (named, from) = (incoming.is.name.clone(), incoming.source);
                if let Some(from) = from {
                    self.forgiven.insert((from, file.clone()), Instant::now() + FORGIVEN);
                }
                self.incoming.remove(&file);
                let _ = self.jobs.send(Job::LetGo(file.clone()));
                self.failed.insert(file);
                out.push(Tended::Tell(format!(
                    "'{named}', a file of the shared song, could not be written on this machine ({why}). What \
                     arrived of it is kept, and it is gone on with when the song is joined again."
                )));
            }
        }
    }

    /// Asks for what is lacked: each file from one program that said it
    /// has it, as far ahead as arrives in a moment.
    fn ask(&mut self, synced: &[LinkId], out: &mut Vec<Tended>) {
        let now = Instant::now();
        for (file, incoming) in &mut self.incoming {
            // Someone who does not answer is not waited for for good.
            if let Some(source) = incoming.source
                && incoming.begun == Some(true)
                && incoming.asked_to > incoming.expect
                && now.duration_since(incoming.heard) > STALLED
            {
                self.not_from.insert((source, file.clone()));
                self.forgiven.insert((source, file.clone()), now + FORGIVEN);
                (incoming.source, incoming.begun) = (None, None);
            }
            if incoming.source.is_none() {
                let has = |link: &&LinkId| {
                    self.theirs.get(*link).is_some_and(|theirs| theirs.contains(file))
                        && !self.not_from.contains(&(**link, file.clone()))
                };
                let Some(source) = synced.iter().find(has) else { continue };
                incoming.source = Some(*source);
            }
            match incoming.begun {
                None => {
                    // How much of it is here is asked first: of a file
                    // that was half here, the rest is asked for.
                    incoming.begun = Some(false);
                    let _ = self.jobs.send(Job::Begin(incoming.is.clone()));
                }
                Some(false) => {}
                Some(true) => {
                    let Some(source) = incoming.source else { continue };
                    while incoming.asked_to < file.bytes && incoming.asked_to - incoming.expect < incoming.window {
                        let room = incoming.window - (incoming.asked_to - incoming.expect);
                        let len = room.min(MAX_WANT).min(file.bytes - incoming.asked_to);
                        let want = Message::Want { file: file.clone(), at: incoming.asked_to, len };
                        out.push(Tended::Say(source, want));
                        incoming.asked_to += len;
                        incoming.heard = now;
                    }
                }
            }
        }
    }

    /// What the look that is over found lacking is all that is wanted:
    /// a file the song no longer names is let go of. `lacked` is what the
    /// song says it plays and has no file for here.
    fn want_only(&mut self, lacked: &HashSet<Content>) {
        let gone: Vec<Content> = self.incoming.keys().filter(|file| !lacked.contains(*file)).cloned().collect();
        for file in gone {
            if let Some(incoming) = self.incoming.remove(&file) {
                if let Some(source) = incoming.source {
                    self.forgiven.insert((source, file.clone()), Instant::now() + FORGIVEN);
                }
                let _ = self.jobs.send(Job::LetGo(file));
            }
        }
    }

    /// The files that are on their way, by their names. `name` says who a
    /// connection is.
    pub fn on_their_way(&self, name: &dyn Fn(LinkId) -> String) -> Vec<OnItsWay> {
        let mut files: Vec<OnItsWay> = (self.incoming.values())
            .map(|incoming| OnItsWay {
                name: incoming.is.name.clone(),
                bytes: incoming.is.bytes,
                here: incoming.here,
                from: incoming.source.map(name),
            })
            .collect();
        files.sort_by(|a, b| (&a.name, a.bytes).cmp(&(&b.name, b.bytes)));
        files
    }
}
