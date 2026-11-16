//! Canned conversations, for the pictures of the panel.
//!
//! A screenshot must look the same on every machine and must not spend
//! anybody's usage, so it cannot come from a live run. A scene plays a
//! script through the same path a live turn takes: the message is sent to a
//! scripted back end, the events come back through the panel's own
//! handling, and where the script says the assistant changed the song, the
//! song is really changed, inside the turn's undo group. What the picture
//! shows is therefore what the panel does, not a drawing of it.
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
    CliInstallation, CliVersion, Event, FailureReason, Notice, SessionId, TokenUsage, TurnStart, TurnSummary,
    Unavailable,
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
    pub const ALL: [(&'static str, Scene); 7] = [
        ("ready", Scene::Ready),
        ("working", Scene::Working),
        ("answered", Scene::Answered),
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
                if let Some(bass) = bass {
                    write_bass_intro(assistant, bass);
                }
                script.say([
                    started(self == Scene::ApiKey),
                    call("1", "get_project", json!({})),
                    answer("1", "get_project", Ok("{ \"name\": \"First Light\", \"tempo\": 96, \"tracks\": 7 }")),
                    call("2", "analyze_music", json!({ "track": id(pad.map(|track| track.0)), "start": 0, "end": 32 })),
                    answer("2", "analyze_music", Ok("{ \"key\": \"A minor\", \"chords\": [\"Am\", \"F\", \"C\", \"G\"] }")),
                    call("3", "write_bass", write),
                    answer("3", "write_bass", Ok("{ \"clip_id\": 58, \"notes\": 32 }")),
                    Event::Text(
                        "I added an eight-bar line to Bass at bar 1. It plays the root of each pad chord in eighths (A, F, \
                         C, G, two bars each) and stays below the pad\u{2019}s lowest voice.\n\nIf it crowds the intro, \
                         halving it to quarter notes would leave more room."
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
                    Event::Text("The hats now run in sixteenths through Beat A. Next, the pad.".to_owned()),
                    call(
                        "4",
                        "update_track",
                        json!({ "track": id(track("Pad").map(|track| track.0)), "volume_db": -9.5 }),
                    ),
                ]);
                assistant.frozen_seconds = Some(14);
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
/// eighths, two bars to a chord.
fn write_bass_intro(assistant: &Assistant, bass: TrackId) {
    let roots = [33u8, 29, 36, 31];
    let notes: Vec<Note> = (0..64)
        .map(|step| Note { pitch: roots[step / 16], start: step as f64 * 0.5, length: 0.4, velocity: 92 })
        .collect();
    let edit = |project: &mut Project| {
        let clip = project.add_midi_clip(bass, "Bass intro", 0.0, 32.0)?;
        project.clip_mut(clip)?.add_notes(&notes)
    };
    if let Err(error) = assistant.session.lock().edit(edit) {
        log::warn!("the scene could not write its bass line: {error}");
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
