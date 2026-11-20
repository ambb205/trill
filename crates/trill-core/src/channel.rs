//! A MIDI channel: which of sixteen a note, a wheel, a pressure or a
//! controller is on.
//!
//! Most instruments take all sixteen as one, and a song for them never
//! says a channel: everything is on the first. Some give the channels
//! meanings of their own (an organ's pedalboard on the second; a guitar
//! with a string to a channel, each with a wheel of its own), and for
//! those a note says which channel it is on and a clip keeps a curve for
//! each controller on each channel.
//!
//! Wherever a person or a model reads or writes a channel it is a number
//! from 1 to 16, as on every instrument's panel, and that is what a
//! project file holds ([`Channel::number`]). On the wire to a plugin it is
//! 0 to 15 ([`Channel::index`]). A value of this type is one of the
//! sixteen and cannot be anything else, so the two countings do not get
//! mixed up.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One of the sixteen MIDI channels. The default is the first.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(u8);

impl Channel {
    /// The channel everything is on that says no other.
    pub const FIRST: Channel = Channel(0);
    /// How many channels there are.
    pub const COUNT: usize = 16;

    /// The channel with the number `number`, 1 to 16; `None` for any
    /// other number.
    pub fn new(number: u8) -> Option<Self> {
        (1..=Self::COUNT as u8).contains(&number).then(|| Channel(number - 1))
    }

    /// The channel at `index`, 0 to 15, as a plugin format counts them;
    /// `None` beyond the sixteenth.
    pub fn from_index(index: usize) -> Option<Self> {
        (index < Self::COUNT).then_some(Channel(index as u8))
    }

    /// Its number, 1 to 16: what is shown and written.
    pub fn number(self) -> u8 {
        self.0 + 1
    }

    /// Its place among the sixteen, 0 to 15: what goes to a plugin, and
    /// what a table of sixteen is indexed by.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    /// Whether it is the first: the one nothing needs to say.
    pub fn is_first(&self) -> bool {
        self.0 == 0
    }

    /// All sixteen, the first first.
    pub fn all() -> impl Iterator<Item = Channel> {
        (0..Self::COUNT as u8).map(Channel)
    }
}

impl std::fmt::Display for Channel {
    /// Its number: `3`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.number())
    }
}

impl Serialize for Channel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.number())
    }
}

impl<'de> Deserialize<'de> for Channel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let number = u64::deserialize(deserializer)?;
        let channel = u8::try_from(number).ok().and_then(Channel::new);
        channel.ok_or_else(|| serde::de::Error::custom(format!("a MIDI channel is 1 to 16, not {number}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_is_one_of_sixteen_counted_from_one_where_it_is_read() {
        assert_eq!(Channel::default(), Channel::FIRST);
        assert_eq!((Channel::FIRST.number(), Channel::FIRST.index()), (1, 0));
        let sixth = Channel::new(6).unwrap();
        assert_eq!((sixth.number(), sixth.index(), sixth.to_string().as_str()), (6, 5, "6"));
        assert_eq!(Channel::from_index(5), Some(sixth));
        assert_eq!((Channel::new(0), Channel::new(17), Channel::from_index(16)), (None, None, None));
        assert_eq!(Channel::all().count(), 16);
        assert!(Channel::FIRST.is_first() && !sixth.is_first() && Channel::FIRST < sixth);
    }

    #[test]
    fn a_channel_is_written_as_its_number_and_no_other_number_is_read_as_one() {
        assert_eq!(serde_json::to_string(&Channel::new(2).unwrap()).unwrap(), "2");
        assert_eq!(serde_json::from_str::<Channel>("16").unwrap(), Channel::new(16).unwrap());
        for wrong in ["0", "17", "300", "-1"] {
            assert!(serde_json::from_str::<Channel>(wrong).is_err(), "{wrong} was read as a channel");
        }
    }
}
