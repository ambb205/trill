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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_runs_forwards() {
        let first = now_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = now_ns();
        assert!(second >= first + 1_000_000, "{first} then {second}");
    }
}
