//! Shared by the integration tests: a directory holding a copy of the fake
//! CLI (`fake_cli.rs`) and the files that tell it how to behave.

// Each test file uses its own part of this.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use trill_assistant::{AssistantConfig, CliInstallation, CliVersion, Conversation, Event, SessionId};

/// The session id the scripted `init` line announces.
pub const SESSION: &str = "5e55104d-0000-4000-8000-000000000001";

/// How long a scripted turn may take before the test gives up on it.
const TURN_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Stage {
    directory: PathBuf,
}

impl Stage {
    /// A fresh directory, named after the test, with the fake CLI in it.
    pub fn new(name: &str) -> Self {
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("stage-{name}"));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("the stage directory");
        fs::copy(env!("CARGO_BIN_EXE_trill-assistant-fake-cli"), Self::program_in(&directory)).expect("the fake CLI");
        Self { directory }
    }

    /// The directory another process set up with [`Stage::new`].
    pub fn reopen(name: &str) -> Self {
        Self { directory: Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("stage-{name}")) }
    }

    fn program_in(directory: &Path) -> PathBuf {
        directory.join(if cfg!(windows) { "cli.exe" } else { "cli" })
    }

    pub fn program(&self) -> PathBuf {
        Self::program_in(&self.directory)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    pub fn write(&self, name: &str, contents: &str) {
        fs::write(self.path(name), contents).expect("a stage file");
    }

    /// The script every turn follows, unless it has one of its own.
    pub fn script(&self, script: &str) {
        self.write("turn.script", script);
    }

    /// The script the `number`th turn follows, counting from one.
    pub fn script_for(&self, number: u32, script: &str) {
        self.write(&format!("turn-{number}.script"), script);
    }

    /// Makes a recorded transcript available to `replay`.
    pub fn fixture(&self, name: &str) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name);
        fs::copy(&source, self.path(name)).unwrap_or_else(|error| panic!("fixture {}: {error}", source.display()));
    }

    pub fn cli(&self) -> CliInstallation {
        CliInstallation { path: self.program(), version: CliVersion::new(2, 1, 325) }
    }

    pub fn config(&self) -> AssistantConfig {
        AssistantConfig { cli_path: Some(self.program()), ..AssistantConfig::new(self.path("work")) }
    }

    pub fn conversation(&self) -> Conversation {
        Conversation::new(self.config(), self.cli()).expect("a valid configuration")
    }

    /// How the fake CLI was started for the `number`th turn.
    pub fn launch(&self, number: u32) -> Launch {
        self.launch_record(&format!("launch-{number}.json"))
    }

    /// How the fake CLI was last started to report on the login.
    pub fn auth_launch(&self) -> Launch {
        self.launch_record("launch-auth.json")
    }

    fn launch_record(&self, file: &str) -> Launch {
        let path = self.path(file);
        let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        Launch(serde_json::from_str(&text).expect("a launch record"))
    }

    pub fn launches(&self) -> u32 {
        (1..).find(|number| !self.path(&format!("launch-{number}.json")).exists()).expect("a number") - 1
    }

    /// Waits for the helper started by the `child` directive to show itself.
    pub fn await_helper(&self) {
        let deadline = Instant::now() + TURN_TIMEOUT;
        while !self.path("heartbeat").exists() {
            assert!(Instant::now() < deadline, "the helper never started");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether the helper is running, judged by its heartbeat. A helper on a
    /// busy machine gets a few seconds to show that it is; one that has been
    /// killed is believed after a second of silence.
    pub fn helper_is_alive(&self) -> bool {
        self.heartbeat_changes_within(Duration::from_secs(5))
    }

    /// Whether the helper has stopped: its heartbeat stays as it is.
    pub fn helper_has_stopped(&self) -> bool {
        !self.heartbeat_changes_within(Duration::from_secs(1))
    }

    fn heartbeat_changes_within(&self, window: Duration) -> bool {
        let beat = || fs::read_to_string(self.path("heartbeat")).unwrap_or_default();
        let (before, deadline) = (beat(), Instant::now() + window);
        while Instant::now() < deadline {
            if beat() != before {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

pub struct Launch(Value);

impl Launch {
    pub fn arguments(&self) -> Vec<String> {
        self.0["arguments"].as_array().expect("arguments").iter().map(|a| a.as_str().unwrap().to_owned()).collect()
    }

    pub fn has_argument(&self, argument: &str) -> bool {
        self.arguments().iter().any(|candidate| candidate == argument)
    }

    /// What arrived on standard input: the message.
    pub fn input(&self) -> &str {
        self.0["input"].as_str().expect("input")
    }

    pub fn working_directory(&self) -> PathBuf {
        PathBuf::from(self.0["working_directory"].as_str().expect("a working directory"))
    }

    pub fn variable(&self, name: &str) -> Option<&str> {
        self.0["environment"][name].as_str()
    }

    pub fn had_console_window(&self) -> bool {
        self.0["console_window"].as_bool().expect("the console flag")
    }
}

/// The `init` line of a healthy start: Trill connected, and nothing else.
pub fn init() -> String {
    format!(
        r#"line {{"type":"system","subtype":"init","session_id":"{SESSION}","model":"test-model","apiKeySource":"none","tools":["mcp__trill__get_project","mcp__trill__set_project"],"mcp_servers":[{{"name":"trill","status":"connected"}}]}}"#
    )
}

/// A `result` line for a turn that went well.
pub fn success() -> &'static str {
    r#"line {"type":"result","subtype":"success","is_error":false,"num_turns":1,"duration_ms":10,"total_cost_usd":0.001,"usage":{"input_tokens":1,"output_tokens":2}}"#
}

/// A complete reply that was not streamed.
pub fn say(text: &str) -> String {
    let message = serde_json::json!({
        "type": "assistant",
        "message": { "id": format!("msg_{}", text.len()), "content": [{ "type": "text", "text": text }] },
    });
    format!("line {message}")
}

pub fn session() -> SessionId {
    SessionId::parse(SESSION).expect("a session id")
}

/// Sends a message and collects the turn's events, terminal event last.
pub fn turn(conversation: &mut Conversation, message: &str) -> Vec<Event> {
    conversation.send(message, None).expect("the message to be sent");
    rest_of_turn(conversation)
}

/// Collects events until the terminal one.
pub fn rest_of_turn(conversation: &mut Conversation) -> Vec<Event> {
    let deadline = Instant::now() + TURN_TIMEOUT;
    let mut events = Vec::new();
    loop {
        match conversation.wait(Duration::from_millis(100)) {
            Some(event) => {
                let last = event.is_terminal();
                events.push(event);
                if last {
                    assert!(!conversation.is_busy(), "a finished turn leaves the conversation free");
                    assert_eq!(conversation.poll(), None, "nothing follows a terminal event");
                    return events;
                }
            }
            None => assert!(Instant::now() < deadline, "the turn did not end; so far: {events:?}"),
        }
    }
}

/// Everything the assistant said during the turn, joined.
pub fn said(events: &[Event]) -> String {
    events.iter().filter_map(|event| if let Event::Text(piece) = event { Some(piece.as_str()) } else { None }).collect()
}
