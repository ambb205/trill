//! A meeting point as a program that goes through one speaks to it:
//! holding a meeting open with a keeper's key, and asking for one with
//! nothing but the name it goes by.
//!
//! Everything here connects outward. Nothing listens.

use crate::key::{Door, Key, Meeting, Ticket};
use crate::said::{Hello, MAX_WORD, No, NoLine, Word, answer, answering, asked, hear_line, say_line};
use crate::secure::{Broken, Room, Sayer, secure_in};
use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// How long one of a meeting point's addresses has to take a connection.
const REACHING: Duration = Duration::from_secs(6);
/// How long it has for an answer that needs nobody but itself.
const ANSWERING: Duration = Duration::from_secs(10);
/// How long it has to answer someone who asks for a meeting: it waits for
/// the meeting's holder to take the knock up (ten seconds, unless its
/// keeper says otherwise), and this is that and the way there and back.
const LETTING_IN: Duration = Duration::from_secs(30);
/// How often a holder says that it is there, and how long the answer may
/// take before the meeting point is taken to be gone.
const PING: Duration = Duration::from_secs(20);
const PONG: Duration = Duration::from_secs(15);

/// Why a meeting point did not do what it was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Turned {
    /// It could not be reached, or went away in the middle: with what the
    /// system said of it.
    NoAnswer(String),
    /// What answered there is no meeting point.
    NotOne,
    /// It said no.
    No(No),
}

impl Turned {
    /// In a sentence, for whoever was turned away.
    pub fn sentence(&self) -> String {
        match self {
            Turned::NoAnswer(why) => format!(
                "the meeting point did not answer ({why}): it is not running, its address is another, or this \
                 machine is not on the internet"
            ),
            Turned::NotOne => "what answers at the meeting point's address is no meeting point: the address or the \
                               port is another"
                .to_owned(),
            Turned::No(no) => no.sentence().to_owned(),
        }
    }

    /// In a few words, for where the meeting point is named already and
    /// what there is to do about it is said beside this.
    pub fn briefly(&self) -> String {
        match self {
            Turned::NoAnswer(why) => why.clone(),
            other => other.sentence(),
        }
    }

    /// Whether asking again by itself is no use: something has to be
    /// changed first (the key, the address, the build), or the meeting is
    /// over.
    pub fn for_good(&self) -> bool {
        matches!(
            self,
            Turned::NotOne | Turned::No(No::Key | No::Version | No::What | No::Taken | No::Closed | No::Ticket)
        )
    }
}

fn gone(error: io::Error) -> Turned {
    match error.kind() {
        // (What the system says of a connection that ended is about
        // buffers, and is nobody's to read.)
        io::ErrorKind::UnexpectedEof => Turned::NoAnswer("the connection was closed".to_owned()),
        _ => Turned::NoAnswer(error.to_string()),
    }
}

fn broken(broken: Broken) -> Turned {
    match broken {
        Broken::Io(error) => gone(error),
        Broken::NotTrusted | Broken::TooMuch(_) => Turned::NotOne,
    }
}

/// Connects to a meeting point at `address` (`host:port`).
fn reach(address: &str) -> Result<TcpStream, Turned> {
    let found =
        address.to_socket_addrs().map_err(|error| Turned::NoAnswer(format!("its address was not found: {error}")))?;
    let mut last = None;
    // (A name may have an address of each kind; no more are tried than a few.)
    for found in found.take(4) {
        match TcpStream::connect_timeout(&found, REACHING) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) => last = Some(error),
        }
    }
    Err(Turned::NoAnswer(match last {
        Some(error) => error.to_string(),
        None => "its address was not found".to_owned(),
    }))
}

/// Says a line and hears the line it is answered with, by `end`.
fn said_and_heard(stream: &TcpStream, line: &str, end: Instant) -> Result<String, Turned> {
    // (A meeting point that is full says so before it is asked anything
    // and hears nothing more: what it said is read whether or not the
    // line could still be said to it.)
    let said = say_line(stream, line, ANSWERING);
    match hear_line(stream, end) {
        Ok(line) => Ok(line),
        Err(NoLine::Late) => Err(Turned::NoAnswer("nothing came back in time".to_owned())),
        Err(NoLine::NoLine) => Err(Turned::NotOne),
        Err(NoLine::Gone(error)) => Err(gone(said.err().unwrap_or(error))),
    }
}

/// What a line that is `ok` or a refusal comes to.
fn taken(heard: &str) -> Result<(), Turned> {
    match answer(heard) {
        Some(Ok(())) => Ok(()),
        Some(Err(no)) => Err(Turned::No(no)),
        None => Err(Turned::NotOne),
    }
}

/// Says what this connection came for and hears the answer.
fn ask(stream: &TcpStream, hello: Hello, within: Duration) -> Result<(), Turned> {
    taken(&said_and_heard(stream, &hello.line(), Instant::now() + within)?)
}

/// A connection that is a pipe from here on: read and written with no
/// time limit of this file's.
fn piped(stream: TcpStream) -> Result<TcpStream, Turned> {
    stream.set_read_timeout(None).map_err(gone)?;
    stream.set_write_timeout(None).map_err(gone)?;
    Ok(stream)
}

/// Asks the meeting point at `address` for `meeting`, and answers the
/// question it puts with what only someone who has the invitation can
/// work out (`door`). What comes back is a connection to whoever holds
/// the meeting, through the meeting point: nothing has been said over
/// it, and the two prove to each other who they are as they would on a
/// connection of their own.
pub fn join(address: &str, meeting: Meeting, door: Door) -> Result<TcpStream, Turned> {
    let stream = reach(address)?;
    let end = Instant::now() + LETTING_IN;
    let question = match asked(&said_and_heard(&stream, &Hello::Join(meeting).line(), end)?) {
        Some(Ok(question)) => question,
        Some(Err(no)) => return Err(Turned::No(no)),
        None => return Err(Turned::NotOne),
    };
    taken(&said_and_heard(&stream, &answering(&door.answer(&question)), end)?)?;
    piped(stream)
}

/// Takes a knock up: what comes back is the holder's end of the
/// connection to whoever knocked.
pub fn take(address: &str, ticket: Ticket) -> Result<TcpStream, Turned> {
    let stream = reach(address)?;
    ask(&stream, Hello::Take(ticket), ANSWERING)?;
    piped(stream)
}

/// A meeting held open at a meeting point: the one connection by which
/// its holder hears of whoever knocks.
pub struct Holding {
    say: Sayer,
    socket: Arc<TcpStream>,
    heard: Receiver<Result<Option<Word>, Broken>>,
    /// When it last said that it is there, and since when it waits for
    /// the answer.
    said: Instant,
    asked: Option<Instant>,
    spent: bool,
    ping: Duration,
}

impl std::fmt::Debug for Holding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Holding")
    }
}

impl Holding {
    /// Opens `meeting` at the meeting point at `address`, with a key its
    /// keeper gave. The key does not cross the network: which key it is
    /// does, and the rest is proved by a handshake that only someone who
    /// has it can finish.
    pub fn open(address: &str, key: &Key, meeting: Meeting, door: Door) -> Result<Holding, Turned> {
        let stream = reach(address)?;
        ask(&stream, Hello::Open(key.id()), ANSWERING)?;
        let (mut say, mut hear) = match secure_in(stream, &key.proof(), true, ANSWERING, Room::SMALL) {
            Ok(secured) => secured,
            // What the meeting point answers someone whose key it does not
            // take is no part of a handshake.
            Err(Broken::NotTrusted) => return Err(Turned::No(No::Key)),
            Err(other) => return Err(broken(other)),
        };
        // (Kept to end the connection by: the thread that hears waits
        // on it for as long as there is nothing to hear.)
        let socket = hear.socket();
        say.say(Word::Open(meeting, door).text().as_bytes()).map_err(broken)?;
        hear.wait_at_most(Some(ANSWERING)).map_err(gone)?;
        match hear.hear(MAX_WORD).map(|heard| Word::read(&heard)) {
            Ok(Some(Word::Ok)) => {}
            Ok(Some(Word::No(no))) => return Err(Turned::No(no)),
            Ok(_) => return Err(Turned::NotOne),
            Err(error) => return Err(broken(error)),
        }
        hear.wait_at_most(None).map_err(gone)?;
        // Heard on a thread of its own, which waits for as long as there
        // is nothing: this one never does.
        let (to, heard) = channel();
        let hearing = std::thread::Builder::new().name("trill-meet-held".into()).spawn(move || {
            loop {
                let word = hear.hear(MAX_WORD).map(|heard| Word::read(&heard));
                let over = word.is_err();
                if to.send(word).is_err() || over {
                    return;
                }
            }
        });
        hearing.map_err(gone)?;
        Ok(Holding { say, socket, heard, said: Instant::now(), asked: None, spent: false, ping: PING })
    }

    /// Waits `within` for someone to knock. `None` is nobody; an error is
    /// the end of this connection, and with it of hearing of knocks until
    /// the meeting is opened again.
    pub fn next(&mut self, within: Duration) -> Result<Option<Ticket>, Turned> {
        let end = Instant::now() + within;
        loop {
            let now = Instant::now();
            if self.asked.is_none() && now.duration_since(self.said) >= self.ping {
                self.say.say(Word::Ping.text().as_bytes()).map_err(broken)?;
                (self.said, self.asked) = (now, Some(now));
            }
            if self.asked.is_some_and(|asked| now.duration_since(asked) >= PONG) {
                return Err(Turned::NoAnswer("it stopped answering".to_owned()));
            }
            let wait = end.saturating_duration_since(now).min(Duration::from_millis(250));
            match self.heard.recv_timeout(wait) {
                Ok(Ok(Some(Word::Knock(ticket)))) => return Ok(Some(ticket)),
                Ok(Ok(Some(Word::Pong))) => (self.asked, self.spent) = (None, false),
                Ok(Ok(Some(Word::No(No::Spent)))) => (self.asked, self.spent) = (None, true),
                Ok(Ok(Some(Word::No(no)))) => return Err(Turned::No(no)),
                Ok(Ok(_)) => return Err(Turned::NotOne),
                Ok(Err(error)) => return Err(broken(error)),
                Err(RecvTimeoutError::Timeout) if Instant::now() >= end => return Ok(None),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Turned::NoAnswer("the connection ended".to_owned()));
                }
            }
        }
    }

    /// Whether the meeting point said, when it was last asked, that the
    /// key has passed its bytes of the day: the meeting is held on, and
    /// nobody is let into it until the day is over.
    pub fn spent(&self) -> bool {
        self.spent
    }

    /// How often it says that it is there, in place of every twenty
    /// seconds: for a test that takes a key away and does not wait that
    /// long to hear of it.
    #[doc(hidden)]
    pub fn say_it_is_there_every(&mut self, ping: Duration) {
        self.ping = ping;
    }

    /// Ends the meeting: whoever asks for it from here on is told that it
    /// is over. (Dropped without this, it is taken to be away for a while,
    /// as when the connection is lost.)
    pub fn close(mut self) {
        if self.say.say(Word::Close.text().as_bytes()).is_ok() {
            // Until the meeting point has taken it, which it shows by
            // closing its end.
            let end = Instant::now() + Duration::from_secs(2);
            while Instant::now() < end {
                match self.heard.recv_timeout(Duration::from_millis(100)) {
                    Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => break,
                    _ => {}
                }
            }
        }
    }
}

impl Drop for Holding {
    fn drop(&mut self) {
        // Which ends the thread that hears.
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}
