//! A tempo and a time signature that change inside a song, as an AI client
//! meets them: set at a bar and read back, written into by the tools that
//! think in bars, rendered and measured, and taken through a MIDI file.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::sync::Arc;
use trill_core::{TempoChange, TempoMap};
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Runtime, RuntimeOptions};

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    client: Client,
    _server: HttpServer,
    /// Where a test puts the files it has made.
    folder: tempfile::TempDir,
}

async fn connect() -> Fixture {
    // Nothing of the user's is read or written: no settings, no library.
    let runtime = Arc::new(Runtime::start(RuntimeOptions::hermetic()).unwrap());
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { client, _server: server, folder: tempfile::tempdir().unwrap() }
}

/// Calls a tool. `Ok` holds the parsed JSON reply; `Err` holds the message
/// of a tool-level error.
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

/// Adds a track with an instrument and returns its id.
async fn track(client: &Client, name: &str, instrument: &str, preset: &str) -> u64 {
    let arguments = json!({ "name": name, "instrument": instrument, "preset": preset });
    ok(client, "create_track", arguments).await["track_id"].as_u64().unwrap()
}

/// A list of a reply's entries, each as the fields named.
fn rows(value: &Value, fields: &[&str]) -> Vec<Vec<Value>> {
    let entries = value.as_array().unwrap_or_else(|| panic!("not a list: {value}"));
    entries.iter().map(|entry| fields.iter().map(|field| entry[*field].clone()).collect()).collect()
}

/// The keys the built-in kit has its kick and its snare on.
const KICK: u64 = 36;
const SNARE: u64 = 38;

/// Where the notes of a clip start in the song, each once, in order; with
/// `pitch`, only the notes on that key.
async fn starts(client: &Client, clip: &Value, pitch: Option<u64>) -> Vec<f64> {
    let reply = ok(client, "get_notes", json!({ "clip": clip })).await;
    let at = reply["start"].as_f64().unwrap();
    let mut starts: Vec<f64> = reply["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|note| pitch.is_none_or(|pitch| note["pitch"] == pitch))
        .map(|note| at + note["start"].as_f64().unwrap())
        .collect();
    starts.sort_by(f64::total_cmp);
    starts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    starts
}

/// The four-four song that goes into two bars of seven-eight at 140 and
/// comes back: bars begin at beats 0, 4, 8, 11.5, 15 and 19.
async fn seven_eight_in_the_middle(client: &Client) -> Value {
    ok(client, "set_project", json!({ "tempo": 120, "time_signature": "4/4" })).await;
    ok(client, "set_time_signature", json!({ "bar": 3, "time_signature": "7/8", "bars": 2 })).await;
    ok(client, "set_tempo", json!({ "bar": 3, "bpm": 140 })).await;
    ok(client, "set_tempo", json!({ "bar": 5, "bpm": 120 })).await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tempo_and_the_time_signature_are_set_at_a_bar_and_read_back() {
    let fixture = connect().await;
    let client = &fixture.client;
    let keys = track(client, "Keys", "builtin:polysynth", "soft_keys").await;
    ok(client, "create_clip", json!({ "track": keys, "start": 8, "length": 16, "notes": [["C4", 0, 1]] })).await;
    ok(client, "set_markers", json!({ "markers": [{ "beat": 15, "name": "Back" }] })).await;

    // Two bars of 7/8 from bar 3, and the song is back in 4/4 after them.
    let set = ok(client, "set_time_signature", json!({ "bar": 3, "time_signature": "7/8", "bars": 2 })).await;
    assert_eq!((set["at_beat"].as_f64(), set["until_beat"].as_f64()), (Some(8.0), Some(15.0)), "{set}");
    assert_eq!(
        rows(&set["time_signature"], &["bar", "beat", "time_signature", "beats_per_bar"]),
        [
            vec![json!(1), json!(0.0), json!("4/4"), json!(4.0)],
            vec![json!(3), json!(8.0), json!("7/8"), json!(3.5)],
            vec![json!(5), json!(15.0), json!("4/4"), json!(4.0)],
        ]
    );
    // 140 from bar 3, and from bar 5 a glide that is back at 120 by bar 6.
    ok(client, "set_tempo", json!({ "bar": 3, "bpm": 140 })).await;
    ok(client, "set_tempo", json!({ "bar": 5, "bpm": 140 })).await;
    let map = ok(client, "set_tempo", json!({ "bar": 6, "bpm": 120, "ramp": true })).await;
    assert_eq!(
        rows(&map["tempo"], &["bar", "beat", "bpm", "ramp"]),
        [
            vec![json!(1), json!(0.0), json!(120.0), Value::Null],
            vec![json!(3), json!(8.0), json!(140.0), Value::Null],
            vec![json!(5), json!(15.0), json!(140.0), Value::Null],
            vec![json!(6), json!(19.0), json!(120.0), json!(true)],
        ]
    );
    // Eight beats at 120 are four seconds; seven at 140 are three more.
    let seconds: Vec<f64> = map["tempo"].as_array().unwrap().iter().map(|at| at["seconds"].as_f64().unwrap()).collect();
    assert_eq!(seconds[..3], [0.0, 4.0, 7.0]);
    let glide = 60.0 / -5.0 * (120.0f64 / 140.0).ln();
    assert!((seconds[3] - (7.0 + glide)).abs() < 1e-3, "the glide takes {glide} s: {seconds:?}");

    // Nothing moved, and everything that counts bars counts them through the changes.
    let project = ok(client, "get_project", json!({})).await;
    let clip = &project["tracks"][0]["clips"][0];
    assert_eq!((clip["start"].as_f64(), clip["start_bar"].as_f64()), (Some(8.0), Some(3.0)), "{clip}");
    assert_eq!(project["markers"][0]["bar"], 5.0, "{project}");
    assert_eq!((project["tempo"].as_f64(), project["time_signature"].as_str()), (Some(120.0), Some("4/4")));
    assert_eq!(project["tempo_changes"], map["tempo"]);
    assert_eq!(project["time_signature_changes"], map["time_signature"]);
    assert_eq!(project["length_bars"], 6.25, "the clip ends a beat into bar 7: {project}");
    let read = ok(client, "get_tempo_map", json!({ "bars": [1, 4, 5, 9] })).await;
    assert_eq!(
        rows(&read["bars"], &["bar", "beat", "beats", "time_signature", "bpm"]),
        [
            vec![json!(1), json!(0.0), json!(4.0), json!("4/4"), json!(120.0)],
            vec![json!(4), json!(11.5), json!(3.5), json!("7/8"), json!(140.0)],
            vec![json!(5), json!(15.0), json!(4.0), json!("4/4"), json!(140.0)],
            vec![json!(9), json!(31.0), json!(4.0), json!("4/4"), json!(120.0)],
        ]
    );
    assert_eq!(read["length_beats"], 24.0);
    ok(client, "transport", json!({ "action": "seek", "position": 12.5 })).await;
    // The audio thread takes the playhead there in its own time.
    let mut transport = Value::Null;
    for _ in 0..200 {
        transport = ok(client, "get_project", json!({})).await["transport"].clone();
        if transport["position_beats"] == 12.5 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(transport["position_bar_beat"], "4.2", "{transport}");
    assert_eq!(
        (transport["tempo_here"].as_f64(), transport["time_signature_here"].as_str()),
        (Some(140.0), Some("7/8"))
    );

    // A beat inside a bar, a tempo that takes over where a glide arrives, and what is removed.
    let pushed = ok(client, "set_tempo", json!({ "bar": 2, "beat": 3, "bpm": 100 })).await;
    assert_eq!((pushed["at_beat"].as_f64(), pushed["tempo"][1]["beat_in_bar"].as_f64()), (Some(6.0), Some(3.0)));
    let both = ok(client, "set_tempo", json!({ "bar": 6, "bpm": 90 })).await;
    let at_six: Vec<Vec<Value>> =
        rows(&both["tempo"], &["bar", "bpm", "ramp"]).into_iter().filter(|row| row[0] == 6).collect();
    assert_eq!(at_six, [vec![json!(6), json!(120.0), json!(true)], vec![json!(6), json!(90.0), Value::Null]]);
    let removed = ok(client, "set_tempo", json!({ "bar": 6, "remove": true })).await;
    assert_eq!(removed["tempo"].as_array().unwrap().len(), 4, "the glide and the tempo after it both go: {removed}");
    let error = call(client, "set_tempo", json!({ "bar": 7, "remove": true })).await.unwrap_err();
    assert!(
        error.contains("it changes at bar 2 beat 3.0, bar 3, bar 5") || error.contains("bar 2 beat 3, bar 3, bar 5"),
        "{error}"
    );
    let error = call(client, "set_tempo", json!({ "bar": 3, "beat": 4.5, "bpm": 100 })).await.unwrap_err();
    assert!(error.contains("bar 3 is 3.5 beats long (7/8)"), "{error}");
    let error = call(client, "set_tempo", json!({ "bar": 2, "bpm": 5 })).await.unwrap_err();
    assert!(error.contains("outside 20..=999 BPM"), "{error}");
    let error = call(client, "set_tempo", json!({ "bar": 1, "bpm": 100, "ramp": true })).await.unwrap_err();
    assert!(error.contains("after the start of the song"), "{error}");

    // A time signature set in the middle of what follows cuts a bar short, and the reply says which.
    let cut = ok(client, "set_time_signature", json!({ "bar": 2, "time_signature": "3/4" })).await;
    let said = cut["cut_bars"][0].as_str().unwrap();
    assert!(said.contains("bar 3 (3/4) is 1 beat(s) long instead of 3") && said.contains("beat 8"), "{cut}");
    let error = call(client, "set_time_signature", json!({ "bar": 9, "remove": true })).await.unwrap_err();
    assert!(error.contains("it changes at bar 2, bar 4, bar 6"), "{error}");
    let back = ok(client, "set_time_signature", json!({ "bar": 2, "remove": true })).await;
    assert!(back.get("cut_bars").is_none() && back["time_signature"].as_array().unwrap().len() == 3, "{back}");
    let error =
        call(client, "set_time_signature", json!({ "bar": 2, "time_signature": "5/4", "bars": 2 })).await.unwrap_err();
    assert!(error.contains("past the change of time signature at bar 3 (beat 8)"), "{error}");
    let error = call(client, "set_time_signature", json!({ "bar": 2, "time_signature": "4/5" })).await.unwrap_err();
    assert!(error.contains("1 to 32 over 1, 2, 4, 8 or 16"), "{error}");

    // Each is a step that can be taken back, and the journal says it in words.
    ok(client, "history", json!({ "action": "undo" })).await;
    let before = ok(client, "get_tempo_map", json!({})).await;
    assert!(before["cut_bars"].is_array(), "the 3/4 at bar 2 is back: {before}");
    ok(client, "history", json!({ "action": "redo" })).await;
    let journal = ok(client, "get_journal", json!({})).await.to_string();
    for said in [
        "Set the time signature to 7/8 at bar 3 for 2 bars",
        "Set the tempo to 140 BPM at bar 3",
        "Made the tempo glide to 120 BPM by bar 6",
        "Removed the change of tempo at bar 6",
    ] {
        assert!(journal.contains(said), "the journal does not say: {said}\n{journal}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_writing_tools_lay_each_bar_in_the_time_signature_it_has() {
    let fixture = connect().await;
    let client = &fixture.client;
    seven_eight_in_the_middle(client).await;
    let kit = track(client, "Kit", "builtin:drumkit", "default").await;
    let keys = track(client, "Keys", "builtin:polysynth", "soft_keys").await;
    let bass = track(client, "Bass", "builtin:polysynth", "bass").await;
    let lead = track(client, "Lead", "builtin:polysynth", "soft_keys").await;

    // Six chords, six bars: each begins with its bar, whatever its length.
    let chart = "Am | F | C | G | Am | F";
    let chords = ok(client, "write_chords", json!({ "track": keys, "progression": chart })).await;
    assert_eq!((chords["bars"].as_u64(), chords["written_beats"].as_f64()), (Some(6), Some(23.0)), "{chords}");
    assert_eq!(starts(client, &chords["clip_id"], None).await, [0.0, 4.0, 8.0, 11.5, 15.0, 19.0]);
    assert_eq!(
        chords["time_signatures"],
        json!(["4/4 in bar(s) 1, 2, 5, 6", "7/8 in bar(s) 3, 4"]),
        "the reply says which bars are which: {chords}"
    );
    let line = ok(client, "write_bass", json!({ "track": bass, "progression": chart, "rhythm": "pulse" })).await;
    let pulse = starts(client, &line["clip_id"], None).await;
    // A beat to the pulse in 4/4, and the three felt beats of 7/8 (2+2+3).
    assert_eq!(pulse[..8], [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    assert_eq!(pulse[8..14], [8.0, 9.0, 10.0, 11.5, 12.5, 13.5], "{pulse:?}");
    assert_eq!(pulse[14..], [15.0, 16.0, 17.0, 18.0, 19.0, 20.0, 21.0, 22.0]);

    // A groove written in 4/4 keeps its steps in the bars of 7/8 and is cut off where they end; its fill
    // ends with the bar it is in. The reply shows the lanes that were played there.
    let drums = ok(
        client,
        "write_drums",
        json!({ "track": kit, "groove": "rock", "bars": 4, "fill": "snare_roll", "lanes": { "closed_hat": "." } }),
    )
    .await;
    assert_eq!(drums["written_beats"], 15.0, "{drums}");
    let said = drums["warnings"].to_string();
    assert!(
        said.contains("'rock' groove is written in 4/4, but these bars are in 7/8")
            && said.contains("cut off where each bar ends"),
        "{drums}"
    );
    assert_eq!(drums["in_other_time_signatures"]["7/8"]["lanes"]["kick"], "X.......X.x...|X.....x.X.....", "{drums}");
    assert_eq!(drums["lanes"]["kick"], "X.......X.x.....|X.....x.X.......");
    let kicks = starts(client, &drums["clip_id"], Some(KICK)).await;
    assert_eq!(kicks[..9], [0.0, 2.0, 2.5, 4.0, 5.5, 6.0, 8.0, 10.0, 10.5], "{kicks:?}");
    let snares = starts(client, &drums["clip_id"], Some(SNARE)).await;
    assert!(snares.contains(&9.0) && snares.contains(&11.0), "the backbeat of bar 3: {snares:?}");
    let last = snares.last().unwrap();
    assert!(*last > 14.5 && *last < 15.0, "the fill runs up to the end of bar 4, at beat 15: {snares:?}");
    assert!(snares.iter().filter(|start| **start >= 13.0).count() >= 6, "a roll in the bar's last two beats");

    // Lanes divide each bar evenly: seven steps are the eighths of a bar of 7/8.
    let lanes = json!({ "kick": "x...x...x...x...|x...x...x...x...|x.x.x..|x..x..x" });
    let written = ok(client, "write_drums", json!({ "track": kit, "start": 23, "lanes": lanes })).await;
    assert_eq!(written["written_beats"], 16.0, "from beat 23 the song is in 4/4: {written}");
    let own = ok(client, "write_drums", json!({ "clip": drums["clip_id"], "mode": "replace", "lanes": lanes })).await;
    let hits = starts(client, &own["clip_id"], Some(KICK)).await;
    assert_eq!(hits[8..], [8.0, 9.0, 10.0, 11.5, 13.0, 14.5], "eighths, grouped as the lane groups them: {hits:?}");

    // A line is counted through in one time signature: in the bars of 7/8 it is checked against 3.5 beats,
    // and across the change it is two calls.
    let seven = ":0.5 1 2 3 4 5 6 7 | 1' 7 6 5 4 3 2";
    let melody =
        ok(client, "write_melody", json!({ "track": lead, "start": 8, "key": "A minor", "phrase": seven })).await;
    assert_eq!((melody["bars"].as_u64(), melody["written_beats"].as_f64()), (Some(2), Some(7.0)), "{melody}");
    let error = call(
        client,
        "write_melody",
        json!({ "track": lead, "start": 8, "key": "A minor", "phrase": ":1 1 2 3 4 | 5" }),
    )
    .await
    .unwrap_err();
    assert!(error.contains("but a bar of 7/8 has 3.5"), "{error}");
    let across = ":1 1 2 3 4 | 5 4 3 2 | :0.5 1 2 3 4 5 6 7";
    let error =
        call(client, "write_melody", json!({ "track": lead, "key": "A minor", "phrase": across })).await.unwrap_err();
    assert!(error.contains("cross a change of time signature (4/4 in bar(s) 1, 2; 7/8 in bar(s) 3)"), "{error}");
    let error = call(
        client,
        "write_melody",
        json!({ "track": lead, "start": 4, "key": "A minor", "phrase": ":1 1 2 3 4", "bars": 2 }),
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("cross a change of time signature (4/4 in bar(s) 1; 7/8 in bar(s) 2)")
            && error.contains("in a call of their own"),
        "{error}"
    );

    // What was written reads back bar by bar, in the song's own bars.
    let heard = ok(client, "analyze_music", json!({ "track": keys })).await;
    assert_eq!(heard["bars"], "1-6", "{heard}");
    let named: Vec<&str> = heard["chords"].as_array().unwrap().iter().map(|line| line.as_str().unwrap()).collect();
    // Each bar's chord, in whatever inversion the voice leading left it.
    let roots: Vec<&str> = named.iter().map(|line| line.split('/').next().unwrap()).collect();
    assert_eq!(roots, ["1: Am", "2: F", "3: C", "4: G", "5: Am", "6: F"], "{heard}");

    // Time that is cut out takes its tempo and its meter with it: without the bars of 7/8 the song is in
    // 4/4 at 120 throughout, and bar 3 holds what bar 5 held.
    let cut = ok(client, "edit_time", json!({ "action": "delete", "start": 8, "end": 15 })).await;
    assert_eq!(cut["length_beats"], 32.0, "{cut}");
    assert_eq!(
        cut["tempo_map"]["tempo"].as_array().unwrap().len(),
        1,
        "the reply says what became of the tempo: {cut}"
    );
    let map = ok(client, "get_tempo_map", json!({})).await;
    assert_eq!(
        (map["tempo"].as_array().unwrap().len(), map["time_signature"].as_array().unwrap().len()),
        (1, 1),
        "{map}"
    );
    assert_eq!(starts(client, &chords["clip_id"], None).await[..2], [0.0, 4.0]);
}

/// Where in `samples` something is struck near `second`: the frame at
/// which what follows is most unlike what went before, in the part of the
/// sound that a blow has and a ringing drum has not.
fn struck_near(samples: &[f32], rate: f64, second: f64) -> (f64, f64) {
    let (window, reach) = ((0.002 * rate) as usize, (0.015 * rate) as usize);
    // With silence before the first frame, so that a hit on it is found too.
    let lead = window + reach;
    let quiet = std::iter::repeat_n(0.0f64, lead);
    let sharp: Vec<f64> = quiet.chain(samples.windows(2).map(|pair| f64::from(pair[1] - pair[0]).powi(2))).collect();
    let at = (second * rate).round() as usize + lead;
    let energy = |from: usize| sharp[from..from + window].iter().sum::<f64>();
    let mut best = (0.0f64, at);
    for frame in at - reach..(at + reach).min(sharp.len() - window) {
        let rise = energy(frame) / (energy(frame - window) + 1e-12);
        if rise > best.0 {
            best = (rise, frame);
        }
    }
    ((best.1 as f64 - lead as f64) / rate, best.0)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_song_goes_from_four_four_into_seven_eight_and_back_and_its_hits_land_where_the_map_says() {
    let fixture = connect().await;
    let client = &fixture.client;
    let map = seven_eight_in_the_middle(client).await;
    assert_eq!(
        rows(&map["tempo"], &["bar", "beat", "seconds", "bpm"]),
        [
            vec![json!(1), json!(0.0), json!(0.0), json!(120.0)],
            vec![json!(3), json!(8.0), json!(4.0), json!(140.0)],
            vec![json!(5), json!(15.0), json!(7.0), json!(120.0)],
        ]
    );
    let kit = track(client, "Kit", "builtin:drumkit", "default").await;
    let keys = track(client, "Keys", "builtin:polysynth", "soft_keys").await;
    let bass = track(client, "Bass", "builtin:polysynth", "bass").await;

    // Six bars of each, written by the tools that think in bars.
    let chart = "Am | F | C | G | Am | F";
    let drums =
        ok(client, "write_drums", json!({ "track": kit, "groove": "rock", "bars": 6, "lanes": { "closed_hat": "." } }))
            .await;
    let chords = ok(client, "write_chords", json!({ "track": keys, "progression": chart, "rhythm": "stabs" })).await;
    let line = ok(client, "write_bass", json!({ "track": bass, "progression": chart })).await;
    for written in [&drums, &chords, &line] {
        assert_eq!((written["bars"].as_u64(), written["written_beats"].as_f64()), (Some(6), Some(23.0)), "{written}");
    }
    assert_eq!(starts(client, &chords["clip_id"], None).await, [0.0, 4.0, 8.0, 11.5, 15.0, 19.0]);
    assert_eq!(starts(client, &line["clip_id"], None).await, [0.0, 4.0, 8.0, 11.5, 15.0, 19.0]);
    let kicks = starts(client, &drums["clip_id"], Some(KICK)).await;
    let snares = starts(client, &drums["clip_id"], Some(SNARE)).await;
    let per_bar = |starts: &[f64], from: f64, to: f64| starts.iter().filter(|at| **at >= from && **at < to).count();
    assert_eq!(kicks.len(), 18, "three to a bar: {kicks:?}");
    assert_eq!(snares, [1.0, 3.0, 5.0, 7.0, 9.0, 11.0, 12.5, 14.5, 16.0, 18.0, 20.0, 22.0], "the backbeat of each bar");
    assert_eq!((per_bar(&kicks, 8.0, 11.5), per_bar(&kicks, 11.5, 15.0)), (3, 3));

    // The song as the map times it: 4 s of 4/4 at 120, 3 s of 7/8 at 140, 4 s more.
    let tempo = TempoMap::new(
        120.0,
        &[TempoChange { beat: 8.0, tempo: 140.0, ramp: false }, TempoChange { beat: 15.0, tempo: 120.0, ramp: false }],
    );
    assert!((tempo.seconds_at(23.0) - 11.0).abs() < 1e-9);

    // Rendered, the drums alone: every hit is heard where the map has its beat.
    let wav = fixture.folder.path().join("seven.wav");
    let sections = json!([{ "bars": [1, 2] }, { "bars": [3, 4] }, { "bars": [5, 6] }]);
    let report = ok(
        client,
        "render",
        json!({ "path": wav.display().to_string(), "tracks": [kit], "max_tail_seconds": 1, "sections": sections }),
    )
    .await;
    assert_eq!(
        rows(&report["sections"], &["name", "start", "end"]),
        [
            vec![json!("bars 1-2"), json!(0.0), json!(8.0)],
            vec![json!("bars 3-4"), json!(8.0), json!(15.0)],
            vec![json!("bars 5-6"), json!(15.0), json!(23.0)],
        ],
        "sections are the song's bars: {report}"
    );
    let seconds = report["duration_seconds"].as_f64().unwrap();
    assert!((11.0..12.1).contains(&seconds), "eleven seconds and what rings on: {report}");
    let audio = trill_media::decode(&wav).unwrap();
    let rate = audio.sample_rate;
    // A drum takes a moment to speak (the kick about a millisecond and a
    // half before its blow shows), so a hit is held to two milliseconds of
    // its beat; that notes fall on their beats to the frame is the
    // engine's own test.
    let mut worst = 0.0f64;
    for (drum, beats) in [("kick", &kicks), ("snare", &snares)] {
        for beat in beats.iter() {
            let wanted = tempo.seconds_at(*beat);
            let (heard, rise) = struck_near(&audio.left, rate, wanted);
            let off = (heard - wanted) * 1000.0;
            assert!(rise > 8.0, "no {drum} is struck at beat {beat} ({wanted:.3} s): the sound rises {rise:.1} times");
            assert!(off.abs() < 2.0, "the {drum} at beat {beat} is heard at {heard:.4} s, {off:+.2} ms from its beat");
            worst = worst.max(off.abs());
        }
    }
    let hits = kicks.len() + snares.len();
    println!("{hits} hits through 4/4 at 120, 7/8 at 140 and back: the worst is heard {worst:.2} ms from its beat");
    // Had the song stayed at 120, the last backbeat would be half a second later than it is.
    let late = 22.0 * 0.5 - tempo.seconds_at(22.0);
    assert!((late - 0.5).abs() < 1e-9, "{late}");

    // The whole song measures as long as the map says, and each part is heard.
    let measured = ok(client, "measure", json!({ "max_tail_seconds": 0, "sections": [] })).await;
    assert!((measured["duration_seconds"].as_f64().unwrap() - 11.0).abs() < 0.05, "{measured}");
    let heard: Vec<f64> =
        measured["tracks"].as_array().unwrap().iter().map(|track| track["peak_db"].as_f64().unwrap()).collect();
    assert!(heard.len() == 3 && heard.iter().all(|peak| *peak > -40.0), "{measured}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_midi_file_with_changes_of_tempo_and_meter_comes_in_and_goes_out_the_same() {
    let fixture = connect().await;
    let client = &fixture.client;
    seven_eight_in_the_middle(client).await;
    let keys = track(client, "Keys", "builtin:polysynth", "soft_keys").await;
    let kit = track(client, "Kit", "builtin:drumkit", "default").await;
    ok(client, "write_chords", json!({ "track": keys, "progression": "Am | F | C | G | Am | F" })).await;
    ok(client, "write_drums", json!({ "track": kit, "groove": "rock", "bars": 6 })).await;
    let before = ok(client, "get_tempo_map", json!({})).await;

    let first = fixture.folder.path().join("first.mid");
    ok(client, "export_midi", json!({ "path": first.display().to_string() })).await;
    let file = trill_core::midi_file::parse(&std::fs::read(&first).unwrap()).unwrap();
    assert_eq!((file.tempo, file.tempo_changes.len(), file.meter_changes.len()), (Some(120.0), 2, 2));

    // Into an empty song, the file brings its tempo and its meter with it.
    ok(client, "new_project", json!({})).await;
    let imported = ok(client, "import_midi", json!({ "path": first.display().to_string() })).await;
    assert!(imported["file_changes"].as_str().unwrap().contains("the song has them now"), "{imported}");
    let after = ok(client, "get_tempo_map", json!({})).await;
    assert_eq!((&after["tempo"], &after["time_signature"]), (&before["tempo"], &before["time_signature"]));
    // Its parts end on the song's bar lines: six bars are 23 beats.
    let lengths: Vec<f64> =
        imported["tracks"].as_array().unwrap().iter().map(|part| part["length"].as_f64().unwrap()).collect();
    assert_eq!(lengths, [23.0, 23.0]);

    // And out again it is the file it was.
    let second = fixture.folder.path().join("second.mid");
    ok(client, "export_midi", json!({ "path": second.display().to_string() })).await;
    let again = trill_core::midi_file::parse(&std::fs::read(&second).unwrap()).unwrap();
    assert_eq!(again, file);
    assert_eq!(std::fs::read(&second).unwrap(), std::fs::read(&first).unwrap(), "to the byte");

    // Beside a song that has tracks, a file leaves the tempo alone unless asked.
    ok(client, "new_project", json!({})).await;
    track(client, "Pad", "builtin:polysynth", "soft_keys").await;
    let beside = ok(client, "import_midi", json!({ "path": first.display().to_string(), "at": 16 })).await;
    assert!(beside["file_changes"].as_str().unwrap().contains("the song kept its own"), "{beside}");
    assert_eq!(ok(client, "get_tempo_map", json!({})).await["tempo"].as_array().unwrap().len(), 1);
    ok(client, "history", json!({ "action": "undo" })).await;
    let taken =
        ok(client, "import_midi", json!({ "path": first.display().to_string(), "at": 16, "use_tempo": true })).await;
    assert!(taken["tracks"].as_array().unwrap().len() == 2, "{taken}");
    let later = ok(client, "get_tempo_map", json!({})).await;
    assert_eq!(
        rows(&later["tempo"], &["bar", "beat", "bpm"]),
        [
            vec![json!(1), json!(0.0), json!(120.0)],
            vec![json!(5), json!(16.0), json!(120.0)],
            vec![json!(7), json!(24.0), json!(140.0)],
            vec![json!(9), json!(31.0), json!(120.0)],
        ],
        "the file's changes begin where it was put: {later}"
    );
}
