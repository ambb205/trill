//! The Linux side of the plugin main thread: it sleeps in `poll()` until a
//! job comes, its next idle is due, or one of the X connections of the
//! editor windows it holds has something to say.
//!
//! On Linux there is no event loop of the system's that every program's
//! windows hang off. A plugin editor draws through an X connection of its
//! own (served by the plugin's own thread, or by its idle and timers that
//! this thread runs), and the window Trill puts it in has a connection of
//! Trill's (`editor/x11.rs`), whose events the editor drains when the thread
//! idles. So the thread waits on what wakes it (an eventfd) and on those
//! connections, and a connection that has something asks for an idle at
//! once instead of at the next interval.

use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

thread_local! {
    /// The connections this thread waits on besides its own wake-ups.
    static WATCHED: RefCell<Vec<RawFd>> = const { RefCell::new(Vec::new()) };
    /// The eventfd that wakes this thread, where it is a plugin thread.
    static WAKES: Cell<RawFd> = const { Cell::new(-1) };
}

/// Has the plugin thread (the calling one) wake when `fd` has something to
/// read, until [`unwatch`].
pub(crate) fn watch(fd: RawFd) {
    WATCHED.with_borrow_mut(|watched| {
        if !watched.contains(&fd) {
            watched.push(fd);
        }
    });
}

/// Stops waking for `fd`.
pub(crate) fn unwatch(fd: RawFd) {
    WATCHED.with_borrow_mut(|watched| watched.retain(|watching| *watching != fd));
}

pub(super) struct Pump {
    wakes: Arc<OwnedFd>,
}

impl Pump {
    /// Must be called on the thread that is to wait.
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: makes a descriptor, which is checked and then owned.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the descriptor was just made, and nothing else owns it.
        let wakes = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        WAKES.set(fd);
        Ok(Self { wakes })
    }

    pub(super) fn waker(&self) -> Waker {
        Waker(self.wakes.clone())
    }

    /// Sleeps until the thread is woken, a watched connection has something
    /// or `idle_in` has passed. A wake-up that came before the call ends it
    /// at once.
    pub(super) fn wait(&self, idle_in: Option<Duration>) {
        let mut fds = vec![libc::pollfd { fd: self.wakes.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        WATCHED.with_borrow(|watched| {
            fds.extend(watched.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }));
        });
        // Rounded up: a wait that ends a little before the idle is due
        // would only come round again at once.
        let timeout = idle_in.map_or(-1, |after| i32::try_from(after.as_micros().div_ceil(1000)).unwrap_or(i32::MAX));
        // SAFETY: the array is the one described, alive for the call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready <= 0 {
            return;
        }
        if fds[0].revents != 0 {
            drain(fds[0].fd);
        }
        if fds[1..].iter().any(|fd| fd.revents != 0) {
            super::IDLE_REQUESTED.set(true);
        }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        WAKES.set(-1);
    }
}

/// Reads what was written to an eventfd, so that it no longer reads ready.
fn drain(fd: RawFd) {
    let mut count = 0u64;
    // SAFETY: reads eight bytes into a local of eight bytes; the descriptor
    // does not block.
    unsafe { libc::read(fd, (&raw mut count).cast(), 8) };
}

/// Writes to an eventfd, which wakes whoever polls it.
fn ring(fd: RawFd) {
    let one = 1u64;
    // SAFETY: writes eight bytes from a local of eight bytes.
    unsafe { libc::write(fd, (&raw const one).cast(), 8) };
}

/// Ends the plugin thread's wait, from any thread.
pub(super) struct Waker(Arc<OwnedFd>);

impl Waker {
    pub(super) fn wake(&self) {
        ring(self.0.as_raw_fd());
    }
}

/// Ends the calling thread's own next wait.
pub(super) fn wake_current() {
    let fd = WAKES.get();
    if fd >= 0 {
        ring(fd);
    }
}

/// How a caller waits for the plugin thread's reply. No loop of the
/// system's runs on a caller's thread that it could miss, so it just waits.
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
