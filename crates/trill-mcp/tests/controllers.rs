//! MIDI controllers over MCP: an assistant reads and writes a clip's curves
//! by name, gives a line its dynamics from its notes in one call, and is
//! told what would otherwise go unheard.
//!
//! The first part needs nothing installed. The second is the proof against
//! the real thing, on a machine that has it: a physically modelled
//! saxophone that is silent without expression (SWAM, VST3), and a CLAP
//! synth that takes MIDI (Surge XT). Those tests are skipped unless asked
//! for:
//!
//! ```sh
//! cargo test -p trill-mcp --test controllers -- --include-ignored --nocapture
//! ```
//!
//! They host the plugins without their windows, read no file of the
//! user's beside the plugins themselves, and want a machine that is not
//! busy: a plugin that is slow to answer fails them.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use trill_engine::{AudioBackend, SharedSession};
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    session: SharedSession,
    client: Client,
    _server: HttpServer,
}

async fn serve(runtime: Arc<Runtime>) -> Fixture {
    let session = runtime.session.clone();
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { session, client, _server: server }
}

async fn connect() -> Fixture {
    // Nothing of the user's is read or written: no settings, no library.
    serve(Arc::new(Runtime::start(RuntimeOptions::hermetic()).unwrap())).await
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

/// A track with a sine that starts and stops at once, and its id.
async fn sine_track(client: &Client, name: &str) -> u64 {
    let track = ok(client, "create_track", json!({ "name": name, "instrument": "builtin:polysynth" })).await;
    let device = track["devices"][0]["id"].as_u64().or(track["instrument_device_id"].as_u64());
    let device = match device {
        Some(device) => device,
        None => ok(client, "get_project", json!({})).await["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|found| found["id"] == track["track_id"])
            .and_then(|found| found["devices"][0]["id"].as_u64())
            .expect("the track's instrument"),
    };
    let params = json!({ "osc1_wave": "sine", "attack": 0.001, "release": 0.01, "sustain": 1.0 });
    ok(client, "update_device", json!({ "device": device, "params": params })).await;
    track["track_id"].as_u64().unwrap()
}

/// The clip's curve for `controller`, as `[beat, value, shape?]` points.
async fn curve(client: &Client, clip: u64, controller: &str) -> Vec<Value> {
    let reply = ok(client, "get_controllers", json!({ "clip": clip, "controller": controller })).await;
    reply["controllers"][0]["points"].as_array().cloned().unwrap_or_default()
}

fn names(curves: &Value) -> Vec<&str> {
    curves.as_array().unwrap().iter().map(|curve| curve["controller"].as_str().unwrap()).collect()
}

/// The left channel of a WAV file.
fn samples(path: &Path) -> (Vec<f32>, f64) {
    let audio = trill_media::decode(path).unwrap();
    (audio.left.clone(), audio.sample_rate)
}

fn rms(samples: &[f32]) -> f64 {
    (samples.iter().map(|sample| f64::from(*sample) * f64::from(*sample)).sum::<f64>() / samples.len().max(1) as f64)
        .sqrt()
}

fn db(level: f64) -> f64 {
    if level > 0.0 { 20.0 * level.log10() } else { -200.0 }
}

/// The level of `seconds` of sound, in dB.
fn level(audio: &(Vec<f32>, f64), seconds: std::ops::Range<f64>) -> f64 {
    let frame = |second: f64| ((second * audio.1) as usize).min(audio.0.len());
    db(rms(&audio.0[frame(seconds.start)..frame(seconds.end)]))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tools_and_what_they_take_are_there_to_be_found() {
    let fixture = connect().await;
    let tools = fixture.client.list_all_tools().await.unwrap();
    let schema_of = |name: &str| {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap_or_else(|| panic!("missing tool {name}"));
        assert!(tool.description.as_ref().is_some_and(|text| text.len() > 40), "{name} needs a description");
        serde_json::to_string(&tool.input_schema).unwrap()
    };
    // The names a controller goes by, the values it takes and what a
    // curve's shape means are in the schemas an assistant reads.
    let set = schema_of("set_controllers");
    for word in ["expression", "mod_wheel", "pitch_bend", "sustain", "breath", "0-127", "hold", "-1"] {
        assert!(set.contains(word), "set_controllers should say '{word}': {set}");
    }
    assert!(schema_of("get_controllers").contains("controller"));
    assert!(schema_of("create_clip").contains("controllers") && schema_of("play_notes").contains("expression"));
    let shape = schema_of("shape_dynamics");
    for word in ["high", "low", "rest", "swell", "phrase_gap", "long_note", "breath"] {
        assert!(shape.contains(word), "shape_dynamics should say '{word}': {shape}");
    }
    let instructions = fixture.client.peer_info().unwrap().instructions.clone().unwrap_or_default();
    for word in ["shape_dynamics", "set_controllers", "expression", "pitch_bend", "breathes between phrases"] {
        assert!(instructions.contains(word), "the instructions should mention {word}");
    }
    assert!(!instructions.contains("set_automation on it"), "the stopgap advice is gone");
}

#[tokio::test(flavor = "multi_thread")]
async fn curves_are_written_and_read_by_name_in_a_few_points() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track = sine_track(client, "Sax").await;
    // A clip made with its notes and a curve in one call.
    let made = ok(
        client,
        "create_clip",
        json!({
            "track": track, "start": 4, "length": 8,
            "notes": [["A4", 0, 4], ["C5", 4, 4]],
            "controllers": { "expression": [[0, 30], [4, 127, 0.4], [8, 64]] },
        }),
    )
    .await;
    let clip = made["clip_id"].as_u64().unwrap();
    assert_eq!(made["controllers"], json!(["expression"]));
    assert!(made.get("warnings").is_none(), "{made}");

    // More of them, by name and by number, each in its own values.
    let written = ok(
        client,
        "set_controllers",
        json!({
            "clip": clip,
            "controllers": {
                "sustain": [[0, 127], [3.5, 0], { "beat": 4, "value": 127 }],
                "pitch_bend": [[2, 0], [3, -1, "hold"], [4, 0]],
                "mod_wheel": [[4, 0], [8, 90]],
                "23": [[0, 64]],
            },
        }),
    )
    .await;
    assert_eq!(names(&written["controllers"]), ["mod_wheel", "expression", "23", "sustain", "pitch_bend"]);
    assert_eq!(written["controllers"][1]["range"], "30..127");
    assert_eq!(written["controllers"][1]["number"], 11);
    let warnings = written["warnings"].as_array().expect("the synth has no use for controller 23");
    assert!(warnings[0].as_str().unwrap().contains("does not respond to controller 23"), "{written}");
    assert!(warnings[0].as_str().unwrap().contains("expression"), "and what it does respond to: {written}");

    // Read back as they were written: a pedal steps without being told to.
    assert_eq!(
        curve(client, clip, "expression").await,
        [json!([0.0, 30.0]), json!([4.0, 127.0, 0.4]), json!([8.0, 64.0])]
    );
    assert_eq!(curve(client, clip, "sustain").await, [json!([0.0, 127.0]), json!([3.5, 0.0]), json!([4.0, 127.0])]);
    assert_eq!(curve(client, clip, "cc64").await.len(), 3, "a name or a number");
    assert_eq!(
        curve(client, clip, "pitch_bend").await,
        [json!([2.0, 0.0]), json!([3.0, -1.0, "hold"]), json!([4.0, 0.0])]
    );
    {
        let session = fixture.session.lock();
        let found = session.project().clip(trill_core::ClipId(clip)).unwrap();
        let pedal = found.controller(trill_core::ControllerKind::SUSTAIN).unwrap();
        assert!(pedal.points.iter().all(|point| point.shape == trill_core::Segment::Hold));
        let swell = found.controller(trill_core::ControllerKind::EXPRESSION).unwrap();
        assert_eq!(swell.points[1].value, 1.0, "kept as a position from 0 to 1");
        assert_eq!(found.controller_value_at(trill_core::ControllerKind::SUSTAIN, 4.0 + 3.75), Some(0.0));
    }

    // The notes' tool lists the curves with their points, the project's
    // without: which controllers a clip holds is seen without opening it.
    let all = ok(client, "get_controllers", json!({ "clip": clip })).await;
    assert_eq!(names(&all["controllers"]).len(), 5);
    let notes = ok(client, "get_notes", json!({ "clip": clip })).await;
    assert_eq!(notes["controllers"][1]["points"].as_array().unwrap().len(), 3);
    assert_eq!(notes["controllers"][2]["controller"], "23", "one without a name goes by its number");
    let project = ok(client, "get_project", json!({})).await;
    let listed = &project["tracks"][0]["clips"][0]["controllers"];
    assert_eq!(names(listed).len(), 5);
    assert!(listed[0].get("points").is_none() && listed[0]["point_count"] == 2, "{listed}");

    // One call, one undo step; an empty list takes a curve out.
    let removed =
        ok(client, "set_controllers", json!({ "clip": clip, "controllers": { "23": [], "mod_wheel": [] } })).await;
    assert_eq!(removed["removed"], json!(["mod_wheel", "23"]));
    assert_eq!(names(&removed["controllers"]), ["expression", "sustain", "pitch_bend"]);
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(names(&ok(client, "get_controllers", json!({ "clip": clip })).await["controllers"]).len(), 5);
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(names(&ok(client, "get_controllers", json!({ "clip": clip })).await["controllers"]), ["expression"]);
    let none = ok(client, "get_controllers", json!({ "clip": clip, "controller": "breath" })).await;
    assert!(none["note"].as_str().unwrap().contains("no breath curve"), "{none}");

    // The curves go where the clip goes.
    ok(client, "split_clip", json!({ "clip": clip, "at": 8 })).await;
    let project = ok(client, "get_project", json!({})).await;
    let clips = project["tracks"][0]["clips"].as_array().unwrap();
    assert_eq!(clips.len(), 2);
    for half in clips {
        assert_eq!(names(&half["controllers"]), ["expression"], "both halves keep the curve: {half}");
        assert_eq!(curve(client, half["id"].as_u64().unwrap(), "expression").await.len(), 3);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mistakes_with_controllers_say_what_to_do() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track = sine_track(client, "Keys").await;
    let clip = ok(client, "create_clip", json!({ "track": track, "start": 0, "length": 4 })).await["clip_id"].as_u64();
    let set = |controllers: Value| call(client, "set_controllers", json!({ "clip": clip, "controllers": controllers }));

    let error = set(json!({ "wobble": [[0, 1]] })).await.unwrap_err();
    assert!(error.contains("'wobble' is not a controller") && error.contains("\"expression\""), "{error}");
    let error = set(json!({ "expression": [[0, 64], [1, 200]] })).await.unwrap_err();
    assert_eq!(error, "expression: points[1]: expression value 200 is outside 0..=127");
    let error = set(json!({ "pitch_bend": [[0, 64]] })).await.unwrap_err();
    assert!(error.contains("outside -1..=1"), "{error}");
    let error = set(json!({ "expression": [[0, 64, "wobbly"]] })).await.unwrap_err();
    assert!(error.contains("shape 'wobbly'"), "{error}");
    let error = set(json!({ "123": [[0, 64]] })).await.unwrap_err();
    assert!(error.contains("'123' is not a controller"), "{error}");
    let error = set(json!({ "expression": [[0, 64]], "11": [[0, 30]] })).await.unwrap_err();
    assert!(error.contains("given twice"), "{error}");
    let error = set(json!({})).await.unwrap_err();
    assert!(error.contains("at least one"), "{error}");
    let error = call(client, "set_controllers", json!({ "clip": 999, "controllers": { "expression": [[0, 64]] } }))
        .await
        .unwrap_err();
    assert!(error.contains("Existing clips"), "{error}");
    // Nothing was changed by any of it.
    assert_eq!(ok(client, "get_controllers", json!({ "clip": clip })).await["controllers"], json!([]));

    // What the reply cannot show: points past what the clip plays, and a
    // track with nothing to respond.
    let late = set(json!({ "expression": [[0, 64], [6, 127]] })).await.unwrap();
    assert!(late["warnings"][0].as_str().unwrap().contains("after beat 4"), "{late}");
    let empty = ok(client, "create_track", json!({ "name": "Empty" })).await["track_id"].as_u64().unwrap();
    let made = ok(
        client,
        "create_clip",
        json!({ "track": empty, "start": 0, "length": 4, "controllers": { "expression": [[0, 64]] } }),
    )
    .await;
    assert!(made["warnings"][0].as_str().unwrap().contains("has no instrument"), "{made}");
    let error = call(
        client,
        "play_notes",
        json!({ "track": track, "notes": [[60, 0, 0.1]], "controllers": { "expression": 300 } }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("expression value 300 is outside 0..=127"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_call_makes_a_line_breathe() {
    let fixture = connect().await;
    let client = &fixture.client;
    let dir = tempfile::tempdir().unwrap();
    let track = sine_track(client, "Sax").await;
    // A rising phrase to a long high note, a beat of rest, and a lower
    // answer.
    let notes = json!([
        ["C4", 0, 1],
        ["D4", 1, 1],
        ["E4", 2, 1],
        ["G4", 3, 4],
        ["F4", 7, 1],
        ["D4", 9, 1],
        ["E4", 10, 1],
        ["C4", 11, 2],
    ]);
    let written = ok(client, "create_clip", json!({ "track": track, "start": 0, "length": 16, "notes": notes })).await;
    let clip = written["clip_id"].as_u64().unwrap();
    let render = |name: &'static str| {
        let path = dir.path().join(name);
        async move {
            let report =
                ok(client, "render", json!({ "path": path.display().to_string(), "max_tail_seconds": 0.2 })).await;
            (samples(&path), report)
        }
    };
    let (plain, _) = render("plain.wav").await;

    let shaped = ok(client, "shape_dynamics", json!({ "clip": clip })).await;
    assert_eq!(shaped["controller"], "expression");
    let phrases = shaped["phrases"].as_array().unwrap();
    assert_eq!(phrases.len(), 2, "{shaped}");
    assert_eq!((phrases[0]["from"].as_str(), phrases[0]["to"].as_str()), (Some("1.1"), Some("3.1")));
    assert_eq!(phrases[0]["notes"], 5);
    assert!(
        phrases[0]["peak"].as_str().unwrap().starts_with("G4 at 1.4")
            || phrases[0]["peak"].as_str().unwrap().starts_with("G4 at 2."),
        "{shaped}"
    );
    assert_eq!(phrases[1]["from"], "3.2");
    assert!(phrases[0]["peak_level"].as_f64().unwrap() > phrases[1]["peak_level"].as_f64().unwrap(), "{shaped}");
    assert_eq!(shaped["levels"], json!({ "rest": 0.0, "low": 57.0, "high": 114.0 }));
    let count = shaped["point_count"].as_u64().unwrap();
    assert!((6..=24).contains(&count), "a few points, not thousands: {shaped}");
    assert_eq!(curve(client, clip, "expression").await.len() as u64, count);
    assert!(shaped.get("warnings").is_none(), "{shaped}");

    // Heard: quiet where the line starts, loudest in the long high note,
    // nothing in the rest, and softer overall than the line at one level.
    // (120 BPM: a beat is half a second.)
    let (heard, _) = render("shaped.wav").await;
    let (start, peak, breath, answer) =
        (level(&heard, 0.1..0.4), level(&heard, 2.2..2.8), level(&heard, 4.05..4.45), level(&heard, 5.6..6.4));
    assert!(peak > start + 4.0, "it rises into the high note: {start:.1} dB to {peak:.1} dB");
    assert!(breath < peak - 50.0, "it breathes between the phrases: {breath:.1} dB");
    assert!(
        answer > breath + 30.0 && answer < peak - 2.0,
        "the lower answer is softer: {answer:.1} dB against {peak:.1} dB"
    );
    assert!(level(&plain, 4.05..4.45) < -90.0 && level(&plain, 2.2..2.8) > peak, "the notes are the same notes");

    // It is one undo step, and the same call writes the same curve.
    let again = ok(client, "shape_dynamics", json!({ "clip": clip })).await;
    assert_eq!(again["point_count"], shaped["point_count"]);
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(curve(client, clip, "expression").await.len(), 0);

    // The levels and the reading of the line are the caller's to set; and
    // a stretch of the clip can be shaped by itself.
    let gentle = json!({ "clip": clip, "high": 90, "low": 70, "rest": 70, "swell": 0, "phrase_gap": 4 });
    let gentle = ok(client, "shape_dynamics", gentle).await;
    assert_eq!(gentle["phrases"].as_array().unwrap().len(), 1, "a longer gap makes one phrase of it: {gentle}");
    let values = |points: &[Value]| points.iter().map(|point| point[1].as_f64().unwrap()).collect::<Vec<_>>();
    let all = values(&curve(client, clip, "expression").await);
    assert!(all.iter().all(|value| (49.0..=90.5).contains(value)), "{all:?}");
    let first_phrase = |session: &SharedSession| -> Vec<f64> {
        let session = session.lock();
        let found = session.project().clip(trill_core::ClipId(clip)).unwrap();
        let swell = found.controller(trill_core::ControllerKind::EXPRESSION).unwrap();
        (0..32).map(|step| swell.value_at(f64::from(step) * 0.25)).collect()
    };
    let before = first_phrase(&fixture.session);
    let second = ok(client, "shape_dynamics", json!({ "clip": clip, "start": 8, "high": 127, "low": 100 })).await;
    assert_eq!(second["phrases"].as_array().unwrap().len(), 1);
    assert_eq!(first_phrase(&fixture.session), before, "what came before the stretch is as it was");
    let after = curve(client, clip, "expression").await;
    assert!(values(&after).iter().any(|value| *value > 100.0), "the answer is loud now: {after:?}");

    // Another controller, for an instrument that listens to it; and the
    // refusals that say what to do instead.
    let breath = ok(client, "shape_dynamics", json!({ "clip": clip, "controller": "breath" })).await;
    assert!(breath["warnings"][0].as_str().unwrap().contains("does not respond to breath"), "{breath}");
    let error = call(client, "shape_dynamics", json!({ "clip": clip, "controller": "pitch_bend" })).await.unwrap_err();
    assert!(error.contains("writes a level") && error.contains("set_controllers"), "{error}");
    let error = call(client, "shape_dynamics", json!({ "clip": clip, "low": 100, "high": 50 })).await.unwrap_err();
    assert!(error.contains("`low` 100 is above `high` 50"), "{error}");
    let error = call(client, "shape_dynamics", json!({ "clip": clip, "start": 64 })).await.unwrap_err();
    assert!(error.contains("no notes starting in that stretch"), "{error}");
    let bare = ok(client, "create_clip", json!({ "track": track, "start": 32, "length": 4 })).await["clip_id"].as_u64();
    let error = call(client, "shape_dynamics", json!({ "clip": bare })).await.unwrap_err();
    assert!(error.contains("no notes to shape") && error.contains("write_melody"), "{error}");
    let error = call(client, "shape_dynamics", json!({ "clip": clip, "swell": 3 })).await.unwrap_err();
    assert!(error.contains("`swell` 3 is outside 0..=1"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn controllers_are_recorded_exported_and_imported_with_the_notes() {
    let fixture = connect().await;
    let client = &fixture.client;
    let dir = tempfile::tempdir().unwrap();
    ok(client, "set_project", json!({ "tempo": 240 })).await;
    let track = sine_track(client, "Keys").await;
    ok(client, "update_track", json!({ "track": track, "armed": true })).await;

    // A take: the expression set as the notes are played is in the clip.
    ok(client, "transport", json!({ "action": "record", "count_in_bars": 0 })).await;
    let notes =
        json!({ "notes": [[60, 0.5, 0.5], [64, 1.5, 0.5]], "controllers": { "expression": 100, "mod_wheel": 32 } });
    let played = ok(client, "play_notes", notes).await;
    assert!(played["recording"].is_string());
    let stopped = ok(client, "transport", json!({ "action": "stop" })).await;
    let take = &stopped["take"][0];
    assert_eq!(
        (take["notes"].as_u64(), take["controllers"].clone()),
        (Some(2), json!(["mod_wheel", "expression"])),
        "{stopped}"
    );
    let clip = take["clip_id"].as_u64().unwrap();
    let level = curve(client, clip, "expression").await;
    assert_eq!(level.len(), 1, "{level:?}");
    assert!((level[0][1].as_f64().unwrap() - 100.0).abs() < 0.01, "{level:?}");

    // Out to a MIDI file and back: the curves travel with the notes.
    ok(
        client,
        "set_controllers",
        json!({ "clip": clip, "controllers": { "expression": [[0, 20], [2, 120]], "sustain": [[1, 127], [1.5, 0]] } }),
    )
    .await;
    let file = dir.path().join("song.mid").display().to_string();
    let written = ok(client, "export_midi", json!({ "path": file })).await;
    assert_eq!((written["notes"].as_u64(), written["controller_curves"].as_u64()), (Some(2), Some(3)), "{written}");
    ok(client, "new_project", json!({})).await;
    let imported = ok(client, "import_midi", json!({ "path": file })).await;
    assert_eq!(imported["tracks"][0]["controllers"], json!(["mod_wheel", "expression", "sustain"]), "{imported}");
    let back = imported["tracks"][0]["clip_id"].as_u64().unwrap();
    let swell = curve(client, back, "expression").await;
    let value = |point: &Value| point[1].as_f64().unwrap();
    assert!(swell.len() <= 4, "breakpoints, not every event: {swell:?}");
    assert!((value(&swell[0]) - 20.0).abs() < 1.5 && (value(swell.last().unwrap()) - 120.0).abs() < 1.5, "{swell:?}");
    assert_eq!(curve(client, back, "sustain").await, [json!([1.0, 127.0]), json!([1.5, 0.0])]);
}

// ---- the real thing ---------------------------------------------------------

/// SWAM Alto Sax 3, as a VST3 plugin: silent until it is given expression.
const SWAM_SAX: &str = "vst3:565354575361337377616D20616C746F";
/// Surge XT, as a CLAP plugin: it takes MIDI.
const SURGE: &str = "clap:org.surge-synth-team.surge-xt";

/// A server whose devices include the plugins installed on this machine,
/// found with caches of the test's own; `None` if `uri` is not among them.
async fn connect_with_plugins(uri: &str) -> Option<(Fixture, tempfile::TempDir)> {
    use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-controllers");
    let clap =
        ClapConfig { cache_path: Some(scratch.join("clap.json")), scanner: Scanner::Auto, ..ClapConfig::default() };
    let vst3 = Vst3Config {
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    let host = PluginHost::with_configs(clap, vst3).unwrap();
    let installed = |host: &PluginHost| {
        use trill_core::device::DeviceFactory;
        host.clap().descriptors().into_iter().chain(host.vst3().descriptors()).any(|descriptor| descriptor.uri == uri)
    };
    if !installed(&host) {
        eprintln!("skipping: {uri} is not installed on this machine");
        return None;
    }
    let folder = tempfile::tempdir().unwrap();
    let storage = Storage::At {
        settings: folder.path().join("config").join("settings.json"),
        library_index: Some(folder.path().join("cache").join("library-index.json")),
    };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let parts = Parts { devices: host.factories(), ..Parts::default() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    Some((serve(runtime).await, folder))
}

/// The pitch of a steady tone, in hertz, from the lag at which it best
/// matches itself.
fn pitch(samples: &[f32], sample_rate: f64) -> f64 {
    let (shortest, longest) = ((sample_rate / 1_000.0) as usize, (sample_rate / 60.0) as usize);
    let window = samples.len() - longest - 1;
    let matched = |lag: usize| -> f64 {
        (0..window).map(|frame| f64::from(samples[frame]) * f64::from(samples[frame + lag])).sum::<f64>()
    };
    let scores: Vec<f64> = (0..=longest + 1).map(|lag| if lag < shortest { f64::MIN } else { matched(lag) }).collect();
    let best = scores.iter().copied().fold(f64::MIN, f64::max);
    // The first lag that matches nearly as well as the best: a later one
    // is the same wave an octave down.
    let lag = (shortest + 1..=longest)
        .find(|&lag| scores[lag] > 0.9 * best && scores[lag] >= scores[lag - 1] && scores[lag] >= scores[lag + 1])
        .unwrap_or(shortest);
    // Between frames, by the parabola through the three scores about it.
    let (before, at, after) = (scores[lag - 1], scores[lag], scores[lag + 1]);
    let offset = if (before - 2.0 * at + after).abs() > 0.0 {
        0.5 * (before - after) / (before - 2.0 * at + after)
    } else {
        0.0
    };
    sample_rate / (lag as f64 + offset)
}

/// With the installed SWAM Alto Sax 3: notes alone render to silence, as
/// its own dialog says they will; the same notes with an expression curve
/// in the clip render with sound that follows the curve, from the top and
/// from the middle; and one call to shape_dynamics makes a line breathe.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs SWAM Alto Sax 3 (VST3) installed; run with `cargo test -p trill-mcp --test controllers -- --include-ignored --nocapture`"]
async fn a_saxophone_that_waits_for_expression_plays_a_clips_curve() {
    let Some((fixture, folder)) = connect_with_plugins(SWAM_SAX).await else { return };
    let client = &fixture.client;
    let said = |what: &str, reply: &Value| eprintln!("\n> {what}\n{reply}");
    let track = ok(client, "create_track", json!({ "name": "Alto", "instrument": SWAM_SAX })).await;
    let track = track["track_id"].as_u64().unwrap();
    // Four long notes in the middle of the alto's range, sixteen beats.
    let notes = json!([["C4", 0, 3.9], ["E4", 4, 3.9], ["G4", 8, 3.9], ["E4", 12, 3.9]]);
    let clip = ok(client, "create_clip", json!({ "track": track, "start": 0, "length": 16, "notes": notes })).await;
    let clip = clip["clip_id"].as_u64().unwrap();
    {
        let session = fixture.session.lock();
        let support = session.controller_support(trill_core::TrackId(track));
        eprintln!("\nwhat the plugin says it responds to: {support:?}");
        assert_eq!(support.responds_to(trill_core::ControllerKind::EXPRESSION), Some(true), "{support:?}");
    }
    let render = |name: &'static str, arguments: Value| {
        let path = folder.path().join(name);
        async move {
            let mut arguments = arguments;
            arguments["path"] = json!(path.display().to_string());
            arguments["max_tail_seconds"] = json!(1.0);
            let report = ok(client, "render", arguments).await;
            (samples(&path), report)
        }
    };

    // Notes alone: nothing, and the reply says what the answer is.
    let (silent, report) = render("silent.wav", json!({})).await;
    said("render, notes only", &json!({ "peak_db": report["peak_db"], "notes": report["notes"] }));
    assert!(report["peak_db"].as_f64().unwrap() < -100.0, "{report}");
    let advice =
        report["notes"].as_array().unwrap().iter().map(|note| note.as_str().unwrap()).collect::<Vec<_>>().join(" ");
    assert!(advice.contains("shape_dynamics") && advice.contains("set_controllers"), "{advice}");
    assert!(level(&silent, 1.0..7.0) < -100.0);

    // A crescendo over the whole clip: sound, louder where the curve is
    // higher.
    let rising = json!({ "clip": clip, "controllers": { "expression": [[0, 25], [16, 120]] } });
    said("set_controllers", &ok(client, "set_controllers", rising).await);
    let (crescendo, report) = render("crescendo.wav", json!({})).await;
    let thirds = [
        level(&crescendo, 0.4..1.8),
        level(&crescendo, 2.4..3.8),
        level(&crescendo, 4.4..5.8),
        level(&crescendo, 6.4..7.8),
    ];
    said("render, crescendo", &json!({ "peak_db": report["peak_db"], "level_of_each_note_db": thirds }));
    assert!(report["peak_db"].as_f64().unwrap() > -40.0, "it sounds: {report}");
    assert!(thirds.windows(2).all(|pair| pair[1] > pair[0] + 1.0), "each note louder than the last: {thirds:?}");
    assert!(thirds[3] > thirds[0] + 8.0, "{thirds:?}");

    // The same, falling: it is the curve that is followed, not the notes.
    let falling = json!({ "clip": clip, "controllers": { "expression": [[0, 120], [16, 25]] } });
    ok(client, "set_controllers", falling).await;
    let (diminuendo, _) = render("diminuendo.wav", json!({})).await;
    let thirds = [
        level(&diminuendo, 0.4..1.8),
        level(&diminuendo, 2.4..3.8),
        level(&diminuendo, 4.4..5.8),
        level(&diminuendo, 6.4..7.8),
    ];
    said("render, diminuendo", &json!({ "level_of_each_note_db": thirds }));
    assert!(thirds.windows(2).all(|pair| pair[1] < pair[0] - 1.0), "each note softer than the last: {thirds:?}");

    // Started in the middle of the crescendo: at the level it has there
    // from its first note, not at nothing and not at full.
    ok(client, "set_controllers", json!({ "clip": clip, "controllers": { "expression": [[0, 25], [16, 120]] } })).await;
    let (middle, report) = render("middle.wav", json!({ "start": 8 })).await;
    let (from_middle, in_whole) = (level(&middle, 0.4..1.8), level(&crescendo, 4.4..5.8));
    said(
        "render from beat 8",
        &json!({ "peak_db": report["peak_db"], "first_note_db": from_middle, "same_note_in_the_whole_db": in_whole }),
    );
    assert!((from_middle - in_whole).abs() < 3.0, "{from_middle:.1} dB against {in_whole:.1} dB in the whole render");
    assert!(from_middle > level(&crescendo, 0.4..1.8) + 4.0, "louder than the song's first note");

    // And one call from the notes: every note sounds, and the line falls
    // silent where it breathes.
    ok(client, "set_notes", json!({ "clip": clip, "notes": [["C4", 0, 1], ["E4", 1, 1], ["G4", 2, 4], ["F4", 6, 1], ["D4", 9, 1], ["E4", 10, 3]] })).await;
    let shaped = ok(client, "shape_dynamics", json!({ "clip": clip })).await;
    said("shape_dynamics", &shaped);
    let (line, report) = render("line.wav", json!({})).await;
    let (first, long, breath, answer) =
        (level(&line, 0.1..0.45), level(&line, 1.4..2.8), level(&line, 3.9..4.4), level(&line, 5.2..6.3));
    said(
        "render, shaped line",
        &json!({ "peak_db": report["peak_db"], "first_note_db": first, "long_note_db": long, "breath_db": breath, "answer_db": answer }),
    );
    assert!(first > -70.0 && answer > -70.0, "every phrase sounds: {first:.1} dB, {answer:.1} dB");
    assert!(long > first, "the long high note is the loudest: {long:.1} dB against {first:.1} dB");
    assert!(breath < long - 25.0, "it breathes between the phrases: {breath:.1} dB against {long:.1} dB");
    assert!(
        report["notes"].as_array().unwrap().iter().all(|note| !note.as_str().unwrap().contains("made no sound")),
        "{report}"
    );
}

/// With the installed Surge XT, as a CLAP plugin that takes MIDI: the pitch
/// wheel from a clip bends the note it is played under, by the two
/// semitones the wheel bends by default, and the bend is not left over
/// where the clip ends.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Surge XT (CLAP) installed; run with `cargo test -p trill-mcp --test controllers -- --include-ignored --nocapture`"]
async fn the_pitch_wheel_in_a_clip_bends_a_clap_synth() {
    let Some((fixture, folder)) = connect_with_plugins(SURGE).await else { return };
    let client = &fixture.client;
    let said = |what: &str, reply: &Value| eprintln!("\n> {what}\n{reply}");
    let track = ok(client, "create_track", json!({ "name": "Surge", "instrument": SURGE })).await;
    let track = track["track_id"].as_u64().unwrap();
    {
        let session = fixture.session.lock();
        let support = session.controller_support(trill_core::TrackId(track));
        eprintln!("\nwhat the plugin says it responds to: {support:?}");
        assert_eq!(support, trill_core::device::ControllerSupport::Any);
    }
    // One long A, then a second clip with the same note and no curve.
    let clip =
        ok(client, "create_clip", json!({ "track": track, "start": 0, "length": 8, "notes": [["A3", 0, 8]] })).await;
    let clip = clip["clip_id"].as_u64().unwrap();
    ok(client, "create_clip", json!({ "track": track, "start": 8, "length": 4, "notes": [["A3", 0, 4]] })).await;
    let render = |name: &'static str| {
        let path = folder.path().join(name);
        async move {
            let report =
                ok(client, "render", json!({ "path": path.display().to_string(), "max_tail_seconds": 1.0 })).await;
            (samples(&path), report)
        }
    };
    let window = |audio: &(Vec<f32>, f64), seconds: std::ops::Range<f64>| {
        let frame = |second: f64| (second * audio.1) as usize;
        pitch(&audio.0[frame(seconds.start)..frame(seconds.end)], audio.1)
    };
    let semitones = |hz: f64, reference: f64| 12.0 * (hz / reference).log2();

    let (plain, report) = render("plain.wav").await;
    assert!(report["peak_db"].as_f64().unwrap() > -40.0, "the synth sounds: {report}");
    let at_rest = window(&plain, 0.5..0.9);
    said("render, no wheel", &json!({ "pitch_hz": at_rest, "peak_db": report["peak_db"] }));
    assert!(semitones(at_rest, 220.0).abs() < 0.3, "an A at {at_rest:.1} Hz");

    // The wheel goes up over the third and fourth beats, stays, and is
    // still up when the clip ends.
    let bend = json!({ "clip": clip, "controllers": { "pitch_bend": [[2, 0], [4, 1]] } });
    said("set_controllers", &ok(client, "set_controllers", bend).await);
    let (bent, _) = render("bent.wav").await;
    let (before, half_way, after, next_clip) =
        (window(&bent, 0.5..0.9), window(&bent, 1.45..1.55), window(&bent, 2.5..3.5), window(&bent, 4.5..5.5));
    let heard = json!({
        "before_semitones": semitones(before, at_rest),
        "half_way_semitones": semitones(half_way, at_rest),
        "bent_semitones": semitones(after, at_rest),
        "in_the_next_clip_semitones": semitones(next_clip, at_rest),
    });
    said("render, wheel up", &heard);
    assert!(semitones(before, at_rest).abs() < 0.1, "{heard}");
    assert!((semitones(half_way, at_rest) - 1.0).abs() < 0.35, "half way up, a semitone: {heard}");
    assert!((semitones(after, at_rest) - 2.0).abs() < 0.15, "fully up, two semitones: {heard}");
    assert!(semitones(next_clip, at_rest).abs() < 0.1, "not left bent after the clip: {heard}");

    // The mod wheel reaches it too: the render is not what it was.
    ok(
        client,
        "set_controllers",
        json!({ "clip": clip, "controllers": { "pitch_bend": [], "mod_wheel": [[0, 127]] } }),
    )
    .await;
    let (wheeled, _) = render("mod.wheel.wav").await;
    let differs = plain.0.iter().zip(&wheeled.0).filter(|(a, b)| (**a - **b).abs() > 1e-4).count();
    said("render, mod wheel up", &json!({ "frames_that_differ": differs, "of": plain.0.len() }));
}
