//! Working in one song with others: what connects the session to the
//! programs of the other people.
//!
//! The session holds the song as something that merges and says what is
//! changed in it (`trill_engine::Session::take_said`, `merge`); the wire
//! is `trill-together`. Between them is one thread of the runtime's own,
//! which is all that ever touches both:
//!
//! * It takes what the session changed and says it to whoever is
//!   connected, a few dozen times a second, with what a drag overwrote on
//!   the way left out.
//! * It takes what arrives and hands it to the session, a batch at a
//!   time: one compile for a batch.
//! * It holds who is here ([`Here`]) and what this program's own person
//!   holds, which is no part of the song and of no journal.
//!
//! The session is held for moments only and never across anything that
//! waits on the network: what is said is queued by the wire, and what
//! arrives is taken from a queue. The thread is no audio thread and no
//! thread that draws; whoever draws reads [`Runtime::together`], which
//! takes a lock of its own for a copy.
//!
//! Nobody is the host. A program that is connected to two others passes
//! on to each what it hears from the other, so three people need only
//! two connections between them.
//!
//! **A link that is lost comes back by itself.** A connection this
//! program asked for and lost on the way (not one that either side ended
//! and said so) is asked for again, after a pause that grows, for as long
//! as the song is open and nobody leaves: what each did meanwhile is
//! merged when it is back, as when two meet again by hand. Nobody pastes
//! an invitation twice in one sitting. Only where asking again is no use
//! (the invitation is another's, or its meeting was ended) does it stop,
//! with the sentence that says to ask for the invitation again.
//!
//! **Through a meeting point** ([`Runtime::share_through_meeting_point`])
//! this program listens for nothing: it holds a meeting open at the
//! meeting point the settings name, which is opened again by itself when
//! it is lost. The meeting point's address and the key its keeper gave
//! are in the settings and nowhere else; the key is said to nobody, not
//! by [`Runtime::meeting_point`] either.

use crate::files::{Files, Heard, OnItsWay, Tended};
use crate::renditions::Renditions;
use crate::{Runtime, SharedSession};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use trill_core::merge::{Key, Replica, Said};
use trill_core::{ClipId, DeviceId, TrackId};
use trill_together::meet::{No, Turned};
use trill_together::{
    Event, Invitation, JoinId, LinkId, MeetingPoint, Message, Net, PROTOCOL, Person, Presence, Reach,
};

/// How often the thread looks at the session and the wire.
const TICK: Duration = Duration::from_millis(25);
/// How often someone who holds nothing new says they are still there.
const STILL_HERE: Duration = Duration::from_millis(1000);
/// How often a presence that changes (a playhead that runs) is said.
const PRESENCE_EVERY: Duration = Duration::from_millis(100);
/// How many colours people are told apart by: `trill-noir` has that many.
pub const COLOURS: u8 = 8;
/// How many sentences are kept for a front end that has not read them.
const SAID_KEPT: usize = 64;
/// How much of what a meeting point's keeper lets a key pass in a day is
/// left for what is said of the song: below it, the song's audio files
/// wait for the next day. (A drag is six kilobytes a second.)
const LEFT_FOR_THE_SONG: u64 = 64 << 20;
/// What a program says as it leaves, and as it is closed.
const LEFT: &str = "left";
const CLOSED: &str = "the program was closed";
/// How long a connection that was lost is left alone before it is asked
/// for again: this long the first time and twice as long each time
/// after, up to the longest.
const FIRST_PAUSE: Duration = Duration::from_secs(1);
const LONGEST_PAUSE: Duration = Duration::from_secs(30);

/// Someone else who has the song open, as a front end shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct Here {
    pub person: Person,
    /// Where their song stood or played when they last said, in beats.
    pub beat: f64,
    pub playing: bool,
    /// When that was heard: a playhead that runs is drawn on from it.
    pub heard: Instant,
    pub tracks: Vec<TrackId>,
    pub clips: Vec<ClipId>,
    /// Notes they have selected, each by its clip and its id.
    pub notes: Vec<(ClipId, u64)>,
    /// What they have hold of right now, in words.
    pub holding: Option<String>,
    /// The devices of the song their machine has not got.
    pub lacks: Vec<DeviceId>,
}

/// What this program's own person holds: said to the others, and never
/// part of the song.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mine {
    pub tracks: Vec<TrackId>,
    pub clips: Vec<ClipId>,
    pub notes: Vec<(ClipId, u64)>,
    pub holding: Option<String>,
}

/// How a meeting stands at the meeting point a song is shared through.
#[derive(Clone, Debug, PartialEq)]
pub enum Meeting {
    /// The meeting point is being reached.
    Reaching,
    /// The meeting is open there: whoever has the invitation gets in.
    Held,
    /// It is not at the moment, and why. This program goes on trying,
    /// and the song is its person's to go on with meanwhile.
    Away(String),
}

/// A connection this program asked for and lost on the way, which it is
/// asking for again by itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Away {
    /// Who was at the other end, as they said.
    pub name: String,
    pub since: Instant,
    /// How often it has been asked for again.
    pub tries: u32,
}

/// The meeting point a program shares through, as the settings have it.
/// (Never the key: whether there is one.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingPointSet {
    /// Its address as its keeper wrote it: `meet.example.org:443`.
    pub address: String,
    pub has_key: bool,
}

/// How this program stands with the others.
#[derive(Clone, Debug, PartialEq)]
pub struct Together {
    /// This program's own person.
    pub me: Person,
    /// The invitation the song is shared under, while it is: text to hand
    /// to someone.
    pub invitation: Option<String>,
    /// Whether a connection that was asked for is still being made.
    pub joining: bool,
    /// How many programs this one is connected to.
    pub connected: usize,
    /// The other people who have the song open, by when they came.
    pub people: Vec<Here>,
    /// Goes up whenever anything of this changes.
    pub revision: u64,
    /// How many bytes this program has said to the others, and in how
    /// many messages: for whoever measures.
    pub said: (u64, u64),
    /// Where the song is shared through a meeting point: its address as
    /// its keeper wrote it, and how the meeting stands there.
    pub meeting: Option<(String, Meeting)>,
    /// The connections that were lost and are being asked for again.
    pub away: Vec<Away>,
    /// The files of the song that are on their way from other programs.
    pub files: Vec<OnItsWay>,
    /// The files the song names on this computer that have not gone to
    /// the others, because nobody was shown them: a song file that was
    /// opened named them, or something that is not the user's hand did.
    /// They go when the user lets them ([`Runtime::let_files_go`]).
    pub held: Vec<String>,
    /// For how many seconds the song's audio files still wait because
    /// the meeting point it is shared through has nearly passed what its
    /// keeper lets a key pass in a day. The song itself goes on.
    pub files_wait: Option<u64>,
    /// The track this program is recording for the others, who have not
    /// got all its devices, by its name: while it does.
    pub recording_for_others: Option<String>,
}

impl Together {
    /// Whether the song is being worked in with others, or offered to be.
    pub fn is_on(&self) -> bool {
        self.invitation.is_some() || self.joining || self.connected > 0 || !self.away.is_empty()
    }
}

/// What a front end asks of the thread.
enum Ask {
    Share(Reach, u16),
    ShareThrough(MeetingPoint),
    /// The user's word: however much of the song's audio may be on its
    /// way at once.
    LetFilesArrive,
    StopSharing,
    Join(Invitation, bool),
    Leave,
}

/// What the thread and the front ends both hold.
struct Shared {
    status: Together,
    mine: Mine,
    /// Sentences for whoever shows them, each with its number.
    said: Vec<(u64, String)>,
    last_said: u64,
    asks: Vec<Ask>,
    /// What came of the last share: the invitation, or why not.
    shared: Option<Result<String, String>>,
}

impl Shared {
    fn say(&mut self, sentence: impl Into<String>) {
        self.last_said += 1;
        self.said.push((self.last_said, sentence.into()));
        if self.said.len() > SAID_KEPT {
            self.said.remove(0);
        }
        self.status.revision += 1;
    }
}

/// The runtime's part in working together: started with the runtime, and
/// idle until someone shares or joins.
pub(crate) struct Sharing {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    session: SharedSession,
    /// The folder files from other programs are kept in, and whether it
    /// is one for this run that goes with it.
    received: PathBuf,
    for_the_run: bool,
    /// What reads a file as audio, and how many bytes of files may be on
    /// their way at once without the user's word.
    media: Arc<trill_media::MediaCache>,
    unasked: u64,
}

/// A number by lot that a number in JSON holds exactly.
fn lot() -> u64 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_nanos()),
    );
    hasher.finish() & ((1 << 53) - 1)
}

/// The name someone goes by until they say another: the one they are
/// signed in to the machine by.
fn name_on_this_machine() -> String {
    let named = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    match named.trim() {
        "" => "Someone".to_owned(),
        named => named.to_owned(),
    }
}

/// The colour each person is shown in, worked out from who is there: the
/// same in every program, with no word said about it. Each takes the
/// first colour that is free from the one their id falls on, the lower id
/// first.
pub fn colours(people: &[u64]) -> HashMap<u64, u8> {
    let mut sorted = people.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut taken = HashSet::new();
    let mut given = HashMap::new();
    for id in sorted {
        let wanted = (id % u64::from(COLOURS)) as u8;
        let colour = (0..COLOURS).map(|step| (wanted + step) % COLOURS).find(|colour| !taken.contains(colour));
        let colour = colour.unwrap_or(wanted);
        taken.insert(colour);
        given.insert(id, colour);
    }
    given
}

/// What a drag overwrote on the way is left out: of the changes that set
/// one property, the last. What makes things is said as it is.
fn thinned(said: Vec<Said>) -> Vec<Said> {
    let mut later: HashSet<Key> = HashSet::new();
    let mut kept: Vec<Said> = Vec::with_capacity(said.len());
    for mut one in said.into_iter().rev() {
        let ops = std::mem::take(&mut one.change.ops);
        one.change.ops = ops.into_iter().filter(|op| op.key().is_none_or(|key| later.insert(key))).collect();
        if !one.change.ops.is_empty() {
            kept.push(one);
        }
    }
    kept.reverse();
    kept
}

/// A program this one is connected to.
struct Peer {
    /// Whether this program asked for the connection.
    joined: bool,
    /// Whether a song it hands over may take the place of one with
    /// changes that are not saved.
    replace: bool,
    /// The invitation it was asked for by: what it is asked for again by
    /// when it is lost.
    invitation: Option<Invitation>,
    person: Option<Person>,
    replica: Option<Replica>,
    /// Both hold the same song, and each has been handed what it lacked:
    /// from here on changes go both ways as they are made.
    synced: bool,
    /// Whether the connection is one through the meeting point this
    /// program shares through: what passes on it counts against the key.
    through: bool,
}

/// A connection that was asked for and has not come about yet.
struct Joining {
    join: JoinId,
    replace: bool,
    invitation: Invitation,
    /// Whether it is one that was lost, asked for again.
    again: bool,
}

/// A connection this program asked for and lost on the way.
struct Again {
    invitation: Invitation,
    name: String,
    since: Instant,
    tries: u32,
    next: Instant,
    state: Trying,
}

/// How far asking again has got.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Trying {
    /// Left alone until its time.
    Waiting,
    /// Asked for.
    Asked(JoinId),
    /// Connected, and waiting for the other to say who it is.
    Greeting(LinkId),
}

impl Again {
    fn new(invitation: Invitation, name: String) -> Again {
        let now = Instant::now();
        Again { invitation, name, since: now, tries: 0, next: now + FIRST_PAUSE, state: Trying::Waiting }
    }

    /// It did not come about this time: left alone for longer than the
    /// time before.
    fn wait(&mut self) {
        let doubled = FIRST_PAUSE.saturating_mul(1u32 << self.tries.min(8));
        (self.state, self.next) = (Trying::Waiting, Instant::now() + doubled.min(LONGEST_PAUSE));
    }
}

/// What the thread holds.
struct Driver {
    session: SharedSession,
    shared: Arc<Mutex<Shared>>,
    net: Net,
    peers: HashMap<LinkId, Peer>,
    /// Who is here, and through which connection each was heard of.
    people: Vec<(Here, LinkId, u64)>,
    /// The song the connections are about: another one ends them.
    epoch: Option<u64>,
    /// Joins asked for that have not come to a connection yet.
    joining: Vec<Joining>,
    /// The connections that were lost and are asked for again.
    again: Vec<Again>,
    /// The people whose connection was lost on the way and is theirs to
    /// ask for again: said to be away, and back when they are.
    were_away: HashSet<u64>,
    /// The meeting point the song is shared through, and how the meeting
    /// stands there.
    through: Option<(MeetingPoint, Meeting)>,
    presence_seq: u64,
    presence_said: Option<(Presence, Instant)>,
    /// The song's files: what is handed out, and what is on its way.
    files: Files,
    /// The files the song names on this computer that may not go to the
    /// others: nobody was shown them.
    held: Vec<String>,
    /// What the meeting point said the key may still pass today, when
    /// its day is over, and how many bytes of files had passed here then.
    allowance: Option<(u64, Instant, u64)>,
    /// Until when the song's files wait for the meeting point's next day.
    files_wait: Option<Instant>,
    /// The tracks this program records for the others, where it has
    /// devices they have not got.
    renditions: Renditions,
}

impl Driver {
    fn is_live(&self) -> bool {
        let waited_for = !self.joining.is_empty() || !self.again.is_empty();
        self.net.listening().is_some() || !self.peers.is_empty() || waited_for
    }

    fn say(&self, sentence: impl Into<String>) {
        self.shared.lock().say(sentence);
    }

    fn name_of(&self, link: LinkId) -> String {
        let named = self.peers.get(&link).and_then(|peer| peer.person.as_ref()).map(|person| person.name.clone());
        named.filter(|name| !name.is_empty()).unwrap_or_else(|| "Someone".to_owned())
    }

    /// Everything is over: nobody is connected and nobody can connect.
    fn leave(&mut self, why: &str) {
        self.net.close_all(why);
        self.peers.clear();
        self.people.clear();
        self.joining.clear();
        self.again.clear();
        self.through = None;
        self.epoch = None;
        self.files.forget();
        self.held.clear();
        (self.allowance, self.files_wait) = (None, None);
        self.renditions.leave(&self.session);
        self.session.lock().hear_changes(false);
    }

    fn hello(&self) -> Message {
        let me = self.shared.lock().status.me.clone();
        let session = self.session.lock();
        Message::Hello {
            protocol: PROTOCOL,
            build: env!("CARGO_PKG_VERSION").to_owned(),
            person: me,
            song: session.shared_song(),
            floors: session.floors(),
            replica: session.replica(),
        }
    }

    fn asks(&mut self) {
        let asks = std::mem::take(&mut self.shared.lock().asks);
        for ask in asks {
            match ask {
                Ask::Share(reach, port) => {
                    // A song that is shared is one that merges: it has an
                    // id from here on, and its changes are kept to be said.
                    let came = self.net.listen(reach, port);
                    if came.is_ok() {
                        let mut session = self.session.lock();
                        session.share();
                        // Whoever shares was shown what the song names on
                        // this computer: it goes to whoever joins.
                        session.let_files_go();
                        session.hear_changes(true);
                        self.epoch = Some(session.song_epoch());
                    }
                    let came = came
                        .map(|invitation| invitation.to_string())
                        .map_err(|error| format!("this program could not start listening for others: {error}"));
                    // Whoever asked reads how things stand as soon as it
                    // has its answer: that is said first.
                    self.show();
                    self.shared.lock().shared = Some(came);
                }
                Ask::ShareThrough(point) => {
                    let came = self.net.listen_through(&point);
                    if let Ok(invitation) = &came {
                        let mut session = self.session.lock();
                        session.share();
                        session.let_files_go();
                        session.hear_changes(true);
                        self.epoch = Some(session.song_epoch());
                        // (A program that shares already goes on as it does.)
                        if invitation.meet && self.through.is_none() {
                            self.through = Some((point, Meeting::Reaching));
                        }
                    }
                    let came = came
                        .map(|invitation| invitation.to_string())
                        .map_err(|error| format!("this program could not start sharing through it: {error}"));
                    self.show();
                    self.shared.lock().shared = Some(came);
                }
                Ask::LetFilesArrive => self.files.any_amount(),
                Ask::StopSharing => {
                    self.net.stop_listening();
                    self.through = None;
                }
                Ask::Join(invitation, replace) => {
                    let mut session = self.session.lock();
                    session.hear_changes(true);
                    self.epoch = Some(session.song_epoch());
                    drop(session);
                    let join = self.net.join(invitation.clone());
                    self.joining.push(Joining { join, replace, invitation, again: false });
                }
                Ask::Leave => {
                    if self.is_live() {
                        // Nothing is said here of what one did oneself.
                        self.leave(LEFT);
                        let _ = self.net.events();
                    }
                }
            }
        }
    }

    fn heard(&mut self, link: LinkId, message: Message) {
        let Some(peer) = self.peers.get(&link) else { return };
        let (greeted, synced, joined, replace) = (peer.person.is_some(), peer.synced, peer.joined, peer.replace);
        match message {
            Message::Hello { protocol, build, person, song, floors, replica } => {
                if protocol != PROTOCOL {
                    let mine = env!("CARGO_PKG_VERSION");
                    let why = format!(
                        "the two programs do not speak the same version of working together (this one {PROTOCOL}, \
                         build {mine}; the other {protocol}, build {build}): both need the same build of Trill"
                    );
                    self.say(format!("No connection: {why}."));
                    // (Said: its going is nothing more to say.)
                    self.peers.remove(&link);
                    self.net.close(link, &why);
                    return;
                }
                if greeted {
                    return;
                }
                let person = Presence::of(person).bounded().person;
                // The same program on another connection: it lost the one
                // it had (its line went down, and it saw that), and here
                // that one was not seen to end yet. The older one is let go
                // of without a word: its person is here.
                let older: Vec<LinkId> = (self.peers.iter())
                    .filter(|(other, peer)| {
                        **other != link && peer.person.as_ref().is_some_and(|known| known.id == person.id)
                    })
                    .map(|(other, _)| *other)
                    .collect();
                for old in &older {
                    self.files.lost(*old);
                    self.peers.remove(old);
                    for (_, via, _) in &mut self.people {
                        if *via == *old {
                            *via = link;
                        }
                    }
                    self.net.close(*old, "the same program connected again");
                }
                let came_back = self.were_away.remove(&person.id);
                let mut session = self.session.lock();
                let same = song.is_some() && song == session.shared_song();
                let reply = if same {
                    // Each is handed what it lacks of the other's.
                    let said = session.since(&floors);
                    Some(Message::Changes { said, floors: session.floors(), replica: session.replica() })
                } else if !joined {
                    // The one who came has not got the song: all of it.
                    Some(Message::Song { text: session.whole_song() })
                } else {
                    // The one who came waits to be handed it.
                    None
                };
                drop(session);
                let name = person.name.clone();
                if let Some(peer) = self.peers.get_mut(&link) {
                    (peer.person, peer.replica) = (Some(person), replica);
                    peer.synced = reply.is_some();
                }
                // A connection that was lost is back with this.
                let before = self.again.len();
                self.again.retain(|again| again.state != Trying::Greeting(link));
                let back = self.again.len() != before;
                // Whoever came is in the song once they have it: said now
                // where both had it already, and else when it is handed
                // over.
                match (joined, reply.is_some()) {
                    (true, true) if back => {
                        self.say(format!("The connection is back: you are in the song with {name}."));
                    }
                    (true, true) => self.say(format!("You are in the song with {name}.")),
                    (true, false) => {}
                    // (Never said to be away: nothing to say of their return.)
                    (false, _) if !older.is_empty() => {}
                    (false, _) if came_back => self.say(format!("{name} is back.")),
                    (false, _) => self.say(format!("{name} is here.")),
                }
                if let Some(reply) = reply {
                    self.net.say(link, &reply);
                }
                // Who else is here, as far as this program knows.
                let others: Vec<Presence> = self.people.iter().map(|(here, ..)| presence_of(here)).collect();
                for presence in others {
                    self.net.say(link, &Message::Presence(presence));
                }
                self.presence_said = None;
            }
            _ if !greeted => {
                self.net.close(link, "it said something before saying who it is");
            }
            Message::Song { text } => {
                if !joined || synced {
                    self.net.close(link, "it handed over a song that was not asked for");
                    return;
                }
                let mut session = self.session.lock();
                if session.is_modified() && !replace {
                    drop(session);
                    let why = "the song that is open here has changes that are not saved, and joining would replace it";
                    self.say(format!("Not joined: {why}. Save it, or join and let it go."));
                    self.net.close(link, why);
                    return;
                }
                let taken = session.take_song(&text);
                let epoch = session.song_epoch();
                session.hear_changes(true);
                drop(session);
                match taken {
                    Ok(()) => {
                        self.epoch = Some(epoch);
                        if let Some(peer) = self.peers.get_mut(&link) {
                            peer.synced = true;
                        }
                        self.say(format!("You are in the song with {}.", self.name_of(link)));
                    }
                    Err(error) => {
                        let why = format!("the song that was handed over could not be opened: {error}");
                        self.say(format!("Not joined: {why}."));
                        self.net.close(link, &why);
                    }
                }
            }
            Message::Changes { said, floors, replica } => {
                if !synced && !joined {
                    self.net.close(link, "it said changes to a song this program has not got with it");
                    return;
                }
                let by = self.name_of(link);
                let direct = replica.or(self.peers.get(&link).and_then(|peer| peer.replica));
                let mut session = self.session.lock();
                let merged = session.merge(&by, said, Some(&floors), direct);
                let (mine, number) = (session.floors(), session.replica());
                drop(session);
                if let Some(peer) = self.peers.get_mut(&link) {
                    peer.synced = true;
                    peer.replica = direct;
                }
                match merged {
                    // What was news here is news to whoever else is
                    // connected.
                    Ok(news) if !news.is_empty() => {
                        let onward = Message::Changes { said: news, floors: mine, replica: number };
                        self.say_to_synced(&onward, Some(link));
                    }
                    Ok(_) => {}
                    Err(refused) => {
                        let why = format!("it said a change that no edit could have made ({refused})");
                        self.say(format!("{by} is no longer connected: their program {why}."));
                        self.net.close(link, &why);
                    }
                }
            }
            Message::Presence(presence) => {
                let presence = presence.bounded();
                let me = self.shared.lock().status.me.id;
                if presence.person.id == me {
                    return;
                }
                let known = self.people.iter_mut().find(|(here, ..)| here.person.id == presence.person.id);
                let news = match known {
                    Some((_, _, seq)) if *seq >= presence.seq => false,
                    Some((here, via, seq)) => {
                        (*here, *via, *seq) = (here_of(&presence), link, presence.seq);
                        true
                    }
                    None => {
                        self.people.push((here_of(&presence), link, presence.seq));
                        true
                    }
                };
                if news {
                    self.say_to_greeted(&Message::Presence(presence), Some(link));
                    self.shared.lock().status.revision += 1;
                }
            }
            Message::Gone { person } => {
                let before = self.people.len();
                self.people.retain(|(here, ..)| here.person.id != person);
                if self.people.len() != before {
                    self.say_to_greeted(&Message::Gone { person }, Some(link));
                    self.shared.lock().status.revision += 1;
                }
            }
            // (Neither is handed on by the connection: said by it, to it.)
            Message::Bye { .. } | Message::Still => {}
            // The song's files. Nothing of them is said between programs
            // that do not hold the same song yet: whatever is asked for
            // then is not here.
            Message::Want { file, .. } if !synced => self.net.say(link, &Message::Nothing { file }),
            Message::Have { .. } | Message::Nothing { .. } | Message::Later { .. } if !synced => {}
            said @ (Message::Have { .. }
            | Message::Want { .. }
            | Message::Nothing { .. }
            | Message::Later { .. }
            | Message::Piece { .. }) => match self.files.heard(link, said) {
                Heard::Fine => {}
                Heard::Later(seconds) => self.say(format!(
                    "Audio files from {} wait: the meeting point you are connected through has nearly passed what \
                     its keeper lets pass in a day, and what is left is kept for the song itself. The song goes on; \
                     its files go on arriving in {}.",
                    self.name_of(link),
                    in_about(seconds)
                )),
                Heard::Answer(answer) => self.net.say(link, &answer),
                Heard::LetGo(why) => self.let_go(link, why),
            },
        }
    }

    /// Lets go of a program that said what nobody who holds the song with
    /// this one says, in a sentence.
    fn let_go(&mut self, link: LinkId, why: &str) {
        self.say(format!("{} is no longer connected: their program {why}.", self.name_of(link)));
        self.net.close(link, why);
    }

    /// Holds the song's files back where the meeting point has nearly
    /// passed what its keeper lets this key pass in a day: what is left
    /// is for what is said of the song, and the files go on when the day
    /// is over there.
    fn keep_the_day_for_the_song(&mut self) {
        let Some((point, _)) = &self.through else { return };
        let Some((left, over, passed_then)) = self.allowance else { return };
        // (What has passed here since the meeting point said.)
        let left = left.saturating_sub(self.files.passed().saturating_sub(passed_then));
        let wait = left < LEFT_FOR_THE_SONG && Instant::now() < over;
        let through: Vec<LinkId> = self.peers.iter().filter(|(_, peer)| peer.through).map(|(link, _)| *link).collect();
        // No more is handed over than leaves that: the thread that hands
        // out stops there by itself, however much was asked for at once
        // and however fast it goes between two looks of this one.
        let more = (Instant::now() < over).then(|| left.saturating_sub(LEFT_FOR_THE_SONG));
        for link in &through {
            self.files.may_hand(*link, more);
        }
        if wait == self.files_wait.is_some() {
            return;
        }
        let at = point.shown();
        let seconds = over.saturating_duration_since(Instant::now()).as_secs().max(1);
        for link in through {
            // Whoever asked for files over it is told, at once: nothing
            // that was asked for before is handed over either.
            if self.files.close_way(link, if wait { seconds } else { 0 }) && wait {
                self.net.say(link, &Message::Later { seconds });
            }
        }
        match wait {
            true => {
                self.files_wait = Some(over);
                self.say(format!(
                    "The meeting point {at} has nearly passed what its keeper lets a key pass in a day. What is left \
                     is kept for the song itself, which goes on; its audio files wait, and go on in {}, when the \
                     day is over there.",
                    in_about(seconds)
                ));
            }
            false => {
                self.files_wait = None;
                self.say(format!("The song's audio files go on through the meeting point {at}."));
            }
        }
        self.shared.lock().status.revision += 1;
    }

    /// Looks after the song's files: has them looked at on this disk
    /// when the song has changed, and does what that and what arrived
    /// come to. Nothing here reads or writes a file: two threads of the
    /// files' own do.
    fn tend_files(&mut self) {
        self.keep_the_day_for_the_song();
        // (Someone who came while the files wait is on a way that is closed.)
        if let Some(over) = self.files_wait {
            let seconds = over.saturating_duration_since(Instant::now()).as_secs().max(1);
            let through: Vec<LinkId> =
                self.peers.iter().filter(|(_, peer)| peer.through).map(|(link, _)| *link).collect();
            for link in through {
                if self.files.close_way(link, seconds) {
                    self.net.say(link, &Message::Later { seconds });
                }
            }
        }
        let revision = self.session.lock().revision();
        if self.files.look_is_due(revision) {
            let (files, received, held) = {
                let session = self.session.lock();
                (session.files_here(), session.received_in(), session.files_held())
            };
            self.files.look(revision, files, received);
            // What the song names on this computer that nobody was shown
            // stays here: said once, with where to look at it.
            if held != self.held {
                if held.len() > self.held.len() {
                    let files = if held.len() == 1 { "1 file".to_owned() } else { format!("{} files", held.len()) };
                    self.say(format!(
                        "{files} that the shared song names on this computer {} not gone to the others: nobody was \
                         shown {}. Look at what is held where the song is shared (File, Share this song), and let \
                         it go if it is to.",
                        if held.len() == 1 { "has" } else { "have" },
                        if held.len() == 1 { "it" } else { "them" },
                    ));
                }
                self.held = held;
                self.shared.lock().status.revision += 1;
            }
        }
        let mut synced: Vec<LinkId> =
            self.peers.iter().filter(|(_, peer)| peer.synced).map(|(link, _)| *link).collect();
        synced.sort_unstable();
        let names: HashMap<LinkId, String> = self.peers.keys().map(|link| (*link, self.name_of(*link))).collect();
        let name = |link: LinkId| names.get(&link).cloned().unwrap_or_else(|| "Someone".to_owned());
        let (mut here, mut kept) = (Vec::new(), Vec::new());
        for tended in self.files.tend(&synced, &name) {
            match tended {
                Tended::Say(link, message) => self.net.say(link, &message),
                Tended::Tell(sentence) => self.say(sentence),
                Tended::SayFile(place, path, is) => {
                    self.session.lock().say_file(&place, &path, is);
                }
                Tended::Read(read) => self.session.lock().files_read(read),
                Tended::Here(path, read) => {
                    here.push(path);
                    kept.extend(read);
                }
                Tended::LetGo(link, why) => self.let_go(link, why),
            }
        }
        if !here.is_empty() {
            // (Each was read as it arrived, by the thread that took it in:
            // the song is not held while a long recording is read.)
            self.session.lock().files_were_read(&here);
        }
        drop(kept);
    }

    /// Looks after the tracks this program records for the others: what
    /// was recorded is taken in, and the next is begun, away from here.
    fn tend_renditions(&mut self) {
        let others: Vec<(u64, Vec<DeviceId>)> =
            self.people.iter().map(|(here, ..)| (here.person.id, here.lacks.clone())).collect();
        let me = self.shared.lock().status.me.clone();
        for sentence in self.renditions.tend(&self.session, &me, &others) {
            self.say(sentence);
        }
    }

    fn say_to_synced(&mut self, message: &Message, except: Option<LinkId>) {
        let to: Vec<LinkId> = self
            .peers
            .iter()
            .filter(|(link, peer)| peer.synced && Some(**link) != except)
            .map(|(link, _)| *link)
            .collect();
        self.net.say_to(&to, message);
    }

    /// Says `message` to every program this one has greeted, but `except`.
    /// Never to every connection the network has: one is there before this
    /// thread has heard of it and said who it is, and a program that hears
    /// who is here before it hears that takes the other for no Trill and
    /// lets go. That is how a third person once never got into a song.
    fn say_to_greeted(&mut self, message: &Message, except: Option<LinkId>) {
        let to: Vec<LinkId> = self.peers.keys().copied().filter(|link| Some(*link) != except).collect();
        self.net.say_to(&to, message);
    }

    fn events(&mut self) {
        for event in self.net.events() {
            match event {
                Event::Open { link, joined } => {
                    let asked = joined
                        .and_then(|join| self.joining.iter().position(|joining| joining.join == join))
                        .map(|at| self.joining.remove(at));
                    // One that was lost and asked for again is as far as
                    // the greeting.
                    for again in &mut self.again {
                        if joined.is_some_and(|join| again.state == Trying::Asked(join)) {
                            again.state = Trying::Greeting(link);
                        }
                    }
                    let (replace, invitation) = match asked {
                        Some(asked) => (asked.replace, Some(asked.invitation)),
                        None => (false, None),
                    };
                    let joined = joined.is_some();
                    let through = !joined && self.through.is_some();
                    let peer =
                        Peer { joined, replace, invitation, person: None, replica: None, synced: false, through };
                    self.peers.insert(link, peer);
                    let hello = self.hello();
                    self.net.say(link, &hello);
                }
                Event::Heard { link, message } => self.heard(link, message),
                Event::Lost { link, why, told } => {
                    self.files.lost(link);
                    let Some(peer) = self.peers.remove(&link) else { continue };
                    // Whoever was heard of through it is gone with it.
                    let gone: Vec<u64> = self
                        .people
                        .iter()
                        .filter(|(_, via, _)| *via == link)
                        .map(|(here, ..)| here.person.id)
                        .collect();
                    self.people.retain(|(_, via, _)| *via != link);
                    for person in gone {
                        self.say_to_greeted(&Message::Gone { person }, None);
                    }
                    // One that was asked for again and lost again before
                    // the greeting: nothing new to say, and asked for again.
                    let greeting = self.again.iter_mut().find(|again| again.state == Trying::Greeting(link));
                    if let Some(again) = greeting {
                        again.wait();
                        self.shared.lock().status.revision += 1;
                        continue;
                    }
                    match (peer.person, peer.invitation) {
                        // Lost on the way, and this program had asked for
                        // it: it asks again by itself.
                        (Some(person), Some(invitation)) if !told => {
                            self.say(format!(
                                "The connection to {} was lost ({why}). This program asks for it again by itself; \
                                 what each of you does meanwhile is merged when it is back.",
                                person.name
                            ));
                            self.again.push(Again::new(invitation, person.name));
                        }
                        (Some(person), _) => match why.as_str() {
                            LEFT => self.say(format!("{} left.", person.name)),
                            CLOSED => self.say(format!("{} closed their Trill.", person.name)),
                            // Lost on the way, and theirs to ask for again.
                            why if !told => {
                                self.were_away.insert(person.id);
                                self.say(format!(
                                    "{} is away: the connection was lost ({why}). Their program asks for it again \
                                     by itself.",
                                    person.name
                                ));
                            }
                            why => self.say(format!("{} is no longer here: {why}.", person.name)),
                        },
                        // A connection this program asked for, gone
                        // before the other had said who it is: the join
                        // came to nothing, and a failure is a sentence.
                        (None, _) if peer.joined => self.say(format!(
                            "Not joined: the connection ended before the other program had said who it is ({why}). \
                             It may not be a Trill, or it stopped sharing its song; ask for the invitation again."
                        )),
                        (None, _) => {}
                    }
                    self.shared.lock().status.revision += 1;
                }
                Event::Held => {
                    let Some((point, meeting)) = &mut self.through else { continue };
                    let back = matches!(meeting, Meeting::Away(_));
                    *meeting = Meeting::Held;
                    let at = point.shown();
                    if back {
                        self.say(format!("The meeting point {at} is back: whoever has the invitation gets in again."));
                    }
                    self.shared.lock().status.revision += 1;
                }
                Event::Allowance { left, over } => {
                    self.allowance = Some((left, over, self.files.passed()));
                }
                Event::Unheld { turned, again } => {
                    let Some((point, meeting)) = &mut self.through else { continue };
                    let at = point.shown();
                    let sentence = turned.sentence();
                    if !again {
                        // Something has to be changed first: nothing is
                        // held there, and nothing is tried.
                        self.through = None;
                        self.net.stop_listening();
                        self.say(format!(
                            "Not shared through the meeting point {at}: {sentence}. The song is not shared, and is \
                             yours as it was."
                        ));
                    } else {
                        // Said when it begins, and not again for every try.
                        let sentence = turned.briefly();
                        let news = *meeting != Meeting::Away(sentence.clone());
                        let was_held = *meeting == Meeting::Held;
                        *meeting = Meeting::Away(sentence.clone());
                        if news && turned == Turned::No(No::Spent) {
                            self.say(format!(
                                "The meeting point {at} passes no more for your key today: it has passed as much as \
                                 its keeper lets a key pass in a day. Nobody gets into the song through it until \
                                 tomorrow, or until its keeper allows more. The song is yours to go on with alone \
                                 meanwhile, and what each of you does is merged when you meet again."
                            ));
                        } else if news && was_held {
                            self.say(format!(
                                "The meeting point {at} is away ({sentence}). This program goes on trying; the \
                                 invitation stays the one it is, and the song is yours to go on with meanwhile."
                            ));
                        } else if news {
                            self.say(format!(
                                "The meeting point {at} is not reached ({sentence}). This program goes on trying; \
                                 until it is reached nobody gets into the song by the invitation."
                            ));
                        }
                    }
                    self.shared.lock().status.revision += 1;
                }
                Event::Failed { join, why, for_good } => {
                    if let Some(at) = self.joining.iter().position(|joining| joining.join == join) {
                        self.joining.remove(at);
                    }
                    match self.again.iter().position(|again| again.state == Trying::Asked(join)) {
                        // The invitation is no longer one: there is nothing
                        // to ask for again.
                        Some(at) if for_good => {
                            let again = self.again.remove(at);
                            self.say(format!(
                                "No longer in the song with {}: {why}. Ask for the invitation again.",
                                again.name
                            ));
                        }
                        Some(at) => self.again[at].wait(),
                        None => self.say(format!("Not joined: {why}.")),
                    }
                    self.shared.lock().status.revision += 1;
                }
            }
        }
    }

    /// What was changed here is said to the others, and where this
    /// program's person is.
    fn tell(&mut self) {
        let mut session = self.session.lock();
        // Another song was opened: the one the others are in is closed.
        if self.epoch.is_some_and(|epoch| epoch != session.song_epoch()) {
            drop(session);
            self.leave("opened another song");
            self.say("You left the shared song: another song was opened.");
            return;
        }
        let said = session.take_said();
        let (floors, replica) = (session.floors(), session.replica());
        let transport = session.transport();
        let holding = session.holding();
        let lacks = session.lacking();
        drop(session);
        if !said.is_empty() && self.peers.values().any(|peer| peer.synced) {
            let message = Message::Changes { said: thinned(said), floors, replica };
            self.say_to_synced(&message, None);
        }

        let (me, mine) = {
            let shared = self.shared.lock();
            (shared.status.me.clone(), shared.mine.clone())
        };
        let mut presence = Presence::of(me);
        (presence.beat, presence.playing) = (transport.position_beats, transport.playing);
        (presence.tracks, presence.clips, presence.notes, presence.holding) =
            (mine.tracks, mine.clips, mine.notes, mine.holding.or(holding));
        presence.lacks = lacks;
        let presence = presence.bounded();
        let due = match &self.presence_said {
            None => true,
            Some((last, when)) => {
                let same = Presence { seq: 0, ..last.clone() } == presence;
                when.elapsed() >= if same { STILL_HERE } else { PRESENCE_EVERY }
            }
        };
        if due && !self.peers.is_empty() {
            self.presence_seq += 1;
            let saying = Presence { seq: self.presence_seq, ..presence.clone() };
            self.say_to_greeted(&Message::Presence(saying), None);
            self.presence_said = Some((presence, Instant::now()));
        }
    }

    /// Asks again for the connections that were lost, each when its time
    /// has come.
    fn ask_again(&mut self) {
        let now = Instant::now();
        for again in &mut self.again {
            if again.state == Trying::Waiting && now >= again.next {
                let join = self.net.join(again.invitation.clone());
                (again.state, again.tries) = (Trying::Asked(join), again.tries + 1);
                let invitation = again.invitation.clone();
                self.joining.push(Joining { join, replace: false, invitation, again: true });
            }
        }
    }

    /// What a front end is shown.
    fn show(&mut self) {
        let mut shared = self.shared.lock();
        let me = shared.status.me.id;
        let ids: Vec<u64> = self.people.iter().map(|(here, ..)| here.person.id).chain([me]).collect();
        let colours = colours(&ids);
        let people: Vec<Here> = (self.people.iter())
            .map(|(here, ..)| {
                let mut here = here.clone();
                here.person.colour = colours.get(&here.person.id).copied().unwrap_or(0);
                here
            })
            .collect();
        let status = Together {
            me: Person { colour: colours.get(&me).copied().unwrap_or(0), ..shared.status.me.clone() },
            invitation: self.net.listening().map(|invitation| invitation.to_string()),
            joining: self.joining.iter().any(|joining| !joining.again),
            connected: self.peers.len(),
            people,
            revision: shared.status.revision,
            said: self.net.said(),
            meeting: self.through.as_ref().map(|(point, meeting)| (point.shown(), meeting.clone())),
            away: (self.again.iter())
                .map(|again| Away { name: again.name.clone(), since: again.since, tries: again.tries })
                .collect(),
            files: self.files.on_their_way(&|link| {
                let person = self.peers.get(&link).and_then(|peer| peer.person.as_ref());
                person.map_or_else(|| "Someone".to_owned(), |person| person.name.clone())
            }),
            held: self.held.clone(),
            // (To the minute: a count that ran would be news every tick.)
            files_wait: (self.files_wait)
                .map(|over| over.saturating_duration_since(Instant::now()).as_secs().div_ceil(60) * 60),
            recording_for_others: self.renditions.making(),
        };
        if status != shared.status {
            shared.status = Together { revision: shared.status.revision + 1, ..status };
        }
    }

    fn run(mut self, stop: Arc<AtomicBool>) {
        let mut told = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            self.asks();
            if self.is_live() {
                // What arrives is taken in as it arrives; what was changed
                // here is said once a tick, which is what thins a drag.
                self.events();
                self.ask_again();
                self.tend_files();
                self.tend_renditions();
                if told.elapsed() >= TICK {
                    self.tell();
                    told = Instant::now();
                }
                self.show();
                self.net.wait(TICK.saturating_sub(told.elapsed()).max(Duration::from_millis(1)));
            } else {
                self.show();
                // Nothing to do until someone shares or joins.
                std::thread::park_timeout(Duration::from_millis(250));
            }
        }
        self.leave(CLOSED);
    }
}

/// A while as a person says it: "about 3 h 20 min", "about 12 min".
pub(crate) fn in_about(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60);
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("about {} min", minutes.max(1)),
        (hours, 0) => format!("about {hours} h"),
        (hours, minutes) => format!("about {hours} h {minutes} min"),
    }
}

fn here_of(presence: &Presence) -> Here {
    Here {
        person: presence.person.clone(),
        beat: presence.beat,
        playing: presence.playing,
        heard: Instant::now(),
        tracks: presence.tracks.clone(),
        clips: presence.clips.clone(),
        notes: presence.notes.clone(),
        holding: presence.holding.clone(),
        lacks: presence.lacks.clone(),
    }
}

fn presence_of(here: &Here) -> Presence {
    Presence {
        person: here.person.clone(),
        // (Passed on as heard: whoever hears it from the person has a
        // later one.)
        seq: 0,
        beat: here.beat,
        playing: here.playing,
        tracks: here.tracks.clone(),
        clips: here.clips.clone(),
        notes: here.notes.clone(),
        holding: here.holding.clone(),
        lacks: here.lacks.clone(),
    }
}

impl Sharing {
    pub fn new(
        session: SharedSession,
        name: Option<String>,
        received: PathBuf,
        for_the_run: bool,
        media: Arc<trill_media::MediaCache>,
        unasked: u64,
    ) -> Self {
        let name = name.filter(|name| !name.trim().is_empty()).unwrap_or_else(name_on_this_machine);
        let me = Person { id: lot(), name, colour: 0 };
        let status = Together {
            me,
            invitation: None,
            joining: false,
            connected: 0,
            people: Vec::new(),
            revision: 0,
            said: (0, 0),
            meeting: None,
            away: Vec::new(),
            files: Vec::new(),
            held: Vec::new(),
            files_wait: None,
            recording_for_others: None,
        };
        let shared =
            Shared { status, mine: Mine::default(), said: Vec::new(), last_said: 0, asks: Vec::new(), shared: None };
        Self {
            shared: Arc::new(Mutex::new(shared)),
            stop: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
            session,
            received,
            for_the_run,
            media,
            unasked,
        }
    }

    /// Hands the thread something to do, starting it if this is the first.
    fn ask(&self, ask: Ask) -> Result<(), String> {
        self.shared.lock().asks.push(ask);
        let mut thread = self.thread.lock();
        match &*thread {
            Some(running) => running.thread().unpark(),
            None => {
                let net = Net::new();
                let bounds = trill_together::files::Bounds { unasked: self.unasked, ..Default::default() };
                let files = Files::new(net.pieces(), self.media.clone(), bounds, trill_fetch::disk_free)
                    .map_err(|error| format!("no thread could be started for the song's files: {error}"))?;
                let driver = Driver {
                    session: self.session.clone(),
                    shared: self.shared.clone(),
                    net,
                    peers: HashMap::new(),
                    people: Vec::new(),
                    epoch: None,
                    joining: Vec::new(),
                    again: Vec::new(),
                    were_away: HashSet::new(),
                    through: None,
                    presence_seq: 0,
                    presence_said: None,
                    files,
                    held: Vec::new(),
                    allowance: None,
                    files_wait: None,
                    renditions: Renditions::new(),
                };
                let stop = self.stop.clone();
                let started = std::thread::Builder::new().name("trill-together".into()).spawn(move || driver.run(stop));
                *thread =
                    Some(started.map_err(|error| format!("no thread could be started to work together: {error}"))?);
            }
        }
        Ok(())
    }
}

impl Drop for Sharing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.lock().take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        // A program that keeps nothing keeps no file it was handed.
        if self.for_the_run {
            let _ = std::fs::remove_dir_all(&self.received);
        }
    }
}

impl Runtime {
    /// Shares the open song: starts listening for the programs of people
    /// who are handed the invitation, and returns it as text. On this
    /// machine only, or on the networks the machine is on (the first time,
    /// the system's firewall asks whether that is allowed). `port` 0 is
    /// whichever the system has free.
    ///
    /// The song becomes one that merges: it is saved with what that
    /// takes, and can be closed, worked on alone and merged again. A
    /// program that is sharing already answers with the invitation it
    /// has.
    pub fn share(&self, reach: Reach, port: u16) -> Result<String, String> {
        self.share_as(Ask::Share(reach, port))
    }

    /// Shares the open song through the meeting point the settings name
    /// ([`Runtime::set_meeting_point`]), and returns the invitation, which
    /// carries the meeting point's address: whoever is handed it sets
    /// nothing up. This program listens for nothing; it connects outward,
    /// as whoever joins does, so no firewall asks and no router has to let
    /// anything in.
    ///
    /// The invitation is there at once. Whether the meeting is open at
    /// the meeting point follows in [`Together::meeting`]; where the
    /// meeting point does not take the key, sharing ends in a sentence
    /// ([`Runtime::said_together`]).
    pub fn share_through_meeting_point(&self) -> Result<String, String> {
        let Some(set) = self.settings().together.meeting_point else {
            return Err("no meeting point is set: its address and the key its keeper gave are set where the song \
                        is shared in the window (File, Share this song), once"
                .to_owned());
        };
        let point = MeetingPoint::parse(&set.address, &set.key)?;
        self.share_as(Ask::ShareThrough(point))
    }

    /// The meeting point the settings name, as a front end shows it: its
    /// address, and whether a key is given. The key itself is said to
    /// nobody.
    pub fn meeting_point(&self) -> Option<MeetingPointSet> {
        let set = self.settings().together.meeting_point?;
        Some(MeetingPointSet { address: set.address, has_key: !set.key.is_empty() })
    }

    /// Sets the meeting point this program shares through: its address as
    /// its keeper wrote it, and the key the keeper gave. `None` for the
    /// key keeps the one that is set (the address alone is changed). Both
    /// are kept in the settings and nowhere else.
    pub fn set_meeting_point(&self, address: &str, key: Option<&str>) -> Result<(), String> {
        let kept = self.settings().together.meeting_point.map(|set| set.key);
        let Some(key) = key.map(str::to_owned).or(kept).filter(|key| !key.trim().is_empty()) else {
            return Err("a meeting point takes the key its keeper gave: ten groups of four letters".to_owned());
        };
        // Read as it will be when it is shared through, and kept as its
        // keeper wrote it.
        let point = MeetingPoint::parse(address, &key)?;
        let set = trill_library::MeetingPointSettings { address: point.shown(), key: point.key.to_string() };
        self.update_settings(|settings| settings.together.meeting_point = Some(set)).map_err(|error| error.to_string())
    }

    /// Reads a meeting point as its keeper wrote it, without keeping
    /// it: the address as it will be shown, or the sentence that says
    /// what is wrong with it. `None` for the key reads the address alone
    /// (for where the key that is kept stays).
    pub fn read_meeting_point(address: &str, key: Option<&str>) -> Result<String, String> {
        // (A key of nothing but zeros is spelt right, and is nobody's.)
        let spelt_right = "0000-0000-0000-0000-0000-0000-0000-0000-0000-0000";
        Ok(MeetingPoint::parse(address, key.unwrap_or(spelt_right))?.shown())
    }

    /// Forgets the meeting point and its key.
    pub fn forget_meeting_point(&self) -> Result<(), String> {
        self.update_settings(|settings| settings.together.meeting_point = None).map_err(|error| error.to_string())
    }

    fn share_as(&self, ask: Ask) -> Result<String, String> {
        self.sharing.shared.lock().shared = None;
        self.sharing.ask(ask)?;
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(came) = self.sharing.shared.lock().shared.take() {
                return came;
            }
            if Instant::now() > until {
                return Err("sharing did not start in time".to_owned());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Stops listening: the invitation is no longer one. Whoever is
    /// connected stays.
    pub fn stop_sharing(&self) {
        let _ = self.sharing.ask(Ask::StopSharing);
    }

    /// Joins the song of whoever handed out `invitation`. If this program
    /// has the same song open (the two worked in it together before), both
    /// are brought up to what the other did meanwhile. Otherwise the
    /// song that is open is replaced by theirs: one with changes that are
    /// not saved only with `replace`, and without it the join is refused
    /// in a sentence ([`Runtime::said_together`]).
    ///
    /// Returns once the invitation is read; whether a connection comes
    /// of it shows in [`Runtime::together`].
    pub fn join(&self, invitation: &str, replace: bool) -> Result<(), String> {
        let invitation = Invitation::parse(invitation)?;
        self.sharing.ask(Ask::Join(invitation, replace))
    }

    /// The user's word that however much of the song's audio may be on
    /// its way at once: the files that waited for it arrive
    /// ([`OnItsWay::waits`]). It holds for as long as the program runs.
    pub fn let_files_arrive(&self) {
        let _ = self.sharing.ask(Ask::LetFilesArrive);
    }

    /// Leaves: every connection is ended and sharing stops. The song
    /// stays as it is, and stays one that can be merged with the others'
    /// again.
    pub fn leave(&self) {
        let _ = self.sharing.ask(Ask::Leave);
    }

    /// Who is here and how this program stands with them. Cheap: a copy.
    pub fn together(&self) -> Together {
        self.sharing.shared.lock().status.clone()
    }

    /// The sentences said since the one numbered `after` (what joined,
    /// who left and why, what was refused), each with its number.
    pub fn said_together(&self, after: u64) -> Vec<(u64, String)> {
        self.sharing.shared.lock().said.iter().filter(|(number, _)| *number > after).cloned().collect()
    }

    /// Says what this program's own person holds, for the others to see.
    pub fn holding(&self, mine: Mine) {
        let mut shared = self.sharing.shared.lock();
        if shared.mine != mine {
            shared.mine = mine;
        }
    }

    /// The name this program's person goes by among the others.
    pub fn set_name_together(&self, name: &str) {
        let name: String = name.trim().chars().take(trill_together::MAX_NAME / 4).collect();
        if !name.is_empty() {
            let mut shared = self.sharing.shared.lock();
            shared.status.me.name = name;
            shared.status.revision += 1;
        }
    }

    /// Waits until the programs this one is connected to have what was
    /// changed here, as far as that can be told from here: until nothing
    /// waits to be said, for `timeout` at the most. For tests and tools.
    #[doc(hidden)]
    pub fn settle_together(&self, timeout: Duration) {
        let until = Instant::now() + timeout;
        // Two looks of the thread: one to take, one to say.
        std::thread::sleep(TICK * 3);
        while Instant::now() < until && self.sharing.shared.lock().status.joining {
            std::thread::sleep(TICK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everyone_works_out_the_same_colour_for_each_person() {
        let people = [9u64, 17, 1, 25];
        let given = colours(&people);
        // 9, 17 and 25 all fall on colour 1: the lowest keeps it and the
        // others take the next that are free.
        assert_eq!((given[&1], given[&9], given[&17], given[&25]), (1, 2, 3, 4));
        let mut shuffled = people;
        shuffled.reverse();
        assert_eq!(colours(&shuffled), given, "in whatever order they are named");
        // More people than colours share, and nobody is without one.
        let many: Vec<u64> = (0..20).collect();
        assert!(colours(&many).values().all(|colour| *colour < COLOURS));
    }

    #[test]
    fn of_the_changes_a_drag_made_between_two_looks_the_last_is_said() {
        use trill_core::merge::{Change, Op, Stamp, TrackOp, TrackSet};
        let volume = |clock: u64, to: f32| Said {
            change: Change {
                stamp: Stamp(clock, 5),
                ops: vec![Op::Track(TrackId(1), TrackOp::Set(TrackSet::Volume(to)))],
            },
            knew: None,
        };
        let pan = Said {
            change: Change { stamp: Stamp(2, 5), ops: vec![Op::Track(TrackId(1), TrackOp::Set(TrackSet::Pan(0.5)))] },
            knew: None,
        };
        let said = thinned(vec![volume(1, -1.0), pan.clone(), volume(3, -2.0), volume(4, -3.0)]);
        assert_eq!(said, [pan, volume(4, -3.0)]);
    }
}
