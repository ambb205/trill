//! Reading the CLI's `stream-json` output: one JSON object per line.
//!
//! Everything here is plain data in, plain data out, so that the whole
//! pipeline can be tested from recorded output without starting a process.
//!
//! The CLI's output grows with every release. Lines of a type this module
//! does not know, fields it does not know, and lines that are not JSON at all
//! are skipped; only the handful of fields named below are relied on. The
//! shapes were recorded from CLI 2.1.325 (see `tests/fixtures`).

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::event::{ApiErrorKind, Event, FailureReason, Notice, SessionId, TokenUsage, TurnStart, TurnSummary};

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

/// What the translator needs to know about the conversation it is reading.
#[derive(Debug, Clone)]
pub(crate) struct Expectations {
    pub(crate) mcp_server_name: String,
    pub(crate) mcp_url: String,
    pub(crate) max_turns: Option<u32>,
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
        let mut unexpected: Vec<String> = tools
            .iter()
            .filter_map(Value::as_str)
            .filter(|tool| !tool.starts_with(&self.tool_prefix))
            .map(str::to_owned)
            .collect();
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
                self.streaming =
                    Some(StreamingMessage { id: text(&event["message"], "id").to_owned(), text: String::new() });
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
                    let tool = self.tool_name(text(block, "name")).to_owned();
                    self.tool_names.insert(id.clone(), tool.clone());
                    self.after_text = false;
                    events.push(Event::ToolCallStarted { id, tool, input: block["input"].clone() });
                }
                _ => {}
            }
        }
    }

    /// A message the CLI sends to the model on the user's behalf: the results
    /// of tool calls.
    fn user(&mut self, value: &Value, events: &mut Vec<Event>) {
        let blocks = value["message"]["content"].as_array().map(Vec::as_slice).unwrap_or_default();
        for block in blocks.iter().filter(|block| text(block, "type") == "tool_result") {
            let id = text(block, "tool_use_id").to_owned();
            let tool = self.tool_names.remove(&id).unwrap_or_default();
            let said = match &block["content"] {
                Value::String(said) => said.clone(),
                Value::Array(parts) => parts.iter().map(|part| text(part, "text")).collect::<Vec<_>>().join("\n"),
                _ => String::new(),
            };
            let result = if block["is_error"] == true { Err(said) } else { Ok(said) };
            self.after_text = false;
            events.push(Event::ToolCallFinished { id, tool, result });
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
        let event = if subtype == "success" && value["is_error"] != true {
            let usage = &value["usage"];
            Event::Finished(TurnSummary {
                turns: number(value, "num_turns") as u32,
                duration: Duration::from_millis(number(value, "duration_ms")),
                usage: TokenUsage {
                    input: number(usage, "input_tokens"),
                    output: number(usage, "output_tokens"),
                    cache_read: number(usage, "cache_read_input_tokens"),
                    cache_write: number(usage, "cache_creation_input_tokens"),
                },
                estimated_cost_usd: value["total_cost_usd"].as_f64(),
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

    fn translator() -> Translator {
        Translator::new(Expectations {
            mcp_server_name: "trill".into(),
            mcp_url: "http://127.0.0.1:7777/mcp".into(),
            max_turns: Some(12),
        })
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

    fn feed(translator: &mut Translator, lines: &[Value]) -> Vec<Event> {
        let mut events = Vec::new();
        for line in lines {
            translator.line(line.to_string().as_bytes(), &mut events);
        }
        events
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

        let limit = json!({"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":13});
        assert_eq!(failure_of(&[limit]), FailureReason::TurnLimitReached { limit: 12 });

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
