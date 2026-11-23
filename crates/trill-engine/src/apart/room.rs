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
    /// What the machine says now. `None` where it cannot be asked (only
    /// Windows is), and then no job is refused for want of memory.
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
        #[cfg(not(windows))]
        None
    }
}

// Only Windows can be asked yet.
#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn the_machine_says_how_much_memory_it_has_left() {
        let room = Room::now().expect("Windows can be asked");
        assert!(room.total >= 1 << 30, "{room:?}");
        assert!(room.free > 0 && room.free <= room.total, "{room:?}");
        assert!(room.can_take > 0, "{room:?}");
    }
}
