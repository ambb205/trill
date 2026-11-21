//! The wavetable synth's bank as a model meets it: found by the words a
//! model would use; and, on a machine that has it, measured beside a synth
//! whose sounds are known to be good.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::sync::Arc;
use trill_mcp::{HttpServer, serve_http};
use trill_runtime::{Runtime, RuntimeOptions};

type Client = RunningService<RoleClient, ClientConfig>;

const WAVETABLE: &str = "builtin:wavetable";

async fn connect() -> (Client, HttpServer) {
    let runtime = Arc::new(Runtime::start(RuntimeOptions::hermetic()).unwrap());
    let server = serve_http(runtime, "127.0.0.1:0".parse().unwrap()).unwrap();
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(server.url()));
    (ClientConfig::default().serve(transport).await.unwrap(), server)
}

async fn ok(client: &Client, tool: &'static str, arguments: Value) -> Value {
    let arguments = arguments.as_object().cloned().unwrap();
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .unwrap_or_else(|error| panic!("{tool}: protocol error {error}"));
    let text = result.content.first().and_then(|block| block.as_text()).map(|t| t.text.clone()).unwrap_or_default();
    assert_ne!(result.is_error, Some(true), "{tool} failed: {text}");
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("{tool} replied with non-JSON: {text}"))
}

/// How many of a search's first answers a fitting sound has to be among.
const FIRST: usize = 5;

/// What a model asks for, and the sounds of the bank that are a fitting
/// answer: any of them among the first few will do. A kind in capitals is
/// any sound of that kind.
const ASKED: &[(&str, &[&str])] = &[
    ("808 sub", &["808 Sub", "Deep Sub"]),
    ("sub bass", &["Deep Sub", "808 Sub", "House Bass"]),
    ("reese", &["Reese"]),
    ("acid bass", &["Acid Line"]),
    ("wobble bass", &["Wobble Bass", "Growl Bass"]),
    ("dubstep bass", &["Wobble Bass", "Growl Bass", "Vowel Bass", "Metal Bass"]),
    ("drum and bass", &["Reese", "Neuro Bass"]),
    ("techno rumble", &["Rumble"]),
    ("house bass", &["House Bass", "Organ Bass", "FM Bass"]),
    ("synthwave bass", &["Pluck Bass", "Pulse Bass"]),
    ("bass guitar", &["Electric Bass"]),
    ("supersaw lead", &["Supersaw Lead", "Anthem Saws"]),
    ("trance lead", &["Supersaw Lead", "Anthem Saws", "Fifth Lead"]),
    ("vocal lead", &["Talk Lead"]),
    ("chiptune", &["Square Lead"]),
    ("hoover", &["Hoover"]),
    ("pluck", &["PLUCK"]),
    ("chord stab", &["House Stab", "Dub Chord"]),
    ("dub techno", &["Dub Chord"]),
    ("future bass chords", &["Future Pluck"]),
    ("arpeggio", &["Arp Pluck"]),
    ("warm pad", &["Warm Pad"]),
    ("dark pad", &["Dark Pad"]),
    ("ambient pad", &["Air Pad", "Glass Pad"]),
    ("choir", &["Choir"]),
    ("electric piano", &["Tine Piano", "FM Keys", "Reed Piano", "Dusty Keys"]),
    ("lofi keys", &["Dusty Keys"]),
    ("piano", &["Bright Piano", "Tine Piano", "Reed Piano"]),
    ("house organ", &["House Organ", "Organ Bass"]),
    ("glass bell", &["Glass Bell"]),
    ("bell", &["BELL"]),
    ("marimba", &["Marimba"]),
    ("strings", &["STRINGS", "PWM Strings", "Pizzicato"]),
    ("brass stab", &["Brass Stab"]),
    ("synth brass", &["Synth Brass"]),
    ("drone", &["Drone"]),
    ("riser", &["Riser", "Pitch Riser"]),
    ("impact", &["Impact"]),
    ("noise sweep", &["Sweep", "Riser", "Fall"]),
    ("sub drop", &["Sub Drop"]),
    ("siren", &["Siren"]),
];

#[tokio::test(flavor = "multi_thread")]
async fn the_words_a_model_asks_with_find_a_fitting_sound_of_the_bank_among_the_first_answers() {
    let (client, _server) = connect().await;
    let mut missed = Vec::new();
    for (asked, fitting) in ASKED {
        let found = ok(&client, "search_presets", json!({ "query": asked, "limit": FIRST })).await;
        let items = found["items"].as_array().unwrap();
        let fits = |item: &Value| {
            item["device"] == WAVETABLE
                && fitting.iter().any(|wanted| {
                    item["preset"] == *wanted
                        || item["category"].as_str().is_some_and(|kind| kind.to_uppercase() == *wanted)
                })
        };
        if !items.iter().any(fits) {
            let got: Vec<String> = items
                .iter()
                .map(|item| {
                    format!("{} of {}", item["preset"].as_str().unwrap_or("?"), item["device"].as_str().unwrap_or("?"))
                })
                .collect();
            missed.push(format!("'{asked}' wants one of {fitting:?} and finds {got:?}"));
        }
    }
    assert!(missed.is_empty(), "{missed:#?}");

    // What is found is ready to give: a track made from the first answer
    // has the sound, with the effects it brings.
    let found = ok(&client, "search_presets", json!({ "query": "warm pad", "device": "wavetable" })).await;
    let first = &found["items"][0];
    assert_eq!((first["preset"].as_str(), first["category"].as_str()), (Some("Warm Pad"), Some("Pad")));
    assert_eq!(first["with"], json!(["Chorus", "Reverb"]));
    let made = json!({ "name": "Pad", "instrument": first["device"], "preset": first["preset"] });
    let made = ok(&client, "create_track", made).await;
    assert_eq!(made["sound"]["made"].as_array().map(Vec::len), Some(2), "{made}");
    // A sound says the keys it is for where a model reads what it is.
    let said = ok(&client, "describe_device", json!({ "uri": WAVETABLE, "preset_query": "808 sub" })).await;
    let description = said["presets"]["items"][0]["description"].as_str().unwrap();
    assert!(description.starts_with("The long 808 of trap and hip hop"), "{description}");
}

// ---- beside a synth whose sounds are known to be good ----------------------------

/// Serum 2, as a VST3 plugin.
const SERUM: &str = "vst3:56534558667350736572756D20320000";
/// How many of its presets of a kind are measured, at the most.
const SPREAD: usize = 10;

/// The kinds the bank files its sounds under, each with the places Serum
/// files the same kind of sound.
const KINDS: &[(&str, &[&str])] = &[
    ("Bass", &["Factory/Bass"]),
    ("Lead", &["Factory/Lead"]),
    ("Pluck", &["Factory/Pluck"]),
    ("Pad", &["Factory/Pad"]),
    ("Keys", &["Factory/Keyboard", "Factory/E Piano", "Factory/Organ"]),
    ("Bell", &["Factory/Bell", "Factory/Mallet"]),
    ("Strings", &["Factory/String"]),
    ("Brass", &["Factory/Brass"]),
    ("Texture", &["Factory/Soundscape"]),
    ("FX", &["Factory/SFX"]),
];

/// Not a test of Trill: the ranges `trill-engine/tests/bank.rs` holds the
/// bank to. A spread of Serum 2's factory presets of each kind (the ones
/// that are a synthesized sound: none that plays samples, and none that
/// plays a pattern of its own), each played the notes the bank's sounds of
/// that kind are played and measured by the same rule: how loud, where its
/// energy lies, how wide, how fast a note begins and ends. What is played
/// rings out inside the song (`Playing::ring`): the plugin is silenced
/// when a song stops. Prints each kind's least and most, as the lines
/// that file takes.
///
/// Measurements only: each preset is loaded into the plugin, played and
/// let go of. Nothing of it is read, kept or written anywhere.
#[test]
#[ignore = "needs Serum 2 (VST3) with its factory presets; run with `cargo test -p trill-mcp --test bank serum -- --ignored --nocapture` (about ten minutes; check that 8 GB are free first)"]
fn serum_2s_presets_measured_by_kind() {
    use trill_core::device::{CompositeFactory, DeviceFactory};
    use trill_core::presets::PresetInfo;
    use trill_core::{ChainRef, Note};
    use trill_engine::character::{Character, Onset, character, onset, playing};
    use trill_engine::{AudioBackend, RenderOptions, Session};
    use trill_plugins::{ClapConfig, PluginHost, Scanner, Vst3Config};

    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("mcp-bank");
    let clap =
        ClapConfig { cache_path: Some(scratch.join("clap.json")), scanner: Scanner::Auto, ..ClapConfig::default() };
    let vst3 = Vst3Config {
        cache_path: Some(scratch.join("vst3.json")),
        scanner: Scanner::Auto,
        preset_paths: vec![scratch.join("vst3 presets")],
        ..Vst3Config::default()
    };
    let host = PluginHost::with_configs(clap, vst3).unwrap();
    if !host.vst3().descriptors().iter().any(|found| found.uri == SERUM) {
        eprintln!("skipping: Serum 2 is not installed on this machine");
        return;
    }
    let listed = host.vst3().presets_within(SERUM, std::time::Duration::from_secs(120)).unwrap_or_default();
    let mut factories: Vec<Box<dyn DeviceFactory>> = vec![Box::new(trill_dsp::BuiltinFactory)];
    factories.extend(host.factories());
    let factory: Arc<dyn DeviceFactory> = Arc::new(CompositeFactory::new(factories));
    let mut session = Session::new(factory, AudioBackend::Null { sample_rate: 48_000 }).unwrap();
    // Nothing after the synth: no limiter on the master.
    let limiter = session.project().master.devices[0].id;
    session.edit(|project| project.remove_device(limiter).map(drop)).unwrap();

    let rate = 48_000.0;
    let render = |session: &mut Session, tail: f64| {
        let options = RenderOptions { sample_rate: 48_000, max_tail_seconds: tail, ..RenderOptions::default() };
        session.render_job().render(&options).map(|out| (out.left, out.right))
    };
    // One preset: its phrase, and one note of it.
    let measure = |session: &mut Session, kind: &str, preset: &PresetInfo| -> Option<(Character, Onset)> {
        let alone = preset.tags.iter().any(|tag| tag == "mono");
        let played = playing(kind, alone);
        let track = session.edit(|project| Ok(project.add_track("Reference"))).ok()?;
        let measured = (|| {
            session.add_device(ChainRef::Track(track), SERUM, Some(&preset.id), None).ok()?;
            session.edit(|project| project.set_tempo(played.tempo)).ok()?;
            let clip = session
                .edit(|project| {
                    let clip = project.add_midi_clip(track, "Played", 0.0, played.beats + played.ring)?;
                    project.clip_mut(clip)?.add_notes(&played.notes)?;
                    Ok(clip)
                })
                .ok()?;
            let (left, right) = render(session, 0.0).ok()?;
            let phrase = character(&left, &right, rate);
            let one =
                [Note { pitch: played.single, start: 0.0, length: played.held, velocity: 100, ..Default::default() }];
            session
                .edit(|project| {
                    project.remove_clip(clip)?;
                    let clip = project.add_midi_clip(track, "One", 0.0, played.held + played.ring)?;
                    project.clip_mut(clip)?.add_notes(&one)
                })
                .ok()?;
            let (left, right) = render(session, 0.0).ok()?;
            let off = ((played.held * 60.0 / played.tempo * rate) as usize).min(left.len());
            Some((phrase, onset(&left, &right, rate, 0, off)))
        })();
        // The plugin is let go of before the next one is made.
        let _ = session.edit(|project| project.remove_track(track).map(drop));
        measured
    };

    println!(
        "kind      preset                                   level   centre   <250    >4k  width  low width  attack release"
    );
    let mut lines = Vec::new();
    for (kind, places) in KINDS {
        // A sound the synth makes itself, played as it is struck.
        let synthesized = |preset: &&PresetInfo| {
            places.iter().any(|place| preset.category.starts_with(place))
                && !preset
                    .tags
                    .iter()
                    .any(|tag| ["multisample", "sample", "arp", "clip", "seq"].contains(&tag.as_str()))
        };
        let of_kind: Vec<&PresetInfo> = listed.presets.iter().filter(synthesized).collect();
        // A spread across the list, which is in the order of its folders.
        let every = (of_kind.len() as f64 / SPREAD as f64).max(1.0);
        let chosen: Vec<&PresetInfo> =
            (0..SPREAD.min(of_kind.len())).map(|index| of_kind[(index as f64 * every) as usize]).collect();
        let mut found: Vec<(Character, Onset)> = Vec::new();
        for preset in chosen {
            let Some((phrase, note)) = measure(&mut session, kind, preset) else {
                println!("{kind:<9} {:<40} could not be played", preset.name);
                continue;
            };
            if phrase.level_db < -60.0 {
                println!("{kind:<9} {:<40} is silent on these notes", preset.name);
                continue;
            }
            println!(
                "{kind:<9} {:<40} {:>6.1} {:>8.0} {:>6.1} {:>6.1} {:>6.1} {:>10.1} {:>7.3} {:>7.2}",
                preset.name,
                phrase.level_db,
                phrase.centre_hz,
                phrase.low_db,
                phrase.high_db,
                phrase.width_db,
                phrase.low_width_db,
                note.attack_s,
                note.release_s
            );
            found.push((phrase, note));
        }
        let span = |number: &dyn Fn(&(Character, Onset)) -> f32| {
            found
                .iter()
                .map(number)
                .fold((f32::MAX, f32::MIN), |(least, most), value| (least.min(value), most.max(value)))
        };
        let (level, centre) = (span(&|f| f.0.level_db), span(&|f| f.0.centre_hz));
        let (low, high, width) = (span(&|f| f.0.low_db), span(&|f| f.0.high_db), span(&|f| f.0.width_db));
        let (attack, release) = (span(&|f| f.1.attack_s), span(&|f| f.1.release_s));
        lines.push(format!(
            "    Reference {{ kind: \"{kind}\", presets: {}, level_db: ({:.1}, {:.1}), centre_hz: ({:.0}.0, {:.0}.0), \
             low_db: ({:.1}, {:.1}), high_db: ({:.1}, {:.1}), width_db: ({:.1}, {:.1}), attack_s: ({:.3}, {:.3}), \
             release_s: ({:.2}, {:.2}) }},",
            found.len(),
            level.0,
            level.1,
            centre.0.floor(),
            centre.1.ceil(),
            low.0,
            low.1,
            high.0,
            high.1,
            width.0,
            width.1,
            attack.0,
            attack.1,
            release.0,
            release.1
        ));
    }
    println!("\nThe ranges, as `REFERENCE` in trill-engine/tests/bank.rs takes them:");
    for line in lines {
        println!("{line}");
    }
}
