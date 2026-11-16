//! The conversation as the panel shows it: what was said, each tool call
//! and each look at the web as a line, the notices, how each turn ended,
//! and beside it the assistant's plan.
//!
//! This is the app's own record, built from the events of a turn as they
//! arrive. It is what is drawn, and what is kept beside the settings so
//! that a conversation taken up again another day can be read back. (The
//! CLI keeps a record of its own for the assistant's memory; that one is
//! never read here.)

use super::places::{self, Place};
use super::tools;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use trill_assistant::{Event, FailureReason, Notice, PlanChange, StepStatus, TurnSummary, WebLookup};
use trill_core::Project;
use trill_engine::session::UndoStep;

/// More items than this and the oldest exchanges are let go.
const MAX_ITEMS: usize = 600;
/// How much of a tool's arguments or of its answer is kept. `get_project`
/// on a full song answers with hundreds of kilobytes; nobody reads that in
/// a side panel, and the assistant has it regardless.
const MAX_DETAIL: usize = 4000;

/// How a tool call stands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    Running,
    Done,
    /// The tool reported an error (which the assistant also sees, and
    /// usually puts right).
    Failed,
    /// The turn ended before the call did.
    Stopped,
}

/// One tool call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolLine {
    /// Pairs the start of a call with its end. Means nothing afterwards.
    #[serde(skip)]
    pub id: String,
    /// Trill's name for the tool: `create_track`.
    pub tool: String,
    /// What it is doing, in words, while it runs.
    pub doing: String,
    /// What it did, in words.
    pub did: String,
    pub state: ToolState,
    /// The arguments, as the tool received them.
    pub input: String,
    /// What the tool answered, or the error it reported.
    pub output: String,
    /// Whether the line is unfolded to show the two above.
    #[serde(skip)]
    pub open: bool,
    /// What in the song the call made or changed, if it did and said so:
    /// where the line leads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<Place>,
    /// The arguments as they came, until the call has answered: what it
    /// changed is read from the two together.
    #[serde(skip)]
    pub arguments: serde_json::Value,
}

impl ToolLine {
    /// The line as it reads now: what it did once it has done it, and
    /// until then, or if it never did, what it set out to do.
    pub fn words(&self) -> &str {
        match self.state {
            ToolState::Done => &self.did,
            ToolState::Running | ToolState::Failed | ToolState::Stopped => &self.doing,
        }
    }
}

/// What a line the panel adds on its own is for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Info,
    Warning,
}

/// What the user can do about a turn that did not finish.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Offer {
    Nothing,
    /// Send the same message again.
    TryAgain,
    /// Ask the assistant to carry on from where it stopped.
    Continue,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum Ending {
    Finished {
        /// How long the CLI worked on the message.
        seconds: f32,
        /// How many times the model was asked.
        steps: u32,
        /// What the CLI estimates the turn would cost at API prices, in US
        /// dollars. An estimate, never a charge.
        estimated_cost: Option<f64>,
    },
    /// The user stopped it.
    Stopped,
    Failed {
        /// The reason, in the back end's own words.
        text: String,
        offer: Offer,
    },
}

/// What a turn that did not finish had taken when it ended, as far as the
/// panel was told. (One that finished says so itself.)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spent {
    /// How long it ran, by the clock on the wall.
    pub seconds: f32,
    /// How many times the model had been asked.
    pub steps: u32,
    /// What the CLI estimated the first `costed_steps` of them would cost
    /// at API prices: it says when one of its runs ends, so the last
    /// stretch is not in it. `None` if it never said.
    pub estimated_cost: Option<f64>,
    pub costed_steps: u32,
}

/// The line that closes a turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnEnd {
    #[serde(flatten)]
    pub ending: Ending,
    /// For a turn that was stopped or failed after it had got going.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent: Option<Spent>,
    /// Whether the turn changed the song.
    pub changed: bool,
    /// The undo step that holds everything the turn changed. Known only to
    /// the session that made it, so it is not kept between runs.
    #[serde(skip)]
    pub step: Option<UndoStep>,
    /// What the turn changed, where its calls did not say: read off the
    /// song before and after. Empty when the lines of the turn lead there
    /// themselves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<Place>,
}

impl TurnEnd {
    /// A turn that ended so, having changed nothing.
    pub fn unchanged(ending: Ending) -> Self {
        Self { ending, spent: None, changed: false, step: None, places: Vec::new() }
    }
}

/// How a step of the assistant's plan stands.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Doing,
    Done,
}

impl From<StepStatus> for StepState {
    fn from(status: StepStatus) -> Self {
        match status {
            StepStatus::Pending => StepState::Pending,
            StepStatus::InProgress => StepState::Doing,
            StepStatus::Done => StepState::Done,
        }
    }
}

/// One step of the assistant's plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// The CLI's number for it. Means nothing to the user.
    pub id: String,
    /// What is to be done: "Write the drums".
    pub title: String,
    /// As it reads while it is being done ("Writing the drums"), if the
    /// assistant said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doing: Option<String>,
    pub state: StepState,
}

impl Step {
    /// The step as it reads now.
    pub fn words(&self) -> &str {
        match (&self.doing, self.state) {
            (Some(doing), StepState::Doing) => doing,
            _ => &self.title,
        }
    }
}

/// The assistant's plan for what it was asked: the steps it wrote down, in
/// its order, and how each stands. The CLI keeps the list for the model;
/// this is the panel's copy, kept in step by the changes the back end
/// reports, to show as a checklist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Plan {
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// How many of the steps are done.
    pub fn done(&self) -> usize {
        self.steps.iter().filter(|step| step.state == StepState::Done).count()
    }

    /// Whether there was a plan and all of it is done.
    pub fn is_finished(&self) -> bool {
        !self.steps.is_empty() && self.done() == self.steps.len()
    }

    /// The step being worked on, or failing that the next one to do.
    pub fn current(&self) -> Option<&Step> {
        let doing = self.steps.iter().find(|step| step.state == StepState::Doing);
        doing.or_else(|| self.steps.iter().find(|step| step.state == StepState::Pending))
    }

    /// Takes in a change to the plan.
    pub fn apply(&mut self, change: &PlanChange) {
        match change {
            PlanChange::Added { id, title, doing } => match self.steps.iter_mut().find(|step| step.id == *id) {
                // Written down twice: the second wording stands.
                Some(step) => {
                    step.title.clone_from(title);
                    step.doing.clone_from(doing);
                }
                None => self.steps.push(Step {
                    id: id.clone(),
                    title: title.clone(),
                    doing: doing.clone(),
                    state: StepState::Pending,
                }),
            },
            PlanChange::Changed { id, status, title, doing } => {
                let at = self.steps.iter().position(|step| step.id == *id).unwrap_or_else(|| {
                    // A step the panel never saw written down (its start
                    // went by unseen). It is shown by what is known of it.
                    let title = title.clone().unwrap_or_else(|| format!("Step {id}"));
                    self.steps.push(Step { id: id.clone(), title, doing: None, state: StepState::Pending });
                    self.steps.len() - 1
                });
                let step = &mut self.steps[at];
                if let Some(status) = status {
                    step.state = (*status).into();
                }
                if let Some(title) = title {
                    step.title.clone_from(title);
                }
                if doing.is_some() {
                    step.doing.clone_from(doing);
                }
            }
            PlanChange::Removed { id } => self.steps.retain(|step| step.id != *id),
            PlanChange::Listed(listed) => {
                // The CLI's word on what the plan is. How a step reads
                // while it is done is not in the list, and is kept.
                let old = std::mem::take(&mut self.steps);
                self.steps = listed
                    .iter()
                    .map(|step| Step {
                        id: step.id.clone(),
                        title: step.title.clone(),
                        doing: old.iter().find(|known| known.id == step.id).and_then(|known| known.doing.clone()),
                        state: step.status.into(),
                    })
                    .collect();
            }
        }
    }

    /// A new request begins. A plan that was carried out belongs to the
    /// request it was made for, and is put away; one with steps still to do
    /// stays, for the assistant may be about to carry on with it.
    pub fn begin_request(&mut self) {
        if self.is_finished() {
            self.steps.clear();
        }
    }
}

/// One thing in the conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    /// What the user sent.
    User {
        text: String,
    },
    /// A passage of the assistant's reply.
    Reply {
        text: String,
    },
    Tool(ToolLine),
    /// Something the back end or the panel has to say.
    Note {
        text: String,
        tone: Tone,
        /// Notes of one slot replace each other: "trying again (2 of 10)"
        /// takes the place of "(1 of 10)".
        #[serde(skip)]
        slot: &'static str,
    },
    End(TurnEnd),
}

/// The conversation so far.
#[derive(Clone, Debug)]
pub struct Transcript {
    pub items: Vec<Item>,
    /// The assistant's plan, as it stands.
    pub plan: Plan,
    /// Tells this conversation's items from any other's, and from its own
    /// once they are no longer where they were: it changes when the oldest
    /// are let go. What the panel keeps about an item by its place in the
    /// list (how its text was set) holds for one epoch.
    pub epoch: u64,
}

/// An epoch no conversation has had.
fn new_epoch() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Default for Transcript {
    fn default() -> Self {
        Self { items: Vec::new(), plan: Plan::default(), epoch: new_epoch() }
    }
}

impl PartialEq for Transcript {
    /// Two conversations are the same if the same was said in them and the
    /// same was planned.
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items && self.plan == other.plan
    }
}

/// What is kept of a conversation between runs.
#[derive(Deserialize)]
#[serde(untagged)]
enum Kept {
    /// As it is kept now.
    Whole {
        items: Vec<Item>,
        #[serde(default)]
        plan: Plan,
    },
    /// As it was kept before the assistant had a plan: the items alone.
    Items(Vec<Item>),
}

#[derive(Serialize)]
struct Keeping<'a> {
    items: &'a [Item],
    plan: &'a Plan,
}

/// `text`, cut to what is kept of a tool's arguments or answer.
fn detail(text: &str) -> String {
    match text.char_indices().nth(MAX_DETAIL) {
        Some((end, _)) => {
            format!("{}\u{2026}\n({} more characters)", &text[..end], text[end..].chars().count())
        }
        None => text.to_owned(),
    }
}

/// How long until `when`, in words: "in about 2 hours", "in 25 minutes".
/// Relative, because telling the time of day would need the time zone.
fn until(when: SystemTime, now: SystemTime) -> Option<String> {
    let minutes = when.duration_since(now).ok()?.as_secs().div_ceil(60);
    Some(match minutes {
        0..=1 => "in a minute".to_owned(),
        2..=89 => format!("in {minutes} minutes"),
        _ => format!("in about {} hours", (minutes + 30) / 60),
    })
}

/// A notice as a line of the conversation.
fn note(notice: &Notice, now: SystemTime) -> Item {
    let (tone, slot) = match notice {
        Notice::EnvironmentVariableIgnored { .. } => (Tone::Info, ""),
        Notice::Retrying { .. } => (Tone::Warning, "retry"),
        Notice::RateLimited { .. } => (Tone::Warning, "retry"),
        Notice::UsageLimitNear { .. } => (Tone::Warning, "limit"),
        Notice::PermissionDenied { .. } => (Tone::Warning, ""),
        // One line for a conversation being summarised: first that it is,
        // then, in its place, that it was.
        Notice::Compacting | Notice::Compacted { .. } => (Tone::Info, "summary"),
        Notice::CompactionFailed { .. } => (Tone::Warning, "summary"),
    };
    let mut text = notice.to_string();
    if let Notice::UsageLimitNear { resets_at: Some(when) } = notice
        && let Some(wait) = until(*when, now)
    {
        // The CLI's own word on when; nothing is worked out from the plan.
        text.push_str(&format!(" Claude Code says it renews {wait}."));
    }
    Item::Note { text, tone, slot }
}

/// What to offer after a failure. `worked` says the turn had got as far as
/// using a tool: what it did is done, and sending the same request again
/// would ask for it a second time.
pub fn offer_for(reason: &FailureReason, worked: bool) -> Offer {
    match reason {
        FailureReason::TurnLimitReached { .. } => Offer::Continue,
        // Sending it again would be refused again.
        FailureReason::UnexpectedTools { .. } => Offer::Nothing,
        // The assistant remembers nothing of the request: it has to be
        // made again.
        FailureReason::SessionNotFound => Offer::TryAgain,
        _ if worked => Offer::Continue,
        _ => Offer::TryAgain,
    }
}

/// Whether a failure means the assistant has to be looked for again before
/// another message can go: the CLI is gone or signed out, or the server it
/// works through is not answering.
pub fn calls_for_a_check(reason: &FailureReason) -> bool {
    matches!(
        reason,
        FailureReason::CliMissing { .. }
            | FailureReason::NotLoggedIn
            | FailureReason::McpServerUnavailable { .. }
            | FailureReason::CannotStart { .. }
    )
}

/// The reason for a failure, as shown: the back end's words, with when a
/// used-up allowance renews if the CLI said.
pub fn failure_text(reason: &FailureReason, now: SystemTime) -> String {
    let mut text = reason.to_string();
    if let FailureReason::UsageLimitReached { resets_at: Some(when), .. } = reason
        && let Some(wait) = until(*when, now)
    {
        text.push_str(&format!(" It says the limit renews {wait}."));
    }
    text
}

impl Transcript {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The user's message opens a turn.
    pub fn push_user(&mut self, text: &str) {
        self.items.push(Item::User { text: text.trim().to_owned() });
        self.trim();
    }

    /// Takes in an event of the turn under way. The three that end a turn
    /// are for [`Self::end`], which needs to know what the turn changed.
    pub fn apply(&mut self, event: &Event, project: &Project) {
        match event {
            Event::Text(piece) => match self.items.last_mut() {
                Some(Item::Reply { text }) => text.push_str(piece),
                _ => {
                    let piece = piece.trim_start();
                    if !piece.is_empty() {
                        self.items.push(Item::Reply { text: piece.to_owned() });
                    }
                }
            },
            Event::ToolCallStarted { id, tool, input } => {
                let wording = tools::describe(tool, input, project);
                let pretty = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
                self.items.push(Item::Tool(ToolLine {
                    id: id.clone(),
                    tool: tool.clone(),
                    doing: wording.doing,
                    did: wording.did,
                    state: ToolState::Running,
                    input: detail(&pretty),
                    output: String::new(),
                    open: false,
                    place: None,
                    arguments: input.clone(),
                }));
            }
            Event::WebStarted { id, lookup } => {
                let wording = tools::describe_web(lookup);
                // The CLI's own name for the tool, and what it was given.
                let (tool, given) = match lookup {
                    WebLookup::Search { query } => ("WebSearch", query.clone()),
                    WebLookup::Page { url, question } if question.is_empty() => ("WebFetch", url.clone()),
                    WebLookup::Page { url, question } => ("WebFetch", format!("{url}\n\n{question}")),
                };
                self.items.push(Item::Tool(ToolLine {
                    id: id.clone(),
                    tool: tool.to_owned(),
                    doing: wording.doing,
                    did: wording.did,
                    state: ToolState::Running,
                    input: detail(&given),
                    output: String::new(),
                    open: false,
                    place: None,
                    arguments: serde_json::Value::Null,
                }));
            }
            Event::WebFinished { id, result } => {
                let (state, output) = match result {
                    Ok(answer) => (ToolState::Done, detail(answer)),
                    Err(error) => (ToolState::Failed, detail(error)),
                };
                let running = self.items.iter_mut().rev().find_map(|item| match item {
                    Item::Tool(line) if line.id == *id && line.state == ToolState::Running => Some(line),
                    _ => None,
                });
                if let Some(line) = running {
                    line.state = state;
                    line.output = output;
                }
            }
            Event::Plan(change) => self.plan.apply(change),
            Event::ToolCallFinished { id, tool, result } => {
                let (state, output) = match result {
                    Ok(answer) => (ToolState::Done, detail(answer)),
                    Err(error) => (ToolState::Failed, detail(error)),
                };
                // Read from the whole answer, not from what is kept of it.
                let made = |arguments: &serde_json::Value| match result {
                    Ok(answer) => places::of_call(tool, arguments, answer),
                    Err(_) => None,
                };
                let running = self.items.iter_mut().rev().find_map(|item| match item {
                    Item::Tool(line) if line.id == *id && line.state == ToolState::Running => Some(line),
                    _ => None,
                });
                match running {
                    Some(line) => {
                        line.state = state;
                        line.output = output;
                        line.place = made(&std::mem::take(&mut line.arguments));
                    }
                    // The start was never seen: the end alone still says
                    // what was done.
                    None => {
                        let wording = tools::describe(tool, &serde_json::Value::Null, project);
                        self.items.push(Item::Tool(ToolLine {
                            id: id.clone(),
                            tool: tool.clone(),
                            doing: wording.doing,
                            did: wording.did,
                            state,
                            input: String::new(),
                            output,
                            open: false,
                            place: made(&serde_json::Value::Null),
                            arguments: serde_json::Value::Null,
                        }));
                    }
                }
            }
            Event::Notice(notice) => {
                let new = note(notice, SystemTime::now());
                let slot_of = |item: &Item| match item {
                    Item::Note { slot, .. } => *slot,
                    _ => "",
                };
                let slot = slot_of(&new);
                match self.items.last_mut() {
                    Some(last) if !slot.is_empty() && slot_of(last) == slot => *last = new,
                    _ => self.items.push(new),
                }
            }
            // How far the turn has come is the turn's to know, not the
            // conversation's.
            Event::Progress(_) => {}
            Event::Started(_) | Event::Finished(_) | Event::Cancelled | Event::Failed(_) => {}
        }
    }

    /// Whether the turn under way has used a tool or the web yet.
    pub fn turn_has_worked(&self) -> bool {
        self.items[self.turn_start(self.items.len())..].iter().any(|item| matches!(item, Item::Tool(_)))
    }

    /// Closes the turn: calls still running are marked as cut short, and
    /// the line that says how it ended goes last.
    pub fn end(&mut self, end: TurnEnd) {
        for item in self.items.iter_mut().rev() {
            match item {
                Item::Tool(line) if line.state == ToolState::Running => line.state = ToolState::Stopped,
                Item::User { .. } | Item::End(_) => break,
                _ => {}
            }
        }
        self.items.push(Item::End(end));
        self.trim();
    }

    /// The last message the user sent.
    pub fn last_message(&self) -> Option<&str> {
        self.items.iter().rev().find_map(|item| match item {
            Item::User { text } => Some(text.as_str()),
            _ => None,
        })
    }

    /// Lets go of the oldest exchanges once there are too many items,
    /// whole exchanges at a time so that none begins in its middle.
    fn trim(&mut self) {
        while self.items.len() > MAX_ITEMS {
            let next = self.items.iter().skip(1).position(|item| matches!(item, Item::User { .. }));
            match next {
                Some(index) => {
                    self.items.drain(..=index);
                    // Nothing is where it was.
                    self.epoch = new_epoch();
                }
                None => break,
            }
        }
    }

    /// Where the turn that the item at `at` belongs to, or closes, begins:
    /// after the message that opened it.
    fn turn_start(&self, at: usize) -> usize {
        let before = &self.items[..at.min(self.items.len())];
        before.iter().rposition(|item| matches!(item, Item::User { .. } | Item::End(_))).map_or(0, |index| index + 1)
    }

    /// What the turn closed by the line at `end` changed in the song, in
    /// the order it did: where each of its calls leads, then what its
    /// closing line adds.
    pub fn turn_places(&self, end: usize) -> impl Iterator<Item = &Place> {
        let lines = self.items[self.turn_start(end)..end.min(self.items.len())].iter().filter_map(|item| match item {
            Item::Tool(line) => line.place.as_ref(),
            _ => None,
        });
        let closing = match self.items.get(end) {
            Some(Item::End(end)) => end.places.as_slice(),
            _ => &[],
        };
        lines.chain(closing)
    }

    /// Settles where the lines of the turn under way lead, as it ends and
    /// against the song as it then is: see [`Place::settled`].
    pub fn settle_places(&mut self, project: &Project) {
        let start = self.turn_start(self.items.len());
        for item in &mut self.items[start..] {
            if let Item::Tool(line) = item {
                line.place = line.place.take().and_then(|place| place.settled(project));
            }
        }
    }

    /// Whether `event` is the answer of a call that put another project in
    /// place of the open one: one was loaded, begun, or brought back. Ask
    /// before the event is applied, while the call's arguments are at hand.
    pub fn opens_another_project(&self, event: &Event) -> bool {
        let Event::ToolCallFinished { id, tool, result: Ok(_) } = event else { return false };
        match tool.as_str() {
            "load_project" | "new_project" => true,
            "recover_project" => self.items.iter().rev().any(|item| {
                matches!(item, Item::Tool(line) if line.id == *id
                    && line.arguments.get("action").and_then(serde_json::Value::as_str) == Some("open"))
            }),
            _ => false,
        }
    }

    /// Forgets where every line leads: the project they led into has been
    /// replaced by another, in which the same ids name other things.
    pub fn forget_places(&mut self) {
        for item in &mut self.items {
            match item {
                Item::Tool(line) => line.place = None,
                Item::End(end) => end.places.clear(),
                _ => {}
            }
        }
    }

    /// For a conversation read back another day: keeps only the places
    /// that still name the same things in `project`.
    pub fn keep_places_in(&mut self, project: &Project) {
        for item in &mut self.items {
            match item {
                Item::Tool(line) => line.place = line.place.take().filter(|place| place.is_the_same_in(project)),
                Item::End(end) => end.places.retain(|place| place.is_the_same_in(project)),
                _ => {}
            }
        }
    }

    /// What is kept between runs.
    pub fn to_json(&self) -> String {
        let keeping = Keeping { items: &self.items, plan: &self.plan };
        serde_json::to_string(&keeping).unwrap_or_else(|_| "[]".to_owned())
    }

    /// Reads a kept conversation. One that was cut off when the program
    /// closed mid-turn is closed here; text that makes no sense is no
    /// conversation.
    pub fn from_json(text: &str) -> Option<Self> {
        let (items, plan) = match serde_json::from_str(text).ok()? {
            Kept::Whole { items, plan } => (items, plan),
            Kept::Items(items) => (items, Plan::default()),
        };
        let mut transcript = Self { items, plan, epoch: new_epoch() };
        let open = transcript.items.iter().rev().find_map(|item| match item {
            Item::End(_) => Some(false),
            Item::User { .. } => Some(true),
            _ => None,
        });
        if open == Some(true) {
            transcript.end(TurnEnd::unchanged(Ending::Stopped));
        }
        Some(transcript)
    }
}

/// How a finished turn is summed up.
pub fn finished(summary: &TurnSummary) -> Ending {
    Ending::Finished {
        seconds: summary.duration.as_secs_f32(),
        steps: summary.turns,
        estimated_cost: summary.estimated_cost_usd,
    }
}

/// `"8 s"`, `"1 min 12 s"`.
pub fn format_seconds(seconds: f32) -> String {
    let whole = Duration::from_secs_f32(seconds.max(0.0)).as_secs();
    match whole {
        0..=59 => format!("{whole} s"),
        _ => format!("{} min {} s", whole / 60, whole % 60),
    }
}

/// The CLI's cost figure as shown: always marked as an estimate.
pub fn format_estimate(dollars: f64) -> String {
    if dollars < 0.01 { "est. under $0.01".to_owned() } else { format!("est. ${dollars:.2}") }
}

/// A count with its thousands set apart: `"1,000"`.
pub fn format_count(count: u32) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + 1);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `"1 step"`, `"132 steps"`.
pub fn format_steps(steps: u32) -> String {
    if steps == 1 { "1 step".to_owned() } else { format!("{} steps", format_count(steps)) }
}

/// The CLI's cost figure for a turn still under way, or one that did not
/// finish: what it covers is said, because the CLI reports a cost only now
/// and then (`costed` of the `steps` so far).
pub fn format_estimate_so_far(dollars: f64, costed: u32, steps: u32) -> String {
    let estimate = format_estimate(dollars);
    if costed >= steps { estimate } else { format!("{estimate} to step {}", format_count(costed)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trill_assistant::{SessionId, TokenUsage, TurnStart};

    fn text(piece: &str) -> Event {
        Event::Text(piece.to_owned())
    }

    fn started(id: &str, tool: &str, input: serde_json::Value) -> Event {
        Event::ToolCallStarted { id: id.to_owned(), tool: tool.to_owned(), input }
    }

    fn finished_call(id: &str, tool: &str, result: Result<&str, &str>) -> Event {
        Event::ToolCallFinished {
            id: id.to_owned(),
            tool: tool.to_owned(),
            result: result.map(str::to_owned).map_err(str::to_owned),
        }
    }

    fn end(ending: Ending) -> TurnEnd {
        TurnEnd::unchanged(ending)
    }

    fn tool(item: &Item) -> &ToolLine {
        match item {
            Item::Tool(line) => line,
            other => panic!("not a tool line: {other:?}"),
        }
    }

    #[test]
    fn text_arriving_in_pieces_is_one_passage_and_tools_part_passages() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("  add drums \n");
        let start = TurnStart {
            session_id: SessionId::parse("39367d22-e40b-4a04-9358-33e91538ff0d").unwrap(),
            model: "m".into(),
            api_key_source: None,
        };
        for event in [
            Event::Started(start),
            text("\n\nLet me "),
            text("look"),
            text(" first."),
            started("a", "get_project", json!({})),
            finished_call("a", "get_project", Ok("{\"tracks\":[]}")),
            started("b", "create_track", json!({ "name": "Drums" })),
            finished_call("b", "create_track", Err("no such instrument")),
            text("Done.\n\nA second "),
            text("paragraph."),
        ] {
            transcript.apply(&event, &project);
        }
        assert_eq!(transcript.items.len(), 5);
        assert_eq!(transcript.items[0], Item::User { text: "add drums".into() });
        assert_eq!(transcript.items[1], Item::Reply { text: "Let me look first.".into() });
        let read = tool(&transcript.items[2]);
        assert_eq!(
            (read.words(), read.state, read.output.as_str()),
            ("Read the project", ToolState::Done, "{\"tracks\":[]}")
        );
        let failed = tool(&transcript.items[3]);
        assert_eq!(failed.words(), "Adding the track \u{201C}Drums\u{201D}", "a call that failed added nothing");
        assert_eq!((failed.state, failed.output.as_str()), (ToolState::Failed, "no such instrument"));
        assert!(failed.input.contains("\"name\": \"Drums\""), "{}", failed.input);
        assert_eq!(transcript.items[4], Item::Reply { text: "Done.\n\nA second paragraph.".into() });
        assert_eq!(transcript.last_message(), Some("add drums"));
    }

    #[test]
    fn a_running_call_reads_as_under_way_and_is_cut_short_when_the_turn_ends() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("go");
        transcript.apply(&started("a", "get_project", json!({})), &project);
        transcript.apply(&finished_call("a", "get_project", Ok("{}")), &project);
        transcript.apply(&started("b", "write_drums", json!({ "groove": "house" })), &project);
        assert_eq!(tool(&transcript.items[2]).words(), "Writing drums");

        transcript.end(end(Ending::Stopped));
        assert_eq!(tool(&transcript.items[1]).state, ToolState::Done, "what had finished stays finished");
        let cut = tool(&transcript.items[2]);
        assert_eq!((cut.state, cut.words()), (ToolState::Stopped, "Writing drums"));
        assert_eq!(transcript.items[3], Item::End(end(Ending::Stopped)));

        // An end without its start still makes a line; an end for a call
        // that already ended makes another instead of rewriting history.
        transcript.push_user("more");
        transcript.apply(&finished_call("zz", "delete_track", Ok("ok")), &project);
        let orphan = tool(transcript.items.last().unwrap());
        assert_eq!((orphan.state, orphan.tool.as_str(), orphan.input.as_str()), (ToolState::Done, "delete_track", ""));
    }

    #[test]
    fn notices_are_lines_and_repeats_take_each_others_place() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        let retry = |attempt| {
            Event::Notice(Notice::Retrying {
                attempt,
                max_attempts: 10,
                delay: Duration::from_secs(1),
                cause: trill_assistant::ApiErrorKind::Server,
            })
        };
        transcript.apply(&retry(1), &project);
        transcript.apply(&retry(2), &project);
        transcript.apply(&Event::Notice(Notice::PermissionDenied { tool: "render".into() }), &project);
        transcript.apply(&Event::Notice(Notice::PermissionDenied { tool: "render".into() }), &project);
        let texts: Vec<&str> = transcript
            .items
            .iter()
            .map(|item| match item {
                Item::Note { text, .. } => text.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "Connection trouble; trying again (2 of 10).",
                "The assistant was not allowed to use render.",
                "The assistant was not allowed to use render.",
            ]
        );
    }

    #[test]
    fn limits_are_relayed_in_the_clis_words_with_when_it_said() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let later = |seconds| Some(now + Duration::from_secs(seconds));
        let Item::Note { text, tone, .. } = note(&Notice::UsageLimitNear { resets_at: later(2 * 3600 + 600) }, now)
        else {
            panic!("a note")
        };
        assert_eq!(text, "You are close to your Claude usage limit. Claude Code says it renews in about 2 hours.");
        assert_eq!(tone, Tone::Warning);
        let Item::Note { text, .. } = note(&Notice::UsageLimitNear { resets_at: None }, now) else { panic!() };
        assert_eq!(text, "You are close to your Claude usage limit.");

        let reached = FailureReason::UsageLimitReached { message: "Limit reached.".into(), resets_at: later(25 * 60) };
        assert_eq!(
            failure_text(&reached, now),
            "Your Claude usage limit has been reached. Claude Code said: Limit reached. It says the limit renews in 25 \
             minutes."
        );
        // A time already past says nothing rather than something wrong.
        let past = FailureReason::UsageLimitReached { message: "Limit reached.".into(), resets_at: Some(now) };
        assert!(failure_text(&past, now + Duration::from_secs(5)).ends_with("Limit reached."));
        assert_eq!(until(now + Duration::from_secs(30), now).as_deref(), Some("in a minute"));
        assert_eq!(until(now + Duration::from_secs(89 * 60), now).as_deref(), Some("in 89 minutes"));
        assert_eq!(until(now + Duration::from_secs(5 * 3600), now).as_deref(), Some("in about 5 hours"));
    }

    #[test]
    fn each_failure_offers_what_can_be_done_about_it() {
        use FailureReason as F;
        let unreachable = F::McpServerUnavailable { url: "u".into(), status: "failed".into() };
        for setup in [
            F::NotLoggedIn,
            F::CliMissing { path: "claude".into() },
            unreachable,
            F::CannotStart { detail: "d".into() },
        ] {
            assert!(calls_for_a_check(&setup), "{setup:?}");
            assert_eq!(offer_for(&setup, false), Offer::TryAgain);
        }
        let limit = F::UsageLimitReached { message: "m".into(), resets_at: None };
        assert_eq!((offer_for(&limit, false), calls_for_a_check(&limit)), (Offer::TryAgain, false));
        assert!(!calls_for_a_check(&F::TurnLimitReached { limit: 80 }));
        // A turn that had done work is carried on, not asked for again:
        // what it did is done.
        let overloaded = F::Api { kind: trill_assistant::ApiErrorKind::Server, message: "529".into() };
        let died = F::CliExited { exit_code: Some(1), stderr_tail: String::new() };
        for later in [limit, overloaded, died, F::NotLoggedIn] {
            assert_eq!(offer_for(&later, true), Offer::Continue, "{later:?}");
        }
        for worked in [false, true] {
            assert_eq!(offer_for(&F::TurnLimitReached { limit: 80 }, worked), Offer::Continue);
            // Nothing is remembered of the request: it has to be made again.
            assert_eq!(offer_for(&F::SessionNotFound, worked), Offer::TryAgain);
            assert_eq!(offer_for(&F::UnexpectedTools { tools: vec!["Bash".into()] }, worked), Offer::Nothing);
        }
    }

    #[test]
    fn the_plan_is_kept_in_step_with_what_the_back_end_reports() {
        use trill_assistant::PlanStep;
        let added = |id: &str, title: &str, doing: Option<&str>| PlanChange::Added {
            id: id.into(),
            title: title.into(),
            doing: doing.map(str::to_owned),
        };
        let stands =
            |id: &str, status| PlanChange::Changed { id: id.into(), status: Some(status), title: None, doing: None };
        let read = |plan: &Plan| -> Vec<(String, StepState)> {
            plan.steps.iter().map(|step| (step.words().to_owned(), step.state)).collect()
        };
        let step = |words: &str, state| (words.to_owned(), state);

        let mut plan = Plan::default();
        assert!(plan.is_empty() && !plan.is_finished() && plan.current().is_none());
        plan.apply(&added("1", "Set tempo and key", Some("Setting tempo and key")));
        plan.apply(&added("2", "Write the drums", None));
        plan.apply(&added("3", "Render", Some("Rendering")));
        assert_eq!(plan.done(), 0);
        assert_eq!(plan.current().map(Step::words), Some("Set tempo and key"), "nothing in hand: the next to do");

        // In hand, a step reads as what is being done, if that was said.
        plan.apply(&stands("1", StepStatus::InProgress));
        assert_eq!(plan.current().map(Step::words), Some("Setting tempo and key"));
        plan.apply(&stands("1", StepStatus::Done));
        plan.apply(&stands("2", StepStatus::InProgress));
        assert_eq!(
            read(&plan),
            [
                step("Set tempo and key", StepState::Done),
                step("Write the drums", StepState::Doing),
                step("Render", StepState::Pending)
            ]
        );
        assert_eq!((plan.done(), plan.is_finished()), (1, false));

        // Worded anew; written down twice; put back; dropped.
        let reworded =
            PlanChange::Changed { id: "3".into(), status: None, title: Some("Render and check".into()), doing: None };
        plan.apply(&reworded);
        assert_eq!(plan.steps[2].title, "Render and check");
        assert_eq!(plan.steps[2].doing.as_deref(), Some("Rendering"), "what was not said again stays");
        plan.apply(&added("2", "Write the drums and a fill", Some("Writing the drums")));
        assert_eq!((plan.steps.len(), plan.steps[1].state), (3, StepState::Doing), "the same step, still in hand");
        plan.apply(&stands("2", StepStatus::Pending));
        assert_eq!(plan.steps[1].state, StepState::Pending);
        plan.apply(&PlanChange::Removed { id: "2".into() });
        plan.apply(&PlanChange::Removed { id: "no such step".into() });
        assert_eq!(
            read(&plan),
            [step("Set tempo and key", StepState::Done), step("Render and check", StepState::Pending)]
        );

        // A step whose writing down went unseen is shown by what is known.
        plan.apply(&stands("7", StepStatus::InProgress));
        assert_eq!(plan.steps[2].words(), "Step 7");

        // The CLI's own list is the last word on what the plan is; how a
        // step reads while it is done is the panel's to remember.
        let listed = |id: &str, title: &str, status| PlanStep { id: id.into(), title: title.into(), status };
        plan.apply(&PlanChange::Listed(vec![
            listed("1", "Set tempo and key", StepStatus::Done),
            listed("3", "Render and check", StepStatus::InProgress),
            listed("8", "Export", StepStatus::Pending),
        ]));
        assert_eq!(
            read(&plan),
            [
                step("Set tempo and key", StepState::Done),
                step("Rendering", StepState::Doing),
                step("Export", StepState::Pending)
            ]
        );

        // A plan with steps still to do is kept when another request comes;
        // one that was carried out is put away.
        plan.begin_request();
        assert_eq!(plan.steps.len(), 3);
        plan.apply(&stands("3", StepStatus::Done));
        plan.apply(&stands("8", StepStatus::Done));
        assert!(plan.is_finished() && plan.current().is_none());
        plan.begin_request();
        assert!(plan.is_empty());
        plan.begin_request();
        assert!(plan.is_empty() && !plan.is_finished());
    }

    #[test]
    fn a_look_at_the_web_is_a_line_with_the_query_or_the_address() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("what is a tresillo?");
        assert!(!transcript.turn_has_worked());
        let search = WebLookup::Search { query: "tresillo rhythm".into() };
        transcript.apply(&Event::WebStarted { id: "w1".into(), lookup: search }, &project);
        assert!(transcript.turn_has_worked());
        let line = tool(&transcript.items[1]);
        assert_eq!(
            (line.words(), line.state),
            ("Searching the web for \u{201C}tresillo rhythm\u{201D}", ToolState::Running)
        );
        assert_eq!((line.tool.as_str(), line.input.as_str()), ("WebSearch", "tresillo rhythm"));

        let found = "Web search results for query: \"tresillo rhythm\"";
        transcript.apply(&Event::WebFinished { id: "w1".into(), result: Ok(found.into()) }, &project);
        let page =
            WebLookup::Page { url: "https://example.com/tresillo".into(), question: "How is it counted?".into() };
        transcript.apply(&Event::WebStarted { id: "w2".into(), lookup: page }, &project);
        transcript.apply(&Event::WebFinished { id: "w2".into(), result: Err("404".into()) }, &project);
        // An answer nobody asked for is nobody's line.
        transcript.apply(&Event::WebFinished { id: "w9".into(), result: Ok("stray".into()) }, &project);

        assert_eq!(transcript.items.len(), 3);
        let searched = tool(&transcript.items[1]);
        assert_eq!(
            (searched.words(), searched.state),
            ("Searched the web for \u{201C}tresillo rhythm\u{201D}", ToolState::Done)
        );
        assert_eq!(searched.output, found);
        let read = tool(&transcript.items[2]);
        assert_eq!(
            (read.words(), read.state, read.output.as_str()),
            ("Reading example.com/tresillo", ToolState::Failed, "404")
        );
        assert_eq!(
            (read.tool.as_str(), read.input.as_str()),
            ("WebFetch", "https://example.com/tresillo\n\nHow is it counted?")
        );
        assert!(searched.place.is_none() && read.place.is_none(), "the web is nowhere in the song");

        // Kept and read back, they read the same.
        let kept = Transcript::from_json(&transcript.to_json()).expect("it reads back");
        assert_eq!(tool(&kept.items[1]).did, "Searched the web for \u{201C}tresillo rhythm\u{201D}");
    }

    #[test]
    fn a_conversation_being_summarised_is_one_line_that_ends_up_saying_what_happened() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("a long one");
        transcript.apply(&Event::Notice(Notice::Compacting), &project);
        let Some(Item::Note { text, tone, .. }) = transcript.items.last() else { panic!("a note") };
        assert!(text.ends_with("summarising the earlier part\u{2026}") && *tone == Tone::Info, "{text}");

        let done = Notice::Compacted { tokens_before: Some(985_053), tokens_after: Some(644) };
        transcript.apply(&Event::Notice(done.clone()), &project);
        assert_eq!(transcript.items.len(), 2, "in the place of the line that said it was under way");
        let Some(Item::Note { text, .. }) = transcript.items.last() else { panic!("a note") };
        assert_eq!(*text, done.to_string());

        transcript.apply(&Event::Notice(Notice::CompactionFailed { detail: "failed".into() }), &project);
        let Some(Item::Note { tone, .. }) = transcript.items.last() else { panic!("a note") };
        assert_eq!(*tone, Tone::Warning);
    }

    #[test]
    fn the_plan_and_what_a_stopped_turn_took_are_kept_with_the_conversation() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("write a song");
        let added = PlanChange::Added {
            id: "1".into(),
            title: "Write the drums".into(),
            doing: Some("Writing the drums".into()),
        };
        transcript.apply(&Event::Plan(added), &project);
        let begun =
            PlanChange::Changed { id: "1".into(), status: Some(StepStatus::InProgress), title: None, doing: None };
        transcript.apply(&Event::Plan(begun), &project);
        // How far the turn has come is not part of the conversation.
        let progress = trill_assistant::Progress { steps: 3, estimated_cost_usd: None, costed_steps: 0 };
        transcript.apply(&Event::Progress(progress), &project);
        assert_eq!(transcript.items.len(), 1);
        let spent = Spent { seconds: 842.0, steps: 131, estimated_cost: Some(1.25), costed_steps: 100 };
        transcript.end(TurnEnd { spent: Some(spent.clone()), ..TurnEnd::unchanged(Ending::Stopped) });

        let kept = transcript.to_json();
        let read = Transcript::from_json(&kept).expect("it reads back");
        assert_eq!(read, transcript);
        assert_eq!(read.plan.steps.len(), 1);
        assert_eq!((read.plan.steps[0].words(), read.plan.steps[0].state), ("Writing the drums", StepState::Doing));
        let Some(Item::End(end)) = read.items.last() else { panic!("an end") };
        assert_eq!(end.spent, Some(spent));

        // A conversation kept before there was a plan is the items alone,
        // and reads as one without a plan.
        let old = r#"[{"kind":"user","text":"hi"},{"kind":"end","how":"stopped","changed":false}]"#;
        let old = Transcript::from_json(old).expect("an older conversation");
        assert!(old.plan.is_empty() && old.items.len() == 2);
        let Some(Item::End(end)) = old.items.last() else { panic!("an end") };
        assert_eq!(end.spent, None);
    }

    #[test]
    fn a_conversation_is_kept_and_read_back_without_what_only_this_run_knows() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        transcript.push_user("add drums");
        transcript.apply(&started("a", "create_track", json!({ "name": "Drums" })), &project);
        transcript.apply(&finished_call("a", "create_track", Ok("{\"track_id\":3}")), &project);
        transcript.apply(&text("Added."), &project);
        if let Item::Tool(line) = &mut transcript.items[1] {
            line.open = true;
        }
        let summary = TurnSummary {
            turns: 2,
            duration: Duration::from_millis(8400),
            usage: TokenUsage::default(),
            estimated_cost_usd: Some(0.0123),
        };
        transcript.end(TurnEnd { changed: true, ..TurnEnd::unchanged(finished(&summary)) });

        let read = Transcript::from_json(&transcript.to_json()).expect("it reads back");
        assert_eq!(read.items.len(), 4);
        let line = tool(&read.items[1]);
        assert_eq!((line.id.as_str(), line.open), ("", false), "ids and what was unfolded are this run's");
        assert_eq!((line.did.as_str(), line.state), ("Added the track \u{201C}Drums\u{201D}", ToolState::Done));
        let Item::End(end) = &read.items[3] else { panic!("an end") };
        assert!(end.changed && end.step.is_none());
        assert_eq!(end.ending, Ending::Finished { seconds: 8.4, steps: 2, estimated_cost: Some(0.0123) });

        // A conversation the program closed in the middle of is closed.
        let mut cut = Transcript::default();
        cut.push_user("go");
        cut.apply(&started("a", "get_project", json!({})), &project);
        let read = Transcript::from_json(&cut.to_json()).unwrap();
        assert_eq!(tool(&read.items[1]).state, ToolState::Stopped);
        assert!(matches!(read.items.last(), Some(Item::End(TurnEnd { ending: Ending::Stopped, .. }))));

        assert_eq!(Transcript::from_json("{ not json"), None);
        assert_eq!(Transcript::from_json("[]"), Some(Transcript::default()));
    }

    #[test]
    fn a_calls_line_leads_to_what_it_made_for_as_long_as_that_is_the_same_thing() {
        let mut project = Project::new("t");
        let bass = project.add_track("Bass");
        let mut transcript = Transcript::default();
        transcript.push_user("a bass line");
        transcript.apply(&started("a", "get_project", json!({})), &project);
        transcript.apply(&finished_call("a", "get_project", Ok("{}")), &project);
        transcript.apply(&started("b", "create_clip", json!({ "track": bass.0, "name": "Line" })), &project);
        assert_eq!(tool(&transcript.items[2]).place, None, "a call that has not answered leads nowhere yet");
        let line = project.add_midi_clip(bass, "Line", 0.0, 8.0).unwrap();
        // The answer in full is what is read, long as it may be.
        let answer = format!("{{\"padding\":\"{}\",\"clip_id\":{}}}", "x".repeat(MAX_DETAIL), line.0);
        transcript.apply(&finished_call("b", "create_clip", Ok(&answer)), &project);
        transcript.apply(&started("c", "create_clip", json!({ "track": 999 })), &project);
        transcript.apply(&finished_call("c", "create_clip", Err("track 999 was not found")), &project);
        transcript.apply(&started("d", "update_clip", json!({ "clip": 4242, "name": "Gone" })), &project);
        transcript.apply(&finished_call("d", "update_clip", Ok("{}")), &project);

        assert_eq!(tool(&transcript.items[1]).place, None, "reading changes nothing");
        assert!(tool(&transcript.items[2]).place.is_some());
        assert_eq!(tool(&transcript.items[3]).place, None, "a call that failed made nothing");
        // As the turn ends, the lines are settled against the song: one
        // that names what is not there leads nowhere, the others are given
        // the names things go by.
        transcript.settle_places(&project);
        let made = Place::Clips { clips: vec![(line.0, "Line".into())] };
        assert_eq!(tool(&transcript.items[2]).place, Some(made.clone()));
        assert_eq!(tool(&transcript.items[4]).place, None);
        assert_eq!(transcript.turn_places(transcript.items.len()).collect::<Vec<_>>(), [&made]);
        let track = Place::Track { id: bass.0, name: "Bass".into() };
        transcript.end(TurnEnd { changed: true, places: vec![track.clone()], ..TurnEnd::unchanged(Ending::Stopped) });
        let end = transcript.items.len() - 1;
        assert_eq!(
            transcript.turn_places(end).collect::<Vec<_>>(),
            [&made, &track],
            "the lines, then the closing line"
        );

        // The next turn's places are its own.
        transcript.push_user("and louder");
        transcript.apply(&started("e", "update_track", json!({ "track": bass.0, "volume_db": 2 })), &project);
        transcript.apply(&finished_call("e", "update_track", Ok("{}")), &project);
        assert_eq!(transcript.turn_places(transcript.items.len()).count(), 1);
        assert_eq!(transcript.turn_places(end).count(), 2);

        // Kept and read back, they lead where they led, as long as what
        // they name is still that: a clip that goes by another name now
        // may be another clip.
        let mut read = Transcript::from_json(&transcript.to_json()).expect("it reads back");
        assert_eq!(tool(&read.items[2]).place, Some(made.clone()));
        assert_ne!(read.epoch, transcript.epoch, "and it is a conversation of its own to whoever keeps things by it");
        read.keep_places_in(&project);
        assert_eq!(read.turn_places(end).count(), 2);
        project.clip_mut(line).unwrap().name = "Another".into();
        read.keep_places_in(&project);
        assert_eq!(tool(&read.items[2]).place, None);
        assert_eq!(read.turn_places(end).collect::<Vec<_>>(), [&track]);
        // Another project in place of this one: nothing leads anywhere.
        let loaded = finished_call("f", "load_project", Ok("{}"));
        assert!(transcript.opens_another_project(&loaded) && !transcript.opens_another_project(&text("load_project")));
        assert!(!transcript.opens_another_project(&finished_call("f", "load_project", Err("refused"))));
        assert!(!transcript.opens_another_project(&finished_call("f", "save_project", Ok("{}"))));
        transcript.apply(&started("g", "recover_project", json!({ "action": "list" })), &project);
        assert!(!transcript.opens_another_project(&finished_call("g", "recover_project", Ok("{}"))));
        transcript.apply(&started("h", "recover_project", json!({ "action": "open", "index": 0 })), &project);
        assert!(transcript.opens_another_project(&finished_call("h", "recover_project", Ok("{}"))));
        transcript.forget_places();
        assert_eq!(transcript.turn_places(end).count() + transcript.turn_places(transcript.items.len()).count(), 0);

        // A conversation kept before lines led anywhere reads as before.
        let old = r#"[{"kind":"user","text":"hi"},{"kind":"tool","tool":"create_clip","doing":"a","did":"b",
            "state":"done","input":"{}","output":"{}"},{"kind":"end","how":"stopped","changed":true}]"#;
        let old = Transcript::from_json(old).expect("an older conversation");
        assert_eq!(tool(&old.items[1]).place, None);
        assert_eq!(old.turn_places(2).count(), 0);
    }

    #[test]
    fn long_answers_are_cut_and_old_exchanges_let_go() {
        let project = Project::new("t");
        let mut transcript = Transcript::default();
        let long = "\u{00E9}".repeat(MAX_DETAIL + 250);
        transcript.push_user("first");
        transcript.apply(&finished_call("a", "get_project", Ok(&long)), &project);
        let line = tool(&transcript.items[1]);
        assert!(line.output.ends_with("(250 more characters)"), "{}", &line.output[line.output.len() - 40..]);
        assert_eq!(line.output.chars().filter(|c| *c == '\u{00E9}').count(), MAX_DETAIL);

        for index in 0..MAX_ITEMS {
            transcript.push_user(&format!("message {index}"));
            transcript.apply(&text("ok"), &project);
        }
        assert!(transcript.items.len() <= MAX_ITEMS);
        assert_ne!(transcript.epoch, Transcript::default().epoch);
        let before = transcript.epoch;
        transcript.push_user("one more");
        transcript.apply(&text("ok"), &project);
        assert_ne!(
            transcript.epoch, before,
            "letting go of the oldest moves every item: what was kept by place is void"
        );
        assert!(matches!(&transcript.items[0], Item::User { .. }), "it still begins with a message");
        assert_eq!(transcript.last_message(), Some("one more"));
    }

    #[test]
    fn durations_and_estimates_read_plainly() {
        assert_eq!(format_seconds(0.4), "0 s");
        assert_eq!(format_seconds(8.4), "8 s");
        assert_eq!(format_seconds(72.9), "1 min 12 s");
        assert_eq!(format_estimate(0.0031), "est. under $0.01");
        assert_eq!(format_estimate(0.1234), "est. $0.12");
        assert_eq!(format_estimate(2.5), "est. $2.50");
        assert_eq!(format_seconds(1864.0), "31 min 4 s");
        assert_eq!(
            (format_count(7), format_count(1000), format_count(12_345)),
            ("7".into(), "1,000".into(), "12,345".into())
        );
        assert_eq!(
            (format_steps(1), format_steps(320), format_steps(2000)),
            ("1 step".into(), "320 steps".into(), "2,000 steps".into())
        );
        // An estimate that is behind says how far it reaches.
        assert_eq!(format_estimate_so_far(1.25, 100, 150), "est. $1.25 to step 100");
        assert_eq!(format_estimate_so_far(2.5, 200, 200), "est. $2.50");
    }
}
