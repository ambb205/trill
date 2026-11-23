//! The window a plugin's editor is shown in, on Linux: an X11 window.
//!
//! Plugins on Linux draw into an X window and nothing else (CLAP's `x11`,
//! VST3's `X11EmbedWindowID`), so the window is made through X also on a
//! Wayland desktop, which runs an X server beside itself for older
//! programs (Xwayland, at `$DISPLAY`). There it is an ordinary window, which
//! goes behind Trill's own as any other window does. Where there is no X
//! server at all, opening an editor fails with a sentence that says so.
//!
//! The windows of the plugin main thread share one connection to the X
//! server ([`Display`]), made when the first one is and kept for as long as
//! the thread lives: a connection's ids are given to the next program that
//! connects once it closes, and a window that was destroyed must stay gone.
//! The thread wakes when the connection has something to say
//! ([`crate::main_thread::watch`]), and what happened to a window is read
//! when the thread next idles ([`Window::take_events`]), each window taking
//! its own: nothing here calls a plugin. A plugin's own window, a child of this one,
//! is the plugin's, drawn through the plugin's own connection.
//!
//! A window that is only there for a picture, or for this crate's tests, is
//! never on screen and never takes the keyboard: it is a window the window
//! manager leaves alone (*override-redirect*), and a test's is never mapped
//! at all, while a picture's is mapped far off every screen
//! ([`keep_out_of_sight`]).
//!
//! An X window manager resizes a window by its size hints and tells the
//! client afterwards, so the view is not asked about every size while an
//! edge is dragged, as on Windows: it is asked when the size has come, and
//! the window follows what it makes of it.

use super::{Events, Size};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use trill_core::device::EditorImage;
use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, ImageFormat, PropMode,
    StackMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Whether this platform hosts plugin editors.
pub(crate) const SUPPORTED: bool = true;

/// Whether a view is told how many of the monitor's pixels one of its own is.
pub(crate) const TELLS_SCALE: bool = true;

/// The class an editor's window is known by.
const CLASS: &[u8] = b"trill-plugin-editor\0Trill\0";
/// What a window is created with, before its view has said what it wants.
const PROVISIONAL_SIZE: u16 = 320;
/// Where a picture's window is put: far up and to the left of any screen,
/// within the 16 bits X has for it.
const FAR_OFF: i16 = -20_000;

/// Set where editor windows are only there for pictures ([`keep_out_of_sight`]).
static OUT_OF_SIGHT: AtomicBool = AtomicBool::new(false);

/// Has every editor window from now on be one that is never seen and never
/// takes the keyboard: for a program that only takes pictures.
pub(crate) fn keep_out_of_sight() {
    OUT_OF_SIGHT.store(true, Ordering::Relaxed);
}

/// Whether windows are never mapped: in this crate's own unit tests, where
/// the windows are real, and are made, resized and destroyed like any
/// other, but nobody running the tests sees one.
const NEVER_MAPPED: bool = cfg!(test);

/// The atoms a window uses, asked for once.
#[derive(Copy, Clone)]
struct Atoms {
    wm_protocols: u32,
    wm_delete_window: u32,
    net_wm_name: u32,
    utf8_string: u32,
    net_wm_user_time: u32,
    net_active_window: u32,
    net_workarea: u32,
    net_frame_extents: u32,
}

impl Atoms {
    fn of(conn: &RustConnection) -> Result<Self, String> {
        let atom = |name: &str| -> Result<u32, String> {
            let reply = conn.intern_atom(false, name.as_bytes()).map_err(|e| e.to_string())?;
            Ok(reply.reply().map_err(|e| e.to_string())?.atom)
        };
        Ok(Self {
            wm_protocols: atom("WM_PROTOCOLS")?,
            wm_delete_window: atom("WM_DELETE_WINDOW")?,
            net_wm_name: atom("_NET_WM_NAME")?,
            utf8_string: atom("UTF8_STRING")?,
            net_wm_user_time: atom("_NET_WM_USER_TIME")?,
            net_active_window: atom("_NET_ACTIVE_WINDOW")?,
            net_workarea: atom("_NET_WORKAREA")?,
            net_frame_extents: atom("_NET_FRAME_EXTENTS")?,
        })
    }
}

/// The plugin main thread's connection to the X server, which its editor
/// windows share, with what happened to each window that it has not yet
/// taken.
struct Display {
    conn: RustConnection,
    root: u32,
    black: u32,
    atoms: Atoms,
    pending: RefCell<HashMap<u32, Events>>,
    /// The windows of this connection that are there: what is read of
    /// another is thrown away.
    windows: RefCell<HashSet<u32>>,
    broken: Cell<bool>,
}

thread_local! {
    static DISPLAY: RefCell<Option<Rc<Display>>> = const { RefCell::new(None) };
}

impl Display {
    /// The calling thread's connection, made where it has none or the one
    /// it had broke.
    fn get() -> Result<Rc<Self>, String> {
        if let Some(display) = DISPLAY.with_borrow(|display| display.clone()).filter(|display| !display.broken.get()) {
            return Ok(display);
        }
        let (conn, screen) = x11rb::connect(None).map_err(|error| {
            format!(
                "a plugin's editor is shown through an X server, and none could be reached ({error}). A Wayland \
                 desktop has one beside it for older programs (Xwayland), where it is set up"
            )
        })?;
        let (root, black) = (conn.setup().roots[screen].root, conn.setup().roots[screen].black_pixel);
        let atoms = Atoms::of(&conn)?;
        let fd = conn.stream().as_raw_fd();
        let display = Rc::new(Self {
            conn,
            root,
            black,
            atoms,
            pending: RefCell::default(),
            windows: RefCell::default(),
            broken: Cell::new(false),
        });
        // Read whenever it has something, whether a window is open or not:
        // what is not read leaves it ready, and the thread turning.
        let weak = Rc::downgrade(&display);
        crate::main_thread::watch(fd, Rc::new(move || weak.upgrade().is_some_and(|display| display.read())));
        DISPLAY.set(Some(display.clone()));
        Ok(display)
    }

    /// Sorts what the server said since it was last asked out to the windows
    /// it is about, throwing away what is of no window that is there, and
    /// what is of none (a keyboard's new mapping, which every client is
    /// told). Says whether there was news for a window.
    fn read(&self) -> bool {
        let mut pending = self.pending.borrow_mut();
        let windows = self.windows.borrow();
        let mut news = false;
        loop {
            let (window, said) = match self.conn.poll_for_event() {
                Ok(Some(Event::ConfigureNotify(configured))) => {
                    let size = Size::new(u32::from(configured.width), u32::from(configured.height));
                    (configured.window, Said::Resized(size))
                }
                Ok(Some(Event::ClientMessage(message)))
                    if message.type_ == self.atoms.wm_protocols
                        && message.data.as_data32()[0] == self.atoms.wm_delete_window =>
                {
                    (message.window, Said::Close)
                }
                Ok(Some(Event::DestroyNotify(gone))) => (gone.window, Said::Gone),
                Ok(Some(_)) => {
                    #[cfg(test)]
                    THROWN.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                Ok(None) => break,
                // A connection that broke is every window gone.
                Err(_) => {
                    self.broken.set(true);
                    crate::main_thread::unwatch(self.conn.stream().as_raw_fd());
                    // Every window there is learns it at its next look.
                    news |= !windows.is_empty();
                    break;
                }
            };
            if !windows.contains(&window) {
                #[cfg(test)]
                THROWN.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            news = true;
            let events = pending.entry(window).or_default();
            match said {
                Said::Resized(size) => events.resized = Some(size),
                Said::Close => events.close_requested = true,
                Said::Gone => events.destroyed = true,
            }
        }
        news
    }
}

/// How many things the server said that were of no window that is there,
/// read and thrown away: for tests.
#[cfg(test)]
pub(crate) static THROWN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What the server said of a window.
enum Said {
    Resized(Size),
    Close,
    Gone,
}

/// A top-level window for one plugin editor. Lives on the plugin main
/// thread; dropping it destroys the window.
pub(crate) struct Window {
    display: Rc<Display>,
    id: u32,
    /// Left alone by the window manager: a picture's or a test's.
    unmanaged: bool,
    /// The size the window was last given, by the host or the user.
    size: Cell<Size>,
    resizable: Cell<bool>,
    events: Cell<Events>,
    mapped: Cell<bool>,
    alive: Cell<bool>,
}

impl Window {
    /// Creates a hidden window titled `title`.
    ///
    /// `owner` is the X window of the window the new one belongs to, where
    /// there is one: Trill's own window is a Wayland window on a Wayland
    /// desktop, and an editor's window is then a window of its own.
    /// `position` is where its top-left corner goes; without one, the window
    /// manager chooses. `constrain` is what Windows asks while an edge is
    /// dragged; an X window manager does not ask (the module says why).
    pub(crate) fn create(
        title: &str,
        owner: Option<isize>,
        position: Option<(i32, i32)>,
        constrain: Box<dyn Fn(Size) -> Size>,
    ) -> Result<Self, String> {
        drop(constrain);
        let display = Display::get()?;
        let failed = |error: &dyn std::fmt::Display| format!("cannot create a window: {error}");
        let conn = &display.conn;
        let id = conn.generate_id().map_err(|e| failed(&e))?;
        let out_of_sight = OUT_OF_SIGHT.load(Ordering::Relaxed);
        let unmanaged = NEVER_MAPPED || out_of_sight;
        let (x, y) = if out_of_sight {
            (FAR_OFF, FAR_OFF)
        } else {
            let clamp = |value: i32| value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            position.map_or((0, 0), |(x, y)| (clamp(x), clamp(y)))
        };
        let aux = CreateWindowAux::new()
            .background_pixel(display.black)
            .override_redirect(u32::from(unmanaged))
            .event_mask(EventMask::STRUCTURE_NOTIFY);
        let size = PROVISIONAL_SIZE;
        conn.create_window(0, id, display.root, x, y, size, size, 0, WindowClass::INPUT_OUTPUT, 0, &aux)
            .map_err(|e| failed(&e))?;
        display.windows.borrow_mut().insert(id);
        let window = Self {
            display: display.clone(),
            id,
            unmanaged,
            size: Cell::new(Size::new(u32::from(size), u32::from(size))),
            resizable: Cell::new(true),
            events: Cell::default(),
            mapped: Cell::new(false),
            alive: Cell::new(true),
        };
        window.describe(title, owner, position.filter(|_| !out_of_sight)).map_err(|e| failed(&e))?;
        // A round trip: the server has made the window before the plugin's
        // own connection is given its id, or that connection may name a
        // window the server does not know yet, and its library end the
        // process for it.
        window.round_trip().map_err(|e| failed(&e))?;
        Ok(window)
    }

    /// Says what the window is to the window manager: its title, its class,
    /// that it is to be asked before it is closed, where it was put, whose
    /// it is.
    fn describe(
        &self,
        title: &str,
        owner: Option<isize>,
        position: Option<(i32, i32)>,
    ) -> Result<(), x11rb::errors::ConnectionError> {
        let Atoms { wm_protocols, wm_delete_window, net_wm_name, utf8_string, .. } = self.display.atoms;
        let replace = PropMode::REPLACE;
        // The title in Latin-1 where it is that, and in UTF-8 for whoever reads that.
        let latin: Vec<u8> = title.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect();
        self.display.conn.change_property8(replace, self.id, AtomEnum::WM_NAME, AtomEnum::STRING, &latin)?;
        self.display.conn.change_property8(replace, self.id, net_wm_name, utf8_string, title.as_bytes())?;
        self.display.conn.change_property8(replace, self.id, AtomEnum::WM_CLASS, AtomEnum::STRING, CLASS)?;
        self.display.conn.change_property32(replace, self.id, wm_protocols, AtomEnum::ATOM, &[wm_delete_window])?;
        if let Some(owner) = owner.and_then(|owner| u32::try_from(owner).ok()).filter(|owner| *owner != 0)
            && self.display.conn.get_geometry(owner)?.reply().is_ok()
        {
            self.display.conn.change_property32(
                replace,
                self.id,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                &[owner],
            )?;
        }
        self.hint(position)?;
        Ok(())
    }

    /// The size hints: where the window was put, and, for one that cannot
    /// be resized, that its size is its least and its most.
    fn hint(&self, position: Option<(i32, i32)>) -> Result<(), x11rb::errors::ConnectionError> {
        let mut hints = WmSizeHints::new();
        if let Some((x, y)) = position {
            hints.position = Some((x11rb::properties::WmSizeHintsSpecification::UserSpecified, x, y));
        }
        if !self.resizable.get() {
            let size = self.size.get();
            let (width, height) = (size.width as i32, size.height as i32);
            hints.min_size = Some((width, height));
            hints.max_size = Some((width, height));
        }
        hints.set_normal_hints(&&self.display.conn, self.id)?;
        Ok(())
    }

    fn round_trip(&self) -> Result<(), x11rb::errors::ReplyError> {
        self.display.conn.get_input_focus()?.reply().map(|_| ())
    }

    /// The window's id, for a view to be put into. Null once the window is
    /// gone.
    pub(crate) fn handle(&self) -> *mut c_void {
        if self.alive.get() { self.id as usize as *mut c_void } else { std::ptr::null_mut() }
    }

    /// How many physical pixels a logical one is. 1 on X: a plugin reads
    /// the display's resolution itself (Surge XT, told 2 on a display of
    /// 192 dpi, made its editor twice the size it had chosen for that
    /// display already).
    pub(crate) fn scale(&self) -> f64 {
        1.0
    }

    /// Lets the user resize the window, or stops them. The area inside the
    /// frame keeps its size.
    pub(crate) fn set_resizable(&self, resizable: bool) {
        self.resizable.set(resizable);
        let _ = self.hint(None);
        let _ = self.round_trip();
    }

    /// Gives the area inside the window's frame `size`, and returns the
    /// size it then has.
    pub(crate) fn set_client_size(&self, size: Size) -> Size {
        let clamp = |length: u32| length.clamp(1, u32::from(u16::MAX));
        let size = Size::new(clamp(size.width), clamp(size.height));
        // What the host did itself is no news from the window.
        self.size.set(size);
        let aux = ConfigureWindowAux::new().width(size.width).height(size.height);
        let _ = self.display.conn.configure_window(self.id, &aux);
        if !self.resizable.get() {
            let _ = self.hint(None);
        }
        let _ = self.round_trip();
        size
    }

    /// Puts the window on screen, in front, taking the keyboard only with
    /// `activate`. A test's window is never put on screen, and a picture's
    /// is put far off it.
    pub(crate) fn show(&self, activate: bool) {
        if NEVER_MAPPED || !self.alive.get() {
            return;
        }
        if !self.mapped.get() {
            if !activate && !self.unmanaged {
                // A time of 0 asks the window manager not to give it the keyboard.
                let user_time = self.display.atoms.net_wm_user_time;
                let _ = self.display.conn.change_property32(
                    PropMode::REPLACE,
                    self.id,
                    user_time,
                    AtomEnum::CARDINAL,
                    &[0],
                );
            }
            let _ = self.display.conn.map_window(self.id);
            self.mapped.set(true);
        } else {
            let _ =
                self.display.conn.configure_window(self.id, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        }
        if activate && !self.unmanaged {
            // Asked of the window manager as a program asks it (source 1).
            let event = ClientMessageEvent::new(32, self.id, self.display.atoms.net_active_window, [1u32, 0, 0, 0, 0]);
            let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
            let _ = self.display.conn.send_event(false, self.display.root, mask, event);
        }
        let _ = self.round_trip();
    }

    /// The largest area the window can have: the part of the screen that is
    /// free for windows, as the window manager says it, or the screen.
    pub(crate) fn available_size(&self) -> Option<Size> {
        let area = self
            .display
            .conn
            .get_property(false, self.display.root, self.display.atoms.net_workarea, AtomEnum::CARDINAL, 0, 4)
            .ok()?;
        if let Some(values) =
            area.reply().ok().and_then(|reply| reply.value32().map(|values| values.collect::<Vec<_>>()))
            && values.len() == 4
            && values[2] > 0
            && values[3] > 0
        {
            return Some(Size::new(values[2], values[3]));
        }
        let geometry = self.display.conn.get_geometry(self.display.root).ok()?.reply().ok()?;
        Some(Size::new(u32::from(geometry.width), u32::from(geometry.height)))
    }

    /// The window's top-left corner on the screen, frame included where a
    /// window manager gave it one.
    pub(crate) fn position(&self) -> (i32, i32) {
        let Some(at) =
            self.display.conn.translate_coordinates(self.id, self.display.root, 0, 0).ok().and_then(|c| c.reply().ok())
        else {
            return (0, 0);
        };
        let (mut x, mut y) = (i32::from(at.dst_x), i32::from(at.dst_y));
        if self.mapped.get() && !self.unmanaged {
            let extents = self.display.atoms.net_frame_extents;
            if let Some(values) =
                (self.display.conn.get_property(false, self.id, extents, AtomEnum::CARDINAL, 0, 4).ok())
                    .and_then(|cookie| cookie.reply().ok())
                    .and_then(|reply| reply.value32().map(|values| values.collect::<Vec<_>>()))
                    .filter(|values| values.len() == 4)
            {
                x -= values[0] as i32;
                y -= values[2] as i32;
            }
        }
        (x, y)
    }

    /// Destroys the window, if it still exists.
    pub(crate) fn destroy(&self) {
        // Out of the connection's list first: what the server says of it
        // from now on (its own going) is thrown away.
        self.display.windows.borrow_mut().remove(&self.id);
        self.display.pending.borrow_mut().remove(&self.id);
        if self.alive.replace(false) {
            let _ = self.display.conn.destroy_window(self.id);
            let _ = self.round_trip();
        }
    }

    /// What happened to the window since the last call.
    pub(crate) fn take_events(&self) -> Events {
        let _ = self.display.read();
        let mut events = self.events.take();
        let mut said = self.display.pending.borrow_mut().remove(&self.id).unwrap_or_default();
        // A size the host gave the window itself is no news from it.
        if let Some(size) = said.resized.take().filter(|size| *size != self.size.get()) {
            self.size.set(size);
            events.resized = Some(size);
        }
        events.close_requested |= said.close_requested;
        if said.destroyed || self.display.broken.get() {
            self.alive.set(false);
            events.destroyed = true;
        }
        events
    }

    /// The area inside the window as it is drawn right now, the view's own
    /// windows included. Nothing for a window that is not mapped.
    ///
    /// The server is asked for the picture of the window itself, which for a
    /// window far off the screen (a picture's) is there only where the server
    /// keeps each window in memory of its own: Xwayland under a Wayland
    /// compositor does (this was measured there), and so does an X server
    /// under a compositing window manager. A plain X server without one keeps
    /// only what is on the screen, and gives nothing usable for a window off
    /// it.
    pub(crate) fn capture(&self) -> Option<EditorImage> {
        if !self.mapped.get() || !self.alive.get() {
            return None;
        }
        let size = self.size.get();
        let (width, height) = (u16::try_from(size.width).ok()?, u16::try_from(size.height).ok()?);
        let image =
            self.display.conn.get_image(ImageFormat::Z_PIXMAP, self.id, 0, 0, width, height, !0).ok()?.reply().ok()?;
        // Four bytes a pixel, blue first, as a screen of 24 or 32 bits has it.
        if !matches!(image.depth, 24 | 32) || image.data.len() < size.width as usize * size.height as usize * 4 {
            return None;
        }
        let rgba = image.data.chunks_exact(4).flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255]).collect();
        Some(EditorImage { width: size.width, height: size.height, rgba })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// What a user does to an editor's window, done by a test without anyone
/// at the machine: requests and events sent to the window over a
/// connection of the test's own, never the pointer or the keyboard.
#[cfg(test)]
pub(crate) mod simulate {
    use super::*;

    thread_local! {
        static CONNECTION: (RustConnection, u32) = {
            let (conn, screen) = x11rb::connect(None).expect("an X server for the editor tests");
            let root = conn.setup().roots[screen].root;
            (conn, root)
        };
    }

    /// Has the server tell every client that a keyboard's mapping changed,
    /// changing nothing: one key given the mapping it has.
    pub(crate) fn tell_every_client_of_a_mapping() {
        use x11rb::protocol::xproto::ConnectionExt as _;
        with(|conn, _| {
            let first = conn.setup().min_keycode;
            let mapping = conn.get_keyboard_mapping(first, 1).expect("asked").reply().expect("the mapping");
            let per = mapping.keysyms_per_keycode;
            conn.change_keyboard_mapping(1, first, per, &mapping.keysyms).expect("asked");
            let _ = conn.get_input_focus().map(|cookie| cookie.reply());
        });
    }

    /// Whether an X server can be reached here: the editor tests are about
    /// windows, and a machine without one has none to give. They say so and
    /// pass over what they would have held.
    pub(crate) fn there_is_an_x_server() -> bool {
        let there = x11rb::connect(None).is_ok();
        if !there {
            eprintln!("skipping: no X server here (DISPLAY), and editor windows are X windows on Linux");
        }
        there
    }

    fn with<R>(job: impl FnOnce(&RustConnection, u32) -> R) -> R {
        CONNECTION.with(|(conn, root)| job(conn, *root))
    }

    fn text(conn: &RustConnection, window: u32, property: AtomEnum) -> Vec<u8> {
        (conn.get_property(false, window, property, AtomEnum::STRING, 0, 1024).ok())
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.value)
            .unwrap_or_default()
    }

    /// The editor window titled `title`, as an integer a test can keep.
    pub(crate) fn find(title: &str) -> Option<isize> {
        with(|conn, root| {
            let tree = conn.query_tree(root).ok()?.reply().ok()?;
            tree.children.into_iter().find_map(|window| {
                let ours = text(conn, window, AtomEnum::WM_CLASS) == CLASS;
                (ours && text(conn, window, AtomEnum::WM_NAME) == title.as_bytes()).then_some(window as isize)
            })
        })
    }

    /// Closes the window as a window manager does when its close button is
    /// clicked: by asking the program.
    pub(crate) fn close(window: isize) {
        with(|conn, _| {
            let atoms = Atoms::of(conn).expect("atoms");
            let event =
                ClientMessageEvent::new(32, window as u32, atoms.wm_protocols, [atoms.wm_delete_window, 0, 0, 0, 0]);
            let _ = conn.send_event(false, window as u32, EventMask::NO_EVENT, event);
            let _ = conn.get_input_focus().map(|cookie| cookie.reply());
        });
    }

    /// Resizes the window as a window manager does when the user lets go of
    /// its edge.
    pub(crate) fn resize_to(window: isize, size: Size) {
        with(|conn, _| {
            let aux = ConfigureWindowAux::new().width(size.width).height(size.height);
            let _ = conn.configure_window(window as u32, &aux);
            let _ = conn.get_input_focus().map(|cookie| cookie.reply());
        });
    }

    /// The area inside the window's frame.
    pub(crate) fn client_size(window: isize) -> Size {
        with(|conn, _| {
            let geometry = conn.get_geometry(window as u32).ok().and_then(|cookie| cookie.reply().ok());
            geometry.map_or(Size::new(0, 0), |g| Size::new(u32::from(g.width), u32::from(g.height)))
        })
    }

    /// The window's top-left corner on the screen.
    pub(crate) fn position(window: isize) -> (i32, i32) {
        with(|conn, root| {
            let at = conn.translate_coordinates(window as u32, root, 0, 0).ok().and_then(|cookie| cookie.reply().ok());
            at.map_or((0, 0), |at| (i32::from(at.dst_x), i32::from(at.dst_y)))
        })
    }

    /// Whether the user can resize the window by its frame: not where its
    /// size hints make its least size its most.
    pub(crate) fn is_resizable(window: isize) -> bool {
        let hints = |conn: &RustConnection| WmSizeHints::get_normal_hints(conn, window as u32).ok()?.reply().ok()?;
        with(|conn, _| match hints(conn) {
            Some(hints) => !(hints.min_size.is_some() && hints.min_size == hints.max_size),
            None => true,
        })
    }

    /// Whether the window still exists.
    pub(crate) fn exists(window: isize) -> bool {
        with(|conn, _| conn.get_geometry(window as u32).ok().is_some_and(|cookie| cookie.reply().is_ok()))
    }

    /// Whether the window is on screen.
    pub(crate) fn is_visible(window: isize) -> bool {
        with(|conn, _| {
            let attributes = conn.get_window_attributes(window as u32).ok().and_then(|cookie| cookie.reply().ok());
            attributes.is_some_and(|attributes| attributes.map_state == x11rb::protocol::xproto::MapState::VIEWABLE)
        })
    }
}
