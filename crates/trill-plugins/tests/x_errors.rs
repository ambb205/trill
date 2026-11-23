//! An X error on a connection of a plugin's own does not end Trill: the
//! handler Trill puts in place takes it, and a plugin's own handler is told
//! of it too. A process of its own, because libX11's handler is the
//! process's. Where libX11 or an X server is not on the machine it says so
//! and passes.
#![cfg(target_os = "linux")]

use std::ffi::{c_int, c_ulong, c_void};
use std::sync::atomic::{AtomicU64, Ordering};

type Handler = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type Open = unsafe extern "C" fn(*const i8) -> *mut c_void;
type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
type Map = unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int;
type Flush = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type SetHandler = unsafe extern "C" fn(Option<Handler>) -> Option<Handler>;

/// libX11, as a plugin has it: loaded, and asked by name.
struct X11 {
    open: Open,
    close: Close,
    map: Map,
    sync: Flush,
    set_handler: SetHandler,
}

fn load() -> Option<X11> {
    // SAFETY: loads a library of the system's, and reads functions of
    // libX11's own types from it; it stays loaded for the process.
    unsafe {
        let library = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW);
        if library.is_null() {
            return None;
        }
        let symbol = |name: &std::ffi::CStr| libc::dlsym(library, name.as_ptr());
        Some(X11 {
            open: std::mem::transmute::<*mut c_void, Open>(symbol(c"XOpenDisplay")),
            close: std::mem::transmute::<*mut c_void, Close>(symbol(c"XCloseDisplay")),
            map: std::mem::transmute::<*mut c_void, Map>(symbol(c"XMapWindow")),
            sync: std::mem::transmute::<*mut c_void, Flush>(symbol(c"XSync")),
            set_handler: std::mem::transmute::<*mut c_void, SetHandler>(symbol(c"XSetErrorHandler")),
        })
    }
}

static THE_PLUGINS: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn plugins_own(_display: *mut c_void, _event: *mut c_void) -> c_int {
    THE_PLUGINS.fetch_add(1, Ordering::Relaxed);
    0
}

/// A window nobody made: the server answers BadWindow.
const NO_WINDOW: c_ulong = 0x1ff_fff1;

#[test]
fn an_x_error_on_a_plugins_own_connection_is_taken_and_trill_goes_on() {
    let Some(x) = load() else {
        eprintln!("skipping: libX11 is not on this machine, and no plugin's connection can have its errors");
        return;
    };
    // SAFETY: libX11's calls, on a connection of this process's own.
    let display = unsafe { (x.open)(std::ptr::null()) };
    if display.is_null() {
        eprintln!("skipping: no X server here (DISPLAY) to report an error");
        return;
    }
    let provoke = || {
        // SAFETY: as above; the window is no window, which is the point.
        unsafe {
            (x.map)(display, NO_WINDOW);
            (x.sync)(display, 0);
        }
    };

    // With libX11's own handler, this would end the process here.
    let before = trill_plugins::hold_x_errors();
    provoke();
    assert_eq!(trill_plugins::hold_x_errors(), before + 1, "Trill's handler took the error, and the process lives");
    provoke();
    assert_eq!(trill_plugins::hold_x_errors(), before + 2, "every error is taken, though only the first is said");

    // A plugin sets a handler of its own, and Trill's is put back after it:
    // the plugin is still told.
    // SAFETY: sets a handler of libX11's type, which lives as long as the program.
    unsafe { (x.set_handler)(Some(plugins_own)) };
    let before = trill_plugins::hold_x_errors();
    provoke();
    assert_eq!(trill_plugins::hold_x_errors(), before + 1, "Trill's handler is the one in place");
    assert_eq!(THE_PLUGINS.load(Ordering::Relaxed), 1, "and the plugin's own was told");

    // SAFETY: the connection made above, let go of once.
    unsafe { (x.close)(display) };
}
