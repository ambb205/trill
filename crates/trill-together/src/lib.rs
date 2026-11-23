//! The wire between programs that hold the same song.
//!
//! Two or more people work in one song, each in a program of their own.
//! Nobody is the host: every program has all of the song, and they are
//! connected to each other by invitation. What makes that
//! possible is in `trill_core::merge` (a song held as properties that
//! merge) and in the session (which takes changes in and says its own);
//! this crate is only how they reach each other:
//!
//! * [`Invitation`]: where a program is reached and the secret it lets
//!   one in by, as text to hand to someone.
//! * [`secure`]: a connection that only holders of the secret can read or
//!   write, by the Noise protocol through the `snow` library. (It lives in
//!   `trill-meet`, which a meeting point is built from too.)
//! * [`Message`]: what is said over it. Changes to the song, the whole
//!   song for a program that has not got it, and who is there
//!   ([`Presence`]).
//! * [`Net`]: the connections of one program, each with threads of its
//!   own, so that whoever drives the song never waits on the network.
//!
//! **Reached by address, or through a meeting point.** A program joins
//! wherever it can open a connection to the other: on the same machine,
//! on a local network, over a VPN, through a port someone forwarded. Two
//! that cannot reach each other (each at home, behind a router) both
//! connect outward to a *meeting point* ([`MeetingPoint`], the
//! `trill-meet` crate), which passes what each says to the other. Then
//! neither listens for anything, and the invitation carries the meeting
//! point's address.
//!
//! **Nothing readable or writable without the invitation.** Listening
//! starts when the user shares and stops when they stop; a connection
//! that does not prove the secret is dropped before anything is said.
//! That holds through a meeting point as it does directly: the two
//! programs prove the invitation to each other end to end, by the same
//! handshake, and the meeting point has neither the secret nor anything
//! it could be worked back from.
//!
//! **Everything that arrives is untrusted**: bounded in size and in rate
//! here, and held to the model's own rules where it is taken in
//! (`trill_core::merge::Change::checked`).

pub mod files;
mod invite;
pub mod line;
mod net;
mod point;
mod wire;

pub use invite::Invitation;
pub use net::{Event, JoinId, LinkId, Net, Pieces, Reach};
pub use point::MeetingPoint;
/// The meeting point's own crate: what it says to whoever it turns away,
/// and, for a test, the meeting point itself.
pub use trill_meet as meet;
pub use trill_meet::secure;
pub use wire::{MAX_HAVE, MAX_HELD, MAX_MESSAGE, MAX_NAME, MAX_PIECE, MAX_WANT, Message, PROTOCOL, Person, Presence};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Waits for something to happen on `net`, for a few seconds.
    fn next(net: &mut Net) -> Event {
        next_within(net, Duration::from_secs(10))
    }

    /// (One thing at a time: two that happened at once are two. What a
    /// meeting point says of a key's day is not something that happened
    /// to a connection, and is looked at by the test that is about it.)
    fn next_within(net: &mut Net, at_most: Duration) -> Event {
        loop {
            match net.next(at_most).expect("nothing happened") {
                Event::Allowance { .. } => {}
                event => return event,
            }
        }
    }

    fn person(name: &str) -> Person {
        Person { id: 7, name: name.into(), colour: 0 }
    }

    #[test]
    fn two_programs_meet_by_invitation_on_this_machine_and_say_things_to_each_other() {
        let (mut sharer, mut joiner) = (Net::new(), Net::new());
        let invitation = sharer.listen(Reach::ThisMachine, 0).unwrap();
        assert_eq!(invitation.host, "127.0.0.1", "this machine only: nothing on a network can connect");
        assert_eq!(sharer.listen(Reach::ThisMachine, 0).unwrap(), invitation, "listening again is the same invitation");
        // The invitation as text, as it is handed over.
        joiner.join(Invitation::parse(&invitation.to_string()).unwrap());
        let Event::Open { link: theirs, joined: Some(_) } = next(&mut joiner) else {
            panic!("the joiner is connected")
        };
        let Event::Open { link: ours, joined: None } = next(&mut sharer) else { panic!("and so is the sharer") };

        let presence = Message::Presence(Presence::of(person("Jo")));
        joiner.say(theirs, &presence);
        let Event::Heard { link, message } = next(&mut sharer) else { panic!("it is heard") };
        assert_eq!((link, &message), (ours, &presence));
        sharer.say_to(&[ours], &Message::Gone { person: 7 });
        assert!(matches!(next(&mut joiner), Event::Heard { message: Message::Gone { person: 7 }, .. }));

        // One of them leaves, and says why.
        joiner.close(theirs, "Jo left");
        assert!(matches!(next(&mut joiner), Event::Lost { why, .. } if why == "Jo left"));
        let Event::Lost { link, why, .. } = next(&mut sharer) else { panic!("the other hears of it") };
        assert_eq!(link, ours);
        assert!(why.contains("Jo left"), "{why}");
        assert!(sharer.links().is_empty() && joiner.links().is_empty());
    }

    #[test]
    fn a_connection_whose_line_goes_down_without_a_word_is_let_go_of_by_the_side_that_sees_nothing() {
        // (Connections: one, through a line, which is two on the loopback.)
        let (still, silent) = (Duration::from_millis(100), Duration::from_millis(700));
        let mut sharer = Net::new().with_pulse(still, silent);
        let mut joiner = Net::new().with_pulse(still, silent);
        let invitation = sharer.listen(Reach::ThisMachine, 0).unwrap();
        let (line, over) = line::before(&invitation, line::Pace::of(8000, 8000, 5)).unwrap();
        joiner.join(over);
        assert!(matches!(next(&mut joiner), Event::Open { joined: Some(_), .. }));
        let Event::Open { link: ours, joined: None } = next(&mut sharer) else { panic!("the sharer is connected") };

        // Neither program says a word for twice as long as a connection
        // may be silent: the connections say that they are there, and
        // nobody hears of that.
        assert!(sharer.next(2 * silent).is_none(), "a connection that has nothing to say is not lost");
        assert!(joiner.events().is_empty());

        // The line goes down. The joiner's side of it ends; the sharer is
        // sent no word of that, and what it says goes nowhere.
        line.lose();
        assert!(matches!(next(&mut joiner), Event::Lost { told: false, .. }));
        let began = Instant::now();
        let Event::Lost { link, why, told } = next_within(&mut sharer, 5 * silent) else {
            panic!("the side that sees nothing lets go by itself")
        };
        assert_eq!((link, told), (ours, false), "lost on the way: whoever asked for it may ask again");
        assert!(why.contains("nothing was heard"), "{why}");
        assert!(began.elapsed() < 3 * silent, "it let go after {:?}", began.elapsed());
        assert!(sharer.links().is_empty());
    }

    #[test]
    fn a_piece_of_a_file_is_written_as_bytes_and_a_file_is_named_by_a_hash_and_by_nothing_else() {
        use trill_core::files::Content;
        let file = Content::new(&[0x5a; 32], 123_456_789_012);
        let piece = Message::Piece { file: file.clone(), at: 99_000_000_000, bytes: (0..=255).collect() };
        let written = piece.encode();
        assert_eq!(written.len(), 256 + 49, "the bytes, and 49 that say which they are: not text");
        assert_eq!(Message::decode(&written).unwrap(), piece);
        let whole = Message::Piece { file: file.clone(), at: 0, bytes: vec![7; MAX_PIECE] };
        assert_eq!(Message::decode(&whole.encode()).unwrap(), whole, "as much as a piece carries");
        // No bytes, more than a piece carries, and one cut short.
        assert!(Message::decode(&Message::Piece { file: file.clone(), at: 0, bytes: Vec::new() }.encode()).is_err());
        let long = Message::Piece { file: file.clone(), at: 0, bytes: vec![7; MAX_PIECE + 1] };
        assert!(Message::decode(&long.encode()).unwrap_err().contains("more than"));
        assert!(Message::decode(&written[..40]).is_err());
        // Written as text a piece is no message: nothing reads megabytes
        // of numbers.
        assert!(Message::decode(br#"{"piece":{"file":{"sha256":"00","bytes":1},"at":0,"bytes":[1,2,3]}}"#).is_err());

        // The rest is text, and names a file by what it is.
        for said in [
            Message::Have { files: vec![file.clone()] },
            Message::Want { file: file.clone(), at: 5, len: MAX_WANT },
            Message::Nothing { file: file.clone() },
        ] {
            assert_eq!(Message::decode(&said.encode()).unwrap(), said);
        }
        // There is nowhere to say a path but where the hash goes, and what
        // is said there has to be a hash.
        for path in [r"C:\Users\Ada\Music\take.wav", "/home/ada/take.wav", "../take.wav", "", &"A".repeat(64)] {
            let by_path = Content { sha256: path.to_owned(), bytes: 10 };
            for said in [
                Message::Want { file: by_path.clone(), at: 0, len: 10 },
                Message::Nothing { file: by_path.clone() },
                Message::Have { files: vec![file.clone(), by_path.clone()] },
            ] {
                let refused = Message::decode(&said.encode()).unwrap_err();
                assert!(refused.contains("something that is no hash"), "{path:?}: {refused}");
            }
            // (A piece of it is a piece of the file whose hash is nothing.)
            let piece = Message::Piece { file: by_path, at: 0, bytes: vec![1] };
            let Message::Piece { file: of, .. } = Message::decode(&piece.encode()).unwrap() else { panic!() };
            assert_eq!(of.sha256, "0".repeat(64));
        }
        let many = Message::Have { files: vec![file; MAX_HAVE + 1] };
        assert!(Message::decode(&many.encode()).unwrap_err().contains("more than"));
    }

    #[test]
    fn what_is_said_of_the_song_goes_ahead_of_the_bytes_of_a_file() {
        use trill_core::files::Content;
        let (mut sharer, mut joiner) = (Net::new(), Net::new());
        let invitation = sharer.listen(Reach::ThisMachine, 0).unwrap();
        joiner.join(invitation);
        let Event::Open { link: theirs, .. } = next(&mut joiner) else { panic!("the joiner is connected") };
        let Event::Open { link: ours, .. } = next(&mut sharer) else { panic!("and so is the sharer") };
        // A file of 96 MB is queued for the other, piece by piece, from a
        // thread that is not the song's; then something is said of the song.
        const PIECES: usize = 2000;
        let file = Content::new(&[1; 32], (PIECES * MAX_PIECE) as u64);
        let pieces = sharer.pieces();
        for at in 0..PIECES {
            let piece =
                Message::Piece { file: file.clone(), at: (at * MAX_PIECE) as u64, bytes: vec![at as u8; MAX_PIECE] };
            assert!(pieces.say(ours, &piece).is_some());
        }
        assert!(pieces.say(ours, &Message::Gone { person: 1 }).is_none(), "it says pieces and nothing else");
        // (How many pieces wait when the song is spoken of: on a machine
        // that is busy the connection has said some of the file by now,
        // and how many is no part of what is held here.)
        let waited = pieces.waiting(ours).unwrap() / (MAX_PIECE + 64);
        assert!(waited > 50, "much of the file waits: {waited} pieces");
        sharer.say(ours, &Message::Gone { person: 7 });
        // The other hears it after the few pieces that were on their way,
        // and not after the pieces that waited when it was said.
        let mut before = 0;
        loop {
            match next(&mut joiner) {
                Event::Heard { message: Message::Piece { at, .. }, .. } => {
                    assert_eq!(at, (before * MAX_PIECE) as u64, "pieces arrive in the order they were said in");
                    before += 1;
                }
                Event::Heard { message: Message::Gone { person: 7 }, .. } => break,
                other => panic!("{other:?}"),
            }
        }
        let overtaken = PIECES - before;
        assert!(
            overtaken > waited / 2,
            "the song waited for {before} of {PIECES} pieces, {waited} of which waited then"
        );
        // A connection that is ended does not say the rest of a file first.
        let began = Instant::now();
        let waited = pieces.waiting(ours).unwrap() / (MAX_PIECE + 64);
        sharer.close(ours, "enough");
        loop {
            match next(&mut joiner) {
                Event::Heard { message: Message::Piece { .. }, .. } => before += 1,
                Event::Lost { why, .. } => break assert!(why.contains("enough"), "{why}"),
                other => panic!("{other:?}"),
            }
        }
        assert!(PIECES - before > waited / 2, "{before} pieces were said before the goodbye, of {PIECES}");
        assert!(began.elapsed() < Duration::from_secs(5));
        let _ = theirs;
    }

    #[test]
    fn an_invitation_that_is_another_ones_or_no_longer_good_is_a_sentence() {
        let (mut sharer, mut joiner) = (Net::new(), Net::new());
        let invitation = sharer.listen(Reach::ThisMachine, 0).unwrap();
        // The right place, the wrong secret.
        let other = Invitation::new("127.0.0.1", invitation.port).unwrap();
        joiner.join(other);
        let Event::Failed { why, .. } = next(&mut joiner) else { panic!("no connection") };
        assert!(why.contains("did not take this invitation"), "{why}");
        assert!(sharer.events().is_empty() && sharer.links().is_empty(), "and the sharer has nothing of it");

        // Sharing was stopped: the invitation is no longer one.
        sharer.stop_listening();
        assert!(sharer.listening().is_none());
        std::thread::sleep(Duration::from_millis(150));
        joiner.join(invitation);
        let Event::Failed { why, .. } = next(&mut joiner) else { panic!("no connection") };
        assert!(why.contains("not sharing its song") || why.contains("did not take"), "{why}");
    }

    #[test]
    fn what_is_no_message_ends_the_connection_in_a_sentence() {
        let (mut sharer, _) = (Net::new(), ());
        let invitation = sharer.listen(Reach::ThisMachine, 0).unwrap();
        // A program that has the secret and says nonsense.
        let stream = std::net::TcpStream::connect(invitation.address()).unwrap();
        let (mut say, _hear) = secure::secure(stream, &invitation.key(), true).unwrap();
        let Event::Open { link, .. } = next(&mut sharer) else { panic!("it is connected") };
        say.say(b"{\"presence\": \"of what\"}").unwrap();
        let Event::Lost { link: lost, why, .. } = next(&mut sharer) else { panic!("and let go of") };
        assert_eq!(lost, link);
        assert!(why.contains("is no message"), "{why}");
        // Bounds on what a presence is hold whatever is said.
        let mut long = Presence::of(person(&"n".repeat(500)));
        long.beat = f64::NAN;
        long.clips = vec![trill_core::ClipId(1); 5000];
        let bounded = long.bounded();
        assert_eq!((bounded.person.name.len(), bounded.beat, bounded.clips.len()), (MAX_NAME, 0.0, MAX_HELD));
    }

    #[test]
    fn many_connections_without_the_invitation_are_let_go_of_and_whoever_has_it_still_joins() {
        let (mut sharer, mut joiner) = (Net::new(), Net::new());
        let invitation = sharer.listen(Reach::ThisMachine, 0).unwrap();
        // More at once than may be proving who they are (sixteen), none
        // of which says a word; then they go. (And no more than a few
        // more: every connection a test makes keeps one of the machine's
        // ports for two minutes after.)
        let silent: Vec<_> = (0..20).filter_map(|_| std::net::TcpStream::connect(invitation.address()).ok()).collect();
        assert!(silent.len() > 16, "they were connected: {}", silent.len());
        std::thread::sleep(Duration::from_millis(300));
        assert!(sharer.events().is_empty() && sharer.links().is_empty(), "none of them is a program in the song");
        drop(silent);
        // Whoever has the invitation is taken, as soon as there is room.
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            joiner.join(invitation.clone());
            match next(&mut joiner) {
                Event::Open { joined: Some(_), .. } => break,
                Event::Failed { .. } => {
                    assert!(Instant::now() < until, "nobody with the invitation was taken any more")
                }
                other => panic!("{other:?}"),
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let Event::Open { joined: None, .. } = next(&mut sharer) else { panic!("the sharer has it too") };
    }

    /// A meeting point on this machine, on a port the system picks, at
    /// which one key is given: what it is reached by, and its log.
    fn meeting_point() -> (trill_meet::Server, MeetingPoint, Lines) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let key = trill_meet::Key::new().unwrap();
        let (server, log) = meeting_point_on(listener, &key);
        (server, MeetingPoint { host: "127.0.0.1".into(), port, key }, log)
    }

    type Lines = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A meeting point on `listener`, at which `key` is given to Ada.
    fn meeting_point_on(listener: std::net::TcpListener, key: &trill_meet::Key) -> (trill_meet::Server, Lines) {
        let log = Lines::default();
        let lines = log.clone();
        let said: trill_meet::Log = std::sync::Arc::new(move |line: &str| lines.lock().unwrap().push(line.to_owned()));
        let keys = trill_meet::Keys::of(vec![trill_meet::keys::Given::of(key, "Ada")]);
        let server = trill_meet::Server::start(vec![listener], keys, trill_meet::Limits::default(), said).unwrap();
        (server, log)
    }

    /// Waits until the meeting point holds what `as_wanted` says.
    fn until(server: &trill_meet::Server, what: &str, as_wanted: impl Fn(trill_meet::Counts) -> bool) {
        let end = Instant::now() + Duration::from_secs(30);
        while !as_wanted(server.counts()) {
            assert!(Instant::now() < end, "{what}: {:?}", server.counts());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The next thing that happened which is not about the meeting's
    /// being held.
    fn next_of_a_link(net: &mut Net) -> Event {
        loop {
            match next(net) {
                Event::Held | Event::Unheld { .. } => {}
                event => return event,
            }
        }
    }

    /// 1 connection.
    #[test]
    fn whoever_holds_a_meeting_is_told_what_its_key_may_still_pass_today() {
        let (_server, point, _) = meeting_point();
        let mut sharer = Net::new();
        sharer.listen_through(&point).unwrap();
        let mut held = false;
        // The meeting is open, and with the first answer to "I am here"
        // comes how much is left of the day, and until when it goes on.
        let (left, over) = loop {
            match sharer.next(Duration::from_secs(10)).expect("nothing happened") {
                Event::Held => held = true,
                Event::Allowance { left, over } => break (left, over),
                other => panic!("{other:?}"),
            }
        };
        assert!(held, "the meeting was open first");
        assert_eq!(left, trill_meet::Limits::default().bytes_a_day, "nothing has passed yet");
        let day = over.saturating_duration_since(Instant::now());
        assert!(day > Duration::from_secs(23 * 3600) && day <= Duration::from_secs(24 * 3600), "{day:?}");
    }

    #[test]
    fn two_programs_that_only_connect_outward_meet_through_a_meeting_point_that_reads_nothing() {
        let (server, point, log) = meeting_point();
        let (mut sharer, mut joiner) = (Net::new(), Net::new());
        let invitation = sharer.listen_through(&point).unwrap();
        let text = invitation.to_string();
        assert!(text.starts_with(&format!("trill:meet:127.0.0.1:{}:", point.port)), "{text}");
        assert_eq!(sharer.listen_through(&point).unwrap(), invitation, "sharing again is the same invitation");
        assert!(matches!(next(&mut sharer), Event::Held), "the meeting is open at the meeting point");

        // Whoever has the invitation needs nothing else: neither a key
        // nor the meeting point's address, which the invitation carries.
        joiner.join(Invitation::parse(&text).unwrap());
        let Event::Open { link: theirs, joined: Some(_) } = next(&mut joiner) else {
            panic!("the joiner is connected")
        };
        let Event::Open { link: ours, joined: None } = next(&mut sharer) else { panic!("and so is the sharer") };
        let presence = Message::Presence(Presence::of(person("Jo")));
        joiner.say(theirs, &presence);
        let Event::Heard { link, message } = next(&mut sharer) else { panic!("it is heard") };
        assert_eq!((link, &message), (ours, &presence));
        sharer.say_to(&[ours], &Message::Gone { person: 7 });
        assert!(matches!(next(&mut joiner), Event::Heard { message: Message::Gone { person: 7 }, .. }));
        let counts = server.counts();
        assert_eq!((counts.meetings, counts.held, counts.pipes, counts.connections), (1, 1, 1, 3));

        // Sharing is stopped: the meeting is over where it was held, and
        // whoever asks for it with the invitation is told so. Who is
        // connected stays.
        sharer.stop_listening();
        until(&server, "the meeting is over", |counts| counts.meetings == 0);
        let mut late = Net::new();
        late.join(invitation.clone());
        let Event::Failed { why, .. } = next(&mut late) else { panic!("no connection") };
        assert!(why.contains("stopped sharing it"), "{why}");
        joiner.say(theirs, &presence);
        assert!(matches!(next(&mut sharer), Event::Heard { .. }));
        // One leaves, and says why, through the meeting point as directly.
        joiner.close(theirs, "Jo left");
        assert!(matches!(next(&mut joiner), Event::Lost { why, .. } if why == "Jo left"));
        let Event::Lost { link, why, .. } = next(&mut sharer) else { panic!("the other hears of it") };
        assert_eq!(link, ours);
        assert!(why.contains("Jo left"), "{why}");
        until(&server, "its pipe is gone", |counts| counts.pipes == 0);

        // What the meeting point wrote down has neither the invitation's
        // secret nor the key, nor either of the two things it knows the
        // meeting by.
        let log = log.lock().unwrap().join("\n");
        assert!(log.contains("Ada holds a meeting") && log.contains("is over: its holder ended it"), "{log}");
        let secret = text.rsplit(':').next().unwrap();
        let key = point.key.to_string();
        let (meeting, door) = (invitation.meeting().to_string(), invitation.door().to_string());
        for kept_out in [secret, &secret.replace('-', ""), &key[10..], &meeting, &door] {
            assert!(!log.to_uppercase().contains(&kept_out.to_uppercase()), "{kept_out} is in the log:\n{log}");
        }
    }

    #[test]
    fn whoever_has_not_got_the_invitation_cannot_knock_and_whoever_knocks_without_it_is_not_in() {
        use trill_meet::{Door, No, Turned, client};
        let (server, point, log) = meeting_point();
        let (mut sharer, mut joiner) = (Net::new(), Net::new());
        let invitation = sharer.listen_through(&point).unwrap();
        assert!(matches!(next(&mut sharer), Event::Held));
        let (at, meeting) = (point.address(), invitation.meeting());

        // Someone who knows what the meeting goes by, which is all that
        // someone who listened on the way to the meeting point has: told
        // that there is no such meeting, and the program that shares hears
        // nothing of it.
        let turned = client::join(&at, meeting, Door::of(b"not the invitation's")).unwrap_err();
        assert_eq!(turned, Turned::No(No::Unknown));
        assert_eq!(server.counts().knocks, 0);
        assert!(sharer.events().is_empty(), "nobody knocked");

        // Someone who has what the meeting point has of the meeting, and
        // not the invitation: they can knock, and are piped to the program
        // that shares, like anyone who asks. One does the handshake with a
        // secret that is not the invitation's, and one says nothing.
        let silent = client::join(&at, meeting, invitation.door()).unwrap();
        assert!(secure::secure(client::join(&at, meeting, invitation.door()).unwrap(), &[9u8; 32], true).is_err());
        until(&server, "the one with another secret is let go of", |counts| counts.pipes == 1);
        // Whoever has the invitation joins meanwhile.
        joiner.join(invitation.clone());
        assert!(matches!(next(&mut joiner), Event::Open { joined: Some(_), .. }));
        let Event::Open { joined: None, .. } = next(&mut sharer) else { panic!("the sharer has it") };
        // The one that never says anything has no longer than the
        // handshake has: ten seconds.
        until(&server, "the silent one is let go of", |counts| counts.pipes == 1 && counts.connections == 3);
        drop(silent);
        assert!(sharer.events().is_empty(), "none of them was anything to the program that shares");
        assert_eq!(sharer.links().len(), 1);
        let log = log.lock().unwrap().join("\n");
        assert_eq!(log.matches("turned away (answer), from 127.0.0.1").count(), 1, "{log}");
    }

    #[test]
    fn a_meeting_point_that_does_not_take_the_key_is_said_and_one_that_is_not_there_is_tried_until_it_is() {
        // A key its keeper did not give: said, and not tried again.
        let (_server, point, _) = meeting_point();
        let other = MeetingPoint { key: trill_meet::Key::new().unwrap(), ..point.clone() };
        let mut sharer = Net::new();
        sharer.listen_through(&other).unwrap();
        let Event::Unheld { turned, again: false } = next(&mut sharer) else { panic!("it is not held") };
        assert_eq!(turned, trill_meet::Turned::No(trill_meet::No::Key));
        assert!(turned.sentence().contains("did not take the key"), "{}", turned.sentence());
        sharer.stop_listening();

        // A meeting point that is not there: said, and tried again.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let nowhere = MeetingPoint { port: free, ..point.clone() };
        let (mut sharer, mut joiner) = (Net::new(), Net::new());
        let invitation = sharer.listen_through(&nowhere).unwrap();
        let Event::Unheld { turned, again: true } = next(&mut sharer) else { panic!("it is not held") };
        assert!(turned.sentence().contains("did not answer"), "{}", turned.sentence());
        joiner.join(invitation.clone());
        let Event::Failed { why, .. } = next(&mut joiner) else { panic!("no connection") };
        assert!(why.contains("the meeting point did not answer"), "{why}");

        // It comes to be there, and the meeting is held without anyone
        // doing anything. (However long the pause before it is tried
        // again has grown.)
        let held = |sharer: &mut Net| loop {
            match next_within(sharer, Duration::from_secs(60)) {
                Event::Held => break,
                Event::Unheld { again: true, .. } => {}
                other => panic!("{other:?}"),
            }
        };
        let listen = || std::net::TcpListener::bind(("127.0.0.1", free)).unwrap();
        let (server, _) = meeting_point_on(listen(), &point.key);
        held(&mut sharer);
        joiner.join(invitation.clone());
        assert!(matches!(next(&mut joiner), Event::Open { joined: Some(_), .. }));
        assert!(matches!(next_of_a_link(&mut sharer), Event::Open { joined: None, .. }));

        // The meeting point goes down and comes back, as when its keeper
        // puts a new one in its place: the connections through it are
        // lost, the meeting is held again by itself under the invitation
        // it had, and whoever has that gets in again.
        server.stop();
        assert!(matches!(next(&mut joiner), Event::Lost { .. }));
        assert!(matches!(next_of_a_link(&mut sharer), Event::Lost { .. }));
        let (server, log) = meeting_point_on(listen(), &point.key);
        held(&mut sharer);
        joiner.join(invitation);
        assert!(matches!(next(&mut joiner), Event::Open { joined: Some(_), .. }));
        assert!(matches!(next_of_a_link(&mut sharer), Event::Open { joined: None, .. }));
        assert_eq!(server.counts().pipes, 1);
        assert!(log.lock().unwrap().join("\n").contains("Ada holds a meeting"));
    }
}
