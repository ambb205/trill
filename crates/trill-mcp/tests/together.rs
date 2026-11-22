//! Two servers, each with a client of its own, joined in one song over
//! this machine's loopback and driven by tools alone: what an AI client
//! has of working in a song with other people.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Runtime, RuntimeOptions};

type Client = RunningService<RoleClient, ClientConfig>;

struct Program {
    client: Client,
    runtime: Arc<Runtime>,
    _server: HttpServer,
}

async fn program() -> Program {
    // Nothing of the user's is read or written: no settings, no library.
    let runtime = Arc::new(Runtime::start(RuntimeOptions::hermetic()).unwrap());
    let server = serve_http(runtime.clone(), "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    let client = ClientConfig::default().serve(transport).await.unwrap();
    Program { client, runtime, _server: server }
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

/// Asks `tool` of `client` until `done` says the reply is what is waited
/// for: what the other program did takes a moment to arrive.
async fn until(
    client: &Client,
    tool: &'static str,
    arguments: Value,
    what: &str,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        // (What is asked about may not be there yet: a refusal is "not yet".)
        let reply = call(client, tool, arguments.clone()).await;
        if let Ok(reply) = &reply
            && done(reply)
        {
            return reply.clone();
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}; the last reply was {reply:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn names(reply: &Value) -> Vec<String> {
    let here = reply["here"].as_array().cloned().unwrap_or_default();
    here.iter().map(|person| person["name"].as_str().unwrap_or_default().to_owned()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_share_a_song_by_tools_and_each_has_what_the_other_writes() {
    let (ada, ben) = (program().await, program().await);
    // Ada's client writes a little song and shares it.
    let arguments = json!({ "name": "Keys", "instrument": "builtin:polysynth" });
    let keys = ok(&ada.client, "create_track", arguments).await["track_id"].clone();
    let notes = json!([{ "pitch": 60, "start": 0, "length": 1 }, { "pitch": 64, "start": 1, "length": 1 }]);
    let arguments = json!({ "track": keys, "name": "Verse", "start": 0, "length": 8, "notes": notes });
    let verse = ok(&ada.client, "create_clip", arguments).await["clip_id"].clone();

    let alone = ok(&ada.client, "shared_song", json!({ "action": "status" })).await;
    assert_eq!((alone["shared"].clone(), alone["here"].clone()), (json!(false), json!([])), "{alone}");
    let shared = ok(&ada.client, "shared_song", json!({ "action": "share", "name": "Ada" })).await;
    let invitation = shared["invitation"].as_str().expect("an invitation to hand to someone").to_owned();
    assert!(
        invitation.starts_with("trill:127.0.0.1:"),
        "on this machine, unless the network is asked for: {invitation}"
    );
    assert_eq!(shared["shared"], json!(true));

    // Ben's client joins by it, and has the song.
    let joined =
        ok(&ben.client, "shared_song", json!({ "action": "join", "invitation": invitation, "name": "Ben" })).await;
    assert_eq!(names(&joined), ["Ada"], "{joined}");
    let song = ok(&ben.client, "get_project", json!({})).await;
    assert_eq!(song["tracks"][0]["name"], "Keys", "{song}");
    let here = until(&ada.client, "shared_song", json!({ "action": "status" }), "Ada sees Ben", |reply| {
        names(reply) == ["Ben"]
    })
    .await;
    assert_ne!(here["me"]["colour"], here["here"][0]["colour"], "{here}");

    // Both write at once, into the same clip and elsewhere.
    let more = json!({ "clip": verse, "notes": [{ "pitch": 67, "start": 2, "length": 1 }] });
    let (from_ada, from_ben) = tokio::join!(
        ok(&ada.client, "add_notes", more),
        ok(&ben.client, "add_notes", json!({ "clip": verse, "notes": [{ "pitch": 48, "start": 0, "length": 4 }] })),
    );
    assert!(from_ada.is_object() && from_ben.is_object());
    ok(&ben.client, "set_tempo", json!({ "bpm": 98 })).await;
    let arguments = json!({ "name": "Lead", "instrument": "builtin:polysynth" });
    let lead = ok(&ada.client, "create_track", arguments).await["track_id"].clone();
    assert!(lead.as_u64().unwrap() < 1 << 53, "an id made in a shared song is exact as a number in JSON: {lead}");
    for client in [&ada.client, &ben.client] {
        let all = |reply: &Value| reply["notes"].as_array().is_some_and(|notes| notes.len() == 4);
        until(client, "get_notes", json!({ "clip": verse }), "all four notes are there", all).await;
        let whole = |reply: &Value| {
            reply["tempo"] == json!(98.0) && reply["tracks"].as_array().is_some_and(|tracks| tracks.len() == 2)
        };
        until(client, "get_project", json!({}), "the tempo and the track are there", whole).await;
    }

    // Each undoes their own: Ben takes back his tempo, and Ada's track stays.
    let undone = ok(&ben.client, "history", json!({ "action": "undo" })).await;
    assert_eq!(undone["steps_applied"], 1);
    let back = |reply: &Value| {
        reply["tempo"] == json!(120.0) && reply["tracks"].as_array().is_some_and(|tracks| tracks.len() == 2)
    };
    until(&ada.client, "get_project", json!({}), "the tempo is back and the track is still there", back).await;

    // A clash is a notice with the way back: Ada removes the track Ben is
    // writing in at the same moment.
    let (_, written) = tokio::join!(
        ok(&ada.client, "delete_track", json!({ "track": keys })),
        call(&ben.client, "add_notes", json!({ "clip": verse, "notes": [{ "pitch": 69, "start": 5, "length": 1 }] })),
    );
    let told = |reply: &Value| reply["notices"].as_array().is_some_and(|notices| !notices.is_empty());
    let status = match written {
        // Ben wrote before he heard of it: he is told, and can bring it back.
        Ok(_) => until(&ben.client, "shared_song", json!({ "action": "status" }), "Ben is told", told).await,
        // He heard of it first: the clip was gone when he wrote, which the tool said.
        Err(refusal) => {
            assert!(refusal.contains("not found"), "{refusal}");
            return;
        }
    };
    let notice = &status["notices"][0];
    assert!(notice["text"].as_str().unwrap().contains("Ada removed the track"), "{notice}");
    assert_eq!(notice["action"], "Bring it back");
    let after = ok(&ben.client, "shared_song", json!({ "action": "put_back", "notice": notice["id"] })).await;
    assert!(after.get("notices").is_none(), "{after}");
    let whole = |reply: &Value| reply["notes"].as_array().is_some_and(|notes| notes.len() == 5);
    until(&ada.client, "get_notes", json!({ "clip": verse }), "the track is back with what Ben wrote", whole).await;
    let refused = call(&ben.client, "shared_song", json!({ "action": "put_back", "notice": 999 })).await.unwrap_err();
    assert!(refused.contains("there are no notices"), "{refused}");

    // Ben leaves; both have the song, and Ada is alone again.
    let left = ok(&ben.client, "shared_song", json!({ "action": "leave" })).await;
    assert_eq!((left["connected_programs"].clone(), left["here"].clone()), (json!(0), json!([])), "{left}");
    assert_eq!(left["shared"], json!(true), "it is still a song that merges");
    until(&ada.client, "shared_song", json!({ "action": "status" }), "Ada sees him go", |reply| {
        names(reply).is_empty()
    })
    .await;
    let lately = ok(&ada.client, "shared_song", json!({ "action": "status" })).await["lately"].to_string();
    assert!(lately.contains("Ben is here.") && lately.contains("Ben left."), "{lately}");
    drop((ada.runtime, ben.runtime));
}

/// Writes `seconds` of a tone and returns the file's path as text.
fn tone(path: std::path::PathBuf, hertz: f64, seconds: f64) -> String {
    let rate = 44_100.0;
    let channel: Vec<f32> = (0..(seconds * rate) as usize)
        .map(|n| (n as f64 * hertz * std::f64::consts::TAU / rate).sin() as f32 * 0.5)
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let buffer = trill_core::audio::AudioBuffer { sample_rate: rate, left: channel.clone(), right: channel };
    trill_media::write_wav(&path, &buffer, trill_media::WavFormat::Int16).unwrap();
    path.to_string_lossy().into_owned()
}

/// 1 connection.
#[tokio::test(flavor = "multi_thread")]
async fn the_audio_of_a_shared_song_is_said_before_it_goes_while_it_arrives_and_where_it_is_held() {
    let folder = tempfile::tempdir().unwrap();
    let (ada, ben) = (program().await, program().await);
    let vocal = tone(folder.path().join("Ada's music").join("vocal take.wav"), 220.0, 1.0);
    let track = ok(&ada.client, "create_track", json!({ "name": "Vocal" })).await["track_id"].clone();
    let arguments = json!({ "track": track, "path": vocal, "start": 0, "name": "Verse vocal" });
    let verse = ok(&ada.client, "create_audio_clip", arguments).await["clip_id"].clone();

    // Before it is shared: what would go, and from where.
    let goes = ok(&ada.client, "shared_song", json!({ "action": "what_goes" })).await;
    assert_eq!(goes["files"], 1, "{goes}");
    assert_eq!(goes["outside_the_songs_folders"], json!([vocal]), "a file outside the song's folders, by name");
    assert!(goes["says"].as_str().unwrap().starts_with("1 file ("), "{goes}");
    assert!(!ada.runtime.together().is_on(), "asking shares nothing");
    // The reply to `share` says it again.
    let shared = ok(&ada.client, "shared_song", json!({ "action": "share", "name": "Ada" })).await;
    assert_eq!(shared["audio_files"]["files"], 1, "{shared}");
    let invitation = shared["invitation"].as_str().unwrap().to_owned();

    // Ben joins: the recording arrives, and is at no time a missing file.
    ok(&ben.client, "shared_song", json!({ "action": "join", "invitation": invitation, "name": "Ben" })).await;
    let missing = ok(&ben.client, "missing_files", json!({})).await;
    assert_eq!(missing["missing"], 0, "a file on its way is not a missing file: {missing}");
    let deadline = Instant::now() + Duration::from_secs(30);
    let whole = |runtime: &Runtime| {
        let files = runtime.session.lock().files_here();
        files.len() == 1 && std::fs::read(&files[0].path).ok() == std::fs::read(&vocal).ok()
    };
    while !whole(&ben.runtime) {
        assert!(Instant::now() < deadline, "the recording never arrived");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let here = |reply: &Value| reply.get("arriving").is_none() && reply["connected_programs"] == 1;
    until(&ben.client, "shared_song", json!({ "action": "status" }), "nothing is on its way any more", here).await;
    let at_ben = ben.runtime.session.lock().files_here();
    assert_eq!(at_ben.len(), 1);
    assert_eq!(std::fs::read(&at_ben[0].path).unwrap(), std::fs::read(&vocal).unwrap(), "the same bytes, at Ben's");
    assert!(!at_ben[0].path.contains("Ada's music"), "under a place of his own: {}", at_ben[0].path);
    let song = ok(&ben.client, "get_project", json!({})).await.to_string();
    assert!(!song.contains("Ada's music"), "and nothing he is told names her folder: {song}");

    // Something that is not the user's hand names a file in the shared
    // song: it is held, said, and goes when the user lets it.
    let demo = tone(folder.path().join("Ada's music").join("demo.wav"), 330.0, 0.5);
    let held = {
        let recording = trill_core::project::AudioContent::new(demo.clone());
        let track = trill_core::TrackId(track.as_u64().unwrap());
        ada.runtime.session.lock().edit(|song| song.add_audio_clip(track, "Demo", 8.0, 1.0, recording)).unwrap()
    };
    let is_held = |reply: &Value| reply["held"]["files"] == json!([demo]);
    let status = until(&ada.client, "shared_song", json!({ "action": "status" }), "it is held", is_held).await;
    assert!(status["held"]["why"].as_str().unwrap().contains("nobody was shown them"), "{status}");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let named = |runtime: &Runtime, clip: trill_core::ClipId| {
        let files = runtime.session.lock().files_here();
        files.into_iter().find(|file| file.place == trill_core::FilePlace::Clip(clip)).map(|file| file.path)
    };
    assert_eq!(named(&ben.runtime, held), None, "nothing of it has gone to Ben");
    let after = ok(&ada.client, "shared_song", json!({ "action": "let_files_go" })).await;
    assert!(after.get("held").is_none(), "{after}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while named(&ben.runtime, held).is_none_or(|path| std::fs::read(&path).ok() != std::fs::read(&demo).ok()) {
        assert!(Instant::now() < deadline, "the file that was let go never arrived");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // A file that is pointed at is taken only where it is the song's file.
    let relink = |path: &str| json!({ "files": [{ "clip": verse, "to": path }] });
    let refused = call(&ben.client, "relink_files", relink(&demo)).await.unwrap_err();
    assert!(refused.contains("is not the file the song names as 'vocal take.wav'"), "{refused}");
    let own = folder.path().join("Ben's packs").join("the same take.wav");
    std::fs::create_dir_all(own.parent().unwrap()).unwrap();
    std::fs::copy(&vocal, &own).unwrap();
    let taken = ok(&ben.client, "relink_files", relink(&own.to_string_lossy())).await;
    assert_eq!(taken["relinked"]["places"], 1, "{taken}");
    let verse = trill_core::ClipId(verse.as_u64().unwrap());
    assert_eq!(named(&ben.runtime, verse), Some(own.to_string_lossy().into_owned()));
    assert_eq!(named(&ada.runtime, verse), Some(vocal.clone()), "and Ada's place for it is as it was");
}

#[tokio::test(flavor = "multi_thread")]
async fn what_goes_wrong_with_sharing_and_joining_is_a_sentence_that_says_what_to_do() {
    let (ada, ben) = (program().await, program().await);
    let refused = call(&ben.client, "shared_song", json!({ "action": "join" })).await.unwrap_err();
    assert!(refused.contains("trill:HOST:PORT:SECRET"), "{refused}");
    let refused =
        call(&ben.client, "shared_song", json!({ "action": "join", "invitation": "trill:nowhere" })).await.unwrap_err();
    assert!(refused.contains("trill:HOST:PORT:SECRET"), "{refused}");
    let refused =
        call(&ada.client, "shared_song", json!({ "action": "share", "reach": "everywhere" })).await.unwrap_err();
    assert!(refused.contains("`this_machine`, `network` or `meeting_point`"), "{refused}");

    let invitation =
        ok(&ada.client, "shared_song", json!({ "action": "share" })).await["invitation"].as_str().unwrap().to_owned();
    // A song with changes that are not saved is not replaced unless that
    // is said.
    ok(&ben.client, "create_track", json!({ "name": "Ben's own" })).await;
    let refused =
        call(&ben.client, "shared_song", json!({ "action": "join", "invitation": invitation })).await.unwrap_err();
    assert!(refused.contains("changes that are not saved"), "{refused}");
    assert_eq!(ok(&ben.client, "get_project", json!({})).await["tracks"][0]["name"], "Ben's own");
    let joined = call(
        &ben.client,
        "shared_song",
        json!({ "action": "join", "invitation": invitation, "replace_unsaved": true }),
    )
    .await;
    assert!(joined.is_ok(), "{joined:?}");
    // Once the invitation is taken back it is no longer one.
    ok(&ben.client, "shared_song", json!({ "action": "leave" })).await;
    let stopped = ok(&ada.client, "shared_song", json!({ "action": "stop_sharing" })).await;
    assert!(stopped.get("invitation").is_none(), "{stopped}");
    let refused =
        call(&ben.client, "shared_song", json!({ "action": "join", "invitation": invitation })).await.unwrap_err();
    assert!(refused.starts_with("Not joined"), "{refused}");
}

/// 3 connections.
#[tokio::test(flavor = "multi_thread")]
async fn a_song_is_shared_through_a_meeting_point_by_tools_and_no_tool_takes_or_says_its_key() {
    let (ada, ben) = (program().await, program().await);
    ok(&ada.client, "create_track", json!({ "name": "Keys", "instrument": "builtin:polysynth" })).await;
    // No meeting point is set: said, with whose it is to set one.
    let share = json!({ "action": "share", "reach": "meeting_point", "name": "Ada" });
    let refused = call(&ada.client, "shared_song", share.clone()).await.unwrap_err();
    assert!(refused.contains("No meeting point is set") && refused.contains("no tool takes them"), "{refused}");

    // A meeting point on this machine, and the key its keeper gave Ada:
    // set as the window sets it. (No tool does: there is no argument a key
    // could be handed over in.)
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let at = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let key = trill_meet::Key::new().unwrap();
    let keys = trill_meet::Keys::of(vec![trill_meet::keys::Given::of(&key, "Ada")]);
    let log: trill_meet::Log = Arc::new(|_: &str| {});
    let _point = trill_meet::Server::start(vec![listener], keys, trill_meet::Limits::default(), log).unwrap();
    ada.runtime.set_meeting_point(&at, Some(&key.to_string())).unwrap();

    // Shared through it: the reply has the invitation, which carries the
    // meeting point's address, and says that the meeting is open there.
    let shared = ok(&ada.client, "shared_song", share).await;
    let invitation = shared["invitation"].as_str().unwrap().to_owned();
    assert!(invitation.starts_with(&format!("trill:meet:{at}:")), "{shared}");
    assert_eq!(shared["meeting_point"], json!({ "at": at, "open": true }), "{shared}");
    // Whoever is handed the invitation joins by it and sets nothing up.
    let joined =
        ok(&ben.client, "shared_song", json!({ "action": "join", "invitation": invitation, "name": "Ben" })).await;
    assert_eq!(names(&joined), ["Ada"], "{joined}");
    assert!(joined.get("meeting_point").is_none(), "{joined}");
    ok(&ben.client, "create_track", json!({ "name": "Bass" })).await;
    until(&ada.client, "get_project", json!({}), "Ada has Ben's track", |reply| {
        reply["tracks"].as_array().is_some_and(|tracks| tracks.iter().any(|track| track["name"] == "Bass"))
    })
    .await;

    // Nothing a tool says has the key: not how things stand, not the
    // song, not the journal of the calls.
    let key = key.to_string();
    for (tool, arguments) in
        [("shared_song", json!({ "action": "status" })), ("get_project", json!({})), ("get_journal", json!({}))]
    {
        let reply = ok(&ada.client, tool, arguments).await.to_string();
        assert!(!reply.contains(&key) && !reply.contains(&key[10..]), "{tool} says the key: {reply}");
    }
    // A wrong key: sharing ends in the sentence that says so.
    ok(&ben.client, "shared_song", json!({ "action": "leave" })).await;
    ok(&ada.client, "shared_song", json!({ "action": "leave" })).await;
    ada.runtime.set_meeting_point(&at, Some(&trill_meet::Key::new().unwrap().to_string())).unwrap();
    let share = json!({ "action": "share", "reach": "meeting_point" });
    let refused = call(&ada.client, "shared_song", share).await.unwrap_err();
    assert!(refused.contains("Not shared through the meeting point") && refused.contains("did not take the key"));
    assert!(!refused.contains(&key[10..]), "{refused}");
    drop((ada.runtime, ben.runtime));
}
