//! The session's part in plugin editors: opening and closing the window a
//! device brings, and keeping the project in step with what the user does
//! in it.
//!
//! The windows themselves belong to the devices' controllers, and close
//! when a controller is dropped. The session drops a controller whenever
//! it lets go of a device (the device is removed, replaced or created
//! again, another project is loaded, the session ends), so an editor
//! never outlives its device and nothing here has to see to that.

use super::{Session, SessionError};
use trill_core::device::{EditorError, EditorImage, EditorOptions, ParamEdit, ParamEditKind};
use trill_core::{DeviceId, EditError};

impl Session {
    /// Whether a device has an editor window of its own to open. Cheap: a
    /// view may ask on every frame.
    pub fn device_has_editor(&self, id: DeviceId) -> bool {
        let controller = self.bank.devices.get(&id).and_then(|meta| meta.controller.as_ref());
        controller.is_some_and(|controller| controller.lock().has_editor())
    }

    /// Opens a device's editor in a window of its own, titled and placed
    /// as `options` say. If it is open already, the window is brought to
    /// the front instead.
    pub fn open_device_editor(&mut self, id: DeviceId, options: &EditorOptions) -> Result<(), SessionError> {
        self.project.device(id)?;
        let meta = self.bank.devices.get(&id).ok_or(EditError::DeviceNotFound(id))?;
        if let Some(error) = &meta.error {
            return Err(SessionError::Device(format!("{id} is unavailable: {error}")));
        }
        let opened = match &meta.controller {
            Some(controller) => controller.lock().open_editor(options),
            None => Err(EditorError::NoEditor),
        };
        opened.map_err(|error| SessionError::Device(format!("{id}: {error}")))
    }

    /// Closes a device's editor, if it is open.
    pub fn close_device_editor(&mut self, id: DeviceId) {
        if let Some(controller) = self.bank.devices.get(&id).and_then(|meta| meta.controller.as_ref()) {
            controller.lock().close_editor();
        }
    }

    /// Whether a device's editor is open right now. The user may close the
    /// window at any time, so views ask rather than remember. Cheap.
    pub fn is_device_editor_open(&self, id: DeviceId) -> bool {
        let controller = self.bank.devices.get(&id).and_then(|meta| meta.controller.as_ref());
        controller.is_some_and(|controller| controller.lock().is_editor_open())
    }

    /// The devices whose editors are open right now.
    pub fn open_device_editors(&self) -> Vec<DeviceId> {
        let mut open: Vec<DeviceId> =
            self.bank.devices.keys().copied().filter(|id| self.is_device_editor_open(*id)).collect();
        open.sort();
        open
    }

    /// The pixels of a device's open editor, as it is drawn right now.
    pub fn capture_device_editor(&mut self, id: DeviceId) -> Option<EditorImage> {
        let controller = self.bank.devices.get(&id)?.controller.as_ref()?;
        controller.lock().capture_editor()
    }

    /// Collects what the user did in the devices' editors since the last
    /// call, and brings the project in step with it. Call this regularly
    /// (where [`Session::housekeeping`] is called) and before saving.
    ///
    /// A plugin keeps its own parameter values, and the project stores
    /// only those the user set through the DAW, which then count for more
    /// than what the plugin reports. An edit made in the plugin's editor
    /// to such a parameter would be hidden behind the stored value, and
    /// undone by it the next time the project is loaded. So the stored
    /// value follows the edit.
    ///
    /// Like plugin state, this is bookkeeping and not an edit of the
    /// project: it creates no undo step, and the history is corrected
    /// along with the present, or undoing something unrelated would turn
    /// the knob back.
    ///
    /// The edits are returned in the order each device made them, with
    /// the start and end of each gesture, for whoever records automation.
    pub fn absorb_editor_edits(&mut self) -> Vec<(DeviceId, ParamEdit)> {
        let mut absorbed = Vec::new();
        for (id, meta) in &self.bank.devices {
            let Some(controller) = &meta.controller else { continue };
            for edit in controller.lock().take_param_edits() {
                absorbed.push((*id, edit));
                let (ParamEditKind::Value(value), Some(info)) = (edit.kind, meta.params.get(edit.index)) else {
                    continue;
                };
                let Some(held) = self.project.device_mut(*id).ok().and_then(|device| device.params.get_mut(&info.id))
                else {
                    continue;
                };
                let stale = std::mem::replace(held, value);
                for snapshot in self.undo.iter_mut().chain(self.redo.iter_mut()) {
                    if let Some(old) = snapshot.device_mut(*id).ok().and_then(|device| device.params.get_mut(&info.id))
                        && *old == stale
                    {
                        *old = value;
                    }
                }
            }
        }
        // A knob turned in an editor changes what saving would write,
        // whether or not the project stores that parameter.
        if absorbed.iter().any(|(_, edit)| matches!(edit.kind, ParamEditKind::Value(_))) {
            self.revision += 1;
        }
        absorbed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AudioBackend;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use trill_core::device::{
        DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance,
        DeviceProcessor, Event, ParamInfo, ParamKind, ProcessInfo,
    };
    use trill_core::{ChainRef, Device};
    use trill_dsp::BuiltinFactory;

    const URI: &str = "test:with-editor";
    const PLAIN_URI: &str = "test:without-editor";

    /// What the controllers of a [`Plugins`] factory did, by device.
    #[derive(Default)]
    struct Seen {
        /// The titles of the editors that are open, with their devices.
        open: Vec<(DeviceId, String)>,
        /// Every editor ever opened: device and title.
        opened: Vec<(DeviceId, String)>,
        /// Requests to open an editor that was already open.
        raised: usize,
        /// Edits waiting to be collected, as the "user" made them.
        edits: Vec<(DeviceId, ParamEdit)>,
    }

    /// A stand-in for a plugin host: devices with two parameters and,
    /// under one of its two URIs, an editor that is a line in a list.
    #[derive(Clone, Default)]
    struct Plugins(Arc<Mutex<Seen>>);

    struct Silent;

    impl DeviceProcessor for Silent {
        fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}
        fn set_param(&mut self, _index: usize, _value: f64) {}
        fn process(&mut self, _: &ProcessInfo, _: &[Event], _: &mut [f32], _: &mut [f32]) {}
        fn reset(&mut self) {}
    }

    struct Controller {
        device: DeviceId,
        has_editor: bool,
        seen: Arc<Mutex<Seen>>,
    }

    impl DeviceController for Controller {
        fn save_state(&mut self) -> Option<Vec<u8>> {
            None
        }

        fn has_editor(&self) -> bool {
            self.has_editor
        }

        fn open_editor(&mut self, options: &EditorOptions) -> Result<(), EditorError> {
            if !self.has_editor {
                return Err(EditorError::NoEditor);
            }
            if options.title == "fails" {
                return Err(EditorError::Failed("scripted to fail".into()));
            }
            let mut seen = self.seen.lock();
            if seen.open.iter().any(|(device, _)| *device == self.device) {
                seen.raised += 1;
            } else {
                seen.open.push((self.device, options.title.clone()));
                seen.opened.push((self.device, options.title.clone()));
            }
            Ok(())
        }

        fn close_editor(&mut self) {
            self.seen.lock().open.retain(|(device, _)| *device != self.device);
        }

        fn is_editor_open(&self) -> bool {
            self.seen.lock().open.iter().any(|(device, _)| *device == self.device)
        }

        fn capture_editor(&mut self) -> Option<EditorImage> {
            self.is_editor_open().then(|| EditorImage { width: 1, height: 1, rgba: vec![1, 2, 3, 255] })
        }

        fn take_param_edits(&mut self) -> Vec<ParamEdit> {
            let mut seen = self.seen.lock();
            let (mine, others) = seen.edits.drain(..).partition(|(device, _)| *device == self.device);
            seen.edits = others;
            mine.into_iter().map(|(_, edit)| edit).collect()
        }
    }

    impl Drop for Controller {
        /// As a real host does: the editor goes with its controller.
        fn drop(&mut self) {
            self.close_editor();
        }
    }

    impl DeviceFactory for Plugins {
        fn descriptors(&self) -> Vec<DeviceDescriptor> {
            [URI, PLAIN_URI]
                .map(|uri| DeviceDescriptor {
                    uri: uri.into(),
                    name: "Plugin".into(),
                    vendor: "Test".into(),
                    category: DeviceCategory::Effect,
                    description: String::new(),
                    params: Vec::new(),
                    presets: Vec::new(),
                })
                .into()
        }

        fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
            if device.uri != URI && device.uri != PLAIN_URI {
                return Err(DeviceError::Unknown(device.uri.clone()));
            }
            let param = |id: &str| ParamInfo {
                id: id.into(),
                name: id.into(),
                min: 0.0,
                max: 1.0,
                default: 0.2,
                unit: String::new(),
                kind: ParamKind::Float,
            };
            let controller = Controller { device: device.id, has_editor: device.uri == URI, seen: self.0.clone() };
            Ok(DeviceInstance {
                processor: Box::new(Silent),
                params: vec![param("cutoff"), param("drive")],
                controller: Some(Box::new(controller)),
            })
        }
    }

    fn session(plugins: &Plugins) -> Session {
        let factory =
            trill_core::device::CompositeFactory::new(vec![Box::new(BuiltinFactory), Box::new(plugins.clone())]);
        Session::new(Arc::new(factory), AudioBackend::Null { sample_rate: 48_000 }).unwrap()
    }

    fn titled(title: &str) -> EditorOptions {
        EditorOptions { title: title.into(), ..EditorOptions::default() }
    }

    fn open_titles(plugins: &Plugins) -> Vec<String> {
        plugins.0.lock().open.iter().map(|(_, title)| title.clone()).collect()
    }

    #[test]
    fn an_editor_is_opened_raised_and_closed_by_device() {
        let plugins = Plugins::default();
        let mut session = session(&plugins);
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        assert!(session.device_has_editor(device));
        assert!(!session.is_device_editor_open(device));
        assert_eq!(session.capture_device_editor(device), None);

        session.open_device_editor(device, &titled("Plugin on Master")).unwrap();
        assert!(session.is_device_editor_open(device));
        assert_eq!(session.open_device_editors(), [device]);
        assert_eq!(open_titles(&plugins), ["Plugin on Master"]);
        assert!(session.capture_device_editor(device).is_some());

        // Opening it again asks the device to bring its window forward.
        session.open_device_editor(device, &titled("Another title")).unwrap();
        assert_eq!((open_titles(&plugins), plugins.0.lock().raised), (vec!["Plugin on Master".to_string()], 1));

        session.close_device_editor(device);
        assert!(!session.is_device_editor_open(device));
        assert!(session.open_device_editors().is_empty());
        session.close_device_editor(device);
    }

    #[test]
    fn devices_without_an_editor_and_failures_are_errors_with_a_reason() {
        let plugins = Plugins::default();
        let mut session = session(&plugins);
        let plain = session.add_device(ChainRef::Master, PLAIN_URI, None, None).unwrap();
        // The limiter every project starts with: a built-in device, which
        // has no controller at all.
        let builtin = session.project().master.devices[0].id;
        for device in [plain, builtin] {
            assert!(!session.device_has_editor(device));
            let error = session.open_device_editor(device, &titled("x")).unwrap_err();
            assert!(error.to_string().contains("no editor"), "{error}");
            assert!(!session.is_device_editor_open(device));
            session.close_device_editor(device);
        }

        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        let error = session.open_device_editor(device, &titled("fails")).unwrap_err();
        assert!(error.to_string().contains("scripted to fail"), "{error}");
        assert!(!session.is_device_editor_open(device));

        // A device that is not in the project.
        session.undo();
        assert!(matches!(session.open_device_editor(device, &titled("x")), Err(SessionError::Edit(_))));
        assert!(!session.device_has_editor(device) && !session.is_device_editor_open(device));
    }

    #[test]
    fn an_editor_closes_when_the_session_lets_go_of_its_device() {
        let plugins = Plugins::default();
        let mut session = session(&plugins);
        let add = |session: &mut Session, title: &str| {
            let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
            session.open_device_editor(device, &titled(title)).unwrap();
            device
        };

        // Removed from the project.
        let removed = add(&mut session, "removed");
        let kept = add(&mut session, "kept");
        session.edit(|project| project.remove_device(removed)).unwrap();
        assert_eq!(open_titles(&plugins), ["kept"]);
        assert_eq!(session.open_device_editors(), [kept]);

        // Created again from another state: a new instance, without the
        // old one's window.
        session.set_device_state(kept, Some(b"another state".to_vec())).unwrap();
        assert_eq!(open_titles(&plugins), [] as [&str; 0]);
        assert!(!session.is_device_editor_open(kept));
        session.open_device_editor(kept, &titled("recreated")).unwrap();

        // Undone out of existence, and brought back by redo: the device
        // returns, its editor does not.
        let undone = add(&mut session, "undone");
        session.undo();
        assert_eq!(open_titles(&plugins), ["recreated"]);
        session.redo();
        assert!(session.device_has_editor(undone) && !session.is_device_editor_open(undone));

        // Another project.
        session.new_project();
        assert_eq!(open_titles(&plugins), [] as [&str; 0]);

        // The end of the session.
        add(&mut session, "last");
        assert_eq!(open_titles(&plugins), ["last"]);
        drop(session);
        assert_eq!(open_titles(&plugins), [] as [&str; 0]);
        assert_eq!(plugins.0.lock().opened.len(), 5);
    }

    #[test]
    fn edits_made_in_an_editor_are_collected_and_stored_values_follow_them() {
        let plugins = Plugins::default();
        let mut session = session(&plugins);
        let device = session.add_device(ChainRef::Master, URI, None, None).unwrap();
        // The user set the cutoff in the DAW; the drive was never touched.
        session.set_device_params(device, &[("cutoff".into(), 0.4)]).unwrap();
        session.add_device(ChainRef::Master, PLAIN_URI, None, None).unwrap();
        assert!(session.absorb_editor_edits().is_empty());
        let revision = session.revision();

        // Then they turn both knobs in the plugin's editor.
        let edit = |index, kind| (device, ParamEdit { index, kind });
        let gesture = vec![
            edit(0, ParamEditKind::Begin),
            edit(0, ParamEditKind::Value(0.6)),
            edit(0, ParamEditKind::Value(0.8)),
            edit(0, ParamEditKind::End),
            edit(1, ParamEditKind::Value(0.9)),
            // Not a parameter the device lists: reported, nothing to store.
            edit(7, ParamEditKind::Value(0.1)),
        ];
        plugins.0.lock().edits = gesture.clone();
        assert_eq!(session.absorb_editor_edits(), gesture, "in order, with the gesture");
        assert!(session.absorb_editor_edits().is_empty(), "edits are collected once");

        // The value the project stored follows the edit; a parameter it
        // did not store stays the plugin's own business.
        let stored = |session: &Session| session.project().device(device).unwrap().params.clone();
        assert_eq!(stored(&session).get("cutoff"), Some(&0.8));
        assert_eq!(stored(&session).get("drive"), None);
        assert_eq!(session.device_params(device).unwrap()[0].value, 0.8);
        assert!(session.revision() > revision, "views are told that something changed");

        // It was no edit of the project: undoing the step before it (the
        // second device) does not turn the knob back, and undoing the
        // step that stored the cutoff removes the stored value as before.
        assert!(session.undo());
        assert_eq!(stored(&session).get("cutoff"), Some(&0.8));
        assert!(session.undo());
        assert_eq!(stored(&session).get("cutoff"), None);
        assert!(session.redo());
        assert_eq!(stored(&session).get("cutoff"), Some(&0.8), "the history was corrected along with the present");
    }
}
