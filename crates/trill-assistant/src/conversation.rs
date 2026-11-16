//! A conversation with the assistant: one CLI process per message.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::config::{AssistantConfig, ConfigError};
use crate::detect::{CliInstallation, CliVersion};
use crate::env;
use crate::event::{Event, FailureReason, Notice, Progress, SessionId, TokenUsage, TurnSummary};
use crate::process::{self, ProcessTree};
use crate::protocol::{Exit, Expectations, Figures, LineSplitter, MAX_LINE_BYTES, Translator, Verdict};

/// The first CLI version checked to accept `--permission-prompts`; 2.1.94
/// does not know the flag. It is a second lock on a door `--permission-mode
/// dontAsk` already shuts, so older versions simply go without.
const PERMISSION_PROMPTS_SINCE: CliVersion = CliVersion::new(2, 1, 325);

/// The first CLI version checked to take a `timeout` for one MCP server in
/// the file that names it. With 2.1.325 that alone let a call of 310 s
/// through. An older CLI is not shown a field it may not know, and has the
/// environment (`env.rs`) to go by.
const SERVER_TIMEOUT_SINCE: CliVersion = CliVersion::new(2, 1, 325);

/// How often the turn's thread looks up from waiting for output, which
/// bounds how long a cancel takes to be noticed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long the CLI may take to exit after reporting the end of a turn. It
/// is still saving the conversation, so it is not hurried; past this it is
/// stopped so that a hung process cannot block the next message.
const EXIT_GRACE: Duration = Duration::from_secs(15);

/// How long output is still read after the CLI itself has exited, in case a
/// helper it started is holding the pipe open.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// How much of the CLI's error stream is kept for a failure report.
const STDERR_TAIL_BYTES: usize = 4096;

/// What the CLI is given in place of a message when a request is carried on
/// in another run. The system prompt says what it means; the words here are
/// for a model that has not read that far.
const CONTINUATION: &str = "\
<trill-continue>
Trill is writing this, not the user. Your work on the user's last request was paused at a checkpoint, and nothing is \
wrong: the user has said nothing new and is still waiting for the same request. Carry on exactly where you stopped. \
Do not greet, recap, start over or re-read what you already know; your plan is as you left it. When everything that \
was asked for is done, close the request as you normally would.
</trill-continue>";

/// A message could not be sent. Both cases are mistakes in the calling code,
/// not conditions to show the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// The previous turn's terminal event has not been taken from
    /// [`Conversation::poll`] yet. Cancel it, or wait for it, first.
    #[error("the assistant is still working on the previous message")]
    TurnInProgress,
    #[error("the message is empty")]
    EmptyMessage,
}

/// A conversation with the assistant.
///
/// Each message starts the CLI afresh and tells it which conversation to
/// continue, so nothing runs between messages, a cancelled or crashed turn
/// cannot take the conversation with it, and continuing five seconds later
/// is the same operation as resuming next week.
///
/// A request that needs more steps than one run of the CLI is given
/// ([`turns_per_run`](AssistantConfig::turns_per_run)) is carried on in
/// another run, and another, up to the limit for one request
/// ([`max_turns`](AssistantConfig::max_turns)). To the caller it is one
/// turn all the same: one stream of events and one terminal event.
///
/// Events are pulled, not pushed: call [`poll`](Conversation::poll) from the
/// UI's frame loop until it returns `None`. Dropping the conversation stops
/// any turn in progress.
pub struct Conversation {
    config: Arc<AssistantConfig>,
    cli: CliInstallation,
    /// Shared with the turn's thread, which learns the id from the CLI.
    session: Arc<Mutex<Option<SessionId>>>,
    files: Arc<HandoverFiles>,
    /// The turn whose terminal event has not been handed out yet.
    turn: Option<RunningTurn>,
    environment_reported: bool,
}

struct RunningTurn {
    events: Receiver<Event>,
    control: Arc<Control>,
}

impl Conversation {
    /// Starts a new conversation. Nothing runs until the first
    /// [`send`](Conversation::send).
    ///
    /// `cli` comes from [`detect`](crate::detect).
    pub fn new(config: AssistantConfig, cli: CliInstallation) -> Result<Self, ConfigError> {
        Self::with_session(config, cli, None)
    }

    /// Continues a conversation from an earlier run of the app.
    ///
    /// `config.working_dir` must be the directory the conversation was
    /// started in: the CLI files conversations by it. The CLI also discards
    /// conversations after a while (thirty days unless the user changed it);
    /// the first message then fails with
    /// [`SessionNotFound`](FailureReason::SessionNotFound) and the one after
    /// starts afresh.
    pub fn resume(config: AssistantConfig, cli: CliInstallation, session: SessionId) -> Result<Self, ConfigError> {
        Self::with_session(config, cli, Some(session))
    }

    fn with_session(
        mut config: AssistantConfig,
        cli: CliInstallation,
        session: Option<SessionId>,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        // The CLI runs in `working_dir`, so a relative path would mean one
        // place to Trill and another to the CLI.
        config.working_dir = absolute(&config.working_dir);
        config.temp_dir = absolute(&config.temp_dir);
        let files = Arc::new(HandoverFiles::new(&config.temp_dir));
        Ok(Self {
            config: Arc::new(config),
            cli,
            session: Arc::new(Mutex::new(session)),
            files,
            turn: None,
            environment_reported: false,
        })
    }

    /// The id to store in order to [`resume`](Conversation::resume) later.
    /// `None` until the first turn has started.
    pub fn session_id(&self) -> Option<SessionId> {
        lock(&self.session).clone()
    }

    /// Whether a turn is under way, or finished with events still to
    /// [`poll`](Conversation::poll).
    pub fn is_busy(&self) -> bool {
        self.turn.is_some()
    }

    /// Sends a message from the user and starts the assistant's turn.
    ///
    /// `context` is for what the app knows and the user did not type: the
    /// selection, the playhead, the loop. It is given to the model alongside
    /// the message, marked as coming from Trill. Send it with every message
    /// it applies to; it is not remembered as current.
    ///
    /// Returns at once. What happens next arrives through
    /// [`poll`](Conversation::poll), ending with exactly one terminal event.
    pub fn send(&mut self, message: &str, context: Option<&str>) -> Result<(), SendError> {
        if self.turn.is_some() {
            return Err(SendError::TurnInProgress);
        }
        let prompt = compose_prompt(message, context).ok_or(SendError::EmptyMessage)?;

        let (events, receiver) = mpsc::channel();
        let control = Arc::new(Control::default());
        let job = TurnJob {
            config: Arc::clone(&self.config),
            cli: self.cli.clone(),
            session: Arc::clone(&self.session),
            files: Arc::clone(&self.files),
            control: Arc::clone(&control),
            report_environment: !std::mem::replace(&mut self.environment_reported, true),
            events: events.clone(),
        };
        let spawned = thread::Builder::new().name("trill-assistant-turn".to_owned()).spawn(move || job.run(prompt));
        if let Err(error) = spawned {
            let _ = events.send(Event::Failed(FailureReason::CannotStart { detail: error.to_string() }));
        }
        self.turn = Some(RunningTurn { events: receiver, control });
        Ok(())
    }

    /// The next event of the current turn, if one is ready. Never blocks.
    pub fn poll(&mut self) -> Option<Event> {
        let received = self.turn.as_ref()?.events.try_recv();
        match received {
            Ok(event) => Some(self.hand_out(event)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(self.hand_out(turn_thread_lost())),
        }
    }

    /// The next event of the current turn, waiting up to `timeout` for it.
    /// For programs without a frame loop; a UI should [`poll`](Conversation::poll).
    pub fn wait(&mut self, timeout: Duration) -> Option<Event> {
        let received = self.turn.as_ref()?.events.recv_timeout(timeout);
        match received {
            Ok(event) => Some(self.hand_out(event)),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(self.hand_out(turn_thread_lost())),
        }
    }

    /// Stops the current turn, if there is one.
    ///
    /// The CLI and everything it started are killed at once; nothing is asked
    /// of them. [`Event::Cancelled`] follows as soon as they are gone. Edits
    /// the assistant had already made stay made, and the conversation can
    /// carry on with the next message.
    pub fn cancel(&self) {
        if let Some(turn) = &self.turn {
            turn.control.cancel();
        }
    }

    fn hand_out(&mut self, event: Event) -> Event {
        if event.is_terminal() {
            self.turn = None;
        }
        event
    }
}

impl Drop for Conversation {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The turn's thread ended without a terminal event, which only a bug in
/// this crate can cause. The turn still has to end for the caller.
fn turn_thread_lost() -> Event {
    Event::Failed(FailureReason::Protocol { detail: "Trill stopped reading the reply".to_owned() })
}

/// What the CLI is given as the user's message.
fn compose_prompt(message: &str, context: Option<&str>) -> Option<String> {
    let message = message.trim();
    if message.is_empty() {
        return None;
    }
    Some(match context.map(str::trim).filter(|context| !context.is_empty()) {
        Some(context) => format!("<trill-context>\n{context}\n</trill-context>\n\n{message}"),
        // The CLI takes a prompt that begins with `/` for one of its own
        // commands and answers it itself, even with commands disabled (seen
        // with 2.1.325). A lead-in keeps the user's words a message.
        None if message.starts_with('/') => format!("Message from the user:\n{message}"),
        None => message.to_owned(),
    })
}

/// The CLI's command line. The message itself travels on standard input.
///
/// Each group below was checked against CLI 2.1.325 by looking at what the
/// CLI then reported about itself; see the crate documentation.
fn arguments(
    config: &AssistantConfig,
    cli: &CliInstallation,
    files: &HandoverFiles,
    session: Option<&SessionId>,
    max_turns: Option<u32>,
) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = Vec::new();
    let mut add = |argument: &str| arguments.push(argument.into());

    // Run once and exit, reporting as one JSON object per line, with the
    // reply delivered as it is written.
    add("-p");
    add("--output-format");
    add("stream-json");
    add("--verbose");
    add("--include-partial-messages");

    // Trill's MCP server and no other.
    add("--mcp-config");
    add(&files.mcp_config.to_string_lossy());
    add("--strict-mcp-config");

    // Nothing of the user's own setup: no settings files (and so no hooks,
    // plugins or permission rules), no skills or custom commands.
    add("--setting-sources");
    add("");
    add("--disable-slash-commands");

    // Of the CLI's own tools, only the ones for keeping a plan and for the
    // web, each if it is switched on; with both off the list is empty, which
    // is no built-in tool at all. Those and Trill's tools are allowed;
    // anything else that would ask for permission is refused instead of
    // waiting for an answer nobody can give.
    let own = config.cli_tools().join(",");
    add("--tools");
    add(&own);
    add("--allowedTools");
    let trills = format!("mcp__{}__*", config.mcp_server_name);
    add(&if own.is_empty() { trills } else { format!("{trills},{own}") });
    if !config.withheld_tools.is_empty() {
        // Denied tools are not merely refused: the model is never told of them.
        let withheld: Vec<String> =
            config.withheld_tools.iter().map(|tool| format!("mcp__{}__{tool}", config.mcp_server_name)).collect();
        add("--disallowedTools");
        add(&withheld.join(","));
    }
    add("--permission-mode");
    add("dontAsk");
    if cli.version >= PERMISSION_PROMPTS_SINCE {
        add("--permission-prompts");
        add("none");
    }

    if !config.system_prompt.trim().is_empty() {
        add("--append-system-prompt-file");
        add(&files.system_prompt.to_string_lossy());
    }
    // `name=value` in one argument, so that no value can be read as an option.
    if let Some(model) = &config.model {
        add(&format!("--model={model}"));
    }
    if let Some(effort) = config.effort {
        add(&format!("--effort={}", effort.name()));
    }
    if let Some(max_turns) = max_turns {
        add(&format!("--max-turns={max_turns}"));
    }
    if let Some(session) = session {
        add(&format!("--resume={session}"));
    }
    arguments
}

/// The two small files the CLI is pointed at, removed when the conversation
/// and its last turn are gone. Files, not arguments: their contents (a URL,
/// free text) then need no quoting on any platform.
struct HandoverFiles {
    mcp_config: PathBuf,
    system_prompt: PathBuf,
}

impl HandoverFiles {
    fn new(temp_dir: &Path) -> Self {
        // Unique per conversation, so that two conversations (or two copies
        // of Trill) sharing a directory do not overwrite each other.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stem = format!("trill-assistant-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        Self {
            mcp_config: temp_dir.join(format!("{stem}-mcp.json")),
            system_prompt: temp_dir.join(format!("{stem}-prompt.txt")),
        }
    }

    fn write(&self, config: &AssistantConfig, cli: &CliInstallation) -> io::Result<()> {
        fs::create_dir_all(&config.working_dir)?;
        fs::create_dir_all(&config.temp_dir)?;
        fs::write(&self.mcp_config, mcp_servers(config, cli).to_string())?;
        fs::write(&self.system_prompt, &config.system_prompt)
    }
}

/// What the CLI is told about MCP servers: Trill's, and how long one call to
/// it may take.
fn mcp_servers(config: &AssistantConfig, cli: &CliInstallation) -> serde_json::Value {
    let mut server = json!({ "type": "http", "url": &config.mcp_url });
    if cli.version >= SERVER_TIMEOUT_SINCE {
        let milliseconds = u64::try_from(config.tool_timeout.as_millis()).unwrap_or(u64::MAX);
        server["timeout"] = json!(milliseconds);
    }
    json!({ "mcpServers": { &config.mcp_server_name: server } })
}

impl Drop for HandoverFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.mcp_config);
        let _ = fs::remove_file(&self.system_prompt);
    }
}

/// How the conversation's owner reaches into a running turn.
#[derive(Default)]
struct Control {
    cancelled: AtomicBool,
    tree: Mutex<Option<ProcessTree>>,
}

impl Control {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.kill();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn kill(&self) {
        if let Some(tree) = lock(&self.tree).as_ref() {
            tree.kill();
        }
    }

    /// Takes charge of a process that has just started. A cancel that came
    /// while it was starting is carried out here; one that comes later finds
    /// the tree in place. The lock makes those the only two cases.
    fn adopt(&self, tree: ProcessTree) {
        let mut slot = lock(&self.tree);
        if self.is_cancelled() {
            tree.kill();
        }
        *slot = Some(tree);
    }

    fn release(&self) -> Option<ProcessTree> {
        lock(&self.tree).take()
    }
}

/// One turn, on its own thread.
struct TurnJob {
    config: Arc<AssistantConfig>,
    cli: CliInstallation,
    session: Arc<Mutex<Option<SessionId>>>,
    files: Arc<HandoverFiles>,
    control: Arc<Control>,
    report_environment: bool,
    events: Sender<Event>,
}

/// What a request has taken so far, over the runs of the CLI that have
/// ended.
#[derive(Default)]
struct Spent {
    /// Model round trips, as `--max-turns` counts them.
    steps: u32,
    duration: Duration,
    usage: TokenUsage,
    /// The CLI's estimates, added up; `None` until a run has given one.
    cost: Option<f64>,
}

impl Spent {
    /// Adds what one run took. A run that was stopped at its limit of
    /// `allowed` used exactly that many steps: that is what the limit
    /// counts, whatever was seen of them.
    fn add(&mut self, figures: Figures, allowed: Option<u32>, stopped_at_limit: bool) {
        self.steps += match (allowed, stopped_at_limit) {
            (Some(allowed), true) => allowed,
            _ => figures.turns,
        };
        self.duration += figures.duration;
        self.usage.input += figures.usage.input;
        self.usage.output += figures.usage.output;
        self.usage.cache_read += figures.usage.cache_read;
        self.usage.cache_write += figures.usage.cache_write;
        if let Some(cost) = figures.cost {
            self.cost = Some(self.cost.unwrap_or(0.0) + cost);
        }
    }

    fn progress(&self) -> Progress {
        Progress { steps: self.steps, estimated_cost_usd: self.cost, costed_steps: self.steps }
    }
}

impl TurnJob {
    fn run(self, prompt: String) {
        let terminal = self.request(prompt);
        if terminal == Event::Failed(FailureReason::SessionNotFound) {
            *lock(&self.session) = None;
        }
        let _ = self.events.send(terminal);
    }

    /// Works on one message until it is answered, stopped or failed, in as
    /// many runs of the CLI as that takes, and returns the terminal event.
    fn request(&self, prompt: String) -> Event {
        let mut spent = Spent::default();
        let mut input = prompt;
        let mut first = true;
        loop {
            // What this run may use: its share, and no more than the
            // request has left.
            let left = self.config.max_turns.map(|limit| limit.saturating_sub(spent.steps));
            let allowed = left.map_or(self.config.turns_per_run, |left| left.min(self.config.turns_per_run));
            let (terminal, figures) = self.work(&input, Some(allowed), spent.progress(), first);
            first = false;

            let stopped_at_limit = matches!(&terminal, Event::Failed(FailureReason::TurnLimitReached { .. }));
            if let Some(figures) = figures {
                spent.add(figures, Some(allowed), stopped_at_limit);
            }
            match terminal {
                Event::Finished(_) => {
                    return Event::Finished(TurnSummary {
                        turns: spent.steps,
                        duration: spent.duration,
                        usage: spent.usage,
                        estimated_cost_usd: spent.cost,
                    });
                }
                // Stopped while the run was ending: stopped.
                Event::Failed(FailureReason::TurnLimitReached { .. }) if self.control.is_cancelled() => {
                    return Event::Cancelled;
                }
                // A run that says it used its steps, in which the model was
                // not seen to answer once: carried on, that would go round
                // for ever. No CLI has been seen to do it.
                Event::Failed(FailureReason::TurnLimitReached { limit })
                    if figures.is_none_or(|figures| figures.replies == 0) =>
                {
                    return Event::Failed(FailureReason::TurnLimitReached { limit });
                }
                Event::Failed(FailureReason::TurnLimitReached { .. }) => {
                    // What the request has cost so far is known now.
                    let _ = self.events.send(Event::Progress(spent.progress()));
                    if let Some(limit) = self.config.max_turns.filter(|limit| spent.steps >= *limit) {
                        return Event::Failed(FailureReason::TurnLimitReached { limit });
                    }
                    CONTINUATION.clone_into(&mut input);
                }
                other => return other,
            }
        }
    }

    /// Runs the CLI once and returns how the run ended, having sent every
    /// other event on the way, with what the CLI said the run took.
    fn work(&self, prompt: &str, max_turns: Option<u32>, before: Progress, first: bool) -> (Event, Option<Figures>) {
        if let Err(error) = self.files.write(&self.config, &self.cli) {
            let detail = format!("could not write to {}: {error}", self.config.temp_dir.display());
            return (Event::Failed(FailureReason::CannotStart { detail }), None);
        }

        let session = lock(&self.session).clone();
        let mut command = Command::new(&self.cli.path);
        command
            .args(arguments(&self.config, &self.cli, &self.files, session.as_ref(), max_turns))
            .current_dir(&self.config.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let ignored = env::apply(&mut command, self.config.keep_redirecting_environment);
        env::allow_long_tool_calls(&mut command, self.config.tool_timeout);
        if self.report_environment && first {
            for name in ignored {
                let _ = self.events.send(Event::Notice(Notice::EnvironmentVariableIgnored { name }));
            }
        }

        if self.control.is_cancelled() {
            return (Event::Cancelled, None);
        }
        let (mut child, tree) = match process::spawn(&mut command) {
            Ok(started) => started,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (Event::Failed(FailureReason::CliMissing { path: self.cli.path.clone() }), None);
            }
            Err(error) => return (Event::Failed(FailureReason::CannotStart { detail: error.to_string() }), None),
        };
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        self.control.adopt(tree);

        // On its own thread so that a CLI slow to read cannot hold up reading
        // what it writes. Dropping `stdin` is the end of the message.
        let message = prompt.to_owned();
        thread::spawn(move || {
            let _ = stdin.write_all(message.as_bytes());
        });
        let stderr_tail = tail_of(stderr);

        let mut translator = Translator::new(Expectations {
            mcp_server_name: self.config.mcp_server_name.clone(),
            mcp_url: self.config.mcp_url.clone(),
            cli_tools: self.config.cli_tools().into_iter().map(str::to_owned).collect(),
            max_turns,
            before,
        });
        let (exit_status, overlong) = self.follow(&mut child, chunks_of(stdout), &mut translator);

        let exit_status = exit_status.or_else(|| self.await_exit(&mut child));
        // The CLI itself is gone. Whatever it left running goes now, which
        // also lets go of the error stream read below.
        if let Some(tree) = self.control.release() {
            tree.close();
        }

        let cancelled = self.control.is_cancelled();
        if overlong && !cancelled {
            let detail = format!("a line of the reply was longer than {} MB", MAX_LINE_BYTES / (1024 * 1024));
            return (Event::Failed(FailureReason::Protocol { detail }), None);
        }
        let stderr_tail = stderr_tail.recv_timeout(DRAIN_GRACE).unwrap_or_default();
        let figures = translator.figures();
        let exit = Exit { code: exit_status.and_then(|status| status.code()), stderr_tail };
        (translator.finish(cancelled, exit), figures)
    }

    /// Reads the CLI's output until it ends or has to be cut short. Returns
    /// the exit status if the process was seen to exit, and whether a line
    /// was too long to accept.
    fn follow(
        &self,
        child: &mut Child,
        chunks: Receiver<Vec<u8>>,
        translator: &mut Translator,
    ) -> (Option<ExitStatus>, bool) {
        let mut splitter = LineSplitter::default();
        let mut events = Vec::new();
        let mut exited: Option<(ExitStatus, Instant)> = None;
        let mut reported_at: Option<Instant> = None;

        loop {
            match chunks.recv_timeout(POLL_INTERVAL) {
                Ok(chunk) => {
                    let pushed = splitter.push(&chunk, |line| translator.line(line, &mut events));
                    self.deliver(&mut events);
                    if pushed.is_err() {
                        self.control.kill();
                        return (exited.map(|(status, _)| status), true);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    splitter.finish(|line| translator.line(line, &mut events));
                    self.deliver(&mut events);
                    break;
                }
            }

            if self.control.is_cancelled() {
                break;
            }
            match translator.verdict() {
                Some(Verdict::Refused(_)) => {
                    self.control.kill();
                    break;
                }
                // The clock starts the first time the report is seen.
                Some(Verdict::Reported(_)) if reported_at.get_or_insert_with(Instant::now).elapsed() > EXIT_GRACE => {
                    self.control.kill();
                    break;
                }
                Some(Verdict::Reported(_)) | None => {}
            }
            if exited.is_none()
                && let Ok(Some(status)) = child.try_wait()
            {
                exited = Some((status, Instant::now()));
            }
            if exited.is_some_and(|(_, at)| at.elapsed() > DRAIN_GRACE) {
                self.control.kill();
                break;
            }
        }
        (exited.map(|(status, _)| status), false)
    }

    /// Waits for a process whose output has ended, stopping it if it does
    /// not go by itself.
    fn await_exit(&self, child: &mut Child) -> Option<ExitStatus> {
        let deadline = Instant::now() + EXIT_GRACE;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {}
                Err(_) => return None,
            }
            if Instant::now() >= deadline {
                self.control.kill();
                return child.wait().ok();
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn deliver(&self, events: &mut Vec<Event>) {
        for event in events.drain(..) {
            if let Event::Started(start) = &event {
                *lock(&self.session) = Some(start.session_id.clone());
            }
            let _ = self.events.send(event);
        }
    }
}

/// Reads `stdout` on its own thread, so that the turn's thread can wait for
/// output and watch the clock at the same time.
fn chunks_of(mut stdout: ChildStdout) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if sender.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    receiver
}

/// Collects the end of the CLI's error stream, delivered when it closes.
fn tail_of(mut stderr: ChildStderr) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            match stderr.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    tail.extend_from_slice(&buffer[..read]);
                    if tail.len() > STDERR_TAIL_BYTES {
                        tail.drain(..tail.len() - STDERR_TAIL_BYTES);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        // The cut may have fallen inside a character.
        let start = tail.iter().position(|byte| byte & 0xC0 != 0x80).unwrap_or(tail.len());
        let _ = sender.send(String::from_utf8_lossy(&tail[start..]).trim().to_owned());
    });
    receiver
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_owned())
}

/// Locks a mutex whose data stays valid even if a holder panicked: every
/// critical section here is a single assignment or read.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Effort;

    fn cli(version: CliVersion) -> CliInstallation {
        CliInstallation { path: PathBuf::from("claude"), version }
    }

    fn files() -> HandoverFiles {
        HandoverFiles { mcp_config: PathBuf::from("mcp.json"), system_prompt: PathBuf::from("prompt.txt") }
    }

    fn joined(arguments: &[OsString]) -> String {
        arguments.iter().map(|argument| format!("[{}]", argument.to_string_lossy())).collect()
    }

    #[test]
    fn the_command_line_confines_the_cli_to_trill() {
        let config = AssistantConfig::new("data");
        let arguments = joined(&arguments(&config, &cli(CliVersion::new(2, 1, 325)), &files(), None, Some(100)));

        for group in [
            "[-p][--output-format][stream-json][--verbose][--include-partial-messages]",
            "[--mcp-config][mcp.json][--strict-mcp-config]",
            "[--setting-sources][][--disable-slash-commands]",
            "[--tools][TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite,WebFetch,WebSearch]",
            "[--allowedTools][mcp__trill__*,TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite,WebFetch,WebSearch]",
            "[--permission-mode][dontAsk][--permission-prompts][none]",
            "[--append-system-prompt-file][prompt.txt]",
            "[--max-turns=100]",
        ] {
            assert!(arguments.contains(group), "missing {group} in {arguments}");
        }
        for absent in ["--model", "--effort", "--resume", "--disallowedTools", "--bare", "--safe-mode", "--dangerously"]
        {
            assert!(!arguments.contains(absent), "unexpected {absent} in {arguments}");
        }
        // Nothing of the CLI's that reads or writes a file, runs a program,
        // or starts another agent is ever asked for.
        let asked = config.cli_tools();
        assert!(arguments.contains(&format!("[--tools][{}]", asked.join(","))), "the list above is what is asked for");
        for tool in ["Read", "Write", "Edit", "Bash", "PowerShell", "Glob", "Grep", "NotebookEdit", "Skill", "Agent"] {
            assert!(!asked.contains(&tool), "{tool}");
        }
    }

    #[test]
    fn with_the_web_switched_off_its_tools_are_not_asked_for_at_all() {
        let offline = AssistantConfig { web: false, ..AssistantConfig::new("data") };
        let asked = joined(&arguments(&offline, &cli(CliVersion::new(2, 1, 325)), &files(), None, None));
        assert!(
            asked.contains("[--tools][TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite][--allowedTools]"),
            "{asked}"
        );
        assert!(
            asked.contains("[--allowedTools][mcp__trill__*,TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite][--perm")
        );
        assert!(!asked.contains("Web"), "{asked}");

        // With the plan off as well, the CLI is given no tool of its own:
        // the empty list, which must still be there.
        let bare = AssistantConfig { web: false, plan: false, ..AssistantConfig::new("data") };
        let asked = joined(&arguments(&bare, &cli(CliVersion::new(2, 1, 325)), &files(), None, None));
        assert!(asked.contains("[--tools][][--allowedTools][mcp__trill__*][--permission-mode]"), "{asked}");
    }

    #[test]
    fn choices_and_the_conversation_to_continue_are_passed_as_single_arguments() {
        let config = AssistantConfig {
            model: Some("haiku".into()),
            effort: Some(Effort::ExtraHigh),
            max_turns: None,
            system_prompt: "  ".into(),
            mcp_server_name: "studio".into(),
            withheld_tools: vec!["load_project".into(), "new_project".into()],
            ..AssistantConfig::new("data")
        };
        let session = SessionId::parse("39367d22-e40b-4a04-9358-33e91538ff0d").unwrap();
        let arguments = joined(&arguments(&config, &cli(CliVersion::new(2, 1, 325)), &files(), Some(&session), None));

        assert!(arguments.contains("[--model=haiku][--effort=xhigh][--resume=39367d22-e40b-4a04-9358-33e91538ff0d]"));
        assert!(arguments.contains(
            "[--allowedTools][mcp__studio__*,TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite,WebFetch,WebSearch]\
             [--disallowedTools][mcp__studio__load_project,mcp__studio__new_project]"
        ));
        assert!(!arguments.contains("--max-turns"));
        assert!(!arguments.contains("--append-system-prompt-file"));
    }

    #[test]
    fn an_older_cli_is_not_given_a_flag_it_does_not_know() {
        let arguments =
            joined(&arguments(&AssistantConfig::new("data"), &cli(CliVersion::new(2, 1, 94)), &files(), None, None));

        assert!(arguments.contains("[--permission-mode][dontAsk]"));
        assert!(!arguments.contains("--permission-prompts"));
    }

    #[test]
    fn context_is_marked_as_coming_from_trill() {
        assert_eq!(
            compose_prompt("  make it swing ", Some("playhead: bar 5\nselection: clip 3\n")).as_deref(),
            Some("<trill-context>\nplayhead: bar 5\nselection: clip 3\n</trill-context>\n\nmake it swing")
        );
        assert_eq!(compose_prompt("make it swing", Some("  ")).as_deref(), Some("make it swing"));
        assert_eq!(compose_prompt("make it swing", None).as_deref(), Some("make it swing"));
    }

    #[test]
    fn a_message_is_never_taken_for_a_cli_command() {
        assert_eq!(compose_prompt("/clear", None).as_deref(), Some("Message from the user:\n/clear"));
        assert!(compose_prompt("/clear", Some("playhead: bar 1")).unwrap().starts_with("<trill-context>"));
    }

    #[test]
    fn an_empty_message_is_not_sent() {
        assert_eq!(compose_prompt(" \n", Some("playhead: bar 5")), None);

        let mut conversation =
            Conversation::new(AssistantConfig::new("data"), cli(CliVersion::new(2, 1, 325))).unwrap();
        assert_eq!(conversation.send("   ", None), Err(SendError::EmptyMessage));
        assert!(!conversation.is_busy());
        assert_eq!(conversation.poll(), None);
    }

    #[test]
    fn an_unusable_configuration_is_refused_before_anything_runs() {
        let config = AssistantConfig { mcp_server_name: String::new(), ..AssistantConfig::new("data") };
        assert!(matches!(Conversation::new(config, cli(CliVersion::new(2, 1, 325))), Err(ConfigError::ServerName(_))));
    }

    #[test]
    fn the_cli_is_told_how_long_a_call_to_trill_may_take_where_it_is_known_to_listen() {
        let config = AssistantConfig { tool_timeout: Duration::from_secs(900), ..AssistantConfig::new("data") };
        assert_eq!(
            mcp_servers(&config, &cli(CliVersion::new(2, 1, 325))),
            json!({ "mcpServers": { "trill": { "type": "http", "url": "http://127.0.0.1:7777/mcp", "timeout": 900_000 } } })
        );
        assert_eq!(
            mcp_servers(&config, &cli(CliVersion::new(2, 1, 94))),
            json!({ "mcpServers": { "trill": { "type": "http", "url": "http://127.0.0.1:7777/mcp" } } })
        );
    }

    #[test]
    fn what_a_request_has_taken_is_added_up_over_its_runs() {
        let run = |turns, cost| Figures {
            turns,
            replies: turns,
            duration: Duration::from_secs(60),
            usage: TokenUsage { input: 10, output: 20, cache_read: 30, cache_write: 40 },
            cost,
        };
        let mut spent = Spent::default();
        assert_eq!(spent.progress(), Progress { steps: 0, estimated_cost_usd: None, costed_steps: 0 });

        // A run stopped at its limit used the limit, whatever was counted.
        spent.add(run(101, Some(1.25)), Some(100), true);
        assert_eq!(spent.progress(), Progress { steps: 100, estimated_cost_usd: Some(1.25), costed_steps: 100 });
        // One that finished used what it says; one that gave no estimate
        // takes nothing from the estimate so far.
        spent.add(run(37, None), Some(100), false);
        spent.add(run(3, Some(0.5)), Some(100), false);
        assert_eq!(spent.progress(), Progress { steps: 140, estimated_cost_usd: Some(1.75), costed_steps: 140 });
        assert_eq!(spent.duration, Duration::from_secs(180));
        assert_eq!(spent.usage, TokenUsage { input: 30, output: 60, cache_read: 90, cache_write: 120 });
    }

    #[test]
    fn the_note_that_carries_a_request_on_says_who_wrote_it() {
        assert!(CONTINUATION.starts_with("<trill-continue>") && CONTINUATION.ends_with("</trill-continue>"));
        assert!(CONTINUATION.contains("Trill is writing this, not the user"));
        // The CLI takes a line that begins with `/` for a command of its own.
        assert!(!CONTINUATION.starts_with('/'));
    }

    #[test]
    fn a_cancel_before_the_process_exists_is_carried_out_when_it_does() {
        let control = Control::default();
        control.cancel();
        assert!(control.is_cancelled());
        // Nothing to kill yet, and nothing to release.
        assert!(control.release().is_none());
    }
}
