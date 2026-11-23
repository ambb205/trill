//! The few CoreFoundation calls a thread's run loop is driven with,
//! declared here: a source that is signalled from any thread, a timer, an
//! observer of the loop's coming and going, and running the loop.

#![allow(non_upper_case_globals, non_snake_case)]

use std::ffi::c_void;

pub(crate) type CFRunLoopRef = *mut c_void;
pub(crate) type CFRunLoopSourceRef = *mut c_void;
pub(crate) type CFRunLoopTimerRef = *mut c_void;
pub(crate) type CFRunLoopObserverRef = *mut c_void;
pub(crate) type CFStringRef = *const c_void;
pub(crate) type CFIndex = isize;
pub(crate) type CFOptionFlags = usize;

/// `CFRunLoopSourceContext` (version 0).
#[repr(C)]
pub(crate) struct SourceContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<extern "C" fn(*const c_void)>,
    pub copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
    pub equal: Option<extern "C" fn(*const c_void, *const c_void) -> u8>,
    pub hash: Option<extern "C" fn(*const c_void) -> usize>,
    pub schedule: Option<extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    pub cancel: Option<extern "C" fn(*mut c_void, CFRunLoopRef, CFStringRef)>,
    pub perform: Option<extern "C" fn(*mut c_void)>,
}

/// `CFRunLoopTimerContext` and `CFRunLoopObserverContext`, which are laid
/// out alike.
#[repr(C)]
pub(crate) struct Context {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<extern "C" fn(*const c_void)>,
    pub copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

impl Context {
    pub(crate) fn of(info: *mut c_void) -> Self {
        Self { version: 0, info, retain: None, release: None, copy_description: None }
    }
}

/// The loop is entered (`kCFRunLoopEntry`) and left (`kCFRunLoopExit`).
pub(crate) const ENTRY: CFOptionFlags = 1;
pub(crate) const EXIT: CFOptionFlags = 1 << 7;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub(crate) static kCFRunLoopCommonModes: CFStringRef;
    pub(crate) static kCFRunLoopDefaultMode: CFStringRef;

    pub(crate) fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub(crate) fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
    pub(crate) fn CFRunLoopStop(run_loop: CFRunLoopRef);
    pub(crate) fn CFRunLoopWakeUp(run_loop: CFRunLoopRef);

    pub(crate) fn CFRunLoopSourceCreate(
        allocator: *const c_void,
        order: CFIndex,
        context: *mut SourceContext,
    ) -> CFRunLoopSourceRef;
    pub(crate) fn CFRunLoopAddSource(run_loop: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    pub(crate) fn CFRunLoopSourceSignal(source: CFRunLoopSourceRef);
    pub(crate) fn CFRunLoopSourceInvalidate(source: CFRunLoopSourceRef);

    pub(crate) fn CFRunLoopTimerCreate(
        allocator: *const c_void,
        fire_date: f64,
        interval: f64,
        flags: CFOptionFlags,
        order: CFIndex,
        callout: extern "C" fn(CFRunLoopTimerRef, *mut c_void),
        context: *mut Context,
    ) -> CFRunLoopTimerRef;
    pub(crate) fn CFRunLoopAddTimer(run_loop: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    pub(crate) fn CFRunLoopTimerSetNextFireDate(timer: CFRunLoopTimerRef, fire_date: f64);
    pub(crate) fn CFRunLoopTimerInvalidate(timer: CFRunLoopTimerRef);
    pub(crate) fn CFAbsoluteTimeGetCurrent() -> f64;

    pub(crate) fn CFRunLoopObserverCreate(
        allocator: *const c_void,
        activities: CFOptionFlags,
        repeats: u8,
        order: CFIndex,
        callout: extern "C" fn(CFRunLoopObserverRef, CFOptionFlags, *mut c_void),
        context: *mut Context,
    ) -> CFRunLoopObserverRef;
    pub(crate) fn CFRunLoopAddObserver(run_loop: CFRunLoopRef, observer: CFRunLoopObserverRef, mode: CFStringRef);

    pub(crate) fn CFRetain(object: *const c_void) -> *const c_void;
    pub(crate) fn CFRelease(object: *const c_void);
}

unsafe extern "C" {
    /// Whether the calling thread is the process's first.
    pub(crate) fn pthread_main_np() -> i32;
}
