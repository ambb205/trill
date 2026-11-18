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
    /// thread that is only ever met inside a callback, and for one that
    /// wants to be asked later what it was given. The first call does it;
    /// every later one is a look at a thread-local. Returns whether the
    /// system granted its audio class.
    pub fn raise_for_good() -> bool {
        let granted = RAISED.try_with(|raised| {
            let Ok(mut raised) = raised.try_borrow_mut() else { return false };
            let raise = || {
                let before = AudioPriority::level_of_this_thread();
                (AudioPriority::raise(), before)
            };
            raised.get_or_insert_with(raise).0.granted()
        });
        granted.unwrap_or(false)
    }

    /// What [`AudioPriority::raise_for_good`] came to on the calling
    /// thread: whether the system granted its audio class. `None` on a
    /// thread that never asked.
    pub fn of_this_thread() -> Option<bool> {
        let granted = RAISED.try_with(|raised| raised.try_borrow().ok()?.as_ref().map(|raised| raised.0.granted()));
        granted.ok().flatten()
    }

    /// The priority the calling thread had before
    /// [`AudioPriority::raise_for_good`] raised it: what whoever made the
    /// thread had left it at.
    pub fn level_before() -> Option<i32> {
        let before = RAISED.try_with(|raised| raised.try_borrow().ok()?.as_ref().and_then(|raised| raised.1));
        before.ok().flatten()
    }

    /// The priority the system says the calling thread has, by its own
    /// numbers (on Windows 0 is an ordinary thread, 15 the most a thread
    /// of an ordinary program is given by asking; a thread in the audio
    /// class is moved by the system as it sees fit). `None` where the
    /// system has no such number.
    pub fn level_of_this_thread() -> Option<i32> {
        #[cfg(windows)]
        {
            use windows::Win32::System::Threading::{GetCurrentThread, GetThreadPriority};
            // SAFETY: asks of the calling thread only.
            Some(unsafe { GetThreadPriority(GetCurrentThread()) })
        }
        #[cfg(not(windows))]
        None
    }
}

thread_local! {
    /// What [`AudioPriority::raise_for_good`] got this thread, and the
    /// priority it had before. Given up when the thread ends.
    static RAISED: std::cell::RefCell<Option<(AudioPriority, Option<i32>)>> = const { std::cell::RefCell::new(None) };
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
