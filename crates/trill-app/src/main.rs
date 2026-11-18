//! The DAW application: starts the audio engine and opens the window.

// A release build is a windowed program: no console window behind it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio_scene;
mod cli;
mod demo;
mod ui;

use anyhow::Context;
use std::sync::Arc;
use trill_runtime::Runtime;

fn main() -> anyhow::Result<()> {
    // Plugin files are scanned in a child process of this same executable,
    // and songs with plugins are rendered, measured and probed in one.
    trill_plugins::scan::run_as_scanner_if_requested();
    trill_runtime::jobs::run_as_job_if_requested();

    let cli = match cli::parse(std::env::args().skip(1))? {
        cli::Parsed::Run(cli) => cli,
        cli::Parsed::Help => {
            print!("{}", cli::USAGE);
            return Ok(());
        }
    };
    trill_mcp::logging::init_stderr();

    // The session, the media cache, the sound library and the settings,
    // shared by the window and the MCP server. It lives until the window
    // closes; dropping it saves the settings.
    // A scene puts made-up audio and MIDI devices in the hardware's place.
    let started = match cli.audio_scene {
        Some(scene) => Runtime::start_custom(cli.runtime_options(), scene.parts()),
        None => Runtime::start(cli.runtime_options()),
    };
    let runtime = Arc::new(started.context("could not start Trill")?);
    runtime.do_jobs_as_usual();
    // And this is a program that may start itself to study a recording,
    // and fetch what the user asks it to with the system's curl.
    runtime.study_and_fetch_as_usual();
    if let Some(scene) = cli.audio_scene {
        scene.stage(&runtime);
    }
    let session = runtime.session.clone();

    // services
    let mut options = cli.ui_options();
    // Held until the window closes; dropping it stops the server.
    let _mcp = cli.mcp_address().and_then(|address| match trill_mcp::serve_http(runtime.clone(), address) {
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
    if cli.making_of_scene {
        ui::play_making_of_scene(&mut session.lock());
    }
    if let Some(clip) = cli.clip_to_select(session.lock().project())? {
        options.select_clip = Some(clip);
    }
    options.also_select = cli.clips_to_also_select(session.lock().project())?;
    let armed = cli.tracks_to_arm(session.lock().project())?;
    for track in armed {
        session.lock().set_armed(track, true).context("could not arm a track")?;
    }
    // Returns at once; the folders are scanned in the background and the
    // browser fills in as they are.
    for folder in &cli.library {
        runtime.library.add_root(folder);
    }
    ui::run(runtime, options)
}
