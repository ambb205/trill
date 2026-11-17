//! The wavetable synth as an AI client meets it: over the real MCP
//! transport, with nothing but tool calls. Found by words, shaped by
//! parameter, given tables it made itself, and heard through `probe`.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_engine::AudioBackend;
use trill_mcp::{HttpServer, serve_http};
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    runtime: Arc<Runtime>,
    client: Client,
    _server: HttpServer,
    /// Holds the runtime's settings and the files a test makes. Nothing of
    /// the user's is read or written.
    folder: tempfile::TempDir,
}

async fn connect() -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let storage = Storage::At {
        settings: folder.path().join("config").join("settings.json"),
        library_index: Some(folder.path().join("cache").join("library-index.json")),
    };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let runtime = Arc::new(Runtime::start(options).unwrap());
    let server = serve_http(runtime.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { runtime, client, _server: server, folder }
}

async fn call(client: &Client, tool: &'static str, arguments: Value) -> Result<Value, String> {
    let arguments = arguments.as_object().cloned().unwrap();
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .unwrap_or_else(|error| panic!("{tool}: protocol error {error}"));
    let text = result.content.first().and_then(|block| block.as_text()).map(|t| t.text.clone()).unwrap_or_default();
    if result.is_error == Some(true) {
        Err(text)
    } else {
        Ok(serde_json::from_str(&text).unwrap_or_else(|_| panic!("{tool} replied with non-JSON: {text}")))
    }
}

async fn ok(client: &Client, tool: &'static str, arguments: Value) -> Value {
    call(client, tool, arguments).await.unwrap_or_else(|error| panic!("{tool} failed: {error}"))
}

async fn refused(client: &Client, tool: &'static str, arguments: Value) -> String {
    match call(client, tool, arguments.clone()).await {
        Err(error) => error,
        Ok(reply) => panic!("{tool} {arguments} should have been refused, and answered {reply}"),
    }
}

/// A track with a wavetable synth on it, from `preset`; its track and
/// device ids.
async fn synth(client: &Client, name: &str, preset: &str) -> (u64, u64) {
    let made =
        ok(client, "create_track", json!({ "name": name, "instrument": "builtin:wavetable", "preset": preset })).await;
    (made["track_id"].as_u64().unwrap(), made["instrument_device_id"].as_u64().unwrap())
}

/// Writes `samples` at 44.1 kHz and returns the file's path as text.
fn wav(path: PathBuf, samples: Vec<f32>) -> String {
    let buffer = AudioBuffer { sample_rate: 44_100.0, left: samples.clone(), right: samples };
    write_wav(&path, &buffer, WavFormat::Float32).unwrap();
    path.display().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_synth_is_found_by_words_and_says_what_it_is() {
    let fixture = connect().await;
    let client = &fixture.client;

    // A sound is found by what it is like, across every instrument.
    let found = ok(client, "search_presets", json!({ "query": "reese bass" })).await;
    let first = &found["items"][0];
    assert_eq!(
        (first["preset"].as_str(), first["device"].as_str()),
        (Some("Reese"), Some("builtin:wavetable")),
        "{found}"
    );
    for (words, preset) in [("electric piano tine", "Tine Piano"), ("glass pad", "Glass Pad"), ("303", "Acid Line")] {
        let found = ok(client, "search_presets", json!({ "query": words, "device": "wavetable" })).await;
        let names: Vec<&str> =
            found["items"].as_array().unwrap().iter().filter_map(|item| item["preset"].as_str()).collect();
        assert!(names.contains(&preset), "'{words}' should find {preset}: {found}");
    }

    // The device says enough to be used without a probe: what it is, what
    // every parameter does, and its presets with a sentence each.
    let described = ok(client, "describe_device", json!({ "uri": "builtin:wavetable", "preset_limit": 100 })).await;
    let text = described["description"].as_str().unwrap();
    for word in ["osc1_position", "modN_source", "macro1", "set_wavetable", "no effects"] {
        assert!(text.contains(word), "the description should mention {word}");
    }
    let params = described["params"].as_array().unwrap();
    assert!(params.len() > 130, "{}", params.len());
    let param = |id: &str| params.iter().find(|param| param["id"] == id).unwrap_or_else(|| panic!("no {id}"));
    assert!(param("osc1_table")["choices"].as_array().unwrap().iter().any(|choice| choice == "Acid"));
    assert!(param("mod1_amount")["name"].as_str().unwrap().contains("48 st of pitch"));
    assert_eq!(param("filter1_cutoff")["unit"], "Hz");
    let presets = described["presets"]["items"].as_array().unwrap();
    assert!(presets.len() >= 16, "{}", presets.len());
    let solid = presets.iter().find(|preset| preset["name"] == "Solid Bass").unwrap();
    assert_eq!(solid["category"], "Bass");
    assert!(solid["description"].as_str().unwrap().starts_with("A firm, round analog bass"));
    // And the instructions say when to reach for it.
    let instructions = client.peer_info().unwrap().instructions.clone().unwrap_or_default();
    for word in ["builtin:wavetable", "describe_wavetable", "set_wavetable", "builtin:polysynth", "macro1"] {
        assert!(instructions.contains(word), "the instructions should mention {word}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sound_is_read_and_shaped_as_data() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (track, device) = synth(client, "Bass", "Solid Bass").await;

    // What the preset is: its tables, its matrix in words, its macros by
    // name with what each moves, and what it says of itself in full.
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!(got["preset"], "Solid Bass");
    let about = &got["wavetable"];
    assert_eq!(about["osc1_table"]["name"], "Basic Shapes");
    let matrix: Vec<&str> = about["matrix"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(matrix.contains(&"env2 -> filter1_cutoff 0.32"), "{matrix:?}");
    let macros = about["macros"].as_array().unwrap();
    assert_eq!((macros[0]["name"].as_str(), macros[0]["param"].as_str()), (Some("Cutoff"), Some("macro1")));
    assert_eq!(macros[0]["moves"][0], "filter1_cutoff +0.35");
    assert_eq!(macros[2]["name"], "Sub");
    assert!(about["preset_says"].as_str().unwrap().contains("Macros: Cutoff opens it"));
    // The parameters that were moved from their defaults are listed, a
    // choice by its name.
    let params = got["params"].as_array().unwrap();
    let value = |id: &str| params.iter().find(|param| param["id"] == id).map(|param| param["value"].clone());
    assert_eq!(value("filter1_cutoff"), Some(json!(160.0)));
    assert!(params.len() < 60, "only what was moved: {}", params.len());

    // A macro is a parameter: set, and drawn over a section.
    let set = ok(client, "update_device", json!({ "device": device, "params": { "macro1": 0.6 } })).await;
    assert_eq!(set["set"]["macro1"], 0.6);
    ok(
        client,
        "set_automation",
        json!({ "target": { "type": "device_param", "device": device, "param": "macro1" },
                "points": [{ "beat": 0, "value": 0 }, { "beat": 8, "value": 1 }] }),
    )
    .await;
    // A slot of the matrix is written by names; a choice that is no choice
    // says what the choices are.
    let slot = json!({ "mod8_source": "lfo1", "mod8_dest": "osc1_position", "mod8_amount": 0.25, "lfo1_sync": "1/8" });
    ok(client, "update_device", json!({ "device": device, "params": slot })).await;
    let got = ok(client, "get_device", json!({ "device": device })).await;
    let matrix: Vec<&str> = got["wavetable"]["matrix"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(matrix.contains(&"lfo1 -> osc1_position 0.25"), "{matrix:?}");
    let error =
        refused(client, "update_device", json!({ "device": device, "params": { "mod8_dest": "cutoff" } })).await;
    assert!(error.contains("filter1_cutoff"), "{error}");

    // The macros are renamed, and one that does nothing yet says so.
    let named = ok(client, "set_wavetable", json!({ "device": device, "macros": ["Open", null, "", "Ring"] })).await;
    assert_eq!(named["macros"], json!(["Open", "Drive", "Macro 3", "Ring"]));
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!(got["wavetable"]["macros"][0]["name"], "Open");

    // It is heard through a probe: the note sounds at its pitch, and the
    // macro that opens the filter makes it brighter.
    let heard = |reply: &Value| {
        let note = &reply["heard"][0];
        (note["heard"].as_str().map(str::to_string), note["bright_hz"].as_f64().unwrap_or(0.0))
    };
    ok(client, "update_device", json!({ "device": device, "params": { "macro1": 0.0 } })).await;
    let closed = ok(client, "probe", json!({ "track": track, "notes": ["E2"] })).await;
    ok(client, "update_device", json!({ "device": device, "params": { "macro1": 1.0 } })).await;
    let open = ok(client, "probe", json!({ "track": track, "notes": ["E2"] })).await;
    assert_eq!(heard(&closed).0.as_deref(), Some("E2"), "{closed}");
    assert!(heard(&open).1 > heard(&closed).1 * 1.1, "open {open}, closed {closed}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_is_chosen_looked_at_and_made() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (_, device) = synth(client, "Lead", "init").await;

    // The tables that ship, each with a sentence; and how one is made.
    let listed = ok(client, "describe_wavetable", json!({})).await;
    let tables = listed["tables"].as_array().unwrap();
    assert_eq!(tables.len(), 22);
    assert!(tables.iter().any(|table| table["name"] == "Growl" && table["about"].as_str().unwrap().contains("phaser")));
    assert!(listed["making"].as_str().unwrap().contains("set_wavetable"));
    // One of them, frame by frame, with the recipe it is made from.
    let acid = ok(client, "describe_wavetable", json!({ "table": "acid" })).await;
    assert_eq!((acid["name"].as_str(), acid["frames"].as_u64()), (Some("Acid"), Some(24)));
    let at = acid["at"].as_array().unwrap();
    assert_eq!(at.len(), 9);
    assert!(at[8]["bright"].as_f64().unwrap() > at[0]["bright"].as_f64().unwrap() * 3.0, "{at:?}");
    assert_eq!(acid["recipe"]["frames"][0]["shape"], "spectrum");
    assert!(acid["reading"].as_str().unwrap().contains("`bright`"));
    let error = refused(client, "describe_wavetable", json!({ "table": "Nope" })).await;
    assert!(error.contains("Basic Shapes") && error.contains("Strings"), "{error}");

    // The oscillator plays one that ships, by its name.
    let chosen = ok(client, "set_wavetable", json!({ "device": device, "table": "Formant Sweep" })).await;
    assert_eq!((chosen["table"]["name"].as_str(), chosen["oscillator"].as_u64()), (Some("Formant Sweep"), Some(1)));
    let got = ok(client, "get_device", json!({ "device": device, "query": "osc1_table" })).await;
    assert_eq!(got["wavetable"]["osc1_table"]["name"], "Formant Sweep");

    // A recipe is looked at before it is given to anything: a pulse that
    // narrows gets brighter and quieter against its peak.
    let recipe = json!({ "name": "Thin Reed", "size": 16,
        "frames": [{ "shape": "pulse", "width": 0.5 }, { "shape": "pulse", "width": 0.04, "tilt": -3 }] });
    let looked = ok(client, "describe_wavetable", json!({ "recipe": recipe })).await;
    let at = looked["at"].as_array().unwrap();
    assert_eq!((looked["name"].as_str(), looked["frames"].as_u64()), (Some("Thin Reed"), Some(16)));
    assert!(
        at[8]["bright"].as_f64() > at[0]["bright"].as_f64() && at[8]["level_db"].as_f64() < at[0]["level_db"].as_f64()
    );
    assert!((at[0]["odd"].as_f64().unwrap() - 1.0).abs() < 0.01, "a square is all odd harmonics");
    let before = ok(client, "get_device", json!({ "device": device, "query": "osc1_table" })).await;
    assert_eq!(before["wavetable"]["osc1_table"]["name"], "Formant Sweep", "looking changed nothing");

    // Given to the second oscillator, it is that oscillator's own: one
    // undo step, and the device says what it plays.
    let made = ok(client, "set_wavetable", json!({ "device": device, "oscillator": 2, "recipe": recipe })).await;
    assert_eq!((made["table"]["name"].as_str(), made["table"]["own"].as_bool()), (Some("Thin Reed"), Some(true)));
    let got = ok(client, "describe_wavetable", json!({ "device": device, "oscillator": 2 })).await;
    assert_eq!((got["name"].as_str(), got["recipe"]["size"].as_u64()), (Some("Thin Reed"), Some(16)));
    ok(client, "history", json!({ "action": "undo" })).await;
    let undone = ok(client, "describe_wavetable", json!({ "device": device, "oscillator": 2 })).await;
    assert_eq!(undone["name"], "Basic Shapes");

    // What is wrong says what would be right, and changes nothing.
    for (arguments, expected) in [
        (json!({ "device": device }), "needs `table`"),
        (json!({ "device": device, "table": "Acid", "file": "x.wav" }), "give one of"),
        (json!({ "device": device, "table": "Bogus" }), "the tables are: Basic Shapes"),
        (json!({ "device": device, "recipe": { "frames": [{ "shape": "sawtooth" }] } }), "one of sine, triangle, saw"),
        (json!({ "device": device, "recipe": { "frames": [{ "shape": "sine", "ratio": 2 }] } }), "does not take"),
        (json!({ "device": device, "recipe": { "frames": [] } }), "at least one frame"),
        (json!({ "device": device, "recipe": recipe, "cycle": 2048 }), "how a `file` is read"),
        (json!({ "device": device, "oscillator": 3, "table": "Acid" }), "oscillators 1 and 2"),
        (json!({ "device": device, "macros": ["a", "b", "c", "d", "e"] }), "four macros"),
        (json!({ "device": 999, "table": "Acid" }), "device 999 not found"),
    ] {
        let error = refused(client, "set_wavetable", arguments.clone()).await;
        assert!(error.contains(expected), "{arguments}: '{error}' should say '{expected}'");
    }
    // A device that is no wavetable synth says what would be one.
    let other = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    let poly = other["instrument_device_id"].as_u64().unwrap();
    let error = refused(client, "set_wavetable", json!({ "device": poly, "table": "Acid" })).await;
    assert!(error.contains("not a wavetable synth") && error.contains("builtin:wavetable"), "{error}");
    // The calls are entered in the journal in words.
    let journal = ok(client, "get_journal", json!({})).await.to_string();
    assert!(
        journal.contains("Gave \u{201c}Wavetable\u{201d} the wavetable \u{201c}Formant Sweep\u{201d}"),
        "{journal}"
    );
    assert!(
        journal.contains("Made the wavetable \u{201c}Thin Reed\u{201d} for \u{201c}Wavetable\u{201d}"),
        "{journal}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_from_a_file_is_one_of_the_songs_files() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (_, device) = synth(client, "Pad", "init").await;
    let tables = fixture.folder.path().join("Tables");
    std::fs::create_dir_all(&tables).unwrap();

    // A wavetable file: eight cycles of 2048 samples, each a harmonic more.
    let cycles: Vec<f32> = (0..8 * 2048)
        .map(|n| {
            let (cycle, phase) = (n / 2048, (n % 2048) as f32 / 2048.0);
            (0..=cycle).map(|h| ((h + 1) as f32 * std::f32::consts::TAU * phase).sin() / (h + 1) as f32).sum::<f32>()
                * 0.4
        })
        .collect();
    let file = wav(tables.join("Stack.wav"), cycles);
    let read = ok(client, "set_wavetable", json!({ "device": device, "file": file })).await;
    assert_eq!((read["table"]["name"].as_str(), read["table"]["frames"].as_u64()), (Some("Stack"), Some(8)));
    assert!(read["table"]["read_as"].as_str().unwrap().contains("single cycles of 2048"));
    assert_eq!(read["table"]["at"][7]["top"], 8, "the eighth cycle has eight harmonics");

    // A recording of a held note, read as its harmonics along its length.
    let note: Vec<f32> = (0..2 * 44_100)
        .map(|n| {
            let turn = std::f32::consts::TAU * 196.0 * n as f32 / 44_100.0;
            0.4 * turn.sin() + 0.4 * (n as f32 / 88_200.0) * (2.0 * turn).sin()
        })
        .collect();
    let recording = wav(tables.join("Held G.wav"), note);
    let read =
        ok(client, "set_wavetable", json!({ "device": device, "oscillator": 2, "file": recording, "frames": 12 }))
            .await;
    assert_eq!(read["table"]["frames"], 12);
    assert!(read["table"]["read_as"].as_str().unwrap().contains("196."), "{read}");
    // Told the note, it need not find it.
    let told = json!({ "device": device, "oscillator": 2, "file": recording, "root": 55, "frames": 4 });
    assert!(ok(client, "set_wavetable", told).await["table"]["read_as"].as_str().unwrap().contains("196.0 Hz"));

    // A file that is no table says why, and the oscillator keeps what it had.
    let silent = wav(tables.join("Silent.wav"), vec![0.0; 4096]);
    let error = refused(client, "set_wavetable", json!({ "device": device, "file": silent })).await;
    assert!(error.contains("is silent") && error.contains("keeps the table it had"), "{error}");
    let missing = tables.join("Gone.wav").display().to_string();
    assert!(refused(client, "set_wavetable", json!({ "device": device, "file": missing })).await.contains("Gone.wav"));
    assert_eq!(ok(client, "get_device", json!({ "device": device })).await["wavetable"]["osc1_table"]["name"], "Stack");

    // The song is saved; its folder of tables is renamed. The file is said
    // to be missing once, found where the folder went, and relinked.
    let project = fixture.folder.path().join("song.trill").display().to_string();
    ok(client, "save_project", json!({ "path": project })).await;
    let moved = fixture.folder.path().join("Wavetables");
    std::fs::rename(&tables, &moved).unwrap();
    ok(client, "load_project", json!({ "path": project })).await;
    let overview = ok(client, "get_project", json!({})).await;
    assert_eq!(
        (overview["missing_files"]["files"].as_u64(), overview["missing_files"]["places"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(overview["missing_files"]["tracks"], json!(["Pad"]), "{overview}");
    let looked = ok(client, "missing_files", json!({ "look_in": [moved.display().to_string()] })).await;
    let found = looked.to_string();
    assert!(found.contains("osc1 table") && found.contains("Wavetables"), "{looked}");
    let relinked = ok(client, "relink_files", json!({ "look_in": [moved.display().to_string()] })).await;
    assert!(relinked.to_string().contains("Stack.wav"), "{relinked}");
    let device = ok(client, "get_project", json!({})).await["tracks"][0]["devices"][0]["id"].as_u64().unwrap();
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!(got["wavetable"]["osc1_table"]["name"], "Stack");
    assert!(got["wavetable"]["osc1_table"]["file"].as_str().unwrap().contains("Wavetables"), "{got}");
    assert!(fixture.runtime.session.lock().problems().is_empty());
}
