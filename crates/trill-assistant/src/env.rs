//! The environment the CLI is started with.
//!
//! The CLI inherits Trill's environment, and three kinds of variable in it
//! matter. Some would send the run somewhere other than the user's own login
//! ([`REDIRECTING_VARIABLES`]). Some are left behind by a Claude Code session
//! that Trill happened to be started from. And a few are set here on purpose,
//! to switch off parts of the CLI that an assistant confined to Trill must not
//! have.
//!
//! Names and meanings are from Claude Code's environment variable reference
//! (<https://code.claude.com/docs/en/env-vars>) as of CLI 2.1.325.

use std::ffi::OsStr;
use std::process::Command;

/// Variables that make the CLI authenticate, or bill, somewhere other than
/// the login the user made with `claude`, or that put it in a mode in which
/// that login is never read. Removed from the CLI's environment unless
/// [`keep_redirecting_environment`](crate::AssistantConfig::keep_redirecting_environment)
/// is set; each one actually removed is reported with
/// [`Notice::EnvironmentVariableIgnored`](crate::Notice::EnvironmentVariableIgnored).
///
/// The values are never read, only the names.
pub const REDIRECTING_VARIABLES: &[&str] = &[
    // Credentials that take precedence over the stored login.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_PROFILE",
    // Workload identity federation.
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_WORKSPACE_ID",
    // A gateway, and headers meant for one.
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    // Other providers.
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    // Bare mode never reads the stored login; safe mode drops the MCP server
    // the assistant depends on (seen with CLI 2.1.325).
    "CLAUDE_CODE_SIMPLE",
    "CLAUDE_CODE_SAFE_MODE",
];

/// Variables Claude Code sets for the programs it starts. When Trill is
/// launched from inside a Claude Code session (a developer's terminal, an
/// agent running the app) they would make Trill's CLI look like part of that
/// session; with `CLAUDE_CODE_ENTRYPOINT` inherited, for example, the run
/// reports the parent's entry point instead of its own. They are not user
/// configuration, so they are removed without a notice.
const INHERITED_SESSION_VARIABLES: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_EFFORT",
    "CLAUDE_JOB_DIR",
    "CLAUDE_PID",
    // Would make a resumed conversation carry on with a turn the user
    // cancelled.
    "CLAUDE_CODE_RESUME_INTERRUPTED_TURN",
];

/// Variables set for every run, whatever the user's environment says.
const FIXED_VARIABLES: &[(&str, &str)] = &[
    // The MCP servers attached to the user's claude.ai account are fetched
    // even under `--strict-mcp-config`; this is the documented way to stop it.
    ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
    // Tool search hides MCP tools behind a built-in tool, and the built-in
    // tools are switched off. Load Trill's tools directly.
    ("ENABLE_TOOL_SEARCH", "false"),
    // No CLAUDE.md from anywhere, and no memory kept between conversations.
    ("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1"),
    ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
    // Using the assistant must not be what replaces the user's CLI.
    ("DISABLE_AUTOUPDATER", "1"),
];

/// Prepares `command`'s environment and returns the names of the redirecting
/// variables that were present and removed.
pub(crate) fn apply(command: &mut Command, keep_redirecting: bool) -> Vec<String> {
    apply_with(command, keep_redirecting, |name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

fn apply_with(command: &mut Command, keep_redirecting: bool, is_set: impl Fn(&OsStr) -> bool) -> Vec<String> {
    let mut removed = Vec::new();
    if !keep_redirecting {
        for name in REDIRECTING_VARIABLES {
            command.env_remove(name);
            if is_set(OsStr::new(name)) {
                removed.push((*name).to_owned());
            }
        }
    }
    for name in INHERITED_SESSION_VARIABLES {
        command.env_remove(name);
    }
    for (name, value) in FIXED_VARIABLES {
        command.env(name, value);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    /// What `command` would do to each variable: `Some(None)` removes it,
    /// `Some(Some(v))` sets it, `None` leaves it to be inherited.
    fn changes(command: &Command) -> HashMap<OsString, Option<OsString>> {
        command.get_envs().map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned))).collect()
    }

    #[test]
    fn redirecting_variables_are_removed_and_the_ones_present_are_reported() {
        let mut command = Command::new("cli");
        let removed =
            apply_with(&mut command, false, |name| name == "ANTHROPIC_API_KEY" || name == "ANTHROPIC_BASE_URL");

        assert_eq!(removed, ["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL"]);
        let changes = changes(&command);
        for name in REDIRECTING_VARIABLES {
            assert_eq!(changes.get(OsStr::new(name)), Some(&None), "{name} must not reach the CLI");
        }
    }

    #[test]
    fn opting_out_keeps_redirecting_variables_but_not_inherited_session_markers() {
        let mut command = Command::new("cli");
        let removed = apply_with(&mut command, true, |_| true);

        assert!(removed.is_empty());
        let changes = changes(&command);
        assert_eq!(changes.get(OsStr::new("ANTHROPIC_API_KEY")), None);
        assert_eq!(changes.get(OsStr::new("CLAUDE_CODE_ENTRYPOINT")), Some(&None));
        assert_eq!(changes.get(OsStr::new("CLAUDECODE")), Some(&None));
    }

    #[test]
    fn isolation_switches_are_always_set() {
        let mut command = Command::new("cli");
        apply_with(&mut command, false, |_| false);

        let changes = changes(&command);
        for (name, value) in FIXED_VARIABLES {
            assert_eq!(changes.get(OsStr::new(name)), Some(&Some(OsString::from(value))));
        }
    }

    #[test]
    fn the_login_location_is_left_alone() {
        // CLAUDE_CONFIG_DIR is where the user's own login lives when they
        // moved it; removing it would sign them out of the assistant.
        assert!(!REDIRECTING_VARIABLES.contains(&"CLAUDE_CONFIG_DIR"));
        assert!(!INHERITED_SESSION_VARIABLES.contains(&"CLAUDE_CONFIG_DIR"));
    }
}
