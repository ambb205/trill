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

    let begun = Instant::now();
    let report = ok(client, "measure", json!({})).await;
    let notes = strings(&report["notes"]);
    let about = |name: &str| notes.iter().find(|note| note.starts_with(&format!("'{name}'"))).cloned();
    // A key that plays nothing sounds like a drum that has not been loaded:
    // the kit was held to it before the render began, and the reply says
    // which note it was and how to find out.
    let waited = |notes: &[String]| notes.iter().find(|note| note.contains("on 'Half'")).cloned();
    let half_note = waited(&notes).unwrap_or_else(|| panic!("{notes:#?}"));
    assert!(half_note.contains("1 of the notes it plays made no sound the whole time: A2 (45)"), "{half_note}");
    assert!(half_note.contains("still loading") && half_note.contains(&format!("`probe` with track {half}")));
    let low_note = about("Low").unwrap_or_else(|| panic!("{notes:#?}"));
    assert!(low_note.contains("run from E0 to D1") && low_note.contains("outside its range"), "{low_note}");
    assert!(low_note.contains(&format!("probe with track {low}")), "{low_note}");
    let switch_note = about("Switch").unwrap();
    assert!(switch_note.contains("keyswitches") && !switch_note.contains("expression"), "{switch_note}");
    assert!(about("Fine").is_none());
    // Neither leads with breath or a bow.
    assert!(notes.iter().all(|note| !note.contains("saxophone")), "{notes:#?}");
    let first = begun.elapsed();

    // The plugins that never answered were waited for once. Measured again,
    // they are known not to answer and cost a moment.
    let begun = Instant::now();
    let again = ok(client, "measure", json!({})).await;
    assert!(begun.elapsed() < first, "{:?} after {first:?}", begun.elapsed());
    // What the kit was held to once is known of it now, and not said twice.
    assert_eq!(waited(&strings(&again["notes"])), None);

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
    for word in ["tracks", "sections", "bars", "detail"] {
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
