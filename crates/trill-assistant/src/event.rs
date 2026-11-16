//! What a turn reports to the app.

use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde_json::Value;

/// Identifies a conversation so that it can be continued later. Store
/// [`as_str`](SessionId::as_str) wherever the app keeps its own transcript
/// and hand it back to [`Conversation::resume`](crate::Conversation::resume).
///
/// It is an identifier chosen by the CLI, not a credential.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// Accepts an id previously obtained from [`as_str`](SessionId::as_str).
    ///
    /// Anything that is not a UUID is refused: the id is placed on the CLI's
    /// command line, and a stored value that had been tampered with must not
    /// be able to pass for an option there.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let well_formed = bytes.len() == 36
            && bytes.iter().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            });
        well_formed.then(|| Self(text.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One thing that happened during a turn.
///
/// A turn produces any number of events and then exactly one terminal event:
/// [`Finished`](Event::Finished), [`Cancelled`](Event::Cancelled) or
/// [`Failed`](Event::Failed).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The CLI is up, connected to Trill, and about to ask the model.
    Started(TurnStart),

    /// The next piece of the assistant's reply. Append the pieces in order;
    /// a paragraph break is included where separate passages of text follow
    /// one another.
    Text(String),

    /// The assistant is calling one of Trill's tools.
    ToolCallStarted {
        /// Pairs this event with its [`ToolCallFinished`](Event::ToolCallFinished).
        id: String,
        /// Trill's name for the tool, such as `create_track`.
        tool: String,
        /// The arguments, as the JSON object the tool receives.
        input: Value,
    },

    /// A tool call returned.
    ToolCallFinished {
        id: String,
        /// The tool's name, as in the matching start; empty if the start was
        /// never seen.
        tool: String,
        /// The text the tool answered with: `Ok` when it succeeded, `Err`
        /// when it reported an error (which the assistant also sees, and
        /// usually corrects).
        result: Result<String, String>,
    },

    /// Something worth showing that does not end the turn.
    Notice(Notice),

    /// The assistant finished its reply.
    Finished(TurnSummary),

    /// The turn was stopped by [`Conversation::cancel`](crate::Conversation::cancel).
    /// Edits made before the cancel stay made; the conversation can go on.
    Cancelled,

    /// The turn could not be completed.
    Failed(FailureReason),
}

impl Event {
    /// Whether this is the last event of its turn.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Event::Finished(_) | Event::Cancelled | Event::Failed(_))
    }
}

/// What is known once a turn is under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStart {
    /// The conversation's id. Available from now on through
    /// [`Conversation::session_id`](crate::Conversation::session_id) too.
    pub session_id: SessionId,
    /// The model answering, as the CLI names it.
    pub model: String,
    /// Where the CLI found an API key, when it is using one instead of the
    /// user's stored login: for example the name of an environment variable.
    /// `None` is the normal case. When this is set the run is billed to an
    /// API account, which the user should be told.
    pub api_key_source: Option<String>,
}

/// Something worth showing that does not end the turn.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// A variable in Trill's environment would have redirected authentication
    /// or billing, and was kept from the CLI. Reported once per conversation.
    EnvironmentVariableIgnored { name: String },

    /// A request to the model failed and the CLI is about to try again.
    Retrying { attempt: u32, max_attempts: u32, delay: Duration, cause: ApiErrorKind },

    /// The service asked the CLI to slow down; it is waiting to try again.
    RateLimited { attempt: u32, max_attempts: u32, delay: Duration },

    /// The CLI reports that the user's usage allowance is nearly used up.
    UsageLimitNear {
        /// When the allowance renews, if the CLI said.
        resets_at: Option<SystemTime>,
    },

    /// A tool call was refused by the CLI's permission rules, so it did not
    /// run.
    PermissionDenied { tool: String },
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Notice::EnvironmentVariableIgnored { name } => {
                write!(f, "Ignored {name} from the environment so that the assistant uses your own Claude Code login.")
            }
            Notice::Retrying { attempt, max_attempts, .. } => {
                write!(f, "Connection trouble; trying again ({attempt} of {max_attempts}).")
            }
            Notice::RateLimited { attempt, max_attempts, .. } => {
                write!(f, "The service is busy; waiting to try again ({attempt} of {max_attempts}).")
            }
            Notice::UsageLimitNear { .. } => f.write_str("You are close to your Claude usage limit."),
            Notice::PermissionDenied { tool } => write!(f, "The assistant was not allowed to use {tool}."),
        }
    }
}

/// How a finished turn went.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    /// How many times the model was asked: one for the reply, plus one for
    /// each round of tool calls.
    pub turns: u32,
    /// How long the CLI worked on the message.
    pub duration: Duration,
    pub usage: TokenUsage,
    /// What the turn would cost at API list prices, in US dollars, as
    /// estimated by the CLI on the user's machine. It is an estimate, not a
    /// charge: label it as one. A user on a subscription is not billed per
    /// message.
    pub estimated_cost_usd: Option<f64>,
}

/// Tokens used by a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    /// Input read back from the prompt cache.
    pub cache_read: u64,
    /// Input written to the prompt cache.
    pub cache_write: u64,
}

/// The kind of error the service reported, in the CLI's classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    Authentication,
    Billing,
    RateLimit,
    InvalidRequest,
    Server,
    /// The model asked for is not one the service knows, or not one the
    /// account may use.
    ModelNotFound,
    /// Not classified, which includes the network being unreachable.
    Other,
}

/// Why a turn failed. The `Display` text is written for the person using
/// Trill and says what they can do about it.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FailureReason {
    /// The CLI is not where it was found.
    #[error(
        "Claude Code could not be found at {}. Install it from https://claude.com/claude-code, then try again.",
        path.display()
    )]
    CliMissing { path: PathBuf },

    /// The CLI has no login. This crate never signs anyone in: the user does
    /// that in the CLI itself.
    #[error("Claude Code is not signed in. Open a terminal, run `claude`, sign in there, then try again.")]
    NotLoggedIn,

    /// The user's allowance is used up for now.
    #[error("Your Claude usage limit has been reached. Claude Code said: {message}")]
    UsageLimitReached {
        /// The CLI's own words, which usually say when the limit resets.
        message: String,
        resets_at: Option<SystemTime>,
    },

    /// The CLI could not reach Trill's MCP server, so the assistant would
    /// have had no tools. The turn was stopped before the model was asked.
    #[error("The assistant could not connect to Trill at {url} ({status}). Restart Trill and try again.")]
    McpServerUnavailable {
        url: String,
        /// The CLI's word for the state of the connection, such as `failed`.
        status: String,
    },

    /// The CLI offered the model something other than Trill's tools. The turn
    /// was stopped before the model was asked: the assistant is only allowed
    /// to act through Trill.
    #[error(
        "This version of Claude Code offers the assistant tools that Trill did not ask for ({}), so the assistant \
         was stopped. Update Trill.",
        tools.join(", ")
    )]
    UnexpectedTools { tools: Vec<String> },

    /// The conversation being resumed is not on this computer any more. The
    /// CLI keeps conversations for a limited time. The stored id has been
    /// forgotten, so the next message starts a new conversation.
    #[error(
        "The earlier conversation is no longer on this computer, so the assistant cannot remember it. Send your \
         message again to start a new one."
    )]
    SessionNotFound,

    /// The assistant used every round trip it was allowed. What it did so
    /// far stays done.
    #[error("The assistant stopped after {limit} steps. Ask it to continue if it was not finished.")]
    TurnLimitReached { limit: u32 },

    /// The model that was asked for cannot be used: the service does not
    /// know the name, or the account has no access to that model. Nothing
    /// was used. Which of the two it is, the service does not say, and
    /// nothing is concluded here about what the user's plan includes.
    #[error(
        "Claude Code could not use the model that was asked for. It said: {message} In Trill, the model is chosen \
         under the assistant\u{2019}s message field."
    )]
    ModelUnavailable {
        /// The CLI's own words, which name the model.
        message: String,
    },

    /// The service refused or failed the request.
    #[error("Claude Code could not complete the request: {message}")]
    Api { kind: ApiErrorKind, message: String },

    /// The CLI could not be started at all.
    #[error("Trill could not start Claude Code: {detail}")]
    CannotStart { detail: String },

    /// The CLI stopped without saying how the turn ended.
    #[error("Claude Code stopped unexpectedly{}. {stderr_tail}", exit_code.map(|code| format!(" (exit code {code})")).unwrap_or_default())]
    CliExited {
        exit_code: Option<i32>,
        /// The end of what the CLI wrote to its error stream.
        stderr_tail: String,
    },

    /// The CLI's output could not be followed.
    #[error("Claude Code answered in a way Trill does not understand ({detail}). Updating Trill may help.")]
    Protocol { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_must_be_uuids() {
        let id = SessionId::parse("39367D22-e40b-4a04-9358-33e91538ff0d").expect("a UUID");
        assert_eq!(id.as_str(), "39367d22-e40b-4a04-9358-33e91538ff0d");

        for bad in [
            "",
            "--dangerously-skip-permissions",
            "39367d22e40b4a04935833e91538ff0d",
            " 39367d22-e40b-4a04-9358-33e91538ff0d",
        ] {
            assert_eq!(SessionId::parse(bad), None, "{bad:?} must be refused");
        }
    }

    #[test]
    fn failures_tell_the_user_what_to_do() {
        assert!(FailureReason::NotLoggedIn.to_string().contains("run `claude`"));

        let exited = FailureReason::CliExited { exit_code: Some(1), stderr_tail: "error: unknown option".into() };
        assert_eq!(exited.to_string(), "Claude Code stopped unexpectedly (exit code 1). error: unknown option");

        let tools = FailureReason::UnexpectedTools { tools: vec!["Bash".into(), "Read".into()] };
        assert!(tools.to_string().contains("(Bash, Read)"));

        let model = FailureReason::ModelUnavailable { message: "There's an issue with the selected model (x).".into() };
        assert_eq!(
            model.to_string(),
            "Claude Code could not use the model that was asked for. It said: There's an issue with the selected \
             model (x). In Trill, the model is chosen under the assistant\u{2019}s message field."
        );
    }

    #[test]
    fn only_three_events_end_a_turn() {
        assert!(Event::Cancelled.is_terminal());
        assert!(Event::Failed(FailureReason::NotLoggedIn).is_terminal());
        assert!(!Event::Text("hi".into()).is_terminal());
        assert!(!Event::Notice(Notice::PermissionDenied { tool: "render".into() }).is_terminal());
    }
}
