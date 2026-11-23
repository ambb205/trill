//! [`Vitals::now`](super::Vitals::now) as Linux says them.
//!
//! Memory in use is what the program's allocations hold now, by the C
//! allocator's own count (glibc's `mallinfo2`): as on macOS, the allocator
//! keeps what was freed and hands it out again, so memory a program grows
//! into that way is resident already, and a sampler that loads in a program
//! that let go of a song before shows only in what is allocated. Where the
//! C library is not glibc, it is what is resident. Memory asked for is
//! what the program wrote to: its anonymous memory, resident or swapped.
//! Reads are what came from a disk, and the calls that read; handles are
//! the files the program has open and its threads.

use super::Vitals;
use std::time::Duration;

pub(super) fn vitals() -> Option<Vitals> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<u64> {
        let line = status.lines().find(|line| line.starts_with(name))?;
        let number = line[name.len()..].trim();
        let number = number.strip_suffix("kB").map(str::trim).unwrap_or(number);
        number.parse().ok()
    };
    let resident = field("VmRSS:")? * 1024;
    let written = (field("RssAnon:").unwrap_or(0) + field("VmSwap:").unwrap_or(0)) * 1024;
    let threads = field("Threads:").unwrap_or(0) as u32;
    let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let counted = |name: &str| -> u64 {
        io.lines().find_map(|line| line.strip_prefix(name)).and_then(|rest| rest.trim().parse().ok()).unwrap_or(0)
    };
    let files = std::fs::read_dir("/proc/self/fd").map_or(0, |listed| listed.count() as u32);
    Some(Vitals {
        in_use: allocated().unwrap_or(resident),
        asked: written,
        read: counted("read_bytes:"),
        reads: counted("syscr:"),
        busy: spent(libc::RUSAGE_SELF),
        own: spent(libc::RUSAGE_THREAD),
        handles: files + threads,
    })
}

/// The processor time used by the process or the calling thread.
fn spent(who: libc::c_int) -> Duration {
    // SAFETY: a structure of numbers, filled in by the call.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::getrusage(who, &mut usage) } != 0 {
        return Duration::ZERO;
    }
    let time = |time: libc::timeval| Duration::new(time.tv_sec as u64, time.tv_usec as u32 * 1000);
    time(usage.ru_utime) + time(usage.ru_stime)
}

/// What the C allocator's allocations hold now, where it is glibc's.
#[cfg(target_env = "gnu")]
fn allocated() -> Option<u64> {
    // SAFETY: returns a structure of numbers and touches nothing else.
    let info = unsafe { libc::mallinfo2() };
    Some((info.uordblks + info.hblkhd) as u64)
}

#[cfg(not(target_env = "gnu"))]
fn allocated() -> Option<u64> {
    None
}
