//! The conversation as the panel shows it: what was said, each tool call as
//! a line, the notices, and how each turn ended.
//!
//! This is the app's own record, built from the events of a turn as they
//! arrive. It is what is drawn, and what is kept beside the settings so
//! that a conversation taken up again another day can be read back. (The
//! CLI keeps a record of its own for the assistant's memory; that one is
//! never read here.)

use super::tools;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use trill_assistant::{Event, FailureReason, Notice, TurnSummary};
use trill_core::Project;
use trill_engine::session::UndoStep;

/// More items than this and the oldest exchanges are let go.
const MAX_ITEMS: usize = 600;
/// How much of a tool's arguments or of its answer is kept. `get_project`
/// on a full song answers with hundreds of kilobytes; nobody reads that in
/// a side panel, and the assistant has it regardless.
const MAX_DETAIL: usize = 4000;

/// How a tool call stands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    Running,
    Done,
    /// The tool reported an error (which the assistant also sees, and
    /// usually puts right).
    Failed,
    /// The turn ended before the call did.
    Stopped,
}

/// One tool call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolLine {
    /// Pairs the start of a call with its end. Means nothing afterwards.
    #[serde(skip)]
    pub id: String,
    /// Trill's name for the tool: `create_track`.
    pub tool: String,
    /// What it is doing, in words, while it runs.
    pub doing: String,
    /// What it did, in words.
    pub did: String,
    pub state: ToolState,
    /// The arguments, as the tool received them.
    pub input: String,
    /// What the tool answered, or the error it reported.
    pub output: String,
    /// Whether the line is unfolded to show the two above.
    #[serde(skip)]
    pub open: bool,
}

impl ToolLine {
    /// The line as it reads now.
    pub fn words(&self) -> &str {
        match self.state {
            ToolState::Running => &self.doing,
            ToolState::Done | ToolState::Failed | ToolState::Stopped => &self.did,
        }
    }
}

/// What a line the panel adds on its own is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Info,
    Warning,
}

/// What the user can do about a turn that did not finish.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Offer {
    Nothing,
    /// Send the same message again.
    TryAgain,
    /// Ask the assistant to carry on from where it stopped.
    Continue,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum Ending {
    Finished {
        /// How long the CLI worked on the message.
        seconds: f32,
        /// How many times the model was asked.
        steps: u32,
        /// What the CLI estimates the turn would cost at API prices, in US
        /// dollars. An estimate, never a charge.
        estimated_cost: Option<f64>,
    },
    /// The user stopped it.
    Stopped,
    Failed {
        /// The reason, in the back end's own words.
        text: String,
        offer: Offer,
    },
}

/// The line that closes a turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnEnd {
    #[serde(flatten)]
    pub ending: Ending,
    /// Whether the turn changed the song.
    pub changed: bool,
    /// The undo step that holds everything the turn changed. Known only to
    /// the session that made it, so it is not kept between runs.
    #[serde(skip)]
    pub step: Option<UndoStep>,
}

/// One thing in the conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    /// What the user sent.
    User {
        text: String,
    },
    /// A passage of the assistant's reply.
    Reply {
        text: String,
    },
    Tool(ToolLine),
    /// Something the back end or the panel has to say.
    Note {
        text: String,
        tone: Tone,
        /// Notes of one slot replace each other: "trying again (2 of 10)"
        /// takes the place of "(1 of 10)".
        #[serde(skip)]
        slot: &'static str,
    },
    End(TurnEnd),
}

/// The conversation so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    pub items: Vec<Item>,
}

/// `text`, cut to what is kept of a tool's arguments or answer.
fn detail(text: &str) -> String {
    match text.char_indices().nth(MAX_DETAIL) {
        Some((end, _)) => {
            format!("{}\u{2026}\n({} more characters)", &text[..end], text[end..].chars().count())
        }
        None => text.to_owned(),
    }
}

/// How long until `when`, in words: "in about 2 hours", "in 25 minutes".
/// Relative, because telling the time of day would need the time zone.
fn until(when: SystemTime, now: SystemTime) -> Option<String> {
    let minutes = when.duration_since(now).ok()?.as_secs().div_ceil(60);
    Some(match minutes {
        0..=1 => "in a minute".to_owned(),
        2..=89 => format!("in {minutes} minutes"),
        _ => format!("in about {} hours", (minutes + 30) / 60),
    })
}

/// A notice as a line of the conversation.
fn note(notice: &Notice, now: SystemTime) -> Item {
    let (tone, slot) = match notice {
        Notice::EnvironmentVariableIgnored { .. } => (Tone::Info, ""),
        Notice::Retrying { .. } => (Tone::Warning, "retry"),
        Notice::RateLimited { .. } => (Tone::Warning, "retry"),
        Notice::UsageLimitNear { .. } => (Tone::Warning, "limit"),
        Notice::PermissionDenied { .. } => (Tone::Warning, ""),
    };
    let mut text = notice.to_string();
    if let Notice::UsageLimitNear { resets_at: Some(when) } = notice
        && let Some(wait) = until(*when, now)
    {
        // The CLI's own word on when; nothing is worked out from the plan.
        text.push_str(&format!(" Claude Code says it renews {wait}."));
    }
    Item::Note { text, tone, slot }
}

/// What to offer after a failure.
pub fn offer_for(reason: &FailureReason) -> Offer {
    match reason {
        FailureReason::TurnLimitReached { .. } => Offer::Continue,
        // Sending it again would be refused again.
        FailureReason::UnexpectedTools { .. } => Offer::Nothing,
        _ => Offer::TryAgain,
    }
}

/// Whether a failure means the assistant has to be looked for again before
/// another message can go: the CLI is gone or signed out, or the server it
/// works through is not answering.
pub fn calls_for_a_check(reason: &FailureReason) -> bool {
    matches!(
        reason,
        FailureReason::CliMissing { .. }
            | FailureReason::NotLoggedIn
            | FailureReason::McpServerUnavailable { .. }
            | FailureReason::CannotStart { .. }
    )
}

/// The reason for a failure, as shown: the back end's words, with when a
/// used-up allowance renews if the CLI said.
pub fn failure_text(reason: &FailureReason, now: SystemTime) -> String {
    let mut text = reason.to_string();
    if let FailureReason::UsageLimitReached { resets_at: Some(when), .. } = reason
        && let Some(wait) = until(*when, now)
    {
        text.push_str(&format!(" It says the limit renews {wait}."));
    }
    text
}

impl Transcript {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The user's message opens a turn.
    pub fn push_user(&mut self, text: &str) {
        self.items.push(Item::User { text: text.trim().to_owned() });
        self.trim();
    }

    /// Takes in an event of the turn under way. The three that end a turn
    /// are for [`Self::end`], which needs to know what the turn changed.
    pub fn apply(&mut self, event: &Event, project: &Project) {
        match event {
            Event::Text(piece) => match self.items.last_mut() {
                Some(Item::Reply { text }) => text.push_str(piece),
                _ => {
                    let piece = piece.trim_start();
                    if !piece.is_empty() {
                        self.items.push(Item::Reply { text: piece.to_owned() });
                    }
                }
            },
            Event::ToolCallStarted { id, tool, input } => {
                let wording = tools::describe(tool, input, project);
                let input = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
                self.items.push(Item::Tool(ToolLine {
                    id: id.clone(),
                    tool: tool.clone(),
                    doing: wording.doing,
                    did: wording.did,
                    state: ToolState::Running,
                    input: detail(&input),
                    output: String::new(),
                    open: false,
                }));
            }
            Event::ToolCallFinished { id, tool, result } => {
                let (state, output) = match result {
                    Ok(answer) => (ToolState::Done, detail(answer)),
                    Err(error) => (ToolState::Failed, detail(error)),
                };
                let running = self.items.iter_mut().rev().find_map(|item| match item {
                    Item::Tool(line) if line.id == *id && line.state == ToolState::Running => Some(line),
                    _ => None,
                });
                match running {
                    Some(line) => {
                        line.state = state;
                        line.output = output;
                    }
                    // The start was never seen: the end alone still says
                    // what was done.
                    None => {
                        let wording = tools::describe(tool, &serde_json::Value::Null, project);
                        self.items.push(Item::Tool(ToolLine {
                            id: id.clone(),
                            tool: tool.clone(),
                            doing: wording.doing,
                            did: wording.did,
                            state,
                            input: String::new(),
                            output,
                            open: false,
                        }));
                    }
                }
            }
            Event::Notice(notice) => {
                let new = note(notice, SystemTime::now());
                let slot_of = |item: &Item| match item {
                    Item::Note { slot, .. } => *slot,
                    _ => "",
                };
                let slot = slot_of(&new);
                match self.items.last_mut() {
                    Some(last) if !slot.is_empty() && slot_of(last) == slot => *last = new,
                    _ => self.items.push(new),
                }
            }
            Event::Started(_) | Event::Finished(_) | Event::Cancelled | Event::Failed(_) => {}
        }
    }

    /// Closes the turn: calls still running are marked as cut short, and
    /// the line that says how it ended goes last.
    pub fn end(&mut self, end: TurnEnd) {
        for item in self.items.iter_mut().rev() {
            match item {
                Item::Tool(line) if line.state == ToolState::Running => line.state = ToolState::Stopped,
                Item::User { .. } | Item::End(_) => break,
                _ => {}
            }
        }
        self.items.push(Item::End(end));
        self.trim();
    }

    /// The last message the user sent.
    pub fn last_message(&self) -> Option<&str> {
        self.items.iter().rev().find_map(|item| match item {
            Item::User { text } => Some(text.as_str()),
            _ => None,
        })
    }

    /// Lets go of the oldest exchanges once there are too many items,
    /// whole exchanges at a time so that none begins in its middle.
    fn trim(&mut self) {
        while self.items.len() > MAX_ITEMS {
            let next = self.items.iter().skip(1).position(|item| matches!(item, Item::User { .. }));
            match next {
                Some(index) => drop(self.items.drain(..=index)),
                None => break,
            }
        }
    }

    /// What is kept between runs.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.items).unwrap_or_else(|_| "[]".to_owned())
    }

    /// Reads a kept conversation. One that was cut off when the program
    /// closed mid-turn is closed here; text that makes no sense is no
    /// conversation.
    pub fn from_json(text: &str) -> Option<Self> {
        let mut transcript = Self { items: serde_json::from_str(text).ok()? };
        let open = transcript.items.iter().rev().find_map(|item| match item {
            Item::End(_) => Some(false),
            Item::User { .. } => Some(true),
            _ => None,
        });
        if open == Some(true) {
            transcript.end(TurnEnd { ending: Ending::Stopped, changed: false, step: None });
        }
        Some(transcript)
    }
}

/// How a finished turn is summed up.
pub fn finished(summary: &TurnSummary) -> Ending {
    Ending::Finished {
        seconds: summary.duration.as_secs_f32(),
        steps: summary.turns,
        estimated_cost: summary.estimated_cost_usd,
    }
}

/// `"8 s"`, `"1 min 12 s"`.
pub fn format_seconds(seconds: f32) -> String {
    let whole = Duration::from_secs_f32(seconds.max(0.0)).as_secs();
    match whole {
        0..=59 => format!("{whole} s"),
        _ => format!("{} min {} s", whole / 60, whole % 60),
    }
}

/// The CLI's cost figure as shown: always marked as an estimate.
pub fn format_estimate(dollars: f64) -> String {
    if dollars < 0.01 { "est. under $0.01".to_owned() } else { format!("est. ${dollars:.2}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trill_assistant::{SessionId, TokenUsage, TurnStart};

    fn text(piece: &str) -> Event {
        Event::Text(piece.to_owned())
    }

    fn started(id: &str, tool: &str, input: serde_json::Value) -> Event {
        Event::ToolCallStarted { id: id.to_owned(), tool: tool.to_owned(), input }
    }

    fn finished_call(id: &str, tool: &str, result: Result<&str, &str>) -> Event {
        Event::ToolCallFinished {
            id: id.to_owned(),
            tool: tool.to_owned(),
            result: result.map(str::to_owned).map_err(str::to_owned),
        }
    }

    fn end(ending: Ending) -> TurnEnd {
        TurnEnd { ending, changed: false, step: None }
    }

    fn tool(item: &Item) -> &ToolLine {
        match item {
            Item::Tool(line) => line,
            other => panic!("not a tool line: {other:?}"),
        }
    }

    #[test]
    fn text_arriving_in_pieces_is_one_passage_and_tools_part_passages() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("  add drums \n");
        let start = TurnStart {
            session_id: SessionId::parse("39367d22-e40b-4a04-9358-33e91538ff0d").unwrap(),
            model: "m".into(),
            api_key_source: None,
        };
        for event in [
            Event::Started(start),
            text("\n\nLet me "),
            text("look"),
            text(" first."),
            started("a", "get_project", json!({})),
            finished_call("a", "get_project", Ok("{\"tracks\":[]}")),
            started("b", "create_track", json!({ "name": "Drums" })),
            finished_call("b", "create_track", Err("no such instrument")),
            text("Done.\n\nA second "),
            text("paragraph."),
        ] {
            transcript.apply(&event, &project);
        }
        assert_eq!(transcript.items.len(), 5);
        assert_eq!(transcript.items[0], Item::User { text: "add drums".into() });
        assert_eq!(transcript.items[1], Item::Reply { text: "Let me look first.".into() });
        let read = tool(&transcript.items[2]);
        assert_eq!(
            (read.words(), read.state, read.output.as_str()),
            ("Read the project", ToolState::Done, "{\"tracks\":[]}")
        );
        let failed = tool(&transcript.items[3]);
        assert_eq!(failed.words(), "Added the track \u{201C}Drums\u{201D}");
        assert_eq!((failed.state, failed.output.as_str()), (ToolState::Failed, "no such instrument"));
        assert!(failed.input.contains("\"name\": \"Drums\""), "{}", failed.input);
        assert_eq!(transcript.items[4], Item::Reply { text: "Done.\n\nA second paragraph.".into() });
        assert_eq!(transcript.last_message(), Some("add drums"));
    }

    #[test]
    fn a_running_call_reads_as_under_way_and_is_cut_short_when_the_turn_ends() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("go");
        transcript.apply(&started("a", "get_project", json!({})), &project);
        transcript.apply(&finished_call("a", "get_project", Ok("{}")), &project);
        transcript.apply(&started("b", "write_drums", json!({ "groove": "house" })), &project);
        assert_eq!(tool(&transcript.items[2]).words(), "Writing drums");

        transcript.end(end(Ending::Stopped));
        assert_eq!(tool(&transcript.items[1]).state, ToolState::Done, "what had finished stays finished");
        let cut = tool(&transcript.items[2]);
        assert_eq!((cut.state, cut.words()), (ToolState::Stopped, "Wrote drums"));
        assert_eq!(transcript.items[3], Item::End(end(Ending::Stopped)));

        // An end without its start still makes a line; an end for a call
        // that already ended makes another instead of rewriting history.
        transcript.push_user("more");
        transcript.apply(&finished_call("zz", "delete_track", Ok("ok")), &project);
        let orphan = tool(transcript.items.last().unwrap());
        assert_eq!((orphan.state, orphan.tool.as_str(), orphan.input.as_str()), (ToolState::Done, "delete_track", ""));
    }

    #[test]
    fn notices_are_lines_and_repeats_take_each_others_place() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        let retry = |attempt| {
            Event::Notice(Notice::Retrying {
                attempt,
                max_attempts: 10,
                delay: Duration::from_secs(1),
                cause: trill_assistant::ApiErrorKind::Server,
            })
        };
        transcript.apply(&retry(1), &project);
        transcript.apply(&retry(2), &project);
        transcript.apply(&Event::Notice(Notice::PermissionDenied { tool: "render".into() }), &project);
        transcript.apply(&Event::Notice(Notice::PermissionDenied { tool: "render".into() }), &project);
        let texts: Vec<&str> = transcript
            .items
            .iter()
            .map(|item| match item {
                Item::Note { text, .. } => text.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "Connection trouble; trying again (2 of 10).",
                "The assistant was not allowed to use render.",
                "The assistant was not allowed to use render.",
            ]
        );
    }

    #[test]
    fn limits_are_relayed_in_the_clis_words_with_when_it_said() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let later = |seconds| Some(now + Duration::from_secs(seconds));
        let Item::Note { text, tone, .. } = note(&Notice::UsageLimitNear { resets_at: later(2 * 3600 + 600) }, now)
        else {
            panic!("a note")
        };
        assert_eq!(text, "You are close to your Claude usage limit. Claude Code says it renews in about 2 hours.");
        assert_eq!(tone, Tone::Warning);
        let Item::Note { text, .. } = note(&Notice::UsageLimitNear { resets_at: None }, now) else { panic!() };
        assert_eq!(text, "You are close to your Claude usage limit.");

        let reached = FailureReason::UsageLimitReached { message: "Limit reached.".into(), resets_at: later(25 * 60) };
        assert_eq!(
            failure_text(&reached, now),
            "Your Claude usage limit has been reached. Claude Code said: Limit reached. It says the limit renews in 25 \
             minutes."
        );
        // A time already past says nothing rather than something wrong.
        let past = FailureReason::UsageLimitReached { message: "Limit reached.".into(), resets_at: Some(now) };
        assert!(failure_text(&past, now + Duration::from_secs(5)).ends_with("Limit reached."));
        assert_eq!(until(now + Duration::from_secs(30), now).as_deref(), Some("in a minute"));
        assert_eq!(until(now + Duration::from_secs(89 * 60), now).as_deref(), Some("in 89 minutes"));
        assert_eq!(until(now + Duration::from_secs(5 * 3600), now).as_deref(), Some("in about 5 hours"));
    }

    #[test]
    fn each_failure_offers_what_can_be_done_about_it() {
        use FailureReason as F;
        let unreachable = F::McpServerUnavailable { url: "u".into(), status: "failed".into() };
        for setup in [
            F::NotLoggedIn,
            F::CliMissing { path: "claude".into() },
            unreachable,
            F::CannotStart { detail: "d".into() },
        ] {
            assert!(calls_for_a_check(&setup), "{setup:?}");
            assert_eq!(offer_for(&setup), Offer::TryAgain);
        }
        assert_eq!(offer_for(&F::TurnLimitReached { limit: 80 }), Offer::Continue);
        let limit = F::UsageLimitReached { message: "m".into(), resets_at: None };
        assert_eq!((offer_for(&limit), calls_for_a_check(&limit)), (Offer::TryAgain, false));
        assert_eq!(offer_for(&F::SessionNotFound), Offer::TryAgain);
        assert_eq!(offer_for(&F::UnexpectedTools { tools: vec!["Bash".into()] }), Offer::Nothing);
        assert!(!calls_for_a_check(&F::TurnLimitReached { limit: 80 }));
    }

    #[test]
    fn a_conversation_is_kept_and_read_back_without_what_only_this_run_knows() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("add drums");
        transcript.apply(&started("a", "create_track", json!({ "name": "Drums" })), &project);
        transcript.apply(&finished_call("a", "create_track", Ok("{\"track_id\":3}")), &project);
        transcript.apply(&text("Added."), &project);
        if let Item::Tool(line) = &mut transcript.items[1] {
            line.open = true;
        }
        let summary = TurnSummary {
            turns: 2,
            duration: Duration::from_millis(8400),
            usage: TokenUsage::default(),
            estimated_cost_usd: Some(0.0123),
        };
        transcript.end(TurnEnd { ending: finished(&summary), changed: true, step: None });

        let read = Transcript::from_json(&transcript.to_json()).expect("it reads back");
        assert_eq!(read.items.len(), 4);
        let line = tool(&read.items[1]);
        assert_eq!((line.id.as_str(), line.open), ("", false), "ids and what was unfolded are this run's");
        assert_eq!((line.did.as_str(), line.state), ("Added the track \u{201C}Drums\u{201D}", ToolState::Done));
        let Item::End(end) = &read.items[3] else { panic!("an end") };
        assert!(end.changed && end.step.is_none());
        assert_eq!(end.ending, Ending::Finished { seconds: 8.4, steps: 2, estimated_cost: Some(0.0123) });

        // A conversation the program closed in the middle of is closed.
        let mut cut = Transcript::default();
        cut.push_user("go");
        cut.apply(&started("a", "get_project", json!({})), &project);
        let read = Transcript::from_json(&cut.to_json()).unwrap();
        assert_eq!(tool(&read.items[1]).state, ToolState::Stopped);
        assert!(matches!(read.items.last(), Some(Item::End(TurnEnd { ending: Ending::Stopped, .. }))));

        assert_eq!(Transcript::from_json("{ not json"), None);
        assert_eq!(Transcript::from_json("[]"), Some(Transcript::default()));
    }

    #[test]
    fn long_answers_are_cut_and_old_exchanges_let_go() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        let long = "\u{00E9}".repeat(MAX_DETAIL + 250);
        transcript.push_user("first");
        transcript.apply(&finished_call("a", "get_project", Ok(&long)), &project);
        let line = tool(&transcript.items[1]);
        assert!(line.output.ends_with("(250 more characters)"), "{}", &line.output[line.output.len() - 40..]);
        assert_eq!(line.output.chars().filter(|c| *c == '\u{00E9}').count(), MAX_DETAIL);

        for index in 0..MAX_ITEMS {
            transcript.push_user(&format!("message {index}"));
            transcript.apply(&text("ok"), &project);
        }
        assert!(transcript.items.len() <= MAX_ITEMS);
        assert!(matches!(&transcript.items[0], Item::User { .. }), "it still begins with a message");
        assert_eq!(transcript.last_message(), Some(format!("message {}", MAX_ITEMS - 1).as_str()));
    }

    #[test]
    fn durations_and_estimates_read_plainly() {
        assert_eq!(format_seconds(0.4), "0 s");
        assert_eq!(format_seconds(8.4), "8 s");
        assert_eq!(format_seconds(72.9), "1 min 12 s");
        assert_eq!(format_estimate(0.0031), "est. under $0.01");
        assert_eq!(format_estimate(0.1234), "est. $0.12");
        assert_eq!(format_estimate(2.5), "est. $2.50");
    }
}
