//! The clock live input is stamped with.
//!
//! A key goes down on one thread (a MIDI driver's, the window's) and is
//! heard through another (the audio thread). To place it in the block it
//! belongs to, the two must read the same clock: this one. It only ever
//! runs forwards, and reading it neither allocates nor waits.

use std::sync::OnceLock;
use std::time::Instant;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Nanoseconds since this clock was first read in this process.
pub fn now_ns() -> u64 {
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// How many of its processor's cycles the calling thread has run in all:
/// what the system counts only while the thread is on a processor. Read
/// before and after a piece of work, beside the clock, it tells work that
/// took long to do from a thread that was kept from doing it. One call to
/// the system that neither allocates nor waits; 0 where the system has no
/// such count.
pub fn thread_cycles() -> u64 {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::GetCurrentThread;
        use windows::Win32::System::WindowsProgramming::QueryThreadCycleTime;
        let mut cycles = 0u64;
        // SAFETY: asks of the calling thread only, into a live local.
        match unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) } {
            Ok(()) => cycles,
            Err(_) => 0,
        }
    }
    #[cfg(not(windows))]
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_that_works_runs_cycles_and_one_that_sleeps_runs_next_to_none() {
        if thread_cycles() == 0 {
            // The system keeps no such count.
            return;
        }
        let before = thread_cycles();
        let began = Instant::now();
        let mut sum = 0u64;
        while began.elapsed() < std::time::Duration::from_millis(20) {
            sum = std::hint::black_box(sum.wrapping_add(1));
        }
        let worked = thread_cycles() - before;
        let before = thread_cycles();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let slept = thread_cycles() - before;
        assert!(worked > 1_000_000 && slept * 10 < worked, "{worked} cycles working, {slept} asleep");
    }

    #[test]
    fn the_clock_runs_forwards() {
        let first = now_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = now_ns();
        assert!(second >= first + 1_000_000, "{first} then {second}");
    }
}
