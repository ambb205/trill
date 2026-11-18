//! The Studies place of the browser, the dialog that asks for a recording
//! to study, and the question before a study goes.

use super::Question;
use crate::ui::state::Cx;
use crate::ui::widgets;
use eframe::egui::{self, Align2, Id, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use trill_noir::{Chip, TextStyle, paint, prose, text};
use trill_study::{Report, StageState, State};

/// Height of a study in the list: its name over what it is at.
const ROW: f32 = 36.0;
const DIALOG: f32 = 520.0;

fn clock(seconds: f64) -> String {
    let whole = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// What a study is at, in a few words, and how far through, where it runs.
fn state_of(report: &Report) -> (String, Option<f32>) {
    let study = &report.study;
    match (study.state, report.at) {
        (State::Running | State::Waiting, _) if report.behind_another => {
            ("Waits for a study another program is making".to_owned(), None)
        }
        (State::Running, Some((stage, share))) => (format!("{}, {:.0} %", stage.doing(), share * 100.0), Some(share)),
        (State::Running, None) => ("Starting\u{2026}".to_owned(), Some(0.0)),
        (State::Waiting, _) => ("Waits for its turn".to_owned(), None),
        (State::Done, _) => (format!("{}  \u{00B7}  {} MB", clock(study.seconds), report.size >> 20), None),
        (State::Stopped, _) => ("Stopped: it can be taken up where it was".to_owned(), None),
        (State::Failed, _) => ("It could not be studied".to_owned(), None),
    }
}

pub fn show(ui: &mut Ui, cx: &mut Cx, body: Rect) {
    let theme = cx.theme;
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    let id = ui.scope_id().with("studies");

    // Ask for one.
    let top = Rect::from_min_size(body.min, vec2(body.width(), 34.0));
    let chip = Chip::action("STUDY A RECORDING\u{2026}").named("Study a recording");
    let rect = Rect::from_min_size(
        pos2(top.left() + 10.0, top.center().y - theme.metric.chip / 2.0),
        vec2(chip.width(ui, theme), theme.metric.chip),
    );
    let response = trill_noir::chip(ui, theme, rect, id.with("ask"), &chip);
    cx.hint(&response, "Take a recording apart with models on this machine: its stems, bars, chords and words");
    if response.clicked() {
        cx.studies.dialog.open = true;
    }
    paint::hline_above(&painter, top.x_range(), top.bottom(), theme.hairline);

    let list: Vec<Report> = cx.studies.list().to_vec();
    let rows = Rect::from_min_max(pos2(body.left(), top.bottom()), body.max);
    if let Some(why) = cx.studies.unavailable().map(str::to_owned) {
        return sentence(ui, cx, rows, &why);
    }
    if list.is_empty() {
        return sentence(
            ui,
            cx,
            rows,
            "No recording has been studied yet. A study is made once and kept: its stems, its bars, its chords, its \
             parts and its words, to hear, to take from and to hold a song against.",
        );
    }

    // The chosen study's own lines stand under the list.
    let chosen = cx.studies.chosen.clone().and_then(|id| list.iter().find(|report| report.study.id == id).cloned());
    let foot_height = if chosen.is_some() { 132.0 } else { 0.0 };
    let foot = Rect::from_min_max(pos2(rows.left(), rows.bottom() - foot_height), rows.max);
    let rows = Rect::from_min_max(rows.min, pos2(rows.right(), foot.top()));
    let clipped = painter.with_clip_rect(rows.intersect(ui.clip_rect()));
    let open = cx.studies.open().map(|open| open.id.clone());
    for (index, report) in list.iter().enumerate() {
        let rect =
            Rect::from_min_size(pos2(rows.left(), rows.top() + index as f32 * ROW), vec2(rows.width() - 1.0, ROW));
        if rect.top() > rows.bottom() {
            break;
        }
        let study = &report.study;
        let response = ui.interact(rect.intersect(rows), id.with(&study.id), Sense::click());
        let is_chosen = cx.studies.chosen.as_deref() == Some(study.id.as_str());
        let (state, share) = state_of(report);
        let name = format!("{}: {state}", study.title);
        response.widget_info(|| WidgetInfo::selected(Role::Button, true, is_chosen, &name));
        if is_chosen {
            clipped.rect_filled(rect, 0.0, theme.raised);
        } else if response.hovered() {
            clipped.rect_filled(rect, 0.0, theme.hover);
        }
        let inside = rect.shrink2(vec2(10.0, 3.0));
        let (upper, lower) = inside.split_top_bottom_at_fraction(0.5);
        let shown = open.as_deref() == Some(study.id.as_str());
        let left = if shown {
            paint::dot(&clipped, upper.left_center(), theme.metric.dot, theme.signal) + 5.0
        } else {
            upper.left()
        };
        text::paint_name(
            &clipped,
            upper.with_min_x(left),
            Align2::LEFT_CENTER,
            theme.text.body,
            &study.title,
            theme.ink,
        );
        let ink = if study.state == State::Failed { theme.signal } else { theme.ink_faint };
        text::paint_name(&clipped, lower, Align2::LEFT_CENTER, theme.text.micro, &state, ink);
        if let Some(share) = share {
            let well = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2.0), rect.max);
            paint::progress(&clipped, well, share, theme.well, theme.ok);
        }
        paint::hline_above(&clipped, rect.x_range(), rect.bottom(), theme.hairline);
        cx.hint(&response, "A study \u{00B7} click for what it is at, double-click opens it");
        if response.clicked() {
            cx.studies.chosen = Some(study.id.clone());
        }
        if widgets::double_clicked(&response) && study.state == State::Done {
            super::open(&ctx, cx, &study.id);
        }
    }
    if let Some(report) = chosen {
        about(ui, cx, &report, foot);
    }
}

/// What the chosen study is at, stage by stage, and what can be done with it.
fn about(ui: &mut Ui, cx: &mut Cx, report: &Report, foot: Rect) {
    let theme = cx.theme;
    let ctx = ui.ctx().clone();
    let painter = ui.painter().with_clip_rect(foot.intersect(ui.clip_rect()));
    let id = ui.scope_id().with("study-about");
    painter.rect_filled(foot, 0.0, theme.panel);
    paint::hline(&painter, foot.x_range(), foot.top(), theme.hairline);
    let study = &report.study;
    let inside = foot.shrink2(vec2(10.0, 6.0));

    // The stages by name: done, running with how far, to come.
    let mut top = inside.top();
    for stage in &study.stages {
        let line = Rect::from_min_size(pos2(inside.left(), top), vec2(inside.width(), 13.0));
        let running = report.at.filter(|(at, _)| *at == stage.stage);
        let (mark, ink) = match (stage.state, running) {
            (_, Some((_, share))) => (format!("{:.0} %", share * 100.0), theme.ink),
            (StageState::Done, _) => ("DONE".to_owned(), theme.ink_faint),
            (StageState::Failed, _) => ("FAILED".to_owned(), theme.signal),
            (StageState::Running, _) => ("\u{2026}".to_owned(), theme.ink),
            _ => (String::new(), theme.ink_faint),
        };
        text::paint(&painter, line, Align2::LEFT_CENTER, theme.text.micro, stage.stage.doing(), ink);
        text::paint(&painter, line, Align2::RIGHT_CENTER, theme.text.micro, &mark, ink);
        top += 13.0;
    }
    // Why it failed, as a sentence.
    if let Some(error) = &study.error {
        let room = Rect::from_min_max(pos2(inside.left(), top + 2.0), pos2(inside.right(), inside.bottom() - 22.0));
        let response = ui.interact(room, id.with("error"), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, error));
        let style = TextStyle { size: 11.0, ..theme.text.body };
        let galley = prose::plain(ui, theme, style, theme.signal, error, room.width());
        prose::paint(&ui.new_child(egui::UiBuilder::new().max_rect(room)), room.min, galley);
        response.on_hover_text(error.clone());
    }

    // What can be done with it.
    let row = Rect::from_min_max(pos2(inside.left(), inside.bottom() - theme.metric.chip), inside.max);
    let mut cursor = row.left();
    let mut chip = |ui: &mut Ui, cx: &mut Cx, label: &str, name: &str, about: &str| -> bool {
        let chip = Chip::action(label).named(name);
        let width = chip.width(ui, cx.theme);
        let rect = Rect::from_min_size(pos2(cursor, row.top()), vec2(width, cx.theme.metric.chip));
        cursor += width + cx.theme.metric.gap - 1.0;
        let response = trill_noir::chip(ui, cx.theme, rect, id.with(("do", name)), &chip);
        cx.hint(&response, about);
        response.clicked()
    };
    match study.state {
        State::Done => {
            if chip(ui, cx, "OPEN", "Open the study", "Shows the study in the workspace along the bottom") {
                super::open(&ctx, cx, &study.id);
            }
            if !study.stems.is_empty()
                && chip(
                    ui,
                    cx,
                    "DROP STEMS",
                    "Drop the stems",
                    "Removes the stems and keeps what was found in them: a couple of hundred megabytes come free",
                )
            {
                super::ask_to_remove(&ctx, cx, study.id.clone(), study.title.clone(), true);
            }
        }
        State::Running | State::Waiting => {
            if chip(ui, cx, "STOP", "Stop the study", "Ends the study: every stage that had finished is kept") {
                super::stop(&ctx, cx, study.id.clone());
            }
        }
        State::Stopped | State::Failed => {
            if chip(ui, cx, "TAKE UP", "Take the study up again", "Goes on with the study at the stage it was at") {
                super::resume(&ctx, cx, study.id.clone());
            }
        }
    }
    if chip(ui, cx, "REMOVE", "Remove the study", "Removes the study and everything in its folder") {
        super::ask_to_remove(&ctx, cx, study.id.clone(), study.title.clone(), false);
    }
}

/// A sentence in place of the list.
fn sentence(ui: &mut Ui, cx: &mut Cx, rows: Rect, words: &str) {
    let theme = cx.theme;
    let room = rows.shrink2(vec2(10.0, 8.0));
    let response = ui.interact(room, ui.scope_id().with("studies-none"), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    let galley = prose::plain(ui, theme, theme.text.body, theme.ink_faint, words, room.width());
    prose::paint(ui, room.min, galley);
}

/// The dialog that asks for a recording, and the question before a study goes.
pub fn modal(ctx: &egui::Context, cx: &mut Cx) {
    // What is shown of studies is read while any of it is on screen.
    let listed = cx.state.browser.open && cx.state.browser.tab == crate::ui::state::BrowserTab::Studies;
    let laid_out = cx.state.detail_tab == crate::ui::state::DetailTab::Study || cx.studies.apart;
    super::poll(ctx, cx, listed || laid_out || cx.studies.dialog.open || cx.studies.question.is_some());
    if let Some(question) = cx.studies.question.take() {
        match ask(ctx, cx, &question) {
            Some(true) => super::remove(ctx, cx, question),
            Some(false) => {}
            None => cx.studies.question = Some(question),
        }
    }
    if !cx.studies.dialog.open {
        return;
    }
    let theme = cx.theme;
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(18, 14));
    let modal =
        egui::Modal::new(Id::unique("study-dialog")).frame(frame).backdrop_color(theme.backdrop).show(ctx, |ui| {
            ui.set_width(DIALOG);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let (title, _) = ui.allocate_exact_size(vec2(DIALOG, 18.0), Sense::hover());
            text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, "Study a recording", theme.ink);
            text::paint(ui.painter(), title, Align2::RIGHT_CENTER, theme.text.micro, "Esc closes", theme.ink_faint);
            ui.add_space(8.0);
            let (rule, _) = ui.allocate_exact_size(vec2(DIALOG, 1.0), Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            ui.add_space(10.0);
            paragraph(
                ui,
                cx,
                theme.ink_dim,
                "A study takes a recording apart with models that run on this machine: its stems, its beats and bars, \
             its key and chords, its parts and the words that are sung. It takes a minute or so on a graphics card \
             and is kept from then on.",
            );
            ui.add_space(10.0);

            // A file or a link, in one field.
            let (row, _) = ui.allocate_exact_size(vec2(DIALOG, theme.metric.control), Sense::hover());
            let browse = Chip::action("BROWSE\u{2026}").named("Choose a file");
            let browse_width = browse.width(ui, theme);
            let field = Rect::from_min_max(row.min, pos2(row.right() - browse_width - 8.0, row.bottom()));
            let mut of = std::mem::take(&mut cx.studies.dialog.of);
            let typed = trill_noir::text_field(
                ui,
                theme,
                field,
                Id::unique("study-of"),
                &mut of,
                "A file, or a link to a recording",
            );
            typed.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, "A file, or a link to a recording"));
            cx.studies.dialog.of = of;
            let chip_rect = Rect::from_min_size(
                pos2(field.right() + 8.0, row.center().y - theme.metric.chip / 2.0),
                vec2(browse_width, theme.metric.chip),
            );
            if trill_noir::chip(ui, theme, chip_rect, Id::unique("study-browse"), &browse).clicked() {
                super::pick_file(ctx, cx);
            }
            ui.add_space(10.0);

            // Whose a recording behind a link is: said before it is fetched.
            let of = cx.studies.dialog.of.trim();
            if of.starts_with("http://") || of.starts_with("https://") {
                paragraph(ui, cx, theme.ink, trill_study::desk::WHOSE);
                ui.add_space(10.0);
            }

            let (row, _) = ui.allocate_exact_size(vec2(DIALOG, theme.metric.chip), Sense::hover());
            let mut cursor = row.left();
            for (on, label, name) in [
                (cx.studies.dialog.words, "WRITE DOWN THE WORDS", "Write down the words"),
                (cx.studies.dialog.best_stems, "BEST STEMS, FOUR TIMES THE WORK", "Best stems"),
            ] {
                let chip = Chip::new(label, on).named(name);
                let width = chip.width(ui, theme);
                let rect = Rect::from_min_size(pos2(cursor, row.top()), vec2(width, theme.metric.chip));
                cursor += width + theme.metric.gap;
                if trill_noir::chip(ui, theme, rect, Id::unique(("study-option", name)), &chip).clicked() {
                    match name {
                        "Best stems" => cx.studies.dialog.best_stems = !on,
                        _ => cx.studies.dialog.words = !on,
                    }
                }
            }
            ui.add_space(12.0);

            // Why the last attempt came to nothing: what is missing, and what to do.
            if let Some(why) = cx.studies.dialog.refused.clone() {
                paragraph(ui, cx, theme.signal, &why);
                ui.add_space(10.0);
            }
            let (rule, _) = ui.allocate_exact_size(vec2(DIALOG, 1.0), Sense::hover());
            paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            ui.add_space(10.0);
            let mut study = false;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                study = trill_noir::button(ui, theme, "Study", trill_noir::Variant::Primary).clicked();
                if trill_noir::button(ui, theme, "Cancel", trill_noir::Variant::Default).clicked() {
                    cx.studies.dialog.open = false;
                }
            });
            let entered = typed.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if study || entered {
                super::start(ctx, cx);
            }
        });
    if modal.should_close() && !cx.studies.dialog.picking {
        cx.studies.dialog.open = false;
    }
}

/// A paragraph of the dialog.
fn paragraph(ui: &mut Ui, cx: &Cx, color: egui::Color32, words: &str) {
    let theme = cx.theme;
    let style = TextStyle { size: 12.0, ..theme.text.body };
    let galley = prose::plain(ui, theme, style, color, words, DIALOG);
    let (rect, response) = ui.allocate_exact_size(vec2(DIALOG, galley.size().y), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, words));
    prose::paint(ui, rect.min, galley);
}

/// The question before a study goes, or its stems: which songs play it.
fn ask(ctx: &egui::Context, cx: &Cx, question: &Question) -> Option<bool> {
    let what = if question.stems { "the stems of" } else { "" };
    let mut body = if question.stems {
        format!(
            "The stems of {} are removed, a couple of hundred megabytes; everything that was found in them is kept, \
             and studying the recording again makes them again.",
            question.title
        )
    } else {
        format!("{} is removed with everything in its folder: its stems and everything that was found.", question.title)
    };
    if !question.used.is_empty() {
        let songs: Vec<String> = question
            .used
            .iter()
            .map(|song| {
                let places = if song.places == 1 { "1 place".to_owned() } else { format!("{} places", song.places) };
                match (&song.path, song.open) {
                    (Some(path), true) => format!(
                        "the song that is open ({}, {places})",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    (None, true) => format!("the song that is open ({places})"),
                    (Some(path), false) => format!("{} ({places})", path.display()),
                    (None, false) => format!("{} ({places})", song.song),
                }
            })
            .collect();
        body = format!(
            "{body}\n\nThese songs play its files, and will be missing them: {}. \u{201C}Gather\u{201D} in the File menu \
             first gives the open song what it plays in a folder of its own.",
            songs.join("; ")
        );
    }
    let title = format!("Remove {what} {}?", question.title).replace("  ", " ");
    widgets::question(ctx, cx.theme, "study-remove", &title, &body, "Yes, remove", "Keep")
}
