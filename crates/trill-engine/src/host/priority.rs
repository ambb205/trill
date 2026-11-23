//! The priority the system keeps for threads that make sound.
//!
//! A thread that has a speaker to keep up with must not be taken off its
//! processor in the middle of a block because something else wants a turn.
//! On Windows that is what the multimedia class scheduler is for: a thread
//! that joins its "Pro Audio" class runs above everything ordinary for as
//! long as it does not use a whole processor all the time. Every thread
//! that makes a playing song's sound joins it: the callback of the
//! system's shared devices (whose library leaves its thread at the
//! priority of any other), the loop of a device taken exclusively, and
//! the threads a block's tracks are handed to ([`crate::team`]).
//!
//! On macOS the class is a thread's time-constraint policy: a thread that
//! has one is run by its deadline, ahead of every ordinary thread. The
//! thread the system calls a device's callback on has one already, which
//! is left as the system gave it; every other thread that makes sound is
//! given one made from the stream's period ([`expect_period`]).
//!
//! On Linux the class is real-time scheduling, asked of the kernel where
//! the user may, and of the desktop's service for it (RealtimeKit) where
//! not, by a thread of its own that a stream starts ([`expect_period`]);
//! see `priority/linux.rs`. Where neither gives it, [`refused_because`]
//! says what the user can do.
//!
//! Elsewhere nothing is asked of the system here, and the threads run as
//! their host has them.

/// Says how long a buffer of the stream lasts, for a thread that is raised
/// after this: on macOS its time-constraint policy is made from it; on
/// Linux the thread that asks for the class on the audio threads' behalf
/// is started. Nothing elsewhere. May be called from any thread but an
/// audio one.
pub fn expect_period(frames: u32, sample_rate: f64) {
    #[cfg(target_os = "macos")]
    mac::expect_period(frames, sample_rate);
    #[cfg(target_os = "linux")]
    linux::prepare();
    #[cfg(not(target_os = "macos"))]
    let _ = (frames, sample_rate);
}

/// Why the threads that make sound are not in the class the system keeps
/// for audio, and what the user can do about it, in a sentence. `None`
/// where they are, where nothing has asked, or where the system says
/// nothing of it.
pub fn refused_because() -> Option<String> {
    #[cfg(target_os = "linux")]
    return linux::refused_because();
    #[cfg(not(target_os = "linux"))]
    None
}

/// The calling thread's place in the class the system keeps for audio. It
/// is given up when this is dropped, on the thread that took it.
pub struct AudioPriority {
    #[cfg(windows)]
    task: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(target_os = "macos")]
    standing: mac::Standing,
    #[cfg(target_os = "linux")]
    standing: linux::Standing,
}

impl AudioPriority {
    /// Raises the calling thread to the priority the system keeps for
    /// audio. Where the system does not grant its class, the thread is set
    /// as high as an ordinary one can be. Says nothing either way: it may
    /// be called from a thread that must not write a log.
    #[cfg(windows)]
    pub fn raise() -> Self {
        use windows::Win32::System::Threading::{
            AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
        };
        use windows::core::w;
        let mut index = 0u32;
        // SAFETY: both calls act on the calling thread only; `index` is a
        // live local the system writes a number to.
        let task = unsafe {
            match AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) {
                Ok(handle) => Some(handle),
                Err(_) => {
                    let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
                    None
                }
            }
        };
        Self { task }
    }

    #[cfg(target_os = "macos")]
    pub fn raise() -> Self {
        Self { standing: mac::raise() }
    }

    /// Neither allocates nor waits: where the service is asked, a thread of
    /// its own asks it.
    #[cfg(target_os = "linux")]
    pub fn raise() -> Self {
        Self { standing: linux::raise() }
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    pub fn raise() -> Self {
        Self {}
    }

    /// Whether the thread is in the system's audio class: granted when it
    /// was asked, or (on macOS and Linux) had by the thread already. On
    /// Linux it is asked of the kernel each time, for a thread the service
    /// gives the class to after it asked. `false` where the system has
    /// none to grant. Called on the thread that holds this.
    pub fn granted(&self) -> bool {
        #[cfg(windows)]
        return self.task.is_some();
        #[cfg(target_os = "macos")]
        return self.standing != mac::Standing::Refused;
        #[cfg(target_os = "linux")]
        return self.standing != linux::Standing::Refused && linux::in_real_time();
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        false
    }

    /// Raises the calling thread once and for as long as it lives: for a
    /// thread that is only ever met inside a callback, and for one that
    /// wants to be asked later what it was given. The first call does it;
    /// every later one is a look at a thread-local. Returns whether the
    /// system granted its audio class.
    pub fn raise_for_good() -> bool {
        let granted = RAISED.try_with(|raised| {
            let Ok(mut raised) = raised.try_borrow_mut() else { return false };
            let raise = || {
                let before = AudioPriority::level_of_this_thread();
                (AudioPriority::raise(), before)
            };
            raised.get_or_insert_with(raise).0.granted()
        });
        granted.unwrap_or(false)
    }

    /// What [`AudioPriority::raise_for_good`] came to on the calling
    /// thread: whether the system granted its audio class. `None` on a
    /// thread that never asked.
    pub fn of_this_thread() -> Option<bool> {
        let granted = RAISED.try_with(|raised| raised.try_borrow().ok()?.as_ref().map(|raised| raised.0.granted()));
        granted.ok().flatten()
    }

    /// The priority the calling thread had before
    /// [`AudioPriority::raise_for_good`] raised it: what whoever made the
    /// thread had left it at.
    pub fn level_before() -> Option<i32> {
        let before = RAISED.try_with(|raised| raised.try_borrow().ok()?.as_ref().and_then(|raised| raised.1));
        before.ok().flatten()
    }

    /// The priority the system says the calling thread has, by its own
    /// numbers (on Windows 0 is an ordinary thread, 15 the most a thread
    /// of an ordinary program is given by asking; a thread in the audio
    /// class is moved by the system as it sees fit; on macOS 31 is an
    /// ordinary thread and 97 one with a time-constraint policy; on Linux a
    /// real-time thread's priority, 1 to 99, and 0 an ordinary one). `None`
    /// where the system has no such number.
    pub fn level_of_this_thread() -> Option<i32> {
        #[cfg(windows)]
        {
            use windows::Win32::System::Threading::{GetCurrentThread, GetThreadPriority};
            // SAFETY: asks of the calling thread only.
            Some(unsafe { GetThreadPriority(GetCurrentThread()) })
        }
        #[cfg(target_os = "macos")]
        return mac::level();
        #[cfg(target_os = "linux")]
        return linux::level();
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        None
    }
}

thread_local! {
    /// What [`AudioPriority::raise_for_good`] got this thread, and the
    /// priority it had before. Given up when the thread ends.
    static RAISED: std::cell::RefCell<Option<(AudioPriority, Option<i32>)>> = const { std::cell::RefCell::new(None) };
}

impl Drop for AudioPriority {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(task) = self.task.take() {
            // SAFETY: the handle is the one this thread was given.
            let _ = unsafe { windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(task) };
        }
        #[cfg(target_os = "macos")]
        if self.standing == mac::Standing::Given {
            mac::lower();
        }
        #[cfg(target_os = "linux")]
        if matches!(self.standing, linux::Standing::Given | linux::Standing::Asked) {
            linux::lower();
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// How long a buffer of the stream lasts, in nanoseconds; 0 before a
    /// stream said.
    static PERIOD_NS: AtomicU64 = AtomicU64::new(0);
    /// What a thread is given before any stream said its period: a buffer
    /// of 512 frames at 48 kHz.
    const USUAL_PERIOD_NS: u64 = 10_666_667;
    /// The share of a period a thread says it computes for. The system
    /// takes the policy away from a thread that computes for much longer
    /// than it said, for a while, and a block spread over the team may
    /// take most of a buffer on one of its threads.
    const COMPUTES_FOR: f64 = 0.5;

    pub(super) fn expect_period(frames: u32, sample_rate: f64) {
        if frames > 0 && sample_rate > 0.0 {
            let ns = (f64::from(frames) / sample_rate * 1e9).round() as u64;
            PERIOD_NS.store(ns, Ordering::Relaxed);
        }
    }

    /// How a thread came to be in the class, or not.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub(super) enum Standing {
        /// The system made it a time-constraint thread itself, as it does
        /// the thread of a device's callback: it is left as it is.
        Had,
        /// It was given a policy here, which it gives up when dropped.
        Given,
        /// It asked and the system said no.
        Refused,
    }

    fn this_thread() -> libc::mach_port_t {
        // SAFETY: names the calling thread; the port is the thread's own
        // and is not one that has to be given back.
        unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) }
    }

    /// Whether the calling thread has a time-constraint policy now.
    fn constrained() -> bool {
        let mut policy =
            libc::thread_time_constraint_policy { period: 0, computation: 0, constraint: 0, preemptible: 0 };
        let mut count = libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT;
        // Set by the call to whether what it filled in is only the default.
        let mut default: libc::boolean_t = 0;
        // SAFETY: the structure and its count are the ones the flavour is
        // asked with, for the calling thread.
        let asked = unsafe {
            libc::thread_policy_get(
                this_thread(),
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                (&raw mut policy).cast(),
                &mut count,
                &mut default,
            )
        };
        asked == libc::KERN_SUCCESS && default == 0
    }

    /// How Mach's units of time become nanoseconds: `numer / denom`.
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        /// Declared here: `libc` has it only under a name it is retiring.
        fn mach_timebase_info(info: *mut Timebase) -> libc::kern_return_t;
    }

    /// Mach's own units of time for `ns` nanoseconds.
    fn ticks(ns: u64) -> u32 {
        let mut base = Timebase { numer: 0, denom: 0 };
        // SAFETY: fills in a local.
        unsafe { mach_timebase_info(&mut base) };
        if base.numer == 0 {
            return ns.min(u64::from(u32::MAX)) as u32;
        }
        (ns as u128 * u128::from(base.denom) / u128::from(base.numer)).min(u128::from(u32::MAX)) as u32
    }

    pub(super) fn raise() -> Standing {
        if constrained() {
            return Standing::Had;
        }
        let period_ns = match PERIOD_NS.load(Ordering::Relaxed) {
            0 => USUAL_PERIOD_NS,
            said => said,
        };
        let period = ticks(period_ns);
        let mut policy = libc::thread_time_constraint_policy {
            period,
            computation: ticks((period_ns as f64 * COMPUTES_FOR) as u64),
            constraint: period,
            preemptible: 1,
        };
        // SAFETY: the structure and its count are the ones the flavour is
        // set with, for the calling thread.
        let set = unsafe {
            libc::thread_policy_set(
                this_thread(),
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                (&raw mut policy).cast(),
                libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            )
        };
        if set == libc::KERN_SUCCESS { Standing::Given } else { Standing::Refused }
    }

    /// Gives the calling thread the policy of any other back.
    pub(super) fn lower() {
        let mut policy = libc::thread_standard_policy { no_data: 0 };
        // SAFETY: the structure and its count are the ones the flavour is
        // set with, for the calling thread.
        unsafe {
            libc::thread_policy_set(
                this_thread(),
                libc::THREAD_STANDARD_POLICY as libc::thread_policy_flavor_t,
                (&raw mut policy).cast(),
                libc::THREAD_STANDARD_POLICY_COUNT as libc::mach_msg_type_number_t,
            );
        }
    }

    /// The calling thread's scheduling priority now, by the system's
    /// numbers.
    pub(super) fn level() -> Option<i32> {
        // SAFETY: a structure of numbers, for which nothing is a fine start.
        let mut info: libc::thread_extended_info = unsafe { std::mem::zeroed() };
        let mut count = libc::THREAD_EXTENDED_INFO_COUNT;
        // SAFETY: the structure and its count are the ones the flavour is
        // asked with, for the calling thread.
        let asked = unsafe {
            libc::thread_info(
                this_thread(),
                libc::THREAD_EXTENDED_INFO as libc::thread_flavor_t,
                (&raw mut info).cast(),
                &mut count,
            )
        };
        (asked == libc::KERN_SUCCESS).then_some(info.pth_curpri)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn a_thread_that_is_raised_is_in_the_class_and_leaves_it_when_it_lets_go() {
        std::thread::spawn(|| {
            let before = AudioPriority::level_of_this_thread().expect("macOS says a thread's priority");
            let raised = AudioPriority::raise();
            assert!(raised.granted(), "macOS gives an ordinary program's thread a time-constraint policy");
            let during = AudioPriority::level_of_this_thread().unwrap();
            assert!(during > before, "{during} is not above {before}");
            // A thread that has the class already keeps what it has.
            let again = AudioPriority::raise();
            assert!(again.granted());
            drop(again);
            assert_eq!(AudioPriority::level_of_this_thread(), Some(during), "what it had is not taken away");
            drop(raised);
            assert_eq!(AudioPriority::level_of_this_thread(), Some(before));
        })
        .join()
        .unwrap();
    }
}

#[cfg(target_os = "linux")]
#[path = "priority/linux.rs"]
mod linux;
