//! The timers a plugin registers with the host (CLAP's `timer-support`).
//!
//! A plugin on Windows or macOS can get timers from the operating system
//! and usually does. On Linux there is no event loop for it to hook into,
//! so plugins that want to be portable ask the host everywhere: without
//! timers their editors do not animate, and some do not draw at all.
//!
//! The timers tick as part of the plugin main thread's idle processing, so
//! they are as exact as that is frequent. That is enough for what they are
//! for: redrawing a meter, polling for a finished background job.

use clack_extensions::timer::TimerId;
use std::time::{Duration, Instant};

/// The most timers one plugin may have. Real plugins use one or two; the
/// limit keeps a plugin that leaks them from slowing every idle down.
const MAX_TIMERS: usize = 64;

struct Timer {
    id: TimerId,
    period: Duration,
    due: Instant,
}

/// The timers of one plugin instance.
#[derive(Default)]
pub(crate) struct Timers {
    timers: Vec<Timer>,
    next_id: u32,
}

impl Timers {
    /// Starts a timer that is first due one period after `now`. `None` if
    /// the plugin already has as many as it may.
    pub(crate) fn register(&mut self, period: Duration, now: Instant) -> Option<TimerId> {
        if self.timers.len() >= MAX_TIMERS {
            return None;
        }
        let id = TimerId(self.next_id);
        // Ids are never reused, so a plugin that unregisters one timer
        // twice cannot stop another by it.
        self.next_id = self.next_id.wrapping_add(1);
        self.timers.push(Timer { id, period, due: now + period });
        Some(id)
    }

    /// Stops a timer; returns whether there was one with this id.
    pub(crate) fn unregister(&mut self, id: TimerId) -> bool {
        let before = self.timers.len();
        self.timers.retain(|timer| timer.id != id);
        self.timers.len() != before
    }

    pub(crate) fn contains(&self, id: TimerId) -> bool {
        self.timers.iter().any(|timer| timer.id == id)
    }

    /// Appends the timers that are due at `now` to `due`, and winds each
    /// of them on by one period from now: a timer that fell behind ticks
    /// once, not once for every period it missed.
    pub(crate) fn take_due(&mut self, now: Instant, due: &mut Vec<TimerId>) {
        for timer in self.timers.iter_mut().filter(|timer| timer.due <= now) {
            timer.due = now + timer.period;
            due.push(timer.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(timers: &mut Timers, now: Instant) -> Vec<TimerId> {
        let mut due = Vec::new();
        timers.take_due(now, &mut due);
        due
    }

    #[test]
    fn a_timer_ticks_once_per_period_and_not_for_missed_ones() {
        let start = Instant::now();
        let at = |milliseconds| start + Duration::from_millis(milliseconds);
        let mut timers = Timers::default();
        let fast = timers.register(Duration::from_millis(10), start).unwrap();
        let slow = timers.register(Duration::from_millis(25), start).unwrap();
        assert_ne!(fast, slow);

        assert_eq!(due(&mut timers, at(9)), []);
        assert_eq!(due(&mut timers, at(10)), [fast]);
        assert_eq!(due(&mut timers, at(19)), [], "a period counts from the tick, not from the schedule");
        assert_eq!(due(&mut timers, at(26)), [fast, slow]);
        // Far behind: one tick each, then a full period of quiet.
        assert_eq!(due(&mut timers, at(1_000)), [fast, slow]);
        assert_eq!(due(&mut timers, at(1_009)), []);
    }

    #[test]
    fn unregistering_stops_exactly_that_timer() {
        let start = Instant::now();
        let mut timers = Timers::default();
        let first = timers.register(Duration::ZERO, start).unwrap();
        let second = timers.register(Duration::ZERO, start).unwrap();
        assert!(timers.contains(first) && timers.contains(second));

        assert!(timers.unregister(first));
        assert!(!timers.unregister(first), "there is no such timer any more");
        assert!(!timers.contains(first));
        assert_eq!(due(&mut timers, start), [second], "a period of zero is due at once, and every time");

        // The id of a timer that is gone is not handed out again.
        let third = timers.register(Duration::ZERO, start).unwrap();
        assert!(third != first && third != second);
    }

    #[test]
    fn a_plugin_cannot_register_timers_without_end() {
        let start = Instant::now();
        let mut timers = Timers::default();
        let registered: Vec<TimerId> =
            (0..MAX_TIMERS).map(|_| timers.register(Duration::from_secs(1), start).unwrap()).collect();
        assert_eq!(timers.register(Duration::from_secs(1), start), None);
        timers.unregister(registered[0]);
        assert!(timers.register(Duration::from_secs(1), start).is_some(), "room again");
    }
}
