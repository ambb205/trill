//! Credits: what this song owes for the sampled instruments it plays, and
//! the makers Trill names for the instruments it offers.
//!
//! Some of the sets of the catalogue (`trill-catalogue`) are published on
//! the condition that their maker is named. Someone who releases a song
//! made with one should not have to work out whom: the panel lists the
//! lines, each with the tracks it is for, and copies them. Under them it
//! says what Trill itself is made of in that respect: the makers who ask
//! to be named for the sets it offers. Trill ships none of their
//! recordings.
//!
//! What the song owes is worked out on a worker (`Sounds::owed`): it
//! reads the instruments' files.

use super::state::Cx;
use eframe::egui::{self, Align2, Rect, Sense, Ui, vec2};
use trill_catalogue::{Catalogue, Owed};
use trill_noir::{Chip, paint, text};

const WIDTH: f32 = 620.0;
const ROW: f32 = 18.0;
const HEADING: f32 = 22.0;
/// The most rows of the song's own lines that are shown before they scroll.
const SONG_ROWS: f32 = 12.0;

/// Opens the panel.
pub fn open(cx: &mut Cx) {
    cx.state.credits_open = true;
}

/// `words` broken at its spaces into rows no wider than `width`: a
/// maker's line is shown whole, however long it is.
fn rows_of(words: &str, width: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in words.split(' ') {
        let longer = if row.is_empty() { word.to_string() } else { format!("{row} {word}") };
        if !row.is_empty() && measure(&longer) > width {
            rows.push(std::mem::replace(&mut row, word.to_string()));
        } else {
            row = longer;
        }
    }
    rows.push(row);
    rows
}

/// A line of the panel, on as many rows as it takes.
fn line(ui: &mut Ui, words: &str, style: trill_noir::TextStyle, ink: egui::Color32) {
    let rows = rows_of(words, WIDTH, |row| text::width(ui.painter(), style, row));
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW * rows.len() as f32), Sense::hover());
    for (index, row) in rows.iter().enumerate() {
        let at = Rect::from_min_size(rect.min + vec2(0.0, ROW * index as f32), vec2(WIDTH, ROW));
        text::paint(ui.painter(), at, Align2::LEFT_CENTER, style, row, ink);
    }
    // One thing to whoever reads the screen, whatever rows it is drawn on.
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, words));
}

fn heading(ui: &mut Ui, cx: &Cx, words: &str) {
    let theme = cx.theme;
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, HEADING), Sense::hover());
    let label = rect.with_max_y(rect.bottom() - 3.0);
    text::paint(ui.painter(), label, Align2::LEFT_CENTER, theme.text.caps, words, theme.ink_dim);
    paint::hline_above(ui.painter(), rect.x_range(), rect.bottom(), theme.hairline);
    ui.add_space(4.0);
}

/// What the song owes, in the lines the panel shows: each credit with the
/// tracks it is for under it; then what nothing is owed for, and what the
/// catalogue does not know.
pub(super) fn song_lines(owed: &Owed) -> Vec<(String, bool)> {
    let mut lines = Vec::new();
    for (credit, uses) in &owed.lines {
        lines.push((credit.line.clone(), true));
        lines.push((format!("for {}", uses.join(", ")), false));
    }
    if owed.lines.is_empty() {
        lines.push((
            if owed.free.is_empty() && owed.unknown.is_empty() {
                "This song plays no sampled instrument, so nothing is owed for one.".to_string()
            } else {
                "No maker asks to be named for what this song plays.".to_string()
            },
            false,
        ));
    }
    if !owed.free.is_empty() {
        lines.push((format!("In the public domain, nothing owed: {}", owed.free.join(", ")), false));
    }
    if !owed.unknown.is_empty() {
        lines.push((format!("Not of Trill's catalogue, their own terms apply: {}", owed.unknown.join("; ")), false));
    }
    lines
}

/// The panel, while it is open.
pub fn window(ctx: &egui::Context, cx: &mut Cx) {
    if !cx.state.credits_open {
        return;
    }
    let theme = cx.theme;
    let owed = cx.sounds.owed(cx.snap.revision);
    let offered = Catalogue::shipped().credits();
    let mut close = false;
    let mut copy = None;
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(18, 14));
    let modal =
        egui::Modal::new(egui::Id::unique("credits")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (title, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
            text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, "Credits", theme.ink);
            text::paint(ui.painter(), title, Align2::RIGHT_CENTER, theme.text.micro, "Esc closes", theme.ink_faint);

            heading(ui, cx, "This song");
            match &owed {
                Some(owed) => {
                    // A song of many instruments scrolls here, and the makers below stay in sight.
                    egui::ScrollArea::vertical().max_height(SONG_ROWS * ROW).auto_shrink([false, true]).show(
                        ui,
                        |ui| {
                            for (words, credit) in song_lines(owed) {
                                let (style, ink) = if credit {
                                    (theme.text.body, theme.ink)
                                } else {
                                    (theme.text.micro, theme.ink_faint)
                                };
                                line(ui, &words, style, ink);
                            }
                        },
                    );
                    if !owed.lines.is_empty() {
                        ui.add_space(6.0);
                        let spec = Chip::action("Copy the lines");
                        let (rect, _) =
                            ui.allocate_exact_size(vec2(spec.width(ui, theme), theme.metric.chip), Sense::hover());
                        let response = trill_noir::chip(ui, theme, rect, ui.scope_id().with("copy-credits"), &spec);
                        cx.hint(&response, "Copy the credit lines, to paste where the song is released");
                        if response.clicked() {
                            copy = Some(owed.text());
                        }
                    }
                }
                None => line(ui, "Reading the song's instruments\u{2026}", theme.text.micro, theme.ink_faint),
            }

            heading(ui, cx, "Instruments Trill offers");
            let about = "Trill ships no recordings. These makers ask to be named for the sets a user may fetch:";
            line(ui, about, theme.text.micro, theme.ink_faint);
            ui.add_space(2.0);
            for credit in &offered {
                line(ui, &credit.line, theme.text.body, theme.ink_dim);
                ui.add_space(3.0);
            }
            let rest =
                "The other sets it offers are dedicated to the public domain. catalogue/CREDITS.md has this list.";
            ui.add_space(2.0);
            line(ui, rest, theme.text.micro, theme.ink_faint);
        });
    if modal.should_close() {
        close = true;
    }
    if let Some(words) = copy {
        ctx.copy_text(words);
        cx.state.toasts.info("Copied the credit lines");
    }
    if close {
        cx.state.credits_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_catalogue::{Credit, Licence};

    #[test]
    fn what_a_song_owes_is_said_line_by_line() {
        let nothing = Owed::default();
        assert_eq!(
            song_lines(&nothing),
            [("This song plays no sampled instrument, so nothing is owed for one.".into(), false)]
        );
        let credit = Credit {
            set: "small-grand".into(),
            name: "Small Grand".into(),
            line: "Small Grand by A. Maker (CC BY 4.0)".into(),
            licence: Licence::CcBy4,
            at: "https://example.org".into(),
        };
        let owed = Owed {
            lines: vec![(credit, vec!["Small Grand (Keys)".into(), "Small Grand (Keys 2)".into()])],
            free: vec!["Violins (Free Strings)".into()],
            unknown: vec!["Mine.sfz, on Pad".into()],
        };
        let lines = song_lines(&owed);
        assert_eq!(lines[0], ("Small Grand by A. Maker (CC BY 4.0)".into(), true));
        assert_eq!(lines[1], ("for Small Grand (Keys), Small Grand (Keys 2)".into(), false));
        assert_eq!(lines[2].0, "In the public domain, nothing owed: Violins (Free Strings)");
        assert_eq!(lines[3].0, "Not of Trill's catalogue, their own terms apply: Mine.sfz, on Pad");
        let free = Owed { free: vec!["Violins (Free Strings)".into()], ..Owed::default() };
        assert_eq!(song_lines(&free)[0].0, "No maker asks to be named for what this song plays.");

        // A line that is too long for the panel goes on to the next row at a space, and loses nothing.
        let seven_a_letter = |row: &str| row.chars().count() as f32 * 7.0;
        let rows = rows_of("Small Grand by A. Maker, licensed under CC BY 4.0", 140.0, seven_a_letter);
        assert_eq!(rows, ["Small Grand by A.", "Maker, licensed", "under CC BY 4.0"]);
        assert_eq!(rows_of("Short", 140.0, seven_a_letter), ["Short"]);
        assert_eq!(rows_of("Awordthatislongerthantherowis and", 70.0, seven_a_letter).len(), 2);
    }
}
