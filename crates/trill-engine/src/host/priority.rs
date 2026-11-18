//! The priority the system keeps for threads that make sound.
//!
//! A thread that has a speaker to keep up with must not be taken off its
//! processor in the middle of a block because something else wants a turn.
//! On Windows that is what the multimedia class scheduler is for: a thread
//! that joins its "Pro Audio" class runs above everything ordinary for as
//! long as it does not use a whole processor all the time. Every thread
//! that makes a playing song's sound joins it: the callback of the
//! system's shared devices (whose library leaves its thread at the
//! priority of any other), the loop of a device taken exclusively, and
//! the threads a block's tracks are handed to ([`crate::team`]).
//!
//! Elsewhere nothing is asked of the system here, and the threads run as
//! their host has them.

/// The calling thread's place in the class the system keeps for audio. It
/// is given up when this is dropped, on the thread that took it.
pub struct AudioPriority {
    #[cfg(windows)]
    task: Option<windows::Win32::Foundation::HANDLE>,
}

impl AudioPriority {
    /// Raises the calling thread to the priority the system keeps for
    /// audio. Where the system does not grant its class, the thread is set
    /// as high as an ordinary one can be. Says nothing either way: it may
    /// be called from a thread that must not write a log.
    #[cfg(windows)]
    pub fn raise() -> Self {
        use windows::Win32::System::Threading::{
            AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
        };
        use windows::core::w;
        let mut index = 0u32;
        // SAFETY: both calls act on the calling thread only; `index` is a
        // live local the system writes a number to.
        let task = unsafe {
            match AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) {
                Ok(handle) => Some(handle),
                Err(_) => {
                    let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
                    None
                }
            }
        };
        Self { task }
    }

    #[cfg(not(windows))]
    pub fn raise() -> Self {
        Self {}
    }

    /// Whether the system granted its audio class. `false` where it has
    /// none to grant.
    pub fn granted(&self) -> bool {
        #[cfg(windows)]
        return self.task.is_some();
        #[cfg(not(windows))]
        false
    }

    /// Raises the calling thread once and for as long as it lives: for a
    /// thread that belongs to somebody else and is only ever met inside a
    /// callback. The first call does it; every later one is a look at a
    /// thread-local.
    pub fn raise_for_good() {
        thread_local! {
            static RAISED: std::cell::RefCell<Option<AudioPriority>> = const { std::cell::RefCell::new(None) };
        }
        let _ = RAISED.try_with(|raised| {
            if let Ok(mut raised) = raised.try_borrow_mut()
                && raised.is_none()
            {
                *raised = Some(AudioPriority::raise());
            }
        });
    }
}

impl Drop for AudioPriority {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(task) = self.task.take() {
            // SAFETY: the handle is the one this thread was given.
            let _ = unsafe { windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(task) };
        }
    }
}
