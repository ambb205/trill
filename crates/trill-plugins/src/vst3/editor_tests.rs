//! The host against the views of the scripted plugins: the whole life of
//! an editor window, and everything that can go wrong with one.
//!
//! The windows are real ones, made and serviced exactly as for an installed
//! plugin, except that this crate's unit tests never put a window on
//! screen. What a user would do with the mouse is done to them by
//! [`simulate`].

use super::Vst3Factory;
use super::scripted::{self, Action, ViewSeen, kind};
use crate::editor::{Size, simulate};
use crate::plugin_thread::plugin_thread;
use crate::test_support::{eventually, unique_title, within};
use std::time::Duration;
use trill_core::device::{
    DeviceFactory, DeviceInstance, EditorError, EditorOptions, ParamEdit, ParamEditKind, ProcessInfo,
};
use trill_core::{ChainRef, Device, Project};
use vst3::Steinberg::Vst::ParamID;
use vst3::Steinberg::kResultOk;

const SR: f64 = 48_000.0;
const BLOCK: usize = 64;
/// Longer than anything here takes unless it hangs.
const WAIT: Duration = Duration::from_secs(10);

fn factory() -> Vst3Factory {
    Vst3Factory::with_modules(vec![("scripted", scripted::factory())])
}

fn device(kind: u8) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, scripted::uri(kind), "plugin", None).unwrap();
    project.device(id).unwrap().clone()
}

fn create(factory: &Vst3Factory, kind: u8) -> DeviceInstance {
    factory.create(&device(kind), SR, BLOCK).unwrap_or_else(|error| panic!("{error}"))
}

fn index(instance: &DeviceInstance, id: ParamID) -> usize {
    let id = id.to_string();
    instance.params.iter().position(|param| param.id == id).unwrap_or_else(|| panic!("no parameter {id}"))
}

/// The value the plugin's controller currently reports for parameter `id`.
fn value(instance: &mut DeviceInstance, id: ParamID) -> f64 {
    let index = index(instance, id);
    instance.controller.as_mut().unwrap().param_values().expect("the plugin reports values")[index]
}

fn serial(instance: &mut DeviceInstance) -> u64 {
    (value(instance, scripted::SERIAL) * 1e6).round() as u64
}

fn view(serial: u64) -> ViewSeen {
    scripted::view_seen(serial).expect("the plugin has made a view")
}

/// Options for an editor nobody is meant to see, under a title of its own.
fn options(what: &str) -> EditorOptions {
    EditorOptions { title: unique_title(what), activate: false, ..EditorOptions::default() }
}

/// Opens the instance's editor and returns its window.
fn open(instance: &mut DeviceInstance, options: &EditorOptions) -> isize {
    instance.controller.as_mut().unwrap().open_editor(options).unwrap_or_else(|error| panic!("{error}"));
    simulate::find(&options.title).expect("the editor has a window with the title it was opened with")
}

fn is_open(instance: &DeviceInstance) -> bool {
    instance.controller.as_ref().unwrap().is_editor_open()
}

/// The size of the view of the instance, as a window size.
fn view_size(serial: u64) -> Size {
    let (width, height) = view(serial).size;
    Size::new(width as u32, height as u32)
}

/// The view's natural size at the scale the host gave it.
fn scaled_size(serial: u64) -> Size {
    let scaled = |length: i32| (length as f32 * view(serial).scale).round() as u32;
    Size::new(scaled(scripted::VIEW_SIZE.0), scaled(scripted::VIEW_SIZE.1))
}

/// Processes one block of `fill` on both sides and returns the left one.
fn block(instance: &mut DeviceInstance, fill: f32) -> Vec<f32> {
    let info = ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        loop_range: None,
    };
    let (mut left, mut right) = (vec![fill; BLOCK], vec![fill; BLOCK]);
    instance.processor.process(&info, &[], &mut left, &mut right);
    left
}

// ---- opening and closing ---------------------------------------------------

#[test]
fn an_editor_opens_in_a_window_of_the_views_size_and_closes_again() {
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    assert!(instance.controller.as_ref().unwrap().has_editor());
    assert!(!is_open(&instance));
    assert_eq!(scripted::view_steps(serial), [] as [&str; 0], "no view is made until the editor is opened");

    let window = open(&mut instance, &options("opens"));
    assert!(is_open(&instance));
    // The view was given a frame and the scale of the window's monitor
    // before it was asked for its size, and the window is exactly that
    // size.
    assert_eq!(scripted::view_steps(serial), ["create", "frame set", "scale", "attached"]);
    let seen = view(serial);
    assert!(seen.scale >= 1.0, "the scale of a monitor, not {}", seen.scale);
    assert_eq!(simulate::client_size(window), scaled_size(serial));
    assert_eq!(seen.parent, window as usize, "the view was put into that window");
    assert!(!simulate::is_visible(window), "unit tests put no window on screen");

    instance.controller.as_mut().unwrap().close_editor();
    assert!(!is_open(&instance));
    assert!(!simulate::exists(window));
    // Taken out of its window, relieved of the frame, and released.
    assert_eq!(scripted::view_steps(serial)[4..], ["removed", "frame cleared", "destroy"]);

    // The plugin is none the worse for it, and closing again is nothing.
    instance.controller.as_mut().unwrap().close_editor();
    assert_eq!(block(&mut instance, 0.5), vec![0.5; BLOCK]);
    assert_eq!(scripted::view_steps(serial).len(), 7);
}

#[test]
fn opening_an_open_editor_keeps_the_one_window() {
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let options = options("twice");
    let window = open(&mut instance, &options);
    assert_eq!(open(&mut instance, &options), window);
    let other = EditorOptions { title: unique_title("another title"), ..options };
    instance.controller.as_mut().unwrap().open_editor(&other).unwrap();
    assert_eq!(simulate::find(&other.title), None);
    assert!(simulate::exists(window) && is_open(&instance));
    assert_eq!(scripted::view_steps(serial).iter().filter(|step| **step == "create").count(), 1);
}

#[test]
fn the_user_closing_the_window_detaches_the_view_and_the_window_remembers_its_place() {
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let window = open(&mut instance, &EditorOptions { position: Some((260, 150)), ..options("placed") });
    assert_eq!(simulate::position(window), (260, 150));

    simulate::close(window);
    assert!(eventually(WAIT, || !is_open(&instance)), "the editor closes when its window is closed");
    assert!(!simulate::exists(window));
    assert_eq!(scripted::view_steps(serial)[4..], ["removed", "frame cleared", "destroy"]);

    // Opened again without saying where, it is where it was, with a view
    // made afresh.
    let again = open(&mut instance, &options("again"));
    assert_eq!(simulate::position(again), (260, 150));
    assert_eq!(scripted::view_steps(serial)[7..], ["create", "frame set", "scale", "attached"]);
}

#[test]
fn an_editor_goes_with_its_controller_and_with_its_device() {
    let factory = factory();

    // The session lets go of the controller when the device leaves the
    // project; the processor follows later, from the audio thread.
    let mut instance = create(&factory, kind::EDITOR);
    let first = serial(&mut instance);
    let window = open(&mut instance, &options("controller goes"));
    instance.controller = None;
    assert!(!simulate::exists(window));
    assert_eq!(scripted::view_steps(first)[4..], ["removed", "frame cleared", "destroy"]);
    assert_eq!(block(&mut instance, 0.5), vec![0.5; BLOCK], "the plugin itself is still there");

    // The other way around: the plugin goes while its editor is open. The
    // view is taken down before the plugin is.
    let mut instance = create(&factory, kind::EDITOR);
    let second = serial(&mut instance);
    let window = open(&mut instance, &options("device goes"));
    let DeviceInstance { processor, controller, .. } = instance;
    drop(processor);
    assert!(!simulate::exists(window));
    assert!(!controller.as_ref().unwrap().is_editor_open());
    assert_eq!(scripted::view_steps(second)[4..], ["removed", "frame cleared", "destroy"]);
    let component = scripted::lifecycle(second);
    assert_eq!(component[component.len() - 2..], ["terminate", "destroy"]);
    // The controller that is left has nothing to open or close.
    let mut controller = controller.unwrap();
    assert!(matches!(controller.open_editor(&options("gone")), Err(EditorError::Failed(_))));
    controller.close_editor();
}

#[test]
fn several_editors_are_open_at_once_and_none_outlives_the_factory() {
    within(Duration::from_secs(60), || {
        let factory = factory();
        let mut instances: Vec<DeviceInstance> = (0..3).map(|_| create(&factory, kind::EDITOR)).collect();
        let windows: Vec<isize> =
            instances.iter_mut().map(|instance| open(instance, &options("one of several"))).collect();
        assert!(windows.iter().all(|window| simulate::exists(*window)));
        assert!(windows[0] != windows[1] && windows[1] != windows[2]);

        // The factory goes first, as when an application shuts down in
        // whatever order its parts happen to be dropped.
        drop(factory);
        assert!(instances.iter().all(is_open), "devices and their editors do not need the factory");
        drop(instances);
        assert!(windows.iter().all(|window| !simulate::exists(*window)), "no window is left behind");
    });
}

// ---- editors that do not open ------------------------------------------------

#[test]
fn a_view_that_cannot_be_attached_is_an_error_and_leaves_no_window() {
    let factory = factory();
    let mut failing = create(&factory, kind::FAILS_ATTACH);
    let serial = serial(&mut failing);
    let options = options("fails");
    let error = failing.controller.as_mut().unwrap().open_editor(&options).unwrap_err();
    assert!(matches!(&error, EditorError::Failed(reason) if reason.contains("window")), "{error}");
    assert!(!is_open(&failing));
    assert_eq!(simulate::find(&options.title), None, "the window made for it is gone again");
    // The view was never in a window, so it is not taken out of one; it is
    // relieved of the frame and released.
    assert_eq!(scripted::view_steps(serial), ["create", "frame set", "scale", "frame cleared", "destroy"]);
    // It can be asked again, and still processes.
    assert!(failing.controller.as_ref().unwrap().has_editor());
    assert!(failing.controller.as_mut().unwrap().open_editor(&options).is_err());
    assert_eq!(block(&mut failing, 0.5), vec![0.5; BLOCK]);
}

#[test]
fn a_plugin_without_a_view_turns_out_to_have_no_editor() {
    let factory = factory();
    // VST3 cannot be asked in advance: a plugin with an edit controller
    // may have a view, and says so when it is asked for one.
    let mut instance = create(&factory, kind::MONO_GAIN);
    let controller = instance.controller.as_mut().unwrap();
    assert!(controller.has_editor());
    assert_eq!(controller.open_editor(&options("none")), Err(EditorError::NoEditor));
    assert!(!controller.has_editor(), "nobody needs to be offered it again");
    assert!(!controller.is_editor_open());
    assert!(controller.capture_editor().is_none());

    // A plugin without an edit controller has nothing to make a view.
    let mut bare = create(&factory, kind::BARE);
    let controller = bare.controller.as_mut().unwrap();
    assert!(!controller.has_editor());
    assert_eq!(controller.open_editor(&options("none")), Err(EditorError::NoEditor));
}

// ---- sizes --------------------------------------------------------------------

#[test]
fn the_user_resizes_an_editor_only_to_sizes_its_view_accepts() {
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let window = open(&mut instance, &options("resizable"));
    assert!(simulate::is_resizable(window));

    // While an edge is dragged, the view is asked about every size.
    assert_eq!(simulate::drag_to(window, Size::new(703, 517)), Size::new(700, 510));
    assert_eq!(simulate::drag_to(window, Size::new(50, 50)), Size::new(150, 100), "no smaller than the view allows");
    assert_eq!(simulate::client_size(window), scaled_size(serial), "dragging alone resizes nothing");

    // When the mouse is released, the view is given the size, and the
    // window follows what the view makes of it.
    simulate::resize_to(window, Size::new(703, 517));
    assert!(eventually(WAIT, || view_size(serial) == Size::new(700, 510)));
    assert!(eventually(WAIT, || simulate::client_size(window) == Size::new(700, 510)));
    assert_eq!(scripted::view_steps(serial).last(), Some(&"onSize"));

    // The size the user chose comes back when the editor is opened again.
    instance.controller.as_mut().unwrap().close_editor();
    let again = open(&mut instance, &options("resized before"));
    assert_eq!(simulate::client_size(again), Size::new(700, 510));
    assert_eq!(view_size(serial), Size::new(700, 510));

    // A view of one size gets a window that cannot be resized, at the
    // size it says; it has no interface to be told a scale by.
    let mut fixed = create(&factory, kind::FIXED_EDITOR);
    let fixed_serial = self::serial(&mut fixed);
    let window = open(&mut fixed, &options("fixed"));
    assert!(!simulate::is_resizable(window));
    assert_eq!(scripted::view_steps(fixed_serial), ["create", "frame set", "attached"]);
    let natural = Size::new(scripted::VIEW_SIZE.0 as u32, scripted::VIEW_SIZE.1 as u32);
    assert_eq!(simulate::client_size(window), natural);
}

#[test]
fn a_view_that_asks_for_another_size_gets_it_and_is_told() {
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let window = open(&mut instance, &options("view resizes"));

    // From its own message handling, as when the user picks a zoom level
    // in the plugin: the plugin thread is free, and the window is resized
    // and the view told before its request returns.
    scripted::act(serial, Action::Resize(520, 380));
    assert!(eventually(WAIT, || view(serial).resize_results == [kResultOk]));
    assert_eq!(view_size(serial), Size::new(520, 380), "the host answered with `onSize` during the request");
    assert_eq!(simulate::client_size(window), Size::new(520, 380));
    assert_eq!(scripted::view_steps(serial).last(), Some(&"onSize"));

    // From inside a call the host is making, here a job on the plugin
    // thread: the request is accepted and served as soon as the thread
    // is free.
    let main = plugin_thread().unwrap();
    main.call(WAIT, move |_| scripted::act_now(serial, Action::Resize(440, 300))).unwrap();
    assert_eq!(view(serial).resize_results, [kResultOk, kResultOk]);
    assert!(eventually(WAIT, || simulate::client_size(window) == Size::new(440, 300)));
    assert!(eventually(WAIT, || view_size(serial) == Size::new(440, 300)));

    // A view whose editor is closed has no window to resize. The view
    // object the test still reaches is gone, so nothing is asked at all.
    instance.controller.as_mut().unwrap().close_editor();
    scripted::act_now(serial, Action::Resize(600, 400));
    assert_eq!(view(serial).resize_results.len(), 2);
}

#[test]
fn an_editor_follows_its_window_to_a_monitor_of_another_scale() {
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let window = open(&mut instance, &options("changes monitor"));

    // 192 dots per inch is a scale of 2.
    simulate::change_dpi(window, 192);
    assert!(eventually(WAIT, || view(serial).scale == 2.0));
    let doubled = Size::new(scripted::VIEW_SIZE.0 as u32 * 2, scripted::VIEW_SIZE.1 as u32 * 2);
    assert!(eventually(WAIT, || simulate::client_size(window) == doubled), "{:?}", simulate::client_size(window));
    assert_eq!(view_size(serial), doubled);
}

// ---- edits ---------------------------------------------------------------------

#[test]
fn an_edit_made_in_the_view_is_heard_and_reported_with_its_gesture() {
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let gain = index(&instance, scripted::GAIN);
    open(&mut instance, &options("edits"));
    // The gain is twice the normalized value, which starts at a half.
    assert_eq!(block(&mut instance, 0.5), vec![0.5; BLOCK]);
    assert_eq!(instance.controller.as_mut().unwrap().take_param_edits(), []);

    // The user turns the gain knob in the view. The controller has the
    // value at once; the processor is given it with the next block.
    scripted::act(serial, Action::TurnKnob(0.25));
    assert!(eventually(WAIT, || value(&mut instance, scripted::GAIN) == 0.25));
    assert_eq!(block(&mut instance, 0.5), vec![0.25; BLOCK], "the edit is heard");
    let edit = |kind| ParamEdit { index: gain, kind };
    assert_eq!(
        instance.controller.as_mut().unwrap().take_param_edits(),
        [edit(ParamEditKind::Begin), edit(ParamEditKind::Value(0.25)), edit(ParamEditKind::End)]
    );
    assert_eq!(instance.controller.as_mut().unwrap().take_param_edits(), [], "edits are reported once");
    assert!(view(serial).ticks > 0);
}
