//! Whatever reaches a meeting point is a stranger's: the slow, the
//! silent, the ones that say a length or a line that is none, the ones
//! that have heard what a meeting goes by and have not got its
//! invitation, many at once, and someone with a key who says what does
//! not belong there. Each is let go of in its time, costs a line in the
//! log, and leaves the meeting point as it was for whoever comes after.
//!
//! (The number beside a test is how many connections it makes.)

mod common;

use common::{ask, heard_line, heard_until_closed, hold, invited, point, quick, say};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use trill_meet::said::{self, Hello, MAX_WORD, Word};
use trill_meet::secure::{Hearer, Room, Sayer, secure_in};
use trill_meet::{Key, Limits, Meeting, No, Ticket, Turned, client};

/// Reads the `ok` a first line was answered with.
fn answered_ok(mut stream: &TcpStream) {
    let mut answer = [0u8; 3];
    stream.read_exact(&mut answer).unwrap();
    assert_eq!(&answer, b"ok\n");
}

/// 14.
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
    for byte in b"trill-meet 2 join 0000".iter().cycle() {
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
        (format!("trill-meet 1 join {nobodys}"), "no version"),
        // A knock nobody was told of.
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
    // the handshake: a key that is given and one that is not are
    // answered the same.
    let mut no_handshake = vec![0u8, 48];
    no_handshake.extend((0..48u8).map(|n| n.wrapping_mul(77)));
    let unknown = Key::new().unwrap().id();
    for (after, named, answer) in [
        (Vec::new(), key.id(), ""),
        (vec![0xff, 0xff], unknown, ""),
        (no_handshake.clone(), key.id(), "\0\0"),
        (no_handshake, unknown, "\0\0"),
    ] {
        let stranger = point.stranger();
        say(&stranger, &Hello::Open(named).line());
        answered_ok(&stranger);
        (&stranger).write_all(&after).unwrap();
        let (heard, took) = heard_until_closed(&stranger);
        assert_eq!(heard, answer);
        assert!(took < Duration::from_secs(12), "after {took:?}");
    }

    // None of them left anything behind, and a meeting is held and asked
    // for as if none of it had been.
    let counts = point.until("nothing is left of them", |counts| counts.connections == 0);
    assert_eq!((counts.meetings, counts.knocks, counts.pipes), (0, 0, 0));
    let invitation = invited(b"an invitation's secret");
    let mut holding = hold(&point.address, &key, invitation).unwrap();
    let (mut ours, mut theirs) = point.pipe(&mut holding, invitation);
    theirs.write_all(b"as ever").unwrap();
    let mut heard = [0u8; 7];
    ours.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"as ever");

    let log = point.log();
    for kind in ["late", "what", "version", "ticket", "key"] {
        let line = format!("turned away ({kind}), from 127.0.0.1: ");
        assert!(log.contains(&line), "\"{line}\" is not in:\n{log}");
    }
    assert!(log.contains("it did not say what it came for within 2 s"), "{log}");
    assert!(!log.contains("fault") && !log.contains("aaaa"), "{log}");
}

/// 8.
#[test]
fn whoever_has_not_got_the_invitation_cannot_knock_by_the_name_or_by_what_someone_else_answered() {
    let key = Key::new().unwrap();
    let point = point(quick(), &[(&key, "Ada")]);
    let at = point.address.as_str();
    let invitation = invited(b"an invitation's secret");
    let (meeting, door) = invitation;
    let mut holding = hold(at, &key, invitation).unwrap();
    let join = Hello::Join(meeting).line();

    // Someone with the invitation asks, and someone on the way hears all
    // of it: what the meeting goes by, the question, and the answer.
    let honest = point.stranger();
    say(&honest, &join);
    let asked = heard_line(&honest);
    let Some(Ok(question)) = said::asked(&asked) else { panic!("it is asked a question: {asked}") };
    let answer = said::answering(&door.answer(&question));
    say(&honest, &answer);
    let ticket = holding.next(Duration::from_secs(20)).unwrap().expect("its holder is told of a knock");
    let _ours = client::take(at, ticket).unwrap();
    assert_eq!(heard_line(&honest), "ok");

    // With the name alone the listener is asked too, another question
    // every time. Saying again what was answered before is no answer to it,
    let again = point.stranger();
    say(&again, &join);
    let asked_again = heard_line(&again);
    assert!(said::asked(&asked_again).is_some_and(|asked| asked.is_ok()) && asked_again != asked, "{asked_again}");
    say(&again, &answer);
    assert_eq!(heard_until_closed(&again).0, "no unknown\n", "it is told that there is no such meeting");
    // nor is an answer that is made up,
    let made_up = point.stranger();
    say(&made_up, &join);
    heard_line(&made_up);
    say(&made_up, &format!("answer {}", "0".repeat(64)));
    assert_eq!(heard_until_closed(&made_up).0, "no unknown\n");
    // nor what is no answer,
    let nonsense = point.stranger();
    say(&nonsense, &join);
    heard_line(&nonsense);
    say(&nonsense, "let me in");
    assert_eq!(heard_until_closed(&nonsense).0, "no what\n");
    // and one that says nothing to it has no longer than anyone has.
    let silent = point.stranger();
    say(&silent, &join);
    heard_line(&silent);
    let (heard, took) = heard_until_closed(&silent);
    assert_eq!(heard, "no late\n");
    assert!(took < Duration::from_secs(12), "after {took:?}");
    // Whoever asks for a meeting that nobody opened is asked the same and
    // told the same: whether there is such a meeting is not to be had
    // without its invitation.
    assert_eq!(ask(at, invited(b"nobody opened this")).unwrap_err(), Turned::No(No::Unknown));

    // The holder was told of the one knock and of none of the others.
    assert_eq!(holding.next(Duration::from_millis(300)), Ok(None));
    let counts = point.until("nothing is left of them", |counts| counts.connections == 3);
    assert_eq!((counts.knocks, counts.pipes), (0, 1));
    let log = point.log();
    assert_eq!(log.matches("turned away (answer), from 127.0.0.1: it asked for a meeting that there is").count(), 1);
    assert!(log.contains("turned away (unknown), from 127.0.0.1: no meeting goes by what it asked for"), "{log}");
    assert!(log.contains("turned away (what)") && log.contains("turned away (late)"), "{log}");
    for kept_out in [door.to_string(), answer["answer ".len()..].to_owned(), asked["ask ".len()..].to_owned()] {
        assert!(!log.contains(&kept_out), "\"{kept_out}\" is in the log:\n{log}");
    }
}

/// 12.
#[test]
fn more_at_once_or_from_one_address_or_more_often_than_the_limits_say_is_turned_away_at_the_door() {
    let nobodys = invited(b"nobody opened this");
    let patient = Duration::from_secs(60);

    // As many at once as it takes, of every kind together.
    let point_ = point(Limits { connections: 2, hello: patient, ..Limits::default() }, &[]);
    let at = point_.address.clone();
    let held: Vec<TcpStream> = (0..2).map(|_| point_.stranger()).collect();
    point_.until("two are taken", |counts| counts.connections == 2);
    let turned = ask(&at, nobodys).unwrap_err();
    assert_eq!(turned, Turned::No(No::Full));
    assert!(!turned.for_good() && turned.sentence().contains("as many connections as it takes at once"));
    // Two more at the same moment: each is turned away and hears why.
    let asking: Vec<_> = (0..2)
        .map(|_| {
            let at = at.clone();
            std::thread::spawn(move || ask(&at, nobodys))
        })
        .collect();
    for one in asking {
        assert_eq!(one.join().unwrap().unwrap_err(), Turned::No(No::Full));
    }
    assert_eq!(point_.server.counts().connections, 2, "and none of them took a place");
    let log = point_.log();
    assert_eq!(log.matches("turned away (full)").count(), 1, "one line for all of them:\n{log}");
    assert!(log.contains("as many connections as are taken at once (2)"), "{log}");
    drop(held);
    point_.until("they have gone", |counts| counts.connections == 0);
    assert_eq!(ask(&at, nobodys).unwrap_err(), Turned::No(No::Unknown), "and the next is taken again");
    point_.server.stop();

    // As many at once from one address.
    let point_ = point(Limits { from_one_address: 1, hello: patient, ..Limits::default() }, &[]);
    let held = point_.stranger();
    point_.until("one is taken", |counts| counts.connections == 1);
    let turned = ask(&point_.address, nobodys).unwrap_err();
    assert_eq!(turned, Turned::No(No::Crowd));
    assert!(turned.sentence().contains("from this address"), "{}", turned.sentence());
    assert!(point_.log().contains("as many connections from one address as are taken at once (1)"));
    drop(held);
    point_.until("it has gone", |counts| counts.connections == 0);
    assert_eq!(ask(&point_.address, nobodys).unwrap_err(), Turned::No(No::Unknown));
    point_.server.stop();

    // As many new ones from one address in a minute, whether or not the
    // earlier ones have gone.
    let point_ = point(Limits { a_minute_from_one_address: 2, ..Limits::default() }, &[]);
    for _ in 0..2 {
        assert_eq!(ask(&point_.address, nobodys).unwrap_err(), Turned::No(No::Unknown));
    }
    point_.until("they have gone", |counts| counts.connections == 0);
    let turned = ask(&point_.address, nobodys).unwrap_err();
    assert_eq!(turned, Turned::No(No::Often));
    assert!(!turned.for_good() && turned.sentence().contains("too often from this address"), "{}", turned.sentence());
    assert!(point_.log().contains("more than 2 new connections from one address in a minute"), "{}", point_.log());
}

/// 6.
#[test]
fn a_knock_that_is_taken_up_does_not_count_against_how_often_its_holder_may_come() {
    let key = Key::new().unwrap();
    let point = point(Limits { a_minute_from_one_address: 4, ..Limits::default() }, &[(&key, "Ada")]);
    let invitation = invited(b"an invitation's secret");
    // One connection to hold the meeting and one that asks for it (on
    // this machine, from the holder's own address): two that count. The
    // one by which the holder takes the knock up does not.
    let mut holding = hold(&point.address, &key, invitation).unwrap();
    let _pipe = point.pipe(&mut holding, invitation);
    for _ in 0..2 {
        assert_eq!(ask(&point.address, invited(b"nobody's")).unwrap_err(), Turned::No(No::Unknown));
    }
    assert_eq!(ask(&point.address, invitation).unwrap_err(), Turned::No(No::Often), "the fifth that counts");
}

/// Through a key's handshake by hand, as someone who has the key and a
/// program of their own might.
fn with_the_key(stream: &TcpStream, key: &Key) -> (Sayer, Hearer) {
    say(stream, &Hello::Open(key.id()).line());
    answered_ok(stream);
    let within = Duration::from_secs(5);
    let (say, hear) = secure_in(stream.try_clone().unwrap(), &key.proof(), true, within, Room::SMALL).unwrap();
    hear.wait_at_most(Some(Duration::from_secs(20))).unwrap();
    (say, hear)
}

/// 5.
#[test]
fn someone_with_a_key_who_says_what_does_not_belong_there_is_let_go_of_too() {
    let key = Key::new().unwrap();
    let point = point(quick(), &[(&key, "Ada")]);
    let invitation = invited(b"an invitation's secret");
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
    say_.say(Word::Open(invitation.0, invitation.1).text().as_bytes()).unwrap();
    assert_eq!(word(&mut hear), Some(Word::Ok));
    say_.say(Word::Knock(Ticket::new().unwrap()).text().as_bytes()).unwrap();
    assert_eq!(word(&mut hear), Some(Word::No(No::What)));
    assert_eq!(word(&mut hear), None);
    point.until("its holder is away", |counts| counts.held == 0 && counts.meetings == 1);
    let _holding = hold(&point.address, &key, invitation).unwrap();
    point.until("it is held again", |counts| counts.held == 1 && counts.meetings == 1);
    assert!(!point.log().contains("fault"), "{}", point.log());
}

/// How many strangers come while the meetings of the test below go on.
const STRANGERS: u32 = 8;

/// 18.
#[test]
fn several_meetings_at_once_each_pass_what_is_theirs_while_strangers_knock() {
    let keys: Vec<Key> = (0..2).map(|_| Key::new().unwrap()).collect();
    let given: Vec<(&Key, &str)> = keys.iter().zip(["Ada", "Ben"]).collect();
    // (Everyone here is at one address, and none of this is about how
    // many there may be.)
    let limits = Limits { from_one_address: 500, a_minute_from_one_address: 1_000_000, ..Limits::default() };
    let point = point(limits, &given);
    let at = point.address.clone();

    // Strangers, while the meetings go on: silent ones, ones that say
    // nonsense, ones that ask for what is not there. A few and no more:
    // every connection a test makes keeps one of the machine's ports for
    // two minutes after.
    let all_came = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let strangers: Vec<_> = (0..2u32)
        .map(|which| {
            let (at, all_came) = (at.clone(), all_came.clone());
            std::thread::spawn(move || {
                for came in 0..STRANGERS / 2 {
                    std::thread::sleep(Duration::from_millis(40));
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

    // Two meetings, each a key's, two people in each beside its holder;
    // every pipe passes what is its own, both ways, again and again until
    // the strangers have all come.
    let meetings: Vec<_> = (0..2u8)
        .map(|number| {
            let (at, key, all_came) = (at.clone(), keys[usize::from(number)].clone(), all_came.clone());
            std::thread::spawn(move || {
                let invitation = invited(&[number; 20]);
                let mut holding = hold(&at, &key, invitation).unwrap();
                let pipes: Vec<(TcpStream, TcpStream)> = (0..2)
                    .map(|_| {
                        let asking = at.clone();
                        let joining = std::thread::spawn(move || ask(&asking, invitation));
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
    assert_eq!((counts.meetings, counts.held, counts.pipes), (2, 2, 4));
    assert!(counts.connections >= 2 + 8, "{counts:?}");

    for stranger in strangers {
        stranger.join().unwrap();
    }
    let came = all_came.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(came, STRANGERS, "the strangers who came meanwhile");
    for (holding, pipes) in held {
        holding.close();
        // (One end and then the other, so that neither end waits out its
        // two minutes twice.)
        let (ours, theirs): (Vec<_>, Vec<_>) = pipes.into_iter().unzip();
        drop(theirs);
        point.until("its people are out", |counts| counts.pipes % 2 == 0);
        drop(ours);
    }
    point.until("everything is let go of", |counts| counts.connections == 0 && counts.meetings == 0);
    let log = point.log();
    assert!(!log.contains("fault") && log.matches("holds a meeting").count() == 2, "{log}");
    assert!(log.lines().count() < 60, "the log of it is {} lines", log.lines().count());
}
