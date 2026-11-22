//! A meeting point to share a song through: where it is, and the key its
//! keeper gave.

use trill_meet::Key;

/// A meeting point as whoever shares through it has it: its address and
/// the key its keeper handed over. Whoever joins needs neither: the
/// invitation carries the address, and joining takes no key.
///
/// The key is a secret of its owner's. It is not in what this shows of
/// itself (`Debug` has which key it is and no more), and nothing in this
/// crate writes it anywhere or says it to anyone: the meeting point is
/// shown that one has it, by a handshake, and never the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingPoint {
    pub host: String,
    pub port: u16,
    pub key: Key,
}

impl MeetingPoint {
    /// Reads what a keeper handed over: an address (`meet.example.org`,
    /// `meet.example.org:443`, `203.0.113.7:443`, `[2001:db8::7]:443`;
    /// without a port it is the one a meeting point takes unless it is
    /// told another) and a key.
    pub fn parse(address: &str, key: &str) -> Result<MeetingPoint, String> {
        let wrong = || {
            "a meeting point's address is a name or a number, and a port if its keeper named one: \
             meet.example.org:443"
                .to_owned()
        };
        let address = address.trim();
        let (host, port) = match address.strip_prefix('[') {
            // A number with colons of its own, in brackets.
            Some(rest) => match rest.split_once(']') {
                Some((host, "")) => (host, None),
                Some((host, port)) => (host, Some(port.strip_prefix(':').ok_or_else(wrong)?)),
                None => return Err(wrong()),
            },
            None => match address.split_once(':') {
                // (Or without them.)
                Some((_, rest)) if rest.contains(':') => (address, None),
                Some((host, port)) => (host, Some(port)),
                None => (address, None),
            },
        };
        let port = match port {
            Some(port) => port.trim().parse().ok().filter(|port| *port != 0).ok_or_else(wrong)?,
            None => trill_meet::PORT,
        };
        let plain = |letter: char| letter.is_ascii_alphanumeric() || matches!(letter, '.' | '-' | ':' | '_' | '%');
        if host.is_empty() || host.len() > 253 || !host.chars().all(plain) {
            return Err(wrong());
        }
        Ok(MeetingPoint { host: host.to_owned(), port, key: Key::parse(key)? })
    }

    /// `host:port`, as a socket takes it.
    pub fn address(&self) -> String {
        match self.host.contains(':') {
            true => format!("[{}]:{}", self.host, self.port),
            false => format!("{}:{}", self.host, self.port),
        }
    }

    /// The address as its keeper would write it: without the port where
    /// it is the usual one.
    pub fn shown(&self) -> String {
        match (self.port == trill_meet::PORT, self.host.contains(':')) {
            (true, _) => self.host.clone(),
            (false, true) => format!("[{}]:{}", self.host, self.port),
            (false, false) => format!("{}:{}", self.host, self.port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meeting_point_is_read_as_its_keeper_wrote_it_and_shows_nothing_of_its_key() {
        let key = Key::new().unwrap();
        let text = key.to_string();
        for (written, host, port, address) in [
            ("meet.example.org", "meet.example.org", trill_meet::PORT, "meet.example.org:8745"),
            ("  meet.example.org:443 ", "meet.example.org", 443, "meet.example.org:443"),
            ("203.0.113.7:443", "203.0.113.7", 443, "203.0.113.7:443"),
            ("[2001:db8::7]:443", "2001:db8::7", 443, "[2001:db8::7]:443"),
            ("[2001:db8::7]", "2001:db8::7", trill_meet::PORT, "[2001:db8::7]:8745"),
            ("2001:db8::7", "2001:db8::7", trill_meet::PORT, "[2001:db8::7]:8745"),
        ] {
            let point = MeetingPoint::parse(written, &text.to_lowercase()).expect(written);
            assert_eq!((point.host.as_str(), point.port, point.address().as_str()), (host, port, address), "{written}");
            assert_eq!(MeetingPoint::parse(&point.shown(), &text).unwrap(), point, "{written}");
            assert_eq!(point.key, key);
            let shown = format!("{point:?}");
            assert!(shown.contains(&key.id().to_string()) && !shown.contains(&text[10..]), "{shown}");
        }
        for wrong in
            ["", "https://meet.example.org", "meet.example.org:", "meet.example.org:0", "a b", "[::1", "[::1]9"]
        {
            let refusal = MeetingPoint::parse(wrong, &text).expect_err(wrong);
            assert!(refusal.contains("meet.example.org:443"), "{wrong}: {refusal}");
        }
        let refusal = MeetingPoint::parse("meet.example.org", "not a key").unwrap_err();
        assert!(refusal.contains("ten groups of four letters"), "{refusal}");
    }
}
