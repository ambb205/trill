//! The window a plugin's editor is shown in, on macOS.
//!
//! One `NSWindow` per editor, whose content is an `NSView` of the host's
//! that the plugin puts its own view into: an `NSView` is what both formats
//! embed in on macOS (CLAP's `cocoa`, VST3's `NSView`). AppKit is used on
//! the process's first thread alone, which on macOS is the plugin thread
//! wherever a program lends it or serves its plugins from it
//! ([`crate::first_thread`]); elsewhere no window is made.
//!
//! The window's delegate is called by AppKit whenever that thread handles
//! an event: from inside the thread's own loop, or from inside a loop
//! someone else runs (the one a window's edge is dragged in). Like the
//! window procedure on Windows, it calls no plugin: it writes down what
//! happened ([`Events`]) and asks the thread to idle soon. The one question
//! it cannot put off, what size a view accepts while its edge is dragged,
//! goes through a callback that asks the plugin there and then where no job
//! of the thread's is further up the stack, and otherwise lets the size
//! through, to be settled when the drag has ended.
//!
//! Sizes are in points, as AppKit's and both formats' on macOS are: a view
//! is drawn at the screen's resolution by AppKit, and is never told a scale.

use super::{Events, Size};
use crate::main_thread::request_idle;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBitmapFormat, NSScreen, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use trill_core::device::EditorImage;

/// Whether this platform hosts plugin editors.
pub(crate) const SUPPORTED: bool = true;

/// Whether a view is told how many of the screen's pixels one of its own
/// is. Not on macOS: AppKit draws a view at its screen's resolution, and
/// both formats leave the scale to it there.
pub(crate) const TELLS_SCALE: bool = false;

/// Whether editor windows are put on screen. In this crate's own unit
/// tests they are not: the windows are real, and are made, resized and
/// closed like any other, but nobody running the tests sees one.
const SHOWN: bool = !cfg!(test);
/// What a window is made with, before its view has said what it wants.
const PROVISIONAL_SIZE: f64 = 320.0;

/// What a window and its delegate share.
struct Shared {
    /// `None` once the window is gone.
    window: RefCell<Option<Retained<NSWindow>>>,
    events: Cell<Events>,
    /// Above zero while the host itself is resizing the window, so that it
    /// does not mistake its own doing for the user's.
    resizing: Cell<u32>,
    constrain: Box<dyn Fn(Size) -> Size>,
}

impl Shared {
    /// Writes down something that happened to the window, and has the
    /// plugin thread look at it without waiting for its next interval.
    fn note(&self, change: impl FnOnce(&mut Events)) {
        let mut events = self.events.get();
        change(&mut events);
        self.events.set(events);
        request_idle();
    }

    /// Runs `resize` with the window's own size changes marked as such.
    fn resizing<R>(&self, resize: impl FnOnce() -> R) -> R {
        self.resizing.set(self.resizing.get() + 1);
        let result = resize();
        self.resizing.set(self.resizing.get() - 1);
        result
    }

    fn window(&self) -> Option<Retained<NSWindow>> {
        self.window.borrow().clone()
    }
}

define_class!(
    /// What AppKit tells of a window of an editor.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TrillPluginEditorDelegate"]
    #[ivars = Rc<Shared>]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSWindowDelegate for Delegate {
        /// The user asked for the window to be closed: the host closes it,
        /// once the plugin has let go of its view.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.ivars().note(|events| events.close_requested = true);
            false
        }

        /// The user drags the window's edge: the view is asked what it makes
        /// of each size on the way.
        #[unsafe(method(windowWillResize:toSize:))]
        fn window_will_resize(&self, window: &NSWindow, frame: NSSize) -> NSSize {
            let shared = self.ivars();
            let content = window.contentRectForFrameRect(NSRect::new(NSPoint::new(0.0, 0.0), frame)).size;
            let asked = size_of(content);
            // A panic must not cross into AppKit: the size goes through.
            let accepted = catch_unwind(AssertUnwindSafe(|| (shared.constrain)(asked))).unwrap_or(asked);
            if accepted.is_empty() || accepted == asked {
                return frame;
            }
            window.frameRectForContentRect(NSRect::new(NSPoint::new(0.0, 0.0), ns_size(accepted))).size
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            let shared = self.ivars();
            if shared.resizing.get() > 0 {
                return;
            }
            if let Some(window) = shared.window() {
                let size = content_size(&window);
                shared.note(|events| events.resized = Some(size));
            }
        }

        /// The window goes, without the host having closed it.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            let shared = self.ivars();
            if shared.window.borrow().is_some() {
                shared.note(|events| events.destroyed = true);
            }
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, shared: Rc<Shared>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(shared);
        // SAFETY: `NSObject`'s `init`, on a class that adds only its ivars.
        unsafe { msg_send![super(this), init] }
    }
}

fn size_of(size: NSSize) -> Size {
    let length = |points: f64| if points.is_finite() && points > 0.0 { points.round() as u32 } else { 0 };
    Size::new(length(size.width), length(size.height))
}

fn ns_size(size: Size) -> NSSize {
    NSSize::new(f64::from(size.width), f64::from(size.height))
}

fn content_size(window: &NSWindow) -> Size {
    size_of(window.contentRectForFrameRect(window.frame()).size)
}

/// The first thread, where AppKit is used; `None` on any other.
fn first_thread() -> Option<MainThreadMarker> {
    MainThreadMarker::new()
}

/// The height of the screen whose top-left corner is the origin of the
/// positions a host says (AppKit's run up from the bottom of that screen).
fn primary_height(mtm: MainThreadMarker) -> Option<f64> {
    NSScreen::screens(mtm).firstObject().map(|screen| screen.frame().size.height)
}

/// A window for one plugin editor. Lives on the first thread; dropping it
/// closes the window.
pub(crate) struct Window {
    shared: Rc<Shared>,
    /// The window holds its delegate weakly.
    _delegate: Retained<Delegate>,
    /// The view the plugin's view is put into.
    content: Retained<NSView>,
}

impl Window {
    /// Makes a hidden window titled `title`.
    ///
    /// `owner` is a window of the application's that the editor would stay
    /// above; on macOS it is not used (see `docs/status.md`). `position` is
    /// where its top-left corner goes, in points from the top-left of the
    /// main screen; without one, or one on no screen, the window is
    /// centred.
    ///
    /// `constrain` is asked, while the user drags an edge of the window,
    /// what the view makes of each size on the way; see the module.
    pub(crate) fn create(
        title: &str,
        owner: Option<isize>,
        position: Option<(i32, i32)>,
        constrain: Box<dyn Fn(Size) -> Size>,
    ) -> Result<Self, String> {
        let _ = owner;
        let mtm = first_thread().ok_or("an editor's window can only be made on the process's first thread")?;
        // A program without a window of its own has no application yet: it
        // is made, without an icon in the Dock or a menu bar of its own.
        let had_one = crate::main_thread::has_application();
        let app = NSApplication::sharedApplication(mtm);
        if !had_one {
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
        let shared =
            Rc::new(Shared { window: RefCell::new(None), events: Cell::default(), resizing: Cell::new(0), constrain });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PROVISIONAL_SIZE, PROVISIONAL_SIZE));
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        // SAFETY: a window made on the first thread, which closing does not
        // free (the host holds it until it is dropped).
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str(title));
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        window.setContentView(Some(&content));
        let delegate = Delegate::new(mtm, shared.clone());
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        match position.zip(primary_height(mtm)).filter(|((x, y), _)| is_on_a_screen(mtm, *x, *y)) {
            Some(((x, y), top)) => window.setFrameTopLeftPoint(NSPoint::new(f64::from(x), top - f64::from(y))),
            None => window.center(),
        }
        *shared.window.borrow_mut() = Some(window);
        Ok(Self { shared, _delegate: delegate, content })
    }

    /// The view a plugin's view is put into: an `NSView`.
    pub(crate) fn handle(&self) -> *mut c_void {
        Retained::as_ptr(&self.content).cast_mut().cast()
    }

    /// How many of a view's pixels one of the window's is: one, since sizes
    /// are in points on macOS (see the module).
    pub(crate) fn scale(&self) -> f64 {
        1.0
    }

    /// Lets the user resize the window by its edges, or stops them. The
    /// area inside the frame keeps its size.
    pub(crate) fn set_resizable(&self, resizable: bool) {
        let Some(window) = self.shared.window() else { return };
        let style = window.styleMask();
        let wanted =
            if resizable { style | NSWindowStyleMask::Resizable } else { style & !NSWindowStyleMask::Resizable };
        if wanted != style {
            self.shared.resizing(|| window.setStyleMask(wanted));
        }
    }

    /// Resizes the window so that the area inside its frame is `size`, and
    /// returns the size that area then has.
    pub(crate) fn set_client_size(&self, size: Size) -> Size {
        let Some(window) = self.shared.window() else { return size };
        self.shared.resizing(|| window.setContentSize(ns_size(size)));
        content_size(&window)
    }

    /// Puts the window on screen, in front of the program's others. It
    /// takes the keyboard only with `activate`. Shown again, a window that
    /// is open comes to the front, or back from the Dock.
    pub(crate) fn show(&self, activate: bool) {
        if !SHOWN {
            return;
        }
        let Some(window) = self.shared.window() else { return };
        if window.isMiniaturized() {
            window.deminiaturize(None);
        }
        if activate {
            window.makeKeyAndOrderFront(None);
        } else {
            window.orderFront(None);
        }
    }

    /// The largest area inside the window's frame that fits the part of its
    /// screen that is free for windows: the screen less the menu bar and
    /// the Dock.
    pub(crate) fn available_size(&self) -> Option<Size> {
        let window = self.shared.window()?;
        let screen = window.screen().or_else(|| NSScreen::mainScreen(first_thread()?))?;
        let room = size_of(window.contentRectForFrameRect(screen.visibleFrame()).size);
        (!room.is_empty()).then_some(room)
    }

    /// The window's top-left corner, in points from the top-left of the
    /// main screen.
    pub(crate) fn position(&self) -> (i32, i32) {
        let Some(window) = self.shared.window() else { return (0, 0) };
        let frame = window.frame();
        let top = first_thread().and_then(primary_height).unwrap_or(0.0);
        (frame.origin.x.round() as i32, (top - (frame.origin.y + frame.size.height)).round() as i32)
    }

    /// Closes the window, if it is still there.
    pub(crate) fn destroy(&self) {
        let window = self.shared.window.borrow_mut().take();
        if let Some(window) = window {
            window.setDelegate(None);
            window.orderOut(None);
            window.close();
        }
    }

    /// What happened to the window since the last call.
    pub(crate) fn take_events(&self) -> Events {
        self.shared.events.take()
    }

    /// The area inside the window's frame as the views in it draw it, asked
    /// of the views themselves: nothing is read off the screen, which on
    /// macOS only a program the user gave leave to record it may do. A view
    /// that draws with the graphics card (a Metal or an OpenGL layer) is not
    /// in such a picture, and comes out as what is behind it.
    pub(crate) fn capture(&self) -> Option<EditorImage> {
        self.shared.window()?;
        let view = &self.content;
        let bounds = view.bounds();
        if bounds.size.width < 1.0 || bounds.size.height < 1.0 {
            return None;
        }
        let rep = view.bitmapImageRepForCachingDisplayInRect(bounds)?;
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let (width, height) = (rep.pixelsWide(), rep.pixelsHigh());
        let (bits, samples, row) = (rep.bitsPerPixel(), rep.samplesPerPixel(), rep.bytesPerRow());
        let data = rep.bitmapData();
        if data.is_null() || width <= 0 || height <= 0 || bits % 8 != 0 || bits < 24 || samples < 3 {
            return None;
        }
        let format = rep.bitmapFormat();
        let alpha_first = format.contains(NSBitmapFormat::AlphaFirst);
        let premultiplied = !format.contains(NSBitmapFormat::AlphaNonpremultiplied);
        let has_alpha = samples >= 4;
        let stride = (bits / 8) as usize;
        let (width, height, row) = (width as usize, height as usize, row as usize);
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            // SAFETY: the representation's own bytes, `row` a line of them.
            let line = unsafe { std::slice::from_raw_parts(data.add(y * row), row) };
            for x in 0..width {
                let pixel = &line[x * stride..x * stride + stride];
                let (colour, alpha) = match (has_alpha, alpha_first) {
                    (true, true) => (&pixel[1..4], pixel[0]),
                    (true, false) => (&pixel[0..3], pixel[3]),
                    (false, _) => (&pixel[0..3], 255),
                };
                for &value in colour {
                    let value = if premultiplied && has_alpha && alpha > 0 && alpha < 255 {
                        ((u32::from(value) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255) as u8
                    } else {
                        value
                    };
                    rgba.push(value);
                }
                rgba.push(alpha);
            }
        }
        Some(EditorImage { width: width as u32, height: height as u32, rgba })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Whether a top-left corner, in points from the top-left of the main
/// screen, is on any screen.
fn is_on_a_screen(mtm: MainThreadMarker, x: i32, y: i32) -> bool {
    let Some(top) = primary_height(mtm) else { return false };
    let point = NSPoint::new(f64::from(x), top - f64::from(y));
    NSScreen::screens(mtm).iter().any(|screen| {
        let frame = screen.frame();
        point.x >= frame.origin.x
            && point.x < frame.origin.x + frame.size.width
            && point.y > frame.origin.y
            && point.y <= frame.origin.y + frame.size.height
    })
}

/// What a user does to a window, done to it in tests, on the first thread
/// and outside any job, where AppKit would have done it. A window is named
/// by the view the plugin's view is put into, as on Windows by the window
/// the view is given.
#[cfg(test)]
pub(crate) mod simulate {
    use super::*;

    /// Runs `what` on the first thread from its own loop, as AppKit calls
    /// a window's delegate, and waits for what it gives.
    fn there<R: Send + 'static>(what: impl FnOnce(MainThreadMarker) -> R + Send + 'static) -> R {
        crate::main_thread::from_the_first_threads_loop(move || what(first_thread().expect("the first thread")))
            .expect("the first thread drives the plugins")
    }

    fn named(window: &NSWindow) -> isize {
        window.contentView().map_or(0, |view| Retained::as_ptr(&view) as isize)
    }

    /// The window named `handle` that is open: one the host has not closed.
    fn window(mtm: MainThreadMarker, handle: isize) -> Option<Retained<NSWindow>> {
        let windows = NSApplication::sharedApplication(mtm).windows();
        windows.iter().find(|window| named(window) == handle && window.delegate().is_some())
    }

    fn with<R: Send + 'static>(handle: isize, what: impl FnOnce(&NSWindow) -> R + Send + 'static) -> R {
        there(move |mtm| what(&window(mtm, handle).expect("the window is there")))
    }

    /// The window of an editor with this title, if one is open.
    pub(crate) fn find(title: &str) -> Option<isize> {
        let title = title.to_owned();
        there(move |mtm| {
            let windows = NSApplication::sharedApplication(mtm).windows();
            let found =
                windows.iter().find(|window| window.delegate().is_some() && window.title().to_string() == title);
            found.map(|window| named(&window))
        })
    }

    /// Clicks the window's close button.
    pub(crate) fn close(window: isize) {
        with(window, |window| {
            if let Some(delegate) = window.delegate() {
                // What the button asks the delegate.
                // SAFETY: the method the delegate has, with its signature.
                let _: bool = unsafe { msg_send![&*delegate, windowShouldClose: window] };
            }
        })
    }

    /// Drags the window's corner to make the area inside the frame `size`,
    /// and returns the size the view made of it, before the drag ends.
    pub(crate) fn drag_to(window: isize, size: Size) -> Size {
        with(window, move |window| {
            let frame = window.frameRectForContentRect(NSRect::new(NSPoint::new(0.0, 0.0), ns_size(size))).size;
            let Some(delegate) = window.delegate() else { return size };
            // SAFETY: the method the delegate has, with its signature.
            let frame: NSSize = unsafe { msg_send![&*delegate, windowWillResize: window, toSize: frame] };
            size_of(window.contentRectForFrameRect(NSRect::new(NSPoint::new(0.0, 0.0), frame)).size)
        })
    }

    /// Ends a drag of the window's edge with the area inside the frame at
    /// `size`.
    pub(crate) fn resize_to(window: isize, size: Size) {
        with(window, move |window| window.setContentSize(ns_size(size)))
    }

    pub(crate) fn client_size(window: isize) -> Size {
        with(window, content_size)
    }

    pub(crate) fn position(handle: isize) -> (i32, i32) {
        there(move |mtm| {
            let window = window(mtm, handle).expect("the window is there");
            let frame = window.frame();
            let top = primary_height(mtm).unwrap_or(0.0);
            (frame.origin.x.round() as i32, (top - (frame.origin.y + frame.size.height)).round() as i32)
        })
    }

    pub(crate) fn is_resizable(window: isize) -> bool {
        with(window, |window| window.styleMask().contains(NSWindowStyleMask::Resizable))
    }

    pub(crate) fn exists(handle: isize) -> bool {
        there(move |mtm| window(mtm, handle).is_some())
    }

    pub(crate) fn is_visible(window: isize) -> bool {
        with(window, |window| window.isVisible())
    }
}
