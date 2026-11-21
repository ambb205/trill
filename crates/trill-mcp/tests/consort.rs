//! Trill against the Consort plugins, where they are installed: the
//! owner's own collection of instruments and effects (VST3), which is what
//! MIDI channels were built for. An organ that plays its pedalboard from
//! channel 2, and a guitar that can have a string to a channel, each with
//! a wheel of its own.
//!
//! Everything here is opt-in: it needs the plugins, and makes them.
//!
//! ```text
//! cargo test -p trill-mcp --test consort -- --include-ignored --nocapture --test-threads 1
//! ```
//!
//! The songs are written through the tools, as a model would write them.
//! A test whose plugins are not installed says so and passes. Only the
//! Consort folder is scanned, with a cache of the tests' own; no window is
//! opened, and nothing is heard. The two files to listen to are written to
//! `target/consort-listening`.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use trill_core::device::{CompositeFactory, DeviceFactory};
use trill_engine::AudioBackend;
use trill_mcp::{HttpServer, serve_http};
use trill_plugins::PluginHost;
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

/// Where the collection installs its VST3 plugins.
const FOLDER: &str = r"C:\Program Files\Common Files\VST3\Consort";
/// The two of its instruments that give MIDI channels meanings.
const ORGAN: &str = "vst3:ABCDEF019182FAEB436E73724F72676E";
const GUITAR: &str = "vst3:ABCDEF019182FAEB436E7372456C4774";
const RATE: f64 = 48_000.0;

struct Fixture {
    client: Client,
    _server: HttpServer,
    folder: tempfile::TempDir,
}

/// The Consort plugins that are installed, found with a cache of the
/// tests' own; `None`, with a word, where the collection is not there.
fn plugins() -> Option<PluginHost> {
    use trill_plugins::{ClapConfig, Scanner, Vst3Config};
    if !Path::new(FOLDER).is_dir() {
        eprintln!("skipping: the Consort plugins are not installed ({FOLDER})");
        return None;
    }
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-consort");
    let clap = ClapConfig {
        search_paths: Vec::new(),
        cache_path: Some(scratch.join("clap.json")),
        scanner: Scanner::Auto,
        ..ClapConfig::default()
    };
    let vst3 = Vst3Config {
        search_paths: vec![PathBuf::from(FOLDER)],
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    Some(PluginHost::with_configs(clap, vst3).unwrap())
}

/// A server whose devices are the built-in ones and the Consort plugins
/// installed on this machine, with settings in a folder of its own.
async fn connect() -> Option<Fixture> {
    let host = plugins()?;
    let folder = tempfile::tempdir().unwrap();
    let storage = Storage::At {
        settings: folder.path().join("config").join("settings.json"),
        library_index: Some(folder.path().join("cache").join("library-index.json")),
    };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let parts = Parts { devices: host.factories(), ..Parts::default() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Some(Fixture { client, _server: server, folder })
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

/// A track that plays `instrument`, and the instrument's device.
async fn track(client: &Client, name: &str, instrument: &str) -> (u64, u64) {
    let track = ok(client, "create_track", json!({ "name": name, "instrument": instrument })).await["track_id"]
        .as_u64()
        .unwrap();
    let said = ok(client, "get_instrument_profile", json!({ "track": track })).await;
    (track, said["instrument"]["device_id"].as_u64().unwrap())
}

/// The start of a reply, for a line of a report.
fn short(value: &Value, most: usize) -> String {
    let text = value.to_string();
    match text.char_indices().nth(most) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text,
    }
}

/// How much memory the machine has free, in gigabytes: a survey that
/// makes one plugin after another stops rather than fill it.
fn free_memory_gb() -> f64 {
    let asked = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", "(Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory/1MB"])
        .output();
    asked.ok().and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok()).unwrap_or(f64::MAX)
}

/// The folder the files to listen to go into: `target/consort-listening`.
fn listening() -> PathBuf {
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().expect("target/tmp is inside target");
    let folder = target.join("consort-listening");
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

// ---- measuring ---------------------------------------------------------------------

/// How much of `audio` is a tone of `hertz`: the level of that tone alone
/// that would measure the same, a tone at full scale being 1.
fn tone(audio: &[f32], hertz: f64) -> f64 {
    let turn = std::f64::consts::TAU * hertz / RATE;
    let (mut real, mut imaginary) = (0.0f64, 0.0f64);
    for (frame, sample) in audio.iter().enumerate() {
        // A window that comes to nothing at both ends, so that a tone is
        // found where it is and not smeared over its neighbours.
        let window = 0.5 - 0.5 * (std::f64::consts::TAU * frame as f64 / audio.len() as f64).cos();
        let angle = turn * frame as f64;
        real += f64::from(*sample) * window * angle.cos();
        imaginary += f64::from(*sample) * window * angle.sin();
    }
    4.0 * (real * real + imaginary * imaginary).sqrt() / audio.len() as f64
}

fn hertz(pitch: f64) -> f64 {
    440.0 * ((pitch - 69.0) / 12.0).exp2()
}

/// The pitch at which `audio` has the most of a tone within a semitone
/// and a half of `near`, as a MIDI note number with its fraction, found
/// to a cent.
fn pitch_near(audio: &[f32], near: f64) -> f64 {
    let cents = (-150..=150).max_by(|a, b| {
        let level = |cents: &i32| tone(audio, hertz(near + f64::from(*cents) / 100.0));
        level(a).total_cmp(&level(b))
    });
    near + f64::from(cents.unwrap()) / 100.0
}

/// The stretch of `audio` from `from` to `to`, in seconds.
fn stretch(audio: &[f32], from: f64, to: f64) -> &[f32] {
    let at = |seconds: f64| ((seconds * RATE) as usize).min(audio.len());
    &audio[at(from)..at(to)]
}

fn db(level: f64) -> f64 {
    20.0 * level.max(1e-9).log10()
}

/// The level of a stretch, in dB, and how often it crosses zero in a
/// second: higher for a brighter sound.
fn level_and_crossings(audio: &[f32]) -> (f64, f64) {
    let power: f64 = audio.iter().map(|sample| f64::from(*sample).powi(2)).sum::<f64>() / audio.len().max(1) as f64;
    let crossed = audio.windows(2).filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0)).count();
    (10.0 * power.max(1e-18).log10(), crossed as f64 * RATE / audio.len().max(1) as f64)
}

// ---- the survey ----------------------------------------------------------------------

/// Every Consort plugin that is installed, each in turn: it is scanned,
/// it is made, an instrument sounds when a probe plays it and an effect
/// passes what a synth plays into it, and what it has of parameters and
/// presets is counted. One line a plugin, and at the end the ones that
/// failed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Consort plugins (VST3) installed; run with `cargo test -p trill-mcp --test consort -- --include-ignored --nocapture --test-threads 1`"]
async fn every_installed_consort_plugin_is_scanned_made_and_heard() {
    let Some(fixture) = connect().await else { return };
    let client = &fixture.client;
    let listed = ok(client, "list_devices", json!({})).await;
    let devices: Vec<Value> = listed["devices"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|device| device["uri"].as_str().is_some_and(|uri| uri.starts_with("vst3:")))
        .collect();
    eprintln!("{} Consort plugins found in {FOLDER}", devices.len());
    assert!(devices.len() >= 20, "the collection has more than twenty plugins; found {}", devices.len());
    let mut failed: Vec<String> = Vec::new();
    for device in &devices {
        let (name, uri) = (device["name"].as_str().unwrap_or("?"), device["uri"].as_str().unwrap_or("?"));
        let free = free_memory_gb();
        assert!(free > 16.0, "only {free:.1} GB of memory free: stopping before {name}");
        let began = std::time::Instant::now();
        let instrument = device["category"] == "instrument";
        // On a track: the instrument by itself, an effect behind a synth
        // that plays a chord into it.
        let first = if instrument { uri } else { "builtin:polysynth" };
        let made = call(client, "create_track", json!({ "name": name, "instrument": first })).await;
        let (heard, about) = match made {
            Err(error) => (Err(format!("not made: {error}")), Value::Null),
            Ok(made) if instrument => {
                let track = made["track_id"].as_u64().unwrap();
                let said = ok(client, "get_instrument_profile", json!({ "track": track })).await;
                let about = ok(client, "get_device", json!({ "device": said["instrument"]["device_id"] })).await;
                let probed = call(client, "probe", json!({ "track": track, "notes": ["C3", "C4"] })).await;
                let heard = match probed {
                    Err(error) => Err(format!("probe: {error}")),
                    Ok(reply) if reply["sounding"].as_str().is_some_and(|keys| !keys.is_empty()) => {
                        let keys: Vec<String> = reply["heard"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|key| format!("{} heard {} at {} dB", key["notes"], key["heard"], key["level_db"]))
                            .collect();
                        Ok(format!("sounds: {}", keys.join("; ")))
                    }
                    Ok(reply) => Err(format!("made no sound: {}", short(&reply, 300))),
                };
                ok(client, "delete_track", json!({ "track": track })).await;
                (heard, about)
            }
            Ok(made) => {
                let track = made["track_id"].as_u64().unwrap();
                let added = call(client, "add_device", json!({ "track": track, "uri": uri })).await;
                let result = match added {
                    Err(error) => (Err(format!("not made: {error}")), Value::Null),
                    Ok(added) => {
                        // (Writing the clip waits for the plugin to have arrived: what it
                        // lists of itself is asked after.)
                        let notes = json!([["C3", 0, 2], ["G3", 0, 2], ["E4", 0, 2]]);
                        ok(client, "create_clip", json!({ "track": track, "start": 0, "length": 2, "notes": notes }))
                            .await;
                        let about = ok(client, "get_device", json!({ "device": added["device_id"] })).await;
                        let asked = json!({ "tracks": [track], "max_tail_seconds": 1, "sections": [] });
                        let heard = match call(client, "measure", asked).await {
                            Err(error) => Err(format!("measure: {error}")),
                            Ok(reply) if reply["peak_db"].as_f64().is_some_and(|peak| peak > -60.0) => {
                                Ok(format!("passes sound: peak {:.1} dB", reply["peak_db"].as_f64().unwrap()))
                            }
                            Ok(reply) => Err(format!("passed no sound: {}", short(&reply, 300))),
                        };
                        (heard, about)
                    }
                };
                ok(client, "delete_track", json!({ "track": track })).await;
                result
            }
        };
        let mut line = format!(
            "{name:<16} {} {} parameters, {} presets",
            if instrument { "instrument" } else { "effect    " },
            about["param_count"],
            about["presets"]["total"],
        );
        match heard {
            Ok(said) => line.push_str(&format!(" | {said}")),
            Err(problem) => {
                line.push_str(&format!(" | FAILED: {problem}"));
                failed.push(format!("{name}: {problem}"));
            }
        }
        eprintln!("{line} ({:.1?}, {:.1} GB free)", began.elapsed(), free_memory_gb());
    }
    assert!(failed.is_empty(), "{} of {} failed:\n{}", failed.len(), devices.len(), failed.join("\n"));
}

// ---- the organ -------------------------------------------------------------------------

/// The Organ plays its pedalboard from channel 2 and its manual from
/// every other: the same key on the first channel and on the second is
/// two different sounds. Its shipped entry names the two, a probe plays
/// either, and a clip that has the key on both is rendered and measured.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Consort plugins (VST3) installed; run with `cargo test -p trill-mcp --test consort -- --include-ignored --nocapture --test-threads 1`"]
async fn one_key_is_two_sounds_on_the_organs_manual_and_on_its_pedalboard() {
    let Some(fixture) = connect().await else { return };
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 120 })).await;
    let (organ, _) = track(client, "Organ", ORGAN).await;
    let said = ok(client, "get_instrument_profile", json!({ "track": organ })).await;
    assert_eq!(said["known"], json!({ "source": "built_in", "same_as_track": true }), "{said}");
    assert_eq!(said["profile"]["channels"], json!({ "1": "Manual", "2": "Pedalboard" }), "{said}");
    assert_eq!(said["channels"]["names"], json!({ "1": "Manual", "2": "Pedalboard" }), "{said}");

    // A probe plays the key on either.
    let manual = ok(client, "probe", json!({ "track": organ, "notes": ["C3"], "length": 1.0 })).await;
    let asked = json!({ "track": organ, "notes": ["C3"], "length": 1.0, "channel": "pedalboard" });
    let pedal = ok(client, "probe", asked).await;
    assert_eq!(pedal["channel"], "2 Pedalboard", "{pedal}");
    eprintln!("probe, C3 on the manual:     {}", short(&manual["heard"][0], 300));
    eprintln!("probe, C3 on the pedalboard: {}", short(&pedal["heard"][0], 300));

    // The same key, two seconds on the manual, a rest, two on the
    // pedalboard: written as a model writes it, rendered and measured.
    let notes = json!([
        { "pitch": "C3", "start": 0, "length": 4 },
        { "pitch": "C3", "start": 8, "length": 4, "channel": "pedalboard" },
    ]);
    let clip = ok(client, "create_clip", json!({ "track": organ, "start": 0, "length": 14, "notes": notes })).await;
    assert_eq!(clip["channels"], json!([1, 2]), "{clip}");
    let wav = fixture.folder.path().join("organ.wav");
    ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 1, "sections": [] })).await;
    let audio = trill_media::decode(&wav).unwrap();
    let (on_manual, on_pedals) = (stretch(&audio.left, 0.75, 1.75), stretch(&audio.left, 4.75, 5.75));
    let (manual_db, manual_crossings) = level_and_crossings(on_manual);
    let (pedal_db, pedal_crossings) = level_and_crossings(on_pedals);
    // How much of each is the key's own pitch, the octave under it and the
    // octave over it: which pipes speak.
    let partials = |audio: &[f32]| [36.0, 48.0, 60.0].map(|pitch| db(tone(audio, hertz(pitch))));
    let (manual_pipes, pedal_pipes) = (partials(on_manual), partials(on_pedals));
    eprintln!(
        "render, C3 on the manual:     {manual_db:.1} dB, {manual_crossings:.0} crossings a second, C2/C3/C4 at \
         {manual_pipes:.1?} dB"
    );
    eprintln!(
        "render, C3 on the pedalboard: {pedal_db:.1} dB, {pedal_crossings:.0} crossings a second, C2/C3/C4 at \
         {pedal_pipes:.1?} dB"
    );
    assert!(manual_db > -60.0 && pedal_db > -60.0, "both sound: {manual_db:.1} dB and {pedal_db:.1} dB");
    let apart = manual_pipes.iter().zip(&pedal_pipes).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    assert!(
        apart > 6.0 || (manual_db - pedal_db).abs() > 3.0,
        "the two are one sound: {manual_pipes:.1?} against {pedal_pipes:.1?}"
    );
}

// ---- the guitar ------------------------------------------------------------------------

/// Two strings held, and one channel's wheel moved: as the song has it.
/// A G on the first string and a D on the second, twice (beats 0 to 4 and
/// 4 to 8); from beat 2 the second channel's wheel is all the way up,
/// which is two semitones: its D is an E from there on.
const HIGH: f64 = 67.0;
const LOW: f64 = 62.0;
const BENT: f64 = 64.0;

/// What is heard of the two strings in `audio`: the level of a D, of an E
/// and of a G in dB, and the pitches nearest the last two.
fn two_strings(audio: &[f32]) -> ([f64; 3], f64, f64) {
    let levels = [LOW, BENT, HIGH].map(|pitch| db(tone(audio, hertz(pitch))));
    (levels, pitch_near(audio, BENT - 1.0), pitch_near(audio, HIGH))
}

/// Holds `audio` to the second string being where the wheel has it: bent
/// up to an E where `bent`, a D where not; and the first string a G.
fn assert_strings(what: &str, audio: &[f32], bent: bool) {
    let ([low, up, high], second, first) = two_strings(audio);
    eprintln!(
        "{what}: D4 {low:.1} dB, E4 {up:.1} dB, G4 {high:.1} dB; the second string is at {second:.2} (D4 is 62, E4 \
         64), the first at {first:.2} (G4 is 67)"
    );
    // (A plucked string dies away: two beats on, the first is 20 dB down.)
    assert!(high > -80.0, "{what}: the first string sounds ({high:.1} dB)");
    assert!((first - HIGH).abs() < 0.2, "{what}: the first string is a G, where it was ({first:.2})");
    match bent {
        true => {
            assert!(up > low + 12.0, "{what}: the second string is an E now ({up:.1} dB against {low:.1} dB of D)");
            assert!((second - BENT).abs() < 0.2, "{what}: the second string is bent two semitones ({second:.2})");
        }
        false => {
            assert!(low > up + 12.0, "{what}: the second string is a D ({low:.1} dB against {up:.1} dB of E)");
            assert!((second - LOW).abs() < 0.2, "{what}: the second string is not bent ({second:.2})");
        }
    }
}

/// The guitar with a string to a channel, and the song of the two
/// strings on it; the device and the clip.
async fn two_string_song(client: &Client) -> (u64, u64) {
    ok(client, "set_project", json!({ "tempo": 120 })).await;
    let (guitar, device) = track(client, "Guitar", GUITAR).await;
    // As it comes it names no channels: every channel is the one player.
    let before = ok(client, "get_instrument_profile", json!({ "track": guitar })).await;
    assert_eq!(before["known"], json!({ "source": "built_in", "same_as_track": true }), "{before}");
    assert!(before.get("channels").is_none(), "{before}");
    ok(client, "update_device", json!({ "device": device, "params": { "String Channels": 1 } })).await;
    // Now its strings are its channels, read from the plugin itself.
    let after = ok(client, "get_instrument_profile", json!({ "track": guitar })).await;
    let strings = json!({
        "1": "String 1", "2": "String 2", "3": "String 3", "4": "String 4", "5": "String 5", "6": "String 6"
    });
    assert_eq!(after["channels"]["names"], strings, "{after}");
    assert!(after["channels"]["read_from"].as_str().unwrap().contains("as it is set now"), "{after}");

    let pair = |start: f64| {
        json!([
            { "pitch": "G4", "start": start, "length": 4, "channel": "string_1" },
            { "pitch": "D4", "start": start, "length": 4, "channel": "string_2" },
        ])
    };
    let mut notes = pair(0.0).as_array().unwrap().clone();
    notes.extend(pair(4.0).as_array().unwrap().clone());
    let clip = ok(client, "create_clip", json!({ "track": guitar, "start": 0, "length": 8, "notes": notes })).await;
    assert_eq!(clip["channels"], json!([1, 2]), "{clip}");
    let clip = clip["clip_id"].as_u64().unwrap();
    let wheel = json!({ "pitch_bend": [[0, 0, "hold"], [2, 1, "hold"]] });
    let bent =
        ok(client, "set_controllers", json!({ "clip": clip, "channel": "string_2", "controllers": wheel })).await;
    assert_eq!(bent["controllers"][0]["channel"], 2, "{bent}");
    (device, clip)
}

/// With the guitar's String Channels on: two notes held on two strings,
/// and the wheel of one channel moved, is one string bent and the other
/// where it was. In a render of the whole song, and in renders that start
/// in its middle, after the bend began.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Consort plugins (VST3) installed; run with `cargo test -p trill-mcp --test consort -- --include-ignored --nocapture --test-threads 1`"]
async fn one_channels_wheel_bends_one_string_of_the_guitar_in_a_render() {
    let Some(fixture) = connect().await else { return };
    let client = &fixture.client;
    two_string_song(client).await;
    let render = |name: &str, start: f64| {
        let wav = fixture.folder.path().join(name);
        let asked = json!({
            "path": wav.display().to_string(), "start": start, "end": 8, "max_tail_seconds": 0.5, "sections": []
        });
        async move {
            let report = ok(client, "render", asked).await;
            eprintln!("rendered from beat {start}: {}", short(&report["notes"], 400));
            trill_media::decode(&wav).unwrap()
        }
    };
    // The whole song: a beat is half a second.
    let whole = render("whole.wav", 0.0).await;
    assert_strings("whole, beats 0.5 to 1.5", stretch(&whole.left, 0.25, 0.75), false);
    assert_strings("whole, beats 2.5 to 3.5", stretch(&whole.left, 1.25, 1.75), true);
    assert_strings("whole, beats 4.5 to 5.5", stretch(&whole.left, 2.25, 2.75), true);
    // From beat 3: the wheel went up a beat before, the notes it starts
    // among are sounding, and the next two begin a beat in.
    let late = render("from-3.wav", 3.0).await;
    assert_strings("from beat 3, beats 3 to 3.5", stretch(&late.left, 0.0, 0.25), true);
    assert_strings("from beat 3, beats 4.5 to 5.5", stretch(&late.left, 0.75, 1.25), true);
    // From beat 5, among notes that began at beat 4.
    let later = render("from-5.wav", 5.0).await;
    assert_strings("from beat 5, beats 5.5 to 6.5", stretch(&later.left, 0.25, 0.75), true);
}

/// The same in a song that plays: through an audio device that exists
/// only in the test, with the test as its audio thread. From the top,
/// and started in the middle after the bend began.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Consort plugins (VST3) installed; run with `cargo test -p trill-mcp --test consort -- --include-ignored --nocapture --test-threads 1`"]
async fn one_channels_wheel_bends_one_string_of_the_guitar_in_a_song_that_plays() {
    use trill_engine::host::scripted::ScriptedHost;
    use trill_engine::{AudioConfig, AudioSetup, Session};
    let Some(fixture) = connect().await else { return };
    let client = &fixture.client;
    two_string_song(client).await;
    // The song as the tools made it, opened where it can be played.
    let file = fixture.folder.path().join("two strings.trill");
    ok(client, "save_project", json!({ "path": file.display().to_string() })).await;

    let Some(plugins) = plugins() else { return };
    let mut devices: Vec<Box<dyn DeviceFactory>> = vec![Box::new(trill_dsp::BuiltinFactory)];
    devices.extend(plugins.factories());
    let host = ScriptedHost::with_speakers();
    let setup = AudioSetup::on(Arc::new(host.clone()), AudioConfig::default());
    let media = Arc::new(trill_media::MediaCache::default());
    let mut session = Session::with_audio(Arc::new(CompositeFactory::new(devices)), media, setup).unwrap();
    session.load(&file).unwrap();
    assert!(session.wait_until_opened(Duration::from_secs(60)), "the guitar arrives");
    assert!(session.problems().is_empty(), "{:?}", session.problems());
    let buffer = host.running().expect("a stream").buffer_frames as usize;
    // What the device asks for over `seconds`, with the session's chores
    // done beside it as its housekeeping thread would.
    let run = |session: &mut Session, seconds: f64| {
        for turn in 0..(seconds * RATE / buffer as f64).ceil() as usize {
            host.advance(buffer);
            if turn % 20 == 0 {
                session.housekeeping();
            }
        }
    };
    // A second of nothing, for the plugin to have settled.
    run(&mut session, 1.0);
    host.keep_output();

    // From the top.
    session.seek(0.0);
    run(&mut session, 0.1);
    host.take_output();
    session.play();
    run(&mut session, 4.0);
    let (played, _) = host.take_output();
    assert_strings("playing, beats 0.5 to 1.5", stretch(&played, 0.25, 0.75), false);
    assert_strings("playing, beats 2.5 to 3.5", stretch(&played, 1.25, 1.75), true);
    assert_strings("playing, beats 4.5 to 5.5", stretch(&played, 2.25, 2.75), true);

    // Stopped, put at beat 3.5, and started there: the second pair of
    // notes begins half a beat in, on a string whose wheel went up long
    // before.
    session.stop();
    run(&mut session, 1.5);
    session.seek(3.5);
    run(&mut session, 0.1);
    host.take_output();
    session.play();
    run(&mut session, 1.5);
    let (played, _) = host.take_output();
    assert_strings("started at beat 3.5, beats 4.5 to 5.5", stretch(&played, 0.5, 1.0), true);
    session.stop();
    run(&mut session, 0.2);
}

// ---- to listen to ----------------------------------------------------------------------

/// Two short files for the owner's ears, written through the tools as a
/// model would write them: a guitar chord with one string bent while the
/// others hold, and the organ with a line on its pedals under its manual.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Consort plugins (VST3) installed; run with `cargo test -p trill-mcp --test consort -- --include-ignored --nocapture --test-threads 1`"]
async fn two_files_to_listen_to_are_written_through_the_tools() {
    let Some(fixture) = connect().await else { return };
    let client = &fixture.client;
    let folder = listening();

    // The guitar: an E minor chord, strummed from the sixth string up and
    // left to ring; the third string, its G, is bent up a whole tone, held
    // there and let back down, while the other five stay where they are.
    ok(client, "new_project", json!({})).await;
    ok(client, "set_project", json!({ "name": "Consort guitar", "tempo": 80 })).await;
    let (guitar, device) = track(client, "Guitar", GUITAR).await;
    ok(client, "update_device", json!({ "device": device, "params": { "String Channels": 1 } })).await;
    let strings = [("E2", 6), ("B2", 5), ("E3", 4), ("G3", 3), ("B3", 2), ("E4", 1)];
    let chord: Vec<Value> = strings
        .iter()
        .enumerate()
        .map(|(at, (pitch, string))| {
            let start = at as f64 * 0.04;
            json!({ "pitch": pitch, "start": start, "length": 9.5 - start, "velocity": 96, "channel": string })
        })
        .collect();
    let clip = ok(client, "create_clip", json!({ "track": guitar, "start": 0, "length": 10, "notes": chord })).await;
    let wheel = json!({ "pitch_bend": [[0, 0], [2, 0, 0.4], [3.5, 1], [6, 1, -0.4], [7.5, 0]] });
    let asked = json!({ "clip": clip["clip_id"], "channel": "string_3", "controllers": wheel });
    ok(client, "set_controllers", asked).await;
    let wav = folder.join("guitar chord, one string bent.wav");
    let report = ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 4 })).await;
    eprintln!("{}: {} s, peak {} dB", wav.display(), report["duration_seconds"], report["peak_db"]);
    // What it holds: the third string a G, then an A, then a G again.
    let audio = trill_media::decode(&wav).unwrap();
    let beat = 60.0 / 80.0;
    let third = |from: f64| pitch_near(stretch(&audio.left, from * beat, (from + 1.0) * beat), 56.0);
    let (before, held, after) = (third(0.75), third(4.25), third(8.0));
    eprintln!("the third string is at {before:.2} (G3 is 55), then {held:.2} (A3 is 57), then {after:.2}");
    assert!((before - 55.0).abs() < 0.3 && (held - 57.0).abs() < 0.3 && (after - 55.0).abs() < 0.3);

    // The organ: four bars of chords on the manual, and under them a line
    // on the pedalboard, which is the second channel of the same clip.
    ok(client, "new_project", json!({})).await;
    ok(client, "set_project", json!({ "name": "Consort organ", "tempo": 72 })).await;
    let (organ, _) = track(client, "Organ", ORGAN).await;
    let progression = "C | Am | F G | C";
    let chords = ok(
        client,
        "write_chords",
        json!({ "track": organ, "progression": progression, "low": "C4", "high": "G5", "gate": 0.98 }),
    )
    .await;
    let pedals = ok(
        client,
        "write_bass",
        json!({
            "clip": chords["clip_id"], "progression": progression, "style": "walking",
            "low": "C2", "high": "E3", "channel": "pedalboard"
        }),
    )
    .await;
    assert_eq!(pedals["channel"], "2 Pedalboard", "{pedals}");
    let held = ok(client, "get_notes", json!({ "clip": chords["clip_id"] })).await;
    let on = |channel: u64| held["notes"].as_array().unwrap().iter().filter(|note| note["channel"] == channel).count();
    eprintln!("the organ's clip holds {} notes on its manual and {} on its pedalboard", on(1), on(2));
    assert!(on(1) >= 12 && on(2) >= 12, "the pedal line was written beside the chords, not over them: {held}");
    let wav = folder.join("organ, a line on the pedals.wav");
    let report = ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 4 })).await;
    eprintln!("{}: {} s, peak {} dB", wav.display(), report["duration_seconds"], report["peak_db"]);
    assert!(report["peak_db"].as_f64().unwrap() > -40.0, "{report}");
}
