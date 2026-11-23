//! The plugin main thread where it has no operating system event queue to
//! serve: it simply sleeps between jobs.
//!
//! Windows, macOS and Linux each have a loop of their own
//! (`windows.rs`, `macos.rs`, `linux.rs`); this is what any other system
//! gets, where plugin editors are not hosted.

use std::io;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, Thread};
use std::time::Duration;

pub(super) struct Pump {
    thread: Thread,
}

impl Pump {
    /// Must be called on the thread that is to wait.
    pub(super) fn new() -> io::Result<Self> {
        Ok(Self { thread: thread::current() })
    }

    pub(super) fn waker(&self) -> Waker {
        Waker(self.thread.clone())
    }

    /// Sleeps until the thread is woken or `idle_in` has passed. A wake-up
    /// that came before the call ends it at once.
    pub(super) fn wait(&self, idle_in: Option<Duration>) {
        match idle_in {
            Some(after) => thread::park_timeout(after),
            None => thread::park(),
        }
    }
}

/// Ends the plugin thread's wait, from any thread.
pub(super) struct Waker(Thread);

impl Waker {
    pub(super) fn wake(&self) {
        self.0.unpark();
    }
}

/// Ends the calling thread's own next wait.
pub(super) fn wake_current() {
    thread::current().unpark();
}

/// How a caller waits for the plugin thread's reply. Without windows there
/// is nothing a waiting caller could miss, so it just waits.
pub(super) struct Arrival;

impl Arrival {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn announcer(&self) -> Announcer {
        Announcer
    }

    pub(super) fn await_reply<R>(&self, result: &Receiver<R>, timeout: Duration) -> Result<R, RecvTimeoutError> {
        result.recv_timeout(timeout)
    }
}

/// What a job takes with it; see the Windows counterpart.
pub(super) struct Announcer;
