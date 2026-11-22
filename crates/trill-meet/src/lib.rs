//! A meeting point: a small program on a server by which two Trills that
//! cannot reach each other directly get a connection to one another.
//!
//! Someone at home is behind a router that lets nothing in. Both programs
//! can connect *out*, though, and so both connect to a meeting point, and
//! it passes what each says to the other. That is all it does, and three
//! rules hold it to that:
//!
//! **It can neither read nor join a song.** The two programs prove the
//! invitation to each other through it, end to end, by the handshake they
//! use on a connection of their own ([`secure`]), and everything after is
//! encrypted under keys only the two of them have. The meeting point
//! never has an invitation's secret: it knows a meeting by something
//! worked out one way from it ([`Meeting`]), and lets through to its
//! holder only who can answer a question that takes the invitation to
//! answer ([`Door`]). Whoever has not got the invitation cannot knock,
//! and whoever knocks is still not in.
//!
//! **Only someone its keeper gave a key can open a meeting on it; joining
//! needs the invitation and nothing else.** A key ([`Key`]) never crosses
//! the network: whoever opens a meeting says which key theirs is and
//! proves the rest by the same handshake, keyed with the key. The keeper's
//! file is a line a person ([`keys`]), so that one person's can be taken
//! away.
//!
//! **Whatever reaches it is a stranger's, and bounded** ([`Limits`]): how
//! many connections at once and from one address, how many meetings and
//! how many people in one, how long anyone has to say what they came for,
//! how long a silent connection is kept, how many bytes a key may pass in
//! a day. No room is made by what a stranger says a length is. One
//! connection's fault is a line in the log and never the server's end. A
//! limit that turned someone away is a line in the log and a sentence in
//! the program that was turned away ([`No::sentence`]). The log holds no
//! key, no secret and nothing of what is passed.
//!
//! [`said`] is what is said to it; [`server`] is the meeting point;
//! [`client`] is the half a Trill speaks. The crate has nothing of Trill
//! in it, no audio and no window, and builds for a server as one file.

pub mod client;
pub mod key;
pub mod keys;
pub mod limits;
pub mod said;
pub mod secure;
pub mod server;
pub mod text;

/// The port a meeting point listens on unless its keeper says another,
/// and so the one an address without a port is taken to mean.
pub const PORT: u16 = 8745;

pub use client::{Holding, Turned};
pub use key::{Door, Key, KeyId, Meeting, Ticket};
pub use keys::Keys;
pub use limits::Limits;
pub use said::No;
pub use server::{Counts, Log, Server};
