//! The tab of Setup that is about Trill itself: which Trill this is,
//! whether a newer one is out, and fetching it.
//!
//! A newer Trill is a thing that is fetched, so it is here with the other
//! things that are, under the same rules: nothing is fetched that was not
//! read first (the version, what is new, its size and where it comes
//! from stand over the button), nothing here waits on a network, and
//! whatever happens reads as a sentence.
//!
//! One thing Trill does by itself, where its user has not switched it
//! off: it looks whether a newer one is out when it starts, which fetches
//! a small text that is the same for everyone. What it found is one line
//! in the status bar and on an empty song, which opens this tab, and
//! which can be told to say no more of that version.
//!
//! A newer Trill that was fetched changes nothing until Trill is started
//! again: it goes in at a start, while no Trill runs, and never under a
//! song that is open.

use super::setup::{GAP, ROW, WIDTH, act, fact, heading, paragraph, row_button};
use super::state::Cx;
use eframe::egui::{self, Align2, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use std::time::SystemTime;
use trill_fetch::said_closely;
use trill_noir::{Chip, TextStyle, paint, text};
use trill_runtime::{Runtime, UpdateNotice};
use trill_update::{Came, Status, Step, Version};

/// What the tab shows, as a helper thread last read it.
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub status: Option<Status>,
    /// Whether Trill looks for a newer one when it starts.
    pub look_at_start: bool,
    /// The version nothing is said of by itself any more.
    pub quiet_about: Option<Version>,
}

impl Program {
    /// What there is to say of a newer Trill without being asked, as the
    /// runtime has it (`Runtime::update_notice`): for a made-up machine.
    pub fn notice(&self) -> Option<UpdateNotice> {
        let status = self.status.as_ref()?;
        let version = status.newer_version()?;
        if self.quiet_about.is_some_and(|quiet| quiet >= version) {
            return None;
        }
        Some(UpdateNotice { version, ready: matches!(status.step, Step::Ready(_)) })
    }
}

/// Reads what the tab shows.
pub(super) fn read(runtime: &Runtime) -> Program {
    let settings = runtime.settings().updates;
    Program {
        status: Some(runtime.updates().status()),
        look_at_start: settings.look_at_start,
        quiet_about: settings.quiet_about.and_then(|quiet| quiet.parse().ok()),
    }
}

/// How long ago something was, in the words of a status line.
fn ago(when: SystemTime) -> String {
    let seconds = when.elapsed().map_or(0, |since| since.as_secs());
    match seconds {
        0..=89 => "a moment ago".to_owned(),
        90..=5399 => format!("{} minutes ago", (seconds + 30) / 60),
        5400..=129_599 => format!("{} hours ago", (seconds + 1800) / 3600),
        _ => format!("{} days ago", (seconds + 43_200) / 86_400),
    }
}

/// Who publishes what is at an address: its host.
fn host(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, rest)| rest).split('/').next().unwrap_or(url)
}

/// A line of body text.
fn line(ui: &mut Ui, cx: &Cx, words: &str, color: egui::Color32) -> Rect {
    let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    text::paint(ui.painter(), row, Align2::LEFT_CENTER, cx.theme.text.body, words, color);
    row
}

/// The tab: this Trill, the newer one where there is one, and how Trill
/// looks for one.
pub(super) fn body(ui: &mut Ui, ctx: &egui::Context, cx: &mut Cx) {
    let theme = cx.theme;
    let program = cx.setup.program().clone();
    let Some(status) = program.status.clone() else {
        paragraph(ui, theme, theme.ink_dim, "Looking at what is here\u{2026}", WIDTH);
        return;
    };
    let busy = cx.setup.is_working();

    // Which Trill this is, and the button that looks for a newer one.
    let row = heading(ui, theme, "This Trill", "");
    let can_look = status.off.is_none();
    if can_look {
        let hint = "Fetch the announcement of the newest release, a small text that is the same for everyone, and read it under its makers' signature";
        let looking = status.looking || busy;
        if row_button(ui, cx, row, "Look now", "Look for a newer Trill", looking, hint) {
            act(ctx, cx, |runtime| {
                runtime.updates().look().map(|newer| match newer {
                    Some(release) => format!("Trill {} is out", release.version),
                    None => "This is the newest Trill".to_owned(),
                })
            });
        }
    }
    line(ui, cx, &format!("{}.", status.this.words()), theme.ink);
    if let Some(why) = &status.off {
        let mut words: String = why.clone();
        if let Some(first) = words.get(..1) {
            words = format!("{}{}.", first.to_uppercase(), &words[1..]);
        }
        paragraph(ui, theme, theme.ink_dim, &words, WIDTH);
        return;
    }

    // The newer one, as far as it is on its way.
    ui.add_space(4.0);
    match (&status.step, &status.newer) {
        (Step::Ready(version), _) => {
            let words = format!(
                "Trill {version} is fetched. It goes in the next time Trill starts while no other Trill is open: \
                 save your song, close every Trill (one that an AI client started too), and start Trill again. \
                 Should {version} not start here, this one is put back by itself."
            );
            paragraph(ui, theme, theme.ink, &words, WIDTH);
            ui.add_space(GAP);
            let row = line(ui, cx, "Nothing has changed yet", theme.ink_dim);
            let hint = "Put away what was fetched: Trill stays as it is until it is fetched again";
            if row_button(ui, cx, row, "Put it away", "Put the fetched update away", busy, hint) {
                act(ctx, cx, |runtime| {
                    runtime.updates().forget();
                    Ok("The update was put away. Trill stays as it is".to_owned())
                });
            }
        }
        (Step::Fetching { got, of }, newer) => {
            let version = newer.as_ref().map_or_else(String::new, |release| format!(" {}", release.version));
            let words = format!("Fetching Trill{version} \u{00B7} {} of {}", said_closely(*got), said_closely(*of));
            let row = line(ui, cx, &words, theme.ink);
            let bar = Rect::from_min_max(pos2(row.left(), row.bottom() - 2.0), row.max);
            let share = if *of == 0 { 0.0 } else { (*got as f64 / *of as f64) as f32 };
            paint::progress(ui.painter(), bar, share, theme.well, theme.signal);
            if row_button(
                ui,
                cx,
                row,
                "Stop",
                "Stop fetching the update",
                false,
                "What has come is kept, and is gone on with",
            ) {
                cx.sounds.runtime.updates().stop();
            }
        }
        (Step::Idle, Some(release)) => {
            ui.add_space(6.0);
            // (The button stands at the right end of the heading, so the day it was made is a line below.)
            let row = heading(ui, theme, &format!("Trill {} is out", release.version), "");
            let build = status.build();
            if build.is_some() {
                let hint = "Fetch it, hold it to what its makers signed, and keep it ready: nothing changes until Trill is started again";
                let label = format!("Fetch {}", release.version);
                if row_button(ui, cx, row, &label, "Fetch the newer Trill", busy, hint) {
                    act(ctx, cx, |runtime| {
                        runtime.updates().fetch().map(|came| match came {
                            Came::Ready(version) => format!(
                                "Trill {version} is fetched: it goes in the next time Trill starts while no other is open"
                            ),
                            Came::Stopped => String::new(),
                        })
                    });
                }
            }
            if !release.notes.is_empty() {
                paragraph(ui, theme, theme.ink, &release.notes, WIDTH);
                ui.add_space(8.0);
            }
            if !release.date.is_empty() {
                fact(ui, theme, "Made", &release.date, theme.ink_dim);
            }
            match build {
                Some(build) => {
                    fact(ui, theme, "Size", &said_closely(build.size), theme.ink_dim);
                    fact(ui, theme, "From", host(&build.url), theme.ink_dim);
                }
                None => {
                    let words = "There is no build of it for this kind of machine: its page says what there is.";
                    fact(ui, theme, "Size", words, theme.warn);
                }
            }
            if !release.page.is_empty() {
                fact(ui, theme, "Its page", &release.page, theme.ink_dim);
            }
        }
        (Step::Idle, None) => {
            let words = match (status.looking, status.looked) {
                (true, _) => "Looking whether a newer Trill is out\u{2026}".to_owned(),
                (false, Some(looked)) => {
                    format!("This is the newest Trill its makers have announced. Looked {}.", ago(looked))
                }
                (false, None) => "Trill has not looked yet whether a newer one is out.".to_owned(),
            };
            paragraph(ui, theme, theme.ink_dim, &words, WIDTH);
        }
    }
    if let Some(problem) = &status.problem {
        ui.add_space(6.0);
        let mut words = problem.clone();
        if !words.ends_with(['.', '!', '?']) {
            words.push('.');
        }
        paragraph(ui, theme, theme.warn, &words, WIDTH);
    }

    // How Trill looks, and what it says by itself.
    ui.add_space(14.0);
    heading(ui, theme, "Looking for a newer Trill", "");
    let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let on = program.look_at_start;
    let words = "Look when Trill starts";
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    text::paint(ui.painter(), row, Align2::LEFT_CENTER, theme.text.body, words, theme.ink);
    let chip = Chip::new(if on { "On" } else { "Off" }, on).dimmed(busy).named("Whether Trill looks when it starts");
    let width = chip.width(ui, theme);
    let rect = Rect::from_min_size(
        pos2(row.right() - width, row.center().y - theme.metric.chip / 2.0),
        vec2(width, theme.metric.chip),
    );
    let response = trill_noir::chip(ui, theme, rect, ui.scope_id().with("look-at-start"), &chip);
    cx.hint(&response, "Whether Trill looks for a newer one by itself. Switched off, it looks only when you ask here");
    if response.clicked() && !busy {
        act(ctx, cx, move |runtime| {
            runtime.set_look_at_start(!on).map_err(|error| error.to_string())?;
            Ok(String::new())
        });
    }
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let about = "Looking fetches one small text that is the same for everyone, and says nothing of you, your songs or \
                 this computer. A newer Trill itself is fetched only when you ask for it here.";
    let galley = trill_noir::prose::plain(ui, theme, style, theme.ink_faint, about, WIDTH);
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, galley.size().y), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, about));
    trill_noir::prose::paint(ui, rect.min, galley);

    // The line in the status bar, and silencing it.
    if let Some(version) = status.newer_version() {
        ui.add_space(GAP);
        let quiet = program.quiet_about.is_some_and(|quiet| quiet >= version);
        let words = match quiet {
            true => format!("Nothing points at Trill {version} by itself any more"),
            false => format!("The status bar says that Trill {version} is there"),
        };
        let row = line(ui, cx, &words, theme.ink_dim);
        let (label, name, hint) = match quiet {
            true => ("Say it again", "Say again that a newer Trill is out", "Have the status bar say it again"),
            false => (
                "Say no more of it",
                "Say no more of this version",
                "Take the line out of the status bar. A version after this one is said again",
            ),
        };
        if row_button(ui, cx, row, label, name, busy, hint) {
            let about = (!quiet).then_some(version);
            act(ctx, cx, move |runtime| {
                runtime.keep_quiet_about(about).map_err(|error| error.to_string())?;
                Ok(String::new())
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn how_long_ago_reads_in_the_unit_that_fits() {
        let before = |seconds: u64| SystemTime::now() - Duration::from_secs(seconds);
        assert_eq!(ago(before(5)), "a moment ago");
        assert_eq!(ago(before(600)), "10 minutes ago");
        assert_eq!(ago(before(3 * 3600)), "3 hours ago");
        assert_eq!(ago(before(4 * 86_400)), "4 days ago");
    }

    #[test]
    fn an_address_says_who_publishes_what_is_there() {
        assert_eq!(host("https://github.com/ambb205/trill/releases/download/v0.3.0/trill.zip"), "github.com");
        assert_eq!(host("example.org"), "example.org");
    }
}
