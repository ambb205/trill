//! The back end of Trill's in-app assistant.
//!
//! Trill already offers everything it can do as tools on a local MCP server.
//! This crate lets the user talk to Claude about their project without
//! leaving the app, by running **the user's own installed, signed-in,
//! unmodified Claude Code CLI** as a child process, pointed at that MCP
//! server and allowed to use nothing else. The chat panel is built on top of
//! it elsewhere.
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
//! in. A [`Conversation`] then runs it once per message:
//!
//! ```text
//! claude -p --output-format stream-json --verbose --include-partial-messages
//!        --mcp-config <file> --strict-mcp-config
//!        --setting-sources "" --disable-slash-commands
//!        --tools "" --allowedTools "mcp__trill__*" [--disallowedTools ..]
//!        --permission-mode dontAsk --permission-prompts none
//!        --append-system-prompt-file <file>
//!        [--model=..] [--effort=..] [--max-turns=..] [--resume=<id>]
//! ```
//!
//! with the message on standard input, and reads the reply as it is written.
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
//! **Cancelling kills the process tree.** A turn is stopped by ending the
//! CLI and everything it started, at once, and the conversation is resumed
//! by its id on the next message. The CLI does have a gentler interrupt
//! message, but its wire format is not documented; killing works on every
//! version and was verified to leave the conversation resumable. On Windows
//! the processes are held in a job object, so they also die if Trill does.
//!
//! **Only Trill's tools.** The flags above remove the CLI's built-in tools,
//! the user's own MCP servers, settings, hooks, skills and instruction
//! files, and anything that would ask for permission. That is then checked,
//! not assumed: every turn begins by reading the CLI's own list of what it
//! is offering the model, and a turn in which that list holds anything but
//! Trill's tools is stopped before the model is asked
//! ([`FailureReason::UnexpectedTools`]).
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
//!   address in [`AssistantConfig::mcp_url`].
//! * Give [`AssistantConfig::working_dir`] a folder of Trill's own that
//!   never changes, and store [`Conversation::session_id`] next to the
//!   transcript the app keeps for display. Those two together are what
//!   resuming needs. The CLI's own transcript files are never read.
//! * Send what the user is looking at as the `context` of each message.
//! * Show a [`FailureReason`] or [`Unavailable`] as its `Display` text. Label
//!   [`TurnSummary::estimated_cost_usd`] as an estimate.
//!
//! # What was checked, and against which CLI
//!
//! Against CLI 2.1.325 on Windows 11, by reading what the CLI reported:
//!
//! * `--safe-mode` cannot be used: it also drops the server given with
//!   `--mcp-config`. The flags above do its work one by one instead.
//! * With those flags the CLI lists Trill's tools and nothing else, one MCP
//!   server, no skills, no commands, no plugins of the user's, and runs none
//!   of the user's hooks. Started with only the MCP flags, it listed its
//!   built-in tools and the user's skills and plugins, and ran a hook of
//!   theirs.
//! * The MCP servers attached to the user's claude.ai account are fetched
//!   even under `--strict-mcp-config`, and a prompt that begins with `/` is
//!   answered by the CLI itself even under `--disable-slash-commands`. The
//!   first is switched off through the environment, the second avoided by
//!   how the message is worded (see `conversation.rs`).
//! * An `ANTHROPIC_API_KEY` in the environment is used in preference to the
//!   stored login, silently. See [`REDIRECTING_VARIABLES`].
//! * A conversation resumes after its CLI was killed: both straight after
//!   the first event and in the middle of a round of tool calls.
//!
//! CLI 2.1.94 was checked to start with the same command line (less
//! `--permission-prompts`, which it does not know) and to report the same
//! shapes for a missing login; that is why it is the
//! [minimum](MINIMUM_CLI_VERSION). The stream when a usage limit is reached
//! could not be provoked and is handled from the documented shapes.

mod config;
mod conversation;
mod detect;
mod env;
mod event;
mod process;
mod protocol;

pub use config::{
    AssistantConfig, ConfigError, DEFAULT_MAX_TURNS, DEFAULT_MCP_SERVER_NAME, DEFAULT_MCP_URL, DEFAULT_SYSTEM_PROMPT,
    Effort,
};
pub use conversation::{Conversation, SendError};
pub use detect::{CliInstallation, CliStatus, CliVersion, LoginMethod, MINIMUM_CLI_VERSION, Unavailable, detect};
pub use env::REDIRECTING_VARIABLES;
pub use event::{ApiErrorKind, Event, FailureReason, Notice, SessionId, TokenUsage, TurnStart, TurnSummary};
