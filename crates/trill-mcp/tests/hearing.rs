//! Hearing over MCP: an assistant finds out what an instrument does in
//! one call each (its range, its octave, its keyswitches, a drum plugin's
//! map), checks a mix without writing a file, and is told why a track is
//! silent.
//!
//! The first part needs nothing installed: its plugins are scripted
//! (`trill_engine::scripted`). The second is the proof against the real
//! thing, on a machine that has it: Superior Drummer 3, which loads its
//! kit piece by piece after it is created, and a SWAM saxophone, which
//! stays silent for seconds after it is. Those tests are skipped unless
//! asked for:
//!
//! ```sh
//! cargo test -p trill-mcp --test hearing -- --include-ignored --nocapture
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
use std::time::{Duration, Instant};
use trill_engine::SharedSession;
use trill_engine::scripted::{BASS, DRUMS, Loading, ScriptedFactory};
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Parts, Runtime, RuntimeOptions};

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

/// A server that offers the scripted instruments beside the built-in
/// devices. Nothing of the user's is read or written.
async fn connect(scripted: ScriptedFactory) -> Fixture {
    let parts = Parts { devices: vec![Box::new(scripted)], ..Parts::default() };
    serve(Arc::new(Runtime::start_custom(RuntimeOptions::hermetic(), parts).unwrap())).await
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

/// A track playing `instrument`, and its id.
async fn track(client: &Client, name: &str, instrument: &str) -> u64 {
    let made = ok(client, "create_track", json!({ "name": name, "instrument": instrument })).await;
    made["track_id"].as_u64().unwrap_or_else(|| panic!("no track id in {made}"))
}

/// A clip of `notes` (`[pitch, start, length]`) on `track`, and its id.
async fn clip(client: &Client, track: u64, start: f64, length: f64, notes: Value) -> u64 {
    let made =
        ok(client, "create_clip", json!({ "track": track, "start": start, "length": length, "notes": notes })).await;
    made["clip_id"].as_u64().unwrap()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| items.iter().map(|item| item.as_str().unwrap_or_default().to_string()).collect())
        .unwrap_or_default()
}

/// What the song is and how many steps there are to undo, to see that a
/// tool left both alone.
fn standing(fixture: &Fixture) -> (trill_core::Project, u64) {
    let session = fixture.session.lock();
    (session.project().clone(), session.revision())
}

// ---- probe ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn one_probe_gives_an_instruments_range_and_the_octave_it_sounds_at() {
    // A bass that takes a third of a second to be ready after it is made.
    let scripted = ScriptedFactory::loading(Loading { first: Duration::from_millis(300), ..Loading::default() });
    let fixture = connect(scripted).await;
    let client = &fixture.client;
    let bass = track(client, "Bass", BASS).await;
    // Whatever the plugin says its state is has been written down, as a
    // save would.
    ok(client, "measure", json!({ "end": 1 })).await;
    let before = standing(&fixture);

    let reply = ok(client, "probe", json!({ "track": bass })).await;
    assert_eq!(reply["instrument"], "Scripted Bass");
    assert_eq!((&reply["track"], &reply["through"]), (&json!("Bass"), &json!("the instrument alone")));
    assert_eq!(reply["played"], 128, "the whole keyboard");
    assert_eq!(reply["sounding"], "E1-G4 (28-67)");
    assert_eq!(reply["silent"], "C-1-D#1 (0-27), G#4-G9 (68-127)");
    assert_eq!(reply["pitch"], "sounds an octave below what is written (-12 semitones): write it an octave higher");
    // Forty notes that measure alike are one entry, not forty.
    let heard = reply["heard"].as_array().unwrap();
    assert!(heard.len() <= 3, "{heard:#?}");
    assert_eq!(heard[0]["offset_semitones"], -12.0);
    assert!(heard[0]["level_db"].is_number() || heard[0]["level_db"].is_array(), "{heard:#?}");
    assert!(reply.to_string().len() < 1_500, "a small reply: {} bytes", reply.to_string().len());

    // Nothing was changed, nothing can be undone, and no file was written.
    assert_eq!(standing(&fixture), before);
    let history = ok(client, "history", json!({ "action": "undo" })).await;
    assert_ne!(standing(&fixture).0, before.0, "the step that was undone is the track, not a probe: {history}");
    ok(client, "history", json!({ "action": "redo" })).await;
    assert!(!std::env::temp_dir().join("trill-renders").join("Untitled").exists());

    // An instrument that is not in the song, with a preset: tried before
    // it is added.
    let up = ok(client, "probe", json!({ "device": BASS, "preset": "Octave up", "notes": ["E2", "E3", "G3"] })).await;
    assert_eq!((&up["pitch"], &up["sounding"]), (&json!("sounds as written"), &json!("E2 (40), E3 (52), G3 (55)")));
    assert!(up.get("track").is_none() && up.get("silent").is_none(), "{up}");
    assert_eq!(standing(&fixture).0, before.0, "and still nothing was added to the song");

    // Asked for in detail, every note is listed, with what was heard.
    let detailed = ok(client, "probe", json!({ "track": bass, "notes": ["D1-F1"], "detail": true })).await;
    let notes = detailed["heard"].as_array().unwrap();
    assert_eq!(notes.len(), 4);
    assert_eq!((&notes[0]["notes"], &notes[0]["silent"]), (&json!("D1 (26)"), &json!(true)));
    assert_eq!((&notes[2]["notes"], &notes[2]["heard"]), (&json!("E1 (28)"), &json!("E0")));
    assert!(notes[2].get("onset_ms").is_none() && notes[2]["bright_hz"].as_f64().unwrap() > 20.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_probe_tells_which_keys_switch_the_articulation_and_how() {
    let fixture = connect(ScriptedFactory::new()).await;
    let client = &fixture.client;
    let bass = track(client, "Bass", BASS).await;
    // An octave of candidates under one note the instrument plays.
    let reply = ok(client, "probe", json!({ "track": bass, "notes": ["E2"], "keyswitches": ["C0-B0"] })).await;
    let found = reply["keyswitches"].as_array().unwrap();
    let keys: Vec<&str> = found.iter().map(|entry| entry["keys"].as_str().unwrap()).collect();
    assert_eq!(keys, ["none", "C0 (12)", "C#0-B0 (13-23)"], "{reply:#}");
    // The one that switches: what it changes, in words and in numbers.
    let mute = &found[1];
    let changes = mute["changes"].as_str().unwrap();
    assert!(changes.contains("shorter") && changes.contains("darker"), "{changes}");
    assert_eq!(mute["latches"], false, "it holds only while its key is down");
    let (plain, muted) = (&found[0]["heard"][0], &mute["heard"][0]);
    assert!(muted["rings_s"].as_f64().unwrap() < plain["rings_s"].as_f64().unwrap() / 2.0, "{plain} then {muted}");
    assert!(muted["bright_hz"].as_f64().unwrap() < plain["bright_hz"].as_f64().unwrap());
    assert_eq!(found[2]["changes"], "nothing");

    // A candidate that is a note of the instrument is told apart: it
    // sounds, whatever else it does to the note played over it.
    let reply = ok(client, "probe", json!({ "track": bass, "notes": ["E2"], "keyswitches": ["A2", 12] })).await;
    let found = reply["keyswitches"].as_array().unwrap();
    let note = found.iter().find(|entry| entry["keys"] == "A2 (45)").unwrap_or_else(|| panic!("{reply:#}"));
    let itself = note["sounds_itself"] == true || note["changes"].as_str().unwrap().contains("notes themselves");
    assert!(itself, "{note}");
    let mute = found.iter().find(|entry| entry["keys"] == "C0 (12)").unwrap();
    assert!(mute.get("sounds_itself").is_none() && mute["changes"].as_str().unwrap().contains("shorter"), "{mute}");

    // A key that is already known is held through a plain probe.
    let held = ok(client, "probe", json!({ "track": bass, "notes": ["E2"], "hold": ["C0"], "length": 0.8 })).await;
    assert!(held["heard"][0]["rings_s"].as_f64().unwrap() < 0.4, "{held}");

    // What cannot be done says what to do.
    let error = call(client, "probe", json!({ "track": bass, "keyswitches": ["C0-B0"] })).await.unwrap_err();
    assert!(error.contains("`keyswitches` needs `notes`"), "{error}");
    let error = call(client, "probe", json!({ "notes": ["E2"] })).await.unwrap_err();
    assert!(error.contains("give `track`") && error.contains("`device`"), "{error}");
    let error = call(client, "probe", json!({ "track": 999 })).await.unwrap_err();
    assert!(error.contains("Existing tracks") && error.contains("Bass"), "{error}");
    let error = call(client, "probe", json!({ "device": "builtin:reverb" })).await.unwrap_err();
    assert!(error.contains("is an effect"), "{error}");
    let error = call(client, "probe", json!({ "track": bass, "notes": ["loud"] })).await.unwrap_err();
    assert!(error.contains("'loud' is not a pitch"), "{error}");
    let error =
        call(client, "probe", json!({ "track": bass, "controllers": { "expression": 300 } })).await.unwrap_err();
    assert!(error.contains("runs from 0 to 127"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_probe_maps_a_drum_plugin() {
    let fixture = connect(ScriptedFactory::new()).await;
    let client = &fixture.client;
    let reply = ok(client, "probe", json!({ "device": DRUMS })).await;
    assert_eq!(reply["sounding"], "C2 (36), D2 (38), E2 (40), F#2 (42), C#3 (49)");
    assert!(reply.get("pitch").is_none(), "a kit has no octave: {reply}");
    let heard = reply["heard"].as_array().unwrap();
    let what: Vec<(&str, &str)> =
        heard.iter().map(|entry| (entry["notes"].as_str().unwrap(), entry["like"].as_str().unwrap())).collect();
    assert_eq!(
        what,
        [
            ("C2 (36)", "a low thump"),
            ("D2 (38)", "a noisy mid hit"),
            ("E2 (40)", "a noisy mid hit"),
            ("F#2 (42)", "a short bright tick"),
            ("C#3 (49)", "a long bright wash"),
        ]
    );
    // The crash rings longer than a quick look at 128 notes listens for.
    assert!(heard[4]["rings_s"].as_str().is_some_and(|rings| rings.starts_with('>')), "{}", heard[4]);
    assert_eq!(heard[0]["drum_name"], "kick");
    // Asked for by itself it is heard out.
    let crash = ok(client, "probe", json!({ "device": DRUMS, "notes": ["crash"], "length": 0.1, "tail": 4 })).await;
    let rings = crash["heard"][0]["rings_s"].as_f64().unwrap();
    assert!(rings > 1.0, "a number, now that it was heard out: {crash}");

    // Harder is louder: three velocities of one drum.
    let levels = ok(client, "probe", json!({ "device": DRUMS, "notes": [36], "velocities": [30, 70, 127] })).await;
    let heard = levels["heard"].as_array().unwrap();
    let level = |index: usize| heard[index]["level_db"].as_f64().unwrap();
    assert_eq!(heard.len(), 3);
    assert!(level(0) < level(1) - 5.0 && level(1) < level(2) - 4.0, "{levels}");
    assert_eq!((&heard[0]["velocity"], &heard[2]["velocity"]), (&json!(30), &json!(127)));

    // More than a call plays: the first part, and what to ask for next.
    let many = ok(client, "probe", json!({ "device": DRUMS, "notes": ["0-127"], "velocities": [60, 120] })).await;
    assert_eq!(many["played"], 128);
    let notes = strings(&many["notes"]);
    assert!(notes[0].contains("left out E4-G9 (64-127)"), "{notes:?}");
}

// ---- measure and render --------------------------------------------------------------

/// A song in two parts with markers: a bass line and a kick in the first,
/// the bass alone and softer in the second, and a compressor on the kick.
async fn two_part_song(client: &Client) -> (u64, u64, u64) {
    let bass = track(client, "Bass", BASS).await;
    let drums = track(client, "Drums", DRUMS).await;
    let mut line = Vec::new();
    for beat in 0..16 {
        line.push(json!([40 + (beat % 4) * 3, beat, 1, if beat < 8 { 120 } else { 40 }]));
    }
    clip(client, bass, 0.0, 16.0, json!(line)).await;
    let kicks: Vec<Value> = (0..8).map(|beat| json!(["kick", beat, 0.25])).collect();
    clip(client, drums, 0.0, 8.0, json!(kicks)).await;
    let markers = json!([{ "beat": 0, "name": "Verse" }, { "beat": 8, "name": "Outro" }]);
    ok(client, "set_markers", json!({ "markers": markers })).await;
    let added = ok(client, "add_device", json!({ "track": drums, "uri": "builtin:compressor" })).await;
    let compressor = added["device_id"].as_u64().unwrap_or_else(|| panic!("{added}"));
    (bass, drums, compressor)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_compressor_is_given_another_track_to_listen_to() {
    let fixture = connect(ScriptedFactory::new()).await;
    let client = &fixture.client;
    // A bass that plays for sixteen beats, and a kick on each of the first
    // eight.
    let (bass, drums, _) = two_part_song(client).await;
    let reduction = |report: &Value, device: u64| -> (f64, f64) {
        let entries = report["gain_reduction"].as_array().unwrap();
        let entry = entries.iter().find(|entry| entry["device"] == device).unwrap_or_else(|| panic!("{report}"));
        (entry["most_db"].as_f64().unwrap(), entry["active"].as_f64().unwrap())
    };

    // A compressor on the bass that listens to the drums: said when it is
    // added, and by everything that shows the device afterwards.
    let squeeze = json!({ "threshold": -30, "ratio": 10, "attack": 1, "release": 40, "knee": 0 });
    let asked = json!({ "track": bass, "uri": "builtin:compressor", "sidechain": drums, "params": squeeze });
    let added = ok(client, "add_device", asked).await;
    let duck = added["device_id"].as_u64().unwrap_or_else(|| panic!("{added}"));
    assert_eq!(added["sidechain"], json!({ "track": drums, "name": "Drums" }));
    let shown = ok(client, "get_device", json!({ "device": duck })).await;
    assert_eq!(shown["sidechain"], json!({ "track": drums, "name": "Drums" }));
    let project = ok(client, "get_project", json!({})).await;
    assert!(project.to_string().contains(&format!("\"sidechain\":{drums}")), "{project}");

    // Hearing the bass alone, the drums are still played for it: the bass
    // is turned down on the kicks, which are in the first half only.
    let ducked = ok(client, "measure", json!({ "tracks": [bass] })).await;
    let (most, active) = reduction(&ducked, duck);
    assert!(most > 6.0 && active > 0.02 && active < 0.5, "takes off {most} dB at most, {active} of the time");
    assert_eq!(ducked["tracks"].as_array().unwrap().len(), 1, "and only the bass is heard: {ducked}");

    // Back to its own input, which is over the threshold whenever the bass
    // plays: it works nearly all the time.
    let own = ok(client, "update_device", json!({ "device": duck, "sidechain": 0 })).await;
    assert_eq!(own["sidechain"], Value::Null, "{own}");
    let shown = ok(client, "get_device", json!({ "device": duck })).await;
    assert!(shown.get("sidechain").is_none(), "{shown}");
    assert!(shown["sidechain_hint"].as_str().unwrap().contains("update_device"), "{shown}");
    let (_, steady) = reduction(&ok(client, "measure", json!({ "tracks": [bass] })).await, duck);
    assert!(steady > active + 0.3, "{steady} of the time on its own input, {active} on the kick");

    // A key that stays under the threshold is named in the notes.
    ok(client, "update_device", json!({ "device": duck, "sidechain": drums, "params": { "threshold": 0 } })).await;
    let idle = ok(client, "measure", json!({ "tracks": [bass] })).await;
    let notes = strings(&idle["notes"]);
    assert!(notes.iter().any(|note| note.contains("listens to 'Drums'")), "{notes:?}");

    // What cannot be: a device that takes no key, a track listening to
    // itself, a track there is none of. Each says what to do instead.
    let reverb = ok(client, "add_device", json!({ "track": bass, "uri": "builtin:reverb" })).await["device_id"].clone();
    let error = call(client, "update_device", json!({ "device": reverb, "sidechain": drums })).await.unwrap_err();
    assert!(error.contains("compressor"), "{error}");
    let error = call(client, "update_device", json!({ "device": duck, "sidechain": bass })).await.unwrap_err();
    assert!(error.contains("itself"), "{error}");
    let error = call(client, "update_device", json!({ "device": duck, "sidechain": 999 })).await.unwrap_err();
    assert!(error.contains("Existing tracks") && error.contains("Drums"), "{error}");
    // And a device that cannot be given its sidechain is not added at all.
    let before = fixture.session.lock().project().clone();
    let asked = json!({ "track": bass, "uri": "builtin:compressor", "sidechain": bass, "params": { "ratio": 2 } });
    let error = call(client, "add_device", asked).await.unwrap_err();
    assert!(error.contains("itself") && error.contains("not added"), "{error}");
    assert_eq!(fixture.session.lock().project().tracks, before.tracks);

    // What a model reads about the devices it mixes with: the choices are
    // there by name, and the new devices are listed.
    let compressor = ok(client, "describe_device", json!({ "uri": "builtin:compressor" })).await;
    let param = |device: &Value, id: &str| -> Value {
        let params = device["params"].as_array().unwrap();
        params.iter().find(|param| param["id"] == id).unwrap_or_else(|| panic!("no {id} in {device}")).clone()
    };
    assert_eq!(param(&compressor, "detector")["choices"], json!(["peak", "bus"]));
    assert_eq!(param(&compressor, "auto_makeup")["type"], "bool");
    assert!(compressor["description"].as_str().unwrap().contains("sidechain"));
    let eq = ok(client, "describe_device", json!({ "uri": "builtin:eq" })).await;
    assert_eq!(param(&eq, "low_cut_slope")["choices"], json!(["12 dB", "24 dB", "48 dB"]));
    assert_eq!(param(&eq, "mid1_freq")["min"], 20.0);
    let set = json!({ "device": duck, "params": { "detector": "bus", "auto_release": true } });
    assert_eq!(ok(client, "update_device", set).await["set"], json!({ "auto_release": 1.0, "detector": 1.0 }));
    let effects = ok(client, "list_devices", json!({ "category": "effect" })).await;
    let listed: Vec<&str> = effects["devices"].as_array().unwrap().iter().filter_map(|d| d["uri"].as_str()).collect();
    assert!(listed.contains(&"builtin:tremolo"), "{listed:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn measure_and_render_use_the_machine_and_what_they_give_is_what_one_processor_gives() {
    let fixture = connect(ScriptedFactory::new()).await;
    let client = &fixture.client;
    two_part_song(client).await;
    let all = ok(client, "measure", json!({})).await;
    let one = ok(client, "measure", json!({ "threads": 1 })).await;
    assert_eq!(one["threads"], 1, "{one}");
    if trill_engine::render::usual_threads() > 1 {
        assert_eq!(all["threads"], 2, "the bass and the drums at once: {all}");
    }
    for said in ["loudness_lufs", "peak_db", "true_peak_db", "duration_seconds", "tracks", "gain_reduction", "notes"] {
        assert_eq!(all[said], one[said], "{said}");
    }

    // And the same files, the mix and every part of it.
    let folder = tempfile::tempdir().unwrap();
    let path = |name: &str| folder.path().join(name).display().to_string();
    let all = ok(client, "render", json!({ "path": path("all.wav"), "stems": ["all"] })).await;
    let one = ok(client, "render", json!({ "path": path("one.wav"), "stems": ["all"], "threads": 1 })).await;
    assert!(std::fs::read(path("all.wav")).unwrap() == std::fs::read(path("one.wav")).unwrap());
    let stems = |report: &Value| -> Vec<String> {
        report["stems"].as_array().unwrap().iter().map(|stem| stem["file"].as_str().unwrap().to_string()).collect()
    };
    assert!(stems(&all).len() >= 4, "{all}");
    for (made, single) in stems(&all).iter().zip(stems(&one)) {
        assert!(std::fs::read(made).unwrap() == std::fs::read(&single).unwrap(), "{made} is not {single}");
    }

    // No processors is not a number of them.
    let refused = call(client, "measure", json!({ "threads": 0 })).await.unwrap_err();
    assert!(refused.contains("leave it out"), "{refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_is_not_run_beside_others_keeps_a_measure_on_one_processor_and_the_reply_says_so() {
    let alone = trill_engine::scripted::Loading { alone: true, ..Default::default() };
    let fixture = connect(ScriptedFactory::loading(alone)).await;
    let client = &fixture.client;
    two_part_song(client).await;
    let report = ok(client, "measure", json!({})).await;
    assert_eq!(report["threads"], 1, "{report}");
    let said: Vec<String> =
        strings(&report["notes"]).into_iter().filter(|note| note.contains("one processor")).collect();
    assert_eq!(said.len(), 1, "{report}");
    assert!(said[0].contains("on 'Bass'") && said[0].contains("more like it"), "{}", said[0]);
    // Asked for one, there is nothing to explain.
    let asked = ok(client, "measure", json!({ "threads": 1 })).await;
    assert!(!strings(&asked["notes"]).iter().any(|note| note.contains("one processor")), "{asked}");
}

#[tokio::test(flavor = "multi_thread")]
async fn measuring_checks_a_mix_without_writing_a_file() {
    let fixture = connect(ScriptedFactory::new()).await;
    let client = &fixture.client;
    let (bass, drums, compressor) = two_part_song(client).await;
    let folder = tempfile::tempdir().unwrap();
    let project = folder.path().join("song.trill");
    ok(client, "save_project", json!({ "path": project.display().to_string() })).await;
    let before = standing(&fixture);

    let report = ok(client, "measure", json!({})).await;
    assert!(report.get("path").is_none(), "{report}");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1, "only the project is in its folder");
    assert_eq!(standing(&fixture), before, "and the song is as it was");

    // The mix, with its true peak.
    assert!((7.9..9.5).contains(&report["duration_seconds"].as_f64().unwrap()), "16 beats at 120: {report}");
    let (peak, true_peak) = (report["peak_db"].as_f64().unwrap(), report["true_peak_db"].as_f64().unwrap());
    assert!(true_peak >= peak && true_peak < peak + 3.0, "{peak} and {true_peak}");
    assert!(report["loudness_lufs"].as_f64().unwrap() > -40.0);
    // Every track in two numbers.
    let tracks = report["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);
    for entry in tracks {
        assert!(entry["peak_db"].as_f64().unwrap() > -40.0 && entry["loudness_lufs"].as_f64().unwrap() > -60.0);
        assert!(entry.get("spectrum").is_none(), "detail is not sent unasked: {entry}");
    }
    // Every part of the song between its markers.
    let sections = report["sections"].as_array().unwrap();
    let names: Vec<&str> = sections.iter().map(|section| section["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Verse", "Outro"]);
    assert_eq!((&sections[1]["start"], &sections[1]["end"]), (&json!(8.0), &json!(16.0)));
    let loudness = |index: usize| sections[index]["loudness_lufs"].as_f64().unwrap();
    assert!(loudness(0) > loudness(1) + 3.0, "the verse is the loud part: {sections:?}");
    // What the compressor took off: at its default threshold of -18 dB a
    // kick that peaks around -8 is turned down, and the number says by
    // how much.
    let reduction = &report["gain_reduction"][0];
    assert_eq!((&reduction["device"], &reduction["on"]), (&json!(compressor), &json!("Drums")));
    let most = reduction["most_db"].as_f64().unwrap();
    assert!(most > 2.0 && most < 12.0 && reduction["typical_db"].as_f64().unwrap() <= most, "{reduction}");
    assert!((0.0..1.0).contains(&reduction["active"].as_f64().unwrap()));

    // A threshold set from that number: far above the kick, the compressor
    // does nothing, and the report says so in words.
    ok(client, "update_device", json!({ "device": compressor, "params": { "threshold": 0 } })).await;
    let idle = ok(client, "measure", json!({ "sections": [] })).await;
    assert_eq!(idle["gain_reduction"][0]["most_db"], 0.0);
    assert!(idle.get("sections").is_none(), "none were asked for");
    let notes = strings(&idle["notes"]);
    assert!(notes.iter().any(|note| note.contains("never reached its threshold")), "{notes:?}");

    // Only some tracks, a stretch of the song, parts given as bars, and
    // the detail: the bass alone over its second half.
    let asked = json!({
        "tracks": [bass], "start": 8, "detail": true,
        "sections": [{ "bars": [3, 3] }, { "name": "Last bar", "start": 12, "end": 16 }],
    });
    let alone = ok(client, "measure", asked).await;
    let tracks = alone["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 1, "only the track that was asked for: {alone}");
    assert_eq!(tracks[0]["track"], bass);
    assert!(tracks[0]["spectrum"]["bass_db"].as_f64().unwrap() > -6.0, "an octave below E2 to A2: {alone}");
    assert_eq!(tracks[0]["section_loudness"].as_array().unwrap().len(), 2);
    assert_eq!(alone["sections"][0]["name"], "bars 3-3");
    assert!((3.9..5.5).contains(&alone["duration_seconds"].as_f64().unwrap()));
    // Mute and solo in the song were not touched to hear it.
    let song = ok(client, "get_project", json!({})).await;
    assert!(song["tracks"].as_array().unwrap().iter().all(|track| track["solo"] != true && track["mute"] != true));
    let _ = drums;

    let error = call(client, "measure", json!({ "tracks": [999] })).await.unwrap_err();
    assert!(error.contains("Existing tracks"), "{error}");
    let error = call(client, "measure", json!({ "sections": [{ "start": 4 }] })).await.unwrap_err();
    assert!(error.contains("`start` and `end`"), "{error}");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1, "still nothing but the project");

    // A render writes the file, where renders go, and measures the same.
    let rendered = ok(client, "render", json!({})).await;
    let path = rendered["path"].as_str().unwrap();
    assert!(Path::new(path).starts_with(folder.path().join("renders")), "{path}");
    assert!(std::fs::metadata(path).unwrap().len() > 100_000);
    assert_eq!(rendered["loudness_lufs"], idle["loudness_lufs"]);
    assert_eq!(rendered["tracks"], idle["tracks"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_render_writes_the_mix_and_every_part_of_the_song_to_files_of_their_own() {
    // The scripted instruments take a second and a half to load.
    let slow = Loading { first: Duration::from_millis(1500), ..Loading::default() };
    let fixture = connect(ScriptedFactory::loading(slow)).await;
    let client = &fixture.client;
    let bass = track(client, "Bass", BASS).await;
    clip(client, bass, 0.0, 8.0, json!([["E2", 0, 2], ["G2", 2, 2], ["A2", 4, 4]])).await;
    let keys = track(client, "Keys", "builtin:polysynth").await;
    clip(client, keys, 0.0, 8.0, json!([["C4", 0, 4], ["E4", 4, 4]])).await;
    let made = ok(client, "create_track", json!({ "name": "Room", "kind": "return" })).await;
    let room = made["track_id"].as_u64().unwrap();
    ok(client, "add_device", json!({ "track": room, "uri": "builtin:reverb" })).await;
    ok(client, "update_track", json!({ "track": keys, "send": { "to": room, "level_db": -6 } })).await;
    let folder = tempfile::tempdir().unwrap();
    let mix = folder.path().join("song.wav");

    // The mix alone, as ever: one file.
    let plain = ok(client, "render", json!({ "path": mix.display().to_string(), "max_tail_seconds": 2 })).await;
    assert!(plain.get("stems").is_none(), "{plain}");
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 1);
    let before = standing(&fixture);

    // Asked for all of them: one pass, and the reply says what is where.
    let arguments = json!({ "path": mix.display().to_string(), "max_tail_seconds": 2, "stems": ["all"] });
    let report = ok(client, "render", arguments).await;
    assert_eq!(standing(&fixture), before, "the song is as it was");
    let stems = report["stems"].as_array().unwrap_or_else(|| panic!("{report}"));
    let listed: Vec<(String, u64)> = stems
        .iter()
        .map(|stem| (stem["what"].as_str().unwrap().to_string(), stem["track"].as_u64().unwrap_or_default()))
        .collect();
    let expected = [
        ("track", bass),
        ("instrument", bass),
        ("track", keys),
        ("instrument", keys),
        ("return", room),
        ("premaster", 0),
    ];
    assert_eq!(listed, expected.map(|(what, track)| (what.to_string(), track)), "{report}");
    let mix_frames = trill_media::decode(&mix).unwrap().left.len();
    for stem in stems {
        let file = Path::new(stem["file"].as_str().unwrap());
        assert_eq!(file.parent().unwrap(), folder.path().join("song stems"), "beside the mix: {stem}");
        let audio = trill_media::decode(file).unwrap();
        assert_eq!(audio.left.len(), mix_frames, "as long as the mix: {stem}");
        let peak = audio.left.iter().chain(&audio.right).fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        let said = stem["peak_db"].as_f64().unwrap();
        assert!((f64::from(20.0 * peak.log10()) - said).abs() < 0.1, "{stem}");
        assert!(stem["loudness_lufs"].as_f64().unwrap() > -60.0 && stem.get("true_peak_db").is_some(), "{stem}");
        assert!(stem.get("spectrum").is_none(), "detail is not sent unasked: {stem}");
    }
    // A track's file is what the track's own row says of it.
    let row = report["tracks"].as_array().unwrap().iter().find(|row| row["track"] == keys).unwrap();
    assert_eq!(stems[2]["peak_db"], row["peak_db"], "{report}");
    assert_eq!(stems[2]["name"], "Keys");

    // Only some kinds, and with `detail` each in full.
    let arguments =
        json!({ "path": mix.display().to_string(), "max_tail_seconds": 2, "stems": ["returns"], "detail": true });
    let report = ok(client, "render", arguments).await;
    let stems = report["stems"].as_array().unwrap();
    assert_eq!(stems.len(), 1, "{report}");
    assert!(stems[0]["spectrum"].is_object() && stems[0]["what"] == "return", "{report}");
    // `measure` writes nothing, and takes no stems.
    let error = call(client, "measure", json!({ "stems": ["all"] })).await;
    assert!(error.is_err() || error.unwrap().get("stems").is_none());

    // Hearing one track makes that track only: the synth alone waits for
    // no plugin, however many the song has.
    let begun = Instant::now();
    let only = ok(client, "measure", json!({ "tracks": [keys] })).await;
    let notes = strings(&only["notes"]);
    assert!(notes.iter().all(|note| !note.contains("Waited")), "{notes:?} after {:?}", begun.elapsed());
    let rows: Vec<u64> = only["tracks"].as_array().unwrap().iter().map(|row| row["track"].as_u64().unwrap()).collect();
    assert_eq!(rows, [keys]);
    // The bass alone waits for the bass.
    let only = ok(client, "measure", json!({ "tracks": [bass] })).await;
    assert!(only["tracks"][0]["peak_db"].as_f64().unwrap() > -40.0, "{only}");

    // The return alone is the return as the song feeds it, not silence:
    // what its stem of the whole song holds. The keys, which feed it, are
    // not heard themselves and have no row.
    let only = ok(client, "measure", json!({ "tracks": [room], "max_tail_seconds": 2 })).await;
    let rows: Vec<u64> = only["tracks"].as_array().unwrap().iter().map(|row| row["track"].as_u64().unwrap()).collect();
    assert_eq!(rows, [room], "{only}");
    let (alone, in_song) = (only["loudness_lufs"].as_f64().unwrap(), stems[0]["loudness_lufs"].as_f64().unwrap());
    assert!(alone > -60.0 && (alone - in_song).abs() < 0.1, "{alone} LUFS alone, {in_song} in the song: {only}");
    assert!(strings(&only["notes"]).iter().all(|note| !note.contains("silent")), "{only}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_render_runs_at_the_rate_it_is_asked_for_and_every_reply_says_which() {
    let fixture = connect(ScriptedFactory::new()).await;
    let client = &fixture.client;
    let bass = track(client, "Bass", BASS).await;
    clip(client, bass, 0.0, 8.0, json!([["E2", 0, 2], ["G2", 2, 2], ["A2", 4, 4]])).await;
    let keys = track(client, "Keys", "builtin:polysynth").await;
    clip(client, keys, 0.0, 8.0, json!([["C4", 0, 4], ["E4", 4, 4]])).await;

    // Unasked, it is the engine's rate, and the reply says so.
    let usual = ok(client, "measure", json!({ "max_tail_seconds": 1 })).await;
    assert_eq!(usual["sample_rate"], 48_000, "{usual}");
    // Asked for another, it is the same song at that rate: as long, and
    // as loud.
    let asked = ok(client, "measure", json!({ "max_tail_seconds": 1, "sample_rate": 44_100 })).await;
    assert_eq!(asked["sample_rate"], 44_100, "{asked}");
    let number = |reply: &Value, name: &str| reply[name].as_f64().unwrap();
    assert!((number(&asked, "duration_seconds") - number(&usual, "duration_seconds")).abs() < 0.05, "{asked}");
    assert!((number(&asked, "loudness_lufs") - number(&usual, "loudness_lufs")).abs() < 0.2, "{asked} {usual}");
    assert_eq!(fixture.session.lock().audio_info().sample_rate as u32, 48_000, "the engine runs as it did");

    // The file, and every stem beside it, is at the rate that was asked.
    let folder = tempfile::tempdir().unwrap();
    let mix = folder.path().join("song.wav");
    let arguments =
        json!({ "path": mix.display().to_string(), "max_tail_seconds": 1, "sample_rate": 96_000, "stems": ["tracks"] });
    let report = ok(client, "render", arguments).await;
    assert_eq!(report["sample_rate"], 96_000);
    assert_eq!(trill_media::decode(&mix).unwrap().sample_rate, 96_000.0);
    let stems = report["stems"].as_array().unwrap();
    assert_eq!(stems.len(), 2, "{report}");
    for stem in stems {
        let audio = trill_media::decode(Path::new(stem["file"].as_str().unwrap())).unwrap();
        assert_eq!(audio.sample_rate, 96_000.0, "{stem}");
    }
    // A probe says its rate as well.
    let probed = ok(client, "probe", json!({ "track": bass, "notes": ["E2"] })).await;
    assert_eq!(probed["sample_rate"], 48_000, "{probed}");

    // A rate that is none: a sentence that names the ones there are.
    let error = call(client, "measure", json!({ "sample_rate": 12 })).await.unwrap_err();
    assert!(error.contains("from 8000 to 384000 Hz") && error.contains("48000"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_track_is_told_what_its_notes_say_and_sent_to_the_probe() {
    let fixture = connect(ScriptedFactory::new()).await;
    let client = &fixture.client;
    // A bass line written an octave too low for the instrument, and a
    // second track that holds only a keyswitch.
    let low = track(client, "Low", BASS).await;
    clip(client, low, 0.0, 4.0, json!([["E0", 0, 1], ["G0", 1, 1], ["D1", 2, 1]])).await;
    let switch = track(client, "Switch", BASS).await;
    clip(client, switch, 0.0, 4.0, json!([["C0", 0, 4]])).await;
    let fine = track(client, "Fine", BASS).await;
    clip(client, fine, 0.0, 4.0, json!([["E2", 0, 1]])).await;
    // And a kit that plays its kick, and a key that is no drum of it.
    let half = track(client, "Half", DRUMS).await;
    clip(client, half, 0.0, 4.0, json!([["C2", 0, 1], ["A2", 1, 1]])).await;

    // Two instruments that never answer any of their notes: a mix
    // without them is not the song. Nothing is measured, no file is
    // written, and the refusal says of each what its notes say and what
    // to do about it.
    let folder = tempfile::tempdir().unwrap();
    let mix = folder.path().join("song.wav");
    let begun = Instant::now();
    let refused = call(client, "measure", json!({})).await.unwrap_err();
    let first = begun.elapsed();
    assert!(refused.starts_with("Nothing was rendered: 2 of the song's instruments never answered"), "{refused}");
    let lines: Vec<&str> = refused.lines().collect();
    let about = |name: &str| lines.iter().find(|line| line.starts_with(&format!("- '{name}'"))).copied();
    let low_note = about("Low").unwrap_or_else(|| panic!("{refused}"));
    assert!(low_note.contains("run from E0 to D1") && low_note.contains("outside its range"), "{low_note}");
    assert!(low_note.contains(&format!("probe with track {low}")), "{low_note}");
    let switch_note = about("Switch").unwrap();
    assert!(switch_note.contains("keyswitches") && !switch_note.contains("expression"), "{switch_note}");
    assert!(about("Fine").is_none() && about("Half").is_none(), "{refused}");
    // A key that plays nothing sounds like a drum that has not been loaded:
    // the kit, which plays its kick, was held to its other key before the
    // refusal, and that is said with it, since it is found once.
    let half_note = lines.iter().find(|line| line.contains("on 'Half'")).unwrap_or_else(|| panic!("{refused}"));
    assert!(half_note.starts_with("Besides: Waited"), "{half_note}");
    assert!(half_note.contains("1 of the notes it plays made no sound the whole time: A2 (45)"), "{half_note}");
    assert!(half_note.contains("still loading") && half_note.contains(&format!("`probe` with track {half}")));
    // Neither leads with breath or a bow.
    assert!(!refused.contains("saxophone"), "{refused}");
    assert!(refused.contains("ask again") && refused.contains("mute it (update_track)"), "{refused}");
    let rendered = call(client, "render", json!({ "path": mix.display().to_string(), "stems": ["all"] })).await;
    assert!(rendered.unwrap_err().starts_with("Nothing was rendered"));
    assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0, "no file, and no folder of stems");
    // What never answered was waited for once: asked again, it costs a
    // moment.
    let begun = Instant::now();
    call(client, "measure", json!({})).await.unwrap_err();
    assert!(begun.elapsed() < first, "{:?} after {first:?}", begun.elapsed());

    // Muted, they are left out on purpose, and the rest is measured. What
    // the kit was held to once is known of it now, and not said twice.
    for silent in [low, switch] {
        ok(client, "update_track", json!({ "track": silent, "mute": true })).await;
    }
    let report = ok(client, "measure", json!({})).await;
    let notes = strings(&report["notes"]);
    assert!(notes.iter().all(|note| !note.contains("made no sound")), "{notes:#?}");
    let rows: Vec<u64> =
        report["tracks"].as_array().unwrap().iter().map(|row| row["track"].as_u64().unwrap()).collect();
    assert_eq!(rows, [fine, half], "{report}");

    // And the probe the note asks for says what the instrument plays.
    let probed = ok(client, "probe", json!({ "track": low })).await;
    assert_eq!(probed["sounding"], "E1-G4 (28-67)");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_was_just_made_is_marked_as_loading_where_a_model_looks() {
    let fixture = connect(ScriptedFactory::new()).await;
    let client = &fixture.client;
    let bass = track(client, "Bass", BASS).await;
    let keys = track(client, "Keys", "builtin:polysynth").await;
    // The plugin may be reading what it plays; the built-in synth is ready
    // when it exists.
    let project = ok(client, "get_project", json!({})).await;
    let devices = |id: u64| {
        let tracks = project["tracks"].as_array().unwrap();
        tracks.iter().find(|track| track["id"] == id).unwrap()["devices"].clone()
    };
    assert_eq!(devices(bass)[0]["loading"], true, "{project}");
    assert!(devices(keys)[0].get("loading").is_none(), "{project}");
    let device = devices(bass)[0]["id"].as_u64().unwrap();
    let said = project["loading"].as_str().unwrap_or_else(|| panic!("{project}"));
    assert!(said.contains(&format!("Scripted Bass (device {device})")) && said.contains("may be silent"), "{said}");
    assert!(said.contains("probe, measure and render wait"), "{said}");
    assert_eq!(ok(client, "get_device", json!({ "device": device })).await["loading"], true);
    // Pressing play now is answered with the same warning.
    let playing = ok(client, "transport", json!({ "action": "play" })).await;
    assert!(playing["loading"].as_str().is_some_and(|said| said.contains("Scripted Bass")), "{playing}");
    ok(client, "transport", json!({ "action": "stop" })).await;
    // A device that is gone is not loading.
    ok(client, "delete_device", json!({ "device": device })).await;
    assert!(ok(client, "get_project", json!({})).await.get("loading").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tools_and_the_instructions_say_how_to_hear() {
    let fixture = connect(ScriptedFactory::new()).await;
    let tools = fixture.client.list_all_tools().await.unwrap();
    let described = |name: &str| {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap_or_else(|| panic!("no tool {name}"));
        (tool.description.clone().unwrap_or_default().to_string(), serde_json::to_string(&tool.input_schema).unwrap())
    };
    let (probe, schema) = described("probe");
    assert!(probe.contains("Nothing in the song changes") && probe.contains("whole keyboard"), "{probe}");
    for word in ["keyswitches", "velocities", "controllers", "effects", "preset", "E1-G4"] {
        assert!(schema.contains(word), "{word}: {schema}");
    }
    let (measure, schema) = described("measure");
    assert!(measure.contains("WITHOUT writing a file"), "{measure}");
    for word in ["tracks", "sections", "bars", "detail", "threads"] {
        assert!(schema.contains(word), "{word}: {schema}");
    }
    // The tool that writes a file says which one to use for checking.
    let (render, schema) = described("render");
    assert!(render.contains("use `measure`"), "{render}");
    assert!(schema.contains("path") && schema.contains("tracks"), "{schema}");

    let instructions = fixture.client.peer_info().unwrap().instructions.clone().unwrap_or_default();
    let words = [
        "probe",
        "measure",
        "ONCE for every plugin",
        "gain reduction",
        "never to check your work",
        "mark it `loading`",
        "several of the machine's processors",
    ];
    for word in words {
        assert!(instructions.contains(word), "the instructions should say '{word}'");
    }
    assert!(!instructions.contains("render writes a WAV and reports"), "the old advice to check by rendering is gone");
}

// ---- the real thing ------------------------------------------------------------------

/// Superior Drummer 3, as a VST3 plugin: it loads its kit piece by piece
/// after it is created, and is silent until it has.
const SD3: &str = "vst3:565354534433617375706572696F7220";
/// SWAM Alto Sax 3, as a VST3 plugin: silent until it is given
/// expression, and for some seconds after it is created whatever it is
/// given.
const SWAM_SAX: &str = "vst3:565354575361337377616D20616C746F";
/// Ample Metal R, a sampled bass guitar with keyswitches, as a VST3 plugin.
const AMPLE_BASS: &str = "vst3:ABCDEF019182FAEB41707364416D7274";

/// The plugins installed on this machine, found with caches of the test's
/// own; `None` if `uri` is not among them.
fn installed(uri: &str) -> Option<trill_plugins::PluginHost> {
    use trill_core::device::DeviceFactory;
    use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-hearing");
    let clap =
        ClapConfig { cache_path: Some(scratch.join("clap.json")), scanner: Scanner::Auto, ..ClapConfig::default() };
    let vst3 = Vst3Config {
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    let host = PluginHost::with_configs(clap, vst3).unwrap();
    let found = host.clap().descriptors().into_iter().chain(host.vst3().descriptors()).any(|found| found.uri == uri);
    if !found {
        eprintln!("skipping: {uri} is not installed on this machine");
    }
    found.then_some(host)
}

/// A server whose devices include the plugins installed on this machine;
/// `None` if `uri` is not among them.
async fn connect_with_plugins(uri: &str) -> Option<(Fixture, tempfile::TempDir)> {
    let host = installed(uri)?;
    let folder = tempfile::tempdir().unwrap();
    let storage = trill_runtime::Storage::At {
        settings: folder.path().join("config").join("settings.json"),
        library_index: Some(folder.path().join("cache").join("library-index.json")),
    };
    let audio = trill_engine::AudioBackend::Null { sample_rate: 48_000 };
    let options = RuntimeOptions { audio, plugins: None, storage };
    let parts = Parts { devices: host.factories(), ..Parts::default() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    Some((serve(runtime).await, folder))
}

/// The highest sample of `audio` in each of the windows that start at
/// `starts` seconds and last `seconds`, in dB.
fn levels(audio: &[f32], rate: f64, starts: &[f64], seconds: f64) -> Vec<f64> {
    starts
        .iter()
        .map(|start| {
            let from = ((start * rate) as usize).min(audio.len());
            let to = (((start + seconds) * rate) as usize).min(audio.len());
            let peak = audio[from..to].iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            20.0 * f64::from(peak.max(1e-6)).log10()
        })
        .collect()
}

/// With the installed Superior Drummer 3: a snare on the first sample of
/// what is rendered, and one 0.2 s in, sound like the one a second later,
/// with their attacks. On a plugin nobody has made in this process yet,
/// which is as cold as it gets, and through the tools, where the song's
/// own instance of the plugin was made a moment before.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Superior Drummer 3 (VST3) installed; run with `cargo test -p trill-mcp --test hearing -- --include-ignored --nocapture`"]
async fn a_snare_on_the_first_sample_of_a_render_sounds_on_superior_drummer_3() {
    let Some(host) = installed(SD3) else { return };
    let hits = [0.0, 0.2, 1.0, 2.0];

    // Cold: nothing has made the plugin in this process before the render.
    let mut project = trill_core::Project::new("cold");
    project.set_tempo(120.0).unwrap();
    let drums = project.add_track("Drums");
    project.add_device(trill_core::ChainRef::Track(drums), SD3, "Superior Drummer 3", None).unwrap();
    let hit_clip = project.add_midi_clip(drums, "Hits", 0.0, 8.0).unwrap();
    let notes: Vec<trill_core::Note> = hits
        .iter()
        .map(|seconds| trill_core::Note { pitch: 38, start: seconds * 2.0, length: 0.25, velocity: 110 })
        .collect();
    project.clip_mut(hit_clip).unwrap().add_notes(&notes).unwrap();
    // Eight seconds on: a crash and the ride, twice, three seconds apart.
    // A kit loads its cymbals last, so these are the ones a render that
    // starts too soon would lose.
    let cymbal_clip = project.add_midi_clip(drums, "Cymbals", 16.0, 16.0).unwrap();
    let cymbals: Vec<trill_core::Note> = [(49, 0.0), (51, 1.0), (49, 6.0), (51, 7.0)]
        .iter()
        .map(|(pitch, start)| trill_core::Note { pitch: *pitch, start: *start, length: 0.25, velocity: 110 })
        .collect();
    project.clip_mut(cymbal_clip).unwrap().add_notes(&cymbals).unwrap();
    let factory = trill_core::device::CompositeFactory::new(host.factories());
    let options = trill_engine::RenderOptions { max_tail_seconds: 1.0, ..trill_engine::RenderOptions::default() };

    // The cymbals first, from the crash on: the first thing this process
    // asks of the plugin.
    let from_crash = trill_engine::RenderOptions { start_beat: 16.0, ..options.clone() };
    let begun = Instant::now();
    let cold = trill_engine::render::render(&project, &factory, &from_crash).unwrap();
    let at = [0.0, 0.5, 3.0, 3.5];
    let cymbal_levels = levels(&cold.left, 48_000.0, &at, 0.3);
    eprintln!(
        "cold, from the crash: rendered in {:.1?}; crash, ride, crash, ride at {at:?} s peak at {cymbal_levels:.1?}          dB; waited {:?}",
        begun.elapsed(),
        cold.measured.waited
    );
    assert!(cymbal_levels[0] > -50.0, "the crash on the first sample sounds: {cymbal_levels:?}");
    assert!((cymbal_levels[0] - cymbal_levels[2]).abs() < 4.0, "as loud as the one after it: {cymbal_levels:?}");
    assert!((cymbal_levels[1] - cymbal_levels[3]).abs() < 4.0, "and the ride as its second: {cymbal_levels:?}");

    let options = trill_engine::RenderOptions { end_beat: Some(8.0), ..options };
    let begun = Instant::now();
    let cold = trill_engine::render::render(&project, &factory, &options).unwrap();
    let took = begun.elapsed();
    let (whole, attacks) = (levels(&cold.left, 48_000.0, &hits, 0.15), levels(&cold.left, 48_000.0, &hits, 0.005));
    eprintln!(
        "cold: rendered in {took:.1?}; the hits at {hits:?} s peak at {whole:.1?} dB, their first 5 ms at \
         {attacks:.1?} dB; waited {:?}",
        cold.measured.waited
    );
    assert!(whole.iter().all(|level| *level > -30.0), "every hit sounds: {whole:?}");
    assert!((whole[0] - whole[2]).abs() < 4.0 && (whole[1] - whole[2]).abs() < 4.0, "as loud as later: {whole:?}");
    assert!(attacks[0] > -40.0 && (attacks[0] - attacks[2]).abs() < 8.0, "the first hit has its attack: {attacks:?}");
    drop((cold, factory, host));

    // Through the tools: a new song, the plugin added, and rendered at once.
    let Some((fixture, folder)) = connect_with_plugins(SD3).await else { return };
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 120 })).await;
    let drums = track(client, "Drums", SD3).await;
    let pattern: Vec<Value> = hits.iter().map(|seconds| json!([38, seconds * 2.0, 0.25, 110])).collect();
    clip(client, drums, 0.0, 8.0, json!(pattern)).await;
    let wav = folder.path().join("snares.wav");
    let begun = Instant::now();
    let report = ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 1 })).await;
    let audio = trill_media::decode(&wav).unwrap();
    let (whole, attacks) = (levels(&audio.left, 48_000.0, &hits, 0.15), levels(&audio.left, 48_000.0, &hits, 0.005));
    eprintln!(
        "through the tools: rendered in {:.1?}; hits {whole:.1?} dB, first 5 ms {attacks:.1?} dB; notes {}",
        begun.elapsed(),
        report["notes"]
    );
    assert!(whole.iter().all(|level| *level > -30.0), "every hit sounds: {whole:?}");
    assert!((whole[0] - whole[2]).abs() < 4.0 && (attacks[0] - attacks[2]).abs() < 8.0, "{whole:?} {attacks:?}");

    // A stretch from the middle of the song that begins on a hit.
    let part = folder.path().join("from-one-second.wav");
    ok(client, "render", json!({ "path": part.display().to_string(), "start": 2, "max_tail_seconds": 1 })).await;
    let audio = trill_media::decode(&part).unwrap();
    let from_hit = levels(&audio.left, 48_000.0, &[0.0, 1.0], 0.15);
    eprintln!("from the hit at 1 s: {from_hit:.1?} dB");
    assert!(from_hit[0] > -30.0 && (from_hit[0] - whole[2]).abs() < 4.0, "{from_hit:?} against {whole:?}");

    // And its map, in one call.
    let begun = Instant::now();
    let map = ok(client, "probe", json!({ "track": drums })).await;
    let size = map.to_string().len();
    eprintln!("the kit's map in {:.1?} ({size} bytes): sounding {}", begun.elapsed(), map["sounding"]);
    for entry in map["heard"].as_array().unwrap() {
        eprintln!("   {entry}");
    }
    let likes: Vec<&str> = map["heard"].as_array().unwrap().iter().filter_map(|entry| entry["like"].as_str()).collect();
    assert!(likes.iter().any(|like| like.contains("low")), "a kick or a tom among them: {likes:?}");
    assert!(likes.iter().any(|like| like.contains("bright")), "and a cymbal or a hat: {likes:?}");
    assert!(size < 20_000, "a map that can be read: {size} bytes");

    // Nothing arrives late: what the drums and the cymbals measure in a
    // probe made now is what they measure half a minute on.
    let kit = json!({ "track": drums, "notes": [36, 38, 42, 46, 49, 51, 53, 55, 57, 52, 86], "detail": true });
    let now = ok(client, "probe", kit.clone()).await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let later = ok(client, "probe", kit).await;
    let level_of = |reply: &Value| -> Vec<(String, f64)> {
        let entries = reply["heard"].as_array().unwrap().iter();
        entries.map(|e| (e["notes"].as_str().unwrap().to_string(), e["level_db"].as_f64().unwrap_or(-120.0))).collect()
    };
    eprintln!(
        "the kit now: {:?}
half a minute on: {:?}",
        level_of(&now),
        level_of(&later)
    );
    assert_eq!(now["sounding"], later["sounding"], "the same drums sound");
    for ((name, before), (_, after)) in level_of(&now).iter().zip(level_of(&later)) {
        assert!((before - after).abs() < 4.0, "{name}: {before} dB now, {after} dB later");
    }
}

/// With the installed SWAM Alto Sax 3, which stays silent for seconds
/// after it is created: a note on the first sample of a render sounds, and
/// a probe says in one call what it takes to make it sound at all.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs SWAM Alto Sax 3 (VST3) installed; run with `cargo test -p trill-mcp --test hearing -- --include-ignored --nocapture`"]
async fn a_note_on_the_first_sample_of_a_render_sounds_on_a_swam_saxophone() {
    let Some((fixture, folder)) = connect_with_plugins(SWAM_SAX).await else { return };
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 120 })).await;
    let sax = track(client, "Alto", SWAM_SAX).await;
    let made = ok(
        client,
        "create_clip",
        json!({
            "track": sax, "start": 0, "length": 8,
            "notes": [["E4", 0, 1.5], ["G4", 2, 1.5], ["E4", 4, 1.5]],
            "controllers": { "expression": [[0, 100]] },
        }),
    )
    .await;
    assert!(made["clip_id"].is_u64());
    let wav = folder.path().join("sax.wav");
    let begun = Instant::now();
    let report = ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 1 })).await;
    let audio = trill_media::decode(&wav).unwrap();
    // Each note over its first three quarters of a second, and the first
    // one over its first 150 ms.
    let notes = levels(&audio.left, 48_000.0, &[0.0, 1.0, 2.0], 0.75);
    let opening = levels(&audio.left, 48_000.0, &[0.0], 0.15);
    eprintln!(
        "rendered in {:.1?}: the notes peak at {notes:.1?} dB, the first 150 ms at {opening:.1?} dB; notes {}",
        begun.elapsed(),
        report["notes"]
    );
    assert!(notes.iter().all(|level| *level > -40.0), "every note sounds: {notes:?}");
    assert!((notes[0] - notes[2]).abs() < 6.0, "the first as the third, which is the same note: {notes:?}");
    assert!(opening[0] > -45.0, "and from its beginning: {opening:?}");
    assert!(strings(&report["notes"]).iter().all(|note| !note.contains("made no sound")), "{}", report["notes"]);

    // What it answers to: nothing as it is, the whole of its range with
    // expression.
    let begun = Instant::now();
    let plain = ok(client, "probe", json!({ "track": sax, "notes": ["C3-C6"], "step": 3 })).await;
    eprintln!(
        "probed without expression in {:.1?}: sounding {}; notes {}",
        begun.elapsed(),
        plain["sounding"],
        plain["notes"]
    );
    assert_eq!(plain["sounding"], "none");
    assert!(strings(&plain["notes"]).iter().any(|note| note.contains("expression")), "{}", plain["notes"]);
    let begun = Instant::now();
    let blown = ok(client, "probe", json!({ "track": sax, "controllers": { "expression": 100 } })).await;
    eprintln!(
        "probed with expression in {:.1?}: sounding {}, silent {}, pitch {}",
        begun.elapsed(),
        blown["sounding"],
        blown["silent"],
        blown["pitch"]
    );
    for entry in blown["heard"].as_array().unwrap() {
        eprintln!("   {entry}");
    }
    assert!(blown["sounding"].as_str().unwrap().contains('-'), "a range of notes: {blown}");
    assert_eq!(blown["pitch"], "sounds as written");
}

/// With the installed Ample Metal R, a sampled bass with keyswitches: its
/// range and octave in one call, and what its keyswitches do in another.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Ample Metal R (VST3) installed; run with `cargo test -p trill-mcp --test hearing -- --include-ignored --nocapture`"]
async fn a_sampled_bass_is_learned_in_two_calls() {
    let Some((fixture, _folder)) = connect_with_plugins(AMPLE_BASS).await else { return };
    let client = &fixture.client;
    let bass = track(client, "Bass", AMPLE_BASS).await;
    let begun = Instant::now();
    let range = ok(client, "probe", json!({ "track": bass })).await;
    eprintln!(
        "range in {:.1?} ({} bytes): range {}; sounding {}; silent {}; pitch {}; notes {}",
        begun.elapsed(),
        range.to_string().len(),
        range["range"],
        range["sounding"],
        range["silent"],
        range["pitch"],
        range["notes"]
    );
    for entry in range["heard"].as_array().unwrap() {
        eprintln!("   {entry}");
    }
    assert!(range["sounding"].as_str().unwrap().contains('-'), "{range}");
    assert!(range["pitch"].as_str().is_some_and(|pitch| pitch.starts_with("sounds ")), "{range}");
    assert!(strings(&range["notes"]).iter().any(|note| note.contains("keyswitches")), "{}", range["notes"]);

    let begun = Instant::now();
    // The keys just under its range, as the first probe suggests.
    let switches = ok(client, "probe", json!({ "track": bass, "notes": ["E2"], "keyswitches": ["C1-A#1"] })).await;
    eprintln!("keyswitches in {:.1?} ({} bytes)", begun.elapsed(), switches.to_string().len());
    for entry in switches["keyswitches"].as_array().unwrap() {
        eprintln!("   {entry}");
    }
    assert!(switches["keyswitches"].as_array().unwrap().len() >= 2, "{switches}");
}

/// Archetype Tim Henson X, an amplifier, as a VST3 plugin: an effect that
/// passes no sound for a moment after it is created.
const AMPLIFIER: &str = "vst3:ABCDEF019182FAEB4E4453504E544858";

/// With the installed Archetype Tim Henson behind a built-in synth, which
/// has nothing to be late with itself: a note on the first sample of a
/// render comes through the amplifier as the same note two seconds later
/// does.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Archetype Tim Henson X (VST3) installed; run with `cargo test -p trill-mcp --test hearing -- --include-ignored --nocapture`"]
async fn a_note_on_the_first_sample_comes_through_an_amplifier_that_takes_a_moment() {
    let Some((fixture, folder)) = connect_with_plugins(AMPLIFIER).await else { return };
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 120 })).await;
    let lead = track(client, "Lead", "builtin:polysynth").await;
    ok(client, "add_device", json!({ "track": lead, "uri": AMPLIFIER })).await;
    clip(client, lead, 0.0, 8.0, json!([["E3", 0, 1], ["E3", 4, 1]])).await;
    let wav = folder.path().join("amp.wav");
    let begun = Instant::now();
    let report = ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 1 })).await;
    let audio = trill_media::decode(&wav).unwrap();
    let notes = levels(&audio.left, 48_000.0, &[0.0, 2.0], 0.4);
    let opening = levels(&audio.left, 48_000.0, &[0.0, 2.0], 0.05);
    eprintln!(
        "rendered in {:.1?}: the note at 0 s and at 2 s peak at {notes:.1?} dB, their first 50 ms at {opening:.1?} dB; \
         notes {}",
        begun.elapsed(),
        report["notes"]
    );
    assert!(notes[0] > -60.0, "the first note comes through: {notes:?}");
    assert!((notes[0] - notes[1]).abs() < 3.0, "as loud as the same note later: {notes:?}");
    assert!((opening[0] - opening[1]).abs() < 6.0, "from its beginning: {opening:?}");
}

/// What is left when one render is taken from another, against the first:
/// far down means the same samples.
fn null_db(one: &[f32], other: &[f32]) -> f64 {
    let frames = one.len().min(other.len());
    let (mut left, mut whole) = (0.0f64, 1e-30f64);
    for frame in 0..frames {
        left += f64::from(one[frame] - other[frame]).powi(2);
        whole += f64::from(one[frame]).powi(2);
    }
    10.0 * (left.max(1e-30) / whole).log10()
}

/// The sampled instruments of Ample Sound, as VST3 plugins: three guitars
/// and the bass. Each keeps several recordings of a note and plays
/// another one every time the note sounds.
const AMPLE: [(&str, &str); 4] = [
    ("Ample Metal R", AMPLE_BASS),
    ("Ample Metal Eclipse", "vst3:ABCDEF019182FAEB41707364416D6574"),
    ("Ample Metal Hellrazer", "vst3:ABCDEF019182FAEB41707364416D6872"),
    ("Ample Guitar PF", "vst3:ABCDEF019182FAEB4170736441677066"),
];

/// Renders `track` alone `times` times, with a probe of it and a measure
/// of the whole song in between (other instances of the plugin, played
/// other notes), and returns what is left of each later render when it is
/// taken from the first, in dB.
async fn rendered_again(client: &Client, folder: &Path, name: &str, track: u64, times: usize) -> Vec<f64> {
    let mut renders: Vec<Vec<f32>> = Vec::new();
    for index in 0..times {
        let wav = folder.join(format!("{name}-{index}.wav"));
        let begun = Instant::now();
        let arguments = json!({ "path": wav.display().to_string(), "tracks": [track], "start": 0, "end": 8, "max_tail_seconds": 2 });
        let report = ok(client, "render", arguments).await;
        let audio = trill_media::decode(&wav).unwrap();
        eprintln!(
            "  {name}, render {index} in {:.1?}: {:.2} LUFS. {}",
            begun.elapsed(),
            report["loudness_lufs"].as_f64().unwrap_or(f64::NAN),
            strings(&report["notes"]).into_iter().filter(|note| note.contains("Waited")).collect::<String>()
        );
        renders.push(audio.left);
        std::fs::remove_file(&wav).unwrap();
        // Between two renders, other instances of the plugin play: a probe
        // of other keys, and the song measured.
        match index {
            0 => drop(ok(client, "probe", json!({ "track": track, "notes": [47, 50, 54] })).await),
            1 => drop(ok(client, "measure", json!({ "start": 2, "end": 6 })).await),
            _ => {}
        }
    }
    renders[1..].iter().map(|render| null_db(&renders[0], render)).collect()
}

/// With the installed instruments of Ample Sound, set to take their
/// recordings in turn: sixteen notes on two pitches, rendered four times
/// with other things played in between, come out as the same samples each
/// time, because the wait before a render sounds every note it rehearses
/// the same number of times, whatever it waited for and for how long.
///
/// **What this cannot hold, and so reports instead of asserting:** a patch
/// that draws at random from a stream which every instance of the plugin
/// in the program shares. These instruments do as they come: the guitars
/// draw every take that way (their `Cycle Mode Switch` is 0), the bass the
/// noise of the hand moving between two frets (`M-Fret Sound`), and its
/// preset "Stingray" a fret buzz on top. Every such draw on any instance
/// moves the stream, nothing a host does puts it back while an instance
/// lives (and the song's own always does), and only a program that has
/// just started begins it at the same place. Two renders of such a patch
/// in one program differ, and the test says by how much.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Ample Sound's instruments (VST3) installed; run with `cargo test -p trill-mcp --test hearing the_same_song_rendered_again -- --include-ignored --nocapture`"]
async fn the_same_song_rendered_again_is_the_same_samples_where_takes_go_round_and_is_not_where_they_are_drawn_at_random()
 {
    let notes: Vec<Value> =
        (0..16).map(|index| json!([if index % 2 == 0 { 45 } else { 52 }, f64::from(index) * 0.5, 0.45, 104])).collect();
    let (mut held, mut not_held) = (Vec::new(), Vec::new());
    for (name, uri) in AMPLE {
        // A program of plugins for each instrument.
        let Some((fixture, folder)) = connect_with_plugins(uri).await else { continue };
        let client = &fixture.client;
        ok(client, "set_project", json!({ "tempo": 156 })).await;
        let part = |name: String, preset: Option<&'static str>| async move {
            let mut arguments = json!({ "name": name, "instrument": uri });
            if let Some(preset) = preset {
                arguments["preset"] = json!(preset);
            }
            let made = ok(client, "create_track", arguments).await;
            (made["track_id"].as_u64().unwrap(), made["instrument_device_id"].as_u64().unwrap())
        };

        // Its takes in turn, and no noise that is drawn at random.
        let (round, device) = part(format!("{name} in turn"), None).await;
        let in_turn = json!({ "Cycle Mode Switch": 1, "M-Fret Sound": 0 });
        ok(client, "update_device", json!({ "device": device, "settings": in_turn })).await;
        clip(client, round, 0.0, 8.0, json!(notes)).await;
        // And as it comes, beside it in the same song.
        let (random, _) = part(format!("{name} as it comes"), None).await;
        clip(client, random, 0.0, 8.0, json!(notes)).await;

        let nulls = rendered_again(client, folder.path(), &format!("{name}, its takes in turn"), round, 4).await;
        eprintln!("{name}, its takes in turn: taken from the first render, the others leave {nulls:.1?} dB");
        assert!(nulls.iter().all(|null| *null < -60.0), "{name}: the same samples every time: {nulls:?}");
        held.push(name);

        let mut cases = vec![("as it comes".to_string(), random)];
        if uri == AMPLE_BASS {
            let (buzz, _) = part("Stingray".to_string(), Some("Stingray")).await;
            clip(client, buzz, 0.0, 8.0, json!(notes)).await;
            cases.push(("with its preset Stingray".to_string(), buzz));
        }
        for (what, track) in cases {
            let nulls = rendered_again(client, folder.path(), &format!("{name} {what}"), track, 3).await;
            not_held.push(format!("{name} {what} {nulls:.1?} dB"));
        }
    }
    eprintln!("HELD, the same samples every time with the takes in turn: {held:?}");
    eprintln!(
        "NOT HELD, and not asserted: patches that draw at random from a stream every instance in the program shares. \
         Taken from their first render, later renders in the same program leave: {}. Only a program that has just \
         started repeats these.",
        not_held.join("; ")
    );
}

/// Serum 2, a synthesizer, as a VST3 plugin.
const SERUM: &str = "vst3:56534558667350736572756D20320000";

/// With the plugins the owner's songs are made of: a song of two
/// amplifiers behind built-in synths, two sampled basses set to take their
/// recordings in turn, two synthesizers, a drum kit and a reverb on a
/// return is rendered on one processor, on one again, and on several,
/// each time with every part written to a file of its own. Every part
/// that is the same samples the two times one processor made it (its
/// plugins do the same thing twice) is those samples again on several.
/// The test says which parts those are and how long each render took, and
/// fails if fewer than six of them are.
///
/// The drum kit is in the song and is not held to this: Superior Drummer
/// 3 chooses its hits for itself, two renders of it on one processor are
/// the same samples some of the time and not most of it, and so two that
/// agree say nothing about a third. What it and the sums it is part of
/// come to is said, and not asserted.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Archetype Tim Henson X, Ample Metal R, Serum 2 and Superior Drummer 3 (VST3) installed; run with `cargo test -p trill-mcp --test hearing a_song_of_installed_plugins -- --include-ignored --nocapture`"]
async fn a_song_of_installed_plugins_is_the_same_samples_on_one_processor_and_on_several() {
    let Some((fixture, folder)) = connect_with_plugins(AMPLIFIER).await else { return };
    if [AMPLE_BASS, SERUM, SD3].into_iter().any(|uri| installed(uri).is_none()) {
        return;
    }
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 132 })).await;
    let room =
        ok(client, "create_track", json!({ "name": "Room", "kind": "return" })).await["track_id"].as_u64().unwrap();
    ok(client, "add_device", json!({ "track": room, "uri": "builtin:reverb" })).await;
    let line = |pitches: [u8; 4]| -> Value {
        (0..32).map(|step| json!([pitches[step % 4], step as f64 * 0.5, 0.4, 100])).collect()
    };
    // Two amplifiers, each behind a built-in synth.
    for (name, pitches) in [("Amp low", [40, 47, 52, 47]), ("Amp high", [64, 59, 67, 71])] {
        let part = track(client, name, "builtin:polysynth").await;
        ok(client, "add_device", json!({ "track": part, "uri": AMPLIFIER })).await;
        clip(client, part, 0.0, 16.0, line(pitches)).await;
        ok(client, "update_track", json!({ "track": part, "send": { "to": room, "level_db": -12 } })).await;
    }
    // Two basses of one maker, which are run in turn, with their takes in
    // turn too.
    for (name, pitches) in [("Bass", [45, 45, 52, 50]), ("Bass above", [57, 64, 62, 57])] {
        let made = ok(client, "create_track", json!({ "name": name, "instrument": AMPLE_BASS })).await;
        let (part, device) = (made["track_id"].as_u64().unwrap(), made["instrument_device_id"].as_u64().unwrap());
        let in_turn = json!({ "Cycle Mode Switch": 1, "M-Fret Sound": 0 });
        ok(client, "update_device", json!({ "device": device, "settings": in_turn })).await;
        clip(client, part, 0.0, 16.0, line(pitches)).await;
    }
    for (name, pitches) in [("Synth", [60, 67, 72, 67]), ("Synth above", [76, 79, 84, 79])] {
        let part = track(client, name, SERUM).await;
        clip(client, part, 0.0, 16.0, line(pitches)).await;
        ok(client, "update_track", json!({ "track": part, "send": { "to": room, "level_db": -9 } })).await;
    }
    let drums = track(client, "Drums", SD3).await;
    clip(client, drums, 0.0, 16.0, line([36, 42, 38, 42])).await;

    let mut renders: Vec<(Value, f64)> = Vec::new();
    for (name, threads) in [("one", json!(1)), ("one again", json!(1)), ("several", Value::Null)] {
        let wav = folder.path().join(format!("{name}.wav"));
        let mut arguments = json!({ "path": wav.display().to_string(), "stems": ["all"], "max_tail_seconds": 2 });
        if !threads.is_null() {
            arguments["threads"] = threads;
        }
        let begun = Instant::now();
        let report = ok(client, "render", arguments).await;
        let took = begun.elapsed().as_secs_f64();
        eprintln!(
            "{name}: {} thread(s), {took:.1} s for {} s of audio; {}",
            report["threads"],
            report["duration_seconds"],
            strings(&report["notes"]).into_iter().filter(|note| note.contains("processor")).collect::<String>()
        );
        renders.push((report, took));
    }
    assert_eq!(renders[0].0["threads"], 1);
    assert!(renders[2].0["threads"].as_u64().unwrap() > 1, "{}", renders[2].0);
    let files = |report: &Value| -> Vec<(String, String)> {
        let name =
            |stem: &Value| format!("{} {}", stem["what"].as_str().unwrap_or(""), stem["name"].as_str().unwrap_or(""));
        let stems = report["stems"].as_array().unwrap().iter();
        let mut files: Vec<_> = stems.map(|stem| (name(stem), stem["file"].as_str().unwrap().to_string())).collect();
        files.push(("the mix".to_string(), report["path"].as_str().unwrap().to_string()));
        files
    };
    let (one, again, all) = (files(&renders[0].0), files(&renders[1].0), files(&renders[2].0));
    let (mut held, mut not_judged, mut drums) = (Vec::new(), Vec::new(), Vec::new());
    for ((part, one), ((_, again), (_, all))) in one.iter().zip(again.iter().zip(&all)) {
        let read = |path: &String| trill_media::decode(Path::new(path)).unwrap().left;
        let (one, again, all) = (read(one), read(again), read(all));
        let with_drums = part.contains("Drums") || part.starts_with("premaster") || part == "the mix";
        if with_drums {
            drums.push(format!(
                "{part}: {:.0} dB left the second time on one processor, {:.0} dB on several",
                null_db(&one, &again),
                null_db(&one, &all)
            ));
        } else if one == again {
            assert!(all == one, "{part}: other samples on several processors ({:.0} dB left)", null_db(&one, &all));
            held.push(part.clone());
        } else {
            not_judged.push(format!(
                "{part} ({:.0} dB left the second time on one processor, {:.0} dB on all)",
                null_db(&one, &again),
                null_db(&one, &all)
            ));
        }
    }
    eprintln!("the same samples on one processor and on several: {held:?}");
    eprintln!("not the same samples twice even on one processor, so not judged: {not_judged:?}");
    eprintln!(
        "the drum kit, which chooses its hits for itself, and the sums it is in (far down is the same): {drums:?}"
    );
    eprintln!("one processor {:.1} s and {:.1} s, several {:.1} s", renders[0].1, renders[1].1, renders[2].1);
    assert!(held.len() >= 6, "too few parts repeat themselves for this to say anything: {held:?}");
}
