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
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use trill_core::device::{
    DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance, DeviceProcessor,
    Event, ParamInfo, ParamKind, PresetLoadError, ProcessInfo,
};
use trill_core::presets::{PresetFile, PresetInfo, PresetList, PresetStatus, short_id};
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

    /// A preset file of the synth's is a line of text that begins as
    /// [`PATCH_FILE`] does, and then names the patch.
    fn preset_file(&self, uri: &str, file: &Path, _within: Option<&str>) -> Option<Result<PresetInfo, String>> {
        if uri != SYNTH && uri != SYNTH_VST3 {
            return None;
        }
        Some(match std::fs::read_to_string(file) {
            Err(_) => Err("there is no such file".to_string()),
            Ok(text) => match text.strip_prefix(PATCH_FILE) {
                Some(patch) => {
                    let locator = format!("file:{}", patch.trim());
                    let name = PresetFile::name_of(file);
                    Ok(PresetInfo { id: short_id(&locator), name, locator, ..PresetInfo::default() })
                }
                None => Err("it is not a preset of Big Synth".to_string()),
            },
        })
    }

    fn preset_file_type(&self, file: &Path) -> Option<String> {
        let named_so = file.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("vstpreset"));
        let ours = named_so && std::fs::read_to_string(file).is_ok_and(|text| text.starts_with(PATCH_FILE));
        ours.then(|| SYNTH.to_string())
    }
}

/// What a preset file of the stand-in synth begins with.
const PATCH_FILE: &str = "big-synth patch ";

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
    server: HttpServer,
    folder: tempfile::TempDir,
}

impl Fixture {
    /// A file of the test's own, in the fixture's folder.
    fn file(&self, name: &str, text: &str) -> String {
        let path = self.folder.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// A client on the endpoint the assistant in the window is given.
    async fn assistant(&self) -> Client {
        let transport = StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(
            self.server.assistant_url(),
        ));
        ClientConfig::default().serve(transport).await.unwrap()
    }
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
    // The assistant's working folder is the fixture's own too.
    let http = trill_mcp::HttpOptions { assistant_folder: Some(folder.path().join("work")) };
    let server = trill_mcp::serve_http_with(runtime, "127.0.0.1:0".parse().unwrap(), http).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { client, synths, server, folder }
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

#[tokio::test(flavor = "multi_thread")]
async fn a_devices_sound_is_saved_and_given_to_another_like_any_preset() {
    let fixture = connect().await;
    let client = &fixture.client;
    let synths = &fixture.synths;
    let track =
        ok(client, "create_track", json!({ "name": "Keys", "instrument": SYNTH, "preset": "glass keys" })).await;
    let device = track["instrument_device_id"].as_u64().unwrap();
    ok(client, "update_device", json!({ "device": device, "params": { "tone": 0.9 } })).await;

    // Kept under a name, in the storage this run was given: a temporary
    // folder here, and never the user's own.
    let arguments = json!({ "device": device, "name": "Chorus Keys", "description": "bright", "tags": ["keys"] });
    let saved = ok(client, "save_preset", arguments).await;
    assert_eq!((saved["saved"].as_str(), saved["device"].as_str()), (Some("Saved/Chorus Keys"), Some(SYNTH)));
    assert!(saved.get("replaced").is_none(), "{saved}");
    let file = std::path::PathBuf::from(saved["file"].as_str().unwrap());
    assert!(file.starts_with(fixture.folder.path().join("config").join("saved-presets")), "{}", file.display());
    assert!(file.exists() && file.extension().unwrap() == "trillpreset");
    assert!(saved["use"].as_str().unwrap().contains("`preset: \"Chorus Keys\"`"), "{saved}");
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!((got["preset"].as_str(), got["presets"]["total"].as_u64()), (Some("Chorus Keys"), Some(3001)));

    // It is one of the device type's presets: listed in front of its own,
    // found by a search, and counted.
    let described = ok(client, "describe_device", json!({ "uri": SYNTH, "preset_limit": 2 })).await;
    assert_eq!(described["presets"]["total"], 3001);
    assert_eq!(described["presets"]["items"][0]["name"], "Chorus Keys");
    assert_eq!(described["presets"]["items"][0]["category"], "Saved");
    let searched = ok(client, "describe_device", json!({ "uri": SYNTH, "preset_category": "saved" })).await;
    assert_eq!(names(&searched["presets"]), ["Chorus Keys"]);
    let listed = ok(client, "list_devices", json!({ "query": "big synth" })).await;
    let entry = |uri: &str| {
        let devices = listed["devices"].as_array().unwrap();
        devices.iter().find(|device| device["uri"] == uri).unwrap_or_else(|| panic!("{uri} is not in {listed}")).clone()
    };
    assert_eq!(entry(SYNTH)["preset_count"], 3001);
    assert_eq!(entry(SYNTH)["saved_presets"], json!(["Chorus Keys"]));
    assert_eq!(entry(SYNTH_VST3)["preset_count"], 0);
    assert!(entry(SYNTH_VST3).get("saved_presets").is_none(), "saved for one format, not for the other");

    // Given to a new track by name, and to a device that has another
    // sound: the patch and the value that was set on top of it.
    let other =
        ok(client, "create_track", json!({ "name": "More Keys", "instrument": SYNTH, "preset": "Chorus Keys" })).await;
    let second = other["instrument_device_id"].as_u64().unwrap();
    assert_eq!(other["preset"], "Chorus Keys");
    assert_eq!(synths.patch(second), "Factory/Keys/Glass Keys");
    let tone = ok(client, "get_device", json!({ "device": second, "query": "tone" })).await;
    assert_eq!(tone["params"][0]["value"], 0.9);
    let third = ok(client, "add_device", json!({ "uri": SYNTH, "preset": "Factory/Pads/Warm Pad" })).await;
    let third = third["device_id"].as_u64().unwrap();
    let updated = ok(client, "update_device", json!({ "device": third, "preset": "saved/chorus keys" })).await;
    assert_eq!(updated["preset"], "Saved/Chorus Keys");
    assert_eq!(synths.patch(third), "Factory/Keys/Glass Keys");
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(synths.patch(third), "Factory/Pads/Warm Pad");

    // Saved again under the name, it replaces what was there.
    let again = ok(client, "save_preset", json!({ "device": third, "name": "chorus keys" })).await;
    assert_eq!((again["replaced"].as_bool(), again["saved"].as_str()), (Some(true), Some("Saved/chorus keys")));
    assert_eq!(
        ok(client, "describe_device", json!({ "uri": SYNTH, "preset_limit": 0 })).await["presets"]["total"],
        3001
    );

    // A built-in device's sound is kept the same way.
    let pad = ok(client, "add_device", json!({ "uri": POLYSYNTH, "preset": "warm_pad" })).await["device_id"].clone();
    ok(client, "update_device", json!({ "device": pad, "params": { "cutoff": 432 } })).await;
    let dark = ok(client, "save_preset", json!({ "device": pad, "name": "Dark Pad" })).await;
    assert_eq!(dark["saved"], "Saved/Dark Pad");
    ok(client, "update_device", json!({ "device": pad, "preset": "sub_bass" })).await;
    let back = ok(client, "update_device", json!({ "device": pad, "preset": "Dark Pad" })).await;
    assert_eq!(back["preset"], "Saved/Dark Pad");
    let cutoff = ok(client, "get_device", json!({ "device": pad, "query": "cutoff" })).await;
    assert_eq!(cutoff["params"][0]["value"], 432.0);
    assert!(names(&cutoff["presets"]).contains(&"Dark Pad"), "{cutoff}");
    let listed = ok(client, "list_devices", json!({ "query": POLYSYNTH })).await;
    assert_eq!(listed["devices"][0]["saved_presets"], json!(["Dark Pad"]));
    assert!(listed["devices"][0].get("preset_count").is_none(), "its own presets are listed by name: {listed}");

    // What cannot be saved says why.
    let error = call(client, "save_preset", json!({ "device": pad, "name": "Pads/Dark" })).await.unwrap_err();
    assert!(error.contains("cannot have '/'"), "{error}");
    let error = call(client, "save_preset", json!({ "device": 999, "name": "x" })).await.unwrap_err();
    assert!(error.contains("Existing devices"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_lists_no_presets_says_where_they_are_and_what_to_do() {
    let fixture = connect().await;
    let client = &fixture.client;
    let described = ok(client, "describe_device", json!({ "uri": SYNTH_VST3 })).await;
    let note = described["presets"]["note"].as_str().unwrap();
    assert!(
        note.starts_with("This plugin gives a host no presets to list, and keeps them in no files that Trill reads"),
        "{note}"
    );
    assert!(note.contains("in its own window, where only the user can reach them"), "{note}");
    assert!(note.contains("show_device_editor") && note.contains("save_preset"), "{note}");
    assert!(note.contains("get_device `query`"), "{note}");
    assert!(note.contains(&format!("The same plugin in another format: {SYNTH} lists 3000 presets")), "{note}");

    let device = ok(client, "add_device", json!({ "uri": SYNTH_VST3 })).await["device_id"].as_u64().unwrap();
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert!(got["presets"]["note"].as_str().unwrap().starts_with("This plugin gives a host no presets"), "{got}");
    let asked = ok(client, "get_device", json!({ "device": device, "preset_query": "pad" })).await;
    assert!(asked["presets"]["note"].as_str().unwrap().contains("save_preset"), "{asked}");

    // Once a sound of it was saved, that is its preset.
    ok(client, "save_preset", json!({ "device": device, "name": "My Sound" })).await;
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!((got["preset"].as_str(), got["presets"]["total"].as_u64()), (Some("My Sound"), Some(1)));
    assert!(got["presets"].get("note").is_none(), "{got}");
    // A built-in device without presets has no window to keep any in.
    let limiter = ok(client, "describe_device", json!({ "uri": "builtin:limiter" })).await;
    assert!(!limiter["presets"]["note"].as_str().unwrap_or_default().contains("window"), "{limiter}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sound_is_found_across_every_instrument_there_is() {
    let fixture = connect().await;
    let client = &fixture.client;
    let devices = |reply: &Value| -> Vec<String> {
        let items = reply["items"].as_array().unwrap();
        items
            .iter()
            .map(|item| format!("{}: {}", item["device"].as_str().unwrap(), item["preset"].as_str().unwrap()))
            .collect()
    };

    // The plugin's and the built-in synth's, in one answer, each with
    // what create_track takes.
    let found = ok(client, "search_presets", json!({ "query": "warm pad" })).await;
    let listed = devices(&found);
    assert_eq!(listed[..2], [format!("{SYNTH}: Warm Pad"), format!("{SYNTH}: Warm Pad")], "{found}");
    assert!(listed.contains(&format!("{POLYSYNTH}: warm_pad")), "{found}");
    assert!(found["searched"].as_str().unwrap().starts_with("30"), "thousands were looked through: {found}");
    // The synth lists none in its other format, and is not said to be
    // without presets for that: it has three thousand.
    assert!(found.get("without_presets").is_none(), "{found}");
    assert!(found.get("still_reading").is_none(), "{found}");
    let first = &found["items"][0];
    let made =
        ok(client, "create_track", json!({ "name": "Pad", "instrument": first["device"], "preset": first["id"] }))
            .await;
    assert_eq!(made["preset"], "Warm Pad");
    assert_eq!(fixture.synths.patch(made["instrument_device_id"].as_u64().unwrap()), "Factory/Pads/Warm Pad");

    // A saved sound is found with the rest.
    ok(client, "save_preset", json!({ "device": made["instrument_device_id"], "name": "Warm Pad Deluxe" })).await;
    let found = ok(client, "search_presets", json!({ "query": "warm pad deluxe" })).await;
    assert_eq!(devices(&found), [format!("{SYNTH}: Warm Pad Deluxe")]);
    assert_eq!(found["items"][0]["category"], "Saved");

    // Narrowed to one device, paged, and among the effects (of which this
    // run has only built-in ones).
    let narrowed = ok(client, "search_presets", json!({ "query": "pad", "device": "big", "limit": 3 })).await;
    assert_eq!(narrowed["items"].as_array().unwrap().len(), 3);
    assert!(devices(&narrowed).iter().all(|found| found.starts_with(SYNTH)), "{narrowed}");
    assert!(narrowed["more"].as_str().unwrap().ends_with("offset 3 lists the next"), "{narrowed}");
    let next = ok(client, "search_presets", json!({ "query": "pad", "device": "big", "limit": 3, "offset": 3 })).await;
    assert_ne!(next["items"], narrowed["items"]);
    let effects = ok(client, "search_presets", json!({ "query": "warm pad", "category": "effect" })).await;
    assert!(devices(&effects).iter().all(|found| !found.starts_with(SYNTH)), "{effects}");
    let any = ok(client, "search_presets", json!({ "query": "glass keys", "category": "any" })).await;
    assert_eq!(devices(&any), [format!("{SYNTH}: Glass Keys")]);

    // While a plugin's presets are being read, the answer says so and has
    // what is known so far.
    fixture.synths.0.lock().unwrap().list = PresetList::reading(presets());
    let partial = ok(client, "search_presets", json!({ "query": "glass keys", "wait_seconds": 0 })).await;
    assert_eq!(partial["still_reading"], json!([format!("Big Synth ({SYNTH})")]));
    assert!(partial["status"].as_str().unwrap().contains("search again in a few seconds"), "{partial}");
    assert_eq!(devices(&partial), [format!("{SYNTH}: Glass Keys")]);

    let error = call(client, "search_presets", json!({ "query": " " })).await.unwrap_err();
    assert!(error.contains("give `query`"), "{error}");
    let error = call(client, "search_presets", json!({ "query": "pad", "category": "drums" })).await.unwrap_err();
    assert!(error.contains("\"instrument\", \"effect\" or \"any\""), "{error}");
    let error = call(client, "search_presets", json!({ "query": "pad", "device": "nonesuch" })).await.unwrap_err();
    assert!(error.contains("no installed device is called anything like 'nonesuch'"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_instructions_say_to_search_everything_before_settling_for_a_built_in_synth() {
    let fixture = connect().await;
    let instructions = fixture.client.peer_info().and_then(|info| info.instructions.clone()).unwrap_or_default();
    for said in [
        "search_presets finds a sound by words",
        "before settling for a built-in synth",
        "save_preset keeps that sound under a name",
        "as the plugin displays it (`text`)",
    ] {
        assert!(instructions.contains(said), "the instructions do not say: {said}");
    }
    let tools = fixture.client.list_all_tools().await.unwrap();
    let schema_of =
        |name: &str| serde_json::to_string(&tools.iter().find(|tool| tool.name == name).unwrap().input_schema).unwrap();
    assert!(schema_of("search_presets").contains("808 sub") && schema_of("search_presets").contains("wait_seconds"));
    assert!(schema_of("save_preset").contains("Glass Lead"));
    assert!(schema_of("get_device").contains("param_offset") && schema_of("update_device").contains("Dynamic 57"));
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

const TIM_HENSON: &str = "vst3:ABCDEF019182FAEB4E4453504E544858";

/// What an assistant does with an amplifier that lists no presets, on the
/// real Archetype Tim Henson: reads the few parameters that matter out of
/// its hundred and seventy, sets a microphone and a gain in the plugin's
/// own words, keeps the sound under a name and gives it to another
/// device; then looks for a pad across every instrument that is
/// installed. Prints the calls' replies.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Archetype Tim Henson X (VST3) installed; run with `cargo test -p trill-mcp --test presets an_assistant_dials -- --include-ignored --nocapture`"]
async fn an_assistant_dials_in_an_amplifier_keeps_its_sound_and_searches_every_instrument() {
    use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};
    // The plugin host's caches are the test's own, and no preset file of
    // the user's is read; what is saved goes to a temporary folder.
    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-sounds");
    let clap =
        ClapConfig { cache_path: Some(scratch.join("clap.json")), scanner: Scanner::Auto, ..ClapConfig::default() };
    let vst3 = Vst3Config {
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    let host = PluginHost::with_configs(clap, vst3).unwrap();
    if !host.vst3().descriptors().iter().any(|descriptor| descriptor.uri == TIM_HENSON) {
        eprintln!("skipping: Archetype Tim Henson X is not installed as a VST3 plugin");
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

    // The amplifier, as it comes: how many parameters, the ones its patch
    // moved, its channels, and how many presets its maker's files give it.
    let amp = ok(client, "add_device", json!({ "uri": TIM_HENSON })).await["device_id"].as_u64().unwrap();
    // Asked about once, so that whether it has presets is known.
    ok(client, "describe_device", json!({ "uri": TIM_HENSON, "preset_limit": 0 })).await;
    let got = ok(client, "get_device", json!({ "device": amp, "param_limit": 6 })).await;
    said("get_device (the amplifier, unasked)", &got);
    let (count, changed) = (got["param_count"].as_u64().unwrap(), got["changed"].as_u64().unwrap());
    assert!(count > 100 && changed > 0 && changed < count / 2, "{changed} of {count}");
    assert_eq!(got["channels"], "stereo in, stereo out");
    assert!(got["params"].as_array().unwrap().iter().all(|param| param["text"].is_string()), "{got}");
    let own = got["presets"]["total"].as_u64().unwrap();
    assert!(own > 100 && got["presets"].get("note").is_none(), "{got}");
    assert!(got.to_string().len() < 4000, "a small reply for {count} parameters: {} bytes", got.to_string().len());

    // Found by name, with what it can be set to.
    let mics = ok(client, "get_device", json!({ "device": amp, "query": "cherubs cab l mic type" })).await;
    said("get_device query \"cherubs cab l mic type\"", &mics["params"]);
    let choices = mics["params"][0]["choices"].as_array().unwrap();
    assert!(choices.contains(&json!("Dynamic 57")) && choices.len() >= 3, "{mics}");
    let gain = ok(client, "get_device", json!({ "device": amp, "query": "input gain" })).await;
    said("get_device query \"input gain\"", &gain["params"]);
    assert_eq!(
        (gain["params"][0]["from"].as_str(), gain["params"][0]["to"].as_str()),
        (Some("-24.0 dB"), Some("+24.0 dB"))
    );

    // Set in its own words; the reply says what each value came to.
    let params = json!({ "Cherubs Amp Cab L Mic Type": "dynamic 57", "Input Gain": "-6 dB", "Doubler Active": "On" });
    let updated = ok(client, "update_device", json!({ "device": amp, "params": params })).await;
    said(&format!("update_device {params}"), &updated);
    assert_eq!(updated["shown"]["Input Gain"], "-6.0 dB");
    assert_eq!(updated["shown"]["Cherubs Amp Cab L Mic Type"], "Dynamic 57");
    assert_eq!(updated["shown"]["Doubler Active"], "On");
    let error = call(client, "update_device", json!({ "device": amp, "params": { "Input Gain": "99 dB" } })).await;
    said("update_device Input Gain = 99 dB", &json!(error.clone().unwrap_err()));
    assert!(error.unwrap_err().contains("values like '-24.0 dB'"));

    // Kept under a name, it is the amplifier's preset from then on.
    let saved = ok(client, "save_preset", json!({ "device": amp, "name": "Dry Rhythm", "tags": ["rhythm"] })).await;
    said("save_preset", &saved);
    assert!(std::path::Path::new(saved["file"].as_str().unwrap()).starts_with(folder.path()));
    let described = ok(client, "describe_device", json!({ "uri": TIM_HENSON, "preset_limit": 3 })).await;
    said("describe_device (its presets)", &described["presets"]);
    // The saved one is first, in front of the maker's own.
    assert_eq!(described["presets"]["total"].as_u64(), Some(own + 1));
    assert_eq!(names(&described["presets"])[0], "Dry Rhythm");
    // Another amplifier starts from it: the plugin's state came with it.
    let second = ok(client, "add_device", json!({ "uri": TIM_HENSON, "preset": "Dry Rhythm" })).await;
    let second = second["device_id"].as_u64().unwrap();
    // A plugin takes a state on a moment after it is created.
    let mut shown = Value::Null;
    for _ in 0..50 {
        shown = ok(client, "get_device", json!({ "device": second, "query": "input gain" })).await;
        if shown["params"][0]["text"] == "-6.0 dB" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    said("get_device (a second amplifier, from the saved preset)", &shown["params"]);
    assert_eq!(shown["params"][0]["text"], "-6.0 dB");
    let mic = ok(client, "get_device", json!({ "device": second, "query": "cherubs cab l mic type" })).await;
    assert_eq!(mic["params"][0]["text"], "Dynamic 57");

    // A sound, across everything that is installed. The first search on a
    // machine reads every instrument's presets, which takes a while.
    let search = json!({ "query": "warm pad", "limit": 8, "wait_seconds": 60 });
    let mut found = ok(client, "search_presets", search.clone()).await;
    for _ in 0..6 {
        if found.get("still_reading").is_none() {
            break;
        }
        said("search_presets (still reading)", &json!({ "status": found["status"], "of": found["still_reading"] }));
        found = ok(client, "search_presets", search.clone()).await;
    }
    said(&format!("search_presets {search}"), &found);
    assert!(found.get("still_reading").is_none(), "everything was read in the end: {found}");
    let without: Vec<&str> = found["without_presets"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    eprintln!("\ninstruments that give a host no presets: {without:?}");
    let listed = ok(client, "list_devices", json!({ "category": "instrument" })).await;
    for device in listed["devices"].as_array().unwrap() {
        if let Some(count) = device.get("preset_count") {
            eprintln!(
                "{:>6} presets: {} ({})",
                count,
                device["name"].as_str().unwrap(),
                device["uri"].as_str().unwrap()
            );
        }
    }
    // The built-in synth has a warm pad, whatever else is installed, and
    // what was found is ready for create_track.
    assert!(found["matching"].as_u64().unwrap() >= 1, "{found}");
    let first = &found["items"][0];
    let preset = first.get("id").unwrap_or(&first["preset"]);
    let track =
        ok(client, "create_track", json!({ "name": "Pad", "instrument": first["device"], "preset": preset })).await;
    said("create_track (from the first match)", &track);
    assert_eq!(track["preset"], first["preset"]);
}

/// What an assistant does now that the plugins it reaches for have their
/// makers' presets: counts them, gives a drum sampler a kit, a sampled
/// guitar a tone and an amplifier a preset by name, takes the guitar out
/// of its automatic hammer-ons, changes a setting of it that has no
/// parameter, and probes what it has before it writes for it. Prints the
/// calls' replies.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Superior Drummer 3, Ample Metal Hellrazer, Archetype Tim Henson X and Serum 2; run with `cargo test -p trill-mcp --test presets an_assistant_gives -- --include-ignored --nocapture`"]
async fn an_assistant_gives_each_plugin_a_preset_of_its_makers_and_probes_it() {
    use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};
    const SUPERIOR: &str = "vst3:565354534433617375706572696F7220";
    const HELLRAZER: &str = "vst3:ABCDEF019182FAEB41707364416D6872";
    const SERUM_2: &str = "vst3:56534558667350736572756D20320000";
    // The plugin host's caches are the test's own. The vendors' folders
    // of this machine are read, and nothing is written to them.
    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-vendor");
    let clap =
        ClapConfig { cache_path: Some(scratch.join("clap.json")), scanner: Scanner::Auto, ..ClapConfig::default() };
    let vst3 = Vst3Config {
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    let host = PluginHost::with_configs(clap, vst3).unwrap();
    let installed: Vec<String> = host.vst3().descriptors().into_iter().map(|descriptor| descriptor.uri).collect();
    if let Some(missing) =
        [SUPERIOR, HELLRAZER, TIM_HENSON, SERUM_2].iter().find(|uri| !installed.contains(&uri.to_string()))
    {
        eprintln!("skipping: '{missing}' is not installed");
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

    // How many presets each has, where the makers' files were read.
    for (uri, at_least) in [(SUPERIOR, 100), (HELLRAZER, 20), (TIM_HENSON, 100), (SERUM_2, 300)] {
        let described = ok(client, "describe_device", json!({ "uri": uri, "preset_limit": 3 })).await;
        said(&format!("describe_device {uri}"), &described["presets"]);
        assert!(described["presets"]["total"].as_u64().unwrap() >= at_least, "{described}");
        assert!(described["presets"].get("note").is_none(), "{described}");
    }
    // One search finds a sound among all of them.
    let found = ok(client, "search_presets", json!({ "query": "metal", "limit": 8, "wait_seconds": 60 })).await;
    said("search_presets \"metal\"", &found);
    let devices: Vec<&str> =
        found["items"].as_array().unwrap().iter().filter_map(|item| item["device"].as_str()).collect();
    assert!(
        devices.contains(&SUPERIOR) && devices.contains(&HELLRAZER),
        "kits and guitar tones in one answer: {found}"
    );

    // A drum sampler started from a kit: the reply says that it reads its
    // samples, and a probe (which waits for that) hears the kick.
    let arguments = json!({ "name": "Drums", "instrument": SUPERIOR, "preset": "Black Metal" });
    let drums = ok(client, "create_track", arguments.clone()).await;
    said(&format!("create_track {arguments}"), &drums);
    assert_eq!(drums["preset"], "Black Metal");
    assert!(drums["note"].as_str().unwrap().contains("reads the kit's samples in the background"), "{drums}");
    let arguments = json!({ "track": drums["track_id"], "notes": [36, 38, 49], "detail": true });
    // The copy a probe plays reads the kit anew, and the probe waits for
    // every drum it is to play: asked once, right away.
    let probed = ok(client, "probe", arguments.clone()).await;
    said(&format!("probe {arguments}"), &probed);
    assert!(!probed.to_string().contains("\"silent\":true"), "kick, snare and a crash are all heard: {probed}");

    // A sampled guitar with one of its maker's tones, then out of the
    // automatic hammer-ons its presets come with.
    let arguments = json!({ "name": "Rhythm", "instrument": HELLRAZER, "preset": "9 String Metal" });
    let guitar = ok(client, "create_track", arguments.clone()).await;
    said(&format!("create_track {arguments}"), &guitar);
    assert_eq!(guitar["preset"], "9 String Metal");
    let device = guitar["instrument_device_id"].as_u64().unwrap();
    let legato = ok(client, "get_device", json!({ "device": device, "query": "auto legato" })).await;
    said("get_device query \"auto legato\"", &legato["params"]);
    assert_eq!(
        (legato["params"][0]["name"].as_str(), legato["params"][0]["text"].as_str()),
        (Some("Auto Legato"), Some("1"))
    );
    let arguments = json!({ "device": device, "params": { "Auto Legato": "0" } });
    let updated = ok(client, "update_device", arguments.clone()).await;
    said(&format!("update_device {arguments}"), &updated);
    assert_eq!(updated["shown"]["Auto Legato"], "0");
    // And a setting that is in its saved state alone: the preset turned
    // the sound of the frets off.
    let frets = ok(client, "get_device", json!({ "device": device, "query": "fret sound" })).await;
    let shown = json!({ "settings": frets["settings"], "settings_count": frets["settings_count"], "matching": frets["matching"] });
    said("get_device query \"fret sound\"", &shown);
    assert_eq!(frets["settings"], json!([{ "name": "M-Fret Sound", "value": "0" }]));
    assert!(frets["settings_count"].as_u64().unwrap() > 100 && frets["matching"] == 0, "{frets}");
    let arguments = json!({ "device": device, "settings": { "M-Fret Sound": 1 } });
    let updated = ok(client, "update_device", arguments.clone()).await;
    said(&format!("update_device {arguments}"), &updated);
    assert_eq!(updated["settings"], json!({ "M-Fret Sound": "1" }));
    let fret = |reply: Value| reply["settings"][0]["value"].as_str().map(str::to_string);
    assert_eq!(
        fret(ok(client, "get_device", json!({ "device": device, "query": "fret sound" })).await).as_deref(),
        Some("1")
    );
    ok(client, "history", json!({ "action": "undo" })).await;
    let undone = fret(ok(client, "get_device", json!({ "device": device, "query": "fret sound" })).await);
    assert_eq!(undone.as_deref(), Some("0"), "undone, the setting is the preset's again");
    let error =
        call(client, "update_device", json!({ "device": device, "settings": { "Fret Buzz": 1 } })).await.unwrap_err();
    said("update_device settings {\"Fret Buzz\": 1}", &json!(error));
    assert!(error.contains("has no setting 'Fret Buzz'; near it: ") && error.contains("M-Fret Sound"), "{error}");

    // An amplifier on the guitar's track, by a preset's name; and a synth
    // patch found by a search and probed before a track is made of it.
    let arguments = json!({ "uri": TIM_HENSON, "track": guitar["track_id"], "preset": "80s Arch Lead" });
    let amp = ok(client, "add_device", arguments.clone()).await;
    said(&format!("add_device {arguments}"), &amp);
    assert_eq!(amp["preset"], "80s Arch Lead");
    let found = ok(client, "search_presets", json!({ "query": "808", "device": "serum", "limit": 3 })).await;
    said("search_presets \"808\" in Serum", &found);
    let first = &found["items"][0];
    assert_eq!(first["device"], SERUM_2);
    let arguments = json!({ "device": SERUM_2, "preset": first["id"], "notes": ["C1", "C2"] });
    let probed = ok(client, "probe", arguments.clone()).await;
    said(&format!("probe {arguments}"), &probed);
    assert!(!probed.to_string().contains("\"silent\":true"), "{probed}");
}

// ---- a preset file, by its path ------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_device_is_given_a_preset_file_by_its_path() {
    let fixture = connect().await;
    let client = &fixture.client;
    let bought = fixture.file("samples/Pack/Bought Lead.vstpreset", "big-synth patch bought lead");
    let other = fixture.file("samples/Pack/Second Lead.vstpreset", "big-synth patch second lead");

    // A track starts from the file as it starts from a preset by name,
    // and the reply says what was loaded.
    let arguments = json!({ "name": "Lead", "instrument": SYNTH, "preset": bought });
    let track = ok(client, "create_track", arguments).await;
    assert_eq!(track["preset"], "Bought Lead");
    let device = track["instrument_device_id"].as_u64().unwrap();
    assert_eq!(fixture.synths.patch(device), "file:bought lead");
    assert_eq!(ok(client, "get_device", json!({ "device": device })).await["preset"], "Bought Lead");
    // So does a device that is added, and one that is there is switched
    // to a file as one undo step.
    let added = ok(client, "add_device", json!({ "uri": SYNTH, "track": track["track_id"], "preset": other })).await;
    assert_eq!(added["preset"], "Second Lead");
    let updated =
        ok(client, "update_device", json!({ "device": device, "preset": other, "params": { "tone": 0.8 } })).await;
    assert_eq!((updated["preset"].as_str(), updated["set"]["tone"].as_f64()), (Some("Second Lead"), Some(0.8)));
    assert_eq!(fixture.synths.patch(device), "file:second lead");
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(fixture.synths.patch(device), "file:bought lead");
    assert_eq!(ok(client, "get_device", json!({ "device": device })).await["preset"], "Bought Lead");

    // A file that is no preset of the device, and a path that leads to
    // none: a sentence that names the file, and nothing is added.
    let wav = fixture.file("samples/Pack/Kick.wav", "RIFF");
    let gone = fixture.folder.path().join("samples").join("Gone.vstpreset").to_string_lossy().into_owned();
    let tracks = ok(client, "get_project", json!({})).await["tracks"].as_array().unwrap().len();
    for (path, reason) in [(&wav, "it is not a preset of Big Synth"), (&gone, "there is no such file")] {
        let said = format!("'{path}' cannot be given to '{SYNTH}' as a preset: {reason}");
        let error = call(client, "create_track", json!({ "name": "x", "instrument": SYNTH, "preset": path })).await;
        let error = error.unwrap_err();
        assert!(error.starts_with(&said) && error.contains("No track was created"), "{error}");
        let error = call(client, "add_device", json!({ "uri": SYNTH, "preset": path })).await.unwrap_err();
        assert!(error.starts_with(&said), "{error}");
        let error = call(client, "update_device", json!({ "device": device, "preset": path })).await.unwrap_err();
        assert!(error.starts_with(&said), "{error}");
        // A probe starts an instrument from a preset the same way.
        let arguments = json!({ "device": SYNTH, "preset": path, "notes": ["C3"] });
        let error = call(client, "probe", arguments).await.unwrap_err();
        assert!(error.starts_with(&said), "{error}");
    }
    assert_eq!(ok(client, "get_project", json!({})).await["tracks"].as_array().unwrap().len(), tracks);
    assert_eq!(fixture.synths.patch(device), "file:bought lead");
    // A built-in device's presets are not files.
    let error = call(client, "add_device", json!({ "uri": POLYSYNTH, "preset": bought })).await.unwrap_err();
    assert!(
        error.contains("as a preset: Poly Synth takes no preset files; its presets are asked for by name"),
        "{error}"
    );

    // The schemas say that a preset may be a file.
    let tools = client.list_all_tools().await.unwrap();
    for tool in ["create_track", "add_device", "update_device"] {
        let schema = tools.iter().find(|listed| listed.name == tool).unwrap();
        let schema = serde_json::to_string(&schema.input_schema).unwrap();
        assert!(schema.contains("whole path"), "{tool}: {schema}");
    }
    let instructions = client.peer_info().unwrap().instructions.clone().unwrap_or_default();
    assert!(instructions.contains("`preset` also takes the whole path of a preset file"), "{instructions}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_preset_file_in_the_sample_library_is_found_by_its_name() {
    let fixture = connect().await;
    let client = &fixture.client;
    // A pack the user bought, in a folder of their samples: a preset of
    // the synth, a preset of something that is not installed, and sounds.
    let saw = fixture.file("samples/BASSPOP MK1/Presets/OMEGA SAW.vstpreset", "big-synth patch omega saw");
    fixture.file("samples/BASSPOP MK1/Presets/OMEGA KEYS.vstpreset", "a preset of some other synth");
    fixture.file("samples/BASSPOP MK1/Presets/Warm Pad.vstpreset", "big-synth patch a bought warm pad");
    fixture.file("samples/BASSPOP MK1/One Shots/Omega Saw Hit.txt", "not a preset");
    let samples = fixture.folder.path().join("samples").to_string_lossy().into_owned();

    // Nothing of the library's is found before the folder is one of its.
    let before = ok(client, "search_presets", json!({ "query": "omega saw" })).await;
    let files =
        |found: &Value| found["items"].as_array().unwrap().iter().filter(|item| item.get("file").is_some()).count();
    assert_eq!(files(&before), 0, "{before}");
    let added = ok(client, "library", json!({ "action": "add_folder", "path": samples })).await;
    assert_eq!(added["scanning"], false, "{added}");
    // As a sound of the library it is found by search_sounds, as a preset.
    let sounds = ok(client, "search_sounds", json!({ "text": "omega saw", "kind": "preset" })).await;
    assert_eq!(sounds["total"], 1, "{sounds}");

    // Found by its name, with the device it is for, under the folders it
    // lies in; and what to give `preset` is its path.
    let found = ok(client, "search_presets", json!({ "query": "omega saw" })).await;
    assert_eq!(found["matching"], 1, "{found}");
    let item = &found["items"][0];
    assert_eq!((item["device"].as_str(), item["preset"].as_str()), (Some(SYNTH), Some("OMEGA SAW")));
    assert_eq!(item["category"], "Library/BASSPOP MK1/Presets");
    assert_eq!(Path::new(item["file"].as_str().unwrap()), Path::new(&saw), "the same file, as the library has it");
    assert!(item.get("id").is_none(), "{item}");
    assert!(found["files"].as_str().unwrap().contains("give `file` as `preset`"), "{found}");
    // The devices' own presets are counted as before.
    assert_eq!(found["searched"], before["searched"], "{found}");
    // By the pack it came in, too; the other synth's preset is nobody's.
    let pack = ok(client, "search_presets", json!({ "query": "basspop", "device": "big" })).await;
    let names: Vec<&str> =
        pack["items"].as_array().unwrap().iter().filter_map(|item| item["preset"].as_str()).collect();
    assert_eq!(names, ["OMEGA SAW", "Warm Pad"], "{pack}");
    // Beside the presets the synth lists, where a name is in both.
    let warm = ok(client, "search_presets", json!({ "query": "warm pad", "device": "big" })).await;
    assert_eq!((warm["matching"].as_u64(), files(&warm)), (Some(3), 1), "{warm}");
    // A search that is kept to effects, or to another device, leaves it out.
    for arguments in [json!({ "query": "omega", "category": "effect" }), json!({ "query": "omega", "device": "poly" })]
    {
        assert_eq!(ok(client, "search_presets", arguments).await["matching"], 0);
    }

    // And the path goes where a preset goes.
    let track =
        ok(client, "create_track", json!({ "name": "Saws", "instrument": SYNTH, "preset": item["file"] })).await;
    assert_eq!(track["preset"], "OMEGA SAW");
    assert_eq!(fixture.synths.patch(track["instrument_device_id"].as_u64().unwrap()), "file:omega saw");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_assistant_is_given_preset_files_of_this_machine_and_no_other() {
    let fixture = connect().await;
    let assistant = &fixture.assistant().await;
    // A preset the user names to it, wherever it lies on a disk: outside
    // the folders it may write into.
    let bought = fixture.file("downloads/Bought Lead.vstpreset", "big-synth patch bought lead");
    let track = ok(assistant, "create_track", json!({ "name": "Lead", "instrument": SYNTH, "preset": bought })).await;
    assert_eq!(track["preset"], "Bought Lead");
    let device = track["instrument_device_id"].as_u64().unwrap();
    assert_eq!(fixture.synths.patch(device), "file:bought lead");

    // A file on another machine is not opened: the path alone is refused,
    // before anything looks at it.
    let tracks = ok(assistant, "get_project", json!({})).await["tracks"].as_array().unwrap().len();
    for share in [r"\\files.example\presets\Lead.vstpreset", "//files.example/presets/Lead.vstpreset"] {
        let refused = format!("'{share}' is not on one of this machine's own disks");
        let arguments = json!({ "name": "x", "instrument": SYNTH, "preset": share });
        let error = call(assistant, "create_track", arguments).await.unwrap_err();
        assert!(error.starts_with(&refused) && error.ends_with("No track was created."), "{error}");
        let error = call(assistant, "add_device", json!({ "uri": SYNTH, "preset": share })).await.unwrap_err();
        assert!(error.starts_with(&refused) && error.ends_with("The device was not added."), "{error}");
        let error = call(assistant, "update_device", json!({ "device": device, "preset": share })).await.unwrap_err();
        assert!(error.starts_with(&refused) && error.ends_with("The device is as it was."), "{error}");
        let error = call(assistant, "probe", json!({ "device": SYNTH, "preset": share })).await.unwrap_err();
        assert!(error.starts_with(&refused), "{error}");
    }
    assert_eq!(ok(assistant, "get_project", json!({})).await["tracks"].as_array().unwrap().len(), tracks);
    assert_eq!(fixture.synths.patch(device), "file:bought lead");
    // A name with slashes in it is a name still.
    let error = call(assistant, "update_device", json!({ "device": device, "preset": "Pads/No Such" })).await;
    assert!(error.unwrap_err().contains("has no preset 'Pads/No Such'"));
}
