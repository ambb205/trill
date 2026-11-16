//! Editors of real, installed plugins, in real windows on the screen.
//!
//! These tests put windows on the desktop of whoever runs them, so they
//! only run when asked to: with `TRILL_TEST_PLUGIN_WINDOWS=1` in the
//! environment. Otherwise each prints a note and passes, and a plain
//! `cargo test` never flashes a window on anyone's screen. (The life of an
//! editor is tested without visible windows, against scripted plugins, in
//! the crate's unit tests.)
//!
//! The windows open without taking the keyboard focus, in a corner, and
//! are closed again within a few seconds. Only plugins that are known not
//! to put up a dialog of their own are used.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use trill_core::ChainRef;
use trill_core::device::{CompositeFactory, DeviceFactory, EditorOptions};
use trill_dsp::BuiltinFactory;
use trill_engine::{AudioBackend, Session};
use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};

/// The plugins whose editors are opened: format and name. Small ones, so
/// that little of the screen is covered.
const PLUGINS: [(&str, &str); 5] = [
    ("clap", "Surge XT Effects"),
    ("vst3", "Surge XT Effects"),
    ("clap", "WaveForge Development Shell"),
    ("vst3", "WaveForge Development Shell"),
    ("vst3", "JS80P"),
];

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
/// How long a window stays open while a test looks at it.
const LOOK: Duration = Duration::from_millis(400);

/// Prints what the host logs, so that a run that goes wrong shows how far
/// it got.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record) {
        eprintln!("[{}] {}", record.level().as_str().to_lowercase(), record.args());
    }

    fn flush(&self) {}
}

/// Says where a test is, on the same stream as the host's log.
fn step(what: &str) {
    eprintln!("-- {what}");
}

fn enabled() -> bool {
    static LOG: StderrLog = StderrLog;
    if log::set_logger(&LOG).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
    let enabled = std::env::var_os("TRILL_TEST_PLUGIN_WINDOWS").is_some_and(|value| value == "1");
    if !enabled {
        eprintln!("skipping: set TRILL_TEST_PLUGIN_WINDOWS=1 to open real plugin editors on this screen");
    }
    enabled && cfg!(windows)
}

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("editor-windows").join(name)
}

fn host() -> PluginHost {
    let scanner = Scanner::Executable(PathBuf::from(env!("CARGO_BIN_EXE_trill-plugin-scan")));
    let clap = ClapConfig {
        scanner: scanner.clone(),
        cache_path: Some(scratch("clap.json")),
        scan_timeout: Duration::from_secs(60),
        ..ClapConfig::default()
    };
    let vst3 = Vst3Config {
        scanner,
        cache_path: Some(scratch("vst3.json")),
        scan_timeout: Duration::from_secs(60),
        ..Vst3Config::default()
    };
    PluginHost::with_configs(clap, vst3).expect("the plugin main thread starts")
}

/// The URI of the installed plugin of `format` called `name`, if there is
/// one. Says so when a plugin is skipped for not being installed.
fn installed(factory: &dyn DeviceFactory, format: &str, name: &str) -> Option<String> {
    let scheme = format!("{format}:");
    let found = factory.descriptors().into_iter().find(|d| d.uri.starts_with(&scheme) && d.name == name);
    if found.is_none() {
        eprintln!("skipping {format} '{name}': the plugin is not installed");
    }
    found.map(|descriptor| descriptor.uri)
}

/// An editor in the bottom right corner of a 1080p screen's worth of
/// desktop, which does not take the focus.
fn options(title: &str) -> EditorOptions {
    EditorOptions { title: title.to_string(), owner: None, position: Some((900, 300)), activate: false }
}

/// Fails a test that deadlocks instead of hanging the whole suite, and
/// with it leaving a window on the screen: a test that panics ends the
/// process, which closes its windows.
fn within(limit: Duration, body: impl FnOnce() + Send + 'static) {
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        body();
        let _ = done.send(());
    });
    match finished.recv_timeout(limit) {
        Ok(()) => worker.join().expect("the test body finished"),
        Err(RecvTimeoutError::Disconnected) => {
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(RecvTimeoutError::Timeout) => {
            eprintln!("not finished after {limit:?}: deadlocked? ending the process to close its windows");
            std::process::exit(101);
        }
    }
}

#[test]
fn real_editors_open_are_captured_reopen_and_go_with_their_device() {
    if !enabled() {
        return;
    }
    within(Duration::from_secs(240), || {
        let host = host();
        let factory = CompositeFactory::new(host.factories());
        for (format, name) in PLUGINS {
            let Some(uri) = installed(&factory, format, name) else { continue };
            let what = format!("{format} '{name}'");
            step(&format!("{what}: create"));
            let mut project = trill_core::Project::new("t");
            let id = project.add_device(ChainRef::Master, uri, name, None).unwrap();
            let mut instance = factory.create(project.device(id).unwrap(), SR, BLOCK).unwrap_or_else(|e| panic!("{e}"));
            let controller = instance.controller.as_mut().expect("a plugin has a controller");
            assert!(controller.has_editor(), "{what}");
            assert!(!controller.is_editor_open(), "{what}");
            assert!(controller.capture_editor().is_none(), "{what}: there is nothing to capture yet");

            let options = options(&format!("{name} ({format}) - Trill test"));
            let started = Instant::now();
            step("open");
            controller.open_editor(&options).unwrap_or_else(|error| panic!("{what}: {error}"));
            assert!(controller.is_editor_open(), "{what}");
            assert!(started.elapsed() < Duration::from_secs(20), "{what}: opening took {:?}", started.elapsed());
            thread::sleep(LOOK);
            let image = controller.capture_editor().unwrap_or_else(|| panic!("{what}: no picture of the editor"));
            assert!(image.width >= 200 && image.height >= 150, "{what}: {}x{}", image.width, image.height);
            assert_eq!(image.rgba.len(), image.width as usize * image.height as usize * 4, "{what}");

            // Opening it again keeps the one editor, at the same size.
            step("open again");
            controller.open_editor(&options).unwrap_or_else(|error| panic!("{what}: {error}"));
            let again = controller.capture_editor().unwrap_or_else(|| panic!("{what}: no picture the second time"));
            assert_eq!((again.width, again.height), (image.width, image.height), "{what}");

            // Closed and opened once more, the plugin makes its editor
            // afresh without complaint.
            step("close");
            controller.close_editor();
            assert!(!controller.is_editor_open(), "{what}");
            assert!(controller.capture_editor().is_none(), "{what}");
            step("reopen");
            controller.open_editor(&options).unwrap_or_else(|error| panic!("{what}: reopening: {error}"));
            assert!(controller.is_editor_open(), "{what}");
            thread::sleep(LOOK);

            // The device goes while its editor is open: the window must go
            // with it, and promptly.
            let closing = Instant::now();
            step("drop the device with its editor open");
            drop(instance);
            assert!(closing.elapsed() < Duration::from_secs(15), "{what}: teardown took {:?}", closing.elapsed());
        }
    });
}

#[test]
fn a_session_opens_editors_and_closes_them_when_it_lets_go_of_their_devices() {
    if !enabled() {
        return;
    }
    within(Duration::from_secs(240), || {
        let host = host();
        let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(BuiltinFactory)];
        factories.extend(host.factories());
        let factory = Arc::new(CompositeFactory::new(factories));
        let uris: Vec<String> =
            PLUGINS[..3].iter().filter_map(|(format, name)| installed(factory.as_ref(), format, name)).collect();
        if uris.len() < 2 {
            eprintln!("skipping: fewer than two of the plugins this test uses are installed");
            return;
        }
        let mut session = Session::new(factory, AudioBackend::Null { sample_rate: SR as u32 }).unwrap();
        let track = session.edit(|project| Ok(project.add_track("Plugins"))).unwrap();
        let mut devices = Vec::new();
        for (index, uri) in uris.iter().enumerate() {
            step(&format!("add and open {uri}"));
            let device = session.add_device(ChainRef::Track(track), uri, None, None).unwrap_or_else(|e| panic!("{e}"));
            assert!(session.device_has_editor(device), "{uri}");
            // Several editors at once, side by side.
            let position = Some((300 + 420 * index as i32, 200 + 60 * index as i32));
            let options = EditorOptions { position, ..options(&format!("Trill test {index}")) };
            session.open_device_editor(device, &options).unwrap_or_else(|error| panic!("{uri}: {error}"));
            assert!(session.is_device_editor_open(device), "{uri}");
            devices.push(device);
        }
        thread::sleep(LOOK);
        assert_eq!(session.open_device_editors().len(), devices.len());
        // Nobody touched a control, but some plugins report where their
        // parameters stand when they are first processed; either way
        // collecting must work while editors are open.
        session.absorb_editor_edits();

        // Removing a device closes its editor and leaves the others.
        step("remove a device");
        session.edit(|project| project.remove_device(devices[0]).map(drop)).unwrap();
        assert!(!session.is_device_editor_open(devices[0]));
        assert_eq!(session.open_device_editors(), devices[1..]);
        // Undo brings the device back, without its window.
        step("undo");
        assert!(session.undo());
        assert!(session.device_has_editor(devices[0]) && !session.is_device_editor_open(devices[0]));

        // Another project closes them all.
        step("new project");
        session.new_project();
        assert!(session.open_device_editors().is_empty());

        // And a session that ends with an editor open takes it along.
        step("add and open the last");
        let device = session.add_device(ChainRef::Master, &uris[0], None, None).unwrap();
        session.open_device_editor(device, &options("Trill test, last")).unwrap();
        thread::sleep(LOOK);
        let closing = Instant::now();
        step("drop the session");
        drop(session);
        step("drop the host");
        drop(host);
        step("done");
        assert!(closing.elapsed() < Duration::from_secs(20), "shutting down took {:?}", closing.elapsed());
    });
}
