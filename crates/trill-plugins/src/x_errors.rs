//! What an X error on a plugin's own connection does to Trill, on Linux.
//!
//! A plugin's editor draws through an X connection of its own, made with
//! libX11, and libX11's handler of errors is the process's, not the
//! connection's: unless someone has set another, an error the server reports
//! (a window that is already gone, as a plugin may ask about while its
//! editor closes) prints a line and ends the whole program, with whatever
//! song was open. Measured with Surge XT: JUCE sets none.
//!
//! So once libX11 is found loaded (found, never linked: Trill does not come
//! to depend on it), Trill's own handler takes the errors. Each kind is said
//! in the log once, and the connection goes on. A handler a plugin had set
//! of its own (not libX11's default) is told of every error as before. The
//! last to set a handler has it, so Trill's is put back after a plugin is
//! made and after each editor opens ([`hold`]).
//!
//! A connection that is lost (the X server went) is libX11's other handler,
//! which is left as it is: libX11 ends the program once it returns, and a
//! plugin thread kept from that for good is a program that can no longer
//! save its plugins' states. The safety copy has the song.

use std::ffi::{c_int, c_uchar, c_ulong, c_void};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// libX11's `XErrorEvent`.
#[repr(C)]
struct XErrorEvent {
    kind: c_int,
    display: *mut c_void,
    resource: c_ulong,
    serial: c_ulong,
    error_code: c_uchar,
    request_code: c_uchar,
    minor_code: c_uchar,
}

type Handler = unsafe extern "C" fn(*mut c_void, *mut XErrorEvent) -> c_int;
type SetHandler = unsafe extern "C" fn(Option<Handler>) -> Option<Handler>;

/// The handler a plugin had set of its own, as an address; 0 for none.
static PREVIOUS: AtomicUsize = AtomicUsize::new(0);
/// libX11's own handler, as libX11 itself said it; 0 until it was asked.
static DEFAULT: AtomicUsize = AtomicUsize::new(0);
/// The kinds already said in the log: an error code and a request, a bit each.
static SAID: [AtomicU64; 1024] = [const { AtomicU64::new(0) }; 1024];
/// How many errors Trill's handler has taken, in all.
static HEARD: AtomicU64 = AtomicU64::new(0);

/// How many X errors Trill's handler has taken in this process.
pub(crate) fn heard() -> u64 {
    HEARD.load(Ordering::Relaxed)
}

/// Has Trill's handler take X errors, where libX11 is loaded; and keeps the
/// one that was there, if it was a plugin's own. Called after a plugin is
/// made and after an editor opens, on the plugin thread.
pub(crate) fn hold() {
    // SAFETY: only asks for a library that is loaded already; it is let go
    // of below, and stays loaded for whoever loaded it.
    let library = unsafe { libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
    if library.is_null() {
        return;
    }
    // SAFETY: a symbol of a library that is loaded.
    let set = unsafe { libc::dlsym(library, c"XSetErrorHandler".as_ptr()) };
    if !set.is_null() {
        // SAFETY: libX11's `XSetErrorHandler`, of this type.
        let set: SetHandler = unsafe { std::mem::transmute::<*mut c_void, SetHandler>(set) };
        let ours = taken as Handler as usize;
        // SAFETY: sets handlers of the right type, which live as long as the
        // program; libX11 takes its lock itself.
        let before = unsafe {
            if DEFAULT.load(Ordering::Acquire) == 0 {
                // libX11 says which is its own: no handler puts its default
                // back and answers with what was there, and the next call
                // answers with the default. Asked once, and known for certain
                // wherever names can be read or not.
                let before = set(None);
                if let Some(default) = set(Some(taken)) {
                    DEFAULT.store(default as usize, Ordering::Release);
                }
                before
            } else {
                set(Some(taken))
            }
        };
        if let Some(before) = before.map(|handler| handler as usize)
            && before != ours
            && before != DEFAULT.load(Ordering::Acquire)
        {
            PREVIOUS.store(before, Ordering::Release);
        }
    }
    // SAFETY: the reference taken above.
    unsafe { libc::dlclose(library) };
}

/// Whether `address` is still in a library that is loaded: a plugin's
/// handler goes with its library.
fn is_loaded(address: usize) -> bool {
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: only fills in `info`.
    unsafe { libc::dladdr(address as *const c_void, &mut info) != 0 }
}

/// Trill's handler: says each kind once, tells a plugin's own handler, and
/// has libX11 go on.
unsafe extern "C" fn taken(display: *mut c_void, event: *mut XErrorEvent) -> c_int {
    HEARD.fetch_add(1, Ordering::Relaxed);
    // SAFETY: libX11 passes the event it is reporting.
    let Some(error) = (unsafe { event.as_ref() }) else { return 0 };
    let kind = (usize::from(error.error_code) << 8) | usize::from(error.request_code);
    let bit = 1u64 << (kind % 64);
    if SAID[kind / 64].fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        log::warn!(
            "a plugin's X connection was told of an error ({}, of request {}); Trill goes on, and says no more of \
             this kind",
            error_name(error.error_code),
            error.request_code
        );
    }
    let previous = PREVIOUS.load(Ordering::Acquire);
    if previous != 0 && is_loaded(previous) {
        // SAFETY: a handler a plugin set, of libX11's type, in a library that
        // is loaded; called as libX11 would have.
        let previous: Handler = unsafe { std::mem::transmute::<usize, Handler>(previous) };
        // SAFETY: as above, with what libX11 gave.
        unsafe { previous(display, event) };
    }
    0
}

fn error_name(code: c_uchar) -> String {
    let name = match code {
        1 => "BadRequest",
        2 => "BadValue",
        3 => "BadWindow",
        4 => "BadPixmap",
        5 => "BadAtom",
        6 => "BadCursor",
        7 => "BadFont",
        8 => "BadMatch",
        9 => "BadDrawable",
        10 => "BadAccess",
        11 => "BadAlloc",
        12 => "BadColor",
        13 => "BadGC",
        14 => "BadIDChoice",
        15 => "BadName",
        16 => "BadLength",
        17 => "BadImplementation",
        _ => return format!("error {code}"),
    };
    name.to_string()
}
