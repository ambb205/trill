//! Plugin editors: what the plugin formats have in common.
//!
//! A plugin's editor is a view the plugin draws into a window the host
//! provides. The formats differ in how the host talks to the view; what
//! the host does about it is the same, and lives here:
//!
//! * [`Window`]: one native top-level window per editor, created and
//!   serviced on the plugin main thread. Its window procedure never calls
//!   a plugin by itself. It notes what happened to the window, and the
//!   plugin thread acts on that when it next idles, so that a plugin is
//!   only ever called from one well-defined place.
//! * [`PluginView`]: the calls of CLAP's `gui` extension and of VST3's
//!   `IPlugView`, reduced to what a window needs of either.
//! * [`HostedEditor`]: an open editor. It carries a view and a window
//!   through the negotiation that opening, resizing, changing monitors
//!   and closing are.
//! * [`EditorStatus`]: what a device's controller knows about its editor
//!   without asking the plugin thread, because views ask on every frame.
//!
//! Editors are hosted on Windows. Elsewhere [`Window`] cannot be created
//! and opening an editor reports that it is not supported yet.

#[cfg(windows)]
#[path = "window.rs"]
mod window;

#[cfg(not(windows))]
#[path = "unsupported.rs"]
mod window;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use trill_core::device::{EditorImage, EditorOptions};
#[cfg(all(test, windows))]
pub(crate) use window::simulate;
pub(crate) use window::{SUPPORTED, Window};

/// How long a plugin may take to open or close its editor before the
/// caller stops waiting. Generous: an editor may load its graphics, or its
/// plugin put a dialog in the way, the first time it opens.
pub(crate) const EDITOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The size an editor is given when its plugin does not say what it
/// wants, at 100 % scale.
const FALLBACK_SIZE: Size = Size { width: 640, height: 480 };

/// A size in physical pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// What happened to a [`Window`] since it was last asked.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Events {
    /// The user asked for the window to be closed.
    pub close_requested: bool,
    /// The window no longer exists, without the host having destroyed it.
    pub destroyed: bool,
    /// The area inside the window's frame was given this size, by the
    /// user or the system.
    pub resized: Option<Size>,
    /// The window is now on a monitor with this scale factor.
    pub scale: Option<f64>,
}

/// What a device's controller and the plugin thread share about the
/// device's editor: the answers a user interface wants on every frame,
/// kept where reading them costs a load instead of a round trip.
#[derive(Default)]
pub(crate) struct EditorStatus {
    available: AtomicBool,
    open: AtomicBool,
}

impl EditorStatus {
    pub(crate) fn new(available: bool) -> Arc<Self> {
        Arc::new(Self { available: AtomicBool::new(available), open: AtomicBool::new(false) })
    }

    /// Whether the plugin has an editor this host can open.
    pub(crate) fn is_available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }

    /// Corrects the answer where it only becomes known later: a VST3
    /// plugin shows whether it has an editor when it is asked for one.
    pub(crate) fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::Release);
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    fn set_open(&self, open: bool) {
        self.open.store(open, Ordering::Release);
    }
}

/// A plugin's view as the window around it sees it. All of it is called on
/// the plugin main thread.
pub(crate) trait PluginView {
    /// Tells the view how many physical pixels one of its own is, on the
    /// monitor it is shown on: 1.5 at 150 % scaling.
    fn set_scale(&mut self, scale: f64);

    /// The size the view wants to be.
    fn size(&mut self) -> Option<Size>;

    /// Whether the user may resize the view.
    fn can_resize(&mut self) -> bool;

    /// The size nearest to `size` that the view accepts.
    fn constrain(&mut self, size: Size) -> Size;

    /// Tells the view that the space it has is now `size`.
    fn set_size(&mut self, size: Size);

    /// Puts the view into `parent`.
    fn attach(&mut self, parent: &Window) -> Result<(), String>;

    /// Makes the attached view visible.
    fn show(&mut self);

    /// Takes the view out of its window and frees what it holds. Also
    /// called for a view that failed to attach.
    fn detach(&mut self);
}

/// Where an editor's window was when it was closed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Placement {
    /// The window's top-left corner, in screen pixels.
    pub position: (i32, i32),
    /// The size the user gave a resizable view.
    pub size: Option<Size>,
}

/// What opening an editor needs besides the view.
pub(crate) struct OpenRequest<'a> {
    pub options: &'a EditorOptions,
    /// The window's title if the options give none: the plugin's name.
    pub plugin_name: &'a str,
    /// Where this plugin's editor was the last time it was open.
    pub remembered: Option<Placement>,
    pub status: Arc<EditorStatus>,
    /// Asks the view what it makes of a size while the user is dragging
    /// the window's edge; see [`Window::create`].
    pub constrain: Box<dyn Fn(Size) -> Size>,
}

/// What an editor needs its owner to do after [`HostedEditor::service`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Serviced {
    /// Nothing: the editor stays open.
    Open,
    /// The user closed the window, or the window is gone: the owner must
    /// [`close`](HostedEditor::close) the editor.
    Close,
}

/// An editor that is open: a plugin's view in a window of its own.
pub(crate) struct HostedEditor {
    window: Window,
    status: Arc<EditorStatus>,
    resizable: bool,
    /// The size the view and the window last agreed on.
    size: Size,
    /// Set once the window asked to be closed, until it is.
    closing: bool,
}

impl HostedEditor {
    /// Makes a window for `view`, sized as the view wants it, puts the
    /// view into it and shows it. On failure nothing is left behind: the
    /// window is destroyed and the view detached.
    pub(crate) fn open(view: &mut dyn PluginView, request: OpenRequest<'_>) -> Result<Self, String> {
        let OpenRequest { options, plugin_name, remembered, status, constrain } = request;
        let title = if options.title.is_empty() { plugin_name } else { &options.title };
        let position = options.position.or(remembered.map(|placement| placement.position));
        let window = Window::create(title, options.owner, position, constrain)?;

        // The order is the one both formats document: scale first, since
        // every size that follows is in pixels of that scale.
        let scale = window.scale();
        view.set_scale(scale);
        let resizable = view.can_resize();
        window.set_resizable(resizable);
        let mut size = view.size().filter(|size| !size.is_empty()).unwrap_or_else(|| {
            log::warn!("[{plugin_name}] does not say how large its editor is; guessing");
            Size::new((f64::from(FALLBACK_SIZE.width) * scale) as u32, (f64::from(FALLBACK_SIZE.height) * scale) as u32)
        });
        // A view the user had resized comes back as they left it, as far
        // as the view still agrees.
        let resized = remembered.and_then(|placement| placement.size).filter(|wanted| resizable && *wanted != size);
        if let Some(wanted) = resized {
            size = view.constrain(wanted);
            view.set_size(size);
        }
        // A view that wants more room than its monitor has, and can do
        // with less, is offered what there is: a window whose edges are
        // beyond the screen cannot be reached to make it smaller.
        if resizable && let Some(fitting) = window.available_size().and_then(|room| fit_into(view, size, room)) {
            log::debug!("[{plugin_name}] its editor wants {size:?}, more than the screen has; it gets {fitting:?}");
            view.set_size(fitting);
            size = fitting;
        }
        let mut editor = Self { size: window.set_client_size(size), window, status, resizable, closing: false };

        if let Err(reason) = view.attach(&editor.window) {
            view.detach();
            return Err(reason);
        }
        // Some views only settle on their size once they are in a window,
        // and tell the host by asking for a resize while attaching, or
        // not at all. Whatever happened in between is ignored: this is
        // where the window and the view start out agreeing.
        editor.window.take_events();
        if let Some(settled) = view.size().filter(|settled| !settled.is_empty() && *settled != editor.size) {
            editor.size = editor.window.set_client_size(settled);
        }
        log::debug!("[{plugin_name}] its editor opens at {:?} (scale {scale}, resizable: {resizable})", editor.size);
        view.show();
        editor.window.show(options.activate);
        editor.status.set_open(true);
        Ok(editor)
    }

    /// Brings the window to the front, as opening an editor that is
    /// already open should.
    pub(crate) fn bring_to_front(&self, activate: bool) {
        self.window.show(activate);
    }

    /// Acts on what happened to the window since the last call.
    pub(crate) fn service(&mut self, view: &mut dyn PluginView) -> Serviced {
        let events = self.window.take_events();
        self.closing |= events.close_requested || events.destroyed;
        if self.closing {
            return Serviced::Close;
        }
        if let Some(scale) = events.scale {
            // The window moved to a monitor of another resolution. The
            // view is told, and says what size that makes it.
            view.set_scale(scale);
            if let Some(size) = view.size().filter(|size| !size.is_empty()) {
                self.size = self.window.set_client_size(size);
            }
        } else if let Some(size) = events.resized.filter(|size| *size != self.size) {
            // The user resized the window. While dragging, the window
            // already asked the view about every size; this is the final
            // word, and the window follows it if it differs.
            let accepted = if self.resizable { view.constrain(size) } else { self.size };
            view.set_size(accepted);
            self.size = if accepted == size { size } else { self.window.set_client_size(accepted) };
        }
        Serviced::Open
    }

    /// Resizes the window because the view asked for `size`. Returns the
    /// size the view is given, for the caller to tell the view if it is
    /// not what was asked: a view that can do with less is not given more
    /// room than its monitor has, and the system has limits of its own.
    pub(crate) fn resize_to(&mut self, view: &mut dyn PluginView, size: Size) -> Size {
        if size.is_empty() {
            return self.size;
        }
        let room = self.window.available_size().filter(|_| self.resizable);
        let granted = room.and_then(|room| fit_into(view, size, room)).unwrap_or(size);
        if granted != self.size {
            self.size = self.window.set_client_size(granted);
        }
        self.size
    }

    /// Lets the user resize the window, or stops them, after the view
    /// changed its mind about it.
    pub(crate) fn set_resizable(&mut self, resizable: bool) {
        if std::mem::replace(&mut self.resizable, resizable) != resizable {
            self.window.set_resizable(resizable);
        }
    }

    /// The window's contents as they are drawn right now.
    pub(crate) fn capture(&self) -> Option<EditorImage> {
        self.window.capture()
    }

    /// Detaches the view and destroys the window. Returns where the window
    /// was, to open the next one there.
    pub(crate) fn close(self, view: &mut dyn PluginView) -> Placement {
        let placement = Placement { position: self.window.position(), size: self.resizable.then_some(self.size) };
        // The view goes first: it is a child of the window, and wants to
        // take itself out of a window that still exists.
        view.detach();
        placement
        // `self` drops here, which destroys the window.
    }
}

/// How often a view is offered a smaller size before the host gives up on
/// fitting it into its monitor.
const FITTING_ROUNDS: u32 = 6;

/// The size a view makes of being given `room` instead of the larger
/// `wanted`, if it settles on one that fits. `None` if the view fits as it
/// is, or does not go along.
///
/// A view that keeps its proportions answers an offer by fixing one side
/// and working out the other, which may again be too large. So it is
/// offered sizes of its own shape, a little smaller each time.
fn fit_into(view: &mut dyn PluginView, wanted: Size, room: Size) -> Option<Size> {
    let fits = |size: Size| !size.is_empty() && size.width <= room.width && size.height <= room.height;
    if fits(wanted) {
        return None;
    }
    let mut offer = Size::new(wanted.width.min(room.width), wanted.height.min(room.height));
    for round in 0..FITTING_ROUNDS {
        let accepted = view.constrain(offer);
        if fits(accepted) {
            return (accepted != wanted).then_some(accepted);
        }
        if accepted.is_empty() {
            return None;
        }
        // The view's answer, shrunk to where its larger side fits, and by
        // a pixel more for every answer that came back rounded up.
        let scale = (f64::from(room.width) / f64::from(accepted.width))
            .min(f64::from(room.height) / f64::from(accepted.height));
        let shrunk = |length: u32| ((f64::from(length) * scale) as u32).saturating_sub(round).max(1);
        offer = Size::new(shrunk(accepted.width), shrunk(accepted.height));
    }
    None
}

impl Drop for HostedEditor {
    /// The window goes before the editor is said to be closed, so that
    /// whoever reads "closed" finds no window.
    fn drop(&mut self) {
        self.window.destroy();
        self.status.set_open(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view that answers every question about sizes with `shape`.
    struct Shaped<F: Fn(Size) -> Size> {
        shape: F,
        asked: u32,
    }

    impl<F: Fn(Size) -> Size> PluginView for Shaped<F> {
        fn set_scale(&mut self, _scale: f64) {}

        fn size(&mut self) -> Option<Size> {
            None
        }

        fn can_resize(&mut self) -> bool {
            true
        }

        fn constrain(&mut self, size: Size) -> Size {
            self.asked += 1;
            (self.shape)(size)
        }

        fn set_size(&mut self, _size: Size) {}

        fn attach(&mut self, _parent: &Window) -> Result<(), String> {
            Ok(())
        }

        fn show(&mut self) {}

        fn detach(&mut self) {}
    }

    fn fit(shape: impl Fn(Size) -> Size, wanted: Size, room: Size) -> (Option<Size>, u32) {
        let mut view = Shaped { shape, asked: 0 };
        (fit_into(&mut view, wanted, room), view.asked)
    }

    const ROOM: Size = Size { width: 1900, height: 1000 };

    #[test]
    fn a_view_that_fits_is_left_alone() {
        assert_eq!(fit(|size| size, Size::new(800, 600), ROOM), (None, 0));
        assert_eq!(fit(|size| size, ROOM, ROOM), (None, 0), "exactly the room there is");
    }

    #[test]
    fn a_view_that_takes_any_size_gets_the_room_there_is() {
        assert_eq!(fit(|size| size, Size::new(3000, 600), ROOM), (Some(Size::new(1900, 600)), 1));
        assert_eq!(fit(|size| size, Size::new(3000, 2000), ROOM), (Some(ROOM), 1));
    }

    #[test]
    fn a_view_that_keeps_its_shape_is_brought_down_to_one_that_fits() {
        // 16 by 10, worked out from the width and rounded up: the first
        // offer comes back too tall, as it does from a real plugin.
        let widescreen = |size: Size| Size::new(size.width, (size.width * 10).div_ceil(16));
        let (fitted, asked) = fit(widescreen, Size::new(3200, 2000), ROOM);
        let fitted = fitted.expect("a size that fits");
        assert!(fitted.width <= ROOM.width && fitted.height <= ROOM.height, "{fitted:?}");
        assert_eq!(fitted, widescreen(fitted), "a size the view itself accepts");
        assert!(fitted.height >= 990, "nearly all of the room is used: {fitted:?}");
        assert!(asked <= FITTING_ROUNDS);
    }

    #[test]
    fn a_view_that_will_not_be_smaller_keeps_its_size() {
        let stubborn = |_: Size| Size::new(3000, 2000);
        assert_eq!(fit(stubborn, Size::new(3000, 2000), ROOM), (None, FITTING_ROUNDS));
        // Nor is nonsense followed.
        assert_eq!(fit(|_| Size::new(0, 0), Size::new(3000, 2000), ROOM), (None, 1));
    }
}
