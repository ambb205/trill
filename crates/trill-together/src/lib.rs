//! The wire between programs that hold the same song.
//!
//! Two or more people work in one song, each in a program of their own.
//! Nobody is the host: every program has all of the song, and they are
//! connected to each other directly, by invitation. What makes that
//! possible is in `trill_core::merge` (a song held as properties that
//! merge) and in the session (which takes changes in and says its own);
//! this crate is only how they reach each other:
//!
//! * [`Invitation`]: where a program listens and the secret it lets one in
//!   by, as text to hand to someone.
//! * [`secure`]: a connection that only holders of the secret can read or
//!   write, by the Noise protocol through the `snow` library.
//! * [`Message`]: what is said over it. Changes to the song, the whole
//!   song for a program that has not got it, and who is there
//!   ([`Presence`]).
//! * [`Net`]: the connections of one program, each with threads of its
//!   own, so that whoever drives the song never waits on the network.
//!
//! **Reached by address.** A program joins wherever it can open a
//! connection to the other: on the same machine, on a local network, over
//! a VPN, through a port someone forwarded. Nothing here introduces two
//! machines that cannot reach each other, and nothing of anyone's runs in
//! between.
//!
//! **Nothing readable or writable without the invitation.** Listening
//! starts when the user shares and stops when they stop; a connection
//! that does not prove the secret is dropped before anything is said.
//!
//! **Everything that arrives is untrusted**: bounded in size and in rate
//! here, and held to the model's own rules where it is taken in
//! (`trill_core::merge::Change::checked`).

mod invite;
mod net;
pub mod secure;
mod wire;

pub use invite::Invitation;
pub use net::{Event, LinkId, Net, Reach};
pub use wire::{MAX_HELD, MAX_MESSAGE, MAX_NAME, Message, PROTOCOL, Person, Presence};

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Waits for something to happen on `net`, for a few seconds.
    fn next(net: &mut Net) -> Event {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(event) = net.events().into_iter().next() {
                return event;
            }
            assert!(Instant::now() < until, "nothing happened");
            std::thread::sleep(Duration::from_millis(5));
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
        let Event::Open { link: theirs, joined: true } = next(&mut joiner) else { panic!("the joiner is connected") };
        let Event::Open { link: ours, joined: false } = next(&mut sharer) else { panic!("and so is the sharer") };

        let presence = Message::Presence(Presence::of(person("Jo")));
        joiner.say(theirs, &presence);
        let Event::Heard { link, message } = next(&mut sharer) else { panic!("it is heard") };
        assert_eq!((link, &message), (ours, &presence));
        sharer.say_to_all(&Message::Gone { person: 7 }, None);
        assert!(matches!(next(&mut joiner), Event::Heard { message: Message::Gone { person: 7 }, .. }));

        // One of them leaves, and says why.
        joiner.close(theirs, "Jo left");
        assert!(matches!(next(&mut joiner), Event::Lost { why, .. } if why == "Jo left"));
        let Event::Lost { link, why } = next(&mut sharer) else { panic!("the other hears of it") };
        assert_eq!(link, ours);
        assert!(why.contains("Jo left"), "{why}");
        assert!(sharer.links().is_empty() && joiner.links().is_empty());
    }

    #[test]
    fn an_invitation_that_is_another_ones_or_no_longer_good_is_a_sentence() {
        let (mut sharer, mut joiner) = (Net::new(), Net::new());
        let invitation = sharer.listen(Reach::ThisMachine, 0).unwrap();
        // The right place, the wrong secret.
        let other = Invitation::new("127.0.0.1", invitation.port).unwrap();
        joiner.join(other);
        let Event::Failed { why } = next(&mut joiner) else { panic!("no connection") };
        assert!(why.contains("did not take this invitation"), "{why}");
        assert!(sharer.events().is_empty() && sharer.links().is_empty(), "and the sharer has nothing of it");

        // Sharing was stopped: the invitation is no longer one.
        sharer.stop_listening();
        assert!(sharer.listening().is_none());
        std::thread::sleep(Duration::from_millis(150));
        joiner.join(invitation);
        let Event::Failed { why } = next(&mut joiner) else { panic!("no connection") };
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
        let Event::Lost { link: lost, why } = next(&mut sharer) else { panic!("and let go of") };
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
        // More at once than may be proving who they are, none of which
        // says a word; then they go.
        let silent: Vec<_> = (0..60).filter_map(|_| std::net::TcpStream::connect(invitation.address()).ok()).collect();
        assert!(silent.len() > 16, "they were connected: {}", silent.len());
        std::thread::sleep(Duration::from_millis(300));
        assert!(sharer.events().is_empty() && sharer.links().is_empty(), "none of them is a program in the song");
        drop(silent);
        // Whoever has the invitation is taken, as soon as there is room.
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            joiner.join(invitation.clone());
            match next(&mut joiner) {
                Event::Open { joined: true, .. } => break,
                Event::Failed { .. } => {
                    assert!(Instant::now() < until, "nobody with the invitation was taken any more")
                }
                other => panic!("{other:?}"),
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let Event::Open { joined: false, .. } = next(&mut sharer) else { panic!("the sharer has it too") };
    }
}
