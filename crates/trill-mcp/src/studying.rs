//! Tools for a recording taken apart, and for what Trill fetches.
//!
//! `study` asks for a study of a recording (its stems, beats and words,
//! found by models in a process of its own, and everything `listen` hears,
//! heard in the stems), says how far one has come, stops one, lists and
//! removes them. What a study found is read with `listen` (`study`).
//!
//! `setup` is what Trill does not ship and fetches when the user asks:
//! the parts there are, list by list (the models of studies are one list;
//! whoever else has things to fetch adds theirs), with each part's size,
//! licence and source; fetching, stopping and removing one; what runs the
//! models on this machine, and the test that finds out; the programs
//! Trill found installed.
//!
//! Two things the assistant in the window does not do, because what it
//! reads was written by strangers and neither can be taken back by an
//! undo: it fetches nothing (no part, and no recording from a link), and
//! it removes nothing that was fetched. The user does those in the
//! window, or lets a client of their own.

use crate::server::{Reply, TrillServer, explain, finish};
use crate::view::round;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use trill_core::ClipId;
use trill_fetch::{ListStatus, PartState, PartStatus};
use trill_study::desk::WHOSE;
use trill_study::models::real::Provider;
use trill_study::setup::{Machine, Setup, provider_name};
use trill_study::{Report, Source, State, StemChoice, StudyFolder, StudyOptions};

/// Appended to the server's instructions.
pub(crate) const INSTRUCTIONS: &str = "\
Studying a recording
- study takes a recording apart with models that run on this machine: its stems (vocals, drums, bass, the rest), \
its beats and bar lines, the words that are sung with a time for each, and then everything listen hears, heard in \
the stems, where it is right of a whole mix too. It takes a minute or so for a song on a graphics card and several \
on a processor, and is kept: asked for once, it is there from then on. action \"start\" with a `path` (or an audio \
`clip` of the song) answers at once with the study's id; \"report\" says the stage it is at (give `wait_seconds` \
to be answered when it ends); \"stop\" ends it and keeps every stage that had finished, and \"start\" with that \
`study` takes it up there; \"list\" and \"delete\" are what they say.
- When it is done, listen with `study` reads it: the same summary and lists as of a file, in the recording's own \
bars, and `show` \"words\" for the lyrics line by line with the bar and beat of every word. `stem` keeps the loops \
and single hits to those of one stem and says where that stem plays. Each stem is an audio file (its `path` is in \
the report and in listen's reply) that create_audio_clip puts on a track. transcribe takes a `study` too. `grid` \
corrections given to listen for a study are kept with it.
- The models are not part of Trill. setup with action \"report\" lists the parts there are (each with its size, \
its licence in the words of its source, and where it comes from), which are here, what runs the models on this \
machine and how fast, and the programs Trill found. A study that lacks a part says which. Fetch a part (setup, \
action \"fetch\") only when the user has asked for it, after telling them its size and its licence line: these are \
other people's files, some of them large. \"test\" times the models on the processor and on each graphics card and \
takes the fastest that gives the same result; it runs for a minute or two.
- A recording behind a `link` is fetched by a program the user installed (yt-dlp), never by Trill itself: only a \
link the user gave you, and only what they may study. What a study finds is of someone's recording: take its \
tempo, its form and how its groove sits, not its melody or its words.";

/// How long a call may wait for a study, in seconds.
const LONGEST_WAIT: f64 = 900.0;
/// How many parts of a list a reply holds when the caller does not say,
/// and the most it can ask for.
const DEFAULT_PARTS: usize = 40;
const MAX_PARTS: usize = 200;

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StudyAction {
    /// Ask for a study of a recording (`path`, `clip` or `link`), or take one up again (`study`).
    Start,
    /// How far a study has come (`study`; without it, the one that is running, or the latest).
    Report,
    /// End the run of a study. Every stage that had finished is kept.
    Stop,
    /// Every study there is, latest first.
    List,
    /// Remove a study and everything in its folder.
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StudyRequest {
    /// "start", "report", "stop", "list" or "delete".
    pub action: StudyAction,
    /// start: the whole path of an audio file to study (WAV, AIFF, FLAC, MP3, Ogg Vorbis; anything else where
    /// ffmpeg is installed), up to 15 minutes long.
    pub path: Option<String>,
    /// start: or an audio clip of the song, by its id: the file it plays is studied.
    pub clip: Option<u64>,
    /// start: or a link to a page with a recording, which the program the user installed for it (yt-dlp) fetches.
    /// Only a link the user gave, of something they may study.
    pub link: Option<String>,
    /// report, stop, delete: the study, by its id. start: a study that was stopped or that failed, to take it up
    /// where it was.
    pub study: Option<String>,
    /// start: what to call the study (default: the file's name, or what the linked page calls the recording).
    pub title: Option<String>,
    /// start: whether the words are written down (default true). Without them a study needs two parts fewer.
    pub words: Option<bool>,
    /// start: the language sung, as two letters ("en", "de", "ja"). Leave it out for the model to tell.
    pub language: Option<String>,
    /// start: split with four models, one tuned for each stem: the cleanest stems, at four times the work, and a
    /// part of its own to fetch (default false).
    pub best_stems: Option<bool>,
    /// report: wait up to this many seconds for the study to end before answering (at most 900).
    pub wait_seconds: Option<f64>,
}

#[derive(Copy, Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupAction {
    /// What there is to fetch and what is here, what runs the models, the programs that were found.
    Report,
    /// Start fetching a part (`part`). Answers at once: report says how far it is.
    Fetch,
    /// Stop the fetch of a part. What has come of it is kept, and fetching it again goes on from there.
    Stop,
    /// Remove a part that was fetched.
    Remove,
    /// Time the models on the processor and on every graphics card, and take the fastest that gives the same
    /// result. Runs for a minute or two.
    Test,
    /// Have the models run on something else the test timed (`run_on`).
    Choose,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetupRequest {
    /// "report", "fetch", "stop", "remove", "test" or "choose".
    pub action: SetupAction,
    /// fetch, stop, remove: the part, by its id as report lists it ("stems", "beats").
    pub part: Option<String>,
    /// report: only this list ("models"), with its parts paged.
    pub list: Option<String>,
    /// choose: what runs the models: "processor", "card" (the first graphics card; "card2" the second), or
    /// "nvidia" (an NVIDIA card through NVIDIA's own libraries).
    pub run_on: Option<String>,
    /// report: skip this many parts of each list.
    pub offset: Option<usize>,
    /// report: how many parts of each list (default 40, at most 200).
    pub limit: Option<usize>,
}

fn megabytes(bytes: u64) -> u64 {
    (bytes as f64 / (1u64 << 20) as f64).round() as u64
}

fn named<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// A study as a reply says it.
pub(crate) fn study_view(report: &Report, folder: &StudyFolder) -> Value {
    let study = &report.study;
    let mut out = json!({
        "study": study.id,
        "title": study.title,
        "state": named(&study.state),
        "size_mb": megabytes(report.size),
    });
    match &study.source {
        Source::File { path } => out["of"] = json!(path.display().to_string()),
        Source::Link { url, by } => {
            out["of"] = json!(url);
            if let Some(by) = by {
                out["by"] = json!(by);
            }
        }
    }
    if study.seconds > 0.0 {
        out["seconds"] = json!(round(study.seconds));
    }
    if let Some((stage, share)) = report.at {
        out["at"] = json!({ "stage": named(&stage), "doing": stage.doing(), "percent": (share * 100.0).round() });
    }
    if !study.stages.is_empty() {
        out["stages"] = study
            .stages
            .iter()
            .map(|stage| {
                let mut item = json!({ "stage": named(&stage.stage), "state": named(&stage.state) });
                if let Some(seconds) = stage.seconds {
                    item["seconds"] = json!(seconds);
                }
                if !stage.note.is_empty() {
                    item["note"] = json!(stage.note);
                }
                item
            })
            .collect();
    }
    if let Some(device) = &study.device {
        out["ran_on"] = json!(device);
    }
    let stems: Vec<Value> = study
        .stems
        .iter()
        .map(|name| (name, folder.stem(name)))
        .filter(|(_, path)| path.is_file())
        .map(|(name, path)| json!({ "name": name, "path": path.display().to_string() }))
        .collect();
    if !stems.is_empty() {
        out["stems"] = json!(stems);
    }
    if !study.corrections.is_empty() {
        out["grid"] = json!(study.corrections.iter().map(|correction| correction.name()).collect::<Vec<_>>());
    }
    if let Some(error) = &study.error {
        out["error"] = json!(error);
    }
    match study.state {
        State::Done => out["next"] = json!("listen with this `study` reads what was found"),
        State::Stopped | State::Failed => {
            out["next"] = json!("study with action \"start\" and this `study` takes it up where it was");
        }
        State::Running | State::Waiting => {}
    }
    out
}

/// Why a study cannot be read yet, said so that the next call is plain.
pub(crate) fn not_done(report: &Report) -> String {
    let id = &report.study.id;
    let again = format!("study with action \"start\" and study \"{id}\" takes it up where it was");
    match report.study.state {
        State::Running | State::Waiting => {
            let at = report.at.map_or_else(
                || "waiting for its turn".to_owned(),
                |(stage, share)| format!("{}, {:.0} %", stage.while_doing(), share * 100.0),
            );
            format!(
                "study {id} is not done yet ({at}): study with action \"report\" and `wait_seconds` answers when it is"
            )
        }
        State::Failed => {
            format!("study {id} failed: {}. {again}", report.study.error.as_deref().unwrap_or("it did not say why"))
        }
        State::Stopped => format!("study {id} was stopped before it was done: {again}"),
        State::Done => format!("study {id} holds nothing that was heard: {again}"),
    }
}

fn part_view(status: &PartStatus) -> Value {
    let part = &status.part;
    let mut out = json!({
        "part": part.id,
        "name": part.name,
        "what": part.what,
        "size_mb": megabytes(part.size()),
        "state": named(&status.state),
        "licence": part.licence,
        "from": part.from,
    });
    if !part.credit.is_empty() {
        out["credit"] = json!(part.credit);
    }
    if !part.needs.is_empty() {
        out["needs"] = json!(part.needs);
    }
    if status.state != PartState::Ready && status.got > 0 {
        out["percent"] = json!((status.got as f64 * 100.0 / part.size().max(1) as f64).floor());
    }
    if status.on_disk > 0 {
        out["on_disk_mb"] = json!(megabytes(status.on_disk));
    }
    if let Some(error) = &status.error {
        out["error"] = json!(error);
    }
    out
}

fn list_view(list: &ListStatus, offset: usize, limit: usize) -> Value {
    let total = list.parts.len();
    let mut out = json!({
        "list": list.id,
        "title": list.title,
        "about": list.about,
        "kept_in": list.folder.display().to_string(),
        "total": total,
        "parts": list.parts.iter().skip(offset).take(limit).map(part_view).collect::<Vec<_>>(),
    });
    if offset + limit < total {
        out["more"] = json!(offset + limit);
    }
    out
}

fn machine_view(setup: &Setup) -> Value {
    let Some(machine) = setup.machine() else {
        return json!({
            "runs_on": provider_name(setup.provider()),
            "note": "the timed test has not been run: until it has (setup, action \"test\"), the models run on the processor, which is the slowest way",
        });
    };
    let Machine { results, chosen, by_user, tested, .. } = &machine;
    let mut out = json!({
        "runs_on": provider_name(setup.provider()),
        "chosen_by": if *by_user { "the user" } else { "the timed test" },
        "tested_at_unix": tested,
        "timed": results
            .iter()
            .map(|timed| match (&timed.checked, &timed.error) {
                (Some(checked), _) => json!({
                    "on": timed.device,
                    "seconds_for_a_minute_of_music": round(checked.seconds_a_minute),
                    "ready_in_seconds": round(checked.ready_seconds),
                    "memory_mb": megabytes(timed.peak),
                    "chosen": timed.provider == *chosen,
                }),
                (None, error) => json!({ "on": timed.device, "did_not_run": error.as_deref().unwrap_or("it did not say why") }),
            })
            .collect::<Vec<_>>(),
    });
    if setup.untested() {
        out["note"] = json!(
            "a part that runs the models has come or gone since the test: until it is run again (setup, action \"test\"), the models run on the processor"
        );
    }
    out
}

fn run_on(text: &str) -> Result<Provider, String> {
    match text.trim() {
        "processor" => Ok(Provider::Processor),
        "nvidia" => Ok(Provider::Nvidia),
        "card" => Ok(Provider::AnyCard { adapter: 0 }),
        other => other
            .strip_prefix("card")
            .and_then(|number| number.parse::<u32>().ok())
            .filter(|&number| number >= 1)
            .map(|number| Provider::AnyCard { adapter: number - 1 })
            .ok_or_else(|| {
                format!("'{other}' is not something that runs models: \"processor\", \"card\", \"card2\" or \"nvidia\"")
            }),
    }
}

impl TrillServer {
    async fn studied(&self, request: StudyRequest) -> Reply {
        let desk = self.runtime.studies()?;
        let needs = |what: &str| format!("say which study: `study` is its id, which \"list\" shows ({what})");
        match request.action {
            StudyAction::Start => {
                let given =
                    [request.path.is_some(), request.clip.is_some(), request.link.is_some(), request.study.is_some()];
                if given.iter().filter(|given| **given).count() != 1 {
                    return Err("say what to study with exactly one of `path` (an audio file), `clip` (an audio clip of the song), `link`, or `study` (one to take up again)".to_owned());
                }
                if let Some(id) = &request.study {
                    let study = desk.resume(id)?;
                    return Ok(study_view(&desk.report(&study.id)?, &desk.studies().folder(&study.id)));
                }
                let source = if let Some(url) = request.link {
                    if self.is_guarded() {
                        return Err("From here a study is of a file or of a clip of the song. A recording behind a link is fetched only when the user gives the link themselves: in the window, or to a client of their own.".to_owned());
                    }
                    Source::Link { url: url.trim().to_owned(), by: None }
                } else if let Some(clip) = request.clip {
                    let session = self.session.lock();
                    let project = session.project();
                    let found = project.clip(ClipId(clip)).map_err(|error| explain(project, error))?;
                    let audio = found.audio().ok_or_else(|| {
                        format!(
                            "clip {clip} ({}) holds notes, not audio: there is no recording in it to study",
                            found.name
                        )
                    })?;
                    Source::File { path: PathBuf::from(&audio.path) }
                } else {
                    Source::File { path: PathBuf::from(request.path.unwrap_or_default().trim()) }
                };
                let language = match request.language.as_deref().map(str::trim).filter(|language| !language.is_empty())
                {
                    None => None,
                    Some(language)
                        if (2..=3).contains(&language.len())
                            && language.chars().all(|letter| letter.is_ascii_lowercase()) =>
                    {
                        Some(language.to_owned())
                    }
                    Some(other) => {
                        return Err(format!("'{other}' is not a language as two letters: \"en\", \"de\", \"ja\""));
                    }
                };
                let options = StudyOptions {
                    stems: if request.best_stems == Some(true) { StemChoice::Best } else { StemChoice::Fast },
                    words: request.words.unwrap_or(true),
                    language,
                };
                let link = matches!(source, Source::Link { .. });
                let study = desk.start(source, options, request.title.as_deref())?;
                let mut out = study_view(&desk.report(&study.id)?, &desk.studies().folder(&study.id));
                out["next"] = json!(
                    "study with action \"report\" (and `wait_seconds`) says how far it is; listen with this `study` reads it when it is done"
                );
                if link {
                    out["note"] = json!(WHOSE);
                }
                if desk.setup().untested() {
                    out["runs_on"] = json!(
                        "the processor, which is the slowest way: the timed test has not been run (setup, action \"test\")"
                    );
                }
                Ok(out)
            }
            StudyAction::Report => {
                let id =
                    match request.study {
                        Some(id) => id,
                        None => {
                            let all = desk.list();
                            let running =
                                all.iter().find(|report| matches!(report.study.state, State::Running | State::Waiting));
                            match running.or(all.first()) {
                                Some(report) => report.study.id.clone(),
                                None => return Err(
                                    "no study has been made yet: study with action \"start\" and a `path` makes one"
                                        .to_owned(),
                                ),
                            }
                        }
                    };
                let report = match request.wait_seconds.filter(|seconds| *seconds > 0.0) {
                    None => desk.report(&id)?,
                    Some(seconds) => {
                        desk.report(&id)?;
                        let (runtime, waited) = (self.runtime.clone(), id.clone());
                        let within = Duration::from_secs_f64(seconds.min(LONGEST_WAIT));
                        tokio::task::spawn_blocking(move || runtime.studies()?.wait(&waited, within))
                            .await
                            .map_err(|error| format!("waiting stopped: {error}"))??
                    }
                };
                Ok(study_view(&report, &desk.studies().folder(&id)))
            }
            StudyAction::Stop => {
                let id = request.study.ok_or_else(|| needs("to stop"))?;
                desk.stop(&id)?;
                // The process is ended at once; its study says so a moment later.
                let (runtime, waited) = (self.runtime.clone(), id.clone());
                let report =
                    tokio::task::spawn_blocking(move || runtime.studies()?.wait(&waited, Duration::from_secs(20)))
                        .await
                        .map_err(|error| format!("waiting stopped: {error}"))??;
                Ok(study_view(&report, &desk.studies().folder(&id)))
            }
            StudyAction::List => {
                let studies: Vec<Value> = desk
                    .list()
                    .iter()
                    .map(|report| {
                        let mut item = json!({
                            "study": report.study.id,
                            "title": report.study.title,
                            "state": named(&report.study.state),
                            "size_mb": megabytes(report.size),
                        });
                        if report.study.seconds > 0.0 {
                            item["seconds"] = json!(round(report.study.seconds));
                        }
                        if let Some((stage, share)) = report.at {
                            item["at"] = json!(format!("{}, {:.0} %", stage.while_doing(), share * 100.0));
                        }
                        item
                    })
                    .collect();
                Ok(json!({ "kept_in": desk.folder().display().to_string(), "studies": studies }))
            }
            StudyAction::Delete => {
                let id = request.study.ok_or_else(|| needs("to remove"))?;
                let runtime = self.runtime.clone();
                let study = tokio::task::spawn_blocking(move || runtime.studies()?.delete(&id))
                    .await
                    .map_err(|error| format!("removing stopped: {error}"))??;
                Ok(json!({ "removed": study.id, "title": study.title }))
            }
        }
    }

    async fn set_up(&self, request: SetupRequest) -> Reply {
        let fetches = self.runtime.fetches()?.clone();
        let desk = self.runtime.studies()?;
        let setup = desk.setup().clone();
        let part = || {
            request.part.clone().ok_or_else(|| "say which part: `part` is its id, which \"report\" lists".to_owned())
        };
        match request.action {
            SetupAction::Report => {
                let (offset, limit) =
                    (request.offset.unwrap_or(0), request.limit.unwrap_or(DEFAULT_PARTS).clamp(1, MAX_PARTS));
                let status = fetches.status();
                if let Some(wanted) = &request.list
                    && !status.iter().any(|list| list.id == *wanted)
                {
                    let known: Vec<&str> = status.iter().map(|list| list.id.as_str()).collect();
                    return Err(format!("there is no list '{wanted}'. Lists: {}", known.join(", ")));
                }
                let lists: Vec<Value> = status
                    .iter()
                    .filter(|list| request.list.as_ref().is_none_or(|wanted| list.id == *wanted))
                    .map(|list| list_view(list, offset, limit))
                    .collect();
                let programs: Vec<Value> = tokio::task::spawn_blocking({
                    let setup = setup.clone();
                    move || setup.programs()
                })
                .await
                .map_err(|error| format!("looking for programs stopped: {error}"))?
                .into_iter()
                .map(|program| match program.path {
                    Some(path) => {
                        json!({ "name": program.name, "for": program.what, "found": path.display().to_string() })
                    }
                    None => json!({ "name": program.name, "for": program.what, "found": false, "get_it": program.get }),
                })
                .collect();
                Ok(json!({
                    "lists": lists,
                    "models": machine_view(&setup),
                    "programs": programs,
                    "note": "Trill ships and hosts none of this: each part comes from where its makers publish it, when the user asks for it, and is checked against what it was when Trill was made.",
                }))
            }
            SetupAction::Fetch => {
                if self.is_guarded() {
                    return Err("From here nothing is fetched. The user fetches a part in Setup, in the File menu, where its size and whose it is are shown; tell them which part is wanted and why.".to_owned());
                }
                let part = fetches.fetch(&part()?)?;
                let mut out = part_view(&fetches.status_of(&part.id).ok_or("the part went away")?);
                out["next"] = json!("setup with action \"report\" says how far it is");
                Ok(out)
            }
            SetupAction::Stop => {
                let part = fetches.stop(&part()?)?;
                Ok(
                    json!({ "stopped": part.id, "note": "what has come of it is kept, and fetching it again goes on from there" }),
                )
            }
            SetupAction::Remove => {
                if self.is_guarded() {
                    return Err(
                        "From here nothing that was fetched is removed: the user does that in Setup, in the File menu."
                            .to_owned(),
                    );
                }
                let id = part()?;
                let freed = fetches.remove(&id)?;
                Ok(json!({ "removed": id, "freed_mb": megabytes(freed) }))
            }
            SetupAction::Test => {
                let Some(program) = desk.program() else {
                    return Err(
                        "this program was not started as one that can study: nothing is timed from here".to_owned()
                    );
                };
                if desk.busy() {
                    return Err("a study is running, and the test would be timed against it: run the test when the study is done".to_owned());
                }
                tokio::task::spawn_blocking({
                    let setup = setup.clone();
                    move || setup.test(&program, &AtomicBool::new(false), &mut |_, _| {})
                })
                .await
                .map_err(|error| format!("the timed test stopped: {error}"))??;
                Ok(machine_view(&setup))
            }
            SetupAction::Choose => {
                let wanted = request
                    .run_on
                    .as_deref()
                    .ok_or("say what runs the models: `run_on` is \"processor\", \"card\", \"card2\" or \"nvidia\"")?;
                setup.choose(run_on(wanted)?)?;
                Ok(machine_view(&setup))
            }
        }
    }
}

#[tool_router(router = studying_router, vis = "pub(crate)")]
impl TrillServer {
    /// Take a recording apart with models that run on this machine, and keep what was found: its stems (vocals,
    /// drums, bass, the rest) as audio files, its beats and bar lines, the words sung with a time for each, and
    /// its key, chords, parts, loops, hits, groove and notes heard in the stems. "start" answers at once; a study
    /// takes a minute or more, and "report" says how far it is. Read a finished study with listen (`study`).
    #[tool(annotations(title = "Study a recording"))]
    async fn study(&self, Parameters(request): Parameters<StudyRequest>) -> CallToolResult {
        finish(self.studied(request).await)
    }

    /// What Trill does not ship and fetches when the user asks: the parts there are (the models of studies, what
    /// runs them), each with its size, its licence in the words of its source and where it comes from; which are
    /// here; what runs the models on this machine, and the test that times it; the programs Trill found
    /// installed. Fetch a part only when the user asked for it and has been told its size and licence.
    #[tool(annotations(title = "Set up what Trill fetches"))]
    async fn setup(&self, Parameters(request): Parameters<SetupRequest>) -> CallToolResult {
        finish(self.set_up(request).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_runs_the_models_is_read_from_a_word() {
        assert_eq!(run_on("processor"), Ok(Provider::Processor));
        assert_eq!(run_on(" card "), Ok(Provider::AnyCard { adapter: 0 }));
        assert_eq!(run_on("card2"), Ok(Provider::AnyCard { adapter: 1 }));
        assert_eq!(run_on("nvidia"), Ok(Provider::Nvidia));
        assert!(run_on("card0").is_err());
        assert_eq!(
            run_on("gpu").unwrap_err(),
            "'gpu' is not something that runs models: \"processor\", \"card\", \"card2\" or \"nvidia\""
        );
        assert_eq!((megabytes(0), megabytes(1 << 30)), (0, 1024));
    }
}
