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
use trill_together::files::{BeforeNamed, Begun, Bounds, Offered, Received, Shelved, Took, Waits, content_of};
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
/// How many bytes are asked for ahead of one program when files begin to
/// arrive from it, the fewest that ever are, and the most.
const FIRST_WINDOW: u64 = 3 * MAX_PIECE as u64;
const LEAST_WINDOW: u64 = 16 << 10;
const MOST_WINDOW: u64 = 16 << 20;
/// The fewest bytes that are asked for at once, of a file that has more.
const LEAST_STRIDE: u64 = 8 << 10;
/// How long what waits to be sent of files, at the program that hands
/// them out, should take to arrive. Whatever that program has to say of
/// the song goes ahead of the files in its own queue, and behind what its
/// system and the line have taken already, which is as much as it was
/// asked for: so what it says of the song waits behind no more than this
/// of files, **however many files it is handing over**. What is asked
/// for ahead is the connection's, and the files that come over it share
/// it, in turn. (It was each file's once. With twelve recordings arriving
/// at once over a line of one megabit a second, a fader the other moved
/// then took two seconds to arrive, where it takes half a second with one
/// recording.)
///
/// What is asked for ahead is this much *and what is on its way*: a
/// question takes time to get there and its bytes time to get back
/// ([`Way::turn`]), and a program that asks only for what arrives in a
/// moment leaves the line with nothing on it while it asks. (Over a way
/// that takes a quarter of a second there and back, twelve megabytes
/// took 45 seconds on a line that carries them in 19.)
const WAITING: Duration = Duration::from_millis(125);
/// Over how long what arrives is counted, to say how fast the way is.
const COUNTED: Duration = Duration::from_millis(250);
/// The longest a question and its answer are taken to be on their way.
const LONGEST_TURN: Duration = Duration::from_secs(2);
/// How many questions for bytes are put to one program in a second at the
/// most: a song of thousands of small files, on a fast connection, would
/// otherwise ask more often than a program that works in a song says
/// anything, and be let go of for it.
const WANTS_A_SECOND: u32 = 200;
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
    /// The name its maker has it under. For one that nobody has said
    /// ([`Waiting::ToBeSaid`]), the name of the clip that plays it, or
    /// the slot and the device: nothing else is known of it, and its
    /// `bytes` are 0.
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
    /// Nobody has said what it is: it is with whoever brought it into
    /// the song, whose program is still reading it, has not got it
    /// either, does not read it as audio, or was not let hand it out
    /// (`Session::files_unsaid`). Nothing can be asked for until it is.
    ToBeSaid,
}

/// What a place whose file nobody has said is shown by: the clip's name,
/// or the slot and the device.
pub(crate) fn unsaid_name(file: &trill_core::files::FileUse) -> String {
    match &file.place {
        FilePlace::Sample { slot, .. } => format!("{slot} of {}", file.name),
        FilePlace::Clip(_) | FilePlace::Rendition(_) if file.name.is_empty() => "A clip".to_owned(),
        FilePlace::Clip(_) | FilePlace::Rendition(_) => file.name.clone(),
    }
}

/// What a song would hand to whoever joins it: the audio files it names
/// on this machine. For showing before the song is shared.
///
/// Only what Trill reads as audio goes, and only that is counted: each
/// file was read as the song plays it, not judged by its name. What the
/// song names that is no audio is listed apart, by its path, because it
/// stays here and because a song file someone else wrote may name
/// anything: its user should see what it named.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Going {
    /// How many audio files, and how many bytes they are together.
    pub files: usize,
    pub bytes: u64,
    /// The folders they lie in, each with how many files and bytes: the
    /// song's own folder, Trill's recordings and the folders of the
    /// sample library.
    pub folders: Vec<(String, usize, u64)>,
    /// The audio files that lie anywhere else, each by its path.
    pub elsewhere: Vec<String>,
    /// The files the song names that are there and that Trill does not
    /// read as audio, each by its path: they go to nobody.
    pub no_audio: Vec<String>,
    /// The files the song names that are not there.
    pub gone: usize,
}

impl Going {
    /// In a sentence or two, for a tool's reply and a front end's panel.
    pub fn sentence(&self) -> String {
        let files = |count: usize| if count == 1 { "1 file".to_owned() } else { format!("{count} files") };
        // What stays, said after what goes.
        let stays = match self.no_audio.len() {
            0 => String::new(),
            count => format!(
                " {} the song names {} no audio as Trill reads {} and {} here: {}.",
                files(count),
                if count == 1 { "is" } else { "are" },
                if count == 1 { "it" } else { "them" },
                if count == 1 { "stays" } else { "stay" },
                self.no_audio.join("; ")
            ),
        };
        if self.files == 0 {
            return format!(
                "The song names no audio files on this computer: nothing but the song itself goes to whoever \
                 joins.{stays}"
            );
        }
        let size = trill_fetch::whole::said_closely;
        let mut said = format!(
            "{} ({}) that the song plays on this computer {} to whoever joins",
            if self.files == 1 { "1 audio file".to_owned() } else { format!("{} audio files", self.files) },
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
        said.push_str(&stays);
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
                // (Under its name and not its length: not it. And not
                // here: wanted, unless it is a recording that stands in
                // for devices this machine has.)
                (_, Some(is)) if places.iter().any(|file| file.wanted) => first.lack.push(is.clone()),
                (_, Some(_)) => {}
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
    /// How many bytes of files may still be handed to a connection, where
    /// the way to it passes only so many more: none are handed over above
    /// that. `None` for as many as are asked for.
    Budget(LinkId, Option<u64>),
    /// The folder what arrives is kept in from here on: the open song's.
    Into(PathBuf),
    /// A file is wanted: how much of it is here?
    Begin(FileIs),
    /// Bytes that were asked for arrived.
    Piece { file: Content, at: u64, bytes: Vec<u8> },
    /// A file is no longer wanted.
    LetGo(Content),
    /// What arrived of a file is not to be gone on from: whoever sent it
    /// sent what nobody asked for.
    ThrowAway(Content),
    /// The user's word: however much may be on its way at once.
    AnyAmount,
    /// The look is over: these are the files the song names.
    Named(HashSet<Content>),
}

/// What it says back.
enum Done {
    /// How much of a file that was begun is here, that it is here whole
    /// (with what was read of it to play it), or why it waits.
    Begun {
        file: Content,
        begun: Result<Begun, String>,
        read: Option<Played>,
    },
    Took {
        file: Content,
        took: Took,
        read: Option<Played>,
    },
    /// A program asked for more than it took.
    TooMuch(LinkId),
    /// A file a program asked for is not here after all (it went, or was
    /// written, since it was offered): it is told so.
    NotHere(LinkId, Content),
}

/// A file as it was read to be played: held until the song has it.
pub(crate) type Played = Arc<trill_core::audio::AudioBuffer>;

/// A stretch of a file that someone asked for and that is not read yet.
struct Owed {
    file: Content,
    at: u64,
    left: u64,
}

/// How the room on a disk is asked.
pub(crate) type Room = fn(&Path) -> Option<u64>;

/// The thread that reads files for whoever asked and writes what arrives.
fn pass(jobs: Receiver<Job>, done: Sender<Done>, pieces: Pieces, bounds: Bounds, room: Room, media: Arc<MediaCache>) {
    // A file that has arrived is read as the song will play it here, on
    // this thread, *before it has its name*: the song names it by that
    // name, and whatever makes the song anew meanwhile (an edit that
    // arrives) would otherwise find the file and wait for it to be read,
    // with the song held. What was read is handed on with the word that
    // the file is here, and held until the song has taken it up: the
    // cache keeps what someone holds. (Reading a long recording takes
    // seconds, and an edit never waits for a file.)
    let just_read: Arc<parking_lot::Mutex<Option<Played>>> = Arc::default();
    let before_named = |media: Arc<MediaCache>, read: Arc<parking_lot::Mutex<Option<Played>>>| -> BeforeNamed {
        Box::new(move |part: &Path, home: &Path| {
            media.forget(home);
            *read.lock() = media.read_as(part, home, media.loader_rate()).ok();
        })
    };
    // (One that was whole when it was asked about has its name already.)
    let read_to_play = |path: &Path| -> Option<Played> {
        match media.loader_rate() {
            Some(rate) => media.audio_at_rate(path, rate).ok(),
            None => media.audio(path).ok(),
        }
    };
    let mut offered = Offered::default();
    let mut received: Option<Received> = None;
    let mut any_amount = false;
    let mut owed: HashMap<LinkId, Vec<Owed>> = HashMap::new();
    let mut budgets: HashMap<LinkId, u64> = HashMap::new();
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
            Ok(Job::Budget(link, Some(bytes))) => drop(budgets.insert(link, bytes)),
            Ok(Job::Budget(link, None)) => drop(budgets.remove(&link)),
            Ok(Job::Into(dir)) => {
                if received.as_ref().is_none_or(|received| received.dir() != dir) {
                    let mut into = Received::new(dir)
                        .bounded(bounds.clone(), Some(Box::new(room)))
                        .before_named(before_named(media.clone(), just_read.clone()));
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
                let read = match &begun {
                    Ok(Begun::Here(path)) => read_to_play(path),
                    _ => None,
                };
                let _ = done.send(Done::Begun { file: is.content(), begun, read });
            }
            Ok(Job::Piece { file, at, bytes }) => {
                let took = received.as_mut().map_or(Took::NotAsked, |received| received.take(&file, at, &bytes));
                let read = just_read.lock().take().filter(|_| matches!(took, Took::Whole(_)));
                let _ = done.send(Done::Took { file, took, read });
            }
            Ok(Job::LetGo(file)) => {
                if let Some(received) = received.as_mut() {
                    received.let_go(&file);
                }
            }
            Ok(Job::ThrowAway(file)) => {
                if let Some(received) = received.as_mut() {
                    received.throw_away(&file);
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
            // On a way that passes only so many more bytes, no piece is
            // handed over that would be more: what is owed waits, until
            // whoever counts the way says it passes more, or closes it.
            let next = first.left.min(MAX_PIECE as u64);
            if budgets.get(link).is_some_and(|left| *left < next) {
                return true;
            }
            read_any = true;
            match offered.read(&first.file, first.at, first.left) {
                Some(bytes) => {
                    let read = bytes.len() as u64;
                    let piece = Message::Piece { file: first.file.clone(), at: first.at, bytes };
                    (first.at, first.left) = (first.at + read, first.left - read);
                    if pieces.say(*link, &piece).is_none() {
                        return false;
                    }
                    if let Some(left) = budgets.get_mut(link) {
                        *left = left.saturating_sub(read);
                    }
                }
                // Not here, whatever the reason: the rest of what was
                // asked for of it is not answered piece by piece.
                None => {
                    first.left = 0;
                    let _ = done.send(Done::NotHere(*link, first.file.clone()));
                }
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
    /// When bytes last arrived, or were last asked for.
    heard: Instant,
}

/// What is asked for over one connection, of every file that comes over
/// it together.
struct Way {
    /// How many bytes a second arrived over it, the last few times that
    /// was counted. A way that is asked for less than it carries has less
    /// arrive than it could carry, and never more: so what it carries is
    /// the most of them. (Going by the last count alone, what is asked
    /// for swings: a count that came out low has less asked for, of which
    /// less arrives.)
    rates: [Option<f64>; 4],
    /// How long a question took to get there and the first of its answer
    /// to arrive, the last few times that was measured: from a question
    /// put while nothing was on its way, which asks for a little first
    /// ([`LEAST_STRIDE`]), so that what is measured is the way and not
    /// how long a whole piece takes to cross the line. Whatever else is
    /// in such a measurement makes it longer (a disk that was slow to
    /// read, a thread that was late), so the way is the shortest of them.
    turns: [Option<Duration>; 4],
    /// For the rate: when counting began, and how much arrived since.
    since: Instant,
    arrived: u64,
    /// When a question was put while nothing was on its way, until its
    /// first bytes arrive.
    dry: Option<Instant>,
    /// The second in which questions are counted, and how many were put
    /// in it.
    asked: (Instant, u32),
}

impl Way {
    fn new() -> Self {
        let now = Instant::now();
        Self { rates: [None; 4], turns: [None; 4], since: now, arrived: 0, dry: None, asked: (now, 0) }
    }

    /// How many bytes a second the way carries, as far as is known.
    fn rate(&self) -> Option<f64> {
        self.rates.iter().flatten().copied().reduce(f64::max)
    }

    /// How long a question and the first of its answer are on their way,
    /// as far as is known.
    fn turn(&self) -> Duration {
        self.turns.iter().flatten().min().copied().unwrap_or_default().min(LONGEST_TURN)
    }

    /// How many bytes may be asked for and not written here yet: what is
    /// on its way there and back, and what arrives in [`WAITING`].
    fn window(&self) -> u64 {
        let Some(rate) = self.rate() else { return FIRST_WINDOW };
        ((rate * (self.turn() + WAITING).as_secs_f64()) as u64).clamp(LEAST_WINDOW, MOST_WINDOW)
    }

    /// How far under the window what is on its way falls before more is
    /// asked for: half of what arrives in [`WAITING`]. A file is then a
    /// few questions a second, and not one for every piece that arrived
    /// (which is more, faster, than a program that works in a song says,
    /// and is let go of for it).
    fn stride(&self) -> u64 {
        let window = self.window();
        let Some(rate) = self.rate() else { return window / 2 };
        ((rate * WAITING.as_secs_f64() / 2.0) as u64).clamp(LEAST_STRIDE, window / 2)
    }

    /// Bytes arrived over it, in one piece.
    fn arrived(&mut self, bytes: u64) {
        let now = Instant::now();
        if let Some(asked) = self.dry.take() {
            // The first piece of a question put while nothing was on its
            // way: how long there and back takes.
            self.turns.rotate_right(1);
            self.turns[0] = Some(now.duration_since(asked).min(LONGEST_TURN));
            // What arrives is counted from here, after this piece: not
            // the time the question took to get there.
            (self.since, self.arrived) = (now, 0);
            return;
        }
        self.arrived += bytes;
        let counted = now.duration_since(self.since);
        // (The first time as soon as there is anything to go by: until
        // then little is asked for, whatever the way would take.)
        let enough = counted >= COUNTED || (self.rate().is_none() && self.arrived >= 2 * MAX_PIECE as u64);
        if enough && !counted.is_zero() {
            self.rates.rotate_right(1);
            self.rates[0] = Some(self.arrived as f64 / counted.as_secs_f64());
            (self.since, self.arrived) = (now, 0);
        }
    }

    /// Whether one more question may be put to it this second, which it
    /// then is.
    fn may_ask(&mut self, now: Instant) -> bool {
        if now.duration_since(self.asked.0) >= Duration::from_secs(1) {
            self.asked = (now, 0);
        }
        self.asked.1 += 1;
        self.asked.1 <= WANTS_A_SECOND
    }
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
    /// A file arrived: the song plays it from here on. With what was
    /// read of it, to be held until the song has taken it up.
    Here(PathBuf, Option<Played>),
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
    /// What each program that files are asked of is asked for at once.
    ways: HashMap<LinkId, Way>,
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
    /// How many bytes of files the thread that hands out was last told
    /// it may still hand each program.
    budgets: HashMap<LinkId, u64>,
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
        let reading = media.clone();
        std::thread::Builder::new()
            .name("trill-files-read".into())
            .spawn(move || read(to_look, say_looked, reading))?;
        let (jobs, to_do) = channel();
        let (say_done, done) = channel();
        let (saying, playing) = (pieces.clone(), media.clone());
        std::thread::Builder::new()
            .name("trill-files-pass".into())
            .spawn(move || pass(to_do, say_done, saying, bounds, room, playing))?;
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
            ways: HashMap::new(),
            incoming: HashMap::new(),
            not_from: HashMap::new(),
            forgiven: HashMap::new(),
            failed: HashSet::new(),
            closed: HashMap::new(),
            budgets: HashMap::new(),
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
        self.ways.remove(&link);
        self.closed.remove(&link);
        self.budgets.remove(&link);
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
        let news = was != (seconds > 0);
        if news && seconds > 0 {
            // What was asked for over it before is not handed over either.
            let _ = self.jobs.send(Job::Gone(link));
        }
        news
    }

    /// How many bytes of files may still be handed to a program, where
    /// the way to it passes only so many more today: the thread that
    /// hands out stops at that, whatever was asked for, between one look
    /// of whoever counts and the next. `None` for a way that passes what
    /// it is asked.
    pub fn may_hand(&mut self, link: LinkId, bytes: Option<u64>) {
        if self.budgets.get(&link).copied() != bytes {
            match bytes {
                Some(bytes) => drop(self.budgets.insert(link, bytes)),
                None => drop(self.budgets.remove(&link)),
            }
            let _ = self.jobs.send(Job::Budget(link, bytes));
        }
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
                    if self.forgiven.get(&(link, file)).is_some_and(|until| *until > Instant::now()) {
                        return Heard::Fine;
                    }
                    // Whoever sends what nobody asked for may have sent
                    // other bytes than the file's where they were asked
                    // for: nothing that came from there is gone on with.
                    // (A part with wrong bytes in it would be found out
                    // when it is whole, and whoever sent the rest of it
                    // in good faith would be taken for the one who did.)
                    for (file, incoming) in &mut self.incoming {
                        if incoming.source == Some(link) {
                            let _ = self.jobs.send(Job::ThrowAway(file.clone()));
                            (incoming.source, incoming.stage) = (None, Stage::New);
                            (incoming.here, incoming.expect, incoming.asked_to) = (0, 0, 0);
                        }
                    }
                    return Heard::LetGo("it sent bytes of a file that were not asked for");
                };
                incoming.expect += len;
                incoming.heard = Instant::now();
                // What is asked for ahead over this connection goes by
                // what arrives over it.
                self.ways.entry(link).or_insert_with(Way::new).arrived(len);
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
                    let incoming =
                        Incoming { is, source: None, stage: Stage::New, here: 0, expect: 0, asked_to: 0, heard: now };
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
                Done::Begun { file, begun, read } => self.begun(file, begun, read, &mut out),
                Done::Took { file, took, read } => self.took(file, took, read, name, &mut out),
                Done::TooMuch(link) => {
                    out.push(Tended::LetGo(link, "it asked for more of the song's files than it took"));
                }
                Done::NotHere(link, file) => out.push(Tended::Say(link, Message::Nothing { file })),
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
    fn begun(&mut self, file: Content, begun: Result<Begun, String>, read: Option<Played>, out: &mut Vec<Tended>) {
        let Some(incoming) = self.incoming.get_mut(&file) else { return };
        match begun {
            Ok(Begun::Here(path)) => {
                self.incoming.remove(&file);
                self.have.insert(file.clone(), Shelved { path: path.clone(), written: None });
                let _ = self.jobs.send(Job::Offer(Offered::new(self.have.clone())));
                out.push(Tended::Here(path, read));
            }
            Ok(Begun::From(here)) => {
                (incoming.here, incoming.expect, incoming.asked_to) = (here, here, here);
                (incoming.stage, incoming.heard) = (Stage::Arriving, Instant::now());
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
    fn took(
        &mut self,
        file: Content,
        took: Took,
        read: Option<Played>,
        name: &dyn Fn(LinkId) -> String,
        out: &mut Vec<Tended>,
    ) {
        let Some(incoming) = self.incoming.get_mut(&file) else { return };
        match took {
            Took::More(here) => incoming.here = here,
            Took::Whole(path) => {
                self.incoming.remove(&file);
                self.have.insert(file.clone(), Shelved { path: path.clone(), written: None });
                let _ = self.jobs.send(Job::Offer(Offered::new(self.have.clone())));
                out.push(Tended::Here(path, read));
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
    /// has it, and of each program as far ahead as arrives from it in a
    /// moment, of all the files it is asked for together.
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
                Stage::Asked | Stage::Waits(..) | Stage::Arriving => {}
            }
        }
        // The bytes, a connection at a time: the files that arrive over
        // one share what is asked for ahead over it, in turn.
        let mut from: BTreeMap<LinkId, Vec<&Content>> = BTreeMap::new();
        for (file, incoming) in &self.incoming {
            if let (Stage::Arriving, Some(source)) = (&incoming.stage, incoming.source) {
                from.entry(source).or_default().push(file);
            }
        }
        // Ahead of what is *written* here, not of what has arrived: where
        // the disk is slower than the way, nothing piles up in memory
        // waiting to be written.
        let ahead = |incoming: &Incoming| incoming.asked_to.saturating_sub(incoming.here);
        let mut wants = Vec::new();
        for (source, mut files) in from {
            let on_their_way: u64 = files.iter().map(|file| ahead(&self.incoming[*file])).sum();
            let way = self.ways.entry(source).or_insert_with(Way::new);
            // In strides: more is asked for once a stride of what was
            // asked for ahead is here.
            let window = way.window();
            if on_their_way > window - way.stride() {
                continue;
            }
            // The file that has least to go first, and each to its end
            // before the next: one after another is as fast over one
            // connection as all at once, and so the most files are whole
            // the soonest. A long recording does not keep the short ones
            // behind it waiting.
            files.sort_by_key(|file| {
                let incoming = &self.incoming[*file];
                (file.bytes.saturating_sub(incoming.asked_to), incoming.is.name.clone(), (*file).clone())
            });
            let mut room = window - on_their_way;
            let mut asked = false;
            // Nothing is on its way: the first question is for a little,
            // and how long that takes to get there and back is how long
            // the way is.
            let mut first = (on_their_way == 0).then_some(LEAST_STRIDE);
            'files: for file in files {
                let mut at = self.incoming[file].asked_to;
                while at < file.bytes {
                    if !way.may_ask(now) {
                        break 'files;
                    }
                    // (Not a few bytes at a time: what is left of the room
                    // is asked for with the next stride.)
                    let left = file.bytes - at;
                    if room < LEAST_STRIDE.min(left) {
                        break 'files;
                    }
                    let len = room.min(MAX_WANT).min(left).min(first.take().unwrap_or(u64::MAX));
                    wants.push((source, file.clone(), at, len));
                    (at, room, asked) = (at + len, room - len, true);
                }
            }
            if asked && on_their_way == 0 {
                way.dry = Some(now);
            }
        }
        for (source, file, at, len) in wants {
            if let Some(incoming) = self.incoming.get_mut(&file) {
                (incoming.asked_to, incoming.heard) = (at + len, now);
            }
            out.push(Tended::Say(source, Message::Want { file, at, len }));
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

/// A path as the file system spelled it out, as it is shown to someone:
/// without the mark Windows puts in front of such a path.
fn as_shown(real: &Path) -> String {
    let text = real.to_string_lossy();
    match (text.strip_prefix(r"\\?\UNC\"), text.strip_prefix(r"\\?\")) {
        (Some(rest), _) => format!(r"\\{rest}"),
        (None, Some(rest)) => rest.to_owned(),
        (None, None) => text.into_owned(),
    }
}

impl Runtime {
    /// What the open song would hand to whoever joins it: the audio files
    /// it names on this computer, how large they are and where they lie.
    /// For showing before the song is shared. It looks at the disk for
    /// each file (that it is there, how large it is, where it really
    /// lies) and asks the media cache whether it is audio, by the reading
    /// that plays it: what the song plays was read when it was opened, and
    /// what was not (a take, a file that is none) is read now. Call it
    /// away from the thread that draws.
    ///
    /// Which folder a file lies in is asked of the file system, not read
    /// off how its path is spelled: a song file that someone else wrote
    /// may name a file anywhere on the disk by a path that begins with the
    /// song's own folder and leaves it again (`..`), or through a link.
    /// Such a file is shown where it is, by itself.
    pub fn files_to_go(&self) -> Going {
        let (files, song, recordings) = {
            let session = self.session.lock();
            let received = session.received_in();
            let own = |file: &FileHere| {
                received.as_ref().is_none_or(|folder| !trill_core::files::lies_in(Path::new(&file.path), folder))
            };
            let files: Vec<String> = session.files_here().into_iter().filter(own).map(|file| file.path).collect();
            let song = session.path().and_then(|path| path.parent().map(Path::to_owned));
            (files, song, session.recordings_folders())
        };
        let files: std::collections::BTreeSet<String> = files.into_iter().collect();
        // Every folder as the file system has it; one that is not there holds nothing.
        let really = |path: &Path| std::fs::canonicalize(path).ok();
        let song = song.as_deref().and_then(really);
        let recordings: Vec<PathBuf> = recordings.iter().filter_map(|folder| really(folder)).collect();
        let library: Vec<(PathBuf, PathBuf)> =
            self.library.roots().into_iter().filter_map(|root| Some((really(&root)?, root))).collect();
        let mut going = Going::default();
        let mut folders: BTreeMap<String, (usize, u64)> = BTreeMap::new();
        let mut elsewhere = std::collections::BTreeSet::new();
        let mut no_audio = std::collections::BTreeSet::new();
        for path in files {
            let Some(found) = std::fs::metadata(&path).ok().filter(std::fs::Metadata::is_file) else {
                going.gone += 1;
                continue;
            };
            // (A file whose real place cannot be told is in no folder that is known.)
            let real = really(Path::new(&path));
            // Only audio goes, by the reading that says and hands it out
            // too (`read`): what is none is shown as what stays.
            if self.media.audio(Path::new(&path)).is_err() {
                no_audio.insert(real.as_deref().map_or(path, as_shown));
                continue;
            }
            going.files += 1;
            going.bytes += found.len();
            let within = |folder: &Path| real.as_deref().is_some_and(|real| real.starts_with(folder));
            let known = song.as_deref().filter(|folder| within(folder)).map(|_| "the song's folder".to_owned());
            let known = known
                .or_else(|| recordings.iter().any(|folder| within(folder)).then(|| "Trill's recordings".to_owned()));
            let known = known.or_else(|| {
                let (_, root) = library.iter().find(|(real, _)| within(real))?;
                Some(format!("the sample library ({})", root.display()))
            });
            match known {
                Some(folder) => {
                    let entry = folders.entry(folder).or_default();
                    (entry.0, entry.1) = (entry.0 + 1, entry.1 + found.len());
                }
                None => drop(elsewhere.insert(real.as_deref().map_or(path, as_shown))),
            }
        }
        going.elsewhere = elsewhere.into_iter().collect();
        going.no_audio = no_audio.into_iter().collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A way over which `rate` bytes a second were found to arrive, a
    /// question and the first of its answer taking `turn`.
    fn way(rate: f64, turn: Duration) -> Way {
        Way { rates: [Some(rate), None, None, None], turns: [Some(turn), None, None, None], ..Way::new() }
    }

    /// Whether two counts of bytes are the same but for a rounding.
    fn near(bytes: u64, wanted: u64) -> bool {
        bytes.abs_diff(wanted) <= 2
    }

    #[test]
    fn what_is_asked_for_ahead_is_what_is_on_its_way_and_an_eighth_of_a_second_more() {
        // Nothing is known yet: three pieces, and half of them a stride.
        let unknown = Way::new();
        assert_eq!((unknown.window(), unknown.stride()), (3 * MAX_PIECE as u64, 3 * MAX_PIECE as u64 / 2));

        // A home's line of five megabits a second, eighty milliseconds
        // there and back: what is on its way (50 kB) and what arrives in
        // an eighth of a second (78 kB), asked for again when half of the
        // second is here.
        let home = way(625_000.0, Duration::from_millis(80));
        assert!(near(home.window(), 128_125) && near(home.stride(), 39_062), "{} {}", home.window(), home.stride());
        // Where the way is longer, more is on its way, and no more waits.
        let far = way(625_000.0, Duration::from_millis(480));
        assert!(near(far.window(), 378_125) && near(far.stride(), 39_062), "{} {}", far.window(), far.stride());
        // A slow line is asked for less than a piece at a time, and never
        // for a few bytes.
        let slow = way(60_000.0, Duration::from_millis(100));
        assert_eq!((slow.window(), slow.stride()), (LEAST_WINDOW, LEAST_STRIDE));
        // A fast one for no more than sixteen megabytes ahead.
        let fast = way(400e6, Duration::from_millis(1));
        assert_eq!((fast.window(), fast.stride()), (MOST_WINDOW, MOST_WINDOW / 2));
        // A way that says it takes a minute is taken to take two seconds.
        let lost = way(1e6, Duration::from_secs(60));
        assert!(near(lost.window(), 2_125_000), "{}", lost.window());
    }

    #[test]
    fn how_long_the_way_is_is_taken_from_a_question_put_while_nothing_was_on_its_way() {
        let mut way = Way::new();
        let piece = MAX_PIECE as u64;
        assert_eq!(way.turn(), Duration::ZERO);
        // The first piece of the first answer says how long the way is,
        // and is not counted towards how fast it is.
        way.dry = Some(Instant::now() - Duration::from_millis(200));
        way.arrived(LEAST_STRIDE);
        let turn = way.turn();
        assert!(turn >= Duration::from_millis(200) && turn < Duration::from_millis(400), "{turn:?}");
        assert_eq!((way.arrived, way.rate()), (0, None));
        assert!(way.dry.is_none());
        // Two more pieces say how fast: the first time as soon as that.
        way.since = Instant::now() - Duration::from_millis(100);
        way.arrived(piece);
        assert_eq!(way.rate(), None);
        way.arrived(piece);
        let rate = way.rate().expect("how fast was measured");
        assert!(rate > 400_000.0 && rate < 1_000_000.0, "{rate}");
        // From then on over a quarter of a second at a time.
        way.arrived(piece);
        assert_eq!(way.rate(), Some(rate), "not again a moment later");
        // What the way carries is the most that was found to arrive of
        // late: a count that came out low (less was asked for than it
        // carries) does not have less asked for.
        way.since = Instant::now() - Duration::from_secs(1);
        way.arrived(piece);
        assert_eq!(way.rates[0].map(|low| low < 200_000.0), Some(true));
        assert_eq!(way.rate(), Some(rate));
        // A way that got slower is believed once the faster counts are old.
        for _ in 0..3 {
            way.since = Instant::now() - Duration::from_secs(1);
            way.arrived(piece);
        }
        assert!(way.rate().is_some_and(|slower| slower < 200_000.0), "{:?}", way.rate());
        // The way is the shortest of the last few times it was measured:
        // whatever else is in a measurement makes it longer.
        way.dry = Some(Instant::now() - Duration::from_millis(840));
        way.arrived(LEAST_STRIDE);
        assert_eq!(way.turn(), turn, "a longer one changes nothing while a shorter one is remembered");
        way.dry = Some(Instant::now() - Duration::from_millis(40));
        way.arrived(LEAST_STRIDE);
        assert!(way.turn() < Duration::from_millis(100), "a shorter one is taken at once: {:?}", way.turn());
        // A way that got longer is believed once the shorter ones are old.
        for _ in 0..4 {
            way.dry = Some(Instant::now() - Duration::from_millis(600));
            way.arrived(LEAST_STRIDE);
        }
        assert!(way.turn() >= Duration::from_millis(600), "{:?}", way.turn());
    }

    #[test]
    fn no_more_questions_are_put_in_a_second_than_a_program_that_works_in_a_song_says_things() {
        let mut way = Way::new();
        let now = Instant::now();
        assert!((0..WANTS_A_SECOND).all(|_| way.may_ask(now)));
        assert!(!way.may_ask(now), "and no more in the same second");
        assert!(way.may_ask(now + Duration::from_secs(1)), "in the next one again");
    }
}
