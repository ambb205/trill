//! The panel itself: a header, the conversation, and at the foot the
//! message field, or in its place the reason the assistant cannot be used.
//!
//! It is laid out like the rest of the window, not like a messenger: the
//! user's messages are section strips, the assistant's reply is running
//! text, each tool call is a row with a status mark that unfolds to its
//! arguments and its answer, and a turn closes with a line that says what
//! it cost in time and whether it changed the song, with the Undo for it.

use super::transcript::{self, Ending, Item, Offer, Tone, ToolLine, ToolState, TurnEnd};
use super::{Assistant, Billing, Problem, Readiness, Reversal, context};
use crate::ui::shortcuts::{self, Command};
use crate::ui::state::{Cx, UiState};
use crate::ui::widgets;
use eframe::egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Galley, Id, Key, KeyboardShortcut, Layout, Modifiers, Rangef, Rect, Response,
    Role, Sense, Shape, Stroke, Ui, WidgetInfo, pos2, vec2,
};
use std::sync::Arc;
use trill_assistant::Unavailable;
use trill_noir::{Chip, Icon, TextStyle, Theme, Variant, paint, text};

/// Height of the header, as the browser's row of tabs.
const HEADER: f32 = 24.0;
/// Space between the panel's edges and what is in it.
const EDGE: f32 = 12.0;
/// Height of a tool call's row.
const ROW: f32 = 22.0;
/// Where a row's words begin, after its status mark.
const INDENT: f32 = EDGE + 13.0;
/// Line height of running text, as a multiple of its size.
const LEADING: f32 = 1.42;
/// The message field shows between this many lines and scrolls beyond.
const FIELD_ROWS: std::ops::RangeInclusive<usize> = 2..=8;

/// What a click in the conversation asks for, carried out once the
/// conversation is no longer being read.
enum Action {
    Unfold(usize),
    Reverse(Reversal),
    Send(String),
    Draft(String),
    Copy(String),
}

/// Running text in `style`, wrapped at `width`.
fn flow(ui: &Ui, style: TextStyle, words: &str, color: Color32, width: f32) -> Arc<Galley> {
    let words = if style.upper { words.to_uppercase() } else { words.to_owned() };
    let format = TextFormat {
        font_id: FontId::proportional(style.size),
        color,
        extra_letter_spacing: style.tracking,
        line_height: Some((style.size * LEADING).round()),
        ..Default::default()
    };
    let mut job = LayoutJob::single_section(words, format);
    job.wrap.max_width = width.max(40.0);
    ui.painter().layout_job(job)
}

/// Adds running text to the column, `left` and `right` in from its edges,
/// and says what it reads to assistive technology.
fn paragraph(ui: &mut Ui, style: TextStyle, words: &str, color: Color32, left: f32, right: f32) -> Response {
    let galley = flow(ui, style, words, color, ui.available_width() - left - right);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), galley.size().y), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    if ui.is_rect_visible(rect) {
        ui.painter().galley(pos2(rect.left() + left, rect.top()), galley, color);
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
fn action_chip(ui: &Ui, theme: &Theme, right: f32, row: Rect, id: Id, words: &str) -> (Response, f32) {
    let chip = Chip::action(words);
    let width = chip.width(ui, theme);
    let rect = Rect::from_min_size(
        pos2(right - width, row.center().y - theme.metric.chip / 2.0),
        vec2(width, theme.metric.chip),
    );
    (trill_noir::chip(ui, theme, rect, id, &chip), rect.left())
}

// ---- the reply as blocks ----------------------------------------------------

/// A piece of the assistant's reply, as it is set.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Block {
    Paragraph(String),
    /// An item of a list, with what stands before it: a dash or a number.
    Listed {
        mark: String,
        words: String,
    },
    Heading(String),
    /// Space between passages.
    Gap,
}

/// Emphasis and code marks taken out: the panel sets replies in one face,
/// so the marks would only be noise.
fn unmarked(words: &str) -> String {
    words.replace("**", "").replace('`', "")
}

/// Cuts a reply into paragraphs, list items and headings. The assistant is
/// asked to write plain text; what Markdown it still writes is read rather
/// than shown as punctuation.
fn blocks(reply: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for line in reply.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("```") {
            if !matches!(blocks.last(), Some(Block::Gap) | None) {
                blocks.push(Block::Gap);
            }
        } else if let Some(title) = line.strip_prefix('#') {
            blocks.push(Block::Heading(unmarked(title.trim_start_matches('#').trim())));
        } else if let Some(item) = ["- ", "* ", "\u{2022} "].iter().find_map(|mark| line.strip_prefix(mark)) {
            blocks.push(Block::Listed { mark: "\u{2013}".to_owned(), words: unmarked(item.trim()) });
        } else if let Some((number, item)) = line.split_once(". ").filter(|(number, _)| {
            !number.is_empty() && number.len() <= 2 && number.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            blocks.push(Block::Listed { mark: format!("{number}."), words: unmarked(item.trim()) });
        } else {
            blocks.push(Block::Paragraph(unmarked(line)));
        }
    }
    while matches!(blocks.last(), Some(Block::Gap)) {
        blocks.pop();
    }
    blocks
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

/// The name of the panel, how it stands, and its two buttons.
fn header(ui: &mut Ui, cx: &mut Cx, bar: Rect) {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    paint::hline_above(&painter, bar.x_range(), bar.bottom(), theme.hairline);
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
    let status = Rect::from_min_max(pos2(x, bar.top()), pos2(bar.right() - 2.0 * HEADER, bar.bottom()));
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

    let button = |index: f32| {
        Rect::from_min_size(pos2(bar.right() - (index + 1.0) * HEADER, bar.top()), vec2(HEADER, HEADER - 1.0))
    };
    let id = ui.scope_id().with("assistant-header");
    let close =
        trill_noir::bare_icon_button(ui, theme, button(0.0), id.with("close"), Icon::Close, "Close the assistant");
    cx.hint(&close, format!("Put the assistant away \u{00B7} {}", shortcuts::keys(Command::ToggleAssistant)));
    if close.clicked() {
        cx.assistant.toggle();
    }
    let fresh = trill_noir::bare_icon_button(ui, theme, button(1.0), id.with("new"), Icon::Plus, "New conversation");
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
    let galley = flow(ui, style, &words, theme.ink, room.width() - INDENT - EDGE);
    let strip = Rect::from_min_size(room.min, vec2(room.width(), galley.size().y + 14.0));
    let painter = ui.painter();
    painter.rect_filled(strip, 0.0, theme.raised);
    paint::hline_above(painter, strip.x_range(), strip.bottom(), theme.hairline);
    paint::dot(painter, pos2(strip.left() + EDGE, strip.top() + 7.0 + style.size * 0.7), theme.metric.dot, theme.warn);
    painter.galley(pos2(strip.left() + INDENT, strip.top() + 7.0), galley, theme.ink);
    let response = ui.interact(strip, ui.scope_id().with("assistant-billing"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &words));
    strip.bottom()
}

// ---- the conversation ---------------------------------------------------------

fn conversation(ui: &mut Ui, cx: &mut Cx, body: Rect) {
    let mut actions = Vec::new();
    widgets::place(ui, body, "assistant-conversation", Layout::top_down(Align::Min), |ui| {
        ui.set_clip_rect(body.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let Cx { assistant, state, theme, .. } = cx;
            if assistant.transcript.is_empty() {
                welcome(ui, theme, state, &mut actions);
            }
            for (index, item) in assistant.transcript.items.iter().enumerate() {
                match item {
                    Item::User { text } => said(ui, theme, text, index > 0, &mut actions),
                    Item::Reply { text } => reply(ui, theme, index, text, &mut actions),
                    Item::Tool(line) => tool(ui, theme, state, index, line, &mut actions),
                    Item::Note { text, tone, .. } => note(ui, theme, text, *tone),
                    Item::End(end) => ended(ui, theme, state, assistant, index, end, &mut actions),
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

    for action in actions {
        match action {
            Action::Unfold(index) => {
                if let Some(Item::Tool(line)) = cx.assistant.transcript.items.get_mut(index) {
                    line.open = !line.open;
                }
            }
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

/// Sends `message` with what the user is looking at.
fn send(cx: &mut Cx, message: &str) -> bool {
    let context = context::describe(&cx.snap.project, cx.state, cx.live);
    let sent = cx.assistant.submit(message, &context);
    if sent {
        cx.assistant.reveal_end = true;
    }
    sent
}

/// What the panel says before anything has been said.
fn welcome(ui: &mut Ui, theme: &Theme, state: &mut UiState, actions: &mut Vec<Action>) {
    ui.add_space(14.0);
    label(ui, theme.text.caps, "Ask about this song", theme.ink, EDGE, 16.0);
    ui.add_space(6.0);
    let style = TextStyle { size: 12.0, ..theme.text.body };
    paragraph(
        ui,
        style,
        "The assistant works on the open project with Trill\u{2019}s own tools. It is told what you have selected, \
         where the playhead and the loop are and which tab is open, so \u{201C}this\u{201D} means what you are looking \
         at.",
        theme.ink_dim,
        EDGE,
        EDGE,
    );
    ui.add_space(6.0);
    paragraph(
        ui,
        style,
        "Everything it changes while answering one message is undone with one Undo.",
        theme.ink_dim,
        EDGE,
        EDGE,
    );
    ui.add_space(14.0);
    label(ui, theme.text.micro, "For example", theme.ink_faint, EDGE, 16.0);
    let top = ui.cursor().top();
    paint::hline(ui.painter(), ui.max_rect().x_range(), top, theme.hairline);
    for example in [
        "What is in this song, and what key is it in?",
        "Add a drum part to the loop that fits what is there.",
        "Make the selected clip busier.",
    ] {
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW + 4.0), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, example));
        if ui.is_rect_visible(rect) {
            if response.hovered() {
                ui.painter().rect_filled(rect, 0.0, theme.hover);
            }
            let color = if response.hovered() { theme.ink } else { theme.ink_dim };
            text::paint(ui.painter(), rect.shrink2(vec2(EDGE, 0.0)), Align2::LEFT_CENTER, style, example, color);
            paint::hline_above(ui.painter(), rect.x_range(), rect.bottom(), theme.hairline);
        }
        hint(state, &response, "Click to put this in the message field");
        if response.clicked() {
            actions.push(Action::Draft(example.to_owned()));
        }
    }
}

/// A message of the user's: a strip across the panel, like a section head.
fn said(ui: &mut Ui, theme: &Theme, words: &str, ruled_above: bool, actions: &mut Vec<Action>) {
    let galley = flow(ui, theme.text.body, words, theme.ink, ui.available_width() - 2.0 * EDGE);
    let height = 8.0 + 14.0 + 3.0 + galley.size().y + 9.0;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("You: {words}")));
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
        painter.galley(pos2(rect.left() + EDGE, head.bottom() + 3.0), galley, theme.ink);
    }
    response.context_menu(|ui| {
        if trill_noir::MenuItem::new("Copy").show(ui, theme).clicked() {
            actions.push(Action::Copy(words.to_owned()));
        }
        if trill_noir::MenuItem::new("Put back in the message field").show(ui, theme).clicked() {
            actions.push(Action::Draft(words.to_owned()));
        }
    });
}

/// A passage of the assistant's reply.
fn reply(ui: &mut Ui, theme: &Theme, index: usize, words: &str, actions: &mut Vec<Action>) {
    ui.add_space(9.0);
    let started = ui.cursor().top();
    for block in blocks(words) {
        match block {
            Block::Paragraph(words) => {
                paragraph(ui, theme.text.body, &words, theme.ink, EDGE, EDGE);
            }
            Block::Listed { mark, words } => {
                let top = ui.cursor().top();
                let response = paragraph(ui, theme.text.body, &words, theme.ink, EDGE + 16.0, EDGE);
                let cell = Rect::from_min_size(pos2(response.rect.left() + EDGE, top), vec2(16.0, 19.0));
                text::paint(ui.painter(), cell, Align2::LEFT_CENTER, theme.text.body, &mark, theme.ink_dim);
            }
            Block::Heading(words) => {
                ui.add_space(2.0);
                label(ui, theme.text.caps, &words, theme.ink_dim, EDGE, 18.0);
            }
            Block::Gap => {
                ui.add_space(7.0);
            }
        }
    }
    ui.add_space(5.0);
    let passage =
        Rect::from_min_max(pos2(ui.max_rect().left(), started), pos2(ui.max_rect().right(), ui.cursor().top()));
    let response = ui.interact(passage, ui.scope_id().with(("reply", index)), Sense::click());
    response.context_menu(|ui| {
        if trill_noir::MenuItem::new("Copy").show(ui, theme).clicked() {
            actions.push(Action::Copy(words.to_owned()));
        }
    });
}

/// A tool call: a row that says what it does, with a mark for how it
/// stands, and under it, unfolded, what went in and what came back.
fn tool(ui: &mut Ui, theme: &Theme, state: &mut UiState, index: usize, line: &ToolLine, actions: &mut Vec<Action>) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let words = line.words();
    let failed = line.state == ToolState::Failed;
    response.widget_info(|| {
        let name = match line.state {
            ToolState::Running => format!("{words}\u{2026}"),
            ToolState::Failed => format!("{words}: failed"),
            ToolState::Stopped => format!("{words}: not finished"),
            ToolState::Done => words.to_owned(),
        };
        WidgetInfo::selected(Role::Button, true, line.open, name)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(rect, 0.0, theme.hover);
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
        trill_noir::paint_icon(painter, caret, if line.open { Icon::CaretDown } else { Icon::CaretRight }, quiet);
        let mut room = Rect::from_min_max(pos2(rect.left() + INDENT, rect.top()), pos2(caret.x - 10.0, rect.bottom()));
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
    hint(
        state,
        &response,
        if failed {
            "The tool reported an error, which the assistant sees too \u{00B7} click for what was sent and answered"
        } else {
            "Click for what was sent to the tool and what it answered"
        },
    );
    if response.clicked() {
        actions.push(Action::Unfold(index));
    }
    if !line.open {
        return;
    }

    // Unfolded: the tool by its own name, what it was given, what it said.
    let style = TextStyle { size: theme.text.value.size, ..theme.text.body };
    let width = ui.available_width() - INDENT - EDGE - 16.0;
    let given = (!line.input.is_empty()).then(|| flow(ui, style, &line.input, theme.ink_dim, width));
    let output = (!line.output.is_empty()).then(|| flow(ui, style, &line.output, theme.ink_dim, width));
    let part = |galley: &Option<Arc<Galley>>| galley.as_ref().map_or(0.0, |galley| 16.0 + galley.size().y + 6.0);
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
        text::paint(painter, head, Align2::LEFT_CENTER, theme.text.micro, title, theme.ink_faint);
        painter.with_clip_rect(well.intersect(painter.clip_rect())).galley(
            pos2(inner.left(), head.bottom()),
            galley.clone(),
            theme.ink_dim,
        );
        y = head.bottom() + galley.size().y + 6.0;
    }
}

/// Something the back end or the panel has to say.
fn note(ui: &mut Ui, theme: &Theme, words: &str, tone: Tone) {
    ui.add_space(6.0);
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let top = ui.cursor().top();
    let response = paragraph(ui, style, words, theme.ink_dim, INDENT, EDGE);
    let color = match tone {
        Tone::Info => theme.info,
        Tone::Warning => theme.warn,
    };
    paint::dot(ui.painter(), pos2(response.rect.left() + EDGE, top + style.size * 0.7), theme.metric.dot, color);
    ui.add_space(6.0);
}

/// The line that closes a turn: how it ended, what it took, and whether it
/// changed the song, with the Undo for that.
fn ended(
    ui: &mut Ui,
    theme: &Theme,
    state: &mut UiState,
    assistant: &Assistant,
    index: usize,
    end: &TurnEnd,
    actions: &mut Vec<Action>,
) {
    let id = ui.scope_id().with(("turn-end", index));
    if let Ending::Failed { text: reason, .. } = &end.ending {
        ui.add_space(8.0);
        let top = ui.cursor().top();
        let response = paragraph(ui, theme.text.body, reason, theme.ink, INDENT, EDGE);
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

    // What can be done: the same message again, a word to carry on, and
    // taking back what the turn changed.
    if let Ending::Failed { offer, .. } = &end.ending
        && let Some(message) = assistant.offered(*offer)
        && assistant.turn.is_none()
        && matches!(assistant.readiness, Readiness::Ready(_))
    {
        let (words, about) = match offer {
            Offer::Continue => ("Continue", "Ask the assistant to carry on from where it stopped"),
            _ => ("Try again", "Send the same message again"),
        };
        let (response, left) = action_chip(ui, theme, right, row, id.with("offer"), words);
        hint(state, &response, about);
        if response.clicked() {
            actions.push(Action::Send(message));
        }
        right = left - 6.0;
    }
    let reversal = end.step.map(|step| assistant.reversal(step));
    match reversal {
        Some(reversal @ Reversal::Undo(_)) => {
            let (response, left) = action_chip(ui, theme, right, row, id.with("undo"), "Undo");
            hint(state, &response, "Take back everything the assistant changed while answering this message");
            if response.clicked() {
                actions.push(Action::Reverse(reversal));
            }
            right = left - 8.0;
        }
        Some(reversal @ Reversal::Redo(_)) => {
            let (response, left) = action_chip(ui, theme, right, row, id.with("redo"), "Redo");
            hint(state, &response, "Bring back what the assistant changed while answering this message");
            if response.clicked() {
                actions.push(Action::Reverse(reversal));
            }
            right = left - 8.0;
        }
        Some(Reversal::Buried) | None => {}
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
    let summary = parts.join(" \u{00B7} ");
    let room = Rect::from_min_max(pos2(row.left() + EDGE, row.top()), pos2(right, row.bottom()));
    let color = if end.changed { theme.ink_dim } else { theme.ink_faint };
    let used = text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, &summary, color);
    let sensed = Rect::from_min_size(room.min, vec2(used, room.height()));
    let response = ui.interact(sensed, id.with("summary"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &summary));
    let mut about = Vec::new();
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
    let legend = if busy { "Send when it has finished" } else { "Enter sends \u{00B7} Shift+Enter new line" };
    let room = bottom.with_max_x(clicked.rect.left() - 8.0);
    text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, legend, theme.ink_faint);

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
    let galley = flow(ui, card_style(cx.theme), &words, cx.theme.ink_dim, width - INDENT - EDGE);
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
    let galley = flow(ui, card_style(theme), &words, theme.ink_dim, head.width());
    let said = Rect::from_min_size(pos2(head.left(), head.bottom() + 4.0), galley.size());
    painter.galley(said.min, galley, theme.ink_dim);
    let response = ui.interact(
        Rect::from_min_max(foot.min, pos2(foot.right(), said.bottom())),
        ui.scope_id().with("assistant-card"),
        Sense::hover(),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{heading}. {words}")));
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
    fn a_reply_is_read_as_paragraphs_lists_and_headings() {
        let reply = "## What I did\n\nI **doubled** the `hats`.\n\n- Kick on *one*\n* Snare on two\n1. First\n12. Twelfth\n\n\n```\nnot a list. really\n```\nDone.\n\n";
        assert_eq!(
            blocks(reply),
            [
                Block::Heading("What I did".into()),
                Block::Gap,
                Block::Paragraph("I doubled the hats.".into()),
                Block::Gap,
                Block::Listed { mark: "\u{2013}".into(), words: "Kick on *one*".into() },
                Block::Listed { mark: "\u{2013}".into(), words: "Snare on two".into() },
                Block::Listed { mark: "1.".into(), words: "First".into() },
                Block::Listed { mark: "12.".into(), words: "Twelfth".into() },
                Block::Gap,
                Block::Paragraph("not a list. really".into()),
                Block::Gap,
                Block::Paragraph("Done.".into()),
            ]
        );
        assert!(blocks("").is_empty() && blocks("\n\n").is_empty());
        // A sentence that merely contains a full stop is not a numbered item.
        assert_eq!(blocks("Bar 5. Then bar 9."), [Block::Paragraph("Bar 5. Then bar 9.".into())]);
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
