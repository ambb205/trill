//! The window a plugin's editor is shown in, on Windows.
//!
//! One top-level window per editor, with the plugin's view as its only
//! child. The window is created, used and destroyed on the plugin main
//! thread, which is also the thread its window procedure runs on.
//!
//! The window procedure can be called at any time that thread dispatches
//! messages, including from inside a call into a plugin. So it does not
//! call plugins and does not touch the thread's state: it writes down what
//! happened ([`Events`]) and asks the thread to idle soon. The one question
//! it cannot put off, what size a view accepts while the user drags the
//! window's edge, goes through a callback that is free to say "ask me
//! later".

use super::{Events, Size};
use crate::main_thread::request_idle;
use std::cell::Cell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::sync::OnceLock;
use trill_core::device::EditorImage;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACK_BRUSH, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GdiFlush, GetDC, GetStockObject, MONITOR_DEFAULTTONULL, MonitorFromPoint, ReleaseDC, SelectObject,
};
use windows_sys::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GW_CHILD, GWL_EXSTYLE, GWL_STYLE,
    GWLP_USERDATA, GetClientRect, GetWindow, GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, HWND_TOP, IDC_ARROW,
    IsIconic, IsWindow, LoadCursorW, LoadIconW, RegisterClassExW, SIZE_MINIMIZED, SW_RESTORE, SW_SHOWNOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WINDOWPLACEMENT, WM_CLOSE, WM_DPICHANGED, WM_NCCREATE, WM_NCDESTROY,
    WM_SETFOCUS, WM_SIZE, WM_SIZING, WMSZ_BOTTOMLEFT, WMSZ_LEFT, WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT, WNDCLASSEXW,
    WS_CAPTION, WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_THICKFRAME,
};

/// Whether this platform hosts plugin editors.
pub(crate) const SUPPORTED: bool = true;

/// Whether editor windows are put on screen. In this crate's own unit
/// tests they are not: the windows are real, and are created, resized and
/// destroyed like any other, but nobody running the tests sees one.
const SHOWN: bool = !cfg!(test);
/// What a window is created with, before its view has said what it wants.
const PROVISIONAL_SIZE: i32 = 320;
/// The DPI at which one logical pixel is one physical pixel.
const BASE_DPI: f64 = 96.0;
/// `PrintWindow`'s flag for capturing what the compositor shows, without
/// which views drawn by a GPU come out black.
const PW_RENDERFULLCONTENT: u32 = 2;
/// The resource the application's icon is embedded as, if it has one.
const APPLICATION_ICON: usize = 1;
/// NUL-terminated UTF-16 for "TrillPluginEditor".
const CLASS_NAME: &[u16] = &{
    let ascii = b"TrillPluginEditor\0";
    let mut wide = [0u16; 18];
    let mut index = 0;
    while index < ascii.len() {
        wide[index] = ascii[index] as u16;
        index += 1;
    }
    wide
};

/// What a window and its window procedure share.
struct Shared {
    /// Null once the window is destroyed.
    window: Cell<HWND>,
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
}

/// A top-level window for one plugin editor. Lives on the plugin main
/// thread; dropping it destroys the window.
pub(crate) struct Window {
    shared: Rc<Shared>,
}

impl Window {
    /// Creates a hidden window titled `title`.
    ///
    /// `owner` is the native handle of the window the new one belongs to,
    /// which keeps it above that window. `position` is where its top-left
    /// corner goes; without one, or if the position is no longer on any
    /// monitor, the system chooses.
    ///
    /// `constrain` is asked, while the user drags an edge of the window,
    /// what the view makes of each size on the way. It is called from the
    /// window procedure and must cope with being called while a plugin is
    /// in the middle of something else, by returning the size unchanged.
    pub(crate) fn create(
        title: &str,
        owner: Option<isize>,
        position: Option<(i32, i32)>,
        constrain: Box<dyn Fn(Size) -> Size>,
    ) -> Result<Self, String> {
        let shared = Rc::new(Shared {
            window: Cell::new(null_mut()),
            events: Cell::default(),
            resizing: Cell::new(0),
            constrain,
        });
        let title: Vec<u16> = title.encode_utf16().chain([0]).collect();
        // SAFETY: plain calls into user32 with valid arguments: the class
        // is registered, the title is NUL-terminated and outlives the
        // call, and the creation parameter points to `shared`, which the
        // window procedure takes a reference of its own to before
        // `CreateWindowExW` returns.
        unsafe {
            let owner = owner.map_or(null_mut(), |owner| owner as HWND);
            let owner = if owner.is_null() || IsWindow(owner) != 0 {
                owner
            } else {
                log::warn!("the window a plugin editor was to belong to does not exist; opening it on its own");
                null_mut()
            };
            let (x, y) =
                position.filter(|position| is_on_a_monitor(*position)).unwrap_or((CW_USEDEFAULT, CW_USEDEFAULT));
            let window = CreateWindowExW(
                0,
                window_class(),
                title.as_ptr(),
                frame_style(false, !owner.is_null()),
                x,
                y,
                PROVISIONAL_SIZE,
                PROVISIONAL_SIZE,
                owner,
                null_mut(),
                GetModuleHandleW(null()),
                Rc::as_ptr(&shared).cast(),
            );
            if window.is_null() {
                return Err(format!("cannot create a window: {}", std::io::Error::last_os_error()));
            }
            // A dark title bar, to go with the application. Where the
            // system does not know the attribute, the bar stays light.
            let dark: i32 = 1;
            DwmSetWindowAttribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&raw const dark).cast(), 4);
        }
        Ok(Self { shared })
    }

    /// The window's native handle, for a view to attach to. Null if the
    /// system destroyed the window, which happens when its owner goes.
    pub(crate) fn handle(&self) -> *mut c_void {
        self.shared.window.get()
    }

    /// How many physical pixels a logical one is on the window's monitor.
    pub(crate) fn scale(&self) -> f64 {
        // SAFETY: the handle is this window's, or null, which yields 0.
        let dpi = unsafe { GetDpiForWindow(self.handle()) };
        if dpi == 0 { 1.0 } else { f64::from(dpi) / BASE_DPI }
    }

    /// Gives the window a frame the user can resize it by, or takes it
    /// away. The area inside the frame keeps its size.
    pub(crate) fn set_resizable(&self, resizable: bool) {
        let window = self.handle();
        // SAFETY: the handle is this window's (calls on null fail
        // harmlessly); styles are read and written as the integers they
        // are.
        unsafe {
            let style = GetWindowLongPtrW(window, GWL_STYLE) as u32;
            let wanted = if resizable { style | WS_THICKFRAME } else { style & !WS_THICKFRAME };
            if wanted != style {
                let client = self.client_size();
                SetWindowLongPtrW(window, GWL_STYLE, wanted as isize);
                self.resize(client, SWP_FRAMECHANGED);
            }
        }
    }

    /// Resizes the window so that the area inside its frame is `size`, and
    /// returns the size that area then has: the system does not let a
    /// window become larger than the desktop or narrower than its title
    /// bar's buttons.
    pub(crate) fn set_client_size(&self, size: Size) -> Size {
        self.resize(size, 0);
        self.client_size()
    }

    fn resize(&self, client: Size, flags: u32) {
        let window = self.handle();
        let (width, height) = outer_size(window, client);
        let flags = flags | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
        // SAFETY: the handle is this window's. `SetWindowPos` runs the
        // window procedure before it returns, which is why the call is
        // marked as the host's own resize.
        self.shared.resizing(|| unsafe { SetWindowPos(window, null_mut(), 0, 0, width, height, flags) });
    }

    fn client_size(&self) -> Size {
        client_size(self.handle())
    }

    /// Puts the window on screen, giving it the keyboard focus only if
    /// `activate` is set.
    pub(crate) fn show(&self, activate: bool) {
        self.raise(activate, SWP_NOZORDER);
    }

    /// Brings the window to the front, restoring it if it is minimized.
    pub(crate) fn bring_to_front(&self, activate: bool) {
        self.raise(activate, 0);
    }

    fn raise(&self, activate: bool, flags: u32) {
        if !SHOWN {
            return;
        }
        let window = self.handle();
        let flags = flags | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | if activate { 0 } else { SWP_NOACTIVATE };
        // SAFETY: the handle is this window's. `SetWindowPos` shows the
        // window where `ShowWindow` might not: the first `ShowWindow` of a
        // process obeys whoever started the process, and a tool that
        // starts programs hidden would keep the window hidden with it.
        unsafe {
            if IsIconic(window) != 0 {
                ShowWindow(window, if activate { SW_RESTORE } else { SW_SHOWNOACTIVATE });
            }
            SetWindowPos(window, HWND_TOP, 0, 0, 0, 0, flags);
            if activate {
                SetForegroundWindow(window);
            }
        }
    }

    /// The window's top-left corner in screen pixels; where it would be
    /// if it is minimized.
    pub(crate) fn position(&self) -> (i32, i32) {
        let window = self.handle();
        // SAFETY: the handle is this window's, and both structures are
        // locals the system fills in; all-zero is valid for them.
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            if IsIconic(window) != 0 {
                let mut placement =
                    WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..std::mem::zeroed() };
                GetWindowPlacement(window, &mut placement);
                rect = placement.rcNormalPosition;
            } else {
                GetWindowRect(window, &mut rect);
            }
            (rect.left, rect.top)
        }
    }

    /// Destroys the window, if it still exists.
    pub(crate) fn destroy(&self) {
        let window = self.handle();
        if !window.is_null() {
            // SAFETY: the handle is this window's and this is its thread.
            // The window procedure lets go of its reference to `shared`,
            // and clears the handle, as the window goes.
            unsafe { DestroyWindow(window) };
        }
    }

    /// What happened to the window since the last call.
    pub(crate) fn take_events(&self) -> Events {
        self.shared.events.take()
    }

    /// The area inside the window's frame as it is drawn right now, the
    /// view's own windows included.
    pub(crate) fn capture(&self) -> Option<EditorImage> {
        let window = self.handle();
        let size = self.client_size();
        if window.is_null() || size.width == 0 || size.height == 0 {
            return None;
        }
        let (width, height) = (size.width as i32, size.height as i32);
        // SAFETY: every GDI object made here is released before returning.
        // The bitmap is 32 bits per pixel and `width` by `height`, so
        // `bits` points to `width * height * 4` bytes for as long as the
        // bitmap exists, which is until after they are copied.
        unsafe {
            let screen = GetDC(null_mut());
            let memory = CreateCompatibleDC(screen);
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: rows run from the top down.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut bits: *mut c_void = null_mut();
            let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            let mut image = None;
            if !bitmap.is_null() && !bits.is_null() {
                let previous = SelectObject(memory, bitmap);
                let printed = PrintWindow(window, memory, PW_CLIENTONLY | PW_RENDERFULLCONTENT) != 0;
                GdiFlush();
                if printed {
                    let bgra =
                        std::slice::from_raw_parts(bits.cast::<u8>(), size.width as usize * size.height as usize * 4);
                    let rgba = bgra.chunks_exact(4).flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255]).collect();
                    image = Some(EditorImage { width: size.width, height: size.height, rgba });
                }
                SelectObject(memory, previous);
            }
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
            image
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// The window style of an editor's frame.
fn frame_style(resizable: bool, owned: bool) -> u32 {
    let mut style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;
    if resizable {
        style |= WS_THICKFRAME;
    }
    // A window that belongs to another has no button on the taskbar, and
    // minimizing it would leave a stub at the bottom of the screen.
    if !owned {
        style |= WS_MINIMIZEBOX;
    }
    style
}

/// Whether a window whose top-left corner is at `position` would have its
/// title bar on a monitor, where the user can reach it.
fn is_on_a_monitor((x, y): (i32, i32)) -> bool {
    // SAFETY: `MonitorFromPoint` takes the point by value.
    !unsafe { MonitorFromPoint(POINT { x: x + 16, y: y + 16 }, MONITOR_DEFAULTTONULL) }.is_null()
}

fn client_size(window: HWND) -> Size {
    // SAFETY: the rectangle is a local the system fills in; with a null
    // or dead handle the call fails and leaves it zeroed.
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        GetClientRect(window, &mut rect);
        Size::new(rect.right.max(0) as u32, rect.bottom.max(0) as u32)
    }
}

/// How much larger than the area inside it the frame of `window` is.
fn frame_extent(window: HWND) -> (i32, i32) {
    // SAFETY: styles are read as the integers they are, and the rectangle
    // is a local.
    unsafe {
        let style = GetWindowLongPtrW(window, GWL_STYLE) as u32;
        let extended = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
        let mut rect: RECT = std::mem::zeroed();
        AdjustWindowRectExForDpi(&mut rect, style, 0, extended, GetDpiForWindow(window));
        (rect.right - rect.left, rect.bottom - rect.top)
    }
}

/// The size `window` must have for the area inside its frame to be
/// `client`.
fn outer_size(window: HWND, client: Size) -> (i32, i32) {
    let (extra_width, extra_height) = frame_extent(window);
    let fit = |inner: u32, extra: i32| i32::try_from(inner).unwrap_or(i32::MAX).saturating_add(extra);
    (fit(client.width, extra_width), fit(client.height, extra_height))
}

/// The window class of editor windows, registered on first use. A failed
/// registration yields a class that does not exist, and with it a failed
/// window creation, which is reported.
fn window_class() -> *const u16 {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        // SAFETY: the class structure is fully initialized, its strings
        // and its window procedure live for the whole process, and all-zero
        // is valid for the fields left alone. An icon or cursor that does
        // not exist is null, which means "the default".
        unsafe {
            let module = GetModuleHandleW(null());
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_procedure),
                hInstance: module,
                hIcon: LoadIconW(module, APPLICATION_ICON as *const u16),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                // What shows until the view has painted, and around a view
                // that is smaller than the window.
                hbrBackground: GetStockObject(BLACK_BRUSH),
                lpszClassName: CLASS_NAME.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&class);
        }
    });
    CLASS_NAME.as_ptr()
}

/// The `Shared` of `window`, if the window has one yet and still.
///
/// # Safety
///
/// `window` must be a window of the class [`window_class`] registers,
/// which is what puts a `Shared` pointer or null into its user data.
unsafe fn shared_of(window: HWND) -> Option<Rc<Shared>> {
    // SAFETY: per this function's contract the user data is null or a
    // pointer the window holds a reference through (`adopt`), which makes
    // it valid for taking another.
    unsafe {
        let shared = GetWindowLongPtrW(window, GWLP_USERDATA) as *const Shared;
        if shared.is_null() {
            return None;
        }
        Rc::increment_strong_count(shared);
        Some(Rc::from_raw(shared))
    }
}

/// Gives the window being created a reference of its own to the `Shared`
/// it was created with.
///
/// # Safety
///
/// `lparam` must be that of the `WM_NCCREATE` message of a window created
/// by [`Window::create`].
unsafe fn adopt(window: HWND, lparam: LPARAM) {
    // SAFETY: `WM_NCCREATE` carries the creation structure, whose
    // parameter is the pointer `Window::create` passed: to a `Shared`
    // inside an `Rc` that the caller keeps alive for the whole call.
    unsafe {
        let shared = (*(lparam as *const CREATESTRUCTW)).lpCreateParams as *const Shared;
        Rc::increment_strong_count(shared);
        SetWindowLongPtrW(window, GWLP_USERDATA, shared as isize);
        (*shared).window.set(window);
    }
}

/// Takes back the reference `adopt` gave the window, as the window goes.
///
/// # Safety
///
/// As for [`shared_of`].
unsafe fn disown(window: HWND) {
    // SAFETY: the pointer is null or the one `adopt` stored, along with a
    // reference that is given up here, once: the user data is cleared.
    unsafe {
        let shared = SetWindowLongPtrW(window, GWLP_USERDATA, 0) as *const Shared;
        if !shared.is_null() {
            let shared = Rc::from_raw(shared);
            shared.window.set(null_mut());
            // Not the host's doing if it comes as news: the system
            // destroys a window along with the window that owns it.
            shared.note(|events| events.destroyed = true);
        }
    }
}

unsafe extern "system" fn window_procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // A panic must not unwind into the system's code. Nothing below is
    // expected to panic; if something does, the message gets the default
    // treatment.
    // SAFETY: the arguments are the ones the system called with.
    let handled = catch_unwind(AssertUnwindSafe(|| unsafe { handle(window, message, wparam, lparam) }));
    match handled {
        Ok(Some(result)) => result,
        // SAFETY: as above.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// Handles a message, or returns `None` to leave it to the system.
///
/// # Safety
///
/// Must be called with the arguments of a window procedure call for a
/// window of [`window_class`].
unsafe fn handle(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    // SAFETY: per this function's contract, which is also that of the
    // helpers; `lparam` is what each message documents it to be.
    unsafe {
        match message {
            WM_NCCREATE => {
                adopt(window, lparam);
                return None;
            }
            WM_NCDESTROY => {
                disown(window);
                return None;
            }
            _ => {}
        }
        let shared = shared_of(window)?;
        match message {
            // The window stays until the plugin thread has taken the view
            // out of it.
            WM_CLOSE => shared.note(|events| events.close_requested = true),
            WM_SIZE => {
                if wparam != SIZE_MINIMIZED as usize && shared.resizing.get() == 0 {
                    let size = Size::new((lparam & 0xFFFF) as u32, ((lparam >> 16) & 0xFFFF) as u32);
                    shared.note(|events| events.resized = Some(size));
                }
            }
            WM_SIZING => {
                constrain_drag(&shared, window, wparam as u32, &mut *(lparam as *mut RECT));
                return Some(1);
            }
            WM_DPICHANGED => {
                // The system proposes a position and size for the new
                // monitor. Taking them is what keeps the window under the
                // mouse that is dragging it; the view is asked afterwards
                // what size it really is at the new scale.
                let proposed = *(lparam as *const RECT);
                let (width, height) = (proposed.right - proposed.left, proposed.bottom - proposed.top);
                let flags = SWP_NOZORDER | SWP_NOACTIVATE;
                shared.resizing(|| SetWindowPos(window, null_mut(), proposed.left, proposed.top, width, height, flags));
                let scale = (wparam & 0xFFFF) as f64 / BASE_DPI;
                shared.note(|events| events.scale = Some(scale));
            }
            // The keyboard belongs to the view: a text field in it cannot
            // be typed into while the frame around it has the focus.
            WM_SETFOCUS => {
                let view = GetWindow(window, GW_CHILD);
                if !view.is_null() {
                    SetFocus(view);
                }
            }
            _ => return None,
        }
        Some(0)
    }
}

/// Adjusts the rectangle the user is dragging the window to, so that the
/// area inside the frame has a size the view accepts.
fn constrain_drag(shared: &Shared, window: HWND, edge: u32, rect: &mut RECT) {
    let (extra_width, extra_height) = frame_extent(window);
    let proposed = Size::new(
        (rect.right - rect.left - extra_width).max(1) as u32,
        (rect.bottom - rect.top - extra_height).max(1) as u32,
    );
    let accepted = (shared.constrain)(proposed);
    if accepted.width == 0 || accepted.height == 0 {
        return;
    }
    let (width, height) = outer_size(window, accepted);
    // The edge being dragged moves; the one opposite it stays put.
    if matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT) {
        rect.left = rect.right - width;
    } else {
        rect.right = rect.left + width;
    }
    if matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT) {
        rect.top = rect.bottom - height;
    } else {
        rect.bottom = rect.top + height;
    }
}

/// What a user does to a window, done to it by a test.
///
/// The tests call these from their own threads, which do not see windows
/// the way the plugin thread does: Windows scales the sizes it reports to
/// a thread that has not declared itself aware of monitor resolutions.
/// Each function therefore takes on the plugin thread's awareness for as
/// long as it runs.
#[cfg(test)]
pub(crate) mod simulate {
    use super::*;
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, SendMessageW, WMSZ_BOTTOMRIGHT};

    /// Sees sizes in physical pixels until dropped.
    struct Aware(DPI_AWARENESS_CONTEXT);

    impl Aware {
        fn enter() -> Self {
            // SAFETY: the context is one of the system's constants.
            Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
        }
    }

    impl Drop for Aware {
        fn drop(&mut self) {
            // SAFETY: the context is the one the thread had before.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }

    /// The editor window titled `title`, as an integer a test can keep.
    pub(crate) fn find(title: &str) -> Option<isize> {
        let title: Vec<u16> = title.encode_utf16().chain([0]).collect();
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let window = unsafe { FindWindowW(CLASS_NAME.as_ptr(), title.as_ptr()) };
        (!window.is_null()).then_some(window as isize)
    }

    /// Clicks the window's close button.
    pub(crate) fn close(window: isize) {
        // SAFETY: posting a message to a window, alive or not, is sound.
        unsafe { PostMessageW(window as HWND, WM_CLOSE, 0, 0) };
    }

    /// Drags the window's bottom-right corner until the area inside the
    /// frame would be `size`, and returns the size the drag is allowed to
    /// reach. The window itself is left alone, as it is until the mouse
    /// button is released.
    pub(crate) fn drag_to(window: isize, size: Size) -> Size {
        let _aware = Aware::enter();
        let window = window as HWND;
        let (width, height) = outer_size(window, size);
        let mut rect = RECT { left: 0, top: 0, right: width, bottom: height };
        // SAFETY: `WM_SIZING` carries a pointer to a rectangle, which
        // outlives the synchronous call.
        unsafe { SendMessageW(window, WM_SIZING, WMSZ_BOTTOMRIGHT as usize, (&raw mut rect) as isize) };
        let (extra_width, extra_height) = frame_extent(window);
        Size::new((rect.right - rect.left - extra_width) as u32, (rect.bottom - rect.top - extra_height) as u32)
    }

    /// Releases the mouse button with the area inside the frame at `size`.
    pub(crate) fn resize_to(window: isize, size: Size) {
        let _aware = Aware::enter();
        let window = window as HWND;
        let (width, height) = outer_size(window, size);
        let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
        // SAFETY: the window is resized as the system does it for a user:
        // without the host marking the change as its own.
        unsafe { SetWindowPos(window, null_mut(), 0, 0, width, height, flags) };
    }

    /// Moves the window to a monitor of `dpi` dots per inch.
    pub(crate) fn change_dpi(window: isize, dpi: u32) {
        let _aware = Aware::enter();
        let window = window as HWND;
        // SAFETY: both rectangles are locals; `WM_DPICHANGED` carries the
        // proposed one, which outlives the synchronous call.
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            GetWindowRect(window, &mut rect);
            SendMessageW(window, WM_DPICHANGED, ((dpi << 16) | dpi) as usize, (&raw mut rect) as isize);
        }
    }

    /// The area inside the window's frame.
    pub(crate) fn client_size(window: isize) -> Size {
        let _aware = Aware::enter();
        super::client_size(window as HWND)
    }

    /// The window's top-left corner, in screen pixels.
    pub(crate) fn position(window: isize) -> (i32, i32) {
        let _aware = Aware::enter();
        // SAFETY: the rectangle is a local the system fills in.
        unsafe {
            let mut rect: RECT = std::mem::zeroed();
            GetWindowRect(window as HWND, &mut rect);
            (rect.left, rect.top)
        }
    }

    /// Whether the user can resize the window by its frame.
    pub(crate) fn is_resizable(window: isize) -> bool {
        // SAFETY: the style is read as the integer it is.
        unsafe { GetWindowLongPtrW(window as HWND, GWL_STYLE) as u32 & WS_THICKFRAME != 0 }
    }

    /// Whether the window still exists.
    pub(crate) fn exists(window: isize) -> bool {
        // SAFETY: `IsWindow` accepts any value.
        unsafe { IsWindow(window as HWND) != 0 }
    }

    /// Whether the window is on screen.
    pub(crate) fn is_visible(window: isize) -> bool {
        // SAFETY: `IsWindowVisible` accepts any value.
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(window as HWND) != 0 }
    }
}
