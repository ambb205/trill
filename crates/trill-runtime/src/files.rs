//! The files of a shared song, between the programs that hold it.
//!
//! The song says what each file it plays *is* (`trill_core::files::FileIs`)
//! and every machine has its own place for it; no path is said or taken
//! (`trill_engine::session::content`). This is what gets the bytes from
//! whoever has them to whoever has not:
//!
//! * **This program says what its own files are.** A thread of its own
//!   reads each file the song names that is on this disk, once, and again
//!   only when the file was written since ([`read`]); what it found is
//!   written into the song as a property of the clip or the device.
//!   **Only audio is ever read to be said or handed out**: a file is one
//!   of the song's to anyone else only if Trill reads it as audio, by the
//!   reading that plays it and never by the ending of its name. What is no
//!   audio is what it always was here, a clip that does not play, and
//!   nobody hears of it or of its hash. And **only what may go**: a file a
//!   song file names is a stranger's until the user, who was shown it, let
//!   it go (`Session::let_files_go`, [`Runtime::files_to_go`]).
//! * **It hands out what its open song plays and it has**, to whoever is
//!   in the song and asks by content: [`trill_together::files::Offered`],
//!   a list made here from the song and the disk. Nothing a program says
//!   adds to it. In a song that is what the song brought here (kept in a
//!   folder of that song's own) and its user's own files in that song.
//! * **It asks for what the song plays and it has not got**, from whoever
//!   said they have it, a stretch at a time, and takes in only the bytes
//!   it asked for ([`trill_together::files::Received`]). What a file may
//!   cost is settled before its first byte: one that is longer than any
//!   file is refused, one the disk has not the room for waits, and more
//!   than the user said may arrive unasked waits for their word
//!   ([`Runtime::let_files_arrive`]).
//!
//! None of it is done on the thread that connects the session to the wire
//! ([`crate::together`]), which only passes things on: disks are read and
//! written by the two threads here, and what is said of the song goes
//! ahead of a file's bytes on every connection (`trill_together::net`).

use crate::Runtime;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, SystemTime};
use trill_core::files::{Content, FileIs, FilePlace};
use trill_engine::FileHere;
use trill_media::MediaCache;
use trill_together::files::{Begun, Bounds, Offered, Received, Shelved, Took, Waits, content_of};
use trill_together::{LinkId, MAX_PIECE, MAX_WANT, Message, Pieces};

/// The folder files from other programs are kept in, in the program's own
/// folder: one folder a shared song in it.
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
/// How often a file that waits for room on the disk is tried again.
const ROOM_AGAIN: Duration = Duration::from_secs(20);
/// How long a program that said it has not got a file is not asked for
/// it again: it may have it a moment later (a file that came beside its
/// song has another name for a moment).
const NOT_NOW: Duration = Duration::from_secs(20);
/// How long a program that sent other bytes than a file's is not asked
/// for that file again.
const NOT_THAT: Duration = Duration::from_secs(600);

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
    /// Why it is not arriving at the moment, where it is not.
    pub waits: Option<Waiting>,
}

/// Why a file that is wanted is not arriving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Waiting {
    /// More would be on its way at once than may be without the user's
    /// word ([`Runtime::let_files_arrive`]).
    ForAWord,
    /// The disk has not the room: how much is missing.
    ForRoom { short: u64 },
    /// The way to whoever has it passes no more files for now: for how
    /// many seconds, as far as was said.
    ForTheWay { seconds: u64 },
    /// Nobody who is in the song has said they have it.
    ForSomeone,
}

/// What a song would hand to whoever joins it: the files it names on this
/// machine. For showing before the song is shared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Going {
    /// How many files, and how many bytes they are together.
    pub files: usize,
    pub bytes: u64,
    /// The folders they lie in, each with how many files and bytes: the
    /// song's own folder, Trill's recordings and the folders of the
    /// sample library.
    pub folders: Vec<(String, usize, u64)>,
    /// The files that lie anywhere else, each by its path.
    pub elsewhere: Vec<String>,
    /// The files the song names that are not there.
    pub gone: usize,
}

impl Going {
    /// In a sentence or two, for a tool's reply and a front end's panel.
    pub fn sentence(&self) -> String {
        if self.files == 0 {
            return "The song names no audio files on this computer: nothing but the song itself goes to whoever \
                    joins."
                .to_owned();
        }
        let files = |count: usize| if count == 1 { "1 file".to_owned() } else { format!("{count} files") };
        let size = trill_fetch::whole::said_closely;
        let mut said = format!(
            "{} ({}) that the song plays on this computer {} to whoever joins, where Trill reads them as audio",
            files(self.files),
            size(self.bytes),
            if self.files == 1 { "goes" } else { "go" },
        );
        let folders: Vec<String> = self
            .folders
            .iter()
            .map(|(folder, count, bytes)| format!("{} in {folder} ({})", files(*count), size(*bytes)))
            .collect();
        if !folders.is_empty() {
            said.push_str(&format!(": {}", folders.join("; ")));
        }
        said.push('.');
        if !self.elsewhere.is_empty() {
            said.push_str(&format!(
                " Outside the song's folder, Trill's recordings and the sample library: {}.",
                self.elsewhere.join("; ")
            ));
        }
        said
    }
}

// ---- reading what one's own files are ---------------------------------------

/// What the thread that reads is asked: the files the song names, to be
/// looked at on this disk.
struct Look {
    files: Vec<FileHere>,
    /// The folder the song's files from other programs are kept in.
    received: Option<PathBuf>,
}

/// What looking at the song's files on this disk came to. A look answers
/// at once with what it knows, and again after each file it had to read.
#[derive(Default)]
struct Looked {
    /// Places whose file is on this disk and is not what the song says it
    /// is, or of which the song does not say: with where it is and what
    /// it was found to be.
    say: Vec<(FilePlace, String, FileIs)>,
    /// Files of the song this machine has, to hand out.
    have: Vec<(Content, Shelved)>,
    /// Files the song says it plays that are not here: only ones whose
    /// place is in the folder of files from other programs.
    lack: Vec<FileIs>,
    /// Whether this is the last word of the look.
    all: bool,
}

/// What this machine has read of a file: what it was (nothing, for a
/// file that is no audio or could not be read), and when it had last been
/// written then.
struct Read {
    bytes: u64,
    written: Option<SystemTime>,
    content: Option<Content>,
}

/// The thread that reads files to say what they are. One look at a time;
/// a file is read once, and again only if it was written since.
fn read(looks: Receiver<Look>, looked: Sender<Looked>, media: Arc<MediaCache>) {
    let mut index: HashMap<String, Read> = HashMap::new();
    while let Ok(mut look) = looks.recv() {
        // (Of several looks that waited, the last is the song as it is.)
        while let Ok(later) = looks.try_recv() {
            look = later;
        }
        let mut by_path: BTreeMap<String, Vec<&FileHere>> = BTreeMap::new();
        for file in &look.files {
            by_path.entry(file.path.clone()).or_default().push(file);
        }
        let mut first = Looked::default();
        let mut unread: Vec<(String, u64, Option<SystemTime>)> = Vec::new();
        let found_to_be = |content: &Content, written, path: &str, places: &[&FileHere], into: &mut Looked| {
            for file in places.iter().filter(|file| file.is.as_ref().map(FileIs::content).as_ref() != Some(content)) {
                into.say.push((file.place.clone(), path.to_owned(), FileIs::new(content.clone(), path)));
            }
            into.have.push((content.clone(), Shelved { path: PathBuf::from(path), written }));
        };
        for (path, places) in &by_path {
            let found = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file);
            // A file that came from another program lies under its hash:
            // it was held against that when it arrived, and is never
            // written again.
            let kept = (places.iter())
                .find_map(|file| file.is.as_ref().filter(|is| arrived(look.received.as_deref(), path, is)));
            match (found, kept) {
                (Some(found), Some(is)) if found.len() == is.bytes => {
                    first.have.push((is.content(), Shelved { path: PathBuf::from(path), written: None }));
                }
                // (Under its name and not its length: not it.)
                (Some(_), Some(is)) => first.lack.push(is.clone()),
                (None, Some(is)) => first.lack.push(is.clone()),
                // A file of this machine's own. One that may not go is not
                // read, whatever it is: nobody was shown it.
                (Some(_), None) if places.iter().any(|file| !file.may_go) => {}
                (Some(found), None) => {
                    let written = found.modified().ok();
                    match index.get(path) {
                        Some(read) if read.bytes == found.len() && read.written == written => {
                            if let Some(content) = &read.content {
                                found_to_be(content, written, path, places, &mut first);
                            }
                        }
                        _ => unread.push((path.to_string(), found.len(), written)),
                    }
                }
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
            // Only audio is read to be said: what Trill does not read as
            // audio, by the reading that plays it, is nobody else's. Its
            // name's ending says nothing.
            let read = match media.audio(Path::new(&path)) {
                Err(_) => Some(None),
                Ok(_) => match content_of(Path::new(&path), |_| true) {
                    Ok(Some(content)) if content.bytes == bytes => Some(Some(content)),
                    // Written while it was read: nothing is remembered of
                    // it, and it is read again at the next look.
                    Ok(_) => None,
                    Err(error) => {
                        log::warn!("a file of the song could not be read to say what it is: {error}");
                        Some(None)
                    }
                },
            };
            if let Some(content) = read {
                if let Some(content) = &content {
                    found_to_be(content, written, &path, &by_path[&path], &mut next);
                }
                index.insert(path, Read { bytes, written, content });
            }
            if looked.send(next).is_err() {
                return;
            }
        }
    }
}

/// Whether `path` is the place a file that is `is` has in `received`, the
/// folder of the song's files from other programs.
fn arrived(received: Option<&Path>, path: &str, is: &FileIs) -> bool {
    received.is_some_and(|received| Path::new(path) == received.join(is.kept_as()))
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
    /// The folder what arrives is kept in from here on: the open song's.
    Into(PathBuf),
    /// A file is wanted: how much of it is here?
    Begin(FileIs),
    /// Bytes that were asked for arrived.
    Piece { file: Content, at: u64, bytes: Vec<u8> },
    /// A file is no longer wanted.
    LetGo(Content),
    /// The user's word: however much may be on its way at once.
    AnyAmount,
    /// The look is over: these are the files the song names.
    Named(HashSet<Content>),
}

/// What it says back.
enum Done {
    /// How much of a file that was begun is here, that it is here whole,
    /// or why it waits.
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

/// How the room on a disk is asked.
pub(crate) type Room = fn(&Path) -> Option<u64>;

/// The thread that reads files for whoever asked and writes what arrives.
fn pass(jobs: Receiver<Job>, done: Sender<Done>, pieces: Pieces, bounds: Bounds, room: Room) {
    let mut offered = Offered::default();
    let mut received: Option<Received> = None;
    let mut any_amount = false;
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
            Ok(Job::Into(dir)) => {
                if received.as_ref().is_none_or(|received| received.dir() != dir) {
                    let mut into = Received::new(dir).bounded(bounds.clone(), Some(Box::new(room)));
                    if any_amount {
                        into.allow_any_amount();
                    }
                    received = Some(into);
                }
            }
            Ok(Job::Begin(is)) => {
                let begun = match received.as_mut() {
                    Some(received) => received.begin(&is).map_err(|error| error.to_string()),
                    None => Err("there is no shared song to keep it for".to_owned()),
                };
                let _ = done.send(Done::Begun { file: is.content(), begun });
            }
            Ok(Job::Piece { file, at, bytes }) => {
                let took = received.as_mut().map_or(Took::NotAsked, |received| received.take(&file, at, &bytes));
                let _ = done.send(Done::Took { file, took });
            }
            Ok(Job::LetGo(file)) => {
                if let Some(received) = received.as_mut() {
                    received.let_go(&file);
                }
            }
            Ok(Job::AnyAmount) => {
                any_amount = true;
                if let Some(received) = received.as_mut() {
                    received.allow_any_amount();
                }
            }
            Ok(Job::Named(named)) => {
                // A part of a file the song no longer names is never
                // gone on with: it goes.
                if let Some(received) = received.as_mut() {
                    let gone = received.sweep(&named);
                    if gone > 0 {
                        log::info!("{gone} part(s) of files the shared song no longer names were removed");
                    }
                }
            }
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

/// How far taking a file in has got.
#[derive(Clone, Debug, PartialEq)]
enum Stage {
    /// Not asked yet how much of it is here.
    New,
    /// Asked, and not answered yet.
    Asked,
    /// It waits, and is asked again at the time (never, for one that
    /// waits for the user's word).
    Waits(Waits, Option<Instant>),
    /// Its bytes are being asked for.
    Arriving,
}

/// A file that is wanted and on its way.
struct Incoming {
    is: FileIs,
    /// Who is being asked for it.
    source: Option<LinkId>,
    stage: Stage,
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
    /// The way to the program that said it passes no files for this many
    /// seconds, which was news.
    Later(u64),
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
    /// What a file of the song was found to be, to be written into it.
    SayFile(FilePlace, String, FileIs),
    /// Where this machine was found to have which bytes.
    Read(HashMap<Content, String>),
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
    /// The folder what arrives is kept in, as the thread that takes in
    /// was last told.
    into: Option<PathBuf>,
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
    /// Programs that are not asked for a file for a while, and until
    /// when: what they sent as it was not it, or they said they have not
    /// got it.
    not_from: HashMap<(LinkId, Content), Instant>,
    /// Bytes that may still arrive from a program that is no longer asked.
    forgiven: HashMap<(LinkId, Content), Instant>,
    /// Files this machine could not keep: not asked for again this run.
    failed: HashSet<Content>,
    /// The ways that pass no files for now, and until when: a way through
    /// a meeting point whose keeper's allowance for the day is nearly
    /// spent. On such a way nothing is asked for and nothing handed out.
    closed: HashMap<LinkId, Instant>,
    /// Whether the user was told that files wait for their word.
    asked_for_a_word: bool,
    /// How many bytes of files arrived, and what counts the ones that
    /// were said.
    got: u64,
    pieces: Pieces,
}

/// How often the song's files are looked at while nothing changes.
const LOOK_EVERY: Duration = Duration::from_millis(1500);
/// How soon after a change they are looked at again at the earliest.
const LOOK_AFTER: Duration = Duration::from_millis(200);
/// How many files a program's saying of what it has may come to.
const MAX_THEIRS: usize = 65_536;

impl Files {
    /// Starts the two threads. `media` is what reads a file as audio;
    /// `bounds` and `room` are what taking a file in may cost.
    pub fn new(pieces: Pieces, media: Arc<MediaCache>, bounds: Bounds, room: Room) -> std::io::Result<Self> {
        let (looks, to_look) = channel();
        let (say_looked, looked) = channel();
        std::thread::Builder::new().name("trill-files-read".into()).spawn(move || read(to_look, say_looked, media))?;
        let (jobs, to_do) = channel();
        let (say_done, done) = channel();
        let saying = pieces.clone();
        std::thread::Builder::new()
            .name("trill-files-pass".into())
            .spawn(move || pass(to_do, say_done, saying, bounds, room))?;
        Ok(Self {
            looks,
            looked,
            jobs,
            done,
            looking: false,
            asked: None,
            into: None,
            have: HashMap::new(),
            finding: HashMap::new(),
            lacking: HashSet::new(),
            told: HashMap::new(),
            theirs: HashMap::new(),
            incoming: HashMap::new(),
            not_from: HashMap::new(),
            forgiven: HashMap::new(),
            failed: HashSet::new(),
            closed: HashMap::new(),
            asked_for_a_word: false,
            got: 0,
            pieces,
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
    /// here: what comes of it is taken up by [`Files::tend`]. `received`
    /// is the folder the song's files from other programs are kept in.
    pub fn look(&mut self, revision: u64, files: Vec<FileHere>, received: Option<PathBuf>) {
        (self.looking, self.asked) = (true, Some((revision, Instant::now())));
        // What is handed out stays what it is while the look goes on:
        // what it finds is added as it is found, and what it no longer
        // finds goes when it is over.
        self.finding.clear();
        self.lacking.clear();
        if received != self.into
            && let Some(dir) = &received
        {
            let _ = self.jobs.send(Job::Into(dir.clone()));
            self.into = received.clone();
        }
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
        self.closed.clear();
        self.asked = None;
        self.asked_for_a_word = false;
        let _ = self.jobs.send(Job::Offer(Offered::default()));
    }

    /// A connection is gone.
    pub fn lost(&mut self, link: LinkId) {
        self.told.remove(&link);
        self.theirs.remove(&link);
        self.closed.remove(&link);
        self.not_from.retain(|(from, _), _| *from != link);
        self.forgiven.retain(|(from, _), _| *from != link);
        let _ = self.jobs.send(Job::Gone(link));
        for incoming in self.incoming.values_mut().filter(|incoming| incoming.source == Some(link)) {
            // It goes on from where it is, from whoever else has it.
            (incoming.source, incoming.stage) = (None, Stage::New);
        }
    }

    /// The user's word: however much may be on its way at once.
    pub fn any_amount(&mut self) {
        let _ = self.jobs.send(Job::AnyAmount);
        for incoming in self.incoming.values_mut() {
            if matches!(incoming.stage, Stage::Waits(Waits::ForAWord { .. }, _)) {
                incoming.stage = Stage::New;
            }
        }
    }

    /// A way passes no files for `seconds` from now (or does again, with
    /// 0): nothing is asked for on it and nothing handed out, and what is
    /// said of the song goes on. Returns whether that is news: it was
    /// open and is closed, or the other way round.
    pub fn close_way(&mut self, link: LinkId, seconds: u64) -> bool {
        let was = self.way_is_closed(link).is_some();
        match seconds {
            0 => drop(self.closed.remove(&link)),
            seconds => drop(self.closed.insert(link, Instant::now() + Duration::from_secs(seconds))),
        }
        was != (seconds > 0)
    }

    /// How many bytes of files have passed between this program and the
    /// others, both ways: for whoever counts what a way has passed.
    pub fn passed(&self) -> u64 {
        self.got + self.pieces.said()
    }

    fn way_is_closed(&self, link: LinkId) -> Option<u64> {
        let until = self.closed.get(&link)?;
        Some(until.saturating_duration_since(Instant::now()).as_secs()).filter(|left| *left > 0)
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
                // On a way that passes no files for now, later.
                if let Some(seconds) = self.way_is_closed(link) {
                    return Heard::Answer(Message::Later { seconds });
                }
                let _ = self.jobs.send(Job::Serve { link, file, at, len });
                Heard::Fine
            }
            Message::Nothing { file } => {
                if let Some(incoming) = self.incoming.get_mut(&file).filter(|incoming| incoming.source == Some(link)) {
                    self.not_from.insert((link, file), Instant::now() + NOT_NOW);
                    (incoming.source, incoming.stage) = (None, Stage::New);
                }
                Heard::Fine
            }
            Message::Later { seconds } => {
                // Whoever is at the other end says the way between passes
                // no files for now: what was asked of them is asked again
                // then, and they are asked for nothing meanwhile.
                let seconds = seconds.clamp(1, 86_400);
                let news = self.close_way(link, seconds);
                let now = Instant::now();
                for (file, incoming) in &mut self.incoming {
                    if incoming.source == Some(link) {
                        self.forgiven.insert((link, file.clone()), now + FORGIVEN);
                        (incoming.source, incoming.stage) = (None, Stage::New);
                    }
                }
                if news { Heard::Later(seconds) } else { Heard::Fine }
            }
            Message::Piece { file, at, bytes } => {
                let len = bytes.len() as u64;
                self.got += len;
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
            for (place, path, is) in looked.say {
                out.push(Tended::SayFile(place, path, is));
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
                        stage: Stage::New,
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
                // Where this machine has which bytes of its own, for a
                // clip that arrives and is one of them.
                let own = found.iter().filter(|(_, shelved)| shelved.written.is_some());
                let read = own.map(|(file, shelved)| (file.clone(), shelved.path.to_string_lossy().into_owned()));
                out.push(Tended::Read(read.collect()));
                let lacking = std::mem::take(&mut self.lacking);
                self.want_only(&lacking);
                // And the parts of what the song no longer names go.
                let named: HashSet<Content> = lacking.iter().chain(self.have.keys()).cloned().collect();
                let _ = self.jobs.send(Job::Named(named));
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
        self.not_from.retain(|_, until| *until > now);
        self.closed.retain(|_, until| *until > now);
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
                (incoming.stage, incoming.heard) = (Stage::Arriving, Instant::now());
                (incoming.since, incoming.arrived) = (Instant::now(), 0);
            }
            Ok(Begun::Waits(waits)) => {
                // Room may come by itself; the user's word does not.
                let again = matches!(waits, Waits::ForRoom { .. }).then(|| Instant::now() + ROOM_AGAIN);
                let word = matches!(waits, Waits::ForAWord { .. });
                incoming.stage = Stage::Waits(waits, again);
                if word && !self.asked_for_a_word {
                    self.asked_for_a_word = true;
                    let waiting = self.incoming.values().filter(|file| matches!(file.stage, Stage::Waits(..)));
                    let (count, bytes) =
                        waiting.fold((0, 0), |(count, bytes), file| (count + 1, bytes + file.is.bytes));
                    out.push(Tended::Tell(format!(
                        "More of the shared song's audio is on its way than arrives without your word: {count} more \
                         file(s) of {} wait. Say that they may arrive where the song is shared (File, Share this \
                         song); the song goes on meanwhile.",
                        trill_fetch::whole::said_closely(bytes)
                    )));
                }
            }
            Err(why) => {
                let named = incoming.is.name.clone();
                self.incoming.remove(&file);
                self.failed.insert(file);
                out.push(Tended::Tell(format!(
                    "'{named}', a file of the shared song, is not taken in on this machine: {why}. The song goes on \
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
                    self.not_from.insert((from, file.clone()), Instant::now() + NOT_THAT);
                    self.forgiven.insert((from, file), Instant::now() + FORGIVEN);
                }
                (incoming.source, incoming.stage, incoming.here) = (None, Stage::New, 0);
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
                && incoming.stage == Stage::Arriving
                && incoming.asked_to > incoming.expect
                && now.duration_since(incoming.heard) > STALLED
            {
                self.not_from.insert((source, file.clone()), now + NOT_NOW);
                self.forgiven.insert((source, file.clone()), now + FORGIVEN);
                (incoming.source, incoming.stage) = (None, Stage::New);
            }
            if let Stage::Waits(_, again) = &incoming.stage {
                match again {
                    Some(again) if now >= *again => incoming.stage = Stage::New,
                    _ => continue,
                }
            }
            if incoming.source.is_none() {
                let has = |link: &&LinkId| {
                    self.theirs.get(*link).is_some_and(|theirs| theirs.contains(file))
                        && !self.not_from.contains_key(&(**link, file.clone()))
                        && !self.closed.contains_key(*link)
                };
                let Some(source) = synced.iter().find(has) else { continue };
                incoming.source = Some(*source);
            }
            match incoming.stage {
                Stage::New => {
                    // How much of it is here is asked first: of a file
                    // that was half here, the rest is asked for.
                    incoming.stage = Stage::Asked;
                    let _ = self.jobs.send(Job::Begin(incoming.is.clone()));
                }
                Stage::Asked | Stage::Waits(..) => {}
                Stage::Arriving => {
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
        if !self.incoming.values().any(|file| matches!(file.stage, Stage::Waits(Waits::ForAWord { .. }, _))) {
            self.asked_for_a_word = false;
        }
    }

    /// The files that are on their way, by their names. `name` says who a
    /// connection is.
    pub fn on_their_way(&self, name: &dyn Fn(LinkId) -> String) -> Vec<OnItsWay> {
        let mut files: Vec<OnItsWay> = (self.incoming.iter())
            .map(|(file, incoming)| {
                let waits = match &incoming.stage {
                    Stage::Waits(Waits::ForAWord { .. }, _) => Some(Waiting::ForAWord),
                    Stage::Waits(Waits::ForRoom { needs, free }, _) => {
                        Some(Waiting::ForRoom { short: needs.saturating_sub(*free) })
                    }
                    _ if incoming.source.is_none() => {
                        // Whoever has it, and whether the way to them is closed.
                        let have = |link: &LinkId| self.theirs.get(link).is_some_and(|theirs| theirs.contains(file));
                        let closed =
                            self.closed.keys().filter(|link| have(link)).filter_map(|link| self.way_is_closed(*link));
                        match closed.min() {
                            Some(seconds) => Some(Waiting::ForTheWay { seconds }),
                            None => Some(Waiting::ForSomeone),
                        }
                    }
                    _ => None,
                };
                OnItsWay {
                    name: incoming.is.name.clone(),
                    bytes: incoming.is.bytes,
                    here: incoming.here,
                    from: incoming.source.map(name),
                    waits,
                }
            })
            .collect();
        files.sort_by(|a, b| (&a.name, a.bytes).cmp(&(&b.name, b.bytes)));
        files
    }
}

// ---- what a front end asks --------------------------------------------------

impl Runtime {
    /// What the open song would hand to whoever joins it: the files it
    /// names on this computer, how large they are and where they lie. For
    /// showing before the song is shared. It looks at the disk for each
    /// file (that it is there, how large it is) and reads none: call it
    /// away from the thread that draws.
    pub fn files_to_go(&self) -> Going {
        let (files, song, recordings) = {
            let session = self.session.lock();
            let received = session.received_in();
            let own =
                |file: &FileHere| received.as_ref().is_none_or(|folder| !Path::new(&file.path).starts_with(folder));
            let files: Vec<String> = session.files_here().into_iter().filter(own).map(|file| file.path).collect();
            let song = session.path().and_then(|path| path.parent().map(Path::to_owned));
            (files, song, session.recordings_folders())
        };
        let files: std::collections::BTreeSet<String> = files.into_iter().collect();
        let library: Vec<PathBuf> = self.library.roots();
        let mut going = Going::default();
        let mut folders: BTreeMap<String, (usize, u64)> = BTreeMap::new();
        for path in files {
            let Some(found) = std::fs::metadata(&path).ok().filter(std::fs::Metadata::is_file) else {
                going.gone += 1;
                continue;
            };
            going.files += 1;
            going.bytes += found.len();
            let within = |folder: &Path| Path::new(&path).starts_with(folder);
            let known = song.as_deref().filter(|folder| within(folder)).map(|_| "the song's folder".to_owned());
            let known = known
                .or_else(|| recordings.iter().any(|folder| within(folder)).then(|| "Trill's recordings".to_owned()));
            let known = known.or_else(|| {
                let root = library.iter().find(|root| within(root))?;
                Some(format!("the sample library ({})", root.display()))
            });
            match known {
                Some(folder) => {
                    let entry = folders.entry(folder).or_default();
                    (entry.0, entry.1) = (entry.0 + 1, entry.1 + found.len());
                }
                None => going.elsewhere.push(path),
            }
        }
        going.folders = folders.into_iter().map(|(folder, (count, bytes))| (folder, count, bytes)).collect();
        going
    }

    /// Lets the files the open song names on this computer go to whoever
    /// else has the song: for the user who was shown them
    /// ([`Runtime::files_to_go`], [`Together::held`](crate::Together)).
    pub fn let_files_go(&self) {
        self.session.lock().let_files_go();
    }
}
