//! An invitation: where to connect, and the secret that says one may.

use sha2::{Digest, Sha256};
use std::fmt;
use trill_meet::text::{Misspelt, spell, unspell};
use trill_meet::{Door, Meeting};

/// How many bytes of secret an invitation carries: 160 bits by lot.
const SECRET_BYTES: usize = 20;

/// Where a program that shares a song is reached, and the secret it lets
/// a program in by. It is text to hand to someone, one line:
///
/// * `trill:HOST:PORT:SECRET`, where the program itself listens;
/// * `trill:meet:HOST:PORT:SECRET`, where it is reached through a meeting
///   point, whose address that then is. Whoever joins sets nothing up:
///   the invitation says where to go.
///
/// Whoever has it can read and change the song while it is shared, and
/// nobody else can: the secret never crosses the wire, each side proves
/// it has it, and everything said afterwards is encrypted under a key
/// that only the two of them have. A meeting point in between has
/// neither: it knows the meeting by something worked out one way from
/// the secret ([`Invitation::meeting`]), and whether someone who asks for
/// it has the invitation by another ([`Invitation::door`]).
#[derive(Clone, PartialEq, Eq)]
pub struct Invitation {
    pub host: String,
    pub port: u16,
    /// Whether `host` and `port` are a meeting point's, through which the
    /// program that shares is reached, and not the program's own.
    pub meet: bool,
    secret: [u8; SECRET_BYTES],
}

impl Invitation {
    /// An invitation to `host` at `port`, with a secret by lot from the
    /// system's own source.
    pub fn new(host: impl Into<String>, port: u16) -> std::io::Result<Self> {
        let mut secret = [0u8; SECRET_BYTES];
        getrandom::fill(&mut secret).map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self { host: host.into(), port, meet: false, secret })
    }

    /// An invitation to a program that is reached through the meeting
    /// point at `host` and `port`.
    pub fn through(host: impl Into<String>, port: u16) -> std::io::Result<Self> {
        Ok(Self { meet: true, ..Self::new(host, port)? })
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
        let wrong = || {
            "an invitation reads trill:HOST:PORT:SECRET (or trill:meet:HOST:PORT:SECRET, through a meeting point), \
             as the program that shares the song shows it"
        };
        let text = text.trim();
        let rest = text.strip_prefix("trill:").ok_or_else(wrong)?;
        // From the right: an address may have colons of its own.
        let (rest, secret) = rest.rsplit_once(':').ok_or_else(wrong)?;
        let (host, port) = rest.rsplit_once(':').ok_or_else(wrong)?;
        // (No address begins so: a name has no colon, and a number that
        // has colons has no `m`.)
        let (meet, host) = match host.strip_prefix("meet:") {
            Some(host) => (true, host),
            None => (false, host),
        };
        let host = host.trim().trim_start_matches('[').trim_end_matches(']');
        let port: u16 = port.trim().parse().map_err(|_| format!("{port} is no port: {}", wrong()))?;
        if host.is_empty() || port == 0 {
            return Err(wrong().to_owned());
        }
        let read = match unspell(secret, SECRET_BYTES) {
            Ok(read) => read.try_into().expect("as many bytes as were asked for"),
            Err(Misspelt::Letter(letter)) => {
                return Err(format!("'{letter}' is in no invitation's secret: {}", wrong()));
            }
            Err(Misspelt::Length) => return Err(format!("the secret is not whole: {}", wrong())),
        };
        Ok(Self { host: host.to_owned(), port, meet, secret: read })
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

    /// What a meeting point knows this invitation's meeting by: worked
    /// out one way from the secret, so that whoever has only this (the
    /// meeting point, someone who listens on the way there) can ask for
    /// the meeting and cannot get into it.
    pub fn meeting(&self) -> Meeting {
        Meeting::of(&self.secret)
    }

    /// What whoever asks a meeting point for this invitation's meeting
    /// shows that they have it by: worked out one way from the secret,
    /// under a label of its own. The meeting point has it from whoever
    /// shares, and asks a question only someone who has it can answer:
    /// without the invitation nobody knocks.
    pub fn door(&self) -> Door {
        Door::of(&self.secret)
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
        let host = match self.host.contains(':') {
            true => format!("[{}]", self.host),
            false => self.host.clone(),
        };
        let meet = if self.meet { "meet:" } else { "" };
        write!(f, "trill:{meet}{host}:{}:{}", self.port, spell(&self.secret))
    }
}

impl fmt::Debug for Invitation {
    /// Without the secret: an invitation in a log is nobody's to use.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let meet = if self.meet { "through " } else { "" };
        write!(f, "Invitation({meet}{}:{})", self.host, self.port)
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
    fn an_invitation_through_a_meeting_point_carries_its_address_and_names_a_meeting_that_is_not_the_secret() {
        let made = Invitation::through("meet.example.org", 443).unwrap();
        let text = made.to_string();
        assert!(text.starts_with("trill:meet:meet.example.org:443:"), "{text}");
        assert!(!text.contains('\n') && text.len() < 80, "one line to paste: {text}");
        let read = Invitation::parse(&format!(" {} ", text.to_lowercase())).unwrap();
        assert_eq!(read, made);
        assert!(read.meet && read.host == "meet.example.org" && read.address() == "meet.example.org:443");
        assert_eq!(format!("{made:?}"), "Invitation(through meet.example.org:443)");
        // By number, of either kind.
        for host in ["203.0.113.7", "2001:db8::7"] {
            let made = Invitation::through(host, 8745).unwrap();
            let read = Invitation::parse(&made.to_string()).unwrap();
            assert_eq!((read.clone(), read.meet, read.host.as_str()), (made, true, host));
        }
        // The same words without `meet:` are an invitation to a program
        // that listens itself, as they always were; a machine that is
        // called meet is one too.
        let direct = Invitation::parse(&text.replace("trill:meet:", "trill:")).unwrap();
        assert!(!direct.meet && direct.host == "meet.example.org" && direct.key() == made.key());
        assert_ne!(direct, made);
        let called_meet = Invitation::parse(&text.replace("trill:meet:meet.example.org", "trill:meet")).unwrap();
        assert!(!called_meet.meet && called_meet.host == "meet");

        // What the meeting point knows the meeting by is the secret's and
        // nothing else's, and is neither the secret nor the key.
        let meeting = made.meeting();
        assert_eq!(meeting, direct.meeting());
        assert!(made.door() == direct.door() && made.door().to_string() != meeting.to_string());
        assert_ne!(made.door().to_string(), trill_meet::text::hex(&made.key()), "nor is the door the key");
        assert_eq!(meeting, made.at("elsewhere.example.org").meeting());
        assert_ne!(meeting, Invitation::through("meet.example.org", 443).unwrap().meeting());
        let (secret, shown) = (text.rsplit(':').next().unwrap().replace('-', ""), meeting.to_string());
        assert!(!shown.eq_ignore_ascii_case(&trill_meet::text::hex(&made.key())));
        assert!(!text.contains(&shown) && !shown.to_uppercase().contains(&secret[..8]));
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
            "trill:meet::9:0000-0000-0000-0000-0000-0000-0000-0000",
            "trill:meet:9:0000-0000-0000-0000-0000-0000-0000-0000-0000",
        ] {
            let refusal = Invitation::parse(wrong).expect_err(wrong);
            assert!(refusal.contains("trill:HOST:PORT:SECRET"), "{wrong}: {refusal}");
        }
    }
}
