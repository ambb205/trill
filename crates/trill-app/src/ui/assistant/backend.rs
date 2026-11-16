//! What stands behind the panel: the seam between it and the program that
//! answers.
//!
//! The panel talks to a [`Backend`] and the [`Chat`]s it opens, and never to
//! `trill-assistant` directly. [`Installed`] is the real thing: the user's
//! own Claude Code, found and run by `trill-assistant`, working through
//! the door Trill's MCP server keeps for it (`/assistant`): every tool is
//! there, but what is written goes into the project's folder or
//! `Documents/Trill`, and unsaved work cannot be thrown away. [`Script`] is
//! a stand-in that says what it is told to, for the tests and for the
//! pictures of the panel, neither of which may start anybody's CLI.
//!
//! Nothing here blocks: finding the CLI takes a second or more and starting
//! a server takes a moment, so both happen on a thread of their own and the
//! answer is collected when it is there.

use eframe::egui;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use trill_assistant::{
    AssistantConfig, CliStatus, Conversation, DEFAULT_MAX_TURNS, Effort, Event, SendError, SessionId, Unavailable,
};
use trill_runtime::Runtime;

/// How a conversation is to be answered: what the panel lets the user
/// choose. A conversation is opened with one of these and keeps to it; to
/// change any of it, the conversation is opened again by its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    /// The model the CLI is asked for; `None` leaves the choice to the CLI.
    pub model: Option<String>,
    /// How hard the model is asked to think; `None` leaves that to the CLI.
    pub effort: Option<Effort>,
    /// Whether the assistant may search the web and read pages.
    pub web: bool,
    /// The most steps one request may take.
    pub step_limit: u32,
}

impl Default for Setup {
    fn default() -> Self {
        Self { model: None, effort: None, web: true, step_limit: DEFAULT_MAX_TURNS }
    }
}

/// Everything a conversation needs to start.
#[derive(Clone, Debug, PartialEq)]
pub struct Ready {
    /// The CLI and how it is signed in.
    pub cli: CliStatus,
    /// The address of the door Trill's MCP server keeps for the assistant.
    pub mcp_url: String,
}

/// Why the assistant cannot be used right now.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// The CLI is missing, too old, not working or not signed in. Shown in
    /// the back end's own words.
    Cli(Unavailable),
    /// Trill's MCP server is not running, and why.
    Mcp(String),
    /// This run keeps nothing on disk, so there is nowhere for the CLI to
    /// work and nothing to continue later.
    NoFolder,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Cli(reason) => reason.fmt(f),
            Problem::Mcp(reason) => f.write_str(reason),
            Problem::NoFolder => f.write_str(
                "The assistant is not available in this run: Trill was started without a settings folder, and the \
                 assistant needs one to work in.",
            ),
        }
    }
}

/// One conversation with the assistant. The methods are those of
/// `trill_assistant::Conversation`, and mean the same.
pub trait Chat {
    /// Sends a message and starts the assistant's turn. Returns at once.
    fn send(&mut self, message: &str, context: Option<&str>) -> Result<(), SendError>;
    /// The next event of the turn, if one is ready. Never blocks.
    fn poll(&mut self) -> Option<Event>;
    /// Stops the turn. `Event::Cancelled` follows.
    fn cancel(&mut self);
    /// What to store in order to continue the conversation another day.
    fn session_id(&self) -> Option<SessionId>;
}

/// Finds out whether the assistant can be used, and opens conversations.
pub trait Backend {
    /// Starts finding out whether the assistant can be used: is Trill's MCP
    /// server up, is the CLI installed, recent enough and signed in.
    /// Returns at once; `ctx` is woken when the answer is there.
    fn check(&mut self, ctx: &egui::Context);
    /// The answer to the last [`Backend::check`], once.
    fn checked(&mut self) -> Option<Result<Ready, Problem>>;
    /// Opens a conversation: a new one, or the one `resume` names, to be
    /// answered as `setup` says. Nothing runs until its first message.
    ///
    /// A conversation is answered by the model it was opened with, with the
    /// tools it was opened with. To have another model carry it on, or to
    /// give or take the web, it is opened again with its id: the CLI then
    /// hands over everything said so far.
    fn open(&mut self, ready: &Ready, resume: Option<SessionId>, setup: &Setup) -> Result<Box<dyn Chat>, String>;
}

// ---- the user's own CLI ---------------------------------------------------

/// Where Trill's MCP server is, as the window was started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpServer {
    /// The window's own server, by the address of its main door: the one
    /// the top bar shows and any MCP client uses.
    Running(String),
    /// It was asked for and could not be started (most often because
    /// another Trill window has the port).
    Failed,
    /// The user turned it off (`--no-mcp`).
    Off,
}

/// The real back end: the user's installed, signed-in Claude Code.
pub struct Installed {
    runtime: Arc<Runtime>,
    /// The folder the settings are in. Everything of the assistant's goes
    /// in `assistant` under it.
    folder: PathBuf,
    app_server: McpServer,
    /// A server started for the assistant because the window had none. It
    /// stops when this is dropped.
    own_server: Option<trill_mcp::HttpServer>,
    answer: Option<Receiver<Checked>>,
}

/// What the checking thread sends back.
struct Checked {
    result: Result<Ready, Problem>,
    /// A server it had to start.
    started: Option<trill_mcp::HttpServer>,
}

impl Installed {
    pub fn new(runtime: Arc<Runtime>, folder: PathBuf, app_server: McpServer) -> Self {
        Self { runtime, folder, app_server, own_server: None, answer: None }
    }
}

/// How the CLI is run. `working_dir` is a folder of Trill's own that never
/// changes (the CLI files conversations by it), never the project's. No
/// tool of Trill's is withheld: the door the assistant comes in by sees to
/// what the tools that take a path may do. Of the CLI's own tools it has
/// the ones for keeping a plan and, if the user leaves the switch on, the
/// ones for the web; never one that reaches a file or runs a program.
fn config(settings_folder: &Path, mcp_url: &str, setup: &Setup) -> AssistantConfig {
    let folder = settings_folder.join("assistant");
    AssistantConfig {
        mcp_url: mcp_url.to_owned(),
        model: setup.model.clone(),
        effort: setup.effort,
        web: setup.web,
        max_turns: Some(setup.step_limit),
        temp_dir: folder.join("tmp"),
        ..AssistantConfig::new(folder.join("work"))
    }
}

/// The assistant's door of the server whose main door is at `url`: the same
/// address, another path. `None` for an address that is not a main door's.
fn assistant_door(url: &str) -> Option<String> {
    Some(format!("{}{}", url.strip_suffix(trill_mcp::HTTP_PATH)?, trill_mcp::ASSISTANT_PATH))
}

/// Whether something is listening where `url` points.
fn listening(url: &str) -> bool {
    let Some(address) = url.split("://").nth(1).and_then(|rest| rest.split('/').next()) else { return false };
    let Ok(address) = address.parse::<SocketAddr>() else { return false };
    TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok()
}

impl Backend for Installed {
    fn check(&mut self, ctx: &egui::Context) {
        let (sender, receiver) = channel();
        self.answer = Some(receiver);

        // The server to use, if one is known: the window's, or the one
        // started here last time.
        let known = match &self.app_server {
            McpServer::Running(url) => assistant_door(url),
            McpServer::Failed | McpServer::Off => None,
        };
        let known = known.or_else(|| self.own_server.as_ref().map(trill_mcp::HttpServer::assistant_url));
        let may_start = self.app_server != McpServer::Off;
        // The thread holds the runtime only to hand it to a server it has
        // to start, so the runtime never ends on it.
        let runtime = may_start.then(|| self.runtime.clone());
        // Looking for the CLI asks nothing of any model.
        let config = config(&self.folder, "http://127.0.0.1:0/assistant", &Setup::default());
        let ctx = ctx.clone();

        let spawned = std::thread::Builder::new().name("assistant-check".into()).spawn(move || {
            let mut started = None;
            let url = match known.filter(|url| listening(url)) {
                Some(url) => Ok(url),
                None => match runtime {
                    None => Err(Problem::Mcp(
                        "Trill was started with --no-mcp, so the MCP server the assistant works through is off. \
                         Start Trill without --no-mcp to use the assistant."
                            .to_owned(),
                    )),
                    // Any free port: the address is handed to the CLI with
                    // every message, so it need not be the usual one.
                    Some(runtime) => match trill_mcp::serve_http(runtime, SocketAddr::from(([127, 0, 0, 1], 0))) {
                        Ok(server) => {
                            let url = server.assistant_url();
                            log::info!("MCP server for the assistant listening on {url}");
                            started = Some(server);
                            Ok(url)
                        }
                        Err(error) => Err(Problem::Mcp(format!(
                            "Trill could not start the MCP server the assistant works through: {error:#}."
                        ))),
                    },
                },
            };
            let result = url.and_then(|mcp_url| {
                // Slow: it starts the CLI twice.
                let config = AssistantConfig { mcp_url: mcp_url.clone(), ..config };
                trill_assistant::detect(&config).map(|cli| Ready { cli, mcp_url }).map_err(Problem::Cli)
            });
            // The receiver is gone if the window closed meanwhile; a server
            // started here then stops as it is dropped.
            let _ = sender.send(Checked { result, started });
            ctx.request_repaint();
        });
        if let Err(error) = spawned {
            log::warn!("the assistant could not be checked: {error}");
            self.answer = None;
        }
    }

    fn checked(&mut self) -> Option<Result<Ready, Problem>> {
        let answer = self.answer.as_ref()?.try_recv().ok()?;
        self.answer = None;
        if let Some(server) = answer.started {
            self.own_server = Some(server);
        }
        Some(answer.result)
    }

    fn open(&mut self, ready: &Ready, resume: Option<SessionId>, setup: &Setup) -> Result<Box<dyn Chat>, String> {
        let config = config(&self.folder, &ready.mcp_url, setup);
        let installation = ready.cli.installation.clone();
        let conversation = match resume {
            Some(session) => Conversation::resume(config, installation, session),
            None => Conversation::new(config, installation),
        };
        conversation.map(|conversation| Box::new(conversation) as Box<dyn Chat>).map_err(|error| error.to_string())
    }
}

impl Chat for Conversation {
    fn send(&mut self, message: &str, context: Option<&str>) -> Result<(), SendError> {
        Conversation::send(self, message, context)
    }

    fn poll(&mut self) -> Option<Event> {
        Conversation::poll(self)
    }

    fn cancel(&mut self) {
        Conversation::cancel(self);
    }

    fn session_id(&self) -> Option<SessionId> {
        Conversation::session_id(self)
    }
}

// ---- a back end that keeps nothing ----------------------------------------

/// The back end of a run that has no settings folder: it can only say so.
pub struct Nowhere;

impl Backend for Nowhere {
    fn check(&mut self, _ctx: &egui::Context) {}

    fn checked(&mut self) -> Option<Result<Ready, Problem>> {
        Some(Err(Problem::NoFolder))
    }

    fn open(&mut self, _: &Ready, _: Option<SessionId>, _: &Setup) -> Result<Box<dyn Chat>, String> {
        Err(Problem::NoFolder.to_string())
    }
}

// ---- a scripted stand-in --------------------------------------------------

/// A message the panel sent to a scripted back end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub message: String,
    pub context: Option<String>,
}

/// What a scripted back end will say, and what it was told.
#[derive(Default)]
pub struct ScriptState {
    /// What the next checks find. The last answer is given again and again.
    pub answers: VecDeque<Result<Ready, Problem>>,
    /// While set, a check gets no answer: the CLI is "still being looked
    /// for".
    pub hold_answer: bool,
    /// How many times the panel asked.
    pub checks: usize,
    /// A check is waiting for its answer.
    asked: bool,
    /// What the turn under way reports next.
    pub events: VecDeque<Event>,
    pub sent: Vec<Sent>,
    pub cancels: usize,
    /// The conversations opened, each with the id it was to resume.
    pub opened: Vec<Option<SessionId>>,
    /// The model each of them was opened for; `None` is the CLI's choice.
    pub models: Vec<Option<String>>,
    /// Everything each of them was opened with.
    pub setups: Vec<Setup>,
    /// The id the conversation goes by once a turn has started.
    pub session: Option<SessionId>,
    busy: bool,
}

/// A back end that says what it is told to. Clones share one script: the
/// panel holds one, and whoever writes the script holds another.
#[derive(Clone, Default)]
pub struct Script(Arc<Mutex<ScriptState>>);

impl Script {
    /// A script whose checks find the assistant ready.
    pub fn ready() -> Self {
        let script = Self::default();
        script.answer(Ok(ready()));
        script
    }

    /// A script whose checks find `problem`.
    pub fn unavailable(problem: Problem) -> Self {
        let script = Self::default();
        script.answer(Err(problem));
        script
    }

    /// What the checks find from now on.
    pub fn answer(&self, answer: Result<Ready, Problem>) {
        let mut state = self.0.lock();
        state.answers.clear();
        state.answers.push_back(answer);
    }

    /// Has the turn under way report `events`, in order.
    pub fn say(&self, events: impl IntoIterator<Item = Event>) {
        self.0.lock().events.extend(events);
    }

    /// What was asked of the script so far, and what it has yet to say.
    #[cfg_attr(not(test), expect(dead_code, reason = "what the panel asked for is looked at by the tests"))]
    pub fn state(&self) -> parking_lot::MutexGuard<'_, ScriptState> {
        self.0.lock()
    }
}

/// A CLI that is installed and signed in with a Claude account, for
/// scripts.
pub fn ready() -> Ready {
    use trill_assistant::{CliInstallation, CliVersion, LoginMethod};
    Ready {
        cli: CliStatus {
            installation: CliInstallation { path: PathBuf::from("claude"), version: CliVersion::new(2, 1, 325) },
            login: LoginMethod::Subscription { plan: None },
        },
        mcp_url: "http://127.0.0.1:7777/assistant".to_owned(),
    }
}

impl Backend for Script {
    fn check(&mut self, _ctx: &egui::Context) {
        let mut state = self.0.lock();
        state.checks += 1;
        state.asked = true;
    }

    fn checked(&mut self) -> Option<Result<Ready, Problem>> {
        let mut state = self.0.lock();
        if !state.asked || state.hold_answer {
            return None;
        }
        state.asked = false;
        match state.answers.len() {
            0 => Some(Ok(ready())),
            1 => state.answers.front().cloned(),
            _ => state.answers.pop_front(),
        }
    }

    fn open(&mut self, ready: &Ready, resume: Option<SessionId>, setup: &Setup) -> Result<Box<dyn Chat>, String> {
        // What the real one refuses, this one refuses in the same words.
        let asked = config(Path::new("nowhere"), &ready.mcp_url, setup);
        Conversation::new(asked, ready.cli.installation.clone()).map_err(|error| error.to_string())?;
        let mut state = self.0.lock();
        state.opened.push(resume.clone());
        state.models.push(setup.model.clone());
        state.setups.push(setup.clone());
        if resume.is_some() {
            state.session = resume;
        }
        Ok(Box::new(self.clone()))
    }
}

impl Chat for Script {
    fn send(&mut self, message: &str, context: Option<&str>) -> Result<(), SendError> {
        let mut state = self.0.lock();
        if state.busy {
            return Err(SendError::TurnInProgress);
        }
        if message.trim().is_empty() {
            return Err(SendError::EmptyMessage);
        }
        state.busy = true;
        state.sent.push(Sent { message: message.to_owned(), context: context.map(str::to_owned) });
        Ok(())
    }

    fn poll(&mut self) -> Option<Event> {
        let mut state = self.0.lock();
        if !state.busy {
            return None;
        }
        let event = state.events.pop_front()?;
        match &event {
            Event::Started(start) => state.session = Some(start.session_id.clone()),
            // As the real conversation does: the id is forgotten, and the
            // next message starts afresh.
            Event::Failed(trill_assistant::FailureReason::SessionNotFound) => state.session = None,
            _ => {}
        }
        if event.is_terminal() {
            state.busy = false;
        }
        Some(event)
    }

    fn cancel(&mut self) {
        self.0.lock().cancels += 1;
    }

    fn session_id(&self) -> Option<SessionId> {
        self.0.lock().session.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_works_in_a_folder_of_trills_own_with_every_tool() {
        let haiku = Setup { model: Some("haiku".into()), ..Setup::default() };
        let config = config(Path::new("settings"), "http://127.0.0.1:9000/assistant", &haiku);
        assert_eq!(config.working_dir, Path::new("settings").join("assistant").join("work"));
        assert_eq!(config.temp_dir, Path::new("settings").join("assistant").join("tmp"));
        assert_eq!(config.mcp_url, "http://127.0.0.1:9000/assistant");
        assert_eq!(config.model.as_deref(), Some("haiku"));
        assert!(config.withheld_tools.is_empty(), "the server's guarded door does the keeping; no tool is withheld");
        assert_eq!(config.cli_path, None, "the CLI is the one the user installed, wherever that is");
        assert!(!config.keep_redirecting_environment);
        // With no model named, the choice is the CLI's.
        assert_eq!(super::config(Path::new("settings"), "http://127.0.0.1:1/assistant", &Setup::default()).model, None);
    }

    #[test]
    fn what_the_user_chose_is_what_the_cli_is_run_with() {
        use trill_assistant::{DEFAULT_MAX_TURNS, PLAN_TOOLS};
        // As the panel starts out: the web on, a plan, the CLI's own effort,
        // and a limit on one request that a whole song stays well inside.
        let usual = config(Path::new("settings"), "http://127.0.0.1:9000/assistant", &Setup::default());
        assert!(usual.web && usual.plan);
        assert_eq!((usual.effort, usual.max_turns), (None, Some(DEFAULT_MAX_TURNS)));
        // Several times the eighty steps one message used to be allowed.
        const { assert!(DEFAULT_MAX_TURNS >= 5 * 80) };
        assert_eq!(usual.cli_tools(), ["TaskCreate", "TaskGet", "TaskList", "TaskUpdate", "WebFetch", "WebSearch"]);

        let chosen = Setup { model: None, effort: Some(Effort::High), web: false, step_limit: 250 };
        let config = config(Path::new("settings"), "http://127.0.0.1:9000/assistant", &chosen);
        assert_eq!((config.effort, config.max_turns, config.web), (Some(Effort::High), Some(250), false));
        // With the web off its tools are not asked for at all; the plan,
        // which reaches nothing, stays.
        assert_eq!(config.cli_tools(), PLAN_TOOLS);
    }

    /// One request to a server on this machine, as a program that is not an
    /// MCP client would make it, and the status line of the answer.
    fn request(address: SocketAddr, request: &str) -> String {
        use std::io::{Read, Write};
        let mut stream = TcpStream::connect(address).expect("the server");
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut answer = Vec::new();
        let mut buffer = [0u8; 2048];
        // The first line is enough, and a stream of events never ends.
        while !answer.contains(&b'\n') {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => answer.extend_from_slice(&buffer[..read]),
            }
        }
        String::from_utf8_lossy(&answer).lines().next().unwrap_or_default().to_owned()
    }

    /// The web tools fetch pages, and Trill's server is on this machine
    /// with a door that is not guarded (`/mcp`) beside the assistant's. A
    /// page that talked the assistant into fetching `http://127.0.0.1:.../mcp`
    /// must get nothing done there. Three things stand in the way, and the
    /// two that are Trill's are checked here:
    ///
    /// * the CLI's page reader asks for everything over HTTPS, whatever was
    ///   written, and refuses `localhost` (recorded in `trill-assistant`,
    ///   `tests/fixtures/web_lookups.jsonl`); the server speaks plain HTTP,
    ///   so no request of the reader's ever reaches it as a request;
    /// * a request that only reads (GET) calls no tool: a tool call is a
    ///   POST with a body, in a session the server has opened;
    /// * a request that says it comes from a web page (an `Origin`) is
    ///   refused, whatever it asks.
    #[test]
    fn nothing_a_page_reader_can_send_drives_the_server() {
        let runtime = Arc::new(Runtime::start(trill_runtime::RuntimeOptions::hermetic()).unwrap());
        let before = runtime.session.lock().project().clone();
        let server = trill_mcp::serve_http(runtime.clone(), SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let (address, host) = (server.address(), server.address().to_string());

        for door in [trill_mcp::HTTP_PATH, trill_mcp::ASSISTANT_PATH] {
            // What a browser, or a page reader, sends for an address.
            let get = format!("GET {door} HTTP/1.1\r\nHost: {host}\r\nAccept: text/html,*/*\r\n\r\n");
            let status = request(address, &get);
            assert!(status.contains(" 4"), "a plain GET of {door} is refused: {status:?}");
            // With the words a tool call would need, in the address.
            let get = format!(
                "GET {door}?method=tools/call&name=new_project HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\n\r\n"
            );
            assert!(request(address, &get).contains(" 4"), "{door}");
            // As an MCP client asks for the server's stream of events,
            // without a session of its own: nothing to listen to.
            let get = format!("GET {door} HTTP/1.1\r\nHost: {host}\r\nAccept: text/event-stream\r\n\r\n");
            assert!(request(address, &get).contains(" 4"), "{door}");

            // A page's own request, which a reader does not make but a
            // browser would: refused for where it says it comes from.
            let body =
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"new_project","arguments":{}}}"#;
            let post = format!(
                "POST {door} HTTP/1.1\r\nHost: {host}\r\nOrigin: https://example.com\r\nContent-Type: application/json\r\n\
                 Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let status = request(address, &post);
            assert!(status.contains(" 403"), "a request from a web page is refused at {door}: {status:?}");
            // And under a name of the page's own for this machine.
            let get = format!("GET {door} HTTP/1.1\r\nHost: evil.example.com\r\nAccept: */*\r\n\r\n");
            assert!(request(address, &get).contains(" 4"), "{door}");
        }
        // What a reader that insists on HTTPS sends is not a request at all.
        let hello = request(address, "\u{16}\u{3}\u{1}\u{0}\u{5}hello");
        assert!(hello.is_empty() || hello.contains(" 400"), "{hello:?}");

        assert_eq!(*runtime.session.lock().project(), before, "and the song is as it was");
    }

    #[test]
    fn the_assistant_comes_in_by_the_servers_guarded_door() {
        // Whatever address the window's server shows, the assistant's is
        // the one the server itself gives for it.
        let runtime = Arc::new(Runtime::start(trill_runtime::RuntimeOptions::hermetic()).unwrap());
        let server = trill_mcp::serve_http(runtime, SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        assert_eq!(assistant_door(&server.url()), Some(server.assistant_url()));
        assert!(server.assistant_url().ends_with("/assistant"));
        assert!(listening(&server.assistant_url()));
        assert_eq!(assistant_door("http://127.0.0.1:7777/somewhere-else"), None);
    }

    #[test]
    fn a_server_is_listening_or_it_is_not() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(listening(&format!("http://127.0.0.1:{port}/mcp")));
        drop(listener);
        assert!(!listening(&format!("http://127.0.0.1:{port}/mcp")));
        assert!(!listening("not an address"));
        assert!(!listening("http://example.invalid/mcp"), "only an address is tried, never a name looked up");
    }

    #[test]
    fn a_run_without_a_folder_says_so() {
        let mut backend = Nowhere;
        assert_eq!(backend.checked(), Some(Err(Problem::NoFolder)));
        assert!(backend.open(&ready(), None, &Setup::default()).is_err());
        assert!(Problem::NoFolder.to_string().contains("settings folder"));
    }

    #[test]
    fn a_script_answers_checks_and_replays_a_turn() {
        let ctx = egui::Context::default();
        let script = Script::unavailable(Problem::Cli(Unavailable::NotInstalled));
        let mut backend: Box<dyn Backend> = Box::new(script.clone());
        assert_eq!(backend.checked(), None, "nothing was asked yet");
        backend.check(&ctx);
        assert_eq!(backend.checked(), Some(Err(Problem::Cli(Unavailable::NotInstalled))));
        assert_eq!(backend.checked(), None, "an answer is given once");

        script.answer(Ok(ready()));
        script.state().hold_answer = true;
        backend.check(&ctx);
        assert_eq!(backend.checked(), None, "held back");
        script.state().hold_answer = false;
        assert_eq!(backend.checked(), Some(Ok(ready())));
        assert_eq!(script.state().checks, 2);

        let haiku = Setup { model: Some("haiku".into()), effort: Some(Effort::Low), ..Setup::default() };
        let mut chat = backend.open(&ready(), None, &haiku).unwrap();
        assert_eq!(chat.poll(), None);
        assert_eq!(chat.send("  ", None), Err(SendError::EmptyMessage));
        chat.send("hello", Some("tempo: 120")).unwrap();
        assert_eq!(chat.send("again", None), Err(SendError::TurnInProgress));
        script.say([Event::Text("hi".into()), Event::Cancelled]);
        assert_eq!(chat.poll(), Some(Event::Text("hi".into())));
        chat.cancel();
        assert_eq!(chat.poll(), Some(Event::Cancelled));
        assert_eq!(chat.poll(), None);
        chat.send("again", None).unwrap();
        let state = script.state();
        assert_eq!(state.sent[0], Sent { message: "hello".into(), context: Some("tempo: 120".into()) });
        assert_eq!((state.sent.len(), state.cancels, state.opened.len()), (2, 1, 1));
        assert_eq!(state.models, [Some("haiku".to_owned())], "the model it was opened for is on record");
        assert_eq!(state.setups, [haiku], "and everything else it was opened with");
        drop(state);
        // A name the command line would take for an option opens nothing.
        let flag = Setup { model: Some("--dangerously-skip-permissions".into()), ..Setup::default() };
        let refused = backend.open(&ready(), None, &flag).err().expect("refused");
        assert!(refused.contains("is not a model name"), "{refused}");
        assert_eq!(script.state().opened.len(), 1);
    }
}
