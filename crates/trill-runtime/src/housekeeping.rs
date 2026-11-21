//! The housekeeping thread: the program's regular chores, done whether or
//! not a window is open to do them.

use crate::RuntimeError;
use crate::recovery::{self, SafetyCopies};
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
/// Ticks between looks at whether the project needs a new safety copy:
/// twenty seconds, which is then the most that a crash can cost.
const COPY_EVERY: u32 = 100;

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
        copies: Arc<Mutex<SafetyCopies>>,
    ) -> Result<Self, RuntimeError> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = stop.clone();
        let thread = std::thread::Builder::new()
            .name("housekeeping".into())
            .spawn(move || {
                let (stopped, wake) = &*signal;
                let mut tick = 0u32;
                // Where the song was last seen to live: a song that comes to
                // live somewhere (saved, opened) is remembered among the
                // songs Trill knows, whichever front end did it.
                let mut lived: Option<std::path::PathBuf> = None;
                // The audio streams as they were when the session was last
                // given what is kept for its devices.
                let mut audio_seen = u64::MAX;
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
                    // What is kept of how long sound takes to go round the
                    // devices, for whichever of them are open now.
                    let round_trips = settings.lock().current().audio.round_trips.clone();
                    let lives = {
                        let mut session = session.lock();
                        session.housekeeping();
                        if session.audio_revision() != audio_seen {
                            crate::recording::apply_kept(&mut session, &round_trips);
                            audio_seen = session.audio_revision();
                        }
                        // What the user did in plugin windows since the
                        // last round: the project and every view follow.
                        session.absorb_editor_edits();
                        session.path().map(std::path::Path::to_path_buf)
                    };
                    if lives != lived {
                        if let Some(path) = &lives {
                            remember_song(&library, &settings, path);
                        }
                        lived = lives;
                    }
                    tick = tick.wrapping_add(1);
                    if tick.is_multiple_of(SETTINGS_EVERY) {
                        save_if_changed(&library, &settings);
                    }
                    if tick.is_multiple_of(COPY_EVERY) {
                        recovery::refresh(&session, &copies);
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

/// Puts a song at the front of the songs Trill knows: the ones it saved or
/// opened lately. A failure is logged; there is nobody here to tell.
pub(crate) fn remember_song(library: &Library, settings: &Mutex<SettingsFile>, path: &std::path::Path) {
    let mut settings = settings.lock();
    if settings.current().recent_projects.first().map(std::path::PathBuf::as_path) == Some(path) {
        return;
    }
    if let Err(error) = settings.update(library, |settings| settings.add_recent_project(path)) {
        log::warn!("{error}");
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
