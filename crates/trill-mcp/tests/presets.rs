//! A device's presets over MCP: listed, searched and paged the same way
//! for a built-in device and for a plugin with thousands, and loaded by
//! name or id. The plugin is a stand-in that exists only here, so the
//! tests need nothing installed; `trill-plugins` tests the real formats.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use trill_core::device::{
    DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    Event, ParamInfo, ParamKind, PresetLoadError, ProcessInfo,
};
use trill_core::presets::{PresetInfo, PresetList, PresetStatus, short_id};
use trill_core::{Device, DeviceId};
use trill_engine::AudioBackend;
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

/// The plugin with presets, and the same plugin "in another format",
/// which lists none.
const SYNTH: &str = "clap:test.big-synth";
const SYNTH_VST3: &str = "vst3:0123456789ABCDEF0123456789ABCDEF";
const POLYSYNTH: &str = "builtin:polysynth";

#[derive(Default)]
struct World {
    list: PresetList,
    /// The patch each plugin has, by device.
    patches: HashMap<DeviceId, String>,
    /// How often the presets were waited for.
    waits: usize,
}

#[derive(Clone, Default)]
struct Synths(Arc<Mutex<World>>);

impl Synths {
    fn patch(&self, device: u64) -> String {
        self.0.lock().unwrap().patches.get(&DeviceId(device)).cloned().unwrap_or_default()
    }
}

struct Silent;

impl DeviceProcessor for Silent {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}
    fn set_param(&mut self, _index: usize, _value: f64) {}
    fn process(&mut self, _: &ProcessInfo, _: &[Event], _: &mut [f32], _: &mut [f32]) {}
    fn reset(&mut self) {}
}

struct Controller {
    device: DeviceId,
    world: Arc<Mutex<World>>,
}

impl DeviceController for Controller {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        self.world.lock().unwrap().patches.get(&self.device).map(|patch| patch.clone().into_bytes())
    }

    fn load_preset(&mut self, preset: &PresetInfo) -> Result<(), PresetLoadError> {
        if preset.locator.ends_with("Damaged") {
            return Err(PresetLoadError::Failed("the plugin could not load it: the file is damaged".into()));
        }
        self.world.lock().unwrap().patches.insert(self.device, preset.locator.clone());
        Ok(())
    }
}

impl DeviceFactory for Synths {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        [SYNTH, SYNTH_VST3]
            .map(|uri| DeviceDescriptor {
                uri: uri.into(),
                name: "Big Synth".into(),
                vendor: "Test".into(),
                category: DeviceCategory::Instrument,
                description: "A synth with a great many presets".into(),
                params: Vec::new(),
                presets: Vec::new(),
            })
            .into()
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != SYNTH && device.uri != SYNTH_VST3 {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        let patch = device.state.as_deref().map(|state| String::from_utf8_lossy(state).into_owned());
        self.0.lock().unwrap().patches.insert(device.id, patch.unwrap_or_else(|| "init".into()));
        let tone = ParamInfo {
            id: "tone".into(),
            name: "Tone".into(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            unit: String::new(),
            kind: ParamKind::Float,
        };
        Ok(DeviceInstance {
            processor: Box::new(Silent),
            params: vec![tone],
            controller: Some(Box::new(Controller { device: device.id, world: self.0.clone() })),
        })
    }

    fn presets(&self, uri: &str) -> Option<PresetList> {
        match uri {
            SYNTH => Some(self.0.lock().unwrap().list.clone()),
            SYNTH_VST3 => Some(PresetList::default()),
            _ => None,
        }
    }

    fn presets_within(&self, uri: &str, _timeout: Duration) -> Option<PresetList> {
        if uri == SYNTH {
            self.0.lock().unwrap().waits += 1;
        }
        self.presets(uri)
    }
}

/// Three thousand presets, as a big synth has them: in banks and folders,
/// with the same names turning up in more than one.
fn presets() -> Vec<PresetInfo> {
    let kinds = ["Pads", "Leads", "Basses", "Keys", "Plucks"];
    let mut presets: Vec<PresetInfo> = (0..2996)
        .map(|n| {
            let bank = if n % 3 == 0 { "Factory".to_string() } else { format!("Third Party/Maker {}", n % 7) };
            let kind = kinds[n % kinds.len()];
            preset(&format!("{} {n}", kind.trim_end_matches('s')), &format!("{bank}/{kind}"))
        })
        .collect();
    presets.push(PresetInfo { author: "Ada".into(), tags: vec!["warm".into()], ..preset("Warm Pad", "Factory/Pads") });
    presets.push(preset("Warm Pad", "Third Party/Maker 3/Pads"));
    presets.push(preset("Glass Keys", "Factory/Keys"));
    presets.push(preset("Damaged", "User"));
    presets
}

fn preset(name: &str, category: &str) -> PresetInfo {
    let locator = format!("{category}/{name}");
    PresetInfo {
        id: short_id(&locator),
        name: name.into(),
        category: category.into(),
        locator,
        ..PresetInfo::default()
    }
}

struct Fixture {
    client: Client,
    synths: Synths,
    _server: HttpServer,
    _folder: tempfile::TempDir,
}

async fn connect() -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let storage = Storage::At {
        settings: folder.path().join("config").join("settings.json"),
        library_index: Some(folder.path().join("cache").join("library-index.json")),
    };
    let options = RuntimeOptions { audio: AudioBackend::Null { sample_rate: 48_000 }, plugins: None, storage };
    let synths = Synths::default();
    synths.0.lock().unwrap().list = PresetList::ready(presets());
    let parts = Parts { devices: vec![Box::new(synths.clone())], ..Parts::default() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { client, synths, _server: server, _folder: folder }
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

fn names(presets: &Value) -> Vec<&str> {
    presets["items"].as_array().unwrap().iter().map(|item| item["name"].as_str().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_type_lists_its_presets_a_page_at_a_time() {
    let fixture = connect().await;
    let client = &fixture.client;

    // A plugin with three thousand: the first twenty, how many there
    // are, what kinds, and where the rest is. Not three thousand lines.
    let described = ok(client, "describe_device", json!({ "uri": SYNTH })).await;
    let presets = &described["presets"];
    assert_eq!(presets["total"], 3000);
    assert_eq!(names(presets).len(), 20);
    assert!(presets["more"].as_str().unwrap().starts_with("2980 more: preset_offset 20 lists the next"));
    assert_eq!(presets["kinds"].as_array().unwrap().len(), 6, "{presets}");
    assert!(presets["kinds"][0].as_str().unwrap().starts_with("Pads (6"), "{presets}");
    assert!(described.to_string().len() < 4000, "{} bytes", described.to_string().len());
    assert_eq!(described["format"], "clap");
    assert!(presets.get("status").is_none(), "{presets}");

    // Searched: every word, anywhere; the nearest match first, each with
    // an id to ask for it by.
    let warm = ok(client, "describe_device", json!({ "uri": SYNTH, "preset_query": "warm pad" })).await;
    let warm = &warm["presets"];
    assert_eq!((warm["total"].as_u64(), warm["matching"].as_u64()), (Some(3000), Some(2)));
    assert_eq!(names(warm), ["Warm Pad", "Warm Pad"]);
    assert_eq!(warm["items"][0]["category"], "Factory/Pads");
    assert_eq!(
        (warm["items"][0]["author"].as_str(), warm["items"][0]["tags"][0].as_str()),
        (Some("Ada"), Some("warm"))
    );
    assert_eq!(warm["items"][0]["id"].as_str().map(str::len), Some(10));
    assert_ne!(warm["items"][0]["id"], warm["items"][1]["id"]);
    assert!(warm.get("more").is_none() && warm.get("kinds").is_none());

    // One category, paged through.
    let page = |offset: usize| json!({ "uri": SYNTH, "preset_category": "factory/leads", "preset_limit": 50, "preset_offset": offset });
    let first = ok(client, "describe_device", page(0)).await;
    let matching = first["presets"]["matching"].as_u64().unwrap() as usize;
    assert!(matching > 150 && matching < 250, "{matching}");
    assert_eq!(names(&first["presets"]).len(), 50);
    let last = ok(client, "describe_device", page(matching - 7)).await;
    assert_eq!((names(&last["presets"]).len(), last["presets"]["offset"].as_u64()), (7, Some(matching as u64 - 7)));
    assert!(last["presets"].get("more").is_none());
    // The most a reply lists is a hundred, whatever is asked for.
    let greedy = ok(client, "describe_device", json!({ "uri": SYNTH, "preset_limit": 100000 })).await;
    assert_eq!(names(&greedy["presets"]).len(), 100);
    let none = ok(client, "describe_device", json!({ "uri": SYNTH, "preset_query": "theremin" })).await;
    assert_eq!((none["presets"]["matching"].as_u64(), names(&none["presets"]).len()), (Some(0), 0));

    // A built-in device answers the same question the same way: its
    // presets go by their names, with what each is for.
    let built_in = ok(client, "describe_device", json!({ "uri": POLYSYNTH })).await;
    let presets = &built_in["presets"];
    assert_eq!(presets["total"].as_u64().unwrap() as usize, names(presets).len());
    assert!(names(presets).contains(&"warm_pad"));
    assert!(presets["items"][0].get("id").is_none() && presets["items"][0]["description"].is_string());
    assert!(built_in["params"].as_array().is_some_and(|params| !params.is_empty()));
    let basses = ok(client, "describe_device", json!({ "uri": POLYSYNTH, "preset_query": "bass" })).await;
    assert!(names(&basses["presets"]).contains(&"sub_bass"), "{basses}");
    assert!(!names(&basses["presets"]).contains(&"warm_pad"));
    // In the catalog, built-in devices still come with their few presets.
    let catalog = ok(client, "list_devices", json!({ "query": "poly" })).await;
    assert!(catalog["devices"][0]["presets"].as_array().is_some_and(|presets| !presets.is_empty()));

    // A plugin that lists none in this format, and does in another.
    let other = ok(client, "describe_device", json!({ "uri": SYNTH_VST3 })).await;
    assert_eq!(other["presets"]["total"], 0);
    let note = other["presets"]["note"].as_str().unwrap();
    assert!(note.contains(&format!("{SYNTH} lists 3000 presets")), "{note}");
    let error = call(client, "describe_device", json!({ "uri": "clap:nobody" })).await.unwrap_err();
    assert!(error.contains("unknown device"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn presets_that_are_still_being_read_are_said_to_be() {
    let fixture = connect().await;
    let client = &fixture.client;
    fixture.synths.0.lock().unwrap().list = PresetList::reading(presets()[..5].to_vec());
    let described = ok(client, "describe_device", json!({ "uri": SYNTH })).await;
    let status = described["presets"]["status"].as_str().unwrap();
    assert!(status.starts_with("still being read (5 found so far)"), "{status}");
    assert_eq!(names(&described["presets"]).len(), 5);
    // The tool waited for them, as far as its patience goes.
    assert_eq!(fixture.synths.0.lock().unwrap().waits, 1);

    // A preset that has not turned up yet: said to be a matter of time.
    let error = call(client, "add_device", json!({ "uri": SYNTH, "preset": "Glass Keys" })).await.unwrap_err();
    assert!(error.contains("still being read, 5 so far; ask again in a moment"), "{error}");
    assert_eq!(fixture.synths.0.lock().unwrap().waits, 2, "adding with a preset waits for the presets too");

    let failed = PresetStatus::Failed("the plugin failed while its presets were read".into());
    fixture.synths.0.lock().unwrap().list = PresetList { status: failed, presets: presets()[..5].to_vec().into() };
    let described = ok(client, "describe_device", json!({ "uri": SYNTH })).await;
    let status = described["presets"]["status"].as_str().unwrap();
    assert_eq!(status, "could not all be read: the plugin failed while its presets were read");
    assert_eq!(names(&described["presets"]).len(), 5, "what was read is listed all the same");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_is_given_a_preset_by_name_or_id_like_a_built_in_device() {
    let fixture = connect().await;
    let client = &fixture.client;
    let synths = &fixture.synths;

    // On a new track, by name.
    let track =
        ok(client, "create_track", json!({ "name": "Keys", "instrument": SYNTH, "preset": "glass keys" })).await;
    assert_eq!(track["preset"], "Glass Keys");
    let device = track["instrument_device_id"].as_u64().unwrap();
    assert_eq!(synths.patch(device), "Factory/Keys/Glass Keys");

    // What the device has, and what it could have: how many, not which.
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!(got["preset"], "Glass Keys");
    assert_eq!(got["presets"]["total"], 3000);
    assert!(got["presets"].get("items").is_none() && got["presets"]["more"].is_string(), "{got}");
    assert_eq!(got["params"].as_array().unwrap().len(), 1);
    // Asked about presets, it lists presets, and parameters only if
    // asked about those too.
    let searched =
        ok(client, "get_device", json!({ "device": device, "preset_query": "warm", "preset_category": "Maker 3" }))
            .await;
    assert_eq!(names(&searched["presets"]), ["Warm Pad"]);
    assert!(searched.get("params").is_none(), "{searched}");
    let warm_id = searched["presets"]["items"][0]["id"].as_str().unwrap().to_string();
    let both = ok(client, "get_device", json!({ "device": device, "preset_limit": 3, "query": "tone" })).await;
    assert_eq!((names(&both["presets"]).len(), both["params"].as_array().unwrap().len()), (3, 1));

    // Switched by id, with a value on top: one call, one undo step.
    let updated =
        ok(client, "update_device", json!({ "device": device, "preset": warm_id, "params": { "tone": 0.9 } })).await;
    assert_eq!(updated["preset"], "Third Party/Maker 3/Pads/Warm Pad");
    assert_eq!(updated["set"]["tone"], 0.9);
    assert_eq!(synths.patch(device), "Third Party/Maker 3/Pads/Warm Pad");
    ok(client, "history", json!({ "action": "undo" })).await;
    let undone = ok(client, "get_device", json!({ "device": device, "query": "tone" })).await;
    assert_eq!((undone["preset"].as_str(), undone["params"][0]["value"].as_f64()), (Some("Glass Keys"), Some(0.5)));
    assert_eq!(synths.patch(device), "Factory/Keys/Glass Keys");

    // By its category and name, onto a chain.
    let added = ok(client, "add_device", json!({ "uri": SYNTH, "preset": "Factory/Pads/Warm Pad" })).await;
    assert_eq!(added["preset"], "Warm Pad");
    assert_eq!(synths.patch(added["device_id"].as_u64().unwrap()), "Factory/Pads/Warm Pad");

    // A built-in device by the same tools.
    let pad = ok(client, "add_device", json!({ "uri": POLYSYNTH, "preset": "warm_pad" })).await;
    assert_eq!(pad["preset"], "warm_pad");
    let pad = pad["device_id"].as_u64().unwrap();
    let got = ok(client, "get_device", json!({ "device": pad, "query": "cutoff" })).await;
    assert_eq!(got["preset"], "warm_pad");
    assert!(names(&got["presets"]).contains(&"sub_bass"), "few enough to list unasked: {got}");
    assert_eq!(ok(client, "update_device", json!({ "device": pad, "preset": "sub_bass" })).await["preset"], "sub_bass");
    let basses = ok(client, "get_device", json!({ "device": pad, "preset_query": "bass" })).await;
    assert!(names(&basses["presets"]).contains(&"sub_bass") && basses.get("params").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preset_that_cannot_be_given_says_what_to_do_instead() {
    let fixture = connect().await;
    let client = &fixture.client;
    let synths = &fixture.synths;
    let device = ok(client, "add_device", json!({ "uri": SYNTH })).await["device_id"].as_u64().unwrap();

    // No such preset: the nearest, and where to search.
    let error = call(client, "update_device", json!({ "device": device, "preset": "glass" })).await.unwrap_err();
    assert!(error.contains("has no preset 'glass' (nearest: Factory/Keys/Glass Keys)"), "{error}");
    assert!(error.contains(&format!("describe_device with preset_query searches the presets of '{SYNTH}'")), "{error}");
    let error = call(client, "update_device", json!({ "device": device, "preset": "theremin" })).await.unwrap_err();
    assert!(error.contains("nothing like it among its 3000 presets"), "{error}");
    // A name two presets share: both, with what tells them apart.
    let error = call(client, "update_device", json!({ "device": device, "preset": "Warm Pad" })).await.unwrap_err();
    assert!(error.contains("2 presets named 'Warm Pad'"), "{error}");
    assert!(error.contains("Factory/Pads/Warm Pad (id ") && error.contains("Third Party/Maker 3/Pads/Warm Pad (id "));
    // One that the plugin fails to load.
    let error = call(client, "update_device", json!({ "device": device, "preset": "Damaged" })).await.unwrap_err();
    assert!(error.contains("did not load the preset 'Damaged'") && error.contains("the file is damaged"), "{error}");
    assert!(error.contains("The device is as it was"), "{error}");
    assert_eq!(synths.patch(device), "init", "none of it changed the plugin");
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert!(got.get("preset").is_none(), "{got}");

    // Nothing is added, and no track made, for a preset that is not had.
    let project = |project: Value| (project["tracks"].clone(), project["master"].clone());
    let before = project(ok(client, "get_project", json!({})).await);
    let error = call(client, "add_device", json!({ "uri": SYNTH, "preset": "Damaged" })).await.unwrap_err();
    assert!(error.contains("the file is damaged"), "{error}");
    let error = call(client, "create_track", json!({ "name": "Keys", "instrument": SYNTH, "preset": "theremin" }))
        .await
        .unwrap_err();
    assert!(error.contains("has no preset 'theremin'") && error.contains("No track was created"), "{error}");
    assert_eq!(project(ok(client, "get_project", json!({})).await), before);

    // A built-in device's few presets are named in the error itself.
    let error = call(client, "add_device", json!({ "uri": POLYSYNTH, "preset": "theremin" })).await.unwrap_err();
    assert!(error.contains("has no preset 'theremin'") && !error.contains("describe_device"), "{error}");
    let error = call(client, "update_device", json!({ "device": 999, "preset": "x" })).await.unwrap_err();
    assert!(error.contains("Existing devices"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_instructions_say_to_look_for_presets_first() {
    let fixture = connect().await;
    let instructions = fixture.client.peer_info().and_then(|info| info.instructions.clone()).unwrap_or_default();
    assert!(instructions.contains("describe_device `preset_query`"), "{instructions}");
    let tools = fixture.client.list_all_tools().await.unwrap();
    for name in ["describe_device", "get_device"] {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap();
        let schema = serde_json::to_string(&tool.input_schema).unwrap();
        for field in ["preset_query", "preset_category", "preset_offset", "preset_limit"] {
            assert!(schema.contains(field), "{name} has no {field}: {schema}");
        }
    }
}

// ---- against an installed plugin -------------------------------------------

const SURGE: &str = "clap:org.surge-synth-team.surge-xt";

/// What an assistant asked for a warm pad does, on the real Surge XT:
/// finds one among its thousands of patches, starts a track from it,
/// listens, changes its mind, and saves. Prints the calls' replies.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Surge XT (CLAP) installed; run with `cargo test -p trill-mcp --test presets -- --include-ignored --nocapture`"]
async fn an_assistant_puts_a_warm_pad_on_surge_xt() {
    use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};
    // The plugin host's caches are the test's own, and no preset file of
    // the user's is read.
    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-presets");
    let clap =
        ClapConfig { cache_path: Some(scratch.join("clap.json")), scanner: Scanner::Auto, ..ClapConfig::default() };
    let vst3 = Vst3Config {
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    let host = PluginHost::with_configs(clap, vst3).unwrap();
    if !host.clap().descriptors().iter().any(|descriptor| descriptor.uri == SURGE) {
        eprintln!("skipping: Surge XT is not installed as a CLAP plugin");
        return;
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
    let client = &ClientConfig::default().serve(transport).await.unwrap();
    let said = |call: &str, reply: &Value| eprintln!("\n> {call}\n{reply}");

    // Searched. The first time on a machine the patches take a while to
    // read, and the reply says so until they are.
    let search = json!({ "uri": SURGE, "preset_query": "warm pad", "preset_limit": 4 });
    let mut found = ok(client, "describe_device", search.clone()).await;
    for _ in 0..20 {
        if found["presets"].get("status").is_none() {
            break;
        }
        said("describe_device (still reading)", &found["presets"]);
        found = ok(client, "describe_device", search.clone()).await;
    }
    said(&format!("describe_device {search}"), &found["presets"]);
    let presets = &found["presets"];
    assert!(presets["total"].as_u64().unwrap() > 500, "{presets}");
    assert!(presets["matching"].as_u64().unwrap() >= 2, "{presets}");
    let (first, second) = (&presets["items"][0], &presets["items"][1]);
    let path = |item: &Value| format!("{}/{}", item["category"].as_str().unwrap(), item["name"].as_str().unwrap());

    // A track that starts from the first, and a chord to hear it by.
    let arguments = json!({ "name": "Pad", "instrument": SURGE, "preset": path(first) });
    let track = ok(client, "create_track", arguments.clone()).await;
    said(&format!("create_track {arguments}"), &track);
    assert_eq!(track["preset"], first["name"]);
    let device = track["instrument_device_id"].as_u64().unwrap();
    let notes = json!([["C3", 0, 4, 90], ["E3", 0, 4, 90], ["G3", 0, 4, 90]]);
    ok(client, "create_clip", json!({ "track": track["track_id"], "start": 0, "length": 4, "notes": notes })).await;
    let wav = |name: &str| folder.path().join(name).display().to_string();
    let with_first = ok(client, "render", json!({ "path": wav("first.wav") })).await;
    assert!(with_first["peak_db"].as_f64().unwrap() > -60.0, "the pad is heard: {with_first}");

    // Another preset by its id, in place.
    let arguments = json!({ "device": device, "preset": second["id"] });
    let updated = ok(client, "update_device", arguments.clone()).await;
    said(&format!("update_device {arguments}"), &updated);
    assert_eq!(updated["preset"], path(second));
    let arguments = json!({ "device": device, "preset_query": "bass", "preset_limit": 3 });
    let got = ok(client, "get_device", arguments.clone()).await;
    said(&format!("get_device {arguments}"), &got);
    assert_eq!(got["preset"], second["name"]);
    assert_eq!(got["presets"]["items"].as_array().unwrap().len(), 3);
    let with_second = ok(client, "render", json!({ "path": wav("second.wav") })).await;
    let loudness = |report: &Value| report["loudness_lufs"].as_f64().unwrap();
    let (was, is) = (loudness(&with_first), loudness(&with_second));
    eprintln!("\nloudness: {was:.1} LUFS with '{}', {is:.1} LUFS with '{}'", first["name"], second["name"]);
    assert!((was - is).abs() > 0.2, "two patches, two sounds");

    // Undone, it is the first again; redone, the second; and the second
    // is what the project keeps.
    ok(client, "history", json!({ "action": "undo" })).await;
    let preset_now = |device: u64| async move {
        ok(client, "get_device", json!({ "device": device, "query": "volume" })).await["preset"].clone()
    };
    assert_eq!(preset_now(device).await, first["name"]);
    let undone = loudness(&ok(client, "render", json!({ "path": wav("undone.wav") })).await);
    assert!((undone - was).abs() < 1.5, "{was} LUFS then, {undone} now");
    ok(client, "history", json!({ "action": "redo" })).await;
    let song = folder.path().join("song.trill").display().to_string();
    ok(client, "save_project", json!({ "path": song })).await;
    ok(client, "new_project", json!({})).await;
    ok(client, "load_project", json!({ "path": song })).await;
    assert_eq!(preset_now(device).await, second["name"]);
    let reopened = loudness(&ok(client, "render", json!({ "path": wav("reopened.wav") })).await);
    assert!((reopened - is).abs() < 1.5, "{is} LUFS saved, {reopened} reopened");

    // The same plugin as a VST3 lists nothing, and says where to look.
    let catalog = ok(client, "list_devices", json!({ "query": "Surge XT" })).await;
    let is_vst3 = |device: &&Value| device["format"] == "vst3" && device["name"] == "Surge XT";
    if let Some(vst3) = catalog["devices"].as_array().unwrap().iter().find(is_vst3) {
        let described = ok(client, "describe_device", json!({ "uri": vst3["uri"] })).await;
        said("describe_device (Surge XT as a VST3)", &described["presets"]);
        assert!(described["presets"]["note"].as_str().unwrap().contains(SURGE), "{described}");
    }
}
