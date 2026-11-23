//! The host against the editors of the scripted plugins: the whole life of
//! an editor window, and everything that can go wrong with one.
//!
//! The windows are real ones, made and serviced exactly as for an installed
//! plugin, except that this crate's unit tests never put a window on
//! screen. What a user would do with the mouse is done to them by
//! [`simulate`].

use super::ClapFactory;
use super::scripted::{self, Action, kind};
use crate::editor::{Size, simulate};
use crate::test_support::{eventually, needs_windows, unique_title, within};
use std::time::Duration;
use trill_core::device::{
    DeviceFactory, DeviceInstance, EditorError, EditorOptions, ParamEdit, ParamEditKind, ProcessInfo,
};
use trill_core::{ChainRef, Device, Project};

const SR: f64 = 48_000.0;
const BLOCK: usize = 64;
/// Longer than anything here takes unless it hangs.
const WAIT: Duration = Duration::from_secs(10);

fn factory() -> ClapFactory {
    ClapFactory::with_entries(&[scripted::entry(kind::MONO_GAIN), scripted::entry(kind::EDITOR)])
}

fn factory_of(kinds: &[u8]) -> ClapFactory {
    let entries: Vec<_> = kinds.iter().map(|kind| scripted::entry(*kind)).collect();
    ClapFactory::with_entries(&entries)
}

fn device(kind: u8) -> Device {
    let mut project = Project::new("t");
    let id = project.add_device(ChainRef::Master, format!("clap:{}", scripted::id(kind)), "plugin", None).unwrap();
    project.device(id).unwrap().clone()
}

fn create(factory: &ClapFactory, kind: u8) -> DeviceInstance {
    factory.create(&device(kind), SR, BLOCK).unwrap_or_else(|error| panic!("{error}"))
}

fn index(instance: &DeviceInstance, id: u32) -> usize {
    let id = id.to_string();
    instance.params.iter().position(|param| param.id == id).unwrap_or_else(|| panic!("no parameter {id}"))
}

/// The value the plugin currently reports for parameter `id`.
fn value(instance: &mut DeviceInstance, id: u32) -> f64 {
    let index = index(instance, id);
    instance.controller.as_mut().unwrap().param_values().expect("the plugin reports values")[index]
}

fn serial(instance: &mut DeviceInstance) -> u64 {
    value(instance, scripted::SERIAL) as u64
}

/// The editor's steps among the lifecycle calls the instance received.
fn editor_steps(serial: u64) -> Vec<&'static str> {
    scripted::lifecycle(serial).into_iter().filter(|step| step.starts_with("gui")).collect()
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

/// The editor's natural size at the scale the host gave it.
fn scaled_size(instance: &mut DeviceInstance) -> Size {
    let scale = value(instance, scripted::SEEN_SCALE);
    let scaled = |length: u32| (f64::from(length) * scale).round() as u32;
    Size::new(scaled(scripted::EDITOR_SIZE.0), scaled(scripted::EDITOR_SIZE.1))
}

/// Processes one block of `fill` on both sides and returns the left one.
fn block(instance: &mut DeviceInstance, fill: f32) -> Vec<f32> {
    let info = ProcessInfo {
        sample_rate: SR,
        tempo: 120.0,
        time_signature: Default::default(),
        playing: true,
        position_beats: 0.0,
        song: Default::default(),
        loop_range: None,
    };
    let (mut left, mut right) = (vec![fill; BLOCK], vec![fill; BLOCK]);
    instance.processor.process(&info, &[], &mut left, &mut right);
    left
}

// ---- opening and closing ---------------------------------------------------

#[test]
fn an_editor_opens_in_a_window_of_the_plugins_size_and_closes_again() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    assert!(instance.controller.as_ref().unwrap().has_editor());
    assert!(!is_open(&instance));
    assert_eq!(editor_steps(serial), [] as [&str; 0], "nothing of the editor exists until it is opened");

    let window = open(&mut instance, &options("opens"));
    assert!(is_open(&instance));
    // The plugin was told the scale of the window's monitor before it was
    // asked for its size, and the window is exactly that size.
    let scale = value(&mut instance, scripted::SEEN_SCALE);
    assert!(scale >= 1.0, "the scale of a monitor, not {scale}");
    assert_eq!(simulate::client_size(window), scaled_size(&mut instance));
    assert_eq!(value(&mut instance, scripted::SEEN_PARENT), window as f64, "the view was put into that window");
    assert_eq!(editor_steps(serial), ["gui create", "gui scale", "gui parent", "gui show"]);
    assert!(!simulate::is_visible(window), "unit tests put no window on screen");

    instance.controller.as_mut().unwrap().close_editor();
    assert!(!is_open(&instance));
    assert!(!simulate::exists(window));
    assert_eq!(editor_steps(serial)[4..], ["gui hide", "gui destroy"]);

    // The plugin is none the worse for it, and closing again is nothing.
    instance.controller.as_mut().unwrap().close_editor();
    assert_eq!(block(&mut instance, 0.5), vec![0.5; BLOCK]);
    assert_eq!(editor_steps(serial).len(), 6);
}

#[test]
fn opening_an_open_editor_keeps_the_one_window() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let options = options("twice");
    let window = open(&mut instance, &options);
    assert_eq!(open(&mut instance, &options), window);
    // Whatever the second request says, the window that is open stays.
    let other = EditorOptions { title: unique_title("another title"), ..options };
    instance.controller.as_mut().unwrap().open_editor(&other).unwrap();
    assert_eq!(simulate::find(&other.title), None);
    assert!(simulate::exists(window) && is_open(&instance));
    assert_eq!(editor_steps(serial).iter().filter(|step| **step == "gui create").count(), 1);
}

#[test]
fn the_user_closing_the_window_detaches_the_view_and_the_window_remembers_its_place() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let placed = EditorOptions { position: Some((180, 140)), ..options("placed") };
    let window = open(&mut instance, &placed);
    assert_eq!(simulate::position(window), (180, 140));

    simulate::close(window);
    assert!(eventually(WAIT, || !is_open(&instance)), "the editor closes when its window is closed");
    assert!(!simulate::exists(window));
    assert_eq!(editor_steps(serial)[4..], ["gui hide", "gui destroy"], "the plugin took its view down");

    // Opened again without saying where, it is where it was.
    let again = open(&mut instance, &options("again"));
    assert_ne!(again, 0);
    assert_eq!(simulate::position(again), (180, 140));
    assert_eq!(editor_steps(serial)[6..], ["gui create", "gui scale", "gui parent", "gui show"]);
    // A caller that says where is obeyed.
    instance.controller.as_mut().unwrap().close_editor();
    let moved = open(&mut instance, &EditorOptions { position: Some((220, 160)), ..options("moved") });
    assert_eq!(simulate::position(moved), (220, 160));
}

#[test]
fn an_editor_goes_with_its_controller_and_with_its_device() {
    needs_windows!();
    let factory = factory();

    // The session lets go of the controller when the device leaves the
    // project; the processor follows later, from the audio thread.
    let mut instance = create(&factory, kind::EDITOR);
    let first = serial(&mut instance);
    let window = open(&mut instance, &options("controller goes"));
    instance.controller = None;
    assert!(!simulate::exists(window));
    assert_eq!(editor_steps(first)[4..], ["gui hide", "gui destroy"]);
    assert_eq!(block(&mut instance, 0.5), vec![0.5; BLOCK], "the plugin itself is still there");

    // The other way around: the plugin goes while its editor is open. The
    // editor is taken down before the plugin is deactivated.
    let mut instance = create(&factory, kind::EDITOR);
    let second = serial(&mut instance);
    let window = open(&mut instance, &options("device goes"));
    let DeviceInstance { processor, controller, .. } = instance;
    drop(processor);
    assert!(!simulate::exists(window));
    assert!(!controller.as_ref().unwrap().is_editor_open());
    let steps = scripted::lifecycle(second);
    assert_eq!(steps[steps.len() - 4..], ["gui hide", "gui destroy", "deactivate", "destroy"]);
    // The controller that is left has nothing to open or close.
    let mut controller = controller.unwrap();
    assert!(matches!(controller.open_editor(&options("gone")), Err(EditorError::Failed(_))));
    controller.close_editor();
}

#[test]
fn several_editors_are_open_at_once_and_none_outlives_the_factory() {
    needs_windows!();
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
fn a_plugin_that_cannot_create_or_attach_its_editor_is_an_error_and_leaves_no_window() {
    needs_windows!();
    let factory = factory_of(&[kind::FAILS_EDITOR_CREATE, kind::FAILS_EDITOR_ATTACH]);

    let mut failing = create(&factory, kind::FAILS_EDITOR_CREATE);
    let options = options("fails");
    let error = failing.controller.as_mut().unwrap().open_editor(&options).unwrap_err();
    assert!(matches!(&error, EditorError::Failed(reason) if reason.contains("create")), "{error}");
    assert!(!is_open(&failing));
    assert_eq!(simulate::find(&options.title), None);

    let mut failing = create(&factory, kind::FAILS_EDITOR_ATTACH);
    let serial = serial(&mut failing);
    let error = failing.controller.as_mut().unwrap().open_editor(&options).unwrap_err();
    assert!(matches!(&error, EditorError::Failed(reason) if reason.contains("window")), "{error}");
    assert!(!is_open(&failing));
    assert_eq!(simulate::find(&options.title), None, "the window made for it is gone again");
    // What the plugin had created was taken down, not left half open.
    assert_eq!(editor_steps(serial), ["gui create", "gui scale", "gui hide", "gui destroy"]);
    // It can be asked again, and still processes.
    assert!(failing.controller.as_mut().unwrap().open_editor(&options).is_err());
    assert_eq!(block(&mut failing, 0.5), vec![0.5; BLOCK]);
}

#[test]
fn a_plugin_without_an_editor_to_embed_has_none() {
    needs_windows!();
    let factory = factory_of(&[kind::MONO_GAIN, kind::FLOATING_EDITOR]);
    // One has no editor at all; the other only a window of its own, which
    // this host does not manage.
    for kind in [kind::MONO_GAIN, kind::FLOATING_EDITOR] {
        let mut instance = create(&factory, kind);
        let controller = instance.controller.as_mut().unwrap();
        assert!(!controller.has_editor(), "kind {kind}");
        assert_eq!(controller.open_editor(&options("none")), Err(EditorError::NoEditor));
        assert!(!controller.is_editor_open());
        assert!(controller.capture_editor().is_none());
        controller.close_editor();
    }
}

// ---- sizes --------------------------------------------------------------------

#[test]
fn the_user_resizes_an_editor_only_to_sizes_its_plugin_accepts() {
    needs_windows!();
    let factory = factory_of(&[kind::EDITOR, kind::FIXED_EDITOR]);
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let window = open(&mut instance, &options("resizable"));
    assert!(simulate::is_resizable(window));

    // While an edge is dragged, Windows asks the plugin about every size (`WM_SIZING`); an X window
    // manager resizes by the size hints and tells the program afterwards, so there is nothing to ask then.
    #[cfg(windows)]
    {
        assert_eq!(simulate::drag_to(window, Size::new(703, 517)), Size::new(700, 510));
        assert_eq!(
            simulate::drag_to(window, Size::new(50, 50)),
            Size::new(200, 150),
            "no smaller than the plugin allows"
        );
        assert_eq!(simulate::client_size(window), scaled_size(&mut instance), "dragging alone resizes nothing");
    }

    // When the mouse is released, the plugin is given the size, and the
    // window follows what the plugin makes of it.
    simulate::resize_to(window, Size::new(703, 517));
    assert!(eventually(WAIT, || value(&mut instance, scripted::EDITOR_WIDTH) == 700.0));
    assert_eq!(value(&mut instance, scripted::EDITOR_HEIGHT), 510.0);
    assert!(eventually(WAIT, || simulate::client_size(window) == Size::new(700, 510)));
    assert_eq!(editor_steps(serial).last(), Some(&"gui size"));

    // The size the user chose comes back when the editor is opened again.
    instance.controller.as_mut().unwrap().close_editor();
    let again = open(&mut instance, &options("resized before"));
    assert_eq!(simulate::client_size(again), Size::new(700, 510));
    assert_eq!(value(&mut instance, scripted::EDITOR_WIDTH), 700.0);

    // An editor of one size gets a window that cannot be resized, at the
    // size it says, though it would not hear of the monitor's scale.
    let mut fixed = create(&factory, kind::FIXED_EDITOR);
    let window = open(&mut fixed, &options("fixed"));
    assert!(!simulate::is_resizable(window));
    assert_eq!(value(&mut fixed, scripted::SEEN_SCALE), 0.0);
    assert_eq!(simulate::client_size(window), Size::new(scripted::EDITOR_SIZE.0, scripted::EDITOR_SIZE.1));
}

#[test]
fn the_plugin_resizes_its_own_editor_and_changes_its_mind_about_resizing() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let window = open(&mut instance, &options("plugin resizes"));

    scripted::act(serial, Action::Resize(520, 380));
    assert!(eventually(WAIT, || simulate::client_size(window) == Size::new(520, 380)));
    assert_eq!(value(&mut instance, scripted::EDITOR_WIDTH), 520.0);
    assert!(!editor_steps(serial).contains(&"gui size"), "a plugin that got the size it asked for is not told it");

    scripted::act(serial, Action::SetResizable(false));
    assert!(eventually(WAIT, || !simulate::is_resizable(window)));
    assert_eq!(simulate::client_size(window), Size::new(520, 380), "the frame changed, the view's space did not");
    scripted::act(serial, Action::SetResizable(true));
    assert!(eventually(WAIT, || simulate::is_resizable(window)));
}

#[test]
fn a_plugin_that_asks_for_more_room_than_the_screen_has_gets_what_fits() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let window = open(&mut instance, &options("asks for too much"));
    let natural = simulate::client_size(window);

    // A window larger than the screen could not be reached to make it
    // smaller, so a plugin that can do with less is given what there is.
    // CLAP has the host say so with `set_size`.
    scripted::act(serial, Action::Resize(30_000, 30_000));
    assert!(eventually(WAIT, || editor_steps(serial).last() == Some(&"gui size")));
    let given = simulate::client_size(window);
    assert!(given.width > natural.width && given.height > natural.height, "{given:?}");
    assert!(given.width < 30_000 && given.height < 30_000, "{given:?}");
    assert_eq!(value(&mut instance, scripted::EDITOR_WIDTH), f64::from(given.width));
    assert_eq!(value(&mut instance, scripted::EDITOR_HEIGHT), f64::from(given.height));
}

// Windows tells a window that it moved to a monitor of another resolution (`WM_DPICHANGED`). On X
// an editor is told a scale of 1, and a plugin reads the display's resolution itself (`editor/x11.rs`).
#[cfg(windows)]
#[test]
fn an_editor_follows_its_window_to_a_monitor_of_another_scale() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let window = open(&mut instance, &options("changes monitor"));

    // 192 dots per inch is a scale of 2.
    simulate::change_dpi(window, 192);
    assert!(eventually(WAIT, || value(&mut instance, scripted::SEEN_SCALE) == 2.0));
    let doubled = Size::new(scripted::EDITOR_SIZE.0 * 2, scripted::EDITOR_SIZE.1 * 2);
    assert!(eventually(WAIT, || simulate::client_size(window) == doubled), "{:?}", simulate::client_size(window));
    assert_eq!(value(&mut instance, scripted::EDITOR_WIDTH), f64::from(doubled.width));
}

#[test]
fn a_plugin_that_reports_its_editor_gone_has_it_closed() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let window = open(&mut instance, &options("lost"));
    scripted::act(serial, Action::Lose);
    assert!(eventually(WAIT, || !is_open(&instance)));
    assert!(!simulate::exists(window));
    assert_eq!(editor_steps(serial).last(), Some(&"gui destroy"), "the host acknowledged by destroying the editor");
}

// ---- what an editor needs while it is open ----------------------------------------

#[test]
fn a_plugins_timer_ticks_for_as_long_as_it_is_registered() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(value(&mut instance, scripted::TIMER_TICKS), 0.0, "no timer is registered yet");

    // The editor registers its timer when it is created.
    open(&mut instance, &options("ticks"));
    assert!(eventually(WAIT, || value(&mut instance, scripted::TIMER_TICKS) >= 5.0), "the timer never ticked");

    // And unregisters it when it is destroyed.
    instance.controller.as_mut().unwrap().close_editor();
    let stopped_at = value(&mut instance, scripted::TIMER_TICKS);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(value(&mut instance, scripted::TIMER_TICKS), stopped_at, "an unregistered timer still ticks");
}

#[test]
fn an_edit_made_in_the_editor_is_heard_and_reported_with_its_gesture() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let gain = index(&instance, scripted::GAIN);
    open(&mut instance, &options("edits"));
    assert_eq!(block(&mut instance, 0.5), vec![0.5; BLOCK]);
    assert_eq!(instance.controller.as_mut().unwrap().take_param_edits(), []);

    // The user turns the gain knob in the editor. The plugin has the value
    // at once; the host hears of it with the next block, or by a flush if
    // that block is long in coming.
    scripted::act(serial, Action::TurnKnob(0.5));
    assert!(eventually(WAIT, || value(&mut instance, scripted::GAIN) == 0.5));
    assert_eq!(block(&mut instance, 0.5), vec![0.25; BLOCK], "the edit is heard");
    let edit = |kind| ParamEdit { index: gain, kind };
    assert_eq!(
        instance.controller.as_mut().unwrap().take_param_edits(),
        [edit(ParamEditKind::Begin), edit(ParamEditKind::Value(0.5)), edit(ParamEditKind::End)]
    );
    assert_eq!(instance.controller.as_mut().unwrap().take_param_edits(), [], "edits are reported once");
}

#[test]
fn a_plugin_that_is_being_processed_is_not_flushed_from_the_main_thread() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    open(&mut instance, &options("processed"));

    // Blocks keep coming, far more often than the main thread's patience
    // with a flush request lasts, while the user turns the knob.
    scripted::act(serial, Action::TurnKnob(0.5));
    let mut edits = 0;
    assert!(eventually(WAIT, || {
        block(&mut instance, 0.5);
        edits += instance.controller.as_mut().unwrap().take_param_edits().len();
        edits >= 3
    }));
    // The plugin's request for a flush was answered by its own blocks.
    // Only a machine that stalls this test for longer than the main
    // thread waits makes that thread flush, and then once.
    let flushes = value(&mut instance, scripted::AUDIO_FLUSHES);
    assert!(flushes <= 1.0, "{flushes} flushes for one request");
}

#[test]
fn an_edit_is_flushed_out_of_a_plugin_nobody_is_processing() {
    needs_windows!();
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let serial = serial(&mut instance);
    let gain = index(&instance, scripted::GAIN);
    open(&mut instance, &options("flushes"));

    // No block comes. The plugin asked for a flush, and gets one from the
    // plugin main thread, in the audio thread's role.
    scripted::act(serial, Action::TurnKnob(1.5));
    let mut edits = Vec::new();
    assert!(eventually(WAIT, || {
        edits.extend(instance.controller.as_mut().unwrap().take_param_edits());
        edits.len() >= 3
    }));
    let edit = |kind| ParamEdit { index: gain, kind };
    assert_eq!(edits, [edit(ParamEditKind::Begin), edit(ParamEditKind::Value(1.5)), edit(ParamEditKind::End)]);
    assert_eq!(value(&mut instance, scripted::AUDIO_FLUSHES), 1.0);
    assert_eq!(value(&mut instance, scripted::GAIN), 1.5);

    // Once it is processed again, the audio thread has it back, and the
    // main thread does not flush a plugin that asked for nothing.
    assert_eq!(block(&mut instance, 0.5), vec![0.75; BLOCK]);
    std::thread::sleep(Duration::from_millis(1_300));
    assert_eq!(value(&mut instance, scripted::AUDIO_FLUSHES), 1.0, "a flush nobody asked for");
}

/// Once every editor is closed, the connection their windows had is still
/// read whenever the server says something by itself (here, that a
/// keyboard's mapping changed, which every client is told), and the plugin
/// thread sleeps: it does not turn round a connection nobody reads.
#[cfg(target_os = "linux")]
#[test]
fn with_every_editor_closed_what_the_server_says_unasked_is_read_and_the_thread_sleeps() {
    needs_windows!();
    use std::sync::atomic::Ordering;
    let factory = factory();
    let mut instance = create(&factory, kind::EDITOR);
    let window = open(&mut instance, &options("closed before the server speaks"));
    instance.controller.as_mut().unwrap().close_editor();
    assert!(!simulate::exists(window));
    let thrown = crate::editor::THROWN.load(Ordering::Relaxed);

    simulate::tell_every_client_of_a_mapping();
    assert!(
        eventually(WAIT, || crate::editor::THROWN.load(Ordering::Relaxed) > thrown),
        "what the server said was read, with no window open"
    );
    let before = crate::main_thread::waits();
    std::thread::sleep(Duration::from_millis(500));
    let turns = crate::main_thread::waits() - before;
    // Other tests' plugin threads wait too (this one's idles while their
    // editors are open, every 15 ms): far fewer than a thread that turns.
    assert!(turns < 2_000, "the plugin threads waited {turns} times in half a second");
}
