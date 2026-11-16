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

use super::{Session, SessionError, preset_values};
use trill_core::device::PresetLoadError;
use trill_core::presets::{PresetInfo, PresetList, PresetLookup, PresetRef, PresetStatus};
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
    /// One undo step, and the song keeps playing. For a built-in device
    /// every parameter returns to its default and the preset's values are
    /// applied. A plugin loads the preset itself, as its own browser
    /// would; the parameter values the project had stored for it are
    /// dropped, since the preset replaces them.
    ///
    /// Fails, with the device as it was, if there is no such preset (the
    /// error names the nearest ones, or the candidates when a name is not
    /// unique, and says so when the list is still being read) or if the
    /// plugin does not load it. A plugin that is given a preset may take
    /// as long over it as the preset needs; this returns when it has
    /// answered.
    pub fn load_preset(&mut self, id: DeviceId, preset: &str) -> Result<PresetInfo, SessionError> {
        let uri = self.project.device(id)?.uri.clone();
        let info = self.find_preset(&uri, preset)?;
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

    fn ask_to_load(&self, id: DeviceId, preset: &PresetInfo) -> Result<(), PresetLoadError> {
        match self.bank.devices.get(&id).and_then(|meta| meta.controller.as_ref()) {
            Some(controller) => controller.lock().load_preset(preset),
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
        values: [f64; 2],
        /// Every value its processor was sent.
        sent: Vec<(usize, f64)>,
    }

    impl Plugin {
        fn with_patch(patch: &str, tweaks: u32) -> Self {
            Self { patch: patch.into(), tweaks, values: patch_values(patch), ..Self::default() }
        }

        fn state(&self) -> Vec<u8> {
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

        /// A patch the plugin said it would load arrives.
        fn arrive(&self, id: DeviceId) {
            let mut world = self.0.lock();
            let plugin = world.plugins.get_mut(&id).unwrap();
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
}
