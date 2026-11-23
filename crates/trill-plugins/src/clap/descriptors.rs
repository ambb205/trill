//! The descriptors a plugin asks the host to hear of for it, on Linux
//! (CLAP's `posix-fd-support`): its own X connection, a pipe from a thread
//! of its own.
//!
//! A plugin on Linux has no loop of the system's to hang them off, so it
//! hands them to the host. Each is watched by the plugin thread's `poll()`
//! ([`crate::main_thread::watch`]); what that finds ready is noted here and
//! has the thread idle at once, and the idle tells the plugin (`on_fd`), as
//! it tells it of its timers. The plugin is told on the plugin thread, never
//! from inside the wait. What it does not take away (a connection not read,
//! a pipe that is always writable) it is told of again, as CLAP has it:
//! the descriptors are level-triggered. Whatever it left registered is let
//! go of when the instance goes.

use crate::main_thread::{self, Ready, Wanted, Watcher};
use clack_extensions::posix_fd::FdFlags;
use std::cell::RefCell;
use std::os::fd::RawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The most one plugin may register. Real plugins use one or two.
const MOST: usize = 64;

/// What was found ready, by descriptor, until the idle takes it.
type Found = Rc<RefCell<Vec<(RawFd, FdFlags)>>>;

/// The descriptors of one plugin instance.
pub(crate) struct Descriptors {
    watcher: Watcher,
    held: Vec<(RawFd, FdFlags)>,
    found: Found,
}

impl Default for Descriptors {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            watcher: Watcher::Plugin(NEXT.fetch_add(1, Ordering::Relaxed)),
            held: Vec::new(),
            found: Found::default(),
        }
    }
}

impl Descriptors {
    pub(crate) fn register(&mut self, fd: RawFd, flags: FdFlags) -> Result<(), &'static str> {
        if fd < 0 {
            return Err("not a descriptor");
        }
        if self.has(fd) {
            return Err("that descriptor is registered already");
        }
        if self.held.len() >= MOST {
            return Err("too many descriptors");
        }
        self.held.push((fd, flags));
        self.watch(fd, flags);
        log::debug!("a CLAP plugin has the host hear of descriptor {fd} ({flags:?})");
        Ok(())
    }

    pub(crate) fn modify(&mut self, fd: RawFd, flags: FdFlags) -> Result<(), &'static str> {
        let held = self.held.iter_mut().find(|(held, _)| *held == fd).ok_or("no such descriptor")?;
        held.1 = flags;
        self.watch(fd, flags);
        Ok(())
    }

    pub(crate) fn unregister(&mut self, fd: RawFd) -> Result<(), &'static str> {
        let before = self.held.len();
        self.held.retain(|(held, _)| *held != fd);
        if self.held.len() == before {
            return Err("no such descriptor");
        }
        main_thread::unwatch(self.watcher, fd);
        self.found.borrow_mut().retain(|(found, _)| *found != fd);
        Ok(())
    }

    /// Lets go of every descriptor: for a plugin that cannot be told of them.
    pub(crate) fn forget(&mut self) {
        for (fd, _) in self.held.drain(..) {
            main_thread::unwatch(self.watcher, fd);
        }
        self.found.borrow_mut().clear();
    }

    pub(crate) fn has(&self, fd: RawFd) -> bool {
        self.held.iter().any(|(held, _)| *held == fd)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Moves what was found ready since the last call into `ready`, as the
    /// plugin is to be told of it: of the descriptors it still has, and only
    /// what it asks for now (and an error, which it is always told of).
    pub(crate) fn take_ready(&self, ready: &mut Vec<(RawFd, FdFlags)>) {
        ready.clear();
        let Ok(mut found) = self.found.try_borrow_mut() else { return };
        for (fd, flags) in found.drain(..) {
            if let Some((_, asked)) = self.held.iter().find(|(held, _)| *held == fd) {
                let told = flags & (*asked | FdFlags::ERROR);
                if !told.is_empty() {
                    ready.push((fd, told));
                }
            }
        }
    }

    fn watch(&self, fd: RawFd, flags: FdFlags) {
        let wanted = Wanted { read: flags.contains(FdFlags::READ), write: flags.contains(FdFlags::WRITE) };
        let found = Rc::downgrade(&self.found);
        let note = move |ready: Ready| {
            let Some(found) = found.upgrade() else { return false };
            let mut flags = FdFlags::empty();
            flags.set(FdFlags::READ, ready.readable());
            flags.set(FdFlags::WRITE, ready.writable());
            flags.set(FdFlags::ERROR, ready.failed());
            let Ok(mut found) = found.try_borrow_mut() else { return false };
            match found.iter_mut().find(|(noted, _)| *noted == fd) {
                Some((_, noted)) => *noted |= flags,
                None => found.push((fd, flags)),
            }
            true
        };
        main_thread::watch(self.watcher, fd, wanted, Rc::new(note));
    }
}

impl Drop for Descriptors {
    fn drop(&mut self) {
        if !self.held.is_empty() {
            log::debug!("a CLAP plugin left {} descriptor(s) registered; let go of", self.held.len());
        }
        self.forget();
    }
}
