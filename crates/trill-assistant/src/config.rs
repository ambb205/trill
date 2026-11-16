//! What the app decides about the assistant.

use std::fmt;
use std::path::PathBuf;

/// The MCP endpoint Trill serves by default.
pub const DEFAULT_MCP_URL: &str = "http://127.0.0.1:7777/mcp";

/// The name Trill's MCP server goes by. Tool names carry it as a prefix
/// (`mcp__trill__create_track`), which this crate strips again.
pub const DEFAULT_MCP_SERVER_NAME: &str = "trill";

/// How many model round trips one message may take before the turn is stopped.
/// A whole arrangement takes dozens of tool calls; this only catches a loop.
pub const DEFAULT_MAX_TURNS: u32 = 80;

/// The instructions added to the CLI's own system prompt.
///
/// Written for an assistant whose only tools are Trill's and whose reader is a
/// musician watching the project change while it works.
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are the assistant built into Trill, a digital audio workstation (DAW) for composing, arranging and mixing music. \
You are running in a chat panel inside the Trill window, next to the project the user has open.

How you work here
- Your only tools are Trill's own (their names start with mcp__trill__). They act on the project that is open right \
now, and the user watches every edit appear live as you make it. You have no shell or web access, and you reach files \
only through those tools (saving and loading projects, rendering, importing and exporting); if a request needs more \
than that, say so plainly instead of trying.
- A tool that writes a file may refuse a path outside the folders Trill lets you write to. Its reply says where it can \
write: use a plain file name, or tell the user what it said. Do not look for another way around it.
- Look before you edit: call get_project (or the relevant get_ tool) instead of guessing ids, positions or names. A \
<trill-context> block in a message was added by Trill, not typed by the user: it describes what the user was looking \
at (selection, playhead, loop) when they sent it. Treat it as a hint and the tools as the truth.
- Work in small, verifiable steps: make one change, read the tool's reply, then make the next. If a tool reports an \
error, read it and fix the cause instead of repeating the call.
- Prefer musical tools to raw edits where one exists: for example transform_notes to transpose, quantize or humanize \
instead of rewriting every note, and duplicate_clip or loop lengths instead of re-entering repeats.
- Every edit can be undone with the history tool, and for the user everything you change while answering one message \
is a single Undo. Ask before doing something destructive the user did not clearly request: deleting tracks or clips, \
replacing notes, starting or loading another project.
- You cannot hear. Never describe how something sounds as though you had listened to it. You may reason from what you \
can read: the notes, the levels, and the loudness and spectrum figures that render reports.

How you answer
- The chat panel is narrow, so be brief. After working, say in a sentence or two what you changed, using the names \
the user sees (track and clip names, bar numbers) and not internal ids.
- The panel shows simple Markdown: paragraphs, **bold** for a name or a value that matters, short lists, and \
`code` for something to be typed exactly. Use it sparingly, as in a note to a colleague. No tables, images or HTML \
(they are shown as typed), and a heading only when an answer really has parts.
- Talk like a helpful studio colleague, in musical terms (bars, beats, note names), unless the user asks for \
technical detail.
- If a request is ambiguous in a way that matters, ask one short question. Otherwise make a sensible musical choice \
and mention it.
";

/// How hard the model should think, as the CLI's `--effort` understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    ExtraHigh,
    Max,
}

impl Effort {
    pub(crate) fn as_flag_value(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::ExtraHigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// Everything a [`Conversation`](crate::Conversation) needs to know.
///
/// Build one with [`AssistantConfig::new`] and change the fields that differ
/// from the defaults. The same values must be used whenever a conversation is
/// resumed: the CLI finds a conversation by its working directory, and does
/// not remember flags between runs.
#[derive(Debug, Clone)]
pub struct AssistantConfig {
    /// Where the CLI is, when the user has pointed Trill at it. `None` lets
    /// [`detect`](crate::detect) look on `PATH` and in the places the CLI's
    /// installers use.
    pub cli_path: Option<PathBuf>,

    /// Where Trill's MCP server listens. It must be running before a message
    /// is sent; the turn fails with
    /// [`McpServerUnavailable`](crate::FailureReason::McpServerUnavailable)
    /// otherwise.
    pub mcp_url: String,

    /// The name the MCP server is registered under for the run. Letters,
    /// digits, `_` and `-` only.
    pub mcp_server_name: String,

    /// Tools of Trill's that the assistant is not offered, by Trill's own
    /// names (`"load_project"`). Empty by default.
    ///
    /// The assistant has no file access of its own, but some of Trill's tools
    /// take a path and read or write wherever it points (saving, loading,
    /// rendering, importing and exporting). An app that wants the assistant
    /// kept to the open project withholds those here.
    pub withheld_tools: Vec<String>,

    /// Instructions added to the CLI's own system prompt.
    ///
    /// The CLI records the system prompt when a conversation starts and
    /// reuses that record on every later turn and resume, so a change here
    /// only reaches new conversations. Put anything that changes from message
    /// to message in the context passed to
    /// [`Conversation::send`](crate::Conversation::send) instead.
    pub system_prompt: String,

    /// A model alias (`"sonnet"`) or full model name. `None` leaves the
    /// choice to the CLI, which is what the user has set up for themselves.
    ///
    /// The CLI takes any name and passes it on; one the service does not
    /// know, or that the account may not use, fails the turn with
    /// [`ModelUnavailable`](crate::FailureReason::ModelUnavailable) before
    /// anything is used. Whether a model is included in the user's plan is
    /// the service's to say and never this crate's. See [`MODEL_ALIASES`].
    pub model: Option<String>,

    /// `None` leaves the CLI's default.
    pub effort: Option<Effort>,

    /// The most model round trips one message may take. `None` removes the
    /// limit.
    pub max_turns: Option<u32>,

    /// The directory the CLI runs in. Use a folder of Trill's own (in the
    /// app data directory), never the user's project folder: the CLI files
    /// conversations by this path, so it must stay the same for a
    /// conversation to be resumed, and anything in it is the CLI's to read.
    /// Created if missing.
    pub working_dir: PathBuf,

    /// Where the two small files handed to the CLI are written (the MCP
    /// server address and the system prompt). They are removed when the
    /// conversation is dropped. Created if missing.
    pub temp_dir: PathBuf,

    /// Leave variables that redirect authentication or billing in the CLI's
    /// environment. Off by default, so that a stray `ANTHROPIC_API_KEY` in
    /// the environment Trill was started from cannot quietly bill an API
    /// account instead of using the login the user expects. See
    /// [`REDIRECTING_VARIABLES`](crate::REDIRECTING_VARIABLES).
    pub keep_redirecting_environment: bool,
}

impl AssistantConfig {
    /// A configuration with Trill's defaults, running the CLI in
    /// `working_dir` and keeping its temporary files in a `tmp` folder there.
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        let working_dir = working_dir.into();
        let temp_dir = working_dir.join("tmp");
        Self {
            cli_path: None,
            mcp_url: DEFAULT_MCP_URL.to_owned(),
            mcp_server_name: DEFAULT_MCP_SERVER_NAME.to_owned(),
            withheld_tools: Vec::new(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_owned(),
            model: None,
            effort: None,
            max_turns: Some(DEFAULT_MAX_TURNS),
            working_dir,
            temp_dir,
            keep_redirecting_environment: false,
        }
    }

    /// Checks the values that end up on the CLI's command line, so that a
    /// mistake is reported here, by name, and not as a confusing CLI error.
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if !is_plain_name(&self.mcp_server_name) {
            return Err(ConfigError::ServerName(self.mcp_server_name.clone()));
        }
        if let Some(tool) = self.withheld_tools.iter().find(|tool| !is_plain_name(tool)) {
            return Err(ConfigError::ToolName(tool.clone()));
        }
        if !(self.mcp_url.starts_with("http://") || self.mcp_url.starts_with("https://")) {
            return Err(ConfigError::McpUrl(self.mcp_url.clone()));
        }
        if let Some(model) = self.model.as_ref().filter(|model| !is_model_name(model)) {
            return Err(ConfigError::Model(model.clone()));
        }
        if self.max_turns == Some(0) {
            return Err(ConfigError::MaxTurns);
        }
        Ok(())
    }
}

/// The names the CLI takes for "the newest model of this family", from the
/// most capable down: its own list, read out of CLI 2.1.325 (whose help
/// names the first three of them), which also has a few for special uses
/// (`best`, `opusplan`, and `[1m]` forms). `fable`, `opus` and `sonnet`
/// were each asked for in a real turn with that CLI and answered, the CLI
/// reporting the full name it had made of each (`claude-fable-5-2`,
/// `claude-opus-5-5`, `claude-sonnet-5-5`); `haiku` is what this crate's
/// real-CLI tests run on. A CLI that does not know one passes it on like
/// any other name, and the service refuses it.
///
/// For a chooser to offer beside "whatever the CLI is set up to use"
/// (`None`) and a name typed by hand. Listing a name promises nothing
/// about the user's plan.
pub const MODEL_ALIASES: &[&str] = &["fable", "opus", "sonnet", "haiku"];

/// Whether `name` can be given to the CLI as a model: not empty, not
/// something the command line would take for an option, and without spaces
/// or control characters. Not whether such a model exists: only the service
/// knows that.
pub fn is_model_name(name: &str) -> bool {
    !(name.is_empty() || name.starts_with('-') || name.chars().any(|c| c.is_whitespace() || c.is_control()))
}

/// Whether `name` is made of letters, digits, `_` and `-` only, as the names
/// of MCP servers and tools are.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A value in an [`AssistantConfig`] that cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The MCP server name has characters other than letters, digits, `_`, `-`.
    ServerName(String),
    /// A withheld tool's name has characters other than letters, digits, `_`,
    /// `-`.
    ToolName(String),
    /// The MCP address is not an `http://` or `https://` URL.
    McpUrl(String),
    /// The model name is empty, starts with `-` or contains spaces.
    Model(String),
    /// A turn limit of zero would let the assistant do nothing.
    MaxTurns,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::ServerName(name) => {
                write!(f, "the MCP server name {name:?} may only contain letters, digits, _ and -")
            }
            ConfigError::ToolName(name) => {
                write!(f, "the tool name {name:?} may only contain letters, digits, _ and -")
            }
            ConfigError::McpUrl(url) => write!(f, "the MCP address {url:?} is not an http:// or https:// URL"),
            ConfigError::Model(model) => write!(f, "{model:?} is not a model name"),
            ConfigError::MaxTurns => f.write_str("the turn limit must be at least 1"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_valid() {
        assert_eq!(AssistantConfig::new("data").validate(), Ok(()));
    }

    #[test]
    fn values_that_would_confuse_the_command_line_are_refused() {
        let base = AssistantConfig::new("data");

        let config = AssistantConfig { mcp_server_name: "tr ill".into(), ..base.clone() };
        assert_eq!(config.validate(), Err(ConfigError::ServerName("tr ill".into())));

        let config = AssistantConfig { withheld_tools: vec!["render".into(), "a,b".into()], ..base.clone() };
        assert_eq!(config.validate(), Err(ConfigError::ToolName("a,b".into())));

        let config = AssistantConfig { mcp_url: "127.0.0.1:7777".into(), ..base.clone() };
        assert_eq!(config.validate(), Err(ConfigError::McpUrl("127.0.0.1:7777".into())));

        let config = AssistantConfig { model: Some("--dangerously-skip-permissions".into()), ..base.clone() };
        assert!(matches!(config.validate(), Err(ConfigError::Model(_))));

        let config = AssistantConfig { max_turns: Some(0), ..base };
        assert_eq!(config.validate(), Err(ConfigError::MaxTurns));
    }

    #[test]
    fn a_model_name_is_anything_the_command_line_takes_as_one() {
        for name in MODEL_ALIASES.iter().copied().chain(["claude-haiku-5-5", "opus[1m]", "us.vendor.model-v1:0"]) {
            assert!(is_model_name(name), "{name:?}");
            let config = AssistantConfig { model: Some(name.to_owned()), ..AssistantConfig::new("data") };
            assert_eq!(config.validate(), Ok(()), "{name:?}");
        }
        for name in ["", "-m", "--resume=x", "son net", "sonnet\n", "a\tb"] {
            assert!(!is_model_name(name), "{name:?}");
        }
        let mut unique = MODEL_ALIASES.to_vec();
        unique.dedup();
        assert_eq!(unique.len(), MODEL_ALIASES.len());
        assert!(MODEL_ALIASES.iter().all(|alias| !alias.contains("claude")), "aliases name a family, not the maker");
    }

    #[test]
    fn the_default_prompt_covers_what_the_assistant_must_know() {
        for phrase in [
            "Trill",
            "live",
            "small, verifiable steps",
            "cannot hear",
            "transform_notes",
            "be brief",
            "a single Undo",
            "simple Markdown",
            "No tables",
        ] {
            assert!(DEFAULT_SYSTEM_PROMPT.contains(phrase), "the default prompt lost {phrase:?}");
        }
    }

    #[test]
    fn the_default_prompt_does_not_talk_the_assistant_out_of_its_tools() {
        // Saving, loading, rendering and the MIDI tools are the assistant's
        // to use; what they may write is the server's to decide, tool call
        // by tool call.
        assert!(DEFAULT_SYSTEM_PROMPT.contains("saving and loading projects, rendering, importing and exporting"));
        for phrase in ["no file", "cannot save", "cannot load", "cannot render", "cannot export", "not offered"] {
            assert!(!DEFAULT_SYSTEM_PROMPT.contains(phrase), "the default prompt says {phrase:?}");
        }
    }
}
