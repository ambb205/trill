//! A meeting at a meeting point, on this machine: held with a key, asked
//! for by whoever has its invitation, and passed without being read;
//! what becomes of it when its holder is lost or ends it, and what each
//! limit says to whoever it turns away.
//!
//! (The number beside a test is how many connections it makes.)

mod common;

use common::{ask, heard_until_closed, hold, invited, point, secured};
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use trill_meet::keys::Given;
use trill_meet::text::hex;
use trill_meet::{Key, Limits, No, Turned};

/// 6.
#[test]
fn a_meeting_is_held_with_a_key_and_whoever_asks_for_it_is_piped_to_its_holder_unread() {
    let key = Key::new().unwrap();
    let point = point(Limits::default(), &[(&key, "Ada")]);
    let secret = [42u8; 20];
    let invitation = invited(&secret);
    let (meeting, door) = invitation;
    let mut holding = hold(&point.address, &key, invitation).unwrap();
    let counts = point.until("a meeting is held", |counts| counts.held == 1);
    assert_eq!((counts.meetings, counts.connections, counts.pipes), (1, 1, 0));
    assert_eq!(holding.next(Duration::from_millis(100)), Ok(None), "nobody has knocked");

    // Someone asks for it, and is piped to its holder.
    let (ours, theirs) = point.pipe(&mut holding, invitation);
    let counts = point.until("someone is in it", |counts| counts.pipes == 1);
    assert_eq!((counts.connections, counts.knocks), (3, 0), "a meeting of two is three connections");
    // Through the pipe the two prove a secret to each other, which the
    // meeting point has not got, and say megabytes both ways.
    let ((mut say, mut hear), (mut their_say, mut their_hear)) = secured(ours, theirs, [7u8; 32]);
    let much: Vec<u8> = (0..3_000_000u32).map(|n| (n * 7 + n / 251) as u8).collect();
    let sent = much.clone();
    let saying = std::thread::spawn(move || {
        their_say.say(&sent).unwrap();
        their_say.say(b"and a few words").unwrap();
        their_say
    });
    assert!(hear.hear(10_000_000).unwrap() == much, "megabytes arrive as they were sent");
    assert_eq!(hear.hear(100).unwrap(), b"and a few words");
    say.say(b"and back").unwrap();
    assert_eq!(their_hear.hear(100).unwrap(), b"and back");
    let mut their_say = saying.join().unwrap();

    // A second person, while the first is in it. What it says here is in
    // the clear, to be looked for in the log afterwards.
    let (mut second, mut their_second) = point.pipe(&mut holding, invitation);
    let counts = point.until("two are in it", |counts| counts.pipes == 2);
    assert_eq!(counts.connections, 5);
    their_second.write_all(b"SAID-IN-THE-CLEAR").unwrap();
    let mut heard = [0u8; 17];
    second.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"SAID-IN-THE-CLEAR");
    // One leaves, and the other stays.
    drop(their_second);
    point.until("one is left", |counts| counts.pipes == 1 && counts.connections == 3);
    drop(second);
    say.say(b"still here").unwrap();
    assert_eq!(their_hear.hear(100).unwrap(), b"still here");

    // The holder ends the meeting: whoever asks from here on is told that
    // it is over. Who is in it stays connected, as with a program that
    // stops listening.
    holding.close();
    point.until("the meeting is over", |counts| counts.meetings == 0);
    let turned = ask(&point.address, invitation).unwrap_err();
    assert_eq!(turned, Turned::No(No::Closed));
    assert!(turned.for_good() && turned.sentence().contains("stopped sharing it"), "{}", turned.sentence());
    their_say.say(b"after").unwrap();
    assert_eq!(hear.hear(100).unwrap(), b"after");
    drop((their_say, their_hear));
    point.until("everyone has gone", |counts| counts.connections == 0 && counts.pipes == 0);
    drop((say, hear));

    // The log says what happened, by the name the key was given to and a
    // few digits of the meeting; it holds no key, no secret, neither of
    // the two things the meeting is known by, and nothing of what was
    // passed.
    let log = point.log();
    for said in [
        &format!("Ada holds a meeting ({})", meeting.tag()),
        "someone is in it; 2 in it",
        "someone is in it; 3 in it",
        "someone is out of it, one side closed it",
        "is over: its holder ended it",
    ] {
        assert!(log.contains(said), "\"{said}\" is not in:\n{log}");
    }
    let key_as_text = key.to_string();
    let kept_out =
        [key_as_text[10..].to_owned(), hex(&key.proof()), meeting.to_string(), door.to_string(), hex(&secret)];
    for kept_out in kept_out.iter().map(String::as_str).chain(["SAID-IN-THE-CLEAR"]) {
        assert!(!log.contains(kept_out), "\"{kept_out}\" is in the log:\n{log}");
    }
    assert_eq!(log.matches("turned away").count(), 1, "the one who asked after it was over:\n{log}");
    assert!(!log.contains("fault"), "{log}");
}

/// 10.
#[test]
fn a_holder_that_is_lost_is_waited_for_and_one_that_stays_away_is_forgotten() {
    let (ada, ben) = (Key::new().unwrap(), Key::new().unwrap());
    let given = [(&ada, "Ada"), (&ben, "Ben")];
    let point = point(Limits::default(), &given);
    let invitation = invited(b"an invitation's secret");
    let mut holding = hold(&point.address, &ada, invitation).unwrap();
    let (mut ours, mut theirs) = point.pipe(&mut holding, invitation);

    // Its connection is lost without a word.
    drop(holding);
    point.until("its holder is away", |counts| counts.meetings == 1 && counts.held == 0);
    let turned = ask(&point.address, invitation).unwrap_err();
    assert_eq!(turned, Turned::No(No::Away));
    assert!(!turned.for_good() && turned.sentence().contains("not connected to the meeting point at the moment"));
    // Who was in it is in it still: theirs is a connection of its own.
    theirs.write_all(b"still").unwrap();
    let mut heard = [0u8; 5];
    ours.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"still");
    // And nobody else holds it meanwhile.
    let turned = hold(&point.address, &ben, invitation).unwrap_err();
    assert_eq!(turned, Turned::No(No::Taken));

    // It is back before it is forgotten, and whoever asks is let in again.
    let mut holding = hold(&point.address, &ada, invitation).unwrap();
    point.until("it is held again", |counts| counts.meetings == 1 && counts.held == 1);
    let again = point.pipe(&mut holding, invitation);
    point.until_said(&format!("Ada holds the meeting ({}) again", invitation.0.tag()));
    drop(theirs);
    point.until("the first is out of it", |counts| counts.pipes == 1);
    drop((ours, again.1));
    point.until("and the second", |counts| counts.pipes == 0);
    drop((again.0, holding));

    // Away for longer than a meeting is remembered: at a meeting point
    // that remembers one for a moment.
    let brief = common::point(Limits { away: Duration::from_millis(300), ..Limits::default() }, &given);
    let holding = hold(&brief.address, &ada, invitation).unwrap();
    brief.until("it is held", |counts| counts.held == 1);
    drop(holding);
    brief.until("it is forgotten", |counts| counts.meetings == 0);
    assert_eq!(ask(&brief.address, invitation).unwrap_err(), Turned::No(No::Unknown));
    let log = brief.log();
    assert!(log.contains("is away: remembered for 0.3 s"), "{log}");
    assert!(log.contains("is forgotten: its holder has been away for 0.3 s"), "{log}");
    assert!(No::Unknown.sentence().contains("no longer good") && !Turned::No(No::Unknown).for_good());
}

/// 10.
#[test]
fn as_many_meetings_and_people_as_the_limits_say_and_no_more_each_turned_away_in_a_sentence() {
    let (ada, ben) = (Key::new().unwrap(), Key::new().unwrap());
    let limits = Limits { meetings: 2, meetings_of_one_key: 1, people: 2, ..Limits::default() };
    let point = point(limits, &[(&ada, "Ada"), (&ben, "Ben")]);
    let at = point.address.as_str();
    let invitations: Vec<_> = (0..3u8).map(|number| invited(&[number; 20])).collect();

    // A key holds one, and the meeting point two.
    let mut first = hold(at, &ada, invitations[0]).unwrap();
    assert_eq!(hold(at, &ada, invitations[1]).unwrap_err(), Turned::No(No::Yours));
    let _second = hold(at, &ben, invitations[1]).unwrap();
    assert_eq!(hold(at, &ben, invitations[2]).unwrap_err(), Turned::No(No::Meetings));
    assert_eq!(point.server.counts().meetings, 2);

    // A meeting has two people: its holder and one more.
    let one = point.pipe(&mut first, invitations[0]);
    assert_eq!(ask(at, invitations[0]).unwrap_err(), Turned::No(No::Seats));
    drop(one.1);
    point.until("the seat is free", |counts| counts.pipes == 0);

    // A holder that does not take a knock up: whoever knocked is told so
    // when the time for it is over, and the knock is good no longer. (At
    // a meeting point that waits a moment for it.)
    let brief = common::point(Limits { answer: Duration::from_millis(300), ..Limits::default() }, &[(&ada, "Ada")]);
    let mut slow = hold(&brief.address, &ada, invitations[0]).unwrap();
    let began = Instant::now();
    assert_eq!(ask(&brief.address, invitations[0]).unwrap_err(), Turned::No(No::Slow));
    assert!(began.elapsed() < Duration::from_secs(8), "after {:?}", began.elapsed());
    let ticket = slow.next(Duration::from_secs(10)).unwrap().expect("the holder was told of it");
    assert_eq!(trill_meet::client::take(&brief.address, ticket).unwrap_err(), Turned::No(No::Ticket));
    brief.until("no knock is left", |counts| counts.knocks == 0 && counts.pipes == 0);

    let log = format!("{}\n{}", point.log(), brief.log());
    for (no, in_the_log) in [
        (No::Yours, "as many meetings as one key may hold at once (1)"),
        (No::Meetings, "as many meetings as are held at once (2)"),
        (No::Seats, "as many people as a meeting has (2)"),
        (No::Slow, "the meeting's holder did not take the knock up within 0.3 s"),
        (No::Ticket, "no knock goes by its ticket"),
    ] {
        let line = format!("turned away ({}), from 127.0.0.1: {in_the_log}", no.word());
        assert!(log.contains(&line), "\"{line}\" is not in:\n{log}");
        assert!(!Turned::No(no).for_good() || no == No::Ticket);
    }
}

/// 12.
#[test]
fn only_a_key_its_keeper_gave_opens_a_meeting_and_one_that_is_taken_away_ends_what_it_held_by_every_route() {
    let keys: Vec<Key> = (0..4).map(|_| Key::new().unwrap()).collect();
    let (ada, ben, cy, nobodys) = (&keys[0], &keys[1], &keys[2], &keys[3]);
    let point = point(Limits::default(), &[(ada, "Ada"), (ben, "Ben"), (cy, "Cy")]);
    let at = point.address.as_str();
    let (adas, cys) = (invited(b"Ada's"), invited(b"Cy's"));

    // A key nobody was given, and one that is Ada's by what it says it is
    // and not by its secret: both are answered the same.
    assert_eq!(hold(at, nobodys, adas).unwrap_err(), Turned::No(No::Key));
    let text = ada.to_string();
    let last = if text.ends_with('0') { '1' } else { '0' };
    let forged = Key::parse(&format!("{}{last}", &text[..text.len() - 1])).unwrap();
    assert_eq!(forged.id(), ada.id());
    let turned = hold(at, &forged, adas).unwrap_err();
    assert_eq!(turned, Turned::No(No::Key));
    assert!(turned.for_good() && turned.sentence().contains("did not take the key"), "{}", turned.sentence());
    point.until("nothing is held for them", |counts| counts.meetings == 0 && counts.connections == 0);

    // Ada and Cy each hold a meeting with someone in it. Ada's program is
    // there and says so often; Cy's connection was lost a moment ago, and
    // the two in Cy's meeting go on talking.
    let mut ada_holds = hold(at, ada, adas).unwrap();
    ada_holds.say_it_is_there_every(Duration::from_millis(100));
    let (_adas_end, in_adas) = point.pipe(&mut ada_holds, adas);
    let mut cy_holds = hold(at, cy, cys).unwrap();
    let (mut cys_end, mut in_cys) = point.pipe(&mut cy_holds, cys);
    drop(cy_holds);
    point.until("Cy's holder is away", |counts| counts.held == 1 && counts.meetings == 2 && counts.pipes == 2);
    in_cys.write_all(b"on").unwrap();
    let mut heard = [0u8; 2];
    cys_end.read_exact(&mut heard).unwrap();

    // Both keys are taken away: two lines out of the keeper's file.
    point.server.keys().set(vec![Given::of(ben, "Ben")]);
    // Ada hears of it the next time she says that she is there, or finds
    // her connection closed, whichever the meeting point gets to first.
    let end = Instant::now() + Duration::from_secs(20);
    let turned = loop {
        match ada_holds.next(Duration::from_millis(50)) {
            Ok(None) => assert!(Instant::now() < end, "she never heard of it"),
            Ok(Some(_)) => panic!("nobody knocked"),
            Err(turned) => break turned,
        }
    };
    assert!(matches!(turned, Turned::No(No::Key) | Turned::NoAnswer(_)), "{turned:?}");
    // What each held is ended, the people in it too: Cy's as well, whose
    // holder is not there to be told.
    assert_eq!(heard_until_closed(&in_adas).0, "", "who was in her meeting is out of it");
    assert_eq!(heard_until_closed(&in_cys).0, "", "and who was in his");
    point.until("nothing of theirs is left", |counts| counts.meetings == 0 && counts.pipes == 0);
    assert_eq!(ask(at, adas).unwrap_err(), Turned::No(No::Unknown));
    assert_eq!(hold(at, ada, adas).unwrap_err(), Turned::No(No::Key));
    // A key that was not taken away opens a meeting as before.
    let _bens = hold(at, ben, invited(b"Ben's")).unwrap();
    let log = point.log();
    for name in ["Ada", "Cy"] {
        let line = format!("the key of {name} was taken away: what it held is ended");
        assert_eq!(log.matches(&line).count(), 1, "\"{line}\" once in:\n{log}");
    }
    assert!(log.contains("someone is out of it, the key was taken away"), "{log}");
    assert!(log.contains("turned away (key), from 127.0.0.1: the key it named is none that is given"), "{log}");
    assert!(!log.contains(&text[10..]) && !log.contains(&hex(&ada.proof())), "no key is in the log:\n{log}");
}

/// 5.
#[test]
fn a_key_that_has_passed_its_bytes_of_the_day_passes_no_more_and_is_told_so() {
    let key = Key::new().unwrap();
    let point = point(Limits { bytes_a_day: 1 << 20, ..Limits::default() }, &[(&key, "Ada")]);
    let at = point.address.as_str();
    let invitation = invited(b"one");
    let mut holding = hold(at, &key, invitation).unwrap();
    holding.say_it_is_there_every(Duration::from_millis(100));
    let (ours, theirs) = point.pipe(&mut holding, invitation);

    // Four hundred kilobytes each way are within the megabyte.
    let mut heard = vec![0u8; 400_000];
    for (from, mut to) in [(&theirs, &ours), (&ours, &theirs)] {
        let mut from = from.try_clone().unwrap();
        let saying = std::thread::spawn(move || from.write_all(&[7u8; 400_000]));
        to.read_exact(&mut heard).unwrap();
        saying.join().unwrap().unwrap();
    }
    assert!(!holding.spent());
    // The next four hundred pass it: the pipe is ended, and less than
    // all of them arrived.
    let mut saying = theirs.try_clone().unwrap();
    let said = std::thread::spawn(move || saying.write_all(&[7u8; 400_000]));
    let (arrived, _) = heard_until_closed(&ours);
    assert!(arrived.len() < 400_000, "{} arrived", arrived.len());
    let _ = said.join().unwrap();
    point.until("the pipe is ended", |counts| counts.pipes == 0);

    // Whoever asks for the meeting is told why not, and so is its holder,
    // which holds it on for the day after.
    let turned = ask(at, invitation).unwrap_err();
    assert_eq!(turned, Turned::No(No::Spent));
    assert!(!turned.for_good() && turned.sentence().contains("as it may in a day"), "{}", turned.sentence());
    let end = Instant::now() + Duration::from_secs(20);
    while !holding.spent() {
        assert_eq!(holding.next(Duration::from_millis(50)), Ok(None));
        assert!(Instant::now() < end, "its holder never heard of it");
    }
    assert_eq!(point.server.counts().held, 1);
    assert_eq!(hold(at, &key, invited(b"another")).unwrap_err(), Turned::No(No::Spent));
    let log = point.log();
    assert_eq!(log.matches("the key of Ada has passed its 1 MB of the day").count(), 1, "{log}");
    assert!(log.contains("someone is out of it, the key's bytes of the day are passed"), "{log}");
    assert!(log.contains("turned away (spent), from 127.0.0.1: the key has passed its 1 MB of the day"), "{log}");
}

/// 5.
#[test]
fn a_connection_over_which_nothing_passes_is_not_kept() {
    let key = Key::new().unwrap();
    let point = point(Limits { silent: Duration::from_secs(3), ..Limits::default() }, &[(&key, "Ada")]);
    let invitation = invited(b"one");
    let mut holding = hold(&point.address, &key, invitation).unwrap();
    holding.say_it_is_there_every(Duration::from_millis(200));
    let (_silent, their_silent) = point.pipe(&mut holding, invitation);
    let (mut ours, mut theirs) = point.pipe(&mut holding, invitation);

    // One pipe says something now and then, and the holder says that it
    // is there; the other says nothing.
    let began = Instant::now();
    let mut heard = [0u8; 1];
    while point.server.counts().pipes == 2 {
        assert!(began.elapsed() < Duration::from_secs(20), "it was kept:\n{}", point.log());
        theirs.write_all(b".").unwrap();
        ours.read_exact(&mut heard).unwrap();
        assert_eq!(holding.next(Duration::from_millis(50)), Ok(None));
    }
    assert_eq!(heard_until_closed(&their_silent).0, "");
    assert!(point.log().contains("nothing passed for as long as a silent connection is kept"), "{}", point.log());
    assert_eq!(point.server.counts().held, 1, "the holder that says it is there is kept");
    theirs.write_all(b".").unwrap();
    ours.read_exact(&mut heard).unwrap();

    // A holder that says nothing any more is taken to be away.
    point.until("its holder is taken to be away", |counts| counts.held == 0 && counts.meetings == 1);
    assert!(matches!(holding.next(Duration::from_secs(10)), Err(Turned::NoAnswer(_))));
}

/// 5.
#[test]
fn a_meeting_point_that_is_stopped_closes_everything_and_listens_no_more() {
    let key = Key::new().unwrap();
    let point = point(Limits::default(), &[(&key, "Ada")]);
    let invitation = invited(b"one");
    let mut holding = hold(&point.address, &key, invitation).unwrap();
    let (ours, theirs) = point.pipe(&mut holding, invitation);
    let waiting = point.stranger();
    let address = point.address.clone();
    point.server.stop();
    for end in [&ours, &theirs, &waiting] {
        heard_until_closed(end);
    }
    assert!(matches!(holding.next(Duration::from_secs(10)), Err(Turned::NoAnswer(_))));
    assert!(matches!(ask(&address, invitation), Err(Turned::NoAnswer(_))));
}
