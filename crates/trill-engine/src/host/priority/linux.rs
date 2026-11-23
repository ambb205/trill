//! The class Linux keeps for audio: a thread scheduled in real time
//! (`SCHED_FIFO`), which runs ahead of every ordinary thread for as long
//! as it has something to do.
//!
//! The kernel gives it to a thread directly where the user's limits allow
//! (`RLIMIT_RTPRIO` above 0: a user in the `audio` group of a system set up
//! for audio, or one given `CAP_SYS_NICE`). Elsewhere a desktop has a
//! service that gives it on a program's behalf, within bounds it keeps
//! (RealtimeKit, over the system's D-Bus, which PipeWire and PulseAudio ask
//! too). The service is asked by a thread of its own, the *broker*, and
//! never by a thread that makes sound: an audio thread only leaves its
//! thread id in a slot, by an atomic, and wakes the broker, which neither
//! allocates nor waits. The broker is started off the audio threads, when
//! a stream is about to begin ([`prepare`]).
//!
//! What a thread has is asked of the kernel each time
//! ([`in_real_time`]): a thread the service raised after it asked has it,
//! and says so.

use parking_lot::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::thread::Thread;
use std::time::Duration;

/// The real-time priority asked for: high among programs' threads (the
/// kernel's own handlers of interrupts run at 50), and lowered to what the
/// user's limit or the service allows.
const PRIORITY: i32 = 70;

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
    /// Neither way was open.
    Refused,
}

/// Threads waiting for the broker, by their ids; 0 is a free slot.
static WANTED: [AtomicI32; 64] = [const { AtomicI32::new(0) }; 64];
/// The broker, once it has been started.
static BROKER: OnceLock<Thread> = OnceLock::new();
/// What the service said last: 0 nothing yet, 1 it gave, 2 it refused.
static SERVICE: AtomicU8 = AtomicU8::new(0);
/// Why the service did not give it, in words, the last time it did not.
static WHY_NOT: Mutex<Option<String>> = Mutex::new(None);

/// Starts the broker if it is not running. Not on an audio thread: it
/// makes a thread.
pub(super) fn prepare() {
    BROKER.get_or_init(|| {
        let started = std::thread::Builder::new().name("trill-realtime".into()).spawn(broker);
        match started {
            Ok(handle) => handle.thread().clone(),
            // A broker that cannot be started leaves every thread to ask
            // the kernel itself; it is never asked to wake.
            Err(_) => std::thread::current(),
        }
    });
}

/// Whether the calling thread is scheduled in real time now.
pub(super) fn in_real_time() -> bool {
    // SAFETY: asks of the calling thread only.
    // (With the flag that its children start ordinary, which RealtimeKit sets.)
    let policy = unsafe { libc::sched_getscheduler(0) } & !libc::SCHED_RESET_ON_FORK;
    policy == libc::SCHED_FIFO || policy == libc::SCHED_RR
}

/// The calling thread's real-time priority now: 1 to 99 in real time, and
/// 0 for an ordinary thread.
pub(super) fn level() -> Option<i32> {
    let mut param = libc::sched_param { sched_priority: 0 };
    // SAFETY: fills in a local, for the calling thread.
    let asked = unsafe { libc::sched_getparam(0, &mut param) };
    (asked == 0).then_some(param.sched_priority)
}

/// Raises the calling thread. Neither allocates nor waits: it may be an
/// audio thread in the middle of its first buffer.
pub(super) fn raise() -> Standing {
    if in_real_time() {
        return Standing::Had;
    }
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: fills in a local.
    let allowed = match unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut limit) } {
        0 => limit.rlim_cur.min(99) as i32,
        _ => 0,
    };
    if allowed > 0 {
        let param = libc::sched_param { sched_priority: PRIORITY.min(allowed) };
        // SAFETY: sets the calling thread's own scheduling. Its children
        // start ordinary (`SCHED_RESET_ON_FORK`).
        if unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) } == 0 {
            return Standing::Given;
        }
    }
    let Some(broker) = BROKER.get() else { return Standing::Refused };
    // SAFETY: the calling thread's own id.
    let tid = unsafe { libc::gettid() };
    for slot in &WANTED {
        if slot.compare_exchange(0, tid, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            broker.unpark();
            return Standing::Asked;
        }
    }
    Standing::Refused
}

/// Gives the calling thread an ordinary thread's scheduling back, and
/// takes back what it asked the broker for, if the broker has not got to
/// it yet.
pub(super) fn lower() {
    // SAFETY: the calling thread's own id.
    let tid = unsafe { libc::gettid() };
    for slot in &WANTED {
        let _ = slot.compare_exchange(tid, 0, Ordering::AcqRel, Ordering::Relaxed);
    }
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: sets the calling thread's own scheduling to an ordinary one,
    // which any thread may. (Keeping the flag that its children start
    // ordinary: only a privileged thread may take it off.)
    unsafe { libc::sched_setscheduler(0, libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK, &param) };
}

/// Why the audio threads are not in the class, and what the user can do
/// about it: `None` where they are, or where nothing has asked yet.
pub(super) fn refused_because() -> Option<String> {
    if SERVICE.load(Ordering::Relaxed) != 2 {
        return None;
    }
    let why = WHY_NOT.lock().clone().unwrap_or_else(|| "it did not say why".into());
    Some(format!(
        "The audio threads run at an ordinary priority, so a busy machine can keep them from their work: this user \
         may not schedule a thread in real time (its limit, `ulimit -r`, is 0), and RealtimeKit, which gives it on a \
         program's behalf, did not ({why}). Installing RealtimeKit (`sudo apt install rtkit` on Debian and Ubuntu), \
         or putting this user in the `audio` group of a system set up for audio (one with `@audio - rtprio 95` in \
         /etc/security/limits.d/), and starting Trill again lets it."
    ))
}

/// The broker: waits to be woken, and asks the service for every thread
/// that is waiting.
fn broker() {
    let mut service: Option<Service> = None;
    loop {
        std::thread::park_timeout(Duration::from_secs(1));
        for slot in &WANTED {
            let tid = slot.load(Ordering::Acquire);
            if tid == 0 {
                continue;
            }
            if service.is_none() {
                service = Service::connect().map_err(said_no).ok();
            }
            if let Some(connected) = &service {
                match connected.raise(tid) {
                    Ok(()) => SERVICE.store(1, Ordering::Relaxed),
                    Err(why) => {
                        said_no(why);
                        // A connection that failed is made again next time.
                        service = None;
                    }
                }
            }
            let _ = slot.compare_exchange(tid, 0, Ordering::AcqRel, Ordering::Relaxed);
        }
    }
}

fn said_no(why: String) {
    log::warn!("the audio class was not given: {why}");
    *WHY_NOT.lock() = Some(why);
    SERVICE.store(2, Ordering::Relaxed);
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
        let rttime = Self::property(&bus, "RTTimeUSecMax").unwrap_or(200_000);
        // The service gives real time only to a program that has a limit
        // on how long a real-time thread may run without a pause, and one
        // no longer than its own: what an audio thread does is far less.
        keep_rttime_under(u64::try_from(rttime).unwrap_or(200_000));
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

/// Sets the program's limit on how long a real-time thread may run without
/// a pause to `most` microseconds, where it has none or a longer one.
fn keep_rttime_under(most: u64) {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: fills in a local, and sets the process's own limit.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) == 0 && limit.rlim_max > most {
            let lower = libc::rlimit { rlim_cur: limit.rlim_cur.min(most), rlim_max: most };
            libc::setrlimit(libc::RLIMIT_RTTIME, &lower);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_that_asks_is_given_the_class_where_this_machine_gives_it_and_gives_it_up_again() {
        prepare();
        std::thread::spawn(|| {
            assert!(!in_real_time(), "a thread starts ordinary");
            let standing = raise();
            // The service answers in its own time.
            let began = std::time::Instant::now();
            while standing == Standing::Asked && !in_real_time() && began.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
            eprintln!("raised: {standing:?}, in real time: {}, at {:?}", in_real_time(), level());
            match standing {
                Standing::Given | Standing::Asked if in_real_time() => assert!(level().unwrap() >= 1),
                _ => assert!(refused_because().is_some() || standing == Standing::Refused, "{standing:?}"),
            }
            lower();
            assert!(!in_real_time() && level() == Some(0));
        })
        .join()
        .unwrap();
    }
}
