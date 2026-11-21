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

use crate::{Runtime, SharedSession};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use trill_core::merge::{Key, Replica, Said};
use trill_core::{ClipId, TrackId};
use trill_together::{Event, Invitation, LinkId, Message, Net, PROTOCOL, Person, Presence, Reach};

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
/// What a program says as it leaves, and as it is closed.
const LEFT: &str = "left";
const CLOSED: &str = "the program was closed";

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
}

impl Together {
    /// Whether the song is being worked in with others, or offered to be.
    pub fn is_on(&self) -> bool {
        self.invitation.is_some() || self.joining || self.connected > 0
    }
}

/// What a front end asks of the thread.
enum Ask {
    Share(Reach, u16),
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
    person: Option<Person>,
    replica: Option<Replica>,
    /// Both hold the same song, and each has been handed what it lacked:
    /// from here on changes go both ways as they are made.
    synced: bool,
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
    /// Joins asked for that have not come to a connection yet, each with
    /// whether it may replace a song that is not saved.
    joining: Vec<bool>,
    presence_seq: u64,
    presence_said: Option<(Presence, Instant)>,
}

impl Driver {
    fn is_live(&self) -> bool {
        self.net.listening().is_some() || !self.peers.is_empty() || !self.joining.is_empty()
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
        self.epoch = None;
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
                Ask::StopSharing => self.net.stop_listening(),
                Ask::Join(invitation, replace) => {
                    let mut session = self.session.lock();
                    session.hear_changes(true);
                    self.epoch = Some(session.song_epoch());
                    drop(session);
                    self.joining.push(replace);
                    self.net.join(invitation);
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
                // Whoever came is in the song once they have it: said now
                // where both had it already, and else when it is handed
                // over.
                match (joined, reply.is_some()) {
                    (true, true) => self.say(format!("You are in the song with {name}.")),
                    (true, false) => {}
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
            Message::Bye { .. } => {}
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
                    let replace = joined && !self.joining.is_empty() && self.joining.remove(0);
                    let peer = Peer { joined, replace, person: None, replica: None, synced: false };
                    self.peers.insert(link, peer);
                    let hello = self.hello();
                    self.net.say(link, &hello);
                }
                Event::Heard { link, message } => self.heard(link, message),
                Event::Lost { link, why } => {
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
                    match peer.person {
                        Some(person) => match why.as_str() {
                            LEFT => self.say(format!("{} left.", person.name)),
                            CLOSED => self.say(format!("{} closed their Trill.", person.name)),
                            why => self.say(format!("{} is no longer here: {why}.", person.name)),
                        },
                        // A connection this program asked for, gone
                        // before the other had said who it is: the join
                        // came to nothing, and a failure is a sentence.
                        None if peer.joined => self.say(format!(
                            "Not joined: the connection ended before the other program had said who it is ({why}). \
                             It may not be a Trill, or it stopped sharing its song; ask for the invitation again."
                        )),
                        None => {}
                    }
                    self.shared.lock().status.revision += 1;
                }
                // Whether a meeting is held at a meeting point: nothing shares
                // through one yet.
                Event::Held | Event::Unheld { .. } => {}
                Event::Failed { why } => {
                    if !self.joining.is_empty() {
                        self.joining.remove(0);
                    }
                    self.say(format!("Not joined: {why}."));
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
            joining: !self.joining.is_empty(),
            connected: self.peers.len(),
            people,
            revision: shared.status.revision,
            said: self.net.said(),
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
    }
}

impl Sharing {
    pub fn new(session: SharedSession, name: Option<String>) -> Self {
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
        };
        let shared =
            Shared { status, mine: Mine::default(), said: Vec::new(), last_said: 0, asks: Vec::new(), shared: None };
        Self {
            shared: Arc::new(Mutex::new(shared)),
            stop: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
            session,
        }
    }

    /// Hands the thread something to do, starting it if this is the first.
    fn ask(&self, ask: Ask) -> Result<(), String> {
        self.shared.lock().asks.push(ask);
        let mut thread = self.thread.lock();
        match &*thread {
            Some(running) => running.thread().unpark(),
            None => {
                let driver = Driver {
                    session: self.session.clone(),
                    shared: self.shared.clone(),
                    net: Net::new(),
                    peers: HashMap::new(),
                    people: Vec::new(),
                    epoch: None,
                    joining: Vec::new(),
                    presence_seq: 0,
                    presence_said: None,
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
        self.sharing.shared.lock().shared = None;
        self.sharing.ask(Ask::Share(reach, port))?;
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
