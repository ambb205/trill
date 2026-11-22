//! A meeting point on this machine for a test, on a port the system
//! picks, with its log kept to be read.
//!
//! Every connection a test makes keeps one of the machine's ports for
//! two minutes after it is closed, and the machine has some sixteen
//! thousand: a test here makes the few connections it is about, each
//! counted beside it, and none in a loop.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use trill_meet::keys::Given;
use trill_meet::secure::{Hearer, Sayer, secure};
use trill_meet::{Counts, Door, Holding, Key, Keys, Limits, Meeting, Server, client};

pub struct Point {
    pub server: Server,
    pub address: String,
    log: Arc<Mutex<Vec<String>>>,
}

/// A meeting point with these limits, at which `keys` are given.
pub fn point(limits: Limits, keys: &[(&Key, &str)]) -> Point {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let log = Arc::new(Mutex::new(Vec::new()));
    let lines = log.clone();
    let given = keys.iter().map(|(key, name)| Given::of(key, name)).collect();
    let said: trill_meet::Log = Arc::new(move |line: &str| lines.lock().unwrap().push(line.to_owned()));
    let server = Server::start(vec![listener], Keys::of(given), limits, said).unwrap();
    Point { server, address, log }
}

/// Limits for a test that waits for a stranger to be let go of: what
/// one has to say what it came for is over in two seconds. Everything
/// else is as a keeper has it, so that on a busy machine nobody in a
/// test is turned away by a limit the test is not about. (A limit that
/// a test is about is set short at a meeting point of its own, where
/// nothing else has to be in time.)
pub fn quick() -> Limits {
    Limits { hello: Duration::from_secs(2), ..Limits::default() }
}

/// What an invitation's secret comes to at a meeting point: what its
/// meeting goes by, and what whoever asks for it shows that they have.
pub type Invited = (Meeting, Door);

pub fn invited(secret: &[u8]) -> Invited {
    (Meeting::of(secret), Door::of(secret))
}

/// Holds the meeting of an invitation open.
pub fn hold(at: &str, key: &Key, (meeting, door): Invited) -> Result<Holding, trill_meet::Turned> {
    Holding::open(at, key, meeting, door)
}

/// Asks for the meeting of an invitation, as someone who has it.
pub fn ask(at: &str, (meeting, door): Invited) -> Result<TcpStream, trill_meet::Turned> {
    client::join(at, meeting, door)
}

impl Point {
    /// The log so far, a line a line.
    pub fn log(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    /// Waits until the meeting point holds what `as_wanted` says, for a
    /// while.
    pub fn until(&self, what: &str, as_wanted: impl Fn(Counts) -> bool) -> Counts {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let counts = self.server.counts();
            if as_wanted(counts) {
                return counts;
            }
            assert!(Instant::now() < end, "{what}: {counts:?}\n{}", self.log());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits until the log has a line with `words` in it.
    pub fn until_said(&self, words: &str) {
        let end = Instant::now() + Duration::from_secs(20);
        while !self.log().contains(words) {
            assert!(Instant::now() < end, "the log never said \"{words}\":\n{}", self.log());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A connection that has said nothing yet.
    pub fn stranger(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        stream
    }

    /// Someone who asks for a meeting, with its invitation, while its
    /// holder takes the knock up: the two ends of the pipe that comes of
    /// it, the holder's first. (Two connections.)
    pub fn pipe(&self, holding: &mut Holding, invited: Invited) -> (TcpStream, TcpStream) {
        let address = self.address.clone();
        let joining = std::thread::spawn(move || ask(&address, invited));
        let ticket = holding.next(Duration::from_secs(20)).unwrap().expect("someone knocked");
        let ours = client::take(&self.address, ticket).unwrap();
        let theirs = joining.join().unwrap().expect("and was let in");
        // (So that a test that waits for what never comes fails.)
        for end in [&ours, &theirs] {
            end.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        }
        (ours, theirs)
    }
}

/// Everything a connection says until it is closed, as text, and how
/// long that took. (A connection that is cut off in the middle counts as
/// closed, with what arrived before.)
pub fn heard_until_closed(mut stream: &TcpStream) -> (String, Duration) {
    stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let began = Instant::now();
    let mut heard = Vec::new();
    let mut piece = [0u8; 256];
    loop {
        match stream.read(&mut piece) {
            Ok(0) => break,
            Ok(count) => heard.extend_from_slice(&piece[..count]),
            Err(error) if matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
                panic!("it was not let go of: {:?} heard in {:?}", String::from_utf8_lossy(&heard), began.elapsed())
            }
            Err(_) => break,
        }
    }
    (String::from_utf8_lossy(&heard).into_owned(), began.elapsed())
}

/// Says a line to a connection.
pub fn say(mut stream: &TcpStream, line: &str) {
    stream.write_all(format!("{line}\n").as_bytes()).unwrap();
}

/// One line a connection says, without its end.
pub fn heard_line(mut stream: &TcpStream) -> String {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while stream.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
        line.push(byte[0]);
    }
    String::from_utf8(line).unwrap()
}

/// The two ends of a pipe, made secure by a secret both have, as two
/// Trills do with an invitation's: the holder's first.
pub fn secured(ours: TcpStream, theirs: TcpStream, secret: [u8; 32]) -> ((Sayer, Hearer), (Sayer, Hearer)) {
    let answering = std::thread::spawn(move || secure(ours, &secret, false).unwrap());
    let asked = secure(theirs, &secret, true).unwrap();
    (answering.join().unwrap(), asked)
}
