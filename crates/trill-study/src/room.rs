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
            Provider::Processor => 3328,
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
/// the machine cannot be asked (Windows, macOS and Linux can), and then
/// nothing is refused for want of it.
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
    #[cfg(target_os = "macos")]
    return macos_memory().map(|(_, free)| free);
    // What the system reckons can be had without swapping.
    #[cfg(target_os = "linux")]
    return trill_fetch::memory().map(|memory| memory.free);
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    None
}

/// What macOS says of its memory, in bytes: what it has, and what can be
/// had at once (what is free, and what holds files' contents and things a
/// program said may be thrown away, which the system gives up at once).
/// Memory compressed or wired is not counted as free.
#[cfg(target_os = "macos")]
fn macos_memory() -> Option<(u64, u64)> {
    unsafe extern "C" {
        /// Declared here: `libc` has it only under a name it is retiring.
        fn mach_host_self() -> libc::mach_port_t;
    }
    let mut total = 0u64;
    let mut size = size_of::<u64>();
    // SAFETY: the name is a C string, the value a `u64` that says its size.
    let asked = unsafe {
        libc::sysctlbyname(c"hw.memsize".as_ptr(), (&raw mut total).cast(), &mut size, std::ptr::null_mut(), 0)
    };
    if asked != 0 || total == 0 {
        return None;
    }
    // SAFETY: a structure of numbers, for which nothing is a fine start.
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: the structure and its count are the ones the flavour is asked with.
    let asked =
        unsafe { libc::host_statistics64(mach_host_self(), libc::HOST_VM_INFO64, (&raw mut stats).cast(), &mut count) };
    if asked != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: a value the system sets before any code runs.
    let page = unsafe { libc::vm_page_size } as u64;
    let held = u64::from(stats.wire_count)
        + u64::from(stats.compressor_page_count)
        + u64::from(stats.internal_page_count).saturating_sub(u64::from(stats.purgeable_count));
    Some((total, total.saturating_sub(held * page)))
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
/// process cannot be asked (Windows, macOS and Linux can).
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
    // Linux gives memory to a program as it touches it, so what a process
    // holds is what is resident, and what of it was put out to swap.
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let field = |name| linux::kilobytes(&status, name);
        let now = field("VmRSS:")? + field("VmSwap:").unwrap_or(0);
        Some(Held { now, peak: field("VmHWM:").unwrap_or(0).max(now) })
    }
    #[cfg(target_os = "macos")]
    {
        // What the system counts against a process (its footprint: what it
        // wrote to, in memory or compressed), which is what a run is given
        // room for; and the most it has had at once.
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        let pid = i32::try_from(pid).ok()?;
        // SAFETY: the buffer is the structure the flavour asks for, and the
        // call writes nothing else.
        let asked = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, info.as_mut_ptr().cast()) };
        (asked == 0).then(|| {
            // SAFETY: the call succeeded, so it filled the structure in.
            let info = unsafe { info.assume_init() };
            Held { now: info.ri_phys_footprint, peak: info.ri_lifetime_max_phys_footprint.max(info.ri_phys_footprint) }
        })
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    /// The field `name` of a file of `/proc` that says sizes as "Name:  123 kB", in bytes.
    pub(super) fn kilobytes(text: &str, name: &str) -> Option<u64> {
        let line = text.lines().find(|line| line.starts_with(name))?;
        let number = line[name.len()..].trim().strip_suffix("kB")?.trim();
        number.parse::<u64>().ok().map(|kilobytes| kilobytes * 1024)
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

    #[cfg(target_os = "macos")]
    #[test]
    fn the_machine_and_this_process_say_what_they_hold_on_macos() {
        let free = free().expect("macOS can be asked");
        assert!(free > 0 && free < 1 << 50, "{free}");
        let own = held_by(std::process::id()).expect("this process is there");
        assert!(own.now > 0 && own.peak >= own.now, "{own:?}");
        assert_eq!(held_by(u32::MAX - 7), None);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn the_machine_and_this_process_say_what_they_hold() {
        assert!(free().expect("the machine can be asked") > 0);
        let own = held_by(std::process::id()).expect("this process is there");
        assert!(own.now > 0 && own.peak >= own.now, "{own:?}");
        assert_eq!(held_by(u32::MAX - 7), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_size_is_read_from_its_line() {
        let text = "VmPeak:\t  100 kB\nVmHWM:\t   12 kB\nThreads:\t3\n";
        assert_eq!(linux::kilobytes(text, "VmHWM:"), Some(12 * 1024));
        assert_eq!(linux::kilobytes(text, "Threads:"), None);
        assert_eq!(linux::kilobytes(text, "VmSwap:"), None);
    }
}
