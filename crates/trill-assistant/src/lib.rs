//! The back end of Trill's in-app assistant.
//!
//! Trill already offers everything it can do as tools on a local MCP server.
//! This crate lets the user talk to Claude about their project without
//! leaving the app, by running **the user's own installed, signed-in,
//! unmodified Claude Code CLI** as a child process, pointed at that MCP
//! server and allowed to use next to nothing else. The chat panel is built
//! on top of it elsewhere.
//!
//! # Rules this crate keeps
//!
//! Anthropic's usage terms let a person sign in to the unmodified Claude Code
//! program with their own subscription. They do not let another application
//! offer that sign-in itself, send requests on a user's plan credentials, or
//! collect, store or pass on Claude.ai credentials or session tokens. Trill is
//! such an application, so this crate:
//!
//! * **never touches credentials.** It does not read, copy, store or transmit
//!   any credential or token, in a file or in the environment. Whether the
//!   user is signed in is learned by asking the CLI (`claude auth status`),
//!   and of the answer only "signed in or not" and the method are kept.
//! * **never signs anyone in.** When the user is not signed in, it says so
//!   and tells them to run `claude` and sign in there. There is no login flow
//!   here and there must never be one.
//! * **never modifies, wraps for redistribution, or bundles the CLI.** It
//!   starts whatever the user installed, with documented flags.
//! * **never promises usage limits.** What the CLI reports about limits is
//!   relayed in the CLI's words; nothing is inferred from the user's plan.
//! * **does not take the names.** "Claude" and "Claude Code" appear only in
//!   plain descriptive text ("runs Claude Code"), never in the name of a
//!   type, a crate or a product feature.
//!
//! Changes to this crate must keep all five.
//!
//! # How it works
//!
//! [`detect`] finds the CLI and checks it: installed, recent enough, signed
//! in. A [`Conversation`] then runs it for each message:
//!
//! ```text
//! claude -p --output-format stream-json --verbose --include-partial-messages
//!        --mcp-config <file> --strict-mcp-config
//!        --setting-sources "" --disable-slash-commands
//!        --tools "TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite,WebFetch,WebSearch"
//!        --allowedTools "mcp__trill__*,TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite,WebFetch,WebSearch"
//!        [--disallowedTools ..]
//!        --permission-mode dontAsk --permission-prompts none
//!        --append-system-prompt-file <file>
//!        [--model=..] [--effort=..] --max-turns=<n> [--resume=<id>]
//! ```
//!
//! with the message on standard input, and reads the reply as it is written.
//! With [`web`](AssistantConfig::web) off, the two `Web` names are left out
//! of both lists; with [`plan`](AssistantConfig::plan) off as well, `--tools`
//! is given the empty string, which is no tool of the CLI's at all. The file
//! named to `--mcp-config` holds Trill's address and how long one call to it
//! may take. The environment is prepared in `env.rs`.
//!
//! **One process per message, not one per conversation.** The CLI can also
//! stay running and take messages on standard input. Starting it afresh was
//! chosen because the assistant lives inside a music program: nothing runs
//! (and nothing holds a few hundred megabytes) while the user is playing;
//! continuing a conversation and resuming it next week are the same
//! operation, so there is one path to test; and a turn that is cancelled or
//! crashes cannot take the conversation's process with it, because there is
//! none. The cost is the CLI's start-up, a few seconds per message.
//!
//! **A long request is several runs, and still one turn.** Writing a whole
//! song takes hundreds of round trips to the model. One run of the CLI is
//! given [`turns_per_run`](AssistantConfig::turns_per_run) of them (a
//! hundred); when it has used them and the request is not finished, the CLI
//! is started again on the same conversation with a short note in place of
//! a message (it says Trill is writing, not the user, and to carry on), and
//! so on until the request is done or has used the
//! [`max_turns`](AssistantConfig::max_turns) one request is allowed (a
//! thousand, which the app lets the user move). The caller sees one turn:
//! one stream of events, with [`Event::Started`] at each run, and one
//! terminal event carrying the sums. Nothing is retried: a run that fails
//! ends the turn with its reason.
//!
//! Runs are that short for one reason. The CLI says what a run cost only as
//! the run ends, so the end of each run is when a long request learns what
//! it has cost so far ([`Event::Progress`]). The price is a restart of the
//! CLI and one line of Trill's in the conversation every hundred steps. The
//! system prompt and the tools a resumed run sends were compared with the
//! run before and were the same, so the service's cache of the conversation
//! is not lost at the seam.
//!
//! **Cancelling kills the process tree.** A turn is stopped by ending the
//! CLI and everything it started, at once, and the conversation is resumed
//! by its id on the next message. The CLI does have a gentler interrupt
//! message, but its wire format is not documented; killing works on every
//! version and was verified to leave the conversation resumable. On Windows
//! the processes are held in a job object, so they also die if Trill does.
//! No further run is started for a request that was stopped.
//!
//! **Only Trill's tools, a plan, and the web if it is left on.** The flags
//! above remove the CLI's built-in tools except the few that are named, the
//! user's own MCP servers, settings, hooks, skills and instruction files,
//! and anything that would ask for permission. That is then checked, not
//! assumed: every run begins by reading the CLI's own list of what it is
//! offering the model, and a run in which that list holds anything but
//! Trill's tools and the ones of the CLI's own that were asked for
//! ([`AssistantConfig::cli_tools`]) is stopped before the model is asked
//! ([`FailureReason::UnexpectedTools`]). A call to a tool that was not on
//! that list stops the run too.
//!
//! The tools of the CLI's own are two fixed sets and not a list an app can
//! add to: [`PLAN_TOOLS`], which reach the plan and nothing else, and
//! [`WEB_TOOLS`]. Reading and writing files, the shell, editing, notebooks,
//! sub-agents, skills, the user's plugins and MCP servers are never asked
//! for. The next section says why that line is where it is.
//!
//! # What the web can do to the assistant
//!
//! With the web on, the assistant reads text that strangers wrote: search
//! results and pages. Such text can be addressed to the assistant ("ignore
//! your instructions and ..."), and no instruction to a model makes it
//! certain that it is never acted on. The system prompt does say that a
//! page is information and never an instruction, and in the one real search
//! made while this was written a result did carry text addressed to an AI,
//! which the model reported and left alone. That is the prompt working. It
//! is not what the design rests on.
//!
//! The design rests on this: **an assistant that has been talked round can
//! do only what its tools can do.** So the question is what the tools can
//! do, and the answer is kept small.
//!
//! What a page can, at worst, get the assistant to do:
//!
//! * **Change the open song.** Everything a request changes is one Undo in
//!   the app, and nothing reaches the project's file until something saves
//!   it.
//! * **Write a project, a render or a MIDI file** with Trill's tools. At the
//!   endpoint the assistant is given (`/assistant`), those write only into
//!   the open project's folder and `Documents/Trill`, only files of their
//!   own kind, and never replace a project that has unsaved changes
//!   (`trill-mcp`, `guard.rs`). A file of one of those kinds in one of
//!   those folders can be overwritten, the open project's own included.
//! * **Let out what the assistant has been told**, in a search query or in
//!   the address of a page it asks for: the names of the project, its
//!   tracks and clips, the names and paths of the user's samples, and the
//!   notes of any project or MIDI file Trill can open (its tools read
//!   those from wherever they are told to). Not the contents of any other
//!   file: nothing the assistant has reads one.
//! * **Make noise and use up steps**: start playback, preview a sound, open
//!   a plugin's editor, change the audio device, and keep working until the
//!   step limit. The user sees every step as it happens and can stop it.
//!
//! What it cannot get the assistant to do, because the assistant cannot do
//! it at all: read, write or delete any other file; run a program; reach
//! the user's settings, skills, plugins or other MCP servers; or be granted
//! more by a permission prompt, since nothing prompts and whatever is not
//! allowed is refused.
//!
//! **That is acceptable. Files and the web together would not be.** In
//! Claude Code itself a person approves what touches their files and what
//! runs. The panel has no such prompt: the user is making music, not
//! reading requests for permission. With a tool that reads files beside one
//! that fetches addresses, a page could have the assistant read a key or a
//! document and send it away in the address of the next page, and nobody
//! would be asked. With a tool that writes files or runs commands, a page
//! could do worse. Whoever wants to give the assistant such a tool has to
//! give the panel a permission prompt first, or keep the web off while the
//! tool is on.
//!
//! **Where the web tools reach.** A search is run by Anthropic's service:
//! the query leaves the machine, and nothing else of Trill's does. A page is
//! fetched by the CLI from this machine, so the site sees the user's
//! address, and (so the CLI's documentation says) the name of the site is
//! first checked against a list kept by Anthropic. The tool takes an
//! address and a question and nothing more.
//!
//! Trill's MCP server is on this machine too, with an endpoint that is not
//! guarded (`/mcp`) beside the assistant's, so it matters what the page
//! reader does with a local address. With CLI 2.1.325: `localhost` and
//! `[::1]` are refused before any request ("cannot fetch localhost or other
//! hostnames without a dot"); `127.0.0.1`, a name that resolves to it and a
//! number that means it are asked for over HTTPS whatever was written,
//! which a server that speaks plain HTTP, as Trill's does, cannot answer
//! (recorded in `tests/fixtures/web_lookups.jsonl`, and again against a
//! running Trill, both endpoints). Should a reader get as far as a request,
//! one that only reads calls no tool, and one that says it comes from a web
//! page is refused; `trill-app` tests both against the real server
//! (`nothing_a_page_reader_can_send_drives_the_server`). What was not
//! tried: a server on the user's network that speaks HTTPS with a
//! certificate the machine trusts. The reader could read from one what that
//! server gives anyone who asks.
//!
//! **The plan tools** reach the plan and nothing else. The newer ones keep
//! it as small files of the CLI's own, in its data folder under the
//! conversation's id (`tasks/<id>/<number>.json`), beside the record the CLI
//! keeps of every conversation; the older one keeps it in the conversation
//! itself. The assistant chooses the words of a step and nothing about
//! where it is kept: a step asked for by a name that is a path
//! (`../../x`) is "not found", and nothing is read or written for it. The
//! variables that would have the CLI keep the list somewhere shared are
//! removed from its environment (`env.rs`).
//!
//! # Using it
//!
//! ```no_run
//! use trill_assistant::{AssistantConfig, Conversation, Event, detect};
//!
//! // Slow (it starts the CLI twice): do this off the UI thread.
//! let config = AssistantConfig::new("C:/Users/me/AppData/Roaming/Trill/assistant");
//! let cli = detect(&config)?; // Err(reason): show `reason` to the user
//! let mut chat = Conversation::new(config, cli.installation)?;
//!
//! chat.send("Add a four-bar drum loop", Some("playhead: bar 5\nselected track: Drums"))?;
//! // Each frame:
//! while let Some(event) = chat.poll() {
//!     match event {
//!         Event::Text(piece) => print!("{piece}"),
//!         Event::ToolCallStarted { tool, input, .. } => println!("[{tool} {input}]"),
//!         Event::WebStarted { lookup, .. } => println!("[on the web: {lookup:?}]"),
//!         Event::Plan(change) => println!("[plan: {change:?}]"),
//!         Event::Progress(progress) => println!("[step {}]", progress.steps),
//!         Event::Failed(reason) => println!("{reason}"),
//!         event if event.is_terminal() => { /* store chat.session_id() to resume later */ }
//!         _ => {}
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! `examples/chat.rs` is a complete terminal client and the reference for
//! the UI.
//!
//! # What the app does around it
//!
//! * Start Trill's MCP server before the first message, and pass its real
//!   address in [`AssistantConfig::mcp_url`]: the guarded endpoint, since
//!   the assistant reads what strangers wrote.
//! * Give [`AssistantConfig::working_dir`] a folder of Trill's own that
//!   never changes, and store [`Conversation::session_id`] next to the
//!   transcript the app keeps for display. Those two together are what
//!   resuming needs. The CLI's own transcript files are never read.
//! * Send what the user is looking at as the `context` of each message.
//! * Show every [`Event::WebStarted`] with its query or its address: it is
//!   the one thing the assistant does that leaves the machine. Give the
//!   user a switch for [`AssistantConfig::web`] and show its state.
//! * Keep the plan from the [`PlanChange`]s and show it; show
//!   [`Event::Progress`] while a turn runs, with the limit it counts
//!   against, and a way to stop.
//! * Show a [`Notice`], a [`FailureReason`] or an [`Unavailable`] as its
//!   `Display` text. Label [`TurnSummary::estimated_cost_usd`] and
//!   [`Progress::estimated_cost_usd`] as estimates.
//! * Begin a new conversation, and do not resume the old one, when
//!   [`AssistantConfig::system_prompt`] has changed since the old one
//!   began: the CLI keeps a conversation to the instructions it began with.
//!
//! # What was checked, and against which CLI
//!
//! Against CLI 2.1.325 on Windows 11, by reading what the CLI reported.
//! Most of it was learned without asking any model: either signed out in a
//! scratch configuration folder, where the CLI still says what it offers,
//! or answering to a stand-in for the model service on this machine that
//! told it which tools to call. Those runs used the real CLI and its real
//! tools; six runs asked a real model (the smallest), and say so below.
//!
//! * `--safe-mode` cannot be used: it also drops the server given with
//!   `--mcp-config`. The flags above do its work one by one instead.
//! * With those flags the CLI lists Trill's tools, the ones of its own that
//!   were named and nothing else, one MCP server, no skills, no commands,
//!   no plugins of the user's, and runs none of the user's hooks. Started
//!   with only the MCP flags, it listed its built-in tools and the user's
//!   skills and plugins, and ran a hook of theirs. Asked by a real model
//!   for its tools other than Trill's, the answer was the four `Task`
//!   tools, `WebFetch` and `WebSearch`; asked to read a file and to run a
//!   command, it had nothing to do either with.
//! * The MCP servers attached to the user's claude.ai account are fetched
//!   even under `--strict-mcp-config`, and a prompt that begins with `/` is
//!   answered by the CLI itself even under `--disable-slash-commands`. The
//!   first is switched off through the environment, the second avoided by
//!   how the message is worded (see `conversation.rs`).
//! * An `ANTHROPIC_API_KEY` in the environment is used in preference to the
//!   stored login, silently. See [`REDIRECTING_VARIABLES`].
//! * A conversation resumes after its CLI was killed: both straight after
//!   the first event and in the middle of a round of tool calls.
//! * `--model` takes any name. One the service does not know is passed on
//!   and refused there (HTTP 404, reported by the CLI as `model_not_found`)
//!   with nothing used; see [`FailureReason::ModelUnavailable`] and
//!   [`MODEL_ALIASES`] for what was asked for and answered.
//! * `--effort` takes `low`, `medium`, `high`, `xhigh` and `max`, and each
//!   reached the request the CLI then made. A word it does not know is set
//!   aside with a warning and the default used. See [`Effort`].
//! * Which tools of its own the CLI has: run with `-p` it offers, by
//!   default, tools for files, the shell, sub-agents, scheduling and the
//!   web, and none for a plan. Named to `--tools`, it offers `TaskCreate`,
//!   `TaskGet`, `TaskList` and `TaskUpdate`; `TodoWrite` it passes over.
//!   What those four take, answer and write is in [`PLAN_TOOLS`].
//! * `--max-turns=n` lets the model be asked exactly `n` times, however
//!   many tools each reply calls, and then ends the run with
//!   `error_max_turns`, the tools' answers delivered. The `num_turns` the
//!   CLI then reports is not that count (six replies that made nine calls:
//!   ten), so this crate counts replies itself. `total_cost_usd` is the
//!   cost of that run alone, and no line before the last carries a cost.
//! * A call to an MCP server over HTTP is given 60 seconds ("The operation
//!   timed out."), and one that is silent 300 ("sent no response or
//!   progress for 300s; aborting"). A `timeout` for the server in the file
//!   that names it lifted both for a call of 310 seconds, and so did the
//!   two variables in `env.rs`; both are set.
//! * A conversation that outgrows the model: when the replies said the
//!   conversation had reached 985,053 tokens of the million the model was
//!   listed with, the CLI, run with `-p`, summarised it by itself and
//!   carried on in the same run: `status: compacting`, a status with
//!   `compact_result`, a `compact_boundary` with the sizes, and then the
//!   next reply. The plan was still there afterwards. See
//!   [`Notice::Compacted`]. This was the stand-in's doing: what a real
//!   model makes of a song after a real summary was not tried.
//! * With a real model (six runs of the smallest, signed in with a
//!   subscription): a request given four steps to a run was carried on
//!   over three runs, kept its plan, made each of its two edits once and
//!   ended as asked; a search and a page came back as recorded in
//!   `tests/real_cli.rs`; and two quick messages through the panel were
//!   answered in one and two steps.
//!
//! CLI 2.1.94 was checked to start with the earlier command line (less
//! `--permission-prompts`, which it does not know) and to report the same
//! shapes for a missing login; that is why it is the
//! [minimum](MINIMUM_CLI_VERSION). With the command line as it is now it
//! was checked only so far: signed out, it offered `WebFetch`, `WebSearch`
//! and `TodoWrite` and passed over the four `Task` names; answering to the
//! stand-in, its `TodoWrite` kept a plan as recorded in
//! `tests/fixtures/todo_write.jsonl`. It is not given the server's
//! `timeout`, which it was not checked to know.
//!
//! Not provoked, and handled from the documented shapes: the stream when a
//! usage limit is reached, and a summary that fails.

mod config;
mod conversation;
mod detect;
mod env;
mod event;
mod process;
mod protocol;

pub use config::{
    AssistantConfig, ConfigError, DEFAULT_MAX_TURNS, DEFAULT_MCP_SERVER_NAME, DEFAULT_MCP_URL, DEFAULT_SYSTEM_PROMPT,
    DEFAULT_TOOL_TIMEOUT, DEFAULT_TURNS_PER_RUN, Effort, MODEL_ALIASES, PLAN_TOOLS, WEB_TOOLS, is_model_name,
};
pub use conversation::{Conversation, SendError};
pub use detect::{CliInstallation, CliStatus, CliVersion, LoginMethod, MINIMUM_CLI_VERSION, Unavailable, detect};
pub use env::REDIRECTING_VARIABLES;
pub use event::{
    ApiErrorKind, Event, FailureReason, Notice, PlanChange, PlanStep, Progress, SessionId, StepStatus, TokenUsage,
    TurnStart, TurnSummary, WebLookup,
};
