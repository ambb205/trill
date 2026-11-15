//! Drives the DAW the way an AI client does: over the real MCP transport,
//! with nothing but tool calls.

use daw_dsp::BuiltinFactory;
use daw_engine::{AudioBackend, Session, SharedSession};
use daw_mcp::{HttpServer, serve_http};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::sync::Arc;

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    session: SharedSession,
    client: Client,
    _server: HttpServer,
}

async fn connect() -> Fixture {
    let session = Session::new(Arc::new(BuiltinFactory), AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    let session: SharedSession = Arc::new(parking_lot::Mutex::new(session));
    let server = serve_http(session.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { session, client, _server: server }
}

/// Calls a tool. `Ok` holds the parsed JSON reply; `Err` holds the message
/// of a tool-level error.
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

#[tokio::test(flavor = "multi_thread")]
async fn tools_are_listed_with_schemas() {
    let fixture = connect().await;
    let tools = fixture.client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    for expected in [
        "get_project",
        "set_project",
        "create_track",
        "update_track",
        "delete_track",
        "list_devices",
        "describe_device",
        "add_device",
        "get_device",
        "update_device",
        "delete_device",
        "create_clip",
        "create_audio_clip",
        "update_clip",
        "duplicate_clip",
        "delete_clip",
        "get_notes",
        "add_notes",
        "set_notes",
        "remove_notes",
        "transform_notes",
        "set_automation",
        "get_automation",
        "transport",
        "render",
        "history",
        "save_project",
        "load_project",
        "new_project",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
    for tool in &tools {
        assert!(tool.description.as_ref().is_some_and(|d| d.len() > 20), "{} needs a description", tool.name);
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        assert_eq!(schema["type"], "object", "{} has no object schema", tool.name);
    }
    let create_clip = tools.iter().find(|tool| tool.name == "create_clip").unwrap();
    let schema = serde_json::to_string(&create_clip.input_schema).unwrap();
    assert!(schema.contains("closed_hat"), "the note schema should teach drum names: {schema}");
    assert!(schema.contains("\"required\":[\"track\",\"start\",\"length\"]") || schema.contains("\"track\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_can_write_mix_and_render_a_song() {
    let fixture = connect().await;
    let client = &fixture.client;
    let dir = std::env::temp_dir().join(format!("daw-mcp-{}", std::process::id()));

    let start = ok(client, "get_project", json!({})).await;
    assert_eq!(start["tracks"].as_array().unwrap().len(), 0);
    assert_eq!(start["master"]["devices"][0]["uri"], "builtin:limiter");

    ok(client, "set_project", json!({ "name": "Night Drive", "tempo": 100, "time_signature": "4/4" })).await;

    // Drums: one bar, looped across four.
    let drums =
        ok(client, "create_track", json!({ "name": "Drums", "instrument": "builtin:drumkit", "preset": "909" })).await;
    let drum_track = drums["track_id"].as_u64().unwrap();
    let beat = ok(
        client,
        "create_clip",
        json!({
            "track": drum_track, "start": 0, "length": 16, "loop_length": 4, "name": "Beat",
            "notes": [
                ["kick", 0, 0.25, 120], ["kick", 2, 0.25, 115], ["kick", 2.75, 0.25, 90],
                ["snare", 1, 0.25, 110], ["snare", 3, 0.25, 112],
                ["closed_hat", 0.5, 0.1, 70], ["closed_hat", 1.5, 0.1, 80], ["closed_hat", 2.5, 0.1, 72],
                { "pitch": "open_hat", "start": 3.5, "length": 0.25, "velocity": 85 },
            ],
        }),
    )
    .await;
    assert_eq!(beat["note_count"], 9);
    assert!(beat.get("warning").is_none());

    // Bass and chords.
    let bass = ok(
        client,
        "create_track",
        json!({ "name": "Bass", "instrument": "builtin:polysynth", "preset": "bass", "volume_db": -3 }),
    )
    .await;
    let bass_track = bass["track_id"].as_u64().unwrap();
    let bass_clip = ok(
        client,
        "create_clip",
        json!({ "track": bass_track, "start": 0, "length": 8, "notes": [["A1", 0, 1.5], ["A1", 2, 0.5], ["C2", 4, 1.5], ["G1", 6, 1.5]] }),
    )
    .await["clip_id"]
        .as_u64()
        .unwrap();
    let copies = ok(client, "duplicate_clip", json!({ "clip": bass_clip })).await;
    assert_eq!(copies["copies"][0]["start"], 8.0);

    let pad = ok(
        client,
        "create_track",
        json!({ "name": "Pad", "instrument": "builtin:polysynth", "preset": "warm_pad", "pan": -0.2 }),
    )
    .await;
    let pad_track = pad["track_id"].as_u64().unwrap();
    let pad_synth = pad["instrument_device_id"].as_u64().unwrap();
    let pad_clip = ok(
        client,
        "create_clip",
        json!({
            "track": pad_track, "start": 0, "length": 16,
            "notes": [["A3", 0, 8, 80], ["C4", 0, 8, 80], ["E4", 0, 8, 80], ["G3", 8, 8, 80], ["B3", 8, 8, 80], ["D4", 8, 8, 80]],
        }),
    )
    .await["clip_id"]
        .as_u64()
        .unwrap();

    // Sound design, effects, automation.
    let reverb = ok(
        client,
        "add_device",
        json!({ "track": pad_track, "uri": "builtin:reverb", "params": { "decay": 4.0, "mix": 0.35 } }),
    )
    .await;
    assert!(reverb["device_id"].is_u64());
    ok(
        client,
        "update_device",
        json!({ "device": pad_synth, "params": { "filter_type": "lowpass24", "Filter Cutoff": 900 } }),
    )
    .await;
    let device = ok(client, "get_device", json!({ "device": pad_synth, "query": "cutoff" })).await;
    let cutoff = device["params"].as_array().unwrap().iter().find(|p| p["id"] == "cutoff").unwrap();
    assert_eq!(cutoff["value"], 900.0);
    assert_eq!(cutoff["unit"], "Hz");
    ok(
        client,
        "set_automation",
        json!({ "target": { "type": "device_param", "device": pad_synth, "param": "cutoff" }, "points": [[0, 400], [16, 6000]] }),
    )
    .await;
    ok(
        client,
        "transform_notes",
        json!({ "clip": pad_clip, "humanize_velocity": 10, "humanize_timing": 0.01, "seed": 7 }),
    )
    .await;

    // The model reflects all of it.
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["name"], "Night Drive");
    assert_eq!(project["tempo"], 100.0);
    assert_eq!(project["length_beats"], 16.0);
    assert_eq!(project["tracks"].as_array().unwrap().len(), 3);
    assert_eq!(project["tracks"][1]["clips"].as_array().unwrap().len(), 2);
    assert_eq!(project["automation"][0]["point_count"], 2);

    // Render and read the numbers back.
    let wav = dir.join("night-drive.wav");
    let report = ok(client, "render", json!({ "path": wav.display().to_string() })).await;
    assert!(report["duration_seconds"].as_f64().unwrap() >= 9.6, "16 beats at 100 BPM is 9.6 s: {report}");
    assert!(report["peak_db"].as_f64().unwrap() <= 0.0, "the master limiter keeps peaks under full scale");
    assert_eq!(report["clipped_samples"], 0);
    let loudness = report["loudness_lufs"].as_f64().unwrap();
    assert!((-40.0..-5.0).contains(&loudness), "implausible loudness {loudness}");
    let track_peaks: Vec<f64> =
        report["tracks"].as_array().unwrap().iter().map(|t| t["peak_db"].as_f64().unwrap()).collect();
    assert!(track_peaks.iter().all(|peak| *peak > -40.0), "every track should be audible: {track_peaks:?}");
    assert!(std::fs::metadata(&wav).unwrap().len() > 1_000_000);

    // Save, wipe, load.
    let file = dir.join("night-drive.dawproj");
    ok(client, "save_project", json!({ "path": file.display().to_string() })).await;
    let blank = ok(client, "new_project", json!({})).await;
    assert_eq!(blank["tracks"].as_array().unwrap().len(), 0);
    let loaded = ok(client, "load_project", json!({ "path": file.display().to_string() })).await;
    assert_eq!(loaded["tracks"].as_array().unwrap().len(), 3);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn mistakes_come_back_as_actionable_tool_errors() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    let track_id = track["track_id"].as_u64().unwrap();
    let synth = track["instrument_device_id"].as_u64().unwrap();
    let before = fixture.session.lock().project().clone();

    let error = call(client, "create_clip", json!({ "track": 999, "start": 0, "length": 4 })).await.unwrap_err();
    assert!(error.contains("track 999 not found") && error.contains("(Keys)"), "{error}");

    let error = call(
        client,
        "create_clip",
        json!({ "track": track_id, "start": 0, "length": 4, "notes": [["C4", 0, 1], ["X9", 1, 1]] }),
    )
    .await
    .unwrap_err();
    assert!(error.starts_with("notes[1]:"), "{error}");

    let error =
        call(client, "update_device", json!({ "device": synth, "params": { "cutoff": 500, "osc1_wave": "wobble" } }))
            .await
            .unwrap_err();
    assert!(error.contains("sine, triangle, saw, square"), "{error}");

    let error =
        call(client, "create_track", json!({ "name": "Nope", "instrument": "builtin:theremin" })).await.unwrap_err();
    assert!(error.contains("unknown device"), "{error}");

    let error = call(client, "set_project", json!({ "tempo": 5000 })).await.unwrap_err();
    assert!(error.contains("tempo"), "{error}");

    let error = call(
        client,
        "set_automation",
        json!({ "target": { "type": "track_pan", "track": track_id }, "points": [[0, 3]] }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("outside the allowed range"), "{error}");

    assert_eq!(fixture.session.lock().project(), &before, "failed calls must not change the project");

    // A note past the end of its clip is accepted but flagged.
    let clip = ok(
        client,
        "create_clip",
        json!({ "track": track_id, "start": 0, "length": 4, "notes": [["C4", 0, 1], ["E4", 6, 1]] }),
    )
    .await;
    assert!(clip["warning"].as_str().unwrap().contains("1 note(s)"), "{clip}");
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_are_undoable_and_transport_responds() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track =
        ok(client, "create_track", json!({ "name": "Lead", "instrument": "builtin:polysynth", "preset": "lead" }))
            .await;
    let track_id = track["track_id"].as_u64().unwrap();
    let clip = ok(
        client,
        "create_clip",
        json!({ "track": track_id, "start": 0, "length": 8, "notes": [[60, 0, 4], [64, 4, 4]] }),
    )
    .await;
    let clip_id = clip["clip_id"].as_u64().unwrap();

    ok(client, "transform_notes", json!({ "clip": clip_id, "transpose": 12 })).await;
    let notes = ok(client, "get_notes", json!({ "clip": clip_id })).await;
    assert_eq!(notes["notes"][0]["name"], "C5");
    ok(client, "remove_notes", json!({ "clip": clip_id, "pitch": "E5" })).await;
    assert_eq!(ok(client, "get_notes", json!({ "clip": clip_id })).await["note_count"], 1);

    let history = ok(client, "history", json!({ "action": "undo", "steps": 2 })).await;
    assert_eq!(history["steps_applied"], 2);
    let notes = ok(client, "get_notes", json!({ "clip": clip_id })).await;
    assert_eq!(notes["note_count"], 2);
    assert_eq!(notes["notes"][0]["name"], "C4");

    let playing = ok(client, "transport", json!({ "action": "play", "position": 2 })).await;
    assert_eq!(playing["playing"], true);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let status = ok(client, "transport", json!({ "action": "status" })).await;
    assert_eq!(status["playing"], true);
    assert!(status["position_beats"].as_f64().unwrap() > 2.1, "{status}");
    ok(client, "transport", json!({ "action": "stop" })).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(ok(client, "transport", json!({ "action": "status" })).await["playing"], false);

    let catalog = ok(client, "list_devices", json!({ "category": "instrument" })).await;
    let uris: Vec<&str> = catalog["devices"].as_array().unwrap().iter().map(|d| d["uri"].as_str().unwrap()).collect();
    // The built-in instruments lead the list; no effect slips through the filter.
    assert_eq!(uris[..2], ["builtin:polysynth", "builtin:drumkit"]);
    assert!(!uris.contains(&"builtin:reverb"));
    let reverb = ok(client, "describe_device", json!({ "uri": "builtin:reverb" })).await;
    assert!(reverb["params"].as_array().unwrap().iter().any(|p| p["id"] == "decay" && p["unit"] == "s"));
}

#[tokio::test(flavor = "multi_thread")]
async fn clips_can_be_split_trimmed_and_the_song_annotated() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    let track_id = track["track_id"].as_u64().unwrap();
    let clip = ok(
        client,
        "create_clip",
        json!({ "track": track_id, "start": 0, "length": 8, "notes": [["C4", 0, 1], ["E4", 2, 1], ["G4", 4, 1], ["C5", 6, 1]] }),
    )
    .await["clip_id"]
        .as_u64()
        .unwrap();

    // Split in the middle, then pull the second half's left edge in.
    let halves = ok(client, "split_clip", json!({ "clip": clip, "at": 4 })).await;
    assert_eq!(halves["first"]["length"], 4.0);
    let second = halves["second"]["clip_id"].as_u64().unwrap();
    assert_eq!(halves["second"]["start"], 4.0);
    let trimmed = ok(client, "update_clip", json!({ "clip": second, "trim_start": 5 })).await;
    assert_eq!((trimmed["start"].as_f64(), trimmed["length"].as_f64()), (Some(5.0), Some(3.0)));
    assert_eq!(trimmed["start_offset"], 5.0, "the clip now starts five beats into its notes");
    assert_eq!(trimmed["note_count"], 4, "trimming hides notes rather than deleting them");

    let error = call(client, "split_clip", json!({ "clip": clip, "at": 4 })).await.unwrap_err();
    assert!(error.contains("not inside the clip"), "{error}");
    let error = call(client, "update_clip", json!({ "clip": second, "start": 0, "trim_start": 6 })).await.unwrap_err();
    assert!(error.contains("not both"), "{error}");

    // Shaped automation on the master, and section markers.
    ok(
        client,
        "set_automation",
        json!({ "target": { "type": "master_volume" }, "points": [[0, -24, 0.6], [8, 0, "hold"], { "beat": 16, "value": -60 }] }),
    )
    .await;
    let lanes = ok(client, "get_automation", json!({})).await;
    assert_eq!(lanes["automation"][0]["target"]["type"], "master_volume");
    assert_eq!(lanes["automation"][0]["points"], json!([[0.0, -24.0, 0.6], [8.0, 0.0, "hold"], [16.0, -60.0]]));

    ok(client, "set_markers", json!({ "markers": [{ "beat": 8, "name": "Drop" }, { "beat": 0, "name": "Intro" }] }))
        .await;
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(
        project["markers"],
        json!([{ "beat": 0.0, "bar": 1.0, "name": "Intro" }, { "beat": 8.0, "bar": 3.0, "name": "Drop" }])
    );
    let error = call(client, "set_markers", json!({ "markers": [{ "beat": -4, "name": "Nope" }] })).await.unwrap_err();
    assert!(error.contains("marker position"), "{error}");
    assert_eq!(
        ok(client, "get_project", json!({})).await["markers"].as_array().unwrap().len(),
        2,
        "a bad list changes nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn midi_files_round_trip_through_the_song() {
    let fixture = connect().await;
    let client = &fixture.client;
    let dir = std::env::temp_dir().join(format!("daw-mcp-midi-{}", std::process::id()));
    let exported = dir.join("out.mid");

    // Write a song, export it, start over and import it again.
    ok(client, "set_project", json!({ "tempo": 132 })).await;
    let keys = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    ok(
        client,
        "create_clip",
        json!({ "track": keys["track_id"], "start": 0, "length": 4, "loop_length": 2, "notes": [["C4", 0, 1], ["G4", 1, 1]] }),
    )
    .await;
    let drums = ok(client, "create_track", json!({ "name": "Drums", "instrument": "builtin:drumkit" })).await;
    ok(client, "create_clip", json!({ "track": drums["track_id"], "start": 0, "length": 4, "notes": [["kick", 0, 0.25], ["snare", 1, 0.25]] })).await;

    let written = ok(client, "export_midi", json!({ "path": exported.display().to_string() })).await;
    assert_eq!(
        (written["tracks"].as_u64(), written["notes"].as_u64()),
        (Some(2), Some(6)),
        "the loop is written out twice"
    );

    ok(client, "new_project", json!({})).await;
    let imported = ok(client, "import_midi", json!({ "path": exported.display().to_string() })).await;
    assert_eq!(imported["tempo"], 132.0, "an empty project takes the file's tempo");
    let tracks = imported["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(
        (tracks[0]["name"].as_str(), tracks[0]["note_count"].as_u64(), tracks[0]["drums"].as_bool()),
        (Some("Keys"), Some(4), Some(false))
    );
    assert_eq!((tracks[1]["name"].as_str(), tracks[1]["drums"].as_bool()), (Some("Drums"), Some(true)));

    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["tracks"][1]["devices"][0]["uri"], "builtin:drumkit", "drum parts get the drum kit");
    assert_eq!(project["tracks"][0]["devices"][0]["uri"], "builtin:polysynth");

    // The whole import is one undo step.
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(ok(client, "get_project", json!({})).await["tracks"].as_array().unwrap().len(), 0);

    let error = call(client, "import_midi", json!({ "path": dir.join("missing.mid").display().to_string() }))
        .await
        .unwrap_err();
    assert!(error.contains("missing.mid"), "{error}");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn tracks_can_share_a_reverb_and_a_group() {
    let fixture = connect().await;
    let client = &fixture.client;
    let keys =
        ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await["track_id"]
            .as_u64()
            .unwrap();
    let verb =
        ok(client, "create_track", json!({ "name": "Verb", "kind": "return" })).await["track_id"].as_u64().unwrap();
    let bus =
        ok(client, "create_track", json!({ "name": "Music", "kind": "group" })).await["track_id"].as_u64().unwrap();
    ok(client, "add_device", json!({ "track": verb, "uri": "builtin:reverb", "params": { "mix": 1.0 } })).await;

    let routed =
        ok(client, "update_track", json!({ "track": keys, "group": bus, "send": { "to": verb, "level_db": -9 } }))
            .await;
    assert_eq!(routed["group"], bus);
    assert_eq!(routed["sends"], json!([{ "to": verb, "level_db": -9.0 }]));
    assert!(routed.get("clips").is_none());

    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["tracks"][1]["kind"], "return");
    assert_eq!(project["tracks"][2]["kind"], "group");
    assert!(project["tracks"][0].get("kind").is_none(), "normal tracks do not say so");

    // The rules come back as plain errors.
    let error = call(client, "create_clip", json!({ "track": verb, "start": 0, "length": 4 })).await.unwrap_err();
    assert!(error.contains("return track and does not hold clips"), "{error}");
    let error =
        call(client, "update_track", json!({ "track": keys, "send": { "to": bus, "level_db": 0 } })).await.unwrap_err();
    assert!(error.contains("is not a return track"), "{error}");

    // The reverb is heard: the render is longer than the note because of its tail.
    ok(client, "create_clip", json!({ "track": keys, "start": 0, "length": 2, "notes": [["C4", 0, 0.5]] })).await;
    let dir = std::env::temp_dir().join(format!("daw-mcp-routing-{}", std::process::id()));
    let report = ok(client, "render", json!({ "path": dir.join("r.wav").display().to_string() })).await;
    let levels: Vec<(String, f64)> = report["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| (track["name"].as_str().unwrap().to_string(), track["peak_db"].as_f64().unwrap()))
        .collect();
    assert!(levels.iter().all(|(_, peak)| *peak > -60.0), "keys, return and group all carry signal: {levels:?}");
    assert!(report["duration_seconds"].as_f64().unwrap() > 1.5, "the shared reverb rings on after the clip");
    std::fs::remove_dir_all(&dir).ok();

    // Removing the send and the group routes the track straight to the master again.
    let direct =
        ok(client, "update_track", json!({ "track": keys, "group": 0, "send": { "to": verb, "level_db": null } }))
            .await;
    assert!(direct.get("group").is_none() && direct.get("sends").is_none(), "{direct}");
}
