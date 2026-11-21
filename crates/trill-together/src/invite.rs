//! An invitation: where to connect, and the secret that says one may.

use sha2::{Digest, Sha256};
use std::fmt;

/// How many bytes of secret an invitation carries: 160 bits by lot.
const SECRET_BYTES: usize = 20;
/// The letters a secret is written in: no I, L, O or U, which are read as
/// something else.
const LETTERS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Where a program that shares a song listens, and the secret it lets a
/// program in by. It is text to hand to someone: `trill:HOST:PORT:SECRET`.
///
/// Whoever has it can read and change the song while it is shared, and
/// nobody else can: the secret never crosses the wire, each side proves
/// it has it, and everything said afterwards is encrypted under a key
/// that only the two of them have.
#[derive(Clone, PartialEq, Eq)]
pub struct Invitation {
    pub host: String,
    pub port: u16,
    secret: [u8; SECRET_BYTES],
}

impl Invitation {
    /// An invitation to `host` at `port`, with a secret by lot from the
    /// system's own source.
    pub fn new(host: impl Into<String>, port: u16) -> std::io::Result<Self> {
        let mut secret = [0u8; SECRET_BYTES];
        getrandom::fill(&mut secret).map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self { host: host.into(), port, secret })
    }

    /// The same invitation to another address of the same program: what
    /// someone on another machine is handed for one that was made for
    /// this one.
    pub fn at(&self, host: impl Into<String>) -> Self {
        Self { host: host.into(), ..self.clone() }
    }

    /// Reads an invitation as it was handed over: spaces and dashes in the
    /// secret do not matter, nor whether its letters are large or small.
    pub fn parse(text: &str) -> Result<Self, String> {
        let wrong = || "an invitation reads trill:HOST:PORT:SECRET, as the program that shares the song shows it";
        let text = text.trim();
        let rest = text.strip_prefix("trill:").ok_or_else(wrong)?;
        // From the right: an address may have colons of its own.
        let (rest, secret) = rest.rsplit_once(':').ok_or_else(wrong)?;
        let (host, port) = rest.rsplit_once(':').ok_or_else(wrong)?;
        let host = host.trim().trim_start_matches('[').trim_end_matches(']');
        let port: u16 = port.trim().parse().map_err(|_| format!("{port} is no port: {}", wrong()))?;
        if host.is_empty() || port == 0 {
            return Err(wrong().to_owned());
        }
        let mut bits: Vec<u8> = Vec::new();
        for letter in secret.chars().filter(|letter| !letter.is_whitespace() && *letter != '-') {
            let letter = match letter.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                letter => letter,
            };
            let value = LETTERS.iter().position(|known| *known as char == letter);
            let value = value.ok_or_else(|| format!("'{letter}' is in no invitation's secret: {}", wrong()))?;
            bits.extend((0..5).rev().map(|bit| (value >> bit) as u8 & 1));
        }
        if bits.len() != SECRET_BYTES * 8 {
            return Err(format!("the secret is not whole: {}", wrong()));
        }
        let mut read = [0u8; SECRET_BYTES];
        for (byte, eight) in read.iter_mut().zip(bits.chunks(8)) {
            *byte = eight.iter().fold(0, |byte, bit| byte << 1 | bit);
        }
        Ok(Self { host: host.to_owned(), port, secret: read })
    }

    /// The key both sides prove they have ([`secure`](crate::secure)):
    /// worked out from the secret, so that the secret itself is never
    /// used as one.
    pub fn key(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"trill together, protocol 1");
        hash.update(self.secret);
        hash.finalize().into()
    }

    /// `host:port`, as a socket takes it.
    pub fn address(&self) -> String {
        match self.host.contains(':') {
            true => format!("[{}]:{}", self.host, self.port),
            false => format!("{}:{}", self.host, self.port),
        }
    }
}

impl fmt::Display for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bits: Vec<u8> = self.secret.iter().flat_map(|byte| (0..8).rev().map(move |bit| byte >> bit & 1)).collect();
        let letters: Vec<char> =
            bits.chunks(5).map(|five| LETTERS[five.iter().fold(0usize, |value, bit| value << 1 | *bit as usize)] as char).collect();
        let groups: Vec<String> = letters.chunks(4).map(|group| group.iter().collect()).collect();
        let host = match self.host.contains(':') {
            true => format!("[{}]", self.host),
            false => self.host.clone(),
        };
        write!(f, "trill:{host}:{}:{}", self.port, groups.join("-"))
    }
}

impl fmt::Debug for Invitation {
    /// Without the secret: an invitation in a log is nobody's to use.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invitation({}:{})", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invitation_is_text_that_reads_back_as_it_was_and_two_are_never_the_same() {
        let made = Invitation::new("192.168.1.20", 47_600).unwrap();
        let text = made.to_string();
        assert!(text.starts_with("trill:192.168.1.20:47600:"), "{text}");
        assert_eq!(text.rsplit(':').next().unwrap().len(), 32 + 7, "thirty-two letters in groups of four: {text}");
        assert_eq!(Invitation::parse(&text).unwrap(), made);
        // As someone types it: small letters, spaces, an O for a zero.
        let (place, secret) = text.rsplit_once(':').unwrap();
        let typed = format!("{place}:{}", secret.to_lowercase().replace('-', " ").replace('0', "o"));
        assert_eq!(Invitation::parse(&format!("  {typed} \n")).unwrap(), made);
        assert_ne!(Invitation::new("192.168.1.20", 47_600).unwrap().key(), made.key(), "the secret is by lot");
        // The same invitation at another address is the same secret.
        let elsewhere = made.at("fe80::1");
        assert_eq!((elsewhere.key(), elsewhere.address().as_str()), (made.key(), "[fe80::1]:47600"));
        assert_eq!(Invitation::parse(&elsewhere.to_string()).unwrap(), elsewhere);
        // And it is not in what a log shows.
        assert_eq!(format!("{made:?}"), "Invitation(192.168.1.20:47600)");
    }

    #[test]
    fn what_is_no_invitation_is_refused_in_a_sentence() {
        let made = Invitation::new("localhost", 9).unwrap().to_string();
        for wrong in [
            "",
            "localhost:9",
            "trill:localhost",
            "trill:localhost:9",
            "trill:localhost:nine:AAAA",
            "trill::9:0000-0000-0000-0000-0000-0000-0000-0000",
            &made[..made.len() - 3],
            &format!("{made}Z"),
            &made.replace("trill:", "http:"),
            &format!("{}!", &made[..made.len() - 1]),
        ] {
            let refusal = Invitation::parse(wrong).expect_err(wrong);
            assert!(refusal.contains("trill:HOST:PORT:SECRET"), "{wrong}: {refusal}");
        }
    }
}
