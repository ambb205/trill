//! What two programs that hold the same song say to each other.

use serde::{Deserialize, Serialize};
use trill_core::files::Content;
use trill_core::merge::{Floors, Replica, Said, SongId};
use trill_core::{ClipId, TrackId};

/// The version of what is said here. Two programs that speak different
/// ones do not guess at each other: the one that hears the other's says
/// so, in a sentence, and lets go.
///
/// 2: a take is said (a clip that is kept beside its track, in a lane),
/// and the order of a song's envelopes.
///
/// 3: the order of a track's sends is said.
///
/// 4: a file is known by what it is. Where a clip's file lies is no
/// longer said by anyone, and a program that says it is let go of; what
/// the file is (its hash, its length, its name) is a property of the
/// clip, and the bytes are asked for by it ([`Message::Want`]).
pub const PROTOCOL: u32 = 4;

/// The most one message may be: a whole song with its plugins' states.
pub const MAX_MESSAGE: usize = 256 << 20;
/// The longest name a person goes by, and the longest saying of what they
/// hold.
pub const MAX_NAME: usize = 64;
/// How many things of a kind one person's presence names.
pub const MAX_HELD: usize = 512;
/// The most bytes of a file one piece carries: with what says which bytes
/// they are, one piece is one frame of the connection, so that whatever
/// is to be said ahead of a file waits for one frame at the most.
pub const MAX_PIECE: usize = 48 << 10;
/// The longest stretch of a file that is asked for at once.
pub const MAX_WANT: u64 = 4 << 20;
/// How many files one saying of what a program has names.
pub const MAX_HAVE: usize = 4096;
/// What a piece begins with: no message written as text begins so.
const PIECE: u8 = 0;
/// What a piece carries before its bytes: the mark, the hash and the
/// length of the file, and where in it the bytes lie.
const PIECE_HEAD: usize = 1 + 32 + 8 + 8;

/// Someone who has the song open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// By lot, once a run of their program: which person a presence is of.
    pub id: u64,
    /// What they go by.
    pub name: String,
    /// Which of the colours people are told apart by is theirs.
    pub colour: u8,
}

/// Where someone is in the song and what they hold: theirs alone, never
/// part of the song or of anyone's journal, and gone when they are.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub person: Person,
    /// Counts up with every saying: an older one that arrives late, by
    /// another way, is not taken for news.
    pub seq: u64,
    /// Where their song stands or plays, in beats.
    pub beat: f64,
    pub playing: bool,
    /// What they have selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<TrackId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<ClipId>,
    /// Notes, each by its clip and its id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<(ClipId, u64)>,
    /// What they have hold of right now, in words: "the volume of Bass".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holding: Option<String>,
}

impl Presence {
    /// Someone who is there and holds nothing.
    pub fn of(person: Person) -> Self {
        Self {
            person,
            seq: 0,
            beat: 0.0,
            playing: false,
            tracks: Vec::new(),
            clips: Vec::new(),
            notes: Vec::new(),
            holding: None,
        }
    }

    /// The presence held to what one may be: a name of a length, a
    /// selection of a size, a place in a song. What is too long is cut,
    /// and what is no number is none.
    pub fn bounded(mut self) -> Self {
        let cut = |text: &mut String| {
            if text.len() > MAX_NAME {
                let end = (0..=MAX_NAME).rev().find(|end| text.is_char_boundary(*end)).unwrap_or(0);
                text.truncate(end);
            }
        };
        cut(&mut self.person.name);
        if let Some(holding) = &mut self.holding {
            cut(holding);
        }
        if !self.beat.is_finite() || self.beat < 0.0 {
            self.beat = 0.0;
        }
        self.tracks.truncate(MAX_HELD);
        self.clips.truncate(MAX_HELD);
        self.notes.truncate(MAX_HELD);
        self
    }
}

/// One thing said.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// The first thing either side says: what it speaks, who it is, and
    /// what it holds of which song.
    Hello {
        protocol: u32,
        /// The build, for a sentence about two that do not match.
        build: String,
        person: Person,
        /// The shared song it has open, if it has one.
        song: Option<SongId>,
        /// How much of everyone's changes to that song it holds.
        floors: Floors,
        /// Its own number among the programs that hold the song, once it
        /// has changed it.
        replica: Option<Replica>,
    },
    /// The whole song, for a program that has not got it: the text of a
    /// project file with what is held beside the song.
    Song { text: String },
    /// Changes: what the other lacked when they met, and from then on
    /// what is made or heard as it is. `floors` is how much the sender
    /// holds of everyone's once it has said these; `replica` its own
    /// number.
    Changes {
        said: Vec<Said>,
        floors: Floors,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replica: Option<Replica>,
    },
    /// Where someone is and what they hold: the sender, or someone the
    /// sender heard it of.
    Presence(Presence),
    /// Someone is no longer there.
    Gone { person: u64 },
    /// The connection is being ended, and why.
    Bye { why: String },
    /// Files of the song that the sender can hand over, each by what it
    /// is. Said of nothing the song does not play.
    Have { files: Vec<Content> },
    /// Asks for bytes of a file, by what the file is and never by where
    /// it lies: `len` of them from `at`. Answered with them in pieces, in
    /// order, or with [`Message::Nothing`].
    Want { file: Content, at: u64, len: u64 },
    /// The answer to a [`Message::Want`] that is not answered with bytes.
    /// It is the same whatever the reason (the song does not play such a
    /// file, the program has not got it, it is gone): nobody learns by
    /// asking what a program has.
    Nothing { file: Content },
    /// Bytes of a file that were asked for, from `at`. Written as bytes
    /// and not as text ([`Message::encode`]).
    #[serde(skip)]
    Piece { file: Content, at: u64, bytes: Vec<u8> },
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        if let Message::Piece { file, at, bytes } = self {
            let mut written = Vec::with_capacity(PIECE_HEAD + bytes.len());
            written.push(PIECE);
            // (One that is not written as a hash is written as none: it
            // is what nobody asked for, wherever it arrives.)
            written.extend_from_slice(&file.hash().unwrap_or([0; 32]));
            written.extend_from_slice(&file.bytes.to_le_bytes());
            written.extend_from_slice(&at.to_le_bytes());
            written.extend_from_slice(bytes);
            return written;
        }
        serde_json::to_vec(self).expect("a message is always written")
    }

    /// Reads what another program said. Whatever is no message is an
    /// error that says so; nothing here trusts the bytes for more than
    /// their shape, and the changes in them are held to the model's rules
    /// where they are taken in.
    pub fn decode(bytes: &[u8]) -> Result<Message, String> {
        if bytes.first() == Some(&PIECE) {
            let Some((head, rest)) = bytes.split_at_checked(PIECE_HEAD) else {
                return Err("what the other program said is a piece of a file cut short".to_owned());
            };
            if rest.is_empty() || rest.len() > MAX_PIECE {
                return Err(format!("a piece of a file of {} bytes: more than {MAX_PIECE}, or none", rest.len()));
            }
            let number = |at: usize| u64::from_le_bytes(head[at..at + 8].try_into().expect("eight bytes"));
            let hash: &[u8; 32] = head[1..33].try_into().expect("thirty-two bytes");
            let file = Content::new(hash, number(33));
            return Ok(Message::Piece { file, at: number(41), bytes: rest.to_vec() });
        }
        let message: Message = serde_json::from_slice(bytes)
            .map_err(|error| format!("what the other program said is no message: {error}"))?;
        // What names a file names it by a hash, and nothing else is read
        // out of it anywhere: a path has no place to be said in.
        let written = |file: &Content| match file.is_well_formed() {
            true => Ok(()),
            false => Err("what the other program said names a file by something that is no hash".to_owned()),
        };
        match &message {
            Message::Have { files } if files.len() > MAX_HAVE => {
                return Err(format!("it says it has {} files at once: more than {MAX_HAVE}", files.len()));
            }
            Message::Have { files } => files.iter().try_for_each(written)?,
            Message::Want { file, .. } | Message::Nothing { file } => written(file)?,
            _ => {}
        }
        Ok(message)
    }

    /// Whether this is bytes of a file, which wait behind everything else
    /// that is to be said.
    pub fn is_piece(&self) -> bool {
        matches!(self, Message::Piece { .. })
    }
}
