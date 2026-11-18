//! What a study does to a song, the way an AI client asks for it: over
//! the real MCP transport, with nothing but tool calls.
//!
//! The study is a real one of a recording the test makes, run in a
//! process of its own on the scripted models (as `studying.rs` runs them),
//! and what is taken from it is read back out of the song.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_engine::AudioBackend;
use trill_mcp::{HttpOptions, HttpServer, serve_http_with};
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage, Studying};
use trill_study::models::scripted::Script;

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    client: Client,
    runtime: Arc<Runtime>,
    _server: HttpServer,
    /// Holds the runtime's settings, its studies and the files a test
    /// makes. Nothing of the user's is read or written.
    folder: tempfile::TempDir,
}

async fn connect() -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let storage = Storage::At {
        settings: folder.path().join("config").join("settings.json"),
        library_index: Some(folder.path().join("cache").join("library-index.json")),
    };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let words = ["one", "two", "three", "four"].map(String::from).to_vec();
    let studying = Studying {
        stand_in: Some(Script { words, ..Script::default() }),
        // This crate's own program, which studies when it is started for that.
        program: Some(PathBuf::from(env!("CARGO_BIN_EXE_trill-mcp"))),
        ..Studying::default()
    };
    let runtime = Arc::new(Runtime::start_custom(options, Parts { studying, ..Parts::default() }).unwrap());
    let options = HttpOptions { assistant_folder: Some(folder.path().join("work")) };
    let server = serve_http_with(runtime.clone(), "127.0.0.1:0".parse().unwrap(), options).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { client, runtime, _server: server, folder }
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

async fn fails(client: &Client, tool: &'static str, arguments: Value) -> String {
    match call(client, tool, arguments).await {
        Err(sentence) => sentence,
        Ok(reply) => panic!("{tool} was expected to refuse, and answered {reply}"),
    }
}

const RATE: f64 = 44_100.0;

/// Adds a note of MIDI pitch `note`, with so many harmonics.
fn tone(out: &mut [f32], at: f64, seconds: f64, note: f64, level: f32, harmonics: usize) {
    let hertz = 440.0 * 2f64.powf((note - 69.0) / 12.0);
    let start = (at * RATE) as usize;
    for index in 0..(seconds * RATE) as usize {
        let Some(slot) = out.get_mut(start + index) else { break };
        let time = index as f64 / RATE;
        let fade = (time / 0.004).min(1.0) * ((seconds - time) / 0.012).clamp(0.0, 1.0);
        let wave: f64 = (1..=harmonics).map(|k| (TAU * hertz * k as f64 * time).sin() / (k * k) as f64).sum();
        *slot += level * (wave * fade) as f32;
    }
}

/// Eight bars at 100 beats a minute, starting 0.3 seconds in: a hat on
/// every eighth, a bass note to the beat, and a voice that sings four
/// notes in every other bar. Made so that the scripted models, which split
/// by register, have a voice, drums and a bass to find.
fn recording(path: &Path) -> String {
    let beat = 0.6;
    let mut out = vec![0.0f32; ((0.3 + 32.0 * beat + 0.6) * RATE) as usize];
    let mut seed = 77u32;
    for index in 0..64 {
        let start = ((0.3 + index as f64 * beat / 2.0) * RATE) as usize;
        let mut previous = 0.0f32;
        for frame in 0..(0.045 * RATE) as usize {
            let Some(slot) = out.get_mut(start + frame) else { break };
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let hiss = (seed >> 8) as f32 / 8_388_608.0 - 1.0;
            *slot += 0.5 * (hiss - previous) * (-(frame as f32) / (0.009 * RATE as f32)).exp();
            previous = hiss;
        }
    }
    for index in 0..32 {
        tone(&mut out, 0.3 + index as f64 * beat, beat * 0.9, [33.0, 33.0, 40.0, 36.0][index % 4], 0.5, 2);
    }
    for bar in (0..8).step_by(2) {
        for (index, note) in [69.0, 72.0, 76.0, 72.0].into_iter().enumerate() {
            tone(&mut out, 0.3 + (bar * 4 + index) as f64 * beat, beat * 0.8, note, 0.3, 4);
        }
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(path, &AudioBuffer { sample_rate: RATE, left: out.clone(), right: out }, WavFormat::Int24).unwrap();
    path.display().to_string()
}

/// Asks for a study of a recording made for the test, and waits for it.
async fn studied(fixture: &Fixture) -> (String, String) {
    let path = recording(&fixture.folder.path().join("refs").join("small song.wav"));
    let started = ok(&fixture.client, "study", json!({ "action": "start", "path": path })).await;
    let id = started["study"].as_str().unwrap().to_owned();
    let done = ok(&fixture.client, "study", json!({ "action": "report", "study": id, "wait_seconds": 120 })).await;
    assert_eq!(done["state"], "done", "{done}");
    (id, path)
}

/// The clip `id` as get_project shows it, with the track it is on.
fn clip_of(project: &Value, id: &Value) -> (Value, Value) {
    for track in project["tracks"].as_array().unwrap() {
        for clip in track["clips"].as_array().unwrap() {
            if &clip["id"] == id {
                return (clip.clone(), track.clone());
            }
        }
    }
    panic!("no clip {id} in {project}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stretches_of_a_study_become_clips_that_lie_on_the_songs_bars() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (id, path) = studied(&fixture).await;
    ok(client, "set_project", json!({ "tempo": 120.0 })).await;
    let study_folder = fixture.folder.path().join("config").join("studies").join(&id);

    // Two bars of the drums, put on bar 5 of a song at another tempo: a clip of the stem's own file that
    // begins where those bars begin in the recording and is two bars of the song long.
    let taken = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "stems": ["drums"], "from_bar": 3, "to_bar": 4, "at_bar": 5 }),
    )
    .await;
    println!("{taken}");
    assert_eq!((&taken["beats"], &taken["at"], &taken["warp"]), (&json!(8.0), &json!(16.0), &json!("beats")));
    assert!((taken["its_tempo"].as_f64().unwrap() - 100.0).abs() < 0.5 && taken["song_tempo"] == 120.0, "{taken}");
    assert_eq!(taken["file"], study_folder.join("stems").join("drums.wav").display().to_string());
    assert_eq!(taken["name"], "Drums bars 3-4 (small song)");
    let project = ok(client, "get_project", json!({})).await;
    let (clip, track) = clip_of(&project, &taken["clip_ids"][0]);
    println!("{clip}");
    assert_eq!((&clip["start"], &clip["length"], &track["name"]), (&json!(16.0), &json!(8.0), &taken["name"]));
    assert_eq!((&clip["path"], &clip["warp"]), (&taken["file"], &json!("beats")));
    let offset = clip["offset_seconds"].as_f64().unwrap();
    let bar_three = ok(client, "listen", json!({ "study": id, "show": ["bars"], "from_bar": 3, "to_bar": 3 })).await;
    println!("{bar_three}");
    assert!((offset - bar_three["bars"]["items"][0]["at_seconds"].as_f64().unwrap()).abs() < 1e-3, "{clip}");
    // One undo step, track and clip together.
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(ok(client, "get_project", json!({})).await["tracks"].as_array().unwrap().len(), 0);
    ok(client, "history", json!({ "action": "redo" })).await;

    // Laid end to end three times on the track that is there, moved up two semitones.
    let track_id = taken["track_id"].clone();
    let thrice = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "stems": ["bass"], "from_bar": 1, "to_bar": 1, "track": track_id,
                "at_bar": 9, "times": 3, "semitones": 2 }),
    )
    .await;
    assert_eq!((thrice["clip_ids"].as_array().unwrap().len(), &thrice["warp"]), (3, &json!("tonal")));
    assert_eq!((&thrice["semitones"], &thrice["track_id"]), (&json!(2.0), &track_id));
    assert!(thrice["said"][0].as_str().unwrap().starts_with("the pitch is moved by the stretcher"), "{thrice}");
    let project = ok(client, "get_project", json!({})).await;
    let starts: Vec<f64> = thrice["clip_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|clip| clip_of(&project, clip).0["start"].as_f64().unwrap())
        .collect();
    assert_eq!(starts, [32.0, 36.0, 40.0]);
    assert_eq!(clip_of(&project, &thrice["clip_ids"][0]).0["transpose"], 2.0);

    // Two stems together are one sound: cut into a file in the study's own folder, which the clip plays
    // from its start.
    let both = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "stems": ["drums", "bass"], "from_bar": 2, "to_bar": 2, "at_bar": 20 }),
    )
    .await;
    let cut = PathBuf::from(both["file"].as_str().unwrap());
    assert!(cut.starts_with(study_folder.join("cuts")) && cut.is_file(), "{both}");
    assert_eq!(both["name"], "Bass + Drums bar 2 (small song)");
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(clip_of(&project, &both["clip_ids"][0]).0["offset_seconds"], 0.0);
    let cut_seconds = trill_media::probe(&cut).unwrap().seconds().unwrap();
    assert!((cut_seconds - 2.4).abs() < 0.02, "{cut_seconds}");

    // No stems named is the whole recording: the file that was studied, where it still is.
    let whole = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "from_seconds": 0.3, "to_seconds": 5.1, "at": 100.0, "as_recorded": true }),
    )
    .await;
    assert_eq!((&whole["file"], &whole["warp"]), (&json!(path), &json!("off")));
    // As recorded, 4.8 seconds are 9.6 beats of a song at 120.
    assert!((whole["beats"].as_f64().unwrap() - 9.6).abs() < 1e-6, "{whole}");

    // The song's key: asked for before the song says one, it is refused with what to do.
    let refused = fails(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "stems": ["vocals"], "from_bar": 1, "to_bar": 1, "to_song_key": true }),
    )
    .await;
    assert!(refused.starts_with("the song does not say what key it is in"), "{refused}");
    let heard = ok(client, "listen", json!({ "study": id })).await;
    println!("{}", heard["key"]);
    // The song is said to be a whole tone above what was read in the recording.
    let read = trill_core::SongKey::parse(heard["key"]["name"].as_str().unwrap()).unwrap();
    let above = trill_core::SongKey::new(read.tonic + 2, read.minor);
    assert_eq!(ok(client, "set_project", json!({ "key": above.name() })).await["key"], above.name());
    let keyed = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "stems": ["vocals"], "from_bar": 1, "to_bar": 1, "to_song_key": true }),
    )
    .await;
    assert_eq!(keyed["semitones"], 2.0, "{keyed}");
    assert!(keyed["said"][0].as_str().unwrap().starts_with("moved 2 semitones up from "), "{keyed}");
    // Drums have no key.
    let drums = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "stems": ["drums"], "from_bar": 1, "to_bar": 1, "to_song_key": true }),
    )
    .await;
    assert!(drums.get("semitones").is_none() && drums["said"][0] == "drums have no key: they are left at their pitch");
    // And the study's own key can be taken as the song's.
    assert_eq!(ok(client, "take_from_study", json!({ "study": id, "what": "key" })).await["key"], read.name());
    assert_eq!(ok(client, "get_project", json!({})).await["key"], read.name());

    // What cannot be: a stem the study has not got, bars past its end, a stretch as recorded and moved.
    let refused =
        fails(client, "take_from_study", json!({ "study": id, "what": "stretch", "stems": ["piano"], "from_bar": 1 }))
            .await;
    assert!(refused.ends_with("has no stem 'piano'; its stems are vocals, drums, bass, other"), "{refused}");
    let refused = fails(client, "take_from_study", json!({ "study": id, "what": "stretch", "from_bar": 40 })).await;
    assert!(refused.contains("bar 40 is past its end"), "{refused}");
    let refused = fails(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "from_bar": 1, "as_recorded": true, "semitones": 3 }),
    )
    .await;
    assert!(refused.starts_with("as recorded is the recording untouched"), "{refused}");
    let refused = fails(client, "take_from_study", json!({ "study": "nothing", "what": "tempo" })).await;
    println!("{refused}");
    drop(fixture.runtime.clone());
}

#[tokio::test(flavor = "multi_thread")]
async fn loops_lines_hits_and_a_kit_of_a_study_are_taken_into_the_song() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (id, _) = studied(&fixture).await;
    let study_folder = fixture.folder.path().join("config").join("studies").join(&id);

    // A loop, by the id listen gives it: the stems and the bars it names.
    let loops = ok(client, "listen", json!({ "study": id, "show": ["loops"] })).await;
    println!("{}", loops["loops"]);
    let first = &loops["loops"]["items"][0];
    let taken =
        ok(client, "take_from_study", json!({ "study": id, "what": "loop", "id": first["id"], "at_bar": 3 })).await;
    println!("{taken}");
    assert_eq!(taken["beats"].as_f64().unwrap(), first["bars"].as_f64().unwrap() * 4.0, "{taken}");
    assert!(taken["name"].as_str().unwrap().contains(" loop, "), "{taken}");
    let refused = fails(client, "take_from_study", json!({ "study": id, "what": "loop", "id": "none-1-1" })).await;
    assert!(refused.contains("has no loop 'none-1-1'; it has "), "{refused}");

    // A sung line, by its number or by a moment it is sung at: that stretch of the vocal, named by its
    // words, from bar line to bar line.
    let sung = ok(client, "listen", json!({ "study": id, "show": ["words"] })).await;
    assert_eq!(sung["lines"]["items"][1]["line"], 2);
    let line = ok(client, "take_from_study", json!({ "study": id, "what": "line", "line": 2, "at_bar": 9 })).await;
    println!("{line}");
    assert_eq!(
        (&line["name"], &line["words"], &line["line"]),
        (&json!("one two three four"), &json!("one two three four"), &json!(2))
    );
    assert_eq!(line["file"], study_folder.join("stems").join("vocals.wav").display().to_string());
    assert_eq!(line["beats"].as_f64().unwrap() % 4.0, 0.0, "{line}");
    let near = ok(client, "take_from_study", json!({ "study": id, "what": "line", "near_seconds": 5.5 })).await;
    assert_eq!(near["line"], 2, "{near}");
    // One word of it, exactly.
    let word = ok(client, "take_from_study", json!({ "study": id, "what": "word", "line": 2, "word": 3 })).await;
    assert_eq!(word["name"], "three");
    assert!(word["beats"].as_f64().unwrap() < 1.5, "{word}");
    let refused = fails(client, "take_from_study", json!({ "study": id, "what": "word", "line": 2, "word": 9 })).await;
    assert!(refused.starts_with("line 2 has 4 words (\"one two three four\")"), "{refused}");
    let refused = fails(client, "take_from_study", json!({ "study": id, "what": "line", "line": 12 })).await;
    assert!(refused.ends_with("has 4 lines; lines are counted from 1"), "{refused}");

    // A single hit onto a lane: a small file of its own, cut from the stem, as long as the hit.
    let shots = ok(client, "listen", json!({ "study": id, "show": ["one_shots"] })).await;
    println!("{}", shots["one_shots"]);
    let items = shots["one_shots"]["items"].as_array().unwrap();
    let hat = items.iter().find(|item| item.get("pitch").is_none()).expect("a drum hit");
    let bass = items.iter().find(|item| item.get("pitch").is_some()).expect("a bass note");
    let on_lane =
        ok(client, "take_from_study", json!({ "study": id, "what": "one_shot", "id": hat["id"], "at": 4.0 })).await;
    let file = PathBuf::from(on_lane["file"].as_str().unwrap());
    assert!(file.starts_with(study_folder.join("cuts")) && file.is_file(), "{on_lane}");
    let seconds = trill_media::probe(&file).unwrap().seconds().unwrap();
    let hit = hat["to_seconds"].as_f64().unwrap() - hat["from_seconds"].as_f64().unwrap();
    assert!((seconds - hit).abs() < 1e-3, "{seconds} against {hit}");
    assert_eq!(on_lane["label"], hat["label"]);

    // Onto a pad of a drum rack: the pad its kind has, or the one named.
    let rack_track = ok(client, "create_track", json!({ "name": "Rack" })).await["track_id"].clone();
    let rack =
        ok(client, "add_device", json!({ "track": rack_track, "uri": "builtin:drumrack" })).await["device_id"].clone();
    let padded =
        ok(client, "take_from_study", json!({ "study": id, "what": "one_shot", "id": hat["id"], "drum_rack": rack }))
            .await;
    println!("{padded}");
    let pad = padded["pad"].as_u64().unwrap();
    let other = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "one_shot", "id": hat["id"], "drum_rack": rack, "pad": 51 }),
    )
    .await;
    assert_eq!(other["pad"], 51);
    let device = ok(client, "get_device", json!({ "device": rack })).await;
    println!("{device}");
    let samples = device["samples"].as_array().unwrap();
    for note in [pad, 51] {
        let filled = samples.iter().find(|entry| entry["slot"] == format!("pad {note}")).unwrap();
        assert_eq!(filled["path"], on_lane["file"], "{device}");
    }

    // A bass note carries its pitch: a sampler made for it plays it unchanged on its own key.
    let sampled = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "one_shot", "id": bass["id"], "new_sampler": true }),
    )
    .await;
    println!("{sampled}");
    let note = bass["pitch"].as_u64().unwrap();
    assert_eq!(sampled["plays_unchanged_on"], trill_core::pitch::pitch_name(note as u8));
    let sampler = ok(client, "get_device", json!({ "device": sampled["device_id"] })).await;
    println!("{sampler}");
    let root = sampler["params"].as_array().unwrap().iter().find(|param| param["id"] == "root").unwrap();
    assert_eq!(root["value"].as_f64(), Some(note as f64), "{sampler}");
    assert_eq!(sampler["samples"][0]["path"], sampled["file"]);
    // And into a sampler that is there.
    let again = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "one_shot", "id": bass["id"], "sampler": sampled["device_id"] }),
    )
    .await;
    assert_eq!(again["device_id"], sampled["device_id"]);

    // The kit: the cleanest hit of each kind of drum that was found, as one drum rack on a track of its
    // own, in one step.
    let before = ok(client, "get_project", json!({})).await["tracks"].as_array().unwrap().len();
    let kit = ok(client, "take_from_study", json!({ "study": id, "what": "kit" })).await;
    println!("{kit}");
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["tracks"].as_array().unwrap().len(), before + 1);
    let made = project["tracks"].as_array().unwrap().iter().find(|track| track["id"] == kit["track_id"]).unwrap();
    assert_eq!(made["name"], "Kit (small song)");
    assert!(!kit["pads"].as_array().unwrap().is_empty(), "{kit}");
    let rack = ok(client, "get_device", json!({ "device": kit["device_id"] })).await;
    assert_eq!(rack["samples"].as_array().unwrap().len(), kit["pads"].as_array().unwrap().len(), "{rack}");
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(ok(client, "get_project", json!({})).await["tracks"].as_array().unwrap().len(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_parts_tempo_and_a_reference_of_a_study_are_taken_into_the_song() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (id, path) = studied(&fixture).await;

    // The bass line of two bars as notes, on a new track with an instrument.
    let bass = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "notes", "part": "bass", "from_bar": 1, "to_bar": 2, "at_bar": 2 }),
    )
    .await;
    println!("{bass}");
    assert_eq!((&bass["part"], &bass["beats"]), (&json!("Bass line"), &json!(8.0)));
    assert!(bass["notes"].as_u64().unwrap() >= 6 && bass.get("instrument_device_id").is_some(), "{bass}");
    // The groove on a track that plays drums: the kit is asked for each drum by its name.
    let drums = ok(client, "create_track", json!({ "name": "Drums" })).await["track_id"].clone();
    ok(client, "add_device", json!({ "track": drums, "uri": "builtin:drumkit" })).await;
    let groove = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "notes", "part": "groove", "from_bar": 1, "to_bar": 2, "track": drums }),
    )
    .await;
    println!("{groove}");
    assert_eq!((&groove["track_id"], &groove["beats"]), (&drums, &json!(8.0)));
    assert!(groove["notes"].as_u64().unwrap() >= 12 && groove.get("left_out").is_none(), "{groove}");
    // A part that plays pitches is not for a drum track.
    let refused =
        fails(client, "take_from_study", json!({ "study": id, "what": "notes", "part": "chords", "track": drums }))
            .await;
    assert!(refused.contains("plays drums: a chords is for an instrument that plays pitches"), "{refused}");

    // The parts as markers, the recording's first bar line on bar 3.
    let parts = ok(client, "take_from_study", json!({ "study": id, "what": "parts", "at_bar": 3 })).await;
    println!("{parts}");
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["markers"].as_array().unwrap().len(), parts["markers"].as_array().unwrap().len());
    assert_eq!(parts["markers"][0]["bar"], 3);

    // The tempo: the song is at 120, the recording at 100. Taken from bar 3, bars 3 to 10 of the song last
    // what the recording's bars last, and the song has its own tempo before.
    let tempo = ok(client, "take_from_study", json!({ "study": id, "what": "tempo", "at_bar": 3 })).await;
    println!("{tempo}");
    assert_eq!((&tempo["from_beat"], &tempo["beats_per_bar"]), (&json!(8.0), &json!(4)));
    let map = ok(client, "get_tempo_map", json!({})).await;
    println!("{map}");
    assert_eq!(ok(client, "get_project", json!({})).await["tempo"], 120.0);

    // The recording beside the song: a reference track on the file that was studied, never part of the
    // song's length, silent until it is listened to.
    let length = ok(client, "get_project", json!({})).await["length_beats"].clone();
    let reference = ok(client, "take_from_study", json!({ "study": id, "what": "reference", "at_bar": 3 })).await;
    println!("{reference}");
    assert_eq!(reference["file"], path);
    let project = ok(client, "get_project", json!({})).await;
    let track =
        project["tracks"].as_array().unwrap().iter().find(|track| track["id"] == reference["track_id"]).unwrap();
    assert_eq!(track["name"], "Reference: small song");
    assert_eq!(track["reference"], json!({ "listening": false, "rendered": false, "study": id }));
    assert_eq!(project["length_beats"], length, "a reference is no part of the song's length");
    assert!(project["references"].as_str().unwrap().starts_with("a track with `reference` is a recording beside"));
    // Its first bar line lies on bar 3: the clip begins what comes before it earlier.
    let clip = &track["clips"][0];
    println!("{clip}");
    // Listened to, and back to the song; made a track of the song again.
    let listening = ok(client, "update_track", json!({ "track": reference["track_id"], "listening": true })).await;
    assert_eq!(listening["reference"]["listening"], true);
    let refused = fails(client, "update_track", json!({ "track": drums, "listening": true })).await;
    assert!(refused.contains("is not a reference"), "{refused}");
    let plain = ok(client, "update_track", json!({ "track": reference["track_id"], "reference": false })).await;
    assert!(plain.get("reference").is_none(), "{plain}");
    assert!(ok(client, "get_project", json!({})).await["length_beats"].as_f64().unwrap() > length.as_f64().unwrap());
    ok(client, "history", json!({ "action": "undo" })).await;

    // With its tempo in the same step, from the start of the song.
    let with = ok(client, "take_from_study", json!({ "study": id, "what": "reference", "with_tempo": true })).await;
    println!("{with}");
    assert!((with["tempo"]["bpm"].as_f64().unwrap() - 100.0).abs() < 0.5, "{with}");
    assert!((ok(client, "get_project", json!({})).await["tempo"].as_f64().unwrap() - 100.0).abs() < 0.5);
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(ok(client, "get_project", json!({})).await["tempo"], 120.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_study_that_songs_play_says_which_before_it_goes_and_a_song_gathers_what_it_plays() {
    let fixture = connect().await;
    let client = &fixture.client;
    let (id, _) = studied(&fixture).await;
    let study_folder = fixture.folder.path().join("config").join("studies").join(&id);
    let songs = fixture.folder.path().join("songs");
    std::fs::create_dir_all(&songs).unwrap();

    // A song that plays a stem and a hit cut from one, saved.
    let stretch = ok(
        client,
        "take_from_study",
        json!({ "study": id, "what": "stretch", "stems": ["drums"], "from_bar": 3, "to_bar": 4, "at_bar": 2 }),
    )
    .await;
    let hit =
        ok(client, "take_from_study", json!({ "study": id, "what": "one_shot", "id": "hat-1", "new_sampler": true }))
            .await;
    // A file of a study is a file of the song like any other: it is listed, and found missing when it is gone.
    assert_eq!(ok(client, "missing_files", json!({})).await["missing"], 0);
    let first = songs.join("first.trill").display().to_string();
    ok(client, "save_project", json!({ "path": first })).await;

    // Asked to go, the study says who plays it, and stays. Its stems likewise: the clip plays one.
    let refused = fails(client, "study", json!({ "action": "delete", "study": id })).await;
    assert!(
        refused.starts_with(&format!(
            "files of the study {id} (small song) are played by the song that is open (first.trill, 2 places)"
        )),
        "{refused}"
    );
    assert!(refused.contains("Nothing was removed") && refused.contains("`confirm`"), "{refused}");
    let refused = fails(client, "study", json!({ "action": "drop_stems", "study": id })).await;
    assert!(
        refused.starts_with(&format!(
            "the stems of the study {id} (small song) are played by the song that is open (first.trill, 1 place)"
        )),
        "{refused}"
    );
    assert!(study_folder.join("stems").join("drums.wav").is_file());
    // A song that is not open is known by where it was saved.
    ok(client, "new_project", json!({})).await;
    let refused = fails(client, "study", json!({ "action": "delete", "study": id })).await;
    assert!(refused.contains(&format!("are played by {first} (2 places)")), "{refused}");

    // Gathered: what the song plays of the study lies in a folder beside it, cut to what it uses.
    ok(client, "load_project", json!({ "path": first })).await;
    let gathered = ok(client, "study", json!({ "action": "gather" })).await;
    assert_eq!((&gathered["gathered"], &gathered["clips"], &gathered["samples"]), (&json!(2), &json!(1), &json!(1)));
    let beside = songs.join("first files");
    assert_eq!(gathered["folder"], beside.display().to_string());
    let project = ok(client, "get_project", json!({})).await;
    let (clip, _) = clip_of(&project, &stretch["clip_ids"][0]);
    let cut = PathBuf::from(clip["path"].as_str().unwrap());
    assert!(cut.starts_with(&beside) && cut.is_file(), "{clip}");
    // Two bars at 100 beats a minute are 4.8 seconds, with a second to spare on each side.
    let seconds = trill_media::probe(&cut).unwrap().seconds().unwrap();
    assert!((seconds - 6.8).abs() < 0.01 && clip["offset_seconds"] == 1.0, "{seconds} {clip}");
    let sampler = ok(client, "get_device", json!({ "device": hit["device_id"] })).await;
    assert!(PathBuf::from(sampler["samples"][0]["path"].as_str().unwrap()).starts_with(&beside), "{sampler}");
    // One undo step; and done again it writes nothing new.
    ok(client, "history", json!({ "action": "undo" })).await;
    let before = ok(client, "get_project", json!({})).await;
    assert_eq!(clip_of(&before, &stretch["clip_ids"][0]).0["path"], stretch["file"]);
    ok(client, "history", json!({ "action": "redo" })).await;
    ok(client, "save_project", json!({})).await;
    assert_eq!(ok(client, "study", json!({ "action": "gather" })).await["gathered"], 0);

    // Now nothing plays the study: its stems are dropped and what was found is kept, and then it goes.
    let dropped = ok(client, "study", json!({ "action": "drop_stems", "study": id })).await;
    assert!(dropped["freed_mb"].as_u64().unwrap() >= 5, "{dropped}");
    assert!(!study_folder.join("stems").exists() && study_folder.join("heard.json").is_file());
    assert_eq!(ok(client, "listen", json!({ "study": id })).await["found"]["words"], 4);
    // A stretch of a stem that is gone is a sentence that says what to do.
    let gone =
        fails(client, "take_from_study", json!({ "study": id, "what": "stretch", "stems": ["drums"], "from_bar": 1 }))
            .await;
    assert!(
        gone.contains("is not there: its stems were dropped") && gone.contains("Studying the recording again"),
        "{gone}"
    );
    assert_eq!(ok(client, "missing_files", json!({})).await["missing"], 0);
    assert_eq!(ok(client, "study", json!({ "action": "delete", "study": id })).await["removed"], id);
    assert!(!study_folder.exists());
}
