//! The browser's Sounds tab: the sample library.
//!
//! From the top: a search field, the filters, the library's folders, the
//! sounds that match, and a strip that shows the chosen one and plays it.
//! A sound is brought into the song by dragging it (see
//! [`crate::ui::dragging`]) or by double-clicking it, which puts it on the
//! selected track at the edit cursor.
//!
//! Nothing here reads a file or searches. The list is whatever the last
//! search a worker ran came back with ([`crate::ui::sounds::Sounds::search`]),
//! and only the rows on screen are laid out, so a library of a hundred
//! thousand files costs a frame no more than one of a hundred.

use super::{EDGE, ROW, SEARCH_BLOCK, SEARCH_HEIGHT};
use crate::ui::sounds::{DropTarget, WaveState, file_stem};
use crate::ui::state::{Cx, FocusArea, SoundBrowser, SoundForm, TempoFilter};
use crate::ui::{dragging, units, widgets};
use eframe::egui::{
    self, Align2, Event, Key, Mesh, Modifiers, Rect, Role, Sense, Shape, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use trill_core::TrackKind;
use trill_library::{EntryKind, Index, Key as MusicalKey, Query, ScanPhase, Tag, Tags, VOCABULARY};
use trill_noir::{Bar, Chip, Icon, MenuItem, Variant, paint, text};

/// Height of the two rows of filters with the space around them.
pub(in crate::ui) const FILTER_BLOCK: f32 = 18.0 + 4.0 + 18.0 + 8.0;
/// Height of the heading of the folders.
pub(in crate::ui) const SECTION_HEADER: f32 = 22.0;
/// Height of one folder of the tree.
pub(in crate::ui) const TREE_ROW: f32 = 20.0;
/// Height of the line above the list: how many sounds, and the columns.
pub(in crate::ui) const RESULTS_HEADER: f32 = 20.0;
/// Height of the strip along the bottom that shows the chosen sound.
pub(in crate::ui) const PREVIEW_HEIGHT: f32 = 104.0;
/// How far a level of the folder tree is set in.
const INDENT: f32 = 12.0;
/// Widths of the columns at the right of a row: length, tempo, key, and
/// the favorite mark.
const LENGTH_COLUMN: f32 = 44.0;
const TEMPO_COLUMN: f32 = 30.0;
const KEY_COLUMN: f32 = 32.0;
const STAR_COLUMN: f32 = 20.0;
/// The share of the tab the folder tree may take before it scrolls.
const TREE_SHARE: f32 = 0.3;
/// How close to the song's tempo "song tempo" means, in beats per minute.
const NEAR_SONG_TEMPO: f32 = 3.0;
/// The range of the preview volume, in decibels.
const PREVIEW_GAIN: (f32, f32) = (-36.0, 6.0);

/// The tempo ranges offered, in beats per minute.
const TEMPO_RANGES: [(f32, f32); 6] =
    [(60.0, 89.0), (90.0, 109.0), (110.0, 129.0), (130.0, 149.0), (150.0, 179.0), (180.0, 240.0)];

/// The search the tab's filters ask for in a song at `song_tempo`.
pub(in crate::ui) fn query(browser: &SoundBrowser, song_tempo: f64) -> Query {
    let mut tags = Tags::EMPTY;
    match browser.form {
        SoundForm::Any => {}
        SoundForm::OneShot => tags.insert(Tag::OneShot),
        SoundForm::Loop => tags.insert(Tag::Loop),
    }
    if let Some(tag) = browser.tag {
        tags.insert(tag);
    }
    let tempo = match browser.tempo {
        TempoFilter::Any => None,
        TempoFilter::Song => Some(song_tempo as f32 - NEAR_SONG_TEMPO..=song_tempo as f32 + NEAR_SONG_TEMPO),
        TempoFilter::Between(low, high) => Some(low..=high),
    };
    Query {
        text: browser.search.clone(),
        // Sounds only: the library also lists MIDI files, projects and
        // presets, which have their own ways in.
        kind: Some(EntryKind::Audio),
        tags,
        tempo,
        key: browser.key,
        seconds: None,
        folder: browser.folder.clone(),
        favorites_only: browser.favorites,
        include_unreadable: false,
        // Every match: only the rows on screen cost anything.
        limit: None,
    }
}

/// `"420 ms"`, `"2.4 s"`, `"1:07"`.
pub(in crate::ui) fn format_seconds(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        String::new()
    } else if seconds < 0.9995 {
        format!("{:.0} ms", seconds * 1000.0)
    } else if seconds < 59.95 {
        format!("{seconds:.1} s")
    } else {
        let whole = seconds.round() as u64;
        format!("{}:{:02}", whole / 60, whole % 60)
    }
}

/// `"140"`, `"87.5"`.
pub(in crate::ui) fn format_tempo(tempo: f64) -> String {
    if (tempo - tempo.round()).abs() < 0.05 { format!("{tempo:.0}") } else { format!("{tempo:.1}") }
}

/// `"1,234,567"`.
pub(in crate::ui) fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A tag's label with a capital, as a menu shows it: `"Kick"`, `"808"`.
fn tag_title(tag: Tag) -> String {
    let label = tag.label();
    let mut letters = label.chars();
    letters.next().map_or_else(String::new, |first| first.to_uppercase().chain(letters).collect())
}

/// One row of the list, copied out of the index so that acting on it does
/// not hold the search results.
#[derive(Clone)]
struct SoundRow {
    row: usize,
    path: PathBuf,
    name: String,
    seconds: Option<f64>,
    tempo: Option<f64>,
    key: Option<MusicalKey>,
    tags: Tags,
}

impl SoundRow {
    fn at(index: &Index, ids: &[trill_library::EntryId], row: usize) -> Option<Self> {
        let entry = index.entry(*ids.get(row)?)?;
        Some(Self {
            row,
            path: entry.path.clone(),
            name: entry.name.clone(),
            seconds: entry.seconds(),
            tempo: entry.tempo.map(f64::from),
            key: entry.key,
            tags: entry.tags,
        })
    }
}

pub(super) fn show(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    sync_search(cx);

    let inner = Rect::from_min_max(area.min, pos2(area.right() - 1.0, area.bottom()));
    let search = Rect::from_min_size(
        pos2(inner.left() + EDGE, inner.top() + 8.0),
        vec2(inner.width() - 2.0 * EDGE, SEARCH_HEIGHT),
    );
    search_field(ui, cx, search);
    let filters_top = inner.top() + SEARCH_BLOCK;
    filters(ui, cx, Rect::from_min_max(pos2(search.left(), filters_top), pos2(search.right(), filters_top + 40.0)));

    let preview = Rect::from_min_max(pos2(inner.left(), inner.bottom() - PREVIEW_HEIGHT), inner.max);
    let middle = Rect::from_min_max(pos2(inner.left(), filters_top + FILTER_BLOCK), pos2(inner.right(), preview.top()));
    paint::hline(ui.painter(), middle.x_range(), middle.top(), cx.theme.hairline);

    let roots = cx.sounds.runtime.library.roots();
    if roots.is_empty() {
        no_folders(ui, cx, middle);
    } else {
        let tree_bottom = folders(ui, cx, middle, &roots);
        results(ui, cx, Rect::from_min_max(pos2(middle.left(), tree_bottom), middle.max));
    }
    preview_strip(ui, cx, preview);

    // A press anywhere in the tab makes the keyboard the list's.
    let pressed_here = ui.input(|input| {
        input.pointer.any_pressed() && input.pointer.interact_pos().is_some_and(|position| area.contains(position))
    });
    if pressed_here {
        cx.state.focus = FocusArea::Browser;
    }
}

/// Asks for the list the filters describe, and takes over what a search
/// that just came back says about the selection.
fn sync_search(cx: &mut Cx) {
    let query = query(&cx.state.browser.sounds, cx.snap.project.tempo);
    let selected = cx.state.browser.sounds.selected.clone();
    cx.sounds.search(&query, selected.as_deref());
    if !std::mem::take(&mut cx.sounds.search.fresh) {
        return;
    }
    let Some(found) = &cx.sounds.search.found else { return };
    let browser = &mut cx.state.browser.sounds;
    // The search looked the selection up as it was when it set out. If
    // another sound has been chosen since, that one is looked up here.
    let row = if found.selected == browser.selected {
        found.selected_row
    } else {
        browser.selected.as_deref().and_then(|path| found.row_of(path))
    };
    // A sound that has just turned up in the list is brought into view.
    browser.reveal |= browser.selected_row.is_none() && row.is_some();
    browser.selected_row = row;
    // A sound named on the command line is chosen once the library has it.
    if let Some(wanted) = &browser.pending_select {
        let index = found.results.index();
        let row = found.results.ids().iter().position(|id| index.entry(*id).is_some_and(|entry| entry.name == *wanted));
        if let Some(row) = SoundRow::at(index, found.results.ids(), row.unwrap_or(usize::MAX)) {
            browser.selected = Some(row.path);
            browser.selected_row = Some(row.row);
            browser.reveal = true;
            browser.pending_select = None;
        }
    }
}

fn search_field(ui: &mut Ui, cx: &mut Cx, rect: Rect) {
    let theme = cx.theme;
    let id = ui.scope_id().with("sound-search");
    let browser = &mut cx.state.browser.sounds;
    let response = trill_noir::text_field(ui, theme, rect, id, &mut browser.search, "Search sounds");
    if std::mem::take(&mut browser.focus_search) {
        response.request_focus();
    }
    browser.search_focused = response.has_focus();
    if response.has_focus() {
        cx.state.focus = FocusArea::Browser;
    }
    cx.hint(
        &response,
        "Words, in any order: names, folders, kinds of sound (kick, pad), tempos (140bpm) and keys (f#m)",
    );
    if !cx.state.browser.sounds.search.is_empty() {
        let clear = Rect::from_center_size(pos2(rect.right() - 11.0, rect.center().y), vec2(18.0, 18.0));
        let response =
            trill_noir::bare_icon_button(ui, theme, clear, id.with("clear"), Icon::Close, "Clear the search");
        cx.hint(&response, "Clear the search");
        if response.clicked() {
            cx.state.browser.sounds.search.clear();
        }
    }
}

/// One-shots or loops, favorites, and the kind, tempo and key.
fn filters(ui: &mut Ui, cx: &mut Cx, rect: Rect) {
    let theme = cx.theme;
    let id = ui.scope_id().with("sound-filters");
    let chip = theme.metric.chip;

    let forms = [SoundForm::Any, SoundForm::OneShot, SoundForm::Loop];
    let current = forms.iter().position(|form| *form == cx.state.browser.sounds.form);
    let labels = ["All", "One-shot", "Loop"];
    let (clicked, _) =
        trill_noir::chip_row(ui, theme, rect.min, rect.width(), id.with("form"), &labels, current, false);
    if let Some(index) = clicked {
        cx.state.browser.sounds.form = forms[index];
    }
    let row = Rect::from_min_size(rect.min, vec2(rect.width() - chip - 6.0, chip));
    if ui.rect_contains_pointer(row) {
        cx.state.hint.set("Show every sound, only one-shots, or only loops");
    }
    let favorites = cx.state.browser.sounds.favorites;
    let star = Rect::from_min_size(pos2(rect.right() - chip, rect.top()), vec2(chip, chip));
    let icon = if favorites { Icon::StarOn } else { Icon::Star };
    let color = if favorites { theme.ink } else { theme.ink_dim };
    let response = trill_noir::icon_button(ui, theme, star, id.with("favorites"), icon, favorites, color);
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, favorites, "Favorites only"));
    cx.hint(&response, "Show only the sounds you marked as favorites");
    if response.clicked() {
        cx.state.browser.sounds.favorites = !favorites;
    }

    // Kind, tempo and key share the second row.
    let top = rect.top() + chip + 4.0;
    let gap = theme.metric.gap;
    let width = ((rect.width() - 2.0 * gap) / 3.0).floor();
    let cell = |index: usize| {
        let left = rect.left() + index as f32 * (width + gap);
        let right = if index == 2 { rect.right() } else { left + width };
        Rect::from_min_max(pos2(left, top), pos2(right, top + chip))
    };

    let tag = cx.state.browser.sounds.tag;
    let shown = tag.map_or_else(|| "Any".to_owned(), tag_title);
    let response = trill_noir::dropdown(ui, theme, cell(0), id.with("kind"), "Kind", &shown, false);
    cx.hint(&response, "Only one kind of sound: kicks, pads, vocals\u{2026}");
    let mut chosen = None;
    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(150.0);
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            if MenuItem::new("Any kind").checked(tag.is_none()).show(ui, theme).clicked() {
                chosen = Some(None);
            }
            trill_noir::menu_separator(ui, theme);
            // One-shot and loop have their chips above.
            let kinds = VOCABULARY.iter().filter(|rule| !matches!(rule.tag, Tag::Loop | Tag::OneShot));
            for rule in kinds {
                let title = tag_title(rule.tag);
                if MenuItem::new(&title).checked(tag == Some(rule.tag)).show(ui, theme).clicked() {
                    chosen = Some(Some(rule.tag));
                }
            }
        });
    });
    if let Some(tag) = chosen {
        cx.state.browser.sounds.tag = tag;
    }

    let song = cx.snap.project.tempo;
    let tempo = cx.state.browser.sounds.tempo;
    let range = |(low, high): (f32, f32)| format!("{low:.0}\u{2013}{high:.0}");
    let shown = match tempo {
        TempoFilter::Any => "Any".to_owned(),
        TempoFilter::Song => format_tempo(song),
        TempoFilter::Between(low, high) => range((low, high)),
    };
    let response = trill_noir::dropdown(ui, theme, cell(1), id.with("tempo"), "BPM", &shown, false);
    cx.hint(&response, "Only sounds whose name says a tempo in this range");
    let mut chosen = None;
    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(170.0);
        if MenuItem::new("Any tempo").checked(tempo == TempoFilter::Any).show(ui, theme).clicked() {
            chosen = Some(TempoFilter::Any);
        }
        let near = format!("Song tempo ({})", format_tempo(song));
        if MenuItem::new(&near).checked(tempo == TempoFilter::Song).show(ui, theme).clicked() {
            chosen = Some(TempoFilter::Song);
        }
        trill_noir::menu_separator(ui, theme);
        for bounds in TEMPO_RANGES {
            let choice = TempoFilter::Between(bounds.0, bounds.1);
            let label = format!("{} BPM", range(bounds));
            if MenuItem::new(&label).checked(tempo == choice).show(ui, theme).clicked() {
                chosen = Some(choice);
            }
        }
    });
    if let Some(tempo) = chosen {
        cx.state.browser.sounds.tempo = tempo;
    }

    let key = cx.state.browser.sounds.key;
    let shown = key.map_or("Any", MusicalKey::name);
    let response = trill_noir::dropdown(ui, theme, cell(2), id.with("key"), "Key", shown, false);
    cx.hint(&response, "Only sounds whose name says this key");
    let mut chosen = None;
    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(120.0);
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            if MenuItem::new("Any key").checked(key.is_none()).show(ui, theme).clicked() {
                chosen = Some(None);
            }
            for minor in [true, false] {
                trill_noir::menu_separator(ui, theme);
                for root in 0..12 {
                    let choice = MusicalKey::new(root, minor);
                    if MenuItem::new(choice.name()).checked(key == Some(choice)).show(ui, theme).clicked() {
                        chosen = Some(Some(choice));
                    }
                }
            }
        });
    });
    if let Some(key) = chosen {
        cx.state.browser.sounds.key = key;
    }
}

/// What the tab says while the library has no folders.
fn no_folders(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    let theme = cx.theme;
    let block = Rect::from_min_max(
        pos2(area.left() + EDGE, area.top() + 18.0),
        pos2(area.right() - EDGE, (area.top() + 150.0).min(area.bottom())),
    );
    let painter = ui.painter_at(area);
    let line = |index: usize| {
        Rect::from_min_size(pos2(block.left(), block.top() + index as f32 * 18.0), vec2(block.width(), 18.0))
    };
    text::paint(&painter, line(0), Align2::LEFT_CENTER, theme.text.caps, "No sound folders yet", theme.ink_dim);
    let quiet = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
    for (index, sentence) in ["Add a folder of samples and Trill", "indexes it in the background."].iter().enumerate() {
        text::paint(&painter, line(index + 1), Align2::LEFT_CENTER, quiet, sentence, theme.ink_faint);
    }
    let button = Rect::from_min_size(pos2(block.left(), block.top() + 66.0), vec2(block.width(), theme.metric.control));
    widgets::place(ui, button, "add-first-folder", egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let response = trill_noir::button(ui, theme, "Add folder\u{2026}", Variant::Primary);
        cx.hint(&response, "Choose a folder of audio files to add to the library");
        if response.clicked() {
            add_folder(cx);
        }
    });
}

fn add_folder(cx: &mut Cx) {
    // Beside the folder added last: packs tend to live together.
    let beside = cx.state.browser.sounds.last_added.as_deref().and_then(Path::parent).map(Path::to_path_buf);
    cx.sounds.add_folder_dialog(beside);
}

/// A folder of the tree as it is shown this frame.
struct TreeNode {
    path: PathBuf,
    name: String,
    depth: usize,
    /// Files in it and beneath it; `None` until a scan has listed it.
    files: Option<usize>,
    has_subfolders: bool,
    is_root: bool,
}

/// The folders on show: the roots, and what is unfolded beneath them.
fn tree(index: &Index, roots: &[PathBuf], browser: &SoundBrowser) -> Vec<TreeNode> {
    fn add(index: &Index, path: &Path, depth: usize, browser: &SoundBrowser, out: &mut Vec<TreeNode>) {
        let folder = index.find_folder(path).and_then(|id| index.folder(id));
        let name = folder.map(|folder| folder.name().to_owned()).filter(|name| !name.is_empty());
        let name = name.unwrap_or_else(|| file_stem_or_path(path));
        out.push(TreeNode {
            path: path.to_path_buf(),
            name,
            depth,
            files: folder.map(|folder| folder.all_files().len()),
            has_subfolders: folder.is_some_and(|folder| !folder.subfolders().is_empty()),
            is_root: depth == 0,
        });
        if let Some(folder) = folder.filter(|_| browser.expanded.contains(path)) {
            for sub in folder.subfolders().iter().filter_map(|id| index.folder(*id)) {
                add(index, sub.path(), depth + 1, browser, out);
            }
        }
    }
    let mut nodes = Vec::new();
    for root in roots {
        add(index, root, 0, browser, &mut nodes);
    }
    nodes
}

/// The last part of `path`, or all of it for a drive.
fn file_stem_or_path(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// The library's folders: add, remove, rescan, and choose one to look in.
/// Returns where the section ends.
fn folders(ui: &mut Ui, cx: &mut Cx, area: Rect, roots: &[PathBuf]) -> f32 {
    let theme = cx.theme;
    let id = ui.scope_id().with("sound-folders");
    let header = Rect::from_min_size(area.min, vec2(area.width(), SECTION_HEADER));
    let open = cx.state.browser.sounds.folders_open;

    // The heading folds the tree away; the actions sit at its right.
    let caret = Rect::from_center_size(pos2(header.left() + EDGE + 4.0, header.center().y), vec2(16.0, 16.0));
    let icon = if open { Icon::CaretDown } else { Icon::CaretRight };
    let name = if open { "Hide the folders" } else { "Show the folders" };
    let response = trill_noir::bare_icon_button(ui, theme, caret, id.with("fold"), icon, name);
    cx.hint(&response, name);
    if response.clicked() {
        cx.state.browser.sounds.folders_open = !open;
    }
    let painter = ui.painter().clone();
    let title = Rect::from_min_max(pos2(caret.right() + 2.0, header.top()), header.max);
    let used = text::paint(&painter, title, Align2::LEFT_CENTER, theme.text.caps, "Folders", theme.ink_dim);

    let mut right = header.right() - EDGE;
    let mut action = |ui: &Ui, label: &str| {
        let spec = Chip::action(label);
        let width = spec.width(ui, theme);
        let rect = Rect::from_min_size(
            pos2(right - width, header.center().y - theme.metric.chip / 2.0),
            vec2(width, theme.metric.chip),
        );
        right = rect.left() - theme.metric.gap;
        trill_noir::chip(ui, theme, rect, id.with(("action", label)), &spec)
    };
    let response = action(ui, "Add");
    cx.hint(&response, "Add a folder of sounds to the library");
    if response.clicked() {
        add_folder(cx);
    }
    let status = cx.sounds.runtime.library.scan_status();
    let response = action(ui, "Rescan");
    cx.hint(&response, "Look through the folders again for files that were added, changed or removed");
    if response.clicked() {
        cx.sounds.quietly(|runtime| {
            runtime.library.rescan();
            Ok(())
        });
    }
    // What the library is doing, between the title and the actions.
    let progress = match status.phase {
        ScanPhase::Idle => None,
        ScanPhase::Listing => Some((format!("Listing \u{00B7} {}", format_count(status.files)), None)),
        ScanPhase::Reading => {
            let fraction = status.headers_read as f32 / status.headers_to_read.max(1) as f32;
            Some((format!("Reading \u{00B7} {:.0} %", fraction * 100.0), Some(fraction)))
        }
        ScanPhase::Indexing => Some(("Indexing".to_owned(), Some(1.0))),
    };
    if let Some((words, fraction)) = &progress {
        let room =
            Rect::from_min_max(pos2(title.left() + used + 10.0, header.top()), pos2(right - 4.0, header.bottom()));
        text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.micro, words, theme.info);
        if let Some(fraction) = fraction {
            let done = header.left() + header.width() * fraction.clamp(0.0, 1.0);
            let line = Rect::from_min_max(pos2(header.left(), header.bottom() - 2.0), pos2(done, header.bottom()));
            painter.rect_filled(line, 0.0, theme.info);
        }
        // The numbers move while nothing else does.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
    }
    if !open {
        paint::hline_above(&painter, header.x_range(), header.bottom(), theme.hairline);
        return header.bottom();
    }

    let index = cx.sounds.runtime.library.index();
    let nodes = tree(&index, roots, &cx.state.browser.sounds);
    // "All folders" leads the list.
    let wanted = (nodes.len() + 1) as f32 * TREE_ROW + 4.0;
    let height = wanted.min((area.height() * TREE_SHARE).max(3.0 * TREE_ROW)).min(area.height() - SECTION_HEADER);
    let list = Rect::from_min_size(pos2(area.left(), header.bottom()), vec2(area.width(), height.max(0.0)));
    let mut child = ui.new_child(UiBuilder::new().id_salt("folder-tree").max_rect(list));
    child.set_clip_rect(list.intersect(ui.clip_rect()));
    egui::ScrollArea::vertical().id_salt("folder-tree").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let chosen = cx.state.browser.sounds.folder.clone();
        let response = tree_row(ui, cx, id.with("all"), "All folders", 0, None, chosen.is_none(), None);
        cx.hint(&response, "Look in every folder of the library");
        if response.clicked() {
            cx.state.browser.sounds.folder = None;
        }
        for node in &nodes {
            folder_row(ui, cx, id, node, chosen.as_deref() == Some(node.path.as_path()));
        }
    });
    paint::hline_above(&painter, list.x_range(), list.bottom(), theme.hairline);
    list.bottom()
}

/// One line of the folder tree. `caret` is `Some(unfolded)` for a folder
/// with folders inside it.
#[expect(clippy::too_many_arguments, reason = "a private paint routine; a struct would only rename the arguments")]
fn tree_row(
    ui: &mut Ui,
    cx: &Cx,
    id: egui::Id,
    name: &str,
    depth: usize,
    files: Option<usize>,
    selected: bool,
    caret: Option<bool>,
) -> egui::Response {
    let theme = cx.theme;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), TREE_ROW), Sense::hover());
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, name));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 0.0, theme.selected);
    } else if response.hovered() {
        painter.rect_filled(rect, 0.0, theme.hover);
    }
    let lit = selected || response.hovered();
    let left = rect.left() + EDGE + depth as f32 * INDENT;
    if let Some(unfolded) = caret {
        let icon = if unfolded { Icon::CaretDown } else { Icon::CaretRight };
        trill_noir::paint_icon(
            painter,
            pos2(left + 4.0, rect.center().y),
            icon,
            if lit { theme.ink } else { theme.ink_faint },
        );
    }
    let mut content = Rect::from_min_max(pos2(left + 14.0, rect.top()), pos2(rect.right() - EDGE, rect.bottom()));
    if let Some(files) = files {
        let quiet = if lit { theme.ink_dim } else { theme.ink_faint };
        let width = text::paint(painter, content, Align2::RIGHT_CENTER, theme.text.value, &format_count(files), quiet);
        content.max.x -= width + 8.0;
    }
    let style = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
    text::paint(painter, content, Align2::LEFT_CENTER, style, name, if lit { theme.ink } else { theme.ink_dim });
    response
}

fn folder_row(ui: &mut Ui, cx: &mut Cx, id: egui::Id, node: &TreeNode, selected: bool) {
    let theme = cx.theme;
    let unfolded = cx.state.browser.sounds.expanded.contains(&node.path);
    let row_id = id.with(("folder", &node.path));
    let caret = node.has_subfolders.then_some(unfolded);
    let response = tree_row(ui, cx, row_id, &node.name, node.depth, node.files, selected, caret);
    cx.hint(
        &response,
        if node.is_root {
            format!("{} \u{00B7} click to look only in here \u{00B7} right-click for more", node.path.display())
        } else {
            format!("{} \u{00B7} click to look only in here", node.path.display())
        },
    );
    let rect = response.rect;
    let toggle = |cx: &mut Cx| {
        let expanded = &mut cx.state.browser.sounds.expanded;
        if !expanded.remove(&node.path) {
            expanded.insert(node.path.clone());
        }
    };
    // The caret has a zone of its own, so that looking inside a folder
    // does not also narrow the list to it.
    let caret_zone =
        Rect::from_min_max(rect.min, pos2(rect.left() + EDGE + node.depth as f32 * INDENT + 12.0, rect.bottom()));
    let on_caret =
        node.has_subfolders && response.interact_pointer_pos().is_some_and(|position| caret_zone.contains(position));
    if response.clicked() {
        if on_caret {
            toggle(cx);
        } else {
            let browser = &mut cx.state.browser.sounds;
            // A second click on the chosen folder lets go of it.
            browser.folder = if selected { None } else { Some(node.path.clone()) };
            if node.has_subfolders && !selected {
                browser.expanded.insert(node.path.clone());
            }
        }
    }
    response.context_menu(|ui| {
        ui.set_min_width(200.0);
        if node.has_subfolders && MenuItem::new(if unfolded { "Fold" } else { "Unfold" }).show(ui, theme).clicked() {
            toggle(cx);
        }
        if MenuItem::new(reveal_label()).show(ui, theme).clicked() {
            reveal(cx, node.path.clone());
        }
        if MenuItem::new("Copy path").show(ui, theme).clicked() {
            cx.state.copied_text = Some(node.path.display().to_string());
        }
        if node.is_root {
            trill_noir::menu_separator(ui, theme);
            if MenuItem::new("Remove from the library")
                .detail("The folder and its files stay on disk; they are only no longer listed here.")
                .show(ui, theme)
                .clicked()
            {
                remove_folder(cx, &node.path);
            }
        }
    });
}

fn remove_folder(cx: &mut Cx, folder: &Path) {
    let browser = &mut cx.state.browser.sounds;
    if browser.folder.as_deref().is_some_and(|chosen| chosen.starts_with(folder)) {
        browser.folder = None;
    }
    browser.expanded.retain(|open| !open.starts_with(folder));
    cx.sounds.remove_folder(folder.to_path_buf());
}

/// What the file manager is called here.
fn reveal_label() -> &'static str {
    if cfg!(windows) {
        "Show in Explorer"
    } else if cfg!(target_os = "macos") {
        "Show in Finder"
    } else {
        "Show in the file manager"
    }
}

/// Opens the system's file manager on `path`, with the file selected where
/// the system can do that.
pub(in crate::ui) fn reveal(cx: &mut Cx, path: PathBuf) {
    cx.sounds.quietly(move |_| {
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("explorer");
            // Explorer wants its own spelling of the path.
            command.arg(format!("/select,{}", path.display().to_string().replace('/', "\\")));
            command
        } else if cfg!(target_os = "macos") {
            let mut command = std::process::Command::new("open");
            command.arg("-R").arg(&path);
            command
        } else {
            let mut command = std::process::Command::new("xdg-open");
            command.arg(if path.is_dir() { path.as_path() } else { path.parent().unwrap_or(&path) });
            command
        };
        command.spawn().map(drop).map_err(|error| format!("Could not open the file manager: {error}"))
    });
}

/// The sounds that match, in a list that lays out only what is on screen.
fn results(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    let theme = cx.theme;
    let header = Rect::from_min_size(area.min, vec2(area.width(), RESULTS_HEADER));
    let list = Rect::from_min_max(pos2(area.left(), header.bottom()), area.max);
    let painter = ui.painter().clone();

    let (count, total) = match &cx.sounds.search.found {
        Some(found) => (found.results.len(), found.total),
        None => (0, 0),
    };
    let searching = cx.sounds.search.found.is_none();
    let summary = match (searching, count, total) {
        (true, ..) => "Searching\u{2026}".to_owned(),
        (false, 0, 0) => "No sounds yet".to_owned(),
        (false, 0, _) => "No sounds match".to_owned(),
        (false, 1, _) => "1 sound".to_owned(),
        (false, count, total) if count == total => format!("{} sounds", format_count(count)),
        (false, count, total) => format!("{} of {}", format_count(count), format_count(total)),
    };
    let titles = header.shrink2(vec2(EDGE, 0.0));
    text::paint(&painter, titles, Align2::LEFT_CENTER, theme.text.micro, &summary, theme.ink_faint);
    // The columns' names, over the columns.
    let mut right = header.right() - EDGE - STAR_COLUMN;
    for (title, width) in [("Key", KEY_COLUMN), ("BPM", TEMPO_COLUMN), ("Len", LENGTH_COLUMN)] {
        let cell = Rect::from_min_max(pos2(right - width, header.top()), pos2(right, header.bottom()));
        text::paint(&painter, cell, Align2::RIGHT_CENTER, theme.text.micro, title, theme.ink_faint);
        right -= width;
    }
    paint::hline_above(&painter, header.x_range(), header.bottom(), theme.hairline);

    if count == 0 {
        if !searching {
            let advice = if total == 0 {
                "The library is still empty: a scan may be running."
            } else {
                "Nothing fits the search and the filters above."
            };
            let room =
                Rect::from_min_size(pos2(list.left() + EDGE, list.top() + 8.0), vec2(list.width() - 2.0 * EDGE, 18.0));
            let quiet = trill_noir::TextStyle { size: 12.0, ..theme.text.body };
            text::paint(&painter, room, Align2::LEFT_CENTER, quiet, advice, theme.ink_faint);
        }
        return;
    }

    let id = ui.scope_id().with("sound-rows");
    let selected_row = cx.state.browser.sounds.selected_row;
    let reveal = std::mem::take(&mut cx.state.browser.sounds.reveal);
    let mut child = ui.new_child(UiBuilder::new().id_salt("sound-list").max_rect(list));
    child.set_clip_rect(list.intersect(ui.clip_rect()));
    egui::ScrollArea::vertical().id_salt("sound-list").auto_shrink([false, false]).show_viewport(
        &mut child,
        |ui, viewport| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.set_height(count as f32 * ROW);
            let origin = ui.max_rect().min;
            let width = ui.max_rect().width();
            let row_rect =
                |row: usize| Rect::from_min_size(pos2(origin.x, origin.y + row as f32 * ROW), vec2(width, ROW));
            if reveal && let Some(row) = selected_row.filter(|row| *row < count) {
                ui.scroll_to_rect(row_rect(row), None);
            }
            let first = (viewport.min.y / ROW).floor().max(0.0) as usize;
            let last = ((viewport.max.y / ROW).ceil() as usize).min(count);
            // Copied out, so that acting on a row may change anything.
            let rows: Vec<SoundRow> = match &cx.sounds.search.found {
                Some(found) => (first..last)
                    .filter_map(|row| SoundRow::at(found.results.index(), found.results.ids(), row))
                    .collect(),
                None => Vec::new(),
            };
            for sound in rows {
                let rect = row_rect(sound.row);
                sound_row(ui, cx, id, rect, &sound, selected_row == Some(sound.row));
            }
        },
    );
}

fn sound_row(ui: &mut Ui, cx: &mut Cx, id: egui::Id, rect: Rect, sound: &SoundRow, selected: bool) {
    let theme = cx.theme;
    let response = ui.interact(rect, id.with(sound.row), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &sound.name));
    let favorite = cx.sounds.runtime.library.is_favorite(&sound.path);
    let star = Rect::from_min_max(
        pos2(rect.right() - EDGE - STAR_COLUMN + 2.0, rect.top()),
        pos2(rect.right() - EDGE + 4.0, rect.bottom()),
    );
    // Registered after the row, so that it takes the clicks that land on it.
    let star_response = ui.interact(star, id.with(("favorite", sound.row)), Sense::click());
    let star_name = if favorite {
        format!("Remove {} from the favorites", sound.name)
    } else {
        format!("Add {} to the favorites", sound.name)
    };
    star_response.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, favorite, &star_name));
    let hovered = response.hovered() || star_response.hovered();

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 0.0, theme.selected);
        } else if hovered {
            painter.rect_filled(rect, 0.0, theme.hover);
        }
        let lit = selected || hovered;
        let quiet = if lit { theme.ink_dim } else { theme.ink_faint };
        // What form it takes, before the name.
        let glyph = pos2(rect.left() + EDGE + 5.0, rect.center().y);
        if sound.tags.contains(Tag::Loop) {
            trill_noir::paint_icon(painter, glyph, Icon::Loop, quiet);
        } else if sound.tags.contains(Tag::OneShot) {
            trill_noir::paint_icon(painter, glyph, Icon::Hit, quiet);
        }
        if favorite || hovered {
            let icon = if favorite { Icon::StarOn } else { Icon::Star };
            let color = match (favorite, star_response.hovered()) {
                (_, true) => theme.ink,
                (true, false) => theme.ink_dim,
                (false, false) => theme.ink_faint,
            };
            trill_noir::paint_icon(painter, star.center(), icon, color);
        }
        let mut right = rect.right() - EDGE - STAR_COLUMN;
        let cells = [
            (sound.key.map(|key| key.name().to_owned()), KEY_COLUMN),
            (sound.tempo.map(format_tempo), TEMPO_COLUMN),
            (sound.seconds.map(format_seconds), LENGTH_COLUMN),
        ];
        for (value, width) in cells {
            let cell = Rect::from_min_max(pos2(right - width, rect.top()), pos2(right, rect.bottom()));
            if let Some(value) = value {
                text::paint(painter, cell, Align2::RIGHT_CENTER, theme.text.value, &value, quiet);
            }
            right -= width;
        }
        let name = Rect::from_min_max(pos2(rect.left() + EDGE + 16.0, rect.top()), pos2(right - 4.0, rect.bottom()));
        text::paint(
            painter,
            name,
            Align2::LEFT_CENTER,
            theme.text.body,
            &sound.name,
            if lit { theme.ink } else { theme.ink_dim },
        );
    }

    if response.hovered() {
        cx.state.hint.set(format!(
            "{} \u{00B7} drag into the song \u{00B7} double-click adds it at the edit cursor \u{00B7} right-click for more",
            sound.name
        ));
    }
    cx.hint(&star_response, if favorite { "A favorite \u{00B7} click to unmark" } else { "Mark as a favorite" });
    if star_response.clicked() {
        toggle_favorite(cx, &sound.path, favorite);
    }
    if response.drag_started() {
        choose(cx, sound.row, &sound.path, false);
        dragging::start(cx, vec![sound.path.clone()], sound.seconds, sound.tempo, sound.tags);
    }
    if widgets::activated(ui, &response) {
        add_at_cursor(cx, sound);
    } else if response.clicked() {
        choose(cx, sound.row, &sound.path, cx.state.browser.sounds.auto_preview);
    }
    if response.secondary_clicked() {
        choose(cx, sound.row, &sound.path, false);
    }
    response.context_menu(|ui| sound_menu(ui, cx, sound, favorite));
}

fn toggle_favorite(cx: &mut Cx, path: &Path, favorite: bool) {
    // A lock and a hash lookup; the settings are written by the runtime's
    // own thread.
    cx.sounds.runtime.library.set_favorite(path, !favorite);
}

/// Makes the sound at `row` the chosen one, and plays it if `play`.
fn choose(cx: &mut Cx, row: usize, path: &Path, play: bool) {
    let browser = &mut cx.state.browser.sounds;
    browser.selected = Some(path.to_path_buf());
    browser.selected_row = Some(row);
    cx.state.focus = FocusArea::Browser;
    if play {
        let gain = cx.state.browser.sounds.preview_gain_db;
        cx.sounds.preview(path, gain);
    }
}

/// Makes the file at `path` the browser's chosen sound, whether or not the
/// library lists it: the strip then shows it, plays it and lets it be
/// dragged. For "find this file in the browser" on a clip, a sampler or a
/// pad.
pub(in crate::ui) fn choose_file(cx: &mut Cx, path: PathBuf) {
    let browser = &mut cx.state.browser;
    browser.open = true;
    browser.tab = crate::ui::state::BrowserTab::Sounds;
    browser.sounds.selected = Some(path);
    browser.sounds.selected_row = None;
    cx.state.focus = FocusArea::Browser;
    // Looked up in the list by the next search.
    cx.sounds.search.outdate();
}

/// The track a sound goes on when nobody says which: the selected one, if
/// it holds clips.
fn cursor_track<'a>(cx: &'a Cx) -> Option<(trill_core::TrackId, &'a str)> {
    let track = cx.state.selected_track.filter(|_| !cx.state.master_selected).and_then(|id| cx.snap.track(id))?;
    (track.kind == TrackKind::Normal).then_some((track.id, track.name.as_str()))
}

/// Puts the sound on the selected track at the edit cursor, or on a track
/// of its own when no track that holds clips is selected.
fn add_at_cursor(cx: &mut Cx, sound: &SoundRow) {
    let beat = cx.state.arrangement.cursor;
    let target = match cursor_track(cx) {
        Some((track, _)) => DropTarget::Lane { track, beat },
        None => DropTarget::NewTrack { beat, colors: dragging::new_track_colors(cx, 1) },
    };
    add_to(cx, sound, target);
}

fn add_to(cx: &mut Cx, sound: &SoundRow, target: DropTarget) {
    let project = &cx.snap.project;
    let length = match sound.seconds {
        Some(seconds) => crate::ui::sounds::dropped_length(seconds, sound.tags, sound.tempo, project.tempo),
        None => project.time_signature.beats_per_bar(),
    };
    cx.sounds.drop_files(vec![sound.path.clone()], target, sound.name.clone(), length);
}

fn sound_menu(ui: &mut Ui, cx: &mut Cx, sound: &SoundRow, favorite: bool) {
    let theme = cx.theme;
    ui.set_min_width(230.0);
    if MenuItem::new("Preview").shortcut("Enter").show(ui, theme).clicked() {
        let gain = cx.state.browser.sounds.preview_gain_db;
        cx.sounds.preview(&sound.path, gain);
    }
    trill_noir::menu_separator(ui, theme);
    let beat = cx.state.arrangement.cursor;
    let on_track = cursor_track(cx).map(|(track, name)| (track, format!("Add to {name} at the edit cursor")));
    let label = on_track.as_ref().map_or("Add to the selected track", |(_, label)| label.as_str());
    if MenuItem::new(label).enabled(on_track.is_some()).show(ui, theme).clicked()
        && let Some((track, _)) = on_track
    {
        add_to(cx, sound, DropTarget::Lane { track, beat });
    }
    if MenuItem::new("Add to a new track").show(ui, theme).clicked() {
        add_to(cx, sound, DropTarget::NewTrack { beat, colors: dragging::new_track_colors(cx, 1) });
    }
    trill_noir::menu_separator(ui, theme);
    if MenuItem::new(if favorite { "Remove from the favorites" } else { "Add to the favorites" })
        .show(ui, theme)
        .clicked()
    {
        toggle_favorite(cx, &sound.path, favorite);
    }
    if let Some(folder) = sound.path.parent()
        && MenuItem::new("Show only its folder").show(ui, theme).clicked()
    {
        let browser = &mut cx.state.browser.sounds;
        browser.folder = Some(folder.to_path_buf());
        browser.folders_open = true;
        // Unfold the way down to it.
        browser.expanded.extend(folder.ancestors().skip(1).map(Path::to_path_buf));
        browser.reveal = true;
    }
    trill_noir::menu_separator(ui, theme);
    if MenuItem::new(reveal_label()).show(ui, theme).clicked() {
        reveal(cx, sound.path.clone());
    }
    if MenuItem::new("Copy path").show(ui, theme).clicked() {
        cx.state.copied_text = Some(sound.path.display().to_string());
    }
}

/// The strip along the bottom: the chosen sound's waveform, with where the
/// audition has got to, and what is known about the file.
fn preview_strip(ui: &mut Ui, cx: &mut Cx, area: Rect) {
    let theme = cx.theme;
    let id = ui.scope_id().with("sound-preview");
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme.bg);
    paint::hline(&painter, area.x_range(), area.top(), theme.hairline);
    let inner = area.shrink2(vec2(EDGE, 0.0));
    let top_line = Rect::from_min_size(pos2(inner.left(), area.top() + 6.0), vec2(inner.width(), 18.0));

    // Auto-preview and the volume are there whether or not a sound is.
    let auto = cx.state.browser.sounds.auto_preview;
    let spec = Chip::new("Auto", auto).named("Preview on selection");
    let width = spec.width(ui, theme);
    let auto_rect = Rect::from_min_size(pos2(top_line.right() - width, top_line.top()), vec2(width, theme.metric.chip));
    let response = trill_noir::chip(ui, theme, auto_rect, id.with("auto"), &spec);
    cx.hint(&response, "Play a sound as soon as it is selected");
    if response.clicked() {
        cx.state.browser.sounds.auto_preview = !auto;
    }
    let gain = cx.state.browser.sounds.preview_gain_db;
    let (low, high) = PREVIEW_GAIN;
    let shown = units::format_db(gain);
    let volume = Rect::from_min_max(
        pos2(auto_rect.left() - 3.0 - 62.0, top_line.top()),
        pos2(auto_rect.left() - 3.0, top_line.top() + theme.metric.bar),
    );
    let bar = Bar::compact("Preview volume", &shown, (gain - low) / (high - low)).default((-6.0 - low) / (high - low));
    let output = trill_noir::bar(ui, theme, volume, id.with("volume"), &bar);
    cx.hint(
        &output.response,
        format!("Preview volume \u{00B7} {shown} \u{00B7} drag to set, double-click for \u{2212}6 dB"),
    );
    if let Some(position) = output.frac {
        cx.state.browser.sounds.preview_gain_db =
            if output.reset { -6.0 } else { (low + position * (high - low)).round() };
    }

    let Some(path) = cx.state.browser.sounds.selected.clone() else {
        let room = Rect::from_min_max(pos2(inner.left(), top_line.top()), pos2(volume.left() - 6.0, top_line.bottom()));
        text::paint(&painter, room, Align2::LEFT_CENTER, theme.text.caps, "Preview", theme.ink_faint);
        let wave = wave_rect(area, inner);
        paint::hline(&painter, wave.x_range(), wave.center().y, theme.ghost);
        let line =
            Rect::from_min_max(pos2(inner.left(), wave.bottom() + 2.0), pos2(inner.right(), area.bottom() - 4.0));
        text::paint(
            &painter,
            line,
            Align2::LEFT_CENTER,
            theme.text.micro,
            "Select a sound to hear it",
            theme.ink_faint,
        );
        return;
    };
    let playing = cx.sounds.preview.position(&path);
    let loading = cx.sounds.preview.loading.as_deref() == Some(path.as_path());

    // Play or stop, and the name.
    let button = Rect::from_min_size(top_line.min, vec2(18.0, 18.0));
    let icon = if playing.is_some() || loading { Icon::Stop } else { Icon::Play };
    let response = trill_noir::icon_button(ui, theme, button, id.with("play"), icon, playing.is_some(), theme.ink_dim);
    // "Play" and "Stop" are the transport's.
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, playing.is_some(), "Preview"));
    cx.hint(
        &response,
        if playing.is_some() { "Stop the preview \u{00B7} Esc" } else { "Play this sound \u{00B7} Enter" },
    );
    if response.clicked() {
        toggle_preview(cx, &path, playing.is_some() || loading);
    }
    let name =
        Rect::from_min_max(pos2(button.right() + 6.0, top_line.top()), pos2(volume.left() - 6.0, top_line.bottom()));
    text::paint(&painter, name, Align2::LEFT_CENTER, theme.text.body, &file_stem(&path), theme.ink);

    let wave = wave_rect(area, inner);
    let response = ui.interact(wave, id.with("wave"), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Preview waveform"));
    cx.hint(&response, "Click to play \u{00B7} drag into the song, onto a sampler or onto a pad");
    let state = cx.sounds.wave(&path.to_string_lossy());
    let facts_line =
        Rect::from_min_max(pos2(inner.left(), wave.bottom() + 2.0), pos2(inner.right(), area.bottom() - 4.0));
    match &state {
        WaveState::Ready(data) => {
            let columns = trill_noir::waveform_columns(&painter, wave.width());
            let bands: Vec<(f32, f32)> = data
                .columns(0.0..data.seconds(), columns)
                .iter()
                .map(|column| (column.mixed.min, column.mixed.max))
                .collect();
            let mut mesh = Mesh::default();
            let ink = if response.hovered() { theme.ink } else { theme.ink_dim };
            trill_noir::waveform(&mut mesh, wave, &bands, 1.0, ink, paint::hair(&painter));
            painter.add(Shape::mesh(Arc::new(mesh)));
            text::paint(
                &painter,
                facts_line,
                Align2::LEFT_CENTER,
                theme.text.micro,
                &facts(&data.facts),
                theme.ink_faint,
            );
        }
        WaveState::Loading => {
            paint::hline(&painter, wave.x_range(), wave.center().y, theme.ghost);
            text::paint(
                &painter,
                facts_line,
                Align2::LEFT_CENTER,
                theme.text.micro,
                "Reading the file\u{2026}",
                theme.ink_faint,
            );
        }
        WaveState::Failed(reason) => {
            paint::hline(&painter, wave.x_range(), wave.center().y, theme.ghost);
            text::paint(&painter, facts_line, Align2::LEFT_CENTER, theme.text.micro, reason, theme.warn);
        }
    }
    if let Some(position) = playing {
        paint::vline(&painter, wave.left() + wave.width() * position, wave.y_range(), theme.signal);
    }
    if response.drag_started() {
        let known = state.ready().cloned();
        let seconds = known.as_ref().map(|wave| wave.seconds());
        let tempo = known.as_ref().and_then(|wave| wave.facts.tempo);
        let tags = known.as_ref().map_or(Tags::EMPTY, |wave| wave.facts.tags);
        dragging::start(cx, vec![path.clone()], seconds, tempo, tags);
    } else if response.clicked() {
        toggle_preview(cx, &path, playing.is_some() || loading);
    }
}

/// Where the waveform sits in the strip.
fn wave_rect(area: Rect, inner: Rect) -> Rect {
    Rect::from_min_max(pos2(inner.left(), area.top() + 30.0), pos2(inner.right(), area.bottom() - 22.0))
}

fn toggle_preview(cx: &mut Cx, path: &Path, sounding: bool) {
    if sounding {
        cx.sounds.stop_preview();
    } else {
        let gain = cx.state.browser.sounds.preview_gain_db;
        cx.sounds.preview(path, gain);
    }
}

/// `"2.4 s · 140 BPM · F#m · 44.1 kHz · stereo"`.
pub(in crate::ui) fn facts(facts: &trill_runtime::SoundFacts) -> String {
    let mut parts = vec![format_seconds(facts.seconds)];
    if let Some(tempo) = facts.tempo {
        parts.push(format!("{} BPM", format_tempo(tempo)));
    }
    if let Some(key) = facts.key {
        parts.push(key.name().to_owned());
    }
    let khz = f64::from(facts.sample_rate) / 1000.0;
    parts.push(if (khz - khz.round()).abs() < 0.005 { format!("{khz:.0} kHz") } else { format!("{khz:.1} kHz") });
    parts.push(match facts.channels {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        channels => format!("{channels} channels"),
    });
    parts.join(" \u{00B7} ")
}

// ---- the keyboard ----------------------------------------------------------

/// The keys of the sound list, read before the window's shortcuts while the
/// list is being worked in: the arrows (and Page Up, Page Down, Home, End)
/// move through it, Enter and Space play the chosen sound, and typing goes
/// to the search field. `typing` says a text field has the keyboard, which
/// for the list's purposes only counts when it is its own search field.
pub fn keys(ctx: &egui::Context, cx: &mut Cx, typing: bool) {
    let browser = &cx.state.browser;
    let working =
        browser.open && browser.tab == crate::ui::state::BrowserTab::Sounds && cx.state.focus == FocusArea::Browser;
    let in_search = browser.sounds.search_focused;
    if !working || cx.jobs.confirm.is_some() || cx.state.shortcuts_open || (typing && !in_search) {
        return;
    }
    let mut step = 0isize;
    let mut jump = None;
    let mut play = false;
    let mut typed = String::new();
    let mut erase = false;
    ctx.input_mut(|input| {
        let mut take = |key: Key| input.consume_key(Modifiers::NONE, key);
        if take(Key::ArrowDown) {
            step += 1;
        }
        if take(Key::ArrowUp) {
            step -= 1;
        }
        if take(Key::PageDown) {
            step += 12;
        }
        if take(Key::PageUp) {
            step -= 12;
        }
        if take(Key::Enter) {
            play = true;
        }
        if in_search {
            // Everything else is the field's.
            return;
        }
        if take(Key::Home) {
            jump = Some(0);
        }
        if take(Key::End) {
            jump = Some(usize::MAX);
        }
        if take(Key::Space) {
            play = true;
        }
        if take(Key::Backspace) {
            erase = true;
        }
        // What is typed is a search. The key presses behind the text go
        // too, or they would be read as shortcuts.
        let plain = |modifiers: &Modifiers| !modifiers.command && !modifiers.ctrl && !modifiers.alt;
        if plain(&input.modifiers) {
            for event in &input.events {
                if let Event::Text(text) = event {
                    typed.push_str(text);
                }
            }
            if !typed.is_empty() {
                input.events.retain(|event| match event {
                    Event::Text(_) => false,
                    Event::Key { key, modifiers, .. } => !plain(modifiers) || !is_character_key(*key),
                    _ => true,
                });
            }
        }
    });

    let sounds = &mut cx.state.browser.sounds;
    // Space was the preview key; the search starts with the first letter.
    let typed: String = typed.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
    if !typed.is_empty() {
        sounds.search.push_str(&typed);
        sounds.focus_search = true;
    }
    if erase && sounds.search.pop().is_some() {
        sounds.focus_search = true;
    }
    if step != 0 || jump.is_some() {
        move_selection(cx, step, jump);
    } else if play && let Some(path) = cx.state.browser.sounds.selected.clone() {
        let gain = cx.state.browser.sounds.preview_gain_db;
        cx.sounds.preview(&path, gain);
    }
}

/// Whether a press of `key` types a character.
fn is_character_key(key: Key) -> bool {
    !matches!(
        key,
        Key::ArrowDown
            | Key::ArrowLeft
            | Key::ArrowRight
            | Key::ArrowUp
            | Key::Escape
            | Key::Tab
            | Key::Backspace
            | Key::Enter
            | Key::Insert
            | Key::Delete
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12
    )
}

/// Moves the selection `step` rows (or to row `jump`), brings it into
/// view, and plays it if sounds play as they are chosen.
fn move_selection(cx: &mut Cx, step: isize, jump: Option<usize>) {
    let Some(found) = &cx.sounds.search.found else { return };
    let count = found.results.len();
    if count == 0 {
        return;
    }
    let current = cx.state.browser.sounds.selected_row;
    let row = match (jump, current) {
        (Some(row), _) => row.min(count - 1),
        // With nothing chosen, the first press chooses the first row.
        (None, None) => 0,
        (None, Some(row)) => row.saturating_add_signed(step).min(count - 1),
    };
    let Some(sound) = SoundRow::at(found.results.index(), found.results.ids(), row) else { return };
    if current == Some(row) && cx.state.browser.sounds.selected.as_deref() == Some(sound.path.as_path()) {
        return;
    }
    let play = cx.state.browser.sounds.auto_preview;
    choose(cx, row, &sound.path, play);
    cx.state.browser.sounds.reveal = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filters_become_a_library_query() {
        let mut browser = SoundBrowser::default();
        let plain = query(&browser, 120.0);
        assert_eq!(plain.kind, Some(EntryKind::Audio), "sounds only");
        assert!(plain.text.is_empty() && plain.tags.is_empty() && plain.tempo.is_none() && plain.key.is_none());
        assert!(plain.folder.is_none() && !plain.favorites_only && plain.limit.is_none());

        browser.search = "deep kick".into();
        browser.form = SoundForm::OneShot;
        browser.tag = Some(Tag::Kick);
        browser.tempo = TempoFilter::Song;
        browser.key = MusicalKey::parse("F#m");
        browser.favorites = true;
        browser.folder = Some("C:/Samples/Drums".into());
        let narrowed = query(&browser, 96.5);
        assert_eq!(narrowed.text, "deep kick");
        assert_eq!(narrowed.tags, Tags::of(&[Tag::OneShot, Tag::Kick]));
        assert_eq!(narrowed.tempo, Some(93.5..=99.5), "within three beats per minute of the song");
        assert_eq!(narrowed.key, MusicalKey::parse("Gb minor"));
        assert!(narrowed.favorites_only);
        assert_eq!(narrowed.folder.as_deref(), Some(Path::new("C:/Samples/Drums")));

        browser.form = SoundForm::Loop;
        browser.tag = None;
        browser.tempo = TempoFilter::Between(110.0, 129.0);
        let loops = query(&browser, 96.5);
        assert_eq!(loops.tags, Tags::of(&[Tag::Loop]));
        assert_eq!(loops.tempo, Some(110.0..=129.0));
    }

    #[test]
    fn lengths_tempos_and_counts_read_briefly() {
        assert_eq!(format_seconds(0.42), "420 ms");
        assert_eq!(format_seconds(0.9996), "1.0 s");
        assert_eq!(format_seconds(2.44), "2.4 s");
        assert_eq!(format_seconds(59.9), "59.9 s");
        assert_eq!(format_seconds(67.4), "1:07");
        assert_eq!(format_seconds(3600.0), "60:00");
        assert_eq!(format_seconds(f64::NAN), "");
        assert_eq!(format_tempo(140.0), "140");
        assert_eq!(format_tempo(87.5), "87.5");
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(100_000), "100,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(tag_title(Tag::Kick), "Kick");
        assert_eq!(tag_title(Tag::Sub808), "808");
    }

    #[test]
    fn a_files_facts_fit_on_a_line() {
        let mut known = trill_runtime::SoundFacts {
            seconds: 2.4,
            sample_rate: 44_100,
            channels: 2,
            tempo: Some(140.0),
            key: MusicalKey::parse("F#m"),
            tags: Tags::EMPTY,
        };
        assert_eq!(facts(&known), "2.4 s \u{00B7} 140 BPM \u{00B7} F#m \u{00B7} 44.1 kHz \u{00B7} stereo");
        known.tempo = None;
        known.key = None;
        known.sample_rate = 48_000;
        known.channels = 1;
        known.seconds = 0.31;
        assert_eq!(facts(&known), "310 ms \u{00B7} 48 kHz \u{00B7} mono");
    }

    #[test]
    fn keys_that_type_are_told_from_keys_that_move() {
        for key in [Key::A, Key::Z, Key::Num0, Key::Space, Key::Minus, Key::Period] {
            assert!(is_character_key(key), "{key:?}");
        }
        for key in [Key::ArrowDown, Key::Enter, Key::Escape, Key::F1, Key::Delete, Key::Backspace, Key::Tab] {
            assert!(!is_character_key(key), "{key:?}");
        }
    }
}
