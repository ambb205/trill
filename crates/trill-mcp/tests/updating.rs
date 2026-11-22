//! A newer Trill the way an AI client asks about one: over the real MCP
//! transport, with nothing but calls of the `setup` tool.
//!
//! The releases are made up and published on a shelf of the test's, under
//! a key of the test's, for a Trill installed in a folder of the test's.
//! Nothing is asked of a network, and the program that runs the test is
//! neither looked at nor changed.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_engine::AudioBackend;
use trill_mcp::{HttpOptions, HttpServer, serve_http_with};
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};
use trill_update::Place;
use trill_update::scripted::Makers;

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    client: Client,
    /// The same server through the door of the assistant in the window.
    assistant: Client,
    makers: Makers,
    _runtime: Arc<Runtime>,
    _server: HttpServer,
    /// Trill is installed in `Trill` in it, and keeps its settings in `config`.
    folder: tempfile::TempDir,
}

/// A server on a Trill 0.2.0 that its makers released, which fetches from
/// their shelf.
async fn connect() -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let makers = Makers::new();
    let home = folder.path().join("Trill");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("trill.exe"), "window 0.2.0").unwrap();
    let config = folder.path().join("config");
    let storage = Storage::At { settings: config.join("settings.json"), library_index: None };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let updating = makers.program("0.2.0", &home, Some(config.join("update")));
    let runtime =
        Arc::new(Runtime::start_custom(options, Parts { updating: Some(updating), ..Parts::default() }).unwrap());
    runtime.updates().fetch_by(makers.shelf.clone());
    let options = HttpOptions { assistant_folder: Some(folder.path().join("work")) };
    let server = serve_http_with(runtime.clone(), "127.0.0.1:0".parse().unwrap(), options).unwrap();
    let client_of = |url: String| async move {
        let transport = StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(url));
        ClientConfig::default().serve(transport).await.unwrap()
    };
    let (client, assistant) = (client_of(server.url()).await, client_of(server.assistant_url()).await);
    Fixture { client, assistant, makers, _runtime: runtime, _server: server, folder }
}

async fn call(client: &Client, arguments: Value) -> Result<Value, String> {
    let arguments = arguments.as_object().cloned().unwrap();
    let result = client
        .call_tool(CallToolRequestParams::new("setup").with_arguments(arguments))
        .await
        .unwrap_or_else(|error| panic!("setup: protocol error {error}"));
    let text = result.content.first().and_then(|block| block.as_text()).map(|t| t.text.clone()).unwrap_or_default();
    if result.is_error == Some(true) {
        Err(text)
    } else {
        Ok(serde_json::from_str(&text).unwrap_or_else(|_| panic!("setup replied with non-JSON: {text}")))
    }
}

async fn ok(client: &Client, arguments: Value) -> Value {
    call(client, arguments).await.unwrap_or_else(|error| panic!("setup failed: {error}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_hears_of_a_newer_trill_fetches_it_when_asked_and_is_told_when_it_goes_in() {
    let fixture = connect().await;
    let client = &fixture.client;
    let home = fixture.folder.path().join("Trill");

    // Which Trill this is, before anything was looked for.
    let report = ok(client, json!({ "action": "report" })).await;
    assert_eq!(report["trill"]["this"], "Trill 0.2.0 (build abc1234)");
    assert_eq!(report["trill"]["state"], "has not looked");
    assert!(fixture.makers.shelf.asked().is_empty(), "a report asks nothing of a network");
    // Nothing to fetch before one is known of.
    let refusal = call(client, json!({ "action": "fetch", "part": "trill" })).await.unwrap_err();
    assert!(refusal.contains("look_for_newer"), "{refusal}");

    // Nothing is published yet: the look fails in a sentence.
    let refusal = call(client, json!({ "action": "look_for_newer" })).await.unwrap_err();
    assert!(refusal.contains("could not look for a newer version"), "{refusal}");

    fixture.makers.publish("0.3.0", &[("trill.exe", b"window 0.3.0")]);
    let looked = ok(client, json!({ "action": "look_for_newer" })).await;
    let trill = &looked["trill"];
    assert_eq!(trill["newer"]["version"], "0.3.0");
    assert_eq!(trill["newer"]["new_in_it"], "What is new in 0.3.0.");
    assert_eq!(trill["state"], "out");
    assert!(trill["fetch_it"].as_str().unwrap().contains("only when the user asked"), "{trill}");

    // Fetched when it is asked for: the call answers at once, and report says how far it is.
    let began = ok(client, json!({ "action": "fetch", "part": "trill" })).await;
    assert_eq!(began["fetching"], "Trill 0.3.0");
    let since = Instant::now();
    let ready = loop {
        let report = ok(client, json!({ "action": "report" })).await;
        if report["trill"]["state"] == "ready" {
            break report;
        }
        assert!(since.elapsed() < Duration::from_secs(30), "it was not fetched: {}", report["trill"]);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let words = ready["trill"]["ready"].as_str().unwrap();
    assert!(words.contains("goes in when the window next starts while no Trill runs"), "{words}");
    assert!(words.contains("Nothing has changed yet"), "{words}");
    // And nothing has: the update waits beside the program.
    assert_eq!(std::fs::read_to_string(home.join("trill.exe")).unwrap(), "window 0.2.0");
    assert_eq!(Place::at(&home).staged().unwrap().version, "0.3.0".parse().unwrap());

    // Put away again, Trill stays as it is.
    let away = ok(client, json!({ "action": "remove", "part": "trill" })).await;
    assert_eq!(away["put_away"], "trill");
    assert_eq!(Place::at(&home).staged(), None);
    assert_eq!(ok(client, json!({ "action": "report" })).await["trill"]["state"], "out");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_assistant_in_the_window_reads_what_is_known_and_neither_looks_nor_fetches() {
    let fixture = connect().await;
    fixture.makers.publish("0.3.0", &[("trill.exe", b"window 0.3.0")]);
    for arguments in [json!({ "action": "look_for_newer" }), json!({ "action": "fetch", "part": "trill" })] {
        let refusal = call(&fixture.assistant, arguments).await.unwrap_err();
        assert!(refusal.contains("neither looks for a newer version nor fetches one"), "{refusal}");
        assert!(refusal.contains("in the File menu, on the tab Trill"), "{refusal}");
    }
    assert!(fixture.makers.shelf.asked().is_empty(), "{:?}", fixture.makers.shelf.asked());
    // What somebody else found, it may read and tell.
    ok(&fixture.client, json!({ "action": "look_for_newer" })).await;
    let asked = fixture.makers.shelf.asked().len();
    let report = ok(&fixture.assistant, json!({ "action": "report" })).await;
    assert_eq!(report["trill"]["newer"]["version"], "0.3.0");
    // But not fetch it, nor put away what the user fetched.
    assert!(call(&fixture.assistant, json!({ "action": "fetch", "part": "trill" })).await.is_err());
    assert!(call(&fixture.assistant, json!({ "action": "remove", "part": "trill" })).await.is_err());
    assert_eq!(fixture.makers.shelf.asked().len(), asked);
}
