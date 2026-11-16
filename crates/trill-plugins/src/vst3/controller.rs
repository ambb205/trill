//! The off-audio-thread half of a hosted plugin: state, parameter values
//! and the plugin's editor.

use super::class_id::ClassId;
use super::instance::{Created, InstanceId};
use super::links::ParamLinks;
use super::params::Mapping;
use crate::editor::{EDITOR_TIMEOUT, EditorStatus};
use crate::edits::Edit;
use crate::main_thread::{CallError, MainThread};
use crate::plugin_thread::{PluginThread, keep_for_good};
use crate::presets::{LOAD_TIMEOUT, Locator, await_arrival};
use crate::scan::Scanner;
use crate::vendor;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_core::device::{
    ChannelLayout, ControllerSupport, DeviceController, EditorError, EditorImage, EditorOptions, ParamEdit,
    ParamEditKind, PresetLoadError,
};
use trill_core::presets::PresetInfo;

/// How long a plugin may take to serialize its state or report its
/// parameter values before the request is abandoned.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a plugin that was handed a state built from a vendor's preset
/// is given to show the preset in its own state, and how often it is
/// looked at meanwhile. Plugins take a state on a moment after they are
/// handed it; none was seen to need a second.
const TOOK_TIMEOUT: Duration = if cfg!(test) { Duration::from_millis(400) } else { Duration::from_secs(8) };
const TOOK_POLL: Duration = Duration::from_millis(25);

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
    /// The MIDI controllers the plugin has named a parameter for.
    support: ControllerSupport,
    /// The channels of the plugin's main buses.
    channels: ChannelLayout,
    /// The value the plugin calls each listed parameter's default.
    defaults: Vec<f64>,
    /// The plugin's class, which a state built for it is filed under.
    class: ClassId,
    /// Where a vendor's preset is made into a state.
    scanner: Scanner,
    /// What the plugin goes on doing after the preset it was last given,
    /// until someone asks.
    note: Option<String>,
}

impl Vst3Controller {
    pub(crate) fn new(
        main: MainThread<PluginThread>,
        name: String,
        created: &Created,
        class: ClassId,
        scanner: Scanner,
    ) -> Self {
        let listed = created.params.iter().enumerate().map(|(index, spec)| (spec.slot, (index, spec.mapping)));
        Self {
            class,
            scanner,
            note: None,
            main,
            instance: created.id,
            name,
            editor: created.editor.clone(),
            links: created.links.clone(),
            listed: listed.collect(),
            support: created.midi.support(),
            channels: ChannelLayout {
                inputs: created.buses.inputs.first().copied().unwrap_or(0),
                outputs: created.buses.outputs.first().copied().unwrap_or(0),
            },
            defaults: created.params.iter().map(|spec| spec.declared).collect(),
        }
    }
}

impl Vst3Controller {
    /// Both states of the plugin as it is now.
    fn states(&self) -> Result<vendor::States, PresetLoadError> {
        let instance = self.instance;
        match self.main.call(REQUEST_TIMEOUT, move |thread| thread.vst3.state_of(instance)) {
            Ok(Some(state)) => Ok(state),
            Ok(None) => Err(PresetLoadError::Failed("the plugin is no longer running".into())),
            Err(CallError::TimedOut) => Err(PresetLoadError::Unresponsive(format!(
                "the plugin did not give its state within {} s",
                REQUEST_TIMEOUT.as_secs()
            ))),
            Err(error) => Err(PresetLoadError::Failed(error.to_string())),
        }
    }

    /// Loads a preset that is in a file of its vendor's own format: makes
    /// the state it stands for, hands the plugin that, and waits until
    /// the plugin's own state shows the preset.
    fn load_vendor_preset(
        &mut self,
        format: vendor::Format,
        file: &Path,
        key: Option<&str>,
    ) -> Result<(), PresetLoadError> {
        let current = self.states()?;
        let built = vendor::child::build(&self.scanner, format, file, key, self.class, &current);
        self.hand_over(format, built.map_err(PresetLoadError::Failed)?, "preset")
    }

    /// Hands the plugin a state that was built for it, and waits until
    /// the plugin's own state shows what the state was built to bring:
    /// `what`, a preset or settings.
    fn hand_over(&mut self, format: vendor::Format, built: vendor::Built, what: &str) -> Result<(), PresetLoadError> {
        let instance = self.instance;
        let vendor::Built { state, marks, note } = built;
        match self.main.call(LOAD_TIMEOUT, move |thread| thread.vst3.load_state(instance, &state)) {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => return Err(PresetLoadError::Failed(reason)),
            Err(CallError::TimedOut) => {
                return Err(PresetLoadError::Unresponsive(format!(
                    "the plugin did not answer within {} s",
                    LOAD_TIMEOUT.as_secs()
                )));
            }
            Err(error) => return Err(PresetLoadError::Failed(error.to_string())),
        }
        // The plugin has said yes. Whether it took the preset is what its
        // own state shows, a moment later.
        let deadline = Instant::now() + TOOK_TIMEOUT;
        loop {
            let after = self.states()?;
            let missing = vendor::missing(format, &marks, &after);
            if vendor::took(&marks, &missing) {
                break;
            }
            if Instant::now() >= deadline {
                let first = missing.first().map(|mark| mark.name.as_str()).unwrap_or_default();
                return Err(PresetLoadError::Failed(format!(
                    "the plugin was handed the {what} and did not take it: its state does not show {} of the {} \
                     settings the {what} gives it ('{first}' among them)",
                    missing.len(),
                    marks.len()
                )));
            }
            std::thread::sleep(TOOK_POLL);
        }
        self.note = note;
        Ok(())
    }
}

impl DeviceController for Vst3Controller {
    fn controller_support(&self) -> ControllerSupport {
        self.support.clone()
    }

    fn channels(&self) -> Option<ChannelLayout> {
        Some(self.channels)
    }

    fn param_defaults(&self) -> Option<Vec<f64>> {
        Some(self.defaults.clone())
    }

    fn param_texts(&mut self, values: &[(usize, f64)]) -> Vec<Option<String>> {
        let (instance, asked) = (self.instance, values.to_vec());
        let count = values.len();
        self.main
            .call(REQUEST_TIMEOUT, move |thread| thread.vst3.param_texts(instance, &asked))
            .unwrap_or_else(|_| vec![None; count])
    }

    fn param_value(&mut self, index: usize, text: &str) -> Option<f64> {
        let (instance, text) = (self.instance, text.to_string());
        self.main.call(REQUEST_TIMEOUT, move |thread| thread.vst3.param_value(instance, index, &text)).ok().flatten()
    }

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
        self.note = None;
        let locator = Locator::parse(&preset.locator)
            .filter(|locator| {
                matches!(locator, Locator::Vst3File { .. } | Locator::Vst3Program { .. } | Locator::Vendor { .. })
            })
            .ok_or_else(|| PresetLoadError::Failed("it is not a preset of a VST3 plugin".into()))?;
        if let Locator::Vendor { format, path, key } = &locator {
            return self.load_vendor_preset(*format, path, key.as_deref());
        }
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

    fn take_preset_note(&mut self) -> Option<String> {
        self.note.take()
    }

    fn settings(&mut self) -> Option<Vec<(String, String)>> {
        let format = vendor::Format::of_class(&self.class.to_string())?;
        vendor::settings(format, &self.states().ok()?)
    }

    fn set_settings(&mut self, values: &[(String, String)]) -> Result<(), PresetLoadError> {
        self.note = None;
        let format = vendor::Format::of_class(&self.class.to_string()).ok_or(PresetLoadError::Unsupported)?;
        let current = self.states()?;
        // The plugin's own state is read here, not a file from elsewhere.
        let built = vendor::with_settings(format, &current, values).map_err(PresetLoadError::Failed)?;
        self.hand_over(format, built, "settings")
    }
}

impl Drop for Vst3Controller {
    /// An editor does not outlive the device it belongs to: when the
    /// session lets go of the device, its window goes with it.
    fn drop(&mut self) {
        self.close_editor();
    }
}
