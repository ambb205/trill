//! A sampled instrument the way an AI client meets it: found in the
//! library as one thing, loaded by its file, written for by what it says
//! of itself, and put right when its recordings have moved. Over the real
//! MCP transport, with nothing but tool calls.
//!
//! The instruments are written here, into a temporary folder: an SFZ file
//! and a few tones each. Nothing of anyone's is read.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::audio::AudioBuffer;
use trill_engine::AudioBackend;
use trill_mcp::{HttpServer, serve_http};
use trill_media::{WavFormat, write_wav};
use trill_runtime::{Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

const SAMPLED: &str = "builtin:sampled";

struct Fixture {
    client: Client,
    _server: HttpServer,
    _runtime: Arc<Runtime>,
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
    Fixture { client, _server: server, _runtime: runtime, folder }
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

/// Writes `seconds` of a tone at `hertz`.
fn tone(path: PathBuf, hertz: f64, seconds: f64) {
    let rate = 44_100.0;
    let frames = (seconds * rate) as usize;
    let channel: Vec<f32> = (0..frames)
        .map(|n| {
            let fade = (1.0 - n as f64 / frames as f64).min(n as f64 / 64.0).clamp(0.0, 1.0);
            ((n as f64 * hertz * std::f64::consts::TAU / rate).sin() * fade * 0.6) as f32
        })
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_wav(&path, &AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel }, WavFormat::Int16)
        .unwrap();
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

/// An upright with two layers of loudness low down, one recording above,
/// a second way of playing chosen by a key, and one thing its file asks
/// for that the device does not do. Returns its file.
fn upright(library: &Path) -> PathBuf {
    let folder = library.join("Keys").join("Upright");
    for (name, hertz, seconds) in
        [("low soft", 130.81, 1.0), ("low hard", 130.81, 1.0), ("high", 523.25, 1.0), ("short", 261.63, 0.06)]
    {
        tone(folder.join("Samples").join(format!("{name}.wav")), hertz, seconds);
    }
    let file = folder.join("Upright.sfz");
    let sfz = "<control> default_path=Samples/
<global> ampeg_release=0.2 sw_lokey=24 sw_hikey=25 sw_default=24 eq1_gain=3
<group> sw_last=24 sw_label=Sustain
<region> sample=low soft.wav lokey=36 hikey=59 pitch_keycenter=48 hivel=80
<region> sample=low hard.wav lokey=36 hikey=59 pitch_keycenter=48 lovel=81
<region> sample=high.wav lokey=60 hikey=84 pitch_keycenter=72
<group> sw_last=25 sw_label=Staccato
<region> sample=short.wav lokey=36 hikey=84 pitch_keycenter=60
";
    std::fs::write(&file, sfz).unwrap();
    file
}

/// A kit whose drums are not on the keys General MIDI has them on.
fn garage_kit(library: &Path) -> PathBuf {
    let folder = library.join("Drums").join("Garage Kit");
    let drums = [("Kick 1", 35, 60.0), ("Snare Center", 40, 190.0), ("Ride Bow", 60, 900.0), ("Crash 1", 57, 1400.0)];
    let mut sfz = String::from("<control> default_path=Samples/\n<global> loop_mode=one_shot\n");
    for (name, key, hertz) in drums {
        tone(folder.join("Samples").join(format!("{name}.wav")), hertz, 0.2);
        sfz.push_str(&format!("<region> sample={name}.wav key={key}\n"));
    }
    let file = folder.join("Garage Kit.sfz");
    std::fs::write(&file, sfz).unwrap();
    file
}

/// The notes of a clip as `(pitch, start, velocity)`.
async fn notes_of(client: &Client, clip: &Value) -> Vec<(u64, f64, u64)> {
    ok(client, "get_notes", json!({ "clip": clip })).await["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| {
            (note["pitch"].as_u64().unwrap(), note["start"].as_f64().unwrap(), note["velocity"].as_u64().unwrap())
        })
        .collect()
}

/// The peak of the track `id` in a measurement, in dB.
fn peak_of(measured: &Value, id: u64) -> f64 {
    let tracks = measured["tracks"].as_array().expect("a measurement lists the tracks");
    tracks.iter().find(|track| track["track"] == id).and_then(|track| track["peak_db"].as_f64()).unwrap_or(f64::NAN)
}

fn names(reply: &Value) -> Vec<&str> {
    reply["sounds"].as_array().unwrap().iter().map(|sound| sound["name"].as_str().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sampled_instrument_is_found_as_one_thing_loaded_by_its_file_and_written_for_by_what_it_says() {
    let fixture = connect().await;
    let client = &fixture.client;
    let library = fixture.folder.path().join("Library");
    let upright = upright(&library);
    tone(library.join("One Shots").join("Clap.wav"), 800.0, 0.1);

    // What the model is told before it does anything.
    let instructions = client.peer_info().and_then(|info| info.instructions.clone()).unwrap_or_default();
    assert!(instructions.contains(SAMPLED) && instructions.contains(".sfz"), "the instructions introduce the device");
    let described = ok(client, "describe_device", json!({ "uri": SAMPLED })).await;
    let description = described["description"].as_str().unwrap_or_default();
    assert!(description.contains("SFZ") && description.contains("preset"), "{described}");

    // The library knows the instrument as one entry that says what it is;
    // its recordings are not sounds to be found one by one.
    let added = ok(client, "library", json!({ "action": "add_folder", "path": text(&library) })).await;
    assert_eq!(added["scanning"], false, "{added}");
    let sounds = ok(client, "search_sounds", json!({})).await;
    assert_eq!(names(&sounds), ["Clap"], "{sounds}");
    let with_recordings = ok(client, "search_sounds", json!({ "include_recordings": true })).await;
    assert_eq!(with_recordings["total"], 5, "the four recordings are there when asked for: {with_recordings}");
    let found = ok(client, "search_sounds", json!({ "kind": "instrument" })).await;
    assert_eq!(names(&found), ["Upright"], "{found}");
    let entry = &found["sounds"][0];
    assert_eq!(entry["path"], json!(text(&upright)));
    let line = entry["instrument"].as_str().unwrap_or_default();
    assert!(
        line.starts_with("4 recordings")
            && line.contains("range C2..C6")
            && line.contains("2 layers of loudness")
            && line.contains("ways of playing: Sustain, Staccato"),
        "{entry}"
    );
    // And as a preset of the device that plays it.
    let presets = ok(client, "search_presets", json!({ "query": "upright" })).await;
    let item = presets["items"].as_array().and_then(|items| items.first()).cloned().unwrap_or_default();
    assert_eq!((&item["device"], &item["file"]), (&json!(SAMPLED), &json!(text(&upright))), "{presets}");

    // Loaded by its file, the reply says enough to write for it.
    let made = ok(client, "create_track", json!({ "name": "Piano", "instrument": SAMPLED, "preset": text(&upright) }));
    let made = made.await;
    let (track, device) = (made["track_id"].as_u64().unwrap(), made["instrument_device_id"].as_u64().unwrap());
    let said = &made["instrument"];
    assert_eq!((&said["name"], &said["file"]), (&json!("Upright"), &json!(text(&upright))), "{made}");
    let about = said["about"].as_str().unwrap_or_default();
    assert!(about.contains("C2") && about.contains("C6"), "its range: {about}");
    assert!(about.contains("sustain") && about.contains("staccato"), "its ways of playing: {about}");
    assert!(about.contains("eq1_gain") && about.contains("Everything else plays"), "what is not done: {about}");
    assert!(said.get("problems").is_none(), "{made}");
    let shown = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!(shown["instrument"]["name"], "Upright", "{shown}");

    // What the track's notes mean is the instrument's to say, and nothing of it is kept in the song.
    let profile = ok(client, "get_instrument_profile", json!({ "track": track })).await;
    assert_eq!(profile["profile"], json!({}), "the track itself says nothing: {profile}");
    assert_eq!(profile["plays"], "pitched", "{profile}");
    let says = &profile["instrument_says"];
    assert_eq!(says["name"], "Upright", "{profile}");
    assert_eq!(says["profile"]["range_names"], "C2-C6", "{profile}");
    let ways = says["profile"]["articulations"].as_object().expect("its ways of playing, by name");
    assert_eq!(ways.keys().collect::<Vec<_>>(), ["staccato", "sustain"], "{profile}");
    assert_eq!(ways["staccato"], json!(25), "the key that chooses it: {profile}");
    assert!(says["note"].as_str().unwrap().contains("none of it is kept in the song"), "{profile}");
    assert!(profile["ask_by_name"].as_str().unwrap().contains("`articulation` on write_chords"), "{profile}");

    // A way of playing is asked for by name, and the key that chooses it is written.
    let chords =
        json!({ "track": track, "progression": "C | F", "low": "C3", "high": "C5", "articulation": "staccato" });
    let short = ok(client, "write_chords", chords).await;
    assert_eq!(short["keyswitches"], json!({ "staccato": 1 }), "{short}");
    assert!(short.get("warnings").is_none(), "{short}");
    assert_eq!(notes_of(client, &short["clip_id"]).await[0].0, 25, "the key that chooses staccato");
    let error = call(client, "write_chords", json!({ "track": track, "progression": "C", "articulation": "tremolo" }));
    let error = error.await.unwrap_err();
    assert!(error.contains("'tremolo' is not an articulation of this instrument; it has sustain, staccato"), "{error}");
    // A note it has no recording for is said when it is written.
    let low = ok(client, "create_clip", json!({ "track": track, "start": 16, "length": 4 })).await;
    let added = ok(client, "add_notes", json!({ "clip": low["clip_id"], "notes": [["C7", 0, 1], ["C3", 1, 1]] })).await;
    let warnings = added.to_string();
    assert!(
        warnings.contains("1 note(s) are outside this instrument's range (C2-C6)") && warnings.contains("C7"),
        "the note over its range is warned about: {added}"
    );

    // It is heard: a measurement waits for the recordings it plays.
    let measured = ok(client, "measure", json!({ "tracks": [track] })).await;
    assert!(peak_of(&measured, track) > -30.0, "{measured}");

    // A file that is no instrument is refused in a sentence, and nothing is added.
    let before = ok(client, "get_project", json!({})).await["tracks"].as_array().unwrap().len();
    let clap = text(&library.join("One Shots").join("Clap.wav"));
    let error = call(client, "add_device", json!({ "track": track, "uri": SAMPLED, "preset": clap })).await;
    assert!(error.is_err(), "{error:?}");
    let empty = text(&library.join("Keys").join("Empty.sfz"));
    std::fs::write(&empty, "// nothing\n").unwrap();
    let error = call(client, "update_device", json!({ "device": device, "preset": empty })).await.unwrap_err();
    assert!(error.contains("Empty.sfz"), "{error}");
    assert_eq!(ok(client, "get_project", json!({})).await["tracks"].as_array().unwrap().len(), before);
    assert_eq!(ok(client, "get_device", json!({ "device": device })).await["instrument"]["name"], "Upright");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_drums_of_a_sampled_kit_are_written_by_name_on_the_keys_the_kit_has_them_on() {
    let fixture = connect().await;
    let client = &fixture.client;
    let kit = garage_kit(&fixture.folder.path().join("Library"));
    let made =
        ok(client, "create_track", json!({ "name": "Drums", "instrument": SAMPLED, "preset": text(&kit) })).await;
    let track = made["track_id"].as_u64().unwrap();
    assert!(made["instrument"]["about"].as_str().unwrap_or_default().to_lowercase().contains("drum"), "{made}");

    let said = ok(client, "get_instrument_profile", json!({ "track": track })).await;
    assert_eq!(said["plays"], "drums", "{said}");
    let map = &said["drum_map"];
    assert_eq!(
        (&map["kick"], &map["snare"], &map["ride"], &map["crash"]),
        (&json!(35), &json!(40), &json!(60), &json!(57)),
        "{said}"
    );
    assert!(map.get("clap").is_none() && map.get("cowbell").is_none(), "no drums but the kit's: {map}");

    // Written by name: each drum on the key this kit has it on, and a drum it has not got left out and said.
    let lanes = json!({ "kick": "x...x...", "snare": "..x...x.", "ride": "x.x.x.x.", "cowbell": "...x...." });
    let beat = ok(client, "write_drums", json!({ "track": track, "lanes": lanes })).await;
    let mut keys: Vec<u64> = notes_of(client, &beat["clip_id"]).await.iter().map(|note| note.0).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys, [35, 40, 60], "nothing on the keys General MIDI has them on");
    assert_eq!(beat["left_out"], json!(["cowbell"]), "{beat}");
    assert!(beat["warnings"].to_string().contains("has no cowbell"), "{beat}");
    let measured = ok(client, "measure", json!({ "tracks": [track] })).await;
    assert!(peak_of(&measured, track) > -30.0, "{measured}");
}

#[tokio::test(flavor = "multi_thread")]
async fn recordings_and_an_instrument_that_moved_are_listed_as_missing_and_relinked() {
    let fixture = connect().await;
    let client = &fixture.client;
    let library = fixture.folder.path().join("Library");
    let upright = upright(&library);
    let made = ok(client, "create_track", json!({ "name": "Piano", "instrument": SAMPLED, "preset": text(&upright) }));
    let made = made.await;
    let (track, device) = (made["track_id"].as_u64().unwrap(), made["instrument_device_id"].as_u64().unwrap());
    let clip = ok(client, "create_clip", json!({ "track": track, "start": 0, "length": 4 })).await;
    ok(client, "add_notes", json!({ "clip": clip["clip_id"], "notes": [["C3", 0, 1], ["C5", 1, 1]] })).await;
    let whole = ok(client, "measure", json!({ "tracks": [track] })).await;
    assert!(whole["missing_files"].is_null() && peak_of(&whole, track) > -30.0, "{whole}");
    let song = text(&fixture.folder.path().join("Songs").join("upright.trill"));
    ok(client, "save_project", json!({ "path": song })).await;
    // The song names the instrument's file and none of its recordings.
    let saved = std::fs::read_to_string(&song).unwrap();
    assert!(saved.contains("Upright.sfz") && !saved.contains("low soft"), "the recordings are the instrument's");

    // Its recordings are moved: the song says so, once, wherever it is looked at.
    let samples = upright.parent().unwrap().join("Samples");
    let moved = fixture.folder.path().join("Elsewhere").join("Upright Samples");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&samples, &moved).unwrap();
    let opened = ok(client, "load_project", json!({ "path": song })).await;
    assert_eq!(opened["missing_files"]["files"], 4, "{opened}");
    let shown = ok(client, "get_device", json!({ "device": device })).await;
    assert!(shown["instrument"]["problems"].to_string().contains("4"), "{shown}");
    let silent = ok(client, "measure", json!({ "tracks": [track] })).await;
    assert_eq!(silent["missing_files"]["files"], 4, "{silent}");
    assert!(peak_of(&silent, track) <= -90.0, "{silent}");
    let lost = ok(client, "missing_files", json!({})).await;
    assert_eq!(lost["missing"], 4, "{lost}");
    let used = &lost["files"][0]["used_by"][0];
    assert_eq!((&used["device"], &used["track"]), (&json!(device), &json!("Piano")), "{lost}");
    assert!(used["slot"].as_str().unwrap_or_default().starts_with("recording "), "{lost}");

    // Told where the folder went, every recording is put right in one step.
    let relinked =
        ok(client, "relink_files", json!({ "folder": { "from": text(&samples), "to": text(&moved) } })).await;
    assert_eq!(relinked["relinked"]["files"], 4, "{relinked}");
    assert_eq!(relinked["still_missing"], 0, "{relinked}");
    let back = ok(client, "measure", json!({ "tracks": [track] })).await;
    assert!(back["missing_files"].is_null(), "{back}");
    assert_eq!(peak_of(&back, track), peak_of(&whole, track), "the track is back as it was");
    assert!(ok(client, "get_device", json!({ "device": device })).await["instrument"].get("problems").is_none());
    ok(client, "save_project", json!({ "path": song })).await;

    // The instrument's own file is moved: one file is missing, and it is given another place.
    let gone = fixture.folder.path().join("Elsewhere").join("Upright.sfz");
    std::fs::rename(&upright, &gone).unwrap();
    let opened = ok(client, "load_project", json!({ "path": song })).await;
    assert_eq!(opened["missing_files"]["files"], 1, "{opened}");
    let lost = ok(client, "missing_files", json!({})).await;
    assert_eq!(lost["files"][0]["file"], json!(text(&upright)), "{lost}");
    assert_eq!(lost["files"][0]["used_by"][0]["slot"], "instrument", "{lost}");
    let relinked =
        ok(client, "relink_files", json!({ "files": [{ "file": text(&upright), "to": text(&gone) }] })).await;
    assert_eq!(relinked["still_missing"], 0, "{relinked}");
    let back = ok(client, "measure", json!({ "tracks": [track] })).await;
    assert!(back["missing_files"].is_null() && peak_of(&back, track) > -30.0, "{back}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_reads_what_instruments_are_to_be_had_and_what_a_song_owes_and_fetches_nothing() {
    let fixture = connect().await;
    let client = &fixture.client;
    let instructions = client.peer_info().and_then(|info| info.instructions.clone()).unwrap_or_default();
    for word in ["instrument_catalogue", "song_credits", "A set is fetched when the user asks for it"] {
        assert!(instructions.contains(word), "the instructions should say '{word}'");
    }

    // What there is, counted: by kind, by licence, and what is offered first. None of it is here.
    let all = ok(client, "instrument_catalogue", json!({})).await;
    assert!(all["sets"].as_u64().unwrap() >= 60 && all["instruments"].as_u64().unwrap() >= 300, "{all}");
    assert_eq!(all["here"], 0, "{all}");
    for kind in ["piano", "electric-piano", "strings", "bass", "drum-kit", "guitar"] {
        assert!(all["by_kind"][kind]["put_forward"].as_u64().unwrap() > 0, "{kind}: {all}");
    }
    assert_eq!(all["by_licence"]["CC0-1.0"]["credit_asked"], false, "{all}");
    assert_eq!(all["by_licence"]["CC-BY-4.0"]["credit_asked"], true, "{all}");
    assert!(all["starter"]["sets"].as_array().unwrap().len() >= 6, "{all}");
    assert!(all.to_string().len() < 4_000, "the counts are a short reply: {} characters", all.to_string().len());

    // A search lists instruments, what is offered first before the rest, a page at a time.
    let pianos = ok(client, "instrument_catalogue", json!({ "kind": "piano", "limit": 2 })).await;
    let listed = pianos["instruments"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{pianos}");
    assert_eq!(listed[0]["starter"], true, "{pianos}");
    assert!(pianos["more"].as_str().unwrap().contains("offset 2 lists the next"), "{pianos}");
    for instrument in listed {
        assert!(instrument["what"].as_str().unwrap().len() >= 12 && instrument["size"].is_string(), "{instrument}");
        assert!(instrument.get("here").is_none() && instrument.get("measured_against_it").is_none(), "{instrument}");
    }
    // What something measured speaks against is left out until it is asked for, and then says what.
    let put_forward = ok(client, "instrument_catalogue", json!({ "kind": "woodwind", "limit": 200 })).await;
    let with_marked =
        ok(client, "instrument_catalogue", json!({ "kind": "woodwind", "include_marked": true, "limit": 200 })).await;
    assert!(with_marked["found"].as_u64().unwrap() > put_forward["found"].as_u64().unwrap(), "{with_marked}");
    let marked: Vec<&Value> = with_marked["instruments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|instrument| instrument.get("measured_against_it").is_some())
        .collect();
    assert!(!marked.is_empty() && marked.iter().all(|instrument| instrument.get("starter").is_none()), "{with_marked}");

    // One set: its licence as its maker wrote it, where it was read, whom to name, how it is fetched.
    let credited = ok(client, "instrument_catalogue", json!({ "query": "salamander" })).await;
    assert_eq!(credited["instruments"][0]["credit_asked"], true, "{credited}");
    let set = credited["instruments"][0]["set"].as_str().unwrap().to_string();
    let whole = ok(client, "instrument_catalogue", json!({ "set": set })).await;
    assert_eq!(whole["licence"]["id"], "CC-BY-3.0", "{whole}");
    assert!(whole["licence"]["says"].as_str().unwrap().contains("Attribution 3.0 Unported"), "{whole}");
    assert!(whole["licence"]["read_at"].as_str().unwrap().starts_with("https://"), "{whole}");
    assert!(whole["credit"].as_str().unwrap().contains("Alexander Holm"), "{whole}");
    assert_eq!(whole["version"].as_str().unwrap().len(), 40, "{whole}");
    assert!(whole["fetching"].as_str().unwrap().contains("This tool fetches nothing"), "{whole}");
    let free = ok(client, "instrument_catalogue", json!({ "public_domain": true, "query": "salamander" })).await;
    assert_eq!(free["found"], 0, "an attribution set is not among what asks for nothing: {free}");

    // A mistake names what there is.
    let error = call(client, "instrument_catalogue", json!({ "kind": "fiddles" })).await.unwrap_err();
    assert!(error.contains("the kinds are piano, electric-piano, organ"), "{error}");
    let error = call(client, "instrument_catalogue", json!({ "set": "salamandr" })).await.unwrap_err();
    assert!(error.contains("there is no set 'salamandr'") && error.contains("Some there are:"), "{error}");

    // A song that plays no sampled instrument owes nothing for one.
    let owed = ok(client, "song_credits", json!({})).await;
    assert_eq!((&owed["owed"], &owed["text"]), (&json!([]), &json!("")), "{owed}");
    assert!(owed["next"].as_str().unwrap().contains("plays no sampled instrument"), "{owed}");
    // One whose instrument the catalogue does not know is told that its terms are its own, and nothing is made up.
    let library = fixture.folder.path().join("Library");
    let upright = upright(&library);
    ok(client, "create_track", json!({ "name": "Keys", "instrument": SAMPLED, "preset": text(&upright) })).await;
    let owed = ok(client, "song_credits", json!({})).await;
    assert_eq!(owed["owed"], json!([]), "{owed}");
    assert_eq!(owed["not_in_the_catalogue"], json!(["Upright.sfz, on Keys"]), "{owed}");
    assert!(owed["not_in_the_catalogue_note"].as_str().unwrap().contains("their own licence does"), "{owed}");
    assert!(owed.get("nothing_owed_for").is_none(), "{owed}");
    // Nothing was fetched, and the library is as it was: nothing of the catalogue is here.
    let all = ok(client, "instrument_catalogue", json!({})).await;
    assert_eq!(all["here"], 0, "{all}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_set_of_the_catalogue_is_fetched_with_setup_when_it_is_asked_for_and_is_then_here() {
    use trill_catalogue::check::{address, file_sum};
    use trill_fetch::scripted::Shelf;
    use trill_runtime::{Parts, Studying};
    // A catalogue of twenty-six sets, each a small upright published file by file under its own id. Each has
    // a file of its own (a line says whose it is): an instrument is known again by its file's checksum.
    let folder = tempfile::tempdir().unwrap();
    let made = upright(&folder.path().join("made")).parent().unwrap().to_owned();
    let paths =
        ["Upright.sfz", "Samples/low soft.wav", "Samples/low hard.wav", "Samples/high.wav", "Samples/short.wav"];
    let at = |path: &str| path.split('/').fold(made.clone(), |at, piece| at.join(piece));
    let version = "0123456789abcdef0123456789abcdef01234567";
    let shelf = Arc::new(Shelf::default());
    let (mut entries, mut lists) = (String::from("format = 1\n"), String::from("format = 1\n[list]\n"));
    for number in 0..26 {
        let id = format!("upright-{number:02}");
        let base = format!("https://example.org/{id}/raw/{version}/");
        let own = folder.path().join("own").join(format!("{id}.sfz"));
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        let mut text = std::fs::read(at("Upright.sfz")).unwrap();
        text.extend_from_slice(format!("\n// {id}\n").as_bytes());
        std::fs::write(&own, text).unwrap();
        let file = |path: &str| if path == "Upright.sfz" { own.clone() } else { at(path) };
        let (mut lines, mut bytes) = (String::new(), 0);
        for path in paths {
            let size = std::fs::metadata(file(path)).unwrap().len();
            lines.push_str(&format!("{} {size} {path}\n", file_sum(&file(path)).unwrap()));
            bytes += size;
            shelf.put(&address(&base, path), std::fs::read(file(path)).unwrap());
        }
        lists.push_str(&format!("{id} = '''\n{lines}'''\n"));
        entries.push_str(&format!(
            "[[set]]\nid = \"{id}\"\nname = \"Upright {number}\"\nabout = \"A small upright, made up for a test.\"\n\
             by = \"A. Maker\"\nlicence = \"CC0-1.0\"\nlicence_at = \"https://example.org/{id}/LICENSE\"\n\
             licence_says = \"This collection is under a Creative Commons 0 license.\"\n\
             from = \"https://example.org/{id}\"\nversion = \"{version}\"\nread = \"2026-11-17\"\nbytes = {bytes}\n\
             files = 5\n[set.fetch]\nbase = \"{base}\"\nlist = \"{id}\"\n\
             [[set.instrument]]\nfile = \"Upright.sfz\"\ntext = \"{}\"\nname = \"Upright {number}\"\nkind = \"piano\"\n\
             words = \"A small upright piano, made up for a test\"\nrange = [36, 84]\nlayers = 2\ntakes = 1\n\
             recordings = 4\nbytes = {bytes}\n",
            file_sum(&own).unwrap(),
        ));
    }
    let catalogue = trill_catalogue::Catalogue::read(&entries, &lists, "format = 1\n").unwrap();
    let storage = Storage::At {
        settings: folder.path().join("config").join("settings.json"),
        library_index: Some(folder.path().join("cache").join("library-index.json")),
    };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let studying = Studying { fetcher: Some(shelf.clone()), parts: Some(Vec::new()), ..Studying::default() };
    let parts = Parts { studying, catalogue: Some(catalogue), ..Parts::default() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    let server = serve_http(runtime.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = &ClientConfig::default().serve(transport).await.unwrap();

    // What there is to fetch, said short: a catalogue is counted, and its tool says what each set is.
    let report = ok(client, "setup", json!({ "action": "report" })).await;
    let lists = report["lists"].as_array().unwrap();
    let sets = lists.iter().find(|list| list["list"] == "instruments").expect("the catalogue's sets are offered");
    assert_eq!((&sets["total"], &sets["here"]), (&json!(26), &json!(0)), "{sets}");
    assert!(sets["parts"].as_str().unwrap().contains("instrument_catalogue"), "{sets}");
    assert!(report.to_string().len() < 6_000, "the report stays short: {} characters", report.to_string().len());
    // Asked for by name, a line each, a page at a time: no licence is said twenty-six times.
    let page = ok(client, "setup", json!({ "action": "report", "list": "instruments", "limit": 3 })).await;
    let rows = page["lists"][0]["parts"].as_array().unwrap();
    assert_eq!((rows.len(), &page["lists"][0]["more"]), (3, &json!(3)), "{page}");
    assert_eq!(rows[0], json!({ "part": "upright-00", "name": "Upright 0", "size_mb": 1, "state": "missing" }));
    // The catalogue says how one is fetched, and fetches nothing.
    let one = ok(client, "instrument_catalogue", json!({ "set": "upright-00" })).await;
    assert!(one["fetching"].as_str().unwrap().contains("with setup, action \"fetch\""), "{one}");
    assert!(shelf.asked().is_empty());

    // Fetched with setup, by its id, it comes, is the library's, and the catalogue says it is here.
    let fetched = ok(client, "setup", json!({ "action": "fetch", "part": "upright-00" })).await;
    assert_eq!(fetched["part"], "upright-00", "{fetched}");
    let mut state = json!(null);
    for _ in 0..200 {
        let page = ok(client, "setup", json!({ "action": "report", "list": "instruments", "limit": 1 })).await;
        state = page["lists"][0]["parts"][0]["state"].clone();
        if state == "ready" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(state, "ready");
    assert_eq!(shelf.asked().len(), 5, "its five files, and nothing of the other sets");
    assert!(runtime.library.wait_until_idle(std::time::Duration::from_secs(30)));
    let found = ok(client, "search_sounds", json!({ "kind": "instrument" })).await;
    assert_eq!(names(&found), ["Upright"], "{found}");
    let here = ok(client, "instrument_catalogue", json!({ "here": true })).await;
    assert_eq!(here["found"], 1, "{here}");
    let kept = folder.path().join("config").join(trill_runtime::SETS_FOLDER).join("upright-00").join("Upright.sfz");
    assert_eq!(
        std::path::PathBuf::from(here["instruments"][0]["here"].as_str().unwrap()),
        std::path::absolute(&kept).unwrap(),
        "{here}"
    );
    // And it loads by that file.
    let made =
        ok(client, "create_track", json!({ "name": "Keys", "instrument": SAMPLED, "preset": text(&kept) })).await;
    assert!(made.is_object(), "{made}");
}
