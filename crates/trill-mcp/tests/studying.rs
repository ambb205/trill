//! A recording taken apart, and what Trill fetches, the way an AI client
//! asks: over the real MCP transport, with nothing but tool calls.
//!
//! The run is the real one, in a process of its own (this crate's own
//! program, started a second time, as it starts itself), over the real
//! files; the models are scripted ones, and what is fetched comes from a
//! shelf of the test's own. Nothing is fetched and no graphics card is
//! asked.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use trill_core::audio::AudioBuffer;
use trill_engine::AudioBackend;
use trill_fetch::scripted::{Shelf, part};
use trill_mcp::{HttpOptions, HttpServer, serve_http_with};
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage, Studying};
use trill_study::models::scripted::Script;
use trill_study::run::Programs;

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    client: Client,
    /// The same server through the door of the assistant in the window.
    assistant: Client,
    shelf: Arc<Shelf>,
    _runtime: Arc<Runtime>,
    _server: HttpServer,
    /// Holds the runtime's settings, its studies, what it fetched and the
    /// files a test makes. Nothing of the user's is read or written.
    folder: tempfile::TempDir,
}

fn words() -> Vec<String> {
    ["one", "two", "three", "four"].map(String::from).to_vec()
}

async fn connect_with(script: Script, programs: Programs) -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let storage = Storage::At {
        settings: folder.path().join("config").join("settings.json"),
        library_index: Some(folder.path().join("cache").join("library-index.json")),
    };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let shelf = Arc::new(Shelf::default());
    let studying = Studying {
        fetcher: Some(shelf.clone()),
        parts: Some(vec![
            part(&shelf, "stems", "Stems", &[("stems.onnx", vec![3u8; 3 << 20])]),
            part(&shelf, "beats", "Beats", &[("beats.onnx", vec![4u8; 1 << 20])]),
        ]),
        stand_in: Some(script),
        programs: Some(programs),
        // This crate's own program, which studies when it is started for that.
        program: Some(PathBuf::from(env!("CARGO_BIN_EXE_trill-mcp"))),
        free: None,
    };
    let runtime = Arc::new(Runtime::start_custom(options, Parts { studying, ..Parts::default() }).unwrap());
    let options = HttpOptions { assistant_folder: Some(folder.path().join("work")) };
    let server = serve_http_with(runtime.clone(), "127.0.0.1:0".parse().unwrap(), options).unwrap();
    let client_of = |url: String| async move {
        let transport = StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(url));
        ClientConfig::default().serve(transport).await.unwrap()
    };
    let (client, assistant) = (client_of(server.url()).await, client_of(server.assistant_url()).await);
    Fixture { client, assistant, shelf, _runtime: runtime, _server: server, folder }
}

async fn connect() -> Fixture {
    connect_with(Script { words: words(), ..Script::default() }, Programs::default()).await
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
async fn studied(fixture: &Fixture) -> String {
    let path = recording(&fixture.folder.path().join("refs").join("small song.wav"));
    let started = ok(&fixture.client, "study", json!({ "action": "start", "path": path })).await;
    let id = started["study"].as_str().unwrap().to_owned();
    let done = ok(&fixture.client, "study", json!({ "action": "report", "study": id, "wait_seconds": 120 })).await;
    assert_eq!(done["state"], "done", "{done}");
    id
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_is_studied_and_what_was_found_is_read_with_listen() {
    let fixture = connect().await;
    let client = &fixture.client;
    let path = recording(&fixture.folder.path().join("refs").join("small song.wav"));

    // Asked for, it answers at once with the study's id and what to do next.
    let started = ok(client, "study", json!({ "action": "start", "path": path })).await;
    let id = started["study"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 10);
    assert_eq!((&started["title"], &started["of"]), (&json!("small song"), &json!(path)));
    assert!(["waiting", "running"].contains(&started["state"].as_str().unwrap()), "{started}");
    assert!(started["next"].as_str().unwrap().contains("report"), "{started}");
    // Read before it is done, it says so and how to wait.
    if let Err(early) = call(client, "listen", json!({ "study": id })).await {
        assert!(
            early.starts_with(&format!("study {id} is not done yet (")) && early.contains("wait_seconds"),
            "{early}"
        );
    }

    // The report, waited for, is the whole of it: every stage, what ran the models, the stems as files.
    let done = ok(client, "study", json!({ "action": "report", "wait_seconds": 120 })).await;
    assert_eq!((&done["study"], &done["state"], &done["ran_on"]), (&json!(id), &json!("done"), &json!("a script")));
    assert!((done["seconds"].as_f64().unwrap() - 20.1).abs() < 0.01, "{done}");
    let stages: Vec<&str> =
        done["stages"].as_array().unwrap().iter().map(|stage| stage["stage"].as_str().unwrap()).collect();
    assert_eq!(stages, ["read", "stems", "beats", "words", "word_times", "listen"]);
    assert!(done["stages"].as_array().unwrap().iter().all(|stage| stage["state"] == "done"), "{done}");
    let stems = done["stems"].as_array().unwrap();
    assert_eq!(
        stems.iter().map(|stem| stem["name"].as_str().unwrap()).collect::<Vec<_>>(),
        ["vocals", "drums", "bass", "other"]
    );
    assert!(stems.iter().all(|stem| Path::new(stem["path"].as_str().unwrap()).is_file()));
    assert!(
        done["size_mb"].as_u64().unwrap() >= 5 && done.get("at").is_none() && done.get("error").is_none(),
        "{done}"
    );
    // It is kept beside the settings, in a folder of its own, and nowhere of the user's.
    assert!(
        stems[0]["path"]
            .as_str()
            .unwrap()
            .starts_with(&fixture.folder.path().join("config").join("studies").join(&id).display().to_string())
    );

    // listen reads it: the same summary as of a file, heard in the stems, with the words.
    let heard = ok(client, "listen", json!({ "study": id })).await;
    assert_eq!((&heard["study"], &heard["title"]), (&json!(id), &json!("small song")));
    assert!((heard["tempo"]["bpm"].as_f64().unwrap() - 100.0).abs() < 0.7, "{heard}");
    assert_eq!(heard["tempo"]["beats_per_bar"], 4);
    assert!(
        heard["heard_as"]
            .as_str()
            .unwrap()
            .starts_with("the stems a model split the recording into (vocals, drums, bass, other)")
    );
    assert_eq!(heard["found"]["words"], 4);
    assert_eq!(heard["words"], json!({ "lines": 4, "language": "en", "each_word_placed": true }));
    assert!(heard["found"]["bass"].as_u64().unwrap() >= 24 && heard["found"]["loops"].as_u64().unwrap() > 0, "{heard}");
    assert_eq!(heard["stems"], done["stems"]);
    assert!(heard.get("file").is_none() && heard.get("stem").is_none());

    // The words, line by line, each word with its bar and its beat.
    let sung = ok(client, "listen", json!({ "study": id, "show": ["words"] })).await;
    let lines = sung["lines"]["items"].as_array().unwrap();
    assert_eq!((sung["lines"]["total"].as_u64(), lines.len()), (Some(4), 4));
    // Where a word is, in beats from the first bar line. (Without a kick to say where the bar
    // begins, the scripted tracker may have the line a beat off: what is held is how the words lie.)
    let at = |word: &Value| (word[0].as_f64().unwrap() - 1.0) * 4.0 + word[1].as_f64().unwrap() - 1.0;
    let mut starts = Vec::new();
    for line in lines {
        assert_eq!(line["text"], "one two three four");
        let words = line["words"].as_array().unwrap();
        assert_eq!(
            words.iter().map(|word| word[2].as_str().unwrap()).collect::<Vec<_>>(),
            ["one", "two", "three", "four"]
        );
        // A word to the beat, and the line where its first word is.
        for pair in words.windows(2) {
            assert!((at(&pair[1]) - at(&pair[0]) - 1.0).abs() < 0.2, "{line}");
        }
        assert_eq!((&line["bar"], &line["beat"]), (&words[0][0], &words[0][1]), "{line}");
        assert!((line["beats"].as_f64().unwrap() - 4.0).abs() < 0.5, "{line}");
        starts.push(at(&words[0]));
    }
    // The voice sings in every other bar.
    assert!(starts.windows(2).all(|pair| (pair[1] - pair[0] - 8.0).abs() < 0.2), "{starts:?}");
    // Kept to a stretch of bars, as every list is.
    let third = lines[2]["bar"].as_i64().unwrap();
    let later = ok(client, "listen", json!({ "study": id, "show": ["words"], "from_bar": third })).await;
    assert_eq!(later["lines"]["total"], 2);

    // One stem: where it plays, its own file, and only what is its own.
    let drums = ok(client, "listen", json!({ "study": id, "stem": "drums", "show": ["loops", "one_shots"] })).await;
    assert_eq!(drums["stem"]["name"], "drums");
    assert_eq!(drums["stem"]["path"], stems[1]["path"]);
    assert!(!drums["stem"]["plays_in_bars"].as_array().unwrap().is_empty(), "{drums}");
    assert!(
        drums["loops"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|found| found["id"].as_str().unwrap().starts_with("drums-")),
        "{drums}"
    );
    assert!(
        drums["one_shots"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|shot| !shot["id"].as_str().unwrap().starts_with("bass-"))
    );
    let voice = ok(client, "listen", json!({ "study": id, "stem": "vocals" })).await;
    let sings: Vec<(i64, i64)> = voice["stem"]["plays_in_bars"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| (pair[0].as_i64().unwrap(), pair[1].as_i64().unwrap()))
        .collect();
    // It sings in four stretches, a bar each, with a bar's rest between.
    assert_eq!(sings.len(), 4, "{voice}");
    assert!(sings.iter().all(|(from, to)| to - from <= 1), "{sings:?}");
    assert!(sings.windows(2).all(|pair| pair[1].0 - pair[0].0 == 2), "{sings:?}");
    let none = fails(client, "listen", json!({ "study": id, "stem": "strings" })).await;
    assert_eq!(none, "the study has no stem 'strings'. Stems: vocals, drums, bass, other");

    // What was heard in it is written as MIDI, as of a file.
    let midi = fixture.folder.path().join("bass.mid");
    let written =
        ok(client, "transcribe", json!({ "study": id, "part": "bass", "file": midi.display().to_string() })).await;
    assert!(written["notes"].as_u64().unwrap() >= 24 && midi.is_file(), "{written}");
    // And a stem goes into the song like any sound.
    let track = ok(client, "create_track", json!({ "name": "Their drums" })).await["track_id"].clone();
    let added = ok(client, "create_audio_clip", json!({ "track": track, "path": stems[1]["path"], "start": 0 })).await;
    assert!(added["clip_id"].as_u64().is_some(), "{added}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grid_corrected_for_a_study_is_kept_with_it() {
    let fixture = connect().await;
    let id = studied(&fixture).await;
    let client = &fixture.client;
    let half = ok(client, "listen", json!({ "study": id, "grid": ["half"] })).await;
    assert!((half["tempo"]["bpm"].as_f64().unwrap() - 50.0).abs() < 0.4, "{half}");
    assert_eq!(half["grid"], json!(["half"]));
    // Asked again with nothing said of the grid, it is as it was corrected.
    let again = ok(client, "listen", json!({ "study": id, "show": ["words"] })).await;
    assert!((again["tempo"]["bpm"].as_f64().unwrap() - 50.0).abs() < 0.4, "{again}");
    assert_eq!(again["lines"]["total"], 4);
    assert_eq!(ok(client, "study", json!({ "action": "report", "study": id })).await["grid"], json!(["half"]));
    // No model was asked: the stages before the listening are as they were.
    let report = ok(client, "study", json!({ "action": "report", "study": id })).await;
    let stems = report["stages"].as_array().unwrap().iter().find(|stage| stage["stage"] == "stems").unwrap();
    assert_eq!(stems["note"], "already done");
    // And taken off again.
    let plain = ok(client, "listen", json!({ "study": id, "grid": ["none"] })).await;
    assert!((plain["tempo"]["bpm"].as_f64().unwrap() - 100.0).abs() < 0.7 && plain.get("grid").is_none(), "{plain}");
    let wrong = fails(client, "listen", json!({ "study": id, "grid": ["sideways"] })).await;
    assert!(wrong.starts_with("'sideways' is not a correction to a beat grid"), "{wrong}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_study_is_stopped_taken_up_where_it_was_listed_and_removed() {
    let fixture = connect_with(Script { words: words(), seconds: 1.5, ..Script::default() }, Programs::default()).await;
    let client = &fixture.client;
    let path = recording(&fixture.folder.path().join("small song.wav"));
    let id = ok(client, "study", json!({ "action": "start", "path": path, "title": "Stopped and gone on with" })).await
        ["study"]
        .as_str()
        .unwrap()
        .to_owned();
    // While it runs it says the stage it is at; it is stopped once the stems are done.
    let mut seen = Value::Null;
    for _ in 0..600 {
        let report = ok(client, "study", json!({ "action": "report", "study": id })).await;
        if report["at"]["stage"] == "beats" {
            seen = report;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!((&seen["state"], &seen["at"]["doing"]), (&json!("running"), &json!("Finding the beat")), "{seen}");
    assert!(seen["at"]["percent"].as_f64().is_some());
    let listed = ok(client, "study", json!({ "action": "list" })).await;
    assert_eq!(listed["studies"][0]["title"], "Stopped and gone on with");
    assert!(listed["studies"][0]["at"].as_str().unwrap().starts_with("finding the beat, "), "{listed}");

    let stopped = ok(client, "study", json!({ "action": "stop", "study": id })).await;
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    assert_eq!(stopped["stems"].as_array().unwrap().len(), 4);
    assert!(stopped["next"].as_str().unwrap().contains("takes it up where it was"));
    let unread = fails(client, "listen", json!({ "study": id })).await;
    assert_eq!(
        unread,
        format!(
            "study {id} was stopped before it was done: study with action \"start\" and study \"{id}\" takes it up where it was"
        )
    );
    assert_eq!(
        fails(client, "study", json!({ "action": "stop", "study": id })).await,
        format!("study {id} is not running")
    );

    ok(client, "study", json!({ "action": "start", "study": id })).await;
    let done = ok(client, "study", json!({ "action": "report", "study": id, "wait_seconds": 120 })).await;
    assert_eq!(done["state"], "done", "{done}");
    let stems = done["stages"].as_array().unwrap().iter().find(|stage| stage["stage"] == "stems").unwrap();
    assert_eq!(stems["note"], "already done");

    let removed = ok(client, "study", json!({ "action": "delete", "study": id })).await;
    assert_eq!(removed, json!({ "removed": id, "title": "Stopped and gone on with" }));
    assert_eq!(ok(client, "study", json!({ "action": "list" })).await["studies"], json!([]));
    assert!(!fixture.folder.path().join("config").join("studies").join(&id).exists());
    let gone = fails(client, "listen", json!({ "study": id })).await;
    assert_eq!(gone, format!("there is no study '{id}', and none has been made yet"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_that_fails_or_takes_its_process_down_is_a_sentence_and_the_server_goes_on() {
    use trill_study::Stage;
    let fixture =
        connect_with(Script { words: words(), crash_at: Some(Stage::Beats), ..Script::default() }, Programs::default())
            .await;
    let client = &fixture.client;
    let path = recording(&fixture.folder.path().join("small song.wav"));
    let id =
        ok(client, "study", json!({ "action": "start", "path": path })).await["study"].as_str().unwrap().to_owned();
    let failed = ok(client, "study", json!({ "action": "report", "study": id, "wait_seconds": 120 })).await;
    assert_eq!(failed["state"], "failed", "{failed}");
    let error = failed["error"].as_str().unwrap();
    // Windows ends a process that aborts with a code; Unix with a signal, and no code.
    let how = if cfg!(windows) { "(it ended with code 0x" } else { "(it was ended by signal 6, SIGABRT)" };
    assert!(error.starts_with(&format!("the process that studies went down while finding the beat {how}")), "{error}");
    assert!(error.contains("Trill is as it was, and every stage of the study that had finished is kept"), "{error}");
    assert_eq!(failed["stems"].as_array().unwrap().len(), 4);
    let unread = fails(client, "listen", json!({ "study": id })).await;
    assert!(unread.starts_with(&format!("study {id} failed: the process that studies went down")), "{unread}");
    // The server is as it was: the song is there, and a file is listened to as before.
    assert!(ok(client, "get_project", json!({})).await["tempo"].as_f64().is_some());
    assert!(
        (ok(client, "listen", json!({ "path": path })).await["tempo"]["bpm"].as_f64().unwrap() - 100.0).abs() < 0.7
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn what_cannot_be_studied_is_said_before_anything_is_made() {
    let fixture = connect().await;
    let client = &fixture.client;
    let gone = fixture.folder.path().join("gone.wav").display().to_string();
    assert_eq!(
        fails(client, "study", json!({ "action": "start", "path": gone })).await,
        format!("there is no file at {gone}")
    );
    let both = fails(client, "study", json!({ "action": "start", "path": gone, "clip": 4 })).await;
    assert!(both.starts_with("say what to study with exactly one of `path`"), "{both}");
    assert!(
        fails(client, "study", json!({ "action": "start" })).await.starts_with("say what to study with exactly one of")
    );
    let path = recording(&fixture.folder.path().join("small song.wav"));
    let language = fails(client, "study", json!({ "action": "start", "path": path, "language": "English" })).await;
    assert_eq!(language, "'English' is not a language as two letters: \"en\", \"de\", \"ja\"");
    assert_eq!(
        fails(client, "study", json!({ "action": "report" })).await,
        "no study has been made yet: study with action \"start\" and a `path` makes one"
    );
    assert_eq!(
        fails(client, "study", json!({ "action": "report", "study": "nope" })).await,
        "there is no study 'nope', and none has been made yet"
    );
    assert!(
        fails(client, "study", json!({ "action": "delete" })).await.starts_with("say which study: `study` is its id")
    );
    // A link, where the program that fetches one is not installed.
    let link = fails(client, "study", json!({ "action": "start", "link": "https://example.org/a-song" })).await;
    assert!(
        link.starts_with("a link is fetched by yt-dlp, which needs ffmpeg, and both are not installed")
            && link.contains("Trill ships neither"),
        "{link}"
    );
    assert_eq!(
        fails(client, "study", json!({ "action": "start", "link": "ftp://example.org/a" })).await,
        "'ftp://example.org/a' is not a link: one begins with https://"
    );
    // Nothing was made of any of it.
    assert_eq!(ok(client, "study", json!({ "action": "list" })).await["studies"], json!([]));
    // And the words are a study's: a file listened to says where they come from.
    let plain = ok(client, "listen", json!({ "path": path, "show": ["words"] })).await;
    assert!(plain["words"].as_str().unwrap().starts_with("the words are written down by a study"), "{plain}");
    assert_eq!(
        fails(client, "listen", json!({ "path": path, "stem": "drums" })).await,
        "`stem` is of a study: a file is listened to as one sound"
    );
    assert_eq!(
        fails(client, "listen", json!({ "path": path, "study": "abc" })).await,
        "give one of `path`, `clip` and `study`, not more"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_is_a_study_like_any_other_and_says_whose_the_recording_is() {
    // With a program taken to be installed for it (which this one is not), the link is taken and the run tries it.
    let programs =
        Programs { ffmpeg: Some(PathBuf::from("no-ffmpeg-here")), fetcher: Some(PathBuf::from("no-yt-dlp-here")) };
    let fixture = connect_with(Script { words: words(), ..Script::default() }, programs).await;
    let client = &fixture.client;
    let started = ok(client, "study", json!({ "action": "start", "link": "https://example.org/a-song" })).await;
    assert_eq!(
        (&started["of"], &started["title"]),
        (&json!("https://example.org/a-song"), &json!("https://example.org/a-song"))
    );
    let note = started["note"].as_str().unwrap();
    assert!(
        note.starts_with("A recording behind a link is somebody's: study only what you may.")
            && note.contains("goes nowhere else"),
        "{note}"
    );
    let id = started["study"].as_str().unwrap();
    let failed = ok(client, "study", json!({ "action": "report", "study": id, "wait_seconds": 60 })).await;
    assert_eq!(failed["state"], "failed", "{failed}");
    assert!(
        failed["error"].as_str().unwrap().starts_with(
            "Fetching the recording failed: the program that fetches ('no-yt-dlp-here') could not be started"
        ),
        "{failed}"
    );
    assert_eq!(
        failed["stages"][0],
        json!({ "stage": "fetch", "state": "failed", "seconds": failed["stages"][0]["seconds"] })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_assistant_in_the_window_studies_a_file_and_is_refused_a_link_and_a_fetch() {
    let programs =
        Programs { ffmpeg: Some(PathBuf::from("no-ffmpeg-here")), fetcher: Some(PathBuf::from("no-yt-dlp-here")) };
    let fixture = connect_with(Script { words: words(), ..Script::default() }, programs).await;
    let assistant = &fixture.assistant;
    let link = fails(assistant, "study", json!({ "action": "start", "link": "https://example.org/a-song" })).await;
    assert!(
        link.starts_with("From here a study is of a file or of a clip of the song.")
            && link.contains("only when the user gives the link themselves"),
        "{link}"
    );
    let fetch = fails(assistant, "setup", json!({ "action": "fetch", "part": "stems" })).await;
    assert!(
        fetch.starts_with("From here nothing is fetched. The user fetches a part in Setup, in the File menu"),
        "{fetch}"
    );
    let remove = fails(assistant, "setup", json!({ "action": "remove", "part": "stems" })).await;
    assert!(remove.starts_with("From here nothing that was fetched is removed"), "{remove}");
    // Nothing was asked of the shelf, and no study was made.
    assert!(fixture.shelf.asked().is_empty());
    assert_eq!(ok(assistant, "study", json!({ "action": "list" })).await["studies"], json!([]));
    // What it may do: read what there is to fetch, and study a file.
    let report = ok(assistant, "setup", json!({ "action": "report" })).await;
    assert_eq!(report["lists"][0]["parts"][0]["part"], "stems");
    let path = recording(&fixture.folder.path().join("small song.wav"));
    let id =
        ok(assistant, "study", json!({ "action": "start", "path": path })).await["study"].as_str().unwrap().to_owned();
    let done = ok(assistant, "study", json!({ "action": "report", "study": id, "wait_seconds": 120 })).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(ok(assistant, "listen", json!({ "study": id })).await["found"]["words"], 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_says_what_there_is_to_fetch_fetches_a_part_and_times_the_models() {
    let fixture = connect().await;
    let client = &fixture.client;
    let report = ok(client, "setup", json!({ "action": "report" })).await;
    let list = &report["lists"][0];
    assert_eq!((&list["list"], &list["title"], &list["total"]), (&json!("models"), &json!("Studies"), &json!(2)));
    assert_eq!(list["kept_in"], fixture.folder.path().join("config").join("models").display().to_string());
    assert!(list["about"].as_str().unwrap().contains("Trill ships none of it"));
    // Each part says its size, whose it is and where it comes from, before anything is fetched.
    assert_eq!(
        list["parts"][0],
        json!({
            "part": "stems",
            "name": "Stems",
            "what": "A part made up for a test.",
            "size_mb": 3,
            "state": "missing",
            "licence": "Nobody's: it was made up for a test.",
            "from": "shelf.test",
        })
    );
    // The programs Trill does not ship, and how to come by one that is not here.
    let programs = report["programs"].as_array().unwrap();
    assert_eq!(
        programs.iter().map(|program| program["name"].as_str().unwrap()).collect::<Vec<_>>(),
        ["ffmpeg", "yt-dlp", "curl"]
    );
    assert_eq!(programs[1]["found"], false);
    assert!(programs[1]["get_it"].as_str().unwrap().contains("yt-dlp"), "{report}");
    assert!(fixture.shelf.asked().is_empty());

    // Fetched when it is asked for by name, and only then.
    fixture.shelf.slow(Duration::from_millis(2));
    let fetching = ok(client, "setup", json!({ "action": "fetch", "part": "stems" })).await;
    assert!(["waiting", "fetching", "checking", "ready"].contains(&fetching["state"].as_str().unwrap()), "{fetching}");
    assert_eq!(
        fails(client, "setup", json!({ "action": "fetch", "part": "stems" })).await,
        "Stems is being fetched already"
    );
    let mut ready = Value::Null;
    for _ in 0..600 {
        ready =
            ok(client, "setup", json!({ "action": "report", "list": "models" })).await["lists"][0]["parts"][0].clone();
        if ready["state"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!((&ready["state"], &ready["on_disk_mb"]), (&json!("ready"), &json!(3)), "{ready}");
    assert_eq!(fixture.shelf.asked(), ["https://shelf.test/stems/stems.onnx"]);
    assert!(fixture.folder.path().join("config").join("models").join("stems").join("stems.onnx").is_file());
    assert_eq!(fails(client, "setup", json!({ "action": "fetch", "part": "stems" })).await, "Stems is here already");
    // A fetch that is stopped keeps what came, and says so.
    fixture.shelf.slow(Duration::from_millis(40));
    ok(client, "setup", json!({ "action": "fetch", "part": "beats" })).await;
    let stopped = ok(client, "setup", json!({ "action": "stop", "part": "beats" })).await;
    assert_eq!(stopped["stopped"], "beats");
    assert_eq!(
        ok(client, "setup", json!({ "action": "remove", "part": "stems" })).await,
        json!({ "removed": "stems", "freed_mb": 3 })
    );
    let unknown = fails(client, "setup", json!({ "action": "fetch", "part": "strings" })).await;
    assert_eq!(unknown, "there is no part 'strings'. Parts: stems, beats");
    assert_eq!(
        fails(client, "setup", json!({ "action": "report", "list": "pianos" })).await,
        "there is no list 'pianos'. Lists: models"
    );
    assert!(fails(client, "setup", json!({ "action": "stop" })).await.starts_with("say which part: `part` is its id"));

    // The timed test: every way of running the models that there is, and the one that was chosen.
    let timed = ok(client, "setup", json!({ "action": "test" })).await;
    assert_eq!(
        (&timed["runs_on"], &timed["chosen_by"]),
        (&json!("the processor"), &json!("the timed test")),
        "{timed}"
    );
    let on = &timed["timed"][0];
    assert_eq!((&on["on"], &on["chosen"]), (&json!("a script"), &json!(true)), "{timed}");
    assert!(
        on["seconds_for_a_minute_of_music"].as_f64().unwrap() > 0.0 && on["memory_mb"].as_u64().unwrap() > 0,
        "{timed}"
    );
    assert_eq!(ok(client, "setup", json!({ "action": "report" })).await["models"]["timed"], timed["timed"]);
    let not_timed = fails(client, "setup", json!({ "action": "choose", "run_on": "nvidia" })).await;
    assert_eq!(
        not_timed,
        "the timed test did not run the models on an NVIDIA card through NVIDIA's libraries. It ran them on: the processor"
    );
    assert!(
        fails(client, "setup", json!({ "action": "choose", "run_on": "gpu" }))
            .await
            .starts_with("'gpu' is not something that runs models")
    );
    assert_eq!(
        ok(client, "setup", json!({ "action": "choose", "run_on": "processor" })).await["runs_on"],
        "the processor"
    );
}
