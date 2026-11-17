//! What is in a sound, the way an AI client asks: over the real MCP
//! transport, with nothing but tool calls, of audio the tests make
//! themselves.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::f64::consts::TAU;
use std::path::Path;
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_engine::AudioBackend;
use trill_mcp::{HttpServer, serve_http};
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    client: Client,
    _runtime: Arc<Runtime>,
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
    Fixture { client, _runtime: runtime, _server: server, folder }
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

const RATE: f64 = 44_100.0;

/// Noise that is the same on every run.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / 8_388_608.0 - 1.0
    }
}

/// Adds a drum at `at` seconds: a kick is a low tone that falls, a snare a
/// tone under noise, a hat a tick of bright noise.
fn drum(out: &mut [f32], at: f64, kind: &str, level: f32, noise: &mut Noise) {
    let start = (at * RATE) as usize;
    let length = match kind {
        "kick" => 0.22,
        "snare" => 0.16,
        _ => 0.045,
    };
    let (mut phase, mut previous) = (0.0f64, 0.0f32);
    for index in 0..(length * RATE) as usize {
        let Some(slot) = out.get_mut(start + index) else { break };
        let time = index as f64 / RATE;
        let value = match kind {
            "kick" => {
                phase += TAU * (48.0 + 70.0 * (-time * 35.0).exp()) / RATE;
                (phase.sin() * (-time * 16.0).exp()) as f32
            }
            "snare" => {
                phase += TAU * 230.0 / RATE;
                (0.45 * phase.sin() * (-time * 28.0).exp()) as f32 + 0.75 * noise.next() * (-time * 26.0).exp() as f32
            }
            _ => {
                let hiss = noise.next();
                let bright = hiss - previous;
                previous = hiss;
                0.6 * bright * (-time * 110.0).exp() as f32
            }
        };
        *slot += level * value;
    }
}

/// Adds a note of MIDI pitch `note` with a few harmonics.
fn tone(out: &mut [f32], at: f64, seconds: f64, note: f64, level: f32) {
    let hertz = 440.0 * 2f64.powf((note - 69.0) / 12.0);
    let start = (at * RATE) as usize;
    for index in 0..(seconds * RATE) as usize {
        let Some(slot) = out.get_mut(start + index) else { break };
        let time = index as f64 / RATE;
        let fade = (time / 0.004).min(1.0) * ((seconds - time) / 0.012).clamp(0.0, 1.0);
        let wave: f64 = (1..=5).map(|k| (TAU * hertz * k as f64 * time).sin() / (k * k) as f64).sum();
        *slot += level * (wave * fade) as f32;
    }
}

fn save(path: &Path, samples: Vec<f32>) -> String {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(path, &AudioBuffer { sample_rate: RATE, left: samples.clone(), right: samples }, WavFormat::Int24)
        .unwrap();
    path.display().to_string()
}

/// Eight bars of drums at 100 beats a minute, starting 0.3 seconds in:
/// kick on one and three, snare on two and four, a hat on every eighth.
fn drums() -> Vec<f32> {
    let beat = 0.6;
    let mut out = vec![0.0f32; ((0.3 + 32.0 * beat + 0.6) * RATE) as usize];
    let mut noise = Noise(77);
    for bar in 0..8 {
        for step in 0..8 {
            let at = 0.3 + (bar * 4) as f64 * beat + step as f64 * beat / 2.0;
            drum(&mut out, at, "hat", 0.45, &mut noise);
            match step {
                // The one is struck harder than the three, as it is played.
                0 => drum(&mut out, at, "kick", 0.9, &mut noise),
                4 => drum(&mut out, at, "kick", 0.55, &mut noise),
                2 | 6 => drum(&mut out, at, "snare", 0.7, &mut noise),
                _ => {}
            }
        }
    }
    out
}

/// The bass line over those drums: Am, Dm, E, Am, a bar each, twice:
/// root, root, fifth, third.
fn bass() -> Vec<f32> {
    let beat = 0.6;
    let mut out = vec![0.0f32; ((0.3 + 32.0 * beat + 0.6) * RATE) as usize];
    for bar in 0..8 {
        let (root, third, fifth) =
            [(33.0, 36.0, 40.0), (38.0, 41.0, 45.0), (28.0, 32.0, 35.0), (33.0, 36.0, 40.0)][bar % 4];
        for (index, note) in [root, root, fifth, third].into_iter().enumerate() {
            tone(&mut out, 0.3 + (bar * 4 + index) as f64 * beat, beat * 0.9, note, 0.5);
        }
    }
    out
}

/// Drums, bass, the chord held through each bar and a voice on its third
/// and fifth: a small song.
fn song() -> Vec<f32> {
    let beat = 0.6;
    let (mut out, bass) = (drums(), bass());
    for (slot, sample) in out.iter_mut().zip(&bass) {
        *slot += sample;
    }
    for bar in 0..8 {
        let (root, third, fifth) =
            [(57.0, 60.0, 64.0), (62.0, 65.0, 69.0), (52.0, 56.0, 59.0), (57.0, 60.0, 64.0)][bar % 4];
        let start = 0.3 + (bar * 4) as f64 * beat;
        for note in [root, third, fifth] {
            tone(&mut out, start, beat * 3.9, note, 0.2);
        }
        tone(&mut out, start, beat * 1.9, third + 12.0, 0.25);
        tone(&mut out, start + 2.0 * beat, beat * 1.9, fifth + 12.0, 0.25);
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sound_is_listened_to_and_its_tempo_key_and_what_was_found_are_said() {
    let fixture = connect().await;
    let path = save(&fixture.folder.path().join("refs").join("small song.wav"), song());

    let heard = ok(&fixture.client, "listen", json!({ "path": path })).await;
    assert_eq!(heard["file"], path);
    assert!((heard["seconds"].as_f64().unwrap() - 20.1).abs() < 0.01);
    let tempo = &heard["tempo"];
    assert!((tempo["bpm"].as_f64().unwrap() - 100.0).abs() < 0.5, "{tempo}");
    assert_eq!(tempo["beats_per_bar"], 4);
    // The first bar line is on the first kick, a hair before it.
    assert!((tempo["first_bar_at_seconds"].as_f64().unwrap() - 0.3).abs() < 0.01, "{tempo}");
    assert_eq!(heard["key"]["name"], "Am");
    assert_eq!(heard["key"]["camelot"], "8A");
    assert!(heard["key"]["also"].as_array().unwrap().len() == 3);
    assert!(heard["tuning_cents"].as_f64().unwrap().abs() < 6.0);
    let found = &heard["found"];
    assert!(found["chords"].as_u64().unwrap() >= 4 && found["bass"].as_u64().unwrap() >= 24, "{found}");
    // Nothing was asked to be listed, so nothing is.
    for list in ["chords", "parts", "loops", "groove", "bass", "bars", "heard_as"] {
        assert!(heard.get(list).is_none(), "{list} was listed unasked");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_are_placed_in_the_recordings_own_bars_kept_to_a_stretch_and_paged() {
    let fixture = connect().await;
    let path = save(&fixture.folder.path().join("small song.wav"), song());

    let heard =
        ok(&fixture.client, "listen", json!({ "path": path, "show": ["chords", "bars", "parts", "loops"] })).await;
    let chords = heard["chords"]["items"].as_array().unwrap();
    let said: Vec<(i64, f64, &str)> = chords
        .iter()
        .map(|chord| (chord["bar"].as_i64().unwrap(), chord["beat"].as_f64().unwrap(), chord["name"].as_str().unwrap()))
        .collect();
    assert_eq!(said[..4], [(1, 1.0, "Am"), (2, 1.0, "Dm"), (3, 1.0, "E"), (4, 1.0, "Am")], "{said:?}");
    assert_eq!(chords[0]["beats"], 4.0);
    let bars = &heard["bars"];
    assert_eq!(bars["total"], 8);
    assert_eq!(bars["items"][0]["bar"], 1);
    assert!((bars["items"][3]["bpm"].as_f64().unwrap() - 100.0).abs() < 0.5);
    assert!((bars["items"][1]["at_seconds"].as_f64().unwrap() - 2.7).abs() < 0.01);
    assert!(heard["parts"]["total"].as_u64().unwrap() >= 1);
    assert_eq!(heard["parts"]["items"][0]["bar"], 1);
    let first = &heard["loops"]["items"][0];
    assert_eq!(first["of"], "Full mix");
    assert!(first["comes_round"].as_u64().unwrap() >= 1 && first["score"].as_f64().unwrap() > 0.4, "{first}");

    // Only the third and fourth bars, a chord to a page.
    let stretch = json!({ "path": path, "show": ["chords", "bass"], "from_bar": 3, "to_bar": 4, "limit": 1 });
    let heard = ok(&fixture.client, "listen", stretch).await;
    assert_eq!(heard["chords"]["total"], 2);
    assert_eq!(heard["chords"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(heard["chords"]["items"][0]["name"], "E");
    assert_eq!(heard["chords"]["more"], 1);
    // The bass of those bars: E, E, B, G sharp, then A, A, E, C.
    assert_eq!(heard["bass"]["total"], 8);
    let low = &heard["bass"]["items"][0];
    assert_eq!((&low["bar"], &low["note"], &low["pitch"]), (&json!(3), &json!("E1"), &json!(28)), "{low}");
    // Under a kick the bass is heard a moment late: this is a whole mix.
    assert!(
        (low["beat"].as_f64().unwrap() - 1.0).abs() < 0.2 && (low["beats"].as_f64().unwrap() - 0.9).abs() < 0.2,
        "{low}"
    );
    let next = ok(
        &fixture.client,
        "listen",
        json!({ "path": path, "show": ["chords"], "from_bar": 3, "to_bar": 4, "limit": 1, "offset": 1 }),
    )
    .await;
    assert_eq!(next["chords"]["items"][0]["name"], "Am");
    assert!(next["chords"].get("more").is_none());
    // What is read from one sound says that it was.
    assert!(heard["heard_as"].as_str().unwrap().starts_with("one sound, not taken apart"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_groove_of_a_drum_loop_is_read_bar_by_bar() {
    let fixture = connect().await;
    let path = save(&fixture.folder.path().join("loops").join("drums 100bpm.wav"), drums());
    let heard = ok(&fixture.client, "listen", json!({ "path": path, "show": ["groove"], "to_bar": 2 })).await;
    let groove = &heard["groove"];
    assert_eq!(groove["total"], 2);
    let eighths: Vec<f64> = (0..8).map(|step| 1.0 + step as f64 * 0.5).collect();
    for bar in groove["items"].as_array().unwrap() {
        assert_eq!(bar["kick"], json!([1.0, 3.0]), "{bar}");
        assert_eq!(bar["snare"], json!([2.0, 4.0]), "{bar}");
        assert_eq!(bar["hat"], json!(eighths), "{bar}");
    }
    // The single hits worth keeping are looked for in the whole of it.
    let heard = ok(&fixture.client, "listen", json!({ "path": path, "show": ["one_shots"] })).await;
    let labels: Vec<&str> =
        heard["one_shots"]["items"].as_array().unwrap().iter().map(|shot| shot["label"].as_str().unwrap()).collect();
    assert!(labels.contains(&"Kick 1") && labels.contains(&"Snare 1") && labels.contains(&"Hat 1"), "{labels:?}");
    // A drum loop has no key to speak of, and says none with confidence.
    assert!(heard["key"].is_null() || heard["key"]["confidence"].as_f64().unwrap() < 0.7, "{}", heard["key"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grid_the_tracker_got_wrong_is_reread_and_a_wrong_word_is_answered_with_the_right_ones() {
    let fixture = connect().await;
    let path = save(&fixture.folder.path().join("drums.wav"), drums());
    let half =
        ok(&fixture.client, "listen", json!({ "path": path, "grid": ["half"], "show": ["groove"], "to_bar": 1 })).await;
    assert!((half["tempo"]["bpm"].as_f64().unwrap() - 50.0).abs() < 0.3, "{}", half["tempo"]);
    // At half the tempo a bar holds twice the music: four kicks to it.
    assert_eq!(half["groove"]["items"][0]["kick"], json!([1.0, 2.0, 3.0, 4.0]));
    let later = ok(&fixture.client, "listen", json!({ "path": path, "grid": ["rotate", "bar3"] })).await;
    assert_eq!(later["tempo"]["beats_per_bar"], 3);
    assert!((later["tempo"]["first_bar_at_seconds"].as_f64().unwrap() - 0.9).abs() < 0.01, "{}", later["tempo"]);

    let error = call(&fixture.client, "listen", json!({ "path": path, "grid": ["faster"] })).await.unwrap_err();
    assert_eq!(
        error,
        "'faster' is not a correction to a beat grid; use \"half\", \"double\", \"rotate\", or \"bar3\" to \"bar12\""
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn what_was_heard_is_written_as_midi_into_a_file_and_into_the_song_in_one_step() {
    let fixture = connect().await;
    // A bass alone, which is what a line is read from note for note.
    let path = save(&fixture.folder.path().join("bass line.wav"), bass());
    let target = fixture.folder.path().join("out").join("bass.mid");

    let written = ok(
        &fixture.client,
        "transcribe",
        json!({ "path": path, "part": "bass", "file": target.display().to_string() }),
    )
    .await;
    assert_eq!(written["part"], "Bass line");
    assert!((written["tempo"].as_f64().unwrap() - 100.0).abs() < 0.5);
    let file = trill_core::midi_file::parse(&std::fs::read(&target).unwrap()).unwrap();
    let notes = &file.tracks[0].notes;
    assert_eq!(written["notes"], notes.len());
    // The file starts at the recording's first bar line, wherever the
    // tracker put it: so many beats into the line.
    let heard = ok(&fixture.client, "listen", json!({ "path": path })).await;
    let skipped = ((heard["tempo"]["first_bar_at_seconds"].as_f64().unwrap() - 0.3) / 0.6).round() as usize;
    assert_eq!(notes.len(), 32 - skipped);
    // On the recording's own beats: a note to a beat, each the line's own.
    let line = [33, 33, 40, 36, 38, 38, 45, 41, 28, 28, 35, 32, 33, 33, 40, 36];
    for (index, note) in notes.iter().take(12).enumerate() {
        assert!((note.start - index as f64).abs() < 0.06, "note {index} starts at {}", note.start);
        assert_eq!(note.pitch, line[(index + skipped) % 16], "note {index}");
    }

    // Into the song: a new track with the groove of the second bar on sixteenths.
    let path = save(&fixture.folder.path().join("drums.wav"), drums());
    let into = json!({ "path": path, "part": "groove", "from_bar": 2, "to_bar": 2, "at": 8.0 });
    let placed = ok(&fixture.client, "transcribe", into).await;
    let track = &placed["tracks"][0];
    assert_eq!(track["drums"], true);
    assert_eq!(track["name"], "Drum groove");
    let clip = track["clip_id"].as_u64().unwrap();
    let notes = ok(&fixture.client, "get_notes", json!({ "clip": clip })).await;
    let kicks: Vec<f64> = notes["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|note| note["pitch"] == 36)
        .map(|note| note["start"].as_f64().unwrap())
        .collect();
    assert!(kicks.contains(&0.0) && kicks.contains(&2.0), "{kicks:?}");
    let project = ok(&fixture.client, "get_project", json!({})).await;
    assert_eq!(project["tracks"].as_array().unwrap().len(), 1);
    // One step back and the song is as it was.
    ok(&fixture.client, "history", json!({ "action": "undo" })).await;
    let project = ok(&fixture.client, "get_project", json!({})).await;
    assert!(project["tracks"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn what_cannot_be_listened_to_is_said_in_a_sentence() {
    let fixture = connect().await;
    let path = save(&fixture.folder.path().join("drums.wav"), drums());

    let neither = call(&fixture.client, "listen", json!({})).await.unwrap_err();
    assert_eq!(neither, "say what to listen to: `path` (an audio file) or `clip` (an audio clip of the song)");
    let gone = call(
        &fixture.client,
        "listen",
        json!({ "path": fixture.folder.path().join("gone.wav").display().to_string() }),
    )
    .await
    .unwrap_err();
    assert!(gone.contains("gone.wav"), "{gone}");

    // A clip is listened to by the file it plays; a clip of notes has none.
    let track = ok(&fixture.client, "create_track", json!({ "name": "Audio" })).await["track_id"].as_u64().unwrap();
    let audio = ok(&fixture.client, "create_audio_clip", json!({ "track": track, "path": path, "start": 0.0 })).await["clip_id"].as_u64().unwrap();
    let heard = ok(&fixture.client, "listen", json!({ "clip": audio })).await;
    assert_eq!(heard["file"], path);
    assert!((heard["tempo"]["bpm"].as_f64().unwrap() - 100.0).abs() < 0.5);
    let synth = ok(&fixture.client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await
        ["track_id"]
        .as_u64()
        .unwrap();
    let midi =
        ok(&fixture.client, "create_clip", json!({ "track": synth, "start": 0.0, "length": 4.0, "name": "Riff" }))
            .await["clip_id"]
            .as_u64()
            .unwrap();
    let error = call(&fixture.client, "listen", json!({ "clip": midi })).await.unwrap_err();
    assert_eq!(error, format!("clip {midi} (Riff) holds notes, not audio: there is no recording in it to listen to"));
    let unknown = call(&fixture.client, "listen", json!({ "clip": 9_999 })).await.unwrap_err();
    assert!(unknown.contains("Existing clips"), "{unknown}");

    // Transcribing wants a place to put it, and something to put there.
    let nowhere = call(&fixture.client, "transcribe", json!({ "path": path, "part": "groove" })).await.unwrap_err();
    assert!(nowhere.starts_with("say where it goes"), "{nowhere}");
    let nothing =
        call(&fixture.client, "transcribe", json!({ "path": path, "part": "chords", "from_bar": 40, "at": 0.0 }))
            .await
            .unwrap_err();
    assert!(nothing.starts_with("no chords was heard there"), "{nothing}");

    // One hit has no tempo, and says so.
    let mut one = vec![0.0f32; RATE as usize];
    drum(&mut one, 0.1, "kick", 0.9, &mut Noise(3));
    let hit = save(&fixture.folder.path().join("kick.wav"), one);
    let heard = ok(&fixture.client, "listen", json!({ "path": hit, "show": ["one_shots"] })).await;
    assert!(heard["tempo"].is_null());
    assert!(heard["note"].as_str().unwrap().starts_with("No pulse was found"));
    assert_eq!(heard["one_shots"]["items"][0]["label"], "Kick 1");
}
