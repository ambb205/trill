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
    /// The CLI is up, connected to Trill, and about to ask the model. A long
    /// request is worked on in several runs of the CLI, and this comes again
    /// at the start of each.
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

    /// The assistant is looking something up on the web, with the CLI's own
    /// tools. Sent only when [`web`](crate::AssistantConfig::web) is on.
    WebStarted {
        /// Pairs this event with its [`WebFinished`](Event::WebFinished).
        id: String,
        lookup: WebLookup,
    },

    /// A web lookup returned.
    WebFinished {
        id: String,
        /// What came back: for a search, titles and addresses; for a page,
        /// what a small model made of it in answer to the question. `Err`
        /// when the search or the page could not be had.
        result: Result<String, String>,
    },

    /// The assistant's plan for the request changed: a step was written
    /// down, begun, finished or dropped.
    Plan(PlanChange),

    /// How far a request has come: sent each time the model is asked again,
    /// and when a run of the CLI has reported what it cost.
    Progress(Progress),

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

/// What the assistant is looking up on the web.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebLookup {
    /// A search, which Anthropic's service runs. The query is all of Trill's
    /// that leaves the machine for it.
    Search { query: String },
    /// One page, which the CLI fetches from this machine and has a small
    /// model read.
    Page {
        /// The address asked for.
        url: String,
        /// What the assistant wants to know from the page.
        question: String,
    },
}

/// How a step of the assistant's plan stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
}

/// One step of the assistant's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    /// The CLI's number for the step. Steps are listed in the order of
    /// their numbers.
    pub id: String,
    /// What is to be done: "Write the drums".
    pub title: String,
    pub status: StepStatus,
}

/// A change to the assistant's plan. The plan itself is the app's to keep:
/// apply the changes in the order they come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanChange {
    /// A step was written down, not yet begun.
    Added {
        id: String,
        title: String,
        /// The step as it reads while it is being worked on ("Writing the
        /// drums"), if the assistant gave one.
        doing: Option<String>,
    },
    /// A step was begun, finished, put back or worded anew. What is `None`
    /// stays as it was.
    Changed { id: String, status: Option<StepStatus>, title: Option<String>, doing: Option<String> },
    /// A step was dropped.
    Removed { id: String },
    /// The whole plan as the CLI holds it, which the assistant asked to see.
    /// Steps the app has that are not in it are gone; what the app knows
    /// about the others beyond title and state it keeps.
    Listed(Vec<PlanStep>),
}

/// How far a request has come.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// How many times the model has been asked so far for this request:
    /// the steps that [`max_turns`](crate::AssistantConfig::max_turns)
    /// counts.
    pub steps: u32,
    /// What the request would have cost so far at API list prices, as the
    /// CLI estimates it, for the part of the request the CLI has reported
    /// on. It reports when one of its runs ends, so this is `None` until
    /// the first run has, and is behind by the run under way after that.
    /// An estimate, never a charge.
    pub estimated_cost_usd: Option<f64>,
    /// How many of the steps that estimate covers.
    pub costed_steps: u32,
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

    /// The conversation has grown past what the model can hold at once, and
    /// the CLI is writing a summary of the earlier part to carry on from.
    /// [`Compacted`](Notice::Compacted) or
    /// [`CompactionFailed`](Notice::CompactionFailed) follows.
    Compacting,

    /// The CLI replaced the earlier part of the conversation with a summary
    /// of it and is carrying on. The request goes on by itself; what the
    /// assistant knows of its early steps is now what the summary says. Its
    /// plan and the project are not part of the conversation and are as
    /// they were.
    Compacted {
        /// How large the conversation had grown, in tokens, if the CLI said.
        tokens_before: Option<u64>,
        /// How large it is with the summary in place.
        tokens_after: Option<u64>,
    },

    /// The CLI could not summarise the conversation. What it does next is
    /// its own affair; most likely the service refuses the next request as
    /// too long, and the turn fails saying so.
    CompactionFailed {
        /// The CLI's word for what went wrong.
        detail: String,
    },
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
            Notice::Compacting => f.write_str(
                "The conversation has grown past what the model can hold at once. Claude Code is summarising the \
                 earlier part\u{2026}",
            ),
            Notice::Compacted { tokens_before, tokens_after } => {
                f.write_str("The conversation had grown past what the model can hold at once")?;
                if let Some(before) = tokens_before {
                    write!(f, " ({} tokens)", thousands(*before))?;
                }
                f.write_str(", so Claude Code summarised the earlier part")?;
                if let Some(after) = tokens_after {
                    write!(f, " ({} tokens now)", thousands(*after))?;
                }
                f.write_str(
                    " and is carrying on. The song and the plan are untouched; what the assistant remembers of its \
                     early steps is now the summary.",
                )
            }
            Notice::CompactionFailed { detail } => write!(
                f,
                "The conversation has grown past what the model can hold at once, and Claude Code could not \
                 summarise it ({detail}). If the assistant stops, start a new conversation: the song stays as it is."
            ),
        }
    }
}

/// A whole number with its thousands set apart: `985,053`.
fn thousands(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// How a finished turn went. For a request that took several runs of the
/// CLI, each figure is the sum over them.
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

    /// The request used every round trip it was allowed
    /// ([`max_turns`](crate::AssistantConfig::max_turns)). What it did so
    /// far stays done, and the conversation can be told to carry on.
    #[error(
        "The assistant stopped at the limit of {limit} steps for one request. It can carry on from where it is; \
         the limit is there to catch an assistant going round in circles."
    )]
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
        let progress = Progress { steps: 100, estimated_cost_usd: Some(1.0), costed_steps: 100 };
        assert!(!Event::Progress(progress).is_terminal());
        assert!(!Event::Plan(PlanChange::Removed { id: "1".into() }).is_terminal());
        assert!(!Event::WebFinished { id: "w".into(), result: Err("no such page".into()) }.is_terminal());
        assert!(!Event::Notice(Notice::Compacting).is_terminal());
    }

    #[test]
    fn a_summarised_conversation_is_said_plainly() {
        let said = Notice::Compacted { tokens_before: Some(985_053), tokens_after: Some(644) }.to_string();
        assert_eq!(
            said,
            "The conversation had grown past what the model can hold at once (985,053 tokens), so Claude Code \
             summarised the earlier part (644 tokens now) and is carrying on. The song and the plan are untouched; \
             what the assistant remembers of its early steps is now the summary."
        );
        let bare = Notice::Compacted { tokens_before: None, tokens_after: None }.to_string();
        assert!(bare.contains("at once, so Claude Code summarised the earlier part and is carrying on."), "{bare}");
        assert!(Notice::Compacting.to_string().ends_with("the earlier part\u{2026}"));
        let failed = Notice::CompactionFailed { detail: "the summary was refused".into() }.to_string();
        assert!(failed.contains("(the summary was refused)") && failed.contains("new conversation"), "{failed}");
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
