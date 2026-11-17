//! Drives the DAW the way an AI client does: over the real MCP transport,
//! with nothing but tool calls.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_engine::host::scripted::{ScriptedDevice, ScriptedHost};
use trill_engine::{AudioBackend, SharedSession};
use trill_mcp::{HttpServer, serve_http};
use trill_media::{WavFormat, decode, write_wav};
use trill_runtime::midi::scripted::ScriptedMidi;
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    session: SharedSession,
    runtime: Arc<Runtime>,
    client: Client,
    _server: HttpServer,
    /// Holds the runtime's settings and library index, and whatever files
    /// a test makes. Nothing of the user's is read or written.
    folder: tempfile::TempDir,
}

impl Fixture {
    fn settings_file(&self) -> PathBuf {
        self.folder.path().join("config").join("settings.json")
    }
}

async fn connect() -> Fixture {
    connect_in(tempfile::tempdir().unwrap()).await
}

/// A runtime that keeps everything it keeps under `folder`.
fn options_in(folder: &std::path::Path) -> RuntimeOptions {
    let storage = Storage::At {
        settings: folder.join("config").join("settings.json"),
        library_index: Some(folder.join("cache").join("library-index.json")),
    };
    RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage }
}

/// A server and a client of it, with whatever `folder` already holds.
async fn connect_in(folder: tempfile::TempDir) -> Fixture {
    connect_with(folder, Parts::default()).await
}

/// The same, with some of the runtime's parts replaced: audio devices and
/// MIDI ports that exist only in the test.
async fn connect_with(folder: tempfile::TempDir, parts: Parts) -> Fixture {
    let runtime = Arc::new(Runtime::start_custom(options_in(folder.path()), parts).unwrap());
    let server = serve_http(runtime.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { session: runtime.session.clone(), runtime, client, _server: server, folder }
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
        "set_tempo",
        "set_time_signature",
        "get_tempo_map",
        "create_track",
        "update_track",
        "delete_track",
        "list_devices",
        "describe_device",
        "add_device",
        "get_device",
        "update_device",
        "delete_device",
        "search_presets",
        "save_preset",
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
        "search_sounds",
        "library",
        "preview_sound",
        "set_sample",
        "describe_wavetable",
        "set_wavetable",
        "missing_files",
        "relink_files",
        "instrument_catalogue",
        "song_credits",
        "listen",
        "transcribe",
        "edit_time",
        "show_device_editor",
        "recover_project",
        "transport",
        "audio",
        "midi_inputs",
        "play_notes",
        "render",
        "measure",
        "probe",
        "get_instrument_profile",
        "set_instrument_profile",
        "history",
        "get_journal",
        "get_song_at",
        "journal",
        "film_plan",
        "film",
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
    // The sound tools teach their own vocabulary: tags, warp modes, pads.
    let schema_of =
        |name: &str| serde_json::to_string(&tools.iter().find(|tool| tool.name == name).unwrap().input_schema).unwrap();
    assert!(schema_of("search_sounds").contains("one-shot") && schema_of("search_sounds").contains("tempo_min"));
    for mode in ["off", "repitch", "beats", "tonal"] {
        assert!(schema_of("create_audio_clip").contains(mode) && schema_of("update_clip").contains(mode), "{mode}");
    }
    assert!(schema_of("set_sample").contains("closed_hat") && schema_of("library").contains("add_folder"));
    // And what to do about a file that is gone.
    assert!(schema_of("library").contains("move_folder") && schema_of("relink_files").contains("likely"));
    assert!(schema_of("missing_files").contains("look_in"));
    let info = fixture.client.peer_info().unwrap();
    let instructions = info.instructions.clone().unwrap_or_default();
    for word in ["search_sounds", "preview_sound", "builtin:drumrack", "set_sample", "create_audio_clip", "warp"] {
        assert!(instructions.contains(word), "the instructions should mention {word}");
    }
    for word in ["missing_files", "relink_files", "move_folder", "A file of the same name can be another file"] {
        assert!(instructions.contains(word), "the instructions should mention {word}");
    }
    // And playing live: arming, recording, the devices.
    for word in ["armed", "record", "play_notes", "midi_inputs", "count-in"] {
        assert!(instructions.contains(word), "the instructions should mention {word}");
    }
    assert!(schema_of("update_track").contains("armed") && schema_of("transport").contains("count_in_bars"));
    // Automation: what can be moved, how a line travels, and how to read a long envelope.
    let automation = tools.iter().find(|tool| tool.name == "set_automation").unwrap();
    let described = automation.description.clone().unwrap_or_default();
    for word in ["send", "fader", "-16 dB half way", "\"gain\"", "Ableton Live", "1.6 kHz", "by ratio", "throw"] {
        assert!(described.contains(word), "set_automation should say '{word}': {described}");
    }
    assert!(schema_of("set_automation").contains("\"send\"") && schema_of("set_automation").contains("travel"));
    assert!(schema_of("get_automation").contains("offset") && schema_of("get_automation").contains("limit"));
    assert!(instructions.contains("throw") && instructions.contains("control's travel"), "{instructions}");
    // And how the song was made: the journal, and the song as it was.
    for word in ["get_journal", "get_song_at", "takes nothing out of the journal", "never in the project file"] {
        assert!(instructions.contains(word), "the instructions should mention {word}");
    }
    assert!(schema_of("get_journal").contains("search") && schema_of("get_song_at").contains("step"));
    assert!(schema_of("journal").contains("export") && schema_of("journal").contains("conversation"));
    assert!(schema_of("audio").contains("exclusive") && schema_of("midi_inputs").contains("disable"));
    let create_clip = tools.iter().find(|tool| tool.name == "create_clip").unwrap();
    let schema = serde_json::to_string(&create_clip.input_schema).unwrap();
    assert!(schema.contains("closed_hat"), "the note schema should teach drum names: {schema}");
    assert!(schema.contains("\"required\":[\"track\",\"start\",\"length\"]") || schema.contains("\"track\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_can_write_mix_and_render_a_song() {
    let fixture = connect().await;
    let client = &fixture.client;
    let dir = std::env::temp_dir().join(format!("trill-mcp-{}", std::process::id()));

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
    let file = dir.join("night-drive.trill");
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
    let dir = std::env::temp_dir().join(format!("trill-mcp-midi-{}", std::process::id()));
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
    let dir = std::env::temp_dir().join(format!("trill-mcp-routing-{}", std::process::id()));
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

const SR: f64 = 48_000.0;

fn write(path: PathBuf, samples: Vec<f32>) -> String {
    let buffer = AudioBuffer { sample_rate: SR, left: samples.clone(), right: samples };
    write_wav(&path, &buffer, WavFormat::Int24).unwrap();
    path.display().to_string()
}

/// A drum hit: a tone at `hz` dying away over about `seconds`.
fn hit(hz: f64, seconds: f64) -> Vec<f32> {
    (0..(seconds * SR) as usize)
        .map(|n| {
            let t = n as f64 / SR;
            ((std::f64::consts::TAU * hz * t).sin() * 0.8 * (-t * 5.0 / seconds).exp()) as f32
        })
        .collect()
}

/// `beats` beats at `tempo` with a click half way through each.
fn click_loop(tempo: f64, beats: usize) -> Vec<f32> {
    let beat = 60.0 / tempo * SR;
    let mut signal = vec![0.0f32; (beats as f64 * beat).round() as usize];
    for index in 0..beats {
        let at = ((index as f64 + 0.5) * beat).round() as usize;
        for (offset, value) in [0.9f32, -0.7, 0.4, -0.2].into_iter().enumerate() {
            signal[at + offset] = value;
        }
    }
    signal
}

struct Pack {
    root: PathBuf,
    kick: String,
    snare: String,
    hat: String,
    drum_loop: String,
    bass_loop: String,
}

/// A small sample pack, named the way packs are.
fn pack(folder: &Path) -> Pack {
    let root = folder.join("My Samples");
    let tone: Vec<f32> = (0..(2.4 * SR) as usize)
        .map(|n| ((std::f64::consts::TAU * 110.0 * n as f64 / SR).sin() * 0.4) as f32)
        .collect();
    Pack {
        kick: write(root.join("Drums").join("Kick 01.wav"), hit(60.0, 0.3)),
        snare: write(root.join("Drums").join("Snare Tight.wav"), hit(1_000.0, 0.2)),
        hat: write(root.join("Drums").join("Closed Hat.wav"), hit(6_000.0, 0.05)),
        drum_loop: write(root.join("Loops").join("Drum Loop 100bpm.wav"), click_loop(100.0, 8)),
        bass_loop: write(root.join("Loops").join("Bass Loop 100bpm Am.wav"), tone),
        root,
    }
}

fn names(reply: &Value) -> Vec<&str> {
    reply["sounds"].as_array().unwrap().iter().map(|sound| sound["name"].as_str().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_finds_sounds_builds_a_beat_and_warps_a_loop() {
    let fixture = connect().await;
    let client = &fixture.client;
    let pack = pack(fixture.folder.path());
    let root = pack.root.display().to_string();

    // An empty library says what to do about it.
    let empty = ok(client, "search_sounds", json!({ "text": "kick" })).await;
    assert_eq!(empty["total"], 0);
    assert!(empty["hint"].as_str().unwrap().contains("add_folder"), "{empty}");
    let status = ok(client, "library", json!({ "action": "status" })).await;
    assert_eq!((status["folders"].as_array().unwrap().len(), status["scanning"].as_bool()), (0, Some(false)));
    assert!(ok(client, "library", json!({ "action": "suggestions" })).await["suggestions"].is_array());
    ok(client, "library", json!({ "action": "rescan" })).await;
    assert!(!fixture.settings_file().exists(), "looking at the library must not write the user's settings");

    // Adding the folder scans it, and it is remembered on disk at once.
    let added = ok(client, "library", json!({ "action": "add_folder", "path": root })).await;
    assert_eq!(
        (added["added"].as_bool(), added["scanning"].as_bool(), added["sounds"].as_u64()),
        (Some(true), Some(false), Some(5))
    );
    assert_eq!(added["folders"], json!([root]));
    let saved = trill_library::Settings::load(&fixture.settings_file()).settings;
    assert_eq!(saved.library.roots, std::slice::from_ref(&pack.root), "the folder is in the settings file");
    let again = ok(client, "library", json!({ "action": "add_folder", "path": root })).await;
    assert_eq!(again["added"], false);

    // Finding: by word, by tag and tempo, by key, by length.
    let kicks = ok(client, "search_sounds", json!({ "text": "kick" })).await;
    assert_eq!(names(&kicks), ["Kick 01"]);
    assert_eq!(kicks["sounds"][0]["path"], pack.kick);
    assert!((kicks["sounds"][0]["seconds"].as_f64().unwrap() - 0.3).abs() < 0.001);
    assert!(kicks["sounds"][0]["tags"].as_array().unwrap().contains(&json!("kick")), "{kicks}");
    let loops = ok(client, "search_sounds", json!({ "tags": ["loop"], "tempo_min": 90, "tempo_max": 110 })).await;
    assert_eq!(names(&loops), ["Bass Loop 100bpm Am", "Drum Loop 100bpm"]);
    assert_eq!((loops["sounds"][0]["tempo"].as_f64(), loops["sounds"][0]["key"].as_str()), (Some(100.0), Some("Am")));
    assert_eq!(names(&ok(client, "search_sounds", json!({ "key": "A minor" })).await), ["Bass Loop 100bpm Am"]);
    let hits = ok(client, "search_sounds", json!({ "seconds_max": 1.0, "folder": pack.root.join("Drums") })).await;
    assert_eq!(hits["total"], 3);
    let few = ok(client, "search_sounds", json!({ "limit": 2 })).await;
    assert_eq!((few["total"].as_u64(), few["sounds"].as_array().unwrap().len()), (Some(5), 2));
    assert!(few["truncated"].as_str().unwrap().contains("2 of 5"));

    // Previewing plays it to the user and says what it is.
    let preview = ok(client, "preview_sound", json!({ "path": pack.bass_loop, "gain_db": -6 })).await;
    assert_eq!(
        (preview["playing"].as_bool(), preview["tempo"].as_f64(), preview["key"].as_str()),
        (Some(true), Some(100.0), Some("Am"))
    );
    assert!(
        (preview["seconds"].as_f64().unwrap() - 2.4).abs() < 0.001
            && (preview["beats"].as_f64().unwrap() - 4.0).abs() < 0.01
    );
    for _ in 0..200 {
        if fixture.session.lock().is_auditioning() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(fixture.session.lock().is_auditioning(), "the preview is playing");
    assert_eq!(ok(client, "preview_sound", json!({})).await["playing"], false);
    assert_eq!(fixture.runtime.library.recent(), [PathBuf::from(&pack.bass_loop)]);

    // A drum rack from the found hits, and a beat written with drum names.
    ok(client, "set_project", json!({ "tempo": 120 })).await;
    let limiter = ok(client, "get_project", json!({})).await["master"]["devices"][0]["id"].as_u64().unwrap();
    // The limiter's look-ahead would shift every measurement below.
    ok(client, "delete_device", json!({ "device": limiter })).await;
    let drums = ok(client, "create_track", json!({ "name": "Drums", "instrument": "builtin:drumrack" })).await;
    let (drum_track, rack) = (drums["track_id"].as_u64().unwrap(), drums["instrument_device_id"].as_u64().unwrap());
    let empty_rack = ok(client, "get_device", json!({ "device": rack, "query": "pad36" })).await;
    assert!(empty_rack["samples"].as_array().unwrap().is_empty() && empty_rack["samples_hint"].is_string());
    ok(client, "set_sample", json!({ "device": rack, "pad": "kick", "path": pack.kick })).await;
    ok(client, "set_sample", json!({ "device": rack, "pad": "snare", "path": pack.snare })).await;
    let loaded = ok(client, "set_sample", json!({ "device": rack, "pad": 42, "path": pack.hat })).await;
    assert_eq!(
        loaded["samples"],
        json!([
            { "slot": "pad 36", "path": pack.kick },
            { "slot": "pad 38", "path": pack.snare },
            { "slot": "pad 42", "path": pack.hat },
        ])
    );
    ok(
        client,
        "create_clip",
        json!({
            "track": drum_track, "start": 0, "length": 8, "loop_length": 4,
            "notes": [["kick", 0, 0.25], ["kick", 2, 0.25], ["snare", 1, 0.25], ["snare", 3, 0.25],
                      ["closed_hat", 0.5, 0.1], ["closed_hat", 1.5, 0.1], ["closed_hat", 2.5, 0.1], ["closed_hat", 3.5, 0.1]],
        }),
    )
    .await;

    // The drum loop, warped: its eight beats become eight beats of the song.
    let loop_track = ok(client, "create_track", json!({ "name": "Loop" })).await["track_id"].as_u64().unwrap();
    let placed = ok(
        client,
        "create_audio_clip",
        json!({ "track": loop_track, "path": pack.drum_loop, "start": 0, "warp": "beats" }),
    )
    .await;
    assert_eq!(
        (placed["length"].as_f64(), placed["source_tempo"].as_f64(), placed["warp"].as_str()),
        (Some(8.0), Some(100.0), Some("beats"))
    );
    assert_eq!(placed["audio_beats"], 8.0);
    let loop_clip = placed["clip_id"].as_u64().unwrap();

    // The bass loop goes in plain first, then is warped and transposed.
    let bass_track = ok(client, "create_track", json!({ "name": "Bass" })).await["track_id"].as_u64().unwrap();
    let plain =
        ok(client, "create_audio_clip", json!({ "track": bass_track, "path": pack.bass_loop, "start": 0 })).await;
    assert!((plain["length"].as_f64().unwrap() - 4.8).abs() < 0.001, "2.4 s is 4.8 beats at 120 BPM: {plain}");
    assert!(plain.get("warp").is_none() && plain.get("notes").is_none(), "{plain}");
    let bass_clip = plain["clip_id"].as_u64().unwrap();
    let warped = ok(client, "update_clip", json!({ "clip": bass_clip, "warp": "tonal" })).await;
    assert_eq!(
        (warped["warp"].as_str(), warped["source_tempo"].as_f64(), warped["audio_beats"].as_f64()),
        (Some("tonal"), Some(100.0), Some(4.0))
    );
    assert!(warped["notes"][0].as_str().unwrap().contains("set length to 4"), "{warped}");
    let fitted = ok(client, "update_clip", json!({ "clip": bass_clip, "length": 4, "transpose": 3 })).await;
    assert_eq!((fitted["length"].as_f64(), fitted["transpose"].as_f64()), (Some(4.0), Some(3.0)));
    ok(client, "duplicate_clip", json!({ "clip": bass_clip })).await;

    let project = ok(client, "get_project", json!({})).await;
    let shown = &project["tracks"][1]["clips"][0];
    assert_eq!(
        (shown["type"].as_str(), shown["warp"].as_str(), shown["source_tempo"].as_f64()),
        (Some("audio"), Some("beats"), Some(100.0))
    );
    assert!(project.get("problems").is_none(), "{project}");

    // Render and read the numbers back.
    let wav = fixture.folder.path().join("beat.wav");
    let report = ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 1 })).await;
    assert!(report["problems"].as_array().unwrap().is_empty(), "{report}");
    assert!((report["duration_seconds"].as_f64().unwrap() - 4.0).abs() < 0.3, "8 beats at 120 BPM: {report}");
    let peak = report["peak_db"].as_f64().unwrap();
    assert!((-12.0..=6.0).contains(&peak), "an implausible peak of {peak} dB");
    let levels: Vec<(String, f64)> = report["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| (track["name"].as_str().unwrap().to_string(), track["peak_db"].as_f64().unwrap()))
        .collect();
    assert!(levels.iter().all(|(_, level)| (-20.0..=3.0).contains(level)), "every track should be heard: {levels:?}");

    // Soloed, the warped loop's clicks are on the half beats of the song.
    ok(client, "update_track", json!({ "track": loop_track, "solo": true })).await;
    ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 1 })).await;
    let audio = decode(&wav).unwrap();
    let clicks: Vec<f64> = audio
        .left
        .iter()
        .enumerate()
        .filter(|(n, sample)| sample.abs() > 0.5 && (*n == 0 || audio.left[n - 1].abs() <= 0.5))
        .map(|(n, _)| n as f64 / SR)
        .filter({
            let mut last = -1.0;
            move |time| std::mem::replace(&mut last, *time) + 0.02 < *time
        })
        .collect();
    assert_eq!(clicks.len(), 8, "{clicks:?}");
    for (index, time) in clicks.iter().enumerate() {
        let expected = (index as f64 + 0.5) * 0.5;
        assert!((time - expected).abs() < 0.001, "click {index} at {time:.4} s, wanted {expected}");
    }
    ok(client, "update_track", json!({ "track": loop_track, "solo": false })).await;

    // Changing the tempo keeps the loop on the beat, and the clip its length.
    ok(client, "set_project", json!({ "tempo": 90 })).await;
    let slower = ok(client, "update_clip", json!({ "clip": loop_clip, "name": "Break" })).await;
    assert_eq!((slower["length"].as_f64(), slower["audio_beats"].as_f64()), (Some(8.0), Some(8.0)));

    // Every sound edit is a step of the history.
    ok(client, "history", json!({ "action": "undo", "steps": 2 })).await;
    assert_eq!(ok(client, "get_project", json!({})).await["tempo"], 120.0);
    let pads = ok(client, "set_sample", json!({ "device": rack, "pad": "closed_hat", "path": null })).await;
    assert_eq!(pads["samples"].as_array().unwrap().len(), 2, "null empties the pad");
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(
        ok(client, "get_device", json!({ "device": rack, "query": "volume" })).await["samples"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // A sampler takes one sound; a file that goes missing is reported
    // against its slot, here and in the project overview.
    let keys = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:sampler" })).await;
    let sampler = keys["instrument_device_id"].as_u64().unwrap();
    let one = ok(client, "set_sample", json!({ "device": sampler, "path": pack.bass_loop })).await;
    assert_eq!(one["samples"], json!([{ "slot": "sample", "path": pack.bass_loop }]));
    let file = fixture.folder.path().join("song.trill");
    ok(client, "save_project", json!({ "path": file.display().to_string() })).await;
    std::fs::remove_file(&pack.snare).unwrap();
    let reloaded = ok(client, "load_project", json!({ "path": file.display().to_string() })).await;
    // The overview says once that a file is missing, on which track and
    // what to do, in place of a line for every pad and clip that names it.
    let missing = &reloaded["missing_files"];
    assert_eq!((&missing["files"], &missing["places"], &missing["tracks"]), (&json!(1), &json!(1), &json!(["Drums"])));
    assert!(missing["note"].as_str().unwrap().contains("relink_files"), "{missing}");
    assert!(reloaded["problems"].is_null(), "{}", reloaded["problems"]);
    let listed = ok(client, "missing_files", json!({})).await;
    assert_eq!(listed["files"][0]["used_by"][0]["slot"], "pad 38", "{listed}");
    let broken = ok(client, "get_device", json!({ "device": rack, "query": "volume" })).await;
    assert!(broken["samples"][1]["problem"].as_str().unwrap().contains("Snare Tight.wav"), "{broken}");
    assert!(broken["samples"][0].get("problem").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn sound_tools_say_what_is_wrong_and_change_nothing() {
    let fixture = connect().await;
    let client = &fixture.client;
    let pack = pack(fixture.folder.path());
    ok(client, "library", json!({ "action": "add_folder", "path": pack.root.display().to_string() })).await;
    let synth = ok(client, "create_track", json!({ "name": "Synth", "instrument": "builtin:polysynth" })).await;
    let (track, synth) = (synth["track_id"].as_u64().unwrap(), synth["instrument_device_id"].as_u64().unwrap());
    let midi = ok(client, "create_clip", json!({ "track": track, "start": 0, "length": 4 })).await["clip_id"]
        .as_u64()
        .unwrap();
    let rack = ok(client, "add_device", json!({ "track": track, "uri": "builtin:drumrack" })).await["device_id"]
        .as_u64()
        .unwrap();
    let sampler = ok(client, "add_device", json!({ "track": track, "uri": "builtin:sampler" })).await["device_id"]
        .as_u64()
        .unwrap();
    let audio =
        ok(client, "create_audio_clip", json!({ "track": track, "path": pack.kick, "start": 8 })).await["clip_id"]
            .as_u64()
            .unwrap();
    let before = fixture.session.lock().project().clone();
    let roots = fixture.runtime.library.roots();
    let nowhere = fixture.folder.path().join("nowhere").display().to_string();
    let missing = pack.root.join("Drums").join("Gone.wav").display().to_string();

    let mistakes: Vec<(&'static str, Value, &str)> = vec![
        ("search_sounds", json!({ "tags": ["drumz"] }), "is not a tag. Tags: kick, snare"),
        ("search_sounds", json!({ "key": "H sharp" }), "is not a key"),
        ("search_sounds", json!({ "kind": "video" }), "\"audio\", \"midi\""),
        ("search_sounds", json!({ "folder": nowhere }), "is not a folder of the library. The library's folders:"),
        ("library", json!({ "action": "add_folder" }), "add_folder needs `path`"),
        ("library", json!({ "action": "add_folder", "path": nowhere }), "is not a folder on this machine"),
        ("library", json!({ "action": "remove_folder", "path": nowhere }), "is not one of the library's folders"),
        ("preview_sound", json!({ "path": missing }), "does not exist"),
        ("preview_sound", json!({ "path": pack.kick, "gain_db": 40 }), "above the 6 dB maximum"),
        ("set_sample", json!({ "device": synth, "path": pack.kick }), "plays no samples"),
        ("set_sample", json!({ "device": rack, "path": pack.kick }), "say which pad"),
        ("set_sample", json!({ "device": rack, "pad": "cowbell", "path": pack.kick }), "no pad for note 56"),
        ("set_sample", json!({ "device": rack, "pad": "kick", "path": missing }), "does not exist"),
        ("set_sample", json!({ "device": rack, "pad": "kick" }), "needs `path`"),
        ("set_sample", json!({ "device": rack, "pad": "boom", "path": pack.kick }), "is not a note name"),
        ("set_sample", json!({ "device": sampler, "pad": "kick", "path": pack.kick }), "leave out `pad`"),
        ("set_sample", json!({ "device": 999, "path": pack.kick }), "device 999 not found. Existing devices:"),
        ("create_audio_clip", json!({ "track": track, "path": missing, "start": 0 }), "does not exist"),
        (
            "create_audio_clip",
            json!({ "track": track, "path": pack.kick, "start": 0, "warp": "beats" }),
            "source tempo",
        ),
        (
            "create_audio_clip",
            json!({ "track": 999, "path": pack.kick, "start": 0 }),
            "track 999 not found. Existing tracks:",
        ),
        (
            "create_audio_clip",
            json!({ "track": track, "path": pack.kick, "start": 0, "offset_seconds": 5 }),
            "past the end",
        ),
        (
            "create_audio_clip",
            json!({ "track": track, "path": pack.kick, "start": 0, "transpose": 40 }),
            "transpose 40 is outside",
        ),
        ("update_clip", json!({ "clip": midi, "warp": "beats" }), "only audio clips can be warped"),
        ("update_clip", json!({ "clip": audio, "warp": "tonal" }), "give `source_tempo`"),
        ("update_clip", json!({ "clip": audio, "source_tempo": 5 }), "source tempo 5 is outside"),
    ];
    for (tool, arguments, expected) in mistakes {
        let error = call(client, tool, arguments.clone()).await.expect_err(expected);
        assert!(error.contains(expected), "{tool} {arguments}: '{error}' should mention '{expected}'");
    }
    assert_eq!(fixture.session.lock().project(), &before, "failed calls must not change the project");
    assert_eq!(fixture.runtime.library.roots(), roots, "nor the library");
    assert!(!fixture.session.lock().is_auditioning());

    // Settings that are accepted but have no effect are pointed out.
    let odd = ok(client, "update_clip", json!({ "clip": audio, "transpose": 5 })).await;
    assert!(odd["notes"][0].as_str().unwrap().contains("transpose has no effect"), "{odd}");
    // With the tempo given, the kick can be warped after all.
    let warped = ok(client, "update_clip", json!({ "clip": audio, "warp": "tonal", "source_tempo": 120 })).await;
    assert_eq!((warped["warp"].as_str(), warped["transpose"].as_f64()), (Some("tonal"), Some(5.0)));

    // Removing the folder empties the library and the settings.
    let removed =
        ok(client, "library", json!({ "action": "remove_folder", "path": pack.root.display().to_string() })).await;
    assert_eq!((removed["folders"].as_array().unwrap().len(), removed["sounds"].as_u64()), (0, Some(0)));
    assert!(trill_library::Settings::load(&fixture.settings_file()).settings.library.roots.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn time_is_cut_repeated_and_opened_up_across_the_song() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    let track = track["track_id"].as_u64().unwrap();
    let note = |start: f64| json!({ "pitch": 60, "start": start, "length": 0.5 });
    for (name, start) in [("A", 0.0), ("B", 16.0), ("C", 32.0)] {
        let notes: Vec<Value> = (0..16).map(|beat| note(beat as f64)).collect();
        ok(
            client,
            "create_clip",
            json!({ "track": track, "name": name, "start": start, "length": 16, "notes": notes }),
        )
        .await;
    }
    ok(
        client,
        "set_markers",
        json!({ "markers": [{ "beat": 0, "name": "A" }, { "beat": 16, "name": "B" }, { "beat": 32, "name": "C" }] }),
    )
    .await;
    let spans = |project: &Value| -> Vec<(f64, f64)> {
        let clips = project["tracks"][0]["clips"].as_array().unwrap();
        clips.iter().map(|clip| (clip["start"].as_f64().unwrap(), clip["length"].as_f64().unwrap())).collect()
    };

    // Section B goes: C closes up behind A.
    let cut = ok(client, "edit_time", json!({ "action": "delete", "start": 16, "end": 32 })).await;
    assert_eq!((cut["was_beats"].as_f64(), cut["length_beats"].as_f64()), (Some(48.0), Some(32.0)));
    assert_eq!(cut["markers"], json!([{ "beat": 0.0, "name": "A" }, { "beat": 16.0, "name": "C" }]));
    assert_eq!(spans(&ok(client, "get_project", json!({})).await), [(0.0, 16.0), (16.0, 16.0)]);

    // The second half of A plays twice, and four beats open up before C.
    ok(client, "edit_time", json!({ "action": "duplicate", "start": 8, "end": 16 })).await;
    let opened = ok(client, "edit_time", json!({ "action": "insert", "start": 24, "length": 4 })).await;
    assert_eq!(opened["length_beats"].as_f64(), Some(44.0));
    assert_eq!(
        spans(&ok(client, "get_project", json!({})).await),
        [(0.0, 16.0), (16.0, 8.0), (28.0, 16.0)],
        "A, the repeat of its second half, then C after the new four beats"
    );

    // One undo step each, like every other edit.
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(spans(&ok(client, "get_project", json!({})).await), [(0.0, 16.0), (16.0, 8.0), (24.0, 16.0)]);

    // What cannot be done says what is missing, and changes nothing.
    let before = ok(client, "get_project", json!({})).await;
    for (arguments, expected) in [
        (json!({ "action": "delete", "start": 8 }), "needs `end`"),
        (json!({ "action": "insert", "start": 8 }), "needs `length`"),
        (json!({ "action": "delete", "start": 8, "end": 8 }), "0 <= start < end"),
        (json!({ "action": "shorten", "start": 8, "end": 12 }), "unknown action"),
    ] {
        let error = call(client, "edit_time", arguments.clone()).await.unwrap_err();
        assert!(error.contains(expected), "{arguments}: {error}");
    }
    assert_eq!(spans(&ok(client, "get_project", json!({})).await), spans(&before));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stretch_of_time_is_copied_to_another_place_on_every_track_or_some() {
    let fixture = connect().await;
    let client = &fixture.client;
    let make = |name: &'static str| async move {
        ok(client, "create_track", json!({ "name": name, "instrument": "builtin:polysynth" })).await["track_id"]
            .as_u64()
            .unwrap()
    };
    let (keys, bass) = (make("Keys").await, make("Bass").await);
    // A verse (0..16) and a chorus (16..32) on both tracks; the chorus swells on the keys.
    for (track, pitch) in [(keys, 60), (bass, 36)] {
        for (name, start) in [("Verse", 0.0), ("Chorus", 16.0)] {
            let notes: Vec<Value> = (0..16).map(|beat| json!([pitch + start as u64 / 16 * 7, beat, 0.5])).collect();
            let clip = json!({ "track": track, "name": name, "start": start, "length": 16, "notes": notes });
            ok(client, "create_clip", clip).await;
        }
    }
    let project = ok(client, "get_project", json!({})).await;
    let chorus = project["tracks"][0]["clips"][1]["id"].clone();
    ok(client, "set_controllers", json!({ "clip": chorus, "controllers": { "expression": [[0, 40], [16, 120]] } }))
        .await;
    let volume = json!({ "type": "track_volume", "track": keys });
    ok(client, "set_automation", json!({ "target": volume, "points": [[0, -12], [16, -12], [32, 0], [64, 0]] })).await;
    ok(client, "set_markers", json!({ "markers": [{ "beat": 0, "name": "Verse" }, { "beat": 16, "name": "Chorus" }] }))
        .await;
    let spans = |project: &Value, track: usize| -> Vec<(String, f64, f64)> {
        let clips = project["tracks"][track]["clips"].as_array().unwrap();
        let span = |clip: &Value| {
            (
                clip["name"].as_str().unwrap().to_string(),
                clip["start"].as_f64().unwrap(),
                clip["length"].as_f64().unwrap(),
            )
        };
        clips.iter().map(span).collect()
    };
    let named = |name: &str, start: f64, length: f64| (name.to_string(), start, length);

    // The chorus again after itself, on every track, in one call: it makes room, so the song grows.
    let copied = ok(client, "edit_time", json!({ "action": "copy", "start": 16, "end": 32, "to": 32 })).await;
    assert_eq!((copied["was_beats"].as_f64(), copied["length_beats"].as_f64()), (Some(32.0), Some(48.0)));
    assert_eq!(copied["copied"]["from"], json!([16.0, 32.0]));
    assert_eq!(
        (copied["copied"]["to"].clone(), copied["copied"]["made_room"].clone()),
        (json!([32.0, 48.0]), json!(true))
    );
    assert_eq!(copied["copied"]["clips"].as_object().unwrap().len(), 2, "one clip on each track: {copied}");
    let project = ok(client, "get_project", json!({})).await;
    for track in 0..2 {
        let expected = [named("Verse", 0.0, 16.0), named("Chorus", 16.0, 16.0), named("Chorus", 32.0, 16.0)];
        assert_eq!(spans(&project, track), expected);
    }
    // The copy carries the chorus's own curve, and the track's automation through it.
    let again = project["tracks"][0]["clips"][2]["id"].clone();
    let curves = ok(client, "get_controllers", json!({ "clip": again })).await;
    assert_eq!(curves["controllers"][0]["controller"], "expression");
    assert_eq!(curves["controllers"][0]["points"], json!([[0.0, 40.0], [16.0, 120.0]]));
    let lanes = ok(client, "get_automation", json!({})).await;
    let points = lanes["automation"][0]["points"].as_array().unwrap();
    // Two points on one beat are a step: the later one is where the envelope goes on from.
    let at = |beat: f64| {
        points.iter().rev().find(|point| point[0].as_f64() == Some(beat)).map(|point| point[1].as_f64().unwrap())
    };
    assert_eq!((at(32.0), at(48.0)), (Some(-12.0), Some(0.0)), "the rise is heard again: {points:?}");

    // Over what is there, on one track only: the bass plays its chorus under the verse; nothing moves.
    let over = ok(
        client,
        "edit_time",
        json!({ "action": "copy", "start": 16, "end": 24, "to": 4, "tracks": [bass], "make_room": false }),
    )
    .await;
    assert_eq!((over["length_beats"].as_f64(), over["copied"]["made_room"].clone()), (Some(48.0), json!(false)));
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(spans(&project, 0).len(), 3, "the keys are as they were");
    assert_eq!(
        spans(&project, 1)[..3],
        [named("Verse", 0.0, 4.0), named("Chorus", 4.0, 8.0), named("Verse", 12.0, 4.0)]
    );
    assert_eq!(project["markers"].as_array().unwrap().len(), 2, "markers are not copied");

    // One undo step each; and what cannot be done says what is missing and changes nothing.
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(spans(&ok(client, "get_project", json!({})).await, 1).len(), 3);
    let before = ok(client, "get_project", json!({})).await;
    for (arguments, expected) in [
        (json!({ "action": "copy", "start": 16, "end": 32 }), "needs `to`"),
        (json!({ "action": "copy", "start": 16, "to": 32 }), "needs `end`"),
        (json!({ "action": "copy", "start": 16, "end": 32, "to": -4 }), "must be >= 0"),
        (json!({ "action": "copy", "start": 16, "end": 32, "to": 0, "tracks": [999] }), "track 999 not found"),
        (json!({ "action": "delete", "start": 16, "end": 32, "tracks": [1] }), "go with action \"copy\""),
    ] {
        let error = call(client, "edit_time", arguments.clone()).await.unwrap_err();
        assert!(error.contains(expected), "{arguments}: {error}");
    }
    assert_eq!(ok(client, "get_project", json!({})).await["tracks"], before["tracks"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_without_a_window_says_how_to_edit_it_instead() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    let device = track["instrument_device_id"].as_u64().unwrap();
    assert!(ok(client, "get_device", json!({ "device": device })).await.get("editor").is_none());
    let error = call(client, "show_device_editor", json!({ "device": device })).await.unwrap_err();
    assert!(error.contains("no window of its own") && error.contains("update_device"), "{error}");
    let error = call(client, "show_device_editor", json!({ "device": 9999 })).await.unwrap_err();
    assert!(error.contains("not found"), "{error}");
}

/// The status line of the server's answer to a hand-written request: one
/// that opens an MCP session, sent with the given Host and, perhaps, Origin.
fn answer_to(server: &HttpServer, host: &str, origin: Option<&str>) -> String {
    use std::io::{Read, Write};
    let address = server.url().trim_start_matches("http://").split('/').next().unwrap().to_owned();
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } }
    })
    .to_string();
    let origin = origin.map(|origin| format!("Origin: {origin}\r\n")).unwrap_or_default();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\n{origin}Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = std::net::TcpStream::connect(&address).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut head = [0u8; 64];
    let read = stream.read(&mut head).unwrap();
    String::from_utf8_lossy(&head[..read]).lines().next().unwrap_or_default().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn web_pages_are_turned_away_and_local_programs_are_not() {
    let fixture = connect().await;
    let server = &fixture._server;
    let address = server.url().trim_start_matches("http://").split('/').next().unwrap().to_owned();
    let port = address.rsplit(':').next().unwrap().to_owned();

    // A program on this machine sends no Origin.
    assert!(answer_to(server, &address, None).contains("200"), "{}", answer_to(server, &address, None));
    assert!(answer_to(server, &format!("localhost:{port}"), None).contains("200"));
    // A page in a browser sends its own, whatever it is.
    for origin in ["https://example.com", "http://localhost:3000", "null", "http://127.0.0.1"] {
        let answer = answer_to(server, &address, Some(origin));
        assert!(answer.contains("403"), "a page from {origin} got: {answer}");
    }
    // And one that reaches this address under a name of its own (DNS
    // rebinding) is told by its Host.
    let answer = answer_to(server, &format!("trill.example.com:{port}"), None);
    assert!(answer.contains("403"), "a foreign Host got: {answer}");
    // Being turned away changes nothing for the clients that belong.
    assert!(ok(&fixture.client, "get_project", json!({})).await["tracks"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_lost_to_a_crash_are_listed_brought_back_and_discarded() {
    // Two programs died here with unsaved work before this one started.
    let folder = tempfile::tempdir().unwrap();
    for name in ["Night Shift", "Scrap"] {
        let lost = Runtime::start(options_in(folder.path())).unwrap();
        lost.session
            .lock()
            .edit(|project| {
                project.name = name.into();
                project.add_track("Bass");
                Ok(())
            })
            .unwrap();
        lost.refresh_safety_copy();
        lost.abandon();
    }
    let fixture = connect_in(folder).await;
    let client = &fixture.client;

    let project = ok(client, "get_project", json!({})).await;
    assert!(project["recoverable"].as_str().unwrap().contains("recover_project"), "{project}");
    let list = ok(client, "recover_project", json!({})).await;
    let index_of = |list: &Value, name: &str| -> u64 {
        let unsaved = list["unsaved"].as_array().unwrap();
        unsaved.iter().find(|entry| entry["name"] == name).unwrap_or_else(|| panic!("{name} in {list}"))["index"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(list["unsaved"].as_array().unwrap().len(), 2);
    assert_eq!(list["unsaved"][0]["minutes_ago"], 0);

    // Asking for the impossible says what is possible.
    let error = call(client, "recover_project", json!({ "action": "open" })).await.unwrap_err();
    assert!(error.contains("`index`"), "{error}");
    let error = call(client, "recover_project", json!({ "action": "open", "index": 7 })).await.unwrap_err();
    assert!(error.contains("no project 7") && error.contains("index 0 to 1"), "{error}");
    let error = call(client, "recover_project", json!({ "action": "restore", "index": 0 })).await.unwrap_err();
    assert!(error.contains("unknown action"), "{error}");
    assert_eq!(ok(client, "recover_project", json!({})).await["unsaved"].as_array().unwrap().len(), 2);

    let gone = ok(client, "recover_project", json!({ "action": "discard", "index": index_of(&list, "Scrap") })).await;
    assert_eq!(gone["discarded"], "Scrap");
    assert_eq!(gone["unsaved"].as_array().unwrap().len(), 1);

    let back = ok(client, "recover_project", json!({ "action": "open", "index": 0 })).await;
    assert_eq!((&back["recovered"], &back["name"]), (&json!("Night Shift"), &json!("Night Shift")));
    assert_eq!(back["tracks"][0]["name"], "Bass");
    assert_eq!(back["unsaved_changes"], true);
    assert!(ok(client, "get_project", json!({})).await.get("recoverable").is_none());
    assert_eq!(ok(client, "recover_project", json!({})).await["unsaved"], json!([]));
    let error = call(client, "recover_project", json!({ "action": "open", "index": 0 })).await.unwrap_err();
    assert!(error.contains("nothing to recover"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn automation_is_switched_off_and_on_without_losing_it() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    let target = json!({ "type": "track_volume", "track": track["track_id"] });
    let lane = |automation: &Value| automation["automation"][0].clone();

    let written = ok(client, "set_automation", json!({ "target": target, "points": [[0, -12], [8, 0]] })).await;
    assert_eq!((written["point_count"].as_u64(), written["enabled"].as_bool()), (Some(2), Some(true)));
    assert!(lane(&ok(client, "get_automation", json!({})).await).get("enabled").is_none(), "on is not worth saying");

    let off = ok(client, "set_automation", json!({ "target": target, "enabled": false })).await;
    assert_eq!((off["point_count"].as_u64(), off["enabled"].as_bool()), (Some(2), Some(false)));
    let shown = lane(&ok(client, "get_automation", json!({})).await);
    assert_eq!((shown["enabled"].as_bool(), shown["points"].as_array().map(Vec::len)), (Some(false), Some(2)));
    assert_eq!(ok(client, "get_project", json!({})).await["automation"][0]["enabled"], false);

    // Writing an envelope means wanting to hear it, unless told otherwise.
    let rewritten = ok(client, "set_automation", json!({ "target": target, "points": [[0, -6]] })).await;
    assert_eq!(rewritten["enabled"], true);
    let quiet = ok(client, "set_automation", json!({ "target": target, "points": [[0, -6]], "enabled": false })).await;
    assert_eq!(quiet["enabled"], false);
    assert_eq!(ok(client, "set_automation", json!({ "target": target, "enabled": true })).await["enabled"], true);

    // Nothing to do, and nothing to switch, say so.
    let error = call(client, "set_automation", json!({ "target": target })).await.unwrap_err();
    assert!(error.contains("`points`") && error.contains("`enabled`"), "{error}");
    let pan = json!({ "type": "track_pan", "track": track["track_id"] });
    let error = call(client, "set_automation", json!({ "target": pan, "enabled": false })).await.unwrap_err();
    assert!(error.contains("no automation for that target"), "{error}");
    assert_eq!(ok(client, "set_automation", json!({ "target": target, "points": [] })).await["cleared"], true);
}

/// A track that plays a steady tone, struck again on every beat for
/// `beats` beats, and a return for it to send to.
async fn lead_and_return(client: &Client, beats: usize) -> (u64, u64) {
    let lead = ok(client, "create_track", json!({ "name": "Lead", "instrument": "builtin:polysynth" })).await;
    let synth = lead["instrument_device_id"].clone();
    let lead = lead["track_id"].as_u64().unwrap();
    // A sine at one level from a millisecond after each note begins.
    let steady = json!({ "osc1_wave": 0, "attack": 0.001, "sustain": 1, "release": 0.01 });
    ok(client, "update_device", json!({ "device": synth, "params": steady })).await;
    let echo =
        ok(client, "create_track", json!({ "name": "Echo", "kind": "return" })).await["track_id"].as_u64().unwrap();
    let notes: Vec<Value> = (0..beats).map(|beat| json!([69, beat, 1])).collect();
    ok(client, "create_clip", json!({ "track": lead, "start": 0, "length": beats, "notes": notes })).await;
    (lead, echo)
}

/// The peak of `track` in dB over a stretch of the song that starts on a
/// beat (a measurement plays the notes that begin in it); `None` where the
/// track is silent.
async fn peak_of(client: &Client, track: u64, start: f64, end: f64) -> Option<f64> {
    let asked = json!({ "start": start, "end": end, "sections": [], "max_tail_seconds": 0 });
    let report = ok(client, "measure", asked).await;
    let tracks = report["tracks"].as_array().unwrap();
    let entry = tracks.iter().find(|entry| entry["track"] == track)?;
    entry["peak_db"].as_f64().filter(|db| *db > -100.0)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_is_opened_for_one_note_by_its_envelope() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (lead, echo) = lead_and_return(client, 16).await;
    assert_eq!(peak_of(client, echo, 0.0, 16.0).await, None, "nothing is sent yet");

    // Shut, opened over two beats, open for two, shut again. No send is
    // set by hand: the envelope is the send.
    let throw = json!({ "type": "send", "track": lead, "to": echo });
    let points = json!([[0, -70], [8, -70], [10, -6], [12, -6], [13, -70]]);
    let written = ok(client, "set_automation", json!({ "target": throw, "points": points })).await;
    assert_eq!(
        (&written["point_count"], &written["enabled"], &written["travel"]),
        (&json!(5), &json!(true), &json!("fader"))
    );

    // The song says so where a model reads about the track and its mix.
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["tracks"][0]["sends"], json!([{ "to": echo, "automated": true }]), "{project}");
    assert_eq!(project["automation"][0]["target"], throw);
    let lanes = ok(client, "get_automation", json!({ "target": throw })).await;
    assert_eq!(lanes["automation"][0]["travel"], "fader");
    assert_eq!(lanes["automation"][0]["points"][2], json!([10.0, -6.0]));

    // And it is heard: nothing before the throw, the track 6 dB down
    // while it is open, nothing after.
    let level = peak_of(client, lead, 10.0, 11.0).await.expect("the lead sounds");
    assert_eq!(peak_of(client, echo, 0.0, 7.9).await, None, "shut is silent");
    let open = peak_of(client, echo, 10.0, 11.0).await.expect("the return is fed while the send is open");
    assert!((open - (level - 6.0)).abs() < 0.3, "{open} for {level}");
    assert_eq!(peak_of(client, echo, 14.0, 16.0).await, None, "and shut again");
    // On the way up it is where a fader would be: about 20 dB down by six
    // tenths of the way, where a line straight in decibels would be 32 down.
    let rising = peak_of(client, echo, 9.0, 9.2).await.expect("it is opening");
    assert!((level - 22.5..level - 19.0).contains(&rising), "{rising} for {level}");

    // A level set by hand lives beside it, and counts once the envelope is off.
    let by_hand = ok(client, "update_track", json!({ "track": lead, "send": { "to": echo, "level_db": -12 } })).await;
    assert_eq!(by_hand["sends"], json!([{ "to": echo, "level_db": -12.0, "automated": true }]));
    ok(client, "set_automation", json!({ "target": throw, "enabled": false })).await;
    let held = ok(client, "get_project", json!({})).await;
    assert_eq!(held["tracks"][0]["sends"], json!([{ "to": echo, "level_db": -12.0, "automation_off": true }]));
    let steady = peak_of(client, echo, 2.0, 3.0).await.expect("the send set by hand is heard");
    assert!((steady - (level - 12.0)).abs() < 0.3, "{steady} for {level}");

    // What cannot be a send says which tracks could be.
    let error =
        call(client, "set_automation", json!({ "target": { "type": "send", "track": lead }, "points": points }))
            .await
            .unwrap_err();
    assert!(error.contains("`to`"), "{error}");
    let wrong = json!({ "type": "send", "track": lead, "to": lead });
    let error = call(client, "set_automation", json!({ "target": wrong, "points": points })).await.unwrap_err();
    assert!(error.contains("not a return track") && error.contains(&format!("{echo} (Echo)")), "{error}");
    let back = json!({ "type": "send", "track": echo, "to": echo });
    let error = call(client, "set_automation", json!({ "target": back, "points": points })).await.unwrap_err();
    assert!(error.contains("returns cannot send to returns"), "{error}");
    let missing = json!({ "type": "send", "track": 999, "to": echo });
    let error = call(client, "set_automation", json!({ "target": missing, "points": points })).await.unwrap_err();
    assert!(error.contains("Existing tracks"), "{error}");
    let loud = json!({ "target": throw, "points": [[0, 12]] });
    let error = call(client, "set_automation", loud).await.unwrap_err();
    assert!(error.contains("at most 6"), "{error}");

    // The envelope goes with the return it feeds, and undo brings both back.
    ok(client, "delete_track", json!({ "track": echo })).await;
    assert_eq!(ok(client, "get_automation", json!({})).await["automation"], json!([]));
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(ok(client, "get_automation", json!({})).await["automation"][0]["point_count"], 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_line_follows_the_travel_of_its_control_and_an_old_file_plays_as_it_did() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (lead, _) = lead_and_return(client, 16).await;
    let filter = ok(client, "add_device", json!({ "track": lead, "uri": "builtin:filter" })).await["device_id"].clone();
    let utility =
        ok(client, "add_device", json!({ "track": lead, "uri": "builtin:utility" })).await["device_id"].clone();
    let eq = ok(client, "add_device", json!({ "track": lead, "uri": "builtin:eq" })).await["device_id"].clone();
    let travel_of = |reply: Value| reply["travel"].as_str().unwrap().to_string();
    let write = |target: Value, points: Value| async move {
        travel_of(ok(client, "set_automation", json!({ "target": target, "points": points })).await)
    };

    // Each kind of control says how its lines go.
    let volume = json!({ "type": "track_volume", "track": lead });
    let fade = json!([[0, 0], [16, -60]]);
    assert_eq!(write(volume.clone(), fade.clone()).await, "fader");
    assert_eq!(write(json!({ "type": "master_volume" }), json!([[0, 0], [16, 0]])).await, "fader");
    assert_eq!(write(json!({ "type": "track_pan", "track": lead }), json!([[0, 0], [16, 0]])).await, "linear");
    let cutoff = json!({ "type": "device_param", "device": filter, "param": "cutoff" });
    assert_eq!(write(cutoff.clone(), json!([[0, 20000], [16, 20000]])).await, "log");
    let gain = json!({ "type": "device_param", "device": utility, "param": "gain" });
    assert_eq!(write(gain, json!([[0, 0], [16, 0]])).await, "gain");
    let boost = json!({ "type": "device_param", "device": eq, "param": "mid1_gain" });
    assert_eq!(write(boost, json!([[0, 0], [16, 0]])).await, "linear");

    // A fade along the fader is at -16.3 dB half way, as Live's is...
    let full = peak_of(client, lead, 0.0, 0.2).await.expect("the lead sounds");
    let half_way = peak_of(client, lead, 8.0, 8.1).await.expect("the fade is still heard half way");
    assert!((half_way - (full - 16.3)).abs() < 0.6, "{half_way} for {full}");
    // ...and straight in decibels, when that is asked for, at -30 dB.
    let straight = ok(client, "set_automation", json!({ "target": volume, "travel": "linear" })).await;
    assert_eq!((straight["travel"].as_str(), straight["point_count"].as_u64()), (Some("linear"), Some(2)));
    let in_db = peak_of(client, lead, 8.0, 8.1).await.expect("heard");
    assert!((in_db - (full - 30.0)).abs() < 0.6, "{in_db} for {full}");
    let lanes = ok(client, "get_automation", json!({ "target": volume })).await;
    assert_eq!(
        (&lanes["automation"][0]["travel"], &lanes["automation"][0]["points"]),
        (&json!("linear"), &json!([[0.0, 0.0], [16.0, -60.0]]))
    );

    // A file as an earlier version saved it says nothing of travel: it
    // opens, and its fade is the one it had.
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("before.trill");
    ok(client, "save_project", json!({ "path": path.display().to_string() })).await;
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["format_version"], trill_core::project::FORMAT_VERSION, "this version's file says which it is");
    let said: Vec<bool> =
        saved["automation"].as_array().unwrap().iter().map(|lane| lane.get("travel").is_some()).collect();
    assert_eq!(said, [false, true, false, true, true, false], "a lane straight in its units says nothing");
    saved["format_version"] = json!(1);
    for lane in saved["automation"].as_array_mut().unwrap() {
        lane.as_object_mut().unwrap().remove("travel");
    }
    std::fs::write(&path, serde_json::to_string_pretty(&saved).unwrap()).unwrap();
    ok(client, "load_project", json!({ "path": path.display().to_string() })).await;
    let lanes = ok(client, "get_automation", json!({})).await;
    let travels: Vec<&str> =
        lanes["automation"].as_array().unwrap().iter().map(|lane| lane["travel"].as_str().unwrap()).collect();
    assert_eq!(travels, ["linear"; 6], "every lane of an old file is straight in its units");
    let then = peak_of(client, lead, 8.0, 8.1).await.expect("heard");
    assert!((then - in_db).abs() < 0.01, "{then} against {in_db}");
    // Written again, an envelope is written as it is described now.
    assert_eq!(write(volume.clone(), fade).await, "fader");
    // And one whose travel is set back to its control's follows it again.
    let again = ok(client, "set_automation", json!({ "target": cutoff, "travel": "control" })).await;
    assert_eq!(again["travel"], "log");

    // A file as formats 3 to 5 saved it calls the law of its day a fader,
    // with the level it was measured from. It opens, its lane says which
    // law that is, and its fade is the one it had: -20 dB half way.
    ok(client, "save_project", json!({ "path": path.display().to_string() })).await;
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    saved["format_version"] = json!(5);
    let mut faders = 0;
    for lane in saved["automation"].as_array_mut().unwrap() {
        if lane["travel"].get("fader").is_some() {
            lane["travel"] = json!({ "fader": { "floor": -70.0 } });
            faders += 1;
        }
    }
    assert_eq!(faders, 1, "the track's fader, the one lane written since the older file was read: {saved}");
    std::fs::write(&path, serde_json::to_string_pretty(&saved).unwrap()).unwrap();
    ok(client, "load_project", json!({ "path": path.display().to_string() })).await;
    let lanes = ok(client, "get_automation", json!({ "target": volume })).await;
    assert_eq!(lanes["automation"][0]["travel"], "square", "{lanes}");
    let as_it_was = peak_of(client, lead, 8.0, 8.1).await.expect("heard");
    assert!((as_it_was - (full - 20.0)).abs() < 0.6, "{as_it_was} for {full}");
    // Set to follow its control, it falls as a fader does now.
    let follows = ok(client, "set_automation", json!({ "target": volume, "travel": "control" })).await;
    assert_eq!(follows["travel"], "fader");
    let now = peak_of(client, lead, 8.0, 8.1).await.expect("heard");
    assert!((now - (full - 16.3)).abs() < 0.6, "{now} for {full}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_envelope_of_thousands_of_points_is_written_at_once_and_read_in_parts() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (lead, echo) = lead_and_return(client, 64).await;
    let pan = json!({ "type": "track_pan", "track": lead });
    ok(client, "set_automation", json!({ "target": pan, "points": [[0, -1], [64, 1]] })).await;
    // As a compressor drawn on paper leaves a lane: 7,332 points.
    let count = 7_332usize;
    let points: Vec<Value> = (0..count)
        .map(|index| {
            let beat = index as f64 * 64.0 / count as f64;
            json!([beat, -12.0 * (-(beat.fract()) * 6.0).exp()])
        })
        .collect();
    let duck = json!({ "type": "send", "track": lead, "to": echo });
    let written = ok(client, "set_automation", json!({ "target": duck, "points": points })).await;
    assert_eq!(written["point_count"], count);

    // The song's overview counts them and lists none.
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["automation"][1]["point_count"], count);
    assert!(project["automation"][1].get("points").is_none());

    // One reply lists a thousand, says how many there are and how to go on.
    let first = ok(client, "get_automation", json!({})).await;
    assert!(first.to_string().len() < 40_000, "a reply of {} characters", first.to_string().len());
    let lanes = first["automation"].as_array().unwrap();
    assert_eq!(lanes[0]["points"].as_array().unwrap().len(), 2);
    assert_eq!(lanes[1]["points"].as_array().unwrap().len(), 998);
    assert_eq!((&lanes[1]["next_offset"], &lanes[1]["point_count"]), (&json!(998), &json!(count)));
    assert_eq!((&first["points_listed"], &first["points_in_all"]), (&json!(1000), &json!(count + 2)));
    assert!(first["note"].as_str().unwrap().contains("next_offset"), "{}", first["note"]);

    // Reading on, part by part, gives every point once and in order.
    let mut read: Vec<Value> = lanes[1]["points"].as_array().unwrap().clone();
    let mut offset = 998;
    loop {
        let part = ok(client, "get_automation", json!({ "target": duck, "offset": offset, "limit": 5000 })).await;
        let lane = &part["automation"][0];
        read.extend(lane["points"].as_array().unwrap().iter().cloned());
        match lane["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(read.len(), count);
    assert!(read.windows(2).all(|pair| pair[0][0].as_f64() < pair[1][0].as_f64()));
    assert_eq!(read[count - 1][0].as_f64(), Some(((count - 1) as f64 * 64.0 / count as f64 * 1e4).round() / 1e4));

    // A stretch of beats of one envelope.
    let bar = ok(client, "get_automation", json!({ "target": duck, "start": 4, "end": 5 })).await;
    let points = bar["automation"][0]["points"].as_array().unwrap();
    assert!((112..=116).contains(&points.len()), "{}", points.len());
    assert!(points.iter().all(|point| (4.0..5.0).contains(&point[0].as_f64().unwrap())));
    assert!(bar.get("note").is_none() && bar["automation"][0].get("next_offset").is_none());

    // What cannot be read says why.
    let error = call(client, "get_automation", json!({ "offset": 10 })).await.unwrap_err();
    assert!(error.contains("`target`"), "{error}");
    let nothing = ok(client, "get_automation", json!({ "target": { "type": "master_volume" } })).await;
    assert_eq!(nothing["automation"], json!([]));
    assert!(nothing["note"].as_str().unwrap().contains("nothing automates"));

    // And it is saved and opened as it is.
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("many.trill");
    ok(client, "save_project", json!({ "path": path.display().to_string() })).await;
    ok(client, "load_project", json!({ "path": path.display().to_string() })).await;
    assert_eq!(ok(client, "get_project", json!({})).await["automation"][1]["point_count"], count);
}

/// A server and a client on the assistant's guarded endpoint. Its working
/// folder is `work` inside the fixture's own folder.
async fn connect_guarded() -> (Fixture, std::path::PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let work = folder.path().join("work");
    let runtime = Arc::new(Runtime::start(options_in(folder.path())).unwrap());
    let options = trill_mcp::HttpOptions { assistant_folder: Some(work.clone()) };
    let server = trill_mcp::serve_http_with(runtime.clone(), "127.0.0.1:0".parse().unwrap(), options).unwrap();
    let transport = StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(
        server.assistant_url(),
    ));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    (Fixture { session: runtime.session.clone(), runtime, client, _server: server, folder }, work)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_assistant_writes_only_into_the_users_working_folders() {
    let (fixture, work) = connect_guarded().await;
    let client = &fixture.client;
    let outside = fixture.folder.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let files_in = |folder: &std::path::Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .map(|entries| entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names
    };
    let track = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    ok(
        client,
        "create_clip",
        json!({ "track": track["track_id"], "start": 0, "length": 4, "notes": [[60, 0, 1, 100]] }),
    )
    .await;

    // Anywhere else is refused, whichever way it is spelled, and nothing is written.
    let elsewhere = outside.join("song.trill").display().to_string();
    for path in [elsewhere.as_str(), "../elsewhere/song.trill", "sub/../../elsewhere/song.trill"] {
        let error = call(client, "save_project", json!({ "path": path })).await.unwrap_err();
        assert!(error.contains("can only be written inside"), "{path}: {error}");
    }
    let error = call(client, "render", json!({ "path": outside.join("mix.wav").display().to_string() })).await;
    assert!(error.unwrap_err().contains("can only be written inside"));
    let error = call(client, "export_midi", json!({ "path": outside.join("song.mid").display().to_string() })).await;
    assert!(error.unwrap_err().contains("can only be written inside"));
    // So is the wrong kind of file, even in the right place.
    let error = call(client, "save_project", json!({ "path": "song.exe" })).await.unwrap_err();
    assert!(error.contains(".trill file, not .exe"), "{error}");
    let error = call(client, "render", json!({ "path": "song.trill" })).await.unwrap_err();
    assert!(error.contains(".wav file"), "{error}");
    assert!(files_in(&outside).is_empty() && files_in(&work).is_empty());

    // A name goes into the working folder, with the right ending.
    let saved = ok(client, "save_project", json!({ "path": "Night Drive" })).await;
    let saved = std::path::PathBuf::from(saved["saved"].as_str().unwrap());
    assert!(saved.ends_with("work/Night Drive.trill") || saved.ends_with(r"work\Night Drive.trill"), "{saved:?}");
    assert!(saved.is_file());
    // From then on the project's own folder is one, too (here the same one).
    let midi = ok(client, "export_midi", json!({ "path": "parts/keys.mid" })).await;
    assert!(std::path::Path::new(midi["path"].as_str().unwrap()).is_file());
    ok(client, "render", json!({ "path": "bounces/mix.wav", "end": 4 })).await;
    assert!(work.join("bounces").join("mix.wav").is_file());
    // A render without a place goes where renders always go.
    ok(client, "render", json!({ "end": 4 })).await;
    assert_eq!(files_in(&work), ["Night Drive.trill", "bounces", "parts", "renders"]);
    assert!(files_in(&outside).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_assistant_cannot_throw_the_users_work_away() {
    let folder = tempfile::tempdir().unwrap();
    {
        let lost = Runtime::start(options_in(folder.path())).unwrap();
        let renamed = lost.session.lock().edit(|project| {
            project.name = "Lost".into();
            Ok(())
        });
        renamed.unwrap();
        lost.refresh_safety_copy();
        lost.abandon();
    }
    let work = folder.path().join("work");
    let runtime = Arc::new(Runtime::start(options_in(folder.path())).unwrap());
    let options = trill_mcp::HttpOptions { assistant_folder: Some(work) };
    let server = trill_mcp::serve_http_with(runtime.clone(), "127.0.0.1:0".parse().unwrap(), options).unwrap();
    let connect = |url: String| async move {
        let transport = StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(url));
        ClientConfig::default().serve(transport).await.unwrap()
    };
    let assistant = connect(server.assistant_url()).await;
    let other = folder.path().join("other.trill");
    std::fs::write(&other, trill_core::Project::new("Other").to_json()).unwrap();
    let other = other.display().to_string();

    // With unsaved changes in the open project, nothing may take its place.
    ok(&assistant, "create_track", json!({ "name": "Unsaved idea" })).await;
    for (tool, arguments) in [
        ("new_project", json!({})),
        ("load_project", json!({ "path": other })),
        ("recover_project", json!({ "action": "open", "index": 0 })),
    ] {
        let error = call(&assistant, tool, arguments).await.unwrap_err();
        assert!(error.contains("unsaved changes") && error.contains("save_project"), "{tool}: {error}");
    }
    assert_eq!(ok(&assistant, "get_project", json!({})).await["tracks"][0]["name"], "Unsaved idea");
    // Nor may it delete what a crash left, saved or not.
    let error = call(&assistant, "recover_project", json!({ "action": "discard", "index": 0 })).await.unwrap_err();
    assert!(error.contains("the user's to do"), "{error}");
    assert_eq!(ok(&assistant, "recover_project", json!({})).await["unsaved"].as_array().unwrap().len(), 1);

    // Once the work is saved, it may.
    ok(&assistant, "save_project", json!({ "path": "idea" })).await;
    assert_eq!(ok(&assistant, "load_project", json!({ "path": other })).await["name"], "Other");
    assert_eq!(ok(&assistant, "recover_project", json!({ "action": "open", "index": 0 })).await["recovered"], "Lost");

    // The user's own clients, on the other endpoint, are held to none of this.
    let full = connect(server.url()).await;
    ok(&full, "create_track", json!({ "name": "Mine to lose" })).await;
    assert_eq!(ok(&full, "new_project", json!({ "name": "Fresh" })).await["name"], "Fresh");
    let anywhere = folder.path().join("anywhere").join("song.trill").display().to_string();
    ok(&full, "save_project", json!({ "path": anywhere })).await;
    assert!(std::path::Path::new(&anywhere).is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_arms_a_track_plays_it_and_records_a_take() {
    let fixture = connect().await;
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 240 })).await;
    let keys = ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    let keys = keys["track_id"].as_u64().unwrap();
    let bus = ok(client, "create_track", json!({ "name": "Bus", "kind": "group" })).await["track_id"].as_u64().unwrap();

    // Nothing is armed: recording says what to do, and so does playing
    // where a keyboard would.
    let error = call(client, "transport", json!({ "action": "record" })).await.unwrap_err();
    assert!(error.contains("arm a track first") && error.contains("update_track"), "{error}");
    let error = call(client, "play_notes", json!({ "notes": [[60, 0, 1]] })).await.unwrap_err();
    assert!(error.contains("No track is armed") && error.contains("armed"), "{error}");
    let error = call(client, "update_track", json!({ "track": bus, "armed": true })).await.unwrap_err();
    assert!(error.contains("group track"), "{error}");
    let error = call(client, "transport", json!({ "action": "record", "count_in_bars": 99 })).await.unwrap_err();
    assert!(error.contains("at most 8"), "{error}");

    // Arming is not an edit: nothing to undo, and the project is unchanged.
    let before = ok(client, "history", json!({ "action": "undo", "steps": 0 })).await;
    let armed = ok(client, "update_track", json!({ "track": keys, "armed": true })).await;
    assert_eq!(armed["armed"], true);
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["tracks"][0]["armed"], true);
    assert!(project["tracks"][1].get("armed").is_none());
    assert_eq!(fixture.session.lock().armed().len(), 1);
    let _ = before;

    // A take with no count-in: two notes played as a keyboard would play
    // them, a beat apart (a quarter of a second at this tempo).
    let started = ok(client, "transport", json!({ "action": "record", "count_in_bars": 0, "position": 4 })).await;
    assert_eq!(started["recording"]["state"], "recording");
    assert_eq!(started["recording"]["tracks"], json!([keys]));
    let played = ok(client, "play_notes", json!({ "notes": [[60, 0.5, 0.5, 90], ["E4", 1.5, 0.5]] })).await;
    assert_eq!((played["played"].as_u64(), played["tracks"].clone()), (Some(2), json!([keys])));
    assert!(played["recording"].is_string());
    let status = ok(client, "transport", json!({ "action": "status" })).await;
    assert_eq!(status["recording"]["state"], "recording");
    assert_eq!(status["recording"]["from_beat"], 4.0);
    let stopped = ok(client, "transport", json!({ "action": "stop" })).await;
    let take = stopped["take"].as_array().unwrap();
    assert_eq!(take.len(), 1, "{stopped}");
    assert_eq!(
        (take[0]["track_id"].as_u64(), take[0]["notes"].as_u64(), take[0]["new_clip"].clone()),
        (Some(keys), Some(2), json!(true))
    );

    // The notes are where they were played: after beat 4, a beat apart,
    // half a beat long, give or take what a busy machine does to a sleep.
    let clip = take[0]["clip_id"].as_u64().unwrap();
    let notes = ok(client, "get_notes", json!({ "clip": clip })).await;
    let notes = notes["notes"].as_array().unwrap().clone();
    assert_eq!(notes.len(), 2);
    assert_eq!((notes[0]["name"].as_str(), notes[1]["name"].as_str()), (Some("C4"), Some("E4")));
    assert_eq!(notes[0]["velocity"], 90);
    let start = |note: &Value| note["start"].as_f64().unwrap();
    let apart = start(&notes[1]) - start(&notes[0]);
    assert!((0.6..1.6).contains(&apart), "the notes are {apart} beats apart: {notes:?}");
    let project = ok(client, "get_project", json!({})).await;
    let recorded = &project["tracks"][0]["clips"][0];
    assert_eq!((recorded["start"].as_f64(), recorded["name"].as_str()), (Some(4.0), Some("Take")));

    // One undo takes the take back; a recording in which nothing is
    // played leaves nothing, and says so.
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(ok(client, "get_project", json!({})).await["tracks"][0]["clips"], json!([]));
    ok(client, "transport", json!({ "action": "record" })).await;
    let stopped = ok(client, "transport", json!({ "action": "stop" })).await;
    assert_eq!(stopped["take"], json!([]));
    assert!(stopped["note"].as_str().unwrap().contains("Nothing was played"));

    // A note played on a named track sounds there, armed or not; mistakes
    // are refused with the reason.
    ok(client, "update_track", json!({ "track": keys, "armed": false })).await;
    let played =
        ok(client, "play_notes", json!({ "track": keys, "notes": [{ "pitch": 72, "start": 0, "length": 0.25 }] }))
            .await;
    assert_eq!(played["tracks"], json!([keys]));
    assert!(played.get("recording").is_none());
    let error = call(client, "play_notes", json!({ "track": 999, "notes": [[60, 0, 1]] })).await.unwrap_err();
    assert!(error.contains("Existing tracks"), "{error}");
    let error = call(client, "play_notes", json!({ "track": keys, "notes": [] })).await.unwrap_err();
    assert!(error.contains("at least one note"), "{error}");
    let error = call(client, "play_notes", json!({ "track": keys, "notes": [[60, 0, 4000]] })).await.unwrap_err();
    assert!(error.contains("at most 60 s"), "{error}");

    // The metronome is the transport's to switch.
    let status = ok(client, "transport", json!({ "action": "status", "metronome": true })).await;
    assert_eq!(status["metronome"], true);
    assert!(fixture.session.lock().metronome());
}

/// The `name` of each item of a list in a reply.
fn listed(list: &Value) -> Vec<&str> {
    list.as_array().unwrap().iter().map(|item| item["name"].as_str().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_chooses_the_audio_device_and_the_midi_inputs() {
    let host = ScriptedHost::with_speakers();
    host.add_device(ScriptedDevice::new("Interface"));
    let midi = ScriptedMidi::new();
    midi.plug("Keystation");
    midi.plug("Loopback");
    midi.refuse("Loopback", Some("another program is using this port"));
    let parts = Parts { audio: Some(Arc::new(host.clone())), midi: Some(Box::new(midi.clone())), ..Parts::default() };
    let fixture = connect_with(tempfile::tempdir().unwrap(), parts).await;
    let client = &fixture.client;
    let track = ok(client, "create_track", json!({ "name": "Keys" })).await["track_id"].as_u64().unwrap();
    ok(client, "transport", json!({ "action": "seek", "position": 6 })).await;

    // What is running, and what could be.
    let status = ok(client, "audio", json!({})).await;
    assert_eq!((status["device"].as_str(), status["sample_rate"].as_f64()), (Some("Speakers"), Some(48_000.0)));
    assert_eq!((status["buffer_frames"].as_u64(), status["latency_ms"].as_f64()), (Some(480), Some(30.0)));
    assert!(status.get("asked_for").is_none() && status.get("warning").is_none(), "{status}");
    let devices = ok(client, "audio", json!({ "action": "devices" })).await;
    assert_eq!(listed(&devices["outputs"]), ["Speakers", "Interface"]);
    assert_eq!(devices["outputs"][0]["default"], true);
    assert_eq!(devices["outputs"][1]["exclusive"], true);

    // Only what is named changes; the project and the playhead stay.
    let changed =
        ok(client, "audio", json!({ "action": "set", "output": "Interface", "exclusive": true, "buffer_size": 64 }))
            .await;
    assert_eq!(
        (changed["device"].as_str(), changed["buffer_frames"].as_u64(), changed["exclusive"].clone()),
        (Some("Interface"), Some(64), json!(true))
    );
    assert!((changed["latency_ms"].as_f64().unwrap() - 1.33).abs() < 0.01, "{changed}");
    assert_eq!(changed["asked_for"]["output"], "Interface");
    let changed = ok(client, "audio", json!({ "action": "set", "buffer_size": 256 })).await;
    assert_eq!((changed["device"].as_str(), changed["buffer_frames"].as_u64()), (Some("Interface"), Some(256)));
    host.advance(512);
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(
        (project["tracks"][0]["id"].as_u64(), project["transport"]["position_beats"].as_f64()),
        (Some(track), Some(6.0))
    );
    let saved = trill_library::Settings::load(&fixture.settings_file()).settings.audio;
    assert_eq!(
        (saved.output_device.as_deref(), saved.buffer_size, saved.exclusive),
        (Some("Interface"), Some(256), true)
    );

    // What cannot be had comes back as what was opened instead, and why.
    let fallback =
        ok(client, "audio", json!({ "action": "set", "output": "Headphones", "exclusive": false, "buffer_size": 0 }))
            .await;
    assert_eq!(fallback["device"], "Speakers");
    assert!(fallback["warning"].as_str().unwrap().contains("no audio device called 'Headphones'"), "{fallback}");
    assert!(fallback["hint"].as_str().unwrap().contains("devices"));
    let back = ok(client, "audio", json!({ "action": "set", "output": "default" })).await;
    assert_eq!((back["device"].as_str(), back.get("warning")), (Some("Speakers"), None));
    assert!(back.get("asked_for").is_none(), "nothing is asked for any more: {back}");

    // The MIDI inputs: on unless switched off, with what is wrong with
    // the one that cannot be opened, and a word on what they play.
    let inputs = ok(client, "midi_inputs", json!({})).await;
    assert_eq!(listed(&inputs["inputs"]), ["Keystation", "Loopback"]);
    assert_eq!(
        (inputs["inputs"][0]["enabled"].clone(), inputs["inputs"][0]["listening"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(inputs["inputs"][1]["listening"], false);
    assert_eq!(inputs["inputs"][1]["problem"], "another program is using this port");
    assert!(inputs["note"].as_str().unwrap().contains("No track is armed"), "{inputs}");
    ok(client, "update_track", json!({ "track": track, "armed": true })).await;
    midi.send("Keystation", &[0x90, 60, 100]);
    let inputs = ok(client, "midi_inputs", json!({})).await;
    assert_eq!((inputs["plays_tracks"].clone(), inputs["inputs"][0]["messages"].as_u64()), (json!([track]), Some(1)));
    assert!(inputs.get("note").is_none());

    let off = ok(client, "midi_inputs", json!({ "action": "disable", "name": "Keystation" })).await;
    assert_eq!(
        (off["inputs"][0]["enabled"].clone(), off["inputs"][0]["listening"].clone()),
        (json!(false), json!(false))
    );
    assert!(!midi.send("Keystation", &[0x80, 60, 0]), "nobody listens to it now");
    let saved = trill_library::Settings::load(&fixture.settings_file()).settings.midi;
    assert_eq!(saved.disabled_inputs, ["Keystation"]);
    let on = ok(client, "midi_inputs", json!({ "action": "enable", "name": "Keystation" })).await;
    assert_eq!(on["inputs"][0]["listening"], true);

    let error = call(client, "midi_inputs", json!({ "action": "disable" })).await.unwrap_err();
    assert!(error.contains("need `name`"), "{error}");
    let error = call(client, "midi_inputs", json!({ "action": "disable", "name": "Theremin" })).await.unwrap_err();
    assert!(error.contains("Inputs: Keystation, Loopback"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_audio_hardware_the_device_tools_say_so() {
    let fixture = connect().await;
    let client = &fixture.client;
    let status = ok(client, "audio", json!({ "action": "devices" })).await;
    assert_eq!((status["device"].clone(), status["outputs"].clone()), (Value::Null, json!([])));
    assert!(status["note"].as_str().unwrap().contains("without sound"));
    let inputs = ok(client, "midi_inputs", json!({})).await;
    assert_eq!(inputs["inputs"], json!([]));
    assert!(inputs["note"].as_str().unwrap().contains("No MIDI input"));
    let error = call(client, "midi_inputs", json!({ "action": "enable", "name": "Keys" })).await.unwrap_err();
    assert!(error.contains("none is plugged in"), "{error}");
}

/// The entries of a journal page, as (step, who, what it did).
fn told(page: &Value) -> Vec<(u64, String, String)> {
    let entries = page["entries"].as_array().expect("a page has entries");
    let line = |entry: &Value| {
        let words = ["did", "said", "looked", "note"].iter().find_map(|key| entry[*key].as_str());
        let words = words.map(str::to_owned).unwrap_or_else(|| entry["plan"].to_string());
        (entry["step"].as_u64().unwrap(), entry["by"].as_str().unwrap().to_owned(), words)
    };
    entries.iter().map(line).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_song_made_in_several_calls_is_read_back_as_it_was_after_each() {
    let fixture = connect().await;
    let client = &fixture.client;
    let me = fixture.client.peer_info().map(|_| ()).map(|()| ClientConfig::default().client_info.name).unwrap();

    // A song, made the way a client makes one: a call at a time.
    ok(client, "set_project", json!({ "name": "Night Drive", "tempo": 96 })).await;
    let bass = ok(client, "create_track", json!({ "name": "Bass", "instrument": "builtin:polysynth" })).await;
    let (bass, synth) = (bass["track_id"].as_u64().unwrap(), bass["instrument_device_id"].as_u64().unwrap());
    let verse = ok(
        client,
        "create_clip",
        json!({ "track": bass, "start": 0, "length": 8, "name": "Verse", "notes": [["A1", 0, 1.5], ["C2", 4, 1.5]] }),
    )
    .await["clip_id"]
        .as_u64()
        .unwrap();
    ok(client, "get_project", json!({})).await;
    let drums = ok(client, "create_track", json!({ "name": "Drums", "instrument": "builtin:drumkit" })).await;
    let drums = drums["track_id"].as_u64().unwrap();
    let chorus = ok(
        client,
        "create_clip",
        json!({ "track": drums, "start": 8, "length": 8, "name": "Chorus", "notes": [["kick", 0, 0.25], ["snare", 1, 0.25]] }),
    )
    .await["clip_id"]
        .as_u64()
        .unwrap();
    ok(client, "update_track", json!({ "track": bass, "volume_db": -6 })).await;
    ok(client, "add_notes", json!({ "clip": verse, "notes": [["E2", 6, 1]] })).await;
    let measured = ok(client, "measure", json!({})).await;
    assert!(measured["loudness_lufs"].is_number(), "{measured}");
    // A call that is refused, and a step taken back.
    call(client, "delete_track", json!({ "track": 999 })).await.unwrap_err();
    ok(client, "history", json!({ "action": "undo" })).await;

    // The steps, oldest first: each says who, what, with which tool and
    // where it landed.
    let page = ok(client, "get_journal", json!({})).await;
    assert_eq!(page["keeping"], true);
    let steps = told(&page);
    let words: Vec<&str> = steps.iter().map(|(_, _, words)| words.as_str()).collect();
    assert_eq!(
        words,
        [
            "Changed the name and tempo",
            "Added the track \u{201C}Bass\u{201D}",
            "Added the clip \u{201C}Verse\u{201D} to \u{201C}Bass\u{201D}",
            "Added the track \u{201C}Drums\u{201D}",
            "Added the clip \u{201C}Chorus\u{201D} to \u{201C}Drums\u{201D}",
            "Changed the volume of \u{201C}Bass\u{201D}",
            "Added 1 note to \u{201C}Verse\u{201D}",
            "Took back: Added 1 note to \u{201C}Verse\u{201D}",
        ]
    );
    assert!(steps.iter().all(|(_, by, _)| *by == me), "every step is this client's ({me}): {steps:?}");
    assert_eq!(page["steps"], 8);
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries[1]["tool"], "create_track");
    assert_eq!(entries[1]["where"], json!({ "tracks": [bass] }));
    assert_eq!(entries[2]["where"], json!({ "clips": [verse] }));
    assert_eq!(entries[7]["took_back"], entries[6]["step"]);
    assert!(entries.iter().all(|entry| entry["t"].is_number()));
    assert!(page["began"].as_str().unwrap().ends_with(" UTC"));
    assert!(page.get("more").is_none() && page.get("earlier").is_none());

    // The song as it was after each of them.
    let step = |index: usize| steps[index].0;
    let at = |step: u64| ok(client, "get_song_at", json!({ "step": step, "include_notes": true }));
    let began = at(0).await;
    assert_eq!((began["name"].clone(), began["tempo"].clone()), (json!("Untitled"), json!(120.0)));
    assert_eq!(began["tracks"], json!([]));
    assert!(began["as_of"]["did"].as_str().unwrap().contains("when the story begins"));
    let named = at(step(0)).await;
    assert_eq!((named["name"].clone(), named["tempo"].clone()), (json!("Night Drive"), json!(96.0)));
    let with_bass = at(step(1)).await;
    assert_eq!(with_bass["tracks"].as_array().unwrap().len(), 1);
    assert_eq!(with_bass["tracks"][0]["devices"][0]["id"], synth);
    assert_eq!(with_bass["tracks"][0]["clips"], json!([]));
    assert_eq!(with_bass["as_of"]["did"], "Added the track \u{201C}Bass\u{201D}");
    let with_verse = at(step(2)).await;
    assert_eq!(with_verse["tracks"][0]["clips"][0]["notes"].as_array().unwrap().len(), 2);
    assert_eq!(with_verse["tracks"].as_array().unwrap().len(), 1, "the drums are not there yet");
    let with_chorus = at(step(4)).await;
    assert_eq!(with_chorus["tracks"][1]["clips"][0]["id"], chorus);
    assert_eq!(with_chorus["tracks"][0]["volume_db"], 0.0);
    let turned_down = at(step(5)).await;
    assert_eq!(turned_down["tracks"][0]["volume_db"], -6.0);
    let three_notes = at(step(6)).await;
    assert_eq!(three_notes["tracks"][0]["clips"][0]["notes"].as_array().unwrap().len(), 3);
    // The last step took the third note back: the song as it is now.
    let now = at(step(7)).await;
    assert_eq!(now["tracks"][0]["clips"][0]["notes"].as_array().unwrap().len(), 2);
    assert!(now["note"].as_str().unwrap().contains("as it is now"));
    let current = ok(client, "get_project", json!({ "include_notes": true })).await;
    assert_eq!(now["tracks"][0]["clips"], current["tracks"][0]["clips"]);
    let error = call(client, "get_song_at", json!({ "step": 9_999 })).await.unwrap_err();
    assert!(error.contains("there is no step 9999") && error.contains("get_journal"), "{error}");

    // "What changed since the chorus was written": found by its words,
    // then listed from there.
    let found = ok(client, "get_journal", json!({ "search": "chorus" })).await;
    assert_eq!(told(&found).len(), 1);
    let since = ok(client, "get_journal", json!({ "after": found["entries"][0]["step"] })).await;
    let since: Vec<String> = told(&since).into_iter().map(|(_, _, words)| words).collect();
    assert_eq!(
        since,
        [
            "Changed the volume of \u{201C}Bass\u{201D}",
            "Added 1 note to \u{201C}Verse\u{201D}",
            "Took back: Added 1 note to \u{201C}Verse\u{201D}"
        ]
    );

    // A page at a time, and the latest.
    let first = ok(client, "get_journal", json!({ "limit": 3 })).await;
    assert_eq!(told(&first).len(), 3);
    let next = ok(client, "get_journal", json!({ "limit": 3, "after": first["more"]["after"] })).await;
    assert_eq!(told(&next)[0].2, "Added the track \u{201C}Drums\u{201D}");
    let last = ok(client, "get_journal", json!({ "last": 2 })).await;
    assert_eq!(
        told(&last).iter().map(|(_, _, words)| words.as_str()).collect::<Vec<_>>(),
        ["Added 1 note to \u{201C}Verse\u{201D}", "Took back: Added 1 note to \u{201C}Verse\u{201D}"]
    );
    assert!(last.get("more").is_none());
    let nobody = ok(client, "get_journal", json!({ "by": "assistant" })).await;
    assert_eq!(nobody["entries"], json!([]));
    assert!(nobody["note"].as_str().unwrap().contains("No entry matches"));

    // Everything: the looks too, with what the measurement heard and the
    // call that was refused. Reading the journal is itself a look.
    let all = ok(client, "get_journal", json!({ "show": "all", "limit": 200 })).await;
    let looks: Vec<&Value> =
        all["entries"].as_array().unwrap().iter().filter(|entry| entry.get("looked").is_some()).collect();
    assert_eq!(looks[0]["looked"], "Read the project");
    let heard = looks.iter().find(|look| look["looked"] == "Measured the song").expect("the measurement is a look");
    assert!(heard["heard"].as_str().unwrap().contains("LUFS"), "{heard}");
    let refused = looks.iter().find(|look| look["failed"] == true).expect("the refused call is there");
    assert_eq!(refused["looked"], "Deleted the track track 999");
    assert!(looks.iter().any(|look| look["looked"] == "Read how the song was made"));
    assert!(looks.iter().any(|look| look["looked"] == "Read the song as it was after step 9999"));

    // The session's own journal says the same, and leads to the song.
    let session = fixture.session.lock();
    let journal = session.journal().unwrap();
    assert_eq!(&journal.head().unwrap(), session.project());
    assert_eq!(journal.steps(), 8);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_journal_is_kept_written_out_and_deleted_on_request() {
    let fixture = connect().await;
    let client = &fixture.client;
    let journals = fixture.folder.path().join("config").join("journals");
    ok(client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    fixture.session.lock().say(trill_core::journal::Author::Person, "Between the user and the assistant.");

    let status = ok(client, "journal", json!({})).await;
    assert_eq!((status["keeping"].clone(), status["steps"].clone()), (json!(true), json!(1)));
    assert!(status["kept_in"].as_str().unwrap().contains("never in the project file"));
    assert!(status["story_bytes"].as_u64().unwrap() > 100 && status["bytes"].as_u64() > status["story_bytes"].as_u64());
    assert!(status["limits"].as_str().unwrap().contains("the oldest entries are let go"));

    // Saved: the song in its folder, the journal in the program's own.
    let song = fixture.folder.path().join("songs").join("keys.trill");
    ok(client, "save_project", json!({ "path": song })).await;
    let saved = std::fs::read_to_string(&song).unwrap();
    assert!(!saved.contains("Between the user") && !saved.contains("journal"));
    let kept: Vec<_> = std::fs::read_dir(&journals).unwrap().flatten().collect();
    assert_eq!(kept.len(), 1);
    assert!(std::fs::read_to_string(kept[0].path()).unwrap().contains("Between the user and the assistant."));

    // Written out to hand on: without the conversation unless asked.
    let story = fixture.folder.path().join("songs").join("keys.trillstory");
    let exported = ok(client, "journal", json!({ "action": "export", "path": story })).await;
    assert_eq!(exported["conversation"], false);
    let text = std::fs::read_to_string(&story).unwrap();
    assert!(text.contains("Added the track") && !text.contains("Between the user"), "{text}");
    ok(client, "journal", json!({ "action": "export", "path": story, "conversation": true })).await;
    assert!(std::fs::read_to_string(&story).unwrap().contains("Between the user and the assistant."));
    let error = call(client, "journal", json!({ "action": "export" })).await.unwrap_err();
    assert!(error.contains("needs `path`"), "{error}");

    // Deleted: the file is gone and the story begins again.
    let deleted = ok(client, "journal", json!({ "action": "delete" })).await;
    assert_eq!((deleted["deleted"].clone(), deleted["steps"].clone()), (json!(true), json!(0)));
    assert_eq!(std::fs::read_dir(&journals).unwrap().count(), 0);
    let empty = ok(client, "get_journal", json!({})).await;
    assert!(empty["note"].as_str().unwrap().contains("Nothing has changed"));

    // Switched off, and remembered as switched off; the tools say so.
    let stopped = ok(client, "journal", json!({ "action": "stop" })).await;
    assert_eq!(stopped["keeping"], false);
    assert!(!trill_library::Settings::load(&fixture.settings_file()).settings.journal.keep);
    ok(client, "create_track", json!({ "name": "Unrecorded" })).await;
    let off = ok(client, "get_journal", json!({})).await;
    assert_eq!((off["keeping"].clone(), off["entries"].clone()), (json!(false), json!([])));
    assert!(call(client, "get_song_at", json!({})).await.unwrap_err().contains("switched off"));
    let kept = ok(client, "journal", json!({ "action": "keep" })).await;
    assert_eq!(kept["keeping"], true);
    assert!(trill_library::Settings::load(&fixture.settings_file()).settings.journal.keep);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_assistant_is_entered_as_itself_and_cannot_do_away_with_the_journal() {
    let (fixture, work) = connect_guarded().await;
    let client = &fixture.client;
    ok(client, "create_track", json!({ "name": "Pad" })).await;
    ok(client, "get_project", json!({})).await;
    let page = ok(client, "get_journal", json!({ "show": "all" })).await;
    let entries = told(&page);
    assert_eq!(entries[0].1, "assistant");
    assert_eq!(entries[0].2, "Added the track \u{201C}Pad\u{201D}");
    assert!(entries.iter().all(|(_, by, _)| by == "assistant"), "{entries:?}");

    for action in ["stop", "delete"] {
        let error = call(client, "journal", json!({ "action": action })).await.unwrap_err();
        assert!(error.contains("from here the journal cannot be") && error.contains("Making Of"), "{action}: {error}");
    }
    assert!(fixture.session.lock().keeps_journal());
    assert_eq!(ok(client, "journal", json!({})).await["steps"], 1);
    // It may write the story out into the user's working folders, without
    // the conversation.
    let error = call(client, "journal", json!({ "action": "export", "path": "story", "conversation": true }))
        .await
        .unwrap_err();
    assert!(error.contains("without the conversation"), "{error}");
    let outside = fixture.folder.path().join("elsewhere").join("story.trillstory");
    let error = call(client, "journal", json!({ "action": "export", "path": outside })).await.unwrap_err();
    assert!(error.contains("can only be written inside"), "{error}");
    let exported = ok(client, "journal", json!({ "action": "export", "path": "story" })).await;
    assert!(exported["exported"].as_str().unwrap().ends_with("story.trillstory"));
    assert!(work.join("story.trillstory").is_file());
}

// ---- films -----------------------------------------------------------------------------

/// A song to film: a bass through sixteen bars, a lead in the second
/// eight, and a marker on each half. Returns the two tracks' ids.
async fn song_to_film(client: &Client) -> (u64, u64) {
    let line = |pitch: u64, beats: u64| -> Vec<Value> {
        (0..beats * 2).map(|n| json!([pitch + n % 5, n as f64 * 0.5, 0.4, 100])).collect()
    };
    let bass = ok(client, "create_track", json!({ "name": "Bass", "instrument": "builtin:polysynth" })).await;
    let bass = bass["track_id"].as_u64().unwrap();
    ok(client, "create_clip", json!({ "track": bass, "start": 0, "length": 64, "notes": line(36, 64) })).await;
    let lead = ok(client, "create_track", json!({ "name": "Lead", "instrument": "builtin:polysynth" })).await;
    let lead = lead["track_id"].as_u64().unwrap();
    ok(client, "create_clip", json!({ "track": lead, "start": 32, "length": 32, "notes": line(72, 32) })).await;
    let markers = json!({ "markers": [{ "beat": 0, "name": "Verse" }, { "beat": 32, "name": "Chorus" }] });
    ok(client, "set_markers", markers).await;
    (bass, lead)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_film_is_planned_take_by_take_and_directed_before_any_of_it_is_made() {
    let fixture = connect().await;
    let client = &fixture.client;
    let nothing = call(client, "film_plan", json!({})).await.unwrap_err();
    assert!(nothing.contains("nothing to film"), "{nothing}");
    let (bass, lead) = song_to_film(client).await;

    // The director's own plan: the song from its first bar to its last, take by take.
    let plan = ok(client, "film_plan", json!({})).await;
    assert_eq!(
        (plan["of"].as_str(), plan["bars"].as_str(), plan["seconds"].as_f64()),
        (Some("song"), Some("1-16"), Some(32.0))
    );
    let takes = plan["takes"].as_array().unwrap();
    assert_eq!(takes[0]["at_seconds"], 0.0);
    for take in takes {
        let (shot, enter) = (take["shot"].as_str().unwrap(), take["enter"].as_str().unwrap());
        assert!(["song", "wide", "ensemble", "close"].contains(&shot), "{take}");
        assert!(["cut", "move", "carries on"].contains(&enter), "{take}");
    }
    assert_eq!(plan["parts"], json!([{ "track": bass, "name": "Bass" }, { "track": lead, "name": "Lead" }]));

    // Directed: the chorus held close on the lead and the verse wide, each as asked.
    let direct = json!([
        { "section": "chorus", "shot": "close", "parts": [lead] },
        { "from_bar": 1, "to_bar": 8, "shot": "wide", "enter": "cut" },
    ]);
    let directed = ok(client, "film_plan", json!({ "direct": direct })).await;
    let takes = directed["takes"].as_array().unwrap();
    assert_eq!(takes.len(), 2, "{directed}");
    assert_eq!((takes[0]["bars"].as_str(), takes[0]["shot"].as_str()), (Some("1-8"), Some("wide")));
    assert_eq!((takes[1]["bars"].as_str(), takes[1]["shot"].as_str()), (Some("9-16"), Some("close")));
    assert_eq!((&takes[1]["parts"], takes[1]["at_seconds"].as_f64()), (&json!(["Lead"]), Some(16.0)));
    assert_eq!((takes[1]["section"].as_str(), takes[1]["enter"].as_str()), (Some("Chorus"), Some("cut")));

    // Of a stretch, only its takes, timed from where the film begins.
    let chorus = ok(client, "film_plan", json!({ "section": "Chorus" })).await;
    assert_eq!((chorus["bars"].as_str(), chorus["seconds"].as_f64()), (Some("9-16"), Some(16.0)));
    assert_eq!(chorus["takes"][0]["at_seconds"], 0.0);

    // A mistake says what does exist, and `film` says it before it does any work.
    for tool in ["film_plan", "film"] {
        let unknown = call(client, tool, json!({ "section": "Bridge" })).await.unwrap_err();
        assert!(unknown.contains("the song's sections are \"Verse\", \"Chorus\""), "{tool}: {unknown}");
        let nobody = json!({ "direct": [{ "section": "Verse", "parts": [9999] }] });
        let nobody = call(client, tool, nobody).await.unwrap_err();
        assert!(nobody.contains("9999") && nobody.contains(&bass.to_string()), "{tool}: {nobody}");
        let speed = call(client, tool, json!({ "speed": 4 })).await.unwrap_err();
        assert!(speed.contains("making_of"), "{tool}: {speed}");
    }
    let size = call(client, "film", json!({ "size": "huge" })).await.unwrap_err();
    assert!(size.contains("'huge'") && size.contains("\"upright\""), "{size}");
    let rate = call(client, "film", json!({ "rate": 240 })).await.unwrap_err();
    assert!(rate.contains("24 to 60"), "{rate}");

    // How the song was made is a film too: the five steps above, where each landed.
    let making = ok(client, "film_plan", json!({ "of": "making_of" })).await;
    assert_eq!((making["of"].as_str(), making["steps"].as_u64()), (Some("making_of"), Some(5)));
    let shots = making["first_shots"].as_array().unwrap();
    assert_eq!(making["shots"].as_u64(), Some(shots.len() as u64));
    assert!(shots.iter().any(|shot| shot["shot"] == "close" && shot["part"] == "Lead"), "{making}");
    assert!(making["seconds"].as_f64().unwrap() > making["story_seconds"].as_f64().unwrap(), "{making}");
    let whole = call(client, "film_plan", json!({ "of": "making_of", "section": "Verse" })).await.unwrap_err();
    assert!(whole.contains("filmed whole"), "{whole}");

    // Reading a plan changes nothing and is no step of the story.
    assert_eq!(fixture.session.lock().journal().unwrap().steps(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_assistant_may_film_only_into_its_working_folders() {
    let (fixture, _work) = connect_guarded().await;
    let client = &fixture.client;
    song_to_film(client).await;
    let outside = fixture.folder.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let elsewhere = outside.join("song.mp4").display().to_string();
    for path in [elsewhere.as_str(), "../elsewhere/song.mp4", "films/../../elsewhere/song.mp4"] {
        let refusal = call(client, "film", json!({ "path": path })).await.unwrap_err();
        assert!(refusal.contains("a film can only be written inside"), "{path}: {refusal}");
    }
    // Nor anything but a film, even in the right place.
    let refusal = call(client, "film", json!({ "path": "song.exe" })).await.unwrap_err();
    assert!(refusal.contains("a film is written as a .mp4 file, not .exe"), "{refusal}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing was written");
}

/// The real thing: a short film made over the assistant's endpoint with no
/// place named, which has to land in its working folder.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a graphics card; run with `cargo test -p trill-mcp --test mcp a_film_is_made -- --ignored --nocapture`"]
async fn a_film_is_made_with_the_songs_own_sound_and_kept_in_the_working_folder() {
    let (fixture, work) = connect_guarded().await;
    let client = &fixture.client;
    song_to_film(client).await;
    let asked = json!({ "section": "Chorus", "size": "640x360", "rate": 24, "quality": "plain" });
    let film = ok(client, "film", asked).await;
    println!("{film}");
    let path = PathBuf::from(film["path"].as_str().unwrap());
    let films = work.join("films").canonicalize().unwrap();
    assert!(path.canonicalize().unwrap().starts_with(&films), "{} is not in {}", path.display(), films.display());
    assert_eq!((film["size"].as_str(), film["rate"].as_u64()), (Some("640x360"), Some(24)));
    // The chorus is sixteen seconds, and the film stays for what rings on after it.
    let (seconds, frames) = (film["seconds"].as_f64().unwrap(), film["frames"].as_f64().unwrap());
    assert!((16.0..20.0).contains(&seconds), "{film}");
    assert!((frames - seconds * 24.0).abs() <= 2.0, "{film}");
    assert!(film["loudness_lufs"].as_f64().unwrap() > -40.0, "the song's own sound was rendered: {film}");
    match path.is_dir() {
        // Without ffmpeg: the frames and the sound as files, and a note that says so.
        true => {
            assert!(film["notes"].to_string().contains("ffmpeg"), "{film}");
            assert!(std::fs::read_dir(&path).unwrap().count() as f64 > frames);
        }
        false => assert!(std::fs::metadata(&path).unwrap().len() > 50_000, "{film}"),
    }
    // The sound it was put together from is not left lying beside it.
    let beside: Vec<_> = std::fs::read_dir(&films).unwrap().flatten().map(|entry| entry.file_name()).collect();
    assert_eq!(beside.len(), 1, "{beside:?}");

    // And the making of it, the song under it.
    let making =
        ok(client, "film", json!({ "of": "making_of", "size": "640x360", "rate": 24, "quality": "plain" })).await;
    println!("{making}");
    assert_eq!(making["of"], "making_of");
    assert!(making["frames"].as_u64().unwrap() > 100, "{making}");
}
