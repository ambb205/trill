//! What making a plugin took, said in the log.
//!
//! A song of many plugins takes as long to open as its plugins take to be
//! made, one after the other on the one thread every plugin is made on.
//! To know which of that time is a plugin's own and which is the host's,
//! every plugin that is made says, under the log's target [`TARGET`], how
//! long it waited for its turn on that thread (plugins being made or taken
//! down ahead of it) and what each step of its making took; and every one
//! that is taken down says how long that kept the thread. Nothing reads
//! these lines but whoever measures (`cargo run -p trill-runtime --example
//! opens`); with no logger they cost a few readings of the clock.

use std::time::{Duration, Instant};

/// The log target the lines are written under.
pub const TARGET: &str = "trill::making";

fn ms(time: Duration) -> f64 {
    time.as_secs_f64() * 1e3
}

/// The steps of one plugin's making, as they are done.
pub(crate) struct Steps {
    waited: Duration,
    began: Instant,
    last: Instant,
    taken: Vec<(&'static str, Duration)>,
}

impl Steps {
    /// A making that begins now, having waited `waited` for its turn.
    pub(crate) fn begun(waited: Duration) -> Self {
        let now = Instant::now();
        Self { waited, began: now, last: now, taken: Vec::with_capacity(8) }
    }

    /// A step begins now, after the plugin thread did other things since
    /// the step before: what passed meanwhile is not the next step's.
    pub(crate) fn resume(&mut self) {
        self.last = Instant::now();
    }

    /// The step `step` is done: what has passed since the one before was
    /// its time.
    pub(crate) fn done(&mut self, step: &'static str) {
        let now = Instant::now();
        self.taken.push((step, now.duration_since(self.last)));
        self.last = now;
    }

    /// Says what the making of the plugin called `name` took.
    pub(crate) fn say(&self, name: &str) {
        self.say_of(name, "made");
    }

    /// Says what was done to the plugin called `name` (`done`: "made",
    /// "activated again") and what it took.
    pub(crate) fn say_of(&self, name: &str, done: &str) {
        let steps: Vec<String> = self.taken.iter().map(|(step, took)| format!("{step} {:.0} ms", ms(*took))).collect();
        log::debug!(
            target: TARGET,
            "{name}: {done} in {:.0} ms ({}), after {:.0} ms of waiting for the plugin thread",
            ms(self.last.duration_since(self.began)),
            steps.join(", "),
            ms(self.waited)
        );
    }
}

/// Says that the plugin thread was `took` with what its plugins had posted
/// to their windows (`messages` of them, one of which took `longest`),
/// where that is long enough to have kept a plugin waiting.
// Only the Windows layer has windows of others to serve.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn windows_served(took: Duration, messages: usize, longest: Duration) {
    if took >= Duration::from_millis(20) {
        log::debug!(
            target: TARGET,
            "the plugin thread: {:.0} ms with what its plugins had posted to their windows ({messages} messages, \
             the longest {:.0} ms)",
            ms(took),
            ms(longest)
        );
    }
}

/// Says that the plugin called `name` was taken down, which kept the
/// plugin thread for `took`.
pub(crate) fn taken_down(name: &str, took: Duration) {
    log::debug!(target: TARGET, "{name}: taken down in {:.0} ms", ms(took));
}
