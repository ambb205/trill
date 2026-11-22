//! A connection that only the two programs that hold a secret can read or
//! write: two Trills that hold an invitation's, or a Trill and a meeting
//! point that hold a keeper's key.
//!
//! The Noise protocol does it, through the `snow` library: the pattern
//! `NNpsk0` over X25519, ChaCha20-Poly1305 and BLAKE2s. Each side makes a
//! key pair for the one connection; the secret both were handed is mixed
//! into the handshake, so that a side that has not got it cannot finish;
//! and everything after travels under keys nobody else has, each message
//! with a tag that says it arrived as it was sent, in the order it was
//! sent. Nothing of that is written here: this file frames what `snow`
//! encrypts.
//!
//! On the wire a connection is frames of two bytes of length and that
//! many bytes: the two messages of the handshake, then encrypted pieces
//! of at most 65 535 bytes. Inside the encryption runs a stream of
//! messages, each four bytes of length and that many bytes, which a
//! message longer than a piece is spread over.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The Noise handshake both sides speak.
const PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
/// The most one encrypted piece carries: Noise's limit, less its tag.
const PIECE: usize = 65_535 - 16;
/// How long a side has to say its part of the handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a write may take before the other side is taken to be gone.
const WRITING: Duration = Duration::from_secs(30);
/// The most a message of the handshake is taken to be. (Each is 48
/// bytes: nobody is given room for more by saying a length.)
const HANDSHAKE_FRAME: usize = 1024;

/// How much room a connection takes: the most one encrypted piece of it
/// carries, which is what its buffers are made for. Both ends of a
/// connection are made with the same.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Room {
    piece: usize,
}

impl Room {
    /// For whole songs: pieces as large as Noise has them.
    pub const WHOLE: Room = Room { piece: PIECE };
    /// For a few words at a time, where many connections are held: a
    /// kilobyte.
    pub const SMALL: Room = Room { piece: 1024 };
}

/// Why a connection did not come about, or ended.
#[derive(Debug)]
pub enum Broken {
    /// The other side is gone, or the network is.
    Io(io::Error),
    /// The other side has not got the invitation's secret, or what
    /// arrived was not what was sent.
    NotTrusted,
    /// The other side said more than may be said at once.
    TooMuch(usize),
}

impl std::fmt::Display for Broken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Broken::Io(error) => write!(f, "the connection ended: {error}"),
            Broken::NotTrusted => write!(
                f,
                "the other program has not got this invitation: its secret is another, or what arrived was not what \
                 was sent"
            ),
            Broken::TooMuch(bytes) => {
                write!(f, "the other program said {bytes} bytes at once, which is more than is taken")
            }
        }
    }
}

impl From<io::Error> for Broken {
    fn from(error: io::Error) -> Self {
        Broken::Io(error)
    }
}

impl From<snow::Error> for Broken {
    fn from(_: snow::Error) -> Self {
        Broken::NotTrusted
    }
}

fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let length = u16::try_from(bytes.len()).map_err(|_| io::Error::other("a frame of more than 65535 bytes"))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(bytes)
}

/// Reads one frame. One that says it is longer than `most` is refused by
/// its length, before room is made for it.
fn read_frame(stream: &mut impl Read, into: &mut Vec<u8>, most: usize) -> Result<(), Broken> {
    let mut length = [0u8; 2];
    stream.read_exact(&mut length)?;
    let length = usize::from(u16::from_be_bytes(length));
    if length > most {
        return Err(Broken::TooMuch(length));
    }
    into.resize(length, 0);
    Ok(stream.read_exact(into)?)
}

/// A socket read until a moment and no longer. A time limit on the socket
/// alone is a limit on each read: a side that said its part of the
/// handshake a byte at a time would be waited for as long as it cared to
/// go on, and hold one of the few places there are for proving an
/// invitation.
struct Until<'a> {
    stream: &'a TcpStream,
    end: Instant,
}

impl Read for Until<'_> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let left = self.end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "its part of the handshake was not said in time"));
        }
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        stream.read(into)
    }
}

/// The keys of one connection, which its reader and its writer share.
struct Keys(snow::StatelessTransportState);

/// The socket of a connection, as its two halves and whoever ends it
/// hold it: one handle of the system's, shared. No second handle is ever
/// made for a socket (`TcpStream::try_clone`). On Windows a second handle
/// is a thing of its own: it keeps time limits of its own, and ending a
/// connection through one has been seen to fail, now and then, with the
/// system saying that the socket is not connected. On Linux neither is
/// so. With one handle the two systems do the same.
struct Wire(Arc<TcpStream>);

impl Read for Wire {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(into)
    }
}

impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        (&*self.0).write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

/// The half of a connection that says.
pub struct Sayer {
    stream: io::BufWriter<Wire>,
    keys: Arc<Keys>,
    room: Room,
    /// How many pieces have been said: each is encrypted under its number,
    /// which the other side counts along.
    said: u64,
    piece: Vec<u8>,
}

/// The half of a connection that hears.
pub struct Hearer {
    stream: io::BufReader<Wire>,
    keys: Arc<Keys>,
    room: Room,
    heard: u64,
    /// What has arrived of the stream of messages and is not yet a whole
    /// message.
    pending: Vec<u8>,
    frame: Vec<u8>,
    piece: Vec<u8>,
}

/// Makes `stream` a connection only the holders of `key` can use: the
/// side that connected is the `initiator`. The other side has said the
/// whole of its part within a few seconds, or there is no connection.
pub fn secure(stream: impl Into<Arc<TcpStream>>, key: &[u8; 32], initiator: bool) -> Result<(Sayer, Hearer), Broken> {
    secure_in(stream, key, initiator, HANDSHAKE, Room::WHOLE)
}

/// [`secure`], with how long the other side has for its part and how
/// much room the connection takes. The socket is taken as it is or as
/// one that is shared already: no second handle is made for it.
pub fn secure_in(
    stream: impl Into<Arc<TcpStream>>,
    key: &[u8; 32],
    initiator: bool,
    within: Duration,
    room: Room,
) -> Result<(Sayer, Hearer), Broken> {
    let stream: Arc<TcpStream> = stream.into();
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(WRITING))?;
    let builder = snow::Builder::new(PATTERN.parse()?).psk(0, key)?;
    let mut state = if initiator { builder.build_initiator()? } else { builder.build_responder()? };
    let (mut said, mut heard) = (vec![0u8; 1024], Vec::new());
    let mut nothing = [0u8; 1024];
    let mut wire = &*stream;
    let mut hearing = Until { stream: &stream, end: Instant::now() + within };
    if initiator {
        let length = state.write_message(&[], &mut said)?;
        write_frame(&mut wire, &said[..length])?;
        read_frame(&mut hearing, &mut heard, HANDSHAKE_FRAME)?;
        state.read_message(&heard, &mut nothing)?;
    } else {
        read_frame(&mut hearing, &mut heard, HANDSHAKE_FRAME)?;
        if state.read_message(&heard, &mut nothing).is_err() {
            // The other side has not got the secret. Nothing can be said
            // under a secret it has not got: an empty message, where this
            // side's part of the handshake would be, tells it so, and
            // tells it apart from a connection that was lost on the way.
            let _ = write_frame(&mut wire, &[]);
            return Err(Broken::NotTrusted);
        }
        let length = state.write_message(&[], &mut said)?;
        write_frame(&mut wire, &said[..length])?;
    }
    let keys = Arc::new(Keys(state.into_stateless_transport_mode()?));
    // From here on a side that hears nothing waits: the other may have
    // nothing to say.
    stream.set_read_timeout(None)?;
    // A piece on the wire is its bytes, its tag and two bytes of length.
    let framed = room.piece + 16;
    let sayer = Sayer {
        stream: io::BufWriter::with_capacity(framed + 2, Wire(stream.clone())),
        keys: keys.clone(),
        room,
        said: 0,
        piece: vec![0u8; framed],
    };
    let hearer = Hearer {
        stream: io::BufReader::with_capacity(framed + 2, Wire(stream)),
        keys,
        room,
        heard: 0,
        pending: Vec::new(),
        frame: Vec::new(),
        piece: vec![0u8; framed],
    };
    Ok((sayer, hearer))
}

impl Sayer {
    /// Says one message, whole.
    pub fn say(&mut self, message: &[u8]) -> Result<(), Broken> {
        let length = u32::try_from(message.len()).map_err(|_| Broken::TooMuch(message.len()))?;
        // The length rides with the first piece.
        let mut whole = length.to_be_bytes().to_vec();
        let room = self.room.piece - whole.len();
        let (first, rest) = message.split_at(message.len().min(room));
        whole.extend_from_slice(first);
        for piece in std::iter::once(&whole[..]).chain(rest.chunks(self.room.piece)) {
            let length = self.keys.0.write_message(self.said, piece, &mut self.piece)?;
            self.said += 1;
            write_frame(&mut self.stream, &self.piece[..length])?;
        }
        Ok(self.stream.flush()?)
    }

    /// How long saying may take before the other side is taken to be
    /// gone, in place of half a minute.
    pub fn within(&self, time: Duration) -> io::Result<()> {
        self.stream.get_ref().0.set_write_timeout(Some(time))
    }
}

impl Hearer {
    /// Waits for the next message and returns it, whole. One that says it
    /// is longer than `limit` is refused before any of it is kept.
    pub fn hear(&mut self, limit: usize) -> Result<Vec<u8>, Broken> {
        loop {
            if self.pending.len() >= 4 {
                let length = u32::from_be_bytes(self.pending[..4].try_into().expect("four bytes")) as usize;
                if length > limit {
                    return Err(Broken::TooMuch(length));
                }
                if self.pending.len() >= 4 + length {
                    let message = self.pending[4..4 + length].to_vec();
                    self.pending.drain(..4 + length);
                    return Ok(message);
                }
            }
            read_frame(&mut self.stream, &mut self.frame, self.room.piece + 16)?;
            let length = self.keys.0.read_message(self.heard, &self.frame, &mut self.piece)?;
            self.heard += 1;
            self.pending.extend_from_slice(&self.piece[..length]);
        }
    }

    /// How long [`Hearer::hear`] waits for the other side to say
    /// something: `None` is for as long as it takes, which is how a
    /// connection begins. A wait that ends in the middle of a message
    /// leaves the connection of no more use: whoever sets this closes it
    /// when the time is over.
    pub fn wait_at_most(&self, time: Option<Duration>) -> io::Result<()> {
        self.stream.get_ref().0.set_read_timeout(time)
    }

    /// The socket, for whoever ends the connection from another thread:
    /// the one handle there is of it, shared.
    pub fn socket(&self) -> Arc<TcpStream> {
        self.stream.get_ref().0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// What one end of a connection came to.
    type End = Result<(Sayer, Hearer), Broken>;

    /// Two ends of a connection on this machine, each made secure with
    /// its key on a thread of its own.
    fn pair(ours: [u8; 32], theirs: [u8; 32]) -> (End, End) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || secure(listener.accept().unwrap().0, &theirs, false));
        let connected = secure(TcpStream::connect(address).unwrap(), &ours, true);
        (connected, accepting.join().unwrap())
    }

    #[test]
    fn two_programs_with_the_secret_say_messages_of_any_size_to_each_other_and_nobody_reads_them() {
        let key = [7u8; 32];
        let (ours, theirs) = pair(key, key);
        let ((mut say, mut hear), (mut their_say, mut their_hear)) = (ours.unwrap(), theirs.unwrap());
        // Short, empty, exactly a piece, and megabytes: each arrives whole
        // and in order, both ways at once.
        let sizes = [5usize, 0, PIECE - 4, PIECE - 3, PIECE, 3 * PIECE + 17, 5_000_000];
        let message = |size: usize| -> Vec<u8> { (0..size).map(|index| (index * 31 + size) as u8).collect() };
        let saying = std::thread::spawn(move || {
            for size in sizes {
                say.say(&message(size)).unwrap();
            }
            say
        });
        for size in sizes {
            assert!(their_hear.hear(10_000_000).unwrap() == message(size), "{size} bytes arrive as they were sent");
        }
        their_say.say(b"and back").unwrap();
        assert_eq!(hear.hear(100).unwrap(), b"and back");
        drop(saying.join().unwrap());
    }

    #[test]
    fn what_is_changed_on_the_way_is_not_taken_and_nothing_is_said_in_the_clear() {
        let key = [5u8; 32];
        let far = TcpListener::bind("127.0.0.1:0").unwrap();
        let far_address = far.local_addr().unwrap();
        let middle = TcpListener::bind("127.0.0.1:0").unwrap();
        let middle_address = middle.local_addr().unwrap();
        // Someone in the middle, who passes everything on, reads along
        // and turns one bit of what is said over.
        let relay = std::thread::spawn(move || {
            let (mut from_near, _) = middle.accept().unwrap();
            let mut to_far = TcpStream::connect(far_address).unwrap();
            let (mut back_from, mut back_to) = (to_far.try_clone().unwrap(), from_near.try_clone().unwrap());
            std::thread::spawn(move || std::io::copy(&mut back_from, &mut back_to));
            let (mut seen, mut clear) = (0usize, false);
            let mut buffer = [0u8; 4096];
            loop {
                let count = from_near.read(&mut buffer).unwrap_or(0);
                if count == 0 {
                    break;
                }
                clear |= buffer[..count].windows(6).any(|window| window == b"secret");
                for byte in &mut buffer[..count] {
                    seen += 1;
                    if seen == 120 {
                        *byte ^= 1;
                    }
                }
                if to_far.write_all(&buffer[..count]).is_err() {
                    break;
                }
            }
            clear
        });
        let accepting = std::thread::spawn(move || secure(far.accept().unwrap().0, &key, false));
        let (mut say, hear) = secure(TcpStream::connect(middle_address).unwrap(), &key, true).unwrap();
        let (_their_say, mut their_hear) = accepting.join().unwrap().unwrap();
        say.say(&b"a secret said over the wire, ".repeat(8)).unwrap();
        assert!(matches!(their_hear.hear(1 << 20), Err(Broken::NotTrusted)), "a message that was changed is not taken");
        drop((say, hear));
        assert!(!relay.join().unwrap(), "and whoever read along read nothing");
    }

    #[test]
    fn a_program_without_the_secret_gets_no_connection_and_one_that_says_too_much_is_cut_off() {
        let (ours, theirs) = pair([1u8; 32], [2u8; 32]);
        assert!(matches!(theirs, Err(Broken::NotTrusted)), "the side that listens refuses");
        assert!(matches!(ours, Err(Broken::NotTrusted)), "and the side that connects is told so, and has nothing");

        // Someone who speaks no Noise at all.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || secure(listener.accept().unwrap().0, &[3u8; 32], false));
        let mut stranger = TcpStream::connect(address).unwrap();
        stranger.write_all(b"GET / HTTP/1.1\r\nHost: trill\r\n\r\n").unwrap();
        drop(stranger);
        assert!(accepting.join().unwrap().is_err());

        // A message that says it is larger than is taken is refused by its
        // length, before it has arrived.
        let key = [9u8; 32];
        let (ours, theirs) = pair(key, key);
        let ((mut say, _hear), (_their_say, mut their_hear)) = (ours.unwrap(), theirs.unwrap());
        std::thread::spawn(move || say.say(&vec![0u8; 300_000]));
        assert!(matches!(their_hear.hear(100_000), Err(Broken::TooMuch(300_000))));

        // A handshake whose first message says it is long is refused by
        // that length, and no room is made for it.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || secure(listener.accept().unwrap().0, &[3u8; 32], false));
        let mut stranger = TcpStream::connect(address).unwrap();
        stranger.write_all(&[0xff, 0xff]).unwrap();
        assert!(matches!(accepting.join().unwrap(), Err(Broken::TooMuch(65_535))));
        drop(stranger);

        // A connection for a few words at a time takes them, takes a
        // message of many pieces, and refuses a piece larger than its own.
        let key = [4u8; 32];
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let small = Duration::from_secs(5);
        let accepting =
            std::thread::spawn(move || secure_in(listener.accept().unwrap().0, &key, false, small, Room::SMALL));
        let (mut say, _hear) = secure_in(TcpStream::connect(address).unwrap(), &key, true, small, Room::SMALL).unwrap();
        let (_their_say, mut their_hear) = accepting.join().unwrap().unwrap();
        say.say(b"a few words").unwrap();
        assert_eq!(their_hear.hear(100).unwrap(), b"a few words");
        say.say(&[7u8; 5000]).unwrap();
        assert_eq!(their_hear.hear(10_000).unwrap(), vec![7u8; 5000]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepting =
            std::thread::spawn(move || secure_in(listener.accept().unwrap().0, &key, false, small, Room::SMALL));
        let (mut say, _hear) = secure(TcpStream::connect(address).unwrap(), &key, true).unwrap();
        let (_their_say, mut their_hear) = accepting.join().unwrap().unwrap();
        std::thread::spawn(move || say.say(&[7u8; 5000]));
        assert!(matches!(their_hear.hear(10_000), Err(Broken::TooMuch(_))), "a piece of a whole song's size");
    }

    #[test]
    fn someone_who_says_their_part_a_byte_at_a_time_has_no_longer_than_anyone_else() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || {
            let stream = listener.accept().unwrap().0;
            let began = Instant::now();
            let secured = secure_in(stream, &[3u8; 32], false, Duration::from_millis(400), Room::WHOLE);
            (secured.is_err(), began.elapsed())
        });
        // A frame that says it is as long as one of the handshake may be,
        // and then a byte every so often: each of them arrives well within
        // the time the whole has.
        let mut stranger = TcpStream::connect(address).unwrap();
        stranger.write_all(&[0x04, 0x00]).unwrap();
        for _ in 0..100 {
            if accepting.is_finished() || stranger.write_all(&[0]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let (refused, took) = accepting.join().unwrap();
        assert!(refused, "no connection comes of it");
        assert!(took < Duration::from_secs(3), "and it was let go of when its time was up, not after {took:?}");
    }
}
