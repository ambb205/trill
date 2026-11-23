//! `trill-mcp`: Trill without a window, driven entirely over MCP.
//!
//! An MCP client launches this binary and talks to it over stdio (the
//! default), or it can listen on a local HTTP port. Sound plays through the
//! default audio device unless `--no-audio` is given.

use anyhow::{Context, bail};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use trill_engine::AudioBackend;
use trill_plugins::Scanner;
use trill_runtime::{Runtime, RuntimeOptions, Storage};

const USAGE: &str = "\
trill-mcp: Trill without a window, controlled over the Model Context Protocol

USAGE:
    trill-mcp [OPTIONS]

OPTIONS:
    --stdio              Serve MCP over stdin/stdout (default)
    --http [ADDR]        Serve MCP over HTTP instead (default 127.0.0.1:7777)
    --no-audio           Do not open an audio device; rendering still works
    --no-plugins         Offer only the built-in devices
    --scratch            Keep nothing between runs: no settings, no sound library,
                         no safety copies of unsaved work
    --profile <FOLDER>   Keep settings, the library index, saved presets and safety
                         copies in this folder instead of the user's own
    --project <FILE>     Open a project file at startup
    -V, --version        Say which Trill this is: its version, and for a release its build
    -h, --help           Show this help";

enum Transport {
    Stdio,
    Http(SocketAddr),
}

struct Options {
    transport: Transport,
    audio: AudioBackend,
    plugins: bool,
    storage: Storage,
    project: Option<PathBuf>,
}

fn parse_args() -> anyhow::Result<Option<Options>> {
    let mut options = Options {
        transport: Transport::Stdio,
        audio: AudioBackend::Auto,
        plugins: true,
        storage: Storage::User,
        project: None,
    };
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stdio" => options.transport = Transport::Stdio,
            "--http" => {
                let address = match args.next_if(|next| !next.starts_with('-')) {
                    Some(text) => {
                        text.parse().with_context(|| format!("'{text}' is not an address like 127.0.0.1:7777"))?
                    }
                    None => SocketAddr::from(([127, 0, 0, 1], trill_mcp::DEFAULT_PORT)),
                };
                options.transport = Transport::Http(address);
            }
            "--no-audio" => options.audio = AudioBackend::Null { sample_rate: 48_000 },
            "--no-plugins" => options.plugins = false,
            "--scratch" => options.storage = Storage::Nowhere,
            "--profile" => {
                let folder = PathBuf::from(args.next().context("--profile needs a folder")?);
                options.storage = Storage::At {
                    settings: folder.join("settings.json"),
                    library_index: Some(folder.join("library-index.json")),
                };
            }
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
    // Plugin files are scanned in a child process of this same executable,
    // and songs with plugins are rendered, measured and probed in one.
    trill_plugins::scan::run_as_scanner_if_requested();
    trill_runtime::jobs::run_as_job_if_requested();
    // On macOS plugins take the first thread for their own: it drives them,
    // and the server runs beside it.
    #[cfg(target_os = "macos")]
    trill_plugins::first_thread::lend(|| match serve() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error:?}");
            1
        }
    });
    #[cfg(not(target_os = "macos"))]
    serve()
}

/// The server, from its options to its end.
fn serve() -> anyhow::Result<()> {
    if std::env::args().skip(1).any(|arg| arg == "--version" || arg == "-V") {
        println!("{}", trill_update::says());
        return Ok(());
    }
    let Some(options) = parse_args()? else {
        println!("{USAGE}");
        return Ok(());
    };
    // To stderr: with the stdio transport, stdout belongs to the protocol.
    trill_mcp::logging::init_stderr();
    // Says for as long as this runs that a Trill does, so that no update
    // goes in under it. An update that is staged is left for the window:
    // the program a client started has to be the one that answers it.
    let _started = trill_update::start(trill_update::Program::Server);

    // The session, the sound library and the user's settings: the same
    // ones the window uses. CLAP and VST3 plugins are scanned in a second
    // process of this executable.
    let runtime = Runtime::start(RuntimeOptions {
        audio: options.audio,
        plugins: options.plugins.then_some(Scanner::CurrentExecutable),
        storage: options.storage,
    })?;
    runtime.do_jobs_as_usual();
    // And this is a program that may start itself to study a recording,
    // and fetch what the user asks it to with the system's curl.
    runtime.study_and_fetch_as_usual();
    // It looks for a newer Trill when it is asked to, and never by itself:
    // there is nobody at a window here to be told.
    runtime.look_for_updates_when_asked();
    {
        let mut session = runtime.session.lock();
        let audio = session.audio_info().clone();
        log::info!("audio output: {}", audio.output);
        if let Some(warning) = &audio.warning {
            log::warn!("{warning}");
        }
        if let Some(path) = &options.project {
            session.load(path).with_context(|| format!("cannot open {}", path.display()))?;
            log::info!("opened {}", path.display());
        }
    }
    let runtime = Arc::new(runtime);

    match options.transport {
        Transport::Stdio => trill_mcp::serve_stdio(runtime),
        Transport::Http(address) => {
            let server = trill_mcp::serve_http(runtime, address)?;
            log::info!("MCP server listening on {}", server.url());
            // Serve until the process is stopped.
            loop {
                std::thread::park();
            }
        }
    }
}
