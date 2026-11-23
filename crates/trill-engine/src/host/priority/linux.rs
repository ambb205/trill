//! The class Linux keeps for audio: a thread scheduled in real time
//! (`SCHED_FIFO`), which runs ahead of every ordinary thread for as long
//! as it has something to do.
//!
//! The kernel gives it to a thread directly where the user's limits allow
//! (`RLIMIT_RTPRIO` above 0: a user in the `audio` group of a system set up
//! for audio, or one given `CAP_SYS_NICE`). Elsewhere a desktop has a
//! service that gives it on a program's behalf, within bounds it keeps
//! (RealtimeKit, over the system's D-Bus, which PipeWire asks too). The
//! service's bounds are the user's, shared with every program of theirs (by
//! its defaults 25 threads, 15 processes and 25 requests in 20 seconds), so
//! it is asked only for a real device's stream or where a program says it
//! is one ([`ask_the_desktop`]): never by a test, nor for a stand-in device.
//!
//! The service is asked by a thread of its own, the *broker*, and never by a
//! thread that makes sound: an audio thread only leaves its thread id in a
//! slot, by an atomic, and wakes the broker, which neither allocates nor
//! waits. The device's and the input's threads are asked for before the
//! threads beside them. The broker is started off the audio threads, when a
//! stream is about to begin ([`stream_opens`], [`prepare`]).
//!
//! **A real-time thread that does not pause is the program's end.** The
//! kernel counts how long a real-time thread has run since it last slept,
//! and ends the process with SIGKILL, without a word, where that passes the
//! hard limit of `RLIMIT_RTTIME`; at a soft limit under it, it says SIGXCPU
//! first. So Trill keeps the soft limit at half the hard one, and SIGXCPU
//! takes every thread it raised out of real time, from inside the handler,
//! by plain system calls on their ids ([`RAISED`]): the thread that is stuck
//! could not look at a flag. From then on nothing is raised until a stream
//! opens again, and the sentence says why. A song that takes more than its
//! buffers' time on the device's thread never sleeps there, and one block
//! of a plugin that takes long does the same: such a song is late, and not
//! ended.
//!
//! What a thread has is asked of the kernel each time
//! ([`in_real_time`]): a thread the service raised after it asked has it,
//! and says so.

use parking_lot::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::thread::Thread;
use std::time::Duration;

/// The real-time priority asked for: high among programs' threads (the
/// kernel's own handlers of interrupts run at 50), and lowered to what the
/// user's limit or the service allows.
const PRIORITY: i32 = 70;
/// RealtimeKit's own default for the hard limit on how long a real-time
/// thread may run without a pause, in microseconds. The soft limit, at
/// which the threads are taken out of real time, is half the hard limit
/// where one is set, and half of this where none is: a hard limit is put in
/// place only where the service asks for one ([`Service::connect`]), since
/// it is what ends a program, cannot be raised again, and is inherited.
const RTTIME_HARD_US: u64 = 200_000;

/// How a thread came to be in the class, or not.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Standing {
    /// It was in real time already (its host made it so): left as it is.
    Had,
    /// The kernel gave it to the thread directly; given up when dropped.
    Given,
    /// The broker was asked to have the service give it; given up when
    /// dropped, whether it came or not.
    Asked,
    /// Neither way was open, or the threads were taken out of real time
    /// since the stream opened.
    Refused,
}

/// Which threads are asked for first.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Rank {
    /// The device's or the input's own thread.
    First,
    /// A thread beside it, of the team.
    Beside,
}

/// The device's and the input's threads waiting for the broker, by their
/// ids; 0 is a free slot.
static WANTED_FIRST: [AtomicI32; 8] = [const { AtomicI32::new(0) }; 8];
/// The threads beside them waiting for the broker.
static WANTED_BESIDE: [AtomicI32; 64] = [const { AtomicI32::new(0) }; 64];
/// Every thread raised, or asked for, and not given up since: what SIGXCPU
/// takes out of real time.
static RAISED: [AtomicI32; 80] = [const { AtomicI32::new(0) }; 80];
/// The broker, once one was started: `None` where it could not be.
static BROKER: OnceLock<Option<Thread>> = OnceLock::new();
/// Whether the desktop's service may be asked ([`ask_the_desktop`]).
static DESKTOP: AtomicBool = AtomicBool::new(false);
/// Whether the threads were taken out of real time since a stream opened.
static TAKEN_DOWN: AtomicBool = AtomicBool::new(false);
/// How many threads the handler took out of real time, the last time, and
/// how many ids it acted on at all (each one a thread of this process).
static TAKEN: AtomicU32 = AtomicU32::new(0);
static ACTED_ON: AtomicU32 = AtomicU32::new(0);
/// The soft limit the kernel says SIGXCPU at, in microseconds.
static SOFT_US: AtomicU64 = AtomicU64::new(0);
/// Whether the handler for SIGXCPU is in place.
static GUARDED: AtomicBool = AtomicBool::new(false);
/// Threads given the class since a stream opened (by either way), and
/// threads the service refused.
static GIVEN: AtomicU32 = AtomicU32::new(0);
static REFUSED: AtomicU32 = AtomicU32::new(0);
/// Why the service did not give it, in words, the last time it did not.
static WHY_NOT: Mutex<Option<String>> = Mutex::new(None);

/// Lets the desktop's service be asked, for a program whose threads make
/// sound for a person to hear: the system's host says so when it opens a
/// real device, and a program that measures as one does says so itself.
pub(super) fn ask_the_desktop() {
    DESKTOP.store(true, Ordering::Relaxed);
}

/// A stream opens: what happened to the last one's threads is forgotten,
/// the limits and the handler are in place, and the broker is started
/// where the service may be asked. Not on an audio thread: it may make a
/// thread.
pub(super) fn stream_opens() {
    TAKEN_DOWN.store(false, Ordering::Relaxed);
    GIVEN.store(0, Ordering::Relaxed);
    REFUSED.store(0, Ordering::Relaxed);
    sweep(&RAISED);
    prepare();
}

/// The limits and the handler in place, and the broker started where the
/// service may be asked. Not on an audio thread: it may make a thread.
pub(super) fn prepare() {
    guard();
    if DESKTOP.load(Ordering::Relaxed) {
        BROKER.get_or_init(|| {
            let started = std::thread::Builder::new().name("trill-realtime".into()).spawn(broker);
            match started {
                Ok(handle) => Some(handle.thread().clone()),
                Err(error) => {
                    log::warn!("the thread that asks for the audio class could not be started: {error}");
                    None
                }
            }
        });
    }
}

/// Whether the calling thread is scheduled in real time now.
pub(super) fn in_real_time() -> bool {
    // SAFETY: asks of the calling thread only.
    // (With the flag that its children start ordinary, which RealtimeKit sets.)
    let policy = unsafe { libc::sched_getscheduler(0) } & !libc::SCHED_RESET_ON_FORK;
    policy == libc::SCHED_FIFO || policy == libc::SCHED_RR
}

/// Whether the threads were taken out of real time since a stream opened.
pub(super) fn taken_down() -> bool {
    TAKEN_DOWN.load(Ordering::Relaxed)
}

/// The calling thread's real-time priority now: 1 to 99 in real time, and
/// 0 for an ordinary thread.
pub(super) fn level() -> Option<i32> {
    let mut param = libc::sched_param { sched_priority: 0 };
    // SAFETY: fills in a local, for the calling thread.
    let asked = unsafe { libc::sched_getparam(0, &mut param) };
    (asked == 0).then_some(param.sched_priority)
}

/// The calling thread's id.
fn this_thread() -> i32 {
    // SAFETY: the calling thread's own id.
    unsafe { libc::gettid() }
}

/// Whether `tid` is a thread of this process. The kernel gives ids out
/// again, to any process: one of a thread that is gone may be another
/// program's. A plain system call: fit for a signal's handler.
fn alive(tid: i32) -> bool {
    // SAFETY: signal 0 sends nothing: it only asks whether the thread is
    // there, in this process.
    unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), tid, 0) == 0 }
}

/// Frees the slots of `slots` held by threads that are gone.
fn sweep(slots: &[AtomicI32]) {
    for slot in slots {
        let tid = slot.load(Ordering::Acquire);
        if tid != 0 && !alive(tid) {
            let _ = slot.compare_exchange(tid, 0, Ordering::AcqRel, Ordering::Relaxed);
        }
    }
}

/// Puts `tid` in the first free slot of `slots`, freeing the slots of
/// threads that are gone where none is free.
fn hold(slots: &[AtomicI32], tid: i32) -> bool {
    let take = || slots.iter().any(|slot| slot.compare_exchange(0, tid, Ordering::AcqRel, Ordering::Relaxed).is_ok());
    take() || {
        sweep(slots);
        take()
    }
}

/// Takes `tid` out of `slots`.
fn let_go(slots: &[AtomicI32], tid: i32) {
    for slot in slots {
        let _ = slot.compare_exchange(tid, 0, Ordering::AcqRel, Ordering::Relaxed);
    }
}

/// Raises the calling thread. Neither allocates nor waits: it may be an
/// audio thread in the middle of its first buffer.
pub(super) fn raise(rank: Rank) -> Standing {
    if TAKEN_DOWN.load(Ordering::Relaxed) {
        return Standing::Refused;
    }
    let tid = this_thread();
    if in_real_time() {
        // A thread its host made real-time is ours to bring down too, or
        // the process goes with it.
        hold(&RAISED, tid);
        return Standing::Had;
    }
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: fills in a local.
    let allowed = match unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut limit) } {
        0 => limit.rlim_cur.min(99) as i32,
        _ => 0,
    };
    if allowed > 0 && hold(&RAISED, tid) {
        // The limit and its handler first: system calls, nothing that
        // allocates or waits.
        guard();
        let param = libc::sched_param { sched_priority: PRIORITY.min(allowed) };
        // SAFETY: sets the calling thread's own scheduling. Its children
        // start ordinary (`SCHED_RESET_ON_FORK`).
        if unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) } == 0 {
            GIVEN.fetch_add(1, Ordering::Relaxed);
            return Standing::Given;
        }
        let_go(&RAISED, tid);
    }
    let Some(Some(broker)) = BROKER.get() else { return Standing::Refused };
    let wanted: &[AtomicI32] = if rank == Rank::First { &WANTED_FIRST } else { &WANTED_BESIDE };
    if !hold(&RAISED, tid) {
        return Standing::Refused;
    }
    if !hold(wanted, tid) {
        let_go(&RAISED, tid);
        return Standing::Refused;
    }
    broker.unpark();
    Standing::Asked
}

/// Gives the calling thread an ordinary thread's scheduling back, and
/// takes back what it asked the broker for, if the broker has not got to
/// it yet.
pub(super) fn lower() {
    release();
    ordinary(0);
}

/// Lets go of the calling thread in every registry, leaving its
/// scheduling as it is: for a thread that had the class from its host.
pub(super) fn release() {
    let tid = this_thread();
    let_go(&WANTED_FIRST, tid);
    let_go(&WANTED_BESIDE, tid);
    let_go(&RAISED, tid);
}

/// Makes the thread `tid` (0: the calling one) an ordinary thread. A plain
/// system call: fit for a signal's handler.
fn ordinary(tid: i32) -> bool {
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: sets a thread of this process to an ordinary scheduling,
    // which any thread may. (Keeping the flag that its children start
    // ordinary: only a privileged thread may take it off.)
    unsafe { libc::sched_setscheduler(tid, libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK, &param) == 0 }
}

/// Keeps the soft limit on how long a real-time thread may run without a
/// pause at half the hard one (half of [`RTTIME_HARD_US`] where there is no
/// hard one), lowering the hard one only to `most` where that is asked
/// (the service's), and SIGXCPU's handler in place. With no hard limit the
/// kernel says SIGXCPU at the soft one, once a second after, and ends
/// nothing. System calls only: an audio thread may call it before it raises
/// itself through the kernel.
fn guard_under(most: Option<u64>) {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: fills in a local, and sets the process's own limit (lowering
    // it, which any process may).
    unsafe {
        if libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) == 0 {
            let hard = most.map_or(limit.rlim_max, |most| limit.rlim_max.min(most));
            let half = if hard == libc::RLIM_INFINITY { RTTIME_HARD_US / 2 } else { hard / 2 };
            let soft = limit.rlim_cur.min(half);
            if (soft, hard) != (limit.rlim_cur, limit.rlim_max) {
                libc::setrlimit(libc::RLIMIT_RTTIME, &libc::rlimit { rlim_cur: soft, rlim_max: hard });
            }
            SOFT_US.store(soft, Ordering::Relaxed);
        }
    }
    if !GUARDED.swap(true, Ordering::AcqRel) {
        install_handler();
    }
}

fn guard() {
    guard_under(None);
}

/// Puts [`on_sigxcpu`] in place for SIGXCPU, where nothing else of the
/// program's is there.
fn install_handler() {
    // SAFETY: reads and sets the process's action for one signal, with a
    // handler that only makes system calls and touches atomics.
    unsafe {
        let mut was: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGXCPU, std::ptr::null(), &mut was);
        if was.sa_sigaction != libc::SIG_DFL && was.sa_sigaction != libc::SIG_IGN {
            return;
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sigxcpu as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGXCPU, &action, std::ptr::null_mut());
    }
}

/// SIGXCPU: a real-time thread ran too long without a pause. Every thread
/// Trill raised is made ordinary, by its id, and nothing is raised again
/// until a stream opens. Runs on whichever thread the kernel chose: only
/// atomics and system calls.
extern "C" fn on_sigxcpu(_signal: libc::c_int) {
    TAKEN_DOWN.store(true, Ordering::Relaxed);
    for slot in WANTED_FIRST.iter().chain(&WANTED_BESIDE) {
        slot.store(0, Ordering::Relaxed);
    }
    let (mut taken, mut acted_on) = (0, 0);
    for slot in &RAISED {
        let tid = slot.swap(0, Ordering::AcqRel);
        // Only a thread of this process: the id of one that is gone may be
        // another program's by now.
        if tid != 0 && alive(tid) {
            acted_on += 1;
            if ordinary(tid) {
                taken += 1;
            }
        }
    }
    TAKEN.store(taken, Ordering::Relaxed);
    ACTED_ON.store(acted_on, Ordering::Relaxed);
}

/// How many of the threads that make sound were given the class since a
/// stream opened, and how many the service refused.
pub(super) fn counts() -> (u32, u32) {
    (GIVEN.load(Ordering::Relaxed), REFUSED.load(Ordering::Relaxed))
}

/// What is so of the class, and what the user can do about it, where the
/// threads that make sound have not all got it: `None` where they have,
/// or where nothing has asked.
pub(super) fn refused_because() -> Option<String> {
    if TAKEN_DOWN.load(Ordering::Relaxed) {
        let soft = SOFT_US.load(Ordering::Relaxed) / 1000;
        return Some(format!(
            "The audio threads were taken out of real time: one of them ran for {soft} ms without a pause, which \
             the system ends a program for a moment later. They run at an ordinary priority until the audio \
             device is opened again. A song that takes more than its buffers' time does this, and so does a \
             plugin that takes long over one block."
        ));
    }
    let (given, refused) = counts();
    if refused == 0 {
        return None;
    }
    let why = WHY_NOT.lock().clone().unwrap_or_else(|| "it did not say why".into());
    let what = "Installing RealtimeKit (`sudo apt install rtkit` on Debian and Ubuntu), or putting this user in the \
                `audio` group of a system set up for audio (one with `@audio - rtprio 95` in \
                /etc/security/limits.d/), and starting Trill again lets it.";
    Some(match given {
        0 => format!(
            "The audio threads run at an ordinary priority, so a busy machine can keep them from their work: this \
             user may not schedule a thread in real time (its limit, `ulimit -r`, is 0), and RealtimeKit, which \
             gives it on a program's behalf, did not ({why}). {what}"
        ),
        _ => format!(
            "{given} of the {} threads that make sound run in real time, and {refused} at an ordinary priority, \
             which a busy machine can keep from their work: RealtimeKit, which gives real time on a program's \
             behalf, gave no more ({why}). It shares what it gives among all of this user's programs; closing \
             another that plays sound, or a song spread over fewer threads, leaves more.",
            given + refused
        ),
    })
}

/// The broker: waits to be woken, and asks the service for every thread
/// that is waiting, the device's and the input's first.
fn broker() {
    let mut service: Option<Service> = None;
    loop {
        std::thread::park_timeout(Duration::from_secs(1));
        for slot in WANTED_FIRST.iter().chain(&WANTED_BESIDE) {
            let tid = slot.load(Ordering::Acquire);
            if tid == 0 {
                continue;
            }
            if !TAKEN_DOWN.load(Ordering::Relaxed) {
                if service.is_none() {
                    service = Service::connect().map_err(said_no).ok();
                }
                match service.as_ref().map(|connected| connected.raise(tid)) {
                    // The threads were taken down while the service was asked:
                    // this one too, and it is not counted as given.
                    Some(Ok(())) if TAKEN_DOWN.load(Ordering::Relaxed) => {
                        if alive(tid) {
                            ordinary(tid);
                        }
                        let_go(&RAISED, tid);
                    }
                    Some(Ok(())) => {
                        GIVEN.fetch_add(1, Ordering::Relaxed);
                    }
                    Some(Err(why)) => {
                        said_no(why);
                        // A connection that failed is made again next time.
                        service = None;
                    }
                    // (Counted where the connection failed.)
                    None => {}
                }
            }
            let _ = slot.compare_exchange(tid, 0, Ordering::AcqRel, Ordering::Relaxed);
        }
    }
}

fn said_no(why: String) {
    log::warn!("the audio class was not given: {why}");
    *WHY_NOT.lock() = Some(why);
    REFUSED.fetch_add(1, Ordering::Relaxed);
}

/// RealtimeKit, on the system's bus.
struct Service {
    bus: zbus::blocking::Connection,
    priority: u32,
}

const NAME: &str = "org.freedesktop.RealtimeKit1";
const PATH: &str = "/org/freedesktop/RealtimeKit1";

impl Service {
    fn connect() -> Result<Self, String> {
        let bus = zbus::blocking::Connection::system().map_err(|error| format!("the system's bus: {error}"))?;
        let most = Self::property(&bus, "MaxRealtimePriority").unwrap_or(1);
        let rttime = Self::property(&bus, "RTTimeUSecMax").unwrap_or(RTTIME_HARD_US as i64);
        // The service gives real time only to a program whose hard limit is
        // no higher than its own.
        guard_under(Some(u64::try_from(rttime).unwrap_or(RTTIME_HARD_US)));
        let priority = PRIORITY.min(i32::try_from(most).unwrap_or(1)).max(1) as u32;
        Ok(Self { bus, priority })
    }

    fn property(bus: &zbus::blocking::Connection, name: &str) -> Option<i64> {
        let reply =
            bus.call_method(Some(NAME), PATH, Some("org.freedesktop.DBus.Properties"), "Get", &(NAME, name)).ok()?;
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
        i64::try_from(&value).ok().or_else(|| i32::try_from(&value).ok().map(i64::from))
    }

    fn raise(&self, tid: i32) -> Result<(), String> {
        let tid = u64::try_from(tid).map_err(|error| error.to_string())?;
        self.bus
            .call_method(Some(NAME), PATH, Some(NAME), "MakeThreadRealtime", &(tid, self.priority))
            .map(|_| ())
            .map_err(|error| format!("RealtimeKit: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::time::Instant;

    /// This test program started again for the one test `name`, with
    /// `TRILL_PRIORITY_CHILD` set. Its output, and how it ended.
    fn child(name: &str) -> (String, std::process::ExitStatus, Duration) {
        let began = Instant::now();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("host::priority::linux::tests::{name}"), "--ignored", "--nocapture"])
            .env("TRILL_PRIORITY_CHILD", "1")
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), out.status, began.elapsed())
    }

    fn in_the_child() -> bool {
        std::env::var_os("TRILL_PRIORITY_CHILD").is_some()
    }

    /// Without real time, the handler's mechanics: SIGXCPU sent to the
    /// process itself, which lives; the flag is set; every thread that was
    /// held was gone through; and nothing is raised until a stream opens.
    #[test]
    fn sigxcpu_takes_every_raised_thread_down_and_the_process_lives() {
        let (said, status, _) = child("sigxcpu_on_ordinary_threads");
        assert!(status.success(), "{status:?}: {said}");
        assert!(
            said.contains("CHILD taken 3, none held after true, raised again Refused, after a stream open"),
            "{said}"
        );
    }

    #[test]
    #[ignore = "run by sigxcpu_takes_every_raised_thread_down_and_the_process_lives, in a process of its own"]
    fn sigxcpu_on_ordinary_threads() {
        if !in_the_child() {
            return;
        }
        guard();
        // Three threads held as raised (ordinary ones: a test asks for no
        // real time): the calling one and two that wait.
        let (stop, waiting) = (std::sync::Arc::new(AtomicBool::new(false)), std::sync::Arc::new(AtomicU32::new(0)));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let (stop, waiting) = (stop.clone(), waiting.clone());
                std::thread::spawn(move || {
                    assert!(hold(&RAISED, this_thread()));
                    waiting.fetch_add(1, Ordering::Relaxed);
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                })
            })
            .collect();
        assert!(hold(&RAISED, this_thread()));
        while waiting.load(Ordering::Relaxed) < 2 {
            std::thread::sleep(Duration::from_millis(5));
        }
        // SAFETY: sends a signal to this process.
        unsafe { libc::kill(libc::getpid(), libc::SIGXCPU) };
        let began = Instant::now();
        while !taken_down() && began.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let again = raise(Rank::First);
        stream_opens();
        println!(
            "CHILD taken {}, none held after {}, raised again {again:?}, after a stream {}",
            TAKEN.load(Ordering::Relaxed),
            !RAISED.iter().any(|slot| slot.load(Ordering::Relaxed) != 0),
            if taken_down() { "still down" } else { "open" }
        );
        stop.store(true, Ordering::Relaxed);
        for thread in threads {
            thread.join().unwrap();
        }
    }

    /// Threads that register and end, more of them than there are slots:
    /// a new thread still finds one, and the handler acts on no id of a
    /// thread that is gone, only on the ones that are there.
    #[test]
    fn threads_that_ended_free_their_slots_and_the_handler_touches_none_of_them() {
        let (said, status, _) = child("threads_that_ended");
        assert!(status.success(), "{status:?}: {said}");
        assert!(said.contains("CHILD every one held true, a new one held true, acted on 2, taken 2"), "{said}");
    }

    #[test]
    #[ignore = "run by threads_that_ended_free_their_slots_and_the_handler_touches_none_of_them, in a process of its own"]
    fn threads_that_ended() {
        if !in_the_child() {
            return;
        }
        guard();
        // Each registers as a device's thread does with ManuallyDrop, and ends without letting go.
        let every_one_held =
            (0..RAISED.len() + 20).all(|_| std::thread::spawn(|| hold(&RAISED, this_thread())).join().unwrap());
        // Two that are there when the signal comes.
        let (stop, waiting) = (std::sync::Arc::new(AtomicBool::new(false)), std::sync::Arc::new(AtomicU32::new(0)));
        let there: Vec<_> = (0..2)
            .map(|_| {
                let (stop, waiting) = (stop.clone(), waiting.clone());
                std::thread::spawn(move || {
                    let held = hold(&RAISED, this_thread());
                    waiting.fetch_add(u32::from(held), Ordering::Relaxed);
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                })
            })
            .collect();
        while waiting.load(Ordering::Relaxed) < 2 {
            std::thread::sleep(Duration::from_millis(5));
        }
        let a_new_one_held = waiting.load(Ordering::Relaxed) == 2;
        // SAFETY: sends a signal to this process.
        unsafe { libc::kill(libc::getpid(), libc::SIGXCPU) };
        let began = Instant::now();
        while !taken_down() && began.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        println!(
            "CHILD every one held {every_one_held}, a new one held {a_new_one_held}, acted on {}, taken {}",
            ACTED_ON.load(Ordering::Relaxed),
            TAKEN.load(Ordering::Relaxed)
        );
        stop.store(true, Ordering::Relaxed);
        for thread in there {
            thread.join().unwrap();
        }
    }

    /// The soft limit is put at half the hard one, and half the service's
    /// default where there is no hard one; a hard one is never put in place
    /// where there was none, nor lowered.
    #[test]
    fn the_soft_limit_is_set_and_no_hard_one_is_put_in_place() {
        let (said, status, _) = child("limits");
        assert!(status.success(), "{status:?}: {said}");
        assert!(said.contains("CHILD as it should be"), "{said}");
    }

    #[test]
    #[ignore = "run by the_soft_limit_is_set_and_no_hard_one_is_put_in_place, in a process of its own"]
    fn limits() {
        if !in_the_child() {
            return;
        }
        let set = |soft, hard| {
            // SAFETY: sets this process's own limit, from a test process of its own.
            unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &libc::rlimit { rlim_cur: soft, rlim_max: hard }) == 0 }
        };
        let now = || {
            let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            // SAFETY: fills in a local.
            unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) };
            (limit.rlim_cur, limit.rlim_max)
        };
        let mut wrong = Vec::new();
        // With no hard limit (where this process has none: one cannot be raised).
        if now().1 == libc::RLIM_INFINITY && set(libc::RLIM_INFINITY, libc::RLIM_INFINITY) {
            guard();
            if now() != (RTTIME_HARD_US / 2, libc::RLIM_INFINITY) {
                wrong.push(format!("with none: {:?}", now()));
            }
        }
        // With one, at or under what this process has.
        let hard = now().1.min(300_000);
        assert!(set(hard, hard), "a limit can be lowered");
        guard();
        if now() != (hard / 2, hard) {
            wrong.push(format!("with {hard}: {:?}", now()));
        }
        println!("CHILD {}", if wrong.is_empty() { "as it should be".to_string() } else { wrong.join("; ") });
    }

    /// On this machine's RealtimeKit: a thread that is given real time and
    /// then computes without a pause for twice the hard limit is taken out
    /// of real time at the soft one, and the process lives and says why.
    /// It asks the desktop's service, which is the user's and is shared
    /// with their other programs: not in the default run.
    #[test]
    #[ignore = "asks this machine's RealtimeKit for real time; run with `cargo test -p trill-engine --lib \
                a_thread_that_never_pauses -- --ignored --nocapture`"]
    fn a_thread_that_never_pauses_is_taken_out_of_real_time_and_the_program_lives() {
        let (said, status, took) = child("never_pauses");
        println!("{said}\nthe process ended after {took:?}: {status:?} (signal {:?})", status.signal());
        assert!(status.success(), "{status:?}: {said}");
        assert!(said.contains("in real time true"), "the service gave it: {said}");
        assert!(said.contains("CHILD out of real time after Some("), "{said}");
        assert!(said.contains("The audio threads were taken out of real time"), "{said}");
    }

    #[test]
    #[ignore = "run by a_thread_that_never_pauses_is_taken_out_of_real_time_and_the_program_lives"]
    fn never_pauses() {
        if !in_the_child() {
            return;
        }
        ask_the_desktop();
        stream_opens();
        std::thread::spawn(|| {
            let standing = raise(Rank::First);
            let began = Instant::now();
            while !in_real_time() && began.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            // SAFETY: fills in a local.
            unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) };
            println!(
                "CHILD {standing:?}, in real time {} at {:?}; limits {} and {} us",
                in_real_time(),
                level(),
                limit.rlim_cur,
                limit.rlim_max
            );
            let spin = Instant::now();
            let mut out_at = None;
            while spin.elapsed() < Duration::from_micros(2 * RTTIME_HARD_US) {
                if out_at.is_none() && !in_real_time() {
                    out_at = Some(spin.elapsed());
                }
            }
            println!("CHILD out of real time after {out_at:?}, still raised {}", in_real_time());
        })
        .join()
        .unwrap();
        println!("CHILD {}", refused_because().unwrap_or_default());
    }

    /// On this machine's RealtimeKit, as the window opens a real device:
    /// the thread is given the class and gives it up again.
    #[test]
    #[ignore = "asks this machine's RealtimeKit for real time; run with `cargo test -p trill-engine --lib \
                a_thread_that_asks_is_given -- --ignored --nocapture`"]
    fn a_thread_that_asks_is_given_the_class_where_this_machine_gives_it_and_gives_it_up_again() {
        ask_the_desktop();
        stream_opens();
        std::thread::spawn(|| {
            assert!(!in_real_time(), "a thread starts ordinary");
            let standing = raise(Rank::First);
            let began = Instant::now();
            while standing == Standing::Asked && !in_real_time() && began.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
            println!("raised: {standing:?}, in real time: {}, at {:?}", in_real_time(), level());
            assert!(in_real_time() && level().unwrap() >= 1, "{standing:?}: {:?}", refused_because());
            lower();
            assert!(!in_real_time() && level() == Some(0));
        })
        .join()
        .unwrap();
    }
}
