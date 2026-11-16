//! Model Context Protocol server for the DAW.
//!
//! Exposes the program's [`Runtime`] (the session, the sound library and
//! what joins them) as MCP tools so an AI agent can find sounds, compose,
//! arrange and mix. Two transports:
//!
//! * [`serve_http`]: streamable HTTP on localhost, for attaching to a
//!   running DAW whose window the user is watching.
//! * [`serve_stdio`]: stdio, for a headless DAW launched by the MCP client.
//!
//! The HTTP server has a second door for the assistant that works beside
//! the user in the window ([`HttpServer::assistant_url`]): the same tools,
//! held to the limits described in [`guard`].

mod guard;
mod input;
mod instruments;
pub mod logging;
mod music;
mod requests;
mod server;
mod sounds;
mod view;

pub use server::TrillServer;

use anyhow::Context;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use tokio_util::sync::CancellationToken;
use trill_runtime::Runtime;

/// Path of the MCP endpoint on the HTTP server.
pub const HTTP_PATH: &str = "/mcp";
/// Path of the guarded endpoint for the assistant in the window.
pub const ASSISTANT_PATH: &str = "/assistant";
pub const DEFAULT_PORT: u16 = 7777;

/// Serves MCP over stdin/stdout until the client disconnects. Blocks.
/// Nothing else may write to stdout while this runs.
pub fn serve_stdio(runtime: Arc<Runtime>) -> anyhow::Result<()> {
    let executor = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    executor.block_on(async {
        let service = TrillServer::new(runtime).serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

/// A running HTTP MCP server. Stops when dropped.
pub struct HttpServer {
    address: SocketAddr,
    cancel: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

impl HttpServer {
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The URL to give an MCP client.
    pub fn url(&self) -> String {
        format!("http://{}{HTTP_PATH}", self.address)
    }

    /// The URL to give the assistant that works beside the user in the
    /// window: the same tools, but it writes only into the user's working
    /// folders and cannot throw unsaved work away.
    pub fn assistant_url(&self) -> String {
        format!("http://{}{ASSISTANT_PATH}", self.address)
    }
}

/// How to serve MCP over HTTP.
#[derive(Clone, Debug, Default)]
pub struct HttpOptions {
    /// Where the assistant's endpoint may save when the open project has
    /// no folder of its own yet. `None` is `Documents/Trill` in the user's
    /// home folder.
    pub assistant_folder: Option<PathBuf>,
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the MCP server on `address` (use port 0 for any free port) on its
/// own thread and returns once it is listening.
///
/// Bind to a loopback address: the server has no authentication, and any
/// program that can reach it can edit the project and write files. Requests
/// from web pages are refused (see the allowed origins below).
pub fn serve_http(runtime: Arc<Runtime>, address: SocketAddr) -> anyhow::Result<HttpServer> {
    serve_http_with(runtime, address, HttpOptions::default())
}

/// [`serve_http`], with the choices it otherwise makes itself.
pub fn serve_http_with(runtime: Arc<Runtime>, address: SocketAddr, options: HttpOptions) -> anyhow::Result<HttpServer> {
    let assistant_folder =
        options.assistant_folder.or_else(guard::default_folder).unwrap_or_else(|| std::env::temp_dir().join("Trill"));
    let cancel = CancellationToken::new();
    let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<SocketAddr>>();

    let server_cancel = cancel.clone();
    let thread = std::thread::Builder::new().name("mcp-server".into()).spawn(move || {
        let executor = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(executor) => executor,
            Err(error) => {
                let _ = ready_tx.send(Err(error.into()));
                return;
            }
        };
        executor.block_on(async move {
            let listener = match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => listener,
                Err(error) => {
                    let _ =
                        ready_tx.send(Err(anyhow::Error::new(error).context(format!("cannot listen on {address}"))));
                    return;
                }
            };
            let bound = listener.local_addr().unwrap_or(address);

            // Programs on this machine talk to the server; pages in a
            // browser must not. A program sends no Origin header; a page
            // always sends its own, and the only origin allowed is the
            // server's, which serves no pages. The Host check (on by
            // default: loopback names only) covers a page that reaches the
            // address under a name of its own.
            let own_origins =
                [format!("http://127.0.0.1:{}", bound.port()), format!("http://localhost:{}", bound.port())];
            let config = || {
                StreamableHttpServerConfig::default()
                    .with_cancellation_token(server_cancel.child_token())
                    .with_allowed_origins(own_origins.clone())
            };
            let full = runtime.clone();
            let service: StreamableHttpService<TrillServer, LocalSessionManager> =
                StreamableHttpService::new(move || Ok(TrillServer::new(full.clone())), Default::default(), config());
            let assistant: StreamableHttpService<TrillServer, LocalSessionManager> = StreamableHttpService::new(
                move || Ok(TrillServer::guarded(runtime.clone(), assistant_folder.clone())),
                Default::default(),
                config(),
            );
            let router = axum::Router::new().nest_service(HTTP_PATH, service).nest_service(ASSISTANT_PATH, assistant);
            let _ = ready_tx.send(Ok(bound));

            let shutdown = server_cancel.clone();
            if let Err(error) = axum::serve(listener, router)
                .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
                .await
            {
                log::error!("MCP server stopped: {error}");
            }
        });
    })?;

    let address = ready_rx.recv().context("the MCP server thread exited before it started")??;
    Ok(HttpServer { address, cancel, thread: Some(thread) })
}
