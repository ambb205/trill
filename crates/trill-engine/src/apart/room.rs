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
    /// What the machine says now. `None` where it cannot be asked (Windows,
    /// macOS and Linux can), and then no job is refused for want of memory.
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
            let memory = trill_fetch::memory()?;
            Some(Self { free: memory.free, can_take: memory.free, total: memory.total })
        }
        // Linux gives a program memory as it touches it, and takes what it
        // has from others by swapping before it ends one: what it can take
        // is what is free and the room left in the swap.
        #[cfg(target_os = "linux")]
        {
            let memory = trill_fetch::memory()?;
            Some(Self { free: memory.free, can_take: memory.free + memory.swap_free, total: memory.total })
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        None
    }
}

#[cfg(all(test, any(windows, target_os = "macos", target_os = "linux")))]
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
