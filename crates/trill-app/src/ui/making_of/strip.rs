//! The session as a line, along the bottom of the making-of: every step
//! a mark in its author's color, where it falls in the replay; what was
//! said, taller; what was only looked at, a speck; the replay's place
//! among them in the one loud color. A click goes there and a drag scrubs.
//!
//! Above the line, the entry under way in words, with who made it.

use super::{Watching, hue, who};
use eframe::egui::{self, Align2, Rect, Sense, Ui, WidgetInfo, pos2, vec2};
use trill_core::journal::replay::Moment;
use trill_core::journal::{Author, Entry, What};
use trill_noir::{TextStyle, Theme, fade, paint, text};

pub const HEIGHT: f32 = 54.0;
const EDGE: f32 = 12.0;
/// Height of the line that says what is under way.
const WORDS: f32 = 22.0;
/// How dim what the replay has not come to yet is drawn.
const AHEAD: f32 = 0.38;

/// The mark an entry leaves on the line: how far above and below its
/// middle it reaches, and in what color. `None` for what leaves none.
fn mark_of(theme: &Theme, entry: &Entry) -> Option<(f32, f32, egui::Color32)> {
    if entry.is_aside() {
        return None;
    }
    match &entry.what {
        What::Step(step) if step.reverses.is_some() => Some((1.0, 5.0, hue(theme, &entry.by))),
        What::Step(_) => Some((4.0, 5.0, hue(theme, &entry.by))),
        What::Said { .. } if entry.by == Author::Person => Some((10.0, 10.0, theme.ink)),
        What::Said { .. } => Some((10.0, -6.0, theme.ink_dim)),
        What::Look(look) if look.failed => Some((1.0, 1.0, theme.warn)),
        What::Look(_) => Some((1.0, 1.0, theme.ink_faint)),
        What::Plan { .. } | What::Mark { .. } => None,
    }
}

pub fn show(ui: &mut Ui, theme: &Theme, watching: &mut Watching) {
    let area = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(area, 0.0, theme.panel);
    paint::hline(&painter, area.x_range(), area.top(), theme.hairline);

    let length = watching.replay.length().max(f64::MIN_POSITIVE);
    let line = Rect::from_min_max(
        pos2(area.left() + EDGE, area.top() + WORDS),
        pos2(area.right() - EDGE, area.bottom() - 4.0),
    );
    let x_of = |time: f64| line.left() + (time / length).clamp(0.0, 1.0) as f32 * line.width();
    let time_of = |x: f32| f64::from(((x - line.left()) / line.width().max(1.0)).clamp(0.0, 1.0)) * length;

    // Scrubbing first, so that what is drawn is where the pointer put it.
    let response =
        ui.interact(line.expand2(vec2(EDGE, 4.0)), ui.scope_id().with("making-of-line"), Sense::click_and_drag());
    let fraction = watching.time() / length;
    response.widget_info(|| WidgetInfo::slider(true, fraction, "Session"));
    if let Some(pointer) = response.interact_pointer_pos().filter(|_| response.clicked() || response.dragged()) {
        watching.seek(time_of(pointer.x));
    }
    let over = response.hover_pos().and_then(|pointer| watching.replay.moment_at(time_of(pointer.x)));

    // What is under way, in words; while the pointer is over the line,
    // what it points at instead.
    let moments = watching.replay.moments();
    let told = over.or(watching.moment()).map(|index| (&moments[index], watching.replay.entry(&moments[index])));
    let words =
        Rect::from_min_max(pos2(area.left() + EDGE, area.top() + 1.0), pos2(area.right() - EDGE, area.top() + WORDS));
    match told {
        Some((_, entry)) => {
            let mut room = words;
            room.min.x =
                paint::dot(&painter, pos2(room.left(), room.center().y), theme.metric.dot, hue(theme, &entry.by)) + 6.0;
            room.min.x +=
                text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, &who(&entry.by), theme.ink_faint)
                    + 10.0;
            let style = TextStyle { size: 12.5, ..theme.text.body };
            // The plan is told by the step it had in hand.
            let line = match &entry.what {
                What::Plan { items } => {
                    let in_hand = items.iter().find(|item| item.state != trill_core::journal::PlanState::Done);
                    in_hand.map_or_else(|| "The plan is done".to_owned(), |item| format!("Next: {}", item.text))
                }
                _ => entry.line(),
            };
            let first = line.lines().next().unwrap_or_default();
            text::paint(&painter, room, Align2::LEFT_CENTER, style, first, theme.ink);
        }
        None => {
            text::paint(
                &painter,
                words,
                Align2::LEFT_CENTER,
                theme.text.micro,
                "The song as it began",
                theme.ink_faint,
            );
        }
    }

    // The line and its marks. Many entries share a pixel in a long
    // session: one mark is drawn for each pixel and kind.
    let middle = (line.center().y + 1.0).round();
    paint::hline(&painter, line.x_range(), middle, theme.hairline);
    let now = watching.time();
    let mut drawn: Option<(i32, f32)> = None;
    for moment in moments {
        let entry = watching.replay.entry(moment);
        let Some((up, down, color)) = mark_of(theme, entry) else { continue };
        let left = x_of(moment.starts).round();
        if drawn == Some((left as i32, up)) {
            continue;
        }
        drawn = Some((left as i32, up));
        let color = if moment.starts <= now { color } else { fade(color, AHEAD) };
        // A step is as wide as the time it is given; the rest are hairs.
        let width = match &entry.what {
            What::Step(_) => (x_of(moment.ends()) - x_of(moment.starts) - 1.0).max(1.0).round(),
            _ => 1.0,
        };
        let mark = Rect::from_min_max(pos2(left, middle - up), pos2(left + width, middle + down));
        painter.rect_filled(mark, 0.0, color);
        if moment.away.is_some() && left > line.left() + 2.0 {
            // Someone went away here: a break in the line.
            painter.rect_filled(
                Rect::from_min_max(pos2(left - 4.0, middle - 1.0), pos2(left - 1.0, middle + 2.0)),
                0.0,
                theme.panel,
            );
        }
    }

    // Where the replay is.
    let head = x_of(now).round();
    paint::vline(&painter, head, (line.top() + 2.0)..=line.bottom(), theme.signal);
    painter.rect_filled(Rect::from_min_size(pos2(head - 2.0, line.top() + 2.0), vec2(5.0, 3.0)), 0.0, theme.signal);

    let pointed = told.filter(|_| over.is_some()).map(|(moment, entry)| pointed_at(moment, entry));
    if let Some(pointed) = pointed {
        watching.hint().set(pointed);
    }
}

/// What the status bar says of the entry the pointer is over.
fn pointed_at(moment: &Moment, entry: &Entry) -> String {
    let what = match &entry.what {
        What::Step(_) => "a step",
        What::Said { .. } => "said",
        What::Look(_) => "a look",
        What::Plan { .. } => "the plan",
        What::Mark { .. } => "a note",
    };
    let away =
        moment.away.map(|away| format!(" \u{00B7} {} after the one before", super::span(away))).unwrap_or_default();
    format!(
        "Entry {} \u{00B7} {what} by {}{away} \u{00B7} click to go there, drag to scrub",
        entry.seq,
        entry.by.name()
    )
}
