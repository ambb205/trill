//! The DAW application: starts the audio engine and opens the window.

// A release build is a windowed program: no console window behind it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod demo;
mod ui;

use anyhow::Context;
use daw_core::device::{CompositeFactory, DeviceFactory};
use daw_dsp::BuiltinFactory;
use daw_engine::{Session, SharedSession};
use daw_plugins::{ClapConfig, ClapFactory, ScanMode, Scanner};
use parking_lot::Mutex;
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    // Plugin files are scanned in a child process of this same executable.
    daw_plugins::scan::run_as_scanner_if_requested();

    let cli = match cli::parse(std::env::args().skip(1))? {
        cli::Parsed::Run(cli) => cli,
        cli::Parsed::Help => {
            print!("{}", cli::USAGE);
            return Ok(());
        }
    };
    daw_mcp::logging::init_stderr();

    // device factories
    let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(BuiltinFactory)];
    if cli.plugins_enabled() {
        let config = ClapConfig { scanner: Scanner::CurrentExecutable, ..ClapConfig::default() };
        let plugins = ClapFactory::new(config).context("could not start the plugin host")?;
        factories.push(Box::new(plugins.clone()));
        // The first scan of a machine's plugins takes seconds. It runs in
        // the background; the window lists plugins once it is done.
        std::thread::Builder::new().name("plugin-scan".into()).spawn(move || {
            plugins.scan(ScanMode::UseCache);
            for failure in plugins.scan_failures() {
                log::warn!("skipped plugin {}: {}", failure.path.display(), failure.reason);
            }
        })?;
    }
    let factory = Arc::new(CompositeFactory::new(factories));

    let session = Session::new(factory, cli.audio_backend()).context("could not start the audio engine")?;
    let session: SharedSession = Arc::new(Mutex::new(session));

    // services
    let mut options = cli.ui_options();
    // Held until the window closes; dropping it stops the server.
    let _mcp = cli.mcp_address().and_then(|address| match daw_mcp::serve_http(session.clone(), address) {
        Ok(server) => {
            log::info!("MCP server listening on {}", server.url());
            options.mcp_url = Some(server.url());
            Some(server)
        }
        // Most likely another DAW window already has the port. This one
        // still works; it just cannot be driven by an assistant.
        Err(error) => {
            log::warn!("the MCP server did not start: {error:#}");
            None
        }
    });

    if let Some(path) = &cli.project {
        session.lock().load(path).with_context(|| format!("could not open {}", path.display()))?;
    }
    if cli.demo {
        options.select_clip = Some(demo::load(&mut session.lock()).context("could not build the demo song")?);
    }
    if let Some(clip) = cli.clip_to_select(session.lock().project())? {
        options.select_clip = Some(clip);
    }
    ui::run(session, options)
}
