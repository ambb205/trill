//! The story down the right of the making-of: the conversation as it
//! came, and every step under who made it.
//!
//! It reads like the assistant's panel did while the session ran: what the
//! user asked, what the assistant said (arriving as it is played), each
//! call as a line in plain words, the plan ticking off at the foot. What a
//! panel never showed is here too: the edits made by hand between the
//! requests, and the steps of a client that has no conversation, whose
//! calls are its story.

use super::{Watching, hue, span, who};
use crate::ui::assistant::reply;
use crate::ui::widgets;
use eframe::egui::{self, Align, Align2, Layout, Rect, Role, Sense, Stroke, Ui, WidgetInfo, pos2, vec2};
use trill_core::journal::replay::Moment;
use trill_core::journal::{Author, Entry, Look, PlanItem, PlanState, Step, What};
use trill_noir::{TextStyle, Theme, paint, prose, text};

const HEADER: f32 = 24.0;
/// What stands off the panel's edges, as in the assistant's.
const EDGE: f32 = 12.0;
/// Height of a step's line, and where its words begin.
const ROW: f32 = 22.0;
const INDENT: f32 = EDGE + 13.0;
/// The most entries the story shows above the moment under way: a session
/// of thousands is not laid out whole on every frame.
const SHOWN: usize = 400;
const PLAN_HEAD: f32 = 24.0;
const PLAN_STEP: f32 = 20.0;
const PLAN_STEPS: usize = 6;

/// The story: a heading, the entries up to the moment under way, and the
/// plan as it stood then.
pub fn show(ui: &mut Ui, theme: &Theme, watching: &mut Watching) {
    let area = ui.max_rect();
    ui.expand_to_include_rect(area);
    let painter = ui.painter().clone();
    painter.rect_filled(area, 0.0, theme.panel);
    paint::vline(&painter, area.left(), area.y_range(), theme.hairline);
    let area = area.with_min_x(area.left() + paint::hair(&painter));

    let replay = &watching.replay;
    let moments = replay.moments();
    let upto = watching.moment().map_or(0, |index| index + 1);

    // The heading: what this is, and how far into the session by the
    // clock on the wall.
    let head = Rect::from_min_size(area.min, vec2(area.width(), HEADER));
    paint::hline_above(&painter, head.x_range(), head.bottom(), theme.hairline);
    let room = head.shrink2(vec2(EDGE, 0.0));
    text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.caps, "Story", theme.ink);
    let first = replay.journal().entries().next().map_or(0, |entry| entry.at);
    if let Some(entry) = watching.entry() {
        let into = format!("{} in", span(entry.at.saturating_sub(first) as f64 / 1000.0));
        text::paint(&painter, room, Align2::RIGHT_CENTER, theme.text.micro, &into, theme.ink_faint);
    }

    // The plan as it stood at this moment, at the foot.
    let plan = moments[..upto].iter().rev().find_map(|moment| match &replay.entry(moment).what {
        What::Plan { items } if !items.is_empty() => Some(items),
        _ => None,
    });
    let planned = plan.map_or(0.0, |items| PLAN_HEAD + items.len().min(PLAN_STEPS) as f32 * PLAN_STEP + 6.0);
    let planned = planned.min((area.height() - HEADER - 96.0).max(0.0));
    let foot = Rect::from_min_max(pos2(area.left(), area.bottom() - planned), area.max);
    let body = Rect::from_min_max(pos2(area.left(), head.bottom()), pos2(area.right(), foot.top()));
    painter.rect_filled(body, 0.0, theme.bg);

    let progress = watching.progress;
    let current = watching.moment();
    let scroll = watching.told != current;
    let mut replies = std::mem::take(&mut watching.replies);
    replies.begin(0, moments.len());
    widgets::place(ui, body, "making-of-story", Layout::top_down(Align::Min), |ui| {
        ui.set_clip_rect(body.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let from = upto.saturating_sub(SHOWN);
            if from > 0 {
                note(ui, theme, &format!("{from} earlier entries"));
            } else if replay.journal().dropped().entries > 0 {
                let dropped = replay.journal().dropped().entries;
                note(ui, theme, &format!("The story begins here: {dropped} earlier entries were let go"));
            }
            // Who the entries just above are by, so that a run of one
            // author's has one heading.
            let mut above: Option<&Author> = None;
            for (index, moment) in moments.iter().enumerate().take(upto).skip(from) {
                let entry = replay.entry(moment);
                if entry.is_aside() || matches!(entry.what, What::Plan { .. }) {
                    continue;
                }
                if let Some(away) = moment.away {
                    note(ui, theme, &format!("{} later", span(away)));
                    above = None;
                }
                let under_way = current == Some(index);
                match &entry.what {
                    What::Said { text } if entry.by == Author::Person => {
                        said(ui, theme, text, index > from);
                        above = None;
                        continue;
                    }
                    What::Mark { label } => {
                        mark(ui, theme, label);
                        above = None;
                        continue;
                    }
                    _ => {}
                }
                if above != Some(&entry.by) {
                    heading(ui, theme, &entry.by);
                    above = Some(&entry.by);
                }
                match &entry.what {
                    What::Said { text } => {
                        // A reply under way arrives as it is played.
                        let (shown, open) = match under_way.then(|| arriving(text, moment, progress)).flatten() {
                            Some(shown) => (shown, true),
                            None => (text.as_str(), false),
                        };
                        passage(ui, theme, &mut replies, index, shown, open);
                    }
                    What::Step(step) => line(ui, theme, entry, Line::Step(step), under_way),
                    What::Look(look) => line(ui, theme, entry, Line::Look(look), under_way),
                    What::Plan { .. } | What::Mark { .. } => {}
                }
            }
            ui.add_space(10.0);
            if scroll {
                ui.scroll_to_cursor(Some(Align::BOTTOM));
            }
        });
    });
    watching.replies = replies;
    watching.told = current;

    if let Some(items) = plan
        && planned >= PLAN_HEAD
    {
        plan_block(ui, theme, foot, items);
    }
}

/// How much of a reply has arrived, `progress` of the way through its
/// moment: `None` once all of it has. It is all there a little before the
/// moment ends, so that it stands to be read.
fn arriving<'a>(words: &'a str, moment: &Moment, progress: f64) -> Option<&'a str> {
    let through = (progress / 0.8).clamp(0.0, 1.0);
    if through >= 1.0 || moment.lasts <= 0.0 {
        return None;
    }
    let letters = words.chars().count();
    let shown = (letters as f64 * through).ceil() as usize;
    let end = words.char_indices().nth(shown).map_or(words.len(), |(at, _)| at);
    Some(&words[..end])
}

/// A quiet line across the story: time that passed, where it begins.
fn note(ui: &mut Ui, theme: &Theme, words: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let width = text::width(painter, theme.text.micro, words);
    let label = Rect::from_center_size(rect.center(), vec2(width, rect.height()));
    text::paint(painter, label, Align2::CENTER_CENTER, theme.text.micro, words, theme.ink_faint);
    paint::hline(painter, (rect.left() + EDGE)..=(label.left() - 8.0), rect.center().y, theme.hairline);
    paint::hline(painter, (label.right() + 8.0)..=(rect.right() - EDGE), rect.center().y, theme.hairline);
}

/// Who the entries below are by.
fn heading(ui: &mut Ui, theme: &Theme, author: &Author) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let row = rect.with_min_y(rect.top() + 6.0);
    let left =
        paint::dot(painter, pos2(rect.left() + EDGE, row.center().y), theme.metric.dot, hue(theme, author)) + 6.0;
    text::paint(painter, row.with_min_x(left), Align2::LEFT_CENTER, theme.text.micro, &who(author), theme.ink_faint);
}

/// What the user asked, set as the assistant's panel sets it.
fn said(ui: &mut Ui, theme: &Theme, words: &str, ruled_above: bool) {
    let galley = prose::plain(ui, theme, theme.text.body, theme.ink, words, ui.available_width() - 2.0 * EDGE);
    let height = 8.0 + 14.0 + 3.0 + galley.size().y + 9.0;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("You: {words}")));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme.panel);
    if ruled_above {
        paint::hline(painter, rect.x_range(), rect.top(), theme.hairline);
    }
    paint::hline_above(painter, rect.x_range(), rect.bottom(), theme.hairline);
    let head = Rect::from_min_size(pos2(rect.left() + EDGE, rect.top() + 8.0), vec2(rect.width() - 2.0 * EDGE, 14.0));
    text::paint(painter, head, Align2::LEFT_CENTER, theme.text.micro, "You", theme.ink_faint);
    prose::paint(ui, pos2(rect.left() + EDGE, head.bottom() + 3.0), galley);
}

/// A passage of the assistant's reply, set from its Markdown. `open` says
/// it is still arriving.
fn passage(ui: &mut Ui, theme: &Theme, replies: &mut reply::Replies, index: usize, words: &str, open: bool) {
    ui.add_space(4.0);
    let width = ui.available_width();
    let laid = replies.laid(ui, theme, index, words, open, width, EDGE);
    let (rect, response) = ui.allocate_exact_size(vec2(width, laid.height), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("The assistant: {words}")));
    reply::paint(ui, theme, laid, rect.min, width, EDGE, ui.scope_id().with(("passage", index)));
    ui.add_space(5.0);
}

/// How a request ended: a quiet line in capitals.
fn mark(ui: &mut Ui, theme: &Theme, words: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    if ui.is_rect_visible(rect) {
        let room = rect.shrink2(vec2(EDGE, 0.0));
        text::paint(ui.painter(), room, Align2::LEFT_CENTER, theme.text.caps, words, theme.ink_faint);
    }
}

/// What a line of the story is of.
enum Line<'a> {
    Step(&'a Step),
    Look(&'a Look),
}

/// A step or a look: a mark for what it is, its words, and under them what
/// it heard. The one under way is lit.
fn line(ui: &mut Ui, theme: &Theme, entry: &Entry, what: Line<'_>, under_way: bool) {
    let (words, heard, failed) = match what {
        Line::Step(step) => (step.label.as_str(), step.heard.as_deref(), false),
        Line::Look(look) => (look.label.as_str(), look.heard.as_deref(), look.failed),
    };
    let height = if heard.is_some() { ROW + 17.0 } else { ROW };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let name = match (&what, heard, failed) {
        (_, _, true) => format!("{}: {words}: failed", who(&entry.by)),
        (_, Some(heard), _) => format!("{}: {words}: {heard}", who(&entry.by)),
        _ => format!("{}: {words}", who(&entry.by)),
    };
    response.widget_info(|| WidgetInfo::selected(Role::Label, true, under_way, &name));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let row = rect.with_max_y(rect.top() + ROW);
    if under_way {
        painter.rect_filled(rect, 0.0, theme.selected);
        painter.rect_filled(rect.with_max_x(rect.left() + 2.0), 0.0, theme.signal);
    }
    // A step is a filled mark in its author's color; one that took steps
    // back is hollow; a look is a small hollow one, and a call that failed
    // is marked in the color of a warning.
    let at = pos2(rect.left() + EDGE, row.center().y);
    let (color, style) = match &what {
        Line::Step(step) if step.reverses.is_some() => {
            paint::hollow_dot(painter, at, theme.metric.dot, hue(theme, &entry.by));
            (theme.ink_dim, TextStyle { size: 12.5, ..theme.text.body })
        }
        Line::Step(_) => {
            paint::dot(painter, at, theme.metric.dot, hue(theme, &entry.by));
            (theme.ink, TextStyle { size: 12.5, ..theme.text.body })
        }
        Line::Look(_) => {
            let color = if failed { theme.warn } else { theme.ghost };
            paint::hollow_dot(painter, at, theme.metric.dot, color);
            (theme.ink_faint, TextStyle { size: 12.0, ..theme.text.body })
        }
    };
    let room = Rect::from_min_max(pos2(rect.left() + INDENT, row.top()), pos2(rect.right() - EDGE, row.bottom()));
    text::paint(painter, room, Align2::LEFT_CENTER, style, words, color);
    if let Some(heard) = heard {
        let room = Rect::from_min_max(
            pos2(rect.left() + INDENT, row.bottom() - 3.0),
            pos2(rect.right() - EDGE, rect.bottom()),
        );
        text::paint(painter, room, Align2::LEFT_CENTER, theme.text.value, heard, theme.ink_dim);
    }
}

/// The mark before a step of the plan: an empty box for one still to do, a
/// box with its middle filled for the one in hand, a tick for one done.
fn plan_mark(painter: &egui::Painter, center: egui::Pos2, state: PlanState, theme: &Theme) {
    let center = pos2(center.x.round(), center.y.round());
    let frame = Rect::from_center_size(center, vec2(9.0, 9.0));
    match state {
        PlanState::Pending => paint::frame(painter, frame, theme.ink_faint),
        PlanState::Doing => {
            paint::frame(painter, frame, theme.info);
            painter.rect_filled(Rect::from_center_size(center, vec2(3.0, 3.0)), 0.0, theme.info);
        }
        PlanState::Done => {
            let stroke = Stroke::new(1.5, theme.ok);
            let (low, turn, high) = (center + vec2(-4.0, 0.0), center + vec2(-1.0, 3.0), center + vec2(4.5, -3.5));
            painter.line_segment([low, turn], stroke);
            painter.line_segment([turn, high], stroke);
        }
    }
}

/// The assistant's plan as it stood: how far it had come, and the steps
/// around the one in hand.
fn plan_block(ui: &mut Ui, theme: &Theme, rect: Rect, items: &[PlanItem]) {
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, theme.panel);
    paint::hline(&painter, rect.x_range(), rect.top(), theme.hairline);
    let done = items.iter().filter(|item| item.state == PlanState::Done).count();
    let count = format!("{done} of {}", items.len());
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), PLAN_HEAD));
    let response = ui.interact(head, ui.scope_id().with("making-of-plan"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("Plan: {count} done")));
    let mut room = head.shrink2(vec2(EDGE, 0.0));
    room.min.x += text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.caps, "Plan", theme.ink) + 10.0;
    text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, &count, theme.ink_faint);

    // As many steps as fit, beginning a step before the one in hand.
    let rows = (((rect.bottom() - 6.0 - head.bottom()) / PLAN_STEP).floor().max(0.0) as usize).min(items.len());
    let in_hand = items.iter().position(|item| item.state != PlanState::Done).unwrap_or(items.len());
    let first = in_hand.saturating_sub(1).min(items.len() - rows);
    let style = TextStyle { size: 12.0, ..theme.text.body };
    for (place, item) in items.iter().skip(first).take(rows).enumerate() {
        let row = Rect::from_min_size(
            pos2(rect.left(), head.bottom() + place as f32 * PLAN_STEP),
            vec2(rect.width(), PLAN_STEP),
        );
        let (state, color) = match item.state {
            PlanState::Done => ("done", theme.ink_faint),
            PlanState::Doing => ("in hand", theme.ink),
            PlanState::Pending => ("to do", theme.ink_dim),
        };
        let response = ui.interact(row, ui.scope_id().with(("making-of-plan-step", first + place)), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{}: {state}", item.text)));
        plan_mark(&painter, pos2(row.left() + EDGE + 4.0, row.center().y), item.state, theme);
        let room =
            Rect::from_min_max(pos2(row.left() + INDENT + 4.0, row.top()), pos2(row.right() - EDGE, row.bottom()));
        text::paint(&painter, room, Align2::LEFT_CENTER, style, &item.text, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_arrives_a_letter_at_a_time_and_stands_before_its_moment_ends() {
        let moment = Moment { entry: 0, starts: 0.0, lasts: 2.0, away: None, arrivals: 0 };
        let words = "I\u{2019}ll add a b\u{00E4}ss line.";
        assert_eq!(arriving(words, &moment, 0.0), Some(""));
        assert_eq!(arriving(words, &moment, 0.4), Some("I\u{2019}ll add a "));
        assert_eq!(
            arriving(words, &moment, 0.48),
            Some("I\u{2019}ll add a b\u{00E4}"),
            "whole letters, whatever they take"
        );
        assert_eq!(arriving(words, &moment, 0.8), None, "all there with a fifth of its time to spare");
        assert_eq!(arriving(words, &moment, 1.0), None);
        let instant = Moment { lasts: 0.0, ..moment };
        assert_eq!(arriving(words, &instant, 0.0), None);
    }
}
