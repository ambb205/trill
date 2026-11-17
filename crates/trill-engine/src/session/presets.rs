//! The session's part in presets: listing what a device offers and giving
//! a device one of them.
//!
//! One interface covers two kinds of preset. A built-in device's preset is
//! a set of parameter values, which goes into the project like any other
//! edit of its parameters. A plugin's preset is the plugin's own: only the
//! plugin can load it, and what comes of that is in the plugin, not in the
//! project. The session asks the running plugin to load it, in place and
//! while it plays, and makes an undo step of the change by how it treats
//! the plugin's saved state:
//!
//! * the state the plugin had is captured just before, into the step;
//! * from then on the project holds no state for the device (`None`
//!   means "what the running plugin has"), until the state is next
//!   captured: when the project is saved, rendered or copied for safety,
//!   or when the step is undone, which is when it is needed.
//!
//! Nothing is read back from the plugin right after it was told to load.
//! A plugin may finish loading a moment later, on threads of its own, and
//! what it would say in between is the sound it had.
//!
//! A preset is asked for by its name or id, among the ones its device type
//! lists, or by the path of its file, for one that lies where nothing is
//! listed from: a preset that was bought and is kept with the samples, one
//! that was sent. Either way it comes to a [`PresetInfo`] and goes through
//! the same door from there.

use super::{Session, SessionError, preset_values};
use crate::ready::Plugins;
use crate::saved::{Saved, SavedPresets};
use std::sync::Arc;
use trill_core::device::PresetLoadError;
use trill_core::presets::{PresetFile, PresetInfo, PresetList, PresetLookup, PresetRef, PresetStatus, SavedPreset};
use trill_core::{DeviceId, EditError};

/// The most presets an error names, as what there is or what is near:
/// enough to name every preset of a built-in device.
const NAMED_IN_ERRORS: usize = 24;

impl Session {
    /// The presets of the device type `uri`, built-in device and plugin
    /// alike, as far as they are known right now. A snapshot: search it,
    /// page it and look presets up in it with the methods of
    /// [`PresetList`], and ask again for a newer one.
    ///
    /// Never waits, so a view may call it while it draws. A plugin's
    /// presets are read in the background, starting the first time they
    /// are asked for; until that is done the list says so
    /// ([`PresetList::status`]) and holds what was found so far, or what
    /// was found the last time the program ran. A type that is not known
    /// has no presets.
    ///
    /// A caller that would rather wait for a complete list (a tool call,
    /// never a view) takes [`Session::factory`], lets go of the session,
    /// and asks the factory with `presets_within`.
    pub fn presets(&self, uri: &str) -> PresetList {
        self.factory.presets(uri).unwrap_or_default()
    }

    /// The presets a device in the project can be given: those of its
    /// type; see [`Session::presets`].
    pub fn device_presets(&self, id: DeviceId) -> Result<PresetList, SessionError> {
        Ok(self.presets(&self.project.device(id)?.uri))
    }

    /// The preset a device was last given, if any: its id, to find it in
    /// the list by, and its name, to show while the list is still being
    /// read. It says what the sound started from, not that nothing has
    /// been changed since. It is part of the project: saved with it, and
    /// taken back by undo along with the sound.
    pub fn device_preset(&self, id: DeviceId) -> Option<&PresetRef> {
        self.project.device(id).ok()?.preset.as_ref()
    }

    /// Has the presets of the device type `uri` read again, in the
    /// background: for a "rescan" button, after the user saved or
    /// installed presets while the program ran. [`Session::presets`]
    /// says that the list is being read until that is done.
    pub fn refresh_presets(&self, uri: &str) {
        self.factory.refresh_presets(uri);
    }

    /// Gives a device one of its presets, replacing its sound, and returns
    /// which preset it came to. `preset` is the preset's id
    /// ([`PresetInfo::id`], what a view that has the list passes), its
    /// name, or its name with as much of its category in front as tells
    /// it apart (`Pads/Warm`).
    ///
    /// Or it is the whole path of a preset file, for a preset that is not
    /// listed because of where it lies ([`PresetFile`]): a file of the
    /// kind the device type's presets are kept in (a plugin's `.vstpreset`,
    /// its maker's own format where that is read), or a preset that was
    /// saved from a device of this type (`.trillpreset`). A file that is
    /// another device's, damaged, or not there is an error that says so,
    /// and the device is as it was.
    ///
    /// One undo step, and the song keeps playing. For a built-in device
    /// every parameter returns to its default and the preset's values are
    /// applied. A plugin loads the preset itself, as its own browser
    /// would; the parameter values the project had stored for it are
    /// dropped, since the preset replaces them.
    ///
    /// Fails, with the device as it was, if there is no such preset (the
    /// error names the nearest ones, or the candidates when a name is not
    /// unique, and says so when the list is still being read) or if the
    /// plugin does not load it.
    ///
    /// This returns when the plugin has answered: for most presets within
    /// a few hundredths of a second, for one that brings samples as long
    /// as those take to read, and with an error after half a minute from
    /// a plugin that does not answer at all.
    pub fn load_preset(&mut self, id: DeviceId, preset: &str) -> Result<PresetInfo, SessionError> {
        let uri = self.project.device(id)?.uri.clone();
        let (info, saved) = self.wanted_preset(&uri, preset)?;
        if let Some(saved) = saved {
            self.load_saved_preset(id, &info, saved)?;
            return Ok(info);
        }
        let values = self.factory.descriptors().iter().find(|d| d.uri == uri).and_then(|d| preset_values(d, &info));
        match values {
            Some(values) => self.edit_scoped(super::Scope::ParamsOnly, |project| {
                let device = project.device_mut(id)?;
                device.params = values.into_iter().collect();
                device.preset = Some(info.reference());
                Ok(())
            })?,
            None => self.load_plugin_preset(id, &info)?,
        }
        Ok(info)
    }

    /// The presets that were saved, of every device type: where they are
    /// kept is the store's to be told ([`SavedPresets::open`]), which an
    /// application does once, when it starts. Until then, and in a
    /// session that is to leave nothing behind, they are kept in memory.
    pub fn saved_presets(&self) -> &Arc<SavedPresets> {
        &self.saved
    }

    /// Saves a device's sound as it is now as a preset of its type, under
    /// `name`, in place of the saved preset of that name if there is one.
    /// From then on the preset is listed with the type's own, under
    /// "Saved", and given to a device like any other: by its name.
    ///
    /// What is saved is everything the device would be saved with in a
    /// project: a plugin's state, with what was done in its own window, a
    /// sample player's files, and every parameter value of a device whose
    /// sound is its parameters.
    ///
    /// The device is marked as being on the preset, which is one undo
    /// step; the preset itself stays when that is undone.
    pub fn save_preset(
        &mut self,
        id: DeviceId,
        name: &str,
        description: &str,
        tags: &[String],
    ) -> Result<Saved, SessionError> {
        let name = SavedPreset::checked_name(name).map_err(SessionError::Device)?;
        let (uri, device_name) = {
            let device = self.project.device(id)?;
            (device.uri.clone(), device.name.clone())
        };
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        if let Some(error) = &meta.error {
            return Err(SessionError::Device(format!("{id} is unavailable, so there is no sound to save: {error}")));
        }
        let plugin = meta.controller.is_some();
        // What a plugin has is asked of it now; what the project holds for
        // it may be from before its editor was last used.
        self.absorb_editor_edits();
        self.capture_state_of(id);
        let device = self.project.device(id)?;
        let params = if plugin {
            device.params.clone()
        } else {
            // Every value, so that loading the preset leaves nothing of
            // the sound the device had before.
            self.device_params(id)?.into_iter().map(|state| (state.info.id, state.value)).collect()
        };
        // A plugin that hands a host no state, with nothing set on it
        // from here: a preset of that would be a name and no sound.
        if plugin && device.state.is_none() && params.is_empty() {
            return Err(SessionError::Device(format!(
                "{id} ({device_name}) gives a host no state to keep, and none of its parameters was set from here, \
                 so there is no sound to save"
            )));
        }
        let type_name = self.factory.descriptors().into_iter().find(|d| d.uri == uri).map(|d| d.name);
        let preset = SavedPreset {
            uri,
            device: type_name.unwrap_or(device_name),
            name,
            description: description.trim().to_string(),
            tags: tags.iter().map(|tag| tag.trim().to_string()).filter(|tag| !tag.is_empty()).collect(),
            params,
            state: device.state.clone(),
        };
        let saved = self.saved.save(preset).map_err(SessionError::Io)?;
        let reference = saved.info.reference();
        self.edit_scoped(super::Scope::ParamsOnly, |project| {
            project.device_mut(id)?.preset = Some(reference);
            Ok(())
        })?;
        // The list of this type's presets is another one now.
        self.revision += 1;
        Ok(saved)
    }

    /// Removes the preset saved under `name` for the device type `uri`,
    /// and says whether there was one.
    pub fn delete_saved_preset(&mut self, uri: &str, name: &str) -> bool {
        let removed = self.saved.delete(uri, name);
        if removed {
            self.revision += 1;
        }
        removed
    }

    /// The saved preset that `info`, a preset of the device type `uri`,
    /// stands for; `None` for a preset the type brings itself.
    pub(super) fn saved_preset(&self, uri: &str, info: &PresetInfo) -> Result<Option<SavedPreset>, SessionError> {
        self.saved.load(uri, info).map_err(SessionError::Io)
    }

    /// Gives a device a preset that was saved: its parameter values and
    /// its state become the preset's, as one undo step. A device with a
    /// state of its own is created again from the preset's, which is how
    /// a plugin takes a state on whole; the song keeps playing meanwhile.
    fn load_saved_preset(&mut self, id: DeviceId, info: &PresetInfo, saved: SavedPreset) -> Result<(), SessionError> {
        let name = self.project.device(id)?.name.clone();
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        if let Some(error) = &meta.error {
            return Err(SessionError::Device(format!("{id} is unavailable: {error}")));
        }
        // What was done in a plugin's editor until now belongs to the time
        // before the preset, and so does the state it is in.
        self.absorb_editor_edits();
        self.capture_state_of(id);
        let revision = self.revision;
        self.edit(|project| {
            let device = project.device_mut(id)?;
            device.params = saved.params;
            device.state = saved.state;
            device.preset = Some(info.reference());
            Ok(())
        })?;
        if let Some(reason) = self.bank.devices.get(&id).and_then(|meta| meta.error.clone()) {
            if self.revision != revision {
                self.back_out();
            }
            return Err(SessionError::Device(format!(
                "{id} ({name}) could not be started from the saved preset '{}': {reason}. The device is as it was",
                info.name
            )));
        }
        Ok(())
    }

    /// What `wanted` asks for of the device type `uri`: one of the type's
    /// listed presets, by its name or id, or the preset in a file, by the
    /// file's path. With it comes the saved preset it stands for, if it is
    /// one. An error says what there is instead, or what is wrong with the
    /// file.
    pub(super) fn wanted_preset(
        &self,
        uri: &str,
        wanted: &str,
    ) -> Result<(PresetInfo, Option<SavedPreset>), SessionError> {
        let Some(asked) = PresetFile::asked(wanted) else {
            let info = self.find_preset(uri, wanted)?;
            let saved = self.saved_preset(uri, &info)?;
            return Ok((info, saved));
        };
        let refused = |reason: String| {
            SessionError::Device(format!("'{}' cannot be given to '{uri}' as a preset: {reason}", asked.path.display()))
        };
        if SavedPresets::is_preset_file(&asked.path) {
            let saved = SavedPresets::read_file(&asked.path).map_err(refused)?;
            if saved.uri != uri {
                let whose = if saved.device.is_empty() { "another device" } else { saved.device.as_str() };
                return Err(refused(format!("it is a preset of {whose} ({})", saved.uri)));
            }
            return Ok((saved.info(), Some(saved)));
        }
        match self.factory.preset_file(uri, &asked.path, asked.within.as_deref()) {
            Some(Ok(info)) => Ok((info, None)),
            Some(Err(reason)) => Err(refused(reason)),
            None => Err(SessionError::Device(format!("unknown device '{uri}'"))),
        }
    }

    /// The preset of the device type `uri` that `wanted` asks for, or an
    /// error that says what there is instead.
    pub(super) fn find_preset(&self, uri: &str, wanted: &str) -> Result<PresetInfo, SessionError> {
        let list = self.presets(uri);
        let named = |presets: &[&PresetInfo]| {
            let shown: Vec<String> = presets.iter().take(NAMED_IN_ERRORS).map(|preset| preset.path()).collect();
            let more = if presets.len() > NAMED_IN_ERRORS { ", ..." } else { "" };
            format!("{}{more}", shown.join(", "))
        };
        let message = match list.find(wanted) {
            PresetLookup::Found(preset) => return Ok(preset.clone()),
            PresetLookup::Ambiguous(found) => {
                let shown: Vec<String> = found
                    .iter()
                    .take(NAMED_IN_ERRORS)
                    .map(|preset| format!("{} (id {})", preset.path(), preset.id))
                    .collect();
                let more = if found.len() > NAMED_IN_ERRORS { ", ..." } else { "" };
                format!(
                    "'{uri}' has {} presets named '{wanted}': {}{more}. Ask for one by its category and name, or by \
                     its id",
                    found.len(),
                    shown.join(", ")
                )
            }
            PresetLookup::Missing { similar } => {
                let all: Vec<&PresetInfo> = list.presets.iter().collect();
                let known = match (&list.status, list.len()) {
                    (PresetStatus::Reading, 0) => "its presets are still being read; ask again in a moment".to_string(),
                    (PresetStatus::Reading, found) => format!(
                        "its presets are still being read, {found} so far; ask again in a moment. Nearest so far: {}",
                        if similar.is_empty() { "none".to_string() } else { named(&similar) }
                    ),
                    (PresetStatus::Failed(reason), 0) => format!("its presets could not be read: {reason}"),
                    (_, 0) => "it offers no presets that can be listed".to_string(),
                    (_, count) if count <= NAMED_IN_ERRORS => format!("available: {}", named(&all)),
                    (_, count) if similar.is_empty() => {
                        format!("nothing like it among its {count} presets; search them for what there is")
                    }
                    (_, _) => format!("nearest: {}", named(&similar)),
                };
                format!("'{uri}' has no preset '{wanted}' ({known})")
            }
        };
        Err(SessionError::Device(message))
    }

    /// Has a plugin that is already in the project load one of its
    /// presets, as an undo step. See the module documentation.
    fn load_plugin_preset(&mut self, id: DeviceId, preset: &PresetInfo) -> Result<(), SessionError> {
        let name = self.project.device(id)?.name.clone();
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        if let Some(error) = &meta.error {
            return Err(SessionError::Device(format!("{id} is unavailable: {error}")));
        }
        // What was done in the plugin's editor until now belongs to the
        // time before the preset, and so does the state it is in.
        self.absorb_editor_edits();
        self.capture_state_of(id);
        let before = self.project.clone();

        if let Err(error) = self.ask_to_load(id, preset) {
            let after = self.put_back(id, &error);
            return Err(SessionError::Device(format!(
                "{id} ({name}) did not load the preset '{}': {error}. {after}",
                preset.name
            )));
        }
        if let Some(meta) = self.bank.devices.get_mut(&id) {
            meta.applied_state = None;
        }
        let device = self.project.device_mut(id)?;
        device.state = None;
        device.params.clear();
        device.preset = Some(preset.reference());
        // The running plugin has made the change itself: there is nothing
        // to bring the engine in line with.
        self.make_step(before);
        Ok(())
    }

    /// Has a plugin that the step under way has just added load the
    /// preset it is to start from. The step is the caller's to take back
    /// if this fails.
    pub(super) fn start_from_preset(&mut self, id: DeviceId, preset: &PresetInfo) -> Result<(), SessionError> {
        let uri = self.project.device(id)?.uri.clone();
        self.ask_to_load(id, preset).map_err(|error| {
            SessionError::Device(format!("'{uri}' did not load the preset '{}': {error}", preset.name))
        })?;
        self.project.device_mut(id)?.preset = Some(preset.reference());
        Ok(())
    }

    /// What a device goes on doing after the preset or the settings it
    /// was last given, where that is worth telling whoever asked: a
    /// sampler reads its samples for a while, silent until it has. Given
    /// once; `None` for a device that is as ready as it was. (Whether it
    /// is ready yet is asked with [`Session::readiness_job`].)
    pub fn take_preset_note(&mut self, id: DeviceId) -> Option<String> {
        self.bank.devices.get(&id)?.controller.as_ref()?.lock().take_preset_note()
    }

    /// The settings in a plugin's saved state that it shows a host no
    /// parameter for, by name, with the value each has as the plugin
    /// writes it. Empty for a device whose state is not a list of named
    /// settings that is read.
    pub fn device_settings(&self, id: DeviceId) -> Result<Vec<(String, String)>, SessionError> {
        self.project.device(id)?;
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        let Some(controller) = &meta.controller else { return Ok(Vec::new()) };
        let mut settings = controller.lock().settings().unwrap_or_default();
        // What has a parameter is set as a parameter.
        settings.retain(|(name, _)| !meta.params.iter().any(|param| param.name == *name));
        Ok(settings)
    }

    /// Gives settings of [`Session::device_settings`] other values, by
    /// name: one undo step, while the song plays. The plugin is handed
    /// its state with the values in place, as it is handed a preset. A
    /// name that is no setting is an error that says which names come
    /// near, and the device is as it was.
    pub fn set_device_settings(&mut self, id: DeviceId, values: &[(String, String)]) -> Result<(), SessionError> {
        let name = self.project.device(id)?.name.clone();
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        if let Some(error) = &meta.error {
            return Err(SessionError::Device(format!("{id} is unavailable: {error}")));
        }
        self.absorb_editor_edits();
        self.capture_state_of(id);
        let before = self.project.clone();
        let asked = match self.bank.devices.get(&id).and_then(|meta| meta.controller.as_ref()) {
            Some(controller) => controller.lock().set_settings(values),
            None => Err(PresetLoadError::Unsupported),
        };
        if let Err(error) = asked {
            let after = self.put_back(id, &error);
            let reason = match error {
                PresetLoadError::Unsupported => "its saved state has no settings that are set by name".to_string(),
                error => error.to_string(),
            };
            return Err(SessionError::Device(format!("{id} ({name}) was not given the settings: {reason}. {after}")));
        }
        if let Some(meta) = self.bank.devices.get_mut(&id) {
            meta.applied_state = None;
        }
        // The state is the running plugin's until it is next captured,
        // as after a preset it loaded itself.
        self.project.device_mut(id)?.state = None;
        self.make_step(before);
        Ok(())
    }

    fn ask_to_load(&self, id: DeviceId, preset: &PresetInfo) -> Result<(), PresetLoadError> {
        match self.bank.devices.get(&id).and_then(|meta| meta.controller.as_ref()) {
            Some(controller) => {
                let loaded = controller.lock().load_preset(preset);
                // A sampler goes on to read the preset's samples.
                if loaded.is_ok() && self.project.device(id).is_ok_and(|device| Plugins::is_plugin(&device.uri)) {
                    self.bank.loads.began(id);
                }
                loaded
            }
            None => Err(PresetLoadError::Unsupported),
        }
    }

    /// Sees to it that a plugin which failed to load a preset is as it
    /// was before it tried, and says in a sentence what came of that.
    fn put_back(&mut self, id: DeviceId, error: &PresetLoadError) -> &'static str {
        if matches!(error, PresetLoadError::Unresponsive(_)) {
            // Asking it for its state would only be waited for in turn.
            return "The plugin is not answering, so whether it changed is not known; removing the device and \
                    undoing that brings it back as it was";
        }
        let kept = self.project.device(id).ok().and_then(|device| device.state.clone());
        match (self.live_state(id), kept) {
            (Some(now), Some(kept)) if now != kept => {
                // The attempt changed it. The running instance no longer
                // is what the project describes, which is all it takes to
                // have it created again from what the project holds.
                if let Some(meta) = self.bank.devices.get_mut(&id) {
                    meta.applied_state = Some(now);
                }
                self.compile_and_swap();
                "The attempt changed the plugin, which was put back as it was"
            }
            _ => "The device is as it was",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AudioBackend;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use trill_core::device::{
        CompositeFactory, DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory,
        DeviceInstance, DeviceProcessor, Event, ParamInfo, ParamKind, ProcessInfo,
    };
    use trill_core::presets::{PresetQuery, short_id};
    use trill_core::{ChainRef, Device, Project};
    use trill_dsp::BuiltinFactory;

    const URI: &str = "test:synth";
    const SYNTH: &str = "builtin:polysynth";

    /// A plugin as the session sees one: a patch it keeps to itself, a
    /// counter for what was done in its editor, and two parameters whose
    /// values come with the patch.
    #[derive(Clone, Debug, Default, PartialEq)]
    struct Plugin {
        patch: String,
        tweaks: u32,
        /// A patch it has said it will load, and has not yet.
        arriving: Option<String>,
        /// It was created from a saved state and has not taken it on yet:
        /// until it has, it reports the state it was born with.
        restoring: bool,
        values: [f64; 2],
        /// Every value its processor was sent.
        sent: Vec<(usize, f64)>,
    }

    impl Plugin {
        fn with_patch(patch: &str, tweaks: u32) -> Self {
            Self { patch: patch.into(), tweaks, values: patch_values(patch), ..Self::default() }
        }

        fn state(&self) -> Vec<u8> {
            if self.restoring {
                return b"init|0".to_vec();
            }
            format!("{}|{}", self.patch, self.tweaks).into_bytes()
        }

        fn from_state(state: Option<&[u8]>) -> Self {
            let text = state.map(String::from_utf8_lossy).unwrap_or_default();
            match text.split_once('|') {
                Some((patch, tweaks)) => Self::with_patch(patch, tweaks.parse().unwrap_or(0)),
                None => Self::with_patch("init", 0),
            }
        }
    }

    /// The cutoff and drive a patch comes with.
    fn patch_values(patch: &str) -> [f64; 2] {
        [patch.len() as f64 / 20.0, 0.25]
    }

    #[derive(Default)]
    struct World {
        plugins: HashMap<DeviceId, Plugin>,
        created: usize,
        list: PresetList,
        refreshed: usize,
        /// Plugins created from now on take a saved state on a moment
        /// after they are created.
        slow_to_restore: bool,
    }

    #[derive(Clone, Default)]
    struct Plugins(Arc<Mutex<World>>);

    impl Plugins {
        fn plugin(&self, id: DeviceId) -> Plugin {
            self.0.lock().plugins[&id].clone()
        }

        fn patch(&self, id: DeviceId) -> String {
            self.plugin(id).patch
        }

        fn created(&self) -> usize {
            self.0.lock().created
        }

        /// The user turns a knob in the plugin's own editor.
        fn tweak(&self, id: DeviceId) {
            self.0.lock().plugins.get_mut(&id).unwrap().tweaks += 1;
        }

        /// A patch the plugin said it would load arrives, and a state it
        /// was created from is taken on.
        fn arrive(&self, id: DeviceId) {
            let mut world = self.0.lock();
            let plugin = world.plugins.get_mut(&id).unwrap();
            plugin.restoring = false;
            if let Some(patch) = plugin.arriving.take() {
                plugin.values = patch_values(&patch);
                plugin.patch = patch;
            }
        }

        /// Waits for the audio thread to hand the plugin's processor the
        /// value `expected` for its parameter `index`.
        fn wait_for_value(&self, id: DeviceId, index: usize, expected: f64) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while self.plugin(id).values[index] != expected {
                assert!(Instant::now() < deadline, "the value never arrived: {:?}", self.plugin(id));
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    struct Processor {
        device: DeviceId,
        world: Arc<Mutex<World>>,
    }

    impl DeviceProcessor for Processor {
        fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}

        fn set_param(&mut self, index: usize, value: f64) {
            // A test stand-in: a real processor would not lock here.
            if let Some(plugin) = self.world.lock().plugins.get_mut(&self.device) {
                plugin.values[index] = value;
                plugin.sent.push((index, value));
            }
        }

        fn process(&mut self, _: &ProcessInfo, _: &[Event], _: &mut [f32], _: &mut [f32]) {}
        fn reset(&mut self) {}
    }

    struct Controller {
        device: DeviceId,
        world: Arc<Mutex<World>>,
    }

    impl DeviceController for Controller {
        fn save_state(&mut self) -> Option<Vec<u8>> {
            self.world.lock().plugins.get(&self.device).map(Plugin::state)
        }

        fn param_values(&mut self) -> Option<Vec<f64>> {
            self.world.lock().plugins.get(&self.device).map(|plugin| plugin.values.to_vec())
        }

        fn load_preset(&mut self, preset: &PresetInfo) -> Result<(), PresetLoadError> {
            let mut world = self.world.lock();
            let plugin = world.plugins.get_mut(&self.device).expect("the plugin exists");
            match preset.locator.as_str() {
                "damaged" => Err(PresetLoadError::Failed("the file is damaged".into())),
                "half" => {
                    plugin.patch = "half loaded".into();
                    Err(PresetLoadError::Failed("it gave up halfway".into()))
                }
                "stuck" => Err(PresetLoadError::Unresponsive("it did not answer within 30 s".into())),
                locator => {
                    match locator.strip_prefix("late:") {
                        Some(patch) => plugin.arriving = Some(patch.into()),
                        None => {
                            plugin.patch = locator.into();
                            plugin.values = patch_values(locator);
                        }
                    }
                    Ok(())
                }
            }
        }
    }

    impl DeviceFactory for Plugins {
        fn descriptors(&self) -> Vec<DeviceDescriptor> {
            vec![DeviceDescriptor {
                uri: URI.into(),
                name: "Synth".into(),
                vendor: "Test".into(),
                category: DeviceCategory::Instrument,
                description: String::new(),
                params: Vec::new(),
                presets: Vec::new(),
            }]
        }

        fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
            if device.uri != URI {
                return Err(DeviceError::Unknown(device.uri.clone()));
            }
            let mut plugin = Plugin::from_state(device.state.as_deref());
            let param = |id: &str, default: f64| ParamInfo {
                id: id.into(),
                name: id.into(),
                min: 0.0,
                max: 1.0,
                default,
                unit: String::new(),
                kind: ParamKind::Float,
            };
            // As a plugin host does: the defaults are what the state came
            // with, and the model's values go on top of them.
            let params = vec![param("cutoff", plugin.values[0]), param("drive", plugin.values[1])];
            for (index, info) in params.iter().enumerate() {
                if let Some(value) = device.params.get(&info.id) {
                    plugin.values[index] = *value;
                }
            }
            let mut world = self.0.lock();
            plugin.restoring = world.slow_to_restore && device.state.is_some();
            world.plugins.insert(device.id, plugin);
            world.created += 1;
            Ok(DeviceInstance {
                processor: Box::new(Processor { device: device.id, world: self.0.clone() }),
                params,
                controller: Some(Box::new(Controller { device: device.id, world: self.0.clone() })),
            })
        }

        fn presets(&self, uri: &str) -> Option<PresetList> {
            (uri == URI).then(|| self.0.lock().list.clone())
        }

        fn refresh_presets(&self, uri: &str) {
            if uri == URI {
                self.0.lock().refreshed += 1;
            }
        }

        /// A preset file of this plugin's is a line of text: the word
        /// "patch" and the patch. One with several lines holds several.
        fn preset_file(&self, uri: &str, file: &Path, within: Option<&str>) -> Option<Result<PresetInfo, String>> {
            if uri != URI {
                return None;
            }
            let Ok(text) = std::fs::read_to_string(file) else { return Some(Err("there is no such file".into())) };
            let patches: Vec<&str> = text.lines().filter_map(|line| line.strip_prefix("patch ")).collect();
            Some(match (patches.as_slice(), within) {
                ([], _) => Err("it is not a preset of Synth".to_string()),
                ([patch], None) => Ok(preset(&PresetFile::name_of(file), "", patch)),
                (_, None) => Err(format!("it holds {} presets", patches.len())),
                (patches, Some(within)) => match patches.iter().find(|patch| **patch == within) {
                    Some(patch) => Ok(preset(patch, "", patch)),
                    None => Err(format!("it holds no preset '{within}'")),
                },
            })
        }
    }

    fn preset(name: &str, category: &str, locator: &str) -> PresetInfo {
        PresetInfo {
            id: short_id(&format!("{category}/{locator}")),
            name: name.into(),
            category: category.into(),
            locator: locator.into(),
            ..PresetInfo::default()
        }
    }

    fn presets() -> Vec<PresetInfo> {
        vec![
            preset("Warm Pad", "Factory/Pads", "warm pad"),
            preset("Glass", "Factory/Pads", "glass"),
            preset("Deep Bass", "Factory/Basses", "deep bass"),
            preset("Warm Pad", "User/Pads", "my warm pad"),
            preset("Slow One", "User", "late:slow one"),
            preset("Damaged", "User", "damaged"),
            preset("Half", "User", "half"),
            preset("Stuck", "User", "stuck"),
        ]
    }

    fn session() -> (Session, Plugins) {
        let plugins = Plugins::default();
        plugins.0.lock().list = PresetList::ready(presets());
        let factory = CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(plugins.clone())]);
        let session = Session::new(Arc::new(factory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
        (session, plugins)
    }

    fn model(session: &Session, id: DeviceId) -> Device {
        session.project().device(id).unwrap().clone()
    }

    fn message(error: SessionError) -> String {
        error.to_string()
    }

    #[test]
    fn built_in_devices_and_plugins_list_their_presets_the_same_way() {
        let (mut session, plugins) = session();
        let synth = session.add_device(ChainRef::Master, SYNTH, None, None).unwrap();
        let plugin = session.add_device(ChainRef::Master, URI, None, None).unwrap();

        let built_in = session.presets(SYNTH);
        assert_eq!(built_in.status, PresetStatus::Ready);
        let pad = built_in.get("warm_pad").expect("the poly synth has a warm pad");
        assert_eq!((pad.name.as_str(), pad.description.is_empty()), ("warm_pad", false));
        assert_eq!(session.device_presets(synth).unwrap(), built_in);

        let listed = session.device_presets(plugin).unwrap();
        assert_eq!(listed.len(), 8);
        let query = PresetQuery { text: "warm", category: Some("user"), offset: 0, limit: 5 };
        assert_eq!(listed.search(&query).presets[0].locator, "my warm pad");

        // A type nobody knows has none, and a device that does not exist
        // is an error.
        assert_eq!(session.presets("test:nobody"), PresetList::default());
        assert!(session.device_presets(DeviceId(999)).is_err());

        session.refresh_presets(URI);
        session.refresh_presets(SYNTH);
        assert_eq!(plugins.0.lock().refreshed, 1);
    }

    #[test]
    fn a_built_in_preset_is_loaded_by_name_and_remembered() {
        let (mut session, _) = session();
        let synth = session.add_device(ChainRef::Master, SYNTH, Some("warm_pad"), None).unwrap();
        assert_eq!(session.device_preset(synth).unwrap().name, "warm_pad");
        let warm = model(&session, synth).params;
        assert!(!warm.is_empty());

        let loaded = session.load_preset(synth, "sub_bass").unwrap();
        assert_eq!(loaded.id, "sub_bass");
        assert_eq!(session.device_preset(synth).unwrap().id, "sub_bass");
        assert_ne!(model(&session, synth).params, warm);

        assert!(session.undo());
        assert_eq!(model(&session, synth).params, warm);
        assert_eq!(session.device_preset(synth).unwrap().id, "warm_pad", "undo takes the name back with the sound");

        // The older way of asking, and a preset there is none of.
        session.apply_preset(synth, "Sub_Bass").unwrap();
        assert_eq!(session.device_preset(synth).unwrap().id, "sub_bass");
        let error = message(session.load_preset(synth, "nope").unwrap_err());
        assert!(error.contains("has no preset 'nope'"), "{error}");
        let error = message(session.add_device(ChainRef::Master, SYNTH, Some("nope"), None).unwrap_err());
        assert!(error.contains("has no preset 'nope'"), "{error}");
    }

    #[test]
    fn a_plugin_loads_its_preset_in_place_as_one_undo_step() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        // The user has set the drive through the DAW, and turned a knob
        // in the plugin's own editor.
        session.set_device_params(device, &[("drive".into(), 0.9)]).unwrap();
        plugins.tweak(device);
        let before = plugins.plugin(device);
        let step = session.undo_step();

        let loaded = session.load_preset(device, "glass").unwrap();
        assert_eq!((loaded.name.as_str(), loaded.category.as_str()), ("Glass", "Factory/Pads"));
        assert_eq!(plugins.patch(device), "glass");
        assert_eq!(plugins.created(), 1, "the running plugin loaded it; nothing was created again");
        assert_ne!(session.undo_step(), step, "a step of its own");
        let after = model(&session, device);
        assert_eq!(after.preset, Some(loaded.reference()));
        assert!(after.params.is_empty(), "the preset replaces what was set by hand");
        assert_eq!(session.device_params(device).unwrap()[0].value, patch_values("glass")[0]);

        // One undo: the patch, the knob turned in the editor and the
        // drive set through the DAW are all back.
        assert!(session.undo());
        assert_eq!(plugins.patch(device), before.patch);
        assert_eq!(plugins.plugin(device).tweaks, before.tweaks);
        assert_eq!(model(&session, device).params.get("drive"), Some(&0.9));
        assert_eq!(model(&session, device).preset, None);
        assert_eq!(session.undo_step(), step);

        // And one redo brings the preset back.
        assert!(session.redo());
        assert_eq!(plugins.patch(device), "glass");
        assert_eq!(model(&session, device).preset, Some(loaded.reference()));
        assert!(model(&session, device).params.is_empty());
    }

    #[test]
    fn what_is_done_to_a_preset_afterwards_comes_back_with_it() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        session.load_preset(device, "Deep Bass").unwrap();
        // The user shapes the preset in the plugin's editor, then does
        // something unrelated.
        plugins.tweak(device);
        plugins.tweak(device);
        let track = session.edit(|project| Ok(project.add_track("Keys"))).unwrap();

        // Undoing the unrelated step leaves the plugin alone.
        assert!(session.undo());
        assert!(session.project().track(track).is_err());
        assert_eq!((plugins.patch(device), plugins.plugin(device).tweaks), ("deep bass".to_string(), 2));
        assert_eq!(plugins.created(), 1);

        // Undoing the preset returns to the patch before it; redoing it
        // returns to the preset as it was left, knobs and all.
        assert!(session.undo());
        assert_eq!((plugins.patch(device), plugins.plugin(device).tweaks), ("init".to_string(), 0));
        assert!(session.redo());
        assert_eq!((plugins.patch(device), plugins.plugin(device).tweaks), ("deep bass".to_string(), 2));
        assert!(session.redo());
        assert!(session.project().track(track).is_ok());
        assert_eq!((plugins.patch(device), plugins.plugin(device).tweaks), ("deep bass".to_string(), 2));
    }

    #[test]
    fn a_plugin_that_loads_a_moment_later_is_undone_and_saved_all_the_same() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        session.load_preset(device, "glass").unwrap();

        // The plugin says yes and loads on a thread of its own: for a
        // while it still has, and would report, the patch it had.
        session.load_preset(device, "Slow One").unwrap();
        assert_eq!(plugins.patch(device), "glass");
        plugins.arrive(device);
        assert_eq!(plugins.patch(device), "slow one");

        // The file gets what the plugin has, not what it had.
        let folder = std::env::temp_dir().join(format!("trill-preset-save-{}", std::process::id()));
        let path = folder.join("song.trill");
        session.save(&path).unwrap();
        let saved = Project::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let stored = saved.device(device).unwrap();
        assert_eq!(stored.state.as_deref(), Some(b"slow one|0".as_slice()));
        assert_eq!(stored.preset.as_ref().unwrap().name, "Slow One");

        // Each undo goes back one preset, and no further.
        assert!(session.undo());
        assert_eq!(plugins.patch(device), "glass");
        assert!(session.undo());
        assert_eq!(plugins.patch(device), "init");
        assert!(session.redo() && session.redo());
        assert_eq!(plugins.patch(device), "slow one");

        // Loading the file brings the preset back in a new instance.
        session.new_project();
        session.load(&path).unwrap();
        assert_eq!(plugins.patch(device), "slow one");
        assert_eq!(session.device_preset(device).unwrap().name, "Slow One");
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_plugin_that_has_not_taken_its_saved_state_on_yet_does_not_lose_it() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, Some("glass"), None).unwrap();
        let folder = std::env::temp_dir().join(format!("trill-preset-settle-{}", std::process::id()));
        let path = folder.join("song.trill");
        session.save(&path).unwrap();
        let stored = |path: &std::path::Path| {
            let saved = Project::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
            String::from_utf8(saved.device(device).unwrap().state.clone().expect("a state")).unwrap()
        };
        assert_eq!(stored(&path), "glass|0");

        // Opened again, with a plugin that takes its state on with its
        // first block, as some do: asked at once, it knows of none.
        plugins.0.lock().slow_to_restore = true;
        session.new_project();
        session.load(&path).unwrap();
        assert_eq!(plugins.plugin(device).state(), b"init|0");
        // Saved, rendered and copied for safety in that moment, the
        // project still has the state it was opened with.
        let again = folder.join("again.trill");
        session.save(&again).unwrap();
        assert_eq!(stored(&again), "glass|0");
        assert_eq!(session.render_job().project.device(device).unwrap().state.as_deref(), Some(b"glass|0".as_slice()));
        let copy = Project::from_json(&session.snapshot_json()).unwrap();
        assert_eq!(copy.device(device).unwrap().state.as_deref(), Some(b"glass|0".as_slice()));
        // And an undo that crosses it keeps it too.
        session.load_preset(device, "Deep Bass").unwrap();
        assert!(session.undo());
        assert_eq!(session.project().device(device).unwrap().state.as_deref(), Some(b"glass|0".as_slice()));

        // Once the plugin has had its moment, what it says counts again:
        // a knob turned in its editor is saved.
        plugins.arrive(device);
        session.bank.devices.get_mut(&device).unwrap().settling = None;
        plugins.tweak(device);
        session.save(&again).unwrap();
        assert_eq!(stored(&again), "glass|1");
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn a_preset_is_asked_for_by_id_name_or_category() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        let list = session.device_presets(device).unwrap();

        // Two presets are named alike: the error names both, each with
        // what tells it apart, and nothing is loaded.
        let error = message(session.load_preset(device, "warm pad").unwrap_err());
        assert!(error.contains("2 presets named 'warm pad'"), "{error}");
        assert!(error.contains("Factory/Pads/Warm Pad") && error.contains("User/Pads/Warm Pad"), "{error}");
        assert!(error.contains(&list.presets[3].id), "{error}");
        assert_eq!(plugins.patch(device), "init");

        session.load_preset(device, "user/pads/warm pad").unwrap();
        assert_eq!(plugins.patch(device), "my warm pad");
        session.load_preset(device, &list.presets[0].id).unwrap();
        assert_eq!(plugins.patch(device), "warm pad");

        // A preset there is none of: the nearest are named.
        let error = message(session.load_preset(device, "deep").unwrap_err());
        assert!(error.contains("has no preset 'deep' (available: "), "{error}");
        assert!(error.contains("Factory/Basses/Deep Bass"), "{error}");
        assert_eq!(plugins.patch(device), "warm pad");
        assert!(matches!(session.load_preset(DeviceId(999), "glass"), Err(SessionError::Edit(_))));
    }

    #[test]
    fn a_list_that_is_still_being_read_says_so_when_a_preset_is_missing() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();

        plugins.0.lock().list = PresetList::reading(Vec::new());
        let error = message(session.load_preset(device, "glass").unwrap_err());
        assert!(error.contains("still being read; ask again in a moment"), "{error}");

        plugins.0.lock().list = PresetList::reading(presets()[..2].to_vec());
        let error = message(session.load_preset(device, "deep bass").unwrap_err());
        assert!(error.contains("still being read, 2 so far"), "{error}");
        // What has been found already can be loaded already.
        session.load_preset(device, "glass").unwrap();

        plugins.0.lock().list =
            PresetList { status: PresetStatus::Failed("the plugin crashed".into()), presets: Vec::new().into() };
        let error = message(session.load_preset(device, "glass").unwrap_err());
        assert!(error.contains("could not be read: the plugin crashed"), "{error}");

        plugins.0.lock().list = PresetList::default();
        let error = message(session.load_preset(device, "glass").unwrap_err());
        assert!(error.contains("offers no presets"), "{error}");

        // Many presets, and nothing like what was asked for.
        let many: Vec<PresetInfo> = (0..40).map(|n| preset(&format!("Patch {n}"), "Bank", &format!("p{n}"))).collect();
        plugins.0.lock().list = PresetList::ready(many);
        let error = message(session.load_preset(device, "glass").unwrap_err());
        assert!(error.contains("nothing like it among its 40 presets"), "{error}");
        let error = message(session.load_preset(device, "patch").unwrap_err());
        assert!(error.contains("nearest: Bank/Patch 0, ") && error.ends_with("Bank/Patch 7)"), "{error}");
    }

    #[test]
    fn a_preset_that_fails_to_load_leaves_the_plugin_as_it_was_and_says_why() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        session.load_preset(device, "glass").unwrap();
        session.set_device_params(device, &[("drive".into(), 0.7)]).unwrap();
        plugins.tweak(device);
        let project = session.project().clone();
        let (step, plugin) = (session.undo_step(), plugins.plugin(device));

        // Refused outright.
        let error = message(session.load_preset(device, "Damaged").unwrap_err());
        assert!(error.contains("did not load the preset 'Damaged': the file is damaged"), "{error}");
        assert!(error.contains("The device is as it was"), "{error}");
        assert_eq!((plugins.patch(device), plugins.created()), ("glass".to_string(), 1));

        // Failed after it had already changed: it is created again from
        // the state it had, with what was set by hand.
        let error = message(session.load_preset(device, "Half").unwrap_err());
        assert!(error.contains("it gave up halfway") && error.contains("put back as it was"), "{error}");
        assert_eq!(plugins.created(), 2);
        let restored = plugins.plugin(device);
        assert_eq!((restored.patch, restored.tweaks, restored.values[1]), (plugin.patch, plugin.tweaks, 0.7));

        // Not answering: said, and nothing more is asked of it.
        let error = message(session.load_preset(device, "Stuck").unwrap_err());
        assert!(error.contains("did not answer within 30 s") && error.contains("is not answering"), "{error}");

        // None of it was an edit.
        assert_eq!(session.undo_step(), step);
        assert_eq!(session.project().device(device).unwrap().params, project.device(device).unwrap().params);
        assert_eq!(session.device_preset(device).unwrap().name, "Glass");
    }

    #[test]
    fn a_plugin_is_added_with_a_preset_in_one_step_or_not_at_all() {
        let (mut session, plugins) = session();
        let before = session.project().clone();

        let device = session.add_device(ChainRef::Master, URI, Some("Deep Bass"), None).unwrap();
        assert_eq!(plugins.patch(device), "deep bass");
        assert_eq!(session.device_preset(device).unwrap().name, "Deep Bass");
        assert!(session.undo());
        assert_eq!(session.project(), &before, "one step for the device and its preset");
        assert!(session.redo());
        assert_eq!(plugins.patch(device), "deep bass", "and the preset comes back with the device");
        assert_eq!(session.device_preset(device).unwrap().name, "Deep Bass");
        assert!(session.undo());

        // A preset there is none of, and one that will not load: nothing
        // is added and nothing is left to undo or redo.
        let depth = session.undo_step();
        for (wanted, reason) in [("nope", "has no preset 'nope'"), ("Damaged", "the file is damaged")] {
            let error = message(session.add_device(ChainRef::Master, URI, Some(wanted), None).unwrap_err());
            assert!(error.contains(reason), "{error}");
            assert_eq!(session.project(), &before);
            assert_eq!(session.undo_step(), depth);
        }
    }

    #[test]
    fn a_parameter_set_after_a_preset_returns_to_the_presets_value_when_undone() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        let created_with = plugins.plugin(device).values[0];
        session.load_preset(device, "Deep Bass").unwrap();
        let with_preset = patch_values("deep bass")[0];
        assert_ne!(created_with, with_preset);

        session.set_device_params(device, &[("cutoff".into(), 0.95)]).unwrap();
        plugins.wait_for_value(device, 0, 0.95);
        assert!(session.undo());
        // Not the value the plugin was created with: the one the preset
        // brought, which is what it had when the cutoff was set.
        plugins.wait_for_value(device, 0, with_preset);
        assert_eq!(session.device_params(device).unwrap()[0].value, with_preset);
        assert_eq!(plugins.patch(device), "deep bass");
    }

    #[test]
    fn a_preset_and_values_set_in_one_gesture_are_one_step() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        let step = session.undo_step();

        // What a tool does that takes a preset and parameters together.
        session.begin_gesture();
        session.load_preset(device, "glass").unwrap();
        session.set_device_params(device, &[("drive".into(), 0.5)]).unwrap();
        session.end_gesture();
        assert_eq!(model(&session, device).params.get("drive"), Some(&0.5));

        assert!(session.undo());
        assert_eq!(session.undo_step(), step);
        assert_eq!(plugins.patch(device), "init");
        assert!(model(&session, device).params.is_empty());
        assert!(session.redo());
        assert_eq!(plugins.patch(device), "glass");
        plugins.wait_for_value(device, 1, 0.5);
    }

    #[test]
    fn a_plugins_sound_is_saved_under_a_name_and_given_to_any_device_of_its_type() {
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, Some("Glass"), None).unwrap();
        // The sound was worked on: in the plugin's own editor, and through
        // the DAW.
        plugins.tweak(device);
        session.set_device_params(device, &[("drive".into(), 0.9)]).unwrap();
        let step = session.undo_step();

        let tags = ["bright".to_string(), " ".to_string()];
        let saved = session.save_preset(device, " My Glass ", "for the chorus", &tags).unwrap();
        assert_eq!((saved.info.path().as_str(), saved.replaced, &saved.file), ("Saved/My Glass", false, &None));
        assert_eq!(saved.info.tags, ["bright"]);
        // It is listed with the plugin's own, in front of them, and the
        // device is on it; nothing about the plugin changed.
        let list = session.presets(URI);
        assert_eq!(list.len(), presets().len() + 1);
        assert_eq!(list.presets[0], saved.info);
        assert_eq!(session.device_preset(device), Some(&saved.info.reference()));
        assert_eq!((plugins.patch(device), plugins.plugin(device).tweaks, plugins.created()), ("glass".into(), 1, 1));
        assert_ne!(session.undo_step(), step, "being on the preset is a step");

        // Another device starts from it: the patch, the knob that was
        // turned in the editor, and the value set through the DAW.
        let second = session.add_device(ChainRef::Master, URI, Some("my glass"), None).unwrap();
        assert_eq!((plugins.patch(second), plugins.plugin(second).tweaks), ("glass".into(), 1));
        assert_eq!(model(&session, second).params.get("drive"), Some(&0.9));
        assert_eq!(session.device_preset(second).unwrap().name, "My Glass");
        assert!(session.undo());
        assert!(session.project().device(second).is_err(), "one step for the device and its preset");

        // A device that has another sound is given it in place of that, as
        // one step.
        let third = session.add_device(ChainRef::Master, URI, Some("Deep Bass"), None).unwrap();
        plugins.tweak(third);
        let step = session.undo_step();
        let loaded = session.load_preset(third, "Saved/My Glass").unwrap();
        assert_eq!(loaded, saved.info);
        assert_eq!((plugins.patch(third), plugins.plugin(third).tweaks), ("glass".into(), 1));
        assert_eq!(model(&session, third).params.get("drive"), Some(&0.9));
        assert!(session.undo());
        assert_eq!(session.undo_step(), step);
        assert_eq!((plugins.patch(third), plugins.plugin(third).tweaks), ("deep bass".into(), 1));
        assert!(model(&session, third).params.is_empty());
        assert_eq!(session.device_preset(third).unwrap().name, "Deep Bass");
        assert!(session.redo());
        assert_eq!(plugins.patch(third), "glass");

        // Saved again under that name, it is replaced; removed, it is gone
        // from the list.
        let again = session.save_preset(third, "MY GLASS", "", &[]).unwrap();
        assert!(again.replaced);
        assert_eq!(session.presets(URI).len(), presets().len() + 1);
        assert!(session.delete_saved_preset(URI, "my glass") && !session.delete_saved_preset(URI, "my glass"));
        assert_eq!(session.presets(URI).len(), presets().len());
        let error = message(session.load_preset(third, "My Glass").unwrap_err());
        assert!(error.contains("has no preset 'My Glass'"), "{error}");
    }

    #[test]
    fn a_built_in_devices_sound_is_saved_with_every_value_it_has() {
        let (mut session, _plugins) = session();
        let synth = session.add_device(ChainRef::Master, SYNTH, Some("warm_pad"), None).unwrap();
        session.set_device_params(synth, &[("cutoff".into(), 432.0)]).unwrap();
        let values = |session: &Session, id| -> Vec<f64> {
            session.device_params(id).unwrap().iter().map(|state| state.value).collect()
        };
        let sound = values(&session, synth);

        let saved = session.save_preset(synth, "Dark Pad", "", &[]).unwrap();
        let list = session.presets(SYNTH);
        assert_eq!(list.presets[0].path(), "Saved/Dark Pad");
        assert!(list.find("warm_pad") != PresetLookup::Missing { similar: Vec::new() }, "its own are still there");

        // Whatever the device is given next, the saved sound comes back
        // whole: also the values that were at their defaults.
        session.load_preset(synth, "sub_bass").unwrap();
        assert_ne!(values(&session, synth), sound);
        session.load_preset(synth, "dark pad").unwrap();
        assert_eq!(values(&session, synth), sound);
        assert_eq!(session.device_preset(synth), Some(&saved.info.reference()));
        assert!(session.undo());
        assert_eq!(session.device_preset(synth).unwrap().name, "sub_bass");

        let other = session.add_device(ChainRef::Master, SYNTH, Some("Dark Pad"), None).unwrap();
        assert_eq!(values(&session, other), sound);
        // It is a preset of the synth, not of anything else.
        let error =
            message(session.add_device(ChainRef::Master, "builtin:reverb", Some("Dark Pad"), None).unwrap_err());
        assert!(error.contains("has no preset 'Dark Pad'"), "{error}");
    }

    #[test]
    fn a_preset_is_not_saved_without_a_name_or_a_device() {
        let (mut session, _plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        let step = session.undo_step();
        for (name, reason) in [("  ", "needs a name"), ("Pads/Mine", "cannot have '/'")] {
            let error = message(session.save_preset(device, name, "", &[]).unwrap_err());
            assert!(error.contains(reason), "{error}");
        }
        assert!(session.save_preset(DeviceId(999), "Mine", "", &[]).is_err());
        assert!(session.saved_presets().is_empty());
        assert_eq!(session.undo_step(), step);
    }

    #[test]
    fn saved_presets_are_kept_in_the_folder_the_store_is_given() {
        let folder = tempfile::tempdir().unwrap();
        let (mut session, plugins) = session();
        session.saved_presets().open(folder.path().to_path_buf());
        let device = session.add_device(ChainRef::Master, URI, Some("Deep Bass"), None).unwrap();
        plugins.tweak(device);
        let saved = session.save_preset(device, "Low End", "", &[]).unwrap();
        assert!(saved.file.as_ref().is_some_and(|file| file.starts_with(folder.path()) && file.exists()));

        // Another run of the program finds it there.
        let (mut later, later_plugins) = self::session();
        later.saved_presets().open(folder.path().to_path_buf());
        assert_eq!(later.presets(URI).presets[0], saved.info);
        let device = later.add_device(ChainRef::Master, URI, Some("Low End"), None).unwrap();
        assert_eq!((later_plugins.patch(device), later_plugins.plugin(device).tweaks), ("deep bass".into(), 1));
    }

    /// A file of the test's own, in a folder that goes with it.
    fn file(folder: &tempfile::TempDir, name: &str, text: &str) -> String {
        let path = folder.path().join(name);
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_plugin_is_given_a_preset_from_a_file_by_the_files_path() {
        let folder = tempfile::tempdir().unwrap();
        let bought = file(&folder, "Bought Lead.patch", "patch bought lead\n");
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, Some("Glass"), None).unwrap();
        session.set_device_params(device, &[("drive".into(), 0.9)]).unwrap();
        plugins.tweak(device);
        let step = session.undo_step();

        // The file is no preset the plugin lists: it lies somewhere else.
        // Its path goes through the door a name goes through.
        let loaded = session.load_preset(device, &bought).unwrap();
        assert_eq!(loaded.name, "Bought Lead");
        assert_eq!((plugins.patch(device), plugins.created()), ("bought lead".to_string(), 1));
        assert_eq!(session.device_preset(device).unwrap().name, "Bought Lead");
        assert!(model(&session, device).params.is_empty(), "the preset replaces what was set by hand");
        assert_ne!(session.undo_step(), step);
        // One step: undone, the plugin is on the preset it was on, with
        // what was done to it; redone, on the file's.
        assert!(session.undo());
        assert_eq!((plugins.patch(device), plugins.plugin(device).tweaks), ("glass".to_string(), 1));
        assert_eq!(session.device_preset(device).unwrap().name, "Glass");
        assert_eq!(session.undo_step(), step);
        assert!(session.redo());
        assert_eq!(plugins.patch(device), "bought lead");

        // A device starts from a file as it starts from a name: one step
        // for the device and its preset.
        let before = session.project().clone();
        let second = session.add_device(ChainRef::Master, URI, Some(&format!("  {bought} ")), None).unwrap();
        assert_eq!(plugins.patch(second), "bought lead");
        assert_eq!(session.device_preset(second).unwrap().name, "Bought Lead");
        assert!(session.undo());
        assert_eq!(session.project(), &before);

        // One of the presets of a file that holds several.
        let bank = file(&folder, "Bank #2.patch", "patch one\npatch two\n");
        let error = message(session.load_preset(device, &bank).unwrap_err());
        assert!(error.ends_with("as a preset: it holds 2 presets"), "{error}");
        assert_eq!(session.load_preset(device, &format!("{bank}#two")).unwrap().name, "two");
        assert_eq!(plugins.patch(device), "two");
    }

    #[test]
    fn a_file_that_is_no_preset_of_the_device_leaves_it_as_it_was_and_says_why() {
        let folder = tempfile::tempdir().unwrap();
        let other = file(&folder, "Other.patch", "a preset of something else");
        let gone = folder.path().join("Gone.patch").to_string_lossy().into_owned();
        let (mut session, plugins) = session();
        let device = session.add_device(ChainRef::Master, URI, Some("Glass"), None).unwrap();
        let synth = session.add_device(ChainRef::Master, SYNTH, Some("warm_pad"), None).unwrap();
        let (step, project) = (session.undo_step(), session.project().clone());

        for (path, reason) in [(&other, "it is not a preset of Synth"), (&gone, "there is no such file")] {
            let error = message(session.load_preset(device, path).unwrap_err());
            assert_eq!(error, format!("'{path}' cannot be given to '{URI}' as a preset: {reason}"));
            // Nor is a device added that was to start from it.
            let error = message(session.add_device(ChainRef::Master, URI, Some(path), None).unwrap_err());
            assert!(error.ends_with(reason), "{error}");
        }
        // A device whose presets are not files says how they are asked for.
        let error = message(session.load_preset(synth, &other).unwrap_err());
        assert!(
            error.ends_with("as a preset: Poly Synth takes no preset files; its presets are asked for by name"),
            "{error}"
        );
        // A name is still a name, with slashes or without.
        let error = message(session.load_preset(device, "Bought/Other.patch").unwrap_err());
        assert!(error.contains("has no preset 'Bought/Other.patch'"), "{error}");

        assert_eq!((session.undo_step(), session.project()), (step, &project), "none of it was an edit");
        assert_eq!((plugins.patch(device), plugins.created()), ("glass".to_string(), 1));
    }

    #[test]
    fn a_saved_presets_file_is_a_preset_wherever_it_lies() {
        // Someone saved two sounds, of a plugin and of a built-in synth.
        let theirs = tempfile::tempdir().unwrap();
        let (mut session, plugins) = session();
        session.saved_presets().open(theirs.path().to_path_buf());
        let device = session.add_device(ChainRef::Master, URI, Some("Deep Bass"), None).unwrap();
        plugins.tweak(device);
        session.set_device_params(device, &[("drive".into(), 0.9)]).unwrap();
        let low = session.save_preset(device, "Low End", "", &[]).unwrap().file.unwrap();
        let synth = session.add_device(ChainRef::Master, SYNTH, Some("warm_pad"), None).unwrap();
        session.set_device_params(synth, &[("cutoff".into(), 432.0)]).unwrap();
        let values = |session: &Session, id| -> Vec<f64> {
            session.device_params(id).unwrap().iter().map(|state| state.value).collect()
        };
        let sound = values(&session, synth);
        let dark = session.save_preset(synth, "Dark Pad", "", &[]).unwrap().file.unwrap();
        let (low, dark) = (low.to_string_lossy().into_owned(), dark.to_string_lossy().into_owned());

        // Sent to someone else, the files are given to their devices by
        // their paths, and are not made presets of theirs.
        let (mut later, later_plugins) = self::session();
        let device = later.add_device(ChainRef::Master, URI, Some(&low), None).unwrap();
        assert_eq!((later_plugins.patch(device), later_plugins.plugin(device).tweaks), ("deep bass".into(), 1));
        assert_eq!(model(&later, device).params.get("drive"), Some(&0.9));
        assert_eq!(later.device_preset(device).unwrap().name, "Low End");
        let synth = later.add_device(ChainRef::Master, SYNTH, Some("sub_bass"), None).unwrap();
        let step = later.undo_step();
        assert_eq!(later.load_preset(synth, &dark).unwrap().path(), "Saved/Dark Pad");
        assert_eq!(values(&later, synth), sound);
        assert!(later.undo());
        assert_eq!((later.undo_step(), later.device_preset(synth).unwrap().name.as_str()), (step, "sub_bass"));
        assert!(later.saved_presets().is_empty(), "nothing was saved here");
        assert_eq!(later.presets(URI).len(), presets().len());

        // Another device's preset, and a file that is not one.
        let error = message(later.load_preset(synth, &low).unwrap_err());
        assert!(error.ends_with(&format!("as a preset: it is a preset of Synth ({URI})")), "{error}");
        let scratch = tempfile::tempdir().unwrap();
        let damaged = file(&scratch, "Damaged.trillpreset", "{ not json");
        let error = message(later.load_preset(device, &damaged).unwrap_err());
        assert!(error.contains("as a preset: it is not a saved preset ("), "{error}");
        let gone = scratch.path().join("Gone.TrillPreset").to_string_lossy().into_owned();
        let error = message(later.load_preset(device, &gone).unwrap_err());
        assert!(error.ends_with("as a preset: there is no such file"), "{error}");
        assert_eq!(later_plugins.patch(device), "deep bass");
    }
}
