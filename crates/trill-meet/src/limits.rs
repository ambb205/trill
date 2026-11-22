//! How much a meeting point takes: whatever reaches it is a stranger's,
//! and bounded.

use std::time::Duration;

/// The limits of a meeting point. The defaults suit a small server (one
/// processor, half a gigabyte of memory); a keeper sets any of them when
/// the program is started.
///
/// One that turned someone away is a line in the meeting point's log and
/// a sentence in the program that was turned away ([`crate::No`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// How many connections it has at once, of every kind. A meeting of
    /// `n` people is `2n - 1` of them: one by which its holder holds it,
    /// and two for each of the others (theirs, and the holder's to them).
    pub connections: usize,
    /// How many of them from one address (for IPv6: from one network of
    /// 2^64 addresses, which is what one household is given). A holder's
    /// are all from its own, so this is at least twice [`Limits::people`].
    pub from_one_address: usize,
    /// How many new connections one address may make in a minute.
    pub a_minute_from_one_address: u32,
    /// How many meetings it holds at once.
    pub meetings: usize,
    /// How many of them one key may hold.
    pub meetings_of_one_key: usize,
    /// How many people a meeting has, its holder among them.
    pub people: usize,
    /// How long anyone has to say what they came for: the first line,
    /// and for a holder the whole of proving the key and naming the
    /// meeting.
    pub hello: Duration,
    /// How long a knock waits for the meeting's holder to take it up.
    pub answer: Duration,
    /// How long a connection over which nothing passes is kept. (Two
    /// Trills say something to each other every second, and a holder says
    /// it is there every twenty.)
    pub silent: Duration,
    /// How long a meeting is remembered after its holder's connection
    /// was lost without a word: whoever asks for it meanwhile is told
    /// that the holder is away, and not that there is no such meeting.
    pub away: Duration,
    /// How many bytes the meetings of one key may pass in a day, both
    /// ways together. (By the key and not by the meeting: a new
    /// invitation is a new meeting.)
    pub bytes_a_day: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 512,
            from_one_address: 48,
            a_minute_from_one_address: 240,
            meetings: 64,
            meetings_of_one_key: 4,
            people: 16,
            hello: Duration::from_secs(10),
            answer: Duration::from_secs(10),
            silent: Duration::from_secs(120),
            away: Duration::from_secs(600),
            bytes_a_day: 2 << 30,
        }
    }
}

impl Limits {
    /// Holds what a keeper set to what a meeting point can run with: a
    /// limit that would turn everyone away, or keep nobody, is said
    /// before anything listens.
    pub fn held_to(&self) -> Result<(), String> {
        let wrong = |what: &str| Err(format!("{what}: see trill-meet help"));
        if self.connections < 3 || self.from_one_address < 3 || self.a_minute_from_one_address < 3 {
            return wrong("a meeting of two is three connections, two of them from one address");
        }
        if self.meetings == 0 || self.meetings_of_one_key == 0 || self.people < 2 {
            return wrong("a meeting point holds at least one meeting, of at least two people");
        }
        if self.answer > Duration::from_secs(20) {
            return wrong("whoever asks for a meeting waits half a minute for it, so --answer is at most 20");
        }
        if self.silent < Duration::from_secs(30) {
            return wrong("whoever holds a meeting says so every twenty seconds, so --silent is at least 30");
        }
        if self.bytes_a_day < 1 << 20 {
            return wrong("--megabytes-a-day is at least 1");
        }
        Ok(())
    }

    /// How many files the meeting point can come to have open at these
    /// limits: one a connection, the few that were turned away a moment
    /// ago, and what a program has open anyway.
    pub fn files(&self) -> usize {
        self.connections + 64 + 16
    }

    /// In words, for the first lines of the log: what the meeting point
    /// was started with.
    pub fn said(&self) -> String {
        format!(
            "{} connections at once, {} from one address and {} new ones a minute; {} meetings, {} of one key, {} \
             people in one; {} s to say what one came for, {} s for a knock to be taken up, {} s of silence, a \
             holder that is away remembered for {} s; {} MB a day through the meetings of one key",
            self.connections,
            self.from_one_address,
            self.a_minute_from_one_address,
            self.meetings,
            self.meetings_of_one_key,
            self.people,
            self.hello.as_secs(),
            self.answer.as_secs(),
            self.silent.as_secs(),
            self.away.as_secs(),
            self.bytes_a_day >> 20
        )
    }
}
