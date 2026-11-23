//! The connections of one program: the invitation it listens under, the
//! programs it is connected to, and what arrives from them.
//!
//! Nothing here waits on anything of the song. Each connection has a
//! thread that hears and one that says; what is heard is put in a queue
//! that whoever drives the song empties ([`Net::events`]), and what is to
//! be said is put in a queue of the connection's own ([`Net::say`]). The
//! thread that drives the song therefore never touches the network, and a
//! program that is slow to hear holds nobody up but itself: one that
//! falls too far behind is let go of.
//!
//! **What is said of the song never waits for a file.** A connection has
//! two queues: what is said of the song and of who is here, and the bytes
//! of files ([`Message::Piece`]). The thread that says takes a piece only
//! when nothing else waits, and a piece is one frame: whatever is changed
//! in the song goes out behind one frame at the most, however large the
//! file that is crossing.

use crate::invite::Invitation;
use crate::point::MeetingPoint;
use crate::wire::{MAX_MESSAGE, Message};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{Shutdown, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use trill_meet::secure::{Broken, Hearer, Sayer, secure};
use trill_meet::{Door, Holding, Key, Meeting, No, Ticket, Turned};

/// Which connection.
pub type LinkId = u64;
/// Which asking for a connection ([`Net::join`]).
pub type JoinId = u64;

/// How much may wait to be said to one program before it is taken to
/// have stopped hearing.
const MAX_WAITING: usize = 512 << 20;
/// How many messages a program may say in [`WINDOW`] before it is taken
/// to be saying nothing anyone asked for: far more than a drag makes.
const MAX_IN_WINDOW: u32 = 4000;
const WINDOW: Duration = Duration::from_secs(5);
/// How long connecting to an address may take.
const CONNECTING: Duration = Duration::from_secs(8);
/// How many connections may be proving at once that they have the
/// invitation. One that has not is held for as long as that takes it (ten
/// seconds at the most), on a thread of its own: whoever opens many
/// without it gets no more threads than this.
const MAX_MEETING: usize = 16;
/// How long a meeting point that could not be reached, or was lost, is
/// left alone before it is tried again: this long the first time and
/// twice as long each time after, up to the longest.
const FIRST_PAUSE: Duration = Duration::from_secs(1);
const LONGEST_PAUSE: Duration = Duration::from_secs(20);
/// How often the thread that holds a meeting looks whether it is to stop.
const LOOKING: Duration = Duration::from_millis(100);
/// How long a connection that has nothing to say leaves it at that before
/// it says that it is there ([`Message::Still`]).
const STILL_EVERY: Duration = Duration::from_secs(2);
/// How long nothing at all may be heard over a connection before it is
/// taken to be lost on the way. The other side says something every
/// [`STILL_EVERY`] at the least, whatever its program is busy with, so
/// this is five of those gone missing. A line that goes down says nothing:
/// neither side is sent a word, and the side that only hears would wait
/// for as long as its system keeps the connection, which is minutes on
/// some. With this, each side has let go within seconds, and the one that
/// asked for the connection asks again.
const SILENT: Duration = Duration::from_secs(10);

/// How a program's connections say that they are there, and how long they
/// hear nothing before they are taken to be lost.
#[derive(Copy, Clone, Debug)]
struct Pulse {
    still_every: Duration,
    silent: Duration,
}

impl Default for Pulse {
    fn default() -> Self {
        Self { still_every: STILL_EVERY, silent: SILENT }
    }
}

/// Where to listen.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// On this machine only: other programs on it can connect, and
    /// nothing on the network can.
    ThisMachine,
    /// On every network the machine is on: the first time, the system's
    /// firewall asks whether that is allowed.
    Network,
}

/// Something that happened on the network.
#[derive(Debug)]
pub enum Event {
    /// A connection is there, proved and encrypted: nothing has been said
    /// over it yet. `joined` for one this program asked for, with which
    /// asking it was.
    Open {
        link: LinkId,
        joined: Option<JoinId>,
    },
    Heard {
        link: LinkId,
        message: Message,
    },
    /// A connection is gone, and why. `told`: one side ended it and said
    /// so (the other's last words, or this program's own). Otherwise it
    /// was lost on the way, and whoever asked for it may ask again.
    Lost {
        link: LinkId,
        why: String,
        told: bool,
    },
    /// Joining did not come to a connection. `for_good`: asking again by
    /// itself is no use (the invitation is another's or no longer one);
    /// otherwise what was in the way may be gone a moment later.
    Failed {
        join: JoinId,
        why: String,
        for_good: bool,
    },
    /// The meeting this program shares through is open at the meeting
    /// point: whoever has the invitation gets in.
    Held,
    /// It is not, and why. With `again` this program goes on trying by
    /// itself, and a [`Event::Held`] follows when it has it back; without,
    /// something has to be changed first (the key, the address) and
    /// nothing is listened for any more.
    Unheld {
        turned: Turned,
        again: bool,
    },
    /// What the meeting point says the key this program shares with may
    /// still pass today, in bytes, and when that day is over. Said when
    /// the meeting point says, every twenty seconds or so.
    Allowance {
        left: u64,
        over: Instant,
    },
}

/// One connection, as the program that drives the song holds it.
struct Link {
    queue: Arc<Queue>,
    /// The connection's one handle, to end it by.
    socket: Arc<TcpStream>,
}

impl Drop for Link {
    fn drop(&mut self) {
        // What waits to be said of the song is said first, where the
        // other side takes it; the thread that says ends then.
        self.queue.close();
    }
}

/// What waits to be said to one program: what is said of the song, and
/// behind all of it the bytes of files.
#[derive(Default)]
struct Queue {
    waiting: Mutex<Waiting>,
    more: Condvar,
}

#[derive(Default)]
struct Waiting {
    first: VecDeque<Arc<Vec<u8>>>,
    pieces: VecDeque<Arc<Vec<u8>>>,
    /// How many bytes of each wait.
    first_bytes: usize,
    piece_bytes: usize,
    closed: bool,
}

/// What a connection is to say next.
enum Next {
    Say(Arc<Vec<u8>>),
    /// Nothing waited for as long as was asked.
    Nothing,
    /// Nothing more, ever.
    Closed,
}

impl Queue {
    /// Puts `bytes` in the queue it belongs to, and returns how many bytes
    /// wait there now. `None` for a connection that says nothing more.
    fn put(&self, bytes: Arc<Vec<u8>>, piece: bool) -> Option<usize> {
        let mut waiting = self.waiting.lock().unwrap();
        if waiting.closed {
            return None;
        }
        let now = match piece {
            true => {
                waiting.piece_bytes += bytes.len();
                waiting.pieces.push_back(bytes);
                waiting.piece_bytes
            }
            false => {
                waiting.first_bytes += bytes.len();
                waiting.first.push_back(bytes);
                waiting.first_bytes
            }
        };
        self.more.notify_one();
        Some(now)
    }

    /// The next thing to say, waited for `at_most` so long: what is said
    /// of the song while any of it waits, and a piece of a file otherwise.
    /// [`Next::Closed`] once the connection is to end and nothing of the
    /// song waits (the pieces that do are let go of).
    fn next(&self, at_most: Duration) -> Next {
        let until = Instant::now() + at_most;
        let mut waiting = self.waiting.lock().unwrap();
        loop {
            if let Some(bytes) = waiting.first.pop_front() {
                waiting.first_bytes -= bytes.len();
                return Next::Say(bytes);
            }
            if waiting.closed {
                return Next::Closed;
            }
            if let Some(bytes) = waiting.pieces.pop_front() {
                waiting.piece_bytes -= bytes.len();
                return Next::Say(bytes);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Next::Nothing;
            }
            waiting = self.more.wait_timeout(waiting, left).unwrap().0;
        }
    }

    fn close(&self) {
        let mut waiting = self.waiting.lock().unwrap();
        waiting.closed = true;
        waiting.pieces.clear();
        waiting.piece_bytes = 0;
        self.more.notify_all();
    }

    /// Nothing more is said, not what waits either.
    fn end(&self) {
        let mut waiting = self.waiting.lock().unwrap();
        waiting.closed = true;
        (waiting.first_bytes, waiting.piece_bytes) = (0, 0);
        waiting.first.clear();
        waiting.pieces.clear();
        self.more.notify_all();
    }

    fn piece_bytes(&self) -> usize {
        self.waiting.lock().unwrap().piece_bytes
    }
}

/// A way to say bytes of files to the programs this one is connected to,
/// for a thread that reads files and is not the one that drives the song.
/// It can say nothing else.
#[derive(Clone)]
pub struct Pieces {
    links: Arc<Mutex<HashMap<LinkId, Link>>>,
    said: Arc<AtomicU64>,
}

impl Pieces {
    /// Queues a piece of a file for one program, behind everything that
    /// is said of the song. Returns how many bytes of files wait for that
    /// program now; `None` for a connection that is gone, and for
    /// anything that is no piece.
    pub fn say(&self, link: LinkId, piece: &Message) -> Option<usize> {
        if !piece.is_piece() {
            return None;
        }
        let bytes = Arc::new(piece.encode());
        let queue = self.links.lock().unwrap().get(&link)?.queue.clone();
        self.said.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        queue.put(bytes, true)
    }

    /// How many bytes of files wait to be said to one program.
    pub fn waiting(&self, link: LinkId) -> Option<usize> {
        Some(self.links.lock().unwrap().get(&link)?.queue.piece_bytes())
    }

    /// How many bytes of pieces have been said, to every program together.
    pub fn said(&self) -> u64 {
        self.said.load(Ordering::Relaxed)
    }
}

struct Listening {
    invitation: Invitation,
    stop: Arc<AtomicBool>,
    /// For a meeting held at a meeting point: where the thread that holds
    /// it says that it has ended it there.
    ended: Option<Receiver<()>>,
    /// For a program that listens itself: what whoever connects has to
    /// prove. It is the invitation's, and another when the invitation is
    /// ([`Net::listen_anew`]).
    proved_by: Option<Arc<Mutex<[u8; 32]>>>,
    /// Held by whoever lets a connection in that has proved the
    /// invitation, while it does, and by whoever ends the invitation or
    /// makes it another, while it does: so that once that is done, every
    /// connection the invitation before let in is among the connections
    /// there are, and none follows.
    gate: Arc<Mutex<()>>,
}

/// What the thread that holds a meeting at a meeting point has.
struct Hold {
    address: String,
    key: Key,
    meeting: Meeting,
    /// What whoever asks for the meeting has to show that they have.
    door: Door,
    /// What the two programs prove to each other: the invitation's.
    proved_by: [u8; 32],
    stop: Arc<AtomicBool>,
    /// How many knocks are being taken up and proved at the moment.
    proving: Arc<AtomicUsize>,
    /// See [`Listening::gate`].
    gate: Arc<Mutex<()>>,
    links: Arc<Mutex<HashMap<LinkId, Link>>>,
    last: Arc<AtomicU64>,
    happened: Sender<Event>,
    pulse: Pulse,
}

impl Hold {
    /// Holds the meeting open until it is told to stop: lost, it is
    /// opened again, after a pause that grows.
    fn run(self) {
        let mut pause = FIRST_PAUSE;
        while !self.stop.load(Ordering::Relaxed) {
            let turned = match Holding::open(&self.address, &self.key, self.meeting, self.door) {
                Ok(mut holding) => {
                    pause = FIRST_PAUSE;
                    let _ = self.happened.send(Event::Held);
                    let mut spent = false;
                    let mut left = None;
                    loop {
                        if self.stop.load(Ordering::Relaxed) {
                            // Whoever asks from here on is told it is over.
                            holding.close();
                            return;
                        }
                        match holding.next(LOOKING) {
                            Ok(Some(ticket)) => self.take(ticket),
                            Ok(None) => {}
                            Err(turned) => break turned,
                        }
                        if holding.left() != left {
                            left = holding.left();
                            if let Some((left, over)) = left {
                                let _ = self.happened.send(Event::Allowance { left, over });
                            }
                        }
                        if holding.spent() != spent {
                            spent = holding.spent();
                            let _ = self.happened.send(match spent {
                                true => Event::Unheld { turned: Turned::No(No::Spent), again: true },
                                false => Event::Held,
                            });
                        }
                    }
                }
                Err(turned) => turned,
            };
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            let again = !turned.for_good();
            let _ = self.happened.send(Event::Unheld { turned, again });
            if !again {
                return;
            }
            let until = Instant::now() + pause;
            while Instant::now() < until && !self.stop.load(Ordering::Relaxed) {
                std::thread::sleep(LOOKING);
            }
            pause = (pause * 2).min(LONGEST_PAUSE);
        }
    }

    /// Someone asked the meeting point for this meeting: a connection to
    /// them is taken up, through it, and they prove the invitation over it
    /// as on a connection of their own, on a thread of its own.
    fn take(&self, ticket: Ticket) {
        // (As many as may be proving who they are: this one is let go of,
        // and the meeting point tells it that nobody answered in time.)
        if self.proving.load(Ordering::Relaxed) >= MAX_MEETING {
            return;
        }
        let (address, proved_by, proving) = (self.address.clone(), self.proved_by, self.proving.clone());
        let (links, last, happened) = (self.links.clone(), self.last.clone(), self.happened.clone());
        let (pulse, stop, gate) = (self.pulse, self.stop.clone(), self.gate.clone());
        proving.fetch_add(1, Ordering::Relaxed);
        let met = std::thread::Builder::new().name("trill-together-meet".into()).spawn(move || {
            let secured = match trill_meet::client::take(&address, ticket) {
                Ok(stream) => secure(stream, &proved_by, false).map_err(|broken| broken.to_string()),
                Err(turned) => Err(turned.sentence()),
            };
            proving.fetch_sub(1, Ordering::Relaxed);
            match secured {
                Ok(pair) => {
                    // Let in while the meeting is known to be held still:
                    // one that was ended while this one proved its
                    // invitation is no longer one, and nobody gets in by
                    // it, also not who was on the way in.
                    let _gate = gate.lock().unwrap();
                    if stop.load(Ordering::Relaxed) {
                        log::info!("a connection through the meeting point proved an invitation that is no longer one");
                    } else if let Err(error) = open(pair, None, &links, &last, &happened, pulse) {
                        log::warn!("a program that connected could not be heard: {error}");
                    }
                }
                // Someone without the invitation: nothing is said of it
                // to anyone but the log.
                Err(why) => log::info!("a connection through the meeting point was not taken: {why}"),
            }
        });
        if met.is_err() {
            self.proving.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// The connections of one program.
pub struct Net {
    links: Arc<Mutex<HashMap<LinkId, Link>>>,
    /// How many bytes of messages were said, to every program together,
    /// and how many messages: for whoever measures. Bytes of files are
    /// counted apart.
    said: (u64, u64),
    pieces_said: Arc<AtomicU64>,
    last: Arc<AtomicU64>,
    events: Receiver<Event>,
    /// What had happened when this was waited for, not yet asked for.
    early: Vec<Event>,
    happened: Sender<Event>,
    listening: Option<Listening>,
    pulse: Pulse,
}

impl Default for Net {
    fn default() -> Self {
        Self::new()
    }
}

/// Starts the two threads of a connection that has been made secure, and
/// says that it is open.
fn open(
    (mut sayer, mut hearer): (Sayer, Hearer),
    joined: Option<JoinId>,
    links: &Arc<Mutex<HashMap<LinkId, Link>>>,
    last: &Arc<AtomicU64>,
    happened: &Sender<Event>,
    pulse: Pulse,
) -> io::Result<()> {
    let link = last.fetch_add(1, Ordering::Relaxed) + 1;
    let socket = hearer.socket();
    let queue = Arc::new(Queue::default());
    links.lock().unwrap().insert(link, Link { queue: queue.clone(), socket: socket.clone() });
    // Open is said before anything can be heard.
    let _ = happened.send(Event::Open { link, joined });

    let saying_socket = socket.clone();
    std::thread::Builder::new().name("trill-together-say".into()).spawn(move || {
        // With nothing to say for a while, it says that it is there: the
        // other side takes a connection it hears nothing over for lost.
        let still = Message::Still.encode();
        loop {
            let said = match queue.next(pulse.still_every) {
                Next::Say(message) => sayer.say(&message),
                Next::Nothing => sayer.say(&still),
                Next::Closed => break,
            };
            if said.is_err() {
                break;
            }
        }
        // Everything that was to be said of the song is said, or cannot
        // be: the connection ends here, and the side that hears says why.
        queue.end();
        let _ = saying_socket.shutdown(Shutdown::Both);
    })?;

    let (links, happened) = (links.clone(), happened.clone());
    std::thread::Builder::new().name("trill-together-hear".into()).spawn(move || {
        let (mut since, mut count) = (Instant::now(), 0u32);
        // (A wait that ends in the middle of a message leaves the
        // connection of no use, and it is closed below: that is what a
        // line does which goes down in the middle of one.)
        let _ = hearer.wait_at_most(Some(pulse.silent));
        // Why it ended, and whether a side ended it and said so: what is
        // neither was lost on the way.
        let (why, told) = loop {
            let message = match hearer.hear(MAX_MESSAGE) {
                Ok(bytes) => Message::decode(&bytes),
                Err(Broken::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    break ("the other program closed the connection".to_owned(), false);
                }
                Err(Broken::Io(error))
                    if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) =>
                {
                    let why = format!("nothing was heard over it for {} seconds", pulse.silent.as_secs());
                    break (why, false);
                }
                Err(Broken::Io(error)) => break (Broken::Io(error).to_string(), false),
                Err(broken) => break (broken.to_string(), true),
            };
            if since.elapsed() > WINDOW {
                (since, count) = (Instant::now(), 0);
            }
            // (Bytes of a file are not counted here: whoever asked for
            // them knows how many it asked for, and lets go of a program
            // that sends one it did not ask for.)
            count += u32::from(!matches!(&message, Ok(message) if message.is_piece()));
            if count > MAX_IN_WINDOW {
                let why = "the other program said more, faster, than anything it could be doing to a song";
                break (why.to_owned(), true);
            }
            match message {
                // In the other side's own words.
                Ok(Message::Bye { why }) => break (shortened(&why), true),
                // It is there, and has nothing to say.
                Ok(Message::Still) => {}
                Ok(message) => {
                    if happened.send(Event::Heard { link, message }).is_err() {
                        break ("this program stopped listening".to_owned(), true);
                    }
                }
                Err(why) => break (why, true),
            }
        };
        let _ = socket.shutdown(Shutdown::Both);
        // Only if it was not let go of from here, which is said there.
        if links.lock().unwrap().remove(&link).is_some() {
            let _ = happened.send(Event::Lost { link, why, told });
        }
    })?;
    Ok(())
}

/// A reason someone else gave, at a length that is theirs to give.
fn shortened(why: &str) -> String {
    why.chars().take(300).collect()
}

/// The address other machines on the network reach this one by, as far as
/// the system says: the one it would send from to an address outside.
/// Nothing is sent.
fn address_on_the_network() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let address = socket.local_addr().ok()?.ip();
    (!address.is_unspecified() && !address.is_loopback()).then(|| address.to_string())
}

impl Net {
    pub fn new() -> Self {
        let (happened, events) = channel();
        let (early, pieces_said) = (Vec::new(), Arc::default());
        let (links, pulse) = (Arc::default(), Pulse::default());
        Self { links, said: (0, 0), pieces_said, last: Arc::default(), events, early, happened, listening: None, pulse }
    }

    /// For a test: connections made from here on say that they are there
    /// every `still_every` and are taken to be lost after `silent` of
    /// hearing nothing, in place of two seconds and ten.
    pub fn with_pulse(mut self, still_every: Duration, silent: Duration) -> Self {
        self.pulse = Pulse { still_every, silent };
        self
    }

    /// A way to say bytes of files from another thread: see [`Pieces`].
    pub fn pieces(&self) -> Pieces {
        Pieces { links: self.links.clone(), said: self.pieces_said.clone() }
    }

    /// Starts listening for programs that have the invitation, and
    /// returns it. `port` 0 is whichever the system has free. A program
    /// that is listening already goes on under the invitation it has.
    pub fn listen(&mut self, reach: Reach, port: u16) -> io::Result<Invitation> {
        if let Some(listening) = &self.listening {
            return Ok(listening.invitation.clone());
        }
        let (bind, host) = match reach {
            Reach::ThisMachine => ("127.0.0.1", "127.0.0.1".to_owned()),
            Reach::Network => ("0.0.0.0", address_on_the_network().unwrap_or_else(|| "127.0.0.1".to_owned())),
        };
        let listener = TcpListener::bind((bind, port))?;
        listener.set_nonblocking(true)?;
        let invitation = Invitation::new(host, listener.local_addr()?.port())?;
        let stop = Arc::new(AtomicBool::new(false));
        let proved_by = Arc::new(Mutex::new(invitation.key()));
        let gate = Arc::new(Mutex::new(()));
        let (key, stopped, let_in) = (proved_by.clone(), stop.clone(), gate.clone());
        let (links, last, happened) = (self.links.clone(), self.last.clone(), self.happened.clone());
        let pulse = self.pulse;
        let meeting = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new().name("trill-together-listen".into()).spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let stream = match listener.accept() {
                    // (As many as may be proving who they are: let go of.)
                    Ok(_) if meeting.load(Ordering::Relaxed) >= MAX_MEETING => continue,
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(40));
                        continue;
                    }
                    Err(error) => {
                        log::warn!("listening for programs that share the song ended: {error}");
                        break;
                    }
                };
                // Proving who it is takes the other side's time, not the
                // listener's: each on a thread of its own.
                let (links, last, happened) = (links.clone(), last.clone(), happened.clone());
                let (proving, key) = (meeting.clone(), key.clone());
                let (stopped, let_in) = (stopped.clone(), let_in.clone());
                proving.fetch_add(1, Ordering::Relaxed);
                let met = std::thread::Builder::new().name("trill-together-meet".into()).spawn(move || {
                    let _ = stream.set_nonblocking(false);
                    // By the invitation as it is when this one connects.
                    let asked = *key.lock().unwrap();
                    let secured = secure(stream, &asked, false);
                    proving.fetch_sub(1, Ordering::Relaxed);
                    match secured {
                        Ok(pair) => {
                            // Let in while the invitation is known to be
                            // the one that was proved: one that was ended
                            // or became another meanwhile is no longer one,
                            // and nobody gets in by it, also not who was on
                            // the way in.
                            let _gate = let_in.lock().unwrap();
                            let another = *key.lock().unwrap() != asked;
                            if another || stopped.load(Ordering::Relaxed) {
                                log::info!("a connection proved an invitation that is no longer one");
                            } else if let Err(error) = open(pair, None, &links, &last, &happened, pulse) {
                                log::warn!("a program that connected could not be heard: {error}");
                            }
                        }
                        // Someone without the invitation: nothing is said
                        // of it to anyone but the log.
                        Err(broken) => log::info!("a connection was not taken: {broken}"),
                    }
                });
                if met.is_err() {
                    meeting.fetch_sub(1, Ordering::Relaxed);
                }
            }
        })?;
        let proved_by = Some(proved_by);
        self.listening = Some(Listening { invitation: invitation.clone(), stop, ended: None, proved_by, gate });
        Ok(invitation)
    }

    /// Shares through a meeting point, and returns the invitation, which
    /// carries the meeting point's address. Nothing listens: this program
    /// connects outward, holds a meeting open there with the key its
    /// keeper gave, and takes up a connection through it for whoever asks
    /// for the meeting. Whether the meeting is open there follows as an
    /// [`Event::Held`] or an [`Event::Unheld`]; lost, it is opened again
    /// by itself. A program that shares already goes on under the
    /// invitation it has.
    pub fn listen_through(&mut self, point: &MeetingPoint) -> io::Result<Invitation> {
        if let Some(listening) = &self.listening {
            return Ok(listening.invitation.clone());
        }
        let invitation = Invitation::through(point.host.clone(), point.port)?;
        let stop = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(Mutex::new(()));
        let hold = Hold {
            address: point.address(),
            key: point.key.clone(),
            meeting: invitation.meeting(),
            door: invitation.door(),
            proved_by: invitation.key(),
            stop: stop.clone(),
            proving: Arc::default(),
            gate: gate.clone(),
            links: self.links.clone(),
            last: self.last.clone(),
            happened: self.happened.clone(),
            pulse: self.pulse,
        };
        let (done, ended) = channel();
        std::thread::Builder::new().name("trill-together-hold".into()).spawn(move || {
            hold.run();
            let _ = done.send(());
        })?;
        let ended = Some(ended);
        self.listening = Some(Listening { invitation: invitation.clone(), stop, ended, proved_by: None, gate });
        Ok(invitation)
    }

    /// Gives the program another invitation in place of the one it
    /// listens under, and returns it: the same address, a secret of its
    /// own by lot. The one before is no longer an invitation from here on:
    /// nobody gets in by it, also not who was proving it at this moment.
    /// The programs that are connected stay, and are told nothing by this.
    ///
    /// Once this returns, every connection the invitation before let in
    /// is among [`Net::links`], whether or not whoever drives the song has
    /// heard of it yet ([`Event::Open`]): a caller that means nobody to be
    /// in by the old invitation whom it has not greeted lets go of those.
    ///
    /// A program that listens itself goes on listening where it does.
    /// One that shares through a meeting point (`point`, the one it
    /// shares through) ends the meeting there and holds another:
    /// [`Event::Held`] follows when that is open. `None` for a program
    /// that is not listening: there is no invitation to change.
    pub fn listen_anew(&mut self, point: Option<&MeetingPoint>) -> io::Result<Option<Invitation>> {
        let Some(listening) = &mut self.listening else { return Ok(None) };
        let Some(proved_by) = &listening.proved_by else {
            let Some(point) = point else {
                return Err(io::Error::other("the meeting point the song is shared through was not named"));
            };
            self.stop_listening();
            return self.listen_through(point).map(Some);
        };
        let invitation = Invitation::new(listening.invitation.host.clone(), listening.invitation.port)?;
        {
            let _gate = listening.gate.lock().unwrap();
            *proved_by.lock().unwrap() = invitation.key();
        }
        listening.invitation = invitation.clone();
        Ok(Some(invitation))
    }

    /// The invitation this program listens under, while it does.
    pub fn listening(&self) -> Option<&Invitation> {
        self.listening.as_ref().map(|listening| &listening.invitation)
    }

    /// Stops listening: the port is closed, or the meeting ended at its
    /// meeting point, and the invitation is no longer one. The programs
    /// that are connected stay.
    pub fn stop_listening(&mut self) {
        if let Some(listening) = self.listening.take() {
            // (With the gate: whoever was let in by the invitation is in
            // by now, and whoever is still proving it is not let in.)
            {
                let _gate = listening.gate.lock().unwrap();
                listening.stop.store(true, Ordering::Relaxed);
            }
            // A meeting is ended where it is held, so that whoever asks
            // for it is told that it is over and not that its holder is
            // away: waited for, a moment, because a program that is being
            // closed ends before a thread of its own has said so.
            if let Some(ended) = listening.ended {
                let _ = ended.recv_timeout(Duration::from_secs(1));
            }
        }
    }

    /// Connects to the program that handed out `invitation`, on a thread
    /// of its own: what comes of it is an [`Event::Open`] or an
    /// [`Event::Failed`], each with the number this returns.
    pub fn join(&mut self, invitation: Invitation) -> JoinId {
        let join = self.last.fetch_add(1, Ordering::Relaxed) + 1;
        let (links, last, happened) = (self.links.clone(), self.last.clone(), self.happened.clone());
        let (failed, pulse) = (self.happened.clone(), self.pulse);
        let spawned = std::thread::Builder::new().name("trill-together-join".into()).spawn(move || {
            // Why not, and whether asking again by itself is no use.
            let connect = || -> Result<(), (String, bool)> {
                use std::net::ToSocketAddrs;
                let address = invitation.address();
                let unheard = |error: io::Error| (error.to_string(), false);
                if invitation.meet {
                    // Through the meeting point the invitation names: it
                    // is asked for the meeting, and what comes back is a
                    // connection to the program that shares, over which
                    // the two prove the invitation as they would directly.
                    let stream = trill_meet::client::join(&address, invitation.meeting(), invitation.door())
                        .map_err(|turned| (turned.sentence(), turned.for_good()))?;
                    let pair = secure(stream, &invitation.key(), true).map_err(|broken| match broken {
                        Broken::Io(_) | Broken::NotTrusted => {
                            let why = "the program that shares the song did not answer through the meeting point: \
                                       its connection may have just dropped";
                            (why.to_owned(), false)
                        }
                        broken => (broken.to_string(), false),
                    })?;
                    return open(pair, Some(join), &links, &last, &happened, pulse).map_err(unheard);
                }
                let found = (address.to_socket_addrs())
                    .map_err(|error| (format!("{address} is no address this machine finds: {error}"), false))?;
                let mut last_error = format!("{address} is no address this machine finds");
                for candidate in found {
                    match TcpStream::connect_timeout(&candidate, CONNECTING) {
                        Ok(stream) => {
                            let pair = secure(stream, &invitation.key(), true).map_err(|broken| match broken {
                                // Told so by the program there.
                                Broken::NotTrusted => {
                                    let why = format!(
                                        "the program at {address} did not take this invitation: it is listening \
                                         under another, or no longer shares its song"
                                    );
                                    (why, true)
                                }
                                Broken::Io(_) => {
                                    let why = format!(
                                        "the connection to the program at {address} ended before the invitation \
                                         was proved: it may have as many joining at once as it takes"
                                    );
                                    (why, false)
                                }
                                broken => (broken.to_string(), false),
                            })?;
                            return open(pair, Some(join), &links, &last, &happened, pulse).map_err(unheard);
                        }
                        Err(error) => {
                            last_error = format!(
                                "nothing answered at {address} ({error}): the program there is not sharing its song, \
                                 or this machine cannot reach it"
                            );
                        }
                    }
                }
                Err((last_error, false))
            };
            if let Err((why, for_good)) = connect() {
                let _ = happened.send(Event::Failed { join, why, for_good });
            }
        });
        if let Err(error) = spawned {
            let why = format!("no thread could be started to connect: {error}");
            let _ = failed.send(Event::Failed { join, why, for_good: false });
        }
        join
    }

    /// Says `message` to one program. Nothing waits: it is said when the
    /// connection gets to it, and a piece of a file when nothing else
    /// waits to be said.
    pub fn say(&mut self, link: LinkId, message: &Message) {
        if message.is_piece() {
            let _ = self.pieces().say(link, message);
            return;
        }
        self.say_encoded(&[link], Arc::new(message.encode()));
    }

    /// Says `message` to each of `to`, written out once.
    ///
    /// There is no saying to every connection there is, on purpose: a
    /// connection is there ([`Net::links`]) before whoever drives the song
    /// has heard of it ([`Event::Open`]) and said who it is, and a program
    /// that hears anything else first lets go. Whoever says names the
    /// connections it has greeted.
    pub fn say_to(&mut self, to: &[LinkId], message: &Message) {
        if !to.is_empty() {
            self.say_encoded(to, Arc::new(message.encode()));
        }
    }

    /// How many bytes of messages have been said, to every program
    /// together, and in how many messages: what goes on the wire is a
    /// little more (sixteen bytes and two of length for every 65 519).
    pub fn said(&self) -> (u64, u64) {
        self.said
    }

    fn say_encoded(&mut self, to: &[LinkId], bytes: Arc<Vec<u8>>) {
        self.said.0 += (bytes.len() * to.len()) as u64;
        self.said.1 += to.len() as u64;
        let mut behind = Vec::new();
        {
            let links = self.links.lock().unwrap();
            for link in to {
                let Some(held) = links.get(link) else { continue };
                if held.queue.put(bytes.clone(), false).is_none_or(|waiting| waiting > MAX_WAITING) {
                    behind.push(*link);
                }
            }
        }
        for link in behind {
            // Not another word to it: it has more waiting than it took.
            if let Some(held) = self.links.lock().unwrap().get(&link) {
                let _ = held.socket.shutdown(Shutdown::Both);
            }
            self.let_go(link, "it stopped hearing what was said to it");
        }
    }

    /// Ends a connection, saying why to the other side where it still
    /// hears. An [`Event::Lost`] follows, with the same words.
    pub fn close(&mut self, link: LinkId, why: &str) {
        self.say(link, &Message::Bye { why: why.to_owned() });
        // Said, then closed: the thread that says ends when its queue does,
        // and the socket goes with what holds it.
        self.let_go(link, why);
    }

    fn let_go(&mut self, link: LinkId, why: &str) {
        let held = self.links.lock().unwrap().remove(&link);
        if let Some(held) = held {
            // What waits to be said is said first, where the other side
            // takes it: the thread that says closes the connection when
            // its queue has ended, which it has now.
            drop(held);
            let _ = self.happened.send(Event::Lost { link, why: why.to_owned(), told: true });
        }
    }

    /// Ends every connection and stops listening.
    pub fn close_all(&mut self, why: &str) {
        self.stop_listening();
        for link in self.links() {
            self.close(link, why);
        }
    }

    /// The connections there are.
    pub fn links(&self) -> Vec<LinkId> {
        let mut links: Vec<LinkId> = self.links.lock().unwrap().keys().copied().collect();
        links.sort_unstable();
        links
    }

    /// What happened since this was last asked. Never waits.
    pub fn events(&mut self) -> Vec<Event> {
        let mut events = std::mem::take(&mut self.early);
        events.extend(self.events.try_iter());
        events
    }

    /// Waits `at_most` so long for the next thing that happened, and
    /// returns it alone: what happened after it stays to be asked for.
    pub fn next(&mut self, at_most: Duration) -> Option<Event> {
        match self.early.is_empty() {
            true => self.events.recv_timeout(at_most).ok(),
            false => Some(self.early.remove(0)),
        }
    }

    /// Waits until something has happened, `at_most` so long: it is there
    /// to be asked for with `events`.
    pub fn wait(&mut self, at_most: Duration) {
        if let Ok(event) = self.events.recv_timeout(at_most) {
            self.early.push(event);
        }
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.close_all("the program was closed");
    }
}
