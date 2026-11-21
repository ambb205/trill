//! Whatever reaches a meeting point is a stranger's: the slow, the
//! silent, the ones that say a length or a line that is none, many at
//! once, and someone with a key who says what does not belong there.
//! Each is let go of in its time, costs a line in the log, and leaves
//! the meeting point as it was for whoever comes after.

mod common;

use common::{heard_until_closed, point, quick, say};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use trill_meet::said::{Hello, MAX_WORD, Word};
use trill_meet::secure::{Hearer, Room, Sayer, secure_in};
use trill_meet::{Holding, Key, Limits, Meeting, No, Ticket, Turned, client};

/// Reads the `ok` a first line was answered with.
fn answered_ok(mut stream: &TcpStream) {
    let mut answer = [0u8; 3];
    stream.read_exact(&mut answer).unwrap();
    assert_eq!(&answer, b"ok\n");
}

#[test]
fn a_stranger_who_says_nothing_or_too_slowly_or_too_much_or_nonsense_is_let_go_of_in_its_time() {
    let key = Key::new().unwrap();
    let point = point(quick(), &[(&key, "Ada")]);

    // Nothing at all.
    let (heard, took) = heard_until_closed(&point.stranger());
    assert_eq!(heard, "no late\n");
    assert!(took > Duration::from_millis(1500) && took < Duration::from_secs(12), "after {took:?}");

    // A byte every so often: each arrives well within the time the whole
    // has, and the whole has no longer for it.
    let slow = point.stranger();
    let began = Instant::now();
    for byte in b"trill-meet 1 join 0000".iter().cycle() {
        if (&slow).write_all(&[*byte]).is_err() || began.elapsed() > Duration::from_secs(20) {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(began.elapsed() < Duration::from_secs(12), "it was let go of after {:?}", began.elapsed());

    // A megabyte with no end of line: no more of it is read than a line
    // may be, and no room is made for the rest.
    let much = point.stranger();
    let mut saying = much.try_clone().unwrap();
    let said = std::thread::spawn(move || saying.write_all(&vec![b'a'; 1 << 20]));
    let (_, took) = heard_until_closed(&much);
    assert!(took < Duration::from_secs(12), "after {took:?}");
    let _ = said.join().unwrap();

    // Lines that are no first line, each answered and let go of at once.
    let nobodys = Meeting::of(b"nobody opened this");
    for (said, answer) in [
        ("GET / HTTP/1.1".to_owned(), "no what"),
        (String::new(), "no what"),
        ("trill-meet 1 dance now".to_owned(), "no what"),
        (format!("trill-meet 2 join {nobodys}"), "no version"),
        // A meeting nobody opened, and a knock nobody was told of.
        (Hello::Join(nobodys).line(), "no unknown"),
        (Hello::Take(Ticket::new().unwrap()).line(), "no ticket"),
    ] {
        let stranger = point.stranger();
        say(&stranger, &said);
        let (heard, took) = heard_until_closed(&stranger);
        assert_eq!(heard, format!("{answer}\n"), "to \"{said}\"");
        assert!(took < Duration::from_secs(10), "after {took:?}");
    }
    let stranger = point.stranger();
    (&stranger).write_all(&[0xff, 0xfe, 0x00, b'\n']).unwrap();
    assert_eq!(heard_until_closed(&stranger).0, "no what\n", "bytes that are no text");

    // Someone who names a key, is answered like anyone who does, and then
    // says nothing more, or a length nothing has, or what is no part of
    // the handshake.
    let mut no_handshake = vec![0u8, 48];
    no_handshake.extend((0..48u8).map(|n| n.wrapping_mul(77)));
    for (after, answer) in [(Vec::new(), ""), (vec![0xff, 0xff], ""), (no_handshake, "\0\0")] {
        for named in [key.id(), Key::new().unwrap().id()] {
            let stranger = point.stranger();
            say(&stranger, &Hello::Open(named).line());
            answered_ok(&stranger);
            (&stranger).write_all(&after).unwrap();
            let (heard, took) = heard_until_closed(&stranger);
            assert_eq!(heard, answer, "a key that is given and one that is not are answered the same");
            assert!(took < Duration::from_secs(12), "after {took:?}");
        }
    }

    // None of them left anything behind, and a meeting is held and asked
    // for as if none of it had been.
    let counts = point.until("nothing is left of them", |counts| counts.connections == 0);
    assert_eq!((counts.meetings, counts.knocks, counts.pipes), (0, 0, 0));
    let meeting = Meeting::of(b"an invitation's secret");
    let mut holding = Holding::open(&point.address, &key, meeting).unwrap();
    let (mut ours, mut theirs) = point.pipe(&mut holding, meeting);
    theirs.write_all(b"as ever").unwrap();
    let mut heard = [0u8; 7];
    ours.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"as ever");

    let log = point.log();
    for kind in ["late", "what", "version", "unknown", "ticket", "key"] {
        let line = format!("turned away ({kind}), from 127.0.0.1: ");
        assert!(log.contains(&line), "\"{line}\" is not in:\n{log}");
    }
    assert!(log.contains("it did not say what it came for within 2 s"), "{log}");
    assert!(!log.contains("fault") && !log.contains("aaaa"), "{log}");
}

#[test]
fn more_at_once_or_from_one_address_or_more_often_than_the_limits_say_is_turned_away_at_the_door() {
    let meeting = Meeting::of(b"nobody opened this");
    let patient = Duration::from_secs(30);

    // As many at once as it takes, of every kind together.
    let point_ = point(Limits { connections: 6, hello: patient, ..Limits::default() }, &[]);
    let at = point_.address.clone();
    let held: Vec<TcpStream> = (0..6).map(|_| point_.stranger()).collect();
    point_.until("six are taken", |counts| counts.connections == 6);
    let turned = client::join(&at, meeting).unwrap_err();
    assert_eq!(turned, Turned::No(No::Full));
    assert!(!turned.for_good() && turned.sentence().contains("as many connections as it takes at once"));
    // Forty more, a few at a time: each is turned away, and nearly all
    // of them hear why. (No more than that: every connection a test makes
    // keeps one of the machine's ports for two minutes after.)
    let mut heard_why = 0;
    for _ in 0..4 {
        let asking: Vec<_> = (0..10)
            .map(|_| {
                let at = at.clone();
                std::thread::spawn(move || client::join(&at, meeting))
            })
            .collect();
        for one in asking {
            match one.join().unwrap() {
                Err(Turned::No(No::Full)) => heard_why += 1,
                Err(Turned::NoAnswer(_)) => {}
                other => panic!("{other:?}"),
            }
        }
    }
    assert!(heard_why >= 36, "{heard_why} of forty heard why");
    assert_eq!(point_.server.counts().connections, 6, "and none of them took a place");
    let log = point_.log();
    assert_eq!(log.matches("turned away (full)").count(), 1, "one line for all of them:\n{log}");
    assert!(log.contains("as many connections as are taken at once (6)"), "{log}");
    drop(held);
    point_.until("they have gone", |counts| counts.connections == 0);
    assert_eq!(client::join(&at, meeting).unwrap_err(), Turned::No(No::Unknown), "and the next is taken again");
    point_.server.stop();

    // As many at once from one address.
    let point_ = point(Limits { from_one_address: 3, hello: patient, ..Limits::default() }, &[]);
    let held: Vec<TcpStream> = (0..3).map(|_| point_.stranger()).collect();
    point_.until("three are taken", |counts| counts.connections == 3);
    let turned = client::join(&point_.address, meeting).unwrap_err();
    assert_eq!(turned, Turned::No(No::Crowd));
    assert!(turned.sentence().contains("from this address"), "{}", turned.sentence());
    assert!(point_.log().contains("as many connections from one address as are taken at once (3)"));
    drop(held);
    point_.until("they have gone", |counts| counts.connections == 0);
    assert_eq!(client::join(&point_.address, meeting).unwrap_err(), Turned::No(No::Unknown));
    point_.server.stop();

    // As many new ones from one address in a minute, whether or not the
    // earlier ones have gone.
    let point_ = point(Limits { a_minute_from_one_address: 5, ..Limits::default() }, &[]);
    for _ in 0..5 {
        assert_eq!(client::join(&point_.address, meeting).unwrap_err(), Turned::No(No::Unknown));
    }
    point_.until("they have gone", |counts| counts.connections == 0);
    let turned = client::join(&point_.address, meeting).unwrap_err();
    assert_eq!(turned, Turned::No(No::Often));
    assert!(!turned.for_good() && turned.sentence().contains("too often from this address"), "{}", turned.sentence());
    assert!(point_.log().contains("more than 5 new connections from one address in a minute"), "{}", point_.log());
}

#[test]
fn a_knock_that_is_taken_up_does_not_count_against_how_often_its_holder_may_come() {
    let key = Key::new().unwrap();
    let point = point(Limits { a_minute_from_one_address: 8, ..Limits::default() }, &[(&key, "Ada")]);
    let meeting = Meeting::of(b"an invitation's secret");
    // One connection to hold the meeting, and four people who ask for it
    // (on this machine, from the holder's own address): five that count.
    // The four by which the holder takes them up do not.
    let mut holding = Holding::open(&point.address, &key, meeting).unwrap();
    let _pipes: Vec<_> = (0..4).map(|_| point.pipe(&mut holding, meeting)).collect();
    for _ in 0..3 {
        assert_eq!(client::join(&point.address, Meeting::of(b"nobody's")).unwrap_err(), Turned::No(No::Unknown));
    }
    assert_eq!(client::join(&point.address, meeting).unwrap_err(), Turned::No(No::Often), "the ninth that counts");
    // A ticket that is none counts like anything else a stranger says.
    assert_eq!(client::take(&point.address, Ticket::new().unwrap()).unwrap_err(), Turned::No(No::Often));
}

/// Through a key's handshake by hand, as someone who has the key and a
/// program of their own might.
fn with_the_key(stream: &TcpStream, key: &Key) -> (Sayer, Hearer) {
    say(stream, &Hello::Open(key.id()).line());
    answered_ok(stream);
    let within = Duration::from_secs(5);
    let (say, hear) = secure_in(stream.try_clone().unwrap(), &key.proof(), true, within, Room::SMALL).unwrap();
    hear.wait_at_most(Some(Duration::from_secs(10))).unwrap();
    (say, hear)
}

#[test]
fn someone_with_a_key_who_says_what_does_not_belong_there_is_let_go_of_too() {
    let key = Key::new().unwrap();
    let point = point(quick(), &[(&key, "Ada")]);
    let meeting = Meeting::of(b"an invitation's secret");
    let word = |hear: &mut Hearer| hear.hear(MAX_WORD).ok().and_then(|heard| Word::read(&heard));

    // A word that is none, where the meeting was to be named.
    let stream = point.stranger();
    let (mut say_, mut hear) = with_the_key(&stream, &key);
    say_.say(b"dance").unwrap();
    assert_eq!(word(&mut hear), Some(Word::No(No::What)));
    assert_eq!(word(&mut hear), None, "and the connection is closed");

    // Far more than a word: refused by its length, before it has arrived.
    let stream = point.stranger();
    let (mut say_, mut hear) = with_the_key(&stream, &key);
    let said = std::thread::spawn(move || say_.say(&vec![b'a'; 300_000]));
    let began = Instant::now();
    while word(&mut hear).is_some() {}
    assert!(began.elapsed() < Duration::from_secs(12), "after {:?}", began.elapsed());
    let _ = said.join().unwrap();

    // Nothing at all after proving the key: no longer than anyone has.
    let stream = point.stranger();
    let (_say, mut hear) = with_the_key(&stream, &key);
    let began = Instant::now();
    assert_eq!(word(&mut hear), Some(Word::No(No::Late)));
    assert!(began.elapsed() < Duration::from_secs(12), "after {:?}", began.elapsed());
    point.until("none of them holds anything", |counts| counts.connections == 0 && counts.meetings == 0);

    // A meeting that is held, and then a word that does not belong: the
    // connection is closed, and the meeting is its holder's to come back
    // to, like one whose connection was lost.
    let stream = point.stranger();
    let (mut say_, mut hear) = with_the_key(&stream, &key);
    say_.say(Word::Open(meeting).text().as_bytes()).unwrap();
    assert_eq!(word(&mut hear), Some(Word::Ok));
    say_.say(Word::Knock(Ticket::new().unwrap()).text().as_bytes()).unwrap();
    assert_eq!(word(&mut hear), Some(Word::No(No::What)));
    assert_eq!(word(&mut hear), None);
    point.until("its holder is away", |counts| counts.held == 0 && counts.meetings == 1);
    let mut holding = Holding::open(&point.address, &key, meeting).unwrap();
    let (mut ours, mut theirs) = point.pipe(&mut holding, meeting);
    theirs.write_all(b"as ever").unwrap();
    let mut heard = [0u8; 7];
    ours.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"as ever");
    assert!(!point.log().contains("fault"), "{}", point.log());
}

/// How many strangers come while the meetings of the test below go on.
const STRANGERS: u32 = 120;

#[test]
fn many_meetings_at_once_each_pass_what_is_theirs_while_strangers_knock() {
    let keys: Vec<Key> = (0..4).map(|_| Key::new().unwrap()).collect();
    let names = ["Ada", "Ben", "Cy", "Dee"];
    let given: Vec<(&Key, &str)> = keys.iter().zip(names).collect();
    let limits = Limits {
        // (Everyone here is at one address, and none of this is about how
        // many there may be.)
        from_one_address: 500,
        a_minute_from_one_address: 1_000_000,
        ..Limits::default()
    };
    let point = point(limits, &given);
    let at = point.address.clone();

    // Strangers, while the meetings go on: silent ones, ones that say
    // nonsense, ones that ask for what is not there. A hundred and twenty
    // of them and no more: every connection a test makes keeps one of the
    // machine's ports for two minutes after.
    let all_came = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let strangers: Vec<_> = (0..4u32)
        .map(|which| {
            let (at, all_came) = (at.clone(), all_came.clone());
            std::thread::spawn(move || {
                for came in 0..STRANGERS / 4 {
                    std::thread::sleep(Duration::from_millis(5));
                    let Ok(mut stream) = TcpStream::connect(&at) else { continue };
                    all_came.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let _ = match (which + came) % 4 {
                        0 => stream.write_all(b"\xff\xff\xff\xff\xff\xff\xff\xff"),
                        1 => stream.write_all(Hello::Join(Meeting::of(&came.to_be_bytes())).line().as_bytes()),
                        2 => stream.write_all(format!("{}\n", Hello::Take(Ticket::new().unwrap()).line()).as_bytes()),
                        _ => Ok(()),
                    };
                }
            })
        })
        .collect();

    // Eight meetings, two a key, three people in each beside its holder;
    // every pipe passes what is its own, both ways, again and again until
    // the strangers have all come.
    let meetings: Vec<_> = (0..8u8)
        .map(|number| {
            let (at, key, all_came) = (at.clone(), keys[usize::from(number) / 2].clone(), all_came.clone());
            std::thread::spawn(move || {
                let meeting = Meeting::of(&[number; 20]);
                let mut holding = Holding::open(&at, &key, meeting).unwrap();
                let pipes: Vec<(TcpStream, TcpStream)> = (0..3)
                    .map(|_| {
                        let asking = at.clone();
                        let joining = std::thread::spawn(move || client::join(&asking, meeting));
                        let ticket = holding.next(Duration::from_secs(20)).unwrap().expect("someone knocked");
                        let ours = client::take(&at, ticket).unwrap();
                        (ours, joining.join().unwrap().unwrap())
                    })
                    .collect();
                let passing: Vec<_> = (pipes.into_iter().enumerate())
                    .map(|(which, (mut ours, mut theirs))| {
                        let all_came = all_came.clone();
                        std::thread::spawn(move || {
                            let own = number.wrapping_mul(16) + which as u8;
                            let mut heard = vec![0u8; 200_000];
                            let began = Instant::now();
                            let enough = || {
                                all_came.load(std::sync::atomic::Ordering::Relaxed) >= STRANGERS
                                    || began.elapsed() > Duration::from_secs(20)
                            };
                            for round in 0.. {
                                if round >= 2 && enough() {
                                    break;
                                }
                                let mut saying = theirs.try_clone().unwrap();
                                let said = std::thread::spawn(move || saying.write_all(&[own; 200_000]));
                                ours.read_exact(&mut heard).unwrap();
                                said.join().unwrap().unwrap();
                                assert!(heard.iter().all(|byte| *byte == own), "a pipe passed what is another's");
                                std::mem::swap(&mut ours, &mut theirs);
                            }
                            (ours, theirs)
                        })
                    })
                    .collect();
                let pipes: Vec<_> = passing.into_iter().map(|passed| passed.join().unwrap()).collect();
                (holding, pipes)
            })
        })
        .collect();
    let held: Vec<_> = meetings.into_iter().map(|meeting| meeting.join().unwrap()).collect();
    let counts = point.server.counts();
    assert_eq!((counts.meetings, counts.held, counts.pipes), (8, 8, 24));
    assert!(counts.connections >= 8 + 48, "{counts:?}");

    for stranger in strangers {
        stranger.join().unwrap();
    }
    let came = all_came.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(came, STRANGERS, "the strangers who came meanwhile");
    for (holding, pipes) in held {
        holding.close();
        drop(pipes);
    }
    point.until("everything is let go of", |counts| counts.connections == 0 && counts.meetings == 0);
    let log = point.log();
    assert!(!log.contains("fault") && log.matches("holds a meeting").count() == 8, "{log}");
    assert!(log.lines().count() < 400, "the log of it is {} lines, however many strangers came", log.lines().count());
}
