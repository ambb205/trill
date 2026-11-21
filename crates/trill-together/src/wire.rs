//! What two programs that hold the same song say to each other.

use serde::{Deserialize, Serialize};
use trill_core::merge::{Floors, Replica, Said, SongId};
use trill_core::{ClipId, TrackId};

/// The version of what is said here. Two programs that speak different
/// ones do not guess at each other: the one that hears the other's says
/// so, in a sentence, and lets go.
pub const PROTOCOL: u32 = 1;

/// The most one message may be: a whole song with its plugins' states.
pub const MAX_MESSAGE: usize = 256 << 20;
/// The longest name a person goes by, and the longest saying of what they
/// hold.
pub const MAX_NAME: usize = 64;
/// How many things of a kind one person's presence names.
pub const MAX_HELD: usize = 512;

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
        Self { person, seq: 0, beat: 0.0, playing: false, tracks: Vec::new(), clips: Vec::new(), notes: Vec::new(), holding: None }
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
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a message is always written")
    }

    /// Reads what another program said. Whatever is no message is an
    /// error that says so; nothing here trusts the bytes for more than
    /// their shape, and the changes in them are held to the model's rules
    /// where they are taken in.
    pub fn decode(bytes: &[u8]) -> Result<Message, String> {
        serde_json::from_slice(bytes).map_err(|error| format!("what the other program said is no message: {error}"))
    }
}
