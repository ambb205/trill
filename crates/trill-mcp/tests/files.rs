//! A song whose files are not where it says, the way an AI client meets
//! it: over the real MCP transport, with nothing but tool calls.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
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
    /// Holds the runtime's settings and library index, and the files a
    /// test makes. Nothing of the user's is read or written.
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

/// Writes `seconds` of a tone and returns the file's path as text.
fn tone(path: PathBuf, seconds: f64) -> String {
    let rate = 44_100.0;
    let channel: Vec<f32> = (0..(seconds * rate) as usize)
        .map(|n| (n as f64 * 220.0 * std::f64::consts::TAU / rate).sin() as f32 * 0.5)
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(&path, &AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }, WavFormat::Int16)
        .unwrap();
    path.display().to_string()
}

struct Song {
    file: String,
    riser: String,
    kick: String,
    fx: u64,
    clips: [u64; 2],
    rack: u64,
}

/// A pack at `packs` (a riser, a kick, and a file nobody uses) and a song
/// that plays the riser in two clips on "FX" and the kick on a drum rack
/// on "Kit", saved.
async fn song(fixture: &Fixture, packs: &Path) -> Song {
    let client = &fixture.client;
    let riser = tone(packs.join("FX").join("Riser 2 Bars.wav"), 2.0);
    let kick = tone(packs.join("Kit").join("Kicks").join("Kick 01.wav"), 0.25);
    tone(packs.join("Kit").join("Snare.wav"), 0.2);
    ok(client, "set_project", json!({ "tempo": 120, "name": "Night" })).await;
    let fx = ok(client, "create_track", json!({ "name": "FX" })).await["track_id"].as_u64().unwrap();
    let first =
        ok(client, "create_audio_clip", json!({ "track": fx, "path": riser, "start": 0, "name": "Riser" })).await;
    let again = json!({ "track": fx, "path": riser, "start": 8, "length": 2, "name": "Riser again", "gain_db": -6 });
    let second = ok(client, "create_audio_clip", again).await;
    let kit = ok(client, "create_track", json!({ "name": "Kit", "instrument": "builtin:drumrack" })).await;
    let rack = kit["instrument_device_id"].as_u64().unwrap();
    ok(client, "set_sample", json!({ "device": rack, "pad": "kick", "path": kick })).await;
    let file = fixture.folder.path().join("Songs").join("night.trill").display().to_string();
    ok(client, "save_project", json!({ "path": file })).await;
    let clips = [first["clip_id"].as_u64().unwrap(), second["clip_id"].as_u64().unwrap()];
    Song { file, riser, kick, fx, clips, rack }
}

fn move_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(from, to).unwrap();
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

/// The peak of the track `id` in a measurement, in dB.
fn peak_of(measured: &Value, id: u64) -> f64 {
    let tracks = measured["tracks"].as_array().expect("a measurement lists the tracks");
    tracks.iter().find(|track| track["track"] == id).and_then(|track| track["peak_db"].as_f64()).unwrap_or(f64::NAN)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_song_with_missing_files_says_so_wherever_it_is_looked_at_and_is_put_right_in_one_call() {
    let fixture = connect().await;
    let client = &fixture.client;
    let old = fixture.folder.path().join("Music").join("packs");
    // Far from where it was, so that nothing finds it by looking nearby.
    let new = fixture.folder.path().join("Other").join("a").join("b").join("c").join("d").join("packs");
    let song = song(&fixture, &old).await;
    let whole = ok(client, "measure", json!({})).await;
    assert!(whole["missing_files"].is_null() && peak_of(&whole, song.fx) > -20.0, "{whole}");
    assert!(ok(client, "get_project", json!({})).await["missing_files"].is_null());
    let nothing = ok(client, "missing_files", json!({})).await;
    assert_eq!((&nothing["missing"], &nothing["next"]), (&json!(0), &json!("No file of the song is missing.")));

    // The folder is moved, and the song opened again.
    move_folder(&old, &new);
    let opened = ok(client, "load_project", json!({ "path": song.file })).await;
    let brief = &opened["missing_files"];
    assert_eq!((&brief["files"], &brief["places"], &brief["tracks"]), (&json!(2), &json!(3), &json!(["FX", "Kit"])));
    let note = brief["note"].as_str().unwrap();
    assert!(
        note.starts_with("2 audio files the song names are not on disk, so 3 places on FX and Kit play nothing."),
        "{note}"
    );
    assert!(note.contains("missing_files") && note.contains("relink_files"));
    // Said once: not again for every clip and pad.
    assert!(opened["problems"].is_null(), "{}", opened["problems"]);

    // A measurement of it is not handed back as a quieter mix.
    let without = ok(client, "measure", json!({})).await;
    let notes = without["notes"].as_array().unwrap();
    let first = notes[0].as_str().unwrap();
    assert!(
        first.starts_with("2 audio files are missing, so 3 places on FX and Kit played nothing: what was measured"),
        "{first}"
    );
    assert_eq!(without["missing_files"]["files"], 2);
    assert_eq!(without["problems"], json!([]), "the lines for each clip give way to that");
    println!("FX without its files: {} dB, with them: {} dB", peak_of(&without, song.fx), peak_of(&whole, song.fx));
    assert!(peak_of(&without, song.fx) <= -90.0, "{without}");
    // Heard alone, a track is told of its own files only.
    let kit = ok(client, "get_project", json!({})).await["tracks"][1]["id"].as_u64().unwrap();
    let alone = ok(client, "measure", json!({ "tracks": [kit] })).await;
    assert_eq!((&alone["missing_files"]["files"], &alone["missing_files"]["tracks"]), (&json!(1), &json!(["Kit"])));

    // Looked for: nobody knows where the folder went.
    let lost = ok(client, "missing_files", json!({})).await;
    assert_eq!(lost["found"], json!({ "sure": 0, "likely": 0, "unsure": 0, "not_found": 2 }));
    assert!(lost["next"].as_str().unwrap().starts_with("Nothing was found. Ask the user"), "{lost}");
    let riser = &lost["files"][0];
    assert_eq!((&riser["file"], &riser["found"]), (&json!(song.riser), &Value::Null));
    assert_eq!(
        riser["used_by"],
        json!([
            { "clip": song.clips[0], "name": "Riser", "track": "FX" },
            { "clip": song.clips[1], "name": "Riser again", "track": "FX" },
        ])
    );
    assert_eq!(
        lost["files"][1]["used_by"],
        json!([{ "device": song.rack, "slot": "pad 36", "name": "Drum Rack", "track": "Kit" }])
    );
    assert_eq!((&riser["was"]["seconds"], &riser["was"]["sample_rate"]), (&json!(2.0), &json!(44_100)));
    let nothing = ok(client, "relink_files", json!({})).await;
    assert_eq!(nothing["relinked"], json!({ "files": 0, "places": 0 }));
    assert!(nothing["note"].as_str().unwrap().contains("nothing was found for the missing files"), "{nothing}");

    // The library is pointed at where the samples are now.
    ok(client, "library", json!({ "action": "add_folder", "path": text(new.parent().unwrap()) })).await;
    let found = ok(client, "missing_files", json!({})).await;
    assert_eq!(found["found"], json!({ "sure": 2, "likely": 0, "unsure": 0, "not_found": 0 }));
    assert_eq!(found["found_in"], json!(text(&new)));
    assert_eq!(found["folders_moved"], json!([{ "from": text(&old), "to": text(&new), "files": 2 }]));
    let riser = &found["files"][0]["found"];
    assert_eq!(riser["path"], json!(text(&new.join("FX").join("Riser 2 Bars.wav"))));
    assert_eq!((&riser["sure"], &riser["seconds"]), (&json!("sure"), &json!(2.0)));
    let why = riser["why"].as_str().unwrap();
    assert!(why.starts_with("where its folder went, like 1 other file of the song"), "{why}");
    assert!(found["next"].as_str().unwrap().starts_with("All 2 were found for sure"), "{found}");
    // Looking relinks nothing.
    assert_eq!(ok(client, "get_project", json!({})).await["missing_files"]["places"], 3);

    // One call, one step.
    let relinked = ok(client, "relink_files", json!({})).await;
    assert_eq!(relinked["relinked"], json!({ "files": 2, "places": 3 }));
    assert_eq!(relinked["still_missing"], 0);
    assert_eq!(relinked["clips"], json!(song.clips));
    assert!(relinked["missing_files"].is_null());
    let after = ok(client, "get_project", json!({})).await;
    assert!(after["missing_files"].is_null() && after["problems"].is_null(), "{after}");
    let back = ok(client, "measure", json!({})).await;
    assert!(back["missing_files"].is_null());
    assert_eq!(peak_of(&back, song.fx), peak_of(&whole, song.fx), "the track is back as it was");
    assert!(!back["notes"].as_array().unwrap().iter().any(|note| note.as_str().unwrap().contains("missing")));

    // It is in the journal under its words, with the clips it landed on.
    let journal = ok(client, "get_journal", json!({ "last": 1 })).await;
    let step = &journal["entries"][0];
    assert_eq!(step["did"], "Relinked the song's missing files", "{journal}");
    assert_eq!(step["tool"], "relink_files");

    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(
        ok(client, "get_project", json!({})).await["missing_files"]["places"],
        3,
        "one step took all of it back"
    );
    ok(client, "history", json!({ "action": "redo" })).await;
    assert!(ok(client, "get_project", json!({})).await["missing_files"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn files_are_relinked_by_hand_and_what_is_not_sure_is_not_taken_unasked() {
    let fixture = connect().await;
    let client = &fixture.client;
    let old = fixture.folder.path().join("Music").join("packs");
    let new = fixture.folder.path().join("Other").join("a").join("b").join("c").join("d").join("Renamed");
    let song = song(&fixture, &old).await;
    move_folder(&old, &new);
    ok(client, "load_project", json!({ "path": song.file })).await;
    let (new_riser, new_kick) =
        (text(&new.join("FX").join("Riser 2 Bars.wav")), text(&new.join("Kit").join("Kicks").join("Kick 01.wav")));

    // What cannot be done says what to do, and changes nothing.
    let not_audio = fixture.folder.path().join("notes.txt");
    std::fs::write(&not_audio, "words").unwrap();
    let nowhere = text(&fixture.folder.path().join("Nowhere"));
    let refused = [
        (json!({ "look_in": [nowhere] }), "is not a folder on this machine"),
        (json!({ "folder": { "from": text(&old), "to": nowhere } }), "is not a folder on this machine"),
        (json!({ "folder": { "from": "", "to": text(&new) } }), "needs both `from`"),
        (json!({ "files": [{ "file": "C:/never/named.wav", "to": new_kick }] }), "the song names no file"),
        (json!({ "files": [{ "to": new_kick }] }), "has to name one thing"),
        (json!({ "files": [{ "clip": song.clips[0], "file": song.riser, "to": new_kick }] }), "has to name one thing"),
        (json!({ "files": [{ "device": song.rack, "to": new_kick }] }), "say which of its samples with `slot`"),
        (json!({ "files": [{ "clip": song.clips[0], "to": " " }] }), "needs `to`"),
        (json!({ "files": [{ "clip": 9999, "to": new_kick }] }), "clip 9999 not found. Existing clips:"),
        (json!({ "files": [{ "device": song.rack, "slot": "pad 40", "to": new_kick }] }), "its files are in: pad 36"),
        (json!({ "files": [{ "clip": song.clips[0], "to": text(&not_audio) }] }), "nothing was relinked"),
        (
            json!({ "files": [{ "clip": song.clips[0], "to": text(&new.join("FX").join("gone.wav")) }] }),
            "nothing was relinked",
        ),
    ];
    for (arguments, expected) in refused {
        let error = call(client, "relink_files", arguments.clone()).await.unwrap_err();
        assert!(error.contains(expected), "{arguments}: '{error}' should say '{expected}'");
        assert_eq!(ok(client, "get_project", json!({})).await["missing_files"]["places"], 3, "{arguments}");
    }
    assert_eq!(
        call(client, "missing_files", json!({ "look_in": [text(&not_audio)] })).await.unwrap_err(),
        format!("'{}' in `look_in` is not a folder on this machine", not_audio.display())
    );

    // One clip, by hand: the other places keep waiting.
    let one = ok(client, "relink_files", json!({ "files": [{ "clip": song.clips[0], "to": new_riser }] })).await;
    assert_eq!((&one["relinked"], &one["still_missing"]), (&json!({ "files": 1, "places": 1 }), &json!(2)));
    assert_eq!(one["missing_files"]["places"], 2);
    let journal = ok(client, "get_journal", json!({ "last": 1 })).await;
    assert_eq!(journal["entries"][0]["did"], "Relinked \u{201C}Riser 2 Bars.wav\u{201D}");
    ok(client, "history", json!({ "action": "undo" })).await;

    // A folder that was named: what was under it is relinked where it is
    // under the new one, and nothing else is looked for.
    let named = ok(client, "relink_files", json!({ "folder": { "from": text(&old), "to": text(&new) } })).await;
    assert_eq!((&named["relinked"], &named["still_missing"]), (&json!({ "files": 2, "places": 3 }), &json!(0)));
    assert_eq!(named["now"], json!([new_riser, new_kick]));
    ok(client, "history", json!({ "action": "undo" })).await;

    // A file by its old path: every place that names it.
    let by_file = ok(client, "relink_files", json!({ "files": [{ "file": song.riser, "to": new_riser }] })).await;
    assert_eq!(by_file["relinked"], json!({ "files": 1, "places": 2 }));
    // And a pad, by its device and slot.
    let pad = json!({ "files": [{ "device": song.rack, "slot": "pad 36", "to": new_kick }] });
    assert_eq!(ok(client, "relink_files", pad).await["still_missing"], 0);
    ok(client, "history", json!({ "action": "undo", "steps": 2 })).await;

    // A folder to look in: the files are in it under their names, and
    // where they are says where their folder went (renamed and all).
    let looked = ok(client, "missing_files", json!({ "look_in": [text(&fixture.folder.path().join("Other"))] })).await;
    assert_eq!(looked["found"]["sure"], 2, "{looked}");
    assert_eq!(looked["folders_moved"][0], json!({ "from": text(&old), "to": text(&new), "files": 2 }));
    ok(client, "history", json!({ "action": "undo" })).await;

    // A kick of the same name that is another kick, in the library: found,
    // said not to be the file, and not taken unless asked for.
    std::fs::remove_dir_all(&new).unwrap();
    let library = fixture.folder.path().join("Library");
    tone(library.join("Kick 01.wav"), 0.6);
    tone(library.join("Riser 2 Bars.wav"), 2.0);
    ok(client, "library", json!({ "action": "add_folder", "path": text(&library) })).await;
    let found = ok(client, "missing_files", json!({})).await;
    assert_eq!(found["found"], json!({ "sure": 1, "likely": 0, "unsure": 1, "not_found": 0 }), "{found}");
    let kick = &found["files"][1]["found"];
    assert_eq!(kick["sure"], "unsure");
    assert_eq!(
        kick["why"],
        "the only file of that name in the library, but it is not the file the song knew: 0.60 s long where that was 0.25 s"
    );
    assert!(found["next"].as_str().unwrap().starts_with("relink_files with no arguments relinks the 1 that is sure"));
    let sure = ok(client, "relink_files", json!({})).await;
    assert_eq!((&sure["relinked"], &sure["still_missing"]), (&json!({ "files": 1, "places": 2 }), &json!(1)));
    assert_eq!(sure["missing_files"]["tracks"], json!(["Kit"]));
    let none = ok(client, "relink_files", json!({ "found": "likely" })).await;
    assert_eq!(none["relinked"]["places"], 0);
    assert!(none["note"].as_str().unwrap().contains("1 was found, none for sure"), "{none}");
    // Asked for by name, it is taken.
    let all = ok(client, "relink_files", json!({ "found": "all" })).await;
    assert_eq!((&all["relinked"]["places"], &all["still_missing"]), (&json!(1), &json!(0)));
    let _ = song.kick;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_library_says_which_of_its_folders_is_missing_and_goes_along_with_the_songs_files() {
    let fixture = connect().await;
    let client = &fixture.client;
    let old = fixture.folder.path().join("Music").join("packs");
    let new = fixture.folder.path().join("Music").join("trill_samples").join("packs");
    let song = song(&fixture, &old).await;
    let added = ok(client, "library", json!({ "action": "add_folder", "path": text(&old) })).await;
    assert_eq!((&added["files"], &added["missing_folders"]), (&json!(3), &Value::Null));
    fixture.runtime.library.set_favorite(Path::new(&song.kick), true);

    move_folder(&old, &new);
    let status = ok(client, "library", json!({ "action": "rescan" })).await;
    let missing = &status["missing_folders"][0];
    assert_eq!((&missing["folder"], &missing["files_known"]), (&json!(text(&old)), &json!(3)));
    assert!(missing["why"].is_string());
    let hint = status["hint"].as_str().unwrap();
    assert!(hint.contains("is not where it was") && hint.contains("move_folder"), "{hint}");
    assert_eq!(status["files"], 3, "what was known of it is still listed");

    // The folder is found near where it was, by the files the library
    // knew in it, and the song's files with it.
    ok(client, "load_project", json!({ "path": song.file })).await;
    let found = ok(client, "missing_files", json!({})).await;
    assert_eq!(
        found["library"],
        json!([{
            "folder": text(&old),
            "files_known": 3,
            "found_at": text(&new),
            "sure": true,
            "why": "3 of 3 of its files that were looked for are there",
        }])
    );
    assert_eq!(found["found"]["sure"], 2);
    let why = found["files"][0]["found"]["why"].as_str().unwrap();
    assert!(why.starts_with("where the library's folder went"), "{why}");

    // Relinking the song puts the library right in the same call.
    let relinked = ok(client, "relink_files", json!({})).await;
    assert_eq!(relinked["relinked"], json!({ "files": 2, "places": 3 }));
    assert_eq!(relinked["library_moved"], json!([{ "from": text(&old), "to": text(&new), "favorites": 1 }]));
    assert!(fixture.runtime.library.wait_until_idle(Duration::from_secs(60)));
    let status = ok(client, "library", json!({ "action": "status" })).await;
    assert_eq!((&status["folders"], &status["missing_folders"]), (&json!([text(&new)]), &Value::Null));
    assert!(fixture.runtime.library.is_favorite(&new.join("Kit").join("Kicks").join("Kick 01.wav")));
    assert_eq!(ok(client, "missing_files", json!({})).await["library"], Value::Null);

    // By hand: moved again, and said where.
    let third = fixture.folder.path().join("Third").join("packs");
    move_folder(&new, &third);
    let refused = [
        (json!({ "action": "move_folder", "path": text(&new) }), "move_folder needs `to`"),
        (json!({ "action": "move_folder", "to": text(&third) }), "move_folder needs `path`"),
        (
            json!({ "action": "move_folder", "path": text(&old), "to": text(&third) }),
            "is not a folder of the library; its folders are:",
        ),
        (json!({ "action": "move_folder", "path": text(&new), "to": text(&old) }), "the library is as it was"),
    ];
    for (arguments, expected) in refused {
        let error = call(client, "library", arguments.clone()).await.unwrap_err();
        assert!(error.contains(expected), "{arguments}: '{error}' should say '{expected}'");
    }
    let moved = ok(client, "library", json!({ "action": "move_folder", "path": text(&new), "to": text(&third) })).await;
    assert_eq!(
        moved["moved"],
        json!({ "from": text(&new), "to": text(&third), "files_known": 3, "favorites": 1, "recent": 1 })
    );
    assert_eq!(
        (&moved["folders"], &moved["files"], &moved["missing_folders"]),
        (&json!([text(&third)]), &json!(3), &Value::Null)
    );
    // It is in the settings, for the next run.
    let saved = std::fs::read_to_string(fixture.folder.path().join("config").join("settings.json")).unwrap();
    assert!(saved.contains("Third") && !saved.contains("trill_samples"), "{saved}");
}
