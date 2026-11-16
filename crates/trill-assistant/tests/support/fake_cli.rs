//! A stand-in for the real CLI, for the tests in `tests/pipeline.rs`.
//!
//! A test copies this program into a directory of its own and puts files
//! beside it that say how to behave. Nothing is passed through arguments or
//! the environment, because those are exactly what the code under test
//! controls and the tests need to see untouched.
//!
//! * `--version` prints `version.txt`, or a current version if there is none.
//! * `auth status` prints `auth.json` and exits with the code in `auth.exit`
//!   (a signed-in report and 0 if the files are missing), and records how it
//!   was started in `launch-auth.json`.
//! * Anything else is a turn. The program reads its standard input to the
//!   end, as the real CLI does, records how it was started in
//!   `launch-<n>.json` (arguments, input, working directory, the environment
//!   variables the tests care about, and whether it has a console window),
//!   and then follows
//!   `turn-<n>.script`, or `turn.script` if that launch has no script of its
//!   own.
//!
//! A script is a list of directives, one per line:
//!
//! ```text
//! # a comment
//! line <text>        write text and a newline to stdout
//! part <text>        write text with no newline
//! hex <bytes>        write raw bytes, given in hex, with no newline
//! replay <file>      write a recorded transcript from beside the program
//! text-line <bytes>  write one assistant message whose text is that long
//! stderr <text>      write text and a newline to stderr
//! sleep <ms>         wait
//! child              start a helper that outlives this process (see below)
//!                    and wait until it is running
//! hang               wait until killed
//! exit <code>        exit with a code; the default at the end is 0
//! ```
//!
//! Output is flushed after every directive, so that a `sleep` between two
//! writes makes them two reads on the other side.
//!
//! `child` starts this program again as a grandchild of the test that
//! inherits the output pipes and rewrites `heartbeat` every few milliseconds
//! until killed, so a test can tell whether stopping a turn stopped
//! everything the CLI had started.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

const HEARTBEAT_ARGUMENT: &str = "--fake-heartbeat";
const RECORDED_PREFIXES: &[&str] = &["ANTHROPIC", "CLAUDE", "ENABLE_", "DISABLE_", "MCP_"];

fn main() -> ExitCode {
    let home = std::env::current_exe()
        .ok()
        .and_then(|program| program.parent().map(Path::to_owned))
        .expect("the program has a directory");
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    if arguments.first().is_some_and(|first| first == HEARTBEAT_ARGUMENT) {
        heartbeat(&home);
    }
    if arguments.iter().any(|argument| argument == "--version") {
        let version = fs::read_to_string(home.join("version.txt")).unwrap_or_else(|_| "2.1.325 (Claude Code)".into());
        println!("{}", version.trim());
        return ExitCode::SUCCESS;
    }
    if arguments.starts_with(&["auth".to_owned(), "status".to_owned()]) {
        record(&home, "launch-auth.json", &arguments, "");
        let report = fs::read_to_string(home.join("auth.json"))
            .unwrap_or_else(|_| r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro"}"#.into());
        println!("{}", report.trim());
        return exit_code(fs::read_to_string(home.join("auth.exit")).ok().and_then(|code| code.trim().parse().ok()));
    }
    turn(&home, &arguments)
}

fn turn(home: &Path, arguments: &[String]) -> ExitCode {
    let mut input = String::new();
    let _ = io::stdin().read_to_string(&mut input);

    let launch = (1..).find(|number| !home.join(format!("launch-{number}.json")).exists()).expect("a free number");
    record(home, &format!("launch-{launch}.json"), arguments, &input);

    let script = fs::read_to_string(home.join(format!("turn-{launch}.script")))
        .or_else(|_| fs::read_to_string(home.join("turn.script")))
        .expect("a script for this turn");
    for directive in script.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let (name, rest) = directive.split_once(' ').unwrap_or((directive, ""));
        if let Some(code) = follow(home, name, rest) {
            return code;
        }
    }
    ExitCode::SUCCESS
}

/// Writes down how this process was started, for the test to read.
fn record(home: &Path, file: &str, arguments: &[String], input: &str) {
    let environment: serde_json::Map<String, Value> = std::env::vars()
        .filter(|(name, _)| RECORDED_PREFIXES.iter().any(|prefix| name.starts_with(prefix)))
        .map(|(name, value)| (name, Value::String(value)))
        .collect();
    let record = json!({
        "arguments": arguments,
        "input": input,
        "working_directory": std::env::current_dir().ok(),
        "environment": environment,
        "console_window": has_console_window(),
    });
    fs::write(home.join(file), record.to_string()).expect("the launch record");
}

/// Whether this process can put a console window on the screen: it has one,
/// or it shares the console of the program that started it.
///
/// A console program started with neither gets a console of its own with no
/// window, which is what a GUI program must ask for; otherwise Windows opens
/// a visible console for the child.
#[cfg(windows)]
fn has_console_window() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleWindow() -> *mut std::ffi::c_void;
        fn GetConsoleProcessList(process_list: *mut u32, process_count: u32) -> u32;
    }
    let mut sharing = [0u32; 16];
    // SAFETY: the first function takes no arguments and its result is only
    // compared with null. The second is given a live buffer and its true
    // length, and writes at most that many entries.
    let (window, attached) =
        unsafe { (GetConsoleWindow(), GetConsoleProcessList(sharing.as_mut_ptr(), sharing.len() as u32)) };
    !window.is_null() || attached > 1
}

#[cfg(not(windows))]
fn has_console_window() -> bool {
    false
}

/// Carries out one directive. Returns the exit code if it ends the program.
fn follow(home: &Path, name: &str, rest: &str) -> Option<ExitCode> {
    let mut stdout = io::stdout().lock();
    match name {
        "line" => writeln!(stdout, "{rest}").expect("stdout"),
        "part" => write!(stdout, "{rest}").expect("stdout"),
        "hex" => stdout.write_all(&bytes_from_hex(rest)).expect("stdout"),
        "replay" => {
            let transcript = fs::read(home.join(rest)).expect("the transcript to replay");
            stdout.write_all(&transcript).expect("stdout");
        }
        "text-line" => {
            let length: usize = rest.parse().expect("a length");
            let message = json!({
                "type": "assistant",
                "message": { "id": "msg_long", "content": [{ "type": "text", "text": "a".repeat(length) }] },
            });
            writeln!(stdout, "{message}").expect("stdout");
        }
        "stderr" => eprintln!("{rest}"),
        "sleep" => thread::sleep(Duration::from_millis(rest.parse().expect("milliseconds"))),
        "child" => {
            let program = std::env::current_exe().expect("this program");
            #[expect(clippy::zombie_processes, reason = "the helper is meant to outlive this process")]
            let _helper = Command::new(program).arg(HEARTBEAT_ARGUMENT).spawn().expect("the helper");
            // Not until the helper is seen to run: a test that stops this
            // process straight away must still find a helper to check on.
            while !home.join("heartbeat").exists() {
                thread::sleep(Duration::from_millis(5));
            }
        }
        "hang" => loop {
            thread::sleep(Duration::from_secs(3600));
        },
        "exit" => return Some(exit_code(rest.parse().ok())),
        other => panic!("unknown directive {other:?}"),
    }
    stdout.flush().expect("stdout");
    None
}

/// Proves this process is alive by rewriting a file, for as long as it is.
fn heartbeat(home: &Path) -> ! {
    let file: PathBuf = home.join("heartbeat");
    for beat in 0u64.. {
        let _ = fs::write(&file, beat.to_string());
        thread::sleep(Duration::from_millis(20));
    }
    unreachable!("the helper runs until it is killed")
}

fn bytes_from_hex(hex: &str) -> Vec<u8> {
    let digits: Vec<u8> = hex.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).expect("hex digits"), 16).expect("a byte"))
        .collect()
}

fn exit_code(code: Option<u8>) -> ExitCode {
    ExitCode::from(code.unwrap_or(0))
}
