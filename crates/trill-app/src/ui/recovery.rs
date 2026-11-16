//! Unsaved work that outlived the program: the offer to bring it back.
//!
//! While a project has unsaved changes the runtime keeps a copy of it, and
//! a program that dies leaves that copy behind. The next one to start finds
//! it ([`Runtime::recoverable`]) and this is where the user is asked what
//! to do with it: once, when the window opens, and afterwards for as long
//! as there is anything left, from the File menu.
//!
//! Three answers. *Recover* opens the copy in place of the open project
//! (asking first if that one has unsaved changes of its own); it comes
//! back with unsaved changes, where its file was but not written there.
//! *Discard* deletes the copy for good, and so asks first. *Decide later*
//! closes the offer and touches nothing: the copies stay. Nothing here ever
//! deletes a copy the user did not say to delete.
//!
//! Recovering loads a project and discarding deletes files, so both run on
//! a worker thread; the offer waits, and the window paints on.
//!
//! [`Runtime::recoverable`]: trill_runtime::Runtime::recoverable

use super::state::{Cx, UiState};
use super::widgets;
use eframe::egui::{self, Align, Align2, Layout, Rect, Role, Sense, UiBuilder, WidgetInfo, pos2, vec2};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, SystemTime};
use trill_noir::{TextStyle, Variant, paint, text};
use trill_runtime::SafetyCopy;

/// Width of the offer.
const WIDTH: f32 = 560.0;
/// Height of one lost project in it: two lines of text and room around.
const ROW: f32 = 46.0;
/// More lost projects than this scroll.
const MAX_ROWS: usize = 6;

/// What the offer shows.
#[derive(Clone, Debug, PartialEq)]
enum View {
    Closed,
    /// The list of what was lost.
    Offer,
    /// "Delete this copy for good?"
    ConfirmDiscard(SafetyCopy),
    /// "The open project has unsaved changes. Replace it?"
    ConfirmReplace(SafetyCopy),
}

/// What a worker reports. Either way it brings the list as it stands now.
enum Outcome {
    Recovered { name: String, result: Result<(), String>, left: Vec<SafetyCopy> },
    Discarded { name: String, left: Vec<SafetyCopy> },
}

/// The lost projects and where the conversation about them stands.
pub struct Recovery {
    /// What was lost and has not been dealt with, most recent first.
    copies: Vec<SafetyCopy>,
    view: View,
    /// A worker is recovering or discarding; the buttons wait for it.
    busy: bool,
    sender: Sender<Outcome>,
    receiver: Receiver<Outcome>,
}

impl Recovery {
    /// Starts with what the runtime found. If that is anything, the offer
    /// is up when the window opens.
    pub fn new(copies: Vec<SafetyCopy>) -> Self {
        let (sender, receiver) = channel();
        let view = if copies.is_empty() { View::Closed } else { View::Offer };
        Self { copies, view, busy: false, sender, receiver }
    }

    /// True while the offer, or a question it asked, is on screen: the
    /// keyboard is then its own.
    pub fn is_open(&self) -> bool {
        self.view != View::Closed
    }

    /// How many lost projects wait to be dealt with.
    pub fn count(&self) -> usize {
        self.copies.len()
    }

    /// Takes in what the workers finished. Returns true if the open
    /// project was replaced by a recovered one.
    pub fn poll(&mut self, state: &mut UiState) -> bool {
        let mut replaced = false;
        for outcome in self.receiver.try_iter() {
            self.busy = false;
            match outcome {
                Outcome::Recovered { name, result: Ok(()), left } => {
                    self.copies = left;
                    self.view = View::Closed;
                    replaced = true;
                    state.reset_for_new_project();
                    state.toasts.success(format!("Recovered {name}. It is not saved yet: save it to keep it."));
                }
                Outcome::Recovered { name, result: Err(reason), left } => {
                    // The copy is kept: it may yet be opened by hand.
                    self.copies = left;
                    self.view = View::Offer;
                    state.toasts.error(format!("{name} could not be recovered: {reason}"));
                }
                Outcome::Discarded { name, left } => {
                    self.copies = left;
                    self.view = if self.copies.is_empty() { View::Closed } else { View::Offer };
                    state.toasts.info(format!("Discarded the unsaved copy of {name}"));
                }
            }
        }
        replaced
    }
}

/// Puts the offer up again: File, Recover unsaved work. The list is read
/// afresh, since another front end may have dealt with some of it.
pub fn open(cx: &mut Cx) {
    cx.recovery.copies = cx.sounds.runtime.recoverable();
    if cx.recovery.copies.is_empty() {
        cx.state.toasts.info("There is no unsaved work to recover");
    } else {
        cx.recovery.view = View::Offer;
    }
}

/// Opens `copy` in place of the open project, on a worker.
fn recover(cx: &mut Cx, copy: SafetyCopy) {
    cx.recovery.busy = true;
    cx.recovery.view = View::Offer;
    let sender = cx.recovery.sender.clone();
    cx.sounds.quietly(move |runtime| {
        let result = runtime.recover(&copy).map_err(|error| error.to_string());
        let outcome = Outcome::Recovered { name: copy.name, result, left: runtime.recoverable() };
        // The receiver is gone only when the window has closed.
        let _ = sender.send(outcome);
        Ok(())
    });
}

/// Deletes `copy` for good, on a worker. Only ever called on the user's
/// say-so, given twice.
fn discard(cx: &mut Cx, copy: SafetyCopy) {
    cx.recovery.busy = true;
    cx.recovery.view = View::Offer;
    let sender = cx.recovery.sender.clone();
    cx.sounds.quietly(move |runtime| {
        runtime.discard(&copy);
        let _ = sender.send(Outcome::Discarded { name: copy.name, left: runtime.recoverable() });
        Ok(())
    });
}

/// How long ago `made` was, in the words of someone asked: to the minute
/// within the hour, to the hour within the day, then in days.
fn age(made: SystemTime, now: SystemTime) -> String {
    let seconds = now.duration_since(made).unwrap_or(Duration::ZERO).as_secs();
    let count = |amount: u64, unit: &str| format!("{amount} {unit}{} ago", if amount == 1 { "" } else { "s" });
    match seconds {
        0..60 => "Moments ago".to_owned(),
        60..3600 => count(seconds / 60, "minute"),
        3600..86_400 => count(seconds / 3600, "hour"),
        _ => count(seconds / 86_400, "day"),
    }
}

/// Where a lost project lived, or that it lived nowhere yet.
fn place(copy: &SafetyCopy) -> String {
    match &copy.original {
        Some(path) => path.display().to_string(),
        None => "Never saved".to_owned(),
    }
}

/// What the user asked for in the offer.
enum Asked {
    Recover(SafetyCopy),
    Discard(SafetyCopy),
    Later,
}

/// Shows the offer, or the question it led to, while there is one, and
/// carries out the answer.
pub fn modal(ctx: &egui::Context, cx: &mut Cx) {
    match cx.recovery.view.clone() {
        View::Closed => {}
        View::Offer => match offer(ctx, cx) {
            Some(Asked::Recover(copy)) if cx.snap.modified => cx.recovery.view = View::ConfirmReplace(copy),
            Some(Asked::Recover(copy)) => recover(cx, copy),
            Some(Asked::Discard(copy)) => cx.recovery.view = View::ConfirmDiscard(copy),
            Some(Asked::Later) => cx.recovery.view = View::Closed,
            None => {}
        },
        View::ConfirmDiscard(copy) => {
            let body = format!(
                "The unsaved copy of \"{}\" will be deleted. It cannot be brought back; what was last saved to its \
                 file, if it has one, stays as it is.",
                copy.name
            );
            let title = "Discard this unsaved work?";
            match widgets::question(ctx, cx.theme, "discard-lost-work", title, &body, "Discard for good", "Keep it") {
                Some(true) => discard(cx, copy),
                Some(false) => cx.recovery.view = View::Offer,
                None => {}
            }
        }
        View::ConfirmReplace(copy) => {
            let body = format!(
                "\"{}\" has changes that are not saved. They will be lost by opening the recovered project in its \
                 place.",
                cx.snap.project.name
            );
            let title = "Discard unsaved changes?";
            match widgets::question(
                ctx,
                cx.theme,
                "replace-with-lost-work",
                title,
                &body,
                "Discard and recover",
                "Cancel",
            ) {
                Some(true) => recover(cx, copy),
                Some(false) => cx.recovery.view = View::Offer,
                None => {}
            }
        }
    }
}

/// The list of lost projects, each with its two buttons. Returns what was
/// asked for, if anything was.
fn offer(ctx: &egui::Context, cx: &mut Cx) -> Option<Asked> {
    let theme = cx.theme;
    let recovery = &*cx.recovery;
    let now = SystemTime::now();
    let mut asked = None;
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(14, 12));
    let modal =
        egui::Modal::new(egui::Id::unique("lost-work")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
            let (title, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
            text::paint(
                ui.painter(),
                title,
                Align2::LEFT_CENTER,
                theme.text.caps,
                "Unsaved work was found",
                theme.ink_dim,
            );
            ui.add_space(6.0);
            let (rule, _) = ui.allocate_exact_size(vec2(WIDTH, 1.0), Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            ui.add_space(10.0);
            let intro = if recovery.copies.len() == 1 {
                "Trill stopped before this project was saved. A copy of it was kept."
            } else {
                "Trill stopped before these projects were saved. A copy of each was kept."
            };
            ui.label(egui::RichText::new(intro).color(theme.ink));
            ui.add_space(8.0);

            let shown = recovery.copies.len().min(MAX_ROWS) as f32;
            egui::ScrollArea::vertical().max_height(shown * ROW).auto_shrink([false, true]).show(ui, |ui| {
                for (index, copy) in recovery.copies.iter().enumerate() {
                    let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
                    let when = age(copy.made, now);
                    // The row's words are painted; this is them for whoever
                    // cannot see paint.
                    let spoken = format!("{} \u{00B7} {} \u{00B7} {when}", copy.name, place(copy));
                    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &spoken));
                    paint::hline(ui.painter(), row.x_range(), row.top(), theme.hairline);
                    // The buttons at the right; what they act on fills the rest.
                    let buttons = Rect::from_min_max(pos2(row.right() - 190.0, row.top()), row.max);
                    let layout = Layout::right_to_left(Align::Center);
                    let mut cell =
                        ui.new_child(UiBuilder::new().id_salt(("lost", index)).max_rect(buttons).layout(layout));
                    cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
                    if recovery.busy {
                        cell.disable();
                    }
                    if trill_noir::button(&mut cell, theme, "Recover", Variant::Primary).clicked() {
                        asked = Some(Asked::Recover(copy.clone()));
                    }
                    if trill_noir::button(&mut cell, theme, "Discard\u{2026}", Variant::Default).clicked() {
                        asked = Some(Asked::Discard(copy.clone()));
                    }

                    let words =
                        Rect::from_min_max(row.min + vec2(0.0, 7.0), pos2(buttons.left() - 12.0, row.bottom() - 7.0));
                    let (upper, lower) = words.split_top_bottom_at_fraction(0.5);
                    // How long ago, at the right of the name's line.
                    let used =
                        text::paint(ui.painter(), upper, Align2::RIGHT_CENTER, theme.text.value, &when, theme.ink_dim);
                    let name = upper.with_max_x(upper.right() - used - 12.0);
                    text::paint(ui.painter(), name, Align2::LEFT_CENTER, theme.text.body, &copy.name, theme.ink);
                    // Where it lived: a path keeps both its ends when it is long.
                    let style = TextStyle { size: 11.5, ..theme.text.body };
                    text::paint_name(ui.painter(), lower, Align2::LEFT_CENTER, style, &place(copy), theme.ink_dim);
                }
            });
            let (rule, _) = ui.allocate_exact_size(vec2(WIDTH, 1.0), Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            ui.add_space(12.0);

            let (foot, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
            let note = if recovery.busy {
                "Working\u{2026}"
            } else {
                "Kept until you decide \u{00B7} File \u{203A} Recover unsaved work"
            };
            text::paint(ui.painter(), foot, Align2::LEFT_CENTER, theme.text.micro, note, theme.ink_faint);
            let layout = Layout::right_to_left(Align::Center);
            let mut cell = ui.new_child(UiBuilder::new().id_salt("later").max_rect(foot).layout(layout));
            if trill_noir::button(&mut cell, theme, "Decide later", Variant::Default).clicked() {
                asked = Some(Asked::Later);
            }
        });
    if modal.should_close() {
        asked.get_or_insert(Asked::Later);
    }
    asked
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ages_read_the_way_one_would_say_them() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ago = |seconds: u64| age(now - Duration::from_secs(seconds), now);
        assert_eq!(ago(0), "Moments ago");
        assert_eq!(ago(59), "Moments ago");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(3 * 60 + 40), "3 minutes ago");
        assert_eq!(ago(3600), "1 hour ago");
        assert_eq!(ago(23 * 3600 + 3599), "23 hours ago");
        assert_eq!(ago(86_400), "1 day ago");
        assert_eq!(ago(9 * 86_400), "9 days ago");
        // A clock that was set back since: not "in the future", just now.
        assert_eq!(age(now + Duration::from_secs(500), now), "Moments ago");
    }

    #[test]
    fn a_lost_project_says_where_it_lived() {
        let copy = |original: Option<&str>| SafetyCopy {
            name: "Night Shift".into(),
            original: original.map(PathBuf::from),
            made: SystemTime::UNIX_EPOCH,
            file: PathBuf::from("recovery/1.trill"),
        };
        assert_eq!(place(&copy(Some("songs/night shift.trill"))), "songs/night shift.trill");
        assert_eq!(place(&copy(None)), "Never saved");
    }

    #[test]
    fn the_offer_is_up_at_startup_only_when_there_is_something_to_offer() {
        let nothing = Recovery::new(Vec::new());
        assert!(!nothing.is_open() && nothing.count() == 0);
        let copy = SafetyCopy {
            name: "Night Shift".into(),
            original: None,
            made: SystemTime::UNIX_EPOCH,
            file: PathBuf::from("recovery/1.trill"),
        };
        let something = Recovery::new(vec![copy]);
        assert!(something.is_open());
        assert_eq!(something.count(), 1);
    }
}
