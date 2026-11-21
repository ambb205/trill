//! A meeting at a meeting point, on this machine: held with a key, asked
//! for by what it goes by, and passed without being read; what becomes of
//! it when its holder is lost or ends it, and what each limit says to
//! whoever it turns away.

mod common;

use common::{heard_until_closed, point, secured};
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use trill_meet::keys::Given;
use trill_meet::text::hex;
use trill_meet::{Holding, Key, Limits, Meeting, No, Turned, client};

#[test]
fn a_meeting_is_held_with_a_key_and_whoever_asks_for_it_is_piped_to_its_holder_unread() {
    let key = Key::new().unwrap();
    let point = point(Limits::default(), &[(&key, "Ada")]);
    let secret = [42u8; 20];
    let meeting = Meeting::of(&secret);
    let mut holding = Holding::open(&point.address, &key, meeting).unwrap();
    let counts = point.until("a meeting is held", |counts| counts.held == 1);
    assert_eq!((counts.meetings, counts.connections, counts.pipes), (1, 1, 0));
    assert_eq!(holding.next(Duration::from_millis(100)), Ok(None), "nobody has knocked");

    // Someone asks for it, and is piped to its holder.
    let (ours, theirs) = point.pipe(&mut holding, meeting);
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
    let (mut second, mut their_second) = point.pipe(&mut holding, meeting);
    let counts = point.until("two are in it", |counts| counts.pipes == 2);
    assert_eq!(counts.connections, 5);
    their_second.write_all(b"SAID-IN-THE-CLEAR").unwrap();
    let mut heard = [0u8; 17];
    second.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"SAID-IN-THE-CLEAR");
    // One leaves, and the other stays.
    drop((second, their_second));
    point.until("one is left", |counts| counts.pipes == 1 && counts.connections == 3);
    say.say(b"still here").unwrap();
    assert_eq!(their_hear.hear(100).unwrap(), b"still here");

    // The holder ends the meeting: whoever asks from here on is told that
    // it is over. Who is in it stays connected, as with a program that
    // stops listening.
    holding.close();
    point.until("the meeting is over", |counts| counts.meetings == 0);
    let turned = client::join(&point.address, meeting).unwrap_err();
    assert_eq!(turned, Turned::No(No::Closed));
    assert!(turned.for_good() && turned.sentence().contains("stopped sharing it"), "{}", turned.sentence());
    their_say.say(b"after").unwrap();
    assert_eq!(hear.hear(100).unwrap(), b"after");
    drop((say, hear, their_say, their_hear));
    point.until("everyone has gone", |counts| counts.connections == 0 && counts.pipes == 0);

    // The log says what happened, by the name the key was given to and a
    // few digits of the meeting; it holds no key, no secret, not what the
    // meeting goes by, and nothing of what was passed.
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
    let kept_out = [key_as_text[10..].to_owned(), hex(&key.proof()), meeting.to_string(), hex(&secret)];
    for kept_out in kept_out.iter().map(String::as_str).chain(["SAID-IN-THE-CLEAR"]) {
        assert!(!log.contains(kept_out), "\"{kept_out}\" is in the log:\n{log}");
    }
    assert_eq!(log.matches("turned away").count(), 1, "the one who asked after it was over:\n{log}");
    assert!(!log.contains("fault"), "{log}");
}

#[test]
fn a_holder_that_is_lost_is_waited_for_and_one_that_stays_away_is_forgotten() {
    let (ada, ben) = (Key::new().unwrap(), Key::new().unwrap());
    let given = [(&ada, "Ada"), (&ben, "Ben")];
    let point = point(Limits::default(), &given);
    let meeting = Meeting::of(b"an invitation's secret");
    let mut holding = Holding::open(&point.address, &ada, meeting).unwrap();
    let (mut ours, mut theirs) = point.pipe(&mut holding, meeting);

    // Its connection is lost without a word.
    drop(holding);
    point.until("its holder is away", |counts| counts.meetings == 1 && counts.held == 0);
    let turned = client::join(&point.address, meeting).unwrap_err();
    assert_eq!(turned, Turned::No(No::Away));
    assert!(!turned.for_good() && turned.sentence().contains("not connected to the meeting point at the moment"));
    // Who was in it is in it still: theirs is a connection of its own.
    theirs.write_all(b"still").unwrap();
    let mut heard = [0u8; 5];
    ours.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"still");
    // And nobody else holds it meanwhile.
    let turned = Holding::open(&point.address, &ben, meeting).unwrap_err();
    assert_eq!(turned, Turned::No(No::Taken));

    // It is back before it is forgotten, and whoever asks is let in again.
    let mut holding = Holding::open(&point.address, &ada, meeting).unwrap();
    point.until("it is held again", |counts| counts.meetings == 1 && counts.held == 1);
    let again = point.pipe(&mut holding, meeting);
    point.until_said(&format!("Ada holds the meeting ({}) again", meeting.tag()));

    drop((holding, again));

    // Away for longer than a meeting is remembered: at a meeting point
    // that remembers one for a moment.
    let brief = common::point(Limits { away: Duration::from_millis(300), ..Limits::default() }, &given);
    let holding = Holding::open(&brief.address, &ada, meeting).unwrap();
    brief.until("it is held", |counts| counts.held == 1);
    drop(holding);
    brief.until("it is forgotten", |counts| counts.meetings == 0);
    assert_eq!(client::join(&brief.address, meeting).unwrap_err(), Turned::No(No::Unknown));
    let log = brief.log();
    assert!(log.contains("is away: remembered for 0.3 s"), "{log}");
    assert!(log.contains("is forgotten: its holder has been away for 0.3 s"), "{log}");
    assert!(No::Unknown.sentence().contains("no longer good") && !Turned::No(No::Unknown).for_good());
    // Then it is anyone's with a key to hold.
    let _bens = Holding::open(&brief.address, &ben, meeting).unwrap();
    brief.until_said(&format!("Ben holds a meeting ({})", meeting.tag()));
}

#[test]
fn as_many_meetings_and_people_as_the_limits_say_and_no_more_each_turned_away_in_a_sentence() {
    let (ada, ben) = (Key::new().unwrap(), Key::new().unwrap());
    let limits = Limits { meetings: 3, meetings_of_one_key: 2, people: 3, ..Limits::default() };
    let point = point(limits, &[(&ada, "Ada"), (&ben, "Ben")]);
    let at = point.address.as_str();
    let meetings: Vec<Meeting> = (0..4u8).map(|number| Meeting::of(&[number; 20])).collect();

    let mut first = Holding::open(at, &ada, meetings[0]).unwrap();
    let _second = Holding::open(at, &ada, meetings[1]).unwrap();
    // A key holds two, and the meeting point three.
    assert_eq!(Holding::open(at, &ada, meetings[2]).unwrap_err(), Turned::No(No::Yours));
    let _third = Holding::open(at, &ben, meetings[2]).unwrap();
    assert_eq!(Holding::open(at, &ben, meetings[3]).unwrap_err(), Turned::No(No::Meetings));
    assert_eq!(point.server.counts().meetings, 3);

    // A meeting has three people: its holder and two more.
    let one = point.pipe(&mut first, meetings[0]);
    let two = point.pipe(&mut first, meetings[0]);
    assert_eq!(client::join(at, meetings[0]).unwrap_err(), Turned::No(No::Seats));
    drop(one);
    point.until("a seat is free", |counts| counts.pipes == 1);
    let _three = point.pipe(&mut first, meetings[0]);
    drop(two);
    point.until("a seat is free again", |counts| counts.pipes == 1);

    // A holder that does not take a knock up: whoever knocked is told so
    // when the time for it is over, and the knock is good no longer. (At
    // a meeting point that waits a moment for it.)
    let brief = common::point(Limits { answer: Duration::from_millis(300), ..Limits::default() }, &[(&ada, "Ada")]);
    let mut slow = Holding::open(&brief.address, &ada, meetings[0]).unwrap();
    let began = Instant::now();
    assert_eq!(client::join(&brief.address, meetings[0]).unwrap_err(), Turned::No(No::Slow));
    assert!(began.elapsed() < Duration::from_secs(8), "after {:?}", began.elapsed());
    let ticket = slow.next(Duration::from_secs(10)).unwrap().expect("the holder was told of it");
    assert_eq!(client::take(&brief.address, ticket).unwrap_err(), Turned::No(No::Ticket));
    brief.until("no knock is left", |counts| counts.knocks == 0 && counts.pipes == 0);

    let log = format!("{}\n{}", point.log(), brief.log());
    for (no, in_the_log) in [
        (No::Yours, "as many meetings as one key may hold at once (2)"),
        (No::Meetings, "as many meetings as are held at once (3)"),
        (No::Seats, "as many people as a meeting has (3)"),
        (No::Slow, "the meeting's holder did not take the knock up within 0.3 s"),
        (No::Ticket, "no knock goes by its ticket"),
    ] {
        let line = format!("turned away ({}), from 127.0.0.1: {in_the_log}", no.word());
        assert!(log.contains(&line), "\"{line}\" is not in:\n{log}");
        assert!(!Turned::No(no).for_good() || no == No::Ticket);
    }
}

#[test]
fn only_a_key_its_keeper_gave_opens_a_meeting_and_one_that_is_taken_away_ends_what_it_held() {
    let (ada, ben, nobodys) = (Key::new().unwrap(), Key::new().unwrap(), Key::new().unwrap());
    let point = point(Limits::default(), &[(&ada, "Ada"), (&ben, "Ben")]);
    let at = point.address.as_str();
    let (meeting, other) = (Meeting::of(b"one"), Meeting::of(b"another"));

    // A key nobody was given, and one that is Ada's by what it says it is
    // and not by its secret: both are answered the same.
    assert_eq!(Holding::open(at, &nobodys, meeting).unwrap_err(), Turned::No(No::Key));
    let text = ada.to_string();
    let last = if text.ends_with('0') { '1' } else { '0' };
    let forged = Key::parse(&format!("{}{last}", &text[..text.len() - 1])).unwrap();
    assert_eq!(forged.id(), ada.id());
    let turned = Holding::open(at, &forged, meeting).unwrap_err();
    assert_eq!(turned, Turned::No(No::Key));
    assert!(turned.for_good() && turned.sentence().contains("did not take the key"), "{}", turned.sentence());
    point.until("nothing is held for them", |counts| counts.meetings == 0 && counts.connections == 0);

    // Ada and Ben each hold a meeting with someone in it.
    let mut adas = Holding::open(at, &ada, meeting).unwrap();
    adas.say_it_is_there_every(Duration::from_millis(100));
    let mut bens = Holding::open(at, &ben, other).unwrap();
    let (_ours, theirs) = point.pipe(&mut adas, meeting);
    let (mut bens_end, mut their_end) = point.pipe(&mut bens, other);

    // Ada's key is taken away: a line out of the keeper's file. She hears
    // of it the next time she says that she is there, and what she held
    // is ended, the people in it too.
    point.server.keys().set(vec![Given::of(&ben, "Ben")]);
    let end = Instant::now() + Duration::from_secs(10);
    let turned = loop {
        match adas.next(Duration::from_millis(50)) {
            Ok(None) => assert!(Instant::now() < end, "she never heard of it"),
            Ok(Some(_)) => panic!("nobody knocked"),
            Err(turned) => break turned,
        }
    };
    assert_eq!(turned, Turned::No(No::Key));
    assert_eq!(heard_until_closed(&theirs).0, "", "who was in her meeting is out of it");
    point.until("hers is gone and his is not", |counts| counts.meetings == 1 && counts.pipes == 1);
    assert_eq!(client::join(at, meeting).unwrap_err(), Turned::No(No::Unknown));
    assert_eq!(Holding::open(at, &ada, meeting).unwrap_err(), Turned::No(No::Key));
    // Ben's goes on.
    their_end.write_all(b"on").unwrap();
    let mut heard = [0u8; 2];
    bens_end.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"on");
    let log = point.log();
    assert!(log.contains("the key of Ada was taken away: what it held is ended"), "{log}");
    assert!(log.contains("turned away (key), from 127.0.0.1: the key it named is none that is given"), "{log}");
    assert!(!log.contains(&text[10..]) && !log.contains(&hex(&ada.proof())), "no key is in the log:\n{log}");
}

#[test]
fn a_key_that_has_passed_its_bytes_of_the_day_passes_no_more_and_is_told_so() {
    let key = Key::new().unwrap();
    let point = point(Limits { bytes_a_day: 1 << 20, ..Limits::default() }, &[(&key, "Ada")]);
    let at = point.address.as_str();
    let meeting = Meeting::of(b"one");
    let mut holding = Holding::open(at, &key, meeting).unwrap();
    holding.say_it_is_there_every(Duration::from_millis(100));
    let (ours, theirs) = point.pipe(&mut holding, meeting);

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
    let turned = client::join(at, meeting).unwrap_err();
    assert_eq!(turned, Turned::No(No::Spent));
    assert!(!turned.for_good() && turned.sentence().contains("as it may in a day"), "{}", turned.sentence());
    let end = Instant::now() + Duration::from_secs(10);
    while !holding.spent() {
        assert_eq!(holding.next(Duration::from_millis(50)), Ok(None));
        assert!(Instant::now() < end, "its holder never heard of it");
    }
    assert_eq!(point.server.counts().held, 1);
    assert_eq!(Holding::open(at, &key, Meeting::of(b"another")).unwrap_err(), Turned::No(No::Spent));
    let log = point.log();
    assert_eq!(log.matches("the key of Ada has passed its 1 MB of the day").count(), 1, "{log}");
    assert!(log.contains("someone is out of it, the key's bytes of the day are passed"), "{log}");
    assert!(log.contains("turned away (spent), from 127.0.0.1: the key has passed its 1 MB of the day"), "{log}");
}

#[test]
fn a_connection_over_which_nothing_passes_is_not_kept() {
    let key = Key::new().unwrap();
    let point = point(Limits { silent: Duration::from_secs(3), ..Limits::default() }, &[(&key, "Ada")]);
    let meeting = Meeting::of(b"one");
    let mut holding = Holding::open(&point.address, &key, meeting).unwrap();
    holding.say_it_is_there_every(Duration::from_millis(200));
    let (_silent, their_silent) = point.pipe(&mut holding, meeting);
    let (mut ours, mut theirs) = point.pipe(&mut holding, meeting);

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

#[test]
fn a_meeting_point_that_is_stopped_closes_everything_and_listens_no_more() {
    let key = Key::new().unwrap();
    let point = point(Limits::default(), &[(&key, "Ada")]);
    let meeting = Meeting::of(b"one");
    let mut holding = Holding::open(&point.address, &key, meeting).unwrap();
    let (ours, theirs) = point.pipe(&mut holding, meeting);
    let waiting = point.stranger();
    let address = point.address.clone();
    point.server.stop();
    for end in [&ours, &theirs, &waiting] {
        heard_until_closed(end);
    }
    assert!(matches!(holding.next(Duration::from_secs(5)), Err(Turned::NoAnswer(_))));
    assert!(matches!(client::join(&address, meeting), Err(Turned::NoAnswer(_))));
}
