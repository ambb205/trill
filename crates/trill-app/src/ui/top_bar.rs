//! The bar across the top of the window: wordmark, project and menus on
//! the left, then the transport and its readouts, and on the right the
//! audio thread's load and the MCP address.

use super::arrangement::ops;
use super::shortcuts::{self, Command};
use super::state::{Clipboard, Cx, DetailTab};
use super::{arrangement, brand, files, timeline};
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
/// The project name gets at most this much of the bar.
const NAME_MAX_WIDTH: f32 = 190.0;
/// Where the transport starts when the window is wide enough to choose.
const TRANSPORT_LEFT: f32 = 372.0;
const POSITION_WIDTH: f32 = 140.0;
/// Room for three digits before the point and the unit after.
const TEMPO_WIDTH: f32 = 106.0;
const SIGNATURE_WIDTH: f32 = 52.0;
const TRANSPORT_WIDTH: f32 =
    4.0 * BUTTON_WIDTH + 3.0 * 4.0 + 10.0 + POSITION_WIDTH + 6.0 + TEMPO_WIDTH + 6.0 + SIGNATURE_WIDTH;
const CPU_METER_WIDTH: f32 = 54.0;

pub fn show(ui: &mut Ui, cx: &mut Cx) {
    let theme = cx.theme;
    let bar = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(bar, 0.0, theme.panel);
    paint::hline_above(&painter, bar.x_range(), bar.bottom(), theme.hairline);

    let left_end = project_group(ui, cx, bar);
    let right_start = status_group(ui, cx, bar);

    // At its usual place when there is room; otherwise squeezed between
    // its neighbors, keeping clear of the menus first.
    let latest = right_start - GAP - TRANSPORT_WIDTH;
    let left = TRANSPORT_LEFT.min(latest).max(left_end + GAP);
    transport_group(ui, cx, bar, bar.left() + left);
}

/// A box of the bar's height at `left`, vertically centered.
fn boxed(bar: Rect, left: f32, width: f32) -> Rect {
    Rect::from_min_size(pos2(left, bar.center().y - BOX / 2.0), vec2(width, BOX))
}

/// Wordmark, project name and the menus. Returns the x where they end.
fn project_group(ui: &mut Ui, cx: &mut Cx, bar: Rect) -> f32 {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    let mut x = bar.left() + EDGE;
    let row = |x: f32| Rect::from_min_max(pos2(x, bar.top()), bar.max);

    x += brand::paint_centered(&painter, x, bar.center().y, 1.0).width() + MARK_GAP;
    x += text::paint(&painter, row(x), Align2::LEFT_CENTER, theme.text.brand, "TRILL", theme.ink) + GAP;

    let name = &cx.snap.project.name;
    let width = text::width(&painter, theme.text.body, name).min(NAME_MAX_WIDTH);
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

    for (title, menu) in [("File", file_menu as fn(&mut Ui, &mut Cx)), ("Edit", edit_menu), ("View", view_menu)] {
        let width = text::width(&painter, theme.text.caps, title) + 14.0;
        let rect = boxed(bar, x, width.ceil());
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
    let pasteable = matches!(cx.state.clipboard, Some(Clipboard::Clips(_)));
    if item(ui, "Cut", Command::Cut, clips) {
        ops::cut(cx);
    }
    if item(ui, "Copy", Command::Copy, clips) {
        ops::copy(cx);
    }
    if item(ui, "Paste", Command::Paste, pasteable) {
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
    let keys = shortcuts::keys;
    if MenuItem::new("Browser").shortcut(&keys(Command::ToggleBrowser)).checked(open).show(ui, theme).clicked() {
        cx.state.browser.open = !open;
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

/// Play, stop, loop and metronome, then position, tempo and time signature.
fn transport_group(ui: &mut Ui, cx: &mut Cx, bar: Rect, left: f32) {
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
    let looping = cx.snap.project.loop_region.enabled;
    let color = if looping { theme.ink } else { theme.ink_dim };
    let response = trill_noir::icon_button(ui, theme, button(&mut x), id.with("loop"), Icon::Loop, looping, color);
    cx.hint(&response, "Loop the region marked on the ruler \u{00B7} L");
    if response.clicked() {
        toggle_loop(cx);
    }
    let metronome = cx.live.metronome;
    let color = if metronome { theme.ink } else { theme.ink_dim };
    let rect = button(&mut x);
    let response = trill_noir::icon_button(ui, theme, rect, id.with("metronome"), Icon::Metronome, metronome, color);
    cx.hint(&response, "Metronome: a click on every beat while the song plays \u{00B7} K");
    if response.clicked() {
        toggle_metronome(cx);
    }
    x += 6.0;

    let signature = cx.snap.project.time_signature;
    let painter = ui.painter().clone();
    let position = boxed(bar, x, POSITION_WIDTH);
    paint::frame(&painter, position, theme.hairline);
    let inner = position.shrink2(vec2(8.0, 0.0));
    let bars = timeline::format_position(cx.live.position, signature).replace('.', ". ");
    text::paint(&painter, inner, Align2::LEFT_CENTER, theme.text.readout, &bars, theme.ink);
    let clock = timeline::format_clock(cx.live.position, cx.snap.project.tempo);
    let clock_style = TextStyle { size: 12.0, ..theme.text.value };
    text::paint(&painter, inner, Align2::RIGHT_CENTER, clock_style, &clock, theme.ink_dim);
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

/// Stops playback; pressing stop again returns to the start of the song.
pub fn stop_or_rewind(cx: &mut Cx) {
    let playing = cx.live.playing;
    cx.with_session(|session| if playing { session.stop() } else { session.seek(0.0) });
}

pub fn toggle_play(cx: &mut Cx) {
    let playing = cx.live.playing;
    cx.with_session(|session| if playing { session.stop() } else { session.play() });
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

/// CPU load and the MCP chip, laid out from the right edge. Returns the x
/// where the group starts.
fn status_group(ui: &mut Ui, cx: &mut Cx, bar: Rect) -> f32 {
    let theme = cx.theme;
    let painter = ui.painter().clone();
    let mut right = bar.right() - EDGE;

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
    let meter = Rect::from_min_size(
        pos2(value.left() - 6.0 - CPU_METER_WIDTH, bar.center().y - 4.0),
        vec2(CPU_METER_WIDTH, 8.0),
    );
    trill_noir::mini_meter(&painter, theme, meter, load, color);
    let label_width = text::width(&painter, theme.text.micro, "CPU").ceil();
    let label =
        Rect::from_min_max(pos2(meter.left() - 8.0 - label_width, bar.top()), pos2(meter.left() - 8.0, bar.bottom()));
    text::paint(&painter, label, Align2::LEFT_CENTER, theme.text.micro, "CPU", theme.ink_faint);

    let group = Rect::from_min_max(pos2(label.left(), bar.top()), pos2(right, bar.bottom()));
    let response = ui.interact(group, Id::unique("cpu-load"), Sense::hover());
    cx.hint(&response, "Share of the audio thread\u{2019}s time budget in use");
    label.left()
}

#[cfg(test)]
mod tests {
    use super::*;

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
