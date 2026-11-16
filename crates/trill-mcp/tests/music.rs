//! The music-writing tools, driven the way an assistant drives them: over
//! the real MCP transport, in musical terms, without ever spelling a note.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::sync::Arc;
use trill_engine::SharedSession;
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Runtime, RuntimeOptions};

type Client = RunningService<RoleClient, ClientConfig>;

struct Fixture {
    session: SharedSession,
    client: Client,
    _server: HttpServer,
}

async fn connect() -> Fixture {
    // Nothing of the user's is read or written: no settings, no library.
    let runtime = Arc::new(Runtime::start(RuntimeOptions::hermetic()).unwrap());
    let session = runtime.session.clone();
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Fixture { session, client, _server: server }
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

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("not a list: {value}"))
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}

/// The notes of a clip as `(name, start)` pairs.
async fn notes_of(client: &Client, clip: &Value) -> Vec<(String, f64, u64)> {
    ok(client, "get_notes", json!({ "clip": clip })).await["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| {
            (
                note["name"].as_str().unwrap().to_string(),
                note["start"].as_f64().unwrap(),
                note["velocity"].as_u64().unwrap(),
            )
        })
        .collect()
}

const PROGRESSION: &str = "Am9 | Fmaj7 | C G/B | E7";

#[tokio::test(flavor = "multi_thread")]
async fn the_music_tools_are_listed_and_introduced() {
    let fixture = connect().await;
    let tools = fixture.client.list_all_tools().await.unwrap();
    let find =
        |name: &str| tools.iter().find(|tool| tool.name == name).unwrap_or_else(|| panic!("missing tool {name}"));
    for name in [
        "write_chords",
        "write_drums",
        "write_arpeggio",
        "write_bass",
        "write_melody",
        "analyze_music",
        "music_reference",
    ] {
        let tool = find(name);
        assert!(tool.description.as_ref().is_some_and(|d| d.len() > 40), "{name} needs a description");
        assert_eq!(serde_json::to_value(&tool.input_schema).unwrap()["type"], "object", "{name}");
    }
    // The original tools are still there beside them.
    for name in ["get_project", "create_clip", "render", "history"] {
        find(name);
    }

    // Schemas teach the notations where the model will read them.
    let chords = serde_json::to_string(&find("write_chords").input_schema).unwrap();
    for taught in ["Am9 | Fmaj7 | C G/B | E7", "ii7 V7", "drop2", "tresillo", "\"required\":[\"progression\"]"] {
        assert!(chords.contains(taught), "write_chords schema should mention {taught}: {chords}");
    }
    let drums = serde_json::to_string(&find("write_drums").input_schema).unwrap();
    for taught in ["boom_bap", "x...x...x...x...", "ghost", "snare_roll"] {
        assert!(drums.contains(taught), "write_drums schema should mention {taught}");
    }
    let melody = serde_json::to_string(&find("write_melody").input_schema).unwrap();
    assert!(melody.contains(":0.5 5 6 5 4 3:2") && melody.contains("\"required\":[\"phrase\"]"), "{melody}");
    assert_eq!(find("analyze_music").annotations.as_ref().and_then(|a| a.read_only_hint), Some(true));

    let instructions = fixture.client.peer_info().and_then(|info| info.instructions.clone()).unwrap_or_default();
    assert!(instructions.contains("create_track per part"), "the original guidance is kept");
    for tool in [
        "write_chords",
        "write_drums",
        "write_bass",
        "write_arpeggio",
        "write_melody",
        "analyze_music",
        "music_reference",
    ] {
        assert!(instructions.contains(tool), "the instructions should introduce {tool}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_writes_a_song_without_spelling_a_note() {
    let fixture = connect().await;
    let client = &fixture.client;
    ok(client, "set_project", json!({ "name": "Glass Hours", "tempo": 124 })).await;
    let drums = track(client, "Drums", "builtin:drumkit", "909").await;
    let pad = track(client, "Pad", "builtin:polysynth", "warm_pad").await;
    let bass = track(client, "Bass", "builtin:polysynth", "bass").await;
    let pluck = track(client, "Pluck", "builtin:polysynth", "pluck").await;
    let lead = track(client, "Lead", "builtin:polysynth", "lead").await;

    // A groove with a fill.
    let beat =
        ok(client, "write_drums", json!({ "track": drums, "groove": "house", "bars": 8, "fill": "build" })).await;
    assert_eq!(beat["created"], true);
    assert_eq!((beat["bars"].as_u64(), beat["written_beats"].as_f64()), (Some(8), Some(32.0)));
    assert_eq!(beat["lanes"]["kick"], "X...X...X...X...");
    assert_eq!((beat["fill"]["name"].as_str(), beat["fill"]["in_bars"].clone()), (Some("build"), json!([8])));
    // The build is a whole bar: it says what of the groove it took the place of.
    let replaced = &beat["fill"]["bars"][0];
    assert_eq!((replaced["bar"].as_u64(), replaced["from_beat"].as_f64()), (Some(8), Some(1.0)));
    assert_eq!(replaced["replaced"], json!({ "clap": 2, "closed_hat": 12, "kick": 4, "open_hat": 4 }));
    assert_eq!(replaced["played"], json!({ "kick": 4, "snare": 16 }));
    assert_eq!(beat["swing"], 54.0, "the groove brings its own swing");
    assert_eq!(beat["hits"]["kick"], 32, "four on the floor for eight bars, the fill included");
    assert!(beat.get("warnings").is_none(), "{beat}");

    // Chords.
    let chords = ok(
        client,
        "write_chords",
        json!({ "track": pad, "progression": PROGRESSION, "voicing": "pad", "bars": 8, "swing": 54 }),
    )
    .await;
    let written: Vec<(&str, &str)> = chords["chords"]
        .as_array()
        .unwrap()
        .iter()
        .map(|chord| (chord["at"].as_str().unwrap(), chord["chord"].as_str().unwrap()))
        .collect();
    assert_eq!(written, [("1.1", "Am9"), ("2.1", "Fmaj7"), ("3.1", "C"), ("3.3", "G/B"), ("4.1", "E7")]);
    assert_eq!(chords["chords"][0]["voicing"], "A2 E3 C4 G4 B4");
    assert_eq!((chords["bars"].as_u64(), chords["voicing"].as_str()), (Some(8), Some("pad")));
    assert!(chords.get("warnings").is_none(), "{chords}");

    // Bass, arpeggio and melody over the same harmony.
    let line = ok(
        client,
        "write_bass",
        json!({ "track": bass, "progression": PROGRESSION, "style": "octaves", "bars": 8, "swing": 54 }),
    )
    .await;
    assert_eq!(strings(&line["chords"]), ["Am9", "Fmaj7", "C G/B", "E7"]);
    assert!(line["first_bars"].as_str().unwrap().starts_with("A1 A2 A1 A2 A1 A2 A1 A2 | F1 F2"), "{line}");
    assert_eq!(line["pitch_range"], "E1-C3");
    assert!(line.get("warnings").is_none(), "{line}");

    let arpeggio = ok(
        client,
        "write_arpeggio",
        json!({ "track": pluck, "progression": PROGRESSION, "order": "up_down", "rate": "1/16", "octaves": 2, "bars": 8, "swing": 54 }),
    )
    .await;
    assert_eq!(arpeggio["note_count"], 8 * 16);
    assert_eq!(arpeggio["rate"], 0.25);
    assert!(arpeggio.get("warnings").is_none(), "{arpeggio}");

    let phrase = ":0.5 5 r 5 6 5:1 3:1 | 1':1.5 7:0.5 6:1 5:1 | :1 5:1.5 3:0.5 5 2 | #7,:2 2:1 3:1 | \
                  :0.5 5 r 5 6 5:1 1':1 | 3':1.5 2':0.5 1':1 6:1 | :1 5:1.5 4:0.5 3 2 | 3:3 r";
    let melody =
        ok(client, "write_melody", json!({ "track": lead, "key": "A minor", "octave": 4, "phrase": phrase })).await;
    assert_eq!((melody["bars"].as_u64(), melody["key"].as_str()), (Some(8), Some("A minor")));
    assert!(melody["pitches"].as_str().unwrap().starts_with("E5 E5 F5 E5 C5 | A5 G5 F5 E5"), "{melody}");
    assert!(
        melody["notes"][0].as_str().unwrap().contains("1 note(s) are outside A minor (fine if intended): G#4 at 4.1"),
        "{melody}"
    );

    // The song is there, and each tool call was one step of its history.
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["length_beats"], 32.0);
    assert!(project["tracks"].as_array().unwrap().iter().all(|track| track["clips"].as_array().unwrap().len() == 1));

    // Reading it back: the key and the chords that were written.
    let harmony = ok(client, "analyze_music", json!({ "track": pad, "resolution": "half_bar" })).await;
    assert_eq!(
        strings(&harmony["chords"]),
        ["1: Am9", "2: Fmaj7", "3: C | G/B", "4: E7", "5: Am9", "6: Fmaj7", "7: C | G/B", "8: E7"]
    );
    assert_eq!(harmony["key"]["estimate"], "A minor");
    let song = ok(client, "analyze_music", json!({ "resolution": "half_bar" })).await;
    assert_eq!(song["scope"], "song (Pad, Bass, Pluck, Lead)", "drums are left out of the harmony");
    assert_eq!(song["key"]["estimate"], "A minor");
    assert!(song["key"]["confidence"].as_f64().unwrap() > 0.7, "{song}");
    assert_eq!(song["bars"], "1-8");
    let heard = strings(&song["chords"]);
    for (bar, chord) in [("1: ", "Am"), ("2: ", "Fmaj7"), ("4: ", "E7")] {
        let line = heard.iter().find(|line| line.starts_with(bar)).unwrap();
        assert!(line[bar.len()..].starts_with(chord), "bar {bar} should be heard as {chord}: {heard:?}");
    }
    // The only notes outside A minor are the G#s of the two E7 bars and the melody's leading tone.
    assert!(song["outside_key"]["count"].as_u64().unwrap() >= 5, "{song}");
    let outside = strings(&song["outside_key"]["notes"]);
    assert!(
        outside
            .iter()
            .all(|note| note == "..."
                || (note.starts_with("G#") && (note.contains(" at 4.") || note.contains(" at 8.")))),
        "{outside:?}"
    );

    // Render it and read the numbers.
    let dir = std::env::temp_dir().join(format!("trill-music-{}", std::process::id()));
    let report = ok(client, "render", json!({ "path": dir.join("glass-hours.wav").display().to_string() })).await;
    assert!(report["duration_seconds"].as_f64().unwrap() >= 32.0 * 60.0 / 124.0, "{report}");
    assert_eq!(report["clipped_samples"], 0);
    let peaks: Vec<f64> =
        report["tracks"].as_array().unwrap().iter().map(|track| track["peak_db"].as_f64().unwrap()).collect();
    assert_eq!(peaks.len(), 5);
    assert!(peaks.iter().all(|peak| *peak > -40.0), "every part should be audible: {peaks:?}");
    std::fs::remove_dir_all(&dir).ok();

    // Five writes, five undo steps: the tracks remain, the clips go.
    assert_eq!(ok(client, "history", json!({ "action": "undo", "steps": 5 })).await["steps_applied"], 5);
    let project = ok(client, "get_project", json!({})).await;
    assert_eq!(project["tracks"].as_array().unwrap().len(), 5);
    assert!(project["tracks"].as_array().unwrap().iter().all(|track| track["clips"].as_array().unwrap().is_empty()));
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_musical_input_changes_nothing_and_explains_itself() {
    let fixture = connect().await;
    let client = &fixture.client;
    let keys = track(client, "Keys", "builtin:polysynth", "keys").await;
    let kit = track(client, "Kit", "builtin:drumkit", "default").await;
    let clip = ok(client, "write_chords", json!({ "track": keys, "progression": "C | F" })).await["clip_id"].clone();
    let before = fixture.session.lock().project().clone();

    let cases: Vec<(&'static str, Value, &[&str])> = vec![
        (
            "write_chords",
            json!({ "track": keys, "progression": "Am | Hm7" }),
            &["bar 2: 'Hm7' is not a chord symbol", "expected a root A-G"],
        ),
        ("write_chords", json!({ "track": keys, "progression": "Am | Cm7b" }), &["bar 2: 'Cm7b'", "expected b5"]),
        ("write_chords", json!({ "track": keys, "progression": "C2 | F" }), &["'C2'", "ambiguous", "sus2", "add9"]),
        (
            "write_chords",
            json!({ "track": keys, "progression": "ii7 V7 | Imaj7" }),
            &["'ii7' is a Roman numeral, which needs a key"],
        ),
        (
            "write_chords",
            json!({ "track": keys, "progression": "C:3 G:2 |" }),
            &["adds up to 5 beats, but a bar of 4/4 has 4"],
        ),
        (
            "write_chords",
            json!({ "progression": "C" }),
            &["give a `track` to create a clip on, or a `clip` to write into"],
        ),
        ("write_chords", json!({ "track": keys, "clip": clip, "progression": "C" }), &["not both"]),
        (
            "write_chords",
            json!({ "track": keys, "offset": 4, "progression": "C" }),
            &["`offset` and `mode` apply when writing into an existing `clip`"],
        ),
        ("write_chords", json!({ "clip": clip, "start": 4, "progression": "C" }), &["go with `track`"]),
        ("write_chords", json!({ "track": 999, "progression": "C" }), &["track 999 not found", "(Keys)"]),
        ("write_chords", json!({ "clip": 999, "progression": "C" }), &["clip 999 not found", "Chords on Keys"]),
        (
            "write_chords",
            json!({ "track": keys, "progression": "C", "voicing": "jazzy" }),
            &["'jazzy' is not a voicing", "drop2"],
        ),
        ("write_chords", json!({ "track": keys, "progression": "C", "low": "C4", "high": "E4" }), &["under an octave"]),
        (
            "write_chords",
            json!({ "track": keys, "progression": "C", "low": "H2" }),
            &["`low`: 'H2' is not a note name"],
        ),
        (
            "write_chords",
            json!({ "track": keys, "progression": "C", "rhythm": "bossa" }),
            &["'bossa' is not a rhythm", "tresillo"],
        ),
        ("write_chords", json!({ "track": keys, "progression": "C", "key": "C foo" }), &["unknown scale 'foo'"]),
        ("write_chords", json!({ "track": keys, "progression": "C", "swing": 90 }), &["swing 90 is outside 50..=75"]),
        ("write_chords", json!({ "track": keys, "progression": "C", "bars": 0 }), &["`bars` 0 is outside 1..=512"]),
        (
            "write_chords",
            json!({ "track": keys, "progression": "C", "velocity": 0 }),
            &["`velocity` 0 is outside 1..=127"],
        ),
        (
            "write_chords",
            json!({ "track": keys, "progression": "C", "humanize_timing": 3 }),
            &["`humanize_timing` 3 is outside 0..=0.25"],
        ),
        ("write_drums", json!({ "track": kit }), &["nothing to play", "boom_bap"]),
        ("write_drums", json!({ "track": kit, "groove": "polka" }), &["'polka' is not a groove", "uk_garage"]),
        (
            "write_drums",
            json!({ "track": kit, "lanes": { "kick": "x..q" } }),
            &["lane 'kick': 'x..q': 'q' is not a step", "X = accent"],
        ),
        ("write_drums", json!({ "track": kit, "lanes": { "gong": "x..." } }), &["'gong' is not a drum", "closed_hat"]),
        (
            "write_drums",
            json!({ "track": kit, "groove": "rock", "fill": "solo" }),
            &["'solo' is not a fill", "snare_roll"],
        ),
        (
            "write_melody",
            json!({ "track": keys, "phrase": "1 2 3" }),
            &["phrase token 1 '1': a scale degree needs a key"],
        ),
        (
            "write_melody",
            json!({ "track": keys, "key": "C", "phrase": "1 2 3 4 | 5:2 6 | 1" }),
            &["bar 2 of the phrase is 3 beats long, but a bar of 4/4 has 4"],
        ),
        ("write_melody", json!({ "track": keys, "phrase": "C4 D4 X4" }), &["phrase token 3 'X4'"]),
        (
            "write_bass",
            json!({ "track": keys, "progression": "C", "style": "slap" }),
            &["'slap' is not a bass style", "walking"],
        ),
        (
            "write_arpeggio",
            json!({ "track": keys, "progression": "C", "rate": "1/7" }),
            &["`rate`: '1/7' is not a note value"],
        ),
        (
            "write_arpeggio",
            json!({ "track": keys, "progression": "C", "order": "up", "pattern": [1, 2] }),
            &["`order` or `pattern`, not both"],
        ),
        (
            "write_arpeggio",
            json!({ "track": keys, "progression": "C", "order": "sideways" }),
            &["'sideways' is not an arpeggio order"],
        ),
        ("analyze_music", json!({ "clip": 999 }), &["clip 999 not found"]),
        ("analyze_music", json!({ "resolution": "beat" }), &["`resolution` 'beat' should be"]),
        ("music_reference", json!({ "topic": "harmony" }), &["'harmony' is not a topic", "chords"]),
    ];
    for (tool, arguments, expected) in cases {
        let error = match call(client, tool, arguments.clone()).await {
            Err(error) => error,
            Ok(reply) => panic!("{tool} {arguments} should fail, but replied {reply}"),
        };
        for part in expected {
            assert!(error.contains(part), "{tool} {arguments}\n  said: {error}\n  expected to mention: {part}");
        }
    }
    assert_eq!(fixture.session.lock().project(), &before, "failed calls must not change the project");
    // Nor did they leave anything in the history: one undo removes the clip written before them.
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(ok(client, "get_project", json!({})).await["tracks"][0]["clips"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_warn_about_what_cannot_be_heard() {
    let fixture = connect().await;
    let client = &fixture.client;
    let pad = track(client, "Pad", "builtin:polysynth", "warm_pad").await;
    let bass = track(client, "Bass", "builtin:polysynth", "sub_bass").await;
    let kit = track(client, "Kit", "builtin:drumkit", "tight").await;
    let empty = ok(client, "create_track", json!({ "name": "Empty" })).await["track_id"].as_u64().unwrap();
    let warnings = |reply: &Value| reply.get("warnings").map(strings).unwrap_or_default().join("\n");
    let remarks = |reply: &Value| reply.get("notes").map(strings).unwrap_or_default().join("\n");

    // The wrong kind of instrument for the part, or none.
    let reply = ok(client, "write_chords", json!({ "track": kit, "progression": "C" })).await;
    assert!(warnings(&reply).contains("is a drum track: these pitches will trigger drums"), "{reply}");
    let reply = ok(client, "write_drums", json!({ "track": pad, "groove": "rock" })).await;
    assert!(warnings(&reply).contains("is not a drum track: these drum notes will play as pitches"), "{reply}");
    let reply = ok(client, "write_bass", json!({ "track": empty, "progression": "C" })).await;
    assert!(warnings(&reply).contains("has no instrument, so this will be silent"), "{reply}");

    // A register the preset is not made for.
    let reply = ok(client, "write_bass", json!({ "track": pad, "progression": "Am | F" })).await;
    assert!(
        warnings(&reply).contains("2 note(s) are below C2, lower than the 'warm_pad' preset is voiced for"),
        "{reply}"
    );
    let reply = ok(client, "write_chords", json!({ "track": bass, "progression": "Am | F" })).await;
    assert!(warnings(&reply).contains("above C4, where the 'sub_bass' preset sounds thin"), "{reply}");
    let reply = ok(client, "write_bass", json!({ "track": bass, "progression": "Am | F" })).await;
    assert!(reply.get("warnings").is_none(), "a bass line on a bass preset is as intended: {reply}");

    // Chords voiced where they turn to mud.
    let reply = ok(
        client,
        "write_chords",
        json!({ "track": pad, "progression": "Cm7 | Fm7", "low": "C2", "high": "C3", "start": 16 }),
    )
    .await;
    assert!(warnings(&reply).contains("close together low down, which sounds muddy"), "{reply}");
    // A register too small for the voicing.
    let reply = ok(client, "write_chords", json!({ "track": pad, "progression": "Cmaj9", "voicing": "open", "low": "C4", "high": "C5", "voices": 5, "start": 32 })).await;
    assert!(warnings(&reply).contains("did not fit between C4 and C5 in the open voicing"), "{reply}");

    // More music than the clip holds.
    let short = ok(client, "create_clip", json!({ "track": pad, "start": 64, "length": 4 })).await["clip_id"].clone();
    let reply = ok(client, "write_chords", json!({ "clip": short, "progression": "C | F | G" })).await;
    assert!(
        warnings(&reply).contains("start at or after beat 4 of the clip, past its end, and will not play"),
        "{reply}"
    );

    // A groove against the project's meter or tempo, and a lane with a slip in it.
    let reply = ok(client, "write_drums", json!({ "track": kit, "groove": "six_eight", "start": 8 })).await;
    assert!(warnings(&reply).contains("'six_eight' groove is written in 6/8, but the project is in 4/4"), "{reply}");
    let reply = ok(client, "write_drums", json!({ "track": kit, "groove": "drum_and_bass", "start": 16 })).await;
    assert!(
        remarks(&reply).contains("'drum_and_bass' is usually played at 165-176 BPM; the project is at 120"),
        "{reply}"
    );
    assert!(
        reply.get("notes").is_some_and(|notes| notes.as_array().unwrap().len() == 1),
        "the kit already suits: {reply}"
    );
    let reply = ok(client, "write_drums", json!({ "track": kit, "groove": "house", "start": 40 })).await;
    assert!(remarks(&reply).contains("written for the '909' drum kit preset; the track is on 'tight'"), "{reply}");
    let reply =
        ok(client, "write_drums", json!({ "track": kit, "lanes": { "kick": "x...x...x...x.." }, "start": 24 })).await;
    assert!(warnings(&reply).contains("lane 'kick' bar 1 has 15 steps"), "{reply}");
    let reply = ok(
        client,
        "write_drums",
        json!({ "track": kit, "groove": "rock", "bars": 2, "fill": "toms", "fill_every": 4, "start": 32 }),
    )
    .await;
    assert!(warnings(&reply).contains("`fill_every` 4 is more than the 2 bar(s) written"), "{reply}");

    // Readings the caller may not expect are said out loud.
    let reply = ok(
        client,
        "write_chords",
        json!({ "track": pad, "key": "A minor", "progression": "i | bVI | vii° | V7", "start": 80 }),
    )
    .await;
    let read: Vec<(&str, &str)> = reply["chords"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["written"].as_str().unwrap(), c["chord"].as_str().unwrap()))
        .collect();
    assert_eq!(read, [("i", "Am"), ("bVI", "F"), ("vii°", "G#dim"), ("V7", "E7")]);
    assert!(
        remarks(&reply).contains("bVI in A minor is F") && remarks(&reply).contains("vii° in A minor is G#dim"),
        "{reply}"
    );
    let reply = ok(client, "write_chords", json!({ "track": pad, "progression": "C G Am | F", "start": 96 })).await;
    assert!(remarks(&reply).contains("3 chords share the bar evenly, 1.333 beats each"), "{reply}");
    let reply = ok(
        client,
        "write_melody",
        json!({ "track": pad, "key": "C", "phrase": ":0.5 5 5 | :1 1' 7 6 5", "start": 112 }),
    )
    .await;
    assert!(remarks(&reply).contains("opens with a pickup of 1 beat(s)"), "{reply}");
    assert_eq!(reply["bars"], 2);
    let reply = ok(
        client,
        "write_bass",
        json!({ "track": bass, "progression": "C | F", "style": "walking", "rhythm": "eighths", "start": 8 }),
    )
    .await;
    assert!(remarks(&reply).contains("`rhythm` was not used"), "{reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn writing_into_clips_replaces_layers_loops_and_repeats_exactly() {
    let fixture = connect().await;
    let client = &fixture.client;
    let kit = track(client, "Kit", "builtin:drumkit", "909").await;
    let keys = track(client, "Keys", "builtin:polysynth", "keys").await;

    // Two bars written, looped across sixteen.
    let beat =
        ok(client, "write_drums", json!({ "track": kit, "groove": "boom_bap", "clip_length": 64, "name": "Beat" }))
            .await;
    assert_eq!((beat["bars"].as_u64(), beat["loops"].as_str()), (Some(2), Some("8 beats written, looped to fill 64")));
    // Two lengths, each under its own name: what was written, and the clip it loops in.
    assert_eq!(
        (beat["written_beats"].as_f64(), beat["clip_length"].as_f64(), beat.get("length")),
        (Some(8.0), Some(64.0), None)
    );
    let clip = ok(client, "get_notes", json!({ "clip": beat["clip_id"] })).await;
    assert_eq!(
        (clip["name"].as_str(), clip["length"].as_f64(), clip["loop_length"].as_f64()),
        (Some("Beat"), Some(64.0), Some(8.0))
    );
    // Sixteenth swing moves the kick on the "a" of two late, and leaves the downbeat alone.
    let kicks: Vec<f64> =
        notes_of(client, &beat["clip_id"]).await.into_iter().filter(|note| note.0 == "C2").map(|note| note.1).collect();
    assert_eq!(kicks[0], 0.0);
    assert_eq!(kicks[1], 1.79, "1.75 swung at 58%");

    // Replace only the bars written; the rest of the clip stays.
    let chords =
        ok(client, "write_chords", json!({ "track": keys, "progression": "C | F | G | C", "bass": "root" })).await;
    let id = chords["clip_id"].clone();
    let before = notes_of(client, &id).await;
    assert_eq!(before.len(), 16);
    let patched =
        ok(client, "write_chords", json!({ "clip": id, "offset": 8, "progression": "Am7", "bass": "root" })).await;
    assert!(patched.get("created").is_none());
    assert_eq!((patched["bars"].as_u64(), patched["note_count"].as_u64()), (Some(1), Some(5)));
    let after = notes_of(client, &id).await;
    assert_eq!(after.len(), 17, "four notes of G out, five of Am7 in");
    let elsewhere = |notes: &[(String, f64, u64)]| {
        notes.iter().filter(|note| note.1 < 8.0 || note.1 >= 12.0).cloned().collect::<Vec<_>>()
    };
    assert_eq!(elsewhere(&after), elsewhere(&before), "the other bars are untouched");
    let heard = ok(client, "analyze_music", json!({ "clip": id })).await;
    assert_eq!(strings(&heard["chords"]), ["1: C", "2: F", "3: Am7", "4: C"]);

    // Layering keeps what is there; one undo takes the layer away again.
    let layered = ok(client, "write_melody", json!({ "clip": id, "mode": "add", "phrase": "G5:2 E5:2 | F5:4" })).await;
    assert_eq!(layered["note_count"], 3);
    assert_eq!(notes_of(client, &id).await.len(), 20);
    ok(client, "history", json!({ "action": "undo" })).await;
    assert_eq!(notes_of(client, &id).await, after);

    // The same request writes the same notes, humanizing included; another seed, other notes.
    let request = |seed: u32| json!({ "clip": id, "progression": "Dm9 | G13", "rhythm": "charleston", "strum": 0.03, "humanize_timing": 0.02, "humanize_velocity": 8, "seed": seed });
    ok(client, "write_chords", request(7)).await;
    let first = notes_of(client, &id).await;
    ok(client, "write_chords", request(7)).await;
    assert_eq!(notes_of(client, &id).await, first);
    ok(client, "write_chords", request(8)).await;
    let other = notes_of(client, &id).await;
    assert_eq!(other.len(), first.len());
    assert_ne!(other, first);
    assert!(
        first.iter().any(|note| note.1.fract() != 0.0 && (note.1 * 2.0).fract() != 0.0),
        "strummed and humanized starts are off the grid"
    );

    // `bars` repeats a short progression and cuts a long one.
    let long = ok(
        client,
        "write_arpeggio",
        json!({ "track": keys, "start": 32, "progression": "Am | F", "bars": 5, "rate": 0.5 }),
    )
    .await;
    assert_eq!(
        (long["bars"].as_u64(), long["note_count"].as_u64(), long["written_beats"].as_f64()),
        (Some(5), Some(40), Some(20.0))
    );
    let cut =
        ok(client, "write_bass", json!({ "track": keys, "start": 64, "progression": "Am | F | C | G", "bars": 2 }))
            .await;
    assert_eq!((cut["bars"].as_u64(), cut["note_count"].as_u64()), (Some(2), Some(2)));
}

#[tokio::test(flavor = "multi_thread")]
async fn other_meters_keys_and_styles() {
    let fixture = connect().await;
    let client = &fixture.client;
    ok(client, "set_project", json!({ "tempo": 96, "time_signature": "6/8" })).await;
    let kit = track(client, "Kit", "builtin:drumkit", "default").await;
    let keys = track(client, "Keys", "builtin:polysynth", "soft_keys").await;
    let bass = track(client, "Bass", "builtin:polysynth", "bass").await;

    // 6/8: a bar is three beats, the groove and its fill agree with the meter.
    let beat =
        ok(client, "write_drums", json!({ "track": kit, "groove": "six_eight", "bars": 4, "fill": "six_eight_toms" }))
            .await;
    assert_eq!((beat["written_beats"].as_f64(), beat.get("warnings")), (Some(12.0), None));
    let chords = ok(client, "write_chords", json!({ "track": keys, "key": "E minor", "progression": "i | bVI bVII | i | V7", "rhythm": "pulse", "bass": "root" })).await;
    assert_eq!(chords["written_beats"], 12.0);
    let starts: Vec<&str> =
        chords["chords"].as_array().unwrap().iter().map(|chord| chord["at"].as_str().unwrap()).collect();
    assert_eq!(starts, ["1.1", "2.1", "2.2.5", "3.1", "4.1"], "two chords split a 6/8 bar on its second dotted beat");
    let walking = ok(
        client,
        "write_bass",
        json!({ "track": bass, "key": "E minor", "progression": "i | bVI bVII | i | V7", "style": "walking" }),
    )
    .await;
    assert_eq!(walking["note_count"], 8, "two dotted beats a bar");
    assert!(walking["first_bars"].as_str().unwrap().starts_with("E1 "), "{walking}");
    let heard = ok(client, "analyze_music", json!({ "track": keys, "resolution": "half_bar" })).await;
    assert_eq!(strings(&heard["chords"]), ["1: Em", "2: C | D", "3: Em", "4: B7"]);
    assert_eq!((heard["key"]["estimate"].as_str(), heard["bars"].as_str()), (Some("E minor"), Some("1-4")));

    // 7/8, in a flat key: names come back with flats.
    ok(client, "set_project", json!({ "time_signature": "7/8" })).await;
    let odd = ok(client, "write_chords", json!({ "track": keys, "start": 14, "key": "Bb", "progression": "Imaj7 | vi7 | ii7 | V7", "voicing": "drop2", "bass": "root" })).await;
    assert_eq!(odd["written_beats"], 14.0);
    let names: Vec<&str> =
        odd["chords"].as_array().unwrap().iter().map(|chord| chord["chord"].as_str().unwrap()).collect();
    assert_eq!(names, ["Bbmaj7", "Gm7", "Cm7", "F7"]);
    assert!(odd["chords"][0]["voicing"].as_str().unwrap().starts_with("Bb"), "the bass note comes first: {odd}");
    assert!(
        odd["pitch_range"].as_str().unwrap().contains('b') && !odd["pitch_range"].as_str().unwrap().contains('#'),
        "{odd}"
    );
    let lanes = ok(client, "write_drums", json!({ "track": kit, "start": 14, "bars": 2, "lanes": { "kick": "x.x.x..", "snare": "..x.x.x", "hat": "xxxxxxxxxxxxxx" } })).await;
    assert_eq!((lanes["note_count"].as_u64(), lanes.get("warnings")), (Some(2 * (3 + 3 + 14)), None));

    // The drum track reads back as drums, not as harmony.
    let drums = ok(client, "analyze_music", json!({ "track": kit })).await;
    assert!(drums["drums"]["kick"].as_u64().unwrap() > 6 && drums.get("key").is_none(), "{drums}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_reference_and_analysis_serve_a_reader() {
    let fixture = connect().await;
    let client = &fixture.client;

    // The reference lists the vocabulary by topic.
    let overview = ok(client, "music_reference", json!({})).await;
    let topics: Vec<&str> =
        overview["topics"].as_array().unwrap().iter().map(|topic| topic["name"].as_str().unwrap()).collect();
    assert_eq!(
        topics,
        [
            "chords",
            "progressions",
            "scales",
            "voicings",
            "rhythms",
            "arpeggios",
            "bass",
            "drums",
            "grooves",
            "melody",
            "ornaments",
            "instruments"
        ]
    );
    for topic in &topics {
        let page = call(client, "music_reference", json!({ "topic": topic }))
            .await
            .unwrap_or_else(|error| panic!("{topic}: {error}"));
        assert!(page.as_object().is_some_and(|fields| !fields.is_empty()), "{topic}");
    }
    let grooves = ok(client, "music_reference", json!({ "topic": "grooves" })).await;
    let names: Vec<&str> =
        grooves["grooves"].as_array().unwrap().iter().map(|groove| groove["name"].as_str().unwrap()).collect();
    for expected in [
        "house",
        "boom_bap",
        "trap",
        "drum_and_bass",
        "uk_garage",
        "reggaeton",
        "funk",
        "rock",
        "half_time",
        "shuffle",
        "six_eight",
    ] {
        assert!(names.contains(&expected), "{expected} is missing from {names:?}");
    }
    assert!(grooves["fills"].as_array().unwrap().iter().any(|fill| fill["name"] == "snare_roll"));
    let chords = ok(client, "music_reference", json!({ "topic": "chords" })).await;
    assert!(strings(&chords["qualities"]).contains(&"m7b5".to_string()));
    assert_eq!(
        ok(client, "music_reference", json!({ "topic": "scales" })).await["scales"][4],
        json!({ "name": "dorian", "on_C": "C D Eb F G A Bb" })
    );
    assert!(
        ok(client, "music_reference", json!({ "topic": "drums" })).await["notation"]
            .as_str()
            .unwrap()
            .contains("g = ghost")
    );

    // Analysis reads what someone else wrote, note by note, and an empty song says so.
    assert_eq!(ok(client, "analyze_music", json!({})).await["note_count"], 0);
    let keys = track(client, "Keys", "builtin:polysynth", "keys").await;
    let clip = ok(
        client,
        "create_clip",
        json!({
            "track": keys, "start": 8, "length": 16,
            "notes": [
                ["D3", 0, 4], ["F4", 0, 4], ["A4", 0, 4], ["C5", 0, 4], ["E5", 1, 0.5], ["D5", 1.5, 0.5],
                ["G2", 4, 4], ["F4", 4, 4], ["B4", 4, 4], ["D5", 4, 4],
                ["C3", 8, 8], ["E4", 8, 8], ["G4", 8, 8], ["B4", 8, 8], ["F#5", 10, 0.5],
            ],
        }),
    )
    .await["clip_id"]
        .clone();
    let read = ok(client, "analyze_music", json!({ "clip": clip })).await;
    assert_eq!(read["bars"], "3-6", "bars are counted on the song's timeline");
    assert_eq!(strings(&read["chords"]), ["3: Dm7", "4: G7", "5-6: Cmaj7"], "the same chord for two bars is one line");
    assert_eq!(read["key"]["estimate"], "C major");
    assert_eq!((read["pitch_range"].as_str(), read["polyphony"].as_u64()), (Some("G2-F#5"), Some(5)));
    assert_eq!(read["outside_key"], json!({ "key": "C major", "count": 1, "notes": ["F#5 at 5.3"] }));
    // Checked against the key the writer meant.
    let meant = ok(client, "analyze_music", json!({ "clip": clip, "key": "G major", "start": 16 })).await;
    assert_eq!((meant["key"]["given"].as_str(), meant["bars"].as_str()), (Some("G major"), Some("5-6")));
    assert!(meant.get("outside_key").is_none(), "F# belongs to G major: {meant}");
}
