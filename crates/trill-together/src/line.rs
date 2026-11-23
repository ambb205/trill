//! A line as a home has it, for tests and for whoever measures: slower
//! one way than the other, late, and one that goes dead without a word.
//!
//! Everything else in the repository connects over the loopback, where a
//! megabyte takes a millisecond and nothing is ever late: a queue that
//! holds an edit behind a file, a wait that is shorter than a slow line
//! takes, a connection that nobody notices is dead, none of them shows
//! there. A [`Line`] stands in between. It listens on the loopback,
//! connects on to where it was told, and passes what each side says at the
//! pace it was made with and after its delay.
//!
//! It is a stand-in and says so: the two ends still speak TCP to the line
//! over the loopback, so nothing is ever lost or sent twice, and what
//! waits in the system's own buffers at each end (some 64 kB a socket) is
//! what the loopback has them hold, which is about what a slow line has
//! them hold too. What it shows is what a program does when bytes take
//! time; what a real network does to packets it does not.

use crate::invite::Invitation;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How fast a line is and how late.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pace {
    /// Bytes a second from whoever connects to the line towards where it
    /// leads: their way up.
    pub up: u64,
    /// Bytes a second the other way: their way down.
    pub down: u64,
    /// How long anything takes to arrive, one way.
    pub late: Duration,
}

impl Pace {
    /// A line of so many kilobits a second up and down, as whoever sells
    /// one says it, on which anything arrives `late_ms` after it was sent.
    pub fn of(up_kbit: u64, down_kbit: u64, late_ms: u64) -> Self {
        Self { up: (up_kbit * 125).max(1), down: (down_kbit * 125).max(1), late: Duration::from_millis(late_ms) }
    }

    /// Reads `UP:DOWN:MS`, the two paces in kilobits a second: `5000:50000:20`
    /// is five megabits up, fifty down, and twenty milliseconds each way.
    pub fn read(text: &str) -> Option<Self> {
        let mut parts = text.split(':').map(|part| part.parse::<u64>().ok());
        let (up, down, late) = (parts.next()??, parts.next()??, parts.next()??);
        (parts.next().is_none() && up > 0 && down > 0).then(|| Self::of(up, down, late))
    }

    /// In words, for whoever reads a measurement.
    pub fn said(&self) -> String {
        let megabits = |bytes: u64| bytes as f64 * 8.0 / 1e6;
        format!(
            "{:.1} Mbit/s up, {:.1} Mbit/s down, {} ms each way",
            megabits(self.up),
            megabits(self.down),
            self.late.as_millis()
        )
    }
}

/// How far behind the clock a line may fall and catch up: a thread that
/// slept a little long does not make the line slower than it is.
const CATCH_UP: Duration = Duration::from_millis(30);
/// How often a thread that waits looks whether it still should.
const LOOKING: Duration = Duration::from_millis(5);

struct Shared {
    stop: AtomicBool,
    /// Nothing passes, and nothing is closed.
    dead: AtomicBool,
    /// Bytes passed up and down.
    passed: [AtomicU64; 2],
    /// Every socket there is, to cut them by: of each connection the near
    /// one (whoever connected) and then the far one.
    sockets: Mutex<Vec<Arc<TcpStream>>>,
    /// The far sockets of connections that were lost ([`Line::lose`]):
    /// nothing is said into them and nothing they say is passed on, and
    /// they are open until the line is dropped or the far side ends them.
    lost: Mutex<Vec<Arc<TcpStream>>>,
}

impl Shared {
    fn is_lost(&self, socket: &Arc<TcpStream>) -> bool {
        self.lost.lock().unwrap().iter().any(|lost| Arc::ptr_eq(lost, socket))
    }
}

/// What has passed the line and is on its way.
#[derive(Default)]
struct Flight {
    waiting: Mutex<OnItsWay>,
    more: Condvar,
}

#[derive(Default)]
struct OnItsWay {
    /// Each with when it arrives.
    bytes: VecDeque<(Instant, Vec<u8>)>,
    /// Whether whoever said it has ended: nothing more follows.
    ended: bool,
}

/// A line between whoever connects to [`Line::address`] and the address
/// it was made to. It is there until it is dropped.
pub struct Line {
    address: SocketAddr,
    shared: Arc<Shared>,
}

impl Line {
    /// A line to `to` (an address as text: `127.0.0.1:8745`), listening on
    /// a port of the loopback that the system picks.
    pub fn to(to: &str, pace: Pace) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            passed: [AtomicU64::new(0), AtomicU64::new(0)],
            sockets: Mutex::default(),
            lost: Mutex::default(),
        });
        let (to, held) = (to.to_owned(), shared.clone());
        std::thread::Builder::new().name("trill-line".into()).spawn(move || {
            while !held.stop.load(Ordering::Relaxed) {
                let near = match listener.accept() {
                    Ok((near, _)) => near,
                    Err(_) => {
                        std::thread::sleep(LOOKING);
                        continue;
                    }
                };
                // (A line that is dead answers nobody either: whoever
                // connects waits, as for a machine that is not there.)
                while held.dead.load(Ordering::Relaxed) && !held.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(LOOKING);
                }
                let Ok(far) = TcpStream::connect(&to) else { continue };
                let _ = (near.set_nonblocking(false), near.set_nodelay(true), far.set_nodelay(true));
                let (near, far) = (Arc::new(near), Arc::new(far));
                held.sockets.lock().unwrap().extend([near.clone(), far.clone()]);
                pass(near.clone(), far.clone(), pace.up, pace.late, held.clone(), 0);
                pass(far, near, pace.down, pace.late, held.clone(), 1);
            }
        })?;
        Ok(Self { address, shared })
    }

    /// Where to connect to be on the line.
    pub fn address(&self) -> String {
        self.address.to_string()
    }

    /// From here on nothing passes, either way, and nothing is closed:
    /// what a line does that is dead and has not said so.
    pub fn go_dead(&self) {
        self.shared.dead.store(true, Ordering::Relaxed);
    }

    /// The line passes again, with what waited on it.
    pub fn come_back(&self) {
        self.shared.dead.store(false, Ordering::Relaxed);
    }

    /// Every connection on the line is closed, as by a router that was
    /// switched off and on. The line itself stays: whoever connects again
    /// is on it.
    pub fn cut(&self) {
        for socket in self.shared.sockets.lock().unwrap().drain(..) {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    /// Every connection on the line is lost as a line that goes down loses
    /// it: whoever connected sees theirs end, and where the line leads
    /// nothing is seen at all. No word of the end arrives there, what is
    /// said from there goes nowhere, and the connection stays open on that
    /// side for as long as that side keeps it. The line itself stays:
    /// whoever connects again is on it.
    pub fn lose(&self) {
        let mut sockets = self.shared.sockets.lock().unwrap();
        // (They were put there in pairs: the near one, then the far one.)
        for pair in sockets.chunks(2) {
            if let [near, far] = pair {
                self.shared.lost.lock().unwrap().push(far.clone());
                let _ = near.shutdown(Shutdown::Both);
            }
        }
        sockets.clear();
    }

    /// How many bytes have passed, up and down.
    pub fn passed(&self) -> (u64, u64) {
        (self.shared.passed[0].load(Ordering::Relaxed), self.shared.passed[1].load(Ordering::Relaxed))
    }
}

/// A line to wherever `invitation` leads, and the invitation whoever is
/// to be on the line joins by: the same secret, at the line's address.
pub fn before(invitation: &Invitation, pace: Pace) -> io::Result<(Line, Invitation)> {
    let line = Line::to(&invitation.address(), pace)?;
    let over = line.carries(invitation);
    Ok((line, over))
}

impl Line {
    /// `invitation` as whoever is to be on this line joins by it: the same
    /// secret, at the line's address. (The line has to lead where the
    /// invitation does.)
    pub fn carries(&self, invitation: &Invitation) -> Invitation {
        let mut over = invitation.at("127.0.0.1");
        over.port = self.address.port();
        over
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.cut();
        for socket in self.shared.lost.lock().unwrap().drain(..) {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

/// Passes what `from` says to `to`, `rate` bytes a second and `late`
/// after: one thread reads no faster than the line is, so that whoever
/// says more than it takes waits as on a line, and one hands on what has
/// arrived.
fn pass(from: Arc<TcpStream>, to: Arc<TcpStream>, rate: u64, late: Duration, shared: Arc<Shared>, way: usize) {
    let flight = Arc::new(Flight::default());
    let (landing, held, far) = (flight.clone(), shared.clone(), to.clone());
    let handing = std::thread::Builder::new().name("trill-line-out".into()).spawn(move || {
        loop {
            let (at, bytes) = {
                let mut waiting = landing.waiting.lock().unwrap();
                loop {
                    if let Some(next) = waiting.bytes.pop_front() {
                        break next;
                    }
                    if waiting.ended {
                        // Everything that was said has been handed on: the
                        // other side is told that it is over, unless the
                        // connection was lost, of which nobody is told.
                        if !held.is_lost(&far) {
                            let _ = far.shutdown(Shutdown::Write);
                        }
                        return;
                    }
                    waiting = landing.more.wait(waiting).unwrap();
                }
            };
            loop {
                if held.stop.load(Ordering::Relaxed) {
                    return;
                }
                let now = Instant::now();
                if now >= at && !held.dead.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(at.saturating_duration_since(now).clamp(Duration::from_micros(200), LOOKING));
            }
            // (What was on its way over a connection that is lost goes nowhere.)
            if held.is_lost(&far) {
                continue;
            }
            // (Counted before it is handed on: whoever has heard it finds
            // it counted.)
            held.passed[way].fetch_add(bytes.len() as u64, Ordering::Relaxed);
            if (&*far).write_all(&bytes).is_err() {
                let _ = far.shutdown(Shutdown::Both);
                return;
            }
        }
    });
    if handing.is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("trill-line-in".into()).spawn(move || {
        // About a hundredth of a second of the line at a time: what a
        // small message waits behind at the most, as behind a packet.
        let mut room = vec![0u8; (rate / 100).clamp(512, 16 << 10) as usize];
        // When the line is free for the next bytes.
        let mut free = Instant::now();
        loop {
            while shared.dead.load(Ordering::Relaxed) && !shared.stop.load(Ordering::Relaxed) {
                std::thread::sleep(LOOKING);
            }
            let count = match (&*from).read(&mut room) {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            if shared.stop.load(Ordering::Relaxed) {
                break;
            }
            // What the far side of a lost connection goes on saying is
            // taken, so that it can go on saying it, and goes nowhere.
            if shared.is_lost(&from) {
                continue;
            }
            let now = Instant::now();
            let begun = free.max(now.checked_sub(CATCH_UP).unwrap_or(now));
            free = begun + Duration::from_secs_f64(count as f64 / rate as f64);
            {
                // (What the line may catch up by is its pace's alone: nothing
                // arrives sooner after it was said than the line is late.)
                let mut waiting = flight.waiting.lock().unwrap();
                waiting.bytes.push_back((free.max(now) + late, room[..count].to_vec()));
                flight.more.notify_one();
            }
            let now = Instant::now();
            if free > now {
                std::thread::sleep(free - now);
            }
        }
        // The side that said it has ended: the other is told when what is
        // on its way has arrived.
        flight.waiting.lock().unwrap().ended = true;
        flight.more.notify_one();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program that says back whatever it hears, on the loopback.
    fn echo() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            // (One connection a test, and no more: see the note on ports
            // in the repository's guide.)
            for stream in listener.incoming().take(2).flatten() {
                std::thread::spawn(move || {
                    let mut room = [0u8; 4096];
                    while let Ok(count) = (&stream).read(&mut room) {
                        if count == 0 || (&stream).write_all(&room[..count]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        address
    }

    #[test]
    fn a_pace_is_read_as_it_is_written_and_nothing_else_is() {
        let pace = Pace::read("5000:50000:20").unwrap();
        assert_eq!(pace, Pace { up: 625_000, down: 6_250_000, late: Duration::from_millis(20) });
        assert_eq!(pace.said(), "5.0 Mbit/s up, 50.0 Mbit/s down, 20 ms each way");
        for wrong in ["", "5000", "5000:50000", "0:1:1", "1:0:1", "a:b:c", "1:2:3:4"] {
            assert_eq!(Pace::read(wrong), None, "{wrong:?}");
        }
    }

    #[test]
    fn what_is_said_arrives_late_and_no_faster_than_the_line_is() {
        // 800 kbit/s up is 100 kB a second: 50 kB take half a second, and
        // come back at ten times that, each way 40 ms late.
        let line = Line::to(&echo().to_string(), Pace::of(800, 8000, 40)).unwrap();
        let stream = Arc::new(TcpStream::connect(line.address()).unwrap());
        stream.set_nodelay(true).unwrap();
        let began = Instant::now();
        (&*stream).write_all(&[7]).unwrap();
        let mut one = [0u8; 1];
        (&*stream).read_exact(&mut one).unwrap();
        let there_and_back = began.elapsed();
        assert!(there_and_back >= Duration::from_millis(80), "a byte was back after {there_and_back:?}");
        assert!(there_and_back < Duration::from_millis(400), "a byte was back only after {there_and_back:?}");
        // On a line that had nothing to pass for a while it is as late.
        std::thread::sleep(Duration::from_millis(120));
        let began = Instant::now();
        (&*stream).write_all(&[8]).unwrap();
        (&*stream).read_exact(&mut one).unwrap();
        let after_a_rest = began.elapsed();
        assert!(after_a_rest >= Duration::from_millis(80), "after a rest a byte was back after {after_a_rest:?}");

        let said: Vec<u8> = (0..50_000u32).map(|n| (n % 251) as u8).collect();
        let began = Instant::now();
        let (saying, wrote) = (stream.clone(), said.clone());
        let writer = std::thread::spawn(move || (&*saying).write_all(&wrote).unwrap());
        let mut heard = vec![0u8; said.len()];
        (&*stream).read_exact(&mut heard).unwrap();
        let took = began.elapsed();
        writer.join().unwrap();
        assert_eq!(heard, said, "what arrives is what was said, in its order");
        assert!(took >= Duration::from_millis(450), "50 kB crossed a line of 100 kB a second in {took:?}");
        assert!(took < Duration::from_millis(2500), "50 kB took {took:?} on a line of 100 kB a second");
        assert_eq!(line.passed(), (50_002, 50_002));
    }

    #[test]
    fn a_connection_that_is_lost_ends_for_whoever_connected_and_where_it_led_nobody_sees_it_end() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let line = Line::to(&listener.local_addr().unwrap().to_string(), Pace::of(8000, 8000, 0)).unwrap();
        let near = TcpStream::connect(line.address()).unwrap();
        let (far, _) = listener.accept().unwrap();
        let mut one = [0u8; 1];
        (&near).write_all(&[1]).unwrap();
        (&far).read_exact(&mut one).unwrap();

        line.lose();
        near.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let ended = (&near).read(&mut one);
        assert!(matches!(ended, Ok(0) | Err(_)), "whoever connected sees the connection end: {ended:?}");
        // Where it led, nothing ends and nothing arrives; what is said
        // there is taken and goes nowhere.
        far.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let waited = (&far).read(&mut one);
        assert!(
            waited
                .as_ref()
                .is_err_and(|error| matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)),
            "where the line led, the connection has not ended: {waited:?}"
        );
        for _ in 0..8 {
            (&far).write_all(&[0u8; 4096]).expect("saying into a lost connection is no error there");
        }

        // The line is still one: whoever connects again is on it.
        let again = TcpStream::connect(line.address()).unwrap();
        let (far_again, _) = listener.accept().unwrap();
        (&again).write_all(&[3]).unwrap();
        (&far_again).read_exact(&mut one).unwrap();
        assert_eq!(one, [3]);
    }

    #[test]
    fn a_dead_line_passes_nothing_and_closes_nothing_until_it_is_back_and_a_cut_closes() {
        let line = Line::to(&echo().to_string(), Pace::of(8000, 8000, 0)).unwrap();
        let stream = TcpStream::connect(line.address()).unwrap();
        let mut one = [0u8; 1];
        (&stream).write_all(&[1]).unwrap();
        (&stream).read_exact(&mut one).unwrap();

        line.go_dead();
        std::thread::sleep(Duration::from_millis(30));
        (&stream).write_all(&[2]).expect("saying into a dead line is no error");
        stream.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let waited = (&stream).read(&mut one);
        assert!(
            waited
                .as_ref()
                .is_err_and(|error| matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)),
            "nothing comes back over a dead line, and it is not closed: {waited:?}"
        );
        line.come_back();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        (&stream).read_exact(&mut one).expect("what waited passes when the line is back");
        assert_eq!(one, [2]);

        line.cut();
        let ended = (&stream).read(&mut one);
        assert!(matches!(ended, Ok(0) | Err(_)), "a line that is cut ends its connections: {ended:?}");
    }
}
