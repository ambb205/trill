//! The off-audio-thread half of a hosted plugin: state, parameter values
//! and the plugin's editor.

use super::instance::{Created, InstanceId};
use super::links::ParamLinks;
use super::params::Mapping;
use crate::editor::{EDITOR_TIMEOUT, EditorStatus};
use crate::edits::Edit;
use crate::main_thread::{CallError, MainThread};
use crate::plugin_thread::{PluginThread, keep_for_good};
use crate::presets::{LOAD_TIMEOUT, Locator, await_arrival};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{
    DeviceController, EditorError, EditorImage, EditorOptions, ParamEdit, ParamEditKind, PresetLoadError,
};
use trill_core::presets::PresetInfo;

/// How long a plugin may take to serialize its state or report its
/// parameter values before the request is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Forwards the session's requests to the plugin main thread, whichever
/// thread the session happens to be driven from.
///
/// The controller does not keep the plugin alive: the instance belongs to
/// its processor. Once that is dropped, requests simply return `None`.
pub(crate) struct Vst3Controller {
    main: MainThread<PluginThread>,
    instance: InstanceId,
    /// The plugin's name, for messages.
    name: String,
    editor: Arc<EditorStatus>,
    /// Where the edits made in the plugin's editor are recorded.
    links: Arc<ParamLinks>,
    /// For each parameter the device lists, by its slot among all of the
    /// plugin's: its place in the device's list, and how its plain values
    /// relate to the normalized ones the plugin deals in.
    listed: HashMap<usize, (usize, Mapping)>,
}

impl Vst3Controller {
    pub(crate) fn new(main: MainThread<PluginThread>, name: String, created: &Created) -> Self {
        let listed = created.params.iter().enumerate().map(|(index, spec)| (spec.slot, (index, spec.mapping)));
        Self {
            main,
            instance: created.id,
            name,
            editor: created.editor.clone(),
            links: created.links.clone(),
            listed: listed.collect(),
        }
    }
}

impl DeviceController for Vst3Controller {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.vst3.save_state(instance)) {
            Ok(state) => state,
            Err(error) => {
                log::warn!("[{}] cannot save its state: {error}", self.name);
                None
            }
        }
    }

    fn param_values(&mut self) -> Option<Vec<f64>> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.vst3.param_values(instance)) {
            Ok(values) => values,
            Err(error) => {
                log::warn!("[{}] cannot report its parameter values: {error}", self.name);
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
        let opened = self.main.call(EDITOR_TIMEOUT, move |thread| thread.vst3.open_editor(instance, &options));
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
        if let Err(error) = self.main.call_at_rest(EDITOR_TIMEOUT, move |thread| thread.vst3.close_editor(instance)) {
            log::warn!("[{}] its editor was not closed cleanly: {error}", self.name);
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
        self.main.call(REQUEST_TIMEOUT, move |thread| thread.vst3.capture_editor(instance)).ok().flatten()
    }

    /// The plugin reports edits by its own parameter ids and in normalized
    /// values; they are handed on by the parameter's place in the device's
    /// list and in plain units. Edits of parameters the device does not
    /// list have nowhere to be reported to.
    fn take_param_edits(&mut self) -> Vec<ParamEdit> {
        let mut edits = Vec::new();
        let lost = self.links.edits.drain(|slot, edit| {
            let Some(&(index, mapping)) = self.listed.get(&slot) else { return };
            let kind = match edit {
                Edit::Begin => ParamEditKind::Begin,
                Edit::Value(normalized) => ParamEditKind::Value(mapping.plain(normalized)),
                Edit::End => ParamEditKind::End,
            };
            edits.push(ParamEdit { index, kind });
        });
        if lost {
            log::warn!(
                "[{}] changed its parameters faster than the changes were collected; some were dropped",
                self.name
            );
        }
        edits
    }

    fn load_preset(&mut self, preset: &PresetInfo) -> Result<(), PresetLoadError> {
        let instance = self.instance;
        let locator = Locator::parse(&preset.locator)
            .filter(|locator| matches!(locator, Locator::Vst3File { .. } | Locator::Vst3Program { .. }))
            .ok_or_else(|| PresetLoadError::Failed("it is not a preset of a VST3 plugin".into()))?;
        let before = self.save_state();
        let loaded = self.main.call(LOAD_TIMEOUT, move |thread| match locator {
            Locator::Vst3File { path } => thread.vst3.load_preset_file(instance, &path),
            Locator::Vst3Program { param, index, steps } => thread.vst3.load_program(instance, param, index, steps),
            _ => Err("it is not a preset of a VST3 plugin".to_string()),
        });
        if matches!(loaded, Ok(Ok(()))) {
            // A program is switched to by the plugin's processor, with its
            // next block; and plugins take even a state they are handed
            // on the spot a moment later, on their own time.
            await_arrival(before.as_deref(), || self.save_state());
        }
        match loaded {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(PresetLoadError::Failed(reason)),
            Err(CallError::TimedOut) => Err(PresetLoadError::Unresponsive(format!(
                "the plugin did not answer within {} s",
                LOAD_TIMEOUT.as_secs()
            ))),
            Err(error) => Err(PresetLoadError::Failed(error.to_string())),
        }
    }
}

impl Drop for Vst3Controller {
    /// An editor does not outlive the device it belongs to: when the
    /// session lets go of the device, its window goes with it.
    fn drop(&mut self) {
        self.close_editor();
    }
}
