//! Helpers shared by this crate's unit tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A directory under the system temporary directory, removed on drop.
pub(crate) struct TestDir(PathBuf);

impl TestDir {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!("trill-plugins-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(name);
        // A leftover from a crashed run with the same process id.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the temporary directory is writable");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// Creates `name` (which may contain directories) with `contents`.
    pub(crate) fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the temporary directory is writable");
        }
        std::fs::write(&path, contents).expect("the temporary directory is writable");
        path
    }

    /// A CLAP plugin at `name` that is no plugin, in the form this system
    /// installs them: a library file, or on macOS a bundle (a folder whose
    /// library is in `Contents/MacOS`). Returns the path a scan names it by.
    pub(crate) fn broken_clap(&self, name: &str) -> PathBuf {
        const NOT_A_LIBRARY: &[u8] = b"this is not a shared library";
        if cfg!(target_os = "macos") {
            let bundle = self.0.join(name);
            let stem = bundle.file_stem().and_then(|stem| stem.to_str()).expect("a plugin's name").to_owned();
            self.file(&format!("{name}/Contents/MacOS/{stem}"), NOT_A_LIBRARY);
            bundle
        } else {
            self.file(name, NOT_A_LIBRARY)
        }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Polls `condition` until it holds or `limit` passes; returns whether it
/// came to hold.
/// Waits until every device the session is making for its song is there:
/// a plugin comes after the song has it.
pub(crate) fn there(session: &mut trill_engine::Session) {
    assert!(session.wait_until_opened(std::time::Duration::from_secs(30)), "the plugin never arrived");
}

pub(crate) fn eventually(limit: std::time::Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if condition() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Fails a test that deadlocks instead of hanging the whole suite.
#[cfg_attr(not(windows), allow(dead_code))] // Editors are hosted, and their tests run, on Windows only.
pub(crate) fn within(limit: std::time::Duration, body: impl FnOnce() + Send + 'static) {
    let (done, finished) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        body();
        let _ = done.send(());
    });
    match finished.recv_timeout(limit) {
        Ok(()) => worker.join().expect("the test body finished"),
        // The body panicked: report its panic, not a timeout.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("not finished after {limit:?}: deadlocked?"),
    }
}

/// A window title no other test uses, to find a test's own editor by.
#[cfg_attr(not(windows), allow(dead_code))] // Editors are hosted, and their tests run, on Windows only.
pub(crate) fn unique_title(what: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!("{what} #{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed))
}

/// On macOS this crate's unit tests drive their plugins from the process's
/// first thread, as every program of Trill's does there
/// ([`crate::first_thread`]): an editor's window is AppKit's, which is used
/// on that thread alone. libtest runs every test on a thread of its own and
/// leaves the first thread waiting for them, so the test program lends its
/// first thread before libtest's `main` begins: a function the loader runs
/// before `main` hands `main` to a thread of its own and keeps the first
/// thread for the plugins, until the tests are done and the process ends
/// with what they came to.
#[cfg(target_os = "macos")]
mod first_thread {
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    /// `RTLD_DEFAULT`: every image of the process, the program first.
    const EVERY_IMAGE: *mut c_void = -2isize as *mut c_void;

    type Main = unsafe extern "C" fn(c_int, *const *const c_char) -> c_int;

    extern "C" fn before_main(argc: c_int, argv: *const *const c_char) {
        // SAFETY: the program's own `main`, which libtest's harness is.
        let main = unsafe { dlsym(EVERY_IMAGE, c"main".as_ptr()) };
        if main.is_null() {
            return;
        }
        let (main, argv) = (main as usize, argv as usize);
        crate::main_thread::lend(Box::new(move || {
            // SAFETY: `main` with the arguments the loader gave the program.
            unsafe { std::mem::transmute::<usize, Main>(main)(argc, argv as *const *const c_char) }
        }));
    }

    /// Run by the loader before `main`, with the program's arguments.
    #[used]
    #[unsafe(link_section = "__DATA,__mod_init_func")]
    static BEFORE_MAIN: extern "C" fn(c_int, *const *const c_char) = before_main;
}
