//! A device's parameters over MCP as a reader takes them in: a plugin's in
//! the plugin's own words, a device with a great many of them a few at a
//! time, and values set by the text they are shown as. The plugin is a
//! stand-in that exists only here, so the tests need nothing installed;
//! `trill-plugins` tests where the texts of a real plugin come from.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::sync::Arc;
use trill_core::Device;
use trill_core::device::{
    ChannelLayout, DeviceCategory, DeviceController, DeviceDescriptor, DeviceError, DeviceFactory, DeviceInstance,
    DeviceProcessor, Event, ParamInfo, ParamKind, ProcessInfo,
};
use trill_engine::AudioBackend;
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Parts, Runtime, RuntimeOptions, Storage};

type Client = RunningService<RoleClient, ClientConfig>;

/// A guitar amplifier as a plugin: one input channel, a hundred and twenty
/// parameters with numbers for ids, and three of them not where the
/// plugin's own defaults are.
const AMP: &str = "vst3:A0000000000000000000000000000001";
const REVERB: &str = "builtin:reverb";
const POLYSYNTH: &str = "builtin:polysynth";

const PARAMS: usize = 120;
const MICS: [&str; 4] = ["Dynamic 57", "Ribbon 121", "Condenser 414", "Custom IR"];
const AMPS: [&str; 3] = ["Clean", "Crunch", "Lead"];

const INPUT_GAIN: usize = 0;
const AMP_TYPE: usize = 1;
const MIC_L: usize = 2;
const MIC_R: usize = 3;
const DOUBLER: usize = 4;

struct Amps;

fn infos() -> Vec<ParamInfo> {
    let param = |index: usize, name: &str, max: f64, start: f64, kind: ParamKind| ParamInfo {
        id: (100 + index).to_string(),
        name: name.into(),
        min: 0.0,
        max,
        // As a plugin host lists it: the value the plugin started with.
        default: start,
        unit: String::new(),
        kind,
    };
    let mut infos = vec![
        param(INPUT_GAIN, "Input Gain", 1.0, 0.375, ParamKind::Float),
        param(AMP_TYPE, "Amp Type", 2.0, 0.0, ParamKind::Enum(AMPS.map(str::to_string).to_vec())),
        param(MIC_L, "Cab L Mic Type", 3.0, 2.0, ParamKind::Int),
        param(MIC_R, "Cab R Mic Type", 3.0, 0.0, ParamKind::Int),
        param(DOUBLER, "Doubler Active", 1.0, 0.0, ParamKind::Bool),
    ];
    for index in infos.len()..PARAMS {
        let start = if index == 7 { 0.8 } else { 0.5 };
        infos.push(param(index, &format!("Knob {index}"), 1.0, start, ParamKind::Float));
    }
    infos
}

/// What the plugin shows for a value of the parameter at `index`.
fn shown(index: usize, value: f64) -> String {
    match index {
        INPUT_GAIN => format!("{:+.1} dB", value * 48.0 - 24.0),
        AMP_TYPE => AMPS[(value.round() as usize).min(2)].to_string(),
        MIC_L | MIC_R => MICS[(value.round() as usize).min(3)].to_string(),
        DOUBLER => if value >= 0.5 { "On" } else { "Off" }.to_string(),
        _ => format!("{:.0} %", value * 100.0),
    }
}

struct Silent;

impl DeviceProcessor for Silent {
    fn prepare(&mut self, _sample_rate: f64, _max_block: usize) {}
    fn set_param(&mut self, _index: usize, _value: f64) {}
    fn process(&mut self, _: &ProcessInfo, _: &[Event], _: &mut [f32], _: &mut [f32]) {}
    fn reset(&mut self) {}
}

struct Controller;

impl DeviceController for Controller {
    fn save_state(&mut self) -> Option<Vec<u8>> {
        None
    }

    fn channels(&self) -> Option<ChannelLayout> {
        Some(ChannelLayout { inputs: 1, outputs: 2 })
    }

    fn param_texts(&mut self, values: &[(usize, f64)]) -> Vec<Option<String>> {
        values.iter().map(|&(index, value)| (index < PARAMS).then(|| shown(index, value))).collect()
    }

    /// The step that is shown as the text, or the gain that many dB are.
    fn param_value(&mut self, index: usize, text: &str) -> Option<f64> {
        let wanted = text.trim().to_lowercase();
        match index {
            INPUT_GAIN => {
                let db: f64 = wanted.trim_end_matches("db").trim().parse().ok()?;
                (-24.0..=24.0).contains(&db).then(|| (db + 24.0) / 48.0)
            }
            AMP_TYPE | MIC_L | MIC_R | DOUBLER => {
                (0..4).map(f64::from).find(|step| shown(index, *step).to_lowercase() == wanted)
            }
            _ => None,
        }
    }

    /// The plugin's own defaults, which three parameters are not at.
    fn param_defaults(&self) -> Option<Vec<f64>> {
        let mut defaults: Vec<f64> = infos().iter().map(|info| info.default).collect();
        (defaults[INPUT_GAIN], defaults[MIC_L], defaults[7]) = (0.5, 0.0, 0.5);
        Some(defaults)
    }
}

impl DeviceFactory for Amps {
    fn descriptors(&self) -> Vec<DeviceDescriptor> {
        vec![DeviceDescriptor {
            uri: AMP.into(),
            name: "Amp".into(),
            vendor: "Test".into(),
            category: DeviceCategory::Effect,
            description: "A guitar amplifier".into(),
            params: Vec::new(),
            presets: Vec::new(),
        }]
    }

    fn create(&self, device: &Device, _sample_rate: f64, _max_block: usize) -> Result<DeviceInstance, DeviceError> {
        if device.uri != AMP {
            return Err(DeviceError::Unknown(device.uri.clone()));
        }
        Ok(DeviceInstance { processor: Box::new(Silent), params: infos(), controller: Some(Box::new(Controller)) })
    }
}

struct Fixture {
    client: Client,
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
    let parts = Parts { devices: vec![Box::new(Amps)], ..Parts::default() };
    let runtime = Arc::new(Runtime::start_custom(options, parts).unwrap());
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { client, _server: server, _folder: folder }
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

async fn amp(client: &Client) -> u64 {
    ok(client, "add_device", json!({ "uri": AMP })).await["device_id"].as_u64().unwrap()
}

/// The `name: text` of each parameter in a reply.
fn texts(reply: &Value) -> Vec<String> {
    let params = reply["params"].as_array().unwrap_or_else(|| panic!("no params in {reply}"));
    params
        .iter()
        .map(|param| format!("{}: {}", param["name"].as_str().unwrap(), param["text"].as_str().unwrap()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_with_many_parameters_is_shown_by_those_that_were_moved() {
    let fixture = connect().await;
    let client = &fixture.client;
    let device = amp(client).await;

    // Unasked: how many there are, and the few that are not where the
    // plugin's own defaults are, each as the plugin shows it.
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!((got["param_count"].as_u64(), got["changed"].as_u64()), (Some(120), Some(3)), "{got}");
    assert_eq!(texts(&got), ["Input Gain: -6.0 dB", "Cab L Mic Type: Condenser 414", "Knob 7: 80 %"]);
    assert_eq!(got["params"][0], json!({ "id": "100", "name": "Input Gain", "value": 0.375, "text": "-6.0 dB" }));
    assert_eq!(got["params"][1]["max"], 3.0, "a value that is not 0 to 1 says how far it goes");
    let note = got["params_note"].as_str().unwrap();
    assert!(note.contains("Of this device's 120 parameters, these 3 are not at its own defaults"), "{note}");
    assert!(note.contains("`query`") && note.contains("update_device sets a parameter by either"), "{note}");
    assert!(got.get("params_more").is_none() && got.get("matching").is_none(), "{got}");

    // All of them, a page at a time.
    let all = ok(client, "get_device", json!({ "device": device, "show": "all" })).await;
    assert_eq!((all["param_count"].as_u64(), all["params"].as_array().unwrap().len()), (Some(120), 40));
    assert_eq!(all["params_more"], "80 more: param_offset 40 lists the next");
    assert!(all.get("changed").is_none(), "{all}");
    let rest =
        ok(client, "get_device", json!({ "device": device, "show": "all", "param_offset": 40, "param_limit": 500 }))
            .await;
    assert_eq!((rest["param_offset"].as_u64(), rest["params"].as_array().unwrap().len()), (Some(40), 80));
    assert_eq!(rest["params"][79]["name"], "Knob 119");
    assert!(rest.get("params_more").is_none(), "{rest}");

    // None of them: the count is all there is to say.
    let none = ok(client, "get_device", json!({ "device": device, "show": "none" })).await;
    assert_eq!((none["param_count"].as_u64(), none["params"].as_array().unwrap().len()), (Some(120), 0));
    assert!(none.get("params_note").is_none(), "{none}");
    let error = call(client, "get_device", json!({ "device": device, "show": "some" })).await.unwrap_err();
    assert!(error.contains("show 'some' should be \"changed\", \"all\" or \"none\""), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parameter_is_found_by_words_in_its_name_with_what_it_can_be_set_to() {
    let fixture = connect().await;
    let client = &fixture.client;
    let device = amp(client).await;

    // A few: each with every choice it has, or with the ends of its range,
    // and with its own default where it is not at it.
    let mics = ok(client, "get_device", json!({ "device": device, "query": "mic type" })).await;
    assert_eq!((mics["param_count"].as_u64(), mics["matching"].as_u64()), (Some(120), Some(2)), "{mics}");
    assert_eq!(texts(&mics), ["Cab L Mic Type: Condenser 414", "Cab R Mic Type: Dynamic 57"]);
    assert_eq!(mics["params"][0]["choices"], json!(MICS));
    assert_eq!(mics["params"][0]["default"], "Dynamic 57");
    assert!(mics["params"][1].get("default").is_none(), "at its default: {mics}");

    let gain = ok(client, "get_device", json!({ "device": device, "query": "INPUT gain" })).await;
    let found = &gain["params"][0];
    assert_eq!((found["text"].as_str(), found["value"].as_f64()), (Some("-6.0 dB"), Some(0.375)));
    assert_eq!((found["from"].as_str(), found["to"].as_str()), (Some("-24.0 dB"), Some("+24.0 dB")));
    assert_eq!(found["default"], "+0.0 dB");
    let switch = ok(client, "get_device", json!({ "device": device, "query": "doubler" })).await;
    assert_eq!(switch["params"][0]["choices"], json!(["Off", "On"]));
    let amps = ok(client, "get_device", json!({ "device": device, "query": "amp type" })).await;
    assert_eq!(amps["params"][0]["choices"], json!(AMPS));
    // By its id as well, along with whatever has that in its name.
    let by_id = ok(client, "get_device", json!({ "device": device, "query": "104" })).await;
    assert_eq!(texts(&by_id), ["Doubler Active: Off", "Knob 104: 50 %"]);

    // Many: a page of them as they are, without the detail.
    let knobs = ok(client, "get_device", json!({ "device": device, "query": "knob", "param_limit": 5 })).await;
    assert_eq!((knobs["matching"].as_u64(), knobs["params"].as_array().unwrap().len()), (Some(115), 5));
    assert_eq!(knobs["params_more"], "110 more: param_offset 5 lists the next");
    let ones = ok(client, "get_device", json!({ "device": device, "query": "knob 1" })).await;
    assert_eq!(ones["matching"], 38, "every knob with a 1 in its number");
    assert!(ones["params"][0].get("from").is_none(), "{ones}");
    let nothing = ok(client, "get_device", json!({ "device": device, "query": "presence" })).await;
    assert_eq!((nothing["matching"].as_u64(), nothing["params"].as_array().unwrap().len()), (Some(0), 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_parameter_is_set_in_the_plugins_own_words() {
    let fixture = connect().await;
    let client = &fixture.client;
    let device = amp(client).await;

    // By its name or by words only its name has; by the text the plugin
    // shows, by a choice's name, or by number.
    let params = json!({
        "Cab L Mic Type": "ribbon 121",
        "input gain": "-12 dB",
        "doubler": "On",
        "Amp Type": "Lead",
        "105": 0.25,
    });
    let updated = ok(client, "update_device", json!({ "device": device, "params": params })).await;
    assert_eq!(updated["set"], json!({ "100": 0.25, "101": 2.0, "102": 1.0, "104": 1.0, "105": 0.25 }), "{updated}");
    assert_eq!(
        updated["shown"],
        json!({
            "Input Gain": "-12.0 dB",
            "Amp Type": "Lead",
            "Cab L Mic Type": "Ribbon 121",
            "Doubler Active": "On",
            "Knob 5": "25 %",
        })
    );
    let got = ok(client, "get_device", json!({ "device": device })).await;
    assert_eq!(got["changed"], 6);
    assert_eq!(
        texts(&got),
        [
            "Input Gain: -12.0 dB",
            "Amp Type: Lead",
            "Cab L Mic Type: Ribbon 121",
            "Doubler Active: On",
            "Knob 5: 25 %",
            "Knob 7: 80 %"
        ]
    );
    // One step: undone, all of it is as it was.
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(ok(client, "get_device", json!({ "device": device })).await["changed"], 3);

    // A text the plugin shows no value as: what it does show.
    let set = |params: Value| call(client, "update_device", json!({ "device": device, "params": params }));
    let error = set(json!({ "Cab R Mic Type": "SM7" })).await.unwrap_err();
    assert!(error.contains("'Cab R Mic Type' has no value that the plugin shows as 'SM7'"), "{error}");
    assert!(error.contains("one of 'Dynamic 57', 'Ribbon 121', 'Condenser 414', 'Custom IR'"), "{error}");
    assert!(error.contains("or a number from 0 to 3"), "{error}");
    let error = set(json!({ "Input Gain": "99 dB" })).await.unwrap_err();
    assert!(error.contains("values like '-24.0 dB', '-12.0 dB', '+0.0 dB', '+12.0 dB', '+24.0 dB'"), "{error}");
    // Words that two names have, and words that none has.
    let error = set(json!({ "mic type": "Dynamic 57" })).await.unwrap_err();
    assert!(error.contains("2 parameters are called something like 'mic type'"), "{error}");
    assert!(error.contains("Cab L Mic Type (102), Cab R Mic Type (103)"), "{error}");
    let error = set(json!({ "presence": 1 })).await.unwrap_err();
    assert!(error.contains("no parameter 'presence' among this device's 120"), "{error}");
    assert!(error.contains("get_device with `query`"), "{error}");
    let error = set(json!({ "Input Gain": 3 })).await.unwrap_err();
    assert!(error.contains("Input Gain") || error.contains("100"), "{error}");
    // None of it changed anything.
    assert_eq!(ok(client, "get_device", json!({ "device": device })).await["changed"], 3);

    // A device is added with values in the plugin's words.
    let added = ok(client, "add_device", json!({ "uri": AMP, "params": { "Cab R Mic Type": "Custom IR" } })).await;
    let second = added["device_id"].as_u64().unwrap();
    let mic = ok(client, "get_device", json!({ "device": second, "query": "cab r" })).await;
    assert_eq!(texts(&mic), ["Cab R Mic Type: Custom IR"]);
    let error = call(client, "add_device", json!({ "uri": AMP, "params": { "Amp Type": "Fuzz" } })).await.unwrap_err();
    assert!(
        error.contains("one of 'Clean', 'Crunch', 'Lead'") && error.contains("The device was not added"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_says_what_channels_it_takes_and_gives() {
    let fixture = connect().await;
    let client = &fixture.client;
    let track = ok(client, "create_track", json!({ "name": "Guitar", "instrument": POLYSYNTH })).await;
    let (track_id, synth) = (track["track_id"].as_u64().unwrap(), track["instrument_device_id"].as_u64().unwrap());
    let reverb = ok(client, "add_device", json!({ "uri": REVERB, "track": track_id })).await["device_id"].clone();
    let amp = ok(client, "add_device", json!({ "uri": AMP, "track": track_id })).await["device_id"].clone();

    let got = ok(client, "get_device", json!({ "device": synth, "show": "none" })).await;
    assert_eq!(got["channels"], "no audio in, stereo out");
    let got = ok(client, "get_device", json!({ "device": reverb })).await;
    assert_eq!(got["channels"], "stereo in, stereo out");
    assert!(got.get("channels_note").is_none(), "{got}");
    // A built-in device reads as it always has: every parameter, in its
    // own units, with its range.
    let params = got["params"].as_array().unwrap();
    assert_eq!(got["param_count"].as_u64(), Some(params.len() as u64));
    assert!(params.iter().all(|param| param.get("text").is_none() && param.get("default").is_some()), "{got}");
    assert!(got.get("changed").is_none() && got.get("params_note").is_none(), "{got}");
    let changed = ok(client, "get_device", json!({ "device": reverb, "show": "changed" })).await;
    assert_eq!((changed["changed"].as_u64(), changed["params"].as_array().unwrap().len()), (Some(0), 0));

    // The amplifier takes one channel: said on the device, and marked in
    // the chain where the song is looked over.
    let got = ok(client, "get_device", json!({ "device": amp, "show": "none" })).await;
    assert_eq!(got["channels"], "mono in, stereo out");
    assert!(got["channels_note"].as_str().unwrap().contains("mixed into this device's one input channel"), "{got}");
    let project = ok(client, "get_project", json!({})).await;
    let chain = project["tracks"][0]["devices"].as_array().unwrap();
    let marks: Vec<Option<&str>> = chain.iter().map(|device| device["channels"].as_str()).collect();
    assert_eq!(marks, [None, None, Some("mono in, stereo out")], "{project}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_that_hands_over_no_state_is_saved_by_the_values_that_were_set() {
    let fixture = connect().await;
    let client = &fixture.client;
    let device = amp(client).await;
    // The stand-in keeps no state a host could ask for: untouched, there
    // is nothing of it to keep.
    let error = call(client, "save_preset", json!({ "device": device, "name": "Nothing" })).await.unwrap_err();
    assert!(error.contains("gives a host no state to keep") && error.contains("no sound to save"), "{error}");

    // What was set from here is kept, and comes back on another device.
    let params = json!({ "Amp Type": "Lead", "Input Gain": "+6 dB" });
    ok(client, "update_device", json!({ "device": device, "params": params })).await;
    let saved = ok(client, "save_preset", json!({ "device": device, "name": "Lead Tone" })).await;
    assert_eq!(saved["saved"], "Saved/Lead Tone");
    let other = ok(client, "add_device", json!({ "uri": AMP, "preset": "Lead Tone" })).await;
    let got = ok(client, "get_device", json!({ "device": other["device_id"], "query": "amp type" })).await;
    assert_eq!((got["preset"].as_str(), texts(&got)), (Some("Lead Tone"), vec!["Amp Type: Lead".to_string()]));
    assert_eq!(got["presets"]["total"], 1, "the saved one is the only preset it has: {got}");
}
