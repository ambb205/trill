//! The Linux side of the plugin main thread: it sleeps in `poll()` until a
//! job comes, its next idle is due, or one of the X connections of the
//! editor windows it holds has something to say.
//!
//! On Linux there is no event loop of the system's that every program's
//! windows hang off. A plugin editor draws through an X connection of its
//! own (served by the plugin's own thread, or by its idle and timers that
//! this thread runs), and the window Trill puts it in has a connection of
//! Trill's (`editor/x11.rs`), whose events the editor drains when the thread
//! idles. So the thread waits on what wakes it (an eventfd), on those
//! connections, and on whatever a plugin asked the host to hear of for it
//! (a VST3 view's run loop, CLAP's `posix-fd-support`). A descriptor that is
//! ready is handed at once to what was handed in with it ([`watch`]),
//! whether a window is open or not: a server says things by itself (a
//! keyboard's mapping changed), and what is not read leaves the connection
//! ready, and the thread turning, for good. Where what was read is news,
//! an idle is asked for at once instead of at the next interval.

use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

/// What is told of a watched descriptor when it is ready, and what for:
/// whether that is news, for a window or a plugin, to be served by an idle
/// at once.
pub(crate) type Reader = Rc<dyn Fn(Ready) -> bool>;

/// Who watches a descriptor. Two may watch the same one: two instances of a
/// plugin may share the library's one connection.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Watcher {
    /// The X connection of Trill's editor windows.
    Editors,
    /// The VST3 views' run loop.
    Views,
    /// A CLAP plugin, by a number its host gave it.
    Plugin(u64),
}

/// What a descriptor is watched for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Wanted {
    pub(crate) read: bool,
    pub(crate) write: bool,
}

impl Wanted {
    pub(crate) const READ: Self = Self { read: true, write: false };

    fn events(self) -> i16 {
        (if self.read { libc::POLLIN } else { 0 }) | (if self.write { libc::POLLOUT } else { 0 })
    }
}

/// What a watched descriptor was found ready for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ready(i16);

impl Ready {
    pub(crate) fn readable(self) -> bool {
        self.0 & libc::POLLIN != 0
    }

    pub(crate) fn writable(self) -> bool {
        self.0 & libc::POLLOUT != 0
    }

    /// An error, the other end gone, or a descriptor that is no longer open.
    pub(crate) fn failed(self) -> bool {
        self.0 & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
    }

    /// The descriptor was closed without a word: it is not watched any more.
    fn closed(self) -> bool {
        self.0 & libc::POLLNVAL != 0
    }
}

struct Watch {
    watcher: Watcher,
    fd: RawFd,
    wanted: Wanted,
    read: Reader,
}

/// How many times a plugin thread has waited, in all: for tests that hold
/// that it sleeps.
static WAITS: AtomicU64 = AtomicU64::new(0);

/// How many times the plugin threads have waited so far.
#[cfg(test)]
pub(crate) fn waits() -> u64 {
    WAITS.load(Ordering::Relaxed)
}

thread_local! {
    /// The connections this thread waits on besides its own wake-ups, each
    /// with what reads it.
    static WATCHED: RefCell<Vec<Watch>> = const { RefCell::new(Vec::new()) };
    /// The eventfd that wakes this thread, where it is a plugin thread.
    static WAKES: Cell<RawFd> = const { Cell::new(-1) };
}

/// Has the plugin thread (the calling one) wake when `fd` is ready for what
/// is `wanted` (or has failed), and tell `read`, until [`unwatch`]. Watching
/// it again for the same watcher takes the place of what was there.
pub(crate) fn watch(watcher: Watcher, fd: RawFd, wanted: Wanted, read: Reader) {
    WATCHED.with_borrow_mut(|watched| {
        watched.retain(|watch| (watch.watcher, watch.fd) != (watcher, fd));
        watched.push(Watch { watcher, fd, wanted, read });
    });
}

/// Stops waking for `fd` on behalf of `watcher`.
pub(crate) fn unwatch(watcher: Watcher, fd: RawFd) {
    WATCHED.with_borrow_mut(|watched| watched.retain(|watch| (watch.watcher, watch.fd) != (watcher, fd)));
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
        WAITS.fetch_add(1, Ordering::Relaxed);
        let mut fds = vec![libc::pollfd { fd: self.wakes.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        let readers: Vec<(Watcher, Reader)> = WATCHED.with_borrow(|watched| {
            fds.extend(watched.iter().map(|watch| libc::pollfd {
                fd: watch.fd,
                events: watch.wanted.events(),
                revents: 0,
            }));
            watched.iter().map(|watch| (watch.watcher, watch.read.clone())).collect()
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
        // Each ready descriptor is told of now, to its reader (which may let
        // go of it), and news has the idle run at once. One that was closed
        // without a word would read ready for good: it is let go of.
        let mut news = false;
        for (polled, (watcher, read)) in fds[1..].iter().zip(readers) {
            if polled.revents != 0 {
                let ready = Ready(polled.revents);
                news |= read(ready);
                if ready.closed() {
                    unwatch(watcher, polled.fd);
                }
            }
        }
        if news {
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
