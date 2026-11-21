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

use crate::invite::Invitation;
use crate::secure::{Broken, Hearer, Sayer, secure};
use crate::wire::{MAX_MESSAGE, Message};
use std::collections::HashMap;
use std::io;
use std::net::{Shutdown, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Which connection.
pub type LinkId = u64;

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
    /// over it yet. `joined` for one this program asked for.
    Open {
        link: LinkId,
        joined: bool,
    },
    Heard {
        link: LinkId,
        message: Message,
    },
    /// A connection is gone, and why.
    Lost {
        link: LinkId,
        why: String,
    },
    /// Joining did not come to a connection.
    Failed {
        why: String,
    },
}

/// One connection, as the program that drives the song holds it.
struct Link {
    say: Sender<Arc<Vec<u8>>>,
    socket: TcpStream,
    /// How many bytes wait to be said.
    waiting: Arc<AtomicUsize>,
}

struct Listening {
    invitation: Invitation,
    stop: Arc<AtomicBool>,
}

/// The connections of one program.
pub struct Net {
    links: Arc<Mutex<HashMap<LinkId, Link>>>,
    /// How many bytes of messages were said, to every program together,
    /// and how many messages: for whoever measures.
    said: (u64, u64),
    last: Arc<AtomicU64>,
    events: Receiver<Event>,
    /// What had happened when this was waited for, not yet asked for.
    early: Vec<Event>,
    happened: Sender<Event>,
    listening: Option<Listening>,
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
    joined: bool,
    links: &Arc<Mutex<HashMap<LinkId, Link>>>,
    last: &Arc<AtomicU64>,
    happened: &Sender<Event>,
) -> io::Result<()> {
    let link = last.fetch_add(1, Ordering::Relaxed) + 1;
    let socket = hearer.socket()?;
    let (say, to_say) = channel::<Arc<Vec<u8>>>();
    let waiting = Arc::new(AtomicUsize::new(0));
    links.lock().unwrap().insert(link, Link { say, socket: socket.try_clone()?, waiting: waiting.clone() });
    // Open is said before anything can be heard.
    let _ = happened.send(Event::Open { link, joined });

    let saying_socket = socket.try_clone()?;
    std::thread::Builder::new().name("trill-together-say".into()).spawn(move || {
        for message in to_say {
            let said = sayer.say(&message);
            waiting.fetch_sub(message.len(), Ordering::Relaxed);
            if said.is_err() {
                break;
            }
        }
        // Everything that was to be said is said, or cannot be: the
        // connection ends here, and the side that hears says why.
        let _ = saying_socket.shutdown(Shutdown::Both);
    })?;

    let (links, happened) = (links.clone(), happened.clone());
    std::thread::Builder::new().name("trill-together-hear".into()).spawn(move || {
        let (mut since, mut count) = (Instant::now(), 0u32);
        let why = loop {
            let message = match hearer.hear(MAX_MESSAGE) {
                Ok(bytes) => Message::decode(&bytes),
                Err(Broken::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    break "the other program closed the connection".to_owned();
                }
                Err(broken) => break broken.to_string(),
            };
            if since.elapsed() > WINDOW {
                (since, count) = (Instant::now(), 0);
            }
            count += 1;
            if count > MAX_IN_WINDOW {
                break "the other program said more, faster, than anything it could be doing to a song".to_owned();
            }
            match message {
                // In the other side's own words.
                Ok(Message::Bye { why }) => break shortened(&why),
                Ok(message) => {
                    if happened.send(Event::Heard { link, message }).is_err() {
                        break "this program stopped listening".to_owned();
                    }
                }
                Err(why) => break why,
            }
        };
        let _ = socket.shutdown(Shutdown::Both);
        // Only if it was not let go of from here, which is said there.
        if links.lock().unwrap().remove(&link).is_some() {
            let _ = happened.send(Event::Lost { link, why });
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
        let early = Vec::new();
        Self { links: Arc::default(), said: (0, 0), last: Arc::default(), events, early, happened, listening: None }
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
        let (key, stopped) = (invitation.key(), stop.clone());
        let (links, last, happened) = (self.links.clone(), self.last.clone(), self.happened.clone());
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
                let proving = meeting.clone();
                proving.fetch_add(1, Ordering::Relaxed);
                let met = std::thread::Builder::new().name("trill-together-meet".into()).spawn(move || {
                    let _ = stream.set_nonblocking(false);
                    let secured = secure(stream, &key, false);
                    proving.fetch_sub(1, Ordering::Relaxed);
                    match secured {
                        Ok(pair) => {
                            if let Err(error) = open(pair, false, &links, &last, &happened) {
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
        self.listening = Some(Listening { invitation: invitation.clone(), stop });
        Ok(invitation)
    }

    /// The invitation this program listens under, while it does.
    pub fn listening(&self) -> Option<&Invitation> {
        self.listening.as_ref().map(|listening| &listening.invitation)
    }

    /// Stops listening: the port is closed, and the invitation is no
    /// longer one. The programs that are connected stay.
    pub fn stop_listening(&mut self) {
        if let Some(listening) = self.listening.take() {
            listening.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Connects to the program that handed out `invitation`, on a thread
    /// of its own: what comes of it is an [`Event::Open`] or an
    /// [`Event::Failed`].
    pub fn join(&mut self, invitation: Invitation) {
        let (links, last, happened) = (self.links.clone(), self.last.clone(), self.happened.clone());
        let failed = self.happened.clone();
        let spawned = std::thread::Builder::new().name("trill-together-join".into()).spawn(move || {
            let connect = || -> Result<(), String> {
                use std::net::ToSocketAddrs;
                let address = invitation.address();
                let found = address.to_socket_addrs().map_err(|error| format!("{address} is no address: {error}"))?;
                let mut last_error = format!("{address} is no address");
                for candidate in found {
                    match TcpStream::connect_timeout(&candidate, CONNECTING) {
                        Ok(stream) => {
                            let pair = secure(stream, &invitation.key(), true).map_err(|broken| match broken {
                                Broken::Io(_) | Broken::NotTrusted => format!(
                                    "the program at {address} did not take this invitation: it is listening under \
                                     another, or no longer shares its song"
                                ),
                                broken => broken.to_string(),
                            })?;
                            return open(pair, true, &links, &last, &happened).map_err(|error| error.to_string());
                        }
                        Err(error) => {
                            last_error = format!(
                                "nothing answered at {address} ({error}): the program there is not sharing its song, \
                                 or this machine cannot reach it"
                            );
                        }
                    }
                }
                Err(last_error)
            };
            if let Err(why) = connect() {
                let _ = happened.send(Event::Failed { why });
            }
        });
        if let Err(error) = spawned {
            let _ = failed.send(Event::Failed { why: format!("no thread could be started to connect: {error}") });
        }
    }

    /// Says `message` to one program. Nothing waits: it is said when the
    /// connection gets to it.
    pub fn say(&mut self, link: LinkId, message: &Message) {
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
                let full = held.waiting.fetch_add(bytes.len(), Ordering::Relaxed) + bytes.len() > MAX_WAITING;
                if full || held.say.send(bytes.clone()).is_err() {
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
            let _ = self.happened.send(Event::Lost { link, why: why.to_owned() });
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
