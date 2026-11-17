//! The bar across the top of the window: wordmark, project and menus on
//! the left, then the transport and its readouts, and on the right the
//! audio thread's load and the MCP address.

use super::arrangement::ops;
use super::files::Confirm;
use super::shortcuts::{self, Command};
use super::state::{Clipboard, Cx, DetailTab, StoryAsk};
use super::{arrangement, audio_settings, brand, files, filming, recording, recovery, timeline};
use eframe::egui::{self, Align2, Id, Rect, Sense, Ui, pos2, vec2};
use trill_core::TimeSignature;
use trill_core::project::{MAX_TEMPO, MIN_TEMPO};
use trill_noir::{Chip, Icon, MenuItem, NumberField, TextStyle, Theme, paint, text};

pub const HEIGHT: f32 = 36.0;
const EDGE: f32 = 10.0;
/// Space between the mark and the wordmark.
const MARK_GAP: f32 = 9.0;
/// Space between unrelated groups.
const GAP: f32 = 14.0;
/// Height of the boxes in the bar: buttons, readouts.
const BOX: f32 = 22.0;
const BUTTON_WIDTH: f32 = 26.0;
/// The project name gets at most this much of the bar, and in a window too
/// narrow for everything gives way first, down to this much.
const NAME_MAX_WIDTH: f32 = 190.0;
const NAME_MIN_WIDTH: f32 = 44.0;
/// Where the transport starts when the window is wide enough to choose.
const TRANSPORT_LEFT: f32 = 372.0;
const POSITION_WIDTH: f32 = 140.0;
/// Room for three digits before the point and the unit after.
const TEMPO_WIDTH: f32 = 106.0;
const SIGNATURE_WIDTH: f32 = 52.0;
/// The box beside the record button: the count-in while nothing is being
/// recorded, and what the take is doing while one is.
const COUNT_WIDTH: f32 = 66.0;
/// The song position without its clock, as the narrowest windows show it.
const POSITION_COMPACT: f32 = 76.0;
const TRANSPORT_WIDTH: f32 =
    6.0 * BUTTON_WIDTH + COUNT_WIDTH + 4.0 * 4.0 + 10.0 + POSITION_WIDTH + 6.0 + TEMPO_WIDTH + 6.0 + SIGNATURE_WIDTH;
/// What the narrowest windows leave out of the transport: the clock, and
/// the button that opens the stage (its key and the View menu still do).
const TRANSPORT_GIVES_UP: f32 = (POSITION_WIDTH - POSITION_COMPACT) + BUTTON_WIDTH + 4.0;
const TRANSPORT_COMPACT: f32 = TRANSPORT_WIDTH - TRANSPORT_GIVES_UP;
const CPU_METER_WIDTH: f32 = 54.0;

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let bar = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(bar, 0.0, theme.panel);
    paint::hline_above(&painter, bar.x_range(), bar.bottom(), theme.hairline);

    // In a window too narrow for everything, things give way in a fixed
    // order: a long project name first, down to a few letters, then the
    // meter beside the load, and last the clock beside the song position.
    // So the right is laid out first, told where the transport would end
    // with the name at its shortest, and what is left over says how much
    // of the name there is room for.
    let shortest = project_group_shortest(&painter, cx, bar);
    let right_start = status_group(ui, cx, bar, shortest + GAP + TRANSPORT_WIDTH + GAP);
    let (width, compact) = transport_width(right_start - shortest);
    let left_end = project_group(ui, cx, bar, right_start - GAP - width - GAP);

    // At its usual place when there is room; otherwise squeezed between
    // its neighbors, keeping clear of the menus first.
    let latest = right_start - GAP - width;
    let left = TRANSPORT_LEFT.min(latest).max(left_end + GAP);
    transport_group(ui, cx, bar, bar.left() + left, compact);
}

/// How wide the transport is set when `room` lies between the end of the
/// project group at its shortest and the start of what is on the right,
/// and whether that is without its clock.
fn transport_width(room: f32) -> (f32, bool) {
    if room < GAP + TRANSPORT_WIDTH + GAP { (TRANSPORT_COMPACT, true) } else { (TRANSPORT_WIDTH, false) }
}

/// A box of the bar's height at `left`, vertically centered.
fn boxed(bar: Rect, left: f32, width: f32) -> Rect {
    Rect::from_min_size(pos2(left, bar.center().y - BOX / 2.0), vec2(width, BOX))
}

/// How wide the project name is set: as wide as it is, up to its share of
/// the bar, and no wider than `room`, short of a few letters.
fn name_width(wanted: f32, room: f32) -> f32 {
    wanted.min(NAME_MAX_WIDTH).min(room.max(NAME_MIN_WIDTH))
}

/// The menus of the bar, in order.
const MENUS: [&str; 3] = ["File", "Edit", "View"];

fn menu_width(painter: &egui::Painter, theme: &Theme, title: &str) -> f32 {
    (text::width(painter, theme.text.caps, title) + 14.0).ceil()
}

/// How much of the project group follows the name: the dot of unsaved
/// work, and the menus.
fn after_name(painter: &egui::Painter, cx: &Cx) -> f32 {
    let theme = cx.theme;
    let unsaved = if cx.snap.modified { 6.0 + theme.metric.dot } else { 0.0 };
    unsaved + GAP - 7.0 + MENUS.iter().map(|title| menu_width(painter, theme, title)).sum::<f32>()
}

/// The x where the project group ends when its name is cut as short as it
/// goes: the least room the left of the bar can do with.
fn project_group_shortest(painter: &egui::Painter, cx: &Cx, bar: Rect) -> f32 {
    let theme = cx.theme;
    let before = EDGE + brand::WIDTH as f32 + MARK_GAP + text::width(painter, theme.text.brand, "TRILL") + GAP;
    let name = name_width(text::width(painter, theme.text.body, &cx.snap.project.name), 0.0);
    bar.left() + before + name + after_name(painter, cx)
}

/// Wordmark, project name and the menus. Returns the x where they end,
/// which is by `end` if the name can be cut short enough for that.
fn project_group(ui: &mut Ui, cx: &mut Cx, bar: Rect, end: f32) -> f32 {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    let mut x = bar.left() + EDGE;
    let row = |x: f32| Rect::from_min_max(pos2(x, bar.top()), bar.max);

    x += brand::paint_centered(&painter, x, bar.center().y, 1.0).width() + MARK_GAP;
    x += text::paint(&painter, row(x), Align2::LEFT_CENTER, theme.text.brand, "TRILL", theme.ink) + GAP;

    let name = &cx.snap.project.name;
    let width = name_width(text::width(&painter, theme.text.body, name), end - x - after_name(&painter, cx));
    let name_rect = Rect::from_min_size(pos2(x, bar.top()), vec2(width, bar.height()));
    text::paint(&painter, name_rect, Align2::LEFT_CENTER, theme.text.body, name, theme.ink);
    x += width;
    if cx.snap.modified {
        // A dot in the signal color: there is work that is not saved.
        x = paint::dot(&painter, pos2(x + 6.0, bar.center().y + 1.0), theme.metric.dot, theme.signal);
    }
    let response = ui.interact(name_rect.expand2(vec2(8.0, 0.0)), ui.scope_id().with("project-name"), Sense::hover());
    let location = cx.snap.path.as_ref().map_or_else(|| "Not saved yet".to_owned(), |path| path.display().to_string());
    cx.hint(&response, if cx.snap.modified { format!("{location} \u{00B7} unsaved changes") } else { location });
    x += GAP - 7.0;

    let menus = [file_menu as fn(&mut Ui, &mut Cx), edit_menu, view_menu];
    for (title, menu) in MENUS.into_iter().zip(menus) {
        let rect = boxed(bar, x, menu_width(&painter, theme, title));
        let response = trill_noir::menu_title(ui, theme, rect, ui.scope_id().with(("menu", title)), title);
        egui::Popup::menu(&response).gap(1.0).show(|ui| {
            ui.set_min_width(220.0);
            menu(ui, cx);
        });
        x = rect.right();
    }
    x
}

fn file_menu(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let item = |ui: &mut Ui, label: &str, command: Command, enabled: bool| {
        MenuItem::new(label).shortcut(&shortcuts::keys(command)).enabled(enabled).show(ui, theme).clicked()
    };
    if item(ui, "New project", Command::New, true) {
        files::new_project(cx);
    }
    if item(ui, "Open\u{2026}", Command::Open, true) {
        files::open(ui.ctx(), cx);
    }
    // What a crash left unsaved, for as long as any of it is left: the
    // count stands where a shortcut would.
    let lost = cx.recovery.count();
    let count = if lost > 0 { lost.to_string() } else { String::new() };
    if MenuItem::new("Recover unsaved work\u{2026}").shortcut(&count).enabled(lost > 0).show(ui, theme).clicked() {
        recovery::open(cx);
    }
    trill_noir::menu_separator(ui, theme);
    if item(ui, "Save", Command::Save, true) {
        files::save(ui.ctx(), cx);
    }
    if item(ui, "Save as\u{2026}", Command::SaveAs, true) {
        files::save_as(ui.ctx(), cx);
    }
    trill_noir::menu_separator(ui, theme);
    if MenuItem::new("Import MIDI\u{2026}").show(ui, theme).clicked() {
        files::import_midi(ui.ctx(), cx);
    }
    if MenuItem::new("Export MIDI\u{2026}").show(ui, theme).clicked() {
        files::export_midi(ui.ctx(), cx);
    }
    let idle = cx.jobs.exporting().is_none();
    if item(ui, "Export WAV\u{2026}", Command::ExportWav, idle) {
        files::export_wav(ui.ctx(), cx);
    }
    // The song, and every track, group and return beside it, in one pass.
    if MenuItem::new("Export WAV and stems\u{2026}").enabled(idle).show(ui, theme).clicked() {
        files::export_stems(ui.ctx(), cx);
    }
    // The stage as a file: the song filmed with its own sound, or how it
    // was made. While one is being made, this is where it is watched.
    let film = if cx.filming.making().is_some() { "Film in progress\u{2026}" } else { "Export a film\u{2026}" };
    if item(ui, film, Command::ExportFilm, true) {
        filming::open(ui.ctx(), cx);
    }
    // How the song was made, as a file to hand on: by name, and without
    // the conversation unless that is what is asked for.
    trill_noir::menu_separator(ui, theme);
    trill_noir::menu_heading(ui, theme, "Making of");
    if MenuItem::new("Export\u{2026}").show(ui, theme).clicked() {
        files::export_story(ui.ctx(), cx, false);
    }
    if MenuItem::new("Export with the conversation\u{2026}").show(ui, theme).clicked() {
        files::export_story(ui.ctx(), cx, true);
    }
    if MenuItem::new("Watch one from a file\u{2026}").show(ui, theme).clicked() {
        files::watch_story(ui.ctx(), cx);
    }
    trill_noir::menu_separator(ui, theme);
    if item(ui, "Audio and MIDI\u{2026}", Command::AudioSettings, true) {
        audio_settings::open(cx);
    }
}

fn edit_menu(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let item = |ui: &mut Ui, label: &str, command: Command, enabled: bool| {
        MenuItem::new(label).shortcut(&shortcuts::keys(command)).enabled(enabled).show(ui, theme).clicked()
    };
    if item(ui, "Undo", Command::Undo, cx.snap.can_undo) {
        cx.with_session(|session| session.undo());
    }
    if item(ui, "Redo", Command::Redo, cx.snap.can_redo) {
        cx.with_session(|session| session.redo());
    }
    trill_noir::menu_separator(ui, theme);

    // These act on the clips; the same keys act on notes while the piano
    // roll is being worked in.
    let clips = !cx.state.selected_clips.is_empty();
    let holds_time = matches!(cx.state.clipboard, Some(Clipboard::Time(_)));
    let pasteable = holds_time || matches!(cx.state.clipboard, Some(Clipboard::Clips(_)));
    if item(ui, "Cut", Command::Cut, clips) {
        ops::cut(cx);
    }
    if item(ui, "Copy", Command::Copy, clips) {
        ops::copy(cx);
    }
    if item(ui, if holds_time { "Paste over" } else { "Paste" }, Command::Paste, pasteable) {
        ops::paste(cx);
    }
    if item(ui, "Duplicate", Command::Duplicate, clips) {
        ops::duplicate_selection(cx);
    }
    if item(ui, "Delete", Command::Delete, clips) {
        ops::delete_selection(cx);
    }
    if item(ui, "Select all clips", Command::SelectAll, true) {
        ops::select_all(cx);
    }
    trill_noir::menu_separator(ui, theme);
    if item(ui, "Split at the edit cursor", Command::Split, true) {
        ops::split_at_cursor(cx);
    }
    if item(ui, "Mute or unmute clips", Command::MuteClips, clips) {
        ops::toggle_mute(cx);
    }
    let time = ops::selected_span(cx).is_some();
    if item(ui, "Loop the selection", Command::LoopSelection, time) {
        ops::loop_selection(cx);
    }
    if item(ui, "Delete time", Command::DeleteTime, time) {
        ops::delete_time(cx);
    }
    if item(ui, "Duplicate time", Command::DuplicateTime, time) {
        ops::duplicate_time(cx);
    }
    if item(ui, "Insert time", Command::InsertTime, time) {
        ops::insert_time(cx);
    }
    if item(ui, "Copy time", Command::CopyTime, time) {
        ops::copy_time(cx);
    }
    if item(ui, "Paste time", Command::PasteTime, holds_time) {
        ops::paste_time(cx, trill_core::PasteMode::Insert);
    }
    trill_noir::menu_separator(ui, theme);
    if item(ui, "Add track", Command::AddTrack, true) {
        arrangement::add_track(cx);
    }
    if MenuItem::new("Add group track").show(ui, theme).clicked() {
        arrangement::add_group_track(cx);
    }
    if MenuItem::new("Add return track").show(ui, theme).clicked() {
        arrangement::add_return_track(cx);
    }
    trill_noir::menu_separator(ui, theme);
    if item(ui, "Add marker", Command::AddMarker, true) {
        ops::add_marker(cx);
    }
    // Every marker, to jump to.
    let signature = cx.snap.project.time_signature;
    let markers = cx.snap.project.markers.clone();
    if !markers.is_empty() {
        trill_noir::menu_heading(ui, theme, "Go to marker");
    }
    for marker in markers {
        let at = timeline::format_position(marker.beat, signature);
        if MenuItem::new(&marker.name).shortcut(&at).show(ui, theme).clicked() {
            cx.state.arrangement.cursor = marker.beat;
            cx.with_session(|session| session.seek(marker.beat));
        }
    }
}

fn view_menu(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let open = cx.state.browser.open;
    // The keys as they are now: letters the computer keyboard plays are
    // shortcuts with Alt while it is on.
    let playing = cx.keyboard.is_on();
    let keys = |command| shortcuts::keys_while(command, playing);
    if MenuItem::new("Browser").shortcut(&keys(Command::ToggleBrowser)).checked(open).show(ui, theme).clicked() {
        cx.state.browser.open = !open;
    }
    let shown = cx.assistant.is_open();
    if MenuItem::new("Assistant").shortcut(&keys(Command::ToggleAssistant)).checked(shown).show(ui, theme).clicked() {
        cx.assistant.toggle();
    }
    let switch = keys(Command::ToggleKeyboard);
    if MenuItem::new("Computer keyboard").shortcut(&switch).checked(playing).show(ui, theme).clicked() {
        cx.keyboard.toggle();
    }
    if MenuItem::new("Stage").shortcut(&keys(Command::ToggleStage)).show(ui, theme).clicked() {
        cx.state.stage_open = true;
    }
    // The making-of: the session played again, and the journal it is
    // played from.
    trill_noir::menu_separator(ui, theme);
    if MenuItem::new("Making of").shortcut(&keys(Command::MakingOf)).show(ui, theme).clicked() {
        cx.state.story = Some(StoryAsk::Watch);
    }
    let keeping = cx.with_session(|session| session.keeps_journal());
    if MenuItem::new("Keep a journal").checked(keeping).show(ui, theme).clicked() {
        files::keep_journal(cx, !keeping);
    }
    if MenuItem::new("Delete this song\u{2019}s journal\u{2026}").enabled(keeping).show(ui, theme).clicked() {
        cx.jobs.confirm = Some(Confirm::DeleteJournal);
    }
    trill_noir::menu_separator(ui, theme);
    for (tab, label) in [(DetailTab::Clip, "Clip"), (DetailTab::Devices, "Devices"), (DetailTab::Mixer, "Mixer")] {
        if MenuItem::new(label).checked(cx.state.detail_tab == tab).show(ui, theme).clicked() {
            cx.state.detail_tab = tab;
        }
    }
    trill_noir::menu_separator(ui, theme);
    if MenuItem::new("Zoom to the selection").shortcut(&keys(Command::ZoomToSelection)).show(ui, theme).clicked() {
        arrangement::zoom_to_selection(ui.ctx(), cx);
    }
    if MenuItem::new("Zoom to the whole song").shortcut(&keys(Command::ZoomToSong)).show(ui, theme).clicked() {
        arrangement::zoom_to_song(ui.ctx(), cx);
    }
    let follow = cx.state.arrangement.follow;
    if MenuItem::new("Follow the playhead")
        .shortcut(&keys(Command::ToggleFollow))
        .checked(follow)
        .show(ui, theme)
        .clicked()
    {
        cx.state.arrangement.follow = !follow;
    }
    trill_noir::menu_separator(ui, theme);
    if MenuItem::new("Shortcuts").shortcut(&keys(Command::ShowShortcuts)).show(ui, theme).clicked() {
        cx.state.shortcuts_open = true;
    }
    trill_noir::menu_separator(ui, theme);
    trill_noir::menu_heading(ui, theme, "Theme");
    for candidate in Theme::all() {
        let label = capitalized(candidate.name);
        if MenuItem::new(&label).checked(candidate.name == theme.name).show(ui, theme).clicked() {
            cx.state.theme_request = Some(candidate.name);
        }
    }
}

fn capitalized(name: &str) -> String {
    let mut letters = name.chars();
    letters.next().map_or_else(String::new, |first| first.to_uppercase().chain(letters).collect())
}

/// Play, stop and record with its count-in, loop, metronome and the stage,
/// then position, tempo and time signature. `compact` leaves out the clock
/// and the stage's button.
fn transport_group(ui: &mut Ui, cx: &mut Cx, bar: Rect, left: f32, compact: bool) {
    let theme = cx.theme;
    let id = ui.scope_id().with("transport");
    let mut x = left;
    let button = |x: &mut f32| {
        let rect = boxed(bar, *x, BUTTON_WIDTH);
        *x += BUTTON_WIDTH + 4.0;
        rect
    };

    let playing = cx.live.playing;
    let color = if playing { theme.ok } else { theme.ink_dim };
    let response = trill_noir::icon_button(ui, theme, button(&mut x), id.with("play"), Icon::Play, playing, color);
    cx.hint(&response, "Play \u{00B7} Space");
    if response.clicked() {
        cx.with_session(|session| session.play());
    }
    let response =
        trill_noir::icon_button(ui, theme, button(&mut x), id.with("stop"), Icon::Stop, false, theme.ink_dim);
    cx.hint(&response, "Stop \u{00B7} Space \u{00B7} when stopped, returns to the start");
    if response.clicked() {
        stop_or_rewind(cx);
    }

    // Record, and beside it the count-in, which while a take runs says
    // what the take is doing. The two share an edge: they are one control.
    let take = cx.live.take.clone();
    let color = if take.is_some() { theme.signal } else { theme.ink_dim };
    let rect = boxed(bar, x, BUTTON_WIDTH);
    let lit = take.is_some();
    let response = trill_noir::icon_button(ui, theme, rect, id.with("record"), Icon::Record, lit, color);
    let keys = shortcuts::keys(Command::Record);
    let about = match &take {
        Some(take) => recording::describe(cx, take),
        None if cx.live.armed.is_empty() => {
            format!("Record on the selected track, which is armed for it \u{00B7} {keys}")
        }
        None => format!("Record what is played on the armed tracks \u{00B7} {keys}"),
    };
    cx.hint(&response, about);
    if response.clicked() {
        recording::toggle(cx);
    }
    let rect = boxed(bar, rect.right() - 1.0, COUNT_WIDTH + 1.0);
    count_in(ui, cx, rect, id.with("count-in"));
    x = rect.right() + 4.0;

    let looping = cx.snap.project.loop_region.enabled;
    let color = if looping { theme.ink } else { theme.ink_dim };
    let response = trill_noir::icon_button(ui, theme, button(&mut x), id.with("loop"), Icon::Loop, looping, color);
    let playing = cx.keyboard.is_on();
    cx.hint(&response, shortcuts::with_keys("Loop the region marked on the ruler", Command::ToggleLoop, playing));
    if response.clicked() {
        toggle_loop(cx);
    }
    let metronome = cx.live.metronome;
    let color = if metronome { theme.ink } else { theme.ink_dim };
    let rect = button(&mut x);
    let response = trill_noir::icon_button(ui, theme, rect, id.with("metronome"), Icon::Metronome, metronome, color);
    let about = "Metronome: a click on every beat while the song plays";
    cx.hint(&response, shortcuts::with_keys(about, Command::ToggleMetronome, playing));
    if response.clicked() {
        toggle_metronome(cx);
    }
    if !compact {
        // The stage: the same playback, filmed.
        let rect = button(&mut x);
        let response = trill_noir::icon_button(ui, theme, rect, id.with("stage"), Icon::Stage, false, theme.ink_dim);
        let about = "The stage: the song filmed as it plays";
        cx.hint(&response, shortcuts::with_keys(about, Command::ToggleStage, playing));
        if response.clicked() {
            cx.state.stage_open = true;
        }
    }
    x += 6.0;

    let signature = cx.snap.project.time_signature;
    let painter = ui.painter().clone();
    let position = boxed(bar, x, if compact { POSITION_COMPACT } else { POSITION_WIDTH });
    paint::frame(&painter, position, theme.hairline);
    let inner = position.shrink2(vec2(8.0, 0.0));
    let bars = timeline::format_position(cx.live.position, signature).replace('.', ". ");
    text::paint(&painter, inner, Align2::LEFT_CENTER, theme.text.readout, &bars, theme.ink);
    if !compact {
        let clock = timeline::format_clock(cx.live.position, cx.snap.project.tempo);
        let clock_style = TextStyle { size: 12.0, ..theme.text.value };
        text::paint(&painter, inner, Align2::RIGHT_CENTER, clock_style, &clock, theme.ink_dim);
    }
    let response = ui.interact(position, id.with("position"), Sense::hover());
    cx.hint(&response, "Song position in bars, beats and sixteenths, and in minutes and seconds");
    x = position.right() + 6.0;

    let tempo = cx.snap.project.tempo;
    let field = NumberField {
        label: "",
        text: &format!("{tempo:.2}"),
        unit: "BPM",
        value: tempo,
        range: MIN_TEMPO..=MAX_TEMPO,
        per_point: 0.25,
        style: theme.text.readout,
        name: "Tempo",
    };
    let output = trill_noir::number_field(ui, theme, boxed(bar, x, TEMPO_WIDTH), id.with("tempo"), &field);
    cx.hint(&output.response, "Tempo \u{00B7} drag, or double-click to type");
    if output.response.drag_started() {
        cx.begin_gesture();
    }
    if let Some(tempo) = output.value.filter(|value| *value != tempo) {
        // Dragging lands on quarter steps; typing keeps what was typed.
        let tempo = if output.response.dragged() { (tempo * 4.0).round() / 4.0 } else { tempo };
        cx.edit(|project| project.set_tempo(tempo));
    }
    x += TEMPO_WIDTH + 6.0;

    time_signature(ui, cx, boxed(bar, x, SIGNATURE_WIDTH), id.with("signature"));
}

/// The box beside the record button. While nothing is recorded it is the
/// count-in, with a menu to choose it; while a take runs it says what the
/// take is doing: counting in, recording, or which pass of the loop.
fn count_in(ui: &mut Ui, cx: &mut Cx, rect: Rect, id: Id) {
    let theme = cx.theme;
    let painter = ui.painter_at(rect);
    if let Some(take) = cx.live.take.clone() {
        let state = recording::state_label(&take, cx.snap.project.loop_region.enabled);
        let response = ui.interact(rect, id, Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, format!("Take: {state}")));
        painter.rect_filled(rect, 0.0, theme.selected);
        paint::frame(&painter, rect, theme.hairline);
        text::paint(&painter, rect.shrink(1.0), Align2::CENTER_CENTER, theme.text.micro, &state, theme.signal);
        cx.hint(&response, recording::describe(cx, &take));
        return;
    }

    let bars = cx.live.count_in;
    let response = ui.interact(rect, id, Sense::click());
    let name = format!("Count-in: {}", recording::count_in_name(bars));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    if open {
        painter.rect_filled(rect, 0.0, theme.selected);
    } else if response.hovered() {
        painter.rect_filled(rect, 0.0, theme.hover);
    }
    paint::frame(&painter, rect, theme.hairline);
    let color = if open || response.hovered() { theme.ink } else { theme.ink_dim };
    let value = if bars == 1 { "1 bar".to_owned() } else { format!("{bars} bars") };
    let content = Rect::from_min_max(pos2(rect.left() + 7.0, rect.top()), pos2(rect.right() - 13.0, rect.bottom()));
    text::paint(&painter, content, Align2::LEFT_CENTER, theme.text.micro, &value, color);
    trill_noir::paint_icon(&painter, pos2(rect.right() - 8.0, rect.center().y), Icon::CaretDown, color);
    cx.hint(&response, "Count-in: the bars the metronome clicks before a take that starts from a stopped song");
    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(130.0);
        trill_noir::menu_heading(ui, theme, "Count-in");
        for choice in recording::COUNT_INS {
            let label = recording::count_in_name(choice);
            if MenuItem::new(&label).checked(choice == bars).show(ui, theme).clicked() {
                recording::set_count_in(cx, choice);
            }
        }
    });
}

/// The signatures the time signature's menu offers.
const SIGNATURES: [(u8, u8); 12] =
    [(2, 4), (3, 4), (4, 4), (5, 4), (6, 4), (7, 4), (3, 8), (5, 8), (6, 8), (7, 8), (9, 8), (12, 8)];
/// The note values a beat can be, as dragging steps through them.
const DENOMINATORS: [u8; 4] = [2, 4, 8, 16];
/// Pixels of drag per step of the time signature.
const SIGNATURE_STEP: f32 = 9.0;

/// The signature `steps` drag steps from `start`, changing its beats per
/// bar (`numerator` true) or the note value of a beat.
fn stepped_signature(start: TimeSignature, numerator: bool, steps: i32) -> (u8, u8) {
    if numerator {
        ((start.numerator as i32 + steps).clamp(1, 32) as u8, start.denominator)
    } else {
        let index = DENOMINATORS.iter().position(|value| *value == start.denominator).unwrap_or(1) as i32;
        let index = (index + steps).clamp(0, DENOMINATORS.len() as i32 - 1);
        (start.numerator, DENOMINATORS[index as usize])
    }
}

/// The time signature: a click offers the usual ones, and dragging either
/// number up or down changes it.
fn time_signature(ui: &mut Ui, cx: &mut Cx, rect: Rect, id: Id) {
    let theme = cx.theme;
    let signature = cx.snap.project.time_signature;
    let response = ui.interact(rect, id, Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeVertical);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Time signature"));
    let painter = ui.painter_at(rect);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    if open {
        painter.rect_filled(rect, 0.0, theme.selected);
    } else if response.hovered() || response.dragged() {
        painter.rect_filled(rect, 0.0, theme.hover);
    }
    paint::frame(&painter, rect, theme.hairline);
    let shown = format!("{} / {}", signature.numerator, signature.denominator);
    text::paint(&painter, rect, Align2::CENTER_CENTER, theme.text.readout, &shown, theme.ink);
    cx.hint(&response, "Time signature \u{00B7} click to choose, or drag either number up or down");

    // A drag is measured from where it began, on the number it began on.
    let memory = id.with("drag");
    if response.drag_started()
        && let Some(press) = ui.input(|input| input.pointer.press_origin())
    {
        cx.begin_gesture();
        ui.data_mut(|data| data.insert_temp(memory, (signature, press.x < rect.center().x)));
    }
    if response.dragged()
        && let Some(travel) = response.total_drag_delta()
        && let Some((start, numerator)) = ui.data(|data| data.get_temp::<(TimeSignature, bool)>(memory))
    {
        let (beats, note) = stepped_signature(start, numerator, (-travel.y / SIGNATURE_STEP).round() as i32);
        if (beats, note) != (signature.numerator, signature.denominator) {
            cx.edit(|project| project.set_time_signature(beats, note));
        }
    }

    egui::Popup::menu(&response).gap(1.0).show(|ui| {
        ui.set_min_width(110.0);
        for (index, (beats, note)) in SIGNATURES.into_iter().enumerate() {
            // The simple meters, then the ones counted in eighths.
            if index == 6 {
                trill_noir::menu_separator(ui, theme);
            }
            let current = (signature.numerator, signature.denominator) == (beats, note);
            if MenuItem::new(&format!("{beats} / {note}")).checked(current).show(ui, theme).clicked() {
                cx.edit(|project| project.set_time_signature(beats, note));
            }
        }
    });
}

/// Stops playback, and with it a take that is being recorded or counted
/// in; pressing stop again returns to the start of the song.
pub fn stop_or_rewind(cx: &mut Cx) {
    let rolling = cx.live.playing || cx.live.take.is_some();
    cx.with_session(|session| if rolling { session.stop() } else { session.seek(0.0) });
}

/// Plays, or stops. A take ends where the song is stopped, and one that
/// is still being counted in is called off: the song has not started, and
/// Space must not then start it.
pub fn toggle_play(cx: &mut Cx) {
    let rolling = cx.live.playing || cx.live.take.is_some();
    cx.with_session(|session| if rolling { session.stop() } else { session.play() });
}

pub fn toggle_loop(cx: &mut Cx) {
    let region = cx.snap.project.loop_region;
    cx.edit(|project| project.set_loop(!region.enabled, region.start, region.end));
}

/// Switches the metronome on or off. It belongs to the session, not the
/// project: it is neither saved nor undone.
pub fn toggle_metronome(cx: &mut Cx) {
    let on = !cx.live.metronome;
    cx.with_session(|session| session.set_metronome(on));
}

/// The computer keyboard's switch, CPU load and the MCP chip, laid out from
/// the right edge. Returns the x where the group starts. In a window too
/// narrow for all of it (`least` is where the group may start), the load's
/// meter goes and its number stays.
fn status_group(ui: &mut Ui, cx: &mut Cx, bar: Rect, least: f32) -> f32 {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    let mut right = bar.right() - EDGE;

    // The assistant's panel opens below this corner; its button is here.
    let toggle = boxed(bar, right - BUTTON_WIDTH, BUTTON_WIDTH);
    super::assistant::toggle_button(ui, cx, toggle);
    right = toggle.left() - GAP;

    if let Some(url) = cx.mcp_url {
        // The port is what tells two windows apart; the tooltip has the rest.
        let port = url
            .split("://")
            .last()
            .and_then(|rest| rest.split(['/', '?']).next())
            .and_then(|host| host.rsplit(':').next());
        let label = port.map_or_else(|| "MCP".to_owned(), |port| format!("MCP :{port}"));
        let chip = Chip::new(&label, true).dot(theme.ok).named("MCP address");
        let width = chip.width(ui, theme);
        let rect = Rect::from_min_size(
            pos2(right - width, bar.center().y - theme.metric.chip / 2.0),
            vec2(width, theme.metric.chip),
        );
        let response = trill_noir::chip(ui, theme, rect, ui.scope_id().with("mcp"), &chip);
        cx.hint(&response, format!("AI assistants can connect at {url} \u{00B7} click to copy"));
        if response.clicked() {
            ui.ctx().copy_text(url.to_owned());
            cx.state.toasts.info("Copied the MCP address");
        }
        response
            .on_hover_text(format!("AI assistants can connect to this session at\n{url}\nClick to copy the address."));
        right = rect.left() - GAP;
    }

    let load = cx.live.cpu_load.clamp(0.0, 9.99);
    let color = match load {
        load if load >= 0.85 => theme.signal,
        load if load >= 0.6 => theme.warn,
        _ => theme.data,
    };
    let percent = format!("{:.0} %", load * 100.0);
    // Wide enough for three digits, so the meter beside it never moves.
    let value_width = text::width(&painter, theme.text.value, "100 %").ceil();
    let value = Rect::from_min_max(pos2(right - value_width, bar.top()), pos2(right, bar.bottom()));
    text::paint(&painter, value, Align2::RIGHT_CENTER, theme.text.value, &percent, theme.ink);
    let label_width = text::width(&painter, theme.text.micro, "CPU").ceil();
    let meter_width = 6.0 + CPU_METER_WIDTH;
    let with_meter = value.left() - meter_width - 8.0 - label_width - GAP - BUTTON_WIDTH >= least;
    let mut label_right = value.left() - 8.0;
    if with_meter {
        let meter =
            Rect::from_min_size(pos2(value.left() - meter_width, bar.center().y - 4.0), vec2(CPU_METER_WIDTH, 8.0));
        trill_noir::mini_meter(&painter, theme, meter, load, color);
        label_right = meter.left() - 8.0;
    }
    let label = Rect::from_min_max(pos2(label_right - label_width, bar.top()), pos2(label_right, bar.bottom()));
    text::paint(&painter, label, Align2::LEFT_CENTER, theme.text.micro, "CPU", theme.ink_faint);

    let group = Rect::from_min_max(pos2(label.left(), bar.top()), pos2(right, bar.bottom()));
    let response = ui.interact(group, Id::unique("cpu-load"), Sense::hover());
    cx.hint(&response, "Share of the audio thread\u{2019}s time budget in use");

    // The computer keyboard's switch, on the transport's side of the
    // readouts. What it plays is shown in a strip of its own while on.
    let switch = boxed(bar, label.left() - GAP - BUTTON_WIDTH, BUTTON_WIDTH);
    super::keyboard::switch(ui, cx, switch);
    switch.left()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_project_name_gives_way_before_anything_overlaps() {
        assert_eq!(name_width(80.0, 400.0), 80.0, "a short name is as wide as it is");
        assert_eq!(name_width(500.0, 400.0), NAME_MAX_WIDTH, "a long one has its share of the bar");
        assert_eq!(name_width(500.0, 120.0), 120.0, "and less in a narrow window");
        assert_eq!(name_width(80.0, 120.0), 80.0);
        assert_eq!(name_width(500.0, -30.0), NAME_MIN_WIDTH, "but always a few letters");
    }

    #[test]
    fn the_transport_gives_up_its_clock_last_and_only_when_it_must() {
        let full = GAP + TRANSPORT_WIDTH + GAP;
        assert_eq!(transport_width(full + 200.0), (TRANSPORT_WIDTH, false));
        assert_eq!(transport_width(full), (TRANSPORT_WIDTH, false), "an exact fit is a fit");
        assert_eq!(transport_width(full - 1.0), (TRANSPORT_COMPACT, true));
        let given_up = (POSITION_WIDTH - POSITION_COMPACT) + BUTTON_WIDTH + 4.0;
        assert_eq!(TRANSPORT_WIDTH - TRANSPORT_COMPACT, given_up, "the clock and the stage's button are what go");
        // The smallest window with everything in the bar that can be: the
        // name cut to a few letters, the dot of unsaved work, the MCP chip.
        // Wordmark and menus take about 290 points and the right about
        // 260; what is left has to hold the transport without its clock.
        let smallest_window = 1060.0;
        assert!(smallest_window - 290.0 - 260.0 >= GAP + TRANSPORT_COMPACT + GAP, "{TRANSPORT_COMPACT}");
    }

    #[test]
    fn dragging_steps_the_time_signature_within_what_the_model_accepts() {
        let four_four = TimeSignature::default();
        assert_eq!(stepped_signature(four_four, true, 3), (7, 4));
        assert_eq!(stepped_signature(four_four, true, -9), (1, 4), "at least one beat in a bar");
        assert_eq!(stepped_signature(four_four, true, 99), (32, 4));
        assert_eq!(stepped_signature(four_four, false, 1), (4, 8));
        assert_eq!(stepped_signature(four_four, false, -1), (4, 2));
        assert_eq!(stepped_signature(four_four, false, 9), (4, 16));
        for (beats, note) in SIGNATURES {
            assert!(TimeSignature::new(beats, note).is_some(), "{beats}/{note}");
        }
        for steps in -40..40 {
            let (beats, note) = stepped_signature(four_four, steps % 2 == 0, steps);
            assert!(TimeSignature::new(beats, note).is_some(), "{beats}/{note}");
        }
    }
}
