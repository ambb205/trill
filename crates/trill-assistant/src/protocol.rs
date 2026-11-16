//! Reading the CLI's `stream-json` output: one JSON object per line.
//!
//! Everything here is plain data in, plain data out, so that the whole
//! pipeline can be tested from recorded output without starting a process.
//!
//! The CLI's output grows with every release. Lines of a type this module
//! does not know, fields it does not know, and lines that are not JSON at all
//! are skipped; only the handful of fields named below are relied on. The
//! shapes were recorded from CLI 2.1.325 (see `tests/fixtures`).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::config::PLAN_TOOLS;
use crate::event::{
    ApiErrorKind, Event, FailureReason, Notice, PlanChange, PlanStep, Progress, SessionId, StepStatus, TokenUsage,
    TurnStart, TurnSummary, WebLookup,
};

/// The longest line accepted. Tool results travel on one line each and can be
/// large (a rendered waveform, a long note list); this only stops a child
/// gone wrong from exhausting memory.
pub(crate) const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// Above this, the buffer of a finished line is released instead of kept.
const KEPT_BUFFER_BYTES: usize = 1024 * 1024;

/// A line exceeded [`MAX_LINE_BYTES`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LineTooLong;

/// Cuts a byte stream into lines, however the reads happen to fall.
///
/// Lines are handed on as bytes and only decoded once complete, so a
/// multi-byte character split across two reads is never seen in halves.
#[derive(Default)]
pub(crate) struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    pub(crate) fn push(&mut self, mut bytes: &[u8], mut on_line: impl FnMut(&[u8])) -> Result<(), LineTooLong> {
        while let Some(end) = bytes.iter().position(|byte| *byte == b'\n') {
            let (head, rest) = bytes.split_at(end);
            if self.pending.is_empty() {
                on_line(head);
            } else {
                self.pending.extend_from_slice(head);
                on_line(&self.pending);
                self.release();
            }
            bytes = &rest[1..];
        }
        if self.pending.len() + bytes.len() > MAX_LINE_BYTES {
            self.release();
            return Err(LineTooLong);
        }
        self.pending.extend_from_slice(bytes);
        Ok(())
    }

    /// Hands on whatever follows the last newline, at the end of the stream.
    pub(crate) fn finish(&mut self, mut on_line: impl FnMut(&[u8])) {
        if !self.pending.is_empty() {
            on_line(&self.pending);
            self.release();
        }
    }

    fn release(&mut self) {
        if self.pending.capacity() > KEPT_BUFFER_BYTES {
            self.pending = Vec::new();
        } else {
            self.pending.clear();
        }
    }
}

/// How the turn ends, once that is known from the output alone.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verdict {
    /// The CLI reported the end of the turn and is expected to exit.
    Reported(Event),
    /// The turn must not go on, and the CLI has to be stopped: it started
    /// without Trill's tools, or with tools that are not Trill's.
    Refused(FailureReason),
}

/// How the CLI process ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Exit {
    /// `None` when the process was killed by a signal.
    pub(crate) code: Option<i32>,
    pub(crate) stderr_tail: String,
}

/// What the translator needs to know about the run of the CLI it is reading.
#[derive(Debug, Clone)]
pub(crate) struct Expectations {
    pub(crate) mcp_server_name: String,
    pub(crate) mcp_url: String,
    /// The CLI's own tools that were asked for, by its names for them. A
    /// tool on offer that is neither one of these nor Trill's stops the
    /// turn.
    pub(crate) cli_tools: Vec<String>,
    /// The most model round trips this run was given.
    pub(crate) max_turns: Option<u32>,
    /// How far the request had come before this run began.
    pub(crate) before: Progress,
}

/// What a run of the CLI took, known once it has reported how it ended,
/// however it ended.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Figures {
    /// How many times the model was asked: the replies seen to begin. Not
    /// the CLI's own `num_turns`, which with 2.1.325 is the number of tool
    /// calls and one more (six replies that made nine calls: ten), while
    /// what its `--max-turns` counts is replies.
    pub(crate) turns: u32,
    pub(crate) duration: Duration,
    pub(crate) usage: TokenUsage,
    pub(crate) cost: Option<f64>,
}

/// Turns the CLI's lines into [`Event`]s.
pub(crate) struct Translator {
    expect: Expectations,
    /// `mcp__<server>__`, which every one of Trill's tools starts with.
    tool_prefix: String,
    initialized: bool,
    /// The reply being streamed, to recognise it when it is repeated whole.
    streaming: Option<StreamingMessage>,
    /// The last event sent was text, so text from a new block needs a break.
    after_text: bool,
    break_pending: bool,
    tool_names: HashMap<String, String>,
    /// Calls to the CLI's plan tools that have not answered: what was
    /// called, and with what. They are not shown as calls; what they did
    /// to the plan is reported when they answer.
    plan_calls: HashMap<String, (String, Value)>,
    /// Web lookups that have not answered.
    web_calls: HashSet<String>,
    /// How many replies of the model's this run has seen begin, and the
    /// last of them.
    steps: u32,
    last_reply: String,
    figures: Option<Figures>,
    /// The last error the CLI reported in place of a reply.
    api_error: Option<(ApiErrorKind, String)>,
    limit_rejected: bool,
    limit_resets_at: Option<SystemTime>,
    verdict: Option<Verdict>,
}

struct StreamingMessage {
    id: String,
    text: String,
}

impl Translator {
    pub(crate) fn new(expect: Expectations) -> Self {
        let tool_prefix = format!("mcp__{}__", expect.mcp_server_name);
        Self {
            expect,
            tool_prefix,
            initialized: false,
            streaming: None,
            after_text: false,
            break_pending: false,
            tool_names: HashMap::new(),
            plan_calls: HashMap::new(),
            web_calls: HashSet::new(),
            steps: 0,
            last_reply: String::new(),
            figures: None,
            api_error: None,
            limit_rejected: false,
            limit_resets_at: None,
            verdict: None,
        }
    }

    /// Set once the output has decided how the turn ends. Later lines are
    /// ignored.
    pub(crate) fn verdict(&self) -> Option<&Verdict> {
        self.verdict.as_ref()
    }

    /// What the CLI said the run took, once it has said how the run ended.
    pub(crate) fn figures(&self) -> Option<Figures> {
        self.figures
    }

    /// Reads one line of output and appends what it means to `events`.
    pub(crate) fn line(&mut self, line: &[u8], events: &mut Vec<Event>) {
        if self.verdict.is_some() {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        match text(&value, "type") {
            "system" => self.system(&value, events),
            "stream_event" => self.stream_event(&value["event"], events),
            "assistant" => self.assistant(&value, events),
            "user" => self.user(&value, events),
            "rate_limit_event" => self.rate_limit(&value["rate_limit_info"], events),
            "result" => self.result(&value),
            _ => {}
        }
    }

    /// The terminal event, once the process has ended.
    pub(crate) fn finish(self, cancelled: bool, exit: Exit) -> Event {
        match self.verdict {
            // The CLI finished the turn before any cancel took effect.
            Some(Verdict::Reported(event)) => event,
            _ if cancelled => Event::Cancelled,
            Some(Verdict::Refused(reason)) => Event::Failed(reason),
            None => Event::Failed(match self.api_error {
                Some((kind, message)) => classify_api_error(kind, message, self.limit_resets_at),
                None if exit.code == Some(0) && self.initialized => {
                    FailureReason::Protocol { detail: "the reply ended without a result".to_owned() }
                }
                None => FailureReason::CliExited { exit_code: exit.code, stderr_tail: exit.stderr_tail },
            }),
        }
    }

    fn system(&mut self, value: &Value, events: &mut Vec<Event>) {
        match text(value, "subtype") {
            "init" => self.init(value, events),
            "api_retry" => {
                let attempt = number(value, "attempt") as u32;
                let max_attempts = number(value, "max_retries") as u32;
                let delay = Duration::from_millis(number(value, "retry_delay_ms"));
                events.push(Event::Notice(match api_error_kind(text(value, "error")) {
                    ApiErrorKind::RateLimit => Notice::RateLimited { attempt, max_attempts, delay },
                    cause => Notice::Retrying { attempt, max_attempts, delay, cause },
                }));
            }
            "permission_denied" => {
                let tool = self.tool_name(text(value, "tool_name")).to_owned();
                events.push(Event::Notice(Notice::PermissionDenied { tool }));
            }
            // The conversation has outgrown what the model can hold. Seen
            // with CLI 2.1.325 run with `-p`: "compacting", then a status
            // with the outcome, then the boundary with the sizes; the run
            // then carries on by itself from the summary.
            "status" => match (text(value, "status"), text(value, "compact_result")) {
                ("compacting", _) => events.push(Event::Notice(Notice::Compacting)),
                (_, "" | "success") => {}
                (_, outcome) => {
                    let said = text(value, "compact_error");
                    let detail = if said.is_empty() { outcome.to_owned() } else { format!("{outcome}: {said}") };
                    events.push(Event::Notice(Notice::CompactionFailed { detail }));
                }
            },
            "compact_boundary" => {
                let sizes = &value["compact_metadata"];
                events.push(Event::Notice(Notice::Compacted {
                    tokens_before: sizes["pre_tokens"].as_u64(),
                    tokens_after: sizes["post_tokens"].as_u64(),
                }));
            }
            _ => {}
        }
    }

    fn init(&mut self, value: &Value, events: &mut Vec<Event>) {
        self.initialized = true;

        let servers = value["mcp_servers"].as_array().map(Vec::as_slice).unwrap_or_default();
        let ours = servers.iter().find(|server| text(server, "name") == self.expect.mcp_server_name);
        let status = ours.map_or("not loaded", |server| text(server, "status"));
        // "pending" means the connection is still being made; the CLI waits
        // for it before the model is asked.
        if !matches!(status, "connected" | "pending") {
            self.verdict = Some(Verdict::Refused(FailureReason::McpServerUnavailable {
                url: self.expect.mcp_url.clone(),
                status: status.to_owned(),
            }));
            return;
        }

        let tools = value["tools"].as_array().map(Vec::as_slice).unwrap_or_default();
        let mut unexpected: Vec<String> =
            tools.iter().filter_map(Value::as_str).filter(|tool| !self.is_allowed(tool)).map(str::to_owned).collect();
        unexpected.extend(
            servers
                .iter()
                .map(|server| text(server, "name"))
                .filter(|name| *name != self.expect.mcp_server_name)
                .map(|name| format!("MCP server {name}")),
        );
        if !unexpected.is_empty() {
            self.verdict = Some(Verdict::Refused(FailureReason::UnexpectedTools { tools: unexpected }));
            return;
        }

        let Some(session_id) = SessionId::parse(text(value, "session_id")) else {
            self.verdict = Some(Verdict::Refused(FailureReason::Protocol {
                detail: "the CLI did not say which conversation this is".to_owned(),
            }));
            return;
        };
        let api_key_source = Some(text(value, "apiKeySource")).filter(|source| !matches!(*source, "" | "none"));
        events.push(Event::Started(TurnStart {
            session_id,
            model: text(value, "model").to_owned(),
            api_key_source: api_key_source.map(str::to_owned),
        }));
    }

    /// A raw event of the model's streamed reply.
    fn stream_event(&mut self, event: &Value, events: &mut Vec<Event>) {
        match text(event, "type") {
            "message_start" => {
                let id = text(&event["message"], "id");
                self.count_reply(id, true, events);
                self.streaming = Some(StreamingMessage { id: id.to_owned(), text: String::new() });
            }
            "content_block_start" if text(&event["content_block"], "type") == "text" => {
                self.begin_passage();
                self.streamed_text(text(&event["content_block"], "text"), events);
            }
            "content_block_delta" if text(&event["delta"], "type") == "text_delta" => {
                self.streamed_text(text(&event["delta"], "text"), events);
            }
            _ => {}
        }
    }

    fn streamed_text(&mut self, piece: &str, events: &mut Vec<Event>) {
        if let Some(streaming) = &mut self.streaming {
            streaming.text.push_str(piece);
        }
        self.emit_text(piece, events);
    }

    /// A complete message from the model: one block of its reply.
    fn assistant(&mut self, value: &Value, events: &mut Vec<Event>) {
        let message = &value["message"];
        let blocks = message["content"].as_array().map(Vec::as_slice).unwrap_or_default();

        // When a request fails, the CLI writes the error as if the model had
        // said it. That is a failure to report, not a reply to show.
        if value["is_api_error_message"] == true || text(message, "model") == "<synthetic>" {
            let said: Vec<&str> = blocks.iter().map(|block| text(block, "text")).collect();
            self.api_error = Some((api_error_kind(text(value, "error")), said.concat()));
            return;
        }
        self.count_reply(text(message, "id"), false, events);

        for block in blocks {
            match text(block, "type") {
                "text" => {
                    let whole = text(block, "text");
                    // Already delivered piece by piece, unless the CLI did
                    // not stream this message.
                    let streamed = self
                        .streaming
                        .as_ref()
                        .is_some_and(|streaming| streaming.id == text(message, "id") && streaming.text.contains(whole));
                    if !streamed {
                        self.begin_passage();
                        self.emit_text(whole, events);
                    }
                }
                "tool_use" => {
                    let id = text(block, "id").to_owned();
                    let name = text(block, "name");
                    let input = &block["input"];
                    if !self.is_allowed(name) {
                        // Not on offer when the run began, and called all
                        // the same: nothing here is waited for.
                        let tools = vec![name.to_owned()];
                        self.verdict = Some(Verdict::Refused(FailureReason::UnexpectedTools { tools }));
                        return;
                    }
                    if name.starts_with(&self.tool_prefix) {
                        let tool = self.tool_name(name).to_owned();
                        self.tool_names.insert(id.clone(), tool.clone());
                        self.after_text = false;
                        events.push(Event::ToolCallStarted { id, tool, input: input.clone() });
                    } else if PLAN_TOOLS.contains(&name) {
                        // No line of its own, so what is said before and
                        // after it stays two passages.
                        self.plan_calls.insert(id, (name.to_owned(), input.clone()));
                    } else {
                        self.web_calls.insert(id.clone());
                        self.after_text = false;
                        events.push(Event::WebStarted { id, lookup: web_lookup(name, input) });
                    }
                }
                _ => {}
            }
        }
    }

    /// A message the CLI sends to the model on the user's behalf: the results
    /// of tool calls.
    fn user(&mut self, value: &Value, events: &mut Vec<Event>) {
        let blocks = value["message"]["content"].as_array().map(Vec::as_slice).unwrap_or_default();
        let results: Vec<&Value> = blocks.iter().filter(|block| text(block, "type") == "tool_result").collect();
        // Beside the words for the model, the CLI gives the result as data.
        // It belongs to the one result the line carries; with several on a
        // line there is no telling whose it is, and the words are read.
        let data = if results.len() == 1 { &value["tool_use_result"] } else { &Value::Null };
        for block in results {
            let id = text(block, "tool_use_id").to_owned();
            let said = match &block["content"] {
                Value::String(said) => said.clone(),
                Value::Array(parts) => parts.iter().map(|part| text(part, "text")).collect::<Vec<_>>().join("\n"),
                _ => String::new(),
            };
            let failed = block["is_error"] == true;
            if let Some((tool, input)) = self.plan_calls.remove(&id) {
                if !failed && let Some(change) = plan_change(&tool, &input, &said, data) {
                    events.push(Event::Plan(change));
                }
                continue;
            }
            let result = if failed { Err(said) } else { Ok(said) };
            self.after_text = false;
            if self.web_calls.remove(&id) {
                events.push(Event::WebFinished { id, result });
            } else {
                let tool = self.tool_names.remove(&id).unwrap_or_default();
                events.push(Event::ToolCallFinished { id, tool, result });
            }
        }
    }

    fn rate_limit(&mut self, info: &Value, events: &mut Vec<Event>) {
        let resets_at = info["resetsAt"].as_u64().map(|seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds));
        match text(info, "status") {
            "allowed_warning" => events.push(Event::Notice(Notice::UsageLimitNear { resets_at })),
            "rejected" => {
                self.limit_rejected = true;
                self.limit_resets_at = resets_at;
            }
            _ => {}
        }
    }

    fn result(&mut self, value: &Value) {
        let subtype = text(value, "subtype");
        let usage = &value["usage"];
        let figures = Figures {
            // The CLI's own count only for a run none of whose replies was
            // seen, which no CLI has been found to write.
            turns: if self.steps > 0 { self.steps } else { number(value, "num_turns") as u32 },
            duration: Duration::from_millis(number(value, "duration_ms")),
            usage: TokenUsage {
                input: number(usage, "input_tokens"),
                output: number(usage, "output_tokens"),
                cache_read: number(usage, "cache_read_input_tokens"),
                cache_write: number(usage, "cache_creation_input_tokens"),
            },
            cost: value["total_cost_usd"].as_f64(),
        };
        self.figures = Some(figures);
        let event = if subtype == "success" && value["is_error"] != true {
            Event::Finished(TurnSummary {
                turns: figures.turns,
                duration: figures.duration,
                usage: figures.usage,
                estimated_cost_usd: figures.cost,
            })
        } else {
            Event::Failed(self.failure(subtype, value))
        };
        self.verdict = Some(Verdict::Reported(event));
    }

    fn failure(&mut self, subtype: &str, value: &Value) -> FailureReason {
        let errors: Vec<&str> = value["errors"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if errors.iter().any(|error| error.contains("No conversation found")) {
            return FailureReason::SessionNotFound;
        }
        if subtype == "error_max_turns" {
            let limit = self.expect.max_turns.unwrap_or_else(|| number(value, "num_turns") as u32);
            return FailureReason::TurnLimitReached { limit };
        }

        let reported = Some(text(value, "result")).filter(|said| !said.is_empty()).map(str::to_owned);
        let (kind, message) = match self.api_error.take() {
            Some((kind, said)) => (kind, reported.unwrap_or(said)),
            None => (ApiErrorKind::Other, reported.unwrap_or_else(|| errors.join("; "))),
        };
        let kind = if self.limit_rejected { ApiErrorKind::RateLimit } else { kind };
        let message = if message.is_empty() { format!("the turn ended with {subtype}") } else { message };
        classify_api_error(kind, message, self.limit_resets_at)
    }

    fn begin_passage(&mut self) {
        self.break_pending = self.after_text;
    }

    fn emit_text(&mut self, piece: &str, events: &mut Vec<Event>) {
        if piece.is_empty() {
            return;
        }
        if std::mem::take(&mut self.break_pending) {
            events.push(Event::Text("\n\n".to_owned()));
        }
        self.after_text = true;
        events.push(Event::Text(piece.to_owned()));
    }

    /// Trill's own name for a tool: `mcp__trill__create_track` is
    /// `create_track`.
    fn tool_name<'a>(&self, full: &'a str) -> &'a str {
        full.strip_prefix(&self.tool_prefix).unwrap_or(full)
    }

    /// Whether `tool`, by the CLI's full name for it, is one the assistant
    /// may have: one of Trill's, or one of the CLI's own that was asked for.
    fn is_allowed(&self, tool: &str) -> bool {
        tool.starts_with(&self.tool_prefix) || self.expect.cli_tools.iter().any(|allowed| allowed == tool)
    }

    /// Counts a reply of the model's, once, however many lines speak of it,
    /// and says how far the request has come. A reply that is streamed is
    /// announced as it begins (`beginning`), and that is always a new one;
    /// its blocks then follow under the same id, as do those of a reply
    /// that was not streamed, which is counted at its first.
    fn count_reply(&mut self, id: &str, beginning: bool, events: &mut Vec<Event>) {
        if !beginning && (id.is_empty() || id == self.last_reply) {
            return;
        }
        id.clone_into(&mut self.last_reply);
        self.steps += 1;
        let before = self.expect.before;
        events.push(Event::Progress(Progress { steps: before.steps + self.steps, ..before }));
    }
}

/// What a call to one of the CLI's web tools looks up.
fn web_lookup(tool: &str, input: &Value) -> WebLookup {
    match tool {
        "WebSearch" => WebLookup::Search { query: text(input, "query").to_owned() },
        _ => WebLookup::Page { url: text(input, "url").to_owned(), question: text(input, "prompt").to_owned() },
    }
}

/// What a call to one of the CLI's plan tools did to the plan, read from
/// what it was given, what it answered in words (`said`) and, where the CLI
/// gave it, the answer as data. The shapes are those of CLI 2.1.325.
fn plan_change(tool: &str, input: &Value, said: &str, data: &Value) -> Option<PlanChange> {
    let given = |key: &str| Some(text(input, key)).filter(|value| !value.is_empty()).map(str::to_owned);
    match tool {
        "TaskCreate" => {
            // "Task #4 created successfully: Render and check"
            let numbered = || {
                let digits = said.split_once('#')?.1.split(|c: char| !c.is_ascii_digit()).next()?;
                Some(digits.to_owned()).filter(|digits| !digits.is_empty())
            };
            let id = Some(text(&data["task"], "id").to_owned()).filter(|id| !id.is_empty()).or_else(numbered)?;
            Some(PlanChange::Added { id, title: given("subject")?, doing: given("activeForm") })
        }
        "TaskUpdate" => {
            // A step that is not there answers "Task not found", as a result
            // like any other.
            if data["success"] == false || said.trim() == "Task not found" {
                return None;
            }
            let id = given("taskId")?;
            let status = match text(input, "status") {
                "deleted" => return Some(PlanChange::Removed { id }),
                status => step_status(status),
            };
            let (title, doing) = (given("subject"), given("activeForm"));
            // Who a step belongs to and what it waits for are not shown.
            let shown = status.is_some() || title.is_some() || doing.is_some();
            shown.then_some(PlanChange::Changed { id, status, title, doing })
        }
        "TaskList" => {
            let listed = |task: &Value| {
                let id = Some(text(task, "id")).filter(|id| !id.is_empty())?.to_owned();
                let (title, status) = (text(task, "subject").to_owned(), step_status(text(task, "status"))?);
                Some(PlanStep { id, title, doing: None, status })
            };
            match data["tasks"].as_array() {
                Some(tasks) => Some(PlanChange::Listed(tasks.iter().filter_map(listed).collect())),
                // "#2 [in_progress] Write the drums", a line each.
                None => {
                    let lines = said.lines().filter(|line| !line.trim().is_empty());
                    let steps: Option<Vec<PlanStep>> = lines.map(listed_line).collect();
                    steps.filter(|steps| !steps.is_empty()).map(PlanChange::Listed)
                }
            }
        }
        // The plan tool of an older CLI (2.1.94): the whole list every
        // time, without numbers. A step goes by its place in the list.
        "TodoWrite" => {
            let todos = input["todos"].as_array()?;
            let steps = todos.iter().enumerate().filter_map(|(index, todo)| {
                Some(PlanStep {
                    id: (index + 1).to_string(),
                    title: Some(text(todo, "content")).filter(|title| !title.is_empty())?.to_owned(),
                    doing: Some(text(todo, "activeForm")).filter(|doing| !doing.is_empty()).map(str::to_owned),
                    status: step_status(text(todo, "status"))?,
                })
            });
            Some(PlanChange::Listed(steps.collect()))
        }
        // Reading one step changes nothing.
        _ => None,
    }
}

fn step_status(word: &str) -> Option<StepStatus> {
    match word {
        "pending" => Some(StepStatus::Pending),
        "in_progress" => Some(StepStatus::InProgress),
        "completed" => Some(StepStatus::Done),
        _ => None,
    }
}

/// One line of the plan as the CLI lists it in words.
fn listed_line(line: &str) -> Option<PlanStep> {
    let (id, rest) = line.trim().strip_prefix('#')?.split_once(' ')?;
    let (status, title) = rest.strip_prefix('[')?.split_once("] ")?;
    Some(PlanStep { id: id.to_owned(), title: title.to_owned(), doing: None, status: step_status(status)? })
}

fn classify_api_error(kind: ApiErrorKind, message: String, resets_at: Option<SystemTime>) -> FailureReason {
    let lowered = message.to_lowercase();
    let names_a_limit = lowered.contains("limit")
        && ["hit your", "usage limit", "limit reached", "session limit", "weekly limit"]
            .iter()
            .any(|phrase| lowered.contains(phrase));
    match kind {
        ApiErrorKind::RateLimit => FailureReason::UsageLimitReached { message, resets_at },
        _ if names_a_limit => FailureReason::UsageLimitReached { message, resets_at },
        // Signed out, or a login that has expired: the CLI words both as a
        // request to log in.
        ApiErrorKind::Authentication if lowered.contains("login") || lowered.contains("log in") => {
            FailureReason::NotLoggedIn
        }
        ApiErrorKind::ModelNotFound => FailureReason::ModelUnavailable { message },
        kind => FailureReason::Api { kind, message },
    }
}

fn api_error_kind(name: &str) -> ApiErrorKind {
    match name {
        "authentication_failed" => ApiErrorKind::Authentication,
        "billing_error" => ApiErrorKind::Billing,
        "rate_limit" => ApiErrorKind::RateLimit,
        "invalid_request" => ApiErrorKind::InvalidRequest,
        "server_error" => ApiErrorKind::Server,
        "model_not_found" => ApiErrorKind::ModelNotFound,
        _ => ApiErrorKind::Other,
    }
}

/// The string at `key`, or `""` when it is missing or not a string.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// The non-negative number at `key`, or zero.
fn number(value: &Value, key: &str) -> u64 {
    let field = &value[key];
    field.as_u64().or_else(|| field.as_f64().map(|float| float.max(0.0) as u64)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SESSION: &str = "39367d22-e40b-4a04-9358-33e91538ff0d";

    const NOWHERE: Progress = Progress { steps: 0, estimated_cost_usd: None, costed_steps: 0 };

    /// What a run is told to expect when it was offered `cli_tools` of the
    /// CLI's own.
    fn expecting(cli_tools: &[&str]) -> Expectations {
        Expectations {
            mcp_server_name: "trill".into(),
            mcp_url: "http://127.0.0.1:7777/mcp".into(),
            cli_tools: cli_tools.iter().map(|tool| (*tool).to_owned()).collect(),
            max_turns: Some(12),
            before: NOWHERE,
        }
    }

    const EVERYTHING: [&str; 7] =
        ["TaskCreate", "TaskGet", "TaskList", "TaskUpdate", "TodoWrite", "WebFetch", "WebSearch"];

    /// A translator for a run with everything on: the plan and the web.
    fn translator() -> Translator {
        Translator::new(expecting(&EVERYTHING))
    }

    fn init(tools: &[&str], servers: Value) -> Value {
        json!({
            "type": "system", "subtype": "init", "session_id": SESSION, "model": "claude-haiku-5-5",
            "apiKeySource": "none", "tools": tools, "mcp_servers": servers,
        })
    }

    fn connected() -> Value {
        json!([{ "name": "trill", "status": "connected" }])
    }

    /// What `lines` mean, with how far the request has come at each reply.
    fn feed_all(translator: &mut Translator, lines: &[Value]) -> Vec<Event> {
        let mut events = Vec::new();
        for line in lines {
            translator.line(line.to_string().as_bytes(), &mut events);
        }
        events
    }

    /// What `lines` mean, without the progress reports.
    fn feed(translator: &mut Translator, lines: &[Value]) -> Vec<Event> {
        let mut events = feed_all(translator, lines);
        events.retain(|event| !matches!(event, Event::Progress(_)));
        events
    }

    fn calls(id: &str, tool: &str, input: Value) -> Value {
        json!({"type":"assistant","message":{"id":format!("msg_{id}"),"content":[
            {"type":"tool_use","id":id,"name":tool,"input":input}]}})
    }

    /// A tool's answer as CLI 2.1.325 writes it: the words for the model,
    /// and beside them the result as data.
    fn answers(id: &str, said: &str, data: Value) -> Value {
        json!({"type":"user","tool_use_result":data,"message":{"content":[
            {"type":"tool_result","tool_use_id":id,"content":said}]}})
    }

    fn lines_of(splitter: &mut LineSplitter, chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();
        for chunk in chunks {
            splitter.push(chunk, |line| lines.push(line.to_vec())).expect("short lines");
        }
        splitter.finish(|line| lines.push(line.to_vec()));
        lines
    }

    #[test]
    fn lines_are_cut_wherever_the_reads_fall() {
        let lines = lines_of(&mut LineSplitter::default(), &[b"one\ntw", b"o\n", b"\nthr", b"ee"]);
        assert_eq!(lines, [b"one".to_vec(), b"two".to_vec(), b"".to_vec(), b"three".to_vec()]);
    }

    #[test]
    fn a_character_split_across_reads_arrives_whole() {
        let line = "{\"text\":\"caf\u{e9} \u{1f3b9}\"}\n".as_bytes();
        for cut in 1..line.len() {
            let lines = lines_of(&mut LineSplitter::default(), &[&line[..cut], &line[cut..]]);
            assert_eq!(lines, [line[..line.len() - 1].to_vec()], "cut at byte {cut}");
        }
    }

    #[test]
    fn a_line_without_end_is_refused_before_it_exhausts_memory() {
        let mut splitter = LineSplitter::default();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut outcome = Ok(());
        for _ in 0..=MAX_LINE_BYTES / chunk.len() {
            outcome = splitter.push(&chunk, |_| panic!("no line ends"));
            if outcome.is_err() {
                break;
            }
        }
        assert_eq!(outcome, Err(LineTooLong));
    }

    #[test]
    fn a_turn_starts_when_trill_is_connected_and_alone() {
        let mut translator = translator();
        let events = feed(&mut translator, &[init(&["mcp__trill__get_project"], connected())]);

        assert_eq!(
            events,
            [Event::Started(TurnStart {
                session_id: SessionId::parse(SESSION).unwrap(),
                model: "claude-haiku-5-5".into(),
                api_key_source: None,
            })]
        );
        assert_eq!(translator.verdict(), None);
    }

    #[test]
    fn an_api_key_in_use_is_reported() {
        let mut line = init(&[], connected());
        line["apiKeySource"] = json!("ANTHROPIC_API_KEY");
        let events = feed(&mut translator(), &[line]);

        let [Event::Started(start)] = events.as_slice() else { panic!("expected a start, got {events:?}") };
        assert_eq!(start.api_key_source.as_deref(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn a_turn_is_refused_when_trill_is_not_connected() {
        for (servers, status) in
            [(json!([{ "name": "trill", "status": "failed" }]), "failed"), (json!([]), "not loaded")]
        {
            let mut translator = translator();
            let events = feed(&mut translator, &[init(&[], servers)]);

            assert_eq!(events, []);
            assert_eq!(
                translator.verdict(),
                Some(&Verdict::Refused(FailureReason::McpServerUnavailable {
                    url: "http://127.0.0.1:7777/mcp".into(),
                    status: status.into(),
                }))
            );
        }
    }

    #[test]
    fn a_turn_is_refused_when_anything_but_trills_tools_is_on_offer() {
        let mut translator = translator();
        let servers = json!([{ "name": "trill", "status": "connected" }, { "name": "mail", "status": "connected" }]);
        feed(&mut translator, &[init(&["Bash", "mcp__trill__render", "mcp__mail__send"], servers)]);

        assert_eq!(
            translator.verdict(),
            Some(&Verdict::Refused(FailureReason::UnexpectedTools {
                tools: vec!["Bash".into(), "mcp__mail__send".into(), "MCP server mail".into()],
            }))
        );
    }

    #[test]
    fn the_clis_own_tools_are_let_through_only_if_they_were_asked_for() {
        let on_offer = [
            "mcp__trill__render",
            "TaskCreate",
            "TaskGet",
            "TaskList",
            "TaskUpdate",
            "TodoWrite",
            "WebFetch",
            "WebSearch",
        ];

        // Everything on: the run starts.
        let mut everything = translator();
        let events = feed(&mut everything, &[init(&on_offer, connected())]);
        assert!(matches!(events.as_slice(), [Event::Started(_)]), "{events:?}");
        assert_eq!(everything.verdict(), None);

        // The web switched off, and a CLI that offers it regardless.
        let mut offline = Translator::new(expecting(&["TaskCreate", "TaskGet", "TaskList", "TaskUpdate", "TodoWrite"]));
        assert_eq!(feed(&mut offline, &[init(&on_offer, connected())]), []);
        let refused = FailureReason::UnexpectedTools { tools: vec!["WebFetch".into(), "WebSearch".into()] };
        assert_eq!(offline.verdict(), Some(&Verdict::Refused(refused)));

        // Nothing of the CLI's asked for: none of it may be there.
        let mut bare = Translator::new(expecting(&[]));
        feed(&mut bare, &[init(&["mcp__trill__render", "TaskCreate"], connected())]);
        let refused = FailureReason::UnexpectedTools { tools: vec!["TaskCreate".into()] };
        assert_eq!(bare.verdict(), Some(&Verdict::Refused(refused)));

        // And a tool of the CLI's that was never asked for is never let
        // through, whatever else is on.
        for stranger in ["Read", "Write", "Edit", "Bash", "PowerShell", "NotebookEdit", "Task", "Skill", "Agent"] {
            let mut translator = translator();
            feed(&mut translator, &[init(&["mcp__trill__render", "WebSearch", stranger], connected())]);
            let refused = FailureReason::UnexpectedTools { tools: vec![stranger.to_owned()] };
            assert_eq!(translator.verdict(), Some(&Verdict::Refused(refused)), "{stranger}");
        }
    }

    #[test]
    fn a_tool_that_was_not_on_offer_stops_the_run_when_it_is_called() {
        // The list at the start is what is checked; a call to something
        // that was not on it is not waited for either.
        let mut translator = translator();
        let events = feed(
            &mut translator,
            &[
                init(&["mcp__trill__render", "WebSearch"], connected()),
                calls("t1", "mcp__trill__render", json!({})),
                calls("t2", "Bash", json!({ "command": "whoami" })),
                calls("t3", "mcp__trill__render", json!({})),
            ],
        );
        assert_eq!(events.len(), 2, "the start and the one call that was Trill's: {events:?}");
        let refused = FailureReason::UnexpectedTools { tools: vec!["Bash".into()] };
        assert_eq!(translator.verdict(), Some(&Verdict::Refused(refused)));
    }

    #[test]
    fn the_plan_tools_are_reported_as_changes_to_the_plan_and_not_as_calls() {
        // As CLI 2.1.325 answered each of these, called through it.
        let created = |id: &str, title: &str| json!({ "task": { "id": id, "subject": title } });
        let updated = |id: &str| json!({ "success": true, "taskId": id });
        let events = feed(
            &mut translator(),
            &[
                calls(
                    "p1",
                    "TaskCreate",
                    json!({"subject":"Set tempo and key","description":"96 BPM","activeForm":"Setting tempo and key"}),
                ),
                answers("p1", "Task #1 created successfully: Set tempo and key", created("1", "Set tempo and key")),
                calls("p2", "TaskCreate", json!({"subject":"Write the drums","description":"House groove"})),
                answers("p2", "Task #2 created successfully: Write the drums", created("2", "Write the drums")),
                calls("p3", "TaskUpdate", json!({"taskId":"1","status":"in_progress"})),
                answers("p3", "Updated task #1 status", updated("1")),
                calls("p4", "TaskUpdate", json!({"taskId":"1","status":"completed"})),
                answers("p4", "Updated task #1 status", updated("1")),
                calls(
                    "p5",
                    "TaskUpdate",
                    json!({"taskId":"2","subject":"Write the drums and a fill","activeForm":"Writing the drums"}),
                ),
                answers("p5", "Updated task #2 subject", updated("2")),
                calls("p6", "TaskUpdate", json!({"taskId":"2","status":"deleted"})),
                answers("p6", "Updated task #2 deleted", updated("2")),
                // What changes nothing that is shown is not reported.
                calls("p7", "TaskUpdate", json!({"taskId":"1","owner":"someone","addBlockedBy":["2"]})),
                answers("p7", "Updated task #1 owner, blockedBy", updated("1")),
                calls("p8", "TaskGet", json!({"taskId":"1"})),
                answers("p8", "Task #1: Set tempo and key", json!({"task":{"id":"1"}})),
                // Nor a step that is not there, which the CLI answers as a
                // result like any other.
                calls("p9", "TaskUpdate", json!({"taskId":"../../x","status":"in_progress"})),
                answers("p9", "Task not found", json!({"success":false,"taskId":"../../x","error":"Task not found"})),
                calls("p10", "TaskList", json!({})),
                answers(
                    "p10",
                    "#1 [completed] Set tempo and key\n#4 [pending] Render",
                    json!({"tasks":[
                        {"id":"1","subject":"Set tempo and key","status":"completed","blockedBy":[]},
                        {"id":"4","subject":"Render","status":"pending","blockedBy":[]}]}),
                ),
            ],
        );
        let step = |id: &str, title: &str, status| PlanStep { id: id.into(), title: title.into(), doing: None, status };
        let changed = |id: &str, status| PlanChange::Changed { id: id.into(), status, title: None, doing: None };
        assert_eq!(
            events,
            [
                Event::Plan(PlanChange::Added {
                    id: "1".into(),
                    title: "Set tempo and key".into(),
                    doing: Some("Setting tempo and key".into()),
                }),
                Event::Plan(PlanChange::Added { id: "2".into(), title: "Write the drums".into(), doing: None }),
                Event::Plan(changed("1", Some(StepStatus::InProgress))),
                Event::Plan(changed("1", Some(StepStatus::Done))),
                Event::Plan(PlanChange::Changed {
                    id: "2".into(),
                    status: None,
                    title: Some("Write the drums and a fill".into()),
                    doing: Some("Writing the drums".into()),
                }),
                Event::Plan(PlanChange::Removed { id: "2".into() }),
                Event::Plan(PlanChange::Listed(vec![
                    step("1", "Set tempo and key", StepStatus::Done),
                    step("4", "Render", StepStatus::Pending),
                ])),
            ]
        );
    }

    #[test]
    fn the_plan_is_read_from_the_words_when_the_answer_comes_without_data() {
        let bare = |id: &str, said: &str| json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":said}]}});
        let events = feed(
            &mut translator(),
            &[
                calls("p1", "TaskCreate", json!({"subject":"Write the bass","description":"Roots"})),
                bare("p1", "Task #12 created successfully: Write the bass"),
                calls("p2", "TaskList", json!({})),
                bare("p2", "#11 [completed] Write the drums\n#12 [in_progress] Write the bass"),
                // An answer that cannot be read is no change, and no harm.
                calls("p3", "TaskCreate", json!({"subject":"Lost"})),
                bare("p3", "Something else entirely"),
                calls("p4", "TaskList", json!({})),
                bare("p4", "No tasks found"),
                // One that failed changed nothing.
                calls("p5", "TaskUpdate", json!({"taskId":"12","status":"completed"})),
                json!({"type":"user","message":{"content":[
                    {"type":"tool_result","tool_use_id":"p5","content":"Permission denied","is_error":true}]}}),
            ],
        );
        assert_eq!(
            events,
            [
                Event::Plan(PlanChange::Added { id: "12".into(), title: "Write the bass".into(), doing: None }),
                Event::Plan(PlanChange::Listed(vec![
                    PlanStep {
                        id: "11".into(),
                        title: "Write the drums".into(),
                        doing: None,
                        status: StepStatus::Done
                    },
                    PlanStep {
                        id: "12".into(),
                        title: "Write the bass".into(),
                        doing: None,
                        status: StepStatus::InProgress
                    },
                ])),
            ]
        );
    }

    #[test]
    fn the_plan_tool_of_an_older_cli_gives_the_whole_plan_each_time() {
        // As CLI 2.1.94 answered it, called through it.
        let said = "Todos have been modified successfully. Ensure that you continue to use the todo list to track \
                    your progress. Please proceed with the current tasks if applicable";
        let events = feed(
            &mut translator(),
            &[
                calls(
                    "p1",
                    "TodoWrite",
                    json!({"todos":[
                        {"content":"Set tempo and key","status":"completed","activeForm":"Setting tempo and key"},
                        {"content":"Write the drums","status":"in_progress","activeForm":"Writing the drums"},
                        {"content":"Render","status":"pending"},
                        {"content":"","status":"pending"},
                        {"content":"Not a state","status":"paused"}]}),
                ),
                answers("p1", said, json!({"oldTodos":[],"newTodos":[]})),
                // One that was refused changed nothing.
                calls("p2", "TodoWrite", json!({"todos":[]})),
                json!({"type":"user","message":{"content":[
                    {"type":"tool_result","tool_use_id":"p2","content":"Permission denied","is_error":true}]}}),
            ],
        );
        let step = |id: &str, title: &str, doing: Option<&str>, status| PlanStep {
            id: id.into(),
            title: title.into(),
            doing: doing.map(str::to_owned),
            status,
        };
        assert_eq!(
            events,
            [Event::Plan(PlanChange::Listed(vec![
                step("1", "Set tempo and key", Some("Setting tempo and key"), StepStatus::Done),
                step("2", "Write the drums", Some("Writing the drums"), StepStatus::InProgress),
                step("3", "Render", None, StepStatus::Pending),
            ]))]
        );
    }

    #[test]
    fn what_is_said_around_a_step_of_the_plan_stays_two_passages() {
        let said = |id: &str, words: &str| json!({"type":"assistant","message":{"id":id,"content":[{"type":"text","text":words}]}});
        let events = feed(
            &mut translator(),
            &[
                said("a", "Here is the plan."),
                calls("p1", "TaskCreate", json!({"subject":"Write the drums","description":"x"})),
                answers("p1", "Task #1 created successfully: Write the drums", json!({"task":{"id":"1"}})),
                said("b", "Now the drums."),
            ],
        );
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|event| if let Event::Text(piece) = event { Some(piece.as_str()) } else { None })
            .collect();
        assert_eq!(texts, ["Here is the plan.", "\n\n", "Now the drums."]);
    }

    #[test]
    fn web_lookups_are_reported_with_the_query_or_the_address() {
        let found = "Web search results for query: \"dorian mode chord progressions\"";
        let events = feed(
            &mut translator(),
            &[
                calls("w1", "WebSearch", json!({"query":"dorian mode chord progressions"})),
                answers("w1", found, json!({"results":[]})),
                calls(
                    "w2",
                    "WebFetch",
                    json!({"url":"https://example.com/dorian","prompt":"Which chords are typical?"}),
                ),
                json!({"type":"user","message":{"content":[
                    {"type":"tool_result","tool_use_id":"w2","content":"getaddrinfo ENOTFOUND example.com","is_error":true}]}}),
            ],
        );
        assert_eq!(
            events,
            [
                Event::WebStarted {
                    id: "w1".into(),
                    lookup: WebLookup::Search { query: "dorian mode chord progressions".into() }
                },
                Event::WebFinished { id: "w1".into(), result: Ok(found.into()) },
                Event::WebStarted {
                    id: "w2".into(),
                    lookup: WebLookup::Page {
                        url: "https://example.com/dorian".into(),
                        question: "Which chords are typical?".into()
                    }
                },
                Event::WebFinished { id: "w2".into(), result: Err("getaddrinfo ENOTFOUND example.com".into()) },
            ]
        );
    }

    #[test]
    fn each_reply_of_the_model_is_a_step_counted_once_from_where_the_request_stood() {
        let before = Progress { steps: 200, estimated_cost_usd: Some(2.5), costed_steps: 200 };
        let mut translator = Translator::new(Expectations { before, ..expecting(&EVERYTHING) });
        let said = |id: &str| json!({"type":"assistant","message":{"id":id,"content":[{"type":"text","text":"x"}]}});
        let events = feed_all(
            &mut translator,
            &[
                // Streamed: announced as it begins, then given whole, block
                // by block, under the same id.
                json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1"}}}),
                said("msg_1"),
                said("msg_1"),
                // Not streamed: counted when it is given.
                said("msg_2"),
                // An error written as if the model had said it is no step.
                json!({"type":"assistant","error":"server_error","is_api_error_message":true,
                       "message":{"id":"msg_3","model":"<synthetic>","content":[{"type":"text","text":"529"}]}}),
            ],
        );
        let steps: Vec<Progress> = events
            .iter()
            .filter_map(|event| if let Event::Progress(progress) = event { Some(*progress) } else { None })
            .collect();
        assert_eq!(steps.iter().map(|progress| progress.steps).collect::<Vec<_>>(), [201, 202]);
        assert!(steps.iter().all(|progress| progress.estimated_cost_usd == Some(2.5) && progress.costed_steps == 200));
    }

    #[test]
    fn a_conversation_that_outgrows_the_model_is_reported_as_it_is_summarised() {
        // As CLI 2.1.325 wrote it, run with `-p`, when the replies said the
        // conversation had reached 985,053 tokens of a million.
        let events = feed(
            &mut translator(),
            &[
                json!({"type":"system","subtype":"status","status":"requesting"}),
                json!({"type":"system","subtype":"status","status":"compacting","compact_output_chars":0}),
                json!({"type":"system","subtype":"status","status":null,"compact_result":"success"}),
                json!({"type":"system","subtype":"compact_boundary","compact_metadata":{
                    "trigger":"auto","pre_tokens":985053,"post_tokens":644,"cumulative_dropped_tokens":984409}}),
                // What the CLI then tells the model is nothing to show.
                json!({"type":"user","isSynthetic":true,"message":{"content":[
                    {"type":"text","text":"This session is being continued from a previous conversation"}]}}),
                json!({"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"too long"}),
            ],
        );
        assert_eq!(
            events,
            [
                Event::Notice(Notice::Compacting),
                Event::Notice(Notice::Compacted { tokens_before: Some(985_053), tokens_after: Some(644) }),
                Event::Notice(Notice::CompactionFailed { detail: "failed: too long".into() }),
            ]
        );
    }

    #[test]
    fn streamed_text_is_not_repeated_when_the_whole_message_follows() {
        let mut translator = translator();
        let events = feed(
            &mut translator,
            &[
                json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text","text":""}}}),
                json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}),
                json!({"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"Hello"}]}}),
            ],
        );
        assert_eq!(events, [Event::Text("Hel".into()), Event::Text("lo".into())]);
    }

    #[test]
    fn text_that_was_never_streamed_is_still_delivered() {
        let message = json!({"type":"assistant","message":{"id":"msg_9","content":[{"type":"text","text":"Hello"}]}});
        assert_eq!(feed(&mut translator(), &[message]), [Event::Text("Hello".into())]);
    }

    #[test]
    fn passages_of_text_are_separated_unless_a_tool_call_comes_between() {
        let said = |id: &str, words: &str| json!({"type":"assistant","message":{"id":id,"content":[{"type":"text","text":words}]}});
        let call = json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t1","name":"mcp__trill__transport","input":{}}]}});

        let events = feed(&mut translator(), &[said("a", "One."), said("b", "Two."), call, said("c", "Three.")]);

        let texts: Vec<&str> = events
            .iter()
            .filter_map(|event| if let Event::Text(piece) = event { Some(piece.as_str()) } else { None })
            .collect();
        assert_eq!(texts, ["One.", "\n\n", "Two.", "Three."]);
    }

    #[test]
    fn tool_calls_are_named_as_trill_names_them() {
        let events = feed(
            &mut translator(),
            &[
                json!({"type":"assistant","message":{"id":"m","content":[
                    {"type":"tool_use","id":"t1","name":"mcp__trill__set_project","input":{"tempo":97}}]}}),
                json!({"type":"user","message":{"content":[
                    {"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"{\"tempo\":97.0}"}]}]}}),
                json!({"type":"user","message":{"content":[
                    {"type":"tool_result","tool_use_id":"t2","content":"no such clip","is_error":true}]}}),
            ],
        );
        assert_eq!(
            events,
            [
                Event::ToolCallStarted { id: "t1".into(), tool: "set_project".into(), input: json!({"tempo": 97}) },
                Event::ToolCallFinished {
                    id: "t1".into(),
                    tool: "set_project".into(),
                    result: Ok("{\"tempo\":97.0}".into())
                },
                Event::ToolCallFinished { id: "t2".into(), tool: String::new(), result: Err("no such clip".into()) },
            ]
        );
    }

    #[test]
    fn retries_and_denials_become_notices() {
        let events = feed(
            &mut translator(),
            &[
                json!({"type":"system","subtype":"api_retry","attempt":2,"max_retries":10,"retry_delay_ms":1222,"error":"unknown"}),
                json!({"type":"system","subtype":"api_retry","attempt":3,"max_retries":10,"retry_delay_ms":2000.5,"error":"rate_limit"}),
                json!({"type":"system","subtype":"permission_denied","tool_name":"mcp__trill__set_project","tool_use_id":"t"}),
                json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1795165200}}),
                json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1795165200}}),
            ],
        );
        assert_eq!(
            events,
            [
                Event::Notice(Notice::Retrying {
                    attempt: 2,
                    max_attempts: 10,
                    delay: Duration::from_millis(1222),
                    cause: ApiErrorKind::Other,
                }),
                Event::Notice(Notice::RateLimited { attempt: 3, max_attempts: 10, delay: Duration::from_millis(2000) }),
                Event::Notice(Notice::PermissionDenied { tool: "set_project".into() }),
                Event::Notice(Notice::UsageLimitNear {
                    resets_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1795165200)),
                }),
            ]
        );
    }

    #[test]
    fn what_is_not_understood_is_skipped() {
        let mut translator = translator();
        let mut events = Vec::new();
        for line in [
            &b"not json at all"[..],
            b"",
            b"[1, 2, 3]",
            b"\xff\xfe broken utf-8",
            br#"{"type":"hologram","payload":{"depth":3}}"#,
            br#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":50}"#,
            br#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hm"}}}"#,
            br#"{"type":"assistant","message":{"content":[{"type":"hologram"}]},"new_field":true}"#,
        ] {
            translator.line(line, &mut events);
        }
        assert_eq!(events, []);
        assert_eq!(translator.verdict(), None);
    }

    #[test]
    fn a_successful_result_ends_the_turn_with_its_figures() {
        let mut translator = translator();
        feed(
            &mut translator,
            &[json!({
                "type":"result","subtype":"success","is_error":false,"num_turns":2,"duration_ms":2914,
                "total_cost_usd":0.0035352,
                "usage":{"input_tokens":4,"output_tokens":90,"cache_read_input_tokens":16460,"cache_creation_input_tokens":16626},
            })],
        );
        let summary = TurnSummary {
            turns: 2,
            duration: Duration::from_millis(2914),
            usage: TokenUsage { input: 4, output: 90, cache_read: 16460, cache_write: 16626 },
            estimated_cost_usd: Some(0.0035352),
        };
        assert_eq!(translator.verdict(), Some(&Verdict::Reported(Event::Finished(summary.clone()))));
        // A cancel that arrives after the CLI finished does not undo that.
        assert_eq!(translator.finish(true, Exit::default()), Event::Finished(summary));
    }

    fn failure_of(lines: &[Value]) -> FailureReason {
        let mut translator = translator();
        feed(&mut translator, lines);
        match translator.finish(false, Exit { code: Some(1), stderr_tail: String::new() }) {
            Event::Failed(reason) => reason,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn api_error(kind: &str, said: &str) -> [Value; 2] {
        [
            json!({"type":"assistant","error":kind,"is_api_error_message":true,
                   "message":{"model":"<synthetic>","content":[{"type":"text","text":said}]}}),
            json!({"type":"result","subtype":"success","is_error":true,"result":said}),
        ]
    }

    #[test]
    fn a_missing_login_is_recognised_and_not_shown_as_a_reply() {
        let lines = api_error("authentication_failed", "Not logged in \u{b7} Please run /login");

        assert_eq!(feed(&mut translator(), &lines), []);
        assert_eq!(failure_of(&lines), FailureReason::NotLoggedIn);
    }

    #[test]
    fn a_rejected_key_is_not_mistaken_for_a_missing_login() {
        let said = "Invalid API key \u{b7} Fix external API key";
        assert_eq!(
            failure_of(&api_error("authentication_failed", said)),
            FailureReason::Api { kind: ApiErrorKind::Authentication, message: said.into() }
        );
    }

    #[test]
    fn a_model_the_service_does_not_know_is_a_failure_of_its_own() {
        // As CLI 2.1.325 reported `--model=not-a-model-name`: the name is
        // passed on, the service answers 404, and nothing is used.
        let said = "There's an issue with the selected model (not-a-model-name). It may not exist or you may not \
                    have access to it. Run --model to pick a different model.";
        let mut start = init(&["mcp__trill__get_project"], connected());
        start["model"] = json!("not-a-model-name");
        let lines = [
            start,
            json!({"type":"assistant","error":"model_not_found","is_api_error_message":true,
                   "message":{"model":"<synthetic>","content":[{"type":"text","text":said}]}}),
            json!({"type":"result","subtype":"success","is_error":true,"result":said,"num_turns":1,
                   "total_cost_usd":0,"api_error_status":404}),
        ];
        let events = feed(&mut translator(), &lines);
        let [Event::Started(start)] = events.as_slice() else { panic!("expected a start only, got {events:?}") };
        assert_eq!(start.model, "not-a-model-name", "the name is reported back as it was given");
        assert_eq!(failure_of(&lines), FailureReason::ModelUnavailable { message: said.into() });
    }

    #[test]
    fn a_usage_limit_is_recognised_by_kind_by_event_and_by_wording() {
        let said = "You've hit your session limit \u{b7} resets 3pm";
        let reached = |resets_at| FailureReason::UsageLimitReached { message: said.into(), resets_at };

        assert_eq!(failure_of(&api_error("rate_limit", said)), reached(None));
        assert_eq!(failure_of(&api_error("unknown", said)), reached(None));

        let resets_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1795165200);
        let mut lines =
            vec![json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1795165200}})];
        lines.extend(api_error("unknown", "Request refused"));
        assert_eq!(
            failure_of(&lines),
            FailureReason::UsageLimitReached { message: "Request refused".into(), resets_at: Some(resets_at) }
        );
    }

    #[test]
    fn other_error_results_are_typed() {
        let lost = json!({"type":"result","subtype":"error_during_execution","is_error":true,
                          "errors":["No conversation found with session ID: 1111"]});
        assert_eq!(failure_of(&[lost]), FailureReason::SessionNotFound);

        let limit = json!({"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":13,
                           "total_cost_usd":0.25,"duration_ms":9000,"usage":{"output_tokens":7}});
        assert_eq!(failure_of(std::slice::from_ref(&limit)), FailureReason::TurnLimitReached { limit: 12 });
        // What the run took is known however it ended.
        let mut stopped = translator();
        feed(&mut stopped, &[limit]);
        let took = Figures {
            turns: 13,
            duration: Duration::from_secs(9),
            usage: TokenUsage { output: 7, ..TokenUsage::default() },
            cost: Some(0.25),
        };
        assert_eq!(stopped.figures(), Some(took));
        assert_eq!(translator().figures(), None);

        let overloaded = api_error("server_error", "API Error: 529 Overloaded");
        assert_eq!(
            failure_of(&overloaded),
            FailureReason::Api { kind: ApiErrorKind::Server, message: "API Error: 529 Overloaded".into() }
        );

        let bare = json!({"type":"result","subtype":"error_during_execution","is_error":true});
        assert_eq!(
            failure_of(&[bare]),
            FailureReason::Api {
                kind: ApiErrorKind::Other,
                message: "the turn ended with error_during_execution".into()
            }
        );
    }

    #[test]
    fn an_ending_without_a_result_is_explained_by_how_the_process_ended() {
        let exit = |code| Exit { code, stderr_tail: "error: unknown option '--frobnicate'".into() };

        // Never got going: the CLI's own words are the best explanation.
        assert_eq!(
            translator().finish(false, exit(Some(1))),
            Event::Failed(FailureReason::CliExited {
                exit_code: Some(1),
                stderr_tail: "error: unknown option '--frobnicate'".into(),
            })
        );

        // Cancelled: nothing went wrong.
        assert_eq!(translator().finish(true, exit(None)), Event::Cancelled);

        // Started, exited cleanly, and never said how the turn ended.
        let mut started = translator();
        feed(&mut started, &[init(&[], connected())]);
        assert!(matches!(started.finish(false, exit(Some(0))), Event::Failed(FailureReason::Protocol { .. })));

        // Refused at the start, then stopped by us: the refusal is the reason.
        let mut refused = translator();
        feed(&mut refused, &[init(&[], json!([{ "name": "trill", "status": "failed" }]))]);
        assert!(matches!(refused.finish(false, exit(None)), Event::Failed(FailureReason::McpServerUnavailable { .. })));
    }

    #[test]
    fn nothing_is_read_after_the_turn_has_ended() {
        let mut translator = translator();
        let done = json!({"type":"result","subtype":"success","is_error":false});
        let late = json!({"type":"assistant","message":{"id":"x","content":[{"type":"text","text":"late"}]}});
        assert_eq!(feed(&mut translator, &[done, late]), []);
    }
}
