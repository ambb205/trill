//! Missing files: what the song names that is not on disk, in one list.
//!
//! A folder of samples that was moved leaves a song with clips and pads
//! that play nothing. Each of them says so where it is (a clip is hatched,
//! a pad is marked); this is the one place that says it of the song: a line
//! in the status bar for as long as anything is missing, and the list it
//! opens.
//!
//! The list shows each missing file with the places that name it and what
//! was found for it, and how sure that is. *Relink* takes everything that
//! was found for sure, in one undo step; a file that only has the name is
//! never taken without a click of its own (*Use*). *Locate* points at one
//! file by hand, *Look in a folder* at a folder to search, and what they
//! say about where a folder went is used for the rest. A folder of the
//! library that is missing is listed too, and goes along with the song's
//! files when the same move explains both.
//!
//! Nothing here reads a disk: the looking is a worker's
//! (`sounds/missing.rs`), and so is every edit.

use super::sounds::file_name;
use super::state::Cx;
use eframe::egui::{self, Align, Align2, Layout, Rect, Role, Sense, UiBuilder, WidgetInfo, pos2, vec2};
use std::path::Path;
use std::sync::Arc;
use trill_core::files::{FilePlace, folder_of};
use trill_noir::{Chip, TextStyle, Variant, paint, text};
use trill_runtime::{Confidence, Missing, MissingFile, MissingFolder};

/// Width of the list.
const WIDTH: f32 = 680.0;
/// Height of one file in it: two lines of text and room around.
const ROW: f32 = 46.0;
/// More files than this scroll.
const MAX_ROWS: usize = 7;
/// Room for a row's two buttons.
const BUTTONS: f32 = 170.0;
/// And for those of a folder of the library, which are wider.
const LIBRARY_BUTTONS: f32 = 200.0;

fn count(number: usize, one: &str, many: &str) -> String {
    if number == 1 { format!("1 {one}") } else { format!("{number} {many}") }
}

/// What the status bar says while anything is missing: "9 files missing ·
/// all found". `None` when nothing is.
pub fn status_words(missing: &Missing) -> Option<String> {
    let files = missing.files.len();
    if files == 0 {
        return (!missing.folders.is_empty()).then(|| "Library folder missing".to_owned());
    }
    let found = missing.found(Confidence::Sure);
    let what = format!("{} missing", count(files, "file", "files"));
    Some(match found {
        0 => what,
        found if found == files && files == 1 => format!("{what} \u{00B7} found"),
        found if found == files => format!("{what} \u{00B7} all found"),
        found => format!("{what} \u{00B7} {found} found"),
    })
}

/// The song's missing files in a sentence: how many, where they are heard.
fn intro(missing: &Missing) -> String {
    let files = missing.files.len();
    let (mut clips, mut samples) = (0, 0);
    for used in missing.files.iter().flat_map(|file| &file.uses) {
        match used.place {
            FilePlace::Clip(_) => clips += 1,
            FilePlace::Sample { .. } => samples += 1,
        }
    }
    let places = match (clips, samples) {
        (clips, 0) => count(clips, "clip", "clips"),
        (0, samples) => count(samples, "sample", "samples"),
        (clips, samples) => format!("{} and {}", count(clips, "clip", "clips"), count(samples, "sample", "samples")),
    };
    let tracks: Vec<String> = missing.tracks();
    let tracks: Vec<&str> = tracks.iter().map(String::as_str).collect();
    format!(
        "{} this song plays {} not on disk. {places} on {} {} silent.",
        count(files, "audio file", "audio files"),
        if files == 1 { "is" } else { "are" },
        trill_core::journal::words::list(&tracks),
        if clips + samples == 1 { "is" } else { "are" },
    )
}

/// The places that name a file, in a few words: "2 clips · FX".
fn uses(file: &MissingFile) -> String {
    let clips = file.uses.iter().filter(|used| matches!(used.place, FilePlace::Clip(_))).count();
    let slots: Vec<&str> = file
        .uses
        .iter()
        .filter_map(|used| match &used.place {
            FilePlace::Sample { slot, .. } => Some(slot.as_str()),
            FilePlace::Clip(_) => None,
        })
        .collect();
    let mut said: Vec<String> = Vec::new();
    if clips > 0 {
        said.push(count(clips, "clip", "clips"));
    }
    match slots.as_slice() {
        [] => {}
        [slot] => said.push((*slot).to_owned()),
        slots => said.push(format!("{} samples", slots.len())),
    }
    format!("{} \u{00B7} {}", said.join(", "), file.tracks.join(", "))
}

/// What was found for a file, as the list says it: the words, the folder
/// it is in, and how sure that is.
fn found_words(file: &MissingFile) -> (&'static str, &str, Option<Confidence>) {
    match file.found() {
        None => ("Not found", "", None),
        Some(candidate) => {
            let words = match candidate.confidence {
                Confidence::Sure => "In",
                Confidence::Likely => "Likely in",
                Confidence::Unsure => "Unsure:",
            };
            (words, folder_of(&candidate.path), Some(candidate.confidence))
        }
    }
}

/// Words and a path as one line, for whoever cannot see paint.
fn spoken(words: &str, path: &str) -> String {
    if path.is_empty() { words.to_owned() } else { format!("{words} {path}") }
}

/// The second line of a row: a dot in the color of how sure the find is,
/// the words, and the path, which keeps both its ends when it is long.
fn found_line(
    painter: &egui::Painter,
    theme: &trill_noir::Theme,
    line: Rect,
    sure: Option<Confidence>,
    words: &str,
    path: &str,
) {
    let after = paint::dot(painter, pos2(line.left(), line.center().y), theme.metric.dot, dot_color(theme, sure));
    let style = TextStyle { size: 11.5, ..theme.text.body };
    let color = if sure.is_none() { theme.warn } else { theme.ink_dim };
    let room = line.with_min_x(after + 6.0);
    let used = text::paint(painter, room, Align2::LEFT_CENTER, style, words, color);
    if !path.is_empty() {
        let rest = room.with_min_x(room.left() + used + 4.0);
        text::paint_name(painter, rest, Align2::LEFT_CENTER, style, path, color);
    }
}

/// Opens the list, and has the files looked for again.
pub fn open(cx: &mut Cx) {
    cx.sounds.missing.open = true;
    cx.sounds.missing.look_again();
}

/// What was asked for in the list.
enum Asked {
    /// Relink everything found for sure.
    Relink,
    /// Use what was found for this file, sure or not.
    Use(String, String),
    Locate(String),
    LookIn,
    LookAgain,
    MoveLibrary(std::path::PathBuf, std::path::PathBuf),
    LocateLibrary(std::path::PathBuf),
    ToggleLibrary,
    Close,
}

/// Shows the list while it is open, and carries out what is asked in it.
pub fn modal(ctx: &egui::Context, cx: &mut Cx) {
    if !cx.sounds.missing.open {
        return;
    }
    match list(ctx, cx) {
        None => {}
        Some(Asked::Relink) => cx.sounds.relink_found(Confidence::Sure),
        Some(Asked::Use(old, new)) => cx.sounds.relink_file(old, new.into()),
        Some(Asked::Locate(old)) => cx.sounds.locate_file_dialog(old),
        Some(Asked::LookIn) => cx.sounds.look_in_folder_dialog(),
        Some(Asked::LookAgain) => cx.sounds.missing.look_again(),
        Some(Asked::MoveLibrary(from, to)) => cx.sounds.move_library_folder(from, to),
        Some(Asked::LocateLibrary(from)) => cx.sounds.locate_library_folder_dialog(from),
        Some(Asked::ToggleLibrary) => cx.sounds.missing.with_library = !cx.sounds.missing.with_library,
        Some(Asked::Close) => cx.sounds.missing.open = false,
    }
}

/// The color of the dot that says how sure a find is.
fn dot_color(theme: &trill_noir::Theme, sure: Option<Confidence>) -> egui::Color32 {
    match sure {
        Some(Confidence::Sure) => theme.ok,
        Some(Confidence::Likely) => theme.info,
        Some(Confidence::Unsure) | None => theme.warn,
    }
}

fn list(ctx: &egui::Context, cx: &mut Cx) -> Option<Asked> {
    let theme = cx.theme;
    let missing: Arc<Missing> = cx.sounds.missing.found.clone();
    let (busy, looking, with_library) =
        (cx.sounds.missing.busy, cx.sounds.missing.looking, cx.sounds.missing.with_library);
    let pointed = cx.sounds.missing.has_pointed();
    let mut asked = None;
    let mut hint: Option<String> = None;
    let frame = egui::Frame::NONE
        .fill(theme.panel)
        .stroke(egui::Stroke::new(1.0, theme.hairline))
        .inner_margin(egui::Margin::symmetric(14, 12));
    let modal = egui::Modal::new(egui::Id::unique("missing-files")).frame(frame).backdrop_color(theme.backdrop).show(
        ctx,
        |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
            let rule = |ui: &mut egui::Ui| {
                let (rule, _) = ui.allocate_exact_size(vec2(WIDTH, 1.0), Sense::hover());
                paint::hline(ui.painter(), rule.x_range(), rule.top(), theme.hairline);
            };
            let (title, _) = ui.allocate_exact_size(vec2(WIDTH, 18.0), Sense::hover());
            text::paint(ui.painter(), title, Align2::LEFT_CENTER, theme.text.caps, "Missing files", theme.ink_dim);
            ui.add_space(6.0);
            rule(ui);
            ui.add_space(10.0);
            let words = match (missing.files.is_empty(), missing.folders.is_empty()) {
                (true, true) if looking => "Looking\u{2026}".to_owned(),
                (true, true) => "Every file this song plays is where the song says it is.".to_owned(),
                (true, false) => {
                    "Every file this song plays is on disk. A folder of the sound library is not.".to_owned()
                }
                (false, _) => intro(&missing),
            };
            ui.label(egui::RichText::new(words).color(theme.ink));
            ui.add_space(10.0);

            // What was found for sure, and the one action that takes it.
            let sure = missing.found(Confidence::Sure);
            if sure > 0 {
                let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
                ui.painter().rect_filled(row, 0.0, theme.raised);
                let folder = missing.found_in(Confidence::Sure).unwrap_or_default();
                let heading = format!("Found {sure} of {} in", missing.files.len());
                response.widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("{heading} {folder}")));
                let buttons = Rect::from_min_max(pos2(row.right() - BUTTONS, row.top()), row.max - vec2(10.0, 0.0));
                let layout = Layout::right_to_left(Align::Center);
                let mut cell = ui.new_child(UiBuilder::new().id_salt("relink-found").max_rect(buttons).layout(layout));
                if busy {
                    cell.disable();
                }
                let label =
                    if sure == missing.files.len() { format!("Relink all {sure}") } else { format!("Relink {sure}") };
                let relink = trill_noir::button(&mut cell, theme, &label, Variant::Primary);
                if relink.hovered() {
                    hint = Some("Point every clip and sample at the file found for it: one undo step".to_owned());
                }
                if relink.clicked() {
                    asked = Some(Asked::Relink);
                }
                let words =
                    Rect::from_min_max(row.min + vec2(10.0, 7.0), pos2(buttons.left() - 12.0, row.bottom() - 7.0));
                let (upper, lower) = words.split_top_bottom_at_fraction(0.5);
                text::paint(ui.painter(), upper, Align2::LEFT_CENTER, theme.text.micro, &heading, theme.ink_dim);
                let style = TextStyle { size: 12.0, ..theme.text.body };
                text::paint_name(ui.painter(), lower, Align2::LEFT_CENTER, style, &folder, theme.ink);

                // The library's folder that went the same way.
                let moves = missing.library_moves();
                if let Some((from, _)) = moves.first() {
                    ui.add_space(6.0);
                    let (line, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.chip + 4.0), Sense::hover());
                    let spec = Chip::new("Library too", with_library).named("Point the sound library there too");
                    let width = spec.width(ui, theme);
                    let rect = Rect::from_min_size(
                        pos2(line.left(), line.center().y - theme.metric.chip / 2.0),
                        vec2(width, theme.metric.chip),
                    );
                    let chip = trill_noir::chip(ui, theme, rect, ui.scope_id().with("library-too"), &spec);
                    if chip.clicked() {
                        asked = Some(Asked::ToggleLibrary);
                    }
                    let rest = Rect::from_min_max(pos2(rect.right() + 10.0, line.top()), line.max);
                    let says = format!(
                        "The sound library still has {}, which went the same way: its favorites stay with their files",
                        file_name(from)
                    );
                    text::paint(ui.painter(), rest, Align2::LEFT_CENTER, theme.text.micro, &says, theme.ink_faint);
                }
                ui.add_space(10.0);
            }

            // The files.
            if !missing.files.is_empty() {
                rule(ui);
                let shown = missing.files.len().min(MAX_ROWS) as f32;
                egui::ScrollArea::vertical()
                    .id_salt("missing-rows")
                    .max_height(shown * ROW)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        // What needs someone first: the files nothing was
                        // found for, then the ones that are not sure.
                        let mut rows: Vec<&MissingFile> = missing.files.iter().collect();
                        rows.sort_by_key(|file| file.found().map(|candidate| candidate.confidence));
                        for (index, file) in rows.into_iter().enumerate() {
                            let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
                            let name = file_name(Path::new(&file.path));
                            let (says, folder, sure) = found_words(file);
                            let used = uses(file);
                            let spoken = format!("{name} \u{00B7} {used} \u{00B7} {}", spoken(says, folder));
                            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &spoken));
                            if index > 0 {
                                paint::hline(ui.painter(), row.x_range(), row.top(), theme.hairline);
                            }
                            if response.hovered() {
                                // Why it is as sure as it is, and where the file was.
                                hint = Some(match file.found() {
                                    Some(candidate) => format!("{}: {}", candidate.path, candidate.why),
                                    None => format!("It was at {}", file.path),
                                });
                            }
                            let buttons = Rect::from_min_max(pos2(row.right() - BUTTONS, row.top()), row.max);
                            let layout = Layout::right_to_left(Align::Center);
                            let mut cell = ui.new_child(
                                UiBuilder::new().id_salt(("missing", index)).max_rect(buttons).layout(layout),
                            );
                            cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
                            if busy {
                                cell.disable();
                            }
                            if trill_noir::button(&mut cell, theme, "Locate\u{2026}", Variant::Default).clicked() {
                                asked = Some(Asked::Locate(file.path.clone()));
                            }
                            // What is not sure is taken one file at a time, by
                            // whoever has read why.
                            if let Some(candidate) =
                                file.found().filter(|candidate| candidate.confidence < Confidence::Sure)
                                && trill_noir::button(&mut cell, theme, "Use", Variant::Default).clicked()
                            {
                                asked = Some(Asked::Use(file.path.clone(), candidate.path.clone()));
                            }

                            let words = Rect::from_min_max(
                                row.min + vec2(0.0, 7.0),
                                pos2(buttons.left() - 12.0, row.bottom() - 7.0),
                            );
                            let (upper, lower) = words.split_top_bottom_at_fraction(0.5);
                            let width = text::paint(
                                ui.painter(),
                                upper,
                                Align2::RIGHT_CENTER,
                                theme.text.value,
                                &used,
                                theme.ink_dim,
                            );
                            let room = upper.with_max_x(upper.right() - width - 12.0);
                            text::paint_name(
                                ui.painter(),
                                room,
                                Align2::LEFT_CENTER,
                                theme.text.body,
                                &name,
                                theme.ink,
                            );
                            found_line(ui.painter(), theme, lower, sure, says, folder);
                        }
                    });
            }

            // The library's own folders.
            if !missing.folders.is_empty() {
                rule(ui);
                ui.add_space(8.0);
                let (heading, _) = ui.allocate_exact_size(vec2(WIDTH, 16.0), Sense::hover());
                text::paint(
                    ui.painter(),
                    heading,
                    Align2::LEFT_CENTER,
                    theme.text.caps,
                    "Sound library",
                    theme.ink_dim,
                );
                for (index, folder) in missing.folders.iter().enumerate() {
                    if let Some(wanted) = library_row(ui, cx, index, folder, busy, &mut hint) {
                        asked = Some(wanted);
                    }
                }
            }
            rule(ui);
            ui.add_space(12.0);

            let (foot, _) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
            let note = match (&hint, busy, looking) {
                (Some(hint), ..) => hint.as_str(),
                (None, true, _) => "Working\u{2026}",
                (None, false, true) => "Looking\u{2026}",
                (None, false, false) if pointed => "Looking where you pointed too \u{00B7} relinking is one undo step",
                (None, false, false) => "Relinking is one undo step \u{00B7} nothing on disk is touched",
            };
            let layout = Layout::right_to_left(Align::Center);
            let mut cell = ui.new_child(UiBuilder::new().id_salt("missing-foot").max_rect(foot).layout(layout));
            cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
            if trill_noir::button(&mut cell, theme, "Close", Variant::Default).clicked() {
                asked = Some(Asked::Close);
            }
            if trill_noir::button(&mut cell, theme, "Look again", Variant::Default).clicked() {
                asked = Some(Asked::LookAgain);
            }
            if !missing.files.is_empty()
                && trill_noir::button(&mut cell, theme, "Look in a folder\u{2026}", Variant::Default).clicked()
            {
                asked = Some(Asked::LookIn);
            }
            let room = foot.with_max_x(cell.min_rect().left() - 12.0);
            text::paint_name(ui.painter(), room, Align2::LEFT_CENTER, theme.text.micro, note, theme.ink_faint);
        },
    );
    if modal.should_close() {
        asked.get_or_insert(Asked::Close);
    }
    asked
}

/// One missing folder of the library: where it was, where it seems to be,
/// and the buttons that tell the library.
fn library_row(
    ui: &mut egui::Ui,
    cx: &Cx,
    index: usize,
    folder: &MissingFolder,
    busy: bool,
    hint: &mut Option<String>,
) -> Option<Asked> {
    let theme = cx.theme;
    let mut asked = None;
    let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
    let was = folder.path.display().to_string();
    let now = folder.found.as_ref().map(|found| found.path.display().to_string()).unwrap_or_default();
    let (says, sure) = match &folder.found {
        Some(found) if found.sure() => ("Now at", Some(Confidence::Sure)),
        Some(_) => ("A folder of that name is at", Some(Confidence::Likely)),
        None => ("Not found", None),
    };
    let known = count(folder.files, "file known", "files known");
    let spoken = format!("Library folder {was} \u{00B7} {}", spoken(says, &now));
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &spoken));
    if response.hovered() {
        *hint = Some(match &folder.found {
            Some(found) if found.checked > 0 => {
                format!("{} of {} of its files that were looked for are there", found.there, found.checked)
            }
            Some(_) => "The library had read nothing of it, so only its name says so".to_owned(),
            None => "Locate says where it is now; its favorites stay with their files".to_owned(),
        });
    }
    let buttons = Rect::from_min_max(pos2(row.right() - LIBRARY_BUTTONS, row.top()), row.max);
    let layout = Layout::right_to_left(Align::Center);
    let mut cell = ui.new_child(UiBuilder::new().id_salt(("library-folder", index)).max_rect(buttons).layout(layout));
    cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
    if busy {
        cell.disable();
    }
    if trill_noir::button(&mut cell, theme, "Locate\u{2026}", Variant::Default).clicked() {
        asked = Some(Asked::LocateLibrary(folder.path.clone()));
    }
    if let Some(found) = &folder.found
        && trill_noir::button(&mut cell, theme, "Move", Variant::Default).clicked()
    {
        asked = Some(Asked::MoveLibrary(folder.path.clone(), found.path.clone()));
    }
    let words = Rect::from_min_max(row.min + vec2(0.0, 7.0), pos2(buttons.left() - 12.0, row.bottom() - 7.0));
    let (upper, lower) = words.split_top_bottom_at_fraction(0.5);
    let width = text::paint(ui.painter(), upper, Align2::RIGHT_CENTER, theme.text.value, &known, theme.ink_dim);
    let room = upper.with_max_x(upper.right() - width - 12.0);
    text::paint_name(ui.painter(), room, Align2::LEFT_CENTER, theme.text.body, &was, theme.ink);
    found_line(ui.painter(), theme, lower, sure, says, &now);
    asked
}

#[cfg(test)]
mod tests {
    use super::*;
    use trill_core::files::FileUse;
    use trill_core::{ClipId, DeviceId, TrackId};
    use trill_runtime::Candidate;

    fn file(name: &str, clips: usize, pads: &[u8], found: Option<Confidence>) -> MissingFile {
        let path = format!("C:/old/packs/{name}");
        let clip = |id: usize| FileUse {
            place: FilePlace::Clip(ClipId(id as u64)),
            path: path.clone(),
            track: Some(TrackId(1)),
            name: "Clip".into(),
            reach_seconds: Some(1.0),
        };
        let pad = |note: &u8| FileUse {
            place: FilePlace::Sample { device: DeviceId(9), slot: format!("pad {note}") },
            path: path.clone(),
            track: Some(TrackId(2)),
            name: "Drum Rack".into(),
            reach_seconds: None,
        };
        let mut tracks = Vec::new();
        if clips > 0 {
            tracks.push("FX".to_owned());
        }
        if !pads.is_empty() {
            tracks.push("Kit".to_owned());
        }
        let candidates = found.map(|confidence| Candidate {
            path: format!("D:/new/packs/{name}"),
            confidence,
            why: "where its folder went".into(),
            seconds: Some(1.0),
        });
        MissingFile {
            uses: (0..clips).map(clip).chain(pads.iter().map(pad)).collect(),
            path,
            tracks,
            known: None,
            candidates: candidates.into_iter().collect(),
        }
    }

    #[test]
    fn the_status_line_says_how_many_are_missing_and_how_many_were_found() {
        let of = |files: Vec<MissingFile>| status_words(&Missing { files, ..Missing::default() });
        let sure = Some(Confidence::Sure);
        assert_eq!(of(Vec::new()), None);
        assert_eq!(of(vec![file("a.wav", 1, &[], None)]).as_deref(), Some("1 file missing"));
        assert_eq!(of(vec![file("a.wav", 1, &[], sure)]).as_deref(), Some("1 file missing \u{00B7} found"));
        let two = vec![file("a.wav", 1, &[], sure), file("b.wav", 1, &[], sure)];
        assert_eq!(of(two).as_deref(), Some("2 files missing \u{00B7} all found"));
        let mixed = vec![
            file("a.wav", 1, &[], sure),
            file("b.wav", 1, &[], Some(Confidence::Likely)),
            file("c.wav", 1, &[], None),
        ];
        assert_eq!(of(mixed).as_deref(), Some("3 files missing \u{00B7} 1 found"), "only what is sure counts as found");
        let library = Missing {
            folders: vec![MissingFolder { path: "C:/old".into(), files: 3, found: None }],
            ..Missing::default()
        };
        assert_eq!(status_words(&library).as_deref(), Some("Library folder missing"));
    }

    #[test]
    fn a_missing_file_says_where_it_is_used_and_what_was_found() {
        let both = file("Kick 01.wav", 2, &[36], Some(Confidence::Sure));
        assert_eq!(uses(&both), "2 clips, pad 36 \u{00B7} FX, Kit");
        assert_eq!(uses(&file("Kick 01.wav", 0, &[36, 38], None)), "2 samples \u{00B7} Kit");
        assert_eq!(uses(&file("Riser.wav", 1, &[], None)), "1 clip \u{00B7} FX");
        let said = |file: &MissingFile| {
            let (words, folder, _) = found_words(file);
            spoken(words, folder)
        };
        assert_eq!(found_words(&both), ("In", "D:/new/packs", Some(Confidence::Sure)));
        assert_eq!(said(&file("a.wav", 1, &[], Some(Confidence::Likely))), "Likely in D:/new/packs");
        assert_eq!(said(&file("a.wav", 1, &[], Some(Confidence::Unsure))), "Unsure: D:/new/packs");
        assert_eq!(found_words(&file("a.wav", 1, &[], None)), ("Not found", "", None));
        assert_eq!(said(&file("a.wav", 1, &[], None)), "Not found");

        let missing = Missing { files: vec![both, file("Riser.wav", 1, &[], None)], ..Missing::default() };
        assert_eq!(
            intro(&missing),
            "2 audio files this song plays are not on disk. 3 clips and 1 sample on FX and Kit are silent."
        );
        let one = Missing { files: vec![file("Riser.wav", 1, &[], None)], ..Missing::default() };
        assert_eq!(intro(&one), "1 audio file this song plays is not on disk. 1 clip on FX is silent.");
    }
}
