//! A conversation with Trill's assistant in the terminal, and the reference
//! for the chat panel: it does everything the panel has to do.
//!
//! Start Trill (or `trill-mcp --http`) first, then:
//!
//! ```text
//! cargo run -p trill-assistant --example chat -- [--mcp <url>] [--model <name>] [--resume <id>]
//!     [--context <text>] [--dir <folder>] [--cli <path>] [--keep-environment]
//!     [--effort <level>] [--no-web] [--max-turns <n>] [--turns-per-run <n>]
//! ```
//!
//! Type a message and press Enter. While the assistant is working, an empty
//! line cancels the turn. `:quit` leaves.
//!
//! This uses your own Claude Code login and counts against your own usage.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use trill_assistant::{
    AssistantConfig, Conversation, Effort, Event, LoginMethod, Notice, PlanChange, SessionId, StepStatus, TurnSummary,
    Unavailable, WebLookup, detect,
};

/// How often a frame loop would come round.
const FRAME: Duration = Duration::from_millis(16);

struct Options {
    config: AssistantConfig,
    resume: Option<SessionId>,
    context: Option<String>,
}

fn main() {
    let options = match options() {
        Ok(options) => options,
        Err(problem) => {
            eprintln!(
                "{problem}\n\nusage: chat [--mcp <url>] [--model <name>] [--resume <id>] [--context <text>] [--dir <folder>] [--cli <path>] [--keep-environment] [--effort <level>] [--no-web] [--max-turns <n>] [--turns-per-run <n>]"
            );
            std::process::exit(2);
        }
    };

    // 1. Is the CLI there, recent enough and signed in? This starts it twice,
    //    so a UI does it on a background thread and shows a spinner.
    println!("Looking for Claude Code...");
    let cli = match detect(&options.config) {
        Ok(cli) => cli,
        Err(reason) => {
            // Each reason is one line that tells the user what to do. Signing
            // in is theirs to do, in the CLI: never offer it here.
            println!("{reason}");
            if let Unavailable::NotLoggedIn { installation } = &reason {
                println!("(found {} at {})", installation.version, installation.path.display());
            }
            std::process::exit(1);
        }
    };
    let login = match &cli.login {
        LoginMethod::Subscription { plan: Some(plan) } => format!("signed in with a Claude account ({plan})"),
        LoginMethod::Subscription { plan: None } => "signed in with a Claude account".to_owned(),
        LoginMethod::ApiKey => "using an API key: messages are billed to that API account".to_owned(),
        LoginMethod::Other(method) => format!("signed in ({method})"),
    };
    println!("Found {} at {}, {login}.", cli.installation.version, cli.installation.path.display());
    println!("Talking to Trill at {}. An empty line cancels a turn; :quit leaves.\n", options.config.mcp_url);

    // 2. One conversation, new or picked up from a stored id.
    let conversation = match options.resume {
        Some(session) => Conversation::resume(options.config, cli.installation, session),
        None => Conversation::new(options.config, cli.installation),
    };
    let mut conversation = match conversation {
        Ok(conversation) => conversation,
        Err(problem) => {
            eprintln!("{problem}");
            std::process::exit(2);
        }
    };

    // The terminal blocks on input, which a frame loop must never do, so the
    // lines are read elsewhere and collected here.
    let lines = lines_from_terminal();
    loop {
        print!("> ");
        let _ = io::stdout().flush();
        let Ok(message) = lines.recv() else { break };
        match message.trim() {
            ":quit" => break,
            "" => continue,
            _ => {}
        }

        // 3. Send. The context is whatever the app knows that the user did
        //    not type; it is sent again with every message it applies to.
        if let Err(problem) = conversation.send(&message, options.context.as_deref()) {
            println!("{problem}");
            continue;
        }

        // 4. Each frame: take what has arrived, and act on the cancel button.
        while conversation.is_busy() {
            while let Some(event) = conversation.poll() {
                show(&event);
            }
            if lines.try_recv().is_ok() {
                conversation.cancel(); // `Event::Cancelled` follows.
            }
            thread::sleep(FRAME);
        }
    }

    // 5. What to store in order to carry on another day.
    if let Some(session) = conversation.session_id() {
        println!("\nTo continue this conversation later: --resume {session}");
    }
}

fn show(event: &Event) {
    match event {
        Event::Started(start) => {
            if let Some(source) = &start.api_key_source {
                println!("[billed to an API key from {source}, not to your Claude login]");
            }
        }
        Event::Text(piece) => {
            print!("{piece}");
            let _ = io::stdout().flush();
        }
        Event::ToolCallStarted { tool, input, .. } => println!("\n  -> {tool} {}", shortened(&input.to_string())),
        Event::ToolCallFinished { result: Ok(answer), .. } => println!("  <- {}", shortened(answer)),
        Event::ToolCallFinished { result: Err(error), .. } => println!("  <- failed: {}", shortened(error)),
        // A lookup on the web, with the CLI's own tools: say what is asked
        // of whom, every time.
        Event::WebStarted { lookup: WebLookup::Search { query }, .. } => println!("\n  -> web search: {query}"),
        Event::WebStarted { lookup: WebLookup::Page { url, .. }, .. } => println!("\n  -> web page: {url}"),
        Event::WebFinished { result: Ok(answer), .. } => println!("  <- {}", shortened(answer)),
        Event::WebFinished { result: Err(error), .. } => println!("  <- failed: {}", shortened(error)),
        // The plan is the app's to keep; a panel shows it as a checklist.
        Event::Plan(PlanChange::Added { title, .. }) => println!("\n  [ ] {title}"),
        Event::Plan(PlanChange::Changed { id, status: Some(StepStatus::InProgress), .. }) => {
            println!("\n  [>] step {id}");
        }
        Event::Plan(PlanChange::Changed { id, status: Some(StepStatus::Done), .. }) => println!("\n  [x] step {id}"),
        Event::Plan(_) => {}
        // A long request is worked on in several runs of the CLI. What it
        // has cost is known when a run ends, and is an estimate.
        Event::Progress(progress) if progress.costed_steps == progress.steps => {
            if let Some(cost) = progress.estimated_cost_usd {
                println!("\n[{} steps so far, about ${cost:.2} at API prices; carrying on]", progress.steps);
            }
        }
        Event::Progress(_) => {}
        Event::Notice(notice) => {
            println!("\n[{notice}]");
            if let Notice::UsageLimitNear { resets_at: Some(_) } = notice {
                println!("[the CLI also said when the limit renews; a UI would show the time]");
            }
        }
        Event::Finished(summary) => println!("\n{}\n", footer(summary)),
        Event::Cancelled => println!("\n[stopped]\n"),
        // The text of a failure is written for the user: show it as it is.
        Event::Failed(reason) => println!("\n[{reason}]\n"),
    }
}

fn footer(summary: &TurnSummary) -> String {
    let cost = match summary.estimated_cost_usd {
        // The figure is the CLI's estimate at API prices. Say so wherever it
        // is shown: a subscriber is not charged per message.
        Some(cost) => format!(", about ${cost:.4} at API prices (an estimate, not a charge)"),
        None => String::new(),
    };
    format!(
        "({} model turn{}, {:.1} s, {} tokens out{cost})",
        summary.turns,
        if summary.turns == 1 { "" } else { "s" },
        summary.duration.as_secs_f32(),
        summary.usage.output,
    )
}

/// One line of at most a hundred characters.
fn shortened(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(100) {
        Some((end, _)) => format!("{}...", &line[..end]),
        None => line,
    }
}

fn lines_from_terminal() -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

fn options() -> Result<Options, String> {
    // A folder of the program's own: the CLI files conversations by it, so
    // it has to be the same folder next time for `--resume` to find one.
    let mut config = AssistantConfig::new(std::env::temp_dir().join("trill-assistant-chat-example"));
    let mut resume = None;
    let mut context = None;

    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        let mut value = || arguments.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--cli" => config.cli_path = Some(PathBuf::from(value()?)),
            "--mcp" => config.mcp_url = value()?,
            "--model" => config.model = Some(value()?),
            "--context" => context = Some(value()?),
            "--dir" => {
                let directory = PathBuf::from(value()?);
                config.temp_dir = directory.join("tmp");
                config.working_dir = directory;
            }
            "--resume" => {
                let id = value()?;
                resume = Some(SessionId::parse(&id).ok_or_else(|| format!("{id} is not a conversation id"))?);
            }
            "--keep-environment" => config.keep_redirecting_environment = true,
            "--no-web" => config.web = false,
            "--effort" => {
                let name = value()?;
                config.effort = Some(Effort::named(&name).ok_or_else(|| format!("{name} is not an effort level"))?);
            }
            "--max-turns" => config.max_turns = Some(value()?.parse().map_err(|_| "--max-turns needs a number")?),
            "--turns-per-run" => {
                config.turns_per_run = value()?.parse().map_err(|_| "--turns-per-run needs a number")?;
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Options { config, resume, context })
}
