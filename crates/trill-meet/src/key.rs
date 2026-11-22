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

/// What whoever asks for a meeting has to show that they have: worked
/// out one way from the invitation's secret, under a label of its own.
///
/// What a meeting goes by ([`Meeting`]) is said in the clear by whoever
/// asks for it, so someone who listens on the way has it. With that
/// alone nobody knocks: the meeting point puts a question, a number by
/// lot ([`Question`]), and lets through only who answers with what that
/// number and this come to ([`Door::answer`]). The holder hands this to
/// the meeting point when it opens the meeting, over the connection its
/// key made, which nobody reads along. Whoever has only this (the
/// meeting point) can answer the question and no more: neither the
/// secret nor what the two programs prove to each other can be worked
/// back from it.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Door([u8; 32]);

impl Door {
    pub fn of(secret: &[u8]) -> Door {
        let mut hash = Sha256::new();
        hash.update(b"trill meeting point, door 1");
        hash.update(secret);
        Door(hash.finalize().into())
    }

    pub fn read(text: &str) -> Option<Door> {
        unhex(text).map(Door)
    }

    /// A door nobody has the invitation for: what an answer is held
    /// against where there is no such meeting, so that being told so
    /// takes as long as being told so of one that there is.
    pub fn nobodys() -> std::io::Result<Door> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Door(bytes))
    }

    /// What `question` and this come to: a hash of both, hashed again
    /// with this, so that neither can be had from the answer and an
    /// answer says nothing of the answer to another question.
    pub fn answer(&self, question: &Question) -> Answer {
        let mut inner = Sha256::new();
        inner.update(b"trill meeting point, answer 1");
        inner.update(self.0);
        inner.update(question.0);
        let inner: [u8; 32] = inner.finalize().into();
        let mut outer = Sha256::new();
        outer.update(b"trill meeting point, answer 1, closed");
        outer.update(self.0);
        outer.update(inner);
        Answer(outer.finalize().into())
    }
}

impl fmt::Display for Door {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for Door {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Door")
    }
}

/// What whoever asks for a meeting is asked: a number by lot, new for
/// every connection, so that an answer heard once is no answer again.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Question([u8; 16]);

impl Question {
    pub fn new() -> std::io::Result<Question> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Question(bytes))
    }

    pub fn read(text: &str) -> Option<Question> {
        unhex(text).map(Question)
    }
}

impl fmt::Display for Question {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for Question {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Question")
    }
}

/// What a question and a meeting's door come to.
#[derive(Copy, Clone)]
pub struct Answer([u8; 32]);

impl Answer {
    pub fn read(text: &str) -> Option<Answer> {
        unhex(text).map(Answer)
    }

    /// Whether this is `other`, found out in the same time whichever
    /// byte is the first that is not.
    pub fn is(&self, other: &Answer) -> bool {
        self.0.iter().zip(other.0).fold(0u8, |differing, (one, other)| differing | (one ^ other)) == 0
    }
}

impl fmt::Display for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl fmt::Debug for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Answer")
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

    #[test]
    fn a_question_is_answered_by_who_has_the_door_and_an_answer_is_no_answer_to_another() {
        let secret = b"a secret of twenty b.";
        let (door, other) = (Door::of(secret), Door::of(b"a secret of twenty c."));
        // The door is the secret's, and is neither what the meeting goes by nor read back from.
        assert_eq!(Door::of(secret), door);
        assert_ne!(door, other);
        assert_ne!(door.to_string(), Meeting::of(secret).to_string());
        assert_eq!(Door::read(&door.to_string()), Some(door));
        assert_eq!(format!("{door:?}"), "Door");
        let (first, second) = (Question::new().unwrap(), Question::new().unwrap());
        assert_ne!(first, second, "a question is by lot");
        assert_eq!(Question::read(&first.to_string()), Some(first));
        // Whoever has the door answers; the same question has the same answer.
        let answer = door.answer(&first);
        assert!(answer.is(&door.answer(&first)));
        assert!(Answer::read(&answer.to_string()).unwrap().is(&answer));
        // An answer is none to another question, and none with another door.
        assert!(!answer.is(&door.answer(&second)));
        assert!(!answer.is(&other.answer(&first)));
        assert!(!Door::nobodys().unwrap().answer(&first).is(&answer));
        // And it is neither the door nor the question.
        let shown = answer.to_string();
        assert!(shown != door.to_string() && !shown.contains(&first.to_string()));
        assert_eq!((Answer::read("00").is_none(), format!("{answer:?}")), (true, "Answer".to_owned()));
    }
}
