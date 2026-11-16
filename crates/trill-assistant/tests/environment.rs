//! What reaches the CLI's environment.
//!
//! This is one test in a file of its own because it changes the environment
//! of the test process, which every test in the same program would see.

mod support;

use support::{Stage, init, say, success, turn};
use trill_assistant::{AssistantConfig, Conversation, Event, LoginMethod, Notice, REDIRECTING_VARIABLES, detect};

#[test]
fn variables_that_redirect_authentication_do_not_reach_the_cli() {
    // SAFETY: this is the only test in this program, so nothing else reads
    // or writes the environment while these calls run. The values are
    // placeholders; no real key is involved.
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", "placeholder-not-a-key");
        std::env::set_var("ANTHROPIC_BASE_URL", "http://127.0.0.1:9/");
        std::env::set_var("CLAUDE_CODE_USE_BEDROCK", "1");
        std::env::set_var("CLAUDE_CODE_ENTRYPOINT", "some-parent-session");
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CONFIG_DIR", "where-the-user-keeps-their-login");
        std::env::set_var("ENABLE_TOOL_SEARCH", "true");
    }

    let stage = Stage::new("environment");
    stage.script(&[init().as_str(), say("Done.").as_str(), success()].join("\n"));

    // By default: removed, and said so once.
    let mut conversation = stage.conversation();
    let ignored = |events: &[Event]| -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Notice(Notice::EnvironmentVariableIgnored { name }) => Some(name.clone()),
                _ => None,
            })
            .collect()
    };
    let first = turn(&mut conversation, "hello");
    assert_eq!(ignored(&first), ["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_USE_BEDROCK"]);
    assert!(matches!(first.last(), Some(Event::Finished(_))));
    assert_eq!(ignored(&turn(&mut conversation, "again")), [] as [&str; 0], "reported once per conversation");

    for number in [1, 2] {
        let launch = stage.launch(number);
        for name in REDIRECTING_VARIABLES {
            assert_eq!(launch.variable(name), None, "{name} reached the CLI");
        }
        // Left over from a session Trill was started from: gone, silently.
        assert_eq!(launch.variable("CLAUDE_CODE_ENTRYPOINT"), None);
        assert_eq!(launch.variable("CLAUDECODE"), None);
        // Where the user's own login lives is theirs to choose.
        assert_eq!(launch.variable("CLAUDE_CONFIG_DIR"), Some("where-the-user-keeps-their-login"));
        // Set for every run, whatever the environment said.
        assert_eq!(launch.variable("ENABLE_TOOL_SEARCH"), Some("false"));
        assert_eq!(launch.variable("ENABLE_CLAUDEAI_MCP_SERVERS"), Some("false"));
        assert_eq!(launch.variable("CLAUDE_CODE_DISABLE_CLAUDE_MDS"), Some("1"));
        assert_eq!(launch.variable("CLAUDE_CODE_DISABLE_AUTO_MEMORY"), Some("1"));
        assert_eq!(launch.variable("DISABLE_AUTOUPDATER"), Some("1"));
    }

    // Opted out: passed through, with nothing to report.
    let config = AssistantConfig { keep_redirecting_environment: true, ..stage.config() };
    let mut conversation = Conversation::new(config, stage.cli()).unwrap();
    assert_eq!(ignored(&turn(&mut conversation, "hello")), [] as [&str; 0]);
    let launch = stage.launch(3);
    assert_eq!(launch.variable("ANTHROPIC_API_KEY"), Some("placeholder-not-a-key"));
    assert_eq!(launch.variable("CLAUDE_CODE_USE_BEDROCK"), Some("1"));
    assert_eq!(launch.variable("CLAUDE_CODE_ENTRYPOINT"), None, "never inherited, opted out or not");

    // Detection asks the CLI about the login in the same cleaned environment,
    // so that it reports the login a message would actually use.
    let status = detect(&stage.config()).expect("the fake CLI is ready");
    assert_eq!(status.login, LoginMethod::Subscription { plan: Some("pro".into()) });
    let asked = stage.auth_launch();
    assert_eq!(asked.arguments(), ["auth", "status", "--json"]);
    assert_eq!(asked.variable("ANTHROPIC_API_KEY"), None);
    assert_eq!(asked.variable("CLAUDE_CODE_ENTRYPOINT"), None);

    let kept = AssistantConfig { keep_redirecting_environment: true, ..stage.config() };
    detect(&kept).expect("the fake CLI is ready");
    assert_eq!(stage.auth_launch().variable("ANTHROPIC_API_KEY"), Some("placeholder-not-a-key"));
}
