//! The meeting point: what listens, what it holds, and what it passes.
//!
//! Every connection has a thread of its own, which reads with a time
//! limit and into room of a fixed size: nothing is asked of memory by
//! what a stranger says, and nobody is waited for longer than the limits
//! say. A meeting that is held is a record and its holder's connection. A
//! person in it is a *pipe*: their connection and one of the holder's,
//! each read by its own thread and written to the other. The meeting
//! point reads nothing of what a pipe carries; it counts the bytes.
//!
//! What goes wrong on one connection ends that connection and is a line
//! in the log. What a connection's thread was to let go of, the chores
//! let go of too when its time is over (a knock, a key that was taken
//! away): nothing is held for good because a thread did not get to it.

use crate::key::{Door, KeyId, Meeting, Question, Ticket};
use crate::keys::{Given, Keys, Reread};
use crate::limits::Limits;
use crate::said::{Hello, MAX_WORD, No, NoLine, Word, answered, ask, hear_line, say_line};
use crate::secure::{Broken, Room, Sayer, secure_in};
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Where the log goes: a line at a time, without its end.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// A connection, as every thread that has a part in it holds it: one
/// socket, and no second handle of the system's for it.
type Socket = Arc<TcpStream>;

/// How long an answer may take to be written to someone who is being
/// turned away or let in: it is a few bytes into an empty buffer.
const ANSWERING: Duration = Duration::from_secs(2);
/// How much of a pipe is read at once.
const PIECE: usize = 16 * 1024;
/// The stack of a connection's thread: the piece it reads into, and what
/// a handshake needs.
const STACK: usize = 256 * 1024;
/// How often a pipe that passes nothing looks whether it is over.
const LOOKING: Duration = Duration::from_secs(2);
/// For how long a meeting that its holder ended is remembered as ended,
/// and how many are.
const ENDED_FOR: Duration = Duration::from_secs(3600);
const ENDED_AT_MOST: usize = 1024;
/// After a refusal was said in the log, how long the same kind is only
/// counted.
const QUIET_FOR: Duration = Duration::from_secs(10);
/// How many addresses are remembered for how often they came.
const ADDRESSES_AT_MOST: usize = 8192;
/// How long a connection that was turned away at the door is kept after
/// it was told so, and how many are: closed at once, with what it said
/// unread, it may never read why.
const LEAVING_FOR: Duration = Duration::from_millis(300);
const LEAVING_AT_MOST: usize = 64;
const A_DAY: Duration = Duration::from_secs(24 * 3600);
/// How long after its time a knock is let go of by the chores, where the
/// thread that was to let go of it has not.
const KNOCK_GRACE: Duration = Duration::from_secs(5);
/// How often the chores ask whether the keys that hold something are
/// still given.
const KEYS_EVERY: Duration = Duration::from_secs(2);

/// A lock that a fault on another thread does not take away: what it
/// guards is counts and tables, each left whole by every hand.
fn lock<T>(guarded: &Mutex<T>) -> MutexGuard<'_, T> {
    guarded.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whose a connection is, for counting: an IPv4 address, or the network
/// of 2^64 addresses an IPv6 address is in, which is what one household
/// has.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum Origin {
    V4([u8; 4]),
    V6([u8; 8]),
}

impl Origin {
    fn of(address: IpAddr) -> Origin {
        match address.to_canonical() {
            IpAddr::V4(address) => Origin::V4(address.octets()),
            IpAddr::V6(address) => Origin::V6(address.octets()[..8].try_into().expect("eight bytes")),
        }
    }
}

/// How one address has been coming.
struct Seen {
    /// Its connections at the moment.
    now: usize,
    /// Since when the new ones are counted, and how many there were.
    since: Instant,
    new: u32,
}

/// What the meetings of one key have passed since `since`.
struct Day {
    /// In seconds since the meeting point began.
    since: AtomicU64,
    bytes: AtomicU64,
    /// Said in the log, for this day.
    said: AtomicBool,
}

/// A holder's connection.
struct Holder {
    connection: u64,
    say: Arc<Mutex<Sayer>>,
    socket: Socket,
}

/// A meeting that is held.
struct Held {
    key: KeyId,
    name: String,
    /// What whoever asks for it has to show that they have.
    door: Door,
    /// Its holder's connection, while there is one.
    holder: Option<Holder>,
    /// Since when there is none.
    away_since: Option<Instant>,
    /// Knocks its holder has been told of and has not taken up yet.
    knocking: usize,
}

/// Someone who asked for a meeting and waits for its holder.
struct Knock {
    meeting: Meeting,
    joiner: Socket,
    /// Where the holder's connection is handed to the thread that waits.
    to: SyncSender<(Socket, Arc<Pipe>)>,
    since: Instant,
}

/// A person in a meeting: their connection and the holder's to them.
struct Pipe {
    id: u64,
    meeting: Meeting,
    key: KeyId,
    name: String,
    joiner: Socket,
    holder: Socket,
    /// When something last passed, in milliseconds since the meeting
    /// point began.
    last: AtomicU64,
    passed: AtomicU64,
    day: Arc<Day>,
    over: AtomicBool,
}

/// Something that can happen as often as a stranger likes, and is
/// therefore said in the log once and then counted.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum Counted {
    /// Someone was turned away, and why.
    Refusal(No),
    /// Someone asked for a meeting that there is and could not answer
    /// the question. (It is told what whoever asks for one that there is
    /// not is told.)
    Answer,
    /// The system would not hand over a connection that arrived.
    Accept,
}

impl Counted {
    fn words(&self) -> String {
        match self {
            Counted::Refusal(no) => format!("turned away ({})", no.word()),
            Counted::Answer => "turned away (answer)".to_owned(),
            Counted::Accept => "a connection could not be taken".to_owned(),
        }
    }
}

/// How one of them is kept count of.
struct Refused {
    quiet_until: Instant,
    more: u64,
}

#[derive(Default)]
struct State {
    /// Every connection there is.
    live: HashMap<u64, Socket>,
    from: HashMap<Origin, Seen>,
    meetings: HashMap<Meeting, Held>,
    knocks: HashMap<Ticket, Knock>,
    pipes: HashMap<u64, Arc<Pipe>>,
    /// The meetings their holders ended, each with what whoever asks for
    /// it has to show: only who has the invitation is told that it is over.
    ended: VecDeque<(Meeting, Door, Instant)>,
    days: HashMap<KeyId, Arc<Day>>,
    refused: HashMap<Counted, Refused>,
    /// Connections that were turned away at the door, for a moment more.
    leaving: VecDeque<(TcpStream, Instant)>,
}

impl State {
    /// How many people are in a meeting or on their way into it, its
    /// holder among them.
    fn people(&self, meeting: Meeting) -> usize {
        let knocking = self.meetings.get(&meeting).map_or(0, |held| held.knocking);
        1 + knocking + self.pipes.values().filter(|pipe| pipe.meeting == meeting).count()
    }
}

struct Shared {
    limits: Limits,
    keys: Keys,
    log: Log,
    began: Instant,
    stop: AtomicBool,
    last: AtomicU64,
    /// When the chores last asked about the keys.
    keys_asked: Mutex<Instant>,
    state: Mutex<State>,
}

/// What a meeting point holds at a moment.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub connections: usize,
    pub meetings: usize,
    /// How many of the meetings have their holder connected.
    pub held: usize,
    /// How many people are in meetings, the holders aside.
    pub pipes: usize,
    pub knocks: usize,
    /// How many addresses are remembered.
    pub addresses: usize,
}

/// A meeting point that is running.
pub struct Server {
    shared: Arc<Shared>,
    addresses: Vec<SocketAddr>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Server {
    /// Starts a meeting point on `listeners`, which are bound already:
    /// it takes connections from the moment this returns.
    pub fn start(listeners: Vec<TcpListener>, keys: Keys, limits: Limits, log: Log) -> io::Result<Server> {
        let shared = Arc::new(Shared::new(keys, limits, log));
        let mut addresses = Vec::new();
        let mut threads = Vec::new();
        for listener in listeners {
            addresses.push(listener.local_addr()?);
            let shared = shared.clone();
            threads.push(std::thread::Builder::new().name("trill-meet-listen".into()).spawn(move || {
                listen(&shared, &listener);
            })?);
        }
        let tending = shared.clone();
        threads.push(std::thread::Builder::new().name("trill-meet-tend".into()).spawn(move || {
            while !tending.stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                tending.tend();
            }
        })?);
        Ok(Server { shared, addresses, threads })
    }

    /// Where it listens.
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    /// Its keys: for a test, which takes one away.
    pub fn keys(&self) -> &Keys {
        &self.shared.keys
    }

    /// What it holds right now.
    pub fn counts(&self) -> Counts {
        let state = lock(&self.shared.state);
        Counts {
            connections: state.live.len(),
            meetings: state.meetings.len(),
            held: state.meetings.values().filter(|held| held.holder.is_some()).count(),
            pipes: state.pipes.len(),
            knocks: state.knocks.len(),
            addresses: state.from.len(),
        }
    }

    /// Ends it: every connection is closed and nothing is listened for.
    pub fn stop(mut self) {
        self.end();
    }

    /// Runs until the program ends.
    pub fn run(mut self) -> ! {
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        std::process::exit(1)
    }

    fn end(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for socket in lock(&self.shared.state).live.values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        // Whoever waits for a connection is given one, to see that it is over.
        for address in &self.addresses {
            let to = match address.ip().is_unspecified() {
                true if address.is_ipv4() => SocketAddr::from(([127, 0, 0, 1], address.port())),
                true => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, address.port())),
                false => *address,
            };
            let _ = TcpStream::connect_timeout(&to, Duration::from_secs(1));
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        // And what came in meanwhile.
        let mut state = lock(&self.shared.state);
        for socket in state.live.values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        state.leaving.clear();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if !self.threads.is_empty() {
            self.end();
        }
    }
}

/// A connection's place among the ones there are: given back when its
/// thread is done, however that came about.
struct Seat<'a> {
    shared: &'a Shared,
    connection: u64,
    origin: Origin,
}

impl Drop for Seat<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        if let Some(socket) = state.live.remove(&self.connection) {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(seen) = state.from.get_mut(&self.origin) {
            seen.now = seen.now.saturating_sub(1);
        }
    }
}

fn listen(shared: &Arc<Shared>, listener: &TcpListener) {
    loop {
        let accepted = listener.accept();
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let (stream, address) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                // The system has no room for another connection at the
                // moment (its count of open files): said once, counted for
                // as long as it goes on, and tried again.
                shared.counted(Counted::Accept, || format!("a connection could not be taken: {error}"));
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let from = address.ip().to_canonical();
        let origin = Origin::of(from);
        let (connection, stream) = match shared.admit(stream, origin) {
            Ok(admitted) => admitted,
            Err((no, stream)) => {
                shared.refused(no, from);
                let _ = say_line(&stream, &format!("no {}", no.word()), Duration::from_millis(50));
                let _ = stream.shutdown(Shutdown::Write);
                let mut state = lock(&shared.state);
                state.leaving.push_back((stream, Instant::now()));
                while state.leaving.len() > LEAVING_AT_MOST {
                    state.leaving.pop_front();
                }
                continue;
            }
        };
        let serving = shared.clone();
        let spawned = std::thread::Builder::new().name("trill-meet-serve".into()).stack_size(STACK).spawn(move || {
            let seat = Seat { shared: &serving, connection, origin };
            let fault = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                serve(&serving, connection, (from, origin), &stream);
            }));
            if fault.is_err() {
                serving.say("a fault in serving one connection, which was closed for it");
            }
            drop(seat);
        });
        if let Err(error) = spawned {
            // (Its seat is given back here: no thread has it.)
            drop(Seat { shared, connection, origin });
            shared.say(&format!("no thread could be started for a connection: {error}"));
        }
    }
}

impl Shared {
    fn new(keys: Keys, limits: Limits, log: Log) -> Shared {
        Shared {
            limits,
            keys,
            log,
            began: Instant::now(),
            stop: AtomicBool::new(false),
            last: AtomicU64::new(0),
            keys_asked: Mutex::new(Instant::now()),
            state: Mutex::default(),
        }
    }

    fn say(&self, line: &str) {
        (self.log)(line);
    }

    fn now_ms(&self) -> u64 {
        self.began.elapsed().as_millis() as u64
    }

    /// Whether a connection that arrived is taken, and as which.
    fn admit(&self, stream: TcpStream, origin: Origin) -> Result<(u64, Socket), (No, TcpStream)> {
        let mut state = lock(&self.state);
        if state.live.len() >= self.limits.connections {
            return Err((No::Full, stream));
        }
        if state.from.len() >= ADDRESSES_AT_MOST && !state.from.contains_key(&origin) {
            // More addresses than are remembered: the ones that have
            // nothing open are forgotten, with how often they came.
            state.from.retain(|_, seen| seen.now > 0);
        }
        let now = Instant::now();
        let seen = state.from.entry(origin).or_insert(Seen { now: 0, since: now, new: 0 });
        if now.duration_since(seen.since) >= Duration::from_secs(60) {
            (seen.since, seen.new) = (now, 0);
        }
        if seen.now >= self.limits.from_one_address {
            return Err((No::Crowd, stream));
        }
        if seen.new >= self.limits.a_minute_from_one_address {
            return Err((No::Often, stream));
        }
        (seen.now, seen.new) = (seen.now + 1, seen.new + 1);
        let connection = self.last.fetch_add(1, Ordering::Relaxed) + 1;
        let socket = Arc::new(stream);
        state.live.insert(connection, socket.clone());
        Ok((connection, socket))
    }

    /// Someone was turned away: said in the log, where the same was not
    /// said a moment ago, and counted where it was. Whoever knocks a
    /// thousand times a second writes a line every ten seconds.
    fn refused(&self, no: No, from: IpAddr) {
        self.counted(Counted::Refusal(no), || {
            format!("turned away ({}), from {from}: {}", no.word(), turned_away(no, &self.limits))
        });
    }

    /// Says `line` in the log, where the same kind of thing was not said
    /// a moment ago, and counts it where it was: the count is said when
    /// the moment is over.
    fn counted(&self, what: Counted, line: impl FnOnce() -> String) {
        let now = Instant::now();
        let mut state = lock(&self.state);
        let refused = state.refused.entry(what).or_insert(Refused { quiet_until: now, more: 0 });
        if now < refused.quiet_until {
            refused.more += 1;
            return;
        }
        (refused.quiet_until, refused.more) = (now + QUIET_FOR, 0);
        drop(state);
        self.say(&line());
    }

    /// The day of a key, begun if it has none.
    fn day_of(&self, state: &mut State, key: KeyId) -> Arc<Day> {
        let began = self.began.elapsed().as_secs();
        let fresh =
            || Arc::new(Day { since: AtomicU64::new(began), bytes: AtomicU64::new(0), said: AtomicBool::new(false) });
        state.days.entry(key).or_insert_with(fresh).clone()
    }

    fn spent(&self, day: &Day) -> bool {
        day.bytes.load(Ordering::Relaxed) >= self.limits.bytes_a_day
    }

    /// The keys are asked, and what reading them again came to is said.
    fn given(&self, key: KeyId) -> Option<Given> {
        self.keys.find(key, &mut |reread| match reread {
            Reread::Read { names, wrong } => {
                let whose = if names.is_empty() { "nobody".to_owned() } else { names.join(", ") };
                self.say(&format!("the keys were read: {} given ({whose})", names.len()));
                if !wrong.is_empty() {
                    let lines: Vec<String> = wrong.iter().map(usize::to_string).collect();
                    self.say(&format!(
                        "the keys' file has lines that are no key and were passed over: {}",
                        lines.join(", ")
                    ));
                }
            }
            Reread::NotRead(why) => {
                self.say(&format!("the keys' file could not be read, and the keys are as they were: {why}"));
            }
        })
    }

    /// The chores: what has had its time is let go of.
    fn tend(&self) {
        let now = Instant::now();
        let mut lines = Vec::new();
        // Whether the keys are asked about this time, and which.
        let mut holding: Vec<(KeyId, String)> = Vec::new();
        let ask_keys = {
            let mut asked = lock(&self.keys_asked);
            let due = now.duration_since(*asked) >= KEYS_EVERY;
            if due {
                *asked = now;
            }
            due
        };
        {
            let mut state = lock(&self.state);
            while state.leaving.front().is_some_and(|(_, since)| now.duration_since(*since) >= LEAVING_FOR) {
                state.leaving.pop_front();
            }
            // A knock that nobody took up, and that the thread which
            // waited with it did not let go of in its time: let go of
            // here, with the seat it kept in its meeting.
            let old = self.limits.answer + KNOCK_GRACE;
            let left: Vec<Ticket> = (state.knocks.iter())
                .filter(|(_, knock)| now.duration_since(knock.since) >= old)
                .map(|(ticket, _)| *ticket)
                .collect();
            for ticket in left {
                let Some(knock) = state.knocks.remove(&ticket) else { continue };
                let _ = knock.joiner.shutdown(Shutdown::Both);
                if let Some(held) = state.meetings.get_mut(&knock.meeting) {
                    held.knocking = held.knocking.saturating_sub(1);
                    lines.push(format!(
                        "the meeting of {} ({}): a knock that nobody answered or gave up was let go of",
                        held.name,
                        knock.meeting.tag()
                    ));
                }
            }
            if ask_keys {
                // Every key that holds something: a meeting, whether or not
                // its holder is connected, or a person in one.
                for (key, name) in (state.meetings.values().map(|held| (held.key, &held.name)))
                    .chain(state.pipes.values().map(|pipe| (pipe.key, &pipe.name)))
                {
                    if !holding.iter().any(|(known, _)| *known == key) {
                        holding.push((key, name.clone()));
                    }
                }
            }
            // A holder that stayed away.
            let away = self.limits.away;
            let gone: Vec<Meeting> = (state.meetings.iter())
                .filter(|(_, held)| held.away_since.is_some_and(|since| now.duration_since(since) >= away))
                .map(|(meeting, _)| *meeting)
                .collect();
            for meeting in gone {
                if let Some(held) = state.meetings.remove(&meeting) {
                    lines.push(format!(
                        "the meeting of {} ({}) is forgotten: its holder has been away for {} s",
                        held.name,
                        meeting.tag(),
                        away.as_secs_f32()
                    ));
                }
            }
            while state.ended.front().is_some_and(|(_, _, at)| now.duration_since(*at) >= ENDED_FOR) {
                state.ended.pop_front();
            }
            state.from.retain(|_, seen| seen.now > 0 || now.duration_since(seen.since) < Duration::from_secs(60));
            // A day that is over.
            let began = self.began.elapsed().as_secs();
            for day in state.days.values() {
                if began.saturating_sub(day.since.load(Ordering::Relaxed)) >= A_DAY.as_secs() {
                    day.since.store(began, Ordering::Relaxed);
                    day.bytes.store(0, Ordering::Relaxed);
                    day.said.store(false, Ordering::Relaxed);
                }
            }
            // What was only counted.
            for (what, refused) in &mut state.refused {
                if now >= refused.quiet_until && refused.more > 0 {
                    lines.push(format!(
                        "{} {} more times in the {} s after",
                        what.words(),
                        refused.more,
                        QUIET_FOR.as_secs()
                    ));
                    refused.more = 0;
                }
            }
        }
        for line in lines {
            self.say(&line);
        }
        // A key that was taken away ends what it holds, whether or not
        // its holder is there to be told: asked here and not only when a
        // holder says that it is there.
        for (key, name) in holding {
            if self.given(key).is_none() {
                self.end_key(key, &name);
            }
        }
    }

    /// Ends a pipe, once: both its connections are closed, which ends the
    /// two threads that pass it.
    fn end_pipe(&self, pipe: &Pipe, why: &str) {
        if pipe.over.swap(true, Ordering::Relaxed) {
            return;
        }
        let _ = pipe.joiner.shutdown(Shutdown::Both);
        let _ = pipe.holder.shutdown(Shutdown::Both);
        let left = {
            let mut state = lock(&self.state);
            state.pipes.remove(&pipe.id);
            state.people(pipe.meeting)
        };
        self.say(&format!(
            "the meeting of {} ({}): someone is out of it, {why}, after {} kB; {left} in it",
            pipe.name,
            pipe.meeting.tag(),
            pipe.passed.load(Ordering::Relaxed) / 1000
        ));
    }

    /// Ends everything a key that was taken away holds: its meetings and
    /// the people in them.
    fn end_key(&self, key: KeyId, name: &str) {
        let (holders, pipes) = {
            let mut state = lock(&self.state);
            let meetings: Vec<Meeting> =
                state.meetings.iter().filter(|(_, held)| held.key == key).map(|(meeting, _)| *meeting).collect();
            let holders: Vec<Held> = meetings.iter().filter_map(|meeting| state.meetings.remove(meeting)).collect();
            let pipes: Vec<Arc<Pipe>> = state.pipes.values().filter(|pipe| pipe.key == key).cloned().collect();
            (holders, pipes)
        };
        if holders.is_empty() && pipes.is_empty() {
            return;
        }
        for held in holders {
            if let Some(holder) = held.holder {
                let _ = holder.socket.shutdown(Shutdown::Both);
            }
        }
        for pipe in pipes {
            self.end_pipe(&pipe, "the key was taken away");
        }
        self.say(&format!("the key of {name} was taken away: what it held is ended"));
    }

    /// A holder's connection is gone, or cannot be told anything: the
    /// meeting is its holder's to come back to, and whoever asks for it
    /// meanwhile hears at once that its holder is away.
    fn holder_lost(&self, meeting: Meeting, connection: u64) {
        let (socket, name) = {
            let mut state = lock(&self.state);
            let Some(held) = state.meetings.get_mut(&meeting) else { return };
            // (Unless another connection holds it by now.)
            let Some(holder) = held.holder.take_if(|holder| holder.connection == connection) else { return };
            held.away_since = Some(Instant::now());
            (holder.socket, held.name.clone())
        };
        let _ = socket.shutdown(Shutdown::Both);
        self.say(&format!(
            "the holder of the meeting of {name} ({}) is away: remembered for {} s",
            meeting.tag(),
            self.limits.away.as_secs_f32()
        ));
    }
}

/// A refusal in the meeting point's own words, with the limit that made it.
fn turned_away(no: No, limits: &Limits) -> String {
    match no {
        No::Full => format!("as many connections as are taken at once ({})", limits.connections),
        No::Crowd => {
            format!("as many connections from one address as are taken at once ({})", limits.from_one_address)
        }
        No::Often => {
            format!("more than {} new connections from one address in a minute", limits.a_minute_from_one_address)
        }
        No::Meetings => format!("as many meetings as are held at once ({})", limits.meetings),
        No::Yours => format!("as many meetings as one key may hold at once ({})", limits.meetings_of_one_key),
        No::Seats => format!("as many people as a meeting has ({})", limits.people),
        No::Late => format!("it did not say what it came for within {} s", limits.hello.as_secs_f32()),
        No::Slow => {
            format!("the meeting's holder did not take the knock up within {} s", limits.answer.as_secs_f32())
        }
        No::Spent => format!("the key has passed its {} MB of the day", limits.bytes_a_day >> 20),
        No::What => "what it said is no first line, or no word that belongs where it was said".to_owned(),
        No::Version => "it speaks another version".to_owned(),
        No::Key => "the key it named is none that is given, or it has not got it".to_owned(),
        No::Taken => "the meeting it would hold is held under another key".to_owned(),
        No::Unknown => "no meeting goes by what it asked for".to_owned(),
        No::Away => "the meeting's holder is away".to_owned(),
        No::Closed => "the meeting was ended by its holder".to_owned(),
        No::Ticket => "no knock goes by its ticket".to_owned(),
    }
}

/// One connection, from its first line to its end.
fn serve(shared: &Arc<Shared>, connection: u64, (from, origin): (IpAddr, Origin), stream: &Socket) {
    let _ = stream.set_nodelay(true);
    let end = Instant::now() + shared.limits.hello;
    let hello = match hear_line(stream, end) {
        Ok(line) => Hello::read(&line),
        Err(NoLine::Late) => Err(No::Late),
        Err(NoLine::NoLine) => Err(No::What),
        // Gone without a word: nothing to answer, and nothing to say of it.
        Err(NoLine::Gone(_)) => return,
    };
    let turned = match hello {
        Ok(Hello::Open(key)) => hold(shared, connection, from, stream, key, end),
        Ok(Hello::Join(meeting)) => join(shared, from, stream, meeting, end),
        Ok(Hello::Take(ticket)) => take(shared, origin, stream, ticket),
        Err(no) => Err(no),
    };
    if let Err(no) = turned {
        shared.refused(no, from);
        let _ = say_line(stream, &format!("no {}", no.word()), ANSWERING);
    }
}

/// Whether a read ended because its time was over (which one system
/// says one way and another the other).
fn timed_out(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}

/// Says one of the few words of a holder's connection.
fn word(say: &Mutex<Sayer>, word: Word) -> bool {
    lock(say).say(word.text().as_bytes()).is_ok()
}

/// Someone who says they have a key and would hold a meeting. `Err` is
/// what the first line is answered with; from the handshake on, what
/// there is to say is said over the connection it makes.
fn hold(
    shared: &Arc<Shared>,
    connection: u64,
    from: IpAddr,
    stream: &Socket,
    key: KeyId,
    end: Instant,
) -> Result<(), No> {
    let given = shared.given(key);
    // Whoever names a key is answered the same, whether there is such a
    // key or not, and gets no further without it either way: a key that
    // is none is held against something nobody has.
    let proof = match &given {
        Some(given) => given.proof,
        None => {
            let mut nobodys = [0u8; 32];
            getrandom::fill(&mut nobodys).map_err(|_| No::Key)?;
            nobodys
        }
    };
    if say_line(stream, "ok", ANSWERING).is_err() {
        return Ok(());
    }
    let left = end.saturating_duration_since(Instant::now());
    let (say, mut hear, given) = match (secure_in(stream.clone(), &proof, false, left, Room::SMALL), given) {
        (Ok((say, hear)), Some(given)) => (say, hear, given),
        (secured, _) => {
            match secured {
                // It has not got the key, and was told so by the
                // handshake: an empty message where this side's part of it
                // would be.
                Ok(_) | Err(Broken::NotTrusted) => shared.refused(No::Key, from),
                Err(Broken::Io(error)) if timed_out(&error) => shared.refused(No::Late, from),
                Err(Broken::TooMuch(_)) => shared.refused(No::What, from),
                Err(Broken::Io(_)) => {}
            }
            return Ok(());
        }
    };
    // (What is said to a holder is a few words into an empty buffer:
    // one that does not take them in a moment is gone.)
    let _ = say.within(ANSWERING);
    let say = Arc::new(Mutex::new(say));
    // Which meeting, within what is left of its time.
    let _ = hear.wait_at_most(Some(end.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))));
    let (meeting, door) = match hear.hear(MAX_WORD).map(|heard| Word::read(&heard)) {
        Ok(Some(Word::Open(meeting, door))) => (meeting, door),
        other => {
            let no = match other {
                Err(Broken::Io(error)) if timed_out(&error) => No::Late,
                // Gone: there is nobody to say anything to.
                Err(Broken::Io(_)) => return Ok(()),
                _ => No::What,
            };
            shared.refused(no, from);
            word(&say, Word::No(no));
            return Ok(());
        }
    };
    let holder = Holder { connection, say: say.clone(), socket: stream.clone() };
    let opened = {
        let mut state = lock(&shared.state);
        let day = shared.day_of(&mut state, key);
        let of_key = state.meetings.values().filter(|held| held.key == key).count();
        let meetings = state.meetings.len();
        match state.meetings.get_mut(&meeting) {
            _ if shared.spent(&day) => Err(No::Spent),
            Some(held) if held.key != key => Err(No::Taken),
            // Held already, by the same key: its holder is back, or has
            // connected again before the old connection was seen to be gone.
            Some(held) => {
                if let Some(before) = held.holder.replace(holder) {
                    let _ = before.socket.shutdown(Shutdown::Both);
                }
                (held.away_since, held.door) = (None, door);
                Ok(false)
            }
            None if meetings >= shared.limits.meetings => Err(No::Meetings),
            None if of_key >= shared.limits.meetings_of_one_key => Err(No::Yours),
            None => {
                state.ended.retain(|(ended, ..)| *ended != meeting);
                let name = given.name.clone();
                let held = Held { key, name, door, holder: Some(holder), away_since: None, knocking: 0 };
                state.meetings.insert(meeting, held);
                Ok(true)
            }
        }
    };
    let tag = meeting.tag();
    match opened {
        Err(no) => {
            shared.refused(no, from);
            word(&say, Word::No(no));
            return Ok(());
        }
        Ok(true) => shared.say(&format!("{} holds a meeting ({tag})", given.name)),
        Ok(false) => shared.say(&format!("{} holds the meeting ({tag}) again", given.name)),
    }
    // From here it says that it is there, now and then, and is told of
    // whoever knocks (by the threads that serve them).
    let _ = hear.wait_at_most(Some(shared.limits.silent));
    let mut ended = false;
    if word(&say, Word::Ok) {
        loop {
            match hear.hear(MAX_WORD).map(|heard| Word::read(&heard)) {
                Ok(Some(Word::Ping)) => {
                    if shared.given(key).is_none() {
                        word(&say, Word::No(No::Key));
                        shared.end_key(key, &given.name);
                        return Ok(());
                    }
                    // A key that has passed its bytes of the day is told so
                    // in place of the answer, and holds its meeting on.
                    let day = shared.day_of(&mut lock(&shared.state), key);
                    // With the answer, what the key may still pass today
                    // and how long the day goes on: its holder leaves what
                    // is left for what matters most to it.
                    let left = shared.limits.bytes_a_day.saturating_sub(day.bytes.load(Ordering::Relaxed));
                    let over = shared.began.elapsed().as_secs().saturating_sub(day.since.load(Ordering::Relaxed));
                    let pong = Word::Pong { left, seconds: A_DAY.as_secs().saturating_sub(over) };
                    if !word(&say, if shared.spent(&day) { Word::No(No::Spent) } else { pong }) {
                        break;
                    }
                }
                Ok(Some(Word::Close)) => {
                    ended = true;
                    break;
                }
                Ok(_) => {
                    word(&say, Word::No(No::What));
                    break;
                }
                Err(_) => break,
            }
        }
    }
    if !ended {
        shared.holder_lost(meeting, connection);
        return Ok(());
    }
    let mut state = lock(&shared.state);
    // (Unless another connection holds it by now.)
    let mine = |held: &Held| held.holder.as_ref().is_some_and(|holder| holder.connection == connection);
    if !state.meetings.get(&meeting).is_some_and(mine) {
        return Ok(());
    }
    if let Some(held) = state.meetings.remove(&meeting) {
        state.ended.push_back((meeting, held.door, Instant::now()));
    }
    while state.ended.len() > ENDED_AT_MOST {
        state.ended.pop_front();
    }
    drop(state);
    shared.say(&format!("the meeting of {} ({tag}) is over: its holder ended it", given.name));
    Ok(())
}

/// Someone who asks for a meeting. Whether they have its invitation is
/// asked first: only who has is told anything of the meeting, and only
/// for them is its holder told of a knock.
fn join(shared: &Arc<Shared>, from: IpAddr, stream: &Socket, meeting: Meeting, end: Instant) -> Result<(), No> {
    // The question, a number by lot, and the answer to it: within the
    // time anyone has to say what they came for.
    let question = Question::new().map_err(|_| No::Full)?;
    if say_line(stream, &ask(&question), ANSWERING).is_err() {
        return Ok(());
    }
    let answer = match hear_line(stream, end) {
        Ok(line) => answered(&line).ok_or(No::What)?,
        Err(NoLine::Late) => return Err(No::Late),
        Err(NoLine::NoLine) => return Err(No::What),
        Err(NoLine::Gone(_)) => return Ok(()),
    };
    let door = {
        let state = lock(&shared.state);
        let held = state.meetings.get(&meeting).map(|held| held.door);
        held.or_else(|| state.ended.iter().find(|(ended, ..)| *ended == meeting).map(|(_, door, _)| *door))
    };
    // Where there is no such meeting the answer is held against a door
    // nobody has, and whoever asked is told the same as someone who asked
    // for one that there is and answered wrong: that there is none.
    let right = match door {
        Some(door) => door.answer(&question).is(&answer),
        None => {
            let nobodys = Door::nobodys().map_err(|_| No::Full)?;
            std::hint::black_box(nobodys.answer(&question).is(&answer));
            false
        }
    };
    if !right && door.is_some() {
        shared.counted(Counted::Answer, || {
            format!(
                "turned away (answer), from {from}: it asked for a meeting that there is and could not say what its \
                 invitation comes to; it was told that there is none"
            )
        });
        let _ = say_line(stream, &format!("no {}", No::Unknown.word()), ANSWERING);
        return Ok(());
    }
    if !right {
        return Err(No::Unknown);
    }
    let ticket = Ticket::new().map_err(|_| No::Full)?;
    let (to, taken) = sync_channel(1);
    let (say, holder) = {
        let mut state = lock(&shared.state);
        let people = state.people(meeting);
        let Some(held) = state.meetings.get(&meeting) else {
            let ended = state.ended.iter().any(|(ended, ..)| *ended == meeting);
            return Err(if ended { No::Closed } else { No::Unknown });
        };
        let Some(holder) = &held.holder else { return Err(No::Away) };
        let (say, holder, key) = (holder.say.clone(), holder.connection, held.key);
        if people >= shared.limits.people {
            return Err(No::Seats);
        }
        let day = shared.day_of(&mut state, key);
        if shared.spent(&day) {
            return Err(No::Spent);
        }
        state.meetings.get_mut(&meeting).expect("it was there").knocking += 1;
        state.knocks.insert(ticket, Knock { meeting, joiner: stream.clone(), to, since: Instant::now() });
        (say, holder)
    };
    // The knock is the holder's or the time's to answer; whichever takes
    // it out of the table has it.
    let give_up = |shared: &Shared| -> bool {
        let mut state = lock(&shared.state);
        let mine = state.knocks.remove(&ticket).is_some();
        if mine && let Some(held) = state.meetings.get_mut(&meeting) {
            held.knocking = held.knocking.saturating_sub(1);
        }
        mine
    };
    let wait = |taken: &Receiver<(Socket, Arc<Pipe>)>| match taken.recv_timeout(shared.limits.answer) {
        Ok(taken) => Some(taken),
        // Not in time, unless the holder took it this very moment.
        Err(_) if give_up(shared) => None,
        Err(_) => taken.recv_timeout(ANSWERING).ok(),
    };
    if !word(&say, Word::Knock(ticket)) {
        // Its holder cannot be told. Its connection is closed for it, so
        // that the meeting is away from this moment: whoever asks next
        // hears so at once, and does not wait in turn for a holder that
        // takes nothing.
        give_up(shared);
        shared.holder_lost(meeting, holder);
        return Err(No::Away);
    }
    let Some((holder, pipe)) = wait(&taken) else { return Err(No::Slow) };
    if say_line(stream, "ok", ANSWERING).is_err() {
        shared.end_pipe(&pipe, "gone as it was let in");
        return Ok(());
    }
    pass(shared, &pipe, stream, &holder);
    Ok(())
}

/// The holder of a meeting, taking a knock up: this connection is the
/// other end of the pipe to whoever knocked.
fn take(shared: &Arc<Shared>, origin: Origin, stream: &Socket, ticket: Ticket) -> Result<(), No> {
    let (knock, pipe, people) = {
        let mut state = lock(&shared.state);
        let Some(knock) = state.knocks.remove(&ticket) else { return Err(No::Ticket) };
        // A knock that is taken up was the meeting point's to ask for: it
        // does not count against how often its holder's address may come.
        // (Or whoever knocks often enough would have the holder turned
        // away for answering.)
        if let Some(seen) = state.from.get_mut(&origin) {
            seen.new = seen.new.saturating_sub(1);
        }
        let meeting = knock.meeting;
        let Some(held) = state.meetings.get_mut(&meeting) else { return Err(No::Closed) };
        held.knocking = held.knocking.saturating_sub(1);
        let (key, name) = (held.key, held.name.clone());
        let day = shared.day_of(&mut state, key);
        let pipe = Arc::new(Pipe {
            id: shared.last.fetch_add(1, Ordering::Relaxed) + 1,
            meeting,
            key,
            name,
            joiner: knock.joiner.clone(),
            holder: stream.clone(),
            last: AtomicU64::new(shared.now_ms()),
            passed: AtomicU64::new(0),
            day,
            over: AtomicBool::new(false),
        });
        state.pipes.insert(pipe.id, pipe.clone());
        let people = state.people(meeting);
        (knock, pipe, people)
    };
    if say_line(stream, "ok", ANSWERING).is_err() {
        shared.end_pipe(&pipe, "its holder was gone as it took the knock up");
        return Ok(());
    }
    shared.say(&format!("the meeting of {} ({}): someone is in it; {people} in it", pipe.name, pipe.meeting.tag()));
    // Whoever knocked is told by the thread that serves them, which passes
    // their half; if it has gone, the pipe ends with it.
    if knock.to.send((stream.clone(), pipe.clone())).is_err() {
        shared.end_pipe(&pipe, "whoever knocked was gone");
        return Ok(());
    }
    pass(shared, &pipe, stream, &knock.joiner);
    Ok(())
}

/// Passes what `from` says to `to`, for as long as the pipe is.
fn pass(shared: &Shared, pipe: &Pipe, from: &TcpStream, to: &TcpStream) {
    let silent = shared.limits.silent;
    let _ = from.set_read_timeout(Some(LOOKING.min(silent).max(Duration::from_millis(1))));
    let _ = to.set_write_timeout(Some(silent));
    let (mut from, mut to) = (from, to);
    let mut piece = [0u8; PIECE];
    let why = loop {
        if pipe.over.load(Ordering::Relaxed) {
            return;
        }
        if shared.stop.load(Ordering::Relaxed) {
            break "the meeting point is ending";
        }
        match from.read(&mut piece) {
            Ok(0) => break "one side closed it",
            Ok(count) => {
                let passed = pipe.day.bytes.fetch_add(count as u64, Ordering::Relaxed) + count as u64;
                if passed > shared.limits.bytes_a_day {
                    if !pipe.day.said.swap(true, Ordering::Relaxed) {
                        shared.say(&format!(
                            "the key of {} has passed its {} MB of the day: its meetings pass no more until the day \
                             is over",
                            pipe.name,
                            shared.limits.bytes_a_day >> 20
                        ));
                        // Whoever holds a meeting with the key is told now,
                        // and not only when it next says that it is there:
                        // the people in it are out of it this moment.
                        let holders: Vec<Arc<Mutex<Sayer>>> = (lock(&shared.state).meetings.values())
                            .filter(|held| held.key == pipe.key)
                            .filter_map(|held| held.holder.as_ref().map(|holder| holder.say.clone()))
                            .collect();
                        for say in holders {
                            word(&say, Word::No(No::Spent));
                        }
                    }
                    break "the key's bytes of the day are passed";
                }
                if to.write_all(&piece[..count]).is_err() {
                    break "one side took nothing more";
                }
                pipe.passed.fetch_add(count as u64, Ordering::Relaxed);
                pipe.last.store(shared.now_ms(), Ordering::Relaxed);
            }
            Err(error) if timed_out(&error) => {
                let quiet = shared.now_ms().saturating_sub(pipe.last.load(Ordering::Relaxed));
                if quiet >= silent.as_millis() as u64 {
                    break "nothing passed for as long as a silent connection is kept";
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break "one side closed it",
        }
    };
    shared.end_pipe(pipe, why);
}

/// What no connection from outside can bring about on demand (a thread
/// that ends where it should not, a holder that takes nothing), set up
/// by hand in what the meeting point holds. (Four connections.)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::Key;
    use crate::said::{answering, asked};

    type Lines = Arc<Mutex<Vec<String>>>;

    fn shared(limits: Limits) -> (Arc<Shared>, Lines) {
        let lines = Lines::default();
        let log = lines.clone();
        let said: Log = Arc::new(move |line: &str| log.lock().unwrap().push(line.to_owned()));
        (Arc::new(Shared::new(Keys::of(Vec::new()), limits, said)), lines)
    }

    /// The two ends of a connection on this machine.
    fn pair() -> (Socket, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let far = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        far.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        (Arc::new(listener.accept().unwrap().0), far)
    }

    fn held(key: &Key, door: Door, holder: Option<Holder>) -> Held {
        Held { key: key.id(), name: "Ada".to_owned(), door, holder, away_since: None, knocking: 0 }
    }

    #[test]
    fn a_knock_that_no_thread_let_go_of_is_let_go_of_by_the_chores_with_its_seat() {
        let (shared, lines) = shared(Limits { answer: Duration::from_millis(100), ..Limits::default() });
        let (key, secret) = (Key::new().unwrap(), b"an invitation's secret");
        let (meeting, door) = (Meeting::of(secret), Door::of(secret));
        let (joiner, mut far) = pair();
        // A knock as the thread that serves whoever knocked leaves it, had
        // it ended there: in the table, with its seat in the meeting. One
        // is long past its time, and one has just been made.
        let (old, new) = (Ticket::new().unwrap(), Ticket::new().unwrap());
        let long_ago = Instant::now().checked_sub(Duration::from_secs(8)).expect("the machine is up that long");
        {
            let mut state = lock(&shared.state);
            state.meetings.insert(meeting, Held { knocking: 2, ..held(&key, door, None) });
            for (ticket, since) in [(old, long_ago), (new, Instant::now())] {
                let (to, _) = sync_channel(1);
                state.knocks.insert(ticket, Knock { meeting, joiner: joiner.clone(), to, since });
            }
            assert_eq!(state.people(meeting), 3);
        }
        shared.tend();
        let state = lock(&shared.state);
        assert!(!state.knocks.contains_key(&old) && state.knocks.contains_key(&new), "only the old one is let go of");
        assert_eq!((state.meetings[&meeting].knocking, state.people(meeting)), (1, 2), "and its seat is free");
        drop(state);
        let mut nothing = [0u8; 1];
        assert!(matches!(far.read(&mut nothing), Ok(0) | Err(_)), "whoever knocked is let go of with it");
        let log = lines.lock().unwrap().join("\n");
        assert!(log.contains("a knock that nobody answered or gave up was let go of"), "{log}");
    }

    #[test]
    fn a_holder_that_cannot_be_told_of_a_knock_is_away_at_once_for_whoever_asks_next() {
        let (shared, lines) = shared(Limits::default());
        let (key, secret) = (Key::new().unwrap(), b"an invitation's secret");
        let (meeting, door) = (Meeting::of(secret), Door::of(secret));
        // A holder's connection, made as one is, to which nothing more
        // can be said: as when its holder takes nothing and what waits
        // to be said to it has filled what the system holds for it.
        let (holders, far) = pair();
        let proof = key.proof();
        let within = Duration::from_secs(10);
        let their_end = std::thread::spawn(move || secure_in(far, &proof, true, within, Room::SMALL).unwrap());
        let (say, _hear) = secure_in(holders.clone(), &key.proof(), false, within, Room::SMALL).unwrap();
        let (_their_say, mut their_hear) = their_end.join().unwrap();
        holders.shutdown(Shutdown::Write).unwrap();
        let holder = Holder { connection: 7, say: Arc::new(Mutex::new(say)), socket: holders };
        lock(&shared.state).meetings.insert(meeting, held(&key, door, Some(holder)));

        // Someone with the invitation asks for the meeting.
        let asks = |shared: &Arc<Shared>| {
            let (joiner, far) = pair();
            let asking = std::thread::spawn(move || {
                let end = Instant::now() + Duration::from_secs(20);
                let Some(Ok(question)) = asked(&hear_line(&far, end).unwrap()) else { panic!("it is asked") };
                say_line(&far, &answering(&door.answer(&question)), Duration::from_secs(5)).unwrap();
                far
            });
            let began = Instant::now();
            let from = IpAddr::from([127, 0, 0, 1]);
            let turned = join(shared, from, &joiner, meeting, Instant::now() + Duration::from_secs(20));
            (turned, began.elapsed(), asking.join().unwrap())
        };
        let (turned, _, _first) = asks(&shared);
        assert_eq!(turned, Err(No::Away), "its holder could not be told");
        // The meeting is away from that moment, its holder's connection
        // closed, and no knock or seat is left of the one who asked.
        {
            let state = lock(&shared.state);
            let held = &state.meetings[&meeting];
            assert!(held.holder.is_none() && held.away_since.is_some() && held.knocking == 0);
            assert!(state.knocks.is_empty());
        }
        their_hear.wait_at_most(Some(Duration::from_secs(20))).unwrap();
        assert!(their_hear.hear(MAX_WORD).is_err(), "the holder's connection is closed for it");
        // Whoever asks next hears so at once: nothing is said to a holder
        // that takes nothing, and nobody waits in turn for it.
        let (turned, took, _second) = asks(&shared);
        assert_eq!(turned, Err(No::Away));
        assert!(took < Duration::from_secs(5), "after {took:?}");
        let log = lines.lock().unwrap().join("\n");
        assert_eq!(log.matches("the holder of the meeting of Ada").count(), 1, "{log}");
        assert!(log.contains("is away: remembered for 600 s"), "{log}");
    }

    #[test]
    fn what_can_happen_as_often_as_a_stranger_likes_is_said_once_and_then_counted() {
        let (shared, lines) = shared(Limits::default());
        // The system hands over no connection, again and again.
        for _ in 0..50 {
            shared.counted(Counted::Accept, || "a connection could not be taken: too many open files".to_owned());
        }
        assert_eq!(lines.lock().unwrap().len(), 1, "one line for all of them");
        // When the moment is over the count is said, once.
        lock(&shared.state).refused.get_mut(&Counted::Accept).unwrap().quiet_until = Instant::now();
        shared.tend();
        shared.tend();
        let log = lines.lock().unwrap().join("\n");
        assert_eq!(log.lines().count(), 2, "{log}");
        assert!(log.contains("a connection could not be taken 49 more times in the 10 s after"), "{log}");
    }
}
