//! The whole pipeline, from sending a message to the terminal event, against
//! a fake CLI that replays scripted output (`support/fake_cli.rs`). Nothing
//! here starts the real CLI or uses the network.
//!
//! The transcripts in `fixtures/` were recorded from CLI 2.1.325 and had
//! everything particular to the machine and account removed.

mod support;

use std::fs;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use support::{SESSION, Stage, init, rest_of_turn, said, say, session, success, turn};
use trill_assistant::{
    ApiErrorKind, AssistantConfig, CliInstallation, CliVersion, Conversation, DEFAULT_MCP_URL, DEFAULT_SYSTEM_PROMPT,
    Event, FailureReason, LoginMethod, Notice, SendError, SessionId, TokenUsage, TurnStart, TurnSummary, Unavailable,
    detect,
};

fn script(lines: &[&str]) -> String {
    lines.join("\n")
}

fn failure(events: &[Event]) -> &FailureReason {
    match events.last() {
        Some(Event::Failed(reason)) => reason,
        other => panic!("expected the turn to fail, got {other:?} after {events:?}"),
    }
}

// --- Recorded transcripts ---------------------------------------------------

#[test]
fn a_recorded_reply_streams_as_text_and_finishes() {
    let stage = Stage::new("recorded-reply");
    stage.fixture("plain_reply.jsonl");
    stage.script("replay plain_reply.jsonl");

    let events = turn(&mut stage.conversation(), "what tempo did you set?");

    assert_eq!(
        events.first(),
        Some(&Event::Started(TurnStart {
            session_id: SessionId::parse("00000002-0000-4000-8000-000000000000").unwrap(),
            model: "claude-haiku-5-5".into(),
            api_key_source: None,
        }))
    );
    assert!(events.iter().filter(|event| matches!(event, Event::Text(_))).count() > 5, "the reply arrives in pieces");
    let reply = said(&events);
    assert!(reply.starts_with("1. I can't read C:\\Windows\\win.ini"), "{reply}");
    assert!(reply.ends_with("6. No."), "{reply}");
    assert!(matches!(events.last(), Some(Event::Finished(summary)) if summary.turns == 1));
}

#[test]
fn a_recorded_tool_call_is_reported_start_to_finish() {
    let stage = Stage::new("recorded-tool-call");
    stage.fixture("tool_call.jsonl");
    stage.script("replay tool_call.jsonl");

    let events = turn(&mut stage.conversation(), "set the tempo to 97");

    let Event::ToolCallStarted { id, tool, input } = &events[1] else { panic!("expected a tool call: {events:?}") };
    assert_eq!(tool, "set_project");
    assert_eq!(input, &json!({ "tempo": 97 }));
    let Event::ToolCallFinished { id: finished, tool, result } = &events[2] else {
        panic!("expected the tool's result: {events:?}")
    };
    assert_eq!((finished, tool.as_str()), (id, "set_project"));
    let answer: Value = serde_json::from_str(result.as_ref().expect("the tool succeeded")).expect("JSON from Trill");
    assert_eq!(answer["tempo"], 97.0);

    assert_eq!(said(&events), "I set the project tempo to 97 BPM, and it's confirmed at 97 in the project settings.");
    assert_eq!(
        events.last(),
        Some(&Event::Finished(TurnSummary {
            turns: 2,
            duration: Duration::from_millis(2914),
            usage: TokenUsage { input: 4, output: 90, cache_read: 16460, cache_write: 16626 },
            estimated_cost_usd: Some(0.0035351999999999996),
        }))
    );
}

#[test]
fn a_recorded_denial_is_a_notice_and_a_failed_tool_call() {
    let stage = Stage::new("recorded-denial");
    stage.fixture("permission_denied.jsonl");
    stage.script("replay permission_denied.jsonl");

    let events = turn(&mut stage.conversation(), "set the tempo to 101");

    assert!(events.contains(&Event::Notice(Notice::PermissionDenied { tool: "set_project".into() })));
    let refused = events.iter().find_map(|event| match event {
        Event::ToolCallFinished { tool, result: Err(message), .. } => Some((tool.as_str(), message.as_str())),
        _ => None,
    });
    let (tool, message) = refused.expect("the refused call is reported as failed");
    assert_eq!(tool, "set_project");
    assert!(message.starts_with("Permission to use mcp__trill__set_project has been denied"));
    assert!(said(&events).starts_with("The tempo change to 101 BPM was refused."));
    assert!(matches!(events.last(), Some(Event::Finished(_))));
}

#[test]
fn a_recorded_missing_login_fails_the_turn_without_showing_the_error_as_a_reply() {
    let stage = Stage::new("recorded-no-login");
    stage.fixture("not_logged_in.jsonl");
    stage.script(&script(&["replay not_logged_in.jsonl", "exit 1"]));

    let events = turn(&mut stage.conversation(), "hello");

    assert_eq!(failure(&events), &FailureReason::NotLoggedIn);
    assert_eq!(said(&events), "");
}

#[test]
fn a_recorded_rejected_api_key_is_reported_with_where_the_key_came_from() {
    let stage = Stage::new("recorded-api-key");
    stage.fixture("invalid_api_key.jsonl");
    stage.script(&script(&["replay invalid_api_key.jsonl", "exit 1"]));

    let events = turn(&mut stage.conversation(), "hello");

    let Some(Event::Started(start)) = events.first() else { panic!("expected a start: {events:?}") };
    assert_eq!(start.api_key_source.as_deref(), Some("ANTHROPIC_API_KEY"));
    assert_eq!(
        failure(&events),
        &FailureReason::Api {
            kind: ApiErrorKind::Authentication,
            message: "Invalid API key \u{b7} Fix external API key".into(),
        }
    );
}

#[test]
fn a_recorded_start_without_trill_is_stopped_before_the_model_is_asked() {
    let stage = Stage::new("recorded-mcp-down");
    stage.fixture("mcp_server_down.jsonl");
    // The real CLI carries on without the server; the fake waits to be killed.
    stage.script(&script(&["child", "replay mcp_server_down.jsonl", "hang"]));
    let mut conversation = stage.conversation();

    let started = Instant::now();
    let events = turn(&mut conversation, "hello");

    assert_eq!(
        events,
        [Event::Failed(FailureReason::McpServerUnavailable { url: DEFAULT_MCP_URL.into(), status: "failed".into() })]
    );
    assert!(started.elapsed() < Duration::from_secs(30), "took {:?}", started.elapsed());
    assert_eq!(conversation.session_id(), None);
    stage.await_helper();
    assert!(stage.helper_has_stopped(), "stopping the turn stops what the CLI started");
}

#[test]
fn a_recorded_lost_conversation_is_forgotten_so_the_next_message_starts_afresh() {
    let stage = Stage::new("recorded-lost-session");
    stage.fixture("session_not_found.jsonl");
    stage.script_for(
        1,
        &script(&[
            "stderr No conversation found with session ID: 00000006-0000-4000-8000-000000000000",
            "replay session_not_found.jsonl",
            "exit 1",
        ]),
    );
    stage.script_for(2, &script(&[&init(), &say("Hello again."), success()]));
    let stale = SessionId::parse("00000006-0000-4000-8000-000000000000").unwrap();
    let mut conversation = Conversation::resume(stage.config(), stage.cli(), stale.clone()).unwrap();
    assert_eq!(conversation.session_id(), Some(stale));

    assert_eq!(failure(&turn(&mut conversation, "where were we?")), &FailureReason::SessionNotFound);
    assert!(stage.launch(1).has_argument("--resume=00000006-0000-4000-8000-000000000000"));
    assert_eq!(conversation.session_id(), None);

    assert!(matches!(turn(&mut conversation, "where were we?").last(), Some(Event::Finished(_))));
    assert!(!stage.launch(2).arguments().iter().any(|argument| argument.starts_with("--resume")));
    assert_eq!(conversation.session_id(), Some(session()));
}

// --- How the CLI is started -------------------------------------------------

#[test]
fn the_cli_is_started_confined_to_trill_with_the_message_on_standard_input() {
    let stage = Stage::new("launch");
    stage.script(&script(&[&init(), &say("Done."), success()]));
    let config = AssistantConfig { mcp_url: "http://127.0.0.1:4321/mcp".into(), ..stage.config() };
    let mut conversation = Conversation::new(config, stage.cli()).unwrap();

    conversation.send("make it swing", Some("playhead: bar 5\nselected track: Drums")).unwrap();
    assert!(matches!(rest_of_turn(&mut conversation).last(), Some(Event::Finished(_))));

    let launch = stage.launch(1);
    assert_eq!(
        launch.input(),
        "<trill-context>\nplayhead: bar 5\nselected track: Drums\n</trill-context>\n\nmake it swing"
    );
    assert_eq!(
        launch.working_directory().canonicalize().unwrap(),
        stage.path("work").canonicalize().unwrap(),
        "the CLI runs in the app's folder"
    );

    let arguments = launch.arguments();
    let after = |flag: &str| {
        let at = arguments.iter().position(|argument| argument == flag).unwrap_or_else(|| panic!("no {flag}"));
        arguments[at + 1].as_str()
    };
    // The empty arguments are the point of these two flags, and the easiest
    // thing to lose on the way to another process.
    assert_eq!(after("--tools"), "");
    assert_eq!(after("--setting-sources"), "");
    assert_eq!(after("--allowedTools"), "mcp__trill__*");
    assert_eq!(after("--permission-mode"), "dontAsk");
    assert_eq!(after("--permission-prompts"), "none");
    assert_eq!(after("--output-format"), "stream-json");
    for flag in ["-p", "--verbose", "--include-partial-messages", "--strict-mcp-config", "--disable-slash-commands"] {
        assert!(launch.has_argument(flag), "missing {flag}");
    }
    assert!(launch.has_argument("--max-turns=80"));
    assert!(!arguments.iter().any(|argument| argument.contains("make it swing")), "the message is not an argument");
    assert!(!launch.had_console_window(), "no console window appears when the CLI starts");

    let servers: Value = serde_json::from_str(&fs::read_to_string(after("--mcp-config")).unwrap()).unwrap();
    assert_eq!(servers, json!({ "mcpServers": { "trill": { "type": "http", "url": "http://127.0.0.1:4321/mcp" } } }));
    assert_eq!(fs::read_to_string(after("--append-system-prompt-file")).unwrap(), DEFAULT_SYSTEM_PROMPT);

    let handed_over = [after("--mcp-config").to_owned(), after("--append-system-prompt-file").to_owned()];
    drop(conversation);
    for file in handed_over {
        assert!(!std::path::Path::new(&file).exists(), "{file} is removed with the conversation");
    }
}

#[test]
fn later_messages_continue_the_conversation_and_a_stored_id_resumes_it() {
    let stage = Stage::new("continue");
    stage.script(&script(&[&init(), &say("Done."), success()]));
    let mut conversation = stage.conversation();
    assert_eq!(conversation.session_id(), None);

    turn(&mut conversation, "first");
    assert!(!stage.launch(1).arguments().iter().any(|argument| argument.starts_with("--resume")));
    let stored = conversation.session_id().expect("the id is known after the first turn").as_str().to_owned();
    assert_eq!(stored, SESSION);

    turn(&mut conversation, "second");
    assert!(stage.launch(2).has_argument(&format!("--resume={SESSION}")));
    assert_eq!(stage.launch(2).input(), "second");
    drop(conversation);

    // A later run of the app, with only the stored text to go on.
    let session = SessionId::parse(&stored).expect("what was stored is an id");
    let mut resumed = Conversation::resume(stage.config(), stage.cli(), session).unwrap();
    turn(&mut resumed, "third");
    assert!(stage.launch(3).has_argument(&format!("--resume={SESSION}")));
}

#[test]
fn a_message_that_looks_like_a_cli_command_is_still_sent_as_a_message() {
    let stage = Stage::new("slash");
    stage.script(&script(&[&init(), &say("Done."), success()]));

    turn(&mut stage.conversation(), "/clear the drums");

    assert_eq!(stage.launch(1).input(), "Message from the user:\n/clear the drums");
}

// --- Output that arrives awkwardly -------------------------------------------

#[test]
fn lines_and_characters_split_across_reads_are_put_back_together() {
    let stage = Stage::new("split");
    stage.script(&script(&[
        &init(),
        // One line, written in four pieces with pauses, cutting both
        // multi-byte characters ("\u{e9}" is c3 a9, the piano is f0 9f 8e b9).
        r#"part {"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_del"#,
        "sleep 60",
        r#"part ta","text":"caf"#,
        "hex c3",
        "sleep 60",
        "hex a9 20 f0 9f",
        "sleep 60",
        "hex 8e b9",
        r#"line "}}}"#,
        success(),
    ]));

    let events = turn(&mut stage.conversation(), "hello");

    assert_eq!(said(&events), "caf\u{e9} \u{1f3b9}");
    assert!(matches!(events.last(), Some(Event::Finished(_))));
}

#[test]
fn a_very_long_line_arrives_whole() {
    let stage = Stage::new("long-line");
    stage.script(&script(&[&init(), "text-line 5000000", success()]));

    let events = turn(&mut stage.conversation(), "hello");

    let reply = said(&events);
    assert_eq!(reply.len(), 5_000_000);
    assert!(reply.bytes().all(|byte| byte == b'a'));
    assert!(matches!(events.last(), Some(Event::Finished(_))));
}

#[test]
fn output_the_crate_does_not_know_is_skipped() {
    let stage = Stage::new("unknown");
    stage.script(&script(&[
        "line Warning: something printed before the first event",
        &init(),
        r#"line {"type":"hologram","depth":3}"#,
        r#"line {"type":"system","subtype":"brand_new_subtype","detail":{"a":[1,2,3]}}"#,
        r#"line {"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"hologram_delta","x":1}}}"#,
        "line",
        "line [1,2,3]",
        "line {\"type\":",
        &say("Still here."),
        r#"line {"type":"result","subtype":"success","is_error":false,"num_turns":1,"brand_new_field":{"x":1}}"#,
    ]));

    let events = turn(&mut stage.conversation(), "hello");

    assert_eq!(said(&events), "Still here.");
    assert_eq!(events.len(), 3, "start, text, finish: {events:?}");
    assert!(matches!(events.last(), Some(Event::Finished(_))));
}

#[test]
fn noise_on_the_error_stream_does_not_disturb_a_turn() {
    let stage = Stage::new("stderr-noise");
    stage.script(&script(&[
        "stderr Warning: something chatty",
        &init(),
        "stderr (node:1234) ExperimentalWarning: more noise",
        &say("Done."),
        "stderr and more",
        success(),
        "stderr goodbye",
    ]));

    let events = turn(&mut stage.conversation(), "hello");

    assert_eq!(said(&events), "Done.");
    assert!(matches!(events.last(), Some(Event::Finished(_))));
}

// --- Endings ------------------------------------------------------------------

#[test]
fn a_usage_limit_ends_the_turn_with_the_clis_words_and_the_reset_time() {
    let stage = Stage::new("usage-limit");
    // Constructed, not recorded: the limit could not be provoked for a test.
    stage.script(&script(&[
        &init(),
        r#"line {"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":500,"error_status":429,"error":"rate_limit"}"#,
        r#"line {"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1795165200,"rateLimitType":"five_hour"}}"#,
        r#"line {"type":"assistant","error":"rate_limit","is_api_error_message":true,"message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your session limit · resets 3pm"}]}}"#,
        r#"line {"type":"result","subtype":"success","is_error":true,"num_turns":1,"result":"You've hit your session limit · resets 3pm"}"#,
        "exit 1",
    ]));

    let events = turn(&mut stage.conversation(), "hello");

    assert_eq!(
        events[1],
        Event::Notice(Notice::RateLimited { attempt: 1, max_attempts: 10, delay: Duration::from_millis(500) })
    );
    assert_eq!(
        failure(&events),
        &FailureReason::UsageLimitReached {
            message: "You've hit your session limit \u{b7} resets 3pm".into(),
            resets_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1795165200)),
        }
    );
    assert_eq!(said(&events), "");
}

#[test]
fn running_out_of_turns_is_its_own_failure() {
    let stage = Stage::new("turn-limit");
    stage.script(&script(&[
        &init(),
        &say("Halfway there."),
        r#"line {"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":8}"#,
    ]));
    let config = AssistantConfig { max_turns: Some(7), ..stage.config() };
    let mut conversation = Conversation::new(config, stage.cli()).unwrap();

    let events = turn(&mut conversation, "write a symphony");

    assert!(stage.launch(1).has_argument("--max-turns=7"));
    assert_eq!(failure(&events), &FailureReason::TurnLimitReached { limit: 7 });
    assert_eq!(said(&events), "Halfway there.");
}

#[test]
fn a_cli_that_dies_mid_reply_fails_the_turn_with_its_exit_code_and_last_words() {
    let stage = Stage::new("killed");
    stage.script(&script(&[&init(), &say("Working on it"), "stderr Fatal: out of memory", "exit 137"]));
    let mut conversation = stage.conversation();

    let events = turn(&mut conversation, "hello");

    assert_eq!(said(&events), "Working on it");
    assert_eq!(
        failure(&events),
        &FailureReason::CliExited { exit_code: Some(137), stderr_tail: "Fatal: out of memory".into() }
    );
    assert_eq!(conversation.session_id(), Some(session()), "what was said so far can still be resumed");
}

#[test]
fn a_cli_that_exits_before_it_starts_fails_the_turn_with_what_it_printed() {
    let stage = Stage::new("early-exit");
    stage.script(&script(&["stderr error: unknown option '--frobnicate'", "exit 1"]));
    let mut conversation = stage.conversation();

    let events = turn(&mut conversation, "hello");

    assert_eq!(
        events,
        [Event::Failed(FailureReason::CliExited {
            exit_code: Some(1),
            stderr_tail: "error: unknown option '--frobnicate'".into(),
        })]
    );
    assert_eq!(conversation.session_id(), None);
}

#[test]
fn a_clean_exit_without_a_result_is_a_protocol_failure() {
    let stage = Stage::new("no-result");
    stage.script(&script(&[&init(), &say("Hm.")]));

    let events = turn(&mut stage.conversation(), "hello");

    assert!(matches!(failure(&events), FailureReason::Protocol { .. }), "{events:?}");
}

#[test]
fn a_cli_that_offers_tools_of_its_own_is_stopped() {
    let stage = Stage::new("unexpected-tools");
    stage.script(&script(&[
        r#"line {"type":"system","subtype":"init","session_id":"5e55104d-0000-4000-8000-000000000001","model":"m","tools":["Bash","mcp__trill__render","WebFetch"],"mcp_servers":[{"name":"trill","status":"connected"}]}"#,
        "hang",
    ]));

    let events = turn(&mut stage.conversation(), "hello");

    assert_eq!(
        events,
        [Event::Failed(FailureReason::UnexpectedTools { tools: vec!["Bash".into(), "WebFetch".into()] })]
    );
}

#[test]
fn a_cli_that_is_not_there_fails_the_turn() {
    let stage = Stage::new("missing");
    let gone = CliInstallation { path: stage.path("uninstalled.exe"), version: CliVersion::new(2, 1, 325) };
    let mut conversation = Conversation::new(stage.config(), gone.clone()).unwrap();

    assert_eq!(turn(&mut conversation, "hello"), [Event::Failed(FailureReason::CliMissing { path: gone.path })]);
}

#[test]
fn a_helper_left_running_by_the_cli_does_not_keep_the_turn_open() {
    let stage = Stage::new("lingering-helper");
    // The helper inherits the output pipe, so the output never ends by itself.
    stage.script(&script(&[&init(), "child", &say("Done."), success()]));

    let started = Instant::now();
    let events = turn(&mut stage.conversation(), "hello");

    assert!(matches!(events.last(), Some(Event::Finished(_))), "{events:?}");
    assert!(started.elapsed() < Duration::from_secs(30), "took {:?}", started.elapsed());
    stage.await_helper();
    assert!(stage.helper_has_stopped(), "what the CLI left behind is cleaned up");
}

// --- Cancelling -----------------------------------------------------------------

#[test]
fn cancel_stops_a_slow_turn_at_once_leaves_nothing_running_and_the_conversation_resumes() {
    let stage = Stage::new("cancel");
    stage.script_for(
        1,
        &script(&[&init(), &say("Starting."), "child", "sleep 100", &say("Still going."), "sleep 100", "hang"]),
    );
    stage.script_for(2, &script(&[&init(), &say("Picking up."), success()]));
    let mut conversation = stage.conversation();

    conversation.send("write a long piece", None).unwrap();
    let mut before_cancel = Vec::new();
    while said(&before_cancel) != "Starting.\n\nStill going." {
        assert!(conversation.is_busy(), "the turn ended early: {before_cancel:?}");
        before_cancel.extend(conversation.wait(Duration::from_secs(30)));
    }
    stage.await_helper();
    assert!(stage.helper_is_alive());

    let asked = Instant::now();
    conversation.cancel();
    let after_cancel = rest_of_turn(&mut conversation);
    let took = asked.elapsed();

    assert_eq!(after_cancel, [Event::Cancelled]);
    assert!(took < Duration::from_secs(2), "cancelling took {took:?}");
    assert!(stage.helper_has_stopped(), "the CLI's helper went with it");

    // The conversation goes on from where it was cut off.
    assert_eq!(conversation.session_id(), Some(session()));
    let events = turn(&mut conversation, "carry on");
    assert!(stage.launch(2).has_argument(&format!("--resume={SESSION}")));
    assert_eq!(said(&events), "Picking up.");
}

#[test]
fn cancel_works_before_the_cli_has_said_anything() {
    let stage = Stage::new("cancel-early");
    stage.script("hang");
    let mut conversation = stage.conversation();

    conversation.send("hello", None).unwrap();
    conversation.cancel();
    let asked = Instant::now();

    assert_eq!(rest_of_turn(&mut conversation), [Event::Cancelled]);
    assert!(asked.elapsed() < Duration::from_secs(30), "took {:?}", asked.elapsed());
    assert_eq!(conversation.session_id(), None);
}

#[test]
fn a_second_message_is_refused_while_a_turn_is_running() {
    let stage = Stage::new("busy");
    stage.script(&script(&[&init(), "hang"]));
    let mut conversation = stage.conversation();

    conversation.send("first", None).unwrap();
    assert!(conversation.is_busy());
    assert_eq!(conversation.send("second", None), Err(SendError::TurnInProgress));
    assert!(matches!(conversation.wait(Duration::from_secs(30)), Some(Event::Started(_))));
    assert_eq!(conversation.send("second", None), Err(SendError::TurnInProgress));

    conversation.cancel();
    assert_eq!(rest_of_turn(&mut conversation).last(), Some(&Event::Cancelled));
    assert_eq!(stage.launches(), 1);
}

#[test]
fn dropping_the_conversation_stops_the_turn_in_progress() {
    let stage = Stage::new("drop");
    stage.script(&script(&[&init(), "child", "hang"]));
    let mut conversation = stage.conversation();

    conversation.send("hello", None).unwrap();
    stage.await_helper();
    assert!(stage.helper_is_alive());
    drop(conversation);

    thread::sleep(Duration::from_millis(200));
    assert!(stage.helper_has_stopped());
}

/// Windows only: a job object is what makes this hold. Elsewhere the CLI is
/// left to notice that its output pipe has gone.
#[cfg(windows)]
#[test]
fn the_cli_dies_with_the_program_that_started_it() {
    let stage = Stage::new("host-killed");
    stage.script(&script(&[&init(), "child", "hang"]));

    // This test program again, running only the test below, as the "Trill"
    // that gets killed.
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "a_host_in_the_middle_of_a_turn", "--ignored", "--nocapture"])
        .env(HOST_STAGE_VARIABLE, "host-killed")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    stage.await_helper();
    assert!(stage.helper_is_alive());

    // No destructor runs in a killed process: only the system can clean up.
    host.kill().unwrap();
    host.wait().unwrap();

    thread::sleep(Duration::from_millis(200));
    assert!(stage.helper_has_stopped(), "the CLI and its helper outlived the program that started them");
}

const HOST_STAGE_VARIABLE: &str = "TRILL_ASSISTANT_TEST_HOST_STAGE";

#[test]
#[ignore = "run by the_cli_dies_with_the_program_that_started_it, in a process of its own"]
fn a_host_in_the_middle_of_a_turn() {
    let Ok(name) = std::env::var(HOST_STAGE_VARIABLE) else { return };
    let stage = Stage::reopen(&name);
    let mut conversation = stage.conversation();
    conversation.send("hello", None).unwrap();
    loop {
        // Until killed.
        let _ = conversation.wait(Duration::from_secs(3600));
    }
}

#[test]
fn a_frame_loop_can_poll_without_ever_waiting() {
    let stage = Stage::new("poll");
    stage.script(&script(&[&init(), "sleep 150", &say("Done."), success()]));
    let mut conversation = stage.conversation();
    assert_eq!(conversation.poll(), None, "nothing to report before a message is sent");

    conversation.send("hello", None).unwrap();
    let mut events = Vec::new();
    let mut empty_polls = 0;
    while conversation.is_busy() {
        match conversation.poll() {
            Some(event) => events.push(event),
            None => {
                empty_polls += 1;
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    assert!(empty_polls > 0, "polling returned while the CLI was still working");
    assert_eq!(said(&events), "Done.");
    assert!(matches!(events.last(), Some(Event::Finished(_))));
}

// --- Detection --------------------------------------------------------------------

#[test]
fn detection_reports_a_cli_that_is_ready() {
    let stage = Stage::new("detect-ready");
    stage.write("version.txt", "2.1.325 (Claude Code)\n");
    stage.write(
        "auth.json",
        r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "someone@example.com", "subscriptionType": "max"}"#,
    );

    let status = detect(&stage.config()).expect("ready");

    assert_eq!(status.installation, CliInstallation { path: stage.program(), version: CliVersion::new(2, 1, 325) });
    assert_eq!(status.login, LoginMethod::Subscription { plan: Some("max".into()) });
    assert!(!format!("{status:?}").contains("someone@example.com"), "account details are not kept");
}

#[test]
fn detection_says_why_the_assistant_cannot_be_offered() {
    let stage = Stage::new("detect-unavailable");

    let nowhere = AssistantConfig { cli_path: Some(stage.path("nowhere.exe")), ..stage.config() };
    assert_eq!(detect(&nowhere), Err(Unavailable::NotInstalled));

    stage.write("version.txt", "2.0.5 (Claude Code)");
    let Err(Unavailable::TooOld { installation, minimum }) = detect(&stage.config()) else {
        panic!("expected the CLI to be too old");
    };
    assert_eq!((installation.version, minimum), (CliVersion::new(2, 0, 5), trill_assistant::MINIMUM_CLI_VERSION));

    stage.write("version.txt", "Segmentation fault");
    assert!(matches!(detect(&stage.config()), Err(Unavailable::NotWorking { .. })));

    stage.write("version.txt", "2.1.325 (Claude Code)");
    stage.write("auth.json", r#"{"loggedIn": false, "authMethod": "none"}"#);
    stage.write("auth.exit", "1");
    let Err(reason) = detect(&stage.config()) else { panic!("expected nobody to be signed in") };
    assert!(matches!(&reason, Unavailable::NotLoggedIn { installation } if installation.path == stage.program()));
    assert!(reason.to_string().contains("run `claude`"), "{reason}");

    stage.write("auth.json", "Not a report at all");
    assert!(matches!(detect(&stage.config()), Err(Unavailable::NotWorking { .. })));
}
