//! What a meeting point knows people and meetings by: a keeper's key, the
//! name a meeting goes by, and the ticket of a knock.

use crate::text::{hex, spell, unhex, unspell};
use sha2::{Digest, Sha256};
use std::fmt;

/// How many bytes a key is: five that say which key it is, and twenty of
/// secret by lot.
const KEY_BYTES: usize = 25;
const ID_BYTES: usize = 5;

/// Which key: the part of a key that is no secret, said in the clear when
/// a meeting is opened so that the meeting point knows whose secret to
/// hold the rest against.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId([u8; ID_BYTES]);

impl KeyId {
    /// As it is written: eight letters.
    pub fn read(text: &str) -> Option<KeyId> {
        Some(KeyId(unspell(text, ID_BYTES).ok()?.try_into().ok()?))
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&spell(&self.0))
    }
}

impl fmt::Debug for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyId({self})")
    }
}

/// A key a meeting point's keeper gave someone: whoever has it can open
/// meetings there. It is text to paste once, ten groups of four letters.
///
/// It never crosses the network. Whoever opens a meeting says which key
/// theirs is ([`KeyId`]) and proves the rest by a handshake that only
/// someone who has it can finish ([`crate::secure`]); the meeting point
/// keeps what the handshake is held against ([`Key::proof`]) and not the
/// key as it was handed over.
#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; KEY_BYTES]);

impl Key {
    /// A new key, by lot from the system's own source.
    pub fn new() -> std::io::Result<Key> {
        let mut bytes = [0u8; KEY_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Key(bytes))
    }

    /// Reads a key as it was handed over: spaces and dashes do not
    /// matter, nor whether letters are large or small.
    pub fn parse(text: &str) -> Result<Key, String> {
        match unspell(text, KEY_BYTES) {
            Ok(bytes) => Ok(Key(bytes.try_into().expect("as many bytes as were asked for"))),
            Err(_) => {
                Err("a meeting point's key is ten groups of four letters, as its keeper handed it over".to_owned())
            }
        }
    }

    pub fn id(&self) -> KeyId {
        KeyId(self.0[..ID_BYTES].try_into().expect("five bytes"))
    }

    /// What the handshake that opens a meeting is keyed with: worked out
    /// from the key, so that the key itself is never used as one, and so
    /// that the meeting point's file need not hold the key.
    pub fn proof(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"trill meeting point, key 1");
        hash.update(self.0);
        hash.finalize().into()
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&spell(&self.0))
    }
}

impl fmt::Debug for Key {
    /// Which key, and nothing of its secret: a key in a log is nobody's
    /// to use.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({})", self.id())
    }
}

/// What a meeting point knows a meeting by: worked out one way from the
/// secret of the invitation the meeting is for, and from nothing else.
///
/// Whoever has the invitation can work it out, and so can ask for the
/// meeting. Whoever has only this (the meeting point; someone who listens
/// on the way) can knock, and no more: the secret cannot be worked back
/// from it, and what the two programs prove to each other through the
/// meeting point is the secret.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Meeting([u8; 32]);

impl Meeting {
    pub fn of(secret: &[u8]) -> Meeting {
        let mut hash = Sha256::new();
        hash.update(b"trill meeting point, meeting 1");
        hash.update(secret);
        Meeting(hash.finalize().into())
    }

    pub fn read(text: &str) -> Option<Meeting> {
        unhex(text).map(Meeting)
    }

    /// A few of its digits, for a log: enough to tell two meetings apart
    /// in what a keeper reads, and too few to knock with.
    pub fn tag(&self) -> String {
        hex(&self.0[..3])
    }
}

impl fmt::Display for Meeting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for Meeting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Meeting({}..)", self.tag())
    }
}

/// What whoever holds a meeting is handed when someone knocks: by lot,
/// good once and for a few seconds, for the one connection that takes the
/// knock up.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct Ticket([u8; 16]);

impl Ticket {
    pub fn new() -> std::io::Result<Ticket> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Ticket(bytes))
    }

    pub fn read(text: &str) -> Option<Ticket> {
        unhex(text).map(Ticket)
    }
}

impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ticket")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_text_that_reads_back_and_says_nothing_of_itself_in_a_log() {
        let key = Key::new().unwrap();
        let text = key.to_string();
        assert_eq!(text.len(), 40 + 9, "ten groups of four letters: {text}");
        assert_eq!(Key::parse(&text).unwrap(), key);
        assert_eq!(Key::parse(&text.to_lowercase().replace('-', " ")).unwrap(), key);
        assert_ne!(Key::new().unwrap().proof(), key.proof(), "a key is by lot");
        // Which key it is is its first eight letters, and that is all a log has.
        assert_eq!(key.id().to_string(), text[..9]);
        assert_eq!(KeyId::read(&text[..9]), Some(key.id()));
        let shown = format!("{key:?}");
        assert_eq!(shown, format!("Key({})", key.id()));
        assert!(!shown.contains(&text[10..]));
        for wrong in ["", "0000-0000", &text[..text.len() - 1], &format!("{text}0"), &text.replace(&text[..1], "!")] {
            let refusal = Key::parse(wrong).expect_err(wrong);
            assert!(refusal.contains("ten groups of four letters"), "{refusal}");
        }
    }

    #[test]
    fn a_meeting_is_known_by_what_the_secret_comes_to_and_not_by_the_secret() {
        let (one, other) = (Meeting::of(b"a secret of twenty b."), Meeting::of(b"a secret of twenty c."));
        assert_ne!(one, other);
        assert_eq!(Meeting::of(b"a secret of twenty b."), one);
        let text = one.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(Meeting::read(&text), Some(one));
        assert_eq!(Meeting::read(&text[..63]), None);
        assert_eq!(one.tag().len(), 6);
        assert_eq!(format!("{one:?}"), format!("Meeting({}..)", &text[..6]));
        let ticket = Ticket::new().unwrap();
        assert_eq!(Ticket::read(&ticket.to_string()), Some(ticket));
        assert_ne!(Ticket::new().unwrap(), ticket);
        assert_eq!(format!("{ticket:?}"), "Ticket");
    }
}
