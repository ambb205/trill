//! Plugin editors: the button on a plugin's panel and on its chip in the
//! mixer, against a stand-in for a plugin whose "window" is an entry in a
//! list. What a real plugin's window does is tested where plugins are
//! hosted; this is the window's side of it.

use super::*;
use trill_core::Device;
use trill_core::device::{
    DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    EditorError, EditorOptions, Event as DeviceEvent, ParamInfo, ParamKind, ProcessInfo,
};

const WITH_EDITOR: &str = "test:synth";
const WITHOUT_EDITOR: &str = "test:gain";

/// What the stand-in plugins did.
#[derive(Default)]
struct Seen {
    /// The editors that are open, each with what it was opened with.
    open: Vec<(DeviceId, EditorOptions)>,
    /// How many editors were ever opened.
    opened: usize,
    /// How often an open editor was asked for again.
    raised: usize,
    /// The next editor asked for does not come up, for this reason.
    fail_next: Option<String>,
}

/// A stand-in for the plugin host: two devices, one with an editor.
#[derive(Clone, Default)]
struct Plugins(Arc<Mutex<Seen>>);

impl Plugins {
    /// The titles of the open editors.
    fn titles(&self) -> Vec<String> {
        self.0.lock().open.iter().map(|(_, options)| options.title.clone()).collect()
    }

    /// How many editors were ever opened, and how often one that was open
    /// was asked for again.
    fn counts(&self) -> (usize, usize) {
        let seen = self.0.lock();
        (seen.opened, seen.raised)
    }

    /// The user closes an editor with the window's own close button.
    fn user_closes(&self, device: DeviceId) {
        self.0.lock().open.retain(|(open, _)| *open != device);
    }
}

struct Silent;

impl DeviceProcessor for Silent {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}
    fn set_param(&mut self, _index: usize, _value: f64) {}
    fn process(&mut self, _: &ProcessInfo, _: &[DeviceEvent], _: &mut [f32], _: &mut [f32]) {}
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
        let mut seen = self.seen.lock();
        if let Some(reason) = seen.fail_next.take() {
            return Err(EditorError::Failed(reason));
        }
        if seen.open.iter().any(|(device, _)| *device == self.device) {
            seen.raised += 1;
        } else {
            seen.open.push((self.device, options.clone()));
            seen.opened += 1;
        }
        Ok(())
    }

    fn close_editor(&mut self) {
        self.seen.lock().open.retain(|(device, _)| *device != self.device);
    }

    fn is_editor_open(&self) -> bool {
        self.seen.lock().open.iter().any(|(device, _)| *device == self.device)
    }
}

impl Drop for Controller {
    /// As with a real plugin: the editor goes with its device.
    fn drop(&mut self) {
        self.close_editor();
    }
}

impl DeviceFactory for Plugins {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        [(WITH_EDITOR, "Synth"), (WITHOUT_EDITOR, "Gain")]
            .map(|(uri, name)| DeviceDescriptor {
                uri: uri.into(),
                name: name.into(),
                vendor: "Test".into(),
                category: DeviceCategory::Effect,
                description: String::new(),
                params: Vec::new(),
                presets: Vec::new(),
            })
            .into()
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != WITH_EDITOR && device.uri != WITHOUT_EDITOR {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        let param = ParamInfo {
            id: "level".into(),
            name: "Level".into(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            unit: String::new(),
            kind: ParamKind::Float,
        };
        let has_editor = device.uri == WITH_EDITOR;
        Ok(DeviceInstance {
            processor: Box::new(Silent),
            params: vec![param],
            controller: Some(Box::new(Controller { device: device.id, has_editor, seen: self.0.clone() })),
        })
    }
}

/// A window on a track called Lead that holds a plugin with an editor, one
/// without and a built-in reverb. Returns the plugin with the editor.
fn with_plugins(options: UiOptions) -> (Harness, Plugins, DeviceId) {
    let plugins = Plugins::default();
    let runtime = Runtime::start_with(RuntimeOptions::hermetic(), vec![Box::new(plugins.clone())]).unwrap();
    let mut synth = None;
    let harness = Harness::on(Arc::new(runtime), options, |session| {
        let track = session.edit(|project| Ok(project.add_track("Lead"))).unwrap();
        synth = Some(session.add_device(ChainRef::Track(track), WITH_EDITOR, None, None).unwrap());
        session.add_device(ChainRef::Track(track), WITHOUT_EDITOR, None, None).unwrap();
        session.add_device(ChainRef::Track(track), "builtin:reverb", None, None).unwrap();
    });
    (harness, plugins, synth.unwrap())
}

/// Lets the workers finish what was asked of them, and the window see it.
fn settle_editors(harness: &mut Harness) {
    assert!(harness.app.sounds().wait_until_idle(Duration::from_secs(10)), "the workers never finished");
    harness.idle(3);
    assert!(!harness.app.editors().is_busy());
}

const BUTTON: &str = "Editor of Synth";

#[test]
fn a_plugin_that_was_just_made_says_that_it_is_loading_and_a_built_in_device_does_not() {
    let (mut harness, _plugins, synth) = with_plugins(devices_tab());
    // A plugin that was made, or given another sound, a moment ago may be
    // reading what it plays. (Told again here, so that the test does not
    // depend on how long the window took to come up.) The reverb is built
    // in, and ready when it exists.
    harness.session.lock().load_watch().began(synth);
    harness.idle(2);
    assert_eq!(harness.all_labeled("Loading Synth").len(), 1);
    assert!(harness.all_labeled("Loading Reverb").is_empty());
    harness.move_to(harness.labeled("Loading Synth"));
    harness.idle(1);
    assert!(harness.hint().contains("Synth is still loading what it plays"), "{}", harness.hint());
    // The button for its own window is still there, after the word.
    assert_eq!(harness.all_labeled("Editor of").len(), 1);
}

#[test]
fn a_plugins_panel_has_a_button_that_opens_and_closes_its_own_window() {
    let (mut harness, plugins, synth) = with_plugins(devices_tab());
    // Only the device that has an editor has the button.
    assert_eq!(harness.all_labeled("Editor of").len(), 1);
    assert!(!harness.is_on(BUTTON));
    let (before, revision) = (harness.undo_depth(), harness.session.lock().revision());

    harness.move_to(harness.labeled(BUTTON));
    harness.idle(1);
    assert!(harness.hint().starts_with("Open the editor of Synth"), "{}", harness.hint());
    harness.click_label(BUTTON);
    settle_editors(&mut harness);
    assert_eq!(plugins.titles(), ["Synth on Lead"], "named for the device and where it sits");
    {
        let seen = plugins.0.lock();
        let (device, options) = &seen.open[0];
        assert_eq!(*device, synth);
        assert_eq!(options.position, None, "where it was last, or where the system puts it");
        assert!(options.activate, "someone clicked: the editor takes the keyboard");
        assert_eq!(options.owner, None, "a run without a window has none to own it");
    }
    assert!(harness.is_on(BUTTON), "the button shows that the window is open");
    harness.move_to(harness.labeled(BUTTON));
    harness.idle(1);
    assert!(harness.hint().contains("is open"), "{}", harness.hint());
    // (Counting the undo steps walks the history, which moves the revision.)
    assert_eq!(harness.session.lock().revision(), revision, "opening a window edits nothing");
    assert_eq!(harness.undo_depth(), before);

    // The user closes the window with its own close button: the button
    // follows, without being told.
    plugins.user_closes(synth);
    harness.idle(2);
    assert!(!harness.is_on(BUTTON));

    // The button closes it as well as opens it.
    harness.settle();
    harness.click_label(BUTTON);
    settle_editors(&mut harness);
    assert!(harness.is_on(BUTTON));
    harness.settle();
    harness.click_label(BUTTON);
    settle_editors(&mut harness);
    assert!(!harness.is_on(BUTTON));
    assert!(plugins.titles().is_empty());
    assert_eq!(plugins.counts(), (2, 0));
    assert_eq!(harness.app.state().toasts.len(), 0);
}

#[test]
fn an_editor_that_does_not_open_says_why_and_one_that_is_on_its_way_waits() {
    let (mut harness, plugins, _) = with_plugins(devices_tab());
    plugins.0.lock().fail_next = Some("the plugin would not show it".into());
    harness.click_label(BUTTON);
    settle_editors(&mut harness);
    assert!(plugins.titles().is_empty());
    assert!(!harness.is_on(BUTTON));
    assert_eq!(harness.app.state().toasts.len(), 1, "the failure is told, as every failure is");

    // A plugin can take seconds to build its window. Meanwhile the button
    // shows it as on its way, and more clicks ask for nothing more.
    harness.app.sounds().hold(true);
    harness.settle();
    harness.click_label(BUTTON);
    assert!(harness.app.editors().is_busy());
    harness.idle(1);
    assert!(harness.is_on(BUTTON), "lit at once: the click was taken");
    harness.move_to(harness.labeled(BUTTON));
    harness.idle(1);
    assert!(harness.hint().starts_with("Opening the editor of Synth"), "{}", harness.hint());
    harness.settle();
    harness.click_label(BUTTON);
    harness.settle();
    harness.click_label(BUTTON);
    harness.app.sounds().hold(false);
    settle_editors(&mut harness);
    assert_eq!(plugins.titles(), ["Synth on Lead"]);
    assert_eq!(plugins.counts(), (1, 0), "asked for once");
    assert!(harness.is_on(BUTTON));
}

#[test]
fn an_editor_goes_with_its_device_and_its_button_with_it() {
    let (mut harness, plugins, synth) = with_plugins(devices_tab());
    harness.click_label(BUTTON);
    settle_editors(&mut harness);
    assert_eq!(plugins.titles().len(), 1);

    // The cross on the plugin's panel removes the device; the session
    // closes its editor, and there is no button left to show one.
    harness.session.lock().edit(|project| project.remove_device(synth)).unwrap();
    harness.idle(2);
    assert!(plugins.titles().is_empty());
    assert!(!harness.is_on_screen(BUTTON));
    // Undo brings the device back, and its button, with the window shut.
    harness.key(Modifiers::COMMAND, Key::Z);
    harness.idle(2);
    assert!(harness.is_on_screen(BUTTON) && !harness.is_on(BUTTON));
    assert!(plugins.titles().is_empty());
}

#[test]
fn the_mixer_opens_an_editor_from_the_end_of_a_plugins_chip() {
    let (mut harness, plugins, synth) = with_plugins(UiOptions { detail_tab: DetailTab::Mixer, ..Default::default() });
    // One button: beside the chip of the one device that has an editor.
    let buttons = harness.all_labeled("Editor");
    assert_eq!(buttons.len(), 1);
    let chip = harness.all_labeled("Synth in the mixer")[0];
    let plain = harness.all_labeled("Gain in the mixer")[0];
    assert_eq!(buttons[0].y_range(), chip.y_range(), "at the end of the plugin's chip");
    assert!(
        buttons[0].left() > chip.right() && buttons[0].right() == plain.right(),
        "{chip:?} {:?} {plain:?}",
        buttons[0]
    );
    assert!(!harness.is_on("Editor"));

    harness.click_label("Editor");
    settle_editors(&mut harness);
    assert_eq!(plugins.titles(), ["Synth on Lead"]);
    assert!(harness.is_on("Editor"));
    assert_eq!(harness.app.state().detail_tab, DetailTab::Mixer, "the mixer stays: the button is not the chip");

    // The chip itself still shows the device in the Devices tab, where the
    // panel's button says the same.
    harness.click_label("Synth in the mixer");
    assert_eq!(harness.app.state().detail_tab, DetailTab::Devices);
    assert_eq!(harness.app.state().selected_device, Some(synth));
    harness.idle(2);
    assert!(harness.is_on(BUTTON));
    harness.click_label(BUTTON);
    settle_editors(&mut harness);
    assert!(plugins.titles().is_empty());
}

#[test]
fn an_editor_can_be_asked_for_at_startup_and_an_unattended_run_leaves_the_keyboard_alone() {
    // What `--editor Synth --exit-after ...` does.
    let options =
        UiOptions { open_editor: Some("Synth".into()), exit_after: Some(Duration::from_secs(3600)), ..devices_tab() };
    let (mut harness, plugins, _) = with_plugins(options);
    settle_editors(&mut harness);
    assert_eq!(plugins.titles(), ["Synth on Lead"]);
    assert!(!plugins.0.lock().open[0].1.activate, "nobody is sitting at an unattended run");
    assert!(harness.is_on(BUTTON));

    // A name that is no device, or a device without an editor, says so.
    for name in ["Nothing", "Reverb"] {
        let (mut harness, plugins, _) = with_plugins(UiOptions { open_editor: Some(name.into()), ..devices_tab() });
        settle_editors(&mut harness);
        assert!(plugins.titles().is_empty());
        assert_eq!(harness.app.state().toasts.len(), 1, "{name}");
    }
}
