//! Plugin editors: CLAP's `gui` extension, as the host uses it.
//!
//! An editor is opened embedded: the plugin puts its view into a window
//! the host makes (see [`crate::editor`]), which is what lets the DAW
//! title, place, stack and close it like its own. Plugins that can only
//! float a window of their own are reported as having no editor.
//!
//! Everything here runs on the plugin main thread. Opening and closing
//! are jobs; what happens in between (the user resizing or closing the
//! window, the plugin asking for another size) is picked up when the
//! thread idles.

use super::host::{HostShared, TrillHost};
use super::instance::{ClapThread, Instance, InstanceId};
use crate::editor::{HostedEditor, OpenRequest, PluginView, SUPPORTED, Serviced, Size, Window};
use crate::main_thread::with_current;
use crate::plugin_thread::PluginThread;
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window as ClapWindow};
use clack_host::prelude::*;
use trill_core::device::{EditorError, EditorImage, EditorOptions};

/// How this host embeds an editor: in a window of the platform's own
/// windowing API. `None` on a platform without one that CLAP knows.
fn embedded() -> Option<GuiConfiguration<'static>> {
    GuiApiType::default_for_current_platform().map(|api_type| GuiConfiguration { api_type, is_floating: false })
}

/// Whether the plugin's editor can be put into a window of the host's.
pub(super) fn is_embeddable(gui: PluginGui, plugin: &PluginMainThreadHandle) -> bool {
    embedded().is_some_and(|configuration| gui.is_api_supported(plugin, configuration))
}

fn size_of(size: GuiSize) -> Size {
    Size::new(size.width, size.height)
}

fn gui_size(size: Size) -> GuiSize {
    GuiSize { width: size.width, height: size.height }
}

/// A plugin's editor, once the plugin has created it.
struct View<'a> {
    gui: PluginGui,
    plugin: PluginMainThreadHandle<'a>,
    plugin_id: &'a str,
}

impl<'a> View<'a> {
    fn new(gui: PluginGui, plugin: &'a mut PluginInstance<TrillHost>, plugin_id: &'a str) -> Self {
        Self { gui, plugin: plugin.plugin_handle(), plugin_id }
    }
}

impl PluginView for View<'_> {
    fn set_scale(&mut self, scale: f64) {
        // A plugin that works the scale out for itself says no, which is
        // as good an answer as yes.
        if self.gui.set_scale(&self.plugin, scale).is_err() {
            log::debug!("[{}] does not take its editor's scale from the host", self.plugin_id);
        }
    }

    fn size(&mut self) -> Option<Size> {
        self.gui.get_size(&self.plugin).map(size_of)
    }

    fn can_resize(&mut self) -> bool {
        self.gui.can_resize(&self.plugin)
    }

    fn constrain(&mut self, size: Size) -> Size {
        self.gui.adjust_size(&self.plugin, gui_size(size)).map_or(size, size_of)
    }

    fn set_size(&mut self, size: Size) {
        if self.gui.set_size(&self.plugin, gui_size(size)).is_err() {
            log::debug!("[{}] did not take the size {size:?} for its editor", self.plugin_id);
        }
    }

    fn attach(&mut self, parent: &Window) -> Result<(), String> {
        let api = embedded().ok_or("this platform has no window to embed an editor in")?.api_type;
        let window = ClapWindow::from_generic_ptr(api, parent.handle());
        // SAFETY: the handle is that of a live window of this thread, and
        // stays valid for as long as the view is attached: an editor
        // detaches its view (`gui.destroy`) before it destroys its window.
        unsafe { self.gui.set_parent(&self.plugin, window) }
            .map_err(|_| "the plugin could not put its editor into a window".to_string())
    }

    fn show(&mut self) {
        if self.gui.show(&self.plugin).is_err() {
            log::warn!("[{}] reported a failure showing its editor", self.plugin_id);
        }
    }

    fn detach(&mut self) {
        // Hiding first is not required, and plugins that have nothing to
        // hide say so; the answer does not matter.
        let _ = self.gui.hide(&self.plugin);
        self.gui.destroy(&self.plugin);
    }
}

impl Instance {
    fn open_editor(&mut self, id: InstanceId, options: &EditorOptions) -> Result<(), EditorError> {
        if let Some(editor) = &self.editor {
            editor.bring_to_front(options.activate);
            return Ok(());
        }
        let gui = self.gui.filter(|_| self.editor_status.is_available()).ok_or(EditorError::NoEditor)?;
        let configuration = embedded().filter(|_| SUPPORTED).ok_or(EditorError::Unsupported)?;

        gui.create(&self.plugin.plugin_handle(), configuration)
            .map_err(|_| EditorError::Failed("the plugin could not create its editor".to_string()))?;
        // From here on the plugin may ask for its window to be resized.
        self.plugin.access_shared_handler(|shared| shared.set_editor_open(true));
        let request = OpenRequest {
            options,
            plugin_name: &self.name,
            remembered: self.placement,
            status: self.editor_status.clone(),
            // Called by the window while the user drags its edge. If the
            // plugin thread is in the middle of something, the size goes
            // through as it is and is settled when the drag has ended.
            constrain: Box::new(move |size| {
                with_current(|thread: &mut PluginThread| thread.clap.constrain_editor(id, size))
                    .flatten()
                    .unwrap_or(size)
            }),
        };
        match HostedEditor::open(&mut View::new(gui, &mut self.plugin, &self.plugin_id), request) {
            Ok(editor) => {
                self.editor = Some(editor);
                self.editor_closing = false;
                Ok(())
            }
            Err(reason) => {
                self.plugin.access_shared_handler(|shared| shared.set_editor_open(false));
                Err(EditorError::Failed(reason))
            }
        }
    }

    /// Closes the editor if it is open: the plugin destroys its view, and
    /// the window goes.
    pub(super) fn close_editor(&mut self) {
        self.editor_closing = false;
        let (Some(editor), Some(gui)) = (self.editor.take(), self.gui) else { return };
        self.placement = Some(editor.close(&mut View::new(gui, &mut self.plugin, &self.plugin_id)));
        self.plugin.access_shared_handler(|shared| shared.set_editor_open(false));
    }

    /// Acts on what the user did to the editor's window and what the
    /// plugin asked about it since the last call.
    pub(super) fn service_editor(&mut self, nested: bool) {
        let (Some(editor), Some(gui)) = (self.editor.as_mut(), self.gui) else { return };
        let requests = self.plugin.access_shared_handler(HostShared::take_editor_requests);
        let mut view = View::new(gui, &mut self.plugin, &self.plugin_id);
        if requests.hints_changed {
            editor.set_resizable(view.can_resize());
        }
        if let Some(size) = requests.resize {
            log::debug!("[{}] its editor asks to be {size:?}", self.plugin_id);
            // A plugin that got what it asked for needs no telling.
            let given = editor.resize_to(&mut view, size);
            if given != size {
                view.set_size(given);
            }
        }
        self.editor_closing |= requests.lost || editor.service(&mut view) == Serviced::Close;
        // Closing takes the view away from the plugin, which waits until
        // nothing of the plugin's can be further up the stack.
        if self.editor_closing && !nested {
            self.close_editor();
        }
    }
}

impl ClapThread {
    /// Opens the editor of an instance, or brings it to the front if it
    /// is open already.
    pub(crate) fn open_editor(&mut self, id: InstanceId, options: &EditorOptions) -> Result<(), EditorError> {
        let gone = || EditorError::Failed("the plugin instance no longer exists".to_string());
        let opened = self.instances.get_mut(&id).ok_or_else(gone)?.open_editor(id, options);
        // An editor may have set its own handler of X errors as it opened.
        #[cfg(target_os = "linux")]
        crate::x_errors::hold();
        opened
    }

    pub(crate) fn close_editor(&mut self, id: InstanceId) {
        if let Some(instance) = self.instances.get_mut(&id) {
            instance.close_editor();
        }
    }

    /// The open editor of an instance, as it is drawn right now.
    pub(crate) fn capture_editor(&mut self, id: InstanceId) -> Option<EditorImage> {
        self.instances.get(&id)?.editor.as_ref()?.capture()
    }

    /// The size nearest to `size` that the open editor of an instance
    /// accepts.
    fn constrain_editor(&mut self, id: InstanceId, size: Size) -> Option<Size> {
        let instance = self.instances.get_mut(&id)?;
        instance.editor.as_ref()?;
        Some(View::new(instance.gui?, &mut instance.plugin, &instance.plugin_id).constrain(size))
    }
}
