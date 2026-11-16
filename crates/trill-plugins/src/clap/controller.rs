//! The off-audio-thread half of a hosted plugin: state, parameter values
//! and the plugin's editor.

use super::events::ParamFeedback;
use super::instance::InstanceId;
use crate::editor::{EDITOR_TIMEOUT, EditorStatus};
use crate::edits::Edit;
use crate::main_thread::MainThread;
use crate::plugin_thread::{PluginThread, keep_for_good};
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{DeviceController, EditorError, EditorImage, EditorOptions, ParamEdit, ParamEditKind};

/// How long a plugin may take to serialize its state or report its
/// parameter values before the request is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Forwards the session's requests to the plugin main thread, whichever
/// thread the session happens to be driven from.
///
/// The controller does not keep the plugin alive: the instance belongs to
/// its processor. Once that is dropped, requests simply return `None`.
pub(crate) struct ClapController {
    main: MainThread<PluginThread>,
    instance: InstanceId,
    plugin_id: String,
    editor: Arc<EditorStatus>,
    feedback: Arc<ParamFeedback>,
}

impl ClapController {
    pub(crate) fn new(
        main: MainThread<PluginThread>,
        instance: InstanceId,
        plugin_id: String,
        editor: Arc<EditorStatus>,
        feedback: Arc<ParamFeedback>,
    ) -> Self {
        Self { main, instance, plugin_id, editor, feedback }
    }
}

impl DeviceController for ClapController {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.clap.save_state(instance)) {
            Ok(state) => state,
            Err(error) => {
                log::warn!("[{}] cannot save its state: {error}", self.plugin_id);
                None
            }
        }
    }

    fn param_values(&mut self) -> Option<Vec<f64>> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.clap.param_values(instance)) {
            Ok(values) => values,
            Err(error) => {
                log::warn!("[{}] cannot report its parameter values: {error}", self.plugin_id);
                None
            }
        }
    }

    fn has_editor(&self) -> bool {
        self.editor.is_available()
    }

    fn open_editor(&mut self, options: &EditorOptions) -> Result<(), EditorError> {
        if !self.editor.is_available() {
            return Err(EditorError::NoEditor);
        }
        let (instance, options) = (self.instance, options.clone());
        let opened = self.main.call(EDITOR_TIMEOUT, move |thread| thread.clap.open_editor(instance, &options));
        // A plugin that has been asked for its editor has made this thread
        // its user-interface thread, whether or not the editor came up.
        keep_for_good(&self.main);
        opened.unwrap_or_else(|error| Err(EditorError::Failed(error.to_string())))
    }

    fn close_editor(&mut self) {
        if !self.editor.is_open() {
            return;
        }
        let instance = self.instance;
        if let Err(error) = self.main.call_at_rest(EDITOR_TIMEOUT, move |thread| thread.clap.close_editor(instance)) {
            log::warn!("[{}] its editor was not closed cleanly: {error}", self.plugin_id);
        }
    }

    fn is_editor_open(&self) -> bool {
        self.editor.is_open()
    }

    fn capture_editor(&mut self) -> Option<EditorImage> {
        if !self.editor.is_open() {
            return None;
        }
        let instance = self.instance;
        self.main.call(REQUEST_TIMEOUT, move |thread| thread.clap.capture_editor(instance)).ok().flatten()
    }

    /// CLAP parameters are in plain units to begin with, and the plugin
    /// reports them by their place in the device's list, so the edits are
    /// handed on as they come.
    fn take_param_edits(&mut self) -> Vec<ParamEdit> {
        let mut edits = Vec::new();
        let lost = self.feedback.edits.drain(|index, edit| {
            let kind = match edit {
                Edit::Begin => ParamEditKind::Begin,
                Edit::Value(value) => ParamEditKind::Value(value),
                Edit::End => ParamEditKind::End,
            };
            edits.push(ParamEdit { index, kind });
        });
        if lost {
            log::warn!(
                "[{}] changed its parameters faster than the changes were collected; some were dropped",
                self.plugin_id
            );
        }
        edits
    }
}

impl Drop for ClapController {
    /// An editor does not outlive the device it belongs to: when the
    /// session lets go of the device, its window goes with it.
    fn drop(&mut self) {
        self.close_editor();
    }
}
