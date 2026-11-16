//! Canned conversations, for the pictures of the panel.
//!
//! A screenshot must look the same on every machine and must not spend
//! anybody's usage, so it cannot come from a live run. A scene plays a
//! script through the same path a live turn takes: the message is sent to a
//! scripted back end, the events come back through the panel's own
//! handling, and where the script says the assistant changed the song, the
//! song is really changed, inside the turn's undo group, and the call
//! answers with what it really made. What the picture shows is therefore
//! what the panel does, not a drawing of it: the reply set from its
//! Markdown, and the window gone to what the turn changed.
//!
//! The scripts are written for `examples/first-light.trill`. On another
//! song they still play, with whatever track comes first standing in.

use super::Assistant;
use super::backend::{Problem, Script};
use super::context;
use crate::ui::snapshot::{Live, Snapshot};
use crate::ui::state::UiState;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use trill_assistant::{
    CliInstallation, CliVersion, Effort, Event, FailureReason, Notice, PlanChange, Progress, SessionId, StepStatus,
    TokenUsage, TurnStart, TurnSummary, Unavailable, WebLookup,
};
use trill_core::{ClipId, Note, Project, TrackId};

/// A canned state of the panel.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Scene {
    /// Ready, with nothing said yet.
    Ready,
    /// In the middle of a turn: text, tool calls done and one under way.
    Working,
    /// A finished turn that changed the song.
    Answered,
    /// A finished turn that changed nothing and answered at length: every
    /// kind of thing a reply is set as.
    Markdown,
    /// After an answer, with the list of models open.
    Model,
    /// After an answer, with the list of thinking efforts open.
    Effort,
    /// In the middle of a whole song: a plan partly ticked off, something
    /// looked up on the web, and well past a hundred steps.
    Song,
    /// The whole song finished: the plan done, what it took, and a
    /// conversation that had to be summarised on the way.
    SongDone,
    /// A turn that ran into the usage limit, after a tool call that failed.
    Limit,
    /// A conversation billed to an API key.
    ApiKey,
    /// The CLI is installed and nobody is signed in.
    SignedOut,
    /// There is no CLI.
    NotInstalled,
}

impl Scene {
    /// Every scene by the name the command line knows it by.
    pub const ALL: [(&'static str, Scene); 12] = [
        ("ready", Scene::Ready),
        ("working", Scene::Working),
        ("answered", Scene::Answered),
        ("markdown", Scene::Markdown),
        ("model", Scene::Model),
        ("effort", Scene::Effort),
        ("song", Scene::Song),
        ("song-done", Scene::SongDone),
        ("limit", Scene::Limit),
        ("api-key", Scene::ApiKey),
        ("signed-out", Scene::SignedOut),
        ("not-installed", Scene::NotInstalled),
    ];

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(known, _)| *known == name).map(|(_, scene)| *scene)
    }

    /// The back end the scene plays against.
    pub(super) fn script(self) -> Script {
        let installation = CliInstallation { path: PathBuf::from("claude"), version: CliVersion::new(2, 1, 325) };
        match self {
            Scene::SignedOut => Script::unavailable(Problem::Cli(Unavailable::NotLoggedIn { installation })),
            Scene::NotInstalled => Script::unavailable(Problem::Cli(Unavailable::NotInstalled)),
            _ => Script::ready(),
        }
    }

    /// Plays the scene: sends its message and has the script answer.
    pub(super) fn play(self, assistant: &mut Assistant, snapshot: &Snapshot, state: &UiState, live: &Live) {
        let Some(script) = assistant.script.clone() else { return };
        let project = &snapshot.project;
        let context = context::describe(project, state, live);
        let track = |name: &str| {
            let named = project.tracks.iter().find(|track| track.name == name);
            named.or(project.tracks.first()).map(|track| track.id)
        };
        let clip = |name: &str| {
            project.tracks.iter().flat_map(|track| &track.clips).find(|clip| clip.name == name).map(|clip| clip.id)
        };
        let id = |thing: Option<u64>| thing.map_or(Value::Null, Value::from);

        match self {
            Scene::Ready | Scene::SignedOut | Scene::NotInstalled => {}
            Scene::Answered | Scene::ApiKey => {
                let (pad, bass) = (track("Pad"), track("Bass"));
                if !assistant.submit(
                    "The bass sits out the first eight bars. Give it a simple line there that follows the pad.",
                    &context,
                ) {
                    return;
                }
                let write = json!({
                    "target": { "track": id(bass.map(|track| track.0)), "start": 0, "name": "Bass intro", "bars": 8 },
                    "progression": "Am | F | C | G",
                    "style": "roots",
                });
                // As the tool answers: with the clip it made.
                let written = bass.and_then(|bass| write_bass_intro(assistant, bass));
                let wrote = match written {
                    Some(clip) => {
                        format!("{{ \"clip_id\": {}, \"created\": true, \"bars\": 8, \"note_count\": 64 }}", clip.0)
                    }
                    None => "{ \"note_count\": 0 }".to_owned(),
                };
                script.say([
                    started(self == Scene::ApiKey),
                    call("1", "get_project", json!({})),
                    answer("1", "get_project", Ok("{ \"name\": \"First Light\", \"tempo\": 96, \"tracks\": 7 }")),
                    call("2", "analyze_music", json!({ "track": id(pad.map(|track| track.0)), "start": 0, "end": 32 })),
                    answer(
                        "2",
                        "analyze_music",
                        Ok("{ \"key\": \"A minor\", \"chords\": [\"Am\", \"F\", \"C\", \"G\"] }"),
                    ),
                    call("3", "write_bass", write),
                    answer("3", "write_bass", Ok(&wrote)),
                    Event::Text(
                        "I added an eight-bar line to **Bass** at bar 1. It follows the pad:\n\n- the root of each \
                         chord, in eighths\n- **A**, **F**, **C** and **G**, two bars each\n- always below the \
                         pad\u{2019}s lowest voice\n\nIf it crowds the intro, halving it to quarter notes would leave \
                         more room."
                            .to_owned(),
                    ),
                    Event::Finished(TurnSummary {
                        turns: 4,
                        duration: Duration::from_millis(21_400),
                        usage: TokenUsage { input: 5200, output: 410, cache_read: 0, cache_write: 0 },
                        estimated_cost_usd: Some(0.0312),
                    }),
                ]);
            }
            Scene::Markdown | Scene::Model | Scene::Effort => {
                if self == Scene::Model {
                    // Chosen before anything is said, so nothing is said of it.
                    let _ = assistant.choose_model(Some("sonnet"));
                    assistant.open_model_menu = true;
                }
                if self == Scene::Effort {
                    let _ = assistant.choose_model(Some("opus"));
                    let _ = assistant.choose_effort(Some(Effort::High));
                    assistant.open_effort_menu = true;
                }
                let question = match self {
                    Scene::Model | Scene::Effort => "What key is this in?",
                    _ => "How would you build up to the drop at bar 17?",
                };
                if !assistant.submit(question, &context) {
                    return;
                }
                let said = match self {
                    Scene::Model | Scene::Effort => {
                        "**A minor.** The pad moves through Am, F, C and G, and nothing in the song leaves the key."
                    }
                    _ => {
                        "## Three moves\n\n1. **Clear the low end** for two bars: mute `Bass` from bar 15.\n2. **Build with the \
                         hats**: sixteenths that open up over the last bar.\n3. **Land it** with a crash on the downbeat \
                         of bar 17.\n\nA fill for bar 16 that suits 96 BPM:\n\n```\nbeat    1 . . . 2 . . . 3 . . . 4 . . .\nsnare   . \
                         . . . x . . . . . x . x x x x\n```\n\n> The pad already swells into bar 17, so the build can stay \
                         *small*.\n\nI have changed nothing yet. More on fills: [grooves and fills](https://example.com/fills)."
                    }
                };
                script.say([
                    started(false),
                    call("1", "get_project", json!({})),
                    answer("1", "get_project", Ok("{ \"name\": \"First Light\", \"tempo\": 96, \"tracks\": 7 }")),
                    Event::Text(said.to_owned()),
                    Event::Finished(TurnSummary {
                        turns: 2,
                        duration: Duration::from_millis(9_300),
                        usage: TokenUsage { input: 4100, output: 220, cache_read: 0, cache_write: 0 },
                        estimated_cost_usd: Some(0.0121),
                    }),
                ]);
            }
            Scene::Working => {
                let beat = clip("Beat A");
                if !assistant.submit(
                    "Make the hats busier from bar 9, and bring the pad down a little under the lead.",
                    &context,
                ) {
                    return;
                }
                if let Some(beat) = beat {
                    busier_hats(assistant, beat);
                }
                let hats: Vec<Value> =
                    (0..32).map(|step| json!(["closed_hat", step as f64 * 0.25 + 0.25, 0.2, 70])).collect();
                script.say([
                    started(false),
                    Event::Text("I\u{2019}ll look at the drums first.".to_owned()),
                    call("1", "get_project", json!({})),
                    answer("1", "get_project", Ok("{ \"name\": \"First Light\", \"tempo\": 96, \"tracks\": 7 }")),
                    call("2", "get_notes", json!({ "clip": id(beat.map(|clip| clip.0)) })),
                    answer("2", "get_notes", Ok("{ \"notes\": 112 }")),
                    call("3", "add_notes", json!({ "clip": id(beat.map(|clip| clip.0)), "notes": hats })),
                    answer("3", "add_notes", Ok("{ \"added\": 32 }")),
                    Event::Text("The hats now run in sixteenths through **Beat A**. Next, the pad.".to_owned()),
                    call(
                        "4",
                        "update_track",
                        json!({ "track": id(track("Pad").map(|track| track.0)), "volume_db": -9.5 }),
                    ),
                ]);
                assistant.frozen_seconds = Some(14);
            }
            Scene::Song | Scene::SongDone => {
                let bass = track("Bass");
                let asked = "Make this a whole song: about two minutes of melodic house in A minor, with a build, \
                             a drop and an outro. Look up how the genre paces a build if you need to.";
                if !assistant.submit(asked, &context) {
                    return;
                }
                let written = bass.and_then(|bass| write_bass_intro(assistant, bass));
                let wrote = match written {
                    Some(clip) => format!("{{ \"clip_id\": {}, \"created\": true, \"bars\": 8 }}", clip.0),
                    None => "{ \"note_count\": 0 }".to_owned(),
                };
                let bass_id = id(bass.map(|track| track.0));
                let drums_id = id(track("Drums").map(|track| track.0));
                let steps = [
                    ("Settle tempo, key and sections", "Settling tempo, key and sections"),
                    ("Look up how the genre paces a build", "Looking up how the genre paces a build"),
                    ("Drums for every section", "Writing the drums"),
                    ("Bass line under the chords", "Writing the bass line"),
                    ("Chords, pad and arp", "Writing chords, pad and arp"),
                    ("Lead for the drop", "Writing the lead"),
                    ("Fills, sweeps and automation", "Adding fills and automation"),
                    ("Render and read the mix", "Rendering and reading the mix"),
                ];
                let planned = steps.iter().enumerate().map(|(index, (title, doing))| {
                    Event::Plan(PlanChange::Added {
                        id: (index + 1).to_string(),
                        title: (*title).to_owned(),
                        doing: Some((*doing).to_owned()),
                    })
                });
                let mut events = vec![
                    started(false),
                    Event::Text(
                        "A two-minute track at **122 BPM** in A minor. Here is the plan; I\u{2019}ll work through it \
                         section by section."
                            .to_owned(),
                    ),
                ];
                events.extend(planned);
                events.extend([
                    stands("1", StepStatus::InProgress),
                    call("1", "get_project", json!({})),
                    answer("1", "get_project", Ok("{ \"name\": \"First Light\", \"tempo\": 96, \"tracks\": 7 }")),
                    call("2", "set_project", json!({ "tempo": 122 })),
                    answer("2", "set_project", Ok("{ \"tempo\": 122.0 }")),
                    stands("1", StepStatus::Done),
                    stands("2", StepStatus::InProgress),
                    looks_up("3", WebLookup::Search { query: "melodic house arrangement build length in bars".into() }),
                    found(
                        "3",
                        "Web search results for query: \"melodic house arrangement build length in bars\"\n\nLinks: \
                         [{\"title\":\"Arranging melodic house: sections and energy\",\"url\":\"https://example.com/melodic-house-arrangement\"}]",
                    ),
                    looks_up(
                        "4",
                        WebLookup::Page {
                            url: "https://example.com/melodic-house-arrangement".into(),
                            question: "How long is a build before the drop, and what changes in it?".into(),
                        },
                    ),
                    found(
                        "4",
                        "Builds run 8 or 16 bars. The kick often drops out for the last 2 to 4; a filter opens on the \
                         pad; snare rolls double every 2 bars.",
                    ),
                    stands("2", StepStatus::Done),
                    Event::Text(
                        "The usual shape is a **16-bar build** with the kick out for the last four bars. I\u{2019}ll \
                         use that."
                            .to_owned(),
                    ),
                    stands("3", StepStatus::InProgress),
                    call("5", "write_drums", json!({ "target": { "track": drums_id, "bars": 16 }, "groove": "house" })),
                    answer("5", "write_drums", Ok("{ \"bars\": 16, \"note_count\": 208 }")),
                    stands("3", StepStatus::Done),
                    stands("4", StepStatus::InProgress),
                    call(
                        "6",
                        "write_bass",
                        json!({ "target": { "track": bass_id, "start": 0, "bars": 8 }, "progression": "Am | F | C | G" }),
                    ),
                ]);
                if self == Scene::Song {
                    // Well into its second run of the CLI: the first has
                    // said what it cost.
                    events.push(Event::Progress(Progress {
                        steps: 118,
                        estimated_cost_usd: Some(1.42),
                        costed_steps: 100,
                    }));
                    script.say(events);
                    assistant.frozen_seconds = Some(14 * 60 + 2);
                    return;
                }
                events.push(answer("6", "write_bass", Ok(&wrote)));
                events.extend((4..=8).flat_map(|step| {
                    let id = step.to_string();
                    [stands(&id, StepStatus::InProgress), stands(&id, StepStatus::Done)]
                }));
                events.extend([
                    Event::Notice(Notice::Compacted { tokens_before: Some(986_210), tokens_after: Some(41_870) }),
                    call("7", "render", json!({ "path": "first-light-house.wav" })),
                    answer("7", "render", Ok("{ \"lufs\": -9.4, \"peak_db\": -1.0, \"seconds\": 126.0 }")),
                    Event::Text(
                        "**Done: 2:06 at 122 BPM in A minor.**\n\n- Intro (bars 1\u{2013}16), build (17\u{2013}32), drop \
                         (33\u{2013}48), break, second drop, outro\n- The build opens a filter on **Pad** and drops \
                         the kick for its last four bars\n- The render reads **\u{2212}9.4 LUFS**, peak \u{2212}1.0 dB, \
                         with the low end centred\n\nListen for whether the lead at bar 33 sits above the pad or \
                         fights it; I can only read its level, not hear it."
                            .to_owned(),
                    ),
                    Event::Finished(TurnSummary {
                        turns: 412,
                        duration: Duration::from_secs(31 * 60 + 4),
                        usage: TokenUsage { input: 5200, output: 410, cache_read: 0, cache_write: 0 },
                        estimated_cost_usd: Some(6.18),
                    }),
                ]);
                script.say(events);
            }
            Scene::Limit => {
                let lead = clip("Lead");
                if !assistant.submit("Double the lead an octave up for the last chorus.", &context) {
                    return;
                }
                script.say([
                    started(false),
                    call("1", "get_project", json!({})),
                    answer("1", "get_project", Ok("{ \"name\": \"First Light\", \"tempo\": 96, \"tracks\": 7 }")),
                    call("2", "duplicate_clip", json!({ "clip": id(lead.map(|clip| clip.0)), "track": 99 })),
                    answer(
                        "2",
                        "duplicate_clip",
                        Err("track 99 was not found. The tracks are: 2 (Pad), 11 (Keys), 19 (Bells), 26 (Bass), 33 \
                             (Pluck), 39 (Lead), 45 (Drums)."),
                    ),
                    Event::Notice(Notice::UsageLimitNear { resets_at: None }),
                    Event::Failed(FailureReason::UsageLimitReached {
                        message: "You\u{2019}ve hit your session limit \u{00B7} resets 3:00pm".to_owned(),
                        resets_at: None,
                    }),
                ]);
            }
        }
    }
}

fn started(with_api_key: bool) -> Event {
    Event::Started(TurnStart {
        session_id: SessionId::parse("5ce11e00-0000-4000-8000-00000000d0c5").expect("a well-formed id"),
        model: "claude-sonnet-5-5".to_owned(),
        api_key_source: with_api_key.then(|| "ANTHROPIC_API_KEY".to_owned()),
    })
}

/// A step of the plan coming to stand as `status`.
fn stands(id: &str, status: StepStatus) -> Event {
    Event::Plan(PlanChange::Changed { id: id.to_owned(), status: Some(status), title: None, doing: None })
}

fn looks_up(id: &str, lookup: WebLookup) -> Event {
    Event::WebStarted { id: format!("web-{id}"), lookup }
}

fn found(id: &str, answer: &str) -> Event {
    Event::WebFinished { id: format!("web-{id}"), result: Ok(answer.to_owned()) }
}

fn call(id: &str, tool: &str, input: Value) -> Event {
    Event::ToolCallStarted { id: id.to_owned(), tool: tool.to_owned(), input }
}

fn answer(id: &str, tool: &str, result: Result<&str, &str>) -> Event {
    Event::ToolCallFinished {
        id: id.to_owned(),
        tool: tool.to_owned(),
        result: result.map(str::to_owned).map_err(str::to_owned),
    }
}

/// What the script's `write_bass` call does: eight bars of roots in
/// eighths, two bars to a chord. Returns the clip it made.
fn write_bass_intro(assistant: &Assistant, bass: TrackId) -> Option<ClipId> {
    let roots = [33u8, 29, 36, 31];
    let notes: Vec<Note> = (0..64)
        .map(|step| Note { pitch: roots[step / 16], start: step as f64 * 0.5, length: 0.4, velocity: 92 })
        .collect();
    let edit = |project: &mut Project| {
        let clip = project.add_midi_clip(bass, "Bass intro", 0.0, 32.0)?;
        project.clip_mut(clip)?.add_notes(&notes)?;
        Ok(clip)
    };
    match assistant.session.lock().edit(edit) {
        Ok(clip) => Some(clip),
        Err(error) => {
            log::warn!("the scene could not write its bass line: {error}");
            None
        }
    }
}

/// What the script's `add_notes` call does: hats on the sixteenths between
/// the ones already there.
fn busier_hats(assistant: &Assistant, clip: ClipId) {
    let notes: Vec<Note> =
        (0..32).map(|step| Note { pitch: 42, start: step as f64 * 0.5 + 0.25, length: 0.2, velocity: 70 }).collect();
    if let Err(error) = assistant.session.lock().edit(|project| project.clip_mut(clip)?.add_notes(&notes)) {
        log::warn!("the scene could not write its hats: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenes_go_by_their_names() {
        for (name, scene) in Scene::ALL {
            assert_eq!(Scene::named(name), Some(scene));
        }
        assert_eq!(Scene::named("live"), None);
        let mut names: Vec<&str> = Scene::ALL.iter().map(|(name, _)| *name).collect();
        names.dedup();
        assert_eq!(names.len(), Scene::ALL.len());
    }
}
