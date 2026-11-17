//! Memory: how much the machine has left before a process that studies
//! is started, and how much that process has taken since.
//!
//! A model's working memory follows from what it is fed, and a tensor of
//! the wrong shape asks for tens of gigabytes without a word. So a run
//! says beforehand what it needs ([`need`]), is refused when the machine
//! has not got it, and is ended when it takes more.

use std::time::Duration;

/// What a run is taken to need whatever it studies: the models, their
/// runtime and the program itself, as measured (see the crate's README
/// numbers) with room to spare.
const BASE: u64 = 5 << 30;
/// And for each minute of the recording: its stems, the copies the
/// analyses make of them, and the transforms.
const PER_MINUTE: u64 = 400 << 20;
/// How often a running process is asked what it holds.
pub const LOOK_EVERY: Duration = Duration::from_millis(500);

/// The memory a run over a recording `seconds` long says it needs, in bytes.
pub fn need(seconds: f64) -> u64 {
    BASE + (seconds.max(0.0) / 60.0 * PER_MINUTE as f64) as u64
}

/// The memory that is free to be used at once, in bytes. `None` where
/// the machine cannot be asked (only Windows is), and then nothing is
/// refused for want of it.
pub fn free() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        // SAFETY: the structure is the one the call fills in, and says how long it is.
        unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
        // What can be had at once, and no more than the machine will still
        // let programs commit: one that runs out of the second stops answering.
        Some(status.ullAvailPhys.min(status.ullAvailPageFile))
    }
    #[cfg(not(windows))]
    None
}

/// What a process holds.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// The memory it has committed now, in bytes.
    pub now: u64,
    /// The most it has had committed at once.
    pub peak: u64,
}

/// What the process `pid` holds. `None` when it is gone, or where a
/// process cannot be asked.
pub fn held_by(pid: u32) -> Option<Held> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        // SAFETY: the handle is opened for reading counters only and closed
        // again; the structure is the one the call fills in.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut counters = PROCESS_MEMORY_COUNTERS::default();
            let asked = K32GetProcessMemoryInfo(process, &mut counters, size_of::<PROCESS_MEMORY_COUNTERS>() as u32);
            let _ = CloseHandle(process);
            asked
                .as_bool()
                .then_some(Held { now: counters.PagefileUsage as u64, peak: counters.PeakPagefileUsage as u64 })
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

pub use trill_fetch::said;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_longer_recording_needs_more_and_bytes_are_said_as_people_say_them() {
        assert!(need(900.0) > need(240.0));
        assert_eq!(need(0.0), BASE);
        assert_eq!(said(3 << 30), "3.0 GB");
        assert_eq!(said(512 << 20), "512 MB");
    }

    #[cfg(windows)]
    #[test]
    fn the_machine_and_this_process_say_what_they_hold() {
        assert!(free().expect("Windows can be asked") > 0);
        let own = held_by(std::process::id()).expect("this process is there");
        assert!(own.now > 0 && own.peak >= own.now, "{own:?}");
        assert_eq!(held_by(u32::MAX - 7), None);
    }
}
