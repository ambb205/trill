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
        if editor.size != size {
            log::debug!("[{plugin_name}] its editor is {:?} instead of the {size:?} it asked for", editor.size);
        }
        view.show();
        editor.window.show(options.activate);
        editor.status.set_open(true);
        Ok(editor)
    }

    /// Brings the window to the front, as opening an editor that is
    /// already open should.
    pub(crate) fn bring_to_front(&self, activate: bool) {
        self.window.bring_to_front(activate);
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
    /// size the window now has, which the system may have limited.
    pub(crate) fn resize_to(&mut self, size: Size) -> Size {
        if !size.is_empty() && size != self.size {
            self.size = self.window.set_client_size(size);
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

impl Drop for HostedEditor {
    /// The window goes before the editor is said to be closed, so that
    /// whoever reads "closed" finds no window.
    fn drop(&mut self) {
        self.window.destroy();
        self.status.set_open(false);
    }
}
