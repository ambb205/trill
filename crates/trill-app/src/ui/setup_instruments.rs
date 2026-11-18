//! The Instruments tab of the Setup panel: the catalogue of sampled
//! instruments that others publish (`trill-catalogue`), and fetching from
//! it.
//!
//! The catalogue is more than a hundred sets, so it is not a list of
//! parts: it is shown by kind, each kind opening to its sets and a set to
//! its instruments, with a field that narrows all of it. Over it stand the
//! two things most people ask for, each with what it takes: the starter
//! selection (the best of each kind that was found) and everything. A
//! kind, or one set, is fetched alone from its own row.
//!
//! The rules of the panel hold here as they do for any part. Nothing is
//! fetched that was not read first: a set's licence in its maker's words,
//! where that was read, who asks to be named and where it comes from stand
//! over the button that fetches it, and the two large asks say their size
//! on the button itself. Nothing here fetches by itself. An instrument
//! that something measured speaks against is listed with what was found,
//! and is not put forward: it is fetched with its set, or by its set's
//! own button, and never ticked for the starter selection.
//!
//! What is asked for at once is shown as one thing: how many of its sets
//! are here, how much has come, and one button that stops all of it. What
//! was whole when it was stopped is kept, and asking again goes on.

use super::setup::{self, GAP, ROW, WIDTH, fact, heading, paragraph, row_button};
use super::state::Cx;
use eframe::egui::{self, Align, Align2, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use trill_catalogue::{Catalogue, Fetch, Instrument, Kind, Licence, Mark, Search, Set};
use trill_core::pitch::pitch_name;
use trill_fetch::{ListStatus, PartState, PartStatus, said_closely};
use trill_noir::{TextStyle, Theme, Variant, paint, text};
use trill_runtime::Ask;

/// How many rows of the catalogue are shown before they scroll.
const TREE_ROWS: usize = 13;
/// How far the rows of a kind's sets, and of a set's instruments, stand in.
const INDENT: f32 = 18.0;

/// What the tab keeps from one frame to the next.
#[derive(Default)]
pub struct Instruments {
    /// What the field narrows the catalogue to.
    pub(super) search: String,
    /// The kinds that are opened to their sets.
    pub(super) open_kinds: HashSet<Kind>,
    /// The sets that are opened to their instruments.
    pub(super) open_sets: HashSet<String>,
    /// The set whose details are shown.
    pub(super) selected: Option<String>,
    /// What was last asked for at once, by what it is called and the sets
    /// it means: shown as one thing while any of it is on its way.
    pub(super) asked: Option<(String, Vec<String>)>,
}

// ---- what the tab says, as pure functions --------------------------------------

/// A licence in a few letters, for a row.
pub(super) fn licence_short(licence: Licence) -> &'static str {
    match licence {
        Licence::Cc0 => "CC0",
        Licence::Unlicense => "Unlicense",
        Licence::CcBy3 => "CC BY 3.0",
        Licence::CcBy4 => "CC BY 4.0",
    }
}

/// Why an instrument is not put forward: each thing that was measured
/// against it, with the figure it was found by. Empty for one that is.
pub(super) fn why_not_put_forward(instrument: &Instrument) -> Vec<String> {
    instrument
        .marks
        .iter()
        .filter(|mark| mark.is_fault())
        .map(|mark| match (mark, instrument.noise, instrument.tuning, instrument.evenness) {
            (Mark::Noisy, Some((middle, _)), _, _) => format!(
                "Its recordings carry noise that is heard: the quietest part of one lies {middle:.0} dB under its peak"
            ),
            (Mark::OutOfTune, _, Some((middle, usual, _)), _) => format!(
                "It is out of tune: its keys lie {middle:+.0} cents from equal temperament, and usually {usual:.0} cents from one another"
            ),
            (Mark::Uneven, _, _, Some((usual, _))) => {
                format!("Its level jumps from key to key: by {usual:.0} dB from one recorded key to the next")
            }
            (Mark::Silent, ..) => match &instrument.note {
                Some(note) if note.contains("controller") => {
                    "It is silent as it loads: its notes sound once a controller of its own is moved".to_owned()
                }
                _ => "It is silent as it loads".to_owned(),
            },
            (mark, ..) => {
                let words = mark.words();
                let mut letters = words.chars();
                letters.next().map_or_else(String::new, |first| first.to_uppercase().chain(letters).collect())
            }
        })
        .collect()
}

/// What an instrument is, in a line for its row: its keys, layers and takes.
pub(super) fn instrument_facts(instrument: &Instrument) -> String {
    let mut words = Vec::new();
    match (instrument.range, instrument.drums) {
        (Some((low, high)), true) => words.push(format!("Drums {}\u{2013}{}", pitch_name(low), pitch_name(high))),
        (Some((low, high)), false) => words.push(format!("{}\u{2013}{}", pitch_name(low), pitch_name(high))),
        (None, true) => words.push("Drums".to_owned()),
        (None, false) => {}
    }
    if instrument.layers > 1 {
        words.push(format!("{} layers", instrument.layers));
    }
    if instrument.takes > 1 {
        words.push(format!("{} takes", instrument.takes));
    }
    words.join(" \u{00B7} ")
}

/// One kind of the catalogue as its row shows it.
pub(super) struct KindRow<'a> {
    pub kind: Kind,
    /// The sets with an instrument of the kind that the search lets through.
    pub sets: Vec<&'a Set>,
    /// How many instruments of the kind those sets have that the search lets through.
    pub instruments: usize,
}

/// The catalogue by kind, narrowed to what `search` finds: the sets of
/// each kind in the catalogue's own order, and the instruments (by their
/// file's checksum) that are shown. Marked instruments are among them:
/// here they are listed with what was found, not left out.
pub(super) fn by_kind<'a>(catalogue: &'a Catalogue, search: &str) -> (Vec<KindRow<'a>>, HashSet<&'a str>) {
    let found = catalogue.find(&Search { words: search.to_owned(), with_faults: true, ..Search::default() });
    let shown: HashSet<&str> = found.iter().map(|(_, instrument)| instrument.text.as_str()).collect();
    let mut rows = Vec::new();
    for kind in Kind::ALL {
        let of_kind = |instrument: &&Instrument| instrument.kind == kind && shown.contains(instrument.text.as_str());
        let sets: Vec<&Set> =
            catalogue.sets.iter().filter(|set| set.instruments.iter().any(|one| of_kind(&one))).collect();
        if sets.is_empty() {
            continue;
        }
        let instruments = sets.iter().map(|set| set.instruments.iter().filter(of_kind).count()).sum();
        rows.push(KindRow { kind, sets, instruments });
    }
    (rows, shown)
}

/// How an ask stands: what it would fetch, and what its button says.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AskWords {
    /// The words on its button: what it is called, and what it takes.
    pub label: String,
    /// Whether pressing it would ask for anything.
    pub idle: bool,
    /// How many sets it means, and how many of them are here.
    pub sets: usize,
    pub here: usize,
}

/// What the button of an ask for `sets` says, called `name`.
pub(super) fn ask_words(name: &str, sets: &[&Set], status: &HashMap<&str, &PartStatus>) -> AskWords {
    let state = |set: &Set| status.get(set.id.as_str()).map_or(PartState::Missing, |status| status.state);
    let all: u64 = sets.iter().map(|set| set.fetched_bytes()).sum();
    let here = sets.iter().filter(|set| state(set) == PartState::Ready).count();
    let going = sets.iter().filter(|set| state(set).is_going()).count();
    let left: u64 = sets.iter().filter(|set| state(set) == PartState::Missing).map(|set| set.fetched_bytes()).sum();
    let (label, idle) = if here == sets.len() {
        (format!("{name} \u{00B7} here"), true)
    } else if left == 0 && going > 0 {
        (format!("{name} \u{00B7} on its way"), true)
    } else if left < all {
        (format!("{name} \u{00B7} {} left", said_closely(left)), false)
    } else {
        (format!("{name} \u{00B7} {}", said_closely(all)), false)
    };
    AskWords { label, idle, sets: sets.len(), here }
}

/// What is asked for at once, as one thing: how many of its sets are here,
/// how much of it has come, and which set is on its way.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Whole {
    pub words: String,
    /// How much of all of it has come, 0..=1.
    pub share: f32,
}

/// The whole of what is on its way: the sets of `asked` (what was last
/// asked for at once), or failing that whatever of the list is going.
/// `None` when nothing is.
pub(super) fn whole(list: &ListStatus, asked: Option<&(String, Vec<String>)>) -> Option<Whole> {
    if !list.parts.iter().any(|status| status.state.is_going()) {
        return None;
    }
    let of_ask = |status: &&PartStatus| match asked {
        Some((_, ids)) => ids.contains(&status.part.id) || status.state.is_going(),
        None => status.state.is_going(),
    };
    let parts: Vec<&PartStatus> = list.parts.iter().filter(of_ask).collect();
    let all: u64 = parts.iter().map(|status| status.part.size()).sum();
    let come: u64 = parts
        .iter()
        .map(|status| match status.state {
            PartState::Ready => status.part.size(),
            _ => status.got.min(status.part.size()),
        })
        .sum();
    let here = parts.iter().filter(|status| status.state == PartState::Ready).count();
    let active = parts.iter().find(|status| status.state.is_going() && status.state != PartState::Waiting);
    let what = match active {
        Some(status) => {
            let doing = match status.state {
                PartState::Checking => "Checking",
                PartState::Unpacking => "Unpacking",
                _ => "Fetching",
            };
            format!("{doing} {}", status.part.name)
        }
        None => "Waiting".to_owned(),
    };
    let name = asked.map_or_else(String::new, |(name, _)| format!("{name}: "));
    Some(Whole {
        words: format!(
            "{name}{here} of {} sets here \u{00B7} {} of {} \u{00B7} {what}",
            parts.len(),
            said_closely(come),
            said_closely(all)
        ),
        share: come as f32 / all.max(1) as f32,
    })
}

/// What asking for several sets came to, as a sentence.
fn asked_words(name: &str, asked: &trill_fetch::Asked) -> String {
    match (asked.queued.len(), asked.going) {
        (0, 0) => format!("{name}: all of it is here already."),
        (0, going) => format!("{name}: its {going} set{} that are not here are on their way already.", plural(going)),
        (count, _) => format!(
            "{name}: {count} set{} asked for, {}. They are fetched one after the other, the smallest first.",
            plural(count),
            said_closely(asked.bytes)
        ),
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Where a set is, in a word or two: as a part says it, and for one that
/// was not fetched here but lies in the library all the same, that.
pub(super) fn set_state(set: &Set, status: Option<&PartStatus>, here: &HashMap<String, PathBuf>) -> (String, bool) {
    let in_library = !set.instruments.is_empty() && set.instruments.iter().all(|one| here.contains_key(&one.text));
    match status {
        Some(status) if status.state == PartState::Missing && status.error.is_none() && in_library => {
            ("In your library".to_owned(), false)
        }
        Some(status) => setup::state_words(status),
        None => ("Not offered here".to_owned(), false),
    }
}

// ---- asking ----------------------------------------------------------------------

fn ask(ctx: &egui::Context, cx: &mut Cx, name: &str, ask: Ask) {
    let catalogue_ids: Vec<String> =
        ask.sets(cx.sounds.runtime.catalogue()).into_iter().map(|set| set.id.clone()).collect();
    cx.setup.instruments.asked = Some((name.to_owned(), catalogue_ids));
    let name = name.to_owned();
    setup::act(ctx, cx, move |runtime| Ok(asked_words(&name, &runtime.ask_for_instruments(&ask)?)));
}

fn stop_all(ctx: &egui::Context, cx: &mut Cx) {
    setup::act(ctx, cx, move |runtime| {
        let stopped = runtime.stop_instruments()?;
        Ok(match stopped {
            0 => String::new(),
            _ => "Stopped. The sets that were whole are kept, and so is what came of one that is fetched file by \
                  file: asking again goes on from there. A set that comes as one archive begins again."
                .to_owned(),
        })
    });
}

// ---- the tab ---------------------------------------------------------------------

/// A small triangle that says whether a row is opened.
fn disclosure(ui: &Ui, theme: &Theme, at: egui::Pos2, open: bool) {
    let points = if open {
        vec![pos2(at.x - 4.0, at.y - 2.5), pos2(at.x + 4.0, at.y - 2.5), pos2(at.x, at.y + 3.0)]
    } else {
        vec![pos2(at.x - 2.5, at.y - 4.0), pos2(at.x + 3.0, at.y), pos2(at.x - 2.5, at.y + 4.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(points, theme.ink_faint, egui::Stroke::NONE));
}

/// A word in small capitals at the right of `room`; answers with what is left of the room.
fn tag(ui: &Ui, theme: &Theme, room: Rect, words: &str, color: egui::Color32) -> Rect {
    let used = text::paint(ui.painter(), room, Align2::RIGHT_CENTER, theme.text.micro, words, color);
    room.with_max_x(room.right() - used - 12.0)
}

/// The tab: see the notes at the top.
pub(super) fn body(ui: &mut Ui, ctx: &egui::Context, cx: &mut Cx, list: &ListStatus) {
    let theme = cx.theme;
    let runtime = cx.sounds.runtime.clone();
    let catalogue = runtime.catalogue();
    let status: HashMap<&str, &PartStatus> =
        list.parts.iter().map(|status| (status.part.id.as_str(), status)).collect();
    let here = cx.setup.here();

    paragraph(ui, theme, theme.ink_dim, &list.about, WIDTH);
    ui.add_space(8.0);
    setup::kept_line(
        ui,
        ctx,
        cx,
        setup::Kept::List(list.id.clone()),
        &list.folder,
        "Keep the sets in another folder, and move the ones that are here: on a disk with more room, say. The folder is one of your sound library's",
    );
    ui.add_space(8.0);

    // The two asks, each with what it takes, and how much of the catalogue is here.
    let ready = list.parts.iter().filter(|status| status.state == PartState::Ready).count();
    let on_disk: u64 = list.parts.iter().map(|status| status.on_disk).sum();
    let (asks, response) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
    let count = format!("{ready} of {} sets here \u{00B7} {} on disk", list.parts.len(), said_closely(on_disk));
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &count));
    let layout = Layout::left_to_right(Align::Center);
    let mut cell = ui.new_child(UiBuilder::new().id_salt("instrument-asks").max_rect(asks).layout(layout));
    cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
    if cx.setup.is_working() {
        cell.disable();
    }
    let starter = ask_words("Starter", &Ask::Starter.sets(catalogue), &status);
    let everything = ask_words("Everything", &Ask::Everything.sets(catalogue), &status);
    let pressed = trill_noir::button(&mut cell, theme, &starter.label, Variant::Primary);
    cx.hint(
        &pressed,
        "Fetch the starter selection: the best of each kind that was found, by what was measured. Each set's licence is in its row below",
    );
    if pressed.clicked() && !starter.idle {
        ask(ctx, cx, "Starter", Ask::Starter);
    }
    let pressed = trill_noir::button(&mut cell, theme, &everything.label, Variant::Default);
    cx.hint(&pressed, "Fetch every set of the catalogue, one after the other, the smallest first");
    if pressed.clicked() && !everything.idle {
        ask(ctx, cx, "Everything", Ask::Everything);
    }
    let room = asks.with_min_x(cell.min_rect().right() + 12.0);
    text::paint_name(ui.painter(), room, Align2::RIGHT_CENTER, theme.text.micro, &count, theme.ink_faint);

    // What is on its way, as one thing that can be stopped.
    if let Some(whole) = whole(list, cx.setup.instruments.asked.as_ref()) {
        ui.add_space(6.0);
        let (row, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &whole.words));
        if row_button(
            ui,
            cx,
            row,
            "Stop",
            "Stop fetching instruments",
            false,
            "Stop everything that is on its way: what is whole is kept, and asking again goes on",
        ) {
            stop_all(ctx, cx);
        }
        let style = TextStyle { size: 12.0, ..theme.text.body };
        let words = row.with_max_x(row.right() - 60.0);
        text::paint_name(ui.painter(), words, Align2::LEFT_CENTER, style, &whole.words, theme.ink);
        let bar = Rect::from_min_max(pos2(row.left(), row.bottom() - 2.0), row.max);
        paint::progress(ui.painter(), bar, whole.share, theme.well, theme.signal);
    }

    // The field that narrows the catalogue.
    ui.add_space(10.0);
    let (field, response) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, "Search the catalogue"));
    let id = ui.scope_id().with("instrument-search");
    let response = trill_noir::text_field(
        ui,
        theme,
        field,
        id,
        &mut cx.setup.instruments.search,
        "Search the catalogue: a kind, an instrument, a maker",
    );
    cx.hint(
        &response,
        "Type to narrow the catalogue: every word has to be in an instrument's name, what it is, its set or its maker",
    );
    let search = cx.setup.instruments.search.trim().to_owned();
    let searching = !search.is_empty();
    let (kinds, shown) = by_kind(catalogue, &search);
    let sets_shown: HashSet<&str> = kinds.iter().flat_map(|row| &row.sets).map(|set| set.id.as_str()).collect();
    ui.add_space(8.0);
    let found = if searching {
        format!("{} instruments in {} sets", shown.len(), sets_shown.len())
    } else {
        format!("{} instruments in {} sets", catalogue.instruments().count(), catalogue.sets.len())
    };
    heading(ui, theme, "The catalogue, by kind", &found);
    if kinds.is_empty() {
        paragraph(ui, theme, theme.ink_dim, "Nothing in the catalogue is called that.", WIDTH);
        return;
    }

    // The rows: a kind, under it its sets where it is opened, under a set its instruments.
    enum Row<'a> {
        Kind(&'a KindRow<'a>),
        Set(&'a Set),
        Instrument(&'a Set, &'a Instrument),
    }
    let mut rows = Vec::new();
    for kind in &kinds {
        rows.push(Row::Kind(kind));
        // While something is searched for, what it found is shown opened.
        if !(searching || cx.setup.instruments.open_kinds.contains(&kind.kind)) {
            continue;
        }
        for set in kind.sets.iter().copied() {
            rows.push(Row::Set(set));
            if searching || cx.setup.instruments.open_sets.contains(&set.id) {
                let of_kind =
                    set.instruments.iter().filter(|one| one.kind == kind.kind && shown.contains(one.text.as_str()));
                rows.extend(of_kind.map(|one| Row::Instrument(set, one)));
            }
        }
    }
    let selected = cx.setup.instruments.selected.clone();
    let (mut toggle_kind, mut choose_set, mut fetch_kind) = (None, None, None);
    let tall = rows.len().min(TREE_ROWS) as f32 * ROW;
    let tree = egui::ScrollArea::vertical()
        .id_salt("instrument-tree")
        .max_height(tall)
        .min_scrolled_height(tall)
        .auto_shrink([false, true]);
    let style = TextStyle { size: 12.0, ..theme.text.body };
    tree.show(ui, |ui| {
        for (index, row) in rows.iter().enumerate() {
            let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, ROW), Sense::click());
            if index > 0 {
                paint::hline(ui.painter(), rect.x_range(), rect.top(), theme.grid);
            }
            match row {
                Row::Kind(kind) => {
                    let open = searching || cx.setup.instruments.open_kinds.contains(&kind.kind);
                    let bytes: u64 = kind.sets.iter().map(|set| set.fetched_bytes()).sum();
                    let ready = kind
                        .sets
                        .iter()
                        .filter(|set| status.get(set.id.as_str()).is_some_and(|one| one.state == PartState::Ready))
                        .count();
                    let mut facts = format!(
                        "{} set{} \u{00B7} {} instrument{} \u{00B7} {}",
                        kind.sets.len(),
                        plural(kind.sets.len()),
                        kind.instruments,
                        plural(kind.instruments),
                        said_closely(bytes)
                    );
                    if ready > 0 {
                        facts.push_str(&format!(" \u{00B7} {ready} here"));
                    }
                    let spoken = format!("{} \u{00B7} {facts}", kind.kind.name());
                    response.widget_info(|| WidgetInfo::selected(Role::Button, true, open, &spoken));
                    if response.hovered() {
                        ui.painter().rect_filled(rect, 0.0, theme.hover);
                    }
                    if response.clicked() && !searching {
                        toggle_kind = Some(kind.kind);
                    }
                    cx.hint(&response, "Open to its sets");
                    disclosure(ui, theme, pos2(rect.left() + 7.0, rect.center().y), open);
                    // One kind, fetched alone: the sets of it that are put forward and not here.
                    let wanted = ask_words("Fetch", &Ask::Kind(kind.kind).sets(catalogue), &status);
                    let mut room = rect.with_min_x(rect.left() + INDENT);
                    if !wanted.idle && !searching {
                        let name = format!("Fetch {}", kind.kind.name());
                        let hint =
                            "Fetch the sets of this kind that are not here, the ones nothing measured speaks against";
                        let working = cx.setup.is_working();
                        if row_button(ui, cx, rect, &wanted.label, &name, working, hint) {
                            fetch_kind = Some(kind.kind);
                        }
                        room = room.with_max_x(rect.right() - 150.0);
                    }
                    let room = tag(ui, theme, room, &facts.to_uppercase(), theme.ink_faint);
                    text::paint_name(
                        ui.painter(),
                        room,
                        Align2::LEFT_CENTER,
                        theme.text.caps,
                        kind.kind.name(),
                        theme.ink,
                    );
                }
                Row::Set(set) => {
                    let part = status.get(set.id.as_str()).copied();
                    let (state, wrong) = set_state(set, part, &here);
                    let size = said_closely(set.fetched_bytes());
                    let licence = licence_short(set.licence);
                    let chosen = selected.as_deref() == Some(set.id.as_str());
                    let spoken = format!("{} \u{00B7} {size} \u{00B7} {licence} \u{00B7} {state}", set.name);
                    response.widget_info(|| WidgetInfo::selected(Role::Button, true, chosen, &spoken));
                    if chosen {
                        ui.painter().rect_filled(rect, 0.0, theme.selected);
                    } else if response.hovered() {
                        ui.painter().rect_filled(rect, 0.0, theme.hover);
                    }
                    if response.clicked() {
                        choose_set = Some(set.id.clone());
                    }
                    cx.hint(&response, &set.about);
                    let lamp = pos2(rect.left() + INDENT + 2.0, rect.center().y);
                    let ready = part.is_some_and(|one| one.state == PartState::Ready);
                    let going = part.is_some_and(|one| one.state.is_going());
                    let after = if ready || state == "In your library" {
                        paint::dot(ui.painter(), lamp, theme.metric.dot, theme.ok)
                    } else if wrong {
                        paint::dot(ui.painter(), lamp, theme.metric.dot, theme.warn)
                    } else if going {
                        paint::dot(ui.painter(), lamp, theme.metric.dot, theme.signal)
                    } else {
                        paint::hollow_dot(ui.painter(), lamp, theme.metric.dot, theme.ghost)
                    };
                    let words =
                        Rect::from_min_max(pos2(after + 8.0, rect.top()), pos2(rect.right() - 8.0, rect.bottom()));
                    let color = if wrong { theme.warn } else { theme.ink_dim };
                    let used = text::paint(ui.painter(), words, Align2::RIGHT_CENTER, style, &state, color);
                    let room = words.with_max_x(words.right() - used.max(96.0) - 12.0);
                    let used = text::paint(
                        ui.painter(),
                        room,
                        Align2::RIGHT_CENTER,
                        theme.text.micro,
                        licence,
                        theme.ink_faint,
                    );
                    let room = room.with_max_x(room.right() - used.max(58.0) - 12.0);
                    let used =
                        text::paint(ui.painter(), room, Align2::RIGHT_CENTER, theme.text.value, &size, theme.ink_dim);
                    let mut room = room.with_max_x(room.right() - used.max(48.0) - 12.0);
                    if set.is_starter() {
                        room = tag(ui, theme, room, "STARTER", theme.signal);
                    }
                    let ink = if ready { theme.ink } else { theme.ink_dim };
                    text::paint_name(ui.painter(), room, Align2::LEFT_CENTER, theme.text.body, &set.name, ink);
                    if let Some(one) = part.filter(|one| one.state.is_going() && one.state != PartState::Waiting) {
                        let bar = Rect::from_min_max(pos2(rect.left() + INDENT, rect.bottom() - 2.0), rect.max);
                        let share = one.got as f32 / one.part.size().max(1) as f32;
                        paint::progress(ui.painter(), bar, share, theme.well, theme.signal);
                    }
                }
                Row::Instrument(set, instrument) => {
                    let against = why_not_put_forward(instrument);
                    let facts = instrument_facts(instrument);
                    let found = here.get(&instrument.text);
                    let mut spoken = format!("{} \u{00B7} {facts}", instrument.name);
                    if instrument.is_starter() {
                        spoken.push_str(" \u{00B7} starter");
                    }
                    if !against.is_empty() {
                        spoken.push_str(" \u{00B7} not put forward");
                    }
                    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &spoken));
                    if response.clicked() {
                        choose_set = Some(set.id.clone());
                    }
                    let hint = if against.is_empty() {
                        instrument.words.clone()
                    } else {
                        format!("{}. {}", instrument.words.trim_end_matches('.'), against.join(". "))
                    };
                    cx.hint(&response, &hint);
                    let mut room = Rect::from_min_max(
                        pos2(rect.left() + 2.0 * INDENT + 8.0, rect.top()),
                        pos2(rect.right() - 8.0, rect.bottom()),
                    );
                    if !against.is_empty() {
                        let marks: Vec<&str> =
                            instrument.marks.iter().filter(|mark| mark.is_fault()).map(|mark| mark.id()).collect();
                        room = tag(ui, theme, room, &marks.join(", ").replace('-', " ").to_uppercase(), theme.warn);
                    } else if instrument.is_starter() {
                        room = tag(ui, theme, room, "STARTER", theme.signal);
                    }
                    if found.is_some() {
                        room = tag(ui, theme, room, "HERE", theme.ok);
                    }
                    let used = text::paint(
                        ui.painter(),
                        room,
                        Align2::RIGHT_CENTER,
                        theme.text.micro,
                        &facts,
                        theme.ink_faint,
                    );
                    let room = room.with_max_x(room.right() - used - 12.0);
                    let ink = if against.is_empty() { theme.ink_dim } else { theme.ink_faint };
                    text::paint_name(ui.painter(), room, Align2::LEFT_CENTER, style, &instrument.name, ink);
                }
            }
        }
    });
    if let Some(kind) = toggle_kind {
        let open = &mut cx.setup.instruments.open_kinds;
        if !open.remove(&kind) {
            open.insert(kind);
        }
    }
    if let Some(set) = choose_set {
        // Chosen: its details below, and its instruments under it; chosen again, they are put away.
        let view = &mut cx.setup.instruments;
        if view.selected.as_deref() == Some(set.as_str()) && view.open_sets.contains(&set) {
            view.open_sets.remove(&set);
        } else {
            view.open_sets.insert(set.clone());
        }
        view.selected = Some(set);
    }
    if let Some(kind) = fetch_kind {
        ask(ctx, cx, kind.name(), Ask::Kind(kind));
    }

    // The set that is looked at, with everything there is to read before it is fetched.
    let looked_at = cx
        .setup
        .instruments
        .selected
        .as_deref()
        .filter(|id| sets_shown.contains(id))
        .and_then(|id| catalogue.set(id))
        .or_else(|| kinds.first().and_then(|kind| kind.sets.first().copied()).filter(|_| searching));
    let Some(set) = looked_at else {
        ui.add_space(10.0);
        let words =
            "Open a kind and choose a set to read its licence, who made it and what was measured of its instruments.";
        paragraph(ui, theme, theme.ink_faint, words, WIDTH);
        return;
    };
    ui.add_space(12.0);
    detail(ui, ctx, cx, set, status.get(set.id.as_str()).copied(), &here);
}

/// Everything there is to read of a set: what it is and whose, its licence
/// in its maker's words and where that was read, the line its maker asks
/// to be named by, where it comes from and what it is checked against,
/// its instruments with what was measured against any of them; and under
/// that the button that fetches it, stops its fetch or removes it.
fn detail(
    ui: &mut Ui,
    ctx: &egui::Context,
    cx: &mut Cx,
    set: &Set,
    status: Option<&PartStatus>,
    here: &HashMap<String, PathBuf>,
) {
    let theme = cx.theme;
    let (state, _) = set_state(set, status, here);
    heading(ui, theme, &set.name, &format!("{} \u{00B7} {state}", said_closely(set.fetched_bytes())));
    paragraph(ui, theme, theme.ink, &set.about, WIDTH);
    ui.add_space(8.0);
    fact(ui, theme, "By", &set.by, theme.ink);
    let licence = format!(
        "{}. \u{201C}{}\u{201D} Read at {} on {}.",
        set.licence.name(),
        set.licence_says,
        set.licence_at,
        set.read
    );
    fact(ui, theme, "Licence", &licence, theme.ink);
    match &set.credit {
        Some(line) => fact(
            ui,
            theme,
            "Credit",
            &format!("{line}. Trill gives this line for a song that plays it (File, Credits)."),
            theme.ink,
        ),
        None => fact(ui, theme, "Credit", "None is asked: it is in the public domain.", theme.ink_dim),
    }
    let version = &set.version[..set.version.len().min(12)];
    fact(ui, theme, "From", &format!("{} (as it was at {version})", set.from), theme.ink_dim);
    let checked = match &set.fetch {
        Fetch::Archive { sum, .. } => format!(
            "One archive, made when it is asked for: checked by what it unpacks to, {} files, SHA-256 {}\u{2026} Its size is an estimate",
            set.files,
            &sum[..12.min(sum.len())]
        ),
        Fetch::Files { files, .. } => format!("{} files, each by its SHA-256", files.len()),
    };
    fact(ui, theme, "Checked by", &checked, theme.ink_dim);

    // Its instruments: each with what it is, and what was measured against it.
    ui.add_space(4.0);
    let marked = set.instruments.iter().filter(|one| !one.is_put_forward()).count();
    let count = match marked {
        0 => format!("{}", set.instruments.len()),
        marked => format!("{} \u{00B7} {marked} not put forward", set.instruments.len()),
    };
    heading(ui, theme, "Its instruments", &count);
    for instrument in &set.instruments {
        let against = why_not_put_forward(instrument);
        let mut words = instrument.name.clone();
        let facts = instrument_facts(instrument);
        if !facts.is_empty() {
            words.push_str(&format!(" \u{00B7} {facts}"));
        }
        if instrument.is_starter() {
            words.push_str(" \u{00B7} in the starter selection");
        }
        if here.contains_key(&instrument.text) {
            words.push_str(" \u{00B7} here");
        }
        let color = if against.is_empty() { theme.ink_dim } else { theme.ink_faint };
        paragraph(ui, theme, color, &words, WIDTH);
        if !against.is_empty() {
            let why = format!("Not put forward. {}.", against.join(". "));
            paragraph(ui, theme, theme.warn, &why, WIDTH);
        }
        if let Some(misses) = &instrument.misses {
            paragraph(ui, theme, theme.ink_faint, misses, WIDTH);
        }
        ui.add_space(3.0);
    }
    let Some(status) = status else { return };
    if let Some(why) = &status.error {
        ui.add_space(4.0);
        paragraph(ui, theme, theme.warn, &setup::stopped(why), WIDTH);
    }

    ui.add_space(GAP);
    let (foot, response) = ui.allocate_exact_size(vec2(WIDTH, theme.metric.control), Sense::hover());
    let going = status.state.is_going();
    let note = match (status.state, going) {
        (PartState::Ready, _) => format!("{} on disk, in {}", said_closely(status.on_disk), status.folder.display()),
        (_, true) => format!("{} of {} have come", said_closely(status.got), said_closely(set.fetched_bytes())),
        _ if status.got > 0 => {
            format!(
                "{} of {} are here already: fetching goes on from there",
                said_closely(status.got),
                said_closely(set.fetched_bytes())
            )
        }
        _ => "Nothing is fetched until you press Fetch".to_owned(),
    };
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &note));
    let layout = Layout::right_to_left(Align::Center);
    let mut cell = ui.new_child(UiBuilder::new().id_salt(("set", &set.id)).max_rect(foot).layout(layout));
    cell.spacing_mut().item_spacing = vec2(theme.metric.gap, 0.0);
    if cx.setup.is_working() {
        cell.disable();
    }
    match (status.state, going) {
        (PartState::Ready, _) => {
            if trill_noir::button(&mut cell, theme, "Remove", Variant::Default).clicked() {
                setup::remove(ctx, cx, set.id.clone());
            }
        }
        (_, true) => {
            if trill_noir::button(&mut cell, theme, "Stop", Variant::Default).clicked() {
                setup::stop(ctx, cx, set.id.clone());
            }
        }
        _ => {
            let left = set.fetched_bytes().saturating_sub(status.got);
            let label = if status.got > 0 {
                format!("Go on \u{00B7} {}", said_closely(left))
            } else {
                format!("Fetch \u{00B7} {}", said_closely(left))
            };
            if trill_noir::button(&mut cell, theme, &label, Variant::Primary).clicked() {
                setup::fetch(ctx, cx, set.id.clone());
            }
            if status.got > 0 && trill_noir::button(&mut cell, theme, "Remove", Variant::Default).clicked() {
                setup::remove(ctx, cx, set.id.clone());
            }
        }
    }
    let room = foot.with_max_x(cell.min_rect().left() - 12.0);
    text::paint_name(ui.painter(), room, Align2::LEFT_CENTER, theme.text.micro, &note, theme.ink_faint);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use trill_fetch::{Fetched, Part};

    fn status_of(set: &Set, state: PartState, got: u64) -> PartStatus {
        let sum = "abaac48bc8fd55fa672268c8f13228b9b00f52f0c4cd10945048982eca4643bc";
        PartStatus {
            part: Arc::new(Part {
                id: set.id.clone(),
                name: set.name.clone(),
                files: vec![Fetched::file("https://example.org/a.zip", sum, set.fetched_bytes(), "a.zip")],
                ..Part::default()
            }),
            folder: PathBuf::from("sets").join(&set.id),
            state,
            got,
            on_disk: got,
            error: None,
        }
    }

    #[test]
    fn the_catalogue_is_shown_by_kind_and_a_search_narrows_it_to_what_it_finds() {
        let catalogue = Catalogue::shipped();
        let (kinds, shown) = by_kind(catalogue, "");
        assert_eq!(shown.len(), catalogue.instruments().count(), "the marked ones are listed too");
        assert_eq!(kinds.first().map(|row| row.kind), Some(Kind::Piano));
        assert_eq!(kinds.iter().map(|row| row.instruments).sum::<usize>(), shown.len());
        // A set with instruments of two kinds stands under both.
        let under = |kind: Kind| kinds.iter().find(|row| row.kind == kind).unwrap();
        assert!(under(Kind::Strings).sets.iter().any(|set| set.id == "vsco-2-ce-contrabass"));
        assert!(under(Kind::Bass).sets.iter().any(|set| set.id == "vsco-2-ce-contrabass"));

        let (kinds, shown) = by_kind(catalogue, "salamander");
        assert_eq!((kinds.len(), kinds[0].kind, kinds[0].sets.len(), shown.len()), (1, Kind::Piano, 1, 1));
        let (kinds, _) = by_kind(catalogue, "no instrument is called this");
        assert!(kinds.is_empty());
    }

    #[test]
    fn an_ask_says_what_it_takes_what_is_left_of_it_and_when_all_of_it_is_here() {
        let catalogue = Catalogue::shipped();
        let starter = Ask::Starter.sets(catalogue);
        let all: u64 = starter.iter().map(|set| set.fetched_bytes()).sum();
        let none = HashMap::new();
        let fresh = ask_words("Starter", &starter, &none);
        assert_eq!(
            (fresh.label.clone(), fresh.idle, fresh.here),
            (format!("Starter \u{00B7} {}", said_closely(all)), false, 0)
        );
        // The smallest of them is here, and the next is on its way.
        let statuses: Vec<PartStatus> = starter
            .iter()
            .enumerate()
            .map(|(index, set)| match index {
                0 => status_of(set, PartState::Ready, set.fetched_bytes()),
                1 => status_of(set, PartState::Fetching, set.fetched_bytes() / 2),
                _ => status_of(set, PartState::Missing, 0),
            })
            .collect();
        let by_id: HashMap<&str, &PartStatus> = statuses.iter().map(|one| (one.part.id.as_str(), one)).collect();
        let left: u64 = starter[2..].iter().map(|set| set.fetched_bytes()).sum();
        let partly = ask_words("Starter", &starter, &by_id);
        assert_eq!(
            (partly.label.clone(), partly.idle, partly.here),
            (format!("Starter \u{00B7} {} left", said_closely(left)), false, 1)
        );
        // As one thing: how many are here, how much has come, which is on its way.
        let list = ListStatus {
            id: "instruments".into(),
            title: "Instruments".into(),
            about: String::new(),
            folder: PathBuf::from("sets"),
            parts: statuses.clone(),
        };
        let ids: Vec<String> = starter.iter().map(|set| set.id.clone()).collect();
        let whole = whole(&list, Some(&("Starter".to_owned(), ids))).unwrap();
        let come = starter[0].fetched_bytes() + starter[1].fetched_bytes() / 2;
        assert_eq!(
            whole.words,
            format!(
                "Starter: 1 of {} sets here \u{00B7} {} of {} \u{00B7} Fetching {}",
                starter.len(),
                said_closely(come),
                said_closely(all),
                starter[1].name
            )
        );
        assert!((whole.share - come as f32 / all as f32).abs() < 1e-6);
        // All of it here: the button says so and asks for nothing; and nothing is on its way.
        let done: Vec<PartStatus> =
            starter.iter().map(|set| status_of(set, PartState::Ready, set.fetched_bytes())).collect();
        let by_id: HashMap<&str, &PartStatus> = done.iter().map(|one| (one.part.id.as_str(), one)).collect();
        assert_eq!(
            ask_words("Starter", &starter, &by_id),
            AskWords { label: "Starter \u{00B7} here".into(), idle: true, sets: starter.len(), here: starter.len() }
        );
        assert_eq!(super::whole(&ListStatus { parts: done, ..list }, None), None);
        assert_eq!(asked_words("Starter", &trill_fetch::Asked::default()), "Starter: all of it is here already.");
        let asked = trill_fetch::Asked { queued: vec!["a".into(), "b".into()], here: 1, going: 0, bytes: 300 << 20 };
        assert_eq!(
            asked_words("Pianos", &asked),
            "Pianos: 2 sets asked for, 300 MB. They are fetched one after the other, the smallest first."
        );
    }

    #[test]
    fn an_instrument_that_is_not_put_forward_says_what_was_measured_against_it() {
        let catalogue = Catalogue::shipped();
        let marked: Vec<(&Set, &Instrument)> =
            catalogue.instruments().filter(|(_, one)| !one.is_put_forward()).collect();
        assert!(!marked.is_empty());
        for (set, instrument) in &marked {
            let why = why_not_put_forward(instrument);
            assert!(
                !why.is_empty() && why.iter().all(|sentence| sentence.len() > 12),
                "{} {}: {why:?}",
                set.id,
                instrument.name
            );
            assert!(!instrument.is_starter(), "{} {} is put first with something against it", set.id, instrument.name);
        }
        let noisy = marked.iter().find(|(_, one)| one.marks.contains(&Mark::Noisy)).expect("one is marked noisy").1;
        assert!(
            why_not_put_forward(noisy)[0]
                .starts_with("Its recordings carry noise that is heard: the quietest part of one lies ")
        );
        let fine = catalogue.instruments().find(|(_, one)| one.is_starter()).unwrap().1;
        assert!(why_not_put_forward(fine).is_empty());
        assert_eq!(licence_short(Licence::CcBy4), "CC BY 4.0");
        // A set that lies in the library without having been fetched here says so.
        let set = catalogue.set("ixox-flute").unwrap();
        let here: HashMap<String, PathBuf> =
            set.instruments.iter().map(|one| (one.text.clone(), PathBuf::from("x.sfz"))).collect();
        let missing = status_of(set, PartState::Missing, 0);
        assert_eq!(set_state(set, Some(&missing), &here), ("In your library".to_owned(), false));
        assert_eq!(set_state(set, Some(&missing), &HashMap::new()), ("Not here".to_owned(), false));
    }
}
