//! Model Context Protocol server for the DAW.
//!
//! Exposes the [`Session`](trill_engine::Session) as MCP tools so an AI agent
//! can compose, arrange and mix. Two transports:
//!
//! * [`serve_http`]: streamable HTTP on localhost, for attaching to a
//!   running DAW whose window the user is watching.
//! * [`serve_stdio`]: stdio, for a headless DAW launched by the MCP client.

mod input;
pub mod logging;
mod requests;
mod server;
mod view;

pub use server::TrillServer;

use anyhow::Context;
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::net::SocketAddr;
use std::sync::mpsc;
use std::thread::JoinHandle;
use tokio_util::sync::CancellationToken;
use trill_engine::SharedSession;

/// Path of the MCP endpoint on the HTTP server.
pub const HTTP_PATH: &str = "/mcp";
pub const DEFAULT_PORT: u16 = 7777;

/// Serves MCP over stdin/stdout until the client disconnects. Blocks.
/// Nothing else may write to stdout while this runs.
pub fn serve_stdio(session: SharedSession) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    runtime.block_on(async {
        let service = TrillServer::new(session).serve(rmcp::transport::stdio()).await?;
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
/// Bind to a loopback address: the server has no authentication, and anyone
/// who can reach it can edit the project and write files.
pub fn serve_http(session: SharedSession, address: SocketAddr) -> anyhow::Result<HttpServer> {
    let cancel = CancellationToken::new();
    let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<SocketAddr>>();

    let server_cancel = cancel.clone();
    let thread = std::thread::Builder::new().name("mcp-server".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = ready_tx.send(Err(error.into()));
                return;
            }
        };
        runtime.block_on(async move {
            let listener = match tokio::net::TcpListener::bind(address).await {
                Ok(listener) => listener,
                Err(error) => {
                    let _ =
                        ready_tx.send(Err(anyhow::Error::new(error).context(format!("cannot listen on {address}"))));
                    return;
                }
            };
            let bound = listener.local_addr().unwrap_or(address);

            let service: StreamableHttpService<TrillServer, LocalSessionManager> = StreamableHttpService::new(
                move || Ok(TrillServer::new(session.clone())),
                Default::default(),
                StreamableHttpServerConfig::default().with_cancellation_token(server_cancel.child_token()),
            );
            let router = axum::Router::new().nest_service(HTTP_PATH, service);
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
