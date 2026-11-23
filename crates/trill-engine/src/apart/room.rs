//! How much memory the machine has left, asked before a process that
//! holds a song's plugins a second time is started.

/// What the machine says of its memory.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Room {
    /// Memory that is free to be used at once, in bytes.
    pub free: u64,
    /// How much more the machine will let programs commit: what is left
    /// of its memory and its paging file together. A machine that runs
    /// out of this stops answering.
    pub can_take: u64,
    /// The memory the machine has.
    pub total: u64,
}

impl Room {
    /// What the machine says now. `None` where it cannot be asked (Windows
    /// and macOS can), and then no job is refused for want of memory.
    ///
    /// macOS grows its swap as it needs and compresses what is not used,
    /// so it has no limit on what may be committed short of its disk: what
    /// it can take is said to be what is free.
    pub fn now() -> Option<Self> {
        #[cfg(windows)]
        {
            use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
            let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
            // SAFETY: the structure is the one the call fills in, and says
            // how long it is.
            unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
            Some(Self { free: status.ullAvailPhys, can_take: status.ullAvailPageFile, total: status.ullTotalPhys })
        }
        #[cfg(target_os = "macos")]
        {
            let (total, free) = macos_memory()?;
            Some(Self { free, can_take: free, total })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        None
    }
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
    /// The host's port, taken once: each `mach_host_self` is a send right
    /// that would have to be given back.
    fn host() -> libc::mach_port_t {
        static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
        // SAFETY: names this machine's host; kept for the life of the process.
        *HOST.get_or_init(|| unsafe { mach_host_self() })
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
    let asked = unsafe { libc::host_statistics64(host(), libc::HOST_VM_INFO64, (&raw mut stats).cast(), &mut count) };
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

// Only Windows and macOS can be asked yet.
#[cfg(all(test, any(windows, target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn the_machine_says_how_much_memory_it_has_left() {
        let room = Room::now().expect("the system can be asked");
        assert!(room.total >= 1 << 30, "{room:?}");
        assert!(room.free > 0 && room.free <= room.total, "{room:?}");
        assert!(room.can_take > 0, "{room:?}");
    }
}
