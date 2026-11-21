//! A connection that only the two programs that hold an invitation's
//! secret can read or write.
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
use std::time::Duration;

/// The Noise handshake both sides speak.
const PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
/// The most one encrypted piece carries: Noise's limit, less its tag.
const PIECE: usize = 65_535 - 16;
/// How long a side has to say its part of the handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a write may take before the other side is taken to be gone.
const WRITING: Duration = Duration::from_secs(30);

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
            Broken::TooMuch(bytes) => write!(f, "the other program said {bytes} bytes at once, which is more than is taken"),
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

fn read_frame(stream: &mut impl Read, into: &mut Vec<u8>) -> io::Result<()> {
    let mut length = [0u8; 2];
    stream.read_exact(&mut length)?;
    into.resize(usize::from(u16::from_be_bytes(length)), 0);
    stream.read_exact(into)
}

/// The keys of one connection, which its reader and its writer share.
struct Keys(snow::StatelessTransportState);

/// The half of a connection that says.
pub struct Sayer {
    stream: io::BufWriter<TcpStream>,
    keys: Arc<Keys>,
    /// How many pieces have been said: each is encrypted under its number,
    /// which the other side counts along.
    said: u64,
    piece: Vec<u8>,
}

/// The half of a connection that hears.
pub struct Hearer {
    stream: io::BufReader<TcpStream>,
    keys: Arc<Keys>,
    heard: u64,
    /// What has arrived of the stream of messages and is not yet a whole
    /// message.
    pending: Vec<u8>,
    frame: Vec<u8>,
    piece: Vec<u8>,
}

/// Makes `stream` a connection only the holders of `key` can use: the
/// side that connected is the `initiator`. Both sides say their part
/// within a few seconds, or there is no connection.
pub fn secure(stream: TcpStream, key: &[u8; 32], initiator: bool) -> Result<(Sayer, Hearer), Broken> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(HANDSHAKE))?;
    stream.set_write_timeout(Some(WRITING))?;
    let builder = snow::Builder::new(PATTERN.parse()?).psk(0, key)?;
    let mut state = if initiator { builder.build_initiator()? } else { builder.build_responder()? };
    let (mut said, mut heard) = (vec![0u8; 1024], Vec::new());
    let mut nothing = [0u8; 1024];
    let mut wire = stream.try_clone()?;
    if initiator {
        let length = state.write_message(&[], &mut said)?;
        write_frame(&mut wire, &said[..length])?;
        read_frame(&mut wire, &mut heard)?;
        state.read_message(&heard, &mut nothing)?;
    } else {
        read_frame(&mut wire, &mut heard)?;
        state.read_message(&heard, &mut nothing)?;
        let length = state.write_message(&[], &mut said)?;
        write_frame(&mut wire, &said[..length])?;
    }
    let keys = Arc::new(Keys(state.into_stateless_transport_mode()?));
    // From here on a side that hears nothing waits: the other may have
    // nothing to say.
    stream.set_read_timeout(None)?;
    let sayer = Sayer {
        stream: io::BufWriter::with_capacity(1 << 16, stream.try_clone()?),
        keys: keys.clone(),
        said: 0,
        piece: vec![0u8; 65_535],
    };
    let hearer = Hearer {
        stream: io::BufReader::with_capacity(1 << 16, stream),
        keys,
        heard: 0,
        pending: Vec::new(),
        frame: Vec::new(),
        piece: vec![0u8; 65_535],
    };
    Ok((sayer, hearer))
}

impl Sayer {
    /// Says one message, whole.
    pub fn say(&mut self, message: &[u8]) -> Result<(), Broken> {
        let length = u32::try_from(message.len()).map_err(|_| Broken::TooMuch(message.len()))?;
        // The length rides with the first piece.
        let mut whole = length.to_be_bytes().to_vec();
        let room = PIECE - whole.len();
        let (first, rest) = message.split_at(message.len().min(room));
        whole.extend_from_slice(first);
        for piece in std::iter::once(&whole[..]).chain(rest.chunks(PIECE)) {
            let length = self.keys.0.write_message(self.said, piece, &mut self.piece)?;
            self.said += 1;
            write_frame(&mut self.stream, &self.piece[..length])?;
        }
        Ok(self.stream.flush()?)
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
            read_frame(&mut self.stream, &mut self.frame)?;
            let length = self.keys.0.read_message(self.heard, &self.frame, &mut self.piece)?;
            self.heard += 1;
            self.pending.extend_from_slice(&self.piece[..length]);
        }
    }

    /// The socket, for whoever ends the connection from another thread.
    pub fn socket(&self) -> io::Result<TcpStream> {
        self.stream.get_ref().try_clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Two ends of a connection on this machine, each made secure with
    /// its key on a thread of its own.
    fn pair(ours: [u8; 32], theirs: [u8; 32]) -> (Result<(Sayer, Hearer), Broken>, Result<(Sayer, Hearer), Broken>) {
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
        assert!(ours.is_err(), "and the side that connects is left with nothing");

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
    }
}
