//! Memory: how much the machine has left before a process that studies
//! is started, and how much that process has taken since.
//!
//! A model's working memory follows from what it is fed, and a tensor of
//! the wrong shape asks for tens of gigabytes without a word. So a run
//! says beforehand what it needs ([`need`]), is refused when the machine
//! has not got it, and is ended when it takes more.

use crate::models::Which;
use crate::models::real::Provider;
use std::time::Duration;

/// What the models, their runtime and the program hold whatever is
/// studied, by what runs them. Each is what was measured, with a quarter
/// to spare: the part that drives a card of any make holds more than
/// NVIDIA's own libraries do, and a processor far less than either.
fn base(models: &Which) -> u64 {
    let megabytes: u64 = match models {
        Which::Scripted(_) => 1024,
        Which::Real(setup) => match setup.provider {
            Provider::Processor => 2560,
            Provider::Nvidia => 5632,
            Provider::AnyCard { .. } => 6400,
        },
    };
    megabytes << 20
}

/// And for each minute of the recording: its stems, the signals that are
/// listened to in them, and the transforms.
const PER_MINUTE: u64 = 350 << 20;
/// How often a running process is asked what it holds.
pub const LOOK_EVERY: Duration = Duration::from_millis(500);

/// The memory a run by `models` over a recording `seconds` long says it
/// needs, in bytes.
pub fn need(seconds: f64, models: &Which) -> u64 {
    base(models) + (seconds.max(0.0) / 60.0 * PER_MINUTE as f64) as u64
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
        let scripted = Which::Scripted(crate::models::scripted::Script::default());
        let on = |provider| Which::Real(crate::models::real::Setup { parts: "parts".into(), provider });
        assert!(need(900.0, &scripted) > need(240.0, &scripted));
        assert_eq!(need(0.0, &scripted), 1 << 30);
        // A card needs more than the processor, and the part for any card the most.
        let (processor, nvidia, any) = (
            need(240.0, &on(Provider::Processor)),
            need(240.0, &on(Provider::Nvidia)),
            need(240.0, &on(Provider::AnyCard { adapter: 0 })),
        );
        assert!(processor < nvidia && nvidia < any, "{processor} {nvidia} {any}");
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
