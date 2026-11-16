//! The housekeeping thread: the program's regular chores, done whether or
//! not a window is open to do them.

use crate::RuntimeError;
use crate::settings::SettingsFile;
use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use trill_engine::SharedSession;
use trill_library::Library;

/// How often the session gets its turn. Short enough that a clip's
/// stretched audio is heard within a moment of being ready, long enough to
/// be free.
const TICK: Duration = Duration::from_millis(200);
/// Ticks between looks at whether the settings need writing.
const SETTINGS_EVERY: u32 = 5;

/// Owns the thread and stops it when dropped.
pub(crate) struct Housekeeper {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl Housekeeper {
    pub fn start(
        session: SharedSession,
        library: Arc<Library>,
        settings: Arc<Mutex<SettingsFile>>,
    ) -> Result<Self, RuntimeError> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = stop.clone();
        let thread = std::thread::Builder::new()
            .name("housekeeping".into())
            .spawn(move || {
                let (stopped, wake) = &*signal;
                let mut tick = 0u32;
                loop {
                    {
                        let mut stopped = stopped.lock();
                        if !*stopped {
                            wake.wait_for(&mut stopped, TICK);
                        }
                        if *stopped {
                            return;
                        }
                    }
                    {
                        let mut session = session.lock();
                        session.housekeeping();
                        // What the user did in plugin windows since the
                        // last round: the project and every view follow.
                        session.absorb_editor_edits();
                    }
                    tick = tick.wrapping_add(1);
                    if tick.is_multiple_of(SETTINGS_EVERY) {
                        save_if_changed(&library, &settings);
                    }
                }
            })
            .map_err(RuntimeError::Thread)?;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Housekeeper {
    fn drop(&mut self) {
        let (stopped, wake) = &*self.stop;
        *stopped.lock() = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Writes the settings if the library's part of them has changed: adding a
/// folder in any front end is then remembered without that front end
/// having to say so. A failure is logged; there is nobody here to tell.
pub(crate) fn save_if_changed(library: &Library, settings: &Mutex<SettingsFile>) {
    if let Err(error) = settings.lock().save_library_changes(library) {
        log::warn!("{error}");
    }
}
