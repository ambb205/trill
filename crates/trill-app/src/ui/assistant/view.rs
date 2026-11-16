//! The panel itself: a header, the conversation, and at the foot the
//! message field with the model that will answer, or in its place the
//! reason the assistant cannot be used.
//!
//! It is laid out like the rest of the window, not like a messenger: the
//! user's messages are section strips, the assistant's reply is running
//! text set from its Markdown, each tool call is a row with a status mark
//! that leads to what the call changed and unfolds to its arguments and its
//! answer, and a turn closes with a line that says what it cost in time and
//! whether it changed the song, with the way to what it changed and the
//! Undo for it.
//!
//! What is said in the conversation is text like any other: it can be
//! selected with the mouse, across passages, and copied.

use super::transcript::{self, Ending, Item, Offer, Tone, ToolLine, ToolState, TurnEnd};
use super::{Assistant, Billing, Problem, Readiness, Reversal, context, markdown, places, reply};
use crate::ui::shortcuts::{self, Command};
use crate::ui::state::{Cx, FocusArea, UiState};
use crate::ui::widgets;
use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Id, Key, KeyboardShortcut, Layout, Modifiers, Rangef, Rect, Response, Role,
    Sense, Shape, Stroke, Ui, WidgetInfo, pos2, vec2,
};
use trill_assistant::{MODEL_ALIASES, Unavailable};
use trill_core::Project;
use trill_noir::prose;
use trill_noir::{Chip, Icon, MenuItem, TextStyle, Theme, Variant, paint, text};

/// Height of the header, as the browser's row of tabs.
const HEADER: f32 = 24.0;
/// Space between the panel's edges and what is in it.
const EDGE: f32 = 12.0;
/// Height of a tool call's row.
const ROW: f32 = 22.0;
/// Where a row's words begin, after its status mark.
const INDENT: f32 = EDGE + 13.0;
/// Width of the part of a tool call's row that unfolds it, at its right.
const UNFOLD: f32 = 28.0;
/// The message field shows between this many lines and scrolls beyond.
const FIELD_ROWS: std::ops::RangeInclusive<usize> = 2..=8;
/// What the chooser calls leaving the model to the CLI.
const DEFAULT_MODEL: &str = "Default";

/// What a click in the conversation asks for, carried out once the
/// conversation is no longer being read.
enum Action {
    Unfold(usize),
    /// Go to what the item at this place in the conversation changed.
    Go(usize),
    Reverse(Reversal),
    Send(String),
    Draft(String),
    Copy(String),
}

/// Adds running text to the column, `left` and `right` in from its edges,
/// and says what it reads to assistive technology. The text can be selected
/// and copied; `id` is its own.
#[expect(clippy::too_many_arguments, reason = "a placement call: what, in which style, and where")]
fn paragraph(
    ui: &mut Ui,
    theme: &Theme,
    id: Id,
    style: TextStyle,
    words: &str,
    color: Color32,
    left: f32,
    right: f32,
) -> Response {
    let galley = prose::plain(ui, theme, style, color, words, ui.available_width() - left - right);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), galley.size().y), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    if ui.is_rect_visible(rect) {
        prose::selectable(ui, id, pos2(rect.left() + left, rect.top()), galley);
    }
    response
}

/// A caps or micro label on a line of its own.
fn label(ui: &mut Ui, style: TextStyle, words: &str, color: Color32, left: f32, height: f32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let room = rect.with_min_x(rect.left() + left).with_max_x(rect.right() - EDGE);
    text::paint(ui.painter(), room, Align2::LEFT_CENTER, style, words, color);
    rect
}

/// Shows `words` in the status bar while the pointer is over `response`.
fn hint(state: &mut UiState, response: &Response, words: impl Into<String>) {
    if response.hovered() {
        state.hint.set(words);
    }
}

/// A chip that does something when clicked, placed with its right edge at
/// `right`. Returns the response and the x where it begins.
fn action_chip(ui: &Ui, theme: &Theme, right: f32, row: Rect, id: Id, chip: &Chip) -> (Response, f32) {
    let width = chip.width(ui, theme);
    let rect = Rect::from_min_size(
        pos2(right - width, row.center().y - theme.metric.chip / 2.0),
        vec2(width, theme.metric.chip),
    );
    (trill_noir::chip(ui, theme, rect, id, chip), rect.left())
}

// ---- the toggle in the top bar ------------------------------------------------

/// The button in the top bar that shows the panel or puts it away. A dot
/// on it says a turn is under way while the panel is closed.
pub fn toggle_button(ui: &Ui, cx: &mut Cx, rect: Rect) -> Response {
    let theme = cx.theme;
    let on = cx.assistant.is_open();
    let response = ui.interact(rect, Id::unique("assistant-toggle"), Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, "Assistant"));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if on || response.is_pointer_button_down_on() {
            painter.rect_filled(rect, 0.0, theme.selected);
        }
        if response.hovered() {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        paint::frame(painter, rect, theme.hairline);
        let color = if on || response.hovered() { theme.ink } else { theme.ink_dim };
        // A speech balloon with two lines of writing in it.
        let center = pos2(rect.center().x.round(), rect.center().y.round());
        let balloon = Rect::from_min_size(center - vec2(6.0, 6.0), vec2(13.0, 9.0));
        paint::frame(painter, balloon, color);
        let tail = vec![center + vec2(-3.0, 3.0), center + vec2(-3.0, 6.0), center + vec2(0.0, 3.0)];
        painter.add(Shape::convex_polygon(tail, color, Stroke::NONE));
        paint::hline(painter, Rangef::new(center.x - 3.0, center.x + 4.0), center.y - 4.0, color);
        paint::hline(painter, Rangef::new(center.x - 3.0, center.x + 2.0), center.y - 2.0, color);
        if cx.assistant.is_working() && !on {
            paint::dot(painter, pos2(rect.right() - 7.0, rect.top() + 5.0), 4.0, theme.info);
        }
    }
    let keys = shortcuts::keys(Command::ToggleAssistant);
    cx.hint(&response, format!("Assistant: talk to an AI about this song \u{00B7} {keys}"));
    if response.clicked() {
        cx.assistant.toggle();
    }
    response
}

// ---- the panel ----------------------------------------------------------------

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let area = ui.max_rect();
    // Everything below is placed by hand, which allocates nothing; a
    // resizable panel shrinks to what its content claims, so claim it all.
    ui.expand_to_include_rect(area);
    cx.assistant.width = area.width();
    cx.assistant.area = area;
    // Said again by whichever of the panel's fields has the keyboard.
    cx.assistant.typing_here = false;
    let painter = ui.painter().clone();
    painter.rect_filled(area, 0.0, theme.panel);
    paint::vline(&painter, area.left(), area.y_range(), theme.hairline);
    let area = area.with_min_x(area.left() + paint::hair(&painter));

    let header_rect = Rect::from_min_size(area.min, vec2(area.width(), HEADER));
    header(ui, cx, header_rect);
    let mut top = header_rect.bottom();
    if let Some(billing) = cx.assistant.billing.clone() {
        top = billing_strip(ui, theme, &billing, Rect::from_min_max(pos2(area.left(), top), area.max));
    }

    let foot_height = match cx.assistant.readiness {
        Readiness::Ready(_) => composer_height(ui, cx, area.width()),
        _ => card_height(ui, cx, area.width()),
    };
    let foot = Rect::from_min_max(pos2(area.left(), (area.bottom() - foot_height).max(top)), area.max);
    let body = Rect::from_min_max(pos2(area.left(), top), pos2(area.right(), foot.top()));
    painter.rect_filled(body, 0.0, theme.bg);
    conversation(ui, cx, body);
    paint::hline(&painter, foot.x_range(), foot.top(), theme.hairline);
    match cx.assistant.readiness {
        Readiness::Ready(_) => composer(ui, cx, foot),
        _ => card(ui, cx, foot),
    }
}

/// The name of the panel, how it stands, and at the right a way to start
/// over and a way to put the panel away.
fn header(ui: &mut Ui, cx: &mut Cx, bar: Rect) {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    paint::hline_above(&painter, bar.x_range(), bar.bottom(), theme.hairline);
    let close_rect = Rect::from_min_size(pos2(bar.right() - HEADER, bar.top()), vec2(HEADER, HEADER - 1.0));
    let fresh_chip = Chip::action("New").named("New conversation");
    let fresh_width = fresh_chip.width(ui, theme);
    let fresh_rect = Rect::from_min_size(
        pos2(close_rect.left() - 2.0 - fresh_width, bar.center().y - theme.metric.chip / 2.0),
        vec2(fresh_width, theme.metric.chip),
    );
    let row = bar.with_min_x(bar.left() + EDGE);
    let mut x = row.left() + text::paint(&painter, row, Align2::LEFT_CENTER, theme.text.caps, "Assistant", theme.ink);

    let assistant = &*cx.assistant;
    let (color, word) = match (&assistant.readiness, assistant.turn.is_some(), assistant.is_stopping()) {
        (Readiness::Unknown | Readiness::Checking, ..) => (theme.ink_faint, "Checking\u{2026}"),
        (Readiness::Unavailable(_), ..) => (theme.warn, "Not available"),
        (Readiness::Ready(_), true, true) => (theme.info, "Stopping\u{2026}"),
        (Readiness::Ready(_), true, false) => (theme.info, "Working\u{2026}"),
        (Readiness::Ready(_), false, _) => (theme.ok, "Ready"),
    };
    x = paint::dot(&painter, pos2(x + 10.0, bar.center().y), theme.metric.dot, color) + 5.0;
    let status = Rect::from_min_max(pos2(x, bar.top()), pos2(fresh_rect.left() - 8.0, bar.bottom()));
    let used = text::paint(&painter, status, Align2::LEFT_CENTER, theme.text.micro, word, theme.ink_faint);
    let about = match &assistant.readiness {
        Readiness::Ready(ready) => {
            let model = assistant.model.as_ref().map(|model| format!(", answering as {model}")).unwrap_or_default();
            format!(
                "Uses your installed Claude Code {} and its sign-in{model}; messages count against your usage there",
                ready.cli.installation.version
            )
        }
        _ => "The assistant works through your own installed Claude Code".to_owned(),
    };
    let sensed = Rect::from_min_max(pos2(bar.left(), bar.top()), pos2(x + used, bar.bottom()));
    let response = ui.interact(sensed, ui.scope_id().with("assistant-status"), Sense::hover());
    cx.hint(&response, about);

    let id = ui.scope_id().with("assistant-header");
    let close =
        trill_noir::bare_icon_button(ui, theme, close_rect, id.with("close"), Icon::Close, "Close the assistant");
    cx.hint(&close, format!("Put the assistant away \u{00B7} {}", shortcuts::keys(Command::ToggleAssistant)));
    if close.clicked() {
        cx.assistant.toggle();
    }
    let fresh = trill_noir::chip(ui, theme, fresh_rect, id.with("new"), &fresh_chip);
    cx.hint(&fresh, "New conversation: the assistant starts over and remembers nothing of this one");
    if fresh.clicked() {
        if cx.assistant.is_working() {
            cx.state.toasts.info("Stop the assistant first, or wait for it to finish");
        } else {
            cx.assistant.new_conversation();
        }
    }
}

/// Who is billed, said where it cannot be missed. Returns the y where the
/// strip ends.
fn billing_strip(ui: &mut Ui, theme: &Theme, billing: &Billing, room: Rect) -> f32 {
    let words = billing.to_string();
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let galley = prose::plain(ui, theme, style, theme.ink, &words, room.width() - INDENT - EDGE);
    let strip = Rect::from_min_size(room.min, vec2(room.width(), galley.size().y + 14.0));
    let painter = ui.painter();
    painter.rect_filled(strip, 0.0, theme.raised);
    paint::hline_above(painter, strip.x_range(), strip.bottom(), theme.hairline);
    paint::dot(painter, pos2(strip.left() + EDGE, strip.top() + 7.0 + style.size * 0.7), theme.metric.dot, theme.warn);
    let id = ui.scope_id().with("assistant-billing");
    let response = ui.interact(strip, id, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &words));
    prose::selectable(ui, id.with("words"), pos2(strip.left() + INDENT, strip.top() + 7.0), galley);
    strip.bottom()
}

// ---- the conversation ---------------------------------------------------------

fn conversation(ui: &mut Ui, cx: &mut Cx, body: Rect) {
    let mut actions = Vec::new();
    // Which rows lead somewhere, as the song is now.
    let leads = cx.assistant.leads(&cx.snap.project, cx.snap.revision).to_vec();
    let leads = |index: usize| leads.get(index).copied().unwrap_or(false);
    // How the replies were set, taken in hand while the conversation is
    // read: what has not changed since is not set again.
    let mut replies = std::mem::take(&mut cx.assistant.replies);
    replies.begin(cx.assistant.transcript.epoch, cx.assistant.transcript.items.len());
    widgets::place(ui, body, "assistant-conversation", Layout::top_down(Align::Min), |ui| {
        ui.set_clip_rect(body.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let Cx { assistant, state, theme, snap, .. } = cx;
            if assistant.transcript.is_empty() {
                let ready = matches!(assistant.readiness, Readiness::Ready(_));
                welcome(ui, theme, state, &snap.project, ready, &mut actions);
            }
            let items = &assistant.transcript.items;
            // The reply that may still grow: the last thing said in a turn
            // that is under way.
            let writing = assistant.turn.is_some().then(|| items.len().saturating_sub(1));
            for (index, item) in items.iter().enumerate() {
                match item {
                    Item::User { text } => said(ui, theme, index, text, index > 0, &mut actions),
                    Item::Reply { text } => {
                        reply(ui, theme, &mut replies, index, text, writing == Some(index), &mut actions);
                    }
                    Item::Tool(line) => tool(ui, theme, state, index, line, leads(index), &mut actions),
                    Item::Note { text, tone, .. } => note(ui, theme, index, text, *tone),
                    Item::End(end) => ended(ui, theme, state, assistant, index, end, leads(index), &mut actions),
                }
            }
            if let Some(seconds) = assistant.working_for() {
                working(ui, theme, assistant.is_stopping(), seconds);
            }
            ui.add_space(10.0);
            if std::mem::take(&mut assistant.reveal_end) {
                ui.scroll_to_cursor(Some(Align::BOTTOM));
            }
        });
    });
    cx.assistant.replies = replies;

    for action in actions {
        match action {
            Action::Unfold(index) => {
                if let Some(Item::Tool(line)) = cx.assistant.transcript.items.get_mut(index) {
                    line.open = !line.open;
                }
            }
            Action::Go(index) => go(cx, index),
            Action::Reverse(reversal) => cx.assistant.reverse(reversal),
            Action::Send(message) => drop(send(cx, &message)),
            Action::Draft(message) => {
                cx.assistant.draft = message;
                cx.assistant.focus_composer = true;
            }
            Action::Copy(words) => ui.ctx().copy_text(words),
        }
    }
}

/// Goes to what the item at `index` of the conversation changed: selects
/// it and brings it into view. The user asked, so it happens whatever else
/// is going on, and the arrangement has the keyboard afterwards.
fn go(cx: &mut Cx, index: usize) {
    let route = places::Route::to(cx.assistant.places_of(index), &cx.snap.project);
    match route {
        Some(route) => {
            route.take(cx.state);
            cx.state.focus = FocusArea::Arrangement;
        }
        None => cx.state.toasts.info("That is no longer in the song"),
    }
}

/// Sends `message` with what the user is looking at.
fn send(cx: &mut Cx, message: &str) -> bool {
    let context = context::describe(&cx.snap.project, cx.state, cx.live);
    let sent = cx.assistant.submit(message, &context);
    if sent {
        cx.assistant.reveal_end = true;
    }
    sent
}

/// A few things to ask, for someone who has not asked before, worded for a
/// musician and chosen by what is open: a song still to be begun, a clip
/// that is selected, or a song with parts in it.
fn starting_points(project: &Project, state: &UiState) -> Vec<&'static str> {
    if project.tracks.is_empty() {
        return vec![
            "Start a laid-back beat at 90 BPM: drums, a bass line and a few chords.",
            "Set up tracks for drums, bass, keys and a lead, with sounds that go together.",
            "What can you do in a song?",
        ];
    }
    let clip = state.selected_clip.and_then(|id| project.clip(id).ok());
    let mut points = vec!["What is in this song, and what key is it in?"];
    match clip {
        Some(clip) if clip.midi().is_some() => {
            points.push("Make the selected clip busier.");
            points.push("Write a bass line that follows the selected clip.");
        }
        Some(_) => points.push("Chop the selected clip into a new rhythm over two bars."),
        None => {}
    }
    points.push("Add a drum part to the loop that fits what is there.");
    if clip.is_none() {
        points.push("Give the song an intro that brings the parts in one at a time.");
    }
    points
}

/// What the panel says before anything has been said: what the assistant
/// is, what it runs on, and, once it can be talked to, a few things to ask.
fn welcome(ui: &mut Ui, theme: &Theme, state: &mut UiState, project: &Project, ready: bool, actions: &mut Vec<Action>) {
    ui.add_space(14.0);
    label(ui, theme.text.caps, "Ask about this song", theme.ink, EDGE, 16.0);
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let said = [
        "The assistant works on the open project with Trill\u{2019}s own tools. It is told what you have selected, \
         where the playhead and the loop are and which tab is open, so \u{201C}this\u{201D} means what you are looking \
         at.",
        "Everything it changes while answering one message is undone with one Undo, and when it has finished you \
         are shown what it changed.",
        "It runs the Claude Code installed on this computer, signed in as you are there. Messages count against \
         your usage there.",
    ];
    for (index, words) in said.into_iter().enumerate() {
        ui.add_space(6.0);
        let id = ui.scope_id().with(("welcome", index));
        paragraph(ui, theme, id, style, words, theme.ink_dim, EDGE, EDGE);
    }
    if !ready {
        return;
    }
    ui.add_space(14.0);
    label(ui, theme.text.micro, "For example", theme.ink_faint, EDGE, 16.0);
    let top = ui.cursor().top();
    paint::hline(ui.painter(), ui.max_rect().x_range(), top, theme.hairline);
    for example in starting_points(project, state) {
        // As tall as its words take: in a narrow panel a request runs on
        // to a second line.
        let galley = prose::plain(ui, theme, style, theme.ink_dim, example, ui.available_width() - 2.0 * EDGE);
        let height = (galley.size().y + 9.0).max(ROW + 4.0);
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, example));
        if ui.is_rect_visible(rect) {
            if response.hovered() {
                ui.painter().rect_filled(rect, 0.0, theme.hover);
            }
            let color = if response.hovered() { theme.ink } else { theme.ink_dim };
            let at = pos2(rect.left() + EDGE, rect.center().y - galley.size().y / 2.0);
            ui.painter().galley_with_override_text_color(at, galley, color);
            paint::hline_above(ui.painter(), rect.x_range(), rect.bottom(), theme.hairline);
        }
        hint(state, &response, "Click to put this in the message field, to change or to send");
        if response.clicked() {
            actions.push(Action::Draft(example.to_owned()));
        }
    }
}

/// A message of the user's: a strip across the panel, like a section head.
fn said(ui: &mut Ui, theme: &Theme, index: usize, words: &str, ruled_above: bool, actions: &mut Vec<Action>) {
    let galley = prose::plain(ui, theme, theme.text.body, theme.ink, words, ui.available_width() - 2.0 * EDGE);
    let height = 8.0 + 14.0 + 3.0 + galley.size().y + 9.0;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("You: {words}")));
    let mut menu = |ui: &mut Ui| {
        if MenuItem::new("Copy message").show(ui, theme).clicked() {
            actions.push(Action::Copy(words.to_owned()));
        }
        if MenuItem::new("Put back in the message field").show(ui, theme).clicked() {
            actions.push(Action::Draft(words.to_owned()));
        }
    };
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, theme.panel);
        if ruled_above {
            paint::hline(painter, rect.x_range(), rect.top(), theme.hairline);
        }
        paint::hline_above(painter, rect.x_range(), rect.bottom(), theme.hairline);
        let head =
            Rect::from_min_size(pos2(rect.left() + EDGE, rect.top() + 8.0), vec2(rect.width() - 2.0 * EDGE, 14.0));
        text::paint(painter, head, Align2::LEFT_CENTER, theme.text.micro, "You", theme.ink_faint);
        let id = ui.scope_id().with(("said", index));
        prose::selectable(ui, id, pos2(rect.left() + EDGE, head.bottom() + 3.0), galley).context_menu(&mut menu);
    }
    response.context_menu(&mut menu);
}

/// A passage of the assistant's reply, set from its Markdown. `open` says
/// it is still being written.
fn reply(
    ui: &mut Ui,
    theme: &Theme,
    replies: &mut reply::Replies,
    index: usize,
    words: &str,
    open: bool,
    actions: &mut Vec<Action>,
) {
    ui.add_space(9.0);
    let width = ui.available_width();
    let laid = replies.laid(ui, theme, index, words, open, width, EDGE);
    let (rect, _) = ui.allocate_exact_size(vec2(width, laid.height), Sense::hover());
    let id = ui.scope_id().with(("reply", index));
    for response in reply::paint(ui, theme, laid, rect.min, width, EDGE, id) {
        response.context_menu(|ui| {
            if MenuItem::new("Copy reply").show(ui, theme).clicked() {
                // As it reads, without the marks that set it.
                actions.push(Action::Copy(markdown::plain_text(&markdown::parse(words, false))));
            }
        });
    }
    ui.add_space(5.0);
}

/// A tool call: a row that says what it does, with a mark for how it
/// stands, and under it, unfolded, what went in and what came back. A row
/// whose call made or changed something that is still in the song leads
/// there (`leads`): a click on it goes, and the caret at its right unfolds.
fn tool(
    ui: &mut Ui,
    theme: &Theme,
    state: &mut UiState,
    index: usize,
    line: &ToolLine,
    leads: bool,
    actions: &mut Vec<Action>,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    let words = line.words();
    let failed = line.state == ToolState::Failed;
    let id = ui.scope_id().with(("tool", index));
    let caret_rect = rect.with_min_x(rect.right() - UNFOLD);
    let response = ui.interact(if leads { rect.with_max_x(caret_rect.left()) } else { rect }, id, Sense::click());
    response.widget_info(|| {
        let name = match line.state {
            ToolState::Running => format!("{words}\u{2026}"),
            ToolState::Failed => format!("{words}: failed"),
            ToolState::Stopped => format!("{words}: not finished"),
            ToolState::Done => words.to_owned(),
        };
        WidgetInfo::selected(Role::Button, true, line.open, name)
    });
    let unfold = leads.then(|| {
        let unfold = ui.interact(caret_rect, id.with("unfold"), Sense::click());
        unfold.widget_info(|| WidgetInfo::selected(Role::Button, true, line.open, format!("Details of: {words}")));
        unfold
    });
    let over_caret = unfold.as_ref().is_some_and(Response::hovered);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(response.rect, 0.0, theme.hover);
        }
        if over_caret {
            painter.rect_filled(caret_rect, 0.0, theme.hover);
        }
        let mark = pos2(rect.left() + EDGE, rect.center().y);
        match line.state {
            ToolState::Running => drop(paint::hollow_dot(painter, mark, theme.metric.dot, theme.ink)),
            ToolState::Done => drop(paint::dot(painter, mark, theme.metric.dot, theme.ghost)),
            ToolState::Failed => drop(paint::dot(painter, mark, theme.metric.dot, theme.signal)),
            ToolState::Stopped => drop(paint::hollow_dot(painter, mark, theme.metric.dot, theme.ink_faint)),
        }
        let caret = pos2(rect.right() - EDGE - 3.0, rect.center().y);
        let quiet = if response.hovered() { theme.ink_dim } else { theme.ink_faint };
        let caret_color = if over_caret { theme.ink } else { quiet };
        trill_noir::paint_icon(painter, caret, if line.open { Icon::CaretDown } else { Icon::CaretRight }, caret_color);
        let mut room = Rect::from_min_max(pos2(rect.left() + INDENT, rect.top()), pos2(caret.x - 10.0, rect.bottom()));
        if leads {
            // The arrow that says the row goes somewhere.
            let arrow = pos2(caret_rect.left() - 8.0, rect.center().y);
            trill_noir::paint_icon(
                painter,
                arrow,
                Icon::GoTo,
                if response.hovered() { theme.ink } else { theme.ink_faint },
            );
            room.max.x = arrow.x - 11.0;
        }
        let word = match line.state {
            ToolState::Failed => Some(("Failed", theme.signal)),
            ToolState::Stopped => Some(("Not finished", quiet)),
            ToolState::Running | ToolState::Done => None,
        };
        if let Some((word, color)) = word {
            room.max.x -= text::paint(painter, room, Align2::RIGHT_CENTER, theme.text.micro, word, color) + 8.0;
        }
        let style = TextStyle { size: 12.0, ..theme.text.body };
        let color = match (line.state, response.hovered()) {
            (ToolState::Running, _) | (_, true) => theme.ink,
            _ => theme.ink_dim,
        };
        let shown = if line.state == ToolState::Running { format!("{words}\u{2026}") } else { words.to_owned() };
        text::paint(painter, room, Align2::LEFT_CENTER, style, &shown, color);
    }
    let about = match (leads, failed) {
        (true, _) => "Show this in the song: select it and bring it into view \u{00B7} the caret unfolds the call",
        (false, true) => {
            "The tool reported an error, which the assistant sees too \u{00B7} click for what was sent and answered"
        }
        (false, false) => "Click for what was sent to the tool and what it answered",
    };
    hint(state, &response, about);
    if let Some(unfold) = &unfold {
        hint(state, unfold, "What was sent to the tool and what it answered");
    }
    if response.clicked() {
        actions.push(if leads { Action::Go(index) } else { Action::Unfold(index) });
    }
    if unfold.is_some_and(|unfold| unfold.clicked()) {
        actions.push(Action::Unfold(index));
    }
    if !line.open {
        return;
    }

    // Unfolded: the tool by its own name, what it was given, what it said.
    // Both are text to take along: an id to look up, an error to pass on.
    let style = TextStyle { size: theme.text.value.size, ..theme.text.body };
    let width = ui.available_width() - INDENT - EDGE - 16.0;
    let set = |words: &str| (!words.is_empty()).then(|| prose::code_block(ui, style, theme.ink_dim, words, width));
    let (given, output) = (set(&line.input), set(&line.output));
    let part = |galley: &Option<std::sync::Arc<egui::Galley>>| {
        galley.as_ref().map_or(0.0, |galley| 16.0 + galley.size().y + 6.0)
    };
    let height = 6.0 + 16.0 + part(&given) + part(&output) + 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height + 6.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let well =
        Rect::from_min_max(pos2(rect.left() + INDENT, rect.top()), pos2(rect.right() - EDGE, rect.top() + height));
    painter.rect_filled(well, 0.0, theme.panel);
    paint::frame(painter, well, theme.hairline);
    let inner = well.shrink2(vec2(8.0, 0.0));
    let mut y = well.top() + 6.0;
    let head = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 16.0));
    text::paint(painter, head, Align2::LEFT_CENTER, theme.text.value, &line.tool, theme.ink);
    y = head.bottom();
    let answered = if failed { "Error" } else { "Answer" };
    for (title, galley) in [("Sent", given), (answered, output)] {
        let Some(galley) = galley else { continue };
        let head = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 16.0));
        text::paint(ui.painter(), head, Align2::LEFT_CENTER, theme.text.micro, title, theme.ink_faint);
        y = head.bottom() + galley.size().y + 6.0;
        prose::selectable(ui, id.with(title), pos2(inner.left(), head.bottom()), galley);
    }
}

/// Something the back end or the panel has to say.
fn note(ui: &mut Ui, theme: &Theme, index: usize, words: &str, tone: Tone) {
    ui.add_space(6.0);
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let top = ui.cursor().top();
    let id = ui.scope_id().with(("note", index));
    let response = paragraph(ui, theme, id, style, words, theme.ink_dim, INDENT, EDGE);
    let color = match tone {
        Tone::Info => theme.info,
        Tone::Warning => theme.warn,
    };
    paint::dot(ui.painter(), pos2(response.rect.left() + EDGE, top + style.size * 0.7), theme.metric.dot, color);
    ui.add_space(6.0);
}

/// The line that closes a turn: how it ended, what it took, and whether it
/// changed the song, with the way to what it changed (`leads`) and the Undo
/// for it.
#[expect(clippy::too_many_arguments, reason = "the turn's end and everything it is drawn from")]
fn ended(
    ui: &mut Ui,
    theme: &Theme,
    state: &mut UiState,
    assistant: &Assistant,
    index: usize,
    end: &TurnEnd,
    leads: bool,
    actions: &mut Vec<Action>,
) {
    let id = ui.scope_id().with(("turn-end", index));
    if let Ending::Failed { text: reason, .. } = &end.ending {
        ui.add_space(8.0);
        let top = ui.cursor().top();
        let response = paragraph(ui, theme, id.with("reason"), theme.text.body, reason, theme.ink, INDENT, EDGE);
        let mark = pos2(response.rect.left() + EDGE, top + theme.text.body.size * 0.7);
        paint::dot(ui.painter(), mark, theme.metric.dot, theme.signal);
        ui.add_space(4.0);
    } else {
        ui.add_space(4.0);
    }

    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let painter = ui.painter().clone();
    paint::hline(&painter, Rangef::new(row.left() + EDGE, row.right() - EDGE), row.top(), theme.hairline);
    let mut right = row.right() - EDGE;

    // What can be done: the same message again, a word to carry on, taking
    // back what the turn changed, and going to see it.
    if let Ending::Failed { offer, .. } = &end.ending
        && let Some(message) = assistant.offered(*offer)
        && assistant.turn.is_none()
        && matches!(assistant.readiness, Readiness::Ready(_))
    {
        let (words, about) = match offer {
            Offer::Continue => ("Continue", "Ask the assistant to carry on from where it stopped"),
            _ => ("Try again", "Send the same message again"),
        };
        let (response, left) = action_chip(ui, theme, right, row, id.with("offer"), &Chip::action(words));
        hint(state, &response, about);
        if response.clicked() {
            actions.push(Action::Send(message));
        }
        right = left - 6.0;
    }
    let reversal = end.step.map(|step| assistant.reversal(step));
    match reversal {
        Some(reversal @ Reversal::Undo(_)) => {
            let (response, left) = action_chip(ui, theme, right, row, id.with("undo"), &Chip::action("Undo"));
            hint(state, &response, "Take back everything the assistant changed while answering this message");
            if response.clicked() {
                actions.push(Action::Reverse(reversal));
            }
            right = left - 6.0;
        }
        Some(reversal @ Reversal::Redo(_)) => {
            let (response, left) = action_chip(ui, theme, right, row, id.with("redo"), &Chip::action("Redo"));
            hint(state, &response, "Bring back what the assistant changed while answering this message");
            if response.clicked() {
                actions.push(Action::Reverse(reversal));
            }
            right = left - 6.0;
        }
        Some(Reversal::Buried) | None => {}
    }
    // Not for a turn that was taken back: what it changed is as it was.
    if leads && !matches!(reversal, Some(Reversal::Redo(_))) {
        let chip = Chip::action("Show").named("Show what changed");
        let (response, left) = action_chip(ui, theme, right, row, id.with("show"), &chip);
        hint(state, &response, "Select what the assistant changed while answering this message and bring it into view");
        if response.clicked() {
            actions.push(Action::Go(index));
        }
        right = left - 8.0;
    }

    // What happened, from the left.
    let mut parts: Vec<String> = Vec::new();
    match (&end.ending, end.changed, reversal) {
        (_, true, Some(Reversal::Redo(_))) => parts.push("Undone".to_owned()),
        (_, true, _) => parts.push("Changed the song".to_owned()),
        (Ending::Stopped, false, _) => {}
        (Ending::Failed { .. }, false, _) => {}
        (Ending::Finished { .. }, false, _) => {}
    }
    match &end.ending {
        Ending::Finished { seconds, estimated_cost, .. } => {
            parts.push(transcript::format_seconds(*seconds));
            if let Some(dollars) = estimated_cost {
                parts.push(transcript::format_estimate(*dollars));
            }
        }
        Ending::Stopped => parts.insert(0, "Stopped".to_owned()),
        Ending::Failed { .. } => parts.insert(0, "Did not finish".to_owned()),
    }
    let room = Rect::from_min_max(pos2(row.left() + EDGE, row.top()), pos2(right, row.bottom()));
    // In a narrow panel the figures give way before the words are cut.
    let whole = parts.join(" \u{00B7} ");
    let mut shown = parts.len();
    let fits =
        |count: usize| text::width(&painter, theme.text.micro, &parts[..count].join(" \u{00B7} ")) <= room.width();
    while shown > 1 && !fits(shown) {
        shown -= 1;
    }
    let summary = parts[..shown].join(" \u{00B7} ");
    let color = if end.changed { theme.ink_dim } else { theme.ink_faint };
    let used = text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, &summary, color);
    let sensed = Rect::from_min_size(room.min, vec2(used, room.height()));
    let response = ui.interact(sensed, id.with("summary"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &whole));
    let mut about = Vec::new();
    if shown < parts.len() {
        about.push(whole.as_str());
    }
    if matches!(reversal, Some(Reversal::Buried)) && assistant.turn.is_none() {
        about.push("Other changes were made since: Edit \u{203A} Undo steps back to this one");
    }
    if matches!(end.ending, Ending::Finished { estimated_cost: Some(_), .. }) {
        about.push(
            "The cost is Claude Code\u{2019}s own estimate at API prices, not a charge: a subscription is not billed \
             per message",
        );
    }
    if !about.is_empty() {
        hint(state, &response, about.join(" \u{00B7} "));
    }
    ui.add_space(8.0);
}

/// The line at the end of a turn that is still under way.
fn working(ui: &mut Ui, theme: &Theme, stopping: bool, seconds: u64) {
    ui.add_space(4.0);
    let (row, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    let word = if stopping { "Stopping\u{2026}" } else { "Working\u{2026}" };
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, word));
    let painter = ui.painter();
    paint::dot(painter, pos2(row.left() + EDGE, row.center().y), theme.metric.dot, theme.info);
    let room = Rect::from_min_max(pos2(row.left() + INDENT, row.top()), pos2(row.right() - EDGE, row.bottom()));
    text::paint(painter, room, Align2::LEFT_CENTER, theme.text.micro, word, theme.ink_dim);
    let elapsed = transcript::format_seconds(seconds as f32);
    text::paint(painter, room, Align2::RIGHT_CENTER, theme.text.value, &elapsed, theme.ink_faint);
}

// ---- the foot: the message field ------------------------------------------------

/// Height of one line of the message field.
fn field_row(ui: &Ui, theme: &Theme) -> f32 {
    let font = FontId::proportional(theme.text.body.size);
    ui.fonts_mut(|fonts| fonts.row_height(&font))
}

/// How many lines the draft takes in a field `width` wide.
fn draft_rows(ui: &Ui, theme: &Theme, draft: &str, width: f32) -> usize {
    let font = FontId::proportional(theme.text.body.size);
    let galley = ui.painter().layout(draft.to_owned(), font, theme.ink, width);
    galley.rows.len().clamp(*FIELD_ROWS.start(), *FIELD_ROWS.end())
}

/// Width of the text inside the message field of a panel `width` wide.
fn field_width(theme: &Theme, width: f32) -> f32 {
    width - 2.0 * EDGE - 2.0 * (theme.metric.pad + 2.0)
}

fn composer_height(ui: &Ui, cx: &Cx, width: f32) -> f32 {
    let rows = draft_rows(ui, cx.theme, &cx.assistant.draft, field_width(cx.theme, width));
    8.0 + 16.0 + 4.0 + rows as f32 * field_row(ui, cx.theme) + 10.0 + 8.0 + cx.theme.metric.control + 9.0
}

/// What the assistant will be told, the field to type in, and Send or Stop.
fn composer(ui: &mut Ui, cx: &mut Cx, foot: Rect) {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    let inner = foot.shrink2(vec2(EDGE, 0.0));

    // What goes along with the message, in a few words.
    let seen = context::describe(&cx.snap.project, cx.state, cx.live);
    let sees = Rect::from_min_size(pos2(inner.left(), foot.top() + 8.0), vec2(inner.width(), 16.0));
    let used = text::paint(&painter, sees, Align2::LEFT_CENTER, theme.text.micro, "Sees", theme.ink_faint);
    let summary = seen.summary.join(" \u{00B7} ");
    let rest = sees.with_min_x(sees.left() + used + 8.0);
    text::paint(&painter, rest, Align2::LEFT_CENTER, theme.text.micro, &summary, theme.ink_dim);
    let response = ui.interact(sees, ui.scope_id().with("assistant-sees"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("Sees {summary}")));
    cx.hint(&response, "What the assistant is told with your message: the selection, the playhead, the loop, the tab");
    response.on_hover_ui(|ui| {
        ui.set_max_width(520.0);
        ui.label(egui::RichText::new("Sent with your message").color(theme.ink));
        for line in seen.text.lines() {
            ui.label(egui::RichText::new(line).color(theme.ink_dim).size(theme.text.value.size));
        }
    });

    // The field.
    let row = field_row(ui, theme);
    let rows = draft_rows(ui, theme, &cx.assistant.draft, field_width(theme, foot.width()));
    let field =
        Rect::from_min_size(pos2(inner.left(), sees.bottom() + 4.0), vec2(inner.width(), rows as f32 * row + 10.0));
    let id = Id::unique("assistant-message");
    let focused = ui.memory(|memory| memory.has_focus(id));
    cx.assistant.typing_here |= focused;
    painter.rect_filled(field, 0.0, theme.bg);
    paint::frame(&painter, field, if focused { theme.ink_faint } else { theme.hairline });
    let busy = cx.assistant.is_working();
    if std::mem::take(&mut cx.assistant.focus_composer) {
        ui.memory_mut(|memory| memory.request_focus(id));
    }

    // Enter sends; Shift+Enter is the field's own, for a new line. The
    // arrows step through what was sent before while nothing new is typed.
    let mut wants_send = false;
    if focused {
        let (enter, up, down) = ui.input_mut(|input| {
            let plain = !input.modifiers.shift;
            let stepping = cx.assistant.draft.is_empty() || cx.assistant.recalled.is_some();
            (
                plain && input.consume_key(Modifiers::NONE, Key::Enter),
                stepping && input.consume_key(Modifiers::NONE, Key::ArrowUp),
                cx.assistant.recalled.is_some() && input.consume_key(Modifiers::NONE, Key::ArrowDown),
            )
        });
        wants_send = enter;
        if (up || down) && cx.assistant.recall(up) {
            // The cursor goes to the end of what was brought back.
            let end = CCursor::new(cx.assistant.draft.chars().count());
            let mut editor = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            editor.cursor.set_char_range(Some(CCursorRange::one(end)));
            editor.store(ui.ctx(), id);
        }
    }

    let text_area = field.shrink2(vec2(theme.metric.pad + 2.0, 5.0));
    let before = cx.assistant.draft.clone();
    let edited = widgets::place(ui, text_area, "assistant-field", Layout::top_down(Align::Min), |ui| {
        ui.set_clip_rect(text_area.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let editor = egui::TextEdit::multiline(&mut cx.assistant.draft)
                    .id(id)
                    .font(FontId::proportional(theme.text.body.size))
                    .text_color(theme.ink)
                    .frame(egui::Frame::NONE)
                    .margin(egui::Margin::ZERO)
                    .desired_width(text_area.width())
                    .desired_rows(rows)
                    .return_key(KeyboardShortcut::new(Modifiers::SHIFT, Key::Enter));
                ui.add(editor)
            })
            .inner
    });
    edited.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, "Message to the assistant"));
    if cx.assistant.draft != before {
        // Typed over: it is a new message now, not one brought back.
        cx.assistant.recalled = None;
    }
    cx.assistant.field = field;
    if cx.assistant.draft.is_empty() {
        let words = if busy { "Type the next message\u{2026}" } else { "Ask for something, or about the song\u{2026}" };
        let style = TextStyle { size: theme.text.body.size, ..theme.text.body };
        let first = Rect::from_min_size(text_area.min, vec2(text_area.width(), row));
        text::paint(&painter, first, Align2::LEFT_CENTER, style, words, theme.ink_faint);
    }
    cx.hint(&edited, "Enter sends \u{00B7} Shift+Enter starts a new line \u{00B7} Up brings back what you sent before");

    // Send, or Stop while the assistant is answering.
    let bottom =
        Rect::from_min_size(pos2(inner.left(), field.bottom() + 8.0), vec2(inner.width(), theme.metric.control));
    let stopping = cx.assistant.is_stopping();
    let clicked = widgets::place(ui, bottom, "assistant-send", Layout::right_to_left(Align::Center), |ui| {
        if busy {
            ui.add_enabled_ui(!stopping, |ui| {
                trill_noir::button(ui, theme, if stopping { "Stopping\u{2026}" } else { "Stop" }, Variant::Danger)
            })
            .inner
        } else {
            let ready = !cx.assistant.draft.trim().is_empty();
            ui.add_enabled_ui(ready, |ui| trill_noir::button(ui, theme, "Send", Variant::Primary)).inner
        }
    });
    let about = match (busy, stopping) {
        (true, true) => "Waiting for the assistant to stop",
        (true, false) => "Stop the assistant: what it has changed so far stays, as one undo step",
        (false, _) => "Send the message \u{00B7} Enter",
    };
    cx.hint(&clicked, about);
    // The model that will answer, at the left; and what the keys do, if
    // there is room left for it.
    let room = bottom.with_max_x(clicked.rect.left() - 8.0);
    let taken = model_chooser(ui, cx, room);
    let legends: &[&str] = match busy {
        true => &["Send when it has finished"],
        false => &["Enter sends \u{00B7} Shift+Enter new line", "Enter sends"],
    };
    let room = room.with_min_x(room.left() + taken + 10.0);
    let fitting = legends.iter().find(|legend| text::width(&painter, theme.text.micro, legend) <= room.width());
    if let Some(legend) = fitting {
        text::paint(&painter, room, Align2::RIGHT_CENTER, theme.text.micro, legend, theme.ink_faint);
    }

    if busy {
        if clicked.clicked() {
            cx.assistant.stop();
        }
    } else if wants_send || clicked.clicked() {
        let message = cx.assistant.draft.clone();
        if send(cx, &message) {
            cx.assistant.draft.clear();
            cx.assistant.focus_composer = true;
        }
    }
}

/// The chooser of the model that answers the next message, at the left
/// of `room`: a chip that reads the choice and opens the list. Returns the
/// width it took.
///
/// The list is "Default" (whatever the CLI is set up to use), the names
/// the CLI takes for its families of models, and a field for any other
/// name. What a name gets, and whether the user's account may use it, is
/// the CLI's to say: the panel shows what the CLI then reports it is
/// answering as, and promises nothing.
fn model_chooser(ui: &mut Ui, cx: &mut Cx, room: Rect) -> f32 {
    let theme = cx.theme;
    let choice = cx.assistant.model_choice().map(str::to_owned);
    let value = choice.as_deref().unwrap_or(DEFAULT_MODEL);
    let busy = cx.assistant.is_working();
    let label_width = text::width(ui.painter(), theme.text.label, "Model") + theme.metric.pad + 3.0;
    let width = (trill_noir::dropdown_width(ui, theme, value) + label_width).min(room.width());
    if width < 96.0 {
        return 0.0;
    }
    let rect = Rect::from_min_size(
        pos2(room.left(), room.center().y - theme.metric.chip / 2.0),
        vec2(width, theme.metric.chip),
    );
    let id = Id::unique("assistant-model");
    let response = trill_noir::dropdown(ui, theme, rect, id, "Model", value, busy);
    let asked = match &choice {
        Some(name) => format!("Model: Claude Code is asked for \u{201C}{name}\u{201D}"),
        None => "Model: the one Claude Code is set up to use".to_owned(),
    };
    let answering = cx.assistant.reported_model().map(|model| format!(", and answers as {model}")).unwrap_or_default();
    let then = match (busy, cx.assistant.transcript.is_empty()) {
        // The turn under way is being answered by the model it began with.
        (true, _) => "it can be changed once the assistant has finished",
        (false, true) => "click to choose another",
        (false, false) => "click to choose another: the conversation carries on with it",
    };
    cx.hint(&response, format!("{asked}{answering} \u{00B7} {then}"));

    let popup = egui::Popup::default_response_id(&response);
    if std::mem::take(&mut cx.assistant.open_model_menu) {
        egui::Popup::open_id(ui.ctx(), popup);
    }
    if busy {
        egui::Popup::close_id(ui.ctx(), popup);
        return width;
    }

    let field = id.with("name");
    cx.assistant.typing_here |= ui.memory(|memory| memory.has_focus(field.with("editor")));
    let reported = cx.assistant.reported_model().map(str::to_owned);
    let (draft, problem) = (&mut cx.assistant.model_draft, &mut cx.assistant.model_problem);
    let mut chosen: Option<Option<String>> = None;
    egui::Popup::menu(&response).gap(1.0).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(236.0);
        trill_noir::menu_heading(ui, theme, "Model");
        let about = "The model Claude Code is set up to use, in its own settings.";
        if MenuItem::new(DEFAULT_MODEL).checked(choice.is_none()).detail(about).show(ui, theme).clicked() {
            chosen = Some(None);
        }
        let about = "Asks Claude Code for its newest model of this family. Whether your account can use it is for \
                     Claude Code to say.";
        for alias in MODEL_ALIASES {
            let checked = choice.as_deref() == Some(alias);
            if MenuItem::new(alias).checked(checked).detail(about).show(ui, theme).clicked() {
                chosen = Some(Some((*alias).to_owned()));
            }
        }
        // A name of the user's own that is in use.
        if let Some(own) = choice.as_deref().filter(|own| !MODEL_ALIASES.contains(own)) {
            MenuItem::new(own).checked(true).show(ui, theme);
        }
        trill_noir::menu_separator(ui, theme);
        trill_noir::menu_heading(ui, theme, "Another name");
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), theme.metric.control + 6.0), Sense::hover());
        let typed = trill_noir::text_field(
            ui,
            theme,
            row.shrink2(vec2(2.0 * theme.metric.pad, 3.0)).with_max_y(row.bottom() - 6.0),
            field,
            draft,
            "A model\u{2019}s name, then Enter",
        );
        typed.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, "Another model name"));
        if typed.changed() {
            *problem = None;
        }
        if typed.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
            let name = draft.trim();
            if trill_assistant::is_model_name(name) {
                chosen = Some(Some(name.to_owned()));
                ui.close();
            } else {
                // Said here, with the field still open to put it right.
                *problem = Some(super::not_a_model_name(name));
                typed.request_focus();
            }
        }
        if let Some(problem) = problem.as_deref() {
            let style = TextStyle { size: 12.0, ..theme.text.body };
            let inset = 2.0 * theme.metric.pad;
            let galley = prose::plain(ui, theme, style, theme.warn, problem, ui.available_width() - 2.0 * inset);
            let (row, said) = ui.allocate_exact_size(vec2(ui.available_width(), galley.size().y + 6.0), Sense::hover());
            said.widget_info(|| WidgetInfo::labeled(Role::Label, true, problem));
            prose::paint(ui, pos2(row.left() + inset, row.top()), galley);
        }
        if let Some(model) = &reported {
            trill_noir::menu_separator(ui, theme);
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
            let words = format!("Answering as {model}");
            ui.interact(row, id.with("reported"), Sense::hover())
                .widget_info(|| WidgetInfo::labeled(Role::Label, true, &words));
            let room = row.shrink2(vec2(2.0 * theme.metric.pad, 0.0));
            text::paint(ui.painter(), room, Align2::LEFT_CENTER, theme.text.micro, &words, theme.ink_faint);
        }
    });
    if let Some(model) = chosen {
        match cx.assistant.choose_model(model.as_deref()) {
            Ok(()) => {
                cx.assistant.model_draft.clear();
                cx.assistant.model_problem = None;
                cx.assistant.focus_composer = true;
            }
            Err(reason) => cx.assistant.model_problem = Some(reason),
        }
    }
    width
}

// ---- the foot: why the assistant cannot be used -------------------------------------

/// The heading and the words for the state the assistant is in while it
/// cannot be talked to. The words are the back end's own.
fn explained(readiness: &Readiness) -> (&'static str, String) {
    match readiness {
        Readiness::Unknown | Readiness::Checking => (
            "Looking for Claude Code\u{2026}",
            "The assistant runs your own installed Claude Code. Finding it takes a second or two.".to_owned(),
        ),
        Readiness::Ready(_) => ("Ready", String::new()),
        Readiness::Unavailable(problem) => {
            let heading = match problem {
                Problem::Cli(Unavailable::NotInstalled) => "Claude Code is not installed",
                Problem::Cli(Unavailable::UnsupportedInstall { .. }) => "Claude Code cannot be started",
                Problem::Cli(Unavailable::NotWorking { .. }) => "Claude Code is not working",
                Problem::Cli(Unavailable::TooOld { .. }) => "Claude Code is too old",
                Problem::Cli(Unavailable::NotLoggedIn { .. }) => "Not signed in",
                Problem::Mcp(_) => "The MCP server is not running",
                Problem::NoFolder => "Not available in this run",
            };
            (heading, problem.to_string())
        }
    }
}

fn card_style(theme: &Theme) -> TextStyle {
    TextStyle { size: 12.0, ..theme.text.body }
}

fn card_height(ui: &Ui, cx: &Cx, width: f32) -> f32 {
    let (_, words) = explained(&cx.assistant.readiness);
    let galley = prose::plain(ui, cx.theme, card_style(cx.theme), cx.theme.ink_dim, &words, width - INDENT - EDGE);
    let checking = matches!(cx.assistant.readiness, Readiness::Unknown | Readiness::Checking);
    let button = if checking { 0.0 } else { 10.0 + cx.theme.metric.control };
    10.0 + 18.0 + 4.0 + galley.size().y + button + 12.0
}

/// Says why the assistant cannot be used and offers to look again. There
/// is nothing to sign in with here, and there must never be.
fn card(ui: &mut Ui, cx: &mut Cx, foot: Rect) {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    let (heading, words) = explained(&cx.assistant.readiness);
    let checking = matches!(cx.assistant.readiness, Readiness::Unknown | Readiness::Checking);

    let head =
        Rect::from_min_size(pos2(foot.left() + INDENT, foot.top() + 10.0), vec2(foot.width() - INDENT - EDGE, 18.0));
    let color = if checking { theme.ink_faint } else { theme.warn };
    paint::dot(&painter, pos2(foot.left() + EDGE, head.center().y), theme.metric.dot, color);
    text::paint(&painter, head, Align2::LEFT_CENTER, theme.text.caps, heading, theme.ink);
    let galley = prose::plain(ui, theme, card_style(theme), theme.ink_dim, &words, head.width());
    let said = Rect::from_min_size(pos2(head.left(), head.bottom() + 4.0), galley.size());
    let id = ui.scope_id().with("assistant-card");
    let response = ui.interact(Rect::from_min_max(foot.min, pos2(foot.right(), said.bottom())), id, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{heading}. {words}")));
    // What it says can be taken along: it names the command to run.
    prose::selectable(ui, id.with("words"), said.min, galley);
    if checking {
        return;
    }

    let row = Rect::from_min_size(pos2(head.left(), said.bottom() + 10.0), vec2(head.width(), theme.metric.control));
    let again = widgets::place(ui, row, "assistant-check", Layout::left_to_right(Align::Center), |ui| {
        trill_noir::button(ui, theme, "Check again", Variant::Primary)
    });
    cx.hint(&again, "Look again for Claude Code and for the server it works through");
    if again.clicked() {
        cx.assistant.check_again();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_things_to_ask_go_by_what_is_open() {
        use crate::ui::DetailTab;
        let mut state = UiState::new(DetailTab::Clip);
        let empty = Project::new("Untitled");
        let begin = starting_points(&empty, &state);
        assert!(begin[0].starts_with("Start a laid-back beat"), "{begin:?}");
        assert!(!begin.iter().any(|point| point.contains("selected")), "there is nothing to select yet");

        let mut song = Project::new("Night Drive");
        let lead = song.add_track("Lead");
        let hook = song.add_midi_clip(lead, "Hook", 0.0, 4.0).unwrap();
        state.reconcile(&song);
        let general = starting_points(&song, &state);
        assert_eq!(general[0], "What is in this song, and what key is it in?");
        assert!(!general.iter().any(|point| point.contains("selected")), "nothing is selected: {general:?}");

        state.select_clip(lead, hook);
        let about_it = starting_points(&song, &state);
        assert!(about_it.contains(&"Make the selected clip busier."), "{about_it:?}");
        for points in [&begin, &general, &about_it] {
            assert!((3..=4).contains(&points.len()), "a few, not a menu: {points:?}");
            assert!(points.iter().all(|point| !point.contains("track_id") && !point.contains("MIDI")), "{points:?}");
        }
    }

    #[test]
    fn every_state_that_is_not_ready_is_explained_in_the_back_ends_words() {
        use trill_assistant::{CliInstallation, CliVersion, MINIMUM_CLI_VERSION};
        let installation = CliInstallation { path: "claude".into(), version: CliVersion::new(2, 0, 1) };
        let problems = [
            Problem::Cli(Unavailable::NotInstalled),
            Problem::Cli(Unavailable::UnsupportedInstall { path: "claude.cmd".into() }),
            Problem::Cli(Unavailable::NotWorking {
                path: "claude".into(),
                detail: "it did not report a version".into(),
            }),
            Problem::Cli(Unavailable::TooOld { installation: installation.clone(), minimum: MINIMUM_CLI_VERSION }),
            Problem::Cli(Unavailable::NotLoggedIn { installation }),
            Problem::Mcp("Trill could not start the MCP server the assistant works through: the port is taken.".into()),
            Problem::NoFolder,
        ];
        let mut headings = Vec::new();
        for problem in problems {
            let (heading, words) = explained(&Readiness::Unavailable(problem.clone()));
            assert_eq!(words, problem.to_string(), "the words are the back end's, unchanged");
            assert!(!heading.is_empty() && !words.is_empty());
            headings.push(heading);
        }
        headings.dedup();
        assert_eq!(headings.len(), 7, "each has a heading of its own: {headings:?}");
        let signed_out = explained(&Readiness::Unavailable(Problem::Cli(Unavailable::NotLoggedIn {
            installation: CliInstallation { path: "claude".into(), version: CliVersion::new(2, 1, 325) },
        })));
        assert!(signed_out.1.contains("run `claude`"), "signing in is done in the CLI: {}", signed_out.1);
        assert!(explained(&Readiness::Checking).0.starts_with("Looking for"));
    }
}
