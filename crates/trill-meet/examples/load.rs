//! Many meetings held at a meeting point at once, to see what that takes
//! of the machine it runs on.
//!
//! ```sh
//! load ADDRESS KEY MEETINGS PEOPLE [SECONDS]
//! ```
//!
//! Opens `MEETINGS` meetings with the key, each with `PEOPLE` people in it
//! (its holder among them), passes a hundred kilobytes each way through
//! every pipe, says so, and then holds everything open for `SECONDS`
//! (sixty unless said), with a byte through every pipe every ten: time to
//! read what the meeting point's process takes (`/proc/PID/status`). The
//! meeting point is started with limits that let one key and one address
//! do this: `--meetings-of-one-key`, `--from-one-address`,
//! `--a-minute-from-one-address`.
//!
//! It is run on the machine the meeting point runs on, or beside it: a
//! measurement, and no part of any test.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use trill_meet::{Door, Holding, Key, Meeting, client};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (Some(address), Some(key), Some(meetings), Some(people)) =
        (arguments.first(), arguments.get(1), arguments.get(2), arguments.get(3))
    else {
        eprintln!("usage: load ADDRESS KEY MEETINGS PEOPLE [SECONDS]");
        std::process::exit(2);
    };
    let key = Key::parse(key).expect("a key its keeper gave");
    let (meetings, people): (usize, usize) = (meetings.parse().expect("a number"), people.parse().expect("a number"));
    let seconds: u64 = arguments.get(4).map_or(60, |seconds| seconds.parse().expect("a number"));

    let began = Instant::now();
    let mut held: Vec<Holding> = Vec::new();
    let mut pipes: Vec<(TcpStream, TcpStream)> = Vec::new();
    for number in 0..meetings {
        let secret = format!("a meeting to see what one takes, number {number}");
        let (meeting, door) = (Meeting::of(secret.as_bytes()), Door::of(secret.as_bytes()));
        let mut holding = Holding::open(address, &key, meeting, door).expect("the meeting is opened");
        for _ in 1..people {
            let asking = address.clone();
            let joining = std::thread::spawn(move || client::join(&asking, meeting, door));
            let ticket = holding.next(Duration::from_secs(20)).expect("the meeting is held").expect("someone knocked");
            let ours = client::take(address, ticket).expect("the knock is taken up");
            pipes.push((ours, joining.join().unwrap().expect("whoever asked is let in")));
        }
        held.push(holding);
    }
    println!(
        "{meetings} meetings of {people} people: {} connections at the meeting point, made in {:.1} s",
        meetings + 2 * pipes.len(),
        began.elapsed().as_secs_f64()
    );

    // Something through every pipe, both ways.
    let began = Instant::now();
    let mut heard = vec![0u8; 100_000];
    for (ours, theirs) in &pipes {
        for (from, mut to) in [(theirs, ours), (ours, theirs)] {
            let mut saying = from.try_clone().expect("another handle");
            let said = std::thread::spawn(move || saying.write_all(&[7u8; 100_000]));
            to.read_exact(&mut heard).expect("what was said arrives");
            said.join().unwrap().expect("it is said");
        }
    }
    println!(
        "{:.1} MB passed, a hundred kilobytes each way through each of {} pipes, in {:.1} s",
        pipes.len() as f64 * 0.2,
        pipes.len(),
        began.elapsed().as_secs_f64()
    );
    println!("holding for {seconds} s");

    // Held open: every holder says that it is there, and every pipe a byte
    // now and then.
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut said = Instant::now();
    while Instant::now() < end {
        for holding in &mut held {
            holding.next(Duration::from_millis(5)).expect("the meeting is held");
        }
        if said.elapsed() >= Duration::from_secs(10) {
            for (mut ours, mut theirs) in pipes.iter().map(|(ours, theirs)| (ours, theirs)) {
                theirs.write_all(&[1]).expect("the pipe is there");
                ours.read_exact(&mut heard[..1]).expect("the pipe is there");
            }
            said = Instant::now();
        }
    }
    for holding in held {
        holding.close();
    }
    println!("done");
}
