//! What a track's notes mean to its instrument, over MCP: drum plugins
//! written for and read as drums, articulations asked for by name,
//! ornaments, and what was learned about a plugin kept for the next song.
//!
//! The plugins are stand-ins that exist only here and make no sound, so the
//! tests need nothing installed. Two of them carry the ids of plugins Trill
//! ships an entry for, which is all that an entry is matched by.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use trill_core::Device;
use trill_core::device::{
    DeviceCategory, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor, Event, ProcessInfo,
};
use trill_engine::AudioBackend;
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

/// A metal guitar and a drum kit that Trill ships entries for.
const ECLIPSE: &str = "vst3:ABCDEF019182FAEB41707364416D6574";
const SUPERIOR: &str = "vst3:565354534433617375706572696F7220";
/// A drum plugin nothing is known about, except that it says it is one.
const KIT: &str = "vst3:D0000000000000000000000000000001";
/// A drum plugin whose state says what its kit has on its keys, as
/// Superior Drummer's does: see [`kit_said`].
const SAYS: &str = "vst3:D0000000000000000000000000000002";
/// Two instruments nothing is known about at all.
const SAMPLER: &str = "vst3:C0000000000000000000000000000001";
const LEAD: &str = "builtin:polysynth";

const PLUGINS: [(&str, &str); 5] = [
    (ECLIPSE, "Ample Metal Eclipse"),
    (SUPERIOR, "Superior Drummer 3"),
    (KIT, "Kit"),
    (SAYS, "Kit That Says"),
    (SAMPLER, "Sampled Bass"),
];

/// Which kit the plugin that says has loaded: the test that uses it
/// changes it, as a user does in the plugin's own window.
static LOADED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// What the kit that is loaded has on its keys. The first has its china
/// in the last cymbal's place and two toms; the second has the china
/// where the first hangs a crash, one tom, and neither splash nor ride.
fn kit_said() -> Vec<trill_core::KeySound> {
    let sound = |key: u8, place: &str, instrument: &str, stroke: &str| trill_core::KeySound {
        key,
        place: place.into(),
        instrument: instrument.into(),
        stroke: stroke.into(),
    };
    let mut kit = vec![
        sound(36, "Kick", "22\" Maple", "Open"),
        sound(38, "Snare", "14\" Brass", "Center"),
        sound(40, "Snare", "14\" Brass", "Rimshot"),
        sound(42, "Hi-Hat", "14\" Hats", "Closed Tip"),
        sound(46, "Hi-Hat", "14\" Hats", "Open Edge 2"),
        sound(48, "Racktom 1", "12\" Tom", "Center"),
    ];
    if LOADED.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        kit.extend([
            sound(43, "Floortom 1", "16\" Tom", "Center"),
            sound(86, "Cymbal 1", "17\" Crash", "Crash"),
            sound(55, "Cymbal 3", "10\" Splash", "Crash"),
            sound(51, "Ride", "20\" Ride", "Bow Tip"),
            sound(53, "Ride", "20\" Ride", "Bell Shank"),
            sound(52, "Cymbal 5", "20\" China", "Crash"),
            sound(54, "Cymbal 5", "20\" China", "Mute Hit"),
        ]);
    } else {
        kit.extend([sound(86, "Cymbal 1", "20\" China", "Crash"), sound(52, "Cymbal 5", "18\" Crash", "Crash")]);
    }
    kit
}

/// The off-audio half of the plugin that says what is on its keys.
struct Says;

impl trill_core::device::DeviceController for Says {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        None
    }

    fn key_sounds(&mut self) -> Option<Vec<trill_core::KeySound>> {
        Some(kit_said())
    }
}

struct Plugins;

struct Silent;

impl DeviceProcessor for Silent {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}
    fn set_param(&mut self, _index: usize, _value: f64) {}
    fn process(&mut self, _info: &ProcessInfo, _events: &[Event], _left: &mut [f32], _right: &mut [f32]) {}
    fn reset(&mut self) {}
}

impl DeviceFactory for Plugins {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        PLUGINS
            .iter()
            .map(|(uri, name)| DeviceDescriptor {
                uri: (*uri).into(),
                name: (*name).into(),
                vendor: "Test".into(),
                category: DeviceCategory::Instrument,
                description: "A stand-in".into(),
                params: Vec::new(),
                presets: Vec::new(),
            })
            .collect()
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if !PLUGINS.iter().any(|(uri, _)| *uri == device.uri) {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        let controller: Option<Box<dyn trill_core::device::DeviceController>> =
            (device.uri == SAYS).then(|| Box::new(Says) as Box<dyn trill_core::device::DeviceController>);
        Ok(DeviceInstance { processor: Box::new(Silent), params: Vec::new(), controller })
    }

    /// As a plugin's own categories say it: these call themselves drum
    /// instruments, the others say nothing.
    fn plays_drums(&self, uri: &str) -> bool {
        uri == KIT || uri == SAYS
    }
}

struct Fixture {
    client: Client,
    _server: HttpServer,
    folder: tempfile::TempDir,
}

/// A server whose settings are in `folder`, as a user's are in theirs.
async fn serve(folder: &Path) -> (Client, HttpServer) {
    let storage = Storage::At {
        settings: folder.join("config").join("settings.json"),
        library_index: Some(folder.join("cache").join("library-index.json")),
    };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let parts = Parts { devices: vec![Box::new(Plugins)], ..Parts::default() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    (ClientConfig::default().serve(transport).await.unwrap(), server)
}

async fn connect() -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let (client, server) = serve(folder.path()).await;
    Fixture { client, _server: server, folder }
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

async fn track(client: &Client, name: &str, instrument: &str) -> u64 {
    ok(client, "create_track", json!({ "name": name, "instrument": instrument })).await["track_id"].as_u64().unwrap()
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

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("not a list: {value}"))
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}

/// The points of a clip's pitch-bend curve as `(beat, value)`.
async fn wheel(client: &Client, clip: &Value) -> Vec<(f64, f64)> {
    let curves = ok(client, "get_controllers", json!({ "clip": clip })).await;
    let curve = curves["controllers"].as_array().unwrap().iter().find(|curve| curve["controller"] == "pitch_bend");
    let points = curve.unwrap_or_else(|| panic!("no pitch_bend curve in {curves}"))["points"].as_array().unwrap();
    points.iter().map(|point| (point[0].as_f64().unwrap(), point[1].as_f64().unwrap())).collect()
}

/// Where a curve of held points stands at `beat`: the last point at or before it.
fn held(points: &[(f64, f64)], beat: f64) -> Option<f64> {
    points.iter().rev().find(|point| point.0 <= beat).map(|point| point.1)
}

fn joined(reply: &Value, field: &str) -> String {
    reply.get(field).map(strings).unwrap_or_default().join("\n")
}

/// The track with `id` as get_project shows it.
async fn shown(client: &Client, id: u64) -> Value {
    let project = ok(client, "get_project", json!({})).await;
    project["tracks"].as_array().unwrap().iter().find(|track| track["id"] == id).cloned().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_instrument_tools_are_listed_and_introduced() {
    let fixture = connect().await;
    let tools = fixture.client.list_all_tools().await.unwrap();
    let find = |name: &str| tools.iter().find(|tool| tool.name == name).unwrap_or_else(|| panic!("missing {name}"));
    let set = serde_json::to_string(&find("set_instrument_profile").input_schema).unwrap();
    for taught in ["palm_mute", "`lasts`", "E1-G4 (28-67)", "offset_semitones", "\"required\":[\"track\"]"] {
        assert!(set.contains(taught), "the schema should teach {taught}: {set}");
    }
    assert_eq!(find("get_instrument_profile").annotations.as_ref().and_then(|a| a.read_only_hint), Some(true));
    let melody = serde_json::to_string(&find("write_melody").input_schema).unwrap();
    assert!(melody.contains("!bend=5") && melody.contains("!hammer") && melody.contains("articulation"), "{melody}");
    let time = serde_json::to_string(&find("edit_time").input_schema).unwrap();
    assert!(time.contains("\\\"copy\\\"") && time.contains("make_room"), "{time}");

    let instructions = fixture.client.peer_info().and_then(|info| info.instructions.clone()).unwrap_or_default();
    for said in ["set_instrument_profile", "get_instrument_profile", "!bend=5", "edit_time \"copy\"", "`plays: drums`"]
    {
        assert!(instructions.contains(said), "the instructions should say {said}");
    }
    // The two texts agree: a plugin that is known needs no probe.
    assert!(instructions.contains("ONCE for every plugin that Trill does not know yet"), "{instructions}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_track_given_a_plugin_trill_knows_starts_out_knowing_its_keys() {
    let fixture = connect().await;
    let client = &fixture.client;
    let guitar = track(client, "Rhythm", ECLIPSE).await;
    let drums = track(client, "Drums", SUPERIOR).await;

    // get_project says what each track knows, in a line or two.
    let rhythm = shown(client, guitar).await;
    assert_eq!(rhythm["range"], "C2-C6");
    let articulations = strings(&rhythm["articulations"]);
    for name in ["open", "palm_mute", "harmonic", "slide", "glide", "hammer", "tap", "pinch_harmonic", "muted"] {
        assert!(articulations.contains(&name.to_string()), "{name} is missing from {articulations:?}");
    }
    assert!(rhythm.get("plays").is_none(), "a pitched track does not say so");
    assert_eq!(shown(client, drums).await["plays"], "drums");

    // All of it, in the shape it is written in.
    let known = ok(client, "get_instrument_profile", json!({ "track": guitar })).await;
    assert_eq!((known["plays"].as_str(), known["instrument"]["uri"].as_str()), (Some("pitched"), Some(ECLIPSE)));
    let profile = &known["profile"];
    assert_eq!(profile["articulations"]["palm_mute"], 26);
    assert_eq!(profile["articulations"]["open"], json!({ "key": 24, "velocity": [64, 126], "default": true }));
    assert_eq!(profile["articulations"]["slide"], json!({ "key": 27, "lasts": "once" }));
    assert_eq!(profile["articulations"]["hammer"], json!({ "key": 29, "lasts": "joins" }));
    assert_eq!((profile["range"].clone(), profile["sounds"]["scrape"].clone()), (json!([36, 84]), json!(101)));
    assert!(profile["notes"].as_str().unwrap().contains("under 64 the string is muted"), "{profile}");
    assert_eq!(known["known"], json!({ "source": "built_in", "same_as_track": true }));

    // The kit: General MIDI where it has nothing of its own to say.
    let kit = ok(client, "get_instrument_profile", json!({ "track": drums })).await;
    assert_eq!(kit["plays"], "drums");
    let map = &kit["drum_map"];
    assert_eq!(
        (map["kick"].clone(), map["china"].clone(), map["ride_bell"].clone()),
        (json!(36), json!(52), json!(53))
    );
    assert_eq!(
        (map["high_tom"].clone(), map["rimshot"].clone(), map["wide_open_hat"].clone()),
        (json!(48), json!(40), json!(26))
    );

    // By device type, and all that is known.
    let entry = ok(client, "get_instrument_profile", json!({ "uri": SUPERIOR })).await;
    assert_eq!((entry["source"].as_str(), entry["device"].as_str()), (Some("built_in"), Some("Superior Drummer 3")));
    let all = ok(client, "get_instrument_profile", json!({})).await;
    let listed = all["instruments"].as_array().unwrap();
    assert_eq!(listed.len(), 15, "the entries Trill ships");
    assert!(
        listed.iter().any(|known| known["uri"] == ECLIPSE && known["knows"].as_str().unwrap().contains("palm_mute"))
    );
    let unknown = ok(client, "get_instrument_profile", json!({ "uri": SAMPLER })).await;
    assert_eq!(unknown["source"], "none");

    // Adding the instrument and what it knows is one step, undone together.
    ok(client, "history", json!({ "action": "undo" })).await;
    ok(client, "history", json!({ "action": "redo" })).await;
    assert_eq!(shown(client, drums).await["plays"], "drums");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drum_plugin_is_written_for_and_read_as_drums() {
    let fixture = connect().await;
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 140 })).await;
    let kit = track(client, "Kit", KIT).await;
    let keys = track(client, "Keys", LEAD).await;
    ok(client, "write_chords", json!({ "track": keys, "progression": "Am | F | C | G", "bass": "root" })).await;

    // A plugin that says it is a drum instrument makes a drum track: no warning, by name.
    let lanes =
        json!({ "china": "x.......x.......", "ride_bell": "..x...x...x...x.", "half_open_hat": "....x.......x..." });
    let beat =
        ok(client, "write_drums", json!({ "track": kit, "groove": "rock", "bars": 4, "fill": "toms", "lanes": lanes }))
            .await;
    assert!(beat.get("warnings").is_none(), "{beat}");
    assert_eq!(beat["lanes"]["china"], "x.......x.......");
    assert_eq!(beat["hits"]["china"], 8 - 1, "the fill took the last of them");
    assert_eq!(beat["hits"]["ride_bell"], 16 - 2, "the fill took two of them");
    // General MIDI has one open hat: it stands in, and the reply says so.
    assert!(joined(&beat, "notes").contains("the nearest was played: open_hat for half_open_hat"), "{beat}");
    let pitches: Vec<u64> = notes_of(client, &beat["clip_id"]).await.into_iter().map(|note| note.0).collect();
    assert!(pitches.contains(&52) && pitches.contains(&53) && pitches.contains(&46), "{pitches:?}");

    // What the fill took the place of, which cannot be heard from a count.
    let fill = &beat["fill"];
    assert_eq!((fill["name"].as_str(), fill["in_bars"].clone()), (Some("toms"), json!([4])));
    let bar = &fill["bars"][0];
    assert_eq!((bar["bar"].as_u64(), bar["from_beat"].as_f64()), (Some(4), Some(3.0)));
    assert_eq!(bar["replaced"]["closed_hat"], 4);
    assert_eq!((bar["replaced"]["ride_bell"].clone(), bar["replaced"]["snare"].clone()), (json!(2), json!(1)));
    assert_eq!((bar["played"]["high_tom"].clone(), bar["played"]["low_tom"].clone()), (json!(2), json!(4)));
    assert!(bar.get("then").is_none(), "the last bar has nothing after it to crash on");

    // This kit keeps its china elsewhere: said once, the name lands there from then on.
    let said = ok(
        client,
        "set_instrument_profile",
        json!({ "track": kit, "sounds": { "china": "D6 (86)", "floor_tom": 41 } }),
    )
    .await;
    assert_eq!((said["plays"].as_str(), said["drum_map"]["china"].clone()), (Some("drums"), json!(86)));
    let again = ok(
        client,
        "write_drums",
        json!({ "clip": beat["clip_id"], "lanes": { "china": "x...x...x...x...", "kick": "x.......x......." } }),
    )
    .await;
    assert_eq!(again["hits"]["china"], 4);
    let first_bar: Vec<(u64, f64, u64)> =
        notes_of(client, &beat["clip_id"]).await.into_iter().filter(|n| n.1 < 4.0).collect();
    assert_eq!(first_bar.iter().filter(|note| note.0 == 86).count(), 4, "{first_bar:?}");
    assert!(first_bar.iter().all(|note| note.0 != 52), "the old china key is not written any more");
    // A raw note by name goes through the same map, and a name the kit lacks lists its own.
    let added = ok(
        client,
        "add_notes",
        json!({ "clip": beat["clip_id"], "notes": [["china", 15.5, 0.25], ["kick", 15.75, 0.25]] }),
    )
    .await;
    assert_eq!(added["added"], 2);
    assert!(notes_of(client, &beat["clip_id"]).await.contains(&(86, 15.5, 100)));
    let error =
        call(client, "add_notes", json!({ "clip": beat["clip_id"], "notes": [["gong", 0, 1]] })).await.unwrap_err();
    assert!(
        error.contains("'gong' is not a note name") && error.contains("this kit's own: china, floor_tom"),
        "{error}"
    );

    // Read back as a drum part, by the kit's names, with its first bars as lanes.
    let read = ok(client, "analyze_music", json!({ "track": kit })).await;
    assert_eq!(read["plays"], "drums");
    assert!(read.get("key").is_none() && read.get("chords").is_none(), "{read}");
    assert_eq!(read["drums"]["china"], 5);
    assert_eq!(read["first_bars"]["china"], "x...x...x...x...|................");
    assert_eq!(
        read["first_bars"]["kick"], "x.......x.......|X.....x.X.......",
        "the bar that was rewritten, then the groove's"
    );
    // The bars written before the kit said where its china is still hit the old key, which is
    // no drum of this kit's any more: that is pointed out.
    assert_eq!(read["drums"]["E3"], 5);
    let note = read["note"].as_str().unwrap();
    assert!(note.starts_with("5 hit(s) are on keys this track's kit has no drum name for (E3)"), "{note}");
    // The song's harmony is the keys' alone: no drum note is counted against the key.
    let song = ok(client, "analyze_music", json!({})).await;
    assert_eq!(song["scope"], "song (Keys)");
    assert_eq!(strings(&song["chords"]), ["1: Am", "2: F", "3: C", "4: G"]);
    assert!(song.get("outside_key").is_none() && song.get("left_out").is_none(), "{song}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drum_name_lands_on_that_drum_in_the_kit_that_is_loaded_and_a_drum_it_has_not_got_is_left_out() {
    use std::sync::atomic::Ordering;
    LOADED.store(0, Ordering::SeqCst);
    let fixture = connect().await;
    let client = &fixture.client;
    ok(client, "create_track", json!({ "name": "Keys", "instrument": LEAD })).await;
    let kit = track(client, "Drums", SAYS).await;

    // What the kit says is on its keys is the track's drum map: nothing of it is in the song.
    let said = ok(client, "get_instrument_profile", json!({ "track": kit })).await;
    assert_eq!(said["profile"], json!({}), "the track itself says nothing: {said}");
    assert_eq!(said["plays"], "drums");
    let map = &said["drum_map"];
    assert_eq!(
        (&map["china"], &map["cymbal_5"], &map["crash"], &map["splash"]),
        (&json!(52), &json!(52), &json!(86), &json!(55))
    );
    assert_eq!(
        (&map["high_tom"], &map["floor_tom"], &map["ride"], &map["ride_bell"]),
        (&json!(48), &json!(43), &json!(51), &json!(53))
    );
    assert!(map.get("cowbell").is_none() && map.get("clap").is_none(), "no drums but the kit's: {map}");
    assert_eq!(strings(&said["kit"]["has_no"]), ["rim", "clap", "pedal_hat", "crash2"]);
    assert!(said["kit"]["what"].as_str().unwrap().contains("china (cymbal_5): 20\" China"), "{said}");
    assert!(said["kit"]["read_from"].as_str().unwrap().contains("the plugin's own state"), "{said}");

    // Written by name: each drum on the key this kit has it on.
    let lanes = json!({ "kick": "x...x...", "china": "x.......", "splash": "....x...", "ride_bell": "..x...x." });
    let beat = ok(client, "write_drums", json!({ "track": kit, "lanes": lanes })).await;
    assert!(beat.get("warnings").is_none(), "{beat}");
    let keys: Vec<u64> = notes_of(client, &beat["clip_id"]).await.iter().map(|note| note.0).collect();
    assert_eq!(keys, [36, 52, 53, 36, 55, 53]);

    // The other kit is loaded, in the plugin's own window: the same names, its keys.
    LOADED.store(1, Ordering::SeqCst);
    let said = ok(client, "get_instrument_profile", json!({ "track": kit })).await;
    assert_eq!((&said["drum_map"]["china"], &said["drum_map"]["crash"]), (&json!(86), &json!(52)), "{said}");
    assert_eq!(strings(&said["kit"]["has_no"]), ["rim", "clap", "pedal_hat", "crash2", "splash", "ride", "ride_bell"]);
    let again = ok(client, "write_drums", json!({ "track": kit, "start": 8, "lanes": lanes })).await;
    let keys: Vec<u64> = notes_of(client, &again["clip_id"]).await.iter().map(|note| note.0).collect();
    assert_eq!(keys, [36, 86, 36], "the china where this kit hangs it; nothing for the splash or the ride's bell");
    assert_eq!(strings(&again["left_out"]), ["ride_bell", "splash"]);
    let warning = joined(&again, "warnings");
    assert!(
        warning.contains("The kit on this track has no ride_bell, splash: nothing was written for them")
            && warning.contains("Its drums: ")
            && warning.contains("china"),
        "{warning}"
    );
    // A groove of the library loses the lanes this kit cannot play, and says which; the fill's crash is the kit's.
    let groove =
        ok(client, "write_drums", json!({ "track": kit, "start": 16, "groove": "jazz_swing", "bars": 2 })).await;
    assert!(joined(&groove, "warnings").contains("has no ride"), "{groove}");
    let keys: Vec<u64> = notes_of(client, &groove["clip_id"]).await.iter().map(|note| note.0).collect();
    assert!(!keys.contains(&51) && !keys.contains(&44), "nothing on the keys General MIDI has them on: {keys:?}");
    // Nothing but drums it has not got: that is refused, with what it has.
    let error = call(client, "write_drums", json!({ "track": kit, "lanes": { "cowbell": "x...", "clap": "..x." } }))
        .await
        .unwrap_err();
    assert!(error.contains("the kit on this track has no clap, cowbell") && error.contains("Its drums: "), "{error}");

    // A raw note by name goes by the kit too, and General MIDI's key for a drum it lacks is not offered.
    let added = ok(
        client,
        "add_notes",
        json!({ "clip": again["clip_id"], "notes": [["china", 2, 0.25], ["tom_high", 3, 0.25]] }),
    )
    .await;
    assert_eq!(added["added"], 2);
    assert!(notes_of(client, &again["clip_id"]).await.contains(&(86, 2.0, 100)));
    let error = call(client, "add_notes", json!({ "clip": again["clip_id"], "notes": [["cowbell", 0, 0.25]] })).await;
    let error = error.unwrap_err();
    assert!(
        error.contains("'cowbell' is not a note name")
            && error.contains("a drum of this track's kit, which has no other"),
        "{error}"
    );

    // Read back by this kit's names: the hits written for the other kit's china are a crash here.
    let read = ok(client, "analyze_music", json!({ "track": kit, "end": 8 })).await;
    assert_eq!(
        read["drums"],
        json!({ "crash": 1, "kick": 2, "E3": 0 })
            .as_object()
            .map(|_| json!({ "crash": 1, "kick": 2, "F3": 2, "G3": 1 }))
            .unwrap(),
        "{read}"
    );
    assert!(read["note"].as_str().unwrap().contains("no drum name for (F3, G3)"), "{read}");
    LOADED.store(0, Ordering::SeqCst);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_note_says_how_it_is_played_and_the_clips_keyswitches_are_written_for_all_its_notes() {
    let fixture = connect().await;
    let client = &fixture.client;
    let guitar = track(client, "Rhythm", ECLIPSE).await;

    // A riff note by note, each saying how it is played: chords that ring, chugs between them
    // muted by how hard they are hit, a palm-muted pair, a hammered run, a scrape by its name.
    let riff = json!([
        ["E2", 0, 0.5, 110], ["B2", 0, 0.5, 110],
        ["E2", 1, 0.25, 100, "muted"], ["E2", 1.25, 0.25, 127, "muted"],
        ["G2", 2, 0.5, 110],
        ["E2", 3, 0.25, 90, "palm_mute"], ["E2", 3.25, 0.25, "palm_mute"],
        ["A3", 4, 0.25, 100], ["B3", 4.25, 0.25, 100, "hammer"], ["A3", 4.5, 0.25, 100, "hammer"],
        { "pitch": "scrape", "start": 5, "length": 0.5, "velocity": 30 },
        { "pitch": "E3", "start": 6, "length": 1, "velocity": 20, "how": "slide" }
    ]);
    let clip = ok(client, "create_clip", json!({ "track": guitar, "start": 0, "length": 8, "notes": riff })).await;
    assert_eq!(clip["note_count"], 12);
    assert_eq!(clip["keyswitches"], json!({ "hammer": 1, "open": 2, "palm_mute": 1, "slide": 1 }), "{clip}");
    // A note that is slid into is an ordinary note: too soft to ring, it is raised, and that is said.
    assert!(joined(&clip, "notes").contains("1 note(s) played the ordinary way"), "{clip}");
    let notes = notes_of(client, &clip["clip_id"]).await;
    let switches: Vec<(u64, f64)> = notes.iter().filter(|note| note.0 < 36).map(|note| (note.0, note.1)).collect();
    assert_eq!(
        switches,
        [(24, 0.0), (26, 2.97), (24, 3.97), (29, 3.985), (27, 5.97)],
        "open for the first chord, palm mute and back, the hammer before its run, the slide before its note"
    );
    let hit = |beat: f64| notes.iter().find(|note| note.0 >= 36 && (note.1 - beat).abs() < 1e-6).unwrap().2;
    assert_eq!((hit(1.0), hit(1.25)), (56, 63), "muted is how hard the note is hit: inside 32-63");
    assert_eq!((hit(3.0), hit(3.25)), (90, 100), "a palm mute keeps its velocity");
    assert_eq!(hit(6.0), 64, "a slide is an open note: raised to where it rings");
    assert!(notes.contains(&(101, 5.0, 30)), "the scrape is left as it is: {notes:?}");

    // More notes, in a second call, among the first: the chords keep their open switch.
    let added = ok(
        client,
        "add_notes",
        json!({ "clip": clip["clip_id"], "notes": [["E2", 0.5, 0.25, 100, "palm_mute"], ["E2", 2.5, 0.25, 100, "palm_mute"]] }),
    )
    .await;
    assert_eq!((added["added"].as_u64(), added["note_count"].as_u64()), (Some(2), Some(21)), "{added}");
    // What these two notes took: their own switch twice (one of them stands in for the one the
    // old palm mutes had), and the muted notes after the first now need their key pressed again.
    assert_eq!(added["keyswitches"], json!({ "muted": 1, "palm_mute": 1 }), "{added}");
    let notes = notes_of(client, &clip["clip_id"]).await;
    let early: Vec<(u64, f64)> =
        notes.iter().filter(|note| note.0 < 36 && note.1 < 3.5).map(|note| (note.0, note.1)).collect();
    assert_eq!(
        early,
        [(24, 0.0), (26, 0.47), (24, 0.97), (26, 2.47)],
        "after the first palm mute the muted notes are given their key again; the palm mute at 3 follows the one at 2.5"
    );
    // Read back, every note is played as it was asked for.
    let read = ok(client, "analyze_music", json!({ "clip": clip["clip_id"] })).await;
    assert_eq!(read["left_out"], json!({ "keyswitches": 7, "noises": 1 }), "{read}");

    // The musical tools write into the same clip the same way: palm-muted chords over its second bar,
    // which follow the palm mutes before them and need no switch of their own.
    let chords = ok(
        client,
        "write_chords",
        json!({ "clip": clip["clip_id"], "offset": 4, "progression": "E5", "low": "E2", "high": "E4", "inversion": 0,
                "rhythm": "x...x...", "articulation": "palm_mute" }),
    )
    .await;
    assert!(chords.get("keyswitches").is_none() && chords.get("warnings").is_none(), "{chords}");
    let notes = notes_of(client, &clip["clip_id"]).await;
    let late: Vec<(u64, f64)> =
        notes.iter().filter(|note| note.0 < 36 && note.1 >= 3.5).map(|note| (note.0, note.1)).collect();
    assert_eq!(late, [], "the hammered run and the slide went with the bar they were in, and their switches with them");
    assert!(!notes.iter().any(|note| note.0 == 101), "so did the scrape: {notes:?}");
    // Open chords in their place: now the switch is needed, once.
    let open = ok(
        client,
        "write_chords",
        json!({ "clip": clip["clip_id"], "offset": 4, "progression": "E5", "low": "E2", "high": "E4", "inversion": 0,
                "rhythm": "x...x..." }),
    )
    .await;
    assert_eq!(open["keyswitches"], json!({ "open": 1 }), "{open}");

    // set_notes writes the clip anew, with its switches.
    let set =
        ok(client, "set_notes", json!({ "clip": clip["clip_id"], "notes": [["E3", 0, 1, "harmonic"], ["E3", 2, 1]] }))
            .await;
    assert_eq!(
        (set["note_count"].as_u64(), &set["keyswitches"]),
        (Some(2), &json!({ "harmonic": 1, "open": 1 })),
        "{set}"
    );
    assert_eq!(
        notes_of(client, &clip["clip_id"]).await,
        [(25, 0.0, 100), (52, 0.03, 100), (24, 1.97, 100), (52, 2.0, 100)],
        "in the note tools a pitch is the key that is played"
    );
    // The musical tools write what is heard: a harmonic's key is an octave under the note asked for.
    let line = ok(client, "write_melody", json!({ "track": guitar, "start": 8, "phrase": "E4:2!harmonic E4:2" })).await;
    assert!(
        joined(&line, "notes").contains("1 note(s) played as 'harmonic' sound 12 semitones above their keys"),
        "{line}"
    );
    let keys: Vec<u64> = notes_of(client, &line["clip_id"]).await.iter().map(|note| note.0).collect();
    assert_eq!(keys, [25, 52, 24, 64]);
    let read = ok(client, "analyze_music", json!({ "clip": line["clip_id"] })).await;
    assert_eq!(read["pitch_range"], "E4-E4", "both are heard as E4: {read}");
    assert!(read["left_out"]["read_as_heard"].as_str().unwrap().starts_with("1 note(s)"), "{read}");

    // Keyswitches placed by hand are left alone, and it is said.
    let raw = ok(
        client,
        "create_clip",
        json!({ "track": guitar, "start": 16, "length": 4, "notes": [[26, 0.5, 0.1], ["E2", 1, 0.5]] }),
    )
    .await;
    assert!(raw.get("keyswitches").is_none(), "notes that say nothing go in as they are: {raw}");
    let more = ok(client, "add_notes", json!({ "clip": raw["clip_id"], "notes": [["E2", 2, 0.5, "muted"]] })).await;
    assert!(joined(&more, "notes").contains("keyswitches that were placed by hand"), "{more}");
    assert_eq!(
        notes_of(client, &raw["clip_id"]).await.iter().map(|note| (note.0, note.1)).collect::<Vec<_>>(),
        [(26, 0.5), (40, 1.0), (24, 1.97), (40, 2.0)]
    );

    // What cannot be: an articulation it has not got, a drum track, an instrument without any.
    let error = call(client, "add_notes", json!({ "clip": clip["clip_id"], "notes": [["E2", 0, 1, "tremolo"]] })).await;
    assert!(error.unwrap_err().contains("'tremolo' is not an articulation of this instrument; it has open, harmonic"));
    let kit = track(client, "Drums", KIT).await;
    let error = call(
        client,
        "create_clip",
        json!({ "track": kit, "start": 0, "length": 4, "notes": [["kick", 0, 1, "muted"]] }),
    )
    .await;
    assert!(error.unwrap_err().contains("is a drum track, whose notes are drums by name"));
    let keys = track(client, "Keys", LEAD).await;
    let error = call(
        client,
        "create_clip",
        json!({ "track": keys, "start": 0, "length": 4, "notes": [["C4", 0, 1, "muted"]] }),
    )
    .await;
    assert!(error.unwrap_err().contains("has no articulations to ask for by name"));
    let error = call(client, "play_notes", json!({ "track": keys, "notes": [["C4", 0, 1, 100, "muted"]] })).await;
    assert!(error.unwrap_err().contains("a note says how it is played only where it is written into a clip"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_track_is_told_it_is_drums_when_its_plugin_does_not_say() {
    let fixture = connect().await;
    let client = &fixture.client;
    let kit = track(client, "Sampled kit", SAMPLER).await;
    let before = ok(client, "write_drums", json!({ "track": kit, "groove": "rock" })).await;
    let warning = joined(&before, "warnings");
    assert!(warning.contains("is not a drum track") && warning.contains("role \"drums\""), "{before}");
    let as_pitches = ok(client, "analyze_music", json!({ "track": kit })).await;
    assert!(as_pitches.get("key").is_some(), "read as pitches until it is told: {as_pitches}");

    let told = ok(client, "set_instrument_profile", json!({ "track": kit, "role": "drums", "remember": false })).await;
    assert_eq!(
        (told["plays"].as_str(), told["remembered"].as_str()),
        (Some("drums"), Some("no: kept in this song only"))
    );
    let after = ok(client, "write_drums", json!({ "track": kit, "groove": "rock", "start": 8 })).await;
    assert!(after.get("warnings").is_none(), "{after}");
    assert_eq!(ok(client, "analyze_music", json!({ "track": kit })).await["plays"], "drums");
    // Pitches on it are warned about now, and "auto" gives the question back to the instrument.
    let chords = ok(client, "write_chords", json!({ "track": kit, "progression": "C", "start": 16 })).await;
    assert!(joined(&chords, "warnings").contains("is a drum track"), "{chords}");
    let auto = ok(client, "set_instrument_profile", json!({ "track": kit, "role": "auto", "remember": false })).await;
    assert_eq!(auto["plays"], "pitched");
    assert!(auto["plays_because"].as_str().unwrap().contains("not a drum instrument"), "{auto}");
    // It was kept in the song only: another track with the plugin is pitched.
    let other = track(client, "Other", SAMPLER).await;
    assert!(shown(client, other).await.get("plays").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn articulations_are_asked_for_by_name_and_the_keyswitches_written() {
    let fixture = connect().await;
    let client = &fixture.client;
    let guitar = track(client, "Rhythm", ECLIPSE).await;

    // Palm-muted power chords: one press before the first chord, which waits for it.
    let riff = ok(
        client,
        "write_chords",
        json!({ "track": guitar, "progression": "E5 | G5 | A5 | E5", "rhythm": "eighths", "low": "E2", "high": "E4", "inversion": 0,
                "articulation": "palm_mute" }),
    )
    .await;
    assert_eq!(riff["keyswitches"], json!({ "palm_mute": 1 }));
    assert!(riff.get("warnings").is_none(), "{riff}");
    let notes = notes_of(client, &riff["clip_id"]).await;
    assert_eq!(notes[0], (26, 0.0, 100), "the keyswitch, by itself, at the very start");
    assert!(notes[1..].iter().all(|note| note.0 >= 36), "everything else is music: {notes:?}");
    assert_eq!(notes[1].1, 0.03, "the first chord waits for its switch");
    assert_eq!(riff["note_count"].as_u64(), Some(notes.len() as u64 - 1), "keyswitches are not counted as notes");

    // Unasked, a part begins by choosing the ordinary way, since the one before left palm mute on;
    // and a note too soft to ring is raised to where it does.
    let bass = ok(
        client,
        "write_bass",
        json!({ "track": guitar, "start": 16, "progression": "E5 | G5", "low": "E2", "high": "E3", "rhythm": "pulse", "velocity": 50, "accent": 0 }),
    )
    .await;
    assert_eq!(bass["keyswitches"], json!({ "open": 1 }));
    assert!(joined(&bass, "notes").contains("8 note(s) were outside the velocities 64-126"), "{bass}");
    assert!(joined(&bass, "notes").contains("muted, choked, dead"), "{bass}");
    let notes = notes_of(client, &bass["clip_id"]).await;
    assert_eq!(notes[0].0, 24);
    assert!(notes[1..].iter().all(|note| note.2 == 64), "{notes:?}");
    // Asked for by name, soft is what is wanted, and it is played under the open switch.
    let muted = ok(
        client,
        "write_bass",
        json!({ "track": guitar, "start": 24, "progression": "E5", "low": "E2", "high": "E3", "rhythm": "pulse", "articulation": "muted" }),
    )
    .await;
    assert_eq!(muted["keyswitches"], json!({ "muted": 1 }), "the same key as open, pressed for what was asked");
    assert!(muted.get("notes").is_none(), "{muted}");
    let notes = notes_of(client, &muted["clip_id"]).await;
    assert!(notes[1..].iter().all(|note| (32..=63).contains(&note.2)), "{notes:?}");

    // Rewriting a stretch takes the old keyswitches with it.
    let clip = ok(
        client,
        "write_chords",
        json!({ "track": guitar, "start": 32, "progression": "E5 | G5", "low": "E2", "high": "E4" }),
    )
    .await["clip_id"]
        .clone();
    ok(client, "write_chords", json!({ "clip": clip, "offset": 4, "progression": "A5", "low": "E2", "high": "E4", "articulation": "palm_mute" }))
        .await;
    ok(client, "write_chords", json!({ "clip": clip, "offset": 4, "progression": "A5", "low": "E2", "high": "E4", "articulation": "palm_mute" }))
        .await;
    let switches: Vec<(u64, f64, u64)> = notes_of(client, &clip).await.into_iter().filter(|note| note.0 < 36).collect();
    assert_eq!(switches, [(24, 0.0, 100), (26, 3.97, 100)], "written twice, there once");

    // An articulation it does not have, and an instrument that has none.
    let error =
        call(client, "write_chords", json!({ "track": guitar, "progression": "E5", "articulation": "tremolo" })).await;
    let error = error.unwrap_err();
    assert!(
        error.contains("'tremolo' is not an articulation of this instrument; it has open, harmonic, palm_mute"),
        "{error}"
    );
    let plain = track(client, "Keys", LEAD).await;
    let error =
        call(client, "write_chords", json!({ "track": plain, "progression": "C", "articulation": "palm_mute" })).await;
    assert!(error.unwrap_err().contains("has no articulations to ask for by name"));

    // Read back: the keyswitches are left out, so the chords are the chords.
    let read = ok(client, "analyze_music", json!({ "track": guitar, "end": 16 })).await;
    assert_eq!(strings(&read["chords"]), ["1: E5", "2: G5", "3: A5", "4: E5"]);
    assert_eq!(read["left_out"], json!({ "keyswitches": 1 }));
    assert!(!read["pitch_range"].as_str().unwrap().contains("D1"), "the keyswitch is not its lowest note: {read}");
    assert!(read.get("outside_key").is_none(), "{read}");
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_an_instrument_cannot_play_are_warned_about_when_they_are_written() {
    let fixture = connect().await;
    let client = &fixture.client;
    let guitar = track(client, "Rhythm", ECLIPSE).await;

    // A clip of nothing but keyswitches, by name or by key.
    let silent = ok(
        client,
        "create_clip",
        json!({ "track": guitar, "start": 0, "length": 4, "notes": [["palm_mute", 0, 0.1], [24, 2, 0.1]] }),
    )
    .await;
    let warning = silent["warning"].as_str().unwrap();
    assert!(warning.contains("holds only keyswitches (palm_mute, open)"), "{warning}");
    assert_eq!(notes_of(client, &silent["clip_id"]).await[0].0, 26, "a keyswitch is asked for by its name");
    // With a note beside them it is a part, and says nothing.
    let fine = ok(client, "add_notes", json!({ "clip": silent["clip_id"], "notes": [["E3", 0.5, 1]] })).await;
    assert!(fine.get("warning").is_none(), "{fine}");
    // Notes outside the range: named, with the range, and the noises are not among them.
    let low = ok(
        client,
        "add_notes",
        json!({ "clip": silent["clip_id"], "notes": [["A1", 1, 1], ["D6", 2, 1], ["scrape", 3, 0.5], ["E3", 3, 0.5]] }),
    )
    .await;
    let warning = low["warning"].as_str().unwrap();
    assert!(
        warning.contains("2 note(s) are outside this instrument's range (C2-C6) and will not sound: A1, D6"),
        "{warning}"
    );
    // The writing tools say it as well, for what they wrote.
    let bass = ok(client, "write_bass", json!({ "track": guitar, "start": 8, "progression": "E | A" })).await;
    let warnings = joined(&bass, "warnings");
    assert!(
        warnings.contains("1 note(s) are outside this instrument's range (C2-C6) and will not sound: A1"),
        "{bass}"
    );
    // E1 is not merely too low on this guitar: it is one of its switches.
    assert!(
        warnings.contains("1 note(s) of this part fall on keys that are keyswitches of this instrument (glide)"),
        "{bass}"
    );

    // Measuring the song: a track that is silent because its clip holds only keyswitches is told so
    // from what the track knows, not from a guess.
    ok(
        client,
        "create_clip",
        json!({ "track": guitar, "start": 32, "length": 4, "notes": [[26, 0, 0.1], [29, 1, 0.1]] }),
    )
    .await;
    let measured = ok(client, "measure", json!({ "start": 32, "end": 36 })).await;
    let said = joined(&measured, "notes");
    assert!(said.contains("are all keyswitches of this instrument (hammer, palm_mute)"), "{measured}");
    assert!(!said.contains("on most instruments"), "it knows, so it does not guess: {said}");
    // A probe says whether the instrument is known already, and if not where to keep what it finds.
    // (A built-in synth is probed here: a stand-in plugin that makes no sound is waited for.)
    let synth = track(client, "Synth", LEAD).await;
    let probed = ok(client, "probe", json!({ "track": synth, "notes": ["E3"] })).await;
    assert!(probed["known"].as_str().unwrap().contains("Keep what this probe found with one call"), "{probed}");
    ok(client, "set_instrument_profile", json!({ "track": synth, "range": "C1-C7 (24-96)" })).await;
    let probed = ok(client, "probe", json!({ "track": synth, "notes": ["E3"] })).await;
    let known = probed["known"].as_str().unwrap();
    assert!(
        known.starts_with("Trill already knows this instrument (it was taught on this machine): range C1-C7"),
        "{probed}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ornaments_are_a_pitch_bend_curve_on_an_instrument_without_articulations() {
    let fixture = connect().await;
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 120 })).await;
    let lead = track(client, "Lead", LEAD).await;
    let phrase = "4:2!bend=5 :0.5 5 6!hammer 5!pull 3 | 1:2!vib 3:2!slide";
    let line =
        ok(client, "write_melody", json!({ "track": lead, "key": "A minor", "phrase": phrase, "gate": 1 })).await;

    // What each ornament was written as, and the notes as they were asked for.
    assert_eq!(line["pitches"], "D5 E5 F5 E5 C5 | A4 C5");
    let ornaments = &line["ornaments"];
    assert_eq!(ornaments["bend"], "1 as a pitch bend");
    assert_eq!(ornaments["hammer"], "2 as a pitch-bend step on the note before, held through");
    assert_eq!(ornaments["vib"], "1 as a pitch-bend vibrato");
    assert_eq!(ornaments["slide"], "1 as a pitch bend into the note");
    assert!(line["pitch_bend"].as_str().unwrap().contains("bends 2 semitone(s) each way"), "{line}");
    assert_eq!(line["note_count"], 5, "the hammered notes sound on the note before them");
    assert!(line.get("warnings").is_none() && line.get("keyswitches").is_none(), "{line}");

    // The curve is in the clip, beside the notes: up a whole tone is the top of this wheel.
    let bend = wheel(client, &line["clip_id"]).await;
    assert_eq!(held(&bend, 1.0), Some(1.0), "the bend has arrived");
    assert_eq!(held(&bend, 2.0), Some(0.0), "the next note is struck in tune");
    assert_eq!(held(&bend, 2.5), Some(0.5), "a semitone up, hammered on");
    assert_eq!(held(&bend, 3.0), Some(0.0), "and pulled off");
    assert_eq!(held(&bend, 6.0), Some(-1.0), "the slide comes from a minor third below, as far as the wheel reaches");
    assert_eq!(held(&bend, 7.0), Some(0.0));
    assert!(
        joined(&line, "notes").contains("A bend of 3 semitones was asked for, and this instrument bends 2"),
        "{line}"
    );

    // A wider wheel, once the track is told: the same bends are a smaller part of it, and the slide,
    // now within reach, is played without a new attack.
    ok(client, "set_instrument_profile", json!({ "track": lead, "bend_range": 12, "remember": false })).await;
    let wide =
        ok(client, "write_melody", json!({ "clip": line["clip_id"], "key": "A minor", "phrase": phrase, "gate": 1 }))
            .await;
    assert!(wide.get("notes").is_none(), "nothing is out of reach now: {wide}");
    assert_eq!(wide["ornaments"]["slide"], "1 as a pitch-bend glide from the note before, held through");
    let bend = wheel(client, &line["clip_id"]).await;
    let highest = bend.iter().map(|point| point.1).fold(0.0, f64::max);
    assert!((highest - 3.0 / 12.0).abs() < 0.01, "the glide up a minor third is the widest: {highest}");
    assert_eq!(notes_of(client, &line["clip_id"]).await.len(), 4, "rewritten, not layered");

    // A line without ornaments takes the wheel back from what was there.
    ok(client, "write_melody", json!({ "clip": line["clip_id"], "key": "A minor", "phrase": "1:4 | 5:4" })).await;
    let bend = wheel(client, &line["clip_id"]).await;
    assert!(bend.iter().all(|point| point.1 == 0.0), "{bend:?}");

    // Over chords the wheel bends them too, which is said.
    let both = track(client, "Both", LEAD).await;
    let chords = ok(client, "write_chords", json!({ "track": both, "progression": "Am | F" })).await;
    let over = ok(
        client,
        "write_melody",
        json!({ "clip": chords["clip_id"], "mode": "add", "key": "A minor", "phrase": "5:4!bend=6 | 4:4" }),
    )
    .await;
    assert!(joined(&over, "warnings").contains("The pitch wheel moves every note of a track at once"), "{over}");

    // A mark that cannot be played says why, and nothing is written.
    let error =
        call(client, "write_melody", json!({ "track": lead, "key": "A minor", "phrase": "5!hammer 6", "start": 16 }))
            .await;
    assert!(error.unwrap_err().contains("'hammer' joins a note to the one before it"));
    let error =
        call(client, "write_melody", json!({ "track": lead, "key": "A minor", "phrase": "5!palm_mute", "start": 16 }))
            .await;
    assert!(error.unwrap_err().contains("has no articulations to ask for by name"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bend_is_written_for_the_wheel_a_built_in_instrument_is_set_to() {
    let fixture = connect().await;
    let client = &fixture.client;
    let lead = track(client, "Lead", LEAD).await;
    let shown_track = shown(client, lead).await;
    let synth = shown_track["devices"][0]["id"].as_u64().unwrap();
    // Where nothing is set, the synth's wheel bends two semitones, and the track says where that comes from.
    let profile = ok(client, "get_instrument_profile", json!({ "track": lead })).await;
    assert!(profile["pitch_wheel"].as_str().unwrap().starts_with("bends 2 semitone(s) each way"), "{profile}");
    assert!(profile["profile"].get("bend_range").is_none(), "the track itself says nothing: {profile}");

    // Set to an octave on the device, the same bends are a smaller part of the wheel: nothing was told to
    // the track.
    ok(client, "update_device", json!({ "device": synth, "params": { "bend_range": 12 } })).await;
    let profile = ok(client, "get_instrument_profile", json!({ "track": lead })).await;
    assert!(profile["pitch_wheel"].as_str().unwrap().starts_with("bends 12 semitone(s) each way"), "{profile}");
    let line =
        ok(client, "write_melody", json!({ "track": lead, "key": "A minor", "phrase": "1:2!bend=2 3:2", "gate": 1 }))
            .await;
    assert!(line["pitch_bend"].as_str().unwrap().contains("bends 12 semitone(s) each way"), "{line}");
    let bend = wheel(client, &line["clip_id"]).await;
    let highest = bend.iter().map(|point| point.1).fold(0.0, f64::max);
    assert!((highest - 2.0 / 12.0).abs() < 0.01, "a whole tone is a sixth of this wheel: {highest}");

    // What the track is told goes before what its instrument is set to.
    ok(client, "set_instrument_profile", json!({ "track": lead, "bend_range": 4, "remember": false })).await;
    let profile = ok(client, "get_instrument_profile", json!({ "track": lead })).await;
    assert!(profile.get("pitch_wheel").is_none() && profile["profile"]["bend_range"] == 4.0, "{profile}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ornaments_are_keyswitches_on_an_instrument_that_has_them() {
    let fixture = connect().await;
    let client = &fixture.client;
    let guitar = track(client, "Lead guitar", ECLIPSE).await;
    let phrase = ":0.5 E4 F4!hammer E4!pull r G4!slide A4!slide E3!palm_mute E3!palm_mute | E4:4!bend=F#4!vib";
    let line = ok(client, "write_melody", json!({ "track": guitar, "phrase": phrase })).await;

    let ornaments = &line["ornaments"];
    assert_eq!(ornaments["hammer"], "2 as the instrument's 'hammer' keyswitch");
    assert_eq!(ornaments["slide"], "1 as the instrument's 'glide' keyswitch; 1 as the instrument's 'slide' keyswitch");
    assert_eq!(ornaments["bend"], "1 as a pitch bend", "it has no key for a bend, so that is the wheel");
    assert_eq!(line["keyswitches"], json!({ "glide": 1, "hammer": 1, "open": 2, "palm_mute": 1, "slide": 1 }));
    assert_eq!(line["note_count"], 8, "every note is struck or joined by the instrument itself");

    let notes = notes_of(client, &line["clip_id"]).await;
    let switches: Vec<(u64, f64)> = notes.iter().filter(|note| note.0 < 36).map(|note| (note.0, note.1)).collect();
    assert_eq!(
        switches,
        [(24, 0.0), (29, 0.015), (27, 1.97), (28, 1.985), (26, 2.97), (24, 3.97)],
        "open and hammer before the run, slide and glide before theirs, palm mute, and open again for the last note"
    );
    assert_eq!(notes.iter().find(|note| note.0 == 29).unwrap().2, 40, "the joining key is pressed softly");
    // Hammered notes start while the one before still sounds.
    let read = ok(client, "get_notes", json!({ "clip": line["clip_id"] })).await;
    let first = read["notes"].as_array().unwrap().iter().find(|note| note["pitch"] == 64).unwrap();
    assert!(first["start"].as_f64().unwrap() + first["length"].as_f64().unwrap() > 0.5, "{first}");
    // Only the bend moves the wheel.
    let bend = wheel(client, &line["clip_id"]).await;
    assert!(bend.iter().all(|point| point.0 >= 4.0), "{bend:?}");

    // Analysis reads the line, not the keys that play it.
    let read = ok(client, "analyze_music", json!({ "clip": line["clip_id"] })).await;
    assert_eq!(read["left_out"], json!({ "keyswitches": 6 }));
    assert_eq!(read["pitch_range"], "E3-A4");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_song_from_before_its_instruments_were_known_takes_what_is_known_when_it_is_opened() {
    let fixture = connect().await;
    let client = &fixture.client;
    let guitar = track(client, "Rhythm", ECLIPSE).await;
    let kit = track(client, "Drums", SUPERIOR).await;
    let keys = track(client, "Keys", LEAD).await;
    // A track that says something of its own keeps it.
    ok(client, "set_instrument_profile", json!({ "track": keys, "range": ["C2", "C6"], "remember": false })).await;
    let riff = ok(
        client,
        "write_chords",
        json!({ "track": guitar, "progression": "E5 | G5", "low": "E2", "high": "E4", "inversion": 0,
                "articulation": "palm_mute" }),
    )
    .await;
    assert_eq!(riff["keyswitches"], json!({ "palm_mute": 1 }));

    // The song as an older Trill wrote it: its tracks say nothing about their instruments.
    let file = fixture.folder.path().join("older.trill");
    ok(client, "save_project", json!({ "path": file.display().to_string() })).await;
    let mut song: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for track in song["tracks"].as_array_mut().unwrap() {
        if track["name"] != "Keys" {
            assert!(track.as_object_mut().unwrap().remove("profile").is_some(), "{}", track["name"]);
        }
    }
    std::fs::write(&file, song.to_string()).unwrap();

    ok(client, "load_project", json!({ "path": file.display().to_string() })).await;
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["unsaved_changes"], false, "opening a song does not change it");
    let rhythm = shown(client, guitar).await;
    assert!(strings(&rhythm["articulations"]).contains(&"palm_mute".to_string()), "{rhythm}");
    assert_eq!(shown(client, kit).await["plays"], "drums");
    assert_eq!(shown(client, keys).await["range"], "C2-C6", "what a track says itself is left alone");
    let known = ok(client, "get_instrument_profile", json!({ "track": guitar })).await;
    assert_eq!(known["known"], json!({ "source": "built_in", "same_as_track": true }));
    // So it is read as it is meant, and written for by name, like a song made today.
    let read = ok(client, "analyze_music", json!({ "track": guitar })).await;
    assert_eq!(read["left_out"], json!({ "keyswitches": 1 }));
    assert_eq!(strings(&read["chords"]), ["1: E5", "2: G5"]);
    let beat = ok(client, "write_drums", json!({ "track": kit, "lanes": { "wide_open_hat": "x..." } })).await;
    assert_eq!(notes_of(client, &beat["clip_id"]).await[0].0, 26);
}

#[tokio::test(flavor = "multi_thread")]
async fn what_a_probe_found_is_written_down_once_and_known_in_every_song() {
    let fixture = connect().await;
    let client = &fixture.client;
    let bass = track(client, "Bass", SAMPLER).await;
    assert!(shown(client, bass).await.get("range").is_none(), "nothing is known yet");

    // One call, in the words a probe reports: a range as runs of keys, an offset in semitones,
    // keys as "name (number)", a keyswitch that latches.
    let said = ok(
        client,
        "set_instrument_profile",
        json!({
            "track": bass,
            "range": "E1-G4 (28-67)",
            "offset_semitones": -12,
            "articulations": {
                "staccato": { "key": "C0 (12)", "latches": true },
                "slide": { "key": 14, "lasts": "once" },
            },
            "sounds": { "fret_noise": "A5 (81)" },
            "bend_range": 12,
            "notes": "Sounds an octave below its keys.",
        }),
    )
    .await;
    assert_eq!(said["profile"]["range"], json!([28, 67]));
    assert_eq!(said["profile"]["sounds_octaves"], -1);
    assert_eq!(said["profile"]["articulations"], json!({ "staccato": 12, "slide": { "key": 14, "lasts": "once" } }));
    assert_eq!(said["profile"]["sounds"], json!({ "fret_noise": 81 }));
    let file = said["remembered"]["file"].as_str().unwrap_or_else(|| panic!("{said}"));
    assert!(Path::new(file).starts_with(fixture.folder.path().join("config").join("instruments")), "{file}");
    assert!(std::fs::read_to_string(file).unwrap().contains("\"uri\": \"vst3:C0000000000000000000000000000001\""));
    assert_eq!(said["remembered"]["for"], format!("Sampled Bass ({SAMPLER})"));
    assert_eq!(said["known"], json!({ "source": "learned", "same_as_track": true }));

    // The writing tools use it at once: what is asked for is what is heard, an octave up on the keys.
    let line =
        ok(client, "write_bass", json!({ "track": bass, "progression": "Am | F | C | G", "articulation": "staccato" }))
            .await;
    assert_eq!((line["pitch_range"].as_str(), line["written_range"].as_str()), (Some("F1-C2"), Some("F2-C3")));
    assert!(joined(&line, "notes").contains("sounds 1 octave(s) below its keys"), "{line}");
    assert_eq!(line["keyswitches"], json!({ "staccato": 1 }));
    assert!(line.get("warnings").is_none(), "{line}");
    let notes = notes_of(client, &line["clip_id"]).await;
    assert_eq!((notes[0].0, notes[1].0), (12, 45), "the keyswitch, then A2 for an A that sounds as A1");
    // And analysis reads it where it sounds.
    let read = ok(client, "analyze_music", json!({ "track": bass })).await;
    assert_eq!(read["pitch_range"], "F1-C2");
    assert_eq!(read["left_out"]["keyswitches"], 1);
    assert!(read["left_out"]["read_at"].as_str().unwrap().contains("the octave the instrument sounds at"), "{read}");
    // A part asked for below what it can sound is warned about, by its keys.
    let low =
        ok(client, "write_bass", json!({ "track": bass, "start": 16, "progression": "D", "low": "C0", "high": "C1" }))
            .await;
    assert!(joined(&low, "warnings").contains("outside this instrument's range (E1-G4)"), "{low}");

    // The next track given the plugin starts out knowing, in this song...
    let second = track(client, "Bass 2", SAMPLER).await;
    let line = shown(client, second).await;
    assert_eq!((line["range"].as_str(), line["sounds_octaves"].as_i64()), (Some("E1-G4"), Some(-1)));
    assert_eq!(strings(&line["articulations"]), ["slide", "staccato"]);
    // ...and undoing what the first track was told does not unlearn the plugin.
    ok(client, "history", json!({ "action": "undo", "steps": 5 })).await;
    let entry = ok(client, "get_instrument_profile", json!({ "uri": SAMPLER })).await;
    assert_eq!((entry["source"].as_str(), entry["profile"]["bend_range"].as_f64()), (Some("learned"), Some(12.0)));

    // Another run of the program, another song: it is read from the user's folder.
    let (later, _server) = serve(fixture.folder.path()).await;
    let bass = track(&later, "Bass", SAMPLER).await;
    assert_eq!(shown(&later, bass).await["range"], "E1-G4");
    let all = ok(&later, "get_instrument_profile", json!({})).await;
    let learned: Vec<&Value> =
        all["instruments"].as_array().unwrap().iter().filter(|known| known["source"] == "learned").collect();
    assert_eq!(learned.len(), 1);
    assert!(learned[0]["knows"].as_str().unwrap().contains("range E1-G4; sounds -1 octave(s)"), "{}", learned[0]);

    // Forgetting takes the file away and leaves the track with what ships: for this plugin, nothing.
    let forgotten = ok(&later, "set_instrument_profile", json!({ "track": bass, "forget": true })).await;
    assert!(forgotten["forgotten"].as_str().unwrap().contains("what was learned about Sampled Bass"), "{forgotten}");
    assert_eq!((forgotten["profile"].clone(), forgotten["known"].clone()), (json!({}), json!({ "source": "none" })));
    assert!(!Path::new(file).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profile_that_cannot_be_true_is_refused_and_changes_nothing() {
    let fixture = connect().await;
    let client = &fixture.client;
    let guitar = track(client, "Rhythm", ECLIPSE).await;
    let before = ok(client, "get_instrument_profile", json!({ "track": guitar })).await;
    for (arguments, reason) in [
        (json!({ "range": [90, 30] }), "upside down"),
        (json!({ "range": "nonsense" }), "`range` \"nonsense\" should be the lowest and highest key"),
        (json!({ "range": [36] }), "should be the lowest and highest key"),
        (json!({ "role": "percussion" }), "`role` 'percussion' should be \"drums\", \"pitched\" or \"auto\""),
        (json!({ "articulations": { "tap": 40 } }), "the keyswitch of 'tap' (E2) is inside the range C2..C6"),
        (json!({ "articulations": { "x": { "lasts": "forever", "key": 1 } } }), "`lasts` \"forever\" should be"),
        (
            json!({ "articulations": { "x": { "key": 1, "pedal": true } } }),
            "'pedal' is not something an articulation has",
        ),
        (json!({ "articulations": { "x": { "velocity": [0, 300] } } }), "should be two velocities from 1 to 127"),
        (json!({ "articulations": { "x": {} } }), "neither a keyswitch nor a velocity range"),
        (
            json!({ "articulations": { "muted": { "key": 24, "default": true } } }),
            "only one articulation can be the default",
        ),
        (json!({ "sounds": { "scrape": "H9" } }), "sound 'scrape': 'H9' is not a note name"),
        (json!({ "offset_semitones": -7 }), "is not a whole number of octaves"),
        (json!({ "sounds_octaves": -1, "offset_semitones": -12 }), "not both"),
        (json!({ "bend_range": 0 }), "bend range"),
        (json!({ "forget": true, "range": [36, 84] }), "give it alone"),
    ] {
        let mut arguments = arguments;
        arguments["track"] = json!(guitar);
        let error = call(client, "set_instrument_profile", arguments.clone()).await;
        let error = error.err().unwrap_or_else(|| panic!("{arguments} should be refused"));
        assert!(error.contains(reason), "{arguments}\n  said: {error}\n  expected: {reason}");
    }
    let error = call(client, "set_instrument_profile", json!({ "track": 999, "role": "drums" })).await.unwrap_err();
    assert!(error.contains("track 999 not found") && error.contains("(Rhythm)"), "{error}");
    let error = call(client, "get_instrument_profile", json!({ "track": guitar, "uri": ECLIPSE })).await.unwrap_err();
    assert!(error.contains("not both"), "{error}");
    assert_eq!(ok(client, "get_instrument_profile", json!({ "track": guitar })).await, before);
    assert_eq!(ok(client, "get_instrument_profile", json!({ "uri": ECLIPSE })).await["source"], "built_in");

    // Changing one thing keeps the rest; `replace` starts over; null takes one name away.
    let changed = ok(
        client,
        "set_instrument_profile",
        json!({ "track": guitar, "range": ["C2", "E6"], "articulations": { "tap": null } }),
    )
    .await;
    assert_eq!(changed["profile"]["range_names"], "C2-E6");
    assert!(changed["profile"]["articulations"].get("tap").is_none());
    assert_eq!(changed["profile"]["articulations"]["palm_mute"], 26, "the rest is as it was");
    assert_eq!(changed["known"], json!({ "source": "learned", "same_as_track": true }));
    let fresh = ok(
        client,
        "set_instrument_profile",
        json!({ "track": guitar, "replace": true, "range": [40, 88], "remember": false }),
    )
    .await;
    assert_eq!(fresh["profile"], json!({ "range": [40, 88], "range_names": "E2-E6" }));
    // What was learned over the shipped entry can be forgotten, and the entry is back.
    let back = ok(client, "set_instrument_profile", json!({ "track": guitar, "forget": true })).await;
    assert_eq!(back["known"], json!({ "source": "built_in", "same_as_track": true }));
    assert_eq!(back["profile"]["articulations"]["tap"], 30);
}

// ---- the real thing ------------------------------------------------------------------
//
// The entries Trill ships, proved on the plugins themselves where they are
// installed: `cargo test -p trill-mcp --test instruments -- --include-ignored --nocapture`.

/// Ample Metal R, the five-string bass.
const RAY5: &str = "vst3:ABCDEF019182FAEB41707364416D7274";

/// A server whose devices are the plugins installed on this machine, and
/// whose settings are in a folder of its own; `None` if `uri` is not among
/// the plugins. They are found with caches of the tests' own, the ones
/// `tests/hearing.rs` keeps, so that they are scanned once for both.
async fn connect_installed(uri: &str) -> Option<Fixture> {
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
    if !host.clap().descriptors().into_iter().chain(host.vst3().descriptors()).any(|found| found.uri == uri) {
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
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Some(Fixture { client, _server: server, folder })
}

/// The highest sample of `audio` in each of the windows that start at
/// `starts` seconds and last `seconds`, in dB.
fn levels(audio: &[f32], starts: &[f64], seconds: f64) -> Vec<f64> {
    let rate = 48_000.0;
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

/// How often `audio` crosses zero in a second, in the stretch that starts
/// at `start` seconds and lasts `seconds`: higher for a brighter sound.
fn crossings(audio: &[f32], start: f64, seconds: f64) -> f64 {
    let rate = 48_000.0;
    let from = ((start * rate) as usize).min(audio.len());
    let to = (((start + seconds) * rate) as usize).min(audio.len());
    let crossed = audio[from..to].windows(2).filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0)).count();
    crossed as f64 / seconds
}

/// With the installed Ample Metal Eclipse: a note written palm-muted, by
/// the name its shipped entry gives the keyswitch, is the dull chug of a
/// muted string, and the same note written plainly between two of them
/// rings bright. Which also says that a keyswitch pressed 0.03 beats before
/// its note is in time, and that the open one brings the ordinary way of
/// playing back.
///
/// Through the amplifier inside the plugin a palm-muted note does not die
/// away sooner than an open one (both are still at full level half a second
/// on), so it is told by its brightness.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Ample Metal Eclipse (VST3) installed; run with `cargo test -p trill-mcp --test instruments -- --include-ignored --nocapture`"]
async fn a_palm_muted_note_is_duller_than_an_open_one_on_the_installed_ample_guitar() {
    let Some(fixture) = connect_installed(ECLIPSE).await else { return };
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 120 })).await;
    let guitar = track(client, "Guitar", ECLIPSE).await;
    let known = ok(client, "get_instrument_profile", json!({ "track": guitar })).await;
    assert_eq!(known["known"], json!({ "source": "built_in", "same_as_track": true }), "{known}");

    // A second of each with a second of nothing after it: palm-muted, open, palm-muted again.
    let phrase = "E3:2!palm_mute r:2 | E3:2 r:2 | E3:2!palm_mute r:2";
    let line = ok(client, "write_melody", json!({ "track": guitar, "phrase": phrase, "gate": 1 })).await;
    assert_eq!(line["keyswitches"], json!({ "open": 1, "palm_mute": 2 }), "{line}");
    let wav = fixture.folder.path().join("guitar.wav");
    let begun = std::time::Instant::now();
    let report = ok(client, "render", json!({ "path": wav.display().to_string(), "max_tail_seconds": 1 })).await;
    let audio = trill_media::decode(&wav).unwrap();
    let starts = [0.0, 2.0, 4.0];
    let begins = levels(&audio.left, &starts, 0.15);
    let later = levels(&audio.left, &starts.map(|start| start + 0.5), 0.3);
    let bright = starts.map(|start| crossings(&audio.left, start + 0.05, 0.9));
    eprintln!(
        "rendered in {:.1?}: palm-muted, open, palm-muted begin at {begins:.1?} dB, are at {later:.1?} dB half a \
         second on, and cross zero {bright:.0?} times a second; notes {}",
        begun.elapsed(),
        report["notes"]
    );
    assert!(begins.iter().all(|level| *level > -50.0), "all three notes sound: {begins:?}");
    for muted in [bright[0], bright[2]] {
        assert!(bright[1] > 1.5 * muted, "the open note is brighter than a palm-muted one: {bright:?}");
    }
}

/// With the installed Ample guitars: what a probe hears of the keys their
/// shipped entries name. Holding `harmonic` plays the octave above,
/// `pinch_harmonic` higher still, `palm_mute` a duller note at the same
/// pitch; and on the two where the entry says a softly hit string is
/// muted, a note hit softly is duller than one hit hard.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Ample Guitar PF, Metal Eclipse or Metal Hellrazer (VST3) installed; run with `cargo test -p trill-mcp --test instruments -- --include-ignored --nocapture`"]
async fn a_probe_hears_what_the_shipped_entries_say_of_the_installed_ample_guitars() {
    const PF: &str = "vst3:ABCDEF019182FAEB4170736441677066";
    const HELLRAZER: &str = "vst3:ABCDEF019182FAEB41707364416D6872";
    for uri in [PF, ECLIPSE, HELLRAZER] {
        let Some(fixture) = connect_installed(uri).await else { continue };
        let client = &fixture.client;
        let guitar = track(client, "Guitar", uri).await;
        let read = ok(client, "get_instrument_profile", json!({ "track": guitar })).await;
        assert_eq!(read["known"], json!({ "source": "built_in", "same_as_track": true }), "{read}");
        eprintln!("{}:", read["instrument"]["name"]);
        let profile = &read["profile"];
        let key = |name: &str| {
            let articulation = &profile["articulations"][name];
            articulation.as_u64().or_else(|| articulation["key"].as_u64()).unwrap_or_else(|| panic!("no {name}"))
        };

        // E3 by itself with each key held, as (the pitch heard, its brightness).
        let mut heard = std::collections::BTreeMap::new();
        for name in ["open", "harmonic", "palm_mute", "pinch_harmonic"] {
            let probed = ok(client, "probe", json!({ "track": guitar, "notes": ["E3"], "hold": [key(name)] })).await;
            let entry = probed["heard"][0].clone();
            eprintln!("   {name} ({}): {entry}", key(name));
            let pitch = entry["heard"].as_str().unwrap_or("none").to_string();
            heard.insert(name, (pitch, entry["bright_hz"].as_f64().unwrap()));
        }
        assert_eq!(heard["open"].0, "E3", "{uri}");
        assert_eq!(heard["harmonic"].0, "E4", "{uri}: a natural harmonic is the octave above");
        assert_eq!(heard["palm_mute"].0, "E3", "{uri}");
        assert!(heard["open"].1 > 1.5 * heard["palm_mute"].1, "{uri}: a palm-muted note is duller: {heard:?}");
        // A pinch harmonic squeals: heard above the note on the metal guitars, and on the PRS at
        // the note's own pitch but brighter.
        let (pinch, open) = (&heard["pinch_harmonic"], &heard["open"]);
        assert!(pinch.0 != "E3" || pinch.1 > 1.3 * open.1, "{uri}: a pinch harmonic: {heard:?}");

        // How hard a note is hit, with open chosen: the entry's `muted` and `open`.
        if profile["articulations"].get("muted").is_none() {
            continue;
        }
        let hit = ok(
            client,
            "probe",
            json!({ "track": guitar, "notes": ["E3"], "hold": [key("open")], "velocities": [50, 100], "detail": true }),
        )
        .await;
        let bright: Vec<f64> =
            hit["heard"].as_array().unwrap().iter().map(|entry| entry["bright_hz"].as_f64().unwrap()).collect();
        eprintln!("   hit at 50 and at 100: {bright:?} Hz");
        assert!(bright[1] > 1.5 * bright[0], "{uri}: a softly hit note is a muted one: {bright:?}");
    }
}

/// With the installed Ample Metal R: it sounds an octave below its keys,
/// as its shipped entry says, so a line asked for by the notes to be heard
/// is written an octave up, and a probe hears it where it was asked for.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Ample Metal R (VST3) installed; run with `cargo test -p trill-mcp --test instruments -- --include-ignored --nocapture`"]
async fn the_installed_ample_bass_is_heard_an_octave_below_its_keys_as_its_entry_says() {
    let Some(fixture) = connect_installed(RAY5).await else { return };
    let client = &fixture.client;
    let bass = track(client, "Bass", RAY5).await;
    assert_eq!(shown(client, bass).await["sounds_octaves"], -1);

    let line = ok(client, "write_melody", json!({ "track": bass, "phrase": "E1 B1 E2 A2", "gate": 0.5 })).await;
    assert_eq!((&line["pitch_range"], &line["written_range"]), (&json!("E1-A2"), &json!("E2-A3")), "{line}");
    let keys: Vec<u64> =
        notes_of(client, &line["clip_id"]).await.iter().map(|note| note.0).filter(|key| *key >= 33).collect();
    assert_eq!(keys, [40, 47, 52, 57]);

    let begun = std::time::Instant::now();
    let probed = ok(client, "probe", json!({ "track": bass, "notes": keys, "detail": true })).await;
    eprintln!("probed in {:.1?}: pitch {}; known {}", begun.elapsed(), probed["pitch"], probed["known"]);
    for entry in probed["heard"].as_array().unwrap() {
        eprintln!("   {entry}");
    }
    assert!(probed["pitch"].as_str().unwrap().starts_with("sounds an octave below what is written"), "{probed}");
    assert!(probed["known"].as_str().unwrap().contains("ships with an entry"), "{probed}");
    // Heard where the line was asked for. This bass answers a note that follows another closely
    // with a noise of its fingers now and then, which has no pitch: one of the four may be that.
    let asked = ["E1", "B1", "E2", "A2"];
    let heard: Vec<Option<&str>> =
        probed["heard"].as_array().unwrap().iter().map(|entry| entry["heard"].as_str()).collect();
    assert_eq!(heard.len(), asked.len(), "{probed}");
    let right = heard.iter().zip(asked).filter(|(heard, asked)| **heard == Some(*asked)).count();
    let wrong = heard.iter().zip(asked).filter(|(heard, asked)| heard.is_some_and(|heard| heard != *asked)).count();
    assert!(right >= 3 && wrong == 0, "{heard:?}");
}

/// With the installed Superior Drummer 3: the drums its shipped entry
/// names are on the keys it gives them, every one of them sounds, and they
/// are what their names say as far as a measurement tells (an open hat
/// rings longer than a closed one, a cymbal is brighter than a kick).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Superior Drummer 3 (VST3) installed; run with `cargo test -p trill-mcp --test instruments -- --include-ignored --nocapture`"]
async fn the_drums_of_the_installed_superior_drummer_3_are_on_the_keys_its_entry_names() {
    let Some(fixture) = connect_installed(SUPERIOR).await else { return };
    let client = &fixture.client;
    let kit = track(client, "Drums", SUPERIOR).await;
    assert_eq!(shown(client, kit).await["plays"], "drums");

    // One hit of each, by name, a step apart.
    let drums = [
        ("kick", 36),
        ("snare", 38),
        ("rimshot", 40),
        ("closed_hat", 42),
        ("closed_hat_edge", 22),
        ("open_hat", 46),
        ("wide_open_hat", 26),
        ("high_tom", 48),
        ("floor_tom", 41),
        ("ride", 51),
        ("ride_bell", 53),
        ("china", 52),
        ("splash", 55),
        ("cymbal_1", 86),
    ];
    let lanes: serde_json::Map<String, Value> = drums
        .iter()
        .enumerate()
        .map(|(step, (name, _))| {
            let steps: String = (0..16).map(|at| if at == step { 'x' } else { '.' }).collect();
            ((*name).to_string(), json!(steps))
        })
        .collect();
    let beat = ok(client, "write_drums", json!({ "track": kit, "lanes": lanes })).await;
    assert!(beat.get("notes").is_none(), "none of them needed a stand-in: {beat}");
    let keys: Vec<u64> = notes_of(client, &beat["clip_id"]).await.iter().map(|note| note.0).collect();
    assert_eq!(keys, drums.map(|(_, key)| key), "each name on the key the entry gives it");

    let begun = std::time::Instant::now();
    let probed = ok(client, "probe", json!({ "track": kit, "notes": keys, "detail": true, "tail": 3 })).await;
    eprintln!("probed in {:.1?}: sounding {}; silent {}", begun.elapsed(), probed["sounding"], probed["silent"]);
    let heard = probed["heard"].as_array().unwrap();
    // What was measured of the drum on `key`: how long it rings, and how bright it is.
    let measured = |name: &str| -> (f64, f64) {
        let key = drums.iter().find(|drum| drum.0 == name).unwrap().1;
        let entry = heard
            .iter()
            .find(|entry| entry["notes"].as_str().unwrap().ends_with(&format!("({key})")))
            .unwrap_or_else(|| panic!("{name} ({key}) was not played: {probed}"));
        assert!(entry.get("silent").is_none(), "{name} ({key}) makes no sound: {entry}");
        let rings = match &entry["rings_s"] {
            Value::String(longer) => longer.trim_start_matches('>').parse().unwrap(),
            seconds => seconds.as_f64().unwrap(),
        };
        (rings, entry["bright_hz"].as_f64().unwrap())
    };
    for (name, key) in drums {
        let (rings, bright) = measured(name);
        eprintln!("   {name} ({key}): rings {rings:.2} s, {bright:.0} Hz");
    }
    assert!(probed.get("silent").is_none(), "{probed}");
    assert!(measured("open_hat").0 > measured("closed_hat").0, "an open hat rings longer than a closed one");
    assert!(measured("wide_open_hat").0 > measured("closed_hat_edge").0, "and so with the two the kit adds");
    for cymbal in ["china", "splash", "cymbal_1", "ride"] {
        assert!(measured(cymbal).1 > 2.0 * measured("kick").1, "{cymbal} is brighter than the kick");
        assert!(measured(cymbal).0 > measured("closed_hat").0, "{cymbal} rings longer than a closed hat");
    }
    assert!(measured("floor_tom").1 < measured("splash").1, "a floor tom is darker than a splash");

    // General MIDI's high tom key is no tom on this kit, which is why the entry moves the name.
    let other = ok(client, "probe", json!({ "track": kit, "notes": [50], "tail": 3 })).await;
    let on_50 = other["heard"][0]["bright_hz"].as_f64().unwrap();
    eprintln!("   key 50: {}", other["heard"][0]);
    assert!(other["heard"][0].get("drum_name").is_none(), "and the probe does not call it one");
    let named = heard.iter().find(|entry| entry["notes"] == "C3 (48)").unwrap();
    assert_eq!(named["drum_name"], "high_tom", "the probe calls drums what the kit does");
    assert!(on_50 > 5.0 * measured("high_tom").1, "key 50 is far brighter than a tom: {on_50} Hz");
}
