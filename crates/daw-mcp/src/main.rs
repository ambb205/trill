//! `daw-mcp`: the DAW without a window, driven entirely over MCP.
//!
//! An MCP client launches this binary and talks to it over stdio (the
//! default), or it can listen on a local HTTP port. Sound plays through the
//! default audio device unless `--no-audio` is given.

use anyhow::{Context, bail};
use daw_core::device::{CompositeFactory, DeviceFactory};
use daw_dsp::BuiltinFactory;
use daw_engine::{AudioBackend, Session, SharedSession};
use daw_plugins::{PluginHost, ScanMode, Scanner};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "\
daw-mcp: headless DAW controlled over the Model Context Protocol

USAGE:
    daw-mcp [OPTIONS]

OPTIONS:
    --stdio              Serve MCP over stdin/stdout (default)
    --http [ADDR]        Serve MCP over HTTP instead (default 127.0.0.1:7777)
    --no-audio           Do not open an audio device; rendering still works
    --no-plugins         Offer only the built-in devices
    --project <FILE>     Open a project file at startup
    -h, --help           Show this help";

enum Transport {
    Stdio,
    Http(SocketAddr),
}

struct Options {
    transport: Transport,
    audio: AudioBackend,
    plugins: bool,
    project: Option<PathBuf>,
}

fn parse_args() -> anyhow::Result<Option<Options>> {
    let mut options = Options { transport: Transport::Stdio, audio: AudioBackend::Auto, plugins: true, project: None };
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stdio" => options.transport = Transport::Stdio,
            "--http" => {
                let address = match args.next_if(|next| !next.starts_with('-')) {
                    Some(text) => {
                        text.parse().with_context(|| format!("'{text}' is not an address like 127.0.0.1:7777"))?
                    }
                    None => SocketAddr::from(([127, 0, 0, 1], daw_mcp::DEFAULT_PORT)),
                };
                options.transport = Transport::Http(address);
            }
            "--no-audio" => options.audio = AudioBackend::Null { sample_rate: 48_000 },
            "--no-plugins" => options.plugins = false,
            "--project" => {
                options.project = Some(args.next().context("--project needs a file")?.into());
            }
            "-h" | "--help" => return Ok(None),
            other => bail!("unknown option '{other}'\n\n{USAGE}"),
        }
    }
    Ok(Some(options))
}

fn main() -> anyhow::Result<()> {
    // Plugin files are scanned in a child process of this same executable.
    daw_plugins::scan::run_as_scanner_if_requested();

    let Some(options) = parse_args()? else {
        println!("{USAGE}");
        return Ok(());
    };
    // To stderr: with the stdio transport, stdout belongs to the protocol.
    daw_mcp::logging::init_stderr();

    // device factories
    let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(BuiltinFactory)];
    if options.plugins {
        // CLAP and VST3 plugins, each scanned in a process of its own.
        let plugins = PluginHost::new(Scanner::CurrentExecutable).context("cannot start the plugin host")?;
        factories.extend(plugins.factories());
        // The first scan of a machine's plugins takes seconds; do it now, in
        // the background, rather than on the first tool call that needs it.
        std::thread::Builder::new().name("plugin-scan".into()).spawn(move || {
            plugins.scan(ScanMode::UseCache);
            for failure in plugins.scan_failures() {
                log::warn!("skipped plugin {}: {}", failure.path.display(), failure.reason);
            }
        })?;
    }
    let factory = Arc::new(CompositeFactory::new(factories));

    let mut session = Session::new(factory, options.audio)?;
    let audio = session.audio_info().clone();
    log::info!("audio output: {}", audio.output);
    if let Some(warning) = &audio.warning {
        log::warn!("{warning}");
    }
    if let Some(path) = &options.project {
        session.load(path).with_context(|| format!("cannot open {}", path.display()))?;
        log::info!("opened {}", path.display());
    }
    let session: SharedSession = Arc::new(parking_lot::Mutex::new(session));

    match options.transport {
        Transport::Stdio => daw_mcp::serve_stdio(session),
        Transport::Http(address) => {
            let server = daw_mcp::serve_http(session, address)?;
            log::info!("MCP server listening on {}", server.url());
            // Serve until the process is stopped.
            loop {
                std::thread::park();
            }
        }
    }
}
