//! Plugin editors: VST3's `IPlugView`, as the host uses it.
//!
//! A plugin's edit controller makes a view on request; the host puts it
//! into a window of its own (see [`crate::editor`]) and gives it an
//! `IPlugFrame` to ask for another size through. VST3 has no way to ask
//! whether a plugin has an editor short of making the view, so a plugin
//! with an edit controller is taken to have one until an attempt to open
//! it finds none.
//!
//! Everything here runs on the plugin main thread. Opening and closing
//! are jobs; what happens in between (the user resizing or closing the
//! window, the view asking for another size) is picked up when the
//! thread idles, or at once where the thread is free.

use super::com::{interface, result_name};
use super::host::PlugFrame;
use super::instance::{Instance, InstanceId, Vst3Thread};
use crate::editor::{HostedEditor, OpenRequest, PluginView, SUPPORTED, Serviced, Size, Window};
use crate::main_thread::with_current;
use crate::plugin_thread::PluginThread;
use std::ptr::null_mut;
use trill_core::device::{EditorError, EditorImage, EditorOptions};
use vst3::Steinberg::Vst::{IEditControllerTrait, ViewType};
use vst3::Steinberg::{
    FIDString, IPlugFrame, IPlugView, IPlugViewContentScaleSupport, IPlugViewContentScaleSupportTrait, IPlugViewTrait,
    ViewRect, kResultOk, kResultTrue,
};
use vst3::{ComPtr, ComWrapper};

/// The kind of window a view is embedded in on this platform.
#[cfg(windows)]
const PLATFORM_TYPE: FIDString = vst3::Steinberg::kPlatformTypeHWND;
#[cfg(target_os = "macos")]
const PLATFORM_TYPE: FIDString = vst3::Steinberg::kPlatformTypeNSView;
#[cfg(not(any(windows, target_os = "macos")))]
const PLATFORM_TYPE: FIDString = vst3::Steinberg::kPlatformTypeX11EmbedWindowID;

fn rect_of(size: Size) -> ViewRect {
    let edge = |length: u32| i32::try_from(length).unwrap_or(i32::MAX);
    ViewRect { left: 0, top: 0, right: edge(size.width), bottom: edge(size.height) }
}

fn size_of(rect: &ViewRect) -> Option<Size> {
    let (width, height) = (rect.right.checked_sub(rect.left)?, rect.bottom.checked_sub(rect.top)?);
    (width > 0 && height > 0).then(|| Size::new(width as u32, height as u32))
}

/// An editor that is open: the plugin's view, in a window, with the frame
/// the view talks to the host through.
pub(super) struct Editor {
    hosted: HostedEditor,
    view: ComPtr<IPlugView>,
    /// The view's way of being told its monitor's scale, if it has one.
    scale: Option<ComPtr<IPlugViewContentScaleSupport>>,
    frame: ComWrapper<PlugFrame>,
}

/// A plugin's view, for the window around it.
struct View<'a> {
    view: &'a ComPtr<IPlugView>,
    scale: Option<&'a ComPtr<IPlugViewContentScaleSupport>>,
    /// The plugin's name, for messages.
    name: &'a str,
    /// Whether the view is in a window, and so has to be taken out of it.
    attached: bool,
}

impl PluginView for View<'_> {
    fn set_scale(&mut self, scale: f64) {
        // A view without the interface works the scale out for itself.
        if let Some(support) = self.scale {
            // SAFETY: the view is live, on the plugin main thread.
            let result = unsafe { support.setContentScaleFactor(scale as f32) };
            if result != kResultOk {
                log::debug!("[{}] its editor did not take the scale {scale} ({})", self.name, result_name(result));
            }
        }
    }

    fn size(&mut self) -> Option<Size> {
        let mut rect = rect_of(Size::new(0, 0));
        // SAFETY: the view is live, and `rect` a local it fills in.
        (unsafe { self.view.getSize(&mut rect) } == kResultOk).then(|| size_of(&rect)).flatten()
    }

    fn can_resize(&mut self) -> bool {
        // SAFETY: the view is live.
        unsafe { self.view.canResize() == kResultTrue }
    }

    fn constrain(&mut self, size: Size) -> Size {
        let mut rect = rect_of(size);
        // SAFETY: the view is live, and `rect` a local it adjusts.
        let checked = unsafe { self.view.checkSizeConstraint(&mut rect) } == kResultTrue;
        // A view that has nothing to say about sizes says "not
        // implemented", and one that says nonsense is not followed.
        size_of(&rect).filter(|_| checked).unwrap_or(size)
    }

    fn set_size(&mut self, size: Size) {
        // SAFETY: the view is live, and the rectangle outlives the call.
        let result = unsafe { self.view.onSize(&mut rect_of(size)) };
        if result != kResultOk {
            log::debug!("[{}] its editor did not take the size {size:?} ({})", self.name, result_name(result));
        }
    }

    fn attach(&mut self, parent: &Window) -> Result<(), String> {
        // SAFETY: the view is live; the handle is that of a live window of
        // this thread, which stays valid for as long as the view is
        // attached: an editor detaches its view before it destroys its
        // window.
        let result = unsafe { self.view.attached(parent.handle(), PLATFORM_TYPE) };
        if result != kResultOk {
            return Err(format!("the plugin could not put its editor into a window ({})", result_name(result)));
        }
        self.attached = true;
        Ok(())
    }

    /// A VST3 view is visible as soon as it is attached.
    fn show(&mut self) {}

    fn detach(&mut self) {
        // SAFETY: the view is live. It is taken out of its window only if
        // it was put into one, and the host's frame is taken away from it
        // before the frame goes.
        unsafe {
            if std::mem::take(&mut self.attached) {
                self.view.removed();
            }
            self.view.setFrame(null_mut());
        }
    }
}

impl Instance {
    fn open_editor(&mut self, id: InstanceId, options: &EditorOptions) -> Result<(), EditorError> {
        if let Some(editor) = &self.editor {
            editor.hosted.bring_to_front(options.activate);
            return Ok(());
        }
        let controller = self.controller.as_ref().ok_or(EditorError::NoEditor)?;
        if !SUPPORTED {
            return Err(EditorError::Unsupported);
        }
        // SAFETY: the controller is live, on the plugin main thread; the
        // name is a C string constant. On success the controller hands
        // over a reference to the view, which `from_raw` takes charge of.
        let view = unsafe { ComPtr::from_raw(controller.edit.createView(ViewType::kEditor)) };
        // SAFETY: the view is live, and the type a C string constant.
        let view = view.filter(|view| unsafe { view.isPlatformTypeSupported(PLATFORM_TYPE) } == kResultTrue);
        let Some(view) = view else {
            // Now it is known, and nobody needs to be offered an editor
            // that is not there again.
            self.editor_status.set_available(false);
            return Err(EditorError::NoEditor);
        };

        let frame = PlugFrame::new(id);
        // SAFETY: the view is live, and the frame outlives its use by the
        // view: `View::detach` takes it away again before it is dropped.
        unsafe { view.setFrame(interface::<_, IPlugFrame>(&frame)) };
        let scale = view.cast::<IPlugViewContentScaleSupport>();
        let request = OpenRequest {
            options,
            plugin_name: &self.name,
            remembered: self.placement,
            status: self.editor_status.clone(),
            // Called by the window while the user drags its edge. If the
            // plugin thread is in the middle of something, the size goes
            // through as it is and is settled when the drag has ended.
            constrain: Box::new(move |size| {
                with_current(|thread: &mut PluginThread| thread.vst3.constrain_editor(id, size))
                    .flatten()
                    .unwrap_or(size)
            }),
        };
        let mut opening = View { view: &view, scale: scale.as_ref(), name: &self.name, attached: false };
        let hosted = HostedEditor::open(&mut opening, request).map_err(EditorError::Failed)?;
        // A view that asked for another size while it was being attached
        // (views do, once they see the window they are in) is given it
        // when the thread idles, which is as soon as this job is done.
        self.editor = Some(Editor { hosted, view, scale, frame });
        self.editor_closing = false;
        Ok(())
    }

    /// Closes the editor if it is open: the view is taken out of its
    /// window and released, and the window goes.
    pub(super) fn close_editor(&mut self) {
        self.editor_closing = false;
        let Some(Editor { hosted, view, scale, frame }) = self.editor.take() else { return };
        let mut closing = View { view: &view, scale: scale.as_ref(), name: &self.name, attached: true };
        self.placement = Some(hosted.close(&mut closing));
        // The view is released before the frame it was given.
        drop((scale, view));
        drop(frame);
    }

    /// Resizes the editor's window because its view asked for `size`, and
    /// tells the view the size it got, as VST3 has it.
    fn resize_editor(&mut self, size: Size) -> bool {
        let Some(Editor { hosted, view, scale, .. }) = self.editor.as_mut() else { return false };
        log::debug!("[{}] its editor asks to be {size:?}", self.name);
        let mut view = View { view, scale: scale.as_ref(), name: &self.name, attached: true };
        let given = hosted.resize_to(&mut view, size);
        view.set_size(given);
        true
    }

    /// Acts on what the user did to the editor's window and what the view
    /// asked for since the last call.
    pub(super) fn service_editor(&mut self, nested: bool) {
        if let Some(size) = self.editor.as_ref().and_then(|editor| editor.frame.take_request()) {
            self.resize_editor(size);
        }
        let Some(editor) = self.editor.as_mut() else { return };
        let Editor { hosted, view, scale, .. } = editor;
        let mut view = View { view, scale: scale.as_ref(), name: &self.name, attached: true };
        self.editor_closing |= hosted.service(&mut view) == Serviced::Close;
        // Closing takes the view away from the plugin, which waits until
        // nothing of the plugin's can be further up the stack.
        if self.editor_closing && !nested {
            self.close_editor();
        }
    }
}

impl Vst3Thread {
    /// Opens the editor of an instance, or brings it to the front if it
    /// is open already.
    pub(crate) fn open_editor(&mut self, id: InstanceId, options: &EditorOptions) -> Result<(), EditorError> {
        let gone = || EditorError::Failed("the plugin instance no longer exists".to_string());
        self.instances.get_mut(&id).ok_or_else(gone)?.open_editor(id, options)
    }

    pub(crate) fn close_editor(&mut self, id: InstanceId) {
        if let Some(instance) = self.instances.get_mut(&id) {
            instance.close_editor();
        }
    }

    /// The open editor of an instance, as it is drawn right now.
    pub(crate) fn capture_editor(&mut self, id: InstanceId) -> Option<EditorImage> {
        self.instances.get(&id)?.editor.as_ref()?.hosted.capture()
    }

    /// Serves a view's request for another size, if the view's editor is
    /// open. For the view's `IPlugFrame`.
    pub(crate) fn resize_editor(&mut self, id: InstanceId, size: Size) -> bool {
        self.instances.get_mut(&id).is_some_and(|instance| instance.resize_editor(size))
    }

    /// The size nearest to `size` that the open editor of an instance
    /// accepts.
    fn constrain_editor(&mut self, id: InstanceId, size: Size) -> Option<Size> {
        let instance = self.instances.get(&id)?;
        let Editor { view, scale, .. } = instance.editor.as_ref()?;
        Some(View { view, scale: scale.as_ref(), name: &instance.name, attached: true }.constrain(size))
    }
}
