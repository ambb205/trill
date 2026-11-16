//! The window a plugin's editor would be shown in, on platforms where
//! editors are not hosted yet. No such window can be made, so opening an
//! editor fails with a reason and everything else is never reached.
//!
//! What each platform needs:
//!
//! * Linux: an X11 window to embed the view in (`GuiApiType::X11`,
//!   `kPlatformTypeX11EmbedWindowID`) with XEmbed, and the plugin thread
//!   serving the file descriptors and timers plugins register with the
//!   host (CLAP `posix-fd-support`, VST3 `IRunLoop`), since there is no
//!   shared event loop for plugins to hook into.
//! * macOS: an `NSView` in an `NSWindow`, and for both to be created and
//!   used on the process's first thread. AppKit accepts no other, so the
//!   application has to lend the plugin host its main thread instead of
//!   the host running one of its own.

use super::{Events, Size};
use std::convert::Infallible;
use std::ffi::c_void;
use trill_core::device::EditorImage;

/// Whether this platform hosts plugin editors.
pub(crate) const SUPPORTED: bool = false;

/// Never exists: see the module documentation.
pub(crate) struct Window(Infallible);

impl Window {
    pub(crate) fn create(
        _title: &str,
        _owner: Option<isize>,
        _position: Option<(i32, i32)>,
        _constrain: Box<dyn Fn(Size) -> Size>,
    ) -> Result<Self, String> {
        Err("plugin editors are not supported on this platform yet".to_string())
    }

    pub(crate) fn handle(&self) -> *mut c_void {
        match self.0 {}
    }

    pub(crate) fn scale(&self) -> f64 {
        match self.0 {}
    }

    pub(crate) fn set_resizable(&self, _resizable: bool) {
        match self.0 {}
    }

    pub(crate) fn set_client_size(&self, _size: Size) -> Size {
        match self.0 {}
    }

    pub(crate) fn show(&self, _activate: bool) {
        match self.0 {}
    }

    pub(crate) fn available_size(&self) -> Option<Size> {
        match self.0 {}
    }

    pub(crate) fn position(&self) -> (i32, i32) {
        match self.0 {}
    }

    pub(crate) fn destroy(&self) {
        match self.0 {}
    }

    pub(crate) fn take_events(&self) -> Events {
        match self.0 {}
    }

    pub(crate) fn capture(&self) -> Option<EditorImage> {
        match self.0 {}
    }
}
