//! What is said to a meeting point, and what it answers.
//!
//! Every connection begins with one line in the clear from whoever
//! connected, which says what they came for:
//!
//! ```text
//! trill-meet 1 open KEY-ID      someone with a key, to hold a meeting open
//! trill-meet 1 join MEETING     someone with an invitation, to be let in
//! trill-meet 1 take TICKET      the holder of a meeting, taking a knock up
//! ```
//!
//! and the meeting point answers with one line, `ok` or `no WORD`
//! ([`No`]). What follows depends on what was come for:
//!
//! * **open**: after `ok` both sides do the handshake of
//!   [`crate::secure`], keyed with what the key comes to; only someone
//!   who has the key gets through it. Over the connection that results,
//!   a few words at a time ([`Word`]): the holder says which meeting
//!   (`open MEETING`), is answered `ok` or `no WORD`, and from then on
//!   is told of every knock (`knock TICKET`). It says `ping` now and
//!   then and is answered `pong`, and `close` when it stops sharing.
//! * **join**: the meeting point tells the holder that someone knocked
//!   and waits a few seconds for the holder to take the knock up. Then
//!   it answers `ok`, and from there on the connection is a pipe to the
//!   holder: every byte either says is passed to the other, and the
//!   meeting point knows nothing of what they are.
//! * **take**: answered `ok`, and from there on the other end of that
//!   pipe.
//!
//! Through the pipe the two programs do what two programs that are
//! connected directly do: prove the invitation's secret to each other
//! and encrypt everything after ([`crate::secure`] again, keyed with the
//! invitation). The meeting point is not a party to that. It never has
//! the secret, and what it knows the meeting by cannot be worked back to
//! it ([`Meeting`]).
//!
//! Nothing may follow a line until it has been answered, and a line is
//! at most [`MAX_LINE`] bytes: no room is made for anything a stranger
//! says before the meeting point knows what they came for.

use crate::key::{KeyId, Meeting, Ticket};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// The version of what is said here: the number in every first line.
pub const VERSION: u32 = 1;
/// The most bytes a line is.
pub const MAX_LINE: usize = 160;
/// The most bytes one of the few words said over a holder's connection is.
pub const MAX_WORD: usize = 160;

/// What someone who connected came for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Hello {
    Open(KeyId),
    Join(Meeting),
    Take(Ticket),
}

impl Hello {
    /// The line, without its end.
    pub fn line(&self) -> String {
        match self {
            Hello::Open(key) => format!("trill-meet {VERSION} open {key}"),
            Hello::Join(meeting) => format!("trill-meet {VERSION} join {meeting}"),
            Hello::Take(ticket) => format!("trill-meet {VERSION} take {ticket}"),
        }
    }

    /// Reads a first line. What is no first line at all is `No::What`;
    /// one of another version is `No::Version`.
    pub fn read(line: &str) -> Result<Hello, No> {
        let mut words = line.split(' ');
        let (Some("trill-meet"), Some(version), Some(what), Some(of), None) =
            (words.next(), words.next(), words.next(), words.next(), words.next())
        else {
            return Err(No::What);
        };
        if version.parse::<u32>().map_err(|_| No::What)? != VERSION {
            return Err(No::Version);
        }
        match what {
            "open" => KeyId::read(of).map(Hello::Open),
            "join" => Meeting::read(of).map(Hello::Join),
            "take" => Ticket::read(of).map(Hello::Take),
            _ => None,
        }
        .ok_or(No::What)
    }
}

/// One of the few words said over a holder's connection, either way.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Word {
    /// The holder: this is the meeting I hold.
    Open(Meeting),
    /// The holder: I am still here.
    Ping,
    /// The holder: the meeting is over; whoever asks for it is told so.
    Close,
    Ok,
    No(No),
    Pong,
    /// The meeting point: someone asked for your meeting.
    Knock(Ticket),
}

impl Word {
    pub fn text(&self) -> String {
        match self {
            Word::Open(meeting) => format!("open {meeting}"),
            Word::Ping => "ping".to_owned(),
            Word::Close => "close".to_owned(),
            Word::Ok => "ok".to_owned(),
            Word::No(no) => format!("no {}", no.word()),
            Word::Pong => "pong".to_owned(),
            Word::Knock(ticket) => format!("knock {ticket}"),
        }
    }

    pub fn read(text: &[u8]) -> Option<Word> {
        let text = std::str::from_utf8(text).ok()?;
        let (what, of) = text.split_once(' ').unwrap_or((text, ""));
        Some(match (what, of) {
            ("open", of) => Word::Open(Meeting::read(of)?),
            ("ping", "") => Word::Ping,
            ("close", "") => Word::Close,
            ("ok", "") => Word::Ok,
            ("no", of) => Word::No(No::read(of)?),
            ("pong", "") => Word::Pong,
            ("knock", of) => Word::Knock(Ticket::read(of)?),
            _ => return None,
        })
    }
}

/// Why a meeting point turned someone away: one word on the wire, and a
/// sentence for whoever was turned away.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum No {
    /// What was said is no first line, or no word that belongs there.
    What,
    /// The meeting point speaks another version.
    Version,
    /// It did not say what it came for in time.
    Late,
    /// As many connections as the meeting point takes at once.
    Full,
    /// As many connections from one address as it takes at once.
    Crowd,
    /// As many new connections from one address as it takes in a minute.
    Often,
    /// The key is none its keeper gave, or was taken away.
    Key,
    /// As many meetings as it holds at once.
    Meetings,
    /// As many meetings as one key may hold at once.
    Yours,
    /// A meeting by this name is held under another key.
    Taken,
    /// No meeting goes by that name.
    Unknown,
    /// Whoever holds the meeting is not connected at the moment.
    Away,
    /// Whoever held the meeting ended it.
    Closed,
    /// As many people as a meeting has.
    Seats,
    /// Whoever holds the meeting did not take the knock up in time.
    Slow,
    /// No knock goes by that ticket: it was taken up, or its time is over.
    Ticket,
    /// The key has passed as many bytes as it may in a day.
    Spent,
}

impl No {
    const ALL: [No; 17] = [
        No::What,
        No::Version,
        No::Late,
        No::Full,
        No::Crowd,
        No::Often,
        No::Key,
        No::Meetings,
        No::Yours,
        No::Taken,
        No::Unknown,
        No::Away,
        No::Closed,
        No::Seats,
        No::Slow,
        No::Ticket,
        No::Spent,
    ];

    /// The word on the wire.
    pub fn word(&self) -> &'static str {
        match self {
            No::What => "what",
            No::Version => "version",
            No::Late => "late",
            No::Full => "full",
            No::Crowd => "crowd",
            No::Often => "often",
            No::Key => "key",
            No::Meetings => "meetings",
            No::Yours => "yours",
            No::Taken => "taken",
            No::Unknown => "unknown",
            No::Away => "away",
            No::Closed => "closed",
            No::Seats => "seats",
            No::Slow => "slow",
            No::Ticket => "ticket",
            No::Spent => "spent",
        }
    }

    pub fn read(word: &str) -> Option<No> {
        No::ALL.into_iter().find(|no| no.word() == word)
    }

    /// In a sentence, for whoever was turned away: what happened and
    /// what there is to do about it.
    pub fn sentence(&self) -> &'static str {
        match self {
            No::What => {
                "the meeting point did not understand what this program said to it: the two are builds that do not fit"
            }
            No::Version => {
                "the meeting point speaks another version than this program: one of the two needs a newer build"
            }
            No::Late => "the meeting point waited for this program to say what it came for, and it did not in time",
            No::Full => {
                "the meeting point has as many connections as it takes at once: try again in a moment, or ask its \
                 keeper to allow more"
            }
            No::Crowd => {
                "the meeting point has as many connections from this address as it takes at once: someone on the \
                 same network may be using it too"
            }
            No::Often => {
                "the meeting point was asked too often from this address in the last minute: it takes this program \
                 again in a moment"
            }
            No::Key => {
                "the meeting point did not take the key: it is not one its keeper gave, or the keeper has taken it \
                 away. The key is set where the song is shared"
            }
            No::Meetings => {
                "the meeting point holds as many meetings as it takes at once: try again later, or ask its keeper \
                 to allow more"
            }
            No::Yours => {
                "this key holds as many meetings at once as one key may: stop sharing a song that is shared with \
                 it elsewhere, or ask the keeper to allow more"
            }
            No::Taken => "the meeting point has a meeting by this invitation that someone else holds",
            No::Unknown => {
                "nobody is sharing a song by this invitation at the meeting point: the invitation is no longer \
                 good, or whoever shares has not got their Trill open"
            }
            No::Away => {
                "whoever shares the song is not connected to the meeting point at the moment: their connection \
                 dropped, or their computer sleeps"
            }
            No::Closed => "whoever shared the song stopped sharing it: the invitation is no longer good",
            No::Seats => "the song has as many people in it as a meeting takes",
            No::Slow => {
                "whoever shares the song did not answer the meeting point in time: their connection may be slow \
                 or gone"
            }
            No::Ticket => "the meeting point no longer waits for this program to take that knock up",
            No::Spent => {
                "the key has passed as much through the meeting point as it may in a day: it takes the song again \
                 tomorrow, or its keeper allows more"
            }
        }
    }
}

/// A socket read until a moment and no longer: a time limit on the
/// socket alone is a limit on each read, and someone who said their line
/// a byte at a time would be waited for as long as they cared to go on.
pub struct Until<'a> {
    pub stream: &'a TcpStream,
    pub end: Instant,
}

impl Read for Until<'_> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let left = self.end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "it was not said in time"));
        }
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        stream.read(into)
    }
}

/// Why a line did not arrive.
#[derive(Debug)]
pub enum NoLine {
    /// Not said, whole, by `end`.
    Late,
    /// More than [`MAX_LINE`] bytes, or bytes that are no text.
    NoLine,
    /// The other side went away.
    Gone(io::Error),
}

/// Reads one line from `stream`, a byte at a time and none beyond its
/// end (what follows a line may be a pipe's, and nobody else's to read),
/// and no later than `end`.
pub fn hear_line(stream: &TcpStream, end: Instant) -> Result<String, NoLine> {
    let mut hearing = Until { stream, end };
    let mut line = [0u8; MAX_LINE];
    let mut length = 0;
    loop {
        let mut byte = [0u8; 1];
        match hearing.read(&mut byte) {
            Ok(0) => return Err(NoLine::Gone(io::ErrorKind::UnexpectedEof.into())),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if length == MAX_LINE => return Err(NoLine::NoLine),
            Ok(_) => {
                line[length] = byte[0];
                length += 1;
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {
                return Err(NoLine::Late);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(NoLine::Gone(error)),
        }
    }
    match std::str::from_utf8(&line[..length]) {
        // Printable and nothing else: what is said here is words.
        Ok(text) if text.bytes().all(|byte| (b' '..0x7f).contains(&byte)) => Ok(text.to_owned()),
        _ => Err(NoLine::NoLine),
    }
}

/// Says one line, whole, within `within`.
pub fn say_line(stream: &TcpStream, line: &str, within: Duration) -> io::Result<()> {
    stream.set_write_timeout(Some(within.max(Duration::from_millis(1))))?;
    let mut wire = stream;
    wire.write_all(format!("{line}\n").as_bytes())
}

/// The answer to a first line: `ok`, or why not.
pub fn answer(heard: &str) -> Option<Result<(), No>> {
    match heard.split_once(' ') {
        None if heard == "ok" => Some(Ok(())),
        Some(("no", word)) => No::read(word).map(Err),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::Key;

    #[test]
    fn a_first_line_says_what_was_come_for_and_what_is_no_first_line_is_none() {
        let (key, meeting, ticket) = (Key::new().unwrap().id(), Meeting::of(b"secret"), Ticket::new().unwrap());
        for hello in [Hello::Open(key), Hello::Join(meeting), Hello::Take(ticket)] {
            let line = hello.line();
            assert!(line.len() <= MAX_LINE && line.starts_with("trill-meet 1 "), "{line}");
            assert_eq!(Hello::read(&line), Ok(hello));
        }
        assert_eq!(Hello::read(&format!("trill-meet 2 join {meeting}")), Err(No::Version));
        for wrong in [
            "",
            "GET / HTTP/1.1",
            "trill-meet",
            "trill-meet 1",
            "trill-meet one join 00",
            &format!("trill-meet 1 join {meeting} and more"),
            &format!("trill-meet 1 join {}", &meeting.to_string()[..60]),
            &format!("trill-meet 1 open {meeting}"),
            &format!("trill-meet 1 dance {meeting}"),
            &format!("trill-meet  1 join {meeting}"),
        ] {
            assert_eq!(Hello::read(wrong), Err(No::What), "{wrong}");
        }
    }

    #[test]
    fn the_few_words_of_a_holder_are_read_back_and_every_refusal_has_a_word_and_a_sentence() {
        let (meeting, ticket) = (Meeting::of(b"secret"), Ticket::new().unwrap());
        let words = [Word::Open(meeting), Word::Ping, Word::Close, Word::Ok, Word::Pong, Word::Knock(ticket)];
        for word in words.into_iter().chain(No::ALL.map(Word::No)) {
            let text = word.text();
            assert!(text.len() <= MAX_WORD, "{text}");
            assert_eq!(Word::read(text.as_bytes()), Some(word), "{text}");
        }
        for wrong in ["", "ping pong", "open", "open 00", "no", "no idea", "knock knock", "\u{1}"] {
            assert_eq!(Word::read(wrong.as_bytes()), None, "{wrong}");
        }
        assert_eq!(Word::read(&[0xff, 0xfe]), None);
        let mut seen = std::collections::HashSet::new();
        for no in No::ALL {
            assert!(seen.insert(no.word()), "{} twice", no.word());
            assert_eq!(No::read(no.word()), Some(no));
            assert_eq!(answer(&format!("no {}", no.word())), Some(Err(no)));
            let sentence = no.sentence();
            assert!(sentence.len() > 40 && !sentence.ends_with('.'), "{sentence}");
        }
        assert_eq!(answer("ok"), Some(Ok(())));
        assert_eq!(answer("ok then"), None);
        assert_eq!(answer("no"), None);
        assert_eq!(answer("HTTP/1.1 400 Bad Request"), None);
    }
}
